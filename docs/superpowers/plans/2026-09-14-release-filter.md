# Release Filter Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Promote the existing `release` field to a global dashboard dimension (org → project → app → release → env) and make it mandatory at SDK init, without re-keying any rollup.

**Architecture:** A small observed table `app_releases` is upserted by the pipeline worker and seeded by an operator backfill. The API accepts `?release=` on the five searched list routes by ANDing a synthetic `release` predicate into the already-resolved query node, and a middleware rejects `release=` everywhere else. The dashboard stores the pick beside the environment, attaches it through the same axios scope interceptor, and shows a "Showing all releases" note on rollup-backed pages. Each SDK throws at init without a release.

**Tech Stack:** Rust (axum, diesel-async, utoipa), Postgres 16, Svelte 5 + TypeScript + vitest, five SDKs (TS browser, TS node, Python, Dart, C#).

**Spec:** `docs/superpowers/specs/2026-09-14-release-filter-design.md`

## Global Constraints

- **Never commit and never create branches.** This checkout is shared with another live session. Never `git stash`. Leave the work in the tree.
- Wire name stays `release`. Literal `none` means `release IS NULL`.
- `environment_id` in `app_releases` is the enrollment id (`app_environments.id`), never `environments.id`.
- Next migration number is `000078`; directory name `backend/migrations/2026-09-14-000078_app_releases`.
- `schema.rs` is hand-maintained; edit it by hand, do not run `diesel print-schema` into it.
- Clippy: run `cargo +1.98.0 clippy --all-targets -- -D warnings` from `backend/` after touching Rust sources. Local 1.94 is weaker than CI.
- DB tests need `TEST_DATABASE_URL` (a maintenance connection, e.g. `postgres://…/sauron`) and API tests also need `TEST_REDIS_URL`. Without them the test prints `ok` having run nothing. Run them from an ordinary shell, outside the sandbox netns, and check the duration is non-zero.
- Dashboard: every user-visible string goes through `t('key')` with both `en` and `ar` in `dashboard/src/lib/i18n/catalog/*.ts`; the leak test scans all `.svelte` files.
- Dashboard tests: `cd dashboard && npx vitest run <file>`. Type check: `npx svelte-check`.
- SDK version bumps: JS 1.6.0→1.7.0, Node 1.5.0→1.6.0, Python 1.5.0→1.6.0, Flutter 1.9.0→1.10.0, C# 1.5.0→1.6.0. Each bump touches manifest, in-code constant, the SDK's own version-assertion tests, `wiki/<X>-SDK.md`, `wiki/Capabilities.md`, the SDK `CHANGELOG.md`, and `sdks/PUBLISHING.md`.

---

## File map

Backend
- Create `backend/migrations/2026-09-14-000078_app_releases/{up.sql,down.sql}` — table, unique expression index, two list indexes.
- Modify `backend/crates/sauron-db/src/schema.rs` — `app_releases` table macro.
- Create `backend/crates/sauron-db/src/releases.rs` — `upsert_seen`, `list_for_app`, `backfill_all`, `AppReleaseRow`.
- Modify `backend/crates/sauron-db/src/lib.rs` — `pub mod releases;`.
- Create `backend/crates/sauron-db/tests/app_releases.rs` — DB tests.
- Create `backend/crates/sauron-pipeline/src/releases.rs` — throttled `note_release`.
- Modify `backend/crates/sauron-pipeline/src/lib.rs`, `process.rs` — call it.
- Modify `backend/bins/sauron-migrate/src/main.rs` — `backfill-releases`.
- Modify `backend/crates/sauron-query/src/catalog.rs` — Issues `release` becomes a column store; Transactions joins the release dimension.
- Create `backend/crates/sauron-query/src/release_scope.rs` — `ReleaseFilter`, `with_release`.
- Modify `backend/crates/sauron-query/src/lib.rs` — export.
- Modify `backend/crates/sauron-db/src/query_plan/{issues,transactions}.rs` — lowering arms.
- Modify `backend/bins/sauron-api/src/routes/scope.rs` — `raw_release`, `parse_release`.
- Create `backend/bins/sauron-api/src/release_guard.rs` — middleware + `RELEASE_ACCEPTING_PATHS`.
- Modify `backend/bins/sauron-api/src/routes/{issues,analytics,sessions,transactions}.rs` — thread the filter.
- Create `backend/bins/sauron-api/src/routes/releases.rs` — `GET /v1/apps/{app_id}/releases`.
- Modify `backend/bins/sauron-api/src/main.rs`, `openapi.rs` — register.
- Create `backend/bins/sauron-api/tests/http_release_scoping.rs` — route tests.

Dashboard
- Create `dashboard/src/lib/api/releases.ts`; modify `dashboard/src/lib/models/index.ts`.
- Modify `dashboard/src/lib/stores/session.svelte.ts`, `dashboard/src/lib/api/scope.ts`, `dashboard/src/lib/api/client.ts`, and their tests.
- Modify `dashboard/src/lib/components/layout/Topbar.svelte`, `dashboard/src/lib/i18n/catalog/nav.ts`, `ui.ts`.
- Modify `dashboard/src/lib/models/shell.ts` + `shell.test.ts` — `RELEASE_AWARE`.
- Create `dashboard/src/lib/components/layout/ReleaseScopeNote.svelte`; modify `AppShell.svelte`.
- Create `dashboard/src/lib/api/release-scope-parity.test.ts`.
- Modify `dashboard/src/lib/components/filters/filters.ts` — release chip on Issues and Transactions.

SDKs and docs
- `sdks/js`, `sdks/node`, `sdks/python`, `sdks/flutter`, `sdks/csharp` — validation, tests, versions, READMEs, CHANGELOGs.
- `sdks/PUBLISHING.md`, `wiki/*.md`, `docs/runbooks/` (rollup runbook) — backfill entry.

---

### Task 1: Migration 78 and schema

**Files:**
- Create: `backend/migrations/2026-09-14-000078_app_releases/up.sql`
- Create: `backend/migrations/2026-09-14-000078_app_releases/down.sql`
- Modify: `backend/crates/sauron-db/src/schema.rs` (after the `app_environments` block, ~line 103)
- Test: `backend/crates/sauron-db/tests/app_releases.rs`

**Interfaces:**
- Produces: table `app_releases` and unique index `app_releases_identity_idx` on `(app_id, COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid), release)`. Diesel table `app_releases (id) { id -> Int8, app_id -> Uuid, environment_id -> Nullable<Uuid>, release -> Text, first_seen_at -> Timestamptz, last_seen_at -> Timestamptz }`.

- [ ] **Step 1: Write the failing DB test**

`backend/crates/sauron-db/tests/app_releases.rs`:

```rust
//! `app_releases` — the observed (app, environment, release) table behind the
//! dashboard's release switcher. Mirrors `device_env_rollup.rs`: the identity
//! lives in a UNIQUE EXPRESSION index because NULL never equals NULL.

mod common;

use common::TestDb;
use diesel_async::RunQueryDsl;

#[tokio::test]
async fn unattributed_release_rows_are_unique_per_app() {
    let Some(db) = TestDb::setup().await else {
        panic!("TEST_DATABASE_URL unset — this test must not silently skip");
    };
    let mut conn = db.conn().await;
    let app_id = db.seed_two_envs().await.app_id;

    let insert = || {
        diesel::sql_query(
            "INSERT INTO app_releases \
               (app_id, environment_id, release, first_seen_at, last_seen_at) \
             VALUES ($1, NULL, '1.0.0', now(), now())",
        )
        .bind::<diesel::sql_types::Uuid, _>(app_id)
    };

    insert().execute(&mut conn).await.expect("first insert");
    assert!(
        insert().execute(&mut conn).await.is_err(),
        "a second NULL-environment row for the same (app, release) must be rejected"
    );

    drop(conn);
    db.cleanup().await;
}
```

- [ ] **Step 2: Run it to verify it fails**

Run from `backend/`:
```bash
TEST_DATABASE_URL=$TEST_DATABASE_URL cargo test -p sauron-db --test app_releases -- --nocapture
```
Expected: FAIL with `relation "app_releases" does not exist`.

- [ ] **Step 3: Write the migration**

`up.sql`:
```sql
-- app_releases: every (app, environment, release) triple ever observed at
-- ingest. Observed, not managed — rows appear when events arrive and are
-- only ever widened. `environment_id` is the ENROLLMENT id
-- (app_environments.id), the same id every environment_id column in the
-- telemetry tables carries; NULL means unattributed.
--
-- The identity is a UNIQUE EXPRESSION index rather than a PRIMARY KEY
-- because NULL never equals NULL: a plain UNIQUE (app_id, environment_id,
-- release) would let unattributed rows duplicate without bound and every
-- upsert against them would INSERT instead of UPDATE (see migration 59 for
-- the same reasoning on device_environments).
CREATE TABLE app_releases (
    id             BIGSERIAL PRIMARY KEY,
    app_id         UUID        NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    environment_id UUID        NULL,
    release        TEXT        NOT NULL,
    first_seen_at  TIMESTAMPTZ NOT NULL,
    last_seen_at   TIMESTAMPTZ NOT NULL
);

CREATE UNIQUE INDEX app_releases_identity_idx
    ON app_releases (app_id, COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid), release);

CREATE INDEX app_releases_app_last_seen_idx
    ON app_releases (app_id, last_seen_at DESC);

-- Release-filtered list pages. error_events and analytics_events already
-- carry (app_id, release, occurred_at DESC) from migration 25; sessions and
-- transactions did not. sessions is partitioned: this builds one index per
-- partition and holds a lock while it does, so apply at low traffic.
CREATE INDEX sessions_app_release_last_event_idx
    ON sessions (app_id, release, last_event_at DESC);

CREATE INDEX transactions_app_release_occurred_idx
    ON transactions (app_id, release, occurred_at DESC);
```

`down.sql`:
```sql
DROP INDEX IF EXISTS transactions_app_release_occurred_idx;
DROP INDEX IF EXISTS sessions_app_release_last_event_idx;
DROP TABLE IF EXISTS app_releases;
```

- [ ] **Step 4: Add the diesel table to `schema.rs`**

Insert after the `app_environments` block:
```rust
diesel::table! {
    app_releases (id) {
        id -> Int8,
        app_id -> Uuid,
        environment_id -> Nullable<Uuid>,
        release -> Text,
        first_seen_at -> Timestamptz,
        last_seen_at -> Timestamptz,
    }
}
```
Add `app_releases` to the `allow_tables_to_appear_in_same_query!` list at the bottom of the file and `diesel::joinable!(app_releases -> apps (app_id));` next to the other joinables (~line 877).

- [ ] **Step 5: Run the test to verify it passes**

Same command as Step 2. Expected: PASS, duration > 0s.

- [ ] **Step 6: Clippy**

```bash
cd backend && cargo +1.98.0 clippy -p sauron-db --all-targets -- -D warnings
```
Expected: clean.

---

### Task 2: `sauron_db::releases` — upsert, list, backfill

**Files:**
- Create: `backend/crates/sauron-db/src/releases.rs`
- Modify: `backend/crates/sauron-db/src/lib.rs` (add `pub mod releases;` after `pub mod query_plan;`)
- Test: `backend/crates/sauron-db/tests/app_releases.rs` (append)

**Interfaces:**
- Produces:
  ```rust
  pub struct AppReleaseRow { pub release: String, pub environment_id: Option<Uuid>, pub first_seen_at: DateTime<Utc>, pub last_seen_at: DateTime<Utc> }
  pub async fn upsert_seen(conn: &mut AsyncPgConnection, app_id: Uuid, environment_id: Option<Uuid>, release: &str, at: DateTime<Utc>) -> QueryResult<()>;
  pub async fn list_for_app(conn: &mut AsyncPgConnection, scope: &ReadScope) -> QueryResult<Vec<AppReleaseRow>>;
  pub async fn backfill_all(pool: &PgPool) -> anyhow::Result<u64>;
  ```

- [ ] **Step 1: Write the failing tests**

Append to `tests/app_releases.rs`:
```rust
use chrono::{Duration, Utc};
use sauron_db::releases::{backfill_all, list_for_app, upsert_seen};
use sauron_db::scope::{EnvFilter, ReadScope};

#[tokio::test]
async fn upsert_widens_one_row_per_identity() {
    let Some(db) = TestDb::setup().await else { panic!("TEST_DATABASE_URL unset") };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs().await;
    let t0 = Utc::now() - Duration::minutes(10);
    let t1 = Utc::now();

    upsert_seen(&mut conn, ids.app_id, Some(ids.env_a), "1.0.0", t1).await.unwrap();
    upsert_seen(&mut conn, ids.app_id, Some(ids.env_a), "1.0.0", t0).await.unwrap();
    upsert_seen(&mut conn, ids.app_id, None, "1.0.0", t1).await.unwrap();
    upsert_seen(&mut conn, ids.app_id, None, "1.0.0", t1).await.unwrap();

    let rows = list_for_app(&mut conn, &ReadScope::new(ids.app_id, EnvFilter::All)).await.unwrap();
    assert_eq!(rows.len(), 2, "{rows:?}");
    let attributed = rows.iter().find(|r| r.environment_id == Some(ids.env_a)).unwrap();
    assert!(attributed.first_seen_at <= t0 + Duration::seconds(1), "first_seen widened backwards");
    assert!(attributed.last_seen_at >= t1 - Duration::seconds(1));

    let only_a = list_for_app(&mut conn, &ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a))).await.unwrap();
    assert_eq!(only_a.len(), 1);
    let unattributed = list_for_app(&mut conn, &ReadScope::new(ids.app_id, EnvFilter::Unattributed)).await.unwrap();
    assert_eq!(unattributed.len(), 1);
    assert_eq!(unattributed[0].environment_id, None);

    drop(conn);
    db.cleanup().await;
}

#[tokio::test]
async fn backfill_seeds_from_both_event_tables_and_is_idempotent() {
    let Some(db) = TestDb::setup().await else { panic!("TEST_DATABASE_URL unset") };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs().await;

    diesel::sql_query(
        "INSERT INTO analytics_events (id, app_id, environment_id, name, distinct_id, properties, context, release, occurred_at, tags, contexts, extra) \
         VALUES (gen_random_uuid(), $1, $2, 'seen', 'd1', '{}', '{}', '2.0.0', now() - interval '2 days', '{}', '{}', '{}')",
    )
    .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
    .bind::<diesel::sql_types::Uuid, _>(ids.env_a)
    .execute(&mut conn).await.unwrap();
    // Seed one error_events row for release 3.0.0 the same way — copy the
    // column list from `seed_issue_with_error` in
    // bins/sauron-api/tests/http_env_scoping.rs, or use repo::insert_error_event.
    drop(conn);

    let first = backfill_all(&db.pool).await.unwrap();
    let second = backfill_all(&db.pool).await.unwrap();
    assert!(first >= 2, "expected at least two seeded rows, got {first}");
    assert_eq!(second, first, "backfill must be idempotent");

    let mut conn = db.conn().await;
    let rows = list_for_app(&mut conn, &ReadScope::new(ids.app_id, EnvFilter::All)).await.unwrap();
    let names: Vec<&str> = rows.iter().map(|r| r.release.as_str()).collect();
    assert!(names.contains(&"2.0.0") && names.contains(&"3.0.0"), "{names:?}");
    drop(conn);
    db.cleanup().await;
}
```
Read `SeedIds` in `tests/common/mod.rs` (line ~592) for the exact env field names and adjust `env_a`. Read the `analytics_events` column list in `schema.rs` for NOT NULL columns and add any missing ones to the INSERT.

- [ ] **Step 2: Run to verify they fail**

Expected: compile error, `releases` module not found.

- [ ] **Step 3: Implement `releases.rs`**

```rust
//! `app_releases`: the observed release catalogue behind the dashboard's
//! release switcher. See migration 78 for why the identity is an expression
//! index. Three entry points: the pipeline's `upsert_seen`, the API's
//! `list_for_app`, and the operator-run `backfill_all`.

use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel::sql_types::{Nullable, Text, Timestamptz, Uuid as SqlUuid};
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use uuid::Uuid;

use crate::scope::{EnvFilter, ReadScope};
use crate::PgPool;

#[derive(Debug, Clone, QueryableByName, serde::Serialize, utoipa::ToSchema)]
pub struct AppReleaseRow {
    #[diesel(sql_type = Text)]
    pub release: String,
    #[diesel(sql_type = Nullable<SqlUuid>)]
    pub environment_id: Option<Uuid>,
    #[diesel(sql_type = Timestamptz)]
    pub first_seen_at: DateTime<Utc>,
    #[diesel(sql_type = Timestamptz)]
    pub last_seen_at: DateTime<Utc>,
}

const NIL: &str = "'00000000-0000-0000-0000-000000000000'::uuid";

/// One row per (app, env, release), widened on conflict. The ON CONFLICT
/// target MUST name the COALESCE expression: the bare column list compiles,
/// runs, and inserts duplicates for unattributed rows.
pub async fn upsert_seen(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    environment_id: Option<Uuid>,
    release: &str,
    at: DateTime<Utc>,
) -> QueryResult<()> {
    diesel::sql_query(format!(
        "INSERT INTO app_releases (app_id, environment_id, release, first_seen_at, last_seen_at) \
         VALUES ($1, $2, $3, $4, $4) \
         ON CONFLICT (app_id, COALESCE(environment_id, {NIL}), release) DO UPDATE SET \
           first_seen_at = LEAST(app_releases.first_seen_at, EXCLUDED.first_seen_at), \
           last_seen_at  = GREATEST(app_releases.last_seen_at, EXCLUDED.last_seen_at)"
    ))
    .bind::<SqlUuid, _>(app_id)
    .bind::<Nullable<SqlUuid>, _>(environment_id)
    .bind::<Text, _>(release)
    .bind::<Timestamptz, _>(at)
    .execute(conn)
    .await
    .map(|_| ())
}

/// Every observed release row the scope may see, newest `last_seen_at` first.
pub async fn list_for_app(
    conn: &mut AsyncPgConnection,
    scope: &ReadScope,
) -> QueryResult<Vec<AppReleaseRow>> {
    // `EnvFilter::sql_fragment(bind_index)` renders the env predicate with $2
    // as its first bind; `bind_uuids()` says whether it consumes one.
    let env_sql = scope.env.sql_fragment(2);
    let sql = format!(
        "SELECT release, environment_id, first_seen_at, last_seen_at \
         FROM app_releases WHERE app_id = $1 AND {env_sql} \
         ORDER BY last_seen_at DESC, release, environment_id"
    );
    let q = diesel::sql_query(sql).bind::<SqlUuid, _>(scope.app_id);
    match scope.env.bind_uuids() {
        Some(ids) => q.bind::<diesel::sql_types::Array<SqlUuid>, _>(ids).load(conn).await,
        None => q.load(conn).await,
    }
}

/// Operator-run seed from the two event tables that already carry an
/// `(app_id, release, occurred_at DESC)` index. Returns the number of rows
/// upserted. Idempotent: re-running only widens.
pub async fn backfill_all(pool: &PgPool) -> anyhow::Result<u64> {
    let mut conn = crate::conn(pool).await?;
    let mut total = 0u64;
    for table in ["error_events", "analytics_events"] {
        let n = diesel::sql_query(format!(
            "INSERT INTO app_releases (app_id, environment_id, release, first_seen_at, last_seen_at) \
             SELECT app_id, environment_id, release, MIN(occurred_at), MAX(occurred_at) \
             FROM {table} WHERE release IS NOT NULL \
             GROUP BY app_id, environment_id, release \
             ON CONFLICT (app_id, COALESCE(environment_id, {NIL}), release) DO UPDATE SET \
               first_seen_at = LEAST(app_releases.first_seen_at, EXCLUDED.first_seen_at), \
               last_seen_at  = GREATEST(app_releases.last_seen_at, EXCLUDED.last_seen_at)"
        ))
        .execute(&mut conn)
        .await?;
        total += n as u64;
    }
    Ok(total)
}
```
Check `EnvFilter::sql_fragment` and `bind_uuids` in `crates/sauron-db/src/scope.rs:54-86` to confirm the bind convention (whether `One` binds a single uuid or an array) and adjust the `match`. If `sql_fragment` expects a different alias, use `sql_fragment_for`.

Note on the backfill: `GROUP BY` over the whole table is a full scan on the remote (158M rows). That is acceptable for a one-time operator step, and the Task 4 runbook entry says so. Do not run it on a request path.

- [ ] **Step 4: Run tests, expect PASS**

```bash
TEST_DATABASE_URL=$TEST_DATABASE_URL cargo test -p sauron-db --test app_releases -- --nocapture
```

- [ ] **Step 5: Clippy** as in Task 1.

---

### Task 3: Pipeline notes releases with a throttle

**Files:**
- Create: `backend/crates/sauron-pipeline/src/releases.rs`
- Modify: `backend/crates/sauron-pipeline/src/lib.rs` (add `pub mod releases;`)
- Modify: `backend/crates/sauron-pipeline/src/process.rs:54-75` (call after the connection is acquired)

**Interfaces:**
- Consumes: `sauron_db::releases::upsert_seen`.
- Produces: `pub async fn note_release(conn: &mut AsyncPgConnection, job: &IngestJob) -> anyhow::Result<()>` and `pub fn should_write(cache: &mut HashMap<(Uuid, Uuid, String), Instant>, key: (Uuid, Uuid, String), now: Instant) -> bool`.

- [ ] **Step 1: Write the failing unit test for the throttle**

In `releases.rs` (module tests):
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn first_sight_writes_then_throttles_then_writes_again() {
        let mut cache = HashMap::new();
        let key = (Uuid::nil(), Uuid::nil(), "1.0.0".to_string());
        let t0 = Instant::now();
        assert!(should_write(&mut cache, key.clone(), t0));
        assert!(!should_write(&mut cache, key.clone(), t0 + Duration::from_secs(10)));
        assert!(should_write(&mut cache, key.clone(), t0 + WRITE_INTERVAL + Duration::from_secs(1)));
    }

    #[test]
    fn different_keys_do_not_share_a_throttle() {
        let mut cache = HashMap::new();
        let t0 = Instant::now();
        assert!(should_write(&mut cache, (Uuid::nil(), Uuid::nil(), "a".into()), t0));
        assert!(should_write(&mut cache, (Uuid::nil(), Uuid::nil(), "b".into()), t0));
    }
}
```

- [ ] **Step 2: Run to verify it fails**

```bash
cd backend && cargo test -p sauron-pipeline releases::
```
Expected: compile error.

- [ ] **Step 3: Implement**

```rust
//! Records every (app, env, release) the worker sees into `app_releases`,
//! throttled per key so a busy release costs one UPDATE per minute per
//! worker rather than one per job. The cache is process-local and bounded
//! only by the number of live keys, which is small (apps × envs × releases
//! still reporting).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use diesel_async::AsyncPgConnection;
use sauron_core::IngestJob;
use uuid::Uuid;

