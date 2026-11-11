//! Personal API key storage (migration 83) and the org MCP switch.

mod common;

use chrono::{Duration, Utc};
use common::TestDb;
use sauron_db::{api_keys, org_settings, repo};
use uuid::Uuid;

async fn user(conn: &mut sauron_db::PgConn) -> Uuid {
    let email = format!("k-{}@example.com", Uuid::new_v4().simple());
    repo::create_user(conn, &email, "hash", "Key Owner")
        .await
        .expect("user")
        .id
}

fn hash() -> String {
    format!("h-{}", Uuid::new_v4().simple())
}

#[tokio::test]
async fn authenticate_finds_live_key_and_rejects_revoked_and_expired() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;
    let u = user(&mut conn).await;

    let h = hash();
    let key = api_keys::insert(&mut conn, u, "k", "sauron_pat_abcdef", &h, None)
        .await
        .unwrap();
    let owner = api_keys::authenticate(&mut conn, &h)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((owner.key_id, owner.user_id), (key.id, u));

    api_keys::revoke(&mut conn, u, key.id)
        .await
        .unwrap()
        .unwrap();
    assert!(api_keys::authenticate(&mut conn, &h)
        .await
        .unwrap()
        .is_none());

    let h2 = hash();
    api_keys::insert(
        &mut conn,
        u,
        "old",
        "sauron_pat_abcdef",
        &h2,
        Some(Utc::now() - Duration::seconds(1)),
    )
    .await
    .unwrap();
    assert!(api_keys::authenticate(&mut conn, &h2)
        .await
        .unwrap()
        .is_none());
    drop(conn);
    db.cleanup().await;
}

#[tokio::test]
async fn revoke_is_scoped_to_owner() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;
    let owner = user(&mut conn).await;
    let other = user(&mut conn).await;
    let h = hash();
    let key = api_keys::insert(&mut conn, owner, "k", "sauron_pat_abcdef", &h, None)
        .await
        .unwrap();

    assert!(api_keys::revoke(&mut conn, other, key.id)
        .await
        .unwrap()
        .is_none());
    assert!(api_keys::authenticate(&mut conn, &h)
        .await
        .unwrap()
        .is_some());

    let revoked = api_keys::revoke(&mut conn, owner, key.id)
        .await
        .unwrap()
        .unwrap();
    assert!(revoked.revoked_at.is_some());
    // Already revoked: nothing to do.
    assert!(api_keys::revoke(&mut conn, owner, key.id)
        .await
        .unwrap()
        .is_none());
    drop(conn);
    db.cleanup().await;
}

#[tokio::test]
async fn revoke_all_for_user_counts_only_live_keys() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;
    let u = user(&mut conn).await;
    let mut ids = vec![];
    for i in 0..3 {
        let k = api_keys::insert(
            &mut conn,
            u,
            &format!("k{i}"),
            "sauron_pat_abcdef",
            &hash(),
            None,
        )
        .await
        .unwrap();
        ids.push(k.id);
    }
    api_keys::revoke(&mut conn, u, ids[0])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        api_keys::count_live_for_user(&mut conn, u).await.unwrap(),
        2
    );
    assert_eq!(
        api_keys::list_for_user(&mut conn, u, false)
            .await
            .unwrap()
            .len(),
        2
    );

    assert_eq!(
        api_keys::revoke_all_for_user(&mut conn, u, api_keys::REVOKE_PASSWORD_RESET, None)
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        api_keys::count_live_for_user(&mut conn, u).await.unwrap(),
        0
    );
    drop(conn);
    db.cleanup().await;
}

