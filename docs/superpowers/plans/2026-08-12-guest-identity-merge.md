# Guest → Identified Person Merge Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** When a guest signs up, fold their entire anonymous history into the identified person so one human is counted once.

**Architecture:** `identities` becomes the source of truth for identity resolution (it is currently write-only dead storage). A fresh alias claim enqueues an `identity_merges` row; a background worker in `sauron-ingest` rewrites the guest's hot rows in place to the person's `distinct_id`, stashing the old value in a new `guest_alias` column, then folds the two rollup tables by *moving* rows so the fold is idempotent. Cold Parquet is immutable, so the alias map is applied as a bounded overlay in the DuckDB path only.

**Tech Stack:** Rust (diesel-async / tokio / axum), PostgreSQL 15+ (declaratively partitioned event tables), DuckDB over Parquet, TypeScript (vitest), Dart/Flutter.

**Spec:** `docs/superpowers/specs/2026-08-12-guest-identity-merge-design.md`

## Global Constraints

- **NEVER commit and NEVER create branches.** This repository's standing rule. Every task ends with a verification step, not a commit. Leave changes in the working tree.
- **Backend DB tests silently skip under the Bash sandbox** (its own netns makes every DB-backed test return early while printing `ok`). Every `cargo test` below MUST run with `dangerouslyDisableSandbox: true` against host-network containers, and the pass count MUST be compared against the pre-change baseline. A green run is not evidence on its own.
- **Enum-like columns are `TEXT` + `CHECK`, never a custom SQL type.** House rule.
- **New columns in `schema.rs` are APPENDED to a table's column list, never inserted mid-list.** `models::*` derive `Queryable`, which decodes positionally, and `ALTER TABLE … ADD COLUMN` appends physically. A field inserted in the middle silently binds every later column to the wrong one.
- **Every `ON CONFLICT` against `event_user_environments` must name the expression `(app_id, distinct_id, COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid))`** — the unique key is an expression index, and naming anything else silently degrades to an unconstrained insert.
- **Migration number is 58.** 57 (`data_purge`) is taken by concurrent work.
- MSRV is 1.82 — no async closures. Use explicit `BEGIN`/`COMMIT` via `batch_execute`, matching `person_env_backfill::backfill_app`.

## File Structure

| File | Responsibility |
|---|---|
| `backend/migrations/2026-08-12-000058_identity_merge/{up,down}.sql` | `guest_alias` columns + `identity_merges` queue |
| `backend/crates/sauron-db/src/schema.rs` | diesel table definitions (append-only) |
| `backend/crates/sauron-db/src/identity_merge.rs` | **new** — claim, enqueue, rewrites, folds, cold map. Kept out of the 16k-line `repo.rs`, following the `person_env_backfill.rs` precedent |
| `backend/crates/sauron-db/src/repo.rs` | delete `insert_identity` (superseded by `claim_identity`) |
| `backend/crates/sauron-pipeline/src/process.rs` | single-item identify path → claim + enqueue |
| `backend/crates/sauron-pipeline/src/batch.rs` | batched identify path → claim + enqueue |
| `backend/crates/sauron-pipeline/src/merge.rs` | **new** — the drain loop |
| `backend/bins/sauron-ingest/src/main.rs` | spawn the merge worker |
| `backend/crates/sauron-tier/src/duck.rs` | alias-map temp table + resolved cold scan |
| `backend/crates/sauron-db/tests/identity_merge.rs` | **new** — merge correctness |
| `backend/crates/sauron-db/tests/identity_merge_cold.rs` | **new** — overlay + pruning |
| `sdks/js/src/{identity,client}.ts`, `src/api/product.ts` | auto-reset on switch, session rotation |
| `sdks/flutter/lib/src/client.dart` | same, for Flutter |

---

### Task 1: Migration 58 — schema

**Files:**
- Create: `backend/migrations/2026-08-12-000058_identity_merge/up.sql`
- Create: `backend/migrations/2026-08-12-000058_identity_merge/down.sql`
- Modify: `backend/crates/sauron-db/src/schema.rs`

**Interfaces:**
- Produces: tables `identity_merges`; columns `analytics_events.guest_alias`, `error_events.guest_alias`. diesel modules `identity_merges`, plus appended `guest_alias -> Nullable<Text>` on the two event tables.

- [ ] **Step 1: Write the failing test**

Create `backend/crates/sauron-db/tests/identity_merge.rs`:

```rust
//! Guest → identified merge. See
//! `docs/superpowers/specs/2026-08-12-guest-identity-merge-design.md`.

mod common;

use common::TestDb;
use diesel::prelude::*;
use diesel::sql_types::{Text, Uuid as SqlUuid};
use diesel_async::RunQueryDsl;

#[derive(QueryableByName)]
struct Count {
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    n: i64,
}

/// Migration 58 must add the derived pre-login marker to both event tables.
/// It is nullable with no default so the ADD COLUMN is metadata-only.
#[tokio::test]
async fn migration_058_adds_guest_alias_to_both_event_tables() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;

    let row: Count = diesel::sql_query(
        "SELECT count(*)::bigint AS n FROM information_schema.columns \
          WHERE table_name IN ('analytics_events','error_events') \
            AND column_name = 'guest_alias' AND is_nullable = 'YES'",
    )
    .get_result(&mut conn)
    .await
    .expect("column probe");

    assert_eq!(row.n, 2, "guest_alias must exist and be nullable on both event tables");

    drop(conn);
    db.cleanup().await;
}

/// The queue must reject a second row for the same alias, so a redelivered
/// identify() cannot schedule the same merge twice.
#[tokio::test]
async fn identity_merges_is_unique_per_alias() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let stmt = || {
        diesel::sql_query(
            "INSERT INTO identity_merges (app_id, alias_id, distinct_id) \
             VALUES ($1, 'anon_x', 'u-42')",
        )
        .bind::<SqlUuid, _>(ids.app_id)
    };

    stmt().execute(&mut conn).await.expect("first enqueue");
    assert!(
        stmt().execute(&mut conn).await.is_err(),
        "a second queue row for the same alias must be rejected"
    );

    drop(conn);
    db.cleanup().await;
}

/// `state` is TEXT + CHECK — house rule, never a custom SQL type.
#[tokio::test]
async fn identity_merges_rejects_an_unknown_state() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let bad = diesel::sql_query(
        "INSERT INTO identity_merges (app_id, alias_id, distinct_id, state) \
         VALUES ($1, 'anon_y', 'u-42', 'sideways')",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .execute(&mut conn)
    .await;

    assert!(bad.is_err(), "the CHECK constraint must reject an unknown state");

    drop(conn);
    db.cleanup().await;
}
```

Import only what this task's tests actually bind — `Text` is not used until Task 2, so leave it out of the `use` list for now and add it then. Do not add a placeholder to keep an unused import alive.

- [ ] **Step 2: Run test to verify it fails**

Run (with `dangerouslyDisableSandbox: true`):
```bash
cd backend && cargo test -p sauron-db --test identity_merge -- --nocapture
```
Expected: FAIL — `relation "identity_merges" does not exist`, and the column probe returns 0.

- [ ] **Step 3: Write the migration**

Create `backend/migrations/2026-08-12-000058_identity_merge/up.sql`:

```sql
-- 0058: guest → identified person merge.
--
-- MUST RUN BEFORE RESTARTING sauron-ingest.
-- Without it the merge worker logs one ERROR at boot and then does nothing:
-- every identify() is still recorded, but no merge is ever enqueued, so the
-- guest/identified double-count this feature exists to remove stays in place.
-- Nothing fails, no request errors, and the dashboard looks exactly as it does
-- today — which is precisely why this warning is here.
--
-- See docs/superpowers/specs/2026-08-12-guest-identity-merge-design.md

-- The derived pre-login marker. Nullable with no default, so ADD COLUMN is
-- metadata-only and every existing row reads NULL without a rewrite. An event
-- happened pre-login iff `guest_alias IS NOT NULL`; nothing is written at
-- ingest, only by the merge job.
--
-- Deliberately NOT added to sessions/transactions/workflows: those get their
-- distinct_id rewritten like everything else, but "was this pre-login" is a
-- question about events. Easy to add later, hard to remove.
ALTER TABLE analytics_events ADD COLUMN guest_alias TEXT;
ALTER TABLE error_events     ADD COLUMN guest_alias TEXT;

-- The merge work queue.
--
-- A dedicated table rather than state columns on `identities`, because
-- `identities` is a pure map read on the hot cold-overlay path and must stay
-- narrow. UNIQUE (app_id, alias_id) makes enqueueing idempotent under
-- redelivery: the alias is claimed exactly once, so it is scheduled exactly
-- once.
--
-- alias_first_seen/alias_last_seen/cold_stale are NULL/TRUE until the
-- `event_users` fold fills them (that fold already reads the row in order to
-- delete it). Until then the cold overlay MUST NOT prune on them — see the
-- selection query in sauron-db/src/identity_merge.rs::cold_alias_map.
CREATE TABLE identity_merges (
    id                UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    app_id            UUID NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    alias_id          TEXT NOT NULL,
    distinct_id       TEXT NOT NULL,
    state             TEXT NOT NULL DEFAULT 'pending'
                      CHECK (state IN ('pending', 'running', 'done', 'failed')),
    attempts          INT  NOT NULL DEFAULT 0,
    last_error        TEXT,
    alias_first_seen  TIMESTAMPTZ,
    alias_last_seen   TIMESTAMPTZ,
    cold_stale        BOOLEAN NOT NULL DEFAULT TRUE,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at      TIMESTAMPTZ,
    UNIQUE (app_id, alias_id)
);

-- The drain's claim query. Partial, because the queue is overwhelmingly 'done'
-- once steady state is reached and only the runnable tail is ever scanned.
CREATE INDEX identity_merges_runnable_idx
    ON identity_merges (created_at)
    WHERE state IN ('pending', 'failed');

-- The cold overlay reads by app and by span.
CREATE INDEX identity_merges_app_span_idx
    ON identity_merges (app_id, alias_last_seen);
```

Create `down.sql`:

