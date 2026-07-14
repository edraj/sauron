# Design: Uptime monitoring

**Date:** 2026-07-14
**Status:** Approved (brainstorming) — pending spec review
**Area:** `backend/` (new migration, new `sauron-monitor` bin + crate, `sauron-db`, `sauron-auth` rbac, `sauron-api` routes) + `dashboard/` + `docker-compose.yml` / `.env`

## Goal

Add **uptime monitoring**: let users define checks against their own services (HTTP(S) or TCP), have Sauron actively probe them on a schedule, and surface **uptime %, latency, and incidents** in the dashboard. On every up→down / down→up transition, record an incident and POST a **webhook**.

This is the platform's first **active / outbound** capability. Everything today is passive (SDKs push → ingest → Redis stream → workers write rows). Uptime monitoring instead has the backend reach out and poll external endpoints. There are **no SDK changes and no ingest wire-contract changes** — it is entirely server-side + dashboard.

## Decisions (settled in brainstorming)

- **Positioning:** customer-facing product feature first; the same prober can later watch Sauron's own endpoints as just another monitor.
- **Anchoring:** monitors are **project-scoped** (belong to a `project`, not an `app`/DSN), because a monitored URL does not map to one SDK app. New project-level routes + RBAC.
- **Check types (MVP):** **HTTP(S)** and **TCP**. (No ICMP ping — raw sockets / `CAP_NET_RAW` is deployment friction for a self-hosted Docker stack.)
- **A monitor is "define URL + verb + interval"** (HTTP) or "host:port + interval" (TCP). URL, method, and interval are first-class config.
- **On state change:** record an **incident** (open on down, resolve on recovery), surface everything in the dashboard, **and** POST a JSON **webhook** to a user-configured URL. Webhook is the one external channel that needs no new infra (no SMTP/Slack).
- **Surface:** authenticated dashboard only — **no public status page** in the MVP.
- **Prober runtime:** a **new dedicated binary `sauron-monitor`** (matches the one-bin-per-concern structure: `sauron-api` / `sauron-ingest` / `sauron-migrate`), its own compose service, scaling-safe via Postgres `FOR UPDATE SKIP LOCKED`.
- **Opinionated defaults:** anti-flap `failure_threshold = 2` (consecutive fails → down), `recovery_threshold = 1`; raw-check retention **30 days** (configurable), pruned by the prober; **SSRF: block loopback/private/link-local targets by default**, overridable by env.

## Data model

Migration `backend/migrations/2026-07-14-000009_monitors/{up,down}.sql` (next in sequence after `000008`). Three project-scoped tables.

### `monitors` — config + live state

```sql
-- up.sql (monitors)
CREATE TABLE monitors (
    id                      UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id              UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    name                    TEXT NOT NULL,
    kind                    TEXT NOT NULL CHECK (kind IN ('http', 'tcp')),
    target                  TEXT NOT NULL,               -- URL (http) or host:port (tcp)
    method                  TEXT NOT NULL DEFAULT 'GET', -- http only
    config                  JSONB NOT NULL DEFAULT '{}', -- {headers, body, expected_status, body_assertion, follow_redirects}
    interval_seconds        INT  NOT NULL DEFAULT 60,
    timeout_ms              INT  NOT NULL DEFAULT 10000,
    failure_threshold       INT  NOT NULL DEFAULT 2,
    recovery_threshold      INT  NOT NULL DEFAULT 1,
    webhook_url             TEXT,
    enabled                 BOOL NOT NULL DEFAULT TRUE,
    status                  TEXT NOT NULL DEFAULT 'unknown'
                              CHECK (status IN ('unknown', 'up', 'down', 'paused')),
    consecutive_failures    INT  NOT NULL DEFAULT 0,
    consecutive_successes   INT  NOT NULL DEFAULT 0,
    last_checked_at         TIMESTAMPTZ,
    next_check_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_status_changed_at  TIMESTAMPTZ,
    created_by              UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX monitors_due_idx ON monitors (next_check_at) WHERE enabled;
CREATE INDEX monitors_project_idx ON monitors (project_id);
```

`config` (jsonb) holds the check-specific params so the schema stays stable as check types grow:
- **http:** `{ "headers": {..}, "body": "..", "expected_status": "200-399", "body_assertion": "OK", "follow_redirects": true }`
- **tcp:** `{}` (host:port live in `target`).

