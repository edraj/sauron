# Mobile Symbol Upload Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the already-working Dart/mobile symbolication pipeline reachable from the dashboard, with build-id derived server-side instead of pasted by hand, then verify the whole path against a real Flutter obfuscated build.

**Architecture:** One new pure function in `sauron-symbols` (ELF build-id extraction), wired into the existing upload handler so `debug_id` becomes optional. The dashboard's Source Maps page becomes kind-aware, swapping its field set and file filter between `js_sourcemap` and `dart_symbols`.

**Tech Stack:** Rust (`object` crate — already a dependency), Svelte 5 runes, vitest, Flutter.

## Global Constraints

- **NEVER commit and never create branches.** This repo's standing rule. Every task ends at "verify". Leave work in the working tree.
- **Backend tests:** `cargo test --workspace` from `backend/`. Clippy: `cargo clippy --workspace --all-targets -- -D warnings`.
- **Frontend tests:** `npm test` from `dashboard/`. Type check: `npm run check`.
- **Do not add a dependency.** `object 0.36` is already in `backend/Cargo.toml:108` and already parses these files for the DWARF resolver.
- **The JS upload path must not change behaviour.** Its required fields (`release`, `name`) and `.map,application/json` filter stay exactly as they are.
- **Uploaded ELFs are untrusted input.** Everything that parses one is wrapped in `catch_unwind`, matching `dart.rs:27-30`.
- **ProGuard/R8 and iOS dSYM are out of scope** — see the spec. No `KINDS` entry beyond the existing two.
- **Coordination note:** `SourceMaps.svelte` is also moved to `/admin/source-maps` by the admin-view plan (a wrapper swap: `AppShell` → `AdminShell`). This plan changes its contents. Whichever lands second resolves a small merge in that one file.

---

### Task 1: ELF build-id extraction

**Files:**
- Create: `backend/crates/sauron-symbols/src/build_id.rs`
- Modify: `backend/crates/sauron-symbols/src/lib.rs:11-22`
- Test: inline `#[cfg(test)] mod tests` in `build_id.rs`

**Interfaces:**
- Produces: `sauron_symbols::build_id_hex(elf: &[u8]) -> Result<String, SymbolError>` — lowercase hex of the GNU build-id note. Consumed by Task 2.

- [ ] **Step 1: Write the failing test**

Create `backend/crates/sauron-symbols/src/build_id.rs` with only the test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    // The same real ELF the DWARF resolver is verified against, built from
    // tests/fixtures/sample.c via `gcc -g -O0 -no-pie`.
    const ELF: &[u8] = include_bytes!("../tests/fixtures/sample.elf");

    #[test]
    fn extracts_a_lowercase_hex_build_id() {
        let id = build_id_hex(ELF).expect("sample.elf should carry a build-id note");
        assert!(!id.is_empty());
        assert_eq!(id, id.to_lowercase());
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn rejects_a_non_elf() {
        assert!(build_id_hex(b"not an elf at all").is_err());
    }

    #[test]
    fn rejects_truncated_input_without_panicking() {
        // A prefix of a real ELF: valid magic, garbage structure. This must
        // return an error, never unwind — the bytes are uploaded by users.
        assert!(build_id_hex(&ELF[..64]).is_err());
    }
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cd backend && cargo test -p sauron-symbols build_id`
Expected: FAIL to compile — `build_id_hex` not found.

- [ ] **Step 3: Implement**

Prepend to `build_id.rs`:

```rust
//! GNU build-id extraction from an uploaded ELF.
//!
//! Dart symbol artifacts match on `debug_id` alone (`arch` is accepted but
//! ignored — see `engine.rs`), and that id is the ELF's build-id. Requiring the
//! uploader to produce it by hand via `readelf -n` is most of why this upload
//! path stayed CLI-only, and a mismatched id fails *silently*: the event
//! symbolicates as `no_artifacts`, indistinguishable from never having
//! uploaded.
//!
//! The ELF is untrusted. `object` is panic-resistant, but this wraps parsing in
//! `catch_unwind` for the same reason `dart::resolve` does — a pathological
//! upload must degrade to a clean 400, never take down an API handler.

use object::{Object, ObjectSection};

use crate::content::SymbolError;

/// Lowercase hex of the ELF's GNU build-id note.
pub fn build_id_hex(elf: &[u8]) -> Result<String, SymbolError> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| build_id_inner(elf)))
        .unwrap_or_else(|_| Err(SymbolError::Corrupt("panic while reading build-id".into())))
}

