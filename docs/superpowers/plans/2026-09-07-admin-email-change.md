# Admin Email Change Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let an org admin change a member's email address, holding the change pending until the new address confirms it, while the old address can veto it.

**Architecture:** One new table, `email_change_requests`, holding one row per pending change with two independent secrets — an approve token mailed to the new address and a cancel token mailed to the old one. A partial unique index guarantees at most one live request per user. Two authenticated endpoints under `/v1/orgs/{org_id}/members/{user_id}/email-change` create and withdraw a request; three unauthenticated endpoints under `/v1/auth/email-change/` preview, confirm, and cancel one. Confirming rewrites `users.email` and nothing else.

**Tech Stack:** Rust (axum 0.8, diesel + diesel-async, Postgres), Svelte 5 (runes) dashboard, `sauron-mail` outbox for delivery.

**Spec:** `docs/superpowers/specs/2026-09-07-admin-email-change-design.md` — read it first; this plan argues from it.

## Global Constraints

- **NEVER run `git commit` or create a branch.** Leave every change unstaged in the working tree. This overrides the commit step every skill's task template includes. Report what changed; the user commits.
- **Workspace MSRV is 1.82** (`packaging/rpm/sauron.spec`). Async closures need 1.85, so `conn.transaction(...)` is unavailable — atomicity comes from single `UPDATE … RETURNING` statements.
- **Raw tokens are `sauron_core::ids::opaque_token()`** = `random_hex(32)` = exactly 64 hex characters. Stored as `sauron_auth::hash_token(raw)`, an unsalted SHA-256 hex string. A raw token must never be logged, returned in a response body, or placed in a URL query string.
- **`EMAIL_CHANGE_TTL_SECS = 86_400`** (24h). Every user-visible "expires in" string derives from it via `expiry_wording()`; never type the number.
- **Both new `MailKind` dedup windows are `Duration::ZERO`.** Not negotiable — see Task 3.
- **No CHECK constraint may reference `initiated_by`.** Its FK is `ON DELETE SET NULL`; that UPDATE re-validates every CHECK on the row, so deleting an admin would error out on unrelated rows.
- **Backend tests print `ok` having run nothing** when `TEST_DATABASE_URL` is unset, and rate-limiter assertions skip silently without `TEST_REDIS_URL`. Both must be set, and **wall-clock duration is the only proof a suite ran** — record it at each verification step.
- **Every new dashboard string needs an Arabic translation** in the same catalog entry (`{ en: '…', ar: '…' }`). The untranslated-string test has passed on pages with real leaks; translations get eyes, not just a green suite.
- **Neither public page may fire its POST on mount.** Link scanners fetch mailed URLs.

---

### Task 1: Migration, schema, and models

**Files:**
- Create: `backend/migrations/2026-09-07-000076_email_change_requests/up.sql`
- Create: `backend/migrations/2026-09-07-000076_email_change_requests/down.sql`
- Modify: `backend/crates/sauron-db/src/schema.rs` (add `table!` block; add to the `allow_tables_to_appear_in_same_query!` list at the end)
- Modify: `backend/crates/sauron-db/src/models.rs` (append after the `NewPasswordResetToken` block, ~line 645)

**Interfaces:**
- Consumes: nothing.
- Produces: `sauron_db::schema::email_change_requests`, `sauron_db::models::EmailChangeRequest`, `sauron_db::models::NewEmailChangeRequest`.

- [ ] **Step 1: Write `up.sql`**

```sql
-- An admin can change a member's email, but not unilaterally: `users.email` is
-- the login identity (users_email_lower_key), so an admin who could move it
-- silently would hold an account-takeover primitive. The change therefore waits
-- here until the NEW address confirms, and the OLD address can veto it.
--
-- Two token columns on one row, not two rows. A pending change is one fact with
-- two secrets; splitting them would make "cancel kills the matching approve" a
-- join every caller has to remember instead of a property of the row.
--
--   email_fingerprint  lower(users.email) at issue time. Re-checked in the same
--                      UPDATE that burns the token, so a link dies implicitly
--                      when the address moves for any other reason. Plaintext,
--                      not hashed: unlike a password hash this is not a
--                      credential, and the approve path already holds the value
--                      it compares against. Same job password_fingerprint does
--                      in 000036, one statement tighter.
--   org_id             confirm/cancel are UNAUTHENTICATED — the token is all
--                      they have, and the audit entry must land in an org.
--
-- INVARIANT enforced by the handlers, NOT by a CHECK: initiated_by is never
-- NULL at insert. Do NOT add that CHECK. The FK is ON DELETE SET NULL, that
-- action performs an UPDATE, and the UPDATE re-validates every CHECK on the
-- row -- so deleting an admin account would error out on an unrelated user's
-- request row. 000036 documents this trap; it applies here verbatim.
--
-- No index on expires_at. Every read path leads with a token hash (UNIQUE
-- btree) or user_id (the partial index) and applies expires_at > now() as a
-- filter on the single matching row; the reaper deletes on created_at.
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

-- THE supersede guarantee. Doing this in the application -- "invalidate the old
-- one, then insert" -- loses a race between two concurrent admins and leaves two
-- live approve links, which is a mistyped domain holding a standing claim on the
-- account. Here the loser gets a constraint error the handler maps to 409.
CREATE UNIQUE INDEX email_change_one_live_per_user
  ON email_change_requests (user_id)
  WHERE approved_at IS NULL AND cancelled_at IS NULL;

-- The members list joins on this to badge pending rows.
CREATE INDEX email_change_live_by_org
  ON email_change_requests (org_id)
  WHERE approved_at IS NULL AND cancelled_at IS NULL;
```

- [ ] **Step 2: Write `down.sql`**

```sql
-- The indexes and the UNIQUE constraints go with the table.
DROP TABLE IF EXISTS email_change_requests;
```

- [ ] **Step 3: Add the `table!` block to `schema.rs`**

Insert alphabetically near the other tables (after the `environments` block), and add `email_change_requests` to the `allow_tables_to_appear_in_same_query!` list at the bottom of the file plus a `joinable!(email_change_requests -> users (user_id));`.

```rust
diesel::table! {
    email_change_requests (id) {
        id -> Uuid,
        user_id -> Uuid,
        org_id -> Uuid,
        new_email -> Text,
        approve_token_hash -> Text,
        cancel_token_hash -> Text,
        email_fingerprint -> Text,
        initiated_by -> Nullable<Uuid>,
        requested_from -> Nullable<Text>,
        expires_at -> Timestamptz,
        approved_at -> Nullable<Timestamptz>,
        cancelled_at -> Nullable<Timestamptz>,
        cancelled_reason -> Nullable<Text>,
        created_at -> Timestamptz,
    }
}
```

Note: `email_change_requests` has two FKs to `users` (`user_id`, `initiated_by`), so like `password_reset_tokens` only the `user_id` one gets a `joinable!` — diesel cannot express two.

- [ ] **Step 4: Add the models**

```rust
/// A pending change to a user's login address.
///
/// Derives no `Serialize`, exactly like [`PasswordResetToken`]: both token
/// hashes must never leave the process, and no endpoint returns this row. The
/// handlers project the two fields a caller may see (`new_email`,
/// `expires_at`) into their own response types.
#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = email_change_requests)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct EmailChangeRequest {
    pub id: Uuid,
    pub user_id: Uuid,
    pub org_id: Uuid,
    pub new_email: String,
    pub approve_token_hash: String,
    pub cancel_token_hash: String,
    pub email_fingerprint: String,
    pub initiated_by: Option<Uuid>,
    pub requested_from: Option<String>,
    pub expires_at: DateTime<Utc>,
    pub approved_at: Option<DateTime<Utc>>,
    pub cancelled_at: Option<DateTime<Utc>>,
    /// `"user"`, `"admin"` or `"superseded"` — see the CHECK in migration 000076.
    pub cancelled_reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Insert-only. Must never gain `Queryable`: that derive decodes positionally,
/// so a struct whose field order differs from the `table!` block would bind
/// `cancel_token_hash` to `approve_token_hash` and still compile — which would
/// mail the veto link to the new address and the approve link to the old one.
#[derive(Debug, Insertable)]
#[diesel(table_name = email_change_requests)]
pub struct NewEmailChangeRequest {
    pub user_id: Uuid,
    pub org_id: Uuid,
    pub new_email: String,
    pub approve_token_hash: String,
    pub cancel_token_hash: String,
    pub email_fingerprint: String,
    pub initiated_by: Option<Uuid>,
    pub requested_from: Option<String>,
    pub expires_at: DateTime<Utc>,
}
```

- [ ] **Step 5: Verify it compiles and the migration applies**

```bash
cd backend && cargo check -p sauron-db 2>&1 | tail -20
```
Expected: no errors.

```bash
cd backend && cargo run -p sauron-migrate 2>&1 | tail -5
```
Expected: `000076` applied. Requires `DATABASE_URL`.

---

### Task 2: Repository layer

**Files:**
- Modify: `backend/crates/sauron-db/src/repo.rs` (append after `prune_password_reset_tokens`, ~line 870)
- Create: `backend/crates/sauron-db/tests/email_change.rs`

**Interfaces:**
- Consumes: `models::{EmailChangeRequest, NewEmailChangeRequest}` from Task 1.
- Produces:
  - `repo::EMAIL_CHANGE_CANCELLED_USER: &str = "user"`
  - `repo::EMAIL_CHANGE_CANCELLED_ADMIN: &str = "admin"`
  - `repo::EMAIL_CHANGE_CANCELLED_SUPERSEDED: &str = "superseded"`
  - `insert_email_change_request(&mut AsyncPgConnection, NewEmailChangeRequest) -> QueryResult<EmailChangeRequest>`
  - `find_live_email_change_by_approve_token(&mut AsyncPgConnection, &str) -> QueryResult<Option<EmailChangeRequest>>`
  - `find_live_email_change_by_cancel_token(&mut AsyncPgConnection, &str) -> QueryResult<Option<EmailChangeRequest>>`
  - `find_live_email_change_for_user(&mut AsyncPgConnection, Uuid) -> QueryResult<Option<EmailChangeRequest>>`
  - `consume_email_change_approval(&mut AsyncPgConnection, token_hash: &str, expected_fingerprint: &str) -> QueryResult<Option<(Uuid, String)>>` — `(user_id, new_email)`
  - `cancel_email_change_by_cancel_token(&mut AsyncPgConnection, &str) -> QueryResult<Option<(Uuid, Uuid)>>` — `(user_id, org_id)`
  - `cancel_live_email_change_for_user(&mut AsyncPgConnection, Uuid, reason: &str) -> QueryResult<usize>`
  - `live_email_changes_for_org(&mut AsyncPgConnection, Uuid) -> QueryResult<Vec<(Uuid, String, DateTime<Utc>)>>` — `(user_id, new_email, expires_at)`
  - `set_user_email(&mut AsyncPgConnection, Uuid, &str) -> QueryResult<usize>`
  - `prune_email_change_requests(&mut AsyncPgConnection, older_than_days: i64) -> QueryResult<usize>`

