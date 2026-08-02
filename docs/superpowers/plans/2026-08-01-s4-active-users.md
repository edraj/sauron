# Active Users (S4) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship a project-scoped "combined active users per UTC day" report — split into total / identified / guest, across a caller-chosen set of (app, environment) pairs — as a JSON endpoint, a CSV download and a dashboard page.

**Architecture:** A new `event_users.identified_at` flag (written first-write-wins by the ingest pipeline) is what lets one `distinct_id` count once across several apps; everything else is a single raw-SQL aggregate in `sauron-db` (`active_users_combined`) that dedups `(app_id, distinct_id, day)` before joining `event_users`, plus one new API module (`routes/active_users.rs`) that resolves per-selection environment authorization, clamps the window to the hot-tier watermark, caches in Redis and renders both JSON and CSV from one `build_report`. The dashboard gets a new page with a per-app environment picker, a chart, five tiles and an export button.

**Tech Stack:** Rust 1.82, axum 0.8, `axum_extra::extract::Query` (serde_html_form), diesel + diesel-async + Postgres, `sauron-redis`, `tokio::sync::Semaphore`, chrono; Svelte 5 runes + vitest; TypeScript in `sdks/js`.

## Global Constraints

- **NEVER run a git commit, git add, or branch command.** The repository owner commits manually.
- Never use `conn.transaction(...)` — the 1.82 MSRV blocks it. Multi-statement atomicity is one data-modifying CTE via `diesel::sql_query` with `.bind()`.
- `backend/crates/sauron-db/src/schema.rs` is HAND-MAINTAINED. The diesel CLI must NEVER run. This slice adds **no new table**, so there is no `diesel::joinable!` edit and no `allow_tables_to_appear_in_same_query!` edit — only two appended fields in the existing `event_users` `table!` block.
- Migrations live at `backend/migrations/YYYY-MM-DD-0000NN_slug/{up,down}.sql`; BOTH files are required; `up.sql` opens with a prose comment explaining WHY. A migration runs in ONE transaction; `CONCURRENTLY` is unavailable; an index build on a partitioned parent locks every child.
- This slice owns migration numbers **000038, 000039, 000040** and no others. The date prefix is the LANDING date and must never decrease as NN increases (diesel orders by the full `YYYY-MM-DD-0000NN` string, date first). If these land after 2026-08-01, rename the directories to the landing date, keeping 38 < 39 < 40 monotone.
- Enum-like columns are TEXT + CHECK, never custom SQL types.
- All SQL lives in `backend/crates/sauron-db/src/repo.rs` as free `pub async fn name(conn: &mut AsyncPgConnection, ...) -> QueryResult<T>`. Handlers never build queries inline.
- Insertable-only structs must NOT gain a `Queryable` derive.
- Never hold a pooled `PgConn` across network I/O. The API pool is 16 connections for the whole process. `drop(conn)` before every Redis call, then check out again.
- Dashboard: house UI components only. There is NO Select, Toggle, Tabs or Menu primitive — raw `<select>`/`<input type="checkbox">` inside a `<label>` is the established idiom. A new page needs three edits: the page file, `src/routes.ts`, and the `Sidebar.svelte` `groups` array. Pure decision logic goes in `src/lib/models/*.ts` with a colocated `*.test.ts` — there is NO DOM test environment.
- Svelte 5 runes. `$state` deep-proxies values so `===` never matches a raw value; use `$state.raw` when identity matters. Sets and Records in `$state` are **replaced**, never mutated in place.
- Comments explain the failure mode that motivated the code, not what the code does.
- `clippy` runs with `-D warnings` on `--all-targets`; `cargo fmt --all --check` is a hard gate.
- **No config change of any kind.** No new environment variable, no changed default, no new workspace dependency (`csv`, `futures`, `tokio-util`, tokio `fs` all stay out), nothing added to `packaging/rpm/binaries.txt` or `sauron.spec`.
- **No new permission.** Both routes gate on the existing `perm::EVENT_READ`, so `perm::ALL`, `rbac.rs`'s four preset-role count assertions, `dashboard/src/lib/models/permissions.ts` and the `Permission` union are all untouched.
- `MAX_ACTIVE_USER_DAYS = 92`, `MAX_SELECTED_APPS = 20`, `MAX_SCAN_BUDGET = 1200`, `ACTIVE_USERS_CACHE_TTL_SECS = 60`, rate limit `30` per `60` seconds under key `sauron:analytics:active_users:{user_id}`, semaphore `3` permits, Redis op timeout `500 ms`.
- Every day boundary is 00:00 UTC. Day bucketing is `(occurred_at AT TIME ZONE 'UTC')::date`, never `date_trunc('day', …)` (which reads the session `TimeZone` GUC nothing sets).

### Commands used verbatim throughout

```bash
# Rust build/check (all crates, all targets)
cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo check --workspace --all-targets

# Rust format + lint gates
cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all --check
cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings

# Apply migrations to the live dev database
cd /home/splimter/projects/freelance/sauron/backend && DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron cargo run --bin sauron-migrate

# Dashboard
cd /home/splimter/projects/freelance/sauron/dashboard && npm run test
cd /home/splimter/projects/freelance/sauron/dashboard && npm run check

# Browser SDK
cd /home/splimter/projects/freelance/sauron/sdks/js && npm run test
cd /home/splimter/projects/freelance/sauron/sdks/js && npm run typecheck
```

Postgres-backed Rust tests **skip silently** unless `TEST_DATABASE_URL` (and, for HTTP tests, `TEST_REDIS_URL`) are set. A "skipping" line in the output is NOT a pass — every step below that expects a real assertion sets both.

---

## File Structure

**New files**

| Path | Responsibility |
|---|---|
| `backend/migrations/2026-08-01-000038_event_users_identified/up.sql` | Add `event_users.identified_at` + `identified_source`, index `identities (app_id, distinct_id)`, backfill from `properties`/`identities`, partial index on identified rows |
| `backend/migrations/2026-08-01-000038_event_users_identified/down.sql` | Drop both indexes and both columns |
| `backend/migrations/2026-08-01-000039_analytics_active_user_index/{up,down}.sql` | Substitute `analytics_events_app_env_time_idx` with an `INCLUDE (distinct_id)` variant |
| `backend/migrations/2026-08-01-000040_error_active_user_index/{up,down}.sql` | Same substitution on `error_events` |
| `backend/bins/sauron-api/src/csv.rs` | RFC 4180 field escaping + row writer + formula-injection guard. The repo's first CSV primitive; S5 reuses it |
| `backend/bins/sauron-api/src/routes/active_users.rs` | `ActiveUsersQuery`, `parse_selection`, `validate_window`, `latest_full_day`, cache fingerprint, `build_report`, the JSON and CSV handlers |
| `backend/bins/sauron-api/tests/http_active_users.rs` | End-to-end HTTP tests against the spawned binary |
| `dashboard/src/lib/models/active-users.ts` + `.test.ts` | Pure selection encode/decode/validate/describe, default window, UTC day label |
| `dashboard/src/lib/api/activeUsers.ts` | `getActiveUsers`, the CSV path builder, the repeated-key params serializer |
| `dashboard/src/lib/api/download.ts` + `download.test.ts` | Blob download helper + `Content-Disposition` filename parser |
| `dashboard/src/lib/api/client.test.ts` | Blob-bodied error body unwrap |
| `dashboard/src/lib/components/AppEnvPicker.svelte` | One row per app: checkbox + environment `<select>` |
| `dashboard/src/pages/ActiveUsers.svelte` | The page |
| `wiki/Active-Users.md` | User-facing docs, carrying the exact-string-equality caveat and the PII-mask warning |

**Modified files**

| Path | Change |
|---|---|
| `backend/crates/sauron-db/src/schema.rs` | Append `identified_at` + `identified_source` to the `event_users` `table!` block |
| `backend/crates/sauron-db/src/models.rs` | Append the same two fields, same order, to `EventUser` |
| `backend/crates/sauron-db/src/scope.rs` | `EnvFilter` gains `serde::Serialize` |
| `backend/crates/sauron-db/src/repo.rs` | `IDENTIFIED_SOURCE_*` consts, `mark_event_user_identified`, `probe_event_users_identified`, `env_ids_for_apps`, `AppEnvScope`, `ActiveUserDay`, `active_users_combined`, `user_stats` gains a `now` parameter |
| `backend/crates/sauron-db/tests/common/mod.rs` | `note_identity` gains `identified: bool`; new `seed_identified_user` / `seed_signal_event` / `seed_signal_error` helpers |
| `backend/crates/sauron-db/tests/env_scoping.rs` | 5 `user_stats` call sites; 10 new tests |
| `backend/crates/sauron-pipeline/src/process.rs` | Identification stamping on the identify/event/error paths; `touch_event_user` errors logged instead of discarded; new pipeline tests |
| `backend/bins/sauron-api/src/error.rs` | `ApiError::Unavailable(&'static str, String)` → 503 with its own code |
| `backend/bins/sauron-api/src/main.rs` | `mod csv;`, `AppState.active_users_gate` + `AppState.event_users_identified`, boot schema probe, two routes, CORS `expose_headers` |
| `backend/bins/sauron-api/src/routes/mod.rs` | `pub mod active_users;` |
| `backend/bins/sauron-api/src/routes/analytics.rs` | `user_stats` call site passes `Utc::now()` |
| `backend/bins/sauron-api/src/routes/auth.rs` | `rate_limit` / `client_addr` widened to `pub(crate)` if S2 has not already done it |
| `backend/bins/sauron-api/tests/http_env_scoping.rs` | `EnvScopedFixture.project_id`; project-scoped route enumeration + correspondence test |
| `dashboard/src/lib/api/scope.ts` | Exported `PROJECT_SCOPED_REJECTS_ENVIRONMENT_ID` array |
| `dashboard/src/lib/api/client.ts` | `unwrapBlobErrorBody` called before the status branching |
| `dashboard/src/lib/models/index.ts` | `ActiveUsersReport`, `ActiveUserPoint`, `SelectionView`, `ReportWindow` |
| `dashboard/src/lib/components/TimeSeriesChart.svelte` | Optional `label` prop used for both the axis and the tooltip |
| `dashboard/src/lib/components/ui/Icon.svelte` | `download` registry entry |
| `dashboard/src/lib/components/layout/Sidebar.svelte` | Nav item in the Analyze group |
| `dashboard/src/routes.ts` | `/active-users` route |
| `dashboard/src/pages/UsersExplorer.svelte` | Missing DAU tile + a link to `#/active-users` |
| `sdks/js/src/identity.ts` | Persisted `sauron.anon_id` + `resetAnonymousId` |
| `sdks/js/src/client.ts` | Anonymous id read through `identity.ts`; `reset()`; anonymous-id-was-used flag |
| `sdks/js/src/index.ts` | Export `reset`; `setUser(null)` calls it |
| `sdks/js/src/api/product.ts` | `anonymous_id` sent only when the anon id was actually used as a `distinct_id` |
| `packaging/rpm/SETUP.md` | Three rows in §11 Upgrading |
| `wiki/_Sidebar.md`, `wiki/Home.md` | Link the new page |
| `wiki/Browser-SDK.md` | `reset()` in the API table + a MUST-CALL-ON-LOGOUT section carrying the durable-`sauron.anon_id` retention/consent note |

---

## Task 1: Migration 000038 — `event_users.identified_at`, schema.rs and models.rs

**Files:**
- Create `backend/migrations/2026-08-01-000038_event_users_identified/up.sql`
- Create `backend/migrations/2026-08-01-000038_event_users_identified/down.sql`
- Modify `backend/crates/sauron-db/src/schema.rs` (the `event_users (id)` block, currently lines 133-143)
- Modify `backend/crates/sauron-db/src/models.rs` (`struct EventUser`, currently lines 482-494)
- Test: `backend/crates/sauron-db/tests/env_scoping.rs` (append)

**Interfaces:**
- Produces: columns `event_users.identified_at TIMESTAMPTZ NULL`, `event_users.identified_source TEXT NULL CHECK (identified_source IN ('identify','context_user','backfill'))`; indexes `identities_app_distinct_idx`, `event_users_app_identified_idx`; struct fields `EventUser.identified_at: Option<DateTime<Utc>>`, `EventUser.identified_source: Option<String>`; the sentinel-delimited backfill statement in `up.sql`.

- [ ] **Step 1: Write the failing migration test.** Append to `backend/crates/sauron-db/tests/env_scoping.rs`:

```rust
/// The 000038 backfill, run against the three row shapes it has to
/// discriminate. It reads the statement out of the migration file rather than
/// re-typing it, because a hand-copy would keep passing after the shipped SQL
/// changed — the same source-not-copy rule `http_env_scoping.rs` follows for
/// the route table.
///
/// The statement is re-run here rather than observed during `TestDb::setup()`
/// because migrations run against an empty database: at the moment 000038
/// executes for real there is nothing to back-fill, so its own run proves
/// nothing.
#[tokio::test]
async fn migration_000038_backfills_only_rows_with_traits_or_an_alias() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let with_traits = format!("backfill-traits-{}", Uuid::new_v4().simple());
    let with_alias = format!("backfill-alias-{}", Uuid::new_v4().simple());
    let bare = format!("backfill-bare-{}", Uuid::new_v4().simple());

    sauron_db::repo::upsert_event_user(
        &mut conn,
        ids.app_id,
        &with_traits,
        &json!({ "plan": "pro" }),
    )
    .await
    .expect("seed the traits-bearing row");
    sauron_db::repo::touch_event_user(&mut conn, ids.app_id, &with_alias)
        .await
        .expect("seed the alias-bearing row");
    sauron_db::repo::insert_identity(&mut conn, ids.app_id, "anon_abc", &with_alias)
        .await
        .expect("seed the identities alias");
    sauron_db::repo::touch_event_user(&mut conn, ids.app_id, &bare)
        .await
        .expect("seed the bare row");

    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../migrations/2026-08-01-000038_event_users_identified/up.sql"
    );
    let src = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("could not read {path}: {e}"));
    let begin = src
        .find("-- BACKFILL-BEGIN")
        .unwrap_or_else(|| panic!("{path} lost its -- BACKFILL-BEGIN sentinel"));
    let end = src
        .find("-- BACKFILL-END")
        .unwrap_or_else(|| panic!("{path} lost its -- BACKFILL-END sentinel"));
    let backfill = &src[begin + "-- BACKFILL-BEGIN".len()..end];
    diesel::sql_query(backfill)
        .execute(&mut conn)
        .await
        .expect("run the 000038 backfill statement");

    #[derive(QueryableByName)]
    struct FlagRow {
        #[diesel(sql_type = Text)]
        distinct_id: String,
        #[diesel(sql_type = diesel::sql_types::Nullable<Text>)]
        identified_source: Option<String>,
    }
    let rows: Vec<FlagRow> = diesel::sql_query(
        "SELECT distinct_id, identified_source FROM event_users \
         WHERE app_id = $1 AND distinct_id = ANY($2) ORDER BY distinct_id",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .bind::<diesel::sql_types::Array<Text>, _>(vec![
        with_traits.clone(),
        with_alias.clone(),
        bare.clone(),
    ])
    .load(&mut conn)
    .await
    .expect("read back the three rows");

    let flagged: Vec<&str> = rows
        .iter()
        .filter(|r| r.identified_source.is_some())
        .map(|r| r.distinct_id.as_str())
        .collect();
    let mut expected = vec![with_alias.as_str(), with_traits.as_str()];
    expected.sort();
    let mut got = flagged.clone();
    got.sort();
    assert_eq!(
        got, expected,
        "exactly the traits-bearing and alias-bearing rows are backfilled; {bare} must stay a guest"
    );
    for r in &rows {
        if r.identified_source.is_some() {
            assert_eq!(
                r.identified_source.as_deref(),
                Some("backfill"),
                "the backfill must stamp its own source so a poisoned cohort stays repairable"
            );
        }
    }

    drop(conn);
    db.cleanup().await;
}
```

- [ ] **Step 2: Run it and see it fail.** Run:
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron cargo test -p sauron-db migration_000038_backfills_only_rows_with_traits_or_an_alias`
  Expected failure: a compile error, `error[E0433]` / `could not read .../2026-08-01-000038_event_users_identified/up.sql` — the migration directory does not exist yet.

- [ ] **Step 3: Write `up.sql`.** Create `backend/migrations/2026-08-01-000038_event_users_identified/up.sql`:

```sql
-- Give `event_users` a flag that says "this distinct_id names a person, not an
-- SDK-minted anonymous token", so that combined active users can count one
-- human once across several apps.
--
-- Nothing on the server can tell the two apart today. Testing the `anon_`
-- prefix was rejected: only the browser SDK mints that shape, and any app may
-- legitimately use it as a real id. So the flag is written explicitly, by
-- `identify()` and by an ingested envelope whose `context.user.id` equals the
-- distinct_id the signal was filed under.
--
-- MUST RUN BEFORE RESTARTING sauron-api AND sauron-ingest.
-- RPM upgrades do not re-run sauron-migrate (packaging/rpm/SETUP.md §11).
-- Without this migration:
--   * GET /v1/projects/{id}/active-users returns 503 schema_migration_required;
--   * the ingest worker logs one ERROR at boot and collects NO identification
--     signal for the lifetime of the process.
-- The second one is NOT recoverable later: the backfill below can only see
-- `properties` and `identities`, so every person first active during an
-- un-migrated window is filed under `active_guest` forever and the split is
-- permanently wrong for those days.
--
-- MAINTENANCE WINDOW. Size it on PAGE LOADS, not on people. The browser SDK
-- re-mints `anon_${uuidv4()}` in memory on every page load and `process_event`
-- calls `touch_event_user` for every non-empty distinct_id, so `event_users`
-- holds roughly one row per page load per browser app — a 5-10x inflation over
-- the real audience. The partial index at the bottom takes a SHARE lock that
-- blocks every `touch_event_user` for the duration of its build.
--
-- REPAIR PATH. Both inputs to the `context_user` rule are client-supplied and
-- ingest authenticates with a public key embedded in browser bundles, so anyone
-- who can read an app's public key can set this flag on any distinct_id in that
-- app. That adds no new class of harm (the same actor can forge events and
-- inflate the counts directly) but the flag is STICKY, and flipping it
-- retroactively moves historical figures from the guest column to the
-- identified one. `identified_source` is what makes a poisoned cohort
-- repairable without touching real identify() rows:
--
--   UPDATE event_users SET identified_at = NULL, identified_source = NULL
--    WHERE app_id = $1 AND identified_source = 'context_user'
--      AND identified_at > $2;
--
-- Enum-like column as TEXT + CHECK, never a custom SQL type — house rule.
ALTER TABLE event_users ADD COLUMN identified_at TIMESTAMPTZ;
ALTER TABLE event_users ADD COLUMN identified_source TEXT
  CHECK (identified_source IN ('identify', 'context_user', 'backfill'));

-- Not optional. `identities` carries only UNIQUE (app_id, alias_id);
-- `distinct_id` is unindexed, so the EXISTS leg of the backfill below has no
-- support at all and degrades to a per-row scan of a table nobody has ever
-- read from.
CREATE INDEX identities_app_distinct_idx ON identities (app_id, distinct_id);

-- The first read of `identities` in the product's history — it has been
-- write-only dead storage since migration 1. Two legs because `identify()`
-- merges traits into `properties` and writes an `identities` row only when the
-- SDK supplied a non-empty `anonymous_id` (browser only).
--
-- This under-merges by design: an identify() with empty traits and no anonymous
-- id (the Node/Python/C#/Flutter shape) leaves no trace here, so those users
-- stay app-local until their next identify() re-stamps them through the live
-- write path. Under-merging is the fail-closed direction.
--
-- The two sentinels below are read by
-- `sauron-db/tests/env_scoping.rs::migration_000038_backfills_only_rows_with_traits_or_an_alias`,
-- which re-runs exactly this statement against seeded rows. Migrations execute
-- against an empty database, so this statement's own run back-fills nothing and
-- proves nothing. Do not remove or reword the sentinels.
-- BACKFILL-BEGIN
UPDATE event_users eu
   SET identified_at = eu.first_seen, identified_source = 'backfill'
 WHERE eu.identified_at IS NULL
   AND (eu.properties <> '{}'::jsonb
        OR EXISTS (SELECT 1 FROM identities i
                    WHERE i.app_id = eu.app_id AND i.distinct_id = eu.distinct_id));
-- BACKFILL-END

-- Partial, because every read of this flag tests `IS NOT NULL` only — the join
-- in `active_users_combined` carries `AND eu.identified_at IS NOT NULL` as a
-- join condition, so the index only ever has to cover identified rows.
CREATE INDEX event_users_app_identified_idx
  ON event_users (app_id, distinct_id) WHERE identified_at IS NOT NULL;
```

- [ ] **Step 4: Write `down.sql`.** Create `backend/migrations/2026-08-01-000038_event_users_identified/down.sql`:

```sql
-- The backfill is not recoverable from this down, but it IS re-derivable: it
-- reads only `event_users.properties` and the `identities` table, neither of
-- which this file touches. Re-running up.sql reconstructs it.
DROP INDEX IF EXISTS event_users_app_identified_idx;
DROP INDEX IF EXISTS identities_app_distinct_idx;
ALTER TABLE event_users DROP COLUMN IF EXISTS identified_source;
ALTER TABLE event_users DROP COLUMN IF EXISTS identified_at;
```

- [ ] **Step 5: Hand-edit `schema.rs`.** In `backend/crates/sauron-db/src/schema.rs`, inside `diesel::table! { event_users (id) { … } }`, append two fields **after** `updated_at -> Timestamptz,` so the block ends:

```rust
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
        // Appended, never inserted mid-list: `models::EventUser` derives
        // `Queryable`, which decodes POSITIONALLY, and `ALTER TABLE … ADD
        // COLUMN` appends physically. A field inserted in the middle here
        // would silently bind every later column to the wrong one.
        identified_at -> Nullable<Timestamptz>,
        identified_source -> Nullable<Text>,
    }
}
```

- [ ] **Step 6: Hand-edit `models.rs`.** In `backend/crates/sauron-db/src/models.rs`, append the same two fields in the same order to the end of `struct EventUser`:

```rust
pub struct EventUser {
    pub id: Uuid,
    pub app_id: Uuid,
    pub distinct_id: String,
    pub properties: Value,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Non-NULL means this distinct_id names a person. Every read tests
    /// `IS NOT NULL` only — the timestamp itself is informational.
    pub identified_at: Option<DateTime<Utc>>,
    /// Which of `identify` / `context_user` / `backfill` set the flag. The
    /// only thing that makes a poisoned `context_user` cohort repairable
    /// without also clearing real identify() rows.
    pub identified_source: Option<String>,
}
```

- [ ] **Step 7: Apply the migration and run the test.** Run:
  `cd /home/splimter/projects/freelance/sauron/backend && DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron cargo run --bin sauron-migrate`
  then
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron cargo test -p sauron-db migration_000038_backfills_only_rows_with_traits_or_an_alias`
  Expected: `test result: ok. 1 passed`.

- [ ] **Step 8: Prove the whole workspace still compiles and the existing suite is green.** Run:
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo check --workspace --all-targets`
  then
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron cargo test -p sauron-db`
  Expected: clean check, all tests pass.

---

## Task 2: `mark_event_user_identified` and the schema probe

**Files:**
- Modify `backend/crates/sauron-db/src/repo.rs` (append beside `touch_event_user`, ~line 2480)
- Test: `backend/crates/sauron-db/tests/env_scoping.rs` (append)

**Interfaces:**
- Consumes: `event_users.identified_at` / `identified_source` (Task 1).
- Produces:
  - `pub const repo::IDENTIFIED_SOURCE_IDENTIFY: &str = "identify";`
  - `pub const repo::IDENTIFIED_SOURCE_CONTEXT_USER: &str = "context_user";`
  - `pub const repo::IDENTIFIED_SOURCE_BACKFILL: &str = "backfill";`
  - `pub async fn repo::mark_event_user_identified(conn: &mut AsyncPgConnection, app_id: Uuid, distinct_id: &str, source: &str) -> QueryResult<usize>`
  - `pub async fn repo::probe_event_users_identified(conn: &mut AsyncPgConnection) -> QueryResult<()>`

- [ ] **Step 1: Write the failing test.** Append to `backend/crates/sauron-db/tests/env_scoping.rs`:

```rust
/// First-write-wins, and an unidentified touch can never clear the flag.
/// This is the property the whole guest/identified split rests on: a single
/// anonymous event arriving after an identify() must not move a person back
/// into the guest column, retroactively, for every day already reported.
#[tokio::test]
async fn identified_at_is_first_write_wins_and_never_cleared() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let did = format!("first-write-{}", Uuid::new_v4().simple());
    sauron_db::repo::touch_event_user(&mut conn, ids.app_id, &did)
        .await
        .expect("create the row");

    let n = sauron_db::repo::mark_event_user_identified(
        &mut conn,
        ids.app_id,
        &did,
        sauron_db::repo::IDENTIFIED_SOURCE_IDENTIFY,
    )
    .await
    .expect("first mark");
    assert_eq!(n, 1, "the first mark writes the flag");

    let n = sauron_db::repo::mark_event_user_identified(
        &mut conn,
        ids.app_id,
        &did,
        sauron_db::repo::IDENTIFIED_SOURCE_CONTEXT_USER,
    )
    .await
    .expect("second mark");
    assert_eq!(n, 0, "a later mark is a primary-key no-op, not an overwrite");

    sauron_db::repo::touch_event_user(&mut conn, ids.app_id, &did)
        .await
        .expect("anonymous touch after identification");

    #[derive(QueryableByName)]
    struct SourceRow {
        #[diesel(sql_type = diesel::sql_types::Nullable<Text>)]
        identified_source: Option<String>,
    }
    let row: SourceRow = diesel::sql_query(
        "SELECT identified_source FROM event_users WHERE app_id = $1 AND distinct_id = $2",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .bind::<Text, _>(did.as_str())
    .get_result(&mut conn)
    .await
    .expect("read back");
    assert_eq!(
        row.identified_source.as_deref(),
        Some("identify"),
        "the original source survives both a losing mark and a later anonymous touch"
    );

    assert!(
        sauron_db::repo::probe_event_users_identified(&mut conn)
            .await
            .is_ok(),
        "the probe must succeed against a migrated schema"
    );

    drop(conn);
    db.cleanup().await;
}
```

- [ ] **Step 2: Run it and see it fail.** Run:
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron cargo test -p sauron-db identified_at_is_first_write_wins_and_never_cleared`
  Expected failure: `error[E0425]: cannot find function 'mark_event_user_identified' in module 'sauron_db::repo'`.

- [ ] **Step 3: Implement.** In `backend/crates/sauron-db/src/repo.rs`, immediately after `touch_event_user` (which keeps its existing statement **verbatim**), add:

```rust
/// The three legal values of `event_users.identified_source`, transcribed from
/// migration `2026-08-01-000038`'s CHECK constraint. Adding a fourth means a
/// widening migration, not just a constant.
pub const IDENTIFIED_SOURCE_IDENTIFY: &str = "identify";
pub const IDENTIFIED_SOURCE_CONTEXT_USER: &str = "context_user";
pub const IDENTIFIED_SOURCE_BACKFILL: &str = "backfill";

