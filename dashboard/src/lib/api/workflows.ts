// Workflows API, scoped to an app — mirrors `screens.ts`'s hand-built
// `URLSearchParams` idiom. `environment_id` is never added by hand here: the
// axios interceptor in `client.ts` auto-injects it for every
// `/v1/apps/{app_id}/...` URL (see `../api/scope.ts`), and `/workflows` is
// not one of the app-configuration exclusions, so it is scoped by default.
//
// `getWorkflow` (detail view) lands in Task 12 alongside `WorkflowDetail`.

import { api } from './client';
import type { WorkflowRow, WorkflowRun, WorkflowSpan, WorkflowStatus } from '../models';

export interface ListWorkflowsParams {
  since_days?: number;
  search?: string;
  limit?: number;
  offset?: number;
}

export async function listWorkflows(
  appId: string,
  opts: ListWorkflowsParams = {},
): Promise<WorkflowRow[]> {
  const p = new URLSearchParams();
  if (opts.since_days !== undefined) p.set('since_days', String(opts.since_days));
  if (opts.search) p.set('search', opts.search);
  if (opts.limit !== undefined) p.set('limit', String(opts.limit));
  if (opts.offset !== undefined) p.set('offset', String(opts.offset));
  const { data } = await api.get<WorkflowRow[]>(`/v1/apps/${appId}/workflows?${p.toString()}`);
  return data;
}

export interface ListWorkflowRunsParams {
  since_days?: number;
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
  if (opts.since_days !== undefined) p.set('since_days', String(opts.since_days));
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
