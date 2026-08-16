# List Time Filtering Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the Events, Sessions, Users and Devices lists a time filter that can express a range, a lower bound, or an upper bound, over a caller-chosen timestamp column.

**Architecture:** Three new query parameters (`time_field`, `from`, `to`) resolved by one shared function in `routes/search.rs`, carried to the repo layer as a `TimeWindow` struct whose column is a whitelisted `&'static str` (never caller text). `since_days` keeps working unchanged and is ignored when `from`/`to` are present. On the frontend, one `TimeFilter.svelte` control backed by a pure `time-filter.ts` model replaces `DateRange` on those four pages.

**Tech Stack:** Rust / axum / diesel-async / Postgres; Svelte 5 (runes) / TypeScript / Vitest.

**Spec:** `docs/superpowers/specs/2026-08-16-list-time-filtering-design.md`

## STATUS: COMPLETE (uncommitted)

Backend **1842 passed / 0 failed / 4 ignored**, `clippy -D warnings` clean.
Frontend **896 passed / 62 files**, `svelte-check` 0 errors. Verified end to end
against the real API + real Postgres, not only in tests.

**Four bugs that passed compile, clippy and the whole suite, and were caught
only by going one layer further out. All fixed.**

1. **`#[serde(flatten)]` + `serde_urlencoded` breaks integers.** Flatten forces
   values through `deserialize_any`, which for a urlencoded body yields a
   *string*, so `Option<i64>` answered `invalid type: string "7", expected i64`.
   Every route would have 400'd on `?since_days=`, i.e. on every existing
   bookmark and the dashboard's own default request. Fixed with
   `opt_i64_from_str_or_int`; pinned by
   `flattened_window_params_survive_the_query_extractor`, which tests the
   EXTRACTOR rather than the resolver. Strings and timestamps were unaffected,
   which is why only the one field failed.
2. **A clamp whose trigger is also its own default can never fire — twice.**
   `to` with no `from` substitutes `to - max_days`, which IS the floor, so
   `from < floor` was false and the narrowing went undisclosed. The identical
   shape then appeared for an oversized `since_days`: 3650 was served as 365
   under `clamped: null`, a regression against `resolve_window`, whose doc
   comment calls out exactly that case. Both need their own flag.
3. **`eu.` inside the subquery that `eu` names.** The persons live shape under
   `EnvFilter::All` put the predicate in the inner subquery using
   `person_seen_expr`'s `eu.first_seen` — but `eu` is the alias the OUTER query
   gives that subquery, so Postgres answered `missing FROM-clause entry for
   table "eu"`. The string composes perfectly; only a DB-backed test sees it.
4. **The test harness reads `TEST_DATABASE_URL`, not `DATABASE_URL`.** An
   8-test DB suite reported `ok` in 0.00s while executing nothing. `0.00s` for
   DB-backed tests is the tell.

## Global Constraints

- **Never commit and never create branches.** Work stays uncommitted in the working tree. Strip any commit step you see in a sub-skill's template.
- The interval is **half-open**: `from <= col < to`. `from` is always resolved; `to` is optional.
- `time_field` is validated by **equality against a per-route whitelist**, and the resolved value is the whitelist's `&'static str`. Caller text never reaches SQL construction.
- Total span is clamped to each route's `max_days` (**365** on all four). A clamp is reported through the existing `clamped: ClampInfo` envelope field, never applied silently.
- `count_*` takes the **same** window as its `search_*` counterpart.
- Backend tests only count if run with `dangerouslyDisableSandbox`, host-network containers and `max_connections=800` — the Bash sandbox has its own netns and DB-backed tests return early while printing `ok`. Baseline before this work: capture it in Task 0 and compare against it, never against a remembered number.
- Route tests use a **two-app fixture**. A single-app fixture returns identical rows whether or not a predicate is scoped.

## Two corrections to the spec, found while reading the code

Both are recorded here rather than silently implemented.

1. **Sessions already windows on the wrong column relative to its own disclosure.**
   `sessions.rs:151` calls `resolve_window("started_at", …)`, but
   `repo.rs:4118` filters `sessions::last_event_at.ge(search.since)`. The
   envelope names `started_at` while the predicate uses `last_event_at`.
   Task 7 fixes the disclosure by making the column explicit and passing the
   resolved one through, so the two can no longer disagree.

2. **Devices and Persons take the window on *different* expressions, deliberately.**
   - **Devices** applies it to the durable `devices.first_seen`/`devices.last_seen`
     inside the paging subquery — indexable, and it preserves the semantics the
     code already documents (`since` decides *which devices are listed*; a
     device's per-environment extremum can predate the window, repo.rs ~7159).
   - **Persons** applies it to the **displayed** expression: `eu.first_seen` under
     `EnvFilter::All`, `r.first_seen` in the rollup shape, and the
     `LEAST`/`GREATEST` expression in the live fallback shape. Persons has no
     pre-existing `since` convention to preserve, and "users last seen in the
     last 7 days" must mean what the Last seen column shows. The rollup path is
     indexed for both columns (`event_user_env_{first,last}_seen_idx`); the live
     fallback is not, and is already slow by documented design.

   Do not "unify" these. They are different questions with the same name.

3. **The Users page default changes visibly.** That page's picker has never
   filtered the table, so it shows all persons today. Its default becomes
   `last 365d` — the route ceiling — so the default view is as close to
   unchanged as an honest window allows, rather than the 30d the other pages
   use. The preset list gains a `365d` ("1y") entry for this.

---

## File Structure

**Backend — created**
- `backend/migrations/2026-08-16-000062_devices_first_seen_index/{up,down}.sql` — the two Devices indexes.

**Backend — modified**
- `bins/sauron-api/src/routes/search.rs` — `TimeFilterQuery`, `TimeWindowSpec`, `resolve_time_filter`, unit tests.
- `bins/sauron-api/src/routes/devices.rs` — `ListQuery` gains the three params; both handlers resolve and pass a window.
- `bins/sauron-api/src/routes/analytics.rs` — `PersonsQuery` gains them; `EventsListQuery` gains `to`.
- `bins/sauron-api/src/routes/sessions.rs` — `ListQuery` gains them.
- `crates/sauron-db/src/repo.rs` — `TimeWindow` + per-resource column enums; `list_devices`, `list_device_groups`, `list_persons` (both SQL shapes each), `SessionSearch`, `EventSearch`.

**Backend — new tests**
- `bins/sauron-api/tests/http_time_filter.rs` — one module per route, two-app fixture.

**Frontend — created**
- `dashboard/src/lib/models/time-filter.ts` + `time-filter.test.ts`
- `dashboard/src/lib/components/TimeFilter.svelte` + `TimeFilter.test.ts`

**Frontend — modified**
- `pages/Events.svelte`, `pages/SessionsList.svelte`, `pages/UsersExplorer.svelte`, `pages/DevicesInventory.svelte`
- `lib/components/filters/FilterBar.svelte` — swaps its embedded `DateRange` for `TimeFilter`
- `lib/api/{sessions,devices,users,events,search}.ts` — param plumbing
- `wiki/Dashboard.md`, `pages/Docs.svelte`

---

### Task 0: Capture the baseline

**Files:** none modified.

- [ ] **Step 1: Record the real backend test count**

```bash
cd backend && cargo test --workspace 2>&1 | tail -30
```

Run this with `dangerouslyDisableSandbox` and host-network containers. Write the
passed/failed/ignored triple into the task ledger. Every later "green" claim is
compared against **this** number, not a remembered one.

- [ ] **Step 2: Record the frontend baseline**

```bash
cd dashboard && npx vitest run 2>&1 | tail -15
```

- [ ] **Step 3: Confirm `schema.rs` is intact**

```bash
cd backend && grep -cE "_p?[0-9]{4}_[0-9]{2}|_default \(" crates/sauron-db/src/schema.rs
```

Expected: `0`. The invariant is **no partition children**, not a fixed table
count — the count legitimately grows as features land, and pinning it makes the
check cry wolf. A non-zero result means `diesel migration run` rewrote the file
(the `print_schema` trap) and it must be restored before proceeding. Measured
2026-08-16: 55 real tables, 0 children.

---

### Task 1: Migration — Devices `first_seen` indexes

**Files:**
- Create: `backend/migrations/2026-08-16-000062_devices_first_seen_index/up.sql`
- Create: `backend/migrations/2026-08-16-000062_devices_first_seen_index/down.sql`

**Interfaces:**
- Produces: `devices_app_first_seen_idx`, `device_env_app_env_first_seen_idx`.

- [ ] **Step 1: Write `up.sql`**

```sql
-- Devices gains a caller-chosen time-window column (`first_seen` alongside
-- `last_seen`), and neither table was indexed for it.
--
-- `devices` had only `devices_app_last_seen_idx (app_id, last_seen DESC)` and
-- `device_environments` only `device_env_app_env_idx
-- (app_id, environment_id, last_seen DESC)`. `event_users` and
-- `event_user_environments` already carry BOTH columns for persons, which is
-- why the Users list needs no migration and this one does.
--
-- Not DESC: `first_seen` is filtered as a range bound and ordered ascending as
-- often as descending, and a btree walks either direction. The `last_seen`
-- indexes are DESC only because their dominant use is `ORDER BY last_seen DESC`.

