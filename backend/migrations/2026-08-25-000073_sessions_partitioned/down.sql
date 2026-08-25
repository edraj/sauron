-- Reverse: rebuild the plain table with the original constraints. The global
-- UNIQUE (app_id, session_id) is restorable because the write path's advisory
-- locks kept sessions one-row-per-key while partitioned.
-- If the deferred copy (finish-sessions-partitioning) never ran to completion,
-- fold the remainder back in first so the rebuild below loses nothing. The
-- rows land in DEFAULT (their day partitions may not exist) — irrelevant, the
-- whole partition set is flattened two statements later.
DO $abs$
BEGIN
  IF to_regclass('sessions_old_73') IS NOT NULL THEN
    INSERT INTO sessions SELECT * FROM sessions_old_73 ON CONFLICT DO NOTHING;
    DROP TABLE sessions_old_73;
  END IF;
END $abs$;

ALTER TABLE sessions RENAME TO sessions_part_old;

CREATE TABLE sessions (
    id                     UUID NOT NULL DEFAULT gen_random_uuid() PRIMARY KEY,
    app_id                 UUID NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    session_id             TEXT NOT NULL,
    distinct_id            TEXT,
    device_key             TEXT,
    started_at             TIMESTAMPTZ NOT NULL,
    last_event_at          TIMESTAMPTZ NOT NULL,
    events_count           BIGINT NOT NULL DEFAULT 0,
    errors_count           BIGINT NOT NULL DEFAULT 0,
    context                JSONB NOT NULL DEFAULT '{}'::jsonb,
    release                TEXT,
    environment_id         UUID,
    ip_address             TEXT,
    created_at             TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at             TIMESTAMPTZ NOT NULL DEFAULT now(),
    unhandled_errors_count BIGINT NOT NULL DEFAULT 0,
    UNIQUE (app_id, session_id)
);

INSERT INTO sessions SELECT * FROM sessions_part_old;

DROP TABLE sessions_part_old;

CREATE INDEX sessions_app_device_env_idx ON sessions (app_id, device_key, environment_id, started_at, last_event_at);
CREATE INDEX sessions_app_device_idx ON sessions (app_id, device_key);
CREATE INDEX sessions_app_device_started_idx ON sessions (app_id, device_key, started_at DESC) WHERE (device_key IS NOT NULL);
CREATE INDEX sessions_app_distinct_env_idx ON sessions (app_id, distinct_id, environment_id, started_at, last_event_at);
CREATE INDEX sessions_app_distinct_idx ON sessions (app_id, distinct_id);
CREATE INDEX sessions_app_distinct_notnull_idx ON sessions (app_id, distinct_id) WHERE (distinct_id IS NOT NULL);
CREATE INDEX sessions_app_env_time_idx ON sessions (app_id, environment_id, last_event_at DESC);
CREATE INDEX sessions_app_last_event_idx ON sessions (app_id, last_event_at DESC);
CREATE INDEX sessions_app_started_idx ON sessions (app_id, started_at);
CREATE INDEX sessions_last_event_idx ON sessions (last_event_at);