`expected_status` is a spec string parsed by the prober: a range (`"200-399"`) or CSV (`"200,204"`); default `"200-399"`.

### `monitor_checks` — append-only raw results

```sql
CREATE TABLE monitor_checks (
    id                UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    monitor_id        UUID NOT NULL REFERENCES monitors(id) ON DELETE CASCADE,
    checked_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    up                BOOL NOT NULL,
    status_code       INT,          -- http only, nullable
    response_time_ms  INT,          -- nullable when the probe never connected
    error             TEXT          -- failure reason, nullable when up
);
CREATE INDEX monitor_checks_monitor_time_idx ON monitor_checks (monitor_id, checked_at DESC);
```

Uptime % and latency series are computed **on read** from this table (no rollup tables in the MVP). The prober prunes rows older than `MONITOR_CHECK_RETENTION_DAYS` (default 30).

### `monitor_incidents`

```sql
CREATE TABLE monitor_incidents (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    monitor_id   UUID NOT NULL REFERENCES monitors(id) ON DELETE CASCADE,
    started_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    resolved_at  TIMESTAMPTZ,             -- NULL = still open
    cause        TEXT NOT NULL,           -- e.g. "HTTP 503", "connection timeout", "TCP connect refused"
    last_error   TEXT
);
-- At most one open incident per monitor:
CREATE UNIQUE INDEX monitor_incidents_one_open_idx ON monitor_incidents (monitor_id) WHERE resolved_at IS NULL;
CREATE INDEX monitor_incidents_monitor_idx ON monitor_incidents (monitor_id, started_at DESC);
```

Incident **duration** is computed on read (`coalesce(resolved_at, now()) - started_at`).

`down.sql` drops the three tables (reverse order) and the added RBAC perms are reverted by re-seeding presets (see below).

### Diesel schema (`sauron-db/src/schema.rs`)

Add `monitors`, `monitor_checks`, `monitor_incidents` `table!` blocks (regenerated / hand-added to match the migration). Add `joinable!` / `allow_tables_to_appear_in_same_query!` entries as the codebase already does for related tables.

### RBAC (`sauron-auth/src/rbac.rs`)

- Add `pub const MONITOR_READ: &str = "monitor:read";` and `pub const MONITOR_WRITE: &str = "monitor:write";`.
- Grow `ALL` from `[&str; 17]` to `[&str; 19]`.
- Add to preset roles: **Viewer** → `monitor:read`; **Developer / Admin / Owner** → `monitor:read` + `monitor:write` (mirrors how `funnel:write` was added — Developer and up, not Viewer). Kept in sync at API startup by `ensure_preset_roles`.
- Extend the rbac unit tests to cover the new perms in the preset expectations.

## The prober (`sauron-monitor`)

New workspace member. Split so the risky I/O is behind a trait and the decision logic is pure and unit-testable:

- **`backend/crates/sauron-monitor/`** (library): `Prober` trait, HTTP + TCP implementations, result evaluation, the state machine, `next_check_at` math, SSRF address classifier, webhook payload builder.
- **`backend/bins/sauron-monitor/`** (binary): `main.rs` wiring (telemetry, `Config`, pool) + the scheduler loop + webhook dispatch + retention prune.

### Scheduler loop

A loop ticking every `MONITOR_TICK_MS` (default 1000). Each tick **atomically claims and reschedules** due monitors in a single statement, so multiple prober replicas never double-probe:

```sql
UPDATE monitors
   SET next_check_at = now() + make_interval(secs => interval_seconds),
       last_checked_at = now()
 WHERE id IN (
     SELECT id FROM monitors
      WHERE enabled AND status <> 'paused' AND next_check_at <= now()
      ORDER BY next_check_at
      FOR UPDATE SKIP LOCKED
      LIMIT :MONITOR_BATCH
 )
RETURNING *;
```

The claimed batch is then probed **concurrently** outside the transaction, bounded by a `tokio::sync::Semaphore` of `MONITOR_MAX_CONCURRENCY` (default 50). A separate low-frequency task prunes `monitor_checks` past the retention window.

### Probe execution (behind `Prober`)

