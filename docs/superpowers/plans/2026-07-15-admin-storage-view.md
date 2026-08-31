# Admin Storage & Records View — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give a deployment operator ("global admin") an admin-gated API + dashboard page showing total Postgres DB size, per-app record counts (hot in Postgres + cold in Parquet), estimated per-app hot bytes, and the cold Parquet files (name + size).

**Architecture:** A new global-admin (`users.is_admin`) concept, bootstrapped by first-registered-user (+ retroactive earliest-user in the migration). `GET /v1/admin/storage` (gated by a new `require_admin`) aggregates, concurrently, Postgres queries + DuckDB per-app Parquet counts + a `/cold` filesystem walk, assembled by `app_id`. A Svelte `Storage` page (admin-only nav) renders it. No new infra — the `api` service already mounts `/cold:ro` and embeds DuckDB.

**Tech Stack:** Rust (axum 0.8, diesel-async over `postgres_backend`, tokio), DuckDB (`duckdb` crate, via the existing `sauron-tier` crate), PostgreSQL 16, Svelte 5 (runes) dashboard.

Design spec: `docs/superpowers/specs/2026-07-15-admin-storage-view-design.md`.

## Global Constraints

- **Workspace conventions (verbatim):** edition `2021`, rust-version `1.82` (crates omit it; workspace sets it), license `AGPL-3.0-only`, `version = "0.1.0"`. Enum-like columns are `TEXT`. All DB I/O goes through `&mut AsyncPgConnection`; repo fns return `QueryResult`. Config is hand-rolled in `sauron-core::config`.
- **No DB/handler integration-test harness** (by design). Pure logic (Task 3: `parse_cold_path`, `DuckEngine::counts_by_app`) gets real `cargo test`. Everything touching Postgres/handlers (Tasks 1, 2, 4, 5) is verified by the **controller's docker-compose e2e** at the checkpoint after Task 5. The dashboard (Task 6) is verified via the preview server. Reviewers get this constraint — do not false-flag missing unit tests for DB/handler code.
- **Diesel Queryable is positional:** a new column added to a `table!` block and its model struct must be appended in the SAME position as the physical DB column (which `ALTER TABLE ADD COLUMN` puts LAST). `is_admin` goes LAST in the `users` `table!` block AND LAST in the `User` struct.
- **Migration numbering** continues the sequence; next free id is `2026-07-15-000014`.
- **Interpolated SQL identifiers** (table names in `pg_total_relation_size`, `GROUP BY` counts) come ONLY from `sauron_tier::TIERED_TABLES` constants, never user input — matches the existing tiering repo pattern.
- **DuckDB is isolated** to `sauron-tier`(+bin) and `sauron-api`. This feature adds no new crate deps (sauron-api already depends on sauron-tier).

---

## Task 1: `is_admin` column + first-user-admin

**Files:**
- Create: `backend/migrations/2026-07-15-000014_users_is_admin/up.sql`
- Create: `backend/migrations/2026-07-15-000014_users_is_admin/down.sql`
- Modify: `backend/crates/sauron-db/src/schema.rs` (users `table!` block)
- Modify: `backend/crates/sauron-db/src/models.rs` (`User`, `NewUser`)
- Modify: `backend/crates/sauron-db/src/repo.rs` (`create_user`)

**Interfaces:**
- Produces: `User.is_admin: bool` (serialized in `/v1/me`); `create_user` sets `is_admin = (user count == 0)`.

- [ ] **Step 1: Migration up.sql**

Create `backend/migrations/2026-07-15-000014_users_is_admin/up.sql`:
```sql
-- Global-admin (superuser) flag. The FIRST registered user becomes admin
-- (handled in repo::create_user). For an already-populated deployment, flag the
-- earliest-created user retroactively so an admin always exists.
ALTER TABLE users ADD COLUMN is_admin BOOLEAN NOT NULL DEFAULT false;
UPDATE users SET is_admin = true
 WHERE id = (SELECT id FROM users ORDER BY created_at ASC LIMIT 1);
```

- [ ] **Step 2: Migration down.sql**

Create `backend/migrations/2026-07-15-000014_users_is_admin/down.sql`:
```sql
ALTER TABLE users DROP COLUMN is_admin;
```

- [ ] **Step 3: schema.rs — add `is_admin` LAST in the users block**

In `backend/crates/sauron-db/src/schema.rs`, add `is_admin -> Bool,` as the LAST column of the `users (id) { … }` `table!` block (after `updated_at`).

- [ ] **Step 4: models.rs — `User` + `NewUser`**

In `backend/crates/sauron-db/src/models.rs`, add to `User` as the LAST field (after `updated_at`):
```rust
    pub is_admin: bool,
```
And add to `NewUser`:
```rust
    pub is_admin: bool,
```

