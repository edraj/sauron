import { api } from './client';
import type { UsersAnalytics } from '../models';
import { lastDays, toParams, type DateRangeValue } from '../models/date-range';

/** The window these reads default to when a caller passes none — unchanged. */
const DEFAULT_WINDOW: DateRangeValue = lastDays(30);


export async function getUserAnalytics(
  appId: string,
  win: DateRangeValue = DEFAULT_WINDOW,
): Promise<UsersAnalytics> {
  const { data } = await api.get<UsersAnalytics>(`/v1/apps/${appId}/users/summary`, {
    params: toParams(win),
  });
  return data;
}
