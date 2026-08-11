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
