# Source Maps — Slice 2: JS Symbolication Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:executing-plans or subagent-driven-development. Steps use `- [ ]`.

**Goal:** De-minify JavaScript stack traces — resolve stored minified frames against uploaded Source Map v3 artifacts and show original file/function/line + source context on the issue/event detail view. Hybrid: pre-symbolicate at ingest when a map is present, backfill lazily on read otherwise.

**Architecture:** Add the Source Map v3 parser + resolver + path matcher to the (pure) `sauron-symbols` crate, plus a storage-agnostic `Symbolicator` that caches parsed maps (in the Slice-1 `ByteLru`) and resolves a frame list via a `BlobFetch` trait. The API implements `BlobFetch` over Postgres + the isolated Redis cache and symbolicates on read (persisting for hot partitions). The ingest worker uses the same `Symbolicator`, time-boxed. The dashboard renders symbolicated frames with a raw/minified toggle + status badge.

**Tech Stack:** Rust (`sauron-symbols`), diesel-async, axum, Svelte 5.

## Global Constraints

- Fingerprinting stays on RAW frames — never touch `fingerprint(...)` in `process.rs:113`. Symbolication is presentational.
- `sauron-symbols` stays storage-agnostic (no `sauron-db`/`sauron-redis` dep) — fetching goes through a trait.
- No auto-commit; leave changes in the working tree.
- Column convention: stack `lineno`/`colno` are **1-based**; Source Map v3 lines/columns are **0-based**. Convert on the boundary.
- **Deviation from spec §6.1 (documented):** v1 does NOT serialize a separate "prebuilt index" blob on upload. Instead the parsed map is cached in the in-proc `ByteLru` on first use (same steady-state performance; `prebuilt_index_sha256` stays null, reserved). Removes a bespoke serialization format from v1.

---

### Task 1: Source Map v3 parser + frame resolver (`sauron-symbols::js`)

**Files:**
- Create: `backend/crates/sauron-symbols/src/js.rs`
- Modify: `backend/crates/sauron-symbols/src/lib.rs` (`pub mod js;`)

**Interfaces:**
- Produces:
  - `js::ParsedSourceMap` with `parse(bytes: &[u8]) -> Result<ParsedSourceMap, SymbolError>`.
  - `ParsedSourceMap::resolve(&self, lineno_1based: u32, colno_1based: u32) -> Option<js::ResolvedLoc>`.
  - `ParsedSourceMap::context(&self, source_index: usize, line_1based: u32, radius: usize) -> Option<js::SourceContext>`.
  - `js::ResolvedLoc { source: String, line: u32 /*1-based*/, column: u32 /*1-based*/, name: Option<String>, source_index: usize }`.
  - `js::SourceContext { pre: Vec<String>, line: String, post: Vec<String>, start_line: u32 }`.
  - `ParsedSourceMap::weight(&self) -> usize` (approx bytes, for the LRU).