pub const WRITE_INTERVAL: Duration = Duration::from_secs(60);

static SEEN: Mutex<Option<HashMap<(Uuid, Uuid, String), Instant>>> = Mutex::new(None);

/// Pure throttle decision, separated so it can be unit-tested without a DB.
pub fn should_write(
    cache: &mut HashMap<(Uuid, Uuid, String), Instant>,
    key: (Uuid, Uuid, String),
    now: Instant,
) -> bool {
    match cache.get(&key) {
        Some(last) if now.duration_since(*last) < WRITE_INTERVAL => false,
        _ => {
            cache.insert(key, now);
            true
        }
    }
}

/// Upsert the job's release if this worker has not written it in the last
/// minute. A job without a release writes nothing. Errors are returned so the
/// caller can log them, but the caller must NOT fail the job on them: a
/// release row is a convenience, the event is the data.
pub async fn note_release(conn: &mut AsyncPgConnection, job: &IngestJob) -> anyhow::Result<()> {
    let Some(release) = job.release.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(());
    };
    let key = (job.app_id, job.environment_id, release.to_string());
    let write = {
        let mut guard = SEEN.lock().unwrap_or_else(|p| p.into_inner());
        let cache = guard.get_or_insert_with(HashMap::new);
        should_write(cache, key, Instant::now())
    };
    if write {
        sauron_db::releases::upsert_seen(conn, job.app_id, Some(job.environment_id), release, job.received_at).await?;
    }
    Ok(())
}
```

In `process.rs`, right after `let mut conn = sauron_db::conn(pool).await?;`:
```rust
    if let Err(e) = crate::releases::note_release(&mut conn, &job).await {
        tracing::warn!(error = %e, app_id = %job.app_id, "app_releases upsert failed; continuing");
    }
