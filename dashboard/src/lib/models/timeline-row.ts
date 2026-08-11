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