- **http:** `reqwest` client with the monitor's `timeout_ms`, `method`, `headers`, `body`, `follow_redirects`. Measure elapsed ms. Result is `up` iff the status code ∈ the parsed `expected_status` set **and** (no `body_assertion`, or the response body contains it as a substring). On failure, `error` = a concise reason (`"HTTP 503"`, `"connection timeout"`, `"assertion 'OK' not found"`).
- **tcp:** `tokio::net::TcpStream::connect` with a timeout wrapper. `up` iff connect succeeds within `timeout_ms`; `error` = the connect error (`"TCP connect refused"`, `"connection timeout"`).
- Every probe yields `ProbeResult { up: bool, status_code: Option<i32>, response_time_ms: Option<i32>, error: Option<String> }`. **A failed probe is data, never a process error.**

### State machine (pure)

`fn apply(state: &MonitorState, result: &ProbeResult) -> Transition` where `MonitorState` carries `status`, `consecutive_failures`, `consecutive_successes`, thresholds:

- Bump the matching counter (reset the other to 0).
- If `status ∈ {up, unknown}` and `consecutive_failures >= failure_threshold` → transition **to `down`**: open an incident (`cause` from the result), set `last_status_changed_at`, emit a `Down` transition event.
- If `status ∈ {down, unknown}` and `consecutive_successes >= recovery_threshold` → transition **to `up`**: resolve the open incident, emit an `Up` transition event.
- Otherwise no transition (status unchanged).
- Every result also inserts one `monitor_checks` row and persists the updated counters/status.

Persisting the result + counters + incident open/resolve for a single monitor happens in one DB transaction.

### Webhook

On a transition, POST JSON to `monitor.webhook_url` (if set):

```json
{
  "monitor_id": "…", "name": "…", "project_id": "…",
  "status": "down", "previous_status": "up",
  "at": "2026-07-14T12:00:00Z",
  "incident_id": "…", "cause": "HTTP 503", "target": "https://…"
}
```

Best-effort: short timeout, a couple of retries with backoff, failures logged. **No delivery guarantee in the MVP** (no durable outbox / DLQ for webhooks).

## API (`sauron-api`, new `routes/monitors.rs`)

Follows the projects pattern: list/create nested under the project; item operations standalone, resolving the monitor's `project_id` and calling `authorize_project(...)` with the right permission.

| Method | Path | Permission |
|---|---|---|
| GET | `/v1/projects/{project_id}/monitors` | `monitor:read` |
| POST | `/v1/projects/{project_id}/monitors` | `monitor:write` |
| GET | `/v1/monitors/{monitor_id}` | `monitor:read` |
| PATCH | `/v1/monitors/{monitor_id}` | `monitor:write` |
| DELETE | `/v1/monitors/{monitor_id}` | `monitor:write` |
| GET | `/v1/monitors/{monitor_id}/checks?range=` | `monitor:read` |
| GET | `/v1/monitors/{monitor_id}/incidents` | `monitor:read` |

- **List** returns each monitor's current `status`, `uptime_24h` (%), last `response_time_ms`, `last_checked_at`.
- **Detail** adds uptime for 24h/7d/30d, recent checks, and open/recent incidents.
- **Create/PATCH** validate: `kind ∈ {http,tcp}`, `interval_seconds >= MONITOR_MIN_INTERVAL_SECS` (default 30), valid `target` for the kind, parseable `expected_status`, `timeout_ms` sane bounds. **Pause** = `PATCH { enabled: false }` (handler sets `status = 'paused'`); un-pause sets `status = 'unknown'` and `next_check_at = now()`.
- New monitors are inserted with `next_check_at = now()` so they get probed on the next tick.
- `authorize_project` + `require_permission` mirror the existing enforcement helpers; wire routes in `main.rs` beside the projects routes.

## Dashboard

New **"Uptime"** sidebar group (project-scoped; visibility gated by `sessionStore.can('monitor:read', { project: currentProjectId })`). Uses `sessionStore.currentProjectId` / `currentProject` for context. Routes: `/monitors` and `/monitors/:id`.

- **`pages/Monitors.svelte`** (list) — `DataTable`: name, kind, target, status pill, uptime% (range toggle), avg latency, last checked. A **"New monitor"** button (shown when `can('monitor:write', { project })`) opens a create form whose fields swap between HTTP (url, method, headers, body, expected status, assertion, timeout) and TCP (host, port, timeout), plus interval and webhook URL.
- **`pages/MonitorDetail.svelte`** — status + uptime `StatTile`s, a latency `TimeSeriesChart`, a recent-checks table, an incident-history list, and edit / pause / delete / webhook controls.
- New **`lib/api/monitors.ts`** (list/create/get/update/delete/checks/incidents) + monitor/incident/check types in `lib/models`. Reuses the kit: `DataTable`, `StatTile`, `LatencyBadge`, `TimeSeriesChart`, `DateRange`; add a small status pill component. Register routes in `routes.ts` and the nav group in `Sidebar.svelte`.

