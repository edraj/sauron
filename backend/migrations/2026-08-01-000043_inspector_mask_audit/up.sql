-- The PII inspector's write side. `inspector_mask_actions` is simultaneously
-- the job queue, the resume cursor, the progress meter and the record of who
-- did it — this repository's first audit table.
--
-- `kind` is load-bearing and is NOT the same axis as `status`. Routing
-- previews through the status machine (status='preview' as a queue state)
-- means the mask claim predicate matches neither arm, no preview ever runs,
-- the dialog polls forever, and confirm — which requires 'previewed' — can
-- never fire. Counting vs. updating branches on `kind`, never on `phase`.
--
-- Upgrade hazard: sauron-migrate.service has no [Install] section and is not
-- in %postun's restart list, so `dnf upgrade` leaves new binaries running
-- against the old schema. Until `systemctl start sauron-migrate` is run by
-- hand, the pipeline's masked_keys_for_app query fails on every cache miss
-- and forward masking is off deployment-wide with only a log line.

CREATE TABLE inspector_mask_actions (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id              UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    app_id              UUID NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    kind                TEXT NOT NULL CHECK (kind IN ('preview','mask')),
    -- Nullable so the audit row outlives finding pruning. Both are validated
    -- against app_id at preview time.
    finding_id          UUID REFERENCES inspector_findings(id) ON DELETE SET NULL,
    scan_id             UUID REFERENCES inspector_scans(id) ON DELETE SET NULL,
    -- The fully resolved [{table, column, path, wildcard}] list, frozen at
    -- preview so confirm can never widen what was counted and shown.
    -- Contains paths, never values.
    targets             JSONB NOT NULL DEFAULT '[]'::jsonb,
    status              TEXT NOT NULL DEFAULT 'preview' CHECK (status IN (
                            'preview','previewed','pending','running',
                            'cancelling','done','failed','cancelled')),
    -- SET NULL, not CASCADE: deleting a user must not erase the trail.
    requested_by        UUID REFERENCES users(id) ON DELETE SET NULL,
    -- Denormalized snapshot, because SET NULL loses the identity.
    requested_by_email  TEXT NOT NULL DEFAULT '',
    cancelled_by        UUID REFERENCES users(id) ON DELETE SET NULL,
    cancelled_by_email  TEXT NOT NULL DEFAULT '',
    cancelled_at        TIMESTAMPTZ,
    requested_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- The preview TTL runs from HERE, not from requested_at: a preview queued
    -- behind a multi-hour mask would otherwise expire before it was readable.
    previewed_at        TIMESTAMPTZ,
    confirmed_at        TIMESTAMPTZ,
    started_at          TIMESTAMPTZ,
    finished_at         TIMESTAMPTZ,
    -- Behind the shipped nginx with API_TRUST_FORWARDED_HEADERS=false this is
    -- the proxy's address for every actor; the value records its own trust
    -- decision so a reader can tell.
    confirm_source      TEXT NOT NULL DEFAULT '',
    estimated_rows      BIGINT NOT NULL DEFAULT 0,
    rows_scanned        BIGINT NOT NULL DEFAULT 0,
    rows_masked         BIGINT NOT NULL DEFAULT 0,
    cold_rows_skipped   BIGINT NOT NULL DEFAULT 0,
    -- Re-recorded at finish, not only at preview, so the audit shows what
    -- execution actually skipped rather than what the preview predicted.
    cold_boundary_at    TIMESTAMPTZ,
    day_cursor          DATE,
    cursor_occurred_at  TIMESTAMPTZ,
    cursor_id           UUID,
    phase               TEXT NOT NULL DEFAULT 'idle' CHECK (phase IN (
                            'idle','counting','hot','default_partition',
                            'companions','tail_sweep','finished')),
    worker_id           TEXT,
    claimed_at          TIMESTAMPTZ,
    vacuum_advised      BOOL NOT NULL DEFAULT FALSE,
    error               TEXT NOT NULL DEFAULT ''
);
CREATE INDEX inspector_mask_actions_app_idx ON inspector_mask_actions (app_id, requested_at DESC);
CREATE INDEX inspector_mask_actions_org_idx ON inspector_mask_actions (org_id, requested_at DESC);
-- Two independent claim slots. A single FIFO would let a multi-hour mask
-- starve every preview past its 15-minute TTL, making confirm permanently
-- impossible on a busy app.
CREATE INDEX inspector_mask_actions_mask_claim_idx ON inspector_mask_actions (requested_at)
    WHERE kind = 'mask' AND status IN ('pending','running','cancelling');
CREATE INDEX inspector_mask_actions_preview_claim_idx ON inspector_mask_actions (requested_at)
    WHERE kind = 'preview' AND status = 'preview';

CREATE TABLE inspector_masked_keys (
    id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    app_id           UUID NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    -- An ALLOWLIST, not a denylist: a denylist silently fails to protect the
    -- next account table someone adds. The scan-only tables (devices,
    -- identities, workflows) are deliberately absent — a masked-key row for
    -- one of them would be read by the pipeline enforcer and the retro-mask,
    -- both of which would report success on a write the next event overwrites.
    target_table     TEXT NOT NULL CHECK (target_table IN (
                         'error_events','analytics_events','transactions',
                         'issues','event_users','sessions')),
    target_column    TEXT NOT NULL,
    -- '' = the whole column (TEXT columns).
    json_path        TEXT NOT NULL DEFAULT '',
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_by       UUID REFERENCES users(id) ON DELETE SET NULL,
    source_action_id UUID REFERENCES inspector_mask_actions(id) ON DELETE SET NULL
);
-- Makes re-masking the same finding idempotent.
CREATE UNIQUE INDEX inspector_masked_keys_key
    ON inspector_masked_keys (app_id, target_table, target_column, json_path);
CREATE INDEX inspector_masked_keys_app_idx ON inspector_masked_keys (app_id);

-- POST /findings/{id}/reveal is an endpoint whose entire purpose is emitting
-- raw customer PII. Shipping it with no record of who revealed what is not
-- defensible. The row is written BEFORE the value is returned, so a failure
-- to audit is a failure to reveal.
CREATE TABLE inspector_reveal_audit (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    app_id         UUID NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    org_id         UUID NOT NULL,
    finding_id     UUID REFERENCES inspector_findings(id) ON DELETE SET NULL,
    user_id        UUID REFERENCES users(id) ON DELETE SET NULL,
    user_email     TEXT NOT NULL DEFAULT '',
    source_table   TEXT NOT NULL,
    source_column  TEXT NOT NULL,
    key_path       TEXT NOT NULL,
    request_source TEXT NOT NULL DEFAULT '',
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX inspector_reveal_audit_app_idx ON inspector_reveal_audit (app_id, created_at DESC);
