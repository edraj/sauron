import type { ViewEnvelope } from './overview';
import { api } from './client';

// ---------------------------------------------------------------------------
// Admin storage report — GET /v1/admin/storage (global-admin only).
// Mirrors backend/bins/sauron-api/src/admin_storage.rs's Serialize structs.
// ---------------------------------------------------------------------------

export interface ColdFile {
  path: string;
  bytes: number;
}

export interface AppTableStorage {
  name: string;
  hot_rows: number;
  cold_rows: number;
  cold_bytes: number;
  estimated_hot_bytes: number;
}

export interface AppStorage {
  app_id: string;
  app_name: string;
  project_name: string;
  org_name: string;
  tables: AppTableStorage[];
  hot_rows_total: number;
  cold_rows_total: number;
  cold_bytes_total: number;
  estimated_hot_bytes_total: number;
  /** Truncated by the API; `cold_files_total` is the untruncated count. */
  cold_files: ColdFile[];
  cold_files_total: number;
}

export interface TableSize {
  name: string;
  total_bytes: number;
  hot_rows: number;
  /** Hot/cold tiered tables carry an `app_id`; only these have cold counterparts. */
  tiered?: boolean;
}

export interface DatabaseInfo {
  /** Postgres bytes attributable to the caller's visible apps. */
  total_bytes: number;
  /**
   * True `pg_database_size` — indexes, TOAST and bloat included. Absent unless
   * the caller manages every org, since a shared database's physical size is
   * necessarily the sum over all tenants.
   */
  physical_bytes?: number | null;
  /** Cold/Parquet bytes across the caller's visible apps. */
  cold_bytes?: number;
  /** Caller's org set covers the whole deployment, so sizes are exact. */
  full_scope?: boolean;
  tables: TableSize[];
}

export interface StorageReport {
  database: DatabaseInfo;
  apps: AppStorage[];
}

/**
 * The report arrives inside a cache envelope: counting rows per app per tiered
 * table is measured in seconds and scales with retained data, so it moved off
 * the request path. A cold read answers `computing` with a null `data`.
 */
export async function getAdminStorage(): Promise<ViewEnvelope<StorageReport>> {
  const { data } = await api.get<ViewEnvelope<StorageReport>>('/v1/admin/storage');
  return data;
}

// ---------------------------------------------------------------------------
// Cold-tier rotation policy
// ---------------------------------------------------------------------------

/** A range protected from re-tiering because it was restored from cold. */
export interface TierPin {
  id: string;
  table_name: string;
  range_start: string;
  range_end: string;
  expires_at: string;
  created_at: string;
  reason: string | null;
  /** Server-computed, so the client never compares against its own clock. */
  expired: boolean;
  /**
   * Inside the warning window and not yet lapsed. Surfaced so a restore never
   * just disappears — the operator gets a chance to extend before the rows go.
   */
  expiring_soon: boolean;
  /** Whole hours until expiry; negative once lapsed. Also server-computed. */
  expires_in_hours: number;
}

export interface TierPolicy {
  /** From TIER_HOT_DAYS (or its default) in the API process. */
  configured_hot_days: number;
  /** What sauron-tier will use on its next cycle. */
  effective_hot_days: number;
  overridden: boolean;
  min_hot_days: number;
  updated_at: string | null;
  /** Components that track a change without a restart. */
  follows_immediately: string[];
  /**
   * Components still on their start-time configuration. Rendered verbatim: an
   * operator changing the policy needs to know the change is not yet total.
   */
  follows_on_restart: string[];
  pins: TierPin[];
  /** From SESSION_RETENTION_DAYS (or its default); 0 = keep forever. */
  configured_session_retention_days: number;
  /** What the daily retention pass will use next; 0 means retention is off. */
  effective_session_retention_days: number;
  session_retention_overridden: boolean;
  min_session_retention_days: number;
  session_retention_updated_at: string | null;
}

/**
 * Requires org-scoped `org:manage` in EVERY org — the rotation age is one
 * deployment-wide value, so a single tenant's admin must not be able to move
 * the hot/cold boundary for everyone. Expect 403 otherwise.
 */
