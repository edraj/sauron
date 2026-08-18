-- Narrowing back can fail where a build legitimately carries both kinds; that
-- is the point of the widening, so the down migration drops the extra rows'
-- index rather than the rows.
DROP INDEX IF EXISTS symbol_artifacts_kind_debugid_idx;
CREATE UNIQUE INDEX symbol_artifacts_debugid_idx
    ON symbol_artifacts (app_id, debug_id) WHERE debug_id IS NOT NULL;
