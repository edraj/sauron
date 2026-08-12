import { api } from './client';
import { overFetched, type ListPage } from '../models/list-state';
import type { Session, SessionDetail, SessionsAnalytics } from '../models';
import type { SearchParams, SearchEnvelope } from './search';

export interface ListSessionsParams extends SearchParams {
  /** Rows to RENDER; the request asks for one more. See `listSessions`. */
  limit: number;
  offset: number;
  distinct_id?: string;
  device_key?: string;
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
  const { data } = await api.get<SearchEnvelope<Session>>(`/v1/apps/${appId}/sessions`, {
    params: { ...params, limit: params.limit + 1 },
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
