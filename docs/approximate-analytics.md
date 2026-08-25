# Approximate analytics — what the ≈ mark means

At production scale the dashboard's aggregate pages are served from **rollup
tables** — small per-day (per-hour for performance) aggregates maintained
continuously by the ingest process — instead of scanning raw events on every
request. That is what keeps a 90-day query fast over billions of rows. The
trade is that a few figure classes become *approximate*, and the UI marks
exactly those with a leading `≈`.

## What is approximate, and by how much

| Figure class | Mechanism | Error bound |
|---|---|---|
| Distinct users (screens' Users column, DAU/WAU/MAU, active-user series, Overview users-in-window) | HyperLogLog sketch, p=12, merged across days/environments | ±~1.6% standard error (near-exact below ~1,000) |
| Latency percentiles (p50/p75/p95/p99, median session duration) | √2 log-bucket histogram, geometrically interpolated | within one bucket ratio (√2) at distribution edges, typically ±~5% in the interior |

**Everything unmarked is exact**: event counts, error counts, session counts,
views, crash counts, issue `times_seen`, list pages, and every drill-down.

## Semantic changes that ride along

- **DAU/WAU/MAU are calendar-day (UTC) windows** — "today", "last 7 days",
  "last 30 days" — not rolling 24 h/7 d/30 d instants.
- **Journeys are day-scoped**: the first ≤10 events per user per UTC day,
  summed over the window and per environment (previously: first N events per
  user counted from the window's start, environments interleaved).
- **Windows match whole buckets**: a range starting mid-day includes that
  whole UTC day (whole hour for performance charts).
- **Sessions pages window by session start day** (previously last-activity).

## Freshness

Rollups fold newly received events continuously; pages show an
**"as of HH:MM:SS"** chip with the fold watermark. The Refresh button forces
an immediate fold and waits for it, so refreshed numbers include everything
received up to a few seconds ago. A daily consistency job compares rollups
against raw counts and rebuilds any drifted day — counters are derived, never
trusted. Days whose raw partitions the cold tier has already dropped from
Postgres are excluded: their Parquet copy is immutable, so a day that was
consistent when exported stays consistent, and recounting the hot store there
would only report false drift.

## Operator notes

- New installs are rollup-served from the first event. Upgrades serve legacy
  raw queries (exact, slow) until `sauron-migrate backfill-rollups` has
  replayed history — run it once, at a time of your choosing.
- `ROLLUP_FOLD_SECS` (60), `ROLLUP_LAG_SECS` (60), `ROLLUP_KICK_LAG_SECS` (2)
  and `ROLLUP_NAME_CAP` (2000) tune the fold task on `sauron-ingest`.
- The tier worker never exports a partition the fold has not fully passed,
  and the consistency job never rebuilds a day the tier has dropped — the two
  boundaries (`rollup_watermarks` and `tiering_state.dropped_thru`) fence each
  other, so enabling tiering cannot un-aggregate history.
- `SESSION_RETENTION_DAYS` (default 0 = keep forever; runtime-tunable from the
  Storage page) drops whole `sessions` day-partitions past the window on the
  daily maintenance pass. Sessions have NO cold copy: past-retention days
  survive only as session-day rollups, so every chart keeps answering but
  per-session drill-down stops at the window. The recompute clamps at the
  recorded boundary (`tiering_state`, `table_name = 'sessions'`), so a late
  stray can never rewrite a dropped day's aggregates. Non-zero values below 7
  days are clamped up.
