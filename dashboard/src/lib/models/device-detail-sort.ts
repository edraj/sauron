/**
 * Which value each sortable column of the two Device detail tables orders by.
 *
 * Lives beside the page rather than inside it for the reason `monitor-sort.ts`
 * gives: vitest runs on the node environment and cannot import a `.svelte`
 * file, so an accessor map written inline in the component is untestable — and
 * these accessors are exactly where the interesting mistakes are. The Duration
 * column is this page's sharpest example: its cell renders `formatDuration`,
 * and ordering that text ascending gives "1h 00m", "10m 0s", "30s" — the
 * collator reads the leading digits as numbers, so an hour sorts before ten
 * minutes and the column looks sorted while being backwards.
 *
 * TWO independent sets, never one shared map: `DeviceDetail.svelte` renders a
 * sessions table and a performance table, and a shared sort would reorder one
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
import type { PerfSummaryRow, Session } from './index';
import type { SortState } from './sort';
import type { SortValue } from './sort-rows';

/**
 * Look `key` up, or fall back to `fallback`'s column and say so in dev.
 *
 * PRIVATE to this module and duplicated in the other `*-sort.ts` files rather
 * than shared: Task 2 deliberately rejected a cross-module `accessorFor`
 * helper, on the grounds that a table copying a fallback line is clearer than
 * an indirection every table has to learn. What is not worth copying is the
 * same six-line dev warning twice inside one file, which is all this collapses.
 * Stripped from production builds — `import.meta.env.DEV` is replaced with a
 * literal at build time.
 */
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
      `[device-detail-sort] no accessor for ${table} column "${key}" — sorting by ` +
        `"${fallback.key}" instead. Add it to device-detail-sort.ts.`,
    );
  }
  return map[fallback.key];
}

// ---------------------------------------------------------------------------
// Recent sessions
// ---------------------------------------------------------------------------

const SESSION_ACCESSORS: Record<string, (s: Session) => SortValue> = {
  session: (s) => s.session_id,
  // The raw ISO instant. `sortRows` compares ISO-8601 as bytes, which is
  // chronological; `TimeValue`'s rendering is relative ("3 hours ago") or
  // locale text, and neither orders.
  started: (s) => s.started_at,
  // MILLISECONDS, never `formatDuration`'s label — see the header comment.
  // Computed from the same two fields the cell's own `sessionDuration` uses,
  // through the same helper, so there is one definition of "how long was this
  // session" rather than two that can drift.
  duration: (s) => durationBetween(s.started_at, s.last_event_at),
  events: (s) => s.events_count,
  errors: (s) => s.errors_count,
};

/**
 * The order the sessions table is in before anyone clicks a header.
 *
 * This one REPLACES the endpoint's ordering rather than describing it.
 * `detail` (backend `routes/devices.rs`) pins `DEVICE_SESSION_SORT` =
 * `last_event_at DESC, id ASC`, and `last_event_at` is not a column this table
 * displays — the Started cell shows `started_at`, and Duration shows the span
 * between the two, not either endpoint. Seeding a column nobody can see would
 * put the caret nowhere, and `sort.ts` has no "unsorted" state to fall back
 * to, so the table now opens newest-STARTED first. In practice the two orders
 * agree for most devices and differ only where a long session began before a
 * short later one; the panel is still "the 50 most recent sessions", which is
 * the server's `LIMIT` and is not affected by the client-side order.
 *
 * Exported so the page seeds from the same constant the unknown-key fallback
 * uses; seeding one column and recovering to another would make the table's
 * initial order and its recovery order disagree, silently.
 */
export const DEVICE_SESSION_DEFAULT_SORT: SortState = { key: 'started', dir: 'desc' };

export function deviceSessionAccessor(key: string): (s: Session) => SortValue {
  return pick('sessions', SESSION_ACCESSORS, key, DEVICE_SESSION_DEFAULT_SORT);
}

// ---------------------------------------------------------------------------
// Performance profile
// ---------------------------------------------------------------------------

const DEVICE_PERF_ACCESSORS: Record<string, (p: PerfSummaryRow) => SortValue> = {
  name: (p) => p.name,
  op: (p) => p.op,
  // Raw milliseconds, not `LatencyBadge`'s rendering. The badge shows "1.2 s"
  // for 1200 and "980 ms" for 980; ordering that text puts the second first.
  p95: (p) => p.p95,
  count: (p) => p.count,
};

/**
 * The order the performance table is in before anyone clicks a header.
 *
 * This one DESCRIBES the endpoint: `performance_summary` (backend `repo.rs`)
 * ends `GROUP BY name, op ORDER BY count DESC LIMIT 100`, which is exactly
 * `{ count, desc }` — so the table opens in the order it does today and the
 * caret names the column that produced it.
 */
export const DEVICE_PERF_DEFAULT_SORT: SortState = { key: 'count', dir: 'desc' };

export function devicePerfAccessor(key: string): (p: PerfSummaryRow) => SortValue {
  return pick('perf', DEVICE_PERF_ACCESSORS, key, DEVICE_PERF_DEFAULT_SORT);
}
