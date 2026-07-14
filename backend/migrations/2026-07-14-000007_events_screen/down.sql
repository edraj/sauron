DROP INDEX IF EXISTS analytics_events_app_screen_idx;
DROP INDEX IF EXISTS error_events_app_screen_idx;
ALTER TABLE analytics_events DROP COLUMN IF EXISTS screen;
ALTER TABLE error_events     DROP COLUMN IF EXISTS screen;
