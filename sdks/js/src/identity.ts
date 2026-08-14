/**
 * Stable device + session identity.
 *
 * - `device_id` is persisted in `localStorage` (`sauron.device_id`) so it
 *   survives reloads and tabs — the backend uses it as the durable device
 *   identity (`context.device.device_id`).
 * - `session_id` is persisted in `sessionStorage` (`sauron.session_id`) so it is
 *   shared across a tab's page loads but starts fresh for a new browsing
 *   session. It is attached to every event, error and transaction item.
 * - `last_identified` is persisted in `localStorage` (`sauron.last_identified`)
 *   as a short one-way digest (`hashIdentity`) of the id passed to
 *   `identify()` — never the identifier itself — so a login by a DIFFERENT
 *   user than last time can be detected even when the app never wired
 *   `reset()` on logout — see `SauronClient.prepareIdentify`.
 *
 * All degrade gracefully: with no writable Web Storage (SSR, private mode,
 * blocked cookies) we fall back to a per-process id generated once in memory.
 */

import { uuidv4 } from './utils.js';

/** localStorage key holding the durable device id. */
export const DEVICE_ID_KEY = 'sauron.device_id';
/** sessionStorage key holding the per-session id. */
export const SESSION_ID_KEY = 'sauron.session_id';
/**
 * localStorage key holding the durable anonymous id.
 *
 * It used to live in a field on the client, re-minted on every page load, so
 * `track()` sent a new `distinct_id` each time and active users for any web app
 * counted PAGE LOADS, not people — a systematic 5-10x inflation, all of it
 * landing in the guest half of the report.
 *
 * Persisting it is a retention and consent consequence, not just an
 * implementation detail: the anon id becomes a durable first-party identifier
 * stored on the user's terminal. It is also why `reset()` exists — see
 * `SauronClient.reset`.
 */
export const ANON_ID_KEY = 'sauron.anon_id';
/**
 * localStorage key holding a short one-way digest (`hashIdentity`) of the id
 * of the last user who called `identify()` — never the identifier itself,
 * which is very often personally identifying (an email, a username) and would
 * otherwise become a second, durable, plaintext copy of it.
 *
 * Exists so a login by a DIFFERENT user can be detected on a device where the
 * app never wired `reset()` on logout. Without it, person B's anonymous
 * activity keeps flowing under person A's already-burned alias, and the server
 * resolves it to A — permanently, and with no client-side symptom.
 */
export const LAST_IDENTIFIED_KEY = 'sauron.last_identified';

/**
 * Format tag prefixed to the stored `sauron.last_identified` value, as
 * `<tag>:<digest>`.
 *
 * `hashIdentity`'s output has already changed shape once (8 hex digits → 16),
 * and an untagged store cannot tell "a digest in a format I no longer
 * produce" from "a digest of a different person". Both read as a SWITCH, so a
 * widening would mint a fresh anonymous id and rotate the session for every
 * returning user on their next `identify()` — once, silently, with no error
 * and nothing in the data saying why guest counts moved.
 *
 * With the tag, an unrecognised (or absent) prefix reads as **no previous
 * identity**, which is the safe reading: the first identify on a device is
 * never a switch, so nothing is rotated and the next write re-tags the entry
 * in the current format. The cost of guessing wrong that way is one missed
 * switch on one device — the same exposure as before anything was persisted
 * at all — instead of a fleet-wide rotation.
 *
 * Byte-compatible with the Flutter SDK, which writes the identical
 * `<tag>:<digest>` string under the same key (see
 * `LastIdentifiedStore` in `sdks/flutter/lib/src/context/last_identified_store.dart`).
 * The tag is on the STORED VALUE, not on `hashIdentity` itself, so the
 * cross-SDK digest golden is unaffected by it.
 */
export const LAST_IDENTIFIED_FORMAT = 'v1';

/** Wrap a digest in the current storage format. */
function encodeLastIdentified(digest: string): string {
  return `${LAST_IDENTIFIED_FORMAT}:${digest}`;
}

/**
 * Unwrap a stored value, or `null` if it is not in a format this build
 * understands — which callers must treat as "nobody has identified here yet".
 */
function decodeLastIdentified(raw: string | null): string | null {
  if (raw === null) return null;
  const sep = raw.indexOf(':');
  // No separator at all is the pre-tag format; a different tag is a newer (or
  // older) build's. Both are unreadable here, and both fail to "no previous
  // identity" rather than to a false switch.
  if (sep < 0 || raw.slice(0, sep) !== LAST_IDENTIFIED_FORMAT) return null;
  const digest = raw.slice(sep + 1);
  return digest === '' ? null : digest;
}

/** One 32-bit FNV-1a pass, returned as an 8-hex-digit string. */
function fnv1a32(s: string): string {
  let h = 0x811c9dc5; // FNV-1a 32-bit offset basis
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 0x01000193); // FNV-1a 32-bit prime
  }
  return (h >>> 0).toString(16).padStart(8, '0');
}

