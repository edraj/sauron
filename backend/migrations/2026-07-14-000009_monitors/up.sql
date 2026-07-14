CREATE TABLE monitors (
    id                      UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id              UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    name                    TEXT NOT NULL,
    kind                    TEXT NOT NULL CHECK (kind IN ('http', 'tcp')),
    target                  TEXT NOT NULL,
    method                  TEXT NOT NULL DEFAULT 'GET',
    config                  JSONB NOT NULL DEFAULT '{}'::jsonb,
    interval_seconds        INT  NOT NULL DEFAULT 60,
    timeout_ms              INT  NOT NULL DEFAULT 10000,
    failure_threshold       INT  NOT NULL DEFAULT 2,
    recovery_threshold      INT  NOT NULL DEFAULT 1,
    webhook_url             TEXT,
    enabled                 BOOL NOT NULL DEFAULT TRUE,
    status                  TEXT NOT NULL DEFAULT 'unknown'
                              CHECK (status IN ('unknown', 'up', 'down', 'paused')),
    consecutive_failures    INT  NOT NULL DEFAULT 0,
    consecutive_successes   INT  NOT NULL DEFAULT 0,
    last_checked_at         TIMESTAMPTZ,
    next_check_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_status_changed_at  TIMESTAMPTZ,
    created_by              UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX monitors_due_idx ON monitors (next_check_at) WHERE enabled;
CREATE INDEX monitors_project_idx ON monitors (project_id);

CREATE TABLE monitor_checks (
    id                UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    monitor_id        UUID NOT NULL REFERENCES monitors(id) ON DELETE CASCADE,
    checked_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    up                BOOL NOT NULL,
    status_code       INT,
    response_time_ms  INT,
    error             TEXT
);
CREATE INDEX monitor_checks_monitor_time_idx ON monitor_checks (monitor_id, checked_at DESC);

CREATE TABLE monitor_incidents (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    monitor_id   UUID NOT NULL REFERENCES monitors(id) ON DELETE CASCADE,
    started_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    resolved_at  TIMESTAMPTZ,
    cause        TEXT NOT NULL,
    last_error   TEXT
);
CREATE UNIQUE INDEX monitor_incidents_one_open_idx ON monitor_incidents (monitor_id) WHERE resolved_at IS NULL;
CREATE INDEX monitor_incidents_monitor_idx ON monitor_incidents (monitor_id, started_at DESC);
