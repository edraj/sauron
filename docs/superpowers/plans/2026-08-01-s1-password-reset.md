# Password Reset Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give a Sauron account two ways back from a forgotten or compromised password — a self-service emailed link, and an admin-forced reset that stops the current password authenticating until the emailed link is used.

**Architecture:** A new `password_reset_tokens` table stores only SHA-256 hashes of high-entropy opaque tokens that exist in plaintext solely inside the email, alongside a `password_fingerprint` (a hash of the account's password hash) that implicitly kills a link the moment the password moves for any other reason. Three axum routes drive it — two unauthenticated (`forgot-password`, `reset-password`) and one org-admin route that additionally stamps `users.credentials_invalidated_at`, which `login` refuses on *after* the Argon2 verification has already succeeded. Mail never touches the request path: handlers render and INSERT into S0's `mail_outbox`, and S0's drain does the SMTP dial.

**Tech Stack:** axum 0.8, diesel + diesel-async + Postgres, `sauron-auth` (Argon2 + `hash_token`), `sauron-core::ids::opaque_token`, `sauron-mail` (S0), Redis fixed-window rate limiting, Svelte 5 runes + svelte-spa-router, vitest.

---

## Prerequisites — verify before Task 1

This slice lands **third** in the programme (S0 → S2 → **S1** → S3). It calls foundations that S0 and S2 build. Run each check below; every one must print at least one line. If any prints nothing, S0 or S2 has not landed and this plan cannot start.

```
cd /home/splimter/projects/freelance/sauron
grep -rn "pub struct MailSender" backend/bins/sauron-api/src/mail.rs
grep -rn "fn enqueue_or_discard" backend/bins/sauron-api/src/mail.rs
grep -rn "PasswordReset" backend/crates/sauron-mail/src/lib.rs
grep -rn "pub struct MailContent" backend/crates/sauron-mail/src/
grep -rn "pub fn substitute\|pub fn html_escape" backend/crates/sauron-mail/src/text.rs
grep -rn "pub fn layout" backend/crates/sauron-mail/src/template.rs
grep -rn "require_dashboard_url" backend/crates/sauron-core/src/config.rs
grep -rn "pub fn spawn_periodic" -A 8 backend/bins/sauron-api/src/tasks.rs
grep -rn "prune_mail_outbox" backend/bins/sauron-api/src/tasks.rs
grep -rn "pub fn dedup_window" -A 8 backend/crates/sauron-mail/src/kind.rs
grep -rn "body_text\|recipient_key" backend/crates/sauron-db/src/schema.rs
grep -rn "pub async fn revoke_sessions_for_user" backend/crates/sauron-db/src/repo.rs
grep -rn "DELIBERATE_REVOKE_REASONS" backend/crates/sauron-db/src/repo.rs
grep -rn "password_reset\|reset_forced" backend/migrations/*000035*/up.sql
grep -rn "fn mark_revoked" backend/bins/sauron-api/src/
grep -rn "guard_member_admin_action" backend/bins/sauron-api/src/routes/orgs.rs
grep -rn "MEMBER_CREDENTIAL" backend/crates/sauron-auth/src/rbac.rs
grep -rn "pub(crate) async fn rate_limit\|pub(crate) fn client_addr" backend/bins/sauron-api/src/routes/auth.rs
grep -rn "onrevokesessions\|canRevokeSessions\|revokingUserId" dashboard/src/lib/components/members/MembersTable.svelte
psql postgres://sauron:sauron@172.20.0.2:5432/sauron -c "\d mail_outbox"
```

The last command is the one that is not a grep: it must list a `mail_outbox` table whose
columns include `kind`, `recipient`, `recipient_key`, `subject`, `body_text`,
`body_html`, `status`, `created_at` and `expires_at`. Task 10 reads three of
those by name in raw SQL, so a differently-named body column is a compile-time
break there rather than a runtime surprise.

The exact symbols this plan calls, and what each does:

| Symbol | Signature this plan assumes | Owner |
|---|---|---|
| `crate::mail::MailSender` | `Clone`; owns its own `PgPool` | S0 |
| `MailSender::enqueue` | `async fn enqueue(&self, kind: MailKind, recipient: &str, content: &MailContent, user_id: Option<Uuid>, ttl: std::time::Duration) -> anyhow::Result<Option<Uuid>>` | S0 |
| `MailSender::enqueue_or_discard` | `async fn enqueue_or_discard(&self, kind: MailKind, recipient: Option<&str>, content: &MailContent, user_id: Option<Uuid>, ttl: std::time::Duration) -> anyhow::Result<Option<Uuid>>` — renders on both branches, normalises a missing recipient to `discard@invalid`, commits nothing when it is `None` | S0 |
| `sauron_mail::MailKind::PasswordReset` | maps to the `mail_outbox.kind` string `"password_reset"` | S0 |
| `MailKind::dedup_window` | `pub fn dedup_window(&self) -> std::time::Duration` — **300 seconds for `PasswordReset`**. A second message to the same address inside that window is silently suppressed and `enqueue` returns `Ok(None)`. Task 10 works around it explicitly; nothing in the handlers does | S0 |
| `mail_outbox` columns | `id, kind, recipient, recipient_key, subject, body_text, body_html, status, attempts, max_attempts, next_attempt_at, expires_at, last_error, user_id, created_at, updated_at, sent_at` — in that declaration order. Task 10 reads `recipient`, `kind`, `body_text`, `created_at` and `expires_at` by name in raw SQL | S0 |
| `sauron_mail::MailContent` | `pub struct MailContent { pub subject: String, pub text: String, pub html: String }` | S0 |
| `sauron_mail::text::substitute` | `pub fn substitute(template: &str, vars: &BTreeMap<String, String>) -> String` — unknown keys render blank, never echo `{{key}}` | S0 (moved from `sauron_alerts::render`) |
| `sauron_mail::text::html_escape` | `pub fn html_escape(s: &str) -> String` — escapes `& < > "` but **not** `'` | S0 |
| `sauron_mail::template::layout` | `pub fn layout(subject: &str, body_html: &str) -> String` — the house HTML shell | S0 |
| `Config::require_dashboard_url` | `pub fn require_dashboard_url(&self) -> anyhow::Result<&str>` — no trailing slash | S0 |
| `crate::tasks::spawn_periodic` | `pub fn spawn_periodic<F, Fut>(name: &'static str, period: std::time::Duration, task: F) where F: Fn() -> Fut + Send + Sync + 'static, Fut: std::future::Future<Output = anyhow::Result<()>> + Send + 'static` — registers a named, respawning, backed-off timer task in `sauron-api` and returns nothing (it must never be `?`-ed out of `main`) | S0 |
| `Config::mail_drain_tick_secs` | `pub mail_drain_tick_secs: u64`, from `MAIL_DRAIN_TICK_SECS`, clamped to `10..=3600`. Task 10 pins it high in the spawned child, because the drain blanks `body_text` the moment it marks a row `sent`/`sink` | S0 |
| `repo::revoke_sessions_for_user` | `pub async fn revoke_sessions_for_user(conn: &mut AsyncPgConnection, user_id: Uuid, except: Option<Uuid>, reason: &str, actor: Option<Uuid>) -> QueryResult<Vec<Uuid>>` | S2 |
| `repo::DELIBERATE_REVOKE_REASONS` | `pub const DELIBERATE_REVOKE_REASONS: [&str; 3]` today; S1 takes it to `[&str; 5]` | S2 |
| `AppState.revocations` | field exposing `fn mark_revoked(&self, ids: &[Uuid])` | S2 |
| `guard_member_admin_action` | `async fn guard_member_admin_action(conn: &mut PgConn, caller_id: Uuid, org_id: Uuid, target_user_id: Uuid, allow_self: bool) -> Result<Vec<(String, Uuid, Value)>, ApiError>` in `routes/orgs.rs` | S2 |
| `perm::MEMBER_CREDENTIAL` | `"member:credential"`, in the Owner and Admin presets | S2 |
| `routes::auth::rate_limit` / `client_addr` | `pub(crate)` | S2 |
| `MembersTable.svelte` sign-out props | `canRevokeSessions: boolean`, `revokingUserId: string \| null`, `onrevokesessions: (member: Member) => void`, rendering a `<Button size="sm" variant="ghost">Sign out</Button>` inside `.row-actions`. Task 12 folds that button into the new menu and keeps all three prop names verbatim | S2 |

If a name differs from the table, the foundation landed under another name — reconcile the call site to the real one before writing the step. Do **not** build a second copy of any row in that table.

---

## Global Constraints

- **Never** run `git commit`, `git add`, or create a branch. The repository owner commits manually.
- **Never** call `conn.transaction(...)`. The workspace MSRV is 1.82 (`packaging/rpm/sauron.spec`) and that helper needs async closures from 1.85. Multi-statement atomicity is one data-modifying CTE via `diesel::sql_query` with `.bind()`.
- **Never** run the diesel CLI. `backend/crates/sauron-db/src/schema.rs` is hand-maintained; the CLI rewrites every `table!` block including the partitioned and hand-tuned ones, and the result still compiles.
- A new table means three hand edits to `schema.rs`: a `diesel::table!` block, a `diesel::joinable!` line, and the name inside `allow_tables_to_appear_in_same_query!`.
- Migrations live at `backend/migrations/YYYY-MM-DD-0000NN_slug/{up,down}.sql`. **Both** files are required. `up.sql` opens with a prose comment explaining WHY. A migration runs in ONE transaction; `CONCURRENTLY` is unavailable.
- Enum-like columns are `TEXT` + `CHECK`, never a custom SQL type.
- All SQL lives in `backend/crates/sauron-db/src/repo.rs` as free `pub async fn name(conn: &mut AsyncPgConnection, ...) -> QueryResult<T>`. Handlers never build queries inline.
- Insertable-only structs must **not** derive `Queryable`. `Queryable` decodes positionally and would silently bind fields to the wrong columns.
- Never hold a pooled `PgConn` across network I/O or across a call that checks out a second connection. The API pool is **16 connections for the whole process**. `drop(conn)` first.
- Dashboard: house UI components only. There is NO `Select`, `Toggle`, `Tabs` or `Menu` primitive in `dashboard/src/lib/components/ui/`.
- Pure decision logic goes in `dashboard/src/lib/models/*.ts` with a colocated `*.test.ts`. There is NO DOM test environment.
- Svelte 5 runes. `$state` deep-proxies values so `===` never matches a raw value; use `$state.raw` when identity matters. Sets and Records in `$state` are replaced, never mutated in place.
- Comments explain the failure mode that motivated the code, not what the code does.
- `cargo clippy --all-targets` runs with `-D warnings` and `cargo fmt --all --check` is a hard gate.
- **Migration number is 000036** and the directory date prefix is the **landing** date. Diesel orders by the full `YYYY-MM-DD-0000NN` string, date first. Last on disk today is `2026-07-30-000033_env_per_project`; S0 takes 000034 and S2 takes 000035.
- Both TTLs are compile-time constants, never env vars: `SELF_RESET_TTL_SECS = 3_600`, `ADMIN_RESET_TTL_SECS = 86_400`.
- Every dead-token state answers identically: `401`, code `invalid_token`, message "invalid or expired token".
- `forgot-password` answers `200 {"ok": true}` for **every** input that parses — unknown address, deactivated account, happy path, and a deployment with no SMTP.
- Per-IP limiter windows are **60 seconds**, never an hour. Behind the shipped nginx the per-IP bucket is the entire deployment.

**Shell setup for every Rust command in this plan:**
```
export DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu
```
Each step below still spells the variable out inline so the command can be pasted alone.

---

## File Structure

**Created**

| Path | Responsibility |
|---|---|
| `backend/migrations/2026-08-01-000036_password_reset/up.sql` | `password_reset_tokens` table + two indexes + `users.credentials_invalidated_at` |
| `backend/migrations/2026-08-01-000036_password_reset/down.sql` | Drops both |
| `backend/bins/sauron-api/tests/http_password_reset.rs` | Integration suite against a spawned real binary and an ephemeral migrated DB |
| `dashboard/src/lib/models/password-reset.ts` | Pure decision logic: token parsing, password rules, the 403 predicate, the two member-action predicates |
| `dashboard/src/lib/models/password-reset.test.ts` | vitest for the above |
| `dashboard/src/lib/components/ui/RowActionsMenu.svelte` | The kebab overflow menu primitive — none exists in `ui/` |
| `dashboard/src/lib/components/members/ResetPasswordDialog.svelte` | Two-state danger confirm (force reset / cancel reset) |
| `dashboard/src/pages/ForgotPassword.svelte` | Public page: request a link, always show the same panel |
| `dashboard/src/pages/ResetPassword.svelte` | Public page: consume a link, then sign out locally and go to `#/login` |

**Modified**

| Path | Change |
|---|---|
| `backend/crates/sauron-db/src/schema.rs` | +1 `table!` block, +1 `joinable!`, +1 allow-list entry, +1 column on `users` |
| `backend/crates/sauron-db/src/models.rs` | `PasswordResetToken`, `NewPasswordResetToken`, `User.credentials_invalidated_at` |
| `backend/crates/sauron-db/src/repo.rs` | 9 new fns, 4 new consts, `DELIBERATE_REVOKE_REASONS` → 5, one new clause on `set_user_password`, `list_org_grants` select tuple |
| `backend/bins/sauron-api/src/error.rs` | `ApiError::Unavailable(String)` → 503 `unavailable` |
| `backend/crates/sauron-auth/src/extractors.rs` | `AuthError::PasswordResetRequired` → 403 `password_reset_required`, plus two tests |
| `backend/bins/sauron-api/src/routes/auth.rs` | 6 limiter consts, 2 TTL consts, `ResetMode`, 4 render helpers, `forgot_password`, `reset_password`, the `login` refusal |
| `backend/bins/sauron-api/src/routes/orgs.rs` | `reset_member_password`, `credentials_invalidated_at` on `MemberGrant` |
| `backend/bins/sauron-api/src/main.rs` | 3 route registrations |
| `backend/bins/sauron-api/src/tasks.rs` | Hourly `prune_password_reset_tokens` |
| `dashboard/src/lib/models/index.ts` | `credentials_invalidated_at` on `MemberGrant` + `Member`, `groupMembers` carries it, `MemberPasswordResetResult` |
| `dashboard/src/lib/api/auth.ts` | `forgotPassword`, `resetPassword` (both via `bareClient`) |
| `dashboard/src/lib/api/orgs.ts` | `resetMemberPassword` (via `api`) |
| `dashboard/src/lib/components/members/MembersTable.svelte` | Row actions become one `RowActionsMenu`; new `onresetpassword` / `currentUserId` / `canCredential` props; pending badge |
| `dashboard/src/pages/Members.svelte` | `resetTarget` state, dialog, wiring |
| `dashboard/src/pages/Login.svelte` | "Forgot your password?" footer link + the `password_reset_required` panel |
| `dashboard/src/routes.ts` | Two **bare** route entries |
| `dashboard/src/App.svelte` | `/forgot-password` joins `PUBLIC_ROUTES`; `/reset-password` deliberately does not |
| `wiki/Dashboard.md` | "Forgot your password" subsection + members note |
| `packaging/rpm/SETUP.md` | One row in §11 Upgrading |
| `README.md` | The `API_TRUST_FORWARDED_HEADERS` row gains the reset limiters and the inverse hazard |

---

### Task 1: Migration 000036, schema.rs and models.rs

**Files:**
- Create `backend/migrations/2026-08-01-000036_password_reset/up.sql`
- Create `backend/migrations/2026-08-01-000036_password_reset/down.sql`
- Modify `backend/crates/sauron-db/src/schema.rs` (new block after the `refresh_tokens` block at 212-223; `joinable!` list at ~486; `allow_tables_to_appear_in_same_query!` at ~503-533; `users` block at 249-261)
- Modify `backend/crates/sauron-db/src/models.rs` (`User` at 74-85; new structs after `NewRefreshToken` at 515-522)

**Interfaces:**
- Consumes: nothing.
- Produces: table `password_reset_tokens`; column `users.credentials_invalidated_at`; `sauron_db::schema::password_reset_tokens`; `sauron_db::models::PasswordResetToken`; `sauron_db::models::NewPasswordResetToken { user_id: Uuid, token_hash: String, password_fingerprint: String, mode: String, initiated_by: Option<Uuid>, requested_from: Option<String>, expires_at: DateTime<Utc> }`; `User.credentials_invalidated_at: Option<DateTime<Utc>>`.

- [ ] **Step 1: Write `up.sql`.** Create `backend/migrations/2026-08-01-000036_password_reset/up.sql`:
```sql
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
--   password_fingerprint  hash_token(users.password_hash) at issue time — a
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
-- performs an UPDATE, and the CHECK would re-validate and fail — so deleting an
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
```

- [ ] **Step 2: Write `down.sql`.** Create `backend/migrations/2026-08-01-000036_password_reset/down.sql`:
```sql
-- The indexes and the UNIQUE constraint go with the table.
DROP TABLE IF EXISTS password_reset_tokens;
ALTER TABLE users DROP COLUMN IF EXISTS credentials_invalidated_at;
```

- [ ] **Step 3: Apply the migration and see it succeed.** Run:
```
cd /home/splimter/projects/freelance/sauron/backend && DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo run --bin sauron-migrate
```
Expect it to report applying `2026-08-01-000036_password_reset` and exit 0. Confirm the column landed:
```
psql postgres://sauron:sauron@172.20.0.2:5432/sauron -c "\d password_reset_tokens" -c "\d users"
```
Expect `password_reset_tokens` to list twelve columns and `users` to end with `credentials_invalidated_at | timestamp with time zone`.

- [ ] **Step 4: Add the `table!` block.** In `backend/crates/sauron-db/src/schema.rs`, immediately after the `refresh_tokens` block (which ends at line 223 with `}`), insert:
```rust
diesel::table! {
    password_reset_tokens (id) {
        id -> Uuid,
        user_id -> Uuid,
        token_hash -> Text,
        password_fingerprint -> Text,
        mode -> Text,
        initiated_by -> Nullable<Uuid>,
        requested_from -> Nullable<Text>,
        expires_at -> Timestamptz,
        consumed_at -> Nullable<Timestamptz>,
        invalidated_at -> Nullable<Timestamptz>,
        invalidated_reason -> Nullable<Text>,
        created_at -> Timestamptz,
    }
}
```

- [ ] **Step 5: Add the column to the `users` block.** In the same file, in the `users (id)` block (lines 249-261), add `credentials_invalidated_at -> Nullable<Timestamptz>,` as the **last** entry, after `must_change_password -> Bool,`. Order is load-bearing: diesel matches `Queryable` positionally, so a field inserted anywhere but the end of both the `table!` block and the struct silently binds `name` to `email` and still compiles.

- [ ] **Step 6: Add the `joinable!` line and the allow-list entry.** In the same file, beside `diesel::joinable!(refresh_tokens -> users (user_id));`, add:
```rust
// Only the user_id FK. `password_reset_tokens` has two FKs to `users` and
// `joinable!` accepts one per table pair, so a future query for the initiating
// admin's email needs an explicit `.on(...)` rather than a second line here.
diesel::joinable!(password_reset_tokens -> users (user_id));
```
Then add `password_reset_tokens,` to `allow_tables_to_appear_in_same_query!`, immediately after the `refresh_tokens,` line. S1's delta to this file is **+1** `table!` block and **+1** allow-list entry — never assert a total, because several slices in this programme add blocks to the same file.

- [ ] **Step 7: Add the models.** In `backend/crates/sauron-db/src/models.rs`, immediately after `NewRefreshToken` (ends line 522), insert:
```rust
/// A password-reset link.
///
/// Deliberately derives no `Serialize`, exactly like [`RefreshToken`]:
/// `token_hash` and `password_fingerprint` must never leave the process, and no
/// endpoint returns this row.
#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = password_reset_tokens)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct PasswordResetToken {
    pub id: Uuid,
    pub user_id: Uuid,
    pub token_hash: String,
    pub password_fingerprint: String,
    /// `"self"` or `"admin"` — see the CHECK in migration 000036.
    pub mode: String,
    pub initiated_by: Option<Uuid>,
    pub requested_from: Option<String>,
    pub expires_at: DateTime<Utc>,
    pub consumed_at: Option<DateTime<Utc>>,
    pub invalidated_at: Option<DateTime<Utc>>,
    pub invalidated_reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Insert-only. Must never gain `Queryable`: that derive decodes positionally,
/// so a struct whose field order differs from the `table!` block would bind
/// `mode` to `password_fingerprint` and still compile.
#[derive(Debug, Insertable)]
#[diesel(table_name = password_reset_tokens)]
pub struct NewPasswordResetToken {
    pub user_id: Uuid,
    pub token_hash: String,
    pub password_fingerprint: String,
    pub mode: String,
    pub initiated_by: Option<Uuid>,
    pub requested_from: Option<String>,
    pub expires_at: DateTime<Utc>,
}
```

- [ ] **Step 8: Add the `User` field.** In the same file, in `struct User` (lines 74-85), add as the **last** field, after `pub must_change_password: bool,`:
```rust
    /// Set when an admin forced a password reset and the replacement has not
    /// been chosen yet; `login` refuses on it *after* the Argon2 verification.
    ///
    /// `#[serde(skip_serializing)]` because `User` is returned by `/v1/me` and
    /// inside `AuthResponse`, and a caller holding either has by definition just
    /// authenticated — so the field could only ever be null there. A
    /// permanently-null key in the public user object is noise someone will
    /// eventually build a client behaviour on.
    #[serde(skip_serializing)]
    pub credentials_invalidated_at: Option<DateTime<Utc>>,
```

- [ ] **Step 9: Compile the workspace and see it pass.** Run:
```
cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo check --workspace --all-targets
```
Expect `Finished`. If it reports `no field \`credentials_invalidated_at\`` the `table!` block edit in Step 5 was missed; if it reports a column-count mismatch on `User`, the field is not last in both places.

- [ ] **Step 10: Format and lint.** Run:
```
cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings
```
Expect both to exit 0.

---

### Task 2: Repo functions and revocation-reason constants

**Files:**
- Modify `backend/crates/sauron-db/src/repo.rs` (`set_user_password` at 155-168; the `REVOKE_*` block at ~203-215; new section after `revoke_all_refresh_tokens_for_user_with_reason` at ~318)

**Interfaces:**
- Consumes: `PasswordResetToken`, `NewPasswordResetToken`, `password_reset_tokens` (Task 1); `mail_outbox` (S0); `DELIBERATE_REVOKE_REASONS` (S2).
- Produces:
  - `pub const REVOKE_PASSWORD_RESET: &str = "password_reset"`
  - `pub const REVOKE_RESET_FORCED: &str = "reset_forced"`
  - `pub const RESET_INVALIDATED_SUPERSEDED: &str = "superseded"`
  - `pub const RESET_INVALIDATED_PASSWORD_SET: &str = "password_set"`
  - `insert_password_reset_token(conn, user_id: Uuid, token_hash: String, password_fingerprint: String, expires_at: DateTime<Utc>, mode: &str, initiated_by: Option<Uuid>, requested_from: Option<String>) -> QueryResult<PasswordResetToken>`
  - `find_live_password_reset_token(conn, token_hash: &str) -> QueryResult<Option<PasswordResetToken>>`
  - `consume_password_reset_token(conn, token_hash: &str) -> QueryResult<Option<(Uuid, String, String)>>`
  - `invalidate_password_reset_tokens_for_user(conn, user_id: Uuid, reason: &str) -> QueryResult<usize>`
  - `prune_password_reset_tokens(conn, older_than_days: i64) -> QueryResult<usize>`
  - `set_user_must_change_password(conn, user_id: Uuid, must_change: bool) -> QueryResult<usize>`
  - `set_user_credentials_invalidated(conn, user_id: Uuid, at: Option<DateTime<Utc>>) -> QueryResult<usize>`
  - `set_user_password_if_hash_matches(conn, user_id: Uuid, expected_hash: &str, new_hash: &str) -> QueryResult<usize>`
  - `password_reset_preflight(conn) -> QueryResult<()>`

- [ ] **Step 1: Write the failing constants test.** Append to `backend/crates/sauron-db/src/repo.rs` (if the file already ends with a `#[cfg(test)] mod tests { ... }` block, add these two functions inside it; otherwise append the whole block):
```rust
#[cfg(test)]
mod password_reset_reason_tests {
    use super::*;

    #[test]
    fn reset_reasons_have_their_wire_values() {
        // These four strings are written into `auth_sessions.revoked_reason`
        // (CHECK-constrained by migration 000035) and into
        // `password_reset_tokens.invalidated_reason`. A rename is a schema
        // change, not a refactor.
        assert_eq!(REVOKE_PASSWORD_RESET, "password_reset");
        assert_eq!(REVOKE_RESET_FORCED, "reset_forced");
        assert_eq!(RESET_INVALIDATED_SUPERSEDED, "superseded");
        assert_eq!(RESET_INVALIDATED_PASSWORD_SET, "password_set");
    }

    #[test]
    fn both_reset_revoke_reasons_are_deliberate() {
        // Missing from this list, the target's still-live refresh token lands
        // in `refresh`'s reuse branch and fires a family kill — the exact
        // poisoning bug routes/auth.rs:388-397 records as having happened once
        // already with routine deactivations.
        assert!(DELIBERATE_REVOKE_REASONS.contains(&REVOKE_PASSWORD_RESET));
        assert!(DELIBERATE_REVOKE_REASONS.contains(&REVOKE_RESET_FORCED));
    }
}
```

- [ ] **Step 2: Run it and see it fail.** Run:
```
cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-db password_reset_reason_tests
```
Expect a compile error: `cannot find value \`REVOKE_PASSWORD_RESET\` in this scope`.

- [ ] **Step 3: Add the four constants and extend the deliberate list.** In `backend/crates/sauron-db/src/repo.rs`, immediately after `pub const REVOKE_PASSWORD_CHANGED: &str = "password_changed";`, insert:
```rust
/// Sessions killed because a reset link was consumed. A deliberate act by the
/// person who proved control of the mailbox, so it is never a theft signal.
pub const REVOKE_PASSWORD_RESET: &str = "password_reset";
/// Sessions killed because an admin forced a password reset on the account.
pub const REVOKE_RESET_FORCED: &str = "reset_forced";
/// `password_reset_tokens.invalidated_reason` when a newer admin-initiated
/// reset replaced this link.
pub const RESET_INVALIDATED_SUPERSEDED: &str = "superseded";
/// `password_reset_tokens.invalidated_reason` when the account's password was
/// set, which kills every sibling link.
pub const RESET_INVALIDATED_PASSWORD_SET: &str = "password_set";
```
Then add both revoke reasons to `DELIBERATE_REVOKE_REASONS`, taking the array from `[&str; 3]` to `[&str; 5]` — update the declared length in the same edit:
```rust
pub const DELIBERATE_REVOKE_REASONS: [&str; 5] = [
    REVOKE_LOGOUT_ALL_OTHERS,
    REVOKE_ADMIN_SIGNOUT,
    REVOKE_DEACTIVATED,
    REVOKE_PASSWORD_RESET,
    REVOKE_RESET_FORCED,
];
```
(Keep whatever three names S2 put there; only append the two new ones and bump the length.) No migration accompanies this: S2 already seeded both strings into `auth_sessions_revoked_reason_check` while the table was still empty.

- [ ] **Step 4: Run the test and see it pass.** Run:
```
cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-db password_reset_reason_tests
```
Expect `test result: ok. 2 passed`.

- [ ] **Step 5: Add the one missing clause to `set_user_password`.** In `backend/crates/sauron-db/src/repo.rs`, change `set_user_password` (lines 155-168) so its doc comment and `.set(...)` read:
```rust
/// Set a new password and clear the forced-change flag. Always clears it: the
/// only way to reach this is the self-service change endpoint, where the user
/// chose the password themselves.
///
/// It also clears `credentials_invalidated_at`, so the invariant is "any
/// successful password write clears the invalidation" rather than "the writes
/// somebody happened to think of clear it". A future third writer inherits the
/// rule; the failure it prevents is an account locked out by a column nothing
/// left will ever reset.
pub async fn set_user_password(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    password_hash: &str,
) -> QueryResult<usize> {
    diesel::update(users::table.find(user_id))
        .set((
            users::password_hash.eq(password_hash),
            users::must_change_password.eq(false),
            users::credentials_invalidated_at.eq(None::<DateTime<Utc>>),
            users::updated_at.eq(Utc::now()),
        ))
        .execute(conn)
        .await
}
```

- [ ] **Step 6: Add the token functions.** In `backend/crates/sauron-db/src/repo.rs`, immediately after `revoke_all_refresh_tokens_for_user_with_reason` (ends ~line 318, just before the `// Organizations` banner), insert:
```rust
// ===========================================================================
// Password reset tokens
// ===========================================================================

#[allow(clippy::too_many_arguments)]
pub async fn insert_password_reset_token(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    token_hash: String,
    password_fingerprint: String,
    expires_at: DateTime<Utc>,
    mode: &str,
    initiated_by: Option<Uuid>,
    requested_from: Option<String>,
) -> QueryResult<PasswordResetToken> {
    diesel::insert_into(password_reset_tokens::table)
        .values(NewPasswordResetToken {
            user_id,
            token_hash,
            password_fingerprint,
            mode: mode.to_string(),
            initiated_by,
            requested_from,
            expires_at,
        })
        .returning(PasswordResetToken::as_returning())
        .get_result(conn)
        .await
}

/// The cheap pre-check, before any Argon2 work. Sibling of
/// [`find_active_refresh_token`].
pub async fn find_live_password_reset_token(
    conn: &mut AsyncPgConnection,
    token_hash: &str,
) -> QueryResult<Option<PasswordResetToken>> {
    password_reset_tokens::table
        .filter(password_reset_tokens::token_hash.eq(token_hash))
        .filter(password_reset_tokens::consumed_at.is_null())
        .filter(password_reset_tokens::invalidated_at.is_null())
        .filter(password_reset_tokens::expires_at.gt(Utc::now()))
        .select(PasswordResetToken::as_select())
        .first(conn)
        .await
        .optional()
}

#[derive(QueryableByName)]
struct ConsumedResetRow {
    #[diesel(sql_type = SqlUuid)]
    user_id: Uuid,
    #[diesel(sql_type = Text)]
    password_fingerprint: String,
    #[diesel(sql_type = Text)]
    mode: String,
}

/// Burn a reset link, atomically. Returns `(user_id, password_fingerprint, mode)`.
///
/// Zero rows means somebody else burned it first. This has to be one
/// `UPDATE … RETURNING` rather than a SELECT then an UPDATE: single-use is the
/// whole security property, and `conn.transaction` is unavailable (async
/// closures need Rust 1.85, workspace MSRV is 1.82).
pub async fn consume_password_reset_token(
    conn: &mut AsyncPgConnection,
    token_hash: &str,
) -> QueryResult<Option<(Uuid, String, String)>> {
    let row: Option<ConsumedResetRow> = diesel::sql_query(
        "UPDATE password_reset_tokens SET consumed_at = now() \
         WHERE token_hash = $1 AND consumed_at IS NULL AND invalidated_at IS NULL \
           AND expires_at > now() \
         RETURNING user_id, password_fingerprint, mode",
    )
    .bind::<Text, _>(token_hash)
    .get_result(conn)
    .await
    .optional()?;
    Ok(row.map(|r| (r.user_id, r.password_fingerprint, r.mode)))
}

/// Kill every outstanding link for a user. Sibling of
/// [`revoke_all_refresh_tokens_for_user_with_reason`].
pub async fn invalidate_password_reset_tokens_for_user(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    reason: &str,
) -> QueryResult<usize> {
    diesel::update(
        password_reset_tokens::table
            .filter(password_reset_tokens::user_id.eq(user_id))
            .filter(password_reset_tokens::consumed_at.is_null())
            .filter(password_reset_tokens::invalidated_at.is_null()),
    )
    .set((
        password_reset_tokens::invalidated_at.eq(Utc::now()),
        password_reset_tokens::invalidated_reason.eq(reason),
    ))
    .execute(conn)
    .await
}

/// Deletes by `created_at`, not `expires_at`, so a consumed token's audit trace
/// survives a fixed window regardless of its TTL. This table is the only record
/// that an admin forced a reset on someone — there is no `audit_events` table.
pub async fn prune_password_reset_tokens(
    conn: &mut AsyncPgConnection,
    older_than_days: i64,
) -> QueryResult<usize> {
    diesel::sql_query(
        "DELETE FROM password_reset_tokens WHERE created_at < now() - ($1 || ' days')::interval",
    )
    .bind::<Text, _>(older_than_days.to_string())
    .execute(conn)
    .await
}

/// Set the forced-change flag and nothing else.
///
/// Deliberately not routed through [`set_user_password`]: that one clears
/// `must_change_password`, which would un-gate the very account an admin reset
/// is trying to gate. The mistake is invisible in review, hence this comment.
pub async fn set_user_must_change_password(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    must_change: bool,
) -> QueryResult<usize> {
    diesel::update(users::table.find(user_id))
        .set((
            users::must_change_password.eq(must_change),
            users::updated_at.eq(Utc::now()),
        ))
        .execute(conn)
        .await
}

/// One function for both directions: `Some(now)` locks the credential, `None`
/// is the admin's cancel. Two functions would let one of them be added without
/// the other, and the one that would go missing is the undo.
pub async fn set_user_credentials_invalidated(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    at: Option<DateTime<Utc>>,
) -> QueryResult<usize> {
    diesel::update(users::table.find(user_id))
        .set((
            users::credentials_invalidated_at.eq(at),
            users::updated_at.eq(Utc::now()),
        ))
        .execute(conn)
        .await
}

/// Compare-and-swap password write. Zero rows means the password moved under us.
///
/// The reset handler reads `users.password_hash` to check the link's
/// fingerprint and writes ~100-200 ms later, with two Argon2 operations in
/// between. A legitimate user changing their password via `/v1/auth/password`
/// inside that window would otherwise have it silently clobbered by a stale
/// link — precisely what `password_fingerprint` exists to prevent. Guarding the
/// write itself moves the guarantee to the commit point rather than to a read.
///
/// The same statement clears `must_change_password` and
/// `credentials_invalidated_at`, so an account an admin locked is unlocked by
/// the write that satisfies the demand. A follow-up UPDATE would leave a window
/// in which the new password is live and `login` still refuses it.
pub async fn set_user_password_if_hash_matches(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    expected_hash: &str,
    new_hash: &str,
) -> QueryResult<usize> {
    diesel::update(
        users::table
            .filter(users::id.eq(user_id))
            .filter(users::password_hash.eq(expected_hash)),
    )
    .set((
        users::password_hash.eq(new_hash),
        users::must_change_password.eq(false),
        users::credentials_invalidated_at.eq(None::<DateTime<Utc>>),
        users::updated_at.eq(Utc::now()),
    ))
    .execute(conn)
    .await
}

/// Two zero-row probes, run before `forgot-password` branches on whether the
/// account exists.
///
/// Without it that endpoint is a *perfect* enumeration oracle on any deployment
/// that upgraded without running `sauron-migrate` — the RPM never re-runs it.
/// Unknown address: one SELECT against `users`, 200. Known address: an INSERT
/// against a table that does not exist, 500. This moves the failure ahead of
/// the branch so it is uniform, and turns a cheerful "we have sent a link"
/// forever into a loud 500 that pages someone.
pub async fn password_reset_preflight(conn: &mut AsyncPgConnection) -> QueryResult<()> {
    password_reset_tokens::table
        .select(password_reset_tokens::id)
        .limit(0)
        .load::<Uuid>(conn)
        .await?;
    mail_outbox::table
        .select(mail_outbox::id)
        .limit(0)
        .load::<Uuid>(conn)
        .await?;
    Ok(())
}
```

- [ ] **Step 7: Compile and see it pass.** Run:
```
cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo check --workspace --all-targets
```
Expect `Finished`. If it reports `cannot find derive macro \`QueryableByName\``, add it to the `use diesel::prelude::*;`-adjacent imports — it is re-exported by the prelude, so the more likely error is a missing `SqlUuid`/`Text` import, both of which are already in the file's `use diesel::sql_types::{...}` list at line 7-9.

- [ ] **Step 8: Add `credentials_invalidated_at` to `list_org_grants`.** In `backend/crates/sauron-db/src/repo.rs`, change `list_org_grants` (the fn whose doc comment reads "All grants in an org with the user email/name/active-status and role name, for the members page") to select and return one more column:
```rust
/// All grants in an org with the user email/name/active-status, role name, and
/// pending-reset marker, for the members page.
///
/// `credentials_invalidated_at` is visible to `member:read`, a wider audience
/// than `member:credential`. Acceptable: `is_active` is equally an
/// account-state disclosure and already ships in the same row, and without this
/// column the cancel action exists on the server and is unreachable from the UI.
pub async fn list_org_grants(
    conn: &mut AsyncPgConnection,
    org_id: Uuid,
) -> QueryResult<Vec<(RoleGrant, String, String, String, bool, Option<DateTime<Utc>>)>> {
    role_grants::table
        .inner_join(users::table.on(users::id.eq(role_grants::user_id)))
        .inner_join(roles::table.on(roles::id.eq(role_grants::role_id)))
        .filter(role_grants::org_id.eq(org_id))
        .select((
            RoleGrant::as_select(),
            users::email,
            users::name,
            roles::name,
            users::is_active,
            users::credentials_invalidated_at,
        ))
        .order(role_grants::created_at.asc())
        .load(conn)
        .await
}
```

- [ ] **Step 9: Compile and see the one expected caller break.** Run:
```
cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo check --workspace --all-targets
```
Expect exactly one error in `backend/bins/sauron-api/src/routes/orgs.rs` at the `list_members` closure: the pattern `|(g, email, name, role_name, is_active)|` no longer matches a 6-tuple. Task 8 fixes it; to keep this task green, change that closure's pattern now to `|(g, email, name, role_name, is_active, _credentials_invalidated_at)|` and re-run the command. Expect `Finished`.

- [ ] **Step 10: Format and lint.** Run:
```
cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings
```
Expect both to exit 0.

---

### Task 3: `ApiError::Unavailable` and `AuthError::PasswordResetRequired`

**Files:**
- Modify `backend/bins/sauron-api/src/error.rs` (enum at 11-23; match arms at 26-46)
- Modify `backend/crates/sauron-auth/src/extractors.rs` (enum at 43-62; `parts()` at 65-98; tests at 154-229)
- Test: `backend/crates/sauron-auth/src/extractors.rs` `mod tests`

**Interfaces:**
- Consumes: nothing.
- Produces: `ApiError::Unavailable(String)` → 503, code `unavailable`; `AuthError::PasswordResetRequired` → 403, code `password_reset_required`, message "an administrator reset this password — check your email for the link".

- [ ] **Step 1: Write the two failing tests.** In `backend/crates/sauron-auth/src/extractors.rs`, inside `mod tests`, add:
```rust
    #[test]
    fn password_reset_required_maps_to_403_with_its_own_code() {
        let (status, code, message) = AuthError::PasswordResetRequired.parts();
        assert_eq!(status, StatusCode::FORBIDDEN);
        // The dashboard's `isPasswordResetRequired` branches on this exact
        // string to swap the login form for the emailed-link panel. A rename
        // would otherwise only be caught by a human clicking through a
        // locked-out login.
        assert_eq!(code, "password_reset_required");
        assert_eq!(
            message,
            "an administrator reset this password — check your email for the link"
        );
        // Must not collide with the temp-password gate: the two names invite
        // exactly this confusion and the two panels say opposite things.
        assert_ne!(code, AuthError::PasswordChangeRequired.parts().1);
        assert_ne!(code, AuthError::AccountDeactivated.parts().1);
    }
```
And extend the existing `password_change_allowlist_is_exactly_two_paths` test's rejected list so it reads:
```rust
        // Everything a temp-password holder might otherwise reach.
        for p in [
            "/v1/orgs",
            "/v1/auth/refresh",
            "/v1/projects",
            "/v1/admin/storage",
            "/v1/auth/passwordx",
            // Deliberately unauthenticated, so `password_change_gate` is never
            // reached for either and neither must ever need the allowlist. A
            // future change that bolts an extractor onto one of them turns this
            // red instead of silently 403ing every reset for exactly the
            // population that needs one.
            "/v1/auth/forgot-password",
            "/v1/auth/reset-password",
        ] {
```

- [ ] **Step 2: Run them and see them fail.** Run:
```
cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-auth extractors
```
Expect a compile error: `no variant named \`PasswordResetRequired\` found for enum \`AuthError\``.

- [ ] **Step 3: Add the variant.** In `backend/crates/sauron-auth/src/extractors.rs`, in `enum AuthError`, immediately after the `PasswordChangeRequired` variant, add:
```rust
    /// The password was correct, but an admin invalidated this credential and
    /// the replacement has not been chosen yet. Only ever returned *after* a
    /// successful password verification — placing it before would answer in
    /// microseconds for a reset-pending account and in tens of milliseconds for
    /// every other one, handing back the enumeration oracle `spend_dummy_verify`
    /// was written to close, and leaking to anyone who can type an address that
    /// a particular person is mid-lockout.
    PasswordResetRequired,
```
And in `parts()`, after the `PasswordChangeRequired` arm:
```rust
            AuthError::PasswordResetRequired => (
                StatusCode::FORBIDDEN,
                "password_reset_required",
                "an administrator reset this password — check your email for the link",
            ),
```

- [ ] **Step 4: Run and see them pass.** Run:
```
cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-auth extractors
```
Expect `test result: ok.` with 8 tests passing.

- [ ] **Step 5: Add `ApiError::Unavailable`.** In `backend/bins/sauron-api/src/error.rs`, add to the enum after `RateLimited,`:
```rust
    /// A dependency the route needs is not configured on this deployment, and
    /// the route refuses **before applying anything**. The message carries the
    /// `require_*()` text so an operator learns which setting is missing.
    Unavailable(String),
```
And in `IntoResponse`, after the `RateLimited` arm:
```rust
            ApiError::Unavailable(m) => body(StatusCode::SERVICE_UNAVAILABLE, "unavailable", &m),
```

- [ ] **Step 6: Compile, format and lint.** Run:
```
cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo check --workspace --all-targets && cargo fmt --all && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings
```
Expect all three to exit 0.

---

### Task 4: Reset link, expiry wording, token shape and email rendering

**Files:**
- Modify `backend/bins/sauron-api/src/routes/auth.rs` (constants beside the block at 24-36; helpers and templates after `rate_limit` ends at ~158; new `#[cfg(test)] mod` at end of file)
- Test: `backend/bins/sauron-api/src/routes/auth.rs` `mod password_reset_render_tests`

**Interfaces:**
- Consumes: `sauron_mail::MailContent`, `sauron_mail::text::{substitute, html_escape}`, `sauron_mail::template::layout` (S0).
- Produces:
  - `pub(crate) const SELF_RESET_TTL_SECS: i64 = 3_600`
  - `pub(crate) const ADMIN_RESET_TTL_SECS: i64 = 86_400`
  - `pub(crate) const FORGOT_ATTEMPTS_PER_EMAIL_PER_HOUR: u32 = 3`
  - `pub(crate) const FORGOT_ATTEMPTS_PER_MIN_PER_IP: u32 = 60`
  - `pub(crate) const RESET_ATTEMPTS_PER_MIN_PER_IP: u32 = 60`
  - `pub(crate) const RESET_ATTEMPTS_PER_TOKEN_PER_HOUR: u32 = 10`
  - `pub(crate) const ADMIN_RESET_PER_CALLER_PER_HOUR: u32 = 20`
  - `pub(crate) const ADMIN_RESET_PER_TARGET_PER_HOUR: u32 = 5`
  - `pub(crate) enum ResetMode { SelfService, Admin }` with `fn as_str(self) -> &'static str` and `fn ttl_secs(self) -> i64`
  - `pub(crate) fn reset_link(dashboard_url: &str, raw_token: &str) -> String`
  - `pub(crate) fn expiry_wording(ttl_secs: i64) -> String`
  - `fn is_reset_token_shape(token: &str) -> bool`
  - `pub(crate) struct ResetMailVars<'a> { pub mode: ResetMode, pub display_name: &'a str, pub reset_url: &'a str, pub org_name: &'a str }`
  - `pub(crate) fn render_password_reset_mail(vars: ResetMailVars<'_>) -> sauron_mail::MailContent`

- [ ] **Step 1: Write the failing tests.** Append to `backend/bins/sauron-api/src/routes/auth.rs`:
```rust
#[cfg(test)]
mod password_reset_render_tests {
    use super::*;

    #[test]
    fn reset_link_puts_the_token_in_the_fragment() {
        // Browsers never send a fragment in a request line or a Referer, so the
        // token reaches no server log, proxy log or analytics beacon. A
        // pre-hash query string would not have that property.
        assert_eq!(
            reset_link("https://sauron.example.com", "abc123"),
            "https://sauron.example.com/#/reset-password?token=abc123"
        );
    }

    #[test]
    fn reset_link_tolerates_a_trailing_slash() {
        assert_eq!(
            reset_link("https://sauron.example.com/", "abc123"),
            "https://sauron.example.com/#/reset-password?token=abc123"
        );
    }

    #[test]
    fn expiry_wording_is_derived_from_the_constants() {
        // Derived, never hand-typed: changing a TTL must not leave the email
        // claiming the old number.
        assert_eq!(expiry_wording(SELF_RESET_TTL_SECS), "1 hour");
        assert_eq!(expiry_wording(ADMIN_RESET_TTL_SECS), "24 hours");
    }

    #[test]
    fn token_shape_accepts_a_real_token_and_rejects_near_misses() {
        let real = sauron_core::ids::opaque_token();
        assert!(is_reset_token_shape(&real));
        assert!(!is_reset_token_shape(&real[..63]));
        assert!(!is_reset_token_shape(&format!("{real}a")));
        assert!(!is_reset_token_shape(&"z".repeat(64)));
        assert!(!is_reset_token_shape(""));
    }

    #[test]
    fn self_mode_text_carries_the_raw_url_and_the_reassurance() {
        let out = render_password_reset_mail(ResetMailVars {
            mode: ResetMode::SelfService,
            display_name: "Ada",
            reset_url: "https://s.example/#/reset-password?token=deadbeef",
            org_name: "",
        });
        assert_eq!(out.subject, "Reset your Sauron password");
        assert!(out.text.contains("\nhttps://s.example/#/reset-password?token=deadbeef\n"));
        assert!(out.text.contains("expires in 1 hour"));
        assert!(out.text.contains("If this wasn't you, nothing has changed"));
    }

    #[test]
    fn admin_mode_names_the_org_and_omits_the_ignore_it_reassurance() {
        let out = render_password_reset_mail(ResetMailVars {
            mode: ResetMode::Admin,
            display_name: "Ada",
            reset_url: "https://s.example/#/reset-password?token=deadbeef",
            org_name: "Acme",
        });
        assert_eq!(out.subject, "Set a new Sauron password");
        assert!(out.text.contains("An administrator of Acme reset your password"));
        assert!(out.text.contains("expires in 24 hours"));
        // Ignoring it is not an option, and a recipient told otherwise will not
        // act until they next try to sign in.
        assert!(!out.text.contains("nothing has changed"));
    }

    #[test]
    fn html_escapes_variables_and_text_does_not() {
        let out = render_password_reset_mail(ResetMailVars {
            mode: ResetMode::SelfService,
            display_name: "<script>alert(1)</script>",
            reset_url: "https://s.example/#/reset-password?token=a&b=c",
            org_name: "",
        });
        assert!(out.text.contains("<script>alert(1)</script>"));
        assert!(!out.html.contains("<script>alert(1)</script>"));
        assert!(out.html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        // Every attribute in these templates is double-quoted, which is what
        // makes `html_escape` safe despite not escaping `'`.
        assert!(out
            .html
            .contains("href=\"https://s.example/#/reset-password?token=a&amp;b=c\""));
    }

    #[test]
    fn an_absent_variable_renders_blank_rather_than_echoing_its_placeholder() {
        // `render_password_reset_mail` always populates all four keys, so this
        // pins the S0 contract it *rests* on. If `substitute` echoed `{{key}}`
        // for an absent one, the first template to gain a fifth variable would
        // mail a literal `{{support_url}}` to a user and nothing else in this
        // file would have noticed.
        let vars: std::collections::BTreeMap<String, String> =
            std::collections::BTreeMap::from([("name".to_string(), "Ada".to_string())]);
        assert_eq!(
            sauron_mail::text::substitute("Hi {{name}}, org {{org_name}}.", &vars),
            "Hi Ada, org ."
        );

        // And the shipped self-service output carries no unsubstituted
        // placeholder of its own — the path that passes `org_name: ""`.
        let out = render_password_reset_mail(ResetMailVars {
            mode: ResetMode::SelfService,
            display_name: "Ada",
            reset_url: "https://s.example/#/reset-password?token=deadbeef",
            org_name: "",
        });
        assert!(!out.text.contains("{{"), "text: {}", out.text);
        assert!(!out.html.contains("{{"), "html: {}", out.html);
    }

    #[test]
    fn modes_carry_their_wire_values_and_ttls() {
        assert_eq!(ResetMode::SelfService.as_str(), "self");
        assert_eq!(ResetMode::Admin.as_str(), "admin");
        assert_eq!(ResetMode::SelfService.ttl_secs(), SELF_RESET_TTL_SECS);
        assert_eq!(ResetMode::Admin.ttl_secs(), ADMIN_RESET_TTL_SECS);
    }
}
```

- [ ] **Step 2: Run them and see them fail.** Run:
```
cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-api password_reset_render_tests
```
Expect a compile error: `cannot find function \`reset_link\` in this scope`.

