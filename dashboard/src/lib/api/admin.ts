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
}

export interface DatabaseInfo {
  total_bytes: number;
  tables: TableSize[];
}

export interface StorageReport {
  database: DatabaseInfo;
  apps: AppStorage[];
}

export async function getAdminStorage(): Promise<StorageReport> {
  const { data } = await api.get<StorageReport>('/v1/admin/storage');
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
