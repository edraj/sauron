-- Give `event_users` a flag that says "this distinct_id names a person, not an
-- SDK-minted anonymous token", so that combined active users can count one
-- human once across several apps.
--
-- Nothing on the server can tell the two apart today. Testing the `anon_`
-- prefix was rejected: only the browser SDK mints that shape, and any app may
-- legitimately use it as a real id. So the flag is written explicitly, by
-- `identify()` and by an ingested envelope whose `context.user.id` equals the
-- distinct_id the signal was filed under.
--
-- MUST RUN BEFORE RESTARTING sauron-api AND sauron-ingest.
-- RPM upgrades do not re-run sauron-migrate (packaging/rpm/SETUP.md §11).
-- Without this migration:
--   * GET /v1/projects/{id}/active-users returns 503 schema_migration_required;
--   * the ingest worker logs one ERROR at boot and collects NO identification
--     signal for the lifetime of the process.
-- The second one is NOT recoverable later: the backfill below can only see
-- `properties` and `identities`, so every person first active during an
-- un-migrated window is filed under `active_guest` forever and the split is
-- permanently wrong for those days.
--
-- MAINTENANCE WINDOW. Size it on PAGE LOADS, not on people. The browser SDK
-- re-mints `anon_${uuidv4()}` in memory on every page load and `process_event`
-- calls `touch_event_user` for every non-empty distinct_id, so `event_users`
-- holds roughly one row per page load per browser app — a 5-10x inflation over
-- the real audience. The partial index at the bottom takes a SHARE lock that
-- blocks every `touch_event_user` for the duration of its build.
--
-- REPAIR PATH. Both inputs to the `context_user` rule are client-supplied and
-- ingest authenticates with a public key embedded in browser bundles, so anyone
-- who can read an app's public key can set this flag on any distinct_id in that
-- app. That adds no new class of harm (the same actor can forge events and
-- inflate the counts directly) but the flag is STICKY, and flipping it
-- retroactively moves historical figures from the guest column to the
-- identified one. `identified_source` is what makes a poisoned cohort
-- repairable without touching real identify() rows:
--
--   UPDATE event_users SET identified_at = NULL, identified_source = NULL
--    WHERE app_id = $1 AND identified_source = 'context_user'
--      AND identified_at > $2;
--
-- Enum-like column as TEXT + CHECK, never a custom SQL type — house rule.
ALTER TABLE event_users ADD COLUMN identified_at TIMESTAMPTZ;
ALTER TABLE event_users ADD COLUMN identified_source TEXT
  CHECK (identified_source IN ('identify', 'context_user', 'backfill'));

-- Not optional. `identities` carries only UNIQUE (app_id, alias_id);
-- `distinct_id` is unindexed, so the EXISTS leg of the backfill below has no
-- support at all and degrades to a per-row scan of a table nobody has ever
-- read from.
CREATE INDEX identities_app_distinct_idx ON identities (app_id, distinct_id);

-- The first read of `identities` in the product's history — it has been
-- write-only dead storage since migration 1. Two legs because `identify()`
-- merges traits into `properties` and writes an `identities` row only when the
-- SDK supplied a non-empty `anonymous_id` (browser only).
--
-- This under-merges by design: an identify() with empty traits and no anonymous
-- id (the Node/Python/C#/Flutter shape) leaves no trace here, so those users
-- stay app-local until their next identify() re-stamps them through the live
-- write path. Under-merging is the fail-closed direction.
--
-- The two sentinels below are read by
-- `sauron-db/tests/env_scoping.rs::migration_000038_backfills_only_rows_with_traits_or_an_alias`,
-- which re-runs exactly this statement against seeded rows. Migrations execute
-- against an empty database, so this statement's own run back-fills nothing and
-- proves nothing. Do not remove or reword the sentinels.
-- BACKFILL-BEGIN
UPDATE event_users eu
   SET identified_at = eu.first_seen, identified_source = 'backfill'
 WHERE eu.identified_at IS NULL
   AND (eu.properties <> '{}'::jsonb
        OR EXISTS (SELECT 1 FROM identities i
                    WHERE i.app_id = eu.app_id AND i.distinct_id = eu.distinct_id));
-- BACKFILL-END

-- Partial, because every read of this flag tests `IS NOT NULL` only — the join
-- in `active_users_combined` carries `AND eu.identified_at IS NOT NULL` as a
-- join condition, so the index only ever has to cover identified rows.
CREATE INDEX event_users_app_identified_idx
  ON event_users (app_id, distinct_id) WHERE identified_at IS NOT NULL;
