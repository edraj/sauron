-- Grant the new pii:read / pii:manage pair to CUSTOM roles that already hold
-- org:manage. Preset roles need no UPDATE — `ensure_preset_roles` re-syncs
-- them from rbac.rs at every API boot.
--
-- The NOT EXISTS clause is the whole point. `org:manage` is INERT outside org
-- scope (`authorize_org` only ever accepts an org grant), so a custom role
-- holding it that happens to be granted at app scope is harmless today.
-- `pii:manage` is enforced by `authorize_app`, so it is fully live at app
-- scope. Granting the pair on the permission predicate alone would silently
-- promote those holders to irreversible bulk destruction of one app's data.
--
-- The condition is evaluated once. A role with zero grants qualifies and could
-- later be granted at app scope — but only by someone who already holds
-- pii:manage, because `create_grant`'s escalation check requires it.
UPDATE roles SET permissions = permissions || '["pii:read","pii:manage"]'::jsonb
WHERE org_id IS NOT NULL
  AND jsonb_typeof(permissions) = 'array'
  AND permissions @> '["org:manage"]'::jsonb
  AND NOT permissions @> '["pii:read"]'::jsonb
  AND NOT EXISTS (
    SELECT 1 FROM role_grants g WHERE g.role_id = roles.id AND g.scope_type <> 'org'
  );
