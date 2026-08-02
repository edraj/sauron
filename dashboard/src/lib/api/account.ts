import { api } from './client';
import type { AccountSession } from '../models';

/**
 * The `/v1/me/*` namespace. Bearer-authenticated, so this goes through `api`
 * and never `bareClient` — these calls must participate in the 401
 * refresh-and-replay.
 *
 * `api/scope.ts`'s `computeScopeParams` only matches `/^\/v1\/apps\/[^/]+/`, so
 * none of these paths pick up an `environment_id` param and no
 * `BACKEND_REJECTS_ENVIRONMENT_ID` entry is needed — which is what keeps the
 * Rust-side router enumeration in `http_env_scoping.rs` green.
 */
export async function listMySessions(includeRevoked = false): Promise<AccountSession[]> {
  const { data } = await api.get<AccountSession[]>('/v1/me/sessions', {
    params: includeRevoked ? { include_revoked: 1 } : undefined,
  });
  return data;
}

export async function revokeMySession(sessionId: string): Promise<void> {
  await api.delete(`/v1/me/sessions/${sessionId}`);
}

export async function revokeMyOtherSessions(): Promise<number> {
  const { data } = await api.post<{ ok: boolean; revoked: number }>(
    '/v1/me/sessions/revoke-others',
    {},
  );
  return data.revoked;
}

/** Admin force-logout. Requires `member:credential` AND `member:manage`. */
export async function revokeMemberSessions(orgId: string, userId: string): Promise<number> {
  const { data } = await api.post<{ ok: boolean; revoked: number }>(
    `/v1/orgs/${orgId}/members/${userId}/revoke-sessions`,
    {},
  );
  return data.revoked;
}
