-- Composite index for the hot permission-resolution query, which filters
-- role_grants by (user_id, org_id) on every authorized request.
CREATE INDEX IF NOT EXISTS role_grants_user_org_idx ON role_grants (user_id, org_id);
