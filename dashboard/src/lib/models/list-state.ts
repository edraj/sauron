import { advance, emptyPage, goBack, type CursorPage } from './cursor-page';
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
export function cursorBack(s: CursorListState): CursorListState {
  const page = goBack(s.page);
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
