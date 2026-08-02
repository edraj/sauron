mod common;

use chrono::{Duration, Utc};
use common::TestDb;
use sauron_db::models::{NewInspectorPolicy, NewInspectorScan};
use sauron_db::repo::{self, FindingDelta};
use serde_json::json;

async fn seed_scan(db: &TestDb) -> (uuid::Uuid, uuid::Uuid, uuid::Uuid) {
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let keys = json!(["email"]);
    let empty = json!([]);
    let policy = repo::create_inspector_policy(
        &mut conn,
        NewInspectorPolicy {
            org_id: ids.org_id,
            target_type: "app",
            target_id: ids.app_id,
            enabled: true,
            tracked_keys: &keys,
            detectors: &empty,
            scan_columns: None,
            rollups: &empty,
            window_days: 30,
            schedule_enabled: false,
            schedule_days: 0,
            schedule_time: chrono::NaiveTime::from_hms_opt(3, 0, 0).unwrap(),
            schedule_tz: "UTC",
            created_by: None,
        },
    )
    .await
    .unwrap();
    let params = json!({"tracked_keys": ["email"]});
    let targets = json!([[ids.app_id, ids.env_a]]);
    let scan = repo::insert_inspector_scan(
        &mut conn,
        NewInspectorScan {
            policy_id: policy.id,
            org_id: ids.org_id,
            trigger_type: "manual",
            requested_by: None,
            window_from: Utc::now() - Duration::days(30),
            window_to: Utc::now(),
            params: &params,
            targets: &targets,
            units_total: 2,
        },
    )
    .await
    .unwrap();
    (policy.id, scan.id, ids.app_id)
}

fn delta(app_id: uuid::Uuid, org_id: uuid::Uuid, path: &str, n: i64) -> FindingDelta {
    FindingDelta {
        org_id,
        app_id,
        environment_id: None,
        env_scope: "no_env_column".into(),
        source_table: "error_events".into(),
        source_column: "extra".into(),
        key_path: path.into(),
        matched_key: "email".into(),
        detector: String::new(),
        value_type: "string".into(),
        match_count: n,
        match_count_exact: true,
        sample_preview: "j…m".into(),
        sample_row_id: None,
        sample_occurred_at: None,
        partition_kind: "ranged".into(),
        first_seen_at: Some(Utc::now()),
        last_seen_at: Some(Utc::now()),
    }
}

/// The partial unique index is what makes "one active scan per policy" a
/// database invariant instead of a race between the API and the scheduler.
#[tokio::test]
async fn a_second_queued_scan_is_a_unique_violation() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let (policy_id, _scan_id, _) = seed_scan(&db).await;
    let mut conn = db.conn().await;
    let params = json!({});
    let targets = json!([]);
    let err = repo::insert_inspector_scan(
        &mut conn,
        NewInspectorScan {
            policy_id,
            org_id: uuid::Uuid::new_v4(),
            trigger_type: "manual",
            requested_by: None,
            window_from: Utc::now(),
            window_to: Utc::now(),
            params: &params,
            targets: &targets,
            units_total: 0,
        },
    )
    .await;
    assert!(matches!(
        err,
        Err(diesel::result::Error::DatabaseError(
            diesel::result::DatabaseErrorKind::UniqueViolation,
            _
        ))
    ));
    // The handler turns this into a 409 with the active scan id, never a 500.
    assert!(repo::active_scan_for_policy(&mut conn, policy_id)
        .await
        .unwrap()
        .is_some());
    db.cleanup().await;
}

#[tokio::test]
async fn a_claim_is_exclusive_and_stamps_the_worker() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let (_p, scan_id, _) = seed_scan(&db).await;
    let mut conn = db.conn().await;
    let first = repo::claim_one_scan(&mut conn, "w1", 120).await.unwrap();
    assert_eq!(first.as_ref().map(|s| s.id), Some(scan_id));
    assert_eq!(first.unwrap().attempts, 1);
    let second = repo::claim_one_scan(&mut conn, "w2", 120).await.unwrap();
    assert!(
        second.is_none(),
        "a running, heartbeating scan must not be re-claimable"
    );
    db.cleanup().await;
}