- [ ] **Step 5: repo.rs — first-user-admin in `create_user`**

In `backend/crates/sauron-db/src/repo.rs`, change `create_user` to compute `is_admin` from the current user count and pass it in `NewUser`:
```rust
pub async fn create_user(
    conn: &mut AsyncPgConnection,
    email: &str,
    password_hash: &str,
    name: &str,
) -> QueryResult<User> {
    let email = email.to_lowercase();
    // First user ever becomes the global admin.
    let existing: i64 = users::table.count().get_result(conn).await?;
    let is_admin = existing == 0;
    diesel::insert_into(users::table)
        .values(NewUser {
            email: &email,
            password_hash,
            name,
            is_admin,
        })
        .returning(User::as_returning())
        .get_result(conn)
        .await
}
```
(Keep the rest of the file unchanged. `count`/`get_result` come from the already-imported `diesel::prelude::*` + `diesel_async::RunQueryDsl`.)

- [ ] **Step 6: Compile check**

Run: `cd backend && cargo build -p sauron-db`
Expected: compiles (the `User`/`NewUser`/schema changes line up).

- [ ] **Step 7: Commit**

```bash
git add backend/migrations/2026-07-15-000014_users_is_admin backend/crates/sauron-db/src/schema.rs backend/crates/sauron-db/src/models.rs backend/crates/sauron-db/src/repo.rs
git commit -m "feat(admin): users.is_admin column + first-user-admin"
```

---

## Task 2: `require_admin` gate

**Files:**
- Modify: `backend/crates/sauron-db/src/repo.rs` (`is_user_admin`)
- Modify: `backend/crates/sauron-auth/src/rbac.rs` (`require_admin`)

**Interfaces:**
- Consumes: `User.is_admin` (Task 1).
- Produces: `repo::is_user_admin(conn, user_id) -> QueryResult<bool>`; `sauron_auth::require_admin(conn, user_id) -> Result<(), AuthError>` (returns `AuthError::Forbidden` when not admin; `AuthError::Forbidden`→HTTP 403 via the existing `From<AuthError> for ApiError`).

- [ ] **Step 1: repo `is_user_admin`**

Append to `backend/crates/sauron-db/src/repo.rs` (users section):
```rust
/// Whether the user is a global admin. Missing user ⇒ false.
pub async fn is_user_admin(conn: &mut AsyncPgConnection, user_id: Uuid) -> QueryResult<bool> {
    users::table
        .find(user_id)
        .select(users::is_admin)
        .first(conn)
        .await
        .optional()
        .map(|o| o.unwrap_or(false))
}
```

- [ ] **Step 2: `require_admin` gate**

In `backend/crates/sauron-auth/src/rbac.rs`, add next to `authorize_app`:
```rust
/// Deployment-wide global-admin gate. Re-checks `users.is_admin` fresh each call.
pub async fn require_admin(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
) -> Result<(), AuthError> {
    let is_admin = repo::is_user_admin(conn, user_id)
        .await
        .map_err(|_| AuthError::Internal)?;
    if is_admin {
        Ok(())
    } else {
        Err(AuthError::Forbidden)
    }
}
```
(`repo`, `AuthError`, `AsyncPgConnection`, `Uuid` are already in scope in `rbac.rs` — `authorize_app` uses all of them.)

- [ ] **Step 3: Export check**

