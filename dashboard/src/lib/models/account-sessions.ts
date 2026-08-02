import type { AccountSession } from './index';

/** Longest raw user-agent string rendered before it is elided. */
const MAX_RAW_UA = 60;

function clean(v: string | null): string | null {
  const t = v?.trim();
  return t ? t : null;
}

function isLive(s: AccountSession): boolean {
  return s.revoked_at === null;
}

/**
 * How to *phrase* a device.
 *
 * The client half of a deliberate split: the server answers the data question
 * (what does this UA string mean, using the same woothee vocabulary the ingest
 * pipeline uses), and this answers the copy question.
 */
export function describeSession(s: AccountSession): string {
  const browser = clean(s.browser);
  const os = clean(s.os);
  if (browser && os) return `${browser} on ${os}`;
  if (browser) return browser;
  if (os) return os;
  const raw = clean(s.user_agent);
  if (raw) return raw.length > MAX_RAW_UA ? `${raw.slice(0, MAX_RAW_UA)}…` : raw;
  return 'Unknown device';
}

/**
 * Current session first, then most recently used.
 *
 * Returns a new array: the caller holds this list in `$state`, and sorting in
 * place would mutate a proxied array during a derivation.
 */
export function sortSessions(list: AccountSession[]): AccountSession[] {
  return [...list].sort((a, b) => {
    if (a.current !== b.current) return a.current ? -1 : 1;
    return Date.parse(b.last_used_at) - Date.parse(a.last_used_at);
  });
}

/** Live sessions that are not the caller's own — what "Sign out other devices" reaches. */
export function otherSessionCount(list: AccountSession[]): number {
  return list.filter((s) => isLive(s) && !s.current).length;
}

/**
 * Does the caller's own access token name a session in this list?
 *
 * False means a legacy token minted before the session feature shipped: the
 * server refuses `revoke-others` for it (it has nothing to spare), so the UI
 * disables both revoke affordances rather than offering an action that 400s.
 */
export function hasCurrentSession(list: AccountSession[]): boolean {
  return list.some((s) => isLive(s) && s.current);
}

/**
 * Do all live rows report one address?
 *
 * On both shipped topologies they will: `API_TRUST_FORWARDED_HEADERS` defaults
 * to false in `config.rs`, in `packaging/rpm/config/api.env` and in
 * docker-compose, and the shipped nginx sits in front — so every session records
 * the proxy. Detecting it client-side turns a column that looks broken into a
 * legible configuration message, with no new API surface.
 *
 * A single row is not evidence of anything, and a null address is not an
 * address, so both answer false.
 */
export function allSameIp(list: AccountSession[]): boolean {
  const ips = list.filter(isLive).map((s) => s.ip);
  if (ips.length < 2) return false;
  if (ips.some((ip) => ip === null)) return false;
  return ips.every((ip) => ip === ips[0]);
}
