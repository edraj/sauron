# App Store Install & Uninstall Metrics — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Pull daily install and uninstall counts from Google Play and the Apple App Store into Sauron, and show them as one diverging-bar chart on Overview when the admin-designated "store version" environment is selected.

**Architecture:** A new `sauron-store` crate holds two `StoreConnector` implementations (Play reports from a GCS bucket; Apple deletions from the App Store Connect Analytics Reports API) that know nothing about Postgres. A new `sauron-storesync` daemon claims due connections `FOR UPDATE SKIP LOCKED` — the `sauron-monitor` pattern — fetches them concurrently, and upserts into `store_daily_metrics`. The API exposes credential CRUD (write-only secrets) plus a chart feed; the dashboard renders it conditionally on `apps.store_environment_id`.

**Tech Stack:** Rust (axum 0.8, diesel 2.3 + diesel-async 0.9, tokio, reqwest with rustls, jsonwebtoken 10 `rust_crypto`, aes-gcm, flate2, csv), Postgres, Svelte 5 (runes), Vitest.

**Spec:** `docs/superpowers/specs/2026-08-10-store-install-metrics-design.md`

## Global Constraints

- **NEVER commit and NEVER create branches.** This repository's standing rule. The writing-plans template ends each task with a commit step; those steps are deliberately **absent** from every task below. Leave changes in the working tree.
- Migration number is `2026-08-10-000049_store_metrics`. Migration 48 is the current head.
- Encryption uses the existing `sauron_alerts::SecretCipher` keyed from `NOTIFY_SECRET_KEY`. Fail-closed, **no** derivation fallback from `JWT_SECRET`.
- `store_daily_metrics` has **no** `environment_id` column, ever. The stores do not segment by environment.
- Metric upserts are `ON CONFLICT DO UPDATE SET`, **never** `+=`. Both stores restate recent days.
- Secrets never appear in any HTTP response body. Tests assert against raw JSON, not typed structs.
- Report parsers map columns **by header name, never by index**, and error naming the missing header.
- Dashboard: house UI components only (`Card`, `Button`, `Icon`, `EmptyState`) — no raw `<button>`/`<table>`. Every `viewKey` includes `sessionStore.scopeKey`.
- `vendor_number` renders as `<input type="text">`. Never `type="number"` — `bind:value` writes back `number | null` and freezes the DOM.
- **Backend tests must run with `dangerouslyDisableSandbox: true`** and host-network containers. Under the Bash sandbox's netns every DB-backed test returns early while printing `ok`. Baseline is **1391** passing; a smaller passing count with no failures means tests were skipped.

## File Structure

| Path | Responsibility |
|---|---|
| `backend/migrations/2026-08-10-000049_store_metrics/{up,down}.sql` | Schema |
| `backend/crates/sauron-db/src/schema.rs` | Diesel table macros (modify) |
| `backend/crates/sauron-db/src/models.rs` | `App.store_environment_id`, `AppStoreConnection`, `StoreDailyMetric` (modify) |
| `backend/crates/sauron-db/src/repo.rs` | Connection CRUD, claim query, metric upsert, metric read (modify) |
| `backend/crates/sauron-store/src/lib.rs` | `StoreConnector` trait, `DailyMetric`, `StoreError`, `SyncState` |
| `backend/crates/sauron-store/src/google.rs` | Play: OAuth2, GCS fetch, UTF-16LE CSV parse |
| `backend/crates/sauron-store/src/apple.rs` | Apple: ES256 JWT, report walk, gzip CSV parse |
| `backend/crates/sauron-store/tests/fixtures/` | Real report files |
| `backend/bins/sauron-storesync/src/main.rs` | The daemon loop |
| `backend/bins/sauron-api/src/routes/stores.rs` | Six HTTP handlers |
| `dashboard/src/lib/api/stores.ts` | Typed client |
| `dashboard/src/lib/components/settings/StoreConnectionsCard.svelte` | Credential + environment UI |
| `dashboard/src/lib/components/StoreInstallsChart.svelte` | Diverging bars |
| `dashboard/src/lib/components/StoreSection.svelte` | Overview section + gating |
| `packaging/rpm/binaries.txt`, `sauron.spec`, `packaging/systemd/` | Shipping |

---

### Task 1: Schema and models

**Files:**
- Create: `backend/migrations/2026-08-10-000049_store_metrics/up.sql`
- Create: `backend/migrations/2026-08-10-000049_store_metrics/down.sql`
- Modify: `backend/crates/sauron-db/src/schema.rs`
- Modify: `backend/crates/sauron-db/src/models.rs:132` (add field to `App`)

**Interfaces:**
- Consumes: nothing.
- Produces: tables `app_store_connections`, `store_daily_metrics`; column `apps.store_environment_id`. Models `AppStoreConnection { id: Uuid, app_id: Uuid, store: String, enabled: bool, identifiers: serde_json::Value, secret_enc: Option<Vec<u8>>, sync_state: serde_json::Value, next_sync_at: DateTime<Utc>, last_synced_at: Option<DateTime<Utc>>, last_error: Option<String>, created_at: DateTime<Utc>, updated_at: DateTime<Utc> }` and `StoreDailyMetric { app_id: Uuid, store: String, day: NaiveDate, installs: i64, uninstalls: i64, updated_at: DateTime<Utc> }`.

- [ ] **Step 1: Write `up.sql`**

```sql
-- Which environment represents the build that ships to the stores.
--
-- The stores key their data to a package name / bundle id and have no idea
-- environments exist, so this is a VISIBILITY choice, not a data partition:
-- store_daily_metrics below is deliberately not environment-scoped.
--
-- References the per-app ENROLLMENT (app_environments), not the project
-- catalogue (environments), because the enrollment id is what the dashboard's
-- switcher and `?environment_id=` already carry.
--
-- SET NULL, not CASCADE: retiring an environment should hide the Overview
-- section, not delete the app.
ALTER TABLE apps
  ADD COLUMN store_environment_id UUID REFERENCES app_environments(id) ON DELETE SET NULL;

CREATE TABLE app_store_connections (
  id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  app_id         UUID NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
  store          TEXT NOT NULL CHECK (store IN ('google_play', 'app_store')),
  enabled        BOOLEAN NOT NULL DEFAULT true,
  -- Non-secret, displayable identifiers. JSONB rather than seven columns that
  -- would be half NULL on every row, because the two stores need disjoint
  -- field sets:
  --   google_play: {package_name, gcs_bucket}
  --   app_store:   {bundle_id, apple_app_id, issuer_id, key_id, vendor_number}
  identifiers    JSONB NOT NULL DEFAULT '{}'::jsonb,
  -- AES-256-GCM (sauron_alerts::SecretCipher, NOTIFY_SECRET_KEY).
  -- Play: the service-account JSON. Apple: the .p8 private key.
  secret_enc     BYTEA,
  -- Apple's analyticsReportRequests id lives here: created once, reused for
  -- the life of the connection.
  sync_state     JSONB NOT NULL DEFAULT '{}'::jsonb,
  next_sync_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
  last_synced_at TIMESTAMPTZ,
  last_error     TEXT,
  created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
  UNIQUE (app_id, store)
);

-- The daemon's claim query orders by next_sync_at over enabled rows only.
CREATE INDEX app_store_connections_due_idx
  ON app_store_connections (next_sync_at) WHERE enabled;

CREATE TABLE store_daily_metrics (
  app_id     UUID NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
  store      TEXT NOT NULL CHECK (store IN ('google_play', 'app_store')),
  day        DATE NOT NULL,
  installs   BIGINT NOT NULL DEFAULT 0,
  uninstalls BIGINT NOT NULL DEFAULT 0,
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  -- Writers MUST use ON CONFLICT DO UPDATE SET (not +=). Both stores restate
  -- recent days as their pipelines settle; an additive upsert inflates every
  -- number on every sync and produces a chart that still looks plausible.
  PRIMARY KEY (app_id, store, day)
);

-- The chart feed reads one app's range across both stores.
CREATE INDEX store_daily_metrics_app_day_idx ON store_daily_metrics (app_id, day);
```

- [ ] **Step 2: Write `down.sql`**

```sql
DROP INDEX IF EXISTS store_daily_metrics_app_day_idx;
DROP TABLE IF EXISTS store_daily_metrics;
DROP INDEX IF EXISTS app_store_connections_due_idx;
DROP TABLE IF EXISTS app_store_connections;
ALTER TABLE apps DROP COLUMN IF EXISTS store_environment_id;
```

- [ ] **Step 3: Run the migration against a real database**

```bash
cd backend && cargo run --bin sauron-migrate
```

Expected: migration 49 applies with no error. Run with `dangerouslyDisableSandbox: true` — the sandbox netns cannot reach Postgres.

- [ ] **Step 4: Add the tables to `schema.rs`**

Add `store_environment_id -> Nullable<Uuid>` to the existing `apps` block (last field, matching column order after `ALTER TABLE ADD COLUMN`), then:

```rust
diesel::table! {
    app_store_connections (id) {
        id -> Uuid,
        app_id -> Uuid,
        store -> Text,
        enabled -> Bool,
        identifiers -> Jsonb,
        secret_enc -> Nullable<Bytea>,
        sync_state -> Jsonb,
        next_sync_at -> Timestamptz,
        last_synced_at -> Nullable<Timestamptz>,
        last_error -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    store_daily_metrics (app_id, store, day) {
        app_id -> Uuid,
        store -> Text,
        day -> Date,
        installs -> BigInt,
        uninstalls -> BigInt,
        updated_at -> Timestamptz,
    }
}
```

Then add to the existing `joinable!`/`allow_tables_to_appear_in_same_query!` blocks:

```rust
diesel::joinable!(app_store_connections -> apps (app_id));
diesel::joinable!(store_daily_metrics -> apps (app_id));
```

and add `app_store_connections,` and `store_daily_metrics,` to the `allow_tables_to_appear_in_same_query!` list at `schema.rs:860`.

- [ ] **Step 5: Add the models**

In `models.rs`, add `pub store_environment_id: Option<Uuid>,` as the **last** field of `App` (field order must match `schema.rs`), then append:

```rust
/// One app's credentials for one store.
///
/// `identifiers` is public, displayable configuration; `secret_enc` is the
/// AES-GCM credential and must never reach a response body. `sync_state`
/// carries connector-private bookkeeping — for Apple, the id of the ongoing
/// `analyticsReportRequest`, which is created once and reused forever.
#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = app_store_connections)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct AppStoreConnection {
    pub id: Uuid,
    pub app_id: Uuid,
    pub store: String,
    pub enabled: bool,
    pub identifiers: serde_json::Value,
    pub secret_enc: Option<Vec<u8>>,
    pub sync_state: serde_json::Value,
    pub next_sync_at: DateTime<Utc>,
    pub last_synced_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One store's counts for one calendar day.
///
/// No `environment_id`, deliberately — see the migration comment.
#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = store_daily_metrics)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct StoreDailyMetric {
    pub app_id: Uuid,
    pub store: String,
    pub day: chrono::NaiveDate,
    pub installs: i64,
    pub uninstalls: i64,
    pub updated_at: DateTime<Utc>,
}
```

Note: `AppStoreConnection` derives **no** `Serialize`. That is the compile-time half of "secrets are write-only" — the API's response type is a separate struct built in Task 7.

- [ ] **Step 6: Verify it compiles**

```bash
cd backend && cargo check -p sauron-db
```

Expected: clean. A `check_for_backend` mismatch here means the `schema.rs` field order does not match the model's field order.

---

### Task 2: Repository functions

**Files:**
- Modify: `backend/crates/sauron-db/src/repo.rs` (append a new section)
- Test: `backend/crates/sauron-db/tests/store_metrics.rs` (create)

**Interfaces:**
- Consumes: models from Task 1.
- Produces:
  - `list_store_connections(conn, app_id) -> QueryResult<Vec<AppStoreConnection>>`
  - `get_store_connection(conn, app_id, store: &str) -> QueryResult<Option<AppStoreConnection>>`
  - `upsert_store_connection(conn, app_id, store: &str, identifiers: &Value, secret_enc: Option<Option<Vec<u8>>>) -> QueryResult<AppStoreConnection>`
  - `delete_store_connection(conn, app_id, store: &str) -> QueryResult<usize>`
  - `queue_store_sync(conn, app_id, store: &str) -> QueryResult<usize>`
  - `claim_due_store_connections(conn, batch: i64, interval_secs: i64) -> QueryResult<Vec<AppStoreConnection>>`
  - `record_store_sync_result(conn, id: Uuid, error: Option<&str>) -> QueryResult<usize>`
  - `set_store_sync_state(conn, id: Uuid, state: &Value) -> QueryResult<usize>`
  - `upsert_store_daily_metrics(conn, app_id, store: &str, rows: &[(NaiveDate, i64, i64)]) -> QueryResult<usize>`
  - `store_metrics_range(conn, app_id, since: NaiveDate) -> QueryResult<Vec<StoreDailyMetric>>`