/// Flag `(app_id, distinct_id)` as naming a real person, first-write-wins.
///
/// A separate statement rather than a column added to `touch_event_user` /
/// `upsert_event_user`, and the separation is load-bearing. RPM upgrades do not
/// re-run `sauron-migrate`, so a new binary can meet an old schema. If the
/// identification column list rode along inside `touch_event_user`, every
/// statement would fail with `undefined_column` and `process_event`'s
/// `let _ = …` would DISCARD the failure — `first_seen`/`last_seen` would
/// silently stop advancing deployment-wide with no dead letter, no metric and
/// no log. `process_identify`'s upsert is `.await?`, so the same missing column
/// would dead-letter every identify() in the window, destroying exactly the
/// `properties` and `identities` rows the 000038 backfill later depends on.
///
/// First-write-wins falls out of the `IS NULL` predicate rather than a
/// `COALESCE`, so after the first hit this is a primary-key no-op. Returning 0
/// is the normal steady state and is never an error.
pub async fn mark_event_user_identified(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    distinct_id: &str,
    source: &str,
) -> QueryResult<usize> {
    diesel::sql_query(
        "UPDATE event_users SET identified_at = now(), identified_source = $3 \
         WHERE app_id = $1 AND distinct_id = $2 AND identified_at IS NULL",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Text, _>(distinct_id)
    .bind::<Text, _>(source)
    .execute(conn)
    .await
}

/// Cheap existence probe for `event_users.identified_at`.
///
/// `LIMIT 0` so it costs a parse and nothing else. Callers run it once at boot
/// and latch the answer: `sauron-ingest` skips identification for the process
/// lifetime after logging one ERROR, and `sauron-api` turns the active-users
/// routes into a `503` that names `sauron-migrate` instead of letting a raw
/// `undefined_column` surface as a 500.
pub async fn probe_event_users_identified(conn: &mut AsyncPgConnection) -> QueryResult<()> {
    diesel::sql_query("SELECT identified_at FROM event_users LIMIT 0")
        .execute(conn)
        .await
        .map(|_| ())
}
```

- [ ] **Step 4: Run it and see it pass.** Run:
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron cargo test -p sauron-db identified_at_is_first_write_wins_and_never_cleared`
  Expected: `test result: ok. 1 passed`.

- [ ] **Step 5: Format and lint.** Run:
  `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all` then
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`
  Expected: no warnings.

---

## Task 3: Pipeline — stamp identification, stop discarding `touch_event_user` errors

**Files:**
- Modify `backend/crates/sauron-pipeline/src/process.rs` (`process_error` ~line 174 and ~line 296, `process_event` ~line 392, `process_identify` ~line 449, and its `#[cfg(test)] mod workflow_pipeline_tests`)

**Interfaces:**
- Consumes: `repo::mark_event_user_identified(conn, app_id, distinct_id, source) -> QueryResult<usize>`, `repo::probe_event_users_identified(conn) -> QueryResult<()>`, `repo::IDENTIFIED_SOURCE_IDENTIFY`, `repo::IDENTIFIED_SOURCE_CONTEXT_USER` (Task 2).
- Produces: `event_users.identified_at` written on the live ingest path; no new public API.

- [ ] **Step 1: Make the existing test harness reusable.** In `backend/crates/sauron-pipeline/src/process.rs`, inside `mod workflow_pipeline_tests`, change the visibility of the harness so a sibling test module can use it. Change `struct PipelineTestDb {` to `pub(super) struct PipelineTestDb {`, and change `async fn setup()`, `async fn conn(&self)` and `async fn cleanup(&self)` to `pub(super) async fn …`. Add this doc line above the struct:

```rust
    /// `pub(super)` so the identification tests in `identity_pipeline_tests`
    /// can reuse it. A second hand-rolled ephemeral-database harness in the
    /// same file would drift from this one's load-bearing database-name shape
    /// (see `setup`).
```

- [ ] **Step 2: Write the failing tests.** Append a new module at the end of `backend/crates/sauron-pipeline/src/process.rs`:

```rust
#[cfg(test)]
mod identity_pipeline_tests {
    use super::workflow_pipeline_tests::PipelineTestDb;
    use super::{process_error, process_event, process_identify};
    use chrono::Utc;
    use diesel::sql_types::{Nullable, Text, Uuid as SqlUuid};
    use diesel_async::RunQueryDsl;
    use sauron_core::envelope::{
        AnalyticsItem, EnvelopeContext, EnvelopeItem, ErrorItem, EventUser, IdentifyItem, IngestJob,
        Level,
    };
    use sauron_db::models::NewAppEnvironment;
    use sauron_db::repo;
    use sauron_redis::RedisStore;
    use serde_json::json;
    use uuid::Uuid;

    struct Fixture {
        app_id: Uuid,
        job: IngestJob,
    }

    async fn seed(db: &PipelineTestDb, scope_user_id: Option<&str>) -> Fixture {
        let mut conn = db.conn().await;
        let suffix = Uuid::new_v4().simple().to_string();
        let org = repo::create_org(&mut conn, "id org", &format!("id-org-{suffix}"))
            .await
            .expect("create org");
        let project = repo::create_project(
            &mut conn,
            org.id,
            "id project",
            &format!("id-project-{suffix}"),
        )
        .await
        .expect("create project");
        let app = repo::create_app(
            &mut conn,
            project.id,
            "id app",
            &format!("id-app-{suffix}"),
            "web",
        )
        .await
        .expect("create app");
        let env = repo::create_project_environment(&mut conn, project.id, "production")
            .await
            .expect("create catalogue env");
        let environment_id = repo::create_app_environments(
            &mut conn,
            &[NewAppEnvironment {
                app_id: app.id,
                environment_id: env.id,
                public_key: &format!("pk_id_{suffix}"),
                is_default: true,
            }],
        )
        .await
        .expect("enroll")
        .remove(0)
        .id;
        drop(conn);

        // One literal, never `default()` + field assignment: clippy's
        // `field_reassign_with_default` is warn-by-default and every task here
        // gates on `-D warnings`.
        let context = EnvelopeContext {
            user: scope_user_id.map(|id| EventUser {
                id: Some(id.to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };

        Fixture {
            app_id: app.id,
            job: IngestJob {
                app_id: app.id,
                project_id: project.id,
                org_id: org.id,
                environment_id,
                release: None,
                received_at: Utc::now(),
                ip: None,
                user_agent: None,
                context,
                sdk: None,
                // Never read: every call below passes its item explicitly.
                item: EnvelopeItem::Identify(IdentifyItem {
                    distinct_id: "unused".to_string(),
                    anonymous_id: None,
                    traits: json!({}),
                    timestamp: Utc::now(),
                }),
            },
        }
    }

    fn event(distinct_id: &str) -> AnalyticsItem {
        AnalyticsItem {
            name: "app.opened".to_string(),
            distinct_id: distinct_id.to_string(),
            properties: json!({}),
            timestamp: Utc::now(),
            session_id: None,
            workflow_id: None,
            workflow_name: None,
            screen: None,
            tags: json!({}),
            contexts: json!({}),
            extra: json!({}),
        }
    }

    #[derive(diesel::QueryableByName)]
    struct SourceRow {
        #[diesel(sql_type = Nullable<Text>)]
        identified_source: Option<String>,
    }

    async fn source_of(
        conn: &mut sauron_db::PgConn,
        app_id: Uuid,
        distinct_id: &str,
    ) -> Option<String> {
        let row: SourceRow = diesel::sql_query(
            "SELECT identified_source FROM event_users WHERE app_id = $1 AND distinct_id = $2",
        )
        .bind::<SqlUuid, _>(app_id)
        .bind::<Text, _>(distinct_id)
        .get_result(conn)
        .await
        .expect("event_users row must exist");
        row.identified_source
    }

    #[tokio::test]
    async fn process_identify_marks_the_user_identified() {
        let Some(db) = PipelineTestDb::setup().await else {
            eprintln!("TEST_DATABASE_URL unset — skipping");
            return;
        };
        let f = seed(&db, None).await;
        let mut conn = db.conn().await;
        process_identify(
            &mut conn,
            &f.job,
            IdentifyItem {
                distinct_id: "u-42".to_string(),
                anonymous_id: None,
                traits: json!({ "plan": "pro" }),
                timestamp: Utc::now(),
            },
        )
        .await
        .expect("process identify");
        assert_eq!(
            source_of(&mut conn, f.app_id, "u-42").await.as_deref(),
            Some("identify"),
        );
        drop(conn);
        db.cleanup().await;
    }

    #[tokio::test]
    async fn process_event_marks_identified_only_when_the_envelope_user_id_matches() {
        let Some(db) = PipelineTestDb::setup().await else {
            eprintln!("TEST_DATABASE_URL unset — skipping");
            return;
        };

        // Case 1: scope user id == the event's distinct_id -> flagged.
        let f = seed(&db, Some("u-match")).await;
        let mut conn = db.conn().await;
        process_event(&mut conn, &f.job, None, json!({}), event("u-match"))
            .await
            .expect("matching event");
        assert_eq!(
            source_of(&mut conn, f.app_id, "u-match").await.as_deref(),
            Some("context_user"),
        );

        // Case 2: scope user id present but different -> NOT flagged. Server
        // SDKs take an explicit distinctId that may differ from any scope
        // user; marking THAT one identified would be wrong.
        process_event(&mut conn, &f.job, None, json!({}), event("u-other"))
            .await
            .expect("mismatched event");
        assert_eq!(source_of(&mut conn, f.app_id, "u-other").await, None);
        drop(conn);

        // Case 3: no scope user at all -> NOT flagged.
        let g = seed(&db, None).await;
        let mut conn = db.conn().await;
        process_event(&mut conn, &g.job, None, json!({}), event("u-anon"))
            .await
            .expect("userless event");
        assert_eq!(source_of(&mut conn, g.app_id, "u-anon").await, None);

        drop(conn);
        db.cleanup().await;
    }

    #[tokio::test]
    async fn process_error_runs_the_same_identification_test_as_process_event() {
        let Some(db) = PipelineTestDb::setup().await else {
            eprintln!("TEST_DATABASE_URL unset — skipping");
            return;
        };
        let Ok(redis_url) = std::env::var("TEST_REDIS_URL") else {
            eprintln!("TEST_REDIS_URL unset — skipping");
            db.cleanup().await;
            return;
        };
        let redis = RedisStore::connect(&redis_url).await.expect("connect redis");
        // `SymbolizeCtx` derives only `Clone` and its `presence` field is
        // private, so `new` is the only constructor. `SymbolBlobCache::connect`
        // with a `None` url returns a disabled cache, which is what this test
        // wants: no error here carries a stack trace, so symbolication has
        // nothing to do and must not reach out to anything.
        let sym = crate::symbolize::SymbolizeCtx::new(
            std::sync::Arc::new(sauron_symbols::Symbolicator::new(1 << 20)),
            sauron_redis::SymbolBlobCache::connect(None, 1 << 20).await,
            100,
            1 << 20,
        );

        let f = seed(&db, Some("u-err")).await;
        let conn = db.conn().await;
        // Field-by-field: `ErrorItem` derives `Debug, Clone, Serialize,
        // Deserialize` and no `Default`, and `event_id`/`timestamp`/
        // `breadcrumbs` are not `Option` — their serde defaults do nothing for
        // a Rust literal.
        let item = ErrorItem {
            event_id: Uuid::new_v4(),
            level: Level::Error,
            timestamp: Utc::now(),
            exception: None,
            message: Some("boom".to_string()),
            breadcrumbs: vec![],
            tags: json!({}),
            contexts: json!({}),
            extra: json!({}),
            fingerprint: None,
            user: None,
            session_id: None,
            workflow_id: None,
            workflow_name: None,
            screen: None,
            raw_stacktrace: None,
            debug_meta: None,
        };
        process_error(
            &redis,
            db.pool(),
            &sym,
            conn,
            &f.job,
            None,
            json!({}),
            item,
        )
        .await
        .expect("process error");

        let mut conn = db.conn().await;
        assert_eq!(
            source_of(&mut conn, f.app_id, "u-err").await.as_deref(),
            Some("context_user"),
            "the error path runs the same equality test, so the two can never disagree"
        );
        drop(conn);
        db.cleanup().await;
    }

    /// The §2.4 contract, and the only thing that pins it: with the column
    /// gone, an ordinary event must still advance `last_seen` and must not
    /// return an error (which `worker.rs` would turn into a dead letter).
    #[tokio::test]
    async fn event_users_maintenance_survives_a_missing_identified_at_column() {
        let Some(db) = PipelineTestDb::setup().await else {
            eprintln!("TEST_DATABASE_URL unset — skipping");
            return;
        };
        let f = seed(&db, Some("u-nocol")).await;
        let mut conn = db.conn().await;
        diesel::sql_query("ALTER TABLE event_users DROP COLUMN identified_at")
            .execute(&mut conn)
            .await
            .expect("drop the column to simulate an un-migrated deployment");

        process_event(&mut conn, &f.job, None, json!({}), event("u-nocol"))
            .await
            .expect("an un-migrated schema must not fail the job");

        #[derive(diesel::QueryableByName)]
        struct Seen {
            #[diesel(sql_type = diesel::sql_types::Timestamptz)]
            last_seen: chrono::DateTime<Utc>,
        }
        let row: Seen = diesel::sql_query(
            "SELECT last_seen FROM event_users WHERE app_id = $1 AND distinct_id = $2",
        )
        .bind::<SqlUuid, _>(f.app_id)
        .bind::<Text, _>("u-nocol")
        .get_result(&mut conn)
        .await
        .expect("touch_event_user must still have created the row");
        assert!(row.last_seen <= Utc::now());

        drop(conn);
        db.cleanup().await;
    }
}
```

- [ ] **Step 3: Expose the pool on the test harness.** `process_error` needs a `&PgPool`. In `mod workflow_pipeline_tests`, add to `impl PipelineTestDb`:

```rust
        /// `process_error` re-acquires its own connection after symbolication,
        /// so it needs the pool rather than a checked-out connection.
        pub(super) fn pool(&self) -> &sauron_db::PgPool {
            &self.pool
        }
```

- [ ] **Step 4: Run the tests and see them fail.** Run:
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron TEST_REDIS_URL=redis://172.20.0.3:6379 cargo test -p sauron-pipeline identity_pipeline_tests`
  Expected failure: `assertion 'left == right' failed: left: None, right: Some("identify")` on `process_identify_marks_the_user_identified` — the stamping does not exist yet.

- [ ] **Step 5: Add the probe latch.** Near the top of `backend/crates/sauron-pipeline/src/process.rs`, after the `use` block, add:

```rust
/// Whether `event_users.identified_at` exists, probed once per process.
///
/// An RPM upgrade installs new binaries against an old schema (SETUP.md §11),
/// so this worker must degrade rather than write to a column that is not
/// there. One ERROR at first contact, then silence: a log line per ingested
/// event would drown the deployment it is trying to warn.
#[cfg(not(test))]
static IDENTIFIED_COLUMN: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

#[cfg(not(test))]
async fn identified_column_present(conn: &mut AsyncPgConnection) -> bool {
    if let Some(v) = IDENTIFIED_COLUMN.get() {
        return *v;
    }
    let present = repo::probe_event_users_identified(conn).await.is_ok();
    if !present {
        tracing::error!(
            "event_users.identified_at is missing — run sauron-migrate (see \
             packaging/rpm/SETUP.md §11). Active-user identification will not be \
             recorded for the lifetime of this process, and it cannot be \
             reconstructed afterwards."
        );
    }
    *IDENTIFIED_COLUMN.get_or_init(|| present)
}

/// Tests get an un-latched probe on purpose. Each test owns its own ephemeral
/// database and one of them deliberately drops the column, so a process-global
/// latch would leak one test's answer into another's depending on the order
/// the runner happened to pick.
#[cfg(test)]
async fn identified_column_present(conn: &mut AsyncPgConnection) -> bool {
    repo::probe_event_users_identified(conn).await.is_ok()
}
```

- [ ] **Step 6: Stamp on the identify path.** In `process_identify`, replace the body between the upsert and the `insert_identity` block so the function reads:

```rust
async fn process_identify(
    conn: &mut AsyncPgConnection,
    job: &IngestJob,
    id: IdentifyItem,
) -> anyhow::Result<()> {
    let traits = object_or_empty(id.traits);
    repo::upsert_event_user(conn, job.app_id, &id.distinct_id, &traits).await?;
    // Identified by construction: identify() IS the caller naming a person.
    if !id.distinct_id.is_empty() && identified_column_present(conn).await {
        if let Err(e) = repo::mark_event_user_identified(
            conn,
            job.app_id,
            &id.distinct_id,
            repo::IDENTIFIED_SOURCE_IDENTIFY,
        )
        .await
        {
            tracing::warn!(app_id = %job.app_id, error = %e, "marking an identified user failed");
        }
    }
    if let Some(anon) = id.anonymous_id {
        if !anon.is_empty() {
            let _ = repo::insert_identity(conn, job.app_id, &anon, &id.distinct_id).await;
        }
    }
    Ok(())
}
```

- [ ] **Step 7: Stamp on the event path and stop discarding the touch error.** In `process_event`, replace

```rust
    if !distinct_id.is_empty() {
        let _ = repo::touch_event_user(conn, job.app_id, &distinct_id).await;
    }
```

with

```rust
    if !distinct_id.is_empty() {
        // Was `let _ = …`. Swallowing this is how a deployment-wide stall of
        // `event_users.first_seen`/`last_seen` became invisible: no dead
        // letter, no metric, no log.
        if let Err(e) = repo::touch_event_user(conn, job.app_id, &distinct_id).await {
            tracing::warn!(
                app_id = %job.app_id,
                error = %e,
                "touch_event_user failed; event_users.last_seen did not advance"
            );
        }
        // An envelope-scoped `context.user.id` identifies this person only
        // when it IS the id the signal was filed under. Server SDKs take an
        // explicit distinctId that may differ from any scope user, and
        // marking that one identified would flag an id nobody claimed.
        let context_user_matches = job
            .context
            .user
            .as_ref()
            .and_then(|u| u.id.as_deref())
            .is_some_and(|id| !id.is_empty() && id == distinct_id);
        if context_user_matches && identified_column_present(conn).await {
            if let Err(e) = repo::mark_event_user_identified(
                conn,
                job.app_id,
                &distinct_id,
                repo::IDENTIFIED_SOURCE_CONTEXT_USER,
            )
            .await
            {
                tracing::warn!(app_id = %job.app_id, error = %e, "marking an identified user failed");
            }
        }
    }
```

- [ ] **Step 8: Stamp on the error path with the same test.** In `process_error`, immediately after `let distinct = distinct_id(user);` (currently line 175) insert:

```rust
    // The SAME equality test `process_event` runs, computed here while `user`
    // is still borrowable. On this path it is trivially true whenever
    // `distinct` is `Some` — `distinct` is derived from `user.id` — and that
    // is the point: an earlier draft passed `identified = true`
    // unconditionally here, which made the two paths disagree for no benefit
    // and let one error envelope carrying any `user.id` flag that id.
    let context_user_matches = user
        .and_then(|u| u.id.as_deref())
        .is_some_and(|id| !id.is_empty() && Some(id) == distinct.as_deref());
```

and replace the tail of the `if let Some(did) = distinct` block so it reads:

```rust
    if let Some(did) = distinct {
        let key = keys::issue_users(&issue_id.to_string());
        if redis.pf_add(&key, &did).await.is_ok() {
            if let Ok(count) = redis.pf_count(&key).await {
                let _ = repo::set_issue_users_seen(&mut conn, issue_id, count).await;
            }
        }
        // Was `let _ = …`; see `process_event`'s identical comment.
        if let Err(e) = repo::touch_event_user(&mut conn, job.app_id, &did).await {
            tracing::warn!(
                app_id = %job.app_id,
                error = %e,
                "touch_event_user failed; event_users.last_seen did not advance"
            );
        }
        if context_user_matches && identified_column_present(&mut conn).await {
            if let Err(e) = repo::mark_event_user_identified(
                &mut conn,
                job.app_id,
                &did,
                repo::IDENTIFIED_SOURCE_CONTEXT_USER,
            )
            .await
            {
                tracing::warn!(app_id = %job.app_id, error = %e, "marking an identified user failed");
            }
        }
    }
```

- [ ] **Step 9: Run the tests and see them pass.** Run:
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron TEST_REDIS_URL=redis://172.20.0.3:6379 cargo test -p sauron-pipeline`
  Expected: all `identity_pipeline_tests` pass and `lifecycle_events_and_a_stamped_event_produce_one_completed_workflow_row` still passes.

- [ ] **Step 10: Format and lint.** Run:
  `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all` then
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`
  Expected: no warnings.

---

## Task 4: Test harness — `note_identity(identified)` and the active-user seed helpers

**Files:**
- Modify `backend/crates/sauron-db/tests/common/mod.rs` (`note_identity` ~line 1690; its three callers — `seed_two_envs` ~line 1097, `seed_analytics_event` ~line 1505, `seed_error_event` ~line 1573; append the new helpers at the end of the file)

**Interfaces:**
- Consumes: `repo::mark_event_user_identified`, `repo::IDENTIFIED_SOURCE_IDENTIFY` (Task 2).
- Produces:
  - `pub async fn common::seed_identified_user(conn: &mut sauron_db::PgConn, app_id: Uuid, distinct_id: &str)`
  - `pub async fn common::seed_signal_event(conn: &mut sauron_db::PgConn, app_id: Uuid, env: Option<Uuid>, distinct_id: &str, occurred_at: DateTime<Utc>)`
  - `pub async fn common::seed_signal_error(conn: &mut sauron_db::PgConn, app_id: Uuid, env: Option<Uuid>, issue_id: Uuid, distinct_id: Option<&str>, occurred_at: DateTime<Utc>)`

- [ ] **Step 1: Write the failing test that pins the harness's new default.** Append to `backend/crates/sauron-db/tests/env_scoping.rs`:

```rust
/// Every identity `seed_two_envs()` produces is a GUEST unless a test asks
/// otherwise. Left as it was — `note_identity` calling `upsert_event_user`,
/// the identify() write shape — every seeded distinct_id would key as
/// `'u:'‖distinct_id`, merge across apps, drive `active_guest` to zero in
/// every test, and make the two anonymity tests below inexpressible against
/// the harness at all. The split would look correct and be untested.
#[tokio::test]
async fn the_harness_seeds_guests_not_identified_users() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let row: CountRow = diesel::sql_query(
        "SELECT count(*)::bigint AS n FROM event_users \
         WHERE app_id = $1 AND identified_at IS NOT NULL",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .get_result(&mut conn)
    .await
    .expect("count identified");
    assert_eq!(row.n, 0, "the ordinary event seed must not identify anyone");

    common::seed_identified_user(&mut conn, ids.app_id, &ids.shared_distinct_id).await;
    let row: CountRow = diesel::sql_query(
        "SELECT count(*)::bigint AS n FROM event_users \
         WHERE app_id = $1 AND identified_at IS NOT NULL",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .get_result(&mut conn)
    .await
    .expect("count identified after an explicit identify seed");
    assert_eq!(row.n, 1, "an explicit identify seed is the only way in");

    drop(conn);
    db.cleanup().await;
}
```

- [ ] **Step 2: Run it and see it fail.** Run:
  `DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron cargo test -p sauron-db the_harness_seeds_guests_not_identified_users`
  Expected failure: `error[E0425]: cannot find function 'seed_identified_user' in module 'common'`.

- [ ] **Step 3: Change `note_identity`.** In `backend/crates/sauron-db/tests/common/mod.rs`, replace the `note_identity` signature and its `upsert_event_user` call:

```rust
async fn note_identity(
    conn: &mut sauron_db::PgConn,
    app_id: Uuid,
    distinct_id: &str,
    device_key: &str,
    at: DateTime<Utc>,
    events_delta: i64,
    errors_delta: i64,
    // `//`, not `///`: rustc rejects a doc comment on a function parameter
    // outright ("documentation comments cannot be applied to function
    // parameters"). `identified` says whether this seed models an `identify()`
    // call or an ordinary signal. The ordinary path must go through a plain
    // touch: `upsert_event_user` is the identify() write shape, and using it
    // for every seeded row silently hands every test merge-across-apps
    // semantics it never asked for.
    identified: bool,
) {
    if identified {
        repo::upsert_event_user(conn, app_id, distinct_id, &json!({}))
            .await
            .expect("upsert event user");
        repo::mark_event_user_identified(
            conn,
            app_id,
            distinct_id,
            repo::IDENTIFIED_SOURCE_IDENTIFY,
        )
        .await
        .expect("mark event user identified");
    } else {
        repo::touch_event_user(conn, app_id, distinct_id)
            .await
            .expect("touch event user");
    }
    repo::bump_device(
```

(the `bump_device` call and everything after it is unchanged).

- [ ] **Step 4: Update the three callers.** In `seed_analytics_event` (line 1505), change
  `note_identity(conn, app_id, distinct_id, device_key, occurred_at, 1, 0).await;`
  to
  `note_identity(conn, app_id, distinct_id, device_key, occurred_at, 1, 0, false).await;`
  In `seed_error_event` (line 1573), change its `note_identity(conn, app_id, distinct_id, device_key, occurred_at, 0, 1).await;` to
  `note_identity(conn, app_id, distinct_id, device_key, occurred_at, 0, 1, false).await;`
  The third caller is the multi-line one inside `seed_two_envs` (line 1097), for `session_only_distinct_id`. Add `false` as its eighth argument so the block reads:

```rust
        note_identity(
            &mut conn,
            app.id,
            &session_only_distinct_id,
            &session_only_device_key,
            now,
            0,
            0,
            // `false`, not `true`. This identity is present only as a
            // `sessions` row; nothing ever called identify() for it, and
            // `the_harness_seeds_guests_not_identified_users` asserts the whole
            // seed leaves ZERO identified rows on `ids.app_id`.
            false,
        )
        .await;
```

- [ ] **Step 5: Add the three public helpers.** Append to `backend/crates/sauron-db/tests/common/mod.rs`:

```rust
/// Flag `distinct_id` on `app_id` as identified, exactly as `identify()` does
/// on the live path. The only way a harness-seeded identity becomes a `'u:'`
/// key.
pub async fn seed_identified_user(conn: &mut sauron_db::PgConn, app_id: Uuid, distinct_id: &str) {
    repo::touch_event_user(conn, app_id, distinct_id)
        .await
        .expect("create the event_users row");
    repo::mark_event_user_identified(
        conn,
        app_id,
        distinct_id,
        repo::IDENTIFIED_SOURCE_IDENTIFY,
    )
    .await
    .expect("mark identified");
}

/// One `analytics_events` row and NOTHING else — no `event_users` row, no
/// device bump. `active_users_combined` reads raw signal and joins
/// `event_users` separately, so its tests have to control the two independently
/// or they cannot express "active but never identified".
pub async fn seed_signal_event(
    conn: &mut sauron_db::PgConn,
    app_id: Uuid,
    env: Option<Uuid>,
    distinct_id: &str,
    occurred_at: DateTime<Utc>,
) {
    repo::insert_analytics_event(
        conn,
        NewAnalyticsEvent {
            id: Uuid::new_v4(),
            app_id,
            environment_id: env,
            name: "signal".to_string(),
            distinct_id: distinct_id.to_string(),
            properties: json!({}),
            context: json!({}),
            session_id: None,
            release: None,
            ip_address: None,
            occurred_at,
            device_key: None,
            screen: None,
            workflow_id: None,
            workflow_name: None,
            tags: json!({}),
            contexts: json!({}),
            extra: json!({}),
        },
    )
    .await
    .expect("insert signal analytics event");
}

/// One `error_events` row and nothing else. `distinct_id` is `Option` so a
/// test can seed the NULL case the union has to exclude.
pub async fn seed_signal_error(
    conn: &mut sauron_db::PgConn,
    app_id: Uuid,
    env: Option<Uuid>,
    issue_id: Uuid,
    distinct_id: Option<&str>,
    occurred_at: DateTime<Utc>,
) {
    repo::insert_error_event(
        conn,
        NewErrorEvent {
            id: Uuid::new_v4(),
            app_id,
            environment_id: env,
            issue_id,
            fingerprint: "harness-fingerprint".to_string(),
            level: "error".into(),
            message: "signal error".into(),
            exception_type: "HarnessError".into(),
            exception_value: "seeded".into(),
            stacktrace: json!([]),
            breadcrumbs: json!([]),
            context: json!({}),
            tags: json!({}),
            release: None,
            distinct_id: distinct_id.map(|s| s.to_string()),
            event_user: None,
            sdk: None,
            ip_address: None,
            occurred_at,
            session_id: None,
            device_key: None,
            screen: None,
            workflow_id: None,
            workflow_name: None,
            stacktrace_symbolicated: None,
            symbolication_status: "not_applicable".into(),
            debug_meta: None,
            contexts: json!({}),
            extra: json!({}),
            handled: None,
            title: None,
            culprit: None,
        },
    )
    .await
    .expect("insert signal error event");
}
```

If `NewErrorEvent`'s field list has drifted, copy it verbatim from the existing `seed_error_event` in the same file and change only `distinct_id`, `device_key`, `screen`, `title` and `culprit`.

- [ ] **Step 6: Run the new test and the whole `sauron-db` suite.** Run:
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron cargo test -p sauron-db`
  Expected: `the_harness_seeds_guests_not_identified_users` passes and **every pre-existing test still passes**. `touch_event_user` and `upsert_event_user(…, &json!({}))` both leave `properties = '{}'`, so no count or trait assertion should move; if one does, that assertion was reading the identify shape by accident and the failure is the finding.

- [ ] **Step 7: Format and lint.** Run:
  `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all` then
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`
  Expected: no warnings.

---

## Task 5: `active_users_combined` — the query

**Files:**
- Modify `backend/crates/sauron-db/src/scope.rs` (the `EnvFilter` derive, line 18)
- Modify `backend/crates/sauron-db/src/repo.rs` (append a new section at the end of the analytics area)
- Test: `backend/crates/sauron-db/tests/env_scoping.rs` (append)

**Interfaces:**
- Consumes: `EnvFilter::{sql_fragment_for, consumes_bind}`, `bind_env!`, `event_users.identified_at` (Task 1), `common::{seed_identified_user, seed_signal_event, seed_signal_error}` (Task 4).
- Produces:
  - `pub struct repo::AppEnvScope { pub app_id: Uuid, pub env: EnvFilter }` (derives `Debug, Clone, serde::Serialize`)
  - `pub struct repo::ActiveUserDay { pub day: chrono::NaiveDate, pub active_total: i64, pub active_identified: i64, pub active_guest: i64 }`
  - `pub async fn repo::active_users_combined(conn: &mut AsyncPgConnection, scopes: &[AppEnvScope], from: DateTime<Utc>, to: DateTime<Utc>) -> QueryResult<Vec<ActiveUserDay>>`
  - `EnvFilter` now derives `serde::Serialize`

- [ ] **Step 1: Write the failing tests.** Append to `backend/crates/sauron-db/tests/env_scoping.rs`:

```rust
// ===========================================================================
// active_users_combined
// ===========================================================================

use sauron_db::repo::AppEnvScope;

/// A second app in the same project, with one environment enrollment.
/// Returns `(app_id, env_id, issue_id)`.
async fn second_app(
    conn: &mut sauron_db::PgConn,
    project_id: Uuid,
    label: &str,
) -> (Uuid, Uuid, Uuid) {
    let suffix = Uuid::new_v4().simple().to_string();
    let app = sauron_db::repo::create_app(
        conn,
        project_id,
        label,
        &format!("{label}-{suffix}"),
        "web",
    )
    .await
    .expect("create second app");
    let env = sauron_db::repo::create_project_environment(conn, project_id, &format!("e-{suffix}"))
        .await
        .expect("create catalogue env");
    let enrollment = sauron_db::repo::create_app_environments(
        conn,
        &[sauron_db::models::NewAppEnvironment {
            app_id: app.id,
            environment_id: env.id,
            public_key: &format!("pk_{label}_{suffix}"),
            is_default: true,
        }],
    )
    .await
    .expect("enroll second app")
    .remove(0)
    .id;
    let issue = sauron_db::repo::upsert_issue(
        conn,
        sauron_db::models::NewIssue {
            app_id: app.id,
            fingerprint: "second-app-fingerprint",
            type_: "Error",
            title: "seeded",
            culprit: "seeded",
            level: "error",
            first_seen: far_past(),
            last_seen: far_past(),
            times_seen: 1,
        },
    )
    .await
    .expect("create second app issue");
    (app.id, enrollment, issue)
}

fn day_at(day: &str, hhmmss: &str) -> chrono::DateTime<Utc> {
    chrono::DateTime::parse_from_rfc3339(&format!("{day}T{hhmmss}Z"))
        .expect("valid RFC3339")
        .with_timezone(&Utc)
}

fn window(from_day: &str, to_day: &str) -> (chrono::DateTime<Utc>, chrono::DateTime<Utc>) {
    (day_at(from_day, "00:00:00"), day_at(to_day, "00:00:00"))
}

#[tokio::test]
async fn active_users_combined_merges_identified_users_across_apps() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let (app_b, env_b2, _issue_b) = second_app(&mut conn, ids.project_id, "merge-b").await;

    let did = format!("person-{}", Uuid::new_v4().simple());
    common::seed_identified_user(&mut conn, ids.app_id, &did).await;
    common::seed_identified_user(&mut conn, app_b, &did).await;
    common::seed_signal_event(&mut conn, ids.app_id, Some(ids.env_a), &did, day_at("2026-05-04", "09:00:00")).await;
    common::seed_signal_event(&mut conn, app_b, Some(env_b2), &did, day_at("2026-05-04", "21:00:00")).await;

    let (from, to) = window("2026-05-04", "2026-05-05");
    let rows = sauron_db::repo::active_users_combined(
        &mut conn,
        &[
            AppEnvScope { app_id: ids.app_id, env: EnvFilter::One(ids.env_a) },
            AppEnvScope { app_id: app_b, env: EnvFilter::One(env_b2) },
        ],
        from,
        to,
    )
    .await
    .expect("query");

    assert_eq!(rows.len(), 1, "one day in the window");
    assert_eq!(rows[0].active_total, 1, "one person, not two");
    assert_eq!(rows[0].active_identified, 1);
    assert_eq!(rows[0].active_guest, 0);

    drop(conn);
    db.cleanup().await;
}

/// The anti-test for the `'a:'‖app_id‖':'` prefix. Without `app_id` in the
/// guest key this silently returns 1, and the number would then change
/// depending on which OTHER apps happened to be selected.
#[tokio::test]
async fn active_users_combined_keeps_anonymous_ids_app_local() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let (app_b, env_b2, _issue_b) = second_app(&mut conn, ids.project_id, "guest-b").await;

    let did = format!("anon-{}", Uuid::new_v4().simple());
    common::seed_signal_event(&mut conn, ids.app_id, Some(ids.env_a), &did, day_at("2026-05-04", "09:00:00")).await;
    common::seed_signal_event(&mut conn, app_b, Some(env_b2), &did, day_at("2026-05-04", "09:00:00")).await;

    let (from, to) = window("2026-05-04", "2026-05-05");
    let rows = sauron_db::repo::active_users_combined(
        &mut conn,
        &[
            AppEnvScope { app_id: ids.app_id, env: EnvFilter::One(ids.env_a) },
            AppEnvScope { app_id: app_b, env: EnvFilter::One(env_b2) },
        ],
        from,
        to,
    )
    .await
    .expect("query");

    assert_eq!(rows[0].active_total, 2, "identical strings, two apps, no merge");
    assert_eq!(rows[0].active_identified, 0);
    assert_eq!(rows[0].active_guest, 2);

    drop(conn);
    db.cleanup().await;
}

/// Under-merging is intentional and has to stay pinned: identified in one app
/// only means two keys, one in each bucket.
#[tokio::test]
async fn active_users_combined_does_not_merge_an_identified_id_with_an_unidentified_copy() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let (app_b, env_b2, _issue_b) = second_app(&mut conn, ids.project_id, "half-b").await;

    let did = format!("half-{}", Uuid::new_v4().simple());
    common::seed_identified_user(&mut conn, ids.app_id, &did).await;
    common::seed_signal_event(&mut conn, ids.app_id, Some(ids.env_a), &did, day_at("2026-05-04", "09:00:00")).await;
    common::seed_signal_event(&mut conn, app_b, Some(env_b2), &did, day_at("2026-05-04", "09:00:00")).await;

    let (from, to) = window("2026-05-04", "2026-05-05");
    let rows = sauron_db::repo::active_users_combined(
        &mut conn,
        &[
            AppEnvScope { app_id: ids.app_id, env: EnvFilter::One(ids.env_a) },
            AppEnvScope { app_id: app_b, env: EnvFilter::One(env_b2) },
        ],
        from,
        to,
    )
    .await
    .expect("query");

    assert_eq!(rows[0].active_total, 2);
    assert_eq!(rows[0].active_identified, 1);
    assert_eq!(rows[0].active_guest, 1);

    drop(conn);
    db.cleanup().await;
}

