/**
 * Offset math and export naming for the person Activity timeline — the parts
 * that are pure functions of the row list, kept out of `PersonProfile.svelte`
 * so they can be tested without a DOM (the dashboard's vitest runs on the node
 * environment).
 *
 * Deliberately separate from `timeline-row.ts`, which serves the *session*
 * timeline. The two lists run in opposite directions: a session reads oldest
 * first, a person's activity reads newest first. `offsetMs` there measures
 * against the row above and suppresses negatives, so pointing it at this list
 * would render an em dash on every single row.
 */
import { formatDuration, formatMs } from '../utils/format';

/** Which offset a person-timeline row's trailing time reads against. */
export type PersonTimeMode = 'start' | 'delta';

/**
 * The minimum a row must expose to be measured. `PersonProfile` holds richer
 * items; this asks only for the epoch-millisecond timestamp so the tests need
 * no event fixtures.
 */
export interface PersonTimelineEntry {
  at: number;
}

/** A day in milliseconds — the tier where `formatDuration` stops being useful. */
const DAY_MS = 86_400_000;

/** Longest distinct-id fragment allowed into a download filename. */
const MAX_FILENAME_ID = 80;

/**
 * The trailing offset for row `i`, in milliseconds, or `null` when there is
 * nothing to measure against.
 *
 * `items` is **descending** — newest first, as the profile page sorts it. So
 * the chronologically previous entry is the row *below* (`i + 1`), which is
 * what `delta` mode measures against; measuring against the row above would
 * invert every gap.
 *
 * `null` — not `0` — for the last row in `delta` mode: it is the oldest entry
 * loaded, it has no predecessor, and a `+0` there would claim a measurement
 * that was never made. Also `null` for an unparseable timestamp, or for a list
 * that is not actually descending (which would otherwise render a negative).
 */
export function personOffsetMs(
  items: readonly PersonTimelineEntry[],
  i: number,
  mode: PersonTimeMode,
): number | null {
  const item = items[i];
  if (!item) return null;

  // `start` anchors on the oldest *loaded* entry, not on the person's
  // first_seen: the page pulls a capped window, so first_seen may sit outside
  // it, and every number here should be derivable from what is on screen.
  const from = mode === 'delta' ? items[i + 1]?.at : items[items.length - 1]?.at;
  if (from === undefined) return null;

  const ms = item.at - from;
  if (Number.isNaN(ms) || ms < 0) return null;
  return ms;
}

/**
 * An offset rendered for humans, across the whole range one person's activity
 * can span — which is milliseconds to months, wider than either shared
 * formatter covers alone.
 *
 * - Under a second, `formatMs`, because `formatDuration` rounds to a tenth of
 *   a second and would print three consecutive rows 0 ms, 2 ms and 962 ms
 *   apart as an identical "0.0s". This is also what the session timeline
 *   renders, so the two pages agree wherever their ranges overlap.
 * - Under a day, `formatDuration`, because `formatMs` tops out in seconds and
 *   would print an hour as "3600.00 s".
 * - Above that, a day tier of this module's own, because `formatDuration` tops
 *   out in hours and would print a month as "720h 00m".
 */
export function formatOffset(ms: number | null | undefined): string {
  if (ms === null || ms === undefined || Number.isNaN(ms) || ms < 0) return '—';
  if (ms < 1000) return formatMs(ms);
  if (ms < DAY_MS) return formatDuration(ms);
  const days = Math.floor(ms / DAY_MS);
  const hours = Math.floor((ms % DAY_MS) / 3_600_000);
  return `${days}d ${String(hours).padStart(2, '0')}h`;
}

/**
 * The filename for a person's JSON export.
 *
 * Distinct IDs are supplied by the instrumented app — emails, paths, anything
 * a developer passed to `identify` — so unlike the session page's UUIDs they
 * cannot go straight into `a.download`. A separator would make the browser
 * drop or reinterpret the name, and a leading dot would hide the file.
 */
export function personJsonFilename(distinctId: string): string {
  const safe = distinctId
    .replace(/[^A-Za-z0-9._-]+/g, '_')
    .replace(/^[._-]+/, '')
    .slice(0, MAX_FILENAME_ID);
  return `person-${safe === '' ? 'unknown' : safe}.json`;
}
