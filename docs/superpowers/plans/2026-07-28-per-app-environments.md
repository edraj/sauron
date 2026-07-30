# Per-App Environments (Slice 1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make environments a real, admin-managed entity that owns the ingest credential, so an event's environment is proven by the key it arrived with rather than asserted by a client string.

**Architecture:** The `environments` table gains `public_key`, `ingest_enabled`, `is_default` and `retired_at`, and `apps.public_key` is dropped. The ingest edge resolves `key → environment → app → project → org` in one lookup, so the envelope's `environment` field disappears from the wire entirely. Environments get their own `env:*` permission family; scope stays `(org, project, app)` — env-as-scope is Slice 3.

**Tech Stack:** Rust (axum, diesel-async, tokio), Postgres, Redis, Svelte 5 (runes) + svelte-spa-router, five client SDKs (TypeScript browser, TypeScript Node, Python, Dart/Flutter, C#).

**Spec:** `docs/superpowers/specs/2026-07-28-per-app-environments-design.md`

## Global Constraints

- **Never commit.** This repository's standing rule is that the agent does not run `git commit` or create branches. Each task ends with a verification gate instead. Leave changes in the working tree.
- **Backend gate after every backend task:** `cd backend && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
- **DuckDB linking:** `cargo` commands need `DUCKDB_LIB_DIR` and `LD_LIBRARY_PATH` pointing at a prebuilt `libduckdb.so` (see `packaging/rpm/fetch-libduckdb.sh`). Never pass `--all-features` — it enables `sauron-tier`'s `bundled` feature and recompiles DuckDB from source.
- **No DB integration-test harness exists.** All 66 backend test modules are in-file `#[cfg(test)] mod` unit tests over pure functions; none touch Postgres or Redis. Follow that pattern: extract pure logic into free functions and test those. Database behaviour is verified in Task 11 against a live stack, not in `cargo test`.
- **Public key format:** `pk_` + 32 lowercase hex chars, produced by `sauron_core::ids::public_key()` (`backend/crates/sauron-core/src/ids.rs:19`).
- **SDK versions:** all five SDKs go to exactly `1.1.0`.
- **Permission count:** `perm::ALL` ends this slice at **27** entries (23 − `app:rotate_key` + 5 `env:*`).
- **Migration numbering:** `backend/migrations/YYYY-MM-DD-NNNNNN_snake_case/` with a globally monotonic 6-digit ordinal. Last used is `2026-07-27-000025_search_indexes`, so this slice uses `2026-07-28-000026_env_keys`.

---

## File Structure

**Created:**
- `backend/migrations/2026-07-28-000026_env_keys/up.sql` — schema, backfill, permission strip
- `backend/migrations/2026-07-28-000026_env_keys/down.sql` — reverse
- `backend/bins/sauron-api/src/routes/environments.rs` — env CRUD handlers + pure validators
- `dashboard/src/lib/api/environments.ts` — env API client
- `dashboard/src/lib/components/settings/EnvironmentsCard.svelte` — the management surface

**Modified (backend):**
- `backend/crates/sauron-db/src/schema.rs` — `environments` + `apps` table blocks
- `backend/crates/sauron-db/src/models.rs` — `Environment`, `NewEnvironment`, `App`, `NewApp`
- `backend/crates/sauron-db/src/repo.rs` — env CRUD, `find_env_by_public_key`, `env_ancestry`; delete `upsert_environment`, `rotate_app_key`, `find_app_by_public_key`
- `backend/crates/sauron-auth/src/rbac.rs` — `perm` module, presets, invariant tests
- `backend/bins/sauron-api/src/routes/apps.rs` — drop `rotate_key`, move env listing out
- `backend/bins/sauron-api/src/routes/projects.rs` — `create_app` seeds `dev` and no longer mints an app key
- `backend/bins/sauron-api/src/routes/mod.rs` — register the new module
- `backend/bins/sauron-api/src/main.rs` — router
- `backend/bins/sauron-ingest/src/main.rs` — `resolve_app` → `resolve_env`
- `backend/crates/sauron-redis/src/lib.rs` — cache key prefix bump
- `backend/crates/sauron-core/src/envelope.rs` — drop `EnvelopeHeader.environment`, retype `IngestJob`
- `backend/crates/sauron-pipeline/src/process.rs` — consume `job.environment_id`

**Modified (frontend/SDK/docs):** enumerated in Tasks 6–10.

---

## Task 1: Migration

**Files:**
- Create: `backend/migrations/2026-07-28-000026_env_keys/up.sql`
- Create: `backend/migrations/2026-07-28-000026_env_keys/down.sql`

**Interfaces:**
- Consumes: nothing.
- Produces: the `environments` columns `public_key TEXT NOT NULL UNIQUE`, `ingest_enabled BOOLEAN NOT NULL`, `is_default BOOLEAN NOT NULL`, `retired_at TIMESTAMPTZ NULL`, `updated_at TIMESTAMPTZ NOT NULL`; removal of `apps.public_key`; indexes `environments_default_key` and `environments_app_name_active_key`. Every later backend task depends on this shape.

- [ ] **Step 1: Write `up.sql`**

Create `backend/migrations/2026-07-28-000026_env_keys/up.sql`:

```sql
-- Environments become the ingest credential holder.
--
-- Until now the environment name arrived as a free string in the envelope header
-- and was upserted on first sight, so it was asserted by whoever held the app's
-- single key. Moving the key onto the environment makes the value provable: the
-- environment is whichever row the presented key belongs to, and there is no way
-- for a client to claim a different one.
--
-- Ordering matters throughout. Columns are added nullable, backfilled, and only
-- then constrained, because NOT NULL + UNIQUE cannot be declared against rows
-- that do not yet have values.

-- 1. New columns, all nullable or defaulted so the ALTER succeeds on live data.
ALTER TABLE environments
  ADD COLUMN public_key     TEXT,
  ADD COLUMN ingest_enabled BOOLEAN NOT NULL DEFAULT true,
  ADD COLUMN is_default     BOOLEAN NOT NULL DEFAULT false,
  ADD COLUMN retired_at     TIMESTAMPTZ,
  ADD COLUMN updated_at     TIMESTAMPTZ NOT NULL DEFAULT now();

-- 2. Every app must own at least one environment, because the app no longer has
--    a key of its own and would otherwise become unreachable by any SDK.
INSERT INTO environments (app_id, name)
SELECT a.id, 'dev'
FROM apps a
WHERE NOT EXISTS (SELECT 1 FROM environments e WHERE e.app_id = a.id);

-- 3. Backfill keys. `gen_random_uuid()` is built into Postgres 13+ (already relied
--    on for every `id` default in this schema), and stripping its dashes yields
--    exactly the `pk_` + 32-hex shape `ids::public_key()` produces — so no pgcrypto
--    dependency is introduced. 122 bits of entropy rather than 128; acceptable for
--    a one-time backfill, and every key minted afterwards uses `getrandom`.
UPDATE environments
SET public_key = 'pk_' || replace(gen_random_uuid()::text, '-', '')
WHERE public_key IS NULL;

ALTER TABLE environments ALTER COLUMN public_key SET NOT NULL;
ALTER TABLE environments ADD CONSTRAINT environments_public_key_key UNIQUE (public_key);

-- 4. Exactly one default per app, chosen deterministically: `production` if it
--    exists, else `dev`, else alphabetically first. `production` wins because
--    apps were seeded with it at creation and are actively reporting into it —
--    defaulting them to a lexicographically-earlier environment would silently
--    change which one the dashboard treats as primary.
UPDATE environments e
SET is_default = true
FROM (
    SELECT DISTINCT ON (app_id) id
    FROM environments
    ORDER BY app_id,
             (name = 'production') DESC,
             (name = 'dev') DESC,
             name ASC
) pick
WHERE e.id = pick.id;

-- Predicate includes `retired_at IS NULL` so a retired row can never occupy an app's
-- only default slot. Without it, retiring a default (however that came about) would
-- make promoting any other environment fail with a duplicate-key error, leaving the app
-- with a default that ingest can never reach — ingest filters `retired_at IS NULL`.
CREATE UNIQUE INDEX environments_default_key
    ON environments (app_id) WHERE is_default AND retired_at IS NULL;

-- 5. Name uniqueness applies only to live environments, so retiring `staging`
--    does not block creating a fresh `staging` later. The original constraint was
--    declared as UNIQUE (project_id, name) in the init migration and the column
--    was later renamed to app_id; renaming a column does NOT rename the
--    constraint, so the stored name may be either. Drop both spellings.
ALTER TABLE environments DROP CONSTRAINT IF EXISTS environments_project_id_name_key;
ALTER TABLE environments DROP CONSTRAINT IF EXISTS environments_app_id_name_key;
CREATE UNIQUE INDEX environments_app_name_active_key
    ON environments (app_id, name) WHERE retired_at IS NULL;

-- 6. The app-level credential is gone. Ingest resolves through the environment.
ALTER TABLE apps DROP COLUMN public_key;

-- 7. `app:rotate_key` no longer exists as a permission, because apps no longer
--    hold a key to rotate. Left in a custom role's JSONB it would be worse than
--    dead weight: `check_no_escalation` requires the granting caller to hold
--    every permission in the role, and nobody can hold one that is no longer in
--    `perm::ALL` — so the role would become permanently ungrantable. System
--    (preset) roles are re-synced from code at every API boot and need no fixup.
UPDATE roles
SET permissions = permissions - 'app:rotate_key'
WHERE permissions @> '["app:rotate_key"]'::jsonb;
```

- [ ] **Step 2: Write `down.sql`**

Create `backend/migrations/2026-07-28-000026_env_keys/down.sql`:

```sql
-- Restore the app-level key. Values cannot be recovered, so every app gets a
-- fresh one and every deployed SDK must be reconfigured after a revert.
ALTER TABLE apps ADD COLUMN public_key TEXT;
UPDATE apps SET public_key = 'pk_' || replace(gen_random_uuid()::text, '-', '')
WHERE public_key IS NULL;
ALTER TABLE apps ALTER COLUMN public_key SET NOT NULL;
ALTER TABLE apps ADD CONSTRAINT apps_public_key_key UNIQUE (public_key);

DROP INDEX IF EXISTS environments_app_name_active_key;
DROP INDEX IF EXISTS environments_default_key;

-- Retired environments may collide on (app_id, name) once the predicate is gone.
-- Rename rather than delete. A DELETE would cascade through the `ON DELETE SET NULL`
-- FKs on error_events and analytics_events, permanently stripping environment_id from
-- every historical row that pointed at a retired environment — re-applying up.sql
-- restores none of it. It is also slow and lock-heavy: no index leads with
-- environment_id (the closest, error_events_app_env_time_idx, leads with app_id), so
-- the FK probe seq-scans ~40 partitions of the two largest tables inside this
-- migration's single transaction. And transactions.environment_id has no FK at all,
-- so those rows would have been left dangling rather than nulled.
--
--
-- Note the asymmetry this creates: up.sql re-adds `retired_at` as NULL and
-- `ingest_enabled` as DEFAULT true, so a down->up round trip RESURRECTS these rows as
-- active, under the mangled name and with a freshly generated key. Re-retire them after
-- any such cycle. (Not a credential regression: up.sql regenerates every public_key, so
-- an environment retired because its key leaked stays dead either way.)
-- Appending the row's own id guarantees uniqueness without a collision check.
UPDATE environments
SET name = name || '-retired-' || id::text
WHERE retired_at IS NOT NULL;
ALTER TABLE environments ADD CONSTRAINT environments_app_id_name_key UNIQUE (app_id, name);

ALTER TABLE environments DROP CONSTRAINT IF EXISTS environments_public_key_key;
ALTER TABLE environments
  DROP COLUMN IF EXISTS public_key,
  DROP COLUMN IF EXISTS ingest_enabled,
  DROP COLUMN IF EXISTS is_default,
  DROP COLUMN IF EXISTS retired_at,
  DROP COLUMN IF EXISTS updated_at;
```

- [ ] **Step 3: Bring up a database with pre-migration data**

```bash
cd /home/splimter/projects/freelance/sauron
docker compose up -d db
export DATABASE_URL='postgres://sauron:sauron@localhost:5432/sauron'
cd backend && cargo run -p sauron-migrate
```

Expected: migrations apply through `2026-07-28-000026_env_keys` with no error.

- [ ] **Step 4: Verify the resulting shape**

Run each query and check the stated expectation:

```bash
psql "$DATABASE_URL" -c "
SELECT count(*) FILTER (WHERE public_key IS NULL)            AS null_keys,
       count(*) - count(DISTINCT public_key)                 AS dup_keys,
       count(*) FILTER (WHERE public_key !~ '^pk_[0-9a-f]{32}\$') AS bad_format
FROM environments;"
```
Expected: `null_keys = 0`, `dup_keys = 0`, `bad_format = 0`.

```bash
psql "$DATABASE_URL" -c "
SELECT a.id, count(e.id) FILTER (WHERE e.is_default) AS defaults
FROM apps a LEFT JOIN environments e ON e.app_id = a.id
GROUP BY a.id HAVING count(e.id) FILTER (WHERE e.is_default) <> 1;"
```
Expected: zero rows — every app has exactly one default.

```bash
psql "$DATABASE_URL" -c "
SELECT count(*) FROM roles WHERE permissions @> '[\"app:rotate_key\"]'::jsonb;"
```
Expected: `0`.

```bash
psql "$DATABASE_URL" -c "\d apps" | grep -c public_key
```
Expected: `0` — the column is gone.

- [ ] **Step 5: Verify the down migration reverses cleanly**

`sauron-migrate` is a thin embedded-migrations runner with **no subcommands** — it
ignores argv entirely, so `cargo run -p sauron-migrate -- revert` silently runs a
forward migration instead of reverting. Apply the down migration directly, clear its
tracking row, then re-apply:

```bash
psql "$DATABASE_URL" -f migrations/2026-07-28-000026_env_keys/down.sql
psql "$DATABASE_URL" -c "DELETE FROM __diesel_schema_migrations WHERE version = '20260728000026';"
cd backend && cargo run -p sauron-migrate
```
Expected: all three succeed, and re-running Step 4's queries gives the same results.
Confirm the exact `version` string first with
`psql "$DATABASE_URL" -c "SELECT version FROM __diesel_schema_migrations ORDER BY version DESC LIMIT 3;"`.

---

## Task 2: Diesel schema, models, and repo layer

**Files:**
- Modify: `backend/crates/sauron-db/src/schema.rs:25-47`
- Modify: `backend/crates/sauron-db/src/models.rs:119-160`
- Modify: `backend/crates/sauron-db/src/repo.rs:802-989` and `:1310-1323`

**Interfaces:**
- Consumes: Task 1's column set.
- Produces, for Tasks 4 and 5:
  - `models::Environment { id: Uuid, app_id: Uuid, name: String, public_key: String, ingest_enabled: bool, is_default: bool, retired_at: Option<DateTime<Utc>>, created_at: DateTime<Utc>, updated_at: DateTime<Utc> }`
  - `models::EnvRef { env_id: Uuid, app_id: Uuid, project_id: Uuid, org_id: Uuid, env_ingest_enabled: bool, app_ingest_enabled: bool }`
  - `repo::create_environment(conn, app_id: Uuid, name: &str, public_key: &str, is_default: bool) -> QueryResult<Environment>`
  - `repo::list_environments(conn, app_id: Uuid, include_retired: bool) -> QueryResult<Vec<Environment>>`
  - `repo::get_environment(conn, id: Uuid) -> QueryResult<Option<Environment>>`
  - `repo::count_active_environments(conn, app_id: Uuid) -> QueryResult<i64>`
  - `repo::rename_environment(conn, id: Uuid, name: &str) -> QueryResult<Environment>`
  - `repo::set_environment_ingest(conn, id: Uuid, enabled: bool) -> QueryResult<Environment>`
  - `repo::promote_environment_default(conn, app_id: Uuid, id: Uuid) -> QueryResult<Environment>`
  - `repo::retire_environment(conn, id: Uuid) -> QueryResult<Environment>`
  - `repo::rotate_environment_key(conn, id: Uuid, new_key: &str) -> QueryResult<Environment>`
  - `repo::find_env_by_public_key(conn, public_key: &str) -> QueryResult<Option<EnvRef>>`
  - `repo::env_ancestry(conn, env_id: Uuid) -> QueryResult<Option<(Uuid, Uuid, Uuid)>>` returning `(app_id, project_id, org_id)`
  - `repo::MAX_ENVIRONMENTS_PER_APP: i64`
- Removed (callers fixed in Tasks 4 and 5): `repo::upsert_environment`, `repo::rotate_app_key`, `repo::find_app_by_public_key`, `repo::count_environments`, `repo::environment_id_by_name`.

- [ ] **Step 1: Update the Diesel schema**

In `backend/crates/sauron-db/src/schema.rs`, replace lines 25-47 with:

```rust
diesel::table! {
    apps (id) {
        id -> Uuid,
        name -> Text,
        slug -> Text,
        platform -> Nullable<Text>,
        ingest_enabled -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
        app_type -> Text,
        project_id -> Uuid,
    }
}

diesel::table! {
    environments (id) {
        id -> Uuid,
        app_id -> Uuid,
        name -> Text,
        created_at -> Timestamptz,
        public_key -> Text,
        ingest_enabled -> Bool,
        is_default -> Bool,
        retired_at -> Nullable<Timestamptz>,
        updated_at -> Timestamptz,
    }
}
```

Column order must match the physical table: `ADD COLUMN` appends, so the five new columns come after `created_at`.

- [ ] **Step 2: Update the models**

In `backend/crates/sauron-db/src/models.rs`, replace lines 119-160 with:

```rust
#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = apps)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct App {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub platform: Option<String>,
    pub ingest_enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub app_type: String,
    pub project_id: Uuid,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = apps)]
pub struct NewApp<'a> {
    pub project_id: Uuid,
    pub name: &'a str,
    pub slug: &'a str,
    pub app_type: &'a str,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = environments)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Environment {
    pub id: Uuid,
    pub app_id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub public_key: String,
    pub ingest_enabled: bool,
    pub is_default: bool,
    pub retired_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = environments)]
pub struct NewEnvironment<'a> {
    pub app_id: Uuid,
    pub name: &'a str,
    pub public_key: &'a str,
    pub is_default: bool,
}

/// Everything the ingest edge needs after presenting a key: the environment it
/// belongs to, its ancestry, and both ingest switches. Cached in Redis as JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvRef {
    pub env_id: Uuid,
    pub app_id: Uuid,
    pub project_id: Uuid,
    pub org_id: Uuid,
    pub env_ingest_enabled: bool,
    pub app_ingest_enabled: bool,
}
```

`EnvRef` needs `Deserialize`; confirm `serde::Deserialize` is imported at the top of `models.rs` and add it to the existing `use serde::{...}` line if absent.

- [ ] **Step 3: Replace the apps repo functions**

In `backend/crates/sauron-db/src/repo.rs`, change `create_app` (lines 806-825) to drop the key parameter:

```rust
pub async fn create_app(
    conn: &mut AsyncPgConnection,
    project_id: Uuid,
    name: &str,
    slug: &str,
    app_type: &str,
) -> QueryResult<App> {
    diesel::insert_into(apps::table)
        .values(NewApp {
            project_id,
            name,
            slug,
            app_type,
        })
        .returning(App::as_returning())
        .get_result(conn)
        .await
}
```

Delete `find_app_by_public_key` (lines 848-858) and `rotate_app_key` (lines 878-891) entirely.

- [ ] **Step 4: Replace the environments repo section**

In `backend/crates/sauron-db/src/repo.rs`, replace the whole environments block (lines 926-989) with:

```rust
// --- environments -----------------------------------------------------------

/// Cap on how many live environments an app may hold. Creation is now an
/// authenticated admin action rather than a side effect of ingest, so this is a
/// sanity bound rather than an abuse control.
pub const MAX_ENVIRONMENTS_PER_APP: i64 = 500;

pub async fn create_environment(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    name: &str,
    public_key: &str,
    is_default: bool,
) -> QueryResult<Environment> {
    diesel::insert_into(environments::table)
        .values(NewEnvironment {
            app_id,
            name,
            public_key,
            is_default,
        })
        .returning(Environment::as_returning())
        .get_result(conn)
        .await
}

pub async fn list_environments(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    include_retired: bool,
) -> QueryResult<Vec<Environment>> {
    let mut q = environments::table
        .filter(environments::app_id.eq(app_id))
        .into_boxed();
    if !include_retired {
        q = q.filter(environments::retired_at.is_null());
    }
    q.select(Environment::as_select())
        .order(environments::name.asc())
        .limit(MAX_ENVIRONMENTS_PER_APP)
        .load(conn)
        .await
}

pub async fn get_environment(
    conn: &mut AsyncPgConnection,
    id: Uuid,
) -> QueryResult<Option<Environment>> {
    environments::table
        .find(id)
        .select(Environment::as_select())
        .first(conn)
        .await
        .optional()
}

/// Live environments only — the cap must not be consumed by retired rows.
pub async fn count_active_environments(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
) -> QueryResult<i64> {
    environments::table
        .filter(environments::app_id.eq(app_id))
        .filter(environments::retired_at.is_null())
        .count()
        .get_result(conn)
        .await
}

pub async fn rename_environment(
    conn: &mut AsyncPgConnection,
    id: Uuid,
    name: &str,
) -> QueryResult<Environment> {
    diesel::update(environments::table.find(id))
        .set((
            environments::name.eq(name),
            environments::updated_at.eq(Utc::now()),
        ))
        .returning(Environment::as_returning())
        .get_result(conn)
        .await
}

pub async fn set_environment_ingest(
    conn: &mut AsyncPgConnection,
    id: Uuid,
    enabled: bool,
) -> QueryResult<Environment> {
    diesel::update(environments::table.find(id))
        .set((
            environments::ingest_enabled.eq(enabled),
            environments::updated_at.eq(Utc::now()),
        ))
        .returning(Environment::as_returning())
        .get_result(conn)
        .await
}

/// Move the default flag within an app. Both statements run in one transaction
/// because `environments_default_key` is a partial unique index on
/// `(app_id) WHERE is_default` — setting the new default before clearing the old
/// one violates it mid-statement.
pub async fn promote_environment_default(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    id: Uuid,
) -> QueryResult<Environment> {
    // Native `async |conn|`, NOT `Box::pin(async move { .. })`. diesel-async 0.9's
    // `AsyncFunc` blanket impl pins one associated future type, which a boxed
    // `dyn Future + 'r` cannot satisfy for every lifetime — the boxed form fails with
    // "implementation of `AsyncFnOnce` is not general enough".
    conn.transaction::<_, diesel::result::Error, _>(async |conn| {
        diesel::update(environments::table)
            .filter(environments::app_id.eq(app_id))
            .filter(environments::is_default.eq(true))
            .set((
                environments::is_default.eq(false),
                environments::updated_at.eq(Utc::now()),
            ))
            .execute(conn)
            .await?;
        // `app_id` is re-asserted here rather than trusting `find(id)` alone: a caller
        // that authorized on app A but passed app B's env id would otherwise leave A
        // with zero defaults and silently give B one.
        diesel::update(
            environments::table
                .find(id)
                .filter(environments::app_id.eq(app_id)),
        )
        .set((
            environments::is_default.eq(true),
            environments::updated_at.eq(Utc::now()),
        ))
        .returning(Environment::as_returning())
        .get_result(conn)
        .await
    })
    .await
}