#[tokio::test]
async fn two_flushes_accumulate_and_advance_the_cursor() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let (_p, scan_id, app_id) = seed_scan(&db).await;
    let mut conn = db.conn().await;
    let claimed = repo::claim_one_scan(&mut conn, "w1", 120)
        .await
        .unwrap()
        .unwrap();
    let org_id = claimed.org_id;
    let d = vec![delta(app_id, org_id, "customer.email", 10)];
    let out = repo::flush_scan_unit(&mut conn, scan_id, "w1", &json!({"unit": 1}), 1, 100, &d)
        .await
        .unwrap()
        .expect("fence must hold");
    assert_eq!(out.new_findings, 1);
    let out = repo::flush_scan_unit(&mut conn, scan_id, "w1", &json!({"unit": 2}), 2, 100, &d)
        .await
        .unwrap()
        .expect("fence must hold");
    assert_eq!(
        out.new_findings, 0,
        "the second flush updates, it does not insert"
    );
    let s = repo::get_inspector_scan(&mut conn, scan_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(s.rows_scanned, 200);
    assert_eq!(s.units_done, 2);
    assert_eq!(s.cursor, json!({"unit": 2}));
    let f = repo::list_findings_for_scan(&mut conn, scan_id, 100, None)
        .await
        .unwrap();
    assert_eq!(f.len(), 1);
    assert_eq!(
        f[0].match_count, 20,
        "counts must SUM across units, not GREATEST"
    );
    db.cleanup().await;
}

/// The assertion that catches the snapshot bug: a subquery counting
/// inspector_findings inside the same data-modifying WITH sees the table as of
/// BEFORE the insert, so the counter is permanently one flush behind and a
/// single-unit scan reports 0 while GET /findings returns rows.
#[tokio::test]
async fn findings_count_equals_the_row_count_after_one_unit() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let (_p, scan_id, app_id) = seed_scan(&db).await;
    let mut conn = db.conn().await;
    let claimed = repo::claim_one_scan(&mut conn, "w1", 120)
        .await
        .unwrap()
        .unwrap();
    let d = vec![
        delta(app_id, claimed.org_id, "a.email", 3),
        delta(app_id, claimed.org_id, "b.email", 4),
    ];
    repo::flush_scan_unit(&mut conn, scan_id, "w1", &json!({"unit": 1}), 1, 7, &d)
        .await
        .unwrap()
        .unwrap();
    let s = repo::get_inspector_scan(&mut conn, scan_id)
        .await
        .unwrap()
        .unwrap();
    let rows = repo::list_findings_for_scan(&mut conn, scan_id, 100, None)
        .await
        .unwrap();
    assert_eq!(s.findings_count as usize, rows.len());
    db.cleanup().await;
}

/// A worker stalled past its lease can have its scan reclaimed while still
/// alive. Without the fence, `match_count + excluded.match_count` double-counts
/// silently. A flush that affects zero rows MUST abort the unit.
#[tokio::test]
async fn a_stale_worker_id_affects_nothing_and_does_not_move_the_cursor() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let (_p, scan_id, app_id) = seed_scan(&db).await;
    let mut conn = db.conn().await;
    let claimed = repo::claim_one_scan(&mut conn, "w1", 120)
        .await
        .unwrap()
        .unwrap();
    let d = vec![delta(app_id, claimed.org_id, "a.email", 5)];
    repo::flush_scan_unit(&mut conn, scan_id, "w1", &json!({"unit": 1}), 1, 5, &d)
        .await
        .unwrap()
        .unwrap();
    let ghost = repo::flush_scan_unit(
        &mut conn,
        scan_id,
        "zombie",
        &json!({"unit": 99}),
        99,
        5,
        &d,
    )
    .await
    .unwrap();
    assert!(ghost.is_none(), "a fenced-out flush must return None");
    let s = repo::get_inspector_scan(&mut conn, scan_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(s.cursor, json!({"unit": 1}));
    assert_eq!(s.rows_scanned, 5);
    let f = repo::list_findings_for_scan(&mut conn, scan_id, 100, None)
        .await
        .unwrap();
    assert_eq!(
        f[0].match_count, 5,
        "the zombie must not have added its delta"
    );
    db.cleanup().await;
}

