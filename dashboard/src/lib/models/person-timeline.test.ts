import { describe, it, expect } from 'vitest';
import { formatOffset, personJsonFilename, personOffsetMs } from './person-timeline';

/** A row list in the order the profile page renders it: newest first. */
function descending(...isoTimes: string[]) {
  return isoTimes.map((at) => ({ at: new Date(at).getTime() }));
}

describe('personOffsetMs', () => {
  // 14:17:03 → 14:17:13, listed newest first.
  const items = descending(
    '2026-08-11T14:17:13.740Z',
    '2026-08-11T14:17:03.964Z',
    '2026-08-11T14:17:03.962Z',
    '2026-08-11T14:17:03.000Z',
  );

  it('measures from the oldest entry in start mode', () => {
    expect(items.map((_, i) => personOffsetMs(items, i, 'start'))).toEqual([10740, 964, 962, 0]);
  });

  // The row *below* is the earlier one, because the list runs newest first.
  it('measures from the chronologically previous entry in delta mode', () => {
    expect(items.map((_, i) => personOffsetMs(items, i, 'delta'))).toEqual([9776, 2, 962, null]);
  });

  // null, not 0: the oldest loaded row has no predecessor, and "+0" there
  // would report a gap that was never measured.
  it('has no delta for the last row', () => {
    expect(personOffsetMs(items, items.length - 1, 'delta')).toBeNull();
  });

  it('handles a single-entry timeline', () => {
    const one = descending('2026-08-11T14:17:03.000Z');
    expect(personOffsetMs(one, 0, 'start')).toBe(0);
    expect(personOffsetMs(one, 0, 'delta')).toBeNull();
  });

  it('returns null for an empty list or an index outside it', () => {
    expect(personOffsetMs([], 0, 'start')).toBeNull();
    expect(personOffsetMs([], 0, 'delta')).toBeNull();
    expect(personOffsetMs(items, 9, 'start')).toBeNull();
    expect(personOffsetMs(items, -1, 'delta')).toBeNull();
  });

  it('returns null for an unparseable timestamp', () => {
    const broken = [{ at: new Date('not-a-date').getTime() }, { at: 1000 }];
    expect(personOffsetMs(broken, 0, 'delta')).toBeNull();
    expect(personOffsetMs(broken, 0, 'start')).toBeNull();
  });

  // Two events ingested in the same millisecond are a real occurrence, and a
  // measured zero gap is not the same as no measurement.
  it('reports a zero gap between simultaneous entries', () => {
    const tied = descending('2026-08-11T14:17:03.000Z', '2026-08-11T14:17:03.000Z');
    expect(personOffsetMs(tied, 0, 'delta')).toBe(0);
  });

  // A list sorted the wrong way would otherwise render negative offsets.
  it('returns null rather than a negative offset', () => {
    const ascending = descending('2026-08-11T14:17:03.000Z', '2026-08-11T14:17:13.000Z');
    expect(personOffsetMs(ascending, 0, 'delta')).toBeNull();
    expect(personOffsetMs(ascending, 0, 'start')).toBeNull();
  });
});

describe('formatOffset', () => {
  // formatDuration rounds to a tenth of a second, which rendered every one of
  // these as an identical "0.0s" until the sub-second tier existed.
  it('keeps millisecond precision below a second', () => {
    expect(formatOffset(0)).toBe('<1 ms');
    expect(formatOffset(2)).toBe('2 ms');
    expect(formatOffset(962)).toBe('962 ms');
  });

  it('defers to formatDuration from a second to a day', () => {
    expect(formatOffset(1000)).toBe('1.0s');
    expect(formatOffset(1500)).toBe('1.5s');
    expect(formatOffset(90_000)).toBe('1m 30s');
    expect(formatOffset(3_600_000)).toBe('1h 00m');
    expect(formatOffset(86_399_000)).toBe('23h 59m');
  });

  // The tier that exists because a person's activity spans weeks: these would
  // otherwise read "86400.00 s" and "720h 00m".
  it('switches to days at 24 hours', () => {
    expect(formatOffset(86_400_000)).toBe('1d 00h');
    expect(formatOffset(90_000_000)).toBe('1d 01h');
    expect(formatOffset(2_592_000_000)).toBe('30d 00h');
  });

  it('renders an em dash when there is nothing to show', () => {
    expect(formatOffset(null)).toBe('—');
    expect(formatOffset(undefined)).toBe('—');
    expect(formatOffset(Number.NaN)).toBe('—');
    expect(formatOffset(-1)).toBe('—');
  });
});

describe('personJsonFilename', () => {
  it('keeps an id that is already filename-safe', () => {
    expect(personJsonFilename('u-123')).toBe('person-u-123.json');
    expect(personJsonFilename('User.42_x')).toBe('person-User.42_x.json');
  });

  // Distinct ids come from the instrumented app, so they are routinely emails.
  it('replaces characters a filename cannot carry', () => {
    expect(personJsonFilename('user@example.com')).toBe('person-user_example.com.json');
    expect(personJsonFilename('acct 7/tenant 3')).toBe('person-acct_7_tenant_3.json');
  });

  it('strips leading separators that would hide or split the file', () => {
    expect(personJsonFilename('../../etc/passwd')).toBe('person-etc_passwd.json');
    expect(personJsonFilename('.hidden')).toBe('person-hidden.json');
  });

  it('falls back when the id contributes nothing', () => {
    expect(personJsonFilename('')).toBe('person-unknown.json');
    expect(personJsonFilename('///')).toBe('person-unknown.json');
  });

  it('caps an unreasonably long id', () => {
    const name = personJsonFilename('x'.repeat(500));
    expect(name).toBe(`person-${'x'.repeat(80)}.json`);
  });
});
