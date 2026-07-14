/**
 * Stable device + session identity.
 *
 * - `device_id` is persisted in `localStorage` (`sauron.device_id`) so it
 *   survives reloads and tabs — the backend uses it as the durable device
 *   identity (`context.device.device_id`).
 * - `session_id` is persisted in `sessionStorage` (`sauron.session_id`) so it is
 *   shared across a tab's page loads but starts fresh for a new browsing
 *   session. It is attached to every event, error and transaction item.
 *
 * Both degrade gracefully: with no writable Web Storage (SSR, private mode,
 * blocked cookies) we fall back to a per-process id generated once in memory.
 */

import { uuidv4 } from './utils.js';

/** localStorage key holding the durable device id. */
export const DEVICE_ID_KEY = 'sauron.device_id';
/** sessionStorage key holding the per-session id. */
export const SESSION_ID_KEY = 'sauron.session_id';

/** The minimal Web Storage surface we need (a subset of `Storage`). */
interface WebStorageLike {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

/** Return a named Web Storage area if present and writable, else `null`. */
function webStorage(name: 'localStorage' | 'sessionStorage'): WebStorageLike | null {
  try {
    const s = (globalThis as Record<string, unknown>)[name] as WebStorageLike | undefined;
    if (!s) return null;
    const probe = '__sauron_probe__';
    s.setItem(probe, '1');
    s.removeItem(probe);
    return s;
  } catch {
    // Storage disabled (private mode, blocked cookies, SSR, ...).
    return null;
  }
}

/**
 * Return the persisted id under `key`, generating and persisting a fresh v4
 * uuid when absent. `cached` short-circuits repeated lookups and doubles as the
 * in-memory fallback when no storage is available.
 */
function persistentId(cached: string | null, storage: WebStorageLike | null, key: string): string {
  if (cached) return cached;
  if (storage) {
    try {
      const existing = storage.getItem(key);
      if (existing) return existing;
    } catch {
      /* fall through and generate */
    }
  }
  const fresh = uuidv4();
  if (storage) {
    try {
      storage.setItem(key, fresh);
    } catch {
      /* best effort — degrade to the in-memory value returned below */
    }
  }
  return fresh;
}

let deviceId: string | null = null;
let sessionId: string | null = null;

/** The stable device id (persisted in localStorage; per-process fallback). */
export function getDeviceId(): string {
  deviceId = persistentId(deviceId, webStorage('localStorage'), DEVICE_ID_KEY);
  return deviceId;
}

/** The current session id (persisted in sessionStorage; in-memory fallback). */
export function getSessionId(): string {
  sessionId = persistentId(sessionId, webStorage('sessionStorage'), SESSION_ID_KEY);
  return sessionId;
}

/** Drop the in-memory memoization (used by tests and teardown). */
export function resetIdentity(): void {
  deviceId = null;
  sessionId = null;
}
