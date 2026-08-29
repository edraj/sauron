# Retention & Cohorts Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship cohort retention, lifecycle, error-impact and churn analytics for Sauron, backed by one new exact per-person-per-day rollup table.

**Architecture:** A new `person_days` table records one row per (app, environment, distinct_id, day), folded incrementally from the existing analytics and error firehose pulls in `sauron-db::rollups::fold`. Cohort membership comes free from `event_user_environments.first_seen`. Four read endpoints in a new `routes/retention.rs` serve a new `#/retention` dashboard page. The table gets its own epoch and readiness marker so an un-backfilled app reports `ready: false` instead of a plausible-looking 0% grid.

**Tech Stack:** Rust 1.82 · axum 0.8 · diesel 2.3 / diesel-async 0.9 (raw `sql_query`, not the DSL, matching every other rollup) · PostgreSQL 16 · Svelte 5 (runes) · TypeScript · axios · Vitest

**Spec:** [`docs/superpowers/specs/2026-08-28-retention-and-cohorts-design.md`](../specs/2026-08-28-retention-and-cohorts-design.md)

## Global Constraints

- **NEVER run `git commit`, and never create a branch.** Leave every change unstaged in the working tree. This overrides the commit step that normally ends each task below — there are no commit steps in this plan by design. Report what changed; the user commits.
- **Never `git stash`.** Another session edits this checkout concurrently.
- All rollup SQL uses `diesel::sql_query` with explicit `bind::<Type, _>`, never the Diesel DSL — match the surrounding code in `backend/crates/sauron-db/src/rollups/mod.rs`.
- The environment sentinel is `COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid)`, spelled out literally in SQL and produced in Rust by `rollups::env_key`. Never a bare nullable column in a unique index.
- `environment_id` is **never** a field on an axum `Query<T>` struct in a route handler. Read it from `RawQuery` and pass to `scope::authorized_read_scope`. See `backend/bins/sauron-api/src/routes/scope.rs` module docs.
- Every new dashboard route must appear in **four** parity-tested tables or CI fails: `routes.ts`, `PAGE_ACCESS`, `SHELL_FLAGS`, `Sidebar`.
- Every user-visible string ships with **both** `en` and `ar` at authoring time. The untranslated-string test has produced false greens twice; it is not a safety net.
- Backend tests silently pass in ~0.00s when no test database is reachable. **Verify by elapsed time and row assertions, never by a green line.** Before trusting any backend run in this plan, confirm `TEST_DATABASE_URL` is set and the suite took real time.
- Version lockstep if a release is cut: `backend/Cargo.toml`, `packaging/sauron.spec`, `dashboard/package.json`.

## File Structure

**Create:**
- `backend/migrations/2026-08-28-000074_person_days/{up,down}.sql` — the table, its epoch, its backfill marker.
- `backend/crates/sauron-db/src/rollups/person_days.rs` — delta type, additive upsert, readiness gate, pruning.
- `backend/crates/sauron-db/src/person_days_backfill.rs` — the operator-run pre-epoch backfill.
- `backend/crates/sauron-db/src/retention.rs` — the three read queries (grid, lifecycle, churn).
- `backend/bins/sauron-api/src/routes/retention.rs` — three handlers.
- `backend/crates/sauron-db/tests/person_days_rollup.rs` — fold equivalence, merge, purge, backfill disjointness.
- `backend/bins/sauron-api/tests/http_retention.rs` — API shapes and caps.
- `dashboard/src/lib/api/retention.ts` — typed client.
- `dashboard/src/lib/components/RetentionGrid.svelte` — the cohort matrix.
- `dashboard/src/lib/components/LifecycleChart.svelte` — stacked bars with a negative series.
- `dashboard/src/pages/Retention.svelte` — the page, four independent cards.

**Modify:**
- `backend/crates/sauron-db/src/rollups/mod.rs` — re-export the new module.
- `backend/crates/sauron-db/src/rollups/fold.rs` — add `person_days` to both delta structs and both drivers.
- `backend/crates/sauron-db/src/lib.rs` — declare `retention` and `person_days_backfill`.
- `backend/crates/sauron-db/src/identity_merge.rs` — merge person-day rows.
- `backend/crates/sauron-db/src/purge.rs` — erase person-day rows.
- `backend/crates/sauron-db/src/schema.rs` — regenerated.
- `backend/bins/sauron-api/src/routes/mod.rs`, `backend/bins/sauron-api/src/main.rs` — module + route registration.
- `backend/bins/sauron-migrate/src/main.rs` — `backfill-person-days` subcommand.
- `dashboard/src/routes.ts`, `lib/models/page-access.ts`, `lib/models/shell.ts`, `lib/components/layout/Sidebar.svelte`, `lib/i18n/catalog/analyze.ts`.

---

### Task 1: Migration 74 — table, epoch, marker

**Files:**
- Create: `backend/migrations/2026-08-28-000074_person_days/up.sql`
- Create: `backend/migrations/2026-08-28-000074_person_days/down.sql`
- Test: `backend/crates/sauron-db/tests/schema_drift.rs` (existing — it must still pass)

`schema.rs` is deliberately NOT modified — see Step 4.

**Interfaces:**
- Consumes: nothing.
- Produces: tables `person_days`, `person_days_epoch`, `person_days_backfill`. Every later task depends on these names.

- [ ] **Step 1: Write the failing test**

Append to `backend/crates/sauron-db/tests/person_days_rollup.rs` (create the file):

```rust
mod common;
use common::TestDb;
use diesel_async::RunQueryDsl;

#[derive(diesel::QueryableByName)]
struct BoolRow {
    #[diesel(sql_type = diesel::sql_types::Bool)]
    present: bool,
}

#[tokio::test]
async fn migration_74_creates_person_days_with_env_sentinel_index() {
    let Some(db) = TestDb::setup().await else { return };
    let mut conn = db.conn().await;

    // The table exists.
    let r: BoolRow = diesel::sql_query(
        "SELECT to_regclass('person_days') IS NOT NULL AS present",
    )
    .get_result(&mut conn)
    .await
    .unwrap();
    assert!(r.present, "person_days table missing");

    // The unique index leads with the cohort probe, not a day scan, and
    // spells the nil-uuid sentinel rather than a bare nullable column.
    let r: BoolRow = diesel::sql_query(
        "SELECT EXISTS (SELECT 1 FROM pg_indexes \
           WHERE tablename = 'person_days' \
             AND indexdef LIKE '%UNIQUE%' \
             AND indexdef LIKE '%distinct_id%' \
             AND indexdef LIKE '%00000000-0000-0000-0000-000000000000%') AS present",
    )
    .get_result(&mut conn)
    .await
    .unwrap();
    assert!(r.present, "person_days unique index missing or lacks the env sentinel");

    // The epoch is stamped BY THE MIGRATION, not later.
    let r: BoolRow = diesel::sql_query(
        "SELECT EXISTS (SELECT 1 FROM person_days_epoch) AS present",
    )
    .get_result(&mut conn)
    .await
    .unwrap();
    assert!(r.present, "person_days_epoch not stamped by the migration");

    db.cleanup().await;
}
```

- [ ] **Step 2: Run it and confirm it fails**

```bash
cd backend && cargo test -p sauron-db --test person_days_rollup -- --nocapture
```

Expected: FAIL on `person_days table missing`. If it passes in 0.00s, `TEST_DATABASE_URL` is unset and **nothing ran** — fix that before continuing.

- [ ] **Step 3: Write the migration**

`backend/migrations/2026-08-28-000074_person_days/up.sql`:

```sql
-- Retention's substrate: one row per (app, environment, person, day).
--
-- This is the FIRST rollup whose size is bounded by users x days rather than
-- by keys x environments x days, which migration 71's header states as the
-- rollup principle. The exception is deliberate and is argued in
-- docs/superpowers/specs/2026-08-28-retention-and-cohorts-design.md: retention
-- is an INTERSECTION (who was in cohort C and also active in period N), and
-- user_activity_daily stores HyperLogLog, which unions but does not intersect.
-- ~2 GB per 90 days at 1M active users, against ~1.7 TB of firehose covering
-- the same window.
CREATE TABLE person_days (
    app_id         uuid NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    environment_id uuid REFERENCES app_environments(id) ON DELETE CASCADE,
    distinct_id    text NOT NULL,
    day            date NOT NULL,
    events         bigint NOT NULL DEFAULT 0,
    errors         bigint NOT NULL DEFAULT 0,
    updated_at     timestamptz NOT NULL DEFAULT now()
);

-- Leading (app_id, env, distinct_id): the retention grid PROBES BY PERSON --
-- it joins each cohort member to their own later days. This is the opposite
-- of device_sessions_daily, whose reader is a day-range scan and whose index
-- therefore leads with day.
--
-- The nil-uuid sentinel rather than a bare nullable column, for the reason
-- migration 56 records: NULL <> NULL, so a plain UNIQUE over a nullable
-- environment_id lets one person accumulate unlimited unattributed rows, and
-- every upsert against them INSERTs instead of UPDATEs -- counters silently
-- stop accumulating for exactly the scope that has no environment.
CREATE UNIQUE INDEX person_days_key ON person_days
    (app_id, COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid),
     distinct_id, day);

-- The other direction: lifecycle classifies everyone active in a day range,
-- so it scans by day and needs distinct_id available from the index.
CREATE INDEX person_days_day ON person_days
    (app_id, COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid), day)
    INCLUDE (distinct_id);

-- Its OWN epoch, not rollup_epoch.
--
-- rollup_backfill markers already exist for every app backfilled under
-- migration 71, and those runs never wrote person_days because the table did
-- not exist. Gating on rollups::is_ready() would therefore report READY for an
-- app whose person_days is empty, and the API would answer 0% retention --
-- confidently, which is worse than an error because it looks like an answer.
-- event_user_env_rollup_epoch and device_env_rollup_epoch exist as separate
-- tables for exactly this reason; this is the third instance.
--
-- Stamped HERE, in the same migration that creates the table: a stamp taken
-- later lies about every row that arrived in between, and that instant is not
-- recoverable after the fact (the migration-70 lesson).
CREATE TABLE person_days_epoch (
    only_row   boolean     PRIMARY KEY DEFAULT true CHECK (only_row),
    started_at timestamptz NOT NULL DEFAULT now()
);
INSERT INTO person_days_epoch DEFAULT VALUES;

-- Per-app readiness marker, written by the backfill IN THE SAME TRANSACTION as
-- the last rows it writes (the device_env_backfill:88 rule: a marker must
-- never be visible before the data it claims).
CREATE TABLE person_days_backfill (
    app_id       uuid PRIMARY KEY REFERENCES apps(id) ON DELETE CASCADE,
    completed_at timestamptz NOT NULL DEFAULT now()
);
```

`down.sql`:

```sql
DROP TABLE IF EXISTS person_days_backfill;
DROP TABLE IF EXISTS person_days_epoch;
DROP TABLE IF EXISTS person_days;
```

- [ ] **Step 4: Apply the migration**

```bash
cd backend && diesel migration run && touch crates/sauron-db/src/lib.rs
```

Two things this step must NOT do, both found the hard way:

- **Do not regenerate `schema.rs`.** [`backend/diesel.toml`](../../../backend/diesel.toml) deliberately omits the `file =` key and documents three destructive effects of a regeneration — it emits a `table!` block per partition child, redeclares `error_events`' primary key, and reorders every `joinable!` list. All three compile cleanly, so no gate catches them. In any case no rollup table is in `schema.rs` at all: every rollup is accessed through raw `sql_query`, so `person_days` does not belong there either.
- **The `touch` is required, not cosmetic.** `sauron-db` has no `build.rs`, so `embed_migrations!("../../migrations")` gets no `cargo:rerun-if-changed` for a new migration directory. Without forcing a rebuild, the test binary keeps the old migration set, the ephemeral test database is built from a stale template, and the test fails with `person_days table missing` **after** the migration has demonstrably been applied — a confusing failure that looks like a broken migration.

