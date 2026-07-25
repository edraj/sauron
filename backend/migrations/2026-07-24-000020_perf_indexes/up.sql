-- Indexes backing hot dashboard access patterns that previously fell back to
-- sequential scans.
--
-- Two rules this file learned the hard way:
--   * Never reuse an index name an earlier migration already took. `CREATE
--     INDEX IF NOT EXISTS` then silently no-ops and the index you meant to
--     create never exists, while `down.sql` cheerfully drops the *other*
--     migration's index.
--   * A composite (a,b,c) already serves every lookup that (a,b) serves, so
--     adding the prefix buys nothing and costs a write on every insert.
--     Verified with EXPLAIN: with both present the planner picks the wider
--     index, and dropping the prefix leaves the plan unchanged.

-- event_users had only its (app_id, distinct_id) uniqueness constraint, so the
-- Users Explorer's `ORDER BY last_seen DESC` and the overview's new/returning
-- splits on first_seen had no usable index.
CREATE INDEX IF NOT EXISTS event_users_app_last_seen_idx
    ON event_users (app_id, last_seen DESC);
CREATE INDEX IF NOT EXISTS event_users_app_first_seen_idx
    ON event_users (app_id, first_seen);

-- The per-person LATERAL counts in list_persons look up (app_id, distinct_id)
-- on each signal table. analytics_events and error_events are deliberately NOT
-- indexed here: `analytics_distinct_idx` (0012) and `error_events_distinct_idx`
-- (0011) are (app_id, distinct_id, occurred_at DESC) and already cover it.
--
-- `sessions` does have (app_id, distinct_id) from 0004, but not a partial one;
-- most rows carry a NULL distinct_id, so a partial index is materially smaller.
-- It needs its own name — reusing `sessions_app_distinct_idx` made this a no-op.
CREATE INDEX IF NOT EXISTS sessions_app_distinct_notnull_idx
    ON sessions (app_id, distinct_id)
    WHERE distinct_id IS NOT NULL;

-- The per-device LATERAL session count looks up (app_id, device_key, started_at).
CREATE INDEX IF NOT EXISTS sessions_app_device_started_idx
    ON sessions (app_id, device_key, started_at DESC)
    WHERE device_key IS NOT NULL;

-- NOTE: devices already has (app_id, last_seen DESC) as `devices_app_last_seen_idx`
-- from 0004. Nothing to add.

-- Alert evaluator windows: (app_id, occurred_at) range scans per tick.
CREATE INDEX IF NOT EXISTS issues_app_first_seen_idx
    ON issues (app_id, first_seen DESC);
CREATE INDEX IF NOT EXISTS issues_app_last_seen_idx
    ON issues (app_id, last_seen DESC);

-- Journey graph: `row_number() OVER (PARTITION BY distinct_id ORDER BY
-- occurred_at)` is satisfiable by an ordered index scan with this in place,
-- instead of sorting the whole window range. The existing 0012 index orders
-- occurred_at DESC, which does not serve the ascending window.
CREATE INDEX IF NOT EXISTS analytics_events_app_distinct_time_idx
    ON analytics_events (app_id, distinct_id, occurred_at);

-- Percentile summaries/series filter transactions by app + time, optionally
-- narrowing on op or name. The predicate is `($n IS NULL OR col = $n)`; under a
-- generic plan that degrades to a filter, but with the parameter bound Postgres
-- folds it to a plain equality and uses these (confirmed via EXPLAIN).
CREATE INDEX IF NOT EXISTS transactions_app_op_time_idx
    ON transactions (app_id, op, occurred_at DESC);
CREATE INDEX IF NOT EXISTS transactions_app_name_time_idx
    ON transactions (app_id, name, occurred_at DESC);

-- Screen detail narrows analytics/error events to one screen within a window.
-- These supersede the plain (app_id, screen) indexes from 0011/0012: a lookup
-- always pins an equality on `screen` (so the partial predicate holds) and then
-- wants the time range. Keeping both would mean two index writes per insert on
-- the two highest-volume tables, so the superseded ones are dropped below.
CREATE INDEX IF NOT EXISTS analytics_events_app_screen_time_idx
    ON analytics_events (app_id, screen, occurred_at DESC) WHERE screen IS NOT NULL;
CREATE INDEX IF NOT EXISTS error_events_app_screen_time_idx
    ON error_events (app_id, screen, occurred_at DESC) WHERE screen IS NOT NULL;

DROP INDEX IF EXISTS analytics_events_app_screen_idx;
DROP INDEX IF EXISTS error_events_app_screen_idx;
