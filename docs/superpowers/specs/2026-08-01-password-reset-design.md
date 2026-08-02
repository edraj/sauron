# Password reset — self-service and admin-initiated

Date: 2026-08-01
Status: designed
Slice: S1 of the email/auth programme (S0 → S2 → **S1** → S3)

## Problem

A Sauron account has exactly one way to change its password today:
`POST /v1/auth/password` (`backend/bins/sauron-api/src/routes/auth.rs:468`),
which requires the **current** password. There is no path back from a forgotten
one.

The workaround the member-lifecycle slice left behind is
`POST /v1/orgs/{org_id}/members` — create a second account with a reveal-once
temp password — which is not a reset at all: it strands the original row on the
`users_email_lower_key` index, so the person cannot even be recreated under
their own address. The only real recovery is `psql`. That document names
"self-service password reset by email" as its first follow-up
(`docs/superpowers/specs/2026-07-26-member-lifecycle-design.md:357`).

An admin has no way to force the issue either. `PATCH /v1/orgs/{org}/members/{user}`
can only deactivate — the biggest hammer available, and the wrong one when the
intent is "I think this credential leaked, make them pick a new one." What that
intent actually requires is that the suspect password stops working at the login
form, and nothing short of deactivation does that today.

## Position in the programme — what S1 consumes and does not build

S1 lands **third**, after S0 (email foundation) and S2 (session management). Both
predecessors move work out of this slice, and the original S1 design was written
before either existed. Where this document disagrees with that design, this
document wins.

| From | S1 uses | S1 therefore does **not** build |
|---|---|---|
| S0 | `AppState.mail: Option<MailSender>`, `MailSender::enqueue(kind, recipient, content, user_id, ttl: Duration) -> anyhow::Result<Option<Uuid>>` and its `enqueue_or_discard` sibling, which takes `recipient: Option<&str>` — both render at call time and INSERT into `mail_outbox`; the SMTP dial happens in S0's drain, not on the request path | Any `tokio::spawn`, any semaphore, any inline SMTP send |
| S0 | `MailKind::PasswordReset` (a Rust enum; S0 dropped the CHECK on `mail_outbox.kind`) | A migration to widen a `kind` CHECK |
| S0 | `Config::require_dashboard_url() -> anyhow::Result<&str>`, normalised with no trailing slash | Any URL config of its own |
| S0 | `sauron_mail::text::{substitute, html_escape}`, both `pub`, and the house multipart layout | A second copy of the escaping helpers |
| S0 | `Config::dev_mode` as a public field, and `SMTP_SINK` for local observability | A "log the reset URL" branch — `SMTP_SINK` already logs the fully rendered message, and reusing `SAURON_DEV` (the flag that also waives the JWT secret check, `config.rs:143`) to print live takeover credentials at INFO would be a bad trade |
| S0 | `SMTP_ALLOW_PRIVATE`, separate from `ALERTS_ALLOW_PRIVATE` | Nothing — but see "Risks carried": without the split, making reset mail reach a LAN relay would disable the SSRF guard on tenant-configured alert channels in the same process |
| S0 | `backend/bins/sauron-api/src/tasks.rs`, the supervised background-task runner | A reaper in `sauron-alerts` |
| S0 | `packaging/rpm/SETUP.md` §11 "Upgrading" | The section itself; S1 appends one row |
| S2 | `repo::revoke_sessions_for_user(conn, user_id, except: Option<Uuid>, reason: &str, actor: Option<Uuid>) -> QueryResult<Vec<Uuid>>` — revokes `auth_sessions` **and** `refresh_tokens` together, and hands back the session ids | Any call to `revoke_all_refresh_tokens_for_user_with_reason`, which would leave `auth_sessions.revoked_at` NULL and desync the two tables |
| S2 | `SessionRevocations::mark_revoked(&ids)` on `AppState.revocations` — the local half of the snapshot, which is what makes a revoke effective on *this* replica before its next poll | Any cache of its own, and any hedge about kill latency |
| S2 | `guard_member_admin_action(conn, caller_id, org_id, target_user_id, allow_self) -> Result<Vec<(String, Uuid, Value)>, ApiError>`, extracted from `set_member_active` (`orgs.rs:702`) | A third verbatim copy of six guards and 35 lines of why-comment, and any cross-org check of its own — the helper refuses a target with grants outside the org for every caller |
| S2 | `perm::MEMBER_CREDENTIAL` (`member:credential`), in the Owner and Admin presets | The `rbac.rs` constant, the `perm::ALL` length, the four preset count assertions, the migration UPDATEing custom roles, `models/permissions.ts` and the `Permission` union — S2 gates force-logout on the same permission and lands first, so all five mirrors ship there |
| S2 | `pub(crate) rate_limit` and `pub(crate) client_addr` | Its own limiter, and the "keep everything in auth.rs so nothing goes `pub(crate)`" constraint the original design was built around |
| S2 | `AUTH_REVOCATION_POLL_SECS` (default 5) — the honest kill latency | The "within 15 minutes" hedge the pre-S2 design had to write |

The one piece of shared UI S1 **does** build is the members row-action overflow
menu (§10). S2 takes that row from two inline buttons to three; S1 is the slice
that makes it four, and `dashboard/src/lib/components/ui/` ships fourteen
components with no menu primitive among them.

Two revocation reasons are settled in S2's migration rather than here, because
S2 adds a CHECK constraint on `auth_sessions.revoked_reason` and S1 writes two
new values into it:

- `repo::REVOKE_PASSWORD_RESET = "password_reset"` and
  `repo::REVOKE_RESET_FORCED = "reset_forced"` are in S2's CHECK list from day
  one — seeded while `auth_sessions` is still empty, which is free — so **S1
  ships exactly one migration** and never widens that constraint. If they were
  missing from the CHECK, every successful self-service reset would 500 at the
  revoke step, not just the admin route. S1's own edit puts both into
  `DELIBERATE_REVOKE_REASONS`; without that the target's still-live refresh
  token lands in `refresh`'s reuse branch and fires a family kill — the exact
  poisoning bug the comment at `auth.rs:388-397` records as having already
  happened once with routine deactivations.

## Decisions

| Question | Decision | Rejected |
|---|---|---|
| How does a link issued before an unrelated password change get invalidated? | A `password_fingerprint` column holding `hash_token(&user.password_hash)` at issue time, **re-checked at the write** via a compare-and-swap UPDATE | A sweep from every password-writing code path. That is a discipline requirement on code not yet written; forgetting it leaves a live link for an already-rotated account. The sweep is still done, as bookkeeping, but it is not the guarantee |
| Does a successful reset log the user in? | No. `{"ok": true}`, then the dashboard signs the browser out locally and sends them to `#/login` | Returning an `AuthResponse` like `change_password`. The reset caller proved control of a mailbox, not of a credential; auto-login makes a forwarded or archived message session-equivalent, and it contradicts the step immediately before it, which revoked every session the account had |
| What does an admin-initiated reset do to the current password? | It stops working. The route stamps `users.credentials_invalidated_at`, sets `must_change_password`, revokes every session, and `login` then refuses with `password_reset_required` | Leaving the old password live and merely gating the session it produces. That is a narrower reading than the requirement's own words, and it leaves a credential the admin believes has leaked able to authenticate for as long as nobody uses the link |
| Is there also a non-destructive "send this member a link" action? | No. One action, always destructive | A second mode that only mails a link. Nothing asks for it, and shipping both puts an admin holding a suspected leak in front of two adjacent buttons, one of which stops the leaked password and one of which looks like it does |
| What happens when the mail never arrives? | The admin re-issues, or cancels — `{"action": "cancel"}` on the same route clears the invalidation and kills the outstanding links | Leaving the account locked until someone reaches `psql`. This action is destructive *and* gated on a mail relay the deployment may have misconfigured; without an undo that does not itself depend on the relay, one bounced message is an account nobody can reach |
| Can an admin reset themselves? | No, 409 — `guard_member_admin_action(..., allow_self: false)` | Allowing it. It is redundant (`/v1/auth/password` exists) and it lets an admin lock themselves out over a relay they may have just broken, leaving nobody with standing to cancel it |
| Can an admin reset a member who also holds grants in another org? | No, 409 — the blanket refusal `set_member_active` already makes, applied by the shared guard | Exempting the case. The refusal exists because deactivation locks someone out of an org you have no authority over, and invalidating their credential does exactly that, more quietly. See "Risks carried" for what the blanket rule costs a multi-org member whose per-email bucket is exhausted |
| New permission? | `perm::MEMBER_CREDENTIAL` at org scope, minted by S2, **in addition to** `member:manage` rather than instead of it — the shared guard stack still requires `member:manage`, so this route needs both | `member:manage` alone. It already means "can deactivate this account", so reuse is arguable — but it is also the routine permission for handing out and revoking grants, and forcing a reset combined with control of the mail relay is a path to account takeover. Splitting lets an org grant day-to-day membership administration without also granting that. S2 needs the same gate for force-logout and lands first, so the constant and its five mirrors are its work, not S1's |
| How do the five dead-token states report themselves? | One indistinguishable `401 invalid_token` for unknown, consumed, invalidated, expired, and stale-fingerprint | Distinct codes. They tell an attacker spraying the token space whether a guess ever corresponded to a real link, and leak one user's activity. The dashboard renders its own copy for `invalid_token`, so UX loses nothing |
| Does a second request invalidate the first link? | Self-service: **no**. Admin-initiated: **yes** | Invalidate-on-issue everywhere. An attacker spamming forgot-password against a known address would kill the link the victim is about to click, turning the anti-abuse limiter into the abuse |
| Where does the token sit in the URL? | `{DASHBOARD_URL}/#/reset-password?token={raw}` — inside the hash fragment | A pre-hash query string. Browsers never send a fragment in the request line or a `Referer`, so the token reaches no server log, proxy log, or analytics beacon. That is a real property here, not just house convention |
| What happens when SMTP is unconfigured? | An admin **reset** refuses with `503 unavailable` before applying anything; **cancel** still works, since undoing a lockout needs no mail. `forgot-password` still answers its generic `200`. Neither route ever returns the link, under any condition | A `503` on `forgot-password` too. That route is unauthenticated, and a status that flips with deployment configuration is a free config-state oracle for an anonymous caller on the one endpoint whose entire contract is that every input gets the same answer. Also rejected: applying the state change and reporting `email_queued: false` with the link so the admin can "copy it and send it yourself". That link is an account-takeover primitive, and it is a strictly larger power than the one the route grants: `member:credential` lets its holder deny a member their account, not sign in as them |

