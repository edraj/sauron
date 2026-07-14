import { api } from './client';
import type { PersonProfile, PersonRow } from '../models';

export interface ListPersonsParams {
  search?: string;
  limit?: number;
  offset?: number;
}

export async function listPersons(
  appId: string,
  params: ListPersonsParams = {},
): Promise<PersonRow[]> {
  const { data } = await api.get<PersonRow[]>(`/v1/apps/${appId}/persons`, { params });
  return data;
}

export async function getPerson(
  appId: string,
  distinctId: string,
  limit = 50,
): Promise<PersonProfile> {
  const { data } = await api.get<PersonProfile>(
    `/v1/apps/${appId}/persons/${encodeURIComponent(distinctId)}`,
    { params: { limit } },
  );
  return data;
}
