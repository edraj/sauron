import { api } from './client';
import type { DeviceRow, DeviceDetail } from '../models';

export interface ListDevicesParams {
  since_days?: number;
  limit?: number;
  offset?: number;
  search?: string;
}

export async function listDevices(
  appId: string,
  params: ListDevicesParams = {},
): Promise<DeviceRow[]> {
  const { data } = await api.get<DeviceRow[]>(`/v1/apps/${appId}/devices`, { params });
  return data;
}

// device_key is passed as a query param — keys can contain `/` and spaces.
export async function getDevice(appId: string, deviceKey: string): Promise<DeviceDetail> {
  const { data } = await api.get<DeviceDetail>(`/v1/apps/${appId}/device`, {
    params: { key: deviceKey },
  });
  return data;
}
