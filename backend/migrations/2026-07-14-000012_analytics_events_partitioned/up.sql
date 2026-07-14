-- 0012: partition analytics_events by RANGE(occurred_at), mirroring error_events.
-- PK becomes (id, occurred_at); rows copied through a DEFAULT partition. Indexes
-- are (re)created AFTER dropping the old table so their names don't collide with
-- the pre-existing indexes that keep their names through the RENAME.
ALTER TABLE analytics_events RENAME TO analytics_events_old;

CREATE TABLE analytics_events (
    id             UUID NOT NULL DEFAULT gen_random_uuid(),
    app_id         UUID NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    environment_id UUID REFERENCES environments(id) ON DELETE SET NULL,
    name           TEXT NOT NULL,
    distinct_id    TEXT NOT NULL DEFAULT '',
    properties     JSONB NOT NULL DEFAULT '{}'::jsonb,
    context        JSONB NOT NULL DEFAULT '{}'::jsonb,
    session_id     TEXT,
    release        TEXT,
    ip_address     TEXT,
    occurred_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    received_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    device_key     TEXT,
    screen         TEXT,
    PRIMARY KEY (id, occurred_at)
) PARTITION BY RANGE (occurred_at);

CREATE TABLE analytics_events_default PARTITION OF analytics_events DEFAULT;

INSERT INTO analytics_events SELECT * FROM analytics_events_old;

DROP TABLE analytics_events_old;

CREATE INDEX analytics_name_idx             ON analytics_events (app_id, name, occurred_at DESC);
CREATE INDEX analytics_distinct_idx         ON analytics_events (app_id, distinct_id, occurred_at DESC);
CREATE INDEX analytics_project_idx          ON analytics_events (app_id, occurred_at DESC);
CREATE INDEX analytics_events_app_device_idx ON analytics_events (app_id, device_key);
CREATE INDEX analytics_events_app_screen_idx ON analytics_events (app_id, screen);
