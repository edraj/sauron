import { api } from './client';
import type { Environment } from '../models';

export async function listEnvironments(
  appId: string,
  includeRetired = false,
): Promise<Environment[]> {
  const { data } = await api.get<Environment[]>(`/v1/apps/${appId}/environments`, {
    params: includeRetired ? { include_retired: true } : undefined,
  });
  return data;
}

export async function createEnvironment(
  appId: string,
  body: { name: string },
): Promise<Environment> {
  const { data } = await api.post<Environment>(`/v1/apps/${appId}/environments`, body);
  return data;
}

export async function updateEnvironment(
  envId: string,
  body: { name?: string; ingest_enabled?: boolean; is_default?: boolean },
): Promise<Environment> {
  const { data } = await api.patch<Environment>(`/v1/environments/${envId}`, body);
  return data;
}

export async function rotateEnvironmentKey(envId: string): Promise<Environment> {
  const { data } = await api.post<Environment>(`/v1/environments/${envId}/rotate-key`);
  return data;
}

/** Retires rather than deletes — the row is kept so history stays attributable. */
export async function retireEnvironment(envId: string): Promise<Environment> {
  const { data } = await api.delete<Environment>(`/v1/environments/${envId}`);
  return data;
}
