import { api } from './client';

// ---------------------------------------------------------------------------
// Admin storage report — GET /v1/admin/storage (global-admin only).
// Mirrors backend/bins/sauron-api/src/admin_storage.rs's Serialize structs.
// ---------------------------------------------------------------------------

export interface ColdFile {
  path: string;
  bytes: number;
}

export interface AppTableStorage {
  name: string;
  hot_rows: number;
  cold_rows: number;
  cold_bytes: number;
  estimated_hot_bytes: number;
}

export interface AppStorage {
  app_id: string;
  app_name: string;
  org_name: string;
  tables: AppTableStorage[];
  hot_rows_total: number;
  cold_rows_total: number;
  cold_bytes_total: number;
  estimated_hot_bytes_total: number;
  cold_files: ColdFile[];
}

export interface TableSize {
  name: string;
  total_bytes: number;
  hot_rows: number;
}

export interface DatabaseInfo {
  total_bytes: number;
  tables: TableSize[];
}

export interface StorageReport {
  database: DatabaseInfo;
  apps: AppStorage[];
}

export async function getAdminStorage(): Promise<StorageReport> {
  const { data } = await api.get<StorageReport>('/v1/admin/storage');
  return data;
}
