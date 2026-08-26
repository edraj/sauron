import { api } from './client';
import type { FunnelResult, SavedFunnel } from '../models';
import { lastDays, toBody, type DateRangeValue } from '../models/date-range';

/** The window these reads default to when a caller passes none — unchanged. */
const DEFAULT_WINDOW: DateRangeValue = lastDays(30);


export async function computeFunnel(
  appId: string,
  steps: string[],
  win: DateRangeValue = DEFAULT_WINDOW,
): Promise<FunnelResult> {
  // A JSON body rather than a query string, so `toBody`, NOT `toParams`:
  // `FunnelReq.since_days` is an `i64`, and the string `toParams` emits for a
  // query string 422s in a JSON body. Same three field names either way —
  // `FunnelReq` accepts `since_days` OR `from`/`to`, and the encoder decides
  // which, so the precedence rule is not restated here.
  const { data } = await api.post<FunnelResult>(`/v1/apps/${appId}/funnel`, {
    steps,
    ...toBody(win),
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
