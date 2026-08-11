# Table sorting slice 3: offset-paged endpoints — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add server-side sorting to the five offset-paged lists — Devices (flat and grouped), Users, Screens, Sessions, Workflows — and give their pagers a correct Next button.

**Architecture:** Each list's hardcoded `ORDER BY` becomes a whitelisted `sort=` parameter built from a `&'static str` chosen by a match, never from caller input. Every ordering appends a unique tiebreaker, because OFFSET paging over a non-unique ordering repeats and skips rows. Each page over-fetches by one row to answer "is there a next page" instead of guessing from the row count.

**Tech Stack:** Rust, axum, diesel-async (raw `sql_query` — these are hand-written SQL), Postgres 16, Svelte 5, TypeScript.

**Spec:** `docs/superpowers/specs/2026-08-10-table-pagination-and-sorting-design.md`

**Depends on:** slice 1 (`SortableTh`, `setOffsetSort`, `sortParam`, the `Pagination` `hasNext` prop). Independent of slice 2.

**Slice 1 shipped two things this plan predates — use them.** Move the page
with `setOffsetPage(list, offset)` from `lib/models/list-state.ts`, never with
`{ ...list, offset: o }`. The spread is what the module's own doc comment tells
a reviewer to treat as suspicious, and at this slice's five call sites the
legitimate spreads would outnumber the illegitimate ones and invert the signal.
`SortState`'s fields are also `readonly` now, so `list.sort.dir = 'asc'` is a
type error rather than a reactive mutation that changes the sort while leaving
the offset where it was.

## Global Constraints

- **Never commit and never create a branch.** Every task ends with verification, not a commit.
- Wire format: a bare column name is **descending**, a `-` prefix is **ascending** (`parse_sort`).
- **The sort column must never be interpolated from caller input.** These queries are built with `format!` into `sql_query`, so a column name reaching the string unchecked is SQL injection. `parse_sort` returns the *validated* name; map it through a `match` to a `&'static str` literal and interpolate that. A test asserts this.
- **Every ORDER BY appends a unique tiebreaker.** Without one, OFFSET paging over ties repeats rows on one page and skips them on another. `last_seen` ties constantly.
- **Backend tests silently pass without a database.** Run with `dangerouslyDisableSandbox: true` against host-network containers with `max_connections=800`. A ~1354 total means nothing ran; the real baseline is 1391.
- Do not touch the containers `sauron-postgres-1`, `sauron-redis-1`, `sauron-api-task12`.
- No new indexes. Comment which columns sort without index support so the cost is deliberate.

---

## File Structure

| File | Responsibility |
|---|---|
| `backend/crates/sauron-db/src/repo.rs` (modify) | `SortSpec` helper + `sort:` parameter on `list_devices` (6064), `list_device_groups` (6241), `list_persons` (6529), `screen_list` (8015), `list_sessions` (5630), `workflow_list` (5216) |
| `backend/bins/sauron-api/src/routes/{devices,sessions,screens,workflows}.rs` (modify) | `sort` query param + whitelist |
| `backend/crates/sauron-db/tests/offset_sort.rs` (create) | Paging stability under ties, per list |
| `dashboard/src/lib/api/{devices,sessions,screens,workflows,users}.ts` (modify) | `sort` param, over-fetch by one |
| `dashboard/src/pages/{DevicesInventory,UsersExplorer,ScreensList,SessionsList,WorkflowsList}.svelte` (modify) | `SortableTh`, `setOffsetSort`, real `hasNext` |
| `dashboard/src/lib/components/devices/{DeviceFlatTable,DeviceGroupTable}.svelte` (modify) | `SortableTh` headers |

---

### Task 1: A safe, tiebroken ORDER BY builder

**Files:**
- Modify: `backend/crates/sauron-db/src/repo.rs`
- Test: `backend/crates/sauron-db/src/repo.rs` `#[cfg(test)] mod sort_spec_tests`

