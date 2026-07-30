# Plan: Background CSV export of person-level analytics

**Date:** 2026-07-29
**Status:** ready to execute
**Scope:** backend (new bin + API routes + migration + RBAC), dashboard (new page + polling), packaging (RPM/systemd)

---

## 1. Goal

An operator viewing an app's analytics can request a CSV of **persons and their
events**, walk away, and be told in the dashboard when the file is ready to
download. Large exports must never block an HTTP request or degrade the API.

## 2. Locked decisions

These were settled during brainstorming. Do **not** relitigate them; if
execution reveals one is unworkable, stop and report rather than substituting a
different design.

| # | Decision | Rationale |
|---|---|---|
| D1 | Dataset is `persons_events`: one CSV row per event, denormalised with its person's columns. Only this one dataset in v1. | This is the shape that actually gets large, which is the whole reason for a background job. The `dataset` column exists so more can be added without reshaping anything. |
| D2 | Artifact lives on **local disk** under `SAURON_EXPORT_PATH`, gzip-compressed. Not Postgres BYTEA, not S3. | Streams straight to disk, no TOAST churn on the DB being queried, and precedent exists — `admin_storage.rs` already reads `tier_cold_path` off local disk from the API side. Accepted cost: worker and API must be co-located. |
| D3 | Completion is surfaced by **polling**, not SSE and not a notification feed. The Exports page and a topbar badge poll only while the current user has a job in flight. | Meets the requirement (don't make the admin wait) at a fraction of the build. A durable notification feed is a separate feature; building it inside this one triples the work. Accepted cost: no notification if the browser tab is closed — the finished export is simply there next visit. |
| D4 | New permission **`perm::EXPORT_WRITE = "export:write"`**. Not a reuse of `EVENT_READ`. | Bulk PII extraction is granted explicitly, not inherited. |
| D5 | A finished export is **shared across the app** — anyone with `export:write` on that app can list and download it. | Avoids duplicate regeneration of expensive exports. |
| D6 | `ensure_preset_roles` **backfills** `export:write` onto existing OWNER/ADMIN role rows at API startup. | Survives the known RPM upgrade gap (nobody re-runs `sauron-migrate` on upgrade), so the feature works immediately after upgrade instead of 403ing. |
| D7 | The worker is a **new dedicated bin `sauron-export`** with a tick loop. Not a `tokio::spawn` inside `sauron-api`. | Matches the house pattern (`sauron-alerts`, `sauron-tier`, `sauron-monitor`) and keeps spiky export CPU out of the request path — the exact complaint motivating the feature. |
| D8 | `params` is a **frozen filter snapshot** captured at request time. | A CSV whose meaning depends on when you download it is a bug. |
| D9 | `expires_at` is set at **creation**, not completion. | Retention becomes one `WHERE expires_at < now()` sweep that treats `ready`, `failed`, and jobs that died mid-flight identically — no state-machine special cases. |

## 3. Reference points in the existing codebase

Read these before writing code; the plan assumes you follow their conventions.

- Worker shape: `backend/bins/sauron-alerts/src/main.rs` — `init telemetry → Config::from_env → build_pool → loop { cycle().await; sleep(tick) }`, with an in-loop retention prune tracked by a `last_prune` timestamp seeded in the past so the first tick prunes. `backend/bins/sauron-tier/src/main.rs` for `spawn_blocking` around heavy work.
- Stale-claim / dead-letter semantics to mirror: `backend/crates/sauron-redis/src/lib.rs` (`claim_stale`, `dead_letter`).
- Config: `backend/crates/sauron-core/src/config.rs` (see `tier_cold_path:200`, `alert_event_retention_days:214`).
- Analytics queries to reuse/extend: `backend/bins/sauron-api/src/routes/analytics.rs` — `persons_list` / `PersonsQuery`, `events_list` / `EventsListQuery`; env scope is read via `super::scope::read_scope_raw(app_id, raw_query)`, **not** a serde field (there is a documented codec bug with `?environment_id=`).
- RBAC: `backend/crates/sauron-auth/src/rbac.rs` — `perm::ALL` (currently `[&str; 27]`, length asserted at `:801`), `PRESETS`, `authorize_app`.
- Migrations: `backend/migrations/<YYYY-MM-DD>-<6digit>_<name>/{up,down}.sql`; highest existing is `2026-07-28-000028_issue_env_covering_index`. **Note** an unmerged sibling exists at `2026-07-29-000029_env_scope_grants` — take `000030` to avoid collision.
- Dashboard: routes in `dashboard/src/routes.ts` (`guarded(...)`), nav in `dashboard/src/lib/components/layout/Sidebar.svelte`, API modules in `dashboard/src/lib/api/*.ts`, toasts via `dashboard/src/lib/stores/toast.svelte.ts`, polling precedent in `dashboard/src/pages/Onboarding.svelte:79`.
- Packaging: `packaging/rpm/binaries.txt` (single source of truth), `packaging/rpm/sauron.spec` (`%files` + `%systemd_*` lines), `packaging/rpm/systemd/`.

Use the house UI components (not raw `button`/`table`) per existing dashboard convention.

---

## 4. Tasks

Execute in order. Each task ends green (compiles, tests pass) before the next.

### Task 1 — Migration `2026-07-29-000030_export_jobs`

```sql
CREATE TABLE export_jobs (
  id            UUID PRIMARY KEY,
  app_id        UUID NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
  requested_by  UUID NOT NULL REFERENCES users(id),
  dataset       TEXT NOT NULL,
  params        JSONB NOT NULL,
  status        TEXT NOT NULL,
  row_count     BIGINT,
  byte_size     BIGINT,
  file_path     TEXT,
  error         TEXT,
  attempts      INT NOT NULL DEFAULT 0,
  created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
  started_at    TIMESTAMPTZ,
  finished_at   TIMESTAMPTZ,
  expires_at    TIMESTAMPTZ NOT NULL
);
CREATE INDEX export_jobs_app_created ON export_jobs (app_id, created_at DESC);
CREATE INDEX export_jobs_claim ON export_jobs (status, created_at) WHERE status = 'queued';
```

`status` ∈ `queued | running | ready | failed | expired`. Transitions:
`queued → running → ready|failed`; any state → `expired` by the sweeper.
`down.sql` drops the table. Add the Diesel schema entry and models
(`ExportJob`, `NewExportJob`) in `backend/crates/sauron-db/`.

**Note:** if `export_jobs` is ever tiered, it must be excluded — it is
operational state, not telemetry. Do not add it to `TIERED_TABLES`.

### Task 2 — RBAC: `export:write`

- Add `pub const EXPORT_WRITE: &str = "export:write";` to the `perm` module.
- Extend `perm::ALL` to `[&str; 28]` and update the hardcoded length assertion at `rbac.rs:801`.
- Add to `OWNER` (automatic, it uses `&perm::ALL`) and `ADMIN`. **Do not** add to `DEVELOPER` or `VIEWER`.
- In `ensure_preset_roles`, backfill the permission onto existing OWNER/ADMIN `role_grants`/role-permission rows (D6). This must be idempotent — it runs on every API boot.
- Mirror in `dashboard/src/lib/models/permissions.ts` (and whatever its test parses off disk).

Tests: existing `perm::ALL` duplicate/length tests must pass; add a test that
an ADMIN role has `export:write` and a VIEWER does not, plus one asserting the
backfill is a no-op on a role that already has it.

### Task 3 — Repo layer (`backend/crates/sauron-db/src/repo.rs`)

- `create_export_job(conn, NewExportJob) -> ExportJob`
- `list_export_jobs(conn, app_id, limit, offset) -> Vec<ExportJob>` (newest first)
- `get_export_job(conn, id) -> Option<ExportJob>`
- `claim_export_job(conn) -> Option<ExportJob>` — `UPDATE ... SET status='running', started_at=now(), attempts=attempts+1 WHERE id = (SELECT id FROM export_jobs WHERE status='queued' ORDER BY created_at FOR UPDATE SKIP LOCKED LIMIT 1) RETURNING *`
- `finish_export_job(conn, id, file_path, row_count, byte_size)` → `ready`
- `fail_export_job(conn, id, error)` → `failed`
- `requeue_stale_export_jobs(conn, stale_after) -> Vec<Uuid>` — jobs `running` with `started_at < now() - stale_after`: back to `queued` if `attempts < 3`, else `failed` with an error explaining the retry exhaustion.
- `expire_export_jobs(conn) -> Vec<(Uuid, Option<String>)>` — mark `expired` where `expires_at < now()` and status ≠ `expired`, returning ids + file paths so the caller can unlink.
- Streaming read for the worker: a function that yields person+event rows for the frozen params in stable `(distinct_id, event timestamp, event id)` order, **keyset-paginated in batches** (e.g. 10k). Do not load the whole result set into memory, and do not use OFFSET pagination — it degrades quadratically and can duplicate or skip rows under concurrent ingest.

Tests go in `backend/crates/sauron-db/tests/` against real Postgres, following
the existing harness in `tests/common/mod.rs`. Cover: claim is exclusive under
two concurrent claimers; stale requeue respects the attempts ceiling; expiry
returns the file path.

### Task 4 — Worker bin `backend/bins/sauron-export`

`Cargo.toml` + `src/main.rs`, modelled on `sauron-alerts/src/main.rs`.

Loop per tick (`SAURON_EXPORT_TICK_SECS`, default 5, clamped like the other bins):

1. `requeue_stale_export_jobs`.
2. Hourly (tracked by a `last_sweep` seeded in the past so the first tick sweeps): `expire_export_jobs`, then unlink each returned file. Also unlink orphan files present on disk with no `ready` row — a crash between write and DB update leaves them.
3. Claim and run up to N jobs concurrently, bounded by `Arc<Semaphore>` (default 2 — exports are IO-heavy and this is a single box). Per-job failures are logged and recorded on the row, never fatal to the loop.

Per job:

- Resolve the output path as `{SAURON_EXPORT_PATH}/{app_id}/{job_id}.csv.gz`; create dirs with restrictive permissions (0700). Path components are UUIDs from the DB, so there is no traversal vector — keep it that way, never interpolate user-supplied strings into the path.
- Write to `{job_id}.csv.gz.partial`, then `rename` on success. An atomic rename is what makes "`ready` implies a complete file" true; without it a crash mid-write leaves a truncated CSV that looks valid.
- Stream: write header, then batch-fetch and write rows through a gzip encoder into a `BufWriter`. Count rows and final bytes.
- Enforce `SAURON_EXPORT_MAX_ROWS` (default 5,000,000). On exceeding it, fail the job with a clear message naming the limit and suggesting a narrower date range. Do **not** silently truncate — a truncated export that looks complete is worse than an error.
- `finish_export_job` on success; `fail_export_job` with the error string on failure, and unlink the `.partial`.

CSV shape (D1) — write with a real CSV writer (add the `csv` crate), never
hand-rolled `format!`, so quoting and embedded newlines/commas are correct:

```
distinct_id,person_first_seen,person_last_seen,event_id,event_name,event_timestamp,session_id,environment,properties_json
```

`properties_json` is the event's property map as compact JSON in one quoted
column. Timestamps are RFC3339 UTC. Excel's habit of interpreting leading `=`,
`+`, `-`, `@` as a formula is a real risk for attacker-controlled event names —
prefix any cell starting with one of those with a single quote, or document
explicitly that the file is for machine consumption. Pick one and say which in
a comment.

### Task 5 — API routes `backend/bins/sauron-api/src/routes/exports.rs`

All app-scoped, all `authorize_app(..., perm::EXPORT_WRITE)`.

- `POST /v1/apps/{app_id}/exports` — body carries the filter set; env scope read via `scope::read_scope_raw(app_id, raw_query)` exactly like the analytics routes. Freeze the resolved filters into `params` (D8), set `expires_at = now() + retention` (D9), insert as `queued`, return the job. Rate-limit per user (a handful of queued jobs per app) so one operator cannot fill the disk; return 429 past the cap.
- `GET /v1/apps/{app_id}/exports` — list for the app (D5), newest first, paginated.
- `GET /v1/apps/{app_id}/exports/{id}` — single job, for polling.
- `GET /v1/apps/{app_id}/exports/{id}/download` — re-authorize (never trust that job creation was legitimate), verify the job belongs to `app_id` in the path, require `status = 'ready'`, stream the file with `Content-Type: text/csv`, `Content-Encoding: gzip`, and a `Content-Disposition: attachment; filename="..."` whose filename is derived from ids and dates only — no user input.
- `DELETE /v1/apps/{app_id}/exports/{id}` — mark expired and unlink.

Register in `backend/bins/sauron-api/src/main.rs` alongside the analytics
routes. Add HTTP tests in `backend/bins/sauron-api/tests/` covering: a VIEWER
gets 403; a job from app A is not downloadable via app B's path; downloading a
`queued` job is a 409/404 rather than a 500.

### Task 6 — Config

In `backend/crates/sauron-core/src/config.rs`, following the `tier_cold_path`
and `alert_event_retention_days` precedents:

| Env var | Default |
|---|---|
| `SAURON_EXPORT_PATH` | `/var/lib/sauron/exports` |
| `SAURON_EXPORT_TICK_SECS` | `5` |
| `SAURON_EXPORT_RETENTION_DAYS` | `7` |
| `SAURON_EXPORT_MAX_ROWS` | `5000000` |
| `SAURON_EXPORT_CONCURRENCY` | `2` |

### Task 7 — Dashboard

- `dashboard/src/lib/api/exports.ts` — typed client for the five endpoints.

  **Gotcha:** `dashboard/src/lib/api/scope.ts` auto-attaches `environment_id`
  to every `/v1/apps/{app_id}/...` request. That is correct for `POST /exports`
  (the env scope belongs in the frozen params) but the list/get/download
  endpoints must either accept and ignore it or be added to the exclusion list.
  Decide by checking whether the handler calls `reject_environment_id` — do not
  guess from the URL shape. This exact mismatch caused blanket 400s on
  Monitors, Alerting and Storage before.

- `dashboard/src/pages/Exports.svelte` — a DataTable of jobs (status pill,
  requester, row count, size, created/expires, download or error). "New export"
  action prefilled from the current filter context. Beware the DataTable
  cell-color specificity trap noted in the dashboard conventions.
- `dashboard/src/lib/stores/exports.svelte.ts` — polling store (D3): starts a
  `setInterval` (3s, per the `Onboarding.svelte` precedent) only while at least
  one job is `queued`/`running`; stops otherwise. On a transition to `ready`,
  push a success toast with a download link; on `failed`, an error toast. Must
  stop the interval on destroy and on logout — a leaked interval hammering the
  API after sign-out is the failure mode here.
- Add the route to `dashboard/src/routes.ts` and a nav entry under **Explore**
  in `Sidebar.svelte`, hidden unless the user holds `export:write`.
- An "Export CSV" button on `UsersExplorer.svelte` that creates a job from the
  page's live filters and routes to Exports.
- Unit tests for the store's start/stop transitions with fake timers.

### Task 8 — Packaging

- Add `sauron-export` to `packaging/rpm/binaries.txt` under `sauron-server`.
- `packaging/rpm/systemd/sauron-export.service`, modelled on
  `sauron-alerts.service`.
- In `sauron.spec`: a new `SourceNN`, the `install -Dm0644` line, the
  `%{_bindir}/sauron-export` and `%{_unitdir}/sauron-export.service` `%files`
  entries, and the service name in the `%systemd_post` / `%systemd_preun` /
  `%systemd_postun_with_restart` lists. Missing a `%files` entry fails the
  build; missing a `%systemd_*` entry does not, and that omission is what broke
  the `sauron-alerts` release — check all three.
- `packaging/rpm/tmpfiles` (or the spec) must create
  `/var/lib/sauron/exports` owned by the sauron user, mode 0700.
- Update `packaging/rpm/INSTALL.md` / `SETUP.md` with the new unit and the new
  env vars.

### Task 9 — Verification

Do not report done on a green `cargo test` alone. Drive it end to end:

1. Run the migration against a real Postgres.
2. Seed enough events that the export takes visibly more than one tick.
3. Start `sauron-api` and `sauron-export`; create a job through the HTTP API.
4. Observe `queued → running → ready`, then download and validate the gzip
   decompresses and the row count matches `row_count`.
5. Force a failure (unwritable `SAURON_EXPORT_PATH`) and confirm the job lands
   `failed` with a useful error and no `.partial` left behind.
6. Kill the worker mid-job and confirm the next tick requeues it, and that
   `attempts` eventually exhausts to `failed`.
7. Set retention to 0 days and confirm the sweep expires the row and unlinks
   the file.
8. In the browser: create an export, navigate away, confirm the toast fires on
   completion and the polling interval stops afterwards.

Report what you actually observed, including anything that did not work.

---

## 5. Out of scope

Deliberately excluded — do not build these:

- A durable in-app notification feed, bell icon, or unread count (D3).
- SSE or websockets.
- Object storage / S3 / presigned URLs (D2).
- Datasets other than `persons_events` (D1); the `dataset` column is the seam
  for adding them later.
- Scheduled/recurring exports, email delivery, formats other than CSV.
