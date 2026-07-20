-- 0017: developer-supplied metadata scopes on analytics_events. `tags` (flat
-- string->string), `contexts` (dev-owned structured blocks — DISTINCT from the
-- machine-owned `context` column), `extra` (freeform JSON). ADD COLUMN on the
-- partitioned parent propagates to every partition.
ALTER TABLE analytics_events ADD COLUMN tags     JSONB NOT NULL DEFAULT '{}'::jsonb;
ALTER TABLE analytics_events ADD COLUMN contexts JSONB NOT NULL DEFAULT '{}'::jsonb;
ALTER TABLE analytics_events ADD COLUMN extra    JSONB NOT NULL DEFAULT '{}'::jsonb;
