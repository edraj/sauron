/**
 * Which value each sortable column of the two Storage tables orders by.
 *
 * Lives beside the page rather than inside it for the reason `monitor-sort.ts`
 * gives: vitest runs on the node environment and cannot import a `.svelte`
 * file, so an accessor map written inline in the component is untestable — and
 * these accessors are exactly where the interesting mistakes are. This page is
 * the worst case for it: five of its ten columns render bytes through
 * `fmtBytes`, and ordering "900 KB" against "1.2 GB" as text puts the
 * kilobytes on top — a wrong answer that looks exactly like a working sort,
 * on the one page whose entire job is telling an operator what is big.
 *
 * TWO independent sets, never one shared map: `Storage.svelte` renders a
 * database-tables table and a per-app table, and a shared sort would reorder
 * one under a header clicked on the other.
 *
 * Be exact about how much a map buys, because it is less than it looks:
 * `SortableTh`'s `key` prop is a plain `string`, so a header whose key is
 * missing or misspelled here is NOT a compile error and NOT a dead header. It
 * falls through to the table's default column, so the table quietly re-sorts by
 * something else while the caret sits on the header that was clicked. `pick`
 * warns in dev to make that case say something; the map is a convention, not a
 * guard.
 */
import type { AppStorage, TableSize } from '../api/admin';
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
      `[storage-sort] no accessor for ${table} column "${key}" — sorting by ` +
        `"${fallback.key}" instead. Add it to storage-sort.ts.`,
    );
  }
  return map[fallback.key];
}

// ---------------------------------------------------------------------------
// Database tables
// ---------------------------------------------------------------------------

const TABLE_ACCESSORS: Record<string, (t: TableSize) => SortValue> = {
  table: (t) => t.name,
  // BYTES, never `fmtBytes`. See the header comment — this is the column the
  // page exists for.
  size: (t) => t.total_bytes,
  // The row count, not `toLocaleString()`'s "1,000,052", which as text sorts
  // before "48".
  hot_rows: (t) => t.hot_rows,
};

/**
 * The order the database-tables table is in before anyone clicks a header.
 *
 * This REPLACES the server's ordering, because the server's ordering cannot be
 * reproduced here: `admin_storage.rs` builds the list by iterating the
 * `TIERED_TABLES` constant, so the rows arrive in a source-code order with no
 * column behind it. `sort.ts` has no "unsorted" state to fall back to, and of
 * the three columns available, biggest-first is what an operator opening a
 * storage report is looking for.
 *
 * Exported so the page seeds from the same constant the unknown-key fallback
 * uses; seeding one column and recovering to another would make the table's
 * initial order and its recovery order disagree, silently.
 */
export const STORAGE_TABLE_DEFAULT_SORT: SortState = { key: 'size', dir: 'desc' };

export function storageTableAccessor(key: string): (t: TableSize) => SortValue {
  return pick('database tables', TABLE_ACCESSORS, key, STORAGE_TABLE_DEFAULT_SORT);
}

// ---------------------------------------------------------------------------
// Storage by app
// ---------------------------------------------------------------------------

const APP_ACCESSORS: Record<string, (a: AppStorage) => SortValue> = {
  org: (a) => a.org_name,
  // `|| null`, mirroring the cell's `a.project_name || '—'` fallback exactly.
  // The field is an empty string — never absent — for a report cached by a
  // build that predates `project_name`, and those rows render an em dash; an
  // empty string would collate them to the top of an ascending list as if
  // their project were named "".
  project: (a) => a.project_name || null,
  app: (a) => a.app_name,
  hot_rows: (a) => a.hot_rows_total,
  cold_rows: (a) => a.cold_rows_total,
  // Bytes for both, never `fmtBytes`.
  cold_bytes: (a) => a.cold_bytes_total,
  hot_bytes: (a) => a.estimated_hot_bytes_total,
};

/**
 * The order the per-app table is in before anyone clicks a header.
 *
 * This DESCRIBES the endpoint, and does so exactly:
 * `list_apps_with_org_scoped` (backend `repo.rs`) returns
 * `ORDER BY o.name, p.name, a.name`, and `sortRows` is a STABLE sort — so
 * ordering the already-sorted response by `org` alone leaves the project and
 * app sub-orders untouched, and the table opens byte-for-byte as it does
 * today. That equivalence is the whole reason the seed is `org` rather than
 * one of the size columns, and it is asserted in the tests so a later change
 * to `sortRows`' stability cannot quietly break it.
 */
export const STORAGE_APP_DEFAULT_SORT: SortState = { key: 'org', dir: 'asc' };

export function storageAppAccessor(key: string): (a: AppStorage) => SortValue {
  return pick('storage by app', APP_ACCESSORS, key, STORAGE_APP_DEFAULT_SORT);
}
