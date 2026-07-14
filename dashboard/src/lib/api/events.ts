import { api } from './client';
import type { AnalyticsEvent, SeriesPoint, TopEvent } from '../models';

export interface ListEventsParams {
  filters?: string[];
  q?: string;
  sinceDays?: number;
  limit?: number;
  offset?: number;
}

export async function listEvents(
  appId: string,
  opts: ListEventsParams = {},
): Promise<AnalyticsEvent[]> {
  const p = new URLSearchParams();
  for (const f of opts.filters ?? []) p.append('filter', f);
  if (opts.q) p.set('q', opts.q);
  if (opts.sinceDays != null) p.set('since_days', String(opts.sinceDays));
  if (opts.limit != null) p.set('limit', String(opts.limit));
  if (opts.offset != null) p.set('offset', String(opts.offset));
  const { data } = await api.get<AnalyticsEvent[]>(`/v1/apps/${appId}/events/list?${p.toString()}`);
  return data;
}

export async function topEvents(
  appId: string,
  params: { since_days?: number; limit?: number } = {},
): Promise<TopEvent[]> {
  const { data } = await api.get<TopEvent[]>(`/v1/apps/${appId}/events/top`, {
    params,
  });
  return data;
}

export async function eventSeries(
  appId: string,
  params: { name?: string; since_days?: number } = {},
): Promise<SeriesPoint[]> {
  const { data } = await api.get<SeriesPoint[]>(`/v1/apps/${appId}/events/series`, {
    params,
  });
  return data;
}