```
Check that `process.rs` imports `tracing` (it logs elsewhere; match the existing macro).

- [ ] **Step 4: Run unit tests, expect PASS**

- [ ] **Step 5: DB test through `process_job`** — skip a new harness. The pipeline has no DB test harness of its own and building one is out of scope. Coverage of the upsert itself is Task 2; coverage of the call site is the browser drive in Task 12 and the sweep in Task 7.

- [ ] **Step 6: Clippy** (`-p sauron-pipeline`).

---

### Task 4: `sauron-migrate backfill-releases` and runbook

**Files:**
- Modify: `backend/bins/sauron-migrate/src/main.rs:24-48` (COMMANDS) and `:252-270` (dispatch)
- Modify: the rollup runbook under `docs/` (find with `grep -rl "backfill-rollups" docs/`) — add an entry.

- [ ] **Step 1: Add the command**

Grow `COMMANDS` to `[Command; 5]` and add:
```rust
    Command {
        name: "backfill-releases",
        summary: "Seed app_releases (the dashboard's release switcher) from \
                  error_events and analytics_events. Full scan of both tables; \
                  run once after migration 78, at low traffic. Idempotent.",
    },
```
Dispatch, next to the others:
```rust
    if tasks.contains(&"backfill-releases") {
        let pool = sauron_db::build_pool(&url, 4)?;
        let n = sauron_db::releases::backfill_all(&pool).await?;
        println!("backfill-releases: upserted {n} rows; run ANALYZE app_releases");
    }
```

- [ ] **Step 2: Verify the help output and the unknown-token guard still work**

```bash
cd backend && cargo run -p sauron-migrate -- --help | grep backfill-releases
```
Expected: the new line appears. If `--help` needs a DATABASE_URL, set a dummy one.

- [ ] **Step 3: Runbook entry**

Add a section to the rollup runbook, in the same voice as the existing backfill sections:

```markdown
## backfill-releases (migration 78)

`app_releases` is empty after the upgrade. The release switcher only lists
releases the pipeline has seen since the upgrade until this runs:

    sauron-migrate backfill-releases
    psql -c 'ANALYZE app_releases'

Full scan of error_events and analytics_events; hours at 158M rows. Safe to
re-run. Nothing else depends on it.
```

- [ ] **Step 4: Clippy** (`-p sauron-migrate`).

---

### Task 5: Query catalog and lowering — `release` on Issues and Transactions, `with_release`

**Files:**
- Modify: `backend/crates/sauron-query/src/catalog.rs:361-369` (Issues release dim) and `:398-406` (release column dim resources)
- Create: `backend/crates/sauron-query/src/release_scope.rs`
- Modify: `backend/crates/sauron-query/src/lib.rs` (`pub mod release_scope; pub use release_scope::{ReleaseFilter, with_release};`)
- Modify: `backend/crates/sauron-db/src/query_plan/issues.rs:436-445` (add arm) and `:1308-1320` (test)
- Modify: `backend/crates/sauron-db/src/query_plan/transactions.rs:639-651` (add arm)
- Test: module tests in each file

**Interfaces:**
- Produces:
  ```rust
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub enum ReleaseFilter { All, One(String), Unknown }
  pub fn with_release(node: ResolvedNode, resource: Resource, filter: &ReleaseFilter) -> ResolvedNode;
  ```

- [ ] **Step 1: Write failing tests**

`release_scope.rs` tests:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{parse, resolve, MatchOp, Resource, ResolvedNode, TypedValue};

    fn base() -> ResolvedNode {
        resolve(&parse("level:error").unwrap(), Resource::Events).unwrap()
    }

    #[test]
    fn all_is_identity() {
        let n = base();
        assert_eq!(format!("{:?}", with_release(n.clone(), Resource::Events, &ReleaseFilter::All)), format!("{n:?}"));
    }

    #[test]
    fn one_ands_an_eq_predicate() {
        let out = with_release(base(), Resource::Events, &ReleaseFilter::One("1.4.0".into()));
        let ResolvedNode::And(parts) = out else { panic!("expected And") };
        let ResolvedNode::Pred(p) = &parts[1] else { panic!("expected Pred") };
        assert_eq!(p.dim.name, "release");
        assert_eq!(p.op, MatchOp::Eq);
        assert_eq!(p.value, TypedValue::Str("1.4.0".into()));
    }

    #[test]
    fn unknown_ands_a_negated_has() {
        let out = with_release(base(), Resource::Sessions, &ReleaseFilter::Unknown);
        let ResolvedNode::And(parts) = out else { panic!("expected And") };
        let ResolvedNode::Not(inner) = &parts[1] else { panic!("expected Not") };
        let ResolvedNode::Pred(p) = &**inner else { panic!("expected Pred") };
        assert_eq!(p.op, MatchOp::Has);
    }

    #[test]
    fn every_list_resource_has_a_release_dimension() {
        for r in [Resource::Issues, Resource::Occurrences, Resource::Events, Resource::Sessions, Resource::Transactions] {
            assert!(crate::catalog::lookup("release", r).is_some(), "{r:?}");
        }
    }
}
```
`issues.rs` tests: replace `every_rollup_dimension_on_issues_is_rejected` so it iterates only `["handled:true"]`, and add:
```rust
    #[test]
    fn release_on_issues_bridges_through_error_events() {
        let sql = lower_issues_sql("release:1.0.0");
        assert!(sql.contains("EXISTS"), "{sql}");
        assert!(sql.contains("e.release = "), "{sql}");
    }
```
`transactions.rs` tests, following the file's existing `lower_transactions_sql` helper:
```rust
    #[test]
    fn release_lowers_to_a_column_equality() {
        let sql = lower_transactions_sql("release:1.0.0");
        assert!(sql.contains("\"transactions\".\"release\" = "), "{sql}");
    }
```
Check `MatchOp`, `TypedValue` and `ResolvedPredicate` derive `PartialEq`/`Debug`; add derives if missing.