```sql
DROP INDEX IF EXISTS identity_merges_app_span_idx;
DROP INDEX IF EXISTS identity_merges_runnable_idx;
DROP TABLE IF EXISTS identity_merges;
ALTER TABLE error_events     DROP COLUMN IF EXISTS guest_alias;
ALTER TABLE analytics_events DROP COLUMN IF EXISTS guest_alias;
```

- [ ] **Step 4: Update `schema.rs`**

In `backend/crates/sauron-db/src/schema.rs`, **append** to the END of the `analytics_events` and `error_events` column lists (positional decode — see Global Constraints):

```rust
        guest_alias -> Nullable<Text>,
```

Add the new table module in alphabetical position among the other `diesel::table!` entries:

```rust
    identity_merges (id) {
        id -> Uuid,
        app_id -> Uuid,
        alias_id -> Text,
        distinct_id -> Text,
        state -> Text,
        attempts -> Integer,
        last_error -> Nullable<Text>,
        alias_first_seen -> Nullable<Timestamptz>,
        alias_last_seen -> Nullable<Timestamptz>,
        cold_stale -> Bool,
        created_at -> Timestamptz,
        completed_at -> Nullable<Timestamptz>,
    }
```

Add to the `allow_tables_to_appear_in_same_query!` list and add `diesel::joinable!(identity_merges -> apps (app_id));` beside the other `joinable!` entries.

- [ ] **Step 5: Run tests to verify they pass**

```bash
cd backend && cargo test -p sauron-db --test identity_merge --test schema_drift -- --nocapture
```
Expected: PASS, 3 tests in `identity_merge`, and `schema_drift` still green (it is what catches a `schema.rs` that disagrees with the migrations).

- [ ] **Step 6: Verify — do NOT commit**

```bash
cd backend && cargo fmt --all && cargo clippy -p sauron-db --all-targets -- -D warnings
```
Leave the changes in the working tree.

---

### Task 2: `claim_identity` — burn, chain rejection, conflict detection

Replaces `repo::insert_identity`, which cannot distinguish "already claimed by the same user" (benign) from "already claimed by someone else" (a real conflict) — both return zero rows today.

**Files:**
- Create: `backend/crates/sauron-db/src/identity_merge.rs`
- Modify: `backend/crates/sauron-db/src/lib.rs` (add `pub mod identity_merge;`)
- Modify: `backend/crates/sauron-db/src/repo.rs` (delete `insert_identity`, ~line 5213)
- Test: `backend/crates/sauron-db/tests/identity_merge.rs`

**Interfaces:**
- Produces:
  ```rust
  pub enum Claim {
      Fresh,                       // first claim — caller must enqueue a merge
      Repeat,                      // same alias, same person — benign
      Conflict { existing: String },// same alias, DIFFERENT person — burned
      Chain,                       // alias is already a target, or target is already an alias
  }
  pub async fn claim_identity(
      conn: &mut AsyncPgConnection, app_id: Uuid, alias_id: &str, distinct_id: &str,
  ) -> QueryResult<Claim>;
  ```

- [ ] **Step 1: Write the failing tests**

Append to `backend/crates/sauron-db/tests/identity_merge.rs`:

```rust
use sauron_db::identity_merge::{claim_identity, Claim};

#[tokio::test]
async fn first_claim_is_fresh_and_a_repeat_is_not() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let first = claim_identity(&mut conn, ids.app_id, "anon_x", "u-42").await.unwrap();
    assert!(matches!(first, Claim::Fresh), "first claim must be Fresh, got {first:?}");

    let again = claim_identity(&mut conn, ids.app_id, "anon_x", "u-42").await.unwrap();
    assert!(matches!(again, Claim::Repeat), "same user re-identifying is benign, got {again:?}");

    drop(conn);
    db.cleanup().await;
}

/// The burn rule: an alias is claimed once and NEVER re-pointed. A second
/// identify() from a different user must be reported as a conflict — that is
/// the only signal anyone ever gets that an app forgot reset() on logout.
#[tokio::test]
async fn a_second_user_cannot_repoint_a_burned_alias() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    claim_identity(&mut conn, ids.app_id, "anon_shared", "ahmed").await.unwrap();
    let sara = claim_identity(&mut conn, ids.app_id, "anon_shared", "sara").await.unwrap();

    match sara {
        Claim::Conflict { existing } => assert_eq!(existing, "ahmed"),
        other => panic!("expected Conflict{{existing: ahmed}}, got {other:?}"),
    }

    let stored: Vec<String> = diesel::sql_query(
        "SELECT distinct_id FROM identities WHERE app_id = $1 AND alias_id = 'anon_shared'",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .load::<Target>(&mut conn)
    .await
    .unwrap()
    .into_iter()
    .map(|t| t.distinct_id)
    .collect();
    assert_eq!(stored, vec!["ahmed".to_string()], "the alias must not be re-pointed");

    drop(conn);
    db.cleanup().await;
}

/// No chains. resolve() must be single-level and idempotent — that property is
/// what makes the cold overlay correct whether a Parquet file was written
/// before or after a merge.
#[tokio::test]
async fn a_target_cannot_become_an_alias_and_vice_versa() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    claim_identity(&mut conn, ids.app_id, "anon_x", "u-42").await.unwrap();

    // u-42 is already a target, so it may not become an alias.
    let forward = claim_identity(&mut conn, ids.app_id, "u-42", "u-99").await.unwrap();
    assert!(matches!(forward, Claim::Chain), "u-42 → u-99 must be refused, got {forward:?}");

    // anon_x is already an alias, so it may not become a target.
    let backward = claim_identity(&mut conn, ids.app_id, "anon_z", "anon_x").await.unwrap();
    assert!(matches!(backward, Claim::Chain), "… → anon_x must be refused, got {backward:?}");

    drop(conn);
    db.cleanup().await;
}

#[derive(QueryableByName)]
struct Target {
    #[diesel(sql_type = Text)]
    distinct_id: String,
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cd backend && cargo test -p sauron-db --test identity_merge -- --nocapture
```
Expected: FAIL to compile — `unresolved import sauron_db::identity_merge`.

- [ ] **Step 3: Implement `claim_identity`**

Create `backend/crates/sauron-db/src/identity_merge.rs`:

```rust
//! Guest → identified person merge: alias claiming, the work queue, the hot
//! rewrites, the rollup folds and the bounded cold-overlay map.
//!
//! Kept out of `repo.rs` for the same reason `person_env_backfill` is: this is
//! a self-contained subsystem with one entry point per phase, and `repo.rs` is
//! already past 16k lines.
//!
//! See `docs/superpowers/specs/2026-08-12-guest-identity-merge-design.md`.

use diesel::prelude::*;
use diesel::sql_types::{Bool, Text, Uuid as SqlUuid};
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use uuid::Uuid;

/// The outcome of trying to bind an anonymous id to a named person.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Claim {
    /// First claim. The caller MUST enqueue a merge.
    Fresh,
    /// The same person re-identifying under the same alias. Benign, common
    /// (every page load after login can emit one) — do nothing.
    Repeat,
    /// The alias is already burned to a DIFFERENT person. Not re-pointed.
    ///
    /// This is the shared-device case, and it is the only externally visible
    /// symptom of an app that never calls `reset()` on logout. Callers log and
    /// count it; without that the safety behaviour is indistinguishable from
    /// the feature being broken.
    Conflict { existing: String },
    /// Refused because it would create a chain (`a → b → c`). Keeping the map
    /// single-level is what makes `resolve()` idempotent, which in turn is what
    /// lets the cold overlay be applied to a Parquet file without caring
    /// whether it was written before or after the merge.
    Chain,
}

#[derive(QueryableByName)]
struct ClaimRow {
    #[diesel(sql_type = Text)]
    distinct_id: String,
    #[diesel(sql_type = Bool)]
    inserted: bool,
}

/// Bind `alias_id` to `distinct_id`, once and for all.
///
/// One statement, three outcomes, no read-then-write race:
///
/// * one row with `inserted = true`  → `Fresh`
/// * one row with `inserted = false` → the alias existed; compare the target
/// * **zero rows**                   → a guard `NOT EXISTS` filtered the
///   `SELECT`, i.e. the insert would have formed a chain
///
/// `RETURNING (xmax = 0) AS inserted` is the house pattern already used by
/// `repo::bump_session`. The `DO UPDATE SET distinct_id = identities.distinct_id`
/// is a deliberate no-op write: `DO NOTHING` would return zero rows on
/// conflict, collapsing the "burned" and "chain" cases into one.
///
/// Both `NOT EXISTS` legs are indexed — `identities_app_distinct_idx`
/// (migration 38) covers the first, the `UNIQUE (app_id, alias_id)` the second.
pub async fn claim_identity(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    alias_id: &str,
    distinct_id: &str,
) -> QueryResult<Claim> {
    let rows: Vec<ClaimRow> = diesel::sql_query(
        "INSERT INTO identities (app_id, alias_id, distinct_id) \
         SELECT $1, $2, $3 \
          WHERE NOT EXISTS (SELECT 1 FROM identities \
                             WHERE app_id = $1 AND distinct_id = $2) \
            AND NOT EXISTS (SELECT 1 FROM identities \
                             WHERE app_id = $1 AND alias_id = $3) \
         ON CONFLICT (app_id, alias_id) \
         DO UPDATE SET distinct_id = identities.distinct_id \
         RETURNING distinct_id, (xmax = 0) AS inserted",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Text, _>(alias_id)
    .bind::<Text, _>(distinct_id)
    .load(conn)
    .await?;

    Ok(match rows.into_iter().next() {
        None => Claim::Chain,
        Some(r) if r.inserted => Claim::Fresh,
        Some(r) if r.distinct_id == distinct_id => Claim::Repeat,
        Some(r) => Claim::Conflict { existing: r.distinct_id },
    })
}
```

Add to `backend/crates/sauron-db/src/lib.rs`, beside `pub mod person_env_backfill;`:

```rust
pub mod identity_merge;
```

Delete `repo::insert_identity` (`backend/crates/sauron-db/src/repo.rs`, the `pub async fn insert_identity` block). Its two call sites are replaced in Task 3.

- [ ] **Step 4: Run tests to verify they pass**

