-- An admin can change a member's email, but not unilaterally: `users.email` is
-- the login identity (users_email_lower_key), so an admin who could move it
-- silently would hold an account-takeover primitive -- park the account on an
-- address they control, then use the ordinary forgotten-password flow. The
-- change therefore waits here until the NEW address confirms, and the OLD
-- address is mailed a link that stops it.
--
-- Two token columns on one row, not two rows. A pending change is one fact with
-- two secrets; splitting them would make "cancel kills the matching approve" a
-- join every caller has to remember instead of a property of the row.
--
--   email_fingerprint  lower(users.email) at issue time. Re-checked inside the
--                      same UPDATE that burns the approve token, so "the address
--                      has not moved since this link was issued" and "the link
--                      is spent" are one atomic fact rather than two statements
--                      with a window between them. Same job password_fingerprint
--                      does in 000036, one statement tighter. Plaintext, not
--                      hashed: unlike a password hash this is not a credential,
--                      and the approve path already holds the value it compares
--                      against.
--   org_id             confirm/cancel are UNAUTHENTICATED -- the token is all
--                      they have, and the audit entry must land in an org.
--
-- INVARIANT enforced by the handlers, NOT by a CHECK: initiated_by is never NULL
-- at insert. Do NOT add that CHECK. The FK is ON DELETE SET NULL, that action
-- performs an UPDATE, and the UPDATE re-validates every CHECK on the row -- so
-- deleting an admin account would error out on an unrelated user's request row.
-- 000036 documents this trap; it applies here verbatim.
--
-- No index on expires_at. Every read path leads with a token hash (UNIQUE btree)
-- or with user_id/org_id (the partial indexes below) and applies
-- expires_at > now() as a filter on the matching rows; the reaper deletes on
-- created_at. An expires_at index would be pure write amplification.
CREATE TABLE email_change_requests (
  id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  user_id             UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  org_id              UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
  new_email           TEXT NOT NULL,
  approve_token_hash  TEXT NOT NULL UNIQUE,
  cancel_token_hash   TEXT NOT NULL UNIQUE,
  email_fingerprint   TEXT NOT NULL,
  initiated_by        UUID REFERENCES users(id) ON DELETE SET NULL,
  requested_from      TEXT,
  expires_at          TIMESTAMPTZ NOT NULL,
  approved_at         TIMESTAMPTZ,
  cancelled_at        TIMESTAMPTZ,
  cancelled_reason    TEXT CHECK (cancelled_reason IN ('user','admin','superseded')),
  created_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- THE supersede guarantee, and the reason it is here rather than in the handler.
-- Doing it in the application -- "invalidate the old one, then insert" -- loses a
-- race between two concurrent admins and leaves two live approve links, which is
-- a mistyped domain holding a standing claim on the account for 24 hours. Here
-- the loser gets a constraint error the handler maps to 409.
CREATE UNIQUE INDEX email_change_one_live_per_user
  ON email_change_requests (user_id)
  WHERE approved_at IS NULL AND cancelled_at IS NULL;

-- The members list reads this to badge pending rows.
CREATE INDEX email_change_live_by_org
  ON email_change_requests (org_id)
  WHERE approved_at IS NULL AND cancelled_at IS NULL;
