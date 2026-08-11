import { api } from './client';
import { overFetched, type ListPage } from '../models/list-state';
import type { DeviceRow, DeviceGroupRow, DeviceDetail } from '../models';

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
export async function getDevice(appId: string, deviceKey: string): Promise<DeviceDetail> {
  const { data } = await api.get<DeviceDetail>(`/v1/apps/${appId}/device`, {
    params: { key: deviceKey },
  });
  return data;
}
