-- Daily RANGE partitions covering the seed window.
--
-- `analytics_events`, `error_events` and `transactions` are partitioned by day
-- on `occurred_at`. Only 32 partitions existed before this ran (2026-07-15 →
-- 2026-08-17, with 08-05 and 08-06 missing), so a 90-day window needs the rest
-- created or every generated row lands in the DEFAULT partition — which is both
-- a correctness trap (partition pruning stops working) and invisible unless you
-- go looking, since the insert still succeeds.
--
-- Idempotent: partition names are date-derived and match the scheme
-- `repo::create_range_partition` uses, so `IF NOT EXISTS` skips the ones that
-- are already there rather than colliding with them.

\set ON_ERROR_STOP on
SET TIME ZONE 'UTC';

DO $seed$
DECLARE
  win_start date := DATE '2026-05-27';
  win_end   date := DATE '2026-08-25';  -- exclusive
  tbl       text;
  d         date;
  made      int  := 0;
  existed   int  := 0;
  part      text;
BEGIN
  FOREACH tbl IN ARRAY ARRAY['analytics_events', 'error_events', 'transactions'] LOOP
    d := win_start;
    WHILE d < win_end LOOP
      part := tbl || '_' || to_char(d, 'YYYY_MM_DD');
      IF to_regclass('public.' || part) IS NULL THEN
        -- The storage settings are NOT cosmetic. Migration 60 exists to put
        -- them on every leaf of all three tables, and a partition created
        -- without them silently diverges from every worker-created one.
        EXECUTE format(
          'CREATE TABLE %I PARTITION OF %I FOR VALUES FROM (%L) TO (%L) '
          'WITH (autovacuum_vacuum_scale_factor = 0.0, autovacuum_vacuum_threshold = 20)',
          part, tbl,
          (to_char(d, 'YYYY-MM-DD') || ' 00:00:00+00')::timestamptz,
          (to_char(d + 1, 'YYYY-MM-DD') || ' 00:00:00+00')::timestamptz
        );
        made := made + 1;
      ELSE
        existed := existed + 1;
      END IF;
      d := d + 1;
    END LOOP;
  END LOOP;
  RAISE NOTICE 'partitions: % created, % already present', made, existed;
END
$seed$;

-- Proof rather than assertion: every day in the window must now have a real
-- partition on all three tables (3 x 90 = 270).
SELECT p.relname AS parent, count(*) AS partitions_in_window
FROM pg_inherits i
JOIN pg_class c ON c.oid = i.inhrelid
JOIN pg_class p ON p.oid = i.inhparent
WHERE p.relname IN ('analytics_events', 'error_events', 'transactions')
  AND c.relname !~ 'default$'
GROUP BY p.relname
ORDER BY p.relname;