- [ ] **Step 1: Write the failing tests**

Create `backend/crates/sauron-db/tests/store_metrics.rs`. Follow the harness already used by `tests/env_scoping.rs` for database setup (read it first — it creates an ephemeral database and runs migrations).

```rust
//! Storage-layer behaviour for store install/uninstall metrics.

mod common; // reuse the ephemeral-DB harness from tests/env_scoping.rs

use chrono::NaiveDate;
use sauron_db::repo;

#[tokio::test]
async fn upsert_is_idempotent_not_additive() {
    // THE bug this table's PK exists to prevent: both stores restate recent
    // days, so syncing the same day twice must SET, not ADD. An additive
    // upsert doubles every number on every tick and the chart still looks
    // plausible, which is why this is asserted rather than assumed.
    let (mut conn, app_id) = common::app_fixture().await;
    let day = NaiveDate::from_ymd_opt(2026, 8, 1).unwrap();

    repo::upsert_store_daily_metrics(&mut conn, app_id, "google_play", &[(day, 100, 10)])
        .await
        .unwrap();
    repo::upsert_store_daily_metrics(&mut conn, app_id, "google_play", &[(day, 100, 10)])
        .await
        .unwrap();

    let rows = repo::store_metrics_range(&mut conn, app_id, day).await.unwrap();
    assert_eq!(rows.len(), 1, "one row per (app, store, day)");
    assert_eq!(rows[0].installs, 100, "second sync must overwrite, not add");
    assert_eq!(rows[0].uninstalls, 10);
}

#[tokio::test]
async fn restated_day_overwrites_with_new_value() {
    let (mut conn, app_id) = common::app_fixture().await;
    let day = NaiveDate::from_ymd_opt(2026, 8, 1).unwrap();

    repo::upsert_store_daily_metrics(&mut conn, app_id, "app_store", &[(day, 100, 10)])
        .await
        .unwrap();
    // Apple settles the number upward a day later.
    repo::upsert_store_daily_metrics(&mut conn, app_id, "app_store", &[(day, 137, 12)])
        .await
        .unwrap();

    let rows = repo::store_metrics_range(&mut conn, app_id, day).await.unwrap();
    assert_eq!(rows[0].installs, 137);
    assert_eq!(rows[0].uninstalls, 12);
}

#[tokio::test]
async fn secret_omitted_is_preserved_secret_null_is_cleared() {
    // Editing a package name must not silently wipe the credential.
    let (mut conn, app_id) = common::app_fixture().await;
    let ids = serde_json::json!({"package_name": "com.example.app", "gcs_bucket": "pubsite_prod_rev_1"});

    repo::upsert_store_connection(&mut conn, app_id, "google_play", &ids, Some(Some(b"sekrit".to_vec())))
        .await
        .unwrap();

    // None = leave unchanged.
    let ids2 = serde_json::json!({"package_name": "com.example.renamed", "gcs_bucket": "pubsite_prod_rev_1"});
    let row = repo::upsert_store_connection(&mut conn, app_id, "google_play", &ids2, None)
        .await
        .unwrap();
    assert_eq!(row.secret_enc.as_deref(), Some(&b"sekrit"[..]), "omitted secret preserved");
    assert_eq!(row.identifiers["package_name"], "com.example.renamed");

    // Some(None) = clear.
    let row = repo::upsert_store_connection(&mut conn, app_id, "google_play", &ids2, Some(None))
        .await
        .unwrap();
    assert!(row.secret_enc.is_none(), "explicit null clears the secret");
}

#[tokio::test]
async fn claim_pushes_next_sync_forward_so_a_peer_cannot_double_claim() {
    let (mut conn, app_id) = common::app_fixture().await;
    let ids = serde_json::json!({"package_name": "com.example.app", "gcs_bucket": "b"});
    repo::upsert_store_connection(&mut conn, app_id, "google_play", &ids, None)
        .await
        .unwrap();

    let first = repo::claim_due_store_connections(&mut conn, 10, 21600).await.unwrap();
    assert_eq!(first.len(), 1, "a brand-new connection is due immediately");

    let second = repo::claim_due_store_connections(&mut conn, 10, 21600).await.unwrap();
    assert!(second.is_empty(), "claiming must push next_sync_at past now()");
}
```

- [ ] **Step 2: Run to verify it fails**

```bash
cd backend && cargo test -p sauron-db --test store_metrics
```

Expected: FAIL to compile — `repo::upsert_store_daily_metrics` not found. Run with `dangerouslyDisableSandbox: true`.

- [ ] **Step 3: Implement the repo functions**

Append to `repo.rs`:

```rust
// ---------------------------------------------------------------------------
// App store connections and daily metrics
// ---------------------------------------------------------------------------

pub async fn list_store_connections(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
) -> QueryResult<Vec<AppStoreConnection>> {
    app_store_connections::table
        .filter(app_store_connections::app_id.eq(app_id))
        .select(AppStoreConnection::as_select())
        .order(app_store_connections::store.asc())
        .load(conn)
        .await
}

pub async fn get_store_connection(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    store: &str,
) -> QueryResult<Option<AppStoreConnection>> {
    app_store_connections::table
        .filter(app_store_connections::app_id.eq(app_id))
        .filter(app_store_connections::store.eq(store))
        .select(AppStoreConnection::as_select())
        .first(conn)
        .await
        .optional()
}

/// `secret_enc`: `None` = leave unchanged, `Some(None)` = clear, `Some(Some(b))`
/// = replace. Same three-state idiom as `update_notification_channel` — without
/// it, saving an edited package name wipes the stored credential.
pub async fn upsert_store_connection(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    store: &str,
    identifiers: &serde_json::Value,
    secret_enc: Option<Option<Vec<u8>>>,
) -> QueryResult<AppStoreConnection> {
    diesel::insert_into(app_store_connections::table)
        .values((
            app_store_connections::app_id.eq(app_id),
            app_store_connections::store.eq(store),
            app_store_connections::identifiers.eq(identifiers),
            app_store_connections::secret_enc.eq(secret_enc.clone().flatten()),
        ))
        .on_conflict((app_store_connections::app_id, app_store_connections::store))
        .do_update()
        .set((
            app_store_connections::identifiers.eq(identifiers),
            app_store_connections::updated_at.eq(diesel::dsl::now),
        ))
        .returning(AppStoreConnection::as_returning())
        .get_result(conn)
        .await?;

    // The secret is written in a second statement precisely because `None` must
    // leave the stored value alone, which a single upsert's SET list cannot
    // express.
    if let Some(s) = secret_enc {
        diesel::update(app_store_connections::table)
            .filter(app_store_connections::app_id.eq(app_id))
            .filter(app_store_connections::store.eq(store))
            .set((
                app_store_connections::secret_enc.eq(s),
                app_store_connections::updated_at.eq(diesel::dsl::now),
            ))
            .execute(conn)
            .await?;
    }

    get_store_connection(conn, app_id, store)
        .await?
        .ok_or(diesel::result::Error::NotFound)
}

pub async fn delete_store_connection(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    store: &str,
) -> QueryResult<usize> {
    // store_daily_metrics is deliberately NOT touched: collected history is not
    // a credential, and re-adding the connection resumes against it.
    diesel::delete(
        app_store_connections::table
            .filter(app_store_connections::app_id.eq(app_id))
            .filter(app_store_connections::store.eq(store)),
    )
    .execute(conn)
    .await
}

/// "Queue sync": make the row due now. The daemon does the work — no
/// multi-minute store download ever runs inside an HTTP request.
pub async fn queue_store_sync(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    store: &str,
) -> QueryResult<usize> {
    diesel::update(
        app_store_connections::table
            .filter(app_store_connections::app_id.eq(app_id))
            .filter(app_store_connections::store.eq(store)),
    )
    .set(app_store_connections::next_sync_at.eq(diesel::dsl::now))
    .execute(conn)
    .await
}

/// Atomically claim due connections and push `next_sync_at` forward so no peer
/// daemon picks the same rows. Shape copied from `claim_due_monitors`.
pub async fn claim_due_store_connections(
    conn: &mut AsyncPgConnection,
    batch: i64,
    interval_secs: i64,
) -> QueryResult<Vec<AppStoreConnection>> {
    diesel::sql_query(
        "UPDATE app_store_connections \
            SET next_sync_at = now() + make_interval(secs => $2) \
          WHERE id IN ( \
              SELECT id FROM app_store_connections \
               WHERE enabled AND next_sync_at <= now() \
               ORDER BY next_sync_at FOR UPDATE SKIP LOCKED LIMIT $1 \
          ) RETURNING *",
    )
    .bind::<BigInt, _>(batch)
    .bind::<BigInt, _>(interval_secs)
    .get_results(conn)
    .await
}

/// Record the outcome. `error: None` clears `last_error` and stamps
/// `last_synced_at`; `Some(msg)` records it WITHOUT stamping success, so a
/// permanently failing connection cannot look freshly synced.
pub async fn record_store_sync_result(
    conn: &mut AsyncPgConnection,
    id: Uuid,
    error: Option<&str>,
) -> QueryResult<usize> {
    match error {
        None => {
            diesel::update(app_store_connections::table.find(id))
                .set((
                    app_store_connections::last_synced_at.eq(diesel::dsl::now.nullable()),
                    app_store_connections::last_error.eq::<Option<String>>(None),
                ))
                .execute(conn)
                .await
        }
        Some(msg) => {
            diesel::update(app_store_connections::table.find(id))
                .set(app_store_connections::last_error.eq(Some(msg)))
                .execute(conn)
                .await
        }
    }
}

pub async fn set_store_sync_state(
    conn: &mut AsyncPgConnection,
    id: Uuid,
    state: &serde_json::Value,
) -> QueryResult<usize> {
    diesel::update(app_store_connections::table.find(id))
        .set(app_store_connections::sync_state.eq(state))
        .execute(conn)
        .await
}

/// SET, never `+=`. See the migration comment on `store_daily_metrics`.
pub async fn upsert_store_daily_metrics(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    store: &str,
    rows: &[(chrono::NaiveDate, i64, i64)],
) -> QueryResult<usize> {
    if rows.is_empty() {
        return Ok(0);
    }
    let values: Vec<_> = rows
        .iter()
        .map(|(day, installs, uninstalls)| {
            (
                store_daily_metrics::app_id.eq(app_id),
                store_daily_metrics::store.eq(store),
                store_daily_metrics::day.eq(*day),
                store_daily_metrics::installs.eq(*installs),
                store_daily_metrics::uninstalls.eq(*uninstalls),
            )
        })
        .collect();

    diesel::insert_into(store_daily_metrics::table)
        .values(values)
        .on_conflict((
            store_daily_metrics::app_id,
            store_daily_metrics::store,
            store_daily_metrics::day,
        ))
        .do_update()
        .set((
            store_daily_metrics::installs.eq(diesel::upsert::excluded(store_daily_metrics::installs)),
            store_daily_metrics::uninstalls
                .eq(diesel::upsert::excluded(store_daily_metrics::uninstalls)),
            store_daily_metrics::updated_at.eq(diesel::dsl::now),
        ))
        .execute(conn)
        .await
}

pub async fn store_metrics_range(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    since: chrono::NaiveDate,
) -> QueryResult<Vec<StoreDailyMetric>> {
    store_daily_metrics::table
        .filter(store_daily_metrics::app_id.eq(app_id))
        .filter(store_daily_metrics::day.ge(since))
        .select(StoreDailyMetric::as_select())
        .order((store_daily_metrics::day.asc(), store_daily_metrics::store.asc()))
        .load(conn)
        .await
}
```

Add `AppStoreConnection, StoreDailyMetric` to the `models::` import list at the top of `repo.rs`, and `app_store_connections, store_daily_metrics` to the `schema::` import list.

- [ ] **Step 4: Run tests to verify they pass**

```bash
cd backend && cargo test -p sauron-db --test store_metrics
```

Expected: 4 passed. Run with `dangerouslyDisableSandbox: true` and host-network containers. **A run reporting "0 passed; 0 failed" means the harness skipped — that is a failure, not a pass.**

---

### Task 3: `sauron-store` crate skeleton and the Google Play parser

**Files:**
- Create: `backend/crates/sauron-store/Cargo.toml`
- Create: `backend/crates/sauron-store/src/lib.rs`
- Create: `backend/crates/sauron-store/src/google.rs`
- Create: `backend/crates/sauron-store/tests/fixtures/installs_com.example.app_202608_overview.csv` (UTF-16LE)
- Modify: `backend/Cargo.toml` (workspace deps)

