import { describe, expect, it } from 'vitest';
import { blankComments, linesMatching } from './source-scan';

describe('blankComments', () => {
  it('blanks HTML, block and line comments, keeping every newline', () => {
    const src = ['a <!-- x', 'y --> b', '/* p', ' * q */ c', 'd // e', 'f'].join('\n');
    const out = blankComments(src);
    expect(out.split('\n')).toHaveLength(6);
    expect(out.replace(/\s+/g, ' ').trim()).toBe('a b c d f');
  });

  it('keeps a URL inside a string', () => {
    expect(blankComments("const u = 'https://x.test/a';")).toBe("const u = 'https://x.test/a';");
  });

  it('reads a /* inside a line comment as part of that comment', () => {
    // page-access.ts: `// Self-scoped (/v1/me/*)` blanked its next 18 lines.
    const src = ['// Self-scoped (/v1/me/*).', 'banned();', '/** doc */'].join('\n');
    expect(linesMatching(src, /banned\(\)/)).toEqual([2]);
  });

  it('keeps comment markers inside strings', () => {
    expect(linesMatching("const g = '**/*.svelte'; banned();\n/* c */", /banned\(\)/)).toEqual([1]);
    expect(linesMatching("const s = 'a//b'; banned();", /banned\(\)/)).toEqual([1]);
    expect(linesMatching('const t = `x /* y`; banned();\n/* c */', /banned\(\)/)).toEqual([1]);
  });
});

describe('linesMatching', () => {
  it('reports 1-based lines whose code matches, never a comment', () => {
    const src = [
      '/**',
      ' * banned() in a doc comment',
      ' */',
      'ok(); // banned() after code',
      '<!-- banned() -->',
      'banned();',
    ].join('\n');
    expect(linesMatching(src, /\bbanned\(\)/)).toEqual([6]);
  });

  it('finds every line even when handed a global regex', () => {
    expect(linesMatching('banned()\nbanned()', /banned\(\)/g)).toEqual([1, 2]);
  });
});