/// Retire, never delete. The row is kept so historical rows — including any
/// already exported to cold Parquet, which no FK can reach — stay attributable.
pub async fn retire_environment(
    conn: &mut AsyncPgConnection,
    id: Uuid,
) -> QueryResult<Environment> {
    let now = Utc::now();
    diesel::update(environments::table.find(id))
        .set((
            environments::retired_at.eq(Some(now)),
            environments::ingest_enabled.eq(false),
            // Clear the flag too. The retire handler refuses to retire a live default,
            // so this is normally a no-op — but leaving it set would make
            // `list_environments(include_retired = true)` return two rows flagged
            // default, and the settings UI would render two "Default" badges.
            environments::is_default.eq(false),
            environments::updated_at.eq(now),
        ))
        .returning(Environment::as_returning())
        .get_result(conn)
        .await
}

pub async fn rotate_environment_key(
    conn: &mut AsyncPgConnection,
    id: Uuid,
    new_key: &str,
) -> QueryResult<Environment> {
    diesel::update(environments::table.find(id))
        .set((
            environments::public_key.eq(new_key),
            environments::updated_at.eq(Utc::now()),
        ))
        .returning(Environment::as_returning())
        .get_result(conn)
        .await
}

/// Resolve an ingest key to its environment and full ancestry in one query.
/// Retired environments are excluded, so a retired key is indistinguishable from
/// an unknown one and falls through to the existing `invalid_key` path.
pub async fn find_env_by_public_key(
    conn: &mut AsyncPgConnection,
    public_key: &str,
) -> QueryResult<Option<EnvRef>> {
    environments::table
        .inner_join(apps::table.on(apps::id.eq(environments::app_id)))
        .inner_join(projects::table.on(projects::id.eq(apps::project_id)))
        .filter(environments::public_key.eq(public_key))
        .filter(environments::retired_at.is_null())
        .select((
            environments::id,
            apps::id,
            apps::project_id,
            projects::org_id,
            environments::ingest_enabled,
            apps::ingest_enabled,
        ))
        .first::<(Uuid, Uuid, Uuid, Uuid, bool, bool)>(conn)
        .await
        .optional()
        .map(|row| {
            row.map(
                |(env_id, app_id, project_id, org_id, env_ingest_enabled, app_ingest_enabled)| {
                    EnvRef {
                        env_id,
                        app_id,
                        project_id,
                        org_id,
                        env_ingest_enabled,
                        app_ingest_enabled,
                    }
                },
            )
        })
}

