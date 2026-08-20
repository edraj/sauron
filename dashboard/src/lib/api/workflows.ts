// Workflows API, scoped to an app — mirrors `screens.ts`'s hand-built
// `URLSearchParams` idiom. `environment_id` is never added by hand here: the
// axios interceptor in `client.ts` auto-injects it for every
// `/v1/apps/{app_id}/...` URL (see `../api/scope.ts`), and `/workflows` is
// not one of the app-configuration exclusions, so it is scoped by default.
//
// `getWorkflow` (detail view) lands in Task 12 alongside `WorkflowDetail`.

import { api } from './client';
import { overFetched, type ListPage } from '../models/list-state';
import type { WorkflowRow, WorkflowRun, WorkflowSpan, WorkflowStatus } from '../models';

export interface ListWorkflowsParams {
  /** Rows to RENDER; the request asks for one more. See `listWorkflows`. */
  limit: number;
  offset: number;
  /**
   * The window, as `date-range`'s `toParams` encodes it — `since_days` OR
   * `from`/`to`, never both. Strings, because that is what goes on the wire.
   */
  since_days?: string;
  from?: string;
  to?: string;
  search?: string;
  /**
   * `sort=` as `sortParam()` encodes it — a BARE column descends, a `-` prefix
   * ascends. Accepts `started`, `name`, `completed`, `cancelled`, `abandoned`,
   * `completion_rate`, `median_duration_ms`, `p95_duration_ms`, `users`,
   * `last_seen`; anything else is a 400.
   *
   * The unique-user count is `users` ON THE WIRE even though the row field and
   * the SQL alias are both `unique_users` — sending `unique_users` is a 400.
   */
  sort?: string;
}

/**
 * One page of workflows, plus whether another page follows.
 *
 * Requests `limit + 1` and returns `limit`; the surplus row is the has-more
 * probe. See `overFetched`.
 */
export async function listWorkflows(
  appId: string,
  opts: ListWorkflowsParams,
): Promise<ListPage<WorkflowRow>> {
  const p = new URLSearchParams();
  if (opts.since_days !== undefined) p.set('since_days', opts.since_days);
  if (opts.from) p.set('from', opts.from);
  if (opts.to) p.set('to', opts.to);
  if (opts.search) p.set('search', opts.search);
  if (opts.sort) p.set('sort', opts.sort);
  p.set('limit', String(opts.limit + 1));
  p.set('offset', String(opts.offset));
  const { data } = await api.get<WorkflowRow[]>(`/v1/apps/${appId}/workflows?${p.toString()}`);
  return overFetched(data, opts.limit);
}

export interface ListWorkflowRunsParams {
  /**
   * The window, as `date-range`'s `toParams` encodes it — `since_days` OR
   * `from`/`to`, never both. Strings, because that is what goes on the wire.
   */
  since_days?: string;
  from?: string;
  to?: string;
  status?: WorkflowStatus;
  limit?: number;
  offset?: number;
}

export async function listWorkflowRuns(
  appId: string,
  name: string,
  opts: ListWorkflowRunsParams = {},
): Promise<WorkflowRun[]> {
  const p = new URLSearchParams();
  if (opts.since_days !== undefined) p.set('since_days', opts.since_days);
  if (opts.from) p.set('from', opts.from);
  if (opts.to) p.set('to', opts.to);
  if (opts.status) p.set('status', opts.status);
  if (opts.limit !== undefined) p.set('limit', String(opts.limit));
  if (opts.offset !== undefined) p.set('offset', String(opts.offset));
  const { data } = await api.get<WorkflowRun[]>(
    `/v1/apps/${appId}/workflows/${encodeURIComponent(name)}/runs?${p.toString()}`,
  );
  return data;
}

export async function listSessionWorkflows(
  appId: string,
  sessionId: string,
): Promise<WorkflowSpan[]> {
  const { data } = await api.get<WorkflowSpan[]>(
    `/v1/apps/${appId}/sessions/${encodeURIComponent(sessionId)}/workflows`,
  );
  return data;
}
