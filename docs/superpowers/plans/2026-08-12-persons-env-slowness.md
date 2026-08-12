# Persons Env-Scoped Slowness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `GET /v1/apps/{app_id}/persons?…&environment_id=…` return in bounded time instead of timing out at 30s and returning 503.

**Architecture:** Two slices. Slice A makes the existing query cheap without changing its shape's semantics — the three missing `(app_id, distinct_id, environment_id, ts)` indexes, plus replacing `list_persons`' open-coded correlated-`EXISTS` membership block with the already-verified uncorrelated helper. Slice B adds an `event_user_environments` rollup maintained by the ingest write path, backfilled by an opt-in one-shot command that writes a per-app completion marker; `list_persons` reads the rollup for marked apps and falls back to the A-optimised live query otherwise.

**Tech Stack:** Rust, diesel + diesel-async (raw `sql_query` for these paths), Postgres 16 with RANGE-partitioned `analytics_events` / `error_events`, tokio.

## Global Constraints

- **No commits, no branches.** Leave all work uncommitted in the working tree. Do not run `git commit`, `git checkout -b`, or `git branch`.
- **MSRV 1.82.** diesel-async 0.9 transaction closures need async closures, which would push past it — use explicit `BEGIN`/`COMMIT` via `batch_execute`, as `write_rows_once` already does.
- **Backend tests silently skip without a reachable database.** Every DB-backed test starts `let Some(db) = TestDb::setup().await else { eprintln!("TEST_DATABASE_URL unset — skipping"); return; };` and returns **printing `ok`**. A green run proves nothing unless the DB was actually reachable. Run backend tests with `dangerouslyDisableSandbox: true`, host-network containers, and `max_connections=800`. Reference baseline: **1391** tests actually executed.
- **Migrations run inside a transaction**, so `CREATE INDEX CONCURRENTLY` is not available. Indexes on partitioned parents build synchronously across all 29 child partitions.
- **`require_current_schema` fail-closes the API** on a stale schema. Nothing slow may be added to a migration or to `sauron-migrate`'s default no-arg path.
- **Migration numbering:** the highest existing is `2026-08-11-000054_transaction_finished_at`. This plan adds `000055` and `000056`.
- **Deleting a doc comment's claim requires deleting the claim, not just the code.** Several comments in `list_persons` assert measured facts that these changes invalidate; each task says which.

---

## File Structure

**Slice A**
- Create: `backend/migrations/2026-08-12-000055_env_person_indexes/{up,down}.sql`
- Modify: `backend/crates/sauron-db/src/repo.rs` — `list_persons` membership block (~line 7423)

**Slice B — storage**
- Create: `backend/migrations/2026-08-12-000056_event_user_environments/{up,down}.sql`
- Modify: `backend/crates/sauron-db/src/schema.rs` — two new tables

**Slice B — write path**
- Modify: `backend/crates/sauron-db/src/batch.rs` — `PersonEnvBump`, `bump_person_envs`, `bump_sessions` return type, `WriteSet`, `write_rows_once`
- Modify: `backend/crates/sauron-db/src/repo.rs` — `bump_person_env` (single-row), `bump_session` return type
- Modify: `backend/crates/sauron-pipeline/src/batch.rs` — `Acc` fold
- Modify: `backend/crates/sauron-pipeline/src/process.rs` — `rollup()` single-item path

**Slice B — backfill**
- Create: `backend/crates/sauron-db/src/person_env_backfill.rs` — aggregation + marker
- Modify: `backend/crates/sauron-db/src/lib.rs` — `pub mod person_env_backfill;`
- Modify: `backend/bins/sauron-migrate/src/main.rs` — opt-in `backfill-person-envs` argument

**Slice B — read path**
- Modify: `backend/crates/sauron-db/src/repo.rs` — `list_persons` rollup branch

**Tests**
- Modify: `backend/crates/sauron-db/tests/env_scoping.rs`
- Modify: `backend/crates/sauron-db/tests/offset_sort.rs`
- Create: `backend/crates/sauron-db/tests/person_env_rollup.rs`

---

### Task 1: Slice A1 — the missing env-person indexes

**Files:**
- Create: `backend/migrations/2026-08-12-000055_env_person_indexes/up.sql`
- Create: `backend/migrations/2026-08-12-000055_env_person_indexes/down.sql`

**Interfaces:**
- Consumes: nothing.
- Produces: indexes `analytics_events_app_distinct_env_idx`, `error_events_app_distinct_env_idx`, `sessions_app_distinct_env_idx`.

- [ ] **Step 1: Write the failing test**

Add to `backend/crates/sauron-db/tests/env_scoping.rs`:

```rust
/// The `distinct_id` twin of migration 53's device indexes. `list_persons`'
/// three LATERALs and its three membership legs all probe
/// `(app_id, distinct_id)` filtered by `environment_id`, but before migration
/// 55 the only usable index was `analytics_distinct_idx (app_id, distinct_id,
/// occurred_at DESC)` — no `environment_id` — so every probe heap-fetched to
/// test the environment, once per person, across every partition.
#[tokio::test]
async fn env_person_indexes_exist() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;

    #[derive(diesel::QueryableByName)]
    struct Name {
        #[diesel(sql_type = diesel::sql_types::Text)]
        indexname: String,
    }

    let rows: Vec<Name> = diesel::sql_query(
        "SELECT indexname FROM pg_indexes \
         WHERE indexname IN ('analytics_events_app_distinct_env_idx', \
                             'error_events_app_distinct_env_idx', \
                             'sessions_app_distinct_env_idx')",
    )
    .get_results(&mut conn)
    .await
    .expect("pg_indexes query");

    let mut found: Vec<String> = rows.into_iter().map(|r| r.indexname).collect();
    found.sort();
    assert_eq!(
        found,
        vec![
            "analytics_events_app_distinct_env_idx".to_string(),
            "error_events_app_distinct_env_idx".to_string(),
            "sessions_app_distinct_env_idx".to_string(),
        ],
        "migration 55 must create all three env-person indexes"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cd backend && cargo test -p sauron-db --test env_scoping env_person_indexes_exist -- --nocapture
```

Expected: FAIL — `found` is empty. **If it prints `TEST_DATABASE_URL unset — skipping` the test did not run and this step is not satisfied.** Fix the environment first.

- [ ] **Step 3: Write the migration**

`backend/migrations/2026-08-12-000055_env_person_indexes/up.sql`:

```sql
-- The distinct_id twin of migration 53's (app_id, device_key, environment_id, ts)
-- indexes. list_persons derives per-environment counts and first/last_seen from
-- three LEFT JOIN LATERALs keyed on (app_id, distinct_id, environment_id), and
-- derives environment membership from three EXISTS legs over the same tables.
--
-- Before this migration the only usable index was
-- analytics_distinct_idx (app_id, distinct_id, occurred_at DESC) — no
-- environment_id — so each probe matched on the first two columns and then
-- heap-fetched every row to test environment_id, once per person, across every
-- partition. list_persons has NO time window at all (no `since` parameter, and
-- ILIKE '%' on an unsearched page), so that cost scales with total retained
-- data rather than with a query window.
--
-- The trailing timestamp column is the aggregate payload, not a filter: ae/ee
-- take occurred_at (count, min, max), se takes started_at and last_event_at.
-- Dropping it still serves the lookup but puts the heap fetch straight back.
--
-- Builds SYNCHRONOUSLY across every live child partition inside this
-- transaction. analytics_events and error_events are hot-write tables: this
-- needs a maintenance window. CONCURRENTLY is not an option — migrations run in
-- a transaction and these are partitioned parents (same constraint as
-- migrations 47 and 53).
CREATE INDEX analytics_events_app_distinct_env_idx
    ON analytics_events (app_id, distinct_id, environment_id, occurred_at);

CREATE INDEX error_events_app_distinct_env_idx
    ON error_events (app_id, distinct_id, environment_id, occurred_at);

CREATE INDEX sessions_app_distinct_env_idx
    ON sessions (app_id, distinct_id, environment_id, started_at, last_event_at);
```

`backend/migrations/2026-08-12-000055_env_person_indexes/down.sql`:

```sql
DROP INDEX IF EXISTS sessions_app_distinct_env_idx;
DROP INDEX IF EXISTS error_events_app_distinct_env_idx;
DROP INDEX IF EXISTS analytics_events_app_distinct_env_idx;
```

- [ ] **Step 4: Run test to verify it passes**

```bash
cd backend && cargo test -p sauron-db --test env_scoping env_person_indexes_exist -- --nocapture
```

Expected: PASS, having actually connected.

- [ ] **Step 5: Verify the migration applies to a fresh database**

```bash
cd backend && DATABASE_URL="$TEST_DATABASE_URL" cargo run -p sauron-migrate
```

Expected: exits 0, log line `migrations up to date`.

---

### Task 2: Slice A2 — replace `list_persons`' open-coded membership block

**Files:**
- Modify: `backend/crates/sauron-db/src/repo.rs` — `list_persons`, the `membership_sql` block at ~7423
- Test: `backend/crates/sauron-db/tests/env_scoping.rs`

**Interfaces:**
- Consumes: `event_user_membership_exists(env: EnvFilter, bind_index: usize) -> String` (`repo.rs:7661`), already present and already verified.
- Produces: no new API. `list_persons`' signature is unchanged.

**Why this is safe:** the helper emits `AND event_users.distinct_id IN (SELECT … UNION …)`. `list_persons`' inner subquery is `FROM event_users WHERE app_id=$1 AND …`, so the qualified name `event_users.distinct_id` resolves there. The helper takes the env bind index as a parameter and `list_persons` already uses `$5` for env, so pass `5` — **no bind renumbering**.

- [ ] **Step 1: Write the failing test**

Add to `backend/crates/sauron-db/tests/env_scoping.rs`. This asserts the *shape*, because the behaviour is already covered by `list_persons_covers_only_the_selected_environment` and would pass either way:

```rust
/// `list_persons` used to open-code the same three correlated `EXISTS` that
/// `event_user_membership_exists` was rewritten away from (measured 32.6s ->
/// 3.5s on `overview_totals`). A correlated `EXISTS` is probed per candidate
/// row across every partition; the uncorrelated `IN (… UNION …)` builds the
/// membership set once per leg. Asserting on the emitted SQL because both
/// shapes return identical rows — which is exactly why the duplicate survived.
#[tokio::test]
async fn list_persons_membership_is_uncorrelated() {
    let sql = sauron_db::repo::list_persons_sql_for_test(EnvFilter::One(Uuid::nil()));
    assert!(
        sql.contains("event_users.distinct_id IN ("),
        "membership must be the uncorrelated IN (… UNION …) form, got:\n{sql}"
    );
    assert!(
        !sql.contains("EXISTS (SELECT 1 FROM analytics_events ae"),
        "the open-coded correlated EXISTS block must be gone, got:\n{sql}"
    );
}
```

- [ ] **Step 2: Extract the query builder so the test can see the SQL**

In `repo.rs`, split the string construction out of `list_persons` so both the test and the function use one source. Add above `list_persons`:

```rust
/// The exact SQL `list_persons` executes, exposed so tests can assert on the
/// emitted shape. Two query shapes now exist (live and rollup — see
/// `list_persons`), and the only thing separating "correct but 30s" from
/// "correct and fast" is which one is emitted; a behavioural test cannot tell
/// them apart because they return identical rows.
pub fn list_persons_sql_for_test(env: EnvFilter) -> String {
    list_persons_live_sql(&env, &SortSpec::default_person())
}
```

If `SortSpec` has no `default_person()`, use the same literal the tests' `common::default_person_sort()` builds and inline it here rather than adding a constructor.

- [ ] **Step 3: Run test to verify it fails**

```bash
cd backend && cargo test -p sauron-db --test env_scoping list_persons_membership_is_uncorrelated -- --nocapture
```

Expected: FAIL on the first assertion — the emitted SQL still contains `EXISTS (SELECT 1 FROM analytics_events ae`.

- [ ] **Step 4: Replace the block**

In `list_persons`, delete the whole `let membership_sql = if matches!(scope.env, EnvFilter::All) { … } else { … };` block (the one building `ae_env`/`ee_env`/`se_env` and three `EXISTS`) and replace with:

```rust
    // Was three open-coded correlated `EXISTS` — a duplicate of
    // `event_user_membership_exists`, which had already been rewritten to the
    // uncorrelated `IN (… UNION …)` form (measured 32.6s -> 3.5s on
    // `overview_totals`) while this copy was left behind. Deleted rather than
    // ported: one membership definition, one place to change it.
    //
    // Bind index 5 is unchanged — `$1` app_id, `$2` pattern, `$3` limit,
    // `$4` offset, `$5` env — so no renumbering follows from this.
    let membership_sql = event_user_membership_exists(scope.env.clone(), 5);
```

- [ ] **Step 5: Update the doc comment that this invalidates**

`list_persons`' comment block describing the membership `EXISTS` ("Each leg aliases its subquery and qualifies the correlated column with that alias…") describes code that no longer exists. Replace that paragraph with a pointer to `event_user_membership_exists`. Leave the paragraph explaining *why* membership must be derived at all — that is still true.

- [ ] **Step 6: Run the full env-scoping and sort suites**

```bash
cd backend && cargo test -p sauron-db --test env_scoping --test offset_sort -- --nocapture
```

Expected: PASS, including the pre-existing `list_persons_covers_only_the_selected_environment` and `persons_page_stably_when_last_seen_ties`. Confirm the output does **not** contain `skipping`.

---

### Task 3: Slice B1 — `event_user_environments` schema

**Files:**
- Create: `backend/migrations/2026-08-12-000056_event_user_environments/{up,down}.sql`
- Modify: `backend/crates/sauron-db/src/schema.rs`
- Test: `backend/crates/sauron-db/tests/person_env_rollup.rs` (new file)

**Interfaces:**
- Produces: tables `event_user_environments` and `event_user_env_backfill`; diesel `schema.rs` entries for both.

- [ ] **Step 1: Write the failing test**

Create `backend/crates/sauron-db/tests/person_env_rollup.rs`:

```rust
mod common;

use common::TestDb;
use diesel_async::RunQueryDsl;
use uuid::Uuid;

/// `environment_id` is NULLABLE and `EnvFilter::Unattributed` is a real row, so
/// uniqueness cannot be a plain primary key — NULL never equals NULL, and a
/// plain unique index would let one person accumulate unlimited unattributed
/// rows. The unique index is over COALESCE(environment_id, nil-uuid), and the
/// upsert's ON CONFLICT must name that same expression or it silently degrades
/// into an unconstrained insert.
#[tokio::test]
async fn unattributed_rollup_rows_are_unique_per_person() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;
    let app_id = db.seed_app().await;

    let insert = |did: &str| {
        diesel::sql_query(
            "INSERT INTO event_user_environments \
               (app_id, distinct_id, environment_id, first_seen, last_seen) \
             VALUES ($1, $2, NULL, now(), now())",
        )
        .bind::<diesel::sql_types::Uuid, _>(app_id)
        .bind::<diesel::sql_types::Text, _>(did.to_string())
    };

    insert("p1").execute(&mut conn).await.expect("first insert");
    let second = insert("p1").execute(&mut conn).await;
    assert!(
        second.is_err(),
        "a second NULL-environment row for the same person must be rejected"
    );
}
```

If `TestDb` has no `seed_app()`, use whatever the existing helpers in `tests/common/mod.rs` expose to create one app and take its id — check `seed_two_envs()`'s return struct, which already carries `app_id`.

- [ ] **Step 2: Run test to verify it fails**

```bash
cd backend && cargo test -p sauron-db --test person_env_rollup -- --nocapture
```

Expected: FAIL — relation `event_user_environments` does not exist.

- [ ] **Step 3: Write the migration**

`backend/migrations/2026-08-12-000056_event_user_environments/up.sql`:

```sql
-- Per-(person, environment) rollup for the Users Explorer.
--
-- event_users carries no environment_id, so list_persons derived environment
-- membership, first_seen/last_seen and all three counts from three LATERALs
-- and three EXISTS legs over analytics_events/error_events/sessions, once per
-- admitted person, with no time bound. Under a scoped read the sort key is
-- GREATEST(...) over those three tables, so a blocking Sort had to consume
-- every person before LIMIT applied — the page size capped nothing and the
-- endpoint crossed sauron-api's 30s TimeoutLayer, which maps a request timeout
-- onto a 503.
--
-- environment_id is NULLABLE on purpose: EnvFilter::Unattributed is a real,
-- surfaced scope (rows ingested before environments existed), and it must be a
-- row here so that All equals the sum of the individual environments rather
-- than exceeding it.
CREATE TABLE event_user_environments (
    app_id          uuid        NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    distinct_id     text        NOT NULL,
    environment_id  uuid        NULL REFERENCES environments(id) ON DELETE CASCADE,
    first_seen      timestamptz NOT NULL,
    last_seen       timestamptz NOT NULL,
    events_count    bigint      NOT NULL DEFAULT 0,
    errors_count    bigint      NOT NULL DEFAULT 0,
    sessions_count  bigint      NOT NULL DEFAULT 0,
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now()
);

-- NULL never equals NULL, so a plain UNIQUE (app_id, distinct_id,
-- environment_id) would let one person accumulate unlimited unattributed rows
-- and every upsert against them would insert instead of update. The nil uuid is
-- never a real environments.id (it has no row, and the FK above would reject
-- it), so it is safe as the sentinel.
CREATE UNIQUE INDEX event_user_env_key_idx
    ON event_user_environments
       (app_id, distinct_id, COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid));

-- One per sortable column of PersonRow. The whole point of the rollup is that
-- ORDER BY … LIMIT applies to a single indexed table, so paging is bounded by
-- page size again instead of by the app's person count.
CREATE INDEX event_user_env_last_seen_idx  ON event_user_environments (app_id, environment_id, last_seen DESC);
CREATE INDEX event_user_env_first_seen_idx ON event_user_environments (app_id, environment_id, first_seen);
CREATE INDEX event_user_env_events_idx     ON event_user_environments (app_id, environment_id, events_count DESC);
CREATE INDEX event_user_env_errors_idx     ON event_user_environments (app_id, environment_id, errors_count DESC);
CREATE INDEX event_user_env_sessions_idx   ON event_user_environments (app_id, environment_id, sessions_count DESC);

-- Which apps' rollups are complete. Reads fall back to the live query for any
-- app without a row here, so a half-populated rollup is never read. A dedicated
-- table rather than runtime_settings because the marker is per-app and wants
-- the foreign key.
CREATE TABLE event_user_env_backfill (
    app_id       uuid        PRIMARY KEY REFERENCES apps(id) ON DELETE CASCADE,
    completed_at timestamptz NOT NULL
);
```

`down.sql`:

```sql
DROP TABLE IF EXISTS event_user_env_backfill;
DROP TABLE IF EXISTS event_user_environments;
```

- [ ] **Step 4: Add both tables to `schema.rs`**

Append to `backend/crates/sauron-db/src/schema.rs`, following the file's existing `diesel::table!` style, and add both names to the `allow_tables_to_appear_in_same_query!` list at the bottom:

```rust
diesel::table! {
    event_user_environments (app_id, distinct_id) {
        app_id -> Uuid,
        distinct_id -> Text,
        environment_id -> Nullable<Uuid>,
        first_seen -> Timestamptz,
        last_seen -> Timestamptz,
        events_count -> BigInt,
        errors_count -> BigInt,
        sessions_count -> BigInt,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    event_user_env_backfill (app_id) {
        app_id -> Uuid,
        completed_at -> Timestamptz,
    }
}
```

The declared primary key is a diesel fiction here (the real uniqueness is the `COALESCE` expression index); every query in this plan is raw `sql_query`, so nothing depends on it. Say so in a comment above the table so the next reader does not "fix" it.

- [ ] **Step 5: Run test to verify it passes**

```bash
cd backend && cargo test -p sauron-db --test person_env_rollup -- --nocapture
```

Expected: PASS.

- [ ] **Step 6: Verify migration on a fresh database**

```bash
cd backend && DATABASE_URL="$TEST_DATABASE_URL" cargo run -p sauron-migrate
```

Expected: exits 0. Confirm 56 migrations are applied.

---

### Task 4: `bump_sessions` reports which sessions were newly inserted

**Files:**
- Modify: `backend/crates/sauron-db/src/batch.rs` — `bump_sessions` (~225)
- Modify: `backend/crates/sauron-db/src/repo.rs` — `bump_session` (~5504)
- Test: `backend/crates/sauron-db/tests/person_env_rollup.rs`

**Interfaces:**
- Produces: `bump_sessions(conn, &[SessionBump]) -> QueryResult<Vec<(Uuid, String)>>` — the `(app_id, session_id)` of rows this call **inserted**, not updated. `bump_session(…) -> QueryResult<bool>` — `true` when it inserted.
- Consumed by: Task 5 (`write_rows_once`) and Task 6 (`process::rollup`).

**Why:** `sessions_count` on the rollup must count distinct sessions. A session is bumped again by every batch it appears in, so `+1` per fold entry counts one session many times, and the error grows with session length. Only the insert contributes.

- [ ] **Step 1: Write the failing test**

Add to `backend/crates/sauron-db/tests/person_env_rollup.rs`:

```rust
/// A session bumped across several batches must be reported as inserted
/// exactly once. `sessions_count` on the rollup is driven by this, and a naive
/// "+1 per bump" over-counts by however many batches the session spans — which
/// a single-batch test cannot see.
#[tokio::test]
async fn bump_sessions_reports_inserts_only_once() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs().await;

    let bump = |n: i64| sauron_db::batch::SessionBump {
        app_id: ids.app_id,
        session_id: "s-repeat".to_string(),
        distinct_id: Some(ids.shared_distinct_id.clone()),
        device_key: None,
        first_at: chrono::Utc::now(),
        last_at: chrono::Utc::now(),
        context: serde_json::json!({}),
        release: None,
        environment_id: Some(ids.env_a),
        ip: None,
        events_delta: n,
        errors_delta: 0,
    };

    let first = sauron_db::batch::bump_sessions(&mut conn, &[bump(1)])
        .await
        .expect("first bump");
    assert_eq!(
        first,
        vec![(ids.app_id, "s-repeat".to_string())],
        "the first bump inserts the session"
    );

    let second = sauron_db::batch::bump_sessions(&mut conn, &[bump(1)])
        .await
        .expect("second bump");
    assert!(
        second.is_empty(),
        "a repeat bump updates rather than inserts and must report nothing"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cd backend && cargo test -p sauron-db --test person_env_rollup bump_sessions_reports_inserts_only_once -- --nocapture
```

Expected: FAIL to compile — `bump_sessions` returns `usize`, not `Vec<(Uuid, String)>`.

- [ ] **Step 3: Change `bump_sessions`**

Add `RETURNING` to the statement and switch from `.execute()` to `.get_results()`. `xmax = 0` is true exactly for rows this statement inserted (an updated row carries the updating transaction's id in `xmax`):

```rust
#[derive(diesel::QueryableByName)]
struct InsertedSession {
    #[diesel(sql_type = SqlUuid)]
    app_id: Uuid,
    #[diesel(sql_type = Text)]
    session_id: String,
    #[diesel(sql_type = diesel::sql_types::Bool)]
    inserted: bool,
}
```

Append to the SQL string, after the `updated_at = now()` line:

```
 RETURNING app_id, session_id, (xmax = 0) AS inserted
```

and change the tail:

```rust
    .get_results::<InsertedSession>(conn)
    .await
    .map(|rows| {
        rows.into_iter()
            .filter(|r| r.inserted)
            .map(|r| (r.app_id, r.session_id))
            .collect()
    })
```

Change the signature to `-> QueryResult<Vec<(Uuid, String)>>` and the empty-input early return from `Ok(0)` to `Ok(Vec::new())`.

Document the `xmax` trick in a doc comment — it is not self-evident, and a reviewer who does not know it will read `(xmax = 0)` as noise.

- [ ] **Step 4: Change `repo::bump_session` the same way**

Append ` RETURNING (xmax = 0) AS inserted` to its statement, change the return type to `QueryResult<bool>`, and read the single row.

- [ ] **Step 5: Fix the callers the signature change breaks**

```bash
cd backend && cargo check --workspace --all-targets 2>&1 | grep -E "^error" -A 5
```

Known call sites: `crates/sauron-db/src/batch.rs:675` (`write_rows_once`, currently `?`-discards), `crates/sauron-pipeline/src/process.rs:112` (currently `let _ =`), `bins/sauron-api/tests/http_sessions_search.rs:274` (currently `.expect("seed sessions")`, result discarded). All three compile unchanged or need only the discard kept — do **not** "fix" them by asserting on the new value.

- [ ] **Step 6: Run test to verify it passes**

```bash
cd backend && cargo test -p sauron-db --test person_env_rollup bump_sessions_reports_inserts_only_once -- --nocapture
```

Expected: PASS.

---

### Task 5: `PersonEnvBump` + `bump_person_envs`

**Files:**
- Modify: `backend/crates/sauron-db/src/batch.rs`
- Test: `backend/crates/sauron-db/tests/person_env_rollup.rs`

**Interfaces:**
- Produces:

```rust
pub struct PersonEnvBump {
    pub app_id: Uuid,
    pub distinct_id: String,
    pub environment_id: Option<Uuid>,
    pub first_at: DateTime<Utc>,
    pub last_at: DateTime<Utc>,
    pub events_delta: i64,
    pub errors_delta: i64,
    pub sessions_delta: i64,
}

pub async fn bump_person_envs(
    conn: &mut AsyncPgConnection,
    rows: &[PersonEnvBump],
) -> QueryResult<usize>;
```

- Consumed by: Task 6 (`WriteSet`), Task 7 (`Acc`), Task 8 (single-item path).

- [ ] **Step 1: Write the failing test**

Add to `backend/crates/sauron-db/tests/person_env_rollup.rs`:

```rust
/// Folding N bumps into one statement must equal N sequential single-row
/// upserts. The trap is the timestamps: `first_seen` is driven by LEAST and
/// `last_seen` by GREATEST, so collapsing a batch to a single timestamp would
/// drag `first_seen` forward to the newest signal in the group — the same
/// reason `SessionBump` carries both ends.
#[tokio::test]
async fn person_env_fold_matches_sequential_upserts() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs().await;

    let old = chrono::Utc::now() - chrono::Duration::days(10);
    let new = chrono::Utc::now();

    let mk = |at: chrono::DateTime<chrono::Utc>, ev: i64| sauron_db::batch::PersonEnvBump {
        app_id: ids.app_id,
        distinct_id: "fold-person".to_string(),
        environment_id: Some(ids.env_a),
        first_at: at,
        last_at: at,
        events_delta: ev,
        errors_delta: 0,
        sessions_delta: 0,
    };

    // Newest first, so a fold that keeps only the last timestamp gets first_seen wrong.
    sauron_db::batch::bump_person_envs(&mut conn, &[mk(new, 1), mk(old, 2)])
        .await
        .expect("fold");

    #[derive(diesel::QueryableByName)]
    struct Row {
        #[diesel(sql_type = diesel::sql_types::Timestamptz)]
        first_seen: chrono::DateTime<chrono::Utc>,
        #[diesel(sql_type = diesel::sql_types::Timestamptz)]
        last_seen: chrono::DateTime<chrono::Utc>,
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        events_count: i64,
    }

    let row: Row = diesel::sql_query(
        "SELECT first_seen, last_seen, events_count FROM event_user_environments \
         WHERE app_id=$1 AND distinct_id='fold-person' AND environment_id=$2",
    )
    .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
    .bind::<diesel::sql_types::Uuid, _>(ids.env_a)
    .get_result(&mut conn)
    .await
    .expect("exactly one rollup row");

    assert_eq!(row.events_count, 3, "deltas add");
    assert!(
        (row.first_seen - old).num_seconds().abs() < 2,
        "first_seen is the OLDEST signal in the fold, not the last one applied"
    );
    assert!(
        (row.last_seen - new).num_seconds().abs() < 2,
        "last_seen is the NEWEST signal in the fold"
    );
}

/// The unattributed row (environment_id IS NULL) must upsert, not accumulate
/// duplicates — the ON CONFLICT has to name the COALESCE expression index or it
/// silently becomes an unconstrained insert and every read doubles.
#[tokio::test]
async fn person_env_upsert_handles_null_environment() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs().await;

    let mk = || sauron_db::batch::PersonEnvBump {
        app_id: ids.app_id,
        distinct_id: "null-env-person".to_string(),
        environment_id: None,
        first_at: chrono::Utc::now(),
        last_at: chrono::Utc::now(),
        events_delta: 1,
        errors_delta: 0,
        sessions_delta: 0,
    };

    sauron_db::batch::bump_person_envs(&mut conn, &[mk()]).await.expect("first");
    sauron_db::batch::bump_person_envs(&mut conn, &[mk()]).await.expect("second");

    #[derive(diesel::QueryableByName)]
    struct Count {
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        n: i64,
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        events_count: i64,
    }

    let c: Count = diesel::sql_query(
        "SELECT count(*) AS n, COALESCE(max(events_count),0) AS events_count \
         FROM event_user_environments \
         WHERE app_id=$1 AND distinct_id='null-env-person'",
    )
    .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
    .get_result(&mut conn)
    .await
    .expect("count");

    assert_eq!(c.n, 1, "two bumps must produce ONE unattributed row, not two");
    assert_eq!(c.events_count, 2, "and its counter must have accumulated both");
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cd backend && cargo test -p sauron-db --test person_env_rollup person_env_ -- --nocapture
```

Expected: FAIL to compile — `PersonEnvBump` and `bump_person_envs` do not exist.

- [ ] **Step 3: Implement**

Add to `backend/crates/sauron-db/src/batch.rs`, after `bump_devices`:

```rust
/// One (person, environment)'s folded contribution from a batch.
///
/// `event_users` carries no `environment_id`, so before this rollup existed the
/// Users Explorer derived membership, first/last-seen and all three counts from
/// three LATERALs and three EXISTS legs per admitted person, unbounded by time.
#[derive(Debug, Clone)]
pub struct PersonEnvBump {
    pub app_id: Uuid,
    pub distinct_id: String,
    /// `None` is `EnvFilter::Unattributed` — a real row, not an absence. See the
    /// migration's comment for why.
    pub environment_id: Option<Uuid>,
    /// See [`SessionBump::first_at`] — `first_seen`/`last_seen` are driven by
    /// `LEAST`/`GREATEST` and need the two ends of the fold, not one point.
    pub first_at: DateTime<Utc>,
    pub last_at: DateTime<Utc>,
    pub events_delta: i64,
    pub errors_delta: i64,
    /// **Insert-only.** A session is bumped by every batch it appears in, so
    /// `+1` per bump counts one session many times — the caller supplies this
    /// from `bump_sessions`' inserted-key list, never from a fold count.
    pub sessions_delta: i64,
}

/// Fold N person/environment bumps into `event_user_environments`, one statement.
pub async fn bump_person_envs(
    conn: &mut AsyncPgConnection,
    rows: &[PersonEnvBump],
) -> QueryResult<usize> {
    if rows.is_empty() {
        return Ok(0);
    }
    // Sorted by the conflict key so every concurrent batch takes these row locks
    // in the same order — see the module's ordering rule. This is the third
    // row-lock participant in `write_rows_once`; the ingest path has already
    // produced one deadlock (users_seen vs. the issue upsert) that stayed
    // invisible because the worker's stdout was being discarded.
    let nil = Uuid::nil();
    let mut ix: Vec<usize> = (0..rows.len()).collect();
    ix.sort_unstable_by(|&a, &b| {
        (
            rows[a].app_id,
            &rows[a].distinct_id,
            rows[a].environment_id.unwrap_or(nil),
        )
            .cmp(&(
                rows[b].app_id,
                &rows[b].distinct_id,
                rows[b].environment_id.unwrap_or(nil),
            ))
    });
    diesel::sql_query(
        "INSERT INTO event_user_environments \
           (app_id, distinct_id, environment_id, first_seen, last_seen, \
            events_count, errors_count, sessions_count) \
         SELECT app_id, distinct_id, environment_id, first_at, last_at, \
                events_delta, errors_delta, sessions_delta \
         FROM unnest($1::uuid[], $2::text[], $3::uuid[], $4::timestamptz[], \
                     $5::timestamptz[], $6::bigint[], $7::bigint[], $8::bigint[]) \
              AS t(app_id, distinct_id, environment_id, first_at, last_at, \
                   events_delta, errors_delta, sessions_delta) \
         ON CONFLICT (app_id, distinct_id, \
                      COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid)) \
         DO UPDATE SET \
            first_seen = LEAST(event_user_environments.first_seen, EXCLUDED.first_seen), \
            last_seen = GREATEST(event_user_environments.last_seen, EXCLUDED.last_seen), \
            events_count = event_user_environments.events_count + EXCLUDED.events_count, \
            errors_count = event_user_environments.errors_count + EXCLUDED.errors_count, \
            sessions_count = event_user_environments.sessions_count + EXCLUDED.sessions_count, \
            updated_at = now()",
    )
    .bind::<Array<SqlUuid>, _>(ix.iter().map(|&i| rows[i].app_id).collect::<Vec<_>>())
    .bind::<Array<Text>, _>(
        ix.iter()
            .map(|&i| rows[i].distinct_id.clone())
            .collect::<Vec<_>>(),
    )
    .bind::<Array<Nullable<SqlUuid>>, _>(
        ix.iter()
            .map(|&i| rows[i].environment_id)
            .collect::<Vec<_>>(),
    )
    .bind::<Array<Timestamptz>, _>(ix.iter().map(|&i| rows[i].first_at).collect::<Vec<_>>())
    .bind::<Array<Timestamptz>, _>(ix.iter().map(|&i| rows[i].last_at).collect::<Vec<_>>())
    .bind::<Array<BigInt>, _>(ix.iter().map(|&i| rows[i].events_delta).collect::<Vec<_>>())
    .bind::<Array<BigInt>, _>(ix.iter().map(|&i| rows[i].errors_delta).collect::<Vec<_>>())
    .bind::<Array<BigInt>, _>(ix.iter().map(|&i| rows[i].sessions_delta).collect::<Vec<_>>())
    .execute(conn)
    .await
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cd backend && cargo test -p sauron-db --test person_env_rollup person_env_ -- --nocapture
```

Expected: both PASS.

---

### Task 6: Wire the rollup into `write_rows_once`

**Files:**
- Modify: `backend/crates/sauron-db/src/batch.rs` — `WriteSet` (~601), `write_rows_once` (~661)
- Test: `backend/crates/sauron-db/tests/person_env_rollup.rs`

**Interfaces:**
- Consumes: `PersonEnvBump`, `bump_person_envs` (Task 5); `bump_sessions -> Vec<(Uuid, String)>` (Task 4).
- Produces: `WriteSet.person_envs: &'a [PersonEnvBump]` — a new required field on the struct.

- [ ] **Step 1: Write the failing test**

Add to `backend/crates/sauron-db/tests/person_env_rollup.rs`:

```rust
/// `sessions_count` is credited inside the transaction, from the sessions that
/// this write actually INSERTED. Writing the same batch twice must leave
/// sessions_count at 1 while events_count doubles — that asymmetry is the whole
/// point, and a test that writes once cannot see it.
#[tokio::test]
async fn write_rows_credits_a_session_once_across_batches() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs().await;
    let did = "wr-person".to_string();

    let session = sauron_db::batch::SessionBump {
        app_id: ids.app_id,
        session_id: "wr-session".to_string(),
        distinct_id: Some(did.clone()),
        device_key: None,
        first_at: chrono::Utc::now(),
        last_at: chrono::Utc::now(),
        context: serde_json::json!({}),
        release: None,
        environment_id: Some(ids.env_a),
        ip: None,
        events_delta: 1,
        errors_delta: 0,
    };
    let person = sauron_db::batch::PersonEnvBump {
        app_id: ids.app_id,
        distinct_id: did.clone(),
        environment_id: Some(ids.env_a),
        first_at: chrono::Utc::now(),
        last_at: chrono::Utc::now(),
        events_delta: 1,
        errors_delta: 0,
        sessions_delta: 0,
    };

    for _ in 0..2 {
        sauron_db::batch::write_rows(
            &mut conn,
            sauron_db::batch::WriteSet {
                errors: &[],
                analytics: &[],
                transactions: &[],
                sessions: std::slice::from_ref(&session),
                devices: &[],
                touch_users: &[],
                identified: &[],
                person_envs: std::slice::from_ref(&person),
            },
        )
        .await
        .expect("write_rows");
    }

    #[derive(diesel::QueryableByName)]
    struct Row {
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        events_count: i64,
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        sessions_count: i64,
    }

    let row: Row = diesel::sql_query(
        "SELECT events_count, sessions_count FROM event_user_environments \
         WHERE app_id=$1 AND distinct_id=$2 AND environment_id=$3",
    )
    .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
    .bind::<diesel::sql_types::Text, _>(did)
    .bind::<diesel::sql_types::Uuid, _>(ids.env_a)
    .get_result(&mut conn)
    .await
    .expect("rollup row");

    assert_eq!(row.events_count, 2, "two batches, two events");
    assert_eq!(
        row.sessions_count, 1,
        "one session across two batches is ONE session"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cd backend && cargo test -p sauron-db --test person_env_rollup write_rows_credits -- --nocapture
```

Expected: FAIL to compile — `WriteSet` has no `person_envs` field.

- [ ] **Step 3: Add the field and the write**

Add to `WriteSet`:

```rust
    /// Per-(person, environment) rollup deltas. `sessions_delta` arrives ZERO
    /// here and is credited inside the transaction from `bump_sessions`'
    /// inserted-key list — the caller cannot know which sessions are new.
    pub person_envs: &'a [PersonEnvBump],
```

In `write_rows_once`, replace the two roll-up lines with:

```rust
        // The roll-ups go LAST, `devices` and the person rollup last of all. A
        // row lock is held until COMMIT, so the later a contended row is taken
        // the shorter every other worker waits for it.
        let inserted = bump_sessions(conn, set.sessions).await?;
        bump_devices(conn, set.devices).await?;

        // `sessions_count` is credited here, not by the caller: only this
        // statement's `RETURNING (xmax = 0)` knows which sessions were newly
        // inserted. A session is bumped again by every batch it appears in, so
        // crediting per bump would over-count by however many batches it spans.
        let mut person_envs: Vec<PersonEnvBump> = set.person_envs.to_vec();
        if !inserted.is_empty() {
            let inserted: HashSet<(Uuid, String)> = inserted.into_iter().collect();
            // Index the pending rows so a session whose person is already in the
            // set credits that row instead of adding a second one for the same key.
            let mut at: HashMap<(Uuid, String, Uuid), usize> = HashMap::new();
            for (i, p) in person_envs.iter().enumerate() {
                at.insert(
                    (p.app_id, p.distinct_id.clone(), p.environment_id.unwrap_or(Uuid::nil())),
                    i,
                );
            }
            for s in set.sessions {
                let Some(did) = s.distinct_id.as_deref().filter(|d| !d.is_empty()) else {
                    continue;
                };
                if !inserted.contains(&(s.app_id, s.session_id.clone())) {
                    continue;
                }
                let key = (s.app_id, did.to_string(), s.environment_id.unwrap_or(Uuid::nil()));
                match at.get(&key) {
                    Some(&i) => person_envs[i].sessions_delta += 1,
                    None => {
                        at.insert(key, person_envs.len());
                        person_envs.push(PersonEnvBump {
                            app_id: s.app_id,
                            distinct_id: did.to_string(),
                            environment_id: s.environment_id,
                            first_at: s.first_at,
                            last_at: s.last_at,
                            events_delta: 0,
                            errors_delta: 0,
                            sessions_delta: 1,
                        });
                    }
                }
            }
        }
        bump_person_envs(conn, &person_envs).await?;
```

Add `use std::collections::{HashMap, HashSet};` to the module if absent.

- [ ] **Step 4: Fix the other `WriteSet` construction sites**

```bash
cd backend && cargo check --workspace --all-targets 2>&1 | grep -E "^error" -A 6
```

`WriteSet` is constructed in `crates/sauron-pipeline/src/batch.rs` (~line 555). Add `person_envs: &acc.person_envs,` there — Task 7 creates that field, so until then pass `&[]` and change it in Task 7.

- [ ] **Step 5: Run test to verify it passes**

```bash
cd backend && cargo test -p sauron-db --test person_env_rollup write_rows_credits -- --nocapture
```

Expected: PASS.

---

### Task 7: Fold `PersonEnvBump` in the pipeline's `Acc`

**Files:**
- Modify: `backend/crates/sauron-pipeline/src/batch.rs` — `Acc` (~115), the fold sites, the `WriteSet` construction (~555)

**Interfaces:**
- Consumes: `PersonEnvBump` (Task 5), `WriteSet.person_envs` (Task 6).
- Produces: `Acc.person_envs: Vec<PersonEnvBump>` and `Acc.person_env_at: HashMap<(Uuid, String, Uuid), usize>`.

**Semantics to preserve:** the rollup's membership must admit exactly the people the live query's three `EXISTS` legs admit — anyone with a row in `analytics_events`, `error_events` **or** `sessions` for that environment. So the fold fires for analytics events and error events too, not only for sessions.

- [ ] **Step 1: Add the fields to `Acc`**

```rust
    person_envs: Vec<PersonEnvBump>,
    /// `(app_id, distinct_id, environment_id-or-nil)` → index into `person_envs`,
    /// for the same `ON CONFLICT DO UPDATE` dedupe reason as `issue_at`.
    person_env_at: HashMap<(Uuid, String, Uuid), usize>,
```

- [ ] **Step 2: Add the fold method**

```rust
impl Acc {
    /// Fold one person/environment signal.
    ///
    /// `sessions_delta` is deliberately absent: only `write_rows_once` knows
    /// which sessions were newly inserted, and crediting here would over-count
    /// a session by however many batches it spans.
    fn person_env(
        &mut self,
        app_id: Uuid,
        distinct_id: &str,
        environment_id: Option<Uuid>,
        at: DateTime<Utc>,
        events_delta: i64,
        errors_delta: i64,
    ) {
        // An empty distinct_id has no `event_users` row, so a rollup entry for
        // it could never be joined to a person — it would be invisible weight.
        if distinct_id.is_empty() {
            return;
        }
        let key = (
            app_id,
            distinct_id.to_string(),
            environment_id.unwrap_or_else(Uuid::nil),
        );
        match self.person_env_at.get(&key) {
            Some(&i) => {
                let b = &mut self.person_envs[i];
                b.first_at = b.first_at.min(at);
                b.last_at = b.last_at.max(at);
                b.events_delta += events_delta;
                b.errors_delta += errors_delta;
            }
            None => {
                self.person_env_at.insert(key, self.person_envs.len());
                self.person_envs.push(PersonEnvBump {
                    app_id,
                    distinct_id: distinct_id.to_string(),
                    environment_id,
                    first_at: at,
                    last_at: at,
                    events_delta,
                    errors_delta,
                    sessions_delta: 0,
                });
            }
        }
    }
}
```

- [ ] **Step 3: Call it from both prepare paths**

Find where `acc.devices` is folded for analytics events and for errors (the same places that already have `environment_id`, `distinct_id` and `at` in scope — `prepare_error` is one, its analytics twin the other). Add alongside each:

```rust
        acc.person_env(job.app_id, distinct_id, environment_id, at, 1, 0);
```

with `(1, 0)` on the analytics path and `(0, 1)` on the error path, matching the `events_delta`/`errors_delta` the device fold already passes at that site. Use the same `distinct_id` value the device fold uses, so the two agree about which identity a signal belongs to.

- [ ] **Step 4: Pass it to `write_rows`**

Change the `WriteSet` construction from `person_envs: &[]` to:

```rust
            person_envs: &acc.person_envs,
```

- [ ] **Step 5: Verify the ingest pipeline still builds and its tests pass**

```bash
cd backend && cargo test -p sauron-pipeline -- --nocapture
```

Expected: PASS. Confirm the output does not contain `skipping`.

---

### Task 8: The single-item path in `process.rs`

**Files:**
- Modify: `backend/crates/sauron-db/src/repo.rs` — new `bump_person_env`
- Modify: `backend/crates/sauron-pipeline/src/process.rs` — `rollup()` (~95-147)

**Interfaces:**
- Produces: `repo::bump_person_env(conn, app_id, distinct_id, environment_id, at, events_delta, errors_delta, sessions_delta) -> QueryResult<usize>`.

**Why this task exists:** `process.rs` is the unbatched path and is still live (`process_event`, `process.rs:82`). If only the batched path maintains the rollup, a deployment running unbatched silently stops updating it — the rollup goes stale while every gate stays green.

- [ ] **Step 1: Add the single-row upsert to `repo.rs`**

```rust
/// Single-row twin of [`crate::batch::bump_person_envs`], for the unbatched
/// `process::rollup` path. The conflict arm is identical — if one changes, both
/// change, or the two ingest paths disagree about what a person's counters mean.
#[allow(clippy::too_many_arguments)]
pub async fn bump_person_env(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    distinct_id: &str,
    environment_id: Option<Uuid>,
    at: DateTime<Utc>,
    events_delta: i64,
    errors_delta: i64,
    sessions_delta: i64,
) -> QueryResult<usize> {
    if distinct_id.is_empty() {
        return Ok(0);
    }
    diesel::sql_query(
        "INSERT INTO event_user_environments \
           (app_id, distinct_id, environment_id, first_seen, last_seen, \
            events_count, errors_count, sessions_count) \
         VALUES ($1, $2, $3, $4, $4, $5, $6, $7) \
         ON CONFLICT (app_id, distinct_id, \
                      COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid)) \
         DO UPDATE SET \
            first_seen = LEAST(event_user_environments.first_seen, EXCLUDED.first_seen), \
            last_seen = GREATEST(event_user_environments.last_seen, EXCLUDED.last_seen), \
            events_count = event_user_environments.events_count + EXCLUDED.events_count, \
            errors_count = event_user_environments.errors_count + EXCLUDED.errors_count, \
            sessions_count = event_user_environments.sessions_count + EXCLUDED.sessions_count, \
            updated_at = now()",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Text, _>(distinct_id.to_string())
    .bind::<Nullable<SqlUuid>, _>(environment_id)
    .bind::<Timestamptz, _>(at)
    .bind::<BigInt, _>(events_delta)
    .bind::<BigInt, _>(errors_delta)
    .bind::<BigInt, _>(sessions_delta)
    .execute(conn)
    .await
}
```

- [ ] **Step 2: Call it from `rollup()`**

In `crates/sauron-pipeline/src/process.rs`, `rollup()` already narrows `distinct_id` to `Option<&str>` non-empty and holds `environment_id`, `at` and both deltas. Change the session block to capture whether it inserted, then add the person bump:

```rust
    let mut sessions_delta = 0i64;
    if let Some(sid) = session_id {
        // `bump_session` reports whether it INSERTED. A session is bumped again
        // by every signal it carries, so crediting the rollup per bump would
        // count one session many times.
        if let Ok(true) = repo::bump_session(
            conn,
            job.app_id,
            sid,
            distinct_id,
            info.device_key.as_deref(),
            at,
            context,
            job.release.as_deref(),
            environment_id,
            job.ip.as_deref(),
            events_delta,
            errors_delta,
        )
        .await
        {
            sessions_delta = 1;
        }
    }

    if let Some(did) = distinct_id {
        // Deliberately `let _ =`, matching the two bumps above: this path's
        // writes are best-effort and a rollup miss must not fail an event that
        // is already durable.
        let _ = repo::bump_person_env(
            conn,
            job.app_id,
            did,
            environment_id,
            at,
            events_delta,
            errors_delta,
            sessions_delta,
        )
        .await;
    }
```

Keep the existing `bump_device` block between them, unchanged.

- [ ] **Step 3: Verify both ingest paths agree**

```bash
cd backend && cargo test -p sauron-pipeline -p sauron-db -- --nocapture
```

Expected: PASS, no `skipping` in the output.

---

### Task 9: The backfill and its marker

**Files:**
- Create: `backend/crates/sauron-db/src/person_env_backfill.rs`
- Modify: `backend/crates/sauron-db/src/lib.rs`
- Modify: `backend/bins/sauron-migrate/src/main.rs`
- Test: `backend/crates/sauron-db/tests/person_env_rollup.rs`

**Interfaces:**
- Produces:
  - `person_env_backfill::backfill_app(conn, app_id, cutoff: DateTime<Utc>) -> QueryResult<u64>`
  - `person_env_backfill::backfill_all(pool: &PgPool) -> anyhow::Result<()>`
  - `person_env_backfill::is_backfilled(conn, app_id) -> QueryResult<bool>` — consumed by Task 10.

**The correctness rule:** the write path (Tasks 6-8) bumps the rollup from the moment migration 56 lands, including for un-backfilled apps. So the backfill **cannot** use `ON CONFLICT DO NOTHING` — a live bump that creates the row first would make the backfill skip it, leaving that person short by their entire history, silently and permanently. It is additive against a cutoff instead.

- [ ] **Step 1: Write the failing test**

Add to `backend/crates/sauron-db/tests/person_env_rollup.rs`:

```rust
/// The backfill runs while ingest is live, so it must ADD to whatever the write
/// path has already written rather than skip rows that already exist. This is
/// the test that catches the `ON CONFLICT DO NOTHING` mistake — which loses a
/// person's entire history and does it silently.
#[tokio::test]
async fn backfill_adds_to_rows_the_write_path_already_created() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs().await;
    let cutoff = chrono::Utc::now();

    // Simulate a live bump landing before the backfill reaches this person.
    sauron_db::batch::bump_person_envs(
        &mut conn,
        &[sauron_db::batch::PersonEnvBump {
            app_id: ids.app_id,
            distinct_id: ids.shared_distinct_id.clone(),
            environment_id: Some(ids.env_a),
            first_at: cutoff,
            last_at: cutoff,
            events_delta: 1,
            errors_delta: 0,
            sessions_delta: 0,
        }],
    )
    .await
    .expect("live bump");

    sauron_db::person_env_backfill::backfill_app(&mut conn, ids.app_id, cutoff)
        .await
        .expect("backfill");

    #[derive(diesel::QueryableByName)]
    struct Row {
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        events_count: i64,
    }

    let row: Row = diesel::sql_query(
        "SELECT events_count FROM event_user_environments \
         WHERE app_id=$1 AND distinct_id=$2 AND environment_id=$3",
    )
    .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
    .bind::<diesel::sql_types::Text, _>(ids.shared_distinct_id.clone())
    .bind::<diesel::sql_types::Uuid, _>(ids.env_a)
    .get_result(&mut conn)
    .await
    .expect("rollup row");

    // seed_two_envs gives shared_distinct_id 4 analytics_events in env_a, all
    // before `cutoff`; the live bump added 1 more.
    assert_eq!(
        row.events_count, 5,
        "backfill must ADD its cutoff-bounded aggregate to the live row, not skip it"
    );
}

/// The marker must never be visible before the data it claims. If it is, reads
/// switch to a half-populated rollup and the persons page goes quiet-wrong
/// instead of erroring.
#[tokio::test]
async fn marker_is_absent_until_the_backfill_finishes() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs().await;

    assert!(
        !sauron_db::person_env_backfill::is_backfilled(&mut conn, ids.app_id)
            .await
            .expect("marker check"),
        "an app with no backfill row must read as not backfilled"
    );

    sauron_db::person_env_backfill::backfill_app(&mut conn, ids.app_id, chrono::Utc::now())
        .await
        .expect("backfill");

    assert!(
        sauron_db::person_env_backfill::is_backfilled(&mut conn, ids.app_id)
            .await
            .expect("marker check"),
        "backfill_app must write the marker in the same transaction as its final batch"
    );
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cd backend && cargo test -p sauron-db --test person_env_rollup backfill -- --nocapture
cd backend && cargo test -p sauron-db --test person_env_rollup marker_is_absent -- --nocapture
```

Expected: FAIL to compile — the module does not exist.

- [ ] **Step 3: Implement the module**

Create `backend/crates/sauron-db/src/person_env_backfill.rs`:

```rust
//! Populate `event_user_environments` for data that predates the rollup.
//!
//! Not part of a migration and not part of `sauron-migrate`'s default no-arg
//! path, both on purpose: `require_current_schema` fail-closes the API on a
//! stale schema, and every RPM daemon `Requires=` the migrator unit, so
//! anything slow in either place is a boot outage proportional to retained data.
//!
//! ADDITIVE AGAINST A CUTOFF, not `ON CONFLICT DO NOTHING`. The write path bumps
//! this table from the moment migration 56 lands, including for apps that are
//! not yet backfilled, so a live bump can create a row before the backfill
//! reaches that person. `DO NOTHING` would skip it and leave that person short
//! by their entire history — silently, and permanently. Instead this aggregates
//! only rows strictly before `cutoff` and adds them to whatever is there; live
//! bumps carry signals at or after `cutoff`, so the two sets are disjoint.
//!
//! KNOWN RESIDUAL: a backdated event — an SDK offline queue replaying with an
//! old `occurred_at` — that arrives between `cutoff` and the backfill finishing
//! is counted twice. Bounded by the backfill's duration, and counter drift is
//! already an accepted property of this table (same trade as `devices`).

use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel::sql_types::{Timestamptz, Uuid as SqlUuid};
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use uuid::Uuid;

use crate::PgPool;

/// Aggregate one app's pre-`cutoff` history into the rollup and mark it done.
///
/// The marker insert shares this function's transaction with the aggregate, so
/// the marker can never be visible before the data it claims.
pub async fn backfill_app(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    cutoff: DateTime<Utc>,
) -> QueryResult<u64> {
    conn.batch_execute("BEGIN").await?;
    let r = backfill_app_inner(conn, app_id, cutoff).await;
    match r {
        Ok(n) => {
            conn.batch_execute("COMMIT").await?;
            Ok(n)
        }
        Err(e) => {
            let _ = conn.batch_execute("ROLLBACK").await;
            Err(e)
        }
    }
}

async fn backfill_app_inner(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    cutoff: DateTime<Utc>,
) -> QueryResult<u64> {
    // One UNION ALL over the three signal tables, grouped once. The three legs
    // mirror the membership `EXISTS` legs exactly — anyone with a row in any of
    // the three qualifies — so the rollup admits precisely the people the live
    // query admits.
    let n = diesel::sql_query(
        "INSERT INTO event_user_environments \
           (app_id, distinct_id, environment_id, first_seen, last_seen, \
            events_count, errors_count, sessions_count) \
         SELECT app_id, distinct_id, environment_id, \
                min(first_at), max(last_at), \
                sum(ev), sum(er), sum(se) \
         FROM ( \
             SELECT app_id, distinct_id, environment_id, occurred_at AS first_at, \
                    occurred_at AS last_at, 1::bigint AS ev, 0::bigint AS er, 0::bigint AS se \
             FROM analytics_events WHERE app_id=$1 AND occurred_at < $2 AND distinct_id <> '' \
             UNION ALL \
             SELECT app_id, distinct_id, environment_id, occurred_at, occurred_at, \
                    0::bigint, 1::bigint, 0::bigint \
             FROM error_events WHERE app_id=$1 AND occurred_at < $2 AND distinct_id <> '' \
             UNION ALL \
             SELECT app_id, distinct_id, environment_id, started_at, last_event_at, \
                    0::bigint, 0::bigint, 1::bigint \
             FROM sessions WHERE app_id=$1 AND started_at < $2 \
                           AND distinct_id IS NOT NULL AND distinct_id <> '' \
         ) t \
         GROUP BY app_id, distinct_id, environment_id \
         ON CONFLICT (app_id, distinct_id, \
                      COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid)) \
         DO UPDATE SET \
            first_seen = LEAST(event_user_environments.first_seen, EXCLUDED.first_seen), \
            last_seen = GREATEST(event_user_environments.last_seen, EXCLUDED.last_seen), \
            events_count = event_user_environments.events_count + EXCLUDED.events_count, \
            errors_count = event_user_environments.errors_count + EXCLUDED.errors_count, \
            sessions_count = event_user_environments.sessions_count + EXCLUDED.sessions_count, \
            updated_at = now()",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Timestamptz, _>(cutoff)
    .execute(conn)
    .await? as u64;

    diesel::sql_query(
        "INSERT INTO event_user_env_backfill (app_id, completed_at) VALUES ($1, now()) \
         ON CONFLICT (app_id) DO UPDATE SET completed_at = now()",
    )
    .bind::<SqlUuid, _>(app_id)
    .execute(conn)
    .await?;

    Ok(n)
}

/// Whether `list_persons` may read the rollup for this app.
pub async fn is_backfilled(conn: &mut AsyncPgConnection, app_id: Uuid) -> QueryResult<bool> {
    #[derive(QueryableByName)]
    struct Present {
        #[diesel(sql_type = diesel::sql_types::Bool)]
        present: bool,
    }
    let r: Present = diesel::sql_query(
        "SELECT EXISTS (SELECT 1 FROM event_user_env_backfill WHERE app_id=$1) AS present",
    )
    .bind::<SqlUuid, _>(app_id)
    .get_result(conn)
    .await?;
    Ok(r.present)
}

/// Backfill every app that has no marker yet, one app per transaction.
///
/// One app at a time rather than one statement for everything: a single
/// transaction over every app's history holds locks for its whole duration and
/// loses all progress on any failure.
pub async fn backfill_all(pool: &PgPool) -> anyhow::Result<()> {
    let mut conn = crate::conn(pool).await?;

    #[derive(QueryableByName)]
    struct AppId {
        #[diesel(sql_type = SqlUuid)]
        id: Uuid,
    }
    let apps: Vec<AppId> = diesel::sql_query(
        "SELECT id FROM apps WHERE id NOT IN (SELECT app_id FROM event_user_env_backfill) \
         ORDER BY id",
    )
    .get_results(&mut conn)
    .await?;

    tracing::info!(apps = apps.len(), "person-env backfill starting");
    for a in apps {
        // One cutoff per app, taken immediately before that app's aggregate, so
        // the disjointness argument holds per app rather than depending on how
        // long earlier apps took.
        let cutoff = Utc::now();
        let n = backfill_app(&mut conn, a.id, cutoff).await?;
        tracing::info!(app_id = %a.id, rows = n, "person-env backfill done");
    }
    tracing::info!("person-env backfill complete");
    Ok(())
}
```

Add `pub mod person_env_backfill;` to `backend/crates/sauron-db/src/lib.rs`.

- [ ] **Step 4: Add the opt-in command to `sauron-migrate`**

In `backend/bins/sauron-migrate/src/main.rs`, after the existing migration run:

```rust
    // Opt-in, and deliberately NOT part of the default no-arg path. This binary
    // is the `sauron-migrate.service` oneshot that every RPM daemon pulls in via
    // `Requires=`, so anything slow here delays — or, on failure, blocks — every
    // daemon's start job. Operators run `sauron-migrate backfill-person-envs`
    // by hand, after the migrations, at a time of their choosing.
    if std::env::args().any(|a| a == "backfill-person-envs") {
        let pool = sauron_db::build_pool(&url, 4)?;
        sauron_db::person_env_backfill::backfill_all(&pool).await?;
    }
```

Confirm `sauron-migrate/Cargo.toml` needs no new dependency — `sauron-db` and `anyhow` are already there.

- [ ] **Step 5: Run tests to verify they pass**

```bash
cd backend && cargo test -p sauron-db --test person_env_rollup -- --nocapture
```

Expected: all PASS.

- [ ] **Step 6: Verify the default path is unchanged**

```bash
cd backend && DATABASE_URL="$TEST_DATABASE_URL" cargo run -p sauron-migrate
```

Expected: exits 0 with `migrations up to date` and **no** backfill log lines — the no-arg path must not have changed.

```bash
cd backend && DATABASE_URL="$TEST_DATABASE_URL" cargo run -p sauron-migrate -- backfill-person-envs
```

Expected: exits 0, logs `person-env backfill complete`.

---

### Task 10: `list_persons` reads the rollup for backfilled apps

**Files:**
- Modify: `backend/crates/sauron-db/src/repo.rs` — `list_persons`
- Test: `backend/crates/sauron-db/tests/env_scoping.rs`, `backend/crates/sauron-db/tests/person_env_rollup.rs`

**Interfaces:**
- Consumes: `person_env_backfill::is_backfilled` (Task 9).
- Produces: no signature change. `list_persons` keeps its exact parameters and `Vec<PersonRow>` return.

**Constraints on the emitted SQL:**
- The person subquery keeps the alias `eu`, because `person_sort_spec` (`bins/sauron-api/src/routes/analytics.rs:195`) emits the qualified column `eu.distinct_id`.
- The other five sort columns are unqualified output aliases resolved against the select list, so the rollup branch must reuse the exact names `first_seen`, `last_seen`, `events_count`, `errors_count`, `sessions_count`. `SortSpec` then needs no change.
- `event_users` is still joined, for `properties` (app-wide by design — see `PersonRow`'s doc comment) and for the `ILIKE` search over `distinct_id` / `properties::text`.
- All four `EnvFilter` variants must work. `Subset` is a distinct variant, not a flavour of `One`, and is the easiest to omit by accident.

- [ ] **Step 1: Write the failing test**

Add to `backend/crates/sauron-db/tests/person_env_rollup.rs`:

```rust
/// The rollup branch must return exactly what the live branch returns, for
/// every scope. Both branches are "correct" in isolation; the failure this
/// catches is the two disagreeing, which no single-branch test can see.
#[tokio::test]
async fn rollup_branch_matches_live_branch_for_every_scope() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs().await;

    let scopes = vec![
        ("All", EnvFilter::All),
        ("One(env_a)", EnvFilter::One(ids.env_a)),
        ("One(env_b)", EnvFilter::One(ids.env_b)),
        ("Subset", EnvFilter::Subset(vec![ids.env_a, ids.env_b])),
        ("Unattributed", EnvFilter::Unattributed),
    ];

    // Before the backfill: the live branch. Capture its answers.
    let mut live = Vec::new();
    for (name, env) in &scopes {
        let rows = sauron_db::repo::list_persons(
            &mut conn,
            ReadScope::new(ids.app_id, env.clone()),
            None,
            200,
            0,
            common::default_person_sort(),
        )
        .await
        .unwrap_or_else(|e| panic!("live list_persons under {name}: {e}"));
        live.push((name, rows));
    }

    // Backfill, then the same calls take the rollup branch.
    sauron_db::person_env_backfill::backfill_app(&mut conn, ids.app_id, chrono::Utc::now())
        .await
        .expect("backfill");

    for ((name, before), (_, env)) in live.iter().zip(scopes.iter()) {
        let after = sauron_db::repo::list_persons(
            &mut conn,
            ReadScope::new(ids.app_id, env.clone()),
            None,
            200,
            0,
            common::default_person_sort(),
        )
        .await
        .unwrap_or_else(|e| panic!("rollup list_persons under {name}: {e}"));

        assert_eq!(
            before.len(),
            after.len(),
            "{name}: rollup branch admitted a different number of people than the live branch"
        );
        for (b, a) in before.iter().zip(after.iter()) {
            assert_eq!(b.distinct_id, a.distinct_id, "{name}: ordering diverged");
            assert_eq!(b.events_count, a.events_count, "{name}: {} events", b.distinct_id);
            assert_eq!(b.errors_count, a.errors_count, "{name}: {} errors", b.distinct_id);
            assert_eq!(b.sessions_count, a.sessions_count, "{name}: {} sessions", b.distinct_id);
        }
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cd backend && cargo test -p sauron-db --test person_env_rollup rollup_branch_matches -- --nocapture
```

Expected: FAIL — after the backfill `list_persons` still takes the live branch, so counts double (the rollup's aggregate is now added on top of nothing, but the live LATERALs are unchanged) or the assertion on ordering trips. Either way it must not pass before Step 3.

- [ ] **Step 3: Split the two shapes into named builders**

Rename the existing body's string construction to `list_persons_live_sql(env: &EnvFilter, sort: &SortSpec) -> String` (Task 2 already extracted this) and add:

```rust
/// The rollup shape. `event_user_environments` carries one row per (person,
/// environment) with the counts and both timestamps already computed, so the
/// three LATERALs and the membership `IN (…)` all collapse into a join — and,
/// critically, `ORDER BY … LIMIT` now applies to a single indexed table instead
/// of to a blocking Sort over every person in the app. That is the actual fix
/// for the 30s timeout: page size caps the work again.
///
/// `eu` stays the person alias because `person_sort_spec` emits the qualified
/// `eu.distinct_id`; the other five sort columns are output aliases and must
/// keep these exact names.
fn list_persons_rollup_sql(env: &EnvFilter, sort: &SortSpec) -> String {
    let env_sql = env.sql_fragment_for("r", 5);
    let order_by = sort.order_by();
    format!(
        "SELECT eu.distinct_id, eu.properties, \
                r.first_seen AS first_seen, r.last_seen AS last_seen, \
                r.events_count AS events_count, r.errors_count AS errors_count, \
                r.sessions_count AS sessions_count \
         FROM ( \
             SELECT app_id, distinct_id, \
                    min(first_seen) AS first_seen, max(last_seen) AS last_seen, \
                    sum(events_count)::bigint AS events_count, \
                    sum(errors_count)::bigint AS errors_count, \
                    sum(sessions_count)::bigint AS sessions_count \
             FROM event_user_environments r \
             WHERE app_id=$1{env_sql} \
             GROUP BY app_id, distinct_id \
         ) r \
         JOIN event_users eu ON eu.app_id = r.app_id AND eu.distinct_id = r.distinct_id \
         WHERE eu.distinct_id ILIKE $2 OR eu.properties::text ILIKE $2 \
         ORDER BY {order_by} \
         LIMIT $3 OFFSET $4"
    )
}
```

The `GROUP BY` is correct for all four variants: under `One`/`Unattributed` it groups a single row per person (a no-op), and under `All`/`Subset` it sums across the environments the filter admits. One shape, not four.

- [ ] **Step 4: Branch on the marker**

Replace `list_persons`' body with:

```rust
    let pattern = search.map(like_contains).unwrap_or_else(|| "%".to_string());
    // Two shapes until every deployment is backfilled. The marker is per-app and
    // is written in the same transaction as that app's final backfill batch, so
    // it can never be visible before the data it claims — a marker that ran
    // ahead of its data would make this page quiet-wrong rather than error.
    let q = if crate::person_env_backfill::is_backfilled(conn, scope.app_id).await? {
        list_persons_rollup_sql(&scope.env, &sort)
    } else {
        list_persons_live_sql(&scope.env, &sort)
    };
    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Text, _>(pattern)
        .bind::<BigInt, _>(limit)
        .bind::<BigInt, _>(offset);
    stmt = crate::bind_env!(stmt, &scope.env);
    stmt.get_results(conn).await
```

- [ ] **Step 5: Update the doc comments this invalidates**

`list_persons`' comment block contains measured claims that no longer describe the marked path — specifically the `EnvFilter::One before/after 900 -> 31463` cost table, "It scales with the person count, not the page size", and "No index can buy the scoped case back … the answer is a materialized per-(person, environment) rollup, not an index and not a second code path."

That last sentence now describes what was built. Rewrite the block to say: these costs describe the **live** shape, which is now the fallback for un-backfilled apps only; the rollup shape is what the marked path uses; and the "second code path" it warned against was accepted deliberately, bounded by the marker. Do not delete the measured numbers — they are still true of the fallback.

Also extend `person_sort_spec`'s `nulls_last` comment (`bins/sauron-api/src/routes/analytics.rs:203`): the claim still holds, but now for a second reason (rollup columns are `NOT NULL`), and a reader checking only the live branch would think the comment had gone stale.

- [ ] **Step 6: Run the full backend suite**

```bash
cd backend && cargo test --workspace -- --nocapture 2>&1 | tail -40
```

Expected: PASS. Confirm the executed-test count is at or above the **1391** baseline and that no line reads `skipping`. A count near 1354 means the sandbox netns blocked the database and nothing actually ran.

---

### Task 11: Measure

**Files:** none modified. This task produces numbers, not code.

**Why it is a task:** the spec commits to reporting measured before/after rather than asserting improvement, and to separating A's contribution from B's. Three points, not two.

- [ ] **Step 1: Build a fixture that can show the effect**

Use `crebain` to generate **many apps and several environments** with events spread **outside** the 30-day window as well as inside. Both properties are load-bearing:
- A single-app / single-environment fixture has every row matching, so the planner correctly prefers a seq scan and the new indexes appear to change nothing.
- A fixture whose events all fall inside the query window understates the bug — production's cost is the data *outside* the window that still gets scanned.

Before running any soak, check free disk. The root filesystem has run at 90% / ~12GB free, and a 10-minute 2× run adds ~12GB and has previously filled it, crash-looping an unrelated container. Bench Redis must run with `--save '' --appendonly no`.

- [ ] **Step 2: Time all three points**

For the reported query — `sort=last_seen&limit=51&offset=0&environment_id=<env>` — record:

1. **Baseline** — stash Tasks 1-10, or check the timing against a build without migrations 55/56 applied.
2. **After A** — migrations 55 applied, membership rewritten, marker absent so the live branch runs.
3. **After B** — backfill run, marker present, rollup branch runs.

- [ ] **Step 3: Confirm the plan actually changed**

```bash
psql "$DATABASE_URL" -c "EXPLAIN (ANALYZE, BUFFERS) <the emitted query>"
```

Expected at point 3: no `Seq Scan` across `analytics_events` partitions, no blocking `Sort` over the full person set, and `Limit` capping the work. If a blocking `Sort` is still present, the rollup's sort index is not being used and the fix is incomplete regardless of the wall-clock number.

- [ ] **Step 4: Write the numbers into the spec**

Append a "Measured" section to `docs/superpowers/specs/2026-08-12-persons-env-slowness-design.md` with all three timings and the fixture's shape (app count, environment count, `event_users` count, event count, window span). A number without its fixture is not reproducible.

---

## Self-Review

**Spec coverage:** A1 → Task 1. A2 → Task 2. B1 schema → Task 3. B2 write path → Tasks 4-8 (session insert-detection, the bump, the transaction wiring, the `Acc` fold, the unbatched path). B3 backfill + marker → Task 9. B4 read path → Task 10. Verification items 1-6 → Tasks 2, 5, 6, 9, 10, 11. Hazards: silent-empty → Task 9 Step 1's marker test; counter drift → documented in Task 9's module comment; `All` regression → Task 10's cross-scope equivalence test; two query shapes → Task 10 Step 5; deadlock surface → Task 5's sort-before-upsert.

**Known gap, stated rather than hidden:** Task 10's equivalence test compares the two branches on `seed_two_envs`' fixture, which has no tiered/cold data. Rollup counts and live counts will diverge for an app whose old partitions have rotated to Parquet — by design (the rollup is the more complete number), but no test in this plan covers it, because the fixture cannot produce it. Task 11's fixture is the place to check it manually.

**Type consistency:** `PersonEnvBump` fields are identical in Tasks 5, 6, 7 and 9. `bump_sessions` returns `Vec<(Uuid, String)>` in Task 4 and is consumed as that in Task 6. `bump_session` returns `bool` in Task 4 and is consumed as `Ok(true)` in Task 8. `is_backfilled` is defined in Task 9 and consumed in Task 10. Sort aliases `first_seen` / `last_seen` / `events_count` / `errors_count` / `sessions_count` and the `eu` person alias are identical across both branches in Task 10.
