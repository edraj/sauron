# `device_environments` Rollup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `GET /v1/apps/{id}/device-groups?…&environment_id=…` bounded by page size instead of by the app's device count, without changing a single displayed number.

**Architecture:** Add `device_environments`, the per-(device, environment) twin of migration 56's `event_user_environments`. The write path bumps it per batch; a one-shot opt-in backfill populates history and writes a per-app marker; `list_device_groups` reads the rollup only for marked apps and otherwise falls back to today's live query. `sessions_count` deliberately stays a live LATERAL — see Global Constraints.

**Tech Stack:** Rust 1.82 (MSRV floor — the RPM spec builds against it), diesel 2 + diesel-async 0.9, raw `sql_query` throughout, PostgreSQL 16, tokio.

## Global Constraints

- **MSRV is 1.82.** No async closures. Transactions use explicit `BEGIN`/`COMMIT` via `batch_execute`, exactly as `batch::write_rows_once` and `person_env_backfill::backfill_app` do.
- **`device_environments.environment_id` REFERENCES `app_environments(id)`, NOT `environments(id)`.** Migration 33 renamed the old `environments` table to `app_environments` and created a new catalogue under the old name; a rename preserves the OID, so every pre-existing FK silently followed it. Existing migration DDL text says `environments(id)` and is wrong. Verify with `pg_constraint`, never by reading migration source.
- **`environment_id` is NULLABLE** — `EnvFilter::Unattributed` is a real, surfaced scope. Uniqueness is therefore an expression index over `COALESCE(environment_id, nil-uuid)`, and **every `ON CONFLICT` against this table must name that same expression** or it silently degrades into an unconstrained insert.
- **`sessions_count` is NOT read from the rollup by `list_device_groups`.** The live query windows it (`count(*) FILTER (WHERE started_at >= $2)`) while `events_count`/`errors_count` are lifetime. Reading a lifetime rollup column changed the number on **all 40 rows** of the measurement fixture. The column is still written (the drill-down in Task 8 uses it, and the table stays shape-identical to its persons twin), but the group query keeps one live `sessions` LATERAL. `sessions` is not partitioned, so that is one index probe per device against the old 45.
- **Never commit and never create branches** — leave every change in the working tree.
- The backfill must **never** be on `sauron-migrate`'s default no-arg path: every RPM daemon `Requires=` that unit and systemd never retries a failed start job, so slow work there is a boot outage.

## Measured baseline (the numbers this plan must beat)

Fixture: 40,000 devices / 13,333 qualifying under one env / 1.68M analytics events / 15 partitions / 3 apps × 3 envs, most volume outside the 30-day window.

| variant | execution | planner cost |
|---|---|---|
| current live query | 4,639 ms | 4,402,800 |
| rollup + live windowed sessions LATERAL | **105 ms** | 40,768 |
| (rejected) rollup with lifetime `sessions_count` | 36 ms | 5,392 |

The rejected row is faster and **wrong**: 40/40 rows differ on `sessions_count`.

Scaling that motivates this: 1,111 qualifying devices → 226 ms; 13,333 → 4,639 ms. Production runs 29 partitions against the fixture's 15.

## File Structure

| File | Responsibility |
|---|---|
| `backend/migrations/2026-08-12-000059_device_environments/{up,down}.sql` | Table, expression unique index, sort indexes, marker table |
| `backend/crates/sauron-db/src/schema.rs` | diesel `table!` entries for both new tables |
| `backend/crates/sauron-db/src/batch.rs` | `DeviceEnvBump`, `bump_device_envs`, `WriteSet.device_envs`, session crediting |
| `backend/crates/sauron-pipeline/src/batch.rs` | `Acc` fold producing `DeviceEnvBump` rows |
| `backend/crates/sauron-db/src/device_env_backfill.rs` | **new** — `backfill_app`, `is_backfilled`, `backfill_all` |
| `backend/crates/sauron-db/src/lib.rs` | `pub mod device_env_backfill;` |
| `backend/bins/sauron-migrate/src/main.rs` | `backfill-device-envs` opt-in subcommand |
| `backend/crates/sauron-db/src/repo.rs` | `list_device_groups_rollup_sql`, live/rollup switch, `list_devices` (Task 8) |
| `backend/crates/sauron-db/tests/device_env_rollup.rs` | **new** — schema, write-path, equivalence tests |

---

### Task 1: Migration — `device_environments` schema

**Files:**
- Create: `backend/migrations/2026-08-12-000059_device_environments/up.sql`
- Create: `backend/migrations/2026-08-12-000059_device_environments/down.sql`
- Create: `backend/crates/sauron-db/tests/device_env_rollup.rs`

**Interfaces:**
- Produces: tables `device_environments`, `device_env_backfill`.

- [ ] **Step 1: Write the failing test**

Create `backend/crates/sauron-db/tests/device_env_rollup.rs`:

```rust
//! `device_environments` — the per-(device, environment) rollup that makes
//! `list_device_groups` bounded by page size instead of by device count.

mod common;

use common::TestDb;
use diesel_async::RunQueryDsl;

/// `environment_id` is NULLABLE because `EnvFilter::Unattributed` is a real
/// scope, and NULL never equals NULL — so a plain `UNIQUE (app_id, device_key,
/// environment_id)` would let one device accumulate unlimited unattributed rows
/// and every upsert against them would INSERT instead of UPDATE. Counters would
/// silently stop accumulating for exactly the scope that has no environment.
#[tokio::test]
async fn unattributed_rollup_rows_are_unique_per_device() {
    let Some(db) = TestDb::setup().await else {
        panic!("TEST_DATABASE_URL unset — this test must not silently skip");
    };
    let mut conn = db.conn().await;
    let app_id = db.seed_app().await;

    let insert = || {
        diesel::sql_query(
            "INSERT INTO device_environments \
               (app_id, device_key, environment_id, first_seen, last_seen) \
             VALUES ($1, 'dev-1', NULL, now(), now())",
        )
        .bind::<diesel::sql_types::Uuid, _>(app_id)
    };

    insert().execute(&mut conn).await.expect("first insert");
    assert!(
        insert().execute(&mut conn).await.is_err(),
        "a second NULL-environment row for the same device must be rejected"
    );
}
```

If `TestDb` exposes no `seed_app()`, use the helper `tests/common/mod.rs` already provides — check `seed_two_envs()`'s return struct, which carries `app_id`.

- [ ] **Step 2: Run test to verify it fails**

```bash
cd backend && cargo test -p sauron-db --test device_env_rollup -- --nocapture
```

Expected: FAIL — relation `device_environments` does not exist. **If it prints `skipping`, stop** — see the harness note in Task 9; a skipped test is not a failing test.

- [ ] **Step 3: Write the migration**

`backend/migrations/2026-08-12-000059_device_environments/up.sql`:

```sql
-- Per-(device, environment) rollup for the Devices inventory.
--
-- devices carries no environment_id, so list_device_groups derived membership,
-- first_seen/last_seen and the counts from three membership EXISTS plus three
-- LEFT JOIN LATERALs over analytics_events/error_events/sessions -- the EXISTS
-- once per device in the window, the LATERALs once per QUALIFYING device, each
-- an Append across every partition. GROUP BY then consumed all of them to emit
-- ~40 rows, so `limit` bounded nothing.
--
-- Migration 53 already made each probe an index-only scan; this removes the
-- probes themselves, which no index can. Measured on a 40,000-device /
-- 13,333-qualifying / 1.68M-event / 15-partition fixture, device-groups under
-- One(env) over 30 days: 4,639ms -> 105ms, with zero row differences in either
-- direction. The same fixture at 1,111 qualifying devices took 226ms, i.e. the
-- cost was linear in device count and production runs 29 partitions, not 15.
--
-- environment_id is NULLABLE on purpose: EnvFilter::Unattributed is a real,
-- surfaced scope (rows ingested before environments existed), and it must be a
-- row here so that "All" equals the sum of the individual environments rather
-- than exceeding it.
--
-- IT REFERENCES app_environments, NOT environments, AND THE MIGRATION SOURCE
-- WILL TELL YOU OTHERWISE. Migration 33 (env_per_project) RENAMED the old
-- environments table to app_environments and created a new catalogue under the
-- old name. A rename preserves the OID, so the pre-existing foreign keys on
-- analytics_events/error_events/workflows silently followed it -- pg_constraint
-- says app_environments while init/up.sql still says environments. The value
-- handed to EnvFilter::One by the API is an app_environments.id; a table
-- written against the catalogue would reject every real id.
CREATE TABLE device_environments (
    app_id          uuid        NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    device_key      text        NOT NULL,
    environment_id  uuid        NULL REFERENCES app_environments(id) ON DELETE CASCADE,
    first_seen      timestamptz NOT NULL,
    last_seen       timestamptz NOT NULL,
    events_count    bigint      NOT NULL DEFAULT 0,
    errors_count    bigint      NOT NULL DEFAULT 0,
    sessions_count  bigint      NOT NULL DEFAULT 0,
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now()
);

-- NULL never equals NULL, so a plain UNIQUE (app_id, device_key,
-- environment_id) would let one device accumulate unlimited unattributed rows,
-- and every upsert against them would INSERT instead of UPDATE -- counters
-- would silently stop accumulating for exactly the scope that has no
-- environment. The nil uuid is safe as the sentinel: it has no app_environments
-- row, and the foreign key above would reject it as a real value.
--
-- EVERY ON CONFLICT against this table must name this same expression list.
CREATE UNIQUE INDEX device_env_key_idx
    ON device_environments
       (app_id, device_key, COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid));

