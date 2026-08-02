-- Recreates migration 25's definition verbatim (`error_events_app_env_time_idx`
-- is created in `2026-07-27-000025_search_indexes/up.sql` line 33, the same
-- migration that creates the analytics twin). Carries the SAME warning as
-- up.sql: a rollback is also a synchronous rebuild across every child
-- partition, so stop sauron-ingest or drain the stream first.
DROP INDEX IF EXISTS error_events_app_env_time_users_idx;
CREATE INDEX error_events_app_env_time_idx ON error_events (app_id, environment_id, occurred_at DESC);