- [ ] **Step 3: Add the eight constants.** In `backend/bins/sauron-api/src/routes/auth.rs`, immediately after `const REFRESH_RACE_GRACE: chrono::Duration = chrono::Duration::seconds(10);`, insert:
```rust
/// How long a self-service reset link lives.
///
/// A mailbox is a password-equivalent credential for exactly as long as the
/// link does, and one hour is the modern norm. Compile-time rather than
/// `Config`: two env knobs cost three files of documentation each, for values
/// nobody tunes.
pub(crate) const SELF_RESET_TTL_SECS: i64 = 3_600;
/// How long an admin-initiated reset link lives.
///
/// The account cannot be signed into at all until this link is used, so a
/// one-hour window would turn "read the mail after lunch" into a second round
/// trip through an administrator. A day is what makes the lockout survivable.
pub(crate) const ADMIN_RESET_TTL_SECS: i64 = 86_400;

/// Self-service reset requests per email address per hour.
///
/// Consumed before the user lookup, so three requests against a known address
/// deny that person self-service reset for an hour — the same property `login`
/// already has. Raising it was rejected: it turns forgot-password into a free
/// mail cannon aimed at any address the attacker names.
pub(crate) const FORGOT_ATTEMPTS_PER_EMAIL_PER_HOUR: u32 = 3;
/// Self-service reset requests per IP per **60 seconds** — a burst limiter, not
/// a lockout.
///
/// `API_TRUST_FORWARDED_HEADERS` defaults to false and the shipped nginx sits in
/// front, so `client_addr` returns the proxy's address and this bucket is the
/// **entire deployment**. An hour-long budget would let an anonymous attacker
/// burn it in about a second, after which every legitimate link-holder gets 429
/// for the remaining 59 minutes. Copy login's window, not just its arithmetic.
pub(crate) const FORGOT_ATTEMPTS_PER_MIN_PER_IP: u32 = 60;
/// Reset submissions per IP per 60 seconds. Same deployment-wide-bucket
/// reasoning as `FORGOT_ATTEMPTS_PER_MIN_PER_IP`, and worse here: this route
/// rejects a malformed token before any DB work, so the budget is cheap to burn.
pub(crate) const RESET_ATTEMPTS_PER_MIN_PER_IP: u32 = 60;
/// Reset submissions per **link** per hour — a key an attacker cannot share.
///
/// The reuse check returns 400 *without* consuming the token when the new
/// password equals the current one, so a link-holder could otherwise loop that
/// branch at 60/min, each iteration costing an Argon2 verify. Ten per hour is
/// generous for a human and useless as an amplifier.
pub(crate) const RESET_ATTEMPTS_PER_TOKEN_PER_HOUR: u32 = 10;
/// Admin-initiated resets per calling admin per hour. Bounds the fan-out.
pub(crate) const ADMIN_RESET_PER_CALLER_PER_HOUR: u32 = 20;
/// Admin-initiated resets per target per hour. The one that matters: an
/// unbounded loop is an unbounded mail bomb aimed at one member's inbox, and an
/// unbounded re-lock of an account somebody is trying to recover.
pub(crate) const ADMIN_RESET_PER_TARGET_PER_HOUR: u32 = 5;
```

