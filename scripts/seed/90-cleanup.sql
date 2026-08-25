-- Remove exactly what the seed added. Not run as part of the seed.
--
-- STRICT REGEXES, NOT `LIKE 'seed\_%'`. This instance already contained 81
-- `event_users` rows from an earlier session's seeding — `seed_mrsuix2u_191`
-- and friends — on the SAME app. A prefix match would delete those too: data
-- this script did not create and cannot restore. Every pattern below is
-- anchored to the exact id shape this seed emits (`seed_u_` + 6 digits,
-- `seed_d_` + 6 digits, `seed_fp_` + 3 digits, `seed_s_<day>_<n>`), so anything
-- shaped differently survives.
--
-- Partitions are NOT dropped. 8 days of the window overlap pre-existing Weby
-- data (2026-07-20 → 2026-07-27), and 30 rescued rows were re-homed into
-- partitions this seed created, so dropping by date would take real data with
-- it. Deleting by identity is slower and correct; dropping by range is fast and
-- wrong.

\set ON_ERROR_STOP on
SET TIME ZONE 'UTC';

\set app_id '\'ee1fb653-cadd-4f27-9321-ff10f382a18c\''

BEGIN;

-- Events first: the rollups and dimensions are derived from them.
DELETE FROM analytics_events WHERE app_id = :app_id::uuid AND session_id ~ '^seed_(s|b)_[0-9]+_[0-9]+$';
DELETE FROM error_events     WHERE app_id = :app_id::uuid AND session_id ~ '^seed_(s|b)_[0-9]+_[0-9]+$';
DELETE FROM transactions     WHERE app_id = :app_id::uuid AND session_id ~ '^seed_(s|b)_[0-9]+_[0-9]+$';
DELETE FROM sessions         WHERE app_id = :app_id::uuid AND session_id ~ '^seed_(s|b)_[0-9]+_[0-9]+$';

DELETE FROM event_users WHERE app_id = :app_id::uuid AND distinct_id ~ '^seed_u_[0-9]{6}$';
DELETE FROM devices     WHERE app_id = :app_id::uuid AND device_key  ~ '^seed_d_[0-9]{6}$';
DELETE FROM issues      WHERE app_id = :app_id::uuid AND fingerprint ~ '^seed_fp_[0-9]{3}$';

-- Rollup rows for identities this seed created.
DELETE FROM event_user_environments WHERE app_id = :app_id::uuid AND distinct_id ~ '^seed_u_[0-9]{6}$';
DELETE FROM device_environments     WHERE app_id = :app_id::uuid AND device_key  ~ '^seed_d_[0-9]{6}$';

COMMIT;

DROP TABLE IF EXISTS seed_models;
DROP TABLE IF EXISTS seed_issue_pool;
DROP TABLE IF EXISTS seed_tpl_analytics;
DROP TABLE IF EXISTS seed_tpl_error;
DROP TABLE IF EXISTS seed_plan;
DROP TABLE IF EXISTS seed_day_sessions;
DROP TABLE IF EXISTS seed_agg_sess;
DROP TABLE IF EXISTS seed_agg_user;
DROP TABLE IF EXISTS seed_agg_dev;

-- `seed_default_rescue` holds the 30 pre-existing `error_events` rows that were
-- moved out of the DEFAULT partition so partitions could be created over their
-- dates. They were re-inserted into real partitions and are NOT seed data, so
-- this table is deliberately left in place as the record of that move. Drop it
-- only after confirming those 30 ids are present in `error_events`.

\echo 'NOTE: the *_backfill marker rows are left in place — the rollups they gate'
\echo 'are still correct for the remaining data. Re-run 30-rollups.sql to rebuild.'

SELECT
  (SELECT count(*) FROM analytics_events WHERE app_id = :app_id::uuid) AS analytics_left,
  (SELECT count(*) FROM error_events     WHERE app_id = :app_id::uuid) AS errors_left,
  (SELECT count(*) FROM event_users      WHERE app_id = :app_id::uuid) AS users_left,
  (SELECT count(*) FROM devices          WHERE app_id = :app_id::uuid) AS devices_left;
