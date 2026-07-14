-- 0011: convert error_events into a RANGE-partitioned table on occurred_at so
-- aged partitions can be exported to Parquet and dropped cheaply.
--
-- Partitioning requires the partition key in every unique/primary key, so the
-- PK becomes (id, occurred_at). We rebuild the table and copy rows through a
-- DEFAULT partition (which also guarantees inserts never fail before the tier
-- worker pre-creates explicit partitions).

ALTER TABLE error_events RENAME TO error_events_old;

CREATE TABLE error_events (
    id              UUID NOT NULL DEFAULT gen_random_uuid(),
    app_id          UUID NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    environment_id  UUID REFERENCES environments(id) ON DELETE SET NULL,
    issue_id        UUID NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    fingerprint     TEXT NOT NULL,
    level           TEXT NOT NULL DEFAULT 'error',
    message         TEXT NOT NULL DEFAULT '',
    exception_type  TEXT NOT NULL DEFAULT '',
    exception_value TEXT NOT NULL DEFAULT '',
    stacktrace      JSONB NOT NULL DEFAULT '[]'::jsonb,
    breadcrumbs     JSONB NOT NULL DEFAULT '[]'::jsonb,
    context         JSONB NOT NULL DEFAULT '{}'::jsonb,
    tags            JSONB NOT NULL DEFAULT '{}'::jsonb,
    release         TEXT,
    distinct_id     TEXT,
    event_user      JSONB,
    sdk             JSONB,
    ip_address      TEXT,
    occurred_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    received_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    session_id      TEXT,
    device_key      TEXT,
    screen          TEXT,
    PRIMARY KEY (id, occurred_at)
) PARTITION BY RANGE (occurred_at);

-- Safety net: catches any row not covered by an explicit range partition.
CREATE TABLE error_events_default PARTITION OF error_events DEFAULT;

-- Move existing rows across (column order matches the old table exactly).
INSERT INTO error_events SELECT * FROM error_events_old;

-- Drop the old table BEFORE recreating the indexes: RENAME kept the old
-- indexes' original names, so they must be gone before we create identically
-- named ones on the new parent.
DROP TABLE error_events_old;

-- Indexes mirror the originals, defined on the partitioned parent so they
-- propagate to every partition (default + future range partitions).
CREATE INDEX error_events_issue_idx      ON error_events (issue_id, occurred_at DESC);
CREATE INDEX error_events_project_idx    ON error_events (app_id, occurred_at DESC);
CREATE INDEX error_events_distinct_idx   ON error_events (app_id, distinct_id, occurred_at DESC);
CREATE INDEX error_events_app_session_idx ON error_events (app_id, session_id);
CREATE INDEX error_events_app_device_idx  ON error_events (app_id, device_key);
CREATE INDEX error_events_app_screen_idx  ON error_events (app_id, screen);
