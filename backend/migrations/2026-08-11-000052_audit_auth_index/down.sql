-- Dropping these costs no data — the rows stay, only the access paths go. The
-- default Wall of Shame view degrades to a scan filtered by `entity_type <>
-- 'auth'` over `audit_log_org_time_idx`.
DROP INDEX IF EXISTS audit_log_org_auth_idx;
DROP INDEX IF EXISTS audit_log_org_actor_admin_idx;
DROP INDEX IF EXISTS audit_log_org_time_admin_idx;
