import { describe, expect, it } from 'vitest';
import {
  ARTIFACT_KINDS,
  DART_PLATFORMS,
  buildUploadParams,
  cliHint,
  fileAccept,
  fileLabel,
  formTitle,
  isDart,
  platformFor,
  resetAfterUpload,
  uploadMessage,
  type UploadForm,
} from './artifact-upload';

/** A form with every field filled, so a test can only pass by *omitting* keys. */
function filled(over: Partial<UploadForm> = {}): UploadForm {
  return {
    kind: 'js_sourcemap',
    dartPlatform: 'ios',
    release: 'web@1.4.2',
    name: '~/static/app.min.js',
    arch: 'arm64',
    ...over,
  };
}

describe('the JS source-map path is unchanged', () => {
  // These four assertions are the regression pin for making the form
  // kind-aware. Everything the JS upload sent before the Dart kind existed was
  // `{ kind: 'js_sourcemap', platform: 'web', release, name }` with `.map` on
  // the file input, and that is what must still go out.
  it('sends exactly kind, platform web, release and name', () => {
    expect(buildUploadParams(filled())).toEqual({
      kind: 'js_sourcemap',
      platform: 'web',
      release: 'web@1.4.2',
      name: '~/static/app.min.js',
    });
  });

  it('never sends arch or debug_id, even with a Dart platform and arch staged', () => {
    const params = buildUploadParams(filled({ dartPlatform: 'android' }));
    expect(params.platform).toBe('web');
    expect('arch' in params).toBe(false);
    expect('debug_id' in params).toBe(false);
  });

  it('omits blank release and name rather than sending empty strings', () => {
    expect(buildUploadParams(filled({ release: '   ', name: '' }))).toEqual({
      kind: 'js_sourcemap',
      platform: 'web',
    });
  });

  it('keeps the .map accept filter and the .map label', () => {
    expect(fileAccept('js_sourcemap')).toBe('.map,application/json');
    expect(fileLabel('js_sourcemap')).toBe('Source map (.map)');
    expect(formTitle('js_sourcemap')).toBe('Upload a source map');
  });

  it('keeps the toast wording, with no debug id appended', () => {
    expect(uploadMessage('js_sourcemap', { deduped: false, derived_debug_id: null })).toBe(
      'Source map uploaded',
    );
    expect(uploadMessage('js_sourcemap', { deduped: true, derived_debug_id: null })).toBe(
      'Already uploaded (deduped)',
    );
    // A derived id cannot come back for js_sourcemap (`artifacts.rs` only
    // derives for dart_symbols) — but if it ever did, it must not be shown as
    // the key a source map matches on, because it isn't one.
    expect(uploadMessage('js_sourcemap', { deduped: false, derived_debug_id: 'ab36cd' })).toBe(
      'Source map uploaded',
    );
  });
});

describe('the Dart symbols path', () => {
  it('sends the chosen mobile platform, release and arch', () => {
    expect(buildUploadParams(filled({ kind: 'dart_symbols' }))).toEqual({
      kind: 'dart_symbols',
      platform: 'ios',
      release: 'web@1.4.2',
      arch: 'arm64',
    });
    const android = buildUploadParams(filled({ kind: 'dart_symbols', dartPlatform: 'android' }));
    expect(android.platform).toBe('android');
  });

  it('never sends a debug_id — the server derives it from the ELF', () => {
    // The form deliberately has no debug-id input: a hand-pasted uppercase or
    // dashed id is the one way to create an artifact that can never match a
    // real crash. `UploadArtifactParams` still carries the field for the CLI's
    // sake, so absence here is worth asserting.
    for (const k of ARTIFACT_KINDS) {
      expect('debug_id' in buildUploadParams(filled({ kind: k.value }))).toBe(false);
    }
  });

  it('omits the minified file path, which Dart never matches on', () => {
    expect('name' in buildUploadParams(filled({ kind: 'dart_symbols' }))).toBe(false);
  });

  it('omits a blank arch instead of sending an empty string', () => {
    const params = buildUploadParams(filled({ kind: 'dart_symbols', arch: '  ' }));
    expect('arch' in params).toBe(false);
  });

  it('trims release and arch', () => {
    const padded = filled({ kind: 'dart_symbols', release: ' a@1 ', arch: ' arm64 ' });
    expect(buildUploadParams(padded)).toEqual({
      kind: 'dart_symbols',
      platform: 'ios',
      release: 'a@1',
      arch: 'arm64',
    });
  });

  it('drops the accept filter and relabels the file input', () => {
    // Flutter's --split-debug-info output has no fixed extension
    // (app.android-arm64.symbols, app.ios-arm64.symbols, …), so any filter
    // would hide the very files this form exists to take.
    expect(fileAccept('dart_symbols')).toBeUndefined();
    expect(fileLabel('dart_symbols')).toBe('Symbol file (ELF)');
    expect(formTitle('dart_symbols')).toBe('Upload Dart symbols');
  });

  it('names the derived debug id in the toast, on a fresh upload and a dedupe', () => {
    // The uploader never typed this id, so showing it is the only way to see
    // that derivation worked rather than assuming it did.
    expect(
      uploadMessage('dart_symbols', { deduped: false, derived_debug_id: 'ab36cd12ef' }),
    ).toBe('Symbols uploaded — debug id ab36cd12ef');
    expect(uploadMessage('dart_symbols', { deduped: true, derived_debug_id: 'ab36cd12ef' })).toBe(
      'Already uploaded (deduped) — debug id ab36cd12ef',
    );
  });

  it('still reports success when no id was derived', () => {
    // Only reachable when an explicit debug_id was supplied (the CLI's escape
    // hatch); the form cannot produce it, but the message must not read
    // "debug id null" if the shape ever changes.
    expect(uploadMessage('dart_symbols', { deduped: false, derived_debug_id: null })).toBe(
      'Symbols uploaded',
    );
  });
});

