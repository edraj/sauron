import { describe, it, expect } from 'vitest';
import { encodeFilters, parseFilters, ISSUE_FIELDS, EVENT_FIELDS, type Filter } from './filters';

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
