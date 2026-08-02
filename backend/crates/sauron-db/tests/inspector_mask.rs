mod common;

use chrono::Utc;
use common::TestDb;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use sauron_db::models::{Issue, NewInspectorMaskAction, NewInspectorMaskedKey};
use sauron_db::repo;
use sauron_db::repo::BatchCursor;
use sauron_db::schema::issues;
use sauron_inspector::targets::{TargetColumn, TargetTable};
use serde_json::json;

async fn seed_action(db: &TestDb, kind: &str) -> (uuid::Uuid, uuid::Uuid, uuid::Uuid) {
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let targets = json!([{"table": "error_events", "column": "extra", "path": "customer.email"}]);
    let a = repo::insert_mask_action(
        &mut conn,
        NewInspectorMaskAction {
            org_id: ids.org_id,
            app_id: ids.app_id,
            kind,
            finding_id: None,
            scan_id: None,
            targets: &targets,
            requested_by: None,
            requested_by_email: "admin@example.com",
        },
    )
    .await
    .unwrap();
    (a.id, ids.app_id, ids.org_id)
}

/// Two independent claim slots. Routing previews through the mask FIFO means a
/// preview requested while a multi-hour mask runs expires before it is ever
/// computed, and confirm — which requires `previewed` — becomes permanently
/// impossible on a busy app.
#[tokio::test]
async fn preview_and_mask_claim_slots_are_independent() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let (preview_id, _app, _org) = seed_action(&db, "preview").await;
    let mut conn = db.conn().await;
    // A preview sits in status='preview' and is invisible to the mask slot.
    assert!(repo::claim_mask_action(&mut conn, "mask", "w1", 300)
        .await
        .unwrap()
        .is_none());
    let claimed = repo::claim_mask_action(&mut conn, "preview", "w1", 300)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.id, preview_id);
    assert_eq!(claimed.phase, "counting");
    db.cleanup().await;
}

#[tokio::test]
async fn confirm_requires_a_fresh_preview_and_a_ceiling() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let (id, _app, _org) = seed_action(&db, "preview").await;
    let mut conn = db.conn().await;
    repo::claim_mask_action(&mut conn, "preview", "w1", 300)
        .await
        .unwrap();
    repo::finish_preview(&mut conn, id, "w1", 1_000, 5, Some(Utc::now()))
        .await
        .unwrap();

    // A wrong ceiling refuses.
    assert_eq!(
        repo::confirm_mask_action(&mut conn, id, "ip=1.2.3.4 (xff)", 900, 100)
            .await
            .unwrap(),
        0
    );
    // A fresh preview under the ceiling promotes to `pending`.
    assert_eq!(
        repo::confirm_mask_action(&mut conn, id, "ip=1.2.3.4 (xff)", 900, 20_000_000)
            .await
            .unwrap(),
        1
    );
    let a = repo::get_mask_action(&mut conn, id).await.unwrap().unwrap();
    assert_eq!(a.status, "pending");
    assert_eq!(
        a.kind, "mask",
        "confirm flips kind so the mask slot can see it"
    );
    assert!(a.confirmed_at.is_some());
    // A second confirm is a no-op, so a double-click cannot enqueue twice.
    assert_eq!(
        repo::confirm_mask_action(&mut conn, id, "ip=1.2.3.4 (xff)", 900, 20_000_000)
            .await
            .unwrap(),
        0
    );
    db.cleanup().await;
}

/// The TTL is measured from `previewed_at` — the preview COMPLETING — not from
/// the request, or a queued preview expires before it is readable.
#[tokio::test]
async fn a_stale_preview_cannot_be_confirmed() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let (id, _app, _org) = seed_action(&db, "preview").await;
    let mut conn = db.conn().await;
    repo::claim_mask_action(&mut conn, "preview", "w1", 300)
        .await
        .unwrap();
    repo::finish_preview(&mut conn, id, "w1", 10, 0, Some(Utc::now()))
        .await
        .unwrap();
    diesel_async::RunQueryDsl::execute(
        diesel::sql_query("UPDATE inspector_mask_actions SET previewed_at = now() - interval '2 hours' WHERE id = $1")
            .bind::<diesel::sql_types::Uuid, _>(id),
        &mut conn,
    )
    .await
    .unwrap();
    assert_eq!(
        repo::confirm_mask_action(&mut conn, id, "ip=x", 900, 20_000_000)
            .await
            .unwrap(),
        0
    );
    db.cleanup().await;
}

