# Admin Storage & Records View — Design

**Goal:** Give a deployment operator ("admin") visibility into storage and record counts across the whole system: total Postgres DB size, per-app record counts (hot in Postgres + cold in Parquet), and the cold Parquet files (name + size). Surfaced as an admin-gated API plus an admin-only dashboard page.

**Context:** This builds on the hot/cold tiering feature. The `api` service already mounts the cold `colddata` volume read-only at `/cold` and already embeds DuckDB — so **no new infrastructure** (no Dockerfile/compose changes). Tiered tables: `error_events`, `analytics_events`, `transactions` (all RANGE-partitioned on `occurred_at`, cold Parquet hive-partitioned by `app_id/year/month`).

## Decisions (from brainstorming)

- **Scope:** per-app breakdown (not just deployment totals), covering **all apps** in the deployment.
- **Access:** a new **global-admin (superuser)** concept — there is no such concept today (RBAC "Admin" is a per-org preset role). Gated deployment-wide, not per-org.
- **Bootstrap:** the **first registered user** becomes the global admin automatically.
- **Metrics per app:** hot record counts (per table), cold record counts (per table), cold Parquet files (name + size), and an **estimated** hot byte size (per app).
- **Delivery:** backend API **and** a dashboard page.

## A. Global-admin auth (new)

### Migration `2026-07-15-000014_users_is_admin`
- `up.sql`:
  - `ALTER TABLE users ADD COLUMN is_admin BOOLEAN NOT NULL DEFAULT false;`
  - Retroactive bootstrap so existing deployments have an admin: `UPDATE users SET is_admin = true WHERE id = (SELECT id FROM users ORDER BY created_at ASC LIMIT 1);` (no-op on an empty table).
- `down.sql`: `ALTER TABLE users DROP COLUMN is_admin;`
- `schema.rs`: add `is_admin -> Bool` to the `users` table! block. Add `is_admin: bool` to the `User` model (serializes in `/v1/me`; keep `password_hash` `#[serde(skip)]` as-is).

### First-user-becomes-admin
- `repo::create_user` (currently `(conn, email, password_hash, name) -> QueryResult<User>`): before insert, compute `is_first = (users count == 0)` and insert with `is_admin = is_first`. Add `is_admin: bool` to `NewUser`. Everyone after the first registers with `is_admin = false`.
- The retroactive migration + this insert logic together cover both fresh and existing deployments (fresh: migration no-ops, first registration flags; existing: migration flags the earliest user).

### Gate
- New `sauron_auth::require_admin(conn: &mut AsyncPgConnection, user_id: Uuid) -> Result<(), AuthError>` mirroring `authorize_app`: re-queries `users.is_admin` fresh each request; returns an `AuthError` when not admin, which the handler `?`-converts to an HTTP 403 via the existing `From<AuthError> for ApiError` mapping (match the not-authorized variant that `authorize_app` failures already use). No JWT/claims change (always-fresh check; only one admin endpoint so the extra query is negligible).
- New repo helper `is_user_admin(conn, user_id) -> QueryResult<bool>` (or fold the query into `require_admin`).

### `/v1/me`
- The `User` returned by `me` now includes `is_admin` (via the model change) — the dashboard reads it to show/hide the admin UI. No handler change beyond the model gaining the field.

## B. Backend — `GET /v1/admin/storage`

Admin-gated (`require_admin`). Handler in a new `bins/sauron-api/src/routes/admin.rs`; the aggregation logic in a new `bins/sauron-api/src/admin_storage.rs`.

### Response shape (JSON)
```jsonc
{
  "database": {
    "total_bytes": 123456789,              // pg_database_size(current_database())
    "tables": [                            // the three tiered tables
      { "name": "error_events",     "total_bytes": 111, "hot_rows": 100 },
      { "name": "analytics_events", "total_bytes": 222, "hot_rows": 200 },
      { "name": "transactions",     "total_bytes": 333, "hot_rows": 300 }
    ]
  },
  "apps": [
    {
      "app_id": "…", "app_name": "…", "org_name": "…",
      "tables": [
        { "name": "error_events", "hot_rows": 10, "cold_rows": 90,
          "cold_bytes": 4096, "estimated_hot_bytes": 5120 }
        // … analytics_events, transactions
      ],
      "hot_rows_total": …, "cold_rows_total": …, "cold_bytes_total": …,
      "estimated_hot_bytes_total": …,
      "cold_files": [
        { "path": "error_events/app_id=…/year=2026/month=5/data_0.parquet", "bytes": 4096 }
      ]
    }
  ]
}
```
Notes: `estimated_hot_bytes` is explicitly approximate. Apps with zero storage are still listed (operator sees the full inventory).

