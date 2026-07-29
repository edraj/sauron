-- Slice 2: indexes that make an environment predicate affordable.
--
-- No new table. An earlier draft added an `issue_environments` rollup maintained by an
-- upsert inside `process_error`; it was benchmarked first and cost ~98us per conflict-heavy
-- upsert, roughly 15-25% added to the per-error write path against a 15% guardrail. Per
-- environment issue counts are computed on read instead, which is what the third index here
-- supports.

-- 1. `sessions` and `transactions` carry environment_id but have no index on it, so an
--    environment-filtered session list would seq-scan. Mirrors the shape
--    2026-07-27-000025 established for error_events and analytics_events: tenant key,
--    then the filtered dimension, then the sort column.
CREATE INDEX sessions_app_env_time_idx
    ON sessions (app_id, environment_id, last_event_at DESC);
CREATE INDEX transactions_app_env_time_idx
    ON transactions (app_id, environment_id, occurred_at DESC);

-- 2. Supports the per-issue, per-environment LATERAL aggregate the Issues list runs when a
--    specific environment is selected. The existing error_events_issue_time_id_idx leads
--    with issue_id but does not carry environment_id, so it cannot serve the grouped count
--    without a filter step over every occurrence of the issue.
--
-- `error_events` is a partitioned parent, and `transactions` above is one too: CREATE INDEX
-- on a partitioned parent builds SYNCHRONOUSLY across every live child partition inside this
-- migration's transaction, holding locks on the parent and each child. error_events is the
-- hottest-write table in the schema. This needs a maintenance window, exactly as
-- 2026-07-27-000025_search_indexes documented for the same reason. CONCURRENTLY is not
-- available here — migrations run in a transaction.
CREATE INDEX error_events_issue_env_idx
    ON error_events (issue_id, environment_id);
