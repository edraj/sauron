import type { SortDir } from './sort';

export type SortValue = string | number | boolean | Date | null | undefined;

const collator = new Intl.Collator(undefined, { sensitivity: 'base', numeric: true });

/** ISO-8601 with a date and a time — the shape every timestamp on the wire has. */
const ISO = /^\d{4}-\d{2}-\d{2}T/;

function compare(a: SortValue, b: SortValue): number {
  if (a instanceof Date && b instanceof Date) return a.getTime() - b.getTime();
  if (typeof a === 'number' && typeof b === 'number') return a - b;
  // `false` before `true` in ascending order — "unchecked" before "checked" is
  // the reading a reader gives a boolean column, and `Number(false)` being `0`
  // makes that the same rule as the numeric case above.
  if (typeof a === 'boolean' && typeof b === 'boolean') return Number(a) - Number(b);
  if (typeof a === 'string' && typeof b === 'string') {
    // ISO-8601 already sorts correctly as bytes, and a locale collator does
    // NOT preserve that — `numeric: true` reads the segments as numbers and
    // reorders around the punctuation.
    if (ISO.test(a) && ISO.test(b)) return a < b ? -1 : a > b ? 1 : 0;
    return collator.compare(a, b);
  }
  // Mixed or unexpected types: fall back to text so the order is at least
  // deterministic rather than dependent on which row the sort visited first.
  return collator.compare(String(a), String(b));
}

/**
 * Order `rows` by `accessor`, stably, without mutating the input.
 *
 * For lists held complete in the browser. Using it on a server-paginated table
 * would sort only the rows currently on screen while presenting itself as
 * having sorted the whole result set — see the spec.
 *
 * Nulls sort last in both directions. A null is an absent value rather than a
 * small one, so floating it to the top of an ascending sort would put "never
 * seen" above "seen longest ago" and read as a real answer.
 */
export function sortRows<T>(
  rows: readonly T[],
  accessor: (row: T) => SortValue,
  dir: SortDir,
): T[] {
  const sign = dir === 'asc' ? 1 : -1;
  // `toSorted` is not available on the ES target here; slice-then-sort is the
  // equivalent, and Array.prototype.sort has been required to be stable since
  // ES2019.
  return rows.slice().sort((ra, rb) => {
    const a = accessor(ra);
    const b = accessor(rb);
    const aEmpty = a === null || a === undefined;
    const bEmpty = b === null || b === undefined;
    // Null placement is direction-independent, so it is decided BEFORE `sign`
    // is applied rather than after.
    if (aEmpty && bEmpty) return 0;
    if (aEmpty) return 1;
    if (bEmpty) return -1;
    return sign * compare(a, b);
  });
}

/**
 * An accessor that orders a small enum by MEANING instead of by spelling.
 *
 * Severity is the case that matters. Sorted as text, `critical < info <
 * warning` — so `info` lands above `warning`, and the column looks sorted while
 * saying something nobody means. The same is true of every status column on
 * these tables: alphabetically a monitor that is `paused` outranks one that is
 * `unknown` for no reason a reader can defend.
 *
 * DIRECTION, stated once here so every caller inherits it: `order` runs from
 * LEAST to MOST — least severe, least urgent, least worth looking at first — so
 * a **higher rank is worse**. That makes a ranked column behave exactly like
 * every other magnitude column in these tables: `desc` (the direction
 * `SortableTh` gives a count by default) puts the worst row at the top, and
 * `aria-sort="descending"` announces what the reader is actually looking at.
 * A ladder written the other way round would invert every caret and every
 * announcement without changing a line of this file, which is why the rule
 * lives here and not in each `*-sort.ts`.
 *
 * UNKNOWN AND ABSENT VALUES rank `null`, not a number — so `sortRows` puts them
 * last in BOTH directions, the same treatment it gives every other absent
 * value. The alternatives are both worse and both look like they work:
 *
 * - `0` (or any low rank) makes an unrecognised status the least severe thing
 *   on the page; `order.length` makes it the most severe in descending order,
 *   so the row leads the "worst first" list it was never ranked for.
 * - Even `order.length` used as a plain number is asymmetric — last ascending,
 *   FIRST descending — which is exactly the asymmetry `emailOrNull` in
 *   `pii-inspector-sort.ts` exists to prevent.
 *
 * This is not a hypothetical: every ladder here is typed against a union the
 * backend can extend, and a status this dashboard has never heard of arrives as
 * a plain string that type-checks fine. It is unknown, not extreme.
 *
 * Generic in `T` so a ladder annotated `readonly AlertSeverity[]` rejects a
 * misspelt or non-member value at compile time. It cannot catch an OMITTED
 * member — an incomplete ladder is still a valid array — so each caller's test
 * pins the ladder against a `Record<Union, number>`, which does fail to compile
 * when the union grows.
 */
export function rankOf<T extends string>(
  order: readonly T[],
): (value: string | null | undefined) => number | null {
  const ranks = new Map<string, number>(order.map((v, i) => [v, i]));
  return (value) => (value == null ? null : (ranks.get(value) ?? null));
}