```bash
cd backend && cargo test -p sauron-db --test identity_merge -- --nocapture
```
Expected: PASS — 6 tests.

- [ ] **Step 5: Verify — do NOT commit**

```bash
cd backend && cargo fmt --all && cargo clippy -p sauron-db --all-targets -- -D warnings
```

---

### Task 3: Enqueue from BOTH identify paths

> **This is the single most likely way to ship this feature broken.** `insert_identity` had two independent callers — `process::process_identify` and the batched loop in `batch.rs`. Wiring only one leaves merges silently not happening on whichever path the deployment actually uses, with no error anywhere. Each path gets its own test; neither may be asserted by proxy.

**Files:**
- Modify: `backend/crates/sauron-db/src/identity_merge.rs`
- Modify: `backend/crates/sauron-pipeline/src/process.rs:606-610`
- Modify: `backend/crates/sauron-pipeline/src/batch.rs:719-724`
- Test: `backend/crates/sauron-pipeline/src/process.rs` (in-file `mod tests`, matching the existing `process_identify_marks_the_user_identified`)

**Interfaces:**
- Consumes: `Claim`, `claim_identity` (Task 2)
- Produces: `pub async fn enqueue_merge(conn, app_id, alias_id, distinct_id) -> QueryResult<usize>`

- [ ] **Step 1: Write the failing tests**

Add to the existing `mod tests` in `backend/crates/sauron-pipeline/src/process.rs`:

```rust
    #[derive(QueryableByName)]
    struct QueuedMerge {
        #[diesel(sql_type = diesel::sql_types::Text)]
        alias_id: String,
        #[diesel(sql_type = diesel::sql_types::Text)]
        distinct_id: String,
    }

    async fn queued(conn: &mut AsyncPgConnection, app_id: Uuid) -> Vec<QueuedMerge> {
        diesel::sql_query(
            "SELECT alias_id, distinct_id FROM identity_merges \
              WHERE app_id = $1 ORDER BY alias_id",
        )
        .bind::<SqlUuid, _>(app_id)
        .load(conn)
        .await
        .expect("queued merges")
    }

    /// The single-item path must enqueue a merge on a fresh claim.
    #[tokio::test]
    async fn process_identify_enqueues_a_merge() {
        let Some(db) = TestDb::setup().await else { return };
        let ids = db.seed_two_envs().await;
        let mut conn = db.conn().await;
        let job = job_for(ids.app_id);

        process_identify(
            &mut conn,
            &job,
            IdentifyItem {
                distinct_id: "u-42".into(),
                anonymous_id: Some("anon_single".into()),
                traits: serde_json::json!({}),
                timestamp: Utc::now(),
            },
        )
        .await
        .expect("process identify");

        let q = queued(&mut conn, ids.app_id).await;
        assert_eq!(q.len(), 1, "the single-item path must enqueue exactly one merge");
        assert_eq!(q[0].alias_id, "anon_single");
        assert_eq!(q[0].distinct_id, "u-42");

        drop(conn);
        db.cleanup().await;
    }

    /// A repeat identify() must NOT enqueue a second merge.
    #[tokio::test]
    async fn a_repeat_identify_does_not_enqueue_twice() {
        let Some(db) = TestDb::setup().await else { return };
        let ids = db.seed_two_envs().await;
        let mut conn = db.conn().await;
        let job = job_for(ids.app_id);

        for _ in 0..2 {
            process_identify(
                &mut conn,
                &job,
                IdentifyItem {
                    distinct_id: "u-42".into(),
                    anonymous_id: Some("anon_twice".into()),
                    traits: serde_json::json!({}),
                    timestamp: Utc::now(),
                },
            )
            .await
            .expect("process identify");
        }

        assert_eq!(queued(&mut conn, ids.app_id).await.len(), 1);

        drop(conn);
        db.cleanup().await;
    }
```

`batch.rs` has **no** in-file `mod tests`, so the batched path needs its own integration test. Create `backend/crates/sauron-pipeline/tests/identity_merge_batch.rs`, following the conventions in the existing `tests/retry_drain.rs` (reads `TEST_DATABASE_URL` and `TEST_ISOLATED_REDIS_URL`, skips when unset):

```rust
//! The BATCHED identify path enqueues merges too.
//!
//! `insert_identity` had two independent callers. This file exists because a
//! test that only drove `process_identify` would pass while the path the
//! deployment actually uses silently never merged anything.

use sauron_pipeline::batch::{process_batch, Decoded};
use sauron_core::envelope::{EnvelopeItem, IdentifyItem};

#[tokio::test]
async fn the_batched_identify_path_enqueues_a_merge() {
    let (Some(pool), Some(redis)) = (test_pool().await, test_redis().await) else {
        eprintln!("TEST_DATABASE_URL / TEST_ISOLATED_REDIS_URL unset — skipping");
        return;
    };
    let ids = seed_app(&pool).await;

    let decoded = vec![Decoded {
        id: "0-1".into(),
        job: identify_job(
            ids.app_id,
            IdentifyItem {
                distinct_id: "u-42".into(),
                anonymous_id: Some("anon_batched".into()),
                traits: serde_json::json!({}),
                timestamp: chrono::Utc::now(),
            },
        ),
        masks: std::sync::Arc::new(Default::default()),
        entry_tail: true,
    }];

    process_batch(&pool, &redis, &sym_ctx(), &decoded)
        .await
        .expect("batch");

    let mut conn = pool.get().await.unwrap();
    let rows: Vec<QueuedMerge> = diesel::sql_query(
        "SELECT alias_id, distinct_id FROM identity_merges WHERE app_id = $1",
    )
    .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
    .load(&mut conn)
    .await
    .unwrap();

    assert_eq!(rows.len(), 1, "the batched path must enqueue exactly one merge");
    assert_eq!(rows[0].alias_id, "anon_batched");
}
```

`identify_job`, `sym_ctx`, `test_pool`, `test_redis` and `seed_app` are local helpers — copy their bodies from `tests/retry_drain.rs`, which already builds an `IngestJob` and a `SymbolizeCtx` against the same fixtures. Do **not** call `process_identify` from this file; exercising the other code path is the entire point.

- [ ] **Step 2: Run tests to verify they fail**

```bash
cd backend && cargo test -p sauron-pipeline enqueues -- --nocapture
```
Expected: FAIL — `relation "identity_merges"` is empty (0 rows returned, assertion on `len()` fails).

- [ ] **Step 3: Implement `enqueue_merge`**

Append to `backend/crates/sauron-db/src/identity_merge.rs`:

```rust
/// Schedule the merge for a freshly claimed alias.
///
/// `ON CONFLICT DO NOTHING` on top of `UNIQUE (app_id, alias_id)` makes this
/// safe under stream redelivery: the alias is claimed exactly once, so it is
/// scheduled exactly once, and a duplicate delivery is a no-op rather than a
/// second rewrite pass.
pub async fn enqueue_merge(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    alias_id: &str,
    distinct_id: &str,
) -> QueryResult<usize> {
    diesel::sql_query(
        "INSERT INTO identity_merges (app_id, alias_id, distinct_id) \
         VALUES ($1, $2, $3) ON CONFLICT (app_id, alias_id) DO NOTHING",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Text, _>(alias_id)
    .bind::<Text, _>(distinct_id)
    .execute(conn)
    .await
}
```

- [ ] **Step 4: Wire both call sites**

Write this helper once in `backend/crates/sauron-pipeline/src/process.rs` and call it from both paths, so the two can never drift:

```rust
/// Claim an alias and, on a fresh claim, schedule the merge.
///
/// Shared by `process_identify` and `batch`'s identify loop. Both call it; a
/// change to one is a change to both, which is the point — an earlier draft had
/// the enqueue inlined in one path only, and the other silently never merged.
pub(crate) async fn claim_and_enqueue(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    alias: &str,
    person: &str,
) {
    match sauron_db::identity_merge::claim_identity(conn, app_id, alias, person).await {
        Ok(sauron_db::identity_merge::Claim::Fresh) => {
            if let Err(e) =
                sauron_db::identity_merge::enqueue_merge(conn, app_id, alias, person).await
            {
                // Best-effort, matching every other roll-up write on this path:
                // the identify() itself is already durable, and failing here
                // would replay the whole batch.
                tracing::warn!(app_id = %app_id, error = %e, "merge was not enqueued");
            }
        }
        Ok(sauron_db::identity_merge::Claim::Repeat) => {}
        Ok(sauron_db::identity_merge::Claim::Conflict { existing }) => {
            // The burn rule drops this silently by design. This warning is the
            // ONLY signal an operator ever gets that an app is not calling
            // reset() on logout.
            tracing::warn!(
                app_id = %app_id, alias, claimed_by = %existing, attempted_by = person,
                "identity alias conflict; alias stays bound to its first claimant"
            );
        }
        Ok(sauron_db::identity_merge::Claim::Chain) => {
            tracing::warn!(
                app_id = %app_id, alias, person,
                "identity alias would form a chain; refused"
            );
        }
        Err(e) => tracing::warn!(app_id = %app_id, error = %e, "claiming an alias failed"),
    }
}
```

In `process.rs::process_identify`, replace:

```rust
    if let Some(anon) = id.anonymous_id {
        if !anon.is_empty() {
            let _ = repo::insert_identity(conn, job.app_id, &anon, &id.distinct_id).await;
        }
    }
```

with:

```rust
    if let Some(anon) = id.anonymous_id {
        if !anon.is_empty() && !id.distinct_id.is_empty() {
            claim_and_enqueue(conn, job.app_id, &anon, &id.distinct_id).await;
        }
    }
```

In `batch.rs`, replace the equivalent `insert_identity` block with the same two lines, calling `crate::process::claim_and_enqueue(&mut conn, job.app_id, &anon, &id.distinct_id).await;`.

- [ ] **Step 5: Run tests to verify they pass**

```bash
cd backend && cargo test -p sauron-pipeline -- --nocapture
```
Expected: PASS, including both new enqueue tests and the pre-existing `process_identify_marks_the_user_identified`.

- [ ] **Step 6: Verify — do NOT commit**

```bash
cd backend && cargo fmt --all && cargo clippy -p sauron-pipeline --all-targets -- -D warnings
```

---

