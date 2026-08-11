# Table sorting slice 1: primitives — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the four shared pieces every later slice consumes — sort state, a client-side row sorter, a sortable table header, and a correct offset pager.

**Architecture:** `DataTable.svelte` is a styling shell whose callers write their own `<th>` markup, so sorting is added as a *sibling* component (`SortableTh`) used inside the existing `head` snippet rather than by rewriting DataTable. Sort state and page state are combined behind a single reducer with `readonly` fields, so changing the sort without resetting the page cannot be written by assignment — awkward and visible rather than impossible, since a deliberate spread still compiles.

**Tech Stack:** Svelte 5 (runes), TypeScript, Vitest.

**Spec:** `docs/superpowers/specs/2026-08-10-table-pagination-and-sorting-design.md`

## Global Constraints

- **Never commit and never create a branch.** This repo's workflow forbids it. Every task ends with a verification step, not a commit. Leave changes in the working tree.
- Sort wire format matches the existing `parse_sort` in `backend/bins/sauron-api/src/routes/search.rs:146`: a **bare column name means descending**, and a **`-` prefix means ascending**. This is the inverse of the common convention and is deliberate; do not "fix" it.
- Use the house UI components (`lib/components/ui/`), never raw `<button>` where an existing component fits. `Icon` is a registry — a name that is not registered renders nothing.
- Svelte 5: `$state` deep-proxies stored values, so `===` never matches on a proxied object. Use `$state.raw` for values compared by identity.
- Dashboard tests run with `npm --prefix dashboard test`; type checking with `npm --prefix dashboard run check`.

---

## File Structure

| File | Responsibility |
|---|---|
| `dashboard/src/lib/models/sort.ts` (create) | `SortDir`, `SortState`, `sortParam`, `toggleSort` — the wire format and the toggle rule |
| `dashboard/src/lib/models/sort.test.ts` (create) | Tests for the above |
| `dashboard/src/lib/models/list-state.ts` (create) | `CursorListState` / `OffsetListState` and their `setSort` reducers — makes "sort change resets paging" structural |
| `dashboard/src/lib/models/list-state.test.ts` (create) | Tests for the above |
| `dashboard/src/lib/sort-rows.ts` (create) | `sortRows` — stable, type-aware client-side ordering for Group C |
| `dashboard/src/lib/sort-rows.test.ts` (create) | Tests for the above |
| `dashboard/src/lib/components/SortableTh.svelte` (create) | The sortable `<th>` |
| `dashboard/src/lib/components/Pagination.svelte` (modify) | `hasNext` becomes a caller-supplied prop; fix the page-5 "No results" text |

---

### Task 1: Sort state and wire format

**Files:**
- Create: `dashboard/src/lib/models/sort.ts`
- Test: `dashboard/src/lib/models/sort.test.ts`

**Interfaces:**
- Consumes: nothing.
- Produces: `type SortDir = 'asc' | 'desc'`; `interface SortState { key: string; dir: SortDir }`;
  `function sortParam(s: SortState): string`;
  `function toggleSort(current: SortState, key: string, columnDefault: SortDir): SortState`.

- [ ] **Step 1: Write the failing tests**

Create `dashboard/src/lib/models/sort.test.ts`:

