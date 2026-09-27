import { api } from './client';
import { overFetched, type ListPage } from '../models/list-state';
import type {
  DeviceRow,
  DeviceGroupRow,
  ErrorEvent,
  PerfSummaryRow,
  Session,
} from '../models';

export interface ListDevicesParams {
  /**
   * Rows to RENDER. The request asks for one more than this — see
   * `overFetched` — so `limit` is required rather than defaulted: without a
   * definite value there is nothing to over-fetch by and nothing to trim to.
   */
  limit: number;
  offset: number;
  since_days?: number;
  /**
   * The time window column, as `models/time-filter`'s `toRecord` encodes it.
   * Accepts `last_seen` (default) and `first_seen`; anything else is a 400.
   *
   * On this list the window decides WHICH DEVICES ARE LISTED, via the durable
   * `devices` column — not via the value the row displays. Under a scoped read
   * the displayed `first_seen`/`last_seen` are per-environment extrema, and a
   * device's per-environment first sighting can postdate its app-level one.
   * That predates this parameter (it is what `since_days` always did) and is
   * also the only form an index can serve. The Persons list deliberately does
   * the opposite — see `ListPersonsParams`.
   */
  time_field?: string;
  /** RFC3339 UTC, inclusive. Suppresses `since_days` server-side. */
  from?: string;
  /** RFC3339 UTC, **exclusive**. Suppresses `since_days` server-side. */
  to?: string;
  /**
   * `sort=` as `sortParam()` encodes it — a BARE column descends, a `-` prefix
   * ascends. Build it with `sortParam`, never by hand. Anything outside the
   * endpoint's whitelist is a 400, not a silently ignored parameter, and the
   * flat and grouped whitelists are NOT the same list: `browser` and
   * `distinct_id` exist only here, `device_count` only on the groups endpoint.
   */
  sort?: string;
  search?: string;
  // The drill-down filter. `group: '1'` is the sentinel that turns the four
  // descriptor fields on; without it the backend ignores them. An omitted
  // field means SQL NULL, which is how the all-NULL group is addressed.
  group?: string;
  family?: string;
  model?: string;
  os_name?: string;
  os_version?: string;
}

/**
 * One page of devices, plus whether another page follows.
 *
 * Requests `limit + 1` and returns `limit`. The surplus row is the has-more
 * probe: it is the only way to distinguish a final page of exactly `limit`
 * rows from a full one, and guessing `rows.length >= limit` offered a Next
 * button that led to an empty page. The endpoint clamps `limit` at 200, so
 * every page size the UI offers stays inside the clamp with the probe added.
 */
export async function listDevices(
  appId: string,
  params: ListDevicesParams,
): Promise<ListPage<DeviceRow>> {
  const { data } = await api.get<DeviceRow[]>(`/v1/apps/${appId}/devices`, {
    params: { ...params, limit: params.limit + 1 },
  });
  return overFetched(data, params.limit);
}

/** [`listDevices`] for the grouped view; the same `limit + 1` probe. */
export async function listDeviceGroups(
  appId: string,
  params: ListDevicesParams,
): Promise<ListPage<DeviceGroupRow>> {
  const { data } = await api.get<DeviceGroupRow[]>(`/v1/apps/${appId}/device-groups`, {
    params: { ...params, limit: params.limit + 1 },
  });
  return overFetched(data, params.limit);
}

// device_key is passed as a query param — keys can contain `/` and spaces.
// Four sections rather than the composite `GET …/device` — see the note on
// `getIssueSummary` (api/issues.ts). The key travels as a query parameter on
// all of them, as it does on the composite: device keys contain `/` and spaces.

/** The device row alone — what the header, the stat tiles and Hardware render. */
export async function getDeviceSummary(appId: string, deviceKey: string): Promise<DeviceRow> {
  const { data } = await api.get<{ device: DeviceRow }>(`/v1/apps/${appId}/device/summary`, {
    params: { key: deviceKey },
  });
  return data.device;
}

/** The device's 50 most recently active sessions in the last 90 days. */
export async function getDeviceSessions(appId: string, deviceKey: string): Promise<Session[]> {
  const { data } = await api.get<{ sessions: Session[] }>(`/v1/apps/${appId}/device/sessions`, {
    params: { key: deviceKey },
  });
  return data.sessions;
}

/** The device's 50 most recent errors. */
export async function getDeviceErrors(appId: string, deviceKey: string): Promise<ErrorEvent[]> {
  const { data } = await api.get<{ errors: ErrorEvent[] }>(`/v1/apps/${appId}/device/errors`, {
    params: { key: deviceKey },
  });
  return data.errors;
}

/** Per-operation latency for the device over the last 90 days. */
export async function getDevicePerf(appId: string, deviceKey: string): Promise<PerfSummaryRow[]> {
  const { data } = await api.get<{ perf: PerfSummaryRow[] }>(`/v1/apps/${appId}/device/perf`, {
    params: { key: deviceKey },
  });
  return data.perf;
}