fn build_id_inner(elf: &[u8]) -> Result<String, SymbolError> {
    let file =
        object::File::parse(elf).map_err(|e| SymbolError::Corrupt(format!("elf parse: {e}")))?;

    // `object` exposes the note directly; prefer it over hand-walking
    // .note.gnu.build-id so we inherit its format handling.
    if let Ok(Some(id)) = file.build_id() {
        if !id.is_empty() {
            return Ok(crate::content::hex(id));
        }
    }

    // Fall back to the section, for toolchains that emit the note in a form
    // `build_id()` does not surface.
    if let Some(section) = file.section_by_name(".note.gnu.build-id") {
        if let Ok(data) = section.data() {
            // Note layout: n_namesz(4) n_descsz(4) n_type(4) name(padded) desc.
            if data.len() >= 16 {
                let namesz = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
                let descsz = u32::from_le_bytes([data[4], data[5], data[6], data[7]]) as usize;
                let name_padded = namesz.div_ceil(4) * 4;
                let start = 12 + name_padded;
                if descsz > 0 && start + descsz <= data.len() {
                    return Ok(crate::content::hex(&data[start..start + descsz]));
                }
            }
        }
    }

    Err(SymbolError::Corrupt(
        "no GNU build-id note in this file — pass debug_id explicitly".into(),
    ))
}
```

Check `crate::content::hex`'s actual signature before using it — `lib.rs:20` re-exports `hex`, and `artifacts.rs:104` calls `sauron_symbols::hex(&sha)`, so it takes a byte slice and returns `String`.

Register the module in `backend/crates/sauron-symbols/src/lib.rs`. Add `pub mod build_id;` to the module block (lines 11-17, alphabetically before `cache`) and add to the re-exports:

```rust
pub use build_id::build_id_hex;
```

- [ ] **Step 4: Run the tests**

Run: `cd backend && cargo test -p sauron-symbols build_id`
Expected: PASS, all three.

If `extracts_a_lowercase_hex_build_id` fails because `sample.elf` carries no build-id note (gcc does not always emit one without `-Wl,--build-id`), **do not weaken the test**. Instead add a fixture that definitely has one: rebuild `sample.c` with `gcc -g -O0 -no-pie -Wl,--build-id -o sample_buildid.elf sample.c`, commit it beside the existing fixtures, and point the test at it. Record in the test comment which flags produced it, as the existing fixture comment does.

- [ ] **Step 5: Clippy**

Run: `cd backend && cargo clippy -p sauron-symbols --all-targets -- -D warnings`
Expected: clean.

---

### Task 2: Wire derivation into the upload handler

**Files:**
- Modify: `backend/bins/sauron-api/src/routes/artifacts.rs:78-176`
- Test: `backend/bins/sauron-api/tests/` (the artifacts integration test file — locate it with `ls backend/bins/sauron-api/tests/`)

**Interfaces:**
- Consumes: `sauron_symbols::build_id_hex` (Task 1).
- Produces: the upload response gains a `debug_id` field, consumed by Task 3.

Today `debug_id` is a plain optional query param (`artifacts.rs:39-40`), and `blank_to_none` collapses empty strings (`:44-46`).

- [ ] **Step 1: Derive when the kind is dart_symbols**

In `artifacts.rs`, after the existing destructuring at lines 80-87 and **before** `let mut conn = db(&state).await?;`, insert:

```rust
    // Dart artifacts match on debug_id alone, so an absent or wrong id means
    // silent non-matching later. Derive it from the ELF's build-id note when
    // the uploader did not supply one; keep an explicit value as an override
    // for toolchains whose note we cannot read.
    let derived_id = if p.kind == "dart_symbols" {
        let body = body.clone();
        tokio::task::spawn_blocking(move || sauron_symbols::build_id_hex(&body))
            .await
            .map_err(|e| ApiError::Internal(format!("build-id task failed: {e}")))?
            .ok()
    } else {
        None
    };
    let debug_id = match (debug_id, derived_id.clone()) {
        // Explicit wins, but the response reports both so a mismatch surfaces
        // now rather than as a puzzling `no_artifacts` weeks later.
        (Some(explicit), _) => Some(explicit),
        (None, Some(derived)) => Some(derived),
        (None, None) if p.kind == "dart_symbols" => {
            return Err(ApiError::BadRequest(
                "no GNU build-id note in this file — pass debug_id explicitly".into(),
            ))
        }
        (None, None) => None,
    };
```

`debug_id` is already bound from `blank_to_none(p.debug_id)` at line 84, so this shadows it. ELF parsing is CPU-bound on a file up to `symbols_max_artifact_mb`, hence `spawn_blocking` — matching the hashing and compression calls at `:99` and `:135`.

- [ ] **Step 2: Report it in both responses**

The handler returns twice — the dedupe path at `:113-120` and the created path at `:168-175`. Add `debug_id` to both JSON bodies:

```rust
                "debug_id": debug_id,
                "derived_debug_id": derived_id,
