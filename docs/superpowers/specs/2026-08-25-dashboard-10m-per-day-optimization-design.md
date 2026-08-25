# Dashboard at 10M records/day — full optimization strategy

**Date:** 2026-08-25 · **Status:** awaiting review · **Prereq reading:**
[2026-08-24 latency baseline](../../benchmarks/2026-08-24-dashboard-latency-baseline.html),
[overview cache + SSE spec](2026-08-17-overview-cache-and-sse-design.md)

## 1. Goal

Every dashboard page paints instantly and fills with data in well under a second
(p95, server time) at a sustained ingest of **~10M records/day** from many
concurrent clients — **without degrading the write path**. The strategy must
keep working as the window grows: query cost must be proportional to the size of
the *answer*, never to the number of events.

Non-goals: an external OLAP store (ClickHouse et al.); SDK wire-format changes;
exactness where a disclosed approximation serves (see §4.2).

## 2. Measured reality — why this is architecture, not query tuning

The 2026-08-24 baseline measured all 52 dashboard endpoints at 212K and again at
10M rows. 10M **total** is ~110K/day equivalent; the production target of
10M/day means a 90-day window of **~900M rows** — 90× denser. Extrapolating the
measured numbers (at best linear in rows scanned; worse once the working set
exceeds cache):

| Endpoint | @212K | @10M measured | @900M est. |
|---|---|---|---|
| `/screens` | 1,552 ms | 24,581 ms | **503** (~30 min of work) |
| `/users/summary` | 645 ms | 7,413 ms | **503** |
| `/journeys` | 437 ms | 7,226 ms | **503** |
| `/performance/summary` | 94 ms | 2,768 ms | **503** |
| `/sessions/summary` | 2.9 ms | 409 ms | **503** (~40 s) |
| keyset lists (`/events/list`, `/issues`, `/transactions`) | ~10–20 ms | ~12 ms | **~12 ms** |
| rollup-backed (`/device-groups`, `/persons`) | ~2–5 ms | 100–212 ms | **~flat** (scales with users, not events) |

The last two rows are the strategy, already proven three times in this codebase:

- **Keyset + hard LIMIT** did not degrade at all between 212K and 10M.
- **Pre-aggregated rollups** (`device_environments`, `event_user_environments`)
  took `/device-groups` from 3,970 ms → 126 ms and held near-flat at 10M.
- **Off-request-path compute** (Overview cache + SSE) serves 2.9 ms cold where
  the same aggregate 503'd at 30 s.

## 3. The rule

> **No request-path query may aggregate raw event rows.** Aggregates read
> pre-computed rollup tables sized to the answer. Raw rows are touched only by
> keyset-paginated lists and single-record drill-downs, bounded to the hot
> window. The two deliberate exceptions — funnels and pro-search, whose queries
> are user-composed — stay raw but hot-window-bounded (§5.4).

## 4. Locked decisions (user, 2026-08-25)

1. **Freshness:** aggregate pages may lag **~1 minute** behind live (continuous
   background aggregator). Every rollup-backed page gets a **Refresh** action
   that forces an immediate fold and returns the newest data.
2. **Exactness:** approximate statistics are accepted — distinct-user counts via
   sketches (±~2%), latency percentiles via log-scale histograms — **and must be
   disclosed** in the dashboard UI and in the docs (§5.5). Values that are
   already exact counters (`issues.times_seen`, list counts, event counts) stay
   exact.
3. **Hot window:** stays a deployment knob (`TIER_HOT_DAYS`); production sizing
   in §5.6. Locally, validate with a **light-payload seed** at production
   *density* (§6) rather than committing this dev box to one number.
4. **Infra:** applied 2026-08-25 — postgres container now runs
   `shm_size: 2gb`, `shared_buffers=8GB`, `effective_cache_size=24GB`,
   `work_mem=32MB`, `maintenance_work_mem=1GB`, `max_wal_size=4GB`
   (image defaults were 128MB/4GB/4MB/64MB/1GB; `/dev/shm` was Docker's 64 MB,
   which killed parallel query and parallel vacuum at 10M rows). Post-tuning
   measurements: §7 phase 0.

## 5. Architecture

### 5.1 Rollup tables

All follow the house pattern set by `device_environments`: plain (unpartitioned)
tables, unique key over `(app_id, …, COALESCE(environment_id, zero-uuid))`,
bigint counters, `first/last`-style bounds where useful. All are small forever —
their size is bounded by (screens × envs × days), never by event volume. Day
buckets are UTC dates of `occurred_at`.