/**
 * Two decorrelated 32-bit FNV-1a passes concatenated into a 16-hex-digit
 * (64-bit) digest.
 *
 * `last_identified` only ever needs an EQUALITY check ("is this the same
 * person as last time"), comparing exactly one previous digest against one
 * current digest — never a set lookup — so the birthday bound doesn't apply
 * here: a collision is ~2^-64 for one consecutive-login pair, and it fails
 * OPEN to the pre-task behaviour for that one pair (a missed switch), never
 * worse.
 *
 * The second pass is decorrelated by INPUT — a `'\x01'`-prefixed copy — not
 * by a different offset basis/prime: two FNV-1a passes over identical bytes
 * with the same constants are structurally correlated, so changing only the
 * basis buys far less independence than it looks like it does.
 *
 * NOT a security boundary, and widening this does not change that: this is
 * an UNKEYED hash over what can be a low-entropy space (an email address),
 * so it is a confirmation oracle, not a secret — anyone with local read
 * access and a guess can verify it instantly by hashing the guess and
 * comparing. It exists only so `sauron.last_identified` isn't a second
 * plaintext copy of the app's user id, not to keep that id confidential.
 * `crypto.subtle` would be equally an oracle here (still unkeyed) and is
 * async, so it wouldn't buy anything worth the API becoming asynchronous.
 */
export function hashIdentity(id: string): string {
  return fnv1a32(id) + fnv1a32('\x01' + id);
}

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
let anonymousId: string | null = null;
let lastIdentified: string | null = null;

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

/**
 * Mint and persist a fresh session id.
 *
 * `reset()` calls this so a `sessions` row never spans two people. The server's
 * `bump_session` sets `distinct_id = COALESCE(EXCLUDED.distinct_id, …)`, i.e.
 * last-write-wins, so without rotation one session row records only whichever
 * of two consecutive users wrote last.
 */
export function rotateSessionId(): string {
  sessionId = null;
  const storage = webStorage('sessionStorage');
  if (storage) {
    try {
      storage.removeItem(SESSION_ID_KEY);
    } catch {
      /* best effort */
    }
  }
  return getSessionId();
}

/** The stable anonymous id (persisted in localStorage; per-process fallback). */
export function getAnonymousId(): string {
  if (anonymousId) return anonymousId;
  const storage = webStorage('localStorage');
  if (storage) {
    try {
      const existing = storage.getItem(ANON_ID_KEY);
      if (existing) {
        anonymousId = existing;
        return anonymousId;
      }
    } catch {
      /* fall through and generate */
    }
  }
  const fresh = `anon_${uuidv4()}`;
  if (storage) {
    try {
      storage.setItem(ANON_ID_KEY, fresh);
    } catch {
      /* best effort — degrade to the in-memory value */
    }
  }
  anonymousId = fresh;
  return anonymousId;
}

/**
 * Mint and persist a fresh anonymous id.
 *
 * MUST be reachable from application code. A persisted anon id plus
 * `process_identify`'s `identities(app_id, alias_id, distinct_id)` insert means
 * one `identify()` permanently binds this browser profile to a named user
 * server-side — so on a kiosk or a shared machine, person B's anonymous
 * activity would be aliased to person A's account, forever, with no escape
 * hatch.
 */
export function resetAnonymousId(): string {
  anonymousId = null;
  const storage = webStorage('localStorage');
  if (storage) {
    try {
      storage.removeItem(ANON_ID_KEY);
    } catch {
      /* best effort */
    }
  }
  return getAnonymousId();
}

/**
 * The digest of the last user who identified on this device, or null.
 *
 * Falls back to the in-memory value not only when storage is absent/throws,
 * but also when storage IS present and simply has nothing under the key —
 * that also covers the case where `setLastIdentified`'s own write silently
 * failed (e.g. quota), so the value it set in memory is still the only place
 * this identify() is recorded at all.
 *
 * Returns the bare DIGEST: the `<tag>:` prefix is a storage concern and never
 * reaches the comparison in `SauronClient.prepareIdentify`. A value whose tag
 * this build does not recognise comes back `null` — see
 * `LAST_IDENTIFIED_FORMAT`.
 */
export function getLastIdentified(): string | null {
  const storage = webStorage('localStorage');
  if (!storage) return decodeLastIdentified(lastIdentified);
  try {
    const stored = storage.getItem(LAST_IDENTIFIED_KEY);
    return decodeLastIdentified(stored ?? lastIdentified);
  } catch {
    return decodeLastIdentified(lastIdentified);
  }
}

/**
 * Record the digest of the user who just identified.
 *
 * The in-memory copy holds the ENCODED value, not the bare digest, so the
 * storage-absent path and the storage-present path decode through exactly the
 * same function — a memory fallback that skipped the tag would be a second
 * format, and the two would drift.
 */
export function setLastIdentified(id: string): void {
  const encoded = encodeLastIdentified(id);
  lastIdentified = encoded;
  const storage = webStorage('localStorage');
  if (storage) {
    try {
      storage.setItem(LAST_IDENTIFIED_KEY, encoded);
    } catch {
      /* best effort — the in-memory value above still applies this session */
    }
  }
}

/** Forget the last identified digest (called by `reset()`). */
export function clearLastIdentified(): void {
  lastIdentified = null;
  const storage = webStorage('localStorage');
  if (storage) {
    try {
      storage.removeItem(LAST_IDENTIFIED_KEY);
    } catch {
      /* best effort */
    }
  }
}

/** Drop the in-memory memoization (used by tests and teardown). */
export function resetIdentity(): void {
  deviceId = null;
  sessionId = null;
  anonymousId = null;
  lastIdentified = null;
}