### Task 4: Hot rewrites (steps 1–6)

**Files:**
- Modify: `backend/crates/sauron-db/src/identity_merge.rs`
- Test: `backend/crates/sauron-db/tests/identity_merge.rs`

**Interfaces:**
- Produces: `pub async fn rewrite_hot_rows(conn, app_id, alias, person) -> QueryResult<u64>` (returns total rows touched, for logging)

- [ ] **Step 1: Write the failing test**

```rust
use chrono::{Duration, Utc};

/// The headline assertion: after a merge, a guest-then-identified timeline is
/// ONE person, not two.
#[tokio::test]
async fn rewriting_hot_rows_collapses_the_person_to_one() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let t0 = Utc::now() - Duration::hours(2);

    for (did, at) in [("anon_x", t0), ("anon_x", t0 + Duration::minutes(5)), ("u-42", Utc::now())] {
        diesel::sql_query(
            "INSERT INTO analytics_events (id, app_id, project_id, name, distinct_id, occurred_at) \
             VALUES (gen_random_uuid(), $1, $2, 'page_view', $3, $4)",
        )
        .bind::<SqlUuid, _>(ids.app_id)
        .bind::<SqlUuid, _>(ids.project_id)
        .bind::<Text, _>(did)
        .bind::<diesel::sql_types::Timestamptz, _>(at)
        .execute(&mut conn)
        .await
        .expect("seed event");
    }

    let before: Count = diesel::sql_query(
        "SELECT count(DISTINCT distinct_id)::bigint AS n FROM analytics_events WHERE app_id = $1",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .get_result(&mut conn)
    .await
    .unwrap();
    assert_eq!(before.n, 2, "precondition: the bug — one human counted twice");

    sauron_db::identity_merge::rewrite_hot_rows(&mut conn, ids.app_id, "anon_x", "u-42")
        .await
        .expect("rewrite");

    let after: Count = diesel::sql_query(
        "SELECT count(DISTINCT distinct_id)::bigint AS n FROM analytics_events WHERE app_id = $1",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .get_result(&mut conn)
    .await
    .unwrap();
    assert_eq!(after.n, 1, "after the merge the guest and the person are one");

    let marked: Count = diesel::sql_query(
        "SELECT count(*)::bigint AS n FROM analytics_events \
          WHERE app_id = $1 AND guest_alias = 'anon_x'",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .get_result(&mut conn)
    .await
    .unwrap();
    assert_eq!(marked.n, 2, "exactly the pre-login events carry the guest marker");

    drop(conn);
    db.cleanup().await;
}

/// Re-running a completed rewrite must be a no-op — recovery is "run the whole
/// job again", so every step before the folds has to be idempotent.
#[tokio::test]
async fn rewriting_hot_rows_twice_changes_nothing() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    diesel::sql_query(
        "INSERT INTO analytics_events (id, app_id, project_id, name, distinct_id, occurred_at) \
         VALUES (gen_random_uuid(), $1, $2, 'page_view', 'anon_x', now())",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .bind::<SqlUuid, _>(ids.project_id)
    .execute(&mut conn)
    .await
    .unwrap();

    let first = sauron_db::identity_merge::rewrite_hot_rows(&mut conn, ids.app_id, "anon_x", "u-42")
        .await
        .unwrap();
    let second = sauron_db::identity_merge::rewrite_hot_rows(&mut conn, ids.app_id, "anon_x", "u-42")
        .await
        .unwrap();

    assert_eq!(first, 1, "the first pass rewrites the guest row");
    assert_eq!(second, 0, "the second pass must match nothing");

    drop(conn);
    db.cleanup().await;
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cd backend && cargo test -p sauron-db --test identity_merge rewriting -- --nocapture
```
Expected: FAIL — `cannot find function rewrite_hot_rows`.

- [ ] **Step 3: Implement**

Append to `identity_merge.rs`:

```rust
/// Rewrite every hot row that names `alias` so it names `person` instead.
///
/// Each statement is idempotent: after it runs, no row matches
/// `distinct_id = alias`, so a re-run touches nothing. That is what makes
/// recovery "run the whole job again" with no per-table progress tracking.
///
/// Each runs in its own implicit transaction rather than one big one, so a
/// heavy guest does not hold a single long-lived lock across every partition.
/// None of them touch `occurred_at`, so no row moves between partitions.
///
/// Returns the total number of rows touched, for the caller's log line.
pub async fn rewrite_hot_rows(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    alias: &str,
    person: &str,
) -> QueryResult<u64> {
    let mut total = 0u64;

    for sql in [
        "UPDATE analytics_events SET distinct_id = $3, guest_alias = $2 \
          WHERE app_id = $1 AND distinct_id = $2",
        "UPDATE error_events SET distinct_id = $3, guest_alias = $2 \
          WHERE app_id = $1 AND distinct_id = $2",
        "UPDATE sessions      SET distinct_id = $3 WHERE app_id = $1 AND distinct_id = $2",
        "UPDATE transactions  SET distinct_id = $3 WHERE app_id = $1 AND distinct_id = $2",
        "UPDATE workflows     SET distinct_id = $3 WHERE app_id = $1 AND distinct_id = $2",
        "UPDATE devices SET last_distinct_id = $3 WHERE app_id = $1 AND last_distinct_id = $2",
    ] {
        total += diesel::sql_query(sql)
            .bind::<SqlUuid, _>(app_id)
            .bind::<Text, _>(alias)
            .bind::<Text, _>(person)
            .execute(conn)
            .await? as u64;
    }

    Ok(total)
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cd backend && cargo test -p sauron-db --test identity_merge -- --nocapture
```
Expected: PASS — 8 tests.

- [ ] **Step 5: Verify — do NOT commit**

```bash
cd backend && cargo fmt --all && cargo clippy -p sauron-db --all-targets -- -D warnings
```

---

### Task 5: The rollup folds (steps 7–8) + span capture

**Files:**
- Modify: `backend/crates/sauron-db/src/identity_merge.rs`
- Test: `backend/crates/sauron-db/tests/identity_merge.rs`

**Interfaces:**
- Produces: `pub async fn fold_rollups(conn, app_id, alias, person, hot_days: i64) -> QueryResult<()>` — folds both rollup tables and writes `alias_first_seen`/`alias_last_seen`/`cold_stale` onto the queue row.

- [ ] **Step 1: Write the failing tests**

```rust
/// A counter fold is NOT idempotent, so it is written as a MOVE: the DELETE
/// consumes the source. Running it twice must not double the counters.
#[tokio::test]
async fn folding_rollups_twice_does_not_double_count() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    for (did, events) in [("anon_x", 7i64), ("u-42", 3i64)] {
        diesel::sql_query(
            "INSERT INTO event_user_environments \
               (app_id, distinct_id, environment_id, first_seen, last_seen, events_count) \
             VALUES ($1, $2, $3, now(), now(), $4)",
        )
        .bind::<SqlUuid, _>(ids.app_id)
        .bind::<Text, _>(did)
        .bind::<diesel::sql_types::Nullable<SqlUuid>, _>(Some(ids.env_a))
        .bind::<diesel::sql_types::BigInt, _>(events)
        .execute(&mut conn)
        .await
        .unwrap();
    }
    diesel::sql_query(
        "INSERT INTO event_users (id, app_id, distinct_id, properties, first_seen, last_seen) \
         VALUES (gen_random_uuid(), $1, 'anon_x', '{}'::jsonb, now(), now()), \
                (gen_random_uuid(), $1, 'u-42',   '{}'::jsonb, now(), now())",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .execute(&mut conn)
    .await
    .unwrap();
    diesel::sql_query(
        "INSERT INTO identity_merges (app_id, alias_id, distinct_id) VALUES ($1, 'anon_x', 'u-42')",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .execute(&mut conn)
    .await
    .unwrap();

    for _ in 0..2 {
        sauron_db::identity_merge::fold_rollups(&mut conn, ids.app_id, "anon_x", "u-42", 7)
            .await
            .expect("fold");
    }

    let total: Count = diesel::sql_query(
        "SELECT events_count::bigint AS n FROM event_user_environments \
          WHERE app_id = $1 AND distinct_id = 'u-42'",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .get_result(&mut conn)
    .await
    .unwrap();
    assert_eq!(total.n, 10, "7 + 3 exactly once, no matter how many times the fold runs");

    let rows: Count = diesel::sql_query(
        "SELECT count(*)::bigint AS n FROM event_user_environments \
          WHERE app_id = $1 AND distinct_id = 'anon_x'",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .get_result(&mut conn)
    .await
    .unwrap();
    assert_eq!(rows.n, 0, "the fold must consume the alias row");

    drop(conn);
    db.cleanup().await;
}

/// environment_id is NULLABLE and Unattributed is a real, surfaced scope. The
/// ON CONFLICT must name the COALESCE expression from migration 0056 or it
/// silently degrades into an unconstrained insert and the person grows a second
/// unattributed row.
#[tokio::test]
async fn folding_an_unattributed_row_does_not_create_a_duplicate() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    for did in ["anon_x", "u-42"] {
        diesel::sql_query(
            "INSERT INTO event_user_environments \
               (app_id, distinct_id, environment_id, first_seen, last_seen, events_count) \
             VALUES ($1, $2, NULL, now(), now(), 4)",
        )
        .bind::<SqlUuid, _>(ids.app_id)
        .bind::<Text, _>(did)
        .execute(&mut conn)
        .await
        .unwrap();
    }
    diesel::sql_query(
        "INSERT INTO identity_merges (app_id, alias_id, distinct_id) VALUES ($1, 'anon_x', 'u-42')",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .execute(&mut conn)
    .await
    .unwrap();

    sauron_db::identity_merge::fold_rollups(&mut conn, ids.app_id, "anon_x", "u-42", 7)
        .await
        .expect("fold");

    let rows: Count = diesel::sql_query(
        "SELECT count(*)::bigint AS n FROM event_user_environments \
          WHERE app_id = $1 AND distinct_id = 'u-42' AND environment_id IS NULL",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .get_result(&mut conn)
    .await
    .unwrap();
    assert_eq!(rows.n, 1, "one unattributed row, not two");

    drop(conn);
    db.cleanup().await;
}

/// A guest active in several environments yields several `moved` rows. They
/// must not collide on one conflict target — Postgres rejects "ON CONFLICT DO
/// UPDATE command cannot affect row a second time" and the whole fold aborts.
#[tokio::test]
async fn folding_a_guest_active_in_several_environments_succeeds() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    for env in [Some(ids.env_a), Some(ids.env_b), None] {
        diesel::sql_query(
            "INSERT INTO event_user_environments \
               (app_id, distinct_id, environment_id, first_seen, last_seen, events_count) \
             VALUES ($1, 'anon_x', $2, now(), now(), 1)",
        )
        .bind::<SqlUuid, _>(ids.app_id)
        .bind::<diesel::sql_types::Nullable<SqlUuid>, _>(env)
        .execute(&mut conn)
        .await
        .unwrap();
    }
    diesel::sql_query(
        "INSERT INTO identity_merges (app_id, alias_id, distinct_id) VALUES ($1, 'anon_x', 'u-42')",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .execute(&mut conn)
    .await
    .unwrap();

    sauron_db::identity_merge::fold_rollups(&mut conn, ids.app_id, "anon_x", "u-42", 7)
        .await
        .expect("a multi-environment fold must not trip the ON CONFLICT row-twice rule");

    let rows: Count = diesel::sql_query(
        "SELECT count(*)::bigint AS n FROM event_user_environments \
          WHERE app_id = $1 AND distinct_id = 'u-42'",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .get_result(&mut conn)
    .await
    .unwrap();
    assert_eq!(rows.n, 3, "one row per environment, unattributed included");

    drop(conn);
    db.cleanup().await;
}
```

