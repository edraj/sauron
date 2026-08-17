import { describe, it, expect } from 'vitest';
import {
  advance,
  canGoBack,
  cursorOf,
  emptyPage,
  goBack,
  jumpTo,
  offsetOf,
  pageKey,
  pageNumber,
} from './cursor-page';

/** Page size for the bookkeeping tests, where the value is arbitrary. */
const L = 10;

describe('cursor paging', () => {
  it('starts with nowhere to go back to', () => {
    expect(canGoBack(emptyPage())).toBe(false);
    expect(cursorOf(emptyPage())).toBeUndefined();
    expect(pageNumber(emptyPage())).toBe(1);
  });

  // A keyset cursor only moves forward, so "previous" is a stack of the
  // cursors already used, not an arithmetic offset.
  it('walks forward and back over the same cursors', () => {
    let p = emptyPage();
    p = advance(p, 'c1');
    p = advance(p, 'c2');
    expect(p.current).toBe('c2');
    expect(canGoBack(p)).toBe(true);
    p = goBack(p, L);
    expect(p.current).toBe('c1');
    p = goBack(p, L);
    expect(p.current).toBeNull();
    expect(canGoBack(p)).toBe(false);
  });

  // `advance` takes the cursor of the page being MOVED TO, so a null argument
  // is "there is no such page" — the last page's Next is not a click that goes
  // nowhere, it is a click that must not happen at all.
  //
  // `toBe`, not `toEqual`: refusing by returning the argument BY REFERENCE is
  // the contract the call sites read (`advance(p, c) !== p` means "it moved"),
  // so a structurally-equal copy would be a silent break.
  it('refuses to advance when there is no next cursor', () => {
    const p1 = emptyPage();
    expect(advance(p1, null)).toBe(p1);
    const p2 = advance(emptyPage(), 'c1');
    expect(advance(p2, null)).toBe(p2);
  });

  // A `next_cursor: ""` is not a position in a result set, and `api/search.ts`
  // drops the cursor from the query string on any FALSY value — so a walk that
  // accepted one would move the page number, then send a request with no cursor
  // at all and label page one "Page 2". Unreachable today; refused here because
  // the symptom is a pager that lies with no trace.
  it('refuses a falsy cursor, not merely a null one', () => {
    const p1 = emptyPage();
    expect(advance(p1, '')).toBe(p1);
    const p2 = advance(emptyPage(), 'c1');
    expect(advance(p2, '')).toBe(p2);
  });

  // The `<=` keyset boundary bug: the server hands back the cursor that produced
  // THIS page. Advancing on it gives {stack:['c1'], current:'c1'}, which keys the
  // same cache entry as {stack:[], current:'c1'} — Next would bump the page
  // number and repaint the identical rows with no request on the wire.
  it('refuses a next cursor equal to the current one', () => {
    const p = advance(emptyPage(), 'c1');
    expect(advance(p, 'c1')).toBe(p);
    expect(pageNumber(advance(p, 'c1'))).toBe(2);
    // Only the CURRENT cursor is refused — revisiting an earlier one is a
    // legitimate (if odd) forward move, not a repeat of what is on screen.
    expect(advance(advance(p, 'c2'), 'c1').current).toBe('c1');
  });

  it('going back past the start is a no-op rather than an error', () => {
    const p = goBack(goBack(emptyPage(), L), L);
    expect(p.current).toBeNull();
    expect(canGoBack(p)).toBe(false);
    expect(pageNumber(p)).toBe(1);
  });

  it('numbers pages from one and reports the request cursor', () => {
    let p = emptyPage();
    expect(pageNumber(p)).toBe(1);
    expect(cursorOf(p)).toBeUndefined();
    p = advance(p, 'c1');
    expect(pageNumber(p)).toBe(2);
    expect(cursorOf(p)).toBe('c1');
    p = advance(p, 'c2');
    expect(pageNumber(p)).toBe(3);
    expect(cursorOf(p)).toBe('c2');
    p = goBack(p, L);
    expect(pageNumber(p)).toBe(2);
    p = goBack(p, L);
    expect(pageNumber(p)).toBe(1);
    expect(cursorOf(p)).toBeUndefined();
  });

  // Two pages deep on purpose. Built from ONE advance the stack is empty, and
  // popping or aliasing an empty array is unobservable — `goBack` popping the
  // caller's stack in place, `advance` handing back an array the caller still
  // holds, and `emptyPage()` returning one shared module-level object all pass
  // a one-deep version of this test. The walk state here is
  // {stack: ['c1'], current: 'c2'}, which has something to lose.
  it('does not mutate the page it is handed', () => {
    const p = advance(advance(emptyPage(), 'c1'), 'c2');
    const before = structuredClone(p);
    const forward = advance(p, 'c3');
    const back = goBack(p, L);
    expect(p).toEqual(before);
    // Not merely equal afterwards: neither result may SHARE the array, or the
    // next write through one page corrupts the other.
    expect(forward.stack).not.toBe(p.stack);
    expect(back.stack).not.toBe(p.stack);
  });

  // The page-1 branch of `advance` has its own copy, and it needs its own
  // assertion: no reachable state has `current === null` with a non-empty stack
  // (goBack pops to null only once the stack is spent), so aliasing there
  // survives every walk-level test above.
  it('does not hand back the first page\'s own array', () => {
    const first = emptyPage();
    expect(advance(first, 'c1').stack).not.toBe(first.stack);
  });

  // `emptyPage()` is called on every predicate change, on both pages, and the
  // result is written into `$state.raw` and then walked. A shared module-level
  // object would make one list's reset visible in the other's walk.
  it('builds a fresh first page every call', () => {
    expect(emptyPage()).not.toBe(emptyPage());
    expect(emptyPage().stack).not.toBe(emptyPage().stack);
  });
});

