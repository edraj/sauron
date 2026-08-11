import { api } from './client';
import type { DeviceRow, DeviceGroupRow, DeviceDetail } from '../models';

export interface ListDevicesParams {
  since_days?: number;
  limit?: number;
  offset?: number;
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

export async function listDevices(
  appId: string,
  params: ListDevicesParams = {},
): Promise<DeviceRow[]> {
  const { data } = await api.get<DeviceRow[]>(`/v1/apps/${appId}/devices`, { params });
  return data;
}

export async function listDeviceGroups(
  appId: string,
  params: ListDevicesParams = {},
): Promise<DeviceGroupRow[]> {
  const { data } = await api.get<DeviceGroupRow[]>(`/v1/apps/${appId}/device-groups`, { params });
  return data;
}

// device_key is passed as a query param — keys can contain `/` and spaces.
export async function getDevice(appId: string, deviceKey: string): Promise<DeviceDetail> {
  const { data } = await api.get<DeviceDetail>(`/v1/apps/${appId}/device`, {
    params: { key: deviceKey },
  });
  return data;
}