export async function getTierPolicy(): Promise<TierPolicy> {
  const { data } = await api.get<TierPolicy>('/v1/admin/tier-policy');
  return data;
}

/**
 * `hotDays: null` clears the override and reverts to the configured value.
 *
 * LOWERING IS NOT REVERSIBLE HERE. The next tier cycle exports and then drops
 * the newly-eligible partitions; raising the number afterwards does not bring
 * them back into Postgres — that needs a restore from cold.
 */
export async function setTierPolicy(hotDays: number | null): Promise<TierPolicy> {
  const { data } = await api.put<TierPolicy>('/v1/admin/tier-policy', { hot_days: hotDays });
  return data;
}

/**
 * `retentionDays: null` clears the override; `0` turns retention off.
 *
 * ENABLING OR LOWERING DELETES DATA WITH NO WAY BACK. Sessions have no cold
 * copy: on the next daily pass, whole day-partitions past the window are
 * dropped and only the session-day rollups remain of them.
 */
export async function setSessionRetention(retentionDays: number | null): Promise<TierPolicy> {
  const { data } = await api.put<TierPolicy>('/v1/admin/session-retention', {
    retention_days: retentionDays,
  });
  return data;
}

// ---------------------------------------------------------------------------
// Cold-data restore
// ---------------------------------------------------------------------------

/** Tables that can be restored from cold. Matches the server allowlist. */
export const RESTORABLE_TABLES = ['error_events', 'analytics_events', 'transactions'] as const;
export type RestorableTable = (typeof RESTORABLE_TABLES)[number];

export type RestoreStatus = 'queued' | 'running' | 'succeeded' | 'failed' | 'cancelled';

export interface RestoreJob {
  id: string;
  table_name: string;
  /** null restores every app in the range. */
  app_id: string | null;
  range_start: string;
  range_end: string;
  status: RestoreStatus;
  /** Nulled once the pin is removed — the job history outlives the data. */
  pin_id: string | null;
  pin_expires_at: string;
  rows_estimated: number;
  rows_restored: number;
  attempts: number;
  error: string;
  created_at: string;
  started_at: string | null;
  finished_at: string | null;
}

export interface CreateRestore {
  table_name: RestorableTable;
  app_id?: string | null;
  range_start: string;
  range_end: string;
  /** Defaults to 30 server-side. Max 365. */
  expires_in_days?: number;
}

/**
 * Queue a restore. Returns immediately with a `queued` job — the copy itself
 * runs in `sauron-tier` and can take minutes, so the caller polls
 * {@link getRestore} rather than waiting on this request.
 *
 * 409 when an active restore already overlaps the range: two overlapping
 * restores would each insert the same Parquet rows under a different pin, and
 * because a pin only ever deletes its own rows the duplicates would outlive the
 * first expiry.
 */
export async function createRestore(body: CreateRestore): Promise<RestoreJob> {
  const { data } = await api.post<RestoreJob>('/v1/admin/restore', body);
  return data;
}

export async function listRestores(): Promise<RestoreJob[]> {
  const { data } = await api.get<RestoreJob[]>('/v1/admin/restore');
  return data;
}

export async function getRestore(id: string): Promise<RestoreJob> {
  const { data } = await api.get<RestoreJob>(`/v1/admin/restore/${id}`);
  return data;
}

export interface ReleasedPin {
  id: string;
  table_name: string;
  /** Rows removed from Postgres. They remain in Parquet — this is not deletion. */
  rows_deleted: number;
}

/**
 * Release a pin now, deleting the rows it restored.
 *
 * This is NOT a bare "forget the pin": the restored rows would otherwise stay in
 * Postgres with nothing able to identify them, and be counted on top of the
 * Parquet copy of the same events.
 */
export async function releasePin(id: string): Promise<ReleasedPin> {
  const { data } = await api.delete<ReleasedPin>(`/v1/admin/tier-pins/${id}`);
  return data;
}

/** Push a pin's expiry out, measured from now. The answer to an expiry warning. */
export async function extendPin(id: string, days: number): Promise<TierPin> {
  const { data } = await api.post<TierPin>(`/v1/admin/tier-pins/${id}/extend`, { days });
  return data;
}