// ---------------------------------------------------------------------------
// The acceptance test: a walk over a fake paged server.
//
// Everything above asserts bookkeeping over cursor STRINGS, which is exactly
// what the pager this reducer drives got wrong before: the offset pager on
// Events re-fetched page one and confidently relabelled the same 50 rows
// "51-100". String bookkeeping would have passed against that. Only walking
// real rows through a server that answers the way the API does can catch a row
// served twice or skipped.
// ---------------------------------------------------------------------------

/** Rows the fake server holds, in the order it returns them. */
const ROWS = ['r1', 'r2', 'r3', 'r4', 'r5', 'r6', 'r7'];
const PAGE_SIZE = 3;

interface FakeEnvelope {
  data: string[];
  next_cursor: string | null;
}

/**
 * Stands in for `listIssues`/`listEvents`: takes the cursor and offset the
 * client would send (`undefined` for both on the first page) and answers the
 * envelope shape the real routes answer, `next_cursor` null on the last page
 * only.
 *
 * The cursor is an opaque token as far as the reducer is concerned; here it
 * happens to encode the keyset boundary as a row index.
 *
 * Mirrors the repo-layer precedence exactly: an offset is honoured only when
 * there is no cursor. A server that applied both would skip rows *within* the
 * keyset-narrowed set, so a test double that quietly allowed the combination
 * would pass a reducer that sends it.
 */
function fetchPage(cursor: string | undefined, offset?: number): FakeEnvelope {
  if (cursor !== undefined && offset) {
    throw new Error(`fake server got cursor AND offset together: ${cursor} / ${offset}`);
  }
  const start = cursor === undefined ? (offset ?? 0) : Number(cursor);
  if (!Number.isInteger(start) || start < 0) {
    throw new Error(`fake server got a cursor it never issued: ${String(cursor)}`);
  }
  const data = ROWS.slice(start, start + PAGE_SIZE);
  const end = start + data.length;
  return { data, next_cursor: end < ROWS.length ? String(end) : null };
}

