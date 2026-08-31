import type { PerfSeriesPoint } from './index';

/** One hour of the selected day, as the day-detail chart plots it. */
export interface DayHour {
  /** 0–23 in the VIEWER's zone — the hours the X axis lays out. */
  hour: number;
  /**
   * p95 latency in ms, or `null` for an hour that recorded no transaction.
   *
   * `null` is not `0` and the distinction is the point: an hour with no
   * traffic measured no latency, and drawing it as 0 ms would render a plunge
   * to the axis that never happened. The chart breaks its line across a
   * `null` instead.
   */
  latency: number | null;
  /** Transactions in the hour. Here `0` IS the measurement, not a gap. */
  throughput: number;
}

/**
 * Identity of the local calendar day `d` renders on.
 *
 * The series carries UTC instants but the chart the user clicked labels its
 * axis in their own zone, so "that day" has to mean the local one — otherwise
 * the modal can open on a date that disagrees with the bar that opened it.
 */
function localDayId(d: Date): number {
  return new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
}

/**
 * The 24 local hours of the local day that `bucket` falls in.
 *
 * Every hour is present whether or not the series covers it — the X axis is a
 * full day regardless of how sparse the traffic was, and an absent hour is
 * information rather than a reason to shorten the chart.
 */
export function sliceLocalDay(series: PerfSeriesPoint[], bucket: string): DayHour[] {
  const target = new Date(bucket);
  const hours: DayHour[] = Array.from({ length: 24 }, (_, hour) => ({
    hour,
    latency: null,
    throughput: 0,
  }));
  if (Number.isNaN(target.getTime())) return hours;

  const day = localDayId(target);
  for (const p of series) {
    const at = new Date(p.bucket);
    if (Number.isNaN(at.getTime()) || localDayId(at) !== day) continue;
    const slot = hours[at.getHours()];
    // Combine rather than assign: on a DST fall-back day two distinct UTC
    // buckets render in the same local hour, and overwriting would drop one.
    slot.throughput += p.throughput;
    slot.latency = slot.latency === null ? p.p95 : Math.max(slot.latency, p.p95);
  }
  return hours;
}