**Interfaces:**
- Consumes: nothing (no database, no network in this task).
- Produces:
  - `pub struct DailyMetric { pub day: NaiveDate, pub installs: i64, pub uninstalls: i64 }`
  - `pub enum StoreKind { GooglePlay, AppStore }` with `as_str()` / `from_str()` matching the DB CHECK values
  - `pub fn google::parse_installs_csv(bytes: &[u8]) -> anyhow::Result<Vec<DailyMetric>>`
  - `pub fn decode_utf16le(bytes: &[u8]) -> anyhow::Result<String>`

- [ ] **Step 1: Add the workspace dependency**

In `backend/Cargo.toml` `[workspace.dependencies]`, add under the serialization section:

```toml
# Play and Apple both ship reports as delimited text with quoted fields (app
# names contain commas). Hand-splitting on ',' corrupts those rows silently.
csv = "1"
```

and register the crate alongside the other internal crates:

```toml
sauron-store = { path = "crates/sauron-store" }
```

- [ ] **Step 2: Create the crate manifest**

`backend/crates/sauron-store/Cargo.toml`:

```toml
[package]
name = "sauron-store"
version.workspace = true
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[dependencies]
anyhow = { workspace = true }
chrono = { workspace = true }
csv = { workspace = true }
flate2 = { workspace = true }
jsonwebtoken = { workspace = true }
reqwest = { workspace = true, features = ["json"] }
serde = { workspace = true }
serde_json = { workspace = true }
tracing = { workspace = true }
```

- [ ] **Step 3: Build the UTF-16LE fixture**

The Play CSV is UTF-16LE with a BOM. Generate a real one rather than hand-typing bytes:

```bash
cd backend/crates/sauron-store/tests/fixtures
printf 'Date,Package Name,Daily Device Installs,Daily Device Uninstalls,Daily Device Upgrades\n2026-08-01,com.example.app,1240,310,88\n2026-08-02,com.example.app,1180,295,74\n2026-08-03,com.example.app,0,12,3\n' \
  | iconv -f UTF-8 -t UTF-16LE > body.tmp
printf '\xff\xfe' > installs_com.example.app_202608_overview.csv
cat body.tmp >> installs_com.example.app_202608_overview.csv
rm body.tmp
xxd installs_com.example.app_202608_overview.csv | head -2
```

Expected first bytes: `fffe 4400 6100 7400 6500` — BOM then `D`, `a`, `t`, `e` as UTF-16LE.

- [ ] **Step 4: Write the failing tests**

`backend/crates/sauron-store/src/google.rs`, test module at the bottom:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &[u8] =
        include_bytes!("../tests/fixtures/installs_com.example.app_202608_overview.csv");

    #[test]
    fn parses_real_utf16le_play_report() {
        // Reading this file as UTF-8 does not error — it yields mojibake that
        // parses as a valid single-column CSV with zero rows. That silent
        // success is why the encoding is asserted here explicitly.
        let rows = parse_installs_csv(FIXTURE).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].day, NaiveDate::from_ymd_opt(2026, 8, 1).unwrap());
        assert_eq!(rows[0].installs, 1240);
        assert_eq!(rows[0].uninstalls, 310);
        assert_eq!(rows[2].installs, 0, "a genuine zero-install day is data, not absence");
        assert_eq!(rows[2].uninstalls, 12);
    }

    #[test]
    fn errors_by_name_when_a_column_is_missing() {
        // Column order in these reports is not contractual. An index-based
        // parser that shifts by one produces NUMBERS, not errors — so a
        // missing header must be loud and must say which one.
        let csv = "Date,Package Name,Daily Device Uninstalls\n2026-08-01,com.example.app,310\n";
        let mut bytes = vec![0xff, 0xfe];
        bytes.extend(csv.encode_utf16().flat_map(|u| u.to_le_bytes()));

        let err = parse_installs_csv(&bytes).unwrap_err().to_string();
        assert!(
            err.contains("Daily Device Installs"),
            "error must name the missing column, got: {err}"
        );
    }

    #[test]
    fn decodes_utf16le_with_and_without_bom() {
        let with_bom = [0xff, 0xfe, 0x41, 0x00, 0x42, 0x00];
        assert_eq!(decode_utf16le(&with_bom).unwrap(), "AB");
        let without_bom = [0x41, 0x00, 0x42, 0x00];
        assert_eq!(decode_utf16le(&without_bom).unwrap(), "AB");
    }

    #[test]
    fn rejects_odd_length_input() {
        assert!(decode_utf16le(&[0xff, 0xfe, 0x41]).is_err());
    }
}
```

- [ ] **Step 5: Run to verify failure**

```bash
cd backend && cargo test -p sauron-store
```

Expected: FAIL to compile — `parse_installs_csv` not found.

- [ ] **Step 6: Write `lib.rs`**

```rust
//! Store report connectors: Google Play and the Apple App Store.
//!
//! Everything here is pure fetch-and-parse. No Postgres, no Sauron models —
//! which is what lets both connectors be tested against committed fixture files
//! with no network and no database.

pub mod apple;
pub mod google;

use chrono::NaiveDate;

/// One store's counts for one calendar day.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DailyMetric {
    pub day: NaiveDate,
    pub installs: i64,
    pub uninstalls: i64,
}

/// The two stores. `as_str` values are the DB CHECK constraint's values; keep
/// them in sync with migration 49.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreKind {
    GooglePlay,
    AppStore,
}

impl StoreKind {
    pub fn as_str(self) -> &'static str {
        match self {
            StoreKind::GooglePlay => "google_play",
            StoreKind::AppStore => "app_store",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "google_play" => Some(StoreKind::GooglePlay),
            "app_store" => Some(StoreKind::AppStore),
            _ => None,
        }
    }
}

/// Look a column up by NAME and return its index, or an error naming it.
///
/// Shared by both connectors because both reports are header-bearing delimited
/// text whose column ORDER is not contractual.
pub(crate) fn column_index(headers: &csv::StringRecord, name: &str) -> anyhow::Result<usize> {
    headers
        .iter()
        .position(|h| h.trim() == name)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "report is missing the {name:?} column; got columns: {:?}",
                headers.iter().collect::<Vec<_>>()
            )
        })
}
```

- [ ] **Step 7: Write the Google parser**

`backend/crates/sauron-store/src/google.rs`:

```rust
//! Google Play install reports.
//!
//! Play does not expose installs over an API. The numbers live as monthly CSVs
//! in the Play Console's GCS reports bucket at
//! `stats/installs/installs_{package}_{YYYYMM}_overview.csv`, read with a
//! service account. Two properties of those files cost a day each if met by
//! surprise: they are UTF-16LE with a BOM, and they are MONTHLY (a 90-day
//! backfill is four object fetches, not ninety).

use anyhow::Context;
use chrono::NaiveDate;

use crate::{column_index, DailyMetric};

const COL_DATE: &str = "Date";
const COL_INSTALLS: &str = "Daily Device Installs";
const COL_UNINSTALLS: &str = "Daily Device Uninstalls";

/// Decode UTF-16LE, tolerating a present or absent BOM.
///
/// Play writes the BOM; the tolerance is for hand-made fixtures and for the day
/// Google stops. Decoding as UTF-8 instead does not fail — it yields mojibake
/// that parses as a valid empty CSV, so this conversion is explicit.
pub fn decode_utf16le(bytes: &[u8]) -> anyhow::Result<String> {
    let body = bytes.strip_prefix(&[0xff, 0xfe]).unwrap_or(bytes);
    anyhow::ensure!(
        body.len() % 2 == 0,
        "UTF-16LE body has an odd byte length ({}); file is truncated or not UTF-16",
        body.len()
    );
    let units: Vec<u16> = body
        .chunks_exact(2)
        .map(|p| u16::from_le_bytes([p[0], p[1]]))
        .collect();
    String::from_utf16(&units).context("report is not valid UTF-16LE")
}

/// Parse one monthly overview CSV into daily metrics.
pub fn parse_installs_csv(bytes: &[u8]) -> anyhow::Result<Vec<DailyMetric>> {
    let text = decode_utf16le(bytes)?;
    let mut rdr = csv::Reader::from_reader(text.as_bytes());
    let headers = rdr.headers().context("report has no header row")?.clone();

    let i_date = column_index(&headers, COL_DATE)?;
    let i_installs = column_index(&headers, COL_INSTALLS)?;
    let i_uninstalls = column_index(&headers, COL_UNINSTALLS)?;

    let mut out = Vec::new();
    for rec in rdr.records() {
        let rec = rec.context("malformed CSV row in Play report")?;
        let raw_day = rec.get(i_date).unwrap_or_default().trim();
        if raw_day.is_empty() {
            continue;
        }
        let day = NaiveDate::parse_from_str(raw_day, "%Y-%m-%d")
            .with_context(|| format!("unparseable Date {raw_day:?} in Play report"))?;
        out.push(DailyMetric {
            day,
            installs: parse_count(rec.get(i_installs)),
            uninstalls: parse_count(rec.get(i_uninstalls)),
        });
    }
    Ok(out)
}

/// Blank cells mean zero in these reports; a non-numeric cell is a real defect
/// but must not discard the whole month, so it reads as zero and the row
/// survives.
fn parse_count(cell: Option<&str>) -> i64 {
    cell.unwrap_or_default().trim().parse::<i64>().unwrap_or(0)
}
```

Create a stub `backend/crates/sauron-store/src/apple.rs` containing only `//! Apple App Store reports. Implemented in Task 5.` so the crate compiles.

- [ ] **Step 8: Run tests to verify they pass**

```bash
cd backend && cargo test -p sauron-store
```

Expected: 4 passed. This task has no database, so it passes under the sandbox too.

---

### Task 4: Google Play fetch path

**Files:**
- Modify: `backend/crates/sauron-store/src/google.rs`
- Modify: `backend/crates/sauron-store/src/lib.rs` (add `StoreConnector`)

**Interfaces:**
- Consumes: `DailyMetric`, `column_index` from Task 3.
- Produces:
  - `pub struct GoogleIdentifiers { pub package_name: String, pub gcs_bucket: String }` (serde `Deserialize`)
  - `pub async fn google::fetch(client: &reqwest::Client, ids: &GoogleIdentifiers, service_account_json: &str, since: NaiveDate, today: NaiveDate) -> anyhow::Result<Vec<DailyMetric>>`
  - `pub const GOOGLE_HOSTS: [&str; 2]`

- [ ] **Step 1: Write the failing tests**

Append to `google.rs`'s test module:

```rust
#[test]
fn months_spanned_covers_the_backfill_window_inclusively() {
    // Files are MONTHLY. A 90-day backfill must resolve to 4 object names, not
    // 90 — getting this wrong is 90 HTTP 404s per tick per app.
    let since = NaiveDate::from_ymd_opt(2026, 5, 20).unwrap();
    let today = NaiveDate::from_ymd_opt(2026, 8, 10).unwrap();
    assert_eq!(months_spanned(since, today), vec![202605, 202606, 202607, 202608]);
}

#[test]
fn months_spanned_handles_a_single_month_and_a_year_boundary() {
    let d = NaiveDate::from_ymd_opt(2026, 8, 1).unwrap();
    assert_eq!(months_spanned(d, d), vec![202608]);
    assert_eq!(
        months_spanned(
            NaiveDate::from_ymd_opt(2025, 12, 15).unwrap(),
            NaiveDate::from_ymd_opt(2026, 1, 3).unwrap()
        ),
        vec![202512, 202601]
    );
}

#[test]
fn object_path_matches_the_play_console_layout() {
    let ids = GoogleIdentifiers {
        package_name: "com.example.app".into(),
        gcs_bucket: "pubsite_prod_rev_01234".into(),
    };
    assert_eq!(
        object_path(&ids, 202608),
        "stats/installs/installs_com.example.app_202608_overview.csv"
    );
}
```

- [ ] **Step 2: Run to verify failure**

```bash
cd backend && cargo test -p sauron-store
```

Expected: FAIL — `months_spanned` and `object_path` not found.

- [ ] **Step 3: Implement the fetch path**

Append to `google.rs`:

```rust
use serde::Deserialize;

/// Hosts this connector is permitted to reach. No operator-supplied URL is ever
/// fetched — only a bucket NAME and a package name, interpolated into paths on
/// these two hosts — which is why the SSRF-guarding resolver `sauron-monitor`
/// needs is not required here.
pub const GOOGLE_HOSTS: [&str; 2] = ["oauth2.googleapis.com", "storage.googleapis.com"];

#[derive(Debug, Clone, Deserialize)]
pub struct GoogleIdentifiers {
    pub package_name: String,
    /// The Play Console reports bucket, e.g. `pubsite_prod_rev_01234567890`.
    /// Stored as a bare name; `gs://` prefixes are stripped on save by the API.
    pub gcs_bucket: String,
}

