/**
 * Which column a list is ordered by, and in which direction.
 *
 * There is deliberately no "unsorted" state. Every list already has a defined
 * default ordering, so returning to it means re-selecting the default column —
 * a third click that clears the sort would leave the table in an order the user
 * cannot name and cannot get back to.
 */
export type SortDir = 'asc' | 'desc';

/**
 * `key` and `dir` are `readonly` so a `SortState` cannot be changed in place —
 * `list.sort.dir = 'asc'` is a type error, not only the coarser
 * `list.sort = ...`. That closes the shorter of the two doors
 * `CursorListState`/`OffsetListState` guard against (see `list-state.ts`):
 * without it, a plain field mutation reaches past `sort`'s own `readonly` and,
 * under Svelte 5 `$state` deep-proxying, is reactive on its own — no reducer
 * call, so no accompanying reset of the cursor stack or offset. `toggleSort`
 * already returns a fresh object on every call, so this changes nothing there.
 */
export interface SortState {
  readonly key: string;
  readonly dir: SortDir;
}

/**
 * The `sort=` query parameter for a sort state.
 *
 * A BARE column name is descending and a `-` prefix is ascending, matching
 * `parse_sort` in `backend/bins/sauron-api/src/routes/search.rs`. That is the
 * inverse of the convention most APIs use, which is exactly why it is encoded
 * here once instead of at each call site: a hand-written `-last_seen` meaning
 * "newest first" produces oldest-first with no error to notice.
 */
export function sortParam(s: SortState): string {
  return s.dir === 'desc' ? s.key : `-${s.key}`;
}

/**
 * The sort state after clicking `key`.
 *
 * Clicking a different column selects it at `columnDefault` — `desc` for times
 * and counts, `asc` for names — because "sort by name" almost always means A-Z
 * while "sort by last seen" almost always means newest first. Clicking the
 * column already active flips it, ignoring `columnDefault`.
 *
 * Every call returns a new object, because every click changes something:
 * with no third "unsorted" state there is no no-op click to detect. Callers
 * therefore must NOT test `next !== current` to decide whether to refetch —
 * it is always true.
 */
export function toggleSort(
  current: SortState,
  key: string,
  columnDefault: SortDir,
): SortState {
  if (key !== current.key) return { key, dir: columnDefault };
  return { key, dir: current.dir === 'desc' ? 'asc' : 'desc' };
}
