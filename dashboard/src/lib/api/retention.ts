import { api } from './client';
import type {
  ChurnPerson,
  Granularity,
  LifecyclePoint,
  RetentionGrid,
} from '../models/retention';

/**
 * Request shapes live here rather than in `models/`, per the `api/alerts.ts`
 * convention: response and domain types are shared, request shapes belong to
 * the module that sends them.
 */
export interface RetentionParams {
  granularity?: Granularity;
  cohorts?: number;
  periods?: number;
  split?: 'none' | 'errors';
}

export interface LifecycleParams {
  granularity?: Granularity;
  periods?: number;
}

export interface ChurnParams {
  granularity?: Granularity;
  silent_periods?: number;
  limit?: number;
  /** Keyset cursor: the `last_seen` of the previous page's final row. */
  /** `column` = descending, `-column` = ascending (the house convention). */
  sort?: string;
  /** Verbatim `next_cursor` from the previous page. */
  cursor?: string;
}

export interface LifecycleOut {
  granularity: Granularity;
  as_of: string | null;
  ready: boolean;
  /** Same contract as `RetentionGrid.computed_at`. */
  computed_at?: string | null;
  points: LifecyclePoint[];
}

export interface ChurnOut {
  ready: boolean;
  silent_days: number;
  people: ChurnPerson[];
  /** Opaque row-value cursor, bound to the `sort` it was minted under. */
  next_cursor: string | null;
}

/**
 * All three take plain query parameters, so they use axios's default
 * serializer. No `toBody` here — that is for JSON-bodied endpoints such as
 * `/funnel`, and crossing the two is what produced the 422 in v1.7.3.
 */
export async function getRetention(
  appId: string,
  params: RetentionParams = {},
): Promise<RetentionGrid> {
  const { data } = await api.get<RetentionGrid>(`/v1/apps/${appId}/retention`, { params });
  return data;
}

export async function getLifecycle(
  appId: string,
  params: LifecycleParams = {},
): Promise<LifecycleOut> {
  const { data } = await api.get<LifecycleOut>(`/v1/apps/${appId}/retention/lifecycle`, {
    params,
  });
  return data;
}

export async function getChurn(appId: string, params: ChurnParams = {}): Promise<ChurnOut> {
  const { data } = await api.get<ChurnOut>(`/v1/apps/${appId}/retention/churn`, { params });
  return data;
}
