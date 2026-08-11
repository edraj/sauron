# Table pagination and sortable columns

**Date:** 2026-08-10
**Status:** design approved, not yet implemented

Every table view in the dashboard should paginate and let the user sort by
column. Today none of them sort, and 13 of 21 do not paginate.

## The problem

`DataTable.svelte` is a styling shell: the parent supplies its own `<th>` and
`<td>` markup through `head` and `children` snippets, so each screen keeps full
control of its columns. There is consequently no sorting primitive anywhere to
extend — the word "sort" does not appear in the component.

The backend accepts `sort=` on exactly three endpoints, restricted to time
columns. The dashboard's typed client already carries the parameter
(`SearchPageParams.sort`, serialised in `lib/api/search.ts`), and no page has
ever passed it. That is the unreachable-feature shape: backend, wire format,
typed client and documentation all present, zero call sites, every check green.

### Current state

28 `DataTable` instances across 21 files — several pages render more than one.

| Group | Tables | Pagination | Server sort |
|---|---|---|---|
| A | Issues, Occurrences, Events | cursor (keyset) | whitelisted, unwired |
| B | Devices (flat + grouped), Users, Screens, Sessions, Workflows | limit/offset | none |
| C | the remaining 19 | none | none |

Group C: Account sessions, Alerts (channels, rules, history), DeviceDetail
(sessions, performance), Inspector (paths, scans, masks), JourneyExplorer,
MonitorDetail (checks, incidents), Monitors, Performance, Roles, SourceMaps,
Storage (tables, tiering), NotificationSubscriptions.

## Two constraints that decide the design

**Client-side sort on a server-paginated table is a lie.** Clicking a header on
page 1 of 40 reorders 25 rows while presenting itself as having ordered 1,000.
Groups A and B must sort server-side. Group C holds its entire result set in the
browser, so sorting there client-side is complete and correct.

**The cursor does not record which column produced it.** `Cursor { ts, id }` is
a bare `(timestamp, uuid)` tuple. A user on page 3 of Issues sorted by
`last_seen` who switches to `first_seen` sends a cursor that is compared against
a different column, and receives wrong rows with no error. Any sort feature on a
cursor-paged table has to address this before it can be correct.

## Design

### 1. `SortableTh` — the UI primitive

A small component used inside a page's existing `head` snippet:

```svelte
<SortableTh key="last_seen" {sort} onsort={setSort}>Last seen</SortableTh>
```

It renders `<th aria-sort="...">` wrapping a real `<button>` carrying the label
and a direction caret, so keyboard operation and screen-reader announcement live
in one place instead of being hand-rolled 21 times.

Rejected alternative: giving `DataTable` a `columns: Column[]` descriptor and
having it render head and body itself. Cleaner in the abstract, but it
contradicts DataTable's stated contract and rewrites all 21 call sites, whose
cells are bespoke — badges, sparklines, links, symbolication markers. That is a
large, risky diff this feature does not require. `SortableTh` is additive:
pages opt in per column and untouched columns keep working.

### 2. Sort state

```ts
export type SortDir = 'asc' | 'desc';
export interface SortState { key: string; dir: SortDir }
```

Clicking a new column selects that column's natural default direction — `desc`
for times and counts, `asc` for names. Clicking the active column flips it.
There is no third "unsorted" state: every table already has a defined default
order, and returning to it is just re-selecting the default column.

### 3. Generalised cursor

`Cursor` becomes `{ key: String, value: CursorValue, id: Uuid }`, where
`CursorValue` is one of timestamp, integer, float, or text.

This is required for correctness — the key must be in the cursor so a
mismatched one is rejected with 400 instead of silently returning wrong rows.
Generalising `value` at the same time is what makes the feature worth having: a
`(timestamp, uuid)` cursor can only ever sort by time, which on the Issues table
means one sortable column out of six, and not the two (Events, Users) that
people actually want.

Keyset paging over a derived aggregate works — `WHERE (agg.times_seen, i.id) <
($1, $2)` — it simply cannot use an index, and recomputes the aggregate over the
candidate set on each page. That is already what the Issues query does today, so
sorting by a derived column adds ordering cost, not a new class of work.

Encoding stays `<key>|<type>:<value>|<uuid>` base64url. Cursors are ephemeral —
clients echo back what the previous response handed them — so changing the
format needs no migration or version negotiation.