CREATE INDEX devices_app_first_seen_idx
  ON devices (app_id, first_seen);

CREATE INDEX device_env_app_env_first_seen_idx
  ON device_environments (app_id, environment_id, first_seen);
```

- [ ] **Step 2: Write `down.sql`**

```sql
DROP INDEX IF EXISTS device_env_app_env_first_seen_idx;
DROP INDEX IF EXISTS devices_app_first_seen_idx;
```

- [ ] **Step 3: Run the migration and verify both indexes exist**

```bash
cd backend && diesel migration run && psql "$DATABASE_URL" -c "\di devices_app_first_seen_idx device_env_app_env_first_seen_idx"
```

Expected: two rows.

- [ ] **Step 4: Re-check `schema.rs`**

```bash
cd backend && grep -c "diesel::table!" crates/sauron-db/src/schema.rs
```

Expected: `27`. An index-only migration must not change it at all —
`git diff --stat crates/sauron-db/src/schema.rs` should be empty.

---

### Task 2: The shared resolver

**Files:**
- Modify: `backend/bins/sauron-api/src/routes/search.rs`

**Interfaces:**
- Produces: `TimeFilterQuery`, `TimeWindowSpec`, `resolve_time_filter`. Every
  route task consumes these exact names.

- [ ] **Step 1: Write the failing tests**

Append to `search.rs`'s `mod tests`:

```rust
fn tq(field: Option<&str>, from: Option<&str>, to: Option<&str>) -> TimeFilterQuery {
    TimeFilterQuery {
        time_field: field.map(str::to_string),
        from: from.map(|s| DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)),
        to: to.map(|s| DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)),
        since_days: 30,
    }
}

const ALLOWED: &[&str] = &["last_seen", "first_seen"];
fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-08-16T12:00:00Z").unwrap().with_timezone(&Utc)
}

#[test]
fn since_days_is_used_when_no_bounds_given() {
    let w = resolve_time_filter("last_seen", ALLOWED, &tq(None, None, None), now(), 365, None).unwrap();
    assert_eq!(w.column, "last_seen");
    assert_eq!(w.from, now() - Duration::days(30));
    assert!(w.to.is_none());
    assert!(w.clamped.is_none());
}

#[test]
fn explicit_bounds_beat_since_days() {
    let w = resolve_time_filter(
        "last_seen", ALLOWED,
        &tq(Some("first_seen"), Some("2026-08-01T00:00:00Z"), Some("2026-08-03T00:00:00Z")),
        now(), 365, None,
    ).unwrap();
    assert_eq!(w.column, "first_seen");
    assert_eq!(w.from, DateTime::parse_from_rfc3339("2026-08-01T00:00:00Z").unwrap());
    assert_eq!(w.to.unwrap(), DateTime::parse_from_rfc3339("2026-08-03T00:00:00Z").unwrap());
}

#[test]
fn an_unlisted_field_is_a_400_naming_the_allowed_set() {
    let err = resolve_time_filter("last_seen", ALLOWED, &tq(Some("occurred_at"), None, None), now(), 365, None)
        .unwrap_err();
    let msg = format!("{err:?}");
    assert!(msg.contains("occurred_at"), "names the rejected value: {msg}");
    assert!(msg.contains("last_seen") && msg.contains("first_seen"), "names the allowed set: {msg}");
}

#[test]
fn an_upper_bound_alone_gets_a_floor_and_discloses_it() {
    let w = resolve_time_filter(
        "last_seen", ALLOWED, &tq(None, None, Some("2026-08-03T00:00:00Z")), now(), 365, None,
    ).unwrap();
    let to = DateTime::parse_from_rfc3339("2026-08-03T00:00:00Z").unwrap();
    assert_eq!(w.from, to - Duration::days(365), "floored to to - max_days");
    let c = w.clamped.expect("a floored window must be disclosed");
    assert_eq!(c.field, "last_seen");
    assert_eq!(c.to, "365d");
}

#[test]
fn an_inverted_range_is_rejected() {
    let err = resolve_time_filter(
        "last_seen", ALLOWED,
        &tq(None, Some("2026-08-05T00:00:00Z"), Some("2026-08-01T00:00:00Z")),
        now(), 365, None,
    ).unwrap_err();
    assert!(format!("{err:?}").contains("from"), "{err:?}");
}

#[test]
fn an_oversized_explicit_range_is_narrowed_from_the_bottom() {
    let w = resolve_time_filter(
        "last_seen", ALLOWED,
        &tq(None, Some("2020-01-01T00:00:00Z"), Some("2026-08-03T00:00:00Z")),
        now(), 365, None,
    ).unwrap();
    let to = DateTime::parse_from_rfc3339("2026-08-03T00:00:00Z").unwrap();
    assert_eq!(w.from, to - Duration::days(365));
    assert!(w.clamped.is_some(), "narrowing must be disclosed");
}

