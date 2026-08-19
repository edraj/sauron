-- 0065: Tier 0 storage/CPU changes on `error_events`, the hottest-write table
-- in the schema. Two independent changes, both chosen because they are
-- SEMANTICALLY INERT: no query returns a different row, a different value, or
-- a different count after this migration than before it. Everything here is
-- either a storage encoding (TOAST compression method) or a duplicate access
-- path.
--
-- ===========================================================================
-- PART 1 -- lz4 TOAST compression on the fat jsonb/text columns
-- ===========================================================================
--
-- Nothing in the migration set has ever set a per-column COMPRESSION, so every
-- column on this table follows the `default_toast_compression` GUC, which is
-- `pglz` (verified: `SHOW default_toast_compression` on the deployed
-- postgres:16 image -- docker-compose.yml:10 -- returns `pglz`).
--
-- READ THIS BEFORE QUOTING THIS MIGRATION IN A DISK NUMBER: lz4 IS NOT A DISK
-- WIN. It is a CPU/latency win that is roughly disk-NEUTRAL, and on some
-- payload shapes it is a disk REGRESSION. Measured on PostgreSQL 16.11, three
-- payload shapes, 20-30k rows each, one jsonb column, pglz table vs lz4 table:
--
--   payload shape                     disk (lz4 vs pglz)   INSERT wall time
--   --------------------------------  -------------------  ----------------
--   realistic Dart stacktrace,        65MB -> 64MB          5452 -> 5028 ms
--     18-42 varied frames, ~12KB raw    -0.5% (a wash)        -7.8%
--   pathologically repetitive,        22MB -> 31MB          3412 -> 2480 ms
--     32 near-identical frames          +39.9% WORSE          -27.3%
--   high-entropy (md5 filler)        264MB -> 179MB        13691 -> 8937 ms
--                                       -32.3% better         -34.7%
--
-- pglz beats lz4 on ratio when a datum repeats itself heavily, and loses badly
-- when it does not (pglz's default strategy gives up entirely if it finds no
-- match in the first 1KB, storing the datum raw -- that is the 264MB row).
-- Real stack traces sit in the middle, which is why the realistic shape is a
-- wash on bytes.
--
-- REPLICATION ATTEMPT (2026-08-18, PostgreSQL 16.11, 20k rows per shape, one
-- jsonb column, fresh UNCOMPRESSED datums fed to both sides -- note that
-- `INSERT ... SELECT` from an already-toasted source copies the source's
-- compressed datum verbatim and silently measures nothing, which is the trap
-- this re-run fell into first). The payload generator behind the table above
-- is recorded NOWHERE, so the numbers below are a reconstruction rather than a
-- re-run -- and that is itself a finding: as written, those three rows can be
-- neither re-derived nor falsified by a later reader. What the reconstruction
-- measured:
--
--   * realistic (48 varied frames, 12.9KB raw): 83,951,616 -> 84,066,304 bytes
--     total relation size, +0.14%. REPRODUCES the "a wash" row. INSERT 8,794
--     -> 4,541 ms median over 3 passes.
--   * repetitive (32 near-identical frames): 10,032 kB -> 9,448 kB, i.e. lz4
--     5.8% SMALLER. Does NOT reproduce "+39.9% WORSE" -- it inverts the sign.
--   * high-entropy (md5-hex filler): 278MB -> 278MB, mean datum 13,458 ->
--     13,135 bytes (-2.4%). Does NOT reproduce "-32.3% better". lz4 is LZ77
--     with no entropy-coding stage, so a 4-bit-per-byte hex alphabet is very
--     nearly incompressible to it as well.
--
-- The pglz MECHANISM described just above DOES reproduce exactly: on the
-- high-entropy shape all 20,000 pglz rows stored raw
-- (`pg_column_compression()` NULL on every one of them). So the mechanism is
-- right and the realistic row is right; treat the other two ratio cells as
-- unverified until whoever produced them commits the generator.
--
-- Where lz4 actually pays, measured on the realistic shape above: a full scan
-- that decompresses every `stacktrace` datum ran 117ms under pglz and 43ms
-- under lz4 -- 2.7x faster, repeatable across three warm passes each. That is
-- the occurrence read path. REPLICATED, same run as above: 20k rows of the
-- realistic shape scanned with `SELECT sum(jsonb_array_length(c))` (forces a
-- full detoast without paying for jsonb->text serialisation, which otherwise
-- swamps the signal) ran 141ms under pglz and 71ms under lz4 across three warm
-- passes -- 1.98x, same direction and same order of magnitude. The INSERT column above is the ingest write path.
-- So this migration's honest claim is "reads and writes of large error
-- payloads get cheaper in CPU, at no meaningful cost in bytes" -- and Tier 0's
-- DISK win comes from Part 2 (the dropped index), not from here.
--
-- WHICH COLUMNS. Only the ones that can plausibly carry a multi-KB value, i.e.
-- the ones the toaster would actually pick. Compression is not applied
-- per-column on a whim: it engages only once a tuple exceeds
-- TOAST_TUPLE_THRESHOLD (~2KB), and then the toaster works down the row's
-- LARGEST attributes. `fingerprint`, `level`, `exception_type`, `release`,
-- `distinct_id`, `session_id`, `device_key`, `screen`, `ip_address`,
-- `symbolication_status`, `title`, `culprit`, `workflow_id`, `workflow_name`
-- and `guest_alias` are short identifiers or short human strings; they are
-- never the largest attribute in an `error_events` row that has a stacktrace
-- and breadcrumbs in it. Setting a compression method on them would be inert
-- noise, so they are deliberately left alone.
--
-- THE THRESHOLD IS THE WHOLE STORY FOR SMALL ROWS. Compression engages only
-- once the RAW tuple exceeds TOAST_TUPLE_THRESHOLD, which is 2,032 bytes on a
-- stock 8KB-page build -- not 2,048. (Measured on PostgreSQL 16.11: an
-- `(int, text)` row stops storing its text raw somewhere between a 2,004-byte
-- and a 2,024-byte payload.) Below that the toaster never fires, nothing is
-- compressed under either algorithm (`pg_column_compression()` returns NULL on
-- every column), and lz4 vs pglz is a bit-for-bit no-op.
--
-- That used to make Part 1 unmeasurable against `bins/crebain`: while its
-- generator emitted a hardcoded 2-frame stacktrace, the trace serialised to
-- 258 bytes and the whole event tuple to ~1.6KB, under the threshold.
-- THAT IS NO LONGER TRUE, and a reader planning a BEFORE/AFTER should not act
-- on it. The generator now carries `--stack-depth`, defaulting to
-- `generator::DEFAULT_STACK_DEPTH = 24` frames, and `generator::pad_frames`
-- emits library frames with long module paths plus a synthesised `abs_path`
-- (bins/crebain/src/generator.rs). Reconstructed from that source and
-- measured, the `stacktrace` column ALONE serialises to 6,021 bytes at the
-- default depth -- roughly 3x the threshold before `context`, `contexts`,
-- `extra` and `breadcrumbs` are even counted. So a default-configured crebain
-- run DOES exercise Part 1; a run passing `--stack-depth 2` does not.
--
-- ONLY AFFECTS NEWLY WRITTEN VALUES. `SET COMPRESSION` is a catalog-only
-- change: it does NOT rewrite the table and it does NOT recompress a single
-- existing datum. Rows already on disk keep their pglz datums and stay
-- readable forever (the compression method is recorded per-datum, not
-- per-column, so a column can hold a mix indefinitely). The effect therefore
-- accrues only to data written AFTER this migration -- which for a benchmark
-- means the BEFORE/AFTER comparison must load fresh data on each side, not
-- re-measure the same rows.
--
-- PARTITION PROPAGATION -- VERIFIED EMPIRICALLY, NOT ASSUMED.
-- `error_events` is a RANGE-partitioned parent (migration 0011). Measured on
-- PostgreSQL 16.11 (the same `postgres:16` image the stack runs), by creating
-- a partitioned parent with two partitions, running
-- `ALTER TABLE parent ALTER COLUMN c SET COMPRESSION lz4`, and reading
-- `pg_attribute.attcompression` for the parent and every child:
--
--   * EXISTING partitions (including the DEFAULT partition): NOT propagated.
--     They stayed at `attcompression = ''` (= follow the GUC). Unlike
--     `ADD COLUMN`, `SET COMPRESSION` does not recurse to already-attached
--     children -- so a migration that only touched the parent would leave
--     every byte of currently-partitioned data on pglz.
--   * FUTURE partitions: PROPAGATED. A child created afterwards with
--     `CREATE TABLE ... PARTITION OF parent` came out with
--     `attcompression = 'l'` (lz4) -- the child's column definitions are
--     copied from the parent at creation time. Re-verified with the exact
--     statement shape `repo::create_range_partition` emits, including its
--     `WITH (autovacuum_vacuum_scale_factor = ..., ...)` clause: still
--     inherited.
--
-- So this migration walks the whole partition tree explicitly (parent + every
-- leaf), and the partition-creation code path needs NO change. That is worth
-- stating loudly because it is the OPPOSITE of migration 0060's situation --
-- and the contrast is sharper than "not inherited". Storage PARAMETERS
-- (`autovacuum_*`) cannot sit on a partitioned parent AT ALL: re-measured
-- here, `ALTER TABLE error_events SET (autovacuum_vacuum_scale_factor = 0.0)`
-- fails with `ERROR: cannot specify storage parameters for a partitioned
-- table`, exactly as 0060's own header records. There is therefore no parent
-- setting for a new partition to inherit, which is why
-- `repo::create_range_partition` (crates/sauron-db/src/repo.rs) has to
-- re-apply them on every partition it creates and why 0060 walks leaves only
-- (`WHERE isleaf`). Per-column COMPRESSION, by contrast, does live on the
-- parent and IS inherited, so setting it on the parent
-- here is sufficient for everything `sauron-tier` creates from now on. The one
-- other way a partition could dodge inheritance -- building a standalone table
-- and `ALTER TABLE ... ATTACH PARTITION`-ing it -- does not occur anywhere in
-- this repo (grepped: zero occurrences of `ATTACH PARTITION` in any .rs/.sql).
--
-- REQUIRES a server built `--with-lz4`. Both the deployed `postgres:16` image
-- and the Fedora/RHEL `postgresql-server` packages the RPM targets are. If
-- some server is not, this migration fails loudly with
-- `ERROR: compression method lz4 not supported` -- which is the correct
-- outcome; a `DO`-block guard that silently skipped would leave the cluster
-- claiming a migration succeeded while none of the storage changed.
--
-- LOCKING. `SET COMPRESSION` takes ACCESS EXCLUSIVE on each relation it names,
-- for the duration of a catalog update only -- no rewrite, no scan, so each
-- lock is held for microseconds rather than for a table-sized rewrite. It is
-- still one lock per partition inside this migration's single transaction, so
-- it wants the same quiet moment migrations 0053/0055 asked for on this table.
DO $$
DECLARE
    rel  regclass;
    col  text;
    -- The fat columns, in table order. Single source of truth for this
    -- migration: the same list is applied to the parent (so FUTURE partitions
    -- inherit it) and to every existing leaf (which inherit nothing).
    cols text[] := ARRAY[
        'message',
        'exception_value',
        'stacktrace',
        'breadcrumbs',
        'context',
        'tags',
        'event_user',
        'sdk',
        'stacktrace_symbolicated',
        'debug_meta',
        'contexts',
        'extra'
    ];
