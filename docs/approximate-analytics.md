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
- **DAU/WAU/MAU's identified/guest split is a second sketch**
  (`user_activity_daily.hll_identified`): a person counts as identified on a
  day if `event_users.identified_at` was set when that day's sketch was last
  (re)computed — the fold adds people identified at fold time, and the daily
  maintenance recomputes the trailing 30 days from `person_days` so a later
  `identify()` is absorbed within a day. Guests are `all − identified`. Days
  folded before the column existed read as **unknown** (the tile says "split
  building…") until the unattended backfill fills them; they never read as
  "all guests". Total/Active/New use the exact, current flag.

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
  raw queries (exact, slow) until history has been replayed into the rollups.
  `sauron-ingest` does that by itself on start-up (and retries on its daily
  maintenance pass) whenever an app's gate is still closed — resumable, one
  runner per deployment, cheapest gates first, `ROLLUP_AUTO_BACKFILL=0` to
  opt out. `sauron-migrate backfill-rollups` (and the `backfill-*-envs` /
  `backfill-person-days` commands) remain for running it by hand; the two
  are safe to combine. The dashboard's freshness chip reads "Building
  history · n/N days" until the gate opens.
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
- `app_releases` (migration 78) is empty after an upgrade — the dashboard's
  release switcher only lists releases the pipeline has seen since the
  upgrade until `sauron-migrate backfill-releases` runs, then
  `psql -c 'ANALYZE app_releases'`. The catalogue is SEEDED from `error_events`
  and `analytics_events` only (they are the only release-bearing tables with a
  `received_at` to date a release by); full scan of both, hours at 158M rows.
  Safe to re-run, and nothing else depends on it.
  **It also WRITES to the five telemetry tables that store `release`** — `error_events`,
  `analytics_events`, `sessions`, `transactions` and `workflows` (`symbol_artifacts` is
  excluded on purpose: uploads have always trimmed the value) —
  which no other backfill does: before seeding, it rewrites blank and
  whitespace-padded historical `release`
  values — `''` and `'\t'` become NULL, `' 1.4.0 '` becomes `'1.4.0'` — so
  that `?release=1.4.0`, which lowers to a plain column equality, stops
  answering "no" for rows that are on that release. That filter is offered on
  the Sessions and Transactions lists too, so repairing only the two event
  tables would leave those lists silently short. The ingest edge has
  applied the same trim since the release filter shipped, but only
  forward. Both statements are scoped by `WHERE`, and both are still a full
  scan: `release` is not the partition key, so nothing prunes. It rewrites
  those rows **partition by partition** — one statement per child partition,
  each committing on its own — so the locks are per partition rather than one
  lock on every partition of a table held for the whole run. (`workflows` is
  not partitioned; it takes one statement.) **Stop
  `sauron-tier` for the run**: a partition being rewritten cannot be
  `DETACH`ed, and the tier worker strands its dependents forever if its
  `DETACH` times out (see the tier-worker runbook note) — and the partition
  list is **snapshotted per table, just before that table is repaired**, so a
  partition dropped after its snapshot makes the statement naming it fail and
  aborts the whole command. The command is
  re-runnable and resumable — an interrupted (or aborted) run keeps every
  partition it finished, and a second run repairs only what is left. Their row
  counts are printed ("normalised N rows") and are 0 on a re-run.
  `first_seen_at`/`last_seen_at` are `MIN`/`MAX` of `received_at`, the same
  clock the live pipeline stamps — not `occurred_at`.