#[test]
fn a_planner_clamp_still_tightens_an_explicit_window() {
    let w = resolve_time_filter(
        "last_seen", ALLOWED, &tq(None, Some("2026-01-01T00:00:00Z"), None), now(), 365,
        Some(Clamp { to_days: 7, reason: "query degrades to a scan" }),
    ).unwrap();
    assert_eq!(w.from, now() - Duration::days(7));
    assert_eq!(w.clamped.unwrap().reason, "query degrades to a scan");
}

#[test]
fn a_planner_clamp_that_does_not_tighten_is_not_reported() {
    let w = resolve_time_filter(
        "last_seen", ALLOWED,
        &tq(None, Some("2026-08-15T00:00:00Z"), None), now(), 365,
        Some(Clamp { to_days: 365, reason: "irrelevant" }),
    ).unwrap();
    assert_eq!(w.from, DateTime::parse_from_rfc3339("2026-08-15T00:00:00Z").unwrap());
    assert!(w.clamped.is_none());
}
```

- [ ] **Step 2: Run to verify they fail**

```bash
cd backend && cargo test -p sauron-api --lib routes::search::tests 2>&1 | tail -20
```

Expected: compile error — `TimeFilterQuery` not found.

- [ ] **Step 3: Implement**

Add to `search.rs`:

```rust
/// The three parameters every windowed list route accepts, flattened into its
/// own `Query<T>` struct.
///
/// `since_days` lives here too rather than beside it, so that the precedence
/// rule — explicit bounds win, `since_days` is ignored when either is present —
/// is decided in ONE place. A route that kept its own `since_days` field could
/// only re-derive that rule, and two derivations of a precedence rule is how
/// two lists that look identical start answering differently.
#[derive(Debug, Deserialize)]
pub struct TimeFilterQuery {
    /// Validated by equality against the route's whitelist. The resolved value
    /// is the whitelist's `&'static str`; this `String` never reaches SQL.
    pub time_field: Option<String>,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    #[serde(default = "default_since_days_365")]
    pub since_days: i64,
}

pub fn default_since_days_365() -> i64 {
    365
}

/// The window a list actually ran over.
///
/// `from` is never optional and `to` is: `analytics_events` is
/// `PARTITION BY RANGE (occurred_at)`, so an unbounded LOWER bound is a
/// MergeAppend across every partition — the shape behind the env-scoped
/// analytics timeout. An unbounded UPPER bound costs nothing, because "up to
/// now" is where the data ends anyway.
#[derive(Debug, Clone)]
pub struct TimeWindowSpec {
    pub column: &'static str,
    pub from: DateTime<Utc>,
    /// Exclusive. See the half-open rule in the spec.
    pub to: Option<DateTime<Utc>>,
    pub clamped: Option<ClampInfo>,
}

