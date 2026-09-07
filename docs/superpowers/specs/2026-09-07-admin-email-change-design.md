# Admin-initiated email change with user approval

**Status:** approved design, not yet implemented
**Date:** 2026-09-07

## Problem

An organization admin has no way to correct or change a member's email
address. The address is the login identity — `users_email_lower_key` is a
UNIQUE index on `lower(email)` — so today the only workaround is to create a
second account, which strands the original row on that index and leaves the
member unable to be recreated under their own address.

The change must not be unilateral. An admin holding `member:credential` who
could silently move an account to an address they control would hold an
account-takeover primitive, so the change stays **pending** until the holder
of the new address confirms it, and the holder of the old address can veto it.

## Decisions

These were settled during brainstorming and are not open in implementation:

1. **The old address can veto.** Its mail carries a one-click cancel token.
   Without it, `member:credential` is an account-takeover primitive.
2. **The old-address mail never names the new address.** Enforced on the
   server, not in the template — see `preview` below.
3. **Approval swaps the address only. Sessions survive.** Access and refresh
   tokens carry `user_id`, not the email, so nothing breaks and nobody is
   signed out.
4. **Approval also invalidates outstanding password-reset tokens.** Closing a
   gap decision 3 opens: `password_reset_tokens.password_fingerprint` kills a
   link when the *password* moves, not when the *address* does, so without
   this a reset link mailed to the old address stays redeemable afterwards.
5. **Admin-initiated only, org-scoped.** No self-service email change. One
   endpoint pair under `/v1/orgs/{org_id}/members/{user_id}/email-change`.
6. **At most one live request per user.** A second request supersedes the
   first. Enforced by a partial unique index, not by application ordering,
   because application ordering loses a race between two concurrent admins and
   leaves two live approve links — which is a typo'd domain holding a standing
   claim on the account.
7. **Admin surface:** a pending badge in the members list, an admin-side
   cancel, and audit entries for the request and every terminal outcome.

## Data model

Migration `2026-09-07-000076_email_change_requests`. One new table; `users` is
not touched.

```sql
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

CREATE UNIQUE INDEX email_change_one_live_per_user
  ON email_change_requests (user_id)
  WHERE approved_at IS NULL AND cancelled_at IS NULL;
```

**Two token columns, one row.** A pending change is one fact with two secrets.
Splitting them across rows would make "cancel kills the matching approve" a
join every caller has to remember; here it is the same row by construction.
Both are unsalted SHA-256 via `sauron_auth::hash_token`, both UNIQUE, matching
`password_reset_tokens.token_hash`. The raw tokens are
`sauron_core::ids::opaque_token()` — `random_hex(32)`, so 64 hex characters.

**`email_fingerprint` is `lower(users.email)` at issue time**, re-checked at
approval. Same role `password_fingerprint` plays for resets and for the reason
migration 36 states: it kills a stale link implicitly, rather than imposing a
sweep on every future email-writing path. Plaintext rather than hashed —
unlike a password hash it is not a credential, and the approve path already
holds the value it compares against.

**`org_id` is stored** because `confirm` and `cancel` are unauthenticated: the
token is all they have, and the audit entry must land in the right org.

**No CHECK may reference `initiated_by`.** Migration 36 documents why: the FK
is `ON DELETE SET NULL`, that action performs an UPDATE, and the UPDATE
re-validates every CHECK on the row — so deleting an admin account would error
out on an unrelated user's request row.

**No index on `expires_at`.** Both read paths lead with a token hash (a UNIQUE
btree) and apply `expires_at > now()` as a filter on the single matching row;
the reaper deletes on `created_at`. Matches migration 36.

`down.sql` drops the table; the indexes and constraints go with it.

### Reaping

Add `repo::prune_email_change_requests`, deleting on `created_at` older than
`EMAIL_CHANGE_RETENTION_DAYS = 30` — the value and the name mirroring
`PASSWORD_RESET_RETENTION_DAYS` at `main.rs:68`. It runs inside the existing
`password_reset_reaper` supervised task at `main.rs:412` rather than as a
second loop: both tables are reaped hourly, both are owned by this process
because their write paths are here, and one task means one connection checkout
per tick against a pool of 16. Rename the task to `credential_token_reaper` and
widen its log line. Deleting these rows disables nothing — nothing reads a dead
request row.

