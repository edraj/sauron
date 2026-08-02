-- The backfill is not recoverable from this down, but it IS re-derivable: it
-- reads only `event_users.properties` and the `identities` table, neither of
-- which this file touches. Re-running up.sql reconstructs it.
DROP INDEX IF EXISTS event_users_app_identified_idx;
DROP INDEX IF EXISTS identities_app_distinct_idx;
ALTER TABLE event_users DROP COLUMN IF EXISTS identified_source;
ALTER TABLE event_users DROP COLUMN IF EXISTS identified_at;