/// The service-account key file, as downloaded from Google Cloud.
#[derive(Debug, Deserialize)]
struct ServiceAccount {
    client_email: String,
    private_key: String,
    #[serde(default = "default_token_uri")]
    token_uri: String,
}

fn default_token_uri() -> String {
    "https://oauth2.googleapis.com/token".to_string()
}

#[derive(Debug, serde::Serialize)]
struct Claims<'a> {
    iss: &'a str,
    scope: &'a str,
    aud: &'a str,
    exp: i64,
    iat: i64,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
}

/// Every `YYYYMM` the window touches, inclusive at both ends.
fn months_spanned(since: NaiveDate, today: NaiveDate) -> Vec<i32> {
    use chrono::Datelike;
    let (mut y, mut m) = (since.year(), since.month());
    let (ey, em) = (today.year(), today.month());
    let mut out = Vec::new();
    while (y, m) <= (ey, em) {
        out.push(y * 100 + m as i32);
        if m == 12 {
            y += 1;
            m = 1;
        } else {
            m += 1;
        }
    }
    out
}

fn object_path(ids: &GoogleIdentifiers, yyyymm: i32) -> String {
    format!(
        "stats/installs/installs_{}_{}_overview.csv",
        ids.package_name, yyyymm
    )
}

/// Exchange the service-account key for a read-only storage access token.
async fn access_token(client: &reqwest::Client, sa: &ServiceAccount) -> anyhow::Result<String> {
    let now = chrono::Utc::now().timestamp();
    let claims = Claims {
        iss: &sa.client_email,
        scope: "https://www.googleapis.com/auth/devstorage.read_only",
        aud: &sa.token_uri,
        exp: now + 3600,
        iat: now,
    };
    let key = jsonwebtoken::EncodingKey::from_rsa_pem(sa.private_key.as_bytes())
        .context("service-account private_key is not a valid RSA PEM")?;
    let assertion = jsonwebtoken::encode(
        &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256),
        &claims,
        &key,
    )?;

    let resp = client
        .post(&sa.token_uri)
        .form(&[
            ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
            ("assertion", &assertion),
        ])
        .send()
        .await
        .context("Google token endpoint unreachable")?;

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    anyhow::ensure!(
        status.is_success(),
        "Google token exchange failed ({status}): {}",
        body.chars().take(300).collect::<String>()
    );
    Ok(serde_json::from_str::<TokenResponse>(&body)
        .context("Google token response was not the expected shape")?
        .access_token)
}

/// Fetch and parse every monthly report the window touches.
///
/// A month that 404s is SKIPPED, not fatal: the first month of a backfill
/// predates the app's release for every new connection, and one missing month
/// must not discard the months that did arrive.
pub async fn fetch(
    client: &reqwest::Client,
    ids: &GoogleIdentifiers,
    service_account_json: &str,
    since: NaiveDate,
    today: NaiveDate,
) -> anyhow::Result<Vec<DailyMetric>> {
    let sa: ServiceAccount = serde_json::from_str(service_account_json)
        .context("stored Google credential is not a service-account JSON key")?;
    let token = access_token(client, &sa).await?;

    let mut out = Vec::new();
    for yyyymm in months_spanned(since, today) {
        let url = format!(
            "https://storage.googleapis.com/storage/v1/b/{}/o/{}?alt=media",
            ids.gcs_bucket,
            urlencode(&object_path(ids, yyyymm))
        );
        let resp = client.get(&url).bearer_auth(&token).send().await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            tracing::debug!(yyyymm, "no Play report for this month; skipping");
            continue;
        }
        let status = resp.status();
        anyhow::ensure!(status.is_success(), "Play report fetch failed ({status})");
        let bytes = resp.bytes().await?;
        out.extend(parse_installs_csv(&bytes)?);
    }
    out.retain(|m| m.day >= since && m.day <= today);
    out.sort_by_key(|m| m.day);
    Ok(out)
}

/// GCS object names go in the PATH segment, so `/` must be escaped — an
/// unescaped `stats/installs/...` addresses a different (nonexistent) object.
fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}
```

- [ ] **Step 4: Run tests**

```bash
cd backend && cargo test -p sauron-store
```

Expected: 7 passed.

---

### Task 5: Apple App Store connector

**Files:**
- Modify: `backend/crates/sauron-store/src/apple.rs`
- Create: `backend/crates/sauron-store/tests/fixtures/apple_installs_deletions.csv.gz`

**Interfaces:**
- Consumes: `DailyMetric`, `column_index`.
- Produces:
  - `pub struct AppleIdentifiers { bundle_id, apple_app_id, issuer_id, key_id, vendor_number }`
  - `pub enum AppleProgress { Pending, Ready(Vec<DailyMetric>) }`
  - `pub fn apple::parse_report_csv(gzipped: &[u8]) -> anyhow::Result<Vec<DailyMetric>>`
  - `pub async fn apple::fetch(client, ids, p8_pem, request_id: Option<&str>, since, today) -> anyhow::Result<(String, AppleProgress)>` — returns the (possibly newly created) report-request id so the caller can persist it.
  - `pub const APPLE_HOST: &str`

**Before writing code:** download one real "App Store Installations and Deletions" report segment from App Store Connect and inspect its header row. The spec flags this as the one known-unknown. The column names used below (`Date`, `Installations`, `Deletions`) are the expected ones — **if the real report differs, change the three constants and the fixture, not the parsing logic.**

- [ ] **Step 1: Build the fixture from the real report**

```bash
cd backend/crates/sauron-store/tests/fixtures
printf 'Date,App Name,App Apple Identifier,Installations,Deletions\n2026-08-01,Example,1234567890,880,195\n2026-08-02,Example,1234567890,910,201\n' \
  | gzip -c > apple_installs_deletions.csv.gz
gunzip -c apple_installs_deletions.csv.gz | head -1
```

Replace this synthetic content with the real downloaded segment as soon as one is available; keep the same filename.

- [ ] **Step 2: Write the failing tests**

In `apple.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &[u8] = include_bytes!("../tests/fixtures/apple_installs_deletions.csv.gz");

    #[test]
    fn parses_gzipped_installs_and_deletions() {
        let rows = parse_report_csv(FIXTURE).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].day, chrono::NaiveDate::from_ymd_opt(2026, 8, 1).unwrap());
        assert_eq!(rows[0].installs, 880);
        assert_eq!(rows[0].uninstalls, 195, "Apple's Deletions map to uninstalls");
    }

    #[test]
    fn errors_by_name_when_deletions_column_is_absent() {
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        use std::io::Write;
        gz.write_all(b"Date,Installations\n2026-08-01,880\n").unwrap();
        let bytes = gz.finish().unwrap();

        let err = parse_report_csv(&bytes).unwrap_err().to_string();
        assert!(err.contains("Deletions"), "must name the missing column, got: {err}");
    }

    #[test]
    fn rejects_input_that_is_not_gzip() {
        assert!(parse_report_csv(b"Date,Installations,Deletions\n").is_err());
    }

    #[test]
    fn aggregates_duplicate_days_across_segments() {
        // Apple segments a day's report by dimension; the same date appears in
        // several rows and the daily total is their SUM. Taking the last row
        // instead silently under-reports every day.
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        use std::io::Write;
        gz.write_all(
            b"Date,Installations,Deletions\n2026-08-01,500,100\n2026-08-01,380,95\n",
        )
        .unwrap();
        let bytes = gz.finish().unwrap();

        let rows = parse_report_csv(&bytes).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].installs, 880);
        assert_eq!(rows[0].uninstalls, 195);
    }
}
```

- [ ] **Step 3: Run to verify failure**

```bash
cd backend && cargo test -p sauron-store apple
```

Expected: FAIL — `parse_report_csv` not found.

- [ ] **Step 4: Implement the parser**

Replace the `apple.rs` stub:

```rust
//! Apple App Store install and deletion reports.
//!
//! The classic Sales & Trends API reports downloads but has no concept of an
//! uninstall. Deletions exist only in the Analytics Reports API, which is
//! request-then-poll:
//!
//!   1. POST /v1/analyticsReportRequests           (accessType ONGOING, once)
//!   2. GET  /v1/analyticsReportRequests/{id}/reports
//!   3. GET  /v1/analyticsReports/{id}/instances?filter[granularity]=DAILY
//!   4. GET  /v1/analyticsReportInstances/{id}/segments  -> gzipped CSV url
//!
//! Apple takes roughly 24-48h after step 1 before the first instance exists.
//! That window is `AppleProgress::Pending` — a normal state, not an error.
//! Rendering it as a failure trains admins to ignore a badge that will later
//! mean something real.

use std::collections::BTreeMap;
use std::io::Read;

use anyhow::Context;
use chrono::NaiveDate;
use serde::Deserialize;

use crate::{column_index, DailyMetric};

pub const APPLE_HOST: &str = "api.appstoreconnect.apple.com";

const REPORT_NAME: &str = "App Store Installations and Deletions";
const COL_DATE: &str = "Date";
const COL_INSTALLS: &str = "Installations";
const COL_DELETIONS: &str = "Deletions";

#[derive(Debug, Clone, Deserialize)]
pub struct AppleIdentifiers {
    pub bundle_id: String,
    /// The numeric App Store id (Apple calls it the "Apple ID" of the app).
    pub apple_app_id: String,
    /// App Store Connect API key issuer UUID.
    pub issuer_id: String,
    /// App Store Connect API key id.
    pub key_id: String,
    /// Vendor number from Sales & Trends. Stored as TEXT: it is an opaque
    /// identifier with leading zeros, not a quantity.
    pub vendor_number: String,
}

/// Whether Apple has published anything yet.
#[derive(Debug)]
pub enum AppleProgress {
    /// Report requested; Apple has not produced an instance yet (~24-48h).
    Pending,
    Ready(Vec<DailyMetric>),
}

/// Parse one gzipped report segment.
///
/// Rows are SUMMED per day: Apple segments a day across several rows, so the
/// daily total is their sum. Taking the last row per day under-reports
/// silently, which is the worst kind of wrong here.
pub fn parse_report_csv(gzipped: &[u8]) -> anyhow::Result<Vec<DailyMetric>> {
    let mut text = String::new();
    flate2::read::GzDecoder::new(gzipped)
        .read_to_string(&mut text)
        .context("report segment is not valid gzip")?;

    let mut rdr = csv::Reader::from_reader(text.as_bytes());
    let headers = rdr.headers().context("report segment has no header row")?.clone();

    let i_date = column_index(&headers, COL_DATE)?;
    let i_installs = column_index(&headers, COL_INSTALLS)?;
    let i_deletions = column_index(&headers, COL_DELETIONS)?;

    let mut by_day: BTreeMap<NaiveDate, (i64, i64)> = BTreeMap::new();
    for rec in rdr.records() {
        let rec = rec.context("malformed row in Apple report segment")?;
        let raw_day = rec.get(i_date).unwrap_or_default().trim();
        if raw_day.is_empty() {
            continue;
        }
        let day = NaiveDate::parse_from_str(raw_day, "%Y-%m-%d")
            .with_context(|| format!("unparseable Date {raw_day:?} in Apple report"))?;
        let entry = by_day.entry(day).or_insert((0, 0));
        entry.0 += parse_count(rec.get(i_installs));
        entry.1 += parse_count(rec.get(i_deletions));
    }

    Ok(by_day
        .into_iter()
        .map(|(day, (installs, uninstalls))| DailyMetric { day, installs, uninstalls })
        .collect())
}

fn parse_count(cell: Option<&str>) -> i64 {
    cell.unwrap_or_default().trim().replace(',', "").parse::<i64>().unwrap_or(0)
}
```

- [ ] **Step 5: Run parser tests**

```bash
cd backend && cargo test -p sauron-store apple
```

Expected: 4 passed.

- [ ] **Step 6: Implement the API walk**

Append to `apple.rs`:

```rust
#[derive(Debug, serde::Serialize)]
struct AppleClaims<'a> {
    iss: &'a str,
    iat: i64,
    exp: i64,
    aud: &'a str,
}

