import { describe, it, expect } from 'vitest';
import type { SymbolArtifact } from '../api/artifacts';
import { coverageGapLabel, dartBuildCoverage, dartCoverageGaps } from './dart-coverage';

function art(over: Partial<SymbolArtifact> = {}): SymbolArtifact {
  return {
    id: `a-${Math.random()}`,
    kind: 'dart_symbols',
    platform: 'android',
    arch: 'arm64',
    release: null,
    dist: null,
    name: null,
    debug_id: 'build1',
    blob_sha256: 'ff',
    has_prebuilt_index: false,
    uncompressed_size: 1,
    compressed_size: 1,
    created_at: '2026-08-18T00:00:00Z',
    ...over,
  };
}

describe('dartBuildCoverage', () => {
  it('groups the two kinds under their shared build id', () => {
    const got = dartBuildCoverage([
      art({ kind: 'dart_symbols' }),
      art({ kind: 'dart_obfuscation_map', arch: null }),
    ]);
    expect(got).toHaveLength(1);
    expect(got[0]).toMatchObject({
      debugId: 'build1',
      hasSymbols: true,
      hasObfuscationMap: true,
      platform: 'android',
    });
  });

  it('accumulates the per-architecture symbol files of one build', () => {
    // One Flutter build emits one ELF per arch, all under the same id.
    const got = dartBuildCoverage([
      art({ arch: 'arm64' }),
      art({ arch: 'armeabi-v7a' }),
      art({ arch: 'arm64' }),
    ]);
    expect(got).toHaveLength(1);
    expect(got[0].arches).toEqual(['arm64', 'armeabi-v7a']);
  });

  it('ignores JS artifacts entirely', () => {
    expect(dartBuildCoverage([art({ kind: 'js_sourcemap', debug_id: null })])).toEqual([]);
  });

  it('skips a Dart artifact with no debug id', () => {
    // Matched on that id alone, so one without it already matches nothing —
    // a different problem from the one this list is about.
    expect(dartBuildCoverage([art({ debug_id: null })])).toEqual([]);
  });
});

describe('dartCoverageGaps', () => {
  it('is empty when every build has both halves', () => {
    // The whole point: a fully-covered app shows no warning at all.
    expect(
      dartCoverageGaps([art({ kind: 'dart_symbols' }), art({ kind: 'dart_obfuscation_map' })]),
    ).toEqual([]);
  });

  it('reports symbols with no map', () => {
    const gaps = dartCoverageGaps([art({ kind: 'dart_symbols' })]);
    expect(gaps).toHaveLength(1);
    expect(gaps[0]).toMatchObject({ hasSymbols: true, hasObfuscationMap: false });
    expect(coverageGapLabel(gaps[0])).toContain('class names do not');
  });

  it('reports a map with no symbols', () => {
    const gaps = dartCoverageGaps([art({ kind: 'dart_obfuscation_map' })]);
    expect(gaps).toHaveLength(1);
    expect(gaps[0]).toMatchObject({ hasSymbols: false, hasObfuscationMap: true });
    expect(coverageGapLabel(gaps[0])).toContain('Stack frames do not');
  });

  it('puts the likelier mistake first', () => {
    // `--split-debug-info` is the flag everyone knows; `--save-obfuscation-map`
    // is the one they have not heard of, so symbols-without-map is the common
    // gap and should lead.
    const gaps = dartCoverageGaps([
      art({ kind: 'dart_obfuscation_map', debug_id: 'onlyMap' }),
      art({ kind: 'dart_symbols', debug_id: 'onlySymbols' }),
    ]);
    expect(gaps.map((g) => g.debugId)).toEqual(['onlySymbols', 'onlyMap']);
  });

  it('reports only the incomplete builds when both kinds exist', () => {
    const gaps = dartCoverageGaps([
      art({ debug_id: 'done', kind: 'dart_symbols' }),
      art({ debug_id: 'done', kind: 'dart_obfuscation_map' }),
      art({ debug_id: 'partial', kind: 'dart_symbols' }),
    ]);
    expect(gaps.map((g) => g.debugId)).toEqual(['partial']);
  });
});
