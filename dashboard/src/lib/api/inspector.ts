// One exported async fn per inspector endpoint.
//
// Imports ONLY `{ api }` from ./client, so the bearer header and the
// single-flight 401 refresh-and-replay apply. Request-body interfaces live
// here; response types live in models/index.ts.

import { api } from './client';
import { downloadCsv } from './download';
import type {
  EffectivePolicy,
  FindingsPage,
  InspectorMaskAction,
  InspectorMaskedKey,
  InspectorPolicy,
  InspectorScan,
  MaskPreviewStart,
  RevealResult,
} from '../models';

export interface CreatePolicyBody {
  target_type: 'project' | 'app' | 'app_env';
  target_id: string;
  tracked_keys?: { key: string; scope: 'any' | 'top' }[];
  detectors?: string[];
  scan_columns?: string[] | null;
  rollups?: string[];
  window_days?: number;
  schedule_enabled?: boolean;
  schedule_days?: number;
  schedule_time?: string;
  schedule_tz?: string;
}

export type PatchPolicyBody = Partial<CreatePolicyBody> & { enabled?: boolean };

export async function listPolicies(orgId: string): Promise<InspectorPolicy[]> {
  const { data } = await api.get<InspectorPolicy[]>(`/v1/orgs/${orgId}/inspector/policies`);
  return data;
}

export async function createPolicy(orgId: string, body: CreatePolicyBody): Promise<InspectorPolicy> {
  const { data } = await api.post<InspectorPolicy>(`/v1/orgs/${orgId}/inspector/policies`, body);
  return data;
}

export async function getPolicy(policyId: string): Promise<InspectorPolicy> {
  const { data } = await api.get<InspectorPolicy>(`/v1/inspector/policies/${policyId}`);
  return data;
}

export async function patchPolicy(policyId: string, body: PatchPolicyBody): Promise<InspectorPolicy> {
  const { data } = await api.patch<InspectorPolicy>(`/v1/inspector/policies/${policyId}`, body);
  return data;
}

export async function deletePolicy(policyId: string): Promise<void> {
  await api.delete(`/v1/inspector/policies/${policyId}`);
}

export async function effectivePolicy(appId: string): Promise<EffectivePolicy> {
  const { data } = await api.get<EffectivePolicy>(`/v1/apps/${appId}/inspector/policy`);
  return data;
}

export async function listScans(policyId: string, limit = 20): Promise<InspectorScan[]> {
  const { data } = await api.get<InspectorScan[]>(`/v1/inspector/policies/${policyId}/scans`, {
    params: { limit },
  });
  return data;
}

export async function startScan(policyId: string): Promise<InspectorScan> {
  const { data } = await api.post<InspectorScan>(`/v1/inspector/policies/${policyId}/scans`);
  return data;
}

export async function getScan(scanId: string): Promise<InspectorScan> {
  const { data } = await api.get<InspectorScan>(`/v1/inspector/scans/${scanId}`);
  return data;
}

export async function cancelScan(scanId: string): Promise<void> {
  await api.post(`/v1/inspector/scans/${scanId}/cancel`);
}

export async function listFindings(
  scanId: string,
  opts: { limit?: number; afterCount?: number; afterId?: string } = {},
): Promise<FindingsPage> {
  const { data } = await api.get<FindingsPage>(`/v1/inspector/scans/${scanId}/findings`, {
    params: {
      limit: opts.limit ?? 100,
      after_count: opts.afterCount,
      after_id: opts.afterId,
    },
  });
  return data;
}

/**
 * Buffered CSV. Goes through `downloadCsv`, which uses the shared `api`
 * instance so refresh-and-replay still works and reads the blob back as text
 * on a non-2xx — `normalizeError` reads `error.response.data` as an
 * `{error:{code,message}}` envelope, and with `responseType: 'blob'` that data
 * IS a Blob and the message is lost.
 *
 * `filename` is only the FALLBACK: the server sends `Content-Disposition` and
 * `downloadCsv` prefers it, so the two agree unless CORS stops exposing the
 * header.
 */
export async function downloadFindingsCsv(scanId: string, filename: string): Promise<void> {
  await downloadCsv(`/v1/inspector/scans/${scanId}/findings`, { format: 'csv' }, filename);
}

export async function revealFinding(findingId: string): Promise<RevealResult> {
  const { data } = await api.post<RevealResult>(`/v1/inspector/findings/${findingId}/reveal`, {});
  return data;
}

export async function maskPreview(
  appId: string,
  body: { finding_id?: string; targets?: { table: string; column: string; path: string }[] },
): Promise<MaskPreviewStart> {
  const { data } = await api.post<MaskPreviewStart>(
    `/v1/apps/${appId}/inspector/mask-preview`,
    body,
  );
  return data;
}

export async function getMaskAction(actionId: string): Promise<InspectorMaskAction> {
  const { data } = await api.get<InspectorMaskAction>(`/v1/inspector/mask-actions/${actionId}`);
  return data;
}

export async function confirmMask(
  actionId: string,
  confirmText: string,
): Promise<{ action: InspectorMaskAction; enforcement_latency_secs: number }> {
  const { data } = await api.post(`/v1/inspector/mask-actions/${actionId}/confirm`, {
    confirm_text: confirmText,
  });
  return data;
}

export async function cancelMask(actionId: string): Promise<InspectorMaskAction> {
  const { data } = await api.post<InspectorMaskAction>(
    `/v1/inspector/mask-actions/${actionId}/cancel`,
  );
  return data;
}

export async function listAppMaskActions(appId: string, limit = 100): Promise<InspectorMaskAction[]> {
  const { data } = await api.get<InspectorMaskAction[]>(
    `/v1/apps/${appId}/inspector/mask-actions`,
    { params: { limit } },
  );
  return data;
}

export async function listOrgMaskActions(orgId: string, limit = 100): Promise<InspectorMaskAction[]> {
  const { data } = await api.get<InspectorMaskAction[]>(
    `/v1/orgs/${orgId}/inspector/mask-actions`,
    { params: { limit } },
  );
  return data;
}

export async function downloadMaskActionsCsv(appId: string, filename: string): Promise<void> {
  await downloadCsv(`/v1/apps/${appId}/inspector/mask-actions`, { format: 'csv' }, filename);
}

export async function listMaskedKeys(
  appId: string,
): Promise<{ masked_keys: InspectorMaskedKey[]; enforcement_latency_secs: number }> {
  const { data } = await api.get(`/v1/apps/${appId}/inspector/masked-keys`);
  return data;
}
