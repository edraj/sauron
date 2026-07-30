# Environment-Scoped Reads (Slice 2) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make environment a first-class navigation context — a fourth topbar level threaded through every read that can be attributed to one, with correct per-environment aggregates for issues.

**Architecture:** A `ReadScope { app_id, env }` replaces the bare `app_id` on every telemetry read, making the dimension a compile error at each call site. Applying it takes two mechanisms because the read layer is written in two styles: a `macro_rules!` for the three boxed diesel queries, and a predicate-fragment-plus-bind for the ~25 raw `sql_query` functions. A new database integration harness is what proves the predicate is actually applied — for the raw-SQL majority it is the only mechanism that can. Per-environment issue counts are computed on read rather than maintained at ingest, because Task 1 measured the per-event upsert at ~15-25% of the error write path. The dashboard attaches the environment through a single axios interceptor rather than threading a parameter through 22 API functions.

**No writes change.** The ingest path is untouched by this slice — every change is on the read side, plus three indexes.

**Tech Stack:** Rust (axum, diesel-async, tokio), Postgres 16, Redis, Svelte 5 (runes), vitest.

**Spec:** `docs/superpowers/specs/2026-07-28-environment-scoped-reads-design.md`

## Global Constraints

- **Never commit, never branch.** This repository's standing rule. Each task ends with a verification gate, not a commit. Leave changes in the working tree.
- **Backend gate after every backend task:** `cd backend && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
- **DuckDB linking:** cargo needs `DUCKDB_LIB_DIR` and `LD_LIBRARY_PATH` pointing at the prebuilt `libduckdb.so` under `.cache/duckdb/<version>/x86_64-unknown-linux-gnu` (fetch via `packaging/rpm/fetch-libduckdb.sh`). **Never pass `--all-features`** — it enables `sauron-tier`'s `bundled` feature and recompiles DuckDB from source.
- **Never run any `diesel` CLI command.** It rewrites `backend/crates/sauron-db/src/schema.rs` from 27 `diesel::table!` blocks to 87 (one per partition child) and still compiles — the count is the only detector. **This slice adds no table, so the count must read 27 throughout.** Any other value means the file was regenerated: revert it.
- **Database connection on this host:** compose services `postgres` and `redis` publish **no host ports**. Get IPs via `docker inspect` (`sauron-postgres-1` was `172.20.0.3`, `sauron-redis-1` on the same network). **`localhost:5432` is an unrelated Postgres container — do not use it.**
- **Migration numbering:** `backend/migrations/YYYY-MM-DD-NNNNNN_snake_case/` with a globally monotonic 6-digit ordinal. Slice 1 used `2026-07-28-000026_env_keys`, so this slice uses **`2026-07-28-000027_env_scoped_reads`**.
- **`sauron-migrate` has no subcommands** — it ignores argv. To revert: apply `down.sql` via psql, then `DELETE FROM __diesel_schema_migrations WHERE version = '<version>';`, then re-run the binary.
- **Wire contract:** `?environment_id=<uuid>` = one environment; `?environment_id=none` = unattributed rows; **parameter absent = all environments including unattributed**.
- **Do not wire the query planner.** `sauron-query` / `sauron-db::query_plan` are built but connected to no route (`prepare.rs:38` says so). This slice threads environment through the **live** hand-written path only. Wiring the planner belongs to the search programme.
- **Scratch artifacts** go in `.superpowers/sdd/` prefixed **`s2-`** (e.g. `s2-task-3-report.md`). A previous programme destroyed two files by colliding on unprefixed `task-N-*.md` names.

---

## File Structure

**Created:**
- `backend/migrations/2026-07-28-000027_env_scoped_reads/{up,down}.sql` — three indexes (no new table)
- `backend/crates/sauron-db/src/scope.rs` — `EnvFilter`, `ReadScope`, the `scope_env!` macro, `sql_fragment`
- `backend/crates/sauron-db/tests/env_scoping.rs` — the integration harness (new test *crate*, first in the repo)
- `dashboard/src/lib/api/scope.ts` — the interceptor's opt-out list and param builder

**Modified (backend):**
- `backend/crates/sauron-db/src/repo.rs` — ~36 read functions
- `backend/bins/sauron-api/src/routes/*.rs` — 22 handlers parse and pass the scope

**Modified (frontend):**
- `dashboard/src/lib/stores/session.svelte.ts` — 4th level + `scopeKey`
- `dashboard/src/lib/api/client.ts` — request interceptor
- `dashboard/src/lib/components/layout/Topbar.svelte` — 4th switcher
- `dashboard/src/lib/components/filters/filters.ts`, `dashboard/src/pages/Events.svelte` — chip retirement
- 15 telemetry pages — effects key on `scopeKey`

---

## Task 1: Measure the rollup's write cost — DONE, and it changed the design

**Files:** none modified — this task produces a measurement and a go/no-go.

**Interfaces:**
- Consumes: nothing.
- Produces: a decision recorded in `.superpowers/sdd/s2-task-1-report.md`. If the cost is unacceptable, **stop and escalate** — Tasks 9 and 10 change shape.

> **This task has been executed. Its outcome is recorded here because it is the reason the
> rest of the plan looks the way it does — do not re-run it.**
>
> Result: the candidate per-event upsert cost **~98µs** under a conflict-heavy load (the
> realistic shape — only 5 distinct fingerprints exist, so the same row is hit repeatedly),
> against a `upsert_issue`-shaped proxy at 127.5µs. That is roughly **15-25% added to the
> per-error write path**, over the 15% guardrail. A plain insert with no unique index was
> ~5µs, so the upsert is ~20× that under conflict.
>
> **Decision: the `issue_environments` rollup was dropped.** Per-environment issue counts are
> computed on read instead (Task 9). This removed a table, an ingest write, a per-environment
> Redis HyperLogLog, and a backfill scan over the largest table in the schema.

This task existed because the search programme measured 9× write amplification for a set of GIN indexes and dropped them on that evidence. The same discipline applied here, and reached the same kind of conclusion.

- [ ] **Step 1: Create the candidate table on a scratch database**

```bash
cd /home/splimter/projects/freelance/sauron
PGIP=$(docker inspect -f '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}' sauron-postgres-1)
export PGPASSWORD=sauron
psql -h "$PGIP" -U sauron -d sauron -c "CREATE DATABASE s2_bench;"
psql -h "$PGIP" -U sauron -d s2_bench -c "
CREATE TABLE ie (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  issue_id UUID NOT NULL,
  environment_id UUID,
  times_seen BIGINT NOT NULL DEFAULT 0,
  users_seen BIGINT NOT NULL DEFAULT 0,
  first_seen TIMESTAMPTZ NOT NULL,
  last_seen TIMESTAMPTZ NOT NULL
);
CREATE UNIQUE INDEX ie_key ON ie (issue_id, environment_id) NULLS NOT DISTINCT;
CREATE INDEX ie_env_last ON ie (environment_id, last_seen DESC);"
```

- [ ] **Step 2: Measure the upsert in isolation, A/B**

The realistic shape is: ~22 issues per app across 5 fingerprints (the current seed's skew — see the ledger's note that `crebain` emits only 5 distinct fingerprints), so the same `(issue_id, environment_id)` row is hit repeatedly. That is the *conflict-heavy* case, which is the expensive one for an upsert.

Run 40,000 upserts and time them, then the same 40,000 inserts against a table with no unique index as the baseline:

```bash
psql -h "$PGIP" -U sauron -d s2_bench -c "
\timing on
INSERT INTO ie (issue_id, environment_id, first_seen, last_seen, times_seen)
SELECT (ARRAY(SELECT gen_random_uuid() FROM generate_series(1,22)))[1 + (i % 22)],
       (ARRAY(SELECT gen_random_uuid() FROM generate_series(1,3)))[1 + (i % 3)],
       now(), now(), 1
FROM generate_series(1,40000) i
ON CONFLICT (issue_id, environment_id) DO UPDATE
SET times_seen = ie.times_seen + 1, last_seen = EXCLUDED.last_seen;"
```

Record the elapsed time. Then repeat against a copy of the table with `ie_key` dropped and a plain `INSERT` (no `ON CONFLICT`) for the baseline.

- [ ] **Step 3: Put it in context**

The number that matters is the upsert cost **relative to what `process_error` already does per event**. That path already performs: `upsert_issue` (itself an `ON CONFLICT` upsert on `issues`), `insert_error_event`, a `rollup` call, a Redis `PFADD` + `PFCOUNT`, and `touch_event_user`. Measure `upsert_issue` the same way on the same box so the comparison is like-for-like, and express the result as "this adds X% to the per-error write path", not as a raw millisecond figure.

- [ ] **Step 4: Decide and record**

Write `.superpowers/sdd/s2-task-1-report.md` with both timings, the ratio, and a clear verdict:

- **Under ~15% added to the per-error path** → proceed as specified.
- **Materially more** → **STOP AND ESCALATE.** Do not proceed to Tasks 9-10. The fallback design is to write the rollup asynchronously (a periodic aggregation job over `error_events` rather than a per-event upsert), which trades freshness for write cost and changes both tasks.

- [ ] **Step 5: Clean up**

```bash
psql -h "$PGIP" -U sauron -d sauron -c "DROP DATABASE s2_bench;"
```
Confirm the main `sauron` database is untouched.

---

## Task 2: The database integration harness

**Files:**
- Create: `backend/crates/sauron-db/tests/env_scoping.rs`
- Create: `backend/crates/sauron-db/tests/common/mod.rs`
- Modify: `backend/crates/sauron-db/Cargo.toml` (dev-dependencies)

**Interfaces:**
- Consumes: nothing.
- Produces, for every later backend task:
  - `common::TestDb` with `async fn setup() -> Option<TestDb>`, returning `None` when no database is configured so the suite skips rather than fails
  - `TestDb::seed_two_envs(&self) -> SeedIds` where `SeedIds { app_id, env_a, env_b, issue_id }`
  - `TestDb::conn(&self) -> PgConn`

This repository has **no database test harness** — all 66 backend test modules are in-file unit tests over pure functions, and four separate prior plans state this constraint independently. You are building it from zero. It is the only mechanism that can verify the ~25 raw-SQL reads, which `diesel::debug_query` cannot inspect.

**Use an ephemeral database, not the shared one.** `sauron-db` already exposes everything
needed, and `crebain` already does exactly this — see `backend/bins/crebain/src/harness.rs`
lines 90, 94 and 355 for the working precedent:

- `sauron_db::create_database(&admin_url, &db_name)` (`lib.rs:56`)
- `sauron_db::run_pending_migrations(&db_url)` (`lib.rs:29`)
- `sauron_db::drop_database(&admin_url, &db_name)` (`lib.rs:68`)
- `sauron_db::pool::build_pool(&db_url, max_size) -> PgPool` (`pool.rs:31`)
- `sauron_db::pool::conn(&pool) -> PgConn` (`pool.rs:49`)

This matters for more than tidiness. The shared `sauron` database holds ~210k events, 10
environments and 23 issues that later tasks verify against; seeding test rows into it would
both risk disturbing that data and make every assertion depend on what else happens to be
there. A per-run database created from the migrations gives exact, known row counts — which
is what lets the tests assert `== 3` rather than `>= 3`, and an exact assertion is the only
kind that catches an over-broad filter.

Name the database with a random suffix so concurrent runs cannot collide, and drop it in
cleanup.

- [ ] **Step 1: Write the harness skeleton with one failing assertion**

Create `backend/crates/sauron-db/tests/common/mod.rs`:

```rust
//! Integration-test harness. The first in this repository — every other backend
//! test is an in-file unit test over pure functions.
//!
//! Skips rather than fails when `TEST_DATABASE_URL` is unset, so `cargo test
//! --workspace` stays green on a machine with no database (which is what CI is
//! today). A developer opts in by exporting the variable.

use diesel_async::AsyncPgConnection;
use uuid::Uuid;

pub struct TestDb {
    pub pool: sauron_db::PgPool,
    /// Every seeded row is namespaced under this app so a run cannot disturb
    /// real data, and cleanup is a single cascading delete.
    pub app_id: Uuid,
}

pub struct SeedIds {
    pub app_id: Uuid,
    pub env_a: Uuid,
    pub env_b: Uuid,
    pub issue_id: Uuid,
}

impl TestDb {
    /// `None` when `TEST_DATABASE_URL` is unset — callers skip.
    pub async fn setup() -> Option<TestDb> {
        let url = std::env::var("TEST_DATABASE_URL").ok()?;
        let pool = sauron_db::pool(&url).await.expect("test pool");
        Some(TestDb {
            pool,
            app_id: Uuid::new_v4(),
        })
    }

    pub async fn conn(&self) -> sauron_db::PgConn {
        sauron_db::conn(&self.pool).await.expect("checkout")
    }
}
```

Check `sauron_db`'s real public surface before writing this — confirm the pool constructor's name and signature (`sauron_db::pool` vs something else) by reading `backend/crates/sauron-db/src/lib.rs`, and match it. Do not invent an API.

Create `backend/crates/sauron-db/tests/env_scoping.rs` with one test that must fail:

```rust
mod common;

use common::TestDb;

/// The harness itself works: it can reach a database, seed two environments,
/// and read back exactly what it wrote. Everything else in this file depends on
/// this being true, so it is asserted first and separately.
#[tokio::test]
async fn harness_seeds_two_isolated_environments() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    assert_ne!(ids.env_a, ids.env_b);
}
```

- [ ] **Step 2: Run it and watch it fail**

```bash
cd backend
export TEST_DATABASE_URL="postgres://sauron:sauron@$(docker inspect -f '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}' sauron-postgres-1):5432/sauron"
cargo test -p sauron-db --test env_scoping
```
Expected: FAIL to compile — `no method named seed_two_envs found for struct TestDb`.

- [ ] **Step 3: Implement the seed**

Add to `common/mod.rs`. It creates a throwaway org → project → app → two environments, then inserts a known set of rows into `analytics_events`, `error_events`, `sessions` and `transactions`: **three rows in `env_a`, two in `env_b`, and one with `environment_id NULL`** per table. Those specific counts matter — they make an off-by-one or a swapped filter visible, which equal counts would not.

Write the seed using `repo::` functions where they exist and raw `diesel::sql_query` where they do not. Return `SeedIds`.

Add a `Drop`-safe cleanup: a `pub async fn cleanup(&self)` that deletes the org, relying on the existing `ON DELETE CASCADE` chain, and call it at the end of each test. **Document on `cleanup()` that callers must `drop(conn)` first** — the pool is sized 1, so a still-held connection deadlocks the cleanup checkout for the full acquire timeout and then panics with a pool error that looks nothing like the real cause. Do not rely on `Drop` itself — it cannot await.

Also add the time helper every later test uses, so no test has to invent its own window:

```rust
/// A `since` bound far enough back that no seeded row is excluded by it. Tests
/// assert on environment scoping, so the time window must never be the reason a
/// row is missing — otherwise a broken env filter and a too-narrow window are
/// indistinguishable from a failing assertion.
pub fn far_past() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now() - chrono::Duration::days(3650)
}
```

- [ ] **Step 4: Run it and watch it pass**

```bash
cargo test -p sauron-db --test env_scoping
```
Expected: PASS.

- [ ] **Step 5: Prove the skip path works**

```bash
unset TEST_DATABASE_URL && cargo test -p sauron-db --test env_scoping
```
Expected: PASS with the skip message printed. This is what keeps `cargo test --workspace` green in CI, which has no database.

- [ ] **Step 6: Gate**

Run the full backend gate. Confirm `cargo test --workspace` passes both with and without `TEST_DATABASE_URL` set.

---

## Task 3: `ReadScope`, the macro, and the SQL fragment

**Files:**
- Create: `backend/crates/sauron-db/src/scope.rs`
- Modify: `backend/crates/sauron-db/src/lib.rs` (add `pub mod scope;` and re-export)

**Interfaces:**
- Consumes: nothing.
- Produces, for Tasks 5-10:
  - `scope::EnvFilter` — `All | One(Uuid) | Unattributed`
  - `scope::ReadScope { pub app_id: Uuid, pub env: EnvFilter }`
  - `ReadScope::new(app_id: Uuid, env: EnvFilter) -> Self`
  - `EnvFilter::sql_fragment(&self, bind_index: usize) -> String`
  - `EnvFilter::bind_uuid(&self) -> Option<Uuid>` — the value for the bind `sql_fragment` reserved, `None` when it reserved none
  - `scope_env!(query, table_module, env_filter)` — **three** arguments, applied to a boxed
    diesel query, e.g. `q = crate::scope_env!(q, sessions, scope.env);`. Note the name is
    `scope_env!`, not `scope_filter!`, and it takes the query as its first argument.

- [ ] **Step 1: Write the failing tests**

Create `backend/crates/sauron-db/src/scope.rs` containing **only** the doc comment and the test module:

```rust
//! Tenant + environment scope for telemetry reads.
//!
//! Replaces the bare `app_id: Uuid` that ~36 read functions took. The point is
//! the compile error: adding the environment dimension cannot be done to some
//! reads and forgotten on others, because every call site must construct one.
//!
//! Note what this does NOT buy: a function body can destructure `app_id` and
//! ignore `env`, and it will compile. `tests/env_scoping.rs` is what closes
//! that gap, and for the raw-SQL reads it is the only thing that can.

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn all_reserves_no_bind_and_emits_nothing() {
        let f = EnvFilter::All;
        assert_eq!(f.sql_fragment(3), "");
        assert_eq!(f.bind_uuid(), None);
    }

    #[test]
    fn one_reserves_the_given_bind_index() {
        let id = Uuid::from_u128(7);
        let f = EnvFilter::One(id);
        assert_eq!(f.sql_fragment(3), " AND environment_id = $3");
        assert_eq!(f.bind_uuid(), Some(id));
    }

    /// Unattributed needs no bind: `IS NULL` is a literal predicate. A caller
    /// that reserved an index for it would leave a gap in the positional
    /// sequence and every later bind would be off by one.
    #[test]
    fn unattributed_emits_is_null_and_reserves_no_bind() {
        let f = EnvFilter::Unattributed;
        assert_eq!(f.sql_fragment(3), " AND environment_id IS NULL");
        assert_eq!(f.bind_uuid(), None);
    }

    /// The fragment is table-qualifiable for queries that join, where a bare
    /// `environment_id` would be ambiguous.
    #[test]
    fn qualified_fragment_prefixes_the_table() {
        let id = Uuid::from_u128(9);
        assert_eq!(
            EnvFilter::One(id).sql_fragment_for("e", 4),
            " AND e.environment_id = $4"
        );
        assert_eq!(
            EnvFilter::Unattributed.sql_fragment_for("e", 4),
            " AND e.environment_id IS NULL"
        );
        assert_eq!(EnvFilter::All.sql_fragment_for("e", 4), "");
    }
}
```

- [ ] **Step 2: Run and watch it fail**

Run: `cd backend && cargo test -p sauron-db scope::`
Expected: FAIL to compile — `cannot find type EnvFilter in this scope`.

- [ ] **Step 3: Implement**

Add above the test module:

```rust
use uuid::Uuid;

/// Which environments a read covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvFilter {
    /// Every environment, including rows with none. The picker's default, and
    /// what an absent `environment_id` query parameter means.
    All,
    One(Uuid),
    /// Rows whose `environment_id IS NULL` — signals ingested before Slice 1,
    /// or under the old per-app environment cap. Surfaced rather than hidden so
    /// "All" equals the sum of the individual environments instead of exceeding
    /// it, which would be unexplainable to a user reading the numbers.
    Unattributed,
}

impl EnvFilter {
    /// SQL to AND into a raw `sql_query`, or `""` for `All`.
    ///
    /// `bind_index` is the next free positional bind. **Only `One` consumes
    /// it** — `All` emits nothing and `Unattributed` emits a literal `IS NULL`.
    /// A caller that assumes an index is always consumed will shift every
    /// subsequent bind by one, which is the single easiest way to get this
    /// wrong. Pair every call with `bind_uuid()`.
    pub fn sql_fragment(&self, bind_index: usize) -> String {
        match self {
            EnvFilter::All => String::new(),
            EnvFilter::One(_) => format!(" AND environment_id = ${bind_index}"),
            EnvFilter::Unattributed => " AND environment_id IS NULL".to_string(),
        }
    }

    /// `sql_fragment` for a query where `environment_id` needs a table alias.
    pub fn sql_fragment_for(&self, alias: &str, bind_index: usize) -> String {
        match self {
            EnvFilter::All => String::new(),
            EnvFilter::One(_) => format!(" AND {alias}.environment_id = ${bind_index}"),
            EnvFilter::Unattributed => format!(" AND {alias}.environment_id IS NULL"),
        }
    }

    /// The value for the bind `sql_fragment` reserved, or `None` if it reserved
    /// none. Bind it only when this returns `Some`.
    pub fn bind_uuid(&self) -> Option<Uuid> {
        match self {
            EnvFilter::One(id) => Some(*id),
            EnvFilter::All | EnvFilter::Unattributed => None,
        }
    }
}

/// Tenant + environment scope for a telemetry read.
#[derive(Debug, Clone, Copy)]
pub struct ReadScope {
    pub app_id: Uuid,
    pub env: EnvFilter,
}

impl ReadScope {
    pub fn new(app_id: Uuid, env: EnvFilter) -> Self {
        Self { app_id, env }
    }

    /// Scope covering every environment — for callers that genuinely have no
    /// environment context, and for tests.
    pub fn all(app_id: Uuid) -> Self {
        Self {
            app_id,
            env: EnvFilter::All,
        }
    }
}
```

- [ ] **Step 4: Run and watch it pass**

Run: `cd backend && cargo test -p sauron-db scope::`
Expected: PASS (4 tests).

- [ ] **Step 5: Add the boxed-query macro**

Only three read functions use diesel's boxed form. A generic helper cannot serve them — the search programme established that diesel's `ValidGrouping`/`QueryFragment` obligations are not provable generically, which is why `query_plan/issues.rs` expands a macro once per concrete column. Same technique:

```rust
/// Apply an [`EnvFilter`] to a boxed diesel query over a table with an
/// `environment_id` column.
///
/// A macro rather than a function for the reason `query_plan/issues.rs`
/// documents: a generic bounded only by `Column<Table = …>` cannot prove the
/// downstream diesel operator obligations, because the compiler cannot see a
/// *specific* column's `IsAggregate`. Expanded once per concrete table, where
/// the real diesel-generated types are visible.
#[macro_export]
macro_rules! scope_env {
    ($q:expr, $table:ident, $env:expr) => {
        match $env {
            $crate::scope::EnvFilter::All => $q,
            $crate::scope::EnvFilter::One(id) => $q.filter($table::environment_id.eq(id)),
            $crate::scope::EnvFilter::Unattributed => {
                $q.filter($table::environment_id.is_null())
            }
        }
    };
}
```

Register the module in `backend/crates/sauron-db/src/lib.rs` with `pub mod scope;`.

- [ ] **Step 6: Gate**

Run the full backend gate. `schema.rs` must still be 27 table blocks.

---

## Task 4: Migration — the indexes an environment predicate needs

**Files:**
- Create: `backend/migrations/2026-07-28-000027_env_scoped_reads/up.sql`
- Create: `backend/migrations/2026-07-28-000027_env_scoped_reads/down.sql`

**Interfaces:**
- Consumes: nothing.
- Produces: three indexes. **No new table and no schema.rs change** — Task 1's measurement
  removed the `issue_environments` rollup from this slice, so `schema.rs` stays at 27
  `diesel::table!` blocks and `models.rs` is untouched.

- [ ] **Step 1: Write `up.sql`**

```sql
-- Slice 2: indexes that make an environment predicate affordable.
--
-- No new table. An earlier draft added an `issue_environments` rollup maintained by an
-- upsert inside `process_error`; it was benchmarked first and cost ~98us per conflict-heavy
-- upsert, roughly 15-25% added to the per-error write path against a 15% guardrail. Per
-- environment issue counts are computed on read instead, which is what the third index here
-- supports.

-- 1. `sessions` and `transactions` carry environment_id but have no index on it, so an
--    environment-filtered session list would seq-scan. Mirrors the shape
--    2026-07-27-000025 established for error_events and analytics_events: tenant key,
--    then the filtered dimension, then the sort column.
CREATE INDEX sessions_app_env_time_idx
    ON sessions (app_id, environment_id, last_event_at DESC);
CREATE INDEX transactions_app_env_time_idx
    ON transactions (app_id, environment_id, occurred_at DESC);

-- 2. Supports the per-issue, per-environment LATERAL aggregate the Issues list runs when a
--    specific environment is selected. The existing error_events_issue_time_id_idx leads
--    with issue_id but does not carry environment_id, so it cannot serve the grouped count
--    without a filter step over every occurrence of the issue.
CREATE INDEX error_events_issue_env_idx
    ON error_events (issue_id, environment_id);
```

- [ ] **Step 2: Write `down.sql`**

```sql
DROP INDEX IF EXISTS error_events_issue_env_idx;
DROP INDEX IF EXISTS transactions_app_env_time_idx;
DROP INDEX IF EXISTS sessions_app_env_time_idx;
```

Note `error_events` is a partitioned parent: `CREATE INDEX` on it builds synchronously
across every live child partition inside this migration's transaction, holding locks on the
parent and each child. `error_events` is the hottest-write table in the schema. **This needs
a maintenance window**, exactly as `2026-07-27-000025_search_indexes` documented for the
same reason. `CONCURRENTLY` is not available — migrations run in a transaction.

- [ ] **Step 3: Apply**

```bash
cd /home/splimter/projects/freelance/sauron
PGIP=$(docker inspect -f '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}' sauron-postgres-1)
export DATABASE_URL="postgres://sauron:sauron@$PGIP:5432/sauron"
cd backend && cargo run -p sauron-migrate
```

- [ ] **Step 4: Verify all three exist and are used**

```bash
psql "$DATABASE_URL" -c "\d+ sessions" | grep env
psql "$DATABASE_URL" -c "\d+ transactions" | grep env
psql "$DATABASE_URL" -c "\d+ error_events" | grep issue_env
```
Expected: all three present.

Then confirm the third one is actually chosen for the shape Task 10 will run, rather than
merely existing:

```bash
psql "$DATABASE_URL" -c "
EXPLAIN (ANALYZE, BUFFERS)
SELECT count(*), count(DISTINCT distinct_id), min(occurred_at), max(occurred_at)
FROM error_events
WHERE issue_id = (SELECT id FROM issues LIMIT 1) AND environment_id IS NOT NULL;"
```
Paste the plan. If it seq-scans, say so — the index shape is wrong and Task 10 depends on it.

- [ ] **Step 5: Verify `schema.rs` is untouched**

Run: `grep -c '^diesel::table!' backend/crates/sauron-db/src/schema.rs`
Expected: **27**. This task adds no table, so any other number means the file was
regenerated by a diesel CLI command — revert it.

- [ ] **Step 6: Verify the down migration**

```bash
psql "$DATABASE_URL" -f backend/migrations/2026-07-28-000027_env_scoped_reads/down.sql
psql "$DATABASE_URL" -c "DELETE FROM __diesel_schema_migrations WHERE version = '20260728000027';"
cd backend && cargo run -p sauron-migrate
```
Expected: all three succeed, and Step 4's checks pass again.

- [ ] **Step 7: Gate**

Full backend gate.

---

## Task 5: Thread env through the three boxed-diesel reads

**Files:**
- Modify: `backend/crates/sauron-db/src/repo.rs` — `list_analytics_events` (~2229), `list_sessions` (~1846)
- Modify: `backend/crates/sauron-db/tests/env_scoping.rs`

`list_issues` is deliberately excluded — it needs the rollup and is Task 10.

**Interfaces:**
- Consumes: `scope::ReadScope`, `scope_env!` from Task 3; the harness from Task 2.
- Produces: `repo::list_analytics_events(conn, scope: ReadScope, filters, q, since, limit, offset)` and `repo::list_sessions(conn, scope: ReadScope, since, limit, offset, distinct_id, device_key)`.

- [ ] **Step 1: Write the failing harness tests**

Add to `tests/env_scoping.rs`. Note the asymmetric seed counts — 3 rows in `env_a`, 2 in `env_b`, 1 unattributed — are what make a swapped filter or an off-by-one visible:

```rust
#[tokio::test]
async fn list_sessions_returns_only_the_selected_environment() {
    let Some(db) = TestDb::setup().await else { return };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let a = sauron_db::repo::list_sessions(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        far_past(), 100, 0, None, None,
    ).await.unwrap();
    assert_eq!(a.len(), 3, "env_a was seeded with 3 sessions");
    assert!(a.iter().all(|s| s.environment_id == Some(ids.env_a)));

    let b = sauron_db::repo::list_sessions(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
        far_past(), 100, 0, None, None,
    ).await.unwrap();
    assert_eq!(b.len(), 3, "env_b was seeded with 3 sessions");

    let none = sauron_db::repo::list_sessions(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::Unattributed),
        far_past(), 100, 0, None, None,
    ).await.unwrap();
    assert_eq!(none.len(), 1);
    assert!(none.iter().all(|s| s.environment_id.is_none()));

    let all = sauron_db::repo::list_sessions(
        &mut conn, ReadScope::all(ids.app_id), far_past(), 100, 0, None, None,
    ).await.unwrap();
    assert_eq!(all.len(), 7, "All must equal the sum of the parts, including unattributed");

    // NOTE: sessions are seeded 3/3/1, so env_a and env_b have the SAME count. A test that
    // asserts only lengths cannot tell a swapped filter from a correct one here — assert on
    // `environment_id` per row (as above) or on `avg_session_ms`, which differs by design.
    // The per-table counts are deliberately NOT uniform; see the `SeedIds` doc comment for
    // the authoritative table.

    // `drop(conn)` before `cleanup()` is REQUIRED, not tidiness. The harness pool is
    // sized 1, so holding a checked-out connection while `cleanup()` tries to check one
    // out deadlocks — it blocks for the 5s acquire timeout and then panics with a
    // confusing pool error rather than a test failure. Every test in this file needs it.
    drop(conn);
    db.cleanup().await;
}
```

Write the equivalent for `list_analytics_events`. **Note `analytics_events` is seeded 5/5/1 = 11**, not 3/2/1 — env_a and env_b hold the SAME number of rows, so a length assertion alone cannot distinguish a correct filter from a swapped one. Assert `environment_id` on every returned row as well: `One(env_a)` → 5 rows all carrying `env_a`, `One(env_b)` → 5 all carrying `env_b`, `Unattributed` → 1 with `environment_id` null, `All` → 11. The four-case shape is deliberate and every later test in this file repeats it — three of the four would still pass if the filter were inverted or dropped, and it is the combination that pins the behaviour.

- [ ] **Step 2: Run and watch it fail**

Run: `cd backend && cargo test -p sauron-db --test env_scoping`
Expected: FAIL to compile — the functions still take `app_id: Uuid`.

- [ ] **Step 3: Change `list_sessions`**

Replace its `app_id: Uuid` parameter with `scope: ReadScope`, and apply the env after the existing app filter:

```rust
pub async fn list_sessions(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    since: DateTime<Utc>,
    limit: i64,
    offset: i64,
    distinct_id: Option<&str>,
    device_key: Option<&str>,
) -> QueryResult<Vec<Session>> {
    let mut q = sessions::table
        .filter(sessions::app_id.eq(scope.app_id))
        .filter(sessions::last_event_at.ge(since))
        .into_boxed();
    q = crate::scope_env!(q, sessions, scope.env);
    if let Some(d) = distinct_id {
        q = q.filter(sessions::distinct_id.eq(d.to_string()));
    }
    if let Some(dk) = device_key {
        q = q.filter(sessions::device_key.eq(dk.to_string()));
    }
    q.select(Session::as_select())
        .order(sessions::last_event_at.desc())
        .limit(limit)
        .offset(offset)
        .load(conn)
        .await
}
```

- [ ] **Step 4: Change `list_analytics_events`**

Same parameter swap. This function already has environment handling for the legacy `filter=environment:eq:<name>` chip — **keep it**. The chip is retired from the dashboard in Task 15, but `EVENT_FILTERS` stays for API back-compatibility, so both paths must coexist: the topbar scope is the outer boundary, the legacy chip narrows within it. Apply `scope_env!` immediately after the `app_id` filter, before the per-filter loop.

- [ ] **Step 5: Fix the call sites**

`cargo check -p sauron-api` will name them. Pass `ReadScope::all(app_id)` for now — Task 11 wires the real value from the query parameter.

- [ ] **Step 6: Run and watch it pass**

Run: `cd backend && cargo test -p sauron-db --test env_scoping`
Expected: PASS.

- [ ] **Step 7: Gate**

Full backend gate, plus `cargo test -p sauron-db --test env_scoping` with `TEST_DATABASE_URL` set.

---

## Task 6: Thread env through the raw-SQL reads

**Files:**
- Modify: `backend/crates/sauron-db/src/repo.rs` — `top_events` (~1606), `event_series` (~1632), `error_series` (~1825), `journey_graph` (~2461), `performance_summary` (~2534), `performance_series` (~2575), `overview_totals` (~2130), `user_stats` (~2626), `active_user_series` (~2691), `session_stats` (~2774), `session_duration_series` (~2794), `session_duration_histogram` (~2811), `funnel` (~2369)
- Modify: `backend/crates/sauron-db/tests/env_scoping.rs`

**Interfaces:**
- Consumes: `EnvFilter::sql_fragment` / `bind_uuid` from Task 3.
- Produces: each of the above taking `scope: ReadScope` in place of `app_id: Uuid`.

**Check each function's pre-existing filters before writing its assertion.** `list_analytics_events` excludes synthetic `$screen` events, so it returns 4/3/1/8 rather than the raw table's 5/5/1/11 — but `top_events`, `event_series` and the rest have no such filter and see the full counts. Derive each expectation from the function's own SQL, not from the seed table alone.

**This is the riskiest task in the slice.** These are hand-written SQL strings with positional binds. A fragment appended without its bind, or a bind index off by one, produces a runtime error at best and a filter on the wrong value at worst — and `diesel::debug_query` cannot see any of it. Work one function at a time and add its harness test before moving to the next.

- [ ] **Step 1: Write the failing test for `top_events`**

```rust
#[tokio::test]
async fn top_events_counts_only_the_selected_environment() {
    let Some(db) = TestDb::setup().await else { return };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let a = sauron_db::repo::top_events(
        &mut conn, ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)), far_past(), 50,
    ).await.unwrap();
    let a_total: i64 = a.iter().map(|r| r.count).sum();
    assert_eq!(a_total, 5, "analytics_events is seeded 5/5/1");

    let all = sauron_db::repo::top_events(
        &mut conn, ReadScope::all(ids.app_id), far_past(), 50,
    ).await.unwrap();
    let all_total: i64 = all.iter().map(|r| r.count).sum();
    assert_eq!(all_total, 11, "All includes the unattributed row");

    // env_a and env_b both hold 5 analytics rows, so this total alone cannot tell a
    // correct filter from a swapped one. Assert the per-name breakdown differs too:
    // the seed gives the two environments different event-name mixes.
    let b = sauron_db::repo::top_events(
        &mut conn, ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)), far_past(), 50,
    ).await.unwrap();
    assert_ne!(a, b, "env_a and env_b must differ by name mix, not just by total");

    db.cleanup().await;
}
```

- [ ] **Step 2: Run and watch it fail**

Run: `cd backend && cargo test -p sauron-db --test env_scoping top_events`
Expected: FAIL to compile.

- [ ] **Step 3: Implement `top_events`**

The existing query binds `$1` app_id, `$2` since, `$3` limit. The env fragment must take the **next free index**, and the `limit` bind shifts only if the fragment consumed one — which is why `bind_uuid()` and `sql_fragment()` must be read together:

```rust
pub async fn top_events(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    since: DateTime<Utc>,
    limit: i64,
) -> QueryResult<Vec<EventCount>> {
    // The env fragment takes $3 when it needs a bind; `limit` therefore lands on
    // $4 in that case and $3 otherwise. Deriving both from the same `EnvFilter`
    // is what keeps the string and the bind sequence in agreement — see
    // `EnvFilter::sql_fragment`'s doc for why only `One` consumes an index.
    let env_bind = scope.env.bind_uuid();
    let env_sql = scope.env.sql_fragment(3);
    let limit_idx = if env_bind.is_some() { 4 } else { 3 };

    let q = format!(
        "SELECT name, count(*)::bigint AS count FROM analytics_events \
         WHERE app_id = $1 AND occurred_at >= $2{env_sql} \
         GROUP BY name ORDER BY count DESC LIMIT ${limit_idx}"
    );
    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Timestamptz, _>(since);
    if let Some(id) = env_bind {
        stmt = stmt.bind::<SqlUuid, _>(id);
    }
    stmt.bind::<BigInt, _>(limit).get_results(conn).await
}
```

If `diesel::sql_query(..).into_boxed()` does not support conditional binding in this diesel version, fall back to matching on `EnvFilter` and writing the two concrete bind chains explicitly. **Report which form you used** — do not silently restructure the query.

- [ ] **Step 4: Run and watch it pass**

Run: `cd backend && cargo test -p sauron-db --test env_scoping top_events`
Expected: PASS.

- [ ] **Step 5: Repeat for each remaining function, one at a time**

Work through these in order, and for **each one** complete the full cycle before starting
the next: write its harness test, run it and watch it fail, implement, run it and watch it
pass. Do not batch — the entire value of this task is that each hand-edited SQL string is
verified independently, and a batched implementation loses exactly that.

| Function | repo.rs | Assertion for `One(env_a)` against the seed |
|---|---|---|
| `event_series` | ~1632 | bucket counts sum to **5** (analytics is 5/5/1 and this fn has NO `$screen` filter, unlike `list_analytics_events`) |
| `error_series` | ~1825 | bucket counts sum to **4** (error_events is seeded 4/2/1) |
| `journey_graph` | ~2461 | node counts reflect only `env_a`'s events |
| `performance_summary` | ~2534 | `count` column is **5** (transactions are seeded 5/2/1); p50/p95/avg and `error_rate` (0.2 vs 0.5) also discriminate |
| `performance_series` | ~2575 | bucket counts sum to **5** (transactions are 5/2/1) |
| `overview_totals` | ~2130 | every one of its four totals is `env_a`'s, not app-wide |
| `user_stats` | ~2626 | distinct-user count covers `env_a` only |
| `active_user_series` | ~2691 | bucket counts sum to `env_a`'s users |
| `session_stats` | ~2774 | assert `avg_session_ms` (120000 vs 400000), NOT the count — sessions are 3/3/1, so the count alone cannot distinguish env_a from env_b |
| `session_duration_series` | ~2794 | buckets cover `env_a`'s sessions only |
| `session_duration_histogram` | ~2811 | env_a's sessions (60/120/180s) all land in the `1-5m` bin, env_b's (300/400/500s) in `5-30m` — a swapped filter changes the bin LABEL, not just a number |
| `funnel` | ~2369 | step counts reflect only `env_a` |

Every test also asserts the `All` case equals the sum including the unattributed row — that
is what catches a fragment that was appended but whose bind was never supplied, since such a
query usually still returns *something*.

`performance_summary` and `performance_series` already carry the `($3::text IS NULL OR op=$3)` optional-filter idiom; follow it rather than inventing a second style, and be careful that the existing optional binds and the new env bind do not collide.

`overview_totals` aggregates across four tables in one statement — its env fragment needs `sql_fragment_for` with the right alias per sub-select.

- [ ] **Step 6: Sweep for missed functions**

```bash
cd backend && grep -n "app_id: Uuid" crates/sauron-db/src/repo.rs
```
Every remaining hit must be either a write function, a config read (`saved_funnels`, `symbol_artifacts`), or a function explicitly deferred to a later task. List them in your report with which category each falls into — an unclassified hit is a missed read.

**This grep has a blind spot: it only finds functions whose tenant parameter is named
`app_id`.** Reads scoped by some other key — `latest_error_event` and
`issue_occurrence_series` take `issue_id`, several session reads take `session_id` — are
invisible to it. Also run:

```bash
cd backend && grep -n "FROM error_events\|FROM analytics_events\|FROM transactions\|FROM sessions\|error_events::table\|analytics_events::table\|transactions::table\|sessions::table" crates/sauron-db/src/repo.rs
```

Every hit reads a table carrying `environment_id`. Classify those too.

- [ ] **Step 7: Gate**

Full backend gate plus the harness suite.

---

## Task 7: Screens — the CTE reads

**Files:**
- Modify: `backend/crates/sauron-db/src/repo.rs` — `screen_ctes` (~3063), `screen_list` (~3094), `screen_stats` (~3125), `recent_events_for_screen` (~3153), `recent_exceptions_for_screen` (~3172)
- Modify: `backend/crates/sauron-db/tests/env_scoping.rs`

**Interfaces:**
- Consumes: Task 3's fragment helper.
- Produces: `screen_ctes(pred: &str, env_sql: &str) -> String` and the four callers taking `ReadScope`.

There is no `screens` table — everything derives from `analytics_events.screen` and `error_events.screen`, both of which carry `environment_id`. `screen_ctes` already interpolates a predicate string, so this is the one raw-SQL case with an existing seam.

- [ ] **Step 1: Write the failing test**

Assert that a screen seeded with views in both environments reports only `env_a`'s view count under `EnvFilter::One(env_a)`, and the sum under `All`.

- [ ] **Step 2: Run and watch it fail**

Run: `cd backend && cargo test -p sauron-db --test env_scoping screen`
Expected: FAIL to compile.

- [ ] **Step 3: Implement**

`screen_ctes` gains a second parameter and interpolates it into **all four** CTEs — `ev`, `ex`, `us` and `dw`. The `us` CTE has two arms inside a `UNION ALL` and the `dw` CTE applies `{pred}` in an outer `WHERE` over a window subquery; every one needs the env fragment, and the `dw` case needs it **inside** the subquery where `environment_id` is in scope, not in the outer filter over `raw_ms`.

Miss one arm and the counts silently mix environments in a single column while the others are correct — which is why the test asserts each returned column, not just the row count.

- [ ] **Step 4: Run and watch it pass, then gate**

Full backend gate plus the harness suite.

---

## Task 8: Persons and devices — the LATERAL reads

**Files:**
- Modify: `backend/crates/sauron-db/src/repo.rs` — `list_persons` (~2065), `get_event_user` (~1568), `events_for_person` (~1582), `error_events_for_person` (~1437), `list_devices` (~1971), `get_device` (~2013), `errors_for_device` (~2027)
- Modify: `backend/crates/sauron-db/tests/env_scoping.rs`

**Interfaces:**
- Consumes: Task 3's fragment helper.
- Produces: the above taking `ReadScope`.

Neither `event_users` nor `devices` has an `environment_id` column, but neither needs a rollup: their counts already come from LATERAL subqueries over tables that do.

- [ ] **Step 1: Write the failing test**

Assert that under `EnvFilter::One(env_a)`, a person seeded with activity in both environments reports only `env_a`'s `events_count` / `errors_count` / `sessions_count`, and that a person active *only* in `env_b` does not appear at all.

- [ ] **Step 2: Run and watch it fail, then implement**

Two changes per function:

1. **The counts** — add the env fragment inside each LATERAL subquery. `list_persons` has three (`analytics_events`, `error_events`, `sessions`); each already filters `app_id=$1 AND distinct_id = eu.distinct_id`.
2. **The membership** — the outer `event_users` page must not list a person with no activity in the selected environment. Add an `EXISTS` over the same three tables, guarded so it is omitted entirely under `EnvFilter::All` (where it would be pure overhead).

For `list_devices`, additionally move `events_count` and `errors_count` off the denormalized `devices` columns onto LATERAL subqueries, matching what `list_persons` already does. Those columns are maintained by `repo::bump_device` and are cross-environment, so leaving them would put an unscoped number next to scoped ones in the same row.

- [ ] **Step 3: Verify the paging comment still holds**

`list_persons` carries a comment explaining that it pages *first* and counts per returned row via LATERAL, because Postgres cannot push the outer LIMIT into a grouped subquery. Adding an `EXISTS` to the inner page preserves that shape — confirm with `EXPLAIN` that the plan still applies the LIMIT before the LATERAL joins, and paste the plan in your report. If it regresses to aggregating the whole history, stop and report rather than shipping it.

- [ ] **Step 4: Gate**

Full backend gate plus the harness suite.

---

## Task 9: Issue reads compute per-environment counts

**Files:**
- Modify: `backend/crates/sauron-db/src/repo.rs` — `list_issues` (~1224), `get_issue` (~1325), `issue_stats` (~2190), `top_issues` (~2150), `issue_occurrence_series` (~1692), `list_error_events_for_issue` (~1372), **`latest_error_event` (~1424)**

**`latest_error_event` is easy to miss and matters.** It reads `error_events` filtered only by
`issue_id`, so it takes `issue_id: Uuid` and the `grep "app_id: Uuid"` sweep used in Task 6
**structurally cannot find it**. Left unscoped, `GET /v1/apps/{app}/issues/{id}?environment_id=<staging>`
renders a *production* stack trace, release string and device context inside a page the user
believes is scoped to staging — with no error and no marker. Scope it with the same
`ReadScope` the other issue reads take.
- Modify: `backend/crates/sauron-db/tests/env_scoping.rs`

**Interfaces:**
- Consumes: Task 4's `error_events_issue_env_idx`; Task 3's `ReadScope`.
- Produces: `list_issues(conn, scope: ReadScope, filters, q, since, limit, offset)` returning issues whose `times_seen` / `users_seen` / `first_seen` / `last_seen` reflect the scope.

`issues` has no `environment_id` and — following Task 1's measurement — no rollup table. Under a specific environment the counts are derived from `error_events` at read time.

- [ ] **Step 1: Write the failing test**

The assertion that matters is the *counts*, not mere presence. The seeded `issue_id` has 6 error events split **4 in `env_a`, 1 in `env_b`, 1 unattributed**, so it must report `times_seen == 4` under `One(env_a)`, `1` under `One(env_b)`, and `6` under `All`. A membership-only filter would return the issue in all three cases with `times_seen == 5` — precisely the bug this task exists to prevent, and it is invisible unless the test reads the number.

```rust
#[tokio::test]
async fn list_issues_reports_per_environment_counts_not_app_wide() {
    let Some(db) = TestDb::setup().await else { return };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let a = sauron_db::repo::list_issues(
        &mut conn, ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        &[], None, far_past(), 50, 0,
    ).await.unwrap();
    let issue = a.iter().find(|i| i.id == ids.issue_id).expect("issue appears under env_a");
    assert_eq!(issue.times_seen, 4, "must be env_a's count, not the app-wide 6");

    let b = sauron_db::repo::list_issues(
        &mut conn, ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
        &[], None, far_past(), 50, 0,
    ).await.unwrap();
    assert_eq!(b.iter().find(|i| i.id == ids.issue_id).unwrap().times_seen, 1);

    let all = sauron_db::repo::list_issues(
        &mut conn, ReadScope::all(ids.app_id), &[], None, far_past(), 50, 0,
    ).await.unwrap();
    assert_eq!(all.iter().find(|i| i.id == ids.issue_id).unwrap().times_seen, 6);

    drop(conn); // required before cleanup(): the pool is sized 1 and would deadlock
    db.cleanup().await;
}
```

Add a second test asserting `ids.issue_env_b_only` — an issue the harness seeds with a single occurrence confined to `env_b` — does **not** appear at all under `One(env_a)`. Without that second issue the inner-join-vs-`LEFT JOIN` mistake this task warns about is invisible, because a single issue present in every bucket is returned either way.

- [ ] **Step 2: Run and watch it fail**

Run: `cd backend && cargo test -p sauron-db --test env_scoping list_issues`
Expected: FAIL to compile — `list_issues` still takes `app_id: Uuid`.

- [ ] **Step 3: Implement**

Two distinct paths, and keeping them separate is the point:

**`EnvFilter::All` keeps today's query unchanged** — reads `issues` directly, no join, no subquery. The default case must not regress, and it is the case almost every request takes.

**`One` / `Unattributed`** page the issues first, then compute the four aggregates per returned row via a LATERAL over `error_events` — the same shape `list_persons` uses and that Task 8 extends to `list_devices`, so the slice has one idiom rather than two. Project the derived values over the issue's own columns so the returned `Issue` carries the scoped numbers.

The LATERAL doubles as the membership filter: make it an inner join, and an issue with no occurrences in the selected environment simply does not appear. No separate `EXISTS` is needed.

Apply `since` to the *derived* `last_seen`, not the issue's own. An issue last seen in `env_b` yesterday but in `env_a` last month must not appear in a 7-day `env_a` view merely because its app-wide `last_seen` is recent.

- [ ] **Step 4: Run and watch it pass**

Run: `cd backend && cargo test -p sauron-db --test env_scoping list_issues`
Expected: PASS.

- [ ] **Step 5: Verify the plan pages before it aggregates**

This is the risk the measurement traded into: read cost. `list_persons` carries a comment explaining it pages *first* and counts per returned row, because Postgres cannot push an outer LIMIT into a grouped subquery — get this wrong and every Issues page load aggregates the app's entire error history.

```bash
psql "$DATABASE_URL" -c "EXPLAIN (ANALYZE, BUFFERS) <the query list_issues generates under One(env)>"
```

Paste the plan. The LIMIT must be applied before the LATERAL. If it is not, **stop and report** — do not ship a query that scans all history per page load.

- [ ] **Step 6: Note the two documented discrepancies**

Add a doc comment on `list_issues` recording both, so a future reader does not treat them as bugs:

1. Per-environment `users_seen` is an exact `count(DISTINCT distinct_id)`, while the app-wide figure comes from a Redis HyperLogLog and is approximate. They will disagree slightly; the per-environment number is the more accurate one.
2. Per-environment counts cannot see tiered data. Once a partition is exported to Parquet and dropped, its occurrences leave `error_events`, so a window older than `TIER_HOT_DAYS` under-reports — whereas `issues.times_seen` does not, having been incremented at ingest.

- [ ] **Step 7: Gate**

Full backend gate plus the harness suite.

---

## Task 10: The wire contract

> **Ordering constraint: `event_users`-backed fields must be scoped before this task ships.**
> From the moment `environment_id` is accepted over HTTP, `overview_totals` returns a single
> JSON object mixing scoped and unscoped numbers with no marker — `events: 5` (staging)
> beside `users: 7` (app-wide) on the same Overview card. That pair is internally impossible
> and nothing in the response explains why.
>
> **This constraint was violated once, and the way it happened is worth recording.** Task 6
> deferred the `event_users` fields in `overview_totals`, `user_stats` and
> `active_user_series` "until Task 8's EXISTS-based membership filter exists" — but recorded
> that only in its *report*. Task 8's own file list named `list_persons` / `list_devices` and
> never those three functions, so Task 8 closed the gap for the entities it owned and left
> these three untouched. This section then asserted Task 8 had closed it, repeating a claim
> nobody had checked. The bug shipped live at Task 10 and was caught by the user, not by any
> review.
>
> **The rule that would have prevented it: a deferral is not complete until it appears in the
> receiving task's Files list.** A note in a report is not a handoff — nothing reads reports
> when generating the next brief. If you defer work to a later task, edit that task.


**Files:**
- Create: `backend/bins/sauron-api/src/routes/scope.rs` — the extractor and its tests
- Modify: `backend/bins/sauron-api/src/routes/mod.rs`
- Modify: the 22 handlers across `analytics.rs`, `issues.rs`, `sessions.rs`, `devices.rs`, `screens.rs`, `journeys.rs`, `performance.rs`, `funnels.rs`

**Interfaces:**
- Consumes: `scope::ReadScope` / `EnvFilter`.
- Produces: `routes::scope::parse_env(raw: Option<&str>) -> Result<EnvFilter, ApiError>`.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_means_all() {
        assert_eq!(parse_env(None).unwrap(), EnvFilter::All);
    }

    #[test]
    fn none_means_unattributed() {
        assert_eq!(parse_env(Some("none")).unwrap(), EnvFilter::Unattributed);
    }

    #[test]
    fn a_uuid_selects_one() {
        let id = Uuid::from_u128(42);
        assert_eq!(parse_env(Some(&id.to_string())).unwrap(), EnvFilter::One(id));
    }

    /// A malformed value is a 400, NOT a silent fallback to All. Falling back
    /// would show the caller MORE data than they asked for, which is the wrong
    /// direction to fail on a scoping parameter.
    #[test]
    fn malformed_is_rejected_not_widened() {
        assert!(parse_env(Some("not-a-uuid")).is_err());
        assert!(parse_env(Some("")).is_err());
    }
}
```

- [ ] **Step 2: Run and watch it fail, then implement**

Add `environment_id: Option<String>` to each handler's query struct, call `parse_env`, build a `ReadScope`, and pass it through.

- [ ] **Step 3: Reject the parameter where it cannot be honoured**

The three timeseries handlers (`error_timeseries`, `event_timeseries`, `transaction_timeseries`) read across hot Postgres and cold Parquet, and cold has no usable environment pruning. They must return **400** with a message naming the limitation rather than silently returning hot-only numbers:

```rust
    if q.environment_id.is_some() {
        return Err(ApiError::BadRequest(
            "environment scoping is not available on cross-tier timeseries yet — \
             cold storage is not partitioned by environment".into(),
        ));
    }
```

- [ ] **Step 4: Verify each of the 22 handlers over HTTP**

For each: a request with no parameter returns data, one with a valid environment returns a subset, `none` returns the unattributed rows, and a malformed value returns 400. A table of 22 rows × 4 cases in your report.

- [ ] **Step 5: Gate**

Full backend gate.

---

## Task 11: Session store — the fourth level

**Files:**
- Modify: `dashboard/src/lib/stores/session.svelte.ts`
- Create: `dashboard/src/lib/stores/session.test.ts`

**Interfaces:**
- Consumes: `listEnvironments` from `lib/api/environments.ts`.
- Produces: `sessionStore.currentEnvId: string | null`, `sessionStore.environments: Environment[]`, `sessionStore.setEnvironment(id: string | null): void`, `sessionStore.scopeKey: string`.

`currentEnvId` is `null` for "All environments" and the literal `'none'` for Unattributed.

- [ ] **Step 1: Write the failing tests**

Cover: `resolveCurrentEnvironment` picks the `is_default` environment rather than `[0]`; `setApp` clears the stored environment (it belongs to the previous app); `reset()` clears the new key; and `scopeKey` changes when either the app or the environment changes.

- [ ] **Step 2: Run and watch it fail, then implement**

Add a `sauron.environment_id` key alongside the existing three. Add `loadAppEnvironments` and `resolveCurrentEnvironment` mirroring `loadProjectApps` / `resolveCurrentApp`. **`setApp` becomes async** — it must load the new app's environments — which changes `Topbar.svelte:65` and `selectApp`.

The new key must be cleared in `setOrg`, `setProject`, `setApp`, `removeApp` and `reset` — the same five places the existing keys are cleared.

```ts
  /// Changes whenever the data on screen should be refetched. Telemetry pages key
  /// their effects on this rather than on `currentAppId` alone: an effect that
  /// tracks only the app will not re-run when the environment changes, leaving
  /// the previous environment's data on screen. That exact bug shipped once in
  /// Docs.svelte and was caught in review; here there would be 24 chances for it.
  get scopeKey(): string {
    return `${this.currentAppId ?? ''}:${this.currentEnvId ?? 'all'}`;
  }
```

- [ ] **Step 3: Run tests, typecheck, gate**

`cd dashboard && npm test && npx svelte-check --tsconfig ./tsconfig.json`

---

## Task 12: The interceptor and the effect rewiring

**Files:**
- Create: `dashboard/src/lib/api/scope.ts`
- Modify: `dashboard/src/lib/api/client.ts`
- Modify: 15 telemetry pages (24 `$effect` blocks)
- Create: `dashboard/src/lib/api/scope.test.ts`

**Interfaces:**
- Consumes: `sessionStore.currentEnvId` / `scopeKey`.
- Produces: the interceptor, and every telemetry effect keyed on `scopeKey`.

- [ ] **Step 1: Write the failing tests for the opt-out list**

```ts
// Reads that must NOT be environment-scoped. `listEnvironments` is the source of
// the list itself; the rest are app configuration rather than telemetry, and
// scoping them would filter a list that has no environment dimension at all.
const UNSCOPED = [
  '/environments',
  '/first-event',
  '/funnels',
  '/artifacts',
];
```

Test that a telemetry URL gets the parameter, that each opt-out URL does not, and that `currentEnvId === null` adds nothing.

- [ ] **Step 2: Run and watch it fail, then implement**

Add a second request interceptor to `api` in `client.ts`, after the existing bearer-token one. Import the predicate from `scope.ts` rather than the store directly, and have `scope.ts` do the store import — `client.ts` already documents that it must not import stores directly to avoid an import cycle, and that constraint still applies.

- [ ] **Step 3: Rewire the 24 effects**

In each telemetry page, the idiom

```ts
  $effect(() => {
    const aid = sessionStore.currentAppId;
    const days = sinceDays;
    if (aid) void load(aid, days);
  });
```

becomes

```ts
  $effect(() => {
    const aid = sessionStore.currentAppId;
    // Touch scopeKey so the effect re-runs when the environment changes; the
    // interceptor supplies the value, but nothing would refetch without this.
    sessionStore.scopeKey;
    const days = sinceDays;
    if (aid) void load(aid, days);
  });
```

Pages: `Overview`, `Issues`, `IssueDetail`, `Events`, `Performance`, `SessionsList`, `SessionDetail`, `UsersExplorer`, `PersonProfile`, `DevicesInventory`, `DeviceDetail`, `ScreensList`, `ScreenDetail`, `FunnelBuilder`, `JourneyExplorer`.

Note `SessionsList` has **two** effects and `FunnelBuilder` reads the app id inside `untrack()` — read each before editing.

- [ ] **Step 4: Add the guard test**

A vitest that reads each telemetry page's source and asserts it references `scopeKey`. Crude, but it is what makes a missed page findable rather than a silent stale-data bug. Precedent: `permissions.test.ts` already parses Rust source off disk for the same reason.

- [ ] **Step 5: Gate**

`cd dashboard && npm test && npm run build`

---

## Task 13: The topbar switcher

> **Carry-forward from Task 11's review — do not assume `environments` is fresh.**
> `removeApp` is synchronous and does not reload the replacement app's environments, and
> `setApp` now has a same-id no-op guard — so calling `setApp(currentAppId)` to force a
> reload **will not work**. If `currentAppId` is non-null but `environments` is empty, treat
> that as "needs load" and trigger it, rather than assuming the two are always in step.


**Files:**
- Modify: `dashboard/src/lib/components/layout/Topbar.svelte`

**Interfaces:**
- Consumes: Task 12's store surface.
- Produces: the user-visible switch. **This is the task that turns the feature on** — everything before it is invisible.

- [ ] **Step 1: Add the switcher**

A fourth `SwitcherMenu` — the component is already generic and needs no changes. Items: `All environments` (id `''`), each live environment, and `Unattributed` (id `'none'`).

```svelte
  const envItems = $derived([
    { id: '', name: 'All environments' },
    ...sessionStore.environments.map((e) => ({ id: e.id, name: e.name })),
    { id: 'none', name: 'Unattributed' },
  ]);
```

Wire `onSelect` through a `void` wrapper, as `setOrg`/`setProject` already do.

- [ ] **Step 2: Handle the layout**

`.left` is `overflow: hidden` and `SwitcherMenu`'s `.name` caps at 180px (110px below 640px). Four triggers plus three separators will truncate. Below 860px drop the environment switcher's `Env` label chip; below 640px hide the *project* switcher's name — app and environment are what change the meaning of the data on screen.

- [ ] **Step 3: Verify in a browser**

Use `preview_*`. Confirm: the picker lists all live environments plus both pseudo-entries; selecting one refetches every page without a reload; the selection survives a reload; switching app resets the environment to that app's default. `preview_screenshot` has been timing out in this environment — use `preview_snapshot`.

- [ ] **Step 4: Gate**

`cd dashboard && npm test && npm run build`

---

## Task 14: Retire the Events chip

**Files:**
- Modify: `dashboard/src/lib/components/filters/filters.ts`
- Modify: `dashboard/src/pages/Events.svelte`
- Modify: `dashboard/src/pages/Docs.svelte` (the filter-reference row)
- Modify: `dashboard/src/lib/components/filters/filters.test.ts`

- [ ] **Step 1: Write the failing test**

```ts
it('drops an environment filter from an old shared URL rather than erroring', () => {
  // The chip moved to the topbar. parseFilters already discards unknown fields,
  // so a link shared before this change still loads — it just no longer
  // constrains environment. Asserted so the graceful degradation is deliberate
  // rather than incidental.
  expect(parseFilters(['environment:eq:prod', 'name:eq:click'], EVENT_FIELDS))
    .toEqual([{ field: 'name', op: 'eq', value: 'click' }]);
});
```

- [ ] **Step 2: Run and watch it fail, then implement**

Remove the `environment` entry from `EVENT_FIELDS` and its comment. In `Events.svelte`, remove `loadEnvironmentOptions`, the `eventFields` state, its `$effect`, and the `listEnvironments` import; pass the static `EVENT_FIELDS` to `FilterBar`. `FilterBar.svelte` needs no changes — it is fully data-driven.

Leave the backend's `EVENT_FILTERS` entry in place for API back-compatibility.

- [ ] **Step 3: Gate**

`cd dashboard && npm test && npm run build`

---

## Task 15: Live end-to-end verification

**Files:** none — this task changes nothing.

- [ ] **Step 1: Bring up the full stack and seed two environments with distinguishable traffic**

- [ ] **Step 2: The central assertion**

With `prod` selected in the topbar, every page shows only `prod` data. Verify each against SQL, not by eye: Overview, Issues, Events, Sessions, Users, Devices, Screens, Performance, Journeys, Funnels.

- [ ] **Step 3: The aggregate assertion**

An issue with occurrences in both environments shows each environment's own `times_seen` and `users_seen` under that environment, and their sum under "All environments". This is the requirement that motivated the rollup — assert it explicitly.

- [ ] **Step 4: Unattributed**

Insert a row with `environment_id = NULL`, confirm it appears under `Unattributed` and under `All`, and **that "All" equals the sum of every environment plus Unattributed** on at least three pages.

- [ ] **Step 5: The deferred endpoints**

`GET /v1/apps/{id}/errors/timeseries?environment_id=<uuid>` returns 400 with the cold-storage message. Without the parameter it still returns data.

- [ ] **Step 6: Switching**

Switching environment refetches every open page with no reload; switching app resets to the new app's default environment; both survive a reload.

- [ ] **Step 7: Full gate**

```
cd backend && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
TEST_DATABASE_URL=... cargo test -p sauron-db --test env_scoping
cd ../dashboard && npx svelte-check --tsconfig ./tsconfig.json && npm test && npm run build
```
