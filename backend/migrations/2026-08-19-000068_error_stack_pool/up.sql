-- 0068: content-addressed stacktrace pool for error_events ("Tier 1").
--
-- A repeated exception stores its stacktrace once per OCCURRENCE today, and the
-- trace is byte-identical across every occurrence of an issue by construction
-- (it is what the fingerprint groups on). Measured on a duplicate-heavy run:
-- 199,990 rows carried 5 distinct stacktraces at ~1.1 kB stored each — the
-- single largest recoverable byte in the row (~25%), and the only column the
-- toaster pushes out of line on realistic payloads.
--
-- DESIGN, and why it is shaped this way:
--
--   * `content` is JSONB, not compressed bytes. The `stack:` query dimension
--     filters stacktraces with `@>` / `@?` / ILIKE; keeping the pooled value
--     queryable lets that lowerer prefilter the pool (a handful of rows)
--     instead of scanning every event row — measured at 133x on the bench.
--   * There is NO refcount. Migration 0067 exists because a hand-maintained
--     refcount under a cascading owner drifted; this table skips the counter
--     entirely. Reachability is derived: a blob is live while any event row
--     references it, and `error_events_stack_sha_idx` makes that probe cheap.
--   * The FK is the backstop that makes over-free STRUCTURALLY impossible: a
--     DELETE of a still-referenced blob fails instead of corrupting reads.
--     NULLs skip FK checks, so rows written with pooling off pay nothing.
--   * Rows written before this migration keep their inline trace and NULL
--     sha256 forever; readers treat NULL as "inline is the truth". Writers
--     only pool when INGEST_STACK_POOLING is enabled (default off).
--
-- GC is a sweep in sauron-tier (the process that drops partitions, which is
-- the event that orphans blobs): delete blobs past a grace age that no row in
-- the partition tree references. The FK turns any sweep bug into a loud error
-- rather than data loss.

CREATE TABLE error_stack_blobs (
    sha256     BYTEA PRIMARY KEY,
    content    JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Appended, never inserted mid-list: models::ErrorEvent decodes positionally
-- and ALTER TABLE ADD COLUMN appends physically — same note as guest_alias.
ALTER TABLE error_events
    ADD COLUMN stacktrace_sha256 BYTEA REFERENCES error_stack_blobs(sha256);

-- Partial: rows written with pooling off (and every pre-existing row) are NULL
-- and cost nothing here. Serves the GC reachability probe and the stack:
-- search prefilter. Created on the parent, so it propagates to every current
-- and future partition.
CREATE INDEX error_events_stack_sha_idx
    ON error_events (stacktrace_sha256) WHERE stacktrace_sha256 IS NOT NULL;
