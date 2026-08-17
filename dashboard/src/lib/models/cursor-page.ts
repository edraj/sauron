/**
 * Page state for a keyset-cursor list that also supports jumping to a numbered
 * page.
 *
 * A keyset cursor only goes forward — there is no "cursor for page N-1" to ask
 * the server for — so going back means remembering the cursors already used.
 * `stack` holds them and `current` is the cursor that produced what is on
 * screen, `null` on the first page.
 *
 * ## Two mechanisms, one control
 *
 * Stepping ±1 is a keyset move: stable under concurrent inserts, and the only
 * thing a live list can page with. Jumping to a numbered page is an OFFSET,
 * because there is no cursor for a page nobody has visited. The two coexist:
 *
 * - `advance` and the pop half of `goBack` are keyset moves.
 * - `jumpTo`, and `goBack` when the stack is spent, are offset moves.
 *
 * A jump is approximate by nature — the reader is asking to land near a
 * position, not to continue a traversal — so the weaker guarantee is bought
 * only where it is unavoidable. Walking is never downgraded to offset.
 *
 * ## `page` is stored, not derived
 *
 * It used to be `stack.length + 2`, which was exact while the stack was the
 * only way to move. It is not any more: a jump leaves the stack empty at page
 * 5, and a derived number would call that page 1 while the rows say otherwise.
 * `page` is therefore the authoritative field and the derivation is gone rather
 * than kept as a second answer to the same question.
 *
 * The same change is why {@link canGoBack} tests `page > 1` and not
 * `current !== null`. On a jumped page `current` IS null, so the old test
 * disabled Prev on a page with four pages behind it.
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
 *   enabled state and the cursor it sends can never disagree. A `next` field
 *   cached in here would be a second copy, and a second copy of a fact is a
 *   thing that can drift from the first.
 */
export interface CursorPage {
  /**
   * Cursors of the pages walked through to reach this one, oldest first.
   *
   * The first page's cursor is `null` and is never pushed, so an empty stack
   * means "the previous page is not reachable by popping" — either because
   * this is page 1, or because a jump discarded the walk.
   */
  stack: string[];
  /** The cursor that produced what is on screen. `null` on page 1 AND after a jump. */
  current: string | null;
  /**
   * Rows the server is asked to skip. Non-zero only on a page reached by a
   * jump; every keyset move resets it to 0.
   *
   * Sent only when `current` is null — the repo layer refuses the combination
   * anyway, because an offset applied on top of a keyset predicate skips rows
   * *within* the already-narrowed set, which is a silently wrong page rather
   * than an error.
   */
  offset: number;
  /** 1-based page number. Authoritative — see the module doc. */
  page: number;
}

export function emptyPage(): CursorPage {
  return { stack: [], current: null, offset: 0, page: 1 };
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
 *
 * Advancing off a JUMPED page is an ordinary keyset move: `current` is null
 * there, so nothing is pushed and the walk restarts its stack from this page.
 * That is what leaves `goBack` with an empty stack at page > 1, which is the
 * case it now handles by jumping.
 */
export function advance(p: CursorPage, nextCursor: string | null): CursorPage {
  if (!nextCursor) return p;
  if (nextCursor === p.current) return p;
  return {
    stack: p.current === null ? [...p.stack] : [...p.stack, p.current],
    current: nextCursor,
    offset: 0,
    page: p.page + 1,
  };
}

/**
 * Back one page. A no-op on the first page rather than an error.
 *
 * Pops the stack when it can. When it cannot — the page was reached by a jump,
 * or by walking forward from one — it jumps to `page - 1` instead, which needs
 * `limit` to turn a page number into an offset.
 *
 * The two paths agree where they overlap. On page 2 of an ordinary walk the
 * stack is empty, so this jumps to page 1 with offset 0 and a null cursor —
 * byte-identical to what popping to `null` produced before.
 */
export function goBack(p: CursorPage, limit: number): CursorPage {
  if (p.page <= 1) return p;
  if (p.stack.length === 0) return jumpTo(p, p.page - 1, limit);
  const stack = [...p.stack];
  const prev = stack.pop() ?? null;
  return { stack, current: prev, offset: 0, page: p.page - 1 };
}

/**
 * Jump to a numbered page by offset, discarding the walk.
 *
 * The stack is cleared rather than kept: its entries are cursors for pages
 * behind the page we *were* on, and after a jump they no longer sit behind the
 * page we are on. Keeping them would make Prev pop a cursor for an unrelated
 * position — the "Prev lies" failure in the module doc, arrived at from a new
 * direction.
 *
 * Returns the page by reference when it would not move, matching `advance`.
 */
export function jumpTo(p: CursorPage, page: number, limit: number): CursorPage {
  const target = Math.max(1, Math.round(page) || 1);
  if (target === p.page) return p;
  return { stack: [], current: null, offset: (target - 1) * limit, page: target };
}

/**
 * Move to a numbered page, choosing between the two mechanisms.
 *
 * The single decision point for every cursor-paged list. A keyset step is
 * stable under concurrent inserts and an offset jump is not, so the stronger
 * guarantee is taken whenever the target is adjacent AND a cursor for it
 * exists; everything else falls back to an offset.
 *
 * Adjacency alone is not enough, which is why `nextCursor` is a parameter and
 * not an assumption. At the count cap the last numbered page can still have
 * rows after it, and a caller can ask for `page + 1` before that envelope has
 * landed. Stepping on a null cursor would send neither a cursor nor an offset,
 * and the server answers that with page one while the pager says "Page 3".
 *
 * `nextCursor` is the `next_cursor` of the envelope on screen — the same value
 * that decides whether Next is enabled — so the button's state and the
 * mechanism chosen here cannot disagree.
 *
 * Refuses by reference, like {@link advance} and {@link jumpTo}.
 */
export function goToPage(
  p: CursorPage,
  target: number,
  nextCursor: string | null,
  limit: number,
): CursorPage {
  if (target === p.page) return p;
  if (target === p.page + 1 && nextCursor) return advance(p, nextCursor);
  if (target === p.page - 1) return goBack(p, limit);
  return jumpTo(p, target, limit);
}

/**
 * True when there is a page before this one.
 *
 * `page > 1`, not `current !== null`: a jumped page has a null cursor and
 * pages behind it, and the old test disabled Prev on every one of them.
 */
export function canGoBack(p: CursorPage): boolean {
  return p.page > 1;
}

/**
 * The `cursor` query parameter for this page — `undefined` on page 1 and on any
 * jumped page, where there is no cursor to send.
 */
export function cursorOf(p: CursorPage): string | undefined {
  return p.current ?? undefined;
}

/**
 * The `offset` query parameter — `undefined` unless this page was jumped to.
 *
 * Falsy-to-undefined on purpose: `offset=0` is the default the server already
 * applies, and sending it would put a parameter on the wire that reads as a
 * deliberate choice.
 */
export function offsetOf(p: CursorPage): number | undefined {
  return p.offset || undefined;
}

/** 1-based page number, for display. */
export function pageNumber(p: CursorPage): number {
  return p.page;
}

/**
 * A stable identity for this page, for cache keys.
 *
 * Must be used instead of `cursorOf(p)` in any `viewKey` tuple. A jumped page
 * carries `current: null`, which is what page 1 carries — so a key built from
 * the cursor alone hashes page 7 to page 1's entry and repaints the first page
 * out of the cache with no request on the wire to notice. That is the
 * CachedView moving-key trap in reverse: not a key that moves when the data
 * has not, but a key that stands still when the page has.
 */
export function pageKey(p: CursorPage): string {
  return `${p.page}:${p.current ?? ''}:${p.offset}`;
}
