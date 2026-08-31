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

/// Past this, the "as of" chip turns warning-toned.
///
/// Three times the rollup chip's threshold, deliberately. A view cache is
/// ALLOWED to be old — the contract is "numbers may be up to an hour old and
/// the page says how old" — so warning at five minutes would be permanently on
/// for an endpoint behaving exactly as designed, and a warning that is always
/// lit conveys nothing.
export const DEFAULT_STALE_AFTER_MS = 15 * 60 * 1000;

export interface ViewFreshnessInput {
  /** The server's own stamp, where the endpoint discloses one. */
  computedAt?: string | null;
  /** `CachedView.fetchedAt` — when THIS BROWSER received the payload. */
  fetchedAt?: number | null;
  /** A refresh is in flight over data already on screen. */
  revalidating?: boolean;
  staleAfterMs?: number;
}

export interface ViewFreshnessView {
  label: string;
  title: string;
  updating: boolean;
  tone: 'neutral' | 'warning';
  /** Which clock produced `label`. Server truth beats local fetch time. */
  source: 'server' | 'local';
}

/// "as of 14:32" for a cached view, plus whether a refresh is running.
///
/// The server stamp wins whenever there is one. A cached endpoint can hand the
/// browser an answer it has held for hours; the browser received that answer
/// seconds ago, so the local clock would date it to "just now" — confidently
/// wrong, in the one place the reader is looking to find out how old it is.
/// The local stamp is the fallback for endpoints computed per request, where it
/// is the only clock there is and an accurate one.
export function viewFreshness(
  input: ViewFreshnessInput,
  now: Date = new Date(),
): ViewFreshnessView | null {
  const {
    computedAt,
    fetchedAt,
    revalidating = false,
    staleAfterMs = DEFAULT_STALE_AFTER_MS,
  } = input;

  let at: Date | null = null;
  let source: 'server' | 'local' = 'local';
  if (computedAt) {
    const d = new Date(computedAt);
    // A malformed stamp falls through to the local clock rather than rendering
    // "as of Invalid Date": a worse timestamp beats a broken one.
    if (!Number.isNaN(d.getTime())) {
      at = d;
      source = 'server';
    }
  }
  if (at === null && fetchedAt != null) {
    const d = new Date(fetchedAt);
    if (!Number.isNaN(d.getTime())) at = d;
  }
  if (at === null) return null;

  return {
    label: t('time.asOf', { time: formatTime(at) }),
    title: at.toISOString(),
    updating: revalidating,
    tone: now.getTime() - at.getTime() > staleAfterMs ? 'warning' : 'neutral',
    source,
  };
}

/// One view's contribution to a page-level freshness chip.
export interface FreshnessSource {
  fetchedAt?: number | null;
  revalidating?: boolean;
}

/// Collapse several `CachedView`s into the one stamp a page header shows.
///
/// Takes the OLDEST stamp, not the newest. A page is only as fresh as its
/// stalest section, and Overview loads five independently — letting a cheap
/// section that just refreshed vouch for four expensive ones still showing
/// hour-old figures is exactly the reassurance this feature exists to remove.
///
/// Sections still loading are skipped rather than suppressing the chip: they
/// are showing a skeleton, so the stamp correctly describes everything that IS
/// on screen. Only when nothing has loaded is there nothing to date.
export function combineFreshness(sources: FreshnessSource[]): FreshnessSource | null {
  const stamped = sources.filter((s): s is FreshnessSource & { fetchedAt: number } =>
    typeof s.fetchedAt === 'number',
  );
  if (stamped.length === 0) return null;
  return {
    fetchedAt: Math.min(...stamped.map((s) => s.fetchedAt)),
    revalidating: sources.some((s) => s.revalidating === true),
  };
}
