# Numbered pagination across every table view

**Date:** 2026-08-17
**Status:** design, awaiting review

## Problem

Every table in the dashboard pages one step at a time. The pager renders at most
three page numbers — `current - 1`, `current`, `current + 1` — so a list of
10,000 transactions (200 pages) offers `‹ Prev  1  2  3  Next ›` and no way to
reach page 47 except by clicking Next 45 times.

A second, smaller complaint: the pager bar is not visually separated from the
table above it, and its horizontal padding does not line up with anything.

## Inventory

Nineteen pager instances across three components, with three different answers
to "how many pages are there".

| Component | Instances | Where | Knows the total? | Can jump to page N? |
|---|---|---|---|---|
| `ClientPager` → `Pagination` | 10 | Account, SourceMaps, Inspector ×3, Monitors, Alerts ×3, NotificationSubscriptions | **yes** — whole list is in memory | yes, free |
| `Pagination` (offset) | 5 | Screens, Devices, Users, Workflows, Sessions | **1 of 5** — Sessions only; the other four use a `limit + 1` over-fetch probe | offset math works, page count unknown |
| `CursorPagination` | 4 | Events, Issues, Transactions, IssueDetail occurrences | **yes** — `total`, capped at 10,000 | **no** — keyset only |

So ten instances could be numbered today with no backend work at all; the other
nine each need something from the server, and they need two different things.

## Decisions

### A — the four cursor routes gain a bounded offset jump

`issues::list`, `issues::events`, `analytics::events_list`, `transactions::list`.

Keyset paging stays the mechanism for stepping ±1. Offset is added **only** to
serve an explicit jump to a numbered page.

This partially reverses a deliberate earlier decision. `api/search.ts` currently
states there is no offset because "an offset cannot page a list stably", and
that is true of *walking* a live list — which is why walking is untouched. A
jump is approximate by nature: the user is asking to land near a position in a
result set, not to continue an ordered traversal.

**Why it is affordable.** The offset is clamped to `COUNT_CAP` (10,000), so the
worst case is `OFFSET 9950` — the same row budget `count_transactions` already
spends on every one of these requests today.

**Accepted costs.**

- Two mechanisms behind one control. Clicking `6` and clicking `Next` both reach
  page 6, by different means, with different consistency guarantees. Under
  concurrent insert a jump can repeat or skip a row; a keyset step cannot. This
  is not surfaced to the user.
- `goBack` gets harder. After a jump the cursor stack is empty, so Prev cannot
  pop and must jump too. `cursor-page.ts` documents a "Prev lies" bug at length;
  this reintroduces the second state variable that made that class possible.
  Mitigated by making `page` the authoritative field and deriving everything
  from it (below), plus unit tests over the reducer.
- Pages beyond ~200 are reachable by Next but not numberable, because `total`
  stops at the cap.

**Rejected alternative:** render the full strip with un-walked pages disabled. No
backend change, but a row of dead numbers is worse than the pager we have.

### B2 — the four count-less offset routes get a separate count endpoint

`screens`, `devices`, `persons`, `workflows` learn their total from a **new
sibling endpoint called in parallel**, not from a changed list response.

Two alternatives were weighed:

| | B1 · envelope on the list route | **B2 · parallel count endpoint** | B3 · only where cheap |
|---|---|---|---|
| Shape change | breaking ×4 | none | breaking ×2 |
| Risk to table paint | count is inline; a slow count slows the rows | isolated | inline, safe routes only |
| Raw-SQL duplication in persons/devices | required | required | avoided |
| Round trips | 1 | 2 | 1 |

B2 wins on the one axis that matters most here: the two expensive counts are off
the latency path of the table itself. A slow count delays the page strip, not
the rows.

**Why not B1.** `list_persons` and `list_devices` are hand-written raw SQL with
dynamically computed bind indices. `list_devices` computes
`to_idx = group_base + if group.is_some() { 4 } else { 0 }` and its own comment
warns that getting an index wrong "does not fail loudly; it silently binds the
timestamp into `family`". A count means a second SQL string whose predicate and
bind arithmetic must stay in exact lockstep with the list's, forever. That tax is
the same under B2 — but under B2 it is paid in an isolated function that can be
shipped and measured independently, rather than inline in the route everyone
already depends on.

