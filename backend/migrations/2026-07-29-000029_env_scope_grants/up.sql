-- `role_grants.scope_type` gains 'env', making an environment a grantable
-- scope level (Slice 3). The CHECK constraint created in
-- 2026-07-12-000002_projects_apps_rbac was unnamed, so it carries Postgres's
-- auto-generated name `role_grants_scope_type_check`.
--
-- `scope_id` remains polymorphic with no FK, exactly as for 'app' and
-- 'project' — a retired environment's grants outlive it, which is why the
-- dashboard's grant editor carries an `unmatched` list.
ALTER TABLE role_grants DROP CONSTRAINT role_grants_scope_type_check;
ALTER TABLE role_grants
    ADD CONSTRAINT role_grants_scope_type_check
    CHECK (scope_type IN ('org', 'project', 'app', 'env'));
