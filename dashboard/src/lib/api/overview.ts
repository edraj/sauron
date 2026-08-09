import { api } from './client';
import type { Overview } from '../models';

export async function getOverview(appId: string, sinceDays = 30): Promise<Overview> {
  const { data } = await api.get<Overview>(`/v1/apps/${appId}/overview`, {
    params: { since_days: sinceDays },
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

export interface OverviewTotalsSection {
  totals: Overview['totals'];
  error_rate: number;
  crash_free_sessions: number;
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
  sinceDays = 30,
): Promise<OverviewTotalsSection> {
  const { data } = await api.get<OverviewTotalsSection>(`/v1/apps/${appId}/overview/totals`, {
    params: { since_days: sinceDays },
  });
  return data;
}

export async function getOverviewSeries(
  appId: string,
  sinceDays = 30,
): Promise<OverviewSeriesSection> {
  const { data } = await api.get<OverviewSeriesSection>(`/v1/apps/${appId}/overview/series`, {
    params: { since_days: sinceDays },
  });
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
  sinceDays = 30,
): Promise<Overview['top_issues']> {
  const { data } = await api.get<Overview['top_issues']>(
    `/v1/apps/${appId}/overview/top-issues`,
    { params: { since_days: sinceDays } },
  );
  return data;
}

export async function getOverviewTopEvents(
  appId: string,
  sinceDays = 30,
): Promise<Overview['top_events']> {
  const { data } = await api.get<Overview['top_events']>(
    `/v1/apps/${appId}/overview/top-events`,
    { params: { since_days: sinceDays } },
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
  sinceDays = 30,
): Promise<ActiveUsersSeries> {
  const { data } = await api.get<ActiveUsersSeries>(
    `/v1/apps/${appId}/analytics/active-users`,
    { params: { since_days: sinceDays } },
  );
  return data;
}
