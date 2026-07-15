# Source Maps — Slice 3: Flutter/Dart Symbolication Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:executing-plans. Steps use `- [ ]`.

**Goal:** De-obfuscate Flutter (Android + iOS) AOT stack traces — resolve Dart PC offsets against an uploaded `--split-debug-info` ELF via DWARF, showing original function/file/line. Hybrid (ingest + on-read), like the JS slice.

**Architecture:** `sauron-symbols` gains a Dart pipeline: parse the verbatim Dart stack string (`dart_trace.rs`) and resolve addresses through an `addr2line`/`gimli`/`object` DWARF context built from the uploaded ELF (`dart.rs`). The SDK captures the raw trace + a debug header; the envelope carries `raw_stacktrace` + `debug_meta`; ingest/API symbolicate through the same `Symbolicator`.

**Tech Stack:** Rust (`object`, `addr2line`, `gimli`), Dart (`sauron_flutter`).

## Global Constraints

- Dart obfuscated frames are addresses, not names → fingerprinting stays on the existing `type + normalized message` fallback (the `exception.stacktrace` Frame list is empty for Dart). Never change that.
- `sauron-symbols` stays storage-agnostic — ELF bytes arrive via the existing `BlobFetch`.
- No auto-commit.
- **Verification boundary (documented):** the DWARF resolver is verified against a real `gcc -g` ELF fixture with objdump-derived addresses (DWARF is format-identical to Dart's split-debug-info). What only a real Flutter build can confirm is that Flutter's emitted `virt`/ELF layout matches — that's a follow-up.
- Address lookup uses each frame's `virt` value (the DSO-relative virtual address, which is exactly the DWARF lookup address); fall back to `abs − isolate_dso_base` when `virt` is absent.

---

### Task 1: Dart stack-trace parser (`sauron-symbols::dart_trace`)

**Files:** Create `backend/crates/sauron-symbols/src/dart_trace.rs`; modify `lib.rs`.

**Interfaces:**
- `dart_trace::parse(raw: &str) -> DartTrace` where `DartTrace { build_id: Option<String>, dso_base: Option<u64>, frames: Vec<DartFrameRef> }` and `DartFrameRef { index: u32, abs: Option<u64>, virt: Option<u64> }`.
- `DartFrameRef::lookup_addr(dso_base: Option<u64>) -> Option<u64>` — `virt`, else `abs - dso_base`.

- [ ] **Step 1: Failing tests** — parse a representative trace:

```rust
const TRACE: &str = "\
*** *** ***\n\
build_id: 'a1b2c3d4'\n\
isolate_dso_base: 7f0000000000\n\
    #00 abs 00007f0000001560 virt 0000000000001560 _kDartIsolateSnapshotInstructions+0x1560\n\
    #01 abs 00007f0000001890 virt 0000000000001890 _kDartIsolateSnapshotInstructions+0x1890\n";

#[test]
fn parses_header_and_frames() {
    let t = parse(TRACE);
    assert_eq!(t.build_id.as_deref(), Some("a1b2c3d4"));
    assert_eq!(t.dso_base, Some(0x7f0000000000));
    assert_eq!(t.frames.len(), 2);
    assert_eq!(t.frames[0].virt, Some(0x1560));
    assert_eq!(t.frames[0].lookup_addr(t.dso_base), Some(0x1560));
}

#[test]
fn falls_back_to_abs_minus_base() {
    let f = DartFrameRef { index: 0, abs: Some(0x7f0000001560), virt: None };
    assert_eq!(f.lookup_addr(Some(0x7f0000000000)), Some(0x1560));
}
```

- [ ] **Step 2-4:** run fail → implement (line-scan; hex parse; regex-free split) → run pass.
- [ ] **Step 5: Commit** *(skip unless asked)*

---

### Task 2: DWARF resolver (`sauron-symbols::dart`) + real ELF fixture

**Files:** Create `backend/crates/sauron-symbols/src/dart.rs`; modify `lib.rs`, `Cargo.toml` (`object`, `addr2line`), workspace `Cargo.toml`.

**Interfaces:**
- `dart::DartSymbols` with `parse(elf: &[u8]) -> Result<DartSymbols, SymbolError>` and `resolve(&self, addr: u64) -> Vec<js::ResolvedLoc>` (inline frames innermost-first; `line`/`column` 1-based; `name` demangled where possible), `weight(&self) -> usize`.

- [ ] **Step 1: Generate a real ELF fixture at test time** — a `tests`/build helper compiles a tiny C file with `gcc -g -O0` into an ELF in a temp dir, and `objdump`/`nm` yields a known function address. (If `gcc` is unavailable the test is `#[ignore]`d with a clear message.)

- [ ] **Step 2: Failing test** — `DartSymbols::parse(elf).resolve(addr_of_foo)` returns a frame whose function is `foo` and filename ends `.c`.

- [ ] **Step 3: Implement** — build `addr2line::Context` from `object::File::parse(elf)`; `resolve` calls `ctx.find_frames(addr)` and maps `Frame{function,location}` → `ResolvedLoc`. Demangle via `addr2line`'s function name (raw for Dart).

- [ ] **Step 4:** run pass.
- [ ] **Step 5: Commit** *(skip unless asked)*

---

### Task 3: Engine — `symbolicate_dart` + `BlobFetch::dart_symbols`

**Files:** modify `backend/crates/sauron-symbols/src/engine.rs`.

**Interfaces:**
- `BlobFetch` gains `fn dart_symbols(&self, debug_id: &str, arch: Option<&str>) -> impl Future<Output = Vec<ArtifactRef>> + Send;`
- `Symbolicator` gains a `dart_cache: ByteLru<Vec<u8>, DartSymbols>` and
  `async fn symbolicate_dart<F: BlobFetch + Sync>(&self, fetch: &F, raw_trace: &str, debug_id: Option<&str>, arch: Option<&str>) -> (Vec<ResolvedFrame>, Status)`.

- [ ] **Step 1: Failing test** with an in-memory `BlobFetch` returning the ELF fixture + a Dart trace whose `virt` = the fixture function address → assert the resolved frame is `foo`.
- [ ] **Step 2-4:** implement (parse trace → for each frame, `dart_symbols(debug_id,arch)` → parse ELF (cached) → resolve lookup_addr → ResolvedFrame; status Symbolicated/Partial/NoArtifacts/NotApplicable) → run pass.
- [ ] **Step 5: Commit** *(skip unless asked)*

---

### Task 4: Envelope — `raw_stacktrace` + `debug_meta`

**Files:** modify `backend/crates/sauron-core/src/envelope.rs`.

**Interfaces:**
- `ErrorItem` gains `#[serde(default)] raw_stacktrace: Option<String>` and `#[serde(default)] debug_meta: Option<DebugMeta>`.
- `pub struct DebugMeta { build_id: Option<String>, isolate_dso_base: Option<String>, arch: Option<String>, os: Option<String> }` (all `#[serde(default)]`).

- [ ] **Step 1:** Add the struct + fields (back-compatible; existing golden envelope still parses).
- [ ] **Step 2:** `cargo test -p sauron-core` (golden test still green).
- [ ] **Step 3: Commit** *(skip unless asked)*

---

### Task 5: Ingest + API Dart symbolication

**Files:** modify `backend/crates/sauron-pipeline/src/{process.rs,symbolize.rs}`, `backend/bins/sauron-api/src/symbolicate.rs`.

- [ ] **Step 1:** In `process_error`, when `e.raw_stacktrace` is present: persist `debug_meta` (merge `{build_id,dso_base,arch,os,raw_stacktrace}`) into `NewErrorEvent.debug_meta`, and time-boxed `symbolicate_dart` → set `stacktrace_symbolicated` + status. (JS path unchanged; pick by presence of `raw_stacktrace`.)
- [ ] **Step 2:** Add `dart_symbols` to the pipeline + API `BlobFetch` impls (`find_artifacts_by_debug_id`+arch → `ArtifactRef`; filter `kind=="dart_symbols"`).
- [ ] **Step 3:** In the API on-read helper, when `event.debug_meta.raw_stacktrace` is present, run `symbolicate_dart` (mirrors the JS branch).
- [ ] **Step 4:** `cargo build -p sauron-pipeline -p sauron-api`.
- [ ] **Step 5: Commit** *(skip unless asked)*

---

### Task 6: Flutter SDK capture

**Files:** modify `sdks/flutter/lib/src/*` (error capture + envelope model) + a test.

- [ ] **Step 1:** On `FlutterError.onError` / `PlatformDispatcher.onError` / zone errors, capture `stackTrace.toString()` verbatim into `raw_stacktrace` and assemble `debug_meta` (build_id/dso_base parsed from the trace header when present; `arch`/`os` from `Platform`).
- [ ] **Step 2:** Add `raw_stacktrace` + `debug_meta` to the JSON error item the SDK emits (mirrors `sauron-core`).
- [ ] **Step 3:** Dart unit test: given a sample obfuscated trace, the emitted error item has `raw_stacktrace` set and `debug_meta.build_id` parsed.
- [ ] **Step 4:** `cd sdks/flutter && flutter test` (or `dart test`).
- [ ] **Step 5: Commit** *(skip unless asked)*

---

### Task 7: Dashboard + CLI

**Files:** modify `dashboard/src/lib/components/StacktraceView.svelte` (raw-trace fallback), `dashboard/src/pages/IssueDetail.svelte` (pass raw trace); `backend/bins/sauron-symcli/src/main.rs` (`upload-dart`).

- [ ] **Step 1:** When an event has `debug_meta.raw_stacktrace` and no symbolicated frames, StacktraceView shows the raw Dart trace (monospace) with a "No symbols uploaded" badge.
- [ ] **Step 2:** `sauron-symcli upload-dart` — upload one symbol file with `--debug-id --arch --platform`; walking a `--split-debug-info` dir (deriving build-id from the ELF) is a documented extension.
- [ ] **Step 3:** `svelte-check` + `cargo build -p sauron-symcli`.
- [ ] **Step 4: Commit** *(skip unless asked)*

---

### Task 8: End-to-end verification (fixture-based)

- [ ] **Step 1:** Stand up pg+redis+api+ingest (as before). Compile the C ELF fixture; `objdump` a function's vaddr.
- [ ] **Step 2:** Upload the ELF as `kind=dart_symbols&platform=android&arch=arm64&debug_id=<id>`.
- [ ] **Step 3:** Send an error envelope with `raw_stacktrace` (Dart format, `virt` = the fixture vaddr) + `debug_meta.build_id=<id>`.
- [ ] **Step 4:** GET the issue detail → assert `symbolication_status == symbolicated` and a frame resolves to the C function/file.
- [ ] **Step 5:** `cargo test --workspace`, `svelte-check`, `flutter test` green. Note the real-Flutter-build follow-up.

## Self-Review

- **Spec coverage:** §6.2 Dart resolver → Tasks 1-3; §8 SDK/envelope → Tasks 4,6; §7 hybrid → Task 5; §9 dashboard → Task 7. iOS uses the same ELF/DWARF path (Flutter `--split-debug-info` emits ELF on both platforms), so `arch`/`platform` are match tags only.
- **Placeholders:** none — resolver/parser/engine carry real tests + a real ELF fixture; SDK/dashboard carry exact files + interfaces.
- **Type consistency:** `DartTrace`, `DartFrameRef`, `DartSymbols`, `symbolicate_dart`, `dart_symbols`, `DebugMeta` consistent across Tasks 1-6.
