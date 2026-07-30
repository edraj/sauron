import { api } from './client';
import type { AppEnvironment, AppEnvironmentRow, ProjectEnvironment } from '../models';

// Two levels, two sets of endpoints — see the model comments in `../models`.
//
//  - The CATALOGUE lives under `/v1/projects/{id}/environments` (list/create)
//    and `/v1/environments/{id}` (rename/retire). Everything here is
//    project-wide.
//  - The ENROLLMENT is listed under `/v1/apps/{id}/environments` and mutated
//    under `/v1/app-environments/{id}`. Everything here affects one app only.
//
// There is deliberately no `createEnvironment(appId, ...)`: `POST` on
// `/v1/apps/{id}/environments` was removed, because an app does not get to
// invent an environment its siblings have never heard of. Create the catalogue
// entry instead; the backend enrolls every app in the project for you.

// ---------------------------------------------------------------------------
// Enrollments (per app)
// ---------------------------------------------------------------------------

/**
 * The environments this app is enrolled in, each with its own ingest key and
 * the catalogue name joined on. `id` on each row is the enrollment id — the id
 * that goes in a DSN and in `?environment_id=`.
 */
export async function listEnvironments(
  appId: string,
  includeRetired = false,
): Promise<AppEnvironment[]> {
  const { data } = await api.get<AppEnvironment[]>(`/v1/apps/${appId}/environments`, {
    params: includeRetired ? { include_retired: true } : undefined,
  });
  return data;
}

/**
 * Flip this app's per-environment switches. `is_default: false` is rejected by
 * the backend with a 400 — a default is *moved* by promoting another
 * enrollment, never unset.
 *
 * Returns the bare enrollment row: no `name`, because the name is not stored
 * here. Callers rendering a list should keep the name they already have.
 */
export async function updateAppEnvironment(
  id: string,
  body: { ingest_enabled?: boolean; is_default?: boolean },
): Promise<AppEnvironmentRow> {
  const { data } = await api.patch<AppEnvironmentRow>(`/v1/app-environments/${id}`, body);
  return data;
}

/** Mint a new ingest key for this app in this environment. No grace period. */
export async function rotateAppEnvironmentKey(id: string): Promise<AppEnvironmentRow> {
  const { data } = await api.post<AppEnvironmentRow>(`/v1/app-environments/${id}/rotate-key`);
  return data;
}

// There is deliberately no "withdraw this app from the environment" call. The
// backend exposes no DELETE on `/v1/app-environments/{id}`, because enrollment
// happens only when an environment or an app is created — so withdrawing would
// be a one-way door with no path back short of retiring the environment
// project-wide and re-keying every sibling app. `updateAppEnvironment(id, {
// ingest_enabled: false })` expresses the same intent, reversibly.

// ---------------------------------------------------------------------------
// Catalogue (per project) — every call here is project-wide
// ---------------------------------------------------------------------------

/** The environment names this project defines, shared by all of its apps. */
export async function listProjectEnvironments(
  projectId: string,
  includeRetired = false,
): Promise<ProjectEnvironment[]> {
  const { data } = await api.get<ProjectEnvironment[]>(
    `/v1/projects/${projectId}/environments`,
    { params: includeRetired ? { include_retired: true } : undefined },
  );
  return data;
}

/**
 * Define a new environment for the project. The backend enrolls every app in
 * the project in it, each with its own freshly minted key — so the returned
 * catalogue row is not enough to render a DSN table, refetch the per-app list
 * for that.
 */
export async function createProjectEnvironment(
  projectId: string,
  body: { name: string },
): Promise<ProjectEnvironment> {
  const { data } = await api.post<ProjectEnvironment>(
    `/v1/projects/${projectId}/environments`,
    body,
  );
  return data;
}

/** Rename the catalogue entry — this renames it for every app in the project. */
export async function renameProjectEnvironment(
  envId: string,
  body: { name: string },
): Promise<ProjectEnvironment> {
  const { data } = await api.patch<ProjectEnvironment>(`/v1/environments/${envId}`, body);
  return data;
}

/**
 * Retire the catalogue entry project-wide, cascading to every app's enrollment
 * in it. Retires rather than deletes — existing data stays queryable.
 */
export async function retireProjectEnvironment(envId: string): Promise<ProjectEnvironment> {
  const { data } = await api.delete<ProjectEnvironment>(`/v1/environments/${envId}`);
  return data;
}
