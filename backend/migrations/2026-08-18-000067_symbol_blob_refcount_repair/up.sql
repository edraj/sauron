-- 0067: make `symbol_blobs.refcount` self-correcting, and reclaim the bytes the
-- hand-maintained version already leaked.
--
-- WHAT IS BROKEN
--
-- `refcount` is maintained by hand from two sides: `repo::put_blob` increments
-- on upload, `repo::delete_symbol_artifact` decrements and GCs at <= 0. Neither
-- runs when an artifact disappears through a foreign key, and
--
--     symbol_artifacts.app_id UUID NOT NULL REFERENCES apps(id) ON DELETE CASCADE
--
-- means deleting an app -- or a project, or an org, which cascade to apps --
-- removes every artifact row without any Rust code executing. The blob is left
-- at refcount >= 1 with zero referrers: unreachable bytes that no code path will
-- ever collect. Two smaller leaks exist on the upload side (the documented
-- unique-violation race loser, and any non-unique-violation error after
-- `put_blob` has already incremented).
--
-- Measured on the dev database before this migration: 5 blobs / 1,238 kB, with
-- `sum(refcount) = 8` against only 4 real references, and one 387 kB blob at
-- refcount 1 with no referring artifact at all -- 31% of stored blob bytes
-- already unreachable, on a near-empty install.
--
-- WHY A TRIGGER, AND WHY RECOMPUTE RATHER THAN DECREMENT
--
-- A trigger is the only mechanism that fires for a cascaded delete, which is the
-- dominant leak path. It RECOMPUTES the count from the referring rows instead of
-- applying a -1 delta, which makes it idempotent: `delete_symbol_artifact` still
-- decrements by hand, and a decrement followed by this recompute converges on
-- the truth rather than double-counting. That property is what lets this land
-- without touching the Rust, which matters because refcount arithmetic spread
-- across two layers is exactly how the counter drifted in the first place.
--
-- The counter is now a CACHE of a derivable fact. Treat any disagreement between
-- it and the referring rows as a bug in this trigger, not as state to reconcile
-- by hand.

-- One row per (blob, referring artifact), covering both reference columns.
CREATE OR REPLACE FUNCTION symbol_blob_refcount(blob BYTEA) RETURNS BIGINT
LANGUAGE sql STABLE AS $$
    SELECT count(*)::bigint
      FROM symbol_artifacts a
     WHERE a.blob_sha256 = blob
        OR a.prebuilt_index_sha256 = blob;
$$;

CREATE OR REPLACE FUNCTION symbol_blobs_resync() RETURNS TRIGGER
LANGUAGE plpgsql AS $$
DECLARE
    touched BYTEA[];
    b       BYTEA;
BEGIN
    -- Both columns of both tuple versions: an UPDATE that repoints an artifact
    -- changes the count of the OLD blob and the NEW one.
    touched := ARRAY(
        SELECT DISTINCT x FROM unnest(ARRAY[
            CASE WHEN TG_OP <> 'INSERT' THEN OLD.blob_sha256 END,
            CASE WHEN TG_OP <> 'INSERT' THEN OLD.prebuilt_index_sha256 END,
            CASE WHEN TG_OP <> 'DELETE' THEN NEW.blob_sha256 END,
            CASE WHEN TG_OP <> 'DELETE' THEN NEW.prebuilt_index_sha256 END
        ]) AS x WHERE x IS NOT NULL
    );

    FOREACH b IN ARRAY touched LOOP
        UPDATE symbol_blobs SET refcount = symbol_blob_refcount(b) WHERE sha256 = b;
        -- GC here rather than in a reaper: the FK from symbol_artifacts makes a
        -- delete of a still-referenced blob impossible, so "refcount reached 0"
        -- is a safe and complete condition. A blob re-uploaded later is
        -- re-created by `put_blob` from content the client still holds.
        DELETE FROM symbol_blobs WHERE sha256 = b AND refcount = 0;
    END LOOP;

    RETURN NULL;
END;
$$;

-- AFTER, and per ROW: a cascaded delete fires row triggers on the referencing
-- table, which is the whole point. STATEMENT-level would not see OLD.
DROP TRIGGER IF EXISTS symbol_artifacts_resync_blobs ON symbol_artifacts;
CREATE TRIGGER symbol_artifacts_resync_blobs
    AFTER INSERT OR UPDATE OF blob_sha256, prebuilt_index_sha256 OR DELETE
    ON symbol_artifacts
    FOR EACH ROW EXECUTE FUNCTION symbol_blobs_resync();

-- WHAT THIS DOES NOT FIX
--
-- A blob that never gets an artifact row at all is invisible to this trigger,
-- because the trigger only fires on `symbol_artifacts`. That is the upload-side
-- race: `put_blob` has already inserted the blob (refcount 1) when
-- `insert_symbol_artifact` loses a unique-violation race or errors for any other
-- reason, and the recovery arm deliberately does not decrement. Those orphans
-- are collected by the one-time DELETE below when this migration runs, but a new
-- one created afterwards will sit there until something sweeps it.
--
-- Since `refcount` is now a cache of a derivable fact, the sweep is a one-liner
-- and is safe to run at any time -- the FK from `symbol_artifacts` makes a zero
-- count both necessary and sufficient for "unreachable":
--
--     DELETE FROM symbol_blobs WHERE symbol_blob_refcount(sha256) = 0;
--
-- Wiring that onto a schedule needs a caller (a reaper tick alongside the
-- existing retention reapers) and is deliberately left out of this migration,
-- which is structural only.
--
-- Verified on a scratch PG16 before shipping: an app DELETE cascading to its
-- artifacts reclaims the blob (1 -> 0); a blob shared by two apps survives the
-- first app's deletion at refcount 1 and is reclaimed only when the second goes;
-- and a seeded orphan with no artifact is NOT collected, as described above.

-- One-time repair of the drift already on disk.
UPDATE symbol_blobs b SET refcount = symbol_blob_refcount(b.sha256)
 WHERE b.refcount IS DISTINCT FROM symbol_blob_refcount(b.sha256);

-- One-time reclaim. Safe for the same reason the trigger's GC is: the FK
-- guarantees a zero count means nothing can reach these bytes.
DELETE FROM symbol_blobs WHERE refcount = 0;
