import { describe, expect, it } from 'vitest';
import { advance, emptyPage } from './cursor-page';
import {
  cursorBack,
  setCursorPage,
  setCursorSort,
  setOffsetPage,
  setOffsetSort,
  type CursorListState,
  type OffsetListState,
} from './list-state';

describe('setCursorSort', () => {
  const onPageThree = {
    sort: { key: 'last_seen', dir: 'desc' } as const,
    page: advance(advance(emptyPage(), 'c1'), 'c2'),
  };

  // The cursor is a position within ONE ordering. Carried across a sort change
  // it is compared against a different column and returns wrong rows with no
  // error, so the walk must restart.
  it('restarts the walk when the column changes', () => {
    const next = setCursorSort(onPageThree, 'times_seen', 'desc');
    expect(next.sort).toEqual({ key: 'times_seen', dir: 'desc' });
    expect(next.page).toEqual(emptyPage());
  });

  it('restarts the walk when only the direction changes', () => {
    const next = setCursorSort(onPageThree, 'last_seen', 'desc');
    expect(next.sort).toEqual({ key: 'last_seen', dir: 'asc' });
    expect(next.page).toEqual(emptyPage());
  });

  it('clears the walked-cursor stack, not just the current position', () => {
    // The half-fix this catches: resetting `current` to null while keeping
    // `stack` leaves Prev holding cursors minted under the OLD ordering. The
    // reader lands on a page assembled from positions in a list that no longer
    // exists, and `pageNumber` still counts them, so the pager reports page 3
    // of a walk that restarted.
    expect(onPageThree.page.stack.length).toBeGreaterThan(0); // the fixture bites
    expect(setCursorSort(onPageThree, 'times_seen', 'desc').page.stack).toEqual([]);
  });

  it('does not mutate the state it was given', () => {
    const before = { sort: { ...onPageThree.sort }, page: onPageThree.page };
    setCursorSort(onPageThree, 'times_seen', 'desc');
    expect(onPageThree.sort).toEqual(before.sort);
    expect(onPageThree.page).toBe(before.page);
  });

  it('rejects assigning sort without a page, at compile time', () => {
    const s: CursorListState = { sort: { key: 'a', dir: 'desc' }, page: emptyPage() };
    // @ts-expect-error `sort` is readonly precisely so this line cannot be
    // written: it is the accidental path to a cursor replayed under a sort it
    // was not minted for. If this stops erroring, the guard is gone — and
    // `@ts-expect-error` fails the build when the error it expects disappears,
    // which is what makes this a test rather than a comment.
    s.sort = { key: 'b', dir: 'desc' };
  });

  it('rejects mutating sort.dir in place, at compile time', () => {
    const s: CursorListState = { sort: { key: 'a', dir: 'desc' }, page: emptyPage() };
    // @ts-expect-error `SortState.dir` is readonly precisely so this line
    // cannot be written either: it is the SHORTER accidental path than
    // `sort = ...` above — a plain field mutation that reaches past `sort`'s
    // own readonly-ness. Under Svelte 5 `$state` deep-proxying that mutation
    // is reactive on its own, with no reducer call and so no reset of the
    // page alongside it. If this stops erroring, the guard is gone — and
    // `@ts-expect-error` fails the build when the error it expects disappears,
    // which is what makes this a test rather than a comment.
    s.sort.dir = 'asc';
  });
});

describe('setOffsetSort', () => {
  const onPageThree = { sort: { key: 'last_seen', dir: 'desc' } as const, offset: 100 };

  it('returns to the first page when the sort changes', () => {
    expect(setOffsetSort(onPageThree, 'events', 'desc')).toEqual({
      sort: { key: 'events', dir: 'desc' },
      offset: 0,
    });
  });

  it('returns to the first page when only the direction flips', () => {
    expect(setOffsetSort(onPageThree, 'last_seen', 'desc')).toEqual({
      sort: { key: 'last_seen', dir: 'asc' },
      offset: 0,
    });
  });
});

describe('setCursorPage', () => {
  const onPageTwo: CursorListState = {
    sort: { key: 'last_seen', dir: 'desc' },
    page: advance(emptyPage(), 'c1'),
  };

  it('moves forward to the given cursor', () => {
    const next = setCursorPage(onPageTwo, 'c2');
    expect(next.page).toEqual(advance(advance(emptyPage(), 'c1'), 'c2'));
  });

  it('carries the sort through unchanged, BY REFERENCE, on a successful move', () => {
    // Not just `toEqual`: a reducer that rebuilds `sort` (e.g. `{ ...s.sort }`)
    // instead of reusing it would pass a structural check while still handing
    // Svelte a "changed" object on every unrelated page move.
    expect(setCursorPage(onPageTwo, 'c2').sort).toBe(onPageTwo.sort);
  });

  it('returns a new list-state object on a successful move', () => {
    expect(setCursorPage(onPageTwo, 'c2')).not.toBe(onPageTwo);
  });

  // `advance` refuses three ways (see cursor-page.ts); `null` — no next page —
  // is the one a real Next click can hit, since the button reading
  // `envelope.next_cursor` is what supplies this argument.
  it('refuses when advance refuses, returning the SAME list-state reference', () => {
    // `cursor-page.ts` documents `advance(p, c) !== p` as how a caller detects
    // a real move. A reducer that spreads — `{ ...s, page: advance(s.page, c) }`
    // — would build a new outer object regardless of whether `advance` moved
    // anything, silently erasing that signal at the list-state level. Asserting
    // `toBe` (not `toEqual`) is what catches that: a structurally-equal copy
    // would pass `toEqual` and still be the bug.
    expect(setCursorPage(onPageTwo, null)).toBe(onPageTwo);
  });

  it('does not mutate the state it was given', () => {
    const before = { sort: onPageTwo.sort, page: onPageTwo.page };
    setCursorPage(onPageTwo, 'c2');
    expect(onPageTwo.sort).toBe(before.sort);
    expect(onPageTwo.page).toBe(before.page);
  });
});

describe('cursorBack', () => {
  const onPageTwo: CursorListState = {
    sort: { key: 'last_seen', dir: 'desc' },
    page: advance(emptyPage(), 'c1'),
  };
  const onFirstPage: CursorListState = {
    sort: { key: 'last_seen', dir: 'desc' },
    page: emptyPage(),
  };

  it('moves back one page', () => {
    expect(cursorBack(onPageTwo).page).toEqual(emptyPage());
  });

  it('carries the sort through unchanged, BY REFERENCE, on a successful move', () => {
    expect(cursorBack(onPageTwo).sort).toBe(onPageTwo.sort);
  });

  it('refuses on the first page, returning the SAME list-state reference', () => {
    // `goBack` refuses on the first page the same way `advance` refuses a
    // dead end — by handing back its argument BY REFERENCE — so this reducer
    // has to preserve that the same way `setCursorPage` does.
    expect(cursorBack(onFirstPage)).toBe(onFirstPage);
  });
});

describe('setOffsetPage', () => {
  const onOffset100: OffsetListState = { sort: { key: 'last_seen', dir: 'desc' }, offset: 100 };

  it('moves to the given offset', () => {
    expect(setOffsetPage(onOffset100, 150)).toEqual({
      sort: { key: 'last_seen', dir: 'desc' },
      offset: 150,
    });
  });

  it('carries the sort through unchanged, BY REFERENCE', () => {
    // Unlike the cursor reducers, there is no refusal case here — every
    // offset a Next/Prev click computes is a valid place to be — so the only
    // reference-identity property to pin is the sort half surviving untouched.
    expect(setOffsetPage(onOffset100, 150).sort).toBe(onOffset100.sort);
  });
});
