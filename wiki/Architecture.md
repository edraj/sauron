# Architecture — how it works under the hood

This page explains what happens **after** an SDK sends a signal: how the backend
ingests it, groups and enriches it, computes the numbers the dashboard shows, ages the
data out, and guards access. It complements the **[Ingest Wire Contract](Ingest-Wire-Contract.md)**
(what SDKs emit) and the **[Dashboard](Dashboard.md)** (what you see).

See also: **[Home](Home.md)** · **[Ingest Wire Contract](Ingest-Wire-Contract.md)** ·
**[Capabilities](Capabilities.md)**.

Everything an SDK sends — errors, events, identifies, transactions, breadcrumbs —
travels the same path and lands on **one timeline**, keyed to your app.

---

## 1. The ingest pipeline

```
SDK batch ──▶ Ingest edge ──▶ Redis stream ──▶ Workers ──▶ Postgres
             (auth + split)   (XADD, capped)   (consume     (partitioned
                                                + ACK)        by app_id)
```

An SDK batches items into an **envelope** and `POST`s it to
`/api/{project_id}/envelope` with the public key in the `X-Sauron-Key` header (or the
`?k=` query param, for `navigator.sendBeacon`, which can't set headers).

**At the edge** (a small, stateless service):

- **Auth & tenancy.** The public key is resolved to an app — cached in Redis, falling
  back to Postgres — yielding its project and org. The `{project_id}` in the URL is
  **ignored**; tenancy comes from the key. An unknown key is `401`; an app with ingest
  disabled is `403`.
- **Rate limiting.** A per-app fixed-window limit (Redis `INCR` + `EXPIRE`) returns
  `429` with `Retry-After` when exceeded.
- **Decompression.** `Content-Encoding: gzip` is decompressed transparently; the body
  size is capped.
- **Structural validation only.** The envelope must deserialize (else `400`). No
  semantic work happens here.
- **Enqueue & ack fast.** The edge emits **one job per envelope item** onto a Redis
  **stream** (`XADD`, length-capped) and immediately answers `202 Accepted` with the
  count. Your app never blocks on processing.

**The workers** run as a consumer group on that stream (co-located in the same
process). Each worker reads a batch, processes each job, and **acknowledges** it on
success; a job that fails or can't be parsed is moved to a **dead-letter queue** rather
than blocking the stream (at-least-once delivery). Per item type:

| Item | What the worker does |
| --- | --- |
| `error` | Fingerprint → upsert the issue → symbolicate → insert the event → roll up session/device → bump the affected-user sketch |
| `event` | Insert the analytics event → roll up session/device |
| `identify` | Upsert the person's traits; alias an anonymous id onto a known one when present |
| `transaction` | Insert the transaction → freshen the session/device window |
| `breadcrumb_batch` | Push breadcrumbs into a short-lived Redis list (not a table) for attaching to a later crash |

All rows are written **keyed by `app_id`** into time-partitioned tables
(`error_events`, `analytics_events`, `transactions`, range-partitioned by
`occurred_at`). That shared key is what lets an error and an event for the same person
sit on one timeline.

## 2. Error grouping & symbolication

A raw exception collapses into a grouped **Issue** by a stable **fingerprint** — a
SHA-256 computed with the first rule that applies:

1. **Your override.** If the SDK sends a `fingerprint[]`, it is hashed verbatim — you
   control the grouping.
2. **Stack frames.** Otherwise: the exception type plus up to **five** frames (in-app
   first, crash last), each reduced to `module::function`. Line numbers, `0x…`
   addresses, UUIDs, and content-hashed filenames (`app.4f3a2b.js` → `app.js`) are
   masked, so the same bug groups across builds and machines.
3. **Message.** No usable frames falls back to the type plus a normalized message; no
   exception at all hashes just the message.

The issue is an upsert keyed on `(app_id, fingerprint)`: a repeat occurrence bumps
`times_seen` and refreshes `last_seen`, level, title, and culprit. **Affected-user**
counts use a HyperLogLog sketch per issue, so they stay cheap at any volume.

**Symbolication** makes minified / ahead-of-time traces readable server-side:

- **JavaScript** — Source Map v3. Requires a `release`; resolves each `(line, column)`
  back to the original source, line, column, name, and a few lines of surrounding
  context.
- **Dart** — DWARF via `addr2line`. Parses the AOT trace's build id and load base,
  resolves each program-counter address, and expands inline frames.

It runs at **ingest** when the matching symbol artifacts are already uploaded
(time-boxed and non-fatal); a miss or timeout leaves the trace to be symbolicated **on
read** instead.

## 3. Product analytics & people

`track()` writes events; `identify()` writes people (and can alias an anonymous id onto
a known one). **Sessions** and **devices** are materialized roll-ups, upserted on every
signal:

- **Session** — keyed on `(app_id, session_id)`. Its span grows to `[first seen, last
  seen]` (`started_at` shrinks to the min, `last_event_at` grows to the max) with
  running event and error counts.
- **Device** — keyed on a stable **`device_key`**: your SDK's persistent install id
  when present, otherwise a `family|model|os|arch|browser` descriptor so web clients
  with no install id still cluster. OS/browser fall back to User-Agent parsing when the
  structured context is absent.

**Breadcrumbs** don't become rows. They ride ahead of a crash in a capped, expiring
Redis list per `(app_id, distinct_id)` and get attached to that person's next error.

## 4. The queries behind the screens

The harder numbers are computed **on read**, in SQL — there is no pre-aggregation
service.

### Funnels — distinct people, in order

One CTE per step; each step is matched **per person** and only at-or-after the previous
step's time:

```sql
-- one CTE per step; each must happen at-or-after the previous, per person
s0 AS (SELECT distinct_id, min(occurred_at) AS t
       FROM analytics_events
       WHERE app_id = $1 AND name = 'signup_started'
       GROUP BY distinct_id),
s1 AS (SELECT a.distinct_id, min(a.occurred_at) AS t
       FROM analytics_events a
       JOIN s0 ON s0.distinct_id = a.distinct_id
       WHERE a.name = 'signup_completed' AND a.occurred_at >= s0.t
       GROUP BY a.distinct_id)
-- a step's count = the number of distinct people in its CTE
```

Conversion and step drop-off come from the per-step counts.

### Screen dwell — gap to the next event

Time on a screen is the gap to the next event in that session, capped at 30 minutes:

```sql
SELECT screen, sum(LEAST(raw_ms, 1800000)) AS total_dwell_ms
FROM (
  SELECT screen, 1000 * EXTRACT(EPOCH FROM (
    LEAD(occurred_at) OVER (PARTITION BY session_id ORDER BY occurred_at)
      - occurred_at
  )) AS raw_ms
  FROM analytics_events
  WHERE session_id IS NOT NULL AND screen IS NOT NULL
) g
WHERE raw_ms IS NOT NULL AND raw_ms > 0   -- a session's last event has no "next"
GROUP BY screen
```

The raw gap is computed in an inner subquery and the last event of each session
(no successor → `raw_ms` is `NULL`) is filtered out **before** the cap — otherwise
`LEAST(NULL, 1800000)` would hand every session's last screen a bogus 30-minute dwell.

### Performance — interpolated percentiles

```sql
SELECT name, op,
  percentile_cont(0.50) WITHIN GROUP (ORDER BY duration_ms) AS p50,
  percentile_cont(0.95) WITHIN GROUP (ORDER BY duration_ms) AS p95,
  count(*) FILTER (WHERE status = 'error' OR http_status >= 500)::float8
    / count(*) AS error_rate
FROM transactions
WHERE app_id = $1 AND op = 'http'
GROUP BY name, op
```

`percentile_cont` interpolates smooth p50/p75/p95/p99 over `duration_ms`, grouped by
route and `op` (`navigation | http | resource | screen_load | custom`).

### Journeys, and the user metrics

- **Journeys** number each person's events into steps
  (`row_number() OVER (PARTITION BY distinct_id ORDER BY occurred_at)`) and count
  step→step transitions into a Sankey (nodes per `(step, event)`, links per
  `(step, from, to)`).
- **DAU / WAU / MAU** are rolling 1 / 7 / 30-day counts of distinct active people;
  **stickiness** is DAU ÷ MAU. **Session duration** is `last_event_at − started_at`.

## 5. Data lifecycle — hot Postgres, cold Parquet

Signals stay **hot** in Postgres for ~30 days, then age into columnar **Parquet** —
and reads span both tiers transparently.

An hourly job walks each partitioned table oldest-first:

1. **Export** whole partitions older than the hot window to Parquet via DuckDB (laid
   out by `app_id / year / month`).
2. **Verify** the exported row count matches Postgres; on mismatch it stops and retries
   next cycle (never a partial or duplicated export).
3. **Advance a watermark** (monotonic — it never moves backward).
4. **Drop** the Postgres partition — but only after a grace lag **and** a re-count
   guard: if the live partition grew from late-arriving rows, it is kept, never
   dropped. Rows that arrive after a drop land in a `_default` partition instead.

On **read**, a query's time window is split at the watermark. The **hot** half (live
partitions, plus late arrivals in `_default`) and the **cold** half (Parquet, via
DuckDB) run **concurrently**, and their per-day partials are summed — so a day that
straddles the boundary is counted once. Holistic metrics like percentiles stay
hot-only.

