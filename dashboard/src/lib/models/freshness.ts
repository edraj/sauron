// Freshness-chip + approximation-mark logic for rollup-served pages. Pure —
// the RollupChip component is only wiring around this (house rule: no
// component-render harness; logic lives here with a co-located test).
import { t } from '../i18n';
import { localeStore } from '../i18n/locale.svelte';
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

/// Past this, the "as of" stamp is emphasised.
///
/// SIX HOURS, not the fifteen minutes this started at. A view cache is ALLOWED
/// to be old — `/active-users` serves up to three hours by design — so a
/// threshold inside the normal range lit the warning permanently on pages
/// behaving exactly as intended, which is both wrong and, once it is always
/// on, meaningless. The stamp is passive metadata; it should read as an alert
/// only when the age is genuinely surprising.
export const DEFAULT_STALE_AFTER_MS = 6 * 60 * 60 * 1000;

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
/// Hours and minutes, never seconds.
///
/// `formatTime` includes seconds, which is precision nobody reads on a
/// staleness stamp and which made the chip wide enough to compete with the
/// buttons beside it. Local rather than a change to `formatTime`, which the
/// rollup chip and other callers share.
function hhmm(d: Date): string {
  return d.toLocaleTimeString(localeStore.tag, { hour: '2-digit', minute: '2-digit' });
}

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
    label: t('time.asOf', { time: hhmm(at) }),
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

/// What a page should do with a cache envelope.
export interface EnvelopeStatusInput {
  /** `ViewEnvelope.state`; absent for endpoints that return no envelope. */
  state?: 'fresh' | 'stale' | 'computing';
  /** `ViewEnvelope.error` — the server's last RECOMPUTE failed. */
  error?: string | null;
  /** Whether the page currently has a payload to render. */
  hasData: boolean;
  /** `CachedView.error` — the request itself failed. */
  viewError?: string | null;
}

export interface EnvelopeStatus {
  error: string | null;
  /** Waiting on the server's first compute, with nothing to show meanwhile. */
  computing: boolean;
  /** Ask again shortly. */
  shouldPoll: boolean;
}

/// Read a cache envelope the way a page has to.
///
/// The subtlety, and the reason this is tested rather than inlined: a failed
/// server-side recompute is an HTTP **200** carrying
/// `{state:"computing", data:null, error:"…"}`. A page that reads only the
/// transport error sees a success with nothing in it, renders a skeleton and
/// polls forever — while the reason sits unread in the payload and the server's
/// failure backoff means nothing is retrying either. That is a perpetual
/// spinner with a diagnosable cause one field away, and it shipped once.
///
/// `viewError` wins when both are present: it means the read never landed, so
/// the envelope beside it is from an older response or absent entirely.
export function envelopeStatus(input: EnvelopeStatusInput): EnvelopeStatus {
  const { state, error = null, hasData, viewError = null } = input;
  const combined = viewError ?? error ?? null;
  const computing = state === 'computing' && !hasData && combined === null;
  return {
    error: combined,
    computing,
    // Never poll through a failure: the server is backing off, so asking again
    // returns the same failure and only adds load.
    shouldPoll: computing,
  };
}
