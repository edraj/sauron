import { describe, expect, it } from 'vitest';
import { advance, emptyPage } from './cursor-page';
import {
  cursorBack,
  overFetched,
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

describe('overFetched', () => {
  // The bug this exists to remove: `Pagination` inferred `hasNext` as
  // `rows.length >= limit`, so a final page of exactly `limit` rows offered an
  // enabled Next leading to an empty page. The probe row is the only thing
  // that tells the two apart, which is why this case is first.
  it('reports no next page when the response is exactly full', () => {
    const data = [1, 2, 3];
    expect(overFetched(data, 3)).toEqual({ rows: [1, 2, 3], hasNext: false });
  });

  it('reports a next page only when the surplus probe row came back', () => {
    expect(overFetched([1, 2, 3, 4], 3)).toEqual({ rows: [1, 2, 3], hasNext: true });
  });

  it('drops the probe row from the rendered page', () => {
    // Rendering `limit + 1` rows is the other half of the same mistake: the
    // page would show one row that also opens the next page.
    expect(overFetched([1, 2, 3, 4], 3).rows).toHaveLength(3);
  });

  it('handles a short page and an empty one', () => {
    expect(overFetched([1, 2], 3)).toEqual({ rows: [1, 2], hasNext: false });
    expect(overFetched([], 3)).toEqual({ rows: [], hasNext: false });
  });

  it('never reports a next page when the server ignored the over-fetch', () => {
    // A limit clamped below `limit + 1` server-side returns exactly `limit`
    // rows on a full page. That reads as a last page — wrong, but the safe
    // direction, and the reason every caller must stay inside the clamp.
    expect(overFetched([1, 2, 3], 3).hasNext).toBe(false);
  });
});

/**
 * The invariant the test above describes but cannot enforce.
 *
 * Every offset-paged page asks the server for `LIMIT + 1` rows and hands
 * `overFetched` the un-incremented `LIMIT`. All five Group B endpoints clamp
 * `limit` with `.clamp(1, 200)`, so `LIMIT` must stay at 199 or below. Raise
 * any page's constant to 200 and the request for 201 is clamped to 200,
 * `data.length > limit` is false on every full page, and **Next goes dead
 * permanently, with no error anywhere** — the exact silent shape the
 * over-fetch was introduced to remove, reintroduced by a one-character edit.
 *
 * It was documented three times (`overFetched`, `listDevices`, `listSessions`)
 * and enforced nowhere.
 *
 * This reads the page components' SOURCE rather than importing a shared
 * constant, because there is no shared constant: each page declares its own
 * `const LIMIT` inside its `<script>` block, which no test can import. A copy
 * of the number here would be a fourth place to forget it.
 *
 * `import.meta.glob(..., { query: '?raw' })` rather than `node:fs`: the
 * dashboard has no `@types/node`, so a `readFileSync` version type-checks
 * nowhere and `npm run check` — which gates the build — fails on the import
 * alone. Vite resolves these at transform time, so the set is also fixed at
 * build time and cannot silently come back empty at runtime.
 */
describe('the limit + 1 over-fetch stays inside the server clamp', () => {
  // `q.limit.clamp(1, 200)` in routes/{devices,analytics,screens,sessions,
  // workflows}.rs. Duplicated across the language boundary because a Rust
  // const cannot be imported here; if the backend ever lowers a clamp, this
  // number is what has to follow it.
  const SERVER_CLAMP = 200;

  /** The five offset-paged pages. Keys are as `import.meta.glob` spells them. */
  const PAGES = [
    '../../pages/DevicesInventory.svelte', // both the flat and the grouped list
    '../../pages/ScreensList.svelte',
    '../../pages/UsersExplorer.svelte',
    '../../pages/WorkflowsList.svelte',
  ];

  const pageSources = import.meta.glob('../../pages/*.svelte', {
    query: '?raw',
    import: 'default',
    eager: true,
  }) as Record<string, string>;

  const apiSources = import.meta.glob('../api/*.ts', {
    query: '?raw',
    import: 'default',
    eager: true,
  }) as Record<string, string>;

  it.each(PAGES)('%s asks for at most the clamp', (page) => {
    const source = pageSources[page];
    expect(source, `${page} was not found by the glob`).toBeTypeOf('string');
    const declarations = [...source.matchAll(/\bconst LIMIT\s*=\s*(\d+)\s*;/g)];
    // A page that stopped declaring `LIMIT` has been restructured and this
    // guard no longer covers it — fail rather than vacuously pass.
    expect(declarations.length, `${page} declares no \`const LIMIT\``).toBeGreaterThan(0);
    for (const [, digits] of declarations) {
      const limit = Number(digits);
      expect(limit, `${page}: LIMIT must be at least 1`).toBeGreaterThan(0);
      expect(
        limit + 1,
        `${page}: LIMIT is ${limit}, so the request asks for ${limit + 1}, which the ` +
          `server clamps to ${SERVER_CLAMP} — overFetched then never sees the probe row ` +
          `and Next is disabled forever with no error`,
      ).toBeLessThanOrEqual(SERVER_CLAMP);
    }
  });

  it('covers every page that over-fetches', () => {
    // `PAGES` is hand-written, so it can go stale, and a page it misses is a
    // page this guard silently does not protect. Rediscover the set from the
    // wiring instead: an over-fetching client is an exported function whose
    // body calls `overFetched`, and an over-fetching PAGE is one that names
    // such a function. A sixth offset-paged list therefore fails here until it
    // is listed above — it cannot slip in unchecked.
    const overFetchers = Object.values(apiSources).flatMap((source) =>
      source
        .split('export async function ')
        .slice(1)
        .filter((body) => body.includes('overFetched('))
        .map((body) => body.slice(0, body.indexOf('('))),
    );
    // If this ever empties, the discovery above broke and the comparison
    // below becomes vacuous.
    expect(overFetchers.length, 'no over-fetching API client found').toBeGreaterThan(0);

    const found = Object.entries(pageSources)
      .filter(([, source]) => overFetchers.some((fn) => new RegExp(`\\b${fn}\\b`).test(source)))
      .map(([path]) => path)
      .sort();
    expect(found).toEqual([...PAGES].sort());
  });
});
