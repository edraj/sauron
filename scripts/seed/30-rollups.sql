-- Derived state: everything the ingest worker would have written as a
-- side-effect of the events, rebuilt from the events that actually exist.
--
-- Counters and first/last-seen are DERIVED here rather than invented in
-- `10-dimensions.sql`, so the dimension rows and the event rows cannot
-- disagree. A seeded dataset whose `devices.events_count` says one thing while
-- `analytics_events` says another is worse than no counter at all: every page
-- reading it is then quietly wrong, and nothing fails.
--
-- FULL RECOMPUTE, not the additive `backfill_app` shape. That function
-- aggregates only `occurred_at < cutoff` and ADDS to whatever the live write
-- path already counted, because the two sets are disjoint. Here they are not
-- merely disjoint — the live path counted NOTHING, since these rows were
-- INSERTed straight into the tables and never passed through the worker. So the
-- correct aggregate is over every row for this app, replacing what is there.
-- Running the real backfill instead would miss every seeded row whose
-- `occurred_at` lands after the epoch, and would mark the app done anyway.
--
-- The `*_backfill` marker rows at the end are not bookkeeping: `is_backfilled()`
-- picks the FAST query shape for /device-groups and /persons on their presence
-- alone (repo.rs:8125, repo.rs:19474). Without them the rollup is built and
-- then never read.

\set ON_ERROR_STOP on
SET TIME ZONE 'UTC';
SET work_mem = '256MB';

