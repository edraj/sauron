import { describe, expect, it } from 'vitest';
import { sliceLocalDay, type DayHour } from './day-detail';
import type { PerfSeriesPoint } from './index';

/**
 * A bucket whose LOCAL hour is `hour` on the local day of `day`.
 *
 * Built through `setHours` rather than a literal UTC string so these tests
 * hold in every zone the suite might run in, including the half-hour offsets
 * where a UTC hour boundary is not a local one.
 */
function bucketAt(day: Date, hour: number): string {
  const d = new Date(day);
  d.setHours(hour, 0, 0, 0);
  return d.toISOString();
}

function point(bucket: string, p95: number, throughput: number): PerfSeriesPoint {
  return { bucket, p50: p95 / 2, p95, throughput };
}

const DAY = new Date(2026, 7, 27); // 27 Aug 2026, local

describe('sliceLocalDay', () => {
  it('returns one slot per local hour, in order', () => {
    const got = sliceLocalDay([point(bucketAt(DAY, 9), 100, 5)], bucketAt(DAY, 9));
    expect(got).toHaveLength(24);
    expect(got.map((h) => h.hour)).toEqual([...Array(24).keys()]);
  });

  it('places each point in its own local hour', () => {
    const series = [point(bucketAt(DAY, 3), 120, 7), point(bucketAt(DAY, 17), 340, 21)];
    const got = sliceLocalDay(series, bucketAt(DAY, 3));
    expect(got[3]).toEqual<DayHour>({ hour: 3, latency: 120, throughput: 7 });
    expect(got[17]).toEqual<DayHour>({ hour: 17, latency: 340, throughput: 21 });
  });

  /**
   * The honesty rule, and the reason this is a function with tests rather than
   * an inline `?? 0`. An hour with no transactions really did serve zero of
   * them, so the throughput line should touch the floor. It did NOT record a
   * latency of zero — nothing was measured — and drawing 0 ms would render a
   * plunge to the axis that never happened.
   */
  it('reads an empty hour as zero throughput but an absent latency', () => {
    const got = sliceLocalDay([point(bucketAt(DAY, 9), 100, 5)], bucketAt(DAY, 9));
    expect(got[10]).toEqual<DayHour>({ hour: 10, latency: null, throughput: 0 });
  });

  /**
   * Two buckets can land in the same local hour: on a DST fall-back day the
   * local 1 a.m. hour happens twice, and the backend's two distinct UTC
   * buckets both render there. Assigning would silently discard one of them —
   * an hour of traffic missing from a reporting chart, twice a year. They
   * combine instead: throughput adds, and the slot keeps the worse latency,
   * which is the one a p95 chart exists to surface.
   */
  it('combines two buckets that share a local hour instead of dropping one', () => {
    const first = bucketAt(DAY, 1);
    const second = new Date(first);
    second.setMinutes(30);
    const series = [point(first, 100, 4), point(second.toISOString(), 250, 6)];
    const got = sliceLocalDay(series, first);
    expect(got[1]).toEqual<DayHour>({ hour: 1, latency: 250, throughput: 10 });
  });

  it('ignores points that fall on other local days', () => {
    const next = new Date(DAY);
    next.setDate(next.getDate() + 1);
    const series = [point(bucketAt(DAY, 9), 100, 5), point(bucketAt(next, 9), 999, 999)];
    const got = sliceLocalDay(series, bucketAt(DAY, 9));
    expect(got[9]).toEqual<DayHour>({ hour: 9, latency: 100, throughput: 5 });
    expect(got.some((h) => h.throughput === 999)).toBe(false);
  });
});