/// The one invariant the page renders as three tiles side by side. If the two
/// halves were ever computed as separate subqueries this would start drifting.
#[tokio::test]
async fn active_users_combined_split_always_sums_to_the_total() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let (app_b, env_b2, issue_b) = second_app(&mut conn, ids.project_id, "sum-b").await;

    for i in 0..5 {
        let did = format!("mix-{i}-{}", Uuid::new_v4().simple());
        if i % 2 == 0 {
            common::seed_identified_user(&mut conn, ids.app_id, &did).await;
            common::seed_identified_user(&mut conn, app_b, &did).await;
        }
        common::seed_signal_event(&mut conn, ids.app_id, Some(ids.env_a), &did, day_at("2026-05-04", "01:00:00")).await;
        common::seed_signal_error(&mut conn, app_b, Some(env_b2), issue_b, Some(&did), day_at("2026-05-05", "01:00:00")).await;
    }

    let (from, to) = window("2026-05-04", "2026-05-07");
    let rows = sauron_db::repo::active_users_combined(
        &mut conn,
        &[
            AppEnvScope { app_id: ids.app_id, env: EnvFilter::One(ids.env_a) },
            AppEnvScope { app_id: app_b, env: EnvFilter::One(env_b2) },
        ],
        from,
        to,
    )
    .await
    .expect("query");

    assert_eq!(rows.len(), 3, "three whole days in [from, to)");
    for r in &rows {
        assert_eq!(
            r.active_total,
            r.active_identified + r.active_guest,
            "day {} does not add up",
            r.day
        );
    }

    drop(conn);
    db.cleanup().await;
}

/// Mixed `One`/`All` selection, so the bind-index walk is the thing under
/// test: deriving the env bind from anything but `consumes_bind()` silently
/// pairs an environment with the wrong app.
#[tokio::test]
async fn active_users_combined_respects_per_app_environment_filters() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let (app_b, env_b2, _issue_b) = second_app(&mut conn, ids.project_id, "envfilter-b").await;

    let only_in_env_b = format!("only-b-{}", Uuid::new_v4().simple());
    let in_env_a = format!("in-a-{}", Uuid::new_v4().simple());
    let in_app_b = format!("in-appb-{}", Uuid::new_v4().simple());
    common::seed_signal_event(&mut conn, ids.app_id, Some(ids.env_b), &only_in_env_b, day_at("2026-05-04", "09:00:00")).await;
    common::seed_signal_event(&mut conn, ids.app_id, Some(ids.env_a), &in_env_a, day_at("2026-05-04", "09:00:00")).await;
    common::seed_signal_event(&mut conn, app_b, Some(env_b2), &in_app_b, day_at("2026-05-04", "09:00:00")).await;

    let (from, to) = window("2026-05-04", "2026-05-05");
    let rows = sauron_db::repo::active_users_combined(
        &mut conn,
        &[
            AppEnvScope { app_id: ids.app_id, env: EnvFilter::One(ids.env_a) },
            AppEnvScope { app_id: app_b, env: EnvFilter::All },
        ],
        from,
        to,
    )
    .await
    .expect("query");

    // env_a's identity plus app B's, but NOT the env_b-only one. The harness's
    // own seeded env_a rows land far in the past, outside this window.
    assert_eq!(rows[0].active_total, 2, "the env_b-only identity must not appear");

    drop(conn);
    db.cleanup().await;
}

/// UTC calendar days, proven independent of the session `TimeZone` GUC — the
/// exact hazard `date_trunc('day', timestamptz)` has.
#[tokio::test]
async fn active_user_days_are_utc_calendar_days() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    diesel::sql_query("SET TimeZone = 'America/New_York'")
        .execute(&mut conn)
        .await
        .expect("move the session clock off UTC");

    let did = format!("midnight-{}", Uuid::new_v4().simple());
    common::seed_signal_event(&mut conn, ids.app_id, Some(ids.env_a), &did, day_at("2026-05-04", "23:30:00")).await;
    common::seed_signal_event(&mut conn, ids.app_id, Some(ids.env_a), &did, day_at("2026-05-05", "00:30:00")).await;

    let (from, to) = window("2026-05-04", "2026-05-06");
    let rows = sauron_db::repo::active_users_combined(
        &mut conn,
        &[AppEnvScope { app_id: ids.app_id, env: EnvFilter::One(ids.env_a) }],
        from,
        to,
    )
    .await
    .expect("query");

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].day.to_string(), "2026-05-04");
    assert_eq!(rows[0].active_total, 1);
    assert_eq!(rows[1].day.to_string(), "2026-05-05");
    assert_eq!(rows[1].active_total, 1);

    drop(conn);
    db.cleanup().await;
}

/// A gap day is present with three zeros, not absent. The CSV's row count is
/// checked against this grid.
#[tokio::test]
async fn active_users_combined_returns_zero_rows_for_days_with_no_signal() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let did = format!("gap-{}", Uuid::new_v4().simple());
    common::seed_signal_event(&mut conn, ids.app_id, Some(ids.env_a), &did, day_at("2026-05-04", "09:00:00")).await;
    common::seed_signal_event(&mut conn, ids.app_id, Some(ids.env_a), &did, day_at("2026-05-06", "09:00:00")).await;

    let (from, to) = window("2026-05-04", "2026-05-07");
    let rows = sauron_db::repo::active_users_combined(
        &mut conn,
        &[AppEnvScope { app_id: ids.app_id, env: EnvFilter::One(ids.env_a) }],
        from,
        to,
    )
    .await
    .expect("query");

    assert_eq!(rows.len(), 3);
    assert_eq!(rows[1].day.to_string(), "2026-05-05");
    assert_eq!(rows[1].active_total, 0);
    assert_eq!(rows[1].active_identified, 0);
    assert_eq!(rows[1].active_guest, 0);

    drop(conn);
    db.cleanup().await;
}

/// The empty string is a REAL value on this wire — server SDKs deliberately
/// let the three `$workflow_*` events through with one — so it has to be
/// excluded explicitly, not assumed away.
#[tokio::test]
async fn active_users_combined_excludes_empty_and_null_distinct_ids() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    common::seed_signal_event(&mut conn, ids.app_id, Some(ids.env_a), "", day_at("2026-05-04", "09:00:00")).await;
    common::seed_signal_error(&mut conn, ids.app_id, Some(ids.env_a), ids.issue_id, None, day_at("2026-05-04", "10:00:00")).await;

    let (from, to) = window("2026-05-04", "2026-05-05");
    let rows = sauron_db::repo::active_users_combined(
        &mut conn,
        &[AppEnvScope { app_id: ids.app_id, env: EnvFilter::One(ids.env_a) }],
        from,
        to,
    )
    .await
    .expect("query");

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].active_total, 0, "neither an empty nor a NULL distinct_id is a person");

    drop(conn);
    db.cleanup().await;
}
```

- [ ] **Step 2: Run them and see them fail.** Run:
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron cargo test -p sauron-db active_users_combined`
  Expected failure: `error[E0432]: unresolved import 'sauron_db::repo::AppEnvScope'`.

- [ ] **Step 3: Make `EnvFilter` serializable.** In `backend/crates/sauron-db/src/scope.rs`, change the derive on `EnvFilter` and add the reason:

```rust
/// `Serialize` is here for exactly one caller: the active-users Redis cache
/// key hashes a JSON document containing the RESOLVED filter. JSON because it
/// is self-delimiting — `Subset(Vec<Uuid>)` is a variable-length nesting
/// inside a variable-length list, and a naive join lets two distinct
/// selections flatten to the same bytes. A collision there is a cross-tenant
/// data leak, not a staleness bug.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum EnvFilter {
```

- [ ] **Step 4: Implement the query.** Append to `backend/crates/sauron-db/src/repo.rs`:

```rust
// ===========================================================================
// Combined active users (project-scoped, multi-app)
// ===========================================================================

/// One resolved `(app, environment filter)` pair.
///
/// Deliberately NOT `ReadScope`. `ReadScope` is singular by contract and ~36
/// read functions take it, so adding a plural variant of it would let a caller
/// hand a multi-app scope to a single-app query and get a silently wrong
/// number back.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AppEnvScope {
    pub app_id: Uuid,
    pub env: EnvFilter,
}

/// One UTC calendar day of the combined report. The three counts are exact:
/// `active_total == active_identified + active_guest` always.
#[derive(Debug, QueryableByName, serde::Serialize)]
pub struct ActiveUserDay {
    #[diesel(sql_type = diesel::sql_types::Date)]
    pub day: chrono::NaiveDate,
    #[diesel(sql_type = BigInt)]
    pub active_total: i64,
    #[diesel(sql_type = BigInt)]
    pub active_identified: i64,
    #[diesel(sql_type = BigInt)]
    pub active_guest: i64,
}

/// Distinct active identities per UTC day over `[from, to)`, combined across
/// `scopes` and split into identified / guest.
///
/// # The identity key
///
/// `'u:'‖distinct_id` when some selected app has `identified_at IS NOT NULL`
/// for that id, `'a:'‖app_id‖':'‖distinct_id` otherwise. Joining on
/// `distinct_id` alone was rejected: the count for {A,B} would then change
/// depending on whether C was also selected, and a metric that is not stable
/// under widening the selection is unexplainable.
///
/// Cross-app merging is EXACT STRING EQUALITY on `distinct_id`. If app A calls
/// someone `u-42` and app B calls them `auth0|abc`, this counts two people
/// where there is one. There is no server-side fix short of an
/// identity-resolution table; the guest column is what makes the limitation
/// legible instead of hidden.
///
/// # Why `days` exists
///
/// An earlier draft joined `event_users` directly against `signal`. Because the
/// projected key depends on `eu`, Postgres cannot push the `DISTINCT` below the
/// join — the outer side is every matching raw event row across up to 20
/// selections and up to 92 days, with no LIMIT, and the text key
/// `'u:'||distinct_id` is materialized once per event row before the dedup
/// sort. Interposing `days` collapses the join input by the average
/// events-per-user-per-day factor (typically 10-1000x) with a HashAggregate
/// over three narrow columns, and makes the `event_users` join cost
/// proportional to the ANSWER rather than to the input. `event_users` is the
/// table dominated by anonymous-id churn and it has no reaper, so this matters;
/// and the tier clamp does not save the naive shape on a deployment that never
/// enabled `sauron-tier`, which is exactly the deployment with the most rows.
///
/// # Why the split cannot fail to add up
///
/// `identified` is a property of the KEY, not of the row. A `'u:'` key exists
/// only because some selected app has `identified_at IS NOT NULL` for that
/// `distinct_id`; an `'a:'` key exists only where no selected app does. The
/// prefix therefore determines the flag, so carrying `identified` inside the
/// `DISTINCT` cannot split one key across both buckets and cannot change the
/// cardinality `active_total` counts. Two `count(*) FILTER` clauses over one
/// already-deduplicated set is the only shape with that property — computing
/// the halves as separate subqueries and adding them would reintroduce a total
/// that does not match its parts.
///
/// # Binds
///
/// `$1` from, `$2` to, then per scope in order `app_id` and — ONLY when
/// `env.consumes_bind()` — the environment bind. Deriving that index from
/// anything else is the documented easiest way to get `EnvFilter` wrong, and
/// here it silently pairs an environment with the wrong app.
pub async fn active_users_combined(
    conn: &mut AsyncPgConnection,
    scopes: &[AppEnvScope],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> QueryResult<Vec<ActiveUserDay>> {
    if scopes.is_empty() {
        return Ok(Vec::new());
    }

    let mut legs: Vec<String> = Vec::with_capacity(scopes.len() * 2);
    let mut next = 3usize;
    for s in scopes {
        let app_bind = next;
        next += 1;
        let env_bind = next;
        let env_a = s.env.sql_fragment_for("analytics_events", env_bind);
        let env_e = s.env.sql_fragment_for("error_events", env_bind);
        if s.env.consumes_bind() {
            next += 1;
        }
        legs.push(format!(
            "SELECT app_id, occurred_at, distinct_id FROM analytics_events \
             WHERE app_id = ${app_bind} AND occurred_at >= $1 AND occurred_at < $2{env_a} \
               AND distinct_id IS NOT NULL AND distinct_id <> ''"
        ));
        legs.push(format!(
            "SELECT app_id, occurred_at, distinct_id FROM error_events \
             WHERE app_id = ${app_bind} AND occurred_at >= $1 AND occurred_at < $2{env_e} \
               AND distinct_id IS NOT NULL AND distinct_id <> ''"
        ));
    }
    let signal = legs.join(" UNION ALL ");

    // `::timestamp` on both generate_series bounds is a disambiguation, not
    // decoration: `generate_series(date, date, interval)` has no exact
    // overload, and letting Postgres pick between the timestamp and timestamptz
    // forms would make the grid's boundaries depend on the session TimeZone —
    // the very dependency `AT TIME ZONE 'UTC'` exists to remove.
    let q = format!(
        "WITH signal AS ({signal}), \
         days AS ( \
           SELECT DISTINCT app_id, distinct_id, (occurred_at AT TIME ZONE 'UTC')::date AS day \
             FROM signal \
         ), \
         keyed AS ( \
           SELECT DISTINCT \
                  CASE WHEN eu.distinct_id IS NOT NULL \
                       THEN 'u:' || d.distinct_id \
                       ELSE 'a:' || d.app_id::text || ':' || d.distinct_id END AS identity_key, \
                  (eu.distinct_id IS NOT NULL) AS identified, \
                  d.day \
             FROM days d \
             LEFT JOIN event_users eu \
               ON eu.app_id = d.app_id AND eu.distinct_id = d.distinct_id \
              AND eu.identified_at IS NOT NULL \
         ), \
         per_day AS ( \
           SELECT day, \
                  count(*)::bigint                               AS active_total, \
                  count(*) FILTER (WHERE identified)::bigint     AS active_identified, \
                  count(*) FILTER (WHERE NOT identified)::bigint AS active_guest \
             FROM keyed GROUP BY day \
         ), \
         grid AS ( \
           SELECT generate_series( \
                    ($1 AT TIME ZONE 'UTC')::date::timestamp, \
                    (($2 - interval '1 microsecond') AT TIME ZONE 'UTC')::date::timestamp, \
                    interval '1 day')::date AS day \
         ) \
         SELECT g.day AS day, \
                COALESCE(p.active_total, 0)::bigint      AS active_total, \
                COALESCE(p.active_identified, 0)::bigint AS active_identified, \
                COALESCE(p.active_guest, 0)::bigint      AS active_guest \
           FROM grid g \
           LEFT JOIN per_day p ON p.day = g.day \
          ORDER BY g.day"
    );

    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<Timestamptz, _>(from)
        .bind::<Timestamptz, _>(to);
    for s in scopes {
        stmt = stmt.bind::<SqlUuid, _>(s.app_id);
        stmt = crate::bind_env!(stmt, &s.env);
    }
    stmt.load(conn).await
}
```

- [ ] **Step 5: Run the tests and see them pass.** Run:
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron cargo test -p sauron-db active_users_combined`
  then
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron cargo test -p sauron-db active_user_days_are_utc_calendar_days`
  Expected: all eight new tests pass.

- [ ] **Step 6: Format and lint.** Run:
  `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all` then
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`
  Expected: no warnings.

---

## Task 6: `env_ids_for_apps`

**Files:**
- Modify `backend/crates/sauron-db/src/repo.rs` (immediately after `env_ids_for_app`, ~line 1405)
- Test: `backend/crates/sauron-db/tests/env_scoping.rs` (append)

**Interfaces:**
- Produces: `pub async fn repo::env_ids_for_apps(conn: &mut AsyncPgConnection, app_ids: &[Uuid]) -> QueryResult<Vec<(Uuid, Uuid)>>` returning `(app_id, app_environments.id)`.

- [ ] **Step 1: Write the failing test.** Append to `backend/crates/sauron-db/tests/env_scoping.rs`:

```rust
/// The batched `env_ids_for_app`, keyed so a caller can build a per-app map.
/// A FLAT set of these ids is meaningless — `role_grants.scope_id` for
/// `scope_type='env'` holds an `app_environments.id`, which is per-app — and
/// handing the union to `resolve_env_filter` breaks both of its decisions in
/// the granting direction.
#[tokio::test]
async fn env_ids_for_apps_keys_every_enrollment_by_its_app() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let (app_b, env_b2, _issue_b) = second_app(&mut conn, ids.project_id, "envids-b").await;

    let mut rows = sauron_db::repo::env_ids_for_apps(&mut conn, &[ids.app_id, app_b])
        .await
        .expect("query");
    rows.sort();

    assert!(rows.contains(&(ids.app_id, ids.env_a)));
    assert!(rows.contains(&(ids.app_id, ids.env_b)));
    assert!(rows.contains(&(app_b, env_b2)));
    assert!(
        !rows.contains(&(ids.app_id, env_b2)),
        "app B's enrollment must never be attributed to app A"
    );

    assert!(
        sauron_db::repo::env_ids_for_apps(&mut conn, &[])
            .await
            .expect("empty input")
            .is_empty(),
        "an empty input must not produce a query with an empty ANY()"
    );

    drop(conn);
    db.cleanup().await;
}
```

- [ ] **Step 2: Run it and see it fail.** Run:
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron cargo test -p sauron-db env_ids_for_apps_keys_every_enrollment_by_its_app`
  Expected failure: `error[E0425]: cannot find function 'env_ids_for_apps' in module 'sauron_db::repo'`.

- [ ] **Step 3: Implement.** In `backend/crates/sauron-db/src/repo.rs`, immediately after `env_ids_for_app`, add:

```rust
/// `(app_id, app_environments.id)` for every enrollment of every app in
/// `app_ids` — the batched [`env_ids_for_app`], same semantics INCLUDING
/// retired enrollments (retired history stays readable, and
/// `resolve_env_filter` needs the full set for its `EnvNotInApp` check).
///
/// Callers MUST fold this into a map keyed by `app_id` and hand
/// `resolve_env_filter` only that app's slice. `resolve_env_filter` uses
/// `app_env_ids` for two decisions — the `EnvNotInApp` membership test and
/// `readable = app_env_ids ∩ reach.envs` — and the union across several apps
/// breaks both in the same direction, TOWARDS GRANTING. Concretely: a caller
/// holding an env grant only on app B's staging enrollment, asking for app A,
/// gets a non-empty `readable` for app A (it contains app B's id), so instead
/// of `NoReach` → 403, app A resolves to a `Subset` naming an environment that
/// is not its own and contributes zero rows, silently, inside a combined number
/// the caller should have been refused outright.
///
/// Deliberately unordered and unlimited, unlike `list_app_environments`: this
/// feeds an authorization decision, and a truncated or filtered input to that
/// decision is a wrong answer, not a shorter list.
pub async fn env_ids_for_apps(
    conn: &mut AsyncPgConnection,
    app_ids: &[Uuid],
) -> QueryResult<Vec<(Uuid, Uuid)>> {
    if app_ids.is_empty() {
        return Ok(Vec::new());
    }
    app_environments::table
        .filter(app_environments::app_id.eq_any(app_ids.to_vec()))
        .select((app_environments::app_id, app_environments::id))
        .load(conn)
        .await
}
```

- [ ] **Step 4: Run it and see it pass.** Run:
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron cargo test -p sauron-db env_ids_for_apps_keys_every_enrollment_by_its_app`
  Expected: `test result: ok. 1 passed`.

- [ ] **Step 5: Format and lint.** Run:
  `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all` then
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`
  Expected: no warnings.

---

## Task 7: Re-anchor `user_stats` to a supplied `now`, and surface the DAU tile

**Files:**
- Modify `backend/crates/sauron-db/src/repo.rs` (`user_stats`, lines 5198-5250)
- Modify `backend/bins/sauron-api/src/routes/analytics.rs` (line 351)
- Modify `backend/crates/sauron-db/tests/env_scoping.rs` (5 call sites: lines 1252, 1295, 1322, 1344, 5377; plus one new test)
- Modify `dashboard/src/pages/UsersExplorer.svelte` (tile row ~line 152, and the Audience heading ~line 147)

**Interfaces:**
- Produces: `pub async fn repo::user_stats(conn: &mut AsyncPgConnection, scope: ReadScope, since: DateTime<Utc>, now: DateTime<Utc>) -> QueryResult<UserStats>` — one added parameter, six call sites.

- [ ] **Step 1: Write the failing test.** Append to `backend/crates/sauron-db/tests/env_scoping.rs`:

```rust
/// Impossible to write before the re-anchoring, which is the point: the three
/// windows were three separate `now()` calls evaluated by Postgres inside one
/// statement, so they were three different instants and no test could place a
/// row relative to them without freezing the server clock.
#[tokio::test]
async fn user_stats_dau_wau_are_anchored_to_the_supplied_now() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let pinned = day_at("2026-05-10", "12:00:00");
    let did = format!("anchored-{}", Uuid::new_v4().simple());
    common::seed_signal_event(
        &mut conn,
        ids.app_id,
        Some(ids.env_a),
        &did,
        pinned - Duration::days(2),
    )
    .await;

    let s = sauron_db::repo::user_stats(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        far_past(),
        pinned,
    )
    .await
    .expect("user_stats");

    // The harness's own env_a rows sit far in the past, so only the row seeded
    // above can fall inside any of the three windows.
    assert_eq!(s.dau, 0, "two days before `now` is outside the 1-day window");
    assert_eq!(s.wau, 1, "…inside the 7-day window");
    assert_eq!(s.mau, 1, "…and inside the 30-day window");

    drop(conn);
    db.cleanup().await;
}
```

- [ ] **Step 2: Run it and see it fail.** Run:
  `DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron cargo test -p sauron-db user_stats_dau_wau_are_anchored_to_the_supplied_now`
  Expected failure: `error[E0061]: this function takes 3 arguments but 4 arguments were supplied`.

- [ ] **Step 3: Re-anchor `user_stats`.** In `backend/crates/sauron-db/src/repo.rs`, change the signature and the three interval literals. Replace the `pub async fn user_stats(` signature line and the body's opening through the `stmt` construction with:

```rust
/// …existing doc comment stays verbatim, plus:
///
/// `now` is supplied by the caller rather than read from the database clock.
/// The 1/7/30-day literals are NOT the bug and must not become parameters:
/// `dau`/`wau`/`mau` mean those spans by definition and the dashboard tiles are
/// literally labelled "7-day"/"30-day", so repointing them at `since_days`
/// would make a user on the 90-day range read "MAU" as a 90-day count. The bug
/// was that three separate `now()` calls inside one statement are three
/// different instants, that this was the last read in the analytics path
/// anchored to the DATABASE clock, and that it was untestable without freezing
/// the server clock.
///
/// Known limitation, deliberately not fixed here: `user_stats` is HOT-TIER
/// ONLY, and its 30-day `mau` window is exactly the default `TIER_HOT_DAYS`, so
/// once `sauron-tier` has run that number silently loses its oldest days.
/// `GET /v1/projects/{id}/active-users` and its `truncated` flag are the
/// principled answer; this endpoint keeps the cheap behaviour and says so.
pub async fn user_stats(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    since: DateTime<Utc>,
    now: DateTime<Utc>,
) -> QueryResult<UserStats> {
    let env_sql = scope.env.sql_fragment(3);
    // `.clone()`, not a move — the final `bind_env!` call below still needs
    // `scope.env`; see `overview_totals`'s identical call for why.
    let membership_sql = event_user_membership_exists(scope.env.clone(), 3);
    // Derived from `consumes_bind()`, never assumed: `All` and `Unattributed`
    // reserve no bind, so the three cutoffs start at $3 for them and $4 for
    // `One`/`Subset`. Hardcoding either shifts every cutoff by one and silently
    // compares a timestamp against a uuid.
    let n = if scope.env.consumes_bind() { 4 } else { 3 };
    let (b1, b7, b30) = (n, n + 1, n + 2);
```

Then in the `format!` string, replace each of the two occurrences of `now() - interval '1 day'` with `${b1}`, each of the two `now() - interval '7 days'` with `${b7}`, and each of the two `now() - interval '30 days'` with `${b30}` — six replacements in total, two per sub-select: once in the `analytics_events` leg and once in the `error_events` leg. Finally replace the bind tail:

```rust
    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Timestamptz, _>(since);
    // `bind_env!` sits BETWEEN `since` and the three cutoffs so positional
    // order matches the indices computed above.
    stmt = crate::bind_env!(stmt, &scope.env);
    stmt = stmt
        .bind::<Timestamptz, _>(now - chrono::Duration::days(1))
        .bind::<Timestamptz, _>(now - chrono::Duration::days(7))
        .bind::<Timestamptz, _>(now - chrono::Duration::days(30));
    stmt.get_result(conn).await
}
```

- [ ] **Step 4: Update the API call site.** In `backend/bins/sauron-api/src/routes/analytics.rs`, replace lines 349-351 — `let since = …`, the blank line, and the `user_stats` call, but NOT line 352's `let series = repo::active_user_series(&mut conn, scope, since).await?;`, which stays exactly as it is — so `since` and `now` come from ONE `Utc::now()` binding — two bindings could disagree by a scheduler tick and put the `since` window and the rolling windows on different clocks:

```rust
    let now = Utc::now();
    let since = now - Duration::days(q.since_days.clamp(1, 365));

    let stats = repo::user_stats(&mut conn, scope.clone(), since, now).await?;
```

- [ ] **Step 5: Update the five test call sites.** In `backend/crates/sauron-db/tests/env_scoping.rs`, add `Utc::now(),` as the fourth argument to the four `user_stats` calls in `user_stats_covers_only_the_selected_environment` (lines 1252, 1295, 1322, 1344) and to the one inside the `Subset` smoke test (line 5377). These four assertion-bearing tests currently rely on the database clock to place their seeded rows inside the 1/7/30-day windows, so passing `Utc::now()` preserves exactly today's behaviour; only the new test above passes a fixed instant. Add a comment at the first of them:

```rust
    // `Utc::now()` preserves the pre-re-anchoring behaviour these assertions
    // were written against (every seeded row lands within minutes of it).
    // `user_stats_dau_wau_are_anchored_to_the_supplied_now` is the one that
    // pins a fixed instant.
```

- [ ] **Step 6: Run the backend tests.** Run:
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron cargo test -p sauron-db`
  then
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo check --workspace --all-targets`
  Expected: the new test passes, `user_stats_covers_only_the_selected_environment` still passes with its original numbers, and the workspace compiles.

- [ ] **Step 7: Render the DAU tile.** In `dashboard/src/pages/UsersExplorer.svelte`, insert a DAU tile immediately before the WAU tile:

```svelte
      <!-- `stats.dau` has always been in the payload and in the `UserStats`
           model; the tile was simply never rendered, which is why this page
           shows a stickiness ratio whose numerator is invisible. -->
      <StatTile label="DAU" value={compactNumber(analytics.stats.dau)} sub="24h" />
      <StatTile label="WAU" value={compactNumber(analytics.stats.wau)} sub="7-day" />
```

- [ ] **Step 8: Link the combined view.** In the same file, replace the `analytics-head` block with:

```svelte
  <div class="analytics-head">
    <div>
      <h2 class="section-title">Audience</h2>
      <p class="muted sub">
        This app only. <a href="#/active-users">Combined active users</a> counts people
        across several apps at once.
      </p>
    </div>
    <DateRange value={sinceDays} onchange={(d) => (sinceDays = d)} />
  </div>
```

- [ ] **Step 9: Typecheck the dashboard.** Run:
  `cd /home/splimter/projects/freelance/sauron/dashboard && npm run check`
  Expected: `0 errors`.

- [ ] **Step 10: Format and lint the backend.** Run:
  `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all` then
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`
  Expected: no warnings.

---

## Task 8: `csv.rs` — the export primitive

**Files:**
- Create `backend/bins/sauron-api/src/csv.rs`
- Modify `backend/bins/sauron-api/src/main.rs` (module list, line 7)

**Interfaces:**
- Produces: `pub fn crate::csv::escape_field(s: &str) -> String`, `pub fn crate::csv::write_row(out: &mut String, fields: &[&str])`.

- [ ] **Step 1: Create the module with tests only, and register it.** Create `backend/bins/sauron-api/src/csv.rs` containing ONLY the module doc and the test module below, then add `mod csv;` to `backend/bins/sauron-api/src/main.rs` immediately after `mod admin_storage;`:

```rust
//! RFC 4180 CSV writing, plus the spreadsheet formula-injection guard.
//!
//! The `csv` crate was rejected and the reason is recorded here so it is not
//! re-litigated: `backend/Cargo.toml` has no `csv` dependency, adding one puts
//! a crate in every RPM build, and — the decisive point — the `csv` crate does
//! not do formula-injection escaping, so the one non-trivial rule below is
//! hand-rolled either way. The repo's precedent is to hand-roll small,
//! fully-testable primitives (`sauron_alerts::render::substitute` instead of a
//! template engine, hand-rolled `hmac_sha256_hex`, hand-rolled config parsing).
//!
//! This module exists even though v1's four columns (an ISO date and three
//! integers) trigger none of the guard, because a hand-rolled
//! join-with-commas at the one call site is exactly what would get copied into
//! the next export — the one that carries app, environment and person names.
//!
//! **No UTF-8 BOM.** v1 emits pure ASCII so the question is moot, and a BOM
//! breaks naive line-oriented tooling in a way that is harder to diagnose than
//! an Excel encoding prompt. Revisit on the first export that carries
//! non-ASCII text.

#[cfg(test)]
mod tests {
    use super::{escape_field, write_row};