- [ ] **Step 1: Write the failing DB tests**

Create `backend/crates/sauron-db/tests/email_change.rs`. Use the same `TestDb` harness the neighbouring suites use — copy the top-of-file setup from `backend/crates/sauron-db/tests/audit_log.rs` verbatim, including its skip-when-no-database guard.

```rust
//! `email_change_requests` at the repository layer: the supersede guarantee,
//! the fingerprint kill-switch, and the two burn paths.

mod common;
use common::TestDb;

use chrono::{Duration, Utc};
use sauron_db::models::NewEmailChangeRequest;
use sauron_db::repo;

/// Build a request row for `user_id`. `tag` keeps the two token hashes unique
/// across calls so a test can insert several without colliding on the UNIQUE
/// columns.
fn new_request(user_id: uuid::Uuid, org_id: uuid::Uuid, tag: &str, fingerprint: &str)
    -> NewEmailChangeRequest
{
    NewEmailChangeRequest {
        user_id,
        org_id,
        new_email: format!("{tag}@example.com"),
        approve_token_hash: format!("approve-{tag}"),
        cancel_token_hash: format!("cancel-{tag}"),
        email_fingerprint: fingerprint.to_string(),
        initiated_by: None,
        requested_from: None,
        expires_at: Utc::now() + Duration::hours(24),
    }
}

#[tokio::test]
async fn a_second_live_request_for_one_user_is_refused_by_the_index() {
    let Some(db) = TestDb::new().await else { return };
    let mut conn = db.conn().await;
    let (user, org) = db.seed_user_and_org(&mut conn).await;

    repo::insert_email_change_request(&mut conn, new_request(user, org, "first", "old@example.com"))
        .await
        .expect("first insert");

    // The partial unique index, not the application, is what makes supersede
    // safe under two concurrent admins.
    let second = repo::insert_email_change_request(
        &mut conn,
        new_request(user, org, "second", "old@example.com"),
    )
    .await;
    assert!(
        matches!(
            second,
            Err(diesel::result::Error::DatabaseError(
                diesel::result::DatabaseErrorKind::UniqueViolation,
                _
            ))
        ),
        "expected a unique violation, got {second:?}"
    );
}

#[tokio::test]
async fn cancelling_frees_the_slot_for_a_new_request() {
    let Some(db) = TestDb::new().await else { return };
    let mut conn = db.conn().await;
    let (user, org) = db.seed_user_and_org(&mut conn).await;

    repo::insert_email_change_request(&mut conn, new_request(user, org, "first", "old@example.com"))
        .await
        .expect("first insert");
    let n = repo::cancel_live_email_change_for_user(
        &mut conn,
        user,
        repo::EMAIL_CHANGE_CANCELLED_SUPERSEDED,
    )
    .await
    .expect("cancel");
    assert_eq!(n, 1);

    repo::insert_email_change_request(&mut conn, new_request(user, org, "second", "old@example.com"))
        .await
        .expect("the slot is free once the first is cancelled");
}

#[tokio::test]
async fn consume_refuses_a_stale_fingerprint() {
    let Some(db) = TestDb::new().await else { return };
    let mut conn = db.conn().await;
    let (user, org) = db.seed_user_and_org(&mut conn).await;

    repo::insert_email_change_request(&mut conn, new_request(user, org, "t", "old@example.com"))
        .await
        .expect("insert");

    // The address moved since the link was issued: the link is stale and must
    // not fire. Checked inside the burn's WHERE, so it is atomic with it.
    let stale = repo::consume_email_change_approval(&mut conn, "approve-t", "moved@example.com")
        .await
        .expect("query");
    assert!(stale.is_none(), "a stale fingerprint must not burn the row");

    // And the row is still live, so the real link keeps working.
    let ok = repo::consume_email_change_approval(&mut conn, "approve-t", "old@example.com")
        .await
        .expect("query");
    assert_eq!(ok, Some((user, "t@example.com".to_string())));
}

#[tokio::test]
async fn a_link_burns_exactly_once() {
    let Some(db) = TestDb::new().await else { return };
    let mut conn = db.conn().await;
    let (user, org) = db.seed_user_and_org(&mut conn).await;

    repo::insert_email_change_request(&mut conn, new_request(user, org, "t", "old@example.com"))
        .await
        .expect("insert");

    let first = repo::consume_email_change_approval(&mut conn, "approve-t", "old@example.com")
        .await
        .expect("query");
    assert!(first.is_some());
    let second = repo::consume_email_change_approval(&mut conn, "approve-t", "old@example.com")
        .await
        .expect("query");
    assert!(second.is_none(), "single-use is the security property");
}

#[tokio::test]
async fn the_cancel_token_kills_the_approve_token() {
    let Some(db) = TestDb::new().await else { return };
    let mut conn = db.conn().await;
    let (user, org) = db.seed_user_and_org(&mut conn).await;

    repo::insert_email_change_request(&mut conn, new_request(user, org, "t", "old@example.com"))
        .await
        .expect("insert");

    let cancelled = repo::cancel_email_change_by_cancel_token(&mut conn, "cancel-t")
        .await
        .expect("query");
    assert_eq!(cancelled, Some((user, org)));

    let approve = repo::consume_email_change_approval(&mut conn, "approve-t", "old@example.com")
        .await
        .expect("query");
    assert!(approve.is_none(), "the veto must kill the approve link");
}

#[tokio::test]
async fn an_expired_request_is_neither_found_nor_burnable() {
    let Some(db) = TestDb::new().await else { return };
    let mut conn = db.conn().await;
    let (user, org) = db.seed_user_and_org(&mut conn).await;

    let mut req = new_request(user, org, "t", "old@example.com");
    req.expires_at = Utc::now() - Duration::minutes(1);
    repo::insert_email_change_request(&mut conn, req).await.expect("insert");

    assert!(repo::find_live_email_change_by_approve_token(&mut conn, "approve-t")
        .await
        .expect("query")
        .is_none());
    assert!(repo::consume_email_change_approval(&mut conn, "approve-t", "old@example.com")
        .await
        .expect("query")
        .is_none());
}

#[tokio::test]
async fn live_changes_for_org_lists_only_live_ones() {
    let Some(db) = TestDb::new().await else { return };
    let mut conn = db.conn().await;
    let (user, org) = db.seed_user_and_org(&mut conn).await;

    repo::insert_email_change_request(&mut conn, new_request(user, org, "t", "old@example.com"))
        .await
        .expect("insert");
    let live = repo::live_email_changes_for_org(&mut conn, org).await.expect("query");
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].0, user);
    assert_eq!(live[0].1, "t@example.com");

    repo::cancel_live_email_change_for_user(&mut conn, user, repo::EMAIL_CHANGE_CANCELLED_ADMIN)
        .await
        .expect("cancel");
    let live = repo::live_email_changes_for_org(&mut conn, org).await.expect("query");
    assert!(live.is_empty());
}
```

If `common::TestDb` has no `seed_user_and_org` helper, add one that inserts a user via `repo::create_user` and an organization via `repo::create_org`, returning `(user_id, org_id)`.

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cd backend && time cargo test -p sauron-db --test email_change 2>&1 | tail -30
```
Expected: FAIL to compile — `insert_email_change_request` and friends do not exist.

- [ ] **Step 3: Implement the repo functions**

Append to `backend/crates/sauron-db/src/repo.rs`:

```rust
// --- email change requests ---------------------------------------------------

/// Written to `email_change_requests.cancelled_reason`. Stable wire strings: the
/// CHECK in migration 000076 matches on them, and the audit trail reads them to
/// tell "the member rejected an admin's attempt" from "the admin withdrew it".
pub const EMAIL_CHANGE_CANCELLED_USER: &str = "user";
pub const EMAIL_CHANGE_CANCELLED_ADMIN: &str = "admin";
pub const EMAIL_CHANGE_CANCELLED_SUPERSEDED: &str = "superseded";

/// Open a pending change. A `UniqueViolation` here means either a concurrent
/// admin won the race for this user's single live slot, or a token hash
/// collided; callers map it to 409 rather than retrying.
pub async fn insert_email_change_request(
    conn: &mut AsyncPgConnection,
    req: NewEmailChangeRequest,
) -> QueryResult<EmailChangeRequest> {
    diesel::insert_into(email_change_requests::table)
        .values(req)
        .returning(EmailChangeRequest::as_returning())
        .get_result(conn)
        .await
}

/// The three conditions that make a request live, in one place so the read
/// paths and the burn paths cannot disagree about what "live" means.
macro_rules! live_email_change {
    () => {
        email_change_requests::table
            .filter(email_change_requests::approved_at.is_null())
            .filter(email_change_requests::cancelled_at.is_null())
            .filter(email_change_requests::expires_at.gt(Utc::now()))
    };
}

pub async fn find_live_email_change_by_approve_token(
    conn: &mut AsyncPgConnection,
    token_hash: &str,
) -> QueryResult<Option<EmailChangeRequest>> {
    live_email_change!()
        .filter(email_change_requests::approve_token_hash.eq(token_hash))
        .select(EmailChangeRequest::as_select())
        .first(conn)
        .await
        .optional()
}

pub async fn find_live_email_change_by_cancel_token(
    conn: &mut AsyncPgConnection,
    token_hash: &str,
) -> QueryResult<Option<EmailChangeRequest>> {
    live_email_change!()
        .filter(email_change_requests::cancel_token_hash.eq(token_hash))
        .select(EmailChangeRequest::as_select())
        .first(conn)
        .await
        .optional()
}

pub async fn find_live_email_change_for_user(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
) -> QueryResult<Option<EmailChangeRequest>> {
    live_email_change!()
        .filter(email_change_requests::user_id.eq(user_id))
        .select(EmailChangeRequest::as_select())
        .first(conn)
        .await
        .optional()
}

#[derive(QueryableByName)]
struct ConsumedEmailChangeRow {
    #[diesel(sql_type = SqlUuid)]
    user_id: Uuid,
    #[diesel(sql_type = Text)]
    new_email: String,
}

/// Burn an approve link, atomically. Returns `(user_id, new_email)`.
///
/// `expected_fingerprint` is `lower(users.email)` as the caller just read it,
/// and it is matched INSIDE this statement rather than checked beforehand: a
/// read-then-burn leaves a window in which the address moves between the two,
/// and the burn would then apply a change the fingerprint was supposed to stop.
/// This is one statement tighter than `consume_password_reset_token`, which
/// checks its fingerprint on a prior read.
///
/// Zero rows means the link was already burned, cancelled, expired, or is stale.
/// One `UPDATE … RETURNING` rather than a SELECT then an UPDATE because
/// single-use is the whole security property and `conn.transaction` is
/// unavailable (async closures need Rust 1.85; workspace MSRV is 1.82).
pub async fn consume_email_change_approval(
    conn: &mut AsyncPgConnection,
    token_hash: &str,
    expected_fingerprint: &str,
) -> QueryResult<Option<(Uuid, String)>> {
    let row: Option<ConsumedEmailChangeRow> = diesel::sql_query(
        "UPDATE email_change_requests SET approved_at = now() \
         WHERE approve_token_hash = $1 AND approved_at IS NULL AND cancelled_at IS NULL \
           AND expires_at > now() AND email_fingerprint = $2 \
         RETURNING user_id, new_email",
    )
    .bind::<Text, _>(token_hash)
    .bind::<Text, _>(expected_fingerprint)
    .get_result(conn)
    .await
    .optional()?;
    Ok(row.map(|r| (r.user_id, r.new_email)))
}

