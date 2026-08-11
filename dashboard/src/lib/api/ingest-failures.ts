import { api } from './client';

// ---------------------------------------------------------------------------
// Ingest failure recovery — /v1/admin/ingest-failures.
// Mirrors backend/bins/sauron-api/src/routes/failures.rs's Serialize structs.
// ---------------------------------------------------------------------------

/**
 * One *kind* of ingest failure, not one event.
 *
 * A bad deploy that produces 242,700 identical failures is ONE row here with
 * `occurrences: 242700`. The individual payloads are retained separately and
 * capped, which is what `retained` and `dropped` describe.
 */
export interface IngestFailure {
  id: string;
  /** Stable group key. Shown truncated; useful when correlating with logs. */
  fingerprint: string;
  /** Low-cardinality slug: `decode`, `db_contention`, `db_fk_violation`, … */
  error_kind: string;
  error_message: string;
  org_id: string | null;
  project_id: string | null;
  app_id: string | null;
  /** Snapshot, not a join — stays readable after the app is deleted. */
  app_name: string;
  /** Everything ever seen, including occurrences the payload cap refused. */
  occurrences: number;
  /** Payloads actually kept. These are the ones Retry can replay. */
  retained: number;
  /**
   * `occurrences - retained`: events that are gone for good.
   *
   * Rendered wherever non-zero. A Retry button that implies full recovery when
   * only 1,000 of 242,700 payloads survive is precisely the silent-truncation
   * failure this page exists to make visible.
   */
  dropped: number;
  status: 'failed' | 'requeued' | 'resolved';
  first_seen_at: string;
  last_seen_at: string;
}

export interface IngestFailurePayload {
  id: string;
  failure_id: string;
  /** Already PII-masked by the worker before it was ever stored. */
  payload: unknown;
  attempts: number;
  created_at: string;
  requeued_at: string | null;
}

export interface IngestFailurePage {
  failures: IngestFailure[];
  /** `null` on the last page. Opaque — do not parse or rebuild it. */
  next_cursor: string | null;
}

export interface RetryResult {
  requeued: number;
  /** Payloads the re-injection could not place. */
  failed: number;
  /** Occurrences that were never retained and can never be replayed. */
  unrecoverable: number;
}

export interface ListParams {
  status?: string;
  error_kind?: string;
  limit?: number;
  cursor?: string;
}

export async function listIngestFailures(params: ListParams = {}): Promise<IngestFailurePage> {
  const { data } = await api.get<IngestFailurePage>('/v1/admin/ingest-failures', { params });
  return data;
}

export async function getIngestFailurePayloads(
  id: string,
  limit = 20,
): Promise<IngestFailurePayload[]> {
  const { data } = await api.get<IngestFailurePayload[]>(
    `/v1/admin/ingest-failures/${id}/payloads`,
    { params: { limit } },
  );
  return data;
}

export async function retryIngestFailure(id: string): Promise<RetryResult> {
  const { data } = await api.post<RetryResult>(`/v1/admin/ingest-failures/${id}/retry`);
  return data;
}

/** Permanent. The audit-log entry is the only thing that survives. */
export async function dropIngestFailure(id: string): Promise<void> {
  await api.delete(`/v1/admin/ingest-failures/${id}`);
}