- [ ] **Step 4: Add `ResetMode` and the three pure helpers.** In the same file, immediately after `rate_limit`'s closing brace (~line 158), insert:
```rust
/// Which copy a reset email uses, and how long its link lives.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResetMode {
    SelfService,
    Admin,
}

impl ResetMode {
    /// The value stored in `password_reset_tokens.mode`, which is TEXT + CHECK.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ResetMode::SelfService => "self",
            ResetMode::Admin => "admin",
        }
    }

    pub(crate) fn ttl_secs(self) -> i64 {
        match self {
            ResetMode::SelfService => SELF_RESET_TTL_SECS,
            ResetMode::Admin => ADMIN_RESET_TTL_SECS,
        }
    }
}

/// The URL that goes in the email.
///
/// The token sits inside the **hash fragment**. Browsers never put a fragment
/// in a request line or a `Referer`, so it reaches no server log, proxy log or
/// analytics beacon — a real property here, not just house convention.
pub(crate) fn reset_link(dashboard_url: &str, raw_token: &str) -> String {
    format!(
        "{}/#/reset-password?token={}",
        dashboard_url.trim_end_matches('/'),
        raw_token
    )
}

/// "1 hour" / "24 hours", derived from the TTL constants rather than typed by
/// hand, so changing a constant cannot leave the email claiming the old number.
pub(crate) fn expiry_wording(ttl_secs: i64) -> String {
    let hours = ttl_secs / 3_600;
    if hours == 1 {
        "1 hour".to_string()
    } else {
        format!("{hours} hours")
    }
}

/// `opaque_token()` is `random_hex(32)`, so a real token is 64 hex characters.
///
/// Rejecting garbage here keeps a spray off the database and, more importantly,
/// out of Redis: the per-token limiter would otherwise mint one key per guess.
fn is_reset_token_shape(token: &str) -> bool {
    token.len() == 64 && token.chars().all(|c| c.is_ascii_hexdigit())
}
```

- [ ] **Step 5: Add the four templates.** In the same file, directly below `is_reset_token_shape`, insert:
```rust
const RESET_TEXT_SELF: &str = "Hi {{name}},

Someone asked to reset the password for this address.

Open this link to choose a new one:
{{reset_url}}

The link expires in {{expiry}}.

If this wasn't you, nothing has changed and you can ignore this email.
";

const RESET_TEXT_ADMIN: &str = "Hi {{name}},

An administrator of {{org_name}} reset your password.

Open this link to choose a new one:
{{reset_url}}

The link expires in {{expiry}}.

Your old password no longer works and you have been signed out on all devices. This link is how you get back in — if it has expired, ask an administrator of {{org_name}} to send you another.
";

// Every attribute below is DOUBLE-quoted, and must stay that way.
// `sauron_mail::text::html_escape` escapes `&`, `<`, `>` and `"` but not `'`,
// so a single-quoted href='{{reset_url}}' would be injectable through an
// operator-controlled DASHBOARD_URL.
const RESET_HTML_SELF: &str = "<p>Hi {{name}},</p>\
<p>Someone asked to reset the password for this address.</p>\
<p><a href=\"{{reset_url}}\">Choose a new password</a></p>\
<p>Or paste this link into your browser:<br>{{reset_url}}</p>\
<p>The link expires in {{expiry}}.</p>\
<p>If this wasn't you, nothing has changed and you can ignore this email.</p>";

const RESET_HTML_ADMIN: &str = "<p>Hi {{name}},</p>\
<p>An administrator of {{org_name}} reset your password.</p>\
<p><a href=\"{{reset_url}}\">Set a new password</a></p>\
<p>Or paste this link into your browser:<br>{{reset_url}}</p>\
<p>The link expires in {{expiry}}.</p>\
<p>Your old password no longer works and you have been signed out on all \
devices. This link is how you get back in — if it has expired, ask an \
administrator of {{org_name}} to send you another.</p>";
```

- [ ] **Step 6: Add the renderer.** In the same file, directly below the templates, insert:
```rust
pub(crate) struct ResetMailVars<'a> {
    pub mode: ResetMode,
    /// The recipient's display name, already falling back to their email.
    pub display_name: &'a str,
    pub reset_url: &'a str,
    /// Only read in `ResetMode::Admin`. The acting admin is deliberately not
    /// named anywhere: the org is what a recipient needs to judge legitimacy,
    /// and naming an individual invites a reply to a person rather than a route
    /// back into the account.
    pub org_name: &'a str,
}

pub(crate) fn render_password_reset_mail(vars: ResetMailVars<'_>) -> sauron_mail::MailContent {
    let expiry = expiry_wording(vars.mode.ttl_secs());

    let mut text_vars: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    text_vars.insert("name".to_string(), vars.display_name.to_string());
    text_vars.insert("reset_url".to_string(), vars.reset_url.to_string());
    text_vars.insert("expiry".to_string(), expiry);
    text_vars.insert("org_name".to_string(), vars.org_name.to_string());

    // Same values, escaped once, for the HTML part only.
    let html_vars: std::collections::BTreeMap<String, String> = text_vars
        .iter()
        .map(|(k, v)| (k.clone(), sauron_mail::text::html_escape(v)))
        .collect();

    let (subject, text_tpl, html_tpl) = match vars.mode {
        ResetMode::SelfService => (
            "Reset your Sauron password",
            RESET_TEXT_SELF,
            RESET_HTML_SELF,
        ),
        ResetMode::Admin => (
            "Set a new Sauron password",
            RESET_TEXT_ADMIN,
            RESET_HTML_ADMIN,
        ),
    };

    let body_html = sauron_mail::text::substitute(html_tpl, &html_vars);
    sauron_mail::MailContent {
        subject: subject.to_string(),
        text: sauron_mail::text::substitute(text_tpl, &text_vars),
        html: sauron_mail::template::layout(subject, &body_html),
    }
}
```

- [ ] **Step 7: Run the tests and see them pass.** Run:
```
cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-api password_reset_render_tests
```
Expect `test result: ok. 9 passed`. If `html_escapes_variables_and_text_does_not` fails on the `href="…"` assertion, `sauron_mail::template::layout` is rewriting the body — check that S0's layout only wraps rather than re-escapes.

- [ ] **Step 8: Format and lint.** Run:
```
cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings
```
Expect both to exit 0. `is_reset_token_shape` is not yet called anywhere, so clippy may report `dead_code`; leave it and complete Task 6 in the same session, or temporarily add `#[allow(dead_code)]` and remove it in **Task 6 Step 5**, which is the lint step that names the attribute.

---

### Task 5: `POST /v1/auth/forgot-password`

**Files:**
- Modify `backend/bins/sauron-api/src/routes/auth.rs` (imports at 4-21; new handler after `logout`, ~line 458)
- Modify `backend/bins/sauron-api/src/main.rs` (route table, beside `/v1/auth/logout` at ~line 152)

**Interfaces:**
- Consumes: `repo::password_reset_preflight`, `repo::insert_password_reset_token`, `ResetMode`, `reset_link`, `render_password_reset_mail`, `ResetMailVars`, `FORGOT_ATTEMPTS_PER_EMAIL_PER_HOUR`, `FORGOT_ATTEMPTS_PER_MIN_PER_IP`, `SELF_RESET_TTL_SECS`, `rate_limit`, `client_addr`, `AppState.mail`, `Config::require_dashboard_url`, `MailSender::enqueue_or_discard`, `MailKind::PasswordReset`.
- Produces: `pub struct ForgotPasswordReq { pub email: String }`; `pub async fn forgot_password(...) -> Result<Json<serde_json::Value>, ApiError>`; route `POST /v1/auth/forgot-password`.

- [ ] **Step 1: Add the imports.** In `backend/bins/sauron-api/src/routes/auth.rs`, extend the import block so it includes `opaque_token` and the mail kind:
```rust
use sauron_core::ids::opaque_token;
use sauron_mail::MailKind;
```
Add `use uuid::Uuid;` if the file does not already have it.

- [ ] **Step 2: Add the request struct and the handler.** In the same file, immediately after `logout` (ends ~line 458), insert:
```rust
#[derive(Deserialize)]
pub struct ForgotPasswordReq {
    pub email: String,
}

/// Start a self-service password reset.
///
/// **Every input that parses gets a byte-identical `200 {"ok": true}`** —
/// unknown address, deactivated account, happy path, and a deployment with no
/// SMTP alike. This route is fully unauthenticated (no `AuthUser`, so it never
/// reaches the `must_change_password` gate), and any answer that varies with
/// account existence or with deployment configuration is an oracle handed to an
/// anonymous caller.
///
/// Constant time is achieved structurally rather than by padding: the handler
/// never touches a socket (S0's outbox owns the SMTP dial), the preflight puts
/// two round trips on both branches, and `enqueue_or_discard` deliberately
/// spends the render, the round trip and the drain nudge on the discard branch
/// too. What is left on the found branch is one local INSERT and two SHA-256
/// hashes — sub-millisecond against network jitter orders of magnitude larger.
/// If instrumentation ever shows a signal, the fix is a fixed-cost pad, not a
/// dummy INSERT, which would be visible in the table.
pub async fn forgot_password(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: axum::http::HeaderMap,
    Json(req): Json<ForgotPasswordReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // Shape only. Allowed to be a distinguishable answer because it does not
    // depend on whether an account exists.
    if req.email.len() > 320 || !req.email.contains('@') {
        return Err(ApiError::BadRequest("a valid email is required".into()));
    }

    let addr = client_addr(&headers, &peer, &state);

    // A 503 here would be a config-state oracle for an anonymous caller on the
    // one endpoint whose entire contract is that every input gets the same
    // answer. The operator learns from S0's startup WARN and from the admin
    // route's 503; the caller learns nothing.
    let (mail, dashboard_url) = match (state.mail.as_ref(), state.cfg.require_dashboard_url()) {
        (Some(m), Ok(url)) => (m.clone(), url.to_string()),
        _ => {
            tracing::error!(
                "forgot-password: SMTP or DASHBOARD_URL is unconfigured; answering 200 and \
                 sending nothing"
            );
            return Ok(Json(serde_json::json!({ "ok": true })));
        }
    };

    rate_limit(
        &state,
        &format!("sauron:auth:forgot:{}", req.email.to_lowercase()),
        FORGOT_ATTEMPTS_PER_EMAIL_PER_HOUR,
        3600,
    )
    .await?;
    rate_limit(
        &state,
        &format!("sauron:auth:forgot:ip:{addr}"),
        FORGOT_ATTEMPTS_PER_MIN_PER_IP,
        60,
    )
    .await?;

    let mut conn = db(&state).await?;
    repo::password_reset_preflight(&mut conn).await?;

    // Minted before the branch, so both branches have a URL to render and pay
    // the same cost. On the discard branch it corresponds to no row anywhere
    // and the message it lands in is never inserted.
    let raw = opaque_token();
    let reset_url = reset_link(&dashboard_url, &raw);

    let mut recipient: Option<String> = None;
    let mut display_name = String::new();
    let mut user_id: Option<Uuid> = None;

    // Past the preflight, an error from the lookup or the INSERT is logged and
    // still answered 200: both branches have already touched both tables, so a
    // failure is no longer correlated with account existence, and a 500 that
    // fired only on the account-exists branch would be exactly the oracle the
    // preflight just closed.
    match repo::find_user_by_email(&mut conn, &req.email).await {
        Ok(Some(user)) if user.is_active => {
            // Self-service deliberately does NOT invalidate the user's other
            // outstanding links: an attacker spamming forgot-password against a
            // known address would otherwise kill the link the victim is about
            // to click, turning the anti-abuse limiter into the abuse.
            let fingerprint = hash_token(&user.password_hash);
            match repo::insert_password_reset_token(
                &mut conn,
                user.id,
                hash_token(&raw),
                fingerprint,
                Utc::now() + chrono::Duration::seconds(SELF_RESET_TTL_SECS),
                ResetMode::SelfService.as_str(),
                None,
                Some(addr.clone()),
            )
            .await
            {
                Ok(_) => {
                    display_name = if user.name.trim().is_empty() {
                        user.email.clone()
                    } else {
                        user.name.clone()
                    };
                    user_id = Some(user.id);
                    recipient = Some(user.email);
                }
                Err(e) => {
                    tracing::error!(error = %e, "forgot-password: could not record the reset token")
                }
            }
        }
        Ok(Some(user)) => {
            // Mailing a deactivated account "your account is disabled" is an
            // information leak dressed as helpfulness, and they cannot log in
            // anyway.
            tracing::info!(user_id = %user.id, "forgot-password ignored for a deactivated account");
        }
        Ok(None) => tracing::debug!("forgot-password for an address with no account"),
        Err(e) => tracing::error!(error = %e, "forgot-password: user lookup failed"),
    }

    // `MailSender` owns a `PgPool` and checks out its OWN connection for the
    // enqueue INSERT. The API pool is 16 for the whole process with a 5s
    // checkout timeout, so a handler still holding one while enqueueing needs a
    // seventeenth to make progress: sixteen concurrent resets then hold all
    // sixteen, every one stalls for the full timeout, and every other endpoint
    // in the process 500s alongside them.
    drop(conn);

    let content = render_password_reset_mail(ResetMailVars {
        mode: ResetMode::SelfService,
        display_name: &display_name,
        reset_url: &reset_url,
        org_name: "",
    });

    // Rendered and enqueued on BOTH branches. `enqueue_or_discard` runs the
    // same statement against the same index and commits nothing when the
    // recipient is None. An `if let Some(user)` around this is what would
    // rebuild the enumeration oracle S0 went to the trouble of closing.
    if let Err(e) = mail
        .enqueue_or_discard(
            MailKind::PasswordReset,
            recipient.as_deref(),
            &content,
            user_id,
            Duration::from_secs(SELF_RESET_TTL_SECS as u64),
        )
        .await
    {
        tracing::error!(error = %e, "forgot-password: could not enqueue the reset email");
    }

    Ok(Json(serde_json::json!({ "ok": true })))
}
```

- [ ] **Step 3: Register the route.** In `backend/bins/sauron-api/src/main.rs`, in the flat route table, immediately after the `/v1/auth/logout` line, add:
```rust
        // Unauthenticated by design: the reset token travels in the body/URL
        // fragment, never as a bearer, so `password_change_gate` is never
        // reached and the extractor's allowlist stays exactly two paths.
        .route(
            "/v1/auth/forgot-password",
            post(routes::auth::forgot_password),
        )
```
No nested router and no prefix: `password_change_allowed_path` is an exact-path match that a prefix would silently break.

- [ ] **Step 4: Compile and see it pass.** Run:
```
cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo check --workspace --all-targets
```
Expect `Finished`. If it reports `no method named \`clone\` found for reference \`&MailSender\``, S0's `MailSender` is missing `#[derive(Clone)]` — add it there rather than restructuring this handler, because the `drop(conn)` before `enqueue` requires the sender to outlive the connection.

- [ ] **Step 5: Drive the route by hand and see the generic 200.** Start the API against the live database in one shell:
```
cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron REDIS_URL=redis://172.20.0.3:6379 SAURON_DEV=1 API_PORT=8099 SMTP_SINK=1 DASHBOARD_URL=http://localhost:5173 cargo run --bin sauron-api
```
In a second shell:
```
curl -s -i -X POST localhost:8099/v1/auth/forgot-password -H 'content-type: application/json' -d '{"email":"nobody-at-all@example.com"}'
curl -s -i -X POST localhost:8099/v1/auth/forgot-password -H 'content-type: application/json' -d '{"email":"not-an-email"}'
```
Expect the first to be `HTTP/1.1 200 OK` with body `{"ok":true}` and the second `HTTP/1.1 400 Bad Request` with `"a valid email is required"`. Stop the server.

- [ ] **Step 6: Format and lint.** Run:
```
cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings
```
Expect both to exit 0.

---

### Task 6: `POST /v1/auth/reset-password`

**Files:**
- Modify `backend/bins/sauron-api/src/routes/auth.rs` (new handler after `forgot_password`)
- Modify `backend/bins/sauron-api/src/main.rs` (route table, beside the line added in Task 5)

**Interfaces:**
- Consumes: `is_reset_token_shape`, `RESET_ATTEMPTS_PER_MIN_PER_IP`, `RESET_ATTEMPTS_PER_TOKEN_PER_HOUR`, `MAX_PASSWORD_LEN`, `repo::find_live_password_reset_token`, `repo::get_user`, `repo::consume_password_reset_token`, `repo::revoke_sessions_for_user`, `repo::REVOKE_PASSWORD_RESET`, `repo::set_user_password_if_hash_matches`, `repo::invalidate_password_reset_tokens_for_user`, `repo::RESET_INVALIDATED_PASSWORD_SET`, `state.revocations.mark_revoked`.
- Produces: `pub struct ResetPasswordReq { pub token: String, pub new_password: String }`; `pub async fn reset_password(...) -> Result<Json<serde_json::Value>, ApiError>`; route `POST /v1/auth/reset-password`.

