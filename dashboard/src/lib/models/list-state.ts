import { advance, emptyPage, goBack, goToPage, type CursorPage } from './cursor-page';
import { toggleSort, type SortDir, type SortState } from './sort';

/**
 * Sort and page position, changed together.
 *
 * They are one type rather than two fields on a page component because
 * changing the sort without resetting the page is a bug worth making hard to
 * write. `setCursorSort` and `setOffsetSort` return both halves, and the fields
 * are `readonly` so `state.sort = toggleSort(...)` — the accidental path — is a
 * type error. `SortState`'s own `key` and `dir` are `readonly` too (see
 * `sort.ts`), which blocks the shorter door as well: `state.sort.dir = 'asc'`
 * is now ALSO a type error, not just a reassignment of `sort` itself. That one
 * is the door that matters most in practice — under Svelte 5 `$state`
 * deep-proxying, mutating a field in place is reactive with no reducer call
 * anywhere near it, so nothing else has a chance to reset the page alongside
 * it.
 *
 * Be clear about how far that goes: `readonly` stops assignment and in-place
 * mutation, not reconstruction. `{ ...state, sort }` still compiles, and so
 * does building a replacement `sort` by spreading the old one —
 * `toggleSort` is still exported for `SortableTh` to use, and it already
 * returns a fresh object on every call rather than mutating its argument.
 * This makes the bug awkward and visible, NOT impossible, so a reviewer
 * looking at a page that spreads its own list state should check the page
 * came with it.
 *
 * The cursor case is the one that bites: a keyset cursor encodes a position
 * within ONE ordering. Replayed under a different sort it is compared against a
 * different column and the server answers with wrong rows and HTTP 200. (Slice
 * 2 adds a server-side backstop by embedding the sort key in the cursor; this
 * is the client-side half, and neither makes the other redundant.)
 */
export interface CursorListState {
  readonly sort: SortState;
  readonly page: CursorPage;
}

/** The offset-paged equivalent. Restarting the walk here means `offset = 0`. */
export interface OffsetListState {
  readonly sort: SortState;
  readonly offset: number;
}

export function setCursorSort(
  s: CursorListState,
  key: string,
  columnDefault: SortDir,
): CursorListState {
  return { sort: toggleSort(s.sort, key, columnDefault), page: emptyPage() };
}

/**
 * Move to the page reached by `nextCursor` — the `next_cursor` of the
 * envelope currently on screen — keeping the sort untouched.
 *
 * The Next/Prev reducer for cursor-paged lists. Without it, every page move
 * has to spread list state by hand at its call site —
 * `{ ...list, page: advance(list.page, c) }` — which is exactly the shape the
 * doc comment above asks a reviewer to treat as suspicious. Once several
 * tables adopt this state, legitimate page-move spreads would outnumber the
 * illegitimate sort-mutating ones the comment is actually watching for, and
 * that signal inverts.
 *
 * Returns `s` unchanged, BY REFERENCE, in exactly the cases `advance` itself
 * refuses to move (see `cursor-page.ts`). `cursor-page.ts` documents
 * `advance(p, c) !== p` as how a caller tells a real move from a refused one;
 * spreading `{ ...s, page: advance(s.page, c) }` here would build a new
 * `CursorListState` on every call regardless of whether `advance` moved
 * anything, since a fresh outer object is never `===` the old one — silently
 * erasing that signal at exactly the layer callers read it from.
 */
export function setCursorPage(
  s: CursorListState,
  nextCursor: string | null,
): CursorListState {
  const page = advance(s.page, nextCursor);
  return page === s.page ? s : { sort: s.sort, page };
}

/**
 * Back one page, keeping the sort untouched.
 *
 * Refuses the same way `goBack` does — `s` back BY REFERENCE when already on
 * the first page — for the same reason `setCursorPage` preserves `advance`'s
 * refusal: a no-op Prev has to stay invisible to a caller testing
 * `next !== current`, not turn into a structurally-equal copy that such a
 * test cannot tell apart from a real move.
 */
export function cursorBack(s: CursorListState, limit: number): CursorListState {
  const page = goBack(s.page, limit);
  return page === s.page ? s : { sort: s.sort, page };
}

/**
 * Move to a numbered page, choosing between the two mechanisms.
 *
 * This is the ONLY place that decision is made. A keyset step is stable under
 * concurrent inserts and an offset jump is not, so the cheaper guarantee is
 * taken whenever the target is adjacent and a cursor for it exists; everything
 * else falls back to an offset. Duplicating the branch at four call sites would
 * be four chances for one list to page differently from the others.
 *
 * `nextCursor` is the `next_cursor` of the envelope on screen — the same value
 * that decides whether Next is enabled, so the button's state and the mechanism
 * this picks cannot disagree.
 *
 * Refuses by reference like every other reducer here.
 */
export function cursorGoTo(
  s: CursorListState,
  target: number,
  nextCursor: string | null,
  limit: number,
): CursorListState {
  const page = goToPage(s.page, target, nextCursor, limit);
  return page === s.page ? s : { sort: s.sort, page };
}

export function setOffsetSort(
  s: OffsetListState,
  key: string,
  columnDefault: SortDir,
): OffsetListState {
  return { sort: toggleSort(s.sort, key, columnDefault), offset: 0 };
}

/**
 * Move to `offset`, keeping the sort untouched. The Next/Prev reducer for
 * offset-paged lists, for the same reason `setCursorPage` exists on the
 * cursor side.
 *
 * Unlike its cursor counterpart there is nothing to refuse: every offset a
 * Next/Prev click computes from the current one is a valid place to ask for,
 * so this always returns a new object. Bounds-checking `offset` against the
 * total row count is the caller's job — this module has no total to check it
 * against.
 */
export function setOffsetPage(s: OffsetListState, offset: number): OffsetListState {
  return { sort: s.sort, offset };
}

/**
 * One page of rows, plus whether another page follows it.
 *
 * The offset-paged endpoints return a bare array with no total, so "is there a
 * page after this one" cannot be read off the response. `Pagination` used to
 * infer it as `rows.length >= limit`, which is wrong for the one case that
 * matters: a final page holding exactly `limit` rows offered an enabled Next
 * leading to an empty page.
 */
export interface ListPage<T> {
  rows: T[];
  hasNext: boolean;
}

/**
 * Split an over-fetched response into the page to render and the has-more
 * answer.
 *
 * Callers request `limit + 1` rows; the surplus row is the probe and is
 * discarded. Every Group B endpoint clamps `limit` at 200 or above, so asking
 * for one more than any page size the UI offers stays inside the clamp — a
 * clamped request would silently return `limit` rows and report `hasNext:
 * false` on a full page.
 *
 * Lives here, tested, rather than inline at five clients because the whole
 * fix is one comparison: `data.length > limit` is right and `>= limit` is the
 * bug being removed, and the two differ by one character at a call site where
 * nothing would fail loudly.
 */
export function overFetched<T>(data: T[], limit: number): ListPage<T> {
  return { rows: data.slice(0, limit), hasNext: data.length > limit };
}