- [ ] **Step 1: Write failing golden tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    // sources=[foo.ts], names=[greet], one segment [0,0,0,0,0] on gen line 0.
    const MAP1: &str = r#"{"version":3,"sources":["foo.ts"],"names":["greet"],
        "mappings":"AAAAA",
        "sourcesContent":["export function greet(){\n  return 'hi'\n}\n"]}"#;

    #[test]
    fn resolves_single_segment() {
        let m = ParsedSourceMap::parse(MAP1.as_bytes()).unwrap();
        let r = m.resolve(1, 1).unwrap();
        assert_eq!(r.source, "foo.ts");
        assert_eq!(r.line, 1);        // 0-based 0 -> 1-based 1
        assert_eq!(r.column, 1);
        assert_eq!(r.name.as_deref(), Some("greet"));
    }

    // Two segments on gen line 0: [0,0,0,0,0] then [4,0,1,1,0]  ("AAAAA,IAACA")
    // seg2 = gen_col 4 -> foo.ts line 2 col 2 (deltas: src_line +1, src_col +1).
    const MAP2: &str = r#"{"version":3,"sources":["foo.ts"],"names":["greet"],
        "mappings":"AAAAA,IAACA",
        "sourcesContent":["line0\nline1\nline2\n"]}"#;

    #[test]
    fn picks_greatest_segment_le_column() {
        let m = ParsedSourceMap::parse(MAP2.as_bytes()).unwrap();
        // col 6 (0-based 5) -> seg2 (gen_col 4)
        let r = m.resolve(1, 6).unwrap();
        assert_eq!((r.line, r.column), (2, 2));
        // col 2 (0-based 1) -> seg1 (gen_col 0)
        let r1 = m.resolve(1, 2).unwrap();
        assert_eq!((r1.line, r1.column), (1, 1));
    }

    #[test]
    fn out_of_range_column_before_first_segment_is_none_or_first() {
        let m = ParsedSourceMap::parse(MAP2.as_bytes()).unwrap();
        // col 0 is invalid (1-based min is 1); col 1 -> 0-based 0 -> seg1
        assert!(m.resolve(1, 1).is_some());
        // a line with no segments -> None
        assert!(m.resolve(9, 1).is_none());
    }

    #[test]
    fn context_extracts_surrounding_lines() {
        let m = ParsedSourceMap::parse(MAP2.as_bytes()).unwrap();
        let c = m.context(0, 2, 1).unwrap(); // line 2 (1-based) with radius 1
        assert_eq!(c.line, "line1");
        assert_eq!(c.pre, vec!["line0".to_string()]);
        assert_eq!(c.post, vec!["line2".to_string()]);
        assert_eq!(c.start_line, 1);
    }

    #[test]
    fn vlq_decodes_negative_and_multichar() {
        // 'D' = 3 -> value -1 (sign bit set). Verify via a crafted delta.
        assert_eq!(decode_vlq(&mut "D".chars().peekable()), Some(-1));
        // 'A'=0
        assert_eq!(decode_vlq(&mut "A".chars().peekable()), Some(0));
    }
}
```

- [ ] **Step 2: Run — expect fail**

Run: `cd backend && cargo test -p sauron-symbols js::`
Expected: FAIL (unresolved names).

- [ ] **Step 3: Implement `js.rs`**

Implement: base64 VLQ decode (`decode_vlq(&mut Peekable<Chars>) -> Option<i64>`), the mappings decoder (per-line segment vecs; `gen_col` resets each `;`; `src_idx`/`src_line`/`src_col`/`name_idx` are running deltas across the whole file), a `Segment { gen_col: u32, src: Option<SrcRef> }`, `resolve` = binary search on the line's segments for greatest `gen_col <= col0`, `context` slices `sourcesContent[source_index]` by lines. Deserialize the map JSON with `serde_json::Value` (avoid a full typed struct to tolerate extra fields); guard `version==3`. Convert 0-based→1-based on output.

- [ ] **Step 4: Run — expect pass**

Run: `cd backend && cargo test -p sauron-symbols js::`
Expected: PASS (all golden tests).

- [ ] **Step 5: Commit** *(skip unless asked)*

---

### Task 2: Path normalization + artifact matching (`sauron-symbols::matcher`)

**Files:**
- Create: `backend/crates/sauron-symbols/src/matcher.rs`
- Modify: `backend/crates/sauron-symbols/src/lib.rs` (`pub mod matcher;`)

**Interfaces:**
- Produces:
  - `matcher::normalize(abs_path: &str) -> String` — strip scheme+host to a `~/path` form, drop query string/fragment.
  - `matcher::matches(frame_path: &str, artifact_name: &str) -> bool` — true when the normalized frame path equals the artifact name, or the artifact name equals the frame's basename (fallback).

- [ ] **Step 1: Failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strips_origin_and_query() {
        assert_eq!(normalize("https://cdn.example.com/static/app.min.js?v=3"), "~/static/app.min.js");
        assert_eq!(normalize("/static/app.min.js"), "~/static/app.min.js");
        assert_eq!(normalize("app.min.js"), "~/app.min.js");
    }
    #[test]
    fn matches_full_and_basename() {
        assert!(matches("https://x.io/static/app.min.js", "~/static/app.min.js"));
        assert!(matches("https://x.io/static/app.min.js", "app.min.js")); // basename fallback
        assert!(!matches("https://x.io/static/app.min.js", "~/static/other.js"));
    }
}
```