- [ ] **Step 5: Run the test and the drift check**

```bash
cd backend && cargo test -p sauron-db --test person_days_rollup --test schema_drift
```

Expected: PASS, in nonzero time. Leave everything unstaged.

---

### Task 2: Fold person-days from both firehoses

**Files:**
- Create: `backend/crates/sauron-db/src/rollups/person_days.rs`
- Modify: `backend/crates/sauron-db/src/rollups/mod.rs` (add `pub mod person_days;` beside the existing modules)
- Modify: `backend/crates/sauron-db/src/rollups/fold.rs:133-137` (AnalyticsDeltas), `:296-299` (ErrorDeltas), `:153` (fold_analytics_rows), `:301` (fold_error_rows), `:651` (fold_analytics), `:712` (fold_errors)
- Test: `backend/crates/sauron-db/tests/person_days_rollup.rs`

**Interfaces:**
- Consumes: `person_days` from Task 1; existing `DayKey = (Uuid, Option<Uuid>, NaiveDate)`, `env_key`, `CHUNK` from `rollups::mod`.
- Produces:
  - `pub type PersonKey = (DayKey, String);` — the `String` is `distinct_id`.
  - `#[derive(Default, Clone)] pub struct PersonDayDelta { pub events: i64, pub errors: i64 }`
  - `pub async fn add_person_days(conn: &mut AsyncPgConnection, deltas: &BTreeMap<PersonKey, PersonDayDelta>) -> diesel::QueryResult<()>`
  - `AnalyticsDeltas.person_days` and `ErrorDeltas.person_days`, both `BTreeMap<PersonKey, PersonDayDelta>`

- [ ] **Step 1: Write the failing pure-logic test**

Add to `backend/crates/sauron-db/src/rollups/fold.rs`'s existing `#[cfg(test)] mod tests`:

```rust
#[test]
fn person_days_counts_events_per_person_day_and_skips_anonymous() {
    let app = Uuid::new_v4();
    let day = NaiveDate::from_ymd_opt(2026, 8, 28).unwrap();
    let at = Utc.from_utc_datetime(&day.and_hms_opt(9, 0, 0).unwrap());
    let rows = vec![
        arow(app, None, at, "view", "u1"),
        arow(app, None, at + Duration::hours(1), "click", "u1"),
        arow(app, None, at, "view", "u2"),
        // Anonymous: '' would otherwise become one giant shared "person",
        // exactly as the journey walk already guards against.
        arow(app, None, at, "view", ""),
    ];
    let mut sess = HashMap::new();
    let mut jour = HashMap::new();
    let d = fold_analytics_rows(&rows, &mut sess, &mut jour, 100);

    assert_eq!(d.person_days.len(), 2, "one row per person-day, anonymous dropped");
    let k = ((app, None, day), "u1".to_string());
    assert_eq!(d.person_days[&k].events, 2, "both of u1's events land on one day row");
    assert_eq!(d.person_days[&k].errors, 0);
}

#[test]
fn person_days_from_errors_sets_errors_not_events() {
    let app = Uuid::new_v4();
    let day = NaiveDate::from_ymd_opt(2026, 8, 28).unwrap();
    let at = Utc.from_utc_datetime(&day.and_hms_opt(9, 0, 0).unwrap());
    let d = fold_error_rows(&[erow(app, None, at, Some("u1"))]);

    let k = ((app, None, day), "u1".to_string());
    assert_eq!(d.person_days[&k].errors, 1, "error firehose sets errors");
    assert_eq!(d.person_days[&k].events, 0, "and never events");
}
```

If `arow`/`erow` constructors do not already exist in that test module, write them to build `ARow`/`ERow` literals with every field populated — read the struct definitions at `fold.rs:56` and `fold.rs` (ERow) and fill each field explicitly.

- [ ] **Step 2: Run and confirm it fails**

```bash
cd backend && cargo test -p sauron-db --lib rollups::fold::tests::person_days
```

Expected: FAIL — `no field person_days on AnalyticsDeltas`.

- [ ] **Step 3: Add the delta type and the upsert**

Create `backend/crates/sauron-db/src/rollups/person_days.rs`:

```rust
//! Per-person, per-day activity — retention's substrate.
//!
//! Unlike `add_user_activity`, this needs no read-modify-write round trip:
//! there is no sketch to merge in Rust, so it is a plain additive upsert in
//! the shape of `add_event_top`. That matters at this table's volume, which is
//! the one rollup that scales with users rather than with buckets.

use std::collections::BTreeMap;

use diesel::sql_types::{Array, BigInt, Date, Nullable, Text, Uuid as SqlUuid};
use diesel_async::{AsyncPgConnection, RunQueryDsl};

use super::{DayKey, CHUNK};

/// `(app, env, day, distinct_id)`.
pub type PersonKey = (DayKey, String);

#[derive(Default, Clone)]
pub struct PersonDayDelta {
    pub events: i64,
    pub errors: i64,
}

/// Additive upsert. Same-day collisions collapse onto one row via
/// `person_days_key`, which is what makes an identity merge a SET UNION of
/// days rather than a double count.
pub async fn add_person_days(
    conn: &mut AsyncPgConnection,
    deltas: &BTreeMap<PersonKey, PersonDayDelta>,
) -> diesel::QueryResult<()> {
    for chunk in deltas.iter().collect::<Vec<_>>().chunks(CHUNK) {
        diesel::sql_query(
            "INSERT INTO person_days (app_id, environment_id, distinct_id, day, events, errors) \
             SELECT app_id, env, distinct_id, day, events, errors \
             FROM unnest($1::uuid[], $2::uuid[], $3::text[], $4::date[], $5::bigint[], $6::bigint[]) \
                  AS t(app_id, env, distinct_id, day, events, errors) \
             ON CONFLICT (app_id, COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid), distinct_id, day) \
             DO UPDATE SET events = person_days.events + EXCLUDED.events, \
                           errors = person_days.errors + EXCLUDED.errors, \
                           updated_at = now()",
        )
        .bind::<Array<SqlUuid>, _>(chunk.iter().map(|((k, _), _)| k.0).collect::<Vec<_>>())
        .bind::<Array<Nullable<SqlUuid>>, _>(chunk.iter().map(|((k, _), _)| k.1).collect::<Vec<_>>())
        .bind::<Array<Text>, _>(chunk.iter().map(|((_, d), _)| d.clone()).collect::<Vec<_>>())
        .bind::<Array<Date>, _>(chunk.iter().map(|((k, _), _)| k.2).collect::<Vec<_>>())
        .bind::<Array<BigInt>, _>(chunk.iter().map(|(_, v)| v.events).collect::<Vec<_>>())
        .bind::<Array<BigInt>, _>(chunk.iter().map(|(_, v)| v.errors).collect::<Vec<_>>())
        .execute(conn)
        .await?;
    }
    Ok(())
}
```

In `rollups/mod.rs`, beside the existing `pub mod fold;`, add:

```rust
pub mod person_days;
pub use person_days::{add_person_days, PersonDayDelta, PersonKey};
```

- [ ] **Step 4: Wire it into both folds**

In `fold.rs`, add the field to both delta structs:

```rust
#[derive(Default)]
pub(crate) struct AnalyticsDeltas {
    pub screens: BTreeMap<(DayKey, String), ScreenDelta>,
    pub nodes: BTreeMap<(DayKey, i16, String), i64>,
    pub links: BTreeMap<(DayKey, i16, String, String), i64>,
    pub top: BTreeMap<(DayKey, String), i64>,
    pub activity: BTreeMap<DayKey, UserActivityDelta>,
    pub person_days: BTreeMap<PersonKey, PersonDayDelta>,
}
```

and the same `person_days` field on `ErrorDeltas`. Import `PersonDayDelta, PersonKey` from `super` alongside the existing `UserActivityDelta`.

In `fold_analytics_rows`, inside the existing per-row loop that already computes `key`, immediately after the `a.hll_analytics.insert(&r.distinct_id);` line, extend the same non-empty guard:

```rust
        if !r.distinct_id.is_empty() {
            a.hll_all.insert(&r.distinct_id);
            a.hll_analytics.insert(&r.distinct_id);
            d.person_days
                .entry((key, r.distinct_id.clone()))
                .or_default()
                .events += 1;
        }
```

In `fold_error_rows`, inside the existing `if let Some(did) = did` block:

```rust
        if let Some(did) = did {
            a.hll_all.insert(did);
            d.person_days
                .entry((key, did.to_string()))
                .or_default()
                .errors += 1;
        }
```

In `fold_analytics` (`fold.rs:651`), add one line directly after `add_user_activity(conn, &mut d.activity).await?;`:

```rust
        add_person_days(conn, &d.person_days).await?;
```

Add the identical line to `fold_errors` (`fold.rs:712`) after its own `add_user_activity` call. Import `add_person_days` in the `use super::{...}` list at `fold.rs:26-31`.

- [ ] **Step 5: Run the pure tests**

```bash
cd backend && cargo test -p sauron-db --lib rollups::fold::tests
```

Expected: PASS, including the two new cases.

- [ ] **Step 6: Write the equivalence test**

Add to `backend/crates/sauron-db/tests/person_days_rollup.rs`:

```rust
#[derive(diesel::QueryableByName)]
struct CountRow {
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    n: i64,
}

/// The fold must agree with counting the raw rows directly. This is the case
/// that catches double-counting -- a person active twice in one day is ONE
/// person-day, and a fold that runs twice over overlapping windows must not
/// invent a second.
#[tokio::test]
async fn folded_person_days_equal_direct_count() {
    let Some(db) = TestDb::setup().await else { return };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs().await;

    // Open the gate: fixtures pin the epoch forward, which would make the fold
    // select nothing and let this test pass while verifying nothing.
    diesel::sql_query("UPDATE person_days_epoch SET started_at = now() - interval '1 day'")
        .execute(&mut conn)
        .await
        .unwrap();
    diesel::sql_query(
        "UPDATE rollup_watermarks SET watermark = now() - interval '1 day'",
    )
    .execute(&mut conn)
    .await
    .unwrap();

    sauron_db::rollups::fold::fold_analytics(&mut conn, Utc::now(), 1000)
        .await
        .unwrap();

    let folded: CountRow = diesel::sql_query(
        "SELECT count(*) AS n FROM person_days WHERE app_id = $1",
    )
    .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
    .get_result(&mut conn)
    .await
    .unwrap();

    let direct: CountRow = diesel::sql_query(
        "SELECT count(*) AS n FROM ( \
           SELECT DISTINCT app_id, \
                  COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid), \
                  distinct_id, occurred_at::date \
           FROM analytics_events \
           WHERE app_id = $1 AND distinct_id <> '' \
         ) t",
    )
    .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
    .get_result(&mut conn)
    .await
    .unwrap();

    assert!(direct.n > 0, "fixture seeded no analytics rows -- test proves nothing");
    assert_eq!(folded.n, direct.n, "folded person-days disagree with direct count");

    db.cleanup().await;
}
```

- [ ] **Step 7: Run it**

```bash
cd backend && cargo test -p sauron-db --test person_days_rollup -- --nocapture
```

Expected: PASS in nonzero time, with `direct.n > 0` holding. If it completes in 0.00s the database was unreachable and nothing was proved.

---

### Task 3: Readiness gate

**Files:**
- Modify: `backend/crates/sauron-db/src/rollups/person_days.rs`
- Test: `backend/crates/sauron-db/tests/person_days_rollup.rs`

**Interfaces:**
- Consumes: `person_days_epoch`, `person_days_backfill` from Task 1.
- Produces:
  - `pub async fn is_ready(conn: &mut AsyncPgConnection, app_id: Uuid) -> diesel::QueryResult<bool>`
  - `pub async fn mark_all_backfilled(conn: &mut AsyncPgConnection) -> diesel::QueryResult<usize>`

