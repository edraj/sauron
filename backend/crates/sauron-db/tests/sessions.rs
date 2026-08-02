//! `auth_sessions` against a real Postgres: the mint/continue upsert, the four
//! revoke helpers, the list query and the reaper.
//!
//! Skips (does not fail) when `TEST_DATABASE_URL` is unset, mirroring
//! `env_scoping.rs` and `workflows.rs`. CI has no database service.

mod common;

use chrono::{Duration, Utc};
use common::TestDb;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use sauron_db::models::{AuthSession, RefreshToken};
use sauron_db::repo;
use sauron_db::schema::{auth_sessions, refresh_tokens};
use uuid::Uuid;

/// A real `users` row — every `auth_sessions` FK needs one.
async fn seed_user(conn: &mut sauron_db::PgConn) -> Uuid {
    let suffix = Uuid::new_v4().simple().to_string();
    repo::create_user(
        conn,
        &format!("sessions-{suffix}@example.com"),
        "not-a-real-hash",
        "Sessions Fixture",
    )
    .await
    .expect("create user")
    .id
}

async fn session_row(conn: &mut sauron_db::PgConn, id: Uuid) -> AuthSession {
    auth_sessions::table
        .find(id)
        .select(AuthSession::as_select())
        .first(conn)
        .await
        .expect("auth_sessions row")
}

async fn tokens_for_session(conn: &mut sauron_db::PgConn, id: Uuid) -> Vec<RefreshToken> {
    refresh_tokens::table
        .filter(refresh_tokens::session_id.eq(id))
        .select(RefreshToken::as_select())
        .order(refresh_tokens::created_at.asc())
        .load(conn)
        .await
        .expect("refresh_tokens rows")
}

#[tokio::test]
async fn a_fresh_session_id_creates_one_session_and_one_token() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut conn = db.conn().await;
    let user_id = seed_user(&mut conn).await;
    let sid = Uuid::new_v4();

    let rows = repo::start_or_continue_session(
        &mut conn,
        sid,
        user_id,
        "hash-a".to_string(),
        Utc::now() + Duration::days(30),
        Some("Mozilla/5.0 (Macintosh)".to_string()),
        Some("203.0.113.7".to_string()),
    )
    .await
    .expect("start session");
    assert_eq!(rows, 1, "one refresh token inserted");

    let session = session_row(&mut conn, sid).await;
    assert_eq!(session.user_id, user_id);
    assert_eq!(
        session.user_agent.as_deref(),
        Some("Mozilla/5.0 (Macintosh)")
    );
    assert_eq!(session.ip.as_deref(), Some("203.0.113.7"));
    assert!(session.revoked_at.is_none());
    assert_eq!(tokens_for_session(&mut conn, sid).await.len(), 1);

    db.cleanup().await;
}

#[tokio::test]
async fn continuing_a_session_adds_a_token_and_keeps_the_login_time_facts() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut conn = db.conn().await;
    let user_id = seed_user(&mut conn).await;
    let sid = Uuid::new_v4();
    let first_expiry = Utc::now() + Duration::days(10);

    repo::start_or_continue_session(
        &mut conn,
        sid,
        user_id,
        "hash-a".to_string(),
        first_expiry,
        Some("original-agent".to_string()),
        Some("203.0.113.7".to_string()),
    )
    .await
    .expect("start session");
    let before = session_row(&mut conn, sid).await;

    let second_expiry = Utc::now() + Duration::days(30);
    let rows = repo::start_or_continue_session(
        &mut conn,
        sid,
        user_id,
        "hash-b".to_string(),
        second_expiry,
        Some("attacker-agent".to_string()),
        Some("198.51.100.9".to_string()),
    )
    .await
    .expect("continue session");
    assert_eq!(rows, 1);

    let after = session_row(&mut conn, sid).await;
    assert_eq!(tokens_for_session(&mut conn, sid).await.len(), 2);
    assert!(
        after.last_used_at > before.last_used_at,
        "last_used_at bumped"
    );
    assert!(after.expires_at > before.expires_at, "expires_at slides");
    // The UI renders these next to created_at as login-time facts ("was that
    // login me?"). Last-writer-wins would let whoever rotates the token destroy
    // the original login's evidence and leave the row looking unchanged.
    assert_eq!(after.user_agent.as_deref(), Some("original-agent"));
    assert_eq!(after.ip.as_deref(), Some("203.0.113.7"));
    assert_eq!(after.created_at, before.created_at);

    db.cleanup().await;
}

