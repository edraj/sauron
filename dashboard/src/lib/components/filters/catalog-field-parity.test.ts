import { describe, it, expect } from 'vitest';
// `?raw`, as in `filter-registry-parity.test.ts`: Vite inlines the file at
// transform time, so this needs no `@types/node` and no cwd-relative pathing.
import catalogRs from '../../../../../backend/crates/sauron-query/src/catalog.rs?raw';
import { SESSION_FIELDS, TRANSACTION_FIELDS, type FieldDef, type Op } from './filters';

/**
 * Chip lists for the query-language lists, against the catalog that decides
 * whether a chip resolves.
 *
 * Separate from `filter-registry-parity.test.ts` because the SOURCE is
 * different, not because the check is. Issues/events validate a chip against
 * `sauron-db`'s `FieldSpec` registries; sessions and transactions bridge the
 * same `filter=field:op:value` through `from_legacy` into an AST that
 * `sauron-query`'s `resolve` checks against `CATALOG`. A chip aimed at the
 * wrong one of those two lists is a 400 the reader cannot act on, and a
 * catalog dimension nobody chipped is a filter no click can reach — both
 * silent, both survive `svelte-check` and the whole vitest suite.
 */

/** `R_SESSIONS`-style aliases, resolved so a dimension's list can name one. */
function resourceAliases(): Record<string, string[]> {
  const out: Record<string, string[]> = {};
  // `R_*` and `TAGGABLE` alike — the pattern keys on the type, not the name,
  // so the tag set is resolved by the same pass.
  for (const m of catalogRs.matchAll(
    /const (\w+): &\[Resource\] = &\[([\s\S]*?)\];/g,
  )) {
    out[m[1]] = [...m[2].matchAll(/Resource::(\w+)/g)].map((r) => r[1]);
  }
  return out;
}

interface CatalogDim {
  name: string;
  /** `ValueType::Str`, `ValueType::Int`, … — the discriminant only. */
  ty: string;
  /** `OPS_TEXT`, `OPS_EQ`, `OPS_ORD`, … */
  ops: string;
}

/** Every dimension the catalog declares for `resource`. */
function catalogDims(resource: string): CatalogDim[] {
  const aliases = resourceAliases();
  const dims: CatalogDim[] = [];
  for (const block of catalogRs.matchAll(/Dimension \{([\s\S]*?)\n {4}\},/g)) {
    const body = block[1];
    const name = /name: "([^"]+)"/.exec(body)?.[1];
    const ty = /ty: ([^\n,]+)/.exec(body)?.[1]?.trim();
    const ops = /ops: ([^\n,]+)/.exec(body)?.[1]?.trim();
    const rawResources = /resources: ([\s\S]+?),\n\s*index:/.exec(body)?.[1]?.trim();
    if (!name || !ty || !ops || !rawResources) continue;
    const expanded = aliases[rawResources] ?? [
      ...rawResources.matchAll(/Resource::(\w+)/g),
    ].map((m) => m[1]);
    if (expanded.includes(resource)) dims.push({ name, ty, ops });
  }
  // `tag` is declared OUTSIDE `CATALOG`, as the standalone `TAG_DIM` whose
  // `resources: TAGGABLE` decides which lists can answer a `tag.<key>`
  // predicate at all. Sessions, Persons and Devices are NOT in that set — the
  // resolver refuses `Store::Tag` for them outright — so a tag chip on those
  // pages would be a 400, and this parity check has to see the difference.
  const tagBlock = /pub const TAG_DIM: Dimension = Dimension \{([\s\S]*?)\n\};/.exec(catalogRs);
  if (!tagBlock) throw new Error('TAG_DIM not found in catalog.rs — was it renamed?');
  const tagResources = /resources: (\w+)/.exec(tagBlock[1])?.[1] ?? '';
  if ((aliases[tagResources] ?? []).includes(resource)) {
    dims.push({
      name: 'tag',
      ty: /ty: ([^\n,]+)/.exec(tagBlock[1])?.[1]?.trim() ?? '',
      ops: /ops: ([^\n,]+)/.exec(tagBlock[1])?.[1]?.trim() ?? '',
    });
  }
  if (dims.length === 0) throw new Error(`no ${resource} dimensions found — did catalog.rs move?`);
  return dims;
}