- [ ] **Step 2: Run, expect failures** (`cargo test -p sauron-query`, `cargo test -p sauron-db --lib query_plan`).

- [ ] **Step 3: Catalog changes**

Issues release dim (`catalog.rs:361-369`) becomes:
```rust
    Dimension {
        name: "release",
        aliases: NO_ALIAS,
        ty: ValueType::Str,
        store: Store::Column("release"),
        ops: OPS_EQ,
        resources: R_ISSUES,
        index: IndexClass::Bounded,
    },
```
Column release dim (`catalog.rs:398-406`): `resources: &[Resource::Occurrences, Resource::Events, Resource::Sessions, Resource::Transactions],`.

Update the `catalog.rs` module tests that assert `release` on Issues is a rollup, if any exist beyond `environment_on_issues_is_the_rollup` (grep `"release"` in the tests block). Update the module doc at `issues.rs:4` which lists `release` among the rollup dims.

- [ ] **Step 4: Lowering arms**

`issues.rs`, next to the `screen` arm:
```rust
            Store::Column("release") => {
                occurrence_column_leaf(" AND e.release", p, negate, self.env, self.since)
            }
```
`transactions.rs`, after `url`:
```rust
            Store::Column("release") => str_leaf!(transactions::release, p, negate),
```

- [ ] **Step 5: `release_scope.rs`**

```rust
//! The `?release=` query parameter, expressed as a query-node rewrite so it
//! composes with every other predicate the planner already understands.
//! `Unknown` (wire literal `none`) is `!has:release`, i.e. `release IS NULL`.

use crate::catalog::lookup;
use crate::{MatchOp, Resource, ResolvedNode, ResolvedPredicate, TypedValue};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseFilter {
    All,
    One(String),
    Unknown,
}

pub fn with_release(node: ResolvedNode, resource: Resource, filter: &ReleaseFilter) -> ResolvedNode {
    let dim = lookup("release", resource)
        .expect("every searched list resource carries a `release` dimension (catalog parity test)");
    let pred = |op: MatchOp, value: TypedValue| ResolvedPredicate {
        dim,
        path: None,
        op,
        value,
        at: 0,
        index: dim.index,
    };
    let extra = match filter {
        ReleaseFilter::All => return node,
        ReleaseFilter::One(v) => ResolvedNode::Pred(pred(MatchOp::Eq, TypedValue::Str(v.clone()))),
        ReleaseFilter::Unknown => ResolvedNode::Not(Box::new(ResolvedNode::Pred(pred(MatchOp::Has, TypedValue::Absent)))),
    };
    ResolvedNode::And(vec![node, extra])
}
```
Check `ResolvedPredicate` for any field not listed here (resolve.rs:50-65) and fill it the way `resolve` does.

- [ ] **Step 6: Run tests, expect PASS.** Also run the full `cargo test -p sauron-query -p sauron-db --lib` because the catalog has parity tests of its own.

- [ ] **Step 7: Dashboard catalog parity** — run `cd dashboard && npx vitest run src/lib/components/filters/catalog-field-parity.test.ts`. If it fails because Issues/Transactions now expose `release`, add `{ key: 'release', labelKey: 'filter.field.release', type: 'string', ops: OPS_STR }` to the Issues and Transactions field lists in `filters.ts` (Issues gets `OPS_EQ`-shaped ops; copy the ops constant the file uses for `environment` on Issues). Re-run until green.

- [ ] **Step 8: Clippy.**

---

### Task 6: API — parse, thread, and guard `release=`

**Files:**
- Modify: `backend/bins/sauron-api/src/routes/scope.rs` (after `parse_env`)
- Create: `backend/bins/sauron-api/src/release_guard.rs`
- Modify: `backend/bins/sauron-api/src/main.rs` (`mod release_guard;`, `.layer(axum::middleware::from_fn(release_guard::reject_release_outside_allowlist))` next to the CORS layer at ~1121)
- Modify: `routes/issues.rs:196` (`list`) and `:610` (`events`), `routes/analytics.rs:536` (`events_list`), `routes/sessions.rs:148` (`list`), `routes/transactions.rs:116` (`list`)
- Test: `backend/bins/sauron-api/tests/http_release_scoping.rs`

**Interfaces:**
- Produces:
  ```rust
  pub fn raw_release(raw_query: Option<&str>) -> Option<String>;
  pub fn parse_release(raw: Option<&str>) -> Result<ReleaseFilter, ApiError>;
  pub const RELEASE_ACCEPTING_PATHS: &[&str] = &[
      "/v1/apps/{app_id}/issues",
      "/v1/apps/{app_id}/issues/{issue_id}/events",
      "/v1/apps/{app_id}/events/list",
      "/v1/apps/{app_id}/sessions",
      "/v1/apps/{app_id}/transactions",
  ];
  ```
  The dashboard parity test in Task 11 reads `RELEASE_ACCEPTING_PATHS` from this file's source text, so keep the array literal exactly in that shape: one string per line, no computed entries.

- [ ] **Step 1: Write the failing route tests**

Copy the `TestServer`, `swap_database`, `free_port`, `seed_env`, `seed_fixture`, and `seed_analytics_event` helpers verbatim from `tests/http_env_scoping.rs` into `tests/http_release_scoping.rs` (each test file is self-contained by convention; `#[path = "../src/route_table.rs"] mod route_table;` too). Then:

```rust
#[tokio::test]
async fn release_filters_the_events_list_and_none_means_null() {
    let Some(mut srv) = TestServer::start().await else { panic!("TEST_DATABASE_URL/TEST_REDIS_URL unset") };
    let fx = srv.seed_fixture().await;
    {
        let mut conn = srv.conn().await;
        seed_analytics_event_with_release(&mut conn, fx.app_id, Some(fx.env_id), "with", Some("1.4.0")).await;
        seed_analytics_event_with_release(&mut conn, fx.app_id, Some(fx.env_id), "without", None).await;
    }
    let all = srv.get_json(&format!("/v1/apps/{}/events/list", fx.app_id), &fx.token).await;
    assert_eq!(all["total"], 2, "{all}");
    let one = srv.get_json(&format!("/v1/apps/{}/events/list?release=1.4.0", fx.app_id), &fx.token).await;
    assert_eq!(one["total"], 1, "{one}");
    assert_eq!(one["data"][0]["name"], "with");
    let none = srv.get_json(&format!("/v1/apps/{}/events/list?release=none", fx.app_id), &fx.token).await;
    assert_eq!(none["total"], 1, "{none}");
    assert_eq!(none["data"][0]["name"], "without");
    srv.shutdown().await;
}

#[tokio::test]
async fn empty_release_is_a_400() {
    let Some(mut srv) = TestServer::start().await else { panic!("env unset") };
    let fx = srv.seed_fixture().await;
    let status = srv.get_status(&format!("/v1/apps/{}/events/list?release=", fx.app_id), &fx.token).await;
    assert_eq!(status, 400);
    srv.shutdown().await;
}

/// Every app-scoped GET that is NOT in the allowlist must reject `release=`
/// with 400 — the fail-loud rule `environment_id` already follows.
#[tokio::test]
async fn every_other_app_scoped_get_rejects_release() {
    let Some(mut srv) = TestServer::start().await else { panic!("env unset") };
    let fx = srv.seed_fixture().await;
    let accepting: Vec<String> = release_guard::RELEASE_ACCEPTING_PATHS.iter().map(|s| s.to_string()).collect();
    for template in route_table::app_scoped_get_paths() {
        if accepting.contains(&template) { continue; }
        let path = build_request_path(&template, fx.app_id); // copy from http_env_scoping.rs
        let sep = if path.contains('?') { '&' } else { '?' };
        let (status, body) = srv.get_status_and_body(&format!("{path}{sep}release=1.0.0"), &fx.token).await;
        assert_eq!(status, 400, "{template} must reject release=; got {status}: {body}");
    }
    srv.shutdown().await;
}

#[tokio::test]
async fn allowlisted_routes_accept_release() {
    let Some(mut srv) = TestServer::start().await else { panic!("env unset") };
    let fx = srv.seed_fixture().await;
    for template in release_guard::RELEASE_ACCEPTING_PATHS {
        let path = build_request_path(template, fx.app_id);
        let sep = if path.contains('?') { '&' } else { '?' };
        let (status, body) = srv.get_status_and_body(&format!("{path}{sep}release=1.0.0"), &fx.token).await;
        assert!(status == 200 || status == 404, "{template}: {status} {body}");
    }
    srv.shutdown().await;
}
```
Add `#[path = "../src/release_guard.rs"] mod release_guard;` at the top. Write `seed_analytics_event_with_release` as a copy of `seed_analytics_event` that sets `release: release.map(str::to_string)`.

- [ ] **Step 2: Run, expect compile failure** (`cargo test -p sauron-api --test http_release_scoping`).

- [ ] **Step 3: `scope.rs` additions**

```rust
/// The raw `release` query value, if present. Same shape as `raw_environment_id`.
pub fn raw_release(raw_query: Option<&str>) -> Option<String> {
    let raw_query = raw_query?;
    form_urlencoded::parse(raw_query.as_bytes())
        .find(|(k, _)| k == "release")
        .map(|(_, v)| v.into_owned())
}

/// `?release=`: absent = all, literal `none` = rows with no release, anything
/// else = exact match. Empty and whitespace-only are 400, never "all" — the
/// same rule `parse_env` applies.
pub fn parse_release(raw: Option<&str>) -> Result<sauron_query::ReleaseFilter, ApiError> {
    use sauron_query::ReleaseFilter;
    match raw.map(str::trim) {
        None => Ok(ReleaseFilter::All),
        Some("") => Err(ApiError::BadRequest("release must not be empty".into())),
        Some("none") => Ok(ReleaseFilter::Unknown),
        Some(s) => Ok(ReleaseFilter::One(s.to_string())),
    }
}
```
Confirm `sauron-api`'s `Cargo.toml` depends on `sauron-query` (it does; `routes/search.rs` imports it).