#[tokio::test]
async fn a_revoked_session_cannot_be_resurrected_by_a_rotation() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut conn = db.conn().await;
    let user_id = seed_user(&mut conn).await;
    let sid = Uuid::new_v4();

    repo::start_or_continue_session(
        &mut conn,
        sid,
        user_id,
        "hash-a".to_string(),
        Utc::now() + Duration::days(30),
        None,
        None,
    )
    .await
    .expect("start session");

    diesel::update(auth_sessions::table.find(sid))
        .set((
            auth_sessions::revoked_at.eq(Utc::now()),
            auth_sessions::revoked_reason.eq(repo::REVOKE_USER_REVOKED_OTHERS),
        ))
        .execute(&mut conn)
        .await
        .expect("revoke session");

    // The centrepiece guard, asserted at the SQL layer where it lives: the
    // 10-second refresh-race grace window can otherwise resurrect a session the
    // user just killed. A Rust-side pre-check would be a TOCTOU race and must
    // not be substituted.
    let rows = repo::start_or_continue_session(
        &mut conn,
        sid,
        user_id,
        "hash-b".to_string(),
        Utc::now() + Duration::days(30),
        None,
        None,
    )
    .await
    .expect("continue a revoked session must not error");
    assert_eq!(rows, 0, "no token may be minted for a revoked session");
    assert_eq!(tokens_for_session(&mut conn, sid).await.len(), 1);

    db.cleanup().await;
}

#[tokio::test]
async fn a_session_id_belonging_to_another_user_mints_nothing() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut conn = db.conn().await;
    let owner = seed_user(&mut conn).await;
    let stranger = seed_user(&mut conn).await;
    let sid = Uuid::new_v4();

    repo::start_or_continue_session(
        &mut conn,
        sid,
        owner,
        "hash-a".to_string(),
        Utc::now() + Duration::days(30),
        None,
        None,
    )
    .await
    .expect("start session");

    let rows = repo::start_or_continue_session(
        &mut conn,
        sid,
        stranger,
        "hash-b".to_string(),
        Utc::now() + Duration::days(30),
        None,
        None,
    )
    .await
    .expect("mis-threaded session id must not error");
    assert_eq!(
        rows, 0,
        "a mis-threaded session id must never cross-link users"
    );

    db.cleanup().await;
}

/// Mint one session with one live token. Returns the session id.
async fn seed_session(conn: &mut sauron_db::PgConn, user_id: Uuid, hash: &str) -> Uuid {
    let sid = Uuid::new_v4();
    repo::start_or_continue_session(
        conn,
        sid,
        user_id,
        hash.to_string(),
        Utc::now() + Duration::days(30),
        Some("agent".to_string()),
        Some("203.0.113.7".to_string()),
    )
    .await
    .expect("seed session");
    sid
}

async fn live_token_count(conn: &mut sauron_db::PgConn, user_id: Uuid) -> i64 {
    refresh_tokens::table
        .filter(refresh_tokens::user_id.eq(user_id))
        .filter(refresh_tokens::revoked_at.is_null())
        .count()
        .get_result(conn)
        .await
        .expect("count live tokens")
}

#[tokio::test]
async fn revoke_session_is_scoped_to_its_owner_and_reports_the_session_not_the_tokens() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut conn = db.conn().await;
    let owner = seed_user(&mut conn).await;
    let stranger = seed_user(&mut conn).await;
    let sid = seed_session(&mut conn, owner, "hash-a").await;

    // Absent, already-revoked and someone-else's all look identical, so the
    // handler can map every empty result to 404 and the response cannot be used
    // to probe which session ids exist.
    let foreign = repo::revoke_session(
        &mut conn,
        sid,
        stranger,
        repo::REVOKE_USER_REVOKED,
        Some(stranger),
    )
    .await
    .expect("foreign revoke");
    assert!(
        foreign.is_empty(),
        "one user must never revoke another's session"
    );
    assert!(session_row(&mut conn, sid).await.revoked_at.is_none());

    // A live session whose refresh tokens are all already dead must still report
    // itself as revoked. Reading the token UPDATE's row count instead would
    // answer 404 on a successful revoke, and the handler would then skip
    // `mark_revoked` — leaving the killed session's access token good for the
    // full 900s.
    diesel::update(refresh_tokens::table.filter(refresh_tokens::session_id.eq(sid)))
        .set((
            refresh_tokens::revoked_at.eq(Utc::now()),
            refresh_tokens::revoked_reason.eq(repo::REVOKE_ROTATED),
        ))
        .execute(&mut conn)
        .await
        .expect("kill the session's tokens");

    let ids = repo::revoke_session(
        &mut conn,
        sid,
        owner,
        repo::REVOKE_USER_REVOKED,
        Some(owner),
    )
    .await
    .expect("owner revoke");
    assert_eq!(ids, vec![sid]);
    let row = session_row(&mut conn, sid).await;
    assert!(row.revoked_at.is_some());
    assert_eq!(
        row.revoked_reason.as_deref(),
        Some(repo::REVOKE_USER_REVOKED)
    );
    assert_eq!(row.revoked_by, Some(owner));

    // Second call is a no-op, not an error.
    let again = repo::revoke_session(
        &mut conn,
        sid,
        owner,
        repo::REVOKE_USER_REVOKED,
        Some(owner),
    )
    .await
    .expect("idempotent revoke");
    assert!(again.is_empty());

    db.cleanup().await;
}