/// Cancel is attributable. In an audit table whose whole justification is "who
/// did it", the one adversarial action the design permits — stopping a
/// redaction — must not be the one it cannot attribute.
#[tokio::test]
async fn cancel_records_who_and_only_from_a_live_state() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let (id, _app, _org) = seed_action(&db, "mask").await;
    let mut conn = db.conn().await;
    diesel_async::RunQueryDsl::execute(
        diesel::sql_query("UPDATE inspector_mask_actions SET status='running' WHERE id=$1")
            .bind::<diesel::sql_types::Uuid, _>(id),
        &mut conn,
    )
    .await
    .unwrap();
    assert_eq!(
        repo::cancel_mask_action(&mut conn, id, None, "ops@example.com")
            .await
            .unwrap(),
        1
    );
    let a = repo::get_mask_action(&mut conn, id).await.unwrap().unwrap();
    assert_eq!(a.status, "cancelling");
    assert_eq!(a.cancelled_by_email, "ops@example.com");
    assert!(a.cancelled_at.is_some());
    // A terminal action refuses: the handler answers 409.
    repo::finish_mask_action(&mut conn, id, "w1", "cancelled", true, Some(Utc::now()))
        .await
        .unwrap();
    assert_eq!(
        repo::cancel_mask_action(&mut conn, id, None, "ops@example.com")
            .await
            .unwrap(),
        0
    );
    db.cleanup().await;
}

#[tokio::test]
async fn masked_keys_are_idempotent_per_app_and_path() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let rows = vec![
        NewInspectorMaskedKey {
            app_id: ids.app_id,
            target_table: "error_events",
            target_column: "extra",
            json_path: "customer.email",
            created_by: None,
            source_action_id: None,
        },
        NewInspectorMaskedKey {
            app_id: ids.app_id,
            target_table: "error_events",
            target_column: "extra",
            json_path: "customer.email",
            created_by: None,
            source_action_id: None,
        },
    ];
    repo::insert_masked_keys(&mut conn, &rows).await.unwrap();
    repo::insert_masked_keys(&mut conn, &rows).await.unwrap();
    let loaded = repo::masked_keys_for_app(&mut conn, ids.app_id)
        .await
        .unwrap();
    assert_eq!(
        loaded.len(),
        1,
        "re-masking the same finding must be idempotent"
    );
    db.cleanup().await;
}

/// Forward enforcement alone leaves two gaps on `issues.title` — PII inside
/// `exception_type`, which `build_title` also concatenates, and the 30s cache
/// window — and both restore the raw string on the very next occurrence.
#[tokio::test]
async fn a_masked_issue_title_stays_masked_across_upserts() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let fp = format!("sticky-{}", uuid::Uuid::new_v4());
    let first = sauron_db::models::NewIssue {
        app_id: ids.app_id,
        fingerprint: &fp,
        type_: "error",
        title: "TypeError: jane@acme.com is not a function",
        culprit: "checkout",
        level: "error",
        first_seen: Utc::now(),
        last_seen: Utc::now(),
        times_seen: 1,
    };
    let issue_id = repo::upsert_issue(&mut conn, first).await.unwrap();
    diesel_async::RunQueryDsl::execute(
        diesel::sql_query("UPDATE issues SET title='****', culprit='****' WHERE id=$1")
            .bind::<diesel::sql_types::Uuid, _>(issue_id),
        &mut conn,
    )
    .await
    .unwrap();
    let again = sauron_db::models::NewIssue {
        app_id: ids.app_id,
        fingerprint: &fp,
        type_: "error",
        title: "TypeError: jane@acme.com is not a function",
        culprit: "checkout",
        level: "error",
        first_seen: Utc::now(),
        last_seen: Utc::now(),
        times_seen: 1,
    };
    repo::upsert_issue(&mut conn, again).await.unwrap();
    // `repo::get_issue_row` does not exist; the plan's own fallback is a direct
    // select of the row.
    let issue: Issue = issues::table
        .find(issue_id)
        .select(Issue::as_select())
        .first(&mut conn)
        .await
        .unwrap();
    assert_eq!(issue.title, "****", "the sticky guard must hold");
    assert_eq!(issue.culprit, "****");
    db.cleanup().await;
}

