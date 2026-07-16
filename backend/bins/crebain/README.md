# crebain 🐦‍⬛

A load/benchmark generator for the Sauron **ingest write path**. A fixed pool of
virtual users hammers the ingest edge (`POST /api/{app_id}/envelope`), exercising
all five envelope signal types — `error`, `event`, `identify`, `transaction`,
`breadcrumb_batch`.

*crebain* = the spy-crows of Dunland (LOTR): a flock hammering the tower.

## The four knobs

| Flag | Default | Meaning |
|---|---|---|
| `--users` | 1000 | Fixed pool of virtual users (each a stable `crebain-user-{i}`) |
| `--duration` | 60 | Run length, seconds |
| `--events-per-min` | 10 | Analytics events **per user** per minute (`0` disables) |
| `--issues-per-min` | 10 | Errors (issues) **per user** per minute (`0` disables) |

Each user emits at the per-user rate for the whole run, so `1000 × 10/min` =
10k events/min + 10k issues/min at defaults. The pool is reused (never grows past
`--users`). Derived automatically: one `identify` per user at start, one
`transaction` per event, one `breadcrumb_batch` per error — so all five types fire
without extra flags. `--no-gzip` sends envelopes uncompressed (gzip is on).

## Two modes

### Isolated (self-contained, self-cleaning) — recommended

Creates its own ephemeral database + dedicated ingest, runs, then **drops
everything** — on completion, error, or Ctrl-C.

```bash
cargo build -p sauron-ingest -p crebain          # crebain spawns the ingest sibling binary

cargo run -p crebain -- --isolated \
  --database-url postgres://sauron:sauron@localhost:5432/sauron \
  --redis-url    redis://localhost:6379 \
  --users 1000 --duration 60
```

What it does, in order: `CREATE DATABASE crebain_bench_<id>` → run migrations →
seed one org/project/app (mints a DSN) → `FLUSHDB` an isolated Redis index
(`--redis-bench-db`, default 15) → spawn `sauron-ingest` on `--ingest-port`
(default 8091) with a high rate limit → run the load → **drop the database, flush
Redis, kill the ingest**. `--keep` retains the bench database for inspection.

Requirements: the Postgres role needs `CREATEDB`; the `sauron-ingest` binary must
be built (crebain looks for it next to its own binary, or pass `--ingest-bin`).

### Direct — point at a running edge

```bash
cargo run -p crebain -- --dsn 'http://<public_key>@localhost:8081/<app_id>'
# or: CREBAIN_DSN=... cargo run -p crebain --
```

No database management. All users share one app, so the ingest's per-app rate
limit applies — crebain reports `429`s as a first-class metric rather than hiding
them.

## Output

A live once-a-second line, then a summary: target-vs-achieved throughput,
per-signal item counts (accepted / attempted), HTTP status breakdown
(202 / 429 / other / transport errors), and latency p50/p90/p99/max.

## Isolated-mode flags

`--database-url` (env `DATABASE_URL`) · `--redis-url` (env `REDIS_URL`) ·
`--redis-bench-db` (15) · `--ingest-port` (8091) · `--ingest-bin` ·
`--rate-limit` (high) · `--keep`.

## HTML report

Pass `--report <path>` to write a self-contained HTML report (opens offline, no
network) alongside the text summary:

```
crebain --isolated --database-url "$DATABASE_URL" --duration 60 --report bench.html
```

The report charts requests/sec and success/fail records (cumulative and
per-second) over the run. In `--isolated` mode it also charts the ingest
server's CPU (in cores) and RSS memory over time, sampled once per second from
`/proc`. In `--dsn` mode there is no server process to sample, so the CPU/RAM
charts are omitted.

## Design

See [`docs/superpowers/specs/2026-07-15-crebain-benchmark-design.md`](../../../docs/superpowers/specs/2026-07-15-crebain-benchmark-design.md).