#[derive(QueryableByName)]
struct CancelledEmailChangeRow {
    #[diesel(sql_type = SqlUuid)]
    user_id: Uuid,
    #[diesel(sql_type = SqlUuid)]
    org_id: Uuid,
}

/// Burn a veto link, atomically. Returns `(user_id, org_id)` — the org because
/// the caller is unauthenticated and the audit entry has to land somewhere.
///
/// Deliberately does NOT return `new_email`. This is the old address's path, and
/// the whole point of that mail is that it never learns the new one.
pub async fn cancel_email_change_by_cancel_token(
    conn: &mut AsyncPgConnection,
    token_hash: &str,
) -> QueryResult<Option<(Uuid, Uuid)>> {
    let row: Option<CancelledEmailChangeRow> = diesel::sql_query(
        "UPDATE email_change_requests SET cancelled_at = now(), cancelled_reason = 'user' \
         WHERE cancel_token_hash = $1 AND approved_at IS NULL AND cancelled_at IS NULL \
           AND expires_at > now() \
         RETURNING user_id, org_id",
    )
    .bind::<Text, _>(token_hash)
    .get_result(conn)
    .await
    .optional()?;
    Ok(row.map(|r| (r.user_id, r.org_id)))
}

/// Kill whatever this user has outstanding. Used by the admin cancel and by the
/// supersede step of a new request.
pub async fn cancel_live_email_change_for_user(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    reason: &str,
) -> QueryResult<usize> {
    diesel::update(
        email_change_requests::table
            .filter(email_change_requests::user_id.eq(user_id))
            .filter(email_change_requests::approved_at.is_null())
            .filter(email_change_requests::cancelled_at.is_null()),
    )
    .set((
        email_change_requests::cancelled_at.eq(Utc::now()),
        email_change_requests::cancelled_reason.eq(reason),
    ))
    .execute(conn)
    .await
}

/// `(user_id, new_email, expires_at)` for every live request in the org — what
/// the members list badges. Rides `email_change_live_by_org`.
pub async fn live_email_changes_for_org(
    conn: &mut AsyncPgConnection,
    org_id: Uuid,
) -> QueryResult<Vec<(Uuid, String, DateTime<Utc>)>> {
    live_email_change!()
        .filter(email_change_requests::org_id.eq(org_id))
        .select((
            email_change_requests::user_id,
            email_change_requests::new_email,
            email_change_requests::expires_at,
        ))
        .load(conn)
        .await
}

/// Move the login identity. Returns the row count so a caller can tell a no-op
/// from a hit; a `UniqueViolation` means the address was claimed since the
/// request was issued and the caller must report 409 rather than retry.
///
/// `updated_at` moves with it — this is a change to the account, not a
/// bookkeeping write.
pub async fn set_user_email(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    email: &str,
) -> QueryResult<usize> {
    diesel::update(users::table.filter(users::id.eq(user_id)))
        .set((
            users::email.eq(email.to_lowercase()),
            users::updated_at.eq(Utc::now()),
        ))
        .execute(conn)
        .await
}

/// Deletes by `created_at`, not `expires_at`, so a resolved request's trace
/// survives a fixed window regardless of its TTL. Same reasoning as
/// [`prune_password_reset_tokens`], and the two run in the same reaper tick.
pub async fn prune_email_change_requests(
    conn: &mut AsyncPgConnection,
    older_than_days: i64,
) -> QueryResult<usize> {
    diesel::sql_query(
        "DELETE FROM email_change_requests WHERE created_at < now() - ($1 || ' days')::interval",
    )
    .bind::<Text, _>(older_than_days.to_string())
    .execute(conn)
    .await
}
```

Add `EmailChangeRequest, NewEmailChangeRequest` to the `models::` import list at the top of `repo.rs`, and `email_change_requests` to the `schema::` import list.

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cd backend && time cargo test -p sauron-db --test email_change 2>&1 | tail -20
```
Expected: 7 passed. **The wall-clock time must be non-trivial (seconds, not 0.00s).** A `0.00s` run means `TEST_DATABASE_URL` is unset and every test returned at its `else { return }` guard, proving nothing.

---

### Task 3: Mail kinds

**Files:**
- Modify: `backend/crates/sauron-mail/src/kind.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `MailKind::EmailChangeNotice` (wire `email_change_notice`), `MailKind::EmailChangeApproval` (wire `email_change_approval`), both with `dedup_window() == Duration::ZERO`.

- [ ] **Step 1: Extend the existing tests**

In `kind.rs`'s `mod tests`, add both variants to the `all` array in `wire_strings_are_stable_and_distinct` and to the expected `names` vec (order: after `SmtpTest`). Then add to `dedup_windows_are_the_reviewed_values`:

```rust
        // ZERO, and it must stay zero. This is the veto signal: the mail whose
        // link is the member's only way to stop an admin moving their login
        // identity. A second request SUPERSEDES the first, so repeat notices to
        // the SAME old address are a normal event — and PasswordReset's
        // 300-second window would swallow the second attempt's warning, which is
        // exactly the case the veto exists for. Suppression returns the same
        // `Ok(None)` a successful discard returns, so this would fail silently.
        // The bound is EMAIL_CHANGE_PER_TARGET_PER_HOUR on an authenticated,
        // member:credential-gated endpoint, not this window.
        assert_eq!(MailKind::EmailChangeNotice.dedup_window(), Duration::ZERO);
        // ZERO for a plainer reason: an admin correcting a mistyped address
        // mails a DIFFERENT recipient, and an admin resending after a bounce
        // must actually resend.
        assert_eq!(MailKind::EmailChangeApproval.dedup_window(), Duration::ZERO);
```

- [ ] **Step 2: Run to verify failure**

```bash
cd backend && cargo test -p sauron-mail kind 2>&1 | tail -15
```
Expected: FAIL to compile — no variant `EmailChangeNotice`.

- [ ] **Step 3: Add the variants**

```rust
pub enum MailKind {
    PasswordReset,
    NotificationDigest,
    PersonalNotification,
    SmtpTest,
    EmailChangeNotice,
    EmailChangeApproval,
}
```

In `as_str`: `MailKind::EmailChangeNotice => "email_change_notice"`, `MailKind::EmailChangeApproval => "email_change_approval"`.
In `dedup_window`: both `Duration::ZERO`.

- [ ] **Step 4: Run to verify pass**

```bash
cd backend && cargo test -p sauron-mail 2>&1 | tail -10
```
Expected: all pass.

---

### Task 4: Mail rendering

**Files:**
- Modify: `backend/bins/sauron-api/src/routes/auth.rs` (append after `render_password_reset_mail`, ~line 396)

**Interfaces:**
- Consumes: `sauron_mail::{MailContent, Cta, TemplateError}`, `expiry_wording`, `PASTE_FALLBACK` (all already in this file).
- Produces:
  - `pub(crate) const EMAIL_CHANGE_TTL_SECS: i64 = 86_400;`
  - `pub(crate) fn email_change_confirm_link(dashboard_url: &str, raw_token: &str) -> String`
  - `pub(crate) fn email_change_cancel_link(dashboard_url: &str, raw_token: &str) -> String`
  - `pub(crate) struct EmailChangeMailVars<'a> { pub display_name: &'a str, pub org_name: &'a str, pub url: &'a str }`
  - `pub(crate) fn render_email_change_notice(EmailChangeMailVars) -> Result<MailContent, TemplateError>`
  - `pub(crate) fn render_email_change_approval(EmailChangeMailVars) -> Result<MailContent, TemplateError>`

- [ ] **Step 1: Write the failing tests**

Append to `auth.rs`'s existing `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn the_notice_never_names_the_new_address() {
        let content = render_email_change_notice(EmailChangeMailVars {
            display_name: "Bob",
            org_name: "Acme",
            url: "https://dash.example.com/#/cancel-email-change?token=abc",
        })
        .expect("render");

        // Asserted against the RENDERED body, not the template source: the rule
        // is about what the old address receives, and a future edit that
        // interpolates the address into a footnote would pass a source check.
        let body = format!(
            "{} {} {} {}",
            content.subject,
            content.heading,
            content.paragraphs.join(" "),
            content.footnotes.join(" ")
        );
        assert!(
            !body.contains("new@example.com"),
            "the old-address mail must never name the new address: {body}"
        );
        // And the veto is reachable.
        assert_eq!(
            content.cta.expect("cta").url,
            "https://dash.example.com/#/cancel-email-change?token=abc"
        );
    }

    #[test]
    fn the_approval_names_the_org_and_carries_the_confirm_link() {
        let content = render_email_change_approval(EmailChangeMailVars {
            display_name: "Bob",
            org_name: "Acme",
            url: "https://dash.example.com/#/confirm-email-change?token=abc",
        })
        .expect("render");
        assert!(content.paragraphs.iter().any(|p| p.contains("Acme")));
        assert!(content.footnotes.iter().any(|f| f.contains("24 hours")));
    }

    #[test]
    fn neither_mail_names_the_acting_admin() {
        // `EmailChangeMailVars` has no field for one, which is the enforcement.
        // This test exists so that adding such a field is a deliberate act with
        // a failing test attached, matching the rule ResetMailVars documents.
        let vars = EmailChangeMailVars {
            display_name: "Bob",
            org_name: "Acme",
            url: "https://dash.example.com/#/confirm-email-change?token=abc",
        };
        let content = render_email_change_approval(vars).expect("render");
        let body = content.paragraphs.join(" ");
        assert!(!body.contains("admin@"), "no individual is named");
    }

    #[test]
    fn a_non_http_link_is_refused_rather_than_rendered() {
        let err = render_email_change_approval(EmailChangeMailVars {
            display_name: "Bob",
            org_name: "Acme",
            url: "javascript:alert(1)",
        });
        assert!(err.is_err());
    }
```

- [ ] **Step 2: Run to verify failure**

```bash
cd backend && cargo test -p sauron-api --lib email_change 2>&1 | tail -15
```
Expected: FAIL to compile.

- [ ] **Step 3: Implement**

```rust
/// How long an email-change link lives.
///
/// 24 hours, matching [`ADMIN_RESET_TTL_SECS`]: both are admin-initiated acts
/// aimed at someone who may not read mail the same day, and a shorter window
/// would mostly produce dead links an admin has to reissue.
pub(crate) const EMAIL_CHANGE_TTL_SECS: i64 = 86_400;

