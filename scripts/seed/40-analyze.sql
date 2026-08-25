-- Statistics for the planner, plus visibility-map maintenance.
--
-- `PARALLEL 0` is REQUIRED, not defensive. Parallel vacuum workers allocate a
-- shared-memory segment per worker, and on this deployment that exhausts the
-- container's `/dev/shm`, failing with "could not resize shared memory segment
-- ... No space left on device". The serial path succeeds on exactly the same
-- data.
--
-- ANALYZE matters more than usual here: 10M rows have just landed in tables
-- whose statistics still describe ~212k, and every plan the dashboard picks
-- comes from those statistics. Skipping this leaves the planner choosing shapes
-- for a dataset that no longer exists.
--
-- VACUUM cannot run inside a transaction or a DO block, so the statements are
-- generated and executed with \gexec.

\set ON_ERROR_STOP on
SET TIME ZONE 'UTC';

\echo 'Vacuuming event partitions (serial — parallel workers exhaust /dev/shm)...'

SELECT format('VACUUM (ANALYZE, PARALLEL 0) public.%I;', c.relname)
FROM pg_inherits i
JOIN pg_class c ON c.oid = i.inhrelid
JOIN pg_class p ON p.oid = i.inhparent
WHERE p.relname IN ('analytics_events', 'error_events', 'transactions')
ORDER BY c.relname
\gexec

\echo 'Vacuuming dimension and rollup tables...'

VACUUM (ANALYZE, PARALLEL 0) public.sessions;
VACUUM (ANALYZE, PARALLEL 0) public.devices;
VACUUM (ANALYZE, PARALLEL 0) public.event_users;
VACUUM (ANALYZE, PARALLEL 0) public.issues;
VACUUM (ANALYZE, PARALLEL 0) public.device_environments;
VACUUM (ANALYZE, PARALLEL 0) public.event_user_environments;

-- The parents carry their own (empty) statistics; analyzing them refreshes the
-- inheritance-wide estimates the planner uses for a query that spans partitions.
ANALYZE public.analytics_events;
ANALYZE public.error_events;
ANALYZE public.transactions;