- [ ] **Step 4: `release_guard.rs`**

```rust
//! Rejects `?release=` on every route that does not consume it, so a caller
//! never silently gets an unfiltered answer. Mirrors the `environment_id`
//! fail-loud rule, but as one middleware instead of a call per handler,
//! because the accepting set is five routes and the rejecting set is
//! everything else.

use axum::{extract::Request, middleware::Next, response::Response};
use axum::http::StatusCode;

/// Route TEMPLATES (as registered in main.rs) that read `release`. The
/// dashboard's `release-scope-parity.test.ts` parses this array from source.
pub const RELEASE_ACCEPTING_PATHS: &[&str] = &[
    "/v1/apps/{app_id}/issues",
    "/v1/apps/{app_id}/issues/{issue_id}/events",
    "/v1/apps/{app_id}/events/list",
    "/v1/apps/{app_id}/sessions",
    "/v1/apps/{app_id}/transactions",
];

fn matches_template(template: &str, path: &str) -> bool {
    let t: Vec<&str> = template.split('/').collect();
    let p: Vec<&str> = path.split('/').collect();
    t.len() == p.len()
        && t.iter().zip(&p).all(|(a, b)| a.starts_with('{') || a == b)
}

pub fn path_accepts_release(path: &str) -> bool {
    RELEASE_ACCEPTING_PATHS.iter().any(|t| matches_template(t, path))
}

pub async fn reject_release_outside_allowlist(req: Request, next: Next) -> Result<Response, (StatusCode, axum::Json<serde_json::Value>)> {
    let has_release = req
        .uri()
        .query()
        .map(|q| form_urlencoded::parse(q.as_bytes()).any(|(k, _)| k == "release"))
        .unwrap_or(false);
    if has_release && !path_accepts_release(req.uri().path()) {
        return Err((
            StatusCode::BAD_REQUEST,
            axum::Json(serde_json::json!({ "error": { "message": "release is not supported on this endpoint" } })),
        ));
    }
    Ok(next.run(req).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_match_concrete_paths_only_at_the_same_depth() {
        assert!(path_accepts_release("/v1/apps/0b6a/issues"));
        assert!(path_accepts_release("/v1/apps/0b6a/issues/77/events"));
        assert!(!path_accepts_release("/v1/apps/0b6a/issues/77"));
        assert!(!path_accepts_release("/v1/apps/0b6a/overview"));
        assert!(!path_accepts_release("/v1/orgs/1/alert-rules"));
    }
}
```
Match the error body shape to `ApiError::BadRequest`'s serialisation (see `error.rs` in the bin); if `ApiError` implements `IntoResponse`, return `Err(ApiError::BadRequest(...))` instead of a hand-built tuple.

- [ ] **Step 5: Thread into the five handlers**

In each handler, immediately after `resolve_query(...)` produces `node`, and before `reject_withheld_dimensions`:
```rust
    let release = super::scope::parse_release(
        super::scope::raw_release(raw_query.as_deref()).as_deref(),
    )?;
    let node = sauron_query::with_release(node, sauron_query::Resource::Transactions, &release);
```
with the resource matching each route (`Issues`, `Occurrences`, `Events`, `Sessions`, `Transactions`). `node` must be `let mut`/rebound as shown. The issues `events` route (per-issue occurrences) uses `Resource::Occurrences`.

Also register the middleware in `main.rs` next to `.layer(cors)`:
```rust
        .layer(axum::middleware::from_fn(release_guard::reject_release_outside_allowlist))
```
and `mod release_guard;` at the top.

- [ ] **Step 6: Run the route tests, expect PASS**

```bash
cd backend && TEST_DATABASE_URL=… TEST_REDIS_URL=… cargo test -p sauron-api --test http_release_scoping -- --nocapture
```
Also re-run `--test http_env_scoping` and the openapi parity test (`cargo test -p sauron-api openapi`) since a middleware and route table are involved.

- [ ] **Step 7: Clippy.**

---

### Task 7: `GET /v1/apps/{app_id}/releases`

**Files:**
- Create: `backend/bins/sauron-api/src/routes/releases.rs`
- Modify: `backend/bins/sauron-api/src/routes/mod.rs` (`pub mod releases;`)
- Modify: `backend/bins/sauron-api/src/main.rs:657` (route next to environments), `openapi.rs:169` (paths)
- Test: append to `tests/http_release_scoping.rs`

**Interfaces:**
- Produces JSON `[{ "release": "1.4.0", "environment_ids": ["<uuid>", null], "first_seen_at": "...", "last_seen_at": "..." }]`, newest `last_seen_at` first. Honors `?environment_id=` via `authorized_read_scope` with `perm::EVENT_READ`.

- [ ] **Step 1: Failing test**

```rust
#[tokio::test]
async fn releases_endpoint_groups_by_release_and_narrows_by_env() {
    let Some(mut srv) = TestServer::start().await else { panic!("env unset") };
    let fx = srv.seed_fixture().await;
    {
        let mut conn = srv.conn().await;
        let now = chrono::Utc::now();
        sauron_db::releases::upsert_seen(&mut conn, fx.app_id, Some(fx.env_id), "1.4.0", now).await.unwrap();
        sauron_db::releases::upsert_seen(&mut conn, fx.app_id, None, "1.4.0", now).await.unwrap();
        sauron_db::releases::upsert_seen(&mut conn, fx.app_id, None, "1.3.0", now - chrono::Duration::days(1)).await.unwrap();
    }
    let all = srv.get_json(&format!("/v1/apps/{}/releases", fx.app_id), &fx.token).await;
    assert_eq!(all.as_array().unwrap().len(), 2, "{all}");
    assert_eq!(all[0]["release"], "1.4.0");
    assert_eq!(all[0]["environment_ids"].as_array().unwrap().len(), 2);
    let env_only = srv.get_json(&format!("/v1/apps/{}/releases?environment_id={}", fx.app_id, fx.env_id), &fx.token).await;
    assert_eq!(env_only.as_array().unwrap().len(), 1, "{env_only}");
    let unattributed = srv.get_json(&format!("/v1/apps/{}/releases?environment_id=none", fx.app_id), &fx.token).await;
    assert_eq!(unattributed.as_array().unwrap().len(), 2, "{unattributed}");
    srv.shutdown().await;
}
```

- [ ] **Step 2: Run, expect 404** on the route.

- [ ] **Step 3: Implement**

```rust
//! `GET /v1/apps/{app_id}/releases` — the release switcher's list. Read from
//! `app_releases`, grouped per release across the caller's environment reach.

use axum::{extract::{Path, RawQuery, State}, Json};
use chrono::{DateTime, Utc};
use std::collections::BTreeMap;
use uuid::Uuid;

use crate::{db, error::ApiError, AppState};
use sauron_auth::{perm, AuthUser};

#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub struct ReleaseView {
    pub release: String,
    /// Enrollment ids that have seen this release; `null` = unattributed.
    pub environment_ids: Vec<Option<Uuid>>,
    pub first_seen_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
}

#[utoipa::path(
    get, path = "/v1/apps/{app_id}/releases", tag = "releases",
    params(("app_id" = Uuid, Path), ("environment_id" = Option<String>, Query, description = "Narrow to one enrollment id, or `none`")),
    responses((status = 200, body = Vec<ReleaseView>), (status = 400, body = crate::error::ErrorResponse), (status = 403, body = crate::error::ErrorResponse)),
    security(("bearer" = []))
)]
pub async fn list_app_releases(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<Vec<ReleaseView>>, ApiError> {
    let mut conn = db(&state).await?;
    let scope = super::scope::authorized_read_scope(
        &mut conn, auth.user_id, app_id, perm::EVENT_READ, raw_query.as_deref(),
    ).await?;
    let rows = sauron_db::releases::list_for_app(&mut conn, &scope).await?;

    let mut grouped: BTreeMap<String, ReleaseView> = BTreeMap::new();
    for r in rows {
        let entry = grouped.entry(r.release.clone()).or_insert_with(|| ReleaseView {
            release: r.release.clone(),
            environment_ids: Vec::new(),
            first_seen_at: r.first_seen_at,
            last_seen_at: r.last_seen_at,
        });
        entry.environment_ids.push(r.environment_id);
        entry.first_seen_at = entry.first_seen_at.min(r.first_seen_at);
        entry.last_seen_at = entry.last_seen_at.max(r.last_seen_at);
    }
    let mut out: Vec<ReleaseView> = grouped.into_values().collect();
    out.sort_by(|a, b| b.last_seen_at.cmp(&a.last_seen_at).then_with(|| a.release.cmp(&b.release)));
    Ok(Json(out))
}
```
Copy the exact `#[utoipa::path]` attribute style from `list_app_environments` in `routes/environments.rs:502` (tag names, `ErrorResponse` path, security scheme name). Register in `main.rs` right under the environments route and in `openapi.rs` `paths(...)`. If the OpenAPI schema test complains, add `ReleaseView` to `components(schemas(...))`.

- [ ] **Step 4: Run tests: `http_release_scoping`, `http_env_scoping` (the sweep now includes the new route and must see it honor `environment_id`), and `openapi`. Expect PASS.**

- [ ] **Step 5: Clippy.**

---

### Task 8: Dashboard store, API client, and scope interceptor

**Files:**
- Create: `dashboard/src/lib/api/releases.ts`
- Modify: `dashboard/src/lib/models/index.ts` (add `AppRelease`)
- Modify: `dashboard/src/lib/api/scope.ts` (bridge + `RELEASE_SCOPED_URL` + `computeScopeParams`)
- Modify: `dashboard/src/lib/api/client.ts:110-116`
- Modify: `dashboard/src/lib/stores/session.svelte.ts`
- Test: `dashboard/src/lib/api/scope.test.ts`, `dashboard/src/lib/stores/session.test.ts` (create if absent, mirroring how other store tests mock `localStorage`)

