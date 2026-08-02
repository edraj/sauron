-- Order is load-bearing: the referencing column must go before the referenced table.
-- This is a real inverse; it loses session history, which is acceptable because the pre-migration
-- system had none.
--
-- The permission is stripped unconditionally rather than only from member:manage holders, because
-- a role edited between the up and the down could hold one without the other.
UPDATE roles SET permissions = permissions - 'member:credential';
DROP INDEX IF EXISTS refresh_tokens_session_idx;
ALTER TABLE refresh_tokens DROP COLUMN IF EXISTS session_id;  -- drops the FK with it
DROP INDEX IF EXISTS auth_sessions_revoked_idx;
DROP INDEX IF EXISTS auth_sessions_user_live_idx;
DROP TABLE IF EXISTS auth_sessions;
