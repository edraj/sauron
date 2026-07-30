-- Reverses 000028: restore the narrow index this migration replaced, then drop the covering
-- one, so a revert leaves `error_events` with exactly the index shape it had before this
-- migration ran -- neither index missing, neither doubled up.
CREATE INDEX error_events_issue_env_idx
    ON error_events (issue_id, environment_id);

DROP INDEX IF EXISTS error_events_issue_env_covering_idx;
