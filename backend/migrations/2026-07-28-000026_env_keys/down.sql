-- Reverse step 7 of up.sql. Leaving `env:*` on a custom role after the code is
-- reverted makes that role permanently ungrantable: the grant path requires the
-- caller to hold every permission in the role, and nobody can hold one that is no
-- longer in `perm::ALL`. Presets re-sync from code at boot; custom roles do not.
UPDATE roles
SET permissions = (
    (permissions - 'env:read' - 'env:create' - 'env:update' - 'env:delete' - 'env:rotate_key')
    || CASE WHEN permissions @> '["env:rotate_key"]'::jsonb THEN '["app:rotate_key"]'::jsonb ELSE '[]'::jsonb END
)
WHERE jsonb_typeof(permissions) = 'array'
  AND permissions ?| array['env:read','env:create','env:update','env:delete','env:rotate_key'];

-- Restore the app-level key. Values cannot be recovered, so every app gets a
-- fresh one and every deployed SDK must be reconfigured after a revert.
ALTER TABLE apps ADD COLUMN public_key TEXT;
UPDATE apps SET public_key = 'pk_' || replace(gen_random_uuid()::text, '-', '')
WHERE public_key IS NULL;
ALTER TABLE apps ALTER COLUMN public_key SET NOT NULL;
ALTER TABLE apps ADD CONSTRAINT apps_public_key_key UNIQUE (public_key);

DROP INDEX IF EXISTS environments_app_name_active_key;
DROP INDEX IF EXISTS environments_default_key;

-- Retired environments may collide on (app_id, name) once the predicate is gone.
-- Rename rather than delete. A DELETE would cascade through the `ON DELETE SET NULL`
-- FKs on error_events and analytics_events, permanently stripping environment_id from
-- every historical row that pointed at a retired environment — re-applying up.sql
-- restores none of it. It is also slow and lock-heavy: no index leads with
-- environment_id (the closest, error_events_app_env_time_idx, leads with app_id), so
-- the FK probe seq-scans ~40 partitions of the two largest tables inside this
-- migration's single transaction. And transactions.environment_id has no FK at all,
-- so those rows would have been left dangling rather than nulled.
--
--
-- Note the asymmetry this creates: up.sql re-adds `retired_at` as NULL and
-- `ingest_enabled` as DEFAULT true, so a down->up round trip RESURRECTS these rows as
-- active, under the mangled name and with a freshly generated key. Re-retire them after
-- any such cycle. (Not a credential regression: up.sql regenerates every public_key, so
-- an environment retired because its key leaked stays dead either way.)
-- Appending the row's own id guarantees uniqueness without a collision check.
UPDATE environments
SET name = name || '-retired-' || id::text
WHERE retired_at IS NOT NULL;
ALTER TABLE environments ADD CONSTRAINT environments_app_id_name_key UNIQUE (app_id, name);

ALTER TABLE environments DROP CONSTRAINT IF EXISTS environments_public_key_key;
ALTER TABLE environments
  DROP COLUMN IF EXISTS public_key,
  DROP COLUMN IF EXISTS ingest_enabled,
  DROP COLUMN IF EXISTS is_default,
  DROP COLUMN IF EXISTS retired_at,
  DROP COLUMN IF EXISTS updated_at;
