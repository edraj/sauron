DROP INDEX IF EXISTS store_daily_metrics_app_day_idx;
DROP TABLE IF EXISTS store_daily_metrics;
DROP INDEX IF EXISTS app_store_connections_due_idx;
DROP TABLE IF EXISTS app_store_connections;
ALTER TABLE apps DROP COLUMN IF EXISTS store_environment_id;