## Non-goals

- Email verification at registration. `users` has no `email_verified` column and
  the concept exists nowhere. Reset-by-email currently trusts an address nobody
  ever proved control of — worth naming, its own slice.
- Real invitation tokens replacing the reveal-once temp password in
  `POST /v1/orgs/{org_id}/members`. The natural sequel now that a one-time-token
  table exists; `create_member` is untouched here.
- A general `audit_events` table. `password_reset_tokens` rows are the audit
  trail for this feature and nothing else.
- Password strength policy beyond the shipped `>= 8` / `<= 256`.
- A `password_history` table. This slice refuses reuse of the **current**
  password only, matching `change_password`.
- Account lockout after N failed logins, CAPTCHA, 2FA.
- Localized or operator-editable email templates.
- A voluntary "Change password" entry point for a non-forced user.
  `ChangePassword.svelte` remains reachable only when `must_change_password` is
  true.

---

## 1. Migration `2026-08-01-000036_password_reset`

**Numbering.** Migration numbers are allocated in **landing** order, not design
order, because `run_pending_migrations` (`backend/crates/sauron-db/src/lib.rs:30`)
orders by the full directory version string — date first. A slice that lands
late with an early date prefix runs out of order and nobody notices until a
foreign key fails. Under the programme sequence (S0 = 000034, S2 = 000035) S1
takes **000036**; if S1 lands ahead of S2 for any reason it takes 000035 instead,
and the date prefix is always the landing date. Last on disk today is
`2026-07-30-000033_env_per_project`.

```sql
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

CREATE INDEX password_reset_tokens_user_idx    ON password_reset_tokens (user_id);
CREATE INDEX password_reset_tokens_created_idx ON password_reset_tokens (created_at);

ALTER TABLE users ADD COLUMN credentials_invalidated_at TIMESTAMPTZ;
```

`down.sql` is `DROP TABLE IF EXISTS password_reset_tokens;` — the indexes and
the UNIQUE go with it — plus
`ALTER TABLE users DROP COLUMN IF EXISTS credentials_invalidated_at;`.

The shape is a deliberate copy of `refresh_tokens`: a 256-bit
`sauron_core::ids::opaque_token()` that exists only in the email, an unsalted
SHA-256 `sauron_auth::hash_token()` in a UNIQUE column, an explicit `expires_at`,
a single-use marker. Three columns `refresh_tokens` does not have:

- **`password_fingerprint`** is `hash_token(&user.password_hash)` at issue time —
  a hash of a hash, not a credential. It is what makes a link expire implicitly
  when the password moves for any other reason, checked at the point of use so
  it cannot be forgotten by future code.
- **`mode`** is TEXT + CHECK per the house rule. It records *why* the token
  exists, which both the email copy and the audit trail need.
- **`initiated_by`** is NULL for self-service and the acting admin otherwise;
  `ON DELETE SET NULL` matches `notification_channels.created_by`.

`requested_from` stores `client_addr(...)`. This table is the only audit trail
the deployment has — there is no `audit_*` table anywhere — and it is
**proxy-blind** whenever `API_TRUST_FORWARDED_HEADERS` is false, which is the
default in `config.rs`, in `packaging/rpm/config/api.env` and in
`docker-compose.yml`. Say so in the column comment, so nobody later reads a
column full of `10.0.0.5` as a finding.

`consumed_at` and `invalidated_at`/`invalidated_reason` are split on purpose:
consumed means the user used it, invalidated means something else killed it. The
free-text reason mirrors the `refresh_tokens.revoked_reason` precedent from
migration 22.

**Do not add `CHECK ((mode = 'self') = (initiated_by IS NULL))`.** It is the
obvious integrity constraint and it is a trap: `initiated_by` is
`ON DELETE SET NULL`, that FK action performs an UPDATE, and the CHECK would
re-validate and fail — so deleting an admin account would error out on an
unrelated user's reset row. The invariant lives in the migration prose instead.

There is no index on `expires_at`. Nothing filters on it first:
`find_live_password_reset_token` and `consume_password_reset_token` both lead
with `token_hash`, which is a UNIQUE btree, and `expires_at > now()` is applied
as a filter on the single matching row. The reaper deletes on `created_at`, which
is why that is the column that gets an index. An `expires_at` index would be pure
write amplification on every insert and both UPDATE paths.

### `users.credentials_invalidated_at`

This is the column that makes an admin-initiated reset mean what it says. It is
NULL for every account that has one; a timestamp means "an admin invalidated
this credential and the replacement has not been chosen yet". `login` reads it
**after** the Argon2 verification succeeds, never before — the same rule the
shipped `is_active` check follows for the same reason (`auth.rs:303-310`), since
a check ahead of the verification answers in microseconds for one class of
account and tens of milliseconds for another, which is the enumeration oracle
`spend_dummy_verify` exists to close.

A timestamp rather than a boolean because it is also the only record of *when*,
and the members page renders it. Nothing indexes it: it is only ever read on a
row already fetched by primary key or by `lower(email)`.

**This is the migration's real upgrade hazard, and it is much larger than the
new table's.** `User` is `Selectable`, so every query naming it emits an explicit
column list — including this one — and an upgraded binary against an unmigrated
database therefore fails `login`, `refresh` and `/v1/me` with a missing-column
error. Not "the three password-reset routes 500": *authentication is down for the
whole deployment*. The RPM never re-runs `sauron-migrate`, so this is a
plausible upgrade, and the SETUP.md row in §"Files" must say so in those words.

### `schema.rs` and `models.rs`

Four hand edits to `backend/crates/sauron-db/src/schema.rs`, no diesel CLI —
the CLI rewrites every `table!` block in the file, including the partitioned and
hand-tuned ones, and the result still compiles, so `cargo check` will not tell
you it happened. The diff is the only detector:

1. A `diesel::table! { password_reset_tokens (id) { … } }` block beside
   `refresh_tokens` (schema.rs:212). `invalidated_reason -> Nullable<Text>`.
2. `diesel::joinable!(password_reset_tokens -> users (user_id));` beside the
   existing `refresh_tokens` line (schema.rs:486) — **and only that one**. The
   table has two FKs to `users` and `joinable!` accepts one per table pair, so a
   future query for the initiating admin's email needs an explicit `.on(...)`.
   Put that in a comment; the silent alternative is a confusing diesel type
   error months later.
3. `password_reset_tokens,` in `allow_tables_to_appear_in_same_query!`
   (schema.rs:503-533).
4. `credentials_invalidated_at -> Nullable<Timestamptz>,` at the end of the
   `users` block. Order matters: diesel matches `Queryable` positionally, so a
   field inserted anywhere but the end of both the `table!` block and the struct
   silently binds `name` to `email` and still compiles.

S1's delta is **+1** `table!` block and +1 entry in the allow list. State the
delta, never an absolute count: several slices in this programme add blocks to
this same file, so any total pinned in a document today is wrong for everyone but
whichever slice happens to land first.

`models.rs` gains `PasswordResetToken` (`Queryable, Selectable`) and
`NewPasswordResetToken` (`Insertable`) in the refresh-token neighbourhood
(models.rs:495-522 is the template). **Neither derives `Serialize`**, exactly
like `RefreshToken`: `token_hash` and `password_fingerprint` must never leave
the process, and no endpoint returns this row.

`User` gains `credentials_invalidated_at: Option<DateTime<Utc>>` as its last
field, marked `#[serde(skip_serializing)]` beside `password_hash`. `User` is
returned by `/v1/me` and inside `AuthResponse`, and a caller holding either of
those has by definition just authenticated, so the field could only ever be null
there — a permanently-null key in the public user object is noise that someone
will eventually build a client behaviour on.

The shipped `must_change_password BOOLEAN NOT NULL DEFAULT false` from migration
23 is reused as-is and still carries its own job; §6 says which.

## 2. Repo functions

All in `backend/crates/sauron-db/src/repo.rs` under a new
`// Password reset tokens` section after the refresh-token block (~repo.rs:330).

| Function | Shape |
|---|---|
| `insert_password_reset_token(conn, user_id, token_hash, password_fingerprint, expires_at, mode: &str, initiated_by: Option<Uuid>, requested_from: Option<String>) -> QueryResult<PasswordResetToken>` | `insert_into(...).values(NewPasswordResetToken{..}).returning(PasswordResetToken::as_returning()).get_result()` |
| `find_live_password_reset_token(conn, token_hash: &str) -> QueryResult<Option<PasswordResetToken>>` | `token_hash.eq()`, `consumed_at.is_null()`, `invalidated_at.is_null()`, `expires_at.gt(Utc::now())`, `.first().optional()`. Sibling of `find_active_refresh_token` (repo.rs:189). The cheap pre-check, before any Argon2 |
| `consume_password_reset_token(conn, token_hash: &str) -> QueryResult<Option<(Uuid, String, String)>>` | One `UPDATE … SET consumed_at = now() WHERE token_hash = $1 AND consumed_at IS NULL AND invalidated_at IS NULL AND expires_at > now() RETURNING user_id, password_fingerprint, mode`, `.get_result().optional()`. Zero rows means somebody else burned it |
| `invalidate_password_reset_tokens_for_user(conn, user_id, reason: &str) -> QueryResult<usize>` | `SET invalidated_at = now(), invalidated_reason = $reason WHERE user_id = $1 AND consumed_at IS NULL AND invalidated_at IS NULL`. Sibling of `revoke_all_refresh_tokens_for_user_with_reason` (repo.rs:305) |
| `prune_password_reset_tokens(conn, older_than_days: i64) -> QueryResult<usize>` | `DELETE … WHERE created_at < now() - ($1 \|\| ' days')::interval`, the `prune_alert_events` shape (repo.rs:6090). Deletes by `created_at`, not `expires_at`, so a consumed token's audit trace survives a fixed window regardless of its TTL |
| `set_user_must_change_password(conn, user_id, must_change: bool) -> QueryResult<usize>` | Sets the flag and `updated_at`, nothing else |
| `set_user_credentials_invalidated(conn, user_id, at: Option<DateTime<Utc>>) -> QueryResult<usize>` | Sets the column and `updated_at`. One function for both directions: `Some(now)` locks the credential, `None` is the admin's cancel. Two functions would let one of them be added without the other, and the one that would go missing is the undo |
| `set_user_password_if_hash_matches(conn, user_id, expected_hash: &str, new_hash: &str) -> QueryResult<usize>` | `UPDATE users SET password_hash = $3, must_change_password = false, credentials_invalidated_at = NULL, updated_at = now() WHERE id = $1 AND password_hash = $2` |
| `password_reset_preflight(conn) -> QueryResult<()>` | Two zero-row probes: `password_reset_tokens::table.select(id).limit(0).load()` and the same on `mail_outbox`. Exists for §3 |

