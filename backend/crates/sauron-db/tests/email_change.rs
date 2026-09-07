//! `email_change_requests` at the repository layer.
//!
//! Three properties live in SQL the Rust compiler cannot check, and each one is
//! load-bearing for the feature's security model:
//!
//!   * `email_change_one_live_per_user` — a partial unique index is what makes
//!     "a second request supersedes the first" hold under two *concurrent*
//!     admins. Application-level supersede-then-insert loses that race and
//!     leaves two live approve links, which is a mistyped domain holding a
//!     standing claim on the account.
//!   * the fingerprint match inside `consume_email_change_approval`'s UPDATE —
//!     a read-then-burn would leave a window in which the address moves.
//!   * single-use, likewise inside one `UPDATE … RETURNING`.
//!
//! Skips (does not fail) when `TEST_DATABASE_URL` is unset — see `common`.

mod common;

use chrono::{Duration, Utc};
use common::TestDb;
use sauron_db::models::NewEmailChangeRequest;
use sauron_db::repo;
use uuid::Uuid;

/// A user and an org to hang requests off. Neither is joined to the other —
/// `email_change_requests` only needs both ids to exist for its two FKs.
async fn seed(db: &TestDb) -> (Uuid, Uuid) {
    let mut conn = db.conn().await;
    let suffix = Uuid::new_v4();
    let user = repo::create_user(
        &mut conn,
        &format!("old-{suffix}@example.com"),
        "argon2-hash-placeholder",
        "Old Name",
    )
    .await
    .expect("create user");
    let org = repo::create_org(&mut conn, "Acme", &format!("acme-{suffix}"))
        .await
        .expect("create org");
    (user.id, org.id)
}

/// A request row for `user_id`. `tag` keeps the two token hashes unique across
/// calls so one test can insert several without colliding on the UNIQUE columns.
fn new_request(user_id: Uuid, org_id: Uuid, tag: &str, fingerprint: &str) -> NewEmailChangeRequest {
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
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let (user, org) = seed(&db).await;
    let mut conn = db.conn().await;

    repo::insert_email_change_request(
        &mut conn,
        new_request(user, org, "first", "old@example.com"),
    )
    .await
    .expect("first insert");

    // The index, not the handler, is what makes supersede safe under two
    // concurrent admins: both would pass an application-level check.
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

    drop(conn);
    db.cleanup().await;
}

#[tokio::test]
async fn cancelling_frees_the_slot_for_a_new_request() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let (user, org) = seed(&db).await;
    let mut conn = db.conn().await;

    repo::insert_email_change_request(
        &mut conn,
        new_request(user, org, "first", "old@example.com"),
    )
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

    // The partial index only covers live rows, so the cancelled one no longer
    // occupies the slot.
    repo::insert_email_change_request(
        &mut conn,
        new_request(user, org, "second", "old@example.com"),
    )
    .await
    .expect("the slot is free once the first is cancelled");

    drop(conn);
    db.cleanup().await;
}

#[tokio::test]
async fn consume_refuses_a_stale_fingerprint() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let (user, org) = seed(&db).await;
    let mut conn = db.conn().await;

    repo::insert_email_change_request(&mut conn, new_request(user, org, "t", "old@example.com"))
        .await
        .expect("insert");

    // The address moved since the link was issued: the link is stale and must
    // not fire. Matched inside the burn's WHERE, so it is atomic with it.
    let stale = repo::consume_email_change_approval(&mut conn, "approve-t", "moved@example.com")
        .await
        .expect("query");
    assert!(stale.is_none(), "a stale fingerprint must not burn the row");

    // And a refused attempt must not have spent the link.
    let ok = repo::consume_email_change_approval(&mut conn, "approve-t", "old@example.com")
        .await
        .expect("query");
    assert_eq!(ok, Some((user, "t@example.com".to_string())));

    drop(conn);
    db.cleanup().await;
}

#[tokio::test]
async fn a_link_burns_exactly_once() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let (user, org) = seed(&db).await;
    let mut conn = db.conn().await;

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

    drop(conn);
    db.cleanup().await;
}

#[tokio::test]
async fn the_cancel_token_kills_the_approve_token() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let (user, org) = seed(&db).await;
    let mut conn = db.conn().await;

    repo::insert_email_change_request(&mut conn, new_request(user, org, "t", "old@example.com"))
        .await
        .expect("insert");

    let cancelled = repo::cancel_email_change_by_cancel_token(&mut conn, "cancel-t")
        .await
        .expect("query");
    assert_eq!(cancelled, Some((user, org)));

    // The whole point of the veto.
    let approve = repo::consume_email_change_approval(&mut conn, "approve-t", "old@example.com")
        .await
        .expect("query");
    assert!(approve.is_none(), "the veto must kill the approve link");

    drop(conn);
    db.cleanup().await;
}

#[tokio::test]
async fn the_cancel_token_is_single_use_too() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let (user, org) = seed(&db).await;
    let mut conn = db.conn().await;

    repo::insert_email_change_request(&mut conn, new_request(user, org, "t", "old@example.com"))
        .await
        .expect("insert");

    assert!(
        repo::cancel_email_change_by_cancel_token(&mut conn, "cancel-t")
            .await
            .expect("query")
            .is_some()
    );
    // Otherwise a second click would write a second audit entry claiming the
    // member rejected the change twice.
    assert!(
        repo::cancel_email_change_by_cancel_token(&mut conn, "cancel-t")
            .await
            .expect("query")
            .is_none()
    );

    drop(conn);
    db.cleanup().await;
}

