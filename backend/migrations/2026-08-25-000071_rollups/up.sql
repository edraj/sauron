-- Dashboard rollups: the request path must never aggregate raw event rows.
--
-- Seven small aggregate tables, folded incrementally by a watermarked task in
-- the ingest process (sauron-pipeline::rollup_task) and read by sauron-api
-- behind rollups::is_ready() — the same gate shape as device_env_backfill.
-- Their size is bounded by (keys × environments × days), never by event
-- volume, which is the entire point: a 90-day aggregate touches at most a few
-- thousand rows regardless of how many billions of events stand behind them.
--
-- Spec: docs/superpowers/specs/2026-08-25-dashboard-10m-per-day-optimization-design.md

-- Epoch + watermarks. The fold covers (epoch, ∞) by received_at; the one-shot
-- `sauron-migrate backfill-rollups` covers (-∞, epoch]. Stamping the epoch in
-- the SAME migration that creates the tables is the migration-70 lesson: a
-- stamp taken later lies about every row that arrived in between, and that
-- instant is not recoverable after the fact.
CREATE TABLE rollup_epoch (
    only_row   boolean     PRIMARY KEY DEFAULT true CHECK (only_row),
    started_at timestamptz NOT NULL DEFAULT now()
);
INSERT INTO rollup_epoch DEFAULT VALUES;

-- One watermark per source, same shape as tiering_state. "sessions" is not a
-- received_at watermark — sessions mutate in place, so their rollup is a
-- rolling recompute and the row is purely a freshness stamp for as_of().
CREATE TABLE rollup_watermarks (
    source     text        PRIMARY KEY,
    watermark  timestamptz NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now()
);
INSERT INTO rollup_watermarks (source, watermark)
SELECT s, (SELECT started_at FROM rollup_epoch)
FROM unnest(ARRAY['analytics_events','error_events','transactions','sessions']) AS s;

-- Per-app readiness marker, written by backfill IN THE SAME TRANSACTION as the
-- last aggregate it writes (device_env_backfill:88 rule: the marker must never
-- be visible before the data it claims). Apps created after the epoch are
-- implicitly ready — every row they will ever have is post-epoch and folded
-- live — which rollups::is_ready() encodes as an OR on apps.created_at.
CREATE TABLE rollup_backfill (
    app_id       uuid PRIMARY KEY REFERENCES apps(id) ON DELETE CASCADE,
    completed_at timestamptz NOT NULL DEFAULT now()
);

-- /screens. views = analytics name='$screen'; events = the rest; exceptions
-- from error_events (screen IS NOT NULL); users_hll spans both streams; dwell
-- is the gap from a screen view to the session's next analytics event, capped
-- at 30 min, exactly like repo::screen_ctes' LEAD shape.
CREATE TABLE screen_stats_daily (
    app_id uuid NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    environment_id uuid REFERENCES app_environments(id) ON DELETE CASCADE,
    day date NOT NULL,
    screen text NOT NULL,
    views bigint NOT NULL DEFAULT 0,
    events bigint NOT NULL DEFAULT 0,
    exceptions bigint NOT NULL DEFAULT 0,
    users_hll bytea,
    dwell_ms_sum double precision NOT NULL DEFAULT 0,
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX screen_stats_daily_key ON screen_stats_daily
    (app_id, day, screen, COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid));

-- /journeys, day-scoped: the first ≤10 analytics events per user per UTC day.
-- This is a DISCLOSED semantic change from "first N from window start" — that
-- shape cannot be pre-aggregated for arbitrary windows, and at 90-day windows
-- it mostly showed users' first-ever day anyway.
CREATE TABLE journey_nodes_daily (
    app_id uuid NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    environment_id uuid REFERENCES app_environments(id) ON DELETE CASCADE,
    day date NOT NULL,
    step smallint NOT NULL,
    name text NOT NULL,
    count bigint NOT NULL DEFAULT 0
);
CREATE UNIQUE INDEX journey_nodes_daily_key ON journey_nodes_daily
    (app_id, day, step, name, COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid));

CREATE TABLE journey_links_daily (
    app_id uuid NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    environment_id uuid REFERENCES app_environments(id) ON DELETE CASCADE,
    day date NOT NULL,
    step smallint NOT NULL,
    from_name text NOT NULL,
    to_name text NOT NULL,
    count bigint NOT NULL DEFAULT 0
);
CREATE UNIQUE INDEX journey_links_daily_key ON journey_links_daily
    (app_id, day, step, from_name, to_name, COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid));

-- /performance. Hourly because /performance/series is hourly on the wire; the
-- summary aggregates hours. duration_hist is the √2 log-bucket histogram
-- (sauron_db::sketch, 56 buckets) — mergeable across hours, environments AND
-- tiers, which removes the old "percentiles are hot-only" limitation.
CREATE TABLE perf_agg_hourly (
    app_id uuid NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    environment_id uuid REFERENCES app_environments(id) ON DELETE CASCADE,
    hour timestamptz NOT NULL,
    name text NOT NULL,
    op text NOT NULL,
    count bigint NOT NULL DEFAULT 0,
    error_count bigint NOT NULL DEFAULT 0,
    duration_sum double precision NOT NULL DEFAULT 0,
    duration_hist bytea NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX perf_agg_hourly_key ON perf_agg_hourly
    (app_id, hour, name, op, COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid));

