import { api } from './client';
import type { Overview } from '../models';

export async function getOverview(appId: string, sinceDays = 30): Promise<Overview> {
  const { data } = await api.get<Overview>(`/v1/apps/${appId}/overview`, {
    params: { since_days: sinceDays },
  });
  return data;
}