#[tokio::test]
async fn an_expired_request_is_neither_found_nor_burnable() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let (user, org) = seed(&db).await;
    let mut conn = db.conn().await;

    let mut req = new_request(user, org, "t", "old@example.com");
    req.expires_at = Utc::now() - Duration::minutes(1);
    repo::insert_email_change_request(&mut conn, req)
        .await
        .expect("insert");

    assert!(
        repo::find_live_email_change_by_approve_token(&mut conn, "approve-t")
            .await
            .expect("query")
            .is_none()
    );
    assert!(
        repo::consume_email_change_approval(&mut conn, "approve-t", "old@example.com")
            .await
            .expect("query")
            .is_none()
    );
    assert!(
        repo::cancel_email_change_by_cancel_token(&mut conn, "cancel-t")
            .await
            .expect("query")
            .is_none()
    );

    drop(conn);
    db.cleanup().await;
}

#[tokio::test]
async fn the_two_token_columns_are_not_interchangeable() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let (user, org) = seed(&db).await;
    let mut conn = db.conn().await;

    repo::insert_email_change_request(&mut conn, new_request(user, org, "t", "old@example.com"))
        .await
        .expect("insert");

    // A field-order slip in `NewEmailChangeRequest` (which decodes positionally
    // if it ever gains `Queryable`) would mail the veto link to the new address
    // and the approve link to the old one. Each lookup must see only its own
    // column.
    assert!(
        repo::find_live_email_change_by_approve_token(&mut conn, "cancel-t")
            .await
            .expect("query")
            .is_none(),
        "a cancel token must not resolve as an approve token"
    );
    assert!(
        repo::find_live_email_change_by_cancel_token(&mut conn, "approve-t")
            .await
            .expect("query")
            .is_none(),
        "an approve token must not resolve as a cancel token"
    );

    drop(conn);
    db.cleanup().await;
}

#[tokio::test]
async fn live_changes_for_org_lists_only_live_ones() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let (user, org) = seed(&db).await;
    let mut conn = db.conn().await;

    repo::insert_email_change_request(&mut conn, new_request(user, org, "t", "old@example.com"))
        .await
        .expect("insert");
    let live = repo::live_email_changes_for_org(&mut conn, org)
        .await
        .expect("query");
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].0, user);
    assert_eq!(live[0].1, "t@example.com");

    repo::cancel_live_email_change_for_user(&mut conn, user, repo::EMAIL_CHANGE_CANCELLED_ADMIN)
        .await
        .expect("cancel");
    let live = repo::live_email_changes_for_org(&mut conn, org)
        .await
        .expect("query");
    assert!(live.is_empty(), "a withdrawn change must stop badging");

    drop(conn);
    db.cleanup().await;
}

#[tokio::test]
async fn set_user_email_moves_the_login_identity_and_lowercases_it() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let (user, _org) = seed(&db).await;
    let mut conn = db.conn().await;

    repo::set_user_email(&mut conn, user, "New.Address@Example.COM")
        .await
        .expect("set email");

    // `users_email_lower_key` is on `lower(email)`, and `find_user_by_email`
    // lower-cases its argument — storing a mixed-case address would still be
    // findable, but two rows differing only in case could then both exist.
    let found = repo::find_user_by_email(&mut conn, "new.address@example.com")
        .await
        .expect("query");
    assert_eq!(found.map(|u| u.id), Some(user));

    drop(conn);
    db.cleanup().await;
}

#[tokio::test]
async fn set_user_email_reports_a_collision_rather_than_overwriting() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let (user, _org) = seed(&db).await;
    let mut conn = db.conn().await;

    let taken = format!("taken-{}@example.com", Uuid::new_v4());
    repo::create_user(&mut conn, &taken, "hash", "Other")
        .await
        .expect("create other user");

    // The residual race the confirm handler maps to 409: the address was
    // claimed between the request and the confirmation.
    let clash = repo::set_user_email(&mut conn, user, &taken).await;
    assert!(
        matches!(
            clash,
            Err(diesel::result::Error::DatabaseError(
                diesel::result::DatabaseErrorKind::UniqueViolation,
                _
            ))
        ),
        "expected a unique violation, got {clash:?}"
    );

    drop(conn);
    db.cleanup().await;
}

#[tokio::test]
async fn pruning_deletes_by_created_at_not_expiry() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let (user, org) = seed(&db).await;
    let mut conn = db.conn().await;

    repo::insert_email_change_request(&mut conn, new_request(user, org, "t", "old@example.com"))
        .await
        .expect("insert");

    // Fresh row, long retention: kept, even though a *shorter* window than its
    // 24h expiry would tempt a reaper keyed on expires_at to drop it.
    let removed = repo::prune_email_change_requests(&mut conn, 30)
        .await
        .expect("prune");
    assert_eq!(removed, 0);
    assert_eq!(
        repo::live_email_changes_for_org(&mut conn, org)
            .await
            .expect("query")
            .len(),
        1
    );

    drop(conn);
    db.cleanup().await;
}
