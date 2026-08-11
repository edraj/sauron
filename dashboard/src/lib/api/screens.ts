import { api } from './client';
import { overFetched, type ListPage } from '../models/list-state';
import type { ScreenRow, ScreenDetail } from '../models';

export interface ListScreensParams {
  /** Rows to RENDER; the request asks for one more. See `listScreens`. */
  limit: number;
  offset: number;
  q?: string;
  sinceDays?: number;
  /**
   * `sort=` as `sortParam()` encodes it — a BARE column descends, a `-` prefix
   * ascends. Accepts `views`, `screen`, `events`, `exceptions`, `users`,
   * `avg_dwell_ms`; anything else is a 400.
   */
  sort?: string;
}

/**
 * One page of screens, plus whether another page follows.
 *
 * Requests `limit + 1` and returns `limit`; the surplus row is the has-more
 * probe. See `overFetched`.
 */
export async function listScreens(
  appId: string,
  opts: ListScreensParams,
): Promise<ListPage<ScreenRow>> {
  const p = new URLSearchParams();
  if (opts.q) p.set('q', opts.q);
  if (opts.sinceDays != null) p.set('since_days', String(opts.sinceDays));
  if (opts.sort) p.set('sort', opts.sort);
  p.set('limit', String(opts.limit + 1));
  p.set('offset', String(opts.offset));
  const { data } = await api.get<ScreenRow[]>(`/v1/apps/${appId}/screens?${p.toString()}`);
  return overFetched(data, opts.limit);
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