- [ ] **Step 1: Write the failing test**

```rust
/// The gate must be CLOSED for an app that predates this feature's epoch and
/// has no marker -- otherwise the API answers 0% retention confidently.
#[tokio::test]
async fn gate_is_closed_for_unbackfilled_app_and_opens_on_marker() {
    let Some(db) = TestDb::setup().await else { return };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs().await;

    // Push this feature's epoch AFTER the app's creation: the app predates it,
    // so only a marker can make it ready.
    diesel::sql_query("UPDATE person_days_epoch SET started_at = now() + interval '1 hour'")
        .execute(&mut conn)
        .await
        .unwrap();

    let ready = sauron_db::rollups::person_days::is_ready(&mut conn, ids.app_id)
        .await
        .unwrap();
    assert!(!ready, "an app with no person_days marker must NOT report ready");

    sauron_db::rollups::person_days::mark_all_backfilled(&mut conn)
        .await
        .unwrap();
    let ready = sauron_db::rollups::person_days::is_ready(&mut conn, ids.app_id)
        .await
        .unwrap();
    assert!(ready, "marker written, app must now report ready");

    db.cleanup().await;
}

/// Regression guard for the trap this feature's testing section names: if the
/// fixture pin leaves the gate shut, every later assertion degenerates to
/// "empty == empty" and the suite goes green having checked nothing.
#[tokio::test]
async fn gate_opens_for_apps_created_after_the_epoch() {
    let Some(db) = TestDb::setup().await else { return };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs().await;

    diesel::sql_query("UPDATE person_days_epoch SET started_at = now() - interval '1 day'")
        .execute(&mut conn)
        .await
        .unwrap();

    let ready = sauron_db::rollups::person_days::is_ready(&mut conn, ids.app_id)
        .await
        .unwrap();
    assert!(ready, "app created after the epoch is implicitly ready");

    db.cleanup().await;
}
```

- [ ] **Step 2: Run and confirm it fails**

```bash
cd backend && cargo test -p sauron-db --test person_days_rollup gate_
```

Expected: FAIL — `is_ready` not found.

- [ ] **Step 3: Implement the gate**

Append to `rollups/person_days.rs`:

```rust
use chrono::{DateTime, Utc};
use uuid::Uuid;

#[derive(diesel::QueryableByName)]
struct BoolRow {
    #[diesel(sql_type = diesel::sql_types::Bool)]
    present: bool,
}

/// `rollups::is_ready`'s twin, over THIS feature's own marker.
///
/// Deliberately not `rollups::is_ready`: markers in `rollup_backfill` were
/// written by migration-71 backfills that never touched `person_days`, so
/// reusing them reports ready for an app whose person-days are empty.
pub async fn is_ready(conn: &mut AsyncPgConnection, app_id: Uuid) -> diesel::QueryResult<bool> {
    let r: BoolRow = diesel::sql_query(
        "SELECT EXISTS (SELECT 1 FROM person_days_backfill WHERE app_id = $1) \
             OR EXISTS (SELECT 1 FROM apps a, person_days_epoch e \
                        WHERE a.id = $1 AND a.created_at >= e.started_at) AS present",
    )
    .bind::<SqlUuid, _>(app_id)
    .get_result(conn)
    .await?;
    Ok(r.present)
}

/// Written by the backfill inside its FINAL transaction. The marker must never
/// be visible before the rows it claims.
pub async fn mark_all_backfilled(conn: &mut AsyncPgConnection) -> diesel::QueryResult<usize> {
    diesel::sql_query(
        "INSERT INTO person_days_backfill (app_id) SELECT id FROM apps \
         ON CONFLICT (app_id) DO UPDATE SET completed_at = now()",
    )
    .execute(conn)
    .await
}

pub async fn epoch(conn: &mut AsyncPgConnection) -> diesel::QueryResult<DateTime<Utc>> {
    #[derive(diesel::QueryableByName)]
    struct TsRow {
        #[diesel(sql_type = diesel::sql_types::Timestamptz)]
        t: DateTime<Utc>,
    }
    let r: TsRow = diesel::sql_query("SELECT started_at AS t FROM person_days_epoch")
        .get_result(conn)
        .await?;
    Ok(r.t)
}
```

- [ ] **Step 4: Run the tests**

```bash
cd backend && cargo test -p sauron-db --test person_days_rollup
```

Expected: PASS, all cases, nonzero elapsed time.

---

### Task 4: Identity merge must union days, not sum them

**Files:**
- Modify: `backend/crates/sauron-db/src/identity_merge.rs` (beside the `env_fold` statement at ~line 902 that rebuilds `event_user_environments`)
- Test: `backend/crates/sauron-db/tests/person_days_rollup.rs`

**Interfaces:**
- Consumes: `person_days` (Task 1), the additive-upsert semantics of `person_days_key` (Task 2).
- Produces: no new public API. Behavioural contract: after merging alias → person, the surviving person holds the **union** of both parties' active days, with counters summed per day.

- [ ] **Step 1: Write the failing test**

```rust
/// Guest active on days {D-2, D-1}, identified person already active on {D-1}.
/// After the merge the person must hold exactly {D-2, D-1} -- three rows
/// collapsing to two, with the shared day's counters SUMMED, not duplicated.
///
/// Without this hook every identify() inflates retention, and guest-then-
/// identify is the normal path, not an edge case.
#[tokio::test]
async fn identity_merge_unions_person_days() {
    let Some(db) = TestDb::setup().await else { return };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs().await;

    let d1 = (Utc::now() - chrono::Duration::days(2)).date_naive();
    let d2 = (Utc::now() - chrono::Duration::days(1)).date_naive();

    for (who, day, events) in [
        ("guest_abc", d1, 3i64),
        ("guest_abc", d2, 2),
        ("person_1", d2, 5),
    ] {
        diesel::sql_query(
            "INSERT INTO person_days (app_id, environment_id, distinct_id, day, events) \
             VALUES ($1, NULL, $2, $3, $4)",
        )
        .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
        .bind::<diesel::sql_types::Text, _>(who)
        .bind::<diesel::sql_types::Date, _>(day)
        .bind::<diesel::sql_types::BigInt, _>(events)
        .execute(&mut conn)
        .await
        .unwrap();
    }

    sauron_db::identity_merge::merge_alias(&mut conn, ids.app_id, "guest_abc", "person_1", 30)
        .await
        .unwrap();

    let rows: Vec<PersonDayRow> = diesel::sql_query(
        "SELECT day, events FROM person_days \
          WHERE app_id = $1 AND distinct_id = 'person_1' ORDER BY day",
    )
    .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
    .get_results(&mut conn)
    .await
    .unwrap();

    assert_eq!(rows.len(), 2, "days must UNION to two, not sum to three");
    assert_eq!(rows[0].events, 3, "the guest-only day carries over intact");
    assert_eq!(rows[1].events, 7, "the shared day sums 2 + 5");

    let leftover: CountRow = diesel::sql_query(
        "SELECT count(*) AS n FROM person_days WHERE app_id = $1 AND distinct_id = 'guest_abc'",
    )
    .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
    .get_result(&mut conn)
    .await
    .unwrap();
    assert_eq!(leftover.n, 0, "alias rows must not survive the merge");

    db.cleanup().await;
}
```

Add the row struct near the top of the test file:

```rust
#[derive(diesel::QueryableByName)]
struct PersonDayRow {
    #[diesel(sql_type = diesel::sql_types::Date)]
    day: chrono::NaiveDate,
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    events: i64,
}
```

Confirm the exact name and signature of the merge entry point before writing the call — read `identity_merge.rs` and use whatever the surrounding tests in `backend/crates/sauron-db/tests/identity_merge.rs` already call. Do not invent one.

- [ ] **Step 2: Run and confirm it fails**

```bash
cd backend && cargo test -p sauron-db --test person_days_rollup identity_merge_unions
```

Expected: FAIL — three rows survive, or alias rows remain.

- [ ] **Step 3: Add the merge statement**

In `identity_merge.rs`, directly after the `env_fold` statement executes, add:

```rust
    // Person-days move with the identity, and they UNION rather than sum.
    //
    // The DELETE ... RETURNING + INSERT ... ON CONFLICT shape is the same one
    // `env_fold` above uses, and the union falls out of `person_days_key`: the
    // alias and the person having both been active on one day is a conflict,
    // so their two rows collapse into one with the counters added. A plain
    // UPDATE of distinct_id would instead raise a unique violation on exactly
    // that overlap -- which is the common case, not the rare one, since an
    // identify() typically happens on a day the guest was already active.
    diesel::sql_query(format!(
        "WITH moved AS ( \
             DELETE FROM person_days \
              WHERE app_id = $1 AND distinct_id = $2 \
             RETURNING environment_id, day, events, errors) \
         INSERT INTO person_days (app_id, environment_id, distinct_id, day, events, errors) \
         SELECT $1, environment_id, $3, day, events, errors FROM moved \
         ON CONFLICT (app_id, COALESCE(environment_id, {NIL}), distinct_id, day) \
         DO UPDATE SET events = person_days.events + EXCLUDED.events, \
                       errors = person_days.errors + EXCLUDED.errors, \
                       updated_at = now()"
    ))
    .bind::<SqlUuid, _>(app_id)
    .bind::<Text, _>(alias)
    .bind::<Text, _>(person)
    .execute(conn)
    .await?;
```

`NIL` is the existing constant this file already interpolates into `env_fold`; reuse it rather than retyping the literal.

- [ ] **Step 4: Run the tests**

```bash
cd backend && cargo test -p sauron-db --test person_days_rollup --test identity_merge
```

Expected: PASS — both the new case and every pre-existing identity-merge test.

---

### Task 5: GDPR erasure must reach person_days

**Files:**
- Modify: `backend/crates/sauron-db/src/purge.rs` (beside the `event_user_environments` deletion at ~line 1030)
- Test: `backend/crates/sauron-db/tests/data_purge.rs`

**Interfaces:**
- Consumes: `person_days` (Task 1).
- Produces: no new public API. Contract: erasing a person removes their `person_days` rows.

- [ ] **Step 1: Write the failing test**

Add to `backend/crates/sauron-db/tests/data_purge.rs`:

```rust
/// Erasure must not leave per-person daily activity behind. person_days is
/// keyed by distinct_id and is therefore personal data in its own right.
#[tokio::test]
async fn erasure_removes_person_days() {
    let Some(db) = TestDb::setup().await else { return };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs().await;

    diesel::sql_query(
        "INSERT INTO person_days (app_id, environment_id, distinct_id, day, events) \
         VALUES ($1, NULL, 'erase_me', current_date, 4)",
    )
    .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
    .execute(&mut conn)
    .await
    .unwrap();

    // Use the same erasure entry point data_purge.rs's existing tests call.
    run_person_erasure(&mut conn, ids.app_id, "erase_me").await;

    let left: CountRow = diesel::sql_query(
        "SELECT count(*) AS n FROM person_days WHERE app_id = $1 AND distinct_id = 'erase_me'",
    )
    .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
    .get_result(&mut conn)
    .await
    .unwrap();
    assert_eq!(left.n, 0, "person_days survived an erasure");

    db.cleanup().await;
}
```

Replace `run_person_erasure` with the actual helper or public function the neighbouring tests in that file already use — read them first and match.

- [ ] **Step 2: Run and confirm it fails**

```bash
cd backend && cargo test -p sauron-db --test data_purge erasure_removes_person_days
```

Expected: FAIL with `left.n == 1`.

- [ ] **Step 3: Add the delete**

In `purge.rs`, immediately after the `DELETE FROM event_user_environments …` statement that removes environments with no surviving rows:

```rust
    // person_days is keyed by distinct_id, so it is personal data and follows
    // event_user_environments out. An unconditional delete rather than the
    // recompute-from-survivors shape above: there are no survivors to
    // recompute from once the raw rows are gone, and a stale daily activity
    // row for an erased person is precisely what erasure must remove.
    diesel::sql_query("DELETE FROM person_days WHERE app_id = $1 AND distinct_id = $2")
        .bind::<SqlUuid, _>(app_id)
        .bind::<Text, _>(key)
        .execute(conn)
        .await?;
```

- [ ] **Step 4: Run the tests**

```bash
cd backend && cargo test -p sauron-db --test data_purge
```

Expected: PASS, including every pre-existing purge case.

---

### Task 6: Operator-run backfill

**Files:**
- Create: `backend/crates/sauron-db/src/person_days_backfill.rs`
- Modify: `backend/crates/sauron-db/src/lib.rs` (add `pub mod person_days_backfill;`)
- Modify: `backend/bins/sauron-migrate/src/main.rs:22` (the known-subcommand list) and the dispatch block near `:90`
- Test: `backend/crates/sauron-db/tests/person_days_rollup.rs`

**Interfaces:**
- Consumes: `person_days_epoch` (Task 1), `mark_all_backfilled` (Task 3).
- Produces: `pub async fn backfill_all(pool: &crate::PgPool) -> anyhow::Result<()>`

- [ ] **Step 1: Write the failing test**

```rust
/// The backfill covers (-inf, cutoff] and the live fold covers (cutoff, inf).
/// Their disjointness is a property of the CUTOFF being the instant the live
/// path started counting -- so a day spanning both must be counted ONCE.
#[tokio::test]
async fn backfill_is_additive_and_does_not_double_count() {
    let Some(db) = TestDb::setup().await else { return };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs().await;

    // Pretend the live path started an hour ago: rows before that are the
    // backfill's, rows after are the fold's.
    diesel::sql_query("UPDATE person_days_epoch SET started_at = now() - interval '1 hour'")
        .execute(&mut conn)
        .await
        .unwrap();
    diesel::sql_query(
        "UPDATE rollup_watermarks SET watermark = (SELECT started_at FROM person_days_epoch)",
    )
    .execute(&mut conn)
    .await
    .unwrap();

    sauron_db::rollups::fold::fold_analytics(&mut conn, Utc::now(), 1000)
        .await
        .unwrap();
    sauron_db::person_days_backfill::backfill_all(db.pool())
        .await
        .unwrap();

    // Every (person, day) appears at most once -- the unique index guarantees
    // it structurally, so assert the COUNTERS instead, which is where a
    // double count would actually show.
    let bad: CountRow = diesel::sql_query(
        "SELECT count(*) AS n FROM ( \
           SELECT p.distinct_id, p.day, p.events, \
                  (SELECT count(*) FROM analytics_events a \
                    WHERE a.app_id = p.app_id AND a.distinct_id = p.distinct_id \
                      AND a.occurred_at::date = p.day \
                      AND a.environment_id IS NOT DISTINCT FROM p.environment_id) AS raw \
             FROM person_days p WHERE p.app_id = $1 \
         ) t WHERE events <> raw",
    )
    .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
    .get_result(&mut conn)
    .await
    .unwrap();
    assert_eq!(bad.n, 0, "a person-day's counter disagrees with the raw rows behind it");

    let ready = sauron_db::rollups::person_days::is_ready(&mut conn, ids.app_id)
        .await
        .unwrap();
    assert!(ready, "backfill must write its readiness marker");

    db.cleanup().await;
}
```

- [ ] **Step 2: Run and confirm it fails**

```bash
cd backend && cargo test -p sauron-db --test person_days_rollup backfill_is_additive
```

Expected: FAIL — module not found.

- [ ] **Step 3: Write the backfill**

Create `backend/crates/sauron-db/src/person_days_backfill.rs`:

```rust
//! Populate `person_days` for data that predates its epoch.
//!
//! Not part of a migration and not part of `sauron-migrate`'s default no-arg
//! path, for the reason `person_env_backfill` records: `require_current_schema`
//! fail-closes the API on a stale schema and every RPM daemon `Requires=` the
//! migrator unit, so anything slow in either place is a boot outage
//! proportional to retained data.
//!
//! ## Additive against a cutoff, NOT `ON CONFLICT DO NOTHING`
//!
//! The live fold bumps `person_days` from the moment migration 74 lands,
//! including for apps this backfill has not reached, so a live bump can create
//! a row before the backfill gets there. `DO NOTHING` would then skip it and
//! leave that person short by their entire pre-epoch history -- silently, and
//! permanently. This aggregates only rows strictly before the cutoff and ADDS
//! them; live bumps carry rows at or after the cutoff, so the two sets are
//! disjoint and the addition is exact.
//!
//! That disjointness is a property of the CUTOFF, not of this SQL: it holds
//! only when the cutoff is the instant the live path started counting, which
//! is why it reads `person_days_epoch` and is not `Utc::now()`.
//!
//! KNOWN RESIDUAL, inherited unchanged from `person_env_backfill`: a backdated
//! event -- an SDK offline queue replaying with an old `occurred_at` -- that
//! arrives between the cutoff and this finishing is counted twice. Bounded by
//! the backfill's duration, and disclosed rather than fixed.

use diesel_async::{AsyncPgConnection, RunQueryDsl};

use crate::rollups::person_days::mark_all_backfilled;

/// One day per statement, oldest first, so a long run makes visible progress
/// and a crash resumes cheaply.
pub async fn backfill_all(pool: &crate::PgPool) -> anyhow::Result<()> {
    let mut conn = pool.get().await?;
    let cutoff = crate::rollups::person_days::epoch(&mut conn).await?;

    for (table, col) in [("analytics_events", "events"), ("error_events", "errors")] {
        let sql = format!(
            "INSERT INTO person_days (app_id, environment_id, distinct_id, day, {col}) \
             SELECT app_id, environment_id, distinct_id, occurred_at::date, count(*) \
               FROM {table} \
              WHERE received_at < $1 AND distinct_id IS NOT NULL AND distinct_id <> '' \
              GROUP BY app_id, environment_id, distinct_id, occurred_at::date \
             ON CONFLICT (app_id, COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid), distinct_id, day) \
             DO UPDATE SET {col} = person_days.{col} + EXCLUDED.{col}, updated_at = now()"
        );
        diesel::sql_query(sql)
            .bind::<diesel::sql_types::Timestamptz, _>(cutoff)
            .execute(&mut conn)
            .await?;
        tracing::info!(%table, "person-days backfill: table complete");
    }

    // The marker LAST, and only after both tables landed: it must never be
    // visible before the rows it claims.
    mark_all_backfilled(&mut conn).await?;
    tracing::info!("person-days backfill complete");
    Ok(())
}
```

Add `pub mod person_days_backfill;` to `backend/crates/sauron-db/src/lib.rs` beside `pub mod person_env_backfill;`.

- [ ] **Step 4: Add the subcommand**

In `backend/bins/sauron-migrate/src/main.rs`, add `"backfill-person-days"` to the known-subcommand list at line 22, and mirror the existing `backfill-person-envs` dispatch block:

```rust
    if std::env::args().any(|a| a == "backfill-person-days") {
        tracing::info!("running person-days backfill");
        sauron_db::person_days_backfill::backfill_all(&pool).await?;
        return Ok(());
    }
```

- [ ] **Step 5: Run the tests**

```bash
cd backend && cargo test -p sauron-db --test person_days_rollup
```

Expected: PASS, all cases, nonzero elapsed time.

- [ ] **Step 6: Confirm the subcommand is recognised**

```bash
cd backend && cargo run -p sauron-migrate -- backfill-person-days --help 2>&1 | head -5
```

Expected: it does not report an unknown subcommand.

---

### Task 7: The three read queries

**Files:**
- Create: `backend/crates/sauron-db/src/retention.rs`
- Modify: `backend/crates/sauron-db/src/lib.rs` (add `pub mod retention;`)
- Test: `backend/crates/sauron-db/tests/person_days_rollup.rs`

**Interfaces:**
- Consumes: `person_days` (Task 1), `event_user_environments` (existing), `sauron_db::scope::EnvFilter` (existing).
- Produces:
  ```rust
  pub enum Granularity { Day, Week }
  pub struct CohortRow { pub cohort: NaiveDate, pub size: i64, pub period: i32, pub users: i64 }
  pub struct LifecyclePoint { pub start: NaiveDate, pub new_users: i64, pub returning_users: i64,
                              pub resurrected_users: i64, pub dormant_users: i64 }
  pub async fn retention_grid(conn, scope, g: Granularity, from: NaiveDate, to: NaiveDate,
                              periods: i32, errors_only: Option<bool>) -> QueryResult<Vec<CohortRow>>
  pub async fn lifecycle(conn, scope, g: Granularity, from: NaiveDate, to: NaiveDate)
                              -> QueryResult<Vec<LifecyclePoint>>
  ```

- [ ] **Step 1: Write the failing test**

```rust
/// Two users, both first seen on day 0. One returns on day 2, one never does.
/// Period 0 must be the cohort SIZE (2), period 2 must be 1, and period 1 --
/// which nobody was active in -- must be absent from the rows, so the caller
/// renders 0 rather than inventing a null.
#[tokio::test]
async fn retention_grid_counts_returners_by_period() {
    let Some(db) = TestDb::setup().await else { return };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs().await;

    let d0 = (Utc::now() - chrono::Duration::days(6)).date_naive();
    for (who, offset) in [("r1", 0i64), ("r2", 0), ("r1", 2)] {
        let day = d0 + chrono::Duration::days(offset);
        diesel::sql_query(
            "INSERT INTO person_days (app_id, environment_id, distinct_id, day, events) \
             VALUES ($1, NULL, $2, $3, 1) \
             ON CONFLICT (app_id, COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid), distinct_id, day) \
             DO UPDATE SET events = person_days.events + 1",
        )
        .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
        .bind::<diesel::sql_types::Text, _>(who)
        .bind::<diesel::sql_types::Date, _>(day)
        .execute(&mut conn)
        .await
        .unwrap();
    }
    for who in ["r1", "r2"] {
        diesel::sql_query(
            "INSERT INTO event_user_environments \
               (app_id, distinct_id, environment_id, first_seen, last_seen) \
             VALUES ($1, $2, NULL, $3, $3)",
        )
        .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
        .bind::<diesel::sql_types::Text, _>(who)
        .bind::<diesel::sql_types::Timestamptz, _>(
            d0.and_hms_opt(0, 0, 0).unwrap().and_utc(),
        )
        .execute(&mut conn)
        .await
        .unwrap();
    }

    let rows = sauron_db::retention::retention_grid(
        &mut conn,
        /* scope */ ids.app_scope(),
        sauron_db::retention::Granularity::Day,
        d0,
        d0 + chrono::Duration::days(1),
        7,
        None,
    )
    .await
    .unwrap();

    let p0 = rows.iter().find(|r| r.period == 0).expect("period 0 missing");
    assert_eq!(p0.size, 2, "cohort size");
    assert_eq!(p0.users, 2, "everyone is active in period 0 by construction");
    let p2 = rows.iter().find(|r| r.period == 2).expect("period 2 missing");
    assert_eq!(p2.users, 1, "only r1 returned on day 2");
    assert!(rows.iter().all(|r| r.period != 1), "period 1 has no returners and must not be emitted");

    db.cleanup().await;
}
```

Match `ids.app_scope()` to whatever the existing tests use to build the scope argument the other repo functions take — read `backend/crates/sauron-db/tests/common/mod.rs` and the signature of an existing scoped repo call such as `repo::user_stats` before writing this line.

- [ ] **Step 2: Run and confirm it fails**

```bash
cd backend && cargo test -p sauron-db --test person_days_rollup retention_grid_counts
```

