-- Reverses up.sql.
--
-- The order of the two parts is NOT load-bearing and no earlier claim that it
-- was survives: recreating the index has no bearing on reading `pg_attribute`,
-- and the compression reset has none on the index. The one real ordering
-- constraint lives inside the `DO` block below, where `cols` must be captured
-- from the parent BEFORE the loop resets the parent -- which it is, since the
-- `SELECT ... INTO` completes before the `FOR` begins.

-- Restores migration 0011's definition of the index (which is itself migration
-- 0004's), with `IF NOT EXISTS` added so this file matches up.sql's
-- re-runnability -- up.sql is fully idempotent (`SET COMPRESSION` is
-- catalogue-assignment, `DROP INDEX IF EXISTS` skips), and without the guard a
-- second `diesel migration revert` on the same database aborts here with
-- `relation "error_events_app_device_idx" already exists` (verified on
-- PostgreSQL 16.11) before the reset block below is ever reached.
--
-- Built on the partitioned parent, so it propagates to every child
-- synchronously inside this transaction, holding locks on the parent and each
-- partition for the duration of the build -- on a full-size `error_events`
-- this is the expensive half of the rollback, not the compression reset.
CREATE INDEX IF NOT EXISTS error_events_app_device_idx ON error_events (app_id, device_key);

-- Resets the per-column compression method back to `default` -- i.e. back to
-- `attcompression = ''`, "follow the `default_toast_compression` GUC", which is
-- the state every column on this table was in before up.sql ran. Verified on
-- PostgreSQL 16.11 that `SET COMPRESSION default` restores exactly that
-- catalogue state, rather than pinning `pglz` explicitly.
--
-- WHAT THIS CANNOT UNDO: values already written as lz4 stay lz4. The
-- compression method lives in the datum's header, not in the column
-- definition, so those rows keep decompressing correctly forever -- a rollback
-- is safe, it simply does not recompress history (for the same reason up.sql
-- did not compress history). There is no data-loss or unreadability hazard in
-- either direction.
--
-- Same partition-tree walk as up.sql (parent + every leaf), and for the same
-- verified reason: resetting the parent alone would leave every existing
-- partition pinned to lz4. Partitions created between up.sql and this rollback
-- inherited lz4 from the parent, and are reset here too -- there is nothing to
-- preserve about "which partitions existed when up.sql ran", the same
-- best-effort stance migration 0060's down.sql takes.
--
-- The column list is derived from the PARENT's catalogue rather than repeated
-- as a literal, so it cannot drift from up.sql's list: it resets precisely the
-- columns that are currently marked lz4 ('l'). A second run finds none, so
-- `cols` comes back NULL and the block returns without touching anything --
-- which, together with the `IF NOT EXISTS` above, makes this whole file
-- re-runnable.
DO $$
DECLARE
    rel  regclass;
    col  text;
    cols text[];
BEGIN
    SELECT array_agg(a.attname::text ORDER BY a.attnum)
      INTO cols
      FROM pg_attribute a
     WHERE a.attrelid = 'error_events'::regclass
       AND a.attnum > 0
       AND NOT a.attisdropped
       AND a.attcompression = 'l';

    IF cols IS NULL THEN
        RETURN;
    END IF;

    FOR rel IN SELECT relid FROM pg_partition_tree('error_events'::regclass)
    LOOP
        FOREACH col IN ARRAY cols
        LOOP
            EXECUTE format(
                'ALTER TABLE %s ALTER COLUMN %I SET COMPRESSION default', rel, col
            );
        END LOOP;
    END LOOP;
END $$;

-- ===========================================================================
-- LIVE INDEX INVENTORY on `error_events` as of migration 0064, replayed from
-- the migration set into a clean PostgreSQL 16 and read back from
-- `pg_indexes`. Recorded here because up.sql's Part 2 rests on it.
--
--   error_events_pkey1                  UNIQUE (id, occurred_at)          0011
--   error_events_project_idx            (app_id, occurred_at DESC)        0001/0011
--   error_events_distinct_idx           (app_id, distinct_id,
--                                        occurred_at DESC)                0001/0011
--   error_events_app_session_idx        (app_id, session_id)              0004/0011
--   error_events_app_device_idx         (app_id, device_key)              0004/0011  <- dropped by 0065
--   error_events_tags_gin               GIN (tags jsonb_path_ops)         0018
--   error_events_app_screen_time_idx    (app_id, screen, occurred_at DESC)
--                                        WHERE screen IS NOT NULL         0020
--   error_events_issue_time_id_idx      (issue_id, occurred_at DESC,
--                                        id DESC)                         0025
--   error_events_app_level_time_idx     (app_id, level, occurred_at DESC) 0025
--   error_events_app_release_time_idx   (app_id, release,
--                                        occurred_at DESC)                0025
--   error_events_issue_env_time_idx     (issue_id, environment_id,
--                                        occurred_at DESC)
--                                        INCLUDE (distinct_id)            0031
--   error_events_app_workflow_idx       (app_id, workflow_name,
--                                        occurred_at DESC)
--                                        WHERE workflow_id IS NOT NULL    0032
--   error_events_app_env_time_users_idx (app_id, environment_id,
--                                        occurred_at DESC)
--                                        INCLUDE (distinct_id)            0040
--   error_events_app_device_env_idx     (app_id, device_key,
--                                        environment_id, occurred_at)     0053
--   error_events_app_distinct_env_idx   (app_id, distinct_id,
--                                        environment_id, occurred_at)     0055
--
-- Considered and KEPT (the near-misses, so the next audit does not re-derive
-- them):
--   * `error_events_distinct_idx` vs `error_events_app_distinct_env_idx` --
--     NOT a prefix. They diverge at column 3 (`occurred_at DESC` vs
--     `environment_id`). The narrow one is the only index that can produce
--     `ORDER BY occurred_at DESC` for a person UNSCOPED by environment; the
--     wide one can only do so once `environment_id` is pinned by equality.
--     Both are load-bearing.
--   * `error_events_project_idx` vs `error_events_app_env_time_users_idx` --
--     NOT a prefix; `environment_id` sits between `app_id` and `occurred_at`
--     in the wider one, so it cannot serve app-wide time-ordered reads.
--   * `error_events_issue_time_id_idx` vs `error_events_issue_env_time_idx` --
--     NOT a prefix, same reason, on `issue_id`.
--   * `error_events_app_session_idx` -- nothing wider leads with
--     `(app_id, session_id)`; it is the only path for the session drill-down.
--   * The two partial indexes (`..._app_screen_time_idx`,
--     `..._app_workflow_idx`) and the GIN are not comparable to anything here:
--     a partial index is only substitutable by an index whose predicate is
--     implied by it, and no other index on this table carries a predicate.
-- ===========================================================================
