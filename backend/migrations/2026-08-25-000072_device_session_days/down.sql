-- Derived data; a re-run of `sauron-migrate backfill-rollups` (or any session
-- recompute) rebuilds it exactly.
DROP TABLE IF EXISTS device_sessions_daily;
