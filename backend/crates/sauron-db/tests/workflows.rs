//! Workflow grouping: Task 3's `bump_workflow` (the per-signal roll-up
//! upsert) and `apply_workflow_lifecycle` (the `$workflow_start`/`_end`/
//! `_cancel` status transition), plus Task 4's read-side aggregation
//! (`workflow_list`/`workflow_detail`/`workflow_runs`/
//! `workflow_spans_for_session`), against a real Postgres database.
//!
//! Does NOT re-test the `Workflow` struct's field order against `schema.rs`
//! — `workflow_row_round_trips_in_declared_column_order` in `env_scoping.rs`
//! already guards that (Task 1). Every assertion here reads the row back via
//! `Workflow::as_select()` (named columns), which would mask a pure ordering
//! bug — that is by design: this file is about the upsert *logic*, not the
//! column mapping.

mod common;

use chrono::{Duration, Timelike, Utc};
use common::TestDb;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use sauron_db::models::{NewAnalyticsEvent, NewErrorEvent, NewIssue, Workflow};
use sauron_db::repo::{self, WorkflowAction, DURATION_BUCKETS};
use sauron_db::schema::workflows;
use sauron_db::scope::{EnvFilter, ReadScope};
use serde_json::json;
use uuid::Uuid;

/// Pinned to today's date at a fixed mid-day time, mirroring every other
/// harness fixture in this crate's test suite (`seed_two_envs`'s own `now`,
/// `workflow_row_round_trips_in_declared_column_order`'s `now`) — keeps every
/// timestamp comparison in this file exact (`==`) rather than only a relative
/// ordering, and stays far from a UTC day boundary regardless of wall-clock
/// time.
fn pinned_now() -> chrono::DateTime<Utc> {
    Utc::now()
        .date_naive()
        .and_hms_opt(12, 0, 0)
        .expect("12:00:00 is a valid time")
        .and_utc()
}

/// Fetch the single `workflows` row for `(app_id, workflow_id)`. Panics if
/// there isn't exactly one — every test in this file expects a single row
/// (that is the whole point of the `(app_id, workflow_id)` upsert key), so a
/// duplicate or a miss is itself a failure worth surfacing loudly rather than
/// silently taking `.first()`'s arbitrary pick.
async fn only_workflow_row(
    conn: &mut sauron_db::PgConn,
    app_id: Uuid,
    workflow_id: &str,
) -> Workflow {
    let mut rows: Vec<Workflow> = workflows::table
        .filter(workflows::app_id.eq(app_id))
        .filter(workflows::workflow_id.eq(workflow_id))
        .select(Workflow::as_select())
        .load(conn)
        .await
        .expect("select workflows row");
    assert_eq!(
        rows.len(),
        1,
        "expected exactly one workflows row for workflow_id={workflow_id}, got {}",
        rows.len()
    );
    rows.remove(0)
}

#[tokio::test]
async fn bump_workflow_inserts_then_accumulates() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let t0 = pinned_now();
    let suffix = Uuid::new_v4().simple().to_string();
    let wf_id = format!("wf-accumulate-{suffix}");

    repo::bump_workflow(
        &mut conn,
        ids.app_id,
        ids.env_a,
        &wf_id,
        "checkout",
        Some("wf-session-1"),
        Some("wf-user-1"),
        Some("wf-device-1"),
        Some("1.0.0"),
        t0,
        1,
        0,
    )
    .await
    .expect("bump_workflow #1 (events_delta=1, errors_delta=0)");

    repo::bump_workflow(
        &mut conn,
        ids.app_id,
        ids.env_a,
        &wf_id,
        "checkout",
        Some("wf-session-1"),
        Some("wf-user-1"),
        Some("wf-device-1"),
        Some("1.0.0"),
        t0 + Duration::minutes(5),
        0,
        1,
    )
    .await
    .expect("bump_workflow #2 (events_delta=0, errors_delta=1)");

    let row = only_workflow_row(&mut conn, ids.app_id, &wf_id).await;
    assert_eq!(row.events_count, 1, "events_count");
    assert_eq!(row.errors_count, 1, "errors_count");
    assert_eq!(row.started_at, t0, "started_at");
    assert_eq!(
        row.last_event_at,
        t0 + Duration::minutes(5),
        "last_event_at"
    );
    assert_eq!(row.status, "active", "status");

    drop(conn);
    db.cleanup().await;
}

#[tokio::test]
async fn bump_workflow_takes_earliest_start_and_latest_activity() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let t0 = pinned_now();
    let suffix = Uuid::new_v4().simple().to_string();
    let wf_id = format!("wf-out-of-order-{suffix}");

    // Arrival order is deliberately reversed: the LATER signal (t0 + 5min)
    // lands first, then the EARLIER one (t0) arrives after — the shape a
    // network reorder or a retried request produces in practice.
    repo::bump_workflow(
        &mut conn,
        ids.app_id,
        ids.env_a,
        &wf_id,
        "checkout",
        None,
        None,
        None,
        None,
        t0 + Duration::minutes(5),
        1,
        0,
    )
    .await
    .expect("bump_workflow at t0+5min (arrives first)");

    repo::bump_workflow(
        &mut conn, ids.app_id, ids.env_a, &wf_id, "checkout", None, None, None, None, t0, 1, 0,
    )
    .await
    .expect("bump_workflow at t0 (arrives second, out of order)");

    let row = only_workflow_row(&mut conn, ids.app_id, &wf_id).await;
    assert_eq!(
        row.started_at, t0,
        "started_at must be the EARLIEST of the two"
    );
    assert_eq!(
        row.last_event_at,
        t0 + Duration::minutes(5),
        "last_event_at must be the LATEST of the two"
    );

    drop(conn);
    db.cleanup().await;
}

#[tokio::test]
async fn lifecycle_end_marks_completed_and_sets_ended_at() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let t0 = pinned_now();
    let suffix = Uuid::new_v4().simple().to_string();
    let wf_id = format!("wf-end-{suffix}");

    repo::apply_workflow_lifecycle(
        &mut conn,
        ids.app_id,
        ids.env_a,
        &wf_id,
        "checkout",
        WorkflowAction::Start,
        None,
        Some("wf-session-2"),
        Some("wf-user-2"),
        t0,
    )
    .await
    .expect("lifecycle Start");

    repo::apply_workflow_lifecycle(
        &mut conn,
        ids.app_id,
        ids.env_a,
        &wf_id,
        "checkout",
        WorkflowAction::End,
        None,
        Some("wf-session-2"),
        Some("wf-user-2"),
        t0 + Duration::minutes(2),
    )
    .await
    .expect("lifecycle End");

    let row = only_workflow_row(&mut conn, ids.app_id, &wf_id).await;
    assert_eq!(row.status, "completed", "status");
    assert_eq!(row.ended_at, Some(t0 + Duration::minutes(2)), "ended_at");

    drop(conn);
    db.cleanup().await;
}

