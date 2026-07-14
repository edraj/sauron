-- 0002: introduce Project (grouping) → App (ingest unit) + fine-grained RBAC.
--
-- The old `projects` table (which held the DSN) becomes `apps`. A new `projects`
-- table is created above as the grouping. All signal tables move from
-- project_id → app_id. org_members is replaced by roles + role_grants.

-- ---------------------------------------------------------------------------
-- 1. Rename the DSN-holder to `apps` and add app columns.
-- ---------------------------------------------------------------------------
ALTER TABLE projects RENAME TO apps;
ALTER TABLE apps
    ADD COLUMN app_type TEXT NOT NULL DEFAULT 'web'
        CHECK (app_type IN ('web', 'flutter', 'ios', 'android', 'react_native', 'node'));
-- `project_id` is added in step 3, after the new `projects` table exists.

-- ---------------------------------------------------------------------------
-- 2. Create the new grouping `projects`.
-- ---------------------------------------------------------------------------
CREATE TABLE projects (
    id         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id     UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    name       TEXT NOT NULL,
    slug       TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (org_id, slug)
);

-- ---------------------------------------------------------------------------
-- 3. Backfill: one grouping project per existing app; link apps to it.
-- ---------------------------------------------------------------------------
ALTER TABLE apps ADD COLUMN project_id UUID REFERENCES projects(id) ON DELETE CASCADE;

INSERT INTO projects (org_id, name, slug, created_at, updated_at)
    SELECT org_id, name, slug, created_at, updated_at FROM apps;

UPDATE apps a
    SET project_id = p.id
    FROM projects p
    WHERE p.org_id = a.org_id AND p.slug = a.slug;

ALTER TABLE apps ALTER COLUMN project_id SET NOT NULL;

-- Slug is now unique per project (not per org); org_id is derivable via project.
ALTER TABLE apps DROP CONSTRAINT projects_org_id_slug_key;
ALTER TABLE apps ADD CONSTRAINT apps_project_slug_key UNIQUE (project_id, slug);
ALTER TABLE apps DROP COLUMN org_id;

-- ---------------------------------------------------------------------------
-- 4. Repoint signal tables from project_id → app_id.
-- ---------------------------------------------------------------------------
ALTER TABLE environments    RENAME COLUMN project_id TO app_id;
ALTER TABLE issues          RENAME COLUMN project_id TO app_id;
ALTER TABLE error_events    RENAME COLUMN project_id TO app_id;
ALTER TABLE analytics_events RENAME COLUMN project_id TO app_id;
ALTER TABLE event_users     RENAME COLUMN project_id TO app_id;
ALTER TABLE identities      RENAME COLUMN project_id TO app_id;

-- ---------------------------------------------------------------------------
-- 5. RBAC: roles + role_grants (replacing org_members).
-- ---------------------------------------------------------------------------
CREATE TABLE roles (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id      UUID REFERENCES organizations(id) ON DELETE CASCADE,  -- NULL = system preset
    name        TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    is_system   BOOLEAN NOT NULL DEFAULT false,
    permissions JSONB NOT NULL DEFAULT '[]'::jsonb,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX roles_system_name_key ON roles (name) WHERE org_id IS NULL;
CREATE UNIQUE INDEX roles_org_name_key ON roles (org_id, name) WHERE org_id IS NOT NULL;

CREATE TABLE role_grants (
    id         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id     UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    user_id    UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role_id    UUID NOT NULL REFERENCES roles(id) ON DELETE CASCADE,
    scope_type TEXT NOT NULL CHECK (scope_type IN ('org', 'project', 'app')),
    scope_id   UUID NOT NULL,   -- org_id / project_id / app_id (polymorphic, no FK)
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (user_id, role_id, scope_type, scope_id)
);
CREATE INDEX role_grants_user_idx ON role_grants (user_id);
CREATE INDEX role_grants_org_idx ON role_grants (org_id);
CREATE INDEX role_grants_scope_idx ON role_grants (scope_type, scope_id);

-- ---------------------------------------------------------------------------
-- 6. Seed the four preset roles (org_id NULL, usable at any scope).
-- ---------------------------------------------------------------------------
INSERT INTO roles (org_id, name, description, is_system, permissions) VALUES
(NULL, 'Owner', 'Full control including organization settings', true,
 '["issue:read","issue:write","event:read","app:read","app:create","app:update","app:delete","app:rotate_key","project:read","project:create","project:update","project:delete","member:read","member:manage","role:manage","org:manage"]'::jsonb),
(NULL, 'Admin', 'Manage projects, apps, members and roles', true,
 '["issue:read","issue:write","event:read","app:read","app:create","app:update","app:delete","app:rotate_key","project:read","project:create","project:update","project:delete","member:read","member:manage","role:manage"]'::jsonb),
(NULL, 'Developer', 'Work with issues and apps', true,
 '["issue:read","issue:write","event:read","app:read","app:create","app:update","app:rotate_key","project:read","member:read"]'::jsonb),
(NULL, 'Viewer', 'Read-only access', true,
 '["issue:read","event:read","app:read","project:read","member:read"]'::jsonb);

-- ---------------------------------------------------------------------------
-- 7. Convert existing org_members into org-scoped role_grants, then drop it.
--    owner→Owner, admin→Admin, member→Developer.
-- ---------------------------------------------------------------------------
INSERT INTO role_grants (org_id, user_id, role_id, scope_type, scope_id)
    SELECT m.org_id, m.user_id, r.id, 'org', m.org_id
    FROM org_members m
    JOIN roles r ON r.is_system AND r.name = CASE m.role
        WHEN 'owner' THEN 'Owner'
        WHEN 'admin' THEN 'Admin'
        ELSE 'Developer'
    END;

DROP TABLE org_members;
