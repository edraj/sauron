import { api } from './client';
import type {
  AlertEvent,
  AlertMeta,
  AlertRule,
  AlertSeverity,
  ChannelKind,
  NotificationChannel,
  TriggerType,
} from '../models';

// --- channels ---------------------------------------------------------------

export async function listChannels(orgId: string): Promise<NotificationChannel[]> {
  const { data } = await api.get<NotificationChannel[]>(
    `/v1/orgs/${orgId}/notification-channels`,
  );
  return data;
}

export interface CreateChannelBody {
  name: string;
  kind: ChannelKind;
  config?: Record<string, unknown>;
  /** Write-only. Never returned by the API. */
  secret?: Record<string, string>;
}

export async function createChannel(
  orgId: string,
  body: CreateChannelBody,
): Promise<NotificationChannel> {
  const { data } = await api.post<NotificationChannel>(
    `/v1/orgs/${orgId}/notification-channels`,
    body,
  );
  return data;
}

export interface UpdateChannelBody {
  name?: string;
  config?: Record<string, unknown>;
  /** Omit to keep the stored secret, `{}` to clear it, values to replace it. */
  secret?: Record<string, string>;
  enabled?: boolean;
}

export async function updateChannel(
  channelId: string,
  body: UpdateChannelBody,
): Promise<NotificationChannel> {
  const { data } = await api.patch<NotificationChannel>(
    `/v1/notification-channels/${channelId}`,
    body,
  );
  return data;
}

export async function deleteChannel(channelId: string): Promise<void> {
  await api.delete(`/v1/notification-channels/${channelId}`);
}

export interface TestChannelResult {
  ok: boolean;
  attempts: number;
  error?: string;
}

/** Send a test notification so the admin can verify the wiring end to end. */
export async function testChannel(channelId: string): Promise<TestChannelResult> {
  const { data } = await api.post<TestChannelResult>(
    `/v1/notification-channels/${channelId}/test`,
  );
  return data;
}

// --- rules ------------------------------------------------------------------

export async function listRules(orgId: string): Promise<AlertRule[]> {
  const { data } = await api.get<AlertRule[]>(`/v1/orgs/${orgId}/alert-rules`);
  return data;
}

export interface CreateRuleBody {
  name: string;
  trigger_type: TriggerType;
  project_id?: string | null;
  app_id?: string | null;
  /** Pins a monitor_down/monitor_up rule to one monitor; the API derives project_id from it, so send this XOR project_id. */
  monitor_id?: string | null;
  conditions?: Record<string, unknown>;
  severity?: AlertSeverity;
  throttle_seconds?: number;
  message_template?: string | null;
  channel_ids?: string[];
}

export async function createRule(orgId: string, body: CreateRuleBody): Promise<AlertRule> {
  const { data } = await api.post<AlertRule>(`/v1/orgs/${orgId}/alert-rules`, body);
  return data;
}

export interface UpdateRuleBody {
  name?: string;
  enabled?: boolean;
  conditions?: Record<string, unknown>;
  severity?: AlertSeverity;
  throttle_seconds?: number;
  message_template?: string | null;
  channel_ids?: string[];
}

export async function updateRule(ruleId: string, body: UpdateRuleBody): Promise<AlertRule> {
  const { data } = await api.patch<AlertRule>(`/v1/alert-rules/${ruleId}`, body);
  return data;
}

export async function deleteRule(ruleId: string): Promise<void> {
  await api.delete(`/v1/alert-rules/${ruleId}`);
}

// --- history + metadata -----------------------------------------------------

export async function listAlertEvents(
  orgId: string,
  limit = 50,
  offset = 0,
): Promise<AlertEvent[]> {
  const { data } = await api.get<AlertEvent[]>(`/v1/orgs/${orgId}/alert-events`, {
    params: { limit, offset },
  });
  return data;
}

export async function getAlertMeta(): Promise<AlertMeta> {
  const { data } = await api.get<AlertMeta>('/v1/alert-meta');
  return data;
}