**Counting is bounded for flat paths, plan-dependent for grouped devices.** The
`count_issues` idiom — `select(id).limit(cap + 1)` so the planner can stop early
— works only when the plan streams rows. Grouped devices aggregate over
family/model/os, and an aggregate must consume its input before emitting unless
Postgres picks a GroupAggregate fed by an ordered index; a HashAggregate cannot
stop early. This is the query behind the earlier device-groups fan-out
regression, so it is gated on measurement rather than assumed (see Gates).

### Result

All 19 instances get real page numbers.

## Backend design

### A · offset on the cursor searches

Add to `IssueSearch`, `OccurrenceSearch`, `EventSearch`, `TransactionSearch`:

```rust
/// Rows to skip before the page. Serves an explicit page JUMP only.
///
/// Ignored whenever `after` is set: keyset is the stable mechanism and stays
/// in charge of stepping. Clamped to `COUNT_CAP` by the route, so the planner
/// never sees an offset larger than the row budget `count_*` already spends.
pub offset: i64,
```

Applied in the repo search functions beside the existing limit:

```rust
let mut q = transaction_query_for(scope, search)?
    .select(Transaction::as_select())
    .limit(search.limit + 1);
if search.after.is_none() && search.offset > 0 {
    q = q.offset(search.offset);
}
```

The `after.is_none()` guard is load bearing and belongs in the repo, not the
route: an offset applied on top of a keyset predicate skips rows *within* the
already-narrowed set, which is a silently wrong page rather than an error.

Route side, in each of the four handlers:

```rust
let offset = q.offset.unwrap_or(0).clamp(0, super::search::COUNT_CAP);
```

`next_cursor` is already minted from the last row of whatever page came back, so
Next-after-jump needs no handler change. `count_*` is unaffected — an offset does
not change a total.

`SearchPageParams` in `api/search.ts` regains an `offset`, and the doc comment
that currently says the backend ignores it is rewritten to state the jump-only
rule and the `after` precedence. `cursor-page.ts`'s module doc gains the same
note, since its argument against offset is now conditional rather than absolute.

### B2 · count endpoints

Four new routes, each accepting exactly the predicate parameters of its list
sibling and none of its page parameters:

```
GET /v1/apps/{app_id}/counts/screens      → routes::screens::count
GET /v1/apps/{app_id}/counts/devices      → routes::devices::count
GET /v1/apps/{app_id}/counts/persons      → routes::analytics::persons_count
GET /v1/apps/{app_id}/counts/workflows    → routes::workflows::count
    → { "total": 1204, "total_is_capped": false }
```

**Why `/counts/{resource}` and not `{resource}/count`.** The obvious nesting
collides on persons. `/v1/apps/{app_id}/persons/{distinct_id}` already exists,
and axum's router resolves a static segment ahead of a `{param}` capture — so
`/persons/count` would always reach the count handler, and any person whose
`distinct_id` is literally `"count"` would lose their profile page. Distinct IDs
come from SDK `identify()` calls and are arbitrary caller strings, so that is
reachable, not pathological. The codebase already dodges this once: devices use
`/device` (singular) for detail precisely to keep `/devices` free. Rather than
repeat that trick unevenly across four resources, all four counts live under one
prefix that has no dynamic sibling.

Same permission as the list route, resolved through the same
`authorized_read_scope_with_perms` call, and the same `environment_id` RawQuery
handling — a count that answers over a wider scope than the list is a disclosure
bug, not a display bug.

Sharing the predicate is the whole risk of this slice. Each `count_*` is written
by extracting the qualifying-set subquery from its list function into a shared
`fn <resource>_qualifying_sql(...) -> String` that both call, so the predicate
and its bind indices exist once. Where that extraction is not clean —
`list_devices` in grouped mode is the likely case — the count SQL carries a
comment naming the list function it must track, and a test asserts the two
return consistent results over a seeded fixture.

The counts drop the LATERAL count columns entirely: a total needs the qualifying
set, not the per-row event/error/session rollups. For flat devices and persons
that alone should make the count substantially cheaper than the list.

## Frontend design

### One strip, three adapters

`Pagination.svelte` and `CursorPagination.svelte` currently carry near-identical
markup and byte-identical CSS. Both are reduced to adapters over a new
presentational component.

```
PageStrip.svelte      pure presentation; knows page/totalPages/busy, emits onjump(n)
  ├── Pagination.svelte        offset adapter   → onchange(offset)
  │     └── ClientPager.svelte in-memory wrapper (unchanged API)
  └── CursorPagination.svelte  cursor adapter   → keyset step or offset jump
```