/// ES256, signed with the .p8. `kid` is the key id; `aud` is fixed by Apple.
fn bearer(ids: &AppleIdentifiers, p8_pem: &str) -> anyhow::Result<String> {
    let now = chrono::Utc::now().timestamp();
    let claims = AppleClaims {
        iss: &ids.issuer_id,
        iat: now,
        // Apple rejects tokens with a lifetime over 20 minutes.
        exp: now + 900,
        aud: "appstoreconnect-v1",
    };
    let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::ES256);
    header.kid = Some(ids.key_id.clone());
    let key = jsonwebtoken::EncodingKey::from_ec_pem(p8_pem.as_bytes())
        .context("stored Apple credential is not a valid .p8 EC private key")?;
    Ok(jsonwebtoken::encode(&header, &claims, &key)?)
}

#[derive(Deserialize)]
struct DataList {
    data: Vec<Resource>,
}

#[derive(Deserialize)]
struct Resource {
    id: String,
    #[serde(default)]
    attributes: serde_json::Value,
}

async fn get_json(
    client: &reqwest::Client,
    token: &str,
    url: &str,
) -> anyhow::Result<DataList> {
    let resp = client.get(url).bearer_auth(token).send().await?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    anyhow::ensure!(
        status.is_success(),
        "App Store Connect {url} failed ({status}): {}",
        body.chars().take(300).collect::<String>()
    );
    serde_json::from_str(&body).context("unexpected App Store Connect response shape")
}

/// Fetch installs and deletions, creating the ongoing report request on first
/// use. Returns the request id so the caller can persist it in `sync_state` —
/// creating a second request for the same app is wasteful and Apple may reject
/// it.
pub async fn fetch(
    client: &reqwest::Client,
    ids: &AppleIdentifiers,
    p8_pem: &str,
    request_id: Option<&str>,
    since: NaiveDate,
    today: NaiveDate,
) -> anyhow::Result<(String, AppleProgress)> {
    let token = bearer(ids, p8_pem)?;
    let base = format!("https://{APPLE_HOST}");

    let request_id = match request_id {
        Some(id) => id.to_string(),
        None => create_report_request(client, &token, &base, &ids.apple_app_id).await?,
    };

    let reports = get_json(
        client,
        &token,
        &format!("{base}/v1/analyticsReportRequests/{request_id}/reports?filter[name]={}",
            REPORT_NAME.replace(' ', "%20")),
    )
    .await?;

    let Some(report) = reports.data.into_iter().next() else {
        // Requested, nothing produced yet.
        return Ok((request_id, AppleProgress::Pending));
    };

    let instances = get_json(
        client,
        &token,
        &format!(
            "{base}/v1/analyticsReports/{}/instances?filter[granularity]=DAILY",
            report.id
        ),
    )
    .await?;

    let mut out: Vec<DailyMetric> = Vec::new();
    for inst in instances.data {
        // `processingDate` is the day this instance covers; skip instances
        // outside the window rather than downloading every segment ever made.
        if let Some(d) = inst.attributes.get("processingDate").and_then(|v| v.as_str()) {
            if let Ok(day) = NaiveDate::parse_from_str(d, "%Y-%m-%d") {
                if day < since || day > today {
                    continue;
                }
            }
        }
        let segments = get_json(
            client,
            &token,
            &format!("{base}/v1/analyticsReportInstances/{}/segments", inst.id),
        )
        .await?;
        for seg in segments.data {
            let Some(url) = seg.attributes.get("url").and_then(|v| v.as_str()) else {
                continue;
            };
            // Segment URLs are pre-signed and point at Apple-controlled hosts.
            let bytes = client.get(url).send().await?.bytes().await?;
            out.extend(parse_report_csv(&bytes)?);
        }
    }

    if out.is_empty() {
        return Ok((request_id, AppleProgress::Pending));
    }

    // Segments from different instances can repeat a day; fold again.
    let mut by_day: BTreeMap<NaiveDate, (i64, i64)> = BTreeMap::new();
    for m in out {
        let e = by_day.entry(m.day).or_insert((0, 0));
        e.0 += m.installs;
        e.1 += m.uninstalls;
    }
    let merged = by_day
        .into_iter()
        .filter(|(day, _)| *day >= since && *day <= today)
        .map(|(day, (installs, uninstalls))| DailyMetric { day, installs, uninstalls })
        .collect();

    Ok((request_id, AppleProgress::Ready(merged)))
}

async fn create_report_request(
    client: &reqwest::Client,
    token: &str,
    base: &str,
    apple_app_id: &str,
) -> anyhow::Result<String> {
    let body = serde_json::json!({
        "data": {
            "type": "analyticsReportRequests",
            "attributes": { "accessType": "ONGOING", "name": "Sauron install metrics" },
            "relationships": {
                "app": { "data": { "type": "apps", "id": apple_app_id } }
            }
        }
    });
    let resp = client
        .post(format!("{base}/v1/analyticsReportRequests"))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    anyhow::ensure!(
        status.is_success(),
        "creating the Apple report request failed ({status}): {}",
        text.chars().take(300).collect::<String>()
    );
    let v: serde_json::Value = serde_json::from_str(&text)?;
    v["data"]["id"]
        .as_str()
        .map(str::to_string)
        .context("Apple report request response had no data.id")
}
```

- [ ] **Step 7: Verify the crate compiles and all tests pass**

```bash
cd backend && cargo test -p sauron-store
```

Expected: 11 passed.

---

### Task 6: The `sauron-storesync` daemon

**Files:**
- Create: `backend/bins/sauron-storesync/Cargo.toml`
- Create: `backend/bins/sauron-storesync/src/main.rs`
- Modify: `backend/crates/sauron-core/src/config.rs`

**Interfaces:**
- Consumes: everything from Tasks 2–5.
- Produces: the running daemon. Config fields `store_sync_interval_secs: i64`, `store_sync_max_concurrency: usize`, `store_backfill_days: i64`.

- [ ] **Step 1: Add config fields**

In `config.rs`, add to the `Config` struct and its `from_env`, following the existing `monitor_*` fields as the pattern:

```rust
/// How often each store connection is re-synced. Store reports are daily
/// and lag 1-3 days, so polling faster buys nothing.
pub store_sync_interval_secs: i64,
pub store_sync_max_concurrency: usize,
/// How far back the first sync of a new connection reaches.
pub store_backfill_days: i64,
```

```rust
store_sync_interval_secs: var("STORE_SYNC_INTERVAL_SECS")
    .ok()
    .and_then(|v| v.parse().ok())
    .unwrap_or(21_600),
store_sync_max_concurrency: var("STORE_SYNC_MAX_CONCURRENCY")
    .ok()
    .and_then(|v| v.parse().ok())
    .unwrap_or(8),
store_backfill_days: var("STORE_BACKFILL_DAYS")
    .ok()
    .and_then(|v| v.parse().ok())
    .unwrap_or(90),
```

- [ ] **Step 2: Write the manifest**

`backend/bins/sauron-storesync/Cargo.toml`:

```toml
[package]
name = "sauron-storesync"
version.workspace = true
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[[bin]]
name = "sauron-storesync"
path = "src/main.rs"

[dependencies]
sauron-core = { workspace = true }
sauron-db = { workspace = true }
sauron-store = { workspace = true }
sauron-alerts = { workspace = true }
sauron-telemetry = { workspace = true }
tokio = { workspace = true }
reqwest = { workspace = true, features = ["json"] }
chrono = { workspace = true }
uuid = { workspace = true }
serde_json = { workspace = true }
tracing = { workspace = true }
anyhow = { workspace = true }
```

- [ ] **Step 3: Write the daemon**

`backend/bins/sauron-storesync/src/main.rs`:

```rust
//! `sauron-storesync` — pulls daily install/uninstall counts from Google Play
//! and the Apple App Store.
//!
//! Same shape as `sauron-monitor`: claim due rows FOR UPDATE SKIP LOCKED, fetch
//! concurrently, persist, reschedule. One connection's failure is written to
//! that connection's `last_error` and touches nothing else — a store outage for
//! one tenant must not stall every other tenant's sync.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Semaphore;
use tracing::{info, warn};

use sauron_alerts::SecretCipher;
use sauron_core::Config;
use sauron_db::models::AppStoreConnection;
use sauron_db::{repo, PgPool};
use sauron_store::{apple, google, AppleProgress, StoreKind};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    sauron_telemetry::init("sauron-storesync");
    let cfg = Arc::new(Config::from_env()?);
    let pool = sauron_db::build_pool(&cfg.database_url, cfg.store_sync_max_concurrency + 4)?;

    // Fail-closed, no JWT_SECRET derivation. A key mismatch here would surface
    // only as a stream of decrypt errors hours later, so it is a boot failure.
    let cipher = Arc::new(SecretCipher::new(&cfg.require_notify_secret_key()?));

    // Prove the configured key can actually open what is stored, in the style
    // of the API's channel-secret self-test. A silently wrong key otherwise
    // looks exactly like "every store credential is invalid".
    {
        let mut conn = pool.get().await?;
        if let Some(blob) = repo::any_store_secret_enc(&mut conn).await? {
            cipher.decrypt(&blob).map_err(|_| {
                anyhow::anyhow!(
                    "NOTIFY_SECRET_KEY cannot decrypt stored store credentials — \
                     refusing to start rather than reporting every connection as broken"
                )
            })?;
        }
    }

    let http = reqwest::Client::builder()
        .user_agent("Sauron-StoreSync/1.0")
        .timeout(Duration::from_secs(120))
        .build()?;

    let sem = Arc::new(Semaphore::new(cfg.store_sync_max_concurrency));
    info!(
        interval_secs = cfg.store_sync_interval_secs,
        concurrency = cfg.store_sync_max_concurrency,
        "store sync started"
    );

    loop {
        let claimed = {
            let mut conn = pool.get().await?;
            repo::claim_due_store_connections(&mut conn, 50, cfg.store_sync_interval_secs).await?
        };

        if !claimed.is_empty() {
            info!(count = claimed.len(), "syncing store connections");
        }

        let mut tasks = Vec::new();
        for c in claimed {
            let permit = sem.clone().acquire_owned().await?;
            let (pool, http, cipher, cfg) =
                (pool.clone(), http.clone(), cipher.clone(), cfg.clone());
            tasks.push(tokio::spawn(async move {
                let _permit = permit;
                let id = c.id;
                if let Err(e) = sync_one(&pool, &http, &cipher, &cfg, c).await {
                    warn!(connection_id = %id, error = %e, "store sync failed");
                    if let Ok(mut conn) = pool.get().await {
                        let _ = repo::record_store_sync_result(
                            &mut conn,
                            id,
                            Some(&e.to_string()),
                        )
                        .await;
                    }
                }
            }));
        }
        for t in tasks {
            let _ = t.await;
        }

        tokio::time::sleep(Duration::from_secs(60)).await;
    }
}

async fn sync_one(
    pool: &PgPool,
    http: &reqwest::Client,
    cipher: &SecretCipher,
    cfg: &Config,
    c: AppStoreConnection,
) -> anyhow::Result<()> {
    let kind = StoreKind::parse(&c.store)
        .ok_or_else(|| anyhow::anyhow!("unknown store {:?}", c.store))?;
    let blob = c
        .secret_enc
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("no credential saved for this store"))?;
    let secret = cipher.decrypt_str(blob)?;

    let today = chrono::Utc::now().date_naive();
    // Backfill only on a connection that has never synced; afterwards a short
    // window is enough and re-reading a year of reports every 6 hours is waste.
    let lookback = if c.last_synced_at.is_none() { cfg.store_backfill_days } else { 7 };
    let since = today - chrono::Duration::days(lookback);

    let metrics = match kind {
        StoreKind::GooglePlay => {
            let ids: google::GoogleIdentifiers = serde_json::from_value(c.identifiers.clone())?;
            google::fetch(http, &ids, &secret, since, today).await?
        }
        StoreKind::AppStore => {
            let ids: apple::AppleIdentifiers = serde_json::from_value(c.identifiers.clone())?;
            let request_id = c.sync_state.get("report_request_id").and_then(|v| v.as_str());
            let (new_id, progress) =
                apple::fetch(http, &ids, &secret, request_id, since, today).await?;

            if request_id != Some(new_id.as_str()) {
                let mut conn = pool.get().await?;
                repo::set_store_sync_state(
                    &mut conn,
                    c.id,
                    &serde_json::json!({ "report_request_id": new_id }),
                )
                .await?;
            }

            match progress {
                // Apple's normal 24-48h startup window. Recorded as a clean
                // sync with no rows, NOT as an error — see the module docs.
                AppleProgress::Pending => {
                    info!(connection_id = %c.id, "Apple report still pending");
                    let mut conn = pool.get().await?;
                    repo::record_store_sync_result(&mut conn, c.id, None).await?;
                    return Ok(());
                }
                AppleProgress::Ready(m) => m,
            }
        }
    };

    let rows: Vec<_> = metrics
        .iter()
        .map(|m| (m.day, m.installs, m.uninstalls))
        .collect();

    let mut conn = pool.get().await?;
    repo::upsert_store_daily_metrics(&mut conn, c.app_id, kind.as_str(), &rows).await?;
    repo::record_store_sync_result(&mut conn, c.id, None).await?;
    info!(connection_id = %c.id, days = rows.len(), "store sync ok");
    Ok(())
}
```

- [ ] **Step 4: Add the two helpers the daemon needs**

In `repo.rs`:

```rust
/// Any one stored store credential, for proving the configured key can open
/// what is on disk. Mirrors `any_channel_secret_enc`.
pub async fn any_store_secret_enc(
    conn: &mut AsyncPgConnection,
) -> QueryResult<Option<Vec<u8>>> {
    app_store_connections::table
        .filter(app_store_connections::secret_enc.is_not_null())
        .select(app_store_connections::secret_enc)
        .first::<Option<Vec<u8>>>(conn)
        .await
        .optional()
        .map(Option::flatten)
}
```

In `config.rs`, confirm a `require_notify_secret_key()` accessor exists (the monitor already fails closed on this key). If it is named differently there, use that name — do not add a second accessor.

- [ ] **Step 5: Verify the workspace builds**

```bash
cd backend && cargo check --workspace
```

Expected: clean.

---

### Task 7: Store connection API routes

**Files:**
- Create: `backend/bins/sauron-api/src/routes/stores.rs`
- Modify: `backend/bins/sauron-api/src/routes/mod.rs` (add `pub mod stores;`)
- Modify: `backend/bins/sauron-api/src/main.rs` (register routes)
- Test: `backend/bins/sauron-api/tests/http_stores.rs` (create)

**Interfaces:**
- Consumes: repo functions from Task 2, identifier structs from Tasks 4–5.
- Produces: `StoreConnectionOut { store, enabled, identifiers, has_secret, secret_updated_at, state, last_synced_at, last_error }` where `state` is one of `never_synced | pending | ok | error`.

- [ ] **Step 1: Write the failing tests**

`backend/bins/sauron-api/tests/http_stores.rs` — follow the harness in `tests/http_alerting.rs`:

```rust
mod common;