`db.seed_two_envs()` returns `SeedIds`, which already exposes `app_id`, `project_id`, `org_id`, `owner_email`, `env_a`, `env_b` and the issue ids ([common/mod.rs:262](../../../backend/crates/sauron-db/tests/common/mod.rs)). No change to the fixture is needed.

- [ ] **Step 2: Run tests to verify they fail**

```bash
cd backend && cargo test -p sauron-db --test identity_merge folding -- --nocapture
```
Expected: FAIL — `cannot find function fold_rollups`.

- [ ] **Step 3: Implement**

Append to `identity_merge.rs`:

```rust
/// Fold the alias's rollup rows into the person's, and record what the cold
/// overlay needs to know about this alias.
///
/// Both folds are written as a MOVE, not a copy: the `DELETE` in the CTE
/// consumes the source, so a second run finds nothing to move and adds nothing.
/// A plain "copy and add" would double every counter on retry, and retry is the
/// documented recovery path.
///
/// The `ON CONFLICT` target names the `COALESCE(environment_id, nil-uuid)`
/// expression from migration 0056 verbatim. It has to: the unique key is an
/// expression index, and naming `(app_id, distinct_id, environment_id)` instead
/// would degrade into an unconstrained insert, silently giving one person
/// several unattributed rows.
///
/// No two `moved` rows can collide on one conflict target — the alias's own
/// rows are already unique per environment key — so this cannot trip
/// "ON CONFLICT DO UPDATE command cannot affect row a second time".
pub async fn fold_rollups(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    alias: &str,
    person: &str,
    hot_days: i64,
) -> QueryResult<()> {
    const NIL: &str = "'00000000-0000-0000-0000-000000000000'::uuid";

    let env_fold = format!(
        "WITH moved AS ( \
             DELETE FROM event_user_environments \
              WHERE app_id = $1 AND distinct_id = $2 \
             RETURNING environment_id, first_seen, last_seen, \
                       events_count, errors_count, sessions_count) \
         INSERT INTO event_user_environments \
             (app_id, distinct_id, environment_id, first_seen, last_seen, \
              events_count, errors_count, sessions_count) \
         SELECT $1, $3, environment_id, first_seen, last_seen, \
                events_count, errors_count, sessions_count \
           FROM moved \
         ON CONFLICT (app_id, distinct_id, COALESCE(environment_id, {NIL})) \
         DO UPDATE SET \
             first_seen     = LEAST(event_user_environments.first_seen, EXCLUDED.first_seen), \
             last_seen      = GREATEST(event_user_environments.last_seen, EXCLUDED.last_seen), \
             events_count   = event_user_environments.events_count   + EXCLUDED.events_count, \
             errors_count   = event_user_environments.errors_count   + EXCLUDED.errors_count, \
             sessions_count = event_user_environments.sessions_count + EXCLUDED.sessions_count, \
             updated_at     = now()"
    );

    diesel::sql_query(env_fold)
        .bind::<SqlUuid, _>(app_id)
        .bind::<Text, _>(alias)
        .bind::<Text, _>(person)
        .execute(conn)
        .await?;

    // `properties` is concatenated ANON-FIRST so the person's identify() traits
    // win: jsonb `||` lets the right-hand side override. identified_at and
    // identified_source are left untouched — the surviving row is already
    // stamped by process_identify, and the alias never was.
    diesel::sql_query(
        "WITH moved AS ( \
             DELETE FROM event_users WHERE app_id = $1 AND distinct_id = $2 \
             RETURNING properties, first_seen, last_seen) \
         INSERT INTO event_users (id, app_id, distinct_id, properties, first_seen, last_seen) \
         SELECT gen_random_uuid(), $1, $3, properties, first_seen, last_seen FROM moved \
         ON CONFLICT (app_id, distinct_id) DO UPDATE SET \
             first_seen = LEAST(event_users.first_seen, EXCLUDED.first_seen), \
             last_seen  = GREATEST(event_users.last_seen, EXCLUDED.last_seen), \
             properties = EXCLUDED.properties || event_users.properties, \
             updated_at = now()",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Text, _>(alias)
    .bind::<Text, _>(person)
    .execute(conn)
    .await?;

    // What the cold overlay needs, derived from the rows the person now owns.
    //
    // `cold_stale` is deliberately conservative: over-marking costs a few extra
    // overlay rows and a slower cold query, under-marking is a silently wrong
    // number. The extra day covers the tier watermark advancing between enqueue
    // and this statement.
    diesel::sql_query(
        "UPDATE identity_merges m SET \
             alias_first_seen = s.first_seen, \
             alias_last_seen  = s.last_seen, \
             cold_stale       = s.first_seen < now() - make_interval(days => ($4::int - 1)) \
           FROM (SELECT min(occurred_at) AS first_seen, max(occurred_at) AS last_seen \
                   FROM analytics_events \
                  WHERE app_id = $1 AND guest_alias = $2) s \
          WHERE m.app_id = $1 AND m.alias_id = $2 AND s.first_seen IS NOT NULL",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Text, _>(alias)
    .bind::<Text, _>(person)
    .bind::<diesel::sql_types::Integer, _>(hot_days as i32)
    .execute(conn)
    .await?;

    Ok(())
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cd backend && cargo test -p sauron-db --test identity_merge -- --nocapture
```
Expected: PASS — 11 tests.

- [ ] **Step 5: Verify — do NOT commit**

```bash
cd backend && cargo fmt --all && cargo clippy -p sauron-db --all-targets -- -D warnings
```

---

### Task 6: The drain loop

**Files:**
- Create: `backend/crates/sauron-pipeline/src/merge.rs`
- Modify: `backend/crates/sauron-pipeline/src/lib.rs` (add `pub mod merge;`)
- Modify: `backend/bins/sauron-ingest/src/main.rs` (spawn beside `spawn_workers`, ~line 536)
- Modify: `backend/crates/sauron-db/src/identity_merge.rs` (claim/complete/fail)

**Interfaces:**
- Consumes: `rewrite_hot_rows` (Task 4), `fold_rollups` (Task 5)
- Produces:
  ```rust
  pub const MAX_ATTEMPTS: i32 = 5;
  pub async fn claim_next(conn: &mut AsyncPgConnection) -> QueryResult<Option<PendingMerge>>;
  pub struct PendingMerge { pub id: Uuid, pub app_id: Uuid, pub alias_id: String, pub distinct_id: String }
  pub async fn complete_merge(conn, id: Uuid) -> QueryResult<usize>;
  pub async fn fail_merge(conn, id: Uuid, err: &str) -> QueryResult<usize>;
  pub fn spawn_merge_worker(pool: PgPool, hot_days: i64) -> tokio::task::JoinHandle<()>;
  ```

- [ ] **Step 1: Write the failing test**

```rust
/// End-to-end through the queue: a pending row is claimed, executed and marked
/// done, and a completed row is never claimed again.
#[tokio::test]
async fn the_drain_runs_a_pending_merge_exactly_once() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    diesel::sql_query(
        "INSERT INTO analytics_events (id, app_id, project_id, name, distinct_id, occurred_at) \
         VALUES (gen_random_uuid(), $1, $2, 'page_view', 'anon_x', now())",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .bind::<SqlUuid, _>(ids.project_id)
    .execute(&mut conn)
    .await
    .unwrap();
    diesel::sql_query(
        "INSERT INTO identity_merges (app_id, alias_id, distinct_id) VALUES ($1, 'anon_x', 'u-42')",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .execute(&mut conn)
    .await
    .unwrap();

    let job = sauron_db::identity_merge::claim_next(&mut conn)
        .await
        .unwrap()
        .expect("one pending merge");
    assert_eq!(job.alias_id, "anon_x");

    sauron_db::identity_merge::rewrite_hot_rows(&mut conn, job.app_id, &job.alias_id, &job.distinct_id)
        .await
        .unwrap();
    sauron_db::identity_merge::fold_rollups(&mut conn, job.app_id, &job.alias_id, &job.distinct_id, 7)
        .await
        .unwrap();
    sauron_db::identity_merge::complete_merge(&mut conn, job.id).await.unwrap();

    assert!(
        sauron_db::identity_merge::claim_next(&mut conn).await.unwrap().is_none(),
        "a completed merge must never be claimed again"
    );

    drop(conn);
    db.cleanup().await;
}

/// No infinite retry: a row that keeps failing lands in 'failed' and stops
/// being claimed, so one poisoned merge cannot spin the worker forever.
#[tokio::test]
async fn a_merge_stops_being_claimed_after_max_attempts() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    diesel::sql_query(
        "INSERT INTO identity_merges (app_id, alias_id, distinct_id) VALUES ($1, 'anon_x', 'u-42')",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .execute(&mut conn)
    .await
    .unwrap();

    for _ in 0..sauron_db::identity_merge::MAX_ATTEMPTS {
        let job = sauron_db::identity_merge::claim_next(&mut conn)
            .await
            .unwrap()
            .expect("still runnable");
        sauron_db::identity_merge::fail_merge(&mut conn, job.id, "boom").await.unwrap();
    }

    assert!(
        sauron_db::identity_merge::claim_next(&mut conn).await.unwrap().is_none(),
        "after MAX_ATTEMPTS the row must be parked, not retried forever"
    );

    drop(conn);
    db.cleanup().await;
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cd backend && cargo test -p sauron-db --test identity_merge drain claimed -- --nocapture
```
Expected: FAIL — `cannot find function claim_next`.

