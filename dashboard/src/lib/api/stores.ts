import { api } from './client';

// App-store install/uninstall metrics.
//
// None of these endpoints take `environment_id` — the backend rejects it with a
// 400, and `scope.ts`'s `BACKEND_REJECTS_ENVIRONMENT_ID` lists both routes so
// the axios interceptor does not attach one. Google and Apple report per
// package/bundle id and have no environment dimension; the app's
// `store_environment_id` decides where the Overview section is shown, not which
// numbers it shows.

export type StoreKind = 'google_play' | 'app_store';

/**
 * `never_synced` — saved, the daemon has not reached it yet.
 * `pending`      — App Store only: the ongoing report was requested and Apple
 *                  has not published an instance yet (its normal 24-48h
 *                  window). Deliberately NOT an error.
 * `ok` / `error` — as they read.
 */
export type StoreState = 'never_synced' | 'pending' | 'ok' | 'error';

export interface StoreConnection {
  store: StoreKind;
  enabled: boolean;
  /** Shape depends on `store` — see `STORE_FIELDS` in StoreConnectionsCard. */
  identifiers: Record<string, string>;
  /** The credential itself is never returned; only whether one is stored. */
  has_secret: boolean;
  secret_updated_at: string | null;
  state: StoreState;
  last_synced_at: string | null;
  last_error: string | null;
}

export interface StoreCounts {
  installs: number;
  uninstalls: number;
}

/**
 * One day.
 *
 * A store key is ABSENT when that store published nothing for the day — it is
 * deliberately not `{installs: 0, uninstalls: 0}`, because zero is a real value
 * that means something different ("nobody installed it") from silence ("the
 * store has not told us yet").
 */
export interface StoreDay {
  day: string;
  google_play?: StoreCounts;
  app_store?: StoreCounts;
}

export interface PendingDay {
  day: string;
  /** Rendered verbatim. */
  reason: string;
}

export interface StoreMetrics {
  series: StoreDay[];
  pending_days: PendingDay[];
  stores: StoreConnection[];
}

export async function listStoreConnections(appId: string): Promise<StoreConnection[]> {
  const { data } = await api.get<StoreConnection[]>(`/v1/apps/${appId}/store-connections`);
  return data;
}

/**
 * Omit `secret` to leave the stored credential untouched; pass `null` to clear
 * it.
 *
 * Never send `secret: ''` — the backend refuses it with a 400 rather than
 * storing a credential that can never authenticate. A form must not turn an
 * untouched password field into an empty string.
 */
export async function upsertStoreConnection(
  appId: string,
  store: StoreKind,
  body: { identifiers: Record<string, string>; secret?: string | null },
): Promise<StoreConnection> {
  const { data } = await api.put<StoreConnection>(
    `/v1/apps/${appId}/store-connections/${store}`,
    body,
  );
  return data;
}

/** Removes the credential. Collected history is deliberately kept. */
export async function deleteStoreConnection(appId: string, store: StoreKind): Promise<void> {
  await api.delete(`/v1/apps/${appId}/store-connections/${store}`);
}

/**
 * Makes the connection due now and returns 202. `sauron-storesync` does the
 * work on its next pass — nothing is fetched inside this request, so callers
 * must not tell the user the data is now fresh.
 */
export async function queueStoreSync(appId: string, store: StoreKind): Promise<void> {
  await api.post(`/v1/apps/${appId}/store-connections/${store}/sync`);
}

export async function getStoreMetrics(appId: string, sinceDays = 30): Promise<StoreMetrics> {
  const { data } = await api.get<StoreMetrics>(`/v1/apps/${appId}/store-metrics`, {
    params: { since_days: sinceDays },
  });
  return data;
}
