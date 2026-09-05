-- Dropping each parent index drops every partition's child index with it.
DROP INDEX IF EXISTS error_events_context_gin;
DROP INDEX IF EXISTS analytics_events_context_gin;
DROP INDEX IF EXISTS sessions_context_gin;