/// Every revocation records why and by whom (migration 85), and the owner's
/// "Show revoked" list and the Members summary read it back.
#[tokio::test]
async fn revocations_record_reason_and_actor_and_summaries_read_them() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;
    let owner = user(&mut conn).await;
    let admin = user(&mut conn).await;
    let keyless = user(&mut conn).await;
    let mut ids = vec![];
    for i in 0..4 {
        let k = api_keys::insert(&mut conn, owner, &format!("k{i}"), "p", &hash(), None)
            .await
            .unwrap();
        ids.push(k.id);
    }

    // The owner revokes one; an admin revokes the rest.
    let own = api_keys::revoke(&mut conn, owner, ids[0])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (own.revoked_reason.as_deref(), own.revoked_by),
        (Some(api_keys::REVOKE_USER_REVOKED), Some(owner))
    );
    let summary = api_keys::summaries_for_users(&mut conn, &[owner, keyless])
        .await
        .unwrap();
    assert_eq!(summary[&owner].active, 3);
    assert_eq!(summary[&owner].revoked_by_password_reset_at, None);
    assert!(
        !summary.contains_key(&keyless),
        "a user who never held a key has no row"
    );

    assert_eq!(
        api_keys::revoke_all_for_user(&mut conn, owner, api_keys::REVOKE_ADMIN, Some(admin))
            .await
            .unwrap(),
        3
    );
    let listed = api_keys::list_for_user(&mut conn, owner, true)
        .await
        .unwrap();
    assert_eq!(listed.len(), 4, "every revoked key is listed");
    assert!(listed.iter().all(|k| k.revoked_at.is_some()));
    let by_admin: Vec<_> = listed
        .iter()
        .filter(|k| k.revoked_reason.as_deref() == Some(api_keys::REVOKE_ADMIN))
        .collect();
    assert_eq!(by_admin.len(), 3);
    assert!(by_admin.iter().all(|k| k.revoked_by == Some(admin)));
    assert!(
        api_keys::list_for_user(&mut conn, owner, false)
            .await
            .unwrap()
            .is_empty(),
        "without include_revoked only live keys are listed"
    );

    // A reset through the emailed link: no actor, and the summary dates it.
    let fresh = api_keys::insert(&mut conn, owner, "after", "p", &hash(), None)
        .await
        .unwrap();
    // An expired key is not an active one.
    api_keys::insert(
        &mut conn,
        owner,
        "stale",
        "p",
        &hash(),
        Some(chrono::Utc::now() - chrono::Duration::seconds(1)),
    )
    .await
    .unwrap();
    assert_eq!(
        api_keys::summaries_for_users(&mut conn, &[owner])
            .await
            .unwrap()[&owner]
            .active,
        1
    );
    assert_eq!(
        api_keys::revoke_all_for_user(&mut conn, owner, api_keys::REVOKE_PASSWORD_RESET, None)
            .await
            .unwrap(),
        2
    );
    let reset = api_keys::list_for_user(&mut conn, owner, true)
        .await
        .unwrap()
        .into_iter()
        .find(|k| k.id == fresh.id)
        .unwrap();
    assert_eq!(
        (reset.revoked_reason.as_deref(), reset.revoked_by),
        (Some(api_keys::REVOKE_PASSWORD_RESET), None)
    );
    let summary = &api_keys::summaries_for_users(&mut conn, &[owner])
        .await
        .unwrap()[&owner];
    assert_eq!(summary.active, 0);
    assert_eq!(summary.revoked_by_password_reset_at, reset.revoked_at);
    drop(conn);
    db.cleanup().await;
}

#[tokio::test]
async fn authenticate_reports_owner_state() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;
    let u = user(&mut conn).await;
    let h = hash();
    api_keys::insert(&mut conn, u, "k", "sauron_pat_abcdef", &h, None)
        .await
        .unwrap();

    let o = api_keys::authenticate(&mut conn, &h)
        .await
        .unwrap()
        .unwrap();
    assert!(o.is_active && !o.credentials_invalidated);

    repo::set_user_active(&mut conn, u, false).await.unwrap();
    let o = api_keys::authenticate(&mut conn, &h)
        .await
        .unwrap()
        .unwrap();
    assert!(!o.is_active);

    repo::set_user_credentials_invalidated(&mut conn, u, Some(Utc::now()))
        .await
        .unwrap();
    let o = api_keys::authenticate(&mut conn, &h)
        .await
        .unwrap()
        .unwrap();
    assert!(o.credentials_invalidated);
    drop(conn);
    db.cleanup().await;
}

#[tokio::test]
async fn touch_last_used_is_throttled() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;
    let u = user(&mut conn).await;
    let key = api_keys::insert(&mut conn, u, "k", "sauron_pat_abcdef", &hash(), None)
        .await
        .unwrap();
    assert!(key.last_used_at.is_none());

    api_keys::touch_last_used(&mut conn, key.id).await.unwrap();
    let first = api_keys::list_for_user(&mut conn, u, false).await.unwrap()[0].last_used_at;
    assert!(first.is_some());

    api_keys::touch_last_used(&mut conn, key.id).await.unwrap();
    let second = api_keys::list_for_user(&mut conn, u, false).await.unwrap()[0].last_used_at;
    assert_eq!(first, second);
    drop(conn);
    db.cleanup().await;
}

#[tokio::test]
async fn mcp_enabled_defaults_true_and_round_trips() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;
    let slug = format!("mcp-{}", Uuid::new_v4().simple());
    let org = repo::create_org(&mut conn, "mcp org", &slug).await.unwrap();
    assert!(org.mcp_enabled);
    assert!(org_settings::mcp_enabled(&mut conn, org.id).await.unwrap());

    assert!(org_settings::set_mcp_enabled(&mut conn, org.id, false)
        .await
        .unwrap());
    assert!(!org_settings::mcp_enabled(&mut conn, org.id).await.unwrap());
    assert!(!org_settings::set_mcp_enabled(&mut conn, org.id, true)
        .await
        .unwrap());
    drop(conn);
    db.cleanup().await;
}
