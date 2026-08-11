/**
 * Which value each sortable column of the Privacy inspector's three tables
 * orders by: findings ("paths"), scans, and the mask audit trail.
 *
 * Named `pii-inspector-sort` rather than `inspector-sort` on purpose: a
 * concurrent branch owns `models/inspector*`, and a new file matching that
 * prefix would collide with work this task must not touch. The page it serves
 * is `pages/Inspector.svelte`.
 *
 * Lives outside the component for the reason `monitor-sort.ts` gives: vitest
 * runs on the node environment and cannot import a `.svelte` file, so an
 * inline accessor map is untestable — and these accessors are where the
 * interesting mistakes are. Two on this page in particular: Matches renders
 * `formatMatchCount`, which produces "at least 1,234", and Rows scanned renders
 * `toLocaleString`, which produces "1,000" — ordering either as text is a
 * confident wrong answer.
 *
 * THREE independent sets, never one shared map: a shared sort would make
 * ordering the scans table reorder the audit table under a header nobody
 * clicked.
 *
 * `SortableTh`'s `key` prop is a plain `string`, so a header whose key is
 * missing or misspelled here is NOT a compile error and NOT a dead header — it
 * falls through to the table's default column while the caret sits on the
 * header that was clicked. `pick` warns in dev; the maps are a convention, not
 * a guard.
 */
import type { FindingView } from './inspector-findings';
import type { InspectorMaskAction, InspectorScan } from './index';
import type { SortState } from './sort';
import { rankOf, type SortValue } from './sort-rows';

/** See `alert-sort.ts` for why this is private per module rather than shared. */
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
      `[pii-inspector-sort] no accessor for ${table} column "${key}" — sorting by ` +
        `"${fallback.key}" instead. Add it to pii-inspector-sort.ts.`,
    );
  }
  return map[fallback.key];
}

/**
 * An email the server records as an empty string is an ABSENT one, not the
 * alphabetically smallest one. Mapping it to null hands it to `sortRows`'s
 * nulls-last rule, which is where it belongs — otherwise every unattributed
 * row leads an ascending sort of Who.
 *
 * Note this is the opposite direction from the "never `?? 0`" rule and for the
 * same reason: both say the sort must not invent a magnitude for a value that
 * has none.
 */
const emailOrNull = (email: string): SortValue => email || null;

// ---------------------------------------------------------------------------
// Findings (the "paths" table — one per source table.column group)
// ---------------------------------------------------------------------------

const FINDING_ACCESSORS: Record<string, (f: FindingView) => SortValue> = {
  // The raw path. The cell substitutes "(whole value)" for an empty path;
  // both sort to the front of an ascending list, so nothing is gained by
  // ordering the substitute text and the raw value is the honest key.
  path: (f) => f.key_path,
  type: (f) => f.value_type,
  // The count, never `formatMatchCount(...)` — that renders "at least 1,234"
  // for a truncated count, which as text sorts under "a" alongside every other
  // inexact row regardless of size.
  matches: (f) => f.match_count,
  last_seen: (f) => f.last_seen_at,
};

/**
 * This one DESCRIBES the existing order rather than replacing it:
 * `groupFindings` already sorts each group by `match_count` descending (with a
 * `key_path` tiebreak, which the seed does not reproduce and which only shows
 * on exact ties).
 */
export const FINDING_DEFAULT_SORT: SortState = { key: 'matches', dir: 'desc' };

export function findingAccessor(key: string): (f: FindingView) => SortValue {
  return pick('findings', FINDING_ACCESSORS, key, FINDING_DEFAULT_SORT);
}

// ---------------------------------------------------------------------------
// Scans
// ---------------------------------------------------------------------------

/**
 * Scan outcome, LEAST worth looking at first — the direction `rankOf`
 * documents, so `desc` leads with the scans that broke.
 *
 * Not a severity but the same axis every other ranked status column on these
 * pages uses: how much does this row want a human. `succeeded` wants nothing;
 * `cancelled` was stopped on purpose and is explained by the person who stopped
 * it; `queued` and `running` are in flight, `running` being the one worth
 * watching; `failed` is the row this column gets clicked for.
 *
 * The in-flight pair is the judgement call and there is no strong reading of
 * it — what is load-bearing is that failures lead and clean finishes trail.
 * Alphabetically the five run `cancelled, failed, queued, running, succeeded`,
 * which leads with a deliberate stop and buries a failure in the middle.
 */