-- /sessions/summary. Keyed by STARTED day (disclosed drift from the old
-- last_event_at bound). Rebuilt as a rolling recompute of recently-active
-- days rather than folded — sessions mutate while open, and re-aggregating a
-- day of the sessions table is cheap (it is 1-2 orders smaller than events).
-- The 5 fixed d_* buckets mirror repo::DURATION_BUCKET_CASE_SQL exactly.
CREATE TABLE session_stats_daily (
    app_id uuid NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    environment_id uuid REFERENCES app_environments(id) ON DELETE CASCADE,
    day date NOT NULL,
    sessions bigint NOT NULL DEFAULT 0,
    crashed bigint NOT NULL DEFAULT 0,
    duration_ms_sum double precision NOT NULL DEFAULT 0,
    duration_hist bytea NOT NULL,
    d_lt10s bigint NOT NULL DEFAULT 0,
    d_10_60s bigint NOT NULL DEFAULT 0,
    d_1_5m bigint NOT NULL DEFAULT 0,
    d_5_30m bigint NOT NULL DEFAULT 0,
    d_gte30m bigint NOT NULL DEFAULT 0,
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX session_stats_daily_key ON session_stats_daily
    (app_id, day, COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid));

-- Users + overview series. hll_all spans analytics ∪ errors (users/summary,
-- DAU/WAU/MAU — now CALENDAR-day, disclosed); hll_analytics is analytics-only
-- (the active-users series endpoints deliberately exclude error-only rows).
-- events/errors are exact per-day counts and feed the Overview series/totals.
CREATE TABLE user_activity_daily (
    app_id uuid NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    environment_id uuid REFERENCES app_environments(id) ON DELETE CASCADE,
    day date NOT NULL,
    hll_all bytea,
    hll_analytics bytea,
    events bigint NOT NULL DEFAULT 0,
    errors bigint NOT NULL DEFAULT 0,
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX user_activity_daily_key ON user_activity_daily
    (app_id, day, COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid));

-- /events/top and the Overview top-events card (name + count only; the
-- endpoint returns nothing per-user, so no sketch). The fold caps distinct
-- names per (app, day) at rollup_name_cap and folds the tail into '~other'.
CREATE TABLE event_top_daily (
    app_id uuid NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    environment_id uuid REFERENCES app_environments(id) ON DELETE CASCADE,
    day date NOT NULL,
    name text NOT NULL,
    count bigint NOT NULL DEFAULT 0
);
CREATE UNIQUE INDEX event_top_daily_key ON event_top_daily
    (app_id, day, name, COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid));

-- Cross-fold state. A screen view's dwell ends at the session's NEXT analytics
-- event, which may arrive in a later fold — session_state carries the pending
-- view. A user's first-10-of-day spans folds — journey_state carries the step
-- cursor. env_key stores the zero uuid for NULL environments so it can be a
-- primary-key column. Both are pruned by age (2 days) in the maintenance pass;
-- deliberately no FK — these are transient cursors, not durable data, and the
-- fold path should not pay per-row FK probes for them.
CREATE TABLE rollup_session_state (
    app_id uuid NOT NULL,
    session_id text NOT NULL,
    environment_id uuid,
    pending_screen text,
    pending_at timestamptz,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (app_id, session_id)
);
CREATE INDEX rollup_session_state_age ON rollup_session_state (updated_at);

CREATE TABLE rollup_journey_state (
    app_id uuid NOT NULL,
    day date NOT NULL,
    distinct_id text NOT NULL,
    env_key uuid NOT NULL,
    steps smallint NOT NULL,
    last_name text NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (app_id, day, distinct_id, env_key)
);
CREATE INDEX rollup_journey_state_age ON rollup_journey_state (updated_at);

-- The fold's range read. BRIN, not btree: received_at is append-ordered, so a
-- kilobyte-sized block-range summary answers "rows since the watermark" while
-- costing effectively nothing per insert — against the 11 btrees each of these
-- tables already carries, it is noise. Cascades to every partition. NOTE for
-- production: building it takes a brief write-blocking lock per partition.
-- Session recompute reads whole started-days; sessions has no (app, started_at)
-- index and the recompute must not seq-scan a 50M-row table every cycle.
CREATE INDEX sessions_app_started_idx ON sessions (app_id, started_at);
-- The recompute's dirty-day probe is a bare `last_event_at >= $1` with no app
-- bound; without this it seq-scans the whole sessions table every cycle.
CREATE INDEX sessions_last_event_idx ON sessions (last_event_at);

CREATE INDEX analytics_events_received_brin ON analytics_events USING brin (received_at);
CREATE INDEX error_events_received_brin ON error_events USING brin (received_at);
CREATE INDEX transactions_received_brin ON transactions USING brin (received_at);