BEGIN
    -- `pg_partition_tree` returns the parent itself plus every partition, so
    -- this single loop covers both halves of the propagation finding above:
    -- the parent (for future partitions) and each existing leaf (for the data
    -- already partitioned), INCLUDING `error_events_default`, which holds the
    -- longest-lived rows in the system because `list_child_partitions`
    -- excludes it from tiering.
    FOR rel IN SELECT relid FROM pg_partition_tree('error_events'::regclass)
    LOOP
        FOREACH col IN ARRAY cols
        LOOP
            EXECUTE format(
                'ALTER TABLE %s ALTER COLUMN %I SET COMPRESSION lz4', rel, col
            );
        END LOOP;
    END LOOP;
END $$;

-- ===========================================================================
-- PART 2 -- drop one index that is a strict prefix of another
-- ===========================================================================
--
-- `error_events_app_device_idx (app_id, device_key)` dates from migration 0004
-- and was recreated verbatim by the partitioning rebuild in 0011. Migration
-- 0053 then added `error_events_app_device_env_idx
-- (app_id, device_key, environment_id, occurred_at)` for the environment-scoped
-- device LATERALs -- and did not retire the narrower one, whose key is a strict
-- prefix of the wider one's, same column order, same (default ASC/NULLS LAST)
-- direction on both shared columns, neither index partial.
--
-- VERIFIED, not assumed, three ways:
--
--  1. LIVE INDEX SET. Every migration's CREATE/DROP INDEX was replayed in order
--     into a fresh PostgreSQL 16 database (all 64 migrations, clean) and the
--     result read back from `pg_indexes`. `error_events` carries the PK
--     (`error_events_pkey1`) plus 14 secondary indexes. A catalogue query over
--     `pg_index` for "index A's key column list is a strict prefix of index B's,
--     same column order, same DESC/NULLS flags, same (null) predicate, both
--     btree" returns EXACTLY ONE pair across all 14:
--     `error_events_app_device_idx` inside `error_events_app_device_env_idx`.
--     Nothing else on this table is subsumed -- see down.sql's inventory, which
--     records the full live set and the near-misses that were kept.
--
--  2. NO QUERY DEPENDS ON THE NARROWER SHAPE. The call sites that filter
--     `error_events` on `device_key` are `repo::list_devices` /
--     `repo::list_device_groups` (the membership
--     `EXISTS (SELECT 1 FROM error_events ee WHERE ee.app_id=$1 AND
--     ee.device_key = devices.device_key [AND ee.environment_id = $n])`) and
--     `repo::errors_for_device` (`app_id =` + `device_key =` [+ env],
--     `ORDER BY occurred_at DESC LIMIT n`), plus the `deviceKey` dimension in
--     `sauron-query`'s catalog, which lowers to that same correlated EXISTS.
--     A FOURTH shape, missed by this audit's first pass and recorded here so
--     the inventory is complete: `repo::device_last_distinct_id_join`
--     (crates/sauron-db/src/repo.rs), the LATERAL behind the "last
--     distinct_id" column of `list_devices` / `list_device_groups` /
--     `get_device` -- `SELECT distinct_id, occurred_at FROM error_events lee
--     WHERE lee.app_id = $1 AND lee.device_key = d.device_key AND
--     lee.distinct_id IS NOT NULL [AND lee.environment_id = $n]`, wrapped in a
--     `UNION ALL ... ORDER BY occurred_at DESC LIMIT 1`. It is the shape most
--     exposed to this drop because it wants `occurred_at` ordering, and it is
--     the one helped most: the wider index carries `occurred_at` in its key
--     and the narrow one never did.
--     All of them lead with `app_id, device_key` equality, which the wider
--     index serves by prefix; the env-scoped and ordered variants are served
--     STRICTLY BETTER by it (`environment_id` and `occurred_at` are in the key,
--     so they need no heap recheck and no sort -- that is what 0053 was for).
--     No test asserts on this index name and no plan-guard test names it
--     (grepped repo-wide: the only non-migration hits are prose). Two PROSE
--     references do name it and both go stale the moment this runs:
--     `crates/sauron-query/src/catalog.rs`'s `deviceKey` dimension, updated in
--     this same change to name `error_events_app_device_env_idx`; and
--     `repo::list_device_groups`' doc comment
--     (crates/sauron-db/src/repo.rs), whose "Each probe is an index probe"
--     list still names the dropped index -- that one is OUTSTANDING and must
--     be repointed at `error_events_app_device_env_idx` too.
--
--  3. MEASURED PLAN EQUIVALENCE. On a 300k-row replica of the two indexes'
--     exact key shapes, all three predicate shapes above were EXPLAIN ANALYZEd
--     with the narrow index present and again after dropping it. Identical plan
--     shapes on both sides -- Index Only Scan for the EXISTS probe, Bitmap
--     Index Scan + Sort for the unscoped ordered read, Index Scan Backward for
--     the env-scoped one -- with the wider index substituted in. No sequential
--     scan appeared, and the env-scoped shape had already been ignoring the
--     narrow index while it existed.
--
-- Dropping an index cannot change a result set, so this stays inside Tier 0's
-- "semantically inert" boundary. What it buys is one fewer B-tree insert per
-- persisted error event on the table that takes the most inserts in the
-- system, plus the disk the duplicate index occupied across every partition --
-- and unlike Part 1, that disk saving is immediate and does not wait for fresh
-- data to be written.
--
-- DROP INDEX on a partitioned parent cascades to the matching index on every
-- child, synchronously, inside this transaction (CONCURRENTLY is unavailable
-- inside a migration transaction -- the same constraint migrations 0028, 0031,
-- 0040, 0053 and 0055 each documented on this table).
DROP INDEX IF EXISTS error_events_app_device_idx;
