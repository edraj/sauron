-- Cold-data restore: the marker that makes a restore exactly reversible, and
-- the job row that tracks one in flight.
--
-- Restored rows go back into the LIVE table. They are inserted into the
-- partitioned parent, so Postgres routes each row to whichever partition covers
-- its occurred_at — an explicit child if one still exists, otherwise
-- `<table>_default`. No partition is created and none is re-attached.
--
-- Why not re-create the explicit partition instead: `CREATE TABLE ... PARTITION
-- OF` scans the DEFAULT partition and FAILS if it holds any row in the new
-- range, and that is exactly where late arrivals for an already-dropped cold
-- range land. Working around it means an atomic move-rows-then-attach dance, and
-- then dropping that partition at expiry would destroy the late arrivals it
-- absorbed — rows that were never in Parquet and exist nowhere else. Inserting
-- into the parent has neither problem.
--
-- `restored_pin_id` is what makes expiry exact. Without it, un-restoring means
-- "delete the rows in this range that came from Parquet", which is
-- indistinguishable from "delete the late arrivals that never made it to
-- Parquet" — a silent data-loss bug wearing a cleanup costume. With it, expiry
-- is `DELETE ... WHERE restored_pin_id = $1`, which can only ever remove rows
-- this feature itself inserted.
--
-- It also makes a crashed restore safely retryable: a re-claimed job deletes its
-- own partial output by pin id before re-inserting, so a mid-insert crash cannot
-- leave duplicated rows behind.

-- ADD COLUMN with no DEFAULT is catalog-only on a partitioned parent — it
-- rewrites nothing and does not lock out writers for the length of a table
-- scan. See the note in 2026-07-29-000030_error_event_title_culprit.
ALTER TABLE error_events     ADD COLUMN restored_pin_id UUID;
ALTER TABLE analytics_events ADD COLUMN restored_pin_id UUID;
ALTER TABLE transactions     ADD COLUMN restored_pin_id UUID;

-- Deliberately NO foreign key to tier_pins, and deliberately NO index.
--
-- No FK: these three tables carry the product's highest-volume writes. A
-- reference from them to tier_pins makes every ingested row pay a referential
-- integrity check for a column that is NULL on essentially all of them.
--
-- No index: the only query against this column is the expiry delete, and it is
-- always issued with the pin's time range alongside the pin id
-- (`restored_pin_id = $1 AND occurred_at >= $2 AND occurred_at < $3`). The range
-- predicate prunes to the handful of partitions the restore actually touched, so
-- the scan is bounded by the restored range rather than by the table. That is
-- worth more than an index here: `CREATE INDEX` on a partitioned parent builds
-- one on every existing child, each a full scan under lock, which on a
-- production-sized deployment is a long outage bolted onto a migration.

CREATE TABLE restore_jobs (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    -- Constrained to the tiered set so a job can never name an arbitrary
    -- relation. The executor interpolates this into SQL (a table name cannot be
    -- a bind parameter), and this CHECK plus a Rust-side allowlist are the two
    -- things that make that safe.
    table_name     TEXT NOT NULL
                     CHECK (table_name IN ('error_events','analytics_events','transactions')),
    -- NULL means every app in the range. Cold Parquet is hive-partitioned by
    -- app_id, so a single-app restore reads far less.
    app_id         UUID,
    -- Half-open [range_start, range_end), same convention as tier_pins and
    -- bucket_bounds().
    range_start    TIMESTAMPTZ NOT NULL,
    range_end      TIMESTAMPTZ NOT NULL,
    status         TEXT NOT NULL DEFAULT 'queued'
                     CHECK (status IN ('queued','running','succeeded','failed','cancelled')),
    -- The pin created for this restore. ON DELETE SET NULL so the job history
    -- survives the pin being purged at expiry — the record that a restore
    -- happened outlives the restore itself.
    pin_id         UUID REFERENCES tier_pins(id) ON DELETE SET NULL,
    -- Copied out of the request so the job row alone says how long the restored
    -- data was meant to live, even after pin_id has been nulled.
    pin_expires_at TIMESTAMPTZ NOT NULL,
    -- Counted from Parquet before the insert, so progress has a denominator.
    rows_estimated BIGINT NOT NULL DEFAULT 0,
    rows_restored  BIGINT NOT NULL DEFAULT 0,
    -- Claim/heartbeat/attempts, copied in shape from inspector_scans: a
    -- `running` row whose heartbeat has expired is re-claimable, and that IS the
    -- crash-resume path.
    worker_id      TEXT,
    heartbeat_at   TIMESTAMPTZ,
    attempts       INT NOT NULL DEFAULT 0,
    error          TEXT NOT NULL DEFAULT '',
    requested_by   UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    started_at     TIMESTAMPTZ,
    finished_at    TIMESTAMPTZ,
    CONSTRAINT restore_jobs_range_ordered CHECK (range_end > range_start)
);

-- The claim query: oldest queued or lease-expired job first.
CREATE INDEX restore_jobs_claim_idx ON restore_jobs (status, created_at);
-- Overlap detection on create (two concurrent restores of the same range would
-- double-insert) and the admin list, both scoped by table.
CREATE INDEX restore_jobs_range_idx ON restore_jobs (table_name, range_start, range_end);
