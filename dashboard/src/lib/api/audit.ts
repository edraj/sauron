import { api } from './client';
import { downloadCsv } from './download';

// ---------------------------------------------------------------------------
// Wall of Shame — GET /v1/admin/audit.
// Mirrors backend/bins/sauron-api/src/routes/audit.rs's Serialize structs.
// ---------------------------------------------------------------------------

/**
 * One recorded action.
 *
 * Every `*_name` is a snapshot taken when the action happened, not a join, so
 * an entry stays readable after its target is deleted. That is deliberate:
 * the entries you most want are usually about things that no longer exist.
 */
export interface AuditEntry {
  id: string;
  actor_id: string | null;
  actor_email: string;
  /** `entity.verb`, e.g. `environment.create`. */
  action: string;
  entity_type: string;
  entity_id: string | null;
  entity_name: string;
  project_id: string | null;
  project_name: string;
  app_id: string | null;
  app_name: string;
  environment_id: string | null;
  environment_name: string;
  /** `{field: {from, to}}`, changed fields only. `{}` when there is no diff. */
  changes: Record<string, { from: unknown; to: unknown }>;
  created_at: string;
  /**
   * `'audit'` for rows this feature writes, `'inspector'` for the two
   * pre-existing PII audit tables the backend projects into the same shape.
   * Inspector rows carry detail fields rather than a before/after diff.
   */
  source: 'audit' | 'inspector';
}

export interface AuditFacet {
  id: string | null;
  label: string;
}

export interface AuditFacets {
  actors: AuditFacet[];
  actions: AuditFacet[];
  projects: AuditFacet[];
  apps: AuditFacet[];
  environments: AuditFacet[];
}

export interface AuditPage {
  entries: AuditEntry[];
  /** `null` on the last page. */
  next_cursor: string | null;
  facets: AuditFacets;
}

export interface AuditFilters {
  /**
   * Include sign-in activity. Omitted or false keeps auth events out — they are
   * a separate stream so logins cannot bury the admin events the Wall is for.
   */
  include_auth?: boolean | null;
  project_id?: string | null;
  app_id?: string | null;
  environment_id?: string | null;
  actor_id?: string | null;
  action?: string | null;
  entity_type?: string | null;
  /** RFC3339. */
  from?: string | null;
  to?: string | null;
}

/**
 * One page of the org's trail.
 *
 * `org_id` is required by the API and authorized against the caller's grants,
 * so this cannot be used to read another tenant — passing someone else's id
 * returns 403 rather than their history.
 */
export async function getAuditLog(
  orgId: string,
  filters: AuditFilters = {},
  cursor?: string | null,
  limit = 50,
): Promise<AuditPage> {
  const params = new URLSearchParams({ org_id: orgId, limit: String(limit) });
  for (const [key, value] of Object.entries(filters)) {
    // Empty string is what an unselected <select> yields; sending it would
    // filter for a literal empty value and return nothing.
    if (value) params.set(key, value);
  }
  if (cursor) params.set('cursor', cursor);
  const { data } = await api.get<AuditPage>(`/v1/admin/audit?${params.toString()}`);
  return data;
}

/**
 * Download the current filtered view as CSV.
 *
 * The server exports every matching row (up to its cap), not the page the
 * browser is showing — an export that silently covered only the loaded rows
 * would look complete and not be. Goes through `downloadCsv`, which keeps the
 * bearer header and reads the filename from the CORS-exposed
 * `Content-Disposition`.
 */
export function downloadAuditCsv(orgId: string, filters: AuditFilters = {}): Promise<void> {
  const params: Record<string, unknown> = { org_id: orgId };
  for (const [key, value] of Object.entries(filters)) {
    if (value) params[key] = value;
  }
  return downloadCsv('/v1/admin/audit.csv', params, `sauron-audit-${orgId}.csv`);
}
