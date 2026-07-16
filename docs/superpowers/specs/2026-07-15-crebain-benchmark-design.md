# crebain — load/benchmark generator (design)

**Date:** 2026-07-15
**Status:** approved (brainstorm), implementing
**Crate:** `backend/bins/crebain` (workspace bin, `members = ["bins/*"]`)

## Purpose

A benchmark tool that exercises the full Sauron write path — the ingest edge and
all five envelope signal types — under configurable synthetic load, optionally
against a fully **isolated, self-cleaning** ephemeral database + ingest stack.

*crebain* = the spy-crows of Dunland (LOTR): a flock hammering the tower.

## Configuration (the four headline knobs)

| Flag | Default | Meaning |
|---|---|---|
| `--users` | 1000 | Size of the fixed pool of virtual users |
| `--duration` | 60 | Run length, seconds |
| `--events-per-min` | 10 | Analytics events emitted **per user** per minute |
| `--issues-per-min` | 10 | Errors (issues) emitted **per user** per minute |

**Load model:** a *fixed pool* of `N = --users` virtual users, each with a stable
`distinct_id` (`crebain-user-{i}`) and randomized-once traits, runs for the whole
duration. **Each** user emits at the configured per-user rates (so 1000 × 10/min =
10k events/min + 10k issues/min at defaults). No identities are minted beyond N —
the pool *is* the users, looping. A rate of `0` disables that stream.

**Derived signals (keeps the 4-knob UX, still exercises all 5 types):**
- `identify` — once, when a user first starts.
- `transaction` — one per event (event tick sends `[event, transaction]`).
- `breadcrumb_batch` — one per error (issue tick sends `[breadcrumb_batch, error]`;
  the error also carries inline breadcrumbs).

## Modes

Both modes converge on a `Target { base_url, dsn }` + optional teardown hook; the
load engine, generator, client, metrics, and reporting are **identical**.

### Direct mode — `--dsn <DSN>`
Point at an already-running ingest edge. DSN is the SDK format
`scheme://<public_key>@host:port/<app_id>` (or `CREBAIN_DSN`). No DB management.
The shared per-app rate limit applies; `429`s are reported, not hidden.

### Isolated mode — `--isolated` (flagship; mutually exclusive with `--dsn`)
crebain owns an ephemeral stack end-to-end:

1. **Create** `crebain_bench_<hex>` on the Postgres **admin** URL (`--database-url`
   / `DATABASE_URL`) via `CREATE DATABASE` (raw DDL through `batch_execute`, simple
   protocol — the tier crate's precedent). Connecting role needs `CREATEDB`.
2. **Migrate** — `sauron_db::run_pending_migrations(bench_url)`.
3. **Seed** — `create_org` → `create_project` → `create_app` (self-generated
   `public_key`, `app_type = "web"`) → compose the DSN. No user/RBAC needed;
   ingest resolves by key → `app_ancestry`.
4. **Isolate Redis** — a separate DB index (`--redis-bench-db`, default 15);
   `FLUSHDB` for a clean slate (clears any stale stream / consumer group).
5. **Spawn** its own `sauron-ingest` child pointed at the bench DB + bench Redis,
   on `--ingest-port` (default 8091), with `INGEST_RATE_LIMIT_PER_MIN` set very
   high (`--rate-limit`) so the benchmark measures throughput, not throttling.
   Binary located as a sibling of crebain's own exe (`--ingest-bin` override;
   clear error → `cargo build -p sauron-ingest`). `RUST_LOG=warn` for the child.
6. **Wait** — poll `GET /ready` until healthy → hand back the `Target`.

**Teardown** (`HarnessGuard`, best-effort, idempotent, once-only) runs on normal
completion, **Ctrl-C** (`tokio::signal`), and failure. The run is wrapped in a
`select!`; teardown always follows; a `Drop` backstop does a blocking last-ditch
`DROP DATABASE` if a panic slips through. Steps: **drop crebain's own seed pool**
(so no lingering connection blocks the drop) → kill the ingest child → `DROP
DATABASE IF EXISTS "crebain_bench_<hex>" WITH (FORCE)` → `FLUSHDB` the bench Redis
index. `--keep` skips teardown to inspect the DB after a run.

## Modules (small, single-purpose)

| File | Responsibility |
|---|---|
| `main.rs` | parse CLI → build `RunConfig` → resolve mode → run → report → exit code; wires Ctrl-C + teardown |
| `cli.rs` | clap `Args`; validate into `RunConfig`; mode resolution; per-user intervals from rates |
| `dsn.rs` | parse SDK-format DSN → `{ base_url, app_id, public_key }`; build a DSN from parts (isolated mode) |
| `db_url.rs` | pure helpers: `swap_database(pg_url, db)`, `swap_redis_db(redis_url, n)`, `bench_db_name()` |
| `harness.rs` | isolated-mode setup + `HarnessGuard` teardown (create/migrate/seed/spawn/ready → drop) |
| `user.rs` | `VirtualUser` identity + counters; decides what is due each tick |
| `generator.rs` | pure builders of `sauron_core` envelope items with seeded pseudo-random fake data |
| `client.rs` | `IngestClient` over one shared `reqwest::Client`; gzip body, headers; returns `SendOutcome` |
| `engine.rs` | spawn N user tasks against a deadline; feed outcomes to the metrics channel |
| `metrics.rs` | single-owner aggregator: per-status + per-signal counters, latency samples; snapshot + finalize |
| `report.rs` | live 1s progress line + final summary table |

**Concurrency shape:** one tokio task per virtual user; each user's requests are
**sequential (awaited)** so in-flight ≤ N and a slow server lowers *achieved* rate
below *target* (surfaced, not queued). Outcomes flow over one mpsc channel to a
single metrics-aggregator task (single owner → no lock contention). One shared
`reqwest::Client` (connection pooling) across all users.

## Reporting

Live line each second: elapsed, req/s, accepted/failed, active users. Final
summary: total requests & items; per-signal-type counts; **target vs achieved**
throughput; HTTP status breakdown (**202 / 429 / other / transport-error**);
latency **p50/p90/p99/max** (exact from sorted samples; reservoir-capped if a run
gets huge). Non-zero exit if the run could not start (bad DSN, setup failure).

## New dependencies

`clap` (derive) and `flate2` (gzip request bodies) added to `backend/Cargo.toml`
workspace deps. `redis` (already a workspace dep) for the teardown `FLUSHDB`.
Everything else already exists (`tokio`, `reqwest`, `serde_json`, `uuid`, `chrono`,
`anyhow`, `tracing`, `sauron-core`, `sauron-db`).

**Small additive change to `sauron-db`:** `create_database` / `drop_database`
helpers (encapsulate diesel-async `batch_execute` DDL + identifier validation) so
crebain stays free of a direct diesel dependency. Purely additive → low conflict
risk with concurrent work.

## Testing

- **Unit:** DSN parse (valid/invalid); `db_url` swaps (with/without query, auth,
  existing index); bench-DB naming/validation; interval math (incl. rate 0);
  generator output round-trips as `sauron_core::Envelope` with all 5 item types;
  metrics percentile/counter correctness; `HarnessGuard` once-only teardown (fake).
- **Integration (`#[ignore]`-gated, needs live PG+Redis):** full isolated
  round-trip — setup → tiny load → teardown — asserting the bench DB is created
  then gone. Keeps `cargo test` hermetic.

## Out of scope (YAGNI)

Read/dashboard API calls, tenancy provisioning beyond the one seeded app,
ramp-up curves, think-time distributions, JSON artifact output, multi-machine
coordination.
