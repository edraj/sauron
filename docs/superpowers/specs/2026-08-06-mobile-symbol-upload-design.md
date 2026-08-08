# Mobile symbol upload and verification

**Date:** 2026-08-06
**Status:** Approved design, not yet implemented

## Problem

Mobile symbolication is **built, tested and shipping — and unreachable from the
dashboard.**

The backend accepts `dart_symbols` artifacts on platforms `android` and `ios`
(`artifacts.rs:24-25`), stores them content-addressed, and resolves Dart AOT
frames through a full ELF/DWARF `addr2line` pipeline (`sauron-symbols/src/dart.rs:27`),
including inline-chain expansion. The Flutter SDK attaches `rawStacktrace` and
`debugMeta` whenever a trace is obfuscated (`client.dart:212-213`).

But `SourceMaps.svelte` hardcodes the upload:

```js
kind: 'js_sourcemap',
platform: 'web',
```

…at `SourceMaps.svelte:59-60`, with a file input accepting only
`.map,application/json` (`:129`) and a subtitle reading "Upload JavaScript
source maps". The page *displays* `platform` / `kind` / `arch` columns, so
CLI-uploaded Dart artifacts are visible and deletable — just not uploadable.

The only upload path is `sauron-symcli upload-dart`, which requires the user to
supply `--debug-id` by hand. Its own source concedes the gap
(`sauron-symcli/src/main.rs:14-15`): *"Dart split-debug-info directory walking +
build-id derivation lands with the Flutter pipeline in a later slice."*

Net effect: a working feature with no usable entry point. This is the
unreachable-feature class — backend and typed API client complete, zero UI call
site — and it is why mobile symbolication has gone unused.

## Scope

**In scope:**
1. Expose `dart_symbols` upload in the dashboard, with server-side build-id
   derivation.
2. Verify the whole mobile path against a real Flutter obfuscated build.

**Out of scope, deliberately:**

**Android ProGuard/R8** and **iOS dSYM** symbolication. Both were requested and
both are deferred, because neither has a data source:

- The Flutter SDK's four capture layers are all Dart-side — `FlutterError.onError`,
  `PlatformDispatcher.onError`, `runZonedGuarded`, `Isolate.addErrorListener`
  (`client.dart:110-111`, `types.dart:29-30`).
- `find sdks -name "*.kt" -o -name "*.swift" -o -name "*.java" -o -name "*.m"`
  returns nothing. There is no native Android or iOS SDK.

ProGuard/R8 deobfuscates Java/Kotlin traces; dSYM symbolicates Obj-C/Swift
crashes. Sauron currently produces neither. Building those resolvers would
create a feature that cannot be exercised except by POSTing hand-crafted traces
to the ingest API. They are revisited when a native SDK exists to feed them —
that is a separate programme (two new SDKs), not an extension of this one.

**Also out of scope:** JS debug-id + Vite plugin, JS prebuilt-index-on-upload,
and an in-proc Dart context cache — the standing post-v1 items from the original
symbolication spec.

---

## Section A — Server-side build-id derivation

### A1. Why

`dart_symbols` artifacts match on `debug_id` alone — `arch` is accepted but
ignored by both call sites (`symbolicate.rs:75`, `symbolize.rs:102` bind
`_arch`), relying on debug-id uniqueness per architecture.

Today the uploader must produce that id by hand, via `readelf -n`. Requiring a
hex string pasted from a terminal is most of why this path stayed CLI-only. A
mismatched id fails silently: the event symbolicates as `no_artifacts`, which is
indistinguishable from not having uploaded at all.

### A2. Design

On upload of `kind=dart_symbols`, parse the ELF's GNU build-id note server-side
and use it as `debug_id`. The `object` crate is already a dependency
(`Cargo.toml:108`) and already parses these files for the DWARF resolver.

- A `debug_id` query param remains accepted as a **manual override**, for
  toolchains that emit no build-id note or emit one Flutter does not use.
- When neither a note nor an override is present → 400 naming the problem, not
  a silent accept that will never match anything.
- When both are present, the override wins and the response reports both, so a
  mismatch is visible at upload time rather than at symbolication time.

