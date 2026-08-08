//! Repository functions. Each takes `&mut AsyncPgConnection` and returns a
//! `QueryResult`. Grouped by domain.

use chrono::{DateTime, Utc};
use diesel::dsl::sql;
use diesel::prelude::*;
use diesel::sql_types::{
    Array, BigInt, Bool, Date, Double, Integer, Jsonb, Nullable, SmallInt, Text, Timestamptz,
    Uuid as SqlUuid,
};
use diesel::upsert::excluded;
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl, SimpleAsyncConnection};
use sauron_inspector::targets::{PolicyNode, PolicyTargetType, ScanPair};
use sauron_inspector::units::{tables_for, units_for};
use serde_json::Value;
use std::collections::HashMap;
use uuid::Uuid;

use crate::models::*;
use crate::schema::*;
use crate::scope::{EnvFilter, ReadScope};

// ===========================================================================
// Users & refresh tokens
// ===========================================================================

pub async fn create_user(
    conn: &mut AsyncPgConnection,
    email: &str,
    password_hash: &str,
    name: &str,
) -> QueryResult<User> {
    let email = email.to_lowercase();
    diesel::insert_into(users::table)
        .values(NewUser {
            email: &email,
            password_hash,
            name,
        })
        .returning(User::as_returning())
        .get_result(conn)
        .await
}

#[derive(Debug, QueryableByName)]
pub struct NewMemberRow {
    #[diesel(sql_type = SqlUuid)]
    pub user_id: Uuid,
    #[diesel(sql_type = SqlUuid)]
    pub grant_id: Uuid,
}

/// Create a user and all of their initial grants in one statement.
///
/// A single data-modifying CTE rather than a transaction: Postgres runs both
/// INSERTs atomically within the statement, so a grant failure rolls the user
/// back for free. This avoids `conn.transaction`, whose diesel-async 0.9
/// signature needs async closures (Rust 1.85) and would push the workspace
/// MSRV past the 1.82 the RPM spec builds against. The scopes travel as two
/// parallel arrays unnested into rows, so N grants stay one round trip; they
/// must be the same length, as multi-argument `unnest` pads the shorter one
/// with NULLs that then fail `role_grants`' NOT NULL.
///
/// The caller must de-duplicate `(scope_type, scope_id)` pairs first: a repeat
/// trips `role_grants`' UNIQUE key, and that `UniqueViolation` is
/// indistinguishable here from the duplicate-email one below.
///
/// A duplicate email surfaces as `DatabaseError(UniqueViolation)` from
/// `users_email_lower_key`; the caller maps that to 409.
#[allow(clippy::too_many_arguments)]
pub async fn create_member_with_grants(
    conn: &mut AsyncPgConnection,
    email: &str,
    password_hash: &str,
    name: &str,
    org_id: Uuid,
    role_id: Uuid,
    scope_types: &[String],
    scope_ids: &[Uuid],
) -> QueryResult<Vec<NewMemberRow>> {
    let email = email.to_lowercase();
    diesel::sql_query(
        "WITH new_user AS ( \
             INSERT INTO users (email, password_hash, name, must_change_password) \
             VALUES ($1, $2, $3, true) \
             RETURNING id \
         ) \
         INSERT INTO role_grants (org_id, user_id, role_id, scope_type, scope_id) \
         SELECT $4, new_user.id, $5, s.scope_type, s.scope_id \
         FROM new_user, unnest($6::text[], $7::uuid[]) AS s(scope_type, scope_id) \
         RETURNING user_id, id AS grant_id",
    )
    .bind::<Text, _>(email)
    .bind::<Text, _>(password_hash)
    .bind::<Text, _>(name)
    .bind::<SqlUuid, _>(org_id)
    .bind::<SqlUuid, _>(role_id)
    .bind::<Array<Text>, _>(scope_types.to_vec())
    .bind::<Array<SqlUuid>, _>(scope_ids.to_vec())
    .get_results(conn)
    .await
}

pub async fn find_user_by_email(
    conn: &mut AsyncPgConnection,
    email: &str,
) -> QueryResult<Option<User>> {
    let email = email.to_lowercase();
    users::table
        .filter(users::email.eq(email))
        .select(User::as_select())
        .first(conn)
        .await
        .optional()
}

pub async fn find_user_by_id(conn: &mut AsyncPgConnection, id: Uuid) -> QueryResult<Option<User>> {
    users::table
        .find(id)
        .select(User::as_select())
        .first(conn)
        .await
        .optional()
}

pub async fn touch_last_login(conn: &mut AsyncPgConnection, id: Uuid) -> QueryResult<usize> {
    diesel::update(users::table.find(id))
        .set(users::last_login_at.eq(Utc::now()))
        .execute(conn)
        .await
}

pub async fn get_user(conn: &mut AsyncPgConnection, id: Uuid) -> QueryResult<Option<User>> {
    users::table
        .find(id)
        .select(User::as_select())
        .first(conn)
        .await
        .optional()
}

pub async fn set_user_active(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    active: bool,
) -> QueryResult<usize> {
    diesel::update(users::table.find(user_id))
        .set((
            users::is_active.eq(active),
            users::updated_at.eq(Utc::now()),
        ))
        .execute(conn)
        .await
}

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

/// Mint a refresh token, starting a new session or continuing an existing one.
///
/// One data-modifying CTE rather than a transaction: `conn.transaction`'s
/// diesel-async 0.9 signature needs async closures (Rust 1.85) and this
/// workspace's MSRV is 1.82 per `packaging/rpm/sauron.spec`. Postgres runs both
/// statements atomically within the one statement anyway.
///
/// Returns the number of refresh-token rows inserted: `1` on success, `0` when
/// the session exists but is revoked or belongs to somebody else.
///
/// Three things in the SQL are load-bearing.
///
/// **`WHERE auth_sessions.revoked_at IS NULL` is the only thing stopping the
/// rotation grace window from resurrecting a killed session.**
/// `REFRESH_RACE_GRACE` is 10 seconds and exists because two dashboard tabs
/// share one localStorage refresh token. Concretely: session B rotates at T (old
/// token -> `rotated`); the user revokes B at T+2s; B's other tab presents the
/// old token at T+3s. The reason *is* `rotated`, it *is* inside the grace, and
/// `user_has_active_refresh_token` is *true* because the spared session A is
/// live — so the race path runs. Postgres's `DO UPDATE ... WHERE` skips the
/// update, `RETURNING` yields nothing, the outer INSERT inserts nothing, and
/// this returns 0. A Rust-side pre-check would be a TOCTOU race and must not be
/// substituted; if a refactor moves the session upsert out of this CTE, the hole
/// silently reopens.
///
/// **The COALESCE order is inverted from the obvious.** `EXCLUDED` holds the
/// *current rotator's* values, so `COALESCE(EXCLUDED.x, auth_sessions.x)` would
/// overwrite the row every 15 minutes. The UI renders these beside `created_at`
/// as login-time facts, and the sole justification for returning an unmasked IP
/// is "was that login me?" — so if an attacker steals a refresh token and
/// rotates it, last-writer-wins would destroy the original login IP and user
/// agent and leave the row visually unchanged. If a "currently seen from" value
/// is ever wanted, it gets its own columns and its own label.
///
/// **`user_id = $2` in the WHERE is defensive**: it makes a mis-threaded
/// `session_id` fail rather than cross-link two users' tokens.
///
/// The outer INSERT deliberately omits `user_agent`. Writing the sanitized UA to
/// `refresh_tokens` on every rotation (~96 times a day per session) persists
/// ~120 bytes nothing reads — `list_auth_sessions` never touches that table and the
/// same string is already on the session row — on the workspace's
/// fastest-growing never-pruned table.
pub async fn start_or_continue_session(
    conn: &mut AsyncPgConnection,
    session_id: Uuid,
    user_id: Uuid,
    token_hash: String,
    expires_at: DateTime<Utc>,
    user_agent: Option<String>,
    ip: Option<String>,
) -> QueryResult<usize> {
    diesel::sql_query(
        "WITH s AS ( \
           INSERT INTO auth_sessions (id, user_id, expires_at, user_agent, ip) \
           VALUES ($1,$2,$3,$4,$5) \
           ON CONFLICT (id) DO UPDATE \
              SET last_used_at = now(), \
                  expires_at   = EXCLUDED.expires_at, \
                  user_agent   = COALESCE(auth_sessions.user_agent, EXCLUDED.user_agent), \
                  ip           = COALESCE(auth_sessions.ip, EXCLUDED.ip) \
            WHERE auth_sessions.revoked_at IS NULL AND auth_sessions.user_id = $2 \
           RETURNING id) \
         INSERT INTO refresh_tokens (user_id, token_hash, expires_at, session_id) \
         SELECT $2,$6,$3,s.id FROM s",
    )
    .bind::<SqlUuid, _>(session_id)
    .bind::<SqlUuid, _>(user_id)
    .bind::<Timestamptz, _>(expires_at)
    .bind::<Nullable<Text>, _>(user_agent)
    .bind::<Nullable<Text>, _>(ip)
    .bind::<Text, _>(token_hash)
    .execute(conn)
    .await
}

/// One id returned by a revoke CTE. The primary query must always be
/// `SELECT id FROM s` — see the note on [`revoke_session`].
#[derive(Debug, QueryableByName)]
pub struct RevokedSessionRow {
    #[diesel(sql_type = SqlUuid)]
    pub id: Uuid,
}

/// End one session the caller owns. Returns the ids actually revoked (0 or 1).
///
/// **The `auth_sessions` UPDATE must be the row-count-bearing statement.**
/// Postgres reports the command tag of the *outer* statement, so the obvious
/// shape — `WITH s AS (UPDATE auth_sessions ... RETURNING id) UPDATE
/// refresh_tokens ... FROM s`, read with `.execute()` — returns the number of
/// *token* rows touched. A live session that currently has no live refresh token
/// would then revoke successfully in the database while the handler answered
/// 404, and because the handler only updates its in-process snapshot on the
/// success branch, the killed session's access token would keep working for the
/// full 900s. That state is reachable and is *created* by this slice, because
/// deactivation used to kill tokens without touching sessions. So: session
/// UPDATE in one CTE arm, token UPDATE in a second, `SELECT id FROM s` as the
/// primary query. Data-modifying CTE arms execute exactly once and to completion
/// whether or not the primary query reads them, which is why the token arm can
/// be a bare `RETURNING r.id` nobody selects.
///
/// `user_id = $2` is the ownership check. It is why the handler needs no
/// separate SELECT, why there is no window between check and write, and why one
/// user can never revoke another's session by guessing a uuid. An empty result
/// means absent, already revoked, or someone else's — all mapped to 404, never
/// 403, so the response cannot be used to probe which session ids exist.
pub async fn revoke_session(
    conn: &mut AsyncPgConnection,
    session_id: Uuid,
    user_id: Uuid,
    reason: &str,
    actor: Option<Uuid>,
) -> QueryResult<Vec<Uuid>> {
    let rows: Vec<RevokedSessionRow> = diesel::sql_query(
        "WITH s AS ( \
           UPDATE auth_sessions SET revoked_at = now(), revoked_reason = $3, revoked_by = $4 \
            WHERE id = $1 AND user_id = $2 AND revoked_at IS NULL \
           RETURNING id), \
         t AS ( \
           UPDATE refresh_tokens r SET revoked_at = now(), revoked_reason = $3 \
             FROM s WHERE r.session_id = s.id AND r.revoked_at IS NULL \
           RETURNING r.id) \
         SELECT id FROM s",
    )
    .bind::<SqlUuid, _>(session_id)
    .bind::<SqlUuid, _>(user_id)
    .bind::<Text, _>(reason)
    .bind::<Nullable<SqlUuid>, _>(actor)
    .get_results(conn)
    .await?;
    Ok(rows.into_iter().map(|r| r.id).collect())
}

/// End every live session for a user, optionally sparing one. Returns the ids
/// revoked, which the caller must hand to the in-process revocation snapshot or
/// the kill is invisible until the next poll.
///
/// The token arm expresses the sparing rule **directly** rather than as
/// `session_id IN (SELECT id FROM s)`. The `IN` form silently skips live tokens
/// whose session was already revoked — reachable, because the refresh race path
/// issues a new token without revoking the presented one (so one session can
/// hold two live tokens) and `logout` revokes one hash while revoking the
/// session. Those leftovers are inert for minting, but they make
/// `user_has_active_refresh_token` return true after an account-wide kill,
/// weakening the invariant the grace window depends on. `IS DISTINCT FROM` also
/// gives the right NULL semantics: session-less legacy tokens are still killed
/// by "revoke others", which is correct — a user asking to kill their other
/// devices means those too.
pub async fn revoke_sessions_for_user(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    except: Option<Uuid>,
    reason: &str,
    actor: Option<Uuid>,
) -> QueryResult<Vec<Uuid>> {
    let rows: Vec<RevokedSessionRow> = diesel::sql_query(
        "WITH s AS ( \
           UPDATE auth_sessions SET revoked_at = now(), revoked_reason = $2, revoked_by = $3 \
            WHERE user_id = $1 AND revoked_at IS NULL \
              AND ($4::uuid IS NULL OR id <> $4) \
           RETURNING id), \
         t AS ( \
           UPDATE refresh_tokens r SET revoked_at = now(), revoked_reason = $2 \
            WHERE r.user_id = $1 AND r.revoked_at IS NULL \
              AND ($4::uuid IS NULL OR r.session_id IS DISTINCT FROM $4) \
           RETURNING r.id) \
         SELECT id FROM s",
    )
    .bind::<SqlUuid, _>(user_id)
    .bind::<Text, _>(reason)
    .bind::<Nullable<SqlUuid>, _>(actor)
    .bind::<Nullable<SqlUuid>, _>(except)
    .get_results(conn)
    .await?;
    Ok(rows.into_iter().map(|r| r.id).collect())
}

/// Revoke one refresh token by hash and take its session with it. Returns the
/// session id if one was ended.
///
/// Today's `logout` revokes the token and leaves the session live in the owner's
/// list forever — dead token, live row. The `AND revoked_at IS NULL` guard is a
/// small, deliberate behaviour change from `revoke_refresh_token`: without it,
/// logging out with an already-rotated token rewrites `revoked_reason` from
/// `rotated` to `logout`, and the 10-second grace window (which fires only on
/// `rotated`) stops firing for the other tab. This does not widen logout's
/// authorization surface — whoever holds the raw refresh token could already
/// revoke it.
pub async fn revoke_refresh_token_and_session(
    conn: &mut AsyncPgConnection,
    token_hash: &str,
    reason: &str,
) -> QueryResult<Option<Uuid>> {
    let rows: Vec<RevokedSessionRow> = diesel::sql_query(
        "WITH t AS ( \
           UPDATE refresh_tokens SET revoked_at = now(), revoked_reason = $2 \
            WHERE token_hash = $1 AND revoked_at IS NULL \
           RETURNING session_id), \
         s AS ( \
           UPDATE auth_sessions a SET revoked_at = now(), revoked_reason = $2 \
             FROM t WHERE a.id = t.session_id AND a.revoked_at IS NULL \
           RETURNING a.id) \
         SELECT id FROM s",
    )
    .bind::<Text, _>(token_hash)
    .bind::<Text, _>(reason)
    .get_results(conn)
    .await?;
    Ok(rows.into_iter().next().map(|r| r.id))
}

/// Ceiling on how many session rows one account may render. A real account has
/// single-digit live sessions; this exists so a pathological one cannot turn a
/// user-facing page into an unbounded scan.
pub const MAX_SESSIONS_LISTED: i64 = 200;

/// How long a revoked or long-expired session row is kept.
///
/// A compile-time constant, not an environment variable: three files of
/// documentation for a value nobody tunes is how a config surface becomes
/// unmaintainable.
pub const AUTH_SESSION_RETENTION_DAYS: i64 = 30;

/// The caller's own sessions, live first.
///
/// Named `list_auth_sessions` rather than `list_sessions`: this module already
/// owns an analytics `list_sessions` over the unrelated `sessions` table, and a
/// same-name collision does not compile.
///
/// **Never touches `refresh_tokens`.** That is the structural reason
/// `token_hash` cannot leak through the session endpoint: the column is not in
/// this query's source table, so no careless `select()` can reach it.
pub async fn list_auth_sessions(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    include_revoked: bool,
) -> QueryResult<Vec<AuthSession>> {
    let now = Utc::now();
    let mut rows = auth_sessions::table
        .filter(auth_sessions::user_id.eq(user_id))
        .filter(auth_sessions::revoked_at.is_null())
        .filter(auth_sessions::expires_at.gt(now))
        .select(AuthSession::as_select())
        .order(auth_sessions::last_used_at.desc())
        .limit(MAX_SESSIONS_LISTED)
        .load(conn)
        .await?;

    if include_revoked {
        // Served by `auth_sessions_revoked_idx`. The reaper is what keeps that
        // partial index small enough that this needs no `user_id` support: the
        // whole index is the last 30 days of revocations.
        let cutoff = now - chrono::Duration::days(AUTH_SESSION_RETENTION_DAYS);
        let revoked = auth_sessions::table
            .filter(auth_sessions::user_id.eq(user_id))
            .filter(auth_sessions::revoked_at.ge(cutoff))
            .select(AuthSession::as_select())
            .order(auth_sessions::revoked_at.desc())
            .limit(MAX_SESSIONS_LISTED)
            .load(conn)
            .await?;
        rows.extend(revoked);
    }
    Ok(rows)
}

/// Session ids revoked within the last `window_secs`, for the per-replica
/// revocation snapshot.
///
/// The cutoff is computed **in the database**. Computing `Utc::now() - window`
/// in Rust would make the control depend on API-vs-Postgres clock skew:
/// `revoked_at` is written by Postgres `now()`, so an API host running ahead by
/// more than the slack would drop recently-revoked sessions from its snapshot
/// and silently re-enable their access tokens — invisibly, because the poll
/// succeeded.
///
/// The `LIMIT` is not decoration: without it a plausible
/// `JWT_ACCESS_TTL_SECS=604800` plus any bulk event (an offboarding script, a
/// deactivation sweep) has every replica materialising an enormous set 17 280
/// times a day and swapping it into a lock every authenticated request reads.
/// The caller logs at ERROR when the limit is hit — a silently truncated
/// snapshot is a security control that has stopped working while reporting
/// healthy.
pub async fn revoked_session_ids(
    conn: &mut AsyncPgConnection,
    window_secs: i64,
) -> QueryResult<Vec<Uuid>> {
    let rows: Vec<RevokedSessionRow> = diesel::sql_query(
        "SELECT id FROM auth_sessions \
          WHERE revoked_at >= now() - ($1 || ' seconds')::interval \
          LIMIT 50000",
    )
    .bind::<Text, _>(window_secs.to_string())
    .get_results(conn)
    .await?;
    Ok(rows.into_iter().map(|r| r.id).collect())
}

/// Delete `auth_sessions` rows older than `days`.
///
/// This table is a permanent per-user record of where and on what device someone
/// signed in — a new PII class; `refresh_tokens` never had an `ip` column at
/// all. Nothing writes `revoked_at` when a session merely *expires*, so
/// `WHERE revoked_at IS NULL` excludes only explicitly-revoked sessions and
/// every abandoned session would otherwise stay indexed forever.
///
/// Deleting a row sets its tokens' `session_id` to NULL rather than deleting
/// them — that is exactly why the FK is `ON DELETE SET NULL`, and it is what
/// keeps replay detection intact through a reap.
pub async fn prune_auth_sessions(conn: &mut AsyncPgConnection, days: i64) -> QueryResult<usize> {
    diesel::sql_query(
        "DELETE FROM auth_sessions \
          WHERE (revoked_at IS NOT NULL AND revoked_at < now() - ($1 || ' days')::interval) \
             OR expires_at < now() - ($1 || ' days')::interval",
    )
    .bind::<Text, _>(days.to_string())
    .execute(conn)
    .await
}

pub async fn find_active_refresh_token(
    conn: &mut AsyncPgConnection,
    token_hash: &str,
) -> QueryResult<Option<RefreshToken>> {
    refresh_tokens::table
        .filter(refresh_tokens::token_hash.eq(token_hash))
        .filter(refresh_tokens::revoked_at.is_null())
        .filter(refresh_tokens::expires_at.gt(Utc::now()))
        .select(RefreshToken::as_select())
        .first(conn)
        .await
        .optional()
}

/// Revoked because it was exchanged for a successor — the normal refresh path.
/// Only this reason is eligible for the concurrent-refresh grace window.
pub const REVOKE_ROTATED: &str = "rotated";
/// Revoked by an explicit logout.
pub const REVOKE_LOGOUT: &str = "logout";
/// Revoked as part of a token-family kill after replay was detected.
pub const REVOKE_REUSE: &str = "reuse";
/// Refresh tokens killed because an admin deactivated the account. Distinct
/// from `REVOKE_REUSE` so the rotation grace window (which exists to survive
/// two dashboard tabs racing) can never resurrect a deactivated session.
pub const REVOKE_DEACTIVATED: &str = "deactivated";
/// Refresh tokens rotated out because the user changed their own password.
pub const REVOKE_PASSWORD_CHANGED: &str = "password_changed";
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
/// The owner ended one specific session from their own account page.
pub const REVOKE_USER_REVOKED: &str = "user_revoked";
/// The owner pressed "sign out other devices" — every session but the caller's.
pub const REVOKE_USER_REVOKED_OTHERS: &str = "user_revoked_others";
/// An administrator with `member:credential` signed a member out of everything.
pub const REVOKE_ADMIN: &str = "admin_revoked";

/// Reasons that mean a human deliberately ended the session. Presenting a token
/// revoked for one of these is NOT evidence of theft and must not trip the
/// family kill in `refresh`.
///
/// Without this, a device killed by "sign out other devices" presents its dead
/// token on its existing 15-minute timer, lands in the reuse branch, and kills
/// the user's WHOLE family — including the session they explicitly chose to
/// keep. The symptom reads as "sign out other devices logs me out too, about
/// fifteen minutes later", which looks like a flaky bug rather than a design
/// fault.
///
/// This is "deliberate, not theft" — not "every reason". `REVOKE_ROTATED` and
/// `REVOKE_REUSE` must never appear here: rotation would take the early-return
/// path on every ordinary refresh and break the 10-second multi-tab grace
/// window, and reuse is the theft signal itself. `REVOKE_LOGOUT` and
/// `REVOKE_PASSWORD_CHANGED` stay out too — a token surfacing after either is
/// exactly the replay the alarm exists for.
///
/// Adding a reason that `auth_sessions_revoked_reason_check` does not already
/// list ALSO needs a widening migration, or the revoke path 500s.
pub const DELIBERATE_REVOKE_REASONS: [&str; 5] = [
    REVOKE_USER_REVOKED,
    REVOKE_USER_REVOKED_OTHERS,
    REVOKE_ADMIN,
    REVOKE_PASSWORD_RESET,
    REVOKE_RESET_FORCED,
];

pub async fn revoke_refresh_token(
    conn: &mut AsyncPgConnection,
    token_hash: &str,
    reason: &str,
) -> QueryResult<usize> {
    diesel::update(refresh_tokens::table.filter(refresh_tokens::token_hash.eq(token_hash)))
        .set((
            refresh_tokens::revoked_at.eq(Utc::now()),
            refresh_tokens::revoked_reason.eq(reason),
        ))
        .execute(conn)
        .await
}

/// Revocation metadata for a token hash, whatever its state.
///
/// Returns `(user_id, session_id, revoked_at, revoked_reason)`. The handler
/// needs all four: the first three to tell a benign concurrent refresh from a
/// genuine replay, and `session_id` because the race path holds only the hash
/// and must continue the *same* session rather than starting a new one.
pub async fn refresh_token_revocation(
    conn: &mut AsyncPgConnection,
    token_hash: &str,
) -> QueryResult<Option<(Uuid, Option<Uuid>, Option<DateTime<Utc>>, Option<String>)>> {
    refresh_tokens::table
        .filter(refresh_tokens::token_hash.eq(token_hash))
        .select((
            refresh_tokens::user_id,
            refresh_tokens::session_id,
            refresh_tokens::revoked_at,
            refresh_tokens::revoked_reason,
        ))
        .first(conn)
        .await
        .optional()
}

/// Whether the user still holds any usable refresh token.
///
/// After a family kill there are none, which is what stops the grace window from
/// resurrecting a session that was just revoked for replay.
///
/// That rationale stops holding the moment one session can be revoked while
/// others live: with a second session alive this returns true even though the
/// session being resurrected is dead. The real guard is now
/// `WHERE auth_sessions.revoked_at IS NULL` inside
/// [`start_or_continue_session`]; this check is a cheap first filter, not the
/// thing standing between a revoked session and a fresh token.
pub async fn user_has_active_refresh_token(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
) -> QueryResult<bool> {
    use diesel::dsl::exists;
    diesel::select(exists(
        refresh_tokens::table
            .filter(refresh_tokens::user_id.eq(user_id))
            .filter(refresh_tokens::revoked_at.is_null())
            .filter(refresh_tokens::expires_at.gt(Utc::now())),
    ))
    .get_result(conn)
    .await
}

/// The owner of a refresh-token hash **regardless of revocation/expiry**.
///
/// Used to detect replay of an already-rotated token: `find_active_refresh_token`
/// cannot distinguish "never existed" from "already used", but that difference
/// is the whole theft signal in a rotating-refresh scheme.
pub async fn refresh_token_owner(
    conn: &mut AsyncPgConnection,
    token_hash: &str,
) -> QueryResult<Option<Uuid>> {
    refresh_tokens::table
        .filter(refresh_tokens::token_hash.eq(token_hash))
        .select(refresh_tokens::user_id)
        .first(conn)
        .await
        .optional()
}

/// Revoke every still-active refresh token for a user (token-family kill).
pub async fn revoke_all_refresh_tokens_for_user(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
) -> QueryResult<usize> {
    diesel::update(
        refresh_tokens::table
            .filter(refresh_tokens::user_id.eq(user_id))
            .filter(refresh_tokens::revoked_at.is_null()),
    )
    .set((
        refresh_tokens::revoked_at.eq(Utc::now()),
        refresh_tokens::revoked_reason.eq(REVOKE_REUSE),
    ))
    .execute(conn)
    .await
}

pub async fn revoke_all_refresh_tokens_for_user_with_reason(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    reason: &str,
) -> QueryResult<usize> {
    diesel::update(
        refresh_tokens::table
            .filter(refresh_tokens::user_id.eq(user_id))
            .filter(refresh_tokens::revoked_at.is_null()),
    )
    .set((
        refresh_tokens::revoked_at.eq(Utc::now()),
        refresh_tokens::revoked_reason.eq(reason),
    ))
    .execute(conn)
    .await
}

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

/// Does this address already hold a self-service reset link that still works?
///
/// Keyed on the EMAIL, not on a user id, so it is one statement against one
/// index whether or not the account exists. `forgot_password` is built so that
/// both branches do identical work — an existence check that ran only on the
/// found branch would hand back the enumeration oracle that route's preflight
/// exists to close.
///
/// Scoped to the self-service mode on purpose. An admin-initiated link lives for
/// 24h, and treating one as "you already have a link" would mute a whole day of
/// self-service mail for someone whose admin link may have gone astray.
///
/// The mode is `"self"`, matching the table's CHECK constraint —
/// `ResetMode::SelfService.as_str()`, NOT the variant's name. Spelling it
/// `"self_service"` compiles, matches nothing, and silently turns the send cap
/// in `forgot_password` into a no-op that mails on every request.
pub async fn has_live_self_service_reset_token(
    conn: &mut AsyncPgConnection,
    email: &str,
) -> QueryResult<bool> {
    let email = email.to_lowercase();
    password_reset_tokens::table
        .inner_join(users::table)
        .filter(users::email.eq(email))
        .filter(password_reset_tokens::mode.eq("self"))
        .filter(password_reset_tokens::consumed_at.is_null())
        .filter(password_reset_tokens::invalidated_at.is_null())
        .filter(password_reset_tokens::expires_at.gt(Utc::now()))
        .select(password_reset_tokens::id)
        .first::<Uuid>(conn)
        .await
        .optional()
        .map(|row| row.is_some())
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

// ===========================================================================
// Organizations
// ===========================================================================

pub async fn create_org(
    conn: &mut AsyncPgConnection,
    name: &str,
    slug: &str,
) -> QueryResult<Organization> {
    diesel::insert_into(organizations::table)
        .values(NewOrganization { name, slug })
        .returning(Organization::as_returning())
        .get_result(conn)
        .await
}

pub async fn get_org(conn: &mut AsyncPgConnection, id: Uuid) -> QueryResult<Option<Organization>> {
    organizations::table
        .find(id)
        .select(Organization::as_select())
        .first(conn)
        .await
        .optional()
}

/// Orgs the user has any grant in.
pub async fn list_orgs_for_user(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
) -> QueryResult<Vec<Organization>> {
    let org_ids: Vec<Uuid> = role_grants::table
        .filter(role_grants::user_id.eq(user_id))
        .select(role_grants::org_id)
        .distinct()
        .load(conn)
        .await?;
    organizations::table
        .filter(organizations::id.eq_any(org_ids))
        .select(Organization::as_select())
        .order(organizations::created_at.asc())
        .load(conn)
        .await
}

// ===========================================================================
// RBAC: roles & grants
// ===========================================================================

/// System presets + this org's custom roles.
pub async fn list_roles(conn: &mut AsyncPgConnection, org_id: Uuid) -> QueryResult<Vec<Role>> {
    roles::table
        .filter(roles::org_id.is_null().or(roles::org_id.eq(org_id)))
        .select(Role::as_select())
        .order((roles::is_system.desc(), roles::name.asc()))
        .load(conn)
        .await
}

pub async fn get_role(conn: &mut AsyncPgConnection, id: Uuid) -> QueryResult<Option<Role>> {
    roles::table
        .find(id)
        .select(Role::as_select())
        .first(conn)
        .await
        .optional()
}

pub async fn get_system_role(
    conn: &mut AsyncPgConnection,
    name: &str,
) -> QueryResult<Option<Role>> {
    roles::table
        .filter(roles::org_id.is_null())
        .filter(roles::name.eq(name))
        .select(Role::as_select())
        .first(conn)
        .await
        .optional()
}

pub async fn create_role(
    conn: &mut AsyncPgConnection,
    org_id: Uuid,
    name: &str,
    description: &str,
    permissions: Value,
) -> QueryResult<Role> {
    diesel::insert_into(roles::table)
        .values(NewRole {
            org_id: Some(org_id),
            name,
            description,
            is_system: false,
            permissions,
        })
        .returning(Role::as_returning())
        .get_result(conn)
        .await
}

/// Update a custom role. Scoped by `org_id` as well as `role_id` so a mistaken
/// call cannot reach across orgs, and filtered on `is_system` so a preset can
/// never be written even if a caller-side check is missed.
pub async fn update_role(
    conn: &mut AsyncPgConnection,
    org_id: Uuid,
    role_id: Uuid,
    name: &str,
    description: &str,
    permissions: Value,
) -> QueryResult<Role> {
    diesel::update(
        roles::table
            .filter(roles::id.eq(role_id))
            .filter(roles::org_id.eq(org_id))
            .filter(roles::is_system.eq(false)),
    )
    .set((
        roles::name.eq(name),
        roles::description.eq(description),
        roles::permissions.eq(permissions),
    ))
    .returning(Role::as_returning())
    .get_result(conn)
    .await
}

/// Delete a custom role. Scoped by `org_id` as well as `role_id` so a mistaken
/// call cannot reach across orgs, and filtered on `is_system` so a preset can
/// never be deleted even if a caller-side check is missed — the same defence in
/// depth `update_role` uses.
///
/// `role_grants.role_id` is ON DELETE CASCADE, so every grant holding this role
/// disappears with it. Callers must have already counted and confirmed that.
pub async fn delete_role(
    conn: &mut AsyncPgConnection,
    org_id: Uuid,
    role_id: Uuid,
) -> QueryResult<usize> {
    diesel::delete(
        roles::table
            .filter(roles::id.eq(role_id))
            .filter(roles::org_id.eq(org_id))
            .filter(roles::is_system.eq(false)),
    )
    .execute(conn)
    .await
}

/// How many grants currently hold `role_id`. Used to report what a delete
/// cascaded, since `role_grants.role_id` is ON DELETE CASCADE and the rows are
/// gone by the time the delete returns.
pub async fn count_grants_for_role(
    conn: &mut AsyncPgConnection,
    role_id: Uuid,
) -> QueryResult<i64> {
    let row: GrantCountRow =
        diesel::sql_query("SELECT count(*)::bigint AS n FROM role_grants WHERE role_id = $1")
            .bind::<SqlUuid, _>(role_id)
            .get_result(conn)
            .await?;
    Ok(row.n)
}

/// Idempotently upsert a system preset role (keeps DB in sync with code).
pub async fn upsert_preset_role(
    conn: &mut AsyncPgConnection,
    name: &str,
    description: &str,
    permissions: &Value,
) -> QueryResult<usize> {
    diesel::sql_query(
        "INSERT INTO roles (org_id, name, description, is_system, permissions) \
         VALUES (NULL, $1, $2, true, $3) \
         ON CONFLICT (name) WHERE org_id IS NULL \
         DO UPDATE SET permissions = EXCLUDED.permissions, description = EXCLUDED.description",
    )
    .bind::<Text, _>(name)
    .bind::<Text, _>(description)
    .bind::<diesel::sql_types::Jsonb, _>(permissions.clone())
    .execute(conn)
    .await
}

pub async fn create_grant(
    conn: &mut AsyncPgConnection,
    grant: NewRoleGrant,
) -> QueryResult<RoleGrant> {
    diesel::insert_into(role_grants::table)
        .values(&grant)
        .on_conflict((
            role_grants::user_id,
            role_grants::role_id,
            role_grants::scope_type,
            role_grants::scope_id,
        ))
        .do_update()
        .set(role_grants::org_id.eq(excluded(role_grants::org_id)))
        .returning(RoleGrant::as_returning())
        .get_result(conn)
        .await
}

/// Upsert a batch of grants in one statement, same idempotent semantics as
/// `create_grant`: re-granting an existing `(user, role, scope)` just re-points
/// its `org_id`. Because that is a DO UPDATE rather than DO NOTHING, every row
/// comes back, so the caller can rely on `ids.len() == rows.len()`.
pub async fn create_grants(
    conn: &mut AsyncPgConnection,
    rows: Vec<NewRoleGrant>,
) -> QueryResult<Vec<Uuid>> {
    // An empty VALUES list is not valid SQL; nothing to insert either way.
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    diesel::insert_into(role_grants::table)
        .values(&rows)
        .on_conflict((
            role_grants::user_id,
            role_grants::role_id,
            role_grants::scope_type,
            role_grants::scope_id,
        ))
        .do_update()
        .set(role_grants::org_id.eq(excluded(role_grants::org_id)))
        .returning(role_grants::id)
        .get_results(conn)
        .await
}

pub async fn delete_grant(
    conn: &mut AsyncPgConnection,
    org_id: Uuid,
    grant_id: Uuid,
) -> QueryResult<usize> {
    diesel::delete(
        role_grants::table
            .filter(role_grants::id.eq(grant_id))
            .filter(role_grants::org_id.eq(org_id)),
    )
    .execute(conn)
    .await
}

pub async fn update_grant(
    conn: &mut AsyncPgConnection,
    grant_id: Uuid,
    role_id: Uuid,
    scope_type: &str,
    scope_id: Uuid,
) -> QueryResult<RoleGrant> {
    diesel::update(role_grants::table.find(grant_id))
        .set((
            role_grants::role_id.eq(role_id),
            role_grants::scope_type.eq(scope_type),
            role_grants::scope_id.eq(scope_id),
        ))
        .returning(RoleGrant::as_returning())
        .get_result(conn)
        .await
}

/// The org a grant belongs to (for authorizing its deletion).
pub async fn grant_org(conn: &mut AsyncPgConnection, grant_id: Uuid) -> QueryResult<Option<Uuid>> {
    role_grants::table
        .find(grant_id)
        .select(role_grants::org_id)
        .first(conn)
        .await
        .optional()
}

/// The full grant row, so the caller can evaluate its role and scope before
/// allowing a deletion.
pub async fn get_grant(
    conn: &mut AsyncPgConnection,
    grant_id: Uuid,
) -> QueryResult<Option<RoleGrant>> {
    role_grants::table
        .find(grant_id)
        .select(RoleGrant::as_select())
        .first(conn)
        .await
        .optional()
}

/// How many grants in `org_id` — other than `exclude_id` — confer `org:manage`.
///
/// Guards against deleting the last administrator: with no `org:manage` left,
/// the anti-escalation rule in `create_grant` makes it impossible for anyone to
/// grant it again.
pub async fn count_org_manage_grants_excluding(
    conn: &mut AsyncPgConnection,
    org_id: Uuid,
    exclude_id: Uuid,
) -> QueryResult<i64> {
    let row: GrantCountRow = diesel::sql_query(
        "SELECT count(*)::bigint AS n \
         FROM role_grants g JOIN roles r ON g.role_id = r.id \
         WHERE g.org_id = $1 AND g.id <> $2 AND g.scope_type = 'org' \
           AND r.permissions @> to_jsonb('org:manage'::text)",
    )
    .bind::<SqlUuid, _>(org_id)
    .bind::<SqlUuid, _>(exclude_id)
    .get_result(conn)
    .await?;
    Ok(row.n)
}

#[derive(Debug, QueryableByName)]
pub struct GrantCountRow {
    #[diesel(sql_type = BigInt)]
    pub n: i64,
}

/// How many grants would still confer `org:manage` in this org if `role_id`
/// stopped conferring it.
///
/// Editing a role affects every grant that holds it at once, unlike deleting
/// one grant or deactivating one user, so the exclusion here is by role
/// rather than by grant id or user id.
pub async fn count_org_manage_grants_excluding_role(
    conn: &mut AsyncPgConnection,
    org_id: Uuid,
    role_id: Uuid,
) -> QueryResult<i64> {
    let row: GrantCountRow = diesel::sql_query(
        "SELECT count(*)::bigint AS n \
         FROM role_grants g JOIN roles r ON g.role_id = r.id \
         WHERE g.org_id = $1 AND g.role_id <> $2 AND g.scope_type = 'org' \
           AND r.permissions @> to_jsonb('org:manage'::text)",
    )
    .bind::<SqlUuid, _>(org_id)
    .bind::<SqlUuid, _>(role_id)
    .get_result(conn)
    .await?;
    Ok(row.n)
}

/// How many grants this user holds in orgs *other* than `org_id`.
///
/// Deactivation is account-global, but `member:manage` is org-scoped. If the
/// target belongs to another org too, this org's admin has no authority to
/// disable their login there, so a non-zero count blocks the operation.
pub async fn count_user_grants_outside_org(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    org_id: Uuid,
) -> QueryResult<i64> {
    let row: GrantCountRow = diesel::sql_query(
        "SELECT count(*)::bigint AS n FROM role_grants \
         WHERE user_id = $1 AND org_id <> $2",
    )
    .bind::<SqlUuid, _>(user_id)
    .bind::<SqlUuid, _>(org_id)
    .get_result(conn)
    .await?;
    Ok(row.n)
}

/// How many grants conferring `org:manage` this org would still have if every
/// grant belonging to `user_id` were ignored.
///
/// `count_org_manage_grants_excluding` excludes a single grant, which is right
/// for deleting one. Deactivation disables a whole person, who may hold several
/// org:manage grants at once, so the exclusion has to be by user.
///
/// Unlike its two siblings this one joins `users.is_active`: it guards a
/// *deactivation*, and a holder who is already deactivated cannot administer
/// anything, so counting them would let an admin walk the org's owners down one
/// at a time — each deactivation kept legal by the ones already performed. The
/// other three clauses stay identical to the siblings on purpose; they must all
/// agree on what "a grant conferring org:manage" is.
pub async fn count_org_manage_grants_for_user_excluding_user(
    conn: &mut AsyncPgConnection,
    org_id: Uuid,
    user_id: Uuid,
) -> QueryResult<i64> {
    let row: GrantCountRow = diesel::sql_query(
        "SELECT count(*)::bigint AS n \
         FROM role_grants g JOIN roles r ON g.role_id = r.id \
         JOIN users u ON u.id = g.user_id AND u.is_active \
         WHERE g.org_id = $1 AND g.user_id <> $2 AND g.scope_type = 'org' \
           AND r.permissions @> to_jsonb('org:manage'::text)",
    )
    .bind::<SqlUuid, _>(org_id)
    .bind::<SqlUuid, _>(user_id)
    .get_result(conn)
    .await?;
    Ok(row.n)
}

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
) -> QueryResult<
    Vec<(
        RoleGrant,
        String,
        String,
        String,
        bool,
        Option<DateTime<Utc>>,
    )>,
> {
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

/// `(scope_type, scope_id, permissions)` for every grant the user holds in the
/// org — the raw material for permission resolution.
pub async fn user_grants_in_org(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    org_id: Uuid,
) -> QueryResult<Vec<(String, Uuid, Value)>> {
    role_grants::table
        .inner_join(roles::table.on(roles::id.eq(role_grants::role_id)))
        .filter(role_grants::user_id.eq(user_id))
        .filter(role_grants::org_id.eq(org_id))
        .select((
            role_grants::scope_type,
            role_grants::scope_id,
            roles::permissions,
        ))
        .load(conn)
        .await
}

// ===========================================================================
// Projects (grouping)
// ===========================================================================

pub async fn create_project(
    conn: &mut AsyncPgConnection,
    org_id: Uuid,
    name: &str,
    slug: &str,
) -> QueryResult<Project> {
    diesel::insert_into(projects::table)
        .values(NewProject { org_id, name, slug })
        .returning(Project::as_returning())
        .get_result(conn)
        .await
}

pub async fn list_projects_for_org(
    conn: &mut AsyncPgConnection,
    org_id: Uuid,
) -> QueryResult<Vec<Project>> {
    projects::table
        .filter(projects::org_id.eq(org_id))
        .select(Project::as_select())
        .order(projects::created_at.asc())
        .load(conn)
        .await
}

pub async fn get_project(conn: &mut AsyncPgConnection, id: Uuid) -> QueryResult<Option<Project>> {
    projects::table
        .find(id)
        .select(Project::as_select())
        .first(conn)
        .await
        .optional()
}

pub async fn rename_project(
    conn: &mut AsyncPgConnection,
    id: Uuid,
    name: &str,
) -> QueryResult<Option<Project>> {
    diesel::update(projects::table.find(id))
        .set((projects::name.eq(name), projects::updated_at.eq(Utc::now())))
        .returning(Project::as_returning())
        .get_result(conn)
        .await
        .optional()
}

/// Delete a project, taking every inspector policy under it with it.
///
/// Three levels, because a project owns apps and those apps own enrollments,
/// and a policy may target any of the three. See [`delete_app`] for why the
/// polymorphic `target_id` gets no cascade and why this is one CTE.
pub async fn delete_project(conn: &mut AsyncPgConnection, id: Uuid) -> QueryResult<usize> {
    diesel::sql_query(
        "WITH orphaned_policies AS ( \
             DELETE FROM inspector_policies \
              WHERE (target_type = 'project' AND target_id = $1) \
                 OR (target_type = 'app' \
                     AND target_id IN (SELECT id FROM apps WHERE project_id = $1)) \
                 OR (target_type = 'app_env' \
                     AND target_id IN ( \
                           SELECT ae.id FROM app_environments ae \
                             JOIN apps a ON a.id = ae.app_id \
                            WHERE a.project_id = $1)) \
         ) \
         DELETE FROM projects WHERE id = $1",
    )
    .bind::<SqlUuid, _>(id)
    .execute(conn)
    .await
}

/// The org that owns a project.
pub async fn project_org(
    conn: &mut AsyncPgConnection,
    project_id: Uuid,
) -> QueryResult<Option<Uuid>> {
    projects::table
        .find(project_id)
        .select(projects::org_id)
        .first(conn)
        .await
        .optional()
}

/// The projects among `ids` that belong to `org_id` — the discovery-query
/// counterpart to `list_projects_for_org`: a caller whose reach is a handful of
/// scoped grants (rather than the whole org) only gets those projects back,
/// not every project in the org.
pub async fn list_projects_by_ids_in_org(
    conn: &mut AsyncPgConnection,
    org_id: Uuid,
    ids: &[Uuid],
) -> QueryResult<Vec<Project>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    projects::table
        .filter(projects::org_id.eq(org_id))
        .filter(projects::id.eq_any(ids.to_vec()))
        .select(Project::as_select())
        .order(projects::created_at.asc())
        .load(conn)
        .await
}

/// Which of `ids` are projects in `org_id`. Used to validate a batch of
/// scopes without one round trip per scope.
pub async fn projects_in_org(
    conn: &mut AsyncPgConnection,
    org_id: Uuid,
    ids: &[Uuid],
) -> QueryResult<Vec<Uuid>> {
    projects::table
        .filter(projects::org_id.eq(org_id))
        .filter(projects::id.eq_any(ids.to_vec()))
        .select(projects::id)
        .load(conn)
        .await
}

// ===========================================================================
// Apps (ingest unit)
// ===========================================================================

pub async fn create_app(
    conn: &mut AsyncPgConnection,
    project_id: Uuid,
    name: &str,
    slug: &str,
    app_type: &str,
) -> QueryResult<App> {
    diesel::insert_into(apps::table)
        .values(NewApp {
            project_id,
            name,
            slug,
            app_type,
        })
        .returning(App::as_returning())
        .get_result(conn)
        .await
}

pub async fn list_apps_for_project(
    conn: &mut AsyncPgConnection,
    project_id: Uuid,
) -> QueryResult<Vec<App>> {
    apps::table
        .filter(apps::project_id.eq(project_id))
        .select(App::as_select())
        .order(apps::created_at.asc())
        .load(conn)
        .await
}

pub async fn get_app(conn: &mut AsyncPgConnection, id: Uuid) -> QueryResult<Option<App>> {
    apps::table
        .find(id)
        .select(App::as_select())
        .first(conn)
        .await
        .optional()
}

pub async fn update_app(
    conn: &mut AsyncPgConnection,
    id: Uuid,
    name: &str,
    ingest_enabled: bool,
) -> QueryResult<Option<App>> {
    diesel::update(apps::table.find(id))
        .set((
            apps::name.eq(name),
            apps::ingest_enabled.eq(ingest_enabled),
            apps::updated_at.eq(Utc::now()),
        ))
        .returning(App::as_returning())
        .get_result(conn)
        .await
        .optional()
}

/// Delete an app, taking its inspector policies with it.
///
/// `inspector_policies.target_id` is polymorphic (it matches `role_grants`), so
/// it carries no foreign key and no `ON DELETE CASCADE`. Everything else the
/// app owns — events, `inspector_findings`, `inspector_masked_keys` — really
/// does cascade, which made the survivors easy to miss: the policy row stayed,
/// `GET /v1/orgs/{org}/inspector/policies` kept LISTING it, and
/// `DELETE /v1/inspector/policies/{id}` answered 404 forever because that
/// handler authorizes through `authorize_app` against an app that no longer
/// exists. Visible, and unreachable.
///
/// One CTE rather than two statements: `conn.transaction(...)` is blocked by
/// the MSRV, and a bare pair could delete the app and then fail, leaving
/// exactly the orphan this exists to prevent. Every arm of a CTE also reads the
/// SAME snapshot, so the `app_environments` sub-select still sees the
/// enrollments even though the app delete cascades them away in the same
/// statement — which is why the `app_env` arm does not need to run first.
pub async fn delete_app(conn: &mut AsyncPgConnection, id: Uuid) -> QueryResult<usize> {
    diesel::sql_query(
        "WITH orphaned_policies AS ( \
             DELETE FROM inspector_policies \
              WHERE (target_type = 'app' AND target_id = $1) \
                 OR (target_type = 'app_env' \
                     AND target_id IN (SELECT id FROM app_environments WHERE app_id = $1)) \
         ) \
         DELETE FROM apps WHERE id = $1",
    )
    .bind::<SqlUuid, _>(id)
    .execute(conn)
    .await
}

/// `(project_id, org_id)` ancestry of an app — for permission resolution.
pub async fn app_ancestry(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
) -> QueryResult<Option<(Uuid, Uuid)>> {
    apps::table
        .inner_join(projects::table.on(projects::id.eq(apps::project_id)))
        .filter(apps::id.eq(app_id))
        .select((apps::project_id, projects::org_id))
        .first(conn)
        .await
        .optional()
}

/// `(app_id, project_id, org_id)` for each of `ids` that resolves — the
/// batched `app_ancestry`, so validating a batch of scopes costs one query.
/// Ids that are not apps are simply absent from the result.
pub async fn app_ancestries(
    conn: &mut AsyncPgConnection,
    ids: &[Uuid],
) -> QueryResult<Vec<(Uuid, Uuid, Uuid)>> {
    apps::table
        .inner_join(projects::table.on(projects::id.eq(apps::project_id)))
        .filter(apps::id.eq_any(ids.to_vec()))
        .select((apps::id, apps::project_id, projects::org_id))
        .load(conn)
        .await
}

/// `(env_id, app_id, project_id, org_id)` for each of `ids` that resolves —
/// the batched `env_ancestry`, mirroring `app_ancestries` exactly so
/// validating a batch of scopes that mixes apps and envs still costs one
/// query per kind. Ids that are not environments are simply absent from the
/// result.
pub async fn env_ancestries(
    conn: &mut AsyncPgConnection,
    ids: &[Uuid],
) -> QueryResult<Vec<(Uuid, Uuid, Uuid, Uuid)>> {
    app_environments::table
        .inner_join(apps::table.on(apps::id.eq(app_environments::app_id)))
        .inner_join(projects::table.on(projects::id.eq(apps::project_id)))
        .filter(app_environments::id.eq_any(ids.to_vec()))
        .select((
            app_environments::id,
            app_environments::app_id,
            apps::project_id,
            projects::org_id,
        ))
        .load(conn)
        .await
}

/// The app an `app_environments` ENROLLMENT belongs to.
pub async fn app_id_for_enrollment(
    conn: &mut AsyncPgConnection,
    enrollment_id: Uuid,
) -> QueryResult<Option<Uuid>> {
    app_environments::table
        .find(enrollment_id)
        .select(app_environments::app_id)
        .first(conn)
        .await
        .optional()
}

// --- environments: the project-level catalogue -------------------------------

/// Cap on how many live environments a project may hold. Creation is an
/// authenticated admin action rather than a side effect of ingest, so this is a
/// sanity bound rather than an abuse control.
///
/// The cap moved from per-app to per-project along with the environments
/// themselves. It also now bounds a *fan-out*: creating one environment enrolls
/// every app in the project, so the real ceiling on rows created is this times
/// the app count.
pub const MAX_ENVIRONMENTS_PER_PROJECT: i64 = 500;

pub async fn create_project_environment(
    conn: &mut AsyncPgConnection,
    project_id: Uuid,
    name: &str,
) -> QueryResult<Environment> {
    diesel::insert_into(environments::table)
        .values(NewEnvironment { project_id, name })
        .returning(Environment::as_returning())
        .get_result(conn)
        .await
}

pub async fn list_project_environments(
    conn: &mut AsyncPgConnection,
    project_id: Uuid,
    include_retired: bool,
) -> QueryResult<Vec<Environment>> {
    let mut q = environments::table
        .filter(environments::project_id.eq(project_id))
        .into_boxed();
    if !include_retired {
        q = q.filter(environments::retired_at.is_null());
    }
    q.select(Environment::as_select())
        .order(environments::name.asc())
        .limit(MAX_ENVIRONMENTS_PER_PROJECT)
        .load(conn)
        .await
}

pub async fn get_project_environment(
    conn: &mut AsyncPgConnection,
    id: Uuid,
) -> QueryResult<Option<Environment>> {
    environments::table
        .find(id)
        .select(Environment::as_select())
        .first(conn)
        .await
        .optional()
}

/// `(project_id, org_id)` ancestry of a catalogue environment, for authorizing
/// the project-level CRUD routes.
pub async fn project_env_ancestry(
    conn: &mut AsyncPgConnection,
    env_id: Uuid,
) -> QueryResult<Option<(Uuid, Uuid)>> {
    environments::table
        .inner_join(projects::table.on(projects::id.eq(environments::project_id)))
        .filter(environments::id.eq(env_id))
        .select((environments::project_id, projects::org_id))
        .first(conn)
        .await
        .optional()
}

/// Live catalogue entries only — the cap must not be consumed by retired rows.
pub async fn count_active_project_environments(
    conn: &mut AsyncPgConnection,
    project_id: Uuid,
) -> QueryResult<i64> {
    environments::table
        .filter(environments::project_id.eq(project_id))
        .filter(environments::retired_at.is_null())
        .count()
        .get_result(conn)
        .await
}

/// Take a project-level lock. Every mutation that reads the project's
/// environment-set invariants (how many are live, is anything still defaulting
/// to this one) and then writes must hold this, for the same reason
/// [`lock_app_for_update`] exists one level down.
pub async fn lock_project_for_update(
    conn: &mut AsyncPgConnection,
    project_id: Uuid,
) -> QueryResult<()> {
    projects::table
        .find(project_id)
        .select(projects::id)
        .for_update()
        .first::<Uuid>(conn)
        .await
        .map(|_| ())
}

pub async fn rename_project_environment(
    conn: &mut AsyncPgConnection,
    id: Uuid,
    name: &str,
) -> QueryResult<Environment> {
    diesel::update(environments::table.find(id))
        .set((
            environments::name.eq(name),
            environments::updated_at.eq(Utc::now()),
        ))
        .returning(Environment::as_returning())
        .get_result(conn)
        .await
}

/// Retire a catalogue entry and every enrollment in it, in one transaction.
///
/// Retire, never delete. The rows are kept so historical events — including any
/// already exported to cold Parquet, which no FK can reach — stay attributable.
/// Returns the public keys of the enrollments that were retired, so the caller
/// can invalidate their Redis DSN cache slots; a key whose row is retired must
/// stop resolving, and `find_env_by_public_key` filters `retired_at IS NULL`.
pub async fn retire_project_environment(
    conn: &mut AsyncPgConnection,
    id: Uuid,
) -> QueryResult<(Environment, Vec<String>)> {
    conn.transaction::<_, diesel::result::Error, _>(async |conn| {
        let now = Utc::now();
        let keys: Vec<String> = diesel::update(
            app_environments::table
                .filter(app_environments::environment_id.eq(id))
                .filter(app_environments::retired_at.is_null()),
        )
        .set((
            app_environments::retired_at.eq(Some(now)),
            app_environments::ingest_enabled.eq(false),
            // Clear the flag too. The caller refuses to retire an environment
            // that is still some app's default, so this is normally a no-op —
            // but leaving it set would put a retired row in an app's default
            // slot, which `promote_app_environment_default` can never move.
            app_environments::is_default.eq(false),
            app_environments::updated_at.eq(now),
        ))
        .returning(app_environments::public_key)
        .get_results(conn)
        .await?;

        let env = diesel::update(environments::table.find(id))
            .set((
                environments::retired_at.eq(Some(now)),
                environments::updated_at.eq(now),
            ))
            .returning(Environment::as_returning())
            .get_result(conn)
            .await?;

        Ok((env, keys))
    })
    .await
}

/// Ids of every app in a project — the fan-out target when an environment is
/// added to the catalogue. A freshly created environment has no enrollments at
/// all, so this needs no "missing" predicate.
pub async fn app_ids_in_project(
    conn: &mut AsyncPgConnection,
    project_id: Uuid,
) -> QueryResult<Vec<Uuid>> {
    apps::table
        .filter(apps::project_id.eq(project_id))
        .select(apps::id)
        .load(conn)
        .await
}

/// `(id, public_key)` of an app's live enrollments — for invalidating Redis DSN
/// cache slots when something app-wide changes.
///
/// Retired enrollments are excluded: they are already unresolvable
/// (`find_env_by_public_key` filters `retired_at IS NULL`), and excluding them
/// keeps them from consuming the row cap ahead of the live keys that matter.
pub async fn live_app_environment_keys(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
) -> QueryResult<Vec<(Uuid, String)>> {
    app_environments::table
        .filter(app_environments::app_id.eq(app_id))
        .filter(app_environments::retired_at.is_null())
        .select((app_environments::id, app_environments::public_key))
        .limit(MAX_ENVIRONMENTS_PER_PROJECT)
        .load(conn)
        .await
}

// --- app_environments: one app's enrollment in one environment ---------------

/// Batch-insert enrollments. Keys are minted by the caller rather than here so
/// that every key in the system comes from `ids::public_key()`; `sauron-db` has
/// no dependency on `sauron-core`.
pub async fn create_app_environments(
    conn: &mut AsyncPgConnection,
    rows: &[NewAppEnvironment<'_>],
) -> QueryResult<Vec<AppEnvironment>> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    diesel::insert_into(app_environments::table)
        .values(rows)
        .returning(AppEnvironment::as_returning())
        .get_results(conn)
        .await
}

/// An app's enrollments joined to their catalogue names.
///
/// A retired *catalogue* entry implies retired enrollments (they are retired in
/// the same transaction), so filtering on the enrollment's own `retired_at` is
/// sufficient and avoids a second predicate that could disagree with it.
pub async fn list_app_environments(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    include_retired: bool,
) -> QueryResult<Vec<AppEnvironmentView>> {
    let mut q = app_environments::table
        .inner_join(environments::table.on(environments::id.eq(app_environments::environment_id)))
        .filter(app_environments::app_id.eq(app_id))
        .into_boxed();
    if !include_retired {
        q = q.filter(app_environments::retired_at.is_null());
    }
    let rows: Vec<(AppEnvironment, String)> = q
        .select((AppEnvironment::as_select(), environments::name))
        .order(environments::name.asc())
        .limit(MAX_ENVIRONMENTS_PER_PROJECT)
        .load(conn)
        .await?;
    Ok(rows
        .into_iter()
        .map(|(enrollment, name)| AppEnvironmentView { enrollment, name })
        .collect())
}

pub async fn get_app_environment(
    conn: &mut AsyncPgConnection,
    id: Uuid,
) -> QueryResult<Option<AppEnvironment>> {
    app_environments::table
        .find(id)
        .select(AppEnvironment::as_select())
        .first(conn)
        .await
        .optional()
}

/// Apps that still treat `environment_id` as their default. A catalogue entry
/// with any of these cannot be retired — doing so would leave those apps with
/// nowhere to report by default.
pub async fn apps_defaulting_to_environment(
    conn: &mut AsyncPgConnection,
    environment_id: Uuid,
) -> QueryResult<i64> {
    app_environments::table
        .filter(app_environments::environment_id.eq(environment_id))
        .filter(app_environments::is_default.eq(true))
        .filter(app_environments::retired_at.is_null())
        .count()
        .get_result(conn)
        .await
}

pub async fn set_app_environment_ingest(
    conn: &mut AsyncPgConnection,
    id: Uuid,
    enabled: bool,
) -> QueryResult<AppEnvironment> {
    diesel::update(app_environments::table.find(id))
        .set((
            app_environments::ingest_enabled.eq(enabled),
            app_environments::updated_at.eq(Utc::now()),
        ))
        .returning(AppEnvironment::as_returning())
        .get_result(conn)
        .await
}

/// Take an app-level lock. Every mutation that reads an app's environment-set
/// invariants (how many are live, which is default) and then writes must hold this,
/// otherwise two such transactions on DIFFERENT rows of the same app never serialize:
/// each locks only its own environment row and both read a pre-commit count.
pub async fn lock_app_for_update(conn: &mut AsyncPgConnection, app_id: Uuid) -> QueryResult<()> {
    apps::table
        .find(app_id)
        .select(apps::id)
        .for_update()
        .first::<Uuid>(conn)
        .await
        .map(|_| ())
}

/// Move the default flag within an app. Both statements run in one transaction
/// because `app_environments_default_key` is a partial unique index on
/// `(app_id) WHERE is_default` — setting the new default before clearing the old
/// one violates it mid-statement.
pub async fn promote_app_environment_default(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    id: Uuid,
) -> QueryResult<AppEnvironment> {
    conn.transaction::<_, diesel::result::Error, _>(async |conn| {
        lock_app_for_update(conn, app_id).await?;
        diesel::update(app_environments::table)
            .filter(app_environments::app_id.eq(app_id))
            .filter(app_environments::is_default.eq(true))
            .set((
                app_environments::is_default.eq(false),
                app_environments::updated_at.eq(Utc::now()),
            ))
            .execute(conn)
            .await?;
        // `app_id` is re-asserted here rather than trusting `find(id)` alone: a caller
        // that authorized on app A but passed app B's enrollment id would otherwise
        // leave A with zero defaults and silently give B one.
        diesel::update(
            app_environments::table
                .find(id)
                .filter(app_environments::app_id.eq(app_id))
                // A retired enrollment can never become the default. Without this the
                // row lock alone is insufficient: a concurrent retire commits first, and
                // this UPDATE's WHERE still matches (retire changes neither id nor
                // app_id), flagging a retired row and leaving the app with zero live
                // defaults. The partial index cannot catch it — retired rows are not in it.
                .filter(app_environments::retired_at.is_null()),
        )
        .set((
            app_environments::is_default.eq(true),
            app_environments::updated_at.eq(Utc::now()),
        ))
        .returning(AppEnvironment::as_returning())
        .get_result(conn)
        .await
    })
    .await
}

/// Retire ONE app's enrollment. Retire, never delete: the row is kept so
/// historical events — including any already exported to cold Parquet, which no
/// FK can reach — stay attributable.
///
/// Deliberately NOT exposed over HTTP. Withdrawing a single app from an
/// environment would be a one-way door, because enrollment happens only when an
/// environment or an app is created; muting via `ingest_enabled` says the same
/// thing reversibly. The only production path that retires an enrollment is
/// [`retire_project_environment`]'s cascade, which retires all of them at once —
/// this single-row form is what lets that end state be constructed directly in
/// tests.
pub async fn retire_app_environment(
    conn: &mut AsyncPgConnection,
    id: Uuid,
) -> QueryResult<AppEnvironment> {
    let now = Utc::now();
    diesel::update(app_environments::table.find(id))
        .set((
            app_environments::retired_at.eq(Some(now)),
            app_environments::ingest_enabled.eq(false),
            // Clear the flag too. The retire handler refuses to retire a live default,
            // so this is normally a no-op — but leaving it set would make
            // `list_app_environments(include_retired = true)` return two rows flagged
            // default, and the settings UI would render two "Default" badges.
            app_environments::is_default.eq(false),
            app_environments::updated_at.eq(now),
        ))
        .returning(AppEnvironment::as_returning())
        .get_result(conn)
        .await
}

pub async fn rotate_app_environment_key(
    conn: &mut AsyncPgConnection,
    id: Uuid,
    new_key: &str,
) -> QueryResult<AppEnvironment> {
    diesel::update(app_environments::table.find(id))
        .set((
            app_environments::public_key.eq(new_key),
            app_environments::updated_at.eq(Utc::now()),
        ))
        .returning(AppEnvironment::as_returning())
        .get_result(conn)
        .await
}

/// Resolve an ingest key to its environment and full ancestry in one query.
/// Retired environments are excluded, so a retired key is indistinguishable from
/// an unknown one and falls through to the existing `invalid_key` path.
pub async fn find_env_by_public_key(
    conn: &mut AsyncPgConnection,
    public_key: &str,
) -> QueryResult<Option<EnvRef>> {
    app_environments::table
        .inner_join(apps::table.on(apps::id.eq(app_environments::app_id)))
        .inner_join(projects::table.on(projects::id.eq(apps::project_id)))
        .filter(app_environments::public_key.eq(public_key))
        .filter(app_environments::retired_at.is_null())
        .select((
            app_environments::id,
            apps::id,
            apps::project_id,
            projects::org_id,
            app_environments::ingest_enabled,
            apps::ingest_enabled,
        ))
        .first::<(Uuid, Uuid, Uuid, Uuid, bool, bool)>(conn)
        .await
        .optional()
        .map(|row| {
            row.map(
                |(env_id, app_id, project_id, org_id, env_ingest_enabled, app_ingest_enabled)| {
                    EnvRef {
                        env_id,
                        app_id,
                        project_id,
                        org_id,
                        env_ingest_enabled,
                        app_ingest_enabled,
                    }
                },
            )
        })
}

/// `(app_id, project_id, org_id)` ancestry of an environment — for permission
/// resolution, mirroring `app_ancestry`. Slice 3's `authorize_env` reuses this.
pub async fn env_ancestry(
    conn: &mut AsyncPgConnection,
    env_id: Uuid,
) -> QueryResult<Option<(Uuid, Uuid, Uuid)>> {
    app_environments::table
        .inner_join(apps::table.on(apps::id.eq(app_environments::app_id)))
        .inner_join(projects::table.on(projects::id.eq(apps::project_id)))
        .filter(app_environments::id.eq(env_id))
        .select((app_environments::app_id, apps::project_id, projects::org_id))
        .first(conn)
        .await
        .optional()
}

/// Every enrollment id of an app, **including retired ones**.
///
/// Retired enrollments are included deliberately: their history stays
/// readable (an app's data does not disappear because an environment was
/// retired), so a caller's readable subset must be able to contain one.
/// `list_app_environments` excludes them because they must not be *selectable*;
/// that is a different question from whether they are *readable*.
///
/// These are `app_environments` ids, which is what `role_grants.scope_id` holds
/// for `scope_type = 'env'` — an env grant names one app's enrollment, never the
/// catalogue entry, so that granting "staging" cannot silently span sibling apps.
pub async fn env_ids_for_app(conn: &mut AsyncPgConnection, app_id: Uuid) -> QueryResult<Vec<Uuid>> {
    app_environments::table
        .filter(app_environments::app_id.eq(app_id))
        .select(app_environments::id)
        .load(conn)
        .await
}

/// `(app_id, app_environments.id)` for every enrollment of every app in
/// `app_ids` — the batched [`env_ids_for_app`], same semantics INCLUDING
/// retired enrollments (retired history stays readable, and
/// `resolve_env_filter` needs the full set for its `EnvNotInApp` check).
///
/// Callers MUST fold this into a map keyed by `app_id` and hand
/// `resolve_env_filter` only that app's slice. `resolve_env_filter` uses
/// `app_env_ids` for two decisions — the `EnvNotInApp` membership test and
/// `readable = app_env_ids ∩ reach.envs` — and the union across several apps
/// breaks both in the same direction, TOWARDS GRANTING. Concretely: a caller
/// holding an env grant only on app B's staging enrollment, asking for app A,
/// gets a non-empty `readable` for app A (it contains app B's id), so instead
/// of `NoReach` → 403, app A resolves to a `Subset` naming an environment that
/// is not its own and contributes zero rows, silently, inside a combined number
/// the caller should have been refused outright.
///
/// Deliberately unordered and unlimited, unlike `list_app_environments`: this
/// feeds an authorization decision, and a truncated or filtered input to that
/// decision is a wrong answer, not a shorter list.
pub async fn env_ids_for_apps(
    conn: &mut AsyncPgConnection,
    app_ids: &[Uuid],
) -> QueryResult<Vec<(Uuid, Uuid)>> {
    if app_ids.is_empty() {
        return Ok(Vec::new());
    }
    app_environments::table
        .filter(app_environments::app_id.eq_any(app_ids.to_vec()))
        .select((app_environments::app_id, app_environments::id))
        .load(conn)
        .await
}

// ===========================================================================
// Issues & error events (app-scoped)
// ===========================================================================

pub async fn upsert_issue(conn: &mut AsyncPgConnection, new: NewIssue<'_>) -> QueryResult<Uuid> {
    diesel::insert_into(issues::table)
        .values(&new)
        .on_conflict((issues::app_id, issues::fingerprint))
        .do_update()
        .set((
            // GREATEST/LEAST rather than a bare `excluded.*` overwrite, so the
            // stored window does not depend on the order occurrences happen to
            // be processed in. A bare overwrite let a late-arriving OLDER
            // occurrence drag `last_seen` backwards, and it is also what made
            // this statement disagree with the batched path (which folds a
            // whole batch before writing, and therefore has no processing order
            // to inherit). Both spellings now agree and both are order-free.
            issues::last_seen.eq(sql::<Timestamptz>(
                "GREATEST(issues.last_seen, excluded.last_seen)",
            )),
            issues::first_seen.eq(sql::<Timestamptz>(
                "LEAST(issues.first_seen, excluded.first_seen)",
            )),
            issues::times_seen.eq(issues::times_seen + 1),
            issues::level.eq(excluded(issues::level)),
            // Sticky mask guard. `error_events.title` is derived server-side by
            // `build_title(exc, message)` and has no wire field, so forward
            // enforcement alone leaves two gaps on the Issues page: PII inside
            // `exception_type`, which `build_title` also concatenates, and the 30s
            // policy-cache window. Both restore the raw string on the very next
            // occurrence. One string compare on a write bounded by DISTINCT
            // FINGERPRINTS, not by event volume.
            //
            // This is permanent: once a fingerprint's title is '****' it stays
            // '****' forever, even if every subsequent occurrence is benign. That is
            // the correct trade — a fingerprint is a stable error identity — but it
            // is a visible regression on the most-looked-at page in the product, and
            // support will be asked about it. It is in the wiki.
            issues::title.eq(diesel::dsl::sql::<diesel::sql_types::Text>(
                "CASE WHEN issues.title = '****' THEN issues.title ELSE excluded.title END",
            )),
            issues::culprit.eq(diesel::dsl::sql::<diesel::sql_types::Text>(
                "CASE WHEN issues.culprit = '****' THEN issues.culprit ELSE excluded.culprit END",
            )),
            issues::updated_at.eq(Utc::now()),
            // Ingest-side watermark for the regression trigger. Set here and
            // nowhere else: keying regression off `last_seen` (client clock)
            // let a poll tick advance past a just-ingested event and drop the
            // alert, and keying it off `updated_at` would fire a bogus
            // "regressed" alert every time someone resolved an issue.
            issues::last_event_at.eq(Utc::now()),
        ))
        .returning(issues::id)
        .get_result(conn)
        .await
}

pub async fn insert_error_event(
    conn: &mut AsyncPgConnection,
    ev: NewErrorEvent,
) -> QueryResult<usize> {
    diesel::insert_into(error_events::table)
        .values(&ev)
        .execute(conn)
        .await
}

/// Raw-SQL row shape for [`list_issues`]/[`get_issue`]/[`top_issues`] under
/// `EnvFilter::One`/`Unattributed`, where `times_seen`/`users_seen`/
/// `first_seen`/`last_seen` come from a per-environment aggregate rather than
/// `issues`' own (app-wide) columns. A local `QueryableByName` struct rather
/// than widening the shared `Issue` model — `Issue` derives `Queryable`/
/// `Selectable` for its many other diesel-query-builder call sites, and this
/// file's convention for a raw-SQL-only row shape (`IssueStatsRow`,
/// `PersonRow`, `DeviceRow`) is a dedicated struct with an explicit
/// `sql_type` per field.
#[derive(Debug, QueryableByName)]
struct IssueRow {
    #[diesel(sql_type = SqlUuid)]
    id: Uuid,
    #[diesel(sql_type = SqlUuid)]
    app_id: Uuid,
    #[diesel(sql_type = Text)]
    fingerprint: String,
    #[diesel(sql_type = Text)]
    type_: String,
    #[diesel(sql_type = Text)]
    title: String,
    #[diesel(sql_type = Text)]
    culprit: String,
    #[diesel(sql_type = Text)]
    level: String,
    #[diesel(sql_type = Text)]
    status: String,
    #[diesel(sql_type = Timestamptz)]
    first_seen: DateTime<Utc>,
    #[diesel(sql_type = Timestamptz)]
    last_seen: DateTime<Utc>,
    #[diesel(sql_type = BigInt)]
    times_seen: i64,
    #[diesel(sql_type = BigInt)]
    users_seen: i64,
    #[diesel(sql_type = Nullable<SqlUuid>)]
    assignee_id: Option<Uuid>,
    #[diesel(sql_type = Timestamptz)]
    created_at: DateTime<Utc>,
    #[diesel(sql_type = Timestamptz)]
    updated_at: DateTime<Utc>,
    #[diesel(sql_type = Timestamptz)]
    last_event_at: DateTime<Utc>,
}

impl From<IssueRow> for Issue {
    fn from(r: IssueRow) -> Self {
        Issue {
            id: r.id,
            app_id: r.app_id,
            fingerprint: r.fingerprint,
            type_: r.type_,
            title: r.title,
            culprit: r.culprit,
            level: r.level,
            status: r.status,
            first_seen: r.first_seen,
            last_seen: r.last_seen,
            times_seen: r.times_seen,
            users_seen: r.users_seen,
            assignee_id: r.assignee_id,
            created_at: r.created_at,
            updated_at: r.updated_at,
            last_event_at: r.last_event_at,
        }
    }
}

/// Lists issues for an app, optionally scoped to one environment.
///
/// `issues` has no `environment_id` and — per Task 1's write-path measurement
/// — no `issue_environments` rollup either (see the design doc's "No new
/// table"). `EnvFilter::All` therefore reads `issues` directly: no join, no
/// subquery, the same query this function ran before Slice 2. That is the
/// path almost every request takes, and it must not regress.
///
/// `EnvFilter::One`/`Unattributed` cannot use `issues`' own `times_seen`/
/// `users_seen`/`first_seen`/`last_seen` — they are app-wide. So: page the
/// issues first (bounded by `limit`/`offset`, ordered by the issue's own
/// `last_seen`, the same order `All` uses — no derived value exists yet to
/// order by), then `JOIN LATERAL` each returned row against `error_events`
/// (`error_events_issue_env_idx` makes this an index scan) to derive the
/// four returned aggregate values.
///
/// That inner paging order is necessarily app-wide (see above), but the
/// **outer**, final `ORDER BY agg.last_seen DESC` is not — it runs after the
/// LATERAL, over at most one page of already-materialized rows, where the
/// derived, per-environment `last_seen` exists. Ordering the returned page by
/// `i.last_seen` (app-wide) instead would sort by a column the caller is not
/// even shown: an issue last seen in `env_b` an hour ago but in `env_a` a
/// month ago would outrank one last seen in `env_a` ten minutes ago, on a
/// page whose every displayed timestamp is `env_a`-scoped. Same shape as
/// `top_issues`' own fix (`ORDER BY agg.times_seen DESC`, below) — read that
/// function's doc comment for the identical reasoning applied to a different
/// column.
///
/// Membership — "does this issue actually belong to the selected
/// environment at all" — is enforced *twice*, deliberately:
/// 1. The paging subquery itself carries `AND EXISTS (SELECT 1 FROM
///    error_events m WHERE m.issue_id = issues.id{env predicate})`. Without
///    this, `LIMIT`/`OFFSET` page by the issue's *app-wide* `last_seen`
///    before membership is known at all — an issue whose only activity is
///    in a different environment can still consume a page slot ahead of a
///    genuine member, producing non-monotonic pages and even an empty first
///    page while a later page returns real rows. Reproduced against the real
///    dev app (`One(demo)`, `limit 5`): `offset 0` returned 0 rows, `offset
///    5` returned 2, `offset 10` returned 5 — see
///    `.superpowers/sdd/s2-task-9-report.md`'s "Critical findings fixed"
///    section for the fixed timings.
/// 2. The `JOIN LATERAL` carries `HAVING count(*) > 0`: an issue with zero
///    occurrences in the selected environment produces zero rows from the
///    LATERAL and is dropped by the inner join. Without it, an aggregate
///    with no `GROUP BY` always returns exactly one row (`count = 0`,
///    `min`/`max` = `NULL`) even when nothing matches, which would silently
///    turn the inner join into a no-op `LEFT JOIN` in every practical sense.
///
/// Do not "simplify" either check away: the seed's `issue_env_b_only`
/// (confined to `env_b` alone) exists specifically to catch a regression —
/// it must not appear at all under `One(env_a)`, regardless of which of the
/// two checks would otherwise have let it through.
///
/// The `tag`/free-text `q` `EXISTS` fragments below carry the identical
/// environment predicate (reusing the single `$3` env bind — see the
/// bind-layout comment further down for why it is allocated early enough for
/// them to reach it). Without it, a tag or payload match that exists only in
/// a *different* environment could surface an issue under a scope that
/// excludes it, or — worse — let a free-text `q` extract characters from
/// that other environment's `tags`/`contexts`/`extra`, exactly where PII and
/// secrets live, even though the row's own displayed counts stayed correctly
/// scoped. See `list_issues_tag_and_q_do_not_leak_across_environments` in
/// `env_scoping.rs` for the regression test.
///
/// `since` is pushed into the LATERAL's own `WHERE e.occurred_at >= $2`
/// rather than only checked against the result afterward — so under
/// `One`/`Unattributed`, the returned `times_seen`/`users_seen`/
/// `first_seen`/`last_seen` are counts *within the requested window*, not
/// lifetime, and will not match `issues.times_seen` (lifetime, incremented
/// at ingest) even for the same environment under `All`. Deliberate, not a
/// bug: a list already filtered to "seen in the last N days" showing
/// lifetime counts beside it would be incoherent, and windowing restores
/// partition pruning on `error_events` (time-partitioned; an unbounded scan
/// cannot prune) — measured on the real 210k-event dev app at `LIMIT 50`,
/// see the report section above for the before/after. The outer `WHERE
/// agg.last_seen >= $2` is now provably redundant given the pushed-in bound
/// (every row the LATERAL emits already has `occurred_at >= $2`, so its
/// `max(occurred_at)` does too) — kept anyway as a second, harmless check,
/// same "verify membership twice" philosophy as above. One consequence:
/// because a paged issue can still fail the LATERAL's own window/`HAVING`
/// (a genuine member with no occurrence inside `since` specifically), the
/// page can come back shorter than `limit` even when more genuinely-matching
/// issues exist past the current `OFFSET`. Accepted in exchange for never
/// aggregating more than one page of issues per request — the cost trade
/// this whole design exists to make (see the design doc).
///
/// Three further discrepancies, neither a bug:
/// 1. Per-environment `users_seen` is an exact `count(DISTINCT distinct_id)`
///    over `error_events`; the app-wide `issues.users_seen` is maintained
///    from a Redis HyperLogLog and is approximate. They will disagree
///    slightly — the per-environment number is the more accurate one.
/// 2. Per-environment counts cannot see tiered data: once `sauron-tier`
///    exports a partition older than `TIER_HOT_DAYS` to Parquet and drops it,
///    those occurrences leave `error_events`, so a per-environment count over
///    an older window under-reports. `issues.times_seen` does not, because it
///    was incremented at ingest — which is also why `All` keeps reading it
///    directly rather than switching to the same derivation.
/// 3. Per-environment counts are windowed by `since` (see above); app-wide
///    counts under `All` are not windowed the same way (`All`'s own `since`
///    filters which issues survive, via `issues.last_seen`, but the counts
///    it returns are still lifetime). A `One(env)` view and an `All` view of
///    the same request can therefore report different numbers for the same
///    issue even setting the first two discrepancies aside.
///
/// **Task 9: `title`/`culprit`/`level` are now derived per environment too**,
/// the same shape as the four aggregate values above. A second `LEFT JOIN
/// LATERAL` (`latest`) beside `agg` selects `title`/`culprit`/`level` from
/// the single newest `error_events` row in the selected environment
/// (`ORDER BY e.occurred_at DESC LIMIT 1`); the outer select list reads
/// `COALESCE(latest.title, i.title)` etc. `LEFT JOIN` + `COALESCE`, not an
/// inner join: a row written before migration 30 has `title`/`culprit =
/// NULL` (they were only added then, not backfilled), and must fall back to
/// the app-wide `issues` column rather than vanishing from the page. The
/// `latest` LATERAL's own `WHERE e.issue_id = i.id{env}` fragment reuses the
/// **same bound env value** (`env_bind_idx`, `$3`) `agg` already uses — no
/// new bind is allocated; see the bind-layout comment below, which this
/// doesn't change. `error_events.level` is `NOT NULL`, and — because
/// `agg`'s own `HAVING count(*) > 0` (or the paging subquery's membership
/// `EXISTS`) already guarantees at least one in-environment row exists —
/// `latest` always finds a row too; `COALESCE` here is purely for the
/// `title`/`culprit` legacy-NULL case, not a "no row" case.
///
/// **Task 9 also moved four filters onto those derived values.** `level`,
/// `culprit`, `times_seen`, `users_seen` used to sit inside the paging
/// subquery, compared against `issues`' stored (app-wide) columns, while the
/// row displayed the derived ones — so `?level=error` and the level shown on
/// the row could disagree, the same class of bug the tag/`q` `EXISTS`
/// fragments had before the S2 Task 10 review fixed *their* environment
/// leak. Those four now live in the **outer** query, compared against
/// `latest.level` / `latest.culprit` / `agg.times_seen` / `agg.users_seen`.
/// `status` and `type` stay in the subquery, comparing against `issues`'
/// own columns — they are genuinely app-wide attributes with no
/// per-environment meaning (`issue_stats` makes the identical call for
/// `status`), so there is no derived value to move them to. `tag`'s `EXISTS`
/// fragments are unaffected either way (already environment-scoped, not
/// stored-column-compared).
///
/// **Trade accepted, not overlooked:** moving those four filters out of the
/// subquery means the subquery can no longer pre-filter on them, so it now
/// pages a *wider* candidate set (bounded only by `app_id`/`status`/`type`/
/// `tag`/environment membership) before the outer query narrows it by
/// `level`/`culprit`/`times_seen`/`users_seen`. A page can therefore come
/// back with fewer than `limit` rows even when more genuinely-matching
/// issues exist past `offset` — OFFSET-based paging combined with one of
/// these four filters is no longer exact beyond the first page. Accepted
/// because the only real caller, `Issues.svelte`, always requests `limit:
/// 100` with no `offset` (see `dashboard/src/pages/Issues.svelte`'s `load()`)
/// — chosen deliberately, not left as an unnoticed regression; revisit if a
/// second caller ever pages past offset 0 with one of these filters set.
pub async fn list_issues(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    filters: &[ParsedFilter],
    q: Option<&str>,
    since: chrono::DateTime<chrono::Utc>,
    limit: i64,
    offset: i64,
) -> QueryResult<Vec<Issue>> {
    if matches!(scope.env, EnvFilter::All) {
        let mut query = issues::table
            .filter(issues::app_id.eq(scope.app_id))
            .filter(issues::last_seen.ge(since))
            .into_boxed();
        for f in filters {
            query = match (f.field, f.op) {
                ("level", Op::Eq) => query.filter(issues::level.eq(f.value.clone())),
                ("level", Op::Neq) => query.filter(issues::level.ne(f.value.clone())),
                ("status", Op::Eq) => query.filter(issues::status.eq(f.value.clone())),
                ("status", Op::Neq) => query.filter(issues::status.ne(f.value.clone())),
                ("type", Op::Eq) => query.filter(issues::type_.eq(f.value.clone())),
                ("type", Op::Neq) => query.filter(issues::type_.ne(f.value.clone())),
                ("type", Op::Contains) => {
                    query.filter(issues::type_.ilike(like_contains(&f.value)))
                }
                ("culprit", Op::Eq) => query.filter(issues::culprit.eq(f.value.clone())),
                ("culprit", Op::Neq) => query.filter(issues::culprit.ne(f.value.clone())),
                ("culprit", Op::Contains) => {
                    query.filter(issues::culprit.ilike(like_contains(&f.value)))
                }
                ("times_seen", Op::Eq) => query.filter(issues::times_seen.eq(as_i64(&f.value))),
                ("times_seen", Op::Gt) => query.filter(issues::times_seen.gt(as_i64(&f.value))),
                ("times_seen", Op::Lt) => query.filter(issues::times_seen.lt(as_i64(&f.value))),
                ("users_seen", Op::Eq) => query.filter(issues::users_seen.eq(as_i64(&f.value))),
                ("users_seen", Op::Gt) => query.filter(issues::users_seen.gt(as_i64(&f.value))),
                ("users_seen", Op::Lt) => query.filter(issues::users_seen.lt(as_i64(&f.value))),
                ("tag", Op::Eq) => {
                    let (k, v) = tag_kv(&f.value);
                    query.filter(
                        sql::<Bool>(
                            "EXISTS (SELECT 1 FROM error_events e \
                             WHERE e.issue_id = issues.id AND e.app_id = issues.app_id AND e.tags @> ",
                        )
                        .bind::<Jsonb, _>(tag_object(k, v))
                        .sql(")"),
                    )
                }
                ("tag", Op::Contains) => {
                    let (k, v) = tag_kv(&f.value);
                    query.filter(
                        sql::<Bool>(
                            "EXISTS (SELECT 1 FROM error_events e \
                             WHERE e.issue_id = issues.id AND e.app_id = issues.app_id AND e.tags ->> ",
                        )
                        .bind::<Text, _>(k)
                        .sql(" ILIKE ")
                        .bind::<Text, _>(like_contains(&v))
                        .sql(")"),
                    )
                }
                // `issues` itself carries no `workflow_name` column — narrow via
                // an EXISTS against the environment-stamped child row, same
                // idiom as `tag` just above.
                //
                // `e.workflow_id IS NOT NULL` is here for the partial-index
                // reason Task 4 measured and documented on `workflow_detail`'s
                // `top_events`/`top_issues` queries: migration
                // 2026-07-29-000032's `error_events_app_workflow_idx` is
                // PARTIAL (`WHERE workflow_id IS NOT NULL`), and Postgres uses
                // a partial index only when the query's WHERE *implies* the
                // index predicate — `workflow_name = $N` does not, they are
                // different columns. Semantically a no-op (the pipeline stamps
                // id and name together, so a row with a name always has an id).
                // Milder here than in `workflow_detail`, because this EXISTS is
                // correlated on `e.issue_id = issues.id` and so has
                // `error_events_issue_idx` as a bounded fallback — but the term
                // is free and is the only thing that lets the planner consider
                // the workflow index at all.
                ("workflow", Op::Eq) => query.filter(
                    sql::<Bool>(
                        "EXISTS (SELECT 1 FROM error_events e \
                         WHERE e.issue_id = issues.id AND e.app_id = issues.app_id \
                         AND e.workflow_id IS NOT NULL AND e.workflow_name = ",
                    )
                    .bind::<Text, _>(f.value.clone())
                    .sql(")"),
                ),
                // NOT EXISTS, so an issue whose occurrences are all unstamped
                // (no workflow at all) DOES match `neq` — "not part of workflow
                // X" is true of a row that is part of no workflow. See
                // `list_error_events_for_issue`'s `workflow` arms for the
                // occurrence-level predicate deliberately built to agree with
                // this, and why that one departs from its own file's
                // `session_id`/`release` precedent to do so.
                ("workflow", Op::Neq) => query.filter(
                    sql::<Bool>(
                        "NOT EXISTS (SELECT 1 FROM error_events e \
                         WHERE e.issue_id = issues.id AND e.app_id = issues.app_id \
                         AND e.workflow_id IS NOT NULL AND e.workflow_name = ",
                    )
                    .bind::<Text, _>(f.value.clone())
                    .sql(")"),
                ),
                ("workflow", Op::Contains) => query.filter(
                    sql::<Bool>(
                        "EXISTS (SELECT 1 FROM error_events e \
                         WHERE e.issue_id = issues.id AND e.app_id = issues.app_id \
                         AND e.workflow_id IS NOT NULL AND e.workflow_name ILIKE ",
                    )
                    .bind::<Text, _>(like_contains(&f.value))
                    .sql(")"),
                ),
                _ => query, // unreachable: Task 1 whitelists field+op
            };
        }
        if let Some(term) = q {
            let p = like_contains(term);
            query = query.filter(
                issues::title
                    .ilike(p.clone())
                    .or(issues::type_.ilike(p.clone()))
                    .or(issues::culprit.ilike(p.clone()))
                    // Payload search casts jsonb to text, which no index can serve.
                    // Bounding the correlated scan by time is what keeps it viable:
                    // without it, an issue with no match forces a full scan of that
                    // issue's entire event history — for EVERY issue in the app.
                    // `since` is always supplied by the caller (see `list()` in
                    // `routes/issues.rs`) — there used to be a `MAX_PAYLOAD_SEARCH_
                    // DAYS` fallback for when it wasn't, but every route already
                    // passed `Some(since)`, so that fallback never fired. Deleted
                    // rather than kept as a guard that reads as protection but
                    // isn't one.
                    .or(sql::<Bool>(
                        "EXISTS (SELECT 1 FROM error_events e \
                         WHERE e.issue_id = issues.id AND e.app_id = issues.app_id \
                         AND e.occurred_at >= ",
                    )
                    .bind::<Timestamptz, _>(since)
                    .sql(" AND (e.contexts::text ILIKE ")
                    .bind::<Text, _>(p.clone())
                    .sql(" OR e.extra::text ILIKE ")
                    .bind::<Text, _>(p.clone())
                    .sql(" OR e.tags::text ILIKE ")
                    .bind::<Text, _>(p)
                    .sql("))")),
            );
        }
        return query
            .select(Issue::as_select())
            .order(issues::last_seen.desc())
            .limit(limit)
            .offset(offset)
            .load(conn)
            .await;
    }

    // ----- One / Unattributed: page first, aggregate via an inner-join LATERAL -----
    //
    // Bind layout: $1 app_id, $2 since. $3 is `env`, allocated *before* the
    // filter loop — unlike every other raw-SQL function in this file, where
    // env is last — because the tag/q `EXISTS` fragments and the paging
    // subquery's own membership `EXISTS` all need to reference the same
    // bound value too, alongside the LATERAL's own; one bind reused
    // everywhere it's needed, same idiom as reusing `$2` for `since`. Under
    // `One`/`Subset`, $3 is bound (`scope.env.consumes_bind()` is `true`) and filters
    // start at $4; under `Unattributed`, $3 is never referenced in the SQL
    // text at all (a literal `IS NULL` needs no bind) and no bind is pushed
    // for it, so filters start at $3 instead — `next_bind`'s initial value
    // is computed from whether `env` actually consumed a bind, specifically
    // so the two cases can never disagree about which placeholder is next.
    // Filters/tag/`q` consume the following numbers dynamically, one bind
    // per distinct value — a value referenced several times in the text
    // reuses its one placeholder, same idiom as `list_persons`' `$5`.
    // limit/offset follow last. Placeholders can appear out of numeric order
    // in the SQL text itself (`$3`'s env fragment sits inside the LATERAL,
    // textually after `$4`'s filter fragment in the paging subquery) —
    // Postgres only requires that the *n*th `.bind()` call supply `$n`, not
    // that `$n` appear before `$n+1` in the text.
    // Every filter/tag/q fragment below is textually inside `SELECT * FROM
    // issues WHERE app_id = $1{filter_sql}` — the *inner* paging subquery,
    // one nesting level below where the `i` alias applies (that alias names
    // the subquery's own *result*, not any scope visible inside it; see
    // `list_persons`' doc comment for the identical situation). So these use
    // bare column names / the literal table name `issues`, never `i.`.
    let env_bind_idx = 3usize;
    let mut next_bind = if scope.env.consumes_bind() {
        4usize
    } else {
        3usize
    };
    let env_sql = scope.env.sql_fragment_for("e", env_bind_idx);
    let member_env_sql = scope.env.sql_fragment_for("m", env_bind_idx);
    // Task 9: split in two. `filter_sql` stays textually inside the paging
    // subquery (`status`/`type`/`tag` — genuinely app-wide columns, or
    // already-environment-scoped `EXISTS`es); `outer_filter_sql` moves to
    // the outer query, after the `agg`/`latest` LATERALs exist to be
    // compared against (`level`/`culprit`/`times_seen`/`users_seen` — see
    // this function's doc comment for why). Both still consume `next_bind`
    // from the same single counter, in the same `filters` iteration order,
    // as before — only *where* each fragment's text lands changed, not the
    // bind numbering, so the bind loop below (which binds by `f.field`/
    // `f.op`, not by which string it went into) needs no change.
    let mut filter_sql = String::new();
    let mut outer_filter_sql = String::new();
    for f in filters {
        match (f.field, f.op) {
            ("level", Op::Eq) => {
                outer_filter_sql += &format!(" AND latest.level = ${next_bind}");
                next_bind += 1;
            }
            ("level", Op::Neq) => {
                outer_filter_sql += &format!(" AND latest.level <> ${next_bind}");
                next_bind += 1;
            }
            ("status", Op::Eq) => {
                filter_sql += &format!(" AND status = ${next_bind}");
                next_bind += 1;
            }
            ("status", Op::Neq) => {
                filter_sql += &format!(" AND status <> ${next_bind}");
                next_bind += 1;
            }
            ("type", Op::Eq) => {
                filter_sql += &format!(" AND type = ${next_bind}");
                next_bind += 1;
            }
            ("type", Op::Neq) => {
                filter_sql += &format!(" AND type <> ${next_bind}");
                next_bind += 1;
            }
            ("type", Op::Contains) => {
                filter_sql += &format!(" AND type ILIKE ${next_bind}");
                next_bind += 1;
            }
            ("culprit", Op::Eq) => {
                outer_filter_sql += &format!(" AND latest.culprit = ${next_bind}");
                next_bind += 1;
            }
            ("culprit", Op::Neq) => {
                outer_filter_sql += &format!(" AND latest.culprit <> ${next_bind}");
                next_bind += 1;
            }
            ("culprit", Op::Contains) => {
                outer_filter_sql += &format!(" AND latest.culprit ILIKE ${next_bind}");
                next_bind += 1;
            }
            ("times_seen", Op::Eq) => {
                outer_filter_sql += &format!(" AND agg.times_seen = ${next_bind}");
                next_bind += 1;
            }
            ("times_seen", Op::Gt) => {
                outer_filter_sql += &format!(" AND agg.times_seen > ${next_bind}");
                next_bind += 1;
            }
            ("times_seen", Op::Lt) => {
                outer_filter_sql += &format!(" AND agg.times_seen < ${next_bind}");
                next_bind += 1;
            }
            ("users_seen", Op::Eq) => {
                outer_filter_sql += &format!(" AND agg.users_seen = ${next_bind}");
                next_bind += 1;
            }
            ("users_seen", Op::Gt) => {
                outer_filter_sql += &format!(" AND agg.users_seen > ${next_bind}");
                next_bind += 1;
            }
            ("users_seen", Op::Lt) => {
                outer_filter_sql += &format!(" AND agg.users_seen < ${next_bind}");
                next_bind += 1;
            }
            ("tag", Op::Eq) => {
                let te_env = scope.env.sql_fragment_for("te", env_bind_idx);
                filter_sql += &format!(
                    " AND EXISTS (SELECT 1 FROM error_events te WHERE te.issue_id = issues.id \
                      AND te.app_id = issues.app_id AND te.tags @> ${next_bind}{te_env})"
                );
                next_bind += 1;
            }
            ("tag", Op::Contains) => {
                let te_env = scope.env.sql_fragment_for("te", env_bind_idx);
                filter_sql += &format!(
                    " AND EXISTS (SELECT 1 FROM error_events te WHERE te.issue_id = issues.id \
                      AND te.app_id = issues.app_id AND te.tags ->> ${a} ILIKE ${b}{te_env})",
                    a = next_bind,
                    b = next_bind + 1
                );
                next_bind += 2;
            }
            // `issues` itself carries no `workflow_name` column — same EXISTS
            // idiom as `tag` just above, against a distinct alias (`we`) so it
            // cannot collide with a `tag`/`q` EXISTS in the same query text.
            // `we.workflow_id IS NOT NULL` is the partial-index term; see the
            // `EnvFilter::All` branch's `workflow` arms above for the full
            // reasoning, and for why `Neq` is `NOT EXISTS` (unstamped rows
            // match) rather than a `<>` that would drop them.
            ("workflow", Op::Eq) => {
                let we_env = scope.env.sql_fragment_for("we", env_bind_idx);
                filter_sql += &format!(
                    " AND EXISTS (SELECT 1 FROM error_events we WHERE we.issue_id = issues.id \
                      AND we.app_id = issues.app_id AND we.workflow_id IS NOT NULL \
                      AND we.workflow_name = ${next_bind}{we_env})"
                );
                next_bind += 1;
            }
            ("workflow", Op::Neq) => {
                let we_env = scope.env.sql_fragment_for("we", env_bind_idx);
                filter_sql += &format!(
                    " AND NOT EXISTS (SELECT 1 FROM error_events we WHERE we.issue_id = issues.id \
                      AND we.app_id = issues.app_id AND we.workflow_id IS NOT NULL \
                      AND we.workflow_name = ${next_bind}{we_env})"
                );
                next_bind += 1;
            }
            ("workflow", Op::Contains) => {
                let we_env = scope.env.sql_fragment_for("we", env_bind_idx);
                filter_sql += &format!(
                    " AND EXISTS (SELECT 1 FROM error_events we WHERE we.issue_id = issues.id \
                      AND we.app_id = issues.app_id AND we.workflow_id IS NOT NULL \
                      AND we.workflow_name ILIKE ${next_bind}{we_env})"
                );
                next_bind += 1;
            }
            _ => {} // unreachable: Task 1 whitelists field+op
        }
    }
    let q_bind = q.map(|_| {
        let b = next_bind;
        next_bind += 1;
        b
    });
    if let Some(b) = q_bind {
        let qe_env = scope.env.sql_fragment_for("qe", env_bind_idx);
        filter_sql += &format!(
            " AND (title ILIKE ${b} OR type ILIKE ${b} OR culprit ILIKE ${b} \
              OR EXISTS (SELECT 1 FROM error_events qe WHERE qe.issue_id = issues.id \
              AND qe.app_id = issues.app_id AND qe.occurred_at >= $2 \
              AND (qe.contexts::text ILIKE ${b} OR qe.extra::text ILIKE ${b} OR qe.tags::text ILIKE ${b}){qe_env}))"
        );
    }
    let limit_bind = next_bind;
    next_bind += 1;
    let offset_bind = next_bind;

    let sql_text = format!(
        "SELECT i.id, i.app_id, i.fingerprint, i.type AS type_, \
                COALESCE(latest.title, i.title)     AS title, \
                COALESCE(latest.culprit, i.culprit) AS culprit, \
                COALESCE(latest.level, i.level)     AS level, \
                i.status, \
                agg.first_seen, agg.last_seen, agg.times_seen, agg.users_seen, \
                i.assignee_id, i.created_at, i.updated_at, i.last_event_at \
         FROM ( \
             SELECT * FROM issues \
             WHERE app_id = $1{filter_sql} \
               AND EXISTS (SELECT 1 FROM error_events m WHERE m.issue_id = issues.id{member_env_sql}) \
             ORDER BY last_seen DESC \
             LIMIT ${limit_bind} OFFSET ${offset_bind} \
         ) i \
         JOIN LATERAL ( \
             SELECT count(*)::bigint AS times_seen, \
                    count(DISTINCT distinct_id)::bigint AS users_seen, \
                    min(occurred_at) AS first_seen, \
                    max(occurred_at) AS last_seen \
             FROM error_events e \
             WHERE e.issue_id = i.id AND e.occurred_at >= $2{env_sql} \
             HAVING count(*) > 0 \
         ) agg ON TRUE \
         LEFT JOIN LATERAL ( \
             SELECT e.title, e.culprit, e.level \
             FROM error_events e \
             WHERE e.issue_id = i.id{env_sql} \
             ORDER BY e.occurred_at DESC \
             LIMIT 1 \
         ) latest ON TRUE \
         WHERE agg.last_seen >= $2{outer_filter_sql} \
         ORDER BY agg.last_seen DESC"
    );

    let mut stmt = diesel::sql_query(sql_text)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Timestamptz, _>(since);
    stmt = crate::bind_env!(stmt, &scope.env);
    for f in filters {
        stmt = match (f.field, f.op) {
            ("level", Op::Eq)
            | ("level", Op::Neq)
            | ("status", Op::Eq)
            | ("status", Op::Neq)
            | ("type", Op::Eq)
            | ("type", Op::Neq)
            | ("culprit", Op::Eq)
            | ("culprit", Op::Neq) => stmt.bind::<Text, _>(f.value.clone()),
            ("type", Op::Contains) | ("culprit", Op::Contains) => {
                stmt.bind::<Text, _>(like_contains(&f.value))
            }
            ("times_seen", Op::Eq)
            | ("times_seen", Op::Gt)
            | ("times_seen", Op::Lt)
            | ("users_seen", Op::Eq)
            | ("users_seen", Op::Gt)
            | ("users_seen", Op::Lt) => stmt.bind::<BigInt, _>(as_i64(&f.value)),
            ("tag", Op::Eq) => {
                let (k, v) = tag_kv(&f.value);
                stmt.bind::<Jsonb, _>(tag_object(k, v))
            }
            ("tag", Op::Contains) => {
                let (k, v) = tag_kv(&f.value);
                stmt.bind::<Text, _>(k).bind::<Text, _>(like_contains(&v))
            }
            ("workflow", Op::Eq) | ("workflow", Op::Neq) => stmt.bind::<Text, _>(f.value.clone()),
            ("workflow", Op::Contains) => stmt.bind::<Text, _>(like_contains(&f.value)),
            _ => stmt,
        };
    }
    if let Some(term) = q {
        stmt = stmt.bind::<Text, _>(like_contains(term));
    }
    stmt = stmt.bind::<BigInt, _>(limit).bind::<BigInt, _>(offset);

    let rows: Vec<IssueRow> = stmt.get_results(conn).await?;
    Ok(rows.into_iter().map(Issue::from).collect())
}

/// Single-issue lookup. `EnvFilter::All` reads `issues` directly (unchanged);
/// `One`/`Unattributed` reuse [`list_issues`]' derivation (inner-join
/// LATERAL, `HAVING count(*) > 0` for membership) as a single-row query, with
/// no `since`/paging concern — mirrors `get_device`'s precedent. Out-of-scope
/// (issue doesn't exist, or has no occurrence in the selected environment)
/// returns `None` either way, so a caller cannot distinguish "wrong id" from
/// "not in this environment" — the same non-disclosure `get_device`/
/// `get_event_user` chose.
///
/// Task 9: also reuses [`list_issues`]' `title`/`culprit`/`level` derivation
/// — a second `LEFT JOIN LATERAL` (`latest`), `COALESCE`d against `issues`'
/// own columns, reusing the identical `$3` env bind `agg` already consumes.
/// See `list_issues`' doc comment for the full reasoning (legacy-NULL
/// fallback, why `LEFT JOIN` not inner).
pub async fn get_issue(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    issue_id: Uuid,
) -> QueryResult<Option<Issue>> {
    if matches!(scope.env, EnvFilter::All) {
        return issues::table
            .filter(issues::app_id.eq(scope.app_id))
            .filter(issues::id.eq(issue_id))
            .select(Issue::as_select())
            .first(conn)
            .await
            .optional();
    }

    let env_sql = scope.env.sql_fragment_for("e", 3);
    let sql_text = format!(
        "SELECT i.id, i.app_id, i.fingerprint, i.type AS type_, \
                COALESCE(latest.title, i.title)     AS title, \
                COALESCE(latest.culprit, i.culprit) AS culprit, \
                COALESCE(latest.level, i.level)     AS level, \
                i.status, \
                agg.first_seen, agg.last_seen, agg.times_seen, agg.users_seen, \
                i.assignee_id, i.created_at, i.updated_at, i.last_event_at \
         FROM ( \
             SELECT * FROM issues WHERE app_id = $1 AND id = $2 \
         ) i \
         JOIN LATERAL ( \
             SELECT count(*)::bigint AS times_seen, \
                    count(DISTINCT distinct_id)::bigint AS users_seen, \
                    min(occurred_at) AS first_seen, \
                    max(occurred_at) AS last_seen \
             FROM error_events e \
             WHERE e.issue_id = i.id{env_sql} \
             HAVING count(*) > 0 \
         ) agg ON TRUE \
         LEFT JOIN LATERAL ( \
             SELECT e.title, e.culprit, e.level \
             FROM error_events e \
             WHERE e.issue_id = i.id{env_sql} \
             ORDER BY e.occurred_at DESC \
             LIMIT 1 \
         ) latest ON TRUE"
    );
    let mut stmt = diesel::sql_query(sql_text)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<SqlUuid, _>(issue_id);
    stmt = crate::bind_env!(stmt, &scope.env);
    let row: Option<IssueRow> = stmt.get_result(conn).await.optional()?;
    Ok(row.map(Issue::from))
}

pub async fn update_issue_status(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    issue_id: Uuid,
    status: &str,
) -> QueryResult<Option<Issue>> {
    diesel::update(
        issues::table
            .filter(issues::app_id.eq(app_id))
            .filter(issues::id.eq(issue_id)),
    )
    .set((
        issues::status.eq(status.to_string()),
        issues::updated_at.eq(Utc::now()),
    ))
    .returning(Issue::as_returning())
    .get_result(conn)
    .await
    .optional()
}

pub async fn set_issue_users_seen(
    conn: &mut AsyncPgConnection,
    issue_id: Uuid,
    count: i64,
) -> QueryResult<usize> {
    diesel::update(issues::table.find(issue_id))
        .set(issues::users_seen.eq(count))
        .execute(conn)
        .await
}

/// `error_events` carries its own `environment_id` directly, so this is an
/// ordinary `scope_env!` filter — unlike `list_issues`, which has to derive
/// membership because `issues` itself carries none. Also filters on
/// `scope.app_id` as defense in depth: every caller already resolves
/// `issue_id` through `get_issue(scope, ...)` first, so this is redundant in
/// practice, but matches the rest of this slice's idiom of never trusting an
/// id alone to imply tenant scope.
pub async fn list_error_events_for_issue(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    issue_id: Uuid,
    filters: &[ParsedFilter],
    q: Option<&str>,
    since: Option<chrono::DateTime<chrono::Utc>>,
    limit: i64,
) -> QueryResult<Vec<ErrorEvent>> {
    error_events_for_issue_query(&scope, issue_id, filters, q, since)
        .select(ErrorEvent::as_select())
        .order(error_events::occurred_at.desc())
        .limit(limit)
        .load(conn)
        .await
}

/// The shared `WHERE` clause behind both the occurrences list and its
/// [`error_event_stats_for_issue`] counts, returned unselected and unordered so
/// each caller can append its own projection.
///
/// Extracted so the two CANNOT drift. The stat strip above the occurrences
/// table claims to describe the rows below it; if these predicates were written
/// twice, any future filter added to one and forgotten in the other would show
/// a user count that silently disagrees with the visible rows — and the
/// `workflow`/`Neq` arm below is exactly the kind of subtlety a second copy
/// would get wrong.
fn error_events_for_issue_query<'a>(
    scope: &'a ReadScope,
    issue_id: Uuid,
    filters: &[ParsedFilter],
    q: Option<&str>,
    since: Option<chrono::DateTime<chrono::Utc>>,
) -> error_events::BoxedQuery<'a, diesel::pg::Pg> {
    let mut query = error_events::table
        .filter(error_events::app_id.eq(scope.app_id))
        .filter(error_events::issue_id.eq(issue_id))
        .into_boxed();
    if let Some(s) = since {
        query = query.filter(error_events::occurred_at.ge(s));
    }
    for f in filters {
        query = match (f.field, f.op) {
            ("tag", Op::Eq) => {
                let (k, v) = tag_kv(&f.value);
                query
                    .filter(sql::<Bool>("error_events.tags @> ").bind::<Jsonb, _>(tag_object(k, v)))
            }
            ("tag", Op::Contains) => {
                let (k, v) = tag_kv(&f.value);
                query.filter(
                    sql::<Bool>("error_events.tags ->> ")
                        .bind::<Text, _>(k)
                        .sql(" ILIKE ")
                        .bind::<Text, _>(like_contains(&v)),
                )
            }
            // Unlike `list_issues`, `error_events` carries `workflow_name`
            // directly — an ordinary column predicate, no EXISTS needed.
            ("workflow", Op::Eq) => query.filter(error_events::workflow_name.eq(f.value.clone())),
            // `OR workflow_name IS NULL` — **deliberately NOT the plain `.ne()`
            // this file uses for `Neq` on every other nullable column.**
            //
            // SQL's three-valued logic makes a bare `workflow_name <> 'x'`
            // drop every unstamped row, because `NULL <> 'x'` is NULL, not
            // true. For `session_id`/`release`/`distinct_id` in
            // `list_analytics_events` that is tolerable: each is a
            // single-level filter, so whatever it means, it means one thing.
            //
            // `workflow` is not single-level — the same chip is offered on
            // Issues (via `list_issues`' `NOT EXISTS`, where an issue with no
            // stamped occurrence DOES match `neq`) and here on that issue's
            // occurrences. A bare `<>` here would make one chip mean two
            // opposite things at two levels of the same drill-down: filter
            // Issues by `workflow:neq:checkout`, see your unattributed issues,
            // open one, and every unattributed occurrence silently vanishes.
            // A filter key must mean one thing wherever the user types it, so
            // this level bends to match the issue level rather than to match
            // its own file's precedent.
            ("workflow", Op::Neq) => query.filter(
                error_events::workflow_name
                    .ne(f.value.clone())
                    .or(error_events::workflow_name.is_null()),
            ),
            ("workflow", Op::Contains) => {
                query.filter(error_events::workflow_name.ilike(like_contains(&f.value)))
            }
            _ => query,
        };
    }
    if let Some(term) = q {
        let p = like_contains(term);
        query = query.filter(
            error_events::message
                .ilike(p.clone())
                .or(error_events::exception_value.ilike(p.clone()))
                .or(error_events::exception_type.ilike(p.clone()))
                .or(sql::<Bool>("error_events.contexts::text ILIKE ").bind::<Text, _>(p.clone()))
                .or(sql::<Bool>("error_events.extra::text ILIKE ").bind::<Text, _>(p.clone()))
                .or(sql::<Bool>("error_events.tags::text ILIKE ").bind::<Text, _>(p)),
        );
    }
    crate::scope_env!(query, error_events, &scope.env)
}

/// Occurrence totals for one issue over the same predicate the list uses.
///
/// HOT-TIER ONLY. `count(DISTINCT …)` is holistic, not additive, so it cannot
/// be split at the tier watermark and summed the way `tier_read.rs` merges
/// per-day counts — the same reason transaction percentiles stay on Postgres.
/// Once partitions age out to Parquet, a wide range under-reports here exactly
/// as it already does for the per-environment `users_seen` in `list_issues`.
pub async fn error_event_stats_for_issue(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    issue_id: Uuid,
    filters: &[ParsedFilter],
    q: Option<&str>,
    since: Option<chrono::DateTime<chrono::Utc>>,
) -> QueryResult<IssueEventStatsRow> {
    // `distinct_id`/`session_id` are both nullable: an anonymous occurrence and
    // a session-less one contribute a NULL, which `count(DISTINCT …)` skips.
    // That is the intent — "3 users" should not count "no user" as a user.
    let (events, users, sessions) =
        error_events_for_issue_query(&scope, issue_id, filters, q, since)
            .select((
                diesel::dsl::count_star(),
                sql::<BigInt>("count(DISTINCT error_events.distinct_id)"),
                sql::<BigInt>("count(DISTINCT error_events.session_id)"),
            ))
            .get_result::<(i64, i64, i64)>(conn)
            .await?;
    Ok(IssueEventStatsRow {
        events,
        users,
        sessions,
    })
}

/// Counts behind the occurrences stat strip. Raw-shape row, so it lives here
/// beside `IssueStatsRow` rather than in `models.rs`.
#[derive(Debug, serde::Serialize)]
pub struct IssueEventStatsRow {
    pub events: i64,
    pub users: i64,
    pub sessions: i64,
}

/// Reads `error_events` filtered only by `issue_id` — no `app_id: Uuid`
/// parameter for a text-based sweep to catch. Easy to miss for exactly that
/// reason: left unscoped, an issue detail page scoped to one environment
/// renders another environment's stack trace, release string and device
/// context with no error and no marker. `error_events` carries its own
/// `environment_id`, so this is an ordinary `scope_env!` filter, same
/// reasoning as [`list_error_events_for_issue`].
pub async fn latest_error_event(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    issue_id: Uuid,
) -> QueryResult<Option<ErrorEvent>> {
    let query = error_events::table
        .filter(error_events::app_id.eq(scope.app_id))
        .filter(error_events::issue_id.eq(issue_id))
        .into_boxed();
    crate::scope_env!(query, error_events, &scope.env)
        .select(ErrorEvent::as_select())
        .order(error_events::occurred_at.desc())
        .first(conn)
        .await
        .optional()
}

/// `error_events` carries its own `environment_id` directly, so this is an
/// ordinary `scope_env!` filter — unlike `get_event_user`, which has to derive
/// membership because `event_users` itself carries none.
pub async fn error_events_for_person(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    distinct_id: &str,
    limit: i64,
) -> QueryResult<Vec<ErrorEvent>> {
    let q = error_events::table
        .filter(error_events::app_id.eq(scope.app_id))
        .filter(error_events::distinct_id.eq(distinct_id))
        .into_boxed();
    crate::scope_env!(q, error_events, &scope.env)
        .select(ErrorEvent::as_select())
        .order(error_events::occurred_at.desc())
        .limit(limit)
        .load(conn)
        .await
}

// ===========================================================================
// Analytics events & people (app-scoped)
// ===========================================================================

use crate::filter::{Op, ParsedFilter};

/// Escape Postgres ILIKE wildcards (`\`, `%`, `_`) in a user-supplied value so
/// `contains`/free-text search matches it literally, then wrap it in `%…%`.
/// Postgres' default LIKE/ILIKE escape character is `\`.
fn escape_like(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for c in v.chars() {
        if matches!(c, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

pub fn like_contains(v: &str) -> String {
    format!("%{}%", escape_like(v))
}
fn as_i64(v: &str) -> i64 {
    v.parse().unwrap_or_default()
} // parser guarantees numeric

#[cfg(test)]
mod like_contains_tests {
    use super::like_contains;

    #[test]
    fn escapes_percent_wildcard() {
        assert_eq!(like_contains("50%"), "%50\\%%");
    }

    #[test]
    fn escapes_underscore_wildcard() {
        assert_eq!(like_contains("a_b"), "%a\\_b%");
    }

    #[test]
    fn escapes_backslash() {
        assert_eq!(like_contains("a\\b"), "%a\\\\b%");
    }

    #[test]
    fn passes_through_plain_value() {
        assert_eq!(like_contains("hello"), "%hello%");
    }
}

pub async fn insert_analytics_event(
    conn: &mut AsyncPgConnection,
    ev: NewAnalyticsEvent,
) -> QueryResult<usize> {
    diesel::insert_into(analytics_events::table)
        .values(&ev)
        .execute(conn)
        .await
}

pub async fn upsert_event_user(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    distinct_id: &str,
    traits: &Value,
) -> QueryResult<usize> {
    diesel::sql_query(
        "INSERT INTO event_users (app_id, distinct_id, properties) \
         VALUES ($1, $2, $3) \
         ON CONFLICT (app_id, distinct_id) \
         DO UPDATE SET properties = event_users.properties || EXCLUDED.properties, \
                       last_seen = now(), updated_at = now()",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Text, _>(distinct_id)
    .bind::<diesel::sql_types::Jsonb, _>(traits.clone())
    .execute(conn)
    .await
}

pub async fn touch_event_user(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    distinct_id: &str,
) -> QueryResult<usize> {
    diesel::sql_query(
        "INSERT INTO event_users (app_id, distinct_id) VALUES ($1, $2) \
         ON CONFLICT (app_id, distinct_id) DO UPDATE SET last_seen = now(), updated_at = now()",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Text, _>(distinct_id)
    .execute(conn)
    .await
}

/// The three legal values of `event_users.identified_source`, transcribed from
/// migration `2026-08-01-000038`'s CHECK constraint. Adding a fourth means a
/// widening migration, not just a constant.
pub const IDENTIFIED_SOURCE_IDENTIFY: &str = "identify";
pub const IDENTIFIED_SOURCE_CONTEXT_USER: &str = "context_user";
pub const IDENTIFIED_SOURCE_BACKFILL: &str = "backfill";

/// Flag `(app_id, distinct_id)` as naming a real person, first-write-wins.
///
/// A separate statement rather than a column added to `touch_event_user` /
/// `upsert_event_user`, and the separation is load-bearing. RPM upgrades do not
/// re-run `sauron-migrate`, so a new binary can meet an old schema. If the
/// identification column list rode along inside `touch_event_user`, every
/// statement would fail with `undefined_column` and `process_event`'s
/// `let _ = …` would DISCARD the failure — `first_seen`/`last_seen` would
/// silently stop advancing deployment-wide with no dead letter, no metric and
/// no log. `process_identify`'s upsert is `.await?`, so the same missing column
/// would dead-letter every identify() in the window, destroying exactly the
/// `properties` and `identities` rows the 000038 backfill later depends on.
///
/// First-write-wins falls out of the `IS NULL` predicate rather than a
/// `COALESCE`, so after the first hit this is a primary-key no-op. Returning 0
/// is the normal steady state and is never an error.
pub async fn mark_event_user_identified(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    distinct_id: &str,
    source: &str,
) -> QueryResult<usize> {
    diesel::sql_query(
        "UPDATE event_users SET identified_at = now(), identified_source = $3 \
         WHERE app_id = $1 AND distinct_id = $2 AND identified_at IS NULL",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Text, _>(distinct_id)
    .bind::<Text, _>(source)
    .execute(conn)
    .await
}

/// Cheap existence probe for `event_users.identified_at`.
///
/// `LIMIT 0` so it costs a parse and nothing else. Callers run it once at boot
/// and latch the answer: `sauron-ingest` skips identification for the process
/// lifetime after logging one ERROR, and `sauron-api` turns the active-users
/// routes into a `503` that names `sauron-migrate` instead of letting a raw
/// `undefined_column` surface as a 500.
pub async fn probe_event_users_identified(conn: &mut AsyncPgConnection) -> QueryResult<()> {
    diesel::sql_query("SELECT identified_at FROM event_users LIMIT 0")
        .execute(conn)
        .await
        .map(|_| ())
}

pub async fn insert_identity(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    alias_id: &str,
    distinct_id: &str,
) -> QueryResult<usize> {
    diesel::insert_into(identities::table)
        .values((
            identities::app_id.eq(app_id),
            identities::alias_id.eq(alias_id),
            identities::distinct_id.eq(distinct_id),
        ))
        .on_conflict((identities::app_id, identities::alias_id))
        .do_nothing()
        .execute(conn)
        .await
}

/// `event_users` carries no `environment_id`, so membership in a specific
/// environment is derived the same way [`list_persons`]' membership `EXISTS`
/// derives it — activity in `analytics_events`/`error_events`/`sessions`, any
/// one of which is enough. Omitted under `All`, same reasoning as
/// `list_persons`: every `event_users` row exists only because some event
/// registered it, so an unfiltered `EXISTS` would add three subquery lookups
/// for no narrowing effect.
///
/// Returns [`PersonRow`], not the raw [`EventUser`] model — the same move F4
/// made for [`list_persons`]/[`list_devices`]/[`get_device`], and for the
/// identical reason: this is the Person Profile page's single-identity
/// counterpart to `list_persons`' paged rows, and `first_seen`/`last_seen`
/// need a different source depending on `scope.env` (the durable
/// `event_users` columns under `All`, an environment-scoped
/// `LEAST`/`GREATEST` LATERAL under `One`/`Unattributed` — see
/// `list_persons`' doc comment for the full derivation, mirrored here
/// verbatim). `EventUser` has no way to carry two different answers for the
/// same field depending on scope, and raw SQL is what lets a single query
/// switch a selected column's source per branch the way the diesel query
/// builder cannot.
///
/// Before this change the Person Profile page
/// (`bins/sauron-api/src/routes/analytics.rs`'s `PersonProfile`) rendered
/// `EventUser`'s raw, cross-environment, all-time `first_seen`/`last_seen`
/// directly beside an events/errors list that Task 8 already scoped — a
/// person viewed under `One(staging)` would show a production-derived "first
/// seen a year ago" above a list containing one day of staging activity.
/// That is the bug this function exists to not have.
///
/// `events_count`/`errors_count`/`sessions_count` ride along because they're
/// `PersonRow`'s other fields, computed by the same LATERALs regardless of
/// scope (no durable-column fast path, same as `list_persons` — see that
/// function's doc comment). `properties` is read straight off `event_users`
/// unconditionally, same non-derivation and same reasoning as `list_persons`'
/// own `properties` field.
pub async fn get_event_user(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    distinct_id: &str,
) -> QueryResult<Option<PersonRow>> {
    let env_sql = scope.env.sql_fragment(3);

    // See `list_persons`' membership `EXISTS` doc comment: each leg is
    // aliased and the correlated column qualified with that alias
    // (`ae.distinct_id`, not bare `distinct_id`) — an unqualified name
    // colliding with the outer `event_users` row would silently bind to the
    // outer table instead of failing, turning the predicate into a
    // tautology rather than a hard query error.
    let membership_sql = if matches!(scope.env, EnvFilter::All) {
        String::new()
    } else {
        let ae_env = scope.env.sql_fragment_for("ae", 3);
        let ee_env = scope.env.sql_fragment_for("ee", 3);
        let se_env = scope.env.sql_fragment_for("se", 3);
        format!(
            " AND ( \
                EXISTS (SELECT 1 FROM analytics_events ae WHERE ae.app_id=$1 AND ae.distinct_id = event_users.distinct_id{ae_env}) \
                OR EXISTS (SELECT 1 FROM error_events ee WHERE ee.app_id=$1 AND ee.distinct_id = event_users.distinct_id{ee_env}) \
                OR EXISTS (SELECT 1 FROM sessions se WHERE se.app_id=$1 AND se.distinct_id = event_users.distinct_id{se_env}) \
              )"
        )
    };

    // Same `All`-vs-scoped split as `list_persons` for `first_seen`/`last_seen`
    // — see that function's doc comment for the full reasoning, including why
    // `LEAST`/`GREATEST` skipping `NULL` legs is safe given membership already
    // guarantees at least one leg is non-null.
    let seen_select = if matches!(scope.env, EnvFilter::All) {
        "eu.first_seen AS first_seen, eu.last_seen AS last_seen".to_string()
    } else {
        "LEAST(ae.min_occurred, ee.min_occurred, se.min_started) AS first_seen, \
         GREATEST(ae.max_occurred, ee.max_occurred, se.max_last_event) AS last_seen"
            .to_string()
    };

    let q = format!(
        "SELECT eu.distinct_id, eu.properties, {seen_select}, \
                COALESCE(ae.cnt,0)::bigint AS events_count, \
                COALESCE(ee.cnt,0)::bigint AS errors_count, \
                COALESCE(se.cnt,0)::bigint AS sessions_count \
         FROM ( \
             SELECT distinct_id, properties, first_seen, last_seen FROM event_users \
             WHERE app_id=$1 AND distinct_id=$2{membership_sql} \
         ) eu \
         LEFT JOIN LATERAL (SELECT count(*) cnt, min(occurred_at) min_occurred, \
                    max(occurred_at) max_occurred FROM analytics_events \
                    WHERE app_id=$1 AND distinct_id = eu.distinct_id{env_sql}) ae ON TRUE \
         LEFT JOIN LATERAL (SELECT count(*) cnt, min(occurred_at) min_occurred, \
                    max(occurred_at) max_occurred FROM error_events \
                    WHERE app_id=$1 AND distinct_id = eu.distinct_id{env_sql}) ee ON TRUE \
         LEFT JOIN LATERAL (SELECT count(*) cnt, min(started_at) min_started, \
                    max(last_event_at) max_last_event FROM sessions \
                    WHERE app_id=$1 AND distinct_id = eu.distinct_id{env_sql}) se ON TRUE"
    );
    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Text, _>(distinct_id.to_string());
    stmt = crate::bind_env!(stmt, &scope.env);
    stmt.get_result(conn).await.optional()
}

/// `analytics_events` carries its own `environment_id` directly, so this is an
/// ordinary `scope_env!` filter — unlike `get_event_user`, which has to derive
/// membership because `event_users` itself carries none.
pub async fn events_for_person(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    distinct_id: &str,
    limit: i64,
) -> QueryResult<Vec<AnalyticsEvent>> {
    let q = analytics_events::table
        .filter(analytics_events::app_id.eq(scope.app_id))
        .filter(analytics_events::distinct_id.eq(distinct_id))
        .into_boxed();
    crate::scope_env!(q, analytics_events, &scope.env)
        .select(AnalyticsEvent::as_select())
        .order(analytics_events::occurred_at.desc())
        .limit(limit)
        .load(conn)
        .await
}

#[derive(Debug, PartialEq, QueryableByName, serde::Serialize)]
pub struct EventCount {
    #[diesel(sql_type = Text)]
    pub name: String,
    #[diesel(sql_type = BigInt)]
    pub count: i64,
}

pub async fn top_events(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    since: DateTime<Utc>,
    limit: i64,
) -> QueryResult<Vec<EventCount>> {
    // The env fragment takes $3 when it needs a bind; `limit` therefore lands on
    // $4 in that case and $3 otherwise. Deriving both from the same `EnvFilter`
    // is what keeps the string and the bind sequence in agreement — see
    // `EnvFilter::sql_fragment`'s doc for why only `One` consumes an index.
    let env_sql = scope.env.sql_fragment(3);
    let limit_idx = if scope.env.consumes_bind() { 4 } else { 3 };

    let q = format!(
        "SELECT name, count(*)::bigint AS count FROM analytics_events \
         WHERE app_id = $1 AND occurred_at >= $2{env_sql} \
         GROUP BY name ORDER BY count DESC LIMIT ${limit_idx}"
    );
    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Timestamptz, _>(since);
    stmt = crate::bind_env!(stmt, &scope.env);
    stmt.bind::<BigInt, _>(limit).get_results(conn).await
}

#[derive(Debug, QueryableByName, serde::Serialize)]
pub struct SeriesPoint {
    #[diesel(sql_type = Timestamptz)]
    pub bucket: DateTime<Utc>,
    #[diesel(sql_type = BigInt)]
    pub count: i64,
}

pub async fn event_series(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    name: Option<&str>,
    since: DateTime<Utc>,
) -> QueryResult<Vec<SeriesPoint>> {
    match name {
        Some(n) => {
            // $1 app_id, $2 since, $3 name — env takes $4 when it needs a bind.
            let env_sql = scope.env.sql_fragment(4);
            let q = format!(
                "SELECT date_trunc('day', occurred_at) AS bucket, count(*)::bigint AS count \
                 FROM analytics_events \
                 WHERE app_id = $1 AND occurred_at >= $2 AND name = $3{env_sql} \
                 GROUP BY bucket ORDER BY bucket"
            );
            let mut stmt = diesel::sql_query(q)
                .into_boxed()
                .bind::<SqlUuid, _>(scope.app_id)
                .bind::<Timestamptz, _>(since)
                .bind::<Text, _>(n);
            stmt = crate::bind_env!(stmt, &scope.env);
            stmt.get_results(conn).await
        }
        None => {
            // $1 app_id, $2 since — env takes $3 when it needs a bind.
            let env_sql = scope.env.sql_fragment(3);
            let q = format!(
                "SELECT date_trunc('day', occurred_at) AS bucket, count(*)::bigint AS count \
                 FROM analytics_events \
                 WHERE app_id = $1 AND occurred_at >= $2{env_sql} \
                 GROUP BY bucket ORDER BY bucket"
            );
            let mut stmt = diesel::sql_query(q)
                .into_boxed()
                .bind::<SqlUuid, _>(scope.app_id)
                .bind::<Timestamptz, _>(since);
            stmt = crate::bind_env!(stmt, &scope.env);
            stmt.get_results(conn).await
        }
    }
}

/// `error_events` carries its own `environment_id` directly, so this is a
/// plain predicate fragment — no LATERAL/`EXISTS` needed, unlike the
/// `issues`-table-level reads above.
pub async fn issue_occurrence_series(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    issue_id: Uuid,
    since: DateTime<Utc>,
) -> QueryResult<Vec<SeriesPoint>> {
    // $1 issue_id, $2 app_id, $3 since — env takes $4 when it needs a bind.
    let env_sql = scope.env.sql_fragment(4);
    let mut stmt = diesel::sql_query(format!(
        "SELECT date_trunc('day', occurred_at) AS bucket, count(*)::bigint AS count \
         FROM error_events \
         WHERE issue_id = $1 AND app_id = $2 AND occurred_at >= $3{env_sql} \
         GROUP BY bucket ORDER BY bucket"
    ))
    .into_boxed()
    .bind::<SqlUuid, _>(issue_id)
    .bind::<SqlUuid, _>(scope.app_id)
    .bind::<Timestamptz, _>(since);
    stmt = crate::bind_env!(stmt, &scope.env);
    stmt.get_results(conn).await
}

// ===========================================================================
// Sessions & devices (roll-ups upserted by the pipeline)
// ===========================================================================

/// Upsert a session row, folding one signal into it: bump last/first seen and
/// the event/error counters. `context` snapshots the device/os block (only
/// written when non-empty). Idempotent per `(app_id, session_id)`.
///
/// `environment_id` is `COALESCE(EXCLUDED.environment_id,
/// sessions.environment_id)` — the most recent non-null value wins — and
/// `events_count`/`errors_count` accumulate across every environment that
/// touched this session id. The row's own environment label therefore cannot
/// disambiguate its counters. Readers that need per-environment truth derive
/// it from the environment-stamped child tables instead; see
/// `events_for_session`, which says the same thing for the same reason.
///
/// Two readers do NOT do this today: `overview_totals`'s `crashed_sessions`
/// and `session_stats`'s `crashed` both count `errors_count > 0 AND
/// environment_id = $env` directly off this row, trusting the label. Task 10
/// (Slice 3) measured the fix — deriving `crashed` from an `EXISTS` against
/// `error_events` in the selected environment instead — against the largest
/// dev app and declined to ship it: the semi-join cost roughly 11x the
/// column predicate's planning+execution time even with a purpose-built
/// `error_events (app_id, session_id, environment_id)` index, because the
/// cost is a correlated per-session subquery re-probed against every
/// partition of `error_events` (partition pruning cannot help — neither
/// `session_id` nor `environment_id` is the partition key), not a missing
/// index. See
/// `.superpowers/sdd/2026-07-29-environment-rbac-scope/task-10-report.md`
/// for the full measurement. Until a cheaper derivation exists (e.g. a
/// per-session/environment crash flag maintained by this function itself,
/// rather than computed at read time), this is a *mislabelling*, not just an
/// over-count: a session that errored in `env_a` but was last touched by a
/// signal from `env_b` shows as crashed under `One(env_b)` (which never saw
/// the error) and simultaneously invisible — not merely "not crashed" —
/// under `One(env_a)` (which did), because the row's single label can only
/// point at one environment at a time. `errors_count` itself never
/// under-counts (it only grows), but which environment gets credited for it
/// can be wrong in either direction.
#[allow(clippy::too_many_arguments)]
pub async fn bump_session(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    session_id: &str,
    distinct_id: Option<&str>,
    device_key: Option<&str>,
    at: DateTime<Utc>,
    context: &Value,
    release: Option<&str>,
    environment_id: Option<Uuid>,
    ip: Option<&str>,
    events_delta: i64,
    errors_delta: i64,
) -> QueryResult<usize> {
    diesel::sql_query(
        "INSERT INTO sessions \
           (app_id, session_id, distinct_id, device_key, started_at, last_event_at, \
            events_count, errors_count, context, release, environment_id, ip_address) \
         VALUES ($1, $2, $3, $4, $5, $5, $6, $7, $8, $9, $10, $11) \
         ON CONFLICT (app_id, session_id) DO UPDATE SET \
            last_event_at = GREATEST(sessions.last_event_at, EXCLUDED.last_event_at), \
            started_at = LEAST(sessions.started_at, EXCLUDED.started_at), \
            events_count = sessions.events_count + EXCLUDED.events_count, \
            errors_count = sessions.errors_count + EXCLUDED.errors_count, \
            distinct_id = COALESCE(EXCLUDED.distinct_id, sessions.distinct_id), \
            device_key = COALESCE(EXCLUDED.device_key, sessions.device_key), \
            context = CASE WHEN EXCLUDED.context <> '{}'::jsonb THEN EXCLUDED.context ELSE sessions.context END, \
            release = COALESCE(EXCLUDED.release, sessions.release), \
            environment_id = COALESCE(EXCLUDED.environment_id, sessions.environment_id), \
            ip_address = COALESCE(EXCLUDED.ip_address, sessions.ip_address), \
            updated_at = now()",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Text, _>(session_id)
    .bind::<Nullable<Text>, _>(distinct_id)
    .bind::<Nullable<Text>, _>(device_key)
    .bind::<Timestamptz, _>(at)
    .bind::<BigInt, _>(events_delta)
    .bind::<BigInt, _>(errors_delta)
    .bind::<Jsonb, _>(context.clone())
    .bind::<Nullable<Text>, _>(release)
    .bind::<Nullable<SqlUuid>, _>(environment_id)
    .bind::<Nullable<Text>, _>(ip)
    .execute(conn)
    .await
}

/// Upsert a device row, folding one signal into it. Idempotent per
/// `(app_id, device_key)`; descriptor fields only overwrite when non-null.
#[allow(clippy::too_many_arguments)]
pub async fn bump_device(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    device_key: &str,
    family: Option<&str>,
    model: Option<&str>,
    os_name: Option<&str>,
    os_version: Option<&str>,
    arch: Option<&str>,
    browser: Option<&str>,
    distinct_id: Option<&str>,
    at: DateTime<Utc>,
    events_delta: i64,
    errors_delta: i64,
) -> QueryResult<usize> {
    diesel::sql_query(
        "INSERT INTO devices \
           (app_id, device_key, family, model, os_name, os_version, arch, browser, \
            last_distinct_id, first_seen, last_seen, events_count, errors_count) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $10, $11, $12) \
         ON CONFLICT (app_id, device_key) DO UPDATE SET \
            last_seen = GREATEST(devices.last_seen, EXCLUDED.last_seen), \
            first_seen = LEAST(devices.first_seen, EXCLUDED.first_seen), \
            events_count = devices.events_count + EXCLUDED.events_count, \
            errors_count = devices.errors_count + EXCLUDED.errors_count, \
            last_distinct_id = COALESCE(EXCLUDED.last_distinct_id, devices.last_distinct_id), \
            family = COALESCE(EXCLUDED.family, devices.family), \
            model = COALESCE(EXCLUDED.model, devices.model), \
            os_name = COALESCE(EXCLUDED.os_name, devices.os_name), \
            os_version = COALESCE(EXCLUDED.os_version, devices.os_version), \
            arch = COALESCE(EXCLUDED.arch, devices.arch), \
            browser = COALESCE(EXCLUDED.browser, devices.browser), \
            updated_at = now()",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Text, _>(device_key)
    .bind::<Nullable<Text>, _>(family)
    .bind::<Nullable<Text>, _>(model)
    .bind::<Nullable<Text>, _>(os_name)
    .bind::<Nullable<Text>, _>(os_version)
    .bind::<Nullable<Text>, _>(arch)
    .bind::<Nullable<Text>, _>(browser)
    .bind::<Nullable<Text>, _>(distinct_id)
    .bind::<Timestamptz, _>(at)
    .bind::<BigInt, _>(events_delta)
    .bind::<BigInt, _>(errors_delta)
    .execute(conn)
    .await
}

// ===========================================================================
// Workflows (optional, explicitly-bounded spans; roll-up upserted by the
// pipeline, keyed by `(app_id, workflow_id)` rather than `session_id` — see
// migration 2026-07-29-000032_workflows' own doc comment for why)
// ===========================================================================

/// A workflow still `active` with no activity for this long is reported as
/// abandoned (derived on read, never stored — there is no fourth `status`
/// value and no sweeper job). Matches the breadcrumb-buffer TTL. Declared
/// once, here, so no second copy of the threshold can drift from it.
pub const WORKFLOW_STALE_MINUTES: i64 = 30;

/// The three reserved lifecycle transitions a `$workflow_start` /
/// `$workflow_end` / `$workflow_cancel` event drives via
/// [`apply_workflow_lifecycle`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowAction {
    Start,
    End,
    Cancel,
}

/// Upsert a `workflows` row, folding one stamped signal into it: bump
/// last-activity/earliest-start and the event/error counters. Idempotent per
/// `(app_id, workflow_id)` — mirrors [`bump_session`] with `session_id`
/// swapped for `workflow_id`; read that function's doc comment first.
///
/// Deliberately never touches `status`, `ended_at` or `cancel_reason`: those
/// three columns are [`apply_workflow_lifecycle`]'s alone to set, which is
/// what protects a terminal (`completed`/`cancelled`) workflow from being
/// reopened by a late-arriving stamped event that has nothing to do with the
/// lifecycle itself (e.g. an ordinary analytics event stamped with a
/// `workflow_id` whose `$workflow_end` was already processed).
///
/// ## `environment_id` is the FIRST writer's, and is NOT authoritative
///
/// The unique key is `(app_id, workflow_id)` — **app-wide**. `environment_id`
/// is in the INSERT column list but in no `DO UPDATE SET` clause, so whichever
/// signal creates the row labels it forever, while `events_count`/
/// `errors_count` accumulate from every environment that touches that
/// `workflow_id`. A workflow can therefore be *labelled* one environment while
/// its counters were incremented by signals from another — the same hazard
/// [`bump_session`]'s own doc comment describes for sessions, arrived at by a
/// different route (it lets the label flip to the latest non-null value; this
/// pins it to the first).
///
/// **Read-side scoping filters on `workflows.environment_id` directly** —
/// see `workflow_list`/`workflow_detail`/`workflow_runs`/
/// `workflow_spans_for_session` below, which splice `ReadScope`'s SQL
/// fragment against this table exactly as they would against any other
/// environment-stamped one. An earlier version of this comment said the
/// opposite — "a read-side caller must not treat this as an environment
/// filter... derive environment from the environment-stamped child rows
/// instead". That instruction was overstated and is superseded by this
/// paragraph (see
/// `.superpowers/sdd/2026-07-29-workflow-grouping/task-4-report.md` for the
/// ruling).
///
/// Why the column is trustworthy enough to filter on directly, despite being
/// the first writer's rather than the most recent: `workflow_id` is a
/// **client-generated UUID** (see the design doc), so a workflow row belongs
/// to exactly one app+environment in practice — cross-environment
/// mislabelling requires a client to violate that contract by reusing an id.
/// That is a narrower exposure than `sessions.environment_id`'s identical
/// trade-off, where every reader already trusts the column even though a
/// real session can legitimately span environments (this one, keyed by a
/// UUID, is not expected to).
///
/// The alternative — deriving environment from the stamped child rows via an
/// `EXISTS` semi-join instead of trusting this column — **has not been
/// measured against this table.** What *was* measured is the analogous
/// semi-join on `sessions`: Slice 3's Task 10 replaced `sessions.crashed`'s
/// column predicate with an `EXISTS` against `error_events` and found it
/// ~11x more expensive (11.3x / 11.0x on the two shapes it timed), even with
/// a purpose-built index, because it becomes a correlated per-row subquery
/// re-probed against every partition of `error_events` rather than a
/// missing-index problem. See
/// `.superpowers/sdd/2026-07-29-environment-rbac-scope/task-10-report.md` —
/// note that it predates this table and mentions workflows nowhere. The same
/// shape and therefore a similar cost is *expected* here (same child tables,
/// same partitioning, same correlated-subquery structure), but that is an
/// inference from the `sessions` result, not a workflows measurement. Anyone
/// designing a stricter scoping fix should re-measure against `workflows`
/// rather than treat ~11x as an established number for this table.
///
/// None of this makes the label infallible, and it is still not "fixed" with
/// a `COALESCE` refresh on every `bump_workflow` call: that would make the
/// label flip-flop between environments on every signal, which is strictly
/// worse than a stable-but-occasionally-wrong one. The residual risk — a
/// hand-rolled client reusing one `workflow_id` across two environments — is
/// accepted rather than paid for with a materially more expensive read on
/// every workflow query.
///
/// ## `COALESCE` argument order is inverted vs. [`bump_session`], on purpose
///
/// `session_id`/`distinct_id`/`device_key`/`release`/`name` here are
/// `COALESCE(workflows.<col>, EXCLUDED.<col>)` — **first** non-null wins —
/// whereas `bump_session` writes `COALESCE(EXCLUDED.<col>, sessions.<col>)`,
/// i.e. **last** non-null wins. Not a typo in either place. A session is a
/// long-lived, re-identified thing whose newest attribution is its best one
/// (a user logging in mid-session should re-label it); an explicitly-bounded
/// workflow's owning session/device/release is a property of where it *began*,
/// and letting a later straggler signal repoint it would rewrite history the
/// UI already showed. `name` follows the same rule via `NULLIF`, treating `''`
/// as absent — see [`apply_workflow_lifecycle`] for the clobber that guards
/// against.
#[allow(clippy::too_many_arguments)]
pub async fn bump_workflow(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    environment_id: Uuid,
    workflow_id: &str,
    workflow_name: &str,
    session_id: Option<&str>,
    distinct_id: Option<&str>,
    device_key: Option<&str>,
    release: Option<&str>,
    occurred_at: DateTime<Utc>,
    events_delta: i32,
    errors_delta: i32,
) -> QueryResult<usize> {
    diesel::sql_query(
        "INSERT INTO workflows \
           (app_id, environment_id, workflow_id, name, session_id, distinct_id, \
            device_key, release, started_at, last_event_at, events_count, errors_count) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $9, $10, $11) \
         ON CONFLICT (app_id, workflow_id) DO UPDATE SET \
            last_event_at = GREATEST(workflows.last_event_at, EXCLUDED.last_event_at), \
            started_at    = LEAST(workflows.started_at, EXCLUDED.started_at), \
            events_count  = workflows.events_count + EXCLUDED.events_count, \
            errors_count  = workflows.errors_count + EXCLUDED.errors_count, \
            name          = COALESCE(NULLIF(workflows.name, ''), EXCLUDED.name), \
            session_id    = COALESCE(workflows.session_id, EXCLUDED.session_id), \
            distinct_id   = COALESCE(workflows.distinct_id, EXCLUDED.distinct_id), \
            device_key    = COALESCE(workflows.device_key, EXCLUDED.device_key), \
            release       = COALESCE(workflows.release, EXCLUDED.release), \
            updated_at    = now()",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<SqlUuid, _>(environment_id)
    .bind::<Text, _>(workflow_id)
    .bind::<Text, _>(workflow_name)
    .bind::<Nullable<Text>, _>(session_id)
    .bind::<Nullable<Text>, _>(distinct_id)
    .bind::<Nullable<Text>, _>(device_key)
    .bind::<Nullable<Text>, _>(release)
    .bind::<Timestamptz, _>(occurred_at)
    .bind::<Integer, _>(events_delta)
    .bind::<Integer, _>(errors_delta)
    .execute(conn)
    .await
}

/// Apply a `$workflow_start` / `$workflow_end` / `$workflow_cancel` lifecycle
/// event to its `workflows` row, upserting it if this is the first signal
/// seen for the workflow.
///
/// `Start` deliberately does **not** set `status = 'active'` on conflict —
/// only backfilling `name`/`started_at` — so a start event that arrives
/// *after* the end/cancel event (out-of-order delivery) cannot reopen an
/// already-terminal workflow. `End`/`Cancel` guard every field they touch
/// (`status`, `ended_at`, `cancel_reason`) with `CASE WHEN workflows.status =
/// 'active'`: the first terminal transition wins and a second one — of
/// either kind — is silently ignored, matching `bump_workflow`'s own
/// never-reopen guarantee.
///
/// `Start`'s `name` clause is `COALESCE(NULLIF(EXCLUDED.name, ''),
/// workflows.name)`, not a bare `EXCLUDED.name`. `workflows.name` is `TEXT NOT
/// NULL` with no emptiness CHECK, and the caller resolves an absent name to
/// `""` — so a bare assignment would let the exact client this function's
/// property-fallback exists for (a hand-rolled one posting `$workflow_start`
/// with a `workflow_id` but no name) permanently destroy a good display name
/// that stamped SDK events had already established. `NULLIF` makes `''` mean
/// "I have nothing to offer", which is what it actually means here.
///
/// The caller is deliberately NOT made to skip the lifecycle call when no name
/// resolves: dropping the transition would strand the workflow `active`
/// forever and it would then be misreported as abandoned once
/// [`WORKFLOW_STALE_MINUTES`] elapsed. A missing name is cosmetic; a missing
/// terminal transition is not. The row can still be *created* with `name = ''`
/// when a nameless lifecycle event is the very first signal for a workflow —
/// there is genuinely nothing better to store at that point — and
/// [`bump_workflow`]'s own `COALESCE(NULLIF(workflows.name, ''), …)` upgrades
/// it as soon as any named signal arrives.
#[allow(clippy::too_many_arguments)]
pub async fn apply_workflow_lifecycle(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    environment_id: Uuid,
    workflow_id: &str,
    workflow_name: &str,
    action: WorkflowAction,
    cancel_reason: Option<&str>,
    session_id: Option<&str>,
    distinct_id: Option<&str>,
    occurred_at: DateTime<Utc>,
) -> QueryResult<usize> {
    match action {
        WorkflowAction::Start => {
            diesel::sql_query(
                "INSERT INTO workflows \
                   (app_id, environment_id, workflow_id, name, session_id, distinct_id, \
                    status, started_at, last_event_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, 'active', $7, $7) \
                 ON CONFLICT (app_id, workflow_id) DO UPDATE SET \
                    name       = COALESCE(NULLIF(EXCLUDED.name, ''), workflows.name), \
                    started_at = LEAST(workflows.started_at, EXCLUDED.started_at), \
                    updated_at = now()",
            )
            .bind::<SqlUuid, _>(app_id)
            .bind::<SqlUuid, _>(environment_id)
            .bind::<Text, _>(workflow_id)
            .bind::<Text, _>(workflow_name)
            .bind::<Nullable<Text>, _>(session_id)
            .bind::<Nullable<Text>, _>(distinct_id)
            .bind::<Timestamptz, _>(occurred_at)
            .execute(conn)
            .await
        }
        WorkflowAction::End | WorkflowAction::Cancel => {
            let status = if action == WorkflowAction::End {
                "completed"
            } else {
                "cancelled"
            };
            diesel::sql_query(
                "INSERT INTO workflows \
                   (app_id, environment_id, workflow_id, name, session_id, distinct_id, \
                    status, cancel_reason, started_at, ended_at, last_event_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $9, $9) \
                 ON CONFLICT (app_id, workflow_id) DO UPDATE SET \
                    status        = CASE WHEN workflows.status = 'active' THEN EXCLUDED.status ELSE workflows.status END, \
                    ended_at      = CASE WHEN workflows.status = 'active' THEN EXCLUDED.ended_at ELSE workflows.ended_at END, \
                    cancel_reason = CASE WHEN workflows.status = 'active' THEN EXCLUDED.cancel_reason ELSE workflows.cancel_reason END, \
                    last_event_at = GREATEST(workflows.last_event_at, EXCLUDED.last_event_at), \
                    started_at    = LEAST(workflows.started_at, EXCLUDED.started_at), \
                    updated_at    = now()",
            )
            .bind::<SqlUuid, _>(app_id)
            .bind::<SqlUuid, _>(environment_id)
            .bind::<Text, _>(workflow_id)
            .bind::<Text, _>(workflow_name)
            .bind::<Nullable<Text>, _>(session_id)
            .bind::<Nullable<Text>, _>(distinct_id)
            .bind::<Text, _>(status)
            .bind::<Nullable<Text>, _>(cancel_reason)
            .bind::<Timestamptz, _>(occurred_at)
            .execute(conn)
            .await
        }
    }
}

// ---------------------------------------------------------------------------
// Workflows (read side, Task 4): on-read aggregation over `workflows` plus
// the environment-stamped child tables it names in its own doc comment
// (`analytics_events`/`error_events`), following the `screen_list`/
// `screen_stats` template — raw `sql_query`, `ReadScope::sql_fragment(_for)`
// spliced into the WHERE clause, positional binds in matching order,
// `QueryableByName` result structs. `workflows.environment_id` is filtered on
// directly (see `bump_workflow`'s doc comment above for why that is now the
// correct call, not an oversight).
// ---------------------------------------------------------------------------

/// The `eff` (effective status) projection every workflow read function
/// shares: a workflow's own `status`, unless it is still `active` and has had
/// no activity for `WORKFLOW_STALE_MINUTES`, in which case it reads as
/// `'abandoned'` — derived on read, never stored (see `WORKFLOW_STALE_MINUTES`'s
/// own doc comment). A function rather than a `const &str`, because the
/// threshold has to be spliced in via `format!`; safe to do so —
/// `WORKFLOW_STALE_MINUTES` is a compile-time integer constant, never user
/// input. Defined once so `workflow_list`/`workflow_detail`/`workflow_runs`/
/// `workflow_spans_for_session` cannot drift from one another or from the
/// constant itself. Assumes the `workflows` row is aliased `w`, which every
/// call site below does.
fn workflow_effective_status_sql() -> String {
    format!(
        "CASE WHEN w.status = 'active' AND w.last_event_at < now() - make_interval(mins => {WORKFLOW_STALE_MINUTES}) \
         THEN 'abandoned' ELSE w.status END"
    )
}

/// Shared inner subquery for the workflow outcome/duration aggregate, reused
/// by `workflow_list` (grouped by name, an outer `ILIKE` narrows which names
/// survive) and `workflow_detail` (narrowed to one name *inside* this
/// subquery, so the outer aggregate collapses to a single row) — the same
/// "shared CTE, different predicate" idiom `screen_ctes` uses for
/// `screen_list`/`screen_stats`.
///
/// `name_pred` is a compile-time SQL fragment, never user data: `""` for the
/// list (name filtering happens outside, against the derived table's own
/// `w.name`) or `" AND w.name = $3"` for the detail view (a *bound* param at
/// a fixed index, never interpolated). `env_sql` is an
/// `EnvFilter::sql_fragment_for("w", _)` output, and MUST be applied inside
/// this subquery rather than the outer query — same reasoning as
/// `screen_ctes`'s own doc comment: the outer query only sees the columns
/// this subquery selects, and even if `environment_id` were visible there,
/// filtering after the aggregate `eff`/`dur` projection would still compute
/// them from every environment's rows first.
fn workflow_outcome_subquery(env_sql: &str, name_pred: &str) -> String {
    let eff = workflow_effective_status_sql();
    format!(
        "SELECT w.*, {eff} AS eff, \
                CASE WHEN w.ended_at IS NOT NULL \
                     THEN EXTRACT(EPOCH FROM (w.ended_at - w.started_at)) * 1000 END AS dur \
         FROM workflows w \
         WHERE w.app_id = $1 AND w.started_at >= now() - make_interval(days => $2){env_sql}{name_pred}"
    )
}

/// One row per workflow **name** — the outcome/duration aggregate
/// `workflow_list` and `workflow_detail` both produce (`workflow_detail`
/// reuses this exact shape for its own single-name aggregate before folding
/// in the other three pieces).
#[derive(Debug, QueryableByName, serde::Serialize)]
pub struct WorkflowRow {
    #[diesel(sql_type = Text)]
    pub name: String,
    #[diesel(sql_type = BigInt)]
    pub started: i64,
    #[diesel(sql_type = BigInt)]
    pub completed: i64,
    #[diesel(sql_type = BigInt)]
    pub cancelled: i64,
    #[diesel(sql_type = BigInt)]
    pub abandoned: i64,
    #[diesel(sql_type = BigInt)]
    pub active: i64,
    #[diesel(sql_type = BigInt)]
    pub unique_users: i64,
    /// `NULL` when no run in the window has an `ended_at` yet — only
    /// `completed`/`cancelled` rows have one, so an all-`active`/`abandoned`
    /// group has no duration to report. That is the intended semantic:
    /// duration describes finished runs.
    #[diesel(sql_type = Nullable<Double>)]
    pub median_duration_ms: Option<f64>,
    #[diesel(sql_type = Nullable<Double>)]
    pub p95_duration_ms: Option<f64>,
    #[diesel(sql_type = Timestamptz)]
    pub last_seen: DateTime<Utc>,
}

/// The SELECT list shared by `workflow_list`'s and `workflow_detail`'s
/// outcome/duration aggregate — factored out so the two cannot drift on a
/// column, an alias, or a FILTER predicate. Operates over
/// `workflow_outcome_subquery`'s derived table, aliased `w`.
const WORKFLOW_OUTCOME_SELECT: &str = "\
    w.name, \
    COUNT(*)::bigint AS started, \
    COUNT(*) FILTER (WHERE w.eff = 'completed')::bigint AS completed, \
    COUNT(*) FILTER (WHERE w.eff = 'cancelled')::bigint AS cancelled, \
    COUNT(*) FILTER (WHERE w.eff = 'abandoned')::bigint AS abandoned, \
    COUNT(*) FILTER (WHERE w.eff = 'active')::bigint AS active, \
    COUNT(DISTINCT w.distinct_id)::bigint AS unique_users, \
    percentile_cont(0.5) WITHIN GROUP (ORDER BY w.dur)::double precision AS median_duration_ms, \
    percentile_cont(0.95) WITHIN GROUP (ORDER BY w.dur)::double precision AS p95_duration_ms, \
    MAX(w.last_event_at) AS last_seen";

/// One row per workflow name: started/completed/cancelled/abandoned/active
/// counts, unique users, median/p95 duration (finished runs only) and last
/// seen — paginated, optionally substring-filtered by name.
///
/// Bind layout: `$1` app_id, `$2` since_days — env takes `$3` when it needs a
/// bind (reserved against the inner subquery's `w` alias). `search` always
/// binds (`Nullable<Text>`) at the next free index — `$4` if env consumed
/// `$3`, else `$3` — and `limit`/`offset` trail it. Same "trailing-index
/// shift" idiom as `screen_list`/`top_events`: every index downstream of the
/// env fragment is computed *from* `scope.env.consumes_bind()`, not
/// hard-coded, so the two can never drift apart.
///
/// `search` is matched with [`like_contains`], so a term containing `%` or
/// `_` matches those characters *literally* rather than as wildcards —
/// matching every other free-text search in this file. The `%…%` wrapping
/// happens in Rust, not via SQL-side `||` concatenation, which is what lets
/// the escaping apply at all.
pub async fn workflow_list(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    since_days: i32,
    search: Option<&str>,
    limit: i64,
    offset: i64,
) -> QueryResult<Vec<WorkflowRow>> {
    let env_sql = scope.env.sql_fragment_for("w", 3);
    let search_idx = if scope.env.consumes_bind() { 4 } else { 3 };
    let limit_idx = search_idx + 1;
    let offset_idx = limit_idx + 1;
    let search_pattern = search.map(like_contains);

    let q = format!(
        "SELECT {WORKFLOW_OUTCOME_SELECT} \
         FROM ({}) w \
         WHERE (${search_idx}::text IS NULL OR w.name ILIKE ${search_idx}) \
         GROUP BY w.name \
         ORDER BY started DESC, w.name ASC \
         LIMIT ${limit_idx} OFFSET ${offset_idx}",
        workflow_outcome_subquery(&env_sql, "")
    );

    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Integer, _>(since_days);
    stmt = crate::bind_env!(stmt, &scope.env);
    stmt.bind::<Nullable<Text>, _>(search_pattern)
        .bind::<BigInt, _>(limit)
        .bind::<BigInt, _>(offset)
        .get_results(conn)
        .await
}

/// One contained event name and its count within a workflow — `top_events`'
/// row shape.
#[derive(Debug, QueryableByName, serde::Serialize)]
pub struct NameCount {
    #[diesel(sql_type = Text)]
    pub name: String,
    #[diesel(sql_type = BigInt)]
    pub count: i64,
}

/// One contained issue and its occurrence count within a workflow —
/// `top_issues`' row shape.
#[derive(Debug, QueryableByName, serde::Serialize)]
pub struct WorkflowIssue {
    #[diesel(sql_type = SqlUuid)]
    pub issue_id: Uuid,
    #[diesel(sql_type = Text)]
    pub title: String,
    #[diesel(sql_type = BigInt)]
    pub count: i64,
}

/// A single workflow name's full detail: the same outcome/duration aggregate
/// as `workflow_list` (one name's worth), a duration histogram, the top
/// contained (non-lifecycle) event names, and the top contained issues.
#[derive(Debug, serde::Serialize)]
pub struct WorkflowDetail {
    pub name: String,
    pub started: i64,
    pub completed: i64,
    pub cancelled: i64,
    pub abandoned: i64,
    pub active: i64,
    pub unique_users: i64,
    pub median_duration_ms: Option<f64>,
    pub p95_duration_ms: Option<f64>,
    pub duration_buckets: Vec<HistoBucket>,
    pub top_events: Vec<NameCount>,
    pub top_issues: Vec<WorkflowIssue>,
}

/// The three reserved lifecycle event names' shared prefix — `NOT LIKE
/// '$workflow%'` in `workflow_detail`'s `top_events` query excludes all three
/// (`$workflow_start`/`_end`/`_cancel`), which would otherwise dominate every
/// contained-event list. A compile-time literal, not user data, so it is
/// written directly into the SQL text rather than bound.
const WORKFLOW_LIFECYCLE_EVENT_PATTERN: &str = "$workflow%";

/// Full detail for one workflow name: outcome/duration aggregate, duration
/// histogram, top contained events, top contained issues.
///
/// Every one of the four queries below binds in the same order — `$1`
/// app_id, `$2` since_days, `$3` name, `$4` env (only when
/// `scope.env.consumes_bind()`) — a deliberate uniformity (none of them
/// *needs* to share a layout, since each is an independent prepared
/// statement) that exists purely to make this function's own bind-index
/// bookkeeping easy to verify by inspection.
///
/// Returns `Err(NotFound)` if `name` never had a matching `workflows` row in
/// `scope`'s environment(s) within `since_days` — the outcome aggregate's
/// `GROUP BY w.name` yields zero rows in that case, same "vanishes rather
/// than zero-fills" behaviour `screen_stats` has for a screen never seen.
///
/// ## The four queries use two *different* time windows, on purpose
///
/// The outcome aggregate and the duration histogram bound
/// `workflows.started_at`; `top_events` and `top_issues` bound the child
/// tables' `occurred_at`. These do not describe the same set of runs, and
/// the totals are not expected to reconcile: a run that *started* just
/// outside the window but emitted events inside it contributes to
/// `top_events`/`top_issues` while contributing nothing to
/// `started`/`completed`/the histogram. The reverse also happens — a run
/// that started inside the window but whose events all predate it.
///
/// This is deliberate rather than an oversight. Bounding the child tables by
/// their owning workflow's `started_at` instead would require joining
/// `workflows` into both queries, which is exactly the correlated-subquery
/// shape `bump_workflow`'s doc comment records being measured at ~11x on the
/// analogous `sessions` case — a large cost to make two independently
/// meaningful numbers agree at the edges. "Events seen inside this window
/// that belonged to this workflow" is a defensible reading in its own right.
/// Dashboard note: do not present `sum(top_events.count)` as "events in the
/// runs counted above" — it is not that number.
pub async fn workflow_detail(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    name: &str,
    since_days: i32,
) -> QueryResult<WorkflowDetail> {
    const NAME_PRED: &str = " AND w.name = $3";

    // --- outcome/duration aggregate (one row) ------------------------------
    let env_sql = scope.env.sql_fragment_for("w", 4);
    let outcome_q = format!(
        "SELECT {WORKFLOW_OUTCOME_SELECT} FROM ({}) w GROUP BY w.name",
        workflow_outcome_subquery(&env_sql, NAME_PRED)
    );
    let mut outcome_stmt = diesel::sql_query(outcome_q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Integer, _>(since_days)
        .bind::<Text, _>(name);
    outcome_stmt = crate::bind_env!(outcome_stmt, &scope.env);
    let outcome: WorkflowRow = outcome_stmt.get_result(conn).await?;

    // --- duration histogram (finished runs only) ---------------------------
    // Reuses the whole existing duration-histogram scheme —
    // `DURATION_BUCKET_CASE_SQL` (the bucketing SQL itself),
    // `DURATION_BUCKETS`, `order_histogram` and `HistoBucket` — rather than a
    // fresh `width_bucket` scheme or a second copy of the CASE.
    let env_sql = scope.env.sql_fragment_for("w", 4);
    let buckets_q = format!(
        "SELECT bucket, count(*)::bigint AS count FROM ( \
           SELECT {DURATION_BUCKET_CASE_SQL} AS bucket \
           FROM (SELECT EXTRACT(EPOCH FROM (w.ended_at - w.started_at)) * 1000 AS d \
                 FROM workflows w \
                 WHERE w.app_id = $1 AND w.started_at >= now() - make_interval(days => $2) \
                   AND w.name = $3 AND w.ended_at IS NOT NULL{env_sql}) s \
         ) b GROUP BY bucket"
    );
    let mut buckets_stmt = diesel::sql_query(buckets_q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Integer, _>(since_days)
        .bind::<Text, _>(name);
    buckets_stmt = crate::bind_env!(buckets_stmt, &scope.env);
    let bucket_rows: Vec<HistoBucket> = buckets_stmt.get_results(conn).await?;
    let duration_buckets = order_histogram(bucket_rows);

    // --- top contained events -----------------------------------------------
    // `analytics_events` is the only table in this FROM clause, so the env
    // fragment is unqualified — same as `top_events` above.
    //
    // `workflow_id IS NOT NULL` is load-bearing for performance, not
    // semantics: migration 2026-07-29-000032's index on this table is
    // PARTIAL (`... WHERE workflow_id IS NOT NULL`), and Postgres only uses a
    // partial index when the query's WHERE clause *implies* the index
    // predicate. `workflow_name = $3` does not imply it — they are different
    // columns — so without this term the index is unusable and the planner
    // falls back to scanning every partition of the largest table in the
    // system, filtering `workflow_name` as a post-scan qual. Measured on the
    // dev app (212k events, 22 partitions): 52,744 buffers / cost 56,190
    // without it, 14 buffers / cost 2,025 with it. Semantically a no-op — the
    // pipeline stamps `workflow_id` and `workflow_name` together, so a row
    // with a name always has an id. See task-4-report.md's "Fix round 1" for
    // both full plans.
    let env_sql = scope.env.sql_fragment(4);
    let events_q = format!(
        "SELECT name, COUNT(*)::bigint AS count \
         FROM analytics_events \
         WHERE app_id = $1 AND occurred_at >= now() - make_interval(days => $2) \
           AND workflow_name = $3 AND workflow_id IS NOT NULL \
           AND name NOT LIKE '{WORKFLOW_LIFECYCLE_EVENT_PATTERN}'{env_sql} \
         GROUP BY name ORDER BY count DESC LIMIT 10"
    );
    let mut events_stmt = diesel::sql_query(events_q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Integer, _>(since_days)
        .bind::<Text, _>(name);
    events_stmt = crate::bind_env!(events_stmt, &scope.env);
    let top_events: Vec<NameCount> = events_stmt.get_results(conn).await?;

    // --- top contained issues -----------------------------------------------
    // `error_events` (aliased `e`) joined to `issues` (aliased `i`) — env
    // must qualify `e`, the environment-stamped side of the join.
    //
    // `e.workflow_id IS NOT NULL` is here for the same partial-index reason
    // as the `top_events` query above — `error_events` carries the identical
    // partial index. Measured on the dev app (210k error events): 52,559
    // buffers / cost 56,267 without it, 14 buffers / cost 2,043 with it.
    let env_sql = scope.env.sql_fragment_for("e", 4);
    let issues_q = format!(
        "SELECT i.id AS issue_id, i.title, COUNT(*)::bigint AS count \
         FROM error_events e \
         JOIN issues i ON i.id = e.issue_id \
         WHERE e.app_id = $1 AND e.occurred_at >= now() - make_interval(days => $2) \
           AND e.workflow_name = $3 AND e.workflow_id IS NOT NULL{env_sql} \
         GROUP BY i.id, i.title \
         ORDER BY count DESC LIMIT 10"
    );
    let mut issues_stmt = diesel::sql_query(issues_q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Integer, _>(since_days)
        .bind::<Text, _>(name);
    issues_stmt = crate::bind_env!(issues_stmt, &scope.env);
    let top_issues: Vec<WorkflowIssue> = issues_stmt.get_results(conn).await?;

    Ok(WorkflowDetail {
        name: outcome.name,
        started: outcome.started,
        completed: outcome.completed,
        cancelled: outcome.cancelled,
        abandoned: outcome.abandoned,
        active: outcome.active,
        unique_users: outcome.unique_users,
        median_duration_ms: outcome.median_duration_ms,
        p95_duration_ms: outcome.p95_duration_ms,
        duration_buckets,
        top_events,
        top_issues,
    })
}

/// One individual workflow run — `workflow_runs`' row shape, linking to
/// session detail via `session_id`.
#[derive(Debug, QueryableByName, serde::Serialize)]
pub struct WorkflowRun {
    #[diesel(sql_type = Text)]
    pub workflow_id: String,
    #[diesel(sql_type = Nullable<Text>)]
    pub session_id: Option<String>,
    #[diesel(sql_type = Nullable<Text>)]
    pub distinct_id: Option<String>,
    /// The effective status (see `workflow_effective_status_sql`), not the
    /// raw column — an `active` row past `WORKFLOW_STALE_MINUTES` reports as
    /// `abandoned` here too.
    #[diesel(sql_type = Text)]
    pub status: String,
    #[diesel(sql_type = Timestamptz)]
    pub started_at: DateTime<Utc>,
    #[diesel(sql_type = Nullable<Timestamptz>)]
    pub ended_at: Option<DateTime<Utc>>,
    #[diesel(sql_type = Nullable<BigInt>)]
    pub duration_ms: Option<i64>,
    #[diesel(sql_type = Integer)]
    pub events_count: i32,
    #[diesel(sql_type = Integer)]
    pub errors_count: i32,
}

/// Individual runs of one workflow name, newest first, optionally filtered by
/// effective status (`active`/`completed`/`cancelled`/`abandoned` — compared
/// against the *projection*, not the raw `status` column, which is what makes
/// `abandoned` a filterable value at all, since it never appears as a stored
/// value).
///
/// Bind layout: `$1` app_id, `$2` since_days, `$3` name — env takes `$4` when
/// it needs a bind. `status` always binds (`Nullable<Text>`) at the next free
/// index — `$5` if env consumed `$4`, else `$4` — and `limit`/`offset` trail
/// it, same trailing-index-shift idiom as `workflow_list`.
pub async fn workflow_runs(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    name: &str,
    since_days: i32,
    status: Option<&str>,
    limit: i64,
    offset: i64,
) -> QueryResult<Vec<WorkflowRun>> {
    let env_sql = scope.env.sql_fragment_for("w", 4);
    let status_idx = if scope.env.consumes_bind() { 5 } else { 4 };
    let limit_idx = status_idx + 1;
    let offset_idx = limit_idx + 1;
    let eff = workflow_effective_status_sql();

    let q = format!(
        "SELECT w.workflow_id, w.session_id, w.distinct_id, {eff} AS status, \
                w.started_at, w.ended_at, \
                CASE WHEN w.ended_at IS NOT NULL \
                     THEN (EXTRACT(EPOCH FROM (w.ended_at - w.started_at)) * 1000)::bigint END AS duration_ms, \
                w.events_count, w.errors_count \
         FROM workflows w \
         WHERE w.app_id = $1 AND w.started_at >= now() - make_interval(days => $2) \
           AND w.name = $3{env_sql} \
           AND (${status_idx}::text IS NULL OR {eff} = ${status_idx}) \
         ORDER BY w.started_at DESC \
         LIMIT ${limit_idx} OFFSET ${offset_idx}"
    );

    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Integer, _>(since_days)
        .bind::<Text, _>(name);
    stmt = crate::bind_env!(stmt, &scope.env);
    stmt.bind::<Nullable<Text>, _>(status)
        .bind::<BigInt, _>(limit)
        .bind::<BigInt, _>(offset)
        .get_results(conn)
        .await
}

/// One workflow span within a session — for the session timeline lane.
#[derive(Debug, QueryableByName, serde::Serialize)]
pub struct WorkflowSpan {
    #[diesel(sql_type = Text)]
    pub workflow_id: String,
    #[diesel(sql_type = Text)]
    pub name: String,
    #[diesel(sql_type = Text)]
    pub status: String,
    #[diesel(sql_type = Timestamptz)]
    pub started_at: DateTime<Utc>,
    #[diesel(sql_type = Nullable<Timestamptz>)]
    pub ended_at: Option<DateTime<Utc>>,
}

/// Every workflow span within one session, oldest first — feeds the session
/// timeline lane.
///
/// Bind layout: `$1` app_id, `$2` session_id — env takes `$3` when it needs a
/// bind.
pub async fn workflow_spans_for_session(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    session_id: &str,
) -> QueryResult<Vec<WorkflowSpan>> {
    let env_sql = scope.env.sql_fragment_for("w", 3);
    let eff = workflow_effective_status_sql();
    let q = format!(
        "SELECT w.workflow_id, w.name, {eff} AS status, w.started_at, w.ended_at \
         FROM workflows w \
         WHERE w.app_id = $1 AND w.session_id = $2{env_sql} \
         ORDER BY w.started_at ASC"
    );
    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Text, _>(session_id);
    stmt = crate::bind_env!(stmt, &scope.env);
    stmt.get_results(conn).await
}

pub async fn insert_transaction(
    conn: &mut AsyncPgConnection,
    tx: NewTransaction,
) -> QueryResult<usize> {
    diesel::insert_into(transactions::table)
        .values(&tx)
        .execute(conn)
        .await
}

/// `(error_event_count, analytics_event_count)` for an app — onboarding poll.
/// Whether the app has received any error / analytics events yet.
///
/// Deliberately `EXISTS` rather than `count(*)`: the only consumer is the
/// onboarding "have we seen your first event?" poll, which needs a boolean.
/// Counting scanned every partition of the two largest tables on each poll.
///
/// Takes `ReadScope`, not a bare `app_id`: onboarding builds its DSN from one
/// specific environment, so a poll that answered "has ANY environment sent
/// anything" could report success purely from a *different* environment's
/// traffic (e.g. an app with existing prod events, where a user adds a
/// staging environment and revisits onboarding — the staging DSN would
/// immediately show "received" from prod rows alone).
pub async fn app_has_events(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
) -> QueryResult<(bool, bool)> {
    let has_errors: bool = diesel::select(diesel::dsl::exists(crate::scope_env!(
        error_events::table
            .filter(error_events::app_id.eq(scope.app_id))
            .into_boxed(),
        error_events,
        &scope.env
    )))
    .get_result(conn)
    .await?;
    let has_events: bool = diesel::select(diesel::dsl::exists(crate::scope_env!(
        analytics_events::table
            .filter(analytics_events::app_id.eq(scope.app_id))
            .into_boxed(),
        analytics_events,
        &scope.env
    )))
    .get_result(conn)
    .await?;
    Ok((has_errors, has_events))
}

pub async fn error_series(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    since: DateTime<Utc>,
) -> QueryResult<Vec<SeriesPoint>> {
    let env_sql = scope.env.sql_fragment(3);
    let q = format!(
        "SELECT date_trunc('day', occurred_at) AS bucket, count(*)::bigint AS count \
         FROM error_events WHERE app_id = $1 AND occurred_at >= $2{env_sql} \
         GROUP BY bucket ORDER BY bucket"
    );
    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Timestamptz, _>(since);
    stmt = crate::bind_env!(stmt, &scope.env);
    stmt.get_results(conn).await
}

// ===========================================================================
// Sessions (list + per-session signal streams for the timeline)
// ===========================================================================

#[allow(clippy::too_many_arguments)]
pub async fn list_sessions(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    since: DateTime<Utc>,
    limit: i64,
    offset: i64,
    distinct_id: Option<&str>,
    device_key: Option<&str>,
) -> QueryResult<Vec<Session>> {
    let mut q = sessions::table
        .filter(sessions::app_id.eq(scope.app_id))
        .filter(sessions::last_event_at.ge(since))
        .into_boxed();
    q = crate::scope_env!(q, sessions, &scope.env);
    if let Some(d) = distinct_id {
        q = q.filter(sessions::distinct_id.eq(d.to_string()));
    }
    if let Some(dk) = device_key {
        q = q.filter(sessions::device_key.eq(dk.to_string()));
    }
    q.select(Session::as_select())
        .order(sessions::last_event_at.desc())
        .limit(limit)
        .offset(offset)
        .load(conn)
        .await
}

/// A session outside `scope` returns `None`, not the row — the handler turns
/// that into a 404 (fail narrow). `sessions` is the only one of these four
/// tables with a `UNIQUE (app_id, session_id)` constraint, and `bump_session`
/// lets `environment_id` flip to the most recent non-null value on conflict
/// (see its own doc comment), so the session's own label cannot be trusted to
/// disambiguate its children — this function's scope check exists
/// independently of theirs, not as a shortcut for them.
pub async fn get_session(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    session_id: &str,
) -> QueryResult<Option<Session>> {
    let q = sessions::table
        .filter(sessions::app_id.eq(scope.app_id))
        .filter(sessions::session_id.eq(session_id.to_string()))
        .into_boxed();
    crate::scope_env!(q, sessions, &scope.env)
        .select(Session::as_select())
        .first(conn)
        .await
        .optional()
}

/// `analytics_events.session_id` is nullable free text with no uniqueness and
/// no environment linkage — unlike `sessions`, a session's own environment
/// label does not disambiguate which environment its child rows belong to
/// (e.g. a device repointed from staging to prod without a fresh session id).
/// The environment predicate is applied here directly rather than inherited
/// from the session.
pub async fn events_for_session(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    session_id: &str,
    limit: i64,
) -> QueryResult<Vec<AnalyticsEvent>> {
    let q = analytics_events::table
        .filter(analytics_events::app_id.eq(scope.app_id))
        .filter(analytics_events::session_id.eq(session_id.to_string()))
        .into_boxed();
    crate::scope_env!(q, analytics_events, &scope.env)
        .select(AnalyticsEvent::as_select())
        .order(analytics_events::occurred_at.asc())
        .limit(limit)
        .load(conn)
        .await
}

/// See [`events_for_session`]'s doc comment — same reasoning, `error_events`.
pub async fn errors_for_session(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    session_id: &str,
    limit: i64,
) -> QueryResult<Vec<ErrorEvent>> {
    let q = error_events::table
        .filter(error_events::app_id.eq(scope.app_id))
        .filter(error_events::session_id.eq(session_id.to_string()))
        .into_boxed();
    crate::scope_env!(q, error_events, &scope.env)
        .select(ErrorEvent::as_select())
        .order(error_events::occurred_at.asc())
        .limit(limit)
        .load(conn)
        .await
}

/// See [`events_for_session`]'s doc comment — same reasoning, `transactions`.
pub async fn transactions_for_session(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    session_id: &str,
    limit: i64,
) -> QueryResult<Vec<Transaction>> {
    let q = transactions::table
        .filter(transactions::app_id.eq(scope.app_id))
        .filter(transactions::session_id.eq(session_id.to_string()))
        .into_boxed();
    crate::scope_env!(q, transactions, &scope.env)
        .select(Transaction::as_select())
        .order(transactions::occurred_at.asc())
        .limit(limit)
        .load(conn)
        .await
}

// ===========================================================================
// Devices (inventory + per-device errors)
// ===========================================================================

/// F4 (final whole-branch review, `.superpowers/sdd/s2-final-review.md`):
/// `events_count`/`errors_count`/`sessions_count` were already
/// environment-scoped (Task 8); `first_seen`/`last_seen`/`last_distinct_id`
/// are now derived per-environment too, under `One`/`Unattributed` — see
/// [`list_devices`]'s doc comment for how. Under `EnvFilter::All` all three
/// still read the stored `devices` row directly (the durable fast path,
/// unchanged).
///
/// `last_distinct_id` was the concrete disclosure vector F4 named: a device
/// whose most recent identity is a production-only user must not surface
/// that identity under a staging scope, because `bump_device`'s
/// `last_distinct_id` column folds every environment's writes into one
/// app-wide value with no notion of "as of this environment".
///
/// `family`/`model`/`os_name`/`os_version`/`arch`/`browser` are, like
/// `PersonRow::properties`, deliberately left app-wide and undocumented no
/// longer — a physical device has one descriptor, not one per environment
/// it happens to report telemetry from, so there is no per-environment
/// reading to derive these from any more than there is for a person's
/// property bag.
#[derive(Debug, QueryableByName, serde::Serialize)]
pub struct DeviceRow {
    #[diesel(sql_type = SqlUuid)]
    pub id: Uuid,
    #[diesel(sql_type = Text)]
    pub device_key: String,
    #[diesel(sql_type = Nullable<Text>)]
    pub family: Option<String>,
    #[diesel(sql_type = Nullable<Text>)]
    pub model: Option<String>,
    #[diesel(sql_type = Nullable<Text>)]
    pub os_name: Option<String>,
    #[diesel(sql_type = Nullable<Text>)]
    pub os_version: Option<String>,
    #[diesel(sql_type = Nullable<Text>)]
    pub arch: Option<String>,
    #[diesel(sql_type = Nullable<Text>)]
    pub browser: Option<String>,
    #[diesel(sql_type = Nullable<Text>)]
    pub last_distinct_id: Option<String>,
    #[diesel(sql_type = Timestamptz)]
    pub first_seen: DateTime<Utc>,
    #[diesel(sql_type = Timestamptz)]
    pub last_seen: DateTime<Utc>,
    #[diesel(sql_type = BigInt)]
    pub events_count: i64,
    #[diesel(sql_type = BigInt)]
    pub errors_count: i64,
    #[diesel(sql_type = BigInt)]
    pub sessions_count: i64,
}

/// The `distinct_id` of the most recent (by time) signal in the selected
/// environment, across all three tables that carry one — used by
/// [`list_devices`]/[`get_device`] to derive `last_distinct_id` per
/// environment instead of reading `devices.last_distinct_id` directly (see
/// `DeviceRow`'s doc comment for why that column is the disclosure vector F4
/// named: `bump_device`'s `COALESCE(EXCLUDED.last_distinct_id,
/// devices.last_distinct_id)` folds every environment's writes into one
/// app-wide value, last-write-wins, with no per-environment reading at all).
///
/// A `distinct_id IS NOT NULL` guard on the two nullable-`distinct_id` legs
/// (`error_events`, `sessions` — `analytics_events.distinct_id` is `NOT
/// NULL` in the schema, so it needs none) mirrors `bump_device`'s own
/// `COALESCE(EXCLUDED.last_distinct_id, devices.last_distinct_id)`: a NULL
/// write never overwrites a known identity, so an anonymous event must never
/// win over an identified one that is merely slightly older.
///
/// Aliased `lae`/`lee`/`lse` rather than reusing `ae`/`ee`/`se` — the names
/// the sibling count/min/max LATERALs already use in the same query. Postgres
/// scopes them correctly either way (each is local to its own subquery), but
/// a human skimming the SQL text next to those siblings should not have to
/// check.
fn device_last_distinct_id_join(env: EnvFilter, bind_index: usize) -> String {
    let ae_env = env.sql_fragment_for("lae", bind_index);
    let ee_env = env.sql_fragment_for("lee", bind_index);
    let se_env = env.sql_fragment_for("lse", bind_index);
    format!(
        " LEFT JOIN LATERAL ( \
             SELECT distinct_id FROM ( \
                 SELECT distinct_id, occurred_at FROM analytics_events lae \
                 WHERE lae.app_id = $1 AND lae.device_key = d.device_key{ae_env} \
                 UNION ALL \
                 SELECT distinct_id, occurred_at FROM error_events lee \
                 WHERE lee.app_id = $1 AND lee.device_key = d.device_key \
                   AND lee.distinct_id IS NOT NULL{ee_env} \
                 UNION ALL \
                 SELECT distinct_id, last_event_at AS occurred_at FROM sessions lse \
                 WHERE lse.app_id = $1 AND lse.device_key = d.device_key \
                   AND lse.distinct_id IS NOT NULL{se_env} \
             ) recent \
             ORDER BY occurred_at DESC LIMIT 1 \
         ) ld ON TRUE"
    )
}

pub async fn list_devices(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    since: DateTime<Utc>,
    limit: i64,
    offset: i64,
    search: Option<&str>,
) -> QueryResult<Vec<DeviceRow>> {
    // Escape LIKE metacharacters: an unescaped `%`/`_` makes a literal search
    // term match the wrong rows, and a pattern of many wildcards makes ILIKE
    // matching super-linear per scanned row.
    let pattern = search.map(like_contains).unwrap_or_else(|| "%".to_string());

    // $1 app_id, $2 since, $3 pattern, $4 limit, $5 offset — env takes $6 when
    // it needs a bind, reused across the count LATERALs that are actually
    // emitted (see the `counts_select`/`counts_join` comment below — `events`/
    // `errors` only under `One`/`Unattributed`, `sessions` always) and the
    // membership `EXISTS` (only emitted when `scope.env != All`). Same idiom
    // as `list_persons`.
    let env_sql = scope.env.sql_fragment(6);

    // See `list_persons`' doc comment: this is a WHERE-clause predicate on the
    // paging subquery, not a join, so it does not disturb where LIMIT is
    // applied. Omitted entirely under `All` — same reasoning as `list_persons`.
    //
    // Each leg aliases its subquery and qualifies the correlated column with
    // that alias (`ae.device_key`, not bare `device_key`). Demonstrated live
    // during review: with no alias, an unqualified name that happens to also
    // exist on the inner table resolves there only by luck — if a future copy
    // of this pattern targets a table with no `device_key` column, Postgres
    // silently binds the bare name to the *outer* `devices` row instead,
    // collapsing the whole `EXISTS` into `devices.device_key =
    // devices.device_key` (always true, no error). Qualifying turns that
    // mistake into a hard query error instead.
    //
    // The sessions leg also carries `started_at >= $2`, matching the `se`
    // LATERAL below (and matching the `se` LATERAL's own pre-existing time
    // bound, which predates this task). Without it, a device whose only
    // env_a session is older than `since` — but whose `devices.last_seen` is
    // recent from unrelated env_b activity — would still pass membership and
    // render an all-zero row under `One(env_a)`, the exact bug this filter
    // exists to prevent.
    let membership_sql = if matches!(scope.env, EnvFilter::All) {
        String::new()
    } else {
        let ae_env = scope.env.sql_fragment_for("ae", 6);
        let ee_env = scope.env.sql_fragment_for("ee", 6);
        let se_env = scope.env.sql_fragment_for("se", 6);
        format!(
            " AND ( \
                EXISTS (SELECT 1 FROM analytics_events ae WHERE ae.app_id=$1 AND ae.device_key = devices.device_key{ae_env}) \
                OR EXISTS (SELECT 1 FROM error_events ee WHERE ee.app_id=$1 AND ee.device_key = devices.device_key{ee_env}) \
                OR EXISTS (SELECT 1 FROM sessions se WHERE se.app_id=$1 AND se.device_key = devices.device_key AND se.started_at >= $2{se_env}) \
              )"
        )
    };

    // `devices.events_count`/`errors_count` are lifetime counters that
    // `bump_device` increments on every event regardless of environment —
    // durable, because `devices` is never partitioned and never dropped.
    // `analytics_events`/`error_events` ARE partitioned by `sauron-tier`
    // (`bins/sauron-tier/src/main.rs`), which exports aged partitions (past
    // `TIER_HOT_DAYS`, default 30 days) to Parquet and then drops them from
    // Postgres. The `ae`/`ee` LATERALs below can only see rows still in
    // Postgres, so for a device whose activity has aged out of the hot window
    // they under-report — all the way down to 0 for a device with a real,
    // large lifetime count. This is the same tiering blind spot the design
    // doc records for per-environment issue counts (see "No new table" in
    // `docs/superpowers/specs/2026-07-28-environment-scoped-reads-design.md`:
    // `issues.times_seen` vs. a per-environment LATERAL over `error_events`)
    // — the scoped count cannot see tiered data, and that is accepted rather
    // than solved here.
    //
    // So `All` — "every environment, all time" — reads the durable columns
    // directly, no join, no subquery, matching that design's precedent for
    // `All`. `One`/`Unattributed` have no alternative but the LATERALs: they
    // are the only thing that *can* be scoped to a single environment,
    // tiering blind spot and all. `sessions_count` has no durable column to
    // fall back to (`devices` was never denormalized for it, and `sessions`
    // itself is not one of `sauron-tier`'s tiered tables), so it stays a
    // LATERAL under every variant, exactly as it already was before this
    // task — do not read this as an oversight; the two fields are computed
    // differently on purpose.
    //
    // F4: `first_seen`/`last_seen`/`last_distinct_id` follow the identical
    // `All`-vs-scoped split, folded into this same variable rather than a
    // parallel one — under `All` they read straight off `d`; under
    // `One`/`Unattributed` they extend the very `ae`/`ee` LATERALs this
    // fixes' counts already join, adding `min`/`max(occurred_at)`, plus
    // [`device_last_distinct_id_join`] for `last_distinct_id` (see its own
    // doc comment). `LEAST`/`GREATEST` ignore `NULL` arguments (Postgres's
    // documented behaviour), so a device that qualifies via only one of
    // `ae`/`ee`/`se` (e.g. `session_only_device_key`, sessions alone) still
    // gets a real value from the other two `NULL` legs.
    let (scoped_select, scoped_join) = if matches!(scope.env, EnvFilter::All) {
        (
            "d.events_count AS events_count, d.errors_count AS errors_count, \
             d.first_seen AS first_seen, d.last_seen AS last_seen, \
             d.last_distinct_id AS last_distinct_id"
                .to_string(),
            String::new(),
        )
    } else {
        // `.clone()`, not a move: `env_sql`/the final `bind_env!` call below both
        // still need `scope.env` after this — `device_last_distinct_id_join` keeps
        // its pre-existing owned-`EnvFilter` signature (not reshaped to take `&`),
        // per the rule of adding a clone at the call site instead.
        let ld_join = device_last_distinct_id_join(scope.env.clone(), 6);
        (
            "COALESCE(ae.cnt, 0)::bigint AS events_count, \
             COALESCE(ee.cnt, 0)::bigint AS errors_count, \
             LEAST(ae.min_occurred, ee.min_occurred, se.min_started) AS first_seen, \
             GREATEST(ae.max_occurred, ee.max_occurred, se.max_last_event) AS last_seen, \
             ld.distinct_id AS last_distinct_id"
                .to_string(),
            format!(
                " LEFT JOIN LATERAL ( \
                     SELECT count(*) AS cnt, min(occurred_at) AS min_occurred, \
                            max(occurred_at) AS max_occurred FROM analytics_events \
                     WHERE app_id = $1 AND device_key = d.device_key{env_sql} \
                 ) ae ON TRUE \
                 LEFT JOIN LATERAL ( \
                     SELECT count(*) AS cnt, min(occurred_at) AS min_occurred, \
                            max(occurred_at) AS max_occurred FROM error_events \
                     WHERE app_id = $1 AND device_key = d.device_key{env_sql} \
                 ) ee ON TRUE{ld_join}"
            ),
        )
    };

    // Page FIRST, then count per returned device via LATERAL subqueries — same
    // reasoning as `list_persons` (Postgres cannot push the outer LIMIT into a
    // grouped subquery).
    //
    // The `se` LATERAL's `since` bound moved from a `WHERE` clause to a
    // `count(*) FILTER (...)` — F4 needs `min(started_at)`/`max(last_event_at)`
    // over *all* of this device's env-scoped sessions, not just the ones
    // after `since` (a device's true per-environment `first_seen` can predate
    // the page's window; `since` only decides which devices are listed, via
    // the outer `WHERE ... last_seen >= $2`, unchanged). Filtering only the
    // count aggregate is equivalent to the old `WHERE started_at >= $2` for
    // `cnt` specifically (same rows excluded, same count), while leaving the
    // two new aggregates unbounded.
    let q = format!(
        "SELECT d.id, d.device_key, d.family, d.model, d.os_name, d.os_version, d.arch, \
                d.browser, \
                {scoped_select}, \
                COALESCE(se.cnt, 0)::bigint AS sessions_count \
         FROM ( \
             SELECT * FROM devices \
             WHERE app_id = $1 AND last_seen >= $2 \
               AND (COALESCE(family,'') || ' ' || COALESCE(model,'') || ' ' || \
                    COALESCE(os_name,'') || ' ' || COALESCE(device_key,'')) ILIKE $3{membership_sql} \
             ORDER BY last_seen DESC LIMIT $4 OFFSET $5 \
         ) d{scoped_join} \
         LEFT JOIN LATERAL ( \
             SELECT count(*) FILTER (WHERE started_at >= $2) AS cnt, \
                    min(started_at) AS min_started, max(last_event_at) AS max_last_event \
             FROM sessions \
             WHERE app_id = $1 AND device_key = d.device_key{env_sql} \
         ) se ON TRUE \
         ORDER BY d.last_seen DESC"
    );
    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Timestamptz, _>(since)
        .bind::<Text, _>(pattern)
        .bind::<BigInt, _>(limit)
        .bind::<BigInt, _>(offset);
    stmt = crate::bind_env!(stmt, &scope.env);
    stmt.get_results(conn).await
}

/// `devices` carries no `environment_id`, so membership is derived the same
/// way [`list_devices`]' membership `EXISTS` derives it — activity in
/// `analytics_events`/`error_events`/`sessions`, keyed by `device_key`.
/// Omitted under `All`, same reasoning.
///
/// Returns [`DeviceRow`], not the raw [`Device`] model, and is raw SQL rather
/// than the diesel query builder `list_devices` used to be — both follow from
/// the same fact: `events_count`/`errors_count` need a different source
/// depending on `scope.env` (the durable `devices` columns under `All`, an
/// environment-scoped LATERAL under `One`/`Unattributed` — see `list_devices`'
/// doc comment for the full tiering reasoning this works around), and
/// `Device` has no way to carry two different answers for the same field
/// depending on scope; diesel's query builder has no easy way to switch a
/// selected column's source per branch either. Before this change the Device
/// Detail page (`bins/sauron-api/src/routes/devices.rs`'s `DeviceDetail`)
/// rendered `Device`'s raw, cross-environment, all-time counters directly
/// above a sessions/errors/performance list that Task 8 *did* scope — a
/// device viewed under `One(staging)` would show prod+staging all-time totals
/// above a handful of staging-only rows. That is the bug this function exists
/// to not have.
pub async fn get_device(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    device_key: &str,
) -> QueryResult<Option<DeviceRow>> {
    let env_sql = scope.env.sql_fragment(3);

    // See `list_devices`' membership `EXISTS` doc comment: each leg is
    // aliased and the correlated column qualified with that alias, so an
    // unqualified name colliding with the outer `devices` row is a hard query
    // error rather than a silent always-true tautology. No `started_at` bound
    // on the sessions leg — unlike `list_devices`, this function has no
    // `since` parameter to bound it against; a single-identity lookup has no
    // notion of a page's time window.
    let membership_sql = if matches!(scope.env, EnvFilter::All) {
        String::new()
    } else {
        let ae_env = scope.env.sql_fragment_for("ae", 3);
        let ee_env = scope.env.sql_fragment_for("ee", 3);
        let se_env = scope.env.sql_fragment_for("se", 3);
        format!(
            " AND ( \
                EXISTS (SELECT 1 FROM analytics_events ae WHERE ae.app_id=$1 AND ae.device_key = devices.device_key{ae_env}) \
                OR EXISTS (SELECT 1 FROM error_events ee WHERE ee.app_id=$1 AND ee.device_key = devices.device_key{ee_env}) \
                OR EXISTS (SELECT 1 FROM sessions se WHERE se.app_id=$1 AND se.device_key = devices.device_key{se_env}) \
              )"
        )
    };

    // Same `All`-vs-scoped source split as `list_devices` — see that
    // function's doc comment for the full reasoning, including why
    // `first_seen`/`last_seen`/`last_distinct_id` (F4) join the same split.
    // No `since` bound anywhere in this function (single-identity lookup, no
    // page window), so — unlike `list_devices` — the `se` LATERAL needs no
    // `FILTER` trick: its `min`/`max` were already unbounded.
    let (scoped_select, scoped_join) = if matches!(scope.env, EnvFilter::All) {
        (
            "d.events_count AS events_count, d.errors_count AS errors_count, \
             d.first_seen AS first_seen, d.last_seen AS last_seen, \
             d.last_distinct_id AS last_distinct_id"
                .to_string(),
            String::new(),
        )
    } else {
        // `.clone()`, not a move — see `list_devices`' identical call for why.
        let ld_join = device_last_distinct_id_join(scope.env.clone(), 3);
        (
            "COALESCE(ae.cnt, 0)::bigint AS events_count, \
             COALESCE(ee.cnt, 0)::bigint AS errors_count, \
             LEAST(ae.min_occurred, ee.min_occurred, se.min_started) AS first_seen, \
             GREATEST(ae.max_occurred, ee.max_occurred, se.max_last_event) AS last_seen, \
             ld.distinct_id AS last_distinct_id"
                .to_string(),
            format!(
                " LEFT JOIN LATERAL ( \
                     SELECT count(*) AS cnt, min(occurred_at) AS min_occurred, \
                            max(occurred_at) AS max_occurred FROM analytics_events \
                     WHERE app_id = $1 AND device_key = d.device_key{env_sql} \
                 ) ae ON TRUE \
                 LEFT JOIN LATERAL ( \
                     SELECT count(*) AS cnt, min(occurred_at) AS min_occurred, \
                            max(occurred_at) AS max_occurred FROM error_events \
                     WHERE app_id = $1 AND device_key = d.device_key{env_sql} \
                 ) ee ON TRUE{ld_join}"
            ),
        )
    };

    let q = format!(
        "SELECT d.id, d.device_key, d.family, d.model, d.os_name, d.os_version, d.arch, \
                d.browser, \
                {scoped_select}, \
                COALESCE(se.cnt, 0)::bigint AS sessions_count \
         FROM ( \
             SELECT * FROM devices \
             WHERE app_id = $1 AND device_key = $2{membership_sql} \
         ) d{scoped_join} \
         LEFT JOIN LATERAL ( \
             SELECT count(*) AS cnt, min(started_at) AS min_started, \
                    max(last_event_at) AS max_last_event FROM sessions \
             WHERE app_id = $1 AND device_key = d.device_key{env_sql} \
         ) se ON TRUE"
    );
    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Text, _>(device_key.to_string());
    stmt = crate::bind_env!(stmt, &scope.env);
    stmt.get_result(conn).await.optional()
}

/// `error_events` carries its own `environment_id` directly, so this is an
/// ordinary `scope_env!` filter — unlike `get_device`, which has to derive
/// membership because `devices` itself carries none.
pub async fn errors_for_device(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    device_key: &str,
    limit: i64,
) -> QueryResult<Vec<ErrorEvent>> {
    let q = error_events::table
        .filter(error_events::app_id.eq(scope.app_id))
        .filter(error_events::device_key.eq(device_key.to_string()))
        .into_boxed();
    crate::scope_env!(q, error_events, &scope.env)
        .select(ErrorEvent::as_select())
        .order(error_events::occurred_at.desc())
        .limit(limit)
        .load(conn)
        .await
}

// ===========================================================================
// Persons (Users Explorer — event_user + activity counts)
// ===========================================================================

/// F4 (final whole-branch review, `.superpowers/sdd/s2-final-review.md`):
/// `events_count`/`errors_count`/`sessions_count` were already environment-scoped
/// (Task 8); `first_seen`/`last_seen` are now derived per-environment too — see
/// [`list_persons`]'s doc comment for how (the same `ae`/`ee`/`se` LATERALs that
/// already compute the three counts, extended with `min`/`max(occurred_at)`).
/// Under `EnvFilter::All` they still read `event_users.first_seen`/`last_seen`
/// directly — the durable fast path, unchanged.
///
/// `properties` is the one field on this struct that is **not** derived, and
/// that is a decision, not an oversight. `event_users` carries no
/// `environment_id` at all, and a person has exactly one property bag — unlike
/// `first_seen`/`last_seen` (a `min`/`max` over a set of per-environment rows),
/// there is no per-environment *copy* of `properties` to fall back to; the
/// value either is app-wide or does not exist. Membership already gates
/// whether this row is visible at all (see the membership `EXISTS` below): a
/// person only appears because they have real activity in the selected
/// environment, so showing their one property bag is showing the properties
/// of someone the caller is legitimately looking at, not a cross-environment
/// leak the way a *different* person's `last_distinct_id` on someone else's
/// device would be (see `DeviceRow`). Slice 3, where environment becomes an
/// access boundary rather than a read-scoping dimension, should make this
/// choice explicitly — does a property bag stay visible to a caller scoped to
/// an environment the person merely also happens to appear in, or should
/// `properties` require broader access? — rather than inherit it silently.
#[derive(Debug, QueryableByName, serde::Serialize)]
pub struct PersonRow {
    #[diesel(sql_type = Text)]
    pub distinct_id: String,
    #[diesel(sql_type = Jsonb)]
    pub properties: Value,
    #[diesel(sql_type = Timestamptz)]
    pub first_seen: DateTime<Utc>,
    #[diesel(sql_type = Timestamptz)]
    pub last_seen: DateTime<Utc>,
    #[diesel(sql_type = BigInt)]
    pub events_count: i64,
    #[diesel(sql_type = BigInt)]
    pub errors_count: i64,
    #[diesel(sql_type = BigInt)]
    pub sessions_count: i64,
}

pub async fn list_persons(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    search: Option<&str>,
    limit: i64,
    offset: i64,
) -> QueryResult<Vec<PersonRow>> {
    // Escape LIKE metacharacters: an unescaped `%`/`_` makes a literal search
    // term match the wrong rows, and a pattern of many wildcards makes ILIKE
    // matching super-linear per scanned row.
    let pattern = search.map(like_contains).unwrap_or_else(|| "%".to_string());

    // $1 app_id, $2 pattern, $3 limit, $4 offset — env takes $5 when it needs a
    // bind, reused across the three count LATERALs (always emitted, `""` under
    // `All`) and the membership `EXISTS` (only emitted when `scope.env != All`).
    // Same "one bind, several textual occurrences" idiom as `user_stats`' `$3`.
    //
    // Unlike `list_devices` (see that function's doc comment), `events_count`/
    // `errors_count` here have no `All`-only fast path onto a durable column:
    // `event_users` was never denormalized with lifetime counters the way
    // `devices` was — checked directly against `EventUser`'s fields, not
    // assumed — so every variant, including `All`, reads these two LATERALs
    // unconditionally. That also means `list_persons` carries none of
    // `list_devices`' tiering blind spot under `All`; it already had it, and
    // still has it, under every scope.
    let env_sql = scope.env.sql_fragment(5);

    // `event_users` carries no `environment_id` at all, so a person's
    // membership in a specific environment can only be derived from whether
    // they have any row in one of the three tables that do carry it. This is a
    // WHERE-clause predicate on the *inner* paging subquery (not a join), so it
    // does not disturb where LIMIT is applied — see the paging comment below.
    // Omitted entirely under `All`: every `event_users` row exists only because
    // `note_identity` registered it from a real analytics/error event, so an
    // unfiltered `EXISTS` would add three subquery lookups per candidate row
    // for no narrowing effect.
    //
    // Each leg aliases its subquery and qualifies the correlated column with
    // that alias (`ae.distinct_id`, not bare `distinct_id`) — see
    // `list_devices`' membership `EXISTS` doc comment for why an unqualified
    // name is a live footgun, not a style nit.
    let membership_sql = if matches!(scope.env, EnvFilter::All) {
        String::new()
    } else {
        let ae_env = scope.env.sql_fragment_for("ae", 5);
        let ee_env = scope.env.sql_fragment_for("ee", 5);
        let se_env = scope.env.sql_fragment_for("se", 5);
        format!(
            " AND ( \
                EXISTS (SELECT 1 FROM analytics_events ae WHERE ae.app_id=$1 AND ae.distinct_id = event_users.distinct_id{ae_env}) \
                OR EXISTS (SELECT 1 FROM error_events ee WHERE ee.app_id=$1 AND ee.distinct_id = event_users.distinct_id{ee_env}) \
                OR EXISTS (SELECT 1 FROM sessions se WHERE se.app_id=$1 AND se.distinct_id = event_users.distinct_id{se_env}) \
              )"
        )
    };

    // Page FIRST, then count per returned person via LATERAL subqueries.
    //
    // The previous form used three grouped subqueries over analytics_events,
    // error_events and sessions filtered only by app_id. Postgres cannot push
    // the outer LIMIT into a GROUP BY subquery, so every page load aggregated
    // the app's entire history across the two largest tables and then discarded
    // all but ~50 rows. Counting per-page turns that into a handful of
    // index lookups on (app_id, distinct_id). Adding `membership_sql` above
    // preserves this shape — it narrows the *same* inner subquery's WHERE
    // clause, before LIMIT/OFFSET are applied, rather than adding a join stage
    // — confirmed with `EXPLAIN`, see the task report.
    //
    // F4: `ae`/`ee`/`se` also compute `min`/`max(occurred_at)` now (sessions'
    // own analogue is `started_at`/`last_event_at` — it has no single
    // `occurred_at` column), extending the same three LATERALs rather than
    // adding a fourth. `first_seen`/`last_seen` under `All` still read
    // `eu.first_seen`/`eu.last_seen` directly (the durable fast path,
    // unaffected by this fix); under `One`/`Unattributed` they are
    // `LEAST`/`GREATEST` over the three per-source extrema. Postgres's
    // `LEAST`/`GREATEST` skip `NULL` arguments (documented behaviour, not an
    // assumption) rather than propagating them, so a person who qualifies via
    // only one of the three tables (e.g. `session_only_distinct_id`, sessions
    // alone) still gets a real value out of the other two `NULL` legs instead
    // of `NULL` itself — membership already guarantees at least one leg is
    // non-null for any row that reaches this point.
    let seen_select = if matches!(scope.env, EnvFilter::All) {
        "eu.first_seen AS first_seen, eu.last_seen AS last_seen".to_string()
    } else {
        "LEAST(ae.min_occurred, ee.min_occurred, se.min_started) AS first_seen, \
         GREATEST(ae.max_occurred, ee.max_occurred, se.max_last_event) AS last_seen"
            .to_string()
    };
    let q = format!(
        "SELECT eu.distinct_id, eu.properties, {seen_select}, \
                COALESCE(ae.cnt,0)::bigint AS events_count, \
                COALESCE(ee.cnt,0)::bigint AS errors_count, \
                COALESCE(se.cnt,0)::bigint AS sessions_count \
         FROM ( \
             SELECT distinct_id, properties, first_seen, last_seen FROM event_users \
             WHERE app_id=$1 AND (distinct_id ILIKE $2 OR properties::text ILIKE $2){membership_sql} \
             ORDER BY last_seen DESC LIMIT $3 OFFSET $4 \
         ) eu \
         LEFT JOIN LATERAL (SELECT count(*) cnt, min(occurred_at) min_occurred, \
                    max(occurred_at) max_occurred FROM analytics_events \
                    WHERE app_id=$1 AND distinct_id = eu.distinct_id{env_sql}) ae ON TRUE \
         LEFT JOIN LATERAL (SELECT count(*) cnt, min(occurred_at) min_occurred, \
                    max(occurred_at) max_occurred FROM error_events \
                    WHERE app_id=$1 AND distinct_id = eu.distinct_id{env_sql}) ee ON TRUE \
         LEFT JOIN LATERAL (SELECT count(*) cnt, min(started_at) min_started, \
                    max(last_event_at) max_last_event FROM sessions \
                    WHERE app_id=$1 AND distinct_id = eu.distinct_id{env_sql}) se ON TRUE \
         ORDER BY eu.last_seen DESC"
    );
    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Text, _>(pattern)
        .bind::<BigInt, _>(limit)
        .bind::<BigInt, _>(offset);
    stmt = crate::bind_env!(stmt, &scope.env);
    stmt.get_results(conn).await
}

// ===========================================================================
// Overview (composite health snapshot)
// ===========================================================================

#[derive(Debug, QueryableByName, serde::Serialize)]
pub struct OverviewTotals {
    #[diesel(sql_type = BigInt)]
    pub events: i64,
    #[diesel(sql_type = BigInt)]
    pub errors: i64,
    #[diesel(sql_type = BigInt)]
    pub sessions: i64,
    #[diesel(sql_type = BigInt)]
    pub users: i64,
    #[diesel(sql_type = BigInt)]
    pub new_users: i64,
    #[diesel(sql_type = BigInt)]
    pub crashed_sessions: i64,
}

/// `event_users` carries no `environment_id` column, so membership in a specific environment
/// can only be derived from whether an identity has any row in one of the three signal
/// tables that do carry it — the same `EXISTS`-over-three-tables idiom `list_persons` uses
/// for its own membership filter (see that function's doc comment for the full reasoning;
/// this is a shared helper rather than a third copy of the same fragment, one per caller
/// below).
///
/// Every one of this fragment's callers has `app_id` bound at `$1` (checked at each call
/// site, not assumed generically), so the three `EXISTS` legs hardcode `$1` rather than
/// taking it as a parameter. `bind_index` is the *environment*'s bind — reused verbatim from
/// the caller's own `env_sql`, since it is the identical value, not a second bind
/// (`list_persons`' "one bind, many textual occurrences" idiom).
///
/// Returns `""` under `EnvFilter::All`, omitting the `EXISTS` entirely rather than narrowing
/// it to a tautology: every `event_users` row exists only because some event registered it,
/// so an unfiltered membership check would add three subquery lookups per row for no
/// narrowing effect.
///
/// Each leg aliases its subquery and qualifies the correlated column with that alias
/// (`ae.distinct_id`, not bare `distinct_id`) — an unqualified name that happens to also
/// exist on the outer table resolves there silently instead of erroring, turning the
/// predicate into an always-true tautology with no query error to catch it. Demonstrated
/// live during Task 8's review; see `list_persons`' doc comment.
fn event_user_membership_exists(env: EnvFilter, bind_index: usize) -> String {
    if matches!(env, EnvFilter::All) {
        return String::new();
    }
    let ae_env = env.sql_fragment_for("ae", bind_index);
    let ee_env = env.sql_fragment_for("ee", bind_index);
    let se_env = env.sql_fragment_for("se", bind_index);
    format!(
        " AND ( \
            EXISTS (SELECT 1 FROM analytics_events ae WHERE ae.app_id=$1 AND ae.distinct_id = event_users.distinct_id{ae_env}) \
            OR EXISTS (SELECT 1 FROM error_events ee WHERE ee.app_id=$1 AND ee.distinct_id = event_users.distinct_id{ee_env}) \
            OR EXISTS (SELECT 1 FROM sessions se WHERE se.app_id=$1 AND se.distinct_id = event_users.distinct_id{se_env}) \
          )"
    )
}

pub async fn overview_totals(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    since: DateTime<Utc>,
) -> QueryResult<OverviewTotals> {
    // $1 app_id, $2 since, reused across all six sub-selects (as before). Env takes $3 when
    // it needs a bind, reused across the four sub-selects whose table actually carries
    // `environment_id` (analytics_events, error_events, sessions x2) AND, as of this fix,
    // across `users`/`new_users`' membership `EXISTS` legs below — same environment, same
    // bind, not a second one. Each analytics/error/sessions sub-select is un-aliased (no
    // join), so `sql_fragment_for` is passed the table's own name rather than a shortened
    // alias — purely for self-documentation, since a bare `sql_fragment` would emit
    // identical SQL here.
    let env_sql_analytics = scope.env.sql_fragment_for("analytics_events", 3);
    let env_sql_errors = scope.env.sql_fragment_for("error_events", 3);
    let env_sql_sessions = scope.env.sql_fragment_for("sessions", 3);

    // `users`/`new_users` read `event_users`, which carries no `environment_id` — scoped by
    // membership (see `event_user_membership_exists`'s doc comment), the gap Task 8 deferred
    // and this fix closes.
    //
    // `new_users` keeps its existing `first_seen>=$2` predicate — "globally-first-seen in
    // the window" — and ANDs membership onto it. This is reading (a) from the two documented
    // in this fix's spec: "globally-first-seen in the window AND has activity in this
    // environment", not reading (b) ("first activity *in this environment* falls in the
    // window", which needs a per-(distinct_id, environment) `min(occurred_at)` derived from
    // the three signal tables — materially more expensive, and not what `list_persons`/
    // `user_stats`/`active_user_series` do). Taken for consistency with those three.
    // Consequence: a user who first appeared in production last year and reached staging
    // today counts as "new" in *neither* environment's window under this reading — their
    // global `first_seen` predates `since` regardless of which environment's membership is
    // checked.
    // `.clone()`, not a move: the final `bind_env!` call below still needs
    // `scope.env` — `event_user_membership_exists` keeps its pre-existing
    // owned-`EnvFilter` signature (not reshaped to take `&`), per the rule of
    // adding a clone at the call site instead.
    let membership_sql = event_user_membership_exists(scope.env.clone(), 3);

    // `crashed_sessions` trusts `sessions.errors_count`/`environment_id` directly —
    // known to be able to mislabel which environment a crash counts against (not
    // just over/under by a fixed amount). See `bump_session`'s doc comment for the
    // mechanism and `.superpowers/sdd/2026-07-29-environment-rbac-scope/
    // task-10-report.md` for why the `EXISTS`-against-`error_events` fix was
    // measured and declined rather than shipped.
    let q = format!(
        "SELECT \
           (SELECT count(*) FROM analytics_events WHERE app_id=$1 AND occurred_at>=$2{env_sql_analytics})::bigint AS events, \
           (SELECT count(*) FROM error_events WHERE app_id=$1 AND occurred_at>=$2{env_sql_errors})::bigint AS errors, \
           (SELECT count(*) FROM sessions WHERE app_id=$1 AND last_event_at>=$2{env_sql_sessions})::bigint AS sessions, \
           (SELECT count(*) FROM event_users WHERE app_id=$1 AND last_seen>=$2{membership_sql})::bigint AS users, \
           (SELECT count(*) FROM event_users WHERE app_id=$1 AND first_seen>=$2{membership_sql})::bigint AS new_users, \
           (SELECT count(*) FROM sessions WHERE app_id=$1 AND last_event_at>=$2 AND errors_count>0{env_sql_sessions})::bigint AS crashed_sessions"
    );
    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Timestamptz, _>(since);
    stmt = crate::bind_env!(stmt, &scope.env);
    stmt.get_result(conn).await
}

/// Same derivation as [`list_issues`] under `One`/`Unattributed` — see its
/// doc comment for the full reasoning (membership via inner-join LATERAL +
/// `HAVING`, `since` pushed into the LATERAL's own bound rather than only
/// checked against the result afterward). No filters/`q`/`offset` here, so
/// the bind layout is fixed: $1 app_id, $2 since, $3 limit, $4 env (only
/// under `One`; last, so `Unattributed` leaves no gap — unlike
/// `list_issues`, nothing here needs `env` allocated early, since there are
/// no filter/tag/`q` fragments to share it with).
///
/// Unlike `list_issues`, the candidate set cannot be paged by `LIMIT` before
/// the join runs: the whole point of "top issues" is *ranking by the
/// per-environment count*, which does not exist until after the LATERAL
/// computes it. So the paging subquery only pre-filters — `app_id`,
/// `last_seen >= since` (a sound bound: the derived, windowed `last_seen`
/// can never exceed the issue's own app-wide `last_seen`, so this can only
/// drop rows the outer `WHERE` would have dropped anyway), and environment
/// membership via the same `EXISTS` `list_issues` uses. The LATERAL then
/// computes every surviving candidate's derived `times_seen`, and `ORDER BY
/// agg.times_seen DESC LIMIT $3` ranks and pages *after* that.
///
/// This replaces the previous shape, which paged `ORDER BY i.times_seen DESC
/// LIMIT $3` — the issue's own *app-wide* count — before the join, then
/// relabelled the page with `agg.times_seen` for display. That made the
/// top-N *selection* wrong, not just the display: an issue with 1,000,000
/// app-wide occurrences and 1 in the selected environment would permanently
/// outrank one with 5,000 in that environment, and the displayed numbers
/// were not even guaranteed to be in descending order (still sorted by the
/// app-wide count). Trading the "never aggregate more than one page" cost
/// property `list_issues` keeps for a correct ranking here — see
/// `.superpowers/sdd/s2-task-9-report.md`'s "Critical findings fixed"
/// section for the measured cost on the real dev app.
pub async fn top_issues(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    since: DateTime<Utc>,
    limit: i64,
) -> QueryResult<Vec<Issue>> {
    if matches!(scope.env, EnvFilter::All) {
        return issues::table
            .filter(issues::app_id.eq(scope.app_id))
            .filter(issues::last_seen.ge(since))
            .select(Issue::as_select())
            .order(issues::times_seen.desc())
            .limit(limit)
            .load(conn)
            .await;
    }

    let env_bind_idx = 4usize;
    let env_sql = scope.env.sql_fragment_for("e", env_bind_idx);
    let member_env_sql = scope.env.sql_fragment_for("m", env_bind_idx);
    // Task 9: same `latest` LATERAL as `list_issues`/`get_issue`, reusing
    // the identical `$4` env bind `agg` already consumes — see
    // `list_issues`' doc comment for the full title/culprit/level
    // derivation reasoning.
    let sql_text = format!(
        "SELECT i.id, i.app_id, i.fingerprint, i.type AS type_, \
                COALESCE(latest.title, i.title)     AS title, \
                COALESCE(latest.culprit, i.culprit) AS culprit, \
                COALESCE(latest.level, i.level)     AS level, \
                i.status, \
                agg.first_seen, agg.last_seen, agg.times_seen, agg.users_seen, \
                i.assignee_id, i.created_at, i.updated_at, i.last_event_at \
         FROM ( \
             SELECT * FROM issues \
             WHERE app_id = $1 AND last_seen >= $2 \
               AND EXISTS (SELECT 1 FROM error_events m WHERE m.issue_id = issues.id{member_env_sql}) \
         ) i \
         JOIN LATERAL ( \
             SELECT count(*)::bigint AS times_seen, \
                    count(DISTINCT distinct_id)::bigint AS users_seen, \
                    min(occurred_at) AS first_seen, \
                    max(occurred_at) AS last_seen \
             FROM error_events e \
             WHERE e.issue_id = i.id AND e.occurred_at >= $2{env_sql} \
             HAVING count(*) > 0 \
         ) agg ON TRUE \
         LEFT JOIN LATERAL ( \
             SELECT e.title, e.culprit, e.level \
             FROM error_events e \
             WHERE e.issue_id = i.id{env_sql} \
             ORDER BY e.occurred_at DESC \
             LIMIT 1 \
         ) latest ON TRUE \
         WHERE agg.last_seen >= $2 \
         ORDER BY agg.times_seen DESC \
         LIMIT $3"
    );
    let mut stmt = diesel::sql_query(sql_text)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Timestamptz, _>(since)
        .bind::<BigInt, _>(limit);
    stmt = crate::bind_env!(stmt, &scope.env);
    let rows: Vec<IssueRow> = stmt.get_results(conn).await?;
    Ok(rows.into_iter().map(Issue::from).collect())
}

// ===========================================================================
// Issue stats (Exceptions dashboard header)
// ===========================================================================

#[derive(Debug, QueryableByName, serde::Serialize)]
pub struct IssueStatsRow {
    #[diesel(sql_type = BigInt)]
    pub total: i64,
    #[diesel(sql_type = BigInt)]
    pub unresolved: i64,
    #[diesel(sql_type = BigInt)]
    pub resolved: i64,
    #[diesel(sql_type = BigInt)]
    pub ignored: i64,
    #[diesel(sql_type = BigInt)]
    pub fatal: i64,
    #[diesel(sql_type = BigInt)]
    pub error: i64,
    #[diesel(sql_type = BigInt)]
    pub warning: i64,
    #[diesel(sql_type = BigInt)]
    pub info: i64,
}

/// `issues` carries no `environment_id`. Under `EnvFilter::All` this reads
/// `issues` directly and is unchanged — same query this function ran before
/// Slice 2/Task 9, no join.
///
/// Under `One`/`Subset`/`Unattributed`, Task 9 replaced the plain membership
/// `EXISTS` with an **inner** `JOIN LATERAL` (`lvl`) that derives each
/// issue's `level` from its single newest `error_events` row in the selected
/// environment (`ORDER BY e.occurred_at DESC LIMIT 1`) — the identical
/// `latest` shape `list_issues`/`get_issue`/`top_issues` use, minus
/// `title`/`culprit` (unneeded here). The `level` `FILTER (WHERE ...)`
/// clauses move onto `lvl.level`, so the fatal/error/warning/info split
/// matches this environment's own occurrences, not whichever environment
/// last overwrote `issues.level`. The join is deliberately **inner, not
/// left**: an issue with no occurrence in this environment has nothing to
/// derive a level from and must not be counted at all — exactly what the
/// membership `EXISTS` it replaces was already doing. `status` stays
/// app-wide (`i.status`) in both branches — issue triage is an app-wide act
/// by design, not a per-environment one, same call `list_issues` makes for
/// the identical two columns.
pub async fn issue_stats(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
) -> QueryResult<IssueStatsRow> {
    if matches!(scope.env, EnvFilter::All) {
        return diesel::sql_query(
            "SELECT count(*)::bigint AS total, \
               count(*) FILTER (WHERE status='unresolved')::bigint AS unresolved, \
               count(*) FILTER (WHERE status='resolved')::bigint AS resolved, \
               count(*) FILTER (WHERE status='ignored')::bigint AS ignored, \
               count(*) FILTER (WHERE level='fatal')::bigint AS fatal, \
               count(*) FILTER (WHERE level='error')::bigint AS error, \
               count(*) FILTER (WHERE level='warning')::bigint AS warning, \
               count(*) FILTER (WHERE level IN ('info','debug'))::bigint AS info \
             FROM issues WHERE app_id=$1",
        )
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .get_result(conn)
        .await;
    }

    // $1 app_id, $2 env — always consumed here (`One`/`Subset` bind it;
    // `Unattributed`'s `IS NULL` fragment needs no bind, and `bind_env!`
    // below correspondingly binds nothing for it), reused by `lvl`'s own
    // `WHERE` fragment, same one-bind-reused-everywhere idiom as
    // `list_issues`.
    let env_sql = scope.env.sql_fragment_for("e", 2);
    let mut stmt = diesel::sql_query(format!(
        "SELECT count(*)::bigint AS total, \
           count(*) FILTER (WHERE i.status='unresolved')::bigint AS unresolved, \
           count(*) FILTER (WHERE i.status='resolved')::bigint AS resolved, \
           count(*) FILTER (WHERE i.status='ignored')::bigint AS ignored, \
           count(*) FILTER (WHERE lvl.level='fatal')::bigint AS fatal, \
           count(*) FILTER (WHERE lvl.level='error')::bigint AS error, \
           count(*) FILTER (WHERE lvl.level='warning')::bigint AS warning, \
           count(*) FILTER (WHERE lvl.level IN ('info','debug'))::bigint AS info \
         FROM issues i \
         JOIN LATERAL ( \
             SELECT e.level \
             FROM error_events e \
             WHERE e.issue_id = i.id{env_sql} \
             ORDER BY e.occurred_at DESC \
             LIMIT 1 \
         ) lvl ON TRUE \
         WHERE i.app_id=$1"
    ))
    .into_boxed()
    .bind::<SqlUuid, _>(scope.app_id);
    stmt = crate::bind_env!(stmt, &scope.env);
    stmt.get_result(conn).await
}

// ===========================================================================
// Event Explorer (raw analytics event stream with filters)
// ===========================================================================

/// Split a `parse_filters`-validated tag value (`key=value`) on the first `=`.
/// The value slot always contains exactly one leading `key=`, guaranteed by
/// `FieldType::Tag` validation, so the `None` arm is defensive only.
fn tag_kv(value: &str) -> (String, String) {
    match value.split_once('=') {
        Some((k, v)) => (k.to_string(), v.to_string()),
        None => (value.to_string(), String::new()),
    }
}

/// A single-key JSONB object `{key: value}` for a `tags @> …` containment bind.
fn tag_object(key: String, value: String) -> serde_json::Value {
    let mut m = serde_json::Map::new();
    m.insert(key, serde_json::Value::String(value));
    serde_json::Value::Object(m)
}

#[allow(clippy::too_many_arguments)]
pub async fn list_analytics_events(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    filters: &[ParsedFilter],
    q: Option<&str>,
    since: Option<chrono::DateTime<chrono::Utc>>,
    limit: i64,
    offset: i64,
) -> QueryResult<Vec<AnalyticsEvent>> {
    // Environment filters need a name->id lookup before the query is built.
    let mut env_eq: Option<Option<Uuid>> = None; // Some(id) filter present
    let mut env_neq: Option<Option<Uuid>> = None;
    for f in filters {
        if f.field == "environment" {
            // The id wanted here is the ENROLLMENT's, because that is what the
            // event tables store in `environment_id` — the catalogue entry is
            // only how a human names it.
            //
            // `retired_at IS NULL` on the enrollment is load-bearing: a name is
            // only unique among LIVE rows, so retiring `staging` and creating a
            // fresh `staging` leaves two enrollments reachable by that name.
            // Without this filter `.first()` returns an arbitrary one, and a
            // filter on the current `staging` could silently show only
            // pre-retirement events. Retiring a catalogue entry retires its
            // enrollments in the same transaction, so this single predicate
            // covers both levels and cannot disagree with itself.
            let id: Option<Uuid> = app_environments::table
                .inner_join(
                    environments::table.on(environments::id.eq(app_environments::environment_id)),
                )
                .filter(app_environments::app_id.eq(scope.app_id))
                .filter(environments::name.eq(&f.value))
                .filter(app_environments::retired_at.is_null())
                .select(app_environments::id)
                .first::<Uuid>(conn)
                .await
                .ok();
            match f.op {
                Op::Eq => env_eq = Some(id),
                Op::Neq => env_neq = Some(id),
                _ => {}
            }
        }
    }

    let mut query = analytics_events::table
        .filter(analytics_events::app_id.eq(scope.app_id))
        // Synthetic screen-view events belong to the Screens section, not the stream.
        .filter(analytics_events::name.ne("$screen"))
        .into_boxed();
    // The scope and the legacy `environment:eq/neq` chip (handled via `env_eq`/`env_neq`
    // below, after the per-filter loop) are both `.filter()` calls on the same boxed
    // query, so both are ANDed: the chip can only narrow within the scope, never widen
    // past it. That property comes from the AND, not from the order — applying the chip
    // first would emit identical SQL. It sits here simply to keep it adjacent to the
    // `app_id` filter it belongs with.
    //
    // Non-widening matters beyond tidiness: Slice 3 makes the environment scope an
    // access boundary, at which point a filter that could widen past it would be a
    // data leak rather than a wrong result.
    query = crate::scope_env!(query, analytics_events, &scope.env);
    if let Some(s) = since {
        query = query.filter(analytics_events::occurred_at.ge(s));
    }
    for f in filters {
        query = match (f.field, f.op) {
            ("name", Op::Eq) => query.filter(analytics_events::name.eq(f.value.clone())),
            ("name", Op::Neq) => query.filter(analytics_events::name.ne(f.value.clone())),
            ("name", Op::Contains) => {
                query.filter(analytics_events::name.ilike(like_contains(&f.value)))
            }
            ("distinct_id", Op::Eq) => {
                query.filter(analytics_events::distinct_id.eq(f.value.clone()))
            }
            ("distinct_id", Op::Neq) => {
                query.filter(analytics_events::distinct_id.ne(f.value.clone()))
            }
            ("distinct_id", Op::Contains) => {
                query.filter(analytics_events::distinct_id.ilike(like_contains(&f.value)))
            }
            ("session_id", Op::Eq) => {
                query.filter(analytics_events::session_id.eq(f.value.clone()))
            }
            ("session_id", Op::Neq) => {
                query.filter(analytics_events::session_id.ne(f.value.clone()))
            }
            ("session_id", Op::Contains) => {
                query.filter(analytics_events::session_id.ilike(like_contains(&f.value)))
            }
            ("release", Op::Eq) => query.filter(analytics_events::release.eq(f.value.clone())),
            ("release", Op::Neq) => query.filter(analytics_events::release.ne(f.value.clone())),
            ("release", Op::Contains) => {
                query.filter(analytics_events::release.ilike(like_contains(&f.value)))
            }
            ("tag", Op::Eq) => {
                let (k, v) = tag_kv(&f.value);
                query.filter(
                    sql::<Bool>("analytics_events.tags @> ").bind::<Jsonb, _>(tag_object(k, v)),
                )
            }
            ("tag", Op::Contains) => {
                let (k, v) = tag_kv(&f.value);
                query.filter(
                    sql::<Bool>("analytics_events.tags ->> ")
                        .bind::<Text, _>(k)
                        .sql(" ILIKE ")
                        .bind::<Text, _>(like_contains(&v)),
                )
            }
            // `workflow_id IS NOT NULL` alongside the name predicate is the
            // partial-index term: migration 2026-07-29-000032's
            // `analytics_events_app_workflow_idx` is
            // `WHERE workflow_id IS NOT NULL`, and Postgres uses a partial
            // index only when the query's WHERE *implies* that predicate —
            // `workflow_name = $N` does not. Semantically a no-op (the
            // pipeline stamps id and name together). This is the case Task 4
            // measured on the largest table in the system: 14 buffers / cost
            // 2,025 with the term vs 52,744 / 56,190 without.
            ("workflow", Op::Eq) => query
                .filter(analytics_events::workflow_id.is_not_null())
                .filter(analytics_events::workflow_name.eq(f.value.clone())),
            // `OR workflow_name IS NULL`, and deliberately NO
            // `workflow_id IS NOT NULL` term — see
            // `list_error_events_for_issue`'s `workflow` arms for the full
            // reasoning. Short version: `workflow` is one chip offered at
            // three levels (Events, Issues, occurrences), so it must mean the
            // same thing at each, and at the issue level `NOT EXISTS` already
            // makes an unstamped row match `neq`. The partial index is
            // deliberately forgone here: this arm's whole purpose is to RETURN
            // the unstamped rows, which are exactly the rows that index
            // excludes.
            ("workflow", Op::Neq) => query.filter(
                analytics_events::workflow_name
                    .ne(f.value.clone())
                    .or(analytics_events::workflow_name.is_null()),
            ),
            ("workflow", Op::Contains) => query
                .filter(analytics_events::workflow_id.is_not_null())
                .filter(analytics_events::workflow_name.ilike(like_contains(&f.value))),
            _ => query, // environment handled below; others unreachable
        };
    }
    // environment eq: unknown name -> no rows (filter on the impossible nil id).
    if let Some(id) = env_eq {
        query = match id {
            Some(id) => query.filter(analytics_events::environment_id.eq(id)),
            None => query.filter(analytics_events::environment_id.eq(Uuid::nil())),
        };
    }
    // environment neq: unknown name -> nothing to exclude.
    if let Some(Some(id)) = env_neq {
        query = query.filter(analytics_events::environment_id.ne(id));
    }
    if let Some(term) = q {
        let p = like_contains(term);
        query = query.filter(
            analytics_events::name
                .ilike(p.clone())
                .or(analytics_events::distinct_id.ilike(p.clone()))
                .or(sql::<Bool>("analytics_events.contexts::text ILIKE ")
                    .bind::<Text, _>(p.clone()))
                .or(sql::<Bool>("analytics_events.extra::text ILIKE ").bind::<Text, _>(p.clone()))
                .or(sql::<Bool>("analytics_events.properties::text ILIKE ")
                    .bind::<Text, _>(p.clone()))
                .or(sql::<Bool>("analytics_events.tags::text ILIKE ").bind::<Text, _>(p)),
        );
    }
    query
        .select(AnalyticsEvent::as_select())
        .order(analytics_events::occurred_at.desc())
        .limit(limit)
        .offset(offset)
        .load(conn)
        .await
}

// ===========================================================================
// Funnel (ordered multi-step conversion)
// ===========================================================================

#[derive(Debug, QueryableByName, serde::Serialize)]
pub struct FunnelStepCount {
    #[diesel(sql_type = BigInt)]
    pub step: i64,
    #[diesel(sql_type = BigInt)]
    pub count: i64,
}

/// Ordered funnel: how many distinct people did step 0, then step 1 at-or-after
/// their step-0 time, and so on. Built as a chained-CTE query over the steps.
pub async fn funnel(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    steps: &[String],
    since: DateTime<Utc>,
) -> QueryResult<Vec<FunnelStepCount>> {
    // $1 = app_id, $2 = since, $3 = env (only when scope.env is One), then each step name in
    // order starting at the next free index.
    //
    // The env predicate must apply to EVERY step's CTE, not just s0: each s{i} independently
    // re-reads `analytics_events`, so scoping only s0 would let a step-0 candidate whose
    // later step happened in a *different* environment count anyway — silently widening the
    // funnel past the selected environment instead of erroring. `s0` has no table alias
    // (bare `analytics_events`); `s{i>0}` aliases it `a` — `sql_fragment` (unqualified) is
    // right for the former, `sql_fragment_for("a", ..)` for the rest.
    let base_idx = if scope.env.consumes_bind() { 4 } else { 3 };
    let env_sql_bare = scope.env.sql_fragment(3);
    let env_sql_aliased = scope.env.sql_fragment_for("a", 3);

    let mut ctes: Vec<String> = Vec::new();
    let mut selects: Vec<String> = Vec::new();
    for i in 0..steps.len() {
        let name_param = i + base_idx;
        if i == 0 {
            ctes.push(format!(
                "s0 AS (SELECT distinct_id, min(occurred_at) AS t FROM analytics_events \
                 WHERE app_id=$1 AND occurred_at>=$2 AND name=${name_param}{env_sql_bare} GROUP BY distinct_id)"
            ));
        } else {
            let prev = i - 1;
            ctes.push(format!(
                "s{i} AS (SELECT a.distinct_id, min(a.occurred_at) AS t FROM analytics_events a \
                 JOIN s{prev} ON s{prev}.distinct_id = a.distinct_id \
                 WHERE a.app_id=$1 AND a.name=${name_param}{env_sql_aliased} AND a.occurred_at >= s{prev}.t \
                 GROUP BY a.distinct_id)"
            ));
        }
        selects.push(format!(
            "SELECT {i}::bigint AS step, (SELECT count(*) FROM s{i})::bigint AS count"
        ));
    }
    let sql = format!(
        "WITH {} {} ORDER BY step",
        ctes.join(", "),
        selects.join(" UNION ALL ")
    );

    let mut query = diesel::sql_query(sql)
        .into_boxed::<diesel::pg::Pg>()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Timestamptz, _>(since);
    query = crate::bind_env!(query, &scope.env);
    for step in steps {
        query = query.bind::<Text, _>(step.clone());
    }
    query.get_results(conn).await
}

// ===========================================================================
// Journeys (step-indexed transition graph for a Sankey)
// ===========================================================================

#[derive(Debug, QueryableByName, serde::Serialize, serde::Deserialize)]
pub struct JourneyLink {
    #[diesel(sql_type = BigInt)]
    pub from_step: i64,
    #[diesel(sql_type = Text)]
    pub from_event: String,
    #[diesel(sql_type = Text)]
    pub to_event: String,
    #[diesel(sql_type = BigInt)]
    pub count: i64,
}

#[derive(Debug, QueryableByName, serde::Serialize, serde::Deserialize)]
pub struct JourneyNode {
    #[diesel(sql_type = BigInt)]
    pub step: i64,
    #[diesel(sql_type = Text)]
    pub event: String,
    #[diesel(sql_type = BigInt)]
    pub count: i64,
}

/// Maximum node/link rows returned by a journey query.
///
/// Both result sets grow with event-name cardinality, which is caller-supplied
/// (every distinct `name` an SDK ever sent). Without a cap a high-cardinality
/// app produces an unbounded response.
const JOURNEY_MAX_ROWS: i64 = 500;

#[derive(Debug, QueryableByName)]
struct JourneyGraphRow {
    #[diesel(sql_type = Jsonb)]
    data: Value,
}

/// Nodes + links for the journey Sankey, computed in ONE query.
///
/// The step-indexed CTE (`row_number() OVER (PARTITION BY distinct_id ORDER BY
/// occurred_at)`) is the expensive part. Running separate node and link queries
/// evaluated it twice per page load; because `capped` is referenced more than
/// once here, Postgres materializes it and both aggregates read the same
/// intermediate. The `(app_id, distinct_id, occurred_at)` index lets the window
/// be satisfied by an ordered index scan rather than a full sort.
pub async fn journey_graph(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    since: DateTime<Utc>,
    depth: i64,
) -> QueryResult<(Vec<JourneyNode>, Vec<JourneyLink>)> {
    // $1 app_id, $2 since — env takes $3 when it needs a bind, which pushes depth/max_rows
    // from $3/$4 to $4/$5. Both indices are derived from the same `env_bind`/`env_sql` pair
    // so the string and the bind chain can't drift apart.
    let env_sql = scope.env.sql_fragment(3);
    let depth_idx = if scope.env.consumes_bind() { 4 } else { 3 };
    let max_rows_idx = depth_idx + 1;

    let q = format!(
        "WITH ordered AS ( \
           SELECT distinct_id, name, \
             (row_number() OVER (PARTITION BY distinct_id ORDER BY occurred_at) - 1) AS step \
           FROM analytics_events WHERE app_id=$1 AND occurred_at>=$2{env_sql}), \
         capped AS (SELECT * FROM ordered WHERE step < ${depth_idx}), \
         nodes AS ( \
           SELECT step, name AS event, count(*)::bigint AS count \
           FROM capped GROUP BY step, name ORDER BY step, count DESC LIMIT ${max_rows_idx}), \
         links AS ( \
           SELECT a.step AS from_step, a.name AS from_event, b.name AS to_event, \
                  count(*)::bigint AS count \
           FROM capped a JOIN capped b ON b.distinct_id=a.distinct_id AND b.step=a.step+1 \
           GROUP BY a.step, a.name, b.name ORDER BY a.step, count DESC LIMIT ${max_rows_idx}) \
         SELECT jsonb_build_object( \
           'nodes', COALESCE((SELECT jsonb_agg(to_jsonb(n)) FROM nodes n), '[]'::jsonb), \
           'links', COALESCE((SELECT jsonb_agg(to_jsonb(l)) FROM links l), '[]'::jsonb) \
         ) AS data"
    );

    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Timestamptz, _>(since);
    stmt = crate::bind_env!(stmt, &scope.env);
    let row: JourneyGraphRow = stmt
        .bind::<BigInt, _>(depth)
        .bind::<BigInt, _>(JOURNEY_MAX_ROWS)
        .get_result(conn)
        .await?;

    let nodes = row
        .data
        .get("nodes")
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    let links = row
        .data
        .get("links")
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    Ok((nodes, links))
}

// ===========================================================================
// Performance (percentile aggregates over transactions)
// ===========================================================================

#[derive(Debug, QueryableByName, serde::Serialize)]
pub struct PerfSummaryRow {
    #[diesel(sql_type = Text)]
    pub name: String,
    #[diesel(sql_type = Text)]
    pub op: String,
    #[diesel(sql_type = BigInt)]
    pub count: i64,
    #[diesel(sql_type = Double)]
    pub p50: f64,
    #[diesel(sql_type = Double)]
    pub p75: f64,
    #[diesel(sql_type = Double)]
    pub p95: f64,
    #[diesel(sql_type = Double)]
    pub p99: f64,
    #[diesel(sql_type = Double)]
    pub avg: f64,
    #[diesel(sql_type = Double)]
    pub error_rate: f64,
}

pub async fn performance_summary(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    since: DateTime<Utc>,
    op: Option<&str>,
    device_key: Option<&str>,
) -> QueryResult<Vec<PerfSummaryRow>> {
    // $1 app_id, $2 since, $3 op, $4 device_key (the pre-existing `(...::text IS NULL OR
    // ...)` optional-filter idiom — left untouched). Env is appended AFTER those, at the
    // next free index ($5), rather than interleaved among them, so $3/$4 never renumber and
    // there's no collision to reason about.
    let env_sql = scope.env.sql_fragment(5);
    let q = format!(
        "SELECT name, op, count(*)::bigint AS count, \
           percentile_cont(0.5)  WITHIN GROUP (ORDER BY duration_ms) AS p50, \
           percentile_cont(0.75) WITHIN GROUP (ORDER BY duration_ms) AS p75, \
           percentile_cont(0.95) WITHIN GROUP (ORDER BY duration_ms) AS p95, \
           percentile_cont(0.99) WITHIN GROUP (ORDER BY duration_ms) AS p99, \
           avg(duration_ms) AS avg, \
           (count(*) FILTER (WHERE status='error' OR http_status>=500))::float8 \
             / NULLIF(count(*),0) AS error_rate \
         FROM transactions \
         WHERE app_id=$1 AND occurred_at>=$2 \
           AND ($3::text IS NULL OR op=$3) AND ($4::text IS NULL OR device_key=$4){env_sql} \
         GROUP BY name, op ORDER BY count DESC LIMIT 100"
    );
    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Timestamptz, _>(since)
        .bind::<Nullable<Text>, _>(op)
        .bind::<Nullable<Text>, _>(device_key);
    stmt = crate::bind_env!(stmt, &scope.env);
    stmt.get_results(conn).await
}

#[derive(Debug, QueryableByName, serde::Serialize)]
pub struct PerfSeriesPoint {
    #[diesel(sql_type = Timestamptz)]
    pub bucket: DateTime<Utc>,
    #[diesel(sql_type = Double)]
    pub p50: f64,
    #[diesel(sql_type = Double)]
    pub p95: f64,
    #[diesel(sql_type = BigInt)]
    pub throughput: i64,
}

pub async fn performance_series(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    since: DateTime<Utc>,
    name: Option<&str>,
    op: Option<&str>,
) -> QueryResult<Vec<PerfSeriesPoint>> {
    // Same shape as `performance_summary`: env appended after the pre-existing $3/$4
    // optional-filter idiom, at the next free index ($5), so those two never renumber.
    let env_sql = scope.env.sql_fragment(5);
    let q = format!(
        "SELECT date_trunc('hour', occurred_at) AS bucket, \
           percentile_cont(0.5)  WITHIN GROUP (ORDER BY duration_ms) AS p50, \
           percentile_cont(0.95) WITHIN GROUP (ORDER BY duration_ms) AS p95, \
           count(*)::bigint AS throughput \
         FROM transactions \
         WHERE app_id=$1 AND occurred_at>=$2 \
           AND ($3::text IS NULL OR name=$3) AND ($4::text IS NULL OR op=$4){env_sql} \
         GROUP BY bucket ORDER BY bucket LIMIT 5000"
    );
    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Timestamptz, _>(since)
        .bind::<Nullable<Text>, _>(name)
        .bind::<Nullable<Text>, _>(op);
    stmt = crate::bind_env!(stmt, &scope.env);
    stmt.get_results(conn).await
}

// ---------------------------------------------------------------------------
// Audience & session-engagement analytics (feature A).
// ---------------------------------------------------------------------------

#[derive(Debug, QueryableByName, serde::Serialize)]
pub struct UserStats {
    #[diesel(sql_type = BigInt)]
    pub total_users: i64,
    #[diesel(sql_type = BigInt)]
    pub active_in_range: i64,
    #[diesel(sql_type = BigInt)]
    pub new_in_range: i64,
    #[diesel(sql_type = BigInt)]
    pub dau: i64,
    #[diesel(sql_type = BigInt)]
    pub wau: i64,
    #[diesel(sql_type = BigInt)]
    pub mau: i64,
    #[diesel(sql_type = Double)]
    pub avg_session_ms: f64,
    #[diesel(sql_type = Double)]
    pub median_session_ms: f64,
}

/// Aggregate audience stats for an app. `total_users`/`wau`/`mau` ignore `since`
/// (all-time / rolling-from-now); the rest are scoped to `since`.
///
/// `total_users`/`active_in_range`/`new_in_range` read `event_users`, which carries no
/// `environment_id` column at all — scoped by membership (see
/// `event_user_membership_exists`'s doc comment), the gap Task 8 deferred and this fix
/// closes. `total_users` has no `since` bound of its own, so membership is the *only*
/// predicate added to it under `One`/`Unattributed`. `new_in_range`'s existing
/// `first_seen>=$2` combined with membership is reading (a) — "globally-first-seen in the
/// window AND has activity in this environment" — not (b) ("first activity *in this
/// environment* falls in the window"); see `overview_totals`'s doc comment for the full
/// rationale and consequence, which applies identically here. Every other sub-select reads a
/// table that does carry `environment_id` (analytics_events/error_events for dau/wau/mau,
/// sessions for the two `*_session_ms` fields) and gets the real predicate, reused across
/// all 8 of those sub-selects via the same bind ($3, only when `scope.env` is `One`) that
/// `event_user_membership_exists` also reuses for its three `EXISTS` legs.
///
/// `now` is supplied by the caller rather than read from the database clock.
/// The 1/7/30-day literals are NOT the bug and must not become parameters:
/// `dau`/`wau`/`mau` mean those spans by definition and the dashboard tiles are
/// literally labelled "7-day"/"30-day", so repointing them at `since_days`
/// would make a user on the 90-day range read "MAU" as a 90-day count. The bug
/// was that three separate `now()` calls inside one statement are three
/// different instants, that this was the last read in the analytics path
/// anchored to the DATABASE clock, and that it was untestable without freezing
/// the server clock.
///
/// Known limitation, deliberately not fixed here: `user_stats` is HOT-TIER
/// ONLY, and its 30-day `mau` window is exactly the default `TIER_HOT_DAYS`, so
/// once `sauron-tier` has run that number silently loses its oldest days.
/// `GET /v1/projects/{id}/active-users` and its `truncated` flag are the
/// principled answer; this endpoint keeps the cheap behaviour and says so.
pub async fn user_stats(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    since: DateTime<Utc>,
    now: DateTime<Utc>,
) -> QueryResult<UserStats> {
    let env_sql = scope.env.sql_fragment(3);
    // `.clone()`, not a move — the final `bind_env!` call below still needs
    // `scope.env`; see `overview_totals`'s identical call for why.
    let membership_sql = event_user_membership_exists(scope.env.clone(), 3);
    // Derived from `consumes_bind()`, never assumed: `All` and `Unattributed`
    // reserve no bind, so the three cutoffs start at $3 for them and $4 for
    // `One`/`Subset`. Hardcoding either shifts every cutoff by one and silently
    // compares a timestamp against a uuid.
    let n = if scope.env.consumes_bind() { 4 } else { 3 };
    let (b1, b7, b30) = (n, n + 1, n + 2);
    let q = format!(
        "SELECT \
           (SELECT count(*) FROM event_users WHERE app_id=$1{membership_sql})::bigint AS total_users, \
           (SELECT count(*) FROM event_users WHERE app_id=$1 AND last_seen>=$2{membership_sql})::bigint AS active_in_range, \
           (SELECT count(*) FROM event_users WHERE app_id=$1 AND first_seen>=$2{membership_sql})::bigint AS new_in_range, \
           (SELECT count(DISTINCT distinct_id) FROM ( \
              SELECT distinct_id FROM analytics_events WHERE app_id=$1 AND occurred_at >= ${b1}{env_sql} AND distinct_id IS NOT NULL AND distinct_id <> '' \
              UNION ALL \
              SELECT distinct_id FROM error_events WHERE app_id=$1 AND occurred_at >= ${b1}{env_sql} AND distinct_id IS NOT NULL AND distinct_id <> '' \
            ) d1)::bigint AS dau, \
           (SELECT count(DISTINCT distinct_id) FROM ( \
              SELECT distinct_id FROM analytics_events WHERE app_id=$1 AND occurred_at >= ${b7}{env_sql} AND distinct_id IS NOT NULL AND distinct_id <> '' \
              UNION ALL \
              SELECT distinct_id FROM error_events WHERE app_id=$1 AND occurred_at >= ${b7}{env_sql} AND distinct_id IS NOT NULL AND distinct_id <> '' \
            ) d7)::bigint AS wau, \
           (SELECT count(DISTINCT distinct_id) FROM ( \
              SELECT distinct_id FROM analytics_events WHERE app_id=$1 AND occurred_at >= ${b30}{env_sql} AND distinct_id IS NOT NULL AND distinct_id <> '' \
              UNION ALL \
              SELECT distinct_id FROM error_events WHERE app_id=$1 AND occurred_at >= ${b30}{env_sql} AND distinct_id IS NOT NULL AND distinct_id <> '' \
            ) d30)::bigint AS mau, \
           COALESCE((SELECT avg(EXTRACT(EPOCH FROM (last_event_at - started_at)) * 1000) \
                     FROM sessions WHERE app_id=$1 AND last_event_at>=$2{env_sql}), 0)::double precision AS avg_session_ms, \
           COALESCE((SELECT percentile_cont(0.5) WITHIN GROUP (ORDER BY EXTRACT(EPOCH FROM (last_event_at - started_at)) * 1000) \
                     FROM sessions WHERE app_id=$1 AND last_event_at>=$2{env_sql}), 0)::double precision AS median_session_ms"
    );
    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Timestamptz, _>(since);
    // `bind_env!` sits BETWEEN `since` and the three cutoffs so positional
    // order matches the indices computed above.
    stmt = crate::bind_env!(stmt, &scope.env);
    stmt = stmt
        .bind::<Timestamptz, _>(now - chrono::Duration::days(1))
        .bind::<Timestamptz, _>(now - chrono::Duration::days(7))
        .bind::<Timestamptz, _>(now - chrono::Duration::days(30));
    stmt.get_result(conn).await
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct UserSeriesPoint {
    pub bucket: DateTime<Utc>,
    pub active: i64,
    pub new_users: i64,
}

/// Merge per-day active + per-day new counts into one sorted series, 0-filling
/// days present in only one input. Pure — unit-tested.
pub fn merge_user_series(active: Vec<SeriesPoint>, new: Vec<SeriesPoint>) -> Vec<UserSeriesPoint> {
    use std::collections::BTreeMap;
    let mut map: BTreeMap<DateTime<Utc>, (i64, i64)> = BTreeMap::new();
    for p in active {
        map.entry(p.bucket).or_default().0 = p.count;
    }
    for p in new {
        map.entry(p.bucket).or_default().1 = p.count;
    }
    map.into_iter()
        .map(|(bucket, (active, new_users))| UserSeriesPoint {
            bucket,
            active,
            new_users,
        })
        .collect()
}

/// Per-day distinct active users (analytics ∪ errors) and per-day new users,
/// merged. Both scoped to `since`.
///
/// `active` reads analytics_events/error_events (both carry `environment_id`) and gets the
/// real predicate. `new` reads `event_users`, which does not — scoped by membership (see
/// `event_user_membership_exists`'s doc comment), the gap Task 8 deferred and this fix
/// closes. Same reading-(a) semantics as `overview_totals.new_users`/`user_stats.new_in_range`
/// (globally-first-seen in the window AND a member of this environment, not first-seen-in-
/// this-environment) — see `overview_totals`'s doc comment for the full rationale.
pub async fn active_user_series(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    since: DateTime<Utc>,
) -> QueryResult<Vec<UserSeriesPoint>> {
    let env_sql = scope.env.sql_fragment(3);
    let active_q = format!(
        "SELECT date_trunc('day', occurred_at) AS bucket, count(DISTINCT distinct_id)::bigint AS count \
         FROM ( \
            SELECT occurred_at, distinct_id FROM analytics_events \
              WHERE app_id=$1 AND occurred_at>=$2{env_sql} AND distinct_id IS NOT NULL AND distinct_id <> '' \
            UNION ALL \
            SELECT occurred_at, distinct_id FROM error_events \
              WHERE app_id=$1 AND occurred_at>=$2{env_sql} AND distinct_id IS NOT NULL AND distinct_id <> '' \
         ) u \
         GROUP BY bucket ORDER BY bucket"
    );
    let mut active_stmt = diesel::sql_query(active_q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Timestamptz, _>(since);
    active_stmt = crate::bind_env!(active_stmt, &scope.env);
    let active: Vec<SeriesPoint> = active_stmt.get_results(conn).await?;

    // `.clone()`, not a move: the second `bind_env!` call below (for
    // `new_stmt`) still needs `scope.env` — see `overview_totals`'s identical
    // call for why `event_user_membership_exists` itself is not reshaped to
    // take `&EnvFilter` instead.
    let membership_sql = event_user_membership_exists(scope.env.clone(), 3);
    let new_q = format!(
        "SELECT date_trunc('day', first_seen) AS bucket, count(*)::bigint AS count \
         FROM event_users WHERE app_id=$1 AND first_seen>=$2{membership_sql} \
         GROUP BY bucket ORDER BY bucket"
    );
    let mut new_stmt = diesel::sql_query(new_q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Timestamptz, _>(since);
    new_stmt = crate::bind_env!(new_stmt, &scope.env);
    let new: Vec<SeriesPoint> = new_stmt.get_results(conn).await?;

    Ok(merge_user_series(active, new))
}

#[derive(Debug, QueryableByName, serde::Serialize)]
pub struct SessionStats {
    #[diesel(sql_type = BigInt)]
    pub sessions: i64,
    #[diesel(sql_type = BigInt)]
    pub crashed: i64,
    #[diesel(sql_type = Double)]
    pub avg_session_ms: f64,
    #[diesel(sql_type = Double)]
    pub median_session_ms: f64,
}

#[derive(Debug, QueryableByName, serde::Serialize)]
pub struct SeriesAvgPoint {
    #[diesel(sql_type = Timestamptz)]
    pub bucket: DateTime<Utc>,
    #[diesel(sql_type = Double)]
    pub avg_ms: f64,
}

#[derive(Debug, QueryableByName, serde::Serialize)]
pub struct HistoBucket {
    #[diesel(sql_type = Text)]
    pub bucket: String,
    #[diesel(sql_type = BigInt)]
    pub count: i64,
}

/// Duration-histogram bucket labels, in display order.
pub const DURATION_BUCKETS: [&str; 5] = ["<10s", "10-60s", "1-5m", "5-30m", "30m+"];

/// The SQL `CASE` mapping a duration in milliseconds (aliased `d`) onto
/// [`DURATION_BUCKETS`]' labels.
///
/// Shared by every duration histogram — `session_duration_histogram` and
/// `workflow_detail` — rather than copied into each, because the failure
/// mode of a divergent copy is *silent*: [`order_histogram`] matches these
/// labels against `DURATION_BUCKETS` by string equality and 0-fills anything
/// it doesn't recognise, so a single typo'd label here produces a
/// permanently all-zero bucket that no error, no type check and no `NULL`
/// ever reveals. `duration_bucket_case_emits_exactly_the_declared_labels`
/// below locks the SQL and the Rust array together.
const DURATION_BUCKET_CASE_SQL: &str = "CASE \
             WHEN d < 10000  THEN '<10s' \
             WHEN d < 60000  THEN '10-60s' \
             WHEN d < 300000 THEN '1-5m' \
             WHEN d < 1800000 THEN '5-30m' \
             ELSE '30m+' END";

/// Reorder DB histogram rows into the fixed bucket order, 0-filling gaps. Pure.
pub fn order_histogram(rows: Vec<HistoBucket>) -> Vec<HistoBucket> {
    DURATION_BUCKETS
        .iter()
        .map(|label| {
            let count = rows
                .iter()
                .find(|r| r.bucket == *label)
                .map(|r| r.count)
                .unwrap_or(0);
            HistoBucket {
                bucket: (*label).to_string(),
                count,
            }
        })
        .collect()
}

pub async fn session_stats(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    since: DateTime<Utc>,
) -> QueryResult<SessionStats> {
    // $1 app_id, $2 since, reused across all four sub-selects, all against `sessions` — env
    // takes $3 when it needs a bind, reused the same way.
    //
    // `crashed` has the same known mislabelling gap as `overview_totals`'
    // `crashed_sessions` — see `bump_session`'s doc comment and
    // `.superpowers/sdd/2026-07-29-environment-rbac-scope/task-10-report.md`.
    let env_sql = scope.env.sql_fragment(3);
    let q = format!(
        "SELECT \
           (SELECT count(*) FROM sessions WHERE app_id=$1 AND last_event_at>=$2{env_sql})::bigint AS sessions, \
           (SELECT count(*) FROM sessions WHERE app_id=$1 AND last_event_at>=$2 AND errors_count>0{env_sql})::bigint AS crashed, \
           COALESCE((SELECT avg(EXTRACT(EPOCH FROM (last_event_at - started_at)) * 1000) \
                     FROM sessions WHERE app_id=$1 AND last_event_at>=$2{env_sql}), 0)::double precision AS avg_session_ms, \
           COALESCE((SELECT percentile_cont(0.5) WITHIN GROUP (ORDER BY EXTRACT(EPOCH FROM (last_event_at - started_at)) * 1000) \
                     FROM sessions WHERE app_id=$1 AND last_event_at>=$2{env_sql}), 0)::double precision AS median_session_ms"
    );
    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Timestamptz, _>(since);
    stmt = crate::bind_env!(stmt, &scope.env);
    stmt.get_result(conn).await
}

pub async fn session_duration_series(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    since: DateTime<Utc>,
) -> QueryResult<Vec<SeriesAvgPoint>> {
    let env_sql = scope.env.sql_fragment(3);
    let q = format!(
        "SELECT date_trunc('day', started_at) AS bucket, \
                COALESCE(avg(EXTRACT(EPOCH FROM (last_event_at - started_at)) * 1000), 0)::double precision AS avg_ms \
         FROM sessions WHERE app_id=$1 AND started_at>=$2{env_sql} \
         GROUP BY bucket ORDER BY bucket"
    );
    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Timestamptz, _>(since);
    stmt = crate::bind_env!(stmt, &scope.env);
    stmt.get_results(conn).await
}

pub async fn session_duration_histogram(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    since: DateTime<Utc>,
) -> QueryResult<Vec<HistoBucket>> {
    let env_sql = scope.env.sql_fragment(3);
    let q = format!(
        "SELECT bucket, count(*)::bigint AS count FROM ( \
           SELECT {DURATION_BUCKET_CASE_SQL} AS bucket \
           FROM (SELECT EXTRACT(EPOCH FROM (last_event_at - started_at)) * 1000 AS d \
                 FROM sessions WHERE app_id=$1 AND last_event_at>=$2{env_sql}) s \
         ) b GROUP BY bucket"
    );
    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Timestamptz, _>(since);
    stmt = crate::bind_env!(stmt, &scope.env);
    let rows: Vec<HistoBucket> = stmt.get_results(conn).await?;
    Ok(order_histogram(rows))
}

#[cfg(test)]
mod user_series_tests {
    use super::{merge_user_series, SeriesPoint};
    use chrono::{TimeZone, Utc};

    fn pt(day: u32, count: i64) -> SeriesPoint {
        SeriesPoint {
            bucket: Utc.with_ymd_and_hms(2026, 7, day, 0, 0, 0).unwrap(),
            count,
        }
    }

    #[test]
    fn merges_active_and_new_by_day_zero_filling() {
        let active = vec![pt(1, 10), pt(2, 8)];
        let new = vec![pt(2, 3), pt(3, 5)]; // day 1 has no new; day 3 has no active
        let out = merge_user_series(active, new);
        let got: Vec<(u32, i64, i64)> = out
            .iter()
            .map(|p| {
                (
                    p.bucket.format("%d").to_string().parse().unwrap(),
                    p.active,
                    p.new_users,
                )
            })
            .collect();
        assert_eq!(got, vec![(1, 10, 0), (2, 8, 3), (3, 0, 5)]);
    }

    #[test]
    fn empty_inputs_yield_empty() {
        assert!(merge_user_series(vec![], vec![]).is_empty());
    }
}

#[cfg(test)]
mod histogram_tests {
    use super::{order_histogram, HistoBucket, DURATION_BUCKETS};

    fn b(bucket: &str, count: i64) -> HistoBucket {
        HistoBucket {
            bucket: bucket.to_string(),
            count,
        }
    }

    #[test]
    fn fills_missing_buckets_in_fixed_order() {
        let rows = vec![b("30m+", 2), b("<10s", 5)];
        let out = order_histogram(rows);
        let got: Vec<(&str, i64)> = out.iter().map(|h| (h.bucket.as_str(), h.count)).collect();
        assert_eq!(
            got,
            vec![
                ("<10s", 5),
                ("10-60s", 0),
                ("1-5m", 0),
                ("5-30m", 0),
                ("30m+", 2)
            ]
        );
        assert_eq!(out.len(), DURATION_BUCKETS.len());
    }

    /// The SQL `CASE` and the Rust label array must agree exactly.
    ///
    /// This is the only thing standing between a typo'd SQL label and a
    /// permanently all-zero histogram bucket: `order_histogram` matches by
    /// string equality and 0-fills whatever it doesn't recognise, so a
    /// divergence produces no error, no `NULL`, and no type failure — just a
    /// bucket that is always empty, in every environment, forever. Asserting
    /// on the extracted string literals (rather than eyeballing the two) is
    /// what makes the coupling mechanical.
    #[test]
    fn duration_bucket_case_emits_exactly_the_declared_labels() {
        // Pull every `'...'` literal out of the CASE's THEN/ELSE arms.
        let emitted: Vec<&str> = super::DURATION_BUCKET_CASE_SQL
            .match_indices('\'')
            .collect::<Vec<_>>()
            .chunks(2)
            .filter(|pair| pair.len() == 2)
            .map(|pair| {
                let start = pair[0].0 + 1;
                let end = pair[1].0;
                &super::DURATION_BUCKET_CASE_SQL[start..end]
            })
            .collect();
        assert_eq!(
            emitted,
            DURATION_BUCKETS.to_vec(),
            "the CASE's labels must match DURATION_BUCKETS exactly, in order — \
             a mismatch silently 0-fills that bucket instead of erroring"
        );
    }
}

// ===========================================================================
// Saved funnels (persisted, app-scoped funnel templates)
// ===========================================================================

#[derive(Debug, QueryableByName, serde::Serialize)]
pub struct SavedFunnelRow {
    #[diesel(sql_type = SqlUuid)]
    pub id: Uuid,
    #[diesel(sql_type = SqlUuid)]
    pub app_id: Uuid,
    #[diesel(sql_type = Text)]
    pub name: String,
    #[diesel(sql_type = Nullable<Text>)]
    pub description: Option<String>,
    #[diesel(sql_type = Jsonb)]
    pub steps: Value,
    #[diesel(sql_type = Nullable<Text>)]
    pub created_by_name: Option<String>,
    #[diesel(sql_type = Timestamptz)]
    pub created_at: DateTime<Utc>,
    #[diesel(sql_type = Timestamptz)]
    pub updated_at: DateTime<Utc>,
}

const SAVED_FUNNEL_SELECT: &str = "SELECT sf.id, sf.app_id, sf.name, sf.description, sf.steps, \
    u.name AS created_by_name, sf.created_at, sf.updated_at \
    FROM saved_funnels sf LEFT JOIN users u ON u.id = sf.created_by ";

pub async fn list_saved_funnels(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
) -> QueryResult<Vec<SavedFunnelRow>> {
    // Bounded: saved funnels are user-created and otherwise unlimited.
    diesel::sql_query(format!(
        "{SAVED_FUNNEL_SELECT} WHERE sf.app_id=$1 ORDER BY sf.updated_at DESC LIMIT 500"
    ))
    .bind::<SqlUuid, _>(app_id)
    .get_results(conn)
    .await
}

pub async fn create_saved_funnel(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    created_by: Uuid,
    name: &str,
    description: Option<&str>,
    steps: &Value,
) -> QueryResult<SavedFunnelRow> {
    diesel::sql_query(format!(
        "WITH ins AS ( \
           INSERT INTO saved_funnels (app_id, name, description, steps, created_by) \
           VALUES ($1, $2, $3, $4, $5) RETURNING * \
         ) {} FROM ins sf LEFT JOIN users u ON u.id = sf.created_by",
        // reuse the same projection but from the CTE
        "SELECT sf.id, sf.app_id, sf.name, sf.description, sf.steps, u.name AS created_by_name, sf.created_at, sf.updated_at"
    ))
    .bind::<SqlUuid, _>(app_id)
    .bind::<Text, _>(name)
    .bind::<Nullable<Text>, _>(description)
    .bind::<Jsonb, _>(steps)
    .bind::<SqlUuid, _>(created_by)
    .get_result(conn)
    .await
}

/// Returns number of rows updated (0 → not found / wrong app).
pub async fn update_saved_funnel(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    id: Uuid,
    name: &str,
    description: Option<&str>,
    steps: &Value,
) -> QueryResult<usize> {
    diesel::sql_query(
        "UPDATE saved_funnels SET name=$3, description=$4, steps=$5, updated_at=now() \
         WHERE app_id=$1 AND id=$2",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<SqlUuid, _>(id)
    .bind::<Text, _>(name)
    .bind::<Nullable<Text>, _>(description)
    .bind::<Jsonb, _>(steps)
    .execute(conn)
    .await
}

/// Returns number of rows deleted (0 → not found / wrong app).
pub async fn delete_saved_funnel(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    id: Uuid,
) -> QueryResult<usize> {
    diesel::sql_query("DELETE FROM saved_funnels WHERE app_id=$1 AND id=$2")
        .bind::<SqlUuid, _>(app_id)
        .bind::<SqlUuid, _>(id)
        .execute(conn)
        .await
}

// ===========================================================================
// Screens (on-read per-screen metrics + capped dwell, app-scoped)
// ===========================================================================

#[derive(Debug, QueryableByName, serde::Serialize)]
pub struct ScreenRow {
    #[diesel(sql_type = Text)]
    pub screen: String,
    #[diesel(sql_type = BigInt)]
    pub views: i64,
    #[diesel(sql_type = BigInt)]
    pub events: i64,
    #[diesel(sql_type = BigInt)]
    pub exceptions: i64,
    #[diesel(sql_type = BigInt)]
    pub users: i64,
    #[diesel(sql_type = Double)]
    pub avg_dwell_ms: f64,
}

#[derive(Debug, QueryableByName, serde::Serialize)]
pub struct ScreenStats {
    #[diesel(sql_type = Text)]
    pub screen: String,
    #[diesel(sql_type = BigInt)]
    pub views: i64,
    #[diesel(sql_type = BigInt)]
    pub events: i64,
    #[diesel(sql_type = BigInt)]
    pub exceptions: i64,
    #[diesel(sql_type = BigInt)]
    pub users: i64,
    #[diesel(sql_type = Double)]
    pub total_dwell_ms: f64,
    #[diesel(sql_type = Double)]
    pub avg_dwell_ms: f64,
}

/// total dwell / views, guarding views=0. Pure.
pub fn avg_dwell(total_ms: f64, views: i64) -> f64 {
    if views > 0 {
        total_ms / views as f64
    } else {
        0.0
    }
}

// Shared CTE fragment: per-screen views/events/users/exceptions/dwell. $1 app, $2 since.

/// Build the screen CTEs with `pred` (a compile-time SQL fragment, never user
/// data) narrowing which screens are aggregated, and `env_sql` (an
/// [`EnvFilter::sql_fragment`]/`sql_fragment_for` output, e.g. `" AND
/// environment_id = $4"` or `""`) narrowing which environment's rows feed
/// them. There is no `screens` table — every column here derives from
/// `analytics_events`/`error_events`, both of which carry `environment_id`,
/// so `env_sql` must reach all four CTEs or a scoped read silently mixes
/// environments in whichever one it missed.
///
/// `ev`/`ex`/`us` push both predicates into their own WHERE clauses — `us`
/// has **two** arms (one per table) inside its `UNION ALL`, and both need
/// `env_sql` independently. Previously both callers aggregated **every**
/// screen in the app and filtered only in the outer query — so the
/// single-screen detail view computed the whole app's stats to return one
/// row, and the list paginated after full aggregation.
///
/// `dw` is deliberately NOT narrowed by `pred` (the screen filter) inside the
/// window: dwell is measured to the next event in the session *whatever
/// screen it is on*, so restricting the window input by screen would compute
/// the wrong gaps. `pred` is applied after `LEAD`, on the outer query, which
/// preserves the value while still shrinking the grouping.
///
/// `env_sql`, however, MUST go inside the inner subquery that computes
/// `raw_ms`, not the outer `WHERE` — unlike `pred`. Two reasons, one loud and
/// one silent:
/// - The outer query only has `g.screen`/`g.raw_ms` in scope (that's all the
///   inner subquery selects), so `environment_id = $N` in the outer `WHERE`
///   is a hard, self-detecting SQL error (no such column).
/// - Even if it *could* resolve, filtering after `LEAD` would still compute
///   dwell gaps using next-events from every environment, then merely hide
///   the *result* rows outside the requested one — the boundary itself would
///   already be crossed. Filtering the rows `LEAD` sees, before the window
///   runs, is what keeps a session's dwell gaps from crossing environments in
///   the first place (a session's own events are expected to share one
///   environment, matching `pred`'s screen-membership semantics: restrict
///   *inputs*, not just outputs, for correctness).
fn screen_ctes(pred: &str, env_sql: &str) -> String {
    format!(
        "WITH ev AS ( \
        SELECT screen, \
          count(*) FILTER (WHERE name='$screen')::bigint AS views, \
          count(*) FILTER (WHERE name<>'$screen')::bigint AS events \
        FROM analytics_events WHERE app_id=$1 AND occurred_at>=$2 AND screen IS NOT NULL AND {pred}{env_sql} GROUP BY screen), \
      ex AS ( \
        SELECT screen, count(*)::bigint AS exceptions \
        FROM error_events WHERE app_id=$1 AND occurred_at>=$2 AND screen IS NOT NULL AND {pred}{env_sql} GROUP BY screen), \
      us AS ( \
        SELECT screen, count(DISTINCT distinct_id)::bigint AS users FROM ( \
          SELECT screen, distinct_id FROM analytics_events WHERE app_id=$1 AND occurred_at>=$2 AND screen IS NOT NULL AND {pred}{env_sql} AND distinct_id IS NOT NULL AND distinct_id<>'' \
          UNION ALL \
          SELECT screen, distinct_id FROM error_events WHERE app_id=$1 AND occurred_at>=$2 AND screen IS NOT NULL AND {pred}{env_sql} AND distinct_id IS NOT NULL AND distinct_id<>'' \
        ) u GROUP BY screen), \
      dw AS ( \
        SELECT screen, sum(LEAST(raw_ms, 1800000))::double precision AS total_dwell_ms FROM ( \
          SELECT screen, EXTRACT(EPOCH FROM ( \
            LEAD(occurred_at) OVER (PARTITION BY session_id ORDER BY occurred_at) - occurred_at)) * 1000 AS raw_ms \
          FROM analytics_events WHERE app_id=$1 AND occurred_at>=$2 AND session_id IS NOT NULL AND screen IS NOT NULL{env_sql}) g \
        WHERE raw_ms IS NOT NULL AND raw_ms > 0 AND {pred} GROUP BY screen), \
      keys AS (SELECT screen FROM ev UNION SELECT screen FROM ex) "
    )
}

/// Predicate for the single-screen detail view.
const SCREEN_PRED_EXACT: &str = "screen = $3";
/// Predicate for the paginated list (`$3` is an escaped ILIKE pattern).
const SCREEN_PRED_LIKE: &str = "screen ILIKE $3";

pub async fn screen_list(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    since: DateTime<Utc>,
    q_pattern: &str, // '%' for no filter, else like_contains(term)
    limit: i64,
    offset: i64,
) -> QueryResult<Vec<ScreenRow>> {
    // $1 app_id, $2 since, $3 q_pattern (SCREEN_PRED_LIKE's own bind) — env
    // takes $4 when it needs a bind, which pushes limit/offset from $4/$5 to
    // $5/$6. Both indices derive from the same `env_bind`/`env_sql` pair, the
    // same "trailing-index shift" idiom `top_events`/`journey_graph` use.
    let env_sql = scope.env.sql_fragment(4);
    let limit_idx = if scope.env.consumes_bind() { 5 } else { 4 };
    let offset_idx = limit_idx + 1;
    let q = format!(
        "{} \
         SELECT k.screen, \
           COALESCE(ev.views,0)::bigint AS views, \
           COALESCE(ev.events,0)::bigint AS events, \
           COALESCE(ex.exceptions,0)::bigint AS exceptions, \
           COALESCE(us.users,0)::bigint AS users, \
           COALESCE(COALESCE(dw.total_dwell_ms,0) / NULLIF(COALESCE(ev.views,0),0), 0)::double precision AS avg_dwell_ms \
         FROM keys k \
         LEFT JOIN ev ON ev.screen=k.screen LEFT JOIN ex ON ex.screen=k.screen \
         LEFT JOIN us ON us.screen=k.screen LEFT JOIN dw ON dw.screen=k.screen \
         ORDER BY views DESC, k.screen ASC LIMIT ${limit_idx} OFFSET ${offset_idx}",
        screen_ctes(SCREEN_PRED_LIKE, &env_sql)
    );
    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Timestamptz, _>(since)
        .bind::<Text, _>(q_pattern);
    stmt = crate::bind_env!(stmt, &scope.env);
    stmt.bind::<BigInt, _>(limit)
        .bind::<BigInt, _>(offset)
        .get_results(conn)
        .await
}

pub async fn screen_stats(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    since: DateTime<Utc>,
    name: &str,
) -> QueryResult<ScreenStats> {
    // $1 app_id, $2 since, $3 name (SCREEN_PRED_EXACT's own bind) — env takes
    // $4 when it needs a bind. No trailing binds after it, so unlike
    // `screen_list` nothing needs to shift.
    let env_sql = scope.env.sql_fragment(4);
    let q = format!(
        "{} \
         SELECT k.screen, \
           COALESCE(ev.views,0)::bigint AS views, \
           COALESCE(ev.events,0)::bigint AS events, \
           COALESCE(ex.exceptions,0)::bigint AS exceptions, \
           COALESCE(us.users,0)::bigint AS users, \
           COALESCE(dw.total_dwell_ms,0)::double precision AS total_dwell_ms, \
           COALESCE(COALESCE(dw.total_dwell_ms,0) / NULLIF(COALESCE(ev.views,0),0), 0)::double precision AS avg_dwell_ms \
         FROM keys k \
         LEFT JOIN ev ON ev.screen=k.screen LEFT JOIN ex ON ex.screen=k.screen \
         LEFT JOIN us ON us.screen=k.screen LEFT JOIN dw ON dw.screen=k.screen \
         WHERE k.screen = $3",
        screen_ctes(SCREEN_PRED_EXACT, &env_sql)
    );
    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Timestamptz, _>(since)
        .bind::<Text, _>(name);
    stmt = crate::bind_env!(stmt, &scope.env);
    stmt.get_result(conn).await
}

pub async fn recent_events_for_screen(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    screen: &str,
    since: DateTime<Utc>,
    limit: i64,
) -> QueryResult<Vec<AnalyticsEvent>> {
    let q = analytics_events::table
        .filter(analytics_events::app_id.eq(scope.app_id))
        .filter(analytics_events::screen.eq(screen))
        .filter(analytics_events::occurred_at.ge(since))
        .filter(analytics_events::name.ne("$screen"))
        .into_boxed();
    crate::scope_env!(q, analytics_events, &scope.env)
        .select(AnalyticsEvent::as_select())
        .order(analytics_events::occurred_at.desc())
        .limit(limit)
        .load(conn)
        .await
}

pub async fn recent_exceptions_for_screen(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    screen: &str,
    since: DateTime<Utc>,
    limit: i64,
) -> QueryResult<Vec<ErrorEvent>> {
    let q = error_events::table
        .filter(error_events::app_id.eq(scope.app_id))
        .filter(error_events::screen.eq(screen))
        .filter(error_events::occurred_at.ge(since))
        .into_boxed();
    crate::scope_env!(q, error_events, &scope.env)
        .select(ErrorEvent::as_select())
        .order(error_events::occurred_at.desc())
        .limit(limit)
        .load(conn)
        .await
}

#[cfg(test)]
mod avg_dwell_tests {
    use super::avg_dwell;

    #[test]
    fn divides_total_by_views() {
        assert!((avg_dwell(9000.0, 3) - 3000.0).abs() < 1e-9);
    }

    #[test]
    fn zero_views_is_zero() {
        assert_eq!(avg_dwell(9000.0, 0), 0.0);
    }
}

// ===========================================================================
// Monitors (uptime checks, keyed by project_id)
// ===========================================================================

#[derive(QueryableByName, serde::Serialize)]
pub struct MonitorListRow {
    #[diesel(sql_type = SqlUuid)]
    pub id: Uuid,
    #[diesel(sql_type = Text)]
    pub name: String,
    #[diesel(sql_type = Text)]
    pub kind: String,
    #[diesel(sql_type = Text)]
    pub target: String,
    #[diesel(sql_type = Text)]
    pub status: String,
    #[diesel(sql_type = Bool)]
    pub enabled: bool,
    #[diesel(sql_type = Nullable<Integer>)]
    pub last_response_time_ms: Option<i32>,
    #[diesel(sql_type = Nullable<Timestamptz>)]
    pub last_checked_at: Option<DateTime<Utc>>,
    #[diesel(sql_type = Nullable<Double>)]
    pub uptime_24h: Option<f64>,
}

#[derive(QueryableByName, serde::Serialize)]
pub struct CheckPoint {
    #[diesel(sql_type = Timestamptz)]
    pub checked_at: DateTime<Utc>,
    #[diesel(sql_type = Bool)]
    pub up: bool,
    #[diesel(sql_type = Nullable<Integer>)]
    pub response_time_ms: Option<i32>,
    #[diesel(sql_type = Nullable<Integer>)]
    pub status_code: Option<i32>,
    #[diesel(sql_type = Nullable<Text>)]
    pub error: Option<String>,
}

/// How many monitors a single project may have.
///
/// Each enabled monitor is polled on its own interval by every prober, so the
/// count directly sets sustained load on the prober fleet and the database.
pub const MAX_MONITORS_PER_PROJECT: i64 = 100;

/// Current monitor count for a project.
pub async fn count_monitors_for_project(
    conn: &mut AsyncPgConnection,
    project_id: Uuid,
) -> QueryResult<i64> {
    monitors::table
        .filter(monitors::project_id.eq(project_id))
        .count()
        .get_result(conn)
        .await
}

pub async fn create_monitor(
    conn: &mut AsyncPgConnection,
    m: NewMonitor<'_>,
) -> QueryResult<Monitor> {
    diesel::insert_into(monitors::table)
        .values(m)
        .returning(Monitor::as_returning())
        .get_result(conn)
        .await
}

pub async fn get_monitor(conn: &mut AsyncPgConnection, id: Uuid) -> QueryResult<Option<Monitor>> {
    monitors::table
        .find(id)
        .select(Monitor::as_select())
        .first(conn)
        .await
        .optional()
}

pub async fn monitor_project(conn: &mut AsyncPgConnection, id: Uuid) -> QueryResult<Option<Uuid>> {
    monitors::table
        .find(id)
        .select(monitors::project_id)
        .first(conn)
        .await
        .optional()
}

pub async fn delete_monitor(conn: &mut AsyncPgConnection, id: Uuid) -> QueryResult<usize> {
    diesel::delete(monitors::table.find(id)).execute(conn).await
}

pub async fn list_incidents(
    conn: &mut AsyncPgConnection,
    monitor_id: Uuid,
    limit: i64,
) -> QueryResult<Vec<MonitorIncidentRow>> {
    monitor_incidents::table
        .filter(monitor_incidents::monitor_id.eq(monitor_id))
        .select(MonitorIncidentRow::as_select())
        .order(monitor_incidents::started_at.desc())
        .limit(limit)
        .load(conn)
        .await
}

pub async fn list_monitors_for_project(
    conn: &mut AsyncPgConnection,
    project_id: Uuid,
) -> QueryResult<Vec<MonitorListRow>> {
    diesel::sql_query(
        "SELECT m.id, m.name, m.kind, m.target, m.status, m.enabled, \
                lc.response_time_ms AS last_response_time_ms, m.last_checked_at, \
                up.pct AS uptime_24h \
         FROM monitors m \
         LEFT JOIN LATERAL ( \
             SELECT response_time_ms FROM monitor_checks c \
             WHERE c.monitor_id = m.id ORDER BY c.checked_at DESC LIMIT 1 \
         ) lc ON TRUE \
         LEFT JOIN LATERAL ( \
             SELECT (100.0 * avg(CASE WHEN c.up THEN 1 ELSE 0 END))::double precision AS pct \
             FROM monitor_checks c \
             WHERE c.monitor_id = m.id AND c.checked_at >= now() - interval '24 hours' \
         ) up ON TRUE \
         WHERE m.project_id = $1 \
         ORDER BY m.created_at ASC",
    )
    .bind::<SqlUuid, _>(project_id)
    .get_results(conn)
    .await
}

#[derive(QueryableByName)]
struct PctRow {
    #[diesel(sql_type = Nullable<Double>)]
    pct: Option<f64>,
}

pub async fn uptime_pct(
    conn: &mut AsyncPgConnection,
    monitor_id: Uuid,
    since_hours: i64,
) -> QueryResult<Option<f64>> {
    let row: PctRow = diesel::sql_query(
        "SELECT (100.0 * avg(CASE WHEN up THEN 1 ELSE 0 END))::double precision AS pct FROM monitor_checks \
         WHERE monitor_id = $1 AND checked_at >= now() - ($2 || ' hours')::interval",
    )
    .bind::<SqlUuid, _>(monitor_id)
    .bind::<Text, _>(since_hours.to_string())
    .get_result(conn)
    .await?;
    Ok(row.pct)
}

pub async fn latency_series(
    conn: &mut AsyncPgConnection,
    monitor_id: Uuid,
    since_hours: i64,
) -> QueryResult<Vec<CheckPoint>> {
    diesel::sql_query(
        "SELECT checked_at, up, response_time_ms, status_code, error FROM monitor_checks \
         WHERE monitor_id = $1 AND checked_at >= now() - ($2 || ' hours')::interval \
         ORDER BY checked_at ASC",
    )
    .bind::<SqlUuid, _>(monitor_id)
    .bind::<Text, _>(since_hours.to_string())
    .get_results(conn)
    .await
}

pub async fn prune_checks(
    conn: &mut AsyncPgConnection,
    older_than_days: i64,
) -> QueryResult<usize> {
    diesel::sql_query(
        "DELETE FROM monitor_checks WHERE checked_at < now() - ($1 || ' days')::interval",
    )
    .bind::<Text, _>(older_than_days.to_string())
    .execute(conn)
    .await
}

/// Delete `alert_events` rows older than `older_than_days`.
///
/// This table is an audit log that grows on every *evaluation*, not just every
/// delivery: a throttled rule writes a `throttled` row each tick it suppresses.
/// A 30s tick on a handful of flapping rules is millions of rows a year, with
/// nothing reclaiming them.
pub async fn prune_alert_events(
    conn: &mut AsyncPgConnection,
    older_than_days: i64,
) -> QueryResult<usize> {
    diesel::sql_query(
        "DELETE FROM alert_events WHERE created_at < now() - ($1 || ' days')::interval",
    )
    .bind::<Text, _>(older_than_days.to_string())
    .execute(conn)
    .await
}

/// Atomically claim due monitors and push their next_check_at forward so no
/// other prober picks the same rows. Returns the claimed rows to probe.
pub async fn claim_due_monitors(
    conn: &mut AsyncPgConnection,
    batch: i64,
) -> QueryResult<Vec<Monitor>> {
    diesel::sql_query(
        "UPDATE monitors SET next_check_at = now() + make_interval(secs => interval_seconds), \
                last_checked_at = now() \
         WHERE id IN ( \
             SELECT id FROM monitors \
             WHERE enabled AND status <> 'paused' AND next_check_at <= now() \
             ORDER BY next_check_at FOR UPDATE SKIP LOCKED LIMIT $1 \
         ) RETURNING *",
    )
    .bind::<BigInt, _>(batch)
    .get_results(conn)
    .await
}

/// Persist one probe result: insert the check row and update the monitor's
/// counters + status. `new_status` is the state machine's decision.
#[allow(clippy::too_many_arguments)]
pub async fn record_check_and_state(
    conn: &mut AsyncPgConnection,
    monitor_id: Uuid,
    up: bool,
    status_code: Option<i32>,
    response_time_ms: Option<i32>,
    error: Option<&str>,
    new_status: &str,
    consecutive_failures: i32,
    consecutive_successes: i32,
    status_changed: bool,
) -> QueryResult<()> {
    diesel::sql_query(
        "INSERT INTO monitor_checks (monitor_id, up, status_code, response_time_ms, error) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind::<SqlUuid, _>(monitor_id)
    .bind::<Bool, _>(up)
    .bind::<Nullable<Integer>, _>(status_code)
    .bind::<Nullable<Integer>, _>(response_time_ms)
    .bind::<Nullable<Text>, _>(error)
    .execute(conn)
    .await?;

    diesel::sql_query(
        "UPDATE monitors SET status = $2, consecutive_failures = $3, consecutive_successes = $4, \
                updated_at = now(), \
                last_status_changed_at = CASE WHEN $5 THEN now() ELSE last_status_changed_at END \
         WHERE id = $1",
    )
    .bind::<SqlUuid, _>(monitor_id)
    .bind::<Text, _>(new_status)
    .bind::<Integer, _>(consecutive_failures)
    .bind::<Integer, _>(consecutive_successes)
    .bind::<Bool, _>(status_changed)
    .execute(conn)
    .await?;
    Ok(())
}

#[derive(QueryableByName)]
struct IdRow {
    #[diesel(sql_type = SqlUuid)]
    id: Uuid,
}

pub async fn open_incident(
    conn: &mut AsyncPgConnection,
    monitor_id: Uuid,
    cause: &str,
    last_error: Option<&str>,
) -> QueryResult<Uuid> {
    // ON CONFLICT on the partial unique index: if an incident is already open,
    // keep it and just refresh last_error.
    let row: IdRow = diesel::sql_query(
        "INSERT INTO monitor_incidents (monitor_id, cause, last_error) VALUES ($1, $2, $3) \
         ON CONFLICT (monitor_id) WHERE resolved_at IS NULL \
         DO UPDATE SET last_error = EXCLUDED.last_error RETURNING id",
    )
    .bind::<SqlUuid, _>(monitor_id)
    .bind::<Text, _>(cause)
    .bind::<Nullable<Text>, _>(last_error)
    .get_result(conn)
    .await?;
    Ok(row.id)
}

pub async fn resolve_incident(conn: &mut AsyncPgConnection, monitor_id: Uuid) -> QueryResult<()> {
    diesel::sql_query(
        "UPDATE monitor_incidents SET resolved_at = now() \
         WHERE monitor_id = $1 AND resolved_at IS NULL",
    )
    .bind::<SqlUuid, _>(monitor_id)
    .execute(conn)
    .await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn update_monitor(
    conn: &mut AsyncPgConnection,
    id: Uuid,
    name: Option<&str>,
    enabled: Option<bool>,
    status: Option<&str>,
    interval_seconds: Option<i32>,
    webhook_url: Option<Option<&str>>, // outer None = leave; inner None = set NULL
) -> QueryResult<Option<Monitor>> {
    // webhook: encode "leave" as a sentinel by splitting into two binds.
    let (set_webhook, webhook_val) = match webhook_url {
        None => (false, None),
        Some(v) => (true, v),
    };
    diesel::sql_query(
        "UPDATE monitors SET \
            name = COALESCE($2, name), \
            enabled = COALESCE($3, enabled), \
            status = COALESCE($4, status), \
            interval_seconds = COALESCE($5, interval_seconds), \
            webhook_url = CASE WHEN $6 THEN $7 ELSE webhook_url END, \
            next_check_at = CASE \
                WHEN $4 = 'unknown' THEN now() \
                WHEN $5 IS NOT NULL THEN now() + make_interval(secs => $5) \
                ELSE next_check_at END, \
            updated_at = now() \
         WHERE id = $1 RETURNING *",
    )
    .bind::<SqlUuid, _>(id)
    .bind::<Nullable<Text>, _>(name)
    .bind::<Nullable<Bool>, _>(enabled)
    .bind::<Nullable<Text>, _>(status)
    .bind::<Nullable<Integer>, _>(interval_seconds)
    .bind::<Bool, _>(set_webhook)
    .bind::<Nullable<Text>, _>(webhook_val)
    .get_result(conn)
    .await
    .optional()
}

// ===========================================================================
// Tiering (hot/cold watermark)
// ===========================================================================

pub async fn get_watermark(
    conn: &mut AsyncPgConnection,
    table: &str,
) -> QueryResult<Option<DateTime<Utc>>> {
    tiering_state::table
        .find(table)
        .select(tiering_state::watermark)
        .first(conn)
        .await
        .optional()
}

/// Upsert the watermark; never moves it backward.
pub async fn advance_watermark(
    conn: &mut AsyncPgConnection,
    table: &str,
    wm: DateTime<Utc>,
) -> QueryResult<()> {
    diesel::insert_into(tiering_state::table)
        .values((
            tiering_state::table_name.eq(table),
            tiering_state::watermark.eq(wm),
            tiering_state::updated_at.eq(Utc::now()),
        ))
        .on_conflict(tiering_state::table_name)
        .do_update()
        .set((
            tiering_state::watermark.eq(diesel::dsl::sql::<Timestamptz>(
                "GREATEST(tiering_state.watermark, EXCLUDED.watermark)",
            )),
            tiering_state::updated_at.eq(Utc::now()),
        ))
        .execute(conn)
        .await?;
    Ok(())
}

pub async fn get_dropped_thru(
    conn: &mut AsyncPgConnection,
    table: &str,
) -> QueryResult<Option<DateTime<Utc>>> {
    tiering_state::table
        .find(table)
        .select(tiering_state::dropped_thru)
        .first::<Option<DateTime<Utc>>>(conn)
        .await
        .optional()
        .map(|o| o.flatten())
}

pub async fn set_dropped_thru(
    conn: &mut AsyncPgConnection,
    table: &str,
    t: DateTime<Utc>,
) -> QueryResult<()> {
    diesel::update(tiering_state::table.find(table))
        .set((
            tiering_state::dropped_thru.eq(Some(t)),
            tiering_state::updated_at.eq(Utc::now()),
        ))
        .execute(conn)
        .await?;
    Ok(())
}

// ===========================================================================
// Runtime settings (operator-tunable, no restart)
// ===========================================================================

/// Raw value for `key`, or `None` when no row exists.
///
/// `None` is the normal, default state and means "use the process's configured
/// value" — it is not an error and must not be treated as one. Nothing seeds this
/// table, so every key reads as `None` until an operator sets it.
pub async fn get_runtime_setting(
    conn: &mut AsyncPgConnection,
    key: &str,
) -> QueryResult<Option<String>> {
    runtime_settings::table
        .find(key)
        .select(runtime_settings::value)
        .first(conn)
        .await
        .optional()
}

/// Value plus when it was last changed, for a UI that shows provenance.
pub async fn get_runtime_setting_row(
    conn: &mut AsyncPgConnection,
    key: &str,
) -> QueryResult<Option<(String, DateTime<Utc>)>> {
    runtime_settings::table
        .find(key)
        .select((runtime_settings::value, runtime_settings::updated_at))
        .first(conn)
        .await
        .optional()
}

/// Upsert `key`. `updated_by` is the acting user, or `None` for a script.
pub async fn set_runtime_setting(
    conn: &mut AsyncPgConnection,
    key: &str,
    value: &str,
    updated_by: Option<Uuid>,
) -> QueryResult<()> {
    diesel::insert_into(runtime_settings::table)
        .values((
            runtime_settings::key.eq(key),
            runtime_settings::value.eq(value),
            runtime_settings::updated_at.eq(Utc::now()),
            runtime_settings::updated_by.eq(updated_by),
        ))
        .on_conflict(runtime_settings::key)
        .do_update()
        .set((
            runtime_settings::value.eq(value),
            runtime_settings::updated_at.eq(Utc::now()),
            runtime_settings::updated_by.eq(updated_by),
        ))
        .execute(conn)
        .await?;
    Ok(())
}

/// Remove `key`, reverting it to the process's configured value.
pub async fn delete_runtime_setting(conn: &mut AsyncPgConnection, key: &str) -> QueryResult<usize> {
    diesel::delete(runtime_settings::table.find(key))
        .execute(conn)
        .await
}

/// Total number of orgs in the deployment.
///
/// Paired with `orgs_with_permission` to express "administers the whole
/// deployment" using only the existing grant primitives: a caller who holds
/// org-scoped `org:manage` in every org that exists is, for practical purposes,
/// the operator. This deployment has no separate super-admin flag (one was added
/// and removed), and a deployment-wide setting must not be changeable by an admin
/// of a single tenant — that would let one tenant force another's data out of
/// Postgres.
pub async fn count_all_orgs(conn: &mut AsyncPgConnection) -> QueryResult<i64> {
    organizations::table.count().get_result(conn).await
}

/// `runtime_settings` key for the cold-rotation age, in days.
pub const TIER_HOT_DAYS_KEY: &str = "tier.hot_days";

/// Smallest rotation age an operator may set.
///
/// Not zero, and not negative. Zero would make every partition instantly
/// eligible, so the worker would tier the current day's data out from under live
/// writes; negative would put the cutoff in the future and tier everything. One
/// day is the smallest value that still leaves a whole bucket hot, matching the
/// worker's day granularity.
pub const TIER_HOT_DAYS_MIN: i64 = 1;

/// The rotation age actually in force: the operator's override when one is set
/// and valid, otherwise the process's configured value.
///
/// Invalid stored values (unparseable, or below `TIER_HOT_DAYS_MIN`) fall back to
/// `configured` rather than erroring. A malformed row must not be able to stop
/// tiering deployment-wide or, worse, drive the cutoff to zero — the write path
/// validates, and this is the second line of defence for a value edited by hand
/// in psql. The caller is expected to log the fallback; this returns no signal
/// beyond the value so it stays usable on a hot path.
pub async fn effective_tier_hot_days(
    conn: &mut AsyncPgConnection,
    configured: i64,
) -> QueryResult<i64> {
    let raw = get_runtime_setting(conn, TIER_HOT_DAYS_KEY).await?;
    Ok(match raw.as_deref().map(str::trim).map(str::parse::<i64>) {
        Some(Ok(v)) if v >= TIER_HOT_DAYS_MIN => v,
        _ => configured,
    })
}

// ===========================================================================
// Tier pins (protect restored ranges from being re-dropped)
// ===========================================================================

/// True iff any UNEXPIRED pin overlaps `[start, end)` for `table`.
///
/// Overlap, not containment: a pin covering part of a partition still has to
/// block the drop, because dropping the partition would take the pinned rows
/// with it. The comparison is the standard half-open overlap test
/// (`pin.start < end AND pin.end > start`), so a pin that merely abuts the
/// partition does not block it.
pub async fn is_range_pinned(
    conn: &mut AsyncPgConnection,
    table: &str,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> QueryResult<bool> {
    let n: i64 = tier_pins::table
        .filter(tier_pins::table_name.eq(table))
        .filter(tier_pins::expires_at.gt(Utc::now()))
        .filter(tier_pins::range_start.lt(end))
        .filter(tier_pins::range_end.gt(start))
        .count()
        .get_result(conn)
        .await?;
    Ok(n > 0)
}

/// Create a pin. Overlapping pins are allowed and are not merged — each records
/// a separate restore with its own expiry, and the drop check only asks whether
/// ANY is live, so the longest-lived one wins naturally.
pub async fn create_tier_pin(
    conn: &mut AsyncPgConnection,
    table: &str,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    created_by: Option<Uuid>,
    reason: Option<&str>,
) -> QueryResult<TierPin> {
    diesel::insert_into(tier_pins::table)
        .values((
            tier_pins::table_name.eq(table),
            tier_pins::range_start.eq(start),
            tier_pins::range_end.eq(end),
            tier_pins::expires_at.eq(expires_at),
            tier_pins::created_by.eq(created_by),
            tier_pins::reason.eq(reason),
        ))
        .returning(TierPin::as_returning())
        .get_result(conn)
        .await
}

/// Every pin, newest first, expired ones included — the UI shows expiry so an
/// operator can tell a lapsed restore from a live one.
pub async fn list_tier_pins(conn: &mut AsyncPgConnection) -> QueryResult<Vec<TierPin>> {
    tier_pins::table
        .select(TierPin::as_select())
        .order(tier_pins::created_at.desc())
        .load(conn)
        .await
}

/// Drop a pin, making its range immediately eligible for re-tiering. The rows
/// stay in Parquet, so this frees Postgres space without losing data.
pub async fn delete_tier_pin(conn: &mut AsyncPgConnection, id: Uuid) -> QueryResult<usize> {
    diesel::delete(tier_pins::table.find(id))
        .execute(conn)
        .await
}

/// Remove pins that expired before `now`. Housekeeping only: `is_range_pinned`
/// already ignores expired rows, so leaving them costs a little space and
/// nothing else.
pub async fn purge_expired_tier_pins(conn: &mut AsyncPgConnection) -> QueryResult<usize> {
    diesel::delete(tier_pins::table.filter(tier_pins::expires_at.le(Utc::now())))
        .execute(conn)
        .await
}

// ===========================================================================
// Partition maintenance
// ===========================================================================

/// Create a range partition if it does not already exist. `table`/`suffix` are
/// internal identifiers (never user input); timestamps are formatted as ISO
/// literals because partition bounds cannot be bound parameters in DDL.
pub async fn create_range_partition(
    conn: &mut AsyncPgConnection,
    table: &str,
    suffix: &str,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> QueryResult<()> {
    let sql = format!(
        "CREATE TABLE IF NOT EXISTS {table}_{suffix} PARTITION OF {table} \
         FOR VALUES FROM ('{start}') TO ('{end}')",
        table = table,
        suffix = suffix,
        start = start.to_rfc3339(),
        end = end.to_rfc3339(),
    );
    diesel::sql_query(sql).execute(conn).await?;
    Ok(())
}

#[derive(diesel::QueryableByName)]
struct ChildName {
    #[diesel(sql_type = Text)]
    child: String,
}

/// Child partition relation names for `table`, excluding the DEFAULT partition.
pub async fn list_child_partitions(
    conn: &mut AsyncPgConnection,
    table: &str,
) -> QueryResult<Vec<String>> {
    let rows: Vec<ChildName> = diesel::sql_query(
        "SELECT c.relname AS child \
         FROM pg_inherits i \
         JOIN pg_class c ON c.oid = i.inhrelid \
         JOIN pg_class p ON p.oid = i.inhparent \
         WHERE p.relname = $1 AND c.relname <> ($1 || '_default') \
         ORDER BY c.relname",
    )
    .bind::<Text, _>(table)
    .load(conn)
    .await?;
    Ok(rows.into_iter().map(|r| r.child).collect())
}

#[derive(diesel::QueryableByName)]
struct CountRow {
    #[diesel(sql_type = BigInt)]
    n: i64,
}

pub async fn count_child_rows(conn: &mut AsyncPgConnection, child: &str) -> QueryResult<i64> {
    // `child` is an internal relation name derived from our own suffix, not user input.
    let row: CountRow = diesel::sql_query(format!("SELECT count(*)::bigint AS n FROM {child}"))
        .get_result(conn)
        .await?;
    Ok(row.n)
}

/// Detach then drop a partition in one transaction. Detach first so the parent
/// is never briefly missing the range.
pub async fn detach_and_drop_partition(
    conn: &mut AsyncPgConnection,
    table: &str,
    child: &str,
) -> QueryResult<()> {
    // Multiple statements in one command require the SIMPLE query protocol.
    // diesel-async's `sql_query(...).execute()` uses the EXTENDED protocol, which
    // rejects "cannot insert multiple commands into a prepared statement".
    // `batch_execute` (SimpleAsyncConnection) sends the BEGIN/DETACH/DROP/COMMIT
    // block via the simple protocol; the explicit transaction keeps it atomic.
    let sql =
        format!("BEGIN; ALTER TABLE {table} DETACH PARTITION {child}; DROP TABLE {child}; COMMIT;");
    conn.batch_execute(&sql).await
}

// ===========================================================================
// Cross-tier reads (hot side)
// ===========================================================================

#[derive(diesel::QueryableByName)]
pub struct DayCountRow {
    #[diesel(sql_type = diesel::sql_types::Date)]
    pub day: chrono::NaiveDate,
    #[diesel(sql_type = BigInt)]
    pub count: i64,
}

/// Per-day error counts from the HOT (Postgres) tier for `[from, to)`.
pub async fn error_counts_by_day_hot(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> QueryResult<Vec<DayCountRow>> {
    diesel::sql_query(
        "SELECT (occurred_at AT TIME ZONE 'UTC')::date AS day, count(*)::bigint AS count \
         FROM error_events \
         WHERE app_id = $1 AND occurred_at >= $2 AND occurred_at < $3 \
         GROUP BY 1 ORDER BY 1",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Timestamptz, _>(from)
    .bind::<Timestamptz, _>(to)
    .load(conn)
    .await
}

/// Per-day counts from ONLY a table's DEFAULT partition, for `[from, to)`.
/// Late-arriving events whose explicit partition was already tiered+dropped land
/// in `<table>_default` (never exported to Parquet). The cross-tier reader adds
/// these to the COLD half so they aren't lost. `default_table` is an INTERNAL
/// identifier (e.g. "error_events_default"), never user input.
pub async fn default_partition_counts_by_day(
    conn: &mut AsyncPgConnection,
    default_table: &str,
    app_id: Uuid,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> QueryResult<Vec<DayCountRow>> {
    diesel::sql_query(format!(
        "SELECT (occurred_at AT TIME ZONE 'UTC')::date AS day, count(*)::bigint AS count \
         FROM {default_table} \
         WHERE app_id = $1 AND occurred_at >= $2 AND occurred_at < $3 \
         GROUP BY 1 ORDER BY 1"
    ))
    .bind::<SqlUuid, _>(app_id)
    .bind::<Timestamptz, _>(from)
    .bind::<Timestamptz, _>(to)
    .load(conn)
    .await
}

/// Per-day analytics-event counts from the HOT (Postgres) tier for `[from, to)`.
pub async fn event_counts_by_day_hot(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> QueryResult<Vec<DayCountRow>> {
    diesel::sql_query(
        "SELECT (occurred_at AT TIME ZONE 'UTC')::date AS day, count(*)::bigint AS count \
         FROM analytics_events \
         WHERE app_id = $1 AND occurred_at >= $2 AND occurred_at < $3 \
         GROUP BY 1 ORDER BY 1",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Timestamptz, _>(from)
    .bind::<Timestamptz, _>(to)
    .load(conn)
    .await
}

/// Per-day transaction THROUGHPUT (count) from the HOT (Postgres) tier for `[from, to)`.
/// ADDITIVE metric only — safe to sum across tiers. Transaction PERCENTILES
/// (p50/p95 of duration_ms) are HOLISTIC and are NOT merged across tiers; those
/// endpoints stay hot-only (Postgres). Do not add percentiles to the cold path
/// without mergeable sketches (t-digest/DDSketch).
pub async fn transaction_counts_by_day_hot(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> QueryResult<Vec<DayCountRow>> {
    diesel::sql_query(
        "SELECT (occurred_at AT TIME ZONE 'UTC')::date AS day, count(*)::bigint AS count \
         FROM transactions \
         WHERE app_id = $1 AND occurred_at >= $2 AND occurred_at < $3 \
         GROUP BY 1 ORDER BY 1",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Timestamptz, _>(from)
    .bind::<Timestamptz, _>(to)
    .load(conn)
    .await
}

// ===========================================================================
// Storage (admin) — sizes and per-app row counts. `table` args are internal
// identifiers from sauron_tier::TIERED_TABLES, never user input.
// ===========================================================================

#[derive(diesel::QueryableByName)]
struct BytesRow {
    #[diesel(sql_type = BigInt)]
    bytes: i64,
}

pub async fn db_total_bytes(conn: &mut AsyncPgConnection) -> QueryResult<i64> {
    let row: BytesRow =
        diesel::sql_query("SELECT pg_database_size(current_database())::bigint AS bytes")
            .get_result(conn)
            .await?;
    Ok(row.bytes)
}

pub async fn table_total_bytes(conn: &mut AsyncPgConnection, table: &str) -> QueryResult<i64> {
    // A partitioned parent has no storage of its own; sum the whole partition
    // tree (parent + children). Works for a non-partitioned table too (tree = self).
    let row: BytesRow = diesel::sql_query(format!(
        "SELECT COALESCE(sum(pg_total_relation_size(relid)), 0)::bigint AS bytes \
         FROM pg_partition_tree('{table}'::regclass)"
    ))
    .get_result(conn)
    .await?;
    Ok(row.bytes)
}

pub async fn table_avg_row_width(conn: &mut AsyncPgConnection, table: &str) -> QueryResult<i64> {
    // pg_stats for a partitioned PARENT is empty until inherited stats exist, so
    // read the whole partition tree. avg_width is per-column; take one representative
    // width per column (max across partitions) then sum → estimated bytes/row.
    let row: BytesRow = diesel::sql_query(
        "SELECT COALESCE(sum(w), 0)::bigint AS bytes FROM ( \
           SELECT s.attname, max(s.avg_width) AS w \
           FROM pg_partition_tree($1::regclass) t \
           JOIN pg_class c ON c.oid = t.relid \
           JOIN pg_namespace n ON n.oid = c.relnamespace \
           JOIN pg_stats s ON s.schemaname = n.nspname AND s.tablename = c.relname \
           GROUP BY s.attname \
         ) x",
    )
    .bind::<Text, _>(table)
    .get_result(conn)
    .await?;
    Ok(row.bytes)
}

#[derive(diesel::QueryableByName)]
pub struct AppCountRow {
    #[diesel(sql_type = SqlUuid)]
    pub app_id: Uuid,
    #[diesel(sql_type = BigInt)]
    pub n: i64,
}

pub async fn hot_rows_by_app(
    conn: &mut AsyncPgConnection,
    table: &str,
) -> QueryResult<Vec<AppCountRow>> {
    diesel::sql_query(format!(
        "SELECT app_id, count(*)::bigint AS n FROM {table} GROUP BY app_id"
    ))
    .load(conn)
    .await
}

#[derive(diesel::QueryableByName)]
pub struct AppOrgRow {
    #[diesel(sql_type = SqlUuid)]
    pub app_id: Uuid,
    #[diesel(sql_type = Text)]
    pub app_name: String,
    #[diesel(sql_type = Text)]
    pub project_name: String,
    #[diesel(sql_type = Text)]
    pub org_name: String,
}

pub async fn list_apps_with_org(conn: &mut AsyncPgConnection) -> QueryResult<Vec<AppOrgRow>> {
    diesel::sql_query(
        "SELECT a.id AS app_id, a.name AS app_name, p.name AS project_name, o.name AS org_name \
         FROM apps a JOIN projects p ON a.project_id = p.id \
         JOIN organizations o ON p.org_id = o.id \
         ORDER BY o.name, p.name, a.name",
    )
    .load(conn)
    .await
}

/// Apps belonging to `org_ids` only — the tenant-scoped form of
/// [`list_apps_with_org`], used by the storage report so a caller never sees
/// apps outside the orgs they administer.
pub async fn list_apps_with_org_scoped(
    conn: &mut AsyncPgConnection,
    org_ids: &[Uuid],
) -> QueryResult<Vec<AppOrgRow>> {
    diesel::sql_query(
        "SELECT a.id AS app_id, a.name AS app_name, p.name AS project_name, o.name AS org_name \
         FROM apps a JOIN projects p ON a.project_id = p.id \
         JOIN organizations o ON p.org_id = o.id \
         WHERE o.id = ANY($1) \
         ORDER BY o.name, p.name, a.name",
    )
    .bind::<diesel::sql_types::Array<SqlUuid>, _>(org_ids)
    .load(conn)
    .await
}

/// Per-app hot row counts restricted to `app_ids`.
///
/// The unscoped [`hot_rows_by_app`] scans every partition of the largest tables
/// in the deployment; restricting by `app_id` lets the planner use the app-keyed
/// indexes and bounds the work to the caller's own data.
pub async fn hot_rows_by_app_scoped(
    conn: &mut AsyncPgConnection,
    table: &str,
    app_ids: &[Uuid],
) -> QueryResult<Vec<AppCountRow>> {
    // `table` is never user input: callers pass a literal from TIERED_TABLES.
    diesel::sql_query(format!(
        "SELECT app_id, count(*)::bigint AS n FROM {table} WHERE app_id = ANY($1) GROUP BY app_id"
    ))
    .bind::<diesel::sql_types::Array<SqlUuid>, _>(app_ids)
    .load(conn)
    .await
}

/// The orgs in which `user_id` holds an **org-scoped** grant carrying `permission`.
///
/// Used to scope deployment-wide reports to the tenants a caller actually
/// administers.
pub async fn orgs_with_permission(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    permission: &str,
) -> QueryResult<Vec<Uuid>> {
    diesel::sql_query(
        "SELECT DISTINCT g.org_id AS id \
         FROM role_grants g JOIN roles r ON g.role_id = r.id \
         WHERE g.user_id = $1 AND g.scope_type = 'org' \
           AND r.permissions @> to_jsonb($2::text)",
    )
    .bind::<SqlUuid, _>(user_id)
    .bind::<Text, _>(permission)
    .load::<IdRow>(conn)
    .await
    .map(|rows| rows.into_iter().map(|r| r.id).collect())
}

// ===========================================================================
// Symbol artifacts (source maps / Dart debug-info), content-addressed
// ===========================================================================

/// Insert a content-addressed blob, or bump its refcount if it already exists.
pub async fn put_blob(
    conn: &mut AsyncPgConnection,
    sha: &[u8],
    compressed: &[u8],
    uncompressed_size: i64,
    compressed_size: i64,
) -> QueryResult<()> {
    diesel::insert_into(symbol_blobs::table)
        .values(NewSymbolBlob {
            sha256: sha,
            content: compressed,
            uncompressed_size,
            compressed_size,
            refcount: 1,
        })
        .on_conflict(symbol_blobs::sha256)
        .do_update()
        .set(symbol_blobs::refcount.eq(symbol_blobs::refcount + 1))
        .execute(conn)
        .await?;
    Ok(())
}

/// Cheap indexed check: does this app have ANY symbol artifacts uploaded? Lets
/// the ingest path skip a per-error artifact lookup for apps that use no symbols.
pub async fn app_has_symbol_artifacts(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
) -> QueryResult<bool> {
    diesel::select(diesel::dsl::exists(
        symbol_artifacts::table.filter(symbol_artifacts::app_id.eq(app_id)),
    ))
    .get_result(conn)
    .await
}

/// Fetch the compressed bytes of a blob by content hash.
pub async fn get_blob(conn: &mut AsyncPgConnection, sha: &[u8]) -> QueryResult<Option<Vec<u8>>> {
    symbol_blobs::table
        .filter(symbol_blobs::sha256.eq(sha))
        .select(symbol_blobs::content)
        .first::<Vec<u8>>(conn)
        .await
        .optional()
}

/// Persist symbolicated frames + status onto an error event (by its composite
/// PK: id + occurred_at). Used by the on-read backfill for hot partitions.
pub async fn update_event_symbolication(
    conn: &mut AsyncPgConnection,
    event_id: Uuid,
    occurred_at: DateTime<Utc>,
    frames: Value,
    status: &str,
) -> QueryResult<usize> {
    diesel::update(
        error_events::table
            .filter(error_events::id.eq(event_id))
            .filter(error_events::occurred_at.eq(occurred_at)),
    )
    .set((
        error_events::stacktrace_symbolicated.eq(Some(frames)),
        error_events::symbolication_status.eq(status.to_string()),
    ))
    .execute(conn)
    .await
}

pub async fn insert_symbol_artifact(
    conn: &mut AsyncPgConnection,
    art: NewSymbolArtifact,
) -> QueryResult<SymbolArtifact> {
    diesel::insert_into(symbol_artifacts::table)
        .values(&art)
        .returning(SymbolArtifact::as_returning())
        .get_result(conn)
        .await
}

pub async fn list_symbol_artifacts(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
) -> QueryResult<Vec<SymbolArtifact>> {
    symbol_artifacts::table
        .filter(symbol_artifacts::app_id.eq(app_id))
        .select(SymbolArtifact::as_select())
        .order(symbol_artifacts::created_at.desc())
        .load(conn)
        .await
}

/// List artifacts for an app joined to their blob sizes (uncompressed, compressed).
pub async fn list_artifacts_with_sizes(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
) -> QueryResult<Vec<(SymbolArtifact, i64, i64)>> {
    symbol_artifacts::table
        .inner_join(symbol_blobs::table.on(symbol_artifacts::blob_sha256.eq(symbol_blobs::sha256)))
        .filter(symbol_artifacts::app_id.eq(app_id))
        .select((
            SymbolArtifact::as_select(),
            symbol_blobs::uncompressed_size,
            symbol_blobs::compressed_size,
        ))
        .order(symbol_artifacts::created_at.desc())
        .load(conn)
        .await
}

pub async fn get_symbol_artifact(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    id: Uuid,
) -> QueryResult<Option<SymbolArtifact>> {
    symbol_artifacts::table
        .filter(symbol_artifacts::app_id.eq(app_id))
        .filter(symbol_artifacts::id.eq(id))
        .select(SymbolArtifact::as_select())
        .first(conn)
        .await
        .optional()
}

/// Idempotency lookup by Dart build-id.
pub async fn find_artifact_by_debug_id(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    debug_id: &str,
) -> QueryResult<Option<SymbolArtifact>> {
    symbol_artifacts::table
        .filter(symbol_artifacts::app_id.eq(app_id))
        .filter(symbol_artifacts::debug_id.eq(debug_id))
        .select(SymbolArtifact::as_select())
        .first(conn)
        .await
        .optional()
}

/// Idempotency lookup by (release, name, blob) for JS uploads.
pub async fn find_artifact_by_release_name(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    release: Option<&str>,
    name: Option<&str>,
    blob_sha: &[u8],
) -> QueryResult<Option<SymbolArtifact>> {
    let mut q = symbol_artifacts::table
        .filter(symbol_artifacts::app_id.eq(app_id))
        .filter(symbol_artifacts::blob_sha256.eq(blob_sha.to_vec()))
        .into_boxed();
    q = match release {
        Some(r) => q.filter(symbol_artifacts::release.eq(r.to_string())),
        None => q.filter(symbol_artifacts::release.is_null()),
    };
    q = match name {
        Some(n) => q.filter(symbol_artifacts::name.eq(n.to_string())),
        None => q.filter(symbol_artifacts::name.is_null()),
    };
    q.select(SymbolArtifact::as_select())
        .first(conn)
        .await
        .optional()
}

/// All artifacts uploaded for a release (used by the JS matcher). Newest first,
/// so re-uploads with the same (release, name) win deterministically.
pub async fn find_artifacts_for_release(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    release: &str,
) -> QueryResult<Vec<SymbolArtifact>> {
    symbol_artifacts::table
        .filter(symbol_artifacts::app_id.eq(app_id))
        .filter(symbol_artifacts::release.eq(release))
        .select(SymbolArtifact::as_select())
        .order(symbol_artifacts::created_at.desc())
        .load(conn)
        .await
}

/// Delete an artifact (scoped to `app_id`), decrement referenced blob refcounts,
/// and GC any blob that reaches zero. Returns false if the artifact wasn't found.
///
/// Not wrapped in a transaction: a crash mid-way can leave a blob with a stale
/// refcount (orphaned, harmless) — acceptable for the MVP artifact store.
pub async fn delete_symbol_artifact(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    id: Uuid,
) -> QueryResult<bool> {
    let art = match get_symbol_artifact(conn, app_id, id).await? {
        Some(a) => a,
        None => return Ok(false),
    };
    diesel::delete(
        symbol_artifacts::table
            .filter(symbol_artifacts::app_id.eq(app_id))
            .filter(symbol_artifacts::id.eq(id)),
    )
    .execute(conn)
    .await?;

    let mut hashes = vec![art.blob_sha256];
    if let Some(idx) = art.prebuilt_index_sha256 {
        if !hashes.contains(&idx) {
            hashes.push(idx);
        }
    }
    for h in hashes {
        diesel::update(symbol_blobs::table.filter(symbol_blobs::sha256.eq(&h)))
            .set(symbol_blobs::refcount.eq(symbol_blobs::refcount - 1))
            .execute(conn)
            .await?;
        diesel::delete(
            symbol_blobs::table
                .filter(symbol_blobs::sha256.eq(&h))
                .filter(symbol_blobs::refcount.le(0)),
        )
        .execute(conn)
        .await?;
    }
    Ok(true)
}

// ===========================================================================
// Alerting: notification channels, rules, deliveries
// ===========================================================================

pub async fn create_channel(
    conn: &mut AsyncPgConnection,
    ch: NewNotificationChannel<'_>,
) -> QueryResult<NotificationChannel> {
    diesel::insert_into(notification_channels::table)
        .values(ch)
        .returning(NotificationChannel::as_returning())
        .get_result(conn)
        .await
}

pub async fn list_channels_for_org(
    conn: &mut AsyncPgConnection,
    org_id: Uuid,
) -> QueryResult<Vec<NotificationChannel>> {
    notification_channels::table
        .filter(notification_channels::org_id.eq(org_id))
        .order(notification_channels::created_at.desc())
        .limit(500)
        .select(NotificationChannel::as_select())
        .load(conn)
        .await
}

pub async fn get_channel(
    conn: &mut AsyncPgConnection,
    id: Uuid,
) -> QueryResult<Option<NotificationChannel>> {
    notification_channels::table
        .filter(notification_channels::id.eq(id))
        .select(NotificationChannel::as_select())
        .first(conn)
        .await
        .optional()
}

/// Update a channel's mutable fields. `secret_enc`: `None` = leave unchanged,
/// `Some(None)` = clear, `Some(Some(blob))` = replace.
pub async fn update_channel(
    conn: &mut AsyncPgConnection,
    id: Uuid,
    name: Option<&str>,
    config: Option<&Value>,
    secret_enc: Option<Option<Vec<u8>>>,
    enabled: Option<bool>,
) -> QueryResult<Option<NotificationChannel>> {
    let mut any = false;
    if let Some(n) = name {
        diesel::update(notification_channels::table.filter(notification_channels::id.eq(id)))
            .set(notification_channels::name.eq(n))
            .execute(conn)
            .await?;
        any = true;
    }
    if let Some(c) = config {
        diesel::update(notification_channels::table.filter(notification_channels::id.eq(id)))
            .set(notification_channels::config.eq(c))
            .execute(conn)
            .await?;
        any = true;
    }
    if let Some(s) = secret_enc {
        diesel::update(notification_channels::table.filter(notification_channels::id.eq(id)))
            .set(notification_channels::secret_enc.eq(s))
            .execute(conn)
            .await?;
        any = true;
    }
    if let Some(e) = enabled {
        diesel::update(notification_channels::table.filter(notification_channels::id.eq(id)))
            .set(notification_channels::enabled.eq(e))
            .execute(conn)
            .await?;
        any = true;
    }
    if any {
        diesel::update(notification_channels::table.filter(notification_channels::id.eq(id)))
            .set(notification_channels::updated_at.eq(Utc::now()))
            .execute(conn)
            .await?;
    }
    get_channel(conn, id).await
}

pub async fn delete_channel(conn: &mut AsyncPgConnection, id: Uuid) -> QueryResult<usize> {
    diesel::delete(notification_channels::table.filter(notification_channels::id.eq(id)))
        .execute(conn)
        .await
}

pub async fn create_alert_rule(
    conn: &mut AsyncPgConnection,
    rule: NewAlertRule<'_>,
) -> QueryResult<AlertRule> {
    diesel::insert_into(alert_rules::table)
        .values(rule)
        .returning(AlertRule::as_returning())
        .get_result(conn)
        .await
}

pub async fn list_alert_rules_for_org(
    conn: &mut AsyncPgConnection,
    org_id: Uuid,
) -> QueryResult<Vec<AlertRule>> {
    alert_rules::table
        .filter(alert_rules::org_id.eq(org_id))
        .order(alert_rules::created_at.desc())
        .limit(500)
        .select(AlertRule::as_select())
        .load(conn)
        .await
}

pub async fn get_alert_rule(
    conn: &mut AsyncPgConnection,
    id: Uuid,
) -> QueryResult<Option<AlertRule>> {
    alert_rules::table
        .filter(alert_rules::id.eq(id))
        .select(AlertRule::as_select())
        .first(conn)
        .await
        .optional()
}

#[allow(clippy::too_many_arguments)]
pub async fn update_alert_rule(
    conn: &mut AsyncPgConnection,
    id: Uuid,
    name: Option<&str>,
    enabled: Option<bool>,
    conditions: Option<&Value>,
    severity: Option<&str>,
    throttle_seconds: Option<i32>,
    message_template: Option<Option<&str>>,
) -> QueryResult<Option<AlertRule>> {
    if let Some(n) = name {
        diesel::update(alert_rules::table.filter(alert_rules::id.eq(id)))
            .set(alert_rules::name.eq(n))
            .execute(conn)
            .await?;
    }
    if let Some(e) = enabled {
        diesel::update(alert_rules::table.filter(alert_rules::id.eq(id)))
            .set(alert_rules::enabled.eq(e))
            .execute(conn)
            .await?;
    }
    if let Some(c) = conditions {
        diesel::update(alert_rules::table.filter(alert_rules::id.eq(id)))
            .set(alert_rules::conditions.eq(c))
            .execute(conn)
            .await?;
    }
    if let Some(s) = severity {
        diesel::update(alert_rules::table.filter(alert_rules::id.eq(id)))
            .set(alert_rules::severity.eq(s))
            .execute(conn)
            .await?;
    }
    if let Some(t) = throttle_seconds {
        diesel::update(alert_rules::table.filter(alert_rules::id.eq(id)))
            .set(alert_rules::throttle_seconds.eq(t))
            .execute(conn)
            .await?;
    }
    if let Some(m) = message_template {
        diesel::update(alert_rules::table.filter(alert_rules::id.eq(id)))
            .set(alert_rules::message_template.eq(m))
            .execute(conn)
            .await?;
    }
    diesel::update(alert_rules::table.filter(alert_rules::id.eq(id)))
        .set(alert_rules::updated_at.eq(Utc::now()))
        .execute(conn)
        .await?;
    get_alert_rule(conn, id).await
}

pub async fn delete_alert_rule(conn: &mut AsyncPgConnection, id: Uuid) -> QueryResult<usize> {
    diesel::delete(alert_rules::table.filter(alert_rules::id.eq(id)))
        .execute(conn)
        .await
}

/// Replace a rule's channel attachments with `channel_ids` (already validated
/// as belonging to the rule's org by the route layer).
pub async fn set_rule_channels(
    conn: &mut AsyncPgConnection,
    rule_id: Uuid,
    channel_ids: &[Uuid],
) -> QueryResult<()> {
    diesel::delete(alert_rule_channels::table.filter(alert_rule_channels::rule_id.eq(rule_id)))
        .execute(conn)
        .await?;
    for cid in channel_ids {
        diesel::insert_into(alert_rule_channels::table)
            .values((
                alert_rule_channels::rule_id.eq(rule_id),
                alert_rule_channels::channel_id.eq(*cid),
            ))
            .on_conflict_do_nothing()
            .execute(conn)
            .await?;
    }
    Ok(())
}

pub async fn rule_channel_ids(
    conn: &mut AsyncPgConnection,
    rule_id: Uuid,
) -> QueryResult<Vec<Uuid>> {
    alert_rule_channels::table
        .filter(alert_rule_channels::rule_id.eq(rule_id))
        .select(alert_rule_channels::channel_id)
        .load(conn)
        .await
}

/// Channel ids for many rules at once, grouped by rule.
///
/// The rules list rendered one `rule_channel_ids` query per rule, so an org with
/// 200 rules issued 201 queries per page load.
pub async fn rule_channel_ids_for_rules(
    conn: &mut AsyncPgConnection,
    rule_ids: &[Uuid],
) -> QueryResult<HashMap<Uuid, Vec<Uuid>>> {
    let rows: Vec<(Uuid, Uuid)> = alert_rule_channels::table
        .filter(alert_rule_channels::rule_id.eq_any(rule_ids))
        .select((
            alert_rule_channels::rule_id,
            alert_rule_channels::channel_id,
        ))
        .load(conn)
        .await?;
    let mut out: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
    for (rule_id, channel_id) in rows {
        out.entry(rule_id).or_default().push(channel_id);
    }
    Ok(out)
}

/// The (enabled or not) channels attached to a rule.
pub async fn channels_for_rule(
    conn: &mut AsyncPgConnection,
    rule_id: Uuid,
) -> QueryResult<Vec<NotificationChannel>> {
    alert_rule_channels::table
        .inner_join(notification_channels::table)
        .filter(alert_rule_channels::rule_id.eq(rule_id))
        .select(NotificationChannel::as_select())
        .load(conn)
        .await
}

pub async fn insert_alert_event(
    conn: &mut AsyncPgConnection,
    ev: NewAlertEvent<'_>,
) -> QueryResult<usize> {
    diesel::insert_into(alert_events::table)
        .values(ev)
        .execute(conn)
        .await
}

/// Durable throttle backstop: was an alert with this dedup key *sent* within
/// the last `within_seconds`? (Used when Redis is unavailable.)
pub async fn alert_recently_sent(
    conn: &mut AsyncPgConnection,
    dedup_key: &str,
    within_seconds: i32,
) -> QueryResult<bool> {
    let cutoff = Utc::now() - chrono::Duration::seconds(within_seconds.max(0) as i64);
    let n: i64 = alert_events::table
        .filter(alert_events::dedup_key.eq(dedup_key))
        .filter(alert_events::status.eq("sent"))
        .filter(alert_events::created_at.gt(cutoff))
        .count()
        .get_result(conn)
        .await?;
    Ok(n > 0)
}

/// Paginated alert history for an org (bounded).
pub async fn list_alert_events(
    conn: &mut AsyncPgConnection,
    org_id: Uuid,
    limit: i64,
    offset: i64,
) -> QueryResult<Vec<AlertEventRow>> {
    alert_events::table
        .filter(alert_events::org_id.eq(org_id))
        .order(alert_events::created_at.desc())
        .limit(limit.clamp(1, 200))
        .offset(offset.clamp(0, 100_000))
        .select(AlertEventRow::as_select())
        .load(conn)
        .await
}

/// Enabled rules the evaluator polls (all metric trigger types).
pub async fn enabled_metric_alert_rules(
    conn: &mut AsyncPgConnection,
) -> QueryResult<Vec<AlertRule>> {
    alert_rules::table
        .filter(alert_rules::enabled.eq(true))
        .filter(alert_rules::trigger_type.ne_all(vec!["monitor_down", "monitor_up"]))
        .select(AlertRule::as_select())
        .load(conn)
        .await
}

/// Enabled monitor-transition rules that apply to `project_id` (org-wide rules
/// plus rules narrowed to exactly this project).
pub async fn alert_rules_for_monitor(
    conn: &mut AsyncPgConnection,
    project_id: Uuid,
    trigger_type: &str,
) -> QueryResult<Vec<AlertRule>> {
    let org: Option<Uuid> = projects::table
        .filter(projects::id.eq(project_id))
        .select(projects::org_id)
        .first(conn)
        .await
        .optional()?;
    let Some(org_id) = org else {
        return Ok(Vec::new());
    };
    alert_rules::table
        .filter(alert_rules::enabled.eq(true))
        .filter(alert_rules::trigger_type.eq(trigger_type))
        .filter(alert_rules::org_id.eq(org_id))
        .filter(
            alert_rules::project_id
                .is_null()
                .or(alert_rules::project_id.eq(project_id)),
        )
        .select(AlertRule::as_select())
        .load(conn)
        .await
}

pub async fn touch_rule_evaluated(
    conn: &mut AsyncPgConnection,
    rule_id: Uuid,
    at: DateTime<Utc>,
) -> QueryResult<usize> {
    diesel::update(alert_rules::table.filter(alert_rules::id.eq(rule_id)))
        .set(alert_rules::last_evaluated_at.eq(at))
        .execute(conn)
        .await
}

/// The app ids a rule's scope covers (org-wide, project-narrowed, or one app).
pub async fn apps_in_alert_scope(
    conn: &mut AsyncPgConnection,
    org_id: Uuid,
    project_id: Option<Uuid>,
    app_id: Option<Uuid>,
) -> QueryResult<Vec<Uuid>> {
    let mut q = apps::table
        .inner_join(projects::table)
        .filter(projects::org_id.eq(org_id))
        .into_boxed();
    if let Some(p) = project_id {
        q = q.filter(apps::project_id.eq(p));
    }
    if let Some(a) = app_id {
        q = q.filter(apps::id.eq(a));
    }
    q.select(apps::id).load(conn).await
}

#[derive(Debug, QueryableByName)]
pub struct AlertCountRow {
    #[diesel(sql_type = BigInt)]
    pub n: i64,
}

#[derive(Debug, QueryableByName)]
pub struct AlertValueRow {
    #[diesel(sql_type = Nullable<Double>)]
    pub v: Option<f64>,
}

/// `(enrollment_id, app_id, catalogue_environment_id)` for every LIVE
/// enrollment of `app_ids`.
///
/// This is one of exactly two sanctioned bridges between the two environment
/// id spaces. A subscription stores CATALOGUE ids (they are the wildcard RBAC
/// lacks, and stay correct when a new app is auto-enrolled); everything
/// downstream — event rows, `role_grants.scope_id`, `Reach.envs` — is
/// ENROLLMENT ids. Mixing them produces a filter that matches nothing, and the
/// failure is silent at every layer.
pub async fn live_enrollments_for_apps(
    conn: &mut AsyncPgConnection,
    app_ids: &[Uuid],
) -> QueryResult<Vec<(Uuid, Uuid, Uuid)>> {
    if app_ids.is_empty() {
        return Ok(Vec::new());
    }
    app_environments::table
        .filter(app_environments::app_id.eq_any(app_ids.to_vec()))
        .filter(app_environments::retired_at.is_null())
        .select((
            app_environments::id,
            app_environments::app_id,
            app_environments::environment_id,
        ))
        .load(conn)
        .await
}

/// The ENROLLMENT ids of the live environment named `name` across `app_ids`.
///
/// `retired_at IS NULL` is load-bearing on BOTH tables: `(app_id, name)` is
/// only unique among LIVE environments, so retiring `staging` and creating a
/// fresh `staging` leaves two rows with that name. Without these filters the
/// resolver returns both ids and the count silently includes the retired
/// environment's events too. The partial unique index guarantees at most one
/// live match per name, so this is deterministic.
pub async fn enrollment_ids_for_env_name(
    conn: &mut AsyncPgConnection,
    app_ids: &[Uuid],
    name: &str,
) -> QueryResult<Vec<Uuid>> {
    if app_ids.is_empty() {
        return Ok(Vec::new());
    }
    app_environments::table
        .inner_join(environments::table.on(environments::id.eq(app_environments::environment_id)))
        .filter(app_environments::app_id.eq_any(app_ids.to_vec()))
        .filter(app_environments::retired_at.is_null())
        .filter(environments::retired_at.is_null())
        .filter(environments::name.eq(name))
        .select(app_environments::id)
        .load(conn)
        .await
}

/// Count error events across `app_ids` in `(from, to]`, with optional
/// level/environment/tag filters. All values are bound parameters.
///
/// `env_ids` are **enrollment** ids (`app_environments.id`), because that is
/// what `error_events.environment_id` holds. Callers that start from an
/// environment *name* resolve it through [`enrollment_ids_for_env_name`]
/// first. `Some(&[])` short-circuits to zero explicitly rather than by
/// accident through an empty `ANY()`.
pub async fn alert_count_errors(
    conn: &mut AsyncPgConnection,
    app_ids: &[Uuid],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    level: Option<&str>,
    env_ids: Option<&[Uuid]>,
    tag: Option<&Value>,
) -> QueryResult<i64> {
    if env_ids.is_some_and(|e| e.is_empty()) {
        return Ok(0);
    }
    let row: AlertCountRow = diesel::sql_query(
        "SELECT count(*) AS n FROM error_events \
         WHERE app_id = ANY($1) AND occurred_at > $2 AND occurred_at <= $3 \
           AND ($4::text IS NULL OR level = $4) \
           AND ($5::uuid[] IS NULL OR environment_id = ANY($5)) \
           AND ($6::jsonb IS NULL OR tags @> $6)",
    )
    .bind::<diesel::sql_types::Array<SqlUuid>, _>(app_ids)
    .bind::<Timestamptz, _>(from)
    .bind::<Timestamptz, _>(to)
    .bind::<Nullable<Text>, _>(level)
    .bind::<Nullable<diesel::sql_types::Array<SqlUuid>>, _>(env_ids.map(|e| e.to_vec()))
    .bind::<Nullable<Jsonb>, _>(tag)
    .get_result(conn)
    .await?;
    Ok(row.n)
}

/// Count analytics events across `app_ids` in `(from, to]`, with optional
/// name/environment/tag filters. `env_ids` are **enrollment** ids; see
/// [`alert_count_errors`].
pub async fn alert_count_events(
    conn: &mut AsyncPgConnection,
    app_ids: &[Uuid],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    name: Option<&str>,
    env_ids: Option<&[Uuid]>,
    tag: Option<&Value>,
) -> QueryResult<i64> {
    if env_ids.is_some_and(|e| e.is_empty()) {
        return Ok(0);
    }
    let row: AlertCountRow = diesel::sql_query(
        "SELECT count(*) AS n FROM analytics_events \
         WHERE app_id = ANY($1) AND occurred_at > $2 AND occurred_at <= $3 \
           AND ($4::text IS NULL OR name = $4) \
           AND ($5::uuid[] IS NULL OR environment_id = ANY($5)) \
           AND ($6::jsonb IS NULL OR tags @> $6)",
    )
    .bind::<diesel::sql_types::Array<SqlUuid>, _>(app_ids)
    .bind::<Timestamptz, _>(from)
    .bind::<Timestamptz, _>(to)
    .bind::<Nullable<Text>, _>(name)
    .bind::<Nullable<diesel::sql_types::Array<SqlUuid>>, _>(env_ids.map(|e| e.to_vec()))
    .bind::<Nullable<Jsonb>, _>(tag)
    .get_result(conn)
    .await?;
    Ok(row.n)
}

#[derive(Debug, QueryableByName)]
pub struct AlertAppCountRow {
    #[diesel(sql_type = SqlUuid)]
    pub app_id: Uuid,
    #[diesel(sql_type = BigInt)]
    pub n: i64,
}

/// Per-app error counts over one window — the grouped form the personal
/// subscription evaluator needs.
///
/// A probe deliberately spans every app of every subscription that shares its
/// condition bucket (keying on a single app id would turn one query over a
/// 200-app project into 200), so the result has to come back attributed by
/// app id. Fanning out positionally instead would let a key-collision bug
/// attribute one app's counts to another user's subscription — a telemetry
/// leak inside an email.
pub async fn alert_count_errors_by_app(
    conn: &mut AsyncPgConnection,
    app_ids: &[Uuid],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    level: Option<&str>,
    env_ids: Option<&[Uuid]>,
) -> QueryResult<Vec<(Uuid, i64)>> {
    if app_ids.is_empty() || env_ids.is_some_and(|e| e.is_empty()) {
        return Ok(Vec::new());
    }
    let rows: Vec<AlertAppCountRow> = diesel::sql_query(
        "SELECT app_id, count(*) AS n FROM error_events \
         WHERE app_id = ANY($1) AND occurred_at > $2 AND occurred_at <= $3 \
           AND ($4::text IS NULL OR level = $4) \
           AND ($5::uuid[] IS NULL OR environment_id = ANY($5)) \
         GROUP BY app_id",
    )
    .bind::<diesel::sql_types::Array<SqlUuid>, _>(app_ids)
    .bind::<Timestamptz, _>(from)
    .bind::<Timestamptz, _>(to)
    .bind::<Nullable<Text>, _>(level)
    .bind::<Nullable<diesel::sql_types::Array<SqlUuid>>, _>(env_ids.map(|e| e.to_vec()))
    .load(conn)
    .await?;
    Ok(rows.into_iter().map(|r| (r.app_id, r.n)).collect())
}

/// A latency metric over transactions in the window. `percentile` is the
/// fraction for percentile_cont; `None` means avg, `Some(-1.0)` means max
/// (the caller maps the whitelisted metric string).
pub async fn alert_latency_metric(
    conn: &mut AsyncPgConnection,
    app_ids: &[Uuid],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    percentile: Option<f64>,
    op: Option<&str>,
) -> QueryResult<Option<f64>> {
    let row: AlertValueRow = match percentile {
        Some(p) if p >= 0.0 => {
            diesel::sql_query(
                "SELECT percentile_cont($4) WITHIN GROUP (ORDER BY duration_ms)::double precision AS v \
                 FROM transactions \
                 WHERE app_id = ANY($1) AND occurred_at > $2 AND occurred_at <= $3 \
                   AND ($5::text IS NULL OR op = $5)",
            )
            .bind::<diesel::sql_types::Array<SqlUuid>, _>(app_ids)
            .bind::<Timestamptz, _>(from)
            .bind::<Timestamptz, _>(to)
            .bind::<Double, _>(p)
            .bind::<Nullable<Text>, _>(op)
            .get_result(conn)
            .await?
        }
        Some(_) => {
            diesel::sql_query(
                "SELECT max(duration_ms)::double precision AS v FROM transactions \
                 WHERE app_id = ANY($1) AND occurred_at > $2 AND occurred_at <= $3 \
                   AND ($4::text IS NULL OR op = $4)",
            )
            .bind::<diesel::sql_types::Array<SqlUuid>, _>(app_ids)
            .bind::<Timestamptz, _>(from)
            .bind::<Timestamptz, _>(to)
            .bind::<Nullable<Text>, _>(op)
            .get_result(conn)
            .await?
        }
        None => {
            diesel::sql_query(
                "SELECT avg(duration_ms)::double precision AS v FROM transactions \
                 WHERE app_id = ANY($1) AND occurred_at > $2 AND occurred_at <= $3 \
                   AND ($4::text IS NULL OR op = $4)",
            )
            .bind::<diesel::sql_types::Array<SqlUuid>, _>(app_ids)
            .bind::<Timestamptz, _>(from)
            .bind::<Timestamptz, _>(to)
            .bind::<Nullable<Text>, _>(op)
            .get_result(conn)
            .await?
        }
    };
    Ok(row.v)
}

#[derive(Debug, QueryableByName, serde::Serialize)]
pub struct AlertIssueBrief {
    #[diesel(sql_type = SqlUuid)]
    pub id: Uuid,
    #[diesel(sql_type = SqlUuid)]
    pub app_id: Uuid,
    #[diesel(sql_type = Text)]
    pub title: String,
    #[diesel(sql_type = Text)]
    pub level: String,
    #[diesel(sql_type = BigInt)]
    pub times_seen: i64,
}

/// Issues first seen in `(from, to]` (new-issue trigger). Bounded.
///
/// `limit` is a bound parameter rather than a literal 20 because a personal
/// subscription's probe spans several apps, and a fixed 20 lets one noisy
/// app starve the rest. Callers pass `n + 1` and treat the extra row as a
/// truncation sentinel.
pub async fn alert_new_issues(
    conn: &mut AsyncPgConnection,
    app_ids: &[Uuid],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    level: Option<&str>,
    limit: i64,
) -> QueryResult<Vec<AlertIssueBrief>> {
    diesel::sql_query(
        // `created_at`, not `first_seen`: the latter is the SDK-supplied event
        // timestamp, while the evaluator's watermark moves on its own clock and
        // the row only lands after pipeline latency. A tick landing in that gap
        // advanced the watermark past `first_seen` and the issue was never
        // alerted; backdated/offline batches lost the same way. `created_at` is
        // Postgres `now()` at INSERT, so it can never predate the watermark.
        "SELECT id, app_id, title, level, times_seen FROM issues \
         WHERE app_id = ANY($1) AND created_at > $2 AND created_at <= $3 \
           AND ($4::text IS NULL OR level = $4) \
         ORDER BY created_at DESC LIMIT $5",
    )
    .bind::<diesel::sql_types::Array<SqlUuid>, _>(app_ids)
    .bind::<Timestamptz, _>(from)
    .bind::<Timestamptz, _>(to)
    .bind::<Nullable<Text>, _>(level)
    .bind::<BigInt, _>(limit.clamp(1, 201))
    .load(conn)
    .await
}

/// Resolved/ignored issues that saw new events in `(from, to]` (regression
/// trigger). `upsert_issue` advances `last_seen` without resetting `status`,
/// so this catches the recurrence. Bounded.
///
/// `limit` is a bound parameter rather than a literal 20 because a personal
/// subscription's probe spans several apps, and a fixed 20 lets one noisy
/// app starve the rest. Callers pass `n + 1` and treat the extra row as a
/// truncation sentinel.
pub async fn alert_regressed_issues(
    conn: &mut AsyncPgConnection,
    app_ids: &[Uuid],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    level: Option<&str>,
    limit: i64,
) -> QueryResult<Vec<AlertIssueBrief>> {
    diesel::sql_query(
        // `last_event_at` is the ingest-side twin of `last_seen`, advanced only
        // by `upsert_issue`. See `alert_new_issues` for why the client-supplied
        // column loses the race with the poll tick.
        "SELECT id, app_id, title, level, times_seen FROM issues \
         WHERE app_id = ANY($1) AND status IN ('resolved','ignored') \
           AND last_event_at > $2 AND last_event_at <= $3 \
           AND ($4::text IS NULL OR level = $4) \
         ORDER BY last_event_at DESC LIMIT $5",
    )
    .bind::<diesel::sql_types::Array<SqlUuid>, _>(app_ids)
    .bind::<Timestamptz, _>(from)
    .bind::<Timestamptz, _>(to)
    .bind::<Nullable<Text>, _>(level)
    .bind::<BigInt, _>(limit.clamp(1, 201))
    .load(conn)
    .await
}

/// [`alert_new_issues`], narrowed to a set of **enrollment** environment ids.
///
/// The EXISTS is bounded by the issue's own `first_seen`/`last_event_at`, NOT
/// by the caller's tick window. Those are two different clocks: the window
/// comes from the server-clock watermark, while `error_events.occurred_at` is
/// SDK-supplied. A backdated or offline batch creates an issue whose
/// `created_at` is inside the window while every one of its events sits
/// outside it — the window-bounded form returns false, the subscription never
/// fires, and nothing is logged. The `- interval '1 hour'` absorbs client clock
/// skew in the direction that matters. Served by
/// `error_events_issue_env_time_idx (issue_id, environment_id, occurred_at DESC)`
/// from migration 31, and the `occurred_at` bounds still prune partitions.
pub async fn alert_new_issues_env(
    conn: &mut AsyncPgConnection,
    app_ids: &[Uuid],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    level: Option<&str>,
    env_ids: &[Uuid],
    limit: i64,
) -> QueryResult<Vec<AlertIssueBrief>> {
    if env_ids.is_empty() {
        return Ok(Vec::new());
    }
    diesel::sql_query(
        "SELECT i.id, i.app_id, i.title, i.level, i.times_seen FROM issues i \
         WHERE i.app_id = ANY($1) AND i.created_at > $2 AND i.created_at <= $3 \
           AND ($4::text IS NULL OR i.level = $4) \
           AND EXISTS ( \
                 SELECT 1 FROM error_events e \
                  WHERE e.issue_id = i.id \
                    AND e.environment_id = ANY($5) \
                    AND e.occurred_at >  i.first_seen - interval '1 hour' \
                    AND e.occurred_at <= i.last_event_at \
           ) \
         ORDER BY i.created_at DESC LIMIT $6",
    )
    .bind::<diesel::sql_types::Array<SqlUuid>, _>(app_ids)
    .bind::<Timestamptz, _>(from)
    .bind::<Timestamptz, _>(to)
    .bind::<Nullable<Text>, _>(level)
    .bind::<diesel::sql_types::Array<SqlUuid>, _>(env_ids)
    .bind::<BigInt, _>(limit.clamp(1, 201))
    .load(conn)
    .await
}

/// [`alert_regressed_issues`], narrowed to a set of **enrollment** environment
/// ids. See [`alert_new_issues_env`] for why the EXISTS uses the issue's own
/// timestamps rather than the tick window.
pub async fn alert_regressed_issues_env(
    conn: &mut AsyncPgConnection,
    app_ids: &[Uuid],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    level: Option<&str>,
    env_ids: &[Uuid],
    limit: i64,
) -> QueryResult<Vec<AlertIssueBrief>> {
    if env_ids.is_empty() {
        return Ok(Vec::new());
    }
    diesel::sql_query(
        "SELECT i.id, i.app_id, i.title, i.level, i.times_seen FROM issues i \
         WHERE i.app_id = ANY($1) AND i.status IN ('resolved','ignored') \
           AND i.last_event_at > $2 AND i.last_event_at <= $3 \
           AND ($4::text IS NULL OR i.level = $4) \
           AND EXISTS ( \
                 SELECT 1 FROM error_events e \
                  WHERE e.issue_id = i.id \
                    AND e.environment_id = ANY($5) \
                    AND e.occurred_at >  i.first_seen - interval '1 hour' \
                    AND e.occurred_at <= i.last_event_at \
           ) \
         ORDER BY i.last_event_at DESC LIMIT $6",
    )
    .bind::<diesel::sql_types::Array<SqlUuid>, _>(app_ids)
    .bind::<Timestamptz, _>(from)
    .bind::<Timestamptz, _>(to)
    .bind::<Nullable<Text>, _>(level)
    .bind::<diesel::sql_types::Array<SqlUuid>, _>(env_ids)
    .bind::<BigInt, _>(limit.clamp(1, 201))
    .load(conn)
    .await
}

// ===========================================================================
// Transactional email outbox
// ===========================================================================

/// Queue one rendered message, subject to a per-recipient suppression window,
/// and optionally throw it away without telling the caller.
///
/// One statement, no `conn.transaction`: the dedup probe and the INSERT have to
/// be atomic, and `INSERT ... SELECT ... WHERE` gives that for free.
///
/// `ttl_secs` is the CALLER'S, not the kind's. The only code that knows how long
/// a body is worth delivering is whatever minted the credential inside it, and
/// `password_reset` alone spans two token lifetimes an order of magnitude apart.
///
/// `dedup_secs` is the only chokepoint where a per-recipient cap can live. The
/// `status <> 'failed'` term means a permanently-failed attempt does not block a
/// genuine retry. `0` disables suppression.
///
/// `commit` is how the timing oracle is closed. `enqueue` is only reachable when
/// a user row was found, so without it an existing address pays a render plus a
/// round trip and an unknown address pays nothing — the same class of gap
/// `spend_dummy_verify` exists to close on the login path. `commit = false` runs
/// the same statement, against the same index, over the network, and inserts
/// nothing. The honest claim is not "identical cost"; it is that the SMTP round
/// trip is off the request path entirely and the enqueue itself costs one round
/// trip either way, leaving only a planner-level difference orders of magnitude
/// below network jitter.
pub async fn enqueue_mail(
    conn: &mut AsyncPgConnection,
    row: NewMailOutbox<'_>,
    ttl_secs: i64,
    dedup_secs: i64,
    commit: bool,
) -> QueryResult<Option<Uuid>> {
    #[derive(QueryableByName)]
    struct Inserted {
        #[diesel(sql_type = SqlUuid)]
        id: Uuid,
    }

    let inserted: Vec<Inserted> = diesel::sql_query(
        "INSERT INTO mail_outbox (kind, recipient, recipient_key, subject, body_text, \
                                  body_html, user_id, expires_at) \
         SELECT $1, $2, $3, $4, $5, $6, $7, now() + make_interval(secs => $8::double precision) \
          WHERE $10 \
            AND ($9 = 0 OR NOT EXISTS ( \
                  SELECT 1 FROM mail_outbox \
                   WHERE kind = $1 AND recipient_key = $3 AND status <> 'failed' \
                     AND created_at > now() - make_interval(secs => $9::double precision))) \
         RETURNING id",
    )
    .bind::<Text, _>(row.kind)
    .bind::<Text, _>(row.recipient)
    .bind::<Text, _>(row.recipient_key)
    .bind::<Text, _>(row.subject)
    .bind::<Text, _>(row.body_text)
    .bind::<Text, _>(row.body_html)
    .bind::<Nullable<SqlUuid>, _>(row.user_id)
    .bind::<BigInt, _>(ttl_secs)
    .bind::<BigInt, _>(dedup_secs)
    .bind::<Bool, _>(commit)
    .get_results(conn)
    .await?;

    Ok(inserted.into_iter().next().map(|r| r.id))
}

/// Atomically claim due messages and flip them to `sending` so no other drainer
/// picks the same rows.
///
/// Shape copied from `claim_due_monitors`, the concurrency-safe worker pattern
/// this repository already uses. There are zero advisory locks in this codebase
/// and this does not introduce the first one: a lock held by a process killed
/// with SIGKILL has no owner to release it, and nothing here handles SIGTERM.
///
/// `expires_at > now()` is what stops a stale message being delivered on
/// authorization that has since been revoked — a digest rendered at enqueue is a
/// snapshot, and the drain cannot consult `role_grants` because the body is
/// already rendered.
pub async fn claim_due_mail(
    conn: &mut AsyncPgConnection,
    batch: i64,
) -> QueryResult<Vec<MailOutbox>> {
    diesel::sql_query(
        "UPDATE mail_outbox SET status = 'sending', attempts = attempts + 1, updated_at = now() \
         WHERE id IN ( \
             SELECT id FROM mail_outbox \
              WHERE status = 'pending' AND next_attempt_at <= now() AND expires_at > now() \
              ORDER BY next_attempt_at FOR UPDATE SKIP LOCKED LIMIT $1 \
         ) RETURNING *",
    )
    .bind::<BigInt, _>(batch)
    .get_results(conn)
    .await
}

/// Push a claimed row's `updated_at` forward immediately before its send.
///
/// This is what makes the stale-row threshold independent of the batch size and
/// the send concurrency: without it, the last row in a batch can sit for the
/// whole batch's duration before its send even starts, and the next person to
/// tune those two numbers without re-deriving the threshold reintroduces a
/// duplicate reset email.
pub async fn heartbeat_mail(conn: &mut AsyncPgConnection, id: Uuid) -> QueryResult<usize> {
    diesel::sql_query(
        "UPDATE mail_outbox SET updated_at = now() WHERE id = $1 AND status = 'sending'",
    )
    .bind::<SqlUuid, _>(id)
    .execute(conn)
    .await
}

/// Complete a claimed row and scrub its body.
///
/// The `status = 'sending' AND attempts = $2` fence is load-bearing: without it a
/// slow drainer whose row was reclaimed underneath it can blank and mark `sent` a
/// row another drainer is mid-send on. Returns the affected count so the caller
/// can log a lost claim at `warn!` rather than silently doing nothing.
///
/// `sink` writes `status = 'sink'`, never `'sent'` — `sent` is the one observable
/// this design offers, and a sink row reporting it makes the single place an
/// operator would look actively lie.
pub async fn mark_mail_sent(
    conn: &mut AsyncPgConnection,
    id: Uuid,
    attempts: i32,
    sink: bool,
) -> QueryResult<usize> {
    diesel::sql_query(
        "UPDATE mail_outbox \
            SET status = CASE WHEN $3 THEN 'sink' ELSE 'sent' END, \
                sent_at = now(), updated_at = now(), \
                body_text = '', body_html = '', \
                last_error = CASE WHEN $3 THEN 'delivered to log sink (SMTP_SINK=1)' ELSE NULL END \
          WHERE id = $1 AND status = 'sending' AND attempts = $2",
    )
    .bind::<SqlUuid, _>(id)
    .bind::<Integer, _>(attempts)
    .bind::<Bool, _>(sink)
    .execute(conn)
    .await
}

/// Record a failed attempt: back to `pending` with backoff, or `failed`.
///
/// Ladder: 30/60/120/240/480/900/900 seconds, about 45 minutes of coverage at the
/// default `max_attempts` of 8. The exponent is clamped at 6 because
/// `POWER(2, attempts - 1)::int` overflows an `int` once an operator hand-bumps
/// `max_attempts` past ~38 — and the clamp changes nothing below that, since
/// `LEAST(900, ...)` has already flattened the ladder by then.
///
/// It deliberately does NOT blank the body. Blanking on failure is what made a
/// misclassification irreversible; the expiry sweep covers the credential
/// instead, and until `expires_at` passes an operator can requeue the row by hand.
pub async fn mark_mail_failed(
    conn: &mut AsyncPgConnection,
    id: Uuid,
    attempts: i32,
    error: &str,
    permanent: bool,
) -> QueryResult<usize> {
    diesel::sql_query(
        "UPDATE mail_outbox \
            SET status = CASE WHEN $4 OR attempts >= max_attempts THEN 'failed' ELSE 'pending' END, \
                last_error = $3, \
                next_attempt_at = now() + make_interval(secs => \
                    LEAST(900, (30 * POWER(2, LEAST(GREATEST(attempts - 1, 0), 6)))::int)), \
                updated_at = now() \
          WHERE id = $1 AND status = 'sending' AND attempts = $2",
    )
    .bind::<SqlUuid, _>(id)
    .bind::<Integer, _>(attempts)
    .bind::<Text, _>(error)
    .bind::<Bool, _>(permanent)
    .execute(conn)
    .await
}

/// Recover rows orphaned by a process killed mid-send.
///
/// Nothing else ever reclaims them: `claim_due_mail` only looks at `pending`.
///
/// Three guards, each covering a failure the obvious version has. The
/// `attempts >= max_attempts` branch exists because the give-up decision
/// otherwise lives only in `mark_mail_failed`, which a process that crashed or
/// was OOM-killed never reaches — so a row whose send reliably kills the process
/// would be claimed, orphaned, requeued and claimed again, forever. Resetting
/// `next_attempt_at` exists because a requeued row is otherwise immediately
/// eligible for the very next claim, bypassing the backoff ladder on exactly the
/// path that most needs it. And the `updated_at` window is what the per-send
/// heartbeat keeps honest.
pub async fn requeue_stuck_mail(
    conn: &mut AsyncPgConnection,
    stale_secs: i64,
) -> QueryResult<usize> {
    diesel::sql_query(
        "UPDATE mail_outbox \
            SET status = CASE WHEN attempts >= max_attempts THEN 'failed' ELSE 'pending' END, \
                last_error = CASE WHEN attempts >= max_attempts \
                             THEN 'orphaned mid-send ' || attempts || ' times; giving up' \
                             ELSE 'orphaned mid-send; requeued' END, \
                next_attempt_at = now() + make_interval(secs => \
                    LEAST(900, (30 * POWER(2, LEAST(GREATEST(attempts - 1, 0), 6)))::int)), \
                updated_at = now() \
          WHERE status = 'sending' AND updated_at < now() - make_interval(secs => $1::double precision)",
    )
    .bind::<BigInt, _>(stale_secs)
    .execute(conn)
    .await
}

/// Fail every non-terminal row whose own deadline has passed.
///
/// Neither this nor [`blank_expired_mail_bodies`] is indexed: the non-terminal
/// set is small by construction, and every status transition already rewrites two
/// partial indexes, so a fifth index costs more than these sweeps save.
pub async fn expire_stale_mail(conn: &mut AsyncPgConnection) -> QueryResult<usize> {
    diesel::sql_query(
        "UPDATE mail_outbox \
            SET status = 'failed', last_error = 'expired before delivery', updated_at = now() \
          WHERE status IN ('pending', 'sending') AND expires_at < now()",
    )
    .execute(conn)
    .await
}

/// Scrub the body of any row past its own `expires_at`, whatever its status.
///
/// Takes no age argument on purpose. The row already carries the only deadline
/// that means anything, and a second flat constant sitting beside it is the drift
/// that scrubs a live 24-hour reset link at the one-hour mark — destroying the
/// manual requeue path while the token it carried stays valid for another 23
/// hours.
///
/// Blanking a row the drain is mid-send on is harmless: `claim_due_mail` returned
/// the body by value, so the sender is working from its own copy.
pub async fn blank_expired_mail_bodies(conn: &mut AsyncPgConnection) -> QueryResult<usize> {
    diesel::sql_query(
        "UPDATE mail_outbox SET body_text = '', body_html = '', updated_at = now() \
          WHERE (body_text <> '' OR body_html <> '') AND expires_at < now()",
    )
    .execute(conn)
    .await
}

/// Delete up to `batch` terminal rows older than `older_than_days`. Call in a
/// loop until it returns 0.
///
/// Bounded and non-blocking, unlike `prune_alert_events`, which is an unbounded
/// DELETE — that one runs in a standalone worker, this one runs inside
/// `sauron-api`, which serves HTTP from a 16-connection pool. An operator
/// lowering `MAIL_OUTBOX_RETENTION_DAYS` after a digest run would otherwise hold
/// one of those 16 for minutes.
///
/// The `FOR UPDATE SKIP LOCKED` is also what lets N API instances reap
/// concurrently without serialising on row locks.
pub async fn prune_mail_outbox(
    conn: &mut AsyncPgConnection,
    older_than_days: i64,
    batch: i64,
) -> QueryResult<usize> {
    diesel::sql_query(
        "DELETE FROM mail_outbox WHERE id IN ( \
             SELECT id FROM mail_outbox \
              WHERE status IN ('sent', 'failed', 'sink') \
                AND created_at < now() - ($1 || ' days')::interval \
              ORDER BY created_at LIMIT $2 FOR UPDATE SKIP LOCKED)",
    )
    .bind::<Text, _>(older_than_days.to_string())
    .bind::<BigInt, _>(batch)
    .execute(conn)
    .await
}

/// `(pending_count, age_of_oldest_pending_row_in_seconds)`.
///
/// The only queue-depth signal this slice ships, and it is logged
/// unconditionally: there is no metrics endpoint and no admin view, so without it
/// a stalled queue is invisible until a user reports that password reset does not
/// work.
pub async fn mail_outbox_depth(conn: &mut AsyncPgConnection) -> QueryResult<(i64, Option<i64>)> {
    #[derive(QueryableByName)]
    struct Depth {
        #[diesel(sql_type = BigInt)]
        pending: i64,
        #[diesel(sql_type = Nullable<BigInt>)]
        oldest_secs: Option<i64>,
    }

    let row: Depth = diesel::sql_query(
        "SELECT count(*)::bigint AS pending, \
                (EXTRACT(EPOCH FROM (now() - min(created_at))))::bigint AS oldest_secs \
           FROM mail_outbox WHERE status = 'pending'",
    )
    .get_result(conn)
    .await?;
    Ok((row.pending, row.oldest_secs))
}

#[cfg(test)]
mod revocation_reason_tests {
    use super::*;

    /// Every `REVOKE_*` constant must fall into exactly one of three buckets, so
    /// that a new reason cannot be added without someone choosing a bucket for
    /// it. The blanket formulation "everything except REVOKE_REUSE" is actively
    /// dangerous: it sweeps up `REVOKE_ROTATED` (which would send every ordinary
    /// rotation down the early-return path and break the 10-second multi-tab
    /// grace window) and `REVOKE_LOGOUT` (which would disable replay detection
    /// on exactly the tokens where a replay is most diagnostic).
    #[test]
    fn every_revocation_reason_is_classified() {
        /// Bucket two: `refresh` already handles these in a branch of its own.
        const HAS_ITS_OWN_BRANCH: [&str; 2] = [REVOKE_ROTATED, REVOKE_DEACTIVATED];
        /// Bucket three: presenting a token revoked for one of these IS worth
        /// the theft alarm, so they fall through to the family kill.
        /// `REVOKE_PASSWORD_CHANGED` belongs here and not in bucket one: the
        /// user changed their own password, every session died with it, and a
        /// token surfacing afterwards is exactly the replay the alarm is for.
        const FALLS_THROUGH_TO_THE_ALARM: [&str; 3] =
            [REVOKE_REUSE, REVOKE_LOGOUT, REVOKE_PASSWORD_CHANGED];

        const ALL_REASONS: [&str; 10] = [
            REVOKE_ROTATED,
            REVOKE_LOGOUT,
            REVOKE_REUSE,
            REVOKE_DEACTIVATED,
            REVOKE_PASSWORD_CHANGED,
            REVOKE_USER_REVOKED,
            REVOKE_USER_REVOKED_OTHERS,
            REVOKE_ADMIN,
            REVOKE_PASSWORD_RESET,
            REVOKE_RESET_FORCED,
        ];

        for reason in ALL_REASONS {
            let buckets = [
                DELIBERATE_REVOKE_REASONS.contains(&reason),
                HAS_ITS_OWN_BRANCH.contains(&reason),
                FALLS_THROUGH_TO_THE_ALARM.contains(&reason),
            ]
            .into_iter()
            .filter(|hit| *hit)
            .count();
            assert_eq!(
                buckets, 1,
                "{reason} must belong to exactly one bucket, not {buckets}"
            );
        }
    }

    /// The cheapest possible defence against the deploy coupling: a reason the
    /// `auth_sessions_revoked_reason_check` CHECK does not list makes the revoke
    /// path 500 in production, and nothing else in the build would notice.
    #[test]
    fn every_reason_that_can_revoke_a_session_is_in_the_check_constraint() {
        const UP_SQL: &str =
            include_str!("../../../migrations/2026-08-01-000035_auth_sessions/up.sql");

        // Every assertion below is scoped to the CHECK body, never to the whole
        // file. up.sql's prose comment names 'rotated' twice -- explaining why
        // it is EXCLUDED -- and names several of the accepted reasons as well,
        // so matching the file would test the comment: the positive assertions
        // would pass even against an empty constraint, and the negative one
        // would fail against a correct one.
        const MARKER: &str = "CONSTRAINT auth_sessions_revoked_reason_check CHECK (";
        let start = UP_SQL
            .find(MARKER)
            .expect("up.sql declares auth_sessions_revoked_reason_check by name")
            + MARKER.len();
        let after = &UP_SQL[start..];
        let end = after
            .find(");")
            .expect("the CHECK is the last item in CREATE TABLE auth_sessions");
        let check = &after[..end];

        for reason in [
            REVOKE_LOGOUT,
            REVOKE_REUSE,
            REVOKE_DEACTIVATED,
            REVOKE_PASSWORD_CHANGED,
            REVOKE_USER_REVOKED,
            REVOKE_USER_REVOKED_OTHERS,
            REVOKE_ADMIN,
            REVOKE_PASSWORD_RESET,
            REVOKE_RESET_FORCED,
        ] {
            assert!(
                check.contains(&format!("'{reason}'")),
                "'{reason}' is missing from auth_sessions_revoked_reason_check; the revoke path \
                 will 500 in production"
            );
        }

        // A rotation revokes a token, never a session. Listing it would make the
        // database stop catching that bug.
        assert!(
            !check.contains(&format!("'{REVOKE_ROTATED}'")),
            "'{REVOKE_ROTATED}' must NOT be accepted by auth_sessions_revoked_reason_check"
        );
    }
}

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

// ===========================================================================
// Personal notification subscriptions (S3)
//
// Two environment id spaces meet in this section and confusing them produces a
// subscription that matches nothing, silently:
//   * `notification_subscription_envs.environment_id` is a CATALOGUE id
//     (`environments.id`, project-level since migration 33).
//   * `notification_queue_envs.environment_id`, `error_events.environment_id`
//     and `role_grants.scope_id` for `scope_type='env'` are ENROLLMENT ids
//     (`app_environments.id`).
// `live_enrollments_for_apps` is the only sanctioned bridge.
// ===========================================================================

/// Create or update a subscription and replace its environment set, in ONE
/// data-modifying CTE.
///
/// One statement means atomicity without `conn.transaction`, which the MSRV
/// blocks. A two-statement version could leave the parent updated and the child
/// rows stale — and a stale-empty child set is read everywhere downstream as
/// "all environments", which WIDENS the subscription rather than narrowing it.
#[allow(clippy::too_many_arguments)]
pub async fn upsert_subscription(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    org_id: Uuid,
    scope_type: &str,
    scope_id: Uuid,
    kind: &str,
    conditions: &Value,
    delivery: &str,
    throttle_seconds: i32,
    quiet_start_min: Option<i16>,
    quiet_end_min: Option<i16>,
    quiet_tz: &str,
    env_ids: &[Uuid],
) -> QueryResult<NotificationSubscription> {
    diesel::sql_query(
        "WITH up AS ( \
             INSERT INTO notification_subscriptions \
                 (user_id, org_id, scope_type, scope_id, kind, conditions, delivery, \
                  throttle_seconds, quiet_start_min, quiet_end_min, quiet_tz, \
                  enabled, disabled_reason, disabled_at, last_evaluated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, true, NULL, NULL, now()) \
             ON CONFLICT (user_id, scope_type, scope_id, kind) DO UPDATE SET \
                 org_id = EXCLUDED.org_id, \
                 conditions = EXCLUDED.conditions, \
                 delivery = EXCLUDED.delivery, \
                 throttle_seconds = EXCLUDED.throttle_seconds, \
                 quiet_start_min = EXCLUDED.quiet_start_min, \
                 quiet_end_min = EXCLUDED.quiet_end_min, \
                 quiet_tz = EXCLUDED.quiet_tz, \
                 enabled = true, \
                 disabled_reason = NULL, \
                 disabled_at = NULL, \
                 updated_at = now() \
             RETURNING * \
         ), del AS ( \
             DELETE FROM notification_subscription_envs \
              WHERE subscription_id = (SELECT id FROM up) \
                AND environment_id <> ALL($12) \
         ), ins AS ( \
             INSERT INTO notification_subscription_envs (subscription_id, environment_id) \
             SELECT (SELECT id FROM up), e FROM unnest($12::uuid[]) AS e \
             ON CONFLICT DO NOTHING \
         ) \
         SELECT * FROM up",
    )
    .bind::<SqlUuid, _>(user_id)
    .bind::<SqlUuid, _>(org_id)
    .bind::<Text, _>(scope_type)
    .bind::<SqlUuid, _>(scope_id)
    .bind::<Text, _>(kind)
    .bind::<Jsonb, _>(conditions)
    .bind::<Text, _>(delivery)
    .bind::<Integer, _>(throttle_seconds)
    .bind::<Nullable<SmallInt>, _>(quiet_start_min)
    .bind::<Nullable<SmallInt>, _>(quiet_end_min)
    .bind::<Text, _>(quiet_tz)
    .bind::<diesel::sql_types::Array<SqlUuid>, _>(env_ids)
    .get_result(conn)
    .await
}

pub async fn list_subscriptions_for_user(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
) -> QueryResult<Vec<NotificationSubscription>> {
    notification_subscriptions::table
        .filter(notification_subscriptions::user_id.eq(user_id))
        .order(notification_subscriptions::created_at.asc())
        .select(NotificationSubscription::as_select())
        .load(conn)
        .await
}

pub async fn get_subscription(
    conn: &mut AsyncPgConnection,
    id: Uuid,
) -> QueryResult<Option<NotificationSubscription>> {
    notification_subscriptions::table
        .find(id)
        .select(NotificationSubscription::as_select())
        .first(conn)
        .await
        .optional()
}

/// Owner-scoped delete. `user_id` is part of the predicate rather than checked
/// by the caller so a missing check cannot delete someone else's row; the
/// handler turns a zero row count into 404, never 403, so a non-owner learns
/// nothing about whether the id exists.
pub async fn delete_subscription(
    conn: &mut AsyncPgConnection,
    id: Uuid,
    user_id: Uuid,
) -> QueryResult<usize> {
    diesel::delete(
        notification_subscriptions::table
            .filter(notification_subscriptions::id.eq(id))
            .filter(notification_subscriptions::user_id.eq(user_id)),
    )
    .execute(conn)
    .await
}

/// Owner-driven enable/disable. Re-enabling always clears `disabled_reason`:
/// re-granting access does not silently resurrect a subscription, the user
/// turns it back on themselves, and at that moment the reason is stale.
pub async fn set_subscription_enabled(
    conn: &mut AsyncPgConnection,
    id: Uuid,
    user_id: Uuid,
    enabled: bool,
) -> QueryResult<usize> {
    diesel::update(
        notification_subscriptions::table
            .filter(notification_subscriptions::id.eq(id))
            .filter(notification_subscriptions::user_id.eq(user_id)),
    )
    .set((
        notification_subscriptions::enabled.eq(enabled),
        notification_subscriptions::disabled_reason.eq::<Option<String>>(if enabled {
            None
        } else {
            Some("unsubscribed".into())
        }),
        notification_subscriptions::disabled_at.eq::<Option<DateTime<Utc>>>(if enabled {
            None
        } else {
            Some(Utc::now())
        }),
        notification_subscriptions::updated_at.eq(Utc::now()),
    ))
    .execute(conn)
    .await
}

/// System-driven disable: the unsubscribe link (`'unsubscribed'`) and the
/// revocation sweep (`'access_revoked'`). Not owner-scoped, because neither
/// caller is the owner acting through the UI.
pub async fn disable_subscription(
    conn: &mut AsyncPgConnection,
    id: Uuid,
    reason: &str,
) -> QueryResult<usize> {
    diesel::update(notification_subscriptions::table.find(id))
        .set((
            notification_subscriptions::enabled.eq(false),
            notification_subscriptions::disabled_reason.eq(Some(reason.to_string())),
            notification_subscriptions::disabled_at.eq(Some(Utc::now())),
            notification_subscriptions::updated_at.eq(Utc::now()),
        ))
        .execute(conn)
        .await
}

/// `(subscription_id, catalogue_environment_id)` for many subscriptions at
/// once — the evaluator resolves every subscription's environment set in one
/// query, never one per subscription.
pub async fn subscription_envs_for(
    conn: &mut AsyncPgConnection,
    subscription_ids: &[Uuid],
) -> QueryResult<Vec<(Uuid, Uuid)>> {
    if subscription_ids.is_empty() {
        return Ok(Vec::new());
    }
    notification_subscription_envs::table
        .filter(notification_subscription_envs::subscription_id.eq_any(subscription_ids.to_vec()))
        .select((
            notification_subscription_envs::subscription_id,
            notification_subscription_envs::environment_id,
        ))
        .load(conn)
        .await
}

/// Live CATALOGUE environment ids of a project — what a subscription's
/// `environment_ids` are validated against, and what the dashboard's chip row
/// offers.
pub async fn live_catalogue_envs_for_project(
    conn: &mut AsyncPgConnection,
    project_id: Uuid,
) -> QueryResult<Vec<Uuid>> {
    environments::table
        .filter(environments::project_id.eq(project_id))
        .filter(environments::retired_at.is_null())
        .order(environments::name.asc())
        .select(environments::id)
        .load(conn)
        .await
}

#[derive(Debug, QueryableByName)]
struct BoolRow {
    #[diesel(sql_type = Bool)]
    ok: bool,
}

/// Whether `tz` is a zone this Postgres knows.
///
/// Validated at write time so a typo is a 400 rather than a row the enqueue
/// then has to defend against. The enqueue re-checks anyway: a zone that
/// validated here can vanish with an OS tzdata update, and
/// `now() AT TIME ZONE 'Missing/Zone'` raises, which would kill the whole
/// batch over one bad row.
pub async fn timezone_exists(conn: &mut AsyncPgConnection, tz: &str) -> QueryResult<bool> {
    let row: BoolRow =
        diesel::sql_query("SELECT EXISTS(SELECT 1 FROM pg_timezone_names WHERE name = $1) AS ok")
            .bind::<Text, _>(tz)
            .get_result(conn)
            .await?;
    Ok(row.ok)
}

/// Every enabled subscription of the given kinds, in one query, served by
/// `notification_subscriptions_kind_idx (kind) WHERE enabled`.
pub async fn enabled_subscriptions_by_kinds(
    conn: &mut AsyncPgConnection,
    kinds: &[&str],
) -> QueryResult<Vec<NotificationSubscription>> {
    if kinds.is_empty() {
        return Ok(Vec::new());
    }
    notification_subscriptions::table
        .filter(notification_subscriptions::enabled.eq(true))
        .filter(
            notification_subscriptions::kind
                .eq_any(kinds.iter().map(|k| k.to_string()).collect::<Vec<_>>()),
        )
        .select(NotificationSubscription::as_select())
        .load(conn)
        .await
}

/// Enabled uptime subscriptions on exactly this project.
///
/// The prober calls this from `notify_transition`; the caller still runs the
/// coverage predicate against freshly loaded grants, because a subscription's
/// owner may have lost project reach since it was created.
pub async fn uptime_subscriptions_for_project(
    conn: &mut AsyncPgConnection,
    project_id: Uuid,
) -> QueryResult<Vec<NotificationSubscription>> {
    notification_subscriptions::table
        .filter(notification_subscriptions::enabled.eq(true))
        .filter(notification_subscriptions::kind.eq("uptime"))
        .filter(notification_subscriptions::scope_type.eq("project"))
        .filter(notification_subscriptions::scope_id.eq(project_id))
        .select(NotificationSubscription::as_select())
        .load(conn)
        .await
}

/// Every subscription a user holds inside one org — what the synchronous
/// revocation sweep re-evaluates after a grant change commits.
pub async fn subscriptions_for_user_in_org(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    org_id: Uuid,
) -> QueryResult<Vec<NotificationSubscription>> {
    notification_subscriptions::table
        .filter(notification_subscriptions::user_id.eq(user_id))
        .filter(notification_subscriptions::org_id.eq(org_id))
        .filter(notification_subscriptions::enabled.eq(true))
        .select(NotificationSubscription::as_select())
        .load(conn)
        .await
}

/// `(project_id, app_id)` for every app under any of `project_ids` — the
/// batched `list_apps_for_project`.
///
/// The evaluation pass resolves N project-scoped subscriptions per tick. Calling
/// `list_apps_for_project` once each is N round trips against a pool of 8 shared
/// with the drain, which is precisely the per-subscription blow-up the probe
/// coalescing exists to prevent; doing it in the resolution loop would put the
/// cost back one layer down.
pub async fn apps_for_projects(
    conn: &mut AsyncPgConnection,
    project_ids: &[Uuid],
) -> QueryResult<Vec<(Uuid, Uuid)>> {
    if project_ids.is_empty() {
        return Ok(Vec::new());
    }
    apps::table
        .filter(apps::project_id.eq_any(project_ids.to_vec()))
        .select((apps::project_id, apps::id))
        .load(conn)
        .await
}

/// Advance the watermark on a batch of subscriptions in one statement.
pub async fn touch_subscriptions_evaluated(
    conn: &mut AsyncPgConnection,
    ids: &[Uuid],
    at: DateTime<Utc>,
) -> QueryResult<usize> {
    if ids.is_empty() {
        return Ok(0);
    }
    diesel::update(
        notification_subscriptions::table
            .filter(notification_subscriptions::id.eq_any(ids.to_vec())),
    )
    .set(notification_subscriptions::last_evaluated_at.eq(at))
    .execute(conn)
    .await
}

/// One row to enqueue. The environment list is **enrollment** ids
/// (`app_environments.id`) — what the events the body was computed from
/// actually carry, and what the drain's coverage check compares against
/// `Reach.envs`.
#[derive(Debug, Clone)]
pub struct QueueInsert<'a> {
    pub subscription_id: Uuid,
    pub project_id: Uuid,
    /// `None` for uptime.
    pub app_id: Option<Uuid>,
    pub includes_unattributed: bool,
    pub kind: &'a str,
    pub dedup_key: &'a str,
    pub severity: &'a str,
    pub title: &'a str,
    pub body: &'a str,
    pub link: Option<&'a str>,
    pub env_enrollments: Vec<Uuid>,
}

/// Insert a batch of notifications and their environment child rows in ONE
/// data-modifying CTE, computing `deliver_after` in SQL.
///
/// `deliver_after` HAS to be computed here: the workspace has no `chrono-tz`
/// (adding one is a workspace-dependency edit affecting every crate), so
/// nothing in Rust can produce a subscription's local wall-clock time.
///
/// The `pg_timezone_names` lookup is not paranoia. A zone that validated at
/// write time can vanish with an OS tzdata update, and
/// `now() AT TIME ZONE 'Missing/Zone'` RAISES — one bad row would kill the
/// whole batch. Falling back to UTC is visible in the account card (which
/// renders the effective zone) rather than silent.
///
/// The env rows are in the same statement because a queue row with a stale-empty
/// env list is read downstream as "the body spans everything", so a partial
/// failure would WIDEN a row's implied scope instead of narrowing it.
pub async fn enqueue_notifications(
    conn: &mut AsyncPgConnection,
    rows: &[QueueInsert<'_>],
) -> QueryResult<i64> {
    if rows.is_empty() {
        return Ok(0);
    }
    let sub_ids: Vec<Uuid> = rows.iter().map(|r| r.subscription_id).collect();
    let project_ids: Vec<Uuid> = rows.iter().map(|r| r.project_id).collect();
    let app_ids: Vec<Option<Uuid>> = rows.iter().map(|r| r.app_id).collect();
    let unattributed: Vec<bool> = rows.iter().map(|r| r.includes_unattributed).collect();
    let kinds: Vec<String> = rows.iter().map(|r| r.kind.to_string()).collect();
    let dedups: Vec<String> = rows.iter().map(|r| r.dedup_key.to_string()).collect();
    let severities: Vec<String> = rows.iter().map(|r| r.severity.to_string()).collect();
    let titles: Vec<String> = rows.iter().map(|r| r.title.to_string()).collect();
    let bodies: Vec<String> = rows.iter().map(|r| r.body.to_string()).collect();
    let links: Vec<Option<String>> = rows.iter().map(|r| r.link.map(String::from)).collect();

    // Parallel arrays of (dedup_key, enrollment_id). `dedup_key` embeds the
    // subscription id, so it is unique within one batch and can join the child
    // rows back to their parent without a second round trip.
    let mut env_keys: Vec<String> = Vec::new();
    let mut env_ids: Vec<Uuid> = Vec::new();
    for r in rows {
        for e in &r.env_enrollments {
            env_keys.push(r.dedup_key.to_string());
            env_ids.push(*e);
        }
    }

    let row: AlertCountRow = diesel::sql_query(
        "WITH v AS ( \
             SELECT * FROM unnest($1::uuid[], $2::uuid[], $3::uuid[], $4::bool[], $5::text[], \
                                  $6::text[], $7::text[], $8::text[], $9::text[], $10::text[]) \
                    AS t(subscription_id, project_id, app_id, includes_unattributed, kind, \
                         dedup_key, severity, title, body, link) \
         ), j AS ( \
             SELECT v.*, s.user_id, s.org_id, s.delivery, s.quiet_start_min, s.quiet_end_min, \
                    COALESCE((SELECT n.name FROM pg_timezone_names n WHERE n.name = s.quiet_tz), \
                             'UTC') AS tz \
               FROM v JOIN notification_subscriptions s ON s.id = v.subscription_id \
         ), b AS ( \
             SELECT j.*, \
                    CASE j.delivery \
                      WHEN 'hourly' THEN date_trunc('hour', now()) + interval '1 hour' \
                      WHEN 'daily'  THEN (date_trunc('day', now() AT TIME ZONE j.tz) \
                                          + interval '1 day') AT TIME ZONE j.tz \
                      ELSE now() \
                    END AS base \
               FROM j \
         ), q AS ( \
             SELECT b.*, \
                    (EXTRACT(HOUR FROM (b.base AT TIME ZONE b.tz)) * 60 \
                     + EXTRACT(MINUTE FROM (b.base AT TIME ZONE b.tz)))::int AS local_min, \
                    date_trunc('day', b.base AT TIME ZONE b.tz) AS local_day \
               FROM b \
         ), ins AS ( \
             INSERT INTO notification_queue \
                 (subscription_id, user_id, org_id, project_id, app_id, includes_unattributed, \
                  kind, dedup_key, severity, title, body, link, deliver_after) \
             SELECT q.subscription_id, q.user_id, q.org_id, q.project_id, q.app_id, \
                    q.includes_unattributed, q.kind, q.dedup_key, q.severity, q.title, q.body, \
                    q.link, \
                    CASE \
                      WHEN q.quiet_start_min IS NULL THEN q.base \
                      WHEN q.quiet_start_min = q.quiet_end_min THEN q.base \
                      WHEN q.quiet_start_min < q.quiet_end_min THEN \
                        CASE WHEN q.local_min >= q.quiet_start_min \
                              AND q.local_min <  q.quiet_end_min \
                             THEN (q.local_day + make_interval(mins => q.quiet_end_min)) \
                                  AT TIME ZONE q.tz \
                             ELSE q.base END \
                      ELSE \
                        CASE WHEN q.local_min >= q.quiet_start_min \
                             THEN (q.local_day + interval '1 day' \
                                   + make_interval(mins => q.quiet_end_min)) AT TIME ZONE q.tz \
                             WHEN q.local_min < q.quiet_end_min \
                             THEN (q.local_day + make_interval(mins => q.quiet_end_min)) \
                                  AT TIME ZONE q.tz \
                             ELSE q.base END \
                    END \
               FROM q \
             ON CONFLICT (subscription_id, dedup_key) WHERE status IN ('pending','claimed') \
             DO NOTHING \
             RETURNING id, dedup_key \
         ), envs AS ( \
             INSERT INTO notification_queue_envs (queue_id, environment_id) \
             SELECT ins.id, e.env_id \
               FROM ins JOIN unnest($11::text[], $12::uuid[]) AS e(dk, env_id) \
                 ON e.dk = ins.dedup_key \
             ON CONFLICT DO NOTHING \
             RETURNING queue_id \
         ) \
         SELECT count(*) AS n FROM ins",
    )
    .bind::<diesel::sql_types::Array<SqlUuid>, _>(sub_ids)
    .bind::<diesel::sql_types::Array<SqlUuid>, _>(project_ids)
    .bind::<diesel::sql_types::Array<Nullable<SqlUuid>>, _>(app_ids)
    .bind::<diesel::sql_types::Array<Bool>, _>(unattributed)
    .bind::<diesel::sql_types::Array<Text>, _>(kinds)
    .bind::<diesel::sql_types::Array<Text>, _>(dedups)
    .bind::<diesel::sql_types::Array<Text>, _>(severities)
    .bind::<diesel::sql_types::Array<Text>, _>(titles)
    .bind::<diesel::sql_types::Array<Text>, _>(bodies)
    .bind::<diesel::sql_types::Array<Nullable<Text>>, _>(links)
    .bind::<diesel::sql_types::Array<Text>, _>(env_keys)
    .bind::<diesel::sql_types::Array<SqlUuid>, _>(env_ids)
    .get_result(conn)
    .await?;
    Ok(row.n)
}

/// Durable throttle backstop: was a notification with this dedup key enqueued
/// for this subscription within the last `within_seconds`?
///
/// Used when Redis is unavailable. Extending the key with the subscription id
/// is what gives per-RECIPIENT throttling with no new infrastructure — the org
/// engine's equivalent (`alert_recently_sent`) is per rule.
pub async fn notification_recently_queued(
    conn: &mut AsyncPgConnection,
    subscription_id: Uuid,
    dedup_key: &str,
    within_seconds: i32,
) -> QueryResult<bool> {
    if within_seconds <= 0 {
        return Ok(false);
    }
    let cutoff = Utc::now() - chrono::Duration::seconds(within_seconds as i64);
    let n: i64 = notification_queue::table
        .filter(notification_queue::subscription_id.eq(subscription_id))
        .filter(notification_queue::dedup_key.eq(dedup_key))
        .filter(notification_queue::created_at.gt(cutoff))
        .count()
        .get_result(conn)
        .await?;
    Ok(n > 0)
}

/// How long a `claimed` row may sit before the requeue reclaims it.
pub const STUCK_CLAIM_SECS: i64 = 900;
/// How many claims a row gets before it is abandoned as `failed`.
pub const MAX_QUEUE_ATTEMPTS: i16 = 3;

/// Claim due notifications for exclusive delivery.
///
/// The `status = 'claimed'` write is the entire point and is the one thing that
/// cannot be copied from `claim_due_monitors` without thinking. THAT query's
/// exclusivity comes from its SET clause — `next_check_at = now() + …` moves
/// the row out of the inner SELECT's predicate at commit. `FOR UPDATE SKIP
/// LOCKED` alone only skips rows locked by an UNCOMMITTED transaction; once one
/// replica commits, another replica's next pass re-selects the same rows and
/// mails them again. A `claimed` state that leaves the partial index is what
/// makes the claim real, and `attempts` is what makes a crash between claim and
/// terminal status recoverable instead of an infinite redelivery loop.
pub async fn claim_due_notifications(
    conn: &mut AsyncPgConnection,
    batch: i64,
) -> QueryResult<Vec<NotificationQueueItem>> {
    diesel::sql_query(
        "UPDATE notification_queue \
            SET status = 'claimed', claimed_at = now(), attempts = attempts + 1 \
          WHERE id IN ( \
              SELECT id FROM notification_queue \
               WHERE status = 'pending' AND deliver_after <= now() \
               ORDER BY deliver_after \
               FOR UPDATE SKIP LOCKED \
               LIMIT $1 \
          ) RETURNING *",
    )
    .bind::<BigInt, _>(batch.clamp(1, 5000))
    .load(conn)
    .await
}

/// Stamp one delivered message across every row it carried.
pub async fn mark_notifications_sent(
    conn: &mut AsyncPgConnection,
    ids: &[Uuid],
    message_id: Uuid,
) -> QueryResult<usize> {
    if ids.is_empty() {
        return Ok(0);
    }
    diesel::sql_query(
        "UPDATE notification_queue \
            SET status = 'sent', message_id = $2, sent_at = now(), finished_at = now() \
          WHERE id = ANY($1)",
    )
    .bind::<diesel::sql_types::Array<SqlUuid>, _>(ids)
    .bind::<SqlUuid, _>(message_id)
    .execute(conn)
    .await
}

/// Terminally drop rows and BLANK their content in the same statement.
///
/// A dropped row's title/body/link have no further purpose and must not sit at
/// rest for the retention window outside the reader's authorization — which,
/// for `dropped_no_access`, is exactly the authorization that just failed.
pub async fn drop_notifications(
    conn: &mut AsyncPgConnection,
    ids: &[Uuid],
    status: &str,
) -> QueryResult<usize> {
    if ids.is_empty() {
        return Ok(0);
    }
    diesel::sql_query(
        "UPDATE notification_queue \
            SET status = $2, title = NULL, body = NULL, link = NULL, finished_at = now() \
          WHERE id = ANY($1)",
    )
    .bind::<diesel::sql_types::Array<SqlUuid>, _>(ids)
    .bind::<Text, _>(status)
    .execute(conn)
    .await
}

/// Record a delivery failure without blanking the body, so a later requeue can
/// still send it — but only while there is a retry left.
///
/// The attempts guard is load-bearing and is the ONLY thing that terminates a
/// deterministic failure. `requeue_stuck_notifications` cannot help here: it
/// matches `WHERE status = 'claimed' AND claimed_at < …`, and a row this
/// function returns to `pending` is neither. A render that fails on its own
/// content — a `format!` that panics on a malformed body, an outbox that
/// rejects the row every time — would otherwise be re-claimed, re-failed and
/// re-queued forever, which is exactly the infinite redelivery loop
/// `MAX_QUEUE_ATTEMPTS` exists to stop.
///
/// `attempts` was already incremented by the claim, so `>= max_attempts` here
/// means "this was the last try".
pub async fn fail_notifications(
    conn: &mut AsyncPgConnection,
    ids: &[Uuid],
    error: &str,
    max_attempts: i16,
) -> QueryResult<usize> {
    if ids.is_empty() {
        return Ok(0);
    }
    diesel::sql_query(
        "UPDATE notification_queue \
            SET status      = CASE WHEN attempts >= $3 THEN 'failed' ELSE 'pending' END, \
                finished_at = CASE WHEN attempts >= $3 THEN now() ELSE NULL END, \
                claimed_at  = NULL, \
                error       = $2 \
          WHERE id = ANY($1)",
    )
    .bind::<diesel::sql_types::Array<SqlUuid>, _>(ids)
    .bind::<Text, _>(error)
    .bind::<SmallInt, _>(max_attempts.max(1))
    .execute(conn)
    .await
}

/// Return abandoned `claimed` rows to `pending`, or give up on them.
///
/// There is no graceful shutdown anywhere in this codebase, so a process killed
/// mid-drain leaves rows `claimed` forever. `attempts >= max_attempts` is what
/// makes the give-up decision reachable rather than looping.
pub async fn requeue_stuck_notifications(
    conn: &mut AsyncPgConnection,
    stale_secs: i64,
    max_attempts: i16,
) -> QueryResult<usize> {
    diesel::sql_query(
        "UPDATE notification_queue \
            SET status      = CASE WHEN attempts >= $2 THEN 'failed' ELSE 'pending' END, \
                finished_at = CASE WHEN attempts >= $2 THEN now() ELSE NULL END, \
                error       = CASE WHEN attempts >= $2 \
                                   THEN 'abandoned after repeated claims' ELSE error END, \
                claimed_at  = NULL \
          WHERE status = 'claimed' AND claimed_at < now() - make_interval(secs => $1)",
    )
    .bind::<BigInt, _>(stale_secs.max(60))
    .bind::<SmallInt, _>(max_attempts.max(1))
    .execute(conn)
    .await
}

/// Delete terminal rows past retention.
///
/// `alert_events` is append-only audit and prunes on `created_at`; this is a
/// WORK QUEUE. Pruning on `created_at` with no status guard would destroy
/// still-`pending` rows — precisely the evidence of the outage that made them
/// pile up — and none of the other indexes leads with `created_at`, so the
/// hourly DELETE would seq-scan a churned heap.
/// `notification_queue_finished_idx` serves this predicate directly.
pub async fn prune_notification_queue(
    conn: &mut AsyncPgConnection,
    retention_days: i32,
) -> QueryResult<usize> {
    diesel::sql_query(
        "DELETE FROM notification_queue \
          WHERE finished_at IS NOT NULL \
            AND finished_at < now() - make_interval(days => $1)",
    )
    .bind::<Integer, _>(retention_days.clamp(1, 365))
    .execute(conn)
    .await
}

#[derive(Debug, QueryableByName)]
struct QueueDepthRow {
    #[diesel(sql_type = BigInt)]
    n: i64,
    #[diesel(sql_type = Nullable<Timestamptz>)]
    oldest: Option<DateTime<Utc>>,
}

/// `(pending depth, oldest pending deliver_after)`.
///
/// Nothing else in the system would reveal a backlog: `status='sent'` means only
/// "handed to the outbox", so a stalled outbox and a healthy one look identical
/// from here.
pub async fn notification_queue_depth(
    conn: &mut AsyncPgConnection,
) -> QueryResult<(i64, Option<DateTime<Utc>>)> {
    let row: QueueDepthRow = diesel::sql_query(
        "SELECT count(*) AS n, min(deliver_after) AS oldest \
           FROM notification_queue WHERE status = 'pending'",
    )
    .get_result(conn)
    .await?;
    Ok((row.n, row.oldest))
}

/// `(queue_id, enrollment_environment_id)` for many queued rows at once.
pub async fn queue_envs_for(
    conn: &mut AsyncPgConnection,
    queue_ids: &[Uuid],
) -> QueryResult<Vec<(Uuid, Uuid)>> {
    if queue_ids.is_empty() {
        return Ok(Vec::new());
    }
    notification_queue_envs::table
        .filter(notification_queue_envs::queue_id.eq_any(queue_ids.to_vec()))
        .select((
            notification_queue_envs::queue_id,
            notification_queue_envs::environment_id,
        ))
        .load(conn)
        .await
}

/// `(project_id, org_id)` for many projects at once.
///
/// The drain re-derives every queued row's org from its project rather than
/// trusting the denormalized `notification_queue.org_id`. `reach_for`'s org arm
/// is `Scope::Org(_) => reach.org = true` and never compares the org id, so if a
/// row's stored `org_id` ever diverged from the true owner of its `project_id`,
/// `reach.org` would go true and the coverage test would accept a foreign
/// tenant's project. The column stays for indexing and the sweep; it is no
/// longer the tenant boundary.
pub async fn project_org_batch(
    conn: &mut AsyncPgConnection,
    project_ids: &[Uuid],
) -> QueryResult<Vec<(Uuid, Uuid)>> {
    if project_ids.is_empty() {
        return Ok(Vec::new());
    }
    projects::table
        .filter(projects::id.eq_any(project_ids.to_vec()))
        .select((projects::id, projects::org_id))
        .load(conn)
        .await
}

/// `(user_id, scope_type, scope_id, permissions)` for many users in ONE org.
///
/// The batched form of `user_grants_in_org`. Filtered to a single organization
/// for the reason `reach_for`'s doc comment records: its org arm does not
/// compare the grant's org id, so an unfiltered list would leak another org's
/// visibility.
pub async fn grants_for_users_in_org(
    conn: &mut AsyncPgConnection,
    user_ids: &[Uuid],
    org_id: Uuid,
) -> QueryResult<Vec<(Uuid, String, Uuid, Value)>> {
    if user_ids.is_empty() {
        return Ok(Vec::new());
    }
    role_grants::table
        .inner_join(roles::table.on(roles::id.eq(role_grants::role_id)))
        .filter(role_grants::user_id.eq_any(user_ids.to_vec()))
        .filter(role_grants::org_id.eq(org_id))
        .select((
            role_grants::user_id,
            role_grants::scope_type,
            role_grants::scope_id,
            roles::permissions,
        ))
        .load(conn)
        .await
}

/// How many distinct MESSAGES this user received in the trailing hour.
///
/// `COUNT(DISTINCT message_id)`, not a row count: one legitimate grouped email
/// carrying 25 issue rows would otherwise report 25 against a cap of 20 and
/// degrade the user to digests on their first normal delivery.
pub async fn sent_messages_last_hour(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
) -> QueryResult<i64> {
    let row: AlertCountRow = diesel::sql_query(
        "SELECT count(DISTINCT message_id) AS n FROM notification_queue \
          WHERE user_id = $1 AND status = 'sent' AND sent_at > now() - interval '1 hour'",
    )
    .bind::<SqlUuid, _>(user_id)
    .get_result(conn)
    .await?;
    Ok(row.n)
}

/// A user's own notification history, newest first.
///
/// Ownership alone is NOT a sufficient gate — the caller must still run the
/// coverage predicate against freshly loaded grants and drop non-covered rows,
/// because a row written with a title and body at enqueue time would otherwise
/// let a member whose grant was revoked read exactly the issue titles and counts
/// the drain refused to mail them.
pub async fn notification_history_for_user(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    limit: i64,
) -> QueryResult<Vec<NotificationQueueItem>> {
    notification_queue::table
        .filter(notification_queue::user_id.eq(user_id))
        .order(notification_queue::created_at.desc())
        .limit(limit.clamp(1, 200))
        .select(NotificationQueueItem::as_select())
        .load(conn)
        .await
}

/// Projects by id, unfiltered by org — a best-effort display lookup for
/// polymorphic `scope_id`s the caller has already authorized.
pub async fn list_projects_by_ids(
    conn: &mut AsyncPgConnection,
    ids: &[Uuid],
) -> QueryResult<Vec<Project>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    projects::table
        .filter(projects::id.eq_any(ids.to_vec()))
        .select(Project::as_select())
        .load(conn)
        .await
}

/// Apps by id — the display counterpart to [`list_projects_by_ids`].
pub async fn apps_by_ids(conn: &mut AsyncPgConnection, ids: &[Uuid]) -> QueryResult<Vec<App>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    apps::table
        .filter(apps::id.eq_any(ids.to_vec()))
        .select(App::as_select())
        .load(conn)
        .await
}

/// Every enabled subscription, for the daily revocation sweep.
///
/// The daily pass is the backstop for the paths nobody remembered — a role's
/// permission list edited, a project deleted. The synchronous sweeps in
/// `routes/orgs.rs` cover the three deliberate grant-mutation sites and close
/// the 24-hour window for them.
pub async fn enabled_subscriptions_all(
    conn: &mut AsyncPgConnection,
) -> QueryResult<Vec<NotificationSubscription>> {
    notification_subscriptions::table
        .filter(notification_subscriptions::enabled.eq(true))
        .select(NotificationSubscription::as_select())
        .load(conn)
        .await
}

// ===========================================================================
// Combined active users (project-scoped, multi-app)
// ===========================================================================

/// One resolved `(app, environment filter)` pair.
///
/// Deliberately NOT `ReadScope`. `ReadScope` is singular by contract and ~36
/// read functions take it, so adding a plural variant of it would let a caller
/// hand a multi-app scope to a single-app query and get a silently wrong
/// number back.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AppEnvScope {
    pub app_id: Uuid,
    pub env: EnvFilter,
}

/// One UTC calendar day of the combined report. The three counts are exact:
/// `active_total == active_identified + active_guest` always.
#[derive(Debug, QueryableByName, serde::Serialize)]
pub struct ActiveUserDay {
    #[diesel(sql_type = diesel::sql_types::Date)]
    pub day: chrono::NaiveDate,
    #[diesel(sql_type = BigInt)]
    pub active_total: i64,
    #[diesel(sql_type = BigInt)]
    pub active_identified: i64,
    #[diesel(sql_type = BigInt)]
    pub active_guest: i64,
}

/// Distinct active identities per UTC day over `[from, to)`, combined across
/// `scopes` and split into identified / guest.
///
/// # The identity key
///
/// `'u:'‖distinct_id` when some selected app has `identified_at IS NOT NULL`
/// for that id, `'a:'‖app_id‖':'‖distinct_id` otherwise. Joining on
/// `distinct_id` alone was rejected: the count for {A,B} would then change
/// depending on whether C was also selected, and a metric that is not stable
/// under widening the selection is unexplainable.
///
/// Cross-app merging is EXACT STRING EQUALITY on `distinct_id`. If app A calls
/// someone `u-42` and app B calls them `auth0|abc`, this counts two people
/// where there is one. There is no server-side fix short of an
/// identity-resolution table; the guest column is what makes the limitation
/// legible instead of hidden.
///
/// # Why `days` exists
///
/// An earlier draft joined `event_users` directly against `signal`. Because the
/// projected key depends on `eu`, Postgres cannot push the `DISTINCT` below the
/// join — the outer side is every matching raw event row across up to 20
/// selections and up to 92 days, with no LIMIT, and the text key
/// `'u:'||distinct_id` is materialized once per event row before the dedup
/// sort. Interposing `days` collapses the join input by the average
/// events-per-user-per-day factor (typically 10-1000x) with a HashAggregate
/// over three narrow columns, and makes the `event_users` join cost
/// proportional to the ANSWER rather than to the input. `event_users` is the
/// table dominated by anonymous-id churn and it has no reaper, so this matters;
/// and the tier clamp does not save the naive shape on a deployment that never
/// enabled `sauron-tier`, which is exactly the deployment with the most rows.
///
/// # Why the split cannot fail to add up
///
/// `identified` is a property of the KEY, not of the row. A `'u:'` key exists
/// only because some selected app has `identified_at IS NOT NULL` for that
/// `distinct_id`; an `'a:'` key exists only where no selected app does. The
/// prefix therefore determines the flag, so carrying `identified` inside the
/// `DISTINCT` cannot split one key across both buckets and cannot change the
/// cardinality `active_total` counts. Two `count(*) FILTER` clauses over one
/// already-deduplicated set is the only shape with that property — computing
/// the halves as separate subqueries and adding them would reintroduce a total
/// that does not match its parts.
///
/// # Binds
///
/// `$1` from, `$2` to, then per scope in order `app_id` and — ONLY when
/// `env.consumes_bind()` — the environment bind. Deriving that index from
/// anything else is the documented easiest way to get `EnvFilter` wrong, and
/// here it silently pairs an environment with the wrong app.
pub async fn active_users_combined(
    conn: &mut AsyncPgConnection,
    scopes: &[AppEnvScope],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> QueryResult<Vec<ActiveUserDay>> {
    if scopes.is_empty() {
        return Ok(Vec::new());
    }

    let mut legs: Vec<String> = Vec::with_capacity(scopes.len() * 2);
    let mut next = 3usize;
    for s in scopes {
        let app_bind = next;
        next += 1;
        let env_bind = next;
        let env_a = s.env.sql_fragment_for("analytics_events", env_bind);
        let env_e = s.env.sql_fragment_for("error_events", env_bind);
        if s.env.consumes_bind() {
            next += 1;
        }
        legs.push(format!(
            "SELECT app_id, occurred_at, distinct_id FROM analytics_events \
             WHERE app_id = ${app_bind} AND occurred_at >= $1 AND occurred_at < $2{env_a} \
               AND distinct_id IS NOT NULL AND distinct_id <> ''"
        ));
        legs.push(format!(
            "SELECT app_id, occurred_at, distinct_id FROM error_events \
             WHERE app_id = ${app_bind} AND occurred_at >= $1 AND occurred_at < $2{env_e} \
               AND distinct_id IS NOT NULL AND distinct_id <> ''"
        ));
    }
    let signal = legs.join(" UNION ALL ");

    // `::timestamp` on both generate_series bounds is a disambiguation, not
    // decoration: `generate_series(date, date, interval)` has no exact
    // overload, and letting Postgres pick between the timestamp and timestamptz
    // forms would make the grid's boundaries depend on the session TimeZone —
    // the very dependency `AT TIME ZONE 'UTC'` exists to remove.
    let q = format!(
        "WITH signal AS ({signal}), \
         days AS ( \
           SELECT DISTINCT app_id, distinct_id, (occurred_at AT TIME ZONE 'UTC')::date AS day \
             FROM signal \
         ), \
         keyed AS ( \
           SELECT DISTINCT \
                  CASE WHEN eu.distinct_id IS NOT NULL \
                       THEN 'u:' || d.distinct_id \
                       ELSE 'a:' || d.app_id::text || ':' || d.distinct_id END AS identity_key, \
                  (eu.distinct_id IS NOT NULL) AS identified, \
                  d.day \
             FROM days d \
             LEFT JOIN event_users eu \
               ON eu.app_id = d.app_id AND eu.distinct_id = d.distinct_id \
              AND eu.identified_at IS NOT NULL \
         ), \
         per_day AS ( \
           SELECT day, \
                  count(*)::bigint                               AS active_total, \
                  count(*) FILTER (WHERE identified)::bigint     AS active_identified, \
                  count(*) FILTER (WHERE NOT identified)::bigint AS active_guest \
             FROM keyed GROUP BY day \
         ), \
         grid AS ( \
           SELECT generate_series( \
                    ($1 AT TIME ZONE 'UTC')::date::timestamp, \
                    (($2 - interval '1 microsecond') AT TIME ZONE 'UTC')::date::timestamp, \
                    interval '1 day')::date AS day \
         ) \
         SELECT g.day AS day, \
                COALESCE(p.active_total, 0)::bigint      AS active_total, \
                COALESCE(p.active_identified, 0)::bigint AS active_identified, \
                COALESCE(p.active_guest, 0)::bigint      AS active_guest \
           FROM grid g \
           LEFT JOIN per_day p ON p.day = g.day \
          ORDER BY g.day"
    );

    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<Timestamptz, _>(from)
        .bind::<Timestamptz, _>(to);
    for s in scopes {
        stmt = stmt.bind::<SqlUuid, _>(s.app_id);
        stmt = crate::bind_env!(stmt, &s.env);
    }
    stmt.load(conn).await
}

// ===========================================================================
// PII inspector: policies + scheduling
// ===========================================================================

/// The next due instant for a policy row aliased `p`.
///
/// All timezone arithmetic is Postgres's, because `chrono-tz` is not a
/// workspace dependency: Rust cannot resolve `Europe/Paris` at all, and adding
/// it is a workspace edit plus ~1 MB of tz data in every binary. There is also
/// no cron parser anywhere in the repo and no cron crate in Cargo.lock, so the
/// cadence is a 7-bit weekday mask plus a local wall-clock TIME — trivially
/// testable in SQL with `(days >> dow) & 1`, and a 1:1 map to a row of
/// checkboxes.
///
/// Eight days of candidates always covers a once-a-week schedule. Candidates
/// are built as LOCAL timestamps and converted back with `AT TIME ZONE`, so
/// Postgres resolves DST: on spring-forward a 02:30 schedule resolves to a
/// valid instant, on fall-back to the first occurrence. Never zero runs,
/// never double runs.
///
/// The update target MUST be aliased (`UPDATE inspector_policies AS p`) —
/// this fragment references `p.*`, and the pattern it copies
/// (`claim_due_monitors`) aliases nothing. The inner sub-select gets its own
/// alias so the two scopes cannot collide.
pub const NEXT_RUN_SQL: &str = "(SELECT min(ts) FROM ( \
     SELECT ((date_trunc('day', now() AT TIME ZONE p.schedule_tz) \
              + (d || ' day')::interval + p.schedule_time) \
             AT TIME ZONE p.schedule_tz) AS ts \
     FROM generate_series(0, 8) d) c \
   WHERE ((p.schedule_days >> EXTRACT(DOW FROM (c.ts AT TIME ZONE p.schedule_tz))::int) & 1) = 1 \
     AND c.ts > now())";

/// Recompute `next_run_at`. Called after EVERY schedule-field write so the
/// materialized due time is never stale.
pub async fn reschedule_policy(
    conn: &mut AsyncPgConnection,
    id: Uuid,
) -> QueryResult<Option<DateTime<Utc>>> {
    let sql = format!(
        "UPDATE inspector_policies AS p SET next_run_at = CASE \
           WHEN p.enabled AND p.schedule_enabled AND p.schedule_days <> 0 THEN {NEXT_RUN_SQL} \
           ELSE NULL END \
         WHERE p.id = $1 RETURNING p.next_run_at"
    );
    #[derive(QueryableByName)]
    struct NextRow {
        #[diesel(sql_type = Nullable<Timestamptz>)]
        next_run_at: Option<DateTime<Utc>>,
    }
    let row: Option<NextRow> = diesel::sql_query(sql)
        .bind::<SqlUuid, _>(id)
        .get_result(conn)
        .await
        .optional()?;
    Ok(row.and_then(|r| r.next_run_at))
}

/// Claim due policies, advancing `next_run_at` in the same statement.
///
/// `FOR UPDATE SKIP LOCKED` is the only concurrency primitive this repository
/// uses — there are zero advisory locks, deliberately, because a lock held by
/// a process that took a SIGKILL has nobody to release it and there is no
/// shutdown handler anywhere. The claim ALWAYS advances `next_run_at`, so a
/// row can never get stuck permanently due; the worker then decides whether to
/// actually start a scan.
pub async fn claim_due_policies(
    conn: &mut AsyncPgConnection,
    batch: i64,
) -> QueryResult<Vec<InspectorPolicy>> {
    let sql = format!(
        "UPDATE inspector_policies AS p \
         SET next_run_at = {NEXT_RUN_SQL}, last_run_at = now() \
         WHERE p.id IN ( \
           SELECT q.id FROM inspector_policies q \
           WHERE q.enabled AND q.schedule_enabled AND q.schedule_days <> 0 \
             AND q.next_run_at IS NOT NULL AND q.next_run_at <= now() \
           ORDER BY q.next_run_at FOR UPDATE SKIP LOCKED LIMIT $1 \
         ) RETURNING p.*"
    );
    diesel::sql_query(sql)
        .bind::<BigInt, _>(batch)
        .get_results(conn)
        .await
}

/// Whether `(target_type, target_id)` actually lives in `org_id`.
///
/// `inspector_policies.target_id` has NO foreign key (it is polymorphic, like
/// `role_grants`), so without this any authenticated user can mint an org
/// where they hold `org:manage` (`POST /v1/orgs` requires only `AuthUser`),
/// POST a policy naming a victim's `app_id`, and have the worker scan the
/// victim's `error_events` into rows carrying the attacker's `org_id` — which
/// is exactly what every list query filters on.
///
/// Called on every policy create and PATCH, AND again in the worker when the
/// scan is claimed, because grants outlive targets.
pub async fn validate_scope_in_org(
    conn: &mut AsyncPgConnection,
    org_id: Uuid,
    target_type: &str,
    target_id: Uuid,
) -> QueryResult<bool> {
    // An unknown target_type is a hard false, never a permissive default.
    let sql = match target_type {
        "project" => "SELECT EXISTS (SELECT 1 FROM projects WHERE id = $1 AND org_id = $2) AS ok",
        "app" => {
            "SELECT EXISTS (SELECT 1 FROM apps a JOIN projects p ON p.id = a.project_id \
             WHERE a.id = $1 AND p.org_id = $2) AS ok"
        }
        // For app_env the id is an app_environments ENROLLMENT id.
        "app_env" => {
            "SELECT EXISTS (SELECT 1 FROM app_environments ae \
             JOIN apps a ON a.id = ae.app_id JOIN projects p ON p.id = a.project_id \
             WHERE ae.id = $1 AND p.org_id = $2) AS ok"
        }
        _ => return Ok(false),
    };
    #[derive(QueryableByName)]
    struct OkRow {
        #[diesel(sql_type = Bool)]
        ok: bool,
    }
    let row: OkRow = diesel::sql_query(sql)
        .bind::<SqlUuid, _>(target_id)
        .bind::<SqlUuid, _>(org_id)
        .get_result(conn)
        .await?;
    Ok(row.ok)
}

/// Whether Postgres recognises this IANA timezone name.
///
/// The name is bound, never interpolated, and a failure is a plain `false`
/// rather than an error: `SET`-style timezone errors abort the surrounding
/// statement, and this runs inside a request handler that must answer 400.
pub async fn timezone_is_valid(conn: &mut AsyncPgConnection, tz: &str) -> bool {
    #[derive(QueryableByName)]
    struct TsRow {
        #[diesel(sql_type = diesel::sql_types::Timestamp)]
        #[allow(dead_code)]
        t: chrono::NaiveDateTime,
    }
    diesel::sql_query("SELECT now() AT TIME ZONE $1 AS t")
        .bind::<Text, _>(tz)
        .get_result::<TsRow>(conn)
        .await
        .is_ok()
}

pub async fn create_inspector_policy(
    conn: &mut AsyncPgConnection,
    new: NewInspectorPolicy<'_>,
) -> QueryResult<InspectorPolicy> {
    diesel::insert_into(inspector_policies::table)
        .values(&new)
        .returning(InspectorPolicy::as_returning())
        .get_result(conn)
        .await
}

pub async fn get_inspector_policy(
    conn: &mut AsyncPgConnection,
    id: Uuid,
) -> QueryResult<Option<InspectorPolicy>> {
    inspector_policies::table
        .find(id)
        .select(InspectorPolicy::as_select())
        .first(conn)
        .await
        .optional()
}

pub async fn list_inspector_policies_for_org(
    conn: &mut AsyncPgConnection,
    org_id: Uuid,
) -> QueryResult<Vec<InspectorPolicy>> {
    inspector_policies::table
        .filter(inspector_policies::org_id.eq(org_id))
        .select(InspectorPolicy::as_select())
        .order(inspector_policies::created_at.desc())
        .load(conn)
        .await
}

pub async fn patch_inspector_policy(
    conn: &mut AsyncPgConnection,
    id: Uuid,
    patch: InspectorPolicyPatch<'_>,
) -> QueryResult<Option<InspectorPolicy>> {
    diesel::update(inspector_policies::table.find(id))
        .set(patch)
        .returning(InspectorPolicy::as_returning())
        .get_result(conn)
        .await
        .optional()
}

pub async fn delete_inspector_policy(conn: &mut AsyncPgConnection, id: Uuid) -> QueryResult<usize> {
    diesel::delete(inspector_policies::table.find(id))
        .execute(conn)
        .await
}

/// The policy that governs `app_id`: most specific wins, whole row.
///
/// `app_env` beats `app` beats `project`, and `UNIQUE (target_type, target_id)`
/// means there is exactly one candidate per level, so the ranking is a
/// database fact rather than an ordering problem. An `app_env` row is only
/// preferred when the app has exactly one live enrollment; with several, the
/// app-level answer is the honest one for an app-scoped question.
pub async fn effective_policy_for_app(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
) -> QueryResult<Option<InspectorPolicy>> {
    diesel::sql_query(
        "SELECT p.* FROM inspector_policies p \
         WHERE (p.target_type = 'app' AND p.target_id = $1) \
            OR (p.target_type = 'project' \
                AND p.target_id = (SELECT project_id FROM apps WHERE id = $1)) \
            OR (p.target_type = 'app_env' \
                AND p.target_id IN (SELECT id FROM app_environments WHERE app_id = $1)) \
         ORDER BY CASE p.target_type \
                    WHEN 'app_env' THEN 0 WHEN 'app' THEN 1 ELSE 2 END, p.created_at \
         LIMIT 1",
    )
    .bind::<SqlUuid, _>(app_id)
    .get_result(conn)
    .await
    .optional()
}

/// Every policy row whose node falls strictly UNDER `(target_type, target_id)`,
/// enabled or not.
///
/// Enabled-or-not is the point: "most specific wins, whole row" applies to
/// EXCLUSION as well as configuration. A disabled child policy is how an admin
/// excludes one noisy environment, and a parent that keeps walking it would
/// persist that environment's key paths for 90 days while the UI showed it as
/// excluded.
pub async fn list_inspector_policies_under(
    conn: &mut AsyncPgConnection,
    target_type: &str,
    target_id: Uuid,
) -> QueryResult<Vec<(String, Uuid)>> {
    #[derive(QueryableByName)]
    struct NodeRow {
        #[diesel(sql_type = Text)]
        target_type: String,
        #[diesel(sql_type = SqlUuid)]
        target_id: Uuid,
    }
    let sql = match target_type {
        "project" => {
            "SELECT p.target_type, p.target_id FROM inspector_policies p \
             WHERE (p.target_type = 'app' \
                    AND p.target_id IN (SELECT id FROM apps WHERE project_id = $1)) \
                OR (p.target_type = 'app_env' \
                    AND p.target_id IN (SELECT ae.id FROM app_environments ae \
                                        JOIN apps a ON a.id = ae.app_id \
                                        WHERE a.project_id = $1))"
        }
        "app" => {
            "SELECT p.target_type, p.target_id FROM inspector_policies p \
             WHERE p.target_type = 'app_env' \
               AND p.target_id IN (SELECT id FROM app_environments WHERE app_id = $1)"
        }
        // Nothing is narrower than an app_env node.
        _ => return Ok(Vec::new()),
    };
    let rows: Vec<NodeRow> = diesel::sql_query(sql)
        .bind::<SqlUuid, _>(target_id)
        .load(conn)
        .await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.target_type, r.target_id))
        .collect())
}

/// Expand a policy node into ordered `(app_id, app_env_id|NULL)` pairs.
///
/// The NULL pair is the unattributed bucket and is only emitted for app- and
/// project-scoped nodes: `EnvFilter::Subset` uses `= ANY`, which never matches
/// NULL, so those rows are unreachable from an env-scoped policy. If a
/// deployment runs mostly `app_env` policies those rows go silently unscanned,
/// which is what the effective-policy endpoint surfaces.
pub async fn scan_pairs_for_node(
    conn: &mut AsyncPgConnection,
    target_type: &str,
    target_id: Uuid,
) -> QueryResult<Vec<(Uuid, Option<Uuid>)>> {
    #[derive(QueryableByName)]
    struct PairRow {
        #[diesel(sql_type = SqlUuid)]
        app_id: Uuid,
        #[diesel(sql_type = Nullable<SqlUuid>)]
        env_id: Option<Uuid>,
    }
    let sql = match target_type {
        "project" => {
            "SELECT a.id AS app_id, ae.id AS env_id FROM apps a \
             LEFT JOIN app_environments ae ON ae.app_id = a.id AND ae.retired_at IS NULL \
             WHERE a.project_id = $1 ORDER BY a.id, ae.id"
        }
        "app" => {
            "SELECT a.id AS app_id, ae.id AS env_id FROM apps a \
             LEFT JOIN app_environments ae ON ae.app_id = a.id AND ae.retired_at IS NULL \
             WHERE a.id = $1 ORDER BY ae.id"
        }
        "app_env" => {
            "SELECT ae.app_id AS app_id, ae.id AS env_id FROM app_environments ae \
             WHERE ae.id = $1"
        }
        _ => return Ok(Vec::new()),
    };
    let rows: Vec<PairRow> = diesel::sql_query(sql)
        .bind::<SqlUuid, _>(target_id)
        .load(conn)
        .await?;
    let mut out: Vec<(Uuid, Option<Uuid>)> =
        rows.into_iter().map(|r| (r.app_id, r.env_id)).collect();
    // The unattributed bucket, once per app, for app/project nodes only.
    if target_type != "app_env" {
        let apps: Vec<Uuid> = {
            let mut a: Vec<Uuid> = out.iter().map(|(app, _)| *app).collect();
            a.sort_unstable();
            a.dedup();
            a
        };
        for app in apps {
            out.push((app, None));
        }
    }
    Ok(out)
}

// ===========================================================================
// PII inspector: scans
// ===========================================================================

/// Insert a scan. A `UniqueViolation` here is the partial unique index
/// `inspector_scans_active_key` refusing a second queued/running scan for the
/// policy — the handler answers 409 with the active scan id, never 500.
pub async fn insert_inspector_scan(
    conn: &mut AsyncPgConnection,
    new: NewInspectorScan<'_>,
) -> QueryResult<InspectorScan> {
    diesel::insert_into(inspector_scans::table)
        .values(&new)
        .returning(InspectorScan::as_returning())
        .get_result(conn)
        .await
}

pub async fn active_scan_for_policy(
    conn: &mut AsyncPgConnection,
    policy_id: Uuid,
) -> QueryResult<Option<InspectorScan>> {
    inspector_scans::table
        .filter(inspector_scans::policy_id.eq(policy_id))
        .filter(inspector_scans::status.eq_any(vec!["queued", "running"]))
        .select(InspectorScan::as_select())
        .first(conn)
        .await
        .optional()
}

/// Claim one scan, copying `claim_due_monitors` verbatim in shape.
///
/// This is what makes N replicas safe, unlike `sauron-alerts` (no claim) and
/// `sauron-tier` (a watermark row with no locking). Re-claiming a `running`
/// row whose heartbeat expired IS the crash-resume mechanism; the caller
/// finalizes the scan as `failed` once `attempts > inspector_max_attempts` so
/// one poison unit cannot loop forever.
///
/// THE OWNER MUST BE ABLE TO RE-CLAIM ITS OWN RUNNING SCAN. The executor does
/// ONE unit per tick and re-enters, and every flush refreshes `heartbeat_at`
/// — so with only the two arms above, a scan the worker itself is mid-way
/// through is not claimable again until its own lease expires. That is one
/// unit per `INSPECTOR_LEASE_SECS`, and since each of those claims also
/// increments `attempts`, a scan dies at `INSPECTOR_MAX_ATTEMPTS` after four
/// units no matter how many it has. Observed directly: a 17-unit scan sat at
/// `units_done = 1` for a full 45-second drive. The third arm is what makes
/// "run one unit, flush, yield, re-enter" actually advance; `attempts` stays
/// honest because `flush_scan_unit` resets it whenever a unit completes, so
/// it counts claims SINCE THE LAST PROGRESS rather than claims in total.
pub async fn claim_one_scan(
    conn: &mut AsyncPgConnection,
    worker_id: &str,
    lease_secs: i64,
) -> QueryResult<Option<InspectorScan>> {
    diesel::sql_query(
        "UPDATE inspector_scans SET status='running', worker_id=$1, heartbeat_at=now(), \
                attempts=attempts+1, started_at=COALESCE(started_at, now()) \
         WHERE id IN (SELECT id FROM inspector_scans \
                      WHERE status='queued' \
                         OR (status='running' AND worker_id = $1) \
                         OR (status='running' AND heartbeat_at < now() - make_interval(secs => $2)) \
                      ORDER BY created_at FOR UPDATE SKIP LOCKED LIMIT 1) \
         RETURNING *",
    )
    .bind::<Text, _>(worker_id)
    .bind::<BigInt, _>(lease_secs)
    .get_result(conn)
    .await
    .optional()
}

#[allow(clippy::too_many_arguments)]
pub async fn finish_scan(
    conn: &mut AsyncPgConnection,
    scan_id: Uuid,
    worker_id: &str,
    status: &str,
    coverage: &str,
    coverage_note: &str,
    error: &str,
) -> QueryResult<usize> {
    diesel::sql_query(
        "UPDATE inspector_scans SET status=$3, coverage=$4, coverage_note=$5, error=$6, \
                finished_at=now(), heartbeat_at=now() \
         WHERE id=$1 AND worker_id=$2",
    )
    .bind::<SqlUuid, _>(scan_id)
    .bind::<Text, _>(worker_id)
    .bind::<Text, _>(status)
    .bind::<Text, _>(coverage)
    .bind::<Text, _>(coverage_note)
    .bind::<Text, _>(error)
    .execute(conn)
    .await
}

/// Ask a running scan to stop. The worker observes this on the `RETURNING` of
/// the next flush — a write it was making anyway.
pub async fn request_scan_cancel(
    conn: &mut AsyncPgConnection,
    scan_id: Uuid,
) -> QueryResult<usize> {
    diesel::sql_query(
        "UPDATE inspector_scans SET cancel_requested_at = COALESCE(cancel_requested_at, now()) \
         WHERE id = $1 AND status IN ('queued','running')",
    )
    .bind::<SqlUuid, _>(scan_id)
    .execute(conn)
    .await
}

pub async fn get_inspector_scan(
    conn: &mut AsyncPgConnection,
    id: Uuid,
) -> QueryResult<Option<InspectorScan>> {
    inspector_scans::table
        .find(id)
        .select(InspectorScan::as_select())
        .first(conn)
        .await
        .optional()
}

pub async fn list_scans_for_policy(
    conn: &mut AsyncPgConnection,
    policy_id: Uuid,
    limit: i64,
) -> QueryResult<Vec<InspectorScan>> {
    inspector_scans::table
        .filter(inspector_scans::policy_id.eq(policy_id))
        .select(InspectorScan::as_select())
        .order(inspector_scans::created_at.desc())
        .limit(limit.clamp(1, 200))
        .load(conn)
        .await
}

/// One unit's aggregated result, ready to be folded into `inspector_findings`.
#[derive(Debug, Clone)]
pub struct FindingDelta {
    pub org_id: Uuid,
    pub app_id: Uuid,
    pub environment_id: Option<Uuid>,
    pub env_scope: String,
    pub source_table: String,
    pub source_column: String,
    pub key_path: String,
    pub matched_key: String,
    pub detector: String,
    pub value_type: String,
    pub match_count: i64,
    pub match_count_exact: bool,
    pub sample_preview: String,
    pub sample_row_id: Option<Uuid>,
    pub sample_occurred_at: Option<DateTime<Utc>>,
    pub partition_kind: String,
    pub first_seen_at: Option<DateTime<Utc>>,
    pub last_seen_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy)]
pub struct FlushOutcome {
    /// How many rows the CTE actually INSERTED (as opposed to updated).
    pub new_findings: i64,
    /// Set once an operator has asked the scan to stop.
    pub cancel_requested_at: Option<DateTime<Utc>>,
}

/// Persist one unit's findings AND advance the cursor in ONE data-modifying
/// CTE. There is no `conn.transaction` in this repository (MSRV 1.82).
///
/// Four properties are load-bearing:
///
/// `attempts = 0`. `claim_one_scan` increments `attempts` on every claim, and
/// the executor re-claims its own running scan once per unit, so without this
/// reset a healthy multi-unit scan trips `INSPECTOR_MAX_ATTEMPTS` and is
/// finalized `failed` after four units. Resetting it HERE — on the one write
/// that only happens when a unit actually completed — is what makes the
/// counter mean "claims since the last progress", which is the thing
/// `attempts > inspector_max_attempts` is trying to detect. A unit that keeps
/// failing never reaches this statement, so the poison-unit guard still fires.
///
/// ATOMICITY. The deltas and the cursor advance in one commit, so a SIGKILL
/// between them is impossible and re-running the lost range re-adds exact
/// counts from the last durable cursor. Counts stay correct without
/// `GREATEST`-style deduplication — which would be correct across re-runs but
/// WRONG across units, which must sum.
///
/// THE `worker_id` FENCE. A worker stalled past the lease (GC, IO) can have
/// its scan reclaimed while still alive, and `match_count +
/// excluded.match_count` would then double-count. A flush that affects zero
/// rows returns `None` and the caller MUST abort the unit. Any refactor that
/// drops the fence silently corrupts counts.
///
/// `findings_count` READS THE CTE, NOT THE TABLE. Postgres executes all
/// sub-statements of a data-modifying `WITH` against one snapshot and
/// documents that they cannot see one another's effects, so
/// `(SELECT count(*) FROM inspector_findings WHERE scan_id = $1)` here counts
/// the table as of BEFORE `f` ran: the counter is permanently one flush
/// behind, the final flush's findings are never counted, and a single-unit
/// scan reports 0 while `GET /findings` returns rows. It is also an aggregate
/// over the whole finding set on every flush — hundreds of millions of index
/// tuples over a scan, on the connection that is supposed to be duty-cycled.
#[allow(clippy::too_many_arguments)]
pub async fn flush_scan_unit(
    conn: &mut AsyncPgConnection,
    scan_id: Uuid,
    worker_id: &str,
    cursor: &Value,
    units_done: i32,
    rows_delta: i64,
    deltas: &[FindingDelta],
) -> QueryResult<Option<FlushOutcome>> {
    // Columnar unnest: one bound array per column keeps the statement text
    // constant regardless of how many findings a unit produced, so Postgres
    // reuses the plan instead of parsing a fresh VALUES list every flush.
    let org_ids: Vec<Uuid> = deltas.iter().map(|d| d.org_id).collect();
    let app_ids: Vec<Uuid> = deltas.iter().map(|d| d.app_id).collect();
    let env_ids: Vec<Option<Uuid>> = deltas.iter().map(|d| d.environment_id).collect();
    let env_scopes: Vec<String> = deltas.iter().map(|d| d.env_scope.clone()).collect();
    let tables: Vec<String> = deltas.iter().map(|d| d.source_table.clone()).collect();
    let columns: Vec<String> = deltas.iter().map(|d| d.source_column.clone()).collect();
    let paths: Vec<String> = deltas.iter().map(|d| d.key_path.clone()).collect();
    let keys: Vec<String> = deltas.iter().map(|d| d.matched_key.clone()).collect();
    let dets: Vec<String> = deltas.iter().map(|d| d.detector.clone()).collect();
    let types: Vec<String> = deltas.iter().map(|d| d.value_type.clone()).collect();
    let counts: Vec<i64> = deltas.iter().map(|d| d.match_count).collect();
    let exacts: Vec<bool> = deltas.iter().map(|d| d.match_count_exact).collect();
    let previews: Vec<String> = deltas.iter().map(|d| d.sample_preview.clone()).collect();
    let row_ids: Vec<Option<Uuid>> = deltas.iter().map(|d| d.sample_row_id).collect();
    let occurred: Vec<Option<DateTime<Utc>>> =
        deltas.iter().map(|d| d.sample_occurred_at).collect();
    let kinds: Vec<String> = deltas.iter().map(|d| d.partition_kind.clone()).collect();
    let firsts: Vec<Option<DateTime<Utc>>> = deltas.iter().map(|d| d.first_seen_at).collect();
    let lasts: Vec<Option<DateTime<Utc>>> = deltas.iter().map(|d| d.last_seen_at).collect();

    #[derive(QueryableByName)]
    struct FlushRow {
        #[diesel(sql_type = BigInt)]
        inserted: i64,
        #[diesel(sql_type = Nullable<Timestamptz>)]
        cancel_requested_at: Option<DateTime<Utc>>,
    }

    let row: Option<FlushRow> = diesel::sql_query(
        "WITH me AS (SELECT id FROM inspector_scans WHERE id = $1 AND worker_id = $2), \
         f AS ( \
           INSERT INTO inspector_findings ( \
             scan_id, org_id, app_id, environment_id, env_scope, source_table, source_column, \
             key_path, matched_key, detector, value_type, match_count, match_count_exact, \
             sample_preview, sample_row_id, sample_occurred_at, partition_kind, \
             first_seen_at, last_seen_at) \
           SELECT $1, u.org_id, u.app_id, u.env_id, u.env_scope, u.src_table, u.src_column, \
                  u.key_path, u.matched_key, u.detector, u.value_type, u.match_count, u.exact, \
                  u.preview, u.row_id, u.occurred, u.kind, u.first_seen, u.last_seen \
           FROM unnest($6::uuid[], $7::uuid[], $8::uuid[], $9::text[], $10::text[], $11::text[], \
                       $12::text[], $13::text[], $14::text[], $15::text[], $16::bigint[], \
                       $17::bool[], $18::text[], $19::uuid[], $20::timestamptz[], $21::text[], \
                       $22::timestamptz[], $23::timestamptz[]) \
                AS u(org_id, app_id, env_id, env_scope, src_table, src_column, key_path, \
                     matched_key, detector, value_type, match_count, exact, preview, row_id, \
                     occurred, kind, first_seen, last_seen) \
           WHERE EXISTS (SELECT 1 FROM me) \
           ON CONFLICT (scan_id, app_id, env_scope, \
                        COALESCE(environment_id,'00000000-0000-0000-0000-000000000000'::uuid), \
                        source_table, source_column, key_path, detector) \
           DO UPDATE SET \
             match_count = inspector_findings.match_count + excluded.match_count, \
             last_seen_at = GREATEST(inspector_findings.last_seen_at, excluded.last_seen_at), \
             first_seen_at = LEAST(inspector_findings.first_seen_at, excluded.first_seen_at), \
             match_count_exact = inspector_findings.match_count_exact AND excluded.match_count_exact \
           RETURNING (xmax = 0) AS inserted \
         ) \
         UPDATE inspector_scans SET \
           cursor = $3, units_done = $4, \
           rows_scanned = rows_scanned + $5, \
           findings_count = findings_count + \
               (SELECT count(*) FROM f WHERE inserted)::int, \
           attempts = 0, \
           heartbeat_at = now() \
         WHERE id = $1 AND worker_id = $2 \
         RETURNING (SELECT count(*) FROM f WHERE inserted)::bigint AS inserted, \
                   cancel_requested_at",
    )
    .bind::<SqlUuid, _>(scan_id)
    .bind::<Text, _>(worker_id)
    .bind::<Jsonb, _>(cursor)
    .bind::<Integer, _>(units_done)
    .bind::<BigInt, _>(rows_delta)
    .bind::<Array<SqlUuid>, _>(org_ids)
    .bind::<Array<SqlUuid>, _>(app_ids)
    .bind::<Array<Nullable<SqlUuid>>, _>(env_ids)
    .bind::<Array<Text>, _>(env_scopes)
    .bind::<Array<Text>, _>(tables)
    .bind::<Array<Text>, _>(columns)
    .bind::<Array<Text>, _>(paths)
    .bind::<Array<Text>, _>(keys)
    .bind::<Array<Text>, _>(dets)
    .bind::<Array<Text>, _>(types)
    .bind::<Array<BigInt>, _>(counts)
    .bind::<Array<Bool>, _>(exacts)
    .bind::<Array<Text>, _>(previews)
    .bind::<Array<Nullable<SqlUuid>>, _>(row_ids)
    .bind::<Array<Nullable<Timestamptz>>, _>(occurred)
    .bind::<Array<Text>, _>(kinds)
    .bind::<Array<Nullable<Timestamptz>>, _>(firsts)
    .bind::<Array<Nullable<Timestamptz>>, _>(lasts)
    .get_result(conn)
    .await
    .optional()?;

    Ok(row.map(|r| FlushOutcome {
        new_findings: r.inserted,
        cancel_requested_at: r.cancel_requested_at,
    }))
}

// ===========================================================================
// PII inspector: findings + reveal
// ===========================================================================

/// Findings for a scan, biggest first, keyset-paginated on
/// `(match_count DESC, id)`. OFFSET is not offered: Postgres must walk and
/// discard every skipped row, so deep paging over a 33k-finding scan turns a
/// cheap request into a full ordered scan.
pub async fn list_findings_for_scan(
    conn: &mut AsyncPgConnection,
    scan_id: Uuid,
    limit: i64,
    after: Option<(i64, Uuid)>,
) -> QueryResult<Vec<InspectorFinding>> {
    let limit = limit.clamp(1, 1_000);
    match after {
        Some((count, id)) => {
            inspector_findings::table
                .filter(inspector_findings::scan_id.eq(scan_id))
                .filter(
                    inspector_findings::match_count
                        .lt(count)
                        .or(inspector_findings::match_count
                            .eq(count)
                            .and(inspector_findings::id.gt(id))),
                )
                .select(InspectorFinding::as_select())
                .order((
                    inspector_findings::match_count.desc(),
                    inspector_findings::id.asc(),
                ))
                .limit(limit)
                .load(conn)
                .await
        }
        None => {
            inspector_findings::table
                .filter(inspector_findings::scan_id.eq(scan_id))
                .select(InspectorFinding::as_select())
                .order((
                    inspector_findings::match_count.desc(),
                    inspector_findings::id.asc(),
                ))
                .limit(limit)
                .load(conn)
                .await
        }
    }
}

pub async fn count_findings_for_scan(
    conn: &mut AsyncPgConnection,
    scan_id: Uuid,
) -> QueryResult<i64> {
    inspector_findings::table
        .filter(inspector_findings::scan_id.eq(scan_id))
        .count()
        .get_result(conn)
        .await
}

pub async fn get_inspector_finding(
    conn: &mut AsyncPgConnection,
    id: Uuid,
) -> QueryResult<Option<InspectorFinding>> {
    inspector_findings::table
        .find(id)
        .select(InspectorFinding::as_select())
        .first(conn)
        .await
        .optional()
}

/// One live single-row read behind `POST /findings/{id}/reveal`.
///
/// `table` and `column` are `&'static str`s from `sauron_inspector::columns`,
/// never caller bytes — SQL identifiers cannot be bound, so the caller MUST
/// have resolved them through the inventory first.
///
/// The `app_id` predicate is not redundant: without it the tenant decision
/// rests entirely on `inspector_findings.app_id` being correct, a
/// worker-written value with no constraint tying it to the row
/// `sample_row_id` points at. Any attribution bug would convert silently into
/// cross-tenant raw-PII disclosure. `occurred_at` is mandatory for a
/// partitioned source so the query prunes to one child.
///
/// `None` is a 410 Gone: the partition was dropped by `sauron-tier`, the
/// rollup row was replaced, or the locator points at another tenant. Nothing
/// is persisted by this call.
pub async fn reveal_one_value(
    conn: &mut AsyncPgConnection,
    table: &'static str,
    column: &'static str,
    row_id: Uuid,
    occurred_at: Option<DateTime<Utc>>,
    app_id: Uuid,
) -> QueryResult<Option<Value>> {
    #[derive(QueryableByName)]
    struct ValRow {
        #[diesel(sql_type = Nullable<Jsonb>)]
        v: Option<Value>,
    }
    // `to_jsonb` normalizes the TEXT columns into the same shape as the jsonb
    // ones so the handler has one extraction path instead of two.
    let sql = match occurred_at {
        Some(_) => format!(
            "SELECT to_jsonb({column}) AS v FROM {table} \
             WHERE id = $1 AND occurred_at = $2 AND app_id = $3"
        ),
        None => {
            format!("SELECT to_jsonb({column}) AS v FROM {table} WHERE id = $1 AND app_id = $3")
        }
    };
    let q = diesel::sql_query(sql)
        .bind::<SqlUuid, _>(row_id)
        .bind::<Nullable<Timestamptz>, _>(occurred_at)
        .bind::<SqlUuid, _>(app_id);
    let row: Option<ValRow> = q.get_result(conn).await.optional()?;
    Ok(row.and_then(|r| r.v))
}

/// Record who revealed what, BEFORE the value is returned, so a failure to
/// audit is a failure to reveal.
pub async fn insert_reveal_audit(
    conn: &mut AsyncPgConnection,
    new: NewInspectorRevealAudit<'_>,
) -> QueryResult<usize> {
    diesel::insert_into(inspector_reveal_audit::table)
        .values(&new)
        .execute(conn)
        .await
}

/// The email to denormalize into an audit row. `SET NULL` on the FK loses the
/// identity, so the trail carries a snapshot.
pub async fn user_email(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
) -> QueryResult<Option<String>> {
    users::table
        .find(user_id)
        .select(users::email)
        .first(conn)
        .await
        .optional()
}

/// A real `(id, occurred_at)` locator for `app_id`, for tests and for the
/// storage report's sanity checks. Returns `None` on an app with no errors.
pub async fn first_error_event_locator(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
) -> QueryResult<Option<(Uuid, DateTime<Utc>)>> {
    #[derive(QueryableByName)]
    struct LocRow {
        #[diesel(sql_type = SqlUuid)]
        id: Uuid,
        #[diesel(sql_type = Timestamptz)]
        occurred_at: DateTime<Utc>,
    }
    let row: Option<LocRow> = diesel::sql_query(
        "SELECT id, occurred_at FROM error_events WHERE app_id = $1 ORDER BY occurred_at DESC LIMIT 1",
    )
    .bind::<SqlUuid, _>(app_id)
    .get_result(conn)
    .await
    .optional()?;
    Ok(row.map(|r| (r.id, r.occurred_at)))
}

// ===========================================================================
// PII inspector: mask actions (audit + queue + cursor + progress meter)
// ===========================================================================

pub async fn insert_mask_action(
    conn: &mut AsyncPgConnection,
    new: NewInspectorMaskAction<'_>,
) -> QueryResult<InspectorMaskAction> {
    diesel::insert_into(inspector_mask_actions::table)
        .values(&new)
        .returning(InspectorMaskAction::as_returning())
        .get_result(conn)
        .await
}

pub async fn get_mask_action(
    conn: &mut AsyncPgConnection,
    id: Uuid,
) -> QueryResult<Option<InspectorMaskAction>> {
    inspector_mask_actions::table
        .find(id)
        .select(InspectorMaskAction::as_select())
        .first(conn)
        .await
        .optional()
}

/// The ceiling is 100_000, matching `list_mask_actions_for_org`, and it is not
/// cosmetic. `list_app_mask_actions` hands this function
/// `INSPECTOR_EXPORT_MAX_ROWS` (50_000 by default) for a CSV export and then
/// refuses the export if the row count comes back AT that ceiling — a guard
/// that a clamp of 1_000 makes unreachable, so every app with more than a
/// thousand audit rows would download the newest 1_000 with a 200 and a
/// friendly filename. That is the silent prefix the export guard exists to
/// refuse, arriving by the other door; the JSON path is unaffected because the
/// handler clamps it to `MAX_LIMIT` (500) before it ever gets here.
pub async fn list_mask_actions_for_app(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    limit: i64,
) -> QueryResult<Vec<InspectorMaskAction>> {
    inspector_mask_actions::table
        .filter(inspector_mask_actions::app_id.eq(app_id))
        .select(InspectorMaskAction::as_select())
        .order(inspector_mask_actions::requested_at.desc())
        .limit(limit.clamp(1, 100_000))
        .load(conn)
        .await
}

pub async fn list_mask_actions_for_org(
    conn: &mut AsyncPgConnection,
    org_id: Uuid,
    limit: i64,
) -> QueryResult<Vec<InspectorMaskAction>> {
    inspector_mask_actions::table
        .filter(inspector_mask_actions::org_id.eq(org_id))
        .select(InspectorMaskAction::as_select())
        .order(inspector_mask_actions::requested_at.desc())
        .limit(limit.clamp(1, 100_000))
        .load(conn)
        .await
}

/// Claim one action for the given slot.
///
/// `kind` selects the SLOT, never the phase: previews and masks are two
/// independent claim slots, because a single FIFO lets a multi-hour mask
/// starve every preview past its 15-minute TTL and confirm — which requires
/// `previewed` — becomes permanently impossible on a busy app.
///
/// `LIMIT 1` is deliberate for masks: masking is heavy write and one action at
/// a time per worker IS the throttle; N workers take N different actions.
/// Re-claiming a stale row is the crash-resume mechanism.
pub async fn claim_mask_action(
    conn: &mut AsyncPgConnection,
    kind: &str,
    worker_id: &str,
    stale_secs: i64,
) -> QueryResult<Option<InspectorMaskAction>> {
    let sql = if kind == "preview" {
        "UPDATE inspector_mask_actions SET phase='counting', claimed_at=now(), worker_id=$1, \
                started_at=COALESCE(started_at, now()) \
         WHERE id IN (SELECT id FROM inspector_mask_actions \
                      WHERE kind='preview' AND status='preview' \
                        AND (claimed_at IS NULL OR claimed_at < now() - make_interval(secs => $2)) \
                      ORDER BY requested_at FOR UPDATE SKIP LOCKED LIMIT 1) \
         RETURNING *"
    } else {
        "UPDATE inspector_mask_actions SET status='running', claimed_at=now(), worker_id=$1, \
                started_at=COALESCE(started_at, now()) \
         WHERE id IN (SELECT id FROM inspector_mask_actions \
                      WHERE kind='mask' \
                        AND (status='pending' \
                             OR (status IN ('running','cancelling') \
                                 AND claimed_at < now() - make_interval(secs => $2))) \
                      ORDER BY requested_at FOR UPDATE SKIP LOCKED LIMIT 1) \
         RETURNING *"
    };
    diesel::sql_query(sql)
        .bind::<Text, _>(worker_id)
        .bind::<BigInt, _>(stale_secs)
        .get_result(conn)
        .await
        .optional()
}

/// A preview finished counting. `previewed_at` is stamped HERE, not at
/// request time, because the TTL must run from the moment the numbers became
/// readable.
pub async fn finish_preview(
    conn: &mut AsyncPgConnection,
    id: Uuid,
    worker_id: &str,
    estimated_rows: i64,
    cold_rows_skipped: i64,
    cold_boundary_at: Option<DateTime<Utc>>,
) -> QueryResult<usize> {
    diesel::sql_query(
        "UPDATE inspector_mask_actions \
         SET status='previewed', phase='finished', previewed_at=now(), finished_at=now(), \
             estimated_rows=$3, cold_rows_skipped=$4, cold_boundary_at=$5 \
         WHERE id=$1 AND worker_id=$2 AND kind='preview' AND status='preview'",
    )
    .bind::<SqlUuid, _>(id)
    .bind::<Text, _>(worker_id)
    .bind::<BigInt, _>(estimated_rows)
    .bind::<BigInt, _>(cold_rows_skipped)
    .bind::<Nullable<Timestamptz>, _>(cold_boundary_at)
    .execute(conn)
    .await
}

/// Promote `previewed` -> `pending` and hand the row to the mask slot.
///
/// Every gate is IN THE STATEMENT rather than in the handler, so a
/// double-clicked confirm, a concurrent second confirm and a stale preview all
/// resolve to "0 rows updated" instead of racing. `targets` is deliberately
/// not a parameter: it was frozen at preview, so a confirm can never widen
/// what was counted and shown.
///
/// `cold_rows_skipped=0` is not tidying. Preview and mask are the SAME row, and
/// `finish_preview` SETs that column while the mask executor ADDs to it once
/// per skipped day — so without the reset every confirmed action reports the
/// cold rows twice. MEASURED: a 2800-row cold window came back as 5600 on a
/// `done` action. It is the number the Audit column, the audit CSV and the
/// MaskDialog all present as "rows we could not reach", next to `rows_masked`;
/// an operator reads it and acts on it, so it has to have exactly one writer.
pub async fn confirm_mask_action(
    conn: &mut AsyncPgConnection,
    id: Uuid,
    confirm_source: &str,
    preview_ttl_secs: i64,
    max_rows: i64,
) -> QueryResult<usize> {
    diesel::sql_query(
        "UPDATE inspector_mask_actions \
         SET kind='mask', status='pending', phase='idle', confirmed_at=now(), \
             confirm_source=$2, finished_at=NULL, claimed_at=NULL, worker_id=NULL, \
             cold_rows_skipped=0 \
         WHERE id=$1 AND kind='preview' AND status='previewed' \
           AND previewed_at IS NOT NULL \
           AND previewed_at > now() - make_interval(secs => $3) \
           AND estimated_rows <= $4",
    )
    .bind::<SqlUuid, _>(id)
    .bind::<Text, _>(confirm_source)
    .bind::<BigInt, _>(preview_ttl_secs)
    .bind::<BigInt, _>(max_rows)
    .execute(conn)
    .await
}

/// Ask a queued or running mask to stop.
///
/// `running -> cancelling` is allowed; the batch loop observes it on the
/// `RETURNING status` of a write it was making anyway and lands in terminal
/// `cancelled` with the cursor and counters already durable. A terminal action
/// updates zero rows and the handler answers 409.
pub async fn cancel_mask_action(
    conn: &mut AsyncPgConnection,
    id: Uuid,
    cancelled_by: Option<Uuid>,
    cancelled_by_email: &str,
) -> QueryResult<usize> {
    diesel::sql_query(
        "UPDATE inspector_mask_actions \
         SET status = CASE WHEN status = 'running' THEN 'cancelling' ELSE 'cancelled' END, \
             cancelled_by=$2, cancelled_by_email=$3, cancelled_at=now(), \
             finished_at = CASE WHEN status = 'running' THEN finished_at ELSE now() END \
         WHERE id=$1 AND status IN ('preview','previewed','pending','running')",
    )
    .bind::<SqlUuid, _>(id)
    .bind::<Nullable<SqlUuid>, _>(cancelled_by)
    .bind::<Text, _>(cancelled_by_email)
    .execute(conn)
    .await
}

/// Deactivating a member must also stop the destruction they queued.
///
/// Confirm re-authorizes, but the action then sits in `pending` — with one
/// slot per worker and a 200 ms inter-batch pause, a backlog can be hours
/// deep. A member whose account was deactivated (which revokes refresh tokens
/// and touches nothing queued) must not have their queued destruction execute.
pub async fn cancel_pending_mask_actions_for_user(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
) -> QueryResult<usize> {
    diesel::sql_query(
        "UPDATE inspector_mask_actions \
         SET status='cancelled', cancelled_at=now(), finished_at=now(), \
             error='requester was deactivated before the action ran' \
         WHERE requested_by=$1 AND status IN ('preview','previewed','pending')",
    )
    .bind::<SqlUuid, _>(user_id)
    .execute(conn)
    .await
}

pub async fn set_mask_phase(
    conn: &mut AsyncPgConnection,
    id: Uuid,
    worker_id: &str,
    phase: &str,
    day_cursor: Option<chrono::NaiveDate>,
) -> QueryResult<usize> {
    diesel::sql_query(
        "UPDATE inspector_mask_actions \
         SET phase=$3, day_cursor=$4, cursor_occurred_at=NULL, cursor_id=NULL \
         WHERE id=$1 AND worker_id=$2",
    )
    .bind::<SqlUuid, _>(id)
    .bind::<Text, _>(worker_id)
    .bind::<Text, _>(phase)
    .bind::<Nullable<Date>, _>(day_cursor)
    .execute(conn)
    .await
}

pub async fn fail_mask_action(
    conn: &mut AsyncPgConnection,
    id: Uuid,
    reason: &str,
) -> QueryResult<usize> {
    diesel::sql_query(
        "UPDATE inspector_mask_actions SET status='failed', phase='finished', \
                finished_at=now(), error=$2 \
         WHERE id=$1 AND status NOT IN ('done','failed','cancelled')",
    )
    .bind::<SqlUuid, _>(id)
    .bind::<Text, _>(reason)
    .execute(conn)
    .await
}

/// `cold_boundary_at` is re-recorded HERE, not only at preview, so the audit
/// shows what execution actually skipped rather than what the preview
/// predicted — `sauron-tier` defers the drop to a later cycle than the export,
/// so the boundary genuinely moves during a multi-hour pass.
pub async fn finish_mask_action(
    conn: &mut AsyncPgConnection,
    id: Uuid,
    worker_id: &str,
    status: &str,
    vacuum_advised: bool,
    cold_boundary_at: Option<DateTime<Utc>>,
) -> QueryResult<usize> {
    diesel::sql_query(
        "UPDATE inspector_mask_actions SET status=$3, phase='finished', finished_at=now(), \
                vacuum_advised=$4, cold_boundary_at=COALESCE($5, cold_boundary_at) \
         WHERE id=$1 AND (worker_id=$2 OR worker_id IS NULL)",
    )
    .bind::<SqlUuid, _>(id)
    .bind::<Text, _>(worker_id)
    .bind::<Text, _>(status)
    .bind::<Bool, _>(vacuum_advised)
    .bind::<Nullable<Timestamptz>, _>(cold_boundary_at)
    .execute(conn)
    .await
}

/// Register mask targets for forward enforcement.
///
/// `ON CONFLICT DO NOTHING` against `inspector_masked_keys_key` is what makes
/// re-masking the same finding idempotent — an operator who runs the same mask
/// twice must not end up with two rows the enforcer walks twice per event.
pub async fn insert_masked_keys(
    conn: &mut AsyncPgConnection,
    rows: &[NewInspectorMaskedKey<'_>],
) -> QueryResult<usize> {
    if rows.is_empty() {
        return Ok(0);
    }
    diesel::insert_into(inspector_masked_keys::table)
        .values(rows)
        .on_conflict((
            inspector_masked_keys::app_id,
            inspector_masked_keys::target_table,
            inspector_masked_keys::target_column,
            inspector_masked_keys::json_path,
        ))
        .do_nothing()
        .execute(conn)
        .await
}

/// The enforcer's cache-miss load. One indexed read per app per 30 seconds.
pub async fn masked_keys_for_app(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
) -> QueryResult<Vec<InspectorMaskedKey>> {
    inspector_masked_keys::table
        .filter(inspector_masked_keys::app_id.eq(app_id))
        .select(InspectorMaskedKey::as_select())
        .order(inspector_masked_keys::created_at.asc())
        .load(conn)
        .await
}

/// Same rows, for the read-only "Forward enforcement" card on the Policy tab.
pub async fn list_masked_keys(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
) -> QueryResult<Vec<InspectorMaskedKey>> {
    masked_keys_for_app(conn, app_id).await
}

// ===========================================================================
// PII inspector: mask + count batches
// ===========================================================================

use sauron_inspector::targets::{TargetColumn, TargetTable};

/// Keyset position within one day's partition. The zero value starts a day.
#[derive(Debug, Clone, Copy, Default)]
pub struct BatchCursor {
    pub occurred_at: Option<DateTime<Utc>>,
    pub id: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct BatchOutcome {
    pub rows_scanned: i64,
    pub rows_masked: i64,
    /// `None` when the batch came back short — the day is finished.
    pub next_cursor: Option<BatchCursor>,
    /// Observed on a write the worker was making anyway. `cancelling` is how
    /// an operator stops a multi-hour grind at 3am without hand-written SQL.
    pub status: String,
}

#[derive(QueryableByName)]
struct BatchRow {
    #[diesel(sql_type = BigInt)]
    scanned: i64,
    #[diesel(sql_type = BigInt)]
    masked: i64,
    #[diesel(sql_type = Nullable<Timestamptz>)]
    cur_occurred_at: Option<DateTime<Utc>>,
    #[diesel(sql_type = Nullable<SqlUuid>)]
    cur_id: Option<Uuid>,
    #[diesel(sql_type = Text)]
    status: String,
}

fn to_outcome(r: BatchRow, limit: i64) -> BatchOutcome {
    BatchOutcome {
        rows_scanned: r.scanned,
        rows_masked: r.masked,
        next_cursor: if r.scanned >= limit {
            Some(BatchCursor {
                occurred_at: r.cur_occurred_at,
                id: r.cur_id,
            })
        } else {
            None
        },
        status: r.status,
    }
}

/// One day-bounded, keyset-paginated mask batch over a jsonb path.
///
/// The day window appears TWICE on purpose. Joining `sel` on `(id,
/// occurred_at)` does NOT reproduce `update_event_symbolication`'s pruning:
/// that function compares `occurred_at` to a BOUND SCALAR PARAMETER, which is
/// eligible for runtime pruning; comparing it to a CTE column gives the
/// planner no pruning key and it plans one `Update` node per child.
///
/// `coalesce(col, '{}'::jsonb)` is required because `jsonb_set` returns NULL
/// if any argument is NULL, and a NULL written into a `NOT NULL DEFAULT '{}'`
/// column is the single most likely implementation bug in this slice.
/// `create_missing => false` leaves a row lacking the path untouched.
///
/// The cursor and both counters advance in the same commit as the data change,
/// so a SIGKILL loses at most one batch and can never double-count.
///
/// `{c} #> $6 <> '"****"'::jsonb` is what makes that last clause TRUE, and it
/// is the same guard `mask_batch_text` already carries as `{c} <> '****'`.
/// Resume is per DAY — the executor re-enters `day_cursor` from the top of the
/// day and nothing reads the persisted keyset cursor back — so without it a
/// crash, or a cancel followed by a re-queue, re-selects every row the previous
/// pass already masked (`'"****"'` IS NOT NULL, so the bare null check still
/// matches) and inflates the counters. MEASURED: a SIGKILL 850 rows into a
/// 3000-row pass finished `done` reporting `rows_masked = 3550` against 3000
/// actually-masked rows — an audit number an operator reads, overstating what
/// was destroyed by 18%.
#[allow(clippy::too_many_arguments)]
pub async fn mask_batch_jsonb(
    conn: &mut AsyncPgConnection,
    table: TargetTable,
    column: TargetColumn,
    app_id: Uuid,
    day: chrono::NaiveDate,
    path: &[String],
    cursor: BatchCursor,
    limit: i64,
    action_id: Uuid,
    worker_id: &str,
) -> QueryResult<Option<BatchOutcome>> {
    // Identifiers are &'static str from the TargetTable/TargetColumn enums,
    // never caller bytes: SQL identifiers cannot be bound, and the worker
    // reads `targets` back out of Postgres in a different process from the one
    // that validated it.
    let (t, c) = (table.as_sql(), column.as_sql());
    let sql = format!(
        "WITH sel AS ( \
           SELECT id, occurred_at FROM {t} \
           WHERE app_id=$1 AND occurred_at >= $2 AND occurred_at < $3 \
             AND ($4::timestamptz IS NULL OR (occurred_at, id) > ($4, $5)) \
             AND {c} #> $6 IS NOT NULL AND {c} #> $6 <> '\"****\"'::jsonb \
           ORDER BY occurred_at, id LIMIT $7), \
         upd AS ( \
           UPDATE {t} e \
           SET {c} = jsonb_set(coalesce(e.{c}, '{{}}'::jsonb), $6, '\"****\"'::jsonb, false) \
           FROM sel WHERE e.id = sel.id AND e.occurred_at = sel.occurred_at \
             AND e.occurred_at >= $2 AND e.occurred_at < $3 \
           RETURNING 1 AS one) \
         UPDATE inspector_mask_actions SET \
           cursor_occurred_at = (SELECT max(occurred_at) FROM sel), \
           cursor_id = (SELECT id FROM sel ORDER BY occurred_at DESC, id DESC LIMIT 1), \
           rows_masked = rows_masked + (SELECT count(*) FROM upd), \
           rows_scanned = rows_scanned + (SELECT count(*) FROM sel), \
           claimed_at = now() \
         WHERE id = $8 AND worker_id = $9 \
         RETURNING (SELECT count(*) FROM sel)::bigint AS scanned, \
                   (SELECT count(*) FROM upd)::bigint AS masked, \
                   cursor_occurred_at AS cur_occurred_at, cursor_id AS cur_id, status"
    );
    let lo = day.and_hms_opt(0, 0, 0).unwrap().and_utc();
    let hi = lo + chrono::Duration::days(1);
    let row: Option<BatchRow> = diesel::sql_query(sql)
        .bind::<SqlUuid, _>(app_id)
        .bind::<Timestamptz, _>(lo)
        .bind::<Timestamptz, _>(hi)
        .bind::<Nullable<Timestamptz>, _>(cursor.occurred_at)
        .bind::<Nullable<SqlUuid>, _>(cursor.id)
        .bind::<Array<Text>, _>(path.to_vec())
        .bind::<BigInt, _>(limit)
        .bind::<SqlUuid, _>(action_id)
        .bind::<Text, _>(worker_id)
        .get_result(conn)
        .await
        .optional()?;
    Ok(row.map(|r| to_outcome(r, limit)))
}

/// `EXPLAIN` for the same statement, so the pruning regression is a test and
/// not a code review.
#[allow(clippy::too_many_arguments)]
pub async fn explain_mask_batch_jsonb(
    conn: &mut AsyncPgConnection,
    table: TargetTable,
    column: TargetColumn,
    app_id: Uuid,
    day: chrono::NaiveDate,
    path: &[String],
    cursor: BatchCursor,
    limit: i64,
) -> QueryResult<String> {
    let (t, c) = (table.as_sql(), column.as_sql());
    let sql = format!(
        "EXPLAIN WITH sel AS ( \
           SELECT id, occurred_at FROM {t} \
           WHERE app_id=$1 AND occurred_at >= $2 AND occurred_at < $3 \
             AND ($4::timestamptz IS NULL OR (occurred_at, id) > ($4, $5)) \
             AND {c} #> $6 IS NOT NULL AND {c} #> $6 <> '\"****\"'::jsonb \
           ORDER BY occurred_at, id LIMIT $7) \
         UPDATE {t} e \
         SET {c} = jsonb_set(coalesce(e.{c}, '{{}}'::jsonb), $6, '\"****\"'::jsonb, false) \
         FROM sel WHERE e.id = sel.id AND e.occurred_at = sel.occurred_at \
           AND e.occurred_at >= $2 AND e.occurred_at < $3"
    );
    #[derive(QueryableByName)]
    struct PlanRow {
        #[diesel(sql_type = Text, column_name = "QUERY PLAN")]
        plan: String,
    }
    let lo = day.and_hms_opt(0, 0, 0).unwrap().and_utc();
    let hi = lo + chrono::Duration::days(1);
    let rows: Vec<PlanRow> = diesel::sql_query(sql)
        .bind::<SqlUuid, _>(app_id)
        .bind::<Timestamptz, _>(lo)
        .bind::<Timestamptz, _>(hi)
        .bind::<Nullable<Timestamptz>, _>(cursor.occurred_at)
        .bind::<Nullable<SqlUuid>, _>(cursor.id)
        .bind::<Array<Text>, _>(path.to_vec())
        .bind::<BigInt, _>(limit)
        .load(conn)
        .await?;
    Ok(rows
        .into_iter()
        .map(|r| r.plan)
        .collect::<Vec<_>>()
        .join("\n"))
}

/// The wildcard lowering: rebuild the array, per element.
///
/// `WITH ORDINALITY` + `ORDER BY ord` is required because `jsonb_agg` order is
/// NOT guaranteed, and `coalesce(..., '[]')` is required because `jsonb_agg`
/// over an empty array returns NULL. The rebuild re-serializes the whole array
/// per row, so it is measurably more expensive than the `jsonb_set` case —
/// the caller halves the batch size when any target carries a wildcard.
///
/// The already-masked guard sits on the ELEMENT, not the row: an array with one
/// unmasked element must still be selected, and a fully masked one must not be
/// re-counted after a resume. See `mask_batch_jsonb` for why.
#[allow(clippy::too_many_arguments)]
pub async fn mask_batch_jsonb_wildcard(
    conn: &mut AsyncPgConnection,
    table: TargetTable,
    column: TargetColumn,
    app_id: Uuid,
    day: chrono::NaiveDate,
    sub_path: &[String],
    cursor: BatchCursor,
    limit: i64,
    action_id: Uuid,
    worker_id: &str,
) -> QueryResult<Option<BatchOutcome>> {
    let (t, c) = (table.as_sql(), column.as_sql());
    let sql = format!(
        "WITH sel AS ( \
           SELECT id, occurred_at FROM {t} \
           WHERE app_id=$1 AND occurred_at >= $2 AND occurred_at < $3 \
             AND ($4::timestamptz IS NULL OR (occurred_at, id) > ($4, $5)) \
             AND jsonb_typeof({c}) = 'array' \
             AND EXISTS (SELECT 1 FROM jsonb_array_elements({c}) el \
                         WHERE el #> $6 IS NOT NULL AND el #> $6 <> '\"****\"'::jsonb) \
           ORDER BY occurred_at, id LIMIT $7), \
         upd AS ( \
           UPDATE {t} e \
           SET {c} = coalesce(( \
               SELECT jsonb_agg( \
                        CASE WHEN el #> $6 IS NOT NULL \
                             THEN jsonb_set(el, $6, '\"****\"'::jsonb, false) ELSE el END \
                        ORDER BY ord) \
               FROM jsonb_array_elements(e.{c}) WITH ORDINALITY AS t(el, ord)), '[]'::jsonb) \
           FROM sel WHERE e.id = sel.id AND e.occurred_at = sel.occurred_at \
             AND e.occurred_at >= $2 AND e.occurred_at < $3 \
           RETURNING 1 AS one) \
         UPDATE inspector_mask_actions SET \
           cursor_occurred_at = (SELECT max(occurred_at) FROM sel), \
           cursor_id = (SELECT id FROM sel ORDER BY occurred_at DESC, id DESC LIMIT 1), \
           rows_masked = rows_masked + (SELECT count(*) FROM upd), \
           rows_scanned = rows_scanned + (SELECT count(*) FROM sel), \
           claimed_at = now() \
         WHERE id = $8 AND worker_id = $9 \
         RETURNING (SELECT count(*) FROM sel)::bigint AS scanned, \
                   (SELECT count(*) FROM upd)::bigint AS masked, \
                   cursor_occurred_at AS cur_occurred_at, cursor_id AS cur_id, status"
    );
    let lo = day.and_hms_opt(0, 0, 0).unwrap().and_utc();
    let hi = lo + chrono::Duration::days(1);
    let row: Option<BatchRow> = diesel::sql_query(sql)
        .bind::<SqlUuid, _>(app_id)
        .bind::<Timestamptz, _>(lo)
        .bind::<Timestamptz, _>(hi)
        .bind::<Nullable<Timestamptz>, _>(cursor.occurred_at)
        .bind::<Nullable<SqlUuid>, _>(cursor.id)
        .bind::<Array<Text>, _>(sub_path.to_vec())
        .bind::<BigInt, _>(limit)
        .bind::<SqlUuid, _>(action_id)
        .bind::<Text, _>(worker_id)
        .get_result(conn)
        .await
        .optional()?;
    Ok(row.map(|r| to_outcome(r, limit)))
}

/// TEXT columns take the WHOLE value. No partial redaction: the workspace has
/// no direct regex dependency and partial masking leaves recoverable residue.
#[allow(clippy::too_many_arguments)]
pub async fn mask_batch_text(
    conn: &mut AsyncPgConnection,
    table: TargetTable,
    column: TargetColumn,
    app_id: Uuid,
    day: chrono::NaiveDate,
    cursor: BatchCursor,
    limit: i64,
    action_id: Uuid,
    worker_id: &str,
) -> QueryResult<Option<BatchOutcome>> {
    let (t, c) = (table.as_sql(), column.as_sql());
    let sql = format!(
        "WITH sel AS ( \
           SELECT id, occurred_at FROM {t} \
           WHERE app_id=$1 AND occurred_at >= $2 AND occurred_at < $3 \
             AND ($4::timestamptz IS NULL OR (occurred_at, id) > ($4, $5)) \
             AND {c} IS NOT NULL AND {c} <> '****' \
           ORDER BY occurred_at, id LIMIT $6), \
         upd AS ( \
           UPDATE {t} e SET {c} = '****' \
           FROM sel WHERE e.id = sel.id AND e.occurred_at = sel.occurred_at \
             AND e.occurred_at >= $2 AND e.occurred_at < $3 \
           RETURNING 1 AS one) \
         UPDATE inspector_mask_actions SET \
           cursor_occurred_at = (SELECT max(occurred_at) FROM sel), \
           cursor_id = (SELECT id FROM sel ORDER BY occurred_at DESC, id DESC LIMIT 1), \
           rows_masked = rows_masked + (SELECT count(*) FROM upd), \
           rows_scanned = rows_scanned + (SELECT count(*) FROM sel), \
           claimed_at = now() \
         WHERE id = $7 AND worker_id = $8 \
         RETURNING (SELECT count(*) FROM sel)::bigint AS scanned, \
                   (SELECT count(*) FROM upd)::bigint AS masked, \
                   cursor_occurred_at AS cur_occurred_at, cursor_id AS cur_id, status"
    );
    let lo = day.and_hms_opt(0, 0, 0).unwrap().and_utc();
    let hi = lo + chrono::Duration::days(1);
    let row: Option<BatchRow> = diesel::sql_query(sql)
        .bind::<SqlUuid, _>(app_id)
        .bind::<Timestamptz, _>(lo)
        .bind::<Timestamptz, _>(hi)
        .bind::<Nullable<Timestamptz>, _>(cursor.occurred_at)
        .bind::<Nullable<SqlUuid>, _>(cursor.id)
        .bind::<BigInt, _>(limit)
        .bind::<SqlUuid, _>(action_id)
        .bind::<Text, _>(worker_id)
        .get_result(conn)
        .await
        .optional()?;
    Ok(row.map(|r| to_outcome(r, limit)))
}

/// The `_default` phase, against the child BY NAME.
///
/// `repo::list_child_partitions` excludes `{table}_default` by design, so
/// those rows are never tiered and never dropped — they are the longest-lived
/// PII in the system. Rows CANNOT be in the default partition inside a covered
/// range (Postgres rejects `CREATE TABLE ... PARTITION OF ...` if the default
/// holds a conflicting row); they are there because their `occurred_at` is
/// OUTSIDE every explicit range — clock-skewed clients, offline queues.
///
/// Bounded by the same `>= now() - tier_hot_days` predicate as every other
/// phase: without it this would happily rewrite rows years older than the hot
/// window, contradicting the hot/cold rule and the `cold_rows_skipped` number.
///
/// Carries the same already-masked guard as `mask_batch_jsonb`, for the same
/// reason: this phase is re-entered whole on every resume.
#[allow(clippy::too_many_arguments)]
pub async fn mask_default_partition_batch(
    conn: &mut AsyncPgConnection,
    table: TargetTable,
    column: TargetColumn,
    app_id: Uuid,
    lo_bound: DateTime<Utc>,
    path: &[String],
    cursor: BatchCursor,
    limit: i64,
    action_id: Uuid,
    worker_id: &str,
) -> QueryResult<Option<BatchOutcome>> {
    // The child name is derived internally from our own suffix, never input.
    let child = format!("{}_default", table.as_sql());
    let c = column.as_sql();
    let sql = format!(
        "WITH sel AS ( \
           SELECT id, occurred_at FROM {child} \
           WHERE app_id=$1 AND occurred_at >= $2 \
             AND ($3::timestamptz IS NULL OR (occurred_at, id) > ($3, $4)) \
             AND {c} #> $5 IS NOT NULL AND {c} #> $5 <> '\"****\"'::jsonb \
           ORDER BY occurred_at, id LIMIT $6), \
         upd AS ( \
           UPDATE {child} e \
           SET {c} = jsonb_set(coalesce(e.{c}, '{{}}'::jsonb), $5, '\"****\"'::jsonb, false) \
           FROM sel WHERE e.id = sel.id AND e.occurred_at = sel.occurred_at \
           RETURNING 1 AS one) \
         UPDATE inspector_mask_actions SET \
           cursor_occurred_at = (SELECT max(occurred_at) FROM sel), \
           cursor_id = (SELECT id FROM sel ORDER BY occurred_at DESC, id DESC LIMIT 1), \
           rows_masked = rows_masked + (SELECT count(*) FROM upd), \
           rows_scanned = rows_scanned + (SELECT count(*) FROM sel), \
           claimed_at = now() \
         WHERE id = $7 AND worker_id = $8 \
         RETURNING (SELECT count(*) FROM sel)::bigint AS scanned, \
                   (SELECT count(*) FROM upd)::bigint AS masked, \
                   cursor_occurred_at AS cur_occurred_at, cursor_id AS cur_id, status"
    );
    let row: Option<BatchRow> = diesel::sql_query(sql)
        .bind::<SqlUuid, _>(app_id)
        .bind::<Timestamptz, _>(lo_bound)
        .bind::<Nullable<Timestamptz>, _>(cursor.occurred_at)
        .bind::<Nullable<SqlUuid>, _>(cursor.id)
        .bind::<Array<Text>, _>(path.to_vec())
        .bind::<BigInt, _>(limit)
        .bind::<SqlUuid, _>(action_id)
        .bind::<Text, _>(worker_id)
        .get_result(conn)
        .await
        .optional()?;
    Ok(row.map(|r| to_outcome(r, limit)))
}

/// One keyset pass over a non-partitioned companion table, filtered on
/// `app_id`. No day loop — these are orders of magnitude smaller than the
/// event tables. `path` empty means the column is TEXT and takes `'****'`.
///
/// The cursor is `ORDER BY id DESC LIMIT 1`, not `max(id)`: Postgres ships no
/// `max(uuid)` aggregate, so `max(id)` is not a slower spelling of the same
/// thing — it is a hard `function max(uuid) does not exist` on the very first
/// rollup batch, at 3am, in an unattended worker.
///
/// The jsonb match excludes an already-masked value exactly as the TEXT match
/// excludes `'****'`; without it the two branches disagree about whether a
/// re-run double-counts. See `mask_batch_jsonb`.
#[allow(clippy::too_many_arguments)]
pub async fn mask_rollup_batch(
    conn: &mut AsyncPgConnection,
    table: TargetTable,
    column: TargetColumn,
    app_id: Uuid,
    path: &[String],
    after_id: Option<Uuid>,
    limit: i64,
    action_id: Uuid,
    worker_id: &str,
) -> QueryResult<Option<BatchOutcome>> {
    let (t, c) = (table.as_sql(), column.as_sql());
    let set_expr = if path.is_empty() {
        format!("{c} = '****'")
    } else {
        format!("{c} = jsonb_set(coalesce(e.{c}, '{{}}'::jsonb), $3, '\"****\"'::jsonb, false)")
    };
    let match_expr = if path.is_empty() {
        format!("{c} IS NOT NULL AND {c} <> '****'")
    } else {
        format!("{c} #> $3 IS NOT NULL AND {c} #> $3 <> '\"****\"'::jsonb")
    };
    let sql = format!(
        "WITH sel AS ( \
           SELECT id FROM {t} \
           WHERE app_id=$1 AND ($2::uuid IS NULL OR id > $2) AND {match_expr} \
           ORDER BY id LIMIT $4), \
         upd AS (UPDATE {t} e SET {set_expr} FROM sel WHERE e.id = sel.id RETURNING 1 AS one) \
         UPDATE inspector_mask_actions SET \
           cursor_id = (SELECT id FROM sel ORDER BY id DESC LIMIT 1), cursor_occurred_at = NULL, \
           rows_masked = rows_masked + (SELECT count(*) FROM upd), \
           rows_scanned = rows_scanned + (SELECT count(*) FROM sel), \
           claimed_at = now() \
         WHERE id = $5 AND worker_id = $6 \
         RETURNING (SELECT count(*) FROM sel)::bigint AS scanned, \
                   (SELECT count(*) FROM upd)::bigint AS masked, \
                   cursor_occurred_at AS cur_occurred_at, cursor_id AS cur_id, status"
    );
    let row: Option<BatchRow> = diesel::sql_query(sql)
        .bind::<SqlUuid, _>(app_id)
        .bind::<Nullable<SqlUuid>, _>(after_id)
        .bind::<Array<Text>, _>(path.to_vec())
        .bind::<BigInt, _>(limit)
        .bind::<SqlUuid, _>(action_id)
        .bind::<Text, _>(worker_id)
        .get_result(conn)
        .await
        .optional()?;
    Ok(row.map(|r| to_outcome(r, limit)))
}

/// The tail sweep closes the enforcement race between "mask applied" and
/// "every pipeline replica's policy cache refreshed".
///
/// Keyed on `received_at`, NOT `occurred_at`: `occurred_at` is the CLIENT's
/// timestamp (`process.rs` sets `occurred_at: ev.timestamp`), so a mobile SDK
/// offline queue or a skewed clock flushes events whose `occurred_at` is days
/// old — those rows land in a partition the day loop already swept and would
/// never be revisited. The `occurred_at` range stays for PRUNING only, because
/// `error_events.received_at` has no index.
///
/// Carries the same already-masked guard as `mask_batch_jsonb`: the sweep runs
/// again in full whenever the action is resumed.
#[allow(clippy::too_many_arguments)]
pub async fn mask_tail_sweep_batch(
    conn: &mut AsyncPgConnection,
    table: TargetTable,
    column: TargetColumn,
    app_id: Uuid,
    lo: DateTime<Utc>,
    hi: DateTime<Utc>,
    received_since: DateTime<Utc>,
    path: &[String],
    cursor: BatchCursor,
    limit: i64,
    action_id: Uuid,
    worker_id: &str,
) -> QueryResult<Option<BatchOutcome>> {
    let (t, c) = (table.as_sql(), column.as_sql());
    let sql = format!(
        "WITH sel AS ( \
           SELECT id, occurred_at FROM {t} \
           WHERE app_id=$1 AND occurred_at >= $2 AND occurred_at < $3 \
             AND received_at >= $4 \
             AND ($5::timestamptz IS NULL OR (occurred_at, id) > ($5, $6)) \
             AND {c} #> $7 IS NOT NULL AND {c} #> $7 <> '\"****\"'::jsonb \
           ORDER BY occurred_at, id LIMIT $8), \
         upd AS ( \
           UPDATE {t} e \
           SET {c} = jsonb_set(coalesce(e.{c}, '{{}}'::jsonb), $7, '\"****\"'::jsonb, false) \
           FROM sel WHERE e.id = sel.id AND e.occurred_at = sel.occurred_at \
             AND e.occurred_at >= $2 AND e.occurred_at < $3 \
           RETURNING 1 AS one) \
         UPDATE inspector_mask_actions SET \
           cursor_occurred_at = (SELECT max(occurred_at) FROM sel), \
           cursor_id = (SELECT id FROM sel ORDER BY occurred_at DESC, id DESC LIMIT 1), \
           rows_masked = rows_masked + (SELECT count(*) FROM upd), \
           rows_scanned = rows_scanned + (SELECT count(*) FROM sel), \
           claimed_at = now() \
         WHERE id = $9 AND worker_id = $10 \
         RETURNING (SELECT count(*) FROM sel)::bigint AS scanned, \
                   (SELECT count(*) FROM upd)::bigint AS masked, \
                   cursor_occurred_at AS cur_occurred_at, cursor_id AS cur_id, status"
    );
    let row: Option<BatchRow> = diesel::sql_query(sql)
        .bind::<SqlUuid, _>(app_id)
        .bind::<Timestamptz, _>(lo)
        .bind::<Timestamptz, _>(hi)
        .bind::<Timestamptz, _>(received_since)
        .bind::<Nullable<Timestamptz>, _>(cursor.occurred_at)
        .bind::<Nullable<SqlUuid>, _>(cursor.id)
        .bind::<Array<Text>, _>(path.to_vec())
        .bind::<BigInt, _>(limit)
        .bind::<SqlUuid, _>(action_id)
        .bind::<Text, _>(worker_id)
        .get_result(conn)
        .await
        .optional()?;
    Ok(row.map(|r| to_outcome(r, limit)))
}

/// Preview counting: the identical day loop with `count(*)` instead of UPDATE.
///
/// Run on the INSPECTOR's pool, never the API's. Counting `col #> path IS NOT
/// NULL` over an app's hot window is a Parallel Append seq scan — 184 ms per
/// 210k rows measured — with no index that can serve it, since the tags GIN is
/// `jsonb_path_ops` and answers `@>` only. On the API's 16-connection pool
/// that is how the whole dashboard goes down.
pub async fn count_batch_jsonb(
    conn: &mut AsyncPgConnection,
    table: TargetTable,
    column: TargetColumn,
    app_id: Uuid,
    day: chrono::NaiveDate,
    path: &[String],
) -> QueryResult<i64> {
    let (t, c) = (table.as_sql(), column.as_sql());
    let sql = format!(
        "SELECT count(*)::bigint AS n FROM {t} \
         WHERE app_id=$1 AND occurred_at >= $2 AND occurred_at < $3 AND {c} #> $4 IS NOT NULL"
    );
    let lo = day.and_hms_opt(0, 0, 0).unwrap().and_utc();
    let hi = lo + chrono::Duration::days(1);
    let row: CountRow = diesel::sql_query(sql)
        .bind::<SqlUuid, _>(app_id)
        .bind::<Timestamptz, _>(lo)
        .bind::<Timestamptz, _>(hi)
        .bind::<Array<Text>, _>(path.to_vec())
        .get_result(conn)
        .await?;
    Ok(row.n)
}

pub async fn count_batch_text(
    conn: &mut AsyncPgConnection,
    table: TargetTable,
    column: TargetColumn,
    app_id: Uuid,
    day: chrono::NaiveDate,
) -> QueryResult<i64> {
    let (t, c) = (table.as_sql(), column.as_sql());
    let sql = format!(
        "SELECT count(*)::bigint AS n FROM {t} \
         WHERE app_id=$1 AND occurred_at >= $2 AND occurred_at < $3 \
           AND {c} IS NOT NULL AND {c} <> '****'"
    );
    let lo = day.and_hms_opt(0, 0, 0).unwrap().and_utc();
    let hi = lo + chrono::Duration::days(1);
    let row: CountRow = diesel::sql_query(sql)
        .bind::<SqlUuid, _>(app_id)
        .bind::<Timestamptz, _>(lo)
        .bind::<Timestamptz, _>(hi)
        .get_result(conn)
        .await?;
    Ok(row.n)
}

/// How many rows have a SQL NULL in a jsonb column. Exists so the "jsonb_set
/// returns NULL if any argument is NULL" bug is a test rather than a
/// production incident.
pub async fn count_null_column(
    conn: &mut AsyncPgConnection,
    table: &'static str,
    column: &'static str,
    app_id: Uuid,
) -> QueryResult<i64> {
    let sql =
        format!("SELECT count(*)::bigint AS n FROM {table} WHERE app_id=$1 AND {column} IS NULL");
    let row: CountRow = diesel::sql_query(sql)
        .bind::<SqlUuid, _>(app_id)
        .get_result(conn)
        .await?;
    Ok(row.n)
}

// ===========================================================================
// PII inspector: session settings + retention
// ===========================================================================

/// Bound every statement this connection runs.
///
/// MUST be paired with [`reset_statement_timeout`] before `drop(conn)`:
/// deadpool's recycle does NOT reset session state, so a leaked `SET` silently
/// poisons a later checkout in the same process — an API request that has
/// nothing to do with the inspector then fails at 30 seconds with a message
/// nobody can trace. This is the ONLY place the setting is written; never an
/// ad-hoc `SET` at a call site.
///
/// The value is formatted, not bound, because `SET` does not take parameters.
/// It is an `i64` from `Config`, never caller input.
pub async fn set_statement_timeout(conn: &mut AsyncPgConnection, ms: u64) -> QueryResult<()> {
    conn.batch_execute(&format!("SET statement_timeout = {ms}"))
        .await
}

pub async fn reset_statement_timeout(conn: &mut AsyncPgConnection) -> QueryResult<()> {
    conn.batch_execute("RESET statement_timeout").await
}

/// Keep the newest `keep` scans per policy.
///
/// Findings are deleted in BOUNDED batches before the parent row is dropped.
/// The house prune idiom has no LIMIT, and an unbounded cascading DELETE of up
/// to 660k findings is a bloat and lock spike — a nightly scan producing 33k
/// findings is 12M rows a year, which is the exact failure `alert_events`'
/// reaper doc comment warns about.
pub async fn prune_inspector_scans(
    conn: &mut AsyncPgConnection,
    keep: i64,
    batch: i64,
) -> QueryResult<usize> {
    // Findings first, in batches, so the cascade never has to.
    loop {
        let n = diesel::sql_query(
            "DELETE FROM inspector_findings WHERE ctid IN ( \
               SELECT f.ctid FROM inspector_findings f \
               WHERE f.scan_id IN ( \
                 SELECT id FROM ( \
                   SELECT id, row_number() OVER (PARTITION BY policy_id ORDER BY created_at DESC) rn \
                   FROM inspector_scans) r WHERE r.rn > $1) \
               LIMIT $2)",
        )
        .bind::<BigInt, _>(keep)
        .bind::<BigInt, _>(batch)
        .execute(conn)
        .await?;
        if n == 0 {
            break;
        }
    }
    diesel::sql_query(
        "DELETE FROM inspector_scans WHERE id IN ( \
           SELECT id FROM ( \
             SELECT id, row_number() OVER (PARTITION BY policy_id ORDER BY created_at DESC) rn \
             FROM inspector_scans) r WHERE r.rn > $1)",
    )
    .bind::<BigInt, _>(keep)
    .execute(conn)
    .await
}

/// Age out findings, stamping the owning scan so a scan row's
/// `findings_count` and its empty finding list never silently disagree.
pub async fn prune_inspector_findings(
    conn: &mut AsyncPgConnection,
    days: i64,
    batch: i64,
) -> QueryResult<usize> {
    let mut total = 0usize;
    loop {
        let n = diesel::sql_query(
            "WITH doomed AS ( \
               SELECT ctid, scan_id FROM inspector_findings \
               WHERE created_at < now() - ($1 || ' days')::interval LIMIT $2), \
             stamped AS ( \
               UPDATE inspector_scans s SET findings_reaped_at = now() \
               WHERE s.id IN (SELECT scan_id FROM doomed) AND s.findings_reaped_at IS NULL \
               RETURNING 1) \
             DELETE FROM inspector_findings f \
             WHERE f.ctid IN (SELECT ctid FROM doomed)",
        )
        .bind::<BigInt, _>(days)
        .bind::<BigInt, _>(batch)
        .execute(conn)
        .await?;
        total += n;
        if n == 0 {
            break;
        }
    }
    Ok(total)
}

/// Delete inspector policies whose target no longer resolves.
///
/// [`delete_app`] and [`delete_project`] stop NEW orphans; this repairs the
/// ones already on disk, and covers the paths those two do not own — an app or
/// project removed by a direct SQL delete, and an enrollment retired out from
/// under an `app_env` policy.
///
/// `NOT EXISTS` against the right table per `target_type`, never a bare `NOT IN
/// (SELECT ...)`: a single NULL in the subquery makes `NOT IN` return NULL for
/// every row, which silently deletes NOTHING and would leave this looking like
/// it ran clean. `target_type` is `TEXT` + CHECK constrained to exactly these
/// three, so an unrecognized value cannot exist — but the match is written
/// exhaustively anyway so a fourth type added later fails closed (kept, not
/// deleted) rather than being reaped by a wildcard.
pub async fn prune_orphaned_inspector_policies(
    conn: &mut AsyncPgConnection,
    batch: i64,
) -> QueryResult<usize> {
    diesel::sql_query(
        "DELETE FROM inspector_policies p \
          WHERE p.id IN ( \
            SELECT id FROM inspector_policies q \
             WHERE (q.target_type = 'project' \
                    AND NOT EXISTS (SELECT 1 FROM projects x WHERE x.id = q.target_id)) \
                OR (q.target_type = 'app' \
                    AND NOT EXISTS (SELECT 1 FROM apps x WHERE x.id = q.target_id)) \
                OR (q.target_type = 'app_env' \
                    AND NOT EXISTS ( \
                          SELECT 1 FROM app_environments x WHERE x.id = q.target_id)) \
             LIMIT $1)",
    )
    .bind::<BigInt, _>(batch)
    .execute(conn)
    .await
}

/// Abandoned previews are not audit-relevant, so this ALWAYS runs.
pub async fn prune_mask_previews(conn: &mut AsyncPgConnection, days: i64) -> QueryResult<usize> {
    diesel::sql_query(
        "DELETE FROM inspector_mask_actions \
         WHERE kind='preview' AND status IN ('preview','previewed','failed','cancelled') \
           AND requested_at < now() - ($1 || ' days')::interval",
    )
    .bind::<BigInt, _>(days)
    .execute(conn)
    .await
}

/// Prune terminal MASK actions, and ONLY when explicitly enabled.
///
/// `days = 0` means never. This table grows per human action, not per rule
/// evaluation, and it is the record a compliance question is answered from.
pub async fn prune_mask_actions(
    conn: &mut AsyncPgConnection,
    days: i64,
    batch: i64,
) -> QueryResult<usize> {
    if days <= 0 {
        return Ok(0);
    }
    diesel::sql_query(
        "DELETE FROM inspector_mask_actions WHERE ctid IN ( \
           SELECT ctid FROM inspector_mask_actions \
           WHERE kind='mask' AND status IN ('done','failed','cancelled') \
             AND requested_at < now() - ($1 || ' days')::interval \
           LIMIT $2)",
    )
    .bind::<BigInt, _>(days)
    .bind::<BigInt, _>(batch)
    .execute(conn)
    .await
}

/// Null the staff identities on old audit rows, keeping counts and targets.
///
/// Everywhere else in this schema a user row cascades (`refresh_tokens`,
/// `role_grants`), so deleting a user IS the product's de-facto erasure
/// mechanism. `ON DELETE SET NULL` plus a denormalized email breaks that by
/// design — deliberately, so the trail survives — which makes this the only
/// un-erasable store of staff PII in the product unless it is aged out.
pub async fn pseudonymize_mask_actions(
    conn: &mut AsyncPgConnection,
    days: i64,
) -> QueryResult<usize> {
    diesel::sql_query(
        "UPDATE inspector_mask_actions \
         SET requested_by_email='', cancelled_by_email='', confirm_source='' \
         WHERE requested_at < now() - ($1 || ' days')::interval \
           AND (requested_by_email <> '' OR cancelled_by_email <> '' OR confirm_source <> '')",
    )
    .bind::<BigInt, _>(days)
    .execute(conn)
    .await
}

/// Record why a scheduled run was not started. Kept as its own statement so
/// the reason is a plain `&str` rather than a lifetime inside the patch
/// struct, and so the write is one round trip.
pub async fn record_policy_skip(
    conn: &mut AsyncPgConnection,
    id: Uuid,
    reason: &str,
) -> QueryResult<usize> {
    diesel::sql_query("UPDATE inspector_policies SET last_skip_reason = $2 WHERE id = $1")
        .bind::<SqlUuid, _>(id)
        .bind::<Text, _>(reason)
        .execute(conn)
        .await
}

/// Point a policy at the scan it most recently started.
pub async fn record_policy_scan(
    conn: &mut AsyncPgConnection,
    id: Uuid,
    scan_id: Uuid,
) -> QueryResult<usize> {
    diesel::sql_query(
        "UPDATE inspector_policies SET last_scan_id = $2, last_skip_reason = '' WHERE id = $1",
    )
    .bind::<SqlUuid, _>(id)
    .bind::<SqlUuid, _>(scan_id)
    .execute(conn)
    .await
}

/// Why an enqueue did or did not produce a scan.
///
/// An enum rather than a bool because the two callers need different
/// answers from the same logic: the scheduler logs and moves on, the API
/// turns each arm into a distinct status code.
// `InspectorScan` is 432 bytes, so `clippy::large_enum_variant` (a `-D
// warnings` gate here) wants the payload boxed. Suppressed rather than
// satisfied: exactly one of these is constructed per enqueue — a rare,
// once-per-scan operation — and it is returned by value and immediately
// destructured. Boxing would buy a heap allocation per scan and force the
// scan row through a `Deref` at both call sites in exchange for nothing.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum EnqueueOutcome {
    Queued(InspectorScan),
    /// The partial unique index refused a second active scan.
    AlreadyActive,
    /// The target is no longer inside the policy's org.
    TargetGone,
    /// Neither tracked keys nor detectors: it would report a confident false
    /// negative, which is the worst thing a privacy scan can emit.
    NoMatchers,
    /// Every target pair is covered by a more specific policy.
    FullySubtracted,
}

/// Freeze a policy into a scan row. The ONLY way a scan is created.
///
/// Re-validates the target against the org even though the API already did:
/// `inspector_policies.target_id` has no FK, and grants outlive targets.
pub async fn enqueue_scan_for_policy(
    conn: &mut AsyncPgConnection,
    cfg: &sauron_core::Config,
    policy: &InspectorPolicy,
    trigger: &str,
    requested_by: Option<Uuid>,
) -> anyhow::Result<EnqueueOutcome> {
    if !validate_scope_in_org(conn, policy.org_id, &policy.target_type, policy.target_id).await? {
        return Ok(EnqueueOutcome::TargetGone);
    }
    let Some(level) = PolicyTargetType::from_sql(&policy.target_type) else {
        return Ok(EnqueueOutcome::TargetGone);
    };

    let keys = sauron_inspector::matching::parse_tracked_keys(&policy.tracked_keys);
    let dets = sauron_inspector::detect::parse_detectors(&policy.detectors);
    if keys.is_empty() && dets.is_empty() {
        return Ok(EnqueueOutcome::NoMatchers);
    }

    // Detector mode changes the cost model by an order of magnitude — no
    // prefilter, every row shipped out of Postgres, every string leaf walked —
    // so it gets its own much shorter window.
    let window_days = if dets.is_empty() {
        policy.window_days as i64
    } else {
        cfg.inspector_detector_window_days
    }
    .min(cfg.inspector_window_days);

    let to = Utc::now();
    let from = to - chrono::Duration::days(window_days);

    let pairs: Vec<ScanPair> = scan_pairs_for_node(conn, &policy.target_type, policy.target_id)
        .await?
        .into_iter()
        .map(|(app_id, app_env_id)| ScanPair { app_id, app_env_id })
        .collect();
    let narrower: Vec<PolicyNode> =
        list_inspector_policies_under(conn, &policy.target_type, policy.target_id)
            .await?
            .into_iter()
            .filter_map(|(t, id)| {
                PolicyTargetType::from_sql(&t).map(|tt| PolicyNode {
                    target_type: tt,
                    target_id: id,
                })
            })
            .collect();
    let node = PolicyNode {
        target_type: level,
        target_id: policy.target_id,
    };
    let resolved = sauron_inspector::targets::resolve_targets(node, &pairs, &narrower);
    if resolved.pairs.is_empty() {
        return Ok(EnqueueOutcome::FullySubtracted);
    }

    let tables = tables_for(&policy.rollups);
    let units = units_for(&resolved.pairs, &tables, from, to, level);

    let params = serde_json::json!({
        "tracked_keys": policy.tracked_keys,
        "detectors": policy.detectors,
        "scan_columns": policy.scan_columns,
        "rollups": policy.rollups,
        "tables": tables,
        "level": policy.target_type,
    });
    let targets_json = serde_json::Value::Array(
        resolved
            .pairs
            .iter()
            .map(|p| serde_json::json!([p.app_id, p.app_env_id]))
            .collect(),
    );

    let scan = match insert_inspector_scan(
        conn,
        crate::models::NewInspectorScan {
            policy_id: policy.id,
            org_id: policy.org_id,
            trigger_type: trigger,
            requested_by,
            window_from: from,
            window_to: to,
            params: &params,
            targets: &targets_json,
            units_total: units.len() as i32,
        },
    )
    .await
    {
        Ok(s) => s,
        // The partial unique index refusing a second active scan is the
        // arbiter, not a handler check, so two schedulers racing produce one.
        Err(diesel::result::Error::DatabaseError(
            diesel::result::DatabaseErrorKind::UniqueViolation,
            _,
        )) => return Ok(EnqueueOutcome::AlreadyActive),
        Err(e) => return Err(e.into()),
    };

    if resolved.subtracted > 0 || resolved.truncated {
        let note = format!(
            "{} target pair(s) excluded by a more specific policy{}",
            resolved.subtracted,
            if resolved.truncated {
                "; target list truncated at the cap"
            } else {
                ""
            }
        );
        note_scan_coverage(conn, scan.id, "partial", &note).await?;
    }
    record_policy_scan(conn, policy.id, scan.id).await?;
    Ok(EnqueueOutcome::Queued(scan))
}

/// Record a coverage downgrade on a scan without touching its status.
pub async fn note_scan_coverage(
    conn: &mut AsyncPgConnection,
    scan_id: Uuid,
    coverage: &str,
    note: &str,
) -> QueryResult<usize> {
    diesel::sql_query(
        "UPDATE inspector_scans SET coverage=$2, \
                coverage_note = CASE WHEN coverage_note = '' THEN $3 \
                                     ELSE coverage_note || '; ' || $3 END \
         WHERE id=$1",
    )
    .bind::<SqlUuid, _>(scan_id)
    .bind::<Text, _>(coverage)
    .bind::<Text, _>(note)
    .execute(conn)
    .await
}

/// One phase-1 page. THREE statement shapes, because the tables genuinely
/// differ and one shape produces `column "occurred_at" does not exist`.
///
/// `Ranged` is the partitioned case: an INDEX-BOUNDED inner window, then the
/// prefilter on the outer statement. Both halves matter. Putting the LIMIT on
/// the same statement as the ILIKE bounds MATCHES, not SCANNED ROWS — and the
/// design's premise is that the prefilter eliminates 95-99% of rows, so such a
/// statement must scan the ENTIRE app-day range to emit fewer than `limit`
/// rows. Three consequences, all bad: no heartbeat and no inter-batch pause
/// for the whole scan (so the duty cycle is fiction); `statement_timeout`
/// aborts somewhere around 2-3M rows per app-day; and on abort THE CURSOR
/// NEVER ADVANCES, so the retry replays the identical statement and
/// `INSPECTOR_MAX_ATTEMPTS` permanently fails the scan. The
/// `(app_id, environment_id, occurred_at)` predicate matches
/// `error_events_app_env_time_users_idx` /
/// `analytics_events_app_env_time_users_idx` exactly.
///
/// `DefaultChild` reads `{table}_default` BY NAME with no time predicate: the
/// rows are in that child precisely because their `occurred_at` is outside
/// every explicit range, so a windowed query cannot see them. The child name
/// is derived from our own suffix, never from input — the same construction
/// `mask_default_partition_batch` uses.
///
/// `Rollup` reads a non-partitioned companion with an `id` keyset and NO time
/// and NO environment predicate. `issues`, `event_users` and `identities`
/// have neither column, and `inspector_policies.rollups` defaults to
/// `["issues","event_users"]` — so a shared shape here fails on the shipped
/// default policy, not on some exotic configuration.
///
/// Column names are `&'static str`s from the inventory; every value is bound.
#[derive(Debug, Clone, Copy)]
pub enum ScanShape {
    Ranged {
        env_id: Option<Uuid>,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    },
    DefaultChild,
    Rollup,
}

/// Keyset position. `occurred_at` is `None` for a rollup, which has none.
#[derive(Debug, Clone, Copy, Default)]
pub struct ScanCursor {
    pub occurred_at: Option<DateTime<Utc>>,
    pub id: Option<Uuid>,
}

pub struct ScanRow {
    pub id: Uuid,
    /// `None` on a rollup. `inspector_findings.first_seen_at` /
    /// `last_seen_at` / `sample_occurred_at` are nullable for this reason.
    pub occurred_at: Option<DateTime<Utc>>,
    pub columns: Vec<(String, Value)>,
}

#[allow(clippy::too_many_arguments)]
pub async fn scan_window_rows(
    conn: &mut AsyncPgConnection,
    table: &str,
    cols: &[&'static str],
    app_id: Uuid,
    shape: ScanShape,
    cursor: ScanCursor,
    limit: i64,
    patterns: &[String],
    text_patterns: &[String],
) -> QueryResult<Vec<ScanRow>> {
    // The identifiers come from the inventory in `sauron-inspector`; refuse
    // anything else rather than interpolating it. `table` is always the PARENT
    // name even for the default child, so this check is never bypassed.
    if sauron_inspector::columns::table_class(table).is_none()
        || cols
            .iter()
            .any(|c| sauron_inspector::columns::find(table, c).is_none())
    {
        return Ok(Vec::new());
    }
    let payload = cols
        .iter()
        .map(|c| format!("'{c}', to_jsonb(e.{c})"))
        .collect::<Vec<_>>()
        .join(", ");

    // A TEXT column holds no JSON, so the quoted `%"email"%` pattern the jsonb
    // columns use matches nothing in it — which is how ten `default_on` TEXT
    // columns come to report zero findings with `coverage='full'`. Each column
    // gets the pattern array for its own kind.
    // Both pattern arrays are ALWAYS bound, so the statement must ALWAYS
    // mention both. Postgres derives a prepared statement's parameter count
    // from the highest `$n` it can see and answers `bind message supplies 9
    // parameters, but prepared statement requires 4` otherwise. Two ways to
    // hit that without this floor: a detector-only policy (no prefilter at
    // all, both arrays empty) and any all-jsonb column set (`$6` never
    // referenced), which is `analytics_events`' entire default set.
    const PARAM_FLOOR: &str = " AND ($5::text[] IS NOT NULL OR $6::text[] IS NOT NULL OR TRUE)";
    let ilike = if patterns.is_empty() && text_patterns.is_empty() {
        PARAM_FLOOR.to_string()
    } else {
        let ors = cols
            .iter()
            .map(|c| {
                let is_text = sauron_inspector::columns::find(table, c)
                    .map(|e| e.kind == sauron_inspector::columns::ColumnKind::Text)
                    .unwrap_or(false);
                if is_text {
                    format!("e.{c} ILIKE ANY($6)")
                } else {
                    format!("e.{c}::text ILIKE ANY($5)")
                }
            })
            .collect::<Vec<_>>()
            .join(" OR ");
        format!("{PARAM_FLOOR} AND ({ors})")
    };

    // The inert value bound to `$3`/`$4` on the two shapes that have no time
    // predicate. It must be a timestamp POSTGRES CAN REPRESENT:
    // `DateTime::<Utc>::MIN_UTC` is the year -262144 and Postgres' floor is
    // 4713 BC, so binding it fails the whole statement with `timestamp out of
    // range` before the server ever sees it — which took out every
    // `DefaultChild` and `Rollup` unit, i.e. the `_default` sweep and the
    // shipped `["issues","event_users"]` rollups. Both parameters are
    // neutralized by an `IS NULL OR TRUE` on those shapes, so the value is
    // never compared against anything and the epoch is as good as any.
    const INERT_TS: DateTime<Utc> = DateTime::<Utc>::UNIX_EPOCH;

    let (sql, env_id, lo, hi) = match shape {
        ScanShape::Ranged { env_id, from, to } => (
            // `env_id = NULL` is THE UNATTRIBUTED BUCKET, not "no filter".
            // `scan_pairs_for_node` emits one `(app, NULL)` pair per app
            // precisely because `EnvFilter::Subset` uses `= ANY`, which never
            // matches NULL, so those rows are unreachable from an env-scoped
            // policy — and `accumulate` labels this unit `env_scope =
            // 'unattributed'`. Spelling it `$2 IS NULL OR ...` instead makes
            // that unit read EVERY row of the app-day a second time: counts
            // double, enrollment rows are relabelled `unattributed`, and a
            // project policy walks the very environment a narrower policy
            // subtracted. Both arms stay indexable on
            // `(app_id, environment_id, occurred_at DESC)`.
            format!(
                "WITH win AS ( \
                   SELECT id, occurred_at FROM {table} \
                   WHERE app_id = $1 \
                     AND (environment_id = $2 \
                          OR ($2::uuid IS NULL AND environment_id IS NULL)) \
                     AND occurred_at >= $3 AND occurred_at < $4 \
                     AND ($7::timestamptz IS NULL OR (occurred_at, id) > ($7, $8)) \
                   ORDER BY occurred_at, id LIMIT $9) \
                 SELECT e.id, e.occurred_at, jsonb_build_object({payload}) AS payload \
                 FROM {table} e JOIN win ON e.id = win.id AND e.occurred_at = win.occurred_at \
                 WHERE e.occurred_at >= $3 AND e.occurred_at < $4{ilike} \
                 ORDER BY e.occurred_at, e.id"
            ),
            env_id,
            from,
            to,
        ),
        ScanShape::DefaultChild => {
            let child = format!("{table}_default");
            (
                format!(
                    "WITH win AS ( \
                       SELECT id, occurred_at FROM {child} \
                       WHERE app_id = $1 AND ($2::uuid IS NULL OR TRUE) \
                         AND ($3::timestamptz IS NULL OR TRUE) \
                         AND ($4::timestamptz IS NULL OR TRUE) \
                         AND ($7::timestamptz IS NULL OR (occurred_at, id) > ($7, $8)) \
                       ORDER BY occurred_at, id LIMIT $9) \
                     SELECT e.id, e.occurred_at, jsonb_build_object({payload}) AS payload \
                     FROM {child} e JOIN win ON e.id = win.id AND e.occurred_at = win.occurred_at \
                     WHERE TRUE{ilike} \
                     ORDER BY e.occurred_at, e.id"
                ),
                None,
                INERT_TS,
                INERT_TS,
            )
        }
        // No `occurred_at`, no `environment_id`, no window CTE: these tables
        // are orders of magnitude smaller than the event tables and one `id`
        // keyset walks them.
        ScanShape::Rollup => (
            format!(
                "SELECT e.id, NULL::timestamptz AS occurred_at, \
                        jsonb_build_object({payload}) AS payload \
                 FROM {table} e \
                 WHERE e.app_id = $1 AND ($2::uuid IS NULL OR TRUE) \
                   AND ($3::timestamptz IS NULL OR TRUE) \
                   AND ($4::timestamptz IS NULL OR TRUE) \
                   AND ($7::timestamptz IS NULL OR TRUE) \
                   AND ($8::uuid IS NULL OR e.id > $8){ilike} \
                 ORDER BY e.id LIMIT $9"
            ),
            None,
            INERT_TS,
            INERT_TS,
        ),
    };
    // Every shape binds all nine parameters in the same order, with the
    // irrelevant ones neutralized by an `IS NULL OR TRUE`. Postgres rejects a
    // statement whose parameter count does not match the bind list, and a
    // per-shape bind list is a fourth thing to keep in sync.

    #[derive(QueryableByName)]
    struct RawRow {
        #[diesel(sql_type = SqlUuid)]
        id: Uuid,
        #[diesel(sql_type = Nullable<Timestamptz>)]
        occurred_at: Option<DateTime<Utc>>,
        #[diesel(sql_type = Jsonb)]
        payload: Value,
    }
    let rows: Vec<RawRow> = diesel::sql_query(sql)
        .bind::<SqlUuid, _>(app_id)
        .bind::<Nullable<SqlUuid>, _>(env_id)
        .bind::<Timestamptz, _>(lo)
        .bind::<Timestamptz, _>(hi)
        .bind::<Array<Text>, _>(patterns.to_vec())
        .bind::<Array<Text>, _>(text_patterns.to_vec())
        .bind::<Nullable<Timestamptz>, _>(cursor.occurred_at)
        .bind::<Nullable<SqlUuid>, _>(cursor.id)
        .bind::<BigInt, _>(limit)
        .load(conn)
        .await?;
    Ok(rows
        .into_iter()
        .map(|r| ScanRow {
            id: r.id,
            occurred_at: r.occurred_at,
            columns: r
                .payload
                .as_object()
                .map(|o| o.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                .unwrap_or_default(),
        })
        .collect())
}

/// Whether `user_id` is active AND holds `permission` on `app_id`.
///
/// Re-evaluated at claim time, in the worker's process, because confirm's
/// authorization can be hours old by the time a queued action runs.
/// Deliberately does NOT accept an env-scoped grant: masking is app-scoped, and
/// `authorize_app` — which this mirrors — never resolves an env grant either.
pub async fn user_is_active_with_app_permission(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    app_id: Uuid,
    permission: &str,
) -> QueryResult<bool> {
    #[derive(QueryableByName)]
    struct OkRow {
        #[diesel(sql_type = Bool)]
        ok: bool,
    }
    let row: OkRow = diesel::sql_query(
        "SELECT EXISTS ( \
           SELECT 1 FROM role_grants g \
           JOIN roles r ON r.id = g.role_id \
           JOIN users u ON u.id = g.user_id \
           JOIN apps a ON a.id = $2 \
           JOIN projects p ON p.id = a.project_id \
           WHERE g.user_id = $1 AND u.is_active \
             AND r.permissions @> to_jsonb(ARRAY[$3::text]) \
             AND ( (g.scope_type = 'org' AND g.scope_id = p.org_id) \
                OR (g.scope_type = 'project' AND g.scope_id = p.id) \
                OR (g.scope_type = 'app' AND g.scope_id = a.id) ) \
         ) AS ok",
    )
    .bind::<SqlUuid, _>(user_id)
    .bind::<SqlUuid, _>(app_id)
    .bind::<Text, _>(permission)
    .get_result(conn)
    .await?;
    Ok(row.ok)
}

/// Fold the rows a day skipped for being at or below the tier boundary would
/// have masked into the audit row, so a `done` action with a small
/// `rows_masked` is explicable.
///
/// ROWS, not days. The column, the CSV header, the Audit tab column and the
/// MaskDialog all say rows, and a day count sitting in a column called
/// `cold_rows_skipped` next to `rows_masked` is a number an operator will
/// read as rows and act on.
pub async fn add_cold_skip(
    conn: &mut AsyncPgConnection,
    action_id: Uuid,
    rows: i64,
) -> QueryResult<usize> {
    diesel::sql_query(
        "UPDATE inspector_mask_actions SET cold_rows_skipped = cold_rows_skipped + $2 WHERE id = $1",
    )
    .bind::<SqlUuid, _>(action_id)
    .bind::<BigInt, _>(rows)
    .execute(conn)
    .await
}
