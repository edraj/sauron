-- The PII inspector's read side: where inspection is switched on, when it
-- runs, one row per run, and the aggregated result.
--
-- `inspector_findings` deliberately has NO raw-value column and NO hash
-- column. A findings table that keeps sample values is a second, longer-lived,
-- more concentrated copy of the PII in a table nobody tiers — strictly worse
-- than the original. And a SHA-256 of an email is a stable pseudonymous
-- identifier of a person, trivially brute-forced for low-entropy values, so
-- "just hash it" is not a mitigation. A locator plus a shape-only preview is
-- everything an admin needs to decide.
--
-- `target_type` is NOT named `scope_type`: dashboard/src/lib/models/
-- scope-type.test.ts parses the newest `CHECK (scope_type IN (...))` out of
-- this directory and asserts it equals ['app','env','org','project']. A new
-- column with that name fails that test.
--
-- `scan_columns` is NOT named `columns`: diesel_derives emits `pub mod
-- columns` inside every generated table module and re-exports it, so a column
-- named `columns` produces `error[E0573]: expected type, found module` on the
-- table! block AND on every #[diesel(table_name = ...)] derive.

CREATE TABLE inspector_policies (
    id                UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    -- Denormalized tenant key, same as alert_rules: list queries and the
    -- reaper must never join upward to find the org.
    org_id            UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    target_type       TEXT NOT NULL CHECK (target_type IN ('project','app','app_env')),
    -- Polymorphic, no FK (matches role_grants). For 'app_env' this holds an
    -- app_environments.id — the ENROLLMENT id, never a catalogue
    -- environments.id. Event rows store the enrollment id, so the other one
    -- would silently match nothing.
    target_id         UUID NOT NULL,
    enabled           BOOL NOT NULL DEFAULT TRUE,
    -- [{key, scope:'any'|'top'}], key lowercased at write.
    tracked_keys      JSONB NOT NULL DEFAULT '[]'::jsonb,
    -- Preset detector ids from a &'static list in sauron-inspector.
    detectors         JSONB NOT NULL DEFAULT '[]'::jsonb,
    -- NULL = the default column set from the inventory.
    scan_columns      JSONB,
    rollups           JSONB NOT NULL DEFAULT '["issues","event_users"]'::jsonb,
    window_days       INT NOT NULL DEFAULT 30 CHECK (window_days BETWEEN 1 AND 400),
    schedule_enabled  BOOL NOT NULL DEFAULT FALSE,
    -- Bit N = EXTRACT(DOW) = N, so Sunday is bit 0.
    schedule_days     SMALLINT NOT NULL DEFAULT 0 CHECK (schedule_days BETWEEN 0 AND 127),
    schedule_time     TIME NOT NULL DEFAULT '03:00',
    -- IANA name, validated at write with `SELECT now() AT TIME ZONE $1`.
    schedule_tz       TEXT NOT NULL DEFAULT 'UTC',
    -- Materialized due time; the monitors.next_check_at pattern.
    next_run_at       TIMESTAMPTZ,
    last_run_at       TIMESTAMPTZ,
    last_scan_id      UUID,
    last_skip_reason  TEXT NOT NULL DEFAULT '',
    created_by        UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);
-- One policy per node is what makes precedence a database fact rather than an
-- ordering problem.
CREATE UNIQUE INDEX inspector_policies_target_key ON inspector_policies (target_type, target_id);
CREATE INDEX inspector_policies_org_idx ON inspector_policies (org_id);
CREATE INDEX inspector_policies_due_idx ON inspector_policies (next_run_at)
    WHERE enabled AND schedule_enabled;

