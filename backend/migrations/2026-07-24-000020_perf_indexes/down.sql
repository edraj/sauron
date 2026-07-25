-- Drops ONLY what up.sql created, and restores what it dropped.
--
-- The previous version listed `sessions_app_distinct_idx` and
-- `devices_app_last_seen_idx`, which this migration never created — they come
-- from 0004. Reverting therefore destroyed another migration's indexes and left
-- the schema in a state neither migration describes.

DROP INDEX IF EXISTS error_events_app_screen_time_idx;
DROP INDEX IF EXISTS analytics_events_app_screen_time_idx;
DROP INDEX IF EXISTS transactions_app_name_time_idx;
DROP INDEX IF EXISTS transactions_app_op_time_idx;
DROP INDEX IF EXISTS analytics_events_app_distinct_time_idx;
DROP INDEX IF EXISTS issues_app_last_seen_idx;
DROP INDEX IF EXISTS issues_app_first_seen_idx;
DROP INDEX IF EXISTS sessions_app_device_started_idx;
DROP INDEX IF EXISTS sessions_app_distinct_notnull_idx;
DROP INDEX IF EXISTS event_users_app_first_seen_idx;
DROP INDEX IF EXISTS event_users_app_last_seen_idx;

-- Recreate the (app_id, screen) indexes from 0011/0012 that up.sql superseded.
CREATE INDEX IF NOT EXISTS analytics_events_app_screen_idx
    ON analytics_events (app_id, screen);
CREATE INDEX IF NOT EXISTS error_events_app_screen_idx
    ON error_events (app_id, screen);
