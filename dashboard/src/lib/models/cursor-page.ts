/**
 * Page state for a keyset-cursor list.
 *
 * A keyset cursor only goes forward — there is no "cursor for page N-1" to ask
 * the server for — so going back means remembering the cursors already used.
 * `stack` holds them and `current` is the cursor that produced what is on
 * screen, `null` on the first page.
 *
 * ## `advance` takes the cursor of the page being MOVED TO
 *
 * The obvious alternative is to have one function that records the server's
 * `next_cursor` after every response and treats that as the page transition.
 * It is wrong, and the way it is wrong is silent.
 *
 * A page reloads for four reasons and only one of them is a page move: a Next
 * or Prev click, a Refresh click, a stale-while-revalidate refetch behind
 * cached rows, and a Retry after an error. Fold "record the response" into
 * "move forward" and the other three step the page state forward while the
 * rows on screen stay put.
 *
 * Traced concretely on page 2 of a 3-per-page list, with the plan's
 * `{stack, current, next}` state and its `advance` (push `current`, take
 * `current` from the old `next`). Page 2 is `{stack: [], current: '3',
 * next: '6'}`. Refresh refetches with cursor `'3'`, the server answers
 * `next_cursor: '6'`, and a record-is-move reducer files that as a move:
 *
 * ```
 * advance({stack: [], current: '3', next: '6'}, '6')
 *   → {stack: ['3'], current: '6', next: '6'}
 * ```
 *
 * The rows never moved. Note what does NOT break: the next Next click loads
 * `p.next`, which the refresh left at the correct value, so it still lands on
 * page 3. Forward paging keeps working, which is exactly why this survives
 * being clicked through once. What breaks is behind it:
 *
 * - **The stack grows one entry per Refresh, unbounded.** Nothing reads its
 *   length except Prev, so it accumulates silently.
 * - **Prev lies.** After one Refresh it pops `'3'` and reloads the rows already
 *   on screen — Prev looks dead. After two it pops `'6'` and moves the reader
 *   *forward* to page 3: a Prev button that pages forward.
 *
 * So the transition is driven by the click, and the cursor it moves to is read
 * from the envelope that produced the rows currently on screen. That gives two
 * properties worth stating:
 *
 * - **Reloading never moves the page.** Nothing in this module runs on a
 *   response, so Refresh, revalidate and Retry are inert by construction, not
 *   by a guard someone has to maintain.
 * - **There is one source of truth for "is there a next page".** It is
 *   `envelope.next_cursor` on the payload being rendered, so the Next button's
 *   enabled state and the cursor that button sends can never disagree. A
 *   `next` field cached in here would be a second copy, and a second copy of a
 *   fact is a thing that can drift from the first.
 *
 * That second property is why {@link CursorPage} has no `next` field, which is
 * the one place this deviates from the S2c plan's sketch.
 */
export interface CursorPage {
  /**
   * Cursors of the pages walked through to reach this one, oldest first.
   *
   * The first page's cursor is `null` and is never pushed, so an empty stack
   * means "the previous page is the first page" — which is exactly what popping
   * an empty stack to `null` yields.
   */
  stack: string[];
  /** The cursor that produced what is on screen. `null` on the first page. */
  current: string | null;
}

export function emptyPage(): CursorPage {
  return { stack: [], current: null };
}

/**
 * Move to the page reached by `nextCursor` — the `next_cursor` of the envelope
 * currently on screen.
 *
 * Returns the page it was handed, by reference, whenever the move is refused —
 * so a caller can test `advance(p, c) !== p` to mean "this actually moved" and
 * skip the reload otherwise. Three things are refused, and the last two exist
 * because each of them silently rebuilds the confidently-wrong pager this
 * control replaced:
 *
 * - **No next page.** `null` is the server saying there is nothing after this,
 *   so a Next click that cannot go anywhere must not move the state either. The
 *   button is disabled in that state; this makes a stray call harmless rather
 *   than corrupting the walk.
 * - **A falsy cursor.** `if (opts.cursor)` in `api/search.ts` drops the cursor
 *   from the query string on any falsy value, so a `next_cursor: ""` would move
 *   the walk, pass `canNext`, then be dropped from the request — and the server
 *   would answer page one while the pager called it "Page 2". Empty string is
 *   not a position in a result set, so it is refused here rather than trusted
 *   and then quietly discarded downstream.
 * - **A cursor equal to the current one.** The classic `<=` keyset boundary bug
 *   returns the cursor that produced this very page. Advancing on it yields
 *   `{stack: [...,'c1'], current: 'c1'}`, which hashes to the same cache key as
 *   `{stack: [], current: 'c1'}` — so Next would increment the page number and
 *   repaint the identical rows straight out of the cache, with no request on
 *   the wire to notice.
 *
 * Both of the last two are unreachable against today's backend. They are
 * guarded anyway because the failure they produce is a pager that lies with
 * total confidence, and neither leaves a trace anywhere else.
 */
export function advance(p: CursorPage, nextCursor: string | null): CursorPage {
  if (!nextCursor) return p;
  if (nextCursor === p.current) return p;
  return {
    stack: p.current === null ? [...p.stack] : [...p.stack, p.current],
    current: nextCursor,
  };
}

/** Back one page. A no-op on the first page rather than an error. */
export function goBack(p: CursorPage): CursorPage {
  if (p.current === null) return p;
  const stack = [...p.stack];
  const prev = stack.pop() ?? null;
  return { stack, current: prev };
}

/** True when there is a page before this one. */
export function canGoBack(p: CursorPage): boolean {
  return p.current !== null;
}

/**
 * The `cursor` query parameter for this page — `undefined` on the first page,
 * since the request simply omits it there.
 */
export function cursorOf(p: CursorPage): string | undefined {
  return p.current ?? undefined;
}

/**
 * 1-based page number, for display.
 *
 * Counted from the walk itself — the number of Next clicks plus one — and not
 * inferred from row counts. A row range ("51-100") would have to assume every
 * page before the last is exactly `limit` rows long, and a pager that infers a
 * range it was not told is precisely what the offset pager on Events did wrong
 * before this replaced it.
 */
export function pageNumber(p: CursorPage): number {
  return p.current === null ? 1 : p.stack.length + 2;
}
