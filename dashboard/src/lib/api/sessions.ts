import { api } from './client';
import { lastDays, toParams, type DateRangeValue } from '../models/date-range';
/** The window this read defaults to when a caller passes none — unchanged. */
const DEFAULT_WINDOW: DateRangeValue = lastDays(30);

import type { Session, SessionsAnalytics, TimelineItem } from '../models';
import type { SearchParams, SearchEnvelope } from './search';

export interface ListSessionsParams extends SearchParams {
  /** Rows to RENDER; the request asks for one more. See `listSessions`. */
  limit: number;
  offset: number;
  distinct_id?: string;
  device_key?: string;
  /**
   * The time window, snake_case, as `models/time-filter`'s `toRecord` emits it.
   *
   * Deliberately NOT the camelCase `timeField`/`sinceDays` that
   * `SearchPredicateParams` declares for the `URLSearchParams` encoder: this
   * client passes a plain object straight to axios, so whatever key is written
   * here is the key that goes on the wire. `sinceDays` needs the rename below
   * precisely because it came in through the camelCase door; spreading
   * `toRecord`'s output avoids that whole class of mismatch.
   *
   * Accepts `started_at` and `last_event_at`. Note the default is
   * `last_event_at` — that is the column this list has always filtered on,
   * even while the response envelope's `clamped` field claimed `started_at`.
   */
  time_field?: string;
  from?: string;
  to?: string;
  since_days?: number;
}

/**
 * One page of sessions, plus whether another page follows.
 *
 * Requests `limit + 1` and returns `limit`; the surplus row is the has-more
 * probe. The endpoint clamps `limit` at 200. See `overFetched`.
 */
export async function listSessions(
  appId: string,
  params: ListSessionsParams,
): Promise<SearchEnvelope<Session>> {
  // `filters` is pulled OUT of the object handed to axios on purpose. The
  // route reads repeated `filter=` parameters (`Vec<String>`), and axios
  // serialises an array property as `filters[]=…` — a name the extractor does
  // not know, so every chip would be dropped silently and the list would come
  // back unnarrowed while the chips sat on screen claiming otherwise. Written
  // into the URL directly instead; axios appends its own `params` after the
  // existing query string.
  const { filters, ...rest } = params;
  const queryParams: any = { ...rest, limit: params.limit + 1 };
  if (queryParams.sinceDays !== undefined) {
    queryParams.since_days = queryParams.sinceDays;
    delete queryParams.sinceDays;
  }
  const encoded = new URLSearchParams();
  for (const f of filters ?? []) encoded.append('filter', f);
  const qs = encoded.toString();
  const { data } = await api.get<SearchEnvelope<Session>>(
    qs ? `/v1/apps/${appId}/sessions?${qs}` : `/v1/apps/${appId}/sessions`,
    { params: queryParams },
  );
  return data;
}

// Two sections rather than the composite `GET …/sessions/{id}` — see the note
// on `getIssueSummary` (api/issues.ts). The split is starkest here: the session
// row is one keyed lookup, the timeline up to 1,500 rows plus symbolication.

/** The session row alone — what the header, the stat tiles and Context render. */
export async function getSessionSummary(appId: string, sessionId: string): Promise<Session> {
  const { data } = await api.get<{ session: Session }>(
    `/v1/apps/${appId}/sessions/${encodeURIComponent(sessionId)}/summary`,
  );
  return data.session;
}

/** The session's events, errors and transactions, in time order. */
export async function getSessionTimeline(
  appId: string,
  sessionId: string,
): Promise<TimelineItem[]> {
  const { data } = await api.get<{ timeline: TimelineItem[] }>(
    `/v1/apps/${appId}/sessions/${encodeURIComponent(sessionId)}/timeline`,
  );
  return data.timeline;
}

export async function getSessionAnalytics(
  appId: string,
  win: DateRangeValue = DEFAULT_WINDOW,
): Promise<SessionsAnalytics> {
  const { data } = await api.get<SessionsAnalytics>(`/v1/apps/${appId}/sessions/summary`, {
    params: toParams(win),
  });
  return data;
}
