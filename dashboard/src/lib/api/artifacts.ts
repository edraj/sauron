import { api } from './client';

// Symbol artifacts (source maps / Dart debug-info) — /v1/apps/{id}/artifacts.
export interface SymbolArtifact {
  id: string;
  kind: string;
  platform: string;
  arch: string | null;
  release: string | null;
  dist: string | null;
  name: string | null;
  debug_id: string | null;
  blob_sha256: string;
  has_prebuilt_index: boolean;
  uncompressed_size: number;
  compressed_size: number;
  created_at: string;
}

export interface UploadArtifactParams {
  kind: string;
  platform: string;
  release?: string;
  name?: string;
  dist?: string;
  arch?: string;
  debug_id?: string;
}

export async function listArtifacts(appId: string): Promise<SymbolArtifact[]> {
  const { data } = await api.get<SymbolArtifact[]>(`/v1/apps/${appId}/artifacts`);
  return data;
}

export async function deleteArtifact(appId: string, id: string): Promise<void> {
  await api.delete(`/v1/apps/${appId}/artifacts/${id}`);
}

export async function uploadArtifact(
  appId: string,
  file: File,
  params: UploadArtifactParams,
): Promise<{ id: string; deduped: boolean; blob_sha256: string }> {
  const qs = new URLSearchParams();
  for (const [k, v] of Object.entries(params)) {
    if (v) qs.set(k, v);
  }
  const { data } = await api.post<{ id: string; deduped: boolean; blob_sha256: string }>(
    `/v1/apps/${appId}/artifacts?${qs.toString()}`,
    file,
    { headers: { 'Content-Type': 'application/octet-stream' } },
  );
  return data;
}