`PageStrip` knows nothing about offsets or cursors. It receives a page number, a
page count, and a callback taking a page number.

### The window is a pure function

```ts
/** Slots for the number strip: page numbers and gap markers, left to right. */
export function pageWindow(
  page: number,
  totalPages: number,
  siblings = 1,
): Array<number | 'gap'>;
```

Always emits the same number of slots for a given `totalPages` — `2 * siblings + 5`
once `totalPages` exceeds that, every page otherwise. Constant slot count is a
requirement, not an optimisation: a strip that changes width as you page moves
the Next button out from under the cursor, and it only does it in one direction,
so it reads as a control that breaks at random.

```
page 1   of 200 → [1, 2, 3, 4, 5, 'gap', 200]
page 5   of 200 → [1, 'gap', 4, 5, 6, 'gap', 200]
page 198 of 200 → [1, 'gap', 196, 197, 198, 199, 200]
page 3   of 6   → [1, 2, 3, 4, 5, 6]
```

Pure and DOM-free, so it is unit-tested directly. This matters: the repo has a
documented history of green suites that assert nothing, and a windowing bug is
exactly the kind of off-by-one that a rendering test waves through.

### Page count from a capped total

```ts
const totalPages = $derived(
  Math.max(Math.ceil((total ?? 0) / limit), page)
);
```

`Math.max(..., page)` handles walking past the cap with Next: the strip extends
rather than rendering a current page beyond its own last slot. The `+` for a
capped total stays where it is today — in the count text ("10,000+ transactions")
— and is not put on the last button, where `200+` would read as a page number
that does not exist.

### Jump semantics on cursor lists

The adapter routes each click to the cheaper, more consistent mechanism when it
can:

```ts
function onjump(n: number) {
  if (n === page) return;
  if (n === page + 1 && nextCursor) return goNext();  // keyset, stable
  if (n === page - 1 && canGoBack(pos)) return goPrev();
  return goJump(n);                                    // offset
}
```

`CursorPage` gains two fields:

```ts
interface CursorPage {
  stack: string[];
  current: string | null;
  /** Rows to skip. Non-zero only on a page reached by jump. */
  offset: number;
  /** 1-based page. AUTHORITATIVE — no longer derived from stack.length. */
  page: number;
}
```

`page` becoming a stored field rather than `stack.length + 2` is the change that
keeps Prev honest across a jump. `goBack` pops the stack when it can and falls
back to `goJump(page - 1)` when the stack is empty and `page > 1`. `advance`
clears `offset`. `pageNumber()` becomes a field read and its current derivation
is deleted rather than left as a second source of truth.

### Cache keys must include the offset

Every one of these lists is behind `CachedView`, keyed by a `viewKey(...)` tuple.
The offset joins that tuple everywhere it is now possible to send one. Omitting
it means a jump to page 7 can repaint page 1 straight out of the cache with no
request on the wire to notice — the same failure mode as the CachedView
moving-key trap, arrived at from the opposite direction.

## UI

The complaint is spacing, alignment and separation, and the cause is structural
rather than cosmetic: `Card` has a `card-head` with a `border-bottom` and
`14px 18px` padding, but no footer. Pagers are rendered either as the last child
of `card-body` or, on Screens/Devices/Users, entirely outside the Card — so the
same control sits at three different insets depending on the page.

**Add a `footer` snippet to `Card`**, mirroring `card-head`:

```css
.card-foot {
  padding: 12px 18px;
  border-top: 1px solid var(--border);
}
```

Every table view moves its pager into that slot, and the three pages whose table
is not in a Card get wrapped in one. The pager's own `padding: 10px 2px 0` is
removed — its inset then comes from the footer and matches the header by
construction.

Remaining changes inside the pager:

- `min-height` on the bar so it does not change height when `countText` is
  `null` during a page move.
- `font-variant-numeric: tabular-nums` on the number buttons, not only on
  `.range`, so digits do not jitter between pages.
- Gap markers are non-interactive spans at the same width as a number button, so
  the strip's slot grid stays even.
- Below ~640px the number strip is hidden and the bar falls back to
  `‹ Prev · Page 5 of 200 · Next ›`.

## Testing

- `pageWindow` — unit tests over the four shapes above, plus `totalPages` of 0
  and 1, and the invariant that slot count is constant for a fixed `totalPages`.