Columns are marked index-backed or scan in the whitelist. Scan sorts are bounded
by the existing candidate cap, so an unindexed sort is slower, never unbounded.

### 4. Group A — Issues, Occurrences, Events

Server-side sort through the existing `SearchPageParams.sort`. Sort changes
clear the cursor in the page reducer; the cursor's embedded key is the
server-side backstop for when that is forgotten.

| Table | Sortable | Not sortable |
|---|---|---|
| Events | Event, User, Session, Time | Properties (JSON) |
| Occurrences | Time, User, Session, Device | — |
| Issues | *deferred — see below* | |

`occurred_at` is index-backed; the rest sort within the already-materialised
candidate set.

#### Issues sorting is deferred to its own slice

`issues::list` orders and pages on the stored columns of `issues`, and then —
whenever an environment is selected — `apply_issue_env_stats` overwrites
`times_seen`, `users_seen`, `first_seen`, `last_seen`, `title`, `culprit` and
`level` on the returned rows with per-environment values. Only `status`
survives. The dashboard auto-selects an environment, so this is most requests.

Sorting under that arrangement orders by one number and displays another.
`last_seen` does this today and gets away with it because the two orderings
correlate; "Events" would not, and would present a count column in visibly no
order.

Events and Occurrences are unaffected: they are event rows, where environment
scoping filters rather than overwrites.

The fix is to make issue statistics environment-aware in the query itself, so
the counts are correct before ordering and the phase-2 patch disappears. That
is a query rewrite larger than this whole feature, so it gets its own spec.
Until then Issues keeps its fixed `last_seen` ordering and its headers stay
plain — an unsorted column is honest, a wrongly-sorted one is not.

**Severity-like enums sort by rank, not alphabetically.** This applies wherever
one appears — Alerts' `critical | warning | info`, an error `level`, an issue
`status`. Ordering any of them as text puts `info` above `warning` and `debug`
above `error`, which reads as a working sort and is not one. Each orders
through an explicit ranking held beside the enum, so adding a value without
ranking it is a type error rather than a silent reshuffle. Client-side that is
`rankOf()`; server-side it would be a `CASE` expression.

### 5. Group B — Devices, Users, Screens, Sessions, Workflows

Each `ListQuery` gains `sort: Option<String>`, parsed by the existing
`search::parse_sort` against a per-endpoint whitelist. The ORDER BY branch for
each column **always appends a unique tiebreaker**. Without one, OFFSET paging
repeats and skips rows whenever the sort column ties — and `last_seen` ties
constantly.

| Table | Sortable | Not sortable |
|---|---|---|
| Devices (flat) | Browser/Arch, Last user, Sessions, Events, Errors, Last seen | Device, OS |
| Devices (grouped) | Device, OS, Devices, Sessions, Events, Errors, Last seen | — |
| Users | User, Sessions, Events, Errors, First seen, Last seen | Traits (JSON) |
| Screens | Screen, Views, Events, Exceptions, Users, Avg dwell | — |
| Sessions | User, Device, Started, Duration, Events, Errors | Session |
| Workflows | Workflow, Started, Completed, Cancelled, Abandoned, Completion rate, Median, p95, Users, Last seen | — |

Two rows above are narrower than the endpoint's whitelist, and the difference
is deliberate in both cases — do not "close the gap" by wiring a header.

**Sessions' `Session` column.** It renders an opaque `session_key` that nobody
orders by, so it is not in `SESSION_SORTS` and `session_id` is pinned as
*refused* by a route test. An earlier draft of this table listed it as
sortable; a header for it would 400 the page.

**The flat Devices table's `Device` and `OS` columns.** The endpoint's
whitelist does accept `family` and `os_name` — `/devices` without `group=1` is
a legal call and they are meaningful there — but the dashboard never makes that
call: `DevicesInventory` renders the flat table only inside a group drill-down,
and a drill-down pins all four descriptor columns with `IS NOT DISTINCT FROM`.
Every row the flat table can render therefore shares one family, model, os_name
and os_version, so those two headers would flip a caret and move no row. They
are plain `<th>` in `DeviceFlatTable.svelte` for the reason stated for Issues
above: an unsorted column is honest, a wrongly-sorted one is not — and a header
that never reorders is closer to the second. They stay sortable on the
**grouped** table, where the descriptors vary. No backend change: the whitelist
is correct for the endpoint, and this is a client affordance.

