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
 @sauron/browser  ┐                        ┌─────────────────────────────┐
 sauron_flutter   ├── gzip envelope ──────▶│ sauron-ingest (axum edge)   │
                  ┘  POST /api/{pid}/       │  DSN auth → rate-limit →     │
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
       └─ App       (app_type + its own DSN — the ingest unit)
            └─ Environments, Issues, Events, People   (keyed by app_id)
```

One product ("Project X") can hold many heterogeneous **apps** (e.g. 3 Flutter apps + 2 webapps), each with its own DSN. `app_type ∈ web · flutter · ios · android · react_native · node`.

**Fine-grained RBAC.** Atomic permissions (`issue:read`, `issue:write`, `event:read`, `app:*`, `project:*`, `member:*`, `role:manage`, `org:manage`) are bundled into **roles**. Four presets ship — **Owner ⊇ Admin ⊇ Developer ⊇ Viewer** — plus custom roles. A user is granted a role at **org, project, or app** scope; permissions resolve as a **union down the tree** (an org grant covers everything; a project grant covers its apps but not siblings; an app grant is narrowest). So "Admin of Project X, Viewer of Project Y" is expressible. Grants and custom roles are guarded against privilege escalation (you can't grant permissions you don't hold). The dashboard reads `GET /v1/orgs/{org}/access` and hides actions the caller can't perform. See [`docs/audit-2026-07-12-rbac.md`](docs/audit-2026-07-12-rbac.md).

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
  js/             @sauron/browser (TypeScript, tsup)
  flutter/        sauron_flutter (Dart)
examples/
  svelte-web/     runnable demo webapp wired to @sauron/browser
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

Register in the dashboard, create a project → an app, copy that app's DSN into an SDK, and watch the first event land.

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
`ingest`, `monitor`, `alerts`, `tier`, `migrate`, `dashboard`.

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

### Dashboard API

| Variable | What it does | Default | Used by |
| --- | --- | --- | --- |
| `API_PORT` | TCP port `sauron-api` binds. | `8080` | api |
| `CORS_ALLOWED_ORIGINS` | Comma-separated browser origins allowed to call the API. Must list the origin the dashboard is actually served from. | `http://localhost:3000` | api |
| `API_TRUST_FORWARDED_HEADERS` | Honour `X-Forwarded-For` / `X-Real-IP` when identifying the caller. Enable **only** behind a reverse proxy you control that overwrites the header — it is client-controlled otherwise. While it is off *and* a proxy is in front, every request looks like it came from the proxy, so the per-IP auth limits (10 registrations/hour, 60 logins/min) throttle the whole deployment instead of each client. | `false` | api |

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

> Monitor up/down alerts fire inline from `sauron-monitor`; only the metric rules need
> the `sauron-alerts` service. Without it those rules are creatable in the UI but never evaluate.

### Hot/cold tiering

| Variable | What it does | Default | Used by |
| --- | --- | --- | --- |
| `TIER_HOT_DAYS` | Age at which a partition is exported to Parquet and becomes eligible to leave Postgres. | `30` | tier |
| `TIER_GRANULARITY` | Partition granularity. | `day` | tier |
| `TIER_COLD_PATH` | Directory holding the cold Parquet files. `sauron-api` must be able to **read the same path** — it answers cross-tier queries from it. | `/var/lib/sauron/cold` | tier, api |
| `TIER_DROP_LAG_HOURS` | Grace period between exporting a partition and dropping it from Postgres. | `24` | tier |
| `TIER_TICK_SECS` | Tiering loop cadence. | `3600` | tier |
| `TIER_PARTITION_AHEAD` | How many future partitions to pre-create. | `7` | tier |

### Search & query planner

| Variable | What it does | Default | Used by |
| --- | --- | --- | --- |
| `SEARCH_SCAN_CLAMP_DAYS` | Window an unindexed search query (wildcard, substring, or free-text match) is clamped to. Defaults to `TIER_HOT_DAYS`: clamping a scan further back than the tier worker's hot window buys nothing, since older rows are already gone from Postgres — so the default is simultaneously the honest cost bound and the honest coverage bound. | `TIER_HOT_DAYS` (`30`) | api |

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
| `INGEST_BASE_URL` | Ingest base URL, used to render app DSNs. | `http://localhost:8081` | dashboard |
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

**Web (`@sauron/browser`):**
```js
import { Sauron } from '@sauron/browser';
Sauron.init({ dsn: 'http://<app_public_key>@localhost:8081/<app_id>' });
throw new Error('hello from the browser');   // auto-captured & grouped
Sauron.track('checkout_completed', { cart_value: 42.5 });
Sauron.identify('u_42', { plan: 'pro' });
```

**Flutter (`sauron_flutter`):**
```dart
await Sauron.init(
  (o) => o.dsn = 'http://<app_public_key>@localhost:8081/<app_id>',
  appRunner: () => runApp(const MyApp()),
);
Sauron.track('checkout_completed', properties: {'cart_value': 42.5});
```

## The wire contract

One JSON envelope, shared by both SDKs and the backend (defined in `backend/crates/sauron-core/src/envelope.rs`; a golden fixture guards parity across all three test suites):

```
POST /api/{app_id}/envelope
X-Sauron-Key: <public_key>          # or ?k=<public_key> for sendBeacon
Content-Encoding: gzip              # optional
```

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
