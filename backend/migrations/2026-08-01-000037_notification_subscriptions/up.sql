-- Per-user notification subscriptions. Every notification this product could
-- send before this migration was org-owned and admin-typed: an `alert_rules`
-- row belongs to an organization and its `notification_channels` carry a
-- static recipient list somebody pasted into a dialog. A developer who wanted
-- to know when their own app broke had to ask an admin, and everyone else on
-- that channel got it too.
--
-- READ THIS BEFORE TOUCHING EITHER `environment_id` COLUMN. There are two
-- environment id spaces in this schema and getting them backwards produces a
-- subscription that looks right in the database and matches nothing, silently:
--
--   * `environments`      -- the PROJECT-LEVEL CATALOGUE (since migration 33).
--                            One row means "prod, everywhere in this project".
--   * `app_environments`  -- the PER-APP ENROLLMENT. This is what
--                            `error_events.environment_id`,
--                            `analytics_events.environment_id` and
--                            `role_grants.scope_id` (scope_type='env') hold.
--
-- `notification_subscription_envs.environment_id` stores CATALOGUE ids on
-- purpose: the catalogue is exactly the wildcard RBAC lacks, and it stays
-- correct when a new app is created and auto-enrolled. Storing enrollment ids
-- would freeze the set at creation time.
-- `notification_queue_envs.environment_id` stores ENROLLMENT ids, because that
-- is what the event rows the body was computed from actually carry.
--
-- `notification_queue` exists rather than enqueueing straight into
-- `mail_outbox` so that producers never send mail. That split is what lets
-- `sauron-monitor` participate in personal uptime notifications without ever
-- learning about SMTP, and it is what makes delivery exclusive across
-- replicas via a `claimed` status.

CREATE TABLE notification_subscriptions (
    id                UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id           UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    org_id            UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    scope_type        TEXT NOT NULL CHECK (scope_type IN ('project','app')),
    -- Polymorphic with no FK, exactly like `role_grants.scope_id`: a row can
    -- outlive its target, so every read path must tolerate an unresolvable id.
    scope_id          UUID NOT NULL,
    kind              TEXT NOT NULL CHECK (kind IN
                          ('uptime','error_spike','error_new_issue','error_regression')),
    enabled           BOOLEAN NOT NULL DEFAULT true,
    disabled_reason   TEXT CHECK (disabled_reason IN ('unsubscribed','access_revoked')),
    disabled_at       TIMESTAMPTZ,
    conditions        JSONB NOT NULL DEFAULT '{}'::jsonb,
    delivery          TEXT NOT NULL DEFAULT 'immediate'
                          CHECK (delivery IN ('immediate','hourly','daily')),
    throttle_seconds  INT NOT NULL DEFAULT 900 CHECK (throttle_seconds BETWEEN 0 AND 604800),
    quiet_start_min   SMALLINT CHECK (quiet_start_min BETWEEN 0 AND 1439),
    quiet_end_min     SMALLINT CHECK (quiet_end_min BETWEEN 0 AND 1439),
    -- An IANA name, validated at write time against `pg_timezone_names`. The
    -- enqueue re-checks it: a zone that validated at write time can vanish with
    -- an OS tzdata update, and `now() AT TIME ZONE 'Missing/Zone'` raises.
    quiet_tz          TEXT NOT NULL DEFAULT 'UTC',
    -- Seeded to now() at INSERT so a brand-new subscription cannot retro-storm
    -- over whatever backlog already exists.
    last_evaluated_at TIMESTAMPTZ,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK ((quiet_start_min IS NULL) = (quiet_end_min IS NULL))
);

CREATE UNIQUE INDEX notification_subscriptions_user_scope_kind_key
    ON notification_subscriptions (user_id, scope_type, scope_id, kind);
CREATE INDEX notification_subscriptions_kind_idx
    ON notification_subscriptions (kind) WHERE enabled;
CREATE INDEX notification_subscriptions_user_idx ON notification_subscriptions (user_id);
CREATE INDEX notification_subscriptions_org_idx  ON notification_subscriptions (org_id);

