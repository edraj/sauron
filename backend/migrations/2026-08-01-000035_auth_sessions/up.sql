-- Sessions get an identity of their own.
--
-- Today a "session" has no durable name. `refresh_tokens` rows are replaced wholesale on every
-- rotation -- new id, new token_hash, new created_at -- so after fifteen minutes there is nothing
-- left to point at. That makes three things impossible: showing a user where they are logged in,
-- ending one session without ending all of them, and recording who ended it. `auth_sessions.id`
-- is that missing identity, and it is what goes into the access token's new `sid` claim.
--
-- MAINTENANCE WINDOW. This migration is not a background change. `run_pending_migrations` runs the
-- whole file in ONE transaction, so `CONCURRENTLY` is unavailable (same constraint spelled out in
-- 2026-07-28-000028_issue_env_covering_index). `ALTER TABLE refresh_tokens ADD COLUMN` takes
-- AccessExclusiveLock and holds it to COMMIT, and `refresh_tokens` is written by every login,
-- refresh, logout and password change. The costs, largest first:
--   1. `refresh_tokens_session_idx` -- a full heap scan. The table has exactly one index today
--      (refresh_tokens_user_idx) and nothing has ever reaped it; a deployment live for a year with
--      50 active sessions holds roughly 1.7M rows. Making the index partial does not avoid the
--      scan, it bounds the resulting index to live sessions.
--   2. The backfill UPDATE -- same scan, negligible write volume: every rotated row is already
--      revoked, so `revoked_at IS NULL AND expires_at > now()` matches about one row per active
--      session.
--   3. The ALTER itself -- metadata-only for a nullable column with no default.
-- Do NOT try to bound the backfill with `created_at > now() - interval '30 days'`. It is redundant
-- under the default JWT_REFRESH_TTL_SECS (2592000 = 30 days, and expires_at = created_at + ttl) and
-- silently lossy if an operator raised that TTL: live tokens outside the window would keep
-- session_id IS NULL and their owners' current sessions would be unmanageable, with no error
-- anywhere. `expires_at > now()` is the correct liveness predicate and it is already minimal.
--
-- Column notes worth keeping:
--   * last_used_at is stamped on ROTATION, not per request, so "last used" is accurate only to
--     within JWT_ACCESS_TTL_SECS. A session used 30 seconds ago can display as "15 minutes ago".
--     Do not "fix" this by writing on every request -- that turns a read-only auth path into a
--     write on every API call.
--   * expires_at mirrors the newest refresh token's expiry (sliding, matching today's behaviour)
--     so liveness needs no join.
--   * revoked_by is ON DELETE SET NULL, not CASCADE: deleting the admin must not delete the
--     victim's audit row.
--   * The CHECK deliberately EXCLUDES 'rotated'. A rotation revokes a token, never a session, so
--     writing 'rotated' here is a bug the database catches. Note that refresh_tokens.revoked_reason
--     has no CHECK, so the two columns share a vocabulary and only one enforces it. THIS CHECK IS A
--     DEPLOY COUPLING: adding a reason in code without a widening migration produces a 500 on the
--     revoke path.
--   * 'password_reset' and 'reset_forced' are listed from day one even though nothing in this slice
--     writes them. They belong to the password-reset slice that lands next and revokes sessions on
--     both of its reset paths, one of them unauthenticated. Arriving with that slice instead, every
--     successful reset would 500 at the revoke step until a second migration caught up -- landing
--     on a user who has just proved they cannot get into their account. Widening the list costs
--     nothing while the table is created empty in this same transaction.
--
-- refresh_tokens.session_id is ON DELETE SET NULL, NOT CASCADE. CASCADE would pre-authorise a real
-- failure: deleting one auth_sessions row would take that session's whole token history with it,
-- and `refresh_token_revocation` -- which reads revoked rows regardless of state and is the entire
-- replay signal -- would then find nothing and treat a replayed token as "never existed": a plain
-- 401, no family kill, no WARN. The 30-day reaper deletes auth_sessions rows by design, so this is
-- not hypothetical.
--
-- auth_sessions_user_live_idx is (user_id) WHERE revoked_at IS NULL and deliberately does NOT
-- include last_used_at. Indexing it would make every rotation a non-HOT update -- the
-- ON CONFLICT DO UPDATE would rewrite the heap tuple AND both index entries, leaving two dead
-- versions for autovacuum on the hottest-updated column in the table. With only id and user_id
-- indexed and neither changing on rotation, the update is HOT-eligible and the page self-vacuums.
-- The ordering the index would have provided buys nothing: the query is scoped to one user_id,
-- capped at 200 rows, and a real account has single-digit live sessions.
--
-- TODO: `refresh_tokens` is unbounded and unreaped -- roughly 96 rows/day per active session. This
-- migration makes it materially worse (a second index means more write amplification and more disk
-- on the fastest-growing never-pruned table). A reaper must delete on EXPIRY only, never merely
-- because a row is revoked: revoked rows are the whole replay signal.

CREATE TABLE auth_sessions (
  id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  user_id        UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
  last_used_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
  expires_at     TIMESTAMPTZ NOT NULL,
  user_agent     TEXT,
  ip             TEXT,
  revoked_at     TIMESTAMPTZ,
  revoked_reason TEXT,
  revoked_by     UUID REFERENCES users(id) ON DELETE SET NULL,
  CONSTRAINT auth_sessions_revoked_reason_check CHECK (
    revoked_reason IS NULL OR revoked_reason IN (
      'logout','user_revoked','user_revoked_others','admin_revoked',
      'password_changed','deactivated','reuse',
      'password_reset','reset_forced')
  )
);

CREATE INDEX auth_sessions_user_live_idx
  ON auth_sessions (user_id) WHERE revoked_at IS NULL;

CREATE INDEX auth_sessions_revoked_idx
  ON auth_sessions (revoked_at) WHERE revoked_at IS NOT NULL;

ALTER TABLE refresh_tokens
  ADD COLUMN session_id UUID REFERENCES auth_sessions(id) ON DELETE SET NULL;

INSERT INTO auth_sessions (id, user_id, created_at, last_used_at, expires_at, user_agent)
  SELECT r.id, r.user_id, r.created_at, r.created_at, r.expires_at, r.user_agent
    FROM refresh_tokens r
   WHERE r.revoked_at IS NULL AND r.expires_at > now();

UPDATE refresh_tokens r SET session_id = r.id
 WHERE r.revoked_at IS NULL AND r.expires_at > now();

CREATE INDEX refresh_tokens_session_idx
  ON refresh_tokens (session_id) WHERE session_id IS NOT NULL;

-- Seed the new `member:credential` permission. The predicate matches member:manage HOLDERS, not
-- the preset names: member:credential is carved OUT of member:manage rather than added beside it,
-- so every role that holds member:manage today can already sign a member out via
-- deactivate-then-reactivate. Matching on preset names would silently strip that from every custom
-- role an operator has built while leaving Owner and Admin whole. `ensure_preset_roles` re-syncs
-- Owner and Admin from rbac.rs at api startup regardless, so the presets are covered twice and the
-- custom roles only here.
UPDATE roles SET permissions = permissions || '["member:credential"]'::jsonb
 WHERE permissions @> '["member:manage"]'::jsonb
   AND NOT (permissions @> '["member:credential"]'::jsonb);