**Interfaces:**
- Produces:
  ```rust
  pub struct SortSpec { pub column: &'static str, pub descending: bool, pub tiebreak: &'static str }
  impl SortSpec { pub fn order_by(&self) -> String }
  ```

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod sort_spec_tests {
    use super::SortSpec;

    #[test]
    fn writes_the_direction_and_always_appends_the_tiebreak() {
        let s = SortSpec {
            column: "last_seen",
            descending: true,
            tiebreak: "d.device_key",
            nulls_last: false,
        };
        assert_eq!(s.order_by(), "last_seen DESC, d.device_key ASC");
    }

    #[test]
    fn the_tiebreak_never_reverses() {
        // The tiebreak exists to make the ordering TOTAL, and a total order is
        // total in either direction. Keeping it ASC in both means the two
        // directions are exact reverses of each other row-for-row; flipping it
        // with the sort would leave two tied rows in the same relative order in
        // both directions, so reversing the sort would not reverse the list.
        let s = SortSpec {
            column: "last_seen",
            descending: false,
            tiebreak: "d.device_key",
            nulls_last: false,
        };
        assert_eq!(s.order_by(), "last_seen ASC, d.device_key ASC");
    }

    #[test]
    fn nulls_sort_last_on_a_nullable_column() {
        // Postgres defaults NULLS LAST for ASC and NULLS FIRST for DESC, so a
        // descending sort on a nullable column leads with rows that have no
        // value at all — which reads as "the biggest" and is not. Pinned.
        let s = SortSpec {
            column: "screen",
            descending: true,
            tiebreak: "id",
            nulls_last: true,
        };
        assert_eq!(s.order_by(), "screen DESC NULLS LAST, id ASC");
    }
}
```

- [ ] **Step 2: Run and verify they fail**

Run: `cd backend && cargo test -p sauron-db --lib sort_spec`
Expected: FAIL — `SortSpec` not found.

- [ ] **Step 3: Implement**

```rust
/// A validated ORDER BY.
///
/// `column` and `tiebreak` are `&'static str` rather than `String` on purpose:
/// these queries are assembled with `format!` into `sql_query`, so anything
/// derived from caller input reaching them is SQL injection. A route obtains
/// the validated name from `parse_sort` and then maps it through a `match` to
/// one of these literals, which means the compiler — not a reviewer — is what
/// guarantees no caller string is ever interpolated.
///
/// `tiebreak` must be UNIQUE within the result set. OFFSET paging re-runs the
/// query per page, so two rows tied on `column` with no further ordering may
/// come back in either order on either page: one row appears twice and another
/// never appears. `last_seen` ties constantly.
pub struct SortSpec {
    pub column: &'static str,
    pub descending: bool,
    /// A column, or expression, that is unique across the result set.
    pub tiebreak: &'static str,
    /// True when `column` is nullable, so NULLS LAST is pinned rather than
    /// left to Postgres' direction-dependent default.
    pub nulls_last: bool,
}

impl SortSpec {
    pub fn order_by(&self) -> String {
        let dir = if self.descending { "DESC" } else { "ASC" };
        let nulls = if self.nulls_last { " NULLS LAST" } else { "" };
        format!("{} {dir}{nulls}, {} ASC", self.column, self.tiebreak)
    }
}
```

- [ ] **Step 4: Run and verify they pass**

Run: `cd backend && cargo test -p sauron-db --lib sort_spec`
Expected: PASS, 3 tests.

---

### Task 2: `list_devices` and `list_device_groups`

**Files:**
- Modify: `backend/crates/sauron-db/src/repo.rs:6064` and `:6241`
- Modify: `backend/bins/sauron-api/src/routes/devices.rs:20-26` (`ListQuery`), `:56` (`list`)
- Test: `backend/crates/sauron-db/tests/offset_sort.rs` (create)

**Interfaces:**
- Consumes: `SortSpec` from Task 1.
- Produces: a `sort: SortSpec` parameter on both functions; `pub sort: Option<String>` on the route's `ListQuery`.

Whitelist and tiebreak, flat list (`d` is the devices subquery alias):

| `sort=` | column | tiebreak | nulls_last |
|---|---|---|---|
| `last_seen` (default) | `last_seen` | `d.device_key` | false |
| `family` | `d.family` | `d.device_key` | true |
| `os_name` | `d.os_name` | `d.device_key` | true |
| `browser` | `d.browser` | `d.device_key` | true |
| `distinct_id` | `last_distinct_id` | `d.device_key` | true |
| `sessions_count` | `sessions_count` | `d.device_key` | false |
| `events_count` | `events_count` | `d.device_key` | false |
| `errors_count` | `errors_count` | `d.device_key` | false |

`d.device_key` is the tiebreak throughout because it is unique per device within
an app, which `id` is not for the grouped query. For `list_device_groups` the
tiebreak is the group key — `d.family, d.model, d.os_name, d.os_version`, the
tuple the existing query already orders by and which uniquely identifies a group.

**The ORDER BY must move.** `list_devices` currently orders inside the inner
subquery at `repo.rs:6064` (`ORDER BY last_seen DESC LIMIT $4 OFFSET $5`), which
only works because `last_seen` is a column of `devices`. `sessions_count` and
friends are computed in the OUTER select by lateral joins and are not
addressable there. Move the LIMIT/OFFSET and the ORDER BY to the outer query for
every sort, so one code path serves all columns — and re-read the comment at
`repo.rs:6204-6221` first, which documents why the existing ORDER BY resolves
against output aliases.

- [ ] **Step 1: Write the failing stability test**

Create `backend/crates/sauron-db/tests/offset_sort.rs`:

```rust
//! Paging stability for the offset-paged lists.
//!
//! The defect these exist to catch: OFFSET paging re-runs the query for each
//! page, so an ORDER BY that does not fully determine row order lets Postgres
//! return two tied rows in either order on either page. One row is served
//! twice and another is never served at all — and nothing in the response says
//! so. `last_seen` ties whenever two devices were last seen in the same
//! millisecond, which on a seeded fixture is all of them.

