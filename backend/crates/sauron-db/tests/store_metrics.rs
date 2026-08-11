//! Storage-layer behaviour for Google Play / App Store install metrics.
//!
//! The two defects these pin, both of which produce output that still *looks*
//! plausible and so would survive every other gate:
//!
//!  * **Additive upsert.** Both stores restate recent days as their reporting
//!    pipelines settle, so the same day is fetched over and over. `+=` instead
//!    of `SET` multiplies every number by the number of syncs — a chart that
//!    rises smoothly and is entirely fiction.
//!  * **Secret clobbering.** `PUT` carries identifiers and an *optional*
//!    credential. If "field absent" is not distinguished from "field null",
//!    editing a package name silently wipes the service-account key, and the
//!    only symptom is a sync that starts failing hours later.
//!
//! Skips (does not fail) when `TEST_DATABASE_URL` is unset — the harness in
//! `common/` provisions a throwaway database per test.

mod common;

use chrono::NaiveDate;
use common::TestDb;
use sauron_db::repo;
use serde_json::json;

fn day(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).expect("valid date")
}

#[tokio::test]
async fn upsert_is_idempotent_not_additive() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let d = day(2026, 8, 1);

    repo::upsert_store_daily_metrics(&mut conn, ids.app_id, "google_play", &[(d, 100, 10)])
        .await
        .expect("first upsert");
    repo::upsert_store_daily_metrics(&mut conn, ids.app_id, "google_play", &[(d, 100, 10)])
        .await
        .expect("second upsert");

    let rows = repo::store_metrics_range(&mut conn, ids.app_id, d)
        .await
        .expect("read back");
    assert_eq!(rows.len(), 1, "one row per (app, store, day)");
    assert_eq!(
        rows[0].installs, 100,
        "re-syncing a day must SET, not ADD — 200 here means every tick inflates the chart"
    );
    assert_eq!(rows[0].uninstalls, 10);

    db.cleanup().await;
}

#[tokio::test]
async fn restated_day_overwrites_with_the_new_value() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let d = day(2026, 8, 1);

    repo::upsert_store_daily_metrics(&mut conn, ids.app_id, "app_store", &[(d, 100, 10)])
        .await
        .expect("initial");
    // Apple settles the number upward a day later.
    repo::upsert_store_daily_metrics(&mut conn, ids.app_id, "app_store", &[(d, 137, 12)])
        .await
        .expect("restatement");

    let rows = repo::store_metrics_range(&mut conn, ids.app_id, d)
        .await
        .expect("read back");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].installs, 137);
    assert_eq!(rows[0].uninstalls, 12);

    db.cleanup().await;
}

#[tokio::test]
async fn stores_are_kept_separate_for_the_same_day() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let d = day(2026, 8, 1);

    repo::upsert_store_daily_metrics(&mut conn, ids.app_id, "google_play", &[(d, 100, 10)])
        .await
        .expect("play");
    repo::upsert_store_daily_metrics(&mut conn, ids.app_id, "app_store", &[(d, 80, 5)])
        .await
        .expect("apple");

    let rows = repo::store_metrics_range(&mut conn, ids.app_id, d)
        .await
        .expect("read back");
    assert_eq!(
        rows.len(),
        2,
        "the PK includes `store`; one must not overwrite the other"
    );
    let play = rows
        .iter()
        .find(|r| r.store == "google_play")
        .expect("play row");
    let apple = rows
        .iter()
        .find(|r| r.store == "app_store")
        .expect("apple row");
    assert_eq!(play.installs, 100);
    assert_eq!(apple.installs, 80);

    db.cleanup().await;
}

#[tokio::test]
async fn range_read_excludes_days_before_the_window() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    repo::upsert_store_daily_metrics(
        &mut conn,
        ids.app_id,
        "google_play",
        &[(day(2026, 7, 1), 1, 0), (day(2026, 8, 1), 2, 0)],
    )
    .await
    .expect("seed two days");

    let rows = repo::store_metrics_range(&mut conn, ids.app_id, day(2026, 8, 1))
        .await
        .expect("read back");
    assert_eq!(rows.len(), 1, "the July day is outside the window");
    assert_eq!(rows[0].day, day(2026, 8, 1));

    db.cleanup().await;
}