/// Resolve `time_field`/`from`/`to`/`since_days` into the window that will be
/// served, and disclose whatever narrowed it.
///
/// Generalises [`resolve_window`], which it will replace once every windowed
/// route is migrated. The three narrowing rules are unchanged and still apply
/// tightest-wins: the caller's own bounds, the route's `max_days`, and the
/// planner's cost clamp.
///
/// The only genuinely new rule is the **floor**. `to` with no `from` asks for
/// everything before an instant, which on a partitioned table prunes nothing.
/// `from` becomes `to - max_days` and the narrowing is reported, because a
/// window that was silently narrowed is a wrong answer carrying a 200.
pub fn resolve_time_filter(
    default_field: &'static str,
    allowed: &[&'static str],
    q: &TimeFilterQuery,
    now: DateTime<Utc>,
    max_days: i64,
    planner: Option<Clamp>,
) -> Result<TimeWindowSpec, ApiError> {
    let column = match q.time_field.as_deref() {
        None => default_field,
        Some(name) => *allowed.iter().find(|a| **a == name).ok_or_else(|| {
            ApiError::BadRequest(format!(
                "unknown time_field `{name}`; this list accepts: {}",
                allowed.join(", ")
            ))
        })?,
    };

    // Explicit bounds win outright. `since_days` is not consulted at all when
    // either is present — mixing them would make `?from=…&since_days=7` mean
    // something no caller could predict.
    let explicit = q.from.is_some() || q.to.is_some();

    let (mut from, to) = if explicit {
        let to = q.to;
        let from = q.from.unwrap_or_else(|| {
            to.expect("explicit implies at least one bound") - Duration::days(max_days)
        });
        (from, to)
    } else {
        (now - Duration::days(q.since_days.clamp(1, max_days)), None)
    };

    if let Some(t) = to {
        if from >= t {
            return Err(ApiError::BadRequest(
                "`from` must be earlier than `to`".to_string(),
            ));
        }
    }

    let ceiling = to.unwrap_or(now);
    let mut reason = None;

    // The route's own ceiling. Applies to explicit bounds and to `since_days`
    // alike — `since_days` was already clamped above, so this only ever bites
    // an explicit range.
    let floor = ceiling - Duration::days(max_days);
    if from < floor {
        from = floor;
        reason = Some(format!("this view bounds its time window at {max_days} days"));
    }

    // The planner's clamp, strictly tighter only: one that merely matches the
    // window already in force changed nothing, and crediting it would name the
    // wrong rule.
    if let Some(c) = planner {
        let planner_from = now - Duration::days(c.to_days);
        if planner_from > from {
            from = planner_from;
            reason = Some(c.reason.to_string());
        }
    }

    Ok(TimeWindowSpec {
        column,
        from,
        to,
        clamped: reason.map(|reason| ClampInfo {
            field: column.to_string(),
            to: format!("{}d", (ceiling - from).num_days()),
            reason,
        }),
    })
}
```

- [ ] **Step 4: Run the tests**

```bash
cd backend && cargo test -p sauron-api --lib routes::search::tests 2>&1 | tail -20
```

Expected: all pass. Then `cargo clippy -p sauron-api --all-targets -- -D warnings`.

---

### Task 3: Repo window type

**Files:**
- Modify: `backend/crates/sauron-db/src/repo.rs`

**Interfaces:**
- Produces: `repo::TimeWindow`. Tasks 4-7 consume it.

- [ ] **Step 1: Add the type**

```rust
/// The time window a list query runs over, as the repo layer receives it.
///
/// `column` arrives already validated against the route's whitelist and is a
/// `&'static str` for that reason — it is interpolated into raw SQL in
/// [`list_persons`] and [`list_devices`], so it must be impossible for caller
/// text to reach it. Do not widen this to `String`.
///
/// `to` is EXCLUSIVE: `from <= col < to`. An inclusive upper bound would have
/// to be written as the last representable instant, and `timestamptz` stores
/// microseconds, so `23:59:59.999` silently drops the final millisecond.
#[derive(Debug, Clone, Copy)]
pub struct TimeWindow {
    pub column: &'static str,
    pub from: DateTime<Utc>,
    pub to: Option<DateTime<Utc>>,
}

impl TimeWindow {
    /// A window on `column` with no upper bound — the shape every caller had
    /// before this feature, kept so untouched call sites read unchanged.
    pub fn since(column: &'static str, from: DateTime<Utc>) -> Self {
        Self { column, from, to: None }
    }
}
```

- [ ] **Step 2: Verify it compiles**

```bash
cd backend && cargo check -p sauron-db 2>&1 | tail -5
```

---

### Task 4: Devices

**Files:**
- Modify: `backend/crates/sauron-db/src/repo.rs` — `list_devices`, `list_device_groups`, and the two group SQL builders
- Modify: `backend/bins/sauron-api/src/routes/devices.rs`
- Test: `backend/bins/sauron-api/tests/http_time_filter.rs`

**Interfaces:**
- Consumes: `repo::TimeWindow` (Task 3), `resolve_time_filter` (Task 2).
- Produces: `devices::TIME_FIELDS: &[&str] = &["last_seen", "first_seen"]`.

- [ ] **Step 1: Write the failing route test**

New file `backend/bins/sauron-api/tests/http_time_filter.rs`. Build the fixture
with **two apps**, each with devices at known timestamps:

```rust
mod common;
use common::*;

/// Two apps, because a single-app fixture returns the same rows whether or not
/// the predicate is scoped — which is how the slice-2 cross-tenant leak reached
/// a passing suite.
async fn seed_devices(h: &Harness) -> (Uuid, Uuid) {
    let a = h.app("app-a").await;
    let b = h.app("app-b").await;
    // app A: three devices, first_seen 90d / 30d / 1d ago, last_seen all 1d ago.
    h.device(a, "dev-old",  days_ago(90), days_ago(1)).await;
    h.device(a, "dev-mid",  days_ago(30), days_ago(1)).await;
    h.device(a, "dev-new",  days_ago(1),  days_ago(1)).await;
    // app B: one device that must never appear in an app-A response.
    h.device(b, "other-app", days_ago(30), days_ago(1)).await;
    (a, b)
}

#[tokio::test]
async fn devices_first_seen_after() {
    let h = Harness::new().await;
    let (a, _) = seed_devices(&h).await;
    let keys = h.device_keys(a, "time_field=first_seen&from=", days_ago(7)).await;
    assert_eq!(keys, vec!["dev-new"]);
}

#[tokio::test]
async fn devices_first_seen_before() {
    let h = Harness::new().await;
    let (a, _) = seed_devices(&h).await;
    let keys = h.device_keys(a, "time_field=first_seen&to=", days_ago(7)).await;
    assert_eq!(keys, vec!["dev-mid", "dev-old"]);
}

#[tokio::test]
async fn devices_first_seen_between_excludes_the_other_app() {
    let h = Harness::new().await;
    let (a, _) = seed_devices(&h).await;
    let keys = h.device_range(a, "first_seen", days_ago(60), days_ago(7)).await;
    assert_eq!(keys, vec!["dev-mid"], "must not contain app B's device");
}

#[tokio::test]
async fn devices_unlisted_time_field_is_a_400() {
    let h = Harness::new().await;
    let (a, _) = seed_devices(&h).await;
    let r = h.get_raw(&format!("/v1/apps/{a}/devices?time_field=occurred_at")).await;
    assert_eq!(r.status(), 400);
    let body = r.text().await.unwrap();
    assert!(body.contains("last_seen") && body.contains("first_seen"), "{body}");
}
```

- [ ] **Step 2: Run to verify it fails**

```bash
cd backend && cargo test -p sauron-api --test http_time_filter 2>&1 | tail -20
```

Expected: FAIL — `time_field` is currently ignored, so `devices_first_seen_after`
returns all three devices and the 400 test gets a 200.

- [ ] **Step 3: Change the repo signatures**

In `list_devices` and `list_device_groups`, replace `since: DateTime<Utc>` with
`window: TimeWindow`. The window predicate stays on the **durable** column
inside the paging subquery, where the existing `since` predicate already sits —
`$2` keeps its meaning (the lower bound) and `to` becomes a new **trailing**
bind so nothing renumbers:

```rust
// $1 app_id, $2 from, $3 pattern, $4 limit, $5 offset, $6 env, $7 to.
//
// `$7` is appended rather than inserted because `env.sql_fragment(6)` and
// `device_last_distinct_id_join(_, 6)` hard-code 6; inserting `to` before
// them would silently shift the env bind and scope every page to the wrong
// environment while still returning rows.
//
// One SQL shape serves a bounded and an unbounded window: `$7` is bound as
// `Nullable<Timestamptz>` and the predicate short-circuits on NULL. A second
// `format!` branch would be a second shape to keep in step with this one.
let window_sql = format!(
    " AND d.{col} >= $2 AND ($7::timestamptz IS NULL OR d.{col} < $7)",
    col = window.column,
);
```

`window.column` is a `&'static str` from the route whitelist — see
`TimeWindow`'s doc comment. Bind it:

```rust
.bind::<Timestamptz, _>(window.from)
// … existing binds …
.bind::<Nullable<Timestamptz>, _>(window.to)
```

Apply the identical change in `list_device_groups_live_sql` and
`list_device_groups_rollup_sql`. In the rollup shape the durable column lives
on `device_environments`, so the predicate reads `r.{col}` there; the bind list
is identical for both shapes, which is what lets one `bind` chain serve both —
change one and this is the other place to change.

- [ ] **Step 4: Wire the route**

In `devices.rs`, replace the three window fields of `ListQuery` with the shared
struct and resolve it:

```rust
/// The columns this list will window on. `first_seen` was added with the time
/// filter; both are indexed on `devices` and on `device_environments` as of
/// migration 000062.
///
/// Note what the window means here, because it is NOT what the Persons list
/// means by the same words: it decides WHICH DEVICES ARE LISTED, via the
/// durable `devices` column. Under a scoped read the `first_seen`/`last_seen`
/// a row DISPLAYS are per-environment extrema derived from LATERALs, and a
/// device's per-environment first sighting can postdate its app-level one. That
/// asymmetry predates this feature (it is what `since` always did) and is
/// preserved deliberately — it is also the only form of the predicate an index
/// can serve.
pub const TIME_FIELDS: &[&str] = &["last_seen", "first_seen"];

#[derive(Deserialize)]
pub struct ListQuery {
    #[serde(flatten)]
    pub window: super::search::TimeFilterQuery,
    // … limit, offset, search, sort, group, family, model, os_name, os_version …
}
```

`TimeFilterQuery::since_days` defaults to 365 via `default_since_days_365`, but
Devices defaulted to 30 and must keep doing so — override at the resolve site
rather than changing the shared default:

```rust
let mut wq = q.window;
if wq.from.is_none() && wq.to.is_none() && !raw_has_since_days(raw_query.as_deref()) {
    wq.since_days = 30;
}
let window = super::search::resolve_time_filter(
    "last_seen", TIME_FIELDS, &wq, Utc::now(), 365, None,
)?;
```

Use the existing `RawQuery` the handler already extracts for the
`raw_has_since_days` probe — the same technique `scope::raw_environment_id`
uses, and for the same reason: `serde`'s `default` cannot distinguish "absent"
from "sent explicitly".

- [ ] **Step 5: Run the tests**

```bash
cd backend && cargo test -p sauron-api --test http_time_filter 2>&1 | tail -20
```

Expected: the four devices tests pass.

- [ ] **Step 6: Verify the index is actually used**

```bash
cd backend && psql "$DATABASE_URL" -c "EXPLAIN SELECT device_key FROM devices WHERE app_id = '00000000-0000-0000-0000-000000000000' AND first_seen >= now() - interval '7 days'"
```

Expected: `Index Scan using devices_app_first_seen_idx`. A `Seq Scan` here means
Task 1's index is not being reached and the whole point of it was lost.

---

### Task 5: Persons — and the decorative-picker fix

**Files:**
- Modify: `backend/crates/sauron-db/src/repo.rs` — `list_persons`, `list_persons_live_sql`, `list_persons_rollup_sql`, and the two `*_for_test` helpers
- Modify: `backend/bins/sauron-api/src/routes/analytics.rs` — `PersonsQuery`, `persons_list`
- Test: `backend/bins/sauron-api/tests/http_time_filter.rs`

**Interfaces:**
- Consumes: `repo::TimeWindow`, `resolve_time_filter`.
- Produces: `analytics::PERSON_TIME_FIELDS: &[&str] = &["last_seen", "first_seen"]`.

- [ ] **Step 1: Write the failing tests**

The window must be tested against **both** query shapes, because
`list_persons` picks between them on a per-app backfill marker and a test that
exercises only one leaves the other unverified:

```rust
#[tokio::test]
async fn persons_last_seen_window_live_shape() {
    let h = Harness::new().await;
    let a = h.app_with_persons_not_backfilled().await;
    let ids = h.person_ids(a, "time_field=last_seen&from=", days_ago(7)).await;
    assert_eq!(ids, vec!["recent-user"]);
}

#[tokio::test]
async fn persons_last_seen_window_rollup_shape() {
    let h = Harness::new().await;
    let a = h.app_with_persons_backfilled().await;
    let ids = h.person_ids(a, "time_field=last_seen&from=", days_ago(7)).await;
    assert_eq!(ids, vec!["recent-user"], "rollup shape must agree with live");
}

#[tokio::test]
async fn persons_first_seen_finds_new_users_only() {
    let h = Harness::new().await;
    let a = h.app_with_persons_backfilled().await;
    // `old-user` was first seen 90d ago but last seen yesterday: it must match
    // a last_seen window and MISS a first_seen one. This is the case the whole
    // field-choice feature exists for, and a fixture where the two orderings
    // correlate cannot see the difference.
    let ids = h.person_ids(a, "time_field=first_seen&from=", days_ago(7)).await;
    assert_eq!(ids, vec!["recent-user"]);
    let ids = h.person_ids(a, "time_field=last_seen&from=", days_ago(7)).await;
    assert_eq!(ids, vec!["old-user", "recent-user"]);
}
```

- [ ] **Step 2: Run to verify they fail**

```bash
cd backend && cargo test -p sauron-api --test http_time_filter persons 2>&1 | tail -20
```

Expected: FAIL — `persons_list` has no window at all today, so every test
returns both users.

- [ ] **Step 3: Implement in both SQL shapes**

The predicate goes on the **displayed** expression, which differs by shape and
by `EnvFilter`. Factor the expression once so `seen_select` and the new WHERE
cannot drift:

```rust
/// The SQL expression a given shape/scope DISPLAYS for `column`.
///
/// Both the select list and the window predicate must read this. They are the
/// same value by definition — "users last seen in the last 7 days" has to mean
/// what the Last seen column shows — and deriving them separately is how a page
/// starts filtering by one number and rendering another.
fn person_seen_expr(env: &EnvFilter, rollup: bool, column: &str) -> String {
    match (rollup, matches!(env, EnvFilter::All)) {
        // Under `All` both shapes read the durable `event_users` columns; see
        // `list_persons_rollup_sql`'s doc comment for why the rollup does not
        // derive them there.
        (_, true) => format!("eu.{column}"),
        (true, false) => format!("r.{column}"),
        (false, false) => match column {
            "first_seen" => "LEAST(ae.min_occurred, ee.min_occurred, se.min_started)".to_string(),
            _ => "GREATEST(ae.max_occurred, ee.max_occurred, se.max_last_event)".to_string(),
        },
    }
}
```

In `list_persons_rollup_sql` the predicate joins the existing outer WHERE:

```rust
// $1 app_id, $2 pattern, $3 limit, $4 offset, $5 env, $6 from, $7 to.
// Appended, not inserted: `env.sql_fragment_for("r", 5)` hard-codes 5.
let expr = person_seen_expr(env, true, window_column);
let window_sql = format!(" AND {expr} >= $6 AND ($7::timestamptz IS NULL OR {expr} < $7)");
```

In `list_persons_live_sql` the same predicate goes on the outer query, after the
LATERAL joins, since under a scoped read the expression references `ae`/`ee`/`se`.
Under `All` it references `eu.{column}` and Postgres will push it into the `eu`
subquery on its own — verify with `EXPLAIN` in step 5 rather than assuming.

A SQL alias cannot be referenced from `WHERE`, which is why this repeats the
expression rather than saying `WHERE last_seen >= $6`.

- [ ] **Step 4: Wire the route**

```rust
/// Persons windows on the DISPLAYED value, not on a durable column — the
/// opposite of `devices::TIME_FIELDS`. See `person_seen_expr`.
pub const PERSON_TIME_FIELDS: &[&str] = &["last_seen", "first_seen"];
```

`persons_list` resolves with `default_field = "last_seen"` and `max_days = 365`,
and keeps the shared `since_days` default of **365** rather than adopting 30.
This page has never filtered its table; defaulting to 30 would make most of an
app's users vanish from a list that has always shown all of them. 365 is the
route ceiling, so it is the widest honest default available.

- [ ] **Step 5: Run the tests and check the plan**

```bash
cd backend && cargo test -p sauron-api --test http_time_filter persons 2>&1 | tail -20
```

Then confirm the rollup path is index-served:

```bash
cd backend && psql "$DATABASE_URL" -c "EXPLAIN SELECT distinct_id FROM event_user_environments WHERE app_id='00000000-0000-0000-0000-000000000000' AND environment_id='00000000-0000-0000-0000-000000000000' AND first_seen >= now() - interval '7 days'"
```

Expected: `Index Scan using event_user_env_first_seen_idx`.

- [ ] **Step 6: Verify the two shapes agree**

Run the existing `list_persons_sql_for_test` / `list_persons_rollup_sql_for_test`
snapshot tests. Both helpers construct a `SortSpec` inline and now need a
`TimeWindow` too — update them rather than deleting the assertions.

```bash
cd backend && cargo test -p sauron-db list_persons 2>&1 | tail -20
```

---

### Task 6: Sessions

**Files:**
- Modify: `backend/crates/sauron-db/src/repo.rs` — `SessionSearch`, `session_search_base`
- Modify: `backend/bins/sauron-api/src/routes/sessions.rs`
- Test: `backend/bins/sauron-api/tests/http_time_filter.rs`

**Interfaces:**
- Produces: `sessions::TIME_FIELDS: &[&str] = &["started_at", "last_event_at"]`.

- [ ] **Step 1: Write the failing test — including the disclosure bug**

```rust
#[tokio::test]
async fn sessions_window_field_is_disclosed_truthfully() {
    let h = Harness::new().await;
    let a = h.app("app-a").await;
    // `since_days` far beyond the ceiling forces a clamp, so `clamped` is populated.
    let env: SearchEnvelope = h.get_json(&format!(
        "/v1/apps/{a}/sessions?time_field=started_at&since_days=3650"
    )).await;
    assert_eq!(env.clamped.unwrap().field, "started_at");

    let env: SearchEnvelope = h.get_json(&format!(
        "/v1/apps/{a}/sessions?time_field=last_event_at&since_days=3650"
    )).await;
    assert_eq!(
        env.clamped.unwrap().field, "last_event_at",
        "the disclosure must name the column actually filtered — it hard-coded \
         `started_at` while the predicate used `last_event_at`"
    );
}

#[tokio::test]
async fn sessions_started_at_and_last_event_at_select_different_rows() {
    let h = Harness::new().await;
    let a = h.app("app-a").await;
    // A long session: started 30d ago, last event yesterday.
    h.session(a, "long", days_ago(30), days_ago(1)).await;
    h.session(a, "short", days_ago(1), days_ago(1)).await;
    assert_eq!(h.session_ids(a, "started_at", days_ago(7)).await, vec!["short"]);
    assert_eq!(h.session_ids(a, "last_event_at", days_ago(7)).await, vec!["long", "short"]);
}
```

- [ ] **Step 2: Run to verify it fails**

```bash
cd backend && cargo test -p sauron-api --test http_time_filter sessions 2>&1 | tail -20
```

Expected: FAIL — `clamped.field` is the hard-coded `"started_at"` in both cases,
and both field values return the same rows.

- [ ] **Step 3: Implement**

Replace `SessionSearch.since: DateTime<Utc>` with `window: TimeWindow`, and in
`session_search_base` select the column from the window rather than hard-coding
`last_event_at`:

```rust
// The column is chosen by the caller and validated at the route; matching on
// the whitelist's own values keeps this exhaustive without a `&str` reaching
// diesel. `_` is unreachable given `sessions::TIME_FIELDS`, and defaulting it
// to `last_event_at` preserves the behaviour every pre-existing caller had.
let mut query = sessions::table
    .filter(sessions::app_id.eq(scope.app_id))
    .filter(predicate);
query = match search.window.column {
    "started_at" => query.filter(sessions::started_at.ge(search.window.from)).into_boxed(),
    _ => query.filter(sessions::last_event_at.ge(search.window.from)).into_boxed(),
};
if let Some(to) = search.window.to {
    query = match search.window.column {
        "started_at" => query.filter(sessions::started_at.lt(to)),
        _ => query.filter(sessions::last_event_at.lt(to)),
    };
}
```

At the route, pass the resolved column into the disclosure:

```rust
pub const TIME_FIELDS: &[&str] = &["started_at", "last_event_at"];

let window = super::search::resolve_time_filter(
    // `last_event_at` is the default because it is what this list has ALWAYS
    // filtered on — `resolve_window("started_at", …)` named the other column
    // in `clamped` while `session_search_base` filtered this one. Defaulting to
    // `started_at` here would silently change which sessions the unparameterised
    // list returns, which is a bigger change than fixing the label.
    "last_event_at", TIME_FIELDS, &wq, Utc::now(), 365, prepared.clamp,
)?;
```

Note in the route doc comment that `last_event_at` is surfaced in the UI as
**"Last activity"** — `sessions` has no `ended_at`; duration is derived.

- [ ] **Step 4: Run the tests**

```bash
cd backend && cargo test -p sauron-api --test http_time_filter sessions 2>&1 | tail -20
cd backend && cargo test -p sauron-api --test http_sessions_search 2>&1 | tail -10
```

Both must pass — the second is the existing suite and must not regress.

---

### Task 7: Events

**Files:**
- Modify: `backend/crates/sauron-db/src/repo.rs` — `EventSearch`
- Modify: `backend/bins/sauron-api/src/routes/analytics.rs` — `EventsListQuery`, `events_list`
- Test: `backend/bins/sauron-api/tests/http_time_filter.rs`

**Interfaces:**
- Produces: `analytics::EVENT_TIME_FIELDS: &[&str] = &["occurred_at"]`.

Events offers one field, so this task adds the **bounds**, not a field choice.
The single-element whitelist is deliberate: it makes `?time_field=received_at`
a 400 that names what is allowed, rather than a parameter that looks accepted.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn events_between_two_instants() {
    let h = Harness::new().await;
    let a = h.app("app-a").await;
    h.event(a, "old", days_ago(30)).await;
    h.event(a, "mid", days_ago(10)).await;
    h.event(a, "new", days_ago(1)).await;
    let names = h.event_names_range(a, days_ago(20), days_ago(5)).await;
    assert_eq!(names, vec!["mid"]);
}

#[tokio::test]
async fn events_upper_bound_alone_is_floored_and_disclosed() {
    let h = Harness::new().await;
    let a = h.app("app-a").await;
    let env: SearchEnvelope = h.get_json(&format!(
        "/v1/apps/{a}/events?to=2026-08-10T00:00:00Z"
    )).await;
    let c = env.clamped.expect("an unbounded lower bound must be disclosed");
    assert_eq!(c.field, "occurred_at");
    assert_eq!(c.to, "365d");
}

#[tokio::test]
async fn events_received_at_is_rejected_by_name() {
    let h = Harness::new().await;
    let a = h.app("app-a").await;
    let r = h.get_raw(&format!("/v1/apps/{a}/events?time_field=received_at")).await;
    assert_eq!(r.status(), 400);
    assert!(r.text().await.unwrap().contains("occurred_at"));
}
```

- [ ] **Step 2: Run to verify it fails**

```bash
cd backend && cargo test -p sauron-api --test http_time_filter events 2>&1 | tail -20
```

- [ ] **Step 3: Implement**

Add `to: Option<DateTime<Utc>>` to `EventSearch` beside `since`, documented as
exclusive, and apply it in `event_query_for`:

```rust
if let Some(to) = search.to {
    query = query.filter(analytics_events::occurred_at.lt(to));
}
```

The upper bound also prunes partitions, so this is cheaper than the unbounded
query, not more expensive. `count_events` reads the same `EventSearch` and
therefore needs no separate change — which is exactly why the bound lives on the
struct both functions take rather than being passed alongside it.

- [ ] **Step 4: Run the tests**

```bash
cd backend && cargo test -p sauron-api --test http_time_filter 2>&1 | tail -20
cd backend && cargo test -p sauron-api --test http_search 2>&1 | tail -10
```

- [ ] **Step 5: Full backend gate**

```bash
cd backend && cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace 2>&1 | tail -30
```

Compare the triple against Task 0's baseline. `cargo fmt --all --check` is the
wrong invocation — it prints help and exits 0.

---

### Task 8: Frontend model

**Files:**
- Create: `dashboard/src/lib/models/time-filter.ts`
- Test: `dashboard/src/lib/models/time-filter.test.ts`

**Interfaces:**
- Produces: `TimeMode`, `TimeFilterState`, `toParams`, `fromParams`, `validate`,
  `describe`, `defaultFilter`. Tasks 9 and 10 consume these exact names.

- [ ] **Step 1: Write the failing tests**

```ts
import { describe, it, expect } from 'vitest';
import { toParams, fromParams, validate, describe as describeFilter, defaultFilter } from './time-filter';

const FIELDS = [
  { key: 'last_seen', label: 'Last seen' },
  { key: 'first_seen', label: 'First seen' },
];

describe('toParams', () => {
  it('sends since_days for the `last` mode and no bounds', () => {
    const p = toParams({ field: 'last_seen', mode: 'last', lastDays: 7 });
    expect(p.get('since_days')).toBe('7');
    expect(p.get('from')).toBeNull();
    expect(p.get('to')).toBeNull();
  });

  it('omits time_field when it is the page default', () => {
    const p = toParams({ field: 'last_seen', mode: 'last', lastDays: 7 }, 'last_seen');
    expect(p.get('time_field')).toBeNull();
  });

  it('sends bounds and never since_days for an absolute mode', () => {
    const p = toParams({ field: 'first_seen', mode: 'after', from: '2026-08-01T00:00:00.000Z' });
    expect(p.get('from')).toBe('2026-08-01T00:00:00.000Z');
    expect(p.get('since_days')).toBeNull();
  });
});

describe('local to UTC', () => {
  it('makes a bare `to` date the FOLLOWING local midnight', () => {
    // Half-open interval: "between 1 Aug and 3 Aug" must include all of 3 Aug.
    // Truncating `to` to the start of its own day drops the final day entirely.
    const tf = fromParams(new URLSearchParams('mode=between&from=2026-08-01&to=2026-08-03'), FIELDS, 'last_seen');
    expect(new Date(tf.to!).getTime() - new Date(tf.from!).getTime()).toBe(3 * 86_400_000);
  });

  it('survives a DST transition', () => {
    // A 24h calendar day across a spring-forward boundary is 23 real hours.
    // Adding 86_400_000 ms instead of a calendar day gets this wrong by an hour.
    const tf = fromParams(new URLSearchParams('mode=between&from=2026-03-28&to=2026-03-29'), FIELDS, 'last_seen');
    expect(tf.from).toBeTruthy();
    expect(tf.to).toBeTruthy();
    expect(new Date(tf.to!) > new Date(tf.from!)).toBe(true);
  });
});

describe('validate', () => {
  it('rejects an inverted range', () => {
    expect(validate({ field: 'last_seen', mode: 'between', from: '2026-08-05T00:00:00Z', to: '2026-08-01T00:00:00Z' }))
      .toMatch(/earlier/);
  });
  it('rejects a between missing a bound', () => {
    expect(validate({ field: 'last_seen', mode: 'between', from: '2026-08-05T00:00:00Z' })).toBeTruthy();
  });
  it('rejects lastDays below 1', () => {
    expect(validate({ field: 'last_seen', mode: 'last', lastDays: 0 })).toBeTruthy();
  });
  it('accepts a valid range', () => {
    expect(validate({ field: 'last_seen', mode: 'between', from: '2026-08-01T00:00:00Z', to: '2026-08-05T00:00:00Z' }))
      .toBeNull();
  });
});

describe('fromParams', () => {
  it('drops a time_field the page does not offer and falls back to the default', () => {
    // A stale or hand-edited link must degrade to a valid view, not a 400 on
    // first paint.
    const tf = fromParams(new URLSearchParams('time_field=occurred_at'), FIELDS, 'last_seen');
    expect(tf.field).toBe('last_seen');
  });

  it('round-trips every mode through toParams', () => {
    const cases: TimeFilterState[] = [
      { field: 'last_seen', mode: 'last', lastDays: 30 },
      { field: 'first_seen', mode: 'after', from: '2026-08-01T00:00:00.000Z' },
      { field: 'first_seen', mode: 'before', to: '2026-08-01T00:00:00.000Z' },
      { field: 'last_seen', mode: 'between', from: '2026-08-01T00:00:00.000Z', to: '2026-08-05T00:00:00.000Z' },
    ];
    for (const tf of cases) {
      expect(fromParams(toParams(tf), FIELDS, 'last_seen')).toEqual(tf);
    }
  });
});
```

- [ ] **Step 2: Run to verify they fail**

```bash
cd dashboard && npx vitest run src/lib/models/time-filter.test.ts 2>&1 | tail -20
```

Expected: FAIL — module not found.

- [ ] **Step 3: Implement**

```ts
export type TimeMode = 'last' | 'after' | 'before' | 'between';

export interface TimeField {
  readonly key: string;
  readonly label: string;
}

/**
 * Every field is `readonly`, not just the container.
 *
 * Svelte 5 `$state` deep-proxies this object, so `tf.mode = 'after'` is a
 * REACTIVE mutation — a `readonly TimeFilterState` annotation on the holder
 * blocks replacing the object and does nothing about rewriting its fields.
 * That is the exact defect the table-sorting review caught on `SortState`,
 * where a doc comment claimed a protection the type did not provide.
 */
export interface TimeFilterState {
  readonly field: string;
  readonly mode: TimeMode;
  readonly lastDays?: number;
  /** RFC3339 UTC, inclusive. */
  readonly from?: string;
  /** RFC3339 UTC, EXCLUSIVE. */
  readonly to?: string;
}
```

Plus `toParams`, `fromParams`, `validate`, `describe`, `defaultFilter`. The
local→UTC conversion uses `new Date(y, m, d)` and calendar-day arithmetic
(`d.setDate(d.getDate() + 1)`), never `+ 86_400_000` — a DST day is not 24 hours.

- [ ] **Step 4: Run the tests**

```bash
cd dashboard && npx vitest run src/lib/models/time-filter.test.ts 2>&1 | tail -20
```

---

### Task 9: The control

**Files:**
- Create: `dashboard/src/lib/components/TimeFilter.svelte`
- Test: `dashboard/src/lib/components/TimeFilter.test.ts`

**Interfaces:**
- Consumes: everything from Task 8.
- Produces: props `{ fields: TimeField[], value: TimeFilterState, defaultField: string, onchange: (v: TimeFilterState) => void, presets?: number[] }`.

**Correction, found at execution time.** This project has **no component-render
harness** — no `jsdom`, no `happy-dom`, no `@testing-library/svelte`, and not one
existing `.test.ts` mounts a component. The house pattern is the opposite: logic
lives in a pure model that is unit-tested, and the component stays thin enough to
verify by `svelte-check` plus a real browser pass. Adding a DOM harness to write
the four tests originally planned here would be scope the user did not ask for,
so this task keeps the pattern instead: **everything testable already lives in
Task 8's `time-filter.ts`** (30 tests), and this component is verified by types
and by the browser.

The one hazard that cannot be caught by either is enforced by construction and
re-checked in the browser in Task 10 step 6:

> The custom day-count input MUST be `type="text"`. `bind:value` on
> `<input type="number">` writes back `number | null`, which crashes a string
> validator — and because the Apply button's `disabled` is itself a `$derived`,
> the throw happens while *computing the guard*, so the DOM freezes with the
> button still clickable. A number input also silently rounds a mistyped value.

- [ ] **Step 1: Confirm the harness situation before writing tests**

```bash
cd dashboard && grep -E "testing-library|jsdom|happy-dom" package.json; grep -rln "mount(\|render(" src --include=*.test.ts
```

Expected: both empty. If either is non-empty the harness now exists and the
component tests above should be written after all.

- [ ] **Step 2: Type-check instead**

```bash
cd dashboard && npx svelte-check --threshold error 2>&1 | tail -10
```

- [ ] **Step 3: Implement**

Follow the house component conventions: house `ui/` components rather than raw
`<button>`/`<select>`, `Icon` for glyphs, and the `--surface-2` / `--border` /
`--radius` token set `DateRange.svelte` already uses so the two read as siblings.

Presets: `[1, 7, 30, 90, 365]`, labelled `24h / 7d / 30d / 90d / 1y`.

- [ ] **Step 4: Run the tests**

```bash
cd dashboard && npx vitest run src/lib/components/TimeFilter.test.ts 2>&1 | tail -20
```

---

### Task 10: Wire the four pages

**Files:**
- Modify: `pages/Events.svelte`, `pages/SessionsList.svelte`, `pages/UsersExplorer.svelte`, `pages/DevicesInventory.svelte`
- Modify: `lib/components/filters/FilterBar.svelte`
- Modify: `lib/api/{sessions,devices,users,events,search}.ts`

**Interfaces:**
- Consumes: Tasks 8 and 9; the backend params from Tasks 4-7.

- [ ] **Step 1: Add the params to the API clients**

`search.ts`'s `SearchPredicateParams` gains `timeField?`, `from?`, `to?`, and
`predicateParams` writes them. `devices.ts` and a new `listPersons` param block
follow the same names.

- [ ] **Step 2: Replace `DateRange` on each page**

Per-page field lists and defaults:

| Page | fields | default field | default mode |
|---|---|---|---|
| Events | `occurred_at` ("Occurred") | `occurred_at` | last 365d |
| Sessions | `started_at` ("Started"), `last_event_at` ("Last activity") | `last_event_at` | last 30d |
| Users | `last_seen` ("Last seen"), `first_seen` ("First seen") | `last_seen` | last 365d |
| Devices | `last_seen` ("Last seen"), `first_seen` ("First seen") | `last_seen` | last 30d |

- [ ] **Step 3: Pass the window into `UsersExplorer.load()`**

This is the decorative-picker fix. `load()` gains the filter argument it has
never had, and the filter enters the `viewKey`.

**Cache-key hazard:** the key must carry the filter's DECLARATION (`last:30d`),
never the instant `mode: 'last'` resolves to. A clock-derived value in a
`viewKey` mints a fresh entry on every load — the cache stays wired, typed and
green while hitting zero times, and only the network panel shows it.

- [ ] **Step 4: Add URL sync to Sessions, Users and Devices**

Events already reads `since_days` from `location.search`; extend it to the new
params and add a read-on-mount plus `replaceState`-on-change to the other three.

The URL write must not retrigger the load effect. Read the filter through
`untrack` inside the predicate effect, the same guard Events already uses so a
page move or sort change is not immediately reset by the effect watching the
predicate.

- [ ] **Step 5: Frontend gate**

```bash
cd dashboard && npx vitest run 2>&1 | tail -15 && npx svelte-check --threshold error 2>&1 | tail -10
```

Compare against Task 0's baseline; `svelte-check` must report 0 errors.

- [ ] **Step 6: Verify in the browser, both themes**

Start the dev server via `preview_start` and drive each of the four pages:
switch field, switch mode, enter a range, reload to confirm the URL restored it.
Check `read_network_requests` to confirm `from`/`to` are on the wire and the
`viewKey` is hitting — a cache that never hits is invisible from the DOM.

**Check the API base first.** A committed `static/config.js` pins dev to `:8090`
and outranks `VITE_API_BASE_URL`; a container older than the schema 500s and
empties the lists, which reads as a frontend bug.

---

### Task 11: Documentation

**Files:**
- Modify: `wiki/Dashboard.md`
- Modify: `dashboard/src/pages/Docs.svelte`

- [ ] **Step 1: Grep for sentences this makes false**

```bash
cd /home/splimter/projects/freelance/sauron && grep -rn "last 30 days\|date range\|time range\|since_days" wiki/*.md | head -30
```

Any sentence stating these lists offer only a fixed range picker becomes false
the moment this ships and is corrected in the same change — docs go with the
slice that makes them true, not batched after it.

- [ ] **Step 2: Write the `wiki/Dashboard.md` section**

Cover the four modes, the per-page field table from Task 10 step 2, the
half-open interval, and the 365-day span clamp with what `clamped` reports.

State the Devices-vs-Persons asymmetry explicitly: on Devices the window
decides which devices are listed via the durable column; on Users it filters
the value the column displays.

- [ ] **Step 3: Add the `Docs.svelte` cheatsheet rows**

- [ ] **Step 4: Final gate**

```bash
cd backend && cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace 2>&1 | tail -30
cd dashboard && npx vitest run 2>&1 | tail -15
```

Both compared against Task 0's baseline.

---

## Self-Review

**Spec coverage:** §1 control → Task 9. §2 model → Task 8. §3 wire/resolver →
Task 2, repo type Task 3. §3 span clamp → Task 2 step 3 + tests. §4 fields →
Tasks 4-7 whitelists, Task 10 step 2 table. §5 Users gap → Task 5 + Task 10
step 3. §6 migration → Task 1. §7 exclusions → Task 7 (`received_at` 400 test);
`identified_at` needs no task, it is simply absent from every whitelist. §8 URL
→ Task 10 step 4. §9 testing → each task's test step + Task 0 baseline. §10 docs
→ Task 11.

**Naming consistency:** `TimeFilterQuery` / `TimeWindowSpec` / `resolve_time_filter`
(routes) and `TimeWindow` (repo) are two distinct types on purpose — the route
one carries the disclosure, the repo one does not. `TIME_FIELDS` is per-module
(`devices::`, `sessions::`) except in `analytics.rs`, which hosts two routes and
so uses `PERSON_TIME_FIELDS` / `EVENT_TIME_FIELDS`. Frontend `TimeFilterState`,
`toParams`, `fromParams`, `validate`, `describe`, `defaultFilter` are used under
those exact names in Tasks 9 and 10.

**Gap found and added:** Task 5 step 6 — `list_persons_sql_for_test` and
`list_persons_rollup_sql_for_test` construct a `SortSpec` inline and will not
compile once the builders take a `TimeWindow`. Without that step the snapshot
assertions would most likely be deleted to make the build pass.