```ts
import { describe, expect, it } from 'vitest';
import { sortParam, toggleSort, type SortState } from './sort';

describe('sortParam', () => {
  // The backend's `parse_sort` reads a BARE name as descending and a `-`
  // prefix as ascending. Getting this backwards produces a list ordered the
  // wrong way with no error anywhere, so it is pinned here.
  it('writes a descending sort as the bare column name', () => {
    expect(sortParam({ key: 'last_seen', dir: 'desc' })).toBe('last_seen');
  });

  it('writes an ascending sort with a leading dash', () => {
    expect(sortParam({ key: 'last_seen', dir: 'asc' })).toBe('-last_seen');
  });
});

describe('toggleSort', () => {
  const current: SortState = { key: 'last_seen', dir: 'desc' };

  it('selects a new column at that column default direction', () => {
    expect(toggleSort(current, 'times_seen', 'desc')).toEqual({
      key: 'times_seen',
      dir: 'desc',
    });
    expect(toggleSort(current, 'title', 'asc')).toEqual({ key: 'title', dir: 'asc' });
  });

  it('flips direction when the active column is clicked again', () => {
    expect(toggleSort(current, 'last_seen', 'desc')).toEqual({
      key: 'last_seen',
      dir: 'asc',
    });
  });

  it('flips back on a third click rather than clearing the sort', () => {
    const once = toggleSort(current, 'last_seen', 'desc');
    expect(toggleSort(once, 'last_seen', 'desc')).toEqual({
      key: 'last_seen',
      dir: 'desc',
    });
  });

  // The active column FLIPS; it does not re-apply the column default. These
  // coincide whenever the default matches the current direction, so the case
  // is chosen to make them disagree: default `asc` on a column already `asc`
  // must give `desc`. An implementation that returns the default for every
  // click passes every other test in this file and fails this one.
  it('flips the active column even when the default says otherwise', () => {
    expect(toggleSort({ key: 'title', dir: 'asc' }, 'title', 'asc')).toEqual({
      key: 'title',
      dir: 'desc',
    });
  });
});
```

- [ ] **Step 2: Run the tests and verify they fail**

Run: `npm --prefix dashboard test -- sort.test`
Expected: FAIL — `Failed to resolve import "./sort"`.

- [ ] **Step 3: Write the implementation**

Create `dashboard/src/lib/models/sort.ts`:

```ts
/**
 * Which column a list is ordered by, and in which direction.
 *
 * There is deliberately no "unsorted" state. Every list already has a defined
 * default ordering, so returning to it means re-selecting the default column —
 * a third click that clears the sort would leave the table in an order the user
 * cannot name and cannot get back to.
 */
export type SortDir = 'asc' | 'desc';

export interface SortState {
  key: string;
  dir: SortDir;
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
```

- [ ] **Step 4: Run the tests and verify they pass**

Run: `npm --prefix dashboard test -- sort.test`
Expected: PASS, 6 tests.

- [ ] **Step 5: Verify types**

Run: `npm --prefix dashboard run check`
Expected: 0 errors.

---

### Task 2: List state — make "sort resets paging" structural

**Files:**
- Create: `dashboard/src/lib/models/list-state.ts`
- Test: `dashboard/src/lib/models/list-state.test.ts`

**Interfaces:**
- Consumes: `SortState`, `SortDir`, `toggleSort` from Task 1; `CursorPage`, `emptyPage` from `dashboard/src/lib/models/cursor-page.ts`.
- Produces: `interface CursorListState { sort: SortState; page: CursorPage }`;
  `interface OffsetListState { sort: SortState; offset: number }`;
  `function setCursorSort(s: CursorListState, key: string, columnDefault: SortDir): CursorListState`;
  `function setOffsetSort(s: OffsetListState, key: string, columnDefault: SortDir): OffsetListState`.

- [ ] **Step 1: Write the failing tests**

Create `dashboard/src/lib/models/list-state.test.ts`:

```ts
import { describe, expect, it } from 'vitest';
import { advance, emptyPage } from './cursor-page';
import { setCursorSort, setOffsetSort, type CursorListState } from './list-state';

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
```

- [ ] **Step 2: Run the tests and verify they fail**

Run: `npm --prefix dashboard test -- list-state.test`
Expected: FAIL — `Failed to resolve import "./list-state"`.

- [ ] **Step 3: Write the implementation**

Create `dashboard/src/lib/models/list-state.ts`:

```ts
import { emptyPage, type CursorPage } from './cursor-page';
import { toggleSort, type SortDir, type SortState } from './sort';

/**
 * Sort and page position, changed together.
 *
 * They are one type rather than two fields on a page component because
 * changing the sort without resetting the page is a bug worth making hard to
 * write. `setCursorSort` and `setOffsetSort` return both halves, and the fields
 * are `readonly` so `state.sort = toggleSort(...)` — the accidental path — is a
 * type error.
 *
 * Be clear about how far that goes: `readonly` stops assignment, not
 * reconstruction. `{ ...state, sort }` still compiles, and `toggleSort` is
 * still exported for `SortableTh` to use. This makes the bug awkward and
 * visible, NOT impossible, so a reviewer looking at a page that spreads its own
 * list state should check the page came with it.
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

export function setOffsetSort(
  s: OffsetListState,
  key: string,
  columnDefault: SortDir,
): OffsetListState {
  return { sort: toggleSort(s.sort, key, columnDefault), offset: 0 };
}
```

- [ ] **Step 4: Run the tests and verify they pass**

Run: `npm --prefix dashboard test -- list-state.test`
Expected: PASS, 6 tests.

- [ ] **Step 5: Prove the reset assertion bites**

Temporarily change `setCursorSort` to `page: s.page` and re-run. Expected: the
two "restarts the walk" tests FAIL. Restore the correct line and confirm PASS
again. A reset test that passes without the reset is not testing anything.

---

### Task 3: `sortRows` — client-side ordering for Group C

**Files:**
- Create: `dashboard/src/lib/sort-rows.ts`
- Test: `dashboard/src/lib/sort-rows.test.ts`

**Interfaces:**
- Consumes: `SortDir` from Task 1.
- Produces: `type SortValue = string | number | Date | null | undefined`;
  `function sortRows<T>(rows: readonly T[], accessor: (row: T) => SortValue, dir: SortDir): T[]`.

- [ ] **Step 1: Write the failing tests**

Create `dashboard/src/lib/sort-rows.test.ts`:

```ts
import { describe, expect, it } from 'vitest';
import { sortRows } from './sort-rows';

const names = (rows: { name: string }[]) => rows.map((r) => r.name);

describe('sortRows', () => {
  it('orders numbers by value, not lexically', () => {
    const rows = [{ n: 9 }, { n: 100 }, { n: 10 }];
    expect(sortRows(rows, (r) => r.n, 'asc').map((r) => r.n)).toEqual([9, 10, 100]);
    expect(sortRows(rows, (r) => r.n, 'desc').map((r) => r.n)).toEqual([100, 10, 9]);
  });

  it('orders dates chronologically', () => {
    const rows = [
      { name: 'b', at: new Date('2026-02-01T00:00:00Z') },
      { name: 'a', at: new Date('2026-01-01T00:00:00Z') },
    ];
    expect(names(sortRows(rows, (r) => r.at, 'asc'))).toEqual(['a', 'b']);
  });

  it('orders ISO timestamp strings chronologically', () => {
    // Rows carry timestamps as ISO strings straight off the wire far more often
    // than as Date objects, and ISO-8601 sorts correctly as text — but only if
    // it is not run through a locale collator, which reorders punctuation.
    const rows = [
      { name: 'b', at: '2026-02-01T00:00:00Z' },
      { name: 'a', at: '2026-01-01T00:00:00Z' },
    ];
    expect(names(sortRows(rows, (r) => r.at, 'asc'))).toEqual(['a', 'b']);
  });

  it('orders text case-insensitively and in locale order', () => {
    const rows = [{ name: 'banana' }, { name: 'Apple' }, { name: 'cherry' }];
    expect(names(sortRows(rows, (r) => r.name, 'asc'))).toEqual([
      'Apple',
      'banana',
      'cherry',
    ]);
  });

  it('puts nulls last in BOTH directions', () => {
    // A null is an absent value, not a small one. Sorting it as -Infinity puts
    // "never seen" at the top of a "least recently seen" sort, which reads as a
    // real answer and is not one.
    const rows = [{ name: 'a', n: 2 }, { name: 'gap', n: null }, { name: 'b', n: 1 }];
    expect(names(sortRows(rows, (r) => r.n, 'asc'))).toEqual(['b', 'a', 'gap']);
    expect(names(sortRows(rows, (r) => r.n, 'desc'))).toEqual(['a', 'b', 'gap']);
  });

  it('is stable — equal keys keep their input order', () => {
    const rows = [
      { name: 'first', n: 1 },
      { name: 'second', n: 1 },
      { name: 'third', n: 1 },
    ];
    expect(names(sortRows(rows, (r) => r.n, 'desc'))).toEqual([
      'first',
      'second',
      'third',
    ]);
  });

  it('does not mutate the input', () => {
    const rows = [{ n: 2 }, { n: 1 }];
    sortRows(rows, (r) => r.n, 'asc');
    expect(rows.map((r) => r.n)).toEqual([2, 1]);
  });
});
```

