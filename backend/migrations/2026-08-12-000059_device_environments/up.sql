-- Per-(device, environment) rollup for the Devices inventory.
--
-- devices carries no environment_id, so list_device_groups derived membership,
-- first_seen/last_seen and the counts from three membership EXISTS plus three
-- LEFT JOIN LATERALs over analytics_events/error_events/sessions -- the EXISTS
-- once per device in the window, the LATERALs once per QUALIFYING device, each
-- an Append across every partition. GROUP BY then consumed all of them to emit
-- ~40 rows, so `limit` bounded nothing.
--
-- Migration 53 already made each probe an index-only scan; this removes the
-- probes themselves, which no index can. Measured on a 40,000-device /
-- 13,333-qualifying / 1.68M-event / 15-partition fixture, device-groups under
-- One(env) over 30 days: 4,639ms -> 105ms, with zero row differences in either
-- direction. The same fixture at 1,111 qualifying devices took 226ms, i.e. the
-- cost was linear in device count and production runs 29 partitions, not 15.
--
-- environment_id is NULLABLE on purpose: EnvFilter::Unattributed is a real,
-- surfaced scope (rows ingested before environments existed), and it must be a
-- row here so that "All" equals the sum of the individual environments rather
-- than exceeding it.
--
-- IT REFERENCES app_environments, NOT environments, AND THE MIGRATION SOURCE
-- WILL TELL YOU OTHERWISE. Migration 33 (env_per_project) RENAMED the old
-- environments table to app_environments and created a new catalogue under the
-- old name. A rename preserves the OID, so the pre-existing foreign keys on
-- analytics_events/error_events/workflows silently followed it -- pg_constraint
-- says app_environments while init/up.sql still says environments. The value
-- handed to EnvFilter::One by the API is an app_environments.id; a table
-- written against the catalogue would reject every real id.
CREATE TABLE device_environments (
    app_id          uuid        NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    device_key      text        NOT NULL,
    environment_id  uuid        NULL REFERENCES app_environments(id) ON DELETE CASCADE,
    first_seen      timestamptz NOT NULL,
    last_seen       timestamptz NOT NULL,
    events_count    bigint      NOT NULL DEFAULT 0,
    errors_count    bigint      NOT NULL DEFAULT 0,
    sessions_count  bigint      NOT NULL DEFAULT 0,
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now()
);

-- NULL never equals NULL, so a plain UNIQUE (app_id, device_key,
-- environment_id) would let one device accumulate unlimited unattributed rows,
-- and every upsert against them would INSERT instead of UPDATE -- counters
-- would silently stop accumulating for exactly the scope that has no
-- environment. The nil uuid is safe as the sentinel: it has no app_environments
-- row, and the foreign key above would reject it as a real value.
--
-- EVERY ON CONFLICT against this table must name this same expression list.
CREATE UNIQUE INDEX device_env_key_idx
    ON device_environments
       (app_id, device_key, COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid));

-- The read path joins devices to this table filtered by (app_id,
-- environment_id) and then groups, so the driving lookup is that pair. The
-- trailing last_seen serves the default ordering.
CREATE INDEX device_env_app_env_idx ON device_environments (app_id, environment_id, last_seen DESC);

-- The join back to devices is on (app_id, device_key); without this the planner
-- has only the expression index above, whose leading columns suit the upsert
-- rather than the join.
CREATE INDEX device_env_app_device_idx ON device_environments (app_id, device_key);

-- Which apps' rollups are complete. Reads fall back to the live query for any
-- app without a row here, so a half-populated rollup is never read. The marker
-- is written in the same transaction as that app's backfill aggregate, so it
-- can never be visible before the data it claims -- a marker that ran ahead of
-- its data would make the Devices page quiet-wrong rather than error.
--
-- A dedicated table rather than runtime_settings because the marker is per-app
-- and wants the foreign key.
CREATE TABLE device_env_backfill (
    app_id       uuid        PRIMARY KEY REFERENCES apps(id) ON DELETE CASCADE,
    completed_at timestamptz NOT NULL
);

-- When the live write path began maintaining this rollup.
--
-- The backfill aggregates history from BEFORE this instant and bump_device_envs
-- has counted everything at or after it, so the two sets are disjoint and the
-- backfill's `+` is exact. Using now() as the cutoff instead double-counts every
-- signal ingested between this migration landing and an operator getting round to
-- running the backfill -- a window that is never empty, because the migration has
-- to land first. Measured: a signal 30 minutes old came back as 2 instead of 1.
--
-- timestamptz recorded here rather than read from
-- __diesel_schema_migrations.run_on, which diesel declares as a NAIVE `Timestamp`
-- whose UTC meaning depends on the session TimeZone at migration time.
--
-- One row, forever: the boolean-PK-with-CHECK idiom makes a second row impossible.
CREATE TABLE device_env_rollup_epoch (
    only_row   boolean     PRIMARY KEY DEFAULT true CHECK (only_row),
    started_at timestamptz NOT NULL DEFAULT now()
);
INSERT INTO device_env_rollup_epoch DEFAULT VALUES;
