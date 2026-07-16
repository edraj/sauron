# crebain 🐦‍⬛

A load/benchmark generator for the Sauron **ingest write path**. It drives
`POST /api/{app_id}/envelope` and exercises all five envelope signal types —
`error`, `event`, `identify`, `transaction`, `breadcrumb_batch`.

*crebain* = the spy-crows of Dunland (LOTR): a flock hammering the tower.

The engine is an **open-model rate scheduler over a bounded worker pool**:
work is generated at the target rate on a schedule, independent of how fast
requests complete, and a semaphore of size `--max-inflight` caps how many are
ever in flight at once. Latency is measured from each item's *scheduled* send
time (coordinated-omission correction), not from when a worker happened to
pick it up.

## The knobs

| Flag | Default | Meaning |
|---|---|---|
| `--users` | 1000 | Distinct identities (`crebain-user-{i}`) the workload models |
| `--duration` | 60 | Run length, seconds |
| `--events-per-min` | 10 | Analytics events **per user** per minute (`0` disables) |
| `--issues-per-min` | 10 | Errors (issues) **per user** per minute (`0` disables) |
| `--no-gzip` | off | Send envelopes uncompressed (gzip is on by default) |
| `--report <path>` | — | Write a self-contained HTML benchmark report |
| `--dsn` | — | Direct mode: SDK-format DSN of a running ingest edge (env `CREBAIN_DSN`) |
| `--isolated` | off | Isolated mode: ephemeral DB + dedicated ingest, self-cleaning |
| `--max-inflight` | 8192 | Max concurrent in-flight requests — the worker-pool size / connection ceiling |
| `--ramp` | 5 | Seconds over which the initial per-user identify + connection opens are spread (no t=0 herd) |
| `--rps` | — | Explicit aggregate request rate (req/s), overriding the users×rates derivation |
| `--source-ips` | auto | Number of loopback source IPs to fan out across (default: derived from `--max-inflight`) |
| `--transport` | `tcp` | `tcp` or `uds` |
| `--uds-path` | auto (isolated) | Unix-domain-socket path; isolated mode picks one when `--transport uds` |
| `--live-sockets` | off | Hold connections open for a literal-concurrency (peak-sockets) demo instead of a request loop |

Derived automatically, no extra flags needed: one `identify` per user at
start, one `transaction` per event, one `breadcrumb_batch` per error — so all
five signal types fire by default.

### Isolated-mode-only flags