#[tokio::test]
async fn revoke_sessions_for_user_spares_the_exception_and_leaves_no_live_token_behind() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut conn = db.conn().await;
    let user_id = seed_user(&mut conn).await;
    let keep = seed_session(&mut conn, user_id, "hash-keep").await;
    let kill = seed_session(&mut conn, user_id, "hash-kill").await;

    // A session-less legacy token, minted before migration 000035. "Sign out my
    // other devices" means those too.
    diesel::insert_into(refresh_tokens::table)
        .values((
            refresh_tokens::user_id.eq(user_id),
            refresh_tokens::token_hash.eq("hash-legacy"),
            refresh_tokens::expires_at.eq(Utc::now() + Duration::days(30)),
        ))
        .execute(&mut conn)
        .await
        .expect("insert legacy token");

    let ids = repo::revoke_sessions_for_user(
        &mut conn,
        user_id,
        Some(keep),
        repo::REVOKE_USER_REVOKED_OTHERS,
        Some(user_id),
    )
    .await
    .expect("revoke others");
    assert_eq!(ids, vec![kill]);
    assert!(session_row(&mut conn, keep).await.revoked_at.is_none());
    assert!(session_row(&mut conn, kill).await.revoked_at.is_some());
    assert_eq!(
        live_token_count(&mut conn, user_id).await,
        1,
        "only the spared session's token may survive; the legacy token dies too"
    );

    // With no exception, nothing may be left alive — otherwise
    // `user_has_active_refresh_token` still returns true after an account-wide
    // kill and the grace window's invariant weakens.
    let ids =
        repo::revoke_sessions_for_user(&mut conn, user_id, None, repo::REVOKE_ADMIN, Some(user_id))
            .await
            .expect("revoke all");
    assert_eq!(ids, vec![keep]);
    assert_eq!(live_token_count(&mut conn, user_id).await, 0);

    db.cleanup().await;
}

#[tokio::test]
async fn logout_takes_the_session_with_the_token_but_never_rewrites_a_rotation() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut conn = db.conn().await;
    let user_id = seed_user(&mut conn).await;
    let sid = seed_session(&mut conn, user_id, "hash-a").await;

    let revoked = repo::revoke_refresh_token_and_session(&mut conn, "hash-a", repo::REVOKE_LOGOUT)
        .await
        .expect("logout");
    assert_eq!(revoked, Some(sid));
    assert!(session_row(&mut conn, sid).await.revoked_at.is_some());

    // Rotate a second session, then "log out" with the already-rotated token.
    // Rewriting `rotated` to `logout` would stop the 10-second multi-tab grace
    // window firing for the other tab.
    let sid2 = seed_session(&mut conn, user_id, "hash-b").await;
    repo::revoke_refresh_token(&mut conn, "hash-b", repo::REVOKE_ROTATED)
        .await
        .expect("rotate");
    let revoked = repo::revoke_refresh_token_and_session(&mut conn, "hash-b", repo::REVOKE_LOGOUT)
        .await
        .expect("logout with a rotated token");
    assert_eq!(revoked, None);
    let (_, _, _, reason) = repo::refresh_token_revocation(&mut conn, "hash-b")
        .await
        .expect("revocation metadata")
        .expect("row exists");
    assert_eq!(reason.as_deref(), Some(repo::REVOKE_ROTATED));
    assert!(session_row(&mut conn, sid2).await.revoked_at.is_none());

    db.cleanup().await;
}