/// Fragment-based for the reason [`reset_link`] documents: the token sits after
/// the `#`, so it is never sent in a request line or a `Referer` and reaches no
/// server log, proxy log, or analytics beacon.
pub(crate) fn email_change_confirm_link(dashboard_url: &str, raw_token: &str) -> String {
    format!(
        "{}/#/confirm-email-change?token={}",
        dashboard_url.trim_end_matches('/'),
        raw_token
    )
}

/// Sibling of [`email_change_confirm_link`], carrying the veto token instead.
pub(crate) fn email_change_cancel_link(dashboard_url: &str, raw_token: &str) -> String {
    format!(
        "{}/#/cancel-email-change?token={}",
        dashboard_url.trim_end_matches('/'),
        raw_token
    )
}

/// Everything both email-change messages interpolate.
///
/// There is deliberately no field for the acting admin, and no field for the new
/// address. The first matches the rule [`ResetMailVars`] documents: the org is
/// what a recipient needs to judge legitimacy, and naming an individual invites
/// a reply to a person rather than a route back into the account. The second is
/// the structural half of the non-disclosure rule — the notice renderer cannot
/// leak an address it was never given, so the rule holds even if someone edits
/// the copy without reading this comment.
pub(crate) struct EmailChangeMailVars<'a> {
    /// The recipient's display name, already falling back to their email.
    pub display_name: &'a str,
    pub org_name: &'a str,
    /// The confirm link for the approval mail, the cancel link for the notice.
    pub url: &'a str,
}

/// The mail sent to the address being replaced.
///
/// This is the veto. It says a change was requested, never what to, and offers
/// exactly one action: stop it.
pub(crate) fn render_email_change_notice(
    vars: EmailChangeMailVars<'_>,
) -> Result<sauron_mail::MailContent, sauron_mail::TemplateError> {
    let expiry = expiry_wording(EMAIL_CHANGE_TTL_SECS);
    let name = vars.display_name;
    let org = vars.org_name;

    Ok(sauron_mail::MailContent {
        subject: "A change to your Sauron email address was requested".to_string(),
        heading: "Was this you?".to_string(),
        paragraphs: vec![
            format!("Hi {name},"),
            format!(
                "An administrator of {org} asked to change the email address on your account."
            ),
            "Nothing has changed yet. The change takes effect only when the new address \
             confirms it."
                .to_string(),
            format!(
                "If you did not expect this, use the link below to stop it. The request lapses \
                 on its own in {expiry}."
            ),
        ],
        cta: Some(sauron_mail::Cta::new("Stop this change", vars.url)?),
        footnotes: vec![
            PASTE_FALLBACK.to_string(),
            vars.url.to_string(),
            format!(
                "If you were expecting this, no action is needed — you can ignore this email, \
                 or contact an administrator of {org}."
            ),
        ],
    })
}

/// The mail sent to the address being adopted.
///
/// The recipient already holds this mailbox, so naming the address here would
/// tell them nothing they do not know; it is omitted anyway because the sentence
/// reads better without it and the page behind the link states it plainly.
pub(crate) fn render_email_change_approval(
    vars: EmailChangeMailVars<'_>,
) -> Result<sauron_mail::MailContent, sauron_mail::TemplateError> {
    let expiry = expiry_wording(EMAIL_CHANGE_TTL_SECS);
    let name = vars.display_name;
    let org = vars.org_name;

    Ok(sauron_mail::MailContent {
        subject: "Confirm your new Sauron email address".to_string(),
        heading: "Confirm this address".to_string(),
        paragraphs: vec![
            format!("Hi {name},"),
            format!(
                "An administrator of {org} set this address as the new sign-in email for your \
                 account."
            ),
            "Until you confirm, your account keeps its current address and you keep signing in \
             with it."
                .to_string(),
            format!("The link below expires in {expiry}."),
        ],
        cta: Some(sauron_mail::Cta::new("Confirm this address", vars.url)?),
        footnotes: vec![
            PASTE_FALLBACK.to_string(),
            vars.url.to_string(),
            format!(
                "If you were not expecting this, ignore this email and the request will lapse. \
                 The current address on the account has also been notified."
            ),
        ],
    })
}
```

- [ ] **Step 4: Run to verify pass**

```bash
cd backend && cargo test -p sauron-api --lib 2>&1 | tail -10
```
Expected: all pass.

---

### Task 5: Admin endpoints

**Files:**
- Modify: `backend/bins/sauron-api/src/routes/auth.rs` (add four limiter constants beside the reset ones, ~line 100)
- Modify: `backend/bins/sauron-api/src/audit.rs` (three action constants, ~line 55)
- Modify: `backend/bins/sauron-api/src/routes/orgs.rs` (append after `reset_member_password`, ~line 1730)
- Modify: `backend/bins/sauron-api/src/main.rs` (route wiring, ~line 490)
- Modify: `backend/bins/sauron-api/src/openapi.rs` (register both paths and the request/response schemas)

**Interfaces:**
- Consumes: Task 2's repo functions; Task 4's `EMAIL_CHANGE_TTL_SECS`, `email_change_confirm_link`, `email_change_cancel_link`, `EmailChangeMailVars`, `render_email_change_notice`, `render_email_change_approval`; Task 3's `MailKind` variants.
- Produces:
  - `routes::orgs::request_member_email_change` — `POST /v1/orgs/{org_id}/members/{user_id}/email-change`
  - `routes::orgs::cancel_member_email_change` — `DELETE /v1/orgs/{org_id}/members/{user_id}/email-change`
  - `routes::orgs::RequestEmailChangeReq { pub new_email: String }`
  - `audit::action::{MEMBER_EMAIL_CHANGE_REQUEST, MEMBER_EMAIL_CHANGE_APPROVED, MEMBER_EMAIL_CHANGE_CANCELLED}`

- [ ] **Step 1: Add the constants**

In `auth.rs`, beside `ADMIN_RESET_PER_TARGET_PER_HOUR`:

```rust
/// Email-change requests per calling admin per hour. Bounds fan-out, exactly as
/// [`ADMIN_RESET_PER_CALLER_PER_HOUR`] does, and for the same reason:
/// `member:credential` is in the Admin preset, not just Owner.
pub(crate) const EMAIL_CHANGE_PER_CALLER_PER_HOUR: u32 = 20;

/// Email-change requests per target per hour. The one that matters, and the
/// ONLY bound on the two mails this endpoint sends — both `MailKind`s carry a
/// ZERO dedup window on purpose (see `sauron-mail`'s `kind.rs`), so the
/// per-recipient suppression that backstops password-reset mail does not apply
/// here. Lower this and the veto signal gets suppressed; raise it and one
/// member's inbox is the target.
pub(crate) const EMAIL_CHANGE_PER_TARGET_PER_HOUR: u32 = 5;

/// Public confirm/cancel/preview attempts per IP per minute. A burst limiter.
pub(crate) const EMAIL_CHANGE_ATTEMPTS_PER_MIN_PER_IP: u32 = 60;

/// Public attempts per token per hour. Bounds the branches that return WITHOUT
/// burning the row — a stale fingerprint, or an address claimed since issue.
pub(crate) const EMAIL_CHANGE_ATTEMPTS_PER_TOKEN_PER_HOUR: u32 = 10;
```

In `audit.rs`, beside `MEMBER_RESET_PASSWORD`:

```rust
    pub const MEMBER_EMAIL_CHANGE_REQUEST: &str = "member.email_change_request";
    pub const MEMBER_EMAIL_CHANGE_APPROVED: &str = "member.email_change_approved";
    pub const MEMBER_EMAIL_CHANGE_CANCELLED: &str = "member.email_change_cancelled";
```

- [ ] **Step 2: Implement the request handler**

Append to `orgs.rs`:

```rust
#[derive(Deserialize, utoipa::ToSchema)]
pub struct RequestEmailChangeReq {
    pub new_email: String,
}

/// Open a pending change to a member's sign-in address.
///
/// Nothing changes when this returns. `users.email` moves only when the holder
/// of the new address confirms, and the holder of the OLD address is mailed a
/// link that stops it — which is what keeps `member:credential` from being an
/// account-takeover primitive. An admin who could move a login identity
/// silently could park it on an address they control and then use the ordinary
/// forgotten-password flow.
///
/// A second request supersedes the first. That is enforced by
/// `email_change_one_live_per_user`, a partial unique index, and not by the
/// supersede call below: two concurrent admins both pass the supersede step and
/// only the index stops them both inserting, which is the difference between
/// one live approve link and two.
#[utoipa::path(
    post, path = "/v1/orgs/{org_id}/members/{user_id}/email-change", tag = "Organizations",
    summary = "Request a change to a member's email",
    description = "\
Mails the new address a confirmation link valid for 24 hours, and the current \
address a notice carrying a link that cancels the request. The current address \
is never told what the new one is.

