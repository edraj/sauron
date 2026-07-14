import { api } from './client';
import type { Journey } from '../models';

export interface JourneyParams {
  since_days?: number;
  depth?: number;
}

export async function getJourney(appId: string, params: JourneyParams = {}): Promise<Journey> {
  const { data } = await api.get<Journey>(`/v1/apps/${appId}/journeys`, { params });
  return data;
}
