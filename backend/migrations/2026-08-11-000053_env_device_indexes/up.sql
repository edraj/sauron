-- The indexes list_device_groups/list_devices need under an environment filter.
--
-- Under EnvFilter::One the device routes stop reading devices.events_count /
-- errors_count (app-wide, all-time counters) and derive per-environment totals
-- from three LEFT JOIN LATERALs keyed on (app_id, device_key, environment_id).
-- The existing device indexes stop at (app_id, device_key) — no environment_id
-- and no timestamp — so each LATERAL matched on the first two columns and then
-- heap-fetched every row to test environment_id, once per device, across every
-- partition of analytics_events/error_events.
--
-- These three cover the LATERALs' full predicate AND their aggregates
-- (count(*), min/max of the timestamp), so each becomes an index-only scan
-- instead of a heap sweep. Measured on a 1M-event / 5,000-device / 29-partition
-- fixture, device-groups under One(prod) over 30 days: 5.47s -> 2.54s. The win
-- grows with device count, which is what makes this worth the write cost —
-- the LATERALs run for EVERY device matching the filter, before LIMIT, so a
-- 50k-device app pays 10x this and crosses sauron-api's 30s TimeoutLayer,
-- which maps a request timeout onto a 503.
--
-- The trailing timestamp column is the aggregate payload, not a filter: ae/ee
-- take occurred_at (min/max), se takes started_at and last_event_at (the
-- FILTER'd count plus min/max). Dropping it would still serve the lookup but
-- would put the heap fetch straight back.
--
-- Builds SYNCHRONOUSLY across every live child partition inside this
-- transaction, holding locks on the parent and each child. analytics_events and
-- error_events are hot-write tables: this needs a maintenance window.
-- CONCURRENTLY is not an option — migrations run in a transaction and these are
-- partitioned parents (same constraint as migration 47).
CREATE INDEX analytics_events_app_device_env_idx
    ON analytics_events (app_id, device_key, environment_id, occurred_at);

CREATE INDEX error_events_app_device_env_idx
    ON error_events (app_id, device_key, environment_id, occurred_at);

CREATE INDEX sessions_app_device_env_idx
    ON sessions (app_id, device_key, environment_id, started_at, last_event_at);
