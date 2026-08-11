/**
 * One page out of a complete, in-memory list.
 *
 * `hasNext` is computed from the total rather than inferred from the page
 * length, which is the whole advantage of holding every row: the server-side
 * pager could only guess `count >= limit` and got the exactly-full final page
 * wrong every time.
 */
export function pageSlice<T>(
  rows: readonly T[],
  offset: number,
  limit: number,
): { rows: T[]; hasNext: boolean } {
  const start = Math.max(0, offset);
  return {
    rows: rows.slice(start, start + limit),
    hasNext: start + limit < rows.length,
  };
}
