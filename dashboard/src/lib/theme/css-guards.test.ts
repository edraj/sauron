import { describe, expect, it } from 'vitest';
// @ts-expect-error -- no @types/node in this project; the Node runtime that
// executes vitest provides this builtin regardless.
import fs from 'node:fs';
// @ts-expect-error -- no @types/node in this project; the Node runtime that
// executes vitest provides this builtin regardless.
import path from 'node:path';
import { parseThemes } from './tokens';
import { blankComments } from '../testing/source-scan';

/**
 * Guards against the drift the Quiet refresh removed.
 *
 * Every rule here was broken in dozens of places before, and each break was
 * silent: an undefined `var(--danger)` falls back to nothing and a hex picked
 * by eye ignores the theme, and no type check, test or review ever noticed.
 * Source-scanned for the same reason `refresh-parity.test.ts` is: there is no
 * runtime registry of components to ask.
 */

const sources = import.meta.glob('../../**/*.svelte', {
  query: '?raw',
  import: 'default',
  eager: true,
}) as Record<string, string>;

function svelteSources(): Record<string, string> {
  return sources;
}

/** Union of both themes' tokens: a name defined in either is defined. */
function cssDefinedTokens(): Set<string> {
  const css = fs.readFileSync(
    path.resolve(path.dirname(new URL(import.meta.url).pathname), '../../app.css'),
    'utf-8',
  );
  const { dark, light } = parseThemes(css);
  // `[lang='ar']` redeclares --font-sans; every other declared token lives in
  // the two theme blocks.
  return new Set([...Object.keys(dark), ...Object.keys(light)]);
}

const short = (p: string) => p.replace(/^(\.\.\/)+/, '');

/** Just the `<style>` blocks and inline `style=` / `style:` values. */
function styleText(src: string): string {
  const blocks = [...src.matchAll(/<style[^>]*>([\s\S]*?)<\/style>/g)].map((m) => m[1]);
  const inline = [...src.matchAll(/\bstyle(?::[\w-]+)?=(?:"([^"]*)"|\{([^}]*)\})/g)].map(
    (m) => m[1] ?? m[2] ?? '',
  );
  return [...blocks, ...inline].join('\n').replace(/\/\*[\s\S]*?\*\//g, '');
}

/** A token, `inherit`, or a size relative to the parent (`em`, not `rem`). */
function isScaleSize(v: string): boolean {
  return /^var\(--text-(2xs|xs|sm|md|lg|xl)\)$/.test(v) || v === 'inherit' || /^\d*\.?\d+em$/.test(v);
}

function isScaleWeight(v: string): boolean {
  return /^var\(--weight-(regular|medium|strong)\)$/.test(v) || ['400', '500', '600', 'inherit'].includes(v);
}

/**
 * A main column beside a fixed rail (`minmax(0, 1fr) 320px` and its drifted
 * copies). A narrow fixed column, like a button beside a field, is not one.
 */
function isRailColumns(value: string): boolean {
  const m = /1fr\)?\s+(\d+)px$/.exec(value.trim());
  return !!m && Number(m[1]) >= 240;
}

/**
 * Whether a declaration block sets anything but spacing (margin, padding).
 * An allow-list on purpose: a list of layout properties to refuse kept
 * missing one (`gap`'s longhands, the `grid` shorthand, then `align-items`).
 */
function beyondSpacing(body: string): boolean {
  const props = [...body.matchAll(/(?:^|;)\s*([-\w]+)\s*:/g)].map((m) => m[1]);
  return props.some((p) => !/^(margin|padding)(-[a-z]+)*$/.test(p));
}

/** Whether a selector targets the shared layout, compound (`div.detail-layout`) included. */
function targetsDetailLayout(selector: string): boolean {
  return /\.detail-layout(?![\w-])/.test(selector);
}

/**
 * Whether a declaration lays out a rail, through `grid-template-columns` or
 * the columns half (after `/`) of the `grid` / `grid-template` shorthands.
 */
function railDeclaration(prop: string, value: string): boolean {
  if (prop === 'grid-template-columns') return isRailColumns(value);
  if (prop === 'grid' || prop === 'grid-template') return isRailColumns(value.split('/').pop() ?? '');
  return false;
}

/**
 * Whether the layout element is exactly `<div class="detail-layout">`: no
 * inline `style`, `style:` or `class:` on it, which would re-lay it out where
 * the stylesheet checks cannot see.
 */
function bareLayoutTag(tag: string): boolean {
  return /^<div class="detail-layout">$/.test(tag.trim());
}

/** The pages laid out as a main column beside a rail. */
const DETAIL_PAGES = ['IssueDetail', 'SessionDetail', 'DeviceDetail', 'PersonProfile'];

