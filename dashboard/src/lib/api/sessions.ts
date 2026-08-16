import { api } from './client';
import type { Session, SessionDetail, SessionsAnalytics } from '../models';
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
  const queryParams: any = { ...params, limit: params.limit + 1 };
  if (queryParams.sinceDays !== undefined) {
    queryParams.since_days = queryParams.sinceDays;
    delete queryParams.sinceDays;
  }
  const { data } = await api.get<SearchEnvelope<Session>>(`/v1/apps/${appId}/sessions`, {
    params: queryParams,
  });
  return data;
}

export async function getSession(appId: string, sessionId: string): Promise<SessionDetail> {
  const { data } = await api.get<SessionDetail>(
    `/v1/apps/${appId}/sessions/${encodeURIComponent(sessionId)}`,
  );
  return data;
}

export async function getSessionAnalytics(
  appId: string,
  sinceDays = 30,
): Promise<SessionsAnalytics> {
  const { data } = await api.get<SessionsAnalytics>(`/v1/apps/${appId}/sessions/summary`, {
    params: { since_days: sinceDays },
  });
  return data;
}