/// `(app_id, project_id, org_id)` ancestry of an environment — for permission
/// resolution, mirroring `app_ancestry`. Slice 3's `authorize_env` reuses this.
pub async fn env_ancestry(
    conn: &mut AsyncPgConnection,
    env_id: Uuid,
) -> QueryResult<Option<(Uuid, Uuid, Uuid)>> {
    environments::table
        .inner_join(apps::table.on(apps::id.eq(environments::app_id)))
        .inner_join(projects::table.on(projects::id.eq(apps::project_id)))
        .filter(environments::id.eq(env_id))
        .select((environments::app_id, apps::project_id, projects::org_id))
        .first(conn)
        .await
        .optional()
}
```

- [ ] **Step 5: Delete `environment_id_by_name`**

Delete lines 1310-1323 of `backend/crates/sauron-db/src/repo.rs` (the `environment_id_by_name` function). The query planner resolves environment names through `query_plan/prepare.rs::resolve_environments`, which queries the table directly and is unaffected.

Add `use diesel_async::AsyncConnection;` to the imports at the top of `repo.rs` if not already present — `promote_environment_default` needs it for `.transaction()`.

- [ ] **Step 6: Compile**

Run: `cd backend && cargo check -p sauron-db`
Expected: compiles. Errors in *other* crates are expected at this point and are fixed in Tasks 4 and 5.

Then guard against the known `schema.rs` hazard — a previous slice twice had this file
silently regenerated from 27 table blocks to 87 (one block per partition child, which
the tier worker creates and drops on a schedule). It compiles and passes every gate, so
only a count catches it:

```bash
cd backend && grep -c "^diesel::table!" crates/sauron-db/src/schema.rs
```
Expected: `27`. If it is anything else, `schema.rs` was regenerated — revert it and
re-apply this task's two `table!` edits by hand.

- [ ] **Step 7: Verify no stale callers remain in this crate**

Run:
```bash
cd backend && grep -rn "upsert_environment\|rotate_app_key\|find_app_by_public_key\|environment_id_by_name\|count_environments" crates/sauron-db/src/
```
Expected: no output.

---

## Task 3: The `env:*` permission family

**Files:**
- Modify: `backend/crates/sauron-auth/src/rbac.rs:24-160` (perm module + presets)
- Modify: `backend/crates/sauron-auth/src/rbac.rs:414-508` (invariant tests)

**Interfaces:**
- Consumes: nothing.
- Produces, for Task 4: `perm::ENV_READ` (`"env:read"`), `perm::ENV_CREATE` (`"env:create"`), `perm::ENV_UPDATE` (`"env:update"`), `perm::ENV_DELETE` (`"env:delete"`), `perm::ENV_ROTATE_KEY` (`"env:rotate_key"`). Removes `perm::APP_ROTATE_KEY`. `perm::ALL` becomes `[&str; 27]`.

- [ ] **Step 1: Update the failing tests first**

In `backend/crates/sauron-auth/src/rbac.rs`, replace the test bodies at lines 416-446 and 470-475 with the new expectations:

```rust
    #[test]
    fn owner_has_every_permission() {
        for p in perm::ALL {
            assert!(OWNER.permissions.contains(&p), "Owner missing {p}");
        }
        assert_eq!(OWNER.permissions.len(), 27);
    }

    #[test]
    fn admin_is_all_except_org_manage() {
        assert!(!ADMIN.permissions.contains(&perm::ORG_MANAGE));
        assert_eq!(ADMIN.permissions.len(), 26);
        for p in perm::ALL {
            if p != perm::ORG_MANAGE {
                assert!(ADMIN.permissions.contains(&p), "Admin missing {p}");
            }
        }
    }

    #[test]
    fn developer_can_write_issues_not_manage_members() {
        assert!(DEVELOPER.permissions.contains(&perm::ISSUE_WRITE));
        assert!(DEVELOPER.permissions.contains(&perm::ENV_ROTATE_KEY));
        assert!(!DEVELOPER.permissions.contains(&perm::MEMBER_MANAGE));
        assert!(!DEVELOPER.permissions.contains(&perm::PROJECT_DELETE));
        assert!(!DEVELOPER.permissions.contains(&perm::ROLE_MANAGE));
        assert!(DEVELOPER.permissions.contains(&perm::FUNNEL_WRITE));
        assert!(DEVELOPER.permissions.contains(&perm::ARTIFACT_WRITE));
        assert!(DEVELOPER.permissions.contains(&perm::SOURCE_READ));
        assert_eq!(DEVELOPER.permissions.len(), 18);
    }

    /// Developer manages environments day to day but cannot retire one, mirroring
    /// how it holds `app:update` without `app:delete`.
    #[test]
    fn developer_manages_envs_but_cannot_retire() {
        assert!(DEVELOPER.permissions.contains(&perm::ENV_READ));
        assert!(DEVELOPER.permissions.contains(&perm::ENV_CREATE));
        assert!(DEVELOPER.permissions.contains(&perm::ENV_UPDATE));
        assert!(!DEVELOPER.permissions.contains(&perm::ENV_DELETE));
    }
```

And at lines 470-475:

```rust
    #[test]
    fn all_permissions_are_unique() {
        let set: HashSet<_> = perm::ALL.iter().collect();
        assert_eq!(set.len(), perm::ALL.len(), "duplicate in perm::ALL");
        assert_eq!(perm::ALL.len(), 27);
    }
```

In `viewer_is_read_only` (lines 454-462), change the final assertion to account for `env:read`:

```rust
        assert_eq!(VIEWER.permissions.len(), 7);
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd backend && cargo test -p sauron-auth`
Expected: FAIL — `cannot find value ENV_ROTATE_KEY in module perm` and similar unresolved-name errors.

- [ ] **Step 3: Update the `perm` module**

In `backend/crates/sauron-auth/src/rbac.rs`, delete line 40 (`pub const APP_ROTATE_KEY`) and add the env constants after `APP_DELETE` (line 39):

```rust
    pub const APP_DELETE: &str = "app:delete";
    /// Environments own the ingest credential, so they carry their own family
    /// rather than borrowing the app's. These name *what* is managed, not a new
    /// scope level — checks still resolve against the parent app until Slice 3
    /// introduces `Scope::Env`.
    pub const ENV_READ: &str = "env:read";
    pub const ENV_CREATE: &str = "env:create";
    pub const ENV_UPDATE: &str = "env:update";
    pub const ENV_DELETE: &str = "env:delete";
    pub const ENV_ROTATE_KEY: &str = "env:rotate_key";
```

Replace the `ALL` array (lines 56-80) with:

```rust
    /// Every permission, in canonical order.
    pub const ALL: [&str; 27] = [
        ISSUE_READ,
        ISSUE_WRITE,
        EVENT_READ,
        FUNNEL_WRITE,
        ARTIFACT_WRITE,
        SOURCE_READ,
        MONITOR_READ,
        MONITOR_WRITE,
        APP_READ,
        APP_CREATE,
        APP_UPDATE,
        APP_DELETE,
        ENV_READ,
        ENV_CREATE,
        ENV_UPDATE,
        ENV_DELETE,
        ENV_ROTATE_KEY,
        PROJECT_READ,
        PROJECT_CREATE,
        PROJECT_UPDATE,
        PROJECT_DELETE,
        MEMBER_READ,
        MEMBER_MANAGE,
        ROLE_MANAGE,
        ORG_MANAGE,
        ALERT_READ,
        ALERT_WRITE,
    ];
```

- [ ] **Step 4: Update the presets**

In `ADMIN.permissions` (lines 99-122), replace `perm::APP_ROTATE_KEY,` with the five env constants, keeping canonical order — i.e. after `perm::APP_DELETE,` insert:

```rust
        perm::ENV_READ,
        perm::ENV_CREATE,
        perm::ENV_UPDATE,
        perm::ENV_DELETE,
        perm::ENV_ROTATE_KEY,
```

In `DEVELOPER.permissions` (lines 128-144), replace `perm::APP_ROTATE_KEY,` with:

```rust
        perm::ENV_READ,
        perm::ENV_CREATE,
        perm::ENV_UPDATE,
        perm::ENV_ROTATE_KEY,
