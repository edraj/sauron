import { api } from './client';
import type { UsersAnalytics } from '../models';

export async function getUserAnalytics(
  appId: string,
  sinceDays = 30,
): Promise<UsersAnalytics> {
  const { data } = await api.get<UsersAnalytics>(`/v1/apps/${appId}/users/summary`, {
    params: { since_days: sinceDays },
  });
  return data;
}