Plus four constants. S1 lands after S2, which has already taken the `REVOKE_*`
set from five to eight (repo.rs:205-215); S1's two take it to ten:

```rust
pub const REVOKE_PASSWORD_RESET: &str = "password_reset";   // a reset link was consumed
pub const REVOKE_RESET_FORCED:   &str = "reset_forced";     // an admin forced a reset
pub const RESET_INVALIDATED_SUPERSEDED:   &str = "superseded";
pub const RESET_INVALIDATED_PASSWORD_SET: &str = "password_set";
```

Both revoke reasons join `DELIBERATE_REVOKE_REASONS` (S2's registry) in the same
edit, taking it to `[&str; 5]`. No migration goes with that edit: S2 already
seeded both strings into the `auth_sessions_revoked_reason_check` CHECK. Neither
may ever be passed to the reason-less `revoke_all_refresh_tokens_for_user`, which
hardcodes `REVOKE_REUSE` and poisons the theft alarm.

### Why two password-writing functions, and why `set_user_password` is not one of them

`repo::set_user_password` (repo.rs:156) is an unconditional
`UPDATE users SET password_hash = $2, must_change_password = false`. Its doc
comment says the only way to reach it is the self-service change endpoint. That
stays true — S1 adds **no** caller to it — but it does gain one clause,
`credentials_invalidated_at = NULL`, so that the invariant is "any successful
password write clears the invalidation" rather than "the two writes S1 happened
to think of clear it". A future third writer inherits the rule instead of having
to rediscover it, and the failure it prevents is an account locked out by a
column nothing left will ever reset.

Both new functions exist because of what the shipped one does:

- Routing the **admin reset** through it would clear `must_change_password` on
  the very account it is trying to gate. Hence `set_user_must_change_password`,
  which touches nothing but the flag. Its doc comment must say this out loud,
  because the mistake is invisible in review.
- Routing the **reset consume** through it would make the fingerprint guarantee
  a lie. The check reads `user.password_hash` at step 7 of §4; the write lands at
  step 12, with two Argon2 operations (~100-200 ms at m=19456,t=2) in between. A
  legitimate user changing their password via `/v1/auth/password` inside that
  window would have it silently clobbered by a stale link — precisely the
  scenario the column exists to prevent. `set_user_password_if_hash_matches` is
  the same statement with `AND password_hash = $2`; zero rows updated means the
  password moved under us and the caller returns the same dead-token 401. One
  statement, no transaction, and the guarantee now holds at the commit point
  rather than at a read.

## 3. `POST /v1/auth/forgot-password`

```rust
pub async fn forgot_password(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: axum::http::HeaderMap,
    Json(req): Json<ForgotPasswordReq>,   // { email: String }
) -> Result<Json<serde_json::Value>, ApiError>
```

Lives in `routes/auth.rs`, beside the other five `/v1/auth` handlers. Fully
unauthenticated — no `AuthUser` extractor, so it never reaches the
`must_change_password` gate.

Order, and each step is load-bearing:

1. **Shape only**: `email.len() <= 320 && email.contains('@')`, else
   `ApiError::BadRequest("a valid email is required")`. This is allowed to be a
   distinguishable answer because it does not depend on whether an account
   exists.
2. **`state.mail.is_none()`, or `require_dashboard_url()` failing → one
   `tracing::error!` and the same generic `200`.** This route is
   unauthenticated: a status code that flips with deployment configuration is a
   config-state oracle handed to an anonymous caller, and it is the only route
   here whose contract is that *every* input gets the same answer. The operator
   learns from S0's startup WARN and from the admin route's `503`; the anonymous
   caller learns nothing.
3. Per-email limiter, then per-IP limiter (§5).
4. `repo::password_reset_preflight(&mut conn)?` — see below.
5. `let raw = opaque_token();` — no round trip, and it means the render in step 7
   has a URL to substitute on both branches. On the discard branch that URL
   corresponds to no row anywhere, and the message it lands in is never inserted.
6. `repo::find_user_by_email` (lowercases internally; `users_email_lower_key` is
   on `lower(email)`). This decides what goes into `password_reset_tokens` and
   who the recipient is — never whether step 7 runs.
   - Found and `is_active`: `let h = hash_token(&raw); let fp = hash_token(&user.password_hash);`
     → `insert_password_reset_token(..., Utc::now() + Duration::seconds(SELF_RESET_TTL_SECS), "self", None, Some(addr))`,
     `recipient = Some(user.email)`. Self-service does **not** invalidate the
     user's outstanding tokens.
   - Not found: `tracing::debug!`, `recipient = None`.
   - Found but deactivated:
     `tracing::info!(user_id, "forgot-password ignored for a deactivated account")`,
     `recipient = None`. A deactivated user cannot log in anyway, and mailing them
     "your account is disabled" is an information leak dressed as helpfulness.
7. **Outside that branch, unconditionally**: `drop(conn)`, render (§7), then
   `mail.enqueue_or_discard(MailKind::PasswordReset, recipient, &content, user_id, Duration::seconds(SELF_RESET_TTL_SECS))`.
   `enqueue_or_discard` renders on both branches, normalizes a missing recipient
   to `discard@invalid`, and passes `commit = recipient.is_some()` — so the
   discard branch runs the same statement against the same index and inserts
   nothing — then nudges the drain either way. Paying that identically on every
   branch is the anti-enumeration property; an `if let Some(user)` around it is
   what would rebuild the oracle S0 went to the trouble of closing. An error out
   of it is logged and swallowed; the answer stays `200`. The `drop` is not
   optional either — see "Holding a connection" in §6.
8. `Ok(Json(json!({"ok": true})))`.

### The anti-enumeration contract

Every input that parses gets a byte-identical `200 {"ok": true}` — unknown
address, deactivated account, and happy path alike.

The original design achieved constant time structurally, by spawning the SMTP
send off the request path, because a 10-second relay dial is a timing oracle no
dummy Argon2 can mask. Under S0's outbox that whole apparatus is unnecessary:
the handler never touches a socket, and `enqueue_or_discard` deliberately spends
the render, the round trip and the drain nudge on the discard branch too. What
remains is one local INSERT into `password_reset_tokens` and two SHA-256 hashes
on the found branch — sub-millisecond against network jitter that is orders of
magnitude larger, and in the same regime as the residual `login` already accepts
after its `spend_dummy_verify`. The preflight in step 4 shrinks it further by
putting two round trips on **both** branches. This is stated as a measured trade
rather than a claim of perfection: if instrumentation ever shows a signal, the
fix is a fixed-cost pad, not a dummy INSERT, which would be visible in the table.

### Why the preflight exists

Without it, this endpoint becomes a *perfect* enumeration oracle on any
deployment that upgraded without running `sauron-migrate` — the RPM never
re-runs it. Unknown address: one SELECT against `users`, `200`. Known address:
an INSERT against a table that does not exist, `500`. The preflight moves that
failure ahead of the branch so it is uniform, and it converts the original
design's other packaging hazard — a cheerful "we have sent a link" forever, with
only a log line to show for it — into a loud 500 that pages someone.

Past the preflight, an error from the lookup or either INSERT is logged at ERROR
and **still answered `200 {"ok": true}`**. At that point a failure is not
correlated with account existence (both branches have already touched both
tables), and a 500 that fires only on the account-exists branch would be exactly
the oracle we just closed.

## 4. `POST /v1/auth/reset-password`

```rust
pub async fn reset_password(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: axum::http::HeaderMap,
    Json(req): Json<ResetPasswordReq>,    // { token: String, new_password: String }
) -> Result<Json<serde_json::Value>, ApiError>
```

Also unauthenticated; the token travels in the body exactly like `logout`'s
refresh token.

1. Per-IP limiter (§5).
2. Length policy copied verbatim from `change_password` (auth.rs:490-499):
   `>= 8` → `"password must be at least 8 characters"`, `<= MAX_PASSWORD_LEN`
   (256) → `"password must be at most {MAX_PASSWORD_LEN} characters"`. Same
   strings, so the *length* half of password policy keeps one definition.
3. Cheap token shape: `token.len() == 64 && token.chars().all(|c| c.is_ascii_hexdigit())`.
   `opaque_token()` is `random_hex(32)`. Rejects garbage without a DB round trip
   and without minting a Redis key for it.
4. Per-token limiter, keyed on `hash_token(&req.token)` (§5).
5. `let h = hash_token(&req.token);` `find_live_password_reset_token(&h)` →
   `None` ⇒ dead.
6. `repo::get_user` → `None` ⇒ dead. `!user.is_active` ⇒
   `AuthError::AccountDeactivated`. The holder controls the mailbox, so telling
   them is honest rather than a leak.
7. **Fingerprint**: `hash_token(&user.password_hash) != row.password_fingerprint`
   ⇒ dead. This is the "the password changed since this link was issued" case.
8. `verify_password_async(new_password, user.password_hash)` → true ⇒
   `400 "the new password must be different from the current one"`.
9. `hash_password_async(new_password)`.
10. `consume_password_reset_token(&h)` → `None` ⇒ dead (someone raced us).
11. `let ids = repo::revoke_sessions_for_user(&mut conn, user_id, /* except */ None, repo::REVOKE_PASSWORD_RESET, /* actor */ None).await?;`
    then `state.revocations.mark_revoked(&ids);`. `except` is `None` because a
    reset kills every session including any the caller happens to hold, and
    `actor` is `None` because this path is unauthenticated — nobody has proved
    who they are, only that they hold the mailbox, and `auth_sessions.revoked_by`
    must not name the victim as the person who did it. The `mark_revoked` is what
    makes the kill effective on this replica now rather than at its next
    `AUTH_REVOCATION_POLL_SECS` tick.
12. `set_user_password_if_hash_matches(user_id, &user.password_hash, &hash)` →
    0 rows ⇒ dead. That one statement also clears `must_change_password` and
    `credentials_invalidated_at`, so an account an admin locked is unlocked by
    the write that satisfies the demand. A separate follow-up UPDATE would leave
    a window in which the new password is live and `login` still refuses it.
13. `invalidate_password_reset_tokens_for_user(user_id, RESET_INVALIDATED_PASSWORD_SET)`.
14. `Ok(Json(json!({"ok": true})))` — no tokens, no user object.

**Step 8 is a deliberate divergence, not a copy.** `change_password` compares
plaintexts (`if req.new_password == req.current_password`), which is not
available here — there is no current password on this request. Verifying against
the stored hash is the only rule this endpoint can enforce, it is strictly
stronger, and it costs a second Argon2 op. The user-visible string is the shipped
one verbatim so the two endpoints stay consistent; only the mechanism differs.

**Steps 11 → 12 are in `change_password`'s order for its reason** (auth.rs:521-544):
on a partial failure the account must never end up with a new password and live
old sessions. Step 12 failing after step 11 succeeded is the acceptable
direction — it happens only when someone else just set the password, in which
case having revoked sessions is if anything desirable, the link is correctly
spent, and their new password stands.

**The expensive work sits before the burn.** A failed Argon2 must not eat the
user's only link, and a crash between the burn and the password write must leave
the account unchanged. The burn itself is one atomic `UPDATE … RETURNING`, which
is how single-use is enforced without `conn.transaction` — that helper needs
async closures and Rust 1.85; the workspace MSRV is 1.82 per
`packaging/rpm/sauron.spec`.

**Every dead-token state answers identically**:
`ApiError::Auth(AuthError::InvalidToken)` → `401`, code `invalid_token`, message
"invalid or expired token" (`extractors.rs:71`). Unknown, consumed, invalidated,
expired, stale fingerprint, and lost the compare-and-swap all collapse to it.

No preflight is needed here: every input path touches `password_reset_tokens`
unconditionally, so an unmigrated schema 500s uniformly.

## 5. Rate limiting

Constants beside the existing block in `routes/auth.rs:25-37`.

| Constant | Value | Key | Window |
|---|---|---|---|
| `FORGOT_ATTEMPTS_PER_EMAIL_PER_HOUR` | 3 | `sauron:auth:forgot:{email.to_lowercase()}` | 3600s |
| `FORGOT_ATTEMPTS_PER_MIN_PER_IP` | 60 | `sauron:auth:forgot:ip:{client_addr}` | 60s |
| `RESET_ATTEMPTS_PER_MIN_PER_IP` | 60 | `sauron:auth:reset:ip:{client_addr}` | 60s |
| `RESET_ATTEMPTS_PER_TOKEN_PER_HOUR` | 10 | `sauron:auth:reset:tok:{hash_token(&req.token)}` | 3600s |
| `ADMIN_RESET_PER_CALLER_PER_HOUR` | 20 | `sauron:auth:adminreset:{auth.user_id}` | 3600s |
| `ADMIN_RESET_PER_TARGET_PER_HOUR` | 5 | `sauron:auth:adminreset:target:{user_id}` | 3600s |

**The per-IP windows are 60 seconds, not an hour.** This is the single most
important number here and the pre-critique design got it wrong. Because
`API_TRUST_FORWARDED_HEADERS` defaults to false and the shipped nginx sits in
front, `client_addr` returns the proxy's address and the per-IP bucket is the
**entire deployment**. `reset-password` rejects a malformed token before any DB
work, so an anonymous attacker can burn a 100/hour budget in about a second and
every legitimate link-holder gets 429 for the next 59 minutes; ~2400 requests a
day makes it permanent. Login's precedent — the one the original design cited —
does not have this property: its per-IP bucket is `LOGIN_ATTEMPTS_PER_MIN * 6`
per **60 seconds**, equally shared but self-healing within a minute. Copy the
window, not just the arithmetic.

**The per-token bucket** replaces the security value the long window was meant to
supply, with a key an attacker cannot share. Its concrete job: step 8 returns 400
*without* consuming the token when the new password equals the current one, so a
link-holder could otherwise loop that branch at 60/min, each iteration costing an
Argon2 verify. Ten per hour per link is generous for a human and useless as an
amplifier. The key is a hash, so nothing sensitive lands in Redis.

**The admin route gets limiters too**, which the original design explicitly said
it did not need. It is authenticated and gated on `member:credential`, but that
permission is in the Admin preset, not just Owner, and an unbounded loop is an
unbounded mail bomb aimed at one member's inbox — and now also an unbounded
re-lock of an account somebody is trying to recover. The per-target bucket is the
one that matters; the per-caller bucket bounds the fan-out. **`cancel` spends the
per-caller bucket only.** It sends no mail and it can only ever restore access,
so charging it to the per-target bucket would mean an admin who forced five
resets in an hour cannot undo the fifth — a limiter blocking the remedy for the
thing it was limiting.

**Operators behind a proxy they control should set `API_TRUST_FORWARDED_HEADERS=1`**
so the per-IP buckets mean something. Repeat that in the README note for these
endpoints, along with the inverse hazard: turning it on *without* a proxy that
overwrites `X-Forwarded-For` lets a caller pick a fresh bucket per request.

### The per-email DoS, and what is left of the answer

The per-email bucket is consumed before the user lookup, so three requests
against `victim@company.com` deny that person self-service reset for an hour.
`login` has the identical property today (10/min keyed on a caller-supplied
email), so inheriting it is defensible — but it is a decision, not an accident.

For a member whose grants live in one org there is a way round it: the
admin-initiated route is authenticated and keyed on nothing the attacker
controls. For a member who holds grants in more than one org there is not — the
cross-org refusal applies to every admin reset, so nobody can act on their
behalf and the hour has to be waited out. That is a deliberate trade of
availability for the guarantee that no admin can invalidate a credential in an
org they have no standing in, and it is listed as a carried risk rather than
papered over with UI copy that would be false for exactly the people it
addresses.

Raising the per-email budget was rejected: it turns forgot-password into a free
mail cannon aimed at any address the attacker names.

Inherited and deliberate: `rate_limit` degrades to a per-process fallback on a
Redis error or a 250 ms timeout, so during a Redis outage with N API replicas an
attacker gets N× the budget.

## 6. `POST /v1/orgs/{org_id}/members/{user_id}/password-reset`

```rust
pub async fn reset_member_password(
    auth: AuthUser,
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: axum::http::HeaderMap,
    Path((org_id, user_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<ResetMemberPasswordReq>,
) -> Result<Json<serde_json::Value>, ApiError>
```

```rust
#[derive(Deserialize)]
pub struct ResetMemberPasswordReq {
    #[serde(default = "default_reset_action")]  // "reset"
    pub action: String,
}
```

Lives in `routes/orgs.rs` directly below `set_member_active` (orgs.rs:702).

There is one action the route performs and one action that undoes it, so the
default is the forward one and `cancel` has to be asked for by name. A body-less
`POST …/password-reset` — the shape an operator reaches for with `curl` when the
dashboard is down — resets. An unrecognised value is a `400`, never a silent
reset.

`ConnectInfo` and `HeaderMap` are on the signature so `requested_from` is
populated for admin rows. Without them, every `admin` row carries NULL — which is
exactly the half of the audit trail that matters, since self-service rows only
ever record an anonymous stranger's proxy address. This is possible because S2
already lifted `client_addr` to `pub(crate)`.

Response: `{ "ok": true, "action": "reset"|"cancel", "expires_at": "<rfc3339>"|null }`.
**The response never contains the token or the link** — see the last decision row.

Guards, in order, for both actions:

1. `let mut conn = db(&state).await?;`
   `authorize_org(&mut conn, auth.user_id, org_id, perm::MEMBER_CREDENTIAL).await?;`
2. Parse `action` as `"reset"` or `"cancel"`, else
   `ApiError::BadRequest("action must be \"reset\" or \"cancel\"")`.
3. **`reset` only**: `state.mail.is_none()` or `require_dashboard_url()` failing →
   `503 unavailable`, with the `require_*` error text as the message, **before
   anything is applied**. A reset whose mail cannot be delivered strands the
   target with no password and no link, and the ordering is the whole guarantee:
   a destructive change must never land when the message carrying its remedy
   cannot be sent. `cancel` is deliberately exempt — gating the undo on the same
   configuration that motivates it would make it unreachable in precisely the
   deployment that needs it.
4. The admin limiters (§5) — both for `reset`, per-caller only for `cancel`.
5. `let target_grants = guard_member_admin_action(&mut conn, auth.user_id, org_id, user_id, /* allow_self */ false).await?;`
   That helper — S2's extraction of `set_member_active`'s stack — carries:
   `authorize_org(..., perm::MEMBER_MANAGE)` as its first step, so this route
   demands `member:credential` **and** `member:manage` and the narrower
   permission never stands in for the broader one; user
   exists or 404; target holds at least one grant in **this** org or 404 (else any
   admin could reset any account in the deployment by guessing a uuid); self-target
   409; `check_no_escalation(&effective_at_org(caller), &union_permissions(target_grants))`
   against the target's full union, with the caller's *org*-scope set, because a
   password is account-global and an account-global act takes org-level standing;
   and `count_user_grants_outside_org(...) > 0` → 409, unconditionally, for the
   same reason `set_member_active` refuses. It returns the target's grant rows as
   `Vec<(String, Uuid, Value)>` — the shape `repo::user_grants_in_org` already
   returns — so the caller does not re-query. S1 adds no check of its own on top;
   a second local copy of the cross-org rule is one more place for the two to
   drift apart.
6. `!user.is_active` → `409 "reactivate this member before resetting their password"`.
   Same spirit as `create_grant`'s refusal to grant to an inactive account: a
   deactivated user's authority never grows.

There is deliberately **no last-`org:manage` guard**. A forced reset removes
nobody's permission — the target regains their account by using the link — so an
org can never be orphaned by it.

### `reset` — exact effects

1. `repo::set_user_must_change_password(user_id, true)`.
2. `repo::set_user_credentials_invalidated(user_id, Some(Utc::now()))`.
3. `let ids = repo::revoke_sessions_for_user(&mut conn, user_id, /* except */ None, repo::REVOKE_RESET_FORCED, Some(auth.user_id)).await?;`
   then `state.revocations.mark_revoked(&ids);`. `actor` is the admin, which is
   the only way `auth_sessions.revoked_by` ever records who forced the reset —
   `password_reset_tokens.initiated_by` answers the same question for the link,
   but not for the sessions. `mark_revoked` is what turns the dialog's "within a
   few seconds" into a statement about this replica rather than about its next
   poll.
4. `invalidate_password_reset_tokens_for_user(user_id, RESET_INVALIDATED_SUPERSEDED)` —
   an admin trigger is an authoritative act by an identified principal, so unlike
   self-service it supersedes outstanding links. The admin means *this* link, now,
   and a re-issue after a bounce must not leave two live links behind.
5. `insert_password_reset_token(..., Utc::now() + Duration::seconds(ADMIN_RESET_TTL_SECS), "admin", Some(auth.user_id), Some(addr))`.
6. `drop(conn)`, render (§7), then
   `mail.enqueue(MailKind::PasswordReset, &user.email, &content, Some(user_id), Duration::seconds(ADMIN_RESET_TTL_SECS))`.
   The recipient is known here, so this path uses `enqueue` rather than
   `enqueue_or_discard` — there is no branch to hide.

**Gates before revoke is deliberate and fail-safe.** `routes::auth::refresh`
(auth.rs:427-441) re-reads `user.must_change_password` and bakes it into the next
access token, so even if the revocation write fails the target's next refresh
mints a gated token within one access-token lifetime. The reverse order leaves a
window with sessions killed and no gate.

### The login refusal

`routes::auth::login` gains one check, immediately after the `is_active` check
and therefore after the Argon2 verification has already succeeded:

```rust
if user.credentials_invalidated_at.is_some() {
    return Err(ApiError::Auth(AuthError::PasswordResetRequired));
}
```

`AuthError` gains that variant: `403`, code `password_reset_required`, message
"an administrator reset this password — check your email for the link". It sits
beside `AccountDeactivated`, which is the same shape of statement (the password
was right, the account state is the problem) and is already documented as being
returned only after a successful verification.

Placing it before the verification would answer in microseconds for a
reset-pending account and in tens of milliseconds for every other one, handing
back the enumeration oracle `spend_dummy_verify` was written to close — and it
would leak, to anyone who can type an address, that a particular person is
mid-lockout.

`refresh` needs no equivalent check: step 3 above revoked the sessions, and
`revoke_sessions_for_user` revokes `refresh_tokens` in the same statement, so
there is no live refresh token to present. `/v1/auth/password` needs none either,
because reaching it requires a session nobody can now obtain.

**What the target experiences.** Every session ends within a few seconds, and the
old password stops being accepted at the login form: it returns
`password_reset_required` and nothing else. The emailed link is the only way back
in. `must_change_password` is set as well, and it is not redundant — it is what
survives a cancel, and it is the gate on the session the target would get if an
admin restored their access without them ever choosing a new password.

That is a genuine lockout, which is why §"Risks carried" names it as one and why
the cancel action exists. The UI must describe it in exactly these terms: an
admin who reads "we email them a link" and gets an account nobody can sign in to
will not use this feature twice.

### `cancel` — exact effects

1. `repo::set_user_credentials_invalidated(user_id, None)`.
2. `invalidate_password_reset_tokens_for_user(user_id, RESET_INVALIDATED_SUPERSEDED)`.

Nothing else, and in particular **not** `must_change_password`. Cancelling
restores the account's ability to sign in; it does not pretend the admin never
had a reason. The target logs in with their old password and lands on the
change-password screen, which is the admin's original intent minus the lockout.
Clearing the flag would also be unsound in a way that is easy to miss: it may
have been set long before this reset, by `create_member`'s reveal-once temp
password, and cancel has no way to tell the two apart.

Killing the outstanding links is the other half. Leaving them live means the mail
everyone had written off can be delivered a day later, and whoever opens it sets
a password for an account whose owner has been using their old one since the
cancel — a second, unannounced sign-out days after the incident was closed.

### Holding a connection

Neither handler performs network I/O — S0 moved the SMTP dial into the drain —
but `MailSender` owns a `PgPool` and checks out its **own** connection to run the
enqueue INSERT. So every enqueue call site in this slice must `drop(conn)` first.

The API pool is 16 connections for the entire process (`main.rs:68`) with a
5-second checkout timeout. A handler that still holds its connection while
calling `enqueue` needs a second one to make progress, and sixteen concurrent
resets then hold all sixteen and wait for a seventeenth: every one of them stalls
for the full timeout and every other endpoint in the process 500s alongside them,
for as long as the traffic lasts. The pre-S0 shape of this slice — awaiting a
10-second SMTP dial with `conn` live — was the same failure with a longer fuse,
which is why the pool discipline survives the move to an outbox rather than being
retired by it. `backend/bins/sauron-alerts/src/main.rs:99` already carries the
house comment (`drop(conn); // don't hold a pooled connection across the fan-out`).

## 7. Email content

One `MailKind::PasswordReset`; two copy variants selected by `mode`. Rendered
with `sauron_mail::text::substitute` into S0's multipart layout, text and HTML.

| Mode | Subject | Body |
|---|---|---|
| `self` | Reset your Sauron password | Someone asked to reset the password for this address. Link. Expires in 1 hour. "If this wasn't you, nothing has changed and you can ignore this email." |
| `admin` | Set a new Sauron password | An administrator of **{{org_name}}** reset your password. Link. Expires in 24 hours. "Your old password no longer works and you have been signed out on all devices. This link is how you get back in — if it has expired, ask an administrator of {{org_name}} to send you another." |

The admin variant's "if this wasn't you, ignore this" reassurance is deliberately
absent: ignoring it is not an option, and a recipient who is told it is will not
act until they next try to sign in.

Variables: `{{name}}` (falling back to the email), `{{reset_url}}`,
`{{expiry}}`, `{{org_name}}` for the admin variant — one extra `repo::get_org`
in the admin handler. The acting admin is **not** named: the org is what a
recipient needs to judge legitimacy, and naming an individual invites a reply to
a person rather than a route back into the account.

The link is `{DASHBOARD_URL}/#/reset-password?token={raw}`.

TTLs are compile-time constants in `routes/auth.rs`, not `Config`:
`SELF_RESET_TTL_SECS = 3_600` (a mailbox is a password-equivalent credential for
exactly as long as the link lives, and one hour is the modern norm) and
`ADMIN_RESET_TTL_SECS = 86_400` (the account is locked until this link is used,
so a one-hour window would turn "read the mail after lunch" into a second round
trip through an admin; a day is what makes the lockout survivable, and the admin
can re-issue or cancel either way). Both land in the same `expires_at` column. Two env knobs were rejected:
three files of documentation each, for values nobody tunes.

The same lifetime is passed to `enqueue` as a `Duration`, which `MailSender`
turns into the mail row's own `expires_at`, so the
message and the link it carries die together. S0 takes the expiry per enqueue
rather than per kind precisely for this: one constant on `MailKind::PasswordReset`
would mark an admin-initiated mail `expired before delivery` and blank its body an
hour after it was queued, while its token was still good for another twenty-three
— and blanking is what makes S0's manual-requeue path unable to recover it.

Every substituted variable is passed through `sauron_mail::text::html_escape` in
the HTML part and verbatim in the text part. **Every attribute in the HTML
template must be double-quoted**: `html_escape` escapes `&`, `<`, `>` and `"`
but **not** `'`, so a single-quoted `href='{{reset_url}}'` would be injectable
through an operator-controlled `DASHBOARD_URL`. Low severity — the operator owns
that value — and free to get right.

### A new exposure the outbox introduces

The rendered body, containing a live reset URL, now sits in `mail_outbox` in
plaintext until the drain sends it. S0 must blank the body on **successful**
send (and must not blank it on a transient failure, which would destroy the URL
unrecoverably). The residual window is bounded by the drain tick and the outbox
retention reaper. The marginal risk is small — anyone with DB read access already
has `users.password_hash` and could mint their own row — but it is a real change
from the pre-S0 shape, where the raw token existed only in the recipient's
mailbox, and it belongs in the S0 review.

## 8. The `must_change_password` gate, and route registration

Three lines in the flat route table in `backend/bins/sauron-api/src/main.rs`:

```rust
.route("/v1/auth/forgot-password", post(routes::auth::forgot_password))   // beside main.rs:149-154
.route("/v1/auth/reset-password",  post(routes::auth::reset_password))
.route("/v1/orgs/{org_id}/members/{user_id}/password-reset",
       post(routes::orgs::reset_member_password))                          // beside main.rs:167
```

No nested router, no prefix. The codebase has none, and
`password_change_allowed_path` is an exact-path match that a prefix would
silently break.

**The allowlist needs no edit.** Both self-service routes are unauthenticated, so
`password_change_gate` (`extractors.rs:35`) is never reached and the allowlist
stays exactly two paths. That is the clean design, and it is *why* the reset
token travels in the body rather than as a bearer. To make the property durable,
add `"/v1/auth/forgot-password"` and `"/v1/auth/reset-password"` to the
**rejected** list inside the existing `password_change_allowlist_is_exactly_two_paths`
test (`extractors.rs:177-193`), with a comment saying they are deliberately
unauthenticated and must never need the allowlist. A future change that bolts an
extractor onto them then turns the suite red instead of silently 403ing every
reset for exactly the population that needs one.

**Kill latency.** With S2 shipped, `mark_revoked(&ids)` puts the killed sessions
into the acting replica's snapshot immediately and the other replicas pick them
up within `AUTH_REVOCATION_POLL_SECS` (default 5), so the forced reset promises
"signed out within a few seconds" and the dialog may say so. Dropping the
`mark_revoked` call would make that copy false on the very replica the admin is
talking to until its own next poll, which is the one place the delay is visible.
Without S2 at all it would be up to `JWT_ACCESS_TTL_SECS` (default 900), because
`must_change_password` is baked into the JWT at issue time and the extractor
reads the claim, not the row. That gap is the reason S2 is sequenced ahead of S1.

## 9. Reaping

`repo::prune_password_reset_tokens(&mut conn, PASSWORD_RESET_RETENTION_DAYS)`
with `const PASSWORD_RESET_RETENTION_DAYS: i64 = 30`, mounted hourly into S0's
`backend/bins/sauron-api/src/tasks.rs` supervisor.

**Not `sauron-alerts`.** The original design put it in that binary's existing
hourly prune loop, next to `prune_alert_events`. But `packaging/rpm/SETUP.md:71`
— the shipped install procedure — is
`systemctl enable --now sauron-api sauron-ingest sauron-monitor sauron-tier`, and
`sauron-alerts` is not in it. There is no preset file under `packaging/rpm/`, so
`%systemd_post` falls through to the distro default of `disable`. On every RPM
deployment that reaper would simply never run — and this endpoint is
unauthenticated, so the table would grow unbounded along with a `user_id` btree
and a UNIQUE btree over a 64-char hash per row, while the design's stated
"30-day audit window" quietly became "forever". The programme rule is that a
table's reaper lives in the process that owns its write path; `sauron-api` is the
only writer here.

Deleting these rows disables nothing — unlike `refresh_tokens`, whose revoked
rows are load-bearing for replay detection, nothing reads a dead reset row.

No new env var. This table holds a handful of tiny short-lived rows and an env
knob costs three files of documentation.

## 10. Frontend

### New pages

**`dashboard/src/pages/ForgotPassword.svelte`** — public, `AuthLayout`,
title "Reset your password". One `Input` (`type=email`, `autocomplete="email"`)
and a primary `Button`. On submit it calls `forgotPassword(email.trim())` and
then **always** swaps the form for the same static confirmation panel regardless
of outcome, mirroring the API's generic 200 so the UI cannot become the oracle
the API refuses to be:

> If an account exists for that address, we have sent a link to reset the
> password. The link expires in 1 hour.
>
> Nothing arrived? Check your spam folder, then try again in a little while.

**The panel offers no route through an administrator**, and the omission is the
considered part. An admin cannot act at all for a member who holds grants in
another org, and for everyone else the only admin action there is invalidates the
password the person is still using. Copy that sends a stranded user to ask for
something that will lock them out further is worse than copy that tells them to
wait.

Two exceptions to "always the same panel": a `429` additionally toasts the
server's `rate_limited` message, and a `404` (dashboard upgraded ahead of the
server) renders "This server does not support password reset yet — ask an
administrator to finish the upgrade." That one is about the deployment, not about
the account, and it is addressed to whoever is running the upgrade.

