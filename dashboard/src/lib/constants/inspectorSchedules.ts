// Single source of truth for the schedule vocabulary the Policy tab renders.
// Mirrors the backend's `NEXT_RUN_SQL` in
// backend/crates/sauron-db/src/repo.rs, which computes the due instant with
// `(schedule_days >> EXTRACT(DOW FROM ts)) & 1` — so BIT 0 IS SUNDAY, exactly
// as Postgres numbers DOW. Keep the two in sync.

import type { MessageKey } from '../i18n';

/**
 * Short weekday names as catalog keys, Sunday first (index = DOW = bit).
 * Translate with `t()` at render time; the index, not the name, is what maps
 * to the `schedule_days` bit.
 */
export const WEEKDAY_KEYS: MessageKey[] = [
  'inspector.weekday.sun',
  'inspector.weekday.mon',
  'inspector.weekday.tue',
  'inspector.weekday.wed',
  'inspector.weekday.thu',
  'inspector.weekday.fri',
  'inspector.weekday.sat',
];

/**
 * A short list of IANA zones for the picker. Any zone Postgres accepts is
 * valid — the API validates with `SELECT now() AT TIME ZONE $1` and answers
 * 400 — so this is a convenience list, not a whitelist.
 */
export const COMMON_TIMEZONES: string[] = [
  'UTC',
  'Europe/London',
  'Europe/Paris',
  'Europe/Berlin',
  'Africa/Algiers',
  'Africa/Cairo',
  'Asia/Dubai',
  'Asia/Kolkata',
  'Asia/Singapore',
  'Asia/Tokyo',
  'Australia/Sydney',
  'America/New_York',
  'America/Chicago',
  'America/Denver',
  'America/Los_Angeles',
  'America/Sao_Paulo',
];

/**
 * Local times the UI warns about. On spring-forward a 02:30 schedule resolves
 * to a valid instant (effectively 03:30 local); on fall-back it resolves to
 * the first occurrence, so it runs once, not twice. Never zero runs, never
 * double runs — but an operator picking 02:30 should know that.
 */
export const DST_RISK_HOURS: number[] = [0, 1, 2, 3];
