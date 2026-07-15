# Source Maps & Server-Side Symbolication — Design

**Date:** 2026-07-15
**Status:** Approved (design), implementation in progress
**Scope owner:** Sauron backend + SDKs + dashboard

## 1. Goal

Give Sauron the Sentry-style "readable stack traces" feature: turn a minified /
obfuscated production stack frame (e.g. `bundle.abc123.js:1:45213`) into the
original source location (`checkout.ts:88:12 in submitOrder`) with a few lines of
surrounding source context, on the issue/event detail view.

Two symbolication pipelines behind one shared core:

| Pipeline | SDK | Artifact format | Match key (v1) | Resolver |
|---|---|---|---|---|
| **Web JS** | `@sauron/browser` | Source Map v3 (`.map`) | `release` + normalized file path | parse-on-upload → binary search |
| **Flutter mobile** | `sauron_flutter` (Android + iOS, AOT) | Dart split-debug-info (Android ELF/DWARF, iOS dSYM/DWARF) | `debug_id` (build-id) + `arch` | `gimli` + `addr2line` |

Symbolication is **presentational only** — it never changes grouping.

## 2. Non-goals (v1)

- **Debug-id matching for JS** and a **Vite/rollup build plugin** — deferred to a
  later phase; v1 matches JS by `release` + file path (works with what the SDK
  already sends).
- **Node / Python / C# symbolication** — out of scope. Only `@sauron/browser` and
  `sauron_flutter`.
- **Reprocessing / re-grouping** of already-stored events. Symbolication of a
  late-uploaded map is lazy and presentational; fingerprints are never rewritten.
- **URL scraping** of source maps from a live site (avoids SSRF; see the prior
  monitoring SSRF findings). Upload-only.
- **Symbolicated grouping** for Dart (see §7 tradeoff).

## 3. Key decisions (from brainstorming)

1. **Targets:** Web JS (Source Map v3) + Flutter mobile (Android + iOS).
2. **Timing:** **Hybrid** — symbolicate at ingest when artifacts already exist;
   otherwise lazily on first view. Persist backfill **only for hot partitions**;
   for cold/exported partitions, symbolicate in-memory and return without
   persisting (respects the tiering drop-guard — no writes into exported
   partitions).
3. **Fingerprint basis:** **raw frames always.** Symbolication is purely
   presentational → no issue-splitting, hybrid is safe.
4. **Storage:** 3-tier — in-process parsed-index LRU + **isolated** Redis
   warm-blob cache + Postgres content-addressed zstd blobs. Parse-on-upload for
   JS.
5. **JS matching/DX:** `release` + path in v1; debug-id + Vite plugin later.
   Upload via authenticated API + a thin uploader CLI.
6. **Flutter platforms:** Android + iOS both in v1.

## 4. Architecture

### 4.1 New crate: `sauron-symbols`

Holds the shared symbolicator core. Mostly pure, unit-testable logic:

- `trait FrameResolver { fn resolve(&self, frame: &RawFrameRef, ctx: &ResolveCtx) -> ResolvedFrame; }`
- **JS resolver** (`js.rs`): Source Map v3 decode → compact sorted index → binary
  search lookup, `sourcesContent` context extraction.
- **Dart resolver** (`dart.rs`): `gimli`/`addr2line` over ELF (Android) and
  Mach-O/DWARF (iOS dSYM); `pc − isolate_dso_base` address resolution with
  inline-frame expansion.
- **Cache** (`cache.rs`): byte-bounded LRU of `Arc<ParsedJsIndex>` /
  `Arc<DwarfContext>`, single-flight per key.
- **Matching** (`match.rs`): normalize a frame's `abs_path`/`filename` to a
  `~/path` form and resolve `(app_id, release, name)`; Dart resolves
  `(app_id, debug_id, arch)`.
- `ArtifactStore` trait (implemented in `sauron-db` layer) for fetching blob
  bytes; the crate itself is storage-agnostic so it stays pure/testable.