- [ ] **Step 2: Run the tests and verify they fail**

Run: `npm --prefix dashboard test -- sort-rows.test`
Expected: FAIL — `Failed to resolve import "./sort-rows"`.

- [ ] **Step 3: Write the implementation**

Create `dashboard/src/lib/sort-rows.ts`:

```ts
import type { SortDir } from './models/sort';

export type SortValue = string | number | Date | null | undefined;

const collator = new Intl.Collator(undefined, { sensitivity: 'base', numeric: true });

/** ISO-8601 with a date and a time — the shape every timestamp on the wire has. */
const ISO = /^\d{4}-\d{2}-\d{2}T/;

function compare(a: SortValue, b: SortValue): number {
  if (a instanceof Date && b instanceof Date) return a.getTime() - b.getTime();
  if (typeof a === 'number' && typeof b === 'number') return a - b;
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
```

- [ ] **Step 4: Run the tests and verify they pass**

Run: `npm --prefix dashboard test -- sort-rows.test`
Expected: PASS, 7 tests.

- [ ] **Step 5: Prove the null-placement test bites**

Temporarily move the three `aEmpty`/`bEmpty` returns to after `sign *` is
applied (i.e. `return sign * 1`). Re-run: the "puts nulls last in BOTH
directions" test must FAIL on the `desc` assertion. Restore and confirm PASS.

---

### Task 4: `SortableTh`

**Files:**
- Create: `dashboard/src/lib/components/SortableTh.svelte`
- Modify: `dashboard/src/lib/components/ui/Icon.svelte` — register `chevron-up`
- Read first: `dashboard/src/lib/components/DataTable.svelte` (the `<th>` styles it applies via `:global`)

**Interfaces:**
- Consumes: `SortState`, `SortDir` from Task 1.
- Produces: a component with props
  `{ key: string; sort: SortState; onsort: (key: string, columnDefault: SortDir) => void; columnDefault?: SortDir; class?: string; children: Snippet }`.

- [ ] **Step 1: Register `chevron-up`**

The registry has `chevron-down`, `chevron-left` and `chevron-right` but **no
`chevron-up`**. An unregistered name renders nothing at all, silently — the
ascending caret would simply never appear, and nothing would report an error.

Add it to `dashboard/src/lib/components/ui/Icon.svelte` in both places, keeping
the alphabetical order both lists already hold:

```svelte
  import ChevronUp from '@lucide/svelte/icons/chevron-up';
```

immediately after the `ChevronRight` import, and

```svelte
    'chevron-up': ChevronUp,
```

immediately after the `'chevron-right'` entry.

Then confirm both names now resolve:

```bash
grep -n "chevron-up\|chevron-down" dashboard/src/lib/components/ui/Icon.svelte
```

Expected: two lines for each — one import, one registry entry.

- [ ] **Step 2: Write the component**

Create `dashboard/src/lib/components/SortableTh.svelte`:

```svelte
<script lang="ts">
  import type { Snippet } from 'svelte';
  import Icon from './ui/Icon.svelte';
  import type { SortDir, SortState } from '../models/sort';

  /**
   * A sortable column header for `DataTable`.
   *
   * Used inside a page's own `head` snippet, beside plain `<th>` elements for
   * columns that do not sort:
   *
   * ```svelte
   * <tr>
   *   <SortableTh key="title" columnDefault="asc" {sort} {onsort}>Issue</SortableTh>
   *   <th>Actions</th>
   * </tr>
   * ```
   *
   * The label goes in a real `<button>` rather than a click handler on the
   * `<th>`, so it is focusable, operable with Enter and Space, and announced as
   * a control — none of which a clickable `<th>` gets.
   *
   * `onsort` is handed the key and the default direction rather than a
   * finished `SortState`, because the page must funnel it through
   * `setCursorSort`/`setOffsetSort` — the reducers that reset paging. Handing
   * over a finished state here would let a page apply the sort and forget the
   * reset, which is the bug those reducers exist to make inexpressible.
   */
  interface Props {
    key: string;
    sort: SortState;
    onsort: (key: string, columnDefault: SortDir) => void;
    /** `desc` suits times and counts; pass `asc` for names. */
    columnDefault?: SortDir;
    class?: string;
    children: Snippet;
  }

  let {
    key,
    sort,
    onsort,
    columnDefault = 'desc',
    class: klass = '',
    children,
  }: Props = $props();

  const active = $derived(sort.key === key);
  // `aria-sort` takes the literal tokens "ascending"/"descending"/"none"; the
  // internal 'asc'/'desc' spelling is not valid there.
  const ariaSort = $derived(
    active ? (sort.dir === 'asc' ? 'ascending' : 'descending') : 'none',
  );
</script>

<th class="sortable {klass}" aria-sort={ariaSort}>
  <button type="button" class="sort-btn" class:active onclick={() => onsort(key, columnDefault)}>
    {@render children()}
    <span class="caret" aria-hidden="true">
      {#if active}
        <Icon name={sort.dir === 'asc' ? 'chevron-up' : 'chevron-down'} size={12} />
      {/if}
    </span>
  </button>
</th>

<style>
  /* DataTable styles `thead th` through :global, so padding is inherited from
     there and must NOT be repeated — the button carries the click target
     instead, stretched to fill the cell. */
  .sortable {
    padding: 0 !important;
  }
  .sort-btn {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    width: 100%;
    padding: 9px 14px;
    background: none;
    border: none;
    font: inherit;
    letter-spacing: inherit;
    text-transform: inherit;
    color: inherit;
    cursor: pointer;
    transition: color 0.12s ease;
  }
  .sort-btn:hover,
  .sort-btn.active {
    color: var(--text);
  }
  .sort-btn:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: -2px;
  }
  /* Reserve the caret's width always, so the header does not shift sideways
     when a column becomes active. */
  .caret {
    display: inline-flex;
    width: 12px;
    flex: none;
  }
  /* A right-aligned numeric column reads wrong with the label pushed left. */
  :global(th.num) .sort-btn {
    justify-content: flex-end;
  }
</style>
```

- [ ] **Step 3: Verify types**

Run: `npm --prefix dashboard run check`
Expected: 0 errors.

- [ ] **Step 4: Verify it renders and toggles**

There is no component-test harness for `.svelte` files in this repo; the house
pattern is a Vite multi-entry harness driven through the browser preview. Build
one at `dashboard/verify-sortable/` with an entry that mounts a `DataTable`
containing three `SortableTh` headers over static rows, wired to `toggleSort`.

Then: `preview_start` the dashboard dev server, navigate to the harness entry,
and confirm with `read_page`:
- each header renders as a `button` inside a `th`;
- the active header carries `aria-sort="descending"`, the others `aria-sort="none"`;
- clicking the active header flips it to `aria-sort="ascending"`;
- clicking a different header moves the `aria-sort` and leaves the others `none`;
- Tab reaches each header button.