Expected: FAIL — module `retention` not found.

- [ ] **Step 3: Write the grid query**

Create `backend/crates/sauron-db/src/retention.rs` with `retention_grid`. The SQL:

```sql
WITH cohort AS (
    SELECT distinct_id,
           CASE WHEN $6 = 'week'
                THEN (date_trunc('week', MIN(first_seen)))::date
                ELSE (MIN(first_seen))::date END AS c
      FROM event_user_environments
     WHERE app_id = $1 AND ($2::uuid IS NULL OR environment_id = $2)
     GROUP BY distinct_id
),
windowed AS (
    SELECT * FROM cohort WHERE c >= $3 AND c < $4
),
sized AS (
    SELECT c, count(*) AS size FROM windowed GROUP BY c
),
ret AS (
    SELECT w.c,
           CASE WHEN $6 = 'week' THEN (d.day - w.c) / 7 ELSE (d.day - w.c) END AS period,
           count(DISTINCT d.distinct_id) AS users
      FROM windowed w
      JOIN person_days d
        ON d.app_id = $1
       AND ($2::uuid IS NULL OR d.environment_id = $2)
       AND d.distinct_id = w.distinct_id
       AND d.day >= w.c
       AND d.day < w.c + ($5::int * CASE WHEN $6 = 'week' THEN 7 ELSE 1 END)
     GROUP BY 1, 2
)
SELECT s.c AS cohort, s.size, r.period, r.users
  FROM sized s JOIN ret r ON r.c = s.c
 ORDER BY s.c, r.period
```

Two details that are load-bearing and must not be "simplified" away:

- `MIN(first_seen)` in the `cohort` CTE, **not** a bare `first_seen`. `event_user_environments` holds one row per environment, so on an unscoped request a person has several `first_seen` values, and taking any one of them places the same person in a different cohort depending on which row the planner reached first.
- `count(DISTINCT d.distinct_id)`, **not** `count(*)`. A person active in two environments on one day has two `person_days` rows; summing them reports retention above 100%.

Under an environment-scoped request both collapse to the single matching row and cost nothing.

For the `errors_only` variant, add `AND d.errors > 0` to the join for the exposed curve, and `AND d.errors = 0` for the clean one — but **only on the period-0 row**, so the split is by exposure in the first period rather than over the whole window. Implement it as an extra CTE:

```sql
exposed AS (
    SELECT w.distinct_id
      FROM windowed w JOIN person_days d
        ON d.app_id = $1 AND ($2::uuid IS NULL OR d.environment_id = $2)
       AND d.distinct_id = w.distinct_id
       AND d.day >= w.c
       AND d.day < w.c + (CASE WHEN $6 = 'week' THEN 7 ELSE 1 END)
     GROUP BY w.distinct_id
    HAVING sum(d.errors) > 0
)
```

then filter `windowed` by `distinct_id IN (SELECT …)` or `NOT IN`, per the requested side. Measuring exposure only in period 0 is what keeps the comparison from being circular: users who churn early cannot accumulate later error exposure, so a whole-window split would manufacture the correlation it claims to find.

- [ ] **Step 4: Run the grid test**

```bash
cd backend && cargo test -p sauron-db --test person_days_rollup retention_grid_counts
```

Expected: PASS.

- [ ] **Step 5: Write the failing lifecycle test**

```rust
/// u_new first seen today; u_ret active yesterday and today; u_res active
/// three days ago and today; u_dorm active yesterday only.
#[tokio::test]
async fn lifecycle_classifies_each_person_exactly_once() {
    let Some(db) = TestDb::setup().await else { return };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs().await;

    let today = Utc::now().date_naive();
    let seed = [
        ("u_new", vec![0i64], 0i64),
        ("u_ret", vec![-1, 0], -1),
        ("u_res", vec![-3, 0], -3),
        ("u_dorm", vec![-1], -1),
    ];
    for (who, days, first) in seed {
        for off in days {
            diesel::sql_query(
                "INSERT INTO person_days (app_id, environment_id, distinct_id, day, events) \
                 VALUES ($1, NULL, $2, $3, 1)",
            )
            .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
            .bind::<diesel::sql_types::Text, _>(who)
            .bind::<diesel::sql_types::Date, _>(today + chrono::Duration::days(off))
            .execute(&mut conn).await.unwrap();
        }
        diesel::sql_query(
            "INSERT INTO event_user_environments \
               (app_id, distinct_id, environment_id, first_seen, last_seen) \
             VALUES ($1, $2, NULL, $3, $3)",
        )
        .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
        .bind::<diesel::sql_types::Text, _>(who)
        .bind::<diesel::sql_types::Timestamptz, _>(
            (today + chrono::Duration::days(first)).and_hms_opt(0, 0, 0).unwrap().and_utc(),
        )
        .execute(&mut conn).await.unwrap();
    }

    let pts = sauron_db::retention::lifecycle(
        &mut conn,
        ids.app_scope(),
        sauron_db::retention::Granularity::Day,
        today - chrono::Duration::days(4),
        today + chrono::Duration::days(1),
    )
    .await
    .unwrap();

    let t = pts.iter().find(|p| p.start == today).expect("today missing");
    assert_eq!(t.new_users, 1, "u_new");
    assert_eq!(t.returning_users, 1, "u_ret");
    assert_eq!(t.resurrected_users, 1, "u_res");
    assert_eq!(t.dormant_users, 1, "u_dorm was active yesterday and is silent today");
    assert_eq!(
        t.new_users + t.returning_users + t.resurrected_users, 3,
        "the three active classes partition today's actives -- no double count"
    );

    db.cleanup().await;
}
```

- [ ] **Step 6: Implement `lifecycle`**

The three active classes must **partition** the active set — that is what the last assertion pins. The classification, given bucket `b` and previous bucket `p`:

- `new` — the person's first bucket **is** `b`.
- `returning` — not new, and active in `p`.
- `resurrected` — not new, and not active in `p`. (No "active sometime before" subquery is needed: not being new means their first bucket precedes `b`, so they were active before by definition.)
- `dormant` — active in `p`, silent in `b`. Counted separately and rendered negative; it is *not* part of the partition.

```sql
WITH ab AS (
    SELECT DISTINCT distinct_id,
           CASE WHEN $5 = 'week' THEN (date_trunc('week', day::timestamp))::date ELSE day END AS b
      FROM person_days
     WHERE app_id = $1 AND ($2::uuid IS NULL OR environment_id = $2)
       AND day >= $3 AND day < $4
),
fb AS (
    SELECT distinct_id,
           CASE WHEN $5 = 'week' THEN (date_trunc('week', MIN(first_seen)))::date
                ELSE (MIN(first_seen))::date END AS b
      FROM event_user_environments
     WHERE app_id = $1 AND ($2::uuid IS NULL OR environment_id = $2)
     GROUP BY distinct_id
),
stepped AS (
    SELECT ab.b,
           ab.distinct_id,
           fb.b = ab.b AS is_new,
           EXISTS (SELECT 1 FROM ab p
                    WHERE p.distinct_id = ab.distinct_id
                      AND p.b = ab.b - (CASE WHEN $5 = 'week' THEN 7 ELSE 1 END)) AS was_prev
      FROM ab JOIN fb USING (distinct_id)
),
dorm AS (
    SELECT p.b + (CASE WHEN $5 = 'week' THEN 7 ELSE 1 END) AS b, count(*) AS n
      FROM ab p
     WHERE NOT EXISTS (SELECT 1 FROM ab q
                        WHERE q.distinct_id = p.distinct_id
                          AND q.b = p.b + (CASE WHEN $5 = 'week' THEN 7 ELSE 1 END))
     GROUP BY 1
)
SELECT s.b AS start,
       count(*) FILTER (WHERE s.is_new)                           AS new_users,
       count(*) FILTER (WHERE NOT s.is_new AND s.was_prev)        AS returning_users,
       count(*) FILTER (WHERE NOT s.is_new AND NOT s.was_prev)    AS resurrected_users,
       COALESCE(max(d.n), 0)                                      AS dormant_users
  FROM stepped s LEFT JOIN dorm d ON d.b = s.b
 GROUP BY s.b
 ORDER BY s.b
```

- [ ] **Step 7: Run both query tests**

```bash
cd backend && cargo test -p sauron-db --test person_days_rollup
```

Expected: PASS, every case, nonzero elapsed time.

---

### Task 8: API routes

**Files:**
- Create: `backend/bins/sauron-api/src/routes/retention.rs`
- Modify: `backend/bins/sauron-api/src/routes/mod.rs` (add `pub mod retention;` beside `pub mod journeys;` at line 17)
- Modify: `backend/bins/sauron-api/src/main.rs` (three route registrations beside the analytics ones at ~line 695)
- Test: `backend/bins/sauron-api/tests/http_retention.rs`

**Interfaces:**
- Consumes: `sauron_db::retention::{retention_grid, lifecycle, Granularity}` (Task 7), `rollups::person_days::is_ready` (Task 3), `scope::authorized_read_scope` (existing).
- Produces: `GET /v1/apps/{app_id}/retention`, `…/retention/lifecycle`, `…/retention/churn`.

- [ ] **Step 1: Write the failing tests**

Create `backend/bins/sauron-api/tests/http_retention.rs`, following the setup used by `http_active_users.rs`:

```rust
/// An app that predates the epoch with no backfill marker must report
/// ready:false. Answering 0% would look like data and be wrong.
#[tokio::test]
async fn unbackfilled_app_reports_not_ready() {
    let Some(h) = harness().await else { return };
    h.exec("UPDATE person_days_epoch SET started_at = now() + interval '1 hour'").await;

    let body = h.get_json(&format!("/v1/apps/{}/retention?since_days=30", h.app_id)).await;
    assert_eq!(body["ready"], serde_json::json!(false));
    assert!(body["cohorts"].as_array().unwrap().is_empty(),
            "a not-ready response must not ship cohort rows");
}

/// The cap is on the PRODUCT. 30 cohorts and 30 periods are each individually
/// reasonable; their 900 cells are not. active_users.rs learned this exact
/// lesson with MAX_SCAN_BUDGET.
#[tokio::test]
async fn cell_budget_rejects_large_products() {
    let Some(h) = harness().await else { return };
    let status = h.get_status(&format!(
        "/v1/apps/{}/retention?granularity=day&cohorts=30&periods=30", h.app_id)).await;
    assert_eq!(status, 400, "30 x 30 = 900 cells must be rejected");

    let status = h.get_status(&format!(
        "/v1/apps/{}/retention?granularity=day&cohorts=12&periods=12", h.app_id)).await;
    assert_ne!(status, 400, "12 x 12 = 144 cells is inside the budget");
}

/// A period that has not elapsed yet is null, never 0. This is the single most
/// common retention-chart bug and the wire type is what prevents it.
#[tokio::test]
async fn future_periods_are_null_not_zero() {
    let Some(h) = harness().await else { return };
    h.exec("UPDATE person_days_epoch SET started_at = now() - interval '1 day'").await;
    h.seed_cohort_today().await;

    let body = h.get_json(&format!(
        "/v1/apps/{}/retention?granularity=day&cohorts=1&periods=7", h.app_id)).await;
    let periods = body["cohorts"][0]["periods"].as_array().unwrap();
    assert!(periods[0].is_number(), "period 0 is knowable today");
    assert!(periods[6].is_null(), "period 6 has not elapsed and must be null, not 0");
}
```

Write `harness()`, `h.exec`, `h.get_json`, `h.get_status` and `h.seed_cohort_today` by copying the equivalents from `backend/bins/sauron-api/tests/http_active_users.rs` — read that file first and reuse its shapes rather than inventing new ones. Note the shared-Redis hazard: if these return 429 instead of 401/200, leftover `sauron:auth:*` counters are the cause; `DEL` those keys, never `flushall`.