### 4.2 Data model — migration `2026-07-15-000014_symbol_artifacts`

Content-addressed for dedup:

```sql
CREATE TABLE symbol_blobs (
    sha256            BYTEA PRIMARY KEY,
    content           BYTEA NOT NULL,          -- zstd-compressed
    uncompressed_size BIGINT NOT NULL,
    compressed_size   BIGINT NOT NULL,
    refcount          INTEGER NOT NULL DEFAULT 0,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);
ALTER TABLE symbol_blobs ALTER COLUMN content SET STORAGE EXTERNAL; -- pre-compressed; skip TOAST re-compression

CREATE TABLE symbol_artifacts (
    id                    UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    app_id                UUID NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    kind                  TEXT NOT NULL,        -- 'js_sourcemap' | 'dart_symbols'
    platform              TEXT NOT NULL,        -- 'web' | 'android' | 'ios'
    arch                  TEXT,                 -- dart: 'arm64' | 'armeabi-v7a' | 'x86_64'
    release               TEXT,                 -- js matching
    dist                  TEXT,
    name                  TEXT,                 -- js: minified file path/URL the map applies to
    debug_id              TEXT,                 -- dart build-id / uuid
    blob_sha256           BYTEA NOT NULL REFERENCES symbol_blobs(sha256),
    prebuilt_index_sha256 BYTEA REFERENCES symbol_blobs(sha256),  -- js parse-on-upload index
    uploaded_by           UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at            TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX symbol_artifacts_debugid_idx ON symbol_artifacts (app_id, debug_id) WHERE debug_id IS NOT NULL;
CREATE INDEX symbol_artifacts_release_name_idx   ON symbol_artifacts (app_id, release, name);
```

Three columns added to the partitioned `error_events` parent (propagate to
partitions):

```sql
ALTER TABLE error_events ADD COLUMN stacktrace_symbolicated JSONB;      -- resolved frames, NO context lines
ALTER TABLE error_events ADD COLUMN symbolication_status   TEXT NOT NULL DEFAULT 'pending';
ALTER TABLE error_events ADD COLUMN debug_meta             JSONB;       -- dart: {build_id, dso_base, arch, os, raw_stacktrace}
```

`symbolication_status ∈ { pending, symbolicated, partial, no_artifacts, not_applicable, failed }`.

The tier export (Parquet) schema gains these columns. Events symbolicated *after*
their partition is exported keep raw-only in Parquet — documented limitation,
acceptable since symbolication is presentational.

### 4.3 Storage & caching (3 tiers, fastest first)

1. **In-proc parsed-index LRU** (`sauron-symbols::cache`) — `Arc<ParsedJsIndex>` /
   `Arc<DwarfContext>`, bounded by a **bytes** budget (`SYMBOLS_CACHE_MB`, default
   256). **Single-flight per key** so a hot deploy doesn't make every worker parse
   the same file at once. Absorbs ~all reads. `sourcesContent` is *not* held
   long-term for the JS index beyond what the budget allows; context lines are
   pulled on demand.
2. **Isolated Redis warm-blob cache** — dedicated `SYMBOLS_REDIS_URL` (separate
   instance/DB with its own `allkeys-lru maxmemory`), keyed by `sha256`. **Per-blob
   size cap** (`SYMBOLS_REDIS_MAX_BLOB_MB`, default 8) → big Dart ELFs skip Redis
   and live in-proc only. Symbol blobs can **never** evict ingest-stream state.
3. **Postgres durable** — content-addressed zstd blobs; `refcount` GC on artifact
   delete (drop a blob only at refcount 0). `STORAGE EXTERNAL`.