A deployment with no SMTP configured is **not** one of the exceptions: the route
answers `200` there too, so this panel is what an anonymous caller sees, and it
must not describe how the server is configured to someone who has proved nothing.
The operator finds out from the startup WARN and from the members page, not from
here.

Footer: `<a href="#/login">Back to sign in</a>`.

**`dashboard/src/pages/ResetPassword.svelte`** — public, `AuthLayout`, title
"Choose a new password". Reads the token **once at init**, using the house
pattern from `Issues.svelte:2/36` — `import { querystring } from 'svelte-spa-router'`
then `const token = readResetToken($querystring)` — not reactively, so a later
navigation cannot swap the token mid-submit. `token === null` renders the
invalid-link state immediately, with a link to `#/forgot-password` and no form.

Two `Input`s (New password / Confirm), driven by the shared `passwordRules`.
On `401 invalid_token` it renders its own copy — "This reset link is invalid or
has expired — request a new one" — rather than the server string.

The success path is **three statements, in this order**:

```ts
await authStore.logout();   // best-effort server call, unconditional clearLocal()
sessionStore.reset();
replace('/login');
```

`replace('/login')` alone is a no-op for the visitor this page most needs to
handle. `App.svelte:11` defines `PUBLIC_ROUTES = ['/login', '/register']` and
lines 20-24 run an `$effect` that pushes `authStore.isAuthenticated` visitors to
`/issues`. `isAuthenticated` is pure local state
(`auth.svelte.ts:41-43`), untouched by a reset that happened server-side. So a
user who was already signed in in another tab — precisely the person the routing
below is built to accommodate — would submit their new password, get bounced
straight into `/issues` on a session the backend just revoked, never see the
login screen, never confirm the new password works, and only be ejected up to 15
minutes later when `refresh()` fails. `authStore.logout()` (auth.svelte.ts:157-169)
is already best-effort against the server and unconditionally does
`clearLocal()` + `status = 'unauthenticated'`, so the effect stops matching.

