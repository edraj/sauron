-- Release-keyed twins of the Users page's rollups, so the release switcher can
-- narrow /users/summary, /persons and /counts/persons without a raw scan.
-- Design: docs/superpowers/specs/2026-10-15-release-users-design.md
--
-- `release` is NOT NULL on all three tables, with '' standing for "Unknown
-- release" (the telemetry tables' NULL). The ingest edge normalises a blank
-- release to NULL and `sauron-migrate backfill-releases` repaired history, so
-- '' is never a real release name. Storing the sentinel instead of NULL keeps
-- every unique index a plain column list (no second COALESCE) and every read a
-- single `release = $n` that the leading (app_id, release) prefix serves.
--
-- `environment_id` keeps the house nullable-with-nil-sentinel shape (migration
-- 56): NULL is EnvFilter::Unattributed, a real row, and every ON CONFLICT must
-- name the same COALESCE expression as the unique index.

-- One row per (app, release, environment, person): the
-- event_user_environments twin. Exact Total / Active / New (+ identified
-- splits) and the release-scoped persons list read it.
CREATE TABLE person_releases (
    app_id         uuid        NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    environment_id uuid        NULL REFERENCES app_environments(id) ON DELETE CASCADE,
    release        text        NOT NULL,
    distinct_id    text        NOT NULL,
    first_seen     timestamptz NOT NULL,
    last_seen      timestamptz NOT NULL,
    events_count   bigint      NOT NULL DEFAULT 0,
    errors_count   bigint      NOT NULL DEFAULT 0,
    sessions_count bigint      NOT NULL DEFAULT 0,
    updated_at     timestamptz NOT NULL DEFAULT now()
);

-- Person before environment: every release read picks ONE representative row
-- per person (rollups::release's "representative row" predicate) by probing
-- that person's other environment rows on the same release, and the probe is a
-- prefix of this key. It also streams persons in distinct_id order.
CREATE UNIQUE INDEX person_releases_key ON person_releases
    (app_id, release, distinct_id,
     COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid));

-- Page-first: the persons list's two durable sorts and the Active / New
-- window counts stream one release's rows in time order and stop at the page
-- (or the window) instead of aggregating the whole audience.
CREATE INDEX person_releases_last_seen_idx ON person_releases
    (app_id, release, last_seen DESC);
CREATE INDEX person_releases_first_seen_idx ON person_releases
    (app_id, release, first_seen);

-- "Currently on it": is there a LATER row for this person on another release?
-- Probed once per candidate person, so it leads with the person.
CREATE INDEX person_releases_person_idx ON person_releases
    (app_id, distinct_id, last_seen DESC);

-- Per (app, release, env, day) user sketches: the user_activity_daily twin,
-- carrying only the two sketches the Users page reads (no event/error counts,
-- no analytics-only sketch). HLL merge is idempotent, so the backfill may
-- overlap the live fold without inflating.
CREATE TABLE user_activity_release_daily (
    app_id         uuid  NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    environment_id uuid  NULL REFERENCES app_environments(id) ON DELETE CASCADE,
    release        text  NOT NULL,
    day            date  NOT NULL,
    hll_all        bytea NOT NULL,
    hll_identified bytea NOT NULL,
    updated_at     timestamptz NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX user_activity_release_daily_key ON user_activity_release_daily
    (app_id, release, day, COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid));

-- fold::fold_day_body's consistency rebuild deletes one day across every app
-- and release, inside the advisory lock the live fold waits on.
CREATE INDEX user_activity_release_daily_day_idx ON user_activity_release_daily (day);

-- Per (app, release, env, started-day) session duration: the
-- session_stats_daily twin, REPLACE-maintained by fold::recompute_sessions.
CREATE TABLE session_stats_release_daily (
    app_id          uuid   NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    environment_id  uuid   NULL REFERENCES app_environments(id) ON DELETE CASCADE,
    release         text   NOT NULL,
    day             date   NOT NULL,
    sessions        bigint NOT NULL DEFAULT 0,
    duration_ms_sum float8 NOT NULL DEFAULT 0,
    duration_hist   bytea  NOT NULL,
    updated_at      timestamptz NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX session_stats_release_daily_key ON session_stats_release_daily
    (app_id, release, day, COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid));

-- REPLACE deletes by (app, day) across every release.
CREATE INDEX session_stats_release_daily_day_idx ON session_stats_release_daily (app_id, day);

-- The instant the live write path started counting person_releases. Stamped
-- HERE, in the migration that creates the table, because a later stamp lies
-- about everything in between and that instant is unrecoverable (the
-- migration-70 lesson). The backfill adds only signals strictly before it.
CREATE TABLE release_rollup_epoch (
    only_row   boolean     PRIMARY KEY DEFAULT true CHECK (only_row),
    started_at timestamptz NOT NULL DEFAULT now()
);
INSERT INTO release_rollup_epoch DEFAULT VALUES;

-- Per-app readiness marker, written in the SAME transaction as that app's
-- person_releases aggregate, and only after every backfill day is done — a
-- marker must never be visible before the data it claims.
CREATE TABLE release_rollup_backfill (
    app_id       uuid PRIMARY KEY REFERENCES apps(id) ON DELETE CASCADE,
    completed_at timestamptz NOT NULL DEFAULT now()
);

-- Resumable day cursor for the backfill's sketch + session pass: a day is
-- recorded in the same transaction as its writes, so an interrupted run
-- resumes at the first unrecorded day.
CREATE TABLE release_rollup_backfill_days (
    day date PRIMARY KEY
);