const PAGE: i64 = 7;
const ROWS: usize = 40;

/// Walk every page and assert the union is exactly the seeded set.
async fn assert_pages_cover_every_row(
    label: &str,
    mut fetch: impl AsyncFnMut(i64, i64) -> Vec<Uuid>,
    expected: usize,
) {
    let mut seen = Vec::new();
    for page in 0..20 {
        let rows = fetch(PAGE, page * PAGE).await;
        if rows.is_empty() { break; }
        seen.extend(rows);
    }
    let mut unique = seen.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), seen.len(), "{label}: a row was served on more than one page");
    assert_eq!(seen.len(), expected, "{label}: paging did not reach every row");
}

#[tokio::test]
async fn devices_page_stably_when_last_seen_ties() {
    let Some(mut h) = harness().await else { return };
    // Every device shares one `last_seen`, so the sort column alone decides
    // nothing and only the tiebreak makes the order total.
    seed_devices_all_same_last_seen(&mut h, ROWS).await;
    assert_pages_cover_every_row(
        "devices by last_seen",
        async |limit, offset| device_ids(&mut h, "last_seen", limit, offset).await,
        ROWS,
    )
    .await;
}
```

Write `harness`, `seed_devices_all_same_last_seen` and `device_ids` following
the conventions in `backend/crates/sauron-db/tests/keyset_plan.rs` — including
its `harness()` returning `None` when no database is reachable.

- [ ] **Step 2: Run and verify it fails**

Run (sandbox disabled, containers up):
`cd backend && cargo test -p sauron-db --test offset_sort -- --nocapture`
Expected: FAIL — either a compile error for the new `sort` parameter, or, once
it compiles against the CURRENT untiebroken ORDER BY, a duplicate-row assertion.

- [ ] **Step 3: Implement both functions**

Add `sort: SortSpec` to each signature, move the ORDER BY and LIMIT/OFFSET to
the outer query as described above, and interpolate `sort.order_by()`.

- [ ] **Step 4: Route wiring**

In `devices.rs`, add `pub sort: Option<String>` to `ListQuery` and in `list`:

```rust
    let (sort_col, descending) = super::search::parse_sort(
        q.sort.as_deref(),
        &["last_seen", "family", "os_name", "browser", "distinct_id",
          "sessions_count", "events_count", "errors_count"],
        "last_seen",
    )?;
    // The `&'static str` on the right of each arm is what reaches the SQL.
    // `sort_col` itself never does — see SortSpec's doc comment.
    let sort = match sort_col.as_str() {
        "family" => SortSpec { column: "d.family", descending, tiebreak: "d.device_key", nulls_last: true },
        "os_name" => SortSpec { column: "d.os_name", descending, tiebreak: "d.device_key", nulls_last: true },
        "browser" => SortSpec { column: "d.browser", descending, tiebreak: "d.device_key", nulls_last: true },
        "distinct_id" => SortSpec { column: "last_distinct_id", descending, tiebreak: "d.device_key", nulls_last: true },
        "sessions_count" => SortSpec { column: "sessions_count", descending, tiebreak: "d.device_key", nulls_last: false },
        "events_count" => SortSpec { column: "events_count", descending, tiebreak: "d.device_key", nulls_last: false },
        "errors_count" => SortSpec { column: "errors_count", descending, tiebreak: "d.device_key", nulls_last: false },
        // `parse_sort` refused everything else, so this is the default.
        _ => SortSpec { column: "last_seen", descending, tiebreak: "d.device_key", nulls_last: false },
    };