### The login page

`Login.svelte` gains a `Forgot your password?` link in the form footer beside
`New to Sauron?` — the entry point without which the two new pages are reachable
only by typing a URL.

It also has to render the new refusal, and rendering it as a red form error is
not enough. `submit`'s catch arm surfaces `errorMessage(err)` for everything, so
a target whose admin forced a reset would see "an administrator reset this
password" in the same red box as a typo'd password, from the same screen they
have just been told to stop using. The page branches on the code the way the
store already branches on `password_change_required` — a matching
`isPasswordResetRequired(err)` beside `isPasswordChangeRequired`
(`auth.svelte.ts:14`), keyed on `403` / `password_reset_required` — and swaps the
form for a panel:

> An administrator reset the password for this account. We have emailed
> {the address they typed} a link to set a new one. Nothing arrived? Check your
> spam folder, or ask the administrator to send it again.

Naming the address is safe here and nowhere else on this page: the caller just
proved they know the password for it. This is the one place in the feature where
"ask an administrator" is honest copy, because the administrator has already
acted and re-issuing is exactly what they should do next.

### Routing, and the `PUBLIC_ROUTES` asymmetry

`dashboard/src/routes.ts` gains two **bare** components beside `'/login': Login`
and `'/register': Register` — no `wrap`, no `guarded()`, no `authed` or
`passwordCurrent` condition. Wrapping either would fire `conditionsFailed`, which
pushes to `/login` or `/change-password` and makes a reset link unusable.