| Table | Key | Payload | Serves |
|---|---|---|---|
| `screen_stats_daily` | app, env, day, screen | views, sessions, entries/exits, `users_hll`, `load_ms_hist` | `/screens`, `/counts/screens`, screen detail cards |
| `screen_transitions_daily` | app, env, day, from_screen, to_screen | transitions | `/journeys` |
| `perf_op_daily` | app, env, day, name, op | count, error_count, `duration_ms_hist`, `users_hll` | `/performance/summary`, `/performance/series`, transaction drill-down aggregates |
| `session_stats_daily` | app, env, day | sessions, crashed, unhandled, events_sum, `duration_hist` | `/sessions/summary`, Overview crash-free |
| `user_activity_daily` | app, env, day | `active_users_hll`, new_users, events, errors | `/users/summary`, `/analytics/active-users`, project active-users, DAU/WAU/MAU |
| `event_top_daily` | app, env, day, name | count, `users_hll` | `/events/top`, Overview top-events |

Sizing at 10M/day, 100s of screens, a handful of envs: worst table
(`perf_op_daily`, `event_top_daily`) is a few thousand rows/day — a 90-day query
touches ≤ a few hundred thousand small rows via its unique index, typically far
fewer. **Cardinality guard:** event/transaction names are developer-supplied and
unbounded in principle; the fold caps distinct names per (app, day) at a
configurable N (default 2,000) and folds the tail into a literal `~other`
bucket, logging when the cap engages.

Issues need no new table — `issues.times_seen`/`users_seen` are already
maintained counters and `/overview/top-issues` measured 54 ms at 10M.

### 5.2 The aggregator

A background task in the **ingest worker process** (single stateful consumer;
putting it in `sauron-api` would duplicate work when the API scales out).

- **Cycle:** every 60 s (and on demand, §5.5). For each of the three firehose
  tables: fold rows in `received_at ∈ (watermark, now() − LAG]`, grouped by
  bucket keys, and **upsert-add** into the rollup tables; then advance the
  per-table watermark (stored in a `rollup_watermarks` table). All three
  tables carry `received_at timestamptz DEFAULT now()` already.
- **Why arrival time, not event time:** late events (old `occurred_at`, new
  `received_at`) are still folded exactly once, into their correct historical
  bucket. This makes rollups *more* correct on late data than raw partition
  scans, which lose late rows to `_default` after their partition is dropped.
- **`LAG` = 60 s** guards against in-flight ingest transactions whose
  `now()`-stamped rows commit after the fold passes (ingest batch commits are
  sub-second; 60× margin). Belt-and-braces: a **daily consistency check** (the
  `50-verify.sql` pattern) compares yesterday's rollup sums against
  `count(*)` on yesterday's partition and flags drift; repair is a per-bucket
  **rebuild command** that recomputes one day from raw and REPLACEs (scan of one
  partition, idempotent). Counters are derived, never trusted — house rule.
- **Index:** one **BRIN** index on `received_at` per firehose parent
  (cascades to partitions). BRIN on an insert-ordered column is ~KB-sized with
  negligible maintenance — against the 11 btrees each of these tables already
  carries, it is noise, and it is what makes the per-minute fold a
  few-thousand-row range read instead of a partition scan.
- **Backfill:** per-app one-shot that replays history per-day into the rollups
  (the `30-rollups.sql` shape), then writes a marker row. Endpoints gate on the
  marker exactly like `is_backfilled()` does today, keeping the old query as
  fallback until backfill completes.
- **Crash safety:** watermark advances in the same transaction as the fold's
  upserts — a crash re-folds nothing and loses nothing.

### 5.3 Sketches (approximate, no extensions)

Stock `postgres:16` stays (RPM deployments share the schema; no `hll`/
`datasketches` extension dependency):

- **Distinct users:** HyperLogLog computed in Rust by the aggregator, stored as
  `bytea` per rollup row (sparse encoding for small sets; dense ≤ ~4 KB at
  p=12, ±~1.6%). Multi-day/multi-env queries read ≤ a few hundred sketches and
  merge in the API — microseconds. WAU/MAU are merges of daily sketches.
- **Percentiles:** fixed log-scale bucket histogram (`int8[]`, ~55 buckets
  covering 1 ms–60 s, ratio √2 → ±~3.5% value error), element-wise added across
  rows; percentile derived at read time. Mergeable across days, envs, **and
  tiers** — this removes the existing "transactions percentiles are hot-only,
  never merged across tiers" limitation.

