import { api } from './client';
import {
  predicateParams,
  searchParams,
  type SearchEnvelope,
  type SearchParams,
  type SearchPredicateParams,
} from './search';
import type {
  ErrorEvent,
  Issue,
  IssueDetail,
  IssueEventStats,
  IssueStats,
  IssueStatus,
} from '../models';
import { lastDays, toParams, type DateRangeValue } from '../models/date-range';
/** The window these reads default to when a caller passes none — unchanged. */
const DEFAULT_WINDOW: DateRangeValue = lastDays(30);


export async function getIssueStats(
  appId: string,
  win: DateRangeValue = DEFAULT_WINDOW,
): Promise<IssueStats> {
  const { data } = await api.get<IssueStats>(`/v1/apps/${appId}/issues/stats`, {
    params: toParams(win),
  });
  return data;
}

/**
 * `sort` accepts `last_seen` (the default) or `first_seen`, `-`-prefixed for
 * ascending. Anything else is a 400 naming what is allowed — the list refuses
 * an ordering with no keyset index behind it rather than paging it unstably.
 *
 * `limit` is clamped server-side to 1..200.
 */
export type ListIssuesParams = SearchParams;

/**
 * Answers a {@link SearchEnvelope}, not a bare array, since S2c: the array had
 * nowhere to put `total`, `next_cursor` or the planner's `clamped` notice.
 */
export async function listIssues(
  appId: string,
  opts: ListIssuesParams = {},
): Promise<SearchEnvelope<Issue>> {
  const p = searchParams(opts);
  const { data } = await api.get<SearchEnvelope<Issue>>(
    `/v1/apps/${appId}/issues?${p.toString()}`,
  );
  return data;
}

export async function getIssue(appId: string, issueId: string): Promise<IssueDetail> {
  const { data } = await api.get<IssueDetail>(`/v1/apps/${appId}/issues/${issueId}`);
  return data;
}

export async function updateIssueStatus(
  appId: string,
  issueId: string,
  status: IssueStatus,
): Promise<Issue> {
  const { data } = await api.patch<Issue>(
    `/v1/apps/${appId}/issues/${issueId}`,
    { status },
  );
  return data;
}

/**
 * One issue's occurrences. `sort` accepts `occurred_at` (the default),
 * `distinct_id`, `session_id` or `device_key`, `-`-prefixed for ascending —
 * the columns with a supporting keyset index on that table. Anything else is
 * a 400 naming what is allowed. `limit` is clamped server-side to 1..100.
 */
export type ListIssueEventsParams = SearchParams;

/**
 * Answers a {@link SearchEnvelope}, not a bare array, since S2c — same change
 * and same reasons as {@link listIssues}.
 */
export async function listIssueEvents(
  appId: string,
  issueId: string,
  opts: ListIssueEventsParams = {},
): Promise<SearchEnvelope<ErrorEvent>> {
  // The route's own default is 30; this list has always asked for 50 and the
  // page renders that many, so it is passed explicitly rather than inherited.
  const p = searchParams({ ...opts, limit: opts.limit ?? 50 });
  const { data } = await api.get<SearchEnvelope<ErrorEvent>>(
    `/v1/apps/${appId}/issues/${issueId}/events?${p.toString()}`,
  );
  return data;
}

/**
 * Totals for every occurrence matching `opts` — not just the page
 * `listIssueEvents` returns.
 *
 * **Shape unchanged by S2c**: this route does not answer an envelope, because
 * it has no rows to page. It is still the counts and nothing else.
 *
 * Takes only {@link SearchPredicateParams}, and that is the same rule the old
 * shared `occurrenceParams` encoder enforced, now expressed in the type: these
 * counts are rendered as a description of the list's rows, so both requests
 * must carry one predicate or a filter added to one alone would make them
 * quietly disagree. The route reads the list's `EventsQuery` server-side and
 * ignores `sort`/`cursor`/`limit` — no ordering and no page boundary changes a
 * total — so the page half is not merely unnecessary here, it is meaningless,
 * and sending a `cursor` would suggest these were page-scoped counts.
 *
 * `total` on the list envelope is NOT a substitute: it stops at the server's
 * 10,000 count cap, while `events` here is exact.
 */
export async function getIssueEventStats(
  appId: string,
  issueId: string,
  opts: SearchPredicateParams = {},
): Promise<IssueEventStats> {
  const p = predicateParams(opts);
  const { data } = await api.get<IssueEventStats>(
    `/v1/apps/${appId}/issues/${issueId}/events/stats?${p.toString()}`,
  );
  return data;
}