`dashboard/src/App.svelte` becomes
`const PUBLIC_ROUTES = ['/login', '/register', '/forgot-password'];`.

`/forgot-password` **is** added: an authenticated user who lands there should be
bounced to `/issues`, because Change password is what they want.

`/reset-password` is **deliberately not** added, and needs an inline comment
saying why: that array feeds the `$effect` that pushes authenticated users away,
and a logged-in user clicking their own reset link would be bounced off it before
they could use it. It is neither in `PUBLIC_ROUTES` nor guarded, so it simply
renders for everyone. Note `$location` from svelte-spa-router excludes the query
string, so the comparison is `'/reset-password'` even with `?token=…`.

### `dashboard/src/lib/models/password-reset.ts` (+ colocated `.test.ts`)

Pure decision logic, no Svelte and no DOM — the repo has no DOM test
environment.

| Export | Contract |
|---|---|
| `readResetToken(qs: string \| null): string \| null` | `new URLSearchParams(qs ?? '').get('token')`, trimmed; empty string maps to `null` |
| `passwordRules(next: string, confirm: string): { tooShort; mismatch; canSubmit }` | `ChangePassword.svelte`'s derivations minus `reused` — there is no current password on the reset page. One definition, shared by both |
| `isPasswordResetRequired(err: unknown): boolean` | `403` + `password_reset_required`, the twin of `isPasswordChangeRequired` (`auth.svelte.ts:14`). Lives here rather than in the auth store because the login page is its only caller and the store has no reason to know |
| `canResetMemberPassword(member: Member, currentUserId: string, canCredential: boolean): boolean` | False without `canCredential`, false for self, false for `!member.is_active`, false when a reset is already pending — mirroring the server's refusals so the action is never offered for something the server will reject |
| `canCancelPasswordReset(member: Member, currentUserId: string, canCredential: boolean): boolean` | The same three guards — has the permission, not self, active — but true only when a reset **is** pending. At most one of the two predicates holds for a given member, which is what lets the row carry one menu item instead of two that contradict each other |

Both predicates take **`Member`**, not `MemberGrant`. `MemberGrant`
(`models/index.ts:238`) is one grant row; `Member` (:257) is the grouped person
with a `grants: MemberGrant[]`, and `MembersTable.svelte` iterates
`grouped: Member[]` — its existing `ontoggle` is already `(member: Member) => void`.
Structural typing means the wrong annotation compiles and `tsc` never catches it;
it would just be documented and unit-tested against a shape the caller never
passes. Build the test fixtures as `Member` objects.

