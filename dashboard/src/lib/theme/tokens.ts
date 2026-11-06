/**
 * Reading the design tokens back out of `app.css`.
 *
 * The tokens live in CSS because the browser is their consumer; these helpers
 * exist so tests can hold the shipped values to the spec (contrast floors,
 * "every token used is defined") instead of trusting a hex picked by eye.
 */

export type TokenMap = Record<string, string>;

/** Custom properties declared directly inside the first `selector { … }` block. */
function declarationsOf(css: string, selector: string): TokenMap {
  const start = css.indexOf(`${selector} {`);
  if (start < 0) return {};
  const open = css.indexOf('{', start);
  const close = css.indexOf('}', open);
  const out: TokenMap = {};
  const body = css.slice(open + 1, close).replace(/\/\*[\s\S]*?\*\//g, '');
  for (const m of body.matchAll(/--([a-z0-9-]+)\s*:\s*([^;]+);/gi)) {
    out[m[1]] = m[2].trim().replace(/\s+/g, ' ');
  }
  return out;
}

/**
 * The tokens of each theme, keyed without the leading `--`. Light inherits
 * every token it does not redeclare, exactly as the cascade does.
 */
export function parseThemes(css: string): { dark: TokenMap; light: TokenMap } {
  const dark = declarationsOf(css, ':root');
  const light = { ...dark, ...declarationsOf(css, "[data-theme='light']") };
  return { dark, light };
}

function luminance(hex: string): number {
  const m = /^#([0-9a-f]{6})$/i.exec(hex.trim());
  if (!m) throw new Error(`contrast() needs #rrggbb, got "${hex}"`);
  const [r, g, b] = [0, 2, 4].map((i) => {
    const c = parseInt(m[1].slice(i, i + 2), 16) / 255;
    return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

/** WCAG 2 contrast ratio between two `#rrggbb` colours (1–21). */
export function contrast(a: string, b: string): number {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (hi + 0.05) / (lo + 0.05);
}
