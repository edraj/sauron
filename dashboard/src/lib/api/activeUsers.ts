import { api } from './client';
import { downloadCsv } from './download';
import type { ActiveUsersReport } from '../models';

/**
 * Request parameters. Lives here rather than in `models/`, per the
 * `api/alerts.ts` convention: response and domain types are shared, request
 * shapes belong to the module that sends them.
 */
export interface ActiveUsersParams {
  /** RFC3339, already floored to a UTC day boundary by the caller. */
  from: string;
  to: string;
  /** Repeated `?selection=` tokens from `models/active-users.ts`. */
  selection: string[];
}

/**
 * `indexes: null` is load-bearing. Axios 1.x's default serializer renders an
 * array as `selection[]=a&selection[]=b`; the backend deserializes
 * `Vec<String>` with `serde_html_form`, which wants the repeated
 * `selection=a&selection=b` form. Without this the server sees an empty
 * selection and 400s.
 */
const REPEATED_KEYS = { indexes: null } as const;

export async function getActiveUsers(
  projectId: string,
  params: ActiveUsersParams,
): Promise<ActiveUsersReport> {
  const { data } = await api.get<ActiveUsersReport>(
    `/v1/projects/${projectId}/active-users`,
    { params, paramsSerializer: REPEATED_KEYS },
  );
  return data;
}

export function activeUsersCsvPath(projectId: string): string {
  return `/v1/projects/${projectId}/active-users.csv`;
}

/**
 * `fallbackFilename` is built from the same ids and EFFECTIVE dates the server
 * uses, so a download is correctly named even if CORS ever stops exposing
 * `Content-Disposition`.
 */
export async function downloadActiveUsersCsv(
  projectId: string,
  params: ActiveUsersParams,
  effective: { from: string; to: string },
): Promise<void> {
  const stamp = (iso: string) => iso.slice(0, 10).replace(/-/g, '');
  const fallback = `sauron-active-users-${projectId}-${stamp(effective.from)}_${stamp(
    effective.to,
  )}.csv`;
  await downloadCsv(activeUsersCsvPath(projectId), { ...params }, fallback);
}
