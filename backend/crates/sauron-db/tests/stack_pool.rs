//! Tier 1 stack pooling, end to end against the real migrated schema.
//!
//! `INGEST_STACK_POOLING` is latched by a process-wide `OnceLock` on first
//! use, so this file — and only this file — sets it, and every test in it runs
//! with pooling ON. Every other integration binary runs with the flag unset,
//! which is what keeps the default-off parity covered for free.

mod common;

use chrono::Utc;
use common::TestDb;
use diesel::sql_types::{BigInt, Text, Uuid as SqlUuid};
use diesel_async::RunQueryDsl;
use sauron_db::models::NewErrorEvent;
use sauron_db::scope::{EnvFilter, ReadScope};
use sauron_db::{batch, repo, stack_pool};
use serde_json::json;
use uuid::Uuid;

fn pooling_on() {
    std::env::set_var("INGEST_STACK_POOLING", "1");
    assert!(
        stack_pool::pooling_enabled(),
        "the OnceLock latched off — another test in this BINARY read the flag \
         before this file set it; keep all pooling tests in stack_pool.rs"
    );
}

async fn seed_issue(c: &mut sauron_db::PgConn, app: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    diesel::sql_query(
        "INSERT INTO issues \
           (id, app_id, fingerprint, type, title, culprit, level, status, \
            first_seen, last_seen, times_seen, users_seen, last_event_at) \
         VALUES ($1, $2, $3, 'error', 't', 'c', 'error', 'unresolved', \
                 now(), now(), 1, 1, now())",
    )
    .bind::<SqlUuid, _>(id)
    .bind::<SqlUuid, _>(app)
    .bind::<Text, _>(Uuid::new_v4().to_string())
    .execute(c)
    .await
    .unwrap();
    id
}

fn trace_a() -> serde_json::Value {
    json!([
        {"function": "poll", "module": "core::future", "filename": "future.rs",
         "abs_path": "/rustc/registry/core/future.rs", "lineno": 123, "in_app": false},
        {"function": "handle_request", "module": "app::server", "filename": "server.rs",
         "abs_path": "/srv/app/server.rs", "lineno": 42, "in_app": true}
    ])
}

fn ev(app: Uuid, issue: Uuid, trace: serde_json::Value, session: &str) -> NewErrorEvent {
    NewErrorEvent {
        id: Uuid::new_v4(),
        app_id: app,
        environment_id: None,
        issue_id: issue,
        fingerprint: "fp-pool".into(),
        level: "error".into(),
        message: "boom".into(),
        exception_type: "PoolError".into(),
        exception_value: "boom".into(),
        stacktrace: trace,
        breadcrumbs: json!([]),
        context: json!({}),
        tags: json!({}),
        release: None,
        distinct_id: Some("alice".into()),
        event_user: None,
        sdk: None,
        ip_address: None,
        occurred_at: Utc::now(),
        session_id: Some(session.into()),
        device_key: Some("dev-1".into()),
        screen: None,
        workflow_id: None,
        workflow_name: None,
        stacktrace_symbolicated: None,
        symbolication_status: "not_applicable".into(),
        debug_meta: None,
        contexts: json!({}),
        extra: json!({}),
        handled: Some(true),
        title: None,
        culprit: None,
        stacktrace_sha256: None,
    }
}

#[derive(diesel::QueryableByName)]
struct N {
    #[diesel(sql_type = BigInt)]
    n: i64,
}

async fn count(c: &mut sauron_db::PgConn, sql: &'static str) -> i64 {
    let r: N = diesel::sql_query(sql).get_result(c).await.unwrap();
    r.n
}

/// The core storage contract: N duplicates -> one blob; every row keeps its
/// own identity (COUNT is untouched); the placeholder is what sits inline; and
/// hydration returns byte-identical traces — including the mixed page where
/// one row was never pooled because its trace is empty.
#[tokio::test]
async fn duplicates_share_one_blob_and_hydrate_back_identically() {
    pooling_on();
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut c = db.conn().await;
    let issue = seed_issue(&mut c, ids.app_id).await;

    let mut rows: Vec<NewErrorEvent> = (0..50)
        .map(|_| ev(ids.app_id, issue, trace_a(), "s-pool"))
        .collect();
    rows.push(ev(
        ids.app_id,
        issue,
        json!([{"function": "other", "in_app": true}]),
        "s-pool",
    ));
    rows.push(ev(ids.app_id, issue, json!([]), "s-pool")); // stays inline

    let n = batch::insert_error_events(&mut c, &rows).await.unwrap();
    assert_eq!(n, 52, "one row per occurrence, pooling or not");

    assert_eq!(
        count(&mut c, "SELECT count(*) AS n FROM error_stack_blobs").await,
        2,
        "two DISTINCT traces -> two blobs, not 51"
    );
    assert_eq!(
        count(
            &mut c,
            "SELECT count(*) AS n FROM error_events WHERE stacktrace_sha256 IS NOT NULL"
        )
        .await,
        51,
        "every framed row is pooled; the empty-trace row is not"
    );
    assert_eq!(
        count(
            &mut c,
            "SELECT count(*) AS n FROM error_events \
             WHERE stacktrace_sha256 IS NOT NULL AND stacktrace <> '[]'::jsonb"
        )
        .await,
        0,
        "a pooled row's inline column is exactly the placeholder"
    );

    // Read back through a real repo path — hydration must be invisible.
    let got = repo::errors_for_session(
        &mut c,
        ReadScope::new(ids.app_id, EnvFilter::All),
        "s-pool",
        100,
    )
    .await
    .unwrap();
    assert_eq!(got.len(), 52);
    let hydrated_a = got.iter().filter(|e| e.stacktrace == trace_a()).count();
    assert_eq!(hydrated_a, 50, "all 50 duplicates hydrate to the original");
    assert_eq!(
        got.iter().filter(|e| e.stacktrace == json!([])).count(),
        1,
        "the genuinely-empty trace stays empty"
    );
    db.cleanup().await;
}

