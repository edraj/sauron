//! `repo::record_tiering_success` / `record_tiering_failure` — the record that
//! says whether a tiered table's tiering is still moving.
//!
//! The case that matters: a table that fails every cycle must show a rising
//! failure count and keep the full error, and a later success must reset the
//! count without erasing the error that explains the past stall.

mod common;

use chrono::{Duration, TimeZone, Utc};
use common::TestDb;
use sauron_db::repo;

#[tokio::test]
async fn failures_accumulate_and_a_success_resets_the_count_but_keeps_the_cause() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut conn = db.conn().await;
    let t0 = Utc.with_ymd_and_hms(2026, 10, 1, 13, 0, 0).unwrap();

    // A table that has never managed a single export still gets a row.
    let n = repo::record_tiering_failure(
        &mut conn,
        "error_events",
        t0,
        "exporting error_events_2026_08_15: failed to pin block",
    )
    .await
    .unwrap();
    assert_eq!(n, 1);
    let n =
        repo::record_tiering_failure(&mut conn, "error_events", t0 + Duration::hours(1), "second")
            .await
            .unwrap();
    assert_eq!(n, 2);

    let rows = repo::list_tiering_health(&mut conn).await.unwrap();
    let h = rows
        .iter()
        .find(|h| h.table_name == "error_events")
        .unwrap();
    assert_eq!(h.consecutive_failures, 2);
    assert_eq!(h.last_error.as_deref(), Some("second"));
    assert_eq!(h.last_error_at, Some(t0 + Duration::hours(1)));
    assert_eq!(h.last_success_at, None);

    let t1 = t0 + Duration::hours(2);
    repo::record_tiering_success(&mut conn, "error_events", t1)
        .await
        .unwrap();
    let rows = repo::list_tiering_health(&mut conn).await.unwrap();
    let h = rows
        .iter()
        .find(|h| h.table_name == "error_events")
        .unwrap();
    assert_eq!(h.consecutive_failures, 0);
    assert_eq!(h.last_success_at, Some(t1));
    assert_eq!(h.last_cycle_at, t1);
    assert_eq!(
        h.last_error.as_deref(),
        Some("second"),
        "the cause of the past stall survives the recovery"
    );
}

#[tokio::test]
async fn health_is_listed_by_table_name() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut conn = db.conn().await;
    let now = Utc::now();
    for t in ["transactions", "analytics_events", "error_events"] {
        repo::record_tiering_success(&mut conn, t, now)
            .await
            .unwrap();
    }
    let names: Vec<String> = repo::list_tiering_health(&mut conn)
        .await
        .unwrap()
        .into_iter()
        .map(|h| h.table_name)
        .collect();
    assert_eq!(names, ["analytics_events", "error_events", "transactions"]);
}