Nothing changes until the new address confirms. Refused for a member holding \
grants outside this organization.",
    params(("org_id" = Uuid, Path, description = "The organization."), ("user_id" = Uuid, Path, description = "The member.")),
    security(("bearerAuth" = [])),
    request_body(content = RequestEmailChangeReq),
    responses(
        (status = 200, description = "Request opened and both mails queued.", body = OkResponse),
        (status = 400, description = "The address is not usable.", body = ErrorResponse),
        (status = 401, description = "Missing or invalid access token.", body = ErrorResponse),
        (status = 403, description = "Requires member-credential permission.", body = ErrorResponse),
        (status = 404, description = "Not a member of this organization.", body = ErrorResponse),
        (status = 409, description = "Address already in use, unchanged, member inactive, or a concurrent request won.", body = ErrorResponse),
        (status = 429, description = "Per-caller or per-target limit exhausted.", body = ErrorResponse),
        (status = 503, description = "SMTP is not configured on this deployment.", body = ErrorResponse),
    ),
)]
pub async fn request_member_email_change(
    auth: AuthUser,
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: axum::http::HeaderMap,
    Path((org_id, user_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<RequestEmailChangeReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut conn = db(&state).await?;

    // `member:credential` in ADDITION to the `member:manage` that
    // `guard_member_admin_action` demands first. Moving someone's login
    // identity is at least as severe as forcing their password reset, and an
    // org that handed out routine grant administration has not agreed to it.
    authorize_org(&mut conn, auth.user_id, org_id, perm::MEMBER_CREDENTIAL).await?;

    // Resolved BEFORE anything is written, exactly as `reset_member_password`
    // does: a pending change must never exist when the mail carrying its veto
    // cannot be sent. There is no `cancel`-style exemption here because this
    // endpoint has no non-mailing mode — the DELETE sibling is the undo, and it
    // deliberately does not touch mail config.
    let mail = state.mail.as_ref().cloned().ok_or_else(|| {
        ApiError::Unavailable("unavailable", "SMTP is not configured on this server".into())
    })?;
    let dashboard_url = state
        .cfg
        .require_dashboard_url()
        .map_err(|e| ApiError::Unavailable("unavailable", e.to_string()))?
        .to_string();

    rate_limit(
        &state,
        &format!("sauron:auth:emailchange:{}", auth.user_id),
        EMAIL_CHANGE_PER_CALLER_PER_HOUR,
        3600,
    )
    .await?;
    rate_limit(
        &state,
        &format!("sauron:auth:emailchange:target:{user_id}"),
        EMAIL_CHANGE_PER_TARGET_PER_HOUR,
        3600,
    )
    .await?;

    // The whole shared stack: member:manage, user-exists 404, grant-in-this-org
    // 404, self-target 409, no-escalation, and the unwaivable cross-org
    // refusal. `allow_self` is false — an admin changing their own address
    // belongs on an account page, and self-service is explicitly out of scope.
    let _target_grants =
        guard_member_admin_action(&mut conn, auth.user_id, org_id, user_id, false).await?;

    let user = repo::get_user(&mut conn, user_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if !user.is_active {
        return Err(ApiError::Conflict(
            "reactivate this member before changing their email".into(),
        ));
    }

    let new_email = req.new_email.trim().to_lowercase();
    if !new_email.contains('@') || new_email.len() > 320 {
        return Err(ApiError::BadRequest("a valid email is required".into()));
    }
    // `users_email_lower_key` is on `lower(email)`, so the comparison is too.
    if new_email == user.email.to_lowercase() {
        return Err(ApiError::Conflict(
            "that is already this member's address".into(),
        ));
    }
    // Checked here for a usable error message; re-checked at confirm time
    // because the address can be claimed during the link's 24-hour life, and
    // re-enforced by the unique index because neither check is a lock.
    if repo::find_user_by_email(&mut conn, &new_email)
        .await?
        .is_some()
    {
        return Err(ApiError::Conflict(
            "a user with that email already exists".into(),
        ));
    }

    // Best-effort: the index below is the guarantee. This exists so the common
    // single-admin case reads as "supersede" rather than as a 409 the admin has
    // to resolve by hand.
    repo::cancel_live_email_change_for_user(
        &mut conn,
        user_id,
        repo::EMAIL_CHANGE_CANCELLED_SUPERSEDED,
    )
    .await?;

    let approve_raw = sauron_core::ids::opaque_token();
    let cancel_raw = sauron_core::ids::opaque_token();
    let expires_at = Utc::now() + chrono::Duration::seconds(EMAIL_CHANGE_TTL_SECS);

    match repo::insert_email_change_request(
        &mut conn,
        sauron_db::models::NewEmailChangeRequest {
            user_id,
            org_id,
            new_email: new_email.clone(),
            approve_token_hash: sauron_auth::hash_token(&approve_raw),
            cancel_token_hash: sauron_auth::hash_token(&cancel_raw),
            email_fingerprint: user.email.to_lowercase(),
            initiated_by: Some(auth.user_id),
            requested_from: Some(client_addr(&headers, &peer, &state)),
            expires_at,
        },
    )
    .await
    {
        Ok(_) => {}
        // Another admin opened a request for this member between our supersede
        // and our insert — `email_change_one_live_per_user` caught it. Reporting
        // it is right: silently retrying would mean whichever admin lost the
        // race has their address quietly discarded.
        Err(diesel::result::Error::DatabaseError(
            diesel::result::DatabaseErrorKind::UniqueViolation,
            _,
        )) => {
            return Err(ApiError::Conflict(
                "another administrator just opened a change for this member — reload and try again"
                    .into(),
            ))
        }
        Err(e) => return Err(e.into()),
    }

    let org = repo::get_org(&mut conn, org_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let org_name = org.name.clone();
    let display_name = if user.name.trim().is_empty() {
        user.email.clone()
    } else {
        user.name.clone()
    };
    let old_email = user.email.clone();

    // Recorded while the connection is held and BEFORE the enqueue: the pending
    // row exists whether or not either mail goes out, and auditing after the
    // enqueue would lose exactly the case worth investigating — a request that
    // was opened but never announced. Neither raw token is recorded. The new
    // address is: the acting admin typed it, and the audit log is an admin
    // surface, so it discloses nothing new.
    crate::audit::record(
        &mut conn,
        auth.user_id,
        crate::audit::Entry::new(
            org_id,
            crate::audit::action::MEMBER_EMAIL_CHANGE_REQUEST,
            crate::audit::entity::MEMBER,
        )
        .target(user_id, &old_email)
        .changes(crate::audit::created(
            crate::audit::entity::MEMBER,
            &[
                ("new_email", serde_json::json!(new_email)),
                ("expires_at", serde_json::json!(expires_at)),
            ],
        )),
    )
    .await;

    // `MailSender` checks out its own pooled connection; see the identical drop
    // and its reasoning in `routes::auth::forgot_password`.
    drop(conn);

    let ttl = std::time::Duration::from_secs(EMAIL_CHANGE_TTL_SECS as u64);

    // THE NOTICE GOES FIRST, and the order is the point. If only one of these
    // two enqueues survives, it must be the one carrying the veto: a member who
    // is warned but never sees a confirm link loses nothing (the request lapses
    // in 24 hours), while a member whose new address can confirm but who was
    // never warned has lost the only control they have.
    let notice = render_email_change_notice(EmailChangeMailVars {
        display_name: &display_name,
        org_name: &org_name,
        url: &email_change_cancel_link(&dashboard_url, &cancel_raw),
    })
    // Unreachable rather than merely unlikely: the only fallible step is
    // `Cta::new` refusing a non-http(s) href, and `require_dashboard_url()`
    // already returned Ok, which happens only for an http(s) origin.
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    mail.enqueue(MailKind::EmailChangeNotice, &old_email, &notice, Some(user_id), ttl)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    let approval = render_email_change_approval(EmailChangeMailVars {
        display_name: &display_name,
        org_name: &org_name,
        url: &email_change_confirm_link(&dashboard_url, &approve_raw),
    })
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    // `user_id` is passed for the outbox's own bookkeeping, but the recipient is
    // an address the account does not own yet — which is exactly why the notice
    // above went to the address it does own.
    mail.enqueue(
        MailKind::EmailChangeApproval,
        &new_email,
        &approval,
        Some(user_id),
        ttl,
    )
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;

    // Never returns either raw token. `member:credential` lets its holder
    // disrupt a member's account, not sign in as them.
    Ok(Json(serde_json::json!({
        "ok": true,
        "new_email": new_email,
        "expires_at": expires_at.to_rfc3339(),
    })))
}

/// Withdraw a pending change.
///
/// Deliberately does not require SMTP: gating the undo on the configuration
/// whose failure motivates it would make it unreachable in exactly the
/// deployment that needs it. Same reasoning `reset_member_password`'s `cancel`
/// arm records.
///
/// Spends the per-caller budget only. It sends no mail and can only ever
/// withdraw, so charging it to the per-target bucket would mean an admin who
/// opened five requests in an hour cannot withdraw the fifth.
#[utoipa::path(
    delete, path = "/v1/orgs/{org_id}/members/{user_id}/email-change", tag = "Organizations",
    summary = "Withdraw a pending email change",
    description = "Invalidates the outstanding confirmation link. Safe to call when nothing is pending.",
    params(("org_id" = Uuid, Path, description = "The organization."), ("user_id" = Uuid, Path, description = "The member.")),
    security(("bearerAuth" = [])),
    responses(
        (status = 200, description = "Withdrawn, or nothing was pending.", body = OkResponse),
        (status = 401, description = "Missing or invalid access token.", body = ErrorResponse),
        (status = 403, description = "Requires member-credential permission.", body = ErrorResponse),
        (status = 404, description = "Not a member of this organization.", body = ErrorResponse),
        (status = 429, description = "Per-caller limit exhausted.", body = ErrorResponse),
    ),
)]
pub async fn cancel_member_email_change(
    auth: AuthUser,
    State(state): State<AppState>,
    Path((org_id, user_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_org(&mut conn, auth.user_id, org_id, perm::MEMBER_CREDENTIAL).await?;
    rate_limit(
        &state,
        &format!("sauron:auth:emailchange:{}", auth.user_id),
        EMAIL_CHANGE_PER_CALLER_PER_HOUR,
        3600,
    )
    .await?;
    let _ = guard_member_admin_action(&mut conn, auth.user_id, org_id, user_id, false).await?;

    let n = repo::cancel_live_email_change_for_user(
        &mut conn,
        user_id,
        repo::EMAIL_CHANGE_CANCELLED_ADMIN,
    )
    .await?;

    if n > 0 {
        let target_email = repo::user_email(&mut conn, user_id)
            .await?
            .unwrap_or_default();
        crate::audit::record(
            &mut conn,
            auth.user_id,
            crate::audit::Entry::new(
                org_id,
                crate::audit::action::MEMBER_EMAIL_CHANGE_CANCELLED,
                crate::audit::entity::MEMBER,
            )
            .target(user_id, &target_email)
            .changes(crate::audit::created(
                crate::audit::entity::MEMBER,
                &[("cancelled_reason", serde_json::json!("admin"))],
            )),
        )
        .await;
    }

    Ok(Json(serde_json::json!({ "ok": true, "withdrawn": n > 0 })))
}
```

Add to `orgs.rs`'s imports: `EMAIL_CHANGE_PER_CALLER_PER_HOUR`, `EMAIL_CHANGE_PER_TARGET_PER_HOUR`, `EMAIL_CHANGE_TTL_SECS`, `EmailChangeMailVars`, `email_change_cancel_link`, `email_change_confirm_link`, `render_email_change_approval`, `render_email_change_notice` from `crate::routes::auth`.

- [ ] **Step 3: Wire the routes**

In `main.rs`, beside the `password-reset` route:

```rust
        .route(
            "/v1/orgs/{org_id}/members/{user_id}/email-change",
            post(routes::orgs::request_member_email_change)
                .delete(routes::orgs::cancel_member_email_change),
        )
```

CORS already allows both `POST` and `DELETE` (`main.rs:459`), so no method needs adding.

- [ ] **Step 4: Register in OpenAPI**

Add both handler paths to the `paths(...)` list and `RequestEmailChangeReq` to `components(schemas(...))` in `openapi.rs`. The parity test parses `main.rs`, so every route added above must appear here or it fails.

- [ ] **Step 5: Verify**

```bash
cd backend && cargo test -p sauron-api --lib 2>&1 | tail -20
```
Expected: pass, including the OpenAPI route-parity test.

---

### Task 6: Public endpoints

**Files:**
- Modify: `backend/bins/sauron-api/src/routes/auth.rs` (append after `reset_password`)
- Modify: `backend/bins/sauron-api/src/main.rs` (three routes)
- Modify: `backend/bins/sauron-api/src/openapi.rs`

**Interfaces:**
- Consumes: Task 2's repo functions; Task 5's limiter constants.
- Produces:
  - `routes::auth::preview_email_change` — `POST /v1/auth/email-change/preview`
  - `routes::auth::confirm_email_change` — `POST /v1/auth/email-change/confirm`
  - `routes::auth::cancel_email_change` — `POST /v1/auth/email-change/cancel`
  - `routes::auth::EmailChangeTokenReq { pub token: String }`
  - `routes::auth::EmailChangePreview { pub role: String, pub org_name: String, pub expires_at: String, pub new_email: Option<String> }`

- [ ] **Step 1: Implement**

```rust
#[derive(Deserialize, utoipa::ToSchema)]
pub struct EmailChangeTokenReq {
    pub token: String,
}

/// What a public email-change page may know before the visitor acts.
///
/// `new_email` is `Option` and `skip_serializing_if` — NOT nulled — because the
/// cancel page is reached from the mail sent to the OLD address, and that
/// address must never learn the new one. Enforcing it here rather than in the
/// page is what makes the rule hold: a template can be edited by someone who
/// never read the spec, but this endpoint is the only source the page has.
#[derive(Serialize, utoipa::ToSchema)]
pub struct EmailChangePreview {
    /// `"approve"` or `"cancel"` — which token the caller presented.
    pub role: String,
    pub org_name: String,
    pub expires_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_email: Option<String>,
}

/// Shared preamble for the three public endpoints: shape check, IP limiter,
/// per-token limiter. Returns the token's hash.
///
/// The shape check runs first and on every path, so a spray of garbage never
/// reaches Redis — otherwise the per-token limiter mints one key per guess.
async fn email_change_token_preamble(
    state: &AppState,
    headers: &axum::http::HeaderMap,
    peer: &SocketAddr,
    token: &str,
) -> Result<String, ApiError> {
    if !is_reset_token_shape(token) {
        return Err(ApiError::Auth(AuthError::InvalidToken));
    }
    rate_limit(
        state,
        &format!(
            "sauron:auth:emailchange:ip:{}",
            client_addr(headers, peer, state)
        ),
        EMAIL_CHANGE_ATTEMPTS_PER_MIN_PER_IP,
        60,
    )
    .await?;
    let hash = hash_token(token);
    // Keyed on the hash, so nothing sensitive lands in Redis.
    rate_limit(
        state,
        &format!("sauron:auth:emailchange:tok:{hash}"),
        EMAIL_CHANGE_ATTEMPTS_PER_TOKEN_PER_HOUR,
        3600,
    )
    .await?;
    Ok(hash)
}

/// Describe a pending change to whoever holds one of its two links.
#[utoipa::path(
    post, path = "/v1/auth/email-change/preview", tag = "Authentication",
    summary = "Describe a pending email change",
    description = "\
The token travels in the BODY, never a query string, so it reaches no server \
log or `Referer`.

`new_email` is present only for the confirmation token. The cancellation token \
belongs to the address being replaced, which is never told what it is being \
replaced with.",
    request_body(content = EmailChangeTokenReq),
    responses(
        (status = 200, description = "The pending change.", body = EmailChangePreview),
        (status = 401, description = "No live request matches this token.", body = ErrorResponse),
        (status = 429, description = "Rate limited.", body = ErrorResponse),
    ),
)]
pub async fn preview_email_change(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: axum::http::HeaderMap,
    Json(req): Json<EmailChangeTokenReq>,
) -> Result<Json<EmailChangePreview>, ApiError> {
    let hash = email_change_token_preamble(&state, &headers, &peer, &req.token).await?;
    let mut conn = db(&state).await?;

    // Approve first, then cancel. A token is one or the other; both columns are
    // UNIQUE and independently random, so there is no ambiguity to resolve.
    let (row, role) = match repo::find_live_email_change_by_approve_token(&mut conn, &hash).await? {
        Some(r) => (r, "approve"),
        None => match repo::find_live_email_change_by_cancel_token(&mut conn, &hash).await? {
            Some(r) => (r, "cancel"),
            // Expired, already used, cancelled, or never existed — one answer
            // for all four. The distinction is for the audit log, not for
            // whoever is holding the link.
            None => return Err(ApiError::Auth(AuthError::InvalidToken)),
        },
    };

    let org_name = repo::get_org(&mut conn, row.org_id)
        .await?
        .map(|o| o.name)
        .unwrap_or_default();

    Ok(Json(EmailChangePreview {
        role: role.to_string(),
        org_name,
        expires_at: row.expires_at.to_rfc3339(),
        // The one line the non-disclosure rule rests on.
        new_email: if role == "approve" {
            Some(row.new_email)
        } else {
            None
        },
    }))
}

/// Adopt the new address.
#[utoipa::path(
    post, path = "/v1/auth/email-change/confirm", tag = "Authentication",
    summary = "Confirm a new email address",
    description = "\
Moves the account's sign-in address. Existing sessions are deliberately NOT \
revoked — access and refresh tokens carry a user id, not an address, so nothing \
breaks and nobody is signed out. Outstanding password-reset links ARE \
invalidated: they were mailed to an address the account no longer owns.",
    request_body(content = EmailChangeTokenReq),
    responses(
        (status = 200, description = "The address was changed.", body = OkResponse),
        (status = 401, description = "No live request matches this token.", body = ErrorResponse),
        (status = 409, description = "That address now belongs to another account.", body = ErrorResponse),
        (status = 429, description = "Rate limited.", body = ErrorResponse),
    ),
)]
pub async fn confirm_email_change(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: axum::http::HeaderMap,
    Json(req): Json<EmailChangeTokenReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let hash = email_change_token_preamble(&state, &headers, &peer, &req.token).await?;
    let mut conn = db(&state).await?;

    let row = repo::find_live_email_change_by_approve_token(&mut conn, &hash)
        .await?
        .ok_or(ApiError::Auth(AuthError::InvalidToken))?;
    let user = repo::get_user(&mut conn, row.user_id)
        .await?
        .ok_or(ApiError::Auth(AuthError::InvalidToken))?;
    // The holder controls the mailbox, so telling them is honest rather than a
    // leak — same judgement `reset_password` makes.
    if !user.is_active {
        return Err(ApiError::Auth(AuthError::AccountDeactivated));
    }

    // Re-checked at the point of use, not merely at request time: this link
    // lives 24 hours, and `create_member` can claim the address inside that
    // window. Returns WITHOUT burning the row — the address may be freed again,
    // and `EMAIL_CHANGE_ATTEMPTS_PER_TOKEN_PER_HOUR` bounds the retries.
    if let Some(other) = repo::find_user_by_email(&mut conn, &row.new_email).await? {
        if other.id != row.user_id {
            return Err(ApiError::Conflict(
                "that address now belongs to another account".into(),
            ));
        }
    }

    // The fingerprint is matched inside this UPDATE, so "the address has not
    // moved since the link was issued" and "the link is burned" are one atomic
    // fact rather than two statements with a window between them.
    let Some((user_id, new_email)) =
        repo::consume_email_change_approval(&mut conn, &hash, &user.email.to_lowercase()).await?
    else {
        return Err(ApiError::Auth(AuthError::InvalidToken));
    };

    // The residual race: another account claimed the address between the
    // check above and here. The link is correctly spent and the other account
    // keeps the address.
    match repo::set_user_email(&mut conn, user_id, &new_email).await {
        Ok(_) => {}
        Err(diesel::result::Error::DatabaseError(
            diesel::result::DatabaseErrorKind::UniqueViolation,
            _,
        )) => {
            return Err(ApiError::Conflict(
                "that address now belongs to another account".into(),
            ))
        }
        Err(e) => return Err(e.into()),
    }

    // Sessions are deliberately untouched — see this endpoint's description.
    //
    // Reset links are not. `password_reset_tokens.password_fingerprint` kills a
    // link when the PASSWORD moves, never when the ADDRESS does, so without
    // this a reset link mailed to the old address stays redeemable by whoever
    // still reads that mailbox — a full account takeover, days later, by
    // someone the change was meant to move the account away from.
    repo::invalidate_password_reset_tokens_for_user(
        &mut conn,
        user_id,
        repo::RESET_INVALIDATED_SUPERSEDED,
    )
    .await?;

    // `audit::record` takes a non-optional actor. The target user is the honest
    // one: this path is unauthenticated, and the only thing anyone proved is
    // control of the mailbox. Naming the admin would record them as the actor
    // of an act they did not perform.
    crate::audit::record(
        &mut conn,
        user_id,
        crate::audit::Entry::new(
            row.org_id,
            crate::audit::action::MEMBER_EMAIL_CHANGE_APPROVED,
            crate::audit::entity::MEMBER,
        )
        .target(user_id, &new_email)
        .changes(crate::audit::created(
            crate::audit::entity::MEMBER,
            &[("new_email", serde_json::json!(new_email))],
        )),
    )
    .await;

    Ok(Json(serde_json::json!({ "ok": true })))
}

