/**
 * Which value each sortable column of the Performance "Operations" table
 * orders by.
 *
 * Lives beside the page rather than inside it for the reason `monitor-sort.ts`
 * gives: vitest runs on the node environment and cannot import a `.svelte`
 * file, so an accessor map written inline in the component is untestable — and
 * these accessors are exactly where the interesting mistakes are. Six of this
 * table's eight columns render a number as text — four latencies through
 * `LatencyBadge` ("1.2 s" beside "980 ms"), Throughput through
 * `toLocaleString` ("1,000,052" sorts before "48"), and Error rate through
 * `formatPercent` — so every one of them is a chance to order the column by
 * its label and look right while being wrong.
 *
 * `SortableTh`'s `key` prop is a plain `string`, so a header whose key is
 * missing or misspelled here is NOT a compile error and NOT a dead header — it
 * falls through to the default column while the caret sits on the header that
 * was clicked. `operationAccessor` warns in dev to make that audible; the map
 * itself is a convention, not a guard.
 */
import type { PerfSummaryRow } from './index';
import type { SortState } from './sort';
import type { SortValue } from './sort-rows';

const ACCESSORS: Record<string, (r: PerfSummaryRow) => SortValue> = {
  name: (r) => r.name,
  /**
   * The raw `op`, not the page's `opLabel`.
   *
   * This is the one place this slice does NOT follow `alert-sort.ts`'s "sort
   * by the label the cell renders" rule, and the reason is that here the label
   * cannot reorder anything: `opLabel` only replaces an underscore with a
   * space, so it maps each op to a string with the same relative order as
   * every other op. The test asserts that equivalence over the exact five
   * values `OPS` offers rather than taking it on trust. Injecting the label
   * would buy nothing and add a parameter to every call site.
   */
  op: (r) => r.op,
  // The transaction count. The column is headed Throughput and the cell shows
  // `formatNumber(count)`; `count` is the number behind it.
  throughput: (r) => r.count,
  // Raw milliseconds for all four latencies, never `LatencyBadge`'s label.
  p50: (r) => r.p50,
  p95: (r) => r.p95,
  p99: (r) => r.p99,
  avg: (r) => r.avg,
  // The 0..1 ratio, not `formatPercent`'s "1.2%" — text ordering puts 10%
  // ("10.0%") below 2% ("2.0%") once the values cross a digit boundary.
  error_rate: (r) => r.error_rate,
};

/**
 * The order the table is in before anyone clicks a header.
 *
 * This DESCRIBES the endpoint: `performance_summary` (backend `repo.rs`) ends
 * `GROUP BY name, op ORDER BY count DESC LIMIT 100`, and `count` is exactly
 * what the Throughput column shows — so the table opens in the order it does
 * today and the caret names the column that produced it.
 *
 * Exported so the page seeds from the same constant the unknown-key fallback
 * uses; seeding one column and recovering to another would make the table's
 * initial order and its recovery order disagree, silently.
 */
export const OPERATION_DEFAULT_SORT: SortState = { key: 'throughput', dir: 'desc' };

export function operationAccessor(key: string): (r: PerfSummaryRow) => SortValue {
  const accessor = ACCESSORS[key];
  if (accessor) return accessor;
  if (import.meta.env.DEV) {
    console.warn(
      `[performance-sort] no accessor for column "${key}" — sorting by ` +
        `"${OPERATION_DEFAULT_SORT.key}" instead. Add it to ACCESSORS in ` +
        `performance-sort.ts.`,
    );
  }
  return ACCESSORS[OPERATION_DEFAULT_SORT.key];
}