    #[test]
    fn a_plain_field_is_not_quoted() {
        assert_eq!(escape_field("2026-05-04"), "2026-05-04");
        assert_eq!(escape_field("42"), "42");
    }

    #[test]
    fn an_empty_field_emits_nothing() {
        assert_eq!(escape_field(""), "");
        let mut out = String::new();
        write_row(&mut out, &["a", "", "b"]);
        assert_eq!(out, "a,,b\r\n");
    }

    #[test]
    fn a_comma_forces_quoting() {
        assert_eq!(escape_field("a,b"), "\"a,b\"");
    }

    #[test]
    fn a_quote_is_doubled_inside_quotes() {
        assert_eq!(escape_field("say \"hi\""), "\"say \"\"hi\"\"\"");
    }

    #[test]
    fn embedded_newlines_force_quoting() {
        assert_eq!(escape_field("a\r\nb"), "\"a\r\nb\"");
        assert_eq!(escape_field("a\nb"), "\"a\nb\"");
    }

    #[test]
    fn leading_or_trailing_space_forces_quoting() {
        assert_eq!(escape_field(" a"), "\" a\"");
        assert_eq!(escape_field("a "), "\"a \"");
    }

    /// A cell a spreadsheet would EVALUATE rather than display. The `'` goes
    /// on before quoting, because the spreadsheet strips the surrounding
    /// quotes before deciding whether the cell is a formula — a `'` added
    /// outside them would do nothing.
    #[test]
    fn a_formula_leading_byte_gets_a_text_prefix() {
        assert_eq!(escape_field("=1+1"), "'=1+1");
        assert_eq!(escape_field("+1"), "'+1");
        assert_eq!(escape_field("-1"), "'-1");
        assert_eq!(escape_field("@SUM"), "'@SUM");
        assert_eq!(escape_field("\tx"), "\"'\tx\"");
        assert_eq!(escape_field("\rx"), "\"'\rx\"");
    }

    #[test]
    fn the_row_terminator_is_crlf() {
        let mut out = String::new();
        write_row(&mut out, &["day", "active_total"]);
        write_row(&mut out, &["2026-05-04", "7"]);
        assert_eq!(out, "day,active_total\r\n2026-05-04,7\r\n");
    }
}
```

- [ ] **Step 2: Run the tests and see them fail.** Run:
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-api --bin sauron-api csv::`
  Expected failure: `error[E0432]: unresolved imports 'super::escape_field', 'super::write_row'`.

- [ ] **Step 3: Implement.** Insert into `backend/bins/sauron-api/src/csv.rs`, between the module doc and the `#[cfg(test)]` module:

```rust
/// Escape one field per RFC 4180, with a formula-injection guard in front.
///
/// Order is load-bearing: the `'` prefix goes on BEFORE quoting. A spreadsheet
/// strips the surrounding quotes before deciding whether a cell is a formula,
/// so a prefix added outside them protects nothing.
pub fn escape_field(s: &str) -> String {
    let guarded = match s.as_bytes().first() {
        Some(b'=' | b'+' | b'-' | b'@' | b'\t' | b'\r') => format!("'{s}"),
        _ => s.to_string(),
    };
    let needs_quotes = guarded.contains(',')
        || guarded.contains('"')
        || guarded.contains('\r')
        || guarded.contains('\n')
        || guarded.starts_with(' ')
        || guarded.ends_with(' ');
    if needs_quotes {
        format!("\"{}\"", guarded.replace('"', "\"\""))
    } else {
        guarded
    }
}

/// Append one CRLF-terminated row. `\r\n` rather than `\n` because RFC 4180
/// says so and because the consumer is a spreadsheet, not a unix pipeline.
pub fn write_row(out: &mut String, fields: &[&str]) {
    for (i, f) in fields.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&escape_field(f));
    }
    out.push_str("\r\n");
}
```

- [ ] **Step 4: Run them and see them pass.** Run:
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-api --bin sauron-api csv::`
  Expected: `test result: ok. 8 passed`.

- [ ] **Step 5: Format and lint.** Run:
  `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all` then
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`
  Expected: no warnings. (`escape_field`/`write_row` are `pub` in a binary crate, so an unused-warning is possible until Task 10 wires them; if clippy reports `dead_code`, add `#![allow(dead_code)]`-free wiring by completing Task 10 before re-running clippy, and note it here rather than suppressing the lint.)

---

## Task 9: `routes/active_users.rs` — the pure layer

**Files:**
- Create `backend/bins/sauron-api/src/routes/active_users.rs`
- Modify `backend/bins/sauron-api/src/routes/mod.rs` (module list, before `pub mod admin;`)

**Interfaces:**
- Consumes: `sauron_db::repo::AppEnvScope` (Task 5), `sauron_db::scope::EnvFilter`, `sauron_auth::hash_token`.
- Produces (all `pub(crate)` unless noted, all consumed by Task 10):
  - `const MAX_ACTIVE_USER_DAYS: i64`, `MAX_SELECTED_APPS: usize`, `MAX_SCAN_BUDGET: i64`, `ACTIVE_USERS_CACHE_TTL_SECS: u64`
  - `const RESOLVED_ALL/RESOLVED_ONE/RESOLVED_SUBSET/RESOLVED_UNATTRIBUTED: &str`
  - `pub struct ActiveUsersQuery { from: DateTime<Utc>, to: DateTime<Utc>, selection: Vec<String> }`
  - `pub struct ReportWindow { from, to }`, `pub struct ActiveUserPoint { day, active_total, active_identified, active_guest }`, `pub struct SelectionView { app_id, app_name, resolved: String, environment_ids: Vec<Uuid>, environment_labels: Vec<String> }`, `pub struct ActiveUsersReport { requested, effective, truncated, truncation_reason, selections, series, latest }`
  - `fn parse_selection(raw: &[String]) -> Result<Vec<(Uuid, EnvFilter)>, ApiError>`
  - `fn floor_to_utc_day(t: DateTime<Utc>) -> DateTime<Utc>`
  - `fn align_clamp_up(floor: DateTime<Utc>) -> DateTime<Utc>` — the tier clamp's day alignment, consumed by Task 10's `build_report`
  - `fn validate_window(from, to, selections: usize) -> Result<(DateTime<Utc>, DateTime<Utc>), ApiError>`
  - `fn latest_full_day<'a>(series: &'a [ActiveUserPoint], today_utc: NaiveDate) -> Option<&'a ActiveUserPoint>`
  - `fn cache_key(project_id: Uuid, from, to, scopes: &[AppEnvScope]) -> Result<String, ApiError>`
  - `fn resolved_label(env: &EnvFilter) -> &'static str`

- [ ] **Step 1: Create the module with its types and its tests, no handlers yet.** Create `backend/bins/sauron-api/src/routes/active_users.rs`:

```rust
//! Combined active users across the apps of one project, and the CSV export.
//!
//! A module of its own rather than more `analytics.rs`, because this is the
//! first project-scoped telemetry read in the product and its authorization
//! shape has nothing in common with `analytics.rs`'s single-app
//! `authorized_read_scope` handlers: the environment dimension is expressed
//! PER SELECTION, so a global `?environment_id=` is rejected outright.

use std::collections::HashSet;

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use sauron_db::repo::AppEnvScope;
use sauron_db::scope::EnvFilter;

use crate::error::ApiError;

/// Longest window a single request may cover.
const MAX_ACTIVE_USER_DAYS: i64 = 92;
/// Most apps one request may combine.
const MAX_SELECTED_APPS: usize = 20;
/// Cap on selections × displayed days — the thing actually being handed out.
/// 20 apps × 92 days is 1840 partition-day scans, and bounding the two
/// dimensions independently does not bound their product.
const MAX_SCAN_BUDGET: i64 = 1200;
/// How long an assembled report stays warm. A latency optimization, NOT a DoS
/// control: the rate limiter, the scan budget and the semaphore are the
/// control.
const ACTIVE_USERS_CACHE_TTL_SECS: u64 = 60;

/// The four values `SelectionView::resolved` can take. Named constants so the
/// handler, the tests and the dashboard cannot drift on a string literal.
const RESOLVED_ALL: &str = "all";
const RESOLVED_ONE: &str = "one";
const RESOLVED_SUBSET: &str = "subset";
const RESOLVED_UNATTRIBUTED: &str = "unattributed";

/// Deserialized with `axum_extra::extract::Query` (serde_html_form) because
/// `selection` is a repeated key. `environment_id` is deliberately NOT a field:
/// the dimension is per selection, and accepting a global one and ignoring it
/// is exactly the bug `routes::scope`'s module docs exist to prevent.
#[derive(Debug, Deserialize)]
pub struct ActiveUsersQuery {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    #[serde(default)]
    pub selection: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportWindow {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveUserPoint {
    pub day: NaiveDate,
    pub active_total: i64,
    pub active_identified: i64,
    pub active_guest: i64,
}

/// What the server actually queried for one selection.
///
/// `resolved` carries the RESOLVED filter, not the requested one, and it is a
/// tagged shape rather than `environment_id: Option<Uuid>`. That is not
/// cosmetic. `rbac.rs`'s `resolve_env_filter` turns a bare app request from a
/// partial-reach caller into `Subset(readable)`, so a member holding env grants
/// on 2 of an app's 5 environments who sends the default bare
/// `?selection=<app_uuid>` gets a number computed over 2 environments. With
/// `Option<Uuid>` that renders as `None` — indistinguishable from a true `All`
/// — under a picker that still reads "All environments". It matters more here
/// than elsewhere because `All` includes `environment_id IS NULL` rows while
/// `Subset` uses `= ANY(...)`, which never matches NULL, so two callers can
/// legitimately get different totals for what looks like the same selection.
///
/// `resolved` is a `String`, not the `&'static str` it wants to be, because
/// this report round-trips through the Redis cache and must therefore derive
/// `Deserialize`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectionView {
    pub app_id: Uuid,
    pub app_name: String,
    pub resolved: String,
    /// Populated for `one` and `subset`. Empty otherwise.
    #[serde(default)]
    pub environment_ids: Vec<Uuid>,
    #[serde(default)]
    pub environment_labels: Vec<String>,
}

/// Derives `Deserialize` as well as `Serialize`, and every field added after
/// v1 must carry `#[serde(default)]`: a report cached by an older build has to
/// keep deserializing rather than missing the cache for a whole TTL.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveUsersReport {
    pub requested: ReportWindow,
    pub effective: ReportWindow,
    pub truncated: bool,
    /// A full human sentence naming the effective floor date — the UI renders
    /// it verbatim.
    pub truncation_reason: Option<String>,
    pub selections: Vec<SelectionView>,
    pub series: Vec<ActiveUserPoint>,
    pub latest: Option<ActiveUserPoint>,
}

/// Repeated `?selection=<app_uuid>[:<env_token>]`, where `<env_token>` is an
/// `app_environments.id`, the literal `all`, or the literal `none`. A bare
/// `<app_uuid>` means `all`. UUIDs contain hyphens but never colons, so `:` is
/// unambiguous and the whole thing round-trips through
/// `URLSearchParams.getAll()` with no custom codec.
///
/// Parallel `app_ids=`/`env_ids=` arrays were rejected: a length mismatch or a
/// reordering silently pairs the wrong environment with the wrong app, with no
/// error. `Subset` is never requestable — the same rule `parse_env` already
/// enforces.
fn parse_selection(raw: &[String]) -> Result<Vec<(Uuid, EnvFilter)>, ApiError> {
    if raw.is_empty() {
        return Err(ApiError::BadRequest(
            "at least one `selection` is required".into(),
        ));
    }
    if raw.len() > MAX_SELECTED_APPS {
        return Err(ApiError::BadRequest(format!(
            "at most {MAX_SELECTED_APPS} selections are allowed, got {}",
            raw.len()
        )));
    }
    let mut out: Vec<(Uuid, EnvFilter)> = Vec::with_capacity(raw.len());
    let mut seen: HashSet<Uuid> = HashSet::new();
    for token in raw {
        let (app_part, env_part) = match token.split_once(':') {
            Some((a, e)) => (a, e),
            None => (token.as_str(), RESOLVED_ALL),
        };
        let app_id = Uuid::parse_str(app_part).map_err(|_| {
            ApiError::BadRequest(format!(
                "invalid selection {token:?}: {app_part:?} is not a UUID"
            ))
        })?;
        let env = match env_part {
            "all" => EnvFilter::All,
            "none" => EnvFilter::Unattributed,
            other => EnvFilter::One(Uuid::parse_str(other).map_err(|_| {
                ApiError::BadRequest(format!(
                    "invalid selection {token:?}: {other:?} is neither \"all\", \"none\", nor a UUID"
                ))
            })?),
        };
        if !seen.insert(app_id) {
            return Err(ApiError::BadRequest(format!(
                "app {app_id} appears more than once in `selection`"
            )));
        }
        out.push((app_id, env));
    }
    Ok(out)
}

/// Truncate to 00:00 UTC.
fn floor_to_utc_day(t: DateTime<Utc>) -> DateTime<Utc> {
    Utc.from_utc_datetime(
        &t.date_naive()
            .and_hms_opt(0, 0, 0)
            .expect("midnight is a valid time on every date"),
    )
}

/// Round a hot-tier watermark UP to a whole UTC day.
///
/// A function rather than four inline lines in the handler because this is the
/// computation the design's `effective.from == series[0].day` correspondence
/// rests on, and it is the only part of the tier clamp that is testable without
/// a database. Rounding DOWN would put `effective.from` back inside the
/// tiered-out range; leaving it mid-day would render a partial first day as a
/// full day's count, since `active_users_combined` builds its grid from
/// `(effective_from AT TIME ZONE 'UTC')::date` — the same defect flooring the
/// request's `from` fixes on the way in.
fn align_clamp_up(floor: DateTime<Utc>) -> DateTime<Utc> {
    let down = floor_to_utc_day(floor);
    if down < floor {
        down + chrono::Duration::days(1)
    } else {
        down
    }
}

/// Floor both ends to UTC day boundaries, then validate.
///
/// Flooring loses nothing — the output is day-bucketed — and it fixes a real
/// correctness bug the raw contract has, where a mid-day `from` renders a
/// partial first day as a full day's count. It is also what makes the cache key
/// mean something: full-precision RFC3339 against day-granular output means
/// `from + 1µs` mints a brand-new key for a byte-identical series, i.e.
/// unlimited free cache misses.
fn validate_window(
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    selections: usize,
) -> Result<(DateTime<Utc>, DateTime<Utc>), ApiError> {
    let from = floor_to_utc_day(from);
    let to = floor_to_utc_day(to);
    if to <= from {
        return Err(ApiError::BadRequest(
            "`to` must be at least one UTC day after `from`".into(),
        ));
    }
    let days = (to - from).num_days();
    if days > MAX_ACTIVE_USER_DAYS {
        return Err(ApiError::BadRequest(format!(
            "time range must not exceed {MAX_ACTIVE_USER_DAYS} days"
        )));
    }
    let budget = selections as i64 * days;
    if budget > MAX_SCAN_BUDGET {
        return Err(ApiError::BadRequest(format!(
            "selections × days must not exceed {MAX_SCAN_BUDGET} (got {selections} × {days} = {budget})"
        )));
    }
    Ok((from, to))
}

/// The last point strictly before `today_utc`.
///
/// Today is still accumulating, and a headline tile that falls as the day
/// starts and climbs until midnight reads as a product problem. `None` — a
/// window containing only today — must render as an em-dash, never as `0`:
/// zero active users is a real and reportable answer, and rendering "we have no
/// complete day yet" as that answer is exactly the plausible-but-wrong number
/// this feature exists to stop producing.
fn latest_full_day<'a>(
    series: &'a [ActiveUserPoint],
    today_utc: NaiveDate,
) -> Option<&'a ActiveUserPoint> {
    series.iter().rev().find(|p| p.day < today_utc)
}

fn resolved_label(env: &EnvFilter) -> &'static str {
    match env {
        EnvFilter::All => RESOLVED_ALL,
        EnvFilter::One(_) => RESOLVED_ONE,
        EnvFilter::Subset(_) => RESOLVED_SUBSET,
        EnvFilter::Unattributed => RESOLVED_UNATTRIBUTED,
    }
}

/// The canonical, injective document the cache key hashes.
#[derive(Serialize)]
struct CacheFingerprint<'a> {
    project_id: Uuid,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    scopes: &'a [AppEnvScope],
}

/// Sort scopes by `app_id` and each `Subset`'s uuids, so two requests that mean
/// the same thing hash the same.
fn canonical_scopes(scopes: &[AppEnvScope]) -> Vec<AppEnvScope> {
    let mut out: Vec<AppEnvScope> = scopes
        .iter()
        .map(|s| AppEnvScope {
            app_id: s.app_id,
            env: match &s.env {
                EnvFilter::Subset(ids) => {
                    let mut ids = ids.clone();
                    ids.sort();
                    EnvFilter::Subset(ids)
                }
                other => other.clone(),
            },
        })
        .collect();
    out.sort_by_key(|s| s.app_id);
    out
}

