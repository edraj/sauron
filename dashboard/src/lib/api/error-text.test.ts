import { describe, expect, it } from 'vitest';
import { linesMatching } from '../testing/source-scan';

/**
 * Components read caught errors with `errorMessage`, never `instanceof Error`.
 *
 * The API client rejects with a plain `NormalizedError` object, not an Error,
 * so `e instanceof Error ? e.message : String(e)` printed "[object Object]"
 * on every failed request, and `… : fallback` silently dropped the server's
 * own reason. Source-scanned, like the css guards: the mistake type-checks.
 * Components and rune modules (`.svelte.ts`, where stores like `CachedView`
 * catch request errors): the few plain models that test `instanceof Error`
 * (load-error, route-chunk) handle the NormalizedError case first, or never
 * see one.
 */
const sources = import.meta.glob('../../**/*.{svelte,svelte.ts}', {
  query: '?raw',
  import: 'default',
  eager: true,
}) as Record<string, string>;

/** 1-based lines that read an error with `instanceof Error`, comments aside. */
const instanceofErrorLines = (src: string) => linesMatching(src, /\binstanceof Error\b/);

describe('error text', () => {
  it('no component or rune module reads a caught error with instanceof Error', () => {
    const bad: string[] = [];
    for (const [file, src] of Object.entries(sources)) {
      for (const line of instanceofErrorLines(src)) {
        bad.push(`${file.replace(/^(\.\.\/)+/, '')}:${line}`);
      }
    }
    expect(bad).toEqual([]);
  });

  it('reads rune modules as well as components', () => {
    const files = Object.keys(sources);
    expect(files.some((f) => f.endsWith('/stores/cached-view.svelte.ts'))).toBe(true);
    expect(files.some((f) => f.endsWith('.svelte'))).toBe(true);
  });

  it('flags code, not comments', () => {
    const src = [
      '<!-- the old `e instanceof Error` check printed [object Object] -->',
      '<!--',
      '  instanceof Error, again',
      '-->',
      '// instanceof Error in a line comment',
      '<script>',
      '  const m = e instanceof Error ? e.message : String(e);',
      '</script>',
    ].join('\n');
    expect(instanceofErrorLines(src)).toEqual([7]);
  });
});