/// Veto a pending change, from the address being replaced.
#[utoipa::path(
    post, path = "/v1/auth/email-change/cancel", tag = "Authentication",
    summary = "Cancel a pending email change",
    description = "Invalidates the confirmation link. The response never names the address that was requested.",
    request_body(content = EmailChangeTokenReq),
    responses(
        (status = 200, description = "The request was cancelled.", body = OkResponse),
        (status = 401, description = "No live request matches this token.", body = ErrorResponse),
        (status = 429, description = "Rate limited.", body = ErrorResponse),
    ),
)]
pub async fn cancel_email_change(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: axum::http::HeaderMap,
    Json(req): Json<EmailChangeTokenReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let hash = email_change_token_preamble(&state, &headers, &peer, &req.token).await?;
    let mut conn = db(&state).await?;

    let Some((user_id, org_id)) =
        repo::cancel_email_change_by_cancel_token(&mut conn, &hash).await?
    else {
        return Err(ApiError::Auth(AuthError::InvalidToken));
    };

    let target_email = repo::user_email(&mut conn, user_id)
        .await?
        .unwrap_or_default();
    // The signal worth investigating: a member rejected an admin's attempt to
    // move their login identity. Distinguished from an admin withdrawal by
    // `cancelled_reason`.
    crate::audit::record(
        &mut conn,
        user_id,
        crate::audit::Entry::new(
            org_id,
            crate::audit::action::MEMBER_EMAIL_CHANGE_CANCELLED,
            crate::audit::entity::MEMBER,
        )
        .target(user_id, &target_email)
        .changes(crate::audit::created(
            crate::audit::entity::MEMBER,
            &[("cancelled_reason", serde_json::json!("user"))],
        )),
    )
    .await;

    // Says nothing about what was requested.
    Ok(Json(serde_json::json!({ "ok": true })))
}
```

- [ ] **Step 2: Wire the routes**

```rust
        .route("/v1/auth/email-change/preview", post(routes::auth::preview_email_change))
        .route("/v1/auth/email-change/confirm", post(routes::auth::confirm_email_change))
        .route("/v1/auth/email-change/cancel", post(routes::auth::cancel_email_change))