- [ ] **Step 2: Run and confirm they fail**

```bash
cd backend && cargo test -p sauron-api --test http_retention
```

Expected: FAIL — route not found (404). A 0.00s run means no test database; fix that first.

- [ ] **Step 3: Write the handlers**

Create `backend/bins/sauron-api/src/routes/retention.rs`:

```rust
//! Cohort retention, lifecycle and churn.
//!
//! Its own module rather than more `analytics.rs`, which is already 1,768
//! lines. The authorization shape is the same as its neighbours there:
//! `authorized_read_scope` on `EVENT_READ`, with `environment_id` read from
//! `RawQuery` -- NOT as a `Query<T>` field, per `routes::scope`'s module docs.

use axum::extract::{Path, Query, RawQuery, State};
use axum::Json;
use chrono::{Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use sauron_auth::{perm, AuthUser};
use sauron_db::retention::{self, Granularity};
use sauron_db::rollups::person_days;

use super::db;
use crate::error::ApiError;
use crate::AppState;

/// Cap on cohorts x periods -- the thing actually being handed out.
///
/// Bounding the two dimensions independently does not bound their product;
/// `active_users.rs`'s MAX_SCAN_BUDGET records the same lesson after 20 apps x
/// 92 days turned into 1,840 partition-day scans.
const MAX_RETENTION_CELLS: i64 = 400;

#[derive(Deserialize)]
pub struct GridQuery {
    #[serde(default = "default_granularity")]
    pub granularity: String,
    #[serde(default = "default_cohorts")]
    pub cohorts: i64,
    #[serde(default = "default_periods")]
    pub periods: i64,
    #[serde(default)]
    pub split: Option<String>,
    // `environment_id` is deliberately NOT a field here -- see the module docs.
}

fn default_granularity() -> String { "day".into() }
fn default_cohorts() -> i64 { 12 }
fn default_periods() -> i64 { 12 }

#[derive(Serialize)]
pub struct Cohort {
    pub start: NaiveDate,
    pub size: i64,
    /// `None` means NOT KNOWABLE YET, and serializes as JSON `null`. It is a
    /// different fact from zero, and the type is what stops a client
    /// rendering an unelapsed period as 0% retention.
    pub periods: Vec<Option<i64>>,
}

#[derive(Serialize)]
pub struct GridOut {
    pub granularity: String,
    pub as_of: Option<chrono::DateTime<Utc>>,
    /// False when this app's pre-epoch history has not been backfilled. The
    /// dashboard renders the backfill command instead of an empty grid.
    pub ready: bool,
    pub cohorts: Vec<Cohort>,
}

pub async fn grid(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<GridQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<GridOut>, ApiError> {
    let mut conn = db(&state).await?;
    let scope = super::scope::authorized_read_scope(
        &mut conn, auth.user_id, app_id, perm::EVENT_READ, raw_query.as_deref(),
    ).await?;

    let g = match q.granularity.as_str() {
        "week" => Granularity::Week,
        "day" => Granularity::Day,
        other => return Err(ApiError::bad_request(format!(
            "granularity must be 'day' or 'week', got '{other}'"))),
    };
    let cohorts = q.cohorts.clamp(1, 52);
    let periods = q.periods.clamp(1, 52);
    if cohorts * periods > MAX_RETENTION_CELLS {
        return Err(ApiError::bad_request(format!(
            "cohorts x periods must not exceed {MAX_RETENTION_CELLS}; \
             got {cohorts} x {periods} = {}", cohorts * periods)));
    }

    let ready = person_days::is_ready(&mut conn, app_id).await?;
    let as_of = sauron_db::rollups::as_of(&mut conn, &["analytics_events", "error_events"]).await?;
    if !ready {
        return Ok(Json(GridOut { granularity: q.granularity, as_of, ready, cohorts: vec![] }));
    }

    let step = if matches!(g, Granularity::Week) { 7 } else { 1 };
    let today = Utc::now().date_naive();
    let to = today + Duration::days(1);
    let from = to - Duration::days(cohorts * step);

    let errors_only = match q.split.as_deref() {
        None | Some("none") => None,
        Some("errors") => Some(true),
        Some(other) => return Err(ApiError::bad_request(format!(
            "split must be 'none' or 'errors', got '{other}'"))),
    };

    let rows = retention::retention_grid(
        &mut conn, scope, g, from, to, periods as i32, errors_only,
    ).await?;

    // Fold the flat (cohort, period, users) rows into dense per-cohort vectors,
    // and mark each cell that has not elapsed as `None` rather than 0.
    //
    // The completeness test is on the period's END: period n of a cohort that
    // started on `start` is only knowable once `start + (n+1)*step` has passed.
    let as_of_day = as_of.map(|t| t.date_naive()).unwrap_or(today);
    let mut out: Vec<Cohort> = Vec::new();
    for row in &rows {
        if out.last().map(|c| c.start) != Some(row.cohort) {
            out.push(Cohort { start: row.cohort, size: row.size, periods: vec![Some(0); periods as usize] });
        }
        let c = out.last_mut().expect("pushed above");
        if let Some(slot) = c.periods.get_mut(row.period as usize) {
            *slot = Some(row.users);
        }
    }
    for c in &mut out {
        for (n, slot) in c.periods.iter_mut().enumerate() {
            let ends = c.start + Duration::days((n as i64 + 1) * step);
            if ends > as_of_day {
                *slot = None;
            }
        }
    }

    Ok(Json(GridOut { granularity: q.granularity, as_of, ready, cohorts: out }))
}
```

Write `lifecycle` and `churn` in the same file on the same skeleton: same extractor set, same `authorized_read_scope` call, same `ready`/`as_of` envelope. `churn` takes `silent_periods` (default 4, clamped 1..52) at the same granularity, reads `event_user_environments.last_seen`, and paginates by keyset with a hard `LIMIT` — copy the keyset shape from `routes/analytics.rs`'s `persons_list` rather than inventing one.

- [ ] **Step 4: Register the routes**

In `routes/mod.rs` add `pub mod retention;`. In `main.rs`, beside the existing analytics registrations:

```rust
        .route(
            "/v1/apps/{app_id}/retention",
            get(routes::retention::grid),
        )
        .route(
            "/v1/apps/{app_id}/retention/lifecycle",
            get(routes::retention::lifecycle),
        )
        .route(
            "/v1/apps/{app_id}/retention/churn",
            get(routes::retention::churn),
        )
```

- [ ] **Step 5: Run the API tests**

```bash
cd backend && cargo test -p sauron-api --test http_retention
```

Expected: PASS, nonzero elapsed time.

- [ ] **Step 6: Run the env-scoping conformance suite**

```bash
cd backend && cargo test -p sauron-api --test http_env_scoping
```

Expected: PASS. This suite greps route files for `reject_environment_id` and env-aware shapes; a new env-aware endpoint that does not follow the convention fails here. Note it needs `TEST_REDIS_URL` — without it the suite prints a green line having run nothing, so check the elapsed time.

---

### Task 9: Dashboard API client and types

**Files:**
- Create: `dashboard/src/lib/api/retention.ts`
- Modify: `dashboard/src/lib/models/index.ts` (export the new types)
- Test: `dashboard/src/lib/api/retention.test.ts`

**Interfaces:**
- Consumes: the three endpoints from Task 8.
- Produces:
  ```ts
  export interface Cohort { start: string; size: number; periods: (number | null)[] }
  export interface RetentionGrid { granularity: 'day' | 'week'; as_of: string | null; ready: boolean; cohorts: Cohort[] }
  export interface LifecyclePoint { start: string; new_users: number; returning_users: number; resurrected_users: number; dormant_users: number }
  export function getRetention(appId: string, params?: RetentionParams): Promise<RetentionGrid>
  export function getLifecycle(appId: string, params?: LifecycleParams): Promise<LifecycleOut>
  export function getChurn(appId: string, params?: ChurnParams): Promise<ChurnPage>
  ```

- [ ] **Step 1: Write the failing test**

```ts
import { describe, it, expect } from 'vitest';
import { retentionRate } from './retention';

describe('retentionRate', () => {
  it('returns null for an unelapsed period, never 0', () => {
    // The bug this guards: `null` coerced through `?? 0` renders a cohort
    // that simply has not aged yet as 0% churn-to-zero.
    expect(retentionRate(null, 100)).toBeNull();
  });

  it('returns null rather than dividing by an empty cohort', () => {
    expect(retentionRate(0, 0)).toBeNull();
  });

  it('computes a rate for an elapsed period', () => {
    expect(retentionRate(25, 100)).toBe(0.25);
  });
});
```

- [ ] **Step 2: Run and confirm it fails**

```bash
cd dashboard && npx vitest run src/lib/api/retention.test.ts
```

Expected: FAIL — module not found.

- [ ] **Step 3: Write the client**

```ts
import { api } from './client';

export interface Cohort {
  start: string;
  size: number;
  /** `null` means the period has not elapsed yet — NOT zero retention. */
  periods: (number | null)[];
}

export interface RetentionGrid {
  granularity: 'day' | 'week';
  as_of: string | null;
  /** False when this app's pre-epoch history has not been backfilled. */
  ready: boolean;
  cohorts: Cohort[];
}

export interface RetentionParams {
  granularity?: 'day' | 'week';
  cohorts?: number;
  periods?: number;
  split?: 'none' | 'errors';
}

/**
 * Retention as a 0..1 rate, or `null` when there is no answer.
 *
 * Two distinct reasons for `null`, deliberately collapsed because the UI
 * treats them the same way — render an empty cell:
 * the period has not elapsed (`users === null`), or the cohort is empty.
 */
export function retentionRate(users: number | null, size: number): number | null {
  if (users === null) return null;
  if (size <= 0) return null;
  return users / size;
}

export async function getRetention(
  appId: string,
  params: RetentionParams = {},
): Promise<RetentionGrid> {
  const { data } = await api.get<RetentionGrid>(`/v1/apps/${appId}/retention`, { params });
  return data;
}
```

Add `getLifecycle` and `getChurn` on the same shape. Where a date range is passed, build the params with `toParams` from `lib/models/date-range` for **query strings**, and `toBody` for **JSON bodies** — mixing them produces a 422, which is what broke the funnel endpoint in v1.7.3.

- [ ] **Step 4: Run the test**

```bash
cd dashboard && npx vitest run src/lib/api/retention.test.ts
```

Expected: PASS.

---

### Task 10: RetentionGrid component

**Files:**
- Create: `dashboard/src/lib/components/RetentionGrid.svelte`
- Test: `dashboard/src/lib/components/RetentionGrid.test.ts`

**Interfaces:**
- Consumes: `Cohort`, `retentionRate` (Task 9).
- Produces: `<RetentionGrid cohorts={Cohort[]} granularity={'day'|'week'} />`

- [ ] **Step 1: Write the failing test**

```ts
import { describe, it, expect } from 'vitest';
import { render } from '@testing-library/svelte';
import RetentionGrid from './RetentionGrid.svelte';

describe('RetentionGrid', () => {
  it('renders an unelapsed period as an empty cell, not 0%', () => {
    const { container } = render(RetentionGrid, {
      props: {
        granularity: 'day' as const,
        cohorts: [{ start: '2026-08-28', size: 10, periods: [10, 5, null] }],
      },
    });
    const cells = container.querySelectorAll('td[data-period]');
    expect(cells[1].textContent).toContain('50');
    expect(cells[2].getAttribute('data-empty')).toBe('true');
    expect(cells[2].textContent?.trim()).not.toContain('0');
  });

  it('renders period 0 as the cohort size, not 100%', () => {
    const { container } = render(RetentionGrid, {
      props: {
        granularity: 'day' as const,
        cohorts: [{ start: '2026-08-28', size: 10, periods: [10, 5] }],
      },
    });
    const first = container.querySelector('td[data-period="0"]');
    expect(first?.textContent).toContain('10');
    expect(first?.textContent).not.toContain('100%');
  });
});
```

