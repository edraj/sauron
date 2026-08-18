import type { UploadArtifactParams, UploadArtifactResult } from '../api/artifacts';

/**
 * Kind-dependent shape of the symbol-artifact upload form.
 *
 * The kinds match at symbolication time on genuinely different keys — JS on
 * (release, minified file path), Dart on debug-id alone — so the form switches
 * its field set instead of rendering one union with most inputs greyed out.
 * That switch is decided here rather than in the template so the JS path (the
 * only one that has ever shipped) can be pinned by tests: every value the JS
 * form sends is asserted in `artifact-upload.test.ts`.
 *
 * **The debug-id input is shown for `dart_obfuscation_map` ONLY**, and that is
 * not an inconsistency. For `dart_symbols` the server derives the id from the
 * uploaded ELF's build-id note, and a human pasting one was the single
 * demonstrated way to produce an artifact that could never match a real crash —
 * so that form still has no field. A map is plain JSON with nothing identifying
 * inside it: there is no note to read, the server refuses the upload without an
 * id, and the id has to be the one the symbols upload reported. Typing it is
 * the only way in. The value is normalized server-side by the same function the
 * read path applies to `debug_meta.build_id`, so case and dashes are safe.
 */
export type ArtifactKind = 'js_sourcemap' | 'dart_symbols' | 'dart_obfuscation_map';

/** Dart symbols are only meaningful on a mobile platform. */
export type DartPlatform = 'android' | 'ios';

export type ArtifactPlatform = 'web' | DartPlatform;

export const ARTIFACT_KINDS: readonly { value: ArtifactKind; label: string }[] = [
  { value: 'js_sourcemap', label: 'JavaScript source map' },
  { value: 'dart_symbols', label: 'Dart symbols (Flutter) — stack frames' },
  { value: 'dart_obfuscation_map', label: 'Dart obfuscation map — class names' },
];

export const DART_PLATFORMS: readonly { value: DartPlatform; label: string }[] = [
  { value: 'android', label: 'Android' },
  { value: 'ios', label: 'iOS' },
];

export interface UploadForm {
  kind: ArtifactKind;
  /**
   * Consulted only for `dart_symbols`. JS source maps are always platform
   * `web`, so the picker is hidden and this value is ignored — kept as its own
   * field (rather than reset by an effect) so flipping kind back and forth
   * does not silently discard an `ios` choice.
   */
  dartPlatform: DartPlatform;
  release: string;
  name: string;
  arch: string;
  /** Consulted only for `dart_obfuscation_map` — see the note on [`ArtifactKind`]. */
  debugId: string;
}

export function isDart(kind: ArtifactKind): boolean {
  return kind === 'dart_symbols' || kind === 'dart_obfuscation_map';
}

/**
 * Whether this kind needs the uploader to type a debug id.
 *
 * True for `dart_obfuscation_map` alone. The server rejects one without an id
 * rather than storing an artifact that can never match, so this gates the
 * submit button too — a 400 the form could have prevented is a worse answer
 * than a disabled button that says why.
 */
export function requiresDebugId(kind: ArtifactKind): boolean {
  return kind === 'dart_obfuscation_map';
}

/**
 * `artifacts.rs:65-73` validates kind and platform against flat allow-lists and
 * does not check the pairing, so nothing server-side would reject a `web` Dart
 * artifact — it would simply never match anything. Keep the pairing honest here.
 */
export function platformFor(form: Pick<UploadForm, 'kind' | 'dartPlatform'>): ArtifactPlatform {
  return isDart(form.kind) ? form.dartPlatform : 'web';
}

/** `accept` for the file input; `undefined` means unrestricted. */
export function fileAccept(kind: ArtifactKind): string | undefined {
  if (kind === 'dart_obfuscation_map') return '.json,application/json';
  // Unrestricted for `dart_symbols`: the file is an ELF and its name varies by
  // toolchain (`app.android-arm64.symbols`, `app.ios-arm64.symbols`, …).
  return kind === 'dart_symbols' ? undefined : '.map,application/json';
}

export function fileLabel(kind: ArtifactKind): string {
  if (kind === 'dart_obfuscation_map') return 'Obfuscation map (JSON)';
  return kind === 'dart_symbols' ? 'Symbol file (ELF)' : 'Source map (.map)';
}

