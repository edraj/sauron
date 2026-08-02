-- Password reset needs a one-time-token table because there is no path back
-- from a forgotten password today: /v1/auth/password requires the current one,
-- and the only workaround (create a second account with a temp password)
-- strands the original row on users_email_lower_key so the person cannot even
-- be recreated under their own address.
--
-- Shape is a deliberate copy of refresh_tokens: a 256-bit opaque token that
-- exists only in the email, an unsalted SHA-256 of it in a UNIQUE column, an
-- explicit expires_at, a single-use marker. Three columns refresh_tokens does
-- not have:
--
--   password_fingerprint  hash_token(users.password_hash) at issue time -- a
--                         hash of a hash, never a credential. Re-checked at the
--                         write, so a link dies implicitly when the password
--                         moves for any other reason. The alternative was a
--                         sweep from every password-writing code path, which is
--                         a discipline requirement on code not yet written.
--   mode                  why the token exists; both the email copy and the
--                         audit trail read it.
--   initiated_by          NULL for self-service, the acting admin otherwise.
--
-- INVARIANT, enforced by the handlers and NOT by a CHECK:
--   (mode = 'self') = (initiated_by IS NULL).
-- Do not add that CHECK. initiated_by is ON DELETE SET NULL, that FK action
-- performs an UPDATE, and the CHECK would re-validate and fail -- so deleting an
-- admin account would error out on an unrelated user's reset row.
--
-- No index on expires_at. Both read paths lead with token_hash (a UNIQUE btree)
-- and apply expires_at > now() as a filter on the single matching row; the
-- reaper deletes on created_at. An expires_at index would be pure write
-- amplification on every insert and both UPDATE paths.
CREATE TABLE password_reset_tokens (
  id                   UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  user_id              UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  token_hash           TEXT NOT NULL UNIQUE,
  password_fingerprint TEXT NOT NULL,
  mode                 TEXT NOT NULL CHECK (mode IN ('self','admin')),
  initiated_by         UUID REFERENCES users(id) ON DELETE SET NULL,
  requested_from       TEXT,
  expires_at           TIMESTAMPTZ NOT NULL,
  consumed_at          TIMESTAMPTZ,
  invalidated_at       TIMESTAMPTZ,
  invalidated_reason   TEXT,
  created_at           TIMESTAMPTZ NOT NULL DEFAULT now()
);

COMMENT ON COLUMN password_reset_tokens.requested_from IS
  'Caller address at issue time. PROXY-BLIND whenever API_TRUST_FORWARDED_HEADERS is false, which is the default in config.rs, packaging/rpm/config/api.env and docker-compose.yml: a column full of one LAN address is the shipped topology, not a finding.';
COMMENT ON COLUMN password_reset_tokens.consumed_at IS
  'The user used this link. Split from invalidated_at on purpose: invalidated means something else killed it.';

CREATE INDEX password_reset_tokens_user_idx    ON password_reset_tokens (user_id);
CREATE INDEX password_reset_tokens_created_idx ON password_reset_tokens (created_at);

-- This is the real upgrade hazard here, and it is much larger than the new
-- table's. `User` is Selectable, so every query naming it emits an explicit
-- column list including this one. An upgraded binary against an unmigrated
-- database therefore fails login, refresh and /v1/me with a missing-column
-- error: authentication is down for the whole deployment, not just the three
-- new routes. The RPM never re-runs sauron-migrate.
--
-- NULL means the account has one. A timestamp means an admin invalidated the
-- credential and the replacement has not been chosen yet. A timestamp rather
-- than a boolean because it is also the only record of *when*, and the members
-- page renders it. Nothing indexes it: it is only ever read on a row already
-- fetched by primary key or by lower(email).
ALTER TABLE users ADD COLUMN credentials_invalidated_at TIMESTAMPTZ;