Confirm `require_admin` is reachable as `sauron_auth::require_admin` (rbac items are re-exported the same way `authorize_app` is — check the crate root `pub use`/`pub mod` and mirror `authorize_app`'s visibility exactly).

- [ ] **Step 4: Compile check**

Run: `cd backend && cargo build -p sauron-db -p sauron-auth`
Expected: compiles.

- [ ] **Step 5: Commit**

```bash
git add backend/crates/sauron-db/src/repo.rs backend/crates/sauron-auth/src/rbac.rs
git commit -m "feat(admin): require_admin gate + is_user_admin repo fn"
```

---

## Task 3: `sauron-tier` — cold-path parser (TDD) + `counts_by_app`

**Files:**
- Modify: `backend/crates/sauron-tier/src/layout.rs` (`ColdFileKey`, `parse_cold_path`)
- Modify: `backend/crates/sauron-tier/src/lib.rs` (`pub use` the new items)
- Modify: `backend/crates/sauron-tier/src/duck.rs` (`DuckEngine::counts_by_app`)

**Interfaces:**
- Produces: `sauron_tier::{ColdFileKey, parse_cold_path}` where `parse_cold_path(rel: &str) -> Option<ColdFileKey>` and `ColdFileKey { table: String, app_id: Uuid }`; `DuckEngine::counts_by_app(&self, glob: &str) -> anyhow::Result<Vec<(Uuid, i64)>>`.

- [ ] **Step 1: `parse_cold_path` + tests in layout.rs**

Append to `backend/crates/sauron-tier/src/layout.rs`:
```rust
/// The (table, app_id) a cold Parquet file belongs to, parsed from its hive path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColdFileKey {
    pub table: String,
    pub app_id: Uuid,
}

/// Parse a cold-storage path RELATIVE to the base dir, e.g.
/// `error_events/app_id=<uuid>/year=2026/month=5/data_0.parquet` → the table
/// (first segment) and the `app_id=` hive value. Returns None if either is absent
/// or the uuid is unparseable.
pub fn parse_cold_path(rel: &str) -> Option<ColdFileKey> {
    let rel = rel.trim_start_matches('/');
    let table = rel.split('/').next()?.to_string();
    if table.is_empty() {
        return None;
    }
    let app_seg = rel.split('/').find(|s| s.starts_with("app_id="))?;
    let app_id = Uuid::parse_str(app_seg.strip_prefix("app_id=")?).ok()?;
    Some(ColdFileKey { table, app_id })
}

#[cfg(test)]
mod cold_path_tests {
    use super::*;

    #[test]
    fn parses_table_and_app_id() {
        let app = Uuid::new_v4();
        let rel = format!("error_events/app_id={app}/year=2026/month=5/data_0.parquet");
        assert_eq!(
            parse_cold_path(&rel),
            Some(ColdFileKey { table: "error_events".to_string(), app_id: app })
        );
    }

    #[test]
    fn leading_slash_is_tolerated() {
        let app = Uuid::new_v4();
        let rel = format!("/transactions/app_id={app}/year=2026/month=1/x.parquet");
        assert_eq!(parse_cold_path(&rel).unwrap().table, "transactions");
    }

    #[test]
    fn rejects_missing_app_id_or_empty() {
        assert_eq!(parse_cold_path("error_events/year=2026/month=5/x.parquet"), None);
        assert_eq!(parse_cold_path(""), None);
        assert_eq!(parse_cold_path("error_events/app_id=not-a-uuid/x.parquet"), None);
    }
}
```
(`Uuid` is already imported in layout.rs.)

- [ ] **Step 2: Export in lib.rs**

In `backend/crates/sauron-tier/src/lib.rs`, extend the layout re-export line to include the new items:
```rust
pub use layout::{
    bucket_bounds, cold_copy_dir, cold_partition_glob, parse_cold_path, partition_suffix,
    ColdFileKey, Granularity,
};
```

- [ ] **Step 3: `counts_by_app` + test in duck.rs**

Append this method to `impl DuckEngine` in `backend/crates/sauron-tier/src/duck.rs`:
```rust
    /// Per-app row counts across the Parquet matched by `glob` (all apps in one
    /// query). `app_id` is read from the hive path as text, so we parse it back to
    /// Uuid. Returns empty when no files match.
    pub fn counts_by_app(&self, glob: &str) -> anyhow::Result<Vec<(Uuid, i64)>> {
        if !self.any_files_match(glob)? {
            return Ok(Vec::new());
        }
        let sql = "SELECT app_id, count(*) FROM read_parquet(?, hive_partitioning=true, union_by_name=true) GROUP BY app_id";
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map([glob], |r| {
            let app: String = r.get(0)?;
            let n: i64 = r.get(1)?;
            Ok((app, n))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (app, n) = row?;
            if let Ok(id) = Uuid::parse_str(&app) {
                out.push((id, n));
            }
        }
        Ok(out)
    }
```
Add to the `#[cfg(test)] mod tests` in duck.rs (reuses the same write pattern as `write_then_read_counts_by_day`):
```rust
    #[test]
    fn counts_by_app_groups_two_apps() {
        let dir = std::env::temp_dir().join(format!("sauron-tier-cba-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let a1 = Uuid::new_v4();
        let a2 = Uuid::new_v4();
        let eng = DuckEngine::open().unwrap();
        let copy = format!(
            "COPY (SELECT app_id, occurred_at, year(occurred_at) AS year, month(occurred_at) AS month FROM (VALUES \
               ('{a1}'::UUID, TIMESTAMPTZ '2026-05-01 10:00:00+00'), \
               ('{a1}'::UUID, TIMESTAMPTZ '2026-05-02 10:00:00+00'), \
               ('{a2}'::UUID, TIMESTAMPTZ '2026-05-01 11:00:00+00') \
             ) AS v(app_id, occurred_at)) \
             TO '{d}/error_events' (FORMAT PARQUET, PARTITION_BY (app_id, year, month), APPEND)",
            a1 = a1, a2 = a2, d = dir.display()
        );
        eng.conn.execute_batch(&copy).unwrap();
        let glob = format!("{}/error_events/**/*.parquet", dir.display());
        let mut counts = eng.counts_by_app(&glob).unwrap();
        counts.sort_by_key(|(_, n)| *n);
        assert_eq!(counts.len(), 2);
        assert_eq!(counts.iter().map(|(_, n)| *n).sum::<i64>(), 3);
        assert!(counts.iter().any(|(id, n)| *id == a1 && *n == 2));
        assert!(counts.iter().any(|(id, n)| *id == a2 && *n == 1));
        std::fs::remove_dir_all(&dir).ok();
    }
```

- [ ] **Step 4: Run tests**

Run: `cd backend && cargo test -p sauron-tier --lib`
Expected: PASS — the existing 16 tests plus the 4 new ones (`cold_path_tests::*` ×3, `counts_by_app_groups_two_apps` ×1) = 20.

- [ ] **Step 5: Commit**

```bash
git add backend/crates/sauron-tier/src/layout.rs backend/crates/sauron-tier/src/lib.rs backend/crates/sauron-tier/src/duck.rs
git commit -m "feat(admin): cold-path parser + DuckDB per-app counts"
```

---

## Task 4: repo storage queries

**Files:**
- Modify: `backend/crates/sauron-db/src/repo.rs` (append a "Storage (admin)" section)

**Interfaces:**
- Produces:
  - `db_total_bytes(conn) -> QueryResult<i64>`
  - `table_total_bytes(conn, table: &str) -> QueryResult<i64>`
  - `table_avg_row_width(conn, table: &str) -> QueryResult<i64>`
  - `hot_rows_by_app(conn, table: &str) -> QueryResult<Vec<AppCountRow>>` where `AppCountRow { app_id: Uuid, n: i64 }`
  - `list_apps_with_org(conn) -> QueryResult<Vec<AppOrgRow>>` where `AppOrgRow { app_id: Uuid, app_name: String, org_name: String }`

- [ ] **Step 1: Add the queries**

Append to `backend/crates/sauron-db/src/repo.rs`:
```rust
// ===========================================================================
// Storage (admin) — sizes and per-app row counts. `table` args are internal
// identifiers from sauron_tier::TIERED_TABLES, never user input.
// ===========================================================================

#[derive(diesel::QueryableByName)]
struct BytesRow {
    #[diesel(sql_type = BigInt)]
    bytes: i64,
}

pub async fn db_total_bytes(conn: &mut AsyncPgConnection) -> QueryResult<i64> {
    let row: BytesRow = diesel::sql_query(
        "SELECT pg_database_size(current_database())::bigint AS bytes",
    )
    .get_result(conn)
    .await?;
    Ok(row.bytes)
}

pub async fn table_total_bytes(conn: &mut AsyncPgConnection, table: &str) -> QueryResult<i64> {
    let row: BytesRow = diesel::sql_query(format!(
        "SELECT pg_total_relation_size('{table}'::regclass)::bigint AS bytes"
    ))
    .get_result(conn)
    .await?;
    Ok(row.bytes)
}

pub async fn table_avg_row_width(conn: &mut AsyncPgConnection, table: &str) -> QueryResult<i64> {
    let row: BytesRow = diesel::sql_query(
        "SELECT COALESCE(sum(avg_width), 0)::bigint AS bytes FROM pg_stats WHERE tablename = $1",
    )
    .bind::<Text, _>(table)
    .get_result(conn)
    .await?;
    Ok(row.bytes)
}

#[derive(diesel::QueryableByName)]
pub struct AppCountRow {
    #[diesel(sql_type = SqlUuid)]
    pub app_id: Uuid,
    #[diesel(sql_type = BigInt)]
    pub n: i64,
}

pub async fn hot_rows_by_app(
    conn: &mut AsyncPgConnection,
    table: &str,
) -> QueryResult<Vec<AppCountRow>> {
    diesel::sql_query(format!(
        "SELECT app_id, count(*)::bigint AS n FROM {table} GROUP BY app_id"
    ))
    .load(conn)
    .await
}

#[derive(diesel::QueryableByName)]
pub struct AppOrgRow {
    #[diesel(sql_type = SqlUuid)]
    pub app_id: Uuid,
    #[diesel(sql_type = Text)]
    pub app_name: String,
    #[diesel(sql_type = Text)]
    pub org_name: String,
}

pub async fn list_apps_with_org(conn: &mut AsyncPgConnection) -> QueryResult<Vec<AppOrgRow>> {
    diesel::sql_query(
        "SELECT a.id AS app_id, a.name AS app_name, o.name AS org_name \
         FROM apps a JOIN projects p ON a.project_id = p.id \
         JOIN organizations o ON p.org_id = o.id \
         ORDER BY o.name, a.name",
    )
    .load(conn)
    .await
}
```
(`BigInt`, `Text`, `Uuid as SqlUuid` are already imported at the top of repo.rs.)

- [ ] **Step 2: Compile check**

Run: `cd backend && cargo build -p sauron-db`
Expected: compiles.

- [ ] **Step 3: Commit**

```bash
git add backend/crates/sauron-db/src/repo.rs
git commit -m "feat(admin): storage repo queries (db/table size, per-app rows, apps+org)"
```

---

## Task 5: `admin_storage` assembler + `/v1/admin/storage` endpoint  [controller e2e CHECKPOINT]

**Files:**
- Create: `backend/bins/sauron-api/src/admin_storage.rs`
- Create: `backend/bins/sauron-api/src/routes/admin.rs`
- Modify: `backend/bins/sauron-api/src/routes/mod.rs` (add `pub mod admin;`)
- Modify: `backend/bins/sauron-api/src/main.rs` (`mod admin_storage;` + route)

**Interfaces:**
- Consumes: `sauron_tier::{TIERED_TABLES, cold_partition_glob, parse_cold_path, ColdFileKey}`, `sauron_tier::duck::DuckEngine`, `repo::{db_total_bytes, table_total_bytes, table_avg_row_width, hot_rows_by_app, list_apps_with_org}`, `sauron_auth::require_admin`, `AuthUser`, `db`.
- Produces: `admin_storage::collect_storage(state: &AppState) -> anyhow::Result<StorageReport>`; HTTP `GET /v1/admin/storage`.

- [ ] **Step 1: Response types + assembler**

Create `backend/bins/sauron-api/src/admin_storage.rs`:
```rust
//! Admin storage report: total DB size + per-app hot(Postgres)/cold(Parquet)
//! record counts, estimated hot bytes, and the cold Parquet file inventory.
//! Postgres queries, DuckDB per-app counts, and the /cold filesystem walk run
//! concurrently, then are assembled by app_id.

use std::collections::HashMap;
use std::path::Path;

use serde::Serialize;
use uuid::Uuid;

use sauron_db::{conn, repo};
use sauron_tier::duck::DuckEngine;
use sauron_tier::{cold_partition_glob, parse_cold_path, TIERED_TABLES};

use crate::AppState;

#[derive(Serialize)]
pub struct StorageReport {
    pub database: DatabaseInfo,
    pub apps: Vec<AppStorage>,
}

#[derive(Serialize)]
pub struct DatabaseInfo {
    pub total_bytes: i64,
    pub tables: Vec<TableSize>,
}

#[derive(Serialize)]
pub struct TableSize {
    pub name: String,
    pub total_bytes: i64,
    pub hot_rows: i64,
}

#[derive(Serialize)]
pub struct AppStorage {
    pub app_id: Uuid,
    pub app_name: String,
    pub org_name: String,
    pub tables: Vec<AppTableStorage>,
    pub hot_rows_total: i64,
    pub cold_rows_total: i64,
    pub cold_bytes_total: i64,
    pub estimated_hot_bytes_total: i64,
    pub cold_files: Vec<ColdFile>,
}

#[derive(Serialize)]
pub struct AppTableStorage {
    pub name: String,
    pub hot_rows: i64,
    pub cold_rows: i64,
    pub cold_bytes: i64,
    /// Approximate (rows × avg row width from pg_stats).
    pub estimated_hot_bytes: i64,
}

#[derive(Serialize)]
pub struct ColdFile {
    pub path: String,
    pub bytes: i64,
}

/// One cold file found by the /cold walk, keyed to its (table, app_id).
struct WalkedFile {
    table: String,
    app_id: Uuid,
    path: String,
    bytes: i64,
}

pub async fn collect_storage(state: &AppState) -> anyhow::Result<StorageReport> {
    let cold_path = state.cfg.tier_cold_path.clone();

    // --- Postgres branch (async, one connection) ---
    let pool = state.pool.clone();
    let pg = async move {
        let mut c = conn(&pool).await?;
        let total_bytes = repo::db_total_bytes(&mut c).await?;
        let apps = repo::list_apps_with_org(&mut c).await?;
        let mut tables = Vec::new();
        // hot_rows[table][app_id] and avg_width[table]
        let mut hot: HashMap<&'static str, HashMap<Uuid, i64>> = HashMap::new();
        let mut avg_width: HashMap<&'static str, i64> = HashMap::new();
        for t in TIERED_TABLES {
            let size = repo::table_total_bytes(&mut c, t.name).await?;
            let width = repo::table_avg_row_width(&mut c, t.name).await?;
            let rows = repo::hot_rows_by_app(&mut c, t.name).await?;
            let total_hot: i64 = rows.iter().map(|r| r.n).sum();
            tables.push(TableSize { name: t.name.to_string(), total_bytes: size, hot_rows: total_hot });
            hot.insert(t.name, rows.into_iter().map(|r| (r.app_id, r.n)).collect());
            avg_width.insert(t.name, width);
        }
        Ok::<_, anyhow::Error>((total_bytes, tables, apps, hot, avg_width))
    };

    // --- DuckDB branch (blocking): cold rows per (table, app_id) ---
    let cold_path_d = cold_path.clone();
    let cold_counts = tokio::task::spawn_blocking(move || -> anyhow::Result<HashMap<&'static str, HashMap<Uuid, i64>>> {
        let eng = DuckEngine::open()?;
        let mut out: HashMap<&'static str, HashMap<Uuid, i64>> = HashMap::new();
        for t in TIERED_TABLES {
            let glob = format!("{}/{}/**/*.parquet", cold_path_d.trim_end_matches('/'), t.name);
            let counts = eng.counts_by_app(&glob)?;
            out.insert(t.name, counts.into_iter().collect());
        }
        Ok(out)
    });

    // --- Filesystem branch (blocking): cold files per (table, app_id) ---
    let cold_path_w = cold_path.clone();
    let walked = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<WalkedFile>> {
        walk_cold(&cold_path_w)
    });

    let (pg_res, cold_res, walk_res) = tokio::join!(pg, cold_counts, walked);
    let (total_bytes, tables, apps, hot, avg_width) = pg_res?;
    let cold_counts = cold_res??;
    let walked = walk_res??;

    // Group walked files by (app_id, table).
    let mut files_by_app: HashMap<Uuid, Vec<ColdFile>> = HashMap::new();
    let mut cold_bytes: HashMap<(Uuid, &'static str), i64> = HashMap::new();
    for f in walked {
        // Match the walked file's table string to a canonical TIERED_TABLES name.
        if let Some(t) = TIERED_TABLES.iter().find(|t| t.name == f.table) {
            *cold_bytes.entry((f.app_id, t.name)).or_insert(0) += f.bytes;
            files_by_app.entry(f.app_id).or_default().push(ColdFile { path: f.path, bytes: f.bytes });
        }
    }

    let apps_out = apps
        .into_iter()
        .map(|a| {
            let mut per_table = Vec::new();
            let (mut hr, mut cr, mut cb, mut ehb) = (0i64, 0i64, 0i64, 0i64);
            for t in TIERED_TABLES {
                let hot_rows = hot.get(t.name).and_then(|m| m.get(&a.app_id)).copied().unwrap_or(0);
                let cold_rows = cold_counts.get(t.name).and_then(|m| m.get(&a.app_id)).copied().unwrap_or(0);
                let cold_b = cold_bytes.get(&(a.app_id, t.name)).copied().unwrap_or(0);
                let est = avg_width.get(t.name).copied().unwrap_or(0) * hot_rows;
                hr += hot_rows; cr += cold_rows; cb += cold_b; ehb += est;
                per_table.push(AppTableStorage {
                    name: t.name.to_string(),
                    hot_rows,
                    cold_rows,
                    cold_bytes: cold_b,
                    estimated_hot_bytes: est,
                });
            }
            let mut files = files_by_app.remove(&a.app_id).unwrap_or_default();
            files.sort_by(|x, y| x.path.cmp(&y.path));
            AppStorage {
                app_id: a.app_id,
                app_name: a.app_name,
                org_name: a.org_name,
                tables: per_table,
                hot_rows_total: hr,
                cold_rows_total: cr,
                cold_bytes_total: cb,
                estimated_hot_bytes_total: ehb,
                cold_files: files,
            }
        })
        .collect();

    Ok(StorageReport {
        database: DatabaseInfo { total_bytes, tables },
        apps: apps_out,
    })
}

/// Recursively collect `*.parquet` files under `base`, keyed to (table, app_id)
/// via the hive path. Missing base dir ⇒ empty (nothing tiered yet).
fn walk_cold(base: &str) -> anyhow::Result<Vec<WalkedFile>> {
    let base = Path::new(base);
    let mut out = Vec::new();
    if !base.exists() {
        return Ok(out);
    }
    let mut stack = vec![base.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("parquet") {
                let rel = path.strip_prefix(base).ok().and_then(|p| p.to_str()).unwrap_or("");
                if let Some(key) = parse_cold_path(rel) {
                    let bytes = entry.metadata().map(|m| m.len() as i64).unwrap_or(0);
                    out.push(WalkedFile { table: key.table, app_id: key.app_id, path: rel.to_string(), bytes });
                }
            }
        }
    }
    Ok(out)
}
```

- [ ] **Step 2: Handler**

Create `backend/bins/sauron-api/src/routes/admin.rs`:
```rust
//! Admin-only endpoints (global-admin gated).

use axum::extract::State;
use axum::Json;

use sauron_auth::{require_admin, AuthUser};

use super::db;
use crate::admin_storage::{collect_storage, StorageReport};
use crate::error::ApiError;
use crate::AppState;

/// Deployment-wide storage & record report. Global-admin only.
pub async fn storage(
    auth: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<StorageReport>, ApiError> {
    let mut conn = db(&state).await?;
    require_admin(&mut conn, auth.user_id).await?;
    drop(conn); // release before the router checks out its own connection
    let report = collect_storage(&state).await?;
    Ok(Json(report))
}
```
(Match the neighboring route modules for the exact `use super::db;` path and `crate::error::ApiError` — they're identical to `routes/analytics.rs`.)

- [ ] **Step 3: Register module + route**

In `backend/bins/sauron-api/src/routes/mod.rs`, add `pub mod admin;` alongside the other `pub mod` lines.

In `backend/bins/sauron-api/src/main.rs`: add `mod admin_storage;` next to `mod tier_read;`, and add the route in the Router chain (near the other `/v1/...` routes):
```rust
        .route("/v1/admin/storage", get(routes::admin::storage))
```

- [ ] **Step 4: Compile check**

Run: `cd backend && cargo build -p sauron-api`
Expected: compiles (DuckDB already built via the tiering work).

- [ ] **Step 5: Controller e2e checkpoint (stands in for handler tests)**

The controller runs this after review (implementer does NOT run docker). Verifies:
```bash
docker compose down -v && docker compose up -d --build postgres migrate redis api
# Register FIRST user → should be admin; SECOND → not admin.
TOKEN_A=$(curl -s localhost:10000/v1/auth/register -H 'content-type: application/json' \
  -d '{"email":"admin@e2e","password":"pw123456","name":"A"}' | jq -r .access_token)
TOKEN_B=$(curl -s localhost:10000/v1/auth/register -H 'content-type: application/json' \
  -d '{"email":"user@e2e","password":"pw123456","name":"B"}' | jq -r .access_token)
curl -s localhost:10000/v1/me -H "authorization: Bearer $TOKEN_A" | jq .is_admin   # true
curl -s localhost:10000/v1/me -H "authorization: Bearer $TOKEN_B" | jq .is_admin   # false
curl -s -o /dev/null -w '%{http_code}\n' localhost:10000/v1/admin/storage -H "authorization: Bearer $TOKEN_B"  # 403
curl -s localhost:10000/v1/admin/storage -H "authorization: Bearer $TOKEN_A" | jq '.database.total_bytes, (.apps|length)'
# Then seed + tier (reuse the tiering e2e seed) and re-check per-app hot/cold counts + cold_files.
```
Expected: A `is_admin=true`, B `false`, B gets 403, A gets a JSON report with `database.total_bytes > 0` and an `apps` array; after seeding+tiering, the seeded app shows matching hot/cold counts and its `cold_files` list.

- [ ] **Step 6: Commit**

```bash
git add backend/bins/sauron-api/src/admin_storage.rs backend/bins/sauron-api/src/routes/admin.rs backend/bins/sauron-api/src/routes/mod.rs backend/bins/sauron-api/src/main.rs
git commit -m "feat(admin): /v1/admin/storage endpoint + cross-source assembler"
```

---

## Task 6: Dashboard — admin-only Storage page

**Files:**
- Modify: `dashboard/src/lib/models/index.ts` (add `is_admin` to `User`)
- Create: `dashboard/src/lib/api/admin.ts` (`getAdminStorage` + types)
- Create: `dashboard/src/pages/Storage.svelte`
- Modify: `dashboard/src/routes.ts` (add `/storage` route)
- Modify: `dashboard/src/lib/components/layout/Sidebar.svelte` (admin-gated nav link)

**Interfaces:**
- Consumes: `GET /v1/admin/storage`, the auth store's current user `is_admin`.

- [ ] **Step 1: Add `is_admin` to the `User` model**

In `dashboard/src/lib/models/index.ts`, add `is_admin: boolean;` to the `User` interface (match the field the backend now returns in `/v1/me`).

- [ ] **Step 2: Admin API client + types**

Create `dashboard/src/lib/api/admin.ts` mirroring an existing client (e.g. `monitors.ts`) — use the shared `api` client from `./client`:
```ts
import { api } from './client';

export interface ColdFile { path: string; bytes: number; }
export interface AppTableStorage {
  name: string; hot_rows: number; cold_rows: number; cold_bytes: number; estimated_hot_bytes: number;
}
export interface AppStorage {
  app_id: string; app_name: string; org_name: string;
  tables: AppTableStorage[];
  hot_rows_total: number; cold_rows_total: number; cold_bytes_total: number; estimated_hot_bytes_total: number;
  cold_files: ColdFile[];
}
export interface TableSize { name: string; total_bytes: number; hot_rows: number; }
export interface StorageReport {
  database: { total_bytes: number; tables: TableSize[] };
  apps: AppStorage[];
}

export async function getAdminStorage(): Promise<StorageReport> {
  const { data } = await api.get<StorageReport>('/v1/admin/storage');
  return data;
}
```
(Confirm the real import shape of the shared client in `./client` and match it — some clients import a default `api`, others a named export.)

- [ ] **Step 3: Storage page**

Create `dashboard/src/pages/Storage.svelte`, modeled on `dashboard/src/pages/Monitors.svelte` (Svelte 5 runes; **house UI components** — the DataTable, Icon registry, cards — NOT raw `<table>`/`<button>`; per the dashboard conventions). It:
- calls `getAdminStorage()` on mount (`$state`/`$effect`), with loading + error states matching Monitors.svelte,
- renders a totals header: formatted `database.total_bytes` and a small table/cards of `database.tables` (name, size, hot rows),
- renders a per-app DataTable (columns: app, org, hot rows, cold rows, cold bytes, est. hot bytes) using the house DataTable component, with a per-row expand showing that app's `cold_files` (path + size),
- formats bytes human-readably with a local helper:
```ts
function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  const u = ['KB', 'MB', 'GB', 'TB']; let v = n / 1024, i = 0;
  while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
  return `${v.toFixed(1)} ${u[i]}`;
}
```

- [ ] **Step 4: Route + admin-gated nav**

In `dashboard/src/routes.ts`, register the `/storage` route → `Storage.svelte` (mirror how `/monitors` is registered).

In `dashboard/src/lib/components/layout/Sidebar.svelte`, add a "Storage" nav item that renders ONLY when the current user is admin — read the user from the auth store (`src/lib/stores/auth.svelte.ts`) and guard on `user?.is_admin` (mirror the existing nav-item markup + the store access pattern already used in Sidebar).

- [ ] **Step 5: Preview verify**

Run the dashboard dev server via the preview tooling. Verify: signed in as the admin user, the "Storage" nav link appears and the page loads the report (totals + per-app table + expandable file list, bytes formatted); signed in as a non-admin, the link is absent and hitting `/storage` directly shows no data (endpoint 403s). Check the browser console/network for errors. Capture a screenshot as proof.

- [ ] **Step 6: Commit**

```bash
git add dashboard/src/lib/models/index.ts dashboard/src/lib/api/admin.ts dashboard/src/pages/Storage.svelte dashboard/src/routes.ts dashboard/src/lib/components/layout/Sidebar.svelte
git commit -m "feat(admin): dashboard Storage page (admin-gated)"
```

---

## Self-Review

**Spec coverage:** global-admin + first-user bootstrap → Tasks 1–2 ✓. Per-app hot counts → Task 4 (`hot_rows_by_app`) + Task 5 assembler ✓. Per-app cold counts → Task 3 (`counts_by_app`) + Task 5 ✓. Cold Parquet files (name+size) → Task 3 (`parse_cold_path`) + Task 5 (`walk_cold`) ✓. Estimated hot bytes → Task 4 (`table_avg_row_width`) + Task 5 ✓. Deployment DB size + per-table → Task 4/5 ✓. `/v1/me` is_admin → Task 1 (model) ✓. Endpoint + gate → Tasks 2, 5 ✓. Dashboard page + nav gating → Task 6 ✓. Verification split (unit vs e2e vs preview) → per-task + Task 5 checkpoint ✓.

**Placeholder scan:** No TBD/TODO. The two "match the neighboring module" notes (Task 5 Step 2 `db`/`ApiError` import path; Task 6 Step 2 client import shape) are explicit "mirror this existing file" instructions with the file named — file-local symbols read at implementation time, same pattern the tiering plan used.

**Type consistency:** `AppCountRow{app_id: Uuid, n: i64}` / `AppOrgRow{app_id, app_name, org_name}` (Task 4) consumed by the Task 5 assembler as written. `ColdFileKey{table, app_id}` + `parse_cold_path` (Task 3) consumed by `walk_cold` (Task 5). `counts_by_app -> Vec<(Uuid,i64)>` (Task 3) collected into `HashMap<Uuid,i64>` (Task 5). `StorageReport`/`AppStorage`/`ColdFile` (Task 5) mirrored by the TS interfaces (Task 6). `require_admin -> Result<(),AuthError>` (Task 2) `?`-used in the handler (Task 5). `User.is_admin: bool` (Task 1) → `/v1/me` → dashboard `User.is_admin` (Task 6). Consistent.
