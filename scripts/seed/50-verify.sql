-- Evidence that the seed is what it claims to be.
--
-- Every check prints an actual value and its expectation. The ones marked MUST
-- BE 0 are the ones that fail silently in production if they are wrong: rows in
-- a DEFAULT partition still read back fine, and an event whose session says it
-- belongs to a different user still renders — it just makes every rollup built
-- on top of it quietly false.

\set ON_ERROR_STOP on
SET TIME ZONE 'UTC';
\set app_id '\'ee1fb653-cadd-4f27-9321-ff10f382a18c\''
\pset format aligned

\echo '=== Volumes ==='
SELECT
  (SELECT count(*) FROM analytics_events WHERE session_id ~ '^seed_s_') AS analytics,
  (SELECT count(*) FROM error_events     WHERE session_id ~ '^seed_s_') AS errors,
  (SELECT count(*) FROM transactions     WHERE session_id ~ '^seed_s_') AS transactions,
  (SELECT count(*) FROM sessions         WHERE session_id ~ '^seed_s_') AS sessions;

\echo '=== The 80/20 split (expect 80.0 / 20.0) ==='
SELECT
  round(100.0 * a / (a + e), 1) AS pct_analytics,
  round(100.0 * e / (a + e), 1) AS pct_errors,
  a + e AS total_events
FROM (SELECT
  (SELECT count(*) FROM analytics_events WHERE session_id ~ '^seed_s_')::numeric a,
  (SELECT count(*) FROM error_events     WHERE session_id ~ '^seed_s_')::numeric e) t;

\echo '=== Cardinality (expect users=50000, devices=50000, models=100, issues=300) ==='
SELECT
  (SELECT count(*) FROM event_users WHERE app_id = :app_id::uuid AND distinct_id ~ '^seed_u_[0-9]{6}$') AS users,
  (SELECT count(*) FROM devices     WHERE app_id = :app_id::uuid AND device_key  ~ '^seed_d_[0-9]{6}$') AS devices,
  (SELECT count(DISTINCT model) FROM devices WHERE app_id = :app_id::uuid AND device_key ~ '^seed_d_[0-9]{6}$') AS models,
  (SELECT count(*) FROM issues      WHERE app_id = :app_id::uuid AND fingerprint ~ '^seed_fp_[0-9]{3}$') AS issues;

\echo '=== Time spread (expect 90 days, 2026-05-27 .. 2026-08-24) ==='
SELECT min(occurred_at)::date AS first_day,
       max(occurred_at)::date AS last_day,
       count(DISTINCT occurred_at::date) AS distinct_days
FROM analytics_events WHERE session_id ~ '^seed_s_';

\echo '=== MUST BE 0: rows in a DEFAULT partition ==='
\echo '(a non-zero here means timestamps escaped their partition and pruning is dead)'
SELECT (SELECT count(*) FROM analytics_events_default) AS analytics_default,
       (SELECT count(*) FROM error_events_default)     AS error_default,
       (SELECT count(*) FROM transactions_default)     AS transactions_default;

\echo '=== MUST BE 0: events disagreeing with their own session ==='
SELECT count(*) AS incoherent_events
FROM analytics_events e
JOIN sessions s ON s.app_id = e.app_id AND s.session_id = e.session_id
WHERE e.session_id ~ '^seed_s_'
  AND (e.distinct_id <> s.distinct_id
       OR e.device_key <> s.device_key
       OR e.environment_id IS DISTINCT FROM s.environment_id);

\echo '=== MUST BE 0: error rows whose issue fingerprint disagrees ==='
SELECT count(*) AS mismatched_issue_rows
FROM error_events ee JOIN issues i ON i.id = ee.issue_id
WHERE ee.session_id ~ '^seed_s_' AND i.fingerprint <> ee.fingerprint;

\echo '=== MUST BE 0: issues.times_seen disagreeing with actual occurrences ==='
\echo '(error_events has no dedup, so times_seen is the only occurrence counter)'
SELECT count(*) AS drifted_issues FROM (
  SELECT i.id FROM issues i
  JOIN (SELECT issue_id, count(*) n FROM error_events
        WHERE app_id = :app_id::uuid GROUP BY issue_id) a ON a.issue_id = i.id
  WHERE i.times_seen <> a.n
) d;

\echo '=== Distributions ==='
SELECT
  (SELECT round(100.0 * count(*) FILTER (WHERE handled IS false) / count(*), 1)
     FROM error_events WHERE session_id ~ '^seed_s_')            AS pct_unhandled,
  (SELECT round(avg(events_count), 1) FROM sessions
     WHERE session_id ~ '^seed_s_')                              AS avg_events_per_session,
  (SELECT count(DISTINCT distinct_id) FROM analytics_events
     WHERE session_id ~ '^seed_s_')                              AS active_users;

\echo '=== Zipf: share of events held by the busiest 1% of active users ==='
\echo '(a uniform draw would give ~1%)'
SELECT round(100.0 * sum(c) / (SELECT count(*) FROM analytics_events WHERE session_id ~ '^seed_s_'), 1)
       AS pct_events_top_1pct_users
FROM (
  SELECT count(*) c FROM analytics_events WHERE session_id ~ '^seed_s_'
  GROUP BY distinct_id ORDER BY count(*) DESC
  LIMIT (SELECT greatest(1, count(DISTINCT distinct_id) / 100) FROM analytics_events WHERE session_id ~ '^seed_s_')
) t;

\echo '=== Rollups + the markers that gate the fast query shapes ==='
SELECT
  (SELECT count(*) FROM event_user_environments WHERE app_id = :app_id::uuid) AS person_env_rows,
  (SELECT count(*) FROM device_environments     WHERE app_id = :app_id::uuid) AS device_env_rows,
  (SELECT count(*) FROM device_env_backfill     WHERE app_id = :app_id::uuid) AS device_marker,
  (SELECT count(*) FROM event_user_env_backfill WHERE app_id = :app_id::uuid) AS person_marker;

\echo '=== On-disk size ==='
SELECT c.relname AS tbl, pg_size_pretty(sum(pg_total_relation_size(p.oid))) AS total
FROM pg_class c
JOIN pg_inherits i ON i.inhparent = c.oid
JOIN pg_class p ON p.oid = i.inhrelid
WHERE c.relname IN ('analytics_events', 'error_events', 'transactions')
GROUP BY c.relname
UNION ALL
SELECT 'sessions', pg_size_pretty(pg_total_relation_size('sessions'))
UNION ALL
SELECT 'devices + event_users',
       pg_size_pretty(pg_total_relation_size('devices') + pg_total_relation_size('event_users'))
ORDER BY 1;
