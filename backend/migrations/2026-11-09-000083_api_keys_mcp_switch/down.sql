ALTER TABLE organizations DROP COLUMN mcp_enabled;
DROP INDEX IF EXISTS api_keys_user_live_idx;
DROP TABLE IF EXISTS api_keys;
