-- Only drops what this migration created. (Migration 20's down.sql dropped two
-- indexes it had not created — `sessions_app_distinct_idx` and
-- `devices_app_last_seen_idx` predate it — so reverting it silently destroys
-- migration 0004's indexes. Do not repeat that here.)
DROP INDEX IF EXISTS issues_app_last_event_idx;
DROP INDEX IF EXISTS issues_app_created_idx;
ALTER TABLE issues DROP COLUMN IF EXISTS last_event_at;