New helper in `sauron-symbols` beside the existing resolver rather than in the
route, so it is unit-testable against the existing ELF fixtures
(`tests/fixtures/sample.elf`, `sample_inline.elf`).

Parsing an uploaded ELF is parsing untrusted input. Reuse the existing
protection: `dart::resolve` is already wrapped in `catch_unwind`
(`dart.rs:28`) precisely because a malformed ELF must not panic a worker. The
derivation helper takes the same treatment — a corrupt file yields a 400, never
a panic.

### A3. Response

The upload response gains the derived `debug_id` so the UI can display what the
artifact will actually match on, rather than leaving the user to trust that
derivation worked.

---

## Section B — Dashboard upload

`SourceMaps.svelte` becomes kind-aware. The page keeps its existing list,
delete, and `artifact:write` gating unchanged.

**Kind picker** — `js_sourcemap` | `dart_symbols`, driving the rest of the form:

| Field | `js_sourcemap` | `dart_symbols` |
|---|---|---|
| Platform | `web` (fixed) | `android` \| `ios` |
| Release | required | optional |
| Name (minified path) | required | not shown |
| Debug ID | not shown | optional override, placeholder "derived from file" |
| Arch | not shown | optional |
| File input `accept` | `.map,application/json` | unrestricted |

The two kinds match on genuinely different keys — JS on release + file path,
Dart on debug-id — so the form fields differ rather than being one union of
every field with most greyed out.

**Copy.** The subtitle stops saying "JavaScript" and names both pipelines. The
source comment at `SourceMaps.svelte:31` ("Dart symbols upload via the CLI")
is now false and must go.

**Existing behaviour preserved:** the JS path keeps its current required fields
and `.map` accept filter, so nothing about today's working flow changes.

This page is simultaneously moving to `/admin/source-maps` under the admin-view
spec (`2026-08-06-admin-view-and-role-management-design.md`). That move is a
wrapper swap and this is a content change; they do not conflict, but whichever
lands second should expect a small merge.

---

## Section C — Real-build verification

### C1. What is unproven

The DWARF resolver is verified only against `gcc -g -no-pie` C fixtures. The
original spec's own verification boundary states it plainly: *"Only a real
Flutter obfuscated build can confirm Flutter's emitted `virt`/ELF layout matches
ours."*

DWARF is format-identical, so the resolver logic is sound. What is unverified is
the **address arithmetic**: `lookup_addr` uses `virt` when present, else
`abs - dso_base` (`dart_trace.rs`), and whether Flutter's emitted `virt` values
line up with the symbol ELF's addresses has never been observed on real output.
A systematic offset here produces confidently wrong line numbers — worse than no
symbolication, because it looks like it worked.

### C2. Procedure

1. Build the existing `examples/flutter-app` with
   `--split-debug-info=<dir> --obfuscate` for Android.