#[tokio::test]
async fn lifecycle_cancel_records_reason() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let t0 = pinned_now();
    let suffix = Uuid::new_v4().simple().to_string();
    let wf_id = format!("wf-cancel-{suffix}");

    repo::apply_workflow_lifecycle(
        &mut conn,
        ids.app_id,
        ids.env_a,
        &wf_id,
        "checkout",
        WorkflowAction::Start,
        None,
        Some("wf-session-3"),
        Some("wf-user-3"),
        t0,
    )
    .await
    .expect("lifecycle Start");

    repo::apply_workflow_lifecycle(
        &mut conn,
        ids.app_id,
        ids.env_a,
        &wf_id,
        "checkout",
        WorkflowAction::Cancel,
        Some("superseded"),
        Some("wf-session-3"),
        Some("wf-user-3"),
        t0 + Duration::minutes(1),
    )
    .await
    .expect("lifecycle Cancel");

    let row = only_workflow_row(&mut conn, ids.app_id, &wf_id).await;
    assert_eq!(row.status, "cancelled", "status");
    assert_eq!(
        row.cancel_reason.as_deref(),
        Some("superseded"),
        "cancel_reason"
    );

    drop(conn);
    db.cleanup().await;
}

/// The brief's "first terminal transition wins; a second one is ignored" rule,
/// asserted in BOTH directions — the case the other tests leave uncovered.
///
/// Without this, deleting the `CASE WHEN workflows.status = 'active'` guard
/// from `cancel_reason` alone would break nothing in the suite:
/// `terminal_status_is_not_reverted_by_a_late_bump_or_late_start` only ever
/// follows a terminal transition with a `Start` and a bump (neither of which
/// touches `cancel_reason`), so an End→Cancel sequence writing "changed my
/// mind" onto a `completed` workflow would pass unnoticed. Both orderings are
/// checked because the two transitions are not symmetric in the SQL: `End`
/// binds `cancel_reason = NULL` while `Cancel` binds a real string, so a
/// missing guard shows up as a *different* wrong value in each direction.
#[tokio::test]
async fn a_second_terminal_transition_is_ignored_in_either_direction() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let t0 = pinned_now();
    let suffix = Uuid::new_v4().simple().to_string();

    // --- Direction 1: End, then a late Cancel. -----------------------------
    // The Cancel must not flip `status` to 'cancelled', must not move
    // `ended_at` to its own later timestamp, and — the assertion that the
    // `cancel_reason` guard specifically exists for — must not write its
    // reason onto a workflow that actually completed successfully.
    let ended_wf = format!("wf-end-then-cancel-{suffix}");
    repo::apply_workflow_lifecycle(
        &mut conn,
        ids.app_id,
        ids.env_a,
        &ended_wf,
        "checkout",
        WorkflowAction::End,
        None,
        None,
        None,
        t0 + Duration::minutes(2),
    )
    .await
    .expect("End (first terminal transition)");

    repo::apply_workflow_lifecycle(
        &mut conn,
        ids.app_id,
        ids.env_a,
        &ended_wf,
        "checkout",
        WorkflowAction::Cancel,
        Some("changed my mind"),
        None,
        None,
        t0 + Duration::minutes(9),
    )
    .await
    .expect("Cancel (second terminal transition, must be ignored)");

    let row = only_workflow_row(&mut conn, ids.app_id, &ended_wf).await;
    assert_eq!(
        row.status, "completed",
        "End must win over the later Cancel"
    );
    assert_eq!(
        row.ended_at,
        Some(t0 + Duration::minutes(2)),
        "ended_at must stay at the End's timestamp, not the Cancel's"
    );
    assert_eq!(
        row.cancel_reason, None,
        "a completed workflow must never acquire a cancel_reason — a dashboard \
         rendering \"Cancelled: {{reason}}\" off a non-null cancel_reason would \
         report a cancellation that never happened"
    );

    // --- Direction 2: Cancel, then a late End. -----------------------------
    // The End must not flip `status` back to 'completed', and must not clear
    // the real cancellation reason (it binds `cancel_reason = NULL`, so an
    // unguarded clause would blank it rather than overwrite it).
    let cancelled_wf = format!("wf-cancel-then-end-{suffix}");
    repo::apply_workflow_lifecycle(
        &mut conn,
        ids.app_id,
        ids.env_a,
        &cancelled_wf,
        "checkout",
        WorkflowAction::Cancel,
        Some("user abandoned"),
        None,
        None,
        t0 + Duration::minutes(3),
    )
    .await
    .expect("Cancel (first terminal transition)");

    repo::apply_workflow_lifecycle(
        &mut conn,
        ids.app_id,
        ids.env_a,
        &cancelled_wf,
        "checkout",
        WorkflowAction::End,
        None,
        None,
        None,
        t0 + Duration::minutes(8),
    )
    .await
    .expect("End (second terminal transition, must be ignored)");

    let row = only_workflow_row(&mut conn, ids.app_id, &cancelled_wf).await;
    assert_eq!(
        row.status, "cancelled",
        "Cancel must win over the later End"
    );
    assert_eq!(
        row.ended_at,
        Some(t0 + Duration::minutes(3)),
        "ended_at must stay at the Cancel's timestamp, not the End's"
    );
    assert_eq!(
        row.cancel_reason.as_deref(),
        Some("user abandoned"),
        "the later End binds cancel_reason = NULL; an unguarded clause would \
         blank the real reason"
    );

    drop(conn);
    db.cleanup().await;
}

/// A `$workflow_start` that resolves no name (neither stamped nor in
/// `properties`) must not destroy a display name an earlier signal already
/// established — the `COALESCE(NULLIF(EXCLUDED.name, ''), workflows.name)`
/// clause in `apply_workflow_lifecycle`'s Start statement.
///
/// This is the hand-rolled-client case the property fallback exists for, so it
/// is not hypothetical: `workflows.name` is `TEXT NOT NULL` with no emptiness
/// CHECK, and the caller resolves an absent name to `""`, so a bare
/// `name = EXCLUDED.name` would accept it and `bump_workflow` would never
/// repair it.
#[tokio::test]
async fn an_empty_name_never_clobbers_an_established_one() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let t0 = pinned_now();
    let suffix = Uuid::new_v4().simple().to_string();
    let wf_id = format!("wf-name-{suffix}");

    // A stamped SDK event establishes the good name.
    repo::bump_workflow(
        &mut conn, ids.app_id, ids.env_a, &wf_id, "checkout", None, None, None, None, t0, 1, 0,
    )
    .await
    .expect("bump_workflow establishes name");

    // A nameless lifecycle event then lands on the same workflow.
    repo::apply_workflow_lifecycle(
        &mut conn,
        ids.app_id,
        ids.env_a,
        &wf_id,
        "",
        WorkflowAction::Start,
        None,
        None,
        None,
        t0 + Duration::minutes(1),
    )
    .await
    .expect("nameless lifecycle Start");

    let row = only_workflow_row(&mut conn, ids.app_id, &wf_id).await;
    assert_eq!(
        row.name, "checkout",
        "an empty name must be treated as 'nothing to offer', not as an \
         instruction to blank the column"
    );

    // The reverse repair: a workflow first created nameless by a lifecycle
    // event gets its name upgraded by the next named signal, via
    // `bump_workflow`'s own `COALESCE(NULLIF(workflows.name, ''), …)`.
    let nameless_first = format!("wf-nameless-first-{suffix}");
    repo::apply_workflow_lifecycle(
        &mut conn,
        ids.app_id,
        ids.env_a,
        &nameless_first,
        "",
        WorkflowAction::Start,
        None,
        None,
        None,
        t0,
    )
    .await
    .expect("nameless lifecycle Start creates the row");

    let row = only_workflow_row(&mut conn, ids.app_id, &nameless_first).await;
    assert_eq!(row.name, "", "nothing better was available at creation");

    repo::bump_workflow(
        &mut conn,
        ids.app_id,
        ids.env_a,
        &nameless_first,
        "checkout",
        None,
        None,
        None,
        None,
        t0 + Duration::minutes(1),
        1,
        0,
    )
    .await
    .expect("a later named signal upgrades the empty name");

    let row = only_workflow_row(&mut conn, ids.app_id, &nameless_first).await;
    assert_eq!(row.name, "checkout", "an empty name must be upgradeable");

    drop(conn);
    db.cleanup().await;
}