```

These must sit with the other **unauthenticated** `/v1/auth/*` routes — before the auth middleware layer, alongside `/v1/auth/reset-password`.

- [ ] **Step 3: Register in OpenAPI** — three paths, plus `EmailChangeTokenReq` and `EmailChangePreview` schemas.

- [ ] **Step 4: Verify**

```bash
cd backend && cargo test -p sauron-api --lib 2>&1 | tail -20 && cargo clippy -p sauron-api 2>&1 | tail -10
```
Expected: pass, no clippy warnings.

---

### Task 7: Members list badge and the reaper

**Files:**
- Modify: `backend/bins/sauron-api/src/routes/orgs.rs` (`MemberGrant`, `list_members`)
- Modify: `backend/bins/sauron-api/src/main.rs:68` and `:412-434` (reaper)

**Interfaces:**
- Consumes: `repo::live_email_changes_for_org`, `repo::prune_email_change_requests` from Task 2.
- Produces: `orgs::PendingEmailChange { pub new_email: String, pub expires_at: DateTime<Utc> }`, and `MemberGrant.pending_email_change: Option<PendingEmailChange>`.

- [ ] **Step 1: Extend `MemberGrant`**

```rust
/// A change to this member's sign-in address, awaiting the new address's
/// confirmation.
#[derive(Serialize, Clone, utoipa::ToSchema)]
pub struct PendingEmailChange {
    pub new_email: String,
    pub expires_at: DateTime<Utc>,
}
```

Add to `MemberGrant`:

```rust
    /// Non-null while a change to this member's email is awaiting confirmation.
    ///
    /// `GET /v1/orgs/{org}/members` is the only place the dashboard learns
    /// anything about a member's account state; without this the withdraw
    /// action exists on the server and is unreachable from the UI, which is the
    /// same as not existing.
    pub pending_email_change: Option<PendingEmailChange>,
```

- [ ] **Step 2: Populate it in `list_members`**

A second query rather than widening `list_org_grants`'s already-six-wide tuple. One row per member, joined in memory — the org's live-request count is bounded by its member count, and the query rides `email_change_live_by_org`.

```rust
    let rows = repo::list_org_grants(&mut conn, org_id).await?;
    let pending: std::collections::HashMap<Uuid, PendingEmailChange> =
        repo::live_email_changes_for_org(&mut conn, org_id)
            .await?
            .into_iter()
            .map(|(user_id, new_email, expires_at)| {
                (user_id, PendingEmailChange { new_email, expires_at })
            })
            .collect();

    let members = rows
        .into_iter()
        .map(
            |(g, email, name, role_name, is_active, credentials_invalidated_at)| MemberGrant {
                pending_email_change: pending.get(&g.user_id).cloned(),
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
        .collect();
```

- [ ] **Step 3: Extend the reaper**

In `main.rs`, beside `PASSWORD_RESET_RETENTION_DAYS`:

```rust
/// Matches `PASSWORD_RESET_RETENTION_DAYS`, and for the same reason: the row is
/// the only record that an admin tried to move someone's login identity.
const EMAIL_CHANGE_RETENTION_DAYS: i64 = 30;
```

Rename the supervised task `password_reset_reaper` → `credential_token_reaper` and add the second delete inside the same tick — one connection checkout per hour against a pool of 16, rather than two loops competing for it:

```rust
                    let removed = sauron_db::repo::prune_password_reset_tokens(
                        &mut conn,
                        PASSWORD_RESET_RETENTION_DAYS,
                    )
                    .await?;
                    let removed_changes = sauron_db::repo::prune_email_change_requests(
                        &mut conn,
                        EMAIL_CHANGE_RETENTION_DAYS,
                    )
                    .await?;
                    drop(conn);
                    if removed > 0 || removed_changes > 0 {
                        tracing::info!(
                            reset_tokens = removed,
                            email_changes = removed_changes,
                            "pruned expired credential tokens"
                        );
                    }
```

- [ ] **Step 4: Verify**

```bash
cd backend && cargo test -p sauron-api --lib 2>&1 | tail -15
```
Expected: pass. If a test asserts on the task name `password_reset_reaper`, update it.

---

### Task 8: HTTP integration tests

**Files:**
- Create: `backend/bins/sauron-api/tests/http_email_change.rs`

**Interfaces:**
- Consumes: every endpoint from Tasks 5 and 6.
- Produces: nothing consumed by later tasks.

- [ ] **Step 1: Copy the harness**

Copy the whole `TestServer` / `swap_database` / `free_port` scaffolding from `backend/bins/sauron-api/tests/http_password_reset.rs` verbatim, including the skip-when-unset guard and the `JWT_SECRET` const (change its value to `http-email-change-test-secret-000000000000`). The suites duplicate this deliberately; do not factor it out.

- [ ] **Step 2: Write the cases**

Each is a `#[tokio::test]`. Together they must cover:

1. `a_request_mails_both_addresses_and_changes_nothing_yet` — after `POST …/email-change`, `users.email` is unchanged; `mail_outbox` holds exactly two rows, one per address, with kinds `email_change_notice` and `email_change_approval`.
2. `the_notice_body_never_contains_the_new_address` — read the queued `email_change_notice` row's body out of `mail_outbox` and assert the new address does not appear in it.
3. `confirming_moves_the_address` — extract the approve token from the queued approval mail, `POST /v1/auth/email-change/confirm`, assert `users.email` moved and login with the new address succeeds while login with the old one fails.
4. `confirming_leaves_sessions_live` — sign in before confirming, confirm, then use the pre-existing access token on an authenticated route and assert **200**. This choice is deliberate and this test is what stops someone "fixing" it into a revoke.
5. `confirming_invalidates_outstanding_reset_links` — force an admin password reset, then confirm the email change, then assert the reset link now fails.
6. `the_old_address_can_veto` — extract the cancel token from the notice mail, `POST /v1/auth/email-change/cancel`, then assert the approve token no longer confirms.
7. `preview_with_a_cancel_token_omits_the_new_address` — assert the JSON object has **no `new_email` key at all** (`body.get("new_email").is_none()`), not merely a null.
8. `preview_with_an_approve_token_includes_the_new_address`.
9. `a_second_request_supersedes_the_first` — open, then open again with a different address; assert the first approve token is dead and the second works.
10. `two_simultaneous_confirms_yield_exactly_one_success` — `tokio::join!` two confirms of the same token; assert exactly one 200. Model this on `http_password_reset.rs`'s existing simultaneous-use test.
11. `an_address_claimed_after_issue_is_refused_without_burning_the_link` — open a request, create another user holding that address, confirm → 409; then delete that user and confirm again → 200. The second half is what proves the row was not burned.
12. `smtp_unconfigured_returns_503_and_writes_no_row` — start the server with no SMTP config; assert 503 and `SELECT count(*) FROM email_change_requests` is 0.
13. `guards` — one test each: inactive target 409, self-target 409, cross-org member 409, a caller without `member:credential` 403, and a caller trying to act on someone who outranks them 403.
14. `an_expired_request_confirms_nothing` — insert a row directly with `expires_at` in the past; assert confirm returns 401.
15. `garbage_tokens_are_refused_on_shape` — a 10-character token returns 401 without consuming a per-token limiter key.

- [ ] **Step 3: Run**

```bash
cd backend && time cargo test -p sauron-api --test http_email_change 2>&1 | tail -30
```
Expected: all pass. **Check the elapsed time.** This suite spawns real server processes against ephemeral databases; a sub-second run means `TEST_DATABASE_URL` or `TEST_REDIS_URL` is unset and the tests skipped. Record the real duration.

---

### Task 9: Dashboard — admin side

**Files:**
- Modify: `dashboard/src/lib/models/index.ts` (`Member`, `MemberGrant` types)
- Modify: `dashboard/src/lib/api/orgs.ts`
- Create: `dashboard/src/lib/components/members/ChangeEmailDialog.svelte`
- Modify: `dashboard/src/lib/components/members/MembersTable.svelte`
- Modify: `dashboard/src/pages/Members.svelte`
- Modify: `dashboard/src/lib/models/audit.ts`
- Modify: `dashboard/src/lib/i18n/catalog/admin.ts` and `prose.ts`

**Interfaces:**
- Consumes: Task 5 and 7's endpoints and `pending_email_change` field.
- Produces: `requestMemberEmailChange(orgId, userId, newEmail)`, `cancelMemberEmailChange(orgId, userId)`.

- [ ] **Step 1: Extend the model type**

```ts
export interface PendingEmailChange {
  new_email: string;
  expires_at: string;
}
```

Add to `Member`: `pending_email_change: PendingEmailChange | null;`

- [ ] **Step 2: Add the API calls**

```ts
/**
 * Open a pending change to a member's sign-in address.
 *
 * Returns without anything having changed: the address moves only when the new
 * mailbox confirms. Goes through `api`, not `bareClient` — it needs the bearer.
 */
export async function requestMemberEmailChange(
  orgId: string,
  userId: string,
  newEmail: string,
): Promise<{ new_email: string; expires_at: string }> {
  const { data } = await api.post<{ new_email: string; expires_at: string }>(
    `/v1/orgs/${orgId}/members/${userId}/email-change`,
    { new_email: newEmail },
  );
  return data;
}

/** Withdraw a pending change. Works on a deployment with no SMTP configured. */
export async function cancelMemberEmailChange(orgId: string, userId: string): Promise<void> {
  await api.delete(`/v1/orgs/${orgId}/members/${userId}/email-change`);
}
```

- [ ] **Step 3: Build `ChangeEmailDialog.svelte`**

Same house components and `Props` shape as `ResetPasswordDialog.svelte`. The
prose states the two facts an admin will otherwise get wrong: nothing changes
until the member confirms from the new address, and the member's **current**
address is notified and can stop it.

```svelte
<script lang="ts">
  import { t } from '../../i18n';
  import Modal from '../ui/Modal.svelte';
  import Button from '../ui/Button.svelte';
  import Input from '../ui/Input.svelte';
  import type { Member } from '../../models';

  interface Props {
    member: Member;
    busy: boolean;
    onconfirm: (newEmail: string) => void;
    oncancel: () => void;
  }

  let { member, busy, onconfirm, oncancel }: Props = $props();

  let newEmail = $state('');

  // Trimmed and lower-cased here as well as on the server, so the "same
  // address" guard below matches what the server will compare.
  const normalized = $derived(newEmail.trim().toLowerCase());
  const canSubmit = $derived(
    normalized.includes('@') && normalized !== member.email.toLowerCase() && !busy,
  );

  function submit(event: SubmitEvent) {
    event.preventDefault();
    if (!canSubmit) return;
    onconfirm(normalized);
  }
</script>