## Config & deployment

- **`docker-compose.yml`:** new `sauron-monitor` service — builds the new bin, `depends_on: [migrate, postgres]`, needs `DATABASE_URL`. **No Redis** dependency (coordination is `SKIP LOCKED` on Postgres).
- **`.env` / `.env.example`:** `MONITOR_TICK_MS=1000`, `MONITOR_BATCH=100`, `MONITOR_MAX_CONCURRENCY=50`, `MONITOR_CHECK_RETENTION_DAYS=30`, `MONITOR_MIN_INTERVAL_SECS=30`, `MONITOR_SSRF_ALLOW_PRIVATE=false`. Extend `sauron-core::Config` to read them (with defaults).

## Error handling & security

- **Loop resilience:** claim/DB errors log + back off + retry (like `worker_loop`); each probe runs isolated so one panic/timeout never stalls the batch (`tokio::spawn` per probe, join with the semaphore).
- **Restart safety:** on restart, monitors with a past-due `next_check_at` are simply due; the claim reschedules them to `now() + interval`, so there is **no backfill storm** of missed checks.
- **⚠️ SSRF (security-critical):** the prober makes outbound requests to **user-supplied** targets, which can point at internal infrastructure (`169.254.169.254`, `localhost`, RFC-1918 ranges). The MVP resolves the target host and **rejects loopback / private / link-local addresses by default**, both at create/PATCH validation time and again at probe time (guarding against DNS rebinding). Override via `MONITOR_SSRF_ALLOW_PRIVATE=true` (needed later for self-monitoring). This is called out as a required, not optional, guard.
- **Clock:** all scheduling uses `chrono::Utc::now()` consistently.

## Testing strategy

Respecting the codebase gotcha — **no DB/handler integration-test harness exists**; only pure unit tests, with end-to-end verified via `docker compose`.

- **Unit (pure functions in `sauron-monitor` crate + rbac):**
  - `expected_status` parsing (`"200-399"`, `"200,204"`) + result evaluation.
  - body-assertion substring match.
  - state-machine transitions across thresholds (up→down, down→up, flap suppression, `unknown` bootstrap).
  - incident open/resolve decisions.
  - `next_check_at` computation.
  - SSRF address classifier (loopback / private / link-local / public).
  - webhook payload builder.
  - rbac preset expectations include the two new perms.
- **e2e (`docker compose up --build`):** bring up `sauron-monitor`; create a known-up monitor (point at the api `/health`) and a known-down one (unroutable host/port); assert status transitions, `monitor_checks` rows accrue, an incident opens then resolves, and the webhook fires (point `webhook_url` at a catch endpoint). Exercise the API CRUD + the dashboard Uptime screens against seeded data.

## Build sequence

1. Migration `2026-07-14-000009_monitors` + `schema.rs` + RBAC perms (`ALL` 17→19, presets, unit tests).
2. `sauron-monitor` **crate**: `Prober` trait, HTTP/TCP probes, evaluators, state machine, SSRF classifier, webhook payload — all unit-tested.
3. `sauron-monitor` **bin**: scheduler claim loop + concurrent probing + result/incident persistence + webhook dispatch + retention prune.
4. `sauron-db` repo queries (CRUD, list-with-uptime, checks series, incidents) + `sauron-api` `routes/monitors.rs` + `main.rs` wiring + `Config` fields.
5. Dashboard: `lib/api/monitors.ts` + models + `Monitors.svelte` list + `MonitorDetail.svelte` + status pill + `routes.ts` + `Sidebar.svelte`.
6. `docker-compose.yml` `sauron-monitor` service + `.env` / `.env.example`.
7. e2e verification via docker compose (up + down monitor, incident open→resolve, webhook fired).

## Out of scope (explicitly deferred)

Public status page · multi-region / multi-location probing · email / Slack / PagerDuty notifications · SSL-certificate-expiry checks · regex / JSONPath assertions (substring only) · maintenance windows · on-call schedules / escalation · SMS · per-check rollup tables (raw rows + on-read aggregation + retention prune instead; revisit if volume demands) · durable webhook outbox / retry queue · any SDK or ingest wire-contract change.
