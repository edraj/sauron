import { api } from './client';
import type { App, AppType, FirstEventStatus } from '../models';

export async function listApps(projectId: string): Promise<App[]> {
  const { data } = await api.get<App[]>(`/v1/projects/${projectId}/apps`);
  return data;
}

export async function createApp(
  projectId: string,
  body: { name: string; app_type: AppType },
): Promise<App> {
  const { data } = await api.post<App>(`/v1/projects/${projectId}/apps`, body);
  return data;
}

export async function getApp(appId: string): Promise<App> {
  const { data } = await api.get<App>(`/v1/apps/${appId}`);
  return data;
}

/**
 * `store_environment_id` is three-state on the wire: omit the key to leave the
 * designation alone, pass `null` to clear it, pass an id to set it. The backend
 * validates that the id is an environment of THIS app and 400s otherwise —
 * a foreign id would hide the Overview store section forever with nothing to
 * explain why.
 */
export async function updateApp(
  appId: string,
  body: { name?: string; ingest_enabled?: boolean; store_environment_id?: string | null },
): Promise<App> {
  const { data } = await api.patch<App>(`/v1/apps/${appId}`, body);
  return data;
}

export async function deleteApp(appId: string): Promise<void> {
  await api.delete(`/v1/apps/${appId}`);
}

export async function getFirstEvent(appId: string): Promise<FirstEventStatus> {
  const { data } = await api.get<FirstEventStatus>(`/v1/apps/${appId}/first-event`);
  return data;
}
