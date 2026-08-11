/**
 * Which value each sortable column of the two Monitor detail tables orders by.
 *
 * Lives beside the page rather than inside it for the reason `monitor-sort.ts`
 * gives: vitest runs on the node environment and cannot import a `.svelte`
 * file, so an accessor map written inline in the component is untestable — and
 * these accessors are exactly where the interesting mistakes are. Both tables
 * render a magnitude as text: Latency goes through `LatencyBadge` ("1.2 s" vs
 * "980 ms") and Duration through `formatDuration` ("1h 00m" vs "10m 0s"), and
 * ordering either as text reads as a working sort while being wrong.
 *
 * TWO independent sets, never one shared map: `MonitorDetail.svelte` renders a
 * checks table and an incidents table, and a shared sort would reorder one
 * under a header clicked on the other.
 *
 * Be exact about how much a map buys, because it is less than it looks:
 * `SortableTh`'s `key` prop is a plain `string`, so a header whose key is
 * missing or misspelled here is NOT a compile error and NOT a dead header. It
 * falls through to the table's default column, so the table quietly re-sorts by
 * something else while the caret sits on the header that was clicked — a
 * confident wrong answer, not an obvious breakage. `pick` warns in dev to make
 * that case say something; the map itself is a convention, not a guard.
 */
import { durationBetween } from '../utils/format';
import type { MonitorCheck, MonitorIncident } from './index';
import type { SortState } from './sort';
import type { SortValue } from './sort-rows';

/** See `device-detail-sort.ts` — private per module on purpose. */
function pick<T>(
  table: string,
  map: Record<string, (row: T) => SortValue>,
  key: string,
  fallback: SortState,
): (row: T) => SortValue {
  const accessor = map[key];
  if (accessor) return accessor;
  if (import.meta.env.DEV) {
    console.warn(
      `[monitor-detail-sort] no accessor for ${table} column "${key}" — sorting by ` +
        `"${fallback.key}" instead. Add it to monitor-detail-sort.ts.`,
    );
  }
  return map[fallback.key];
}

// ---------------------------------------------------------------------------
// Recent checks
// ---------------------------------------------------------------------------

const CHECK_ACCESSORS: Record<string, (c: MonitorCheck) => SortValue> = {
  // The raw ISO instant. `sortRows` compares ISO-8601 as bytes, which is
  // chronological; the cell's `formatDateTime` is locale text and is not.
  time: (c) => c.checked_at,
  // The boolean, not the "Up"/"Down" word the cell renders. `sortRows` puts
  // `false` first ascending, so ascending is failures-first — which is what
  // someone clicking Result on an uptime log is looking for. A two-valued
  // column, so Task 5's `rankOf` has nothing to add here, but it is a status
  // column and is flagged for that pass all the same.
  result: (c) => c.up,
  // Nullable and NOT coerced to 0: a TCP check has no status code at all, and
  // 0 would rank it below every real code instead of leaving it out of the
  // ordering. `sortRows` puts an absent value last in both directions.
  code: (c) => c.status_code,
  // Milliseconds, never `LatencyBadge`'s label. Null for a check that never
  // connected, and left null for the same reason as `code`.
  latency: (c) => c.response_time_ms,
};

/**
 * The order the checks table is in before anyone clicks a header.
 *
 * This DESCRIBES what the page already does: `recentChecks` is the 100 most
 * recent checks in reverse-chronological order, so `{ time, desc }` reproduces
 * the table exactly as it opens today, with the caret naming the column that
 * produced it.
 *
 * The page keeps its own chronological `checksAsc` — the availability strip
 * above the table reads left-to-right oldest-to-newest and the "100 most
 * recent" selection is taken off the end of it. That selection is the table's
 * definition, like a `LIMIT`, and is not what this sort replaces: this orders
 * the hundred rows, it does not choose them.
 *
 * Exported so the page seeds from the same constant the unknown-key fallback
 * uses; seeding one column and recovering to another would make the table's
 * initial order and its recovery order disagree, silently.
 */
export const MONITOR_CHECK_DEFAULT_SORT: SortState = { key: 'time', dir: 'desc' };

export function monitorCheckAccessor(key: string): (c: MonitorCheck) => SortValue {
  return pick('checks', CHECK_ACCESSORS, key, MONITOR_CHECK_DEFAULT_SORT);
}

// ---------------------------------------------------------------------------
// Incidents
// ---------------------------------------------------------------------------

const INCIDENT_ACCESSORS: Record<string, (i: MonitorIncident) => SortValue> = {
  started: (i) => i.started_at,
  // Null while the incident is open — the cell renders "Ongoing" there rather
  // than a timestamp, so there is no instant to order it by and `sortRows`
  // leaves it last in both directions. Substituting `now` would rank an open
  // incident as the most recently resolved one, which is the opposite of true.
  resolved: (i) => i.resolved_at,
  // Milliseconds between the two instants, and null for an open incident for
  // the same reason — its cell shows an em dash, not a running clock.
  duration: (i) => (i.resolved_at ? durationBetween(i.started_at, i.resolved_at) : null),
  cause: (i) => i.cause,
};

/**
 * The order the incidents table is in before anyone clicks a header.
 *
 * This DESCRIBES the endpoint: `list_monitor_incidents` (backend `repo.rs`)
 * orders by `started_at DESC`, which is exactly `{ started, desc }`.
 */
export const MONITOR_INCIDENT_DEFAULT_SORT: SortState = { key: 'started', dir: 'desc' };

export function monitorIncidentAccessor(key: string): (i: MonitorIncident) => SortValue {
  return pick('incidents', INCIDENT_ACCESSORS, key, MONITOR_INCIDENT_DEFAULT_SORT);
}
