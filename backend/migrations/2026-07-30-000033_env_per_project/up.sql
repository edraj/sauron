-- Environments become a project-level catalogue; the (app, env) pair becomes an
-- explicit enrollment that holds the ingest credential.
--
-- This reverts the parentage 2026-07-12-000002 established when it renamed
-- `environments.project_id` to `app_id`. An environment is something an admin
-- defines once for a project ("we ship to dev, staging, production"), not
-- something re-declared per app — five apps in one project should not need five
-- unrelated `staging` rows that can drift into `staging`, `Staging` and `stage`.
--
-- What stays per app is the DATA: error_events, analytics_events, sessions,
-- transactions and workflows remain keyed by (app_id, environment_id), and each
-- app+env pair keeps its own ingest key so that both the app and the
-- environment remain *provable* from the presented key alone — the property
-- 2026-07-28-000026_env_keys was written to establish.
--
-- The pivot that makes this cheap: today's `environments` table ALREADY is the
-- (app, env) pair. It carries app_id, name, public_key, ingest_enabled,
-- is_default and retired_at. So it is renamed into place rather than rebuilt.
-- Foreign keys in Postgres bind to the table OID, not its name, so every
-- referencing table keeps pointing at the same rows and NOT ONE ROW of
-- error_events or analytics_events is rewritten. Those two are RANGE-partitioned
-- and the hottest-write tables in the schema; a remap UPDATE across them would
-- need a maintenance window (see the warnings in migrations 000025 and 000027).
--
-- 2026-07-12-000002 already relied on exactly this mechanism when it renamed
-- `projects` to `apps` and let `environments_project_id_fkey` follow the OID, so
-- the trick is load-bearing in this schema already rather than novel here.

-- ---------------------------------------------------------------------------
-- 1. Rename the pair table out of the way.
-- ---------------------------------------------------------------------------
ALTER TABLE environments RENAME TO app_environments;

-- The FK on `app_id` is still NAMED `environments_project_id_fkey`: migration
-- 000002 renamed the column but not the constraint, and the constraint followed
-- the renamed `projects`→`apps` OID. It therefore already references apps(id)
-- correctly while describing itself as a project reference. Since this migration
-- is touching the table anyway, drop the misnamed constraint and re-add it
-- honestly rather than carrying the lie forward under an `app_` prefix.
ALTER TABLE app_environments DROP CONSTRAINT IF EXISTS environments_project_id_fkey;
ALTER TABLE app_environments DROP CONSTRAINT IF EXISTS environments_app_id_fkey;
ALTER TABLE app_environments
    ADD CONSTRAINT app_environments_app_id_fkey
    FOREIGN KEY (app_id) REFERENCES apps(id) ON DELETE CASCADE;

-- Every remaining constraint and index still spells itself `environments_*`.
-- These MUST be renamed before the new `environments` table is created: indexes
-- are relations and share one namespace per schema, so a new table with a
-- PRIMARY KEY would otherwise collide with `environments_pkey` and get silently
-- disambiguated to `environments_pkey1` — which is exactly the confusing state
-- migration 000002 left behind for `projects`. A loop rather than a list of
-- explicit names because 000026 documented that the stored names vary depending
-- on which spelling of the constraint survived earlier migrations.
DO $$
DECLARE r RECORD;
BEGIN
    FOR r IN
        SELECT conname
        FROM pg_constraint
        WHERE conrelid = 'app_environments'::regclass
          AND conname LIKE 'environments%'
    LOOP
        EXECUTE format(
            'ALTER TABLE app_environments RENAME CONSTRAINT %I TO %I',
            r.conname, 'app_' || r.conname
        );
    END LOOP;

    -- Constraint-backed indexes were renamed along with their constraints
    -- above, so whatever still matches here is a bare CREATE INDEX.
    FOR r IN
        SELECT c.relname
        FROM pg_class c
        JOIN pg_index i ON i.indexrelid = c.oid
        WHERE i.indrelid = 'app_environments'::regclass
          AND c.relname LIKE 'environments%'
    LOOP
        EXECUTE format('ALTER INDEX %I RENAME TO %I', r.relname, 'app_' || r.relname);
    END LOOP;
END $$;