export function formTitle(kind: ArtifactKind): string {
  if (kind === 'dart_obfuscation_map') return 'Upload a Dart obfuscation map';
  return kind === 'dart_symbols' ? 'Upload Dart symbols' : 'Upload a source map';
}

/** The equivalent `sauron-symcli` invocation, for the CI hint under the form. */
export function cliHint(kind: ArtifactKind): string {
  if (kind === 'dart_obfuscation_map') {
    return 'sauron-symcli upload-obfuscation-map --api <url> --token <jwt> --app <id> --platform android --debug-id <id> obfuscation.json';
  }
  return kind === 'dart_symbols'
    ? 'sauron-symcli upload-dart --api <url> --token <jwt> --app <id> --platform android --arch arm64 app.android-arm64.symbols'
    : 'sauron-symcli upload-sourcemap --api <url> --token <jwt> --app <id> --release <r> --name <path> app.min.js.map';
}

/**
 * Query params for the upload.
 *
 * Keys are omitted rather than set to `undefined` so a field that does not
 * belong to the active kind cannot reach the wire at all — `uploadArtifact`'s
 * `if (v)` filter would drop it anyway, but absence is what the tests can pin.
 */
export function buildUploadParams(form: UploadForm): UploadArtifactParams {
  const release = form.release.trim();
  const params: UploadArtifactParams = {
    kind: form.kind,
    platform: platformFor(form),
    ...(release ? { release } : {}),
  };
  if (requiresDebugId(form.kind)) {
    // No `arch`: one obfuscation map covers a whole build, every architecture.
    // The symbols are per-arch; the renamed identifiers are not.
    const debugId = form.debugId.trim();
    if (debugId) params.debug_id = debugId;
  } else if (isDart(form.kind)) {
    const arch = form.arch.trim();
    if (arch) params.arch = arch;
  } else {
    const name = form.name.trim();
    if (name) params.name = name;
  }
  return params;
}

/**
 * The text fields to put back after a **successful** upload.
 *
 * `release` survives a Dart upload. One Flutter build emits one symbols ELF per
 * architecture (`arm64`, `armv7`, `x86_64`), and the release is the same string
 * for all of them while `arch` is what changes — clearing it made the uploader
 * retype an `app@1.4.2+12` for every variant, and a retyped release that differs
 * by one character is an artifact that matches nothing.
 *
 * The JS path keeps its existing clear-everything behaviour; it is a separate
 * judgement call (a release there usually comes with several differently-named
 * maps) and nothing about it changed here.
 *
 * Takes the form that was actually SENT, not the live one — see `upload()` in
 * `SourceMaps.svelte`.
 */
export function resetAfterUpload(
  sent: UploadForm,
): Pick<UploadForm, 'release' | 'name' | 'arch' | 'debugId'> {
  return {
    release: isDart(sent.kind) ? sent.release : '',
    name: '',
    arch: '',
    // Cleared, unlike `release`: a build needs exactly ONE map, so the id that
    // was just used is the one value that cannot be wanted again. Leaving it
    // would make the obvious next action — the map for the NEXT build — a
    // silent dedupe against the previous one.
    debugId: '',
  };
}

/**
 * Success toast.
 *
 * For Dart the derived debug id is named explicitly: it is the only key the
 * artifact will ever be matched on, and the whole point of deriving it
 * server-side is that the uploader never typed it — so showing it is the only
 * way to see that derivation worked rather than assuming it did.
 *
 * The JS wording is unchanged from before this form became kind-aware.
 */
export function uploadMessage(
  kind: ArtifactKind,
  res: Pick<UploadArtifactResult, 'deduped' | 'derived_debug_id'>,
): string {
  const base = res.deduped
    ? 'Already uploaded (deduped)'
    : kind === 'dart_obfuscation_map'
      ? 'Obfuscation map uploaded'
      : kind === 'dart_symbols'
        ? 'Symbols uploaded'
        : 'Source map uploaded';
  // Only `dart_symbols` derives one — a map has no note to read, so its id came
  // from the uploader and echoing it back would confirm nothing.
  if (kind === 'dart_symbols' && res.derived_debug_id) {
    return `${base} — debug id ${res.derived_debug_id}`;
  }
  return base;
}