- [ ] **Step 1: Add the handler.** In `backend/bins/sauron-api/src/routes/auth.rs`, immediately after `forgot_password`, insert:
```rust
#[derive(Deserialize)]
pub struct ResetPasswordReq {
    pub token: String,
    pub new_password: String,
}

/// Consume a reset link and set a new password.
///
/// Unauthenticated; the token travels in the body exactly like `logout`'s
/// refresh token. **Every dead-token state answers identically** — unknown,
/// consumed, invalidated, expired, stale fingerprint, and lost the
/// compare-and-swap all collapse to `401 invalid_token`. Distinct codes would
/// tell an attacker spraying the token space whether a guess ever corresponded
/// to a real link, and would leak one user's activity.
///
/// A successful reset deliberately does **not** log the caller in. They proved
/// control of a mailbox, not of a credential; auto-login would make a forwarded
/// or archived message session-equivalent, and it would contradict the step
/// immediately before it, which revoked every session the account had.
///
/// No preflight is needed here, unlike `forgot_password`: every input path
/// touches `password_reset_tokens` unconditionally, so an unmigrated schema
/// 500s uniformly.
pub async fn reset_password(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: axum::http::HeaderMap,
    Json(req): Json<ResetPasswordReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    rate_limit(
        &state,
        &format!(
            "sauron:auth:reset:ip:{}",
            client_addr(&headers, &peer, &state)
        ),
        RESET_ATTEMPTS_PER_MIN_PER_IP,
        60,
    )
    .await?;

    // Copied verbatim from `change_password` so the *length* half of password
    // policy keeps one definition and one set of user-visible strings.
    if req.new_password.len() < 8 {
        return Err(ApiError::BadRequest(
            "password must be at least 8 characters".into(),
        ));
    }
    if req.new_password.len() > MAX_PASSWORD_LEN {
        return Err(ApiError::BadRequest(format!(
            "password must be at most {MAX_PASSWORD_LEN} characters"
        )));
    }

    if !is_reset_token_shape(&req.token) {
        return Err(ApiError::Auth(AuthError::InvalidToken));
    }

    let hash = hash_token(&req.token);
    // Keyed on the hash, so nothing sensitive lands in Redis.
    rate_limit(
        &state,
        &format!("sauron:auth:reset:tok:{hash}"),
        RESET_ATTEMPTS_PER_TOKEN_PER_HOUR,
        3600,
    )
    .await?;

    let mut conn = db(&state).await?;
    let row = repo::find_live_password_reset_token(&mut conn, &hash)
        .await?
        .ok_or(ApiError::Auth(AuthError::InvalidToken))?;
    let user = repo::get_user(&mut conn, row.user_id)
        .await?
        .ok_or(ApiError::Auth(AuthError::InvalidToken))?;
    // The holder controls the mailbox, so telling them is honest rather than a
    // leak.
    if !user.is_active {
        return Err(ApiError::Auth(AuthError::AccountDeactivated));
    }

    // "The password changed since this link was issued." Checked at the point
    // of use so it cannot be forgotten by future code, rather than swept from
    // every password-writing path.
    if hash_token(&user.password_hash) != row.password_fingerprint {
        return Err(ApiError::Auth(AuthError::InvalidToken));
    }

    // `change_password` compares plaintexts; there is no current password on
    // this request, so verifying against the stored hash is the only rule this
    // endpoint can enforce. Strictly stronger, one extra Argon2 op, and the
    // user-visible string is the shipped one verbatim so the two endpoints stay
    // consistent. Note this branch returns WITHOUT consuming the token — which
    // is what `RESET_ATTEMPTS_PER_TOKEN_PER_HOUR` exists to bound.
    if verify_password_async(req.new_password.clone(), user.password_hash.clone()).await {
        return Err(ApiError::BadRequest(
            "the new password must be different from the current one".into(),
        ));
    }

    let new_hash = hash_password_async(req.new_password.clone())
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    // The expensive work sits BEFORE the burn: a failed Argon2 must not eat the
    // user's only link, and a crash between the burn and the password write
    // must leave the account unchanged. The burn itself is one atomic
    // `UPDATE … RETURNING`, which is how single-use is enforced without
    // `conn.transaction` (async closures need Rust 1.85; workspace MSRV is
    // 1.82 per packaging/rpm/sauron.spec).
    let Some((user_id, _fingerprint, _mode)) =
        repo::consume_password_reset_token(&mut conn, &hash).await?
    else {
        return Err(ApiError::Auth(AuthError::InvalidToken));
    };

    // Revoke, then write, in `change_password`'s order and for its reason: on a
    // partial failure the account must never end up with a new password and
    // live old sessions. `except` is None because a reset kills every session
    // including any the caller holds; `actor` is None because this path is
    // unauthenticated — nobody has proved who they are, only that they hold the
    // mailbox, and `auth_sessions.revoked_by` must not name the victim as the
    // person who did it.
    let ids = repo::revoke_sessions_for_user(
        &mut conn,
        user_id,
        None,
        repo::REVOKE_PASSWORD_RESET,
        None,
    )
    .await?;
    // Without this the kill is invisible on THIS replica until its next
    // AUTH_REVOCATION_POLL_SECS tick — the one place the delay is visible.
    state.revocations.mark_revoked(&ids);

    // Zero rows means someone else set the password between the fingerprint
    // read and here, two Argon2 ops of wall clock later. Failing at this point
    // rather than earlier is the acceptable direction: the link is correctly
    // spent, the sessions are revoked, and their new password stands.
    if repo::set_user_password_if_hash_matches(&mut conn, user_id, &user.password_hash, &new_hash)
        .await?
        == 0
    {
        return Err(ApiError::Auth(AuthError::InvalidToken));
    }

    // Bookkeeping, not the guarantee: the fingerprint above already kills every
    // sibling link. Doing it explicitly keeps the table's state legible.
    repo::invalidate_password_reset_tokens_for_user(
        &mut conn,
        user_id,
        repo::RESET_INVALIDATED_PASSWORD_SET,
    )
    .await?;

    // No tokens, no user object.
    Ok(Json(serde_json::json!({ "ok": true })))
}
```

- [ ] **Step 2: Register the route.** In `backend/bins/sauron-api/src/main.rs`, immediately after the `/v1/auth/forgot-password` route added in Task 5, add:
```rust
        .route("/v1/auth/reset-password", post(routes::auth::reset_password))
```

- [ ] **Step 3: Compile and see it pass.** Run:
```
cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo check --workspace --all-targets
```
Expect `Finished`.

- [ ] **Step 4: Drive the dead-token path by hand.** Start the API as in Task 5 Step 5, then run:
```
curl -s -i -X POST localhost:8099/v1/auth/reset-password -H 'content-type: application/json' -d "{\"token\":\"$(printf 'a%.0s' {1..64})\",\"new_password\":\"correcthorse\"}"
curl -s -i -X POST localhost:8099/v1/auth/reset-password -H 'content-type: application/json' -d '{"token":"short","new_password":"correcthorse"}'
curl -s -i -X POST localhost:8099/v1/auth/reset-password -H 'content-type: application/json' -d '{"token":"short","new_password":"abc"}'
```
Expect the first two to return `HTTP/1.1 401 Unauthorized` with **byte-identical** bodies `{"error":{"code":"invalid_token","message":"invalid or expired token"}}`, and the third `HTTP/1.1 400 Bad Request` with "password must be at least 8 characters" — length is checked before token shape. Stop the server.

- [ ] **Step 5: Format and lint.** Run:
```
cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings
```
Expect both to exit 0, with no `dead_code` warning for `is_reset_token_shape`. If Task 4 Step 8 added `#[allow(dead_code)]` to it, remove that attribute now.

---

### Task 7: The `login` refusal

**Files:**
- Modify `backend/bins/sauron-api/src/routes/auth.rs` (`login`, the `is_active` check at ~lines 303-310)

**Interfaces:**
- Consumes: `AuthError::PasswordResetRequired` (Task 3), `User.credentials_invalidated_at` (Task 1).
- Produces: `login` answers `403 password_reset_required` when the credential is invalidated, after a successful Argon2 verification.

- [ ] **Step 1: Add the check.** In `backend/bins/sauron-api/src/routes/auth.rs`, in `login`, immediately after the existing `if !user.is_active { return Err(ApiError::Auth(AuthError::AccountDeactivated)); }` block and **before** `repo::touch_last_login`, insert:
```rust
    // Placed here, after the Argon2 verification, for exactly the reason the
    // is_active check above is: a branch ahead of the verification answers in
    // microseconds for a reset-pending account and tens of milliseconds for
    // every other one, which is the enumeration oracle `spend_dummy_verify`
    // exists to close — and it would additionally leak, to anyone who can type
    // an address, that a particular person is mid-lockout.
    //
    // This is what makes an admin-initiated reset mean what it says: the
    // suspect password stops working at the login form, rather than merely
    // producing a gated session. Completing the reset clears the column, in the
    // same statement that writes the new password.
    if user.credentials_invalidated_at.is_some() {
        return Err(ApiError::Auth(AuthError::PasswordResetRequired));
    }
```
`refresh` needs no equivalent: the admin route revokes sessions and `revoke_sessions_for_user` revokes `refresh_tokens` in the same statement, so there is no live refresh token to present. `/v1/auth/password` needs none either, because reaching it requires a session nobody can now obtain.

- [ ] **Step 2: Compile.** Run:
```
cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo check --workspace --all-targets
```
Expect `Finished`.

- [ ] **Step 3: Drive it against the live database.** Start the API as in Task 5 Step 5. In a second shell, register a throwaway account, then lock it directly and observe the refusal:
```
curl -s -X POST localhost:8099/v1/auth/register -H 'content-type: application/json' -d '{"email":"s1probe@example.com","password":"correcthorse","name":"Probe","org_name":"S1 Probe"}' | head -c 120
psql postgres://sauron:sauron@172.20.0.2:5432/sauron -c "UPDATE users SET credentials_invalidated_at = now() WHERE lower(email) = 's1probe@example.com';"
curl -s -i -X POST localhost:8099/v1/auth/login -H 'content-type: application/json' -d '{"email":"s1probe@example.com","password":"correcthorse"}'
curl -s -i -X POST localhost:8099/v1/auth/login -H 'content-type: application/json' -d '{"email":"s1probe@example.com","password":"wrongpassword"}'
```
Expect the third command to return `HTTP/1.1 403 Forbidden` with code `password_reset_required` **and no tokens at all**, and the fourth to return `HTTP/1.1 401 Unauthorized` with `invalid_credentials` — proving the refusal is only reachable after the password verifies. Clean up:
```
psql postgres://sauron:sauron@172.20.0.2:5432/sauron -c "UPDATE users SET credentials_invalidated_at = NULL WHERE lower(email) = 's1probe@example.com';"
```
Stop the server.

- [ ] **Step 4: Format and lint.** Run:
```
cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings
```
Expect both to exit 0.

---

### Task 8: `POST /v1/orgs/{org_id}/members/{user_id}/password-reset`

**Files:**
- Modify `backend/bins/sauron-api/src/routes/orgs.rs` (imports at 1-25; `MemberGrant` at 126-136; `list_members` at 138-160; new handler after `set_member_active`, ~line 786)
- Modify `backend/bins/sauron-api/src/main.rs` (route table, beside `/v1/orgs/{org_id}/members/{user_id}` at ~line 167)

**Interfaces:**
- Consumes: `guard_member_admin_action` (S2), `perm::MEMBER_CREDENTIAL` (S2), `repo::revoke_sessions_for_user` (S2), `state.revocations.mark_revoked` (S2), `routes::auth::{rate_limit, client_addr}` (S2), `routes::auth::{ResetMode, reset_link, render_password_reset_mail, ResetMailVars, ADMIN_RESET_TTL_SECS, ADMIN_RESET_PER_CALLER_PER_HOUR, ADMIN_RESET_PER_TARGET_PER_HOUR}` (Task 4), `ApiError::Unavailable` (Task 3), the repo functions from Task 2.
- Produces: `pub struct ResetMemberPasswordReq { pub action: String }`; `pub async fn reset_member_password(...) -> Result<Json<serde_json::Value>, ApiError>` answering `{ "ok": true, "action": "reset"|"cancel", "expires_at": "<rfc3339>"|null }`; `MemberGrant.credentials_invalidated_at: Option<DateTime<Utc>>`.

- [ ] **Step 1: Add the imports.** In `backend/bins/sauron-api/src/routes/orgs.rs`, extend the import block with:
```rust
use std::net::SocketAddr;

use axum::extract::ConnectInfo;
use chrono::{DateTime, Utc};

use sauron_mail::MailKind;

use super::auth::{
    client_addr, rate_limit, render_password_reset_mail, reset_link, ResetMailVars, ResetMode,
    ADMIN_RESET_PER_CALLER_PER_HOUR, ADMIN_RESET_PER_TARGET_PER_HOUR, ADMIN_RESET_TTL_SECS,
};
```
Merge each line into the file's existing `use` groups rather than duplicating a group.

- [ ] **Step 2: Surface `credentials_invalidated_at` on the members list.** In the same file, add to `struct MemberGrant`, after `pub is_active: bool,`:
```rust
    /// Non-null while an admin-forced reset is outstanding on this account.
    ///
    /// `GET /v1/orgs/{org}/members` is the only place the dashboard learns
    /// anything about a member's account state, and without this field the
    /// cancel action exists on the server and is unreachable from the UI —
    /// which is the same as not existing, since the admin who needs it is
    /// looking at a members table, not at `curl`.
    pub credentials_invalidated_at: Option<DateTime<Utc>>,
```
And in `list_members`, change the map closure (which Task 2 Step 9 left as `|(g, email, name, role_name, is_active, _credentials_invalidated_at)|`) to:
```rust
        .map(
            |(g, email, name, role_name, is_active, credentials_invalidated_at)| MemberGrant {
                id: g.id,
                user_id: g.user_id,
                email,
                name,
                role_id: g.role_id,
                role_name,
                scope_type: g.scope_type,
                scope_id: g.scope_id,
                is_active,
                credentials_invalidated_at,
            },
        )
```

- [ ] **Step 3: Add the handler.** In the same file, immediately after `set_member_active` (ends ~line 786, just before `struct UpdateGrantReq`), insert:
```rust
#[derive(Deserialize)]
pub struct ResetMemberPasswordReq {
    /// `"reset"` or `"cancel"`. The default is the forward action; an
    /// unrecognised value is a 400, never a silent reset.
    ///
    /// This `#[serde(default)]` only covers `{}` — a body that parses but omits
    /// the key. It does **not** cover a body-less `POST`, because `Json`
    /// rejects that before serde is ever called. The handler takes
    /// `Option<Json<…>>` for that case; see its signature.
    #[serde(default = "default_reset_action")]
    pub action: String,
}

fn default_reset_action() -> String {
    "reset".to_string()
}

impl Default for ResetMemberPasswordReq {
    fn default() -> Self {
        Self {
            action: default_reset_action(),
        }
    }
}