**Interfaces:**
- Produces:
  ```ts
  export interface AppRelease { release: string; environment_ids: (string | null)[]; first_seen_at: string; last_seen_at: string }
  export function listReleases(appId: string): Promise<AppRelease[]>;
  // scope.ts
  export interface ScopeBridge { getCurrentEnvironmentId(): string | null; getCurrentRelease(): string | null }
  export const RELEASE_SCOPED_URL: readonly RegExp[];
  export function computeScopeParams(url: string | undefined, envId: string | null, release: string | null): Record<string, string> | undefined;
  // session store
  releases = $state<AppRelease[]>([]); currentRelease = $state<string | null>(null);
  setRelease(id: string | null): void; get scopeKey(): string  // now `${app}:${env}:${release ?? 'all'}`
  ```

- [ ] **Step 1: Failing scope tests**

Append to `scope.test.ts`:
```ts
describe('release scoping', () => {
  it('attaches release only on the five searched list routes', () => {
    expect(computeScopeParams('/v1/apps/a/issues', null, '1.4.0')).toEqual({ release: '1.4.0' });
    expect(computeScopeParams('/v1/apps/a/issues/i1/events', null, '1.4.0')).toEqual({ release: '1.4.0' });
    expect(computeScopeParams('/v1/apps/a/events/list', null, '1.4.0')).toEqual({ release: '1.4.0' });
    expect(computeScopeParams('/v1/apps/a/sessions', null, '1.4.0')).toEqual({ release: '1.4.0' });
    expect(computeScopeParams('/v1/apps/a/transactions', null, '1.4.0')).toEqual({ release: '1.4.0' });
    expect(computeScopeParams('/v1/apps/a/overview', null, '1.4.0')).toBeUndefined();
    expect(computeScopeParams('/v1/apps/a/issues/i1', null, '1.4.0')).toBeUndefined();
  });

  it('combines env and release, and omits release when null', () => {
    expect(computeScopeParams('/v1/apps/a/issues', 'env-1', '1.4.0')).toEqual({ environment_id: 'env-1', release: '1.4.0' });
    expect(computeScopeParams('/v1/apps/a/issues', 'env-1', null)).toEqual({ environment_id: 'env-1' });
    expect(computeScopeParams('/v1/apps/a/issues', null, null)).toBeUndefined();
  });

  it('passes the literal none through', () => {
    expect(computeScopeParams('/v1/apps/a/sessions', null, 'none')).toEqual({ release: 'none' });
  });
});
```
Update every existing `computeScopeParams(url, env)` call in the file to pass a third `null` argument.

- [ ] **Step 2: Run `npx vitest run src/lib/api/scope.test.ts`, expect failures.**

- [ ] **Step 3: Implement `scope.ts`**

Extend the bridge:
```ts
export interface ScopeBridge {
  getCurrentEnvironmentId(): string | null;
  /** `null` = all releases; the literal `'none'` = rows with no release. */
  getCurrentRelease(): string | null;
}
const noopScopeBridge: ScopeBridge = {
  getCurrentEnvironmentId: () => null,
  getCurrentRelease: () => null,
};
export function currentRelease(): string | null {
  return bridge.getCurrentRelease();
}
```
Add, after `APP_CONFIG_SUBPATHS`:
```ts
// ---------------------------------------------------------------------------
// Release scoping is OPT-IN, and narrower than environment scoping: only the
// five searched list routes read `release`; the backend's `release_guard`
// middleware answers 400 to it everywhere else. This list mirrors
// `RELEASE_ACCEPTING_PATHS` in backend/bins/sauron-api/src/release_guard.rs
// and `release-scope-parity.test.ts` fails if the two drift.
export const RELEASE_SCOPED_URL: readonly RegExp[] = [
  /^\/v1\/apps\/[^/]+\/issues(?:\?.*)?$/,
  /^\/v1\/apps\/[^/]+\/issues\/[^/]+\/events(?:\?.*)?$/,
  /^\/v1\/apps\/[^/]+\/events\/list(?:\?.*)?$/,
  /^\/v1\/apps\/[^/]+\/sessions(?:\?.*)?$/,
  /^\/v1\/apps\/[^/]+\/transactions(?:\?.*)?$/,
];

export function shouldScopeRelease(url: string | undefined): boolean {
  if (!url) return false;
  return RELEASE_SCOPED_URL.some((re) => re.test(url));
}
```
Rewrite `computeScopeParams`:
```ts
export function computeScopeParams(
  url: string | undefined,
  envId: string | null,
  release: string | null,
): Record<string, string> | undefined {
  const out: Record<string, string> = {};
  if (shouldScopeUrl(url) && envId !== null) out.environment_id = envId;
  if (shouldScopeRelease(url) && release !== null) out.release = release;
  return Object.keys(out).length ? out : undefined;
}
```
`client.ts` interceptor: `computeScopeParams(config.url, currentEnvironmentId(), currentRelease())` and import `currentRelease`.

- [ ] **Step 4: `releases.ts` and the model**

```ts
import { api } from './client';
import type { AppRelease } from '../models';

/** Releases the caller may see for `appId`, newest `last_seen_at` first. */
export async function listReleases(appId: string): Promise<AppRelease[]> {
  const { data } = await api.get<AppRelease[]>(`/v1/apps/${appId}/releases`);
  return data;
}
```
In `models/index.ts` next to `AppEnvironment`:
```ts
export interface AppRelease {
  release: string;
  /** Enrollment ids that have seen this release; `null` = unattributed. */
  environment_ids: (string | null)[];
  first_seen_at: string;
  last_seen_at: string;
}
```

- [ ] **Step 5: Store**

In `session.svelte.ts`:
- Constant: `const RELEASE_KEY_PREFIX = 'sauron.release:';` and `const releaseKey = (appId: string) => RELEASE_KEY_PREFIX + appId;`.
- State, after `currentEnvId`:
  ```ts
  releases = $state<AppRelease[]>([]);
  releasesError = $state(false);
  // `null` = all releases; the literal `'none'` = rows with no release.
  // Persisted PER APP (`sauron.release:{appId}`), unlike the environment,
  // because a release name is meaningless across apps.
  currentRelease = $state<string | null>(null);
  ```
- Bridge: add `getCurrentRelease: () => this.currentRelease` to `configureScopeBridge`.
- `scopeKey`: `` `${this.currentAppId ?? ''}:${this.currentEnvId ?? 'all'}:${this.currentRelease ?? 'all'}` ``.
- New private `loadAppReleases(appId)` mirroring `loadAppEnvironments` (calls `listReleases`, sets `releasesError`, then `resolveCurrentRelease(appId)`):
  ```ts
  private resolveCurrentRelease(appId: string): void {
    const stored = readStored(releaseKey(appId));
    if (stored && (stored === 'none' || this.releases.some((r) => r.release === stored))) {
      this.currentRelease = stored;
      return;
    }
    this.currentRelease = null;
    writeStored(releaseKey(appId), null);
  }
  ```
- `setApp`: after `this.environments = [];` add `this.releases = []; this.currentRelease = null;` and after `await this.loadAppEnvironments(id);` add `await this.loadAppReleases(id);`. Do the same reset in `setOrg`/`setProject` where `currentEnvId` is cleared.
- Public:
  ```ts
  setRelease(id: string | null): void {
    this.currentRelease = id;
    if (this.currentAppId) writeStored(releaseKey(this.currentAppId), id);
    // A release narrows the env list (Topbar); if the current env is not in
    // it, fall back to "all" rather than sending an impossible pair.
    if (id !== null && this.currentEnvId !== null && this.currentEnvId !== 'none') {
      const r = this.releases.find((x) => x.release === id);
      if (r && !r.environment_ids.includes(this.currentEnvId)) this.setEnvironment(null);
    }
  }
  async ensureReleasesLoaded(): Promise<void> { /* mirror ensureEnvironmentsLoaded */ }
  ```
- Also call `loadAppReleases` wherever the store's initial `load()` calls `loadAppEnvironments` for the restored app.

- [ ] **Step 6: Store test**

Find the existing store test pattern (`grep -l "session.svelte" src/**/*.test.ts`). Add:
```ts
it('persists the release per app and clears it when the stored value is stale', async () => {
  // mock listReleases to return [{ release: '1.4.0', environment_ids: [null], ... }]
  window.localStorage.setItem('sauron.release:app-1', '1.4.0');
  await sessionStore.setApp('app-1');
  expect(sessionStore.currentRelease).toBe('1.4.0');
  window.localStorage.setItem('sauron.release:app-2', '9.9.9');
  await sessionStore.setApp('app-2');
  expect(sessionStore.currentRelease).toBeNull();
  expect(window.localStorage.getItem('sauron.release:app-2')).toBeNull();
});
```
Adapt mocking to how the existing test file stubs `listEnvironments` (vi.mock of `../api/environments`).

- [ ] **Step 7: Run `npx vitest run src/lib/api src/lib/stores` and `npx svelte-check`. Expect green.**

---

### Task 9: Topbar release switcher and i18n

**Files:**
- Modify: `dashboard/src/lib/components/layout/Topbar.svelte:94-98` (envItems) and `:170-178` (switcher markup)
- Modify: `dashboard/src/lib/i18n/catalog/nav.ts`

- [ ] **Step 1: i18n keys** in `nav.ts`, next to `nav.allEnvironments`:
```ts
  'nav.release': { en: 'Release', ar: 'الإصدار' },
  'nav.allReleases': { en: 'All releases', ar: 'كل الإصدارات' },
  'nav.unknownRelease': { en: 'Unknown release', ar: 'إصدار غير معروف' },
  'nav.switchRelease': { en: 'Switch release', ar: 'تبديل الإصدار' },
```

- [ ] **Step 2: Topbar script**

