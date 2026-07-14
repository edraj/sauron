-- 0007: screen attribution. Optional screen/route name stamped by the SDKs on
-- every analytics event and error, mirroring session_id/device_key. Enables the
-- dashboard Screens section (views/events/users/exceptions + on-read dwell).
ALTER TABLE analytics_events ADD COLUMN screen TEXT;
ALTER TABLE error_events     ADD COLUMN screen TEXT;
CREATE INDEX analytics_events_app_screen_idx ON analytics_events (app_id, screen);
CREATE INDEX error_events_app_screen_idx     ON error_events (app_id, screen);
