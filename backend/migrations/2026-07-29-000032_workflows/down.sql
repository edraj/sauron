-- Reverses 2026-07-29-000032 exactly, narrowest-created-object first.
DROP INDEX IF EXISTS transactions_app_workflow_idx;
DROP INDEX IF EXISTS error_events_app_workflow_idx;
DROP INDEX IF EXISTS analytics_events_app_workflow_idx;

ALTER TABLE transactions DROP COLUMN IF EXISTS workflow_name;
ALTER TABLE transactions DROP COLUMN IF EXISTS workflow_id;
ALTER TABLE error_events DROP COLUMN IF EXISTS workflow_name;
ALTER TABLE error_events DROP COLUMN IF EXISTS workflow_id;
ALTER TABLE analytics_events DROP COLUMN IF EXISTS workflow_name;
ALTER TABLE analytics_events DROP COLUMN IF EXISTS workflow_id;

DROP TABLE IF EXISTS workflows;