```

- [ ] **Step 3: Write integration tests**

In the artifacts integration test file, add:

1. Upload a `dart_symbols` artifact **without** `debug_id` → 201, response `debug_id` equals the fixture's known build-id, `derived_debug_id` matches.
2. Upload the same bytes again without `debug_id` → 200 with `deduped: true` (the derived id must feed `find_artifact_by_debug_id` at `:107`).
3. Upload `dart_symbols` with an explicit `debug_id=deadbeef` on a file whose note differs → 201, `debug_id == "deadbeef"`, `derived_debug_id` is the real note value.
4. Upload a `dart_symbols` body that is not an ELF → 400.
5. Upload a `js_sourcemap` with no `debug_id` → 201 and `debug_id` is null. **This is the regression guard** that derivation did not leak into the JS path.

Use the existing `sample.elf` fixture bytes; the test file will need to read them from `backend/crates/sauron-symbols/tests/fixtures/`.

- [ ] **Step 4: Run backend tests**

Run: `cd backend && cargo test --workspace`
Expected: PASS.

- [ ] **Step 5: Clippy**

Run: `cd backend && cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean.

---

### Task 3: Kind-aware upload form

**Files:**
- Modify: `dashboard/src/lib/api/artifacts.ts:38-54`
- Modify: `dashboard/src/pages/SourceMaps.svelte` (script block lines 1-105, template lines 120-145)

**Interfaces:**
- Consumes: the `debug_id` response field from Task 2.
- Produces: nothing downstream.

- [ ] **Step 1: Widen the client's response type**

In `dashboard/src/lib/api/artifacts.ts`, change `uploadArtifact`'s return type in both the signature and the `api.post` generic:

```ts
export async function uploadArtifact(
  appId: string,
  file: File,
  params: UploadArtifactParams,
): Promise<{
  id: string;
  deduped: boolean;
  blob_sha256: string;
  debug_id: string | null;
  derived_debug_id: string | null;
}> {
```

The existing `for (const [k, v] of Object.entries(params)) if (v) qs.set(k, v)` loop already skips empty values, so omitting `debug_id` needs no change.

- [ ] **Step 2: Make the form kind-aware**

In `dashboard/src/pages/SourceMaps.svelte`, replace the upload-form state (currently lines 30-35) with:

```ts
  type ArtifactKind = 'js_sourcemap' | 'dart_symbols';

  // The two kinds match on genuinely different keys — JS on release + file
  // path, Dart on debug-id — so the field set switches rather than being one
  // union with most inputs greyed out.
  let kind = $state<ArtifactKind>('js_sourcemap');
  let platform = $state<'web' | 'android' | 'ios'>('web');
  let release = $state('');
  let name = $state('');
  let debugId = $state('');
  let arch = $state('');
  let file = $state<File | null>(null);
  let uploading = $state(false);

  const isDart = $derived(kind === 'dart_symbols');

  // artifacts.rs:59-68 rejects platform 'web' only by the KINDS/PLATFORMS
  // arrays, not by pairing — but a web Dart artifact is meaningless, so keep
  // the pairing honest here.
  $effect(() => {
    platform = isDart ? 'android' : 'web';
  });
```

Replace `upload()` (lines 51-77) with:

```ts
  async function upload() {
    const appId = sessionStore.currentAppId;
    if (!appId || !file) return;
    uploading = true;
    try {
      const res = await uploadArtifact(appId, file, {
        kind,
        platform,
        release: release.trim() || undefined,
        name: isDart ? undefined : name.trim() || undefined,
        debug_id: isDart ? debugId.trim() || undefined : undefined,
        arch: isDart ? arch.trim() || undefined : undefined,
      });
      const what = isDart ? 'Symbols' : 'Source map';
      toastStore.push(
        res.deduped
          ? 'Already uploaded (deduped)'
          : isDart && res.debug_id
            ? `${what} uploaded — debug id ${res.debug_id}`
            : `${what} uploaded`,
        'success',
      );
      release = '';
      name = '';
      debugId = '';
      arch = '';
      file = null;
      await load(appId);
    } catch (e) {
      toastStore.push((e as Error).message, 'error');
    } finally {
      uploading = false;
    }
  }
```

- [ ] **Step 3: Update the template**

In the upload `Card` (lines 120-145):