/**
 * Chip operators the catalog's op set permits.
 *
 * Only the three the `from_legacy` bridge can emit are modelled: `eq`/`neq`
 * land as `Eq` (negated for `neq`), `contains` as `Contains`, and `gt`/`lt` as
 * the ordered pair. `In`/`Has`/`Like`/`Gte`/`Lte` exist in the catalog but no
 * chip can produce them, so they say nothing about chip validity.
 */
const OPS_ALLOWED: Record<string, Op[]> = {
  OPS_EQ: ['eq', 'neq'],
  OPS_TEXT: ['eq', 'neq', 'contains'],
  OPS_ORD: ['eq', 'gt', 'lt'],
  OPS_WORKFLOW: ['eq', 'neq', 'contains'],
};

/**
 * Dimensions with no chip, and why. Each is a decision, not a backlog item —
 * an empty-by-default list would let a real gap in as a silent addition.
 */
const UNCHIPPED: Record<string, Record<string, string>> = {
  Sessions: {
    environment: 'scoped by the topbar environment switcher, not a per-page chip',
    startedAt: 'the page owns its window through <TimeFilter>, which also picks the column',
    duration: 'ValueType::Duration accepts 2s/500ms, which the i64 chip validator rejects',
    context: 'a JSON root, addressed as the chainable @context.<key> in the query box',
  },
  Transactions: {
    duration: 'ValueType::Duration accepts 2s/500ms, which the i64 chip validator rejects',
    extra: 'a JSON root, addressed as the chainable @extra.<key> in the query box',
  },
};

const CASES: { resource: string; fields: FieldDef[]; name: string }[] = [
  { resource: 'Sessions', fields: SESSION_FIELDS, name: 'SESSION_FIELDS' },
  { resource: 'Transactions', fields: TRANSACTION_FIELDS, name: 'TRANSACTION_FIELDS' },
];

describe('FilterBar chips ↔ query catalog', () => {
  for (const { resource, fields, name } of CASES) {
    it(`${name} offers every dimension Resource::${resource} carries`, () => {
      const chips = new Set(fields.map((f) => f.key));
      const excused = UNCHIPPED[resource] ?? {};
      const missing = catalogDims(resource)
        .map((d) => d.name)
        // `tag` is declared per-resource in the catalog but chipped from the
        // shared tag registry, so it is matched by key like any other.
        .filter((n) => !chips.has(n) && !(n in excused));
      expect(missing, `Resource::${resource} carries these, but no chip can produce them`).toEqual(
        [],
      );
    });

    it(`${name} names nothing Resource::${resource} would reject`, () => {
      const declared = new Set(catalogDims(resource).map((d) => d.name));
      const unknown = fields.map((f) => f.key).filter((k) => !declared.has(k));
      expect(unknown, `no such dimension on Resource::${resource} — these 400`).toEqual([]);
    });

    it(`${name} offers only operators Resource::${resource} accepts`, () => {
      const byName = new Map(catalogDims(resource).map((d) => [d.name, d]));
      const bad: string[] = [];
      for (const f of fields) {
        const dim = byName.get(f.key);
        if (!dim) continue; // reported by the test above
        const allowed = OPS_ALLOWED[dim.ops];
        if (!allowed) throw new Error(`unmodelled op set ${dim.ops} on ${f.key}`);
        for (const op of f.ops) {
          if (!allowed.includes(op)) bad.push(`${f.key}:${op} (${dim.ops})`);
        }
      }
      expect(bad, 'these operator chips resolve to a 400').toEqual([]);
    });
  }
});
