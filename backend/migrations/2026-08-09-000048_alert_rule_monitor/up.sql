-- Pin a monitor alert rule to ONE monitor. NULL keeps the existing meaning:
-- every monitor in the rule's scope, exactly as every stored row behaves today.
ALTER TABLE alert_rules
  ADD COLUMN monitor_id UUID REFERENCES monitors(id) ON DELETE CASCADE;

-- CASCADE, not SET NULL, deliberately. SET NULL would silently WIDEN a rule:
-- delete the one monitor a critical-severity pager rule watches and it would
-- quietly begin firing for every monitor in the project. A rule that exists
-- only to watch one monitor should be removed with it.

-- A monitor_id on any other trigger is dead configuration nothing ever reads.
ALTER TABLE alert_rules ADD CONSTRAINT alert_rules_monitor_trigger_chk
  CHECK (monitor_id IS NULL OR trigger_type IN ('monitor_down','monitor_up'));

CREATE INDEX alert_rules_monitor_idx ON alert_rules (monitor_id)
  WHERE monitor_id IS NOT NULL;
