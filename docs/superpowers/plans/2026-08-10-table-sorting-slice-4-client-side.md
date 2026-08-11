# Table sorting slice 4: client-side sort and pagination — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the 19 tables that hold their entire result set sortable by column, and give the six that grow with usage a pager.

**Architecture:** No backend changes at all. These pages already fetch everything, so `sortRows` orders the full array and the pager slices it — sorting and paging on the same side of the wire, which is what makes both honest. A page that sorted client-side while paging server-side would reorder only the visible rows while presenting itself as having ordered the whole list.

**Tech Stack:** Svelte 5 (runes), TypeScript, Vitest.

**Spec:** `docs/superpowers/specs/2026-08-10-table-pagination-and-sorting-design.md`

**Depends on:** slice 1 (`SortableTh`, `sortRows`, `toggleSort`, the `Pagination` `hasNext` prop). Independent of slices 2 and 3.

## Global Constraints

- **Never commit and never create a branch.** Every task ends with verification, not a commit.
- No endpoint changes. If a table here appears to need one, stop and re-read the spec's Group C section rather than adding one.
- **Sorting must never mutate the source array.** These pages hold their rows in `$state`, and Svelte 5 deep-proxies stored values, so an in-place sort would write through the proxy and reorder the cached data behind the view. `sortRows` returns a new array; keep it that way.
- Columns rendering an action button, a secret, or a list of chips stay plain `<th>`. A header that sorts by "the first chip alphabetically" is worse than one that does not sort.
- Use the house UI components; `Icon` is a registry and an unregistered name renders nothing.
- Dashboard tests: `npm --prefix dashboard test`. Types: `npm --prefix dashboard run check`.

---

## File Structure

| File | Tables | Pager |
|---|---|---|
| `pages/Monitors.svelte` | monitors | yes |
| `pages/Alerts.svelte` | channels, rules, history | yes (all three) |
| `pages/SourceMaps.svelte` | artifacts | yes |
| `pages/Account.svelte` | sessions | yes |
| `pages/Inspector.svelte` | paths, scans, masks | yes (all three) |
| `lib/components/account/NotificationSubscriptions.svelte` | subscriptions | yes |
| `pages/DeviceDetail.svelte` | sessions, performance | no |
| `pages/MonitorDetail.svelte` | checks, incidents | no |
| `pages/JourneyExplorer.svelte` | transitions | no |
| `pages/Performance.svelte` | operations | no |
| `pages/Roles.svelte` | roles | no |
| `pages/Storage.svelte` | tables, tiering | no |
| `lib/components/ClientPager.svelte` (create) | — | the shared slice-and-page control |

---

### Task 1: `ClientPager`

**Files:**
- Create: `dashboard/src/lib/components/ClientPager.svelte`
- Create: `dashboard/src/lib/models/paginate.ts`
- Test: `dashboard/src/lib/models/paginate.test.ts`

**Interfaces:**
- Consumes: `Pagination` (slice 1, with its `hasNext` prop).
- Produces: `function pageSlice<T>(rows: readonly T[], offset: number, limit: number): { rows: T[]; hasNext: boolean }`;
  a `ClientPager` component with props `{ offset: number; limit: number; total: number; onchange: (offset: number) => void }`.

Splitting the arithmetic out of the component is what makes the off-by-one
testable without a browser.

- [ ] **Step 1: Write the failing tests**

Create `dashboard/src/lib/models/paginate.test.ts`:

```ts
import { describe, expect, it } from 'vitest';
import { pageSlice } from './paginate';

const rows = Array.from({ length: 25 }, (_, i) => i);

describe('pageSlice', () => {
  it('returns the requested window', () => {
    expect(pageSlice(rows, 10, 10).rows).toEqual([10, 11, 12, 13, 14, 15, 16, 17, 18, 19]);
  });

  it('reports a next page when rows remain', () => {
    expect(pageSlice(rows, 0, 10).hasNext).toBe(true);
  });

  it('reports no next page on a short final page', () => {
    expect(pageSlice(rows, 20, 10).rows).toEqual([20, 21, 22, 23, 24]);
    expect(pageSlice(rows, 20, 10).hasNext).toBe(false);
  });

  it('reports no next page when the final page is exactly full', () => {
    // The bug the server-side pager had: a last page of exactly `limit` rows
    // offered a Next that led nowhere. Here the total is known, so there is no
    // excuse for guessing.
    const exact = Array.from({ length: 20 }, (_, i) => i);
    expect(pageSlice(exact, 10, 10).hasNext).toBe(false);
  });

  it('returns an empty page past the end rather than throwing', () => {
    expect(pageSlice(rows, 100, 10)).toEqual({ rows: [], hasNext: false });
  });

  it('handles an empty source', () => {
    expect(pageSlice([], 0, 10)).toEqual({ rows: [], hasNext: false });
  });
});
```

- [ ] **Step 2: Run and verify they fail**

Run: `npm --prefix dashboard test -- paginate.test`
Expected: FAIL — `Failed to resolve import "./paginate"`.

- [ ] **Step 3: Implement**

Create `dashboard/src/lib/models/paginate.ts`:

```ts
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
```

- [ ] **Step 4: Run and verify they pass**

Run: `npm --prefix dashboard test -- paginate.test`
Expected: PASS, 6 tests.

- [ ] **Step 5: Write the component**

Create `dashboard/src/lib/components/ClientPager.svelte`:

```svelte
<script lang="ts">
  import Pagination from './Pagination.svelte';

  /**
   * A pager for a list held complete in the browser.
   *
   * Wraps `Pagination` only to compute `hasNext` from a known total, so no
   * caller has to remember that `rows.length >= limit` is the wrong test.
   * The caller slices with `pageSlice` and passes the pre-slice total here.
   */
  interface Props {
    offset: number;
    limit: number;
    /** Rows in the WHOLE list, before slicing. */
    total: number;
    onchange: (offset: number) => void;
  }

  let { offset, limit, total, onchange }: Props = $props();

  const count = $derived(Math.max(0, Math.min(limit, total - offset)));
  const hasNext = $derived(offset + limit < total);
</script>

<Pagination {offset} {limit} {count} {hasNext} {onchange} />
```

- [ ] **Step 6: Verify types**

Run: `npm --prefix dashboard run check`
Expected: 0 errors.

---

### Task 2: The sort-and-page pattern, applied to Monitors

**Files:**
- Modify: `dashboard/src/pages/Monitors.svelte`

Monitors goes first because it needs both halves — sort and a pager — so it
establishes the pattern the remaining tasks follow.

**Columns:** Name (`asc`), Target (`asc`), Status (`asc`), Uptime 24h (`desc`),
Latency (`desc`), Checked (`desc`).

- [ ] **Step 1: Add the state and derivations**

```ts
  import SortableTh from '../lib/components/SortableTh.svelte';
  import ClientPager from '../lib/components/ClientPager.svelte';
  import { pageSlice } from '../lib/models/paginate';
  import { sortRows, type SortValue } from '../lib/models/sort-rows';
  import { toggleSort, type SortDir, type SortState } from '../lib/models/sort';

  const PAGE = 25;

  let sort = $state<SortState>({ key: 'name', dir: 'asc' });
  let offset = $state(0);

  /**
   * How each sortable column reads its value.
   *
   * One place rather than a `switch` inside the comparator, so a column added
   * to the markup without an accessor is a missing key here rather than a
   * header that silently does nothing when clicked.
   */
  const ACCESSORS: Record<string, (m: MonitorListItem) => SortValue> = {
    name: (m) => m.name,
    target: (m) => m.target,
    status: (m) => m.status,
    uptime: (m) => m.uptime_24h,
    latency: (m) => m.latency_ms,
    checked: (m) => m.checked_at,
  };

  const sorted = $derived(sortRows(monitors, ACCESSORS[sort.key] ?? ACCESSORS.name, sort.dir));
  const page = $derived(pageSlice(sorted, offset, PAGE));

  function onsort(key: string, columnDefault: SortDir) {
    sort = toggleSort(sort, key, columnDefault);
    // Sorting reorders the whole list, so the current window no longer shows
    // what the user was looking at. Back to the first page.
    offset = 0;
  }
```

