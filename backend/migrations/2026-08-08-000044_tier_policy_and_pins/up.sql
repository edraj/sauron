-- Runtime-tunable deployment settings, plus the pins that keep restored cold
-- data from being immediately re-tiered.
--
-- `runtime_settings` is deliberately a generic key/value table rather than a
-- typed singleton: every value here is one an operator may need to change
-- without a restart, and the set will grow. Absence of a row is meaningful and
-- is the default state — it means "fall back to the process's configured value"
-- (the TIER_HOT_DAYS env var, itself defaulting to 30). Nothing is seeded, so a
-- fresh install behaves exactly as it did before this migration, and reverting
-- to env-driven behaviour is a DELETE rather than a schema change.
--
-- Values are TEXT and parsed by the reader. That keeps one table serving
-- integers, booleans and paths without a column per type; the write path is the
-- only place that knows a key's shape, and it validates there.
CREATE TABLE runtime_settings (
    key         TEXT PRIMARY KEY,
    value       TEXT NOT NULL,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Who last changed it. Nullable because a value may also be set by a
    -- migration or a maintenance script with no user behind it. ON DELETE SET
    -- NULL, not CASCADE: deleting a user must not silently revert a deployment
    -- setting they happened to have been the last to touch.
    updated_by  UUID REFERENCES users(id) ON DELETE SET NULL
);

-- Ranges that sauron-tier must NOT drop from Postgres, even though they sit
-- below the export watermark and are durable in Parquet.
--
-- This exists because restoring cold data is otherwise self-defeating. The tier
-- worker's drop step (step 4 in sauron-tier/src/main.rs) drops any partition
-- whose range end is at or below the watermark once the drop lag has passed,
-- and its late-write guard only retains a partition when Postgres holds MORE
-- rows than the cold copy. A restore puts back exactly what is in Parquet, so
-- `pg_now == cold_now`, the guard does not fire, and the very next cycle drops
-- the partition again — the restore would survive `tier_drop_lag_hours` and
-- then silently vanish. A pin is what makes a restore durable.
--
-- `expires_at` is required, not optional. An unbounded pin is a permanent
-- opt-out of tiering for that range, which is how a disk fills up months after
-- someone investigated an incident and moved on. Extending a pin is an explicit
-- act; forgetting one is not.
CREATE TABLE tier_pins (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    table_name  TEXT NOT NULL,
    -- Half-open [range_start, range_end), matching the partition bounds the
    -- worker computes via bucket_bounds().
    range_start TIMESTAMPTZ NOT NULL,
    range_end   TIMESTAMPTZ NOT NULL,
    expires_at  TIMESTAMPTZ NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_by  UUID REFERENCES users(id) ON DELETE SET NULL,
    reason      TEXT,
    CONSTRAINT tier_pins_range_ordered CHECK (range_end > range_start)
);

-- The worker's hot-path question is "is any unexpired pin overlapping this
-- partition?", asked once per candidate partition per cycle.
CREATE INDEX tier_pins_lookup ON tier_pins (table_name, expires_at, range_start, range_end);