- [ ] **Step 3: Implement the queue operations**

Append to `identity_merge.rs`:

```rust
/// Hard retry cap. A poisoned merge parks in `failed` for inspection rather
/// than spinning the worker forever.
pub const MAX_ATTEMPTS: i32 = 5;

/// One unit of merge work.
#[derive(Debug, Clone, QueryableByName)]
pub struct PendingMerge {
    #[diesel(sql_type = SqlUuid)]
    pub id: Uuid,
    #[diesel(sql_type = SqlUuid)]
    pub app_id: Uuid,
    #[diesel(sql_type = Text)]
    pub alias_id: String,
    #[diesel(sql_type = Text)]
    pub distinct_id: String,
}

/// Take the oldest runnable merge and mark it `running`.
///
/// `FOR UPDATE SKIP LOCKED` so several replicas can drain the same queue
/// without contending or double-running a merge.
pub async fn claim_next(conn: &mut AsyncPgConnection) -> QueryResult<Option<PendingMerge>> {
    let rows: Vec<PendingMerge> = diesel::sql_query(
        "UPDATE identity_merges SET state = 'running', attempts = attempts + 1 \
          WHERE id = (SELECT id FROM identity_merges \
                       WHERE state IN ('pending', 'failed') AND attempts < $1 \
                       ORDER BY created_at FOR UPDATE SKIP LOCKED LIMIT 1) \
         RETURNING id, app_id, alias_id, distinct_id",
    )
    .bind::<diesel::sql_types::Integer, _>(MAX_ATTEMPTS)
    .load(conn)
    .await?;
    Ok(rows.into_iter().next())
}

/// Mark a merge finished.
pub async fn complete_merge(conn: &mut AsyncPgConnection, id: Uuid) -> QueryResult<usize> {
    diesel::sql_query(
        "UPDATE identity_merges SET state = 'done', completed_at = now(), last_error = NULL \
          WHERE id = $1",
    )
    .bind::<SqlUuid, _>(id)
    .execute(conn)
    .await
}

/// Park a merge for another attempt, retaining the error for inspection.
pub async fn fail_merge(conn: &mut AsyncPgConnection, id: Uuid, err: &str) -> QueryResult<usize> {
    diesel::sql_query("UPDATE identity_merges SET state = 'failed', last_error = $2 WHERE id = $1")
        .bind::<SqlUuid, _>(id)
        .bind::<Text, _>(err)
        .execute(conn)
        .await
}
```

- [ ] **Step 4: Implement the worker**

Create `backend/crates/sauron-pipeline/src/merge.rs`:

```rust
//! The guest → identified merge drain.
//!
//! Co-located in `sauron-ingest` rather than given its own binary: it already
//! owns identity writes and has the pool, and a new bin would mean touching
//! `packaging/rpm/binaries.txt` and the systemd units for no benefit.
//!
//! Off the per-item path on purpose — a merge rewrites every row a guest ever
//! produced, which must never be in the way of accepting an envelope.

use std::time::Duration;

use sauron_db::identity_merge as im;
use sauron_db::PgPool;
use tokio::task::JoinHandle;
use tracing::{info, warn};

/// How long to wait when the queue is empty. Merges are not latency-critical —
/// a few seconds of double-counting right after signup is invisible — so this
/// favours an idle deployment doing almost nothing.
const IDLE_SLEEP: Duration = Duration::from_secs(5);

/// Drain merges until the queue is empty, then sleep. Never returns.
pub fn spawn_merge_worker(pool: PgPool, hot_days: i64) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match drain_once(&pool, hot_days).await {
                Ok(0) => tokio::time::sleep(IDLE_SLEEP).await,
                Ok(_) => {}
                Err(e) => {
                    warn!(error = %e, "merge drain failed; backing off");
                    tokio::time::sleep(IDLE_SLEEP).await;
                }
            }
        }
    })
}

async fn drain_once(pool: &PgPool, hot_days: i64) -> anyhow::Result<usize> {
    let mut conn = pool.get().await?;
    let mut done = 0usize;

    while let Some(job) = im::claim_next(&mut conn).await? {
        let outcome = async {
            let rows =
                im::rewrite_hot_rows(&mut conn, job.app_id, &job.alias_id, &job.distinct_id).await?;
            im::fold_rollups(&mut conn, job.app_id, &job.alias_id, &job.distinct_id, hot_days)
                .await?;
            Ok::<u64, diesel::result::Error>(rows)
        }
        .await;

        match outcome {
            Ok(rows) => {
                im::complete_merge(&mut conn, job.id).await?;
                info!(
                    app_id = %job.app_id, alias = %job.alias_id, person = %job.distinct_id, rows,
                    "merged a guest into an identified person"
                );
                done += 1;
            }
            Err(e) => {
                // Every step is idempotent or consuming, so a partially applied
                // merge is safe to run again from the top.
                warn!(app_id = %job.app_id, alias = %job.alias_id, error = %e, "merge failed");
                im::fail_merge(&mut conn, job.id, &e.to_string()).await?;
            }
        }
    }

    Ok(done)
}
```

Add `pub mod merge;` to `backend/crates/sauron-pipeline/src/lib.rs`.

In `backend/bins/sauron-ingest/src/main.rs`, immediately after the `spawn_workers` call:

```rust
    // The guest → identified merge drain. One per process; several replicas
    // share the queue safely via FOR UPDATE SKIP LOCKED.
    let hot_days = {
        let mut c = pool.get().await?;
        sauron_db::repo::effective_tier_hot_days(&mut c, cfg.tier_hot_days).await?
    };
    let _merge = sauron_pipeline::merge::spawn_merge_worker(pool.clone(), hot_days);
```

`cfg.tier_hot_days` already exists ([config.rs:68](../../../backend/crates/sauron-core/src/config.rs), parsed from `TIER_HOT_DAYS`, default 30). Do not invent a new setting.

- [ ] **Step 5: Run tests to verify they pass**

```bash
cd backend && cargo test -p sauron-db --test identity_merge -- --nocapture && cargo build -p sauron-ingest
```
Expected: PASS — 13 tests — and `sauron-ingest` builds.

- [ ] **Step 6: Verify — do NOT commit**

```bash
cd backend && cargo fmt --all && cargo clippy -p sauron-pipeline -p sauron-ingest --all-targets -- -D warnings
```

---

### Task 7: The cold overlay

**Files:**
- Modify: `backend/crates/sauron-db/src/identity_merge.rs` (the bounded map query)
- Modify: `backend/crates/sauron-tier/src/duck.rs`
- Create: `backend/crates/sauron-db/tests/identity_merge_cold.rs`

**Interfaces:**
- Produces:
  ```rust
  // sauron-db
  pub struct AliasEntry { pub alias: String, pub person: String }
  pub async fn cold_alias_map(conn, app_id: Uuid, from: DateTime<Utc>, to: DateTime<Utc>)
      -> QueryResult<Vec<AliasEntry>>;
  // sauron-tier — NOTE the plain tuple slice, not AliasEntry
  impl DuckEngine { pub fn register_alias_map(&self, entries: &[(String, String)]) -> anyhow::Result<()> }
  // distinct_users_by_day gains an `aliases: &[(String, String)]` parameter
  ```

> **`sauron-tier` does not depend on `sauron-db`** (check its `Cargo.toml` — there is no such dependency, and adding one would invert the layering: `sauron-api` depends on both). So `AliasEntry` **cannot** cross the crate boundary. The DuckDB side takes a plain `&[(alias, person)]` slice, and `sauron-api`'s `tier_read.rs` — which already depends on both — does the one-line mapping. Do not add a `sauron-db` dependency to `sauron-tier` to make the shared type work.

- [ ] **Step 1: Write the failing test**

Create `backend/crates/sauron-db/tests/identity_merge_cold.rs`:

```rust
//! The bounded map that feeds the DuckDB cold overlay.

mod common;

use chrono::{Duration, Utc};
use common::TestDb;
use diesel::prelude::*;
use diesel::sql_types::{Text, Timestamptz, Uuid as SqlUuid};
use diesel_async::RunQueryDsl;
use sauron_db::identity_merge::cold_alias_map;

async fn seed(db: &TestDb, app_id: uuid::Uuid, alias: &str, state: &str,
              first: Option<chrono::DateTime<Utc>>, last: Option<chrono::DateTime<Utc>>,
              cold_stale: bool) {
    let mut conn = db.conn().await;
    diesel::sql_query(
        "INSERT INTO identity_merges \
           (app_id, alias_id, distinct_id, state, alias_first_seen, alias_last_seen, cold_stale) \
         VALUES ($1, $2, 'u-42', $3, $4, $5, $6)",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Text, _>(alias)
    .bind::<Text, _>(state)
    .bind::<diesel::sql_types::Nullable<Timestamptz>, _>(first)
    .bind::<diesel::sql_types::Nullable<Timestamptz>, _>(last)
    .bind::<diesel::sql_types::Bool, _>(cold_stale)
    .execute(&mut conn)
    .await
    .expect("seed merge row");
}

/// A merge whose rows were all still hot when it ran was rewritten BEFORE
/// export, so Parquet already holds the person's id. Carrying it in the overlay
/// forever would be pure cost.
#[tokio::test]
async fn a_not_cold_stale_alias_is_excluded() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let now = Utc::now();
    seed(&db, ids.app_id, "anon_hot", "done", Some(now - Duration::hours(1)), Some(now), false).await;

    let mut conn = db.conn().await;
    let map = cold_alias_map(&mut conn, ids.app_id, now - Duration::days(30), now)
        .await
        .unwrap();
    assert!(map.is_empty(), "cold_stale = false must be pruned, got {map:?}");

    drop(conn);
    db.cleanup().await;
}

/// Window pruning: an alias whose activity does not overlap the query window
/// cannot affect its answer.
#[tokio::test]
async fn an_alias_outside_the_window_is_excluded() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let now = Utc::now();
    seed(&db, ids.app_id, "anon_old", "done",
         Some(now - Duration::days(90)), Some(now - Duration::days(80)), true).await;

    let mut conn = db.conn().await;
    let map = cold_alias_map(&mut conn, ids.app_id, now - Duration::days(7), now)
        .await
        .unwrap();
    assert!(map.is_empty(), "a non-overlapping span must be pruned, got {map:?}");

    drop(conn);
    db.cleanup().await;
}

/// THE HOLE THE SPEC SELF-REVIEW CAUGHT.
///
/// Until the fold runs, the span is NULL and cold_stale is its conservative
/// default — neither prune is safe. Dropping an in-flight alias from the
/// overlay would leave the row stale in BOTH tiers at once: the hot rewrite has
/// not landed either.
#[tokio::test]
async fn a_pending_alias_is_always_included() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let now = Utc::now();
    seed(&db, ids.app_id, "anon_inflight", "pending", None, None, true).await;

    let mut conn = db.conn().await;
    let map = cold_alias_map(&mut conn, ids.app_id, now - Duration::days(7), now)
        .await
        .unwrap();
    assert_eq!(map.len(), 1, "an unmerged alias must never be pruned, got {map:?}");
    assert_eq!(map[0].alias, "anon_inflight");
    assert_eq!(map[0].person, "u-42");

    drop(conn);
    db.cleanup().await;
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cd backend && cargo test -p sauron-db --test identity_merge_cold -- --nocapture
```
Expected: FAIL — `cannot find function cold_alias_map`.

- [ ] **Step 3: Implement the bounded map**

Append to `identity_merge.rs`:

```rust
/// One alias → person edge, as the cold overlay consumes it.
#[derive(Debug, Clone, QueryableByName)]
pub struct AliasEntry {
    #[diesel(sql_type = Text)]
    pub alias: String,
    #[diesel(sql_type = Text)]
    pub person: String,
}

/// The alias edges a cold query over `[from, to)` could possibly need.
///
/// Unbounded, this map is one row per converted device per app — millions at
/// scale, shipped into DuckDB on every query. Two prunes bound it, and BOTH
/// apply only to merges that have actually completed:
///
/// * `cold_stale = false` — every row was still hot when the merge ran, so the
///   rewrite fixed them before export and Parquet is already correct.
/// * span vs. window — the alias was never active in the queried range.
///
/// `state <> 'done'` short-circuits both. Until the fold runs, the span is NULL
/// and `cold_stale` is its conservative default, so pruning on either would
/// drop an alias whose hot rewrite has ALSO not landed yet — the one window in
/// which a row is stale in both tiers simultaneously.
pub async fn cold_alias_map(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    from: chrono::DateTime<chrono::Utc>,
    to: chrono::DateTime<chrono::Utc>,
) -> QueryResult<Vec<AliasEntry>> {
    diesel::sql_query(
        "SELECT alias_id AS alias, distinct_id AS person FROM identity_merges \
          WHERE app_id = $1 \
            AND ( state <> 'done' \
                  OR (cold_stale \
                      AND alias_first_seen < $3 \
                      AND alias_last_seen  >= $2) )",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<diesel::sql_types::Timestamptz, _>(from)
    .bind::<diesel::sql_types::Timestamptz, _>(to)
    .load(conn)
    .await
}
```

- [ ] **Step 4: Implement the DuckDB side**

In `backend/crates/sauron-tier/src/duck.rs`, add to `impl DuckEngine`:

```rust
    /// Publish the alias map as a temp table for the resolved scans to join.
    ///
    /// Registered per query rather than read through `postgres_scanner`: DuckDB
    /// is unbundled and vendored here, and making a correctness-critical path
    /// depend on an extension load is a bad trade.
    pub fn register_alias_map(&self, entries: &[(String, String)]) -> anyhow::Result<()> {
        self.conn
            .execute_batch("CREATE OR REPLACE TEMP TABLE alias_map (alias VARCHAR, person VARCHAR)")?;
        if entries.is_empty() {
            return Ok(());
        }
        let mut app = self.conn.appender("alias_map")?;
        for (alias, person) in entries {
            app.append_row(duckdb::params![alias.as_str(), person.as_str()])?;
        }
        app.flush()?;
        Ok(())
    }

    /// The FROM clause every identity-aggregating cold query must use.
    ///
    /// A second cold aggregation that joined `read_parquet` directly would
    /// silently keep double-counting — no error, no failing test. Funnelling
    /// the resolution through one helper means new queries inherit it by
    /// default instead of by remembering.
    fn resolved_cold_events() -> &'static str {
        "read_parquet(?, hive_partitioning=true, union_by_name=true) e \
         LEFT JOIN alias_map m ON m.alias = e.distinct_id"
    }
```

Change `distinct_users_by_day` to take `aliases: &[(String, String)]`, call `self.register_alias_map(aliases)?` first, and use:

```rust
        let sql = format!(
            "SELECT CAST(e.occurred_at AS DATE) AS day, \
                    count(DISTINCT COALESCE(m.person, e.distinct_id)) AS cnt \
               FROM {} \
              WHERE e.app_id = ? AND e.occurred_at >= ? AND e.occurred_at < ? \
                AND e.distinct_id IS NOT NULL AND e.distinct_id <> '' \
              GROUP BY 1 ORDER BY 1",
            Self::resolved_cold_events()
        );
```

Update the caller in `backend/bins/sauron-api/src/tier_read.rs` to fetch the map and map it across the crate boundary:

```rust
    let aliases: Vec<(String, String)> =
        sauron_db::identity_merge::cold_alias_map(&mut conn, app_id, from, to)
            .await?
            .into_iter()
            .map(|e| (e.alias, e.person))
            .collect();
```

then pass `&aliases` to `distinct_users_by_day`.

- [ ] **Step 5: Run tests to verify they pass**

```bash
cd backend && cargo test -p sauron-db --test identity_merge_cold -- --nocapture && cargo test -p sauron-tier
```
Expected: PASS — 3 new tests, `sauron-tier` still green.

- [ ] **Step 6: Verify — do NOT commit**

```bash
cd backend && cargo fmt --all && cargo clippy -p sauron-db -p sauron-tier -p sauron-api --all-targets -- -D warnings
```

---

### Task 8: JS SDK — auto-reset on identity switch, session rotation

**Files:**
- Modify: `sdks/js/src/identity.ts`
- Modify: `sdks/js/src/client.ts:162-166` (`reset`)
- Modify: `sdks/js/src/api/product.ts:66-83` (`identify`)
- Test: `sdks/js/test/anon-id.test.ts`

**Interfaces:**
- Produces: `LAST_IDENTIFIED_KEY`, `getLastIdentified()`, `setLastIdentified(id)`, `clearLastIdentified()`, `rotateSessionId()` from `identity.ts`; `SauronClient.prepareIdentify(id): string | null`

- [ ] **Step 1: Write the failing tests**

Append to `sdks/js/test/anon-id.test.ts`:

```ts
import { describe, expect, it, beforeEach } from 'vitest';
import {
  getAnonymousId, getSessionId, resetIdentity, rotateSessionId,
  getLastIdentified, setLastIdentified,
} from '../src/identity.js';

describe('identity switch', () => {
  beforeEach(() => {
    localStorage.clear();
    sessionStorage.clear();
    resetIdentity();
  });

  it('rotates the session id so a session never spans two people', () => {
    const first = getSessionId();
    const second = rotateSessionId();
    expect(second).not.toBe(first);
    expect(getSessionId()).toBe(second);
    expect(sessionStorage.getItem('sauron.session_id')).toBe(second);
  });

  it('remembers the last identified user across reloads', () => {
    expect(getLastIdentified()).toBeNull();
    setLastIdentified('u-42');
    expect(getLastIdentified()).toBe('u-42');
    expect(localStorage.getItem('sauron.last_identified')).toBe('u-42');
  });

  it('mints a fresh anon id when a different user identifies', async () => {
    const { SauronClient } = await import('../src/client.js');
    const client = new SauronClient({ dsn: 'https://pub@example.test/1' });

    const ahmedAnon = client.getDistinctId();       // marks the anon id used
    expect(client.prepareIdentify('ahmed')).toBe(ahmedAnon);

    // Logout was never wired. Sara browses, then logs in.
    client.getDistinctId();
    const saraAlias = client.prepareIdentify('sara');

    expect(saraAlias).toBeNull();
    expect(getAnonymousId()).not.toBe(ahmedAnon);
  });
});
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cd sdks/js && npm test -- anon-id
```
Expected: FAIL — `rotateSessionId is not exported`.

- [ ] **Step 3: Implement `identity.ts`**

Append to `sdks/js/src/identity.ts`:

