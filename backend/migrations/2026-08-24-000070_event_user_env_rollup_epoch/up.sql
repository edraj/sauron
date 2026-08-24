-- The instant the persons rollup's cutoff becomes trustworthy.
--
-- `person_env_backfill::backfill_app` aggregates raw signals strictly BEFORE a
-- cutoff, on the assumption that the live write path (`batch::bump_person_envs`)
-- has counted everything from that cutoff onward. The two sets are disjoint only
-- if the cutoff is the instant the live path STARTED counting. `backfill_all`
-- used `Utc::now()` instead, so every signal ingested between the rollup landing
-- and an operator running the backfill was counted twice.
--
-- The device twin got this right at birth: migration 59 created
-- `device_env_rollup_epoch` in the same migration that created the rollup, so
-- the stamp and the live path's first write are simultaneous. Migration 56
-- created `event_user_environments` and stamped nothing, and that instant is not
-- recoverable after the fact -- `__diesel_schema_migrations.run_on` is declared
-- NAIVE `Timestamp` by diesel, so its UTC meaning depends on the session
-- TimeZone in effect when it ran.
--
-- So this migration MAKES the stamp true instead of recovering it, by clearing
-- the counts accumulated before it. See the DELETE below.
--
-- One row, forever: the boolean-PK-with-CHECK idiom makes a second row
-- impossible. Same shape as `device_env_rollup_epoch`.
CREATE TABLE event_user_env_rollup_epoch (
    only_row   boolean     PRIMARY KEY DEFAULT true CHECK (only_row),
    started_at timestamptz NOT NULL DEFAULT now()
);
INSERT INTO event_user_env_rollup_epoch DEFAULT VALUES;

-- What makes the stamp above honest rather than merely later.
--
-- For an app with no `event_user_env_backfill` marker, `event_user_environments`
-- holds ONLY live-path bumps, and NOTHING READS IT: both readers
-- (`repo::list_persons` and `repo::count_persons`) are gated on
-- `person_env_backfill::is_backfilled`, which is false for exactly these apps.
-- The rows are unobservable, so discarding them changes no answer any endpoint
-- can currently give -- while leaving them would leave the epoch above lying
-- about a table that already contains pre-epoch counts.
--
-- Scoped to unbackfilled apps precisely: an app whose backfill already ran is
-- being READ from this table, and its rows must survive untouched.
--
-- Deliberately NOT `TRUNCATE`: that would take an ACCESS EXCLUSIVE lock on a
-- table the ingest write path bumps continuously, and would take out the
-- already-backfilled apps this must not touch.
DELETE FROM event_user_environments
WHERE app_id NOT IN (SELECT app_id FROM event_user_env_backfill);