<Modal open title={t('members.changeEmail.title')} dismissible={!busy} onclose={oncancel}>
  <!-- Stated BEFORE the input, because an admin who reads only the button
       label will assume this takes effect immediately and be surprised when
       the member keeps signing in with the old address. -->
  <p class="lead">{t('prose.members.changeEmailWarning')}</p>
  <form onsubmit={submit}>
    <Input
      type="email"
      bind:value={newEmail}
      placeholder={member.email}
      label={t('members.changeEmail.newAddress')}
      disabled={busy}
    />
  </form>

  {#snippet footer()}
    <Button variant="ghost" onclick={oncancel} disabled={busy}>
      {t('members.reset.neverMind')}
    </Button>
    <Button variant="primary" loading={busy} disabled={!canSubmit} onclick={() => onconfirm(normalized)}>
      {t('members.changeEmail.submit')}
    </Button>
  {/snippet}
</Modal>

<style>
  .lead {
    font-size: 14px;
    line-height: 1.5;
    margin-bottom: 10px;
  }
  form {
    margin-top: 4px;
  }
</style>
```

- [ ] **Step 4: Badge and actions in `MembersTable.svelte`**

Where `credentials_invalidated_at` already renders a "Reset pending" badge, add an "Email change pending" badge fed by `member.pending_email_change`, showing the requested address and its expiry, plus a withdraw action. Reuse the existing badge markup and styles rather than inventing a second visual language.

- [ ] **Step 5: Wire the page**

In `Members.svelte`, add the dialog state, the two handlers, a toast on success, and a refetch of the members list afterwards so the badge appears without a reload.

- [ ] **Step 6: Audit labels**

In `dashboard/src/lib/models/audit.ts`, add to `VERBS`:

```ts
  email_change_request: 'Requested email change for',
  email_change_approved: 'Confirmed new email for',
  email_change_cancelled: 'Cancelled email change for',
```

and add all three verbs to the `CREDENTIAL` set — they move a login identity, which is the emphasis that set exists for.

- [ ] **Step 7: i18n**

Every new string gets `{ en, ar }` in `catalog/admin.ts` (labels) or `catalog/prose.ts` (sentences). Suggested keys: `members.emailChangePending`, `members.changeEmail`, `members.changeEmail.title`, `members.changeEmail.submit`, `members.changeEmail.withdraw`, `prose.members.changeEmailWarning`, `prose.members.emailChangePending`.

- [ ] **Step 8: Verify**

```bash
cd dashboard && npm run check && npm test 2>&1 | tail -20
```
Expected: pass, including the i18n catalog and leak tests.

---

### Task 10: Dashboard — public pages

**Files:**
- Create: `dashboard/src/lib/models/email-change.ts`
- Create: `dashboard/src/lib/models/email-change.test.ts`
- Create: `dashboard/src/pages/ConfirmEmailChange.svelte`
- Create: `dashboard/src/pages/CancelEmailChange.svelte`
- Modify: `dashboard/src/lib/api/auth.ts`
- Modify: `dashboard/src/routes.ts`
- Modify: `dashboard/src/lib/models/page-access.test.ts`
- Modify: `dashboard/src/lib/i18n/catalog/auth.ts`, `prose.ts`

**Interfaces:**
- Consumes: Task 6's three public endpoints.
- Produces: `readEmailChangeToken(qs: string | null): string | null`; `previewEmailChange(token)`, `confirmEmailChange(token)`, `cancelEmailChange(token)` in `api/auth.ts`.

- [ ] **Step 1: Write the failing model test**

`email-change.test.ts` — mirror `password-reset.test.ts`'s shape:

```ts
import { describe, it, expect } from 'vitest';
import { readEmailChangeToken } from './email-change';

describe('readEmailChangeToken', () => {
  it('reads the token from a query string', () => {
    expect(readEmailChangeToken('token=abc123')).toBe('abc123');
  });

  it('returns null for a bare page with no query', () => {
    // svelte-spa-router types `querystring` as `string | undefined` and it is
    // genuinely undefined for `#/confirm-email-change` with no query at all.
    expect(readEmailChangeToken(null)).toBeNull();
  });

  it('returns null for a query carrying no token', () => {
    expect(readEmailChangeToken('foo=bar')).toBeNull();
  });

  it('returns null for an empty token', () => {
    expect(readEmailChangeToken('token=')).toBeNull();
  });
});
```

- [ ] **Step 2: Run to verify it fails**

```bash
cd dashboard && npx vitest run src/lib/models/email-change.test.ts 2>&1 | tail -15
```
Expected: FAIL — module not found.

- [ ] **Step 3: Implement the model**

```ts
/**
 * Pull the token out of a `#/confirm-email-change?token=…` fragment query.
 *
 * The token lives in the fragment so it is never sent in a request line or a
 * `Referer` — it reaches no server log, proxy log, or analytics beacon. Read
 * ONCE at page init, never reactively, so a later navigation cannot swap it
 * mid-submit. Same house pattern as `readResetToken`.
 */
export function readEmailChangeToken(qs: string | null): string | null {
  if (!qs) return null;
  const token = new URLSearchParams(qs).get('token');
  return token && token.length > 0 ? token : null;
}
```

- [ ] **Step 4: Run to verify it passes**

```bash
cd dashboard && npx vitest run src/lib/models/email-change.test.ts 2>&1 | tail -10
```
Expected: 4 passed.

- [ ] **Step 5: Add the API calls**

In `api/auth.ts`, using `bareClient` (no bearer — these are unauthenticated), with the token in the **body**:

```ts
export interface EmailChangePreview {
  role: 'approve' | 'cancel';
  org_name: string;
  expires_at: string;
  /** Present ONLY for the approve token. The address being replaced is never
      told what it is being replaced with. */
  new_email?: string;
}

export async function previewEmailChange(token: string): Promise<EmailChangePreview> {
  const { data } = await bareClient.post<EmailChangePreview>('/v1/auth/email-change/preview', {
    token,
  });
  return data;
}

export async function confirmEmailChange(token: string): Promise<void> {
  await bareClient.post('/v1/auth/email-change/confirm', { token });
}

export async function cancelEmailChange(token: string): Promise<void> {
  await bareClient.post('/v1/auth/email-change/cancel', { token });
}
```

- [ ] **Step 6: Build the two pages**

Both wrap `AuthLayout` and follow `ResetPassword.svelte`'s structure. `ConfirmEmailChange.svelte`:

```svelte
<script lang="ts">
  import { onMount } from 'svelte';
  import { t } from '../lib/i18n';
  import { querystring } from 'svelte-spa-router';
  import AuthLayout from '../lib/components/layout/AuthLayout.svelte';
  import Button from '../lib/components/ui/Button.svelte';
  import { previewEmailChange, confirmEmailChange } from '../lib/api/auth';
  import type { EmailChangePreview } from '../lib/api/auth';
  import { errorMessage, isNormalizedError } from '../lib/api/client';
  import { readEmailChangeToken } from '../lib/models/email-change';

  // Read ONCE at init, not reactively, so a later navigation cannot swap the
  // token mid-submit. `?? null` because svelte-spa-router types `querystring`
  // as `Readable<string | undefined>`.
  const token = readEmailChangeToken($querystring ?? null);

  let preview = $state<EmailChangePreview | null>(null);
  let loading = $state(true);
  let submitting = $state(false);
  let done = $state(false);
  let deadLink = $state(false);
  let error = $state<string | null>(null);

  // LOADS the request; never applies it. Outlook Safe Links and similar
  // scanners fetch mailed URLs, some of them executing JS — an onMount that
  // confirmed would approve changes nobody clicked, and the equivalent on the
  // cancel page would silently kill every pending request. The mutation fires
  // only from the button below.
  onMount(async () => {
    if (!token) {
      deadLink = true;
      loading = false;
      return;
    }
    try {
      preview = await previewEmailChange(token);
    } catch {
      deadLink = true;
    } finally {
      loading = false;
    }
  });

  async function confirm() {
    if (!token || submitting) return;
    error = null;
    submitting = true;
    try {
      await confirmEmailChange(token);
      done = true;
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

<AuthLayout title={t('auth.emailChange.confirmTitle')}>
  {#if loading}
    <p>{t('common.loading')}</p>
  {:else if deadLink}
    <p>{t('prose.auth.emailChangeDeadLink')}</p>
  {:else if done}
    <!-- No forced logout: sessions deliberately survive the change, so the
         user stays signed in everywhere and only the sign-in address moved. -->
    <p>{t('prose.auth.emailChangeConfirmed')}</p>
  {:else if preview}
    <p>{t('prose.auth.emailChangeIntro')}</p>
    <p class="addr">{preview.new_email}</p>
    {#if error}<p class="error">{error}</p>{/if}
    <Button variant="primary" loading={submitting} onclick={confirm}>
      {t('auth.emailChange.confirm')}
    </Button>
  {/if}
</AuthLayout>
```

`CancelEmailChange.svelte` is the same file with three differences, each load-bearing:

1. It calls `cancelEmailChange(token)` from a `variant="danger"` button labelled `t('auth.emailChange.stop')`.
2. **It never reads `preview.new_email`.** The server omits the key for a cancel token; the page must not display it even if a future API change starts sending it. Render `preview.org_name` only.
3. Its success copy states that nothing was changed, not that something was undone — the change had not taken effect.

Both render one "this link is no longer valid" state for a null token or any 401, with no distinction between expired, spent, and never-existed.

- [ ] **Step 7: Register the routes**

In `routes.ts`, beside `/reset-password`, condition-free:

```ts
  // Condition-free for the same reason as '/reset-password': a condition would
  // fire conditionsFailed and push the visitor to /login, making the mailed
  // link unusable. Also deliberately absent from App.svelte's PUBLIC_ROUTES —
  // that array drives an $effect that pushes authenticated users off those
  // paths, and a signed-in member clicking their own link would be bounced off
  // this page before they could use it.
  '/confirm-email-change': open(() => import('./pages/ConfirmEmailChange.svelte')),
  '/cancel-email-change': open(() => import('./pages/CancelEmailChange.svelte')),
```

Add both to `page-access.test.ts`'s `UNAUTHENTICATED` array. Add them to **neither** `SHELL_FLAGS` nor `PAGE_ACCESS` — their parity test pairs those two with each other, and unauthenticated pages appear in neither.

- [ ] **Step 8: i18n** — every string in both pages gets `{ en, ar }` in `catalog/auth.ts` / `catalog/prose.ts`.

- [ ] **Step 9: Verify**

```bash
cd dashboard && npm run check && npm test 2>&1 | tail -25
```
Expected: pass, including `page-access.test.ts`, `shell.test.ts`, and the i18n leak test.

- [ ] **Step 10: Drive it in a browser**

Static gates do not catch a dead button or an untranslated heading. Boot the stack, open both pages against a real token, and confirm each renders, that the cancel page shows no address, and that neither fires without a click.

---

## Verification

Before reporting done:

```bash
cd backend && cargo fmt --check && cargo clippy --workspace --all-targets 2>&1 | tail -20
```

A `cargo fmt` failure short-circuits CI and skips clippy and test entirely — a green-looking run where nothing was verified. Check that clippy and test actually ran.

```bash
cd backend && time cargo test -p sauron-db --test email_change && time cargo test -p sauron-api
cd dashboard && npm run check && npm test
```

Report the wall-clock duration of both backend suites. A suite that prints `ok` in 0.00s ran nothing.

**Do not commit.** Leave everything in the working tree.