## Backend surface

### Constants (`routes/auth.rs`, beside the reset constants)

| Constant | Value | Why |
|---|---|---|
| `EMAIL_CHANGE_TTL_SECS` | `86_400` | Matches `ADMIN_RESET_TTL_SECS`. The member may not read mail the same day. |
| `EMAIL_CHANGE_PER_CALLER_PER_HOUR` | `20` | Matches `ADMIN_RESET_PER_CALLER_PER_HOUR`. Bounds fan-out. |
| `EMAIL_CHANGE_PER_TARGET_PER_HOUR` | `5` | Matches `ADMIN_RESET_PER_TARGET_PER_HOUR`. Bounds a mail bomb at one member, and is the real bound on the zero dedup window below. |
| `EMAIL_CHANGE_ATTEMPTS_PER_MIN_PER_IP` | `60` | Matches `RESET_ATTEMPTS_PER_MIN_PER_IP`. |
| `EMAIL_CHANGE_ATTEMPTS_PER_TOKEN_PER_HOUR` | `10` | Matches `RESET_ATTEMPTS_PER_TOKEN_PER_HOUR`. Bounds the branches that return without burning the row. |

### Authenticated endpoints

`POST /v1/orgs/{org_id}/members/{user_id}/email-change`, body `{ new_email }`.
`DELETE` on the same path is the admin cancel. CORS already permits both
methods (`main.rs:459`), so no method gap.

Both gated on `perm::MEMBER_CREDENTIAL` *in addition to* the `member:manage`
that `guard_member_admin_action` checks first — the same reasoning
`reset_member_password` records: `member:manage` is the routine
grant-administration permission, and moving someone's login identity is not
something an org that handed out the routine permission has agreed to.

Request path, in order, because the order is the guarantee:

1. `authorize_org(..., MEMBER_CREDENTIAL)`.
2. Resolve `state.mail` and `require_dashboard_url()`. **Before any write** —
   a change must never land when the mail carrying its remedy cannot be sent.
   Absent either: 503, nothing applied.
3. Rate-limit per caller, then per target.
4. `guard_member_admin_action(..., allow_self: false)` — carries user-exists
   404, membership 404, self-target 409, no-escalation, and the unwaivable
   cross-org refusal. No local copy of any of those checks.
5. Refuse an inactive target: 409, same spirit as `create_grant`.
6. Normalize `new_email` to lowercase and validate. Refuse if it equals the
   member's current address (409) or already belongs to any user (409).
7. Supersede any live request for this user
   (`cancelled_reason = 'superseded'`).
8. Insert. A unique violation on `email_change_one_live_per_user` means a
   concurrent admin won the race: map it to 409, do not retry silently.
9. Audit `member.email_change_request` while the connection is held, before
   the enqueue — the row exists whether or not the mail ever goes out, and
   auditing after the enqueue loses exactly the case worth investigating.
10. Drop the connection, then enqueue **the notice to the old address first,
    then the approval to the new one**. That direction makes a partial failure
    "the veto signal was sent, the approve link was not", whose worst outcome
    is a request that expires unused.

The response never carries either raw token or either URL, under any
condition. `member:credential` lets its holder disrupt a member's account, not
sign in as them.

Admin cancel spends the per-caller bucket only — it sends no mail and can only
undo — and sets `cancelled_reason = 'admin'`.

### Unauthenticated endpoints

`POST /v1/auth/email-change/preview`, `/confirm`, `/cancel`. All three take
the token **in the JSON body, never a query string**, preserving the property
`reset_link` documents: the token reaches no server log, proxy log, or
`Referer`. All three reject a token that is not 64 hex characters before
touching Redis or the database, so a spray mints no limiter keys.

**`preview` is the non-disclosure chokepoint.** It looks the token up against
both columns and returns:

```jsonc
{
  "role": "approve" | "cancel",  // which column the token matched
  "org_name": "Acme",
  "expires_at": "2026-09-08T10:00:00Z",
  "new_email": "bob@example.com"  // PRESENT ONLY WHEN role == "approve"
}
```

