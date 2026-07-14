import { api } from './client';
import type {
  AccessResponse,
  CreateGrantPayload,
  CreateRolePayload,
  MemberGrant,
  Organization,
  Role,
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
): Promise<{ id: string }> {
  const { data } = await api.post<{ id: string }>(`/v1/orgs/${orgId}/grants`, body);
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
