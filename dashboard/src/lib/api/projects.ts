import { api } from './client';
import type { Project } from '../models';

export async function listProjects(orgId: string): Promise<Project[]> {
  const { data } = await api.get<Project[]>(`/v1/orgs/${orgId}/projects`);
  return data;
}

export async function createProject(
  orgId: string,
  body: { name: string },
): Promise<Project> {
  const { data } = await api.post<Project>(`/v1/orgs/${orgId}/projects`, body);
  return data;
}

export async function getProject(projectId: string): Promise<Project> {
  const { data } = await api.get<Project>(`/v1/projects/${projectId}`);
  return data;
}

export async function updateProject(
  projectId: string,
  body: { name: string },
): Promise<Project> {
  const { data } = await api.patch<Project>(`/v1/projects/${projectId}`, body);
  return data;
}

export async function deleteProject(projectId: string): Promise<void> {
  await api.delete(`/v1/projects/${projectId}`);
}
