# Grouping the Devices inventory by model and OS

Date: 2026-08-09
Status: approved, not yet implemented

## Problem

`/devices` lists one row per physical device. A fleet of five hundred iPhone 15s
on iOS 17.4.1 is five hundred near-identical rows, and the page's real question —
*which hardware/OS combinations do my users run, and which of them crash* — is
unanswerable without scrolling past the duplicates.

The list is paginated at 50 rows
(`dashboard/src/pages/DevicesInventory.svelte`, `LIMIT = 50`) over
`repo::list_devices` (`backend/crates/sauron-db/src/repo.rs:5652`), which pages
`devices` by `last_seen DESC` and then counts per returned device via LATERAL
subqueries.

## Decision

One row per `(family, model, os_name, os_version)` tuple, aggregated
**server-side**, with a drill-down to the flat list filtered to that tuple.

### Locked decisions

1. **Grouping happens in SQL, not in the Svelte page.** Collapsing the 50 rows
   the page already holds would dedupe only *within* a page: the same model/OS
   pair reappears on page 2, and the rendered row count per page becomes
   unpredictable. Paging must apply to groups, which only the database can do.
2. **The OS key is `os_name` + the full `os_version`.** `iOS 17.4.1` and
   `iOS 17.4.0` stay separate rows. This is exactly what the OS column renders
   today, so no on-screen value changes meaning. Major-version folding
   (`iOS 17`) was rejected: it hides patch-level crash differences, which is
   most of the reason to look at this table.
3. **A grouped row navigates to a filtered device list**, rather than expanding
   inline, so each group has a linkable URL.
4. **`browser` and `arch` are not part of the grouping key.** Every browser on
   Windows 11 folds into one row. This follows from "group by device and OS";
   the drill-down still separates them, and both columns survive there.

### Alternatives rejected

- **Client-side grouping.** See locked decision 1.
- **A `/devices/group` route.** It collides with the existing `/devices/:key`
  for a device whose key is literally `group`. Device keys are arbitrary
  strings, so this is a real collision, not a theoretical one. Querystring
  modes on `/devices` avoid it entirely and match the pattern Issues and
  Events already use.
- **Reworking `list_devices` to return either shape.** Two response types
  behind one endpoint, switched by a flag, is worse to type on the client and
  worse to test. The flat list has to keep existing for the drill-down anyway.

## Backend

### `DeviceGroupRow`

A new `QueryableByName` struct in `repo.rs`, sibling to `DeviceRow`:

```rust
pub struct DeviceGroupRow {
    family: Option<String>,
    model: Option<String>,
    os_name: Option<String>,
    os_version: Option<String>,
    device_count: i64,
    events_count: i64,
    errors_count: i64,
    sessions_count: i64,
    first_seen: DateTime<Utc>,
    last_seen: DateTime<Utc>,
}
```

`last_distinct_id` is deliberately absent: it is a per-device value with no
meaningful aggregate over a group, and reproducing it would mean carrying
`device_last_distinct_id_join`'s disclosure-vector reasoning
(`repo.rs:5629`) into a query that has no use for the result.

### `repo::list_device_groups`

Same signature as `list_devices` — `(conn, scope, since, limit, offset, search)`.

The qualifying-devices subquery is reused verbatim from `list_devices`: `app_id
= $1`, `last_seen >= $2`, the `like_contains`-escaped search `ILIKE $3`, and the
`membership_sql` EXISTS legs over `analytics_events` / `error_events` /
`sessions`. Its `ORDER BY ... LIMIT/OFFSET` is dropped — every qualifying device
must be visible to the aggregate — and `LIMIT $4 OFFSET $5` moves to the outer
query, after `GROUP BY`:

```sql
SELECT d.family, d.model, d.os_name, d.os_version,
       count(*)::bigint            AS device_count,
       <summed scoped_select>,
       COALESCE(sum(se.cnt), 0)::bigint AS sessions_count
FROM ( <qualifying devices, unpaged> ) d
<scoped_join>
LEFT JOIN LATERAL ( ... ) se ON TRUE
GROUP BY d.family, d.model, d.os_name, d.os_version
ORDER BY last_seen DESC
LIMIT $4 OFFSET $5
```

**`ORDER BY last_seen`, referring to the output column, not `max(d.last_seen)`.**
The two are the same thing only under `EnvFilter::All`. Under `One` /
`Unattributed` the selected `last_seen` is `max(GREATEST(ae.max_occurred,
ee.max_occurred, se.max_last_event))` — a derived per-environment value — while
`d.last_seen` is the app-wide column, which can be newer because of activity in
an environment this scope cannot see. Ordering by the raw column would sort the
scoped page by numbers it does not display. Postgres resolves a bare `ORDER BY`
name against the select list's output aliases, so one spelling is correct in
both branches.

**`device_last_distinct_id_join` is not emitted here.** `scoped_join` must be
reused minus that LATERAL, since `DeviceGroupRow` has no `last_distinct_id`.
This is a cost decision, not tidiness: that join is a three-way `UNION ALL` with
`ORDER BY occurred_at DESC LIMIT 1` per device, and unlike `list_devices` this
query runs its joins over every qualifying device rather than 50.

Three further properties carried over deliberately:

**The `All`-vs-scoped source split is preserved exactly.** Under
`EnvFilter::All` the aggregate sums the durable `d.events_count` /
`d.errors_count` columns and takes `min(d.first_seen)` / `max(d.last_seen)`
straight off the row; under `One` / `Unattributed` it sums the `ae` / `ee`
LATERALs and derives the timestamps from them. The tiering blind spot that
`list_devices`' doc comment records — scoped counts cannot see partitions
`sauron-tier` has exported to Parquet and dropped — is inherited unchanged, not
newly introduced and not fixed here.