Delete the harness directory afterwards.

---

### Task 5: Fix `Pagination.svelte`

**Files:**
- Modify: `dashboard/src/lib/components/Pagination.svelte:4-17`
- Modify (callers): `dashboard/src/pages/ScreensList.svelte`, `dashboard/src/pages/DevicesInventory.svelte`, `dashboard/src/pages/WorkflowsList.svelte`, `dashboard/src/pages/UsersExplorer.svelte`, `dashboard/src/pages/SessionsList.svelte`

**Interfaces:**
- Consumes: nothing.
- Produces: `Pagination` props become
  `{ offset: number; limit: number; count: number; hasNext: boolean; onchange: (offset: number) => void }`.

`hasNext` is now **required**, so every caller fails to type-check until it
supplies one. That is the point: the compiler enumerates the call sites.

- [ ] **Step 1: Change the component**

In `dashboard/src/lib/components/Pagination.svelte`, replace the `Props`
interface and the two derived values:

```svelte
  interface Props {
    offset: number;
    limit: number;
    /** Number of rows on the current page. */
    count: number;
    /**
     * Whether a page exists after this one.
     *
     * Supplied by the caller rather than inferred from `count >= limit`, which
     * was wrong: a final page holding exactly `limit` rows offered an enabled
     * Next that led to an empty page. The caller knows the answer — from a
     * total, or by requesting `limit + 1` rows and rendering `limit`.
     */
    hasNext: boolean;
    onchange: (offset: number) => void;
  }

  let { offset, limit, count, hasNext, onchange }: Props = $props();

  const from = $derived(count === 0 ? 0 : offset + 1);
  const to = $derived(offset + count);
  const hasPrev = $derived(offset > 0);
```

Delete the `hasNext` `$derived` line entirely, and in the markup change the
empty-state text so it does not claim a populated list is empty:

```svelte
  <span class="range muted">
    {#if count === 0 && offset === 0}No results{:else if count === 0}End of results{:else}{from.toLocaleString()}–{to.toLocaleString()}{/if}
  </span>
```

- [ ] **Step 2: Run the type check to enumerate the callers**

Run: `npm --prefix dashboard run check`
Expected: FAIL — one error per `<Pagination …>` usage, reading
`Property 'hasNext' is missing`. Record the list; it is the work for step 3.

- [ ] **Step 3: Supply `hasNext` at each caller**

These five pages currently fetch exactly `limit` rows, so none of them can
answer the question yet — that over-fetch is slice 3's work, task by task,
alongside each endpoint's sort support.

Until then, preserve today's behaviour explicitly rather than silently, by
passing the old expression at each call site with a comment pointing at the
slice that removes it:

```svelte
  <!-- Slice 3 replaces this with a `limit + 1` over-fetch probe. Until then
       this reproduces the old (wrong) inference rather than hiding it: a final
       page of exactly `limit` rows still offers a Next to an empty page. -->
  <Pagination {offset} {limit} count={rows.length} hasNext={rows.length >= limit} {onchange} />
```

- [ ] **Step 4: Verify types**

Run: `npm --prefix dashboard run check`
Expected: 0 errors.

- [ ] **Step 5: Verify the full dashboard suite still passes**

Run: `npm --prefix dashboard test`
Expected: PASS. Record the file and test counts; slice 1 adds 20 tests across
3 new files. Measured after Task 5: 565 tests in 38 files (vitest), 451 files clean under svelte-check.

---

## Done when

- `npm --prefix dashboard test` passes with the 20 new tests included.
- `npm --prefix dashboard run check` reports 0 errors.
- Both sabotage checks (Task 2 step 5, Task 3 step 5) were run and observed to fail.
- Nothing is committed and no branch was created.