-- Parallel aggregation is safe since 2026-08-25: the postgres service carries
-- `shm_size: 2gb` in docker-compose.yml. (Under Docker's old default 64 MB
-- /dev/shm this had to be 0 or the aggregate died with "could not resize
-- shared memory segment ... No space left on device".)
SET max_parallel_workers_per_gather = 4;

\set app_id '\'ee1fb653-cadd-4f27-9321-ff10f382a18c\''

-- ---------------------------------------------------------------------------
-- Per-session aggregates, from both event streams.
-- ---------------------------------------------------------------------------
DROP TABLE IF EXISTS seed_agg_sess;
CREATE TABLE seed_agg_sess AS
SELECT session_id,
       sum(ev)  AS events_count,
       sum(er)  AS errors_count,
       sum(unh) AS unhandled_count,
       min(lo)  AS first_at,
       max(hi)  AS last_at
FROM (
  SELECT session_id, count(*) ev, 0::bigint er, 0::bigint unh,
         min(occurred_at) lo, max(occurred_at) hi
  FROM analytics_events WHERE app_id = :app_id::uuid AND session_id IS NOT NULL
  GROUP BY session_id
  UNION ALL
  SELECT session_id, 0, count(*), count(*) FILTER (WHERE handled IS false),
         min(occurred_at), max(occurred_at)
  FROM error_events WHERE app_id = :app_id::uuid AND session_id IS NOT NULL
  GROUP BY session_id
) u
GROUP BY session_id;
CREATE UNIQUE INDEX ON seed_agg_sess (session_id);
ANALYZE seed_agg_sess;

UPDATE sessions s
SET events_count = a.events_count,
    errors_count = a.errors_count,
    unhandled_errors_count = a.unhandled_count,
    started_at = a.first_at,
    last_event_at = a.last_at,
    updated_at = now()
FROM seed_agg_sess a
WHERE s.app_id = :app_id::uuid AND s.session_id = a.session_id;

-- ---------------------------------------------------------------------------
-- Per-user and per-device aggregates.
-- ---------------------------------------------------------------------------
DROP TABLE IF EXISTS seed_agg_user;
CREATE TABLE seed_agg_user AS
SELECT distinct_id, sum(ev) AS events_count, sum(er) AS errors_count,
       min(lo) AS first_at, max(hi) AS last_at
FROM (
  SELECT distinct_id, count(*) ev, 0::bigint er, min(occurred_at) lo, max(occurred_at) hi
  FROM analytics_events WHERE app_id = :app_id::uuid AND distinct_id <> ''
  GROUP BY distinct_id
  UNION ALL
  SELECT distinct_id, 0, count(*), min(occurred_at), max(occurred_at)
  FROM error_events WHERE app_id = :app_id::uuid AND distinct_id IS NOT NULL
  GROUP BY distinct_id
) u
GROUP BY distinct_id;
CREATE UNIQUE INDEX ON seed_agg_user (distinct_id);
ANALYZE seed_agg_user;

UPDATE event_users eu
SET first_seen = a.first_at, last_seen = a.last_at, updated_at = now()
FROM seed_agg_user a
WHERE eu.app_id = :app_id::uuid AND eu.distinct_id = a.distinct_id;

DROP TABLE IF EXISTS seed_agg_dev;
CREATE TABLE seed_agg_dev AS
SELECT device_key, sum(ev) AS events_count, sum(er) AS errors_count,
       min(lo) AS first_at, max(hi) AS last_at
FROM (
  SELECT device_key, count(*) ev, 0::bigint er, min(occurred_at) lo, max(occurred_at) hi
  FROM analytics_events WHERE app_id = :app_id::uuid AND device_key IS NOT NULL
  GROUP BY device_key
  UNION ALL
  SELECT device_key, 0, count(*), min(occurred_at), max(occurred_at)
  FROM error_events WHERE app_id = :app_id::uuid AND device_key IS NOT NULL
  GROUP BY device_key
) u
GROUP BY device_key;
CREATE UNIQUE INDEX ON seed_agg_dev (device_key);
ANALYZE seed_agg_dev;

UPDATE devices d
SET first_seen = a.first_at, last_seen = a.last_at,
    events_count = a.events_count, errors_count = a.errors_count, updated_at = now()
FROM seed_agg_dev a
WHERE d.app_id = :app_id::uuid AND d.device_key = a.device_key;

-- ---------------------------------------------------------------------------
-- Issues. `error_events` has no dedup — one full row per occurrence — so
-- `issues.times_seen` is the ONLY occurrence counter, and `users_seen` the only
-- distinct-user one.
-- ---------------------------------------------------------------------------
UPDATE issues i
SET times_seen = a.n, users_seen = a.u,
    first_seen = a.lo, last_seen = a.hi, last_event_at = a.hi, updated_at = now()
FROM (
  SELECT issue_id, count(*) n, count(DISTINCT distinct_id) u,
         min(occurred_at) lo, max(occurred_at) hi
  FROM error_events WHERE app_id = :app_id::uuid GROUP BY issue_id
) a
WHERE i.id = a.issue_id;

-- ---------------------------------------------------------------------------
-- Environment rollups. `sessions_count` is credited from `sessions`, not from
-- the event streams — a session spans many events and counting it per event
-- would inflate it by roughly the events-per-session factor.
-- ---------------------------------------------------------------------------
DELETE FROM event_user_environments WHERE app_id = :app_id::uuid;
INSERT INTO event_user_environments (
  app_id, distinct_id, environment_id, first_seen, last_seen,
  events_count, errors_count, sessions_count)
SELECT :app_id::uuid, distinct_id, environment_id,
       min(lo), max(hi), sum(ev), sum(er), sum(se)
FROM (
  SELECT distinct_id, environment_id, count(*) ev, 0::bigint er, 0::bigint se,
         min(occurred_at) lo, max(occurred_at) hi
  FROM analytics_events WHERE app_id = :app_id::uuid AND distinct_id <> ''
  GROUP BY 1, 2
  UNION ALL
  SELECT distinct_id, environment_id, 0, count(*), 0, min(occurred_at), max(occurred_at)
  FROM error_events WHERE app_id = :app_id::uuid AND distinct_id IS NOT NULL
  GROUP BY 1, 2
  UNION ALL
  SELECT distinct_id, environment_id, 0, 0, count(*), min(started_at), max(last_event_at)
  FROM sessions WHERE app_id = :app_id::uuid AND distinct_id IS NOT NULL
  GROUP BY 1, 2
) u
GROUP BY distinct_id, environment_id;

DELETE FROM device_environments WHERE app_id = :app_id::uuid;
INSERT INTO device_environments (
  app_id, device_key, environment_id, first_seen, last_seen,
  events_count, errors_count, sessions_count)
SELECT :app_id::uuid, device_key, environment_id,
       min(lo), max(hi), sum(ev), sum(er), sum(se)
FROM (
  SELECT device_key, environment_id, count(*) ev, 0::bigint er, 0::bigint se,
         min(occurred_at) lo, max(occurred_at) hi
  FROM analytics_events WHERE app_id = :app_id::uuid AND device_key IS NOT NULL
  GROUP BY 1, 2
  UNION ALL
  SELECT device_key, environment_id, 0, count(*), 0, min(occurred_at), max(occurred_at)
  FROM error_events WHERE app_id = :app_id::uuid AND device_key IS NOT NULL
  GROUP BY 1, 2
  UNION ALL
  SELECT device_key, environment_id, 0, 0, count(*), min(started_at), max(last_event_at)
  FROM sessions WHERE app_id = :app_id::uuid AND device_key IS NOT NULL
  GROUP BY 1, 2
) u
GROUP BY device_key, environment_id;

-- The gate. Without these two rows the rollups above are never read.
INSERT INTO device_env_backfill (app_id, completed_at)
VALUES (:app_id::uuid, now()) ON CONFLICT (app_id) DO UPDATE SET completed_at = now();
INSERT INTO event_user_env_backfill (app_id, completed_at)
VALUES (:app_id::uuid, now()) ON CONFLICT (app_id) DO UPDATE SET completed_at = now();

DROP TABLE IF EXISTS seed_agg_sess;
DROP TABLE IF EXISTS seed_agg_user;
DROP TABLE IF EXISTS seed_agg_dev;

SELECT
  (SELECT count(*) FROM event_user_environments WHERE app_id = :app_id::uuid) AS person_env_rows,
  (SELECT count(*) FROM device_environments     WHERE app_id = :app_id::uuid) AS device_env_rows,
  (SELECT sum(times_seen) FROM issues WHERE app_id = :app_id::uuid)           AS issue_occurrences,
  (SELECT count(*) FROM device_env_backfill     WHERE app_id = :app_id::uuid) AS device_marker,
  (SELECT count(*) FROM event_user_env_backfill WHERE app_id = :app_id::uuid) AS person_marker;