#[tokio::test]
async fn terminal_status_is_not_reverted_by_a_late_bump_or_late_start() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let t0 = pinned_now();
    let suffix = Uuid::new_v4().simple().to_string();
    let wf_id = format!("wf-terminal-{suffix}");

    repo::apply_workflow_lifecycle(
        &mut conn,
        ids.app_id,
        ids.env_a,
        &wf_id,
        "checkout",
        WorkflowAction::Start,
        None,
        Some("wf-session-4"),
        Some("wf-user-4"),
        t0,
    )
    .await
    .expect("lifecycle Start");

    repo::apply_workflow_lifecycle(
        &mut conn,
        ids.app_id,
        ids.env_a,
        &wf_id,
        "checkout",
        WorkflowAction::End,
        None,
        Some("wf-session-4"),
        Some("wf-user-4"),
        t0 + Duration::minutes(2),
    )
    .await
    .expect("lifecycle End");

    let ended_at_after_end = only_workflow_row(&mut conn, ids.app_id, &wf_id)
        .await
        .ended_at;

    // A late-arriving ordinary bump (e.g. a straggler event stamped with this
    // workflow_id, delivered after the end) must only add to the counters —
    // never touch status/ended_at.
    repo::bump_workflow(
        &mut conn,
        ids.app_id,
        ids.env_a,
        &wf_id,
        "checkout",
        Some("wf-session-4"),
        Some("wf-user-4"),
        None,
        None,
        t0 + Duration::minutes(3),
        1,
        0,
    )
    .await
    .expect("late bump_workflow after End");

    // A late-arriving `$workflow_start` (out-of-order delivery) must not
    // reopen an already-terminal workflow either.
    repo::apply_workflow_lifecycle(
        &mut conn,
        ids.app_id,
        ids.env_a,
        &wf_id,
        "checkout",
        WorkflowAction::Start,
        None,
        Some("wf-session-4"),
        Some("wf-user-4"),
        t0 + Duration::minutes(4),
    )
    .await
    .expect("late lifecycle Start after End");

    let row = only_workflow_row(&mut conn, ids.app_id, &wf_id).await;
    assert_eq!(row.status, "completed", "status must stay completed");
    assert_eq!(
        row.ended_at, ended_at_after_end,
        "ended_at must be unchanged by the late bump/late start"
    );
    assert_eq!(
        row.events_count, 1,
        "events_count must still be incremented by the late bump"
    );

    drop(conn);
    db.cleanup().await;
}

// ===========================================================================
// Task 4: read-side aggregation (`workflow_list`/`workflow_detail`/
// `workflow_runs`/`workflow_spans_for_session`).
// ===========================================================================

/// `t` with its sub-second component zeroed.
///
/// Task 4's reads derive abandonment from the server's own `now()`, so unlike
/// the Task 3 tests above they cannot use [`pinned_now`]'s fixed noon anchor —
/// a workflow pinned to noon would read as abandoned or not depending on what
/// time of day the suite runs. They need a real `Utc::now()` base. But
/// `Utc::now()` carries nanoseconds, while `timestamptz` stores microseconds,
/// so a raw `Utc::now()` does not necessarily survive the round-trip
/// unchanged — which would make an exact `last_seen == now` assertion flaky
/// rather than wrong. Truncating to whole seconds keeps the timestamps
/// `now()`-relative (so staleness still behaves) while making them exactly
/// representable, so `==` is safe.
fn pinned_to_second(t: chrono::DateTime<Utc>) -> chrono::DateTime<Utc> {
    t.with_nanosecond(0).expect("0ns is always valid")
}

/// Insert one `analytics_events` row stamped with a workflow — for
/// `workflow_detail`'s `top_events` assertions. Deliberately bare-bones (no
/// `event_users`/`devices` registration): none of Task 4's queries touch
/// either table, unlike `common::seed_analytics_event`.
async fn seed_workflow_analytics_event(
    conn: &mut sauron_db::PgConn,
    app_id: Uuid,
    env: Uuid,
    name: &str,
    workflow_id: &str,
    workflow_name: &str,
    occurred_at: chrono::DateTime<Utc>,
) {
    repo::insert_analytics_event(
        conn,
        NewAnalyticsEvent {
            id: Uuid::new_v4(),
            app_id,
            environment_id: Some(env),
            name: name.to_string(),
            distinct_id: "wf-detail-user".to_string(),
            properties: json!({}),
            context: json!({}),
            session_id: None,
            release: None,
            ip_address: None,
            occurred_at,
            device_key: None,
            screen: None,
            workflow_id: Some(workflow_id.to_string()),
            workflow_name: Some(workflow_name.to_string()),
            tags: json!({}),
            contexts: json!({}),
            extra: json!({}),
        },
    )
    .await
    .expect("insert workflow-stamped analytics event");
}

/// Insert one `error_events` row stamped with a workflow and pointed at
/// `issue_id` — for `workflow_detail`'s `top_issues` assertions.
async fn seed_workflow_error_event(
    conn: &mut sauron_db::PgConn,
    app_id: Uuid,
    env: Uuid,
    issue_id: Uuid,
    workflow_id: &str,
    workflow_name: &str,
    occurred_at: chrono::DateTime<Utc>,
) {
    repo::insert_error_event(
        conn,
        NewErrorEvent {
            id: Uuid::new_v4(),
            app_id,
            environment_id: Some(env),
            issue_id,
            fingerprint: "wf-detail-fingerprint".to_string(),
            level: "error".into(),
            message: format!("workflow harness error {}", Uuid::new_v4().simple()),
            exception_type: "HarnessError".into(),
            exception_value: "seeded".into(),
            stacktrace: json!([]),
            breadcrumbs: json!([]),
            context: json!({}),
            tags: json!({}),
            release: None,
            distinct_id: Some("wf-detail-user".to_string()),
            event_user: None,
            sdk: None,
            ip_address: None,
            occurred_at,
            session_id: None,
            device_key: None,
            screen: None,
            workflow_id: Some(workflow_id.to_string()),
            workflow_name: Some(workflow_name.to_string()),
            stacktrace_symbolicated: None,
            symbolication_status: "not_applicable".into(),
            debug_meta: None,
            contexts: json!({}),
            extra: json!({}),
            handled: Some(true),
            title: None,
            culprit: None,
        },
    )
    .await
    .expect("insert workflow-stamped error event");
}