/// Force a password reset on a member, or cancel one already forced.
///
/// `reset` is destructive and says so: the target's current password stops
/// authenticating at the login form, every session ends within a few seconds,
/// and the emailed link is the only way back in. There is deliberately no
/// second, non-destructive "just send them a link" mode — shipping both puts an
/// admin holding a suspected leak in front of two adjacent buttons, one of
/// which stops the leaked password and one of which looks like it does.
///
/// `cancel` is the undo. It exists because this action is destructive *and*
/// gated on a mail relay the deployment may have misconfigured; without an undo
/// that does not itself depend on the relay, one bounced message is an account
/// nobody can reach.
///
/// There is deliberately no last-`org:manage` guard: a forced reset removes
/// nobody's permission — the target regains their account by using the link —
/// so an org can never be orphaned by it.
pub async fn reset_member_password(
    auth: AuthUser,
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: axum::http::HeaderMap,
    Path((org_id, user_id)): Path<(Uuid, Uuid)>,
    // `Option<Json<…>>`, not `Json<…>`, and that is the whole reason the
    // body-less `curl -X POST` documented above works. A bare `Json` extractor
    // rejects a request with no `content-type: application/json` with 415 and an
    // empty body with 400 — both *before* serde runs, so `#[serde(default)]`
    // never gets a chance. axum 0.8's `OptionalFromRequest for Json` hands back
    // `Ok(None)` when the header is absent, which is exactly the shape an
    // operator's `curl` sends. Body-consuming extractors must stay last.
    req: Option<Json<ResetMemberPasswordReq>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let req = req.map(|Json(r)| r).unwrap_or_default();

    let mut conn = db(&state).await?;
    // `member:credential` in ADDITION to `member:manage`, which
    // `guard_member_admin_action` demands as its first step. `member:manage` is
    // the routine permission for handing out and revoking grants; forcing a
    // reset combined with control of the mail relay is a path to account
    // takeover, and an org that hands out the former has not agreed to the
    // latter. The narrower permission never stands in for the broader one.
    authorize_org(&mut conn, auth.user_id, org_id, perm::MEMBER_CREDENTIAL).await?;

    let cancel = match req.action.as_str() {
        "reset" => false,
        "cancel" => true,
        _ => {
            return Err(ApiError::BadRequest(
                "action must be \"reset\" or \"cancel\"".into(),
            ))
        }
    };

    // Resolved BEFORE anything is applied, and that ordering is the whole
    // guarantee: a destructive change must never land when the message carrying
    // its remedy cannot be sent. `cancel` is deliberately exempt — gating the
    // undo on the same configuration that motivates it would make it
    // unreachable in precisely the deployment that needs it. The response never
    // carries the token or the link under any condition: that link is an
    // account-takeover primitive, and `member:credential` lets its holder deny
    // a member their account, not sign in as them.
    let mail_and_url = if cancel {
        None
    } else {
        let mail = state
            .mail
            .as_ref()
            .cloned()
            .ok_or_else(|| ApiError::Unavailable("SMTP is not configured on this server".into()))?;
        let url = state
            .cfg
            .require_dashboard_url()
            .map_err(|e| ApiError::Unavailable(e.to_string()))?
            .to_string();
        Some((mail, url))
    };

    // `member:credential` is in the Admin preset, not just Owner, and an
    // unbounded loop here is an unbounded mail bomb aimed at one member's inbox
    // and an unbounded re-lock of an account somebody is trying to recover.
    rate_limit(
        &state,
        &format!("sauron:auth:adminreset:{}", auth.user_id),
        ADMIN_RESET_PER_CALLER_PER_HOUR,
        3600,
    )
    .await?;
    if !cancel {
        // `cancel` spends the per-caller bucket ONLY. It sends no mail and can
        // only ever restore access, so charging it to the per-target bucket
        // would mean an admin who forced five resets in an hour cannot undo the
        // fifth — a limiter blocking the remedy for the thing it was limiting.
        rate_limit(
            &state,
            &format!("sauron:auth:adminreset:target:{user_id}"),
            ADMIN_RESET_PER_TARGET_PER_HOUR,
            3600,
        )
        .await?;
    }

    // Carries the whole shared stack: `member:manage`, user-exists 404,
    // grant-in-this-org 404, self-target 409, no-escalation against the
    // target's full union with the caller's org-scope set, and the
    // unconditional cross-org refusal. `allow_self` is false: resetting
    // yourself is redundant (`/v1/auth/password` exists) and it lets an admin
    // lock themselves out over a relay they may have just broken, leaving
    // nobody with standing to cancel it. No local copy of any of those checks
    // is added here — a second copy of the cross-org rule is one more place for
    // the two to drift apart.
    let _target_grants =
        guard_member_admin_action(&mut conn, auth.user_id, org_id, user_id, false).await?;

    let user = repo::get_user(&mut conn, user_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    // Same spirit as `create_grant`'s refusal to grant to an inactive account:
    // a deactivated user's authority never grows.
    if !user.is_active {
        return Err(ApiError::Conflict(
            "reactivate this member before resetting their password".into(),
        ));
    }

    if cancel {
        repo::set_user_credentials_invalidated(&mut conn, user_id, None).await?;
        // Killing the outstanding links is the other half of a cancel. Leaving
        // them live means the mail everyone had written off can be delivered a
        // day later, and whoever opens it sets a password for an account whose
        // owner has been using their old one since — a second, unannounced
        // sign-out days after the incident was closed.
        //
        // `must_change_password` is deliberately NOT cleared. Cancelling
        // restores the ability to sign in; it does not pretend the admin never
        // had a reason. It may also have been set long before this reset, by
        // `create_member`'s reveal-once temp password, and cancel has no way to
        // tell the two apart.
        repo::invalidate_password_reset_tokens_for_user(
            &mut conn,
            user_id,
            repo::RESET_INVALIDATED_SUPERSEDED,
        )
        .await?;
        return Ok(Json(serde_json::json!({
            "ok": true,
            "action": "cancel",
            "expires_at": serde_json::Value::Null,
        })));
    }

    let (mail, dashboard_url) = mail_and_url.expect("the reset branch always resolves mail config");

    // Gates before revoke, and fail-safe in that direction: `routes::auth::refresh`
    // re-reads `user.must_change_password` and bakes it into the next access
    // token, so even if the revocation write fails the target's next refresh
    // mints a gated token within one access-token lifetime. The reverse order
    // leaves a window with sessions killed and no gate.
    repo::set_user_must_change_password(&mut conn, user_id, true).await?;
    repo::set_user_credentials_invalidated(&mut conn, user_id, Some(Utc::now())).await?;

    // `actor` is the admin, which is the only way `auth_sessions.revoked_by`
    // ever records who forced the reset — `password_reset_tokens.initiated_by`
    // answers that for the link, but not for the sessions.
    let ids = repo::revoke_sessions_for_user(
        &mut conn,
        user_id,
        None,
        repo::REVOKE_RESET_FORCED,
        Some(auth.user_id),
    )
    .await?;
    // Turns the dialog's "within a few seconds" into a statement about this
    // replica rather than about its next poll.
    state.revocations.mark_revoked(&ids);

    // Unlike self-service, an admin trigger supersedes outstanding links: this
    // is an authoritative act by an identified principal, the admin means *this*
    // link now, and a re-issue after a bounce must not leave two live links.
    repo::invalidate_password_reset_tokens_for_user(
        &mut conn,
        user_id,
        repo::RESET_INVALIDATED_SUPERSEDED,
    )
    .await?;

    let raw = sauron_core::ids::opaque_token();
    let expires_at = Utc::now() + chrono::Duration::seconds(ADMIN_RESET_TTL_SECS);
    repo::insert_password_reset_token(
        &mut conn,
        user_id,
        sauron_auth::hash_token(&raw),
        sauron_auth::hash_token(&user.password_hash),
        expires_at,
        ResetMode::Admin.as_str(),
        Some(auth.user_id),
        // Populated here and not on the self-service path's behalf: self-service
        // rows only ever record an anonymous stranger's proxy address, so admin
        // rows are the half of the audit trail that matters.
        Some(client_addr(&headers, &peer, &state)),
    )
    .await?;

    let org = repo::get_org(&mut conn, org_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let org_name = org.name.clone();
    let display_name = if user.name.trim().is_empty() {
        user.email.clone()
    } else {
        user.name.clone()
    };
    let email = user.email.clone();

    // `MailSender` checks out its own pooled connection; see the identical drop
    // and its full reasoning in `routes::auth::forgot_password`.
    drop(conn);

    let content = render_password_reset_mail(ResetMailVars {
        mode: ResetMode::Admin,
        display_name: &display_name,
        reset_url: &reset_link(&dashboard_url, &raw),
        org_name: &org_name,
    });
    // The recipient is known here, so this path uses `enqueue` rather than
    // `enqueue_or_discard` — there is no branch to hide. The TTL passed here
    // becomes the mail row's own expires_at, so the message and the link it
    // carries die together.
    mail.enqueue(
        MailKind::PasswordReset,
        &email,
        &content,
        Some(user_id),
        std::time::Duration::from_secs(ADMIN_RESET_TTL_SECS as u64),
    )
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;

    Ok(Json(serde_json::json!({
        "ok": true,
        "action": "reset",
        "expires_at": expires_at.to_rfc3339(),
    })))
}
```

- [ ] **Step 4: Register the route.** In `backend/bins/sauron-api/src/main.rs`, immediately after the `/v1/orgs/{org_id}/members/{user_id}` PATCH route, add:
```rust
        .route(
            "/v1/orgs/{org_id}/members/{user_id}/password-reset",
            post(routes::orgs::reset_member_password),
        )
```

- [ ] **Step 5: Compile and see it pass.** Run:
```
cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo check --workspace --all-targets
```
Expect `Finished`. If it reports `ConnectInfo<SocketAddr>: FromRequestParts` is unsatisfied, the router is not served with `into_make_service_with_connect_info::<SocketAddr>()` — it already is, for `routes::auth::login`, so the more likely cause is a missing `use std::net::SocketAddr;`.

- [ ] **Step 6: Drive both actions, the body-less shape and the 503.** Start the API as in Task 5 Step 5 but **without** `SMTP_SINK` and **without** `DASHBOARD_URL`, then, using an owner token and member ids from your live database, run:
```
curl -s -i -X POST "localhost:8099/v1/orgs/$ORG/members/$TARGET/password-reset" -H "authorization: Bearer $TOKEN" -H 'content-type: application/json' -d '{"action":"reset"}'
curl -s -i -X POST "localhost:8099/v1/orgs/$ORG/members/$TARGET/password-reset" -H "authorization: Bearer $TOKEN"
curl -s -i -X POST "localhost:8099/v1/orgs/$ORG/members/$TARGET/password-reset" -H "authorization: Bearer $TOKEN" -H 'content-type: application/json' -d '{}'
curl -s -i -X POST "localhost:8099/v1/orgs/$ORG/members/$TARGET/password-reset" -H "authorization: Bearer $TOKEN" -H 'content-type: application/json' -d '{"action":"cancel"}'
curl -s -i -X POST "localhost:8099/v1/orgs/$ORG/members/$TARGET/password-reset" -H "authorization: Bearer $TOKEN" -H 'content-type: application/json' -d '{"action":"nonsense"}'
```
Expect the first three all `HTTP/1.1 503 Service Unavailable` with code `unavailable` — the second is the body-less shape and the third the empty-object shape, and both defaulting to `reset` is what the `Option<Json<…>>` extractor buys. Expect the fourth `HTTP/1.1 200 OK` with `"action":"cancel"` — cancel is exempt from the SMTP precondition — and the fifth `HTTP/1.1 400 Bad Request`. A `415 Unsupported Media Type` on the second command means the extractor is still a bare `Json<…>`. Stop the server.

- [ ] **Step 7: Format and lint.** Run:
```
cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings
```
Expect both to exit 0.

---

### Task 9: The 30-day reaper

**Files:**
- Modify `backend/bins/sauron-api/src/tasks.rs` (the task registration list S0 created)

**Interfaces:**
- Consumes: `repo::prune_password_reset_tokens` (Task 2), S0's supervisor.
- Produces: `const PASSWORD_RESET_RETENTION_DAYS: i64 = 30`; an hourly supervised task named `password_reset_reaper`.

- [ ] **Step 1: Add the constant and the task.** In `backend/bins/sauron-api/src/tasks.rs`, add near the file's other retention constants:
```rust
/// How long a reset row survives, consumed or not.
///
/// This table is the only audit trail the deployment has that an admin forced a
/// reset on someone — there is no `audit_events` table — so this constant also
/// caps how far back that question can be answered. A compile-time constant
/// rather than an env var: a handful of tiny short-lived rows do not justify
/// three files of documentation.
const PASSWORD_RESET_RETENTION_DAYS: i64 = 30;
```
Then register the task beside S0's `mail_outbox` reaper, matching that registration's shape verbatim and changing only the name, the period and the body:
```rust
    // Lives here, not in `sauron-alerts`. packaging/rpm/SETUP.md's shipped
    // install line is
    // `systemctl enable --now sauron-api sauron-ingest sauron-monitor sauron-tier`,
    // there is no preset file under packaging/rpm/, and `%systemd_post` falls
    // through to the distro default of `disable` — so on every RPM deployment a
    // reaper in that binary would simply never run, while this table's write
    // path is an unauthenticated endpoint. The rule is that a table's reaper
    // lives in the process that owns its write path.
    //
    // Deleting these rows disables nothing: unlike `refresh_tokens`, whose
    // revoked rows are load-bearing for replay detection, nothing reads a dead
    // reset row.
    spawn_periodic("password_reset_reaper", Duration::from_secs(3600), {
        let pool = pool.clone();
        move || {
            let pool = pool.clone();
            async move {
                let mut conn = sauron_db::conn(&pool).await?;
                let removed =
                    repo::prune_password_reset_tokens(&mut conn, PASSWORD_RESET_RETENTION_DAYS)
                        .await?;
                // Checked out, worked, dropped — the API pool is 16 for the
                // whole process and this loop must not hold one between ticks.
                drop(conn);
                if removed > 0 {
                    tracing::info!(removed, "pruned expired password reset tokens");
                }
                Ok(())
            }
        }
    });
```
`spawn_periodic` returns `()`, so this registration must never `?` out of `main()`: `sauron-api.service` is `Restart=on-failure` with no `StartLimit` override, so a failure at boot burns systemd's five-starts-in-ten-seconds budget and leaves the unit `failed` with no HTTP surface to diagnose from. The closure returns `anyhow::Result<()>` and the supervisor is what handles a failed tick.

- [ ] **Step 2: Compile.** Run:
```
cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo check --workspace --all-targets
```
Expect `Finished`.

- [ ] **Step 3: Observe the task registered.** Start the API as in Task 5 Step 5 but with `RUST_LOG=info`, and confirm the startup log names `password_reset_reaper` among the supervised tasks, and that `/health` reports a `password_reset_reaper` entry:
```
curl -s localhost:8099/health
```
Stop the server.

- [ ] **Step 4: Prove the SQL runs.** Insert a stale row directly and run one reap by restarting with a temporary one-second period is not worth the churn; instead assert the statement itself:
```
psql postgres://sauron:sauron@172.20.0.2:5432/sauron -c "DELETE FROM password_reset_tokens WHERE created_at < now() - ('30' || ' days')::interval;"
```
Expect `DELETE 0` (or a count), not a syntax error — this is the exact string `prune_password_reset_tokens` binds.

- [ ] **Step 5: Format and lint.** Run:
```
cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings
```
Expect both to exit 0.

---

### Task 10: Integration suite

**Files:**
- Create `backend/bins/sauron-api/tests/http_password_reset.rs`
- Reference (copy the harness from) `backend/bins/sauron-api/tests/http_workflows.rs` lines 33-215

**Interfaces:**
- Consumes: every route and repo function from Tasks 1-9.
- Produces: nothing other tasks read.

- [ ] **Step 1: Copy the harness.** Create `backend/bins/sauron-api/tests/http_password_reset.rs` and copy into it, verbatim from `backend/bins/sauron-api/tests/http_workflows.rs`, the items `swap_database`, `free_port`, `struct TestServer`, `impl TestServer` — `start`, `conn`, **`get`**, **`get_status`**, `shutdown` — and `impl Drop for TestServer`. `get` and `get_status` (http_workflows.rs:169-181) are not optional: Step 6 calls `get_status("/v1/me", …)` twice and without them the test binary does not compile. Do **not** copy `percent_encode_segment`, `get_json` or `assert_status`; nothing here calls them and `cargo clippy --all-targets -- -D warnings` fails on a dead one.

  Change these four things:
  - the module doc comment,
  - `const JWT_SECRET: &str = "http-password-reset-test-secret-00000000000";`,
  - inside `start`, the db name discriminator — **timestamp segment first**, discriminator glued to the uuid, because `sauron-db`'s stale-DB reaper parses the first underscore-delimited segment after `sauron_test_` as a timestamp and silently skips anything else, leaking every database it cannot parse:
```rust
        let db_name = format!(
            "sauron_test_{}_pr{}",
            Utc::now().timestamp(),
            Uuid::new_v4().simple()
        );
```
  - and `start` itself, which becomes two entry points over one body, because one test in this file needs a deployment with **no** mail configured. Replace the `async fn start() -> Option<TestServer> {` line with:
```rust
    /// The ordinary fixture: SMTP in sink mode and a dashboard URL, so the mail
    /// path is exercised end to end without a relay.
    async fn start() -> Option<TestServer> {
        Self::start_with_mail(true).await
    }

    /// The deployment whose operator never configured SMTP. `state.mail` is
    /// `None` and `require_dashboard_url()` fails, which is the only way to
    /// reach the admin route's 503 and `forgot_password`'s swallow branch.
    async fn start_without_mail() -> Option<TestServer> {
        Self::start_with_mail(false).await
    }

    async fn start_with_mail(mail: bool) -> Option<TestServer> {
```
  and replace the copied `let mut child = tokio::process::Command::new(bin)` chain (through `.spawn().expect("spawn sauron-api binary");`) with:
```rust
        let mut cmd = tokio::process::Command::new(bin);
        cmd.env("DATABASE_URL", &db_url)
            .env("REDIS_URL", &redis_url)
            .env("JWT_SECRET", JWT_SECRET)
            .env("API_PORT", port.to_string())
            .env("CORS_ALLOWED_ORIGINS", "http://localhost:5173")
            .env("RUST_LOG", "error")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if mail {
            cmd.env("SMTP_SINK", "1")
                .env("SMTP_HOST", "localhost")
                .env("DASHBOARD_URL", "https://dash.test")
                // S0's drain blanks `body_text` and `body_html` the instant it
                // marks a row `sink`, and `newest_reset_token_from_mail` reads
                // exactly that column. At the shipped 60-second default, any
                // test that spends more than a minute in Argon2 loses its token
                // mid-run and fails with "a reset link in the body". 3600 is the
                // config clamp's ceiling: the drain fires once against an empty
                // outbox at boot and never again inside a test's lifetime.
                .env("MAIL_DRAIN_TICK_SECS", "3600");
        }
        let mut child = cmd.spawn().expect("spawn sauron-api binary");
```
  The `mail = false` branch deliberately sets none of the four: S0 registers no `mail_drain` task and leaves `AppState.mail` as `None` when `SMTP_HOST` is absent, which is the state the 503 exists for.

- [ ] **Step 2: Add the JSON POST helpers.** Append to `impl TestServer`:
```rust
    async fn post_json(&self, path: &str, token: Option<&str>, body: Value) -> reqwest::Response {
        let mut req = self.client.post(format!("{}{path}", self.base)).json(&body);
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        req.send()
            .await
            .unwrap_or_else(|e| panic!("POST {path} failed: {e}"))
    }

    /// `(status, raw body text)`. The raw text matters: the anti-enumeration
    /// assertion is that two bodies are **byte-identical**, which a parsed
    /// `Value` comparison would not prove.
    async fn post_raw(&self, path: &str, token: Option<&str>, body: Value) -> (u16, String) {
        let resp = self.post_json(path, token, body).await;
        let status = resp.status().as_u16();
        let text = resp.text().await.expect("read body");
        (status, text)
    }
```

- [ ] **Step 3: Add the fixture helpers.** Append to the file:
```rust
/// Sign in over the real route. Returns `(access_token, refresh_token)`.
async fn login(srv: &TestServer, email: &str, password: &str) -> (String, String) {
    let (status, text) = srv
        .post_raw(
            "/v1/auth/login",
            None,
            json!({"email": email, "password": password}),
        )
        .await;
    assert_eq!(status, 200, "login {email}: {text}");
    let v: Value = serde_json::from_str(&text).expect("login body is JSON");
    (
        v["access_token"].as_str().expect("access_token").to_string(),
        v["refresh_token"]
            .as_str()
            .expect("refresh_token")
            .to_string(),
    )
}

/// Create an organization and its Owner, and sign them in. Returns
/// `(user_id, org_id, access_token, refresh_token)`.
///
/// Deliberately **not** `POST /v1/auth/register`, even though that is the route
/// a person uses. That route spends `REGISTER_ATTEMPTS_PER_HOUR = 10` keyed on
/// `sauron:auth:register:{client_addr}` — and `client_addr` is `127.0.0.1` for
/// every test in this file, in the *shared* `TEST_REDIS_URL`. This file needs
/// more owners than ten, so the eleventh call would 429; and because the window
/// is an hour, a second run inside the same hour would start already over
/// budget even if it needed fewer. `tests/http_workflows.rs` and
/// `tests/http_env_scoping.rs` build their fixtures out of `repo::create_user`
/// for exactly this reason. Login is safe to keep on the real route: its per-IP
/// budget is 60 per **60 seconds**, which self-heals.
///
/// This also gives every account exactly one org — an owner minted here holds no
/// grant anywhere else, which `guard_member_admin_action`'s unconditional
/// cross-org refusal requires of anything an admin test touches. Use
/// `create_member` for the targets.
async fn owner_of_new_org(
    srv: &TestServer,
    email: &str,
    password: &str,
) -> (Uuid, Uuid, String, String) {
    let (user_id, org_id) = {
        let mut conn = srv.conn().await;
        let hash = sauron_auth::hash_password_async(password.to_string())
            .await
            .expect("hash password");
        let user = repo::create_user(&mut conn, email, &hash, "Test Owner")
            .await
            .expect("create owner");
        let org = repo::create_org(
            &mut conn,
            &format!("Org {email}"),
            &format!("org-{}", Uuid::new_v4().simple()),
        )
        .await
        .expect("create org");
        let owner_role = repo::get_system_role(&mut conn, "Owner")
            .await
            .expect("get Owner role")
            .expect("Owner preset role");
        repo::create_grant(
            &mut conn,
            NewRoleGrant {
                org_id: org.id,
                user_id: user.id,
                role_id: owner_role.id,
                scope_type: "org".to_string(),
                scope_id: org.id,
            },
        )
        .await
        .expect("grant Owner at org scope");
        (user.id, org.id)
    };
    let (access, refresh) = login(srv, email, password).await;
    (user_id, org_id, access, refresh)
}

/// Create a member who exists **only** in `org_id`, and return their user id.
///
/// `role_name` is matched against `repo::list_roles(conn, org_id)`, which
/// returns the four system presets plus this org's custom roles — so "Viewer",
/// "Admin" and a role the test just created all resolve here.
async fn create_member(
    srv: &TestServer,
    org_id: Uuid,
    email: &str,
    password: &str,
    role_name: &str,
) -> Uuid {
    let mut conn = srv.conn().await;
    let hash = sauron_auth::hash_password_async(password.to_string())
        .await
        .expect("hash password");
    let roles = repo::list_roles(&mut conn, org_id)
        .await
        .expect("list roles");
    let role = roles
        .iter()
        .find(|r| r.name == role_name)
        .unwrap_or_else(|| panic!("no role named {role_name} in this org"));
    let rows = repo::create_member_with_grants(
        &mut conn,
        email,
        &hash,
        "Test Member",
        org_id,
        role.id,
        &["org".to_string()],
        &[org_id],
    )
    .await
    .expect("create member");
    let user_id = rows[0].user_id;
    // That statement hardcodes `must_change_password = true`, which is
    // `create_member`'s reveal-once temp-password contract. Left set, every
    // access token this account gets is gated by `password_change_gate`, and the
    // guard-stack test's "caller has no permission" 403 would be
    // `password_change_required` rather than the RBAC refusal it claims to
    // prove — a green test asserting nothing.
    repo::set_user_must_change_password(&mut conn, user_id, false)
        .await
        .expect("clear the temp-password demand");
    user_id
}

/// The raw token for a user's newest reset row.
///
/// The test has DB access, so nothing needs to be logged for it — but the raw
/// token exists only in the email, so it is read out of the `mail_outbox` body
/// rather than out of `password_reset_tokens`, which stores only the hash.
/// `body_text` is S0's column name; see the prerequisites table.
async fn newest_reset_token_from_mail(srv: &TestServer, email: &str) -> String {
    #[derive(diesel::QueryableByName)]
    struct BodyRow {
        #[diesel(sql_type = Text)]
        body_text: String,
    }
    let mut conn = srv.conn().await;
    let row: BodyRow = diesel::sql_query(
        "SELECT body_text FROM mail_outbox WHERE recipient = $1 AND kind = 'password_reset' \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind::<Text, _>(email)
    .get_result(&mut conn)
    .await
    .expect("a password_reset row in mail_outbox");
    let marker = "?token=";
    let start = row.body_text.find(marker).expect("a reset link in the body") + marker.len();
    row.body_text[start..]
        .chars()
        .take_while(|c| c.is_ascii_hexdigit())
        .collect()
}

/// Let the *next* `password_reset` mail to `email` through S0's dedup window.
///
/// `MailKind::PasswordReset.dedup_window()` is 300 seconds, and the probe inside
/// `repo::enqueue_mail` matches `(kind, recipient_key)` over rows newer than
/// that **whose status is not 'failed'**. Two reset mails to one address inside
/// one test run would otherwise suppress the second silently: `enqueue` returns
/// `Ok(None)` and neither the handler nor the test can tell that apart from a
/// send. Flipping the existing rows to 'failed' uses S0's own carve-out for a
/// legitimate retry, and unlike a DELETE it leaves them countable.
async fn unblock_reset_mail(srv: &TestServer, email: &str) {
    let mut conn = srv.conn().await;
    diesel::sql_query(
        "UPDATE mail_outbox SET status = 'failed' \
         WHERE recipient_key = $1 AND kind = 'password_reset'",
    )
    .bind::<Text, _>(email.to_lowercase())
    .execute(&mut conn)
    .await
    .expect("release the dedup window");
}
```
Add `use diesel::sql_types::Text;`, `use diesel_async::RunQueryDsl;` and `use sauron_db::models::NewRoleGrant;` to the file's imports — the first two for the `.bind::<Text, _>` calls and for `get_result` / `execute`, the third for `owner_of_new_org`'s grant.

- [ ] **Step 4: Write the anti-enumeration and dead-token tests.** Append:
```rust
#[tokio::test]
async fn forgot_password_answers_identically_for_every_account_state() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("skipping: TEST_DATABASE_URL / TEST_REDIS_URL unset");
        return;
    };
    owner_of_new_org(&srv, "live@example.com", "correcthorse1").await;
    let (dead_id, _, _, _) = owner_of_new_org(&srv, "dead@example.com", "correcthorse2").await;
    {
        let mut conn = srv.conn().await;
        repo::set_user_active(&mut conn, dead_id, false)
            .await
            .expect("deactivate");
    }

    let (s1, b1) = srv
        .post_raw("/v1/auth/forgot-password", None, json!({"email":"live@example.com"}))
        .await;
    let (s2, b2) = srv
        .post_raw("/v1/auth/forgot-password", None, json!({"email":"dead@example.com"}))
        .await;
    let (s3, b3) = srv
        .post_raw("/v1/auth/forgot-password", None, json!({"email":"ghost@example.com"}))
        .await;
    assert_eq!((s1, s2, s3), (200, 200, 200));
    // Byte-identical, not merely equivalent. This is the whole contract.
    assert_eq!(b1, b2);
    assert_eq!(b2, b3);
    assert_eq!(b1, r#"{"ok":true}"#);

    let (s4, _) = srv
        .post_raw("/v1/auth/forgot-password", None, json!({"email":"no-at-sign"}))
        .await;
    assert_eq!(s4, 400, "shape validation may differ; it leaks nothing");

    // The discard branch commits nothing: an unknown address writes zero rows
    // to either table.
    let mut conn = srv.conn().await;
    let ghost_mail: i64 = diesel::sql_query(
        "SELECT count(*)::bigint AS n FROM mail_outbox WHERE recipient = 'ghost@example.com'",
    )
    .get_result::<CountRow>(&mut conn)
    .await
    .expect("count")
    .n;
    assert_eq!(ghost_mail, 0);
    drop(conn);

    srv.shutdown().await;
}

#[derive(diesel::QueryableByName)]
struct CountRow {
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    n: i64,
}

#[tokio::test]
async fn every_dead_token_state_answers_identically() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("skipping: TEST_DATABASE_URL / TEST_REDIS_URL unset");
        return;
    };
    let (user_id, _, _, _) = owner_of_new_org(&srv, "dead-token@example.com", "correcthorse1").await;

    let mut bodies: Vec<(u16, String)> = Vec::new();

    // 1. Never existed.
    bodies.push(
        srv.post_raw(
            "/v1/auth/reset-password",
            None,
            json!({"token": "f".repeat(64), "new_password": "brandnewpass1"}),
        )
        .await,
    );

    // 2. Consumed. Mint, use once, use again.
    srv.post_raw(
        "/v1/auth/forgot-password",
        None,
        json!({"email":"dead-token@example.com"}),
    )
    .await;
    let t2 = newest_reset_token_from_mail(&srv, "dead-token@example.com").await;
    let (ok, _) = srv
        .post_raw(
            "/v1/auth/reset-password",
            None,
            json!({"token": t2, "new_password": "brandnewpass1"}),
        )
        .await;
    assert_eq!(ok, 200);
    bodies.push(
        srv.post_raw(
            "/v1/auth/reset-password",
            None,
            json!({"token": t2, "new_password": "brandnewpass2"}),
        )
        .await,
    );

    // 3. Invalidated.
    let raw3 = sauron_core::ids::opaque_token();
    {
        let mut conn = srv.conn().await;
        let user = repo::get_user(&mut conn, user_id).await.unwrap().unwrap();
        repo::insert_password_reset_token(
            &mut conn,
            user_id,
            sauron_auth::hash_token(&raw3),
            sauron_auth::hash_token(&user.password_hash),
            Utc::now() + ChronoDuration::hours(1),
            "self",
            None,
            None,
        )
        .await
        .expect("insert");
        repo::invalidate_password_reset_tokens_for_user(&mut conn, user_id, "superseded")
            .await
            .expect("invalidate");
    }
    bodies.push(
        srv.post_raw(
            "/v1/auth/reset-password",
            None,
            json!({"token": raw3, "new_password": "brandnewpass3"}),
        )
        .await,
    );

    // 4. Expired.
    let raw4 = sauron_core::ids::opaque_token();
    {
        let mut conn = srv.conn().await;
        let user = repo::get_user(&mut conn, user_id).await.unwrap().unwrap();
        repo::insert_password_reset_token(
            &mut conn,
            user_id,
            sauron_auth::hash_token(&raw4),
            sauron_auth::hash_token(&user.password_hash),
            Utc::now() - ChronoDuration::minutes(1),
            "self",
            None,
            None,
        )
        .await
        .expect("insert");
    }
    bodies.push(
        srv.post_raw(
            "/v1/auth/reset-password",
            None,
            json!({"token": raw4, "new_password": "brandnewpass4"}),
        )
        .await,
    );

    // 5. Stale fingerprint — the one that proves the column earns its keep.
    let raw5 = sauron_core::ids::opaque_token();
    {
        let mut conn = srv.conn().await;
        let user = repo::get_user(&mut conn, user_id).await.unwrap().unwrap();
        repo::insert_password_reset_token(
            &mut conn,
            user_id,
            sauron_auth::hash_token(&raw5),
            sauron_auth::hash_token(&user.password_hash),
            Utc::now() + ChronoDuration::hours(1),
            "self",
            None,
            None,
        )
        .await
        .expect("insert");
        let other = sauron_auth::hash_password_async("adifferentpass1".to_string())
            .await
            .expect("hash");
        repo::set_user_password(&mut conn, user_id, &other)
            .await
            .expect("move the password out from under the link");
    }
    bodies.push(
        srv.post_raw(
            "/v1/auth/reset-password",
            None,
            json!({"token": raw5, "new_password": "brandnewpass5"}),
        )
        .await,
    );

    for (i, (status, body)) in bodies.iter().enumerate() {
        assert_eq!(*status, 401, "dead-token case {i}: {body}");
        assert_eq!(
            body, &bodies[0].1,
            "dead-token case {i} must be byte-identical to case 0"
        );
    }
    assert!(bodies[0].1.contains("invalid_token"));

    // The other half of the compare-and-swap contract, and the half a 401 alone
    // does not prove: the third password — the one that moved under case 5's
    // link — is still the account's. An implementation that consumed the link
    // and wrote anyway would 401 here just the same and be silently wrong.
    let (s_third, b_third) = srv
        .post_raw(
            "/v1/auth/login",
            None,
            json!({"email":"dead-token@example.com","password":"adifferentpass1"}),
        )
        .await;
    assert_eq!(s_third, 200, "the password set out from under the link stands: {b_third}");

    srv.shutdown().await;
}
```

- [ ] **Step 5: Write the happy-path, concurrency and reuse tests.** Append:
```rust
#[tokio::test]
async fn a_consumed_link_sets_the_password_and_kills_every_session() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("skipping: TEST_DATABASE_URL / TEST_REDIS_URL unset");
        return;
    };
    let (_id, _org, _access, refresh) = owner_of_new_org(&srv, "happy@example.com", "correcthorse1").await;

    srv.post_raw("/v1/auth/forgot-password", None, json!({"email":"happy@example.com"}))
        .await;
    let token = newest_reset_token_from_mail(&srv, "happy@example.com").await;

    let (status, body) = srv
        .post_raw(
            "/v1/auth/reset-password",
            None,
            json!({"token": token, "new_password": "thebrandnewone1"}),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    // No auto-login: the caller proved control of a mailbox, not of a credential.
    assert_eq!(body, r#"{"ok":true}"#);
    assert!(!body.contains("access_token") && !body.contains("refresh_token"));

    let (s_new, _) = srv
        .post_raw(
            "/v1/auth/login",
            None,
            json!({"email":"happy@example.com","password":"thebrandnewone1"}),
        )
        .await;
    assert_eq!(s_new, 200);
    let (s_old, _) = srv
        .post_raw(
            "/v1/auth/login",
            None,
            json!({"email":"happy@example.com","password":"correcthorse1"}),
        )
        .await;
    assert_eq!(s_old, 401);
    let (s_refresh, _) = srv
        .post_raw("/v1/auth/refresh", None, json!({"refresh_token": refresh}))
        .await;
    assert_eq!(s_refresh, 401, "a refresh token captured before the reset must be dead");

    srv.shutdown().await;
}

#[tokio::test]
async fn two_simultaneous_resets_with_one_token_yield_exactly_one_success() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("skipping: TEST_DATABASE_URL / TEST_REDIS_URL unset");
        return;
    };
    owner_of_new_org(&srv, "race@example.com", "correcthorse1").await;
    srv.post_raw("/v1/auth/forgot-password", None, json!({"email":"race@example.com"}))
        .await;
    let token = newest_reset_token_from_mail(&srv, "race@example.com").await;

    // A SELECT-then-UPDATE implementation fails this.
    let (a, b) = tokio::join!(
        srv.post_raw(
            "/v1/auth/reset-password",
            None,
            json!({"token": token, "new_password": "racewinnerpass1"}),
        ),
        srv.post_raw(
            "/v1/auth/reset-password",
            None,
            json!({"token": token, "new_password": "racewinnerpass2"}),
        )
    );
    let mut codes = [a.0, b.0];
    codes.sort_unstable();
    assert_eq!(codes, [200, 401], "got {a:?} and {b:?}");

    srv.shutdown().await;
}

