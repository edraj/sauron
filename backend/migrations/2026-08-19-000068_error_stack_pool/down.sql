-- Reverting strands pooled rows with a placeholder stacktrace and no pool to
-- hydrate from — readers would serve '[]' for them. Do not revert on a
-- database that has run with INGEST_STACK_POOLING enabled unless those rows
-- have been de-pooled first (the mask de-pool statement shape does this:
-- UPDATE error_events e SET stacktrace = b.content, stacktrace_sha256 = NULL
-- FROM error_stack_blobs b WHERE e.stacktrace_sha256 = b.sha256).
DROP INDEX IF EXISTS error_events_stack_sha_idx;
ALTER TABLE error_events DROP COLUMN IF EXISTS stacktrace_sha256;
DROP TABLE IF EXISTS error_stack_blobs;
