import { api } from './client';
import type { Overview } from '../models';
import { lastDays, toParams, type DateRangeValue } from '../models/date-range';

/**
 * The window these reads default to when a caller passes none.
 *
 * Every one of them took `sinceDays = 30` before; the default is kept so the
 * change is a widening of what can be expressed, not a change to what an
 * unchanged caller gets.
 */
const DEFAULT_WINDOW: DateRangeValue = lastDays(30);

export async function getOverview(
  appId: string,
  win: DateRangeValue = DEFAULT_WINDOW,
): Promise<Overview> {
  const { data } = await api.get<Overview>(`/v1/apps/${appId}/overview`, {
    params: toParams(win),
  });
  return data;
}

// ---------------------------------------------------------------------------
// Per-section overview reads
// ---------------------------------------------------------------------------
//
// `/overview` runs five aggregates SEQUENTIALLY on one server-side connection
// and returns nothing until the last finishes, so its latency is their sum — on
// a large dataset the page sits blank for seconds. These fetch the same data one
// section at a time, so the browser issues them in parallel (wall clock becomes
// the max, not the sum) and each card can paint the moment its own answer lands.
//
// `getOverview` is kept: it is still the right call for anything that wants the
// whole snapshot in one round trip.

// ---------------------------------------------------------------------------
// The section envelope
// ---------------------------------------------------------------------------
//
// The five section endpoints no longer run their aggregate on the request path.
// They answer from a server-side Redis cache and enqueue a background recompute
// when what they have is stale or missing; the finished aggregate arrives over
// `/overview/stream` (see `overview-stream.ts`).
//
// That is why `data` is nullable. `computing` is a normal, expected, 200-worthy
// state — a cold read returns in milliseconds with nothing rather than holding
// the request open for 30s and being shed as a 503 by the server's timeout
// layer, which is what these endpoints did before. A caller that treats a null
// `data` as an error has misread the contract: it means "ask again, or wait for
// the push", and the UI renders a skeleton.

export type OverviewSectionName =
  | 'totals'
  | 'series'
  | 'top-issues'
  | 'top-events'
  | 'active-users';

/**
 * The server-side cache envelope, shared by every route that answers from
 * `view_cache` — no longer Overview's alone since `/active-users` moved onto
 * the same mechanism. `OverviewEnvelope` remains as an alias so the existing
 * call sites in this file keep reading naturally.
 */
export interface ViewEnvelope<T> {
  /**
   * - `fresh` — computed within the server's freshness window (1h). Nothing running.
   * - `stale` — older than that. Shown as-is while a recompute runs.
   * - `computing` — nothing cached; `data` is null and a recompute is running.
   */
  state: 'fresh' | 'stale' | 'computing';
  /**
   * When the query behind `data` actually ran; null iff `data` is null.
   *
   * This is the value the header renders as "Updated 14:32". It is the SERVER's
   * compute time, not a client receive time — the difference is the whole point
   * of showing it, since a value can be served from cache long after it was
   * computed.
   */
  computed_at: string | null;
  data: T | null;
  /**
   * Set when the most recent recompute FAILED. Independent of `data`: a failure
   * must not erase a good stale value, so both can be present — "here are
   * yesterday's numbers, and the refresh is currently broken".
   */
  error?: string;
}

export interface OverviewTotalsSection {
  totals: Overview['totals'];
  error_rate: number;
  /**
   * `null` when the rate cannot be measured — NOT a fallback.
   *
   * Either the window holds no sessions, or it holds errors whose SDK never
   * reported `mechanism.handled` (node, python and csharp default their
   * uncaught-error capture OFF). Rendering 100% for those would state
   * "crash-free" about an app that may be crashing constantly.
   */
  crash_free_sessions: number | null;
}

export interface OverviewSeriesSection {
  events_series: Overview['events_series'];
  errors_series: Overview['errors_series'];
}

/**
 * One UTC day's distinct-user count.
 *
 * `day`, NOT `bucket`: this comes from the backend's `DayCountOut`, which is a
 * different wire shape from `SeriesPoint` even though both are `{ …, count }`.
 * Reusing `SeriesPoint` here typechecks against nothing and produces a chart of
 * `undefined`.
 */
