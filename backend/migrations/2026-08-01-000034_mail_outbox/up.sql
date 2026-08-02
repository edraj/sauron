-- A durable outbox for transactional email: the deployment sends a message to
-- ONE PERSON, off the request path, and can prove it happened.
--
-- Why a table at all. `sauron-alerts` already sends email, but only through a
-- per-org `notification_channels` row whose SMTP credentials the org's admin
-- owns. A password-reset link routed that way tells an arbitrary org admin that
-- one of their members asked for a reset, and strands entirely a user who
-- belongs to no org. This table is addressed to a person, so it deliberately
-- has NO org_id.
--
-- Why not a bare tokio::spawn. A spawned send dies with the process, and a lost
-- reset mail is unrecoverable for a user who has already spent their rate-limit
-- bucket. Why not Redis: `RedisStore` sets `response_timeout(None)`, so a
-- command against a dead Redis sits through reconnect for 9-19 seconds — on the
-- auth path.
--
-- `kind` deliberately has NO CHECK, deviating from the house TEXT+CHECK rule.
-- The value set keeps growing after this migration lands, and the slice that
-- adds the fifth kind must not also have to widen a CHECK on a table holding
-- live credentials. The authority is `sauron_mail::MailKind`, which also owns
-- each kind's dedup window — two things that must change together, so splitting
-- one of them into SQL guarantees drift. `status` DOES have a CHECK: this
-- migration owns every value it can take.
--
-- A pending row holds a live credential. Before this table, a read-only database
-- compromise — a backup, a replica, an SQL injection — could not take over an
-- account: password hashes are Argon2 and refresh tokens are stored hashed. A
-- `body_html` containing a working reset URL hands over accounts outright. The
-- bound on that exposure is min(delivery time, the row's own `expires_at`): the
-- body is blanked the moment the row reaches 'sent'/'sink', and a hygiene sweep
-- blanks ANY row's body once it is past `expires_at`, regardless of status.
-- Nothing recoverable is lost, because the claim query refuses an expired row
-- anyway — a body that survived that instant could never be delivered, only
-- stolen.
--
-- `expires_at` is what stops a stale message being delivered on revoked
-- authorization. EVERY enqueue sets it explicitly, from the lifetime of whatever
-- the body carries. The DEFAULT below is only a backstop for a row an operator
-- writes by hand: a reader who takes the one hour there for the real policy will
-- scrub 24-hour admin-initiated reset mail early, while its token is still live
-- and the row is still the only thing an operator can requeue.
--
-- `max_attempts` is a column, not a config knob, so an operator can bump one
-- stuck row. Combined with the fact that failing a row does NOT blank its body,
-- a failed row can be resurrected for as long as its body survives:
--   UPDATE mail_outbox SET status='pending', attempts=0, next_attempt_at=now(),
--          expires_at=now()+interval '10 minutes' WHERE id=...;
--
-- `recipient_key` is the parsed, lowercased envelope address. It exists so the
-- per-recipient cap cannot be walked around: `register` validates addresses with
-- `req.email.contains('@')` alone, and lettre's parser discards the unparsed
-- remainder, so `victim@corp.test`, `victim@corp.test ` and
-- `victim@corp.test <x>` are three `users.email` rows that deliver to one mailbox.

CREATE TABLE mail_outbox (
  id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  kind            TEXT NOT NULL,
  recipient       TEXT NOT NULL,
  recipient_key   TEXT NOT NULL,
  subject         TEXT NOT NULL,
  body_text       TEXT NOT NULL,
  body_html       TEXT NOT NULL,
  status          TEXT NOT NULL DEFAULT 'pending'
                  CHECK (status IN ('pending','sending','sent','failed','sink')),
  attempts        INT NOT NULL DEFAULT 0,
  max_attempts    INT NOT NULL DEFAULT 8,
  next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  expires_at      TIMESTAMPTZ NOT NULL DEFAULT now() + interval '1 hour',
  last_error      TEXT,
  user_id         UUID REFERENCES users(id) ON DELETE SET NULL,
  created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
  sent_at         TIMESTAMPTZ
);

-- The claim query's whole predicate, so a drain tick touches only due rows.
CREATE INDEX mail_outbox_due_idx     ON mail_outbox (next_attempt_at) WHERE status = 'pending';
-- Orphan recovery: rows a process was killed mid-send on. Nothing else ever
-- reclaims them, because the claim query only looks at 'pending'.
CREATE INDEX mail_outbox_stuck_idx   ON mail_outbox (updated_at)      WHERE status = 'sending';
-- The per-recipient suppression probe inside the enqueue INSERT.
CREATE INDEX mail_outbox_dedup_idx   ON mail_outbox (kind, recipient_key, created_at DESC);
-- The retention sweep's ORDER BY.
CREATE INDEX mail_outbox_created_idx ON mail_outbox (created_at);
