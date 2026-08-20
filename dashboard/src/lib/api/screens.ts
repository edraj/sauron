import { api } from './client';
import { overFetched, type ListPage } from '../models/list-state';
import type { ScreenRow, ScreenDetail } from '../models';
import { lastDays, toParams, type DateRangeValue } from '../models/date-range';
/** The window these reads default to when a caller passes none — unchanged. */
const DEFAULT_WINDOW: DateRangeValue = lastDays(30);


export interface ListScreensParams {
  /** Rows to RENDER; the request asks for one more. See `listScreens`. */
  limit: number;
  offset: number;
  q?: string;
  /** The window, encoded by `toParams` — `since_days` OR `from`/`to`. */
  window?: DateRangeValue;
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
  if (opts.window) for (const [k, v] of Object.entries(toParams(opts.window))) p.set(k, v);
  if (opts.sort) p.set('sort', opts.sort);
  p.set('limit', String(opts.limit + 1));
  p.set('offset', String(opts.offset));
  const { data } = await api.get<ScreenRow[]>(`/v1/apps/${appId}/screens?${p.toString()}`);
  return overFetched(data, opts.limit);
}

export async function getScreenDetail(
  appId: string,
  name: string,
  win: DateRangeValue = DEFAULT_WINDOW,
): Promise<ScreenDetail> {
  const { data } = await api.get<ScreenDetail>(`/v1/apps/${appId}/screens/detail`, {
    params: { name, ...toParams(win) },
  });
  return data;
}
