-- Reverses up.sql: RESETs the two storage parameters on every leaf partition
-- of all THREE tables (`analytics_events`, `error_events`, `transactions`)
-- that exists at rollback time, back to cluster defaults. Best-effort in the
-- same sense up.sql's forward direction is: any partition created between
-- up.sql running and this rollback already inherited the setting from
-- `create_range_partition` (see its own doc comment), and this resets those
-- too — there is nothing to preserve about "which partitions existed when
-- up.sql ran" specifically.
DO $$
DECLARE
    leaf regclass;
BEGIN
    FOR leaf IN
        SELECT relid FROM pg_partition_tree('analytics_events'::regclass) WHERE isleaf
        UNION ALL
        SELECT relid FROM pg_partition_tree('error_events'::regclass) WHERE isleaf
        UNION ALL
        SELECT relid FROM pg_partition_tree('transactions'::regclass) WHERE isleaf
    LOOP
        EXECUTE format(
            'ALTER TABLE %s RESET (autovacuum_vacuum_scale_factor, autovacuum_vacuum_threshold)',
            leaf
        );
    END LOOP;
END $$;