const SCAN_STATUS_ORDER: readonly InspectorScan['status'][] = [
  'succeeded',
  'cancelled',
  'queued',
  'running',
  'failed',
];
const scanStatusRank = rankOf(SCAN_STATUS_ORDER);

const SCAN_ACCESSORS: Record<string, (s: InspectorScan) => SortValue> = {
  // Null for a queued scan, which has not started. NOT filled in from
  // `created_at`: the cell renders an em dash there, and a row displaying
  // nothing must not be ranked among rows displaying instants.
  started: (s) => s.started_at,
  finished: (s) => s.finished_at,
  // The RANK, not the word — see `SCAN_STATUS_ORDER` above.
  status: (s) => scanStatusRank(s.status),
  // Counts, never their `toLocaleString()` text — "1,000" sorts before "999".
  rows_scanned: (s) => s.rows_scanned,
  findings: (s) => s.findings_count,
  // TEXT, deliberately, and the one status-shaped column on these pages that a
  // rank would not change: `coverage` is `'full' | 'partial'`, and `full <
  // partial` alphabetically is already `full < partial` by completeness. A
  // ladder here would restate the collator's answer in a second place that can
  // drift from it, and — worse — no test could tell the two apart, so it would
  // ship as a rank column with a rank test that cannot fail. Revisit if a third
  // coverage value ever appears; then the two orders can genuinely disagree.
  coverage: (s) => s.coverage,
};

/**
 * A partial REPLACEMENT: `list_scans_for_policy` returns `created_at DESC`, and
 * `created_at` — though on the wire — is not a column of this table, so the
 * seed uses the Started column it does show. The two agree for every scan that
 * has started; a still-queued scan has a null `started_at` and therefore sorts
 * LAST rather than first for the few seconds before it starts.
 *
 * Not worked around: the alternative is to rank a row by a timestamp it does
 * not display. The page's own "which scan do I read findings from" logic is
 * unaffected — it indexes the raw `scans` array, not this ordering.
 */
export const SCAN_DEFAULT_SORT: SortState = { key: 'started', dir: 'desc' };

export function scanAccessor(key: string): (s: InspectorScan) => SortValue {
  return pick('scans', SCAN_ACCESSORS, key, SCAN_DEFAULT_SORT);
}

// ---------------------------------------------------------------------------
// Mask audit trail
// ---------------------------------------------------------------------------

/**
 * Mask-action state, LEAST worth looking at first — same axis and direction as
 * `SCAN_STATUS_ORDER`, so `desc` leads with the actions that broke.
 *
 * Eight values and a branching lifecycle, so the ladder is stated in full: the
 * two clean terminal states (`done`, `cancelled`) want nothing; `preview` and
 * `previewed` are a draft, `previewed` slightly higher because it is waiting on
 * a person; `pending`, `running` and `cancelling` are in flight and touching
 * real data; `failed` may have masked some rows and not others, which is the
 * one that has to lead.
 *
 * The placements inside each of those groups are judgement calls and could move
 * a slot without changing what the column is for. Alphabetically the eight run
 * `cancelled, cancelling, done, failed, pending, preview, previewed, running` —
 * an order in which nothing is next to anything it is related to.
 */
const MASK_STATUS_ORDER: readonly InspectorMaskAction['status'][] = [
  'done',
  'cancelled',
  'preview',
  'previewed',
  'pending',
  'running',
  'cancelling',
  'failed',
];
const maskStatusRank = rankOf(MASK_STATUS_ORDER);

const MASK_ACCESSORS: Record<string, (a: InspectorMaskAction) => SortValue> = {
  when: (a) => a.requested_at,
  who: (a) => emailOrNull(a.requested_by_email),
  targets: (a) => a.targets.length,
  // The RANK, not the word — see `MASK_STATUS_ORDER` above.
  status: (a) => maskStatusRank(a.status),
  rows_masked: (a) => a.rows_masked,
  cold_skipped: (a) => a.cold_rows_skipped,
  cancelled_by: (a) => emailOrNull(a.cancelled_by_email),
};

/**
 * DESCRIBES the endpoint: `list_mask_actions_for_app` returns
 * `requested_at DESC`, which is exactly `{ when, desc }`, so the table opens in
 * the order it does today with the caret on the column that produced it.
 */
export const MASK_DEFAULT_SORT: SortState = { key: 'when', dir: 'desc' };

export function maskActionAccessor(key: string): (a: InspectorMaskAction) => SortValue {
  return pick('audit', MASK_ACCESSORS, key, MASK_DEFAULT_SORT);
}
