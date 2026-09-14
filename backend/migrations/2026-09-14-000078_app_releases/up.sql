-- app_releases: every (app, environment, release) triple ever observed at
-- ingest. Observed, not managed — rows appear when events arrive and are
-- only ever widened. `environment_id` is the ENROLLMENT id
-- (app_environments.id), the same id every environment_id column in the
-- telemetry tables carries; NULL means unattributed.
--
-- The identity is a UNIQUE EXPRESSION index rather than a PRIMARY KEY
-- because NULL never equals NULL: a plain UNIQUE (app_id, environment_id,
-- release) would let unattributed rows duplicate without bound and every
-- upsert against them would INSERT instead of UPDATE (see migration 59 for
-- the same reasoning on device_environments).
CREATE TABLE app_releases (
    id             BIGSERIAL PRIMARY KEY,
    app_id         UUID        NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    environment_id UUID        NULL REFERENCES app_environments(id) ON DELETE CASCADE,
    release        TEXT        NOT NULL,
    first_seen_at  TIMESTAMPTZ NOT NULL,
    last_seen_at   TIMESTAMPTZ NOT NULL
);

CREATE UNIQUE INDEX app_releases_identity_idx
    ON app_releases (app_id, COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid), release);

CREATE INDEX app_releases_app_last_seen_idx
    ON app_releases (app_id, last_seen_at DESC);

-- Release-filtered list pages. error_events and analytics_events already
-- carry (app_id, release, occurred_at DESC) from migration 25; sessions and
-- transactions did not. Both sessions and transactions are PARTITIONED BY
-- RANGE: each CREATE INDEX below builds synchronously across every child
-- partition, holding a lock on the parent and each child for the duration —
-- apply this migration during a low-traffic window.
CREATE INDEX sessions_app_release_last_event_idx
    ON sessions (app_id, release, last_event_at DESC);

CREATE INDEX transactions_app_release_occurred_idx
    ON transactions (app_id, release, occurred_at DESC);