#[tokio::test]
async fn list_sessions_hides_revoked_and_expired_rows_unless_asked() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut conn = db.conn().await;
    let user_id = seed_user(&mut conn).await;
    let live = seed_session(&mut conn, user_id, "hash-live").await;
    let dead = seed_session(&mut conn, user_id, "hash-dead").await;
    let stale = seed_session(&mut conn, user_id, "hash-stale").await;

    repo::revoke_session(
        &mut conn,
        dead,
        user_id,
        repo::REVOKE_USER_REVOKED,
        Some(user_id),
    )
    .await
    .expect("revoke");
    diesel::update(auth_sessions::table.find(stale))
        .set(auth_sessions::expires_at.eq(Utc::now() - Duration::days(1)))
        .execute(&mut conn)
        .await
        .expect("expire");

    let listed = repo::list_auth_sessions(&mut conn, user_id, false)
        .await
        .expect("list live");
    assert_eq!(listed.iter().map(|s| s.id).collect::<Vec<_>>(), vec![live]);

    let listed = repo::list_auth_sessions(&mut conn, user_id, true)
        .await
        .expect("list with history");
    let ids: Vec<Uuid> = listed.iter().map(|s| s.id).collect();
    assert!(ids.contains(&live));
    assert!(
        ids.contains(&dead),
        "a revocation inside 30 days is history the owner may see"
    );
    assert!(
        !ids.contains(&stale),
        "an expired-but-never-revoked row is not history"
    );

    db.cleanup().await;
}

#[tokio::test]
async fn revoked_session_ids_returns_only_the_recent_window() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut conn = db.conn().await;
    let user_id = seed_user(&mut conn).await;
    let recent = seed_session(&mut conn, user_id, "hash-recent").await;
    let ancient = seed_session(&mut conn, user_id, "hash-ancient").await;

    repo::revoke_session(
        &mut conn,
        recent,
        user_id,
        repo::REVOKE_USER_REVOKED,
        Some(user_id),
    )
    .await
    .expect("revoke recent");
    repo::revoke_session(
        &mut conn,
        ancient,
        user_id,
        repo::REVOKE_USER_REVOKED,
        Some(user_id),
    )
    .await
    .expect("revoke ancient");
    diesel::update(auth_sessions::table.find(ancient))
        .set(auth_sessions::revoked_at.eq(Utc::now() - Duration::hours(6)))
        .execute(&mut conn)
        .await
        .expect("age the ancient revocation");

    let ids = repo::revoked_session_ids(&mut conn, 1020)
        .await
        .expect("poll");
    assert!(ids.contains(&recent));
    assert!(
        !ids.contains(&ancient),
        "a revocation older than the window cannot have a live access token left"
    );

    db.cleanup().await;
}

#[tokio::test]
async fn the_reaper_deletes_old_sessions_and_orphans_their_tokens_rather_than_deleting_them() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut conn = db.conn().await;
    let user_id = seed_user(&mut conn).await;
    let old = seed_session(&mut conn, user_id, "hash-old").await;
    let fresh = seed_session(&mut conn, user_id, "hash-fresh").await;

    repo::revoke_session(
        &mut conn,
        old,
        user_id,
        repo::REVOKE_USER_REVOKED,
        Some(user_id),
    )
    .await
    .expect("revoke");
    diesel::update(auth_sessions::table.find(old))
        .set(auth_sessions::revoked_at.eq(Utc::now() - Duration::days(40)))
        .execute(&mut conn)
        .await
        .expect("age it");

    let deleted = repo::prune_auth_sessions(&mut conn, repo::AUTH_SESSION_RETENTION_DAYS)
        .await
        .expect("prune");
    assert_eq!(deleted, 1);
    assert!(session_row(&mut conn, fresh).await.revoked_at.is_none());

    // ON DELETE SET NULL, not CASCADE. Deleting the token history would make a
    // replayed token look like one that never existed: a plain 401, no family
    // kill, no WARN.
    let orphan: RefreshToken = refresh_tokens::table
        .filter(refresh_tokens::token_hash.eq("hash-old"))
        .select(RefreshToken::as_select())
        .first(&mut conn)
        .await
        .expect("token row survives the reap");
    assert_eq!(orphan.session_id, None);

    db.cleanup().await;
}