- [ ] **Step 2: Run and confirm it fails**

```bash
cd dashboard && npx vitest run src/lib/components/RetentionGrid.test.ts
```

Expected: FAIL — component not found.

- [ ] **Step 3: Build the component**

Requirements the tests pin, plus the ones they cannot:

- Every cell carries `data-period={n}`; unelapsed cells carry `data-empty="true"` and render nothing.
- Period 0 renders the cohort **size**; it is 100% by construction and printing that is noise.
- The colour ramp is driven by `retentionRate`, and the empty state is a **distinct** treatment — a hatched or bare cell, not the lightest ramp step, which would read as "almost nobody returned".
- A legend states that empty cells are periods that have not elapsed.
- The matrix scrolls inside its own `overflow-x: auto` container; the page body must never scroll horizontally.
- Use the house `Card`/`Skeleton` components and existing CSS custom properties. Do **not** write Tailwind class names — this app does not load Tailwind, and such classes pass every static gate while rendering unstyled.
- Every string comes from `t(...)`, with `en` and `ar` written now.

- [ ] **Step 4: Run the tests**

```bash
cd dashboard && npx vitest run src/lib/components/RetentionGrid.test.ts
```

Expected: PASS.

---

### Task 11: Retention page and its four registrations

**Files:**
- Create: `dashboard/src/pages/Retention.svelte`
- Modify: `dashboard/src/routes.ts`, `dashboard/src/lib/models/page-access.ts`, `dashboard/src/lib/models/shell.ts`, `dashboard/src/lib/components/layout/Sidebar.svelte`, `dashboard/src/lib/i18n/catalog/analyze.ts`
- Test: the existing `page-access.test.ts`, `shell.test.ts`, `admin-nav.test.ts`, `leak.test.ts` all run against the new route

**Interfaces:**
- Consumes: `getRetention`, `getLifecycle`, `getChurn` (Task 9); `RetentionGrid` (Task 10).
- Produces: the `#/retention` route.

- [ ] **Step 1: Run the parity tests first, to watch them fail**

Add the route to `routes.ts` only:

```ts
  '/retention': guarded(() => import('./pages/Retention.svelte')),
```

then:

```bash
cd dashboard && npx vitest run src/lib/models/page-access.test.ts src/lib/models/shell.test.ts
```

Expected: FAIL in both — a route with no `PAGE_ACCESS` key and no `SHELL_FLAGS` key. This is the guard rail working; it is why the route goes in first.

- [ ] **Step 2: Add the three remaining registrations**

`page-access.ts`, in the Analyze block beside `/funnels`:

```ts
  // `routes/retention.rs` authorizes through `scope::authorized_read_scope`
  // on `event:read`, the ENV-AWARE read path — hence `envAware`.
  '/retention': { perm: 'event:read', level: 'app', title: 'Retention', envAware: true },
```

`shell.ts`, in the matching block:

```ts
  '/retention': APP,
```

`Sidebar.svelte`, in the `nav.group.analyze` items array after `#/funnels`:

```ts
        { href: '#/retention', label: t('nav.retention'), icon: 'repeat', match: (p) => p.startsWith('/retention') },
```

Confirm `repeat` exists in the Icon registry before using it; if not, pick a registered name — an unregistered icon renders blank with no error.

`catalog/analyze.ts` — every string, both languages, written now:

```ts
  'nav.retention': { en: 'Retention', ar: 'الاحتفاظ' },
  'retention.title': { en: 'Retention', ar: 'الاحتفاظ' },
  'retention.subtitle': {
    en: 'Whether the people who arrived came back.',
    ar: 'ما إذا كان الأشخاص الذين وصلوا قد عادوا.',
  },
  'retention.cohort': { en: 'Cohort', ar: 'المجموعة' },
  'retention.users': { en: 'Users', ar: 'المستخدمون' },
  'retention.legend.empty': {
    en: 'Empty cells are periods that have not elapsed yet.',
    ar: 'الخلايا الفارغة هي فترات لم تنقضِ بعد.',
  },
  'retention.notReady.title': { en: 'Historical retention needs a one-time backfill', ar: 'يحتاج الاحتفاظ التاريخي إلى تعبئة أولية لمرة واحدة' },
  'retention.notReady.body': {
    en: 'Run {command} on the server to cover data from before this feature was installed.',
    ar: 'شغّل {command} على الخادم لتغطية البيانات السابقة لتثبيت هذه الميزة.',
  },
  'retention.lifecycle.new': { en: 'New', ar: 'جديد' },
  'retention.lifecycle.returning': { en: 'Returning', ar: 'عائد' },
  'retention.lifecycle.resurrected': { en: 'Resurrected', ar: 'مستعاد' },
  'retention.lifecycle.dormant': { en: 'Dormant', ar: 'خامل' },
  'retention.churn.title': { en: 'At risk', ar: 'معرّضون للفقد' },
  'retention.errorSplit.toggle': { en: 'Compare users who hit an error', ar: 'قارن المستخدمين الذين واجهوا خطأ' },
  'retention.errorSplit.caveat': {
    en: 'An association, not a cause. Exposure is measured in the first period only.',
    ar: 'ارتباط وليس سببًا. يُقاس التعرض في الفترة الأولى فقط.',
  },
```

- [ ] **Step 3: Build the page**

Model it on `JourneyExplorer.svelte`: `CachedView` per card, `DateRange`, `RollupChip`, house `Card`/`Skeleton`/`EmptyState`. Four cards fetching independently (the `ScreenDetail` pattern) so a slow churn query cannot block the grid.

Two specific hazards:

- **`ready === false` is a first-class state**, rendered before the grid: show `retention.notReady.*` naming the exact `sauron-migrate backfill-person-days` command. Do not render an empty grid.
- **`CachedView`'s `viewKey` must not be clock-derived.** A key built from `Date.now()` or "today" moves on every evaluation and the cache hits zero times while every test stays green. Build it from the explicit range and granularity.

- [ ] **Step 4: Run the whole dashboard suite**

```bash
cd dashboard && npx vitest run
```

Expected: PASS — including `page-access.test.ts`, `shell.test.ts` and the i18n `leak.test.ts`.

- [ ] **Step 5: Typecheck and build**

```bash
cd dashboard && npm run check && npm run build
```

Expected: no errors.

---

### Task 12: Lifecycle chart, error split and churn list on the page

**Files:**
- Create: `dashboard/src/lib/components/LifecycleChart.svelte`
- Modify: `dashboard/src/pages/Retention.svelte`
- Test: `dashboard/src/lib/components/LifecycleChart.test.ts`

**Interfaces:**
- Consumes: `LifecyclePoint` (Task 9), `getChurn` (Task 9).
- Produces: `<LifecycleChart points={LifecyclePoint[]} />`

- [ ] **Step 1: Write the failing test**

```ts
import { describe, it, expect } from 'vitest';
import { render } from '@testing-library/svelte';
import LifecycleChart from './LifecycleChart.svelte';

const points = [
  { start: '2026-08-27', new_users: 5, returning_users: 3, resurrected_users: 1, dormant_users: 2 },
];

describe('LifecycleChart', () => {
  it('draws dormant below the axis and the other three above', () => {
    const { container } = render(LifecycleChart, { props: { points } });
    expect(container.querySelector('[data-series="dormant"]')?.getAttribute('data-sign')).toBe('negative');
    for (const s of ['new', 'returning', 'resurrected']) {
      expect(container.querySelector(`[data-series="${s}"]`)?.getAttribute('data-sign')).toBe('positive');
    }
  });

  it('renders without measuring layout, so it works in a hidden pane', () => {
    // Charts that size themselves from getBoundingClientRect inside a
    // requestAnimationFrame hang forever when the pane is display:none —
    // rAF never fires. Assert the bars exist from props alone.
    const { container } = render(LifecycleChart, { props: { points } });
    expect(container.querySelectorAll('[data-series]').length).toBe(4);
  });
});
```

- [ ] **Step 2: Run and confirm it fails**

```bash
cd dashboard && npx vitest run src/lib/components/LifecycleChart.test.ts
```

Expected: FAIL — component not found.

- [ ] **Step 3: Build the chart and wire the remaining two cards**

`LifecycleChart.svelte`: a stacked bar per period with `new`/`returning`/`resurrected` above the axis and `dormant` below, each `<rect>` carrying `data-series` and `data-sign`. Size from the viewBox and props, never from `getBoundingClientRect` inside `requestAnimationFrame` — that hangs in a hidden pane.

On the page: an error-split toggle re-fetching with `split=errors` and drawing the two curves beneath the grid with the `retention.errorSplit.caveat` line always visible. The churn card uses `DataTable` + `CursorPagination` + `SortableTh`, rows linking to `#/persons/{distinct_id}`. Handle `auxclick` as well as `click` on the row — `stopPropagation` on `click` alone lets a middle-click open two tabs.

- [ ] **Step 4: Run the dashboard suite**

```bash
cd dashboard && npx vitest run && npm run check
```

Expected: PASS.

---

### Task 13: Pruning, and the end-to-end drive

**Files:**
- Modify: `backend/crates/sauron-db/src/rollups/person_days.rs` (add `prune`)
- Modify: wherever the rollup maintenance pass runs (`backend/crates/sauron-pipeline/src/` — find the caller of the existing state-table pruning)
- Test: `backend/crates/sauron-db/tests/person_days_rollup.rs`

