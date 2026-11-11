DROP INDEX IF EXISTS api_keys_revoked_by_idx;
DROP INDEX IF EXISTS api_keys_user_idx;
ALTER TABLE api_keys
    DROP CONSTRAINT IF EXISTS api_keys_reason_needs_revoked_at,
    DROP CONSTRAINT IF EXISTS api_keys_revoked_reason_check,
    DROP COLUMN IF EXISTS revoked_by,
    DROP COLUMN IF EXISTS revoked_reason;
