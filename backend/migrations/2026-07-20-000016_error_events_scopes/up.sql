-- 0016: developer-supplied metadata scopes on error_events. `contexts` (plural)
-- is the dev-owned structured-block map and `extra` is freeform JSON — both are
-- DISTINCT from the machine-owned `context` (singular) column; never conflate.
-- ADD COLUMN on the partitioned parent propagates to every partition.
ALTER TABLE error_events ADD COLUMN contexts JSONB NOT NULL DEFAULT '{}'::jsonb;
ALTER TABLE error_events ADD COLUMN extra    JSONB NOT NULL DEFAULT '{}'::jsonb;
