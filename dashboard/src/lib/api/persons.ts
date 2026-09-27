import { api } from './client';
import { overFetched, type ListPage } from '../models/list-state';
import type { AnalyticsEvent, ErrorEvent, PersonRow } from '../models';

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

// Two sections rather than the composite `GET …/persons/{id}` — see the note
// on `getIssueSummary` (api/issues.ts).

/**
 * The profile row alone, or `null` when this `distinct_id` has none in scope.
 *
 * `null` is an answer, not a failure: the backend serves a person who exists
 * only as a `distinct_id` on events, and the page renders them from the URL.
 */
export async function getPersonSummary(
  appId: string,
  distinctId: string,
): Promise<PersonRow | null> {
  const { data } = await api.get<{ distinct_id: string; user: PersonRow | null }>(
    `/v1/apps/${appId}/persons/${encodeURIComponent(distinctId)}/summary`,
  );
  return data.user;
}

export interface PersonTimeline {
  events: AnalyticsEvent[];
  errors: ErrorEvent[];
}

/** The person's most recent `limit` events and `limit` errors. */
export async function getPersonTimeline(
  appId: string,
  distinctId: string,
  limit = 50,
): Promise<PersonTimeline> {
  const { data } = await api.get<PersonTimeline>(
    `/v1/apps/${appId}/persons/${encodeURIComponent(distinctId)}/timeline`,
    { params: { limit } },
  );
  return data;
}
