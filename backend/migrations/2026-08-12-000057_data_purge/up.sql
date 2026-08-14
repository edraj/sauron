-- Admin data purge. `purge_jobs` is simultaneously the queue, the frozen
-- scope, the resume cursor, the progress meter and the record of who did it —
-- the same shape as `inspector_mask_actions`, and for the same reasons.
--
-- The job runs in TWO phases and `phase` is a different axis from `status`:
-- `delete` removes raw rows and records which rollup keys it touched, then
-- `recompute` re-derives those rollups from what survives. A job that stopped
-- between them has deleted rows and left counters overcounting, which is the
-- exact state the feature exists to prevent — so `recompute` must be
-- resumable from the job row alone, never from anything held in worker memory.

CREATE TABLE purge_jobs (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    -- The one FK, and the partitioning key, matching audit_log's rule.
    org_id              UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,

    -- NO foreign key on app_id, deliberately. Purge history has to stay
    -- readable after the app it purged is deleted. `REFERENCES apps(id) ON
    -- DELETE SET NULL` would blank the identifying column on every historical
    -- row, so filtering history by the app you destroyed returns nothing —
    -- which is the exact question someone reads this table to answer. And
    -- `ON DELETE CASCADE` would erase the record of the purge entirely. The id
    -- is an inert snapshot; the two denormalized columns beside it are what
    -- keep the row legible once the app is gone.
    app_id              UUID NOT NULL,
    app_slug            TEXT NOT NULL DEFAULT '',
    app_name            TEXT NOT NULL DEFAULT '',

    -- NULL = every environment, INCLUDING unattributed rows. Not an empty
    -- array: `[]` is a scope that matches nothing, and the two must not be
    -- spelled the same way. Unattributed is a real row rather than an absence
    -- (`EnvFilter::Unattributed`), and a purge that silently skipped those
    -- would leave the most confusing possible remainder.
    environment_ids     JSONB,
    kinds               JSONB NOT NULL DEFAULT '[]'::jsonb,

    range_start         TIMESTAMPTZ,
    range_end           TIMESTAMPTZ,
    -- An explicit column, NOT "both bounds are NULL". Wiping an app's whole
    -- history for a kind is legitimate, but it must be an affirmative choice
    -- rather than the accidental result of a date field left blank. The CHECK
    -- makes the two spellings impossible to confuse: unbounded requires the
    -- flag, and the flag forbids bounds.
    all_time            BOOL NOT NULL DEFAULT FALSE,
    CONSTRAINT purge_jobs_range_or_all_time CHECK (
        (all_time AND range_start IS NULL AND range_end IS NULL)
        OR (NOT all_time AND range_start IS NOT NULL AND range_end IS NOT NULL
            AND range_start < range_end)
    ),

    status              TEXT NOT NULL DEFAULT 'previewing' CHECK (status IN (
                            'previewing','previewed','pending','running',
                            'cancelling','done','failed','cancelled')),
    phase               TEXT NOT NULL DEFAULT 'idle' CHECK (phase IN (
                            'idle','counting','delete','recompute','finished')),

    -- Per-kind maps, {kind: n}. Two separate columns rather than one updated
    -- in place: the whole point of the preview is that the operator can
    -- compare what was promised against what happened.
    estimated_counts    JSONB NOT NULL DEFAULT '{}'::jsonb,
    deleted_counts      JSONB NOT NULL DEFAULT '{}'::jsonb,
    rollups_recomputed  BIGINT NOT NULL DEFAULT 0,
    rollups_deleted     BIGINT NOT NULL DEFAULT 0,

    -- Rows in range that live in cold Parquet and are therefore NOT deleted.
    -- Surfaced at preview so the operator sees what will survive before
    -- confirming, and re-recorded at finish so the record shows what execution
    -- actually skipped rather than what the preview predicted.
    cold_rows_skipped   BIGINT NOT NULL DEFAULT 0,
    cold_boundary_at    TIMESTAMPTZ,

    -- Resume cursor. `kind_cursor` names which kind the delete phase is on;
    -- the pair below is the keyset position within it.
    kind_cursor         TEXT,
    cursor_occurred_at  TIMESTAMPTZ,
    cursor_id           UUID,

    -- SET NULL, not CASCADE: deleting a user must not erase the trail.
    requested_by        UUID REFERENCES users(id) ON DELETE SET NULL,
    -- Denormalized, because SET NULL loses the identity.
    requested_by_email  TEXT NOT NULL DEFAULT '',
    cancelled_by        UUID REFERENCES users(id) ON DELETE SET NULL,
    cancelled_by_email  TEXT NOT NULL DEFAULT '',
    cancelled_at        TIMESTAMPTZ,

    requested_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- The preview TTL runs from HERE, not from requested_at, matching
    -- inspector_mask_actions: a preview queued behind a long-running purge
    -- would otherwise expire before it was ever readable.
    previewed_at        TIMESTAMPTZ,
    confirmed_at        TIMESTAMPTZ,
    started_at          TIMESTAMPTZ,
    finished_at         TIMESTAMPTZ,
    confirm_source      TEXT NOT NULL DEFAULT '',

    -- Whether the app was still receiving events when the job started.
    -- Recompute against live ingest drifts the moment it is written; this does
    -- not prevent that, it makes a confusing result explainable afterwards
    -- instead of a mystery.
    ingest_active       BOOL NOT NULL DEFAULT FALSE,

    worker_id           TEXT,
    claimed_at          TIMESTAMPTZ,
    error               TEXT NOT NULL DEFAULT ''
);

CREATE INDEX purge_jobs_app_idx ON purge_jobs (app_id, requested_at DESC);
CREATE INDEX purge_jobs_org_idx ON purge_jobs (org_id, requested_at DESC);
-- The claim slot. Partial, so the scan stays proportional to outstanding work
-- rather than to the history the table accumulates forever.
CREATE INDEX purge_jobs_claim_idx ON purge_jobs (requested_at)
    WHERE status IN ('pending','running','cancelling');
-- The counting slot, kept separate for the reason the mask table documents:
-- one FIFO would let a multi-hour purge starve every preview past its TTL,
-- making confirm permanently impossible on a busy app.
CREATE INDEX purge_jobs_preview_claim_idx ON purge_jobs (requested_at)
    WHERE status = 'previewing';

-- The rollup keys the delete phase touched, drained by the recompute phase.
--
-- A table rather than a JSONB column on the job: the touched set is one entry
-- per distinct session / device / person / issue and reaches millions on the
-- purges this feature exists for, which is far past what is sane to rewrite on
-- every batch flush.
--
-- UNLOGGED is deliberate and safe here. It is not written to the WAL and is
-- TRUNCATED by crash recovery — but a crash also loses the worker's lease, and
-- the resumed job re-runs its delete phase from `kind_cursor`, which
-- repopulates the set. Durability would buy nothing and cost WAL on the
-- hottest write in the job.
CREATE UNLOGGED TABLE purge_touched_keys (
    job_id  UUID NOT NULL REFERENCES purge_jobs(id) ON DELETE CASCADE,
    kind    TEXT NOT NULL,
    -- TEXT, not UUID: session_id and device_key are SDK-supplied strings while
    -- issue_id is a UUID. One column that holds every key type keeps the
    -- drain loop uniform.
    key     TEXT NOT NULL,
    PRIMARY KEY (job_id, kind, key)
);