"Pending" needs a source. `GET /v1/orgs/{org}/members` is the only place the
dashboard learns anything about a member's account state, so `MemberGrant` gains
`credentials_invalidated_at: string | null` — one more column in the
`repo::list_org_grants` select tuple (repo.rs:680), one more field on the
`MemberGrant` response struct, and one more field carried onto the grouped
`Member`. Without it the cancel action exists on the server and is unreachable
from the UI, which is the same as not existing: the admin who needs it is looking
at a members table, not at `curl`. It is visible to `member:read`, which is a
wider audience than `member:credential` — acceptable, because `is_active` is
equally an account-state disclosure and already ships in the same row.

### Members page

`MembersTable.svelte` gains one `Props` callback
`onresetpassword: (member: Member, action: 'reset' | 'cancel') => void` — one
callback rather than two, so the table cannot offer a member both — and, with it,
the row-action **overflow menu**. S1 owns that conversion. S2
leaves the row at three inline buttons, which still fits; S1 makes it four, which
does not, and `dashboard/src/lib/components/ui/` has fourteen components and no
menu primitive among them to reuse. So this slice adds
`RowActionsMenu.svelte` there — a kebab trigger, dismissal on outside click and
Escape, focus returned to the trigger on close — and folds S2's inline
"Sign out all devices" button into it. Menu order is fixed once, with destructive
actions last: **Edit / Reset password / Sign out all devices / Deactivate**. The
reset item renders only when
`canResetMemberPassword(member, currentUserId, canCredential)`, and is replaced
in place by **Cancel password reset** when
`canCancelPasswordReset(...)` — same slot, so the menu never grows a row that is
disabled half the time. `canCredential` is
`sessionStore.can('member:credential') && sessionStore.can('member:manage')`,
mirroring the two checks the server actually makes — `member:credential` narrows
`member:manage` rather than replacing it, so a menu that gates on either one
alone offers an action the server refuses.

A member with a reset pending also carries a badge in the row itself, beside the
existing inactive marker. An account nobody can sign in to is a state the table
has to show without being opened: the admin who forced it may not be the one
fielding "I can't log in", and the person fielding it needs the answer in the
list.

`Members.svelte` adds
`let resetTarget = $state<{ member: Member; action: 'reset' | 'cancel' } | null>(null)`,
passes `onresetpassword={(m, a) => (resetTarget = { member: m, action: a })}`, and renders
`ResetPasswordDialog` beside the existing deactivation `ConfirmDialog`
(Members.svelte:494-503). It needs `authStore.user?.id` for the self-check.

**`dashboard/src/lib/components/members/ResetPasswordDialog.svelte`** — a `Modal`
in danger styling, sharing S2's confirm-with-consequence-text pattern. It states
the lockout before the confirm button, because the sentence an admin reads here
is the only warning between them and an account that cannot sign in:

> **{email} will not be able to sign in until they use the emailed link.**
>
> Their current password stops working immediately and they are signed out of
> every device within a few seconds. We email them a link that expires in 24
> hours. If it does not arrive, come back here to send another or to cancel.

Confirm button text is **Reset password**. The cancel-side dialog is the same
component in its second state, reached from the Cancel password reset menu item:

> {email} will be able to sign in with their existing password again. They will
> still be asked to choose a new one when they do. Any reset link already sent
> stops working.

Errors surface `errorMessage(err)` verbatim, because the server's 409s carry the
actionable text (self, inactive, cross-org) exactly as `toggleActive` already
does (Members.svelte:311-314). A `503` is the one an operator will actually hit,
and it arrives with the `require_smtp` / `require_dashboard_url` text, which says
which setting is missing. A `404` renders the upgrade-in-progress copy.

No `Sidebar.svelte` edit and no new nav entry: the two new pages are public auth
screens living outside `AppShell`, like Login/Register/ChangePassword, and the
admin trigger is a control on an existing page.

### API client and types

`dashboard/src/lib/api/auth.ts` gains `forgotPassword(email)` and
`resetPassword(token, newPassword)`, both through **`bareClient`**, not `api`:
they are unauthenticated, must never carry a stale bearer, and must never enter
the single-flight 401 refresh-and-replay loop — the same reason
login/register/refresh/logout use it.

`dashboard/src/lib/api/orgs.ts` gains
`resetMemberPassword(orgId, userId, action: 'reset' | 'cancel')` through `api`
(it needs the bearer), beside `setMemberActive` (orgs.ts:75).

`dashboard/src/lib/models/index.ts` gains
`interface MemberPasswordResetResult { ok: boolean; action: 'reset' | 'cancel'; expires_at: string | null; }`
and the `credentials_invalidated_at` field on `MemberGrant` and `Member`.

Nothing is added to the `Permission` union or to `models/permissions.ts`: S2
mints `member:credential` and ships all five of its coordinated mirrors, so S1
only reads it — `sessionStore.can('member:credential')` where the members page
today reads `member:manage`. If S1 somehow lands first, those five edits come
with it, and `permissions.test.ts`, which parses `rbac.rs` and fails on drift, is
what enforces that they arrive together.

## Error handling

| Case | Status | Code / message |
|---|---|---|
| forgot-password, any parsing input | 200 | `{"ok": true}`, byte-identical for unknown / deactivated / happy, and for a deployment with no SMTP |
| forgot-password, malformed email | 400 | "a valid email is required" |
| Admin `reset`, SMTP or `DASHBOARD_URL` unconfigured | 503 | `unavailable`, carrying the `require_*` error text; nothing applied. `cancel` is exempt and still succeeds |
| Login with the old password after an admin reset | 403 | `password_reset_required`, "an administrator reset this password — check your email for the link"; returned only after the password verifies |
| Unmigrated schema on forgot-password | 500 | Uniform, via the preflight |
| Any dead token (unknown / consumed / invalidated / expired / stale fingerprint / lost CAS) | 401 | `invalid_token`, "invalid or expired token" |
| Reset onto a deactivated account | 403 | `account_deactivated` |
| Reset with the current password | 400 | "the new password must be different from the current one"; token **not** consumed |
| Password too short / too long | 400 | `change_password`'s strings verbatim |
| Any limiter | 429 | `rate_limited` |
| Admin: bad `action` | 400 | `action must be "reset" or "cancel"` |
| Admin: unknown user, or no grant in this org | 404 | Deliberately indistinguishable |
| Admin: self-target | 409 | "use Change password to reset your own password" |
| Admin: inactive target | 409 | "reactivate this member before resetting their password" |
| Admin: cross-org target | 409 | "this member belongs to another organization and cannot be reset from here" |
| Admin: caller lacks `member:credential` **or** `member:manage`, or is outranked by the target | 403 | Existing `create_grant` denial shape |

`ApiError` gains one variant: `Unavailable(String)` → `503`, code `unavailable`.
It is the only addition to `backend/bins/sauron-api/src/error.rs`.
`AuthError` gains `PasswordResetRequired` → `403`, code `password_reset_required`
— the only addition to `backend/crates/sauron-auth/src/extractors.rs` beyond the
allowlist test.

## Testing

**Constraint:** CI runs `cargo test --workspace` with no Postgres service, and
the dashboard has no DOM test environment. Unit tests therefore cover pure
functions; everything DB-dependent goes in the integration harness, which skips
itself when `TEST_DATABASE_URL` / `TEST_REDIS_URL` are unset.

**Unit, no DB:**

- `sauron-auth/src/extractors.rs` — extend
  `password_change_allowlist_is_exactly_two_paths` with the two new paths in the
  rejected list, plus the comment explaining that they are deliberately
  unauthenticated. Extend the `parts()` test beside the `AccountDeactivated`
  assertion with `PasswordResetRequired` → `403` / `password_reset_required`: the
  dashboard branches on that exact string, and a rename would otherwise only be
  caught by a human clicking through a locked-out login.
- `routes/auth.rs` — `reset_link(dashboard_url, token) -> String` produces
  `https://host/#/reset-password?token=…` with and without a trailing slash;
  `expiry_wording(ttl_secs)` gives "1 hour" and "24 hours" *derived from the
  consts*, never hand-typed; token-shape validation accepts a real
  `opaque_token()` and rejects 63 chars, 65 chars, and a non-hex string.
- Email rendering — the text part contains the raw URL on its own line and the
  "if this wasn't you" sentence; the HTML part contains the URL inside a
  double-quoted `href` with every variable escaped; a display name of
  `<script>alert(1)</script>` appears escaped in HTML and verbatim in text; a
  variable missing from the map renders blank rather than echoing `{{key}}`.
- `repo.rs` — S2's classification test sorts every `REVOKE_*` constant into
  exactly one of three buckets: deliberate, has-its-own-branch-in-`refresh`, and
  theft-signal. S1's two constants are deliberate, so they join
  `DELIBERATE_REVOKE_REASONS` and take the array to `[&str; 5]`; the test fails
  on an unclassified reason, so neither can be added without someone choosing.
  Membership is not "everything except `REVOKE_REUSE`" and must never be
  restated that way here: it would sweep in `REVOKE_ROTATED`, sending every
  ordinary rotation down the early-return path and breaking the multi-tab grace
  window, and `REVOKE_LOGOUT`, which disables replay detection on logged-out
  tokens.

**Dashboard vitest** — `dashboard/src/lib/models/password-reset.test.ts`:
`readResetToken` for `'token=abc'`, `''`, `null`, `'token='`,
`'a=1&token=abc&b=2'`, and a percent-encoded token; `passwordRules` short /
mismatch / valid; `isPasswordResetRequired` true for the real error shape and
false for a 403 carrying `password_change_required`, which is the confusion the
two names invite; `canResetMemberPassword` false for self, false without
`canCredential`, false for an inactive member, false when a reset is pending, true
otherwise; `canCancelPasswordReset` the mirror of it, plus the assertion that the
two are never both true for the same member — with `Member` fixtures.

**Integration**, new file `backend/bins/sauron-api/tests/http_password_reset.rs`,
following `http_workflows.rs` exactly: ephemeral DB named
`sauron_test_{unix_ts}_pr{uuid}` (timestamp segment **first** — the stale-DB
reaper depends on it), `run_pending_migrations`, spawn the real binary via
`env!("CARGO_BIN_EXE_sauron-api")`, poll `/health`, return `None` to skip when
the env is unset. Mail is observed by reading `mail_outbox`, and the raw token by
reading `password_reset_tokens` — the test has DB access, so nothing needs to be
logged for it.