#[tokio::test]
async fn secret_never_appears_in_any_response_body() {
    // Asserted against RAW JSON, not a typed struct: a struct assertion keeps
    // passing on the day someone adds `secret_enc` back to the response type.
    let h = common::harness().await;
    let app = h.app().await;

    h.put_json(
        &format!("/v1/apps/{}/store-connections/google_play", app.id),
        serde_json::json!({
            "identifiers": {"package_name": "com.example.app", "gcs_bucket": "pubsite_prod_rev_1"},
            "secret": "SUPER-SECRET-SERVICE-ACCOUNT"
        }),
    )
    .await
    .assert_status(200);

    let body = h
        .get(&format!("/v1/apps/{}/store-connections", app.id))
        .await
        .text()
        .await;

    assert!(!body.contains("SUPER-SECRET"), "plaintext leaked: {body}");
    assert!(!body.contains("secret_enc"), "ciphertext field leaked: {body}");
    assert!(body.contains("\"has_secret\":true"));
}

#[tokio::test]
async fn put_without_secret_field_preserves_the_stored_credential() {
    let h = common::harness().await;
    let app = h.app().await;
    let path = format!("/v1/apps/{}/store-connections/google_play", app.id);

    h.put_json(&path, serde_json::json!({
        "identifiers": {"package_name": "com.example.app", "gcs_bucket": "b"},
        "secret": "keep-me"
    })).await.assert_status(200);

    h.put_json(&path, serde_json::json!({
        "identifiers": {"package_name": "com.example.renamed", "gcs_bucket": "b"}
    })).await.assert_status(200);

    let body = h.get(&format!("/v1/apps/{}/store-connections", app.id)).await.text().await;
    assert!(body.contains("\"has_secret\":true"), "editing ids wiped the secret: {body}");
    assert!(body.contains("com.example.renamed"));
}

#[tokio::test]
async fn app_read_cannot_write_connections() {
    let h = common::harness().await;
    let app = h.app().await;
    let reader = h.member_with(&["app:read"]).await;

    h.as_user(&reader)
        .put_json(
            &format!("/v1/apps/{}/store-connections/google_play", app.id),
            serde_json::json!({"identifiers": {"package_name": "x", "gcs_bucket": "y"}}),
        )
        .await
        .assert_status(403);
}

#[tokio::test]
async fn store_environment_id_from_another_app_is_rejected() {
    let h = common::harness().await;
    let app_a = h.app().await;
    let app_b = h.app().await;
    let env_b = h.default_environment(app_b.id).await;

    h.patch_json(
        &format!("/v1/apps/{}", app_a.id),
        serde_json::json!({"name": app_a.name, "store_environment_id": env_b.id}),
    )
    .await
    .assert_status(400);
}

#[tokio::test]
async fn identifiers_are_validated_per_store() {
    let h = common::harness().await;
    let app = h.app().await;

    // Apple identifiers posted to the Google slot must not be stored as an
    // unparseable blob the daemon can only fail on hours later.
    h.put_json(
        &format!("/v1/apps/{}/store-connections/google_play", app.id),
        serde_json::json!({"identifiers": {"bundle_id": "com.example"}}),
    )
    .await
    .assert_status(400);
}

#[tokio::test]
async fn deleting_a_connection_keeps_collected_metrics() {
    let h = common::harness().await;
    let app = h.app().await;
    let path = format!("/v1/apps/{}/store-connections/google_play", app.id);

    h.put_json(&path, serde_json::json!({
        "identifiers": {"package_name": "com.example.app", "gcs_bucket": "b"}
    })).await.assert_status(200);
    h.seed_store_metrics(app.id, "google_play", "2026-08-01", 100, 10).await;

    h.delete(&path).await.assert_status(204);

    let body = h
        .get(&format!("/v1/apps/{}/store-metrics?since_days=30", app.id))
        .await
        .text()
        .await;
    assert!(body.contains("2026-08-01"), "history must survive credential removal: {body}");
}
```

- [ ] **Step 2: Run to verify failure**

```bash
cd backend && cargo test -p sauron-api --test http_stores
```

Expected: FAIL — routes not registered (404s). Run with `dangerouslyDisableSandbox: true`.

- [ ] **Step 3: Write the handlers**

`backend/bins/sauron-api/src/routes/stores.rs`:

```rust
//! Store credential CRUD and the Overview chart feed.
//!
//! Secrets are WRITE-ONLY: no response type in this module carries the
//! credential or its ciphertext. `sauron_db::models::AppStoreConnection`
//! deliberately derives no `Serialize`, so returning one is a compile error
//! rather than a leak.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use sauron_auth::{authorize_app, perm, AuthUser};
use sauron_db::repo;
use sauron_store::{apple::AppleIdentifiers, google::GoogleIdentifiers, StoreKind};

use super::db;
use crate::error::ApiError;
use crate::AppState;

#[derive(Serialize)]
pub struct StoreConnectionOut {
    pub store: String,
    pub enabled: bool,
    pub identifiers: serde_json::Value,
    pub has_secret: bool,
    pub secret_updated_at: Option<DateTime<Utc>>,
    /// `never_synced` | `pending` | `ok` | `error`
    pub state: &'static str,
    pub last_synced_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
}

fn to_out(c: sauron_db::models::AppStoreConnection) -> StoreConnectionOut {
    let state = match (&c.last_error, c.last_synced_at) {
        (Some(_), _) => "error",
        (None, None) => "never_synced",
        // Apple synced cleanly but has published nothing yet: the report
        // request exists and no rows arrived. Normal, not a failure.
        (None, Some(_))
            if c.store == StoreKind::AppStore.as_str()
                && c.sync_state.get("report_request_id").is_some()
                && c.last_synced_at.is_some() =>
        {
            "ok"
        }
        (None, Some(_)) => "ok",
    };
    StoreConnectionOut {
        store: c.store,
        enabled: c.enabled,
        identifiers: c.identifiers,
        has_secret: c.secret_enc.is_some(),
        secret_updated_at: Some(c.updated_at),
        state,
        last_synced_at: c.last_synced_at,
        last_error: c.last_error,
    }
}

pub async fn list(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
) -> Result<Json<Vec<StoreConnectionOut>>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::APP_READ).await?;
    let rows = repo::list_store_connections(&mut conn, app_id).await?;
    Ok(Json(rows.into_iter().map(to_out).collect()))
}

#[derive(Deserialize)]
pub struct UpsertReq {
    pub identifiers: serde_json::Value,
    /// Absent = leave the stored credential alone. `null` = clear it.
    /// Present = replace. Without the double Option, saving an edited package
    /// name silently wipes the credential.
    #[serde(default, deserialize_with = "double_option")]
    pub secret: Option<Option<String>>,
}

fn double_option<'de, D>(d: D) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Some(Option::deserialize(d)?))
}

/// Reject identifiers that do not match the store slot they were posted to.
/// Storing them unvalidated turns a typo into a daemon error six hours later.
fn validate_identifiers(kind: StoreKind, v: &serde_json::Value) -> Result<serde_json::Value, ApiError> {
    match kind {
        StoreKind::GooglePlay => {
            let mut ids: GoogleIdentifiers = serde_json::from_value(v.clone())
                .map_err(|e| ApiError::bad_request(format!("invalid Google Play identifiers: {e}")))?;
            // Operators paste `gs://bucket`; store the bare name so the object
            // URL cannot end up with a doubled scheme.
            ids.gcs_bucket = ids.gcs_bucket.trim_start_matches("gs://").trim_end_matches('/').to_string();
            Ok(serde_json::json!({
                "package_name": ids.package_name,
                "gcs_bucket": ids.gcs_bucket,
            }))
        }
        StoreKind::AppStore => {
            let ids: AppleIdentifiers = serde_json::from_value(v.clone())
                .map_err(|e| ApiError::bad_request(format!("invalid App Store identifiers: {e}")))?;
            Ok(serde_json::json!({
                "bundle_id": ids.bundle_id,
                "apple_app_id": ids.apple_app_id,
                "issuer_id": ids.issuer_id,
                "key_id": ids.key_id,
                "vendor_number": ids.vendor_number,
            }))
        }
    }
}

pub async fn upsert(
    auth: AuthUser,
    State(state): State<AppState>,
    Path((app_id, store)): Path<(Uuid, String)>,
    Json(req): Json<UpsertReq>,
) -> Result<Json<StoreConnectionOut>, ApiError> {
    let kind = StoreKind::parse(&store)
        .ok_or_else(|| ApiError::bad_request("unknown store".to_string()))?;
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::APP_UPDATE).await?;

    let identifiers = validate_identifiers(kind, &req.identifiers)?;
    let secret_enc = match req.secret {
        None => None,
        Some(None) => Some(None),
        Some(Some(plain)) => Some(Some(state.secret_cipher.encrypt_str(&plain)?)),
    };

    let row =
        repo::upsert_store_connection(&mut conn, app_id, kind.as_str(), &identifiers, secret_enc)
            .await?;
    Ok(Json(to_out(row)))
}