- Add a kind `<select>` bound to `kind` with options "JavaScript source map" / "Dart symbols (Flutter)". `AppEnvPicker.svelte:14` notes there is no `Select` primitive in `lib/components/ui/` — a raw `<select>` is the house idiom.
- Add a platform `<select>` bound to `platform`, shown only when `isDart`, with options android / ios.
- Wrap the "Minified file path" `Input` in `{#if !isDart}`.
- Add, inside `{#if isDart}`, an `Input` bound to `debugId` labelled "Debug ID" with placeholder `derived from file`, and an `Input` bound to `arch` labelled "Arch (optional)" with placeholder `arm64`.
- Make the file input's `accept` conditional: `accept={isDart ? undefined : '.map,application/json'}`, and its label text `{isDart ? 'Symbol file (ELF)' : 'Source map (.map)'}`.

Update the page subtitle (line 117) from the JS-only copy to:

```svelte
          Upload JavaScript source maps and Flutter symbol files so minified and
          obfuscated stack traces resolve to your original code.
        </p>
```

Update the CLI hint (lines 139-145) to show `upload-dart` when `isDart`.

- [ ] **Step 4: Delete the stale comment**

`SourceMaps.svelte:31` currently reads `// Upload form (JS source maps; Dart symbols upload via the CLI).` That is now false. Replace it with a comment naming both paths.

- [ ] **Step 5: Verify**

Run: `cd dashboard && npm test && npm run check && npm run build`
Expected: PASS, 0 errors.

- [ ] **Step 6: Runtime check**

With the dev server running and a real API:
- Select "Dart symbols" → the field set swaps, platform offers android/ios, the file input drops its `.map` filter.
- Upload `backend/crates/sauron-symbols/tests/fixtures/sample.elf` with Debug ID left blank → success toast names the derived debug id, and the row appears in the table with kind `dart_symbols`.
- Switch back to "JavaScript source map" → release + minified path return, `.map` filter returns.

---

### Task 4: Real Flutter build verification

**Files:** none changed unless a defect is found.

This is the task that decides whether mobile symbolication actually works. The DWARF resolver is verified only against `gcc -g` C fixtures; what has never been observed is whether Flutter's emitted `virt` addresses line up with the symbol ELF's. A systematic offset produces confidently wrong line numbers — worse than no symbolication, because it looks like it worked.

**This cannot be done under `flutter test`.** Obfuscated AOT output does not exist in a test build at all, so a green Flutter suite is not evidence for anything here.

- [ ] **Step 1: Build the example app obfuscated**

```bash
cd examples/flutter-app
flutter build apk --release --obfuscate --split-debug-info=build/symbols
```
Expected: an APK plus one or more ELF files under `build/symbols/`.

- [ ] **Step 2: Upload the symbols through the new form**

Use the dashboard form from Task 3, Debug ID left blank. This exercises Task 1's derivation against a genuine Flutter build-id rather than a gcc fixture.

Expected: 201, and the toast names a derived debug id.

- [ ] **Step 3: Trigger an obfuscated crash on-device**

Install the release APK on a device or emulator and trigger an uncaught Dart error through the example app's existing error-triggering UI. Confirm the event reaches Sauron.

- [ ] **Step 4: Check the resolved frames against source**

Open the issue in the dashboard. Confirm the stack trace resolves to the **true** function name and line number — verified against the example app's actual source, not merely that some symbol came back.

Expected: frames name real functions at correct lines.

**If the lines are wrong but plausible**, that is the systematic-offset failure this task exists to catch. Compare `dart_trace.rs`'s `lookup_addr` (`virt` when present, else `abs - dso_base`) against what Flutter actually emitted in the raw trace header, and fix the arithmetic. Record what the real header looked like in a comment — that observation is the durable value here.

**If the build-id does not match**, check whether Flutter emits a different note than `build_id_hex` reads, and whether the id in the trace header matches the one in the ELF.

- [ ] **Step 5: Repeat for iOS if a build target is available**

`examples/flutter-app/ios/` exists. Flutter's `--split-debug-info` emits ELF on both platforms, so `platform` is only a match tag and the same path should work. "Should be identical" is exactly the class of claim this task exists to test, so run it if a Mac and device are available; if not, record explicitly that iOS remains unverified rather than implying it passed.

- [ ] **Step 6: Record the outcome**

Update the "Remaining post-v1" note in `docs/superpowers/specs/2026-07-15-source-maps-symbolication-design.md` to reflect what was verified and what was not. If Step 4 found a defect, note the root cause and the fix.

---

### Task 5: Final sweep

- [ ] **Step 1: Full test run**

```bash
cd backend && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings
cd ../dashboard && npm test && npm run check && npm run build
```
Expected: all clean.

- [ ] **Step 2: Confirm the JS path is untouched**

Upload a real `.map` with release + minified path, trigger a minified JS error, and confirm the issue still symbolicates exactly as before. Derivation must not have leaked into the JS path — Task 2's test 5 asserts this, but confirm it end to end.