- **Anti-enumeration** — a registered address, an unregistered address, and a
  registered-but-deactivated address all return 200 with byte-identical bodies.
  A body missing `@` returns 400.
- **Happy path** — register, mint a row via `repo::insert_password_reset_token`,
  reset with the raw token, assert 200 `{"ok":true}` and **no tokens in the
  body**, then: new password logs in, old password 401s, a refresh token captured
  before the reset 401s.
- **Five dead-token states**, each 401 `invalid_token` with an identical body:
  never existed; consumed twice; invalidated; `expires_at` in the past; and
  fingerprint gone stale (issue, change the password via `/v1/auth/password`,
  then try the link). The last one is what proves the column earns its keep.
- **Concurrency** — two simultaneous resets with the same token: exactly one 200,
  one 401. A SELECT-then-UPDATE implementation fails this.
- **Compare-and-swap** — issue a link, then change the password out from under it
  between the fingerprint read and the write. Simulate by issuing a token,
  calling `set_user_password` directly to a third value, then submitting: 401,
  and the third value still authenticates.
- **Reuse policy** — resetting to the current password returns 400 and does
  **not** consume the token; the same token then works with a different password.
- **Admin reset** — 200; `must_change_password` true; `credentials_invalidated_at`
  set; the pre-existing refresh token 401s; **the old password now returns 403
  `password_reset_required` at `/v1/auth/login`** and yields no tokens at all.
  That last assertion is the feature: an implementation that merely gates the
  session passes every other line in this file.
- **Admin reset, then the link** — the emailed token resets the password to a new
  value; the new password logs in and the resulting access token reaches
  `GET /v1/me` without a `password_change_required` gate, proving the one
  statement in §2 cleared both the flag and the invalidation.
- **Admin cancel** — 200; `credentials_invalidated_at` NULL; the old password logs
  in again; `must_change_password` still **true**, and `GET /v1/me` with the
  resulting token still returns 403 `password_change_required`; the token issued
  by the reset it cancelled now 401s. Also: cancel on a member who owed a
  temp-password change before any of this leaves them still owing it.
- **Cancel works with no mail configured** — with `state.mail` unset, `reset`
  returns 503 and `cancel` returns 200. This is the assertion that stops the 503
  check being hoisted above the action parse in a tidy-up.
- **Admin guard stack**, one assertion per refusal: self 409; no grant in this
  org 404; unknown user_id 404; inactive target 409; cross-org target 409 for
  **both** actions; caller with `member:read` only 403; caller holding
  `member:manage` but not `member:credential` 403 (the assertion that proves the
  route moved to the new permission rather than merely mentioning it); Admin
  acting on an Owner 403 via `check_no_escalation`.
- **Supersede semantics** — two admin resets leave the first token 401 and the
  second usable; two self-service requests leave **both** usable until one is
  consumed, after which the other 401s (via the sibling sweep and, independently,
  via the fingerprint).
- **Mail enqueue** — each mode writes exactly one `mail_outbox` row with
  `kind = 'password_reset'`, a body containing the raw token, and an `expires_at`
  matching its own token's (1 hour for `self`, 24 for `admin` — the assertion that
  keeps the two clocks tied). A forgot-password for an **unknown** address writes
  **zero** rows to either table while still returning 200, which is what proves
  the discard branch commits nothing. With `state.mail` unconfigured,
  forgot-password still returns 200 and the admin reset returns 503, and neither
  writes a row anywhere.

**Manual verification** for what no test reaches: run with S0's `SMTP_SINK` (or a
local Mailpit), trigger both flows, and confirm the mail arrives multipart with
both parts rendering; the link opens `#/reset-password?token=…` and the token
appears in **no** API access log; the forgot-password panel is identical for a
real and a fake address; the members menu item is hidden for self and for a
deactivated member and flips to Cancel password reset once a reset is pending;
signing in as the reset target shows the emailed-link panel rather than a red
form error; and — the one the critique caught — resetting while signed in in
another tab lands on `#/login` rather than bouncing into `/issues`.

## Files

**New**

- `backend/migrations/2026-08-01-000036_password_reset/{up,down}.sql`
- `backend/bins/sauron-api/tests/http_password_reset.rs`
- `dashboard/src/pages/{ForgotPassword,ResetPassword}.svelte`
- `dashboard/src/lib/models/password-reset.ts` + `password-reset.test.ts`
- `dashboard/src/lib/components/members/ResetPasswordDialog.svelte`
- `dashboard/src/lib/components/ui/RowActionsMenu.svelte` — the kebab/overflow
  menu the members row action count now requires

**Modified**

- `backend/crates/sauron-db/src/{schema.rs,models.rs,repo.rs}` — including the
  `users` block, the `User` struct and `list_org_grants`
- `backend/bins/sauron-api/src/routes/auth.rs` — two handlers, the login check,
  six constants, two TTLs, the render helpers
- `backend/bins/sauron-api/src/routes/orgs.rs` — `reset_member_password`, and
  `credentials_invalidated_at` on the `MemberGrant` response
- `backend/bins/sauron-api/src/main.rs` — three routes
- `backend/bins/sauron-api/src/error.rs` — `ApiError::Unavailable`
- `backend/bins/sauron-api/src/tasks.rs` — mount the reaper
- `backend/crates/sauron-auth/src/extractors.rs` — `AuthError::PasswordResetRequired`
  and the two tests
- `dashboard/src/{routes.ts,App.svelte}`
- `dashboard/src/pages/{Login.svelte,Members.svelte}`
- `dashboard/src/lib/components/members/MembersTable.svelte`
- `dashboard/src/lib/api/{auth.ts,orgs.ts}`, `dashboard/src/lib/models/index.ts`
- `wiki/Dashboard.md` — a "Forgot your password" subsection and a members note
  saying, in the dialog's exact wording, that a reset locks the member out until
  they use the link and how to cancel one. No new page, so no `_Sidebar.md` /
  `Home.md` registration
- `packaging/rpm/SETUP.md` §11 — one row: migration 000036, symptom **"nobody can
  sign in"**. `users` gains a column and `User` is `Selectable`, so an upgraded
  binary against an unmigrated database fails `login`, `refresh` and `/v1/me`
  with a missing-column error, not just the new routes. The row must say that in
  those words; an operator reading "the reset routes 500" will not connect it to
  a deployment-wide auth outage in time

`packaging/rpm/` is otherwise untouched: no new binary, so `binaries.txt` and
`sauron.spec`'s `%files` do not move; no new unit; no new per-service `.env`.

## Risks carried

- **A reset link makes the mailbox a password-equivalent credential for the
  TTL.** Nothing new — it is how every service works — but the controls are worth
  naming: 1h self-service / 24h admin, single-use enforced atomically, implicit
  invalidation via the fingerprint, admin issue supersedes outstanding links, and
  the token never reaches a server log because it lives in the URL fragment. A
  shared or compromised inbox defeats all of it, which is an argument for 2FA
  later, not for a shorter TTL.
- **The per-email bucket is an availability lever against a named victim.** Three
  requests deny an hour of self-service reset. For a single-org member the admin
  route is the way round it, since it is keyed on nothing the attacker controls.
  **For a member who holds grants in more than one org there is no way round it**:
  every admin reset refuses a cross-org target, so nobody can act on their behalf
  and they wait out the hour. That is the accepted cost of not letting an admin
  invalidate a credential in an org they have no standing in, and it is the one
  place in this feature where the answer to a stuck user is "wait".
- **An admin-initiated reset is a lockout, and it is gated on a mail relay.**
  Between the reset and the link being used, the account cannot be signed into at
  all. If SMTP is misconfigured the route refuses before applying anything, and if
  the mail is merely lost the admin re-issues or cancels — but both remedies live
  on the same route, so a caller who forces a reset and then loses their own
  access (their session revoked by someone else, their org's last admin
  deactivated) leaves an account only `psql` can recover. The 24-hour TTL and the
  cancel action shrink that window; nothing closes it short of a second admin.
- **`ALERTS_ALLOW_PRIVATE` must not become the SMTP switch.** With
  `allow_private: false`, `resolve_checked` rejects any relay on loopback or an
  RFC1918 address — which is the normal self-hosted topology. An operator whose
  only symptom is "reset mail never arrives" will be pushed to set
  `ALERTS_ALLOW_PRIVATE=1`, which simultaneously removes the SSRF guard from
  org-admin-configured `notification_channels` in the same process. S1 is the
  change that creates that pressure, so S0 owning a separate `SMTP_ALLOW_PRIVATE`
  is a hard requirement, not a preference.
- **`password_reset_tokens` is the audit log.** `mode`, `initiated_by`,
  `requested_from` and `consumed_at` are the only record that an admin forced a
  reset on someone. The 30-day reaper therefore also caps how far back that
  question can be answered, and `requested_from` is the proxy's address in the
  default topology. Both are acceptable now; both argue for a real
  `audit_events` table later.
- **A misconfigured relay fails quietly on the self-service path, by design.**
  The caller always sees the same 200, and closing the anonymous config-state
  oracle means there is no room for a signal on that route at all. Three
  mitigations elsewhere are therefore all required: S0's startup WARN when SMTP
  is unconfigured, a `tracing::error!` per failed send in the drain and per
  swallowed enqueue error in the handler, and the `503` on the admin route, which
  is the one refusal a human sees at the moment they are looking. Without all
  three the feature can be dead in production and look healthy.

## Follow-ups (out of scope)

- Denylisting the target's outstanding **access** tokens on an admin reset, once
  S2's session identity makes it possible to be exact rather than merely fast.
- Real invitation tokens replacing the reveal-once temp password, now that a
  one-time-token table exists.
- An `audit_events` table.
- Email verification at registration.
- A voluntary change-password entry point for non-forced users.
- A prefetch guard on the reset link, if a mail client turns out to burn tokens by
  opening them. Single-use at submit is enough until then.