-- The read path joins devices to this table filtered by (app_id,
-- environment_id) and then groups, so the driving lookup is that pair. The
-- trailing last_seen serves the default ordering.
CREATE INDEX device_env_app_env_idx ON device_environments (app_id, environment_id, last_seen DESC);

-- The join back to devices is on (app_id, device_key); without this the planner
-- has only the expression index above, whose leading columns suit the upsert
-- rather than the join.
CREATE INDEX device_env_app_device_idx ON device_environments (app_id, device_key);

-- Which apps' rollups are complete. Reads fall back to the live query for any
-- app without a row here, so a half-populated rollup is never read. The marker
-- is written in the same transaction as that app's backfill aggregate, so it
-- can never be visible before the data it claims -- a marker that ran ahead of
-- its data would make the Devices page quiet-wrong rather than error.
--
-- A dedicated table rather than runtime_settings because the marker is per-app
-- and wants the foreign key.
CREATE TABLE device_env_backfill (
    app_id       uuid        PRIMARY KEY REFERENCES apps(id) ON DELETE CASCADE,
    completed_at timestamptz NOT NULL
);
```

`backend/migrations/2026-08-12-000059_device_environments/down.sql`:

```sql
DROP TABLE IF EXISTS device_env_backfill;
DROP TABLE IF EXISTS device_environments;
```

- [ ] **Step 4: Run test to verify it passes**

```bash
cd backend && cargo test -p sauron-db --test device_env_rollup -- --nocapture
```

Expected: PASS.

- [ ] **Step 5: Verify the FK target is what the plan claims, not what the DDL text says**

```bash
psql "$TEST_DATABASE_URL" -c "SELECT conname, confrelid::regclass FROM pg_constraint WHERE conrelid='device_environments'::regclass AND contype='f';"
```

Expected: the `environment_id` constraint shows `app_environments`. If it shows `environments`, the migration is wrong and every real `environment_id` will be rejected at runtime.

- [ ] **Step 6: Verify the migration applies to a fresh database**

```bash
cd backend && DATABASE_URL="$TEST_DATABASE_URL" cargo run -p sauron-migrate
```

Expected: exits 0, logs `migrations up to date`, 59 migrations applied.

---

### Task 2: diesel `schema.rs` entries

**Files:**
- Modify: `backend/crates/sauron-db/src/schema.rs` — after the `event_user_env_backfill` block (~line 234) and in the `allow_tables_to_appear_in_same_query!` list (~line 1059)

**Interfaces:**
- Produces: `schema::device_environments`, `schema::device_env_backfill`.

- [ ] **Step 1: Add the table declarations**

Insert after the `event_user_env_backfill` block:

```rust
// Same diesel fiction as the two blocks above: the real uniqueness on
// `device_environments` is the expression index `device_env_key_idx` over
// `(app_id, device_key, COALESCE(environment_id, nil))`, which `table!` cannot
// express — `environment_id` is nullable because `EnvFilter::Unattributed` is a
// real row. Every query against these two tables is raw `sql_query`, so nothing
// depends on the declaration; do not "fix" it by adding `environment_id` to the
// key, which would be a different constraint than the database enforces.
diesel::table! {
    device_environments (app_id, device_key) {
        app_id -> Uuid,
        device_key -> Text,
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
    device_env_backfill (app_id) {
        app_id -> Uuid,
        completed_at -> Timestamptz,
    }
}
```

- [ ] **Step 2: Add both names to the joinable list**

In the `allow_tables_to_appear_in_same_query!` block, next to `event_user_environments` / `event_user_env_backfill`, add:

```rust
    device_environments,
    device_env_backfill,
```

- [ ] **Step 3: Verify it compiles**

```bash
cd backend && cargo check -p sauron-db
```

Expected: clean. Note `cargo check` never links DuckDB, so this is the cheap gate; the real link happens in Task 9.

---

### Task 3: `DeviceEnvBump` and `bump_device_envs`

**Files:**
- Modify: `backend/crates/sauron-db/src/batch.rs` — add after `bump_person_envs` (~line 520)
- Test: `backend/crates/sauron-db/tests/device_env_rollup.rs`

**Interfaces:**
- Produces:
  ```rust
  #[derive(Debug, Clone)]
  pub struct DeviceEnvBump {
      pub app_id: Uuid,
      pub device_key: String,
      pub environment_id: Option<Uuid>,
      pub first_at: DateTime<Utc>,
      pub last_at: DateTime<Utc>,
      pub events_delta: i64,
      pub errors_delta: i64,
      pub sessions_delta: i64,
  }
  pub async fn bump_device_envs(conn: &mut AsyncPgConnection, rows: &[DeviceEnvBump]) -> QueryResult<usize>;
  ```

- [ ] **Step 1: Write the failing test**

Append to `backend/crates/sauron-db/tests/device_env_rollup.rs`:

```rust
/// Two bumps for the same (app, device, env) must accumulate into one row, not
/// two — and `first_seen`/`last_seen` must widen rather than overwrite. The
/// `ON CONFLICT` names an EXPRESSION (COALESCE(environment_id, nil)); naming
/// the bare column list instead still compiles and still runs, it just stops
/// matching the index and inserts duplicates.
#[tokio::test]
async fn device_env_bumps_accumulate_into_one_row() {
    let Some(db) = TestDb::setup().await else {
        panic!("TEST_DATABASE_URL unset — this test must not silently skip");
    };
    let mut conn = db.conn().await;
    let app_id = db.seed_app().await;
    let t0 = chrono::Utc::now() - chrono::Duration::hours(2);
    let t1 = chrono::Utc::now();

    let bump = |at: chrono::DateTime<chrono::Utc>, ev: i64| sauron_db::batch::DeviceEnvBump {
        app_id,
        device_key: "dev-1".into(),
        environment_id: None,
        first_at: at,
        last_at: at,
        events_delta: ev,
        errors_delta: 0,
        sessions_delta: 0,
    };

    // ASCENDING order is what actually distinguishes LEAST from overwrite, and
    // this was wrong in an earlier draft of this plan. If the later bump lands
    // first, the update carries the SMALLER value, so LEAST(prev,new) == new and
    // a plain overwrite produces byte-identical output — the test passes either
    // way and proves nothing. Insert the EARLIER timestamp first: then the update
    // carries the larger value, LEAST correctly holds first_seen at t0, and an
    // overwrite wrongly drags it forward to t1.
    sauron_db::batch::bump_device_envs(&mut conn, &[bump(t0, 3)])
        .await
        .expect("first bump");
    sauron_db::batch::bump_device_envs(&mut conn, &[bump(t1, 4)])
        .await
        .expect("second bump");

    #[derive(diesel::QueryableByName)]
    struct Row {
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        n: i64,
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        events_count: i64,
        #[diesel(sql_type = diesel::sql_types::Timestamptz)]
        first_seen: chrono::DateTime<chrono::Utc>,
    }
    let r: Row = diesel::sql_query(
        "SELECT count(*) AS n, max(events_count) AS events_count, min(first_seen) AS first_seen \
         FROM device_environments WHERE app_id=$1 AND device_key='dev-1'",
    )
    .bind::<diesel::sql_types::Uuid, _>(app_id)
    .get_result(&mut conn)
    .await
    .expect("read back");

    assert_eq!(r.n, 1, "both bumps must land on one row");
    assert_eq!(r.events_count, 7, "events_count must accumulate");
    assert!(
        (r.first_seen - t0).num_seconds().abs() < 2,
        "first_seen must STAY at the earlier bump; an overwrite would drag it to t1"
    );
}
```

Remove the stray `let _ = last;` and the unused `last` parameter when writing it — the closure only needs `(first, ev)`.

- [ ] **Step 2: Run test to verify it fails**

```bash
cd backend && cargo test -p sauron-db --test device_env_rollup device_env_bumps_accumulate -- --nocapture
```

Expected: FAIL to compile — `bump_device_envs` not found.

- [ ] **Step 3: Implement**

Add to `backend/crates/sauron-db/src/batch.rs`, directly after `bump_person_envs`:

```rust
/// One device/environment pair's folded contribution from a batch.
///
/// The device twin of [`PersonEnvBump`]. `sessions_delta` carries the same
/// insert-only rule: a session is bumped again by every batch that carries a
/// signal for it, so `+1` per bump would count one session once per batch it
/// spans. [`write_rows_once`] credits it from [`bump_sessions`]' inserted-key
/// list, inside the same transaction; every other producer leaves it at `0`.
#[derive(Debug, Clone)]
pub struct DeviceEnvBump {
    pub app_id: Uuid,
    pub device_key: String,
    pub environment_id: Option<Uuid>,
    pub first_at: DateTime<Utc>,
    pub last_at: DateTime<Utc>,
    pub events_delta: i64,
    pub errors_delta: i64,
    pub sessions_delta: i64,
}

/// Fold N device/environment bumps into `device_environments`, one statement.
///
/// Subject to the module's dedupe rule, and — exactly like [`bump_person_envs`]
/// — fed by TWO producers: `Acc::device_env`'s fold and `write_rows_once`'
/// session crediting. A device with both an event and a newly-inserted session
/// in the same batch is one conflict key reached from two directions. Passing
/// both as separate rows raises `ON CONFLICT DO UPDATE command cannot affect
/// row a second time` and fails the whole batch, so the crediting step merges
/// into the existing row by key rather than pushing.
pub async fn bump_device_envs(
    conn: &mut AsyncPgConnection,
    rows: &[DeviceEnvBump],
) -> QueryResult<usize> {
    if rows.is_empty() {
        return Ok(0);
    }
    // Sorted by the conflict key so every concurrent batch takes these row locks
    // in the same order — see the module's ordering rule. This is the fourth
    // row-lock participant in `write_rows_once`; the ingest path has already
    // produced one deadlock (`users_seen` vs. the issue upsert) that stayed
    // invisible because the worker's stdout was being discarded.
    let nil = Uuid::nil();
    let mut ix: Vec<usize> = (0..rows.len()).collect();
    ix.sort_unstable_by(|&a, &b| {
        (
            rows[a].app_id,
            &rows[a].device_key,
            rows[a].environment_id.unwrap_or(nil),
        )
            .cmp(&(
                rows[b].app_id,
                &rows[b].device_key,
                rows[b].environment_id.unwrap_or(nil),
            ))
    });
    diesel::sql_query(
        "INSERT INTO device_environments \
           (app_id, device_key, environment_id, first_seen, last_seen, \
            events_count, errors_count, sessions_count) \
         SELECT app_id, device_key, environment_id, first_at, last_at, \
                events_delta, errors_delta, sessions_delta \
         FROM unnest($1::uuid[], $2::text[], $3::uuid[], $4::timestamptz[], \
                     $5::timestamptz[], $6::bigint[], $7::bigint[], $8::bigint[]) \
              AS t(app_id, device_key, environment_id, first_at, last_at, \
                   events_delta, errors_delta, sessions_delta) \
         ON CONFLICT (app_id, device_key, \
                      COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid)) \
         DO UPDATE SET \
            first_seen = LEAST(device_environments.first_seen, EXCLUDED.first_seen), \
            last_seen = GREATEST(device_environments.last_seen, EXCLUDED.last_seen), \
            events_count = device_environments.events_count + EXCLUDED.events_count, \
            errors_count = device_environments.errors_count + EXCLUDED.errors_count, \
            sessions_count = device_environments.sessions_count + EXCLUDED.sessions_count, \
            updated_at = now()",
    )
    .bind::<Array<SqlUuid>, _>(ix.iter().map(|&i| rows[i].app_id).collect::<Vec<_>>())
    .bind::<Array<Text>, _>(
        ix.iter()
            .map(|&i| rows[i].device_key.clone())
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
    .bind::<Array<BigInt>, _>(
        ix.iter()
            .map(|&i| rows[i].sessions_delta)
            .collect::<Vec<_>>(),
    )
    .execute(conn)
    .await
}
```

- [ ] **Step 4: Run test to verify it passes**

```bash
cd backend && cargo test -p sauron-db --test device_env_rollup -- --nocapture
```

Expected: PASS, both tests.

---

### Task 4: Wire `device_envs` through `WriteSet` and session crediting

**Files:**
- Modify: `backend/crates/sauron-db/src/batch.rs` — `WriteSet` (~line 752), `credit_sessions` (~line 814), `write_rows_once` (~line 875)
- Test: `backend/crates/sauron-db/tests/device_env_rollup.rs`

**Interfaces:**
- Consumes: `DeviceEnvBump`, `bump_device_envs` (Task 3).
- Produces: `WriteSet.device_envs: &'a [DeviceEnvBump]` — every construction site must supply it.

- [ ] **Step 1: Write the failing test**

Append to `backend/crates/sauron-db/tests/device_env_rollup.rs`:

```rust
/// A session is bumped again by every batch carrying a signal for it, so
/// `sessions_count` may only be credited from the keys `bump_sessions` actually
/// INSERTED. Crediting per bump instead counts one session once per batch it
/// spans — an error that grows with session length and that a single-batch test
/// cannot see, which is why this drives the SAME session through two batches.
#[tokio::test]
async fn sessions_count_counts_each_session_once_across_batches() {
    let Some(db) = TestDb::setup().await else {
        panic!("TEST_DATABASE_URL unset — this test must not silently skip");
    };
    let mut conn = db.conn().await;
    let app_id = db.seed_app().await;
    let now = chrono::Utc::now();

    let session = |ev: i64| sauron_db::batch::SessionBump {
        app_id,
        session_id: "s-1".into(),
        distinct_id: Some("p-1".into()),
        device_key: Some("dev-1".into()),
        environment_id: None,
        first_at: now,
        last_at: now,
        events_delta: ev,
        errors_delta: 0,
        release: None,
    };

    for _ in 0..2 {
        let s = [session(1)];
        sauron_db::batch::write_rows(
            &mut conn,
            sauron_db::batch::WriteSet {
                errors: &[],
                analytics: &[],
                transactions: &[],
                sessions: &s,
                devices: &[],
                touch_users: &[],
                identified: &[],
                person_envs: &[],
                device_envs: &[],
            },
        )
        .await
        .expect("write batch");
    }

    #[derive(diesel::QueryableByName)]
    struct N {
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        sessions_count: i64,
    }
    let r: N = diesel::sql_query(
        "SELECT sessions_count FROM device_environments WHERE app_id=$1 AND device_key='dev-1'",
    )
    .bind::<diesel::sql_types::Uuid, _>(app_id)
    .get_result(&mut conn)
    .await
    .expect("rollup row must exist");

    assert_eq!(
        r.sessions_count, 1,
        "one session spanning two batches must count once, not twice"
    );
}
```

Adjust `SessionBump`'s field list to whatever `batch.rs` actually declares — read it at `batch.rs:~200` before writing this, and match exactly.

- [ ] **Step 2: Run test to verify it fails**

```bash
cd backend && cargo test -p sauron-db --test device_env_rollup sessions_count_counts_each -- --nocapture
```

Expected: FAIL to compile — `WriteSet` has no field `device_envs`.

- [ ] **Step 3: Add the `WriteSet` field**

In `batch.rs`, after `person_envs`:

```rust
    /// Per-(device, environment) rollup deltas. `sessions_delta` arrives ZERO
    /// here and is credited inside the transaction from [`bump_sessions`]'
    /// inserted-key list, exactly as `person_envs` is — the caller cannot know
    /// which sessions are new.
    pub device_envs: &'a [DeviceEnvBump],
```

- [ ] **Step 4: Extend session crediting**

`credit_sessions` currently returns `Vec<PersonEnvBump>`. Add a sibling rather than changing its signature — the two keys differ (`distinct_id` vs `device_key`) and a device-keyed session may have no `distinct_id` at all:

```rust
/// Add `sessions_count` credit to the batch's DEVICE rollup rows.
///
/// The device twin of [`credit_sessions`], and separate from it because the two
/// key on different columns and neither implies the other: a session can carry
/// a `device_key` with no `distinct_id` (an anonymous device) or the reverse (a
/// server SDK with no device). Folding both into one function would drop
/// whichever key the session lacks.
///
/// Merges by conflict key rather than pushing, for the same reason
/// [`credit_sessions`] does: a device with both an event and a new session in
/// one batch is one key reached from two producers, and two rows sharing a key
/// abort the whole statement.
fn credit_device_sessions(
    set: &WriteSet<'_>,
    inserted: &HashSet<(Uuid, String)>,
) -> Vec<DeviceEnvBump> {
    let mut rows: Vec<DeviceEnvBump> = set.device_envs.to_vec();
    if inserted.is_empty() {
        return rows;
    }
    let nil = Uuid::nil();
    let mut at: HashMap<(Uuid, String, Uuid), usize> = rows
        .iter()
        .enumerate()
        .map(|(i, d)| {
            (
                (
                    d.app_id,
                    d.device_key.clone(),
                    d.environment_id.unwrap_or(nil),
                ),
                i,
            )
        })
        .collect();
    for s in set.sessions {
        // A session with no device_key has no row in `devices` and could never
        // be joined back to one.
        let Some(dk) = s.device_key.as_deref().filter(|d| !d.is_empty()) else {
            continue;
        };
        if !inserted.contains(&(s.app_id, s.session_id.clone())) {
            continue;
        }
        let key = (s.app_id, dk.to_string(), s.environment_id.unwrap_or(nil));
        match at.get(&key) {
            Some(&i) => rows[i].sessions_delta += 1,
            None => {
                at.insert(key, rows.len());
                rows.push(DeviceEnvBump {
                    app_id: s.app_id,
                    device_key: dk.to_string(),
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
    rows
}
```

`credit_sessions` currently takes `inserted: Vec<(Uuid, String)>` and builds the `HashSet` itself. Change it to take `&HashSet<(Uuid, String)>` so both crediting functions share one set, and build the set once in `write_rows_once`.

- [ ] **Step 5: Call it in `write_rows_once`**

At the site that currently reads `bump_person_envs(conn, &credit_sessions(set, inserted)).await?;` (~line 882), build the set once and call both:

```rust
        // The roll-ups go LAST, and `devices` and the two env rollups last of
        // all — see the module's lock-ordering rule.
        let inserted: HashSet<(Uuid, String)> = inserted.into_iter().collect();
        bump_person_envs(conn, &credit_sessions(set, &inserted)).await?;
        bump_device_envs(conn, &credit_device_sessions(set, &inserted)).await?;
```

- [ ] **Step 6: Fix every `WriteSet` construction site**

```bash
cd backend && cargo check --workspace 2>&1 | grep -A 3 "missing field \`device_envs\`"
```

Add `device_envs: &[]` (or the real slice, in `sauron-pipeline` — Task 5) to each. Expect sites in `crates/sauron-pipeline/src/batch.rs` and its tests.

- [ ] **Step 7: Run test to verify it passes**

```bash
cd backend && cargo test -p sauron-db --test device_env_rollup -- --nocapture
```

Expected: PASS, all three tests.

---

### Task 5: Fold `DeviceEnvBump` rows in the pipeline `Acc`

**Files:**
- Modify: `backend/crates/sauron-pipeline/src/batch.rs` — `Acc` fields (~line 128), the fold in `record` (~line 338), the `WriteSet` construction (~line 610)

**Interfaces:**
- Consumes: `db::DeviceEnvBump` (Task 3), `WriteSet.device_envs` (Task 4).

- [ ] **Step 1: Add the accumulator fields**

Next to `person_envs` / `person_env_at`:

```rust
    device_envs: Vec<db::DeviceEnvBump>,
    /// Index into `device_envs` by conflict key, for the same `ON CONFLICT DO
    /// UPDATE` dedupe reason as `person_env_at`: two rows sharing a key abort
    /// the whole statement.
    device_env_at: HashMap<(Uuid, String, Uuid), usize>,
```

- [ ] **Step 2: Fold the rows**

In `record`, immediately after the person/environment fold block (which ends at ~line 378), add:

```rust
        // The device/environment rollup, folded on the same signal the device
        // bump above uses. Keyed on device_key rather than distinct_id, and
        // therefore a SEPARATE fold rather than a branch of the person one: an
        // anonymous device has a device_key and no distinct_id, and a server
        // SDK has the reverse. Either fold alone would silently drop one of
        // them from its rollup.
        //
        // `sessions_delta` stays 0: only `write_rows_once` can know which
        // sessions this batch newly INSERTED.
        if let Some(dk) = device_key {
            let key = (
                job.app_id,
                dk.to_string(),
                environment_id.unwrap_or_else(Uuid::nil),
            );
            match self.device_env_at.get(&key) {
                Some(&i) => {
                    let b = &mut self.device_envs[i];
                    b.first_at = b.first_at.min(at);
                    b.last_at = b.last_at.max(at);
                    b.events_delta += events_delta;
                    b.errors_delta += errors_delta;
                }
                None => {
                    self.device_env_at.insert(key, self.device_envs.len());
                    self.device_envs.push(db::DeviceEnvBump {
                        app_id: job.app_id,
                        device_key: dk.to_string(),
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
```

The device bump earlier in this function binds the device key as `dk` inside an `if let`; check its exact binding name and scope before writing this, and re-derive `dk` here from the same source rather than reusing a binding that has gone out of scope.

- [ ] **Step 3: Pass it to `WriteSet`**

At the construction site (~line 610), next to `person_envs: &acc.person_envs,`:

```rust
            device_envs: &acc.device_envs,
```

- [ ] **Step 4: Verify it compiles and the pipeline suite passes**

```bash
cd backend && cargo test -p sauron-pipeline -- --nocapture
```

Expected: PASS. The suite already contains a sequential-vs-batched equivalence test (`person_env_aggs` at ~line 1723); if it has a device analogue, both must agree.

- [ ] **Step 5: Extend the sequential/batched equivalence test**

`crates/sauron-pipeline/src/batch.rs` (~line 1593) diffs `person_env_aggs` between a sequentially-written app and a batch-written one. Add the device twin so the new fold is covered by the same guarantee:

```rust
    async fn device_env_aggs(
        conn: &mut db::AsyncPgConnection,
        app_id: Uuid,
    ) -> Vec<(String, Option<Uuid>, i64, i64, i64)> {
        #[derive(diesel::QueryableByName)]
        struct R {
            #[diesel(sql_type = diesel::sql_types::Text)]
            device_key: String,
            #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Uuid>)]
            environment_id: Option<Uuid>,
            #[diesel(sql_type = diesel::sql_types::BigInt)]
            events_count: i64,
            #[diesel(sql_type = diesel::sql_types::BigInt)]
            errors_count: i64,
            #[diesel(sql_type = diesel::sql_types::BigInt)]
            sessions_count: i64,
        }
        let rows: Vec<R> = diesel::sql_query(
            "SELECT device_key, environment_id, events_count, errors_count, sessions_count \
             FROM device_environments WHERE app_id=$1 \
             ORDER BY device_key, environment_id NULLS FIRST",
        )
        .bind::<diesel::sql_types::Uuid, _>(app_id)
        .get_results(conn)
        .await
        .expect("device env aggs");
        rows.into_iter()
            .map(|r| {
                (
                    r.device_key,
                    r.environment_id,
                    r.events_count,
                    r.errors_count,
                    r.sessions_count,
                )
            })
            .collect()
    }
```

Then, next to the existing `assert_eq!(seq_persons, bat_persons)`:

```rust
        let seq_devices = device_env_aggs(&mut conn, a.app_id).await;
        let bat_devices = device_env_aggs(&mut conn, b.app_id).await;
        assert_eq!(
            seq_devices, bat_devices,
            "batched writes must produce the same device rollup as sequential ones"
        );
```

- [ ] **Step 6: Run it**

```bash
cd backend && cargo test -p sauron-pipeline -- --nocapture
```

Expected: PASS.

---

### Task 6: `device_env_backfill` module

**Files:**
- Create: `backend/crates/sauron-db/src/device_env_backfill.rs`
- Modify: `backend/crates/sauron-db/src/lib.rs` — add `pub mod device_env_backfill;`
- Test: `backend/crates/sauron-db/tests/device_env_rollup.rs`

**Interfaces:**
- Produces:
  ```rust
  pub async fn backfill_app(conn: &mut AsyncPgConnection, app_id: Uuid, cutoff: DateTime<Utc>) -> QueryResult<usize>;
  pub async fn is_backfilled(conn: &mut AsyncPgConnection, app_id: Uuid) -> QueryResult<bool>;
  pub async fn backfill_all(pool: &PgPool) -> anyhow::Result<()>;
  ```

- [ ] **Step 1: Write the failing test**

Append to `backend/crates/sauron-db/tests/device_env_rollup.rs`:

```rust
/// The backfill is ADDITIVE against a cutoff, never `ON CONFLICT DO NOTHING`.
/// The write path bumps this table from the moment the migration lands, so a
/// live bump can create a row before the backfill reaches that device; DO
/// NOTHING would then skip it and drop that device's entire history, silently
/// and permanently. Live bumps carry signals at or after the cutoff and the
/// backfill aggregates strictly before it, so the two sets are disjoint and
/// adding them is exact.
#[tokio::test]
async fn backfill_adds_to_a_row_the_write_path_already_created() {
    let Some(db) = TestDb::setup().await else {
        panic!("TEST_DATABASE_URL unset — this test must not silently skip");
    };
    let mut conn = db.conn().await;
    let app_id = db.seed_app().await;
    let cutoff = chrono::Utc::now();

    // A live bump lands first, as it would on a running deployment.
    sauron_db::batch::bump_device_envs(
        &mut conn,
        &[sauron_db::batch::DeviceEnvBump {
            app_id,
            device_key: "dev-1".into(),
            environment_id: None,
            first_at: cutoff,
            last_at: cutoff,
            events_delta: 5,
            errors_delta: 0,
            sessions_delta: 0,
        }],
    )
    .await
    .expect("live bump");

    // Two historical analytics rows, strictly before the cutoff.
    for _ in 0..2 {
        diesel::sql_query(
            "INSERT INTO analytics_events \
               (id, app_id, environment_id, name, distinct_id, properties, context, \
                occurred_at, received_at, device_key, tags, contexts, extra) \
             VALUES (gen_random_uuid(), $1, NULL, 'evt', 'p-1', '{}', '{}', \
                     $2 - interval '1 hour', now(), 'dev-1', '{}', '{}', '{}')",
        )
        .bind::<diesel::sql_types::Uuid, _>(app_id)
        .bind::<diesel::sql_types::Timestamptz, _>(cutoff)
        .execute(&mut conn)
        .await
        .expect("seed historical event");
    }

    sauron_db::device_env_backfill::backfill_app(&mut conn, app_id, cutoff)
        .await
        .expect("backfill");

    #[derive(diesel::QueryableByName)]
    struct N {
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        events_count: i64,
    }
    let r: N = diesel::sql_query(
        "SELECT events_count FROM device_environments WHERE app_id=$1 AND device_key='dev-1'",
    )
    .bind::<diesel::sql_types::Uuid, _>(app_id)
    .get_result(&mut conn)
    .await
    .expect("read back");

    assert_eq!(
        r.events_count, 7,
        "backfill must ADD its 2 historical events to the 5 the write path already recorded"
    );
    assert!(
        sauron_db::device_env_backfill::is_backfilled(&mut conn, app_id)
            .await
            .expect("marker"),
        "the marker must be set in the same transaction as the aggregate"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cd backend && cargo test -p sauron-db --test device_env_rollup backfill_adds_to_a_row -- --nocapture
```

Expected: FAIL to compile — module `device_env_backfill` not found.

- [ ] **Step 3: Create the module**

`backend/crates/sauron-db/src/device_env_backfill.rs`:

```rust
//! Populate `device_environments` for data that predates the rollup.
//!
//! The device twin of [`crate::person_env_backfill`], and identical in shape —
//! read that module's header for the full reasoning. The short version:
//!
//! Not part of a migration, and not part of `sauron-migrate`'s default no-arg
//! path, both on purpose: `require_current_schema` fail-closes the API on a
//! stale schema, and every RPM daemon `Requires=` the migrator unit, so anything
//! slow in either place is a boot outage proportional to retained data.
//!
//! ## Additive against a cutoff, NOT `ON CONFLICT DO NOTHING`
//!
//! The write path bumps this table from the moment migration 59 lands,
//! including for apps that are not yet backfilled, so a live bump can create a
//! row before the backfill reaches that device. `DO NOTHING` would then skip it
//! and leave that device short by its entire history — silently, and
//! permanently. Instead this aggregates only rows strictly before `cutoff` and
//! ADDS them to whatever is there; live bumps carry signals at or after
//! `cutoff`, so the two sets are disjoint and the addition is exact.
//!
//! KNOWN RESIDUAL: a backdated event — an SDK offline queue replaying with an
//! old `occurred_at` — that arrives between `cutoff` and the backfill finishing
//! is counted twice. Bounded by the backfill's duration, and counter drift is
//! already an accepted property of this table (the same trade `devices` makes).

use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel::sql_types::{Timestamptz, Uuid as SqlUuid};
use diesel_async::{AsyncPgConnection, RunQueryDsl, SimpleAsyncConnection};
use uuid::Uuid;

use crate::PgPool;

/// Aggregate one app's pre-`cutoff` history into the rollup and mark it done.
///
/// The marker insert shares this function's transaction with the aggregate, so
/// the marker can never become visible before the data it claims. That ordering
/// is the only thing standing between this design and a silently empty Devices
/// page, so it is a transaction rather than two statements.
pub async fn backfill_app(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    cutoff: DateTime<Utc>,
) -> QueryResult<usize> {
    // Explicit BEGIN/COMMIT rather than `conn.transaction(|c| …)`: diesel-async
    // 0.9's closure signature needs async closures, which would push the
    // workspace MSRV past the 1.82 the RPM spec builds against. Same reasoning
    // as `batch::write_rows_once`.
    conn.batch_execute("BEGIN").await?;
    match backfill_app_inner(conn, app_id, cutoff).await {
        Ok(n) => {
            conn.batch_execute("COMMIT").await?;
            Ok(n)
        }
        Err(e) => {
            // Best-effort: if the ROLLBACK itself fails the connection is
            // already unusable and the pool discards it on return, which aborts
            // the transaction anyway.
            let _ = conn.batch_execute("ROLLBACK").await;
            Err(e)
        }
    }
}

async fn backfill_app_inner(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    cutoff: DateTime<Utc>,
) -> QueryResult<usize> {
    // One UNION ALL over the three signal tables, grouped once. The three legs
    // mirror `repo::device_membership_sql`' three legs exactly — any device with
    // a row in ANY of them qualifies — so the rollup admits precisely the
    // devices the live query admits. Drop a leg here and a device whose only
    // signal is a session (or an error) silently disappears from the Devices
    // inventory the moment its app is marked backfilled.
    //
    // NOTE the deliberate difference from `device_membership_sql`: that
    // predicate bounds its sessions leg by the page's `since`, because it is
    // deciding which devices to LIST in a window. This is deciding what the
    // rollup CONTAINS, which has no window — the `since` filter still applies
    // later, against `devices.last_seen`, exactly as it does today.
    let n = diesel::sql_query(
        "INSERT INTO device_environments \
           (app_id, device_key, environment_id, first_seen, last_seen, \
            events_count, errors_count, sessions_count) \
         SELECT app_id, device_key, environment_id, \
                min(first_at), max(last_at), sum(ev), sum(er), sum(se) \
         FROM ( \
             SELECT app_id, device_key, environment_id, occurred_at AS first_at, \
                    occurred_at AS last_at, 1::bigint AS ev, 0::bigint AS er, \
                    0::bigint AS se \
             FROM analytics_events \
             WHERE app_id=$1 AND occurred_at < $2 \
               AND device_key IS NOT NULL AND device_key <> '' \
             UNION ALL \
             SELECT app_id, device_key, environment_id, occurred_at, occurred_at, \
                    0::bigint, 1::bigint, 0::bigint \
             FROM error_events \
             WHERE app_id=$1 AND occurred_at < $2 \
               AND device_key IS NOT NULL AND device_key <> '' \
             UNION ALL \
             SELECT app_id, device_key, environment_id, started_at, last_event_at, \
                    0::bigint, 0::bigint, 1::bigint \
             FROM sessions \
             WHERE app_id=$1 AND started_at < $2 \
               AND device_key IS NOT NULL AND device_key <> '' \
         ) t \
         GROUP BY app_id, device_key, environment_id \
         ON CONFLICT (app_id, device_key, \
                      COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid)) \
         DO UPDATE SET \
            first_seen = LEAST(device_environments.first_seen, EXCLUDED.first_seen), \
            last_seen = GREATEST(device_environments.last_seen, EXCLUDED.last_seen), \
            events_count = device_environments.events_count + EXCLUDED.events_count, \
            errors_count = device_environments.errors_count + EXCLUDED.errors_count, \
            sessions_count = device_environments.sessions_count + EXCLUDED.sessions_count, \
            updated_at = now()",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Timestamptz, _>(cutoff)
    .execute(conn)
    .await?;

    diesel::sql_query(
        "INSERT INTO device_env_backfill (app_id, completed_at) VALUES ($1, now()) \
         ON CONFLICT (app_id) DO UPDATE SET completed_at = now()",
    )
    .bind::<SqlUuid, _>(app_id)
    .execute(conn)
    .await?;

    Ok(n)
}

/// Whether `repo::list_device_groups` may read the rollup for this app.
pub async fn is_backfilled(conn: &mut AsyncPgConnection, app_id: Uuid) -> QueryResult<bool> {
    #[derive(QueryableByName)]
    struct Present {
        #[diesel(sql_type = diesel::sql_types::Bool)]
        present: bool,
    }
    let r: Present = diesel::sql_query(
        "SELECT EXISTS (SELECT 1 FROM device_env_backfill WHERE app_id=$1) AS present",
    )
    .bind::<SqlUuid, _>(app_id)
    .get_result(conn)
    .await?;
    Ok(r.present)
}

/// Backfill every app that has no marker yet, one app per transaction.
///
/// One app at a time rather than one statement for everything: a single
/// transaction over every app's history would hold locks for its whole duration
/// and lose all progress on any failure.
pub async fn backfill_all(pool: &PgPool) -> anyhow::Result<()> {
    let mut conn = crate::conn(pool).await?;

    #[derive(QueryableByName)]
    struct AppId {
        #[diesel(sql_type = SqlUuid)]
        id: Uuid,
    }
    let apps: Vec<AppId> = diesel::sql_query(
        "SELECT id FROM apps WHERE id NOT IN (SELECT app_id FROM device_env_backfill) \
         ORDER BY id",
    )
    .get_results(&mut conn)
    .await?;

    tracing::info!(apps = apps.len(), "device/environment backfill starting");
    for a in apps {
        // One cutoff per app, taken immediately before that app's aggregate, so
        // the disjointness argument holds per app rather than depending on how
        // long the earlier apps took.
        let cutoff = Utc::now();
        let n = backfill_app(&mut conn, a.id, cutoff).await?;
        tracing::info!(app_id = %a.id, rows = n, "device/environment backfill done");
    }
    tracing::info!("device/environment backfill complete");
    Ok(())
}
```

- [ ] **Step 4: Export the module**

In `backend/crates/sauron-db/src/lib.rs`, next to `pub mod person_env_backfill;`:

```rust
pub mod device_env_backfill;
```

- [ ] **Step 5: Run test to verify it passes**

```bash
cd backend && cargo test -p sauron-db --test device_env_rollup -- --nocapture
```

Expected: PASS, all four tests.

---

### Task 7: Read path — rollup shape, fallback, and the `sessions_count` carve-out

**Files:**
- Modify: `backend/crates/sauron-db/src/repo.rs` — `list_device_groups` (~7151–7259)
- Test: `backend/crates/sauron-db/tests/device_env_rollup.rs`

**Interfaces:**
- Consumes: `device_env_backfill::is_backfilled` (Task 6).
- Produces:
  ```rust
  pub fn list_device_groups_sql_for_test(env: EnvFilter) -> String;         // live shape
  pub fn list_device_groups_rollup_sql_for_test(env: EnvFilter) -> String;  // rollup shape
  ```

- [ ] **Step 1: Write the failing test**

Append to `backend/crates/sauron-db/tests/device_env_rollup.rs`:

```rust
use sauron_db::scope::EnvFilter;
use uuid::Uuid;

/// The rollup shape must NOT source `sessions_count` from the rollup.
///
/// `list_device_groups` windows that one count (`count(*) FILTER (WHERE
/// started_at >= $2)`) while `events_count`/`errors_count` are lifetime, so a
/// lifetime rollup column changes the displayed number. Measured: reading it
/// from the rollup was 36ms instead of 105ms and differed on 40 of 40 rows.
/// This is a SHAPE assertion because both variants return plausible numbers —
/// which is exactly why the fast-and-wrong one is easy to ship.
#[test]
fn rollup_shape_keeps_sessions_count_live() {
    let sql = sauron_db::repo::list_device_groups_rollup_sql_for_test(EnvFilter::One(Uuid::nil()));
    assert!(
        sql.contains("count(*) FILTER (WHERE started_at >="),
        "sessions_count must stay a windowed live aggregate, got:\n{sql}"
    );
    assert!(
        !sql.contains("sum(de.sessions_count)"),
        "sessions_count must NOT be summed from the rollup, got:\n{sql}"
    );
    assert!(
        !sql.contains("LEFT JOIN LATERAL ( SELECT count(*) AS cnt, min(occurred_at)"),
        "the analytics/error LATERALs must be gone, got:\n{sql}"
    );
}

/// Under `All` the rollup shape must keep reading the durable `devices`
/// counters, exactly as the live shape does. Deriving them from the rollup
/// would silently change what an unscoped page displays on the day an operator
/// runs the backfill — a number moving with no code deploy behind it.
#[test]
fn rollup_shape_under_all_reads_durable_device_columns() {
    let sql = sauron_db::repo::list_device_groups_rollup_sql_for_test(EnvFilter::All);
    assert!(
        sql.contains("sum(d.events_count)"),
        "All must read devices.events_count, got:\n{sql}"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cd backend && cargo test -p sauron-db --test device_env_rollup rollup_shape -- --nocapture
```

Expected: FAIL to compile — `list_device_groups_rollup_sql_for_test` not found.

- [ ] **Step 3: Extract the live shape into a named function**

In `repo.rs`, move the existing query construction out of `list_device_groups` into:

```rust
/// The pre-rollup shape: membership derived per device by three `EXISTS`, counts
/// and extrema by three LATERALs, all before `GROUP BY`. Read for apps whose
/// `device_environments` backfill has not completed — see [`list_device_groups`].
fn list_device_groups_live_sql(env: &EnvFilter, sort: &SortSpec) -> String {
    // body moved verbatim from list_device_groups, unchanged
}
```

Keep the existing block comments with it — they document measured plans and must not be orphaned.

Only `env` and `sort` are parameters: the body's other input, `search`, reaches the SQL solely as the `$3` bind (`pattern`), never as interpolated text, so the emitted string does not vary with it. Leave `pattern` construction in `list_device_groups` itself.

- [ ] **Step 4: Add the rollup shape**

```rust
/// The rollup shape, read for apps whose `device_environments` backfill has
/// completed.
///
/// `device_environments` carries one row per (device, environment) with the
/// counts and both timestamps already computed, so the two count LATERALs and
/// the three membership `EXISTS` all collapse into one join. Measured on a
/// 13,333-qualifying-device fixture: 4,639ms -> 105ms, zero row differences in
/// either direction.
///
/// THREE things must not drift from [`list_device_groups_live_sql`]:
///
/// 1. **`sessions_count` stays live.** It is the one count this endpoint
///    WINDOWS (`count(*) FILTER (WHERE started_at >= $2)`) while `events_count`
///    and `errors_count` are lifetime — an inconsistency that predates this
///    work and is preserved deliberately rather than quietly fixed. Sourcing it
///    from the rollup measured 36ms instead of 105ms and changed the number on
///    40 of 40 rows. `sessions` is not partitioned, so the surviving LATERAL is
///    one index probe per device against the old 45.
/// 2. **`first_seen`/`last_seen` keep the `All`-vs-scoped split.** Under `All`
///    they read the durable `devices` columns exactly as the live shape does.
///    Deriving them from the rollup under `All` too would be defensible in
///    isolation, but it would silently change what an unscoped page displays on
///    the day an operator runs the backfill — a number moving with no code
///    deploy behind it.
/// 3. **The output aliases keep these exact names** — `device_count`,
///    `events_count`, `errors_count`, `sessions_count`, `first_seen`,
///    `last_seen` — because `routes::devices::group_sort_spec` emits them
///    unqualified and Postgres resolves a bare ORDER BY name against the select
///    list. Rename one and the sort silently falls back to a different column.
///
/// Membership is now the join itself: a device with no row for this environment
/// does not join. That is very slightly WIDER than the live predicate, whose
/// sessions leg is bounded by `since` — a device whose only environment signal
/// is a session older than the window would newly appear. Not observed on the
/// measurement fixture (every device there had all three signal kinds); called
/// out because it is a real difference, not a proven-absent one.
fn list_device_groups_rollup_sql(env: &EnvFilter, sort: &SortSpec) -> String {
    let order_by = sort.order_by();
    let env_sql = env.sql_fragment_for("de", 6);
    // Under `All` the join must not multiply a device by its environments, so
    // the rollup is pre-aggregated per device before it reaches the group.
    // Under `One`/`Unattributed` the filter admits a single row per device and
    // the grouping is a no-op — one shape, not four.
    let (scoped_select, scoped_join) = if matches!(env, EnvFilter::All) {
        (
            "sum(d.events_count)::bigint AS events_count, \
             sum(d.errors_count)::bigint AS errors_count, \
             min(d.first_seen) AS first_seen, \
             max(d.last_seen) AS last_seen"
                .to_string(),
            String::new(),
        )
    } else {
        (
            "COALESCE(sum(de.events_count), 0)::bigint AS events_count, \
             COALESCE(sum(de.errors_count), 0)::bigint AS errors_count, \
             min(de.first_seen) AS first_seen, \
             max(de.last_seen) AS last_seen"
                .to_string(),
            format!(
                " JOIN device_environments de \
                    ON de.app_id = d.app_id AND de.device_key = d.device_key{env_sql}"
            ),
        )
    };
    format!(
        "SELECT d.family, d.model, d.os_name, d.os_version, \
                count(*)::bigint AS device_count, \
                {scoped_select}, \
                COALESCE(sum(se.cnt), 0)::bigint AS sessions_count \
         FROM ( \
             SELECT * FROM devices \
             WHERE app_id = $1 AND last_seen >= $2 \
               AND (COALESCE(family,'') || ' ' || COALESCE(model,'') || ' ' || \
                    COALESCE(os_name,'') || ' ' || COALESCE(device_key,'')) ILIKE $3 \
         ) d{scoped_join} \
         LEFT JOIN LATERAL ( \
             SELECT count(*) FILTER (WHERE started_at >= $2) AS cnt \
             FROM sessions \
             WHERE app_id = $1 AND device_key = d.device_key{se_env} \
         ) se ON TRUE \
         GROUP BY d.family, d.model, d.os_name, d.os_version \
         ORDER BY {order_by} \
         LIMIT $4 OFFSET $5",
        se_env = env.sql_fragment(6),
    )
}
```

Note both `de` and `se` reference bind `$6`; that is correct — `bind_env!` binds it once and Postgres allows a parameter to appear any number of times.

- [ ] **Step 5: Add the test accessors**

```rust
/// The exact SQL `list_device_groups` executes, exposed so tests can assert on
/// the emitted shape. Two shapes now exist and the only thing separating
/// "correct but O(devices)" from "correct and bounded" is which one is emitted;
/// a behavioural test cannot tell them apart because they return identical rows.
pub fn list_device_groups_sql_for_test(env: EnvFilter) -> String {
    list_device_groups_live_sql(&env, &group_sort_for_test())
}

/// Companion to [`list_device_groups_sql_for_test`] for the rollup shape.
pub fn list_device_groups_rollup_sql_for_test(env: EnvFilter) -> String {
    list_device_groups_rollup_sql(&env, &group_sort_for_test())
}

/// The default group sort — `last_seen DESC` with the four `GROUP BY` columns as
/// tiebreak — matching what `routes::devices::group_sort_spec` builds for an
/// absent `sort` parameter.
fn group_sort_for_test() -> SortSpec {
    SortSpec {
        column: "last_seen",
        descending: true,
        tiebreak: "d.family, d.model, d.os_name, d.os_version",
        nulls_last: false,
    }
}
```

Match `SortSpec`'s real field types — `column` may be `String` rather than `&'static str`. Read the struct before writing this.

- [ ] **Step 6: Switch on the marker in `list_device_groups`**

Replace the body's query construction with:

```rust
    // Two shapes until every deployment is backfilled. The marker is per-app and
    // is written in the same transaction as that app's backfill aggregate, so it
    // can never be visible before the data it claims — a marker that ran ahead of
    // its data would make this page quiet-wrong rather than error.
    let q = if crate::device_env_backfill::is_backfilled(conn, scope.app_id).await? {
        list_device_groups_rollup_sql(&scope.env, &sort)
    } else {
        list_device_groups_live_sql(&scope.env, &sort)
    };
```

The bind list below is unchanged — both shapes use `$1` app_id, `$2` since, `$3` pattern, `$4` limit, `$5` offset, `$6` env.

- [ ] **Step 7: Update the now-stale doc comment**

`list_device_groups`' doc comment says "the count LATERALs run for every qualifying device in the window, not just the 50 on screen … this is the accepted price of paging over groups". That is no longer true for backfilled apps. Rewrite that paragraph to describe both shapes and point at the rollup; leave the paragraphs on NULL grouping and the `All`-vs-scoped split, which are still accurate.

Also update the block comment at `repo.rs:~7037` — "If this becomes a measured problem in production the answer is a materialized per-(device, environment) rollup" — to record that it DID become a measured problem and the rollup now exists, mirroring how `list_persons` (`repo.rs:~7624`) handles the same sentence.

- [ ] **Step 8: Run the shape tests**

```bash
cd backend && cargo test -p sauron-db --test device_env_rollup -- --nocapture
```

Expected: PASS.

- [ ] **Step 9: Write the equivalence test — the one that matters most**

Append to `backend/crates/sauron-db/tests/device_env_rollup.rs`:

```rust
/// The rollup must return byte-identical rows to the live query. This is the
/// test the whole plan rests on: both shapes return plausible numbers, so
/// nothing else in the suite can tell a correct rollup from a subtly wrong one.
///
/// Seeds every combination that has bitten this table's persons twin: a device
/// with events but no sessions, one with sessions but no events, one with only
/// errors, one in two environments, and one with a NULL environment.
#[tokio::test]
async fn rollup_and_live_shapes_return_identical_rows() {
    let Some(db) = TestDb::setup().await else {
        panic!("TEST_DATABASE_URL unset — this test must not silently skip");
    };
    let mut conn = db.conn().await;
    let seeded = db.seed_two_envs().await;

    // Drive real ingest-shaped writes through the batch path so the rollup is
    // populated the way production populates it, rather than by hand.
    common::seed_mixed_device_activity(&mut conn, &seeded).await;

    let scope = sauron_db::scope::ReadScope {
        app_id: seeded.app_id,
        env: EnvFilter::One(seeded.env_a),
    };
    let since = chrono::Utc::now() - chrono::Duration::days(30);

    // `SortSpec` is deliberately NOT `Clone` (its fields are `&'static str` so
    // no caller string can ever be interpolated), so build it once per call
    // rather than cloning. This is exactly what `group_sort_spec(None)` yields.
    let sort = || sauron_db::repo::SortSpec {
        column: "last_seen",
        descending: true,
        tiebreak: "d.family, d.model, d.os_name, d.os_version",
        nulls_last: false,
    };

    // Live shape (no marker yet).
    let live = sauron_db::repo::list_device_groups(
        &mut conn, scope.clone(), since, 51, 0, sort(), None,
    )
    .await
    .expect("live");

    // Mark it backfilled and read again — same connection, same data.
    sauron_db::device_env_backfill::backfill_app(&mut conn, seeded.app_id, chrono::Utc::now())
        .await
        .expect("backfill");
    let rollup = sauron_db::repo::list_device_groups(
        &mut conn, scope, since, 51, 0, sort(), None,
    )
    .await
    .expect("rollup");

    assert_eq!(live.len(), rollup.len(), "group count must match");
    for (l, r) in live.iter().zip(rollup.iter()) {
        assert_eq!((&l.family, &l.model, &l.os_name, &l.os_version),
                   (&r.family, &r.model, &r.os_name, &r.os_version), "group key");
        assert_eq!(l.device_count, r.device_count, "device_count for {:?}", l.family);
        assert_eq!(l.events_count, r.events_count, "events_count for {:?}", l.family);
        assert_eq!(l.errors_count, r.errors_count, "errors_count for {:?}", l.family);
        assert_eq!(l.sessions_count, r.sessions_count, "sessions_count for {:?}", l.family);
        assert_eq!(l.first_seen, r.first_seen, "first_seen for {:?}", l.family);
        assert_eq!(l.last_seen, r.last_seen, "last_seen for {:?}", l.family);
    }
}
```

Write `common::seed_mixed_device_activity` in `tests/common/mod.rs`. It must insert, for one app and two environments: a device with 3 analytics events and no session; a device with 2 sessions and no analytics event; a device with only an error event; a device with activity in BOTH environments; and a device with `environment_id IS NULL`. Backfill (not the write path) is what populates the rollup here, so plain `INSERT`s into the three signal tables are the right seeding mechanism — mirror the SQL in Task 6's test.

`ReadScope` IS `Clone`; `SortSpec` is not — hence the closure above rather than `.clone()`.

- [ ] **Step 10: Run it**

```bash
cd backend && cargo test -p sauron-db --test device_env_rollup rollup_and_live -- --nocapture
```

Expected: PASS. **If `sessions_count` mismatches, do not "fix" it by changing the assertion** — it means the rollup column leaked into the read path; re-check Task 7 Step 4.

---

### Task 8: `sauron-migrate backfill-device-envs`

**Files:**
- Modify: `backend/bins/sauron-migrate/src/main.rs` (~line 50)

**Interfaces:**
- Consumes: `device_env_backfill::backfill_all` (Task 6).

- [ ] **Step 1: Add the subcommand**

Directly after the existing `backfill-person-envs` block:

```rust
    // Opt-in for exactly the same reason as `backfill-person-envs` above: this
    // binary is the `sauron-migrate.service` oneshot that every RPM daemon pulls
    // in via `Requires=`, systemd never retries a failed start job, and this
    // aggregates all 29 partitions of the two largest tables.
    //
    // Until it has run for an app, `repo::list_device_groups` reads that app
    // through the pre-rollup query, so skipping this is a performance decision
    // and never a correctness one.
    if std::env::args().any(|a| a == "backfill-device-envs") {
        let pool = sauron_db::build_pool(&url, 4)?;
        sauron_db::device_env_backfill::backfill_all(&pool).await?;
    }
```

If the `backfill-person-envs` arm binds `pool` in an outer scope, reuse that binding rather than building a second pool.

- [ ] **Step 2: Verify it builds and runs as a no-op without the flag**

```bash
cd backend && DATABASE_URL="$TEST_DATABASE_URL" cargo run -p sauron-migrate
```

Expected: exits 0, logs `migrations up to date`, and does **not** log `device/environment backfill starting`.

- [ ] **Step 3: Verify the flag works**

```bash
cd backend && DATABASE_URL="$TEST_DATABASE_URL" cargo run -p sauron-migrate -- backfill-device-envs
```

Expected: logs `device/environment backfill starting` … `complete`.

---

### Task 9: Full verification

**Files:** none — this task only runs gates.

- [ ] **Step 1: Start host-network Postgres and Redis**

The Bash sandbox has its own network namespace, so DB-backed tests connect to nothing and **return early while printing `ok`**. A green run proves nothing unless these two conditions hold: containers on host networking, and `dangerouslyDisableSandbox` on the test command.

```bash
docker run -d --name sauron-test-pg --network host -e POSTGRES_PASSWORD=sauron -e POSTGRES_USER=sauron -e POSTGRES_DB=sauron postgres:16 -c max_connections=800
docker run -d --name sauron-test-redis --network host redis:7 --save '' --appendonly no
```

The Redis flags are not optional: without them a failed snapshot turns into a write outage that looks like the workload failing. Check free disk before starting — a full root fs has previously crash-looped an unrelated container.

- [ ] **Step 2: Run the workspace suite**

```bash
cd backend && TEST_DATABASE_URL=postgres://sauron:sauron@localhost:5432/sauron TEST_REDIS_URL=redis://localhost:6379 cargo test --workspace 2>&1 | tail -40
```

Expected: PASS. Baseline before this work is ~1391 real passes; **grep the output for `skipping` and treat any occurrence as a failure** — that is the silent-skip mode, not a pass.

- [ ] **Step 3: Confirm the DB-backed tests actually ran**

```bash
cd backend && TEST_DATABASE_URL=... cargo test -p sauron-db --test device_env_rollup -- --nocapture 2>&1 | grep -c "test result: ok"
```

Expected: non-zero, and the run must not print `skipping`.

- [ ] **Step 4: Lint and format**

```bash
cd backend && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all -- --check
```

Note `cargo fmt --all --check` (without the `--`) is the wrong invocation: it prints help and exits 0, so it passes while checking nothing.

- [ ] **Step 5: Re-measure against the plan's baseline**

Rebuild the measurement fixture (40,000 devices / 13,333 qualifying / 1.68M events / 15 partitions), run the backfill, and `EXPLAIN (ANALYZE)` the emitted rollup SQL.

Expected: ~105 ms against the 4,639 ms baseline, and — the check that actually matters — the plan must show **one `Hash Join` and no `Nested Loop Left Join` at `loops=13333`**. A cost reduction that leaves the plan shape intact does not fix this; check the join count and `Seq Scan` count, not just the cost number.

- [ ] **Step 6: Drive the endpoint end to end**

Start the API against the fixture DB and request the user's exact URL shape:

```bash
curl -s -o /dev/null -w '%{http_code} %{time_total}s\n' -H "Authorization: Bearer $TOKEN" "http://localhost:8080/v1/apps/$APP/device-groups?since_days=30&sort=last_seen&limit=51&offset=0&environment_id=$ENV"
```

Expected: `200` well under a second. Then request it again with `environment_id` omitted and confirm the `All` path still returns the same numbers it did before this work.

---

## Out of scope, deliberately

- **`list_devices` (the `/devices` drill-down) is NOT converted.** It pays the same per-device LATERAL cost under a scoped read and would be the natural next slice, but it is a different endpoint with a different row shape (`DeviceRow`, plus `device_last_distinct_id_join`'s three-way `UNION ALL … LIMIT 1`, which no rollup column can serve). Converting it means re-deciding what `last_distinct_id` means per environment. Left as a follow-up so this plan stays one endpoint wide.
- **The purge does not clean `device_environments`.** Neither does it clean `event_user_environments` or `identities` — `sauron-purge/src/lib.rs:55` claims the Persons kind covers them, but `purge.rs` contains no reference to either. That is a pre-existing gap in the (uncommitted) purge work, not something this plan introduces; app deletion still cascades correctly via the FK. Flagged separately rather than fixed here.

## Deploy note

Migration 59 creates a table and four indexes — no partitioned-parent index build, so unlike migrations 47/53/55 this one does **not** need a maintenance window. Once a binary embedding it ships, `require_current_schema` refuses to boot until it is applied. The backfill is a separate, operator-timed command; until it runs, every app reads the existing live query and nothing changes.