No new indexes are added. Correctness does not depend on them, and choosing
indexes belongs to a measured performance pass, not to a UI feature. Comments
record which columns sort without index support so the cost is a deliberate
choice rather than an accident.

`sessions::list` returns a bare `Vec<Session>` rather than a `SearchEnvelope`,
so it cannot report a total. That is left as it is; converting it is a separate
change with its own client-side blast radius.

### 6. Group C — client-side sort and client-side pagination

Group C pages load their entire result set. They therefore sort **and** paginate
in the browser: `sortRows()` orders the full array, and the pager slices it.

Sorting and paginating on the same side is the point. If these lists were given
a server-side pager while sorting client-side, every one of them would inherit
exactly the bug this design exists to avoid. No Group C endpoint changes.

`sortRows(rows, accessor, dir)` in `lib/sort.ts`: stable, type-aware — numeric,
temporal, and `localeCompare` for text — with nulls last in both directions,
since a null is an absent value rather than a small one.

Sortable columns are every column holding a scalar. Columns rendering an action
button, a secret, or a list of chips are not sortable and get a plain `<th>`.

New pagers go on the six lists that grow with usage: Monitors, Alerts (channels,
rules, history), SourceMaps, Account sessions, Inspector (paths, scans, masks),
NotificationSubscriptions.

The remainder keep no pager, deliberately: Roles is about five rows, Storage is
one row per database table, and the DeviceDetail, MonitorDetail,
JourneyExplorer and Performance tables are already bounded by their queries. A
pager on a five-row table is noise that implies a page 2 which does not exist.

Known and not fixed: `monitors::list` has no `LIMIT`, so it returns every
monitor in the project. Client-side paging neither causes nor worsens this. If a
project ever holds thousands of monitors the fetch is the problem, not the
pager, and it wants server-side paging then.

### 7. `Pagination.svelte` fix

`hasNext` is `count >= limit`, so a final page holding exactly `limit` rows
offers an enabled Next leading to an empty page. Every Group B table has this
today and the six new Group C pagers would inherit it. The component moves to an
explicit `hasNext` prop supplied by the caller, which knows whether more exists
— from a total, or from the `limit + 1` probe Group A already uses.

Group B has neither a total nor an over-fetch to answer that with, so its pages
request `limit + 1` rows and render `limit`, using the surplus row as the
has-more probe — the same technique Group A already uses, and a change confined
to the page since every one of these endpoints already clamps and honours an
arbitrary `limit`.

Its `count === 0` branch also renders "No results" on page 5 of a list that has
plenty of results, which is corrected in the same change.

## Testing

**Paging stability, per Group B endpoint.** Seed rows that tie on the sort
column, page through the whole set, assert no row appears twice and none is
missed. Then delete the tiebreaker and confirm the test fails — a stability test
that passes without the tiebreaker is not testing anything.

**Cursor key mismatch.** A cursor minted under one sort key, replayed under
another, must produce 400 and not rows.

**Whitelist rejection.** An unlisted sort column produces 400 rather than being
silently ignored, matching `parse_sort`'s existing behaviour.

**`sortRows`.** Ordering per type, nulls last in both directions, and stability
— equal keys retain their input order.

**Sort resets paging.** Changing sort while on page 3 returns to the first page.
Asserted at the reducer, where the rule lives.

**`SortableTh`.** `aria-sort` reflects state; the header is reachable and
operable by keyboard.

## Slices

Each is independently shippable.

1. **Primitives** — `SortableTh`, `lib/sort.ts`, sort state and reducer rule,
   `Pagination.svelte` `hasNext` fix.
2. **Cursor generalisation + Group A** — `(key, value, id)` cursor, key
   mismatch rejection, sort whitelists widened, three pages wired.
3. **Group B** — `sort=` on five endpoints with tiebreakers and stability
   tests, then the five pages.
4. **Group C** — client-side sort on 19 tables, client-side pagers on six.

Slice 1 blocks the rest. Slices 2, 3 and 4 are independent of each other.

## Out of scope

- Multi-column sort. No table needs it and it complicates both cursor and URL.
- Persisting sort to the URL or to saved views. Saved views are their own
  programme; sort belongs in that design, not ahead of it.
- New indexes for Group B sort columns; that is a measured performance pass.
- Converting `sessions::list` to a `SearchEnvelope`.
- Server-side paging for Monitors.
