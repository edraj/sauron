-- Retention's substrate: one row per (app, environment, person, day).
--
-- This is the FIRST rollup whose size is bounded by users x days rather than
-- by keys x environments x days, which migration 71's header states as the
-- rollup principle. The exception is deliberate and argued in
-- docs/superpowers/specs/2026-08-28-retention-and-cohorts-design.md: retention
-- is an INTERSECTION -- who was in cohort C and ALSO active in period N -- and
-- user_activity_daily stores HyperLogLog, which unions but does not intersect.
-- HLL is therefore unusable here at any accuracy, not merely imprecise.
--
-- Sizing: ~15M rows / ~2 GB per 90 days at 1M active users, against the ~1.7 TB
-- of firehose covering the same window. Pruned past PERSON_DAYS_KEEP.
CREATE TABLE person_days (
    app_id         uuid NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    environment_id uuid REFERENCES app_environments(id) ON DELETE CASCADE,
    distinct_id    text NOT NULL,
    day            date NOT NULL,
    events         bigint NOT NULL DEFAULT 0,
    errors         bigint NOT NULL DEFAULT 0,
    updated_at     timestamptz NOT NULL DEFAULT now()
);

-- Leading (app_id, env, distinct_id): the retention grid PROBES BY PERSON --
-- it joins each cohort member to their own later days. That is the opposite of
-- device_sessions_daily, whose reader is a day-range scan and whose index
-- therefore leads with day.
--
-- The nil-uuid sentinel rather than a bare nullable column, for the reason
-- migration 56 records: NULL <> NULL, so a plain UNIQUE over a nullable
-- environment_id lets one person accumulate unlimited unattributed rows, and
-- every upsert against them INSERTs instead of UPDATEs -- counters silently
-- stop accumulating for exactly the scope that has no environment. The upsert's
-- ON CONFLICT must name this same expression or it degrades into an
-- unconstrained insert.
CREATE UNIQUE INDEX person_days_key ON person_days
    (app_id, COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid),
     distinct_id, day);

-- The other direction: lifecycle classifies everyone active in a day range, so
-- it scans by day and wants distinct_id straight off the index.
CREATE INDEX person_days_day ON person_days
    (app_id, COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid), day)
    INCLUDE (distinct_id);

-- Its OWN epoch, deliberately NOT rollup_epoch.
--
-- rollup_backfill markers already exist for every app backfilled under
-- migration 71, and those runs never wrote person_days because this table did
-- not exist. Gating on rollups::is_ready() would therefore report READY for an
-- app whose person_days is empty, and the API would answer 0% retention --
-- confidently. That is worse than an error, because it looks like an answer.
-- event_user_env_rollup_epoch and device_env_rollup_epoch exist as separate
-- tables for exactly this reason; this is the third instance of the lesson.
--
-- Stamped HERE, in the same migration that creates the table: a stamp taken
-- later lies about every row that arrived in between, and that instant is not
-- recoverable after the fact (the migration-70 lesson).
--
-- NOTE for the test harness: sauron_db::close_rollup_gate pushes rollup_epoch
-- ten years out so tests exercise the migration-71 aggregates' legacy paths.
-- It must NOT be extended to this table. Retention has no legacy path, so a
-- closed gate here does not select a different code path -- it makes every
-- retention read return empty, and every test asserting against it would pass
-- while verifying nothing.
CREATE TABLE person_days_epoch (
    only_row   boolean     PRIMARY KEY DEFAULT true CHECK (only_row),
    started_at timestamptz NOT NULL DEFAULT now()
);
INSERT INTO person_days_epoch DEFAULT VALUES;

-- Per-app readiness marker, written by the backfill IN THE SAME TRANSACTION as
-- the last rows it writes (the device_env_backfill:88 rule: a marker must never
-- be visible before the data it claims).
CREATE TABLE person_days_backfill (
    app_id       uuid PRIMARY KEY REFERENCES apps(id) ON DELETE CASCADE,
    completed_at timestamptz NOT NULL DEFAULT now()
);