/// The core abandonment-derivation contract: `eff` reads `'abandoned'` for a
/// row that is still `status='active'` but has had no activity for longer
/// than `WORKFLOW_STALE_MINUTES` (30) — never stored, computed on read.
#[tokio::test]
async fn workflow_list_derives_abandoned_from_staleness() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let now = pinned_to_second(Utc::now());
    let suffix = Uuid::new_v4().simple().to_string();

    // A: completed, ended 1 minute after start -> dur = 60_000ms.
    let wf_a = format!("wf-list-a-{suffix}");
    repo::apply_workflow_lifecycle(
        &mut conn,
        ids.app_id,
        ids.env_a,
        &wf_a,
        "checkout",
        WorkflowAction::Start,
        None,
        None,
        Some("wf-list-user-1"),
        now - Duration::minutes(20),
    )
    .await
    .expect("A: Start");
    repo::apply_workflow_lifecycle(
        &mut conn,
        ids.app_id,
        ids.env_a,
        &wf_a,
        "checkout",
        WorkflowAction::End,
        None,
        None,
        Some("wf-list-user-1"),
        now - Duration::minutes(19),
    )
    .await
    .expect("A: End");

    // B: cancelled, ended 2 minutes after start -> dur = 120_000ms.
    // Deliberately a DIFFERENT duration from A: with two unequal finished
    // runs, median (90_000) and p95 (117_000) are distinct numbers, so a
    // median/p95 column swap is detectable. Two equal durations would make
    // the two percentiles identical and the assertion vacuous.
    let wf_b = format!("wf-list-b-{suffix}");
    repo::apply_workflow_lifecycle(
        &mut conn,
        ids.app_id,
        ids.env_a,
        &wf_b,
        "checkout",
        WorkflowAction::Start,
        None,
        None,
        Some("wf-list-user-1"),
        now - Duration::minutes(15),
    )
    .await
    .expect("B: Start");
    repo::apply_workflow_lifecycle(
        &mut conn,
        ids.app_id,
        ids.env_a,
        &wf_b,
        "checkout",
        WorkflowAction::Cancel,
        Some("user abandoned"),
        None,
        Some("wf-list-user-1"),
        now - Duration::minutes(13),
    )
    .await
    .expect("B: Cancel");

    // C: active, last activity just now. Second distinct user, so
    // unique_users == 2 across the four runs (A/B share user-1).
    let wf_c = format!("wf-list-c-{suffix}");
    repo::bump_workflow(
        &mut conn,
        ids.app_id,
        ids.env_a,
        &wf_c,
        "checkout",
        None,
        Some("wf-list-user-2"),
        None,
        None,
        now,
        1,
        0,
    )
    .await
    .expect("C: bump_workflow");

    // D: active but stale — last activity 45 minutes ago, past
    // WORKFLOW_STALE_MINUTES (30), so it must read as abandoned.
    let wf_d = format!("wf-list-d-{suffix}");
    repo::bump_workflow(
        &mut conn,
        ids.app_id,
        ids.env_a,
        &wf_d,
        "checkout",
        None,
        Some("wf-list-user-2"),
        None,
        None,
        now - Duration::minutes(45),
        1,
        0,
    )
    .await
    .expect("D: bump_workflow");

    let rows = repo::workflow_list(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        1,
        None,
        50,
        0,
    )
    .await
    .expect("workflow_list");

    assert_eq!(rows.len(), 1, "expected exactly one name-grouped row");
    let row = &rows[0];
    assert_eq!(row.name, "checkout");
    assert_eq!(row.started, 4, "started");
    assert_eq!(row.completed, 1, "completed (A)");
    assert_eq!(row.cancelled, 1, "cancelled (B)");
    assert_eq!(row.active, 1, "active (C)");
    assert_eq!(
        row.abandoned, 1,
        "abandoned (D) — status='active' but last_event_at is 45min old, \
         past WORKFLOW_STALE_MINUTES (30)"
    );

    // --- the aggregate VALUES, not just "the query ran" --------------------
    // Everything below would still pass a query that merely returns rows; it
    // is the column mapping and the aggregate expressions that these pin
    // down. `median`/`p95` in particular are two adjacent `Nullable<Double>`
    // columns in `WorkflowRow` — a swap between them is invisible to the type
    // system and to every count assertion above.
    assert_eq!(
        row.unique_users, 2,
        "unique_users: A/B share wf-list-user-1, C/D share wf-list-user-2"
    );
    // Finished runs only: A = 60_000ms, B = 120_000ms. `percentile_cont`
    // interpolates over [60000, 120000]: 0.5 -> 90_000, 0.95 -> 117_000.
    // The active/abandoned runs (C/D) have no `ended_at`, so they contribute
    // no `dur` and are correctly ignored rather than counted as zero.
    assert_eq!(
        row.median_duration_ms,
        Some(90_000.0),
        "median over the two FINISHED runs [60s, 120s] — if C/D's NULL \
         durations were being counted as 0 this would be 60_000"
    );
    assert_eq!(
        row.p95_duration_ms,
        Some(117_000.0),
        "p95 over the same two runs — deliberately different from the median \
         so a median/p95 column swap fails here"
    );
    assert_eq!(
        row.last_seen, now,
        "last_seen = MAX(last_event_at) = C's bump at `now`"
    );

    drop(conn);
    db.cleanup().await;
}

/// `workflow_list` scoped by `EnvFilter::One` must see only that
/// environment's rows — asymmetric counts (2 vs. 3) across `env_a`/`env_b`,
/// same rationale as `SeedIds`' own doc comment: identical tuples would let a
/// swapped or ignored env bind pass silently. `EnvFilter::All` is asserted to
/// equal the sum, the same property every other env-scoped read in this
/// codebase has.
#[tokio::test]
async fn workflow_list_is_environment_scoped() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let now = Utc::now();
    let suffix = Uuid::new_v4().simple().to_string();

    for i in 0..2 {
        let wf_id = format!("wf-scope-a-{i}-{suffix}");
        repo::bump_workflow(
            &mut conn, ids.app_id, ids.env_a, &wf_id, "checkout", None, None, None, None, now, 1, 0,
        )
        .await
        .unwrap_or_else(|e| panic!("env_a workflow {i}: {e}"));
    }
    for i in 0..3 {
        let wf_id = format!("wf-scope-b-{i}-{suffix}");
        repo::bump_workflow(
            &mut conn, ids.app_id, ids.env_b, &wf_id, "checkout", None, None, None, None, now, 1, 0,
        )
        .await
        .unwrap_or_else(|e| panic!("env_b workflow {i}: {e}"));
    }

    let rows_a = repo::workflow_list(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        1,
        None,
        50,
        0,
    )
    .await
    .expect("workflow_list env_a");
    assert_eq!(rows_a.len(), 1);
    assert_eq!(rows_a[0].started, 2, "env_a must see only its own 2 rows");

    let rows_b = repo::workflow_list(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
        1,
        None,
        50,
        0,
    )
    .await
    .expect("workflow_list env_b");
    assert_eq!(rows_b.len(), 1);
    assert_eq!(rows_b[0].started, 3, "env_b must see only its own 3 rows");

    let rows_all = repo::workflow_list(&mut conn, ReadScope::all(ids.app_id), 1, None, 50, 0)
        .await
        .expect("workflow_list All");
    assert_eq!(rows_all.len(), 1);
    assert_eq!(
        rows_all[0].started, 5,
        "All must equal the sum of the two environments (2 + 3)"
    );

    drop(conn);
    db.cleanup().await;
}

