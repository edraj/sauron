-- The distinct_id twin of migration 53's (app_id, device_key, environment_id, ts)
-- indexes. list_persons derives per-environment counts and first/last_seen from
-- three LEFT JOIN LATERALs keyed on (app_id, distinct_id, environment_id), and
-- derives environment membership from three legs over the same three tables.
--
-- Before this migration the only usable index was
-- analytics_distinct_idx (app_id, distinct_id, occurred_at DESC) -- no
-- environment_id -- so each probe matched on the first two columns and then
-- heap-fetched every row to test environment_id, once per person, across every
-- partition. list_persons has NO time window at all (no `since` parameter, and
-- ILIKE '%' on an unsearched page), so that cost scales with total retained
-- data rather than with a query window, which is why it degrades over time and
-- eventually crosses sauron-api's 30s TimeoutLayer -- mapped onto a 503.
--
-- The trailing timestamp column is the aggregate payload, not a filter: ae/ee
-- take occurred_at (count, min, max), se takes started_at and last_event_at.
-- Dropping it would still serve the lookup but would put the heap fetch
-- straight back.
--
-- Builds SYNCHRONOUSLY across every live child partition inside this
-- transaction, holding locks on the parent and each child. analytics_events and
-- error_events are hot-write tables: this needs a maintenance window.
-- CONCURRENTLY is not an option -- migrations run in a transaction and these are
-- partitioned parents (same constraint as migrations 47 and 53).
CREATE INDEX analytics_events_app_distinct_env_idx
    ON analytics_events (app_id, distinct_id, environment_id, occurred_at);

CREATE INDEX error_events_app_distinct_env_idx
    ON error_events (app_id, distinct_id, environment_id, occurred_at);

CREATE INDEX sessions_app_distinct_env_idx
    ON sessions (app_id, distinct_id, environment_id, started_at, last_event_at);
