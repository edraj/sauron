/**
 * Row-level presentation logic for the session `Timeline` — the parts that are
 * pure functions of a `TimelineItem`, kept out of the component so they can be
 * tested without a DOM (the dashboard's vitest runs on the node environment).
 */
import type { TimelineItem, Transaction } from './index';

/** The badge shown at the head of a timeline row. */
export type RowKind = 'navigation' | 'http' | 'event' | 'error' | 'transaction';

/** Which offset a row's trailing time reads against. */
export type TimeMode = 'session' | 'delta';

/**
 * The SDK's synthetic screen-view event. Both the JS and Flutter clients emit
 * exactly this name from `setScreen`, carrying `{ screen }`; no other event
 * name is auto-generated for navigation, so this single check is the whole
 * definition of "navigation related" — a heuristic over app-authored names
 * would mislabel real product events.
 */
export const SCREEN_EVENT = '$screen';

export function isNavigation(item: TimelineItem): boolean {
  return item.kind === 'event' && item.event.name === SCREEN_EVENT;
}

/**
 * An outbound HTTP call. `op` is a first-class field on the transaction — the
 * SDKs' fetch/XHR instrumentation sets it alongside `http_method`/`http_status`
 * /`url` — so this is a read, not a guess. Analytics events carry no equivalent
 * marker, which is why only transactions can answer this.
 */
export function isHttp(item: TimelineItem): boolean {
  return item.kind === 'transaction' && item.transaction.op === 'http';
}

export function rowKind(item: TimelineItem): RowKind {
  if (isNavigation(item)) return 'navigation';
  if (isHttp(item)) return 'http';
  return item.kind;
}

/**
 * The lane a row belongs to in the timeline's category filter — a coarser fold
 * than [`RowKind`], which stays as the row BADGE.
 *
 * The two differ in exactly two places, and both are deliberate. `http` folds
 * into `transaction` because an HTTP call IS a transaction with a known op, and
 * the op filter below is where it is reachable; leaving it as a fifth peer would
 * double-count it against a chip labelled "transaction". And `error` reads as
 * `issue` because that is what the row links to — a row whose badge says ERROR
 * and whose chip says "Issues" is the same row, named for its destination.
 */
export type RowCategory = 'navigation' | 'transaction' | 'event' | 'issue';

/** Every category, in the order the filter chips render. */
export const ROW_CATEGORIES: readonly RowCategory[] = [
  'navigation',
  'transaction',
  'event',
  'issue',
];

/**
 * Written as a total `Record` rather than a `switch` with a default: a sixth
 * `RowKind` added later fails to compile here instead of quietly falling into
 * some catch-all bucket, which would be a row that no chip can show and that
 * disappears the moment any filter is applied.
 */
const CATEGORY_OF_KIND: Record<RowKind, RowCategory> = {
  navigation: 'navigation',
  http: 'transaction',
  transaction: 'transaction',
  event: 'event',
  error: 'issue',
};

export function rowCategory(item: TimelineItem): RowCategory {
  return CATEGORY_OF_KIND[rowKind(item)];
}

/**
 * A transaction's op, normalized; `null` for a row that is not a transaction.
 *
 * The empty string is a real bucket, not an absence. `op` is non-null on the
 * wire but a hand-rolled `trackTransaction` can send it blank, and those rows
 * have to be selectable: dropping them here would leave them visible under the
 * transaction chip and impossible to isolate by op. The UI renders `''` as
 * "(none)" — the label is a presentation choice, so it does not live here.
 */
export function transactionOp(item: TimelineItem): string | null {
  if (item.kind !== 'transaction') return null;
  return (item.transaction.op ?? '').trim();
}

/**
 * What the timeline is narrowed to.
 *
 * An empty set means NO CONSTRAINT, never "match nothing". That is what makes
 * the page's initial state and its "All" button the same value, and it is why
 * no sequence of chip toggles can construct a filter that blanks the timeline
 * while every chip reads as off.
 *
 * `ops` describes transactions and only transactions — see [`filterTimeline`].
 */
export interface TimelineFilter {
  categories: ReadonlySet<RowCategory>;
  ops: ReadonlySet<string>;
}

/** The unfiltered state: everything through. */
export const NO_TIMELINE_FILTER: TimelineFilter = {
  categories: new Set(),
  ops: new Set(),
};

export function isTimelineFiltered(filter: TimelineFilter): boolean {
  return filter.categories.size > 0 || filter.ops.size > 0;
}

/**
 * The rows a filter admits, in their original order.
 *
 * The op set is consulted only for transactions. If it gated every row, turning
 * on an op chip would silently empty the navigation and issue lanes the user
 * had explicitly asked to keep — rows that carry no op at all and so could
 * never match one.
 */
export function filterTimeline(items: TimelineItem[], filter: TimelineFilter): TimelineItem[] {
  if (!isTimelineFiltered(filter)) return items;
  return items.filter((item) => {
    if (filter.categories.size > 0 && !filter.categories.has(rowCategory(item))) return false;
    const op = transactionOp(item);
    if (op === null || filter.ops.size === 0) return true;
    return filter.ops.has(op);
  });
}

