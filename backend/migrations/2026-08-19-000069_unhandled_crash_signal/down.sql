-- Reverting restores the old meaning of crash-free (any error of any level, at
-- any handled state, marks a session crashed). `error_events.handled` is NOT
-- dropped — it predates this migration (0024) and `is:unhandled` still needs it.
ALTER TABLE sessions DROP COLUMN IF EXISTS unhandled_errors_count;
