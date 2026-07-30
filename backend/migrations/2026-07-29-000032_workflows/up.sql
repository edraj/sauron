-- Workflow grouping, Task 1: `workflows` rollup table (not partitioned,
-- mirrors `sessions` -- see 2026-07-13-000004_sessions_devices/up.sql for the
-- house style this follows) plus nullable attribution columns on the three
-- existing telemetry tables.
--
-- `workflows` keys on `(app_id, workflow_id)` rather than `(app_id,
-- session_id)`: workflows are entirely optional (an app that never calls
-- startWorkflow never inserts a row here) and the three server SDKs have no
-- session id at all, so `session_id` here is nullable free text, not part of
-- the uniqueness constraint -- see the design doc's "State placement" table.
--
-- `status` starts 'active' and transitions to 'completed'/'cancelled' via the
-- three reserved `$workflow_start`/`$workflow_end`/`$workflow_cancel` events;
-- abandonment is derived on read from `status = 'active' AND last_event_at <
-- now() - threshold`, never stored -- so there is no fourth status value and
-- no sweeper job.
CREATE TABLE workflows (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    app_id UUID NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    environment_id UUID NOT NULL REFERENCES environments(id) ON DELETE CASCADE,
    workflow_id TEXT NOT NULL,
    name TEXT NOT NULL,
    session_id TEXT,
    distinct_id TEXT,
    device_key TEXT,
    release TEXT,
    status TEXT NOT NULL DEFAULT 'active',
    cancel_reason TEXT,
    started_at TIMESTAMPTZ NOT NULL,
    ended_at TIMESTAMPTZ,
    last_event_at TIMESTAMPTZ NOT NULL,
    events_count INTEGER NOT NULL DEFAULT 0,
    errors_count INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT workflows_app_workflow_key UNIQUE (app_id, workflow_id),
    CONSTRAINT workflows_status_check CHECK (status IN ('active', 'completed', 'cancelled'))
);

CREATE INDEX workflows_app_name_started_idx ON workflows (app_id, name, started_at DESC);
CREATE INDEX workflows_app_status_last_event_idx ON workflows (app_id, status, last_event_at DESC);
CREATE INDEX workflows_app_session_idx ON workflows (app_id, session_id);
CREATE INDEX workflows_app_env_idx ON workflows (app_id, environment_id);

-- Attribution: nullable everywhere, mirroring the existing `screen` field.
-- `analytics_events`, `error_events` and `transactions` are RANGE-partitioned
-- parents (the largest tables in the system); `ADD COLUMN` with no DEFAULT is
-- catalog-only on a partitioned parent -- no rewrite, no long lock -- and
-- propagates to every existing and future partition automatically in
-- Postgres 12+. Do not add a DEFAULT and do not backfill.
ALTER TABLE analytics_events ADD COLUMN workflow_id TEXT;
ALTER TABLE analytics_events ADD COLUMN workflow_name TEXT;
ALTER TABLE error_events ADD COLUMN workflow_id TEXT;
ALTER TABLE error_events ADD COLUMN workflow_name TEXT;
ALTER TABLE transactions ADD COLUMN workflow_id TEXT;
ALTER TABLE transactions ADD COLUMN workflow_name TEXT;

-- Partial indexes keep the cost off apps that never use the feature: a row
-- with `workflow_id IS NULL` (the byte-identical-to-today default) never
-- enters the index at all. `CREATE INDEX` on a partitioned parent (no
-- CONCURRENTLY available inside a migration transaction) propagates
-- synchronously to every child partition, same as migration 000031's
-- precedent.
CREATE INDEX analytics_events_app_workflow_idx
    ON analytics_events (app_id, workflow_name, occurred_at DESC)
    WHERE workflow_id IS NOT NULL;
CREATE INDEX error_events_app_workflow_idx
    ON error_events (app_id, workflow_name, occurred_at DESC)
    WHERE workflow_id IS NOT NULL;
CREATE INDEX transactions_app_workflow_idx
    ON transactions (app_id, workflow_name, occurred_at DESC)
    WHERE workflow_id IS NOT NULL;