### Data sources (gathered concurrently, then assembled in Rust by `app_id`)
Like the tier_read router, run the async-PG work, the blocking DuckDB work, and the blocking filesystem walk concurrently (`tokio::join!` + `spawn_blocking` for the blocking parts).
- **DB total:** `SELECT pg_database_size(current_database())`.
- **Per-table size:** `SELECT pg_total_relation_size('<table>')` (partitioned parent → includes all partitions, indexes, toast).
- **Hot rows per (table, app):** `SELECT app_id, count(*) FROM <table> GROUP BY app_id` (one query per tiered table; exact).
- **Estimated hot bytes per app:** avg row width per table = `SELECT COALESCE(sum(avg_width),0) FROM pg_stats WHERE tablename='<table>'`; `estimated_hot_bytes = avg_width * hot_rows` for that (table, app). Approximate.
- **Cold rows per (table, app):** new `DuckEngine::counts_by_app(glob) -> Vec<(Uuid, i64)>` running `SELECT app_id, count(*) FROM read_parquet('<cold>/<table>/**/*.parquet', hive_partitioning=true, union_by_name=true) GROUP BY app_id` (guarded with the existing zero-match `any_files_match` → empty). Counts come from Parquet footers (cheap).
- **Cold files + bytes per app:** a `/cold` filesystem walk (`std::fs`, in `spawn_blocking`) collecting `*.parquet` under each `<table>/` dir, parsing `app_id=<uuid>` out of the hive path, `stat` for byte size. New pure helper `parse_cold_path(rel_path) -> Option<{ table, app_id }>` + a byte-size formatter — both unit-tested.
- **App/org names:** `SELECT a.id, a.name AS app_name, o.name AS org_name FROM apps a JOIN projects p ON a.project_id = p.id JOIN organizations o ON p.org_id = o.id`.

## C. Dashboard — admin-only Storage page

- New Svelte page `dashboard/src/pages/Storage.svelte`, calling `GET /v1/admin/storage` via the existing api client. Uses **house UI components** (DataTable, Icon registry, etc.) per the dashboard conventions — no raw `<table>`/`<button>`.
- Layout: a deployment-totals header (formatted DB size + per-table size/hot-rows), then a per-app **DataTable** (columns: app, org, hot rows, cold rows, cold bytes, est. hot bytes), with an expandable per-app **Parquet file list** (path, size).
- Gating: the current user's `is_admin` (from `/v1/me`) controls whether the "Storage" nav link + route render. Non-admins never see it; the endpoint also 403s them server-side.

## D. Verification

- **Unit tests (pure logic):** `parse_cold_path` (hive-path → table + app_id; rejects non-conforming paths) and the human byte-size formatter.
- **e2e (docker-compose, the project's DB-verification pattern):**
  1. Register user A on a fresh DB → `is_admin=true`; register user B → `is_admin=false`.
  2. `GET /v1/admin/storage` as B → 403; as A → 200.
  3. Seed apps + rows, tier some to cold (reusing the tiering e2e helpers), then assert the response: `database.total_bytes > 0`; per-app hot_rows/cold_rows match the seeded/tiered counts; `cold_files` lists the app's Parquet with non-zero bytes; the earliest-user retroactive-admin migration path.

## Out of scope / follow-ups
- Caching the exact `GROUP BY app_id` counts (they scan the firehose tables; fine for an infrequent admin call).
- Pagination/caps on very large per-app Parquet file lists.
- Exact per-app Postgres byte size (not derivable; estimate only).
- Per-org rollups, retention controls, or write actions — read-only view only.
- Managing/revoking admin beyond first-user + earliest-user bootstrap (no admin-management UI).