/// THE regression test. `EXPLAIN` the batch UPDATE and assert exactly one
/// `Update on error_events_<child>` node, not one per partition. Comparing
/// occurred_at to a CTE column instead of a bound scalar gives the planner no
/// pruning key and the whole cost model behind the throttle evaporates.
#[tokio::test]
async fn the_batch_update_prunes_to_one_child() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let day = Utc::now().date_naive();
    let plan = repo::explain_mask_batch_jsonb(
        &mut conn,
        TargetTable::ErrorEvents,
        TargetColumn::Extra,
        ids.app_id,
        day,
        &["customer".to_string(), "email".to_string()],
        BatchCursor::default(),
        10,
    )
    .await
    .unwrap();
    let update_nodes = plan.matches("Update on error_events").count();
    assert!(
        update_nodes <= 2,
        "expected pruning to one child, got {update_nodes} Update nodes:\n{plan}"
    );
    db.cleanup().await;
}

#[tokio::test]
async fn a_jsonb_batch_masks_only_matching_rows_and_advances_the_cursor() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let (action_id, app_id, _org) = seed_action(&db, "mask").await;
    let mut conn = db.conn().await;
    diesel_async::RunQueryDsl::execute(
        diesel::sql_query(
            "UPDATE inspector_mask_actions SET status='running', worker_id='w1' WHERE id=$1",
        )
        .bind::<diesel::sql_types::Uuid, _>(action_id),
        &mut conn,
    )
    .await
    .unwrap();
    // Seed two rows in today's partition: one carrying the path, one not.
    let now = Utc::now();
    for (i, extra) in [
        json!({"customer": {"email": "jane@acme.com"}}),
        json!({"other": 1}),
    ]
    .into_iter()
    .enumerate()
    {
        common::seed_error_event_with_extra(
            &mut conn,
            app_id,
            now - chrono::Duration::seconds(i as i64),
            &extra,
        )
        .await;
    }
    // The limit is 1, not 100, because `next_cursor` is `Some` only when the
    // batch came back FULL — that is the signal the day is unfinished. Only one
    // seeded row matches the path, so a limit above 1 returns a short batch and
    // the resumable-cursor assertion below could never hold.
    let out = repo::mask_batch_jsonb(
        &mut conn,
        TargetTable::ErrorEvents,
        TargetColumn::Extra,
        app_id,
        now.date_naive(),
        &["customer".to_string(), "email".to_string()],
        BatchCursor::default(),
        1,
        action_id,
        "w1",
    )
    .await
    .unwrap()
    .expect("the fence must hold");
    assert_eq!(out.rows_masked, 1);
    assert!(out.rows_scanned >= 1);
    assert!(
        out.next_cursor.is_some(),
        "a full-ish batch must leave a resumable cursor"
    );
    assert_eq!(out.status, "running");
    db.cleanup().await;
}

/// `jsonb_set` returns NULL if ANY argument is NULL, and a NULL written into a
/// NOT NULL DEFAULT '{}' column is the single most likely implementation bug
/// in this slice.
#[tokio::test]
async fn a_null_jsonb_column_is_never_written_as_sql_null() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let (action_id, app_id, _org) = seed_action(&db, "mask").await;
    let mut conn = db.conn().await;
    diesel_async::RunQueryDsl::execute(
        diesel::sql_query(
            "UPDATE inspector_mask_actions SET status='running', worker_id='w1' WHERE id=$1",
        )
        .bind::<diesel::sql_types::Uuid, _>(action_id),
        &mut conn,
    )
    .await
    .unwrap();
    let now = Utc::now();
    common::seed_error_event_with_extra(
        &mut conn,
        app_id,
        now,
        &json!({"customer": {"email": "a@b.c"}}),
    )
    .await;
    repo::mask_batch_jsonb(
        &mut conn,
        TargetTable::ErrorEvents,
        TargetColumn::Extra,
        app_id,
        now.date_naive(),
        &["customer".to_string(), "email".to_string()],
        BatchCursor::default(),
        100,
        action_id,
        "w1",
    )
    .await
    .unwrap();
    let nulls = repo::count_null_column(&mut conn, "error_events", "extra", app_id)
        .await
        .unwrap();
    assert_eq!(nulls, 0, "no row may have been written to SQL NULL");
    db.cleanup().await;
}