```

- [ ] **Step 5: Run and verify the test passes**

Run: `cd backend && cargo test -p sauron-db --test offset_sort -- --nocapture`
Expected: PASS.

- [ ] **Step 6: Prove the tiebreak is what makes it pass**

Change `SortSpec::order_by` to omit `, {tiebreak} ASC`. Re-run: the stability
test must FAIL with a duplicate row. Restore and confirm PASS. A stability test
that passes without the tiebreak is testing nothing.

- [ ] **Step 7: Add the injection test**

```rust
#[tokio::test]
async fn a_sort_column_from_the_caller_never_reaches_the_sql() {
    let Some(app) = app().await else { return };
    let res = get(&app, "/v1/apps/{app_id}/devices?sort=last_seen%3B%20DROP%20TABLE%20devices").await;
    assert_eq!(res.status(), 400, "an unlisted sort column must be refused, not interpolated");
    // And the table is still there.
    assert_eq!(get(&app, "/v1/apps/{app_id}/devices").await.status(), 200);
}
```

---

### Task 3: `list_persons`, `screen_list`, `list_sessions`, `workflow_list`

**Files:**
- Modify: `backend/crates/sauron-db/src/repo.rs:6529`, `:8015`, `:5630`, `:5216`
- Modify: `backend/bins/sauron-api/src/routes/{sessions,screens,workflows}.rs`; Users is served from `devices.rs`' neighbours — locate its route with `grep -rn "list_persons" backend/bins`
- Test: `backend/crates/sauron-db/tests/offset_sort.rs`

Repeat Task 2's five steps for each list. Whitelists and tiebreaks:

**Users** (`list_persons`, tiebreak `eu.distinct_id`, unique per app):
`last_seen` (default), `distinct_id`, `first_seen`, `sessions_count`, `events_count`, `errors_count`.

**Screens** (`screen_list`, tiebreak `k.screen`, unique per app):
`views` (default), `screen`, `events`, `exceptions`, `users`, `avg_dwell_ms`.
Its existing `ORDER BY views DESC, k.screen ASC` is already tiebroken; keep that
pairing and let `SortSpec` express it.

**Sessions** (`list_sessions`, tiebreak `id`):
`started_at` (default), `distinct_id`, `device_key`, `duration_ms`,
`events_count`, `errors_count`.

**Workflows** (`workflow_list`, tiebreak `w.name` — the query is `GROUP BY w.name`, so the name is unique in the result):
`started` (default), `name`, `completed`, `cancelled`, `abandoned`,
`completion_rate`, `median_duration_ms`, `p95_duration_ms`, `users`, `last_seen`.

- [ ] **Step 1: One stability test per list**

Four more tests in `offset_sort.rs`, each seeding rows that tie on the default
sort column and asserting full coverage, following
`devices_page_stably_when_last_seen_ties`. Write each out; do not parameterise
them into one loop, because a failure must name which list broke.

- [ ] **Step 2: Run and verify they fail**
- [ ] **Step 3: Implement all four**
- [ ] **Step 4: Run and verify they pass**
- [ ] **Step 5: Remove the tiebreak once more and confirm all five stability tests fail**

Run: `cd backend && cargo test -p sauron-db --test offset_sort`
Expected: 5 failures, then 5 passes after restoring.

- [ ] **Step 6: Run the whole backend suite**

Run (sandbox disabled): `cd backend && cargo test --workspace`
Expected: PASS, total ≥1391 plus the new tests.

---

### Task 4: Over-fetch by one, and wire the five pages

**Files:**
- Modify: `dashboard/src/lib/api/{devices,sessions,screens,workflows}.ts` and the users client
- Modify: `dashboard/src/pages/{DevicesInventory,UsersExplorer,ScreensList,SessionsList,WorkflowsList}.svelte`
- Modify: `dashboard/src/lib/components/devices/{DeviceFlatTable,DeviceGroupTable}.svelte`

**Interfaces:**
- Consumes: `SortableTh`, `setOffsetSort`, `sortParam`, `OffsetListState`, and `Pagination`'s `hasNext` prop from slice 1.

- [ ] **Step 1: Over-fetch in each client**

Each list function requests one more row than it returns, and reports whether
the surplus existed:

```ts
/**
 * One page of devices, plus whether another page follows.
 *
 * Requests `limit + 1` and returns `limit`. The surplus row is the has-more
 * probe: it is the only way to distinguish a final page of exactly `limit`
 * rows from a full one, and guessing `rows.length >= limit` offered a Next
 * button that led to an empty page.
 */
