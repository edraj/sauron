# Sauron 👁️

[![CI](https://github.com/splimter/sauron/actions/workflows/ci.yml/badge.svg)](https://github.com/splimter/sauron/actions/workflows/ci.yml)
[![Release](https://github.com/splimter/sauron/actions/workflows/release.yml/badge.svg)](https://github.com/splimter/sauron/actions/workflows/release.yml)

**Unified error reporting + product analytics** — Sentry-style crash/error grouping and PostHog-style product events in one platform, on one timeline. When an error fires you can see the same person's events; when you look at a person you see their errors. One SDK emits both signals.

📖 Documentation: see the [wiki](wiki/Home.md). Jump to:

- [Getting Started](wiki/Getting-Started.md) · [Architecture](wiki/Architecture.md) — how it works under the hood · [Ingest Wire Contract](wiki/Ingest-Wire-Contract.md) · [Capabilities](wiki/Capabilities.md) — the SDK feature-parity matrix (v0.3.0)
- SDKs: [Browser](wiki/Browser-SDK.md) · [Flutter](wiki/Flutter-SDK.md) · [Node](wiki/Node-SDK.md) · [Python](wiki/Python-SDK.md) · [C#](wiki/CSharp-SDK.md)
- Guides: [Framework Integrations](wiki/Framework-Integrations.md) · [Best Practices](wiki/Best-Practices.md) · [Troubleshooting](wiki/Troubleshooting.md)

This repository is a working MVP: a client SDK emits an error or event → the backend ingests, groups, and enriches it → the dashboard shows the grouped issue and the analytics. Session replay/video, ClickHouse/Kafka/object storage, SSO, and billing are intentionally out of scope for this cut (see [`plan.md`](plan.md) for the full product vision).

## Architecture

```
 @edraj/sauron-browser  ┐                        ┌─────────────────────────────┐
 sauron_flutter   ├── gzip envelope ──────▶│ sauron-ingest (axum edge)   │
                  ┘  POST /api/{env_id}/    │  DSN auth → rate-limit →     │
                     envelope               │  validate → Redis stream     │
                                            │  → [co-located workers]:     │
                                            │    enrich → fingerprint →    │
                                            │    group into issues         │
                                            └──────┬────────────┬──────────┘
                                          Postgres │            │ Redis
                                            ┌───────▼──┐   ┌─────▼────┐
                                            │ Postgres │   │  Redis   │
                                            └───────▲──┘   └─────▲────┘
                        axios + JWT                 │            │
     dashboard (Svelte SPA) ────────────────▶ sauron-api (axum, JWT)
```

- **Write path** (SDK → ingest): authenticated by the non-secret DSN public key, rate-limited per app, fire-and-forget (`202`). Workers drain a Redis stream and write durable rows.
- **Read path** (dashboard → api): JWT auth, with fine-grained RBAC enforced per request.

**Stack:** Rust + axum + diesel-async + JWT · PostgreSQL + Redis · Svelte + axios · JS/TS + Flutter SDKs · Docker Compose.

## Tenancy & access control

```
Organization
  └─ Project        (grouping / product)
       └─ App       (app_type)
            ├─ Environments   (each with its own DSN — the ingest unit; every app
            │                  starts with one, `dev`, marked default)
            └─ Issues, Events, People   (keyed by app_id)
```

One product ("Project X") can hold many heterogeneous **apps** (e.g. 3 Flutter apps + 2 webapps). Every app is created with one **environment** named `dev` (marked default), and can hold more (e.g. `dev`, `staging`, `production`) — each with its own DSN, managed under **Settings → Environments**. `app_type ∈ web · flutter · ios · android · react_native · node`.

**Fine-grained RBAC.** Atomic permissions (`issue:read`, `issue:write`, `event:read`, `app:*`, `env:*`, `project:*`, `member:*`, `role:manage`, `org:manage`) are bundled into **roles**. Four presets ship — **Owner ⊇ Admin ⊇ Developer ⊇ Viewer** — plus custom roles. A user is granted a role at **org, project, or app** scope; permissions resolve as a **union down the tree** (an org grant covers everything; a project grant covers its apps but not siblings; an app grant is narrowest). So "Admin of Project X, Viewer of Project Y" is expressible. Grants and custom roles are guarded against privilege escalation (you can't grant permissions you don't hold). The dashboard reads `GET /v1/orgs/{org}/access` and hides actions the caller can't perform. See [`docs/audit-2026-07-12-rbac.md`](docs/audit-2026-07-12-rbac.md).

## Repository layout

```
backend/          Rust Cargo workspace
  crates/
    sauron-core       envelope wire contract, fingerprint algorithm, config
    sauron-db         diesel schema/models, async pool, repositories, migrations
    sauron-redis      DSN cache, rate limiter, ingest stream, HLL counters
    sauron-auth       argon2, JWT, axum extractor + authorization helpers
    sauron-pipeline   enrich → fingerprint → issue upsert; the worker loop
    sauron-telemetry  tracing setup
  bins/
    sauron-ingest     SDK edge + co-located worker pool
    sauron-api        JWT dashboard API
    sauron-migrate    one-shot migration runner
    crebain           load/benchmark generator (isolated ephemeral stack)
dashboard/        Vite + Svelte 5 (runes) + TypeScript + axios SPA
sdks/
  js/             @edraj/sauron-browser (TypeScript, tsup)
  flutter/        sauron_flutter (Dart)
examples/
  svelte-web/     runnable demo webapp wired to @edraj/sauron-browser
  flutter-app/    runnable demo app wired to sauron_flutter
docs/             design specs + the RBAC security/performance audit
```

## Quick start (Docker Compose)

```bash
cp .env.example .env        # then set JWT_SECRET
docker compose up --build
```

- Dashboard → http://localhost:10002
- API → http://localhost:10000
- Ingest → http://localhost:10001

> Compose publishes on the 10000 range on purpose: inside their containers the services
> still bind `:8080` / `:8081` / `:80`, but remapping on the host leaves 3000/8080/8081
> free for a local dev stack (below) to run alongside. `.env.example` ships the matching
> `API_BASE_URL` / `INGEST_BASE_URL` / `CORS_ALLOWED_ORIGINS`; if you change the
> `ports:` mappings, change those too.

Register in the dashboard, create a project → an app (it starts with one environment, `dev`), copy that environment's DSN from **Settings → Environments** into an SDK, and watch the first event land. The environment an event belongs to is proven by the ingest key it arrived with, so it cannot be spoofed by a client.

> First build compiles the Rust workspace three times (one per service image); subsequent builds are cached.

## Install via RPM (Fedora / RHEL)

For a Docker-less deployment on Fedora/RHEL-family systems, Sauron ships as four
RPMs (`sauron`, `sauron-server`, `sauron-dashboard`, `sauron-cli`) driven by
systemd. Postgres and Redis/Valkey are external.

```bash
./packaging/rpm/build-rpm.sh                 # build the RPMs (needs rust, cargo, node, rpm-build)
sudo dnf install ~/rpmbuild/RPMS/$(uname -m)/sauron-*.rpm
```

Full instructions: **[packaging/rpm/INSTALL.md](packaging/rpm/INSTALL.md)** (build & install)
and **[packaging/rpm/SETUP.md](packaging/rpm/SETUP.md)** (configure DB/Redis, migrate,
enable services, dashboard).

## Local development (without full compose)

```bash
make dev-infra                                   # just Postgres + Redis
export DATABASE_URL=postgres://sauron:sauron@localhost:5432/sauron
export REDIS_URL=redis://localhost:6379
export JWT_SECRET=dev-secret-change-me
make migrate                                     # apply migrations
make api        # terminal 1   (:8080)
make ingest     # terminal 2   (:8081)
cd dashboard && npm install && npm run dev       # terminal 3 (:3000, matches API CORS)
```

## Configuration

Every backend service is configured purely from the environment — no config file, no
config crate. The parser lives in [`backend/crates/sauron-core/src/config.rs`](backend/crates/sauron-core/src/config.rs)
and is deliberately hand-rolled so the env-var → field mapping stays predictable in a
container. A variable that is **unset or blank** falls back to its default, and an
unparseable value silently falls back too, so a typo'd number is never fatal.

All binaries share one `Config` struct but read only the subset they need. **Used by**
below names the services that actually consume each variable: `api` (`sauron-api`),
`ingest`, `monitor`, `alerts`, `tier`, `inspector` (`sauron-inspector`), `migrate`,
`dashboard`.

Docker Compose reads these from a `.env` file at the repo root — copy
[`.env.example`](.env.example) and edit. RPM installs read them from
`/etc/sauron/*.env`, split per service (see [packaging/rpm/SETUP.md](packaging/rpm/SETUP.md)).

> **The Default column is the built-in fallback compiled into the binary** — what you get
> with the variable unset. Each deployment mode ships its own values on top: Compose
> remaps the browser-facing URLs to the 10000 range (see the quick start above), and the
> RPM `/etc/sauron/*.env` files pin the direct-bind ports. Where a table default and your
> deployment disagree, the deployment's env file wins.

### Core

| Variable | What it does | Default | Used by |
| --- | --- | --- | --- |
| `DATABASE_URL` | Postgres connection string, e.g. `postgres://sauron:sauron@localhost:5432/sauron`. The one universally required variable — every binary refuses to start without it. | **required** | all |
| `REDIS_URL` | Redis backing the ingest stream, DSN cache, rate limiter and HLL counters. | `redis://127.0.0.1:6379` | api, ingest, alerts |
| `RUST_LOG` | `tracing` filter directive, e.g. `info,sauron=debug`. | `info,sauron=debug` | all |

### Authentication

| Variable | What it does | Default | Used by |
| --- | --- | --- | --- |
| `JWT_SECRET` | HS256 signing key for access/refresh tokens, and the fallback source for `NOTIFY_SECRET_KEY`. Must be **≥ 32 characters** — generate with `openssl rand -hex 32`. Fail-closed: the services that mint or verify tokens refuse to start without it. `ingest`, `tier` and `migrate` never read it, so they boot fine without one. | **required** (api, monitor, alerts) | api, monitor, alerts |
| `SAURON_DEV` | `1`/`true` relaxes the rule above: a short `JWT_SECRET` is accepted, and a missing one falls back to a compiled-in insecure key. **Local development only** — it makes tokens forgeable. | `false` | api, monitor, alerts |
| `JWT_ACCESS_TTL_SECS` | Access-token lifetime. | `900` (15 min) | api |
| `JWT_REFRESH_TTL_SECS` | Refresh-token lifetime. | `2592000` (30 days) | api |
| `AUTH_REVOCATION_POLL_SECS` | How often each API replica refreshes its revoked-session snapshot. **This is the real kill latency**: a session ended by a logout, a "sign out other devices", an admin force-logout, a deactivation or a password change stops working on a replica at its next poll. Clamped to `1`-`60`. | `5` | api |

### Dashboard API

| Variable | What it does | Default | Used by |
| --- | --- | --- | --- |
| `API_PORT` | TCP port `sauron-api` binds. | `8080` | api |
| `CORS_ALLOWED_ORIGINS` | Comma-separated browser origins allowed to call the API. Must list the origin the dashboard is actually served from. | `http://localhost:3000` | api |
| `DASHBOARD_URL` | Browser-facing origin of the **dashboard**, used to build links inside emails (`https://host/#/reset-password?token=...`). In the shipped nginx topology this is **not** the API's origin — nginx serves the SPA and does not proxy the API — so nothing can derive it. Unset means any email containing a link refuses to render, with an error naming this variable; it does not break anything else. **No default anywhere**, deliberately: a plausible-looking fallback would send mail whose links point at the recipient's own machine while every server-side signal reported success. | unset | api |
| `API_TRUST_FORWARDED_HEADERS` | Honour `X-Forwarded-For` / `X-Real-IP` when identifying the caller. Enable **only** behind a reverse proxy you control that overwrites the header — it is client-controlled otherwise, so turning it on without such a proxy lets a caller pick a fresh rate-limit bucket per request. While it is off *and* a proxy is in front, every request looks like it came from the proxy, so the per-IP limits throttle the whole deployment instead of each client: 10 registrations/hour, 60 logins/min, and 60/min on each of `/v1/auth/forgot-password` and `/v1/auth/reset-password`. Those two windows are 60 seconds rather than an hour precisely so a shared bucket self-heals within a minute; the per-address (3/hour) and per-link (10/hour) budgets are what carry the anti-abuse weight. `password_reset_tokens.requested_from` is also the proxy's address while this is off — a column full of one LAN address is the shipped topology, not a finding. | `false` | api |

### Ingest gateway

| Variable | What it does | Default | Used by |
| --- | --- | --- | --- |
| `INGEST_PORT` | TCP port `sauron-ingest` binds. | `8081` | ingest |
| `INGEST_UDS_PATH` | Listen on this Unix-domain socket instead of TCP. | unset (TCP) | ingest |
| `INGEST_BACKLOG` | TCP `listen()` backlog. Ignored when `INGEST_UDS_PATH` is set. | `4096` | ingest |
| `WORKER_CONCURRENCY` | Co-located pipeline workers draining the Redis stream (enrich → fingerprint → group). | `4` | ingest |
| `INGEST_RATE_LIMIT_PER_MIN` | Envelopes accepted per app per minute. | `6000` | ingest |
| `INGEST_MAX_BODY_BYTES` | Largest accepted envelope body. | `1048576` (1 MiB) | ingest |
| `INGEST_TRUST_FORWARDED_HEADERS` | Same trust caveat as `API_TRUST_FORWARDED_HEADERS`. While off, client IPs are recorded as `NULL` rather than spoofable values. | `false` | ingest |

### Uptime monitoring

| Variable | What it does | Default | Used by |
| --- | --- | --- | --- |
| `MONITOR_TICK_MS` | Scheduler tick — how often due monitors are claimed. | `1000` | monitor |
| `MONITOR_BATCH` | Monitors claimed per tick. | `100` | monitor |
| `MONITOR_MAX_CONCURRENCY` | Probes in flight at once. | `50` | monitor |
| `MONITOR_CHECK_RETENTION_DAYS` | How long individual check rows are kept before the reaper deletes them. | `30` | monitor |
| `MONITOR_SSRF_ALLOW_PRIVATE` | Allow probing private/loopback addresses. Enable only for internal self-monitoring — it is an SSRF guard. | `false` | monitor |

### Alerting & notifications

| Variable | What it does | Default | Used by |
| --- | --- | --- | --- |
| `NOTIFY_SECRET_KEY` | AES-GCM key encrypting stored channel secrets (Slack webhook URLs, SMTP passwords). When unset it is **derived from `JWT_SECRET`** — so rotating `JWT_SECRET` then makes every stored channel secret undecryptable. Set it explicitly to decouple the two, and keep it identical across `api`, `monitor` and `alerts` or they can't read each other's secrets. | unset ⇒ derived from `JWT_SECRET` | api, monitor, alerts |
| `ALERTS_TICK_SECS` | How often metric rules (error spike/threshold, event threshold, latency) are evaluated. Clamped to `5`–`3600`. | `30` | alerts |
| `ALERTS_DELIVER_TIMEOUT_MS` | Per-delivery HTTP/SMTP timeout. | `10000` | alerts |
| `ALERTS_ALLOW_PRIVATE` | Allow delivering to private/loopback targets — an internal webhook or LAN SMTP relay. SSRF guard, same shape as the monitor flag. | `false` | alerts |
| `ALERT_EVENT_RETENTION_DAYS` | How long `alert_events` rows are kept. The table records *every* evaluation, including suppressed ones, so it needs a reaper. | `90` | alerts |
| `NOTIFY_SUBS_TICK_SECS` | How often per-user notification subscriptions are evaluated. Clamped to `30`–`3600`. | `120` | alerts |
| `NOTIFY_SUBS_BATCH` | Rows one notification-drain pass claims at a time. Clamped to `1`–`5000`. | `200` | alerts |
| `NOTIFY_SUBS_MAX_PROBES_PER_ORG` | Per-organization probe ceiling per tick; orgs are processed in rotating order so a clip moves around. Clamped to `1`–`1000`. | `50` | alerts |
| `NOTIFY_DRAIN_BUDGET_MS` | Wall-clock budget for one drain pass, so a backlog cannot stall the tick. Clamped to `500`–`60000`. | `10000` | alerts |
| `NOTIFY_MAX_EMAILS_PER_USER_PER_HOUR` | Above this, a user's notifications are merged into one digest instead of being dropped. Clamped to `1`–`1000`. | `20` | alerts |
| `NOTIFY_QUEUE_RETENTION_DAYS` | How long finished `notification_queue` rows are kept. Pending and claimed rows are never pruned. Clamped to `1`–`365`. | `14` | alerts |

> Monitor up/down alerts fire inline from `sauron-monitor`; only the metric rules need
> the `sauron-alerts` service. Without it those rules are creatable in the UI but never evaluate.

### Transactional email

Deployment-level mail addressed to a **person** — password resets today, digests
later. Separate from the notification channels above: those carry an org's own
SMTP credentials, so routing a user's reset link through one would tell that org's
admin the user asked for a reset, and would strand a user who belongs to no org
entirely. Leaving `SMTP_HOST` unset **disables password reset** and degrades
nothing else — the API boots and serves normally, and logs one INFO line saying
why. Only `sauron-api` reads these; it is also the only process that drains the
queue.

| Variable | What it does | Default | Used by |
| --- | --- | --- | --- |
| `SMTP_HOST` | Relay hostname. Unset ⇒ transactional email is disabled. | unset | api |
| `SMTP_PORT` | Relay port. Also picks the default TLS mode. | `587` | api |
| `SMTP_USERNAME` / `SMTP_PASSWORD` | AUTH credentials. On an RPM install the password belongs in `/etc/sauron/secret.env`, not `api.env`. | unset | api |
| `SMTP_FROM` | Envelope From. **Required** once `SMTP_HOST` is set; a bare address, exactly one `@`, no display name. | unset | api |
| `SMTP_FROM_NAME` | Display name lettre encodes into the From header. | `Sauron` | api |
| `SMTP_TLS` | `implicit`/`smtps`, `starttls`/`required`, or `none`/`plain`. Unset follows the port. `none` sends the password and every reset link in cleartext and is accepted **only** when the relay resolves to loopback — checked at boot against the configured name and again at connect against the resolved address. | `implicit` at port 465, else `starttls` | api |
| `SMTP_ALLOW_PRIVATE` | Allow a relay on a private/LAN address past the SSRF guard. Read on its own; it does **not** inherit `ALERTS_ALLOW_PRIVATE`, which unlocks private delivery for *user-supplied* webhook URLs — a strictly larger surface. | `false` | api |
| `SMTP_TIMEOUT_MS` | Per socket operation. The whole send, DNS included, is bounded at 3× this and capped at 60s. Clamped to `1000`–`60000`. | `10000` | api |
| `SMTP_SINK` | Write mail to the log instead of sending it. Rows are recorded `status='sink'`, never `'sent'`. Read on its own; it does **not** inherit `SAURON_DEV`. The **body** is logged only when `SAURON_DEV=1` as well — a logged body is a working account-takeover URL in your log aggregator. | `false` | api |
| `MAIL_DRAIN_TICK_SECS` | Outbox drain cadence. Clamped to `10`–`3600`. | `60` | api |
| `MAIL_OUTBOX_RETENTION_DAYS` | How long delivered/failed outbox rows are kept before the reaper deletes them. | `30` | api |

> Emails containing a link also need [`DASHBOARD_URL`](#dashboard-api). Without it
> the message refuses to render rather than sending a link to nowhere.

### Hot/cold tiering

| Variable | What it does | Default | Used by |
| --- | --- | --- | --- |
| `TIER_HOT_DAYS` | Age at which a partition is exported to Parquet and becomes eligible to leave Postgres. **Three binaries derive their own boundary from this**, so it is a shared setting rather than a tier knob: on an RPM host it lives in `/etc/sauron/sauron.env`, not `tier.env`. A divergence means the PII masker rewriting rows in a partition the tier worker has already exported — Postgres masked, Parquet raw, and the later drop destroys the only masked copy. | `30` | tier, inspector, api |
| `TIER_GRANULARITY` | Partition granularity. | `day` | tier |
| `TIER_COLD_PATH` | Directory holding the cold Parquet files. `sauron-api` must be able to **read the same path** — it answers cross-tier queries from it. | `/var/lib/sauron/cold` | tier, api |
| `TIER_DROP_LAG_HOURS` | Grace period between exporting a partition and dropping it from Postgres. | `24` | tier |
| `TIER_TICK_SECS` | Tiering loop cadence. | `3600` | tier |
| `TIER_PARTITION_AHEAD` | How many future partitions to pre-create. | `7` | tier |

### Search & query planner

| Variable | What it does | Default | Used by |
| --- | --- | --- | --- |
| `SEARCH_SCAN_CLAMP_DAYS` | Window an unindexed search query (wildcard, substring, or free-text match) is clamped to. Defaults to `TIER_HOT_DAYS`: clamping a scan further back than the tier worker's hot window buys nothing, since older rows are already gone from Postgres — so the default is simultaneously the honest cost bound and the honest coverage bound. | `TIER_HOT_DAYS` (`30`) | api |

### PII inspector

Finds developer-supplied personal data in the telemetry `jsonb` columns, masks it
irreversibly in **hot Postgres**, and enforces the mask on future ingest. Masking
does **not** reach cold Parquet, the Redis dead-letter queue or anything already
delivered by an alert — every surface says so, and so does this table.

`INSPECTOR_ENABLED` is off by default because the scanner reads the same
partitions the ingest path writes. `sauron-inspector` opens its own 4-connection
pool: raise Postgres `max_connections` to at least 150 before enabling it, or
exhaustion surfaces as `sauron-api` 500s and `sauron-ingest` returning 202 and
then dropping the event. The three keys marked *shared* below are read by more
than one binary and, on an RPM host, live in `/etc/sauron/sauron.env` rather than
`inspector.env` — the units that need them never load `inspector.env`.

| Variable | What it does | Default | Used by |
| --- | --- | --- | --- |
| `INSPECTOR_ENABLED` | Master switch. While false the binary starts, logs one line and idles — it never reads a telemetry row. | `false` | inspector |
| `INSPECTOR_TICK_SECS` | Scheduler cadence. This loop only claims *due* policies, so it is never blocked by a running scan. Clamped to `5`–`3600`. | `30` | inspector |
| `INSPECTOR_BATCH_ROWS` | Rows read per phase-1 batch. The `LIMIT` sits on an index-bounded inner window, so this bounds rows **scanned**, not rows matched. Raising it lengthens the gap between heartbeats and between inter-batch pauses. | `5000` | inspector |
| `INSPECTOR_BATCH_PAUSE_MS` | Sleep between scan batches. This plus the batch size *is* the duty cycle that keeps the ingest working set resident in the buffer cache. | `200` | inspector |
| `INSPECTOR_LEASE_SECS` | A scan whose heartbeat is older than this is re-claimable by another worker. Set below the slowest single unit and you get needless re-claims. | `120` | inspector |
| `INSPECTOR_MAX_ATTEMPTS` | After this many claims a scan finalizes as `failed`, so one poison unit cannot loop forever. | `3` | inspector |
| `INSPECTOR_STATEMENT_TIMEOUT_MS` | Per-connection `statement_timeout`, set at checkout and `RESET` before the connection returns to the pool. | `30000` | inspector |
| `INSPECTOR_WINDOW_DAYS` | Scan window ceiling. Defaults to `SEARCH_SCAN_CLAMP_DAYS`, which itself defaults to `TIER_HOT_DAYS` — nothing older is in Postgres anyway, so a larger value buys coverage that does not exist. | `SEARCH_SCAN_CLAMP_DAYS` (`30`) | inspector |
| `INSPECTOR_DETECTOR_WINDOW_DAYS` | Window for detector mode, which drops the SQL prefilter and walks every string leaf of every row: roughly 20× the CPU and 20× the bytes shipped out of Postgres. On a 30M-row app that is the difference between a scan that finishes overnight and one still running at noon. | `7` | inspector |
| `INSPECTOR_MAX_PHASE2_ROWS_PER_UNIT` | Phase-2 rows per unit before counts become **lower bounds** and the scan is reported `partial` rather than `full`. | `200000` | inspector |
| `INSPECTOR_DEFAULT_SWEEP_ROWS` | Truncation point for the default-partition sweep. Those rows are never tiered and never dropped, so on a deployment that had data before the partitioning migration this child can be very large. | `50000` | inspector |
| `INSPECTOR_CATCHUP_GRACE_HOURS` | A missed scheduled run older than this is **skipped, not replayed**. A 03:00 scan firing at 09:00 on a Monday is precisely the production load spike the schedule existed to avoid. | `6` | inspector |
| `INSPECTOR_SCAN_KEEP` | Scans retained per policy. Their findings are deleted in bounded batches before the parent row goes. | `20` | inspector |
| `INSPECTOR_FINDING_RETENTION_DAYS` | Finding retention. A nightly scan producing 33k findings is 12M rows a year without this. | `90` | inspector |
| `INSPECTOR_MASK_BATCH` | Rows rewritten per retro-mask batch. Halved automatically when any target carries a wildcard, because the array rebuild re-serializes the whole array per row. | `2000` | inspector |
| `INSPECTOR_MASK_PAUSE_MS` | Sleep between mask batches. A 2000-row batch is ≈0.37 s of write on `error_events` (13 index updates per row), so 200 ms is a ≈65% duty cycle. Raise it if ingest latency moves during a mask. | `200` | inspector |
| `INSPECTOR_CLAIM_STALE_SECS` | A mask action claimed longer ago than this is re-claimable — this is the crash-resume mechanism. The cursor is durable, so a re-claim never double-counts. | `300` | inspector |
| `INSPECTOR_PREVIEW_GC_DAYS` | Abandoned preview retention. Previews are not audit-relevant. | `7` | inspector |
| `INSPECTOR_AUDIT_RETENTION_DAYS` | Mask-audit retention. **`0` = never prune**, which is the default: the table grows per human action, not per rule evaluation, and it is the record a compliance question is answered from. | `0` (never) | inspector |
| `INSPECTOR_AUDIT_PII_DAYS` | Age at which staff emails and `confirm_source` are nulled on audit rows, keeping the counts and targets. Without it the privacy feature is the only un-erasable store of staff PII in the schema, because deleting a user is this product's de-facto erasure mechanism everywhere else. | `730` | inspector |
| `INSPECTOR_MASK_MAX_ROWS` | The affected-row ceiling a mask confirm refuses above — raise it explicitly rather than by accident. | `20000000` | api |
| `INSPECTOR_PREVIEW_TTL_SECS` | Preview freshness, measured from the preview **completing**, not from the request — otherwise a queued preview can expire before it is readable. Confirming with a stale preview is refused. | `900` | api |
| `INSPECTOR_EXPORT_MAX_ROWS` | Buffered findings-CSV ceiling. A buffered export cannot be truncated honestly, so above this the route answers `400` rather than shipping a silent prefix. | `50000` | api |
| `INSPECTOR_POLICY_CACHE_SECS` | *Shared.* How long a mask takes to reach every ingest replica: the enforcer's per-app cache TTL, and the number the dashboard states to the operator in words. Raising it delays enforcement; lowering it adds one indexed query per app per interval on the ingest pool. | `30` | ingest, api |
| `INSPECTOR_TAIL_SWEEP_SECS` | *Shared.* How far back the retro-mask's tail sweep re-checks. Clamped at load to at least **4×** `INSPECTOR_POLICY_CACHE_SECS`: a sweep shorter than the cache TTL closes nothing, and rows written in that window stay raw forever because the retro-mask is a one-shot job. | `120` (≥ 4× cache TTL) | inspector, api |

### Source maps & symbolication

| Variable | What it does | Default | Used by |
| --- | --- | --- | --- |
| `SYMBOLS_CACHE_MB` | In-process parsed-index LRU byte budget. | `256` | api, ingest |
| `SYMBOLS_REDIS_URL` | Redis holding warm symbol blobs; unset disables that tier (in-process cache only). Point it at a **separate Redis instance** — `maxmemory` is instance-wide, so sharing the ingest Redis lets symbol blobs evict stream state. | unset (disabled) | api, ingest |
| `SYMBOLS_REDIS_MAX_BLOB_MB` | Blobs larger than this are never cached in Redis. The backstop when a separate instance isn't used. | `8` | api, ingest |
| `SYMBOLS_MAX_ARTIFACT_MB` | Reject artifact uploads whose raw file exceeds this size. | `128` | api |
| `SYMBOLS_MAX_UNCOMPRESSED_MB` | Decompression-bomb guard: cap on a blob's uncompressed size. | `512` | api, ingest |
| `SYMBOLS_INGEST_TIMEOUT_MS` | Time box for symbolication on the ingest path; on timeout the raw trace is stored and marked `pending` for on-read symbolication. | `150` | ingest |

### Dashboard (browser-facing URLs)

The dashboard is a static bundle, so these are read at **container start** and written
into `config.js`. They describe the URLs as the *browser* sees them — host-published
ports, not Compose-network service names.

| Variable | What it does | Default | Used by |
| --- | --- | --- | --- |
| `API_BASE_URL` | API base URL the dashboard calls. | `http://localhost:8080` | dashboard |
| `INGEST_BASE_URL` | Ingest base URL, used to render environment DSNs. | `http://localhost:8081` | dashboard |
| `VITE_API_BASE_URL` | Build-time override for `npm run dev`; loses to the runtime value above. | unset ⇒ `http://localhost:8090` | dashboard (dev) |
| `VITE_INGEST_BASE_URL` | Build-time override for `npm run dev`. | unset ⇒ `http://localhost:8091` | dashboard (dev) |

### Docker Compose only

Consumed by [`docker-compose.yml`](docker-compose.yml) itself — they provision the
bundled Postgres container *and* are interpolated into every service's `DATABASE_URL`.

| Variable | What it does | Default |
| --- | --- | --- |
| `POSTGRES_USER` | Superuser for the bundled Postgres container. | `sauron` |
| `POSTGRES_PASSWORD` | Its password. | `sauron` |
| `POSTGRES_DB` | Database created on first boot. | `sauron` |

### crebain (load generator)

| Variable | What it does | Default |
| --- | --- | --- |
| `CREBAIN_DSN` | Target DSN in direct mode — the env equivalent of `--dsn`. Mutually exclusive with `--isolated`. | unset |
| `DATABASE_URL` | Fallback for `--database-url` when running `--isolated` (used to create the ephemeral DB). | unset (required for `--isolated`) |
| `REDIS_URL` | Fallback for `--redis-url` when running `--isolated`. | `redis://127.0.0.1:6379` |

### Build-time

| Variable | What it does | Default |
| --- | --- | --- |
| `DUCKDB_LIB_DIR` | Directory containing `libduckdb.so`, so `sauron-tier` links DuckDB dynamically instead of compiling the C++ amalgamation — the slowest item in the workspace build. `packaging/rpm/fetch-libduckdb.sh` prints a suitable path. | unset (needs a system libduckdb, or the `bundled` cargo feature) |
| `DUCKDB_INCLUDE_DIR` | Directory containing `duckdb.h`. Same value as above when using the fetch script. | unset |
| `DUCKDB_VENDOR_DIR` | Cache directory used by `fetch-libduckdb.sh`. | `<repo>/.cache/duckdb` |

### SDK examples

The runnable apps under [`examples/`](examples) read their own DSN from the environment
so the SDKs themselves stay config-free. Not used by the backend.

| Variable | What it does | Default |
| --- | --- | --- |
| `SAURON_DSN` | DSN for the Node / Python / C# example servers. Unset ⇒ the SDK runs in no-op mode. | unset (disabled) |
| `SAURON_RELEASE` | Value passed as `release` by the Node example. | `1.0.0` |

## Sending your first event

**Web (`@edraj/sauron-browser`):**
```js
import { Sauron } from '@edraj/sauron-browser';
Sauron.init({ dsn: 'http://<public_key>@localhost:8081/<environment_id>' });
throw new Error('hello from the browser');   // auto-captured & grouped
Sauron.track('checkout_completed', { cart_value: 42.5 });
Sauron.identify('u_42', { plan: 'pro' });
```

**Flutter (`sauron_flutter`):**
```dart
await Sauron.init(
  SauronOptions(dsn: 'http://<public_key>@localhost:8081/<environment_id>'),
  appRunner: () => runApp(const MyApp()),
);
Sauron.track('checkout_completed', properties: {'cart_value': 42.5});
```

## The wire contract

One JSON envelope, shared by both SDKs and the backend (defined in `backend/crates/sauron-core/src/envelope.rs`; a golden fixture guards parity across all three test suites):

```
POST /api/{environment_id}/envelope
X-Sauron-Key: <public_key>          # or ?k=<public_key> for sendBeacon
Content-Encoding: gzip              # optional
```

The DSN's key identifies exactly one environment (and therefore one app, project and
org); the `{environment_id}` path segment is informational only — the gateway
authenticates on the key alone, so an event's environment is proven by the key it
arrived with and cannot be spoofed by a client. See the
**[Ingest Wire Contract](wiki/Ingest-Wire-Contract.md)** for the full DSN shape.

Error grouping is line-number–independent: two occurrences of the same bug on different lines/releases collapse into one issue.

## Testing

```bash
cd backend && cargo test --workspace     # fingerprint grouping, JWT, envelope parity
cd sdks/js && npm test                   # envelope shape, stacktrace parsing, offline queue
cd sdks/flutter && flutter test          # golden envelope, error capture, queue
```

An end-to-end check — register → create project → POST an envelope to `:8081` → the grouped issue appears via `:8080` — is described in [`plan.md`](plan.md) under *Verification*.

## License

[AGPL-3.0-only](LICENSE) — GNU Affero General Public License v3.0.
