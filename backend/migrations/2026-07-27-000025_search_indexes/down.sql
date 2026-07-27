-- The jsonb_ops GINs an earlier draft of the up migration created are dropped
-- here too, so that reverting cleans up a database where that draft ran. They
-- are IF EXISTS, so this is a no-op on any database that only ever saw the
-- shipped version.
DROP INDEX IF EXISTS analytics_events_props_gin;
DROP INDEX IF EXISTS analytics_events_extra_gin;
DROP INDEX IF EXISTS analytics_events_contexts_gin;
DROP INDEX IF EXISTS error_events_extra_gin;
DROP INDEX IF EXISTS error_events_contexts_gin;
DROP INDEX IF EXISTS error_events_context_gin;

DROP INDEX IF EXISTS analytics_events_app_rel_time_idx;
DROP INDEX IF EXISTS analytics_events_app_env_time_idx;
DROP INDEX IF EXISTS error_events_app_release_time_idx;
DROP INDEX IF EXISTS error_events_app_level_time_idx;
DROP INDEX IF EXISTS error_events_app_env_time_idx;

-- Recreate what the up migration replaced, so a revert restores the previous
-- plan shapes rather than leaving the table with neither index.
CREATE INDEX error_events_issue_idx ON error_events (issue_id, occurred_at DESC);
DROP INDEX IF EXISTS error_events_issue_time_id_idx;

CREATE INDEX issues_last_seen_idx ON issues (app_id, last_seen DESC);
CREATE INDEX issues_app_last_seen_idx ON issues (app_id, last_seen DESC);
DROP INDEX IF EXISTS issues_app_last_seen_id_idx;
