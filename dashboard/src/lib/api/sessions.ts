import { api } from './client';
import { overFetched, type ListPage } from '../models/list-state';
import type { Session, SessionDetail, SessionsAnalytics } from '../models';

export interface ListSessionsParams {
  /** Rows to RENDER; the request asks for one more. See `listSessions`. */
  limit: number;
  offset: number;
  since_days?: number;
  distinct_id?: string;
  device_key?: string;
  /**
   * `sort=` as `sortParam()` encodes it — a BARE column descends, a `-` prefix
   * ascends. Accepts `started_at`, `distinct_id`, `device_key`, `duration_ms`,
   * `events_count`, `errors_count`; anything else is a 400. Note the default
   * is `started_at`, NOT the `last_event_at` this list used to order by.
   */
  sort?: string;
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
): Promise<ListPage<Session>> {
  const { data } = await api.get<Session[]>(`/v1/apps/${appId}/sessions`, {
    params: { ...params, limit: params.limit + 1 },
  });
  return overFetched(data, params.limit);
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
