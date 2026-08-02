-- Recreates migration 25's definition verbatim. Carries the SAME warning as
-- up.sql: a rollback is also a synchronous rebuild across every child
-- partition, so stop sauron-ingest or drain the stream first.
DROP INDEX IF EXISTS analytics_events_app_env_time_users_idx;
CREATE INDEX analytics_events_app_env_time_idx ON analytics_events (app_id, environment_id, occurred_at DESC);