After `envItems`:
```ts
  // Release sits between App and Environment: picking one narrows the
  // environment list to enrollments that have seen it. `''` = all releases,
  // `'none'` = rows with no release, both mapped to the store's null/'none'.
  const releaseItems = $derived([
    { id: '', name: t('nav.allReleases') },
    ...sessionStore.releases.map((r) => ({ id: r.release, name: r.release })),
    { id: 'none', name: t('nav.unknownRelease') },
  ]);
  const visibleEnvs = $derived.by(() => {
    const sel = sessionStore.currentRelease;
    if (sel === null || sel === 'none') return sessionStore.environments;
    const r = sessionStore.releases.find((x) => x.release === sel);
    if (!r) return sessionStore.environments;
    return sessionStore.environments.filter((e) => r.environment_ids.includes(e.id));
  });
```
Change `envItems` to map over `visibleEnvs` instead of `sessionStore.environments`. Extend the existing `$effect` nudge so it also calls `ensureReleasesLoaded()` when `sessionStore.releases.length === 0`.

- [ ] **Step 3: Topbar markup**, between the app switcher and the env switcher:
```svelte
    {#if sessionStore.currentAppId}
      <span class="sep" aria-hidden="true">/</span>
      <SwitcherMenu
        label={t('nav.release')}
        items={releaseItems}
        currentId={sessionStore.currentRelease ?? ''}
        onSelect={(id) => sessionStore.setRelease(id === '' ? null : id)}
        ariaLabel={t('nav.switchRelease')}
      />
    {/if}
```

- [ ] **Step 4: Run `npx vitest run src/lib/i18n` (catalog + leak tests) and `npx svelte-check`. Expect green.**

---

### Task 10: "Showing all releases" note on rollup-backed pages

**Files:**
- Modify: `dashboard/src/lib/models/shell.ts` (add `RELEASE_AWARE`)
- Modify: `dashboard/src/lib/models/shell.test.ts`
- Create: `dashboard/src/lib/components/layout/ReleaseScopeNote.svelte`
- Modify: `dashboard/src/lib/components/layout/AppShell.svelte:147` (render above `{@render children()}`)
- Modify: `dashboard/src/lib/i18n/catalog/ui.ts`

- [ ] **Step 1: Failing parity test** in `shell.test.ts`:
```ts
import { RELEASE_AWARE } from './shell';
it('every PAGE_ACCESS key says whether it honors the release switcher', () => {
  for (const key of Object.keys(PAGE_ACCESS)) {
    expect(key in RELEASE_AWARE, `RELEASE_AWARE is missing '${key}'`).toBe(true);
  }
  for (const key of Object.keys(RELEASE_AWARE)) {
    expect(key in PAGE_ACCESS, `RELEASE_AWARE has stray key '${key}'`).toBe(true);
  }
});
it('exactly the five searched list pages are release-aware', () => {
  const aware = Object.entries(RELEASE_AWARE).filter(([, v]) => v).map(([k]) => k).sort();
  expect(aware).toEqual(['/events', '/issues', '/issues/:id', '/sessions', '/transactions'].sort());
});
```
Check `PAGE_ACCESS` for the exact keys of those five pages (the issue-detail key may be `/issues/:id` or similar) and fix the expected list to match.

- [ ] **Step 2: Run, expect failure.**

- [ ] **Step 3: `shell.ts`**
```ts
/**
 * Whether a page's data honors the topbar release switcher. `false` pages
 * read rollups keyed on (app, period, env) only, so while a release is
 * selected they show ALL releases and `ReleaseScopeNote` says so. Parity
 * tested against PAGE_ACCESS so a new page must decide.
 */
export const RELEASE_AWARE: Record<string, boolean> = {
  '/issues': true,
  '/issues/:id': true,
  '/events': true,
  '/sessions': true,
  '/transactions': true,
  // every other PAGE_ACCESS key: false
};
export function isReleaseAware(path: string): boolean {
  const key = findPageAccessKey(path);
  return key ? RELEASE_AWARE[key] === true : true;
}
```
Fill in every remaining key from `PAGE_ACCESS` with `false`.

- [ ] **Step 4: Component**
```svelte
<script lang="ts">
  import { location as routePath } from 'svelte-spa-router';
  import { sessionStore } from '../../stores/session.svelte';
  import { isReleaseAware } from '../../models/shell';
  import { t } from '../../i18n';

  const show = $derived(sessionStore.currentRelease !== null && !isReleaseAware($routePath));
</script>

{#if show}
  <p class="release-note muted" role="status">{t('ui.release.showingAll')}</p>
{/if}

<style>
  .release-note { margin: 0 0 8px; font-size: 12px; }
</style>
```
i18n in `ui.ts`: `'ui.release.showingAll': { en: 'Showing all releases — this page does not filter by release yet.', ar: 'يعرض كل الإصدارات — هذه الصفحة لا تُرشّح حسب الإصدار بعد.' },`

Render it in `AppShell.svelte` directly above `{@render children()}`. Confirm how `AppShell` reads the route (`routePath` import at line 8) and reuse that.

- [ ] **Step 5: Run `npx vitest run src/lib/models src/lib/i18n` and `npx svelte-check`. Expect green.**

---

### Task 11: Client ↔ server release allowlist parity test

**Files:**
- Create: `dashboard/src/lib/api/release-scope-parity.test.ts`

- [ ] **Step 1: Write the test**
```ts
import { describe, it, expect } from 'vitest';
import guardRs from '../../../../backend/bins/sauron-api/src/release_guard.rs?raw';
import { RELEASE_SCOPED_URL } from './scope';

function serverTemplates(): string[] {
  const block = /pub const RELEASE_ACCEPTING_PATHS: &\[&str\] = &\[([\s\S]*?)\n\];/.exec(guardRs);
  if (!block) throw new Error('RELEASE_ACCEPTING_PATHS not found in release_guard.rs — was it renamed?');
  return [...block[1].matchAll(/"([^"]+)"/g)].map((m) => m[1]);
}

/** `/v1/apps/{app_id}/issues/{issue_id}/events` → `/v1/apps/x/issues/y/events` */
function concrete(template: string): string {
  return template.replace(/\{[^}]+\}/g, 'x');
}

describe('release scoping parity', () => {
  it('every server-accepting route is client-scoped', () => {
    for (const tpl of serverTemplates()) {
      const url = concrete(tpl);
      expect(RELEASE_SCOPED_URL.some((re) => re.test(url)), `client does not scope ${tpl}`).toBe(true);
    }
  });
  it('the client scopes nothing the server rejects', () => {
    expect(RELEASE_SCOPED_URL.length).toBe(serverTemplates().length);
  });
});
```

- [ ] **Step 2: Run it; expect PASS. If the raw-import path is wrong, copy the relative depth from `filter-registry-parity.test.ts`.**

---

### Task 12: Browser drive of the dashboard

**Files:** none (verification only). Use the Browser pane, not Bash. The pane must be visible for clicks to register.

- [ ] **Step 1:** Boot the local stack per the local e2e recipe in memory (`local-e2e-api-drive`), run migration 78, and start the dashboard with `preview_start`.
- [ ] **Step 2:** Send two envelopes through ingest for the same app: one with `release: "1.4.0"`, one without. Wait for the worker.
- [ ] **Step 3:** In the topbar, confirm the Release switcher appears between App and Environment and lists `All releases`, `1.4.0`, `Unknown release`.
- [ ] **Step 4:** Pick `1.4.0`. Confirm via `read_network_requests` that the next issues/events list request carries `release=1.4.0`, and that the env list shrank to the enrollment that saw it.
- [ ] **Step 5:** Navigate to Overview. Confirm the "Showing all releases" note renders and that no request to `/overview` carries `release=`.
- [ ] **Step 6:** Pick `Unknown release`, open Events, confirm only the release-less event shows.
- [ ] **Step 7:** Switch locale to Arabic and confirm the three new strings render translated. Screenshot both states and report.

---

### Task 13: JS SDK — release required at init

**Files:**
- Modify: `sdks/js/src/client.ts:399-406`, `sdks/js/src/types.ts:333` (doc: required), `sdks/js/src/utils.ts:5`, `sdks/js/package.json:3`, `sdks/js/README.md`, `sdks/js/CHANGELOG.md`
- Test: `sdks/js/test/init.test.ts` (create), `sdks/js/test/envelope.test.ts:116`
- Also: `wiki/Browser-SDK.md:4`, `wiki/Capabilities.md` row

- [ ] **Step 1: Failing test** `sdks/js/test/init.test.ts`:
```ts
import { describe, it, expect } from 'vitest';
import { Sauron } from '../src';

describe('init release validation', () => {
  it('throws without a release', () => {
    expect(() => Sauron.init({ dsn: 'https://pk_test@localhost:8081/1' } as never)).toThrow(/requires a `release`/);
  });
  it('throws on a whitespace-only release', () => {
    expect(() => Sauron.init({ dsn: 'https://pk_test@localhost:8081/1', release: '   ' })).toThrow(/requires a `release`/);
  });
  it('trims the release it sends', () => {
    // Use whatever capture transport envelope.test.ts uses; assert
    // built.header.release === '1.4.2' after init({ release: ' 1.4.2 ' }).
  });
});
```
Fill the third case using the existing envelope-capture helper in `envelope.test.ts`.

- [ ] **Step 2: Run `cd sdks/js && npx vitest run test/init.test.ts`, expect failure.**

- [ ] **Step 3: Implement** in `resolveOptions`:
```ts
  if (typeof options.release !== 'string' || options.release.trim().length === 0) {
    throw new Error('[sauron] init() requires a `release` (the app version this build reports as)');
  }
  ...
    release: options.release.trim(),
```
`types.ts:333`: `release: string;` with a doc comment "Required. The app version every event is attributed to."

- [ ] **Step 4: Version 1.7.0** — `package.json`, `utils.ts` `SDK_VERSION`, `envelope.test.ts:116` expectation, README snippets (all three `init` examples already show `release`; add one line saying it is required), CHANGELOG entry "1.7.0 — `release` is required at init; init throws without it." `wiki/Browser-SDK.md` version sentence and the `wiki/Capabilities.md` row.