## 6. Uptime monitoring

Active HTTP/TCP probes on a fixed schedule — one of 14 interval presets (1 second to
24 hours), each with a timeout and failure/recovery thresholds.

- **Scheduling.** A prober claims due monitors with a single atomic
  `UPDATE … FOR UPDATE SKIP LOCKED` that advances the next-check time **before**
  probing — so multiple probers never double-fire and a slow check can't stack. Claimed
  monitors probe concurrently, bounded by a semaphore.
- **A check** records up/down, status code, and response time. The status code is
  matched against the expected set (default `200–399`) plus an optional body assertion.
- **State machine.** Consecutive-failure and -success thresholds debounce flapping; a
  transition opens or resolves an incident and fires a webhook (with retries).
- **SSRF guard.** Every target **and** webhook URL is checked: loopback, private,
  link-local, CGNAT, and cloud-metadata (`169.254.169.254`) addresses are refused,
  redirects aren't followed, and response bodies are capped at 1 MiB.

## 7. Access control (RBAC)

Fine-grained, and enforced per request:

- **21 atomic permissions** (`issue:read`, `funnel:write`, `source:read`, `monitor:write`,
  `member:manage`, `org:manage`, …) bundle into **roles** (a named permission set),
  which are **granted** at a scope — org, project, or app.
