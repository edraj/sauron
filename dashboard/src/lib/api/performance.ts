import { api } from './client';
import type { PerfSummaryRow, PerfSeriesPoint } from '../models';

/**
 * `since_days` is a STRING here, not a number, because it arrives from
 * `date-range`'s `toParams` — one encoder for the wire, so a page cannot send
 * `since_days` next to a bound the server would let it override.
 */
export interface PerfSummaryParams {
  since_days?: string;
  from?: string;
  to?: string;
  op?: string;
}

export interface PerfSeriesParams {
  since_days?: string;
  from?: string;
  to?: string;
  name?: string;
  op?: string;
}

export async function perfSummary(
  appId: string,
  params: PerfSummaryParams = {},
): Promise<PerfSummaryRow[]> {
  const { data } = await api.get<PerfSummaryRow[]>(`/v1/apps/${appId}/performance/summary`, {
    params,
  });
  return data;
}

export async function perfSeries(
  appId: string,
  params: PerfSeriesParams = {},
): Promise<PerfSeriesPoint[]> {
  const { data } = await api.get<PerfSeriesPoint[]>(`/v1/apps/${appId}/performance/series`, {
    params,
  });
  return data;
}