describe('css guards', () => {
  it('every var(--x) used in a component is defined', () => {
    const defined = cssDefinedTokens();
    const missing: string[] = [];
    for (const [file, src] of Object.entries(svelteSources())) {
      for (const m of src.matchAll(/var\(--([a-z0-9-]+)/gi)) {
        const name = m[1];
        if (defined.has(name)) continue;
        // `var(--chart-${i})` in a template: the prefix of a defined family.
        if (name.endsWith('-') && [...defined].some((d) => d.startsWith(name))) continue;
        // A local custom property: declared in this file's CSS, set inline,
        // or set through a `style:--name` directive.
        const local = new RegExp(`(--${name}\\s*:|style:--${name}\\b|--${name}\\s*\\}|['"]--${name}['"])`);
        if (local.test(src)) continue;
        missing.push(`${short(file)}: --${name}`);
      }
    }
    expect([...new Set(missing)].sort()).toEqual([]);
  });

  it('font sizes come only from the six-step scale', () => {
    const bad: string[] = [];
    for (const [file, src] of Object.entries(svelteSources())) {
      for (const m of styleText(src).matchAll(/font-size:\s*([^;}\n]+)/g)) {
        if (!isScaleSize(m[1].trim())) bad.push(`${short(file)}: ${m[1].trim()}`);
      }
    }
    expect([...new Set(bad)].sort()).toEqual([]);
  });

  it('font weights are only regular, medium and strong', () => {
    const bad: string[] = [];
    for (const [file, src] of Object.entries(svelteSources())) {
      for (const m of styleText(src).matchAll(/font-weight:\s*([^;}\n]+)/g)) {
        if (!isScaleWeight(m[1].trim())) bad.push(`${short(file)}: ${m[1].trim()}`);
      }
    }
    expect([...new Set(bad)].sort()).toEqual([]);
  });

  it('pages title themselves through PageHeader, never a bare <h1>', () => {
    const bad = Object.entries(svelteSources())
      .filter(([file]) => /\/pages\/[^/]+\.svelte$/.test(file))
      // Markup only: a comment that mentions "the page <h1>" is not a heading.
      .filter(([, src]) => /<h1[\s>]/.test(blankComments(src)))
      .map(([file]) => short(file));
    expect(bad).toEqual([]);
  });

  it('multi-line fields use Textarea, never a bare <textarea>', () => {
    // Three pages each hand-copied Input's shell onto a raw textarea, and the
    // copies drifted (one had a disabled state, one a min-height).
    const bad = Object.entries(svelteSources())
      .filter(([file]) => !file.endsWith('/ui/Textarea.svelte'))
      .filter(([, src]) => /<textarea[\s>]/.test(blankComments(src)))
      .map(([file]) => short(file));
    expect(bad).toEqual([]);
  });

  it('detail pages take their rail layout from .detail-layout', () => {
    // The main-column-plus-320px-rail grid was copied into four pages and the
    // copies drifted (300px on one, `1fr` vs `minmax(0, 1fr)` when stacked).
    // app.css owns it now. A page may add spacing to it, never re-lay it out.
    const bad: string[] = [];
    const pages = Object.entries(svelteSources()).filter(([file]) => /\/pages\/[^/]+\.svelte$/.test(file));
    for (const [file, src] of pages) {
      const css = styleText(src);
      for (const m of css.matchAll(/(?<![-\w])(grid-template-columns|grid-template|grid)\s*:\s*([^;}\n]+)/g)) {
        if (railDeclaration(m[1], m[2])) bad.push(`${short(file)}: ${m[1]}: ${m[2].trim()}`);
      }
      for (const r of css.matchAll(/([^{}]+)\{([^{}]*)\}/g)) {
        if (targetsDetailLayout(r[1]) && beyondSpacing(r[2])) {
          bad.push(`${short(file)}: re-lays out .detail-layout`);
        }
      }
    }
    for (const page of DETAIL_PAGES) {
      const src = pages.find(([file]) => file.endsWith(`/pages/${page}.svelte`))?.[1] ?? '';
      const tags = [...src.matchAll(/<[a-z]+\b[^>]*\bclass="detail-layout[^"]*"[^>]*>/g)].map((m) => m[0]);
      if (tags.length === 0) bad.push(`pages/${page}.svelte: no .detail-layout`);
      for (const tag of tags) if (!bareLayoutTag(tag)) bad.push(`pages/${page}.svelte: ${tag}`);
    }
    expect(bad).toEqual([]);
  });

  it('the spacing check allows margin and padding, nothing else', () => {
    expect(beyondSpacing('margin-top: var(--page-gap);')).toBe(false);
    expect(beyondSpacing('padding-inline: 4px; margin-block-end: 8px')).toBe(false);
    // The shared rule's own properties, and every way round them.
    expect(beyondSpacing('align-items: stretch;')).toBe(true);
    expect(beyondSpacing('place-items: center;')).toBe(true);
    expect(beyondSpacing('grid: auto / 1fr 280px;')).toBe(true);
    expect(beyondSpacing('grid-template-areas: "a b";')).toBe(true);
    expect(beyondSpacing('column-gap: 40px;')).toBe(true);
    expect(beyondSpacing('display: block;')).toBe(true);
    expect(beyondSpacing('margin-top: 22px; align-items: stretch')).toBe(true);
  });

  it('the layout checks see compound selectors, shorthands and the tag itself', () => {
    expect(targetsDetailLayout('div.detail-layout')).toBe(true);
    expect(targetsDetailLayout('.page .detail-layout:hover')).toBe(true);
    expect(targetsDetailLayout('.detail-layout-x')).toBe(false);
    expect(railDeclaration('grid', 'auto / minmax(0, 1fr) 320px')).toBe(true);
    expect(railDeclaration('grid-template', 'auto / 1fr 300px')).toBe(true);
    expect(railDeclaration('grid-template-columns', 'minmax(0, 1fr) 320px')).toBe(true);
    expect(railDeclaration('grid', 'auto / 1fr 120px')).toBe(false);
    expect(bareLayoutTag('<div class="detail-layout">')).toBe(true);
    expect(bareLayoutTag('<div class="detail-layout" style="align-items: stretch">')).toBe(false);
    expect(bareLayoutTag('<div class="detail-layout" class:wide={w}>')).toBe(false);
  });

  it('the rail check tells a rail from a fixed-width column', () => {
    expect(isRailColumns('minmax(0, 1fr) 320px')).toBe(true);
    expect(isRailColumns('1fr 300px')).toBe(true);
    // A field beside a button column is not a rail.
    expect(isRailColumns('1fr 120px')).toBe(false);
    expect(isRailColumns('repeat(2, minmax(0, 1fr))')).toBe(false);
  });

  it('the scale checks reject what they exist to reject', () => {
    expect(isScaleSize('var(--text-sm)')).toBe(true);
    expect(isScaleSize('inherit')).toBe(true);
    expect(isScaleSize('0.86em')).toBe(true);
    expect(isScaleSize('13.5px')).toBe(false);
    expect(isScaleSize('0.875rem')).toBe(false);
    expect(isScaleWeight('var(--weight-medium)')).toBe(true);
    expect(isScaleWeight('500')).toBe(true);
    expect(isScaleWeight('620')).toBe(false);
  });

  it('white text sits only on the solid fills', () => {
    // `--primary` and `--chart-N` are tuned to read as text and marks ON the
    // page background; dark-theme they are light (#7c83f7), so white text on
    // them is 3.26:1. Checked per RULE: one that sets `--primary-contrast`
    // text must paint a solid fill itself (tokens.test holds those to 4.5:1),
    // and so must any state variant of it (`.go.pending:hover`). A fill set
    // from script (`style:background={…}`) cannot be read here, so a rule
    // without its own fill fails unless SOLID_PARENT names the rule that
    // paints it, and that rule is checked too.
    // A solid token, or one darkened toward black (only raises the contrast).
    const SOLID =
      /background(?:-color)?:\s*(?:var\(--(?:primary-solid|primary-solid-hover|primary-active|error-solid)\)|color-mix\(in srgb, var\(--(?:primary-solid|error-solid)\) \d+%, black\))/;
    const FILL = /background(?:-color)?:/;
    const WHITE_TEXT = /(?<![-\w])color:\s*var\(--primary-contrast\)/;
    /** File → [white-text rule, the rule whose solid fill sits behind it]. */
    const SOLID_PARENT: Record<string, [string, string]> = {
      'FunnelChart.svelte': ['.fbar-label', '.fbar'],
    };
    const bad: string[] = [];
    for (const [file, src] of Object.entries(svelteSources())) {
      const rules = [...styleText(src).matchAll(/([^{}]+)\{([^{}]*)\}/g)].map((m) => ({
        sel: m[1].trim(),
        body: m[2],
      }));
      const parent = SOLID_PARENT[file.split('/').pop()!];
      for (const r of rules) {
        if (!WHITE_TEXT.test(r.body)) continue;
        const ownFill = SOLID.test(r.body);
        const parentFill =
          parent?.[0] === r.sel && rules.some((p) => p.sel === parent[1] && SOLID.test(p.body));
        if (!ownFill && !parentFill) bad.push(`${short(file)}: ${r.sel} (no solid fill)`);
        for (const v of rules) {
          const isVariant = v.sel !== r.sel && v.sel.startsWith(r.sel) && /^[:.[]/.test(v.sel.slice(r.sel.length));
          if (isVariant && FILL.test(v.body) && !SOLID.test(v.body)) bad.push(`${short(file)}: ${v.sel}`);
        }
      }
    }
    expect([...new Set(bad)].sort()).toEqual([]);
  });

  it('no hex or rgb() colour literals in components', () => {
    const ALLOW = new Set(['EyeMark.svelte', 'Icon.svelte']);
    const found: string[] = [];
    for (const [file, src] of Object.entries(svelteSources())) {
      if (ALLOW.has(file.split('/').pop()!)) continue;
      const css = styleText(src);
      for (const m of css.matchAll(/#[0-9a-f]{3,8}\b|rgba?\([^)]*\)/gi)) {
        found.push(`${short(file)}: ${m[0]}`);
      }
    }
    expect([...new Set(found)].sort()).toEqual([]);
  });
});
