-- 0073 (third shape): partition sessions by RANGE(started_at) — SCHEMA ONLY.
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
-- WHY SCHEMA-ONLY. The first shape of this migration also copied every row
-- and created one partition per calendar day back to min(started_at), all in
-- this one transaction. It failed twice on the first production upgrade:
-- lock-table exhaustion (each daily partition is ~12 locked relations — the
-- table plus 11 auto-created child indexes — against a capacity of
-- max_locks_per_transaction x max_connections, which managed Postgres may
-- not allow raising at all), and a systemd start timeout on the copy. This
-- shape holds ~60 locks whatever the history size and finishes in seconds.
-- The old table survives, renamed, fully indexed, still readable.
--
-- The row copy is deferred to `sauron-migrate finish-sessions-partitioning`:
-- one day per transaction (~15 locks), resumable after any interruption,
-- drops the old table only once it is verifiably empty. RUN IT BEFORE
-- OPENING TRAFFIC — until it completes, session-scoped reads see only rows
-- written after this migration, and live writes would race the copy.

ALTER TABLE sessions RENAME TO sessions_old_73;

-- The rename keeps the old indexes' names, and unlike the 0011-0013 case the
-- old table must SURVIVE this migration — so move its indexes out of the way
-- (rename, not drop) to free the canonical names, including `sessions_pkey`,
-- which the new parent's PRIMARY KEY needs. Since PG12 renaming a
-- constraint-backing index renames the constraint with it.
DO $mv$
DECLARE r record;
BEGIN
  FOR r IN SELECT indexname FROM pg_indexes
           WHERE schemaname = current_schema() AND tablename = 'sessions_old_73'
  LOOP
    EXECUTE format('ALTER INDEX %I RENAME TO %I', r.indexname, left(r.indexname, 59) || '_o73');
  END LOOP;
END $mv$;

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

-- Indexes on the CHILDLESS parent: 11 locks now, and every partition created
-- later — by this file, the finisher, or the rollup maintenance task —
-- inherits them automatically. Creating them after the partitions (the first
-- shape) multiplied the lock cost by the partition count.
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

-- Today and tomorrow only (~24 locks): enough for writes the moment daemons
-- start, without spending 12 locks per day of a whole week here. The rollup
-- maintenance task pre-creates the +7 window in per-statement transactions
-- within a tick of sauron-ingest starting, and DEFAULT nets anything else.
DO $part$
DECLARE d date;
BEGIN
  FOR d IN SELECT generate_series(CURRENT_DATE, CURRENT_DATE + 1, INTERVAL '1 day')::date LOOP
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
