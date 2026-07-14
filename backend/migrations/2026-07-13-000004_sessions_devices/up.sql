-- 0004: sessions + devices, materialized from the existing ingest path, plus the
-- columns that let both signal streams (analytics events + errors) join into them.
--
-- Sessions and devices are upserted by the pipeline as events/errors arrive
-- (keyed by (app_id, session_id) / (app_id, device_key)). No new storage tier —
-- these are roll-up tables over the same Postgres the rest of Sauron uses.

CREATE TABLE sessions (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    app_id         UUID NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    session_id     TEXT NOT NULL,                       -- SDK-provided session id
    distinct_id    TEXT,                                -- person, once identified
    device_key     TEXT,                                -- links to devices.device_key
    started_at     TIMESTAMPTZ NOT NULL,
    last_event_at  TIMESTAMPTZ NOT NULL,
    events_count   BIGINT NOT NULL DEFAULT 0,
    errors_count   BIGINT NOT NULL DEFAULT 0,
    context        JSONB NOT NULL DEFAULT '{}'::jsonb,  -- snapshot: device/os/ua/app/runtime
    release        TEXT,
    environment_id UUID,
    ip_address     TEXT,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (app_id, session_id)
);
CREATE INDEX sessions_app_last_event_idx ON sessions (app_id, last_event_at DESC);
CREATE INDEX sessions_app_distinct_idx ON sessions (app_id, distinct_id);
CREATE INDEX sessions_app_device_idx ON sessions (app_id, device_key);

CREATE TABLE devices (
    id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    app_id           UUID NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    device_key       TEXT NOT NULL,                     -- stable install id, else a descriptor hash
    family           TEXT,
    model            TEXT,
    os_name          TEXT,
    os_version       TEXT,
    arch             TEXT,
    browser          TEXT,
    last_distinct_id TEXT,
    first_seen       TIMESTAMPTZ NOT NULL,
    last_seen        TIMESTAMPTZ NOT NULL,
    events_count     BIGINT NOT NULL DEFAULT 0,
    errors_count     BIGINT NOT NULL DEFAULT 0,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (app_id, device_key)
);
CREATE INDEX devices_app_last_seen_idx ON devices (app_id, last_seen DESC);

-- Tie errors to their session + device (analytics events already carry session_id).
ALTER TABLE error_events ADD COLUMN session_id TEXT;
ALTER TABLE error_events ADD COLUMN device_key TEXT;
CREATE INDEX error_events_app_session_idx ON error_events (app_id, session_id);
CREATE INDEX error_events_app_device_idx ON error_events (app_id, device_key);

-- Tie analytics events to their device (errors resolve device via context).
ALTER TABLE analytics_events ADD COLUMN device_key TEXT;
CREATE INDEX analytics_events_app_device_idx ON analytics_events (app_id, device_key);
