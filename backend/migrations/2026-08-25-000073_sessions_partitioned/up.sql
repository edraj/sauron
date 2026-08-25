-- 0073: partition sessions by RANGE(started_at), closing the last unpartitioned
-- grower (~600k rows/day at production density).
--
-- Sessions differ from the three firehose tables in one way that shapes
-- everything here: they MUTATE via upsert, and a partitioned table cannot
-- carry the global UNIQUE (app_id, session_id) that upsert's ON CONFLICT
-- targeted (every partitioned unique index must include the partition key,
-- and started_at is batch-derived and movable — putting it in the conflict
-- key would split one session into many rows). The write path therefore
-- becomes an advisory-lock-serialized lookup-then-update/insert — see
-- `sauron_db::batch::bump_sessions` / `repo::bump_session`, changed in
-- lockstep with this migration. The per-partition
-- UNIQUE (app_id, session_id, started_at) below is a belt for same-day
-- duplicate attempts; cross-day uniqueness is owned by those locks, and the
-- rollup maintenance's daily duplicate probe is the alarm if they ever fail.
--
-- Partitions are created for the EXISTING data span before the copy — the
-- 0013 template parked everything in DEFAULT, and evicting rows from a
-- default partition later blocks CREATE TABLE .. PARTITION OF (hit live
-- during the 10M seed). Future days are pre-created by the rollup
-- maintenance task (+7 ahead), with DEFAULT as the safety net.
--
-- Storage settings mirror migration 60's firehose partitions, with realistic
-- thresholds for an UPDATE-heavy table.
ALTER TABLE sessions RENAME TO sessions_old;

CREATE TABLE sessions (
    id                     UUID NOT NULL DEFAULT gen_random_uuid(),
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
    PRIMARY KEY (id, started_at)
) PARTITION BY RANGE (started_at);

DO $part$
DECLARE
  lo date;
  d  date;
BEGIN
  SELECT LEAST(COALESCE(min(started_at)::date, CURRENT_DATE), CURRENT_DATE)
    INTO lo FROM sessions_old;
  FOR d IN SELECT generate_series(lo, CURRENT_DATE + 7, INTERVAL '1 day')::date LOOP
    EXECUTE format(
      'CREATE TABLE %I PARTITION OF sessions FOR VALUES FROM (%L) TO (%L) '
      'WITH (autovacuum_vacuum_scale_factor = 0.0, autovacuum_vacuum_threshold = 5000, '
      'autovacuum_analyze_scale_factor = 0.0, autovacuum_analyze_threshold = 5000)',
      'sessions_' || to_char(d, 'YYYY_MM_DD'),
      d::text || ' 00:00:00+00',
      (d + 1)::text || ' 00:00:00+00');
  END LOOP;
END $part$;

CREATE TABLE sessions_default PARTITION OF sessions DEFAULT;

INSERT INTO sessions SELECT * FROM sessions_old;

DROP TABLE sessions_old;

-- Indexes AFTER the drop: RENAME keeps the old table's index names, so
-- creating them earlier collides (the 0011-0013 lesson).
CREATE UNIQUE INDEX sessions_app_session_started_key ON sessions (app_id, session_id, started_at);
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