/**
 * How many rows sit in each lane, over the WHOLE timeline.
 *
 * Deliberately blind to the current filter: a count that moved when you toggled
 * a different chip would be unreadable, and the zeros are what let a lane this
 * session never produced render as disabled rather than as a live control that
 * filters to nothing.
 */
export function categoryCounts(items: TimelineItem[]): Record<RowCategory, number> {
  const counts: Record<RowCategory, number> = {
    navigation: 0,
    transaction: 0,
    event: 0,
    issue: 0,
  };
  for (const item of items) counts[rowCategory(item)] += 1;
  return counts;
}

/**
 * The ops present among this session's transactions, most frequent first and
 * ties broken by name so the chip order is stable across reloads.
 *
 * Derived from the data rather than from a fixed list: the SDKs let an app name
 * its own ops, and offering ops this session never emitted would be a row of
 * chips that all filter to nothing.
 */
export function opCounts(items: TimelineItem[]): { op: string; count: number }[] {
  const counts = new Map<string, number>();
  for (const item of items) {
    const op = transactionOp(item);
    if (op === null) continue;
    counts.set(op, (counts.get(op) ?? 0) + 1);
  }
  return [...counts]
    .map(([op, count]) => ({ op, count }))
    .sort((a, b) => b.count - a.count || a.op.localeCompare(b.op));
}

/** Color bucket for a response status: 2xx green, 3xx neutral, 4xx amber, 5xx red. */
export type StatusTone = 'success' | 'neutral' | 'warning' | 'error';

export function httpStatusTone(status: number | null | undefined): StatusTone {
  if (status === null || status === undefined || Number.isNaN(status)) return 'neutral';
  if (status >= 500) return 'error';
  if (status >= 400) return 'warning';
  if (status >= 300) return 'neutral';
  if (status >= 200) return 'success';
  // 1xx and anything out of range: a code we have no opinion about.
  return 'neutral';
}

/** A non-empty trimmed string, or null — property bags are `unknown`-valued. */
function text(value: unknown): string | null {
  if (typeof value !== 'string') return null;
  const trimmed = value.trim();
  return trimmed === '' ? null : trimmed;
}

/**
 * The screen a `$screen` row is announcing.
 *
 * `properties.screen` is what `setScreen` puts on the wire. The top-level
 * `screen` column is the fallback: both SDKs update their current-screen state
 * *before* emitting the event, so for a `$screen` row the column holds the same
 * new screen rather than the one being left.
 */
function screenOfNavigation(item: TimelineItem): string | null {
  if (item.kind !== 'event') return null;
  return text(item.event.properties?.screen) ?? text(item.event.screen);
}

/**
 * The row's headline.
 *
 * A `$screen` row shows the screen instead of the raw event name, prefixed with
 * `$` — the marker this codebase already uses for names the platform assigned
 * rather than the app. Falls back to the literal `$screen` when the event
 * carries no screen at all.
 */
export function rowTitle(item: TimelineItem): string {
  switch (item.kind) {
    case 'event': {
      if (item.event.name !== SCREEN_EVENT) return item.event.name;
      const screen = screenOfNavigation(item);
      return screen ? `$${screen}` : SCREEN_EVENT;
    }
    case 'error': {
      const e = item.error;
      if (e.exception_type) {
        return e.exception_value ? `${e.exception_type}: ${e.exception_value}` : e.exception_type;
      }
      return e.message ?? 'Error';
    }
    case 'transaction':
      return httpTitle(item.transaction);
  }
}

/**
 * A transaction's headline, with the method restored when it is missing.
 *
 * The JS SDK's auto-instrumentation already names an HTTP transaction
 * `` `${method} ${path}` ``, so blindly prefixing `http_method` would render
 * "GET GET /api/login". A hand-rolled `trackTransaction`, or another SDK, may
 * name it anything — hence the check rather than a rule about who wrote it.
 */
function httpTitle(t: Transaction): string {
  const method = text(t.http_method)?.toUpperCase();
  if (t.op !== 'http' || !method) return t.name;
  return t.name.toUpperCase().startsWith(`${method} `) ? t.name : `${method} ${t.name}`;
}

/**
 * The trailing offset for row `i`, in milliseconds, or `null` when there is
 * nothing to measure against.
 *
 * `null` — not `0` — for the first row in `delta` mode: it has no predecessor,
 * and a `+<1 ms` there would claim a measurement that was never made. Also
 * `null` for an unusable reference (missing `startedAt`, unparseable timestamp,
 * or an item that predates the point it is measured from).
 */
export function offsetMs(
  items: TimelineItem[],
  i: number,
  startedAt: string | null | undefined,
  mode: TimeMode,
): number | null {
  const item = items[i];
  if (!item) return null;

  const from = mode === 'delta' ? (i === 0 ? null : items[i - 1]?.at) : startedAt;
  if (!from) return null;

  const ms = new Date(item.at).getTime() - new Date(from).getTime();
  if (Number.isNaN(ms) || ms < 0) return null;
  return ms;
}