/// The Redis key for one resolved report.
///
/// The fingerprint must be INJECTIVE BY CONSTRUCTION. `admin_storage`'s
/// `hash_token(sorted_org_uuids.join(","))` is injective only because every
/// element is a fixed-length UUID with no nesting; this one is a list of
/// `(app_id, EnvFilter)` pairs where `Subset(Vec<Uuid>)` is variable-length —
/// two levels of repetition. A naive join lets two distinct resolved selections
/// flatten to the same bytes, and the cached entry holds the whole series plus
/// every `selections[].app_name`, so a collision is a cross-tenant DATA LEAK,
/// not a staleness bug. JSON is self-delimiting, so no flattening ambiguity
/// exists.
///
/// The key uses the RESOLVED filter, never the requested token. That is what
/// keeps a caller with app-wide reach (`All`) and a caller with only env-X
/// reach (`Subset([X])`) from ever sharing an entry. Treat any deviation from
/// that in review as a Critical.
fn cache_key(
    project_id: Uuid,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    scopes: &[AppEnvScope],
) -> Result<String, ApiError> {
    let canon = canonical_scopes(scopes);
    let fingerprint = CacheFingerprint {
        project_id,
        from,
        to,
        scopes: &canon,
    };
    let json = serde_json::to_string(&fingerprint).map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(format!(
        "sauron:activeusers:{}",
        sauron_auth::hash_token(&json)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uuid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    #[test]
    fn a_bare_app_id_means_all_environments() {
        let a = uuid(1);
        let parsed = parse_selection(&[a.to_string()]).expect("bare uuid");
        assert_eq!(parsed, vec![(a, EnvFilter::All)]);
    }

    #[test]
    fn the_three_env_tokens_map_to_the_three_requestable_filters() {
        let a = uuid(1);
        let b = uuid(2);
        let c = uuid(3);
        let e = uuid(9);
        let parsed = parse_selection(&[
            format!("{a}:all"),
            format!("{b}:none"),
            format!("{c}:{e}"),
        ])
        .expect("three tokens");
        assert_eq!(
            parsed,
            vec![
                (a, EnvFilter::All),
                (b, EnvFilter::Unattributed),
                (c, EnvFilter::One(e)),
            ]
        );
    }

    #[test]
    fn a_malformed_app_uuid_is_a_400_naming_the_token() {
        let err = parse_selection(&["not-a-uuid:all".to_string()]).expect_err("must reject");
        assert!(format!("{err:?}").contains("not-a-uuid"), "{err:?}");
    }

    #[test]
    fn an_unknown_env_token_is_a_400_naming_the_token() {
        let a = uuid(1);
        let err = parse_selection(&[format!("{a}:production")]).expect_err("must reject");
        assert!(format!("{err:?}").contains("production"), "{err:?}");
    }

    #[test]
    fn a_duplicate_app_id_is_a_400() {
        let a = uuid(1);
        let err = parse_selection(&[a.to_string(), format!("{a}:none")]).expect_err("must reject");
        assert!(format!("{err:?}").contains("more than once"), "{err:?}");
    }

    #[test]
    fn an_empty_selection_is_a_400() {
        assert!(parse_selection(&[]).is_err());
    }

    #[test]
    fn more_than_max_selected_apps_is_a_400() {
        let raw: Vec<String> = (0..=MAX_SELECTED_APPS)
            .map(|i| uuid(i as u128 + 1).to_string())
            .collect();
        let err = parse_selection(&raw).expect_err("must reject");
        assert!(format!("{err:?}").contains("at most 20"), "{err:?}");
    }

    #[test]
    fn the_window_is_floored_to_utc_days() {
        let from = DateTime::parse_from_rfc3339("2026-05-04T13:37:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let to = DateTime::parse_from_rfc3339("2026-05-06T01:02:03Z")
            .unwrap()
            .with_timezone(&Utc);
        let (f, t) = validate_window(from, to, 1).expect("valid");
        assert_eq!(f.to_rfc3339(), "2026-05-04T00:00:00+00:00");
        assert_eq!(t.to_rfc3339(), "2026-05-06T00:00:00+00:00");
    }

    #[test]
    fn a_window_shorter_than_one_day_is_a_400() {
        let from = DateTime::parse_from_rfc3339("2026-05-04T01:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let to = DateTime::parse_from_rfc3339("2026-05-04T23:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert!(validate_window(from, to, 1).is_err(), "both floor to the same day");
        assert!(validate_window(to, from, 1).is_err(), "`to` before `from`");
    }

    #[test]
    fn a_window_longer_than_the_cap_is_a_400() {
        let from = DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let to = from + chrono::Duration::days(MAX_ACTIVE_USER_DAYS + 1);
        let err = validate_window(from, to, 1).expect_err("must reject");
        assert!(format!("{err:?}").contains("92 days"), "{err:?}");
    }

    #[test]
    fn the_scan_budget_bounds_the_product_not_each_dimension() {
        let from = DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let to = from + chrono::Duration::days(92);
        // 20 apps and 92 days are each individually legal; 1840 is not.
        let err = validate_window(from, to, 20).expect_err("must reject");
        assert!(format!("{err:?}").contains("1200"), "{err:?}");
        assert!(validate_window(from, to, 13).is_ok(), "13 × 92 = 1196");
    }

    fn point(day: &str, total: i64) -> ActiveUserPoint {
        ActiveUserPoint {
            day: day.parse().expect("valid date"),
            active_total: total,
            active_identified: 0,
            active_guest: total,
        }
    }

    #[test]
    fn latest_full_day_skips_today() {
        let series = vec![point("2026-05-04", 3), point("2026-05-05", 9)];
        let today: NaiveDate = "2026-05-05".parse().unwrap();
        assert_eq!(
            latest_full_day(&series, today).map(|p| p.day.to_string()),
            Some("2026-05-04".to_string())
        );
    }

    #[test]
    fn latest_full_day_is_none_when_the_window_contains_only_today() {
        let series = vec![point("2026-05-05", 9)];
        let today: NaiveDate = "2026-05-05".parse().unwrap();
        assert!(
            latest_full_day(&series, today).is_none(),
            "the tiles must render an em-dash, never 0"
        );
    }

    /// The `effective.from == series[0].day` correspondence, pinned on the one
    /// computation that can break it. A clamp landing mid-day inside the display
    /// window must move `effective.from` UP to the next midnight, because the
    /// day grid starts at `(effective_from AT TIME ZONE 'UTC')::date` and a
    /// partial day rendered as a full one is a plausible-but-wrong number.
    #[test]
    fn a_mid_day_clamp_rounds_up_to_the_next_whole_utc_day() {
        let mid_day = DateTime::parse_from_rfc3339("2026-05-04T13:37:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let aligned = align_clamp_up(mid_day);
        assert_eq!(aligned.to_rfc3339(), "2026-05-05T00:00:00+00:00");
        assert_eq!(
            aligned.date_naive(),
            point("2026-05-05", 0).day,
            "this is the day the series grid starts on, i.e. `series[0].day`"
        );

        // A watermark already on a boundary must not cost a whole day of data.
        let midnight = DateTime::parse_from_rfc3339("2026-05-04T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            align_clamp_up(midnight).to_rfc3339(),
            "2026-05-04T00:00:00+00:00"
        );
    }

    #[test]
    fn the_cache_fingerprint_is_injective_across_subset_nesting() {
        let p = uuid(100);
        let a = uuid(1);
        let b = uuid(2);
        let x = uuid(10);
        let y = uuid(11);
        let from = DateTime::parse_from_rfc3339("2026-05-04T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let to = from + chrono::Duration::days(7);

        let one = cache_key(
            p,
            from,
            to,
            &[AppEnvScope {
                app_id: a,
                env: EnvFilter::Subset(vec![x, y]),
            }],
        )
        .unwrap();
        let two = cache_key(
            p,
            from,
            to,
            &[
                AppEnvScope {
                    app_id: a,
                    env: EnvFilter::Subset(vec![x]),
                },
                AppEnvScope {
                    app_id: b,
                    env: EnvFilter::Subset(vec![y]),
                },
            ],
        )
        .unwrap();
        assert_ne!(one, two, "a flattening join would collide these");
    }

    /// `All` includes `environment_id IS NULL` rows; a `Subset` over every one
    /// of the app's environments does not. They are different questions and
    /// must never share a cache entry.
    #[test]
    fn all_and_a_full_subset_are_distinct_cache_keys() {
        let p = uuid(100);
        let a = uuid(1);
        let x = uuid(10);
        let y = uuid(11);
        let from = DateTime::parse_from_rfc3339("2026-05-04T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let to = from + chrono::Duration::days(7);
        let all = cache_key(p, from, to, &[AppEnvScope { app_id: a, env: EnvFilter::All }]).unwrap();
        let subset = cache_key(
            p,
            from,
            to,
            &[AppEnvScope {
                app_id: a,
                env: EnvFilter::Subset(vec![x, y]),
            }],
        )
        .unwrap();
        assert_ne!(all, subset);
    }

    #[test]
    fn the_cache_key_is_order_independent() {
        let p = uuid(100);
        let a = uuid(1);
        let b = uuid(2);
        let from = DateTime::parse_from_rfc3339("2026-05-04T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let to = from + chrono::Duration::days(7);
        let ab = cache_key(
            p,
            from,
            to,
            &[
                AppEnvScope { app_id: a, env: EnvFilter::All },
                AppEnvScope { app_id: b, env: EnvFilter::All },
            ],
        )
        .unwrap();
        let ba = cache_key(
            p,
            from,
            to,
            &[
                AppEnvScope { app_id: b, env: EnvFilter::All },
                AppEnvScope { app_id: a, env: EnvFilter::All },
            ],
        )
        .unwrap();
        assert_eq!(ab, ba);
    }
}
```

- [ ] **Step 2: Register the module.** In `backend/bins/sauron-api/src/routes/mod.rs`, add `pub mod active_users;` as the FIRST module line (alphabetically before `admin`).

- [ ] **Step 3: Run the tests and confirm they pass.** Run:
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-api --bin sauron-api active_users::`
  Expected: `test result: ok. 17 passed`.
  There is deliberately no red step here, and this is the one task in the plan without one: Step 1 writes the pure functions and their tests in a single file, so there is no moment at which the tests exist and the implementation does not. `EnvFilter` already derives `PartialEq`, so no `error[E0369]` on `Vec<(Uuid, EnvFilter)>` is expected either. If a test fails, fix the implementation in Step 1, never the assertion. `-D warnings` will report `dead_code` for these items until Task 10 consumes them, which is why the lint gate is deferred to the next step.

- [ ] **Step 4: Format.** Run `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all`. Do NOT run clippy yet — the handlers in Task 10 are what consume these items, and `-D warnings` will report `dead_code` until then.

---

## Task 10: The two handlers, `AppState`, the boot probe, the 503 and the router

**Files:**
- Modify `backend/bins/sauron-api/src/error.rs` (the `ApiError` enum and its `IntoResponse`)
- Modify `backend/bins/sauron-api/src/routes/auth.rs` (visibility of `rate_limit` / `client_addr`, lines 49 and 124)
- Modify `backend/bins/sauron-api/src/routes/active_users.rs` (append the I/O layer)
- Modify `backend/bins/sauron-api/src/main.rs` (`AppState`, boot probe, CORS, two routes)

**Interfaces:**
- Consumes: everything from Task 9; `repo::{project_org, user_grants_in_org, app_ancestries, list_apps_for_project, list_app_environments, env_ids_for_apps, get_watermark, active_users_combined}`; `sauron_auth::rbac::{grants_from_rows, has_permission, reach_for, resolve_env_filter}`; `sauron_auth::perm::EVENT_READ`; `crate::csv::write_row` (Task 8).
- Produces:
  - `ApiError::Unavailable(&'static str, String)` → 503 with a caller-chosen code
  - `AppState.active_users_gate: Arc<tokio::sync::Semaphore>`, `AppState.event_users_identified: bool`
  - `pub async fn routes::active_users::active_users(...) -> Result<Json<ActiveUsersReport>, ApiError>`
  - `pub async fn routes::active_users::active_users_csv(...) -> Result<axum::response::Response, ApiError>`
  - Routes `GET /v1/projects/{project_id}/active-users` and `GET /v1/projects/{project_id}/active-users.csv`
  - CORS `expose_headers([CONTENT_DISPOSITION])`

- [ ] **Step 1: Add the 503 variant.** In `backend/bins/sauron-api/src/error.rs`, add to the enum after `RateLimited`:

```rust
    /// A 503 that names its own machine-readable code, so a caller can tell
    /// "the schema is behind the binary, run sauron-migrate" from "the server
    /// is shedding load, retry". A bare 500 from a missing column tells the
    /// operator nothing and looks like a product bug.
    Unavailable(&'static str, String),
```

and to `IntoResponse`, after the `RateLimited` arm:

```rust
            ApiError::Unavailable(code, m) => {
                body(StatusCode::SERVICE_UNAVAILABLE, code, &m)
            }
```

- [ ] **Step 2: Widen the limiter if S2 has not already.** In `backend/bins/sauron-api/src/routes/auth.rs`, if `fn client_addr` (line 49) and `async fn rate_limit` (line 124) are still module-private, change both to `pub(crate)` **in place** — do not move them, do not create a new module. Add to `rate_limit`'s doc comment:

```rust
/// Key convention: `sauron:{area}:{action}:{principal}`, e.g.
/// `sauron:analytics:active_users:{user_id}`.
```

If they are already `pub(crate)` (S2 landed first), make no change here.

- [ ] **Step 3: Append the I/O layer to `routes/active_users.rs`.** Add these imports to the top of the file:

```rust
use std::collections::HashMap;
use std::time::Duration as StdDuration;

use axum::extract::{Path, RawQuery, State};
use axum::Json;
use axum_extra::extract::Query;

// `rbac::` is not optional: `sauron-auth`'s lib.rs re-exports only
// `authorize_*`, `effective_at*`, `ensure_preset_roles`, `perm` and
// `require_permission`. `grants_from_rows`, `reach_for`, `has_permission` and
// `resolve_env_filter` live behind the module path, exactly as
// `routes/projects.rs:11` and `routes/environments.rs:84` import them.
use sauron_auth::rbac::{grants_from_rows, has_permission, reach_for, resolve_env_filter};
use sauron_auth::{perm, AuthError, AuthUser};
use sauron_db::repo;

use crate::AppState;
```

and append after the `cache_key` function (before `#[cfg(test)] mod tests`):

```rust
/// Requests per minute per user. This is the heaviest query in the product and
/// the lowest-privileged role (`Viewer` holds `event:read`) can run it. It is
/// also the repo's first read-route rate limit, and that is the point: it is
/// the template for the next one.
const ACTIVE_USERS_RATE_LIMIT: u32 = 30;
const ACTIVE_USERS_RATE_WINDOW_SECS: u64 = 60;

/// Budget for one Redis command.
///
/// Do NOT copy `collect_storage_cached`'s untimed `get`/`set_ex`.
/// `sauron-redis` builds its connection with `set_response_timeout(None)`, and
/// `routes/auth.rs` records the measurement: 9-19 s per command against a dead
/// Redis, "long enough that the in-flight cap fills and the whole API stalls".
/// "A Redis error is logged and the report computed" is only true for an
/// ERROR; an outage is a hang, twice per request. `admin_storage` gets away
/// without this because it is a rarely-loaded admin page; this is a nav-item
/// page with a Refresh button.
const CACHE_OP_TIMEOUT: StdDuration = StdDuration::from_millis(500);

/// `GET /v1/projects/{project_id}/active-users`
pub async fn active_users(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(project_id): Path<Uuid>,
    Query(q): Query<ActiveUsersQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<ActiveUsersReport>, ApiError> {
    let report = gated_report(&state, auth.user_id, project_id, &q, raw_query.as_deref()).await?;
    Ok(Json(report))
}

/// `GET /v1/projects/{project_id}/active-users.csv`
///
/// A separate route rather than `?format=csv`: with a format parameter the
/// handler's success type collapses to `Response` for both shapes and content
/// negotiation via a query param is easy to mis-validate. Both routes call one
/// `build_report`, so they can never disagree about the numbers — the only
/// thing `?format=csv` really bought.
pub async fn active_users_csv(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(project_id): Path<Uuid>,
    Query(q): Query<ActiveUsersQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<axum::response::Response, ApiError> {
    let report = gated_report(&state, auth.user_id, project_id, &q, raw_query.as_deref()).await?;

    let mut out = String::new();
    crate::csv::write_row(
        &mut out,
        &["day", "active_total", "active_identified", "active_guest"],
    );
    // Both halves ride along rather than only the total: a spreadsheet is
    // exactly where someone re-derives a figure months later with no page
    // around it to carry the cross-app-matching caveat, and a guest column
    // they can see is the only warning that survives the download. The
    // selection context deliberately stays out of the body — it is a per-file
    // constant, not a per-row value.
    for p in &report.series {
        let day = p.day.to_string();
        let total = p.active_total.to_string();
        let identified = p.active_identified.to_string();
        let guest = p.active_guest.to_string();
        crate::csv::write_row(&mut out, &[&day, &total, &identified, &guest]);
    }

    // Built from the EFFECTIVE window, so a downloaded file's name matches its
    // contents even when the tier clamp shortened it.
    let filename = format!(
        "sauron-active-users-{}-{}_{}.csv",
        project_id,
        report.effective.from.format("%Y%m%d"),
        report.effective.to.format("%Y%m%d"),
    );

    // Buffered `String` -> `Body::from`: at most 93 lines of ASCII. Streaming
    // is not an option anyway — `backend/Cargo.toml` has no `futures`, no
    // `tokio-util`, and tokio's feature list has no `fs`.
    axum::response::Response::builder()
        .header(
            axum::http::header::CONTENT_TYPE,
            "text/csv; charset=utf-8",
        )
        .header(
            axum::http::header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{filename}\""),
        )
        .body(axum::body::Body::from(out))
        .map_err(|e| ApiError::Internal(e.to_string()))
}

/// The guard stack both routes share, in the order the failures must be
/// reported: parameter shape, schema readiness, per-user rate, then admission.
async fn gated_report(
    state: &AppState,
    user_id: Uuid,
    project_id: Uuid,
    q: &ActiveUsersQuery,
    raw_query: Option<&str>,
) -> Result<ActiveUsersReport, ApiError> {
    // The environment dimension is expressed PER SELECTION. Accepting a global
    // one and ignoring it is the bug `routes::scope` exists to prevent.
    crate::routes::scope::reject_environment_id(
        crate::routes::scope::raw_environment_id(raw_query).as_deref(),
    )?;

    if !state.event_users_identified {
        return Err(ApiError::Unavailable(
            "schema_migration_required",
            "event_users.identified_at is missing; run sauron-migrate, then restart \
             sauron-api (see packaging/rpm/SETUP.md §11)"
                .into(),
        ));
    }

    crate::routes::auth::rate_limit(
        state,
        &format!("sauron:analytics:active_users:{user_id}"),
        ACTIVE_USERS_RATE_LIMIT,
        ACTIVE_USERS_RATE_WINDOW_SECS,
    )
    .await?;

    // `try_acquire`, not `acquire`: 503 ahead of the pool rather than queueing
    // behind it. The pool is 16 connections for the WHOLE process and
    // `POOL_WAIT_TIMEOUT` is 5 s, so sixteen people hitting Refresh — or one
    // person with the shareable URL open in a few tabs — would starve
    // /v1/auth/login and /health with "db pool checkout failed" 500s.
    // `ConcurrencyLimitLayer` and `TimeoutLayer` shed the HTTP request but
    // cancel neither the Postgres query nor the pool slot.
    //
    // `let _permit`, never `let _`: the latter drops the permit immediately and
    // the gate becomes a no-op that still compiles.
    let _permit = state.active_users_gate.try_acquire().map_err(|_| {
        ApiError::Unavailable(
            "busy",
            "too many active-user reports are already running; retry shortly".into(),
        )
    })?;

    build_report(state, user_id, project_id, q).await
}

async fn cache_get(state: &AppState, key: &str) -> Option<ActiveUsersReport> {
    match tokio::time::timeout(CACHE_OP_TIMEOUT, state.redis.get(key)).await {
        Ok(Ok(Some(json))) => serde_json::from_str(&json).ok(),
        Ok(Ok(None)) => None,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "active-users cache read failed");
            None
        }
        Err(_elapsed) => {
            tracing::warn!("active-users cache read timed out");
            None
        }
    }
}

async fn cache_put(state: &AppState, key: &str, report: &ActiveUsersReport) {
    let Ok(json) = serde_json::to_string(report) else {
        return;
    };
    match tokio::time::timeout(
        CACHE_OP_TIMEOUT,
        state.redis.set_ex(key, &json, ACTIVE_USERS_CACHE_TTL_SECS),
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!(error = %e, "active-users cache write failed"),
        Err(_elapsed) => tracing::warn!("active-users cache write timed out"),
    }
}

/// Resolve, authorize, clamp, cache and query. The single source of both the
/// JSON body and the CSV body.
async fn build_report(
    state: &AppState,
    user_id: Uuid,
    project_id: Uuid,
    q: &ActiveUsersQuery,
) -> Result<ActiveUsersReport, ApiError> {
    let selections = parse_selection(&q.selection)?;
    let (from, to) = validate_window(q.from, q.to, selections.len())?;
    let requested_app_ids: Vec<Uuid> = selections.iter().map(|(a, _)| *a).collect();

    let mut conn = crate::routes::db(state).await?;

    // --- the three-step reach pattern, verbatim ---------------------------
    // `repo::orgs_with_permission` is UNUSABLE here: it hardcodes
    // `g.scope_type = 'org'` and would 403 every project-, app- and env-scoped
    // member.
    let org_id = repo::project_org(&mut conn, project_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let rows = repo::user_grants_in_org(&mut conn, user_id, org_id).await?;
    if rows.is_empty() {
        return Err(ApiError::Auth(AuthError::Forbidden));
    }
    let grants = grants_from_rows(rows);
    let reach = reach_for(&grants, perm::EVENT_READ);
    if !reach.org && reach.projects.is_empty() && reach.apps.is_empty() && reach.envs.is_empty() {
        return Err(ApiError::Auth(AuthError::Forbidden));
    }

    // --- app-in-project validation ---------------------------------------
    // The caller's app ids carry no FK to the path's project, so this is
    // checked by id rather than inferred, mirroring how `validate_scopes_in_org`
    // treats a scope id that does not belong.
    let ancestries = repo::app_ancestries(&mut conn, &requested_app_ids).await?;
    let in_project: HashSet<Uuid> = ancestries
        .iter()
        .filter(|(_, project, _)| *project == project_id)
        .map(|(app, _, _)| *app)
        .collect();
    for (app_id, _) in &selections {
        if !in_project.contains(app_id) {
            return Err(ApiError::BadRequest(format!(
                "app {app_id} is not in project {project_id}"
            )));
        }
    }

    // --- per-selection environment resolution ----------------------------
    // Folded into a per-app map, NEVER passed as a flat vector: see
    // `repo::env_ids_for_apps`'s doc comment for the exact way the union
    // breaks both of `resolve_env_filter`'s decisions towards granting.
    let env_rows = repo::env_ids_for_apps(&mut conn, &requested_app_ids).await?;
    let mut envs_by_app: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
    for (app_id, env_id) in env_rows {
        envs_by_app.entry(app_id).or_default().push(env_id);
    }

    let mut scopes: Vec<AppEnvScope> = Vec::with_capacity(selections.len());
    let mut denied: Vec<String> = Vec::new();
    for (app_id, requested) in &selections {
        let app_env_ids = envs_by_app.get(app_id).map(Vec::as_slice).unwrap_or(&[]);
        // Fast path: an app-wide holder asking for everything needs no
        // narrowing at all.
        let resolved = if matches!(requested, EnvFilter::All)
            && has_permission(
                &grants,
                perm::EVENT_READ,
                org_id,
                Some(project_id),
                Some(*app_id),
                None,
            ) {
            EnvFilter::All
        } else {
            // Reusing the shipped pure decision function rather than
            // re-deriving the cascade preserves `UnattributedNeedsAppReach`
            // (so `selection=<app>:none` still requires app-wide reach) and
            // the ordering of `EnvNotInApp` before `EnvNotGranted` (so probing
            // for env ids learns nothing).
            match resolve_env_filter(
                &grants,
                perm::EVENT_READ,
                org_id,
                project_id,
                *app_id,
                app_env_ids,
                requested.clone(),
            ) {
                Ok(f) => f,
                Err(_) => {
                    denied.push(app_id.to_string());
                    continue;
                }
            }
        };
        scopes.push(AppEnvScope {
            app_id: *app_id,
            env: resolved,
        });
    }
    // Partial reach is a 403, never partial data. There is no honest way to
    // render "combined active users across A,B,C,D,E" from A,B,C: a number
    // computed over a silent subset is a wrong number presented as a right
    // one, and the CSV carries it out of the UI where no notice travels with
    // it. The denied ids are echoed because the caller supplied them, so
    // nothing new is disclosed, and the page needs them to drop a stale
    // selection and retry.
    if !denied.is_empty() {
        return Err(ApiError::Forbidden(format!(
            "no read access to app(s): {}",
            denied.join(", ")
        )));
    }

    // --- the tier clamp ---------------------------------------------------
    // `None` for a table means nothing has ever been tiered for it, so it
    // imposes no floor; the union is only complete from the MAXIMUM of the
    // watermarks that are present. Deliberately conservative: between
    // sauron-tier's export and the DETACH+DROP `TIER_DROP_LAG_HOURS` later,
    // rows past the watermark are still physically in Postgres, so a caller
    // will sometimes see `truncated: true` for a day that would still have
    // returned rows. Reporting numbers that vanish 24 h later is worse.
    let mut floor: Option<DateTime<Utc>> = None;
    for table in ["analytics_events", "error_events"] {
        if let Ok(Some(wm)) = repo::get_watermark(&mut conn, table).await {
            floor = Some(floor.map_or(wm, |cur: DateTime<Utc>| cur.max(wm)));
        }
    }

    let mut effective_from = from;
    let mut truncated = false;
    let mut truncation_reason: Option<String> = None;
    if let Some(f) = floor {
        if from < f {
            // Round UP to a whole UTC day. The grid starts at
            // `(from AT TIME ZONE 'UTC')::date`, so a mid-day floor would
            // render a partial day as a full one — the same defect flooring
            // `from` fixes on the request side. The helper is unit-tested in
            // Task 9's `a_mid_day_clamp_rounds_up_to_the_next_whole_utc_day`.
            let aligned = align_clamp_up(f);
            effective_from = aligned;
            truncated = true;
            truncation_reason = Some(format!(
                "Data older than {} has been moved to cold storage, so this report starts \
                 there instead of at {}.",
                aligned.date_naive(),
                from.date_naive()
            ));
        }
    }

    let requested_window = ReportWindow { from, to };
    let effective_window = ReportWindow {
        from: effective_from,
        to,
    };

    // --- selection views (cosmetic; the authorization input above is what
    // must stay unfiltered) ----------------------------------------------
    let apps = repo::list_apps_for_project(&mut conn, project_id).await?;
    let app_names: HashMap<Uuid, String> =
        apps.into_iter().map(|a| (a.id, a.name)).collect();
    let mut selection_views: Vec<SelectionView> = Vec::with_capacity(scopes.len());
    for s in &scopes {
        let (environment_ids, environment_labels) = match &s.env {
            EnvFilter::One(id) => (vec![*id], env_labels(&mut conn, s.app_id, &[*id]).await?),
            EnvFilter::Subset(ids) => (ids.clone(), env_labels(&mut conn, s.app_id, ids).await?),
            EnvFilter::All | EnvFilter::Unattributed => (Vec::new(), Vec::new()),
        };
        selection_views.push(SelectionView {
            app_id: s.app_id,
            app_name: app_names
                .get(&s.app_id)
                .cloned()
                .unwrap_or_else(|| s.app_id.to_string()),
            resolved: resolved_label(&s.env).to_string(),
            environment_ids,
            environment_labels,
        });
    }

    // The cache key uses the RESOLVED filters and the DAY-FLOORED requested
    // window, so the JSON call and the CSV call moments later produce the same
    // key by construction.
    let key = cache_key(project_id, from, to, &scopes)?;

    // Never hold a pooled connection across network I/O — the API pool is 16
    // for the whole process and Redis is a different host.
    drop(conn);

    if let Some(hit) = cache_get(state, &key).await {
        return Ok(hit);
    }

    let rows = if effective_from >= to {
        // The clamp swallowed the whole window. Skip the scan entirely rather
        // than paying for a query that can only return an empty grid.
        Vec::new()
    } else {
        let mut conn = crate::routes::db(state).await?;
        let rows = repo::active_users_combined(&mut conn, &scopes, effective_from, to).await?;
        drop(conn);
        rows
    };

    let series: Vec<ActiveUserPoint> = rows
        .into_iter()
        .map(|r| ActiveUserPoint {
            day: r.day,
            active_total: r.active_total,
            active_identified: r.active_identified,
            active_guest: r.active_guest,
        })
        .collect();
    let latest = latest_full_day(&series, Utc::now().date_naive()).cloned();

    let report = ActiveUsersReport {
        requested: requested_window,
        effective: effective_window,
        truncated,
        truncation_reason,
        selections: selection_views,
        series,
        latest,
    };
    cache_put(state, &key, &report).await;
    Ok(report)
}

/// Human names for a resolved environment id list.
///
/// Per-app rather than batched, deliberately: this is DISPLAY data, bounded by
/// `MAX_SELECTED_APPS`, and `list_app_environments` applies an ordering and a
/// cap that would be wrong to feed into an authorization decision.
/// `env_ids_for_apps` is the unlimited, unordered call that feeds that.
async fn env_labels(
    conn: &mut sauron_db::AsyncPgConnection,
    app_id: Uuid,
    ids: &[Uuid],
) -> Result<Vec<String>, ApiError> {
    let views = repo::list_app_environments(conn, app_id, true).await?;
    let by_id: HashMap<Uuid, String> = views
        .into_iter()
        .map(|v| (v.enrollment.id, v.name))
        .collect();
    Ok(ids
        .iter()
        .map(|id| by_id.get(id).cloned().unwrap_or_else(|| id.to_string()))
        .collect())
}
```

If `AppEnvironmentView`'s field names differ from `enrollment`/`name`, read `repo::list_app_environments`'s return type and adjust the two accessors only.

- [ ] **Step 4: Extend `AppState` and probe at boot.** In `backend/bins/sauron-api/src/main.rs`, add two fields to `AppState`:

```rust
    /// Admission gate for the active-users report — the heaviest query in the
    /// product, runnable by the lowest-privileged role. Three permits, and a
    /// 503 rather than a queue: the DB pool is 16 for the whole process, so
    /// queueing here would surface as pool-checkout 500s on unrelated
    /// endpoints, including /v1/auth/login and /health.
    pub active_users_gate: std::sync::Arc<tokio::sync::Semaphore>,
    /// Whether `event_users.identified_at` exists, probed once at boot.
    ///
    /// Probed rather than assumed because RPM upgrades do not re-run
    /// `sauron-migrate`. Refusing to START would be an unnecessary
    /// deployment-wide outage over one endpoint, so this only turns the
    /// active-users routes into a 503 that names the fix.
    pub event_users_identified: bool,
```

In the existing preset-roles block, extend it to do the probe on the same checkout:

```rust
    // Keep the seeded preset roles in sync with code, and learn once whether
    // the schema is ahead of or behind this binary.
    let event_users_identified = {
        let mut conn = sauron_db::conn(&pool).await?;
        sauron_auth::ensure_preset_roles(&mut conn).await?;
        let present = sauron_db::repo::probe_event_users_identified(&mut conn)
            .await
            .is_ok();
        drop(conn);
        if !present {
            tracing::error!(
                "event_users.identified_at is missing — run sauron-migrate (see \
                 packaging/rpm/SETUP.md §11). GET /v1/projects/{{project_id}}/active-users \
                 will return 503 schema_migration_required until it is applied."
            );
        }
        present
    };
```

and add both fields to the `AppState { … }` literal:

```rust
        alerts,
        active_users_gate: Arc::new(tokio::sync::Semaphore::new(3)),
        event_users_identified,
```

- [ ] **Step 5: Expose `Content-Disposition` through CORS.** In `backend/bins/sauron-api/src/main.rs`, extend the `cors` builder:

```rust
        .allow_headers([AUTHORIZATION, CONTENT_TYPE])
        // In BOTH shipped topologies the dashboard origin is not the API
        // origin (nginx serves the SPA on :80 with API_BASE_URL elsewhere; dev
        // is :3000 vs :8090), so without this
        // `res.headers['content-disposition']` is `undefined` in the browser
        // and every CSV download silently falls back to a generic filename —
        // a bug that reproduces in dev AND in production.
        .expose_headers([header::CONTENT_DISPOSITION]);
```

- [ ] **Step 6: Register the routes.** In `backend/bins/sauron-api/src/main.rs`, immediately after the `/v1/projects/{project_id}/monitors` route, add:

```rust
        .route(
            "/v1/projects/{project_id}/active-users",
            get(routes::active_users::active_users),
        )
        // A separate route, not `?format=csv`: browsers download GETs, the view
        // must stay bookmarkable, and one handler returning two content types
        // collapses its success type to `Response` for both.
        .route(
            "/v1/projects/{project_id}/active-users.csv",
            get(routes::active_users::active_users_csv),
        )
```

- [ ] **Step 7: Compile.** Run:
  `DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo check --workspace --all-targets`
  Expected: clean. If `EnvFilter` cannot be compared with `matches!` or `resolve_env_filter` complains about a moved value, clone the `requested` filter at the call site — it is `EnvFilter`, which is not `Copy`.

- [ ] **Step 8: Run the unit tests.** Run:
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-api --bin sauron-api`
  Expected: the 17 `active_users::tests` and the 8 `csv::tests` all pass.

- [ ] **Step 9: Format and lint.** Run:
  `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all` then
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`
  Expected: no warnings. This is the first point at which `csv.rs` has a consumer, so any `dead_code` noted in Task 8 must be gone now.

- [ ] **Step 10: Drive it once by hand.** Start the API against the dev database (`cd /home/splimter/projects/freelance/sauron/backend && DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron REDIS_URL=redis://172.20.0.3:6379 JWT_SECRET=$(openssl rand -hex 32) API_PORT=8090 CORS_ALLOWED_ORIGINS=http://localhost:3000 cargo run --bin sauron-api`), obtain a bearer token by POSTing to `/v1/auth/login`, then confirm:
  - `curl -s -H "Authorization: Bearer $T" "http://127.0.0.1:8090/v1/projects/$P/active-users?from=2026-05-01T00:00:00Z&to=2026-05-08T00:00:00Z&selection=$A"` returns a JSON body with `series` of length 7.
  - `curl -sD- -o/dev/null -H "Authorization: Bearer $T" "http://127.0.0.1:8090/v1/projects/$P/active-users.csv?from=2026-05-01T00:00:00Z&to=2026-05-08T00:00:00Z&selection=$A"` shows `content-type: text/csv; charset=utf-8`, an `access-control-expose-headers` naming `content-disposition`, and a `content-disposition` filename ending `_20260501_20260508.csv`.
  - `curl -s -o/dev/null -w '%{http_code}\n' -H "Authorization: Bearer $T" "http://127.0.0.1:8090/v1/projects/$P/active-users?from=2026-05-01T00:00:00Z&to=2026-05-08T00:00:00Z&selection=$A&environment_id=none"` prints `400`.

---

## Task 11: HTTP tests — `tests/http_active_users.rs`

**Files:**
- Create `backend/bins/sauron-api/tests/http_active_users.rs`

**Interfaces:**
- Consumes: the compiled `sauron-api` binary and the two routes from Task 10.
- Produces: no code other tasks depend on.

- [ ] **Step 1: Copy the harness.** Create `backend/bins/sauron-api/tests/http_active_users.rs` and copy, verbatim from `backend/bins/sauron-api/tests/http_workflows.rs` **lines 1-222** — the module doc, the `use` block, `JWT_SECRET`, `swap_database`, `free_port`, the whole `struct TestServer` + `impl TestServer` (`start`, `conn`, `get`, `get_status`, `get_json`, `shutdown`) and `impl Drop for TestServer`. Two exclusions inside that range, both because `--all-targets -- -D warnings` promotes `dead_code` and `unused_imports` to errors: `percent_encode_segment` (lines 50-67), which nothing below calls, and the workflow-only imports — `use sauron_db::repo::WorkflowAction;` (line 26) and `NewErrorEvent, NewIssue` out of line 24's `use sauron_db::models::{…}`, leaving `use sauron_db::models::{NewAppEnvironment, NewRoleGrant};`. Everything else in the block (`json`, `Value`, `perm`, `JwtKeys`, `repo`, `Uuid`, `Cell`, `Stdio`, the two `Duration` aliases) is used by Step 2 and stays. Everything after line 222 in `http_workflows.rs` (its own `seed_env`, `WorkflowFixture`, the second `impl TestServer`) is workflow-specific and is NOT copied; Step 2 writes this slice's equivalents. Duplicated rather than shared — see that file's own doc comment for why a cross-test-binary dependency is not worth it for machinery this small. Change three things:
  - the module doc to describe this file;
  - the ephemeral database discriminator from `wf` to `au` in `db_name` (segment order stays `sauron_test_{timestamp}_au{uuid}` — the timestamp MUST come first or `sauron-db`'s stale-db reaper silently skips the name and leaks the database);
  - `const JWT_SECRET: &str = "http-active-users-test-secret-0000000000000";`.

- [ ] **Step 2: Write the fixture and the tests.** Append to the same file. Add NO new `use` lines: `json`, `Value`, `perm`, `JwtKeys`, `repo`, `NewAppEnvironment` and `NewRoleGrant` all arrive with the block copied in Step 1, and re-importing any of them is `error[E0252]: the name … is defined multiple times`.

```rust
/// Two apps in one project. `owner_token` reaches both app-wide;
/// `env_member_token` holds `event:read` on app A's `env_a1` ONLY — the
/// persona §4.3 and §4.5 are about.
struct ActiveUsersFixture {
    project_id: Uuid,
    sibling_project_id: Uuid,
    sibling_app_id: Uuid,
    app_a: Uuid,
    app_b: Uuid,
    env_a1: Uuid,
    env_b1: Uuid,
    owner_token: String,
    env_member_token: String,
    outsider_token: String,
}

async fn seed_env(
    conn: &mut sauron_db::PgConn,
    project_id: Uuid,
    app_id: Uuid,
    name: &str,
    public_key: &str,
    is_default: bool,
) -> Uuid {
    let env = repo::create_project_environment(conn, project_id, name)
        .await
        .unwrap_or_else(|e| panic!("create catalogue env {name}: {e}"));
    repo::create_app_environments(
        conn,
        &[NewAppEnvironment {
            app_id,
            environment_id: env.id,
            public_key,
            is_default,
        }],
    )
    .await
    .unwrap_or_else(|e| panic!("enroll app in {name}: {e}"))
    .remove(0)
    .id
}

impl TestServer {
    async fn seed_active_users_fixture(&self) -> ActiveUsersFixture {
        let mut conn = self.conn().await;
        let s = Uuid::new_v4().simple().to_string();

        let org = repo::create_org(&mut conn, "au org", &format!("au-org-{s}"))
            .await
            .expect("org");
        let project = repo::create_project(&mut conn, org.id, "au project", &format!("au-p-{s}"))
            .await
            .expect("project");
        let sibling = repo::create_project(&mut conn, org.id, "au sibling", &format!("au-s-{s}"))
            .await
            .expect("sibling project");
        let app_a = repo::create_app(&mut conn, project.id, "A", &format!("au-a-{s}"), "web")
            .await
            .expect("app a");
        let app_b = repo::create_app(&mut conn, project.id, "B", &format!("au-b-{s}"), "web")
            .await
            .expect("app b");
        let sibling_app =
            repo::create_app(&mut conn, sibling.id, "S", &format!("au-sib-{s}"), "web")
                .await
                .expect("sibling app");
        let env_a1 = seed_env(&mut conn, project.id, app_a.id, "prod", &format!("pk_a1_{s}"), true).await;
        let env_b1 = seed_env(&mut conn, project.id, app_b.id, "prod-b", &format!("pk_b1_{s}"), true).await;

        let owner = repo::create_user(&mut conn, &format!("au-owner-{s}@example.test"), "x", "Owner")
            .await
            .expect("owner");
        let owner_role = repo::create_role(
            &mut conn,
            org.id,
            "au owner role",
            "org-wide event read",
            json!([perm::EVENT_READ]),
        )
        .await
        .expect("owner role");
        repo::create_grant(
            &mut conn,
            NewRoleGrant {
                org_id: org.id,
                user_id: owner.id,
                role_id: owner_role.id,
                scope_type: "org".to_string(),
                scope_id: org.id,
            },
        )
        .await
        .expect("grant owner");

        let member = repo::create_user(&mut conn, &format!("au-member-{s}@example.test"), "x", "Member")
            .await
            .expect("member");
        repo::create_grant(
            &mut conn,
            NewRoleGrant {
                org_id: org.id,
                user_id: member.id,
                role_id: owner_role.id,
                scope_type: "env".to_string(),
                scope_id: env_a1,
            },
        )
        .await
        .expect("grant member on env_a1 only");

        let outsider = repo::create_user(&mut conn, &format!("au-out-{s}@example.test"), "x", "Out")
            .await
            .expect("outsider");

        drop(conn);

        let keys = JwtKeys::new(JWT_SECRET, 900);
        let (owner_token, _) = keys.issue_access(owner.id, false).expect("owner token");
        let (env_member_token, _) = keys.issue_access(member.id, false).expect("member token");
        let (outsider_token, _) = keys.issue_access(outsider.id, false).expect("outsider token");

        ActiveUsersFixture {
            project_id: project.id,
            sibling_project_id: sibling.id,
            sibling_app_id: sibling_app.id,
            app_a: app_a.id,
            app_b: app_b.id,
            env_a1,
            env_b1,
            owner_token,
            env_member_token,
            outsider_token,
        }
    }
}

const WINDOW: &str = "from=2026-05-01T00:00:00Z&to=2026-05-08T00:00:00Z";

fn url(f: &ActiveUsersFixture, extra: &str) -> String {
    format!(
        "/v1/projects/{}/active-users?{WINDOW}&{extra}",
        f.project_id
    )
}

#[tokio::test]
async fn active_users_http_contract() {
    let Some(mut h) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_active_users");
        return;
    };
    let f = h.seed_active_users_fixture().await;

    // A caller with no grant at all in the project's org is a 403 (the project
    // itself resolves, so it is not a 404).
    assert_eq!(
        h.get_status(&url(&f, &format!("selection={}", f.app_a)), &f.outsider_token).await,
        403,
        "non-member"
    );

    // Partial reach is a 403 that NAMES the app, never partial data.
    let resp = h
        .get(
            &url(
                &f,
                &format!("selection={}:{}&selection={}", f.app_a, f.env_a1, f.app_b),
            ),
            &f.env_member_token,
        )
        .await;
    assert_eq!(resp.status().as_u16(), 403);
    let text = resp.text().await.expect("body");
    assert!(
        text.contains(&f.app_b.to_string()),
        "the 403 must name the denied app so the page can drop a stale selection: {text}"
    );

    // The same member, asking only for what they hold, succeeds.
    assert_eq!(
        h.get_status(
            &url(&f, &format!("selection={}:{}", f.app_a, f.env_a1)),
            &f.env_member_token
        )
        .await,
        200,
        "own app+env"
    );

    // The §4.5 headline: a BARE selection from an env-scoped member resolves to
    // `subset`, never `all`. With `Option<Uuid>` this would render as "All
    // environments" over a number computed from one environment.
    let body: Value = h
        .get_json(&url(&f, &format!("selection={}", f.app_a)), &f.env_member_token)
        .await;
    assert_eq!(
        body["selections"][0]["resolved"], "subset",
        "an env-scoped member's bare selection must be labelled subset: {body}"
    );

    // The dimension is per selection, so a global one is refused.
    assert_eq!(
        h.get_status(
            &url(&f, &format!("selection={}&environment_id={}", f.app_a, f.env_a1)),
            &f.owner_token
        )
        .await,
        400,
        "environment_id"
    );

    // Window validation.
    assert_eq!(
        h.get_status(
            &format!(
                "/v1/projects/{}/active-users?from=2026-05-08T00:00:00Z&to=2026-05-01T00:00:00Z&selection={}",
                f.project_id, f.app_a
            ),
            &f.owner_token
        )
        .await,
        400,
        "to < from"
    );
    assert_eq!(
        h.get_status(
            &format!(
                "/v1/projects/{}/active-users?from=2026-01-01T00:00:00Z&to=2026-06-01T00:00:00Z&selection={}",
                f.project_id, f.app_a
            ),
            &f.owner_token
        )
        .await,
        400,
        "span > 92 days"
    );

    // An app that resolves into a DIFFERENT project is a 400, not a silent
    // zero-row leg — the caller's app ids carry no FK to the path's project.
    let status = h
        .get_status(
            &url(&f, &format!("selection={}", f.sibling_app_id)),
            &f.owner_token,
        )
        .await;
    assert_eq!(status, 400, "app in project {}", f.sibling_project_id);

    h.shutdown().await;
}

/// The shared-`build_report` guarantee, checked rather than assumed.
#[tokio::test]
async fn active_users_csv_matches_the_json_route() {
    let Some(mut h) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_active_users");
        return;
    };
    let f = h.seed_active_users_fixture().await;
    let query = format!("selection={}:{}&selection={}:{}", f.app_a, f.env_a1, f.app_b, f.env_b1);

    let json: Value = h.get_json(&url(&f, &query), &f.owner_token).await;
    let series_len = json["series"].as_array().expect("series").len();

    let resp = h
        .get(
            &format!("/v1/projects/{}/active-users.csv?{WINDOW}&{query}", f.project_id),
            &f.owner_token,
        )
        .await;
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(
        resp.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("text/csv; charset=utf-8")
    );
    let disposition = resp
        .headers()
        .get("content-disposition")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let expected_prefix = format!("attachment; filename=\"sauron-active-users-{}-", f.project_id);
    assert!(
        disposition.starts_with(&expected_prefix),
        "content-disposition: {disposition}"
    );
    let dates: String = disposition
        .trim_end_matches(".csv\"")
        .chars()
        .rev()
        .take(17)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    assert!(
        dates.len() == 17 && dates.as_bytes()[8] == b'_',
        "the filename must carry two YYYYMMDD dates joined by '_': {disposition}"
    );

    let body = resp.text().await.expect("csv body");
    let mut lines = body.split("\r\n");
    assert_eq!(
        lines.next(),
        Some("day,active_total,active_identified,active_guest")
    );
    let rows = lines.filter(|l| !l.is_empty()).count();
    assert_eq!(
        rows, series_len,
        "the CSV row count must equal the JSON route's series length for the same query"
    );

    h.shutdown().await;
}
```

- [ ] **Step 3: Run them and see them fail.** Run:
  `DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron TEST_REDIS_URL=redis://172.20.0.3:6379 cargo test -p sauron-api --test http_active_users`
  Expected on the first attempt: compile errors for any harness item you forgot to copy from `http_workflows.rs` (e.g. `cannot find type 'TestServer'`). Fix by copying, not by inventing.

- [ ] **Step 4: Run them and see them pass.** Run the same command.
  Expected: `test result: ok. 2 passed`. If `active_users_http_contract` 429s partway through, the two tests are sharing a Redis limiter bucket across runs — the limiter keys on `user_id` and every run mints fresh users, so this indicates a bug in the key, not flakiness.

- [ ] **Step 5: Format and lint.** Run:
  `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all` then
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`
  Expected: no warnings.

---

## Task 12: A project-scoped class in the env-scoping reconciliation test

**Files:**
- Modify `dashboard/src/lib/api/scope.ts` (append the new exported array + a short doc block)
- Modify `dashboard/src/lib/api/scope.test.ts` (append)
- Modify `backend/bins/sauron-api/tests/http_env_scoping.rs` (`EnvScopedFixture` line 395, `seed_env_scoped_fixture`, and append two functions + one test near the existing reconciliation tests at line ~2020)

**Interfaces:**
- Produces: `export const PROJECT_SCOPED_REJECTS_ENVIRONMENT_ID: RegExp[]`; `fn project_scoped_get_route_templates() -> Vec<String>`; `fn read_dashboard_project_exclusions() -> Vec<String>`; `EnvScopedFixture.project_id: Uuid`.

- [ ] **Step 1: Add the dashboard-side array.** Append to `dashboard/src/lib/api/scope.ts`:

```ts
// ---------------------------------------------------------------------------
// Project-scoped routes.
//
// `APP_SCOPED_URL` above only matches `/v1/apps/...`, so nothing under
// `/v1/projects/...` is ever scoped by the interceptor — that part is safe by
// construction and this array changes no behaviour here. What it does is close
// the same two-directional gap for a NEW route family: the active-users
// endpoints are the first telemetry reads outside `/v1/apps/{id}/…`, so they
// sit outside the only mechanised check that a telemetry GET resolves
// environment scoping rather than accepting-and-ignoring it.
// `backend/bins/sauron-api/tests/http_env_scoping.rs` reads THIS array's
// literal source and asserts it equals the set of project-scoped GETs that
// actually 400 on a valid `environment_id`.
//
// Only the rejecting routes belong here. `/v1/projects/{id}` and
// `/v1/projects/{id}/apps` neither narrow nor reject — they are ordinary
// configuration reads with no environment dimension and no `Query` field for
// one — so listing them would make the Rust test demand a rejection the
// backend does not perform.
export const PROJECT_SCOPED_REJECTS_ENVIRONMENT_ID: RegExp[] = [
  /^\/v1\/projects\/[^/]+\/active-users(?:[/?].*)?$/,
  /^\/v1\/projects\/[^/]+\/active-users\.csv(?:[/?].*)?$/,
  /^\/v1\/projects\/[^/]+\/environments(?:[/?].*)?$/,
  /^\/v1\/projects\/[^/]+\/monitors(?:[/?].*)?$/,
];
```

- [ ] **Step 2: Write the dashboard-side test.** Append to `dashboard/src/lib/api/scope.test.ts`:

```ts
import { PROJECT_SCOPED_REJECTS_ENVIRONMENT_ID } from './scope';

describe('PROJECT_SCOPED_REJECTS_ENVIRONMENT_ID', () => {
  it('covers both active-users routes as separate entries', () => {
    const matches = (url: string) =>
      PROJECT_SCOPED_REJECTS_ENVIRONMENT_ID.some((re) => re.test(url));
    expect(matches('/v1/projects/p1/active-users')).toBe(true);
    expect(matches('/v1/projects/p1/active-users?from=x')).toBe(true);
    expect(matches('/v1/projects/p1/active-users.csv')).toBe(true);
  });

  it('does not claim the two project routes that neither narrow nor reject', () => {
    const matches = (url: string) =>
      PROJECT_SCOPED_REJECTS_ENVIRONMENT_ID.some((re) => re.test(url));
    expect(matches('/v1/projects/p1')).toBe(false);
    expect(matches('/v1/projects/p1/apps')).toBe(false);
  });

  it('is sorted, because the Rust reconciliation test compares sorted lists', () => {
    const sources = PROJECT_SCOPED_REJECTS_ENVIRONMENT_ID.map((re) => re.source);
    expect([...sources].sort()).toEqual(sources);
  });
});
```

- [ ] **Step 3: Run the dashboard tests and see them fail.** Run:
  `cd /home/splimter/projects/freelance/sauron/dashboard && npm run test`
  Expected failure before Step 1 is applied: `SyntaxError: The requested module './scope' does not provide an export named 'PROJECT_SCOPED_REJECTS_ENVIRONMENT_ID'`. With Step 1 applied, expected: pass. (If `is sorted` fails, reorder the array literal — do not relax the assertion; the Rust side sorts both sides and a mismatch there is much harder to read.)

- [ ] **Step 4: Expose `project_id` on the Rust fixture.** In `backend/bins/sauron-api/tests/http_env_scoping.rs`, add to `struct EnvScopedFixture`:

```rust
    /// The project the app hangs off — needed by the project-scoped route
    /// enumeration below, which cannot derive it from `app_id`.
    project_id: Uuid,
```

and populate it in `seed_env_scoped_fixture`'s returned literal with `project_id: project.id,` (the local is already named `project` there; if it is named differently, use whatever `repo::create_project` was bound to).

- [ ] **Step 5: Write the failing reconciliation test.** Append to `backend/bins/sauron-api/tests/http_env_scoping.rs`, next to the two existing reconciliation tests:

```rust
/// Every `.route("...", ...)` path in `main.rs`'s literal source that sits
/// under `/v1/projects/{project_id}` and attaches a `get(...)` handler — the
/// project-scoped twin of [`app_scoped_get_route_templates`], parsed out of
/// the same real router for the same reason.
fn project_scoped_get_route_templates() -> Vec<String> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/main.rs");
    let src = std::fs::read_to_string(path).unwrap_or_else(|e| {
        panic!("project_scoped_get_route_templates: could not read {path}: {e}")
    });
    let bytes = src.as_bytes();

    let marker = ".route(";
    let mut templates = Vec::new();
    let mut search_from = 0usize;
    while let Some(rel) = src[search_from..].find(marker) {
        let open_paren = search_from + rel + marker.len() - 1;
        let mut depth = 0i32;
        let mut i = open_paren;
        let mut close_paren = None;
        while i < bytes.len() {
            match bytes[i] {
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        close_paren = Some(i);
                        break;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let close_paren = close_paren.unwrap_or_else(|| {
            panic!("project_scoped_get_route_templates: unbalanced parens in {path}")
        });
        let args = &src[open_paren + 1..close_paren];
        if let Some(q1) = args.find('"') {
            if let Some(q2_rel) = args[q1 + 1..].find('"') {
                let route_path = &args[q1 + 1..q1 + 1 + q2_rel];
                let is_project_scoped = route_path == "/v1/projects/{project_id}"
                    || route_path.starts_with("/v1/projects/{project_id}/");
                let has_get = {
                    let a = args.as_bytes();
                    (0..a.len().saturating_sub(3)).any(|idx| {
                        &a[idx..idx + 4] == b"get(" && (idx == 0 || !is_ident_byte(a[idx - 1]))
                    })
                };
                if is_project_scoped && has_get {
                    templates.push(route_path.to_string());
                }
            }
        }
        search_from = close_paren + 1;
    }
    templates
}

/// `dashboard/src/lib/api/scope.ts`'s `PROJECT_SCOPED_REJECTS_ENVIRONMENT_ID`,
/// read out of that file's literal source rather than hand-copied.
fn read_dashboard_project_exclusions() -> Vec<String> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../dashboard/src/lib/api/scope.ts"
    );
    let src = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("read_dashboard_project_exclusions: could not read {path}: {e}"));
    let marker = "const PROJECT_SCOPED_REJECTS_ENVIRONMENT_ID: RegExp[] = [";
    let body_start = src
        .find(marker)
        .map(|i| i + marker.len())
        .unwrap_or_else(|| panic!("read_dashboard_project_exclusions: {marker:?} not in {path}"));
    let body_end = src[body_start..]
        .find("];")
        .map(|i| body_start + i)
        .unwrap_or_else(|| panic!("read_dashboard_project_exclusions: unterminated array"));
    let body = &src[body_start..body_end];

    const PREFIX: &str = "/^\\/v1\\/projects\\/[^/]+";
    const SUFFIX: &str = "(?:[/?].*)?$/";

    let mut templates = Vec::new();
    for raw_line in body.lines() {
        let line = raw_line.trim().trim_end_matches(',').trim();
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        let rest = line.strip_prefix(PREFIX).unwrap_or_else(|| {
            panic!(
                "read_dashboard_project_exclusions: entry {line:?} does not start with \
                 {PREFIX:?} — this parser's assumptions are stale; update it to match scope.ts"
            )
        });
        let segment = rest.strip_suffix(SUFFIX).unwrap_or_else(|| {
            panic!(
                "read_dashboard_project_exclusions: entry {line:?} does not end with {SUFFIX:?} \
                 — update this parser to match scope.ts"
            )
        });
        // `\/` -> `/` and `\.` -> `.`: the `.csv` route is the first entry in
        // either array whose literal segment contains a regex metacharacter.
        templates.push(format!(
            "/v1/projects/{{project_id}}{}",
            segment.replace("\\/", "/").replace("\\.", ".")
        ));
    }
    templates.sort();
    templates
}

/// Concrete request path for a project-scoped template, plus whatever
/// non-`environment_id` query parameters the route needs just to get past its
/// OWN `Query<T>` extraction — without them a missing-required-field 400 would
/// be indistinguishable from an `environment_id` rejection.
fn build_project_request_path(template: &str, project_id: Uuid, app_id: Uuid) -> String {
    let mut path = template.replace("{project_id}", &project_id.to_string());
    while let Some(start) = path.find('{') {
        let end = path[start..]
            .find('}')
            .map(|e| start + e)
            .unwrap_or_else(|| panic!("build_project_request_path: unbalanced '{{' in {template:?}"));
        let param = path[start + 1..end].to_string();
        panic!(
            "build_project_request_path: template {template:?} has an unhandled path parameter \
             {{{param}}} — add a substitution rather than sending the literal text"
        );
    }
    let extra_query: Option<String> = match template {
        "/v1/projects/{project_id}/active-users" | "/v1/projects/{project_id}/active-users.csv" => {
            Some(format!(
                "from=2026-05-01T00:00:00Z&to=2026-05-08T00:00:00Z&selection={app_id}"
            ))
        }
        _ => None,
    };
    if let Some(q) = extra_query {
        path.push('?');
        path.push_str(&q);
    }
    path
}

/// The set of `/v1/projects/{id}/…` GETs that reject `environment_id` outright
/// must equal `scope.ts`'s `PROJECT_SCOPED_REJECTS_ENVIRONMENT_ID`.
///
/// The active-users routes are the first telemetry reads outside
/// `/v1/apps/{id}/…`, so `APP_SCOPED_URL` never matches them and
/// `app_scoped_get_route_templates` never enumerates them. Compensating with
/// one bespoke case in a new file would mean the next author never learns to
/// replicate it; this makes `reject_environment_id` mandatory-by-test for
/// every future project-scoped telemetry route.
#[tokio::test]
async fn the_project_rejection_set_matches_the_dashboard_project_exclusion_list() {
    let Some(mut h) = TestServer::start().await else {
        eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping http_env_scoping");
        return;
    };
    let f = h.seed_env_scoped_fixture().await;

    let templates = project_scoped_get_route_templates();
    assert!(
        templates.len() >= 4,
        "project_scoped_get_route_templates() returned only {} route(s): {templates:?} — a test \
         that silently enumerates too few routes passes forever and guards nothing.",
        templates.len(),
    );

    let mut rejecting = Vec::new();
    for template in &templates {
        let base = build_project_request_path(template, f.project_id, f.app_id);
        // A rejecting route 400s even on a perfectly VALID value.
        let path = with_environment_id(&base, &f.granted_env.to_string());
        // `org_owner_token` holds the Owner preset at org scope, so a non-400
        // here is about environment handling and never about permissions.
        let status = h.get_status(&path, &f.org_owner_token).await;
        if status == 400 {
            rejecting.push(template.clone());
        }
    }
    rejecting.sort();
    rejecting.dedup();

    let expected = read_dashboard_project_exclusions();
    assert_eq!(
        rejecting, expected,
        "the backend's project-scoped rejecting-route set and \
         dashboard/src/lib/api/scope.ts's PROJECT_SCOPED_REJECTS_ENVIRONMENT_ID have diverged"
    );

    h.shutdown().await;
}
```

- [ ] **Step 6: Run it and see it fail, then pass.** Run:
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron TEST_REDIS_URL=redis://172.20.0.3:6379 cargo test -p sauron-api --test http_env_scoping the_project_rejection_set_matches_the_dashboard_project_exclusion_list`
  Expected before Step 4: `error[E0609]: no field 'project_id' on type 'EnvScopedFixture'`. After Steps 4-5: `test result: ok. 1 passed`. If the assertion reports a difference, the two lists genuinely disagree — fix whichever side is wrong rather than editing the expectation.

- [ ] **Step 7: Run the whole env-scoping suite.** Run:
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron TEST_REDIS_URL=redis://172.20.0.3:6379 cargo test -p sauron-api --test http_env_scoping`
  Expected: every test passes, including the two pre-existing app-scoped reconciliation tests.

- [ ] **Step 8: Format and lint.** Run:
  `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all` then
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`
  Expected: no warnings.

---

## Task 13: `models/active-users.ts` — the pure selection layer

**Files:**
- Create `dashboard/src/lib/models/active-users.ts`
- Create `dashboard/src/lib/models/active-users.test.ts`

**Interfaces:**
- Produces: `EnvChoice`, `AppEnvSelection`, `MAX_SELECTED_APPS`, `encodeSelection`, `decodeSelection`, `selectionCount`, `validateSelection`, `describeSelection`, `defaultWindow`, `utcDayLabel`.

- [ ] **Step 1: Write the failing tests.** Create `dashboard/src/lib/models/active-users.test.ts`:

```ts
import { describe, expect, it } from 'vitest';
import {
  decodeSelection,
  defaultWindow,
  describeSelection,
  encodeSelection,
  MAX_SELECTED_APPS,
  selectionCount,
  utcDayLabel,
  validateSelection,
  type AppEnvSelection,
} from './active-users';

describe('encodeSelection / decodeSelection', () => {
  it('sorts by app id so the URL and the server cache key are stable', () => {
    const sel: AppEnvSelection = { 'b-app': 'all', 'a-app': 'env-1' };
    expect(encodeSelection(sel)).toEqual(['a-app:env-1', 'b-app']);
  });

  it('emits a bare app id for "all" and round-trips it back', () => {
    const sel: AppEnvSelection = { 'a-app': 'all', 'b-app': 'none', 'c-app': 'env-9' };
    const encoded = encodeSelection(sel);
    expect(encoded).toEqual(['a-app', 'b-app:none', 'c-app:env-9']);
    expect(decodeSelection(encoded)).toEqual(sel);
  });

  it('decodes a bare app id as "all"', () => {
    expect(decodeSelection(['x'])).toEqual({ x: 'all' });
  });

  it('ignores an empty token rather than minting an empty app id', () => {
    expect(decodeSelection(['', 'x'])).toEqual({ x: 'all' });
  });
});

describe('selectionCount / validateSelection', () => {
  it('counts apps, not tokens', () => {
    expect(selectionCount({ a: 'all', b: 'none' })).toBe(2);
    expect(selectionCount({})).toBe(0);
  });

  it('rejects an empty selection', () => {
    const r = validateSelection({});
    expect(r.ok).toBe(false);
    if (!r.ok) expect(r.reason).toMatch(/at least one/i);
  });

  it('rejects more than MAX_SELECTED_APPS', () => {
    const sel: AppEnvSelection = {};
    for (let i = 0; i <= MAX_SELECTED_APPS; i += 1) sel[`app-${i}`] = 'all';
    const r = validateSelection(sel);
    expect(r.ok).toBe(false);
    if (!r.ok) expect(r.reason).toContain(String(MAX_SELECTED_APPS));
  });

  it('accepts a selection at the cap', () => {
    const sel: AppEnvSelection = {};
    for (let i = 0; i < MAX_SELECTED_APPS; i += 1) sel[`app-${i}`] = 'all';
    expect(validateSelection(sel)).toEqual({ ok: true });
  });
});

describe('describeSelection', () => {
  const name = (id: string) => id.toUpperCase();
  const env = (_appId: string, choice: string) => (choice === 'all' ? 'All environments' : choice);

  it('names the environment when exactly one app is selected', () => {
    expect(describeSelection({ web: 'prod' }, name, env)).toBe('WEB · prod');
  });

  it('lists both when two are selected', () => {
    expect(describeSelection({ web: 'all', api: 'all' }, name, env)).toBe('API, WEB');
  });

  it('summarises the tail past two', () => {
    expect(describeSelection({ a: 'all', b: 'all', c: 'all', d: 'all' }, name, env)).toBe(
      'A, B +2 more',
    );
  });

  it('says so when nothing is selected', () => {
    expect(describeSelection({}, name, env)).toBe('No apps selected');
  });
});

describe('defaultWindow', () => {
  it('ends at the start of tomorrow UTC so today is included but never partial-labelled', () => {
    const now = new Date('2026-05-07T18:30:00Z');
    expect(defaultWindow(30, now)).toEqual({
      from: '2026-04-08T00:00:00.000Z',
      to: '2026-05-08T00:00:00.000Z',
    });
  });

  it('is unaffected by the viewer local zone', () => {
    const now = new Date('2026-05-07T23:59:59Z');
    expect(defaultWindow(7, now).to).toBe('2026-05-08T00:00:00.000Z');
  });
});

describe('utcDayLabel', () => {
  it('labels a UTC calendar day in UTC, not in the viewer local zone', () => {
    // The trap this exists for, pinned explicitly so the assertion below is
    // meaningful even on a UTC runner: `new Date('2026-07-31')` parses as UTC
    // but RENDERS in local time, so at a negative offset the bar for the 31st
    // is labelled "Jul 30" while the CSV row says 2026-07-31.
    const naive = new Date('2026-07-31').toLocaleDateString('en-US', {
      month: 'short',
      day: 'numeric',
      timeZone: 'America/New_York',
    });
    expect(naive).toBe('Jul 30');
    expect(utcDayLabel('2026-07-31', 'en-US')).toBe('Jul 31');
  });

  it('passes a value it cannot parse straight through', () => {
    expect(utcDayLabel('not-a-day', 'en-US')).toBe('not-a-day');
  });
});
```

- [ ] **Step 2: Run them and see them fail.** Run:
  `cd /home/splimter/projects/freelance/sauron/dashboard && npm run test`
  Expected failure: `Failed to resolve import "./active-users"`.

- [ ] **Step 3: Implement.** Create `dashboard/src/lib/models/active-users.ts`:

```ts
// Pure decision logic for the Active Users page. Lives here, not in the
// component, because the dashboard has no DOM test environment — this module
// is the only layer of the feature a test can reach.

/** An `AppEnvironment.id`, or the literal `'all'` / `'none'`. */
export type EnvChoice = string;

/** Which environment was chosen for each ticked app. */
export interface AppEnvSelection {
  [appId: string]: EnvChoice;
}

/** Mirrors the backend's `MAX_SELECTED_APPS`; the server 400s past it. */
export const MAX_SELECTED_APPS = 20;

/**
 * Wire tokens for `?selection=`, sorted by app id.
 *
 * Sorting is not cosmetic: the server's Redis cache key hashes the resolved
 * selection, and a stable URL is what makes an export reproducible from the
 * link that produced it. A bare app id means `all`, which keeps the common URL
 * short and round-trips exactly.
 */
export function encodeSelection(sel: AppEnvSelection): string[] {
  return Object.keys(sel)
    .sort()
    .map((appId) => (sel[appId] === 'all' ? appId : `${appId}:${sel[appId]}`));
}

/** Inverse of {@link encodeSelection}. A bare app id decodes to `all`. */
export function decodeSelection(params: string[]): AppEnvSelection {
  const out: AppEnvSelection = {};
  for (const raw of params) {
    const token = raw.trim();
    if (!token) continue;
    const colon = token.indexOf(':');
    if (colon === -1) {
      out[token] = 'all';
    } else {
      const appId = token.slice(0, colon);
      const choice = token.slice(colon + 1);
      if (appId) out[appId] = choice || 'all';
    }
  }
  return out;
}

export function selectionCount(sel: AppEnvSelection): number {
  return Object.keys(sel).length;
}

export function validateSelection(
  sel: AppEnvSelection,
): { ok: true } | { ok: false; reason: string } {
  const n = selectionCount(sel);
  if (n === 0) return { ok: false, reason: 'Pick at least one app.' };
  if (n > MAX_SELECTED_APPS) {
    return { ok: false, reason: `Pick at most ${MAX_SELECTED_APPS} apps.` };
  }
  return { ok: true };
}

/**
 * A one-line summary for the "Apps" tile. Names the environment only when a
 * single app is selected — with several, the per-app environments differ and a
 * concatenated list reads as one combined filter, which it is not.
 */
export function describeSelection(
  sel: AppEnvSelection,
  appName: (appId: string) => string,
  envLabel: (appId: string, choice: EnvChoice) => string,
): string {
  const ids = Object.keys(sel).sort();
  if (ids.length === 0) return 'No apps selected';
  if (ids.length === 1) return `${appName(ids[0])} · ${envLabel(ids[0], sel[ids[0]])}`;
  const named = ids.slice(0, 2).map(appName).join(', ');
  return ids.length === 2 ? named : `${named} +${ids.length - 2} more`;
}

/**
 * The default `[from, to)` window for a range of `rangeDays` whole UTC days
 * ending with today.
 *
 * `to` is the START of tomorrow UTC, so today's still-filling bar is included
 * in the chart (dropping it would make the range shorter than the picker says)
 * while the headline tiles read from the last COMPLETE day. Both ends are day
 * boundaries because the server floors them anyway, and sending an already
 * floored pair is what keeps the JSON request and the CSV request moments
 * later on the same cache key.
 */
export function defaultWindow(rangeDays: number, now: Date): { from: string; to: string } {
  const toMs = Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), now.getUTCDate() + 1);
  const fromMs = toMs - rangeDays * 86_400_000;
  return { from: new Date(fromMs).toISOString(), to: new Date(toMs).toISOString() };
}

/**
 * Render a `YYYY-MM-DD` bucket as a short label IN UTC.
 *
 * `new Date('2026-07-31')` parses as UTC but renders in local time, so a
 * viewer at a negative offset would see the chart and the CSV disagree about
 * which day a number belongs to. `locale` exists so a test can pin the output
 * without pinning the runner's locale.
 */
export function utcDayLabel(day: string, locale?: string): string {
  const d = new Date(`${day}T00:00:00Z`);
  if (Number.isNaN(d.getTime())) return day;
  return d.toLocaleDateString(locale, { month: 'short', day: 'numeric', timeZone: 'UTC' });
}
```

- [ ] **Step 4: Run them and see them pass.** Run:
  `cd /home/splimter/projects/freelance/sauron/dashboard && npm run test`
  Expected: all `active-users` tests pass.

- [ ] **Step 5: Run the suite under a non-UTC clock too.** Run:
  `cd /home/splimter/projects/freelance/sauron/dashboard && TZ=America/New_York npm run test`
  Expected: identical results. A failure here means something in the module reached for the local zone.

- [ ] **Step 6: Typecheck.** Run:
  `cd /home/splimter/projects/freelance/sauron/dashboard && npm run check`
  Expected: `0 errors`.

---

## Task 14: Blob error bodies and the download helper

**Files:**
- Modify `dashboard/src/lib/api/client.ts` (the response interceptor at lines 116-146, and the export list)
- Create `dashboard/src/lib/api/client.test.ts`
- Create `dashboard/src/lib/api/download.ts`
- Create `dashboard/src/lib/api/download.test.ts`

**Interfaces:**
- Produces: `export async function unwrapBlobErrorBody(error: AxiosError): Promise<void>`; `export async function downloadCsv(url: string, params: Record<string, unknown>, fallbackFilename: string): Promise<void>`; `export function filenameFromDisposition(value: string): string | null`.

- [ ] **Step 1: Write the failing tests.** Create `dashboard/src/lib/api/client.test.ts`:

```ts
import { describe, expect, it } from 'vitest';
import { AxiosError } from 'axios';
import { normalizeError, unwrapBlobErrorBody } from './client';

function blobError(status: number, envelope: unknown): AxiosError {
  const blob = new Blob([JSON.stringify(envelope)], { type: 'application/json' });
  return new AxiosError(
    `Request failed with status code ${status}`,
    'ERR_BAD_REQUEST',
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    {} as any,
    undefined,
    {
      status,
      statusText: 'Forbidden',
      data: blob,
      headers: {},
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      config: {} as any,
    },
  );
}

describe('unwrapBlobErrorBody', () => {
  it('turns a Blob-bodied 403 into the envelope its message lives in', async () => {
    const err = blobError(403, {
      error: { code: 'forbidden', message: 'no read access to app(s): abc' },
    });
    // Without the unwrap, `normalizeError` reads the Blob as an envelope, gets
    // `undefined`, and degrades to axios's generic string — which is exactly
    // the case the 403 was designed for: a user opens a shared /active-users
    // URL after a grant was revoked and the toast has to name the app.
    expect(normalizeError(err).message).toBe('Request failed with status code 403');

    await unwrapBlobErrorBody(err);
    expect(normalizeError(err).message).toBe('no read access to app(s): abc');
    expect(normalizeError(err).code).toBe('forbidden');
  });

  it('leaves a non-JSON Blob alone rather than throwing', async () => {
    const err = new AxiosError(
      'Request failed with status code 500',
      'ERR_BAD_RESPONSE',
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      {} as any,
      undefined,
      {
        status: 500,
        statusText: 'Server Error',
        data: new Blob(['day,active_total\r\n']),
        headers: {},
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        config: {} as any,
      },
    );
    await expect(unwrapBlobErrorBody(err)).resolves.toBeUndefined();
    expect(normalizeError(err).message).toBe('Request failed with status code 500');
  });

  it('is a no-op when there is no response at all', async () => {
    const err = new AxiosError('Network Error', 'ERR_NETWORK');
    await expect(unwrapBlobErrorBody(err)).resolves.toBeUndefined();
  });
});
```

and create `dashboard/src/lib/api/download.test.ts`:

```ts
import { describe, expect, it } from 'vitest';
import { filenameFromDisposition } from './download';

describe('filenameFromDisposition', () => {
  it('reads a quoted filename', () => {
    expect(
      filenameFromDisposition('attachment; filename="sauron-active-users-p1-20260501_20260508.csv"'),
    ).toBe('sauron-active-users-p1-20260501_20260508.csv');
  });

  it('reads an unquoted filename', () => {
    expect(filenameFromDisposition('attachment; filename=report.csv')).toBe('report.csv');
  });

  it('returns null when the header is absent or has no filename', () => {
    expect(filenameFromDisposition('')).toBeNull();
    expect(filenameFromDisposition('attachment')).toBeNull();
  });
});
```

- [ ] **Step 2: Run them and see them fail.** Run:
  `cd /home/splimter/projects/freelance/sauron/dashboard && npm run test`
  Expected failure: `does not provide an export named 'unwrapBlobErrorBody'` and `Failed to resolve import "./download"`.

- [ ] **Step 3: Implement the unwrap in `client.ts`.** Add the exported helper above the response interceptor:

```ts
/**
 * With `responseType: 'blob'` (the CSV export) an ERROR body is a Blob too, so
 * `normalizeError`'s `response.data as ApiErrorEnvelope` read yields
 * `undefined` and the message degrades to axios's generic "Request failed with
 * status code 403".
 *
 * This belongs in the interceptor, not in the caller: every branch of the
 * handler below ends in `Promise.reject(normalizeError(error))`, so by the time
 * a caller's `catch` runs there is no `error.response` left to re-read. Doing
 * it here also means every future blob-returning endpoint gets the fix free.
 */
export async function unwrapBlobErrorBody(error: AxiosError): Promise<void> {
  const data = error.response?.data as unknown;
  if (!(data instanceof Blob)) return;
  try {
    const text = await data.text();
    (error.response as { data: unknown }).data = JSON.parse(text);
  } catch {
    /* not JSON — leave the Blob in place */
  }
}
```

and call it inside the rejection handler, immediately after the `if (!error.response)` guard and **before** the status branching:

```ts
    // Must run before the branching below: see `unwrapBlobErrorBody`.
    await unwrapBlobErrorBody(error);

    const status = error.response.status;
```

- [ ] **Step 4: Implement `download.ts`.** Create `dashboard/src/lib/api/download.ts`:

```ts
import { api } from './client';

/**
 * Pull `filename` out of a `Content-Disposition` header value.
 *
 * Exported and pure so it can be tested: the download itself touches the DOM,
 * and there is no DOM test environment here.
 */
export function filenameFromDisposition(value: string): string | null {
  const quoted = /filename="([^"]+)"/.exec(value);
  if (quoted) return quoted[1];
  const bare = /filename=([^;]+)/.exec(value);
  if (bare) return bare[1].trim();
  return null;
}

/**
 * Fetch `url` as a Blob and hand it to the browser as a download.
 *
 * Goes through the SHARED `api` instance, not a bare axios call, so it keeps
 * the bearer header and the 401 refresh-and-replay — the replay path does
 * `api(original)` with the original config, so `responseType` survives it.
 *
 * `paramsSerializer: { indexes: null }` is load-bearing: axios 1.x's default
 * serializer renders an array as `key[]=a&key[]=b`, which `serde_html_form`'s
 * `Vec<String>` on the server does not accept. `indexes: null` produces the
 * repeated `key=a&key=b` form the backend actually parses.
 *
 * `fallbackFilename` is used when `Content-Disposition` is unreadable — in both
 * shipped topologies the dashboard origin is not the API origin, so the header
 * only reaches JS because the API's CORS layer exposes it. Callers build the
 * fallback from the same ids and effective dates the server uses, so the file
 * is correctly named even if that ever regresses.
 *
 * Error handling is deliberately absent: `client.ts` unwraps the Blob error
 * body before normalizing, so the caller's `errorMessage(err)` already reads
 * the real message.
 */
export async function downloadCsv(
  url: string,
  params: Record<string, unknown>,
  fallbackFilename: string,
): Promise<void> {
  const res = await api.get(url, {
    params,
    responseType: 'blob',
    paramsSerializer: { indexes: null },
  });
  const disposition = String(res.headers['content-disposition'] ?? '');
  const filename = filenameFromDisposition(disposition) ?? fallbackFilename;
  const href = URL.createObjectURL(res.data as Blob);
  try {
    const a = document.createElement('a');
    a.href = href;
    a.download = filename;
    a.rel = 'noopener';
    document.body.appendChild(a);
    a.click();
    a.remove();
  } finally {
    // The click starts the download synchronously, so revoking here is safe
    // and is the only place that runs on both the success and the throw path.
    URL.revokeObjectURL(href);
  }
}
```

- [ ] **Step 5: Run them and see them pass.** Run:
  `cd /home/splimter/projects/freelance/sauron/dashboard && npm run test`
  Expected: all `client` and `download` tests pass.

- [ ] **Step 6: Typecheck.** Run:
  `cd /home/splimter/projects/freelance/sauron/dashboard && npm run check`
  Expected: `0 errors`. If `noUnusedLocals` flags the `AxiosError` import in `client.ts`, it is now used as a value type in the new function signature and the flag should clear on its own.

---

## Task 15: Wire types, the API module, the icon and the chart label

**Files:**
- Modify `dashboard/src/lib/models/index.ts` (append near the other analytics types)
- Create `dashboard/src/lib/api/activeUsers.ts`
- Modify `dashboard/src/lib/components/ui/Icon.svelte` (import list and `iconRegistry`)
- Modify `dashboard/src/lib/components/TimeSeriesChart.svelte`

**Interfaces:**
- Consumes: `downloadCsv` (Task 14), `utcDayLabel` (Task 13).
- Produces: `ActiveUsersReport`, `ActiveUserPoint`, `SelectionView`, `ReportWindow` in `models/index.ts`; `ActiveUsersParams`, `getActiveUsers`, `activeUsersCsvPath`, `downloadActiveUsersCsv` in `api/activeUsers.ts`; icon name `'download'`; `TimeSeriesChart` prop `label?: (bucket: string) => string`.

- [ ] **Step 1: Add the wire types.** Append to `dashboard/src/lib/models/index.ts`:

```ts
// ---------------------------------------------------------------------------
// Combined active users (project-scoped)
// ---------------------------------------------------------------------------

export interface ReportWindow {
  from: string;
  to: string;
}

export interface ActiveUserPoint {
  /** A UTC calendar day, `YYYY-MM-DD`. Never a timestamp. */
  day: string;
  active_total: number;
  active_identified: number;
  active_guest: number;
}

export interface SelectionView {
  app_id: string;
  app_name: string;
  /**
   * The filter the server ACTUALLY applied: `all` | `one` | `subset` |
   * `unattributed`. `subset` means the caller's grants reach only some of the
   * app's environments, so the number covers fewer environments than the
   * picker's "All environments" suggests — the page must say so.
   */
  resolved: 'all' | 'one' | 'subset' | 'unattributed';
  environment_ids: string[];
  environment_labels: string[];
}

export interface ActiveUsersReport {
  requested: ReportWindow;
  effective: ReportWindow;
  truncated: boolean;
  /** A full sentence, rendered verbatim. */
  truncation_reason: string | null;
  selections: SelectionView[];
  series: ActiveUserPoint[];
  /** The last COMPLETE UTC day, or null when the window contains only today. */
  latest: ActiveUserPoint | null;
}
```

- [ ] **Step 2: Create the API module.** Create `dashboard/src/lib/api/activeUsers.ts`:

```ts
import { api } from './client';
import { downloadCsv } from './download';
import type { ActiveUsersReport } from '../models';

/**
 * Request parameters. Lives here rather than in `models/`, per the
 * `api/alerts.ts` convention: response and domain types are shared, request
 * shapes belong to the module that sends them.
 */
export interface ActiveUsersParams {
  /** RFC3339, already floored to a UTC day boundary by the caller. */
  from: string;
  to: string;
  /** Repeated `?selection=` tokens from `models/active-users.ts`. */
  selection: string[];
}

/**
 * `indexes: null` is load-bearing. Axios 1.x's default serializer renders an
 * array as `selection[]=a&selection[]=b`; the backend deserializes
 * `Vec<String>` with `serde_html_form`, which wants the repeated
 * `selection=a&selection=b` form. Without this the server sees an empty
 * selection and 400s.
 */
const REPEATED_KEYS = { indexes: null } as const;

export async function getActiveUsers(
  projectId: string,
  params: ActiveUsersParams,
): Promise<ActiveUsersReport> {
  const { data } = await api.get<ActiveUsersReport>(
    `/v1/projects/${projectId}/active-users`,
    { params, paramsSerializer: REPEATED_KEYS },
  );
  return data;
}

export function activeUsersCsvPath(projectId: string): string {
  return `/v1/projects/${projectId}/active-users.csv`;
}

/**
 * `fallbackFilename` is built from the same ids and EFFECTIVE dates the server
 * uses, so a download is correctly named even if CORS ever stops exposing
 * `Content-Disposition`.
 */
export async function downloadActiveUsersCsv(
  projectId: string,
  params: ActiveUsersParams,
  effective: { from: string; to: string },
): Promise<void> {
  const stamp = (iso: string) => iso.slice(0, 10).replace(/-/g, '');
  const fallback = `sauron-active-users-${projectId}-${stamp(effective.from)}_${stamp(
    effective.to,
  )}.csv`;
  await downloadCsv(activeUsersCsvPath(projectId), { ...params }, fallback);
}
```

- [ ] **Step 3: Add the download icon.** In `dashboard/src/lib/components/ui/Icon.svelte`, add the import in alphabetical position (after `Diamond`):

```ts
  import Download from '@lucide/svelte/icons/download';
```

and the registry entry (after `diamond: Diamond,`):

```ts
    download: Download,
```

- [ ] **Step 4: Add the `label` prop to `TimeSeriesChart`.** In `dashboard/src/lib/components/TimeSeriesChart.svelte`, extend `Props`:

```ts
  interface Props {
    data: SeriesPoint[];
    height?: number;
    color?: string;
    emptyLabel?: string;
    format?: (n: number) => string;
    showTotal?: boolean;
    /**
     * Override how a bucket is rendered on the axis AND in the tooltip.
     *
     * The default path parses `bucket` as a Date and renders it in the
     * VIEWER's zone. That is correct for the timestamp buckets every existing
     * caller passes, and wrong for a pure `YYYY-MM-DD` calendar day: parsing
     * is UTC, rendering is not, so in `America/New_York` the bar for
     * `2026-07-31` is labelled "Jul 30" and its tooltip reads "Jul 30, 2026,
     * 08:00 PM" — a time of day on a bucket that has none. The active-users
     * page passes `utcDayLabel` so its chart, its CSV and its filename cannot
     * disagree about which day a number belongs to.
     */
    label?: (bucket: string) => string;
  }
```

destructure it as `label: labelProp` and rewrite the local `label` function:

```ts
  let {
    data,
    height = 160,
    color = 'var(--primary)',
    emptyLabel = 'No data in this range',
    format = (n: number) => n.toLocaleString(),
    showTotal = true,
    label: labelProp,
  }: Props = $props();
```

```ts
  function label(bucket: string): string {
    if (labelProp) return labelProp(bucket);
    const d = new Date(bucket);
    if (Number.isNaN(d.getTime())) return bucket;
    return d.toLocaleDateString(undefined, { month: 'short', day: 'numeric' });
  }

  // The hover title uses the same function when the prop is supplied;
  // `formatDateTime` would put a time of day on a calendar-day bucket.
  function tooltip(bucket: string): string {
    return labelProp ? labelProp(bucket) : formatDateTime(bucket);
  }
```

and change the column markup's `title` to use it:

```svelte
        <div class="col" title={`${tooltip(point.bucket)} · ${format(point.count)}`}>
```

- [ ] **Step 5: Typecheck and run the tests.** Run:
  `cd /home/splimter/projects/freelance/sauron/dashboard && npm run check`
  then
  `cd /home/splimter/projects/freelance/sauron/dashboard && npm run test`
  Expected: `0 errors` and all tests pass. Every existing `TimeSeriesChart` caller omits `label`, so the default branch is unchanged.

---

## Task 16: `AppEnvPicker.svelte`

**Files:**
- Create `dashboard/src/lib/components/AppEnvPicker.svelte`

**Interfaces:**
- Consumes: `AppEnvSelection`, `EnvChoice` (Task 13); `SelectionView` (Task 15); `App`, `AppEnvironment` from `models/index.ts`; `sessionStore.can`.
- Produces: a component with props `{ apps: App[]; envsByApp: Record<string, AppEnvironment[]>; loadingEnvApps: Set<string>; value: AppEnvSelection; resolvedByApp: Record<string, SelectionView>; onchange: (next: AppEnvSelection) => void; onopenapp: (appId: string) => void }`.

- [ ] **Step 1: Create the component.** Create `dashboard/src/lib/components/AppEnvPicker.svelte`:

```svelte
<!--
  One row per app: a checkbox that selects the app and a `<select>` that picks
  which environment its numbers come from.

  `ScopeTree.svelte` cannot be reused, and the reason matters. `ScopeSelection`
  is `{ org, projects[], apps[], envs[] }` and `selectionToScopes` COLLAPSES a
  ticked env under a ticked app — that collapse is the whole point of the grant
  model and it is exactly the pairing this feature must preserve. Running this
  selection through `grant-plan.ts`'s coverage-diff machinery would actively
  destroy the per-app environment choice.

  Raw `<input type="checkbox">` inside a `<label class="node">` and a raw
  `<select class="sel">`: `lib/components/ui/` has no Checkbox and no Select
  primitive, and this is the idiom `ScopeTree`/`PermissionPicker` already use.
-->
<script lang="ts">
  import Spinner from './ui/Spinner.svelte';
  import { sessionStore } from '../stores/session.svelte';
  import type { AppEnvSelection, EnvChoice } from '../models/active-users';
  import type { App, AppEnvironment, SelectionView } from '../models';

  interface Props {
    apps: App[];
    /** Enrollments per app, lazily loaded by the page. */
    envsByApp: Record<string, AppEnvironment[]>;
    loadingEnvApps: Set<string>;
    value: AppEnvSelection;
    /** Keyed by app id; supplies the "2 of 5 environments" label. */
    resolvedByApp: Record<string, SelectionView>;
    onchange: (next: AppEnvSelection) => void;
    onopenapp: (appId: string) => void;
  }

  let { apps, envsByApp, loadingEnvApps, value, resolvedByApp, onchange, onopenapp }: Props =
    $props();

  function toggle(appId: string, checked: boolean) {
    // Records in `$state` are REPLACED, never mutated: a mutation on a
    // deep-proxied object is not what the parent's `$effect` compares against,
    // and the reload silently does not fire.
    const next: AppEnvSelection = { ...value };
    if (checked) {
      next[appId] = 'all';
      onopenapp(appId);
    } else {
      delete next[appId];
    }
    onchange(next);
  }

  function chooseEnv(appId: string, choice: EnvChoice) {
    onchange({ ...value, [appId]: choice });
  }

  /**
   * "Unattributed" is offered only to a caller with app-wide reach, mirroring
   * the backend's `UnattributedNeedsAppReach`: rows attributed to no
   * environment belong to no single environment, so an env-scoped grant can
   * never authorize them. Offering it anyway would produce a 403 on selection.
   */
  function canSeeUnattributed(appId: string): boolean {
    return sessionStore.can('event:read', { app: appId });
  }

  /**
   * What the row says the environment filter is. When the server came back
   * `subset`, the picker's own "All environments" is a LIE — the caller's
   * grants reach only some of the app's environments and the number covers
   * only those.
   */
  function envSummary(appId: string): string | null {
    const view = resolvedByApp[appId];
    if (!view || view.resolved !== 'subset') return null;
    const total = envsByApp[appId]?.length ?? view.environment_ids.length;
    return `${view.environment_ids.length} of ${total} environments`;
  }
</script>

<div class="picker">
  {#each apps as app (app.id)}
    {@const checked = app.id in value}
    <div class="row">
      <label class="node">
        <input
          type="checkbox"
          {checked}
          onchange={(e) => toggle(app.id, (e.currentTarget as HTMLInputElement).checked)}
        />
        <span class="name">{app.name}</span>
      </label>

      <div class="env">
        {#if checked && loadingEnvApps.has(app.id)}
          <Spinner size={14} />
        {:else}
          <select
            class="sel"
            disabled={!checked}
            value={value[app.id] ?? 'all'}
            onchange={(e) => chooseEnv(app.id, (e.currentTarget as HTMLSelectElement).value)}
          >
            <option value="all">All environments</option>
            {#each envsByApp[app.id] ?? [] as env (env.id)}
              <option value={env.id}>{env.name}</option>
            {/each}
            {#if canSeeUnattributed(app.id)}
              <option value="none">Unattributed</option>
            {/if}
          </select>
        {/if}
        {#if checked}
          {@const summary = envSummary(app.id)}
          {#if summary}
            <span class="subset" title="Your access reaches only some of this app's environments.">
              {summary}
            </span>
          {/if}
        {/if}
      </div>
    </div>
  {/each}

  {#if apps.length === 0}
    <p class="muted">No apps in this project.</p>
  {/if}
</div>

<style>
  .picker {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 6px 8px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--surface);
  }
  .node {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    cursor: pointer;
    min-width: 0;
  }
  .name {
    font-weight: 560;
    font-size: 13.5px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .env {
    display: inline-flex;
    align-items: center;
    gap: 8px;
  }
  .sel {
    font: inherit;
    font-size: 12.5px;
    padding: 4px 8px;
    color: var(--text);
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
  }
  .sel:disabled {
    opacity: 0.5;
  }
  .subset {
    font-size: 11.5px;
    color: var(--warning);
    white-space: nowrap;
  }
  .muted {
    color: var(--text-faint);
    font-size: 13px;
  }
</style>
```

- [ ] **Step 2: Typecheck.** Run:
  `cd /home/splimter/projects/freelance/sauron/dashboard && npm run check`
  Expected: `0 errors`. The component has no test of its own — there is no DOM test environment — which is precisely why every decision it makes about *meaning* lives in `models/active-users.ts` instead.

---

## Task 17: `ActiveUsers.svelte`, the route and the nav item

**Files:**
- Create `dashboard/src/pages/ActiveUsers.svelte`
- Modify `dashboard/src/routes.ts` (import + route entry)
- Modify `dashboard/src/lib/components/layout/Sidebar.svelte` (the Analyze group)

**Interfaces:**
- Consumes: `getActiveUsers`, `downloadActiveUsersCsv`, `ActiveUsersParams` (Task 15); `encodeSelection`, `decodeSelection`, `validateSelection`, `describeSelection`, `selectionCount`, `defaultWindow`, `utcDayLabel` (Task 13); `AppEnvPicker` (Task 16).
- Produces: route `/active-users`.

- [ ] **Step 1: Create the page.** Create `dashboard/src/pages/ActiveUsers.svelte`:

```svelte
<script lang="ts">
  import { querystring, replace } from 'svelte-spa-router';
  import AppShell from '../lib/components/layout/AppShell.svelte';
  import AppEnvPicker from '../lib/components/AppEnvPicker.svelte';
  import Card from '../lib/components/ui/Card.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Icon from '../lib/components/ui/Icon.svelte';
  import Spinner from '../lib/components/ui/Spinner.svelte';
  import EmptyState from '../lib/components/ui/EmptyState.svelte';
  import RefreshButton from '../lib/components/ui/RefreshButton.svelte';
  import StatTiles from '../lib/components/StatTiles.svelte';
  import StatTile from '../lib/components/StatTile.svelte';
  import Sparkline from '../lib/components/Sparkline.svelte';
  import TimeSeriesChart from '../lib/components/TimeSeriesChart.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { toastStore } from '../lib/stores/toast.svelte';
  import { listEnvironments } from '../lib/api/environments';
  import { downloadActiveUsersCsv, getActiveUsers } from '../lib/api/activeUsers';
  import { errorMessage } from '../lib/api/client';
  import { compactNumber } from '../lib/utils/format';
  import {
    decodeSelection,
    defaultWindow,
    describeSelection,
    encodeSelection,
    selectionCount,
    utcDayLabel,
    validateSelection,
    type AppEnvSelection,
  } from '../lib/models/active-users';
  import type { ActiveUsersReport, AppEnvironment, SelectionView } from '../lib/models';

  const RANGES = [
    { days: 7, label: '7d' },
    { days: 30, label: '30d' },
    { days: 90, label: '90d' },
  ];

  // Hydrate from the URL ONCE, at init — not inside an effect, so this never
  // re-runs and never fights the sync effect below. House pattern from
  // `Issues.svelte`.
  const initial = new URLSearchParams($querystring ?? '');
  const initialWindow = defaultWindow(30, new Date());
  let from = $state(initial.get('from') ?? initialWindow.from);
  let to = $state(initial.get('to') ?? initialWindow.to);
  let selection = $state<AppEnvSelection>(decodeSelection(initial.getAll('selection')));

  let report = $state<ActiveUsersReport | null>(null);
  let loading = $state(false);
  let error = $state<string | null>(null);
  let refreshing = $state(false);
  let exporting = $state(false);

  // Lazily-loaded per-app enrollments. Records and Sets in `$state` are
  // REPLACED, never mutated in place — a mutation on the deep proxy is not a
  // new value and dependent effects do not re-run.
  let envsByApp = $state<Record<string, AppEnvironment[]>>({});
  let loadingEnvApps = $state<Set<string>>(new Set());

  const apps = $derived(sessionStore.apps);
  const resolvedByApp = $derived.by(() => {
    const out: Record<string, SelectionView> = {};
    for (const s of report?.selections ?? []) out[s.app_id] = s;
    return out;
  });
  const selectionValid = $derived(validateSelection(selection));
  const rangeDays = $derived(
    Math.max(1, Math.round((Date.parse(to) - Date.parse(from)) / 86_400_000)),
  );

  // Copied from `Members.svelte`: guard on both the loaded map and the
  // in-flight set, or a double click fires two identical requests.
  async function ensureEnvsLoaded(appId: string) {
    if (appId in envsByApp || loadingEnvApps.has(appId)) return;
    loadingEnvApps = new Set(loadingEnvApps).add(appId);
    try {
      const envs = await listEnvironments(appId);
      envsByApp = { ...envsByApp, [appId]: envs };
    } catch {
      envsByApp = { ...envsByApp, [appId]: [] };
    } finally {
      const next = new Set(loadingEnvApps);
      next.delete(appId);
      loadingEnvApps = next;
    }
  }

  function setRange(days: number) {
    const w = defaultWindow(days, new Date());
    from = w.from;
    to = w.to;
  }

  async function load(projectId: string, params: { from: string; to: string; selection: string[] }) {
    loading = true;
    error = null;
    try {
      report = await getActiveUsers(projectId, params);
    } catch (err) {
      error = errorMessage(err);
      report = null;
    } finally {
      loading = false;
    }
  }

  async function refresh() {
    const pid = sessionStore.currentProjectId;
    if (!pid || !selectionValid.ok) return;
    refreshing = true;
    try {
      await load(pid, { from, to, selection: encodeSelection(selection) });
    } finally {
      refreshing = false;
    }
  }

  async function exportCsv() {
    const pid = sessionStore.currentProjectId;
    const rep = report;
    if (!pid || !rep) return;
    exporting = true;
    try {
      await downloadActiveUsersCsv(
        pid,
        { from, to, selection: encodeSelection(selection) },
        rep.effective,
      );
      toastStore.success('Export downloaded.');
    } catch (err) {
      toastStore.error(errorMessage(err));
    } finally {
      exporting = false;
    }
  }

  // One effect that both writes the URL and reloads, so the shareable link and
  // the displayed numbers can never describe different requests.
  $effect(() => {
    const pid = sessionStore.currentProjectId;
    const encoded = encodeSelection(selection);
    const f = from;
    const t = to;
    if (!pid) return;
    const p = new URLSearchParams();
    p.set('from', f);
    p.set('to', t);
    for (const s of encoded) p.append('selection', s);
    void replace(`/active-users?${p.toString()}`);
    if (encoded.length === 0) {
      report = null;
      return;
    }
    void load(pid, { from: f, to: t, selection: encoded });
  });

  // Pre-load environments for anything the URL already had ticked, so a shared
  // link renders its environment names rather than raw ids.
  $effect(() => {
    for (const appId of Object.keys(selection)) void ensureEnvsLoaded(appId);
  });

  const chartData = $derived(
    (report?.series ?? []).map((p) => ({ bucket: p.day, count: p.active_total })),
  );
  const identifiedSeries = $derived((report?.series ?? []).map((p) => p.active_identified));
  const guestSeries = $derived((report?.series ?? []).map((p) => p.active_guest));
  const peak = $derived(
    report && report.series.length > 0
      ? Math.max(...report.series.map((p) => p.active_total))
      : null,
  );

  function appName(appId: string): string {
    return apps.find((a) => a.id === appId)?.name ?? appId;
  }

  function envLabel(appId: string, choice: string): string {
    if (choice === 'all') return 'All environments';
    if (choice === 'none') return 'Unattributed';
    return envsByApp[appId]?.find((e) => e.id === choice)?.name ?? choice;
  }

  function rangeLabel(): string {
    if (!report) return '';
    return `${report.effective.from.slice(0, 10)} → ${report.effective.to.slice(0, 10)}`;
  }
</script>

<AppShell requireProject requireApp={false}>
  <div class="active-users">
    <header class="head">
      <div>
        <h1 class="page-title">Active users</h1>
        <!-- The caveat that qualifies the identified number belongs beside the
             number too (see the Identified tile) — a caveat one scroll away
             gets read after the figure has already been believed. -->
        <p class="sub muted">
          Distinct people per UTC day, combined across the apps you pick. Users are matched
          across apps by the distinct ID your SDK sends — apps must use the same identifier.
        </p>
      </div>
      <div class="controls">
        <div class="ranges">
          {#each RANGES as r (r.days)}
            <button
              class="range"
              class:active={rangeDays === r.days}
              onclick={() => setRange(r.days)}
            >
              {r.label}
            </button>
          {/each}
        </div>
        <RefreshButton onclick={refresh} loading={refreshing} />
        <Button
          variant="secondary"
          onclick={exportCsv}
          loading={exporting}
          disabled={!report || !selectionValid.ok}
        >
          <Icon name="download" size={15} />
          Export CSV
        </Button>
      </div>
    </header>

    <Card title="Apps and environments">
      <AppEnvPicker
        {apps}
        {envsByApp}
        {loadingEnvApps}
        {resolvedByApp}
        value={selection}
        onchange={(next) => (selection = next)}
        onopenapp={(appId) => void ensureEnvsLoaded(appId)}
      />
      {#if !selectionValid.ok}
        <p class="hint muted">{selectionValid.reason}</p>
      {/if}
    </Card>

    {#if report?.truncated && report.truncation_reason}
      <!-- A persistent property of the displayed data, not a transient event,
           so a banner rather than a toast. On shipped defaults (TIER_HOT_DAYS
           30, sauron-tier on in both topologies) this fires for essentially
           every operator asking for 90 days. -->
      <div class="info-banner" role="status">
        <Icon name="info" size={15} />
        <span>{report.truncation_reason}</span>
      </div>
    {/if}

    {#if error}
      <div class="err-banner" role="alert">
        <Icon name="triangle-alert" size={15} />
        <span>{error}</span>
      </div>
    {/if}

    {#if loading && !report}
      <div class="center"><Spinner size={24} /></div>
    {:else if !selectionValid.ok}
      <Card>
        <EmptyState
          title="Pick an app to begin"
          description="Tick one or more apps above and choose which environment each one's numbers come from."
          icon="users"
        />
      </Card>
    {:else if report}
      {@const rep = report}
      <StatTiles min={150}>
        <StatTile
          label="Active users"
          value={rep.latest ? compactNumber(rep.latest.active_total) : '—'}
          tone="primary"
          sub={rep.latest ? rep.latest.day : 'no complete day yet'}
        />
        <StatTile
          label="Identified"
          value={rep.latest ? compactNumber(rep.latest.active_identified) : '—'}
          sub={selectionCount(selection) === 1
            ? 'matched by distinct ID'
            : 'matched across apps by raw distinct ID'}
        >
          {#snippet visual()}
            <Sparkline data={identifiedSeries} />
          {/snippet}
        </StatTile>
        <StatTile
          label="Guests"
          value={rep.latest ? compactNumber(rep.latest.active_guest) : '—'}
          sub="never merged across apps"
        >
          {#snippet visual()}
            <Sparkline data={guestSeries} />
          {/snippet}
        </StatTile>
        <StatTile
          label="Peak"
          value={peak === null ? '—' : compactNumber(peak)}
          sub={rangeLabel()}
        />
        <StatTile
          label="Apps"
          value={selectionCount(selection)}
          sub={describeSelection(selection, appName, envLabel)}
        />
      </StatTiles>

      {#if selectionCount(selection) > 1}
        <!-- Beside the figure it qualifies, not only in the page subtitle and
             the wiki. Exact arithmetic over a lossy join is still lossy: the
             three tiles always add up, and the identified half can still
             double-count one person. -->
        <p class="caveat muted">
          Two apps that name the same person differently count that person twice in
          <strong>Identified</strong>. Guests are never merged across apps at all, so a large
          guest share means most of the total was never a candidate for merging.
        </p>
      {/if}

      <Card title="Active users per day">
        {#if chartData.length === 0}
          <EmptyState
            title="No days in range"
            description="The selected window contains no complete day of data."
            icon="chart-column"
          />
        {:else}
          <!-- `utcDayLabel`, not the default: the buckets are pure UTC calendar
               days, and the default renders a parsed-as-UTC Date in the
               viewer's zone. The last bar is today and is still filling; it is
               drawn anyway (dropping it would make the range shorter than the
               picker says) while the tiles read from the last complete day. -->
          <TimeSeriesChart data={chartData} label={(b) => utcDayLabel(b)} />
        {/if}
      </Card>
    {/if}
  </div>
</AppShell>

<style>
  .active-users {
    display: flex;
    flex-direction: column;
    gap: 20px;
  }
  .head {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 16px;
    flex-wrap: wrap;
  }
  .sub {
    font-size: 13.5px;
    margin-top: 3px;
    max-width: 62ch;
  }
  .controls {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }
  .ranges {
    display: inline-flex;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    overflow: hidden;
  }
  .range {
    font: inherit;
    font-size: 12.5px;
    padding: 5px 10px;
    color: var(--text-muted);
    background: var(--surface);
    border: 0;
    cursor: pointer;
  }
  .range.active {
    color: var(--text);
    background: var(--surface-3);
  }
  .info-banner,
  .err-banner {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 10px 14px;
    font-size: 13px;
    border-radius: var(--radius);
  }
  .info-banner {
    color: var(--info);
    background: color-mix(in srgb, var(--info) 12%, transparent);
    border: 1px solid color-mix(in srgb, var(--info) 38%, transparent);
  }
  .err-banner {
    color: var(--error);
    background: var(--error-soft);
    border: 1px solid color-mix(in srgb, var(--error) 38%, transparent);
  }
  .center {
    display: grid;
    place-items: center;
    min-height: 180px;
  }
  .hint {
    margin-top: 8px;
    font-size: 12.5px;
  }
  .caveat {
    font-size: 12.5px;
    max-width: 78ch;
  }
</style>
```

- [ ] **Step 2: Register the route.** In `dashboard/src/routes.ts`, add the import next to the other Analyze pages:

```ts
import ActiveUsers from './pages/ActiveUsers.svelte';
```

and the entry in the `// Analyze` block:

```ts
  '/active-users': guarded(ActiveUsers as Component<never>),
```

- [ ] **Step 3: Add the nav item.** In `dashboard/src/lib/components/layout/Sidebar.svelte`, add to the `Analyze` group's `items` array, first:

```ts
        { href: '#/active-users', label: 'Active users', icon: 'users', match: (p) => p.startsWith('/active-users'),
          show: () => sessionStore.can('event:read') },
```

The existing Users entry matches `p.startsWith('/users')`, which does not match `/active-users`, so there is no collision.

- [ ] **Step 4: Typecheck and test.** Run:
  `cd /home/splimter/projects/freelance/sauron/dashboard && npm run check`
  then
  `cd /home/splimter/projects/freelance/sauron/dashboard && npm run test`
  Expected: `0 errors` and all tests pass. If `toastStore`'s import path is wrong, copy it verbatim from `dashboard/src/pages/Members.svelte`.

- [ ] **Step 5: Drive it in the browser.** With the API running (Task 10 Step 10) and `cd /home/splimter/projects/freelance/sauron/dashboard && npm run dev`, open `http://localhost:3000/#/active-users` and confirm, against a project with two apps that share an identified `distinct_id`:
  - ticking both apps with DIFFERENT environments makes `active_total` **less than** the sum of the two single-app totals;
  - the three tiles add up on screen (`Active users` = `Identified` + `Guests`);
  - Export CSV downloads a file whose numbers equal the on-screen tiles and whose name carries the two effective dates;
  - reloading the URL from the address bar reproduces the same view;
  - after revoking the caller's grant on one app, reloading the shareable URL shows a 403 banner that NAMES the app, and un-ticking that app recovers the page.

---

## Task 18: Browser SDK — persist the anonymous id, and ship `reset()` with it

**Files:**
- Modify `sdks/js/src/identity.ts`
- Modify `sdks/js/src/client.ts` (field at line 47, `getAnonymousId`/`ensureAnonymousId` at lines 138-146)
- Modify `sdks/js/src/index.ts` (export `reset`; `setUser(null)` calls it)
- Modify `sdks/js/src/api/product.ts` (`identify`, line 66)
- Create `sdks/js/test/anon-id.test.ts`
- Modify `sdks/js/CHANGELOG.md`

**Interfaces:**
- Produces: `ANON_ID_KEY`, `getAnonymousId()`, `resetAnonymousId()` in `identity.ts`; `SauronClient.reset()`, `SauronClient.anonymousIdWasUsed()`; `reset()` exported from `index.ts`.

- [ ] **Step 1: Write the failing tests.** Create `sdks/js/test/anon-id.test.ts`:

```ts
import { beforeEach, describe, expect, it } from 'vitest';
import {
  ANON_ID_KEY,
  getAnonymousId,
  resetAnonymousId,
  resetIdentity,
} from '../src/identity.js';

/** Minimal writable localStorage stand-in; the SDK probes before using one. */
function installStorage(): Map<string, string> {
  const map = new Map<string, string>();
  (globalThis as Record<string, unknown>).localStorage = {
    getItem: (k: string) => (map.has(k) ? (map.get(k) as string) : null),
    setItem: (k: string, v: string) => void map.set(k, v),
    removeItem: (k: string) => void map.delete(k),
  };
  return map;
}

describe('anonymous id', () => {
  let store: Map<string, string>;

  beforeEach(() => {
    store = installStorage();
    resetIdentity();
  });

  it('persists across page loads instead of being re-minted in memory', () => {
    const first = getAnonymousId();
    expect(store.get(ANON_ID_KEY)).toBe(first);
    // A fresh page load: the in-memory cache is gone, storage is not.
    resetIdentity();
    expect(getAnonymousId()).toBe(first);
  });

  it('keeps the anon_ prefix so existing data stays recognisable', () => {
    expect(getAnonymousId()).toMatch(/^anon_/);
  });

  it('resetAnonymousId mints a new one and persists it', () => {
    const first = getAnonymousId();
    const second = resetAnonymousId();
    expect(second).not.toBe(first);
    expect(store.get(ANON_ID_KEY)).toBe(second);
    expect(getAnonymousId()).toBe(second);
  });

  it('degrades to a per-process id with no writable storage', () => {
    delete (globalThis as Record<string, unknown>).localStorage;
    resetIdentity();
    const a = getAnonymousId();
    expect(a).toMatch(/^anon_/);
    expect(getAnonymousId()).toBe(a);
  });
});
```

- [ ] **Step 2: Run them and see them fail.** Run:
  `cd /home/splimter/projects/freelance/sauron/sdks/js && npm run test`
  Expected failure: `does not provide an export named 'ANON_ID_KEY'`.

- [ ] **Step 3: Persist the id in `identity.ts`.** Add to `sdks/js/src/identity.ts`:

```ts
/**
 * localStorage key holding the durable anonymous id.
 *
 * It used to live in a field on the client, re-minted on every page load, so
 * `track()` sent a new `distinct_id` each time and active users for any web app
 * counted PAGE LOADS, not people — a systematic 5-10x inflation, all of it
 * landing in the guest half of the report.
 *
 * Persisting it is a retention and consent consequence, not just an
 * implementation detail: the anon id becomes a durable first-party identifier
 * stored on the user's terminal. It is also why `reset()` exists — see
 * `SauronClient.reset`.
 */
export const ANON_ID_KEY = 'sauron.anon_id';
```

```ts
let anonymousId: string | null = null;

/** The stable anonymous id (persisted in localStorage; per-process fallback). */
export function getAnonymousId(): string {
  if (anonymousId) return anonymousId;
  const storage = webStorage('localStorage');
  if (storage) {
    try {
      const existing = storage.getItem(ANON_ID_KEY);
      if (existing) {
        anonymousId = existing;
        return anonymousId;
      }
    } catch {
      /* fall through and generate */
    }
  }
  const fresh = `anon_${uuidv4()}`;
  if (storage) {
    try {
      storage.setItem(ANON_ID_KEY, fresh);
    } catch {
      /* best effort — degrade to the in-memory value */
    }
  }
  anonymousId = fresh;
  return anonymousId;
}

/**
 * Mint and persist a fresh anonymous id.
 *
 * MUST be reachable from application code. A persisted anon id plus
 * `process_identify`'s `identities(app_id, alias_id, distinct_id)` insert means
 * one `identify()` permanently binds this browser profile to a named user
 * server-side — so on a kiosk or a shared machine, person B's anonymous
 * activity would be aliased to person A's account, forever, with no escape
 * hatch.
 */
export function resetAnonymousId(): string {
  anonymousId = null;
  const storage = webStorage('localStorage');
  if (storage) {
    try {
      storage.removeItem(ANON_ID_KEY);
    } catch {
      /* best effort */
    }
  }
  return getAnonymousId();
}
```

and extend `resetIdentity`:

```ts
/** Drop the in-memory memoization (used by tests and teardown). */
export function resetIdentity(): void {
  deviceId = null;
  sessionId = null;
  anonymousId = null;
}
```

- [ ] **Step 4: Run the identity tests and see them pass.** Run:
  `cd /home/splimter/projects/freelance/sauron/sdks/js && npm run test`
  Expected: the four `anonymous id` tests pass; every pre-existing test still passes.

- [ ] **Step 5: Route the client through it and add `reset()`.** In `sdks/js/src/client.ts`, delete the `private anonymousId: string | null = null;` field and replace `getAnonymousId`/`ensureAnonymousId` with:

```ts
  /**
   * Whether the anonymous id has actually been USED as a `distinct_id` in this
   * browser session.
   *
   * A persisted id that has never been observed anonymously must not create a
   * permanent `identities` alias row on the server: aliasing is a durable
   * server-side binding of this browser profile to a named user, and an
   * identify() on a first-ever page load has no anonymous history to link.
   */
  private anonUsed = false;

  /** The current distinct id: the user id when identified, else an anon id. */
  getDistinctId(): string | null {
    const user = this.scope.getUser();
    if (user.id) return user.id;
    this.anonUsed = true;
    return getAnonymousId();
  }

  /** The anonymous id, or null when it was never actually used as an identity. */
  getAnonymousId(): string | null {
    return this.anonUsed ? getAnonymousId() : null;
  }

  /**
   * Forget the current person: clear the scope user and mint a fresh anonymous
   * id.
   *
   * MUST BE CALLED ON LOGOUT. Without it, the next anonymous visitor on this
   * browser reuses the persisted anon id, and a later identify() aliases their
   * activity to the previous account server-side, permanently.
   */
  reset(): void {
    this.scope.setUser(null);
    resetAnonymousId();
    this.anonUsed = false;
  }
```

and update the import at the top of `client.ts` to pull both helpers from `identity.js`:

```ts
import { getAnonymousId, resetAnonymousId } from './identity.js';
```

(merge with the existing `./identity.js` import if there is one; remove the now-unused `uuidv4` import only if nothing else in the file uses it).

- [ ] **Step 6: Export `reset` and wire `setUser(null)`.** In `sdks/js/src/index.ts`, replace `setUser` and add `reset`:

```ts
/**
 * Set (or clear, with `null`) the current user.
 *
 * `setUser(null)` is a logout, so it also rotates the anonymous id — otherwise
 * the next anonymous visitor on this browser inherits the previous person's
 * durable id and a later identify() aliases them together server-side.
 */
export function setUser(user: UserInput): void {
  if (user === null) {
    getClient()?.reset();
    return;
  }
  getClient()?.getScope().setUser(user);
}

/**
 * Forget the current person: clears the scope user and mints a fresh anonymous
 * id. Call this on logout.
 */
export function reset(): void {
  getClient()?.reset();
}
```

and add `reset,` to the `Sauron` facade object.

- [ ] **Step 7: Gate the alias on `identify`.** In `sdks/js/src/api/product.ts`, the `identify` function already reads `client.getAnonymousId()`; with Step 5 that now returns `null` unless the anon id was actually used, so `anonymous_id` rides along only when there is real anonymous history to link. Add the comment so the next reader does not "simplify" it back:

```ts
export function identify(id: string, traits: Record<string, unknown> = {}): void {
  const client = getClient();
  if (!client) return;
  // `null` unless the anon id was actually used as a distinct_id in this
  // browser session. `process_identify` inserts a permanent
  // `identities(app_id, alias_id, distinct_id)` row for any non-empty
  // anonymous_id, and that row is now a LIVE signal (the 000038 backfill reads
  // it), so a speculative alias is a durable server-side mis-merge.
  const anonymousId = client.getAnonymousId();
  client.getScope().setUser({ id, traits });
  const item: IdentifyItem = {
    type: 'identify',
    distinct_id: id,
    anonymous_id: anonymousId,
    traits: traits ?? {},
  };
  client.captureItem(item);
}
```

- [ ] **Step 8: Run the SDK suite and typecheck.** Run:
  `cd /home/splimter/projects/freelance/sauron/sdks/js && npm run test`
  then
  `cd /home/splimter/projects/freelance/sauron/sdks/js && npm run typecheck`
  Expected: all tests pass, no type errors. `test/envelope.test.ts` holds the golden envelope both SDKs must emit — if it now fails on `anonymous_id`, the golden fixture's identify item was relying on a speculative alias and must be updated to reflect the new contract, with a note in the test.

- [ ] **Step 9: Write the release note.** Add to the top of `sdks/js/CHANGELOG.md` under a new version heading:

```md
### Changed

- The anonymous id is now persisted in `localStorage` under `sauron.anon_id`
  instead of being re-minted in memory on every page load. **Every web app's
  reported active-user count drops sharply and permanently on the day this is
  adopted** — the old behaviour counted page loads, not people (a 5-10x
  inflation, all of it in the "guest" half of the Active Users report). The
  drop is a data artifact, not a regression.
- The anonymous id is a durable first-party identifier stored on the user's
  terminal. That is a retention and consent consequence, not just an
  implementation detail.

### Added

- `reset()` — clears the scope user and mints a fresh anonymous id.
  **Call it on logout.** `setUser(null)` now calls it for you. Without it, the
  next anonymous visitor on a shared browser reuses the persisted id and a
  later `identify()` aliases their activity to the previous account,
  server-side, permanently.
- `anonymous_id` is sent on the identify item only when the anonymous id was
  actually used as a `distinct_id` in this browser session.
```

---

## Task 19: Migrations 000039 and 000040 — measure first, then substitute

**Files:**
- Create `backend/migrations/2026-08-01-000039_analytics_active_user_index/{up,down}.sql`
- Create `backend/migrations/2026-08-01-000040_error_active_user_index/{up,down}.sql`

**Interfaces:**
- Consumes: `active_users_combined` (Task 5) — the statement being measured.
- Produces: indexes `analytics_events_app_env_time_users_idx`, `error_events_app_env_time_users_idx`; removes `analytics_events_app_env_time_idx`, `error_events_app_env_time_idx`.

- [ ] **Step 1: Measure BEFORE changing anything.** This is the only index migration in the repo proposed on analogy rather than measurement, and it must not stay that way. Against a real dataset, run the exact statement `active_users_combined` builds for one `(app, One(env))` selection over 30 days. Substituting the four binds by hand:

```sql
EXPLAIN (ANALYZE, BUFFERS)
WITH signal AS (
  SELECT app_id, occurred_at, distinct_id FROM analytics_events
   WHERE app_id = '<APP_UUID>' AND occurred_at >= '2026-04-08T00:00:00Z' AND occurred_at < '2026-05-08T00:00:00Z'
     AND analytics_events.environment_id = '<ENV_UUID>'
     AND distinct_id IS NOT NULL AND distinct_id <> ''
  UNION ALL
  SELECT app_id, occurred_at, distinct_id FROM error_events
   WHERE app_id = '<APP_UUID>' AND occurred_at >= '2026-04-08T00:00:00Z' AND occurred_at < '2026-05-08T00:00:00Z'
     AND error_events.environment_id = '<ENV_UUID>'
     AND distinct_id IS NOT NULL AND distinct_id <> ''
),
days AS (SELECT DISTINCT app_id, distinct_id, (occurred_at AT TIME ZONE 'UTC')::date AS day FROM signal),
keyed AS (
  SELECT DISTINCT
         CASE WHEN eu.distinct_id IS NOT NULL THEN 'u:' || d.distinct_id
              ELSE 'a:' || d.app_id::text || ':' || d.distinct_id END AS identity_key,
         (eu.distinct_id IS NOT NULL) AS identified, d.day
    FROM days d
    LEFT JOIN event_users eu ON eu.app_id = d.app_id AND eu.distinct_id = d.distinct_id
     AND eu.identified_at IS NOT NULL
)
SELECT day, count(*) FROM keyed GROUP BY day ORDER BY day;
```

Record `Heap Fetches` and the shared-buffer counts for both the `analytics_events` and the `error_events` scans.

**Ship 000039 and/or 000040 only if heap fetches actually dominate for that table.** Note specifically that `analytics_distinct_idx (app_id, distinct_id, occurred_at DESC)` already covers the `EnvFilter::All` shape index-only today, which weakens the case for the analytics half. If heap fetches do not dominate, stop here, record the measurement in the slice report, and skip Steps 2-5 — an unnecessary synchronous index rebuild across every child partition of the two largest tables is a real outage risk with no upside.

- [ ] **Step 2: Write 000039.** Create `backend/migrations/2026-08-01-000039_analytics_active_user_index/up.sql`:

```sql
-- Substitute `analytics_events_app_env_time_idx` (migration 25) with a variant
-- carrying `distinct_id` in its INCLUDE payload.
--
-- The dominant scan `active_users_combined` issues is
-- `WHERE app_id AND environment_id AND occurred_at BETWEEN`, projecting only
-- `distinct_id` and `occurred_at`. The existing index gives a perfect index
-- cond but carries no payload, so every matching row costs a heap fetch of a
-- ~1-2 KB tuple.
--
-- This ADDS ZERO INDEXES. It widens one existing btree's leaves by one short
-- text column — the same class of change migration 28 measured at 1-6% on
-- `error_events` — and `INCLUDE` on a partitioned parent is already proven here
-- (migrations 28 and 31). New name, per the rule that an index name an earlier
-- migration took is never reused; replace-don't-accumulate, per the rule
-- migrations 28 and 31 each invoked.
--
-- OPERATIONAL PRECONDITION, not merely "expect read latency":
-- **STOP sauron-ingest OR DRAIN THE STREAM BEFORE RUNNING THIS.**
-- `analytics_events` is a partitioned parent; DROP INDEX + CREATE INDEX apply
-- synchronously across every child inside this migration's single transaction
-- (CONCURRENTLY is unavailable inside one), holding locks that block every
-- INSERT. With TIER_GRANULARITY=day and TIER_PARTITION_AHEAD=7 that is ~37
-- synchronous child builds. While the pipeline is blocked on the lock the
-- Redis stream keeps growing, and `xadd_job(&payload, 1_000_000)` issues
-- `XADD … MAXLEN ~ 1000000`, which trims by ID regardless of the consumer
-- group's pending list — the oldest, still-undelivered entries are trimmed
-- away. That is PERMANENT SILENT EVENT LOSS, not backpressure.
--
-- Split from 000040 deliberately: doing both partitioned parents in one
-- transaction would block both ingest write paths at once. Run them in
-- separate windows, with a pause between.
--
-- MUST RUN BEFORE RESTARTING sauron-api (packaging/rpm/SETUP.md §11) — not
-- for correctness (the query works without it) but because the pre-substitution
-- plan is what the measurement in the slice report was taken against.
DROP INDEX IF EXISTS analytics_events_app_env_time_idx;
CREATE INDEX analytics_events_app_env_time_users_idx
    ON analytics_events (app_id, environment_id, occurred_at DESC) INCLUDE (distinct_id);
```

and `down.sql`:

```sql
-- Recreates migration 25's definition verbatim. Carries the SAME warning as
-- up.sql: a rollback is also a synchronous rebuild across every child
-- partition, so stop sauron-ingest or drain the stream first.
DROP INDEX IF EXISTS analytics_events_app_env_time_users_idx;
CREATE INDEX analytics_events_app_env_time_idx ON analytics_events (app_id, environment_id, occurred_at DESC);
```

- [ ] **Step 3: Write 000040.** Create `backend/migrations/2026-08-01-000040_error_active_user_index/up.sql`:

```sql
-- Substitute `error_events_app_env_time_idx` (migration 25) with a variant
-- carrying `distinct_id` in its INCLUDE payload.
--
-- Both `*_app_env_time_idx` indexes come from ONE migration —
-- `2026-07-27-000025_search_indexes/up.sql` lines 33 and 36 — so this file and
-- 000039 name the same ancestor. Do not "correct" this to 27.
--
-- The dominant scan `active_users_combined` issues is
-- `WHERE app_id AND environment_id AND occurred_at BETWEEN`, projecting only
-- `distinct_id` and `occurred_at`. The existing index gives a perfect index
-- cond but carries no payload, so every matching row costs a heap fetch of a
-- ~1-2 KB tuple.
--
-- This ADDS ZERO INDEXES. It widens one existing btree's leaves by one short
-- text column — the same class of change migration 28 measured at 1-6% on
-- `error_events` — and `INCLUDE` on a partitioned parent is already proven here
-- (migrations 28 and 31). New name, per the rule that an index name an earlier
-- migration took is never reused; replace-don't-accumulate, per the rule
-- migrations 28 and 31 each invoked.
--
-- OPERATIONAL PRECONDITION, not merely "expect read latency":
-- **STOP sauron-ingest OR DRAIN THE STREAM BEFORE RUNNING THIS.**
-- `error_events` is a partitioned parent; DROP INDEX + CREATE INDEX apply
-- synchronously across every child inside this migration's single transaction
-- (CONCURRENTLY is unavailable inside one), holding locks that block every
-- INSERT. With TIER_GRANULARITY=day and TIER_PARTITION_AHEAD=7 that is ~37
-- synchronous child builds. While the pipeline is blocked on the lock the
-- Redis stream keeps growing, and `xadd_job(&payload, 1_000_000)` issues
-- `XADD … MAXLEN ~ 1000000`, which trims by ID regardless of the consumer
-- group's pending list — the oldest, still-undelivered entries are trimmed
-- away. That is PERMANENT SILENT EVENT LOSS, not backpressure.
--
-- Split from 000039 deliberately: doing both partitioned parents in one
-- transaction would block both ingest write paths at once. Run them in
-- separate windows, with a pause between.
--
-- MUST RUN BEFORE RESTARTING sauron-api (packaging/rpm/SETUP.md §11) — not
-- for correctness (the query works without it) but because the pre-substitution
-- plan is what the measurement in the slice report was taken against.
DROP INDEX IF EXISTS error_events_app_env_time_idx;
CREATE INDEX error_events_app_env_time_users_idx
    ON error_events (app_id, environment_id, occurred_at DESC) INCLUDE (distinct_id);
```

and `down.sql`:

```sql
-- Recreates migration 25's definition verbatim (`error_events_app_env_time_idx`
-- is created in `2026-07-27-000025_search_indexes/up.sql` line 33, the same
-- migration that creates the analytics twin). Carries the SAME warning as
-- up.sql: a rollback is also a synchronous rebuild across every child
-- partition, so stop sauron-ingest or drain the stream first.
DROP INDEX IF EXISTS error_events_app_env_time_users_idx;
CREATE INDEX error_events_app_env_time_idx ON error_events (app_id, environment_id, occurred_at DESC);
```

- [ ] **Step 4: Apply and re-measure.** Run:
  `DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron cargo run --bin sauron-migrate`
  then re-run the Step 1 `EXPLAIN (ANALYZE, BUFFERS)`. Record both the before and the after numbers in the slice report — the evidence standard migrations 25, 28 and 31 each set. If heap fetches did not fall, the change did not do what it was for; say so rather than leaving it in silently. Note that on append-only partitioned tables the newest partition — the one an active-users query touches most — is the least likely to be all-visible, so run `VACUUM ANALYZE` on the relevant children and record both readings.

- [ ] **Step 5: Prove a fresh database still migrates end to end.** Run:
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron cargo test -p sauron-db harness_seeds_two_isolated_environments`
  Expected: pass. `TestDb::setup()` creates and migrates a brand-new database, so this is the cheapest full-chain migration check available.

---

## Task 20: Documentation — wiki page and the upgrade runbook rows

**Files:**
- Create `wiki/Active-Users.md`
- Modify `wiki/_Sidebar.md` (Reference section)
- Modify `wiki/Home.md` (the reference bullet list around line 78)
- Modify `wiki/Browser-SDK.md` (the API table at line 62 and the "Identify a user" section at line 102)
- Modify `packaging/rpm/SETUP.md` (§11)

**Scope note — the PII-mask warning ships in ONE of its two required places here.**
Design §1.3 asks for the same sentence in `wiki/Active-Users.md` **and** in "the
mask confirmation dialog's *what this does not reach* panel". Step 1 below writes
the wiki half. There is no mask confirmation dialog in `dashboard/src/` today —
the PII inspector and its dialog are slice **S5** — so the dialog half is
deferred to S5 with this hand-off: **S5's mask confirmation dialog must carry the
"Two things that silently change these numbers" first paragraph from
`wiki/Active-Users.md`, verbatim, in its "what this does not reach" panel, and
link to `Active-Users.md`.** Do not invent a dialog in this slice to satisfy the
design; a confirmation dialog with one warning and no mask behind it is worse
than the hand-off.

**Interfaces:** none — documentation only.

- [ ] **Step 1: Write the wiki page.** Create `wiki/Active-Users.md`:

```md
# Active Users

**Active users** answers "how many distinct people used these apps", per UTC
calendar day, across as many apps in one project as you pick — each with its own
environment.

Open it from **Analyze → Active users**. It needs the `event:read` permission.

## What the three numbers mean

Every day carries three figures, and they always add up:

| Figure | Meaning |
|---|---|
| **Active users** | Distinct identities active that day, across every selected app+environment |
| **Identified** | The part of that total your app told us is a real person — via `identify()`, or an event whose `context.user.id` equals the `distinct_id` it was sent with |
| **Guests** | Everyone else: anonymous, SDK-minted ids |

`Active users = Identified + Guests` is exact, by construction.

## How people are matched across apps

**Identified users merge across apps by exact string equality on the distinct
ID your SDK sends.** If your web app calls someone `u-42` and your mobile app
calls them `auth0|abc`, they count as **two people**, not one. There is no
server-side fix for this — make your apps send the same identifier.

**Guests never merge across apps at all.** An anonymous id in app A and the
identical string in app B are two different guests, deliberately: the number for
{A, B} must not change depending on whether you also tick C.

A large guest share therefore tells you how much of your total was never a
candidate for merging in the first place.

## The window

- Days are **UTC calendar days**, everywhere: the chart, the CSV and the file
  name. There is no per-user or per-organisation display timezone.
- The maximum window is **92 days**, and `apps × days` may not exceed 1200.
- **Your visible window is usually shorter than you asked for.** Data older than
  `TIER_HOT_DAYS` (default 30) is moved to cold Parquet storage by
  `sauron-tier`, which runs by default in both shipped topologies, so a 90-day
  request typically returns about 30 days. When that happens the page shows a
  banner naming the date the report actually starts from, and the CSV's file
  name carries the real range. Raising `TIER_HOT_DAYS` buys a longer window at
  the cost of hot Postgres storage.
- The last bar on the chart is **today**, which is still filling. The headline
  tiles read from the last **complete** day, so they never dip at midnight. A
  window containing only today shows an em-dash, not `0` — zero active users is
  a real answer and this is not it.

## "2 of 5 environments"

If your grants reach only some of an app's environments, selecting "All
environments" for that app quietly means "all the ones you can read". The picker
says `2 of 5 environments` when that happens, because the number is genuinely
not comparable with a colleague's who has app-wide access. (An app-wide reader
also sees rows that belong to no environment at all; a partial reader does not.)

## Export

**Export CSV** downloads exactly what is on screen: one row per displayed day,
columns `day,active_total,active_identified,active_guest`. The file name carries
the project id and the effective date range, so a download in a shared folder
still says what it is. Both halves are exported, not just the total — a
spreadsheet is where someone re-derives a figure months later with no page
around it to carry the matching caveat above.

## Two things that silently change these numbers

**A PII mask on an identity-bearing key dismantles cross-app matching.** The
mask enforcer runs before identification, so once `context.user.id` — or
whatever key your app uses as its `distinct_id`; an email address is both a
common choice and exactly the kind of value a PII policy flags — is masked, no
future person can ever be marked identified through it. Nobody already
identified loses the flag, so nothing moves on the day the mask lands: instead
**Identified** plateaus and then decays as the existing population churns, while
**Guests** climbs to meet it. Nothing labels the cause and nothing can
reconstruct it afterwards. Decide before you apply the mask, not after.

**A skipped migration loses the signal permanently.** RPM upgrades do not re-run
`sauron-migrate` (see the upgrade section of the RPM setup guide). Until
migration `000038` is applied, this page returns `503
schema_migration_required` and the ingest worker records no identification at
all — and that gap **cannot** be backfilled later, because the backfill can only
see stored traits and alias rows. Everyone first active during an un-migrated
window is filed under **Guests** forever.

## Known limitation

Reported numbers for a browser app drop sharply and permanently the day you
adopt browser SDK ≥ the release that persists the anonymous id. Before that
release the SDK re-minted an anonymous id on every page load, so the count was
page loads rather than people — typically a 5-10x inflation, all of it in
**Guests**. The drop is the fix landing, not a regression.
```

- [ ] **Step 2: Link it from the sidebar.** In `wiki/_Sidebar.md`, add under **Reference**, after the Dashboard entry:

```md
- [Active Users](Active-Users.md)
```

- [ ] **Step 3: Link it from Home.** In `wiki/Home.md`, add a bullet to the reference list beside the Dashboard and Search entries:

```md
- **[Active Users](Active-Users.md)** — combined daily active users across
  several apps, the identified/guest split and what it can and cannot merge,
  and the CSV export.
```

- [ ] **Step 4: Document `reset()` in the browser SDK wiki.** Design §9.2 requires the new API be documented "as MUST-CALL-ON-LOGOUT", and `wiki/Browser-SDK.md` — the page a web developer actually reads — mentions neither `reset` nor the durable id today. In `wiki/Browser-SDK.md`, add a row to the API table (line 62) immediately after the `setUser` row:

```md
| `reset` | `reset(): void` — **call on logout.** Clears the scope user and mints a fresh anonymous id |
```

and append this section immediately after **Identify a user** (which ends at line 111, before `### Tags, contexts & extra`):

```md
### Reset on logout — MUST CALL

    Sauron.reset();        // on logout
    Sauron.setUser(null);  // equivalent: setUser(null) calls reset() for you

The anonymous id is persisted in `localStorage` under `sauron.anon_id` and
survives page loads, tabs and browser restarts. That is what makes the Active
Users report count people rather than page loads — and it is also a durable
first-party identifier stored on the user's terminal, so it is a retention and
consent question for your privacy notice, not just an implementation detail.

Because it is durable, **not calling `reset()` on logout aliases the next
person to the last one**. `identify()` sends the current anonymous id as
`anonymous_id`, and the server records that alias permanently. On a shared or
kiosk browser, the next anonymous visitor reuses the stored `sauron.anon_id`,
and their activity is merged into the previous account server-side, forever.
There is no server-side undo.

`reset()` does NOT clear the device id (`sauron.device_id`) — that identifies
the browser installation, not the person.

See [Active Users](Active-Users.md) for what the identified/guest split means
once these ids reach the backend.
```

(The two `Sauron.…` lines are indented four spaces above only so this plan's own
fences nest; in `Browser-SDK.md` itself write them inside a plain
triple-backtick `ts` block, matching every other example on that page.)

- [ ] **Step 5: Append the three migration rows.** In `packaging/rpm/SETUP.md`, find §11 "Upgrading" (created by slice S0). If it does not exist yet, create it first:

```md
## 11. Upgrading

`dnf upgrade` installs new binaries but does **not** run database migrations.
Run them yourself, with the services stopped, on every upgrade:

    systemctl stop sauron-api sauron-ingest
    systemctl start sauron-migrate
    systemctl start sauron-api sauron-ingest

| Migration | What breaks if it is skipped |
|---|---|
```

(The three `systemctl` lines are indented four spaces above only so this plan's
own fences nest; in `SETUP.md` itself write them inside a plain triple-backtick
block, matching how §5 and §6 already render their shell snippets.)

Then append three rows to that table:

```md
| `000038_event_users_identified` | `GET /v1/projects/{id}/active-users` returns `503 schema_migration_required`, and `sauron-ingest` records no identification at all. **Not recoverable later** — the backfill only sees stored traits and alias rows, so everyone first active during the gap is filed as a guest forever. Needs a maintenance window: the partial index blocks `event_users` writes while it builds, and that table holds roughly one row per page load per browser app. |
| `000039_analytics_active_user_index` | Nothing breaks; the active-users query falls back to heap fetches. **Stop `sauron-ingest` or drain the Redis stream before running.** It drops and rebuilds an index on a partitioned parent inside one transaction, blocking every `analytics_events` INSERT; the stream is trimmed with `XADD MAXLEN ~1000000` regardless of pending deliveries, so a long enough window silently discards undelivered events. |
| `000040_error_active_user_index` | Same as 000039, for `error_events`. **Run it in a separate window from 000039** — together they block both ingest write paths at once. |
```

- [ ] **Step 6: Check the links resolve.** Run:
  `cd /home/splimter/projects/freelance/sauron && grep -rn "Active-Users.md" wiki/ && ls wiki/Active-Users.md && grep -n "reset()" wiki/Browser-SDK.md && grep -n "000038\|000039\|000040" packaging/rpm/SETUP.md`
  Expected: the sidebar, Home and `Browser-SDK.md` all reference the file, the file exists, `Browser-SDK.md` documents `reset()` as MUST CALL, and all three migration rows are present in SETUP.md.

---

## Final verification

- [ ] **Step 1: Full backend gate.** Run, in order:
  `cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all --check`
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings`
  `cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron TEST_REDIS_URL=redis://172.20.0.3:6379 cargo test --workspace`
  Expected: clean format, no warnings, all tests pass with no "skipping" lines.

- [ ] **Step 2: Full frontend gate.** Run:
  `cd /home/splimter/projects/freelance/sauron/dashboard && npm run check && npm run test`
  `cd /home/splimter/projects/freelance/sauron/sdks/js && npm run typecheck && npm run test`
  Expected: `0 errors`, all tests pass.

- [ ] **Step 3: Verify the tier clamp against a deployment that has actually run `sauron-tier`.** Every Postgres test seeds a fresh database where no watermark exists and the clamp never fires, so **no test in this plan can catch a break here.** On a deployment where `sauron-tier` has run at least once, request a 90-day window and confirm: the banner appears, `truncation_reason` names the real watermark date, and `effective.from` matches the first bar on the chart.

- [ ] **Step 4: Verify the identification write path end to end.** Point a browser SDK at a dev app, call `identify('u-42', {plan: 'pro'})`, then confirm with
  `psql postgres://sauron:sauron@172.20.0.2:5432/sauron -c "SELECT distinct_id, identified_source FROM event_users WHERE distinct_id = 'u-42'"`
  that `identified_source` is `identify`. Then send a plain `track()` from an anonymous session and confirm the new row's `identified_source` is NULL.

---

## Notes for the reviewer

- **The `identified_source` column is not decoration.** It is the only thing that
  makes a poisoned `context_user` cohort repairable without also clearing real
  `identify()` rows. The repair statement is written into 000038's prose header.
- **The cache key must use the RESOLVED filter, never the requested token.** A
  deviation is a cross-tenant data leak, not a staleness bug: the cached entry
  holds the whole series plus every app name.
- **`env_ids_for_apps` must be folded into a per-app map before it reaches
  `resolve_env_filter`.** Passing the flat vector breaks two authorization
  decisions in the granting direction.
- **`let _permit`, never `let _`,** on the semaphore: the latter drops the permit
  immediately and the gate silently becomes a no-op.
- Three migrations here need real maintenance windows and none should share a
  release with anything else time-sensitive.
