import { api } from './client';
import type { AppRelease } from '../models';

/** Releases the caller may see for `appId`, newest `last_seen_at` first. */
export async function listReleases(appId: string): Promise<AppRelease[]> {
  const { data } = await api.get<AppRelease[]>(`/v1/apps/${appId}/releases`);
  return data;
}
