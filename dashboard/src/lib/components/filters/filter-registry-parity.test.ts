import { describe, it, expect } from 'vitest';
// `?raw` rather than `node:fs`: Vite inlines the file at transform time and
// `vite/client` already types the import, so this needs no `@types/node` and no
// path juggling relative to the test runner's cwd.
import filterRs from '../../../../../backend/crates/sauron-db/src/filter.rs?raw';
import { EVENT_FIELDS, ISSUE_FIELDS, OCCURRENCE_FIELDS, type FieldDef } from './filters';

/**
 * The FilterBar's chip lists against the registry the API validates against.
 *
 * This exists because the gap it checks is SILENT IN BOTH DIRECTIONS. A chip
 * for a field the backend does not accept fails at request time with a 400 the
 * user cannot act on; a backend field with no chip is a filter no amount of
 * clicking can reach, while every type checks, every test passes and the page
 * looks complete. `workflow` sat in the second state on all three lists — the
 * comment beside `PERMISSION_GATED_FILTER_FIELDS` even said so — until
 * 2026-08-18.
 *
 * Reads the Rust source rather than restating its contents: a hand-copied
 * expectation drifts in exactly the case this is meant to catch.
 */
function backendKeys(registry: string): string[] {
  const src = filterRs;
  const block = new RegExp(
    String.raw`pub const ${registry}: &\[FieldSpec\] = &\[([\s\S]*?)\n\];`,
  ).exec(src);
  if (!block) throw new Error(`${registry} not found in filter.rs — was it renamed?`);
  return [...block[1].matchAll(/key:\s*"([^"]+)"/g)].map((m) => m[1]);
}

/**
 * Backend keys with no chip, and why. Each entry is a decision someone made,
 * not a backlog item — an empty-by-default list would let a real gap in as a
 * silent addition.
 */
const DELIBERATELY_UNCHIPPED: Record<string, string> = {
  // Scoped globally through the topbar environment switcher instead of per
  // page; the backend key stays for API back-compatibility. See the note above
  // EVENT_FIELDS.
  environment: 'scoped by the topbar environment switcher, not a per-page chip',
};

const CASES: { registry: string; fields: FieldDef[]; name: string }[] = [
  { registry: 'ISSUE_FILTERS', fields: ISSUE_FIELDS, name: 'ISSUE_FIELDS' },
  { registry: 'EVENT_FILTERS', fields: EVENT_FIELDS, name: 'EVENT_FIELDS' },
  { registry: 'ERROR_EVENT_FILTERS', fields: OCCURRENCE_FIELDS, name: 'OCCURRENCE_FIELDS' },
];

describe('FilterBar chips ↔ backend filter registry', () => {
  for (const { registry, fields, name } of CASES) {
    it(`${name} offers every field ${registry} accepts`, () => {
      const chips = new Set(fields.map((f) => f.key));
      const missing = backendKeys(registry).filter(
        (k) => !chips.has(k) && !(k in DELIBERATELY_UNCHIPPED),
      );
      expect(missing, `${registry} accepts these, but no chip can produce them`).toEqual([]);
    });

    it(`${name} offers nothing ${registry} would reject`, () => {
      const accepted = new Set(backendKeys(registry));
      const extra = fields.map((f) => f.key).filter((k) => !accepted.has(k));
      expect(extra, `these chips would 400 at the API`).toEqual([]);
    });
  }

  it('every deliberately-unchipped key is still a real backend key', () => {
    // Otherwise the exemption outlives the field and quietly starts excusing
    // nothing, which is how the next gap gets waved through.
    const all = new Set(CASES.flatMap((c) => backendKeys(c.registry)));
    for (const key of Object.keys(DELIBERATELY_UNCHIPPED)) {
      expect(all.has(key), `${key} is exempted but no longer exists in filter.rs`).toBe(true);
    }
  });
});
