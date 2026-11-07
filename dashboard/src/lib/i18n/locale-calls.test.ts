import { describe, expect, it } from 'vitest';
import { linesMatching } from '../testing/source-scan';

/**
 * Case changes name the UI locale.
 *
 * A bare `toLocaleLowerCase()` uses the BROWSER's locale, not the language the
 * user picked: a Turkish or Azeri browser running the English UI turns "I"
 * into a dotless "ı". `localeStore.tag` is the UI's, as every `Intl` call in
 * `utils/format.ts` already passes it. Source-scanned: both forms type-check.
 */
const sources = import.meta.glob(['../../**/*.svelte', '../../**/*.ts', '!../../**/*.test.ts'], {
  query: '?raw',
  import: 'default',
  eager: true,
}) as Record<string, string>;

/** 1-based lines with a bare case change, comments aside. */
const bareCaseCallLines = (src: string) => linesMatching(src, /\.toLocale(?:Lower|Upper)Case\(\s*\)/);

describe('locale-sensitive calls', () => {
  it('toLocaleLowerCase / toLocaleUpperCase always name a locale', () => {
    const bad: string[] = [];
    for (const [file, src] of Object.entries(sources)) {
      for (const line of bareCaseCallLines(src)) {
        bad.push(`${file.replace(/^(\.\.\/)+/, '')}:${line}`);
      }
    }
    expect(bad).toEqual([]);
  });

  it('flags code, not comments', () => {
    const src = [
      '/**',
      ' * A bare `x.toLocaleLowerCase()` uses the browser locale.',
      ' */',
      '// so never s.toLocaleUpperCase()',
      '<!-- not title.toLocaleLowerCase() either -->',
      'const a = title.toLocaleLowerCase(localeStore.tag);',
      'const b = title.toLocaleLowerCase();',
    ].join('\n');
    expect(bareCaseCallLines(src)).toEqual([7]);
  });
});
