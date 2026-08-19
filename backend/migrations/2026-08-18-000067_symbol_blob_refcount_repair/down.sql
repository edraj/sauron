-- Reverting drops the self-correction; it does NOT resurrect blobs reclaimed by
-- the one-time DELETE above, which were unreachable by construction (no
-- referring artifact) and cannot be reconstructed from within the database.
DROP TRIGGER IF EXISTS symbol_artifacts_resync_blobs ON symbol_artifacts;
DROP FUNCTION IF EXISTS symbol_blobs_resync();
DROP FUNCTION IF EXISTS symbol_blob_refcount(BYTEA);
