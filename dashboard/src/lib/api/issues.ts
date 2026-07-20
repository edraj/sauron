import { api } from './client';
import type {
  ErrorEvent,
  Issue,
  IssueDetail,
  IssueStats,
  IssueStatus,
} from '../models';

export async function getIssueStats(appId: string, sinceDays = 30): Promise<IssueStats> {
  const { data } = await api.get<IssueStats>(`/v1/apps/${appId}/issues/stats`, {
    params: { since_days: sinceDays },
  });
  return data;
}

export interface ListIssuesParams {
  filters?: string[];
  q?: string;
  sinceDays?: number;
  limit?: number;
  offset?: number;
}

export async function listIssues(
  appId: string,
  opts: ListIssuesParams = {},
): Promise<Issue[]> {
  const p = new URLSearchParams();
  for (const f of opts.filters ?? []) p.append('filter', f);
  if (opts.q) p.set('q', opts.q);
  if (opts.sinceDays != null) p.set('since_days', String(opts.sinceDays));
  if (opts.limit != null) p.set('limit', String(opts.limit));
  if (opts.offset != null) p.set('offset', String(opts.offset));
  const { data } = await api.get<Issue[]>(`/v1/apps/${appId}/issues?${p.toString()}`);
  return data;
}

export async function getIssue(appId: string, issueId: string): Promise<IssueDetail> {
  const { data } = await api.get<IssueDetail>(`/v1/apps/${appId}/issues/${issueId}`);
  return data;
}

export async function updateIssueStatus(
  appId: string,
  issueId: string,
  status: IssueStatus,
): Promise<Issue> {
  const { data } = await api.patch<Issue>(
    `/v1/apps/${appId}/issues/${issueId}`,
    { status },
  );
  return data;
}

export async function listIssueEvents(
  appId: string,
  issueId: string,
  opts: { filters?: string[]; q?: string; sinceDays?: number; limit?: number } = {},
): Promise<ErrorEvent[]> {
  const p = new URLSearchParams();
  for (const f of opts.filters ?? []) p.append('filter', f);
  if (opts.q) p.set('q', opts.q);
  if (opts.sinceDays != null) p.set('since_days', String(opts.sinceDays));
  p.set('limit', String(opts.limit ?? 50));
  const { data } = await api.get<ErrorEvent[]>(
    `/v1/apps/${appId}/issues/${issueId}/events?${p.toString()}`,
  );
  return data;
}
