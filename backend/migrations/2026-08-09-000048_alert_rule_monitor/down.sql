DROP INDEX IF EXISTS alert_rules_monitor_idx;
ALTER TABLE alert_rules DROP CONSTRAINT IF EXISTS alert_rules_monitor_trigger_chk;
ALTER TABLE alert_rules DROP COLUMN IF EXISTS monitor_id;
