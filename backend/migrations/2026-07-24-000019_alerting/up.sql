-- Admin-customizable alerting / notification engine.
--
-- Three concerns:
--   notification_channels  — where an alert is delivered (email/slack/discord/matrix/telegram/webhook).
--   alert_rules            — when to fire (trigger type + fully-customizable conditions) and how to phrase it.
--   alert_rule_channels    — which channels a rule fans out to (many-to-many).
--   alert_events           — every fired/throttled/failed delivery, for history + dedup/throttle state.
--
-- Channels and rules are ORG-scoped: an admin configures them once for the org and can
-- narrow a rule to a project/app. Channel secrets (SMTP password, bot tokens, webhook URLs)
-- are AES-GCM encrypted at rest in `secret_enc` and never returned by the API.

CREATE TABLE notification_channels (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id      UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    kind        TEXT NOT NULL CHECK (kind IN ('email','slack','discord','matrix','telegram','webhook')),
    -- Non-secret, kind-specific settings (e.g. smtp host/port/from/to, matrix room, headers).
    config      JSONB NOT NULL DEFAULT '{}'::jsonb,
    -- AES-GCM ciphertext (nonce-prefixed) of the secret bundle for this channel. NULL = no secret set.
    secret_enc  BYTEA,
    enabled     BOOL NOT NULL DEFAULT TRUE,
    created_by  UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX notification_channels_org_idx ON notification_channels (org_id);

CREATE TABLE alert_rules (
    id                UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id            UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    -- Optional narrowing scope. NULL project = whole org; NULL app = whole project.
    project_id        UUID REFERENCES projects(id) ON DELETE CASCADE,
    app_id            UUID REFERENCES apps(id) ON DELETE CASCADE,
    name              TEXT NOT NULL,
    trigger_type      TEXT NOT NULL CHECK (trigger_type IN (
                          'monitor_down','monitor_up',
                          'issue_new','issue_regression',
                          'error_threshold','error_spike',
                          'event_threshold','perf_degradation')),
    enabled           BOOL NOT NULL DEFAULT TRUE,
    -- Fully-customizable condition bag: threshold, comparator, window_seconds, spike_factor,
    -- metric, and a filters object (level/environment/event_name/tag/http_status/op...).
    conditions        JSONB NOT NULL DEFAULT '{}'::jsonb,
    severity          TEXT NOT NULL DEFAULT 'warning'
                          CHECK (severity IN ('info','warning','critical')),
    -- Per-rule dedup window: suppress repeat deliveries of the same dedup key for this long.
    throttle_seconds  INT NOT NULL DEFAULT 300,
    -- Optional admin-authored message template with {{variable}} placeholders.
    message_template  TEXT,
    -- High-water mark for the periodic evaluator (metric triggers); NULL = never evaluated.
    last_evaluated_at TIMESTAMPTZ,
    created_by        UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX alert_rules_org_idx ON alert_rules (org_id);
CREATE INDEX alert_rules_enabled_trigger_idx ON alert_rules (trigger_type) WHERE enabled;
CREATE INDEX alert_rules_project_idx ON alert_rules (project_id) WHERE project_id IS NOT NULL;
CREATE INDEX alert_rules_app_idx ON alert_rules (app_id) WHERE app_id IS NOT NULL;

CREATE TABLE alert_rule_channels (
    rule_id     UUID NOT NULL REFERENCES alert_rules(id) ON DELETE CASCADE,
    channel_id  UUID NOT NULL REFERENCES notification_channels(id) ON DELETE CASCADE,
    PRIMARY KEY (rule_id, channel_id)
);
CREATE INDEX alert_rule_channels_channel_idx ON alert_rule_channels (channel_id);

CREATE TABLE alert_events (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id        UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    rule_id       UUID REFERENCES alert_rules(id) ON DELETE SET NULL,
    channel_id    UUID REFERENCES notification_channels(id) ON DELETE SET NULL,
    trigger_type  TEXT NOT NULL,
    dedup_key     TEXT NOT NULL,
    status        TEXT NOT NULL CHECK (status IN ('sent','failed','throttled','skipped')),
    title         TEXT NOT NULL DEFAULT '',
    body          TEXT NOT NULL DEFAULT '',
    error         TEXT,
    attempts      INT NOT NULL DEFAULT 0,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX alert_events_org_time_idx ON alert_events (org_id, created_at DESC);
CREATE INDEX alert_events_rule_time_idx ON alert_events (rule_id, created_at DESC);
-- Fast "did we already deliver this dedup key recently?" lookup for throttling.
CREATE INDEX alert_events_dedup_idx ON alert_events (dedup_key, created_at DESC);