/// `search` substring-filters by name via a bound ILIKE pattern.
///
/// Deliberately uses `ReadScope::all` (rather than `EnvFilter::One`, as every
/// other test in this file does) — `workflow_list`'s env fragment reserves a
/// bind index only under `One`/`Subset`; `All` reserves none, shifting
/// `search`/`limit`/`offset` down by one index each. Paired with
/// `workflow_list_is_environment_scoped` (which exercises `One`), both
/// branches of that shift are covered by this file.
#[tokio::test]
async fn workflow_list_search_filters_by_name_substring() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let now = Utc::now();
    let suffix = Uuid::new_v4().simple().to_string();

    repo::bump_workflow(
        &mut conn,
        ids.app_id,
        ids.env_a,
        &format!("wf-search-checkout-{suffix}"),
        "checkout",
        None,
        None,
        None,
        None,
        now,
        1,
        0,
    )
    .await
    .expect("seed checkout");
    repo::bump_workflow(
        &mut conn,
        ids.app_id,
        ids.env_a,
        &format!("wf-search-onboarding-{suffix}"),
        "onboarding",
        None,
        None,
        None,
        None,
        now,
        1,
        0,
    )
    .await
    .expect("seed onboarding");

    // A third name containing a literal LIKE metacharacter, for the
    // escaping assertions below.
    repo::bump_workflow(
        &mut conn,
        ids.app_id,
        ids.env_a,
        &format!("wf-search-underscore-{suffix}"),
        "sign_up",
        None,
        None,
        None,
        None,
        now,
        1,
        0,
    )
    .await
    .expect("seed sign_up");

    let rows = repo::workflow_list(
        &mut conn,
        ReadScope::all(ids.app_id),
        1,
        Some("check"),
        50,
        0,
    )
    .await
    .expect("workflow_list search=check");
    assert_eq!(rows.len(), 1, "only 'checkout' should match 'check'");
    assert_eq!(rows[0].name, "checkout");

    let all_rows = repo::workflow_list(&mut conn, ReadScope::all(ids.app_id), 1, None, 50, 0)
        .await
        .expect("workflow_list search=None");
    assert_eq!(all_rows.len(), 3, "no search filter must return every name");

    // --- LIKE metacharacters are matched literally -------------------------
    // `search` routes through `like_contains`, so a user typing `%` or `_`
    // gets those characters, not wildcards. Without the escaping these two
    // assertions read 3 and 2 respectively.
    let pct = repo::workflow_list(&mut conn, ReadScope::all(ids.app_id), 1, Some("%"), 50, 0)
        .await
        .expect("workflow_list search=%");
    assert!(
        pct.is_empty(),
        "a literal '%' matches no seeded name — unescaped it would be a \
         wildcard matching all three, which is how a search box turns into a \
         'show everything' button: {pct:?}"
    );

    let underscore = repo::workflow_list(
        &mut conn,
        ReadScope::all(ids.app_id),
        1,
        Some("sign_up"),
        50,
        0,
    )
    .await
    .expect("workflow_list search=sign_up");
    assert_eq!(
        underscore.len(),
        1,
        "'sign_up' matches the literal name 'sign_up'"
    );
    assert_eq!(underscore[0].name, "sign_up");

    let underscore_is_not_wildcard = repo::workflow_list(
        &mut conn,
        ReadScope::all(ids.app_id),
        1,
        Some("sign_u"),
        50,
        0,
    )
    .await
    .expect("workflow_list search=sign_u");
    assert_eq!(
        underscore_is_not_wildcard.len(),
        1,
        "sanity: the prefix still matches as a substring"
    );

    drop(conn);
    db.cleanup().await;
}

