import { describe, it, expect } from 'vitest';
import {
  encodeFilters, parseFilters, ISSUE_FIELDS, EVENT_FIELDS, composeTag, splitTag,
  PERMISSION_GATED_FILTER_FIELDS, gatedFilterFields, isNumericFilterValue,
  isFilterValueValid, normalizeFilterValue, type Filter, type FieldDef,
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

  it('keeps a numeric filter a string across the URL round-trip', () => {
    // `parseFilters` always yields strings, so a chip added in the UI has to be
    // a string too — otherwise the same filter is a number when you type it and
    // a string when you reload the page, and `Filter.value: string` is a lie.
    const nf: Filter[] = [{ field: 'times_seen', op: 'gt', value: '100' }];
    expect(encodeFilters(nf)).toEqual(['times_seen:gt:100']);
    const back = parseFilters(encodeFilters(nf), ISSUE_FIELDS);
    expect(back).toEqual(nf);
    expect(typeof back[0].value).toBe('string');
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

describe('isNumericFilterValue', () => {
  // The reference is `value.parse::<i64>()` in
  // backend/crates/sauron-db/src/filter.rs — anything this disagrees with
  // becomes a FilterError::BadValue that fails the whole issues request.
  it('accepts plain and signed integers', () => {
    expect(isNumericFilterValue('0')).toBe(true);
    expect(isNumericFilterValue('42')).toBe(true);
    expect(isNumericFilterValue('-5')).toBe(true);
    expect(isNumericFilterValue('+5')).toBe(true);
  });

  it('accepts the full i64 range, which Number cannot represent', () => {
    expect(isNumericFilterValue('9223372036854775807')).toBe(true);
    expect(isNumericFilterValue('-9223372036854775808')).toBe(true);
  });

  it('rejects values just outside i64', () => {
    expect(isNumericFilterValue('9223372036854775808')).toBe(false);
    expect(isNumericFilterValue('-9223372036854775809')).toBe(false);
  });

  // Every one of these is truthy or numeric to JavaScript and rejected by
  // Rust, which is exactly why this is not a `Number()` check.
  it('rejects what Number would accept but the server will not', () => {
    expect(isNumericFilterValue('1e3')).toBe(false);
    expect(isNumericFilterValue('3.5')).toBe(false);
    expect(isNumericFilterValue('3.0')).toBe(false);
    expect(isNumericFilterValue('0x10')).toBe(false);
    expect(isNumericFilterValue(' 7 ')).toBe(false);
    expect(isNumericFilterValue('')).toBe(false);
    expect(isNumericFilterValue('Infinity')).toBe(false);
  });

  it('rejects non-numeric text', () => {
    expect(isNumericFilterValue('lots')).toBe(false);
    expect(isNumericFilterValue('-')).toBe(false);
  });
});

describe('isFilterValueValid', () => {
  const num = ISSUE_FIELDS.find((f) => f.key === 'times_seen')!;
  const str = ISSUE_FIELDS.find((f) => f.key === 'culprit')!;
  const enm = ISSUE_FIELDS.find((f) => f.key === 'level')!;

  it('rejects an unknown field', () => {
    expect(isFilterValueValid(undefined, '5')).toBe(false);
  });

  it('rejects the empty value for every type', () => {
    expect(isFilterValueValid(num, '')).toBe(false);
    expect(isFilterValueValid(str, '')).toBe(false);
    expect(isFilterValueValid(enm, '')).toBe(false);
  });

  // The reported bug: `bind:value` on <input type="number"> wrote back `null`
  // when the field was cleared, `null === ''` is false, so the old guard let a
  // null value through and the chip encoded to `times_seen:eq:null`.
  it('rejects the non-string values a numberlike binding used to produce', () => {
    expect(isFilterValueValid(num, null as unknown as string)).toBe(false);
    expect(isFilterValueValid(num, undefined as unknown as string)).toBe(false);
    expect(isFilterValueValid(num, 5 as unknown as string)).toBe(false);
  });

  it('applies the i64 rule to number fields only', () => {
    expect(isFilterValueValid(num, '42')).toBe(true);
    expect(isFilterValueValid(num, '3.5')).toBe(false);
    // The same text is a perfectly good substring search.
    expect(isFilterValueValid(str, '3.5')).toBe(true);
  });

  it('holds enum values to the declared options', () => {
    expect(isFilterValueValid(enm, 'error')).toBe(true);
    expect(isFilterValueValid(enm, 'banana')).toBe(false);
  });

  it('accepts any non-empty string field value', () => {
    expect(isFilterValueValid(str, 'TypeError')).toBe(true);
    expect(isFilterValueValid(str, ' ')).toBe(true);
  });

  it('does not crash on an enum def with no options', () => {
    const broken: FieldDef = { key: 'x', label: 'X', type: 'enum', ops: ['eq'] };
    expect(isFilterValueValid(broken, 'anything')).toBe(false);
  });
});

describe('normalizeFilterValue', () => {
  const num = ISSUE_FIELDS.find((f) => f.key === 'times_seen')!;
  const str = ISSUE_FIELDS.find((f) => f.key === 'culprit')!;

  it('trims number values, which Rust\'s from_str would reject with whitespace', () => {
    expect(normalizeFilterValue(num, '  7  ')).toBe('7');
  });

  it('leaves other values verbatim, spaces included', () => {
    expect(normalizeFilterValue(str, ' foo ')).toBe(' foo ');
  });

  // It sits on a bind:value, so it must survive an input type change rather
  // than reintroducing the `.trim is not a function` crash at the call site.
  it('survives the shapes a numberlike binding produces', () => {
    expect(normalizeFilterValue(num, 7)).toBe('7');
    expect(normalizeFilterValue(num, null)).toBe('');
    expect(normalizeFilterValue(num, undefined)).toBe('');
    expect(normalizeFilterValue(str, 7)).toBe('7');
  });

  it('composes with the validator: a cleared number field is rejected', () => {
    expect(isFilterValueValid(num, normalizeFilterValue(num, null))).toBe(false);
    expect(isFilterValueValid(num, normalizeFilterValue(num, '  7  '))).toBe(true);
  });
});
