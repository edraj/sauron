import { api } from './client';
import { searchParams, type SearchEnvelope, type SearchParams } from './search';
import type { AnalyticsEvent, SeriesPoint, TopEvent } from '../models';

/**
 * `sort` accepts `occurred_at` (the default), `name`, `distinct_id` or
 * `session_id`, `-`-prefixed for ascending — the columns with a supporting
 * keyset index on this table. Anything else is a 400 naming what is allowed.
 * `limit` is clamped server-side to 1..200.
 *
 * This route DID have a working `offset` before S2c — it is the one list that
 * genuinely lost a feature rather than a parameter nobody used. The server now
 * accepts and ignores it, so the client stopped sending it; page with `cursor`.
 */
export type ListEventsParams = SearchParams;

/**
 * Answers a {@link SearchEnvelope}, not a bare array, since S2c — the last of
 * the slice's three lists.
 */
export async function listEvents(
  appId: string,
  opts: ListEventsParams = {},
): Promise<SearchEnvelope<AnalyticsEvent>> {
  const p = searchParams(opts);
  const { data } = await api.get<SearchEnvelope<AnalyticsEvent>>(
    `/v1/apps/${appId}/events/list?${p.toString()}`,
  );
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
