UPDATE roles SET permissions = permissions - 'artifact:write'
WHERE name IN ('Owner', 'Admin', 'Developer');

ALTER TABLE error_events DROP COLUMN IF EXISTS debug_meta;
ALTER TABLE error_events DROP COLUMN IF EXISTS symbolication_status;
ALTER TABLE error_events DROP COLUMN IF EXISTS stacktrace_symbolicated;

DROP TABLE IF EXISTS symbol_artifacts;
DROP TABLE IF EXISTS symbol_blobs;
