// Single source of truth for the intervals a monitor may run at. Mirrors the
// backend's `sauron-core::MONITOR_INTERVAL_PRESETS`; the API rejects any value
// not in this set, so keep the two in sync.

import { t, type MessageKey } from '../i18n';

export interface IntervalPreset {
  seconds: number;
  /**
   * Catalog key for the human label. A key, not a string: this list is a
   * module-level constant, and a `t()` call here would freeze the language at
   * import time. Translate with `intervalLabel` where it renders.
   */
  labelKey: MessageKey;
}

export const MONITOR_INTERVALS: IntervalPreset[] = [
  { seconds: 1, labelKey: 'monitor.interval.1s' },
  { seconds: 5, labelKey: 'monitor.interval.5s' },
  { seconds: 15, labelKey: 'monitor.interval.15s' },
  { seconds: 30, labelKey: 'monitor.interval.30s' },
  { seconds: 60, labelKey: 'monitor.interval.1m' },
  { seconds: 180, labelKey: 'monitor.interval.3m' },
  { seconds: 300, labelKey: 'monitor.interval.5m' },
  { seconds: 900, labelKey: 'monitor.interval.15m' },
  { seconds: 1800, labelKey: 'monitor.interval.30m' },
  { seconds: 3600, labelKey: 'monitor.interval.1h' },
  { seconds: 10800, labelKey: 'monitor.interval.3h' },
  { seconds: 21600, labelKey: 'monitor.interval.6h' },
  { seconds: 43200, labelKey: 'monitor.interval.12h' },
  { seconds: 86400, labelKey: 'monitor.interval.24h' },
];

/** A preset's label in the active language. Call at render time. */
export function intervalLabel(preset: IntervalPreset): string {
  return t(preset.labelKey);
}

/**
 * Human label for a stored interval; falls back to `${seconds}s` for legacy
 * values (`s` is a unit symbol, left as is in every locale). Call at render
 * time.
 */
export function formatInterval(seconds: number): string {
  const preset = MONITOR_INTERVALS.find((i) => i.seconds === seconds);
  return preset ? intervalLabel(preset) : `${seconds}s`;
}
