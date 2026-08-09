import { api } from './client';
import type {
  Monitor,
  MonitorCheck,
  MonitorDetail,
  MonitorListItem,
} from '../models';

export async function listMonitors(projectId: string): Promise<MonitorListItem[]> {
  const { data } = await api.get<MonitorListItem[]>(`/v1/projects/${projectId}/monitors`);
  return data;
}

export interface CreateMonitorBody {
  name: string;
  kind: 'http' | 'tcp';
  target: string;
  method?: string;
  config?: Record<string, unknown>;
  interval_seconds?: number;
  timeout_ms?: number;
  webhook_url?: string;
}

export async function createMonitor(projectId: string, body: CreateMonitorBody): Promise<Monitor> {
  const { data } = await api.post<Monitor>(`/v1/projects/${projectId}/monitors`, body);
  return data;
}

export async function getMonitor(id: string): Promise<MonitorDetail> {
  const { data } = await api.get<MonitorDetail>(`/v1/monitors/${id}`);
  return data;
}

export interface UpdateMonitorBody {
  name?: string;
  enabled?: boolean;
  interval_seconds?: number;
  /**
   * Three-state, and all three are reachable: omit the key to leave the stored
   * URL alone, send `null` to clear it, send a string to replace it. There is
   * no read-back — the response redacts the URL (see `Monitor.has_webhook`), so
   * an editor must treat this as write-only and drive it from the boolean.
   */
  webhook_url?: string | null;
}

export async function updateMonitor(id: string, body: UpdateMonitorBody): Promise<Monitor> {
  const { data } = await api.patch<Monitor>(`/v1/monitors/${id}`, body);
  return data;
}

export async function deleteMonitor(id: string): Promise<void> {
  await api.delete(`/v1/monitors/${id}`);
}

export async function getMonitorChecks(id: string, hours = 24): Promise<MonitorCheck[]> {
  const { data } = await api.get<MonitorCheck[]>(`/v1/monitors/${id}/checks`, { params: { hours } });
  return data;
}
