import { api } from './client';
import type { FunnelResult, SavedFunnel } from '../models';

export async function computeFunnel(
  appId: string,
  steps: string[],
  sinceDays = 30,
): Promise<FunnelResult> {
  const { data } = await api.post<FunnelResult>(`/v1/apps/${appId}/funnel`, {
    steps,
    since_days: sinceDays,
  });
  return data;
}

export async function listSavedFunnels(appId: string): Promise<SavedFunnel[]> {
  const { data } = await api.get<SavedFunnel[]>(`/v1/apps/${appId}/funnels`);
  return data;
}

export interface SaveFunnelBody {
  name: string;
  description?: string;
  steps: string[];
}

export async function saveFunnel(appId: string, body: SaveFunnelBody): Promise<SavedFunnel> {
  const { data } = await api.post<SavedFunnel>(`/v1/apps/${appId}/funnels`, body);
  return data;
}

export async function updateFunnel(appId: string, id: string, body: SaveFunnelBody): Promise<void> {
  await api.patch(`/v1/apps/${appId}/funnels/${id}`, body);
}

export async function deleteFunnel(appId: string, id: string): Promise<void> {
  await api.delete(`/v1/apps/${appId}/funnels/${id}`);
}