`new_email` is omitted when the cancel token matched. The old-address page
therefore cannot display the new address even by accident, because the server
never sends it there. Decision 2 is enforced here, in one place, rather than
by remembering to keep it out of a template — and a test asserts the cancel
shape omits the key rather than nulling it.

A token that matches nothing, or matches a row that is expired, approved, or
cancelled, returns 404 with no distinction between those cases: the pages
render one "this link is no longer valid" state, and the reasons are for the
audit log, not for whoever is holding the link.

`confirm`:

1. Rate-limit per IP, then per token hash.
2. Find the live row by `approve_token_hash` (not approved, not cancelled, not
   expired).
3. Load the user; refuse if inactive.
4. **Fingerprint check:** `email_fingerprint` must equal `lower(user.email)`.
   A mismatch means the address already moved and this link is stale.
5. **Re-check the collision.** The address may have been claimed since the
   request was issued. On a clash return 409 **without burning the row** — the
   change may become possible again, and the per-token limiter bounds retries.
6. Burn: one atomic
   `UPDATE … SET approved_at = now() WHERE approve_token_hash = $1 AND approved_at IS NULL AND cancelled_at IS NULL AND expires_at > now() RETURNING user_id, new_email`.
   Single-use without `conn.transaction`, matching
   `consume_password_reset_token` — async closures need Rust 1.85 and the
   workspace MSRV is 1.82 per `packaging/rpm/sauron.spec`.
7. Write `users.email`. A unique violation here is the residual race: 409.
8. `invalidate_password_reset_tokens_for_user(..., RESET_INVALIDATED_SUPERSEDED)`
   — decision 4.
9. Audit `member.email_change_approved` into the row's `org_id`.

Sessions are deliberately **not** revoked and `state.revocations` is not
touched — decision 3.

`cancel` is symmetric: burn by `cancel_token_hash`, set
`cancelled_reason = 'user'`, audit `member.email_change_cancelled`. Its
response body must not name the new address.

`audit::record` takes a non-optional actor `Uuid`. Both unauthenticated paths
pass **the target user's own id**: they proved mailbox control, and recording
the admin as the actor of an act the admin did not perform would be false.

### Mail

Two new `MailKind` variants in `sauron-mail/src/kind.rs`, wire strings
`email_change_notice` and `email_change_approval`. `kind.rs` owns both the
variant and its dedup window precisely so the two cannot drift, and its tests
assert the reviewed values — extend them.

**Both dedup windows are `Duration::ZERO`, and that is load-bearing.**
Supersede makes repeat notices to the *same* old address a normal event, and
`PasswordReset`'s 300-second window would silently swallow the second
attempt's warning — the exact case the veto exists for. Suppression is
indistinguishable from success at the call site (`Ok(None)`), so this would
fail silently. The bound is `EMAIL_CHANGE_PER_TARGET_PER_HOUR` upstream, which
is an authenticated, `member:credential`-gated path — not the dedup window.

Copy, rendered through `sauron_mail::MailContent` (structural prose; the
renderer owns every escape site and the HTML shell):

- **Old address.** Subject: a change to your email address was requested. Body:
  an administrator of `{org}` requested a change to the address on this
  account. **Names no new address.** States that the change takes effect only
  when the new address confirms, and that it lapses in 24 hours otherwise.
  CTA: cancel it, to `#/cancel-email-change?token=`.
- **New address.** Subject: confirm your new email address. Body: an
  administrator of `{org}` set this address as the new email for
  `{display_name}`. CTA: confirm, to `#/confirm-email-change?token=`.

Neither names the acting admin, matching the rule `ResetMailVars` already
documents: the org is what a recipient needs to judge legitimacy, and naming
an individual invites a reply to a person rather than a route back into the
account. Both carry the `PASTE_FALLBACK` line and derive their expiry wording
from `expiry_wording(EMAIL_CHANGE_TTL_SECS)` rather than typing the number.

Both links are fragment-based, so the token never leaves the browser.

### Audit