-- ---------------------------------------------------------------------------
-- 2. The new project-level catalogue.
-- ---------------------------------------------------------------------------
CREATE TABLE environments (
    id         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    name       TEXT NOT NULL,
    retired_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Partial rather than a plain UNIQUE (project_id, name), for the reason 000026
-- gave for the enrollment's name index: retiring `staging` must not permanently
-- burn the name. Retirement is what `DELETE /v1/projects/{id}/environments/{id}`
-- does, because an environment with history behind it can never be hard-deleted
-- without taking that history with it.
CREATE UNIQUE INDEX environments_project_name_active_key
    ON environments (project_id, name) WHERE retired_at IS NULL;

-- `is_default` deliberately does NOT exist here. Which environment an app
-- reports to by default is a property of the enrollment, not the catalogue, and
-- a second `is_default` one level up would give two rows the authority to answer
-- the same question.

-- ---------------------------------------------------------------------------
-- 3. Backfill the catalogue from the names already in use.
-- ---------------------------------------------------------------------------
-- Retired enrollments are included. Their name must still resolve to a
-- catalogue row, because step 4 makes `environment_id` NOT NULL for every row
-- including retired ones — an app's history stays readable after an environment
-- is retired, which is the invariant `env_ids_for_app` documents.
-- A catalogue entry is born retired only if EVERY enrollment that contributed
-- the name was already retired — i.e. nobody is still reporting to it anywhere
-- in the project. `max(retired_at)` is then the moment the last one went away.
-- Without this, app1's retired `prod` would resurface as a live project
-- environment and step 5 would auto-enroll every sibling app into it, handing
-- out fresh ingest keys for an environment that was deliberately shut down.
INSERT INTO environments (project_id, name, retired_at)
SELECT a.project_id,
       ae.name,
       CASE WHEN bool_and(ae.retired_at IS NOT NULL) THEN max(ae.retired_at) END
FROM app_environments ae
JOIN apps a ON a.id = ae.app_id
GROUP BY a.project_id, ae.name;

-- ---------------------------------------------------------------------------
-- 4. Link enrollments to the catalogue.
-- ---------------------------------------------------------------------------
-- Added nullable, backfilled, then constrained — the ordering 000026 documents,
-- because NOT NULL cannot be declared against rows that have no value yet.
ALTER TABLE app_environments ADD COLUMN environment_id UUID;

UPDATE app_environments ae
SET environment_id = e.id
FROM apps a, environments e
WHERE a.id = ae.app_id
  AND e.project_id = a.project_id
  AND e.name = ae.name;

ALTER TABLE app_environments ALTER COLUMN environment_id SET NOT NULL;
ALTER TABLE app_environments
    ADD CONSTRAINT app_environments_environment_id_fkey
    FOREIGN KEY (environment_id) REFERENCES environments(id) ON DELETE CASCADE;

-- The name now lives in exactly one place. Keeping a copy on the enrollment
-- would let the two spellings drift, which is the whole problem this migration
-- exists to remove.
DROP INDEX IF EXISTS app_environments_app_name_active_key;
ALTER TABLE app_environments DROP COLUMN name;

-- ---------------------------------------------------------------------------
-- 5. Auto-enroll: every app is enrolled in every environment of its project.
-- ---------------------------------------------------------------------------
-- Before this migration each app had only the environments someone created on
-- it directly, so collapsing per-app names into a shared catalogue leaves gaps:
-- app A had `staging`, app B did not, and `staging` is now a project-wide
-- environment that B must also be reachable in.
--
-- Keys use the same construction as 000026 — `gen_random_uuid()` is built into
-- Postgres 13+ and stripping its dashes yields exactly the `pk_` + 32-hex shape
-- `ids::public_key()` produces, so no pgcrypto dependency is introduced. 122
-- bits of entropy rather than 128, acceptable for a one-time backfill.
--
-- `is_default = false` unconditionally: every app that already existed already
-- has its one default, and `app_environments_default_key` would reject a second.
INSERT INTO app_environments (app_id, environment_id, public_key, ingest_enabled, is_default)
SELECT a.id,
       e.id,
       'pk_' || replace(gen_random_uuid()::text, '-', ''),
       true,
       false
FROM apps a
JOIN environments e ON e.project_id = a.project_id
WHERE e.retired_at IS NULL
  AND NOT EXISTS (
    SELECT 1 FROM app_environments ae
    WHERE ae.app_id = a.id AND ae.environment_id = e.id
);

-- ---------------------------------------------------------------------------
-- 6. Uniqueness moves from (app_id, name) to (app_id, environment_id).
-- ---------------------------------------------------------------------------
-- Partial on `retired_at IS NULL` for the same reason 000026 gave for the name
-- index: retiring an enrollment must not block re-enrolling that app in that
-- environment later.
CREATE UNIQUE INDEX app_environments_app_env_active_key
    ON app_environments (app_id, environment_id) WHERE retired_at IS NULL;

-- Lookup path for "which environments is this app enrolled in", which replaces
-- the old `environments (app_id, name)` index the catalogue join now needs.
CREATE INDEX app_environments_env_idx ON app_environments (environment_id);
