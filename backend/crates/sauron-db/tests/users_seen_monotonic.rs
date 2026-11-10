//! `issues.users_seen` write-backs never lower the stored count.
//!
//! The value is a HyperLogLog estimate read from Redis BEFORE the write, and
//! two write-backs can commit in the other order than they read: two ingest
//! replicas, or the pending-set drain racing a hot-path write. Both write
//! forms therefore carry `AND users_seen < $n`. Without it, writing 5 and then
//! a stale 3 leaves 3, and this test fails.

mod common;

use chrono::Utc;
use common::TestDb;
use sauron_db::models::NewIssue;
use sauron_db::{batch, repo};
use uuid::Uuid;

async fn seed_issues(conn: &mut sauron_db::PgConn, n: usize) -> (Uuid, Vec<Uuid>) {
    let s = Uuid::new_v4().simple().to_string();
    let org = repo::create_org(conn, "us org", &format!("us-org-{s}"))
        .await
        .expect("org");
    let project = repo::create_project(conn, org.id, "us project", &format!("us-project-{s}"))
        .await
        .expect("project");
    let app = repo::create_app(conn, project.id, "us app", &format!("us-app-{s}"), "web")
        .await
        .expect("app");
    let mut ids = Vec::new();
    for i in 0..n {
        let fp = format!("users-seen-monotonic-{i}");
        let id = repo::upsert_issue(
            conn,
            NewIssue {
                app_id: app.id,
                fingerprint: &fp,
                type_: "TypeError",
                title: "t",
                culprit: "c",
                level: "error",
                first_seen: Utc::now(),
                last_seen: Utc::now(),
                times_seen: 1,
            },
        )
        .await
        .expect("issue");
        ids.push(id);
    }
    (app.id, ids)
}

async fn users_seen(conn: &mut sauron_db::PgConn, app_id: Uuid, id: Uuid) -> i64 {
    repo::issues_by_ids(conn, app_id, &[id])
        .await
        .expect("read issue")
        .remove(0)
        .users_seen
}

/// `repo::set_issue_users_seen`: the hot per-item path and the pending-set
/// drain.
#[tokio::test]
async fn the_single_row_write_back_never_lowers_users_seen() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;
    let (app_id, ids) = seed_issues(&mut conn, 1).await;
    let id = ids[0];

    assert_eq!(
        repo::set_issue_users_seen(&mut conn, id, 5).await.unwrap(),
        1
    );
    assert_eq!(users_seen(&mut conn, app_id, id).await, 5);

    assert_eq!(
        repo::set_issue_users_seen(&mut conn, id, 3).await.unwrap(),
        0,
        "a lower estimate must touch no row"
    );
    assert_eq!(users_seen(&mut conn, app_id, id).await, 5, "5 is kept");

    assert_eq!(
        repo::set_issue_users_seen(&mut conn, id, 5).await.unwrap(),
        0,
        "an equal estimate is a no-op too"
    );
    assert_eq!(
        repo::set_issue_users_seen(&mut conn, id, 8).await.unwrap(),
        1
    );
    assert_eq!(
        users_seen(&mut conn, app_id, id).await,
        8,
        "a higher one is written"
    );

    drop(conn);
    db.cleanup().await;
}

/// `batch::set_issue_users_seen`: the batched hot path's multi-row statement.
/// One row goes down and one goes up in the same statement, so a predicate
/// that applied to the statement as a whole would fail one of them.
#[tokio::test]
async fn the_multi_row_write_back_never_lowers_users_seen() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;
    let (app_id, ids) = seed_issues(&mut conn, 2).await;
    let (a, b) = (ids[0], ids[1]);

    assert_eq!(
        batch::set_issue_users_seen(&mut conn, &[(a, 5), (b, 5)])
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        batch::set_issue_users_seen(&mut conn, &[(a, 3), (b, 7)])
            .await
            .unwrap(),
        1,
        "only the row whose estimate went up is written"
    );
    assert_eq!(
        users_seen(&mut conn, app_id, a).await,
        5,
        "5 is kept, not 3"
    );
    assert_eq!(users_seen(&mut conn, app_id, b).await, 7);

    drop(conn);
    db.cleanup().await;
}