#[tokio::test]
async fn cancellation_surfaces_on_the_next_flush() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let (_p, scan_id, app_id) = seed_scan(&db).await;
    let mut conn = db.conn().await;
    let claimed = repo::claim_one_scan(&mut conn, "w1", 120)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        repo::request_scan_cancel(&mut conn, scan_id).await.unwrap(),
        1
    );
    let d = vec![delta(app_id, claimed.org_id, "a.email", 1)];
    let out = repo::flush_scan_unit(&mut conn, scan_id, "w1", &json!({"unit": 1}), 1, 1, &d)
        .await
        .unwrap()
        .unwrap();
    assert!(out.cancel_requested_at.is_some());
    repo::finish_scan(
        &mut conn,
        scan_id,
        "w1",
        "cancelled",
        "partial",
        "stopped by operator",
        "",
    )
    .await
    .unwrap();
    let s = repo::get_inspector_scan(&mut conn, scan_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(s.status, "cancelled");
    assert!(s.finished_at.is_some());
    db.cleanup().await;
}

#[tokio::test]
async fn a_scan_whose_heartbeat_expired_is_reclaimable() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let (_p, scan_id, _) = seed_scan(&db).await;
    let mut conn = db.conn().await;
    repo::claim_one_scan(&mut conn, "w1", 120)
        .await
        .unwrap()
        .unwrap();
    diesel_async::RunQueryDsl::execute(
        diesel::sql_query(
            "UPDATE inspector_scans SET heartbeat_at = now() - interval '10 minutes' WHERE id = $1",
        )
        .bind::<diesel::sql_types::Uuid, _>(scan_id),
        &mut conn,
    )
    .await
    .unwrap();
    let again = repo::claim_one_scan(&mut conn, "w2", 120)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(again.worker_id.as_deref(), Some("w2"));
    assert_eq!(again.attempts, 2);
    db.cleanup().await;
}

/// The `app_id` predicate on the reveal read is NOT redundant. Without it the
/// tenant decision rests entirely on `inspector_findings.app_id` being correct
/// — a worker-written value with no constraint tying it to the row
/// `sample_row_id` points at — so any attribution bug converts silently into
/// cross-tenant raw-PII disclosure. It costs nothing: `app_id` leads
/// `error_events_app_env_time_users_idx`.
#[tokio::test]
async fn reveal_returns_none_for_a_mismatched_app_id() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let row: Option<(uuid::Uuid, chrono::DateTime<Utc>)> =
        repo::first_error_event_locator(&mut conn, ids.app_id)
            .await
            .unwrap();
    let (event_id, occurred_at) = row.expect("the harness seeds error events");

    let hit = repo::reveal_one_value(
        &mut conn,
        "error_events",
        "extra",
        event_id,
        Some(occurred_at),
        ids.app_id,
    )
    .await
    .unwrap();
    assert!(hit.is_some(), "the real locator must resolve");

    let miss = repo::reveal_one_value(
        &mut conn,
        "error_events",
        "extra",
        event_id,
        Some(occurred_at),
        uuid::Uuid::new_v4(),
    )
    .await
    .unwrap();
    assert!(
        miss.is_none(),
        "a mismatched app_id must be a benign miss, not a disclosure"
    );

    // A dropped partition or a replaced rollup row is the same shape: 410.
    let gone = repo::reveal_one_value(
        &mut conn,
        "error_events",
        "extra",
        uuid::Uuid::new_v4(),
        Some(occurred_at),
        ids.app_id,
    )
    .await
    .unwrap();
    assert!(gone.is_none());
    db.cleanup().await;
}

#[tokio::test]
async fn findings_list_is_ordered_by_match_count_and_keysets() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let (_p, scan_id, app_id) = seed_scan(&db).await;
    let mut conn = db.conn().await;
    let claimed = repo::claim_one_scan(&mut conn, "w1", 120)
        .await
        .unwrap()
        .unwrap();
    let d = vec![
        delta(app_id, claimed.org_id, "a.email", 1),
        delta(app_id, claimed.org_id, "b.email", 50),
        delta(app_id, claimed.org_id, "c.email", 20),
    ];
    repo::flush_scan_unit(&mut conn, scan_id, "w1", &json!({"unit": 1}), 1, 71, &d)
        .await
        .unwrap()
        .unwrap();
    let page = repo::list_findings_for_scan(&mut conn, scan_id, 2, None)
        .await
        .unwrap();
    assert_eq!(page.len(), 2);
    assert_eq!(page[0].match_count, 50);
    assert_eq!(page[1].match_count, 20);
    let next = repo::list_findings_for_scan(&mut conn, scan_id, 2, Some((20, page[1].id)))
        .await
        .unwrap();
    assert_eq!(next.len(), 1);
    assert_eq!(next[0].match_count, 1);
    assert_eq!(
        repo::count_findings_for_scan(&mut conn, scan_id)
            .await
            .unwrap(),
        3
    );
    db.cleanup().await;
}