export interface DayCount {
  day: string;
  count: number;
}

/** A day omitted from the series because it straddles the hot/cold watermark. */
export interface PartialDay {
  day: string;
  /** Why it could not be counted exactly — rendered verbatim. */
  reason: string;
}

export interface ActiveUsersSeries {
  series: DayCount[];
  partial_days: PartialDay[];
}

export async function getOverviewTotals(
  appId: string,
  win: DateRangeValue = DEFAULT_WINDOW,
): Promise<OverviewEnvelope<OverviewTotalsSection>> {
  const { data } = await api.get<OverviewEnvelope<OverviewTotalsSection>>(
    `/v1/apps/${appId}/overview/totals`,
    { params: toParams(win) },
  );
  return data;
}

export async function getOverviewSeries(
  appId: string,
  win: DateRangeValue = DEFAULT_WINDOW,
): Promise<OverviewEnvelope<OverviewSeriesSection>> {
  const { data } = await api.get<OverviewEnvelope<OverviewSeriesSection>>(
    `/v1/apps/${appId}/overview/series`,
    { params: toParams(win) },
  );
  return data;
}

/**
 * Top issues. **403 when the caller lacks `issue:read`** — unlike `/overview`,
 * which returns an empty list.
 *
 * The composite route has to degrade, because one missing permission must not
 * fail the whole response. A section addressed on its own does not: an empty
 * array is indistinguishable from "this app has no issues", which would show a
 * reassuring blank card instead of saying the data is not visible to you.
 * Callers should treat 403 as "hide this card", not as an error.
 */
export async function getOverviewTopIssues(
  appId: string,
  win: DateRangeValue = DEFAULT_WINDOW,
): Promise<OverviewEnvelope<Overview['top_issues']>> {
  const { data } = await api.get<OverviewEnvelope<Overview['top_issues']>>(
    `/v1/apps/${appId}/overview/top-issues`,
    { params: toParams(win) },
  );
  return data;
}

export async function getOverviewTopEvents(
  appId: string,
  win: DateRangeValue = DEFAULT_WINDOW,
): Promise<OverviewEnvelope<Overview['top_events']>> {
  const { data } = await api.get<OverviewEnvelope<Overview['top_events']>>(
    `/v1/apps/${appId}/overview/top-events`,
    { params: toParams(win) },
  );
  return data;
}

/**
 * Distinct people per UTC day, for this app.
 *
 * Not the same thing as `/v1/projects/{id}/active-users`, which the Active Users
 * page renders: that one is PROJECT-scoped, breaks the count down into identified
 * vs guest, and is hot-only — it reports `truncated` once the window reaches the
 * cold-rotation age. This is app-scoped and reads ACROSS tiers, so it keeps
 * answering past the rotation age.
 *
 * `partial_days` lists days deliberately omitted because the count could not be
 * computed exactly (a watermark cutting through a day, which only happens at a
 * non-day `TIER_GRANULARITY`). A visible gap beats a wrong point, so render it as
 * a gap and say why.
 */
export async function getActiveUsersSeries(
  appId: string,
  win: DateRangeValue = DEFAULT_WINDOW,
): Promise<OverviewEnvelope<ActiveUsersSeries>> {
  const { data } = await api.get<OverviewEnvelope<ActiveUsersSeries>>(
    `/v1/apps/${appId}/analytics/active-users`,
    { params: toParams(win) },
  );
  return data;
}

/**
 * Force a recompute of all five sections, ignoring freshness.
 *
 * Returns as soon as the work is ENQUEUED — 202, not 200 — because nothing has
 * been recomputed when it responds. Results arrive on the stream. A caller that
 * awaits this and then reads the sections will get the old values; the point is
 * that the button does not block for 30s.
 *
 * Server-side single-flight means holding the button down cannot multiply load.
 */
export async function refreshOverview(
  appId: string,
  win: DateRangeValue = DEFAULT_WINDOW,
): Promise<void> {
  await api.post(`/v1/apps/${appId}/overview/refresh`, null, {
    params: toParams(win),
  });
}

/** @deprecated name — the envelope is not Overview-specific. Use `ViewEnvelope`. */
export type OverviewEnvelope<T> = ViewEnvelope<T>;
