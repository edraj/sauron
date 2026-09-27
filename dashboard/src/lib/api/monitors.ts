import { api } from './client';
import type {
  Monitor,
  MonitorCheck,
  MonitorDetail,
  MonitorIncident,
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

// Sections rather than the composite `GET /v1/monitors/{id}` — see the note on
// `getIssueSummary` (api/issues.ts). The monitor row says what the monitor IS;
// three uptime aggregates and an incident scan say how it has been doing, and
// the first should not wait on the second.

/** The monitor and the alert rules pinned to it. */
export type MonitorSummary = Pick<MonitorDetail, 'monitor' | 'pinned_alert_rules'>;

export async function getMonitorSummary(id: string): Promise<MonitorSummary> {
  const { data } = await api.get<MonitorSummary>(`/v1/monitors/${id}/summary`);
  return data;
}

export async function getMonitorUptime(id: string): Promise<MonitorDetail['uptime']> {
  const { data } = await api.get<{ uptime: MonitorDetail['uptime'] }>(
    `/v1/monitors/${id}/uptime`,
  );
  return data.uptime;
}

/** The monitor's most recent incidents, newest first. */
export async function getMonitorIncidents(id: string): Promise<MonitorIncident[]> {
  const { data } = await api.get<MonitorIncident[]>(`/v1/monitors/${id}/incidents`);
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
