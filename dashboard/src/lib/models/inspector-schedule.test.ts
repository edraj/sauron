import { describe, it, expect } from 'vitest';
import {
  weekdayMaskToArray,
  weekdayArrayToMask,
  describeSchedule,
  nextRuns,
} from './inspector-schedule';

describe('weekday bitmask', () => {
  // Bit N = EXTRACT(DOW) = N, so SUNDAY IS BIT 0. Getting this backwards
  // shifts every schedule by a day and nobody notices for a week.
  it('maps bit 0 to Sunday', () => {
    expect(weekdayMaskToArray(1)).toEqual([true, false, false, false, false, false, false]);
  });

  it('round-trips every mask', () => {
    for (let m = 0; m <= 127; m += 1) {
      expect(weekdayArrayToMask(weekdayMaskToArray(m))).toBe(m);
    }
  });

  it('maps 127 to every day', () => {
    expect(weekdayMaskToArray(127).every(Boolean)).toBe(true);
  });
});

describe('describeSchedule', () => {
  it('names the days, the time and the zone', () => {
    expect(describeSchedule(0b0010100, '03:00', 'Europe/Paris')).toBe(
      'Every Tue, Thu at 03:00 (Europe/Paris)',
    );
  });

  it('says daily when every bit is set', () => {
    expect(describeSchedule(127, '03:00', 'UTC')).toBe('Every day at 03:00 (UTC)');
  });

  it('says never when no bit is set', () => {
    expect(describeSchedule(0, '03:00', 'UTC')).toBe('No scheduled runs');
  });
});

describe('nextRuns', () => {
  it('returns three future instants on set days only', () => {
    // Sunday only.
    const runs = nextRuns(1, '03:00', 'UTC', new Date('2026-08-01T00:00:00Z'));
    expect(runs).toHaveLength(3);
    for (const r of runs) {
      expect(r.getTime()).toBeGreaterThan(Date.parse('2026-08-01T00:00:00Z'));
      expect(r.getUTCDay()).toBe(0);
    }
    expect(runs[0].getTime()).toBeLessThan(runs[1].getTime());
  });

  it('returns nothing when no day is selected', () => {
    expect(nextRuns(0, '03:00', 'UTC', new Date())).toEqual([]);
  });
});
