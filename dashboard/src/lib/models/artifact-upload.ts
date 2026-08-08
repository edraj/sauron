import type { UploadArtifactParams, UploadArtifactResult } from '../api/artifacts';

/**
 * Kind-dependent shape of the symbol-artifact upload form.
 *
 * The two kinds match at symbolication time on genuinely different keys — JS on
 * (release, minified file path), Dart on debug-id alone — so the form switches
 * its field set instead of rendering one union with most inputs greyed out.
 * That switch is decided here rather than in the template so the JS path (the
 * only one that has ever shipped) can be pinned by tests: every value the JS
 * form sends is asserted in `artifact-upload.test.ts`.
 *
 * There is deliberately NO debug-id input. The server derives the id from the
 * uploaded ELF's GNU build-id note and normalizes it; a human pasting an
 * uppercase or dashed id is the one demonstrated way to produce an artifact
 * that can never match a real crash, so the field is omitted rather than
 * validated. `sauron-symcli upload-dart --debug-id` remains the escape hatch
 * for toolchains whose note cannot be read.
 */
export type ArtifactKind = 'js_sourcemap' | 'dart_symbols';

/** Dart symbols are only meaningful on a mobile platform. */
export type DartPlatform = 'android' | 'ios';

export type ArtifactPlatform = 'web' | DartPlatform;

export const ARTIFACT_KINDS: readonly { value: ArtifactKind; label: string }[] = [
  { value: 'js_sourcemap', label: 'JavaScript source map' },
  { value: 'dart_symbols', label: 'Dart symbols (Flutter)' },
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
}

export function isDart(kind: ArtifactKind): boolean {
  return kind === 'dart_symbols';
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
  return isDart(kind) ? undefined : '.map,application/json';
}

export function fileLabel(kind: ArtifactKind): string {
  return isDart(kind) ? 'Symbol file (ELF)' : 'Source map (.map)';
}

export function formTitle(kind: ArtifactKind): string {
  return isDart(kind) ? 'Upload Dart symbols' : 'Upload a source map';
}

/** The equivalent `sauron-symcli` invocation, for the CI hint under the form. */
export function cliHint(kind: ArtifactKind): string {
  return isDart(kind)
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
  if (isDart(form.kind)) {
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
export function resetAfterUpload(sent: UploadForm): Pick<UploadForm, 'release' | 'name' | 'arch'> {
  return {
    release: isDart(sent.kind) ? sent.release : '',
    name: '',
    arch: '',
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
    : isDart(kind)
      ? 'Symbols uploaded'
      : 'Source map uploaded';
  if (isDart(kind) && res.derived_debug_id) {
    return `${base} — debug id ${res.derived_debug_id}`;
  }
  return base;
}
