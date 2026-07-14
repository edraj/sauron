import { api } from './client';
import type { Session, SessionDetail, SessionsAnalytics } from '../models';

export interface ListSessionsParams {
  since_days?: number;
  limit?: number;
  offset?: number;
  distinct_id?: string;
  device_key?: string;
}

export async function listSessions(
  appId: string,
  params: ListSessionsParams = {},
): Promise<Session[]> {
  const { data } = await api.get<Session[]>(`/v1/apps/${appId}/sessions`, { params });
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
