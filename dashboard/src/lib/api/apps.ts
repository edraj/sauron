import { api } from './client';
import type { App, AppType, Environment, FirstEventStatus } from '../models';

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

export async function updateApp(
  appId: string,
  body: { name?: string; ingest_enabled?: boolean },
): Promise<App> {
  const { data } = await api.patch<App>(`/v1/apps/${appId}`, body);
  return data;
}

export async function deleteApp(appId: string): Promise<void> {
  await api.delete(`/v1/apps/${appId}`);
}

export async function rotateAppKey(appId: string): Promise<App> {
  const { data } = await api.post<App>(`/v1/apps/${appId}/rotate-key`);
  return data;
}

export async function listEnvironments(appId: string): Promise<Environment[]> {
  const { data } = await api.get<Environment[]>(`/v1/apps/${appId}/environments`);
  return data;
}

export async function getFirstEvent(appId: string): Promise<FirstEventStatus> {
  const { data } = await api.get<FirstEventStatus>(`/v1/apps/${appId}/first-event`);
  return data;
}
