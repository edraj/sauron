-- Windowed session counts for /device-groups without the per-device LATERAL.
--
-- The groups page windows sessions_count (`count(*) FILTER (WHERE started_at
-- >= $2)`) while every other count is lifetime, so device_environments'
-- lifetime counter cannot serve it. The old answer was a live LATERAL over
-- `sessions` per qualifying device — measured 856 ms at 5M sessions, cost
-- tracking the sessions table. This table holds one row per (device, day,
-- environment) with that day's session count, REPLACE-maintained by
-- rollups::fold::recompute_sessions alongside session_stats_daily; a window
-- is then one (app_id, day)-led range scan summed per device.
CREATE TABLE device_sessions_daily (
    app_id uuid NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    device_key text NOT NULL,
    environment_id uuid REFERENCES app_environments(id) ON DELETE CASCADE,
    day date NOT NULL,
    sessions bigint NOT NULL DEFAULT 0,
    updated_at timestamptz NOT NULL DEFAULT now()
);
-- Leading (app_id, day): the reader is a day-range scan, not a device probe.
CREATE UNIQUE INDEX device_sessions_daily_key ON device_sessions_daily
    (app_id, day, device_key, COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid));