```

In `VIEWER.permissions` (lines 150-157), add after `perm::APP_READ,`:

```rust
        perm::ENV_READ,
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cd backend && cargo test -p sauron-auth`
Expected: PASS, including `roles_form_a_strict_ladder` (Viewer's `env:read` ⊂ Developer's four ⊂ Admin's five ⊂ Owner's `ALL`) and `viewer_is_read_only` (`env:read` ends in `:read`).

- [ ] **Step 6: Verify no stale references to the removed permission**

Run: `cd backend && grep -rn "APP_ROTATE_KEY\|app:rotate_key" --include=*.rs .`
Expected: only hits in `bins/sauron-api/src/routes/apps.rs` (removed in Task 4). If any other crate references it, fix it now.

---

## Task 4: Environment CRUD routes

**Files:**
- Create: `backend/bins/sauron-api/src/routes/environments.rs`
- Modify: `backend/bins/sauron-api/src/routes/apps.rs` (delete `rotate_key` and `list_environments`)
- Modify: `backend/bins/sauron-api/src/routes/projects.rs:116-153` (`create_app`)
- Modify: `backend/bins/sauron-api/src/routes/mod.rs` (add `pub mod environments;`)
- Modify: `backend/bins/sauron-api/src/main.rs:199-216` (router)

**Interfaces:**
- Consumes: Task 2's repo functions and `models::Environment`; Task 3's `perm::ENV_*`.
- Produces: the HTTP contract consumed by Tasks 6–8, and the pure validators `validate_env_name(&str) -> Result<&str, String>` and `default_env_name() -> &'static str`.

- [ ] **Step 1: Write the failing validator tests**

Create `backend/bins/sauron-api/src/routes/environments.rs` containing **only** the doc comment and the test module — no implementation yet:

```rust
//! Environment management: create, rename, mute, promote, rotate, retire.
//!
//! Environments are app-scoped resources, so every check resolves through
//! `authorize_app` against the parent app. The `env:*` permissions name what is
//! being managed, not a new scope level — `Scope::Env` arrives in Slice 3.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_and_whitespace() {
        assert!(validate_env_name("").is_err());
        assert!(validate_env_name("   ").is_err());
    }

    #[test]
    fn trims_surrounding_whitespace() {
        assert_eq!(validate_env_name("  staging  ").unwrap(), "staging");
    }

    #[test]
    fn rejects_overlong_name() {
        let long = "x".repeat(MAX_ENV_NAME_LEN + 1);
        assert!(validate_env_name(&long).is_err());
        let ok = "x".repeat(MAX_ENV_NAME_LEN);
        assert!(validate_env_name(&ok).is_ok());
    }

    #[test]
    fn counts_characters_not_bytes() {
        // 64 multi-byte characters is 64 characters, not 192 bytes.
        let multibyte = "é".repeat(MAX_ENV_NAME_LEN);
        assert!(validate_env_name(&multibyte).is_ok());
    }

    #[test]
    fn default_env_is_dev() {
        assert_eq!(DEFAULT_ENV_NAME, "dev");
    }
}
```

Add `pub mod environments;` to `backend/bins/sauron-api/src/routes/mod.rs`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd backend && cargo test -p sauron-api routes::environments`
Expected: FAIL to compile — `cannot find function validate_env_name in this scope`, `cannot find value MAX_ENV_NAME_LEN`, `cannot find value DEFAULT_ENV_NAME`.

- [ ] **Step 3: Write the validator**

Add above the test module in `backend/bins/sauron-api/src/routes/environments.rs`:

```rust
/// Cap on a stored environment name. Was 64 when the value arrived from the
/// envelope; kept at 64 now that it is admin-supplied so existing rows stay valid.
const MAX_ENV_NAME_LEN: usize = 64;

/// The environment every new app is born with.
pub const DEFAULT_ENV_NAME: &str = "dev";

/// Trim and bounds-check an admin-supplied environment name.
pub fn validate_env_name(raw: &str) -> Result<&str, String> {
    let name = raw.trim();
    if name.is_empty() {
        return Err("environment name is required".into());
    }
    if name.chars().count() > MAX_ENV_NAME_LEN {
        return Err(format!(
            "environment name must be at most {MAX_ENV_NAME_LEN} characters"
        ));
    }
    Ok(name)
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd backend && cargo test -p sauron-api routes::environments`
Expected: PASS (5 tests).

- [ ] **Step 5: Add the handlers**

Append to `backend/bins/sauron-api/src/routes/environments.rs`:

```rust
use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use sauron_auth::{authorize_app, perm, AuthUser};
use sauron_core::ids;
use sauron_db::models::Environment;
use sauron_db::repo;
use sauron_redis::keys;

use super::db;
use crate::error::ApiError;
use crate::AppState;

#[derive(Deserialize)]
pub struct ListEnvQuery {
    #[serde(default)]
    pub include_retired: bool,
}

pub async fn list_environments(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<ListEnvQuery>,
) -> Result<Json<Vec<Environment>>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::ENV_READ).await?;
    Ok(Json(
        repo::list_environments(&mut conn, app_id, q.include_retired).await?,
    ))
}

#[derive(Deserialize)]
pub struct CreateEnvReq {
    pub name: String,
}

pub async fn create_environment(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Json(req): Json<CreateEnvReq>,
) -> Result<Json<Environment>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::ENV_CREATE).await?;
    let name = validate_env_name(&req.name).map_err(ApiError::BadRequest)?;

    if repo::count_active_environments(&mut conn, app_id).await?
        >= repo::MAX_ENVIRONMENTS_PER_APP
    {
        return Err(ApiError::Conflict(format!(
            "app already has {} environments",
            repo::MAX_ENVIRONMENTS_PER_APP
        )));
    }

    let key = ids::public_key();
    // `environments_app_name_active_key` turns a duplicate live name into a
    // unique violation; map it rather than pre-checking, so a concurrent create
    // cannot slip between the check and the insert.
    match repo::create_environment(&mut conn, app_id, name, &key, false).await {
        Ok(env) => Ok(Json(env)),
        Err(diesel::result::Error::DatabaseError(
            diesel::result::DatabaseErrorKind::UniqueViolation,
            _,
        )) => Err(ApiError::Conflict(format!(
            "an environment named \"{name}\" already exists"
        ))),
        Err(e) => Err(e.into()),
    }
}

#[derive(Deserialize)]
pub struct UpdateEnvReq {
    pub name: Option<String>,
    pub ingest_enabled: Option<bool>,
    pub is_default: Option<bool>,
}

