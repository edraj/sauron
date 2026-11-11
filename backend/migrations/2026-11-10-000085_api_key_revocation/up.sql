-- Why and by whom a personal API key was revoked, so the owner's Account page
-- and the admin's Members page can say what happened ("revoked by a password
-- reset", "revoked by an admin") instead of a bare timestamp.
--
-- The reasons are the `auth_sessions.revoked_reason` vocabulary, for the same
-- events: the owner revoked it, an admin revoked it ("End all sessions" or
-- "Revoke API keys"), an admin forced a password reset, or the owner reset a
-- forgotten password through the emailed link. Rows revoked before this
-- migration keep a NULL reason: unknown, never guessed.
--
-- `revoked_by` is ON DELETE SET NULL, as on `auth_sessions`: deleting the
-- admin must not delete the record of what they did. NULL for a reset through
-- the emailed link, whose actor proved control of a mailbox, not an identity.
ALTER TABLE api_keys
    ADD COLUMN revoked_reason text NULL,
    ADD COLUMN revoked_by uuid NULL REFERENCES users(id) ON DELETE SET NULL,
    ADD CONSTRAINT api_keys_revoked_reason_check CHECK (
        revoked_reason IS NULL
        OR revoked_reason IN ('user_revoked', 'admin_revoked', 'reset_forced', 'password_reset')
    ),
    -- A reason only ever describes a revocation.
    ADD CONSTRAINT api_keys_reason_needs_revoked_at CHECK (
        revoked_reason IS NULL OR revoked_at IS NOT NULL
    );

-- Every key of one user, revoked or not: the owner's "Show revoked" list, the
-- Members page summary, and the probe the `user_id -> users ON DELETE CASCADE`
-- foreign key needs (the live-only partial index cannot serve it).
CREATE INDEX api_keys_user_idx ON api_keys (user_id, revoked_at DESC);

-- `revoked_by -> users ON DELETE SET NULL` probes this when a user is deleted.
CREATE INDEX api_keys_revoked_by_idx ON api_keys (revoked_by) WHERE revoked_by IS NOT NULL;