Guards: decompression-bomb cap (`SYMBOLS_MAX_UNCOMPRESSED_MB`), artifact size cap
(`SYMBOLS_MAX_ARTIFACT_MB`; diesel bytea isn't streaming), and blob reads never on
the ingest hot path unless a parse is actually needed (time-boxed).

## 5. Upload API + RBAC

New atomic permission `artifact:write` added to `perm::ALL` (→ `ALL: [&str; 20]`)
and to the Owner/Admin/Developer preset permission sets in `sauron-auth::rbac`
(not Viewer), seeded in the migration and kept synced by `ensure_preset_roles` at
api startup. Reading symbolicated frames uses the existing `issue:read`/`event:read`.

App-scoped endpoints in `sauron-api` (`routes/artifacts.rs`):

- `POST /v1/apps/{id}/artifacts` — multipart. Fields: `kind`, `platform`, `arch`,
  `release`, `dist`, `name`, `debug_id` + the file. Server: hash → zstd → dedup
  into `symbol_blobs` (bump refcount) → insert `symbol_artifacts`; for
  `kind=js_sourcemap`, parse-on-upload into the compact index and store it as a
  second blob (`prebuilt_index_sha256`). Idempotent on `(app_id, debug_id)` /
  `(app_id, release, name, blob_sha256)`. Enforces size + decompression caps.
- `GET /v1/apps/{id}/artifacts` — list (release, platform, kind, arch, sizes,
  uploaded_at). Backs the dashboard admin view + a basic "did my maps land?" check.
- `DELETE /v1/apps/{id}/artifacts/{artifact_id}` — decrement blob refcount(s), GC
  at zero.

**Uploader CLI** (`backend/bins/sauron-symcli` or a script): thin wrapper over the
API. For Flutter it walks a `--split-debug-info` output dir and uploads each
per-arch symbol file with its `debug_id`. Documented in `wiki/` + a CI snippet.

## 6. Symbolicator core internals

### 6.1 JS / Source Map v3
- **Upload-time:** decode VLQ `mappings` into a sorted array of
  `(gen_line, gen_col) → (src_idx, src_line, src_col, name_idx)`, plus `sources`,
  `names`, `sourcesContent`; serialize as the prebuilt index blob.
- **Lookup:** binary-search the greatest mapping ≤ `(lineno, colno)`.
  **Column math:** stack columns are 1-based, source-map columns 0-based → convert
  before search. Single-line bundles mean correctness rides entirely on the column
  — this is the silent-mismap risk, covered by golden tests (§9).
- **Function name:** from the mapping's `name` when present, else inferred from the
  enclosing function mapping.
- **Context:** ±5 lines from `sourcesContent`, attached at render time (API
  response), not persisted.
- **Matching:** normalize `abs_path` (strip origin → `~/path`, drop query string),
  match `(app_id, release, name)`.

### 6.2 Dart / ELF + DWARF
- Build an `addr2line::Context` (via `gimli`) per `(debug_id, arch)`; cache it.
- **Lookup:** `pc − isolate_dso_base` (+ load bias) → `Context::find_frames(addr)`
  → function + file + line, **expanding inlined frames**. iOS dSYM read through the
  same DWARF path (gimli reads Mach-O).
- **Matching:** `(app_id, debug_id, arch)` from the SDK-reported header
  (`debug_meta`).

Unresolved frames pass through raw with `symbolicated:false` → status `partial`.

## 7. Ingest & on-read integration

- **Ingest (hybrid)** — in `sauron-pipeline::process::process_error`, immediately
  after the raw `stacktrace` is serialized (line ~144) and **without touching the
  fingerprint** (line 113): look up artifacts for the event's `release`/`debug_id`;
  on hit resolve frames → set `stacktrace_symbolicated` + `symbolication_status`;
  on miss leave null + `no_artifacts`/`pending`. **Time-boxed; never blocks the
  insert** — on timeout/error store raw + `pending`.
- **On-read (`sauron-api` issue/event detail)** — if status ∈ `{pending,
  no_artifacts}` and artifacts now exist:
  - **hot** partition → resolve, **persist back** (`stacktrace_symbolicated` +
    status), return;
  - **cold/exported** partition → resolve in-memory, **return without persisting**.
  - context lines hydrated here from the cached map.
- **Dart grouping tradeoff:** raw Dart frames are address offsets (unusable for
  grouping), so Dart errors fall through the existing `type + normalized message`
  fingerprint fallback in `sauron-core::fingerprint`. Build-stable (good), but two
  distinct Dart bugs sharing a type+message over-group. Documented v1 limitation.

## 8. SDK changes

- **`@sauron/browser`** — no required change for v1 (already sends `release` +
  frame URL/line/col). Optional `dist`.
- **`sauron_flutter`** — captures the **verbatim** Dart stack string plus a debug
  header and sends them as a distinct payload:
  - new `ErrorItem` fields `raw_stacktrace: Option<String>` + `debug_meta:
    Option<DebugMeta>` where `DebugMeta { build_id, isolate_dso_base, arch, os }`,
    added to `sauron-core::envelope` (both `#[serde(default)]`, back-compatible).
  - captured from `FlutterError.onError` / `PlatformDispatcher.onError` / zone
    handlers.
  - persisted into `error_events.debug_meta` (with `raw_stacktrace`) so on-read can
    re-resolve.
  - only new SDK versions can be symbolicated (older payloads lack the header).

## 9. Dashboard

- **`StacktraceView.svelte`** renders `stacktrace_symbolicated` when present:
  original file/function/line, expandable **source-context** with the crash line
  highlighted, and a **"show minified/raw" toggle**. A per-issue/event **status
  badge**: *Symbolicated / Partial / No source maps (→ how-to-upload hint) /
  Pending*.
- **New per-app "Source Maps" admin area** — lists uploaded artifacts via
  `GET …/artifacts` (release, platform, kind, size, uploaded_at) with delete. Uses
  house UI components (DataTable etc.) per the dashboard component-kit conventions.
  New `dashboard/src/lib/api/artifacts.ts`.

## 10. Testing

- **`sauron-symbols` unit tests:**
  - JS golden fixtures — a known minified bundle + map → exact resolved
    file/line/col/function + context; per-browser column variants; edge cases
    (single-line bundle, missing name, out-of-range column).
  - Dart fixtures — a small ELF + a known obfuscated stack → resolved frames
    including an inlined frame; base-offset math.
  - Cache — byte-bounded LRU eviction, single-flight, refcount GC.
- **No DB/handler integration harness exists** → verify the upload API, hybrid
  ingest/read, and dashboard rendering end-to-end via `docker compose` (upload a
  real map, send a minified error, confirm the detail view symbolicates).
- **SDK tests:** Flutter capture of raw stack + `debug_meta`; JS unchanged.

## 11. Build sequence (the plan slices this; the spec covers the whole)

1. **Foundation** — migration (tables + `error_events` columns) + `sauron-symbols`
   skeleton + Postgres content-addressed zstd store (+ refcount GC) + in-proc LRU +
   isolated Redis + `artifact:write` perm + upload/list/delete API + uploader CLI.
   *(Stores/serves artifacts; no symbolication yet.)*
2. **JS pipeline** — Source Map v3 parse-on-upload + resolver + release/path
   matching + hybrid ingest/read wiring + dashboard rendering + status. ← headline
   value.
3. **Flutter pipeline** — SDK capture (raw stack + `debug_meta`) + core/envelope
   fields + Dart ELF/DWARF resolver (Android archs + iOS) + e2e.
4. *(Post-v1)* debug-id + Vite plugin.

Each slice ships and verifies independently.

## 12. Config (env)

- `SYMBOLS_CACHE_MB` (default 256) — in-proc parsed-index budget.
- `SYMBOLS_REDIS_URL` — isolated warm-blob Redis (falls back to disabled if unset).
- `SYMBOLS_REDIS_MAX_BLOB_MB` (default 8) — per-blob Redis cap.
- `SYMBOLS_MAX_ARTIFACT_MB` (default 128) — reject larger uploads.
- `SYMBOLS_MAX_UNCOMPRESSED_MB` (default 512) — decompression-bomb guard.
- `SYMBOLS_INGEST_TIMEOUT_MS` (default 150) — ingest-path symbolication time box.