describe('cursor paging against a fake paged server', () => {
  /**
   * Walk forward from `p` to the last page, collecting each page's rows.
   *
   * Bounded on purpose. A reducer whose `advance` fails to move — the exact
   * regression this suite exists to catch — would otherwise refetch page one
   * forever, and the symptom would be a hung run or an out-of-memory kill
   * rather than a failed assertion. Verified: against a no-op stub this
   * throws in milliseconds.
   */
  function walkForward(from: ReturnType<typeof emptyPage>) {
    const MAX_PAGES = Math.ceil(ROWS.length / PAGE_SIZE) + 1;
    let p = from;
    const pages: string[][] = [];
    const canNext: boolean[] = [];
    const canPrev: boolean[] = [];
    let env = fetchPage(cursorOf(p), offsetOf(p));
    for (;;) {
      pages.push(env.data);
      canNext.push(env.next_cursor !== null);
      canPrev.push(canGoBack(p));
      if (env.next_cursor === null) break;
      if (pages.length > MAX_PAGES) {
        throw new Error(`walk did not terminate after ${pages.length} pages — advance is not moving`);
      }
      p = advance(p, env.next_cursor);
      env = fetchPage(cursorOf(p), offsetOf(p));
    }
    return { p, pages, canNext, canPrev };
  }

  it('serves every row exactly once, in order, walking forward', () => {
    const { pages } = walkForward(emptyPage());
    expect(pages).toEqual([['r1', 'r2', 'r3'], ['r4', 'r5', 'r6'], ['r7']]);
    // In order, nothing skipped...
    expect(pages.flat()).toEqual(ROWS);
    // ...and nothing served twice.
    expect(new Set(pages.flat()).size).toBe(ROWS.length);
  });

  it('offers Next only off the last page and Prev only off the first', () => {
    const { canNext, canPrev } = walkForward(emptyPage());
    expect(canNext).toEqual([true, true, false]);
    expect(canPrev).toEqual([false, true, true]);
  });

  /** Walk back to the first page, collecting each page's rows. Bounded for the same reason. */
  function walkBack(from: ReturnType<typeof emptyPage>) {
    const MAX_PAGES = Math.ceil(ROWS.length / PAGE_SIZE) + 1;
    let p = from;
    const pages: string[][] = [];
    while (canGoBack(p)) {
      if (pages.length > MAX_PAGES) {
        throw new Error(`back-walk did not terminate after ${pages.length} pages — goBack is not moving`);
      }
      p = goBack(p, PAGE_SIZE);
      pages.unshift(fetchPage(cursorOf(p), offsetOf(p)).data);
    }
    return { p, pages };
  }

  it('walks back over the pages it came forward through', () => {
    const forward = walkForward(emptyPage());
    expect(pageNumber(forward.p)).toBe(3);

    const { p, pages: back } = walkBack(forward.p);
    expect(pageNumber(p)).toBe(1);
    expect(cursorOf(p)).toBeUndefined();
    // Pages 1 and 2, the ones it walked back over.
    expect(back).toEqual(forward.pages.slice(0, 2));
  });

  it('re-walking forward after going back yields identical pages', () => {
    const first = walkForward(emptyPage());
    const second = walkForward(walkBack(first.p).p);
    expect(second.pages).toEqual(first.pages);
    expect(second.canNext).toEqual(first.canNext);
    expect(second.canPrev).toEqual(first.canPrev);
  });

  // The bug that decided the shape of `advance`. Every page reloads for reasons
  // that are not a page move: a Refresh click, a stale-while-revalidate
  // refetch, a Retry after an error. If recording the server's `next_cursor`
  // were itself the page transition, each of those would silently step the page
  // forward while the rows on screen stayed put, and the walk would then skip
  // rows. Page state moves on clicks only, so a reload is inert.
  it('reloading the current page does not move it', () => {
    let p = emptyPage();
    p = advance(p, fetchPage(cursorOf(p)).next_cursor);
    expect(pageNumber(p)).toBe(2);

    const seen = [fetchPage(cursorOf(p)), fetchPage(cursorOf(p)), fetchPage(cursorOf(p))];
    expect(seen[1]).toEqual(seen[0]);
    expect(seen[2]).toEqual(seen[0]);
    expect(pageNumber(p)).toBe(2);

    // ...and carrying on from there still covers the fixture exactly once.
    const rest = walkForward(p);
    expect([...fetchPage(undefined).data, ...rest.pages.flat()]).toEqual(ROWS);
  });

  // -------------------------------------------------------------------------
  // Jumps. Everything above walks; these land somewhere the walk never visited
  // and then resume walking from there, which is where the two mechanisms meet.
  // -------------------------------------------------------------------------

  it('lands on the rows the page number promises', () => {
    const p = jumpTo(emptyPage(), 3, PAGE_SIZE);
    expect(pageNumber(p)).toBe(3);
    expect(cursorOf(p)).toBeUndefined();
    expect(offsetOf(p)).toBe(6);
    expect(fetchPage(cursorOf(p), offsetOf(p)).data).toEqual(['r7']);
  });

  it('resumes the keyset walk after a jump', () => {
    // Jump to page 2, then Next: the cursor comes off the jumped page's own
    // envelope, so the forward walk continues without another offset.
    const jumped = jumpTo(emptyPage(), 2, PAGE_SIZE);
    const env = fetchPage(cursorOf(jumped), offsetOf(jumped));
    expect(env.data).toEqual(['r4', 'r5', 'r6']);

    const next = advance(jumped, env.next_cursor);
    expect(pageNumber(next)).toBe(3);
    expect(offsetOf(next)).toBeUndefined();
    expect(cursorOf(next)).toBe('6');
    expect(fetchPage(cursorOf(next), offsetOf(next)).data).toEqual(['r7']);
  });

  /**
   * The regression the stored `page` field exists for.
   *
   * Jump to 2, walk forward to 3, then Prev. The stack is empty — the jump
   * discarded it and `advance` off a null cursor pushes nothing — so a `goBack`
   * that only pops would return `{current: null}` and the old
   * `pageNumber = stack.length + 2` would call that page 1. The reader clicks
   * Prev on page 3 and lands on page 1, having skipped page 2 entirely.
   */
  it('goes back correctly from a page walked to from a jump', () => {
    const jumped = jumpTo(emptyPage(), 2, PAGE_SIZE);
    const onThree = advance(jumped, fetchPage(cursorOf(jumped), offsetOf(jumped)).next_cursor);
    expect(pageNumber(onThree)).toBe(3);
    expect(onThree.stack).toEqual([]);

    const back = goBack(onThree, PAGE_SIZE);
    expect(pageNumber(back)).toBe(2);
    expect(fetchPage(cursorOf(back), offsetOf(back)).data).toEqual(['r4', 'r5', 'r6']);
  });

  it('walks all the way back from a jump without skipping a page', () => {
    let p = jumpTo(emptyPage(), 3, PAGE_SIZE);
    const seen: string[][] = [];
    while (canGoBack(p)) {
      p = goBack(p, PAGE_SIZE);
      seen.unshift(fetchPage(cursorOf(p), offsetOf(p)).data);
    }
    expect(pageNumber(p)).toBe(1);
    expect(seen).toEqual([
      ['r1', 'r2', 'r3'],
      ['r4', 'r5', 'r6'],
    ]);
  });

  it('offers Prev on a jumped page', () => {
    // `current` is null on a jumped page, which is what page 1 carries. The
    // old `canGoBack` tested exactly that and disabled Prev on a page with two
    // pages behind it.
    expect(canGoBack(jumpTo(emptyPage(), 3, PAGE_SIZE))).toBe(true);
    expect(canGoBack(emptyPage())).toBe(false);
  });

  it('jumping to page 1 is the first page, offset and all', () => {
    const p = jumpTo(jumpTo(emptyPage(), 4, PAGE_SIZE), 1, PAGE_SIZE);
    expect(p).toEqual(emptyPage());
  });

  it('refuses a jump that does not move, by reference', () => {
    const p = jumpTo(emptyPage(), 3, PAGE_SIZE);
    expect(jumpTo(p, 3, PAGE_SIZE)).toBe(p);
    const first = emptyPage();
    expect(jumpTo(first, 1, PAGE_SIZE)).toBe(first);
  });

  it('clamps a jump below page 1', () => {
    expect(pageNumber(jumpTo(emptyPage(), 0, PAGE_SIZE))).toBe(1);
    expect(pageNumber(jumpTo(emptyPage(), -5, PAGE_SIZE))).toBe(1);
  });

  /**
   * Cache keys. A jumped page carries `current: null`, the same as page 1, so
   * a key built from the cursor alone hashes page 3 to page 1's entry — Next
   * would repaint the first page's rows out of the cache with no request on
   * the wire to notice.
   */
  it('keys every distinct page distinctly', () => {
    const first = emptyPage();
    const jumped = jumpTo(first, 3, PAGE_SIZE);
    const walked = advance(advance(first, '3'), '6');

    expect(cursorOf(jumped)).toBe(cursorOf(first)); // the collision, stated
    expect(pageKey(jumped)).not.toBe(pageKey(first));
    expect(pageNumber(jumped)).toBe(pageNumber(walked));
    // Same page number, reached two ways, with different rows in flight —
    // these must not share a cache entry either.
    expect(pageKey(jumped)).not.toBe(pageKey(walked));
  });
});
