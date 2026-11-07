/**
 * Helpers for the source-scan guards (css-guards, error-text, locale-calls):
 * tests that read component and module source as text because the mistake
 * they catch type-checks. Test-only; nothing in the app imports this.
 *
 * A guard must read CODE, not prose: comments are where a rule gets explained,
 * and a comment naming the banned call would otherwise fail the suite (or, for
 * a "must contain" check, pass it).
 */

/**
 * One token per match, whichever starts first: a comment (Svelte/HTML
 * `<!-- -->`, block `/* *\/`, `//` to end of line) or a string literal. One
 * left-to-right pass is the point: stripped kind by kind, a `/*` inside a line
 * comment (`// Self-scoped (/v1/me/*)`) or a string (`'**\/*.svelte'`) opened
 * a block comment that blanked real code down to the next `*\/`. A `//` right
 * after `:` is not a comment, so a bare `https://…` in markup text survives.
 */
const TOKEN =
  /<!--[\s\S]*?-->|\/\*[\s\S]*?\*\/|(?<!:)\/\/[^\n]*|'(?:\\.|[^'\\\n])*'|"(?:\\.|[^"\\\n])*"|`(?:\\[\s\S]|[^`\\])*`/g;

/**
 * `src` with every comment blanked to spaces; strings are kept as they are.
 * Newlines survive, so line numbers still point at the source.
 */
export function blankComments(src: string): string {
  return src.replace(TOKEN, (tok) => (/^['"`]/.test(tok) ? tok : tok.replace(/[^\n]/g, ' ')));
}

/** 1-based numbers of the lines whose code (comments blanked) matches `pattern`. */
export function linesMatching(src: string, pattern: RegExp): number[] {
  // Without `g` or `y`: either makes `test` keep `lastIndex` between lines,
  // which skips matches.
  const re = new RegExp(pattern.source, pattern.flags.replace(/[gy]/g, ''));
  const lines: number[] = [];
  blankComments(src)
    .split('\n')
    .forEach((line, i) => {
      if (re.test(line)) lines.push(i + 1);
    });
  return lines;
}
