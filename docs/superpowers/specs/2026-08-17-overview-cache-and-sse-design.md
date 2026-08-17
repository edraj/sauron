# Overview result cache + SSE delivery

**Status:** built and verified end-to-end, 2026-08-17.

## The problem

The five Overview sections are live aggregates over the partitioned event tables,
so their cost scales with retained data, not with the caller's window. Measured on
the reporting app:

| Section | Latency | Result |
|---|---|---|
| `overview/totals` | **30.07 s** | **503** |
| `analytics/active-users` | 13.38 s | 200 |
| `overview/series` | 7.01 s | 200 |
| `overview/top-events` | 6.11 s | 200 |
| `overview/top-issues` | 5.70 s | 200 |

`sauron-api`'s `TimeoutLayer` maps a 30 s request onto `SERVICE_UNAVAILABLE`, so
the KPI tiles did not render at all — not slowly, not partially, but as an error.

Splitting `/overview` into five sections had already bought MAX-instead-of-SUM.
That lever was exhausted: the slowest section alone was over the limit.

## Decisions taken

1. **Staleness budget: 1 hour**, with the page stating how old the numbers are, and
   an explicit Refresh that forces a recompute.
2. **Scope: the five Overview calls only.** Not `/persons`, `/funnel`,
   `/device-groups` — prove the pattern on one page first.
3. **Cold start: skeleton + push.** No request ever blocks on an aggregate.
4. **One header timestamp**, not per-section.

## Architecture

The aggregate moves OFF the request path. A request does three cheap things —
authorize, read Redis, maybe enqueue — so it cannot reach the timeout regardless
of how slow the underlying query is.

```
GET /overview/totals ──▶ authorize ──▶ Redis GET ──┬─ fresh  ─▶ 200 {state:"fresh", data}
                                                   ├─ stale  ─▶ 200 {state:"stale", data} ──┐
                                                   └─ miss   ─▶ 200 {state:"computing"}   ──┤ enqueue
GET /overview/stream ─▶ subscribe ──────────────────── SSE ◀── recompute worker ◀──────────┘
```

### Components

- **`backend/bins/sauron-api/src/overview_cache.rs`** — Redis result cache,
  single-flight, bounded recompute worker, broadcast bus.
- **`GET /v1/apps/{id}/overview/stream`** — SSE, one `section` event per section:
  a snapshot on connect, then live pushes.
- **`POST /v1/apps/{id}/overview/refresh`** — 202; enqueues all five ignoring
  freshness.
- **`dashboard/src/lib/api/overview-stream.ts`** — `fetch()`-based SSE reader.

### Response envelope

All five section endpoints now return `{state, computed_at, data, error?}`.
`data` is nullable because `computing` is a normal 200.

## The four decisions that carry the design

**Redis TTL 24 h, freshness threshold 1 h — two different numbers.** If the entry
also *expired* at an hour, the first request past the hour finds nothing and is
back to a 30 s skeleton. The entry survives 24 h; the 1 h mark only decides
whether a recompute is *also* kicked off while the old value is served.

**Single-flight is load-bearing, not an optimization.** The pool is 16 connections.
Without dedup, five viewers of one dashboard is five concurrent 30 s aggregates,
and `/v1/auth/login` starts failing on pool checkout. Before this, a slow query was
bounded by the client giving up; a detached task has no such backstop. Hence a
per-key in-flight set *and* a 3-permit global semaphore.

**The cache key uses `since_days`, never the derived `since`.** `since_of()`
computed `Utc::now() - days` — a different value every request. Keying on that
mints a fresh entry per request and hits 0% while compiling, passing every test,
and showing a plausible `computed_at`. Guarded by a test, not a comment.

**SSE is read with `fetch()`, not `EventSource`.** `EventSource` cannot set
`Authorization`, and the dashboard is bearer-token. A token in the query string
writes a live JWT into every access log; cookie auth would open a CSRF surface.

## Verification (all against a real DB + Redis, not mocks)

| Check | Result |
|---|---|
| `overview/totals` cold read | **200 in 2.9 ms** (was 503 at 30.07 s) |
| Background recompute | lands with real data: 212,399 events / 210,138 errors |
| SSE stream | snapshot of 5 sections on connect, then live pushes |
| Cache keys | `all:30`, `all:7`, `one:<env>:30` distinct; exactly one `all:30` key across many requests |
| Single-flight | 12 concurrent cold requests → **1** database query |
| Cold-start UI | flushed Redis → page fills in, header shows "just now" |
| Backend | fmt clean, clippy 0, **394 tests passed / 0 failed** |
| Frontend | svelte-check 0 errors, **964 tests passed** |
| Mutation tests | SSE buffer carry-over and envelope unwrap both confirmed failing when broken |

## What this does NOT do

**It does not make the queries faster.** A 30 s totals query is still 30 s; it just
runs in the background. What changed is that nothing waits on it.

The next lever, if the 1 h staleness ever stops being enough, is pre-aggregation:
hourly/daily rollup counters maintained by the ingest worker would turn totals into
a ~30-row read. This design makes that a safe, independent follow-up — the UI
contract does not change when the compute behind it gets cheaper.

## Follow-ups deliberately not taken

- Rollup tables (above).
- Extending the pattern to `/persons`, `/funnel`, `/device-groups`.
- SSE reconnection with backoff. The server sends a full snapshot on connect, so a
  dropped stream is recovered by opening a new one; a retry loop inside the client
  would have to duplicate that decision without knowing if the page is mounted.
- Runtime-tunable freshness/permit counts. Currently constants.
