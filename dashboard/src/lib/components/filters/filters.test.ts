import { describe, it, expect } from 'vitest';
import {
  encodeFilters, parseFilters, ISSUE_FIELDS, EVENT_FIELDS, composeTag, splitTag, type Filter,
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
