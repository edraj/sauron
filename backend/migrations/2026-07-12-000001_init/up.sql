-- Sauron initial schema.
-- Multi-tenancy: organization -> project -> environment.
-- Enum-like columns use TEXT + CHECK (keeps the diesel mapping to String and
-- avoids custom SQL types). gen_random_uuid() is core in PostgreSQL 13+.

-- ---------------------------------------------------------------------------
-- Organizations & users
-- ---------------------------------------------------------------------------
CREATE TABLE organizations (
    id         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name       TEXT NOT NULL,
    slug       TEXT NOT NULL UNIQUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE users (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    email         TEXT NOT NULL,
    password_hash TEXT NOT NULL,
    name          TEXT NOT NULL DEFAULT '',
    last_login_at TIMESTAMPTZ,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
-- Case-insensitive unique email.
CREATE UNIQUE INDEX users_email_lower_key ON users (lower(email));

CREATE TABLE org_members (
    id         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id     UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    user_id    UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role       TEXT NOT NULL DEFAULT 'member' CHECK (role IN ('owner', 'admin', 'member')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (org_id, user_id)
);
CREATE INDEX org_members_user_id_idx ON org_members (user_id);
CREATE INDEX org_members_org_id_idx ON org_members (org_id);

-- ---------------------------------------------------------------------------
-- Projects & environments
-- ---------------------------------------------------------------------------
CREATE TABLE projects (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id         UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    name           TEXT NOT NULL,
    slug           TEXT NOT NULL,
    platform       TEXT,
    public_key     TEXT NOT NULL UNIQUE,
    ingest_enabled BOOLEAN NOT NULL DEFAULT true,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (org_id, slug)
);

CREATE TABLE environments (
    id         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    name       TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (project_id, name)
);

-- ---------------------------------------------------------------------------
-- Issues (error grouping) & error events
-- ---------------------------------------------------------------------------
CREATE TABLE issues (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id  UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    fingerprint TEXT NOT NULL,
    type        TEXT NOT NULL DEFAULT '',
    title       TEXT NOT NULL DEFAULT '',
    culprit     TEXT NOT NULL DEFAULT '',
    level       TEXT NOT NULL DEFAULT 'error',
    status      TEXT NOT NULL DEFAULT 'unresolved'
                CHECK (status IN ('unresolved', 'resolved', 'ignored')),
    first_seen  TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen   TIMESTAMPTZ NOT NULL DEFAULT now(),
    times_seen  BIGINT NOT NULL DEFAULT 0,
    users_seen  BIGINT NOT NULL DEFAULT 0,
    assignee_id UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (project_id, fingerprint)
);
CREATE INDEX issues_list_idx ON issues (project_id, status, last_seen DESC);
CREATE INDEX issues_last_seen_idx ON issues (project_id, last_seen DESC);

CREATE TABLE error_events (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id     UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    environment_id UUID REFERENCES environments(id) ON DELETE SET NULL,
    issue_id       UUID NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    fingerprint    TEXT NOT NULL,
    level          TEXT NOT NULL DEFAULT 'error',
    message        TEXT NOT NULL DEFAULT '',
    exception_type TEXT NOT NULL DEFAULT '',
    exception_value TEXT NOT NULL DEFAULT '',
    stacktrace     JSONB NOT NULL DEFAULT '[]'::jsonb,
    breadcrumbs    JSONB NOT NULL DEFAULT '[]'::jsonb,
    context        JSONB NOT NULL DEFAULT '{}'::jsonb,
    tags           JSONB NOT NULL DEFAULT '{}'::jsonb,
    release        TEXT,
    distinct_id    TEXT,
    event_user     JSONB,
    sdk            JSONB,
    ip_address     TEXT,
    occurred_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    received_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX error_events_issue_idx ON error_events (issue_id, occurred_at DESC);
CREATE INDEX error_events_project_idx ON error_events (project_id, occurred_at DESC);
CREATE INDEX error_events_distinct_idx ON error_events (project_id, distinct_id, occurred_at DESC);

-- ---------------------------------------------------------------------------
-- Product-analytics events & people
-- ---------------------------------------------------------------------------
CREATE TABLE analytics_events (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id     UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    environment_id UUID REFERENCES environments(id) ON DELETE SET NULL,
    name           TEXT NOT NULL,
    distinct_id    TEXT NOT NULL DEFAULT '',
    properties     JSONB NOT NULL DEFAULT '{}'::jsonb,
    context        JSONB NOT NULL DEFAULT '{}'::jsonb,
    session_id     TEXT,
    release        TEXT,
    ip_address     TEXT,
    occurred_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    received_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX analytics_name_idx ON analytics_events (project_id, name, occurred_at DESC);
CREATE INDEX analytics_distinct_idx ON analytics_events (project_id, distinct_id, occurred_at DESC);
CREATE INDEX analytics_project_idx ON analytics_events (project_id, occurred_at DESC);

CREATE TABLE event_users (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id  UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    distinct_id TEXT NOT NULL,
    properties  JSONB NOT NULL DEFAULT '{}'::jsonb,
    first_seen  TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen   TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (project_id, distinct_id)
);

CREATE TABLE identities (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id  UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    alias_id    TEXT NOT NULL,
    distinct_id TEXT NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (project_id, alias_id)
);

-- ---------------------------------------------------------------------------
-- Refresh tokens (rotation / revocation)
-- ---------------------------------------------------------------------------
CREATE TABLE refresh_tokens (
    id         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id    UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL UNIQUE,
    expires_at TIMESTAMPTZ NOT NULL,
    revoked_at TIMESTAMPTZ,
    user_agent TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX refresh_tokens_user_idx ON refresh_tokens (user_id);