describe('kind/platform pairing', () => {
  it('classifies both kinds', () => {
    expect(isDart('dart_symbols')).toBe(true);
    expect(isDart('js_sourcemap')).toBe(false);
  });

  it('pins web to JS regardless of the remembered Dart platform', () => {
    // `artifacts.rs` validates kind and platform against flat allow-lists and
    // never checks the pairing, so a `web` Dart artifact would be accepted and
    // then never match anything. The pairing is enforced here instead.
    for (const p of DART_PLATFORMS) {
      expect(platformFor({ kind: 'js_sourcemap', dartPlatform: p.value })).toBe('web');
      expect(platformFor({ kind: 'dart_symbols', dartPlatform: p.value })).toBe(p.value);
    }
  });

  it('offers exactly the two kinds and the two mobile platforms', () => {
    expect(ARTIFACT_KINDS.map((k) => k.value)).toEqual(['js_sourcemap', 'dart_symbols']);
    expect(DART_PLATFORMS.map((p) => p.value)).toEqual(['android', 'ios']);
    expect(ARTIFACT_KINDS.map((k) => k.label)).toEqual([
      'JavaScript source map',
      'Dart symbols (Flutter)',
    ]);
  });
});

describe('post-upload reset', () => {
  it('keeps the release after a Dart upload and clears the per-file fields', () => {
    // One build, several arch variants: the release is typed once, `arch` is what
    // changes between them. Clearing it made the uploader retype the same string
    // for every file, and a release that differs by one character matches nothing.
    expect(resetAfterUpload(filled({ kind: 'dart_symbols', release: 'app@1.4.2+12' }))).toEqual({
      release: 'app@1.4.2+12',
      name: '',
      arch: '',
    });
  });

  it('clears everything after a JS upload', () => {
    expect(resetAfterUpload(filled({ kind: 'js_sourcemap', release: 'web@1.4.2' }))).toEqual({
      release: '',
      name: '',
      arch: '',
    });
  });

  it('reads the kind off the SENT form, not the live one', () => {
    // The caller snapshots the form before awaiting, so a kind switched while the
    // request was in flight cannot decide what gets cleared when it returns.
    const sent = filled({ kind: 'dart_symbols', release: 'app@2.0.0' });
    expect(resetAfterUpload(sent).release).toBe('app@2.0.0');
    expect(resetAfterUpload({ ...sent, kind: 'js_sourcemap' }).release).toBe('');
  });
});

describe('CLI hint', () => {
  it('names the subcommand that matches the picked kind', () => {
    expect(cliHint('js_sourcemap')).toContain('upload-sourcemap');
    expect(cliHint('js_sourcemap')).toContain('--name <path>');
    expect(cliHint('dart_symbols')).toContain('upload-dart');
    expect(cliHint('dart_symbols')).toContain('--platform android');
  });

  it('does not suggest --debug-id from the form', () => {
    // The CLI still accepts it; the hint under a form with no such field
    // should not imply the browser path needs one.
    expect(cliHint('dart_symbols')).not.toContain('--debug-id');
  });
});