pub async fn delete(
    auth: AuthUser,
    State(state): State<AppState>,
    Path((app_id, store)): Path<(Uuid, String)>,
) -> Result<StatusCode, ApiError> {
    let kind = StoreKind::parse(&store)
        .ok_or_else(|| ApiError::bad_request("unknown store".to_string()))?;
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::APP_UPDATE).await?;
    repo::delete_store_connection(&mut conn, app_id, kind.as_str()).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn queue_sync(
    auth: AuthUser,
    State(state): State<AppState>,
    Path((app_id, store)): Path<(Uuid, String)>,
) -> Result<StatusCode, ApiError> {
    let kind = StoreKind::parse(&store)
        .ok_or_else(|| ApiError::bad_request("unknown store".to_string()))?;
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::APP_UPDATE).await?;
    // Only moves next_sync_at. The daemon does the work — a multi-minute Apple
    // download must never run inside an HTTP request.
    repo::queue_store_sync(&mut conn, app_id, kind.as_str()).await?;
    Ok(StatusCode::ACCEPTED)
}
```

Add `pub mod stores;` to `routes/mod.rs`, and register in `main.rs` beside the other app-scoped routes:

```rust
.route(
    "/v1/apps/{app_id}/store-connections",
    get(routes::stores::list),
)
.route(
    "/v1/apps/{app_id}/store-connections/{store}",
    put(routes::stores::upsert).delete(routes::stores::delete),
)
.route(
    "/v1/apps/{app_id}/store-connections/{store}/sync",
    post(routes::stores::queue_sync),
)
```

If `AppState` has no `secret_cipher` field, add one built the same way the alerting routes build theirs — do not construct a second cipher per request.

- [ ] **Step 4: Run tests**

```bash
cd backend && cargo test -p sauron-api --test http_stores
```

Expected: the four connection tests pass; `store_environment_id_from_another_app_is_rejected` and `deleting_a_connection_keeps_collected_metrics` still fail (Task 8 adds those endpoints).

---

### Task 8: `store_environment_id` and the chart feed

**Files:**
- Modify: `backend/bins/sauron-api/src/routes/apps.rs:47-56` (`UpdateAppReq`)
- Modify: `backend/bins/sauron-api/src/routes/stores.rs` (add `metrics` handler)
- Modify: `backend/bins/sauron-api/src/main.rs`
- Modify: `backend/crates/sauron-db/src/repo.rs` (`set_app_store_environment`)

**Interfaces:**
- Consumes: `repo::store_metrics_range`, `repo::list_store_connections`.
- Produces: `GET /v1/apps/{id}/store-metrics?since_days=N` returning `StoreMetricsOut { series: Vec<StoreDayOut>, pending_days: Vec<PendingDay>, stores: Vec<StoreStatusOut> }`.

- [ ] **Step 1: Write the failing test**

Append to `http_stores.rs`:

```rust
#[tokio::test]
async fn missing_days_are_pending_not_zero_filled() {
    // A zero bar asserts "nobody installed the app that day". The truth is
    // "the store has not published that day yet". Zero-filling turns a gap
    // into a confident lie, which is exactly the silent-drop class this
    // codebase has been bitten by before.
    let h = common::harness().await;
    let app = h.app().await;
    h.put_json(&format!("/v1/apps/{}/store-connections/google_play", app.id),
        serde_json::json!({"identifiers": {"package_name": "p", "gcs_bucket": "b"}}))
        .await.assert_status(200);

    // Seed only ONE day inside a 7-day window.
    let day = (chrono::Utc::now().date_naive() - chrono::Duration::days(3)).to_string();
    h.seed_store_metrics(app.id, "google_play", &day, 100, 10).await;

    let body: serde_json::Value = h
        .get(&format!("/v1/apps/{}/store-metrics?since_days=7", app.id))
        .await
        .json()
        .await;

    assert_eq!(body["series"].as_array().unwrap().len(), 1, "only real days in series");
    assert!(
        !body["pending_days"].as_array().unwrap().is_empty(),
        "unpublished days must be reported, not silently absent"
    );
}
```

- [ ] **Step 2: Run to verify failure**

```bash
cd backend && cargo test -p sauron-api --test http_stores
```

Expected: FAIL — 404 on `/store-metrics`.

- [ ] **Step 3: Extend `UpdateAppReq`**

In `apps.rs`, add to `UpdateAppReq`:

```rust
/// Which environment represents the store build. Absent leaves it alone;
/// explicit `null` clears the designation and hides the Overview section.
#[serde(default, deserialize_with = "double_option_uuid")]
pub store_environment_id: Option<Option<Uuid>>,
```

In `update_app`, after the existing authorization, validate and persist:

```rust
if let Some(env) = req.store_environment_id {
    if let Some(env_id) = env {
        // Must be an enrollment OF THIS APP. Accepting any UUID stores a
        // designation that can never match the switcher, hiding the section
        // forever with no error to explain why.
        let owned = repo::app_environment_belongs_to_app(&mut conn, env_id, app_id).await?;
        if !owned {
            return Err(ApiError::bad_request(
                "store_environment_id is not an environment of this app".to_string(),
            ));
        }
    }
    repo::set_app_store_environment(&mut conn, app_id, env).await?;
}
```

Add to `repo.rs`:

```rust
pub async fn app_environment_belongs_to_app(
    conn: &mut AsyncPgConnection,
    env_id: Uuid,
    app_id: Uuid,
) -> QueryResult<bool> {
    use diesel::dsl::count_star;
    let n: i64 = app_environments::table
        .filter(app_environments::id.eq(env_id))
        .filter(app_environments::app_id.eq(app_id))
        .select(count_star())
        .first(conn)
        .await?;
    Ok(n > 0)
}

pub async fn set_app_store_environment(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    env_id: Option<Uuid>,
) -> QueryResult<usize> {
    diesel::update(apps::table.find(app_id))
        .set((
            apps::store_environment_id.eq(env_id),
            apps::updated_at.eq(diesel::dsl::now),
        ))
        .execute(conn)
        .await
}
```

- [ ] **Step 4: Add the metrics handler**

Append to `stores.rs`:

```rust
#[derive(Serialize)]
pub struct StoreCounts {
    pub installs: i64,
    pub uninstalls: i64,
}

#[derive(Serialize)]
pub struct StoreDayOut {
    pub day: NaiveDate,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub google_play: Option<StoreCounts>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_store: Option<StoreCounts>,
}

#[derive(Serialize)]
pub struct PendingDay {
    pub day: NaiveDate,
    /// Rendered verbatim by the dashboard.
    pub reason: String,
}

#[derive(Serialize)]
pub struct StoreMetricsOut {
    pub series: Vec<StoreDayOut>,
    pub pending_days: Vec<PendingDay>,
    pub stores: Vec<StoreConnectionOut>,
}

#[derive(Deserialize)]
pub struct MetricsQuery {
    #[serde(default = "default_since_days")]
    pub since_days: i64,
}

fn default_since_days() -> i64 {
    30
}

pub async fn metrics(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    axum::extract::Query(q): axum::extract::Query<MetricsQuery>,
) -> Result<Json<StoreMetricsOut>, ApiError> {
    let days = q.since_days.clamp(1, 365);
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::APP_READ).await?;

    let today = Utc::now().date_naive();
    let since = today - chrono::Duration::days(days);
    let rows = repo::store_metrics_range(&mut conn, app_id, since).await?;
    let connections = repo::list_store_connections(&mut conn, app_id).await?;

    let mut by_day: std::collections::BTreeMap<NaiveDate, StoreDayOut> = Default::default();
    for r in rows {
        let e = by_day.entry(r.day).or_insert(StoreDayOut {
            day: r.day,
            google_play: None,
            app_store: None,
        });
        let counts = StoreCounts { installs: r.installs, uninstalls: r.uninstalls };
        if r.store == StoreKind::GooglePlay.as_str() {
            e.google_play = Some(counts);
        } else {
            e.app_store = Some(counts);
        }
    }

    // Days inside the window with no row are PENDING, never zero-filled.
    // Today and yesterday are excluded from the pending list because every
    // store lags 1-3 days: listing them would flag the normal case forever.
    let mut pending_days = Vec::new();
    let mut d = since;
    while d <= today - chrono::Duration::days(2) {
        if !by_day.contains_key(&d) {
            pending_days.push(PendingDay {
                day: d,
                reason: "the store has not published this day yet".to_string(),
            });
        }
        d += chrono::Duration::days(1);
    }

    Ok(Json(StoreMetricsOut {
        series: by_day.into_values().collect(),
        pending_days,
        stores: connections.into_iter().map(to_out).collect(),
    }))
}
```

Register:

```rust
.route(
    "/v1/apps/{app_id}/store-metrics",
    get(routes::stores::metrics),
)
```

- [ ] **Step 5: Run the whole API test file**

```bash
cd backend && cargo test -p sauron-api --test http_stores
```

Expected: 8 passed.

- [ ] **Step 6: Run the full backend suite for regressions**

```bash
cd backend && cargo test --workspace
```

Expected: **at least 1391 passing** (the pre-existing baseline) plus the new tests, zero failures. Run with `dangerouslyDisableSandbox: true` and host-network containers. A total below 1391 with no failures means tests were skipped, not that they passed.

---

### Task 9: Dashboard API client and models

**Files:**
- Create: `dashboard/src/lib/api/stores.ts`
- Modify: `dashboard/src/lib/models/index.ts` (add `store_environment_id` to `App`)

**Interfaces:**
- Consumes: the Task 7–8 endpoints.
- Produces: `listStoreConnections`, `upsertStoreConnection`, `deleteStoreConnection`, `queueStoreSync`, `getStoreMetrics`, and the types `StoreConnection`, `StoreMetrics`, `StoreDay`, `StoreKind`.

- [ ] **Step 1: Add the field to the `App` model**

In `dashboard/src/lib/models/index.ts`, add to the `App` interface:

```ts
  /**
   * The environment whose build ships to the app stores, or null.
   *
   * This is an APP_ENVIRONMENTS id — the same id the environment switcher
   * carries — so it can be compared directly against
   * `sessionStore.currentEnvironmentId`.
   */
  store_environment_id: string | null;
```

- [ ] **Step 2: Write the client**

`dashboard/src/lib/api/stores.ts`:

```ts
import { api } from './client';

export type StoreKind = 'google_play' | 'app_store';

/** `never_synced` before the daemon's first pass; `pending` while Apple is still producing its first report. */
export type StoreState = 'never_synced' | 'pending' | 'ok' | 'error';

export interface StoreConnection {
  store: StoreKind;
  enabled: boolean;
  /** Shape depends on `store`; see the settings card for the per-store fields. */
  identifiers: Record<string, string>;
  /** The credential itself is never returned — only whether one is stored. */
  has_secret: boolean;
  secret_updated_at: string | null;
  state: StoreState;
  last_synced_at: string | null;
  last_error: string | null;
}

export interface StoreCounts {
  installs: number;
  uninstalls: number;
}

/**
 * One day. A store key is ABSENT when that store published nothing for the
 * day — it is deliberately not `{installs: 0}`, because zero is a real value
 * that means something different.
 */
export interface StoreDay {
  day: string;
  google_play?: StoreCounts;
  app_store?: StoreCounts;
}

export interface PendingDay {
  day: string;
  /** Rendered verbatim. */
  reason: string;
}

export interface StoreMetrics {
  series: StoreDay[];
  pending_days: PendingDay[];
  stores: StoreConnection[];
}

export async function listStoreConnections(appId: string): Promise<StoreConnection[]> {
  const { data } = await api.get<StoreConnection[]>(`/v1/apps/${appId}/store-connections`);
  return data;
}

/**
 * Omit `secret` to leave the stored credential untouched; pass `null` to clear
 * it. Sending `secret: ''` would store an empty credential — the caller must
 * not turn an untouched password field into an empty string.
 */
export async function upsertStoreConnection(
  appId: string,
  store: StoreKind,
  body: { identifiers: Record<string, string>; secret?: string | null },
): Promise<StoreConnection> {
  const { data } = await api.put<StoreConnection>(
    `/v1/apps/${appId}/store-connections/${store}`,
    body,
  );
  return data;
}

export async function deleteStoreConnection(appId: string, store: StoreKind): Promise<void> {
  await api.delete(`/v1/apps/${appId}/store-connections/${store}`);
}

/** Makes the connection due now. The daemon does the work on its next pass. */
export async function queueStoreSync(appId: string, store: StoreKind): Promise<void> {
  await api.post(`/v1/apps/${appId}/store-connections/${store}/sync`);
}

export async function getStoreMetrics(appId: string, sinceDays = 30): Promise<StoreMetrics> {
  const { data } = await api.get<StoreMetrics>(`/v1/apps/${appId}/store-metrics`, {
    params: { since_days: sinceDays },
  });
  return data;
}
```

- [ ] **Step 3: Typecheck**

```bash
cd dashboard && npm run check
```

Expected: clean.

---

### Task 10: Store settings card

**Files:**
- Create: `dashboard/src/lib/components/settings/StoreConnectionsCard.svelte`
- Modify: `dashboard/src/pages/SettingsApp.svelte:147-186` (insert the card)

**Interfaces:**
- Consumes: Task 9's client, `listEnvironments` from `lib/api/environments`.
- Produces: a self-contained card taking `{ app: App, onAppUpdated: (app: App) => void }`.

- [ ] **Step 1: Write the component**

Field sets per store, matching the backend validation exactly:

- `google_play`: `package_name`, `gcs_bucket` + a textarea for the service-account JSON.
- `app_store`: `bundle_id`, `apple_app_id`, `issuer_id`, `key_id`, `vendor_number` + a textarea for the `.p8`.

Requirements the component must satisfy:

1. Every identifier input is `<input type="text">`. **`vendor_number` must not be `type="number"`** — `bind:value` on a number input writes back `number | null`, which crashes the string-shaped validator, and because the save button's `disabled` is a derived, computing that guard is what throws: the DOM freezes while the button still looks clickable.
2. The secret textarea starts empty on every load and is only sent when non-empty. An untouched field must send **no** `secret` key at all, not `''`.
3. Buttons are the house `Button` component with `lockedReason={lockedBy('app:update', { app: app.id, level: 'app' })}`.
4. The environment `<select>` lists the app's enrollments plus a "None (hide store section)" option, and saving calls `updateApp(app.id, { name: app.name, store_environment_id: value || null })`.
5. Status line per store, rendered from `state`:
   - `never_synced` → "Waiting for the first sync."
   - `pending` → "App Store is preparing this report. Apple usually takes 24-48 hours after setup."
   - `ok` → "Last synced {relative time}."
   - `error` → `last_error` verbatim, in `var(--error)`.
6. The Remove confirmation says, in words: collected history is kept; only the credential is removed.
7. The sync button is labeled **"Queue sync"** and its toast reads "Queued. The sync daemon will pick this up on its next pass." — not "Syncing now".

- [ ] **Step 2: Wire it into `SettingsApp.svelte`**

Insert between the Ingest and Delete cards, inside the `.settings-stack` div:

```svelte
<StoreConnectionsCard
  {app}
  onAppUpdated={(updated) => {
    sessionStore.upsertApp(updated, false);
    viewCache.invalidate('settings.app');
    void load(app.id, true);
  }}
