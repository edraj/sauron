import { api } from './client';

// ---------------------------------------------------------------------------
// Admin data purge — /v1/admin/purge.
// Mirrors backend/bins/sauron-api/src/routes/purge.rs's Serialize structs.
// ---------------------------------------------------------------------------

/**
 * One selectable kind of data.
 *
 * Served by the API rather than hardcoded here, so the vocabulary cannot drift:
 * a kind added to the `sauron-purge` crate appears in this UI, and one removed
 * disappears, with no matching frontend change.
 */
export interface PurgeKind {
  slug: string;
  /** `raw` rows are deleted; `rollup` rows are recomputed and deleted only when nothing survives. */
  class: 'raw' | 'rollup';
  /**
   * False for kinds whose table has no `environment_id` (`devices`, `issues`,
   * `persons`, `inspector`). The UI MUST disable these while an environment
   * filter is active — the API refuses them, and accepting the tick then
   * quietly doing something narrower would be worse than refusing it.
   */
  env_scoped: boolean;
}

export type PurgeStatus =
  | 'previewing'
  | 'previewed'
  | 'pending'
  | 'running'
  | 'cancelling'
  | 'done'
  | 'failed'
  | 'cancelled';

export interface PurgeJob {
  id: string;
  org_id: string;
  app_id: string;
  /** Snapshots, not joins — these keep history readable after the app is deleted. */
  app_slug: string;
  app_name: string;
  /** `null` = every environment, INCLUDING unattributed rows. */
  environment_ids: string[] | null;
  kinds: string[];
  range_start: string | null;
  range_end: string | null;
  /** A real field, never inferred from empty dates. */
  all_time: boolean;
  status: PurgeStatus;
  phase: 'idle' | 'counting' | 'delete' | 'recompute' | 'finished';
  /** Per-kind, from the preview. */
  estimated_counts: Record<string, number>;
  /** Per-kind, what execution actually removed. */
  deleted_counts: Record<string, number>;
  rollups_recomputed: number;
  rollups_deleted: number;
  /**
   * Rows in range that live in cold Parquet and therefore SURVIVE the purge.
   * Shown before confirming — this is the difference between what was asked
   * for and what will happen.
   */
  cold_rows_skipped: number;
  cold_boundary_at: string | null;
  requested_by_email: string;
  cancelled_by_email: string;
  requested_at: string;
  previewed_at: string | null;
  confirmed_at: string | null;
  started_at: string | null;
  finished_at: string | null;
  /** Whether the app was still receiving events when the job started. */
  ingest_active: boolean;
  error: string;
}

export interface PurgeJobResponse extends PurgeJob {
  preview_ttl_secs: number;
}

export interface PurgeCatalog {
  kinds: PurgeKind[];
  jobs: PurgeJob[];
}

export interface PreviewReq {
  app_id: string;
  /** Omit for every environment. `[]` is refused by the API, not treated as "all". */
  environment_ids?: string[];
  kinds: string[];
  range_start?: string;
  range_end?: string;
  all_time?: boolean;
}

export const purgeApi = {
  catalog: () => api.get<PurgeCatalog>('/v1/admin/purge'),

  /**
   * Returns 202 with a job in `previewing`; poll `get` until it reaches
   * `previewed`. Counting three partitioned tables is exactly the workload
   * that would otherwise sit past the server's 30s request timeout.
   */
  preview: (body: PreviewReq) => api.post<PurgeJobResponse>('/v1/admin/purge', body),

  get: (id: string) => api.get<PurgeJobResponse>(`/v1/admin/purge/${id}`),

  /** `confirmText` must equal the app slug. No scope is sent — it is frozen on the job. */
  confirm: (id: string, confirmText: string) =>
    api.post<PurgeJobResponse>(`/v1/admin/purge/${id}/confirm`, { confirm_text: confirmText }),

  /** Stops further batches. Does NOT restore rows already deleted. */
  cancel: (id: string) => api.post<PurgeJobResponse>(`/v1/admin/purge/${id}/cancel`, {}),
};

/** Statuses where the job is still moving and the UI should keep polling. */
export const ACTIVE_STATUSES: PurgeStatus[] = [
  'previewing',
  'pending',
  'running',
  'cancelling',
];

export function isActive(job: Pick<PurgeJob, 'status'>): boolean {
  return ACTIVE_STATUSES.includes(job.status);
}

/** Total across a per-kind count map. */
export function totalCount(counts: Record<string, number> | null | undefined): number {
  if (!counts) return 0;
  return Object.values(counts).reduce((a, b) => a + (b ?? 0), 0);
}

/**
 * Kinds that must be disabled given the current environment filter.
 *
 * Returns the slugs, so the caller can both disable the checkbox and explain
 * why next to it.
 */
export function blockedByEnvFilter(kinds: PurgeKind[], envFilterActive: boolean): Set<string> {
  if (!envFilterActive) return new Set();
  return new Set(kinds.filter((k) => !k.env_scoped).map((k) => k.slug));
}