| Flag | Default | Meaning |
|---|---|---|
| `--database-url` | env `DATABASE_URL` | Postgres admin URL for CREATE/DROP DATABASE (needs `CREATEDB`) |
| `--redis-url` | env `REDIS_URL`, else `redis://127.0.0.1:6379` | Base Redis URL; the bench uses a separate DB index of it |
| `--redis-bench-db` | 15 | Redis DB index isolated for the bench stream/rate-limit |
| `--ingest-port` | 8091 | Port for the spawned bench ingest |
| `--ingest-bin` | sibling of crebain's own exe | Path to the `sauron-ingest` binary |
| `--rate-limit` | 100,000,000 | Per-app rate limit for the bench ingest (high, so it doesn't throttle) |
| `--keep` | off | Keep the bench database after the run instead of dropping it |

## "1M concurrent users" means two different things

**(A) 1M distinct identities at aggregate load — the default, and the useful
one.** `--users 1000000` models a million identities each ticking at
`--events-per-min` / `--issues-per-min`. By Little's Law, sustaining a given
throughput needs `rate × average-latency` concurrent requests in flight, not
one connection per user — a few thousand reused keep-alive sockets comfortably
carry a million-identity workload. This is what `--users` plus the rate flags
model, and `--max-inflight` caps how much concurrency it actually uses.
Portable: works against any target, local or remote.

**(B) 1M sockets literally open at once — `--live-sockets`.** This switches
the engine to a hold-open loop instead of a request loop: it opens connections
and keeps them idle-open for the run duration, so the report can state a real
**peak connections** number. It's a connection-*capacity* demo, localhost-only,
and is never reported as req/s — holding a socket open isn't serving a request.

## The TCP wall, and how crebain beats it

One source IP talking to one `dst_ip:port` is capped at the ephemeral port
range (`/proc/sys/net/ipv4/ip_local_port_range`, typically `32768–60999`, de-rated
~10% for TIME_WAIT headroom) — about **28,232** simultaneous connections. That
ceiling is per `(src_ip, dst_ip, dst_port)` tuple and is absolute; it doesn't
move no matter how the client is written.

crebain works around it four ways:

1. **Bounds in-flight** with `--max-inflight` so the pool never tries to hold
   more connections than intended.
2. **Fans client source IPs out across `127.0.0.0/8`** when the target is
   loopback, walking `127.0.0.1, 127.0.0.2, … 127.0.1.1, …` — each extra source
   IP buys another ~28,232-connection budget, so the ceiling scales past 28k.
3. **Offers a Unix-domain-socket transport** (`--transport uds`) which has no
   port concept at all, so there is no TCP wall to hit.
4. **Raises `RLIMIT_NOFILE`** (best-effort, soft limit up to the hard cap) so
   the process itself doesn't run out of file descriptors first.

## Honest ceilings

- A single box cannot hold 1,000,000 sockets open to one **remote, unmodified**
  `ip:port` — the ~28,232 wall from one source IP is a hard OS/network fact.
  Only additional source IPs (loopback only) or additional boxes push past it.
- Source-IP fan-out is **loopback-only**. Against a real remote host, a
  `127.x` source address can't route there, so crebain never fans out off-box —
  a non-loopback target is capped at one tuple's budget and the report warns
  about it.
- A literal 1,000,000 live sockets needs **root** to raise the *hard*
  `RLIMIT_NOFILE` — the soft/hard default on this box is 524288, so roughly
  ~500k file descriptors is the unprivileged ceiling. Beyond that also needs
  `net.ipv4.tcp_mem` and `net.core.somaxconn` tuned up, or the kernel starts
  rejecting or dropping connections before crebain's own limits bind.
- The headline workload **1,000,000 users × ~110 req/min ≈ 1.83M req/s**
  (100 events/min + 10 issues/min per user, plus identify) is more than a
  single ingest box can absorb — the write path serializes through one
  single-threaded Redis (`INCR` for rate limiting, `XADD` per item), good for
  roughly **10k–100k req/s**, not millions. That configuration is a
  distributed workload; crebain reports it honestly as offered-vs-accepted
  with load shed (`behind`), it does not pretend a single ingest can keep up.

## Reading the report

- **Offered vs accepted req/s** — offered is what the scheduler tried to send
  at the target rate; accepted is what got a 2xx back. They diverge exactly
  when the target is overloaded.
- **Transport-error %** — connection failures, timeouts, resets — distinct
  from HTTP-level rejections (429s etc.).
- **Peak in-flight** — the highest number of requests concurrently in the
  worker pool during the run (bounded by `--max-inflight`).
- **Peak connections** — the highest number of sockets the server held open
  at once (only meaningful with `--live-sockets`).
- **Shed (behind)** — items the scheduler generated but never attempted
  because the in-flight semaphore had no free permit; reported as its own
  count and percentage of offered, not silently folded into "accepted".

crebain never conflates offered load with served load, and never reports a
held-open-socket count as if it were a req/s result.

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

## HTML report

Pass `--report <path>` to write a self-contained HTML report (opens offline, no
network) alongside the text summary:

```
crebain --isolated --database-url "$DATABASE_URL" --duration 60 --report bench.html
```

The report charts requests/sec and success/fail records (cumulative and
per-second) over the run, plus the concurrency fields above (peak in-flight,
peak connections, offered vs accepted, source IPs). In `--isolated` mode it
also charts the ingest server's CPU (in cores) and RSS memory over time,
sampled once per second from `/proc`. In `--dsn` mode there is no server
process to sample, so the CPU/RAM charts are omitted.

## Examples

Portable fix — the default reading, works against any target:

```bash
crebain --isolated --database-url "$DATABASE_URL" \
  --users 50000 --duration 30 --max-inflight 4096
```

Push concurrency via loopback source-IP fan-out (auto-fans across `127.0.0.0/8`,
raises the fd soft limit):

```bash
crebain --isolated --database-url "$DATABASE_URL" \
  --users 1000000 --max-inflight 200000
```

Literal live sockets — a peak-connections capacity demo, localhost-only (may
need root to clear ~500k):

```bash
crebain --isolated --database-url "$DATABASE_URL" \
  --users 200000 --live-sockets --max-inflight 200000
```

UDS transport — no port wall at all:

```bash
crebain --isolated --database-url "$DATABASE_URL" \
  --users 200000 --transport uds --live-sockets --max-inflight 200000
```

## Design

See [`docs/superpowers/specs/2026-07-15-crebain-benchmark-design.md`](../../../docs/superpowers/specs/2026-07-15-crebain-benchmark-design.md)
and [`docs/superpowers/plans/2026-07-16-crebain-1m-concurrency.md`](../../../docs/superpowers/plans/2026-07-16-crebain-1m-concurrency.md).