/>
```

Import it alongside the other components. The `viewCache.invalidate` prefix call matters for the same reason it does in `toggleIngest`: the cache key carries `scopeKey`, so a forced reload refreshes only the currently-selected environment's entry.

- [ ] **Step 3: Verify in the browser**

Start the dev server with `preview_start`, navigate to App settings, and confirm with `read_page`: the card renders, the environment dropdown lists the app's environments, and saving a Google connection round-trips (reload → `has_secret` true, package name persisted). Check `read_console_messages` for errors.

- [ ] **Step 4: Typecheck and build**

```bash
cd dashboard && npm run check && npm run build
```

---

### Task 11: Overview store section and diverging chart

**Files:**
- Create: `dashboard/src/lib/components/StoreInstallsChart.svelte`
- Create: `dashboard/src/lib/components/StoreSection.svelte`
- Create: `dashboard/src/lib/components/stores.test.ts`
- Modify: `dashboard/src/pages/Overview.svelte`

**Interfaces:**
- Consumes: `getStoreMetrics`, `StoreMetrics`.
- Produces: `divergingScale(series: StoreDay[]): number` and `shouldShowStoreSection(app, currentEnvironmentId): boolean`, both exported from `StoreSection.svelte`'s module script or a sibling `stores.ts` so the test can import them.

- [ ] **Step 1: Write the failing tests**

`dashboard/src/lib/components/stores.test.ts`:

```ts
import { describe, it, expect } from 'vitest';
import { divergingScale, shouldShowStoreSection, dayTotals } from './stores';
import type { StoreDay } from '../api/stores';

describe('divergingScale', () => {
  it('uses one scale across both directions', () => {
    // Independent scales would put a 3-uninstall day level with a
    // 300-install day. UserActivityChart documents having made exactly this
    // mistake once already.
    const series: StoreDay[] = [
      { day: '2026-08-01', google_play: { installs: 300, uninstalls: 3 } },
    ];
    expect(divergingScale(series)).toBe(300);
  });

  it('sums stores within a day before taking the max', () => {
    const series: StoreDay[] = [
      {
        day: '2026-08-01',
        google_play: { installs: 100, uninstalls: 10 },
        app_store: { installs: 80, uninstalls: 5 },
      },
    ];
    expect(divergingScale(series)).toBe(180);
  });

  it('lets uninstalls set the scale when they exceed installs', () => {
    const series: StoreDay[] = [
      { day: '2026-08-01', google_play: { installs: 5, uninstalls: 400 } },
    ];
    expect(divergingScale(series)).toBe(400);
  });

  it('never returns zero, so an all-zero range cannot divide by zero', () => {
    expect(divergingScale([{ day: '2026-08-01', google_play: { installs: 0, uninstalls: 0 } }])).toBe(1);
    expect(divergingScale([])).toBe(1);
  });
});

describe('dayTotals', () => {
  it('treats an absent store as absent, not as zero', () => {
    const d: StoreDay = { day: '2026-08-01', google_play: { installs: 10, uninstalls: 1 } };
    expect(dayTotals(d)).toEqual({ installs: 10, uninstalls: 1 });
  });
});

describe('shouldShowStoreSection', () => {
  const app = (envId: string | null) => ({ id: 'a', store_environment_id: envId }) as never;

  it('hides when no environment is designated', () => {
    expect(shouldShowStoreSection(app(null), 'env-1')).toBe(false);
  });

  it('hides when a different environment is selected', () => {
    expect(shouldShowStoreSection(app('env-1'), 'env-2')).toBe(false);
  });

  it('shows when the designated environment is selected', () => {
    expect(shouldShowStoreSection(app('env-1'), 'env-1')).toBe(true);
  });

  it('hides when no environment is selected at all', () => {
    expect(shouldShowStoreSection(app('env-1'), null)).toBe(false);
  });
});
```

- [ ] **Step 2: Run to verify failure**

```bash
cd dashboard && npx vitest run src/lib/components/stores.test.ts
```

Expected: FAIL — cannot resolve `./stores`.

- [ ] **Step 3: Write the helpers**

`dashboard/src/lib/components/stores.ts`:

```ts
import type { StoreDay } from '../api/stores';
import type { App } from '../models';

/** One day's totals across both stores. */
export function dayTotals(d: StoreDay): { installs: number; uninstalls: number } {
  return {
    installs: (d.google_play?.installs ?? 0) + (d.app_store?.installs ?? 0),
    uninstalls: (d.google_play?.uninstalls ?? 0) + (d.app_store?.uninstalls ?? 0),
  };
}

/**
 * ONE scale for both halves of the diverging chart.
 *
 * The denominator is the largest single-day total in EITHER direction, so an
 * install bar and an uninstall bar of the same height mean the same number.
 * Scaling each half to its own maximum would make a 3-uninstall day as tall as
 * a 300-install day.
 *
 * Floors at 1: an all-zero range would otherwise divide by zero.
 */
export function divergingScale(series: StoreDay[]): number {
  let max = 0;
  for (const d of series) {
    const t = dayTotals(d);
    max = Math.max(max, t.installs, t.uninstalls);
  }
  return Math.max(max, 1);
}

/**
 * The store section is visible only in the environment the admin designated as
 * the store build. The store data itself is app-wide — this gate is the whole
 * mechanism by which the designation means anything.
 */
export function shouldShowStoreSection(
  app: Pick<App, 'store_environment_id'>,
  currentEnvironmentId: string | null,
): boolean {
  return !!app.store_environment_id && app.store_environment_id === currentEnvironmentId;
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cd dashboard && npx vitest run src/lib/components/stores.test.ts
```

Expected: 9 passed.

- [ ] **Step 5: Write `StoreInstallsChart.svelte`**

Diverging bars, following `UserActivityChart.svelte`'s markup and CSS conventions:

- One column per day in `series`. Above the zero line: `google_play.installs` then `app_store.installs` stacked. Below: the two uninstall values stacked.
- Heights are `(value / divergingScale(series)) * 50` percent — each half owns half the plot.
- A day with a zero value keeps a 2% stub, matching the existing charts, so "a day that happened with none" reads differently from a gap.
- `title` attribute per column carries all four numbers plus the date, formatted with `utcDayLabel` — these are `YYYY-MM-DD` calendar days, and the default `new Date()` path renders them in the viewer's zone and labels August 1 as "Jul 31".
- A legend with the two store colors; use `var(--primary)` and `var(--accent)` (or the next house chart color), never hard-coded hex.

- [ ] **Step 6: Write `StoreSection.svelte`**

Owns its own `CachedView<StoreMetrics>`, keyed `viewKey('overview.stores', appId, sessionStore.scopeKey, sinceDays)`. Renders:

- `StatTiles` with installs, uninstalls, and net (installs − uninstalls) across the range.
- `StoreInstallsChart`.
- If `pending_days.length > 0`, a muted line: "{n} day(s) not yet published by the store." Never a zero bar.
- If any connection's `state === 'pending'`, a muted line naming the store: "App Store is still preparing this report."
- If any connection's `state === 'error'`, an inline error with `last_error`, and the chart still renders whatever the other store returned.
- `EmptyState` when `series` is empty and no store is in `error`.

- [ ] **Step 7: Wire into `Overview.svelte`**

Add the import and render it after the existing sections:

```svelte
{#if currentApp && shouldShowStoreSection(currentApp, sessionStore.currentEnvironmentId)}
  <StoreSection appId={currentApp.id} {sinceDays} />
{/if}
```

`currentApp` comes from `sessionStore`. The section fetches its own data inside `StoreSection`, so `Overview.svelte`'s existing `Promise.allSettled` batch is untouched — a store failure cannot abort the other five sections.

- [ ] **Step 8: Verify in the browser**

Seed a few days of metrics directly in Postgres for a test app, designate its environment in App settings, then load Overview with that environment selected. Confirm via `read_page` and a screenshot:

1. Section appears only in the designated environment; switching environments hides it.
2. Install bars point up, uninstall bars point down.
3. The tooltip shows all four numbers.
4. `read_console_messages` is clean.

- [ ] **Step 9: Full dashboard verification**

```bash
cd dashboard && npm run check && npx vitest run && npm run build
```

Expected: typecheck clean, all tests pass, build succeeds.

---

### Task 12: Packaging and operational docs

**Files:**
- Modify: `packaging/rpm/binaries.txt`
- Modify: `packaging/rpm/sauron.spec`
- Create: `packaging/systemd/sauron-storesync.service`
- Modify: `README.md` (environment variable table)

- [ ] **Step 1: Add the binary to the manifest**

In `packaging/rpm/binaries.txt`, under `# --- sauron-server ---`, after `sauron-tier`:

```
sauron-storesync
```

- [ ] **Step 2: Add the matching `%files` entry**

In `packaging/rpm/sauron.spec`, add `%{_bindir}/sauron-storesync` to the `sauron-server` subpackage's `%files` section, and the unit to its `%files`/`%post` handling exactly as `sauron-monitor` is handled.

This step is not optional bookkeeping: rpmbuild **fails the build** on an installed-but-unpackaged file. A `binaries.txt` line without the matching `%files` entry is the exact failure that broke the earlier `sauron-alerts` release.

- [ ] **Step 3: Write the systemd unit**

`packaging/systemd/sauron-storesync.service`, copied from `sauron-monitor.service` with the binary name, description, and `EnvironmentFile` adjusted. It needs `NOTIFY_SECRET_KEY` and `DATABASE_URL`; it does **not** need Redis.

- [ ] **Step 4: Document the environment variables**

Add to the README's variable table:

| Variable | Default | Meaning |
|---|---|---|
| `STORE_SYNC_INTERVAL_SECS` | `21600` | How often each store connection re-syncs |
| `STORE_SYNC_MAX_CONCURRENCY` | `8` | Concurrent store fetches |
| `STORE_BACKFILL_DAYS` | `90` | Window pulled on a connection's first sync |

Also add a line to the upgrade notes: migration 49 must be applied manually after an RPM upgrade, because upgrades do not re-run `sauron-migrate`.

- [ ] **Step 5: Verify the package builds**

```bash
cd packaging/rpm && ./build-rpm.sh --prebuilt
```

Expected: four RPMs build; `sauron-storesync` appears in the `sauron-server` package. Confirm with:

```bash
rpm -qlp packaging/rpm/out/sauron-server-*.rpm | grep storesync
```

---

## Self-Review

**Spec coverage.** Every spec section maps to a task: schema → 1; repo → 2; Google connector → 3–4; Apple connector → 5; daemon, config, key self-test → 6; connection API and write-only secrets → 7; `store_environment_id` and the chart feed → 8; dashboard client → 9; settings UI → 10; Overview section and chart → 11; RPM, systemd, docs → 12. The spec's five decisions are each carried by a named test.

**Placeholder scan.** One deliberate known-unknown remains, flagged in the spec and repeated at Task 5: Apple's real column names. It is scoped to three named constants and one fixture file, with explicit instructions on what to change if the real report differs — not a "TBD".

**Type consistency.** `StoreKind::as_str()` returns the DB CHECK values `google_play`/`app_store`, and those same strings are the TypeScript `StoreKind` union, the URL path segment, and the `StoreDay` object keys. `DailyMetric { day, installs, uninstalls }` is the connector return type throughout; the repo takes the flattened `(NaiveDate, i64, i64)` tuple; the wire type is `StoreCounts { installs, uninstalls }` nested under a store key. `AppleProgress::{Pending, Ready}` is the only place "pending" is a Rust value; everywhere else it is the `state` string. `secret` is `Option<Option<String>>` in Rust and `string | null | undefined` in TypeScript — the same three states.