### 5.4 Endpoint rewrites

Each rewrite is gated on the backfill marker (fallback = today's query):

| Endpoint | Today @10M | After | Source |
|---|---|---|---|
| `/screens`, `/counts/screens` | 24,581 ms | ~ms | `screen_stats_daily` |
| `/journeys` | 7,226 ms | ~ms | `screen_transitions_daily` |
| `/users/summary`, `/analytics/active-users`, project active-users | 7,413 / 1,987 ms | ~ms | `user_activity_daily` (+ exact `new_users` from `event_users.first_seen`) |
| `/performance/summary`, `/performance/series` | 2,768 / 1,022 ms | ~ms | `perf_op_daily` |
| `/sessions/summary` | 409 ms | ~ms | `session_stats_daily` |
| `/events/top`, Overview top-events | 377 ms | ~ms | `event_top_daily` |
| Overview totals/series | cached, 1h stale | cached, ≤1 min stale | cache reads rollups instead of raw — the 30 s aggregate becomes ms, so the refresher can run every minute; **envelope/SSE contract unchanged** |
| Lists: events, sessions, transactions, issues, persons, devices | 2–25 ms | unchanged | keyset + LIMIT; bounded by hot window; cold via tier router |
| Funnels, pro-search | window-bounded raw | unchanged, hot-window-bounded | documented limitation; async-compute envelope is the follow-up **if** usage demands it |
| Screen detail cards | fetch-on-demand | aggregates from `screen_stats_daily`; sample lists stay keyset raw | |

No SSE expansion beyond Overview: once reads are O(answer), an envelope adds
contract complexity for nothing (YAGNI). Overview keeps its existing envelope.

### 5.5 Delivery, freshness & disclosure (dashboard)

- Every rollup-backed response carries `as_of` (the watermark) and
  `approx: true` on sketch-derived fields.
- Rollup-backed pages show an **"as of HH:MM:SS" chip + Refresh button**.
  Refresh calls `POST /v1/apps/{app}/rollups/refresh` → wakes the aggregator
  (Redis signal), waits bounded (≤5 s) for the watermark to pass the request
  time, then the page re-fetches. Single-flight per app + rate-limited.
  ⚠ Any new mutating route trips `audit_coverage.rs` — audit it or EXEMPT with
  a real reason.
- Approximate figures render with a leading `≈` and a shared tooltip
  ("Approximate (±~2%), computed from sketches — exact at small scale, see
  docs"), plus a docs/wiki page. **All new strings in en + ar** (house i18n
  rule), and the tooltip must be reachable by keyboard (no `title` on
  `disabled` — `lockTip` precedent).

### 5.6 Storage: hot window & tiering

Measured **~1.9 GB per million rows** (rich payloads; real traffic with pooled
stacktraces trends smaller) → ~19 GB/day at 10M/day:

| `TIER_HOT_DAYS` | PG size @10M/day | Fits this box today (415 GB free)? |
|---|---|---|
| 7 | ~135 GB | yes |
| 14 | ~270 GB | yes, with headroom |
| 30 | ~570 GB | **no** — prod disk decision |

- Rollups make the hot window a **drill-down-only** concern: every aggregate
  page works over any window from rollups; only raw lists narrow to hot + cold
  router.
- `sauron-tier` stays down until rollup backfill completes; the tier worker
  gains one interlock — never export a partition whose day the aggregator
  watermark has not fully passed (trivially true at 1-min lag vs day-old
  exports, but enforced, not assumed).
- Cold Parquet accumulates ~2–6 GB/day compressed; it moves, never deletes —
  cold-retention policy is a deliberate non-decision until disk pressure says
  otherwise.
- **Known gap:** `sessions` is not partitioned; at production density it grows
  ~600K rows/day (~56M/90d). Lists stay keyset-fast, `session_stats_daily`
  carries the aggregates, but the table needs partitioning + tiering of its own
  as a follow-up migration (§7 phase 6).

### 5.7 Write-path guarantees (the "multiple clients" constraint)

| | items/s |
|---|---|
| Needed: 10M/day average | **116** |
| Needed: diurnal peak (~4×) | **~500** |
| Measured edge accept (single instance) | ~8,000+ |
| Measured worker persist ceiling (post Tier-3, 8 workers / batch 1000) | **~19,000–27,600** |

40–230× headroom. By construction, this strategy adds to the write side only:

1. **Nothing in the ingest transaction.** The aggregator reads committed rows
   behind a watermark; ingest never touches rollup tables, so there is zero
   lock overlap even with N workers × M clients.
2. **One BRIN per firehose table** (vs 11 existing btrees each) — negligible.
3. **No triggers.**

And it *removes* the two real write-side threats at scale: multi-second
dashboard aggregates competing with ingest for buffers/IO/CPU, and Redis
consumer stalls caused by a starved Postgres. Follow-up dividend: once rollups
serve the aggregate shapes, several of the 11 event-table btrees lose their
only reader — auditing and dropping them is a direct write-throughput gain.
Still-open ingest item folded into this programme: the **XPENDING-based backlog
metric + alert** (the multi-client safety valve; silent-loss history demands
it).

## 6. Local validation plan (light payloads — locked decision #3)

Density is what matters, not window length. A 90-day × 110K/day seed does not
exercise 10M/day; a **14-day × 10M/day** seed does, at the same row count.

- **Phase A (now-scale):** run backfill on the existing 10M set → re-measure all
  52 endpoints. Expect every rollup-backed page at low ms.
- **Phase B (production density):** extend `scripts/seed` with a
  `light` payload mode (~0.2–0.3 KB/row vs ~1.9 KB — minimal contexts/extra,
  2-frame stacks) → **140M rows / 14 days ≈ 40–60 GB**, fits easily. Re-measure.
  **Acceptance: p95 server time < 500 ms on every page, zero 503s.**
- **Phase C (writes + reads together):** crebain drives ≥1,000 items/s
  (2× peak) against the stack while `measure.py` loops — dashboard p95 must hold
  and `XPENDING` must stay bounded. This is the direct proof of "smooth
  dashboard while multiple clients write".

## 7. Rollout phases (each independently shippable)

0. ✅ **Infra** (2026-08-25): shm + memory settings — done, and re-measured
   across all 52 endpoints. **Read latency moved 1.0–1.2× only** (`/screens`
   24.6 s → 23.2 s): the slow endpoints are algorithmically bound, not
   memory-starved — the host's OS page cache was already absorbing their I/O.
   This directly validates §2: no amount of tuning fixes an O(events) query.
   The settings still buy what they were for — working parallel query/vacuum
   (shm), fewer checkpoints under sustained ingest (`max_wal_size`), and
   real buffer headroom once ingest and dashboards compete.
1. **Migrations:** 6 rollup tables, `rollup_watermarks`, backfill marker, BRIN
   on `received_at` ×3.
2. **Aggregator:** fold task in ingest worker + backfill command + daily
   consistency check + rebuild command.
3. **Worst-page rewrites:** `/screens`, `/journeys` behind markers.
4. **Remaining rewrites:** performance, users/active-users, sessions/summary,
   events/top; Overview cache reads rollups (refresher to 1-min cadence).
5. **Dashboard:** as-of chip + Refresh, `≈` disclosure + tooltip + docs, en+ar.
6. **Tiering on** with interlock; `sessions` partitioning follow-up.
7. **Validation:** bench phases A→C; then the index audit.

## 8. Risks

- **Aggregator drift** (bug or missed fold) — daily consistency check + cheap
  per-day rebuild; watermark+fold are one transaction.
- **Sketch size** — sparse HLL encoding; cap precision; histograms are int8[55].
- **Name cardinality explosion** — per-day cap + `~other` bucket + log line.
- **Refresh stampede** — single-flight + per-app rate limit (Overview refresh
  precedent).
- **`sessions` growth** — explicit follow-up (phase 6), not silent.
- **Test traps** (house history): DB suites that print ok in 0.00 s having run
  nothing; envelope tests must poll; `audit_coverage` on the new POST route;
  i18n leak test blindness. Each named in the implementation plan.

## 9. Acceptance criteria

1. Phase B bench: **every page < 500 ms p95 server time at 140M rows /
   production density, zero 503s.**
2. Phase C bench: same, concurrent with ≥1,000 items/s sustained ingest,
   `XPENDING` bounded, accepted-vs-persisted loss 0%.
3. Rollup values match exact recompute within disclosed error on a sampled
   window (consistency check green).
4. crebain 3v3 before/after: write throughput unchanged within noise.
5. Every approximate figure in the UI carries the `≈` + tooltip; docs page
   exists; en + ar.
