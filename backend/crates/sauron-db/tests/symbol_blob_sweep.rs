//! The `symbol_blobs` orphan sweep, and its division of labour with migration
//! 0067's trigger.
//!
//! The trigger keeps `refcount` honest for every blob an artifact row has ever
//! pointed at — including CASCADE deletes. What it structurally cannot see is a
//! blob whose artifact insert never happened (the upload race): no
//! `symbol_artifacts` row is ever written, so no trigger fires. The sweep
//! covers exactly that residue, from ground truth rather than the counter.

mod common;

use common::TestDb;
use diesel::sql_types::{Bool, Bytea, Text, Uuid as SqlUuid};
use diesel_async::RunQueryDsl;
use sauron_db::repo;

async fn seed_blob(c: &mut sauron_db::PgConn, tag: &str, age: &str) {
    diesel::sql_query(
        "INSERT INTO symbol_blobs \
           (sha256, content, uncompressed_size, compressed_size, refcount, created_at) \
         VALUES (sha256($1::bytea), $1::bytea, 1, 1, 1, now() - $2::interval)",
    )
    .bind::<Bytea, _>(tag.as_bytes())
    .bind::<Text, _>(age)
    .execute(c)
    .await
    .unwrap();
}

async fn blob_exists(c: &mut sauron_db::PgConn, tag: &str) -> bool {
    #[derive(diesel::QueryableByName)]
    struct Row {
        #[diesel(sql_type = Bool)]
        present: bool,
    }
    let r: Row = diesel::sql_query(
        "SELECT EXISTS(SELECT 1 FROM symbol_blobs WHERE sha256 = sha256($1::bytea)) AS present",
    )
    .bind::<Bytea, _>(tag.as_bytes())
    .get_result(c)
    .await
    .unwrap();
    r.present
}

/// The three populations the sweep must tell apart: a referenced blob (kept
/// forever), an old orphan (swept), and a YOUNG orphan (kept — it may belong to
/// an upload whose artifact insert is still in flight).
#[tokio::test]
async fn sweep_removes_only_orphans_past_the_grace_age() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut c = db.conn().await;

    seed_blob(&mut c, "referenced", "2 days").await;
    seed_blob(&mut c, "old-orphan", "2 days").await;
    seed_blob(&mut c, "young-orphan", "1 hour").await;

    diesel::sql_query(
        "INSERT INTO symbol_artifacts (app_id, kind, platform, blob_sha256) \
         VALUES ($1, 'js_sourcemap', 'web', sha256($2::bytea))",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .bind::<Bytea, _>("referenced".as_bytes())
    .execute(&mut c)
    .await
    .unwrap();

    let swept = repo::sweep_orphan_symbol_blobs(&mut c, repo::SYMBOL_BLOB_SWEEP_GRACE_HOURS)
        .await
        .unwrap();

    assert_eq!(swept, 1, "exactly the old orphan");
    assert!(
        blob_exists(&mut c, "referenced").await,
        "referenced blob must survive"
    );
    assert!(
        blob_exists(&mut c, "young-orphan").await,
        "a blob inside the grace window may be an in-flight upload and must survive"
    );
    assert!(
        !blob_exists(&mut c, "old-orphan").await,
        "the old orphan must be gone"
    );
    db.cleanup().await;
}

/// The other half of the lifecycle, on the real migrated schema: an app DELETE
/// cascades to its artifacts, and 0067's trigger — not any Rust code — must
/// reclaim the blob. Guards the trigger against a future migration weakening it.
#[tokio::test]
async fn cascade_delete_reclaims_the_blob_via_the_trigger() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut c = db.conn().await;

    seed_blob(&mut c, "cascade-me", "1 hour").await;
    diesel::sql_query(
        "INSERT INTO symbol_artifacts (app_id, kind, platform, blob_sha256) \
         VALUES ($1, 'js_sourcemap', 'web', sha256($2::bytea))",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .bind::<Bytea, _>("cascade-me".as_bytes())
    .execute(&mut c)
    .await
    .unwrap();

    diesel::sql_query("DELETE FROM apps WHERE id = $1")
        .bind::<SqlUuid, _>(ids.app_id)
        .execute(&mut c)
        .await
        .unwrap();

    assert!(
        !blob_exists(&mut c, "cascade-me").await,
        "the cascade must reclaim the blob through 0067's trigger, with no sweep needed"
    );
    db.cleanup().await;
}
