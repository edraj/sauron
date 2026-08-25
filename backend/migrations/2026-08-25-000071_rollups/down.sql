-- Rollups are derived data: dropping them loses no source of truth, and a
-- re-run of up.sql + `sauron-migrate backfill-rollups` rebuilds them exactly.
DROP INDEX IF EXISTS transactions_received_brin;
DROP INDEX IF EXISTS error_events_received_brin;
DROP INDEX IF EXISTS analytics_events_received_brin;
DROP INDEX IF EXISTS sessions_last_event_idx;
DROP INDEX IF EXISTS sessions_app_started_idx;
DROP TABLE IF EXISTS rollup_journey_state;
DROP TABLE IF EXISTS rollup_session_state;
DROP TABLE IF EXISTS event_top_daily;
DROP TABLE IF EXISTS user_activity_daily;
DROP TABLE IF EXISTS session_stats_daily;
DROP TABLE IF EXISTS perf_agg_hourly;
DROP TABLE IF EXISTS journey_links_daily;
DROP TABLE IF EXISTS journey_nodes_daily;
DROP TABLE IF EXISTS screen_stats_daily;
DROP TABLE IF EXISTS rollup_backfill;
DROP TABLE IF EXISTS rollup_watermarks;
DROP TABLE IF EXISTS rollup_epoch;