/// `workflow_runs` filters by the effective status (comparing against the
/// abandonment-aware projection, not the raw column — `abandoned` never
/// appears as a stored `status` value) and paginates newest-first with no
/// overlap between pages.
#[tokio::test]
async fn workflow_runs_filters_by_status_and_paginates() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let now = pinned_to_second(Utc::now());
    let suffix = Uuid::new_v4().simple().to_string();

    // Four runs of the same name, distinct `started_at` so `ORDER BY
    // started_at DESC` gives a stable, assertable sequence: active (now,
    // newest) > cancelled (now-10m) > completed (now-20m) > abandoned
    // (now-45m, oldest).
    //
    // The abandoned run carries a session/distinct id and deliberately
    // UNEQUAL event/error deltas (3 and 2) — the four `WorkflowRun`
    // passthrough columns are otherwise only exercised for "the query ran",
    // and equal counters would hide a swap between the two `Integer` columns.
    let wf_abandoned = format!("wf-runs-abandoned-{suffix}");
    repo::bump_workflow(
        &mut conn,
        ids.app_id,
        ids.env_a,
        &wf_abandoned,
        "checkout",
        Some("wf-runs-session-1"),
        Some("wf-runs-user-1"),
        None,
        None,
        now - Duration::minutes(45),
        3,
        2,
    )
    .await
    .expect("seed abandoned run");

    let wf_completed = format!("wf-runs-completed-{suffix}");
    repo::apply_workflow_lifecycle(
        &mut conn,
        ids.app_id,
        ids.env_a,
        &wf_completed,
        "checkout",
        WorkflowAction::Start,
        None,
        None,
        None,
        now - Duration::minutes(20),
    )
    .await
    .expect("completed run: Start");
    repo::apply_workflow_lifecycle(
        &mut conn,
        ids.app_id,
        ids.env_a,
        &wf_completed,
        "checkout",
        WorkflowAction::End,
        None,
        None,
        None,
        now - Duration::minutes(18),
    )
    .await
    .expect("completed run: End");

    let wf_cancelled = format!("wf-runs-cancelled-{suffix}");
    repo::apply_workflow_lifecycle(
        &mut conn,
        ids.app_id,
        ids.env_a,
        &wf_cancelled,
        "checkout",
        WorkflowAction::Start,
        None,
        None,
        None,
        now - Duration::minutes(10),
    )
    .await
    .expect("cancelled run: Start");
    repo::apply_workflow_lifecycle(
        &mut conn,
        ids.app_id,
        ids.env_a,
        &wf_cancelled,
        "checkout",
        WorkflowAction::Cancel,
        Some("user abandoned"),
        None,
        None,
        now - Duration::minutes(9),
    )
    .await
    .expect("cancelled run: Cancel");

    let wf_active = format!("wf-runs-active-{suffix}");
    repo::bump_workflow(
        &mut conn, ids.app_id, ids.env_a, &wf_active, "checkout", None, None, None, None, now, 1, 0,
    )
    .await
    .expect("seed active run");

    // A fifth run of the SAME name in env_b, with a `started_at` newer than
    // every env_a run — so a missing env fragment would not merely add a row
    // somewhere in the tail, it would take the FIRST slot of page 1 and shift
    // every pagination assertion below. Deliberately the newest, rather than
    // the oldest, so the leak cannot hide past the `LIMIT`.
    let wf_env_b = format!("wf-runs-envb-{suffix}");
    repo::bump_workflow(
        &mut conn,
        ids.app_id,
        ids.env_b,
        &wf_env_b,
        "checkout",
        None,
        None,
        None,
        None,
        now + Duration::minutes(1),
        1,
        0,
    )
    .await
    .expect("seed env_b run");

    let scope = || ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a));

    let abandoned_runs =
        repo::workflow_runs(&mut conn, scope(), "checkout", 1, Some("abandoned"), 50, 0)
            .await
            .expect("workflow_runs status=abandoned");
    assert_eq!(abandoned_runs.len(), 1);
    let abandoned = &abandoned_runs[0];
    assert_eq!(abandoned.workflow_id, wf_abandoned);
    assert_eq!(abandoned.status, "abandoned");
    // The passthrough columns — asserted for value and mapping, not just
    // presence. `session_id`/`distinct_id` are adjacent `Nullable<Text>`
    // columns and `events_count`/`errors_count` adjacent `Integer` ones, so
    // each pair is swappable without any type error.
    assert_eq!(
        abandoned.session_id.as_deref(),
        Some("wf-runs-session-1"),
        "session_id (the column the dashboard links to session detail with)"
    );
    assert_eq!(
        abandoned.distinct_id.as_deref(),
        Some("wf-runs-user-1"),
        "distinct_id must not be the session_id"
    );
    assert_eq!(abandoned.events_count, 3, "events_count");
    assert_eq!(abandoned.errors_count, 2, "errors_count");
    assert_eq!(
        abandoned.started_at,
        now - Duration::minutes(45),
        "started_at"
    );
    assert_eq!(
        abandoned.ended_at, None,
        "an abandoned run is still status='active' underneath — it never \
         received a terminal transition, so it has no ended_at"
    );
    assert_eq!(
        abandoned.duration_ms, None,
        "no ended_at means no duration, rather than a zero or a now()-based one"
    );

    let completed_runs =
        repo::workflow_runs(&mut conn, scope(), "checkout", 1, Some("completed"), 50, 0)
            .await
            .expect("workflow_runs status=completed");
    assert_eq!(completed_runs.len(), 1);
    assert_eq!(completed_runs[0].workflow_id, wf_completed);
    assert_eq!(
        completed_runs[0].duration_ms,
        Some(120_000),
        "20min start to 18min-ago end = 2 minutes = 120,000ms"
    );

    // Pagination: newest-first, page 1 then page 2, no overlap — a shifted
    // `limit`/`offset` bind would show up here as a wrong or duplicated
    // ordering, not just a wrong count.
    let page1 = repo::workflow_runs(&mut conn, scope(), "checkout", 1, None, 2, 0)
        .await
        .expect("workflow_runs page 1");
    let page2 = repo::workflow_runs(&mut conn, scope(), "checkout", 1, None, 2, 2)
        .await
        .expect("workflow_runs page 2");
    assert_eq!(
        page1
            .iter()
            .map(|r| r.workflow_id.as_str())
            .collect::<Vec<_>>(),
        vec![wf_active.as_str(), wf_cancelled.as_str()],
        "page 1: newest two, newest first"
    );
    assert_eq!(
        page2
            .iter()
            .map(|r| r.workflow_id.as_str())
            .collect::<Vec<_>>(),
        vec![wf_completed.as_str(), wf_abandoned.as_str()],
        "page 2: oldest two, newest-of-the-rest first"
    );

    // --- environment isolation -------------------------------------------
    // The env_b run is the newest of all five, so it would head page 1 if the
    // env fragment were dropped — the two pagination assertions above already
    // fail in that case, but assert it directly too, so the *reason* is named
    // rather than read as an off-by-one in the paging.
    let unpaged = repo::workflow_runs(&mut conn, scope(), "checkout", 1, None, 50, 0)
        .await
        .expect("workflow_runs env_a, unpaged");
    assert_eq!(unpaged.len(), 4, "env_a has exactly its own four runs");
    assert!(
        !unpaged.iter().any(|r| r.workflow_id == wf_env_b),
        "an env_b run of the same name must never appear under One(env_a)"
    );

    let runs_b = repo::workflow_runs(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
        "checkout",
        1,
        None,
        50,
        0,
    )
    .await
    .expect("workflow_runs env_b");
    assert_eq!(
        runs_b
            .iter()
            .map(|r| r.workflow_id.as_str())
            .collect::<Vec<_>>(),
        vec![wf_env_b.as_str()],
        "env_b sees its own run and only its own — the positive half, without \
         which the assertion above would pass on an unseeded fixture"
    );

    drop(conn);
    db.cleanup().await;
}