**`sessions_count` stays a LATERAL under every variant,** because `devices` was
never denormalized for it. Its `count(*) FILTER (WHERE started_at >= $2)` bound
is unchanged.

**NULL grouping is the desired behaviour.** Postgres `GROUP BY` treats NULLs as
equal, so devices reporting no model collapse into a single "Unknown device"
row rather than scattering into singletons.

### Cost

The count LATERALs now run for every qualifying device in the window, not just
the 50 on screen. The lookups are covered — `sessions_app_device_started_idx`
on `sessions (app_id, device_key, started_at DESC)`, and
`analytics_events_app_device_idx` / `error_events_app_device_idx` on
`(app_id, device_key)` — so each is an index probe. It is still strictly more
work per request than today's page-then-count, and that is the accepted price
of paging over groups instead of devices.

### Drill-down filter on `list_devices`

`list_devices` gains four optional exact-match parameters — `family`, `model`,
`os_name`, `os_version` — applied with `IS NOT DISTINCT FROM`, so a group whose
model is NULL drills down to its members instead of matching nothing.

Those four cannot distinguish "do not filter on model" from "filter to model IS
NULL", because both arrive as an absent query parameter. A `group=1` sentinel
resolves it:

- `group=1` present → all four predicates apply, and an absent parameter means
  SQL `NULL`.
- `group=1` absent → no filtering. Today's behaviour, byte for byte.

### Route

`GET /v1/apps/{app_id}/device-groups`, in
`backend/bins/sauron-api/src/routes/devices.rs`, with the same
`authorized_read_scope` + `perm::EVENT_READ` + `RawQuery` handling as `list`.
`environment_id` is read from the raw query string, never as a `Query<T>` field
— see that module's existing comment for the extractor trap this avoids.
`since_days` clamps to `1..=365`, `limit` to `1..=200`, offset through
`clamp_offset`, matching `list`.

## Frontend

### Two modes on one route

`/devices` stays a single route with two modes driven by the querystring,
hydrated once at init from `$querystring` and synced back with `replace()` —
the pattern `Issues.svelte:44` and `Events.svelte:33` already use.

**Default (no `group` param) — grouped table.** Columns: Device · OS ·
**Devices** · Sessions · Events · Errors · Last seen. `Last user` and
`Browser / Arch` are dropped; neither aggregates meaningfully across a group.
Clicking a row pushes:

```
/devices?group=1&family=…&model=…&os_name=…&os_version=…
```

with NULL components omitted from the querystring.

Absent and empty are distinct on the wire: the encoder omits a NULL component
entirely, and the decoder maps an absent parameter to NULL and a present one —
including `os_version=` — to that exact string. A device whose stored
`os_version` is `''` therefore round-trips to `IS NOT DISTINCT FROM ''` and
matches itself, rather than silently colliding with the NULL group.

**`group=1` present — the flat table, filtered.** Identical to today's table,
including `Last user` and `Browser / Arch` and the existing row click through
to `/devices/:key`, plus a chip naming the group and a link back to the grouped
view.

Both modes keep `DateRange`, `SearchInput`, `Pagination` and `RefreshButton`,
and both keep the `CachedView` stale-while-revalidate wiring. `sessionStore.scopeKey`
stays in the `viewKey` for both — it carries the selected environment, which the
axios interceptor adds to the request but which appears in none of the
arguments; omitting it serves one environment's rows as another's.

### Components

`DevicesInventory.svelte` is 251 lines today. Rather than grow it into a
two-headed 400-line file, the two table bodies move to
`lib/components/devices/DeviceGroupTable.svelte` and
`lib/components/devices/DeviceFlatTable.svelte`. The page keeps mode selection,
loading/error/empty states, and data fetching; each table component takes rows
and renders them.

The group-key querystring encode/decode is a pair of pure functions in
`lib/models/device-groups.ts`, so the NULL round-trip is unit-testable without
a component.

### API client

`lib/api/devices.ts` gains `listDeviceGroups(appId, params)` and four optional
filter fields plus `group` on `ListDevicesParams`. `DeviceGroupRow` is added to
`lib/models/index.ts` beside `DeviceRow`.

## Testing

**`sauron-db` integration tests** (alongside the existing device coverage in
`backend/crates/sauron-db/tests/`):

- Two devices sharing `(family, model, os_name, os_version)` collapse to one
  row with `device_count = 2` and summed event/error/session counts.
- Devices with NULL `model` and NULL `os_name` group together into one row.
- A device differing only in `os_version` forms its own row (locked decision 2).
- Under `EnvFilter::One(env_a)`, a group's counts exclude a device active only
  in `env_b`, and a device whose only `env_a` session predates `since` does not
  produce an all-zero row — the same membership property `list_devices` already
  guarantees.
- `list_devices` with `group=1` returns exactly the members of a group,
  including when the group's `model` is NULL.
- `list_devices` without `group=1` returns the same rows it does today.

**HTTP test** (`backend/bins/sauron-api/tests/`): `device-groups` requires
`perm::EVENT_READ` and honours `environment_id` from the query string.

**Dashboard unit test**: the group-key encode/decode round-trip in
`lib/models/device-groups.test.ts`, NULL components included.

## Out of scope

- Changing `DeviceDetail` (`/devices/:key`). Untouched.
- Backfilling or normalizing `family` / `model` / `os_name` strings. Groups are
  formed from the values as stored; two spellings of the same model stay two
  groups.
- The tiering blind spot on environment-scoped counts. Inherited from
  `list_devices`, documented there, not addressed here.
