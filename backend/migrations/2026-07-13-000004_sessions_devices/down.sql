DROP INDEX IF EXISTS analytics_events_app_device_idx;
ALTER TABLE analytics_events DROP COLUMN IF EXISTS device_key;

DROP INDEX IF EXISTS error_events_app_device_idx;
DROP INDEX IF EXISTS error_events_app_session_idx;
ALTER TABLE error_events DROP COLUMN IF EXISTS device_key;
ALTER TABLE error_events DROP COLUMN IF EXISTS session_id;

DROP TABLE IF EXISTS devices;
DROP TABLE IF EXISTS sessions;
