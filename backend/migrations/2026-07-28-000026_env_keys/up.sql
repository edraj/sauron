-- Environments become the ingest credential holder.
--
-- Until now the environment name arrived as a free string in the envelope header
-- and was upserted on first sight, so it was asserted by whoever held the app's
-- single key. Moving the key onto the environment makes the value provable: the
-- environment is whichever row the presented key belongs to, and there is no way
-- for a client to claim a different one.
--
-- Ordering matters throughout. Columns are added nullable, backfilled, and only
-- then constrained, because NOT NULL + UNIQUE cannot be declared against rows
-- that do not yet have values.

-- 1. New columns, all nullable or defaulted so the ALTER succeeds on live data.
ALTER TABLE environments
  ADD COLUMN public_key     TEXT,
  ADD COLUMN ingest_enabled BOOLEAN NOT NULL DEFAULT true,
  ADD COLUMN is_default     BOOLEAN NOT NULL DEFAULT false,
  ADD COLUMN retired_at     TIMESTAMPTZ,
  ADD COLUMN updated_at     TIMESTAMPTZ NOT NULL DEFAULT now();

-- 2. Every app must own at least one environment, because the app no longer has
--    a key of its own and would otherwise become unreachable by any SDK.
INSERT INTO environments (app_id, name)
SELECT a.id, 'dev'
FROM apps a
WHERE NOT EXISTS (SELECT 1 FROM environments e WHERE e.app_id = a.id);

-- 3. Backfill keys. `gen_random_uuid()` is built into Postgres 13+ (already relied
--    on for every `id` default in this schema), and stripping its dashes yields
--    exactly the `pk_` + 32-hex shape `ids::public_key()` produces — so no pgcrypto
--    dependency is introduced. 122 bits of entropy rather than 128; acceptable for
--    a one-time backfill, and every key minted afterwards uses `getrandom`.
UPDATE environments
SET public_key = 'pk_' || replace(gen_random_uuid()::text, '-', '')
WHERE public_key IS NULL;

ALTER TABLE environments ALTER COLUMN public_key SET NOT NULL;
ALTER TABLE environments ADD CONSTRAINT environments_public_key_key UNIQUE (public_key);

-- 4. Exactly one default per app, chosen deterministically: `production` if it
--    exists, else `dev`, else alphabetically first. `production` wins because
--    apps were seeded with it at creation and are actively reporting into it —
--    defaulting them to a lexicographically-earlier environment would silently
--    change which one the dashboard treats as primary.
UPDATE environments e
SET is_default = true
FROM (
    SELECT DISTINCT ON (app_id) id
    FROM environments
    ORDER BY app_id,
             (name = 'production') DESC,
             (name = 'dev') DESC,
             name ASC
) pick
WHERE e.id = pick.id;

-- Predicate includes `retired_at IS NULL` so a retired row can never occupy an app's
-- only default slot. Without it, retiring a default (however that came about) would
-- make promoting any other environment fail with a duplicate-key error, leaving the app
-- with a default that ingest can never reach — ingest filters `retired_at IS NULL`.
CREATE UNIQUE INDEX environments_default_key
    ON environments (app_id) WHERE is_default AND retired_at IS NULL;

-- 5. Name uniqueness applies only to live environments, so retiring `staging`
--    does not block creating a fresh `staging` later. The original constraint was
--    declared as UNIQUE (project_id, name) in the init migration and the column
--    was later renamed to app_id; renaming a column does NOT rename the
--    constraint, so the stored name may be either. Drop both spellings.
ALTER TABLE environments DROP CONSTRAINT IF EXISTS environments_project_id_name_key;
ALTER TABLE environments DROP CONSTRAINT IF EXISTS environments_app_id_name_key;
CREATE UNIQUE INDEX environments_app_name_active_key
    ON environments (app_id, name) WHERE retired_at IS NULL;

-- 6. The app-level credential is gone. Ingest resolves through the environment.
ALTER TABLE apps DROP COLUMN public_key;

-- 7. `app:rotate_key` no longer exists as a permission, because apps no longer
--    hold a key to rotate. Left in a custom role's JSONB it would be worse than
--    dead weight: `check_no_escalation` requires the granting caller to hold
--    every permission in the role, and nobody can hold one that is no longer in
--    `perm::ALL` — so the role would become permanently ungrantable. System
--    (preset) roles are re-synced from code at every API boot and need no fixup.
--
--    A plain strip is not enough for a *custom* role, though: env management is
--    a brand-new permission family, so a custom role that could fully manage
--    apps would otherwise end up with no env permission at all, and its members
--    would get 403 on all five new env routes with no indication why. Map the
--    old app-level permission each role already holds onto its env-level
--    counterpart, preserving intent rather than just deleting the retired one.
UPDATE roles
SET permissions = (
    (permissions - 'app:rotate_key')
    || CASE WHEN permissions @> '["app:read"]'::jsonb   THEN '["env:read"]'::jsonb   ELSE '[]'::jsonb END
    || CASE WHEN permissions @> '["app:update"]'::jsonb THEN '["env:create","env:update"]'::jsonb ELSE '[]'::jsonb END
    || CASE WHEN permissions @> '["app:delete"]'::jsonb THEN '["env:delete"]'::jsonb ELSE '[]'::jsonb END
    || CASE WHEN permissions @> '["app:rotate_key"]'::jsonb THEN '["env:rotate_key"]'::jsonb ELSE '[]'::jsonb END
)
-- `?|` alone is not enough: a scalar string such as '"app:read"' also satisfies
-- `?|` (it matches the top-level string against the key list) and would then
-- fail on the `-` operator above, which errors with "cannot delete from
-- scalar". `roles.permissions` has no CHECK enforcing array-ness, so this
-- table-wide UPDATE must not assume every row already conforms.
WHERE jsonb_typeof(permissions) = 'array'
  AND permissions ?| array['app:rotate_key','app:read','app:update','app:delete'];

-- `||` on two jsonb arrays concatenates without deduping, so a role that already
-- held one of the `env:*` strings above (or that maps to the same env
-- permission through two different app permissions) now has a duplicate
-- element. De-duplicate every role's permission array, not just the ones just
-- touched, so this step is self-contained and safe to re-run.
--
-- Same non-array hazard as above: `jsonb_array_length` errors on a non-array
-- input, and this UPDATE runs over every row in the table regardless of
-- whether step 7's first statement touched it.
UPDATE roles
SET permissions = (SELECT jsonb_agg(DISTINCT p) FROM jsonb_array_elements(permissions) AS p)
WHERE jsonb_typeof(permissions) = 'array'
  AND jsonb_array_length(permissions) <> (
    SELECT count(DISTINCT p) FROM jsonb_array_elements(permissions) AS p
);