#[tokio::test]
async fn resetting_to_the_current_password_is_400_and_does_not_burn_the_link() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("skipping: TEST_DATABASE_URL / TEST_REDIS_URL unset");
        return;
    };
    owner_of_new_org(&srv, "reuse@example.com", "correcthorse1").await;
    srv.post_raw("/v1/auth/forgot-password", None, json!({"email":"reuse@example.com"}))
        .await;
    let token = newest_reset_token_from_mail(&srv, "reuse@example.com").await;

    let (s1, b1) = srv
        .post_raw(
            "/v1/auth/reset-password",
            None,
            json!({"token": token, "new_password": "correcthorse1"}),
        )
        .await;
    assert_eq!(s1, 400, "{b1}");
    assert!(b1.contains("must be different from the current one"));

    // The same token still works with a different password.
    let (s2, b2) = srv
        .post_raw(
            "/v1/auth/reset-password",
            None,
            json!({"token": token, "new_password": "somethingelse1"}),
        )
        .await;
    assert_eq!(s2, 200, "{b2}");

    srv.shutdown().await;
}
```

- [ ] **Step 6: Write the admin-route tests.** Append:
```rust
#[tokio::test]
async fn admin_reset_stops_the_old_password_at_the_login_form() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("skipping: TEST_DATABASE_URL / TEST_REDIS_URL unset");
        return;
    };
    let (_owner_id, org_id, owner_token, _) =
        owner_of_new_org(&srv, "owner@example.com", "correcthorse1").await;
    let target_id =
        create_member(&srv, org_id, "target@example.com", "correcthorse2", "Viewer").await;

    // A live session, so the revocation below has something to kill. The target
    // is created straight in the database, so this is the only place a refresh
    // token for them comes from.
    let (_target_access, target_refresh) =
        login(&srv, "target@example.com", "correcthorse2").await;

    let (status, body) = srv
        .post_raw(
            &format!("/v1/orgs/{org_id}/members/{target_id}/password-reset"),
            Some(&owner_token),
            json!({"action":"reset"}),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"action\":\"reset\""));
    // The response must never carry the token or the link.
    assert!(!body.contains("token"));

    {
        let mut conn = srv.conn().await;
        let u = repo::get_user(&mut conn, target_id).await.unwrap().unwrap();
        assert!(u.must_change_password);
        assert!(u.credentials_invalidated_at.is_some());
    }

    let (s_refresh, _) = srv
        .post_raw("/v1/auth/refresh", None, json!({"refresh_token": target_refresh}))
        .await;
    assert_eq!(s_refresh, 401);

    // THE assertion. An implementation that merely gates the session passes
    // every other line in this file.
    let (s_login, b_login) = srv
        .post_raw(
            "/v1/auth/login",
            None,
            json!({"email":"target@example.com","password":"correcthorse2"}),
        )
        .await;
    assert_eq!(s_login, 403, "{b_login}");
    assert!(b_login.contains("password_reset_required"));
    assert!(!b_login.contains("access_token"));

    // The emailed link clears both the flag and the invalidation in one write.
    let token = newest_reset_token_from_mail(&srv, "target@example.com").await;
    let (s_reset, b_reset) = srv
        .post_raw(
            "/v1/auth/reset-password",
            None,
            json!({"token": token, "new_password": "chosenbyme1234"}),
        )
        .await;
    assert_eq!(s_reset, 200, "{b_reset}");
    let (s_after, b_after) = srv
        .post_raw(
            "/v1/auth/login",
            None,
            json!({"email":"target@example.com","password":"chosenbyme1234"}),
        )
        .await;
    assert_eq!(s_after, 200, "{b_after}");
    let v: Value = serde_json::from_str(&b_after).unwrap();
    let access = v["access_token"].as_str().unwrap();
    assert_eq!(
        srv.get_status("/v1/me", access).await,
        200,
        "the reset must have cleared must_change_password too"
    );

    srv.shutdown().await;
}

#[tokio::test]
async fn admin_cancel_restores_login_but_keeps_the_change_demand() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("skipping: TEST_DATABASE_URL / TEST_REDIS_URL unset");
        return;
    };
    let (_owner_id, org_id, owner_token, _) =
        owner_of_new_org(&srv, "owner2@example.com", "correcthorse1").await;
    let target_id =
        create_member(&srv, org_id, "target2@example.com", "correcthorse2", "Viewer").await;

    srv.post_raw(
        &format!("/v1/orgs/{org_id}/members/{target_id}/password-reset"),
        Some(&owner_token),
        json!({"action":"reset"}),
    )
    .await;
    let stale = newest_reset_token_from_mail(&srv, "target2@example.com").await;

    let (status, body) = srv
        .post_raw(
            &format!("/v1/orgs/{org_id}/members/{target_id}/password-reset"),
            Some(&owner_token),
            json!({"action":"cancel"}),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"expires_at\":null"));

    {
        let mut conn = srv.conn().await;
        let u = repo::get_user(&mut conn, target_id).await.unwrap().unwrap();
        assert!(u.credentials_invalidated_at.is_none());
        // Cancel does not pretend the admin never had a reason, and it cannot
        // tell this flag apart from one a temp password set long before.
        assert!(u.must_change_password);
    }

    let (s_login, b_login) = srv
        .post_raw(
            "/v1/auth/login",
            None,
            json!({"email":"target2@example.com","password":"correcthorse2"}),
        )
        .await;
    assert_eq!(s_login, 200, "{b_login}");
    let v: Value = serde_json::from_str(&b_login).unwrap();
    assert_eq!(
        srv.get_status("/v1/me", v["access_token"].as_str().unwrap())
            .await,
        403,
        "the change demand survives a cancel"
    );

    // The link the cancelled reset issued is dead.
    let (s_stale, _) = srv
        .post_raw(
            "/v1/auth/reset-password",
            None,
            json!({"token": stale, "new_password": "shouldnotwork1"}),
        )
        .await;
    assert_eq!(s_stale, 401);

    srv.shutdown().await;
}

#[tokio::test]
async fn admin_guard_stack_refuses_each_case_distinctly() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("skipping: TEST_DATABASE_URL / TEST_REDIS_URL unset");
        return;
    };
    let (owner_id, org_id, owner_token, _) =
        owner_of_new_org(&srv, "owner3@example.com", "correcthorse1").await;
    let member_id =
        create_member(&srv, org_id, "member3@example.com", "correcthorse2", "Viewer").await;
    let (outsider_id, _o2, _a2, _r2) = owner_of_new_org(&srv, "outsider@example.com", "correcthorse3").await;

    let path = |uid: Uuid| format!("/v1/orgs/{org_id}/members/{uid}/password-reset");

    // Self-target.
    let (s, b) = srv
        .post_raw(&path(owner_id), Some(&owner_token), json!({"action":"reset"}))
        .await;
    assert_eq!(s, 409, "{b}");

    // Unknown user id, and a user with no grant here — deliberately
    // indistinguishable. `outsider` owns their own org and holds no grant in
    // this one, which is exactly the shape the membership check refuses with 404
    // *before* the cross-org rule is reached.
    let (s, _) = srv
        .post_raw(&path(Uuid::new_v4()), Some(&owner_token), json!({"action":"reset"}))
        .await;
    assert_eq!(s, 404);
    let (s, _) = srv
        .post_raw(&path(outsider_id), Some(&owner_token), json!({"action":"reset"}))
        .await;
    assert_eq!(s, 404);

    // A caller with `member:read` only.
    let (member_token, _) = login(&srv, "member3@example.com", "correcthorse2").await;
    let (s, _) = srv
        .post_raw(&path(owner_id), Some(&member_token), json!({"action":"reset"}))
        .await;
    assert_eq!(s, 403);

    // A caller holding `member:manage` but NOT `member:credential`. This is the
    // assertion that proves the route moved to the new permission rather than
    // merely mentioning it: delete `authorize_org(..., perm::MEMBER_CREDENTIAL)`
    // from the handler and every other line in this test still passes.
    {
        let mut conn = srv.conn().await;
        repo::create_role(
            &mut conn,
            org_id,
            "Member manager",
            "member:manage without member:credential",
            json!([perm::MEMBER_READ, perm::MEMBER_MANAGE]),
        )
        .await
        .expect("create the carve-out role");
    }
    create_member(
        &srv,
        org_id,
        "manager3@example.com",
        "correcthorse5",
        "Member manager",
    )
    .await;
    let (manager_token, _) = login(&srv, "manager3@example.com", "correcthorse5").await;
    let (s, b) = srv
        .post_raw(&path(member_id), Some(&manager_token), json!({"action":"reset"}))
        .await;
    assert_eq!(s, 403, "member:manage must not stand in for member:credential: {b}");

    // An Admin acting on an Owner. Admin holds `member:credential` and
    // `member:manage`, so it clears the route's own gate and dies inside
    // `check_no_escalation` on the target's `org:manage` — the rule that stops
    // an Admin working through every Owner in turn.
    create_member(&srv, org_id, "admin3@example.com", "correcthorse6", "Admin").await;
    let (admin_token, _) = login(&srv, "admin3@example.com", "correcthorse6").await;
    let (s, b) = srv
        .post_raw(&path(owner_id), Some(&admin_token), json!({"action":"reset"}))
        .await;
    assert_eq!(s, 403, "an Admin may not reset an Owner: {b}");

    // Inactive target.
    {
        let mut conn = srv.conn().await;
        repo::set_user_active(&mut conn, member_id, false).await.unwrap();
    }
    let (s, b) = srv
        .post_raw(&path(member_id), Some(&owner_token), json!({"action":"reset"}))
        .await;
    assert_eq!(s, 409, "{b}");
    assert!(b.contains("reactivate this member"), "{b}");
    {
        let mut conn = srv.conn().await;
        repo::set_user_active(&mut conn, member_id, true).await.unwrap();
    }

    // Cross-org target, for BOTH actions. Last, because it is the only case that
    // has to mutate `member3` irreversibly for the rest of the file's sake: one
    // extra grant in a second org and the blanket refusal fires. `cancel` is
    // exempt from the SMTP precondition but not from this — it is a blast-radius
    // boundary, not a mail concern.
    let (_ob_id, org_b, _ob_token, _) = owner_of_new_org(&srv, "ownerb@example.com", "correcthorse4").await;
    grant_org_member(&srv, org_b, member_id).await;
    for action in ["reset", "cancel"] {
        let (s, b) = srv
            .post_raw(&path(member_id), Some(&owner_token), json!({"action": action}))
            .await;
        assert_eq!(s, 409, "cross-org {action}: {b}");
        assert!(b.contains("another organization"), "cross-org {action}: {b}");
    }

    srv.shutdown().await;
}
```
`grant_org_member` is defined in the next step. Add `use sauron_auth::perm;` to the file's imports for the custom role's permission list. Note the three token blocks go through the `login` helper from Step 3 rather than parsing a body inline.

- [ ] **Step 7: Add the grant helper, the mail-enqueue test, and the supersede and no-mail tests.** Append:
```rust
/// Give `user_id` a Viewer grant at org scope in `org_id`.
///
/// The only caller manufactures a **cross-org** target: everything else in this
/// file uses `create_member`, which creates the account and its single-org grant
/// in one statement. `repo::create_grants` takes an owned `Vec` and returns one
/// id per row.
async fn grant_org_member(srv: &TestServer, org_id: Uuid, user_id: Uuid) {
    let mut conn = srv.conn().await;
    let roles = repo::list_roles(&mut conn, org_id)
        .await
        .expect("list roles");
    let viewer = roles
        .iter()
        .find(|r| r.name == "Viewer")
        .expect("Viewer preset role");
    let ids = repo::create_grants(
        &mut conn,
        vec![NewRoleGrant {
            org_id,
            user_id,
            role_id: viewer.id,
            scope_type: "org".to_string(),
            scope_id: org_id,
        }],
    )
    .await
    .expect("grant");
    assert_eq!(ids.len(), 1, "create_grants returns one id per row");
}

#[tokio::test]
async fn each_mode_enqueues_one_mail_row_whose_expiry_matches_its_token() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("skipping: TEST_DATABASE_URL / TEST_REDIS_URL unset");
        return;
    };
    let (_owner_id, org_id, owner_token, _) =
        owner_of_new_org(&srv, "owner4@example.com", "correcthorse1").await;
    let target_id =
        create_member(&srv, org_id, "target4@example.com", "correcthorse2", "Viewer").await;

    srv.post_raw(
        "/v1/auth/forgot-password",
        None,
        json!({"email":"target4@example.com"}),
    )
    .await;
    let self_token = newest_reset_token_from_mail(&srv, "target4@example.com").await;
    assert_eq!(self_token.len(), 64);

    // Without this the admin message below is suppressed by S0's 300-second
    // per-recipient window and the counts underneath assert nothing.
    unblock_reset_mail(&srv, "target4@example.com").await;

    srv.post_raw(
        &format!("/v1/orgs/{org_id}/members/{target_id}/password-reset"),
        Some(&owner_token),
        json!({"action":"reset"}),
    )
    .await;

    let mut conn = srv.conn().await;
    let rows: i64 = diesel::sql_query(
        "SELECT count(*)::bigint AS n FROM mail_outbox \
         WHERE recipient = 'target4@example.com' AND kind = 'password_reset'",
    )
    .get_result::<CountRow>(&mut conn)
    .await
    .unwrap()
    .n;
    assert_eq!(rows, 2, "one row per mode, no more");

    // The two clocks are tied: the message and the link it carries must die
    // together, or S0's manual-requeue path blanks a body whose token is still
    // good for another twenty-three hours.
    let spans: Vec<i64> = diesel::sql_query(
        "SELECT round(extract(epoch FROM (m.expires_at - m.created_at)))::bigint AS n \
         FROM mail_outbox m WHERE m.recipient = 'target4@example.com' \
           AND m.kind = 'password_reset' ORDER BY m.created_at",
    )
    .load::<CountRow>(&mut conn)
    .await
    .unwrap()
    .into_iter()
    .map(|r| r.n)
    .collect();
    assert_eq!(spans, vec![3600, 86400]);
    drop(conn);

    srv.shutdown().await;
}

#[tokio::test]
async fn admin_resets_supersede_each_other_and_self_service_requests_do_not() {
    let Some(mut srv) = TestServer::start().await else {
        eprintln!("skipping: TEST_DATABASE_URL / TEST_REDIS_URL unset");
        return;
    };
    let (_owner_id, org_id, owner_token, _) =
        owner_of_new_org(&srv, "owner5@example.com", "correcthorse1").await;
    let target_id =
        create_member(&srv, org_id, "target5@example.com", "correcthorse2", "Viewer").await;
    let admin_path = format!("/v1/orgs/{org_id}/members/{target_id}/password-reset");

    // An admin trigger is an authoritative act by an identified principal, so
    // the second reset supersedes the first: a re-issue after a bounce must not
    // leave two live links.
    let (s1, b1) = srv
        .post_raw(&admin_path, Some(&owner_token), json!({"action":"reset"}))
        .await;
    assert_eq!(s1, 200, "{b1}");
    let first = newest_reset_token_from_mail(&srv, "target5@example.com").await;
    unblock_reset_mail(&srv, "target5@example.com").await;
    let (s2, b2) = srv
        .post_raw(&admin_path, Some(&owner_token), json!({"action":"reset"}))
        .await;
    assert_eq!(s2, 200, "{b2}");
    let second = newest_reset_token_from_mail(&srv, "target5@example.com").await;
    assert_ne!(first, second, "the second reset must mint a new link");

    let (s_first, _) = srv
        .post_raw(
            "/v1/auth/reset-password",
            None,
            json!({"token": first, "new_password": "supersededpass1"}),
        )
        .await;
    assert_eq!(s_first, 401, "the superseded link must be dead");
    let (s_second, b_second) = srv
        .post_raw(
            "/v1/auth/reset-password",
            None,
            json!({"token": second, "new_password": "thesecondlink1"}),
        )
        .await;
    assert_eq!(s_second, 200, "{b_second}");

    // Self-service is the opposite rule, deliberately: an attacker spamming
    // forgot-password against a known address would otherwise kill the link the
    // victim is about to click, turning the anti-abuse limiter into the abuse.
    owner_of_new_org(&srv, "selfsup@example.com", "correcthorse3").await;
    srv.post_raw(
        "/v1/auth/forgot-password",
        None,
        json!({"email":"selfsup@example.com"}),
    )
    .await;
    let link_a = newest_reset_token_from_mail(&srv, "selfsup@example.com").await;
    unblock_reset_mail(&srv, "selfsup@example.com").await;
    srv.post_raw(
        "/v1/auth/forgot-password",
        None,
        json!({"email":"selfsup@example.com"}),
    )
    .await;
    let link_b = newest_reset_token_from_mail(&srv, "selfsup@example.com").await;
    assert_ne!(link_a, link_b);
    {
        let mut conn = srv.conn().await;
        for raw in [&link_a, &link_b] {
            assert!(
                repo::find_live_password_reset_token(&mut conn, &sauron_auth::hash_token(raw))
                    .await
                    .expect("lookup")
                    .is_some(),
                "a self-service request must not invalidate an outstanding link"
            );
        }
    }

    // Consuming one kills the other — via the sibling sweep and, independently,
    // via the fingerprint.
    let (s_a, b_a) = srv
        .post_raw(
            "/v1/auth/reset-password",
            None,
            json!({"token": link_a, "new_password": "thefirstlink12"}),
        )
        .await;
    assert_eq!(s_a, 200, "{b_a}");
    let (s_b, _) = srv
        .post_raw(
            "/v1/auth/reset-password",
            None,
            json!({"token": link_b, "new_password": "thesecondlink2"}),
        )
        .await;
    assert_eq!(s_b, 401, "consuming one self-service link kills its siblings");

    srv.shutdown().await;
}

