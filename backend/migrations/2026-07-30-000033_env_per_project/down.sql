-- Reverses 2026-07-30-000033: collapse the catalogue back onto the enrollment.
--
-- This is not perfectly information-preserving, and cannot be. Step 5 of `up`
-- auto-enrolled every app in every environment of its project; those rows are
-- indistinguishable from ones an admin created, so they survive the reversal as
-- ordinary per-app environments. That is the safe direction to fail: they carry
-- real ingest keys that may already be deployed in an SDK, so deleting them
-- would silently break reporting for whoever picked them up. An environment in
-- the catalogue that no app was ever enrolled in is the one thing genuinely
-- lost, since the old schema had nowhere to put it.

-- ---------------------------------------------------------------------------
-- 1. Restore `name` on the enrollment from the catalogue.
-- ---------------------------------------------------------------------------
ALTER TABLE app_environments ADD COLUMN name TEXT;

UPDATE app_environments ae
SET name = e.name
FROM environments e
WHERE e.id = ae.environment_id;

ALTER TABLE app_environments ALTER COLUMN name SET NOT NULL;

-- ---------------------------------------------------------------------------
-- 2. Drop the link, then the catalogue.
-- ---------------------------------------------------------------------------
DROP INDEX IF EXISTS app_environments_env_idx;
DROP INDEX IF EXISTS app_environments_app_env_active_key;

-- Dropping the column takes `app_environments_environment_id_fkey` with it,
-- which is what lets the catalogue table be dropped without CASCADE.
ALTER TABLE app_environments DROP COLUMN environment_id;

DROP TABLE environments;

-- ---------------------------------------------------------------------------
-- 3. Rename the pair table back into the vacated name.
-- ---------------------------------------------------------------------------
ALTER TABLE app_environments RENAME TO environments;

DO $$
DECLARE r RECORD;
BEGIN
    FOR r IN
        SELECT conname
        FROM pg_constraint
        WHERE conrelid = 'environments'::regclass
          AND conname LIKE 'app\_environments%'
    LOOP
        EXECUTE format(
            'ALTER TABLE environments RENAME CONSTRAINT %I TO %I',
            r.conname, substring(r.conname from 5)
        );
    END LOOP;

    FOR r IN
        SELECT c.relname
        FROM pg_class c
        JOIN pg_index i ON i.indexrelid = c.oid
        WHERE i.indrelid = 'environments'::regclass
          AND c.relname LIKE 'app\_environments%'
    LOOP
        EXECUTE format('ALTER INDEX %I RENAME TO %I', r.relname, substring(r.relname from 5));
    END LOOP;
END $$;

-- ---------------------------------------------------------------------------
-- 4. Restore the name-based uniqueness `up` replaced.
-- ---------------------------------------------------------------------------
-- Safe by construction: `app_environments_app_env_active_key` guaranteed one row
-- per (app, environment), and the catalogue guaranteed one name per
-- (project, environment), so no app can hold two live rows of the same name.
CREATE UNIQUE INDEX environments_app_name_active_key
    ON environments (app_id, name) WHERE retired_at IS NULL;