CREATE TABLE inspector_scans (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    policy_id           UUID NOT NULL REFERENCES inspector_policies(id) ON DELETE CASCADE,
    org_id              UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    trigger_type        TEXT NOT NULL CHECK (trigger_type IN ('scheduled','manual')),
    requested_by        UUID REFERENCES users(id) ON DELETE SET NULL,
    status              TEXT NOT NULL DEFAULT 'queued'
                          CHECK (status IN ('queued','running','succeeded','failed','cancelled')),
    -- Kept separate from `status` so a completed-but-incomplete scan is not
    -- mistaken for a failure.
    coverage            TEXT NOT NULL DEFAULT 'full' CHECK (coverage IN ('full','partial')),
    coverage_note       TEXT NOT NULL DEFAULT '',
    window_from         TIMESTAMPTZ NOT NULL,
    window_to           TIMESTAMPTZ NOT NULL,
    -- Frozen copies of tracked_keys/detectors/scan_columns/rollups. The unit
    -- list is recomputed from these on resume, so an admin editing the policy
    -- mid-scan must not be able to change what unit #37 means.
    params              JSONB NOT NULL,
    -- Resolved ordered [(app_id, app_env_id|null)] pairs, capped at 2000.
    targets             JSONB NOT NULL,
    units_total         INT NOT NULL DEFAULT 0,
    units_done          INT NOT NULL DEFAULT 0,
    cursor              JSONB NOT NULL DEFAULT '{}'::jsonb,
    rows_scanned        BIGINT NOT NULL DEFAULT 0,
    findings_count      INT NOT NULL DEFAULT 0,
    findings_reaped_at  TIMESTAMPTZ,
    worker_id           TEXT,
    heartbeat_at        TIMESTAMPTZ,
    attempts            INT NOT NULL DEFAULT 0,
    cancel_requested_at TIMESTAMPTZ,
    error               TEXT NOT NULL DEFAULT '',
    started_at          TIMESTAMPTZ,
    finished_at         TIMESTAMPTZ,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX inspector_scans_policy_idx ON inspector_scans (policy_id, created_at DESC);
CREATE INDEX inspector_scans_org_idx ON inspector_scans (org_id, created_at DESC);
CREATE INDEX inspector_scans_claim_idx ON inspector_scans (status, heartbeat_at);
-- "One active scan per policy" as a database invariant instead of a race
-- between the API and the scheduler.
CREATE UNIQUE INDEX inspector_scans_active_key ON inspector_scans (policy_id)
    WHERE status IN ('queued','running');

CREATE TABLE inspector_findings (
    id                 UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    scan_id            UUID NOT NULL REFERENCES inspector_scans(id) ON DELETE CASCADE,
    org_id             UUID NOT NULL,
    app_id             UUID NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    environment_id     UUID,
    -- The third state a two-state (env_id IS NULL) model cannot express.
    -- `issues`, `event_users`, `devices` and `identities` have no environment
    -- column at all, so every rollup finding would otherwise land in the
    -- "unattributed" bucket and conflate "the platform could not attribute
    -- this row" with "this table has no environment concept".
    env_scope          TEXT NOT NULL
                         CHECK (env_scope IN ('enrollment','unattributed','no_env_column')),
    CONSTRAINT inspector_findings_env_consistency
        CHECK ((env_scope = 'enrollment') = (environment_id IS NOT NULL)),
    -- Both from the &'static inventory in sauron-inspector, never caller bytes.
    source_table       TEXT NOT NULL,
    source_column      TEXT NOT NULL,
    -- Dev-controlled bytes: object keys are arbitrary UTF-8, so this is
    -- redacted in Rust before it is written. See sauron_inspector::redact.
    key_path           TEXT NOT NULL,
    matched_key        TEXT NOT NULL,
    detector           TEXT NOT NULL DEFAULT '',
    value_type         TEXT NOT NULL,
    match_count        BIGINT NOT NULL DEFAULT 0,
    match_count_exact  BOOL NOT NULL DEFAULT TRUE,
    -- Shape-only, capped at 64 chars, never more than the first and last
    -- codepoint. NOT the value.
    sample_preview     TEXT NOT NULL DEFAULT '',
    sample_row_id      UUID,
    -- Mandatory for partitioned sources so the reveal query prunes to one child.
    sample_occurred_at TIMESTAMPTZ,
    partition_kind     TEXT NOT NULL DEFAULT 'ranged'
                         CHECK (partition_kind IN ('ranged','default','rollup')),
    first_seen_at      TIMESTAMPTZ,
    last_seen_at       TIMESTAMPTZ,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now()
);
-- An EXPRESSION index, not `NULLS NOT DISTINCT`: that syntax silently raises
-- the deployment's Postgres floor to 15, and because run_pending_migrations
-- stops at the first failure, a PG13 host would apply 000041, fail here, and
-- block every later migration in the product permanently. COALESCE is PG11+.
CREATE UNIQUE INDEX inspector_findings_key ON inspector_findings
    (scan_id, app_id, env_scope,
     COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid),
     source_table, source_column, key_path, detector);
CREATE INDEX inspector_findings_scan_rank_idx ON inspector_findings (scan_id, match_count DESC);
CREATE INDEX inspector_findings_reaper_idx ON inspector_findings (org_id, created_at);