pub async fn update_environment(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(env_id): Path<Uuid>,
    Json(req): Json<UpdateEnvReq>,
) -> Result<Json<Environment>, ApiError> {
    let mut conn = db(&state).await?;
    let (app_id, _, _) = repo::env_ancestry(&mut conn, env_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::ENV_UPDATE).await?;

    let env = repo::get_environment(&mut conn, env_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if env.retired_at.is_some() {
        return Err(ApiError::Conflict(
            "this environment is retired and can no longer be edited".into(),
        ));
    }

    let mut current = env;

    if let Some(raw) = req.name.as_deref() {
        let name = validate_env_name(raw).map_err(ApiError::BadRequest)?;
        current = match repo::rename_environment(&mut conn, env_id, name).await {
            Ok(e) => e,
            Err(diesel::result::Error::DatabaseError(
                diesel::result::DatabaseErrorKind::UniqueViolation,
                _,
            )) => {
                return Err(ApiError::Conflict(format!(
                    "an environment named \"{name}\" already exists"
                )))
            }
            Err(e) => return Err(e.into()),
        };
    }

    if let Some(enabled) = req.ingest_enabled {
        let old_key = current.public_key.clone();
        current = repo::set_environment_ingest(&mut conn, env_id, enabled).await?;
        // The cached EnvRef carries the ingest flags, so it must be dropped.
        let _ = state.redis.del(&keys::dsn_cache(&old_key)).await;
    }

    if let Some(is_default) = req.is_default {
        if !is_default {
            return Err(ApiError::BadRequest(
                "a default environment is moved, not unset — promote another environment instead"
                    .into(),
            ));
        }
        current = repo::promote_environment_default(&mut conn, app_id, env_id).await?;
    }

    Ok(Json(current))
}

pub async fn rotate_environment_key(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(env_id): Path<Uuid>,
) -> Result<Json<Environment>, ApiError> {
    let mut conn = db(&state).await?;
    let (app_id, _, _) = repo::env_ancestry(&mut conn, env_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::ENV_ROTATE_KEY).await?;

    let env = repo::get_environment(&mut conn, env_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if env.retired_at.is_some() {
        return Err(ApiError::Conflict(
            "this environment is retired and can no longer be edited".into(),
        ));
    }

    let new_key = ids::public_key();
    let updated = repo::rotate_environment_key(&mut conn, env_id, &new_key).await?;
    // Invalidate the OLD key's slot, captured before the update.
    let _ = state.redis.del(&keys::dsn_cache(&env.public_key)).await;
    Ok(Json(updated))
}

pub async fn retire_environment(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(env_id): Path<Uuid>,
) -> Result<Json<Environment>, ApiError> {
    let mut conn = db(&state).await?;
    let (app_id, _, _) = repo::env_ancestry(&mut conn, env_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::ENV_DELETE).await?;

    let env = repo::get_environment(&mut conn, env_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if env.retired_at.is_some() {
        return Ok(Json(env)); // idempotent
    }
    if env.is_default {
        return Err(ApiError::Conflict(
            "cannot retire the default environment — promote another one first".into(),
        ));
    }
    if repo::count_active_environments(&mut conn, app_id).await? <= 1 {
        return Err(ApiError::Conflict(
            "cannot retire the last environment — an app must have somewhere to report".into(),
        ));
    }

    let retired = repo::retire_environment(&mut conn, env_id).await?;
    let _ = state.redis.del(&keys::dsn_cache(&env.public_key)).await;
    Ok(Json(retired))
}
```

If `ApiError` has no `Conflict(String)` variant, add one mapping to HTTP 409 in `backend/bins/sauron-api/src/error.rs`, following the existing `BadRequest` variant's shape.

- [ ] **Step 6: Strip the dead app-key surface**

In `backend/bins/sauron-api/src/routes/apps.rs`:
- delete `rotate_key` (lines 71-83)
- delete `list_environments` (lines 85-93)
- delete the now-unused imports `sauron_core::ids` and `sauron_db::models::Environment` from lines 10-11
- in `update_app` (line 55) and `delete_app` (line 67), delete the `state.redis.del(&keys::dsn_cache(&app.public_key))` lines — apps no longer own a key. If `keys` becomes unused, drop its import too.

`update_app`'s `ingest_enabled` still gates ingest (the resolver reads `apps.ingest_enabled`), but its cache entries are keyed by *environment* keys now. Add this comment above the `repo::update_app` call:

```rust
    // No DSN cache to invalidate here: cache slots are keyed by environment key.
    // A mute toggled at app level takes effect within the 300s positive TTL.
```

- [ ] **Step 7: Seed `dev` on app creation**

In `backend/bins/sauron-api/src/routes/projects.rs`, replace lines 140-152 of `create_app` with:

```rust
    let app = repo::create_app(
        &mut conn,
        project_id,
        &req.name,
        &slugify(&req.name),
        &req.app_type,
    )
    .await?;
    // Every app is born with one environment, and it owns the ingest key — an app
    // with none would be unreachable by any SDK.
    repo::create_environment(
        &mut conn,
        app.id,
        crate::routes::environments::DEFAULT_ENV_NAME,
        &ids::public_key(),
        true,
    )
    .await?;
    Ok(Json(app))
```

Note this is now a hard `?` rather than the previous `let _ =`: a keyless app is unusable, so a failure here must surface rather than produce a broken app.

- [ ] **Step 8: Wire the router**

In `backend/bins/sauron-api/src/main.rs`, replace lines 205-212 with:

```rust
        .route(
            "/v1/apps/{app_id}/environments",
            get(routes::environments::list_environments)
                .post(routes::environments::create_environment),
        )
        .route(
            "/v1/environments/{env_id}",
            patch(routes::environments::update_environment)
                .delete(routes::environments::retire_environment),
        )
        .route(
            "/v1/environments/{env_id}/rotate-key",
            post(routes::environments::rotate_environment_key),
        )
```

Confirm `patch` is in the `axum::routing::{...}` import list at the top of `main.rs`; add it if absent.

- [ ] **Step 9: Run the full backend gate**

Run: `cd backend && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: PASS. The ingest crate still fails to compile until Task 5 — if so, complete Task 5 before treating this gate as green.

---

## Task 5: Ingest resolves the environment from the key

**Files:**
- Modify: `backend/crates/sauron-redis/src/lib.rs:43-45` (cache key prefix)
- Modify: `backend/crates/sauron-core/src/envelope.rs:49-62` and `:305-332`
- Modify: `backend/bins/sauron-ingest/src/main.rs:40-45`, `:172-292`, `:294-344`
- Modify: `backend/crates/sauron-pipeline/src/process.rs:17-59` and `:472-481`

**Interfaces:**
- Consumes: Task 2's `repo::find_env_by_public_key` and `models::EnvRef`.
- Produces: `IngestJob.environment_id: Uuid` (no longer `environment: Option<String>`), consumed by the pipeline.

- [ ] **Step 1: Bump the DSN cache prefix**

In `backend/crates/sauron-redis/src/lib.rs`, replace lines 43-45:

```rust
    /// Cache slot for a resolved ingest key. Takes the **raw** public key and
    /// fingerprints it internally.
    ///
    /// The `v2` segment is load-bearing. The cached value changed shape when the
    /// key moved from apps to environments; without a new prefix, entries written
    /// by the previous binary would deserialize into the wrong struct (or fail
    /// and silently fall through to Postgres) for the full 300s TTL after deploy.
    pub fn dsn_cache(public_key: &str) -> String {
        format!("sauron:dsn:v2:{}", key_fingerprint(public_key))
    }
```

- [ ] **Step 2: Remove `environment` from the wire types**

In `backend/crates/sauron-core/src/envelope.rs`, delete lines 58-59 from `EnvelopeHeader`:

```rust
    #[serde(default)]
    pub environment: Option<String>,
```

Serde ignores unknown fields by default, so an SDK that still sends `environment` continues to ingest — its environment simply comes from its key.

In `IngestJob` (lines 309-332), replace lines 314-315:

```rust
    #[serde(default)]
    pub environment: Option<String>,
```

with:

```rust
    /// Resolved at the edge from the presented ingest key, never from client
    /// input. Not `Option`: a job cannot exist without a key, and a key cannot
    /// exist without an environment.
    pub environment_id: Uuid,
```

Update the golden fixture at lines ~347-354 to drop the `"environment": "production",` line, and the assertion at line ~378 that reads `env.header.environment`.

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cd backend && cargo test -p sauron-core`
Expected: FAIL — the envelope test module still references `header.environment`. Fix the fixture and assertion as described until `cargo test -p sauron-core` passes.

- [ ] **Step 4: Rewrite the ingest resolver**

In `backend/bins/sauron-ingest/src/main.rs`, delete the `AppRef` struct (lines 40-45) — `sauron_db::models::EnvRef` replaces it. Add `use sauron_db::models::EnvRef;` to the imports.

Replace `resolve_app` (lines 297-344) with:

```rust
/// Resolve an ingest key to its environment, caching the result in Redis.
///
/// Unknown keys are cached too (briefly). Without that, every request bearing a
/// bogus key is a guaranteed cache miss and therefore a database round-trip on
/// an unauthenticated path — a cheap way to exhaust the ingest pool. A retired
/// environment is excluded by the query, so its key is indistinguishable from an
/// unknown one and lands on the same path.
async fn resolve_env(state: &AppState, key: &str) -> anyhow::Result<Option<EnvRef>> {
    let cache_key = keys::dsn_cache(key);
    if let Some(cached) = state.redis.get(&cache_key).await? {
        if cached == NEGATIVE_CACHE_MARKER {
            return Ok(None);
        }
        if let Ok(e) = serde_json::from_str::<EnvRef>(&cached) {
            return Ok(Some(e));
        }
    }

    let mut conn = sauron_db::conn(&state.pool).await?;
    let resolved = sauron_db::repo::find_env_by_public_key(&mut conn, key).await?;
    drop(conn);

    match resolved {
        Some(eref) => {
            if let Ok(json) = serde_json::to_string(&eref) {
                let _ = state.redis.set_ex(&cache_key, &json, 300).await;
            }
            Ok(Some(eref))
        }
        None => {
            // Short TTL so a key that is created moments later still works.
            let _ = state
                .redis
                .set_ex(&cache_key, NEGATIVE_CACHE_MARKER, 30)
                .await;
            Ok(None)
        }
    }
}
```

- [ ] **Step 5: Update the ingest handler**

In `backend/bins/sauron-ingest/src/main.rs`, replace step 3 of the handler (lines 206-226) with:

```rust
    // 3. Resolve the environment (cache → Postgres). Unknown keys are negatively
    //    cached inside `resolve_env` so a repeat miss never reaches the database.
    let env = match resolve_env(&state, &key).await {
        Ok(Some(e)) if e.env_ingest_enabled && e.app_ingest_enabled => e,
        Ok(Some(_)) => return error(StatusCode::FORBIDDEN, "ingest_disabled", "ingest disabled"),
        Ok(None) => {
            return error(
                StatusCode::UNAUTHORIZED,
                "invalid_key",
                "unknown ingest key",
            )
        }
        Err(e) => {
            warn!(error = %e, "environment resolution failed");
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "resolution failed",
            );
        }
    };
```

Replace line 229 with `let rl_key = keys::rate_limit(&env.app_id.to_string());`, and replace the job construction at lines 266-278 with:

```rust
        let job = IngestJob {
            app_id: env.app_id,
            project_id: env.project_id,
            org_id: env.org_id,
            environment_id: env.env_id,
            release: envelope.header.release.clone(),
            received_at,
            ip: ip.clone(),
            user_agent: user_agent.clone(),
            context: envelope.context.clone(),
            sdk: Some(envelope.header.sdk.clone()),
            item,
        };
```

- [ ] **Step 6: Simplify the pipeline**

In `backend/crates/sauron-pipeline/src/process.rs`, replace lines 17-37 with:

```rust
/// Process one job end to end: dispatch by item type.
pub async fn process_job(
    pool: &PgPool,
    redis: &RedisStore,
    sym: &crate::symbolize::SymbolizeCtx,
    job: IngestJob,
) -> anyhow::Result<()> {
    let mut conn = sauron_db::conn(pool).await?;

    // Resolved at the ingest edge from the presented key. The client no longer
    // has any say in which environment a signal lands in.
    let environment_id = Some(job.environment_id);
```

Delete `MAX_ENVIRONMENT_NAME_LEN` (line 474). **Keep `truncate`** — it has an unrelated
live caller in `build_title` that predates this slice. Verify:

```bash
cd backend && grep -rn "MAX_ENVIRONMENT_NAME_LEN" crates/sauron-pipeline/src/
```
Expected: no output.

- [ ] **Step 7: Fix `crebain`, the third broken crate**

The load generator is also a consumer of both changed contracts. Two edits:

`backend/bins/crebain/src/harness.rs:149-160` — `seed()` creates an app with a caller-chosen
key so the generator can address it. The key now lives on an environment:

```rust
        let app = sauron_db::repo::create_app(
            &mut conn, project.id, "crebain", "crebain", "web",
        )
        .await?;
        // The generator addresses the app by key, and the key now belongs to an
        // environment rather than the app.
        sauron_db::repo::create_environment(&mut conn, app.id, "bench", public_key, true).await?;
```

`backend/bins/crebain/src/generator.rs:117` — delete the `environment: Some(ENVIRONMENT.to_string()),`
line from the envelope header it builds, and remove the now-unused `ENVIRONMENT` const.

`dsn.rs` and `engine.rs` need no change: they carry the key as an opaque string, which is
still exactly what it is.

- [ ] **Step 8: Run the full backend gate**

Run: `cd backend && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: PASS across the whole workspace.

- [ ] **Step 9: Verify the client string is gone from the backend**

Run: `cd backend && grep -rn "header.environment\|job.environment\b" --include=*.rs .`
Expected: no output.

---

## Task 6: Dashboard models, permission mirror, and API client

**Files:**
- Modify: `dashboard/src/lib/models/permissions.ts:11-87`
- Modify: `dashboard/src/lib/models/index.ts:81-100`
- Create: `dashboard/src/lib/api/environments.ts`
- Modify: `dashboard/src/lib/api/apps.ts:34-42`

**Interfaces:**
- Consumes: Task 3's `perm::ALL` order; Task 4's HTTP contract.
- Produces, for Tasks 7 and 8:
  - `Environment { id, app_id, name, public_key, ingest_enabled, is_default, retired_at: string | null, created_at, updated_at }`
  - `listEnvironments(appId: string, includeRetired?: boolean): Promise<Environment[]>`
  - `createEnvironment(appId: string, body: { name: string }): Promise<Environment>`
  - `updateEnvironment(envId: string, body: { name?: string; ingest_enabled?: boolean; is_default?: boolean }): Promise<Environment>`
  - `rotateEnvironmentKey(envId: string): Promise<Environment>`
  - `retireEnvironment(envId: string): Promise<Environment>`

- [ ] **Step 1: Run the permission parity test to verify it fails**

Run: `cd dashboard && npm test -- permissions`
Expected: FAIL on `matches the backend catalog exactly, in order` — the backend now declares 27 permissions including `env:*`, while `ALL_PERMISSIONS` still lists 23 with `app:rotate_key`. This test parses `backend/crates/sauron-auth/src/rbac.rs` directly, so it is already red from Task 3.

- [ ] **Step 2: Update the permission mirror**

In `dashboard/src/lib/models/permissions.ts`, replace lines 11-35:

```ts
export const ALL_PERMISSIONS: Permission[] = [
  'issue:read',
  'issue:write',
  'event:read',
  'funnel:write',
  'artifact:write',
  'source:read',
  'monitor:read',
  'monitor:write',
  'app:read',
  'app:create',
  'app:update',
  'app:delete',
  'env:read',
  'env:create',
  'env:update',
  'env:delete',
  'env:rotate_key',
  'project:read',
  'project:create',
  'project:update',
  'project:delete',
  'member:read',
  'member:manage',
  'role:manage',
  'org:manage',
  'alert:read',
  'alert:write',
];
```

Replace the `Apps` group (lines 48-51) and add an `Environments` group after it:

```ts
  {
    label: 'Apps',
    permissions: ['app:read', 'app:create', 'app:update', 'app:delete'],
  },
  {
    label: 'Environments',
    permissions: ['env:read', 'env:create', 'env:update', 'env:delete', 'env:rotate_key'],
  },
```

In `PERMISSION_LABELS`, replace the `'app:rotate_key'` entry (line 76) with:

```ts
  'env:read': 'View environments and their ingest keys',
  'env:create': 'Create environments',
  'env:update': 'Rename environments, mute ingest, change the default',
  'env:delete': 'Retire environments',
  'env:rotate_key': 'Rotate environment ingest keys',
```

- [ ] **Step 3: Run the permission tests to verify they pass**

Run: `cd dashboard && npm test -- permissions`
Expected: PASS (4 tests) — parity, exact order, every permission grouped exactly once, and every permission labelled.

- [ ] **Step 4: Update the model types**

In `dashboard/src/lib/models/index.ts`, remove `public_key` from `App` (line 87) and replace `Environment` (lines 95-100):

```ts
export interface Environment {
  id: string;
  app_id: string;
  name: string;
  created_at: string;
  /** Non-secret, write-only ingest credential. Safe to render. */
  public_key: string;
  ingest_enabled: boolean;
  is_default: boolean;
  /** Non-null once retired: ingest is off and it is hidden from pickers. */
  retired_at: string | null;
  updated_at: string;
}
```

- [ ] **Step 5: Create the environments API client**

Create `dashboard/src/lib/api/environments.ts`:

```ts
import { api } from './client';
import type { Environment } from '../models';

export async function listEnvironments(
  appId: string,
  includeRetired = false,
): Promise<Environment[]> {
  const { data } = await api.get<Environment[]>(`/v1/apps/${appId}/environments`, {
    params: includeRetired ? { include_retired: true } : undefined,
  });
  return data;
}

export async function createEnvironment(
  appId: string,
  body: { name: string },
): Promise<Environment> {
  const { data } = await api.post<Environment>(`/v1/apps/${appId}/environments`, body);
  return data;
}

export async function updateEnvironment(
  envId: string,
  body: { name?: string; ingest_enabled?: boolean; is_default?: boolean },
): Promise<Environment> {
  const { data } = await api.patch<Environment>(`/v1/environments/${envId}`, body);
  return data;
}

export async function rotateEnvironmentKey(envId: string): Promise<Environment> {
  const { data } = await api.post<Environment>(`/v1/environments/${envId}/rotate-key`);
  return data;
}

/** Retires rather than deletes — the row is kept so history stays attributable. */
export async function retireEnvironment(envId: string): Promise<Environment> {
  const { data } = await api.delete<Environment>(`/v1/environments/${envId}`);
  return data;
}
```

- [ ] **Step 6: Strip the dead app-key client**

In `dashboard/src/lib/api/apps.ts`, delete `rotateAppKey` (lines 34-37) and `listEnvironments` (lines 39-42), and drop `Environment` from the type import on line 2.

- [ ] **Step 7: Update `buildDsn`**

In `dashboard/src/lib/utils/format.ts`, replace the `buildDsn` block:

```ts
/**
 * Build the ingest DSN for an environment:
 * `http(s)://<public_key>@<ingest_host>/<environment_id>`.
 *
 * The ingest edge authenticates on the key alone and discards this path segment,
 * so the id is documentation rather than routing — but it should name the thing
 * the key actually belongs to.
 */
export function buildDsn(publicKey: string, environmentId: string): string {
  try {
    const u = new URL(ingestBaseUrl);
    return `${u.protocol}//${publicKey}@${u.host}/${environmentId}`;
  } catch {
    // A path form with no userinfo is unparseable by every SDK, so fall back to
    // the same shape and let the malformed host surface as a connection error
    // rather than a silently-wrong DSN.
    return `${ingestBaseUrl}/${publicKey}@${environmentId}`;
  }
}
```

- [ ] **Step 8: Verify no stale callers remain**

Run:
```bash
cd dashboard && grep -rn "rotateAppKey\|public_key" src/ | grep -v "environments.ts"
```
Expected: hits only in `pages/SettingsApp.svelte`, `pages/Onboarding.svelte` and `pages/Docs.svelte`, all fixed in Tasks 7 and 8.

Run: `cd dashboard && npx tsc --noEmit`
Expected: errors only in those three pages.

---

## Task 7: Environments management card

**Files:**
- Create: `dashboard/src/lib/components/settings/EnvironmentsCard.svelte`
- Modify: `dashboard/src/pages/SettingsApp.svelte`

**Interfaces:**
- Consumes: Task 6's API client and `Environment` type.
- Produces: `<EnvironmentsCard appId={string} />`, self-loading and self-refreshing.

- [ ] **Step 1: Create the card**

Create `dashboard/src/lib/components/settings/EnvironmentsCard.svelte`:

```svelte
<script lang="ts">
  import Card from '../ui/Card.svelte';
  import Button from '../ui/Button.svelte';
  import Badge from '../ui/Badge.svelte';
  import Icon from '../ui/Icon.svelte';
  import Input from '../ui/Input.svelte';
  import Spinner from '../ui/Spinner.svelte';
  import CopyButton from '../ui/CopyButton.svelte';
  import ConfirmDialog from '../ui/ConfirmDialog.svelte';
  import Modal from '../ui/Modal.svelte';
  import { sessionStore } from '../../stores/session.svelte';
  import { toastStore } from '../../stores/toast.svelte';
  import { errorMessage } from '../../api/client';
  import { buildDsn, relativeTime, formatDateTime } from '../../utils/format';
  import {
    listEnvironments,
    createEnvironment,
    updateEnvironment,
    rotateEnvironmentKey,
    retireEnvironment,
  } from '../../api/environments';
  import type { Environment } from '../../models';

  interface Props {
    appId: string;
  }

  let { appId }: Props = $props();

  let envs = $state<Environment[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let showRetired = $state(false);
  let busyId = $state<string | null>(null);

  let creating = $state(false);
  let newName = $state('');
  let createBusy = $state(false);

  let renaming = $state<Environment | null>(null);
  let renameValue = $state('');

  let confirmRotate = $state<Environment | null>(null);
  let confirmRetire = $state<Environment | null>(null);

  const canCreate = $derived(sessionStore.can('env:create', { app: appId }));
  const canUpdate = $derived(sessionStore.can('env:update', { app: appId }));
  const canRotate = $derived(sessionStore.can('env:rotate_key', { app: appId }));
  const canRetire = $derived(sessionStore.can('env:delete', { app: appId }));

  const active = $derived(envs.filter((e) => !e.retired_at));
  const retired = $derived(envs.filter((e) => e.retired_at));

  async function load() {
    loading = true;
    error = null;
    try {
      envs = await listEnvironments(appId, true);
    } catch (err) {
      error = errorMessage(err);
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    if (appId) void load();
  });

  /** Replace one row in place so the list does not jump while a row is busy. */
  function merge(updated: Environment) {
    envs = envs.map((e) => (e.id === updated.id ? updated : e));
  }

  async function submitCreate() {
    if (createBusy || !newName.trim()) return;
    createBusy = true;
    try {
      const created = await createEnvironment(appId, { name: newName.trim() });
      envs = [...envs, created].sort((a, b) => a.name.localeCompare(b.name));
      newName = '';
      creating = false;
      toastStore.success(`Environment "${created.name}" created.`);
    } catch (err) {
      toastStore.error(errorMessage(err));
    } finally {
      createBusy = false;
    }
  }

  async function submitRename() {
    const target = renaming;
    if (!target || !renameValue.trim()) return;
    busyId = target.id;
    try {
      merge(await updateEnvironment(target.id, { name: renameValue.trim() }));
      renaming = null;
      toastStore.success('Environment renamed.');
    } catch (err) {
      toastStore.error(errorMessage(err));
    } finally {
      busyId = null;
    }
  }

  async function toggleIngest(env: Environment) {
    busyId = env.id;
    try {
      merge(await updateEnvironment(env.id, { ingest_enabled: !env.ingest_enabled }));
      toastStore.success(env.ingest_enabled ? 'Ingest muted.' : 'Ingest resumed.');
    } catch (err) {
      toastStore.error(errorMessage(err));
    } finally {
      busyId = null;
    }
  }

  async function promote(env: Environment) {
    busyId = env.id;
    try {
      await updateEnvironment(env.id, { is_default: true });
      // The previous default also changed, so reload rather than merging one row.
      await load();
      toastStore.success(`"${env.name}" is now the default environment.`);
    } catch (err) {
      toastStore.error(errorMessage(err));
    } finally {
      busyId = null;
    }
  }

  async function doRotate() {
    const target = confirmRotate;
    if (!target) return;
    busyId = target.id;
    try {
      merge(await rotateEnvironmentKey(target.id));
      confirmRotate = null;
      toastStore.success('Key rotated. Update this environment’s DSN everywhere.');
    } catch (err) {
      toastStore.error(errorMessage(err));
    } finally {
      busyId = null;
    }
  }

  async function doRetire() {
    const target = confirmRetire;
    if (!target) return;
    busyId = target.id;
    try {
      merge(await retireEnvironment(target.id));
      confirmRetire = null;
      toastStore.success(`"${target.name}" retired. Its data stays queryable.`);
    } catch (err) {
      toastStore.error(errorMessage(err));
    } finally {
      busyId = null;
    }
  }
</script>

<Card title="Environments">
  {#snippet actions()}
    {#if canCreate}
      <Button variant="secondary" size="sm" onclick={() => (creating = true)}>
        New environment
      </Button>
    {/if}
  {/snippet}

  {#if loading}
    <Spinner />
  {:else if error}
    <p class="err">{error}</p>
  {:else}
    <ul class="env-list">
      {#each active as env (env.id)}
        <li class="env" class:muted-row={!env.ingest_enabled}>
          <div class="head">
            <span class="name">{env.name}</span>
            {#if env.is_default}<Badge tone="info" size="sm">Default</Badge>{/if}
            {#if !env.ingest_enabled}<Badge tone="warning" size="sm">Muted</Badge>{/if}
            <span class="when muted" title={formatDateTime(env.created_at)}>
              created {relativeTime(env.created_at)}
            </span>
          </div>

          <div class="dsn">
            <code>{buildDsn(env.public_key, env.id)}</code>
            <CopyButton value={buildDsn(env.public_key, env.id)} />
          </div>

          <div class="row-actions">
            {#if canUpdate}
              <Button
                variant="ghost"
                size="sm"
                disabled={busyId === env.id}
                onclick={() => {
                  renaming = env;
                  renameValue = env.name;
                }}
              >
                Rename
              </Button>
              <Button
                variant="ghost"
                size="sm"
                disabled={busyId === env.id}
                onclick={() => toggleIngest(env)}
              >
                {env.ingest_enabled ? 'Mute ingest' : 'Resume ingest'}
              </Button>
              {#if !env.is_default}
                <Button
                  variant="ghost"
                  size="sm"
                  disabled={busyId === env.id}
                  onclick={() => promote(env)}
                >
                  Make default
                </Button>
              {/if}
            {/if}
            {#if canRotate}
              <Button
                variant="ghost"
                size="sm"
                disabled={busyId === env.id}
                onclick={() => (confirmRotate = env)}
              >
                Rotate key
              </Button>
            {/if}
            {#if canRetire && !env.is_default && active.length > 1}
              <Button
                variant="ghost"
                size="sm"
                disabled={busyId === env.id}
                onclick={() => (confirmRetire = env)}
              >
                Retire
              </Button>
            {/if}
          </div>
        </li>
      {/each}
    </ul>

    {#if retired.length > 0}
      <button class="toggle-retired" onclick={() => (showRetired = !showRetired)}>
        <Icon name={showRetired ? 'chevron-down' : 'chevron-right'} size={14} />
        {retired.length} retired
      </button>
      {#if showRetired}
        <ul class="env-list retired">
          {#each retired as env (env.id)}
            <li class="env">
              <div class="head">
                <span class="name">{env.name}</span>
                <Badge tone="neutral" size="sm">Retired</Badge>
                <span class="when muted" title={formatDateTime(env.retired_at ?? '')}>
                  retired {relativeTime(env.retired_at ?? '')}
                </span>
              </div>
              <p class="muted note">
                Ingest is off and its key no longer works. Existing data stays queryable.
              </p>
            </li>
          {/each}
        </ul>
      {/if}
    {/if}
  {/if}
</Card>

<Modal bind:open={creating} title="New environment" size="sm">
  <Input
    label="Name"
    bind:value={newName}
    placeholder="staging"
    hint="Lowercase and short works best — this appears in every filter."
  />
  {#snippet footer()}
    <Button variant="secondary" onclick={() => (creating = false)}>Cancel</Button>
    <Button loading={createBusy} disabled={!newName.trim()} onclick={submitCreate}>Create</Button>
  {/snippet}
</Modal>

<Modal
  open={renaming !== null}
  title="Rename environment"
  size="sm"
  onclose={() => (renaming = null)}
>
  <Input label="Name" bind:value={renameValue} />
  {#snippet footer()}
    <Button variant="secondary" onclick={() => (renaming = null)}>Cancel</Button>
    <Button disabled={!renameValue.trim()} onclick={submitRename}>Save</Button>
  {/snippet}
</Modal>

<ConfirmDialog
  open={confirmRotate !== null}
  title="Rotate ingest key?"
  message={`Anything reporting to "${confirmRotate?.name ?? ''}" stops until its DSN is updated. There is no grace period.`}
  confirmLabel="Rotate"
  onconfirm={doRotate}
  oncancel={() => (confirmRotate = null)}
/>

<ConfirmDialog
  open={confirmRetire !== null}
  title="Retire environment?"
  message={`"${confirmRetire?.name ?? ''}" stops accepting events and leaves the picker. Its existing data stays queryable and is archived to cold storage on the normal schedule. This cannot be undone.`}
  confirmLabel="Retire"
  destructive
  onconfirm={doRetire}
  oncancel={() => (confirmRetire = null)}
/>

<style>
  .env-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
  }
  .env {
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 0.75rem;
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }
  .muted-row {
    opacity: 0.7;
  }
  .head {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    flex-wrap: wrap;
  }
  .name {
    font-weight: 600;
  }
  .when {
    margin-left: auto;
    font-size: 0.8rem;
  }
  .dsn {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    min-width: 0;
  }
  .dsn code {
    flex: 1;
    min-width: 0;
    overflow-x: auto;
    white-space: nowrap;
    background: var(--surface-2);
    border-radius: var(--radius-sm);
    padding: 0.35rem 0.5rem;
    font-size: 0.8rem;
  }
  .row-actions {
    display: flex;
    gap: 0.25rem;
    flex-wrap: wrap;
  }
  .toggle-retired {
    margin-top: 0.75rem;
    display: flex;
    align-items: center;
    gap: 0.35rem;
    background: none;
    border: none;
    color: var(--text-muted);
    cursor: pointer;
    font-size: 0.85rem;
    padding: 0;
  }
  .retired {
    margin-top: 0.5rem;
  }
  .note {
    font-size: 0.8rem;
    margin: 0;
  }
  .err {
    color: var(--danger, crimson);
  }
</style>
```

Check `Card`'s `actions`, `Modal`'s `footer`, and `ConfirmDialog`'s prop names against their current definitions in `dashboard/src/lib/components/ui/` and adjust the call sites if they differ — those components predate this task and are the source of truth.

- [ ] **Step 2: Wire it into the settings page**

In `dashboard/src/pages/SettingsApp.svelte`:
- import `EnvironmentsCard` from `../lib/components/settings/EnvironmentsCard.svelte`
- delete the `rotateAppKey` and `listEnvironments` imports, the `environments`/`rotating`/`confirmRotate` state, the `canRotate` derived, the `doRotate` function, the public-key card and its "Regenerate public key" button, and the old `Environments` card (lines 210-226)
- delete `dsn` and `snippet` deriveds that read `app.public_key`; the DSN now lives per environment inside the new card
- render `{#if app}<EnvironmentsCard appId={app.id} />{/if}` where the old Environments card was
- simplify `load` to fetch only the app:

```ts
  async function load(appId: string) {
    loading = true;
    error = null;
    try {
      app = await getApp(appId);
    } catch (err) {
      error = errorMessage(err);
    } finally {
      loading = false;
    }
  }
```

- [ ] **Step 3: Typecheck and build**

Run: `cd dashboard && npx tsc --noEmit && npm run build`
Expected: PASS.

- [ ] **Step 4: Verify in the browser**

Start the dev server with `preview_start`, navigate to the app settings page, then confirm with `preview_snapshot`:
- the `dev` environment renders with a `Default` badge and a DSN of the form `http://pk_…@host/<uuid>`
- "New environment" creates one that appears in the list
- "Mute ingest" flips the row to a `Muted` badge
- "Make default" moves the badge and the previous default loses it
- the retire button is absent on the default row and on the only remaining row

Check `preview_console_logs` for errors after each interaction.

---

## Task 8: Onboarding and Docs DSN

**Files:**
- Modify: `dashboard/src/pages/Onboarding.svelte`
- Modify: `dashboard/src/pages/Docs.svelte`

**Interfaces:**
- Consumes: Task 6's `listEnvironments` and `buildDsn`.
- Produces: nothing downstream.

- [ ] **Step 1: Update Onboarding**

In `dashboard/src/pages/Onboarding.svelte`, add `import { listEnvironments } from '../lib/api/environments';` and a state holder:

```ts
  let defaultEnv = $state<Environment | null>(null);
```

After the app is created, load its seeded environment:

```ts
  async function loadDefaultEnv(appId: string) {
    try {
      const envs = await listEnvironments(appId);
      defaultEnv = envs.find((e) => e.is_default) ?? envs[0] ?? null;
    } catch {
      defaultEnv = null;
    }
  }
```

Call `await loadDefaultEnv(created.id)` immediately after `createApp` resolves, and replace the `dsn` derived:

```ts
  const dsn = $derived(defaultEnv ? buildDsn(defaultEnv.public_key, defaultEnv.id) : '');
```

Guard the DSN step's markup on `dsn` being non-empty so an in-flight environment fetch renders the spinner rather than a broken half-DSN.

- [ ] **Step 2: Update Docs**

`dashboard/src/pages/Docs.svelte` builds its snippets from `app.public_key` and `app.id` around lines 19-20. Replace that with the same environment-sourced pair:

```ts
  import { listEnvironments } from '../lib/api/environments';
  import type { Environment } from '../lib/models';

  let defaultEnv = $state<Environment | null>(null);

  $effect(() => {
    const aid = sessionStore.currentAppId;
    if (!aid) return;
    void (async () => {
      try {
        const envs = await listEnvironments(aid);
        defaultEnv = envs.find((e) => e.is_default) ?? envs[0] ?? null;
      } catch {
        defaultEnv = null;
      }
    })();
  });

  const dsn = $derived(defaultEnv ? buildDsn(defaultEnv.public_key, defaultEnv.id) : '');
```

Every snippet that interpolated the old `dsn` keeps working unchanged. Add a one-line note above the snippet block so a reader knows which DSN they are being handed:

```svelte
<p class="muted">
  Showing the DSN for the <strong>{defaultEnv?.name ?? 'default'}</strong> environment.
  Each environment has its own — see Settings → Environments.
</p>
```

- [ ] **Step 3: Typecheck and build**

Run: `cd dashboard && npx tsc --noEmit && npm run build`
Expected: PASS with no remaining references to `public_key` on `App`.

Run: `cd dashboard && grep -rn "\.public_key" src/`
Expected: hits only where the value is an `Environment`.

- [ ] **Step 4: Verify onboarding in the browser**

Create a fresh project and app through the onboarding flow with `preview_click`/`preview_fill`, then `preview_snapshot` the DSN step. Expected: a DSN whose path segment is a UUID that matches the `dev` environment's id, not the app id.

---

## Task 9: SDKs drop `environment` and go to 1.1.0

**Files:**
- Modify: `sdks/js/src/types.ts:196,256,290`, `sdks/js/src/client.ts:142,275`, `sdks/js/package.json`
- Modify: `sdks/node/src/types.ts:199,266,313`, `sdks/node/src/client.ts:31,51,134`, `sdks/node/src/transport.ts:23,160`, `sdks/node/package.json`
- Modify: `sdks/python/sauron/__init__.py:83,138`, `sdks/python/sauron/_client.py:50,72,130`, `sdks/python/pyproject.toml`
- Modify: `sdks/flutter/lib/src/sauron_options.dart:22-23`, `sdks/flutter/lib/src/envelope.dart:16,28-29,44`, `sdks/flutter/lib/src/client.dart:343`, `sdks/flutter/pubspec.yaml`
- Modify: `sdks/csharp/Sauron/SauronClient.cs:14-15`, `sdks/csharp/Sauron/Envelope.cs:34,42`, `sdks/csharp/Sauron/Transport.cs:166`, `sdks/csharp/Sauron/Sauron.csproj:10`
- Modify: all five `CHANGELOG.md` files

**Interfaces:**
- Consumes: Task 5's envelope contract (no `environment` field).
- Produces: five SDKs at `1.1.0` that never send an environment.

- [ ] **Step 1: Remove the option and header field from each SDK**

For each SDK, delete every site listed in the Files block above: the option declaration in the public options type, the resolved-options field, the default value, the envelope-header field, and the line that writes it. Do not leave a deprecated no-op field — the point of the change is that the wire has no client-supplied environment.

The C# SDK shows the full shape of the edit; the other four are the same five deletions against their own idioms.

`sdks/csharp/Sauron/Envelope.cs:29-36` — delete the `Environment` property:

```csharp
internal sealed class EnvelopeHeader
{
    public string? Dsn { get; set; }
    public SdkInfo Sdk { get; set; } = new();
    public string SentAt { get; set; } = string.Empty;
    public string? Release { get; set; }
}
```

`sdks/csharp/Sauron/Transport.cs:159-171` — delete the assignment:

```csharp
    private Envelope BuildEnvelope(List<object> batch) => new()
    {
        Header = new EnvelopeHeader
        {
            Dsn = _dsn.Raw,
            Sdk = new SdkInfo { Name = SauronSdkMeta.Name, Version = SauronSdkMeta.Version },
            SentAt = Iso8601Now(),
            Release = _options.Release,
        },
        Context = _context,
        Items = batch,
    };
```

`sdks/csharp/Sauron/SauronClient.cs:14-15` — delete the `Environment` option property and its `= "production"` default.

For C#, also bump `SauronSdkMeta.Version` (`sdks/csharp/Sauron/Envelope.cs:42`) from `"0.3.0"` to `"1.1.0"` so the wire identity matches the package version; the other four SDKs carry their version in a single constant already kept in step with the manifest — verify each and update it.

- [ ] **Step 2: Set every version to 1.1.0**

- `sdks/js/package.json` → `"version": "1.1.0"`
- `sdks/node/package.json` → `"version": "1.1.0"`
- `sdks/python/pyproject.toml` → `version = "1.1.0"`
- `sdks/flutter/pubspec.yaml` → `version: 1.1.0`
- `sdks/csharp/Sauron/Sauron.csproj` → `<Version>1.1.0</Version>` (up from `0.3.0`, bringing it into lockstep for the first time)

- [ ] **Step 3: Write the CHANGELOG entries**

Add this to the top of each SDK's `CHANGELOG.md`, under a `## 1.1.0` heading, leading with the breaking change:

```md
## 1.1.0

- **Breaking: the `environment` option has been removed.** An environment is now
  identified by the ingest key it belongs to, not by a string the client sends.
  Create environments in the dashboard under app settings; each one has its own
  DSN. Delete `environment` from your `init` call and swap in the DSN of the
  environment you want to report to.
```

- [ ] **Step 4: Run each SDK's test suite**

```bash
cd sdks/js && npm test
cd sdks/node && npm test
cd sdks/python && python -m pytest
cd sdks/flutter && dart test
cd sdks/csharp && dotnet test
```
Expected: all PASS. Tests asserting the envelope header contains `environment` must be updated to assert its **absence** — that is the behaviour being locked in, not an inconvenience to work around.

- [ ] **Step 5: Verify the option is gone**

Run: `grep -rn "environment" sdks/*/src sdks/python/sauron sdks/flutter/lib sdks/csharp/Sauron --include=* -i | grep -vi "System.Environment\|environment variable"`
Expected: no output.

---

## Task 10: Examples and wiki

**Files:**
- Modify: `examples/node-server/index.ts:67`, `examples/python-server/main.py:85`, `examples/csharp-server/Program.cs:27`, `examples/flutter-app/lib/main.dart` (env config plumbing), `examples/svelte-web/src/lib/sauron.ts:135,154` + `store.svelte.ts` + `components/Header.svelte:54-55`, and **`sdks/flutter/example/lib/main.dart:12`** — the SDK's own bundled example still passes `environment:` to `SauronOptions` and will no longer compile
- Modify: `wiki/Ingest-Wire-Contract.md:14,20,25,66`, `wiki/Getting-Started.md:21,27,58,69,81,90,99`, `wiki/Browser-SDK.md:31,45`, `wiki/Node-SDK.md:28,44`, `wiki/Python-SDK.md:41`, `wiki/Flutter-SDK.md:48,62`, `wiki/Framework-Integrations.md:39,186`, `wiki/Architecture.md:241`

**Interfaces:**
- Consumes: Task 9's SDK surface.
- Produces: nothing downstream.

- [ ] **Step 1: Strip `environment` from the example apps**

Remove the option from each `init` call. For the two examples that expose it as an editable demo control — `examples/svelte-web` (a text input in `Header.svelte` bound to `store.svelte.ts`) and `examples/flutter-app` (a `TextEditingController` in `main.dart`) — remove the input, the persisted field, and the plumbing. These controls existed to demonstrate an option that no longer exists; leaving them would teach the wrong model.

- [ ] **Step 2: Correct the DSN documentation**

In `wiki/Ingest-Wire-Contract.md` replace lines 11-26:

```md
## DSN

```
https://<public_key>@<host>/<environment_id>
```

- `<public_key>` — a **non-secret, write-only** credential (the URL "user" part). Safe
  to embed in client code. A DSN **must not** contain a password/secret component.
  The key identifies exactly one environment, and therefore one app, project and org.
- `<host>` — `host:port` of the ingest gateway (`https` or `http`).
- `<environment_id>` — the environment's UUID. Informational: the gateway
  authenticates on the key alone and does not read this segment.

## Endpoint

```
POST {protocol}://{host}/api/{environment_id}/envelope
```
```

Delete the `"environment": "production",` line from the envelope example at line 66.

Apply the same `<project_id>` → `<environment_id>` correction to `wiki/Getting-Started.md` lines 21, 27, 58, 69, 90, 99, and delete `environment: 'production'` from line 81.

- [ ] **Step 3: Update the five SDK READMEs**

Task 9 removed the `environment` option from all five SDKs but left their `README.md` files
documenting it — the READMEs were in neither task's Files list. Remove the option from every
init snippet and options table in `sdks/{js,node,python,flutter,csharp}/README.md`.

Two stale-version docs were deliberately flagged rather than blanket-edited during the
version alignment; fix them here:
- `sdks/flutter/README.md:646` still documents `kSauronSdkVersion` as `1.0.0`.
- `sdks/PUBLISHING.md`'s version table still lists js/node/python at `1.0.0`.

All five SDKs are at **1.2.0** (Flutter was already there; the other four were brought up to
match so the family stays in lockstep). Any version shown in a README install example or a
docs table must read 1.2.0.

- [ ] **Step 4: Update the per-SDK option tables**

Delete the `environment` row from the options table in `wiki/Browser-SDK.md`, `wiki/Node-SDK.md`, `wiki/Python-SDK.md` and `wiki/Flutter-SDK.md`, and the option from their init snippets. Remove it from the two snippets in `wiki/Framework-Integrations.md`.

In `wiki/Architecture.md:241`, change the envelope description from "a header (SDK, release, environment)" to "a header (SDK, release)".

Add a short section to `wiki/Getting-Started.md` after the DSN block:

```md
### Environments

Every app is created with one environment, `dev`, and each environment has its
own DSN. Add more (`staging`, `production`, …) under **Settings → app →
Environments**, then point each deployment at the matching DSN. The environment a
signal belongs to is determined by the key it arrived with, so it cannot be
spoofed by a client and typos cannot create phantom environments.
```

- [ ] **Step 5: Verify the examples still build**

```bash
cd examples/node-server && npm run build
cd examples/svelte-web && npm run build
cd examples/flutter-app && flutter analyze
cd examples/csharp-server && dotnet build
```
Expected: all succeed.

- [ ] **Step 6: Verify the docs are consistent**

Run: `grep -rn "project_id" wiki/Ingest-Wire-Contract.md wiki/Getting-Started.md`
Expected: no output.

Run: `grep -rn "environment" wiki/*-SDK.md`
Expected: no output referring to the removed init option.

- [ ] **Step 7: Document the upgrade as a hard break**

This slice drops `apps.public_key`, so the previous API binary cannot run against the new schema, and RPM upgrades are known not to re-run `sauron-migrate` on their own. Add to `wiki/Deployment.md` (or the release-notes file the project uses — check `packaging/rpm/` and the repo root for the convention before creating a new one):

```md
### Upgrading to per-app environments

This release moves the ingest key from the app to the environment. It is a
**breaking schema change** — run the migration before starting the new binaries:

This is a **stop-the-world cutover, not a rolling upgrade** — the migration drops
`apps.public_key`, so any still-running old binary 500s on every request regardless.
Drain the queue first, or every in-flight signal is lost:

```bash
# 1. Stop accepting new work, then let the workers finish what is already queued.
sudo systemctl stop sauron-ingest
#    Wait for the stream to empty before continuing:
redis-cli XLEN sauron:ingest:stream     # repeat until 0
sudo systemctl stop sauron-worker sauron-api

# 2. Migrate. RPM upgrades do NOT run this automatically.
sudo -u sauron sauron-migrate

# 3. Start everything together.
sudo systemctl start sauron-api sauron-worker sauron-ingest
```

Two reasons the drain matters. `IngestJob` gained a required `environment_id`, so a job
serialized by the old binary cannot deserialize — the worker dead-letters it to
`sauron:ingest:dlq`, which **nothing reads and nothing trims**. Those signals are gone
from the product even though the SDK already received `202 Accepted`. And `api` and
`ingest` must move as a unit: the DSN cache prefix changes to `sauron:dsn:v2:`, so an old
API invalidating a rotated key writes to a slot the new ingest no longer reads, leaving a
revoked key live for up to the 300s TTL.

Every deployed SDK stops reporting until its DSN is replaced with an environment
DSN, found under **Settings → app → Environments**. Existing environments are
preserved and each is issued a key; the app's old key is gone and cannot be
recovered.
```

---

## Task 11: Live end-to-end verification

**Files:** none — this task changes nothing and exists to prove the slice works.

**Interfaces:**
- Consumes: everything.
- Produces: an observed, recorded pass.

- [ ] **Step 1: Bring up the full stack**

```bash
cd /home/splimter/projects/freelance/sauron
docker compose up -d --build
```
Wait for `api`, `ingest`, `db` and `redis` to report healthy.

- [ ] **Step 2: Confirm a new app is born with `dev`**

Create a project and app through the dashboard. Expected: the app settings page shows exactly one environment, `dev`, badged `Default`, with a DSN.

Record its key: `export DEV_KEY=<the pk_… from the DSN>`

- [ ] **Step 3: Send an event and confirm the environment is stamped**

```bash
curl -sS -X POST "http://localhost:8081/api/00000000-0000-0000-0000-000000000000/envelope" \
  -H "x-sauron-key: $DEV_KEY" -H 'content-type: application/json' \
  -d '{"header":{"sdk":{"name":"manual","version":"0"},"environment":"production"},
       "items":[{"type":"event","name":"verify.env","properties":{}}]}'
```
Expected: `{"accepted":1}`.

Note the deliberately hostile `"environment":"production"` in that body. Then:

```bash
psql "$DATABASE_URL" -c "
SELECT e.name FROM analytics_events ev
JOIN environments e ON e.id = ev.environment_id
WHERE ev.name = 'verify.env' ORDER BY ev.occurred_at DESC LIMIT 1;"
```
Expected: `dev` — **not** `production`. This is the whole point of the slice: the client's claim is ignored and the key decides.

- [ ] **Step 4: Mute and confirm 403**

Click "Mute ingest" on `dev`, then repeat the curl from Step 3.
Expected: HTTP 403 with `{"error":{"code":"ingest_disabled",...}}`. Resume ingest and confirm 202 returns.

- [ ] **Step 5: Rotate and confirm the old key dies**

Click "Rotate key", then repeat the curl with the **old** `$DEV_KEY`.
Expected: HTTP 401 `invalid_key`. Repeat with the new key: `{"accepted":1}`.

If the old key still works, the Redis invalidation did not fire — check that `keys::dsn_cache` was bumped to `v2` (Task 5 Step 1) and that the rotate handler deletes the slot for the **old** key.

- [ ] **Step 6: Add a second environment and confirm separation**

Create `prod`, send an event with its key, then:

```bash
psql "$DATABASE_URL" -c "
SELECT e.name, count(*) FROM analytics_events ev
JOIN environments e ON e.id = ev.environment_id
GROUP BY e.name ORDER BY e.name;"
```
Expected: separate counts for `dev` and `prod`.

- [ ] **Step 7: Confirm the retire guards**

- Try to retire `dev` while it is default. Expected: 409, surfaced as a toast.
- Promote `prod` to default, then retire `dev`. Expected: success; `dev` moves to the collapsed "retired" section.
- Send an event with the retired `dev` key. Expected: HTTP 401 `invalid_key`.
- Confirm the events sent to `dev` in Step 3 are still visible in the dashboard — retire preserves data.

- [ ] **Step 8: Confirm an SDK round-trip**

Point `examples/node-server` at the `prod` DSN, run it, trigger an error, and confirm the issue appears in the dashboard attributed to `prod`. This proves the real SDK path, not just hand-rolled curl.

- [ ] **Step 9: Full gate**

```bash
cd backend && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
cd ../dashboard && npx tsc --noEmit && npm test && npm run build
```
Expected: all green.
