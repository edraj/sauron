import { describe, expect, it } from 'vitest';

/**
 * Every component's source, inlined at transform time.
 *
 * `import.meta.glob(..., { query: '?raw' })` rather than `node:fs`, for the
 * reason `filters/filter-registry-parity.test.ts` records: Vite resolves these
 * at build time, so the test needs no filesystem access and cannot go looking
 * in the wrong working directory.
 */
const SOURCES = import.meta.glob('../../**/*.svelte', {
  query: '?raw',
  import: 'default',
  eager: true,
}) as Record<string, string>;

/**
 * The `<tr …>` opening tags in a source, each with its full attribute list.
 *
 * Hand-scanned rather than matched with `/<tr\b[^>]*>/`, which cannot work
 * here: every handler on these rows is an arrow function, and the `>` of `=>`
 * ends the attribute run early. That version reported ZERO rows across the
 * whole dashboard and every assertion below passed on the empty set — a green
 * suite that had checked nothing.
 */
function rowTags(source: string): Array<{ tag: string; end: number }> {
  const out: Array<{ tag: string; end: number }> = [];
  const open = /<tr\b/g;
  for (const m of source.matchAll(open)) {
    let depth = 0;
    let quote = '';
    for (let i = m.index; i < source.length; i++) {
      const c = source[i];
      if (quote) {
        if (c === quote) quote = '';
      } else if (c === '"' || c === "'") quote = c;
      else if (c === '{') depth++;
      else if (c === '}') depth--;
      else if (c === '>' && depth === 0) {
        out.push({ tag: source.slice(m.index, i + 1), end: i + 1 });
        break;
      }
    }
  }
  return out;
}

/** The `<td>` a row's first cell opens — everything up to the second `<td`. */
function firstCell(source: string, rowEnd: number): string {
  const rest = source.slice(rowEnd);
  const second = rest.indexOf('<td', rest.indexOf('<td') + 1);
  return second === -1 ? rest.slice(0, 600) : rest.slice(0, second);
}

interface Row {
  file: string;
  tag: string;
  cell: string;
}

const navRows: Row[] = [];
const pushRows: Row[] = [];

for (const [file, source] of Object.entries(SOURCES)) {
  for (const { tag, end } of rowTags(source)) {
    const row: Row = { file, tag, cell: firstCell(source, end) };
    if (tag.includes('rowNav')) navRows.push(row);
    // A row that still navigates the old way: `push(...)` straight from the
    // row's own click handler, with no link anywhere in reach.
    else if (/onclick=\{\(\)\s*=>\s*(push|replace)\(/.test(tag)) pushRows.push(row);
  }
}

describe('rows wired to rowNav', () => {
  it('exist — a glob that matches nothing would pass every check below', () => {
    expect(navRows.length).toBeGreaterThanOrEqual(9);
  });

  it.each(navRows.map((r) => [r.file, r]))(
    '%s handles auxclick as well as click',
    (_file, row: Row) => {
      // The failure this exists for: `onclick` fires only for the primary
      // button, so a row with `rowNav` on click alone type-checks, renders,
      // hovers and navigates correctly — and is silently dead to the middle
      // button, which is the whole point of the change.
      expect(row.tag).toContain('onauxclick');
    },
  );

  it.each(navRows.map((r) => [r.file, r]))(
    '%s puts a real link in its first cell',
    (_file, row: Row) => {
      // Without this the row is reachable by left-click only: no "Open link in
      // new tab" in the context menu, and nothing for the keyboard to focus.
      expect(row.cell).toMatch(/<a\b[\s\S]*href=\{rowHref\(/);
    },
  );

  it.each(navRows.map((r) => [r.file, r]))(
    '%s points its link and its handler at the same path',
    (_file, row: Row) => {
      // Both sides must read the one `{@const path = …}` the row declares.
      // Spelling the destination twice is how they drift.
      expect(row.tag).toMatch(/rowNav\(e,\s*path\)/);
      expect(row.cell).toContain('href={rowHref(path)}');
    },
  );
});

describe('rows that still navigate straight from a click handler', () => {
  it('is empty — those cannot be opened in a new tab at all', () => {
    // Not a style rule: `onclick={() => push(...)}` on a `<tr>` produces a row
    // with no href, so middle-click does nothing, the context menu offers no
    // "open in new tab", and the keyboard cannot reach it. Route the row
    // through `rowNav` and give the first cell a `rowHref` anchor instead.
    expect(pushRows.map((r) => `${r.file}: ${r.tag.replace(/\s+/g, ' ')}`)).toEqual([]);
  });
});
