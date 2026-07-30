-- Reverses 2026-07-29-000031: restore migration 28's covering index exactly
-- (the real index it created was `error_events_issue_env_covering_idx` --
-- confirmed by reading 2026-07-28-000028_issue_env_covering_index/up.sql
-- before writing this, not assumed), then drop the time-ordered one this
-- migration added, so a revert leaves `error_events` with exactly the index
-- shape it had before this migration ran.
CREATE INDEX error_events_issue_env_covering_idx
    ON error_events (issue_id, environment_id) INCLUDE (distinct_id, occurred_at);

DROP INDEX IF EXISTS error_events_issue_env_time_idx;