Replace `MonitorRow` with the page's actual row type.

- [ ] **Step 2: Make the headers sortable and render the page**

```svelte
  {#snippet head()}
    <tr>
      <SortableTh key="name" columnDefault="asc" {sort} {onsort}>Name</SortableTh>
      <SortableTh key="target" columnDefault="asc" {sort} {onsort}>Target</SortableTh>
      <SortableTh key="status" columnDefault="asc" {sort} {onsort}>Status</SortableTh>
      <SortableTh key="uptime" {sort} {onsort} class="num">Uptime 24h</SortableTh>
      <SortableTh key="latency" {sort} {onsort} class="num">Latency</SortableTh>
      <SortableTh key="checked" {sort} {onsort}>Checked</SortableTh>
    </tr>
  {/snippet}
```

The row loop iterates `page.rows` instead of `monitors`, and a
`<ClientPager {offset} limit={PAGE} total={sorted.length} onchange={(o) => (offset = o)} />`
goes below the table.

- [ ] **Step 3: Verify types and tests**

Run: `npm --prefix dashboard run check` — expected 0 errors.
Run: `npm --prefix dashboard test` — expected PASS.

- [ ] **Step 4: Verify in the running app**

`preview_start`, open Monitors, and confirm with `read_page`:
- clicking each of the six headers reorders the rows and moves `aria-sort`;
- clicking an active header reverses the order;
- a sort click while on page 2 returns to page 1;
- Next is disabled on the last page;
- **no network request is issued on a sort click** — check `read_network_requests`;
  these lists are sorted in the browser, and a request here means the page is
  re-fetching and the pattern was applied wrongly.

---

### Task 3: The five remaining paginated Group C tables

**Files:**
- Modify: `dashboard/src/pages/Alerts.svelte` (three tables), `dashboard/src/pages/SourceMaps.svelte`, `dashboard/src/pages/Account.svelte`, `dashboard/src/pages/Inspector.svelte` (three tables), `dashboard/src/lib/components/account/NotificationSubscriptions.svelte`

Apply Task 2's pattern to each table below. Pages holding more than one table
get **one `sort` and one `offset` per table** — a shared pair would make sorting
the channels list jump the rules list to page 1.

**Alerts — channels:** Name (`asc`), Type (`asc`), Status (`asc`).
Secret and Actions stay plain.

**Alerts — rules:** Name (`asc`), Trigger (`asc`), Severity (`desc`, by rank —
see Task 5), Throttle (`desc`), Status (`asc`). Channels renders a chip list and
Actions a button; both stay plain.

**Alerts — history:** When (`desc`), Title (`asc`), Channel (`asc`),
Status (`asc`), Attempts (`desc`).

**SourceMaps:** Release (`asc`), File (`asc`), Platform (`asc`), Kind (`asc`),
Size (`desc`), Uploaded (`desc`). The trailing action column stays plain.

**Account — sessions:** Device (`asc`), IP (`asc`), Signed in (`desc`),
Last used (`desc`). The revoke column stays plain.

Account currently sorts through `sortSessions(sessions)` at `Account.svelte:51`.
Keep that function as the *initial* order by seeding
`sort = { key: 'last_used', dir: 'desc' }` to match what it produced, then
delete the call — two orderings applied in sequence is a bug waiting for
someone to change one of them.

**Inspector — paths:** Path (`asc`), Type (`asc`), Matches (`desc`),
Last seen (`desc`).

**Inspector — scans:** Started (`desc`), Finished (`desc`), Status (`asc`),
Rows scanned (`desc`), Findings (`desc`), Coverage (`desc`).

**Inspector — masks:** When (`desc`), Who (`asc`), Targets (`asc`),
Status (`asc`), Rows masked (`desc`), Cold skipped (`desc`), Cancelled by (`asc`).