/// `workflow_detail`'s `top_events` excludes the three reserved lifecycle
/// events (`NOT LIKE '$workflow%'`) while keeping every real contained event
/// name, and `top_issues` aggregates contained `error_events` by their shared
/// `issue_id`.
#[tokio::test]
async fn workflow_detail_counts_contained_events_and_issues() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let now = Utc::now();
    let suffix = Uuid::new_v4().simple().to_string();
    let wf_id = format!("wf-detail-{suffix}");

    repo::bump_workflow(
        &mut conn,
        ids.app_id,
        ids.env_a,
        &wf_id,
        "checkout",
        None,
        None,
        None,
        None,
        now - Duration::minutes(5),
        1,
        0,
    )
    .await
    .expect("seed workflow row");

    // Three real contained events, plus one reserved lifecycle event
    // ($workflow_start) that `top_events` must exclude — without that
    // exclusion the reserved event would dominate every workflow's
    // contained-event list.
    for name in [
        "add_to_cart",
        "view_item",
        "apply_coupon",
        "$workflow_start",
    ] {
        seed_workflow_analytics_event(
            &mut conn,
            ids.app_id,
            ids.env_a,
            name,
            &wf_id,
            "checkout",
            now - Duration::minutes(4),
        )
        .await;
    }

    let issue_id = repo::upsert_issue(
        &mut conn,
        NewIssue {
            app_id: ids.app_id,
            fingerprint: "wf-detail-fingerprint",
            type_: "Error",
            title: "TypeError: workflow detail test",
            culprit: "checkout (detail.ts)",
            level: "error",
            first_seen: now - Duration::minutes(4),
            last_seen: now - Duration::minutes(3),
            times_seen: 2,
        },
    )
    .await
    .expect("upsert issue");

    for _ in 0..2 {
        seed_workflow_error_event(
            &mut conn,
            ids.app_id,
            ids.env_a,
            issue_id,
            &wf_id,
            "checkout",
            now - Duration::minutes(3),
        )
        .await;
    }

    // --- an env_b "checkout" that must be invisible under One(env_a) -------
    //
    // `workflow_detail` builds FOUR independent prepared statements, each
    // with its own `sql_fragment_for` + `bind_env!` pair (outcome aggregate,
    // duration histogram, top_events, top_issues) — and two of them use
    // different alias conventions (`top_events` is unqualified, `top_issues`
    // aliases `e`). Dropping or mis-aliasing the fragment in any ONE of them
    // leaks that statement's worth of another environment's data while the
    // other three stay correct, so a test that only checks `started` cannot
    // see it. This env_b fixture is built so that each of the four statements
    // has its own failing assertion below:
    //
    //   1. outcome aggregate -> `started` would read 2, not 1
    //   2. duration histogram -> a bucket would be non-zero (env_a's run
    //      never ends, so env_a's histogram is legitimately all-zero)
    //   3. top_events        -> `env_b_only_event` would appear
    //   4. top_issues        -> the env_b issue's title would appear
    let wf_id_b = format!("wf-detail-envb-{suffix}");
    repo::apply_workflow_lifecycle(
        &mut conn,
        ids.app_id,
        ids.env_b,
        &wf_id_b,
        "checkout",
        WorkflowAction::Start,
        None,
        None,
        None,
        now - Duration::minutes(5),
    )
    .await
    .expect("env_b checkout: Start");
    repo::apply_workflow_lifecycle(
        &mut conn,
        ids.app_id,
        ids.env_b,
        &wf_id_b,
        "checkout",
        WorkflowAction::End,
        None,
        None,
        None,
        // 2 minutes -> the "1-5m" bucket, so a leak is a *specific* wrong
        // bucket rather than merely a non-zero total.
        now - Duration::minutes(3),
    )
    .await
    .expect("env_b checkout: End");

    seed_workflow_analytics_event(
        &mut conn,
        ids.app_id,
        ids.env_b,
        "env_b_only_event",
        &wf_id_b,
        "checkout",
        now - Duration::minutes(4),
    )
    .await;

    let issue_id_b = repo::upsert_issue(
        &mut conn,
        NewIssue {
            app_id: ids.app_id,
            fingerprint: "wf-detail-fingerprint-envb",
            type_: "Error",
            title: "TypeError: env_b only, must never surface under env_a",
            culprit: "checkout (envb.ts)",
            level: "error",
            first_seen: now - Duration::minutes(4),
            last_seen: now - Duration::minutes(3),
            times_seen: 1,
        },
    )
    .await
    .expect("upsert env_b issue");
    seed_workflow_error_event(
        &mut conn,
        ids.app_id,
        ids.env_b,
        issue_id_b,
        &wf_id_b,
        "checkout",
        now - Duration::minutes(3),
    )
    .await;

    let detail = repo::workflow_detail(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        "checkout",
        1,
    )
    .await
    .expect("workflow_detail");

    assert_eq!(detail.name, "checkout");
    assert_eq!(
        detail.started, 1,
        "statement 1 (outcome aggregate): env_b's 'checkout' run must not be counted"
    );
    assert_eq!(
        detail.completed, 0,
        "env_a's run never ended; env_b's did — a leak here reads 1"
    );
    assert!(
        detail.duration_buckets.iter().all(|b| b.count == 0),
        "statement 2 (duration histogram): env_a has no finished run, so every \
         bucket must be zero — env_b's 2-minute run leaking in would put a 1 in \
         '1-5m': {:?}",
        detail.duration_buckets
    );

    let mut event_names: Vec<&str> = detail.top_events.iter().map(|e| e.name.as_str()).collect();
    event_names.sort();
    assert_eq!(
        event_names,
        vec!["add_to_cart", "apply_coupon", "view_item"],
        "statement 3 (top_events): exactly the 3 non-reserved env_a event names — \
         'env_b_only_event' appearing here is an environment leak, not a counting bug"
    );
    assert!(
        !detail
            .top_events
            .iter()
            .any(|e| e.name.starts_with("$workflow")),
        "the reserved $workflow_start event must never appear in top_events"
    );

    assert_eq!(
        detail.top_issues.len(),
        1,
        "statement 4 (top_issues): exactly one contained issue — env_b's issue \
         must not surface: {:?}",
        detail
            .top_issues
            .iter()
            .map(|i| i.title.as_str())
            .collect::<Vec<_>>()
    );
    assert_eq!(detail.top_issues[0].issue_id, issue_id);
    assert_eq!(detail.top_issues[0].count, 2);
    assert_eq!(
        detail.top_issues[0].title,
        "TypeError: workflow detail test"
    );

    // The positive half of the same boundary: env_b really does hold all the
    // things env_a just proved it cannot see. Without this, every assertion
    // above would also pass if the env_b fixture had silently failed to seed.
    let detail_b = repo::workflow_detail(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
        "checkout",
        1,
    )
    .await
    .expect("workflow_detail env_b");
    assert_eq!(detail_b.started, 1, "env_b sees its own run");
    assert_eq!(detail_b.completed, 1, "env_b's run did end");
    assert_eq!(
        detail_b
            .top_events
            .iter()
            .map(|e| e.name.as_str())
            .collect::<Vec<_>>(),
        vec!["env_b_only_event"],
        "env_b sees its own contained event"
    );
    assert_eq!(
        detail_b.top_issues.len(),
        1,
        "env_b sees its own contained issue"
    );
    assert_eq!(detail_b.top_issues[0].issue_id, issue_id_b);

    drop(conn);
    db.cleanup().await;
}

