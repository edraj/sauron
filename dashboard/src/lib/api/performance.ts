import { api } from './client';
import type { PerfSummaryRow, PerfSeriesPoint } from '../models';

export interface PerfSummaryParams {
  since_days?: number;
  op?: string;
}

export interface PerfSeriesParams {
  since_days?: number;
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