- [ ] **Step 2-4: Run fail → implement → run pass**

Run: `cd backend && cargo test -p sauron-symbols matcher::`

- [ ] **Step 5: Commit** *(skip unless asked)*

---

### Task 3: `Symbolicator` engine (fetch trait + orchestration)

**Files:**
- Create: `backend/crates/sauron-symbols/src/engine.rs`
- Modify: `backend/crates/sauron-symbols/src/lib.rs`

**Interfaces:**
- Produces:
  - `engine::RawFrame { function: Option<String>, filename: Option<String>, abs_path: Option<String>, lineno: Option<u32>, colno: Option<u32>, in_app: Option<bool> }`
  - `engine::ResolvedFrame { function, filename, lineno, colno, in_app, symbolicated: bool }` (Serialize)
  - `engine::ArtifactRef { name: Option<String>, blob_sha256: Vec<u8> }`
  - `engine::Status` enum: `Symbolicated | Partial | NoArtifacts | NotApplicable` with `as_str()`.
  - `trait BlobFetch { async fn js_artifacts(&self, release: &str) -> Vec<ArtifactRef>; async fn blob(&self, sha: &[u8]) -> Option<Vec<u8>>; }` (native async-fn-in-trait; `blob` returns **decompressed** bytes).
  - `engine::Symbolicator { cache: ByteLru<Vec<u8>, ParsedSourceMap> }` with `new(budget_bytes)` and
    `async fn symbolicate_js<F: BlobFetch>(&self, fetch: &F, release: Option<&str>, frames: &[RawFrame]) -> (Vec<ResolvedFrame>, Status)`.

