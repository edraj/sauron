import { api } from './client';
import type {
  AccessResponse,
  CreateGrantPayload,
  CreateMemberPayload,
  CreateMemberResult,
  CreateRolePayload,
  MemberGrant,
  MemberPasswordResetResult,
  Organization,
  Role,
  UpdateGrantPayload,
  UpdateRolePayload,
} from '../models';

export async function listOrgs(): Promise<Organization[]> {
  const { data } = await api.get<Organization[]>('/v1/orgs');
  return data;
}

export async function createOrg(name: string): Promise<Organization> {
  const { data } = await api.post<Organization>('/v1/orgs', { name });
  return data;
}

// ---------------------------------------------------------------------------
// Access control
// ---------------------------------------------------------------------------

export async function getAccess(orgId: string): Promise<AccessResponse> {
  const { data } = await api.get<AccessResponse>(`/v1/orgs/${orgId}/access`);
  return data;
}

export async function listMembers(orgId: string): Promise<MemberGrant[]> {
  const { data } = await api.get<MemberGrant[]>(`/v1/orgs/${orgId}/members`);
  return data;
}

export async function createGrant(
  orgId: string,
  body: CreateGrantPayload,
): Promise<{ ids: string[]; id?: string }> {
  const { data } = await api.post<{ ids: string[]; id?: string }>(
    `/v1/orgs/${orgId}/grants`,
    body,
  );
  return data;
}

export async function deleteGrant(grantId: string): Promise<void> {
  await api.delete(`/v1/grants/${grantId}`);
}

export async function listRoles(orgId: string): Promise<Role[]> {
  const { data } = await api.get<Role[]>(`/v1/orgs/${orgId}/roles`);
  return data;
}

export async function createRole(
  orgId: string,
  body: CreateRolePayload,
): Promise<Role> {
  const { data } = await api.post<Role>(`/v1/orgs/${orgId}/roles`, body);
  return data;
}

export async function createMember(
  orgId: string,
  body: CreateMemberPayload,
): Promise<CreateMemberResult> {
  const { data } = await api.post<CreateMemberResult>(`/v1/orgs/${orgId}/members`, body);
  return data;
}

export async function setMemberActive(
  orgId: string,
  userId: string,
  isActive: boolean,
): Promise<void> {
  await api.patch(`/v1/orgs/${orgId}/members/${userId}`, { is_active: isActive });
}

/**
 * Goes through `api`, not `bareClient`: it needs the bearer.
 *
 * `action: 'reset'` is destructive — it stops the member's current password
 * authenticating. `'cancel'` is its undo and is the only one of the two that
 * works on a deployment with no SMTP configured.
 */
export async function resetMemberPassword(
  orgId: string,
  userId: string,
  action: 'reset' | 'cancel',
): Promise<MemberPasswordResetResult> {
  const { data } = await api.post<MemberPasswordResetResult>(
    `/v1/orgs/${orgId}/members/${userId}/password-reset`,
    { action },
  );
  return data;
}

export async function updateGrant(
  grantId: string,
  body: UpdateGrantPayload,
): Promise<{ id: string }> {
  const { data } = await api.patch<{ id: string }>(`/v1/grants/${grantId}`, body);
  return data;
}

export async function updateRole(
  orgId: string,
  roleId: string,
  body: UpdateRolePayload,
): Promise<Role> {
  const { data } = await api.patch<Role>(`/v1/orgs/${orgId}/roles/${roleId}`, body);
  return data;
}
