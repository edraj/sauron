-- Reverse 0002. NOTE: lossy when a project groups more than one app (the
-- grouping collapses back into per-app "projects"). Acceptable pre-release.

-- Recreate org_members from org-scoped grants.
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

INSERT INTO org_members (org_id, user_id, role)
    SELECT DISTINCT ON (g.org_id, g.user_id) g.org_id, g.user_id,
        CASE r.name WHEN 'Owner' THEN 'owner' WHEN 'Admin' THEN 'admin' ELSE 'member' END
    FROM role_grants g JOIN roles r ON r.id = g.role_id
    WHERE g.scope_type = 'org'
    ORDER BY g.org_id, g.user_id,
        CASE r.name WHEN 'Owner' THEN 0 WHEN 'Admin' THEN 1 ELSE 2 END;

DROP TABLE role_grants;
DROP TABLE roles;

-- Repoint signal tables app_id → project_id.
ALTER TABLE identities      RENAME COLUMN app_id TO project_id;
ALTER TABLE event_users     RENAME COLUMN app_id TO project_id;
ALTER TABLE analytics_events RENAME COLUMN app_id TO project_id;
ALTER TABLE error_events    RENAME COLUMN app_id TO project_id;
ALTER TABLE issues          RENAME COLUMN app_id TO project_id;
ALTER TABLE environments    RENAME COLUMN app_id TO project_id;

-- Fold apps back into projects (restore org_id, drop grouping columns).
ALTER TABLE apps ADD COLUMN org_id UUID REFERENCES organizations(id) ON DELETE CASCADE;
UPDATE apps a SET org_id = p.org_id FROM projects p WHERE p.id = a.project_id;
ALTER TABLE apps ALTER COLUMN org_id SET NOT NULL;
ALTER TABLE apps DROP CONSTRAINT apps_project_slug_key;
ALTER TABLE apps ADD CONSTRAINT projects_org_id_slug_key UNIQUE (org_id, slug);
ALTER TABLE apps DROP COLUMN project_id;
ALTER TABLE apps DROP COLUMN app_type;

DROP TABLE projects;
ALTER TABLE apps RENAME TO projects;
