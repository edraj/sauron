-- Personal API keys (MCP server) and the per-organization MCP switch.
CREATE TABLE api_keys (
    id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id      uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name         text NOT NULL,
    token_prefix text NOT NULL,
    token_hash   text NOT NULL UNIQUE,
    created_at   timestamptz NOT NULL DEFAULT now(),
    expires_at   timestamptz,
    last_used_at timestamptz,
    revoked_at   timestamptz
);

CREATE INDEX api_keys_user_live_idx ON api_keys (user_id) WHERE revoked_at IS NULL;

ALTER TABLE organizations ADD COLUMN mcp_enabled boolean NOT NULL DEFAULT true;