/// The tail sweep is keyed on `received_at`, not `occurred_at`, while KEEPING
/// an occurred_at range for pruning. `occurred_at` is the CLIENT's timestamp;
/// a mobile offline queue flushes events whose occurred_at is days old, and
/// those rows land in a partition the day loop already swept.
#[tokio::test]
async fn the_tail_sweep_filters_on_received_at() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let (action_id, app_id, _org) = seed_action(&db, "mask").await;
    let mut conn = db.conn().await;
    diesel_async::RunQueryDsl::execute(
        diesel::sql_query(
            "UPDATE inspector_mask_actions SET status='running', worker_id='w1' WHERE id=$1",
        )
        .bind::<diesel::sql_types::Uuid, _>(action_id),
        &mut conn,
    )
    .await
    .unwrap();
    let now = Utc::now();
    common::seed_error_event_with_extra(
        &mut conn,
        app_id,
        now,
        &json!({"customer": {"email": "a@b.c"}}),
    )
    .await;
    let out = repo::mask_tail_sweep_batch(
        &mut conn,
        TargetTable::ErrorEvents,
        TargetColumn::Extra,
        app_id,
        now - chrono::Duration::days(1),
        now + chrono::Duration::days(1),
        now + chrono::Duration::hours(1), // received_at floor in the future
        &["customer".to_string(), "email".to_string()],
        BatchCursor::default(),
        100,
        action_id,
        "w1",
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        out.rows_masked, 0,
        "a received_at floor in the future must match nothing"
    );
    db.cleanup().await;
}

/// `prune_mask_actions` defaults to 0 = NEVER prune. This table grows per
/// HUMAN ACTION, not per rule evaluation, and it is the record a compliance
/// question is answered from.
#[tokio::test]
async fn audit_retention_of_zero_deletes_nothing() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let (id, _app, _org) = seed_action(&db, "mask").await;
    let mut conn = db.conn().await;
    repo::finish_mask_action(&mut conn, id, "w1", "done", false, None)
        .await
        .unwrap();
    diesel_async::RunQueryDsl::execute(
        diesel::sql_query("UPDATE inspector_mask_actions SET requested_at = now() - interval '900 days' WHERE id=$1")
            .bind::<diesel::sql_types::Uuid, _>(id),
        &mut conn,
    )
    .await
    .unwrap();
    assert_eq!(
        repo::prune_mask_actions(&mut conn, 0, 1_000).await.unwrap(),
        0
    );
    assert!(repo::get_mask_action(&mut conn, id)
        .await
        .unwrap()
        .is_some());
    db.cleanup().await;
}

/// Without pseudonymization the privacy feature is the only UN-ERASABLE store
/// of staff PII in the schema: everywhere else a user row cascades, so
/// deleting a user is the product's de-facto erasure mechanism, and
/// `ON DELETE SET NULL` plus a denormalized email breaks it by design.
#[tokio::test]
async fn pseudonymization_keeps_counts_and_drops_identities() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let (id, _app, _org) = seed_action(&db, "mask").await;
    let mut conn = db.conn().await;
    repo::finish_mask_action(&mut conn, id, "w1", "done", false, None)
        .await
        .unwrap();
    diesel_async::RunQueryDsl::execute(
        diesel::sql_query(
            "UPDATE inspector_mask_actions \
             SET requested_at = now() - interval '900 days', rows_masked = 41200, \
                 confirm_source = 'ip=10.0.0.5 (untrusted-peer)' WHERE id=$1",
        )
        .bind::<diesel::sql_types::Uuid, _>(id),
        &mut conn,
    )
    .await
    .unwrap();
    assert_eq!(
        repo::pseudonymize_mask_actions(&mut conn, 730)
            .await
            .unwrap(),
        1
    );
    let a = repo::get_mask_action(&mut conn, id).await.unwrap().unwrap();
    assert_eq!(a.requested_by_email, "");
    assert_eq!(a.cancelled_by_email, "");
    assert_eq!(a.confirm_source, "");
    assert_eq!(a.rows_masked, 41_200, "counts and targets survive");
    assert!(!a.targets.as_array().unwrap().is_empty());
    db.cleanup().await;
}
