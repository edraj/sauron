import { api } from './client';
import { overFetched, type ListPage } from '../models/list-state';
import type { PersonProfile, PersonRow } from '../models';

export interface ListPersonsParams {
  /** Rows to RENDER; the request asks for one more. See `listPersons`. */
  limit: number;
  offset: number;
  search?: string;
  /**
   * `sort=` as `sortParam()` encodes it — a BARE column descends, a `-` prefix
   * ascends. Accepts `last_seen`, `distinct_id`, `first_seen`,
   * `sessions_count`, `events_count`, `errors_count`; anything else is a 400.
   */
  sort?: string;
  /**
   * The time window, as `models/time-filter`'s `toRecord` encodes it.
   *
   * New with the time filter, and this list had NO window before it: the Users
   * page rendered a range picker that only ever drove the stat tiles, while the
   * table showed every person regardless. Accepts `last_seen` (default) and
   * `first_seen`; anything else is a 400 naming the pair.
   *
   * Note what these filter, because Devices means something different by the
   * same words: here the predicate is applied to the value the column
   * DISPLAYS — env-scoped when an environment is selected — so "last seen in
   * the last 7 days" agrees with the Last seen cell beside it.
   */
  time_field?: string;
  from?: string;
  to?: string;
  since_days?: number;
}

/**
 * One page of people, plus whether another page follows.
 *
 * Requests `limit + 1` and returns `limit`; the surplus row is the has-more
 * probe. See `overFetched` for why the older `rows.length >= limit` guess
 * offered a Next that led to an empty page.
 */
export async function listPersons(
  appId: string,
  params: ListPersonsParams,
): Promise<ListPage<PersonRow>> {
  const { data } = await api.get<PersonRow[]>(`/v1/apps/${appId}/persons`, {
    params: { ...params, limit: params.limit + 1 },
  });
  return overFetched(data, params.limit);
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