- **Resolution** is the **union** of every grant that applies, cascading **down** Org →
  Project → App: an org grant covers everything beneath it; a project grant covers its
  apps but **not** its sibling projects; an app grant covers only that app.
- **Presets** form a strict ladder — **Owner** (all 21) ⊇ **Admin** (all but
  `org:manage`) ⊇ **Developer** (read/write issues, events, funnels, artifacts, source
  maps, monitors; create/update apps) ⊇ **Viewer** (read-only). Presets are re-synced
  from code at startup, so they stay current as permissions are added.
- **No self-escalation.** You can't grant a role — or mint a custom one — with
  permissions you don't already hold at that scope.

## 8. SDK internals

What every SDK does between your call and the wire. Calls accumulate into one
**envelope**: a header (SDK, release, environment), a context block (device, os, app,
runtime, user), and a list of typed items.

- **Batching** — signals buffer and flush every 5 seconds, or as soon as 30
  accumulate, whichever comes first.
- **Compression** — payloads over 1 KiB are gzipped (the edge decompresses).
- **Delivery** — transient failures (`429`, `5xx`, network) retry with exponential
  backoff and honor `Retry-After`; `4xx` are dropped. A byte-bounded queue rides out
  short outages, with opt-in disk persistence across restarts.
- **Scope** — a process-wide scope plus an isolated per-request scope
  (`AsyncLocalStorage` / `contextvars` / `AsyncLocal`) so one request's user, tags, and
  breadcrumbs never leak into another.

Full per-language detail is on the SDK pages (**[Browser](Browser-SDK.md)** ·
**[Flutter](Flutter-SDK.md)** · **[Node](Node-SDK.md)** · **[Python](Python-SDK.md)** ·
**[C#](CSharp-SDK.md)**), and the exact JSON is in the
**[Ingest Wire Contract](Ingest-Wire-Contract.md)**.

---

*Documents the shipped MVP. Session replay/video, ClickHouse/Kafka/object storage, SSO,
and billing are intentionally out of scope for this cut.*
