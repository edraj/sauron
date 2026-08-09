import { describe, it, expect } from 'vitest';
import {
  encodeFilters, parseFilters, ISSUE_FIELDS, EVENT_FIELDS, composeTag, splitTag,
  PERMISSION_GATED_FILTER_FIELDS, gatedFilterFields, type Filter,
} from './filters';

describe('filters codec', () => {
  const f: Filter[] = [
    { field: 'level', op: 'eq', value: 'error' },
    { field: 'culprit', op: 'contains', value: 'foo:bar' },
  ];

  it('encodes to field:op:value with encoded value', () => {
    expect(encodeFilters(f)).toEqual(['level:eq:error', 'culprit:contains:foo%3Abar']);
  });

  it('round-trips through parse', () => {
    expect(parseFilters(encodeFilters(f), ISSUE_FIELDS)).toEqual(f);
  });

  it('drops unknown fields and disallowed ops', () => {
    expect(parseFilters(['nope:eq:x', 'level:contains:err'], ISSUE_FIELDS)).toEqual([]);
  });

  it('drops entries with a malformed percent-escape instead of throwing', () => {
    expect(() => parseFilters(['level:eq:100%'], ISSUE_FIELDS)).not.toThrow();
    expect(parseFilters(['level:eq:100%'], ISSUE_FIELDS)).toEqual([]);
  });

  it('drops raw strings with fewer than two colons', () => {
    expect(parseFilters(['justafield'], ISSUE_FIELDS)).toEqual([]);
    expect(parseFilters(['level:eq'], ISSUE_FIELDS)).toEqual([]);
  });

  it('round-trips EVENT_FIELDS', () => {
    const ef: Filter[] = [{ field: 'name', op: 'contains', value: 'checkout' }];
    expect(parseFilters(encodeFilters(ef), EVENT_FIELDS)).toEqual(ef);
  });

  it('drops an environment filter from an old shared URL rather than erroring', () => {
    // The chip moved to the topbar. parseFilters already discards unknown fields,
    // so a link shared before this change still loads — it just no longer
    // constrains environment. Asserted so the graceful degradation is deliberate
    // rather than incidental.
    expect(parseFilters(['environment:eq:prod', 'name:eq:click'], EVENT_FIELDS))
      .toEqual([{ field: 'name', op: 'eq', value: 'click' }]);
  });
});

describe('tag filter', () => {
  it('round-trips a tag filter through encode/parse', () => {
    const f = [{ field: 'tag', op: 'eq' as const, value: 'region=eu' }];
    const enc = encodeFilters(f);
    expect(enc).toEqual(['tag:eq:region%3Deu']);
    expect(parseFilters(enc, ISSUE_FIELDS)).toEqual(f);
    expect(parseFilters(enc, EVENT_FIELDS)).toEqual(f);
  });

  it('composeTag/splitTag are inverse', () => {
    expect(composeTag('region', 'eu')).toBe('region=eu');
    expect(splitTag('region=eu')).toEqual({ key: 'region', value: 'eu' });
    expect(splitTag('expr=a=b')).toEqual({ key: 'expr', value: 'a=b' });
    expect(splitTag('nope')).toEqual({ key: '', value: '' });
  });

  it('both registries expose a tag field defaulting to contains, with eq available', () => {
    for (const reg of [ISSUE_FIELDS, EVENT_FIELDS]) {
      const tag = reg.find((d) => d.key === 'tag');
      expect(tag?.type).toBe('tag');
      // `contains` is first → it's the default op the FilterBar picks.
      expect(tag?.ops).toEqual(['contains', 'eq']);
    }
  });
});

describe('permission-gated filter fields', () => {
  it('names the fields the API refuses without event:read', () => {
    // Pinned as a literal rather than derived: this list mirrors
    // `reject_body_filters` in the backend, and a test that computed it from the
    // same source it is checking would agree with any drift.
    expect([...PERMISSION_GATED_FILTER_FIELDS]).toEqual(['tag', 'workflow']);
  });

  it('picks out the gated fields present in a filter set', () => {
    const filters: Filter[] = [
      { field: 'level', op: 'eq', value: 'error' },
      { field: 'tag', op: 'contains', value: 'customer=acme' },
    ];
    expect(gatedFilterFields(filters)).toEqual(['tag']);
  });

  it('returns nothing when no gated field is applied', () => {
    // The condition that keeps a genuine loss of page access from being
    // misreported as a filter problem.
    expect(gatedFilterFields([{ field: 'status', op: 'eq', value: 'unresolved' }])).toEqual([]);
  });

  it('deduplicates a field used twice', () => {
    const filters: Filter[] = [
      { field: 'tag', op: 'eq', value: 'a=1' },
      { field: 'tag', op: 'contains', value: 'b' },
    ];
    // Two chips, one permission problem — the recovery button must not offer to
    // "remove Tag and Tag filters".
    expect(gatedFilterFields(filters)).toEqual(['tag']);
  });

  it('reports both when both are applied', () => {
    const filters: Filter[] = [
      { field: 'workflow', op: 'contains', value: 'Checkout' },
      { field: 'tag', op: 'eq', value: 'a=1' },
    ];
    // Order follows the filter set, not the constant, so the message names them
    // in the order the user added them.
    expect(gatedFilterFields(filters)).toEqual(['workflow', 'tag']);
  });
});
