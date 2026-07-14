import { api } from './client';
import type { ScreenRow, ScreenDetail } from '../models';

export interface ListScreensParams {
  q?: string;
  sinceDays?: number;
  limit?: number;
  offset?: number;
}

export async function listScreens(
  appId: string,
  opts: ListScreensParams = {},
): Promise<ScreenRow[]> {
  const p = new URLSearchParams();
  if (opts.q) p.set('q', opts.q);
  if (opts.sinceDays != null) p.set('since_days', String(opts.sinceDays));
  if (opts.limit != null) p.set('limit', String(opts.limit));
  if (opts.offset != null) p.set('offset', String(opts.offset));
  const { data } = await api.get<ScreenRow[]>(`/v1/apps/${appId}/screens?${p.toString()}`);
  return data;
}

export async function getScreenDetail(
  appId: string,
  name: string,
  sinceDays = 30,
): Promise<ScreenDetail> {
  const { data } = await api.get<ScreenDetail>(`/v1/apps/${appId}/screens/detail`, {
    params: { name, since_days: sinceDays },
  });
  return data;
}
