// Single source of truth for the schedule vocabulary the Policy tab renders.
// Mirrors the backend's `NEXT_RUN_SQL` in
// backend/crates/sauron-db/src/repo.rs, which computes the due instant with
// `(schedule_days >> EXTRACT(DOW FROM ts)) & 1` — so BIT 0 IS SUNDAY, exactly
// as Postgres numbers DOW. Keep the two in sync.

export const WEEKDAYS: string[] = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'];

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