Three new constants in `bins/sauron-api/src/audit.rs`, beside
`MEMBER_RESET_PASSWORD`, all against `entity::MEMBER`:
`member.email_change_request`, `member.email_change_approved`,
`member.email_change_cancelled`. The cancelled entry records
`cancelled_reason`, because "the member rejected an admin's attempt" and "the
admin withdrew it" are different events and only the first is a signal worth
investigating. The request entry records the new address: the acting admin
typed it, and the audit log is an admin surface, so this discloses nothing new.

### `MemberGrant`

Gains `pending_email_change: Option<PendingEmailChange>` — `{ new_email,
expires_at }` — fed by a LEFT JOIN in `repo::list_org_grants` against the live
partial index. `GET /v1/orgs/{org}/members` is the only place the dashboard
learns anything about a member's account state; without this the admin cancel
exists on the server and is unreachable from the UI, which is the same as not
existing.

## Frontend

- `lib/components/members/ChangeEmailDialog.svelte`, a sibling of
  `ResetPasswordDialog.svelte`. **Not** a new section in
  `EditMemberDialog.svelte`, which is already 783 lines.
- Pending badge in `MembersTable.svelte`, with the requested address and the
  expiry, plus the cancel action.
- `pages/ConfirmEmailChange.svelte` and `pages/CancelEmailChange.svelte`.
- `lib/api/orgs.ts` gains request/cancel; a bare client (not `api`) serves the
  three public endpoints, as `auth.ts` already does for `reset-password`.

**Neither public page may fire its POST on mount.** Link scanners such as
Outlook Safe Links fetch mailed URLs; an auto-firing cancel page would let a
scanner silently kill every pending request, and an auto-firing confirm page
would approve changes nobody clicked. Each page calls `preview` on mount to
render what is about to happen, then waits for an explicit button press.

Registration, following `/reset-password` exactly:

- `routes.ts`: condition-free entries. A condition would fire
  `conditionsFailed` and push the visitor to `/login`, making the link unusable.
- `page-access.test.ts`: add both to `UNAUTHENTICATED`.
- **Not** in `App.svelte`'s `PUBLIC_ROUTES` — that array drives an `$effect`
  that pushes authenticated users off those paths, and a signed-in member
  clicking their own link would be bounced before they could use it.
- **Not** in `SHELL_FLAGS` or `PAGE_ACCESS`; their parity test pairs those two
  with each other and unauthenticated pages appear in neither.

Every new string needs an Arabic translation. The untranslated-string test has
twice passed on pages with real leaks, so translations get eyes, not just a
green suite.

## Testing

`backend/crates/sauron-db/tests/email_change.rs` and
`backend/bins/sauron-api/tests/http_email_change.rs`, the latter modelled on
`http_password_reset.rs`.

**A green backend run proves nothing on its own.** A suite with no reachable
database prints `ok` having run nothing; the duration is the only tell. Record
the wall-clock time of these suites in the implementation notes and check it,
and confirm `TEST_REDIS_URL` is set — the rate-limiter assertions silently
skip without it.

Cases that must exist:

1. A second request supersedes the first; the first approve token is dead.
2. Two concurrent requests: one wins, the other gets 409 from the partial
   unique index. Not a serial stand-in for concurrency.
3. Cancelling from the old-address token kills the approve token.
4. Confirm after the account's address moved by another path: fingerprint
   mismatch, refused.
5. Confirm when the address was claimed meanwhile: 409, **row not burned**.
6. The old-address mail body does not contain the new address — asserted
   against the rendered body, not the template source.
7. `preview` with a cancel token omits `new_email`; with an approve token
   includes it.
8. SMTP unconfigured: 503 and **no row written**.
9. Inactive target 409; self-target 409; cross-org member 409; escalation 403.
10. Confirm invalidates outstanding password-reset tokens.
11. **Confirm leaves sessions live** — decision 3 is a deliberate choice and
    needs a test that fails if someone later "fixes" it.
12. An expired request confirms nothing and reports itself as expired.
13. Both dedup windows are `ZERO`, asserted in `kind.rs`'s existing test.

Frontend: unit tests for the two public pages' state machines, and the two
parity tests must stay green.

## Out of scope

Self-service email change; changing an address for a member who holds grants
in another org (the existing cross-org refusal already blocks it); any change
to session or refresh-token behaviour.