```ts
/**
 * localStorage key holding the id of the last user who called `identify()`.
 *
 * Exists so a login by a DIFFERENT user can be detected on a device where the
 * app never wired `reset()` on logout. Without it, person B's anonymous
 * activity keeps flowing under person A's already-burned alias, and the server
 * resolves it to A — permanently, and with no client-side symptom.
 */
export const LAST_IDENTIFIED_KEY = 'sauron.last_identified';

/** The last user who identified on this device, or null. */
export function getLastIdentified(): string | null {
  const storage = webStorage('localStorage');
  if (!storage) return lastIdentified;
  try {
    return storage.getItem(LAST_IDENTIFIED_KEY);
  } catch {
    return lastIdentified;
  }
}

/** Record the user who just identified. */
export function setLastIdentified(id: string): void {
  lastIdentified = id;
  const storage = webStorage('localStorage');
  if (storage) {
    try {
      storage.setItem(LAST_IDENTIFIED_KEY, id);
    } catch {
      /* best effort — the in-memory value above still applies this session */
    }
  }
}

/** Forget the last identified user (called by `reset()`). */
export function clearLastIdentified(): void {
  lastIdentified = null;
  const storage = webStorage('localStorage');
  if (storage) {
    try {
      storage.removeItem(LAST_IDENTIFIED_KEY);
    } catch {
      /* best effort */
    }
  }
}

/**
 * Mint and persist a fresh session id.
 *
 * `reset()` calls this so a `sessions` row never spans two people. The server's
 * `bump_session` sets `distinct_id = COALESCE(EXCLUDED.distinct_id, …)`, i.e.
 * last-write-wins, so without rotation one session row records only whichever
 * of two consecutive users wrote last.
 */
export function rotateSessionId(): string {
  sessionId = null;
  const storage = webStorage('sessionStorage');
  if (storage) {
    try {
      storage.removeItem(SESSION_ID_KEY);
    } catch {
      /* best effort */
    }
  }
  return getSessionId();
}
```

Add `let lastIdentified: string | null = null;` beside the other module-level caches, and clear it in `resetIdentity()`.

- [ ] **Step 4: Implement `client.ts` and `product.ts`**

In `sdks/js/src/client.ts`, replace `reset()`:

```ts
  reset(): void {
    this.scope.setUser(null);
    resetAnonymousId();
    clearLastIdentified();
    rotateSessionId();
    this.anonUsed = false;
  }

  /**
   * Prepare for an `identify()`; returns the `anonymous_id` to send.
   *
   * When a DIFFERENT user identifies than last time, the current anon id
   * belongs to the previous person and is already burned server-side, so it is
   * replaced before anything else happens and `null` is sent instead of a
   * cross-user alias. This cannot repair events already sent under the burned
   * alias — nothing can — but it bounds a forgotten `reset()` to one guest
   * window instead of every future one.
   */
  prepareIdentify(id: string): string | null {
    const last = getLastIdentified();
    if (last && last !== id) {
      resetAnonymousId();
      rotateSessionId();
      this.anonUsed = false;
    }
    setLastIdentified(id);
    return this.getAnonymousId();
  }
```

Import `clearLastIdentified`, `getLastIdentified`, `rotateSessionId`, `setLastIdentified` from `./identity.js`.

In `sdks/js/src/api/product.ts`, replace `const anonymousId = client.getAnonymousId();` with:

```ts
  const anonymousId = client.prepareIdentify(id);
```

- [ ] **Step 5: Run tests to verify they pass**

```bash
cd sdks/js && npm test && npm run typecheck
```
Expected: PASS — all suites, including the pre-existing `anon-id`, `envelope` and `wire-fixture` tests.

- [ ] **Step 6: Verify — do NOT commit**

Confirm `sdks/js/test/wire-fixture.test.ts` still passes; it is what catches a wire-shape change.

---

### Task 9: Flutter SDK — same two changes

**Files:**
- Modify: `sdks/flutter/lib/src/client.dart:555-600`
- Modify: `sdks/flutter/lib/src/context/anonymous_id_store.dart` (add a last-identified store)
- Test: `sdks/flutter/test/anonymous_id_test.dart`

**Interfaces:**
- Consumes: nothing from earlier tasks (independent of the backend)
- Produces: `SauronClient.prepareIdentify(String id)`, mirroring the JS method

- [ ] **Step 1: Write the failing test**

Append to `sdks/flutter/test/anonymous_id_test.dart`:

```dart
  test('a different user identifying mints a fresh anonymous id', () async {
    final client = await newTestClient();

    client.analyticsDistinctId();                 // marks the anon id used
    final ahmedAlias = await client.prepareIdentify('ahmed');
    expect(ahmedAlias, isNotNull);

    client.analyticsDistinctId();                 // Sara browses; reset() never called
    final saraAlias = await client.prepareIdentify('sara');

    expect(saraAlias, isNull,
        reason: 'a burned alias must never be offered to a second user');
    expect(client.anonymousId, isNot(equals(ahmedAlias)));
  });

  test('reset rotates the session id', () async {
    final client = await newTestClient();
    final before = client.sessionId;
    await client.reset();
    expect(client.sessionId, isNot(equals(before)));
  });
```

`anonymous_id_test.dart` already sets up `TestWidgetsFlutterBinding`, a `_MockClient`, a temp `dir` per test (`setUp`/`tearDown`) and a `const AnonymousIdStore()` — reuse those. Add `newTestClient()` locally in that file, constructing a client against `dir` and `_MockClient`, following how `init_test.dart` builds one.

- [ ] **Step 2: Run test to verify it fails**

```bash
cd sdks/flutter && flutter test test/anonymous_id_test.dart
```
Expected: FAIL — `prepareIdentify` is not defined.

- [ ] **Step 3: Implement**

In `sdks/flutter/lib/src/client.dart`:

```dart
  /// Prepare for an [identify]; returns the `anonymous_id` to send.
  ///
  /// When a DIFFERENT user identifies than last time, the current anonymous id
  /// belongs to the previous person and is already burned server-side, so it is
  /// replaced and `null` is sent instead of a cross-user alias. Bounds a
  /// forgotten [reset] to one guest window rather than every future one.
  Future<String?> prepareIdentify(String id) async {
    final String? last = await _lastIdentifiedStore.read(_dir);
    if (last != null && last != id) {
      _anonymousId = await _anonymousIdStore.mintFresh(_dir);
      _anonymousIdUsed = false;
      _rotateSessionId();
    }
    await _lastIdentifiedStore.write(_dir, id);
    return _anonymousIdUsed ? _anonymousId : null;
  }
```

Replace `final String? aliasOf = _anonymousIdUsed ? _anonymousId : null;` in the identify builder with `final String? aliasOf = await prepareIdentify(distinctId);`.

Extend `reset()` to clear the last-identified store and call `_rotateSessionId()`. Add `_rotateSessionId()` (mint a fresh session id into the same field the client already exposes as `sessionId`), and add a `LastIdentifiedStore` beside `AnonymousIdStore` in `lib/src/context/`, reusing the same `prefs_store.dart` mechanism.

- [ ] **Step 4: Run tests to verify they pass**

```bash
cd sdks/flutter && flutter test
```
Expected: PASS — the full suite, including `wire_fixture_test.dart`.

- [ ] **Step 5: Verify — do NOT commit**

```bash
cd sdks/flutter && dart analyze
```

> Note: `flutter test` cannot see zone-scoped defects. If anything in this task touches error-zone behaviour, drive it on a device rig instead — but these two changes are pure state handling and do not.

---

### Task 10: Performance regression guards

The whole reason approach B was chosen over a read-time overlay is that migrations 0028, 0031 and 0039 exist to make `count(DISTINCT distinct_id)` index-only. Nothing so far proves that still holds.

**Files:**
- Create: `backend/crates/sauron-db/tests/identity_merge_perf.rs`

- [ ] **Step 1: Write the failing test**

```rust
//! Guards the property that justified approach B: adding `guest_alias` must not
//! cost the covering indexes their index-only scans.

mod common;

use common::TestDb;
use diesel::prelude::*;
use diesel::sql_types::{Text, Uuid as SqlUuid};
use diesel_async::RunQueryDsl;

#[derive(QueryableByName)]
struct Plan {
    #[diesel(sql_type = Text)]
    #[diesel(column_name = "QUERY PLAN")]
    line: String,
}

/// Migration 0039 exists to answer this exact aggregate without touching the
/// heap. If `guest_alias` ever pushes the planner off it, active-users goes
/// from an index-only scan to a heap scan across every partition — the shape
/// that already produced a 30s TimeoutLayer 503 on this codebase.
#[tokio::test]
async fn active_users_still_uses_an_index_only_scan() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    diesel::sql_query("SET enable_seqscan = off").execute(&mut conn).await.unwrap();

    let plan: Vec<Plan> = diesel::sql_query(
        "EXPLAIN SELECT count(DISTINCT distinct_id) FROM analytics_events \
          WHERE app_id = $1 AND environment_id IS NOT NULL \
            AND occurred_at >= now() - interval '7 days'",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .load(&mut conn)
    .await
    .expect("explain");

    let text = plan.iter().map(|p| p.line.as_str()).collect::<Vec<_>>().join("\n");
    assert!(
        text.contains("Index Only Scan"),
        "active-users must stay index-only after guest_alias; plan was:\n{text}"
    );

    drop(conn);
    db.cleanup().await;
}
```

- [ ] **Step 2: Run the test**

```bash
cd backend && cargo test -p sauron-db --test identity_merge_perf -- --nocapture
```
Expected: PASS. If it FAILS, stop — `guest_alias` has cost a covering index its index-only scan and the migration needs the column added to the relevant `INCLUDE` list before going further.

- [ ] **Step 3: Full suite + baseline comparison**

```bash
cd backend && cargo test --workspace 2>&1 | tail -40
```
Expected: the workspace pass count is **at or above 1391** (the known real baseline with containers reachable). A number near 1354 means the tests silently skipped and the run proves nothing — re-check `dangerouslyDisableSandbox` and container networking before reading any result.

- [ ] **Step 4: Verify — do NOT commit**

```bash
cd backend && cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
```

---

## Spec coverage

| Spec section | Task |
|---|---|
| §1 data model — `guest_alias`, `identity_merges` | 1 |
| §1 no-chains invariant, `resolve()` idempotence | 2 |
| §2 enqueue on fresh claim, both paths | 3 |
| §2 hot rewrites (steps 1–6) | 4 |
| §2 folds (steps 7–8), span + `cold_stale` capture | 5 |
| §2 drain, `MAX_ATTEMPTS`, spawn wiring | 6 |
| §3 cold overlay, bounded map, in-flight rule, structural guard | 7 |
| §4 SDK changes 1 & 2 (js) | 8 |
| §4 SDK changes 1 & 2 (flutter) | 9 |
| §5 conflict observability | 3 (`claim_and_enqueue` warn) |
| Testing 15–16 (perf guards) | 10 |
| §4 node/python/csharp unchanged | none needed — verified during design, no code touches them |