**NotificationSubscriptions:** Scope (`asc`), Notify about (`asc`),
Environments (`asc`), Delivery (`asc`), Quiet hours (`asc`), State (`asc`).
The trailing action column stays plain.

- [ ] **Step 1: Apply the pattern to Alerts' three tables**
- [ ] **Step 2: Apply it to SourceMaps**
- [ ] **Step 3: Apply it to Account, deleting the `sortSessions` call**
- [ ] **Step 4: Apply it to Inspector's three tables**
- [ ] **Step 5: Apply it to NotificationSubscriptions**

- [ ] **Step 6: Verify types and tests**

Run: `npm --prefix dashboard run check` — expected 0 errors.
Run: `npm --prefix dashboard test` — expected PASS.

- [ ] **Step 7: Verify each in the running app**

For all nine tables, confirm with `read_page` that every sortable header
reorders its own table and leaves its neighbours alone, that each pager is
independent, and that `read_network_requests` shows no request on a sort click.

---

### Task 4: The unpaginated Group C tables

**Files:**
- Modify: `dashboard/src/pages/DeviceDetail.svelte` (two tables), `dashboard/src/pages/MonitorDetail.svelte` (two), `dashboard/src/pages/JourneyExplorer.svelte`, `dashboard/src/pages/Performance.svelte`, `dashboard/src/pages/Roles.svelte`, `dashboard/src/pages/Storage.svelte` (two)

Same pattern **without** `ClientPager` or `offset`: these lists are bounded by
their queries, and a pager on a five-row table implies a page two that does not
exist.

**DeviceDetail — sessions:** Session (`asc`), Started (`desc`),
Duration (`desc`), Events (`desc`), Errors (`desc`).

**DeviceDetail — performance:** Name (`asc`), Op (`asc`), p95 (`desc`),
Count (`desc`).

**MonitorDetail — checks:** Time (`desc`), Result (`asc`), Code (`desc`),
Latency (`desc`). Error renders free text and stays plain.
Delete the fixed `.sort()` at `MonitorDetail.svelte:63` and seed
`sort = { key: 'time', dir: 'desc' }` to reproduce its order.

**MonitorDetail — incidents:** Started (`desc`), Resolved (`desc`),
Duration (`desc`), Cause (`asc`).

**JourneyExplorer:** From (`asc`), To (`asc`), Users (`desc`).
Delete the two fixed `.sort((a, b) => b.count - a.count)` calls at
`JourneyExplorer.svelte:67` and `:73` and seed `{ key: 'users', dir: 'desc' }`.

**Performance:** Name (`asc`), Op (`asc`), Throughput (`desc`), p50 (`desc`),
p95 (`desc`), p99 (`desc`), Avg (`desc`), Error rate (`desc`).

**Roles:** Name (`asc`), Description (`asc`), Permissions (`desc`, by count),
Members (`desc`). The trailing action column stays plain.

**Storage — tables:** Table (`asc`), Size (`desc`), Hot rows (`desc`).

**Storage — tiering:** Org (`asc`), Project (`asc`), App (`asc`),
Hot rows (`desc`), Cold rows (`desc`), Cold bytes (`desc`),
Est. hot bytes (`desc`).

- [ ] **Step 1: Apply the pattern to all nine tables**

For each `.sort()` deleted, seed the sort state to the ordering it produced, so
the page opens looking exactly as it does today.

- [ ] **Step 2: Sort sizes and durations by their underlying number**

Size, Duration, Latency, Coverage and the byte columns render formatted strings
("2.4 MB", "1m 12s"). The accessor must read the **raw numeric field**, not the
formatted label — sorting "2.4 MB" against "13 KB" as text puts the kilobytes
first and reads as a working sort.

- [ ] **Step 3: Verify types and tests**

Run: `npm --prefix dashboard run check` — expected 0 errors.
Run: `npm --prefix dashboard test` — expected PASS.

- [ ] **Step 4: Verify in the running app**

For each table, confirm the formatted columns sort by magnitude — the largest
size really is first — and that the initial order is unchanged from before this
slice.

---