#[tokio::test]
async fn with_no_mail_configured_reset_refuses_and_cancel_still_works() {
    let Some(mut srv) = TestServer::start_without_mail().await else {
        eprintln!("skipping: TEST_DATABASE_URL / TEST_REDIS_URL unset");
        return;
    };
    let (_owner_id, org_id, owner_token, _) =
        owner_of_new_org(&srv, "owner6@example.com", "correcthorse1").await;
    let target_id =
        create_member(&srv, org_id, "target6@example.com", "correcthorse2", "Viewer").await;
    let path = format!("/v1/orgs/{org_id}/members/{target_id}/password-reset");

    let (s_reset, b_reset) = srv
        .post_raw(&path, Some(&owner_token), json!({"action":"reset"}))
        .await;
    assert_eq!(s_reset, 503, "{b_reset}");
    assert!(b_reset.contains("unavailable"), "{b_reset}");

    // Nothing applied. The 503 sits above every write for exactly this reason: a
    // destructive change must never land when the message carrying its remedy
    // cannot be sent.
    {
        let mut conn = srv.conn().await;
        let u = repo::get_user(&mut conn, target_id).await.unwrap().unwrap();
        assert!(u.credentials_invalidated_at.is_none());
        assert!(!u.must_change_password);
    }

    // Cancel is exempt. This is the assertion that stops the 503 check being
    // hoisted above the action parse in a tidy-up — gating the undo on the
    // configuration that motivates it makes it unreachable in precisely the
    // deployment that needs it.
    let (s_cancel, b_cancel) = srv
        .post_raw(&path, Some(&owner_token), json!({"action":"cancel"}))
        .await;
    assert_eq!(s_cancel, 200, "{b_cancel}");
    assert!(b_cancel.contains("\"action\":\"cancel\""), "{b_cancel}");

    // And a bad action is still a 400 here, not a 503: the parse runs first.
    let (s_bad, b_bad) = srv
        .post_raw(&path, Some(&owner_token), json!({"action":"nonsense"}))
        .await;
    assert_eq!(s_bad, 400, "{b_bad}");

    // forgot-password keeps its generic 200 on this deployment and writes
    // nothing. A status that flips with deployment configuration is a
    // config-state oracle handed to an anonymous caller.
    let (s_forgot, b_forgot) = srv
        .post_raw(
            "/v1/auth/forgot-password",
            None,
            json!({"email":"target6@example.com"}),
        )
        .await;
    assert_eq!(s_forgot, 200, "{b_forgot}");
    assert_eq!(b_forgot, r#"{"ok":true}"#);
    {
        let mut conn = srv.conn().await;
        let queued: i64 = diesel::sql_query(
            "SELECT count(*)::bigint AS n FROM mail_outbox WHERE kind = 'password_reset'",
        )
        .get_result::<CountRow>(&mut conn)
        .await
        .expect("count")
        .n;
        assert_eq!(queued, 0, "an unconfigured deployment must enqueue nothing");
    }

    srv.shutdown().await;
}
```
`NewRoleGrant`, `Text` and `RunQueryDsl` were already imported in Step 3; nothing new is needed here.

- [ ] **Step 8: Compile the test binary and see it build.** Run:
```
cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo check -p sauron-api --all-targets
```
Expect `Finished`. The only repo functions this file calls that S1 does not itself add are `create_user`, `create_org`, `get_system_role`, `create_grant`, `create_grants`, `create_role`, `create_member_with_grants`, `list_roles`, `get_user` and `set_user_active`, all of which ship today — do not add a new repo function for the test.

- [ ] **Step 9: Run the suite with no database and see it skip.** Run:
```
cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo test -p sauron-api --test http_password_reset
```
Expect every test to pass instantly with `skipping: TEST_DATABASE_URL / TEST_REDIS_URL unset` on stdout — CI has no Postgres service and must stay green.

- [ ] **Step 10: Run the suite against a real database and see it pass.** Run:
```
cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron TEST_REDIS_URL=redis://172.20.0.3:6379 cargo test -p sauron-api --test http_password_reset -- --test-threads=2
```
Expect `test result: ok. 11 passed`. Each test provisions and drops its own ephemeral database; if one panics, its `Drop` prints the database name to drop by hand.

- [ ] **Step 11: Format and lint.** Run:
```
cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings
```
Expect both to exit 0.

---

### Task 11: Dashboard models and pure decision logic

**Files:**
- Create `dashboard/src/lib/models/password-reset.ts`
- Create `dashboard/src/lib/models/password-reset.test.ts`
- Modify `dashboard/src/lib/models/index.ts` (`MemberGrant` at 236-248; `Member` at 256-262; `groupMembers` at 317-334)

**Interfaces:**
- Consumes: `isNormalizedError` from `dashboard/src/lib/api/client.ts`.
- Produces:
  - `MemberGrant.credentials_invalidated_at: string | null`, `Member.credentials_invalidated_at: string | null`
  - `interface MemberPasswordResetResult { ok: boolean; action: 'reset' | 'cancel'; expires_at: string | null }`
  - `readResetToken(qs: string | null): string | null`
  - `passwordRules(next: string, confirm: string): { tooShort: boolean; mismatch: boolean; canSubmit: boolean }`
  - `isPasswordResetRequired(err: unknown): boolean`
  - `canResetMemberPassword(member: Member, currentUserId: string, canCredential: boolean): boolean`
  - `canCancelPasswordReset(member: Member, currentUserId: string, canCredential: boolean): boolean`

- [ ] **Step 1: Write the failing test file.** Create `dashboard/src/lib/models/password-reset.test.ts`:
```ts
import { describe, expect, it } from 'vitest';
import {
  canCancelPasswordReset,
  canResetMemberPassword,
  isPasswordResetRequired,
  passwordRules,
  readResetToken,
} from './password-reset';
import type { Member, MemberGrant } from './index';

// Fixtures are `Member`, not `MemberGrant`. `MembersTable.svelte` iterates
// `grouped: Member[]`, and structural typing means the wrong annotation
// compiles — the predicates would then be documented and unit-tested against a
// shape the caller never passes.
function grant(overrides: Partial<MemberGrant> = {}): MemberGrant {
  return {
    id: 'g1',
    user_id: 'u1',
    email: 'ada@example.com',
    name: 'Ada',
    role_id: 'r1',
    role_name: 'Viewer',
    scope_type: 'org',
    scope_id: 'o1',
    is_active: true,
    credentials_invalidated_at: null,
    ...overrides,
  };
}

function member(overrides: Partial<Member> = {}): Member {
  return {
    user_id: 'u1',
    email: 'ada@example.com',
    name: 'Ada',
    is_active: true,
    credentials_invalidated_at: null,
    grants: [grant()],
    ...overrides,
  };
}

describe('readResetToken', () => {
  it('reads the token', () => {
    expect(readResetToken('token=abc')).toBe('abc');
  });
  it('reads it from among other params', () => {
    expect(readResetToken('a=1&token=abc&b=2')).toBe('abc');
  });
  it('decodes a percent-encoded value', () => {
    expect(readResetToken('token=a%2Bb')).toBe('a+b');
  });
  it('treats an empty value as absent', () => {
    expect(readResetToken('token=')).toBeNull();
  });
  it('handles an empty and a null query string', () => {
    expect(readResetToken('')).toBeNull();
    expect(readResetToken(null)).toBeNull();
  });
});

describe('passwordRules', () => {
  it('flags a short password only once something is typed', () => {
    expect(passwordRules('', '').tooShort).toBe(false);
    expect(passwordRules('abc', '').tooShort).toBe(true);
  });
  it('flags a mismatch only once the confirm field is touched', () => {
    expect(passwordRules('correcthorse', '').mismatch).toBe(false);
    expect(passwordRules('correcthorse', 'correcthors').mismatch).toBe(true);
  });
  it('allows submit only when both fields agree and are long enough', () => {
    expect(passwordRules('correcthorse', 'correcthorse').canSubmit).toBe(true);
    expect(passwordRules('short', 'short').canSubmit).toBe(false);
    expect(passwordRules('correcthorse', 'other').canSubmit).toBe(false);
  });
});

describe('isPasswordResetRequired', () => {
  it('matches the real error shape', () => {
    expect(
      isPasswordResetRequired({
        status: 403,
        code: 'password_reset_required',
        message: 'x',
        isNetwork: false,
      }),
    ).toBe(true);
  });
  it('does not match the temp-password gate, which the two names invite', () => {
    expect(
      isPasswordResetRequired({
        status: 403,
        code: 'password_change_required',
        message: 'x',
        isNetwork: false,
      }),
    ).toBe(false);
  });
  it('does not match a non-error', () => {
    expect(isPasswordResetRequired(new Error('boom'))).toBe(false);
    expect(isPasswordResetRequired(null)).toBe(false);
  });
});

describe('canResetMemberPassword / canCancelPasswordReset', () => {
  it('offers reset for an ordinary active member', () => {
    expect(canResetMemberPassword(member(), 'me', true)).toBe(true);
    expect(canCancelPasswordReset(member(), 'me', true)).toBe(false);
  });
  it('offers neither without the permission', () => {
    expect(canResetMemberPassword(member(), 'me', false)).toBe(false);
    expect(canCancelPasswordReset(member({ credentials_invalidated_at: 'x' }), 'me', false)).toBe(
      false,
    );
  });
  it('offers neither for yourself — the server answers 409', () => {
    expect(canResetMemberPassword(member({ user_id: 'me' }), 'me', true)).toBe(false);
    expect(
      canCancelPasswordReset(member({ user_id: 'me', credentials_invalidated_at: 'x' }), 'me', true),
    ).toBe(false);
  });
  it('offers neither for a deactivated member — the server answers 409', () => {
    expect(canResetMemberPassword(member({ is_active: false }), 'me', true)).toBe(false);
    expect(
      canCancelPasswordReset(member({ is_active: false, credentials_invalidated_at: 'x' }), 'me', true),
    ).toBe(false);
  });
  it('swaps reset for cancel once one is pending', () => {
    const pending = member({ credentials_invalidated_at: '2026-08-01T00:00:00Z' });
    expect(canResetMemberPassword(pending, 'me', true)).toBe(false);
    expect(canCancelPasswordReset(pending, 'me', true)).toBe(true);
  });
  it('never offers both — the row carries one menu item, not two that contradict', () => {
    for (const m of [
      member(),
      member({ credentials_invalidated_at: 'x' }),
      member({ is_active: false }),
      member({ user_id: 'me' }),
    ]) {
      expect(canResetMemberPassword(m, 'me', true) && canCancelPasswordReset(m, 'me', true)).toBe(
        false,
      );
    }
  });
});
```

- [ ] **Step 2: Run it and see it fail.** Run:
```
cd /home/splimter/projects/freelance/sauron/dashboard && npm run test
```
Expect `Failed to resolve import "./password-reset"`.

- [ ] **Step 3: Add the model fields.** In `dashboard/src/lib/models/index.ts`, add to `interface MemberGrant`, after `is_active: boolean;`:
```ts
  /** Non-null while an admin-forced reset is outstanding. Comes from
      `GET /v1/orgs/{org}/members`, the only place the dashboard learns anything
      about a member's account state. */
  credentials_invalidated_at: string | null;
```
Add the same field to `interface Member`, after `is_active: boolean;`. Then carry it in `groupMembers`, in the `byUser.set(...)` object literal after `is_active: g.is_active,`:
```ts
        credentials_invalidated_at: g.credentials_invalidated_at,
```
And add, beside the other member payload types:
```ts
export interface MemberPasswordResetResult {
  ok: boolean;
  action: 'reset' | 'cancel';
  /** RFC 3339 when the link expires; null for `cancel`. Never a token — the
      server refuses to return the link under any condition. */
  expires_at: string | null;
}
```

- [ ] **Step 4: Write the model.** Create `dashboard/src/lib/models/password-reset.ts`:
```ts
import { isNormalizedError } from '../api/client';
import type { Member } from './index';

/**
 * Read the reset token out of a hash-fragment query string.
 *
 * The token lives in the fragment precisely so it never reaches a server log,
 * a proxy log or an analytics beacon, so this is the only place it is parsed.
 */
export function readResetToken(qs: string | null): string | null {
  const raw = new URLSearchParams(qs ?? '').get('token');
  const trimmed = raw?.trim() ?? '';
  return trimmed.length > 0 ? trimmed : null;
}

export interface PasswordRules {
  tooShort: boolean;
  mismatch: boolean;
  canSubmit: boolean;
}

/**
 * ChangePassword.svelte's derivations minus `reused` — there is no current
 * password on the reset page. One definition, shared by both screens, so the
 * two cannot drift into disagreeing about what a valid password is.
 */
export function passwordRules(next: string, confirm: string): PasswordRules {
  return {
    tooShort: next.length > 0 && next.length < 8,
    mismatch: confirm.length > 0 && confirm !== next,
    canSubmit: next.length >= 8 && confirm === next,
  };
}

/**
 * True for the API's 403 `password_reset_required` — the twin of
 * `isPasswordChangeRequired` in the auth store.
 *
 * Lives here rather than in that store because the login page is its only
 * caller and the store has no reason to know.
 */
export function isPasswordResetRequired(err: unknown): boolean {
  return isNormalizedError(err) && err.status === 403 && err.code === 'password_reset_required';
}

/** An older server build omits the field entirely, so this is a truthiness
    check rather than `!== null`. */
function resetPending(member: Member): boolean {
  return Boolean(member.credentials_invalidated_at);
}

/**
 * Mirrors the server's refusals, so the action is never offered for something
 * the server will reject with a 409: self, inactive, or already pending.
 */
export function canResetMemberPassword(
  member: Member,
  currentUserId: string,
  canCredential: boolean,
): boolean {
  if (!canCredential) return false;
  if (member.user_id === currentUserId) return false;
  if (!member.is_active) return false;
  return !resetPending(member);
}

/**
 * The same three guards, but true only when a reset **is** pending. At most one
 * of the two predicates holds for a given member, which is what lets the row
 * carry one menu item instead of two that contradict each other.
 */
export function canCancelPasswordReset(
  member: Member,
  currentUserId: string,
  canCredential: boolean,
): boolean {
  if (!canCredential) return false;
  if (member.user_id === currentUserId) return false;
  if (!member.is_active) return false;
  return resetPending(member);
}
```

- [ ] **Step 5: Run the tests and see them pass.** Run:
```
cd /home/splimter/projects/freelance/sauron/dashboard && npm run test
```
Expect all `password-reset.test.ts` tests green. `group-members.test.ts` will fail to typecheck at build time on the new required field — fix its `grant()` fixture by adding `credentials_invalidated_at: null,` and re-run.

- [ ] **Step 6: Typecheck.** Run:
```
cd /home/splimter/projects/freelance/sauron/dashboard && npm run check
```
Expect 0 errors. Any error naming `credentials_invalidated_at` is a fixture or a mapper that has not been updated — fix it rather than making the field optional, because an optional field would let the members page silently never show a pending reset.

---

### Task 12: `RowActionsMenu` and the members row

**Files:**
- Create `dashboard/src/lib/components/ui/RowActionsMenu.svelte`
- Modify `dashboard/src/lib/components/members/MembersTable.svelte` (`Props` at 9-30; destructure at 32-43; row actions cell at 140-156; styles at 200+)

**Interfaces:**
- Consumes: `canResetMemberPassword`, `canCancelPasswordReset` (Task 11); `MembersTable`'s existing S2 props `canRevokeSessions: boolean`, `revokingUserId: string | null`, `onrevokesessions: (member: Member) => void`, which move from an inline button into the new menu **unchanged in name and signature**.
- Produces: `RowActionsMenu` with props `{ label: string; children: Snippet<[() => void]> }`; three new `MembersTable` props `onresetpassword: (member: Member, action: 'reset' | 'cancel') => void`, `currentUserId: string`, `canCredential: boolean`.

- [ ] **Step 1: Create the menu primitive.** Create `dashboard/src/lib/components/ui/RowActionsMenu.svelte`:
```svelte
<script lang="ts">
  import type { Snippet } from 'svelte';

  interface Props {
    /** Accessible name for the trigger, e.g. `Actions for ada@example.com`. A
        table of twenty identical "More" buttons is unusable with a screen
        reader. */
    label: string;
    /** Menu items. Receives a `close` callback: every item must call it, or the
        panel stays open over the dialog the item just opened. */
    children: Snippet<[() => void]>;
  }

  let { label, children }: Props = $props();

  let open = $state(false);
  let trigger = $state<HTMLButtonElement | null>(null);
  let panel = $state<HTMLDivElement | null>(null);

  function close(): void {
    if (!open) return;
    open = false;
    // Focus returns to the trigger. Without this a keyboard user who presses
    // Escape is dropped at the top of the document, twenty rows away from where
    // they were.
    trigger?.focus();
  }

  function onWindowPointerDown(event: PointerEvent) {
    if (!open) return;
    const target = event.target as Node | null;
    if (target && (trigger?.contains(target) || panel?.contains(target))) return;
    // No focus() on this path: the click has already moved focus somewhere
    // deliberate, and yanking it back would fight the user.
    open = false;
  }

  function onWindowKeyDown(event: KeyboardEvent) {
    if (open && event.key === 'Escape') close();
  }
</script>

<svelte:window onpointerdown={onWindowPointerDown} onkeydown={onWindowKeyDown} />

<div class="ram">
  <button
    type="button"
    class="ram-trigger"
    bind:this={trigger}
    aria-haspopup="menu"
    aria-expanded={open}
    aria-label={label}
    onclick={() => (open = !open)}
  >
    <span aria-hidden="true">⋯</span>
  </button>
  {#if open}
    <div class="ram-panel" role="menu" bind:this={panel}>
      {@render children(close)}
    </div>
  {/if}
</div>

<style>
  .ram {
    position: relative;
    display: inline-flex;
  }
  .ram-trigger {
    background: none;
    border: 1px solid transparent;
    border-radius: var(--radius);
    color: var(--text-muted);
    font-size: 16px;
    line-height: 1;
    padding: 4px 8px;
    cursor: pointer;
  }
  .ram-trigger:hover,
  .ram-trigger[aria-expanded='true'] {
    color: var(--text);
    border-color: var(--border);
  }
  .ram-panel {
    position: absolute;
    top: calc(100% + 4px);
    right: 0;
    z-index: 20;
    min-width: 190px;
    padding: 4px;
    display: flex;
    flex-direction: column;
    background: var(--bg-elevated, var(--bg));
    border: 1px solid var(--border);
    border-radius: var(--radius);
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.28);
    text-align: left;
  }
  /* The parent <td> is `white-space: nowrap` for the trigger's sake; items
     inside the panel must not inherit that or a long label is clipped. */
  .ram-panel :global(.ram-item) {
    display: block;
    width: 100%;
    padding: 7px 10px;
    background: none;
    border: none;
    border-radius: calc(var(--radius) - 2px);
    color: var(--text);
    font-size: 13px;
    text-align: left;
    white-space: normal;
    cursor: pointer;
  }
  .ram-panel :global(.ram-item:hover) {
    background: var(--bg-subtle, rgba(255, 255, 255, 0.06));
  }
  .ram-panel :global(.ram-item.danger) {
    color: var(--error);
  }
</style>
```

- [ ] **Step 2: Typecheck the new component alone.** Run:
```
cd /home/splimter/projects/freelance/sauron/dashboard && npm run check
```
Expect 0 errors. A `Snippet<[() => void]>` error means the project's Svelte version predates parameterised snippets — check `package.json` for `svelte` `^5`.

- [ ] **Step 3: Add the new props to `MembersTable`.** In `dashboard/src/lib/components/members/MembersTable.svelte`, extend the `Props` interface after `ontoggle: (member: Member) => void;`:
```ts
    /** Id of the signed-in user, for the self-check the server also makes. */
    currentUserId: string;
    /** `member:credential` AND `member:manage` — the server requires both, so a
        menu gating on either one alone offers an action the server refuses. */
    canCredential: boolean;
    /** ONE callback rather than two, so the table cannot offer a member both a
        reset and a cancel. */
    onresetpassword: (member: Member, action: 'reset' | 'cancel') => void;
```
And add `currentUserId,`, `canCredential,`, `onresetpassword,` to the `let { ... }: Props = $props();` destructure.

- [ ] **Step 4: Import the menu and the predicates.** In the same file's `<script>`, add:
```ts
  import RowActionsMenu from '../ui/RowActionsMenu.svelte';
  import { canCancelPasswordReset, canResetMemberPassword } from '../../models/password-reset';
```

- [ ] **Step 5: Replace the row-action cell.** Replace the whole `<div class="row-actions">…</div>` block inside `<td class="col-act">` with:
```svelte
                <RowActionsMenu label={`Actions for ${member.email}`}>
                  {#snippet children(close)}
                    <button
                      type="button"
                      role="menuitem"
                      class="ram-item"
                      onclick={() => {
                        close();
                        onedit(member.user_id);
                      }}>Edit</button
                    >
                    {#if canResetMemberPassword(member, currentUserId, canCredential)}
                      <button
                        type="button"
                        role="menuitem"
                        class="ram-item"
                        onclick={() => {
                          close();
                          onresetpassword(member, 'reset');
                        }}>Reset password</button
                      >
                    {:else if canCancelPasswordReset(member, currentUserId, canCredential)}
                      <button
                        type="button"
                        role="menuitem"
                        class="ram-item"
                        onclick={() => {
                          close();
                          onresetpassword(member, 'cancel');
                        }}>Cancel password reset</button
                      >
                    {/if}
                    {#if canRevokeSessions && member.user_id !== currentUserId}
                      <button
                        type="button"
                        role="menuitem"
                        class="ram-item"
                        disabled={revokingUserId === member.user_id}
                        onclick={() => {
                          close();
                          onrevokesessions(member);
                        }}>Sign out all devices</button
                      >
                    {/if}
                    <button
                      type="button"
                      role="menuitem"
                      class="ram-item danger"
                      disabled={togglingUserId === member.user_id}
                      onclick={() => {
                        close();
                        ontoggle(member);
                      }}
                    >
                      {member.is_active ? 'Deactivate' : 'Reactivate'}
                    </button>
                  {/snippet}
                </RowActionsMenu>
```
Menu order is fixed once, destructive last: **Edit / Reset password / Sign out all devices / Deactivate**. The third item is S2's inline `<Button size="sm" variant="ghost">Sign out</Button>`, folded in above verbatim except for its element and its label: same `canRevokeSessions` gate, same `revokingUserId` busy check, same `onrevokesessions(member)` callback. Two deliberate changes:

- the self-check reads S1's `currentUserId` prop rather than S2's `authStore.user?.id`, because this cell now has that value passed in and two spellings of one comparison drift;
- `loading` becomes `disabled`, because a `.ram-item` is a plain `<button>` with no spinner slot — the panel closes on click anyway, so the busy state is only ever visible if the parent re-renders it.

Remove the now-unused `import { authStore } from '../../stores/auth.svelte';` that S2 added, if nothing else in the file uses it. Delete the now-unused `.row-actions` CSS rule, and remove the `Button` import if nothing else in the file uses it.

- [ ] **Step 6: Add the pending badge.** In the same file, in the name/email cell — beside whatever renders the existing inactive marker — add:
```svelte
                {#if member.credentials_invalidated_at}
                  <!-- An account nobody can sign in to is a state the table has
                       to show without being opened: the admin who forced it may
                       not be the one fielding "I can't log in". -->
                  <Badge tone="warning" size="sm">Reset pending</Badge>
                {/if}
```

- [ ] **Step 7: Typecheck and see the one expected caller break.** Run:
```
cd /home/splimter/projects/freelance/sauron/dashboard && npm run check
```
Expect exactly one class of error, in `dashboard/src/pages/Members.svelte`: `Property 'currentUserId' is missing`, plus the same for `canCredential` and `onresetpassword`. Task 15 supplies them. To keep this task green, add the three props to the `<MembersTable … />` call now with `currentUserId={authStore.user?.id ?? ''}`, `canCredential={sessionStore.can('member:credential') && sessionStore.can('member:manage')}` and `onresetpassword={() => {}}`, importing `authStore` from `'../lib/stores/auth.svelte'`. Re-run and expect 0 errors.

- [ ] **Step 8: Run the unit tests.** Run:
```
cd /home/splimter/projects/freelance/sauron/dashboard && npm run test
```
Expect all green — this task adds no test of its own because `RowActionsMenu` and `MembersTable` are DOM components and the repo has no DOM test environment; the decision logic they call is already covered by Task 11.

---

### Task 13: The two public pages, the API client and the routing

**Files:**
- Modify `dashboard/src/lib/api/auth.ts` (append)
- Create `dashboard/src/pages/ForgotPassword.svelte`
- Create `dashboard/src/pages/ResetPassword.svelte`
- Modify `dashboard/src/routes.ts` (imports at 5-33; `routes` object at 54-58)
- Modify `dashboard/src/App.svelte` (`PUBLIC_ROUTES` at line 11)

**Interfaces:**
- Consumes: `readResetToken`, `passwordRules` (Task 11); `bareClient` from `lib/api/client`.
- Produces: `forgotPassword(email: string): Promise<void>`; `resetPassword(token: string, newPassword: string): Promise<void>`; routes `/forgot-password` and `/reset-password`.

- [ ] **Step 1: Add the two API functions.** Append to `dashboard/src/lib/api/auth.ts`:
```ts
/**
 * Both reset endpoints go through `bareClient`, not `api`: they are
 * unauthenticated, must never carry a stale bearer, and must never enter the
 * single-flight 401 refresh-and-replay loop — the same reason
 * login/register/refresh/logout use it.
 */
export async function forgotPassword(email: string): Promise<void> {
  await bareClient.post('/v1/auth/forgot-password', { email });
}

export async function resetPassword(token: string, newPassword: string): Promise<void> {
  await bareClient.post('/v1/auth/reset-password', { token, new_password: newPassword });
}
```

- [ ] **Step 2: Create `ForgotPassword.svelte`.** Create `dashboard/src/pages/ForgotPassword.svelte`:
```svelte
<script lang="ts">
  import AuthLayout from '../lib/components/layout/AuthLayout.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Input from '../lib/components/ui/Input.svelte';
  import { forgotPassword } from '../lib/api/auth';
  import { errorMessage, isNormalizedError } from '../lib/api/client';
  import { toastStore } from '../lib/stores/toast.svelte';

  let email = $state('');
  let submitting = $state(false);
  let sent = $state(false);
  let unsupported = $state(false);

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    if (submitting) return;
    submitting = true;
    try {
      await forgotPassword(email.trim());
    } catch (err) {
      // A 429 additionally toasts the server's message, and a 404 means the
      // dashboard was upgraded ahead of the server. Everything else is
      // swallowed on purpose: the API answers 200 for an unknown address, for a
      // deactivated account and for a deployment with no SMTP, so a UI that
      // reported any of those would become the oracle the API refuses to be.
      if (isNormalizedError(err) && err.status === 404) {
        unsupported = true;
        return;
      }
      if (isNormalizedError(err) && err.status === 429) {
        toastStore.error(errorMessage(err));
      }
    } finally {
      submitting = false;
      // Always the same panel, whatever happened.
      if (!unsupported) sent = true;
    }
  }
</script>

<AuthLayout title="Reset your password" subtitle="We'll email you a link to choose a new one.">
  {#if unsupported}
    <div class="panel" role="status">
      <p>
        This server does not support password reset yet — ask an administrator to finish the
        upgrade.
      </p>
    </div>
  {:else if sent}
    <div class="panel" role="status">
      <p>
        If an account exists for that address, we have sent a link to reset the password. The link
        expires in 1 hour.
      </p>
      <p class="muted">Nothing arrived? Check your spam folder, then try again in a little while.</p>
    </div>
  {:else}
    <form onsubmit={submit} class="form">
      <Input label="Email" type="email" bind:value={email} autocomplete="email" required />
      <Button type="submit" variant="primary" size="lg" fullWidth loading={submitting}>
        Email me a link
      </Button>
    </form>
  {/if}

  {#snippet footer()}
    <span><a href="#/login">Back to sign in</a></span>
  {/snippet}
</AuthLayout>

<style>
  .form {
    display: flex;
    flex-direction: column;
    gap: 16px;
  }
  .panel {
    display: flex;
    flex-direction: column;
    gap: 10px;
    font-size: 14px;
    line-height: 1.5;
  }
  .muted {
    color: var(--text-muted);
    font-size: 13px;
  }
</style>
```
The panel deliberately offers **no** route through an administrator: an admin cannot act at all for a member who holds grants in another org, and for everyone else the only admin action there is invalidates the password the person is still using.

- [ ] **Step 3: Create `ResetPassword.svelte`.** Create `dashboard/src/pages/ResetPassword.svelte`:
```svelte
<script lang="ts">
  import { querystring, replace } from 'svelte-spa-router';
  import AuthLayout from '../lib/components/layout/AuthLayout.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import Input from '../lib/components/ui/Input.svelte';
  import { resetPassword } from '../lib/api/auth';
  import { errorMessage, isNormalizedError } from '../lib/api/client';
  import { authStore } from '../lib/stores/auth.svelte';
  import { sessionStore } from '../lib/stores/session.svelte';
  import { toastStore } from '../lib/stores/toast.svelte';
  import { passwordRules, readResetToken } from '../lib/models/password-reset';

  // Read ONCE at init, not reactively, so a later navigation cannot swap the
  // token mid-submit. Same house pattern as Issues.svelte.
  const token = readResetToken($querystring);

  let newPassword = $state('');
  let confirmPassword = $state('');
  let submitting = $state(false);
  let error = $state<string | null>(null);
  let deadLink = $state(false);

  const rules = $derived(passwordRules(newPassword, confirmPassword));

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    if (!token || !rules.canSubmit || submitting) return;
    error = null;
    submitting = true;
    try {
      await resetPassword(token, newPassword);
      toastStore.success('Password updated. Sign in with your new password.');
      // These three statements, in this order. `replace('/login')` alone is a
      // no-op for the visitor this page most needs to handle: App.svelte pushes
      // `authStore.isAuthenticated` visitors to /issues, and `isAuthenticated`
      // is pure local state untouched by a reset that happened server-side. A
      // user already signed in in another tab would otherwise be bounced into
      // /issues on a session the backend just revoked, never see the login
      // screen, and only be ejected when refresh() fails.
      await authStore.logout();
      sessionStore.reset();
      replace('/login');
    } catch (err) {
      if (isNormalizedError(err) && err.status === 401) {
        deadLink = true;
        return;
      }
      error = errorMessage(err);
    } finally {
      submitting = false;
    }
  }
</script>

<AuthLayout title="Choose a new password">
  {#if !token || deadLink}
    <div class="panel" role="status">
      <p>This reset link is invalid or has expired — request a new one.</p>
      <p><a href="#/forgot-password">Email me a new link</a></p>
    </div>
  {:else}
    <form onsubmit={submit} class="form">
      {#if error}<div class="alert" role="alert">{error}</div>{/if}
      <Input
        label="New password"
        type="password"
        bind:value={newPassword}
        autocomplete="new-password"
        hint={rules.tooShort ? undefined : 'At least 8 characters.'}
        error={rules.tooShort ? 'Must be at least 8 characters.' : undefined}
        required
      />
      <Input
        label="Confirm new password"
        type="password"
        bind:value={confirmPassword}
        autocomplete="new-password"
        error={rules.mismatch ? 'Passwords do not match.' : undefined}
        required
      />
      <Button
        type="submit"
        variant="primary"
        size="lg"
        fullWidth
        disabled={!rules.canSubmit}
        loading={submitting}
      >
        Set new password
      </Button>
    </form>
  {/if}

  {#snippet footer()}
    <span><a href="#/login">Back to sign in</a></span>
  {/snippet}
</AuthLayout>

<style>
  .form {
    display: flex;
    flex-direction: column;
    gap: 16px;
  }
  .panel {
    display: flex;
    flex-direction: column;
    gap: 10px;
    font-size: 14px;
    line-height: 1.5;
  }
  .alert {
    padding: 10px 12px;
    border-radius: var(--radius);
    background: var(--error-soft);
    border: 1px solid color-mix(in srgb, var(--error) 30%, transparent);
    color: var(--error);
    font-size: 13px;
  }
</style>
```

- [ ] **Step 4: Register the routes.** In `dashboard/src/routes.ts`, add the imports beside the other page imports:
```ts
import ForgotPassword from './pages/ForgotPassword.svelte';
import ResetPassword from './pages/ResetPassword.svelte';
```
And add to the `routes` object, immediately after `'/register': Register,`:
```ts
  // Both are BARE — no wrap, no guarded(), no `authed` or `passwordCurrent`
  // condition. Wrapping either would fire conditionsFailed, which pushes to
  // /login or /change-password and makes a reset link unusable.
  '/forgot-password': ForgotPassword,
  '/reset-password': ResetPassword,
```

- [ ] **Step 5: Update `PUBLIC_ROUTES`.** In `dashboard/src/App.svelte`, change line 11 to:
```ts
  // '/reset-password' is DELIBERATELY absent. This array feeds the $effect
  // below that pushes authenticated users to /issues, and a logged-in user
  // clicking their own reset link would be bounced off it before they could use
  // it. It is neither listed here nor guarded in routes.ts, so it simply
  // renders for everyone. '/forgot-password' IS listed: an authenticated user
  // who lands there wants Change password instead.
  //
  // Note `$location` from svelte-spa-router excludes the query string, so the
  // comparison is '/reset-password' even with ?token=… — which is exactly why
  // omitting it here is load-bearing rather than accidental.
  const PUBLIC_ROUTES = ['/login', '/register', '/forgot-password'];
```

- [ ] **Step 6: Typecheck and test.** Run:
```
cd /home/splimter/projects/freelance/sauron/dashboard && npm run check && npm run test
```
Expect 0 errors and all tests green.

- [ ] **Step 7: Drive both pages in a browser.** Start the API as in Task 5 Step 5 (with `SMTP_SINK=1` and `DASHBOARD_URL=http://localhost:5173`) and the dashboard:
```
cd /home/splimter/projects/freelance/sauron/dashboard && npm run dev
```
Visit `http://localhost:5173/#/forgot-password`, submit a **real** address and then a **fake** one, and confirm the confirmation panel is byte-identical in both cases. Copy the reset URL from the API's `SMTP_SINK` log output, open it, and confirm the form renders. Visit `http://localhost:5173/#/reset-password` with no `?token=` and confirm the invalid-link panel renders with no form. Finally, sign in in a second tab, then submit the reset in the first: confirm it lands on `#/login` and does **not** bounce to `/issues`.

---

### Task 14: The login page

**Files:**
- Modify `dashboard/src/pages/Login.svelte` (script at 1-39; form at 42-69)

**Interfaces:**
- Consumes: `isPasswordResetRequired` (Task 11).
- Produces: nothing other tasks read.

- [ ] **Step 1: Add the import and the state.** In `dashboard/src/pages/Login.svelte`, add to the `<script>` imports:
```ts
  import { isPasswordResetRequired } from '../lib/models/password-reset';
```
And beside the other `$state` declarations:
```ts
  /** The address the caller typed, held only once a reset refusal proves they
      know its password. */
  let resetRequiredFor = $state<string | null>(null);
```

- [ ] **Step 2: Branch in the catch arm.** Change `submit`'s `catch` block to:
```ts
    } catch (err) {
      // Rendering this as a red form error is not enough. The target of an
      // admin-forced reset would otherwise see "an administrator reset this
      // password" in the same box as a typo'd password, from the same screen
      // they have just been told to stop using. The store branches the same way
      // on password_change_required.
      if (isPasswordResetRequired(err)) {
        resetRequiredFor = email.trim();
        return;
      }
      error = errorMessage(err);
    } finally {
```

- [ ] **Step 3: Render the panel.** In the same file, replace the whole `<form onsubmit={submit} class="form"> … </form>` block inside `AuthLayout` (Login.svelte:43-64) with the branch below. The form's contents are reproduced verbatim; only the wrapper is new:
```svelte
  {#if resetRequiredFor}
    <div class="panel" role="status">
      <p>
        An administrator reset the password for this account. We have emailed
        <strong>{resetRequiredFor}</strong> a link to set a new one.
      </p>
      <p class="muted">
        Nothing arrived? Check your spam folder, or ask the administrator to send it again.
      </p>
    </div>
  {:else}
    <form onsubmit={submit} class="form">
      {#if error}<div class="alert" role="alert">{error}</div>{/if}
      <Input
        label="Email"
        type="email"
        bind:value={email}
        placeholder="you@company.com"
        autocomplete="email"
        required
      />
      <Input
        label="Password"
        type="password"
        bind:value={password}
        placeholder="••••••••"
        autocomplete="current-password"
        required
      />
      <Button type="submit" variant="primary" size="lg" fullWidth loading={submitting}>
        Sign in
      </Button>
    </form>
  {/if}
```
Naming the address is safe here and nowhere else on this page: the caller just proved they know the password for it. This is also the one place in the feature where "ask an administrator" is honest copy — the administrator has already acted, and re-issuing is exactly what they should do next.

- [ ] **Step 4: Add the footer link and the panel styles.** Change the footer snippet to:
```svelte
  {#snippet footer()}
    <span>New to Sauron? <a href="#/register">Create an account</a></span>
    <span><a href="#/forgot-password">Forgot your password?</a></span>
  {/snippet}
```
Without this the two new pages are reachable only by typing a URL. Add to the `<style>` block:
```css
  .panel {
    display: flex;
    flex-direction: column;
    gap: 10px;
    font-size: 14px;
    line-height: 1.5;
  }
  .muted {
    color: var(--text-muted);
    font-size: 13px;
  }
```

- [ ] **Step 5: Typecheck and test.** Run:
```
cd /home/splimter/projects/freelance/sauron/dashboard && npm run check && npm run test
```
Expect 0 errors and all tests green.

- [ ] **Step 6: Drive the refusal in a browser.** With the API and dashboard running as in Task 13 Step 7, lock an account directly:
```
psql postgres://sauron:sauron@172.20.0.2:5432/sauron -c "UPDATE users SET credentials_invalidated_at = now() WHERE lower(email) = 'you@example.com';"
```
Sign in as that account with its correct password and confirm the form is replaced by the emailed-link panel naming the address — not a red error box. Then sign in with a wrong password and confirm you get the ordinary red error. Clean up with `UPDATE users SET credentials_invalidated_at = NULL …`.

---

### Task 15: The members page dialog and wiring

**Files:**
- Create `dashboard/src/lib/components/members/ResetPasswordDialog.svelte`
- Modify `dashboard/src/lib/api/orgs.ts` (append beside `setMemberActive` at ~line 75)
- Modify `dashboard/src/pages/Members.svelte` (imports at 9-35; state block; `<MembersTable … />` at 404-415; dialog block at 494-503)

**Interfaces:**
- Consumes: `MemberPasswordResetResult` (Task 11); `MembersTable` props (Task 12); `Modal`, `Button` from `ui/`.
- Produces: `resetMemberPassword(orgId, userId, action): Promise<MemberPasswordResetResult>`.

- [ ] **Step 1: Add the API function.** Append to `dashboard/src/lib/api/orgs.ts` (and add `MemberPasswordResetResult` to the file's `import type { … } from '../models';` list):
```ts
/**
 * Goes through `api`, not `bareClient`: it needs the bearer.
 *
 * `action: 'reset'` is destructive — it stops the member's current password
 * authenticating. `'cancel'` is its undo and is the only one of the two that
 * works on a deployment with no SMTP configured.
 */
export async function resetMemberPassword(
  orgId: string,
  userId: string,
  action: 'reset' | 'cancel',
): Promise<MemberPasswordResetResult> {
  const { data } = await api.post<MemberPasswordResetResult>(
    `/v1/orgs/${orgId}/members/${userId}/password-reset`,
    { action },
  );
  return data;
}
```

- [ ] **Step 2: Create the dialog.** Create `dashboard/src/lib/components/members/ResetPasswordDialog.svelte`:
```svelte
<script lang="ts">
  import Modal from '../ui/Modal.svelte';
  import Button from '../ui/Button.svelte';
  import type { Member } from '../../models';

  interface Props {
    member: Member;
    action: 'reset' | 'cancel';
    busy: boolean;
    onconfirm: () => void;
    oncancel: () => void;
  }

  let { member, action, busy, onconfirm, oncancel }: Props = $props();
</script>

<Modal
  open
  title={action === 'reset' ? 'Reset this member’s password?' : 'Cancel the password reset?'}
  dismissible={!busy}
  onclose={oncancel}
>
  {#if action === 'reset'}
    <!-- The lockout is stated BEFORE the confirm button, because this sentence
         is the only warning between the admin and an account that cannot sign
         in. An admin who reads "we email them a link" and gets an unreachable
         account will not use this feature twice. -->
    <p class="lead"><strong>{member.email} will not be able to sign in until they use the emailed link.</strong></p>
    <p>
      Their current password stops working immediately and they are signed out of every device
      within a few seconds. We email them a link that expires in 24 hours. If it does not arrive,
      come back here to send another or to cancel.
    </p>
  {:else}
    <p class="lead">
      {member.email} will be able to sign in with their existing password again.
    </p>
    <p>
      They will still be asked to choose a new one when they do. Any reset link already sent stops
      working.
    </p>
  {/if}

  {#snippet footer()}
    <Button variant="ghost" onclick={oncancel} disabled={busy}>Never mind</Button>
    <Button variant={action === 'reset' ? 'danger' : 'primary'} loading={busy} onclick={onconfirm}>
      {action === 'reset' ? 'Reset password' : 'Cancel reset'}
    </Button>
  {/snippet}
</Modal>

<style>
  .lead {
    font-size: 14px;
    line-height: 1.5;
    margin-bottom: 10px;
  }
  p {
    font-size: 13.5px;
    line-height: 1.55;
    color: var(--text-muted);
  }
</style>
```
If `Button` has no `danger` variant, use the variant `ConfirmDialog.svelte` passes for its `danger` prop and match that styling.

- [ ] **Step 3: Wire the page.** In `dashboard/src/pages/Members.svelte`, add to the imports:
```ts
  import ResetPasswordDialog from '../lib/components/members/ResetPasswordDialog.svelte';
  import { resetMemberPassword } from '../lib/api/orgs';
```
(`authStore` was already imported in Task 12 Step 7.) Add beside the other `$state` declarations:
```ts
  let resetTarget = $state<{ member: Member; action: 'reset' | 'cancel' } | null>(null);
  let resetBusy = $state(false);
```
And the handler, beside `confirmDeactivate`:
```ts
  async function confirmPasswordReset() {
    const org = sessionStore.currentOrg;
    const target = resetTarget;
    if (!org || !target) return;
    resetBusy = true;
    try {
      await resetMemberPassword(org.id, target.member.user_id, target.action);
      toastStore.success(
        target.action === 'reset'
          ? `${target.member.email} has been emailed a link to set a new password.`
          : `${target.member.email} can sign in with their existing password again.`,
      );
      resetTarget = null;
      await load(org.id);
    } catch (err) {
      // The backend's 409s carry the actionable text (self, inactive,
      // cross-org) and its 503 names the missing setting — surface both
      // verbatim, exactly as toggleActive already does.
      toastStore.error(errorMessage(err));
    } finally {
      resetBusy = false;
    }
  }
```

- [ ] **Step 4: Replace the placeholder callback and render the dialog.** In the same file, change the `<MembersTable … />` call's `onresetpassword` from the Task 12 placeholder to:
```svelte
      onresetpassword={(m, a) => (resetTarget = { member: m, action: a })}
```
And immediately after the deactivation `ConfirmDialog` block, add:
```svelte
  {#if resetTarget}
    <ResetPasswordDialog
      member={resetTarget.member}
      action={resetTarget.action}
      busy={resetBusy}
      onconfirm={confirmPasswordReset}
      oncancel={() => (resetTarget = null)}
    />
  {/if}
```

- [ ] **Step 5: Typecheck and test.** Run:
```
cd /home/splimter/projects/freelance/sauron/dashboard && npm run check && npm run test
```
Expect 0 errors and all tests green.

- [ ] **Step 6: Drive the whole admin flow in a browser.** With the API (`SMTP_SINK=1`, `DASHBOARD_URL=http://localhost:5173`) and dashboard running, open `#/members` as an Owner and confirm, in this order:
  1. The row actions are one kebab menu, not inline buttons.
  2. **Reset password** is absent on your own row and on a deactivated member's row.
  3. Confirming a reset on another member toasts success, and the row gains a **Reset pending** badge on reload.
  4. The menu item on that row has become **Cancel password reset**, in the same slot.
  5. The API log shows one `password_reset` message with a `#/reset-password?token=…` link.
  6. Signing in as the target with their old password shows the emailed-link panel from Task 14.
  7. Opening the link, choosing a new password, and signing in with it works and does **not** land on `#/change-password`.
  8. Cancelling a reset on a fresh target restores their old password and leaves them landing on `#/change-password`.

---

### Task 16: Documentation

**Files:**
- Modify `wiki/Dashboard.md`
- Modify `packaging/rpm/SETUP.md` (§11 "Upgrading", created by S0)
- Modify `README.md` (the `API_TRUST_FORWARDED_HEADERS` row in §"Dashboard API", line 175)

**Interfaces:**
- Consumes: nothing.
- Produces: nothing.

- [ ] **Step 1: Add the wiki subsection.** In `wiki/Dashboard.md`, under whatever section covers signing in, add:
```markdown
### Forgot your password

The sign-in page carries a **Forgot your password?** link. Enter your address and
Sauron emails a link that expires in **1 hour**. The page shows the same
confirmation whether or not an account exists for that address — deliberately,
so nobody can use it to discover who has an account here.

Opening the link lets you choose a new password. Doing so signs you out of every
device, including the one you are on, and returns you to the sign-in page. Reset
links are single-use, and a link stops working the moment the account's password
changes for any other reason.

If nothing arrives: check your spam folder, then try again in a little while.
Three requests per address per hour are allowed.
```
No new page, so `_Sidebar.md` and `Home.md` need no registration.

- [ ] **Step 2: Add the members note.** In `wiki/Dashboard.md`, in the members section, add:
```markdown
#### Resetting a member's password

The row action menu on the members table carries **Reset password** for anyone
who is not you and is not deactivated. It needs both `member:credential` and
`member:manage`.

**This is a lockout, in the dialog's own words:** the member will not be able to
sign in until they use the emailed link. Their current password stops working
immediately and they are signed out of every device within a few seconds. The
link expires in 24 hours. The row then shows a **Reset pending** badge, visible
to anyone who can read the members list, so whoever fields "I can't log in" has
the answer without asking.

If the mail does not arrive, come back to the same menu — the item has become
**Cancel password reset**. Cancelling lets them sign in with their existing
password again, kills any link already sent, and still asks them to choose a new
password on their next sign-in. Cancel works even when SMTP is unconfigured;
**Reset password** refuses with a 503 and changes nothing, naming the missing
setting.

A member who also holds grants in another organization cannot be reset from
here, and neither can you reset yourself — use Change password for that.
```

- [ ] **Step 3: Add the upgrade row.** In `packaging/rpm/SETUP.md` §11 "Upgrading", append one row to the migration table:
```markdown
| 000036 | `password_reset` | **Nobody can sign in.** This migration adds `users.credentials_invalidated_at`, and the API selects an explicit column list for the whole user row — so an upgraded binary against an unmigrated database fails `login`, `refresh` and `/v1/me` with a missing-column error. This is a deployment-wide authentication outage, not "the three password-reset routes return 500". |
```
Run `sauron-migrate` after every upgrade, per the gate at the top of §11.

- [ ] **Step 4: Document the proxy flag against the new limiters.** In `README.md`, replace the `API_TRUST_FORWARDED_HEADERS` row of the §"Dashboard API" table (line 175) with:
```markdown
| `API_TRUST_FORWARDED_HEADERS` | Honour `X-Forwarded-For` / `X-Real-IP` when identifying the caller. Enable **only** behind a reverse proxy you control that overwrites the header — it is client-controlled otherwise, so turning it on without such a proxy lets a caller pick a fresh rate-limit bucket per request. While it is off *and* a proxy is in front, every request looks like it came from the proxy, so the per-IP limits throttle the whole deployment instead of each client: 10 registrations/hour, 60 logins/min, and 60/min on each of `/v1/auth/forgot-password` and `/v1/auth/reset-password`. Those two windows are 60 seconds rather than an hour precisely so a shared bucket self-heals within a minute; the per-address (3/hour) and per-link (10/hour) budgets are what carry the anti-abuse weight. `password_reset_tokens.requested_from` is also the proxy's address while this is off — a column full of one LAN address is the shipped topology, not a finding. | `false` | api |
```

- [ ] **Step 5: Verify nothing else in `packaging/rpm/` moved.** Run:
```
cd /home/splimter/projects/freelance/sauron && git status --short packaging/
```
Expect exactly one modified file, `packaging/rpm/SETUP.md`. S1 ships no new binary, so `packaging/rpm/binaries.txt` and `sauron.spec`'s `%files` must not change, and there is no new unit and no new per-service `.env`.

- [ ] **Step 6: Final full gate.** Run all four:
```
cd /home/splimter/projects/freelance/sauron/backend && cargo fmt --all --check
cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu cargo clippy --workspace --all-targets -- -D warnings
cd /home/splimter/projects/freelance/sauron/backend && DUCKDB_LIB_DIR=/home/splimter/projects/freelance/sauron/.cache/duckdb/1.5.4/x86_64-unknown-linux-gnu TEST_DATABASE_URL=postgres://sauron:sauron@172.20.0.2:5432/sauron TEST_REDIS_URL=redis://172.20.0.3:6379 cargo test --workspace
cd /home/splimter/projects/freelance/sauron/dashboard && npm run check && npm run test
```
Expect all four to exit 0.