**Interfaces:**
- Consumes: `person_days` (Task 1).
- Produces: `pub async fn prune(conn: &mut AsyncPgConnection, keep_days: i32) -> diesel::QueryResult<usize>`

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn prune_drops_rows_past_the_horizon_and_keeps_the_rest() {
    let Some(db) = TestDb::setup().await else { return };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs().await;

    for off in [-500i64, -10] {
        diesel::sql_query(
            "INSERT INTO person_days (app_id, environment_id, distinct_id, day, events) \
             VALUES ($1, NULL, 'p', current_date + $2, 1)",
        )
        .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
        .bind::<diesel::sql_types::Integer, _>(off as i32)
        .execute(&mut conn).await.unwrap();
    }

    sauron_db::rollups::person_days::prune(&mut conn, 400).await.unwrap();

    let left: CountRow = diesel::sql_query(
        "SELECT count(*) AS n FROM person_days WHERE app_id = $1")
        .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
        .get_result(&mut conn).await.unwrap();
    assert_eq!(left.n, 1, "the 500-day-old row goes, the 10-day-old row stays");

    db.cleanup().await;
}
```

- [ ] **Step 2: Run and confirm it fails**

```bash
cd backend && cargo test -p sauron-db --test person_days_rollup prune_drops
```

Expected: FAIL — `prune` not found.

- [ ] **Step 3: Implement and schedule pruning**

```rust
/// Drop person-days past the horizon. `keep_days` is clamped to the same
/// 1..=400 band as MAX_TIMESERIES_DAYS -- the longest window any endpoint can
/// answer over -- so pruning can never delete a day a query could still ask
/// about.
pub async fn prune(conn: &mut AsyncPgConnection, keep_days: i32) -> diesel::QueryResult<usize> {
    diesel::sql_query(
        "DELETE FROM person_days WHERE day < current_date - make_interval(days => $1::int)",
    )
    .bind::<diesel::sql_types::Integer, _>(keep_days.clamp(1, 400))
    .execute(conn)
    .await
}
```

Call it from the same maintenance pass that already prunes `rollup_session_state` and `rollup_journey_state` by age, reading the horizon from a `PERSON_DAYS_KEEP` env var defaulting to 400.

- [ ] **Step 4: Run the full backend suite**

```bash
cd backend && cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
```

Expected: PASS throughout. **A `cargo fmt` failure skips clippy and test in CI**, so a green pipeline whose fmt step failed has verified nothing — check that every step actually ran, and treat any `skipped` step as unverified.

- [ ] **Step 5: Drive it end to end against a real stack**

Unit tests do not exercise the readiness gate, the backfill, or the page. Bring up the stack, seed events across several days, and confirm by hand:

1. Before the backfill: `GET /v1/apps/{id}/retention` returns `ready: false`, and the page shows the backfill command rather than an empty grid.
2. Run `sauron-migrate backfill-person-days`; the same call now returns `ready: true` with populated cohorts.
3. The newest cohort's later periods are `null` in the JSON and blank in the grid.
4. `?environment_id=<enrollment id>` narrows the numbers. Note this takes the **enrollment** id — the catalogue id returns 403.
5. Trigger an `identify()` for a guest with prior activity and confirm the grid does not jump.

Report what you observed. Do not claim completion from the test suite alone.

---

## Self-Review

**Spec coverage.** Every section of the design maps to a task: data model → 1; fold → 2; hazard 1 (readiness) → 1 and 3; hazard 2 (identity merge) → 4; hazard 3 (erasure) → 5; hazard 4 (backfill) → 6; semantics → 7 (queries) and 8 (null-vs-zero, incompleteness); API → 8; dashboard → 9–12; testing → the test step of every task; rollout and pruning → 13.

**Two gaps found and closed while reviewing:** the spec's `PERSON_DAYS_KEEP` pruning had no task until Task 13 gained it, and the error-impact split had no dashboard task until Task 12 gained the toggle and the caveat line.

**Type consistency.** `PersonKey`/`PersonDayDelta` (Task 2) are the names Tasks 4 and 6 rely on. `is_ready`/`mark_all_backfilled`/`epoch` (Task 3) are what Tasks 6 and 8 call. `CohortRow`/`LifecyclePoint`/`Granularity` (Task 7) are what Task 8 consumes, and their JSON forms are what Task 9 types. `retentionRate` (Task 9) is what Task 10 renders. No name is used before the task that defines it.

**One deliberate deviation from the writing-plans template:** there are no commit steps, per the standing never-commit rule in Global Constraints.

---

## Execution log — 2026-08-28

All 13 tasks implemented and verified. Backend: 2,028 tests pass, `cargo fmt --check`
and `cargo clippy --all-targets -D warnings` clean. Dashboard: 1,220 tests pass,
`svelte-check` 0 errors (12 pre-existing warnings in files this work did not touch),
build clean. Everything left **uncommitted** in the working tree.

### Corrections to this plan, found while executing it

- **Task 1 Step 4 was wrong twice.** It said to regenerate `schema.rs`; `diesel.toml`
  documents three destructive effects of doing that, and no rollup table is in
  `schema.rs` at all. It also omitted `touch crates/sauron-db/src/lib.rs` — without
  it `embed_migrations!` never sees the new directory (no `build.rs`, so no rerun
  trigger) and the test fails with "table missing" after the migration has applied.
  Both corrected in the task above.
- **Tasks 10 and 12 specified component-render tests.** This project has no
  `@testing-library/svelte` and no jsdom — it tests pure logic in `.test.ts` and
  verifies components in a real browser. Implemented as `models/retention.ts` +
  17 logic tests, then a browser drive.
- **Purge needed TWO hooks, not one.** The plan had only the full-erase path;
  `recompute_person` (the time-ranged purge) also had to re-derive person-days from
  surviving rows, or a purged day keeps claiming the person was active.

### Bugs the test suite did not catch, and what now catches them

Five defects survived a fully green suite and were found only by driving the running
system. Each has a regression test now.

1. **`EnvFilter::One` and `Subset` need different bind TYPES** — `= $n` takes a
   scalar, `= ANY($n)` an array. Binding an array for `One` is a 500
   (`operator does not exist: uuid = uuid[]`) on the environment-scoped path only.
   Every unscoped test passed while all three endpoints were broken for the
   environment picker — which is how the dashboard always calls them.
   → `every_query_works_environment_scoped` exercises all four variants, with and
   without a keyset cursor.
2. **`sum(bigint)` returns NUMERIC**, which Diesel's `BigInt` decoder rejects — but a
   numeric zero fits in eight bytes and decodes silently, so the original fixture
   (counters left at their 0 default) passed while the endpoint 500'd.
   → `churn_lists_only_the_silent` now seeds `events_count = 4321` and asserts it;
   confirmed to fail without the `::bigint` cast.
3. **`EmptyState` takes `description`, not `body`**, and `DataTable`'s second snippet
   is `children`, not `body`. → `svelte-check`.
4. **A telemetry page must touch `sessionStore.scopeKey`** or the environment picker
   is silently ignored. → `api/scope.test.ts` already enforced this and failed.
5. **Icon `repeat` was not in the registry** — unregistered names render blank with
   no error. → registered in `Icon.svelte`.

### Runtime verification performed

API on :8090 against the dev Postgres, dashboard dev server on :3001.

- Unbackfilled app → `ready: false`, empty cohorts, and the page renders the
  `sauron-migrate backfill-person-days` command in a copyable block rather than a
  grid.
- Cell budget: 30x30 → 400 with a message naming the product; 12x12 → 200; no auth
  → 401.
- Seeded cohort of 10 → grid `[10, 6, 3, null …]`. Period 0 renders the SIZE; days
  3+ render blank with `data-empty="true"` and no ramp step; a genuine 0% still
  renders as "0%".
- Lifecycle: 10 new → 6 returning / 4 dormant → 3 returning / 3 dormant → 1
  resurrected. `dormant` carries `data-sign="negative"`.
- Error split: 3 exposed (100%/100%) + 7 clean (43%/0%) = the full cohort of 10.
- Churn lists the lapsed person with a correctly decoded `events_count`.
- Page body does not scroll horizontally; the grid scrolls inside its own container.

### Known limitation, not fixed

`lifecycle` emits a period only when someone was ACTIVE in it, so a period in which
everybody went dormant and nobody was active produces no bar at all — its dormant
count is dropped by the `LEFT JOIN` from `stepped`. Visible in the drive as a gap
between 2026-08-24 and 2026-08-27. Left as-is because the fix (a generated period
series) changes the query shape; worth doing if anyone reports a missing bar.

---

## Review round — 2026-08-28 (biz-team usefulness: behavior, performance, UI/UX)

A second pass over the shipped feature, judged as a tool for a product team and
measured against the 63M-event / 51k-person seeded app rather than the 10-person
drive fixture. Changes below are in the working tree, uncommitted.

### Found and fixed

1. **The missing lifecycle bar was the most important one.** The old query
   derived output rows from the ACTIVE set, so a period in which everybody went
   dormant had no row — the total-churn cliff rendered as a gap. Rewritten off a
   `generate_series` of buckets; the all-dormant period now shows its dormant
   count (verified over HTTP: the previously invisible `dorm=3` day). The primer
   bucket is dropped from output since it cannot classify itself.
2. **Lifecycle was 4.7 s at the default window on 51k persons** — the planner
   turned the self-joins into merge joins with four 16 MB external sorts
   (misestimating the bucket CTE 3×). Rewritten with `LAG`/`LEAD` over one
   sort: **1.8 s** (2.5s at 28 days, linear). Grid: 933 ms. Churn: 39 ms.
3. **`http_retention.rs` had silently never been written.** The first execution
   pass verified the handler contracts by manual curl and moved on, so the
   cell-budget, null-vs-zero, not-ready-envelope, env-bind and numeric-decode
   behaviours had no regression net. Now a real spawned-server suite (4 tests,
   5.2 s) covering exactly the bugs only the runtime drive had caught.
4. **RTL was broken by construction**: `text-align: right` + sticky `left: 0`
   pin the cohort column to the wrong edge under `dir="rtl"`, and Arabic is a
   first-class locale. Switched to logical properties; verified in the browser
   that `inset-inline-start: 0` resolves to `right: 0` under Arabic.
5. **No data freshness on the page.** `as_of` was in every response and shown
   nowhere — and the drive DB itself had a 3-day-stale watermark, which renders
   recent cells as unelapsed with no explanation. Added the shared `RollupChip`.
6. **Lifecycle bars had no dates** — hover-only. Added an axis row (~every nth
   label, first/last always).
7. **Churn pagination was wired server-side and unused client-side** — a
   "Load more" now consumes `next_before`.
8. **CSV export** (`gridToCsv`, tested): raw counts, unelapsed periods as EMPTY
   fields — a 0 there would poison downstream spreadsheet aggregates.
9. **Grid + lifecycle now ride `CachedView`** (keys from appId/scope/
   granularity/split — never the clock), so revisits paint instantly given the
   ~1 s server cost.
10. Churn column mislabeled "Cohort" → "Person"; not-ready copy no longer reads
    "Run  on the server".

### Measured (dev box, warm)

| Query | 51k persons | Note |
|---|---|---|
| Backfill (63M rows → 1.42M person-days) | 51 s | one-time, operator-run |
| Grid, weekly 12×12 | 933 ms | count(DISTINCT) temp spill |
| Lifecycle, daily 14 buckets | 1.81 s | was 4.72 s |
| Lifecycle, daily 28 buckets | 2.49 s | linear, was 6.40 s |
| Churn page | 39 ms | |

`person_days` at this density: ~283 B/row all-in (table + both indexes).

### Recommended before GA at ≥100k DAU (not done, deliberately)

- **Server-side SWR cache** for grid + lifecycle, on the `active-users-swr-cache`
  pattern — the spec named this as the escape hatch "if measurement disagrees",
  and at 51k persons measurement now disagrees. Client CachedView masks repeat
  loads only.
- Cohort/period count presets in the UI (the API already accepts them; the page
  pins 12×12).
- Cell drill-down ("who are these 3 users?") — the reason storage kept exact
  distinct_ids; churn partially covers it.
- Monthly granularity for slow-cadence B2B products.

### Concurrent-session note

An OpenAPI/Swagger effort is annotating routes in this same checkout: it added
`utoipa::ToSchema` derives to retention's wire types (kept intact here) and its
`router_parity` gate currently fails on ITS remaining 59 unannotated routes —
pre-existing to this review, not retention's (all three retention routes are
already documented there).

---

## SWR cache — 2026-08-28 (follow-up, user-requested)

Grid and lifecycle now serve stale-while-revalidate from Redis, on the
`active_users.rs` template (get/set_ex under the 500 ms op timeout, `SET NX`
single-flight refresh lock, `computed_at` disclosure). Deliberate deltas, all
documented in `routes/retention.rs`'s module docs: the key hashes the RESOLVED
env filter AND the UTC day (window is server-derived, so the key rotates at
midnight instead of serving yesterday's window); `ready:false` is never cached;
no admission semaphore (compute is ~2 s, not ~25 s — rate limit + TimeoutLayer
remain the bounds). `One(x)` and `Subset([x])` share a key on purpose: different
SQL, same rows. Churn stays uncached (39 ms, cursor-paged).

TDD: the cache-serving test asserts a post-cache MUTATION stays invisible
(distinguishes "cached" from "recomputed fast"), and the isolation test has an
env-scoped member and an app-wide owner send byte-identical requests and
receive different answers — the review-Critical property from active_users.

Measured on the 51k-person app through real HTTP (dev build):

| | Cold | Warm (cached) |
|---|---|---|
| grid weekly 12×12 | 976 ms | 2.6–4.5 ms |
| lifecycle daily 12 | 1,814 ms | 3.5 ms |
| grid + error split | 1,550 ms | 3.1 ms |

Wire change: `computed_at: Option<DateTime<Utc>>` on `GridOut`/`LifecycleOut`
(`#[serde(default)]`; null on not-ready). `LifecyclePoint` gained
`Deserialize` for the cache round-trip. Client types carry the optional field.

http_retention.rs is now 6 tests; the fixture gained a second environment and
an env-scoped member persona.