#[tokio::test]
async fn secret_omitted_is_preserved_and_explicit_null_clears_it() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let google = json!({"package_name": "com.example.app", "gcs_bucket": "pubsite_prod_rev_1"});
    repo::upsert_store_connection(
        &mut conn,
        ids.app_id,
        "google_play",
        &google,
        Some(Some(b"sekrit".to_vec())),
    )
    .await
    .expect("create with secret");

    // `None` = the caller did not send the field: leave the credential alone.
    let renamed =
        json!({"package_name": "com.example.renamed", "gcs_bucket": "pubsite_prod_rev_1"});
    let row = repo::upsert_store_connection(&mut conn, ids.app_id, "google_play", &renamed, None)
        .await
        .expect("update without secret");
    assert_eq!(
        row.secret_enc.as_deref(),
        Some(&b"sekrit"[..]),
        "editing identifiers must not wipe the stored credential"
    );
    assert_eq!(row.identifiers["package_name"], "com.example.renamed");

    // `Some(None)` = the caller explicitly sent null: clear it.
    let row =
        repo::upsert_store_connection(&mut conn, ids.app_id, "google_play", &renamed, Some(None))
            .await
            .expect("clear secret");
    assert!(row.secret_enc.is_none(), "explicit null clears the secret");

    db.cleanup().await;
}

#[tokio::test]
async fn claim_pushes_next_sync_forward_so_a_peer_cannot_double_claim() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    repo::upsert_store_connection(
        &mut conn,
        ids.app_id,
        "google_play",
        &json!({"package_name": "com.example.app", "gcs_bucket": "b"}),
        None,
    )
    .await
    .expect("create connection");

    let first = repo::claim_due_store_connections(&mut conn, 10, 21_600)
        .await
        .expect("first claim");
    assert_eq!(first.len(), 1, "a brand-new connection is due immediately");

    let second = repo::claim_due_store_connections(&mut conn, 10, 21_600)
        .await
        .expect("second claim");
    assert!(
        second.is_empty(),
        "claiming must push next_sync_at past now(), or two daemons sync the same row"
    );

    db.cleanup().await;
}

#[tokio::test]
async fn queue_sync_makes_a_scheduled_connection_due_again() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    repo::upsert_store_connection(
        &mut conn,
        ids.app_id,
        "google_play",
        &json!({"package_name": "com.example.app", "gcs_bucket": "b"}),
        None,
    )
    .await
    .expect("create connection");
    repo::claim_due_store_connections(&mut conn, 10, 21_600)
        .await
        .expect("claim it away");

    repo::queue_store_sync(&mut conn, ids.app_id, "google_play")
        .await
        .expect("queue");

    let claimed = repo::claim_due_store_connections(&mut conn, 10, 21_600)
        .await
        .expect("re-claim");
    assert_eq!(claimed.len(), 1, "queueing must make the row due now");

    db.cleanup().await;
}

#[tokio::test]
async fn a_failed_sync_does_not_stamp_last_synced_at() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let row = repo::upsert_store_connection(
        &mut conn,
        ids.app_id,
        "google_play",
        &json!({"package_name": "com.example.app", "gcs_bucket": "b"}),
        None,
    )
    .await
    .expect("create connection");

    repo::record_store_sync_result(&mut conn, row.id, Some("bucket not found"))
        .await
        .expect("record failure");

    let after = repo::get_store_connection(&mut conn, ids.app_id, "google_play")
        .await
        .expect("read back")
        .expect("row exists");
    assert_eq!(after.last_error.as_deref(), Some("bucket not found"));
    assert!(
        after.last_synced_at.is_none(),
        "a permanently failing connection must not look freshly synced"
    );

    // A later success clears the error and stamps the time.
    repo::record_store_sync_result(&mut conn, row.id, None)
        .await
        .expect("record success");
    let after = repo::get_store_connection(&mut conn, ids.app_id, "google_play")
        .await
        .expect("read back")
        .expect("row exists");
    assert!(after.last_error.is_none(), "success clears the stale error");
    assert!(after.last_synced_at.is_some());

    db.cleanup().await;
}

#[tokio::test]
async fn deleting_a_connection_keeps_collected_metrics() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let d = day(2026, 8, 1);

    repo::upsert_store_connection(
        &mut conn,
        ids.app_id,
        "google_play",
        &json!({"package_name": "com.example.app", "gcs_bucket": "b"}),
        Some(Some(b"sekrit".to_vec())),
    )
    .await
    .expect("create connection");
    repo::upsert_store_daily_metrics(&mut conn, ids.app_id, "google_play", &[(d, 100, 10)])
        .await
        .expect("seed metrics");

    repo::delete_store_connection(&mut conn, ids.app_id, "google_play")
        .await
        .expect("delete");

    let rows = repo::store_metrics_range(&mut conn, ids.app_id, d)
        .await
        .expect("read back");
    assert_eq!(
        rows.len(),
        1,
        "history is not a credential; removing the key must not erase the data"
    );

    db.cleanup().await;
}
