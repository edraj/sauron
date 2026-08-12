-- Per-(person, environment) rollup for the Users Explorer.
--
-- event_users carries no environment_id, so list_persons derived environment
-- membership, first_seen/last_seen and all three counts from three LATERALs and
-- a membership predicate over analytics_events/error_events/sessions, once per
-- admitted person, with no time bound of any kind. Under a scoped read the sort
-- key is GREATEST(...) over those three tables, so a blocking Sort had to
-- consume every person before LIMIT applied -- the page size capped nothing,
-- and the endpoint crossed sauron-api's 30s TimeoutLayer, which maps a request
-- timeout onto a 503.
--
-- environment_id is NULLABLE on purpose: EnvFilter::Unattributed is a real,
-- surfaced scope (rows ingested before environments existed, or under the old
-- per-app environment cap), and it must be a row here so that "All" equals the
-- sum of the individual environments rather than exceeding it.
--
-- IT REFERENCES app_environments, NOT environments, AND THE MIGRATION SOURCE
-- WILL TELL YOU OTHERWISE. analytics_events/error_events/workflows all declare
-- `REFERENCES environments(id)` in their original DDL, but migration 33
-- (env_per_project) RENAMED that table to app_environments and created a new
-- `environments` catalogue in its place. A rename preserves the OID, so those
-- pre-existing foreign keys silently followed the rename and today point at
-- app_environments -- verify with pg_constraint, not by reading the DDL.
--
-- The value carried in every signal table's environment_id column, and handed
-- to EnvFilter::One by the API, is therefore an app_environments.id (a per-app
-- enrollment), not an environments.id (the per-project catalogue entry). A
-- fresh table written against the catalogue would reject every real id.
CREATE TABLE event_user_environments (
    app_id          uuid        NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    distinct_id     text        NOT NULL,
    environment_id  uuid        NULL REFERENCES app_environments(id) ON DELETE CASCADE,
    first_seen      timestamptz NOT NULL,
    last_seen       timestamptz NOT NULL,
    events_count    bigint      NOT NULL DEFAULT 0,
    errors_count    bigint      NOT NULL DEFAULT 0,
    sessions_count  bigint      NOT NULL DEFAULT 0,
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now()
);

-- NULL never equals NULL, so a plain UNIQUE (app_id, distinct_id,
-- environment_id) would let one person accumulate unlimited unattributed rows,
-- and every upsert against them would INSERT instead of UPDATE -- counts would
-- silently stop accumulating for exactly the scope that has no environment. The
-- nil uuid is safe as the sentinel: it has no environments row, and the foreign
-- key above would reject it as a real value.
--
-- Every ON CONFLICT against this table must name this same expression list.
CREATE UNIQUE INDEX event_user_env_key_idx
    ON event_user_environments
       (app_id, distinct_id, COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid));

-- One per sortable column of PersonRow. The whole point of the rollup is that
-- ORDER BY ... LIMIT applies to a single indexed table, so paging is bounded by
-- page size again instead of by the app's person count.
CREATE INDEX event_user_env_last_seen_idx  ON event_user_environments (app_id, environment_id, last_seen DESC);
CREATE INDEX event_user_env_first_seen_idx ON event_user_environments (app_id, environment_id, first_seen);
CREATE INDEX event_user_env_events_idx     ON event_user_environments (app_id, environment_id, events_count DESC);
CREATE INDEX event_user_env_errors_idx     ON event_user_environments (app_id, environment_id, errors_count DESC);
CREATE INDEX event_user_env_sessions_idx   ON event_user_environments (app_id, environment_id, sessions_count DESC);

-- Which apps' rollups are complete. Reads fall back to the live query for any
-- app without a row here, so a half-populated rollup is never read. The marker
-- is written in the same transaction as that app's backfill aggregate, so it
-- can never be visible before the data it claims -- a marker that ran ahead of
-- its data would make the persons page quiet-wrong rather than error.
--
-- A dedicated table rather than runtime_settings because the marker is per-app
-- and wants the foreign key.
CREATE TABLE event_user_env_backfill (
    app_id       uuid        PRIMARY KEY REFERENCES apps(id) ON DELETE CASCADE,
    completed_at timestamptz NOT NULL
);