- [ ] **Step 5: Run the whole JS suite and `cd dashboard && npx vitest run src/lib/models/wiki-sdk-versions.test.ts`. Expect green.**

---

### Task 14: Node SDK — release required at init

**Files:** `sdks/node/src/client.ts:46-54`, `sdks/node/src/types.ts:359`, `sdks/node/src/transport.ts:14`, `sdks/node/package.json`, `README.md`, `CHANGELOG.md`; tests `sdks/node/test/index.test.ts` (append), `test/transport.test.ts:67`, `test/envelope.test.ts:223`; `wiki/Node-SDK.md:3`, `wiki/Capabilities.md`.

- [ ] **Step 1: Failing tests** in `index.test.ts` next to the invalid-DSN case:
```ts
  it('throws without a release', () => {
    expect(() => init({ dsn: 'https://pk_test@localhost:8081/1', flushInterval: 0 } as never)).toThrow(/requires a release/);
  });
  it('throws on an empty dsn', () => {
    expect(() => init({ dsn: '', release: '1.0.0', flushInterval: 0 })).toThrow();
  });
```
- [ ] **Step 2: Run, expect failure.**
- [ ] **Step 3: Implement** in `resolveOptions`:
```ts
  if (!options || typeof options.dsn !== 'string' || options.dsn.length === 0) {
    throw new Error('[sauron] init requires a { dsn } option');
  }
  if (typeof options.release !== 'string' || options.release.trim().length === 0) {
    throw new Error('[sauron] init requires a { release } option (the app version this build reports as)');
  }
  ...
    release: options.release.trim(),
```
`types.ts:359`: `release: string;` required. Fix every `init({ dsn })` call in `sdks/node/test/**` to pass `release`.
- [ ] **Step 4: Version 1.6.0** across manifest, constant, the two version assertions, README, CHANGELOG, wiki sentence and Capabilities row.
- [ ] **Step 5: Full Node suite + `wiki-sdk-versions.test.ts`. Expect green.**

---

### Task 15: Python SDK — release required when a DSN is present

**Files:** `sdks/python/sauron/__init__.py:89-145`, `sdks/python/sauron/_client.py:33`, `pyproject.toml:7`, `README.md`, `CHANGELOG.md`; tests `sdks/python/tests/test_init.py` (create), `tests/test_envelope.py:47`, `tests/test_golden.py:161-164`; `wiki/Python-SDK.md:3`, `wiki/Capabilities.md`.

- [ ] **Step 1: Failing tests** `tests/test_init.py`:
```python
import unittest
import sauron

class InitReleaseTests(unittest.TestCase):
    def tearDown(self):
        sauron.init(None)  # back to disabled

    def test_missing_release_raises(self):
        with self.assertRaises(ValueError):
            sauron.init("https://pk_test@localhost:8081/1")

    def test_blank_release_raises(self):
        with self.assertRaises(ValueError):
            sauron.init("https://pk_test@localhost:8081/1", release="  ")

    def test_empty_dsn_still_disables_without_release(self):
        self.assertIsNone(sauron.init(""))

    def test_release_is_trimmed(self):
        client = sauron.init("https://pk_test@localhost:8081/1", release=" 1.0.0 ")
        self.assertEqual(client.release, "1.0.0")
```
- [ ] **Step 2: `cd sdks/python && python -m pytest tests/test_init.py`, expect failure.**
- [ ] **Step 3: Implement** in `init`, after the `if not dsn:` block:
```python
    if release is None or not str(release).strip():
        raise ValueError(
            "sauron.init() requires release= (the app version this build reports as)"
        )
    release = str(release).strip()
```
Update the docstring. Fix every `init(dsn)` in `tests/` to pass `release=`.
- [ ] **Step 4: Version 1.6.0**: `pyproject.toml`, `_client.py` `SDK_VERSION`, `test_envelope.py:47`, `test_golden.py:161,164`, README, CHANGELOG, wiki sentence, Capabilities row.
- [ ] **Step 5: Full Python suite + `wiki-sdk-versions.test.ts`. Expect green.**

---

### Task 16: Flutter SDK — release required when a DSN is set

**Files:** `sdks/flutter/lib/src/sauron.dart:47-61`, `sdks/flutter/lib/src/sauron_options.dart`, `lib/src/envelope.dart:9`, `pubspec.yaml:6`, `README.md`, `CHANGELOG.md`; tests `test/init_test.dart`; `wiki/Flutter-SDK.md:3`, `wiki/Capabilities.md`.

- [ ] **Step 1: Failing tests** in `init_test.dart`:
```dart
  test('a dsn without a release throws at init', () async {
    expect(
      () => Sauron.init(SauronOptions(dsn: 'https://pk_test@localhost:8081/1', httpClient: httpClient)),
      throwsA(isA<ArgumentError>()),
    );
  });

  test('a blank release throws at init', () async {
    expect(
      () => Sauron.init(SauronOptions(dsn: 'https://pk_test@localhost:8081/1', release: '  ', httpClient: httpClient)),
      throwsA(isA<ArgumentError>()),
    );
  });

  test('an empty dsn still leaves the SDK disabled without a release', () async {
    await Sauron.init(SauronOptions(httpClient: httpClient));
    expect(Sauron.isEnabled, isFalse);
  });
```
- [ ] **Step 2: `cd sdks/flutter && flutter test test/init_test.dart`, expect failure.**
- [ ] **Step 3: Implement** at the top of `Sauron.init`:
```dart
    if (options.isConfigured && (options.release ?? '').trim().isEmpty) {
      throw ArgumentError.value(options.release, 'release',
          'SauronOptions.release is required when a dsn is set (the app version this build reports as)');
    }
    options.release = options.release?.trim();
```
Check `release` is a mutable field on `SauronOptions`; if final, trim in `_buildHeader` instead. Update the `release` doc comment to say required. Fix every `SauronOptions(dsn: …)` in `test/` to pass `release`.
- [ ] **Step 4: Version 1.10.0**: `pubspec.yaml`, `kSauronSdkVersion`, any test asserting `1.9.0`, README, CHANGELOG, wiki sentence, Capabilities row.
- [ ] **Step 5: `flutter test` full + `wiki-sdk-versions.test.ts`. Expect green.**

---

### Task 17: C# SDK — release required

**Files:** `sdks/csharp/Sauron/SauronClient.cs:9-20,124-142`, `sdks/csharp/Sauron/Envelope.cs:41`, `Sauron.csproj:10`, `README.md`, `CHANGELOG.md`; tests `Sauron.Tests/InitTests.cs` (create), `TransportTests.cs:64`, `EnvelopeGoldenTests.cs:28,239`; `wiki/CSharp-SDK.md:3`, `wiki/Capabilities.md`.

- [ ] **Step 1: Failing tests** `InitTests.cs`:
```csharp
using Xunit;

public class InitTests
{
    [Theory]
    [InlineData(null)]
    [InlineData("")]
    [InlineData("   ")]
    public void MissingRelease_Throws(string? release)
    {
        var ex = Assert.Throws<ArgumentException>(() => new SauronClient(new SauronOptions
        {
            Dsn = "https://pk_test@localhost:8081/1",
            Release = release,
            HttpMessageHandler = new CapturingHandler(),
        }));
        Assert.Contains("Release", ex.Message);
    }

    [Fact]
    public void InvalidDsn_StillDisablesWithoutThrowing()
    {
        using var client = new SauronClient(new SauronOptions { Dsn = "not-a-dsn", Release = "1.0.0", HttpMessageHandler = new CapturingHandler() });
        Assert.False(client.Enabled);
    }
}
```
- [ ] **Step 2: `dotnet test sdks/csharp/Sauron.Tests/Sauron.Tests.csproj --filter FullyQualifiedName~InitTests`, expect failure.**
- [ ] **Step 3: Implement** in the ctor, before `Dsn.Parse`:
```csharp
        if (string.IsNullOrWhiteSpace(options.Release))
            throw new ArgumentException("SauronOptions.Release is required (the app version this build reports as).", nameof(options));
        options.Release = options.Release.Trim();
```
Doc comment on `Release` says required. Fix every `new SauronOptions { Dsn = … }` in tests to set `Release`.
- [ ] **Step 4: Version 1.6.0**: csproj, `Envelope.Version`, the three version assertions, README, CHANGELOG, wiki sentence, Capabilities row.
- [ ] **Step 5: Full `dotnet test` + `wiki-sdk-versions.test.ts`. Expect green.**

---

### Task 18: Publishing notes and wire-contract docs

**Files:** `sdks/PUBLISHING.md:32-40`, `wiki/Ingest-Wire-Contract.md`

- [ ] **Step 1:** Rewrite the `Current versions` table in `PUBLISHING.md` with the new versions (JS 1.7.0, Node 1.6.0, Python 1.6.0, C# 1.6.0, Flutter 1.10.0). Add a line under the bump checklist: "Update this table too; nothing asserts it."
- [ ] **Step 2:** In `wiki/Ingest-Wire-Contract.md`, at the `release` header field, add: "Optional on the wire (older SDKs may omit it; the server stores NULL and the dashboard shows it as Unknown release). Required at init by every SDK from JS 1.7.0 / Node 1.6.0 / Python 1.6.0 / Flutter 1.10.0 / C# 1.6.0."
- [ ] **Step 3:** `cd dashboard && npx vitest run src/lib/models/wiki-sdk-versions.test.ts` — the third assertion forbids the phrase "all five ship as"; keep it out.

---

### Task 19: Final verification

- [ ] `cd backend && cargo +1.98.0 clippy --all-targets -- -D warnings && cargo fmt --check`
- [ ] Backend DB and API suites with `TEST_DATABASE_URL` and `TEST_REDIS_URL` set, from a normal shell: `cargo test -p sauron-db -p sauron-query -p sauron-pipeline -p sauron-api`. Confirm non-zero durations on the DB test files.
- [ ] `cd dashboard && npx vitest run && npx svelte-check && npm run lint`
- [ ] Each SDK suite (Tasks 13–17 commands).
- [ ] Report: what changed, every test command with its result, and the two screenshots from Task 12. Do not commit.