- `CursorPage` reducer — jump then Prev, jump then Next then Prev, jump to page
  1, Prev at page 1, and the existing refresh-is-not-a-move cases re-run against
  the new `page` field.
- Backend, per route — offset is ignored when a cursor is present; offset beyond
  the cap is clamped, not rejected; `total` is unchanged by an offset.
- Count endpoints — agreement with the list over a seeded fixture at several
  window and search combinations, and permission/environment parity with the
  list route.
- Route precedence — a person whose `distinct_id` is `"count"` still resolves
  through `/v1/apps/{app_id}/persons/{distinct_id}`. This passes trivially under
  the chosen `/counts/{resource}` layout and fails under the nested one, which
  is the point of writing it down.
- The DB-backed tests must be confirmed to have actually executed. Per the
  standing hazard, a sandboxed netns makes these return early while printing
  `ok`; run them with `dangerouslyDisableSandbox` and host-network containers,
  and check the assertion count rather than the exit code.

## Gates before this ships

1. `EXPLAIN (ANALYZE, BUFFERS)` on `analytics_events` at `OFFSET 9950` with a
   realistic window. This is the largest table in the system and the source of
   prior 503s; the clamp bounds the row count but not the plan shape.
2. `EXPLAIN` on `count_devices` in **grouped** mode. If the aggregate cannot stop
   early, that one count either gets a tighter cap or ships without a numbered
   strip; the other three are unaffected either way.
3. Runtime drive of every one of the 19 pagers. Several bug classes here —
   wrong cache key, a jump landing on the wrong page, a strip that renders but
   never fires — pass compile, clippy and the full test suite.

## As built — deltas from the design above

Three things changed during implementation. Each is reflected in the code and
its comments; recorded here so this document is not read as the final state.

**1. A constant slot count is not a constant width.** The design asserted that
`pageWindow`'s fixed slot count kept the strip from moving. It does not, and the
unit test asserting `length === 7` passes either way. Measured in the browser: a
200-page strip was **275.7px on page 1 and 332.95px on page 200**, because a
1-digit button sits on its 32px floor while a 3-digit one grows to 46.31px. The
strip is right-anchored, so Next held still and **Prev slid 57px**. `PageStrip`
now sizes every slot from the widest page number
(`calc(var(--pg-digits) * 1ch + 23px)`, which is why `tabular-nums` on those
buttons is load bearing rather than decorative). Re-measured: **0px drift**,
every slot 47.203px. The 23px rather than 22px is also measured — at 22px a
3-digit label overflowed its own floor by 0.11px and left 0.44px of drift.

**2. The count routes moved to `/v1/apps/{app_id}/counts/{resource}`.** The
nested `{resource}/count` form collides on persons: `/persons/{distinct_id}`
already exists, axum resolves a static segment ahead of a `{param}` capture, and
distinct IDs are arbitrary strings from SDK `identify()` calls — so a person
identified as `count` would permanently lose their profile page.

**3. `goToPage` lives in `cursor-page.ts`, not only in `list-state.ts`.** Issues
drives a bare `CursorPage` rather than a `CursorListState`, so the
keyset-vs-offset decision had to sit one layer lower for both shapes to share
it. `cursorGoTo` is now a thin wrapper.

Also added, not in the design: `RowCount` (`lib/stores/row-count.svelte.ts`),
which keys the count on the predicate alone so paging never refetches it, and a
`pager-harness` Vite target on port 3031 — the only place the width, spacing and
busy-state behaviour can actually be seen without a database.

## Still unverified

The three gates below have NOT been run. Nothing in this change is known to be
slow; nothing is known to be fast either.

1. `EXPLAIN (ANALYZE, BUFFERS)` on `analytics_events` at `OFFSET 9950`.
2. `EXPLAIN` on `count_device_groups` and `count_persons`, both of which wrap
   their list's SQL and so cost roughly what the list costs. `count_devices`
   (flat) and `count_screens` are the two that are genuinely leaner than their
   lists.
3. A runtime drive of all 19 pagers against real data. The harness covers the
   component; it does not cover the wiring, the cache keys, or the count
   endpoints answering over the right scope.

## Out of scope

- Rows-per-page selector and a "jump to page" input. The chosen UI complaint was
  spacing and separation; adding controls is a different ask.
- Raising `COUNT_CAP`.
- Making offset the paging mechanism anywhere. It serves jumps only.
