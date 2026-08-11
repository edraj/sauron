import { describe, it, expect } from 'vitest';
import { encodeGroupKey, decodeGroupKey, groupLabel, sameGroupKey } from './device-groups';

describe('group key URL round-trip', () => {
  it('round-trips a fully populated key', () => {
    const key = { family: 'iPhone', model: 'iPhone15,2', os_name: 'iOS', os_version: '17.4.1' };
    expect(decodeGroupKey(encodeGroupKey(key))).toEqual(key);
  });

  it('round-trips NULL components as null, not as empty string', () => {
    const key = { family: null, model: null, os_name: null, os_version: null };
    const qs = encodeGroupKey(key);
    expect(qs).toBe('group=1');
    expect(decodeGroupKey(qs)).toEqual(key);
  });

  // The case that decides whether a device with os_version = '' drills down to
  // itself or falls into the NULL group. Absent and empty must stay distinct.
  it('keeps an empty-string component distinct from an absent one', () => {
    const key = { family: 'Web', model: null, os_name: 'Windows', os_version: '' };
    const decoded = decodeGroupKey(encodeGroupKey(key));
    expect(decoded).toEqual(key);
    expect(decoded!.os_version).toBe('');
    expect(decoded!.model).toBeNull();
  });

  it('preserves values needing URL escaping', () => {
    const key = { family: 'Mac & PC', model: 'a/b c', os_name: 'iOS', os_version: '17.4.1' };
    expect(decodeGroupKey(encodeGroupKey(key))).toEqual(key);
  });

  it('returns null when the sentinel is absent, so the page stays in grouped mode', () => {
    expect(decodeGroupKey('')).toBeNull();
    expect(decodeGroupKey(null)).toBeNull();
    expect(decodeGroupKey('family=iPhone')).toBeNull();
    expect(decodeGroupKey('since_days=30')).toBeNull();
  });
});

describe('groupLabel', () => {
  it('joins device and OS halves', () => {
    expect(groupLabel({ family: 'iPhone', model: 'iPhone15,2', os_name: 'iOS', os_version: '17.4.1' }))
      .toBe('iPhone iPhone15,2 · iOS 17.4.1');
  });

  it('names the all-null group rather than rendering an empty string', () => {
    expect(groupLabel({ family: null, model: null, os_name: null, os_version: null }))
      .toBe('Unknown device');
  });

  it('falls back to one half when the other is missing', () => {
    expect(groupLabel({ family: null, model: null, os_name: 'Android', os_version: '14' }))
      .toBe('Android 14');
  });
});

// This must be null-aware, not `encodeGroupKey(a) === encodeGroupKey(b)`:
// `encodeGroupKey` omits null components, so encoding an all-null sentinel
// and encoding the REAL all-NULL group produce the identical string
// (`"group=1"`). A byte-comparison of encodings could never tell "no group
// selected" apart from "the all-NULL group is selected" — see
// DevicesInventory.svelte's URL-sync effect, the only caller.
describe('sameGroupKey', () => {
  const allNull = { family: null, model: null, os_name: null, os_version: null };
  const iphone = { family: 'iPhone', model: 'iPhone15,2', os_name: 'iOS', os_version: '17.4.1' };
  const pixel = { family: 'Pixel', model: '7 Pro', os_name: 'Android', os_version: '14' };

  it('null vs null is equal', () => {
    expect(sameGroupKey(null, null)).toBe(true);
  });

  it('null vs the all-NULL key is NOT equal — the collision this function fixes', () => {
    expect(sameGroupKey(null, allNull)).toBe(false);
    expect(sameGroupKey(allNull, null)).toBe(false);
  });

  it('the all-NULL key vs itself (a different object) is equal', () => {
    expect(sameGroupKey(allNull, { ...allNull })).toBe(true);
  });

  it('two different populated keys are NOT equal', () => {
    expect(sameGroupKey(iphone, pixel)).toBe(false);
  });

  it('the same populated key twice (different object identity) is equal', () => {
    expect(sameGroupKey(iphone, { ...iphone })).toBe(true);
  });

  it('a key differing only by null vs empty-string in one component is NOT equal', () => {
    const withNullVersion = { ...iphone, os_version: null };
    const withEmptyVersion = { ...iphone, os_version: '' };
    expect(sameGroupKey(withNullVersion, withEmptyVersion)).toBe(false);
  });
});