export async function listDevices(
  appId: string,
  opts: { limit: number; offset: number; sort?: string /* …existing… */ },
): Promise<{ rows: Device[]; hasNext: boolean }> {
  const p = new URLSearchParams();
  p.set('limit', String(opts.limit + 1));
  p.set('offset', String(opts.offset));
  if (opts.sort) p.set('sort', opts.sort);
  // …existing params…
  const { data } = await api.get<Device[]>(`/v1/apps/${appId}/devices?${p}`);
  return { rows: data.slice(0, opts.limit), hasNext: data.length > opts.limit };
}
```

Each endpoint clamps `limit` (devices and sessions at 200), so a caller asking
for the page size plus one stays inside the clamp for every page size the UI
offers.

- [ ] **Step 2: Combine sort and offset state in each page**

```ts
  import SortableTh from '../lib/components/SortableTh.svelte';
  import { setOffsetSort, type OffsetListState } from '../lib/models/list-state';
  import { sortParam, type SortDir } from '../lib/models/sort';

  let list = $state<OffsetListState>({ sort: { key: 'last_seen', dir: 'desc' }, offset: 0 });
  let hasNext = $state(false);

  function onsort(key: string, columnDefault: SortDir) {
    list = setOffsetSort(list, key, columnDefault);
  }
```

Requests pass `sort: sortParam(list.sort)` and `offset: list.offset`; the
response's `hasNext` is stored and handed to `Pagination`.

- [ ] **Step 3: Add `sort` to every view-cache key on these pages**

Any `viewKey(...)` on a sorted list gains `sortParam(list.sort)`. Without it a
sort click repaints the previous ordering out of the cache with no request on
the wire.

- [ ] **Step 4: Replace the slice-1 placeholder `hasNext`**

Each `<Pagination …>` loses the `hasNext={rows.length >= limit}` placeholder and
its comment, taking the real value from the client instead.

- [ ] **Step 5: Make the headers sortable**

Per the spec's Group B table. `DeviceFlatTable` and `DeviceGroupTable` take
`sort` and `onsort` as props from `DevicesInventory` and use `SortableTh`; both
device tables sort through the same page state, since only one is visible at a
time.

- [ ] **Step 6: Verify types and tests**

Run: `npm --prefix dashboard run check` — expected 0 errors.
Run: `npm --prefix dashboard test` — expected PASS.

- [ ] **Step 7: Verify in the running app**

`preview_start`, then for each of the five pages confirm with `read_page` and
`read_network_requests`:
- a header click sends `sort=` and resets `offset` to 0;
- the rows come back in the clicked order and `aria-sort` matches;
- paging forward keeps the sort in the query string;
- **on the last page the Next button is disabled** — page to the end and check,
  since this is the bug slice 1 deferred and this task closes;
- no console errors.

---

## Done when

- `cargo test --workspace` passes, sandbox disabled, total ≥1391 plus the new tests.
- All five stability tests were observed to fail with the tiebreak removed.
- The injection test passes.
- `npm --prefix dashboard test` and `run check` pass.
- Next is disabled on the final page of all five lists in the running app.
- Nothing is committed and no branch was created.
