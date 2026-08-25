// Freshness-chip + approximation-mark logic for rollup-served pages. Pure —
// the RollupChip component is only wiring around this (house rule: no
// component-render harness; logic lives here with a co-located test).
import { t } from '../i18n';
import { formatTime } from '../utils/format';
import type { RollupStatus } from './index';

export interface RollupChipView {
  label: string;
  title: string;
  tone: 'neutral' | 'warning';
}

/// Older than this and the chip turns warning-toned: the fold task is
/// normally ≤ ~1 min behind, so a 5-minute lag means it is stuck or down.
const STALE_AFTER_MS = 5 * 60 * 1000;

export function rollupChip(
  status: RollupStatus | null | undefined,
  now: Date = new Date(),
): RollupChipView | null {
  if (!status?.ready || !status.as_of) return null;
  const asOf = new Date(status.as_of);
  if (Number.isNaN(asOf.getTime())) return null;
  return {
    label: t('time.asOf', { time: formatTime(asOf) }),
    title: `${status.as_of} — ${t('time.approxNote')}`,
    tone: now.getTime() - asOf.getTime() > STALE_AFTER_MS ? 'warning' : 'neutral',
  };
}

/// `≈`-prefix a formatted figure while rollups serve the page. Exact figures
/// (plain counts) never pass through this — only sketch-derived ones
/// (distinct users, percentile latencies, medians).
export function approx(formatted: string, active: boolean): string {
  return active ? `≈${formatted}` : formatted;
}
