import { describe, expect, it, vi } from 'vitest';
import { ARTIFACT_DEFAULT_SORT, artifactAccessor } from './artifact-sort';
import { sortRows } from './sort-rows';
import type { SortDir } from './sort';
import type { SymbolArtifact } from '../api/artifacts';

/**
 * Defaults are CONSTANTS, never derived from another field, and any field a
 * test does not distinguish ties across that test's rows — so an accessor that
 * reads a neighbour either collates differently or collapses to input order,
 * and input order is never the expected order.
 */
function art(over: Partial<SymbolArtifact> & { id: string }): SymbolArtifact {
  return {
    kind: 'js_sourcemap',
    platform: 'javascript',
    arch: null,
    release: 'web@1.0.0',
    dist: null,
    name: 'app.min.js',
    debug_id: null,
    blob_sha256: 'sha',
    has_prebuilt_index: false,
    uncompressed_size: 1024,
    compressed_size: 512,
    created_at: '2026-03-01T10:00:00Z',
    ...over,
  };
}

const order = (rows: SymbolArtifact[], key: string, dir: SortDir): string[] =>
  sortRows(rows, artifactAccessor(key), dir).map((a) => a.id);

describe('artifactAccessor', () => {
  it('orders Size by bytes, not by the formatted string', () => {
    // This is the whole reason the accessors live outside the component.
    // `fmtBytes` renders 9 500 000 as "9.1 MB" and 900 as "900 B"; ordering
    // that text descending gives "900 B" first. Bytes give the real answer.
    //
    // The rows are `tiny` / `medium` / `huge` rather than `b` / `kb` / `mb`
    // because unit names collate in magnitude order (b < kb < mb), which made
    // `size: (a) => a.id` — the row label read as if it were the column — pass
    // both assertions. These three collate the other way round.
    const rows = [
      art({ id: 'medium', uncompressed_size: 9500 }),
      art({ id: 'tiny', uncompressed_size: 900 }),
      art({ id: 'huge', uncompressed_size: 9_500_000 }),
    ];
    expect(order(rows, 'size', 'desc')).toEqual(['huge', 'medium', 'tiny']);
    expect(order(rows, 'size', 'asc')).toEqual(['tiny', 'medium', 'huge']);
  });

  it('orders Release, keeping an artifact with no release last both ways', () => {
    // The trap is `a.release ?? ''`: an empty string collates BEFORE every
    // real release, so a release-less artifact would lead the ascending list
    // as though it belonged at the start of the alphabet. It has no release,
    // which is not the same as having the smallest one.
    const rows = [
      art({ id: 'v2', release: 'web@2.0.0' }),
      art({ id: 'none', release: null }),
      art({ id: 'v1', release: 'web@1.0.0' }),
    ];
    expect(order(rows, 'release', 'asc')).toEqual(['v1', 'v2', 'none']);
    expect(order(rows, 'release', 'desc')).toEqual(['v2', 'v1', 'none']);
  });

  it('orders File by name, falling back to debug_id exactly as the cell does', () => {
    // `release` ties, so an accessor reading it instead collapses to input
    // order — which differs from every expected order below.
    const rows = [
      art({ id: 'dart', name: null, debug_id: 'aaa-debug-id' }),
      art({ id: 'js', name: 'zz.min.js', debug_id: null }),
      art({ id: 'neither', name: null, debug_id: null }),
    ];
    expect(order(rows, 'file', 'asc')).toEqual(['dart', 'js', 'neither']);
    expect(order(rows, 'file', 'desc')).toEqual(['js', 'dart', 'neither']);
  });

  it('orders Platform by the platform/arch pair the cell renders', () => {
    // All three are `android`; only the arch differs. An accessor spelled
    // `a.platform` calls them equal, leaving input order — which is not the
    // expected order — so the mutant dies here.
    const rows = [
      art({ id: 'x86', platform: 'android', arch: 'x86_64' }),
      art({ id: 'arm', platform: 'android', arch: 'arm64' }),
      art({ id: 'bare', platform: 'android', arch: null }),
    ];
    expect(order(rows, 'platform', 'asc')).toEqual(['bare', 'arm', 'x86']);
  });

  it('orders Kind and Uploaded by their own fields, and neither by the row id', () => {
    // Kind order and upload order disagree, so neither accessor can satisfy
    // the other's assertion. The earliest version of this fixture had them
    // agreeing and asserted `kind` in one direction only, which made
    // `kind: (a) => a.created_at` a mutant the whole file survived.
    //
    // THREE rows, not two, and that is the counting bound rather than taste:
    // two rows admit only two orderings, Kind and Uploaded take one each, so
    // whatever the ids are called they reproduce one of the two and either
    // `kind: (a) => a.id` or `uploaded: (a) => a.id` passes. With `js-old` /
    // `dart-new` it was Kind. A third row buys an id order that is neither:
    //   kind     asc → a-dart-mid, z-js-new, m-proguard-old
    //   uploaded asc → m-proguard-old, a-dart-mid, z-js-new
    //   ids      asc → a-dart-mid, m-proguard-old, z-js-new
    // `SymbolArtifact.kind` is a plain `string` on the wire, so the third value
    // is a kind this build has not seen — which is also the honest shape.
    // Every other field is the fixture's constant, so an accessor reading one
    // of those collapses to input order, which is none of the four answers.
    const rows = [
      art({ id: 'z-js-new', kind: 'js_sourcemap', created_at: '2026-03-09T10:00:00Z' }),
      art({ id: 'm-proguard-old', kind: 'proguard_mapping', created_at: '2026-03-01T10:00:00Z' }),
      art({ id: 'a-dart-mid', kind: 'dart_symbols', created_at: '2026-03-05T10:00:00Z' }),
    ];
    expect(order(rows, 'kind', 'asc')).toEqual(['a-dart-mid', 'z-js-new', 'm-proguard-old']);
    expect(order(rows, 'kind', 'desc')).toEqual(['m-proguard-old', 'z-js-new', 'a-dart-mid']);
    expect(order(rows, 'uploaded', 'desc')).toEqual(['z-js-new', 'a-dart-mid', 'm-proguard-old']);
    expect(order(rows, 'uploaded', 'asc')).toEqual(['m-proguard-old', 'a-dart-mid', 'z-js-new']);
  });

  it('falls back to Uploaded for an unknown key, and says so in dev', () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    // Release order runs opposite to Uploaded order, so a fallback to any
    // other column would show up here.
    const rows = [
      art({ id: 'older', created_at: '2026-03-01T10:00:00Z', release: 'zzz' }),
      art({ id: 'newer', created_at: '2026-03-09T10:00:00Z', release: 'aaa' }),
    ];
    expect(order(rows, 'no-such-column', 'desc')).toEqual(['newer', 'older']);
    expect(ARTIFACT_DEFAULT_SORT).toEqual({ key: 'uploaded', dir: 'desc' });
    expect(warn).toHaveBeenCalled();
    expect(String(warn.mock.calls[0]?.[0])).toContain('no-such-column');
    warn.mockRestore();
  });

  it('does not warn for a known key', () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    order([art({ id: 'a' })], 'size', 'desc');
    expect(warn).not.toHaveBeenCalled();
    warn.mockRestore();
  });
});
