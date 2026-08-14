-- Task 10 (guest identity merge, performance regression guards) found a real,
-- unmitigated regression the feature itself introduces: `rewrite_hot_rows`'
-- per-merge `UPDATE` clears the all-visible bit on every heap page it
-- touches, and a merged guest's rows are scattered across the partition
-- (they accumulated over real time, interleaved with everyone else's) — so
-- a small merge can clear the bit on a large fraction of a partition's
-- pages. `rewrite_hot_rows` runs this UPDATE against THREE tables, not two:
-- `analytics_events`, `error_events`, AND `transactions` (step 4 of its
-- six-table rewrite: `UPDATE transactions SET distinct_id = $3 WHERE
-- app_id = $1 AND distinct_id = $2`) — `transactions` gets the identical
-- treatment here for the identical reason.
--
-- On `analytics_events`/`error_events` specifically, every `Index Only Scan`
-- on a damaged partition then falls back to a per-row heap fetch, SILENTLY:
-- the plan node stays `Index Only Scan` (still green against migrations
-- 0028/0031/0039/0040's own guards), only `EXPLAIN (ANALYZE, BUFFERS)`'s
-- `Heap Fetches` line shows it, and nothing before this migration ever
-- forced that scan back down. `transactions` carries no covering index over
-- `distinct_id` today (checked: no index on `transactions` includes it, and
-- no query in `sauron-db::repo` runs `count(DISTINCT distinct_id)` against
-- it) so it has no index-only property to lose — but the SAME visibility-map
-- damage still costs it: every future covering index anyone adds, and
-- `VACUUM`'s own scan-skip optimization today, both depend on the same bit.
-- Tuning it now is cheap and forecloses the same class of regression before
-- anything depends on it, rather than after.
--
-- MEASURED (5,000-row partition, one guest merged, one covering index):
--
--   before the merge:   Heap Fetches: 0     Buffers: shared hit=65
--   after ONE merge:    Heap Fetches: 1974  Buffers: shared hit=1985   (37 of
--     5,000 rows touched, ~0.74% of the partition — ~30-48x the buffer
--     traffic through an IDENTICAL, still-green Index Only Scan node,
--     depending on row layout)
--   re-run, unchanged:  Heap Fetches: 1937  Buffers: shared hit=1948
--
-- This is the exact 152 ms -> 28.96 s class of regression this whole feature
-- exists to prevent (see 2026-08-01-000039's own comment and
-- docs/superpowers/specs/2026-08-12-guest-identity-merge-design.md's "Why
-- this task exists"), reintroduced by the feature itself.
--
-- WHY THE DEFAULT AUTOVACUUM SETTINGS NEVER FIX THIS ON THEIR OWN: the
-- trigger is `autovacuum_vacuum_threshold + autovacuum_vacuum_scale_factor *
-- reltuples`. Defaults (50 + 0.2 * reltuples) need 1,050 dead tuples on a
-- 5,000-row partition; one guest's merge produces dead tuples on the order
-- of TENS (37, measured). The gap does not close as a partition grows — the
-- default trigger scales with `reltuples`, a guest's row count does not, so
-- larger (older, "hot" for longer) partitions are worse off, not better.
-- Nothing about ordinary ingest ever reaches this trigger either: it counts
-- DEAD tuples, and all three tables here are otherwise pure INSERT paths —
-- see the shared reasoning below for why that holds for `transactions` too,
-- not only the two event tables.
--
-- THE FIX: `autovacuum_vacuum_scale_factor = 0.0` removes the
-- table-size-proportional term entirely, leaving a FLAT
-- `autovacuum_vacuum_threshold = 20` dead-tuple trigger, independent of
-- partition size. 20 sits comfortably below "tens" (catches a merge as small
-- as 20 dead tuples) while still requiring more than a stray single-row
-- correction to fire — not zero, not table-size-proportional.
--
-- MEASURED SELF-HEAL: with this exact setting applied to the touched leaf
-- partition after a merge, autovacuum restored `Heap Fetches: 0` within 25s
-- in a lightly loaded test database (`autovacuum_naptime` default 1 minute,
-- `autovacuum_max_workers` default 3). Bounded, not instant — a busier
-- production system contending those worker slots across every other table
-- could take longer — but bounded is the entire point: today this NEVER
-- self-heals, at any partition size, ever.
--
-- WHAT THIS TRADES AWAY, ON PURPOSE: more frequent vacuum passes on these
-- THREE tables (`analytics_events`, `error_events`, `transactions`) than the
-- cluster default would run. Accepted because (a) each pass is cheap —
-- measured 0.65 ms at 5,000 rows, ~28 ms at 200,000 rows, ~262 ms at
-- 1,000,000 rows, against the REAL production index set (~14 indexes) on
-- the two event tables — and (b) the only sources of UPDATE/DELETE on ANY
-- of the three are identity-merge (`sauron_db::identity_merge::
-- rewrite_hot_rows`, which touches all three, not just the two event
-- tables) and sauron-tier/purge maintenance — never ordinary high-volume
-- ingest — so a low, table-size-independent dead-tuple threshold cannot be
-- triggered spuriously by normal traffic, only by the writes that actually
-- damage the visibility map. That shared property (append-only except for
-- identity-merge and tier/purge maintenance) is what makes this safe on all
-- three tables in a way it would not be on a general-purpose, UPDATE-heavy
-- table — it is not specific to the two tables Task 10's index-only guards
-- happen to cover.
--
-- WHY THIS IS A MIGRATION, NOT JUST A CODE CHANGE: storage parameters are
-- DDL. `create_range_partition` (sauron-db/src/repo.rs) is updated
-- separately (same commit) to carry this setting forward onto every NEW
-- partition it creates from now on, for every table it is ever called for
-- (today: all three tables here, since it is shared by every entry in
-- `sauron_tier::TIERED_TABLES`) -- see its own doc comment. This migration
-- is the one-time catch-up for every partition, on all three tables, that
-- ALREADY EXISTS.
--
-- WHY THE PARENTS ARE NOT TOUCHED: measured and confirmed —
--   ALTER TABLE analytics_events SET (autovacuum_vacuum_scale_factor = 0.0);
--   ERROR:  cannot specify storage parameters for a partitioned table
--   HINT:  Specify storage parameters for its leaf partitions instead.
-- Postgres refuses storage parameters on a partitioned PARENT outright — this
-- is not "existing leaves need individual ALTERs as a convenience", the
-- parent categorically cannot hold this setting. Every leaf (including the
-- `_default` partition -- ordinary rows land there in any environment that
-- has not yet had `sauron-tier` pre-create a dated partition for "now")
-- needs its own ALTER, which is what the block below does, for all three
-- tables.
--
-- Enumerated via `pg_partition_tree()` rather than a hardcoded partition-name
-- list: partition names encode a date suffix (`partition_suffix`,
-- sauron-tier/src/layout.rs) that this migration has no reason to know, and a
-- hardcoded list would silently stop covering partitions created after
-- whoever wrote this migration stopped looking. `isleaf` excludes the three
-- parents themselves (which `pg_partition_tree` also returns, with
-- `isleaf = false`) — the exact rows the ALTER above proved will error.
--
-- Safe on a brand-new, unmigrated-past-this-point database too: `analytics_
-- events`/`error_events`/`transactions` always have at least their
-- `_default` partition by migrations 0012/0011/0013 respectively, long
-- before this one runs, but the `DO` block below does not assume any
-- particular count for any of the three — a `FOR` loop over zero rows is a
-- no-op, not an error, so this remains safe even if that ever changed.
--
-- No lock concern beyond the ordinary `ALTER TABLE ... SET` (storage
-- parameter, not a rewrite): unlike migrations 0028/0031/0039/0040's
-- `CREATE INDEX`/`DROP INDEX` on these same partitioned parents, this does
-- NOT touch any parent relation at all and does not require a maintenance
-- window for that reason. Still runs inside this migration's own transaction
-- like everything else here.
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
            'ALTER TABLE %s SET (autovacuum_vacuum_scale_factor = 0.0, autovacuum_vacuum_threshold = 20)',
            leaf
        );
    END LOOP;
END $$;