- [ ] **Step 1: Failing test with an in-memory `BlobFetch`**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::content;

    struct Mem { name: String, raw: Vec<u8> }
    impl BlobFetch for Mem {
        async fn js_artifacts(&self, _r: &str) -> Vec<ArtifactRef> {
            vec![ArtifactRef { name: Some(self.name.clone()), blob_sha256: content::sha256(&self.raw).to_vec() }]
        }
        async fn blob(&self, _sha: &[u8]) -> Option<Vec<u8>> { Some(self.raw.clone()) }
    }

    #[tokio::test]
    async fn symbolicates_a_matching_frame() {
        let raw = br#"{"version":3,"sources":["foo.ts"],"names":["greet"],"mappings":"AAAAA","sourcesContent":["export function greet(){}"]}"#.to_vec();
        let fetch = Mem { name: "~/static/app.min.js".into(), raw };
        let s = Symbolicator::new(4 << 20);
        let frames = vec![RawFrame { function: None, filename: Some("https://x.io/static/app.min.js".into()), abs_path: Some("https://x.io/static/app.min.js".into()), lineno: Some(1), colno: Some(1), in_app: Some(true) }];
        let (out, status) = s.symbolicate_js(&fetch, Some("web@1"), &frames).await;
        assert_eq!(out[0].filename.as_deref(), Some("foo.ts"));
        assert_eq!(out[0].lineno, Some(1));
        assert!(out[0].symbolicated);
        assert!(matches!(status, Status::Symbolicated));
    }

    #[tokio::test]
    async fn no_release_is_not_applicable() {
        let s = Symbolicator::new(1 << 20);
        let fetch = Mem { name: "n".into(), raw: vec![] };
        let (_out, status) = s.symbolicate_js(&fetch, None, &[]).await;
        assert!(matches!(status, Status::NotApplicable));
    }
}
```

- [ ] **Step 2-4: fail → implement → pass**

Implementation: if no release or no frames → `NotApplicable`/`NoArtifacts`. Fetch `js_artifacts(release)`. For each frame with a filename+lineno+colno, `matcher::matches` against artifact names; on match, get-or-parse the map via the `ByteLru` keyed by `blob_sha256` (build closure calls `fetch.blob(sha)` then `ParsedSourceMap::parse`); `resolve(lineno,colno)`; on hit build a resolved frame (source→filename, name→function, keep in_app), else pass the raw frame through with `symbolicated:false`. Status: all frames resolved → `Symbolicated`; some → `Partial`; matched no artifact at all → `NoArtifacts`.

- [ ] **Step 5: Commit** *(skip unless asked)*

---

### Task 4: On-read symbolication in the API

**Files:**
- Create: `backend/bins/sauron-api/src/symbolicate.rs` (`SqlBlobFetch` + `symbolicate_event`/`hydrate_context`)
- Modify: `backend/bins/sauron-api/src/main.rs` (add `Symbolicator` to `AppState`; `mod symbolicate;`)
- Modify: the error-event/issue detail handler(s) in `backend/bins/sauron-api/src/routes/issues.rs` (symbolicate on read; persist for hot partitions; attach `stacktrace_symbolicated` + `symbolication_status` + per-frame context to the response)
- Modify: `backend/crates/sauron-db/src/repo.rs` (`update_event_symbolication(conn, event_id, occurred_at, frames_json, status)`; `get_error_event(conn, app_id, event_id)` if not present)

**Interfaces:**
- Consumes Task 3 `Symbolicator`/`BlobFetch`; Slice-1 repo (`find_artifacts_for_release`, `get_blob`), `SymbolBlobCache`, `content::decompress`.
- Produces: event-detail JSON gains `stacktrace_symbolicated` (frames + per-frame `context`) and `symbolication_status`.

- [ ] **Step 1: `SqlBlobFetch`** — holds a pooled conn handle (or the pool) + app_id + `SymbolBlobCache` + max-uncompressed cap. `js_artifacts` = `repo::find_artifacts_for_release` filtered to `kind == "js_sourcemap"` → `ArtifactRef`. `blob` = Redis cache (`symbols.get(hex)`) else `repo::get_blob` (then `symbols.put`), `content::decompress(_, cap)`.

- [ ] **Step 2: On-read helper** — for an event with `symbolication_status ∈ {pending, no_artifacts}` and a release: run `symbolicator.symbolicate_js(...)`. If `Symbolicated|Partial`: if the event's partition is HOT (occurred_at newer than the tier hot window, `cfg.tier_hot_days`) persist via `update_event_symbolication`; always attach to the response. Cold events: attach without persisting. Then hydrate per-frame `context` from the cached parsed map (radius 5).

- [ ] **Step 3: Wire into the issue/event detail handler** — after loading the error event, run the helper and merge results into the response body. Add `Symbolicator` (constructed once in `main`, `Arc`-shared) to `AppState`.

- [ ] **Step 4: Build**

Run: `cd backend && cargo build -p sauron-api`

- [ ] **Step 5: Commit** *(skip unless asked)*

---

### Task 5: Ingest-time pre-symbolication (hybrid write path)

**Files:**
- Modify: `backend/crates/sauron-pipeline/src/process.rs` (`process_error`: after raw `stacktrace` at ~L144, before `insert_error_event`, time-boxed symbolication → set `NewErrorEvent.stacktrace_symbolicated` + `symbolication_status`)
- Modify: `backend/crates/sauron-db/src/models.rs` (`NewErrorEvent` gains `stacktrace_symbolicated: Option<Value>` + `symbolication_status: String`)
- Modify: `backend/crates/sauron-pipeline/src/worker.rs` (construct a shared `Symbolicator` + pass into `process`)
- Modify: `backend/crates/sauron-pipeline/Cargo.toml` (`sauron-symbols`)

**Interfaces:**
- Consumes Task 3 `Symbolicator` + a pipeline `BlobFetch` impl (same shape as `SqlBlobFetch`, over the worker's conn + Redis cache).

- [ ] **Step 1:** Add the two fields to `NewErrorEvent`; set them (`None`/`"pending"`) at the current call site so it compiles.
- [ ] **Step 2:** Build the pipeline `BlobFetch`; in `process_error`, if the exception has frames + a release, run `tokio::time::timeout(cfg.symbols_ingest_timeout_ms, symbolicator.symbolicate_js(...))`. On `Ok((frames, status))` set the fields; on timeout/miss leave `pending`/`no_artifacts`. **Never** return an error from symbolication — it must not fail ingest.
- [ ] **Step 3:** Thread a shared `Symbolicator` + config through `worker.rs` into `process`.
- [ ] **Step 4: Build + test**

Run: `cd backend && cargo build -p sauron-pipeline && cargo test -p sauron-pipeline`

- [ ] **Step 5: Commit** *(skip unless asked)*

---

### Task 6: Dashboard — symbolicated frames + status + Source Maps admin

**Files:**
- Modify: `dashboard/src/lib/components/StacktraceView.svelte` (render `stacktrace_symbolicated` when present; per-frame source context; raw/minified toggle)
- Modify: `dashboard/src/pages/IssueDetail.svelte` (status badge; pass symbolicated frames)
- Create: `dashboard/src/lib/api/artifacts.ts` (list/upload/delete client)
- Create: `dashboard/src/pages/SourceMaps.svelte` (per-app artifact list + delete)
- Modify: `dashboard/src/routes.ts` (+ nav) to add the Source Maps page

**Interfaces:**
- Consumes the event-detail JSON `stacktrace_symbolicated` + `symbolication_status`; the `/v1/apps/{id}/artifacts` endpoints.

- [ ] **Step 1:** `StacktraceView` — if `symbolicated` frames exist, render them (file/function/line + expandable context, crash line highlighted) with a "Show minified" toggle back to raw. Use house UI components per the dashboard conventions.
- [ ] **Step 2:** Status badge on issue/event detail: `Symbolicated` / `Partial` / `No source maps` (link to Source Maps page) / `Pending`.
- [ ] **Step 3:** `artifacts.ts` + `SourceMaps.svelte` (DataTable of artifacts: release, platform, kind, size, uploaded_at + delete) + route/nav entry.
- [ ] **Step 4: Verify**

Run: `cd dashboard && npx svelte-check && npx vite build`
Expected: 0 errors; build OK.

- [ ] **Step 5: Commit** *(skip unless asked)*

---

### Task 7: End-to-end verification (JS symbolication)

- [ ] **Step 1:** Bring up standalone pg+redis + local api + ingest (as in Slice 1). Create app; upload the real minified bundle's `.map` for a `release`.
- [ ] **Step 2:** Send an error envelope (via ingest) whose frame points at the minified file/line/col for that release.
- [ ] **Step 3:** GET the issue/event detail → assert `symbolication_status == "symbolicated"` and a frame shows the ORIGINAL file/function/line + context.
- [ ] **Step 4:** Upload-after-the-fact case: send the error BEFORE uploading the map (status `no_artifacts`/`pending`), upload the map, GET again → now symbolicated (on-read backfill).
- [ ] **Step 5:** `cargo test --workspace` + `svelte-check` green.

## Self-Review

- **Spec coverage:** §6.1 JS resolver → Tasks 1-2 (parse-on-upload prebuilt index deliberately deferred, noted in Global Constraints); §7 hybrid ingest+on-read → Tasks 4-5 (hot-partition persist / cold no-persist); §9 dashboard → Task 6.
- **Placeholders:** none — parser/matcher/engine (Tasks 1-3) carry real test + impl guidance; integration/dashboard carry exact files + interfaces + e2e.
- **Type consistency:** `ParsedSourceMap`, `ResolvedLoc`, `SourceContext`, `RawFrame`, `ResolvedFrame`, `ArtifactRef`, `Status`, `BlobFetch`, `Symbolicator::symbolicate_js` used consistently across Tasks 1-5.