2. Upload the emitted symbol ELF through the new dashboard form (which also
   exercises Section A's derivation against a genuine Flutter build-id).
3. Trigger a deliberate obfuscated crash on-device.
4. Confirm the issue detail resolves to the true function and line — checked
   against the app's source, not merely that *some* symbol came back.
5. Repeat for iOS if a build target is available. Flutter's
   `--split-debug-info` emits ELF on both platforms, so `platform` is only a
   match tag — but "should be identical" is exactly the class of claim this
   slice exists to test.

This must run **on a device against a real obfuscated release build**, not under
`flutter test`. Obfuscated AOT output does not exist in a test build at all, so
a green Flutter test suite is not evidence for anything in this section.
`examples/flutter-app` already carries both `android/` and `ios/` targets and
has been release-built before, so it is the vehicle.

### C3. Expected outcomes

Either the addresses line up — converting a documented assumption into a
verified fact and closing the original spec's last follow-up — or they do not,
and this slice has found a bug that no amount of unit testing would have
surfaced. Both are wins; the second is why this is scoped as its own slice with
room to fix what it finds.

### C4. Measured result (2026-08-08, real device)

Run on a physical Redmi (`camellia`, Android 13 / SDK 33), against
`examples/flutter_symbol_probe` — a purpose-built app with throw sites planted
at unequally spaced lines. **Six** probes carry `@pragma('vm:never-inline')` and
sit at lines 24, 41, 76, 128, 199, 307 — gaps 17/35/52/71/108, strictly
increasing, so no constant shift can alias one probe onto another. A **seventh**,
`probeGInner` at line 340, deliberately carries *no* pragma; its gap of 33 is not
part of the strictly-increasing argument and it exists only to observe what AOT
does when free to inline. Seven probes, seven events per ABI.

Ground truth is the line number embedded in each thrown message, asserted equal
to its own `grep -n` line immediately before every build. It is therefore
compiled into the artifact: `strings` on the *stripped* `libapp.so` yields
`SAURON_PROBE id=A line=24` … `id=G line=340`, so post-build source drift is
ruled out without relying on mtimes.

**Address arithmetic is correct. The measured delta is 0 for every probe, on
both ABIs, both locally and through the full ingest→store→read path.**

| ABI / ELF | delta series (resolved line − planted line) |
|---|---|
| arm64-v8a, ELF64, `eu-addr2line` on `virt` | `[0,0,0,0,0,0,0]` |
| arm64-v8a, ELF64, through Sauron read path | `[0,0,0,0,0,0,0]` |
| armeabi-v7a, **ELF32**, `eu-addr2line` on `virt` | `[0,0,0,0,0,0,0]` |
| armeabi-v7a, **ELF32**, through Sauron read path | `[0,0,0,0,0,0,0]` |

Three independent readings agree per probe: the planted line, the vendor oracle
(`flutter symbolize`), and our own arithmetic. Resolved function names are the
true Dart names (`probeA`…`probeGInner`) while the raw trace contains zero
readable Dart names (only `_kDartIsolateSnapshotInstructions+0x…`), so
obfuscation was genuinely active.

**The discrimination is real, not a coincidence of zeros.** Deliberately wrong
offsets applied to the same seven addresses produce distinct non-zero series:
`+0x50` → `[None,-17,-35,-52,-71,-108,-33]`; `-0x50` → `[17,35,52,71,108,33,10]`;
`+0xa0` → `[None,None,-52,-87,-123,-179,-141]`; `+0x1000` → all `None`. Only an
offset of 0 yields all zeros.

**Precision floor — what this experiment does NOT prove.** The claim is that no
**line-affecting** systematic offset exists, not that the addresses are
byte-exact. A small constant shift is invisible here because each probe body maps
entirely to one source line. Measured against the live arm64 ELF: applying `+0x4`
to all seven addresses leaves every resolved line unchanged; `probes.dart:24`
spans `0x2059a7..0x2059bb` (24 bytes) while probes are `0x50` (80 bytes) apart, so
a constant shift anywhere in roughly **−16..+4 bytes** is undetectable by this
design. That bound is what matters for users — a sub-instruction shift cannot
move a reported line — but do not cite this result as byte-exactness.

Verbatim arm64 trace header as emitted by the device:

```
os: android arch: arm64 comp: yes sim: no
build_id: 'b7188509e5f19c541ab806422af8410e'
isolate_dso_base: 7b9c2b7000, vm_dso_base: 7b9c2b7000
isolate_instructions: 7b9c38da80, vm_instructions: 7b9c377000
    #00 abs 0000007b9c4bc9b7 virt 00000000002059b7 _kDartIsolateSnapshotInstructions+0x12ef37
```

`virt` was present on every frame of all 14 captured traces, so `lookup_addr`
took the `virt` branch throughout. Measured on this output, `virt` is exactly
equal to `abs - isolate_dso_base` for every frame, so both branches agree.

Ignoring `isolate_instructions:` is correct, now measured rather than assumed.
For the header quoted above, `isolate_instructions − isolate_dso_base` is
**0xd6a80**, so resolving probe A against `isolate_instructions` asks for
`0x12ef37` rather than `0x2059b7`. Measured against that build's own ELF, it
yields `Offset.dy` inlined at `custom_paint.dart:591` — confidently wrong, and
entirely plausible.

> An earlier draft of this section gave the shift as −0xc6a80 and the wrong answer
> as `snapshot_widget.dart:299`. Those figures came from the **superseded** build
> `b71885090d142c461ab806429865810f`, not the build whose header is quoted above
> (`b7188509e5f19c541ab806422af8410e`); the two share their first four bytes, which
> is how they were conflated. Against the live ELF, −0xc6a80 lands on
> `iterable.dart:365` — matching neither stated answer. The conclusion (ignore
> `isolate_instructions`) verified true on both builds.

Negative control: arm64 addresses resolved against the ELF32 symbol file return
`focus_traversal.dart:2331` and `typed_data_patch.dart:315`. A mismatched symbol
file therefore produces plausible wrong answers rather than an error, which is
why the pass above is stated as a per-probe delta series and not as one crash
that "looked right".

**Explicitly NOT established by this run:**

- **iOS.** No Mac was available. The `platform` tag's "should be identical" claim
  is precisely the untested one.
- **`dart.rs`'s inline-chain expansion.** AOT declined to inline the probe built
  to exercise it, so `eu-addr2line -i` produced 76 frames from 76 raw frames —
  zero inline expansion anywhere in the run. The code path remains unexercised on
  real output.
- **Re-verifiability of the arm64 half from the working tree.** The arm64 symbol
  ELF and arm64 APK are no longer under `examples/flutter_symbol_probe/build`
  (only the armeabi-v7a pair survives). The arm64 result was re-confirmed by
  recovering the blob from Postgres `symbol_blobs` by sha256 — content-addressed,
  so it does not trust the working tree, but it does require DB access. Anyone
  re-checking later without it can only reproduce the arm32 half.

Two defects found in passing, neither affecting the result above:

1. `DebugMeta.fromTrace` (`sdks/flutter/lib/src/types.dart`) takes the rest of
   the `isolate_dso_base:` line, so on real Flutter output it stores
   `"7b9c2b7000, vm_dso_base: 7b9c2b7000"`. `parse_hex` in `dart_trace.rs` then
   fails on that string, so `DartTrace::dso_base` is `None` for every real
   trace. Measured: with `virt` present `lookup_addr` returns the right address
   anyway; with `virt` removed from the same real header it returns `None`, so
   the `abs - dso_base` fallback cannot resolve real Flutter traces at all.
2. `--split-debug-info` output is not regenerated by an incremental Gradle
   build. A rebuild that reuses cached AOT output leaves the symbols directory
   absent (measured: deleted, then three successive builds did not recreate it)
   while still producing an APK, so a fresh APK can pair with a stale or
   missing symbol file.

---

## Error handling

| Case | Behaviour |
|---|---|
| `dart_symbols` upload, no build-id note, no override | 400 naming the missing note |
| Corrupt / non-ELF file | 400 via `catch_unwind`, never a panic |
| Derived id and override disagree | Override wins, response reports both |
| Upload exceeds `SYMBOLS_MAX_ARTIFACT_MB` | Existing 413, unchanged |
| Caller lacks `artifact:write` | Existing 403, unchanged |
| Event arrives with an unmatched `build_id` | Existing `no_artifacts` + raw trace stored |

## Testing

**Backend:**
- Build-id derivation unit tests against both existing ELF fixtures, plus a
  truncated-file case asserting 400 rather than panic.
- Integration test: upload `dart_symbols` with no `debug_id`, assert the
  response carries the derived value and that a subsequent matching event
  symbolicates.

**Frontend:**
- The kind picker swaps the field set and the `accept` filter.
- The JS path's existing required-field validation is unchanged.

**Runtime verification:** Section C is itself the verification, and it is the
part that decides whether this feature actually works. It is not optional and
not substitutable by tests.

## Build order

1. **A** — build-id derivation + response field. Independently testable.
2. **B** — dashboard kind-aware form. Depends on A for the derivation to be
   worth exposing.
3. **C** — real Flutter build verification. Exercises A and B together and is
   the only step that can confirm the pipeline end to end.

## Decisions locked during design

| Decision | Choice | Why |
|---|---|---|
| Scope | UI exposure + verification only | ProGuard/dSYM have no data source |
| ProGuard/R8, dSYM | Deferred until a native SDK exists | Would be resolvers for traces nothing sends |
| Debug ID | Derived server-side, manual override kept | Pasting `readelf` output is why this stayed CLI-only |
| Form shape | Fields vary by kind | JS and Dart match on different keys |
