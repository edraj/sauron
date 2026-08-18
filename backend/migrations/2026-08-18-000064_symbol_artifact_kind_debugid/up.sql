-- One build can now carry MORE THAN ONE kind of artifact.
--
-- `symbol_artifacts_debugid_idx` was UNIQUE on (app_id, debug_id), which was
-- right while `debug_id` belonged to exactly one kind (`dart_symbols`, whose id
-- is derived from the ELF's GNU build-id note). `dart_obfuscation_map` arrives
-- for the SAME build and must carry the SAME id — that is the only thing that
-- ties a map to the symbols it was emitted beside, since the map is plain JSON
-- with nothing identifying inside it. Under the old index the second upload
-- lost a unique-violation race against the first and was handed back the
-- other's row as a dedupe.
--
-- Widening to (app_id, kind, debug_id) keeps every guarantee the old index
-- gave: uploading the same kind twice for one build still dedupes, and the
-- idempotency lookup in `routes/artifacts.rs` is kind-scoped to match.
DROP INDEX IF EXISTS symbol_artifacts_debugid_idx;
CREATE UNIQUE INDEX symbol_artifacts_kind_debugid_idx
    ON symbol_artifacts (app_id, kind, debug_id) WHERE debug_id IS NOT NULL;
