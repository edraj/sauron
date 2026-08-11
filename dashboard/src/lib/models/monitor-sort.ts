/**
 * Which value each sortable column of the Monitors table orders by.
 *
 * Lives beside the page rather than inside it for the same reason
 * `timeline-row.ts` does: the dashboard's vitest runs on the node environment
 * and cannot import a `.svelte` file, so an accessor map written inline in the
 * component is untestable — and these accessors are exactly where the
 * interesting mistakes are. A column must sort by its MAGNITUDE, not by the
 * text its cell renders: `Checked` displays `toLocaleTimeString`, which drops
 * the date, and sorting by that string orders three days of history by time of
 * day while looking entirely correct.
 *
 * One map rather than a `switch` inside a comparator, so there is a single
 * place to add a column and a single place to look one up.
 *
 * Be exact about how much that buys, because it is less than it looks:
 * `SortableTh`'s `key` prop is a plain `string`, so a header whose key is
 * missing or misspelled here is NOT a compile error and NOT a dead header. It
 * falls through to the default column below, which means the table quietly
 * re-sorts by Name while the caret sits on the header the user clicked — a
 * confident wrong answer, not an obvious breakage. The `import.meta.env.DEV`
 * warning in `monitorAccessor` exists to make that case say something; the map
 * itself is a convention, not a guard.
 */
import type { MonitorListItem, MonitorStatus } from './index';
import type { SortState } from './sort';
import { rankOf, type SortValue } from './sort-rows';

/**
 * Monitor health, LEAST urgent first — the direction `rankOf` documents, so a
 * higher rank is worse and `desc` puts the outages at the top.
 *
 * The reading, in words: `up` needs nothing; `paused` is not being checked but
 * that was somebody's decision; `unknown` (the pill reads "Pending") has never
 * reported, which is not an outage but is not proof of health either; `down` is
 * the row this page exists to show.
 *
 * Alphabetically the middle two are the other way round — `paused` above
 * `unknown` — which is close enough to right to look deliberate and is not.
 *
 * Annotated `readonly MonitorStatus[]` so a misspelt or non-member state is a
 * compile error. That cannot catch an omitted one; `monitor-sort.test.ts` pins
 * the ladder against a `Record<MonitorStatus, number>` for that.
 */
const MONITOR_STATUS_ORDER: readonly MonitorStatus[] = ['up', 'paused', 'unknown', 'down'];
const statusRank = rankOf(MONITOR_STATUS_ORDER);

const ACCESSORS: Record<string, (m: MonitorListItem) => SortValue> = {
  name: (m) => m.name,
  target: (m) => m.target,
  // The RANK, not the word. As text `down < paused < unknown < up`, so the
  // column would sort by spelling while looking like it sorted by health.
  // A state this build does not know ranks null and sorts last both ways.
  status: (m) => statusRank(m.status),
  // Nulls are passed through, NOT coerced to 0: `sortRows` puts an absent
  // value last in both directions, whereas 0 would rank a monitor that has
  // never reported as the least available one.
  uptime: (m) => m.uptime_24h,
  latency: (m) => m.last_response_time_ms,
  // The raw ISO instant. `sortRows` compares ISO-8601 as bytes, which is
  // chronological; the cell's formatted time is not.
  checked: (m) => m.last_checked_at,
};

/**
 * The order the table is in before anyone clicks a header.
 *
 * Unlike `ScreensList`'s seed, this does NOT describe what the endpoint already
 * returns: `list_monitors_for_project` (backend `repo.rs`) orders by
 * `m.created_at ASC`, and `created_at` is not a field on `MonitorListItem`, so
 * creation order cannot be reproduced in the browser. This seed therefore
 * CHANGES the table's initial order to name A-Z. Deliberate and forced: `sort.ts`
 * has no "unsorted" state to fall back to, and putting `created_at` on the wire
 * would be a backend change.
 *
 * Exported so the page seeds its sort state from the same constant the unknown-
 * key fallback below uses. Seeding one column and falling back to another would
 * mean the table's initial order and its recovery order disagree.
 */
export const MONITOR_DEFAULT_SORT: SortState = { key: 'name', dir: 'asc' };

/**
 * The accessor for `key`, or the default column's if the key is unknown.
 *
 * The fallback stays — a mistyped key must not throw in front of a user — but
 * it is exactly the silent-wrong-answer case described at the top of this file,
 * so in dev it says so. Stripped from production builds: `import.meta.env.DEV`
 * is replaced with a literal at build time.
 */
export function monitorAccessor(key: string): (m: MonitorListItem) => SortValue {
  const accessor = ACCESSORS[key];
  if (accessor) return accessor;
  if (import.meta.env.DEV) {
    console.warn(
      `[monitor-sort] no accessor for column "${key}" — sorting by ` +
        `"${MONITOR_DEFAULT_SORT.key}" instead. Add it to ACCESSORS in monitor-sort.ts.`,
    );
  }
  return ACCESSORS[MONITOR_DEFAULT_SORT.key];
}
