import { describe, expect, it } from 'vitest';

/**
 * Every component's source, inlined at transform time.
 *
 * `import.meta.glob(..., { query: '?raw', eager: true })` rather than
 * `node:fs`, for the reason `filters/filter-registry-parity.test.ts` records:
 * Vite resolves these at build time, so the test needs no filesystem access
 * and cannot go looking in the wrong working directory.
 */
const SOURCES = import.meta.glob('../../**/*.svelte', {
  query: '?raw',
  import: 'default',
  eager: true,
}) as Record<string, string>;

/**
 * Attributes whose value a user reads. Everything else — `class`, `href`,
 * `variant`, `data-*` — is machinery.
 */
const TEXT_ATTRS =
  /\b(label|title|placeholder|aria-label|description|confirmLabel|cancelLabel|heading|subtitle|tooltip|caption|hint|emptyText|message|alt)="([^"{}]{2,})"/g;

/**
 * A text node: the run between `>` and the next `<`.
 *
 * Deliberately NOT anchored on a leading capital. An earlier version was, and
 * it silently passed every prose run that follows an inline element — in
 * "An <b>environment</b> holds the DSN…" the second run starts lowercase, so a
 * capital-anchored pattern reported the whole paragraph as translated. Whole
 * pages read as clean that way.
 *
 * The `looksTranslatable` filter below carries the weight instead: a run has to
 * contain at least two words of three or more letters to count, which keeps
 * out stray fragments and punctuation without hiding sentences.
 */
const TEXT_NODE = />([^<>{}]{6,}?)</g;

/**
 * Strip everything that is not rendered prose: `<script>`, `<style>`, HTML
 * comments — and `<code>`, `<kbd>`, `<pre>`.
 *
 * Comments matter as much as the rest. This file's comments are English by
 * design, and a `<!-- … -->` block sits in the markup where the text-node
 * pattern will happily match it, so leaving them in reports a dozen
 * well-written explanations as untranslated UI.
 *
 * Text inside `code`/`kbd`/`pre` is a literal the reader must type or match
 * verbatim: a header name, a CLI flag, a SQL fragment. Translating it would
 * make the example describe something that does not exist, so it is not a leak.
 */
function markupOf(source: string): string {
  return source
    .replace(/<!--[\s\S]*?-->/g, '\n')
    .replace(/<(script|style|code|kbd|pre)\b[^>]*>[\s\S]*?<\/\1>/g, '\n');
}

/**
 * Turn a glob key into a repo-relative path.
 *
 * Vite keys these relative to *this* file, so `../components/Foo.svelte` means
 * `src/lib/components/Foo.svelte`. Resolving it properly matters only when the
 * test fails — but that is exactly the moment someone needs to open the file.
 */
function repoPath(globKey: string): string {
  const parts = 'src/lib/i18n'.split('/');
  for (const segment of globKey.split('/')) {
    if (segment === '..') parts.pop();
    else if (segment !== '.') parts.push(segment);
  }
  return parts.join('/');
}

function looksTranslatable(value: string): boolean {
  const s = value.trim();
  if (s.length < 3) return false;
  // Two or more real words. One word is usually an identifier, a unit, or a
  // fragment of markup; two is a phrase somebody wrote to be read.
  if ((s.match(/[A-Za-z]{3,}/g) ?? []).length < 2) return false;
  if (/^[A-Z_]{2,}$/.test(s)) return false; // CONSTANT_CASE
  if (/^(https?:|\/|#|\{|\.)/.test(s)) return false;
  if (/^[\d\s.,:%+-]+$/.test(s)) return false;
  return true;
}

/**
 * English text that is *correct* to leave untranslated, with the reason.
 *
 * Three kinds only:
 *
 * 1. The product name. "Sauron" is the brand; it is not translated on the
 *    login screen for the same reason "Slack" is not.
 * 2. Technology and standard names — Postgres, Parquet, Source Map v3, DWARF —
 *    and the "Enter" key cap. Arabic technical writing keeps all of these in
 *    Latin script, and the key cap has to match what is printed on the key.
 * 3. Format-example placeholders. Their *shape* is the instruction: an Arabic
 *    rendering of `smtp.example.com` or `web@1.4.2` would stop demonstrating
 *    the format the field actually accepts.
 *
 * Anything else appearing here is a genuine miss. Growing this list is meant to
 * require an argument, so each entry states one.
 */
const ALLOWED = new Set<string>([
  // 1. brand
  'Sauron',
  // 2. technology, standards and key caps
  'Postgres',
  'Parquet',
  'Source Map v3',
  'DWARF / addr2line',
  'DAU/WAU/MAU',
  'Enter',
  // 3. format examples
  'smtp.example.com',
  'sauron@example.com',
  'oncall@example.com, sre@example.com',
  '!abcdef:matrix.org',
  'production',
  'error',
  'checkout_completed',
  'web@1.4.2',
  'app@1.4.2+12',
  'arm64',
  '~/static/app.min.js',
]);

/**
 * The gate that catches a page nobody remembered to translate.
 *
 * The type system proves every catalogue key has Arabic; the catalogue tests
 * prove that Arabic is really Arabic. Neither can see a string that was never
 * put in the catalogue at all — it type-checks, renders, and looks finished.
 * This scans the markup for user-facing English literals instead, so adding
 * one without a key fails here rather than shipping an English label into an
 * otherwise Arabic UI.
 *
 * ## What it cannot see
 *
 * `markupOf` strips `<script>`, so a display string declared as data in a
 * component's script block — a table of rows, a nav array, an options list —
 * is invisible here. `/docs` had seven such reference tables, 88 strings, that
 * this test reported as clean while the page rendered them in English; they
 * were found by driving the page in the browser, not by any static gate.
 *
 * They now route through the catalogue, so nothing is currently hiding there.
 * But the gap is structural, not fixed: if you add a component that renders
 * user-facing text from a script-level array, this test will not check it.
 * Put the strings in the catalogue when you write them, and drive the page in
 * `i18n-harness` before believing a green run.
 */
describe('no untranslated strings in markup', () => {
  it('routes every user-facing literal through t()', () => {
    const leaks: string[] = [];
    for (const [path, source] of Object.entries(SOURCES)) {
      const markup = markupOf(source);
      const found = new Set<string>();
      for (const m of markup.matchAll(TEXT_ATTRS)) found.add(m[2]);
      for (const m of markup.matchAll(TEXT_NODE)) found.add(m[1]);
      for (const value of found) {
        const flat = value.replace(/\s+/g, ' ').trim();
        if (!looksTranslatable(flat) || ALLOWED.has(flat)) continue;
        leaks.push(`${repoPath(path)}: ${JSON.stringify(flat.slice(0, 90))}`);
      }
    }
    expect(leaks).toEqual([]);
  });

  it('actually reads the component sources', () => {
    // A glob that silently resolved to nothing would make the test above pass
    // unconditionally — the exact failure mode it exists to prevent.
    expect(Object.keys(SOURCES).length).toBeGreaterThan(100);
  });
});
