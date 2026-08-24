-- The DELETE in up.sql is not reversible: the discarded rows were derivable
-- only from raw signals that may since have been tiered out of Postgres. Down
-- drops the stamp; `backfill_all` reverts to its old (double-counting) cutoff.
DROP TABLE IF EXISTS event_user_env_rollup_epoch;