-- Composite PK with no surrogate id, mirroring `alert_rule_channels`.
-- An EMPTY set means all environments, including unattributed events.
CREATE TABLE notification_subscription_envs (
    subscription_id UUID NOT NULL REFERENCES notification_subscriptions(id) ON DELETE CASCADE,
    environment_id  UUID NOT NULL REFERENCES environments(id) ON DELETE CASCADE,
    PRIMARY KEY (subscription_id, environment_id)
);
CREATE INDEX notification_subscription_envs_env_idx
    ON notification_subscription_envs (environment_id);

COMMENT ON COLUMN notification_subscription_envs.environment_id IS
    'CATALOGUE id (environments.id), NOT an app_environments enrollment id.';

CREATE TABLE notification_queue (
    id                    UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    subscription_id       UUID NOT NULL REFERENCES notification_subscriptions(id) ON DELETE CASCADE,
    user_id               UUID NOT NULL,
    org_id                UUID NOT NULL,
    project_id            UUID NOT NULL,
    app_id                UUID,
    includes_unattributed BOOLEAN NOT NULL DEFAULT false,
    kind                  TEXT NOT NULL,
    dedup_key             TEXT NOT NULL,
    severity              TEXT NOT NULL DEFAULT 'warning'
                              CHECK (severity IN ('info','warning','critical')),
    -- Nullable because the drain blanks all three in the same UPDATE that marks
    -- a row `dropped_no_access`: the content has no further purpose and must not
    -- sit at rest for the retention window outside the reader's authorization.
    title                 TEXT,
    body                  TEXT,
    link                  TEXT,
    occurred_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    deliver_after         TIMESTAMPTZ NOT NULL,
    status                TEXT NOT NULL DEFAULT 'pending' CHECK (status IN
                              ('pending','claimed','sent','dropped_no_access',
                               'dropped_inactive','dropped_unsubscribed','failed')),
    attempts              SMALLINT NOT NULL DEFAULT 0,
    message_id            UUID,
    claimed_at            TIMESTAMPTZ,
    sent_at               TIMESTAMPTZ,
    finished_at           TIMESTAMPTZ,
    error                 TEXT,
    created_at            TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- DELIBERATELY NO FOREIGN KEY on environment_id. A cascade delete would
-- silently SHRINK a row's environment list, and an empty list is read as "the
-- body spans everything" -- so a deleted enrollment would WIDEN a queue row's
-- implied scope instead of narrowing it. An unresolvable enrollment id is
-- simply unreachable at drain time, which fails closed.
CREATE TABLE notification_queue_envs (
    queue_id       UUID NOT NULL REFERENCES notification_queue(id) ON DELETE CASCADE,
    environment_id UUID NOT NULL,
    PRIMARY KEY (queue_id, environment_id)
);

COMMENT ON COLUMN notification_queue_envs.environment_id IS
    'ENROLLMENT id (app_environments.id), NOT a catalogue environments.id. No FK by design.';

CREATE INDEX notification_queue_due_idx
    ON notification_queue (deliver_after) WHERE status = 'pending';

-- The explicit ON CONFLICT target for the enqueue. Without a unique constraint
-- `ON CONFLICT DO NOTHING` can only ever fire on the id PK -- i.e. never -- and
-- the clause would read as idempotency while providing none. Scoped to LIVE
-- rows so a row that already sent does not block the next legitimate one.
CREATE UNIQUE INDEX notification_queue_live_dedup_key
    ON notification_queue (subscription_id, dedup_key) WHERE status IN ('pending','claimed');

CREATE INDEX notification_queue_user_created_idx ON notification_queue (user_id, created_at DESC);
CREATE INDEX notification_queue_user_sent_idx
    ON notification_queue (user_id, sent_at DESC) WHERE status = 'sent';
CREATE INDEX notification_queue_finished_idx
    ON notification_queue (finished_at) WHERE finished_at IS NOT NULL;

-- Unlike `alert_events`, this is a work queue: every notification costs one
-- INSERT plus two UPDATEs, `status` appears in a partial index predicate so
-- neither update is HOT-eligible, and three heap versions per row against
-- default autovacuum thresholds leaves a bloated heap the prune must scan.
ALTER TABLE notification_queue
    SET (autovacuum_vacuum_scale_factor = 0.02, autovacuum_analyze_scale_factor = 0.01);