/// The tenant-safety property the whole design hangs on: masking a scope
/// de-pools THAT SCOPE's rows and masks their private copies, while rows
/// outside the scope keep hydrating the original, unmasked trace from the
/// untouched shared blob.
#[tokio::test]
async fn mask_depools_its_scope_and_never_touches_the_shared_blob() {
    pooling_on();
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut c = db.conn().await;
    let issue = seed_issue(&mut c, ids.app_id).await;

    // Two rows sharing one blob: one inside the masked day, one 10 days back.
    let mut in_scope = ev(ids.app_id, issue, trace_a(), "s-in");
    in_scope.occurred_at = Utc::now();
    let mut out_of_scope = ev(ids.app_id, issue, trace_a(), "s-out");
    out_of_scope.occurred_at = Utc::now() - chrono::Duration::days(10);
    batch::insert_error_events(&mut c, &[in_scope, out_of_scope])
        .await
        .unwrap();
    assert_eq!(
        count(&mut c, "SELECT count(*) AS n FROM error_stack_blobs").await,
        1
    );

    // A running mask action targeting stacktrace[].abs_path, worker w1.
    let targets = json!([{"table": "error_events", "column": "stacktrace", "path": "[].abs_path"}]);
    let action = repo::insert_mask_action(
        &mut c,
        sauron_db::models::NewInspectorMaskAction {
            org_id: ids.org_id,
            app_id: ids.app_id,
            kind: "mask",
            finding_id: None,
            scan_id: None,
            targets: &targets,
            requested_by: None,
            requested_by_email: "admin@example.com",
        },
    )
    .await
    .unwrap();
    diesel::sql_query(
        "UPDATE inspector_mask_actions SET status='running', worker_id='w1' WHERE id=$1",
    )
    .bind::<SqlUuid, _>(action.id)
    .execute(&mut c)
    .await
    .unwrap();

    let outcome = repo::mask_batch_jsonb_wildcard(
        &mut c,
        sauron_inspector::targets::TargetTable::ErrorEvents,
        sauron_inspector::targets::TargetColumn::Stacktrace,
        ids.app_id,
        Utc::now().date_naive(),
        &["abs_path".to_string()],
        repo::BatchCursor {
            occurred_at: None,
            id: None,
        },
        100,
        action.id,
        "w1",
    )
    .await
    .unwrap();
    assert!(outcome.is_some(), "the batch must claim and process rows");

    // In-scope: de-pooled, own copy, masked.
    assert_eq!(
        count(
            &mut c,
            "SELECT count(*) AS n FROM error_events \
             WHERE session_id = 's-in' AND stacktrace_sha256 IS NULL \
               AND stacktrace::text LIKE '%****%'"
        )
        .await,
        1,
        "the in-scope row must be de-pooled and masked in its own copy"
    );
    // The shared blob: byte-for-byte untouched.
    assert_eq!(
        count(
            &mut c,
            "SELECT count(*) AS n FROM error_stack_blobs \
             WHERE content::text LIKE '%/srv/app/server.rs%' \
               AND content::text NOT LIKE '%****%'"
        )
        .await,
        1,
        "the shared blob must never be rewritten by a mask"
    );
    // Out-of-scope: still pooled, still hydrates the ORIGINAL trace.
    let got = repo::errors_for_session(
        &mut c,
        ReadScope::new(ids.app_id, EnvFilter::All),
        "s-out",
        10,
    )
    .await
    .unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(
        got[0].stacktrace,
        trace_a(),
        "a row outside the mask scope must keep reading the unmasked original"
    );
    db.cleanup().await;
}

/// GC lifecycle: a referenced blob survives a sweep (and the FK makes deleting
/// it outright an error); once the last referencing row is gone, the sweep
/// reclaims it.
#[tokio::test]
async fn sweep_reclaims_a_blob_only_after_its_last_reference_is_gone() {
    pooling_on();
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut c = db.conn().await;
    let issue = seed_issue(&mut c, ids.app_id).await;

    batch::insert_error_events(&mut c, &[ev(ids.app_id, issue, trace_a(), "s-gc")])
        .await
        .unwrap();

    // Referenced: grace 0 must still keep it...
    assert_eq!(
        stack_pool::sweep_orphan_stack_blobs(&mut c, 0)
            .await
            .unwrap(),
        0,
        "a referenced blob must survive the sweep"
    );
    // ...and a raw DELETE must be refused by the FK, loudly.
    let err = diesel::sql_query("DELETE FROM error_stack_blobs")
        .execute(&mut c)
        .await;
    assert!(
        err.is_err(),
        "the FK must refuse to free a still-referenced blob"
    );

    diesel::sql_query("DELETE FROM error_events WHERE app_id = $1")
        .bind::<SqlUuid, _>(ids.app_id)
        .execute(&mut c)
        .await
        .unwrap();
    assert_eq!(
        stack_pool::sweep_orphan_stack_blobs(&mut c, 0)
            .await
            .unwrap(),
        1,
        "the last reference is gone; the sweep must reclaim the blob"
    );
    db.cleanup().await;
}