### Task 5: Rank ordering for severity

**Files:**
- Modify: `dashboard/src/lib/models/sort-rows.ts` (add `rankOf`)
- Test: `dashboard/src/lib/models/sort-rows.test.ts`

**Interfaces:**
- Produces: `function rankOf(order: readonly string[]): (value: string | null | undefined) => number`

- [ ] **Step 1: Write the failing test**

```ts
import { rankOf, sortRows } from './sort-rows';

describe('rankOf', () => {
  const severity = rankOf(['fatal', 'error', 'warning', 'info', 'debug']);

  it('orders by rank, not alphabetically', () => {
    const rows = [{ l: 'debug' }, { l: 'fatal' }, { l: 'warning' }];
    // Alphabetically this would be debug, fatal, warning — which reads as a
    // working sort and puts debug above fatal.
    expect(sortRows(rows, (r) => severity(r.l), 'asc').map((r) => r.l)).toEqual([
      'fatal',
      'warning',
      'debug',
    ]);
  });

  it('sorts an unranked value last rather than first', () => {
    // A level the ranking has never heard of must not lead the list; it is
    // unknown, not most severe.
    const rows = [{ l: 'error' }, { l: 'wat' }];
    expect(sortRows(rows, (r) => severity(r.l), 'asc').map((r) => r.l)).toEqual(['error', 'wat']);
  });
});
```

- [ ] **Step 2: Run and verify it fails**

Run: `npm --prefix dashboard test -- sort-rows.test`
Expected: FAIL — `rankOf` is not exported.

- [ ] **Step 3: Implement**

```ts
/**
 * An accessor that orders a small enum by meaning instead of by spelling.
 *
 * Severity is the case that matters: sorting `fatal | error | warning | info |
 * debug` as text puts debug above error and fatal above everything, which looks
 * like a working sort and is not one.
 *
 * An unranked value sorts after every ranked one in ascending order. It is
 * unknown, not extreme, and leading the list with it would be a confident
 * wrong answer.
 */
export function rankOf(
  order: readonly string[],
): (value: string | null | undefined) => number {
  const ranks = new Map(order.map((v, i) => [v, i]));
  return (value) => (value == null ? order.length : ranks.get(value) ?? order.length);
}
```

- [ ] **Step 4: Run and verify it passes**

Run: `npm --prefix dashboard test -- sort-rows.test`
Expected: PASS, 9 tests (7 from slice 1 plus 2).

- [ ] **Step 5: Use it for the Alerts rules Severity column**

`AlertSeverity` is `'info' | 'warning' | 'critical'`
(`dashboard/src/lib/models/index.ts:1055`), so most-severe-first is:

```ts
  import type { AlertSeverity } from '../lib/models';

  // Ordered most severe first, matching the column default of `desc`.
  // Alphabetically this is critical, info, warning — which puts info above
  // warning and looks entirely plausible.
  const SEVERITY: readonly AlertSeverity[] = ['critical', 'warning', 'info'];
  const severity = rankOf(SEVERITY);
```

Typing the list as `readonly AlertSeverity[]` is what keeps it honest: a fourth
severity added to the union makes this array fail to satisfy the type only if
it is also missing, so pair it with a check that `SEVERITY.length` matches the
union's arity if that ever grows beyond three.

Apply the same treatment to any other enum column whose alphabetical order is
not its meaningful one — check Inspector's Status and Alerts' history Status
while you are there.

- [ ] **Step 6: Verify**

Run: `npm --prefix dashboard run check` and `npm --prefix dashboard test`.
Then confirm in the running app that sorting Alerts rules by Severity puts
critical first, not "critical, high, low, medium".

---

## Done when

- All 19 Group C tables sort by every scalar column, and no table issues a network request on a sort click.
- The six unbounded lists have working pagers with Next disabled on the final page.
- Formatted columns (size, duration, latency) sort by magnitude.
- Every fixed `.sort()` deleted was replaced by a seeded sort state producing the same initial order.
- `npm --prefix dashboard test` and `run check` pass.
- Nothing is committed and no branch was created.
