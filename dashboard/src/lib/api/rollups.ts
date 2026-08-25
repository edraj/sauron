import { api } from './client';
import type { RollupStatus } from '../models';

export async function getRollupStatus(appId: string): Promise<RollupStatus> {
  const { data } = await api.get<RollupStatus>(`/v1/apps/${appId}/rollups/status`);
  return data;
}

export interface RollupRefreshOut {
  as_of: string | null;
  caught_up: boolean;
}

/// Kicks an immediate rollup fold and waits (server-side, bounded) for it to
/// land, so a reload right after returns fresh aggregates.
export async function refreshRollups(appId: string): Promise<RollupRefreshOut> {
  const { data } = await api.post<RollupRefreshOut>(`/v1/apps/${appId}/rollups/refresh`);
  return data;
}
