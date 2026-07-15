// Single source of truth for the intervals a monitor may run at. Mirrors the
// backend's `sauron-core::MONITOR_INTERVAL_PRESETS`; the API rejects any value
// not in this set, so keep the two in sync.

export interface IntervalPreset {
  seconds: number;
  label: string;
}

export const MONITOR_INTERVALS: IntervalPreset[] = [
  { seconds: 1, label: '1 second' },
  { seconds: 5, label: '5 seconds' },
  { seconds: 15, label: '15 seconds' },
  { seconds: 30, label: '30 seconds' },
  { seconds: 60, label: '1 minute' },
  { seconds: 180, label: '3 minutes' },
  { seconds: 300, label: '5 minutes' },
  { seconds: 900, label: '15 minutes' },
  { seconds: 1800, label: '30 minutes' },
  { seconds: 3600, label: '1 hour' },
  { seconds: 10800, label: '3 hours' },
  { seconds: 21600, label: '6 hours' },
  { seconds: 43200, label: '12 hours' },
  { seconds: 86400, label: '24 hours' },
];

/** Human label for a stored interval; falls back to `${seconds}s` for legacy values. */
export function formatInterval(seconds: number): string {
  return MONITOR_INTERVALS.find((i) => i.seconds === seconds)?.label ?? `${seconds}s`;
}