/// `workflow_detail`'s duration histogram must land FINISHED runs in the
/// right labelled buckets.
///
/// Its own test rather than a few extra lines on
/// `workflow_detail_counts_contained_events_and_issues`, because that test's
/// workflow never ends: with no `ended_at` anywhere the bucket query returns
/// zero rows and `order_histogram` zero-fills, so the histogram there is
/// all-zeros whether the bucketing SQL is right, wrong, or absent. Nothing
/// about the labels is actually exercised until at least one run finishes —
/// and a typo'd label would be invisible, since `order_histogram` matches by
/// string equality and silently 0-fills what it cannot match rather than
/// erroring. (`duration_bucket_case_emits_exactly_the_declared_labels` in
/// `repo.rs` guards the same coupling statically; this is the end-to-end
/// half, through a real Postgres `CASE`.)
#[tokio::test]
async fn workflow_detail_buckets_finished_runs_by_duration() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let now = pinned_to_second(Utc::now());
    let suffix = Uuid::new_v4().simple().to_string();

    // Three finished runs in three DIFFERENT buckets — 5s ("<10s"), 2min
    // ("1-5m"), 10min ("5-30m") — plus one unfinished run that must not be
    // bucketed at all. Different buckets rather than three in one: a CASE
    // whose arms are mis-ordered or whose thresholds are wrong would still
    // put three identical durations in one (possibly wrong) bucket and could
    // pass a weaker assertion.
    for (i, (start_offset_min, dur)) in [
        (30, Duration::seconds(5)),
        (25, Duration::minutes(2)),
        (20, Duration::minutes(10)),
    ]
    .iter()
    .enumerate()
    {
        let wf = format!("wf-bucket-{i}-{suffix}");
        let started = now - Duration::minutes(*start_offset_min);
        repo::apply_workflow_lifecycle(
            &mut conn,
            ids.app_id,
            ids.env_a,
            &wf,
            "checkout",
            WorkflowAction::Start,
            None,
            None,
            None,
            started,
        )
        .await
        .unwrap_or_else(|e| panic!("bucket run {i}: Start: {e}"));
        repo::apply_workflow_lifecycle(
            &mut conn,
            ids.app_id,
            ids.env_a,
            &wf,
            "checkout",
            WorkflowAction::End,
            None,
            None,
            None,
            started + *dur,
        )
        .await
        .unwrap_or_else(|e| panic!("bucket run {i}: End: {e}"));
    }

    // Still active — no `ended_at`, so it must contribute to `started` but
    // to no bucket.
    repo::bump_workflow(
        &mut conn,
        ids.app_id,
        ids.env_a,
        &format!("wf-bucket-unfinished-{suffix}"),
        "checkout",
        None,
        None,
        None,
        None,
        now,
        1,
        0,
    )
    .await
    .expect("unfinished run");

    let detail = repo::workflow_detail(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        "checkout",
        1,
    )
    .await
    .expect("workflow_detail");

    assert_eq!(detail.started, 4, "3 finished + 1 still active");
    assert_eq!(detail.completed, 3);

    // The histogram is always all five labels, in `DURATION_BUCKETS` order,
    // zero-filled — asserting the whole vector (not just the non-zero
    // entries) is what catches a label the SQL emits but Rust doesn't know.
    let got: Vec<(&str, i64)> = detail
        .duration_buckets
        .iter()
        .map(|b| (b.bucket.as_str(), b.count))
        .collect();
    assert_eq!(
        got,
        vec![
            ("<10s", 1),
            ("10-60s", 0),
            ("1-5m", 1),
            ("5-30m", 1),
            ("30m+", 0),
        ],
        "5s -> <10s, 2min -> 1-5m, 10min -> 5-30m; the unfinished run is in \
         no bucket. A label the SQL emits but DURATION_BUCKETS lacks would \
         show up here as an unexpected zero."
    );
    assert_eq!(
        detail.duration_buckets.len(),
        DURATION_BUCKETS.len(),
        "every declared bucket is always present, zero-filled"
    );

    drop(conn);
    db.cleanup().await;
}

/// `workflow_spans_for_session` returns every span in one session ordered by
/// `started_at ASC` — deliberately seeded out of insertion order, so a query
/// with no `ORDER BY` (or the wrong direction) would fail this. A fourth
/// workflow in a *different* session proves `session_id` is actually filtered
/// on, not just returned wholesale.
#[tokio::test]
async fn workflow_spans_for_session_returns_ordered_spans() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let now = Utc::now();
    let suffix = Uuid::new_v4().simple().to_string();
    let session_id = format!("wf-span-session-{suffix}");

    let wf_2 = format!("wf-span-2-{suffix}");
    repo::apply_workflow_lifecycle(
        &mut conn,
        ids.app_id,
        ids.env_a,
        &wf_2,
        "step2",
        WorkflowAction::Start,
        None,
        Some(&session_id),
        None,
        now - Duration::minutes(5),
    )
    .await
    .expect("span 2: Start");

    let wf_1 = format!("wf-span-1-{suffix}");
    repo::apply_workflow_lifecycle(
        &mut conn,
        ids.app_id,
        ids.env_a,
        &wf_1,
        "step1",
        WorkflowAction::Start,
        None,
        Some(&session_id),
        None,
        now - Duration::minutes(10),
    )
    .await
    .expect("span 1: Start");
    repo::apply_workflow_lifecycle(
        &mut conn,
        ids.app_id,
        ids.env_a,
        &wf_1,
        "step1",
        WorkflowAction::End,
        None,
        Some(&session_id),
        None,
        now - Duration::minutes(9),
    )
    .await
    .expect("span 1: End");

    let wf_3 = format!("wf-span-3-{suffix}");
    repo::apply_workflow_lifecycle(
        &mut conn,
        ids.app_id,
        ids.env_a,
        &wf_3,
        "step3",
        WorkflowAction::Start,
        None,
        Some(&session_id),
        None,
        now,
    )
    .await
    .expect("span 3: Start");

    // Same app+env, a DIFFERENT session — must never appear in this
    // session's spans.
    let wf_other_session = format!("wf-span-other-{suffix}");
    repo::bump_workflow(
        &mut conn,
        ids.app_id,
        ids.env_a,
        &wf_other_session,
        "unrelated",
        Some("some-other-session"),
        None,
        None,
        None,
        now,
        1,
        0,
    )
    .await
    .expect("other-session workflow");

    // Same app, same SESSION, different environment. A session id is a client
    // string, not a scoped key, so nothing but the env fragment keeps this out
    // of an env_a-scoped read — and a session that genuinely spans
    // environments is not exotic (see `bump_session`'s doc comment on
    // `sessions.environment_id` being the *latest* writer's). Placed between
    // wf_1 and wf_2 by `started_at`, so a leak also corrupts the ORDER the
    // assertion below pins, not just the length.
    let wf_env_b = format!("wf-span-envb-{suffix}");
    repo::apply_workflow_lifecycle(
        &mut conn,
        ids.app_id,
        ids.env_b,
        &wf_env_b,
        "step-env-b",
        WorkflowAction::Start,
        None,
        Some(&session_id),
        None,
        now - Duration::minutes(7),
    )
    .await
    .expect("env_b span in the same session");

    let spans = repo::workflow_spans_for_session(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        &session_id,
    )
    .await
    .expect("workflow_spans_for_session");

    assert_eq!(
        spans
            .iter()
            .map(|s| s.workflow_id.as_str())
            .collect::<Vec<_>>(),
        vec![wf_1.as_str(), wf_2.as_str(), wf_3.as_str()],
        "spans must be ordered by started_at ASC, not insertion order"
    );
    assert_eq!(spans[0].name, "step1");
    assert_eq!(spans[0].status, "completed", "step1 was ended");
    assert_eq!(spans[1].name, "step2");
    assert_eq!(spans[1].status, "active", "step2 is still active");
    assert_eq!(spans[2].name, "step3");
    assert_eq!(spans[2].status, "active", "step3 is still active");
    assert!(
        !spans.iter().any(|s| s.workflow_id == wf_other_session),
        "a workflow in a different session must never appear here"
    );
    assert!(
        !spans.iter().any(|s| s.workflow_id == wf_env_b),
        "a workflow in the SAME session but a different environment must never \
         appear under One(env_a) — the vector assertion above already fails in \
         that case, but name the reason"
    );

    // The positive half: env_b holds exactly its own span of this session.
    let spans_b = repo::workflow_spans_for_session(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
        &session_id,
    )
    .await
    .expect("workflow_spans_for_session env_b");
    assert_eq!(
        spans_b
            .iter()
            .map(|s| s.workflow_id.as_str())
            .collect::<Vec<_>>(),
        vec![wf_env_b.as_str()],
        "env_b sees its own span of this session and only its own"
    );

    drop(conn);
    db.cleanup().await;
}
