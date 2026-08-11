//! Ingest failure recovery, against a real Postgres.
//!
//! Almost everything worth testing here is SQL the Rust compiler cannot check,
//! and specifically SQL that depends on how Postgres schedules the
//! sub-statements of a single data-modifying CTE. Those semantics are the whole
//! reason the schema looks the way it does:
//!
//! * sub-statements share one snapshot and cannot see each other's effects, so
//!   a `NOT EXISTS` guard must exclude the row its sibling CTE is deleting;
//! * one statement may not update the same row twice, which is why `retained`
//!   and `dropped` are derived rather than stored.
//!
//! Both produce silently wrong numbers rather than errors, so only a real
//! database can catch them.
//!
//! Skips (does not fail) when `TEST_DATABASE_URL` is unset — see `common`.

mod common;

use chrono::{Duration, Utc};
use common::TestDb;
use sauron_db::models::NewIngestFailure;
use sauron_db::repo;
use serde_json::json;
use uuid::Uuid;

async fn record(
    db: &TestDb,
    fingerprint: &str,
    kind: &str,
    message: &str,
    app_id: Option<Uuid>,
    payload: Option<serde_json::Value>,
    cap: i64,
) -> repo::RecordedFailure {
    let mut conn = db.conn().await;
    repo::record_ingest_failure(
        &mut conn,
        &NewIngestFailure {
            fingerprint,
            error_kind: kind,
            error_message: message,
            org_id: None,
            project_id: None,
            app_id,
        },
        payload.as_ref(),
        0,
        cap,
    )
    .await
    .expect("record failure")
}

async fn group(db: &TestDb, id: Uuid) -> sauron_db::models::IngestFailureRow {
    let mut conn = db.conn().await;
    repo::get_ingest_failure(&mut conn, id)
        .await
        .expect("get")
        .expect("group exists")
}

#[tokio::test]
async fn repeated_failures_fold_into_one_group() {
    let Some(db) = TestDb::setup().await else {
        return;
    };

    let mut id = Uuid::nil();
    for i in 0..5 {
        let r = record(&db, "fp-fold", "decode", "bad json", None, Some(json!({"i": i})), 100).await;
        id = r.id;
        assert!(r.retained, "under the cap, every payload is retained");
    }

    let g = group(&db, id).await;
    assert_eq!(g.occurrences, 5, "five occurrences must be one group of five");
    assert_eq!(g.retained, 5);
    assert_eq!(g.dropped, 0);

    db.cleanup().await;
}

/// The cap must stop retaining payloads while still counting occurrences, and
/// the derived `dropped` must equal exactly what was refused.
///
/// A stored counter here would need a second UPDATE of the row the upsert just
/// wrote, which Postgres declines to apply — the counters would read 0 dropped
/// forever while payloads silently vanished.
#[tokio::test]
async fn the_payload_cap_refuses_without_losing_the_count() {
    let Some(db) = TestDb::setup().await else {
        return;
    };

    let cap = 3;
    let mut id = Uuid::nil();
    let mut retained_reports = 0;
    for i in 0..10 {
        let r = record(&db, "fp-cap", "decode", "bad json", None, Some(json!({"i": i})), cap).await;
        id = r.id;
        if r.retained {
            retained_reports += 1;
        }
    }

    let g = group(&db, id).await;
    assert_eq!(g.occurrences, 10, "every occurrence counts, capped or not");
    assert_eq!(g.retained, cap, "the cap bounds retained payloads");
    assert_eq!(retained_reports, cap, "record() must report the refusals honestly");
    assert_eq!(
        g.dropped,
        10 - cap,
        "dropped must account for every refused occurrence — a 0 here is the \
         silent-truncation bug this page exists to expose"
    );

    db.cleanup().await;
}

/// Different fingerprints stay different groups, and the list filter does not
/// leak one into the other.
#[tokio::test]
async fn distinct_fingerprints_are_distinct_groups() {
    let Some(db) = TestDb::setup().await else {
        return;
    };

    record(&db, "fp-a", "decode", "a", None, Some(json!({})), 10).await;
    record(&db, "fp-b", "db_constraint", "b", None, Some(json!({})), 10).await;

    let mut conn = db.conn().await;
    let all = repo::list_ingest_failures(&mut conn, None, None, None, 50)
        .await
        .unwrap();
    assert_eq!(all.len(), 2);

    let decodes = repo::list_ingest_failures(&mut conn, None, Some("decode"), None, 50)
        .await
        .unwrap();
    assert_eq!(decodes.len(), 1, "the error_kind filter must not over-match");
    assert_eq!(decodes[0].error_kind, "decode");
    drop(conn);

    db.cleanup().await;
}

/// The CTE-snapshot trap, stated as a test.
///
/// `resolve_ingest_failure_payload` deletes a child and resolves the parent
/// only if no children remain. The `NOT EXISTS` subquery still sees the row
/// being deleted, because sub-statements share a snapshot — so without the
/// explicit `p.id <> $1` exclusion the group would NEVER reach `resolved`, and
/// the page would show permanently-requeued groups that are in fact done.
#[tokio::test]
async fn resolving_the_last_payload_resolves_the_group() {
    let Some(db) = TestDb::setup().await else {
        return;
    };

    let r = record(&db, "fp-resolve", "decode", "x", None, Some(json!({"n": 1})), 10).await;

    let mut conn = db.conn().await;
    let items = repo::start_ingest_failure_retry(&mut conn, r.id).await.unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(
        repo::get_ingest_failure(&mut conn, r.id).await.unwrap().unwrap().status,
        "requeued",
    );

    repo::resolve_ingest_failure_payload(&mut conn, items[0].id)
        .await
        .unwrap();
    let g = repo::get_ingest_failure(&mut conn, r.id).await.unwrap().unwrap();
    assert_eq!(
        g.status, "resolved",
        "the last payload resolving must resolve the group"
    );
    assert_eq!(g.retained, 0);
    drop(conn);

    db.cleanup().await;
}

/// The other half of the same trap: with payloads still outstanding, the group
/// must NOT resolve early.
#[tokio::test]
async fn resolving_one_of_many_leaves_the_group_open() {
    let Some(db) = TestDb::setup().await else {
        return;
    };

    let mut id = Uuid::nil();
    for i in 0..3 {
        id = record(&db, "fp-partial", "decode", "x", None, Some(json!({"i": i})), 10)
            .await
            .id;
    }

    let mut conn = db.conn().await;
    let items = repo::start_ingest_failure_retry(&mut conn, id).await.unwrap();
    assert_eq!(items.len(), 3);
    repo::resolve_ingest_failure_payload(&mut conn, items[0].id)
        .await
        .unwrap();

    let g = repo::get_ingest_failure(&mut conn, id).await.unwrap().unwrap();
    assert_eq!(g.status, "requeued", "two payloads are still outstanding");
    assert_eq!(g.retained, 2);
    drop(conn);

    db.cleanup().await;
}

/// A failed replay returns the payload to the pool and reopens the group with
/// the new error, so the admin learns why the retry did not take.
#[tokio::test]
async fn a_failed_replay_reopens_the_group() {
    let Some(db) = TestDb::setup().await else {
        return;
    };

    let r = record(&db, "fp-refail", "db_constraint", "original", None, Some(json!({})), 10).await;

    let mut conn = db.conn().await;
    let items = repo::start_ingest_failure_retry(&mut conn, r.id).await.unwrap();
    repo::fail_ingest_failure_payload(&mut conn, items[0].id, "still broken")
        .await
        .unwrap();

    let g = repo::get_ingest_failure(&mut conn, r.id).await.unwrap().unwrap();
    assert_eq!(g.status, "failed");
    assert_eq!(g.error_message, "still broken", "the NEW error must be shown");
    assert_eq!(g.retained, 1, "a failed replay must not consume the payload");

    let payloads = repo::list_ingest_failure_payloads(&mut conn, r.id, 10, 0)
        .await
        .unwrap();
    assert!(
        payloads[0].requeued_at.is_none(),
        "a payload left stamped in-flight would await a verdict nobody sends"
    );
    drop(conn);

    db.cleanup().await;
}

/// A new occurrence must reopen a group someone had resolved, or a recurring
/// problem would go unnoticed after its first fix.
#[tokio::test]
async fn a_new_occurrence_reopens_a_resolved_group() {
    let Some(db) = TestDb::setup().await else {
        return;
    };

    let r = record(&db, "fp-reopen", "decode", "x", None, Some(json!({})), 10).await;
    let mut conn = db.conn().await;
    let items = repo::start_ingest_failure_retry(&mut conn, r.id).await.unwrap();
    repo::resolve_ingest_failure_payload(&mut conn, items[0].id)
        .await
        .unwrap();
    assert_eq!(
        repo::get_ingest_failure(&mut conn, r.id).await.unwrap().unwrap().status,
        "resolved"
    );
    drop(conn);

    record(&db, "fp-reopen", "decode", "x again", None, Some(json!({})), 10).await;
    let g = group(&db, r.id).await;
    assert_eq!(g.status, "failed", "a recurrence must reopen the group");
    assert_eq!(g.occurrences, 2);

    db.cleanup().await;
}

/// Dropping is a hard delete and must take the children with it. An orphaned
/// child would be a masked copy of a real event that no page can show and no
/// reaper can find.
#[tokio::test]
async fn dropping_a_group_cascades_to_its_payloads() {
    let Some(db) = TestDb::setup().await else {
        return;
    };

    let mut id = Uuid::nil();
    for i in 0..4 {
        id = record(&db, "fp-drop", "decode", "x", None, Some(json!({"i": i})), 10)
            .await
            .id;
    }

    let mut conn = db.conn().await;
    assert_eq!(repo::delete_ingest_failure(&mut conn, id).await.unwrap(), 1);
    assert!(repo::get_ingest_failure(&mut conn, id).await.unwrap().is_none());
    assert!(
        repo::list_ingest_failure_payloads(&mut conn, id, 50, 0)
            .await
            .unwrap()
            .is_empty(),
        "children must cascade"
    );
    drop(conn);

    db.cleanup().await;
}

/// The retention bound: aged groups go, current ones stay.
#[tokio::test]
async fn the_reaper_deletes_only_aged_groups() {
    let Some(db) = TestDb::setup().await else {
        return;
    };

    let old = record(&db, "fp-old", "decode", "x", None, Some(json!({})), 10).await;
    let fresh = record(&db, "fp-fresh", "decode", "y", None, Some(json!({})), 10).await;

    let mut conn = db.conn().await;
    // Age the first group by hand — the column has no setter, deliberately.
    diesel_async::RunQueryDsl::execute(
        diesel::sql_query("UPDATE ingest_failures SET last_seen_at = $1 WHERE id = $2")
            .bind::<diesel::sql_types::Timestamptz, _>(Utc::now() - Duration::days(60))
            .bind::<diesel::sql_types::Uuid, _>(old.id),
        &mut conn,
    )
    .await
    .unwrap();

    let cutoff = Utc::now() - Duration::days(30);
    assert_eq!(repo::reap_ingest_failures(&mut conn, cutoff).await.unwrap(), 1);
    assert!(repo::get_ingest_failure(&mut conn, old.id).await.unwrap().is_none());
    assert!(repo::get_ingest_failure(&mut conn, fresh.id).await.unwrap().is_some());
    drop(conn);

    db.cleanup().await;
}

/// Keyset paging with the `id` tiebreaker.
///
/// Groups written in one burst share `last_seen_at` to microsecond precision;
/// an untiebroken cursor would skip or repeat one of them at the boundary. The
/// assertion that matters is that the union of pages is the full set with no
/// duplicates.
#[tokio::test]
async fn paging_covers_every_group_exactly_once() {
    let Some(db) = TestDb::setup().await else {
        return;
    };

    for i in 0..7 {
        record(&db, &format!("fp-page-{i}"), "decode", "x", None, Some(json!({})), 10).await;
    }

    let mut conn = db.conn().await;
    let mut seen: Vec<Uuid> = Vec::new();
    let mut cursor = None;
    loop {
        let page = repo::list_ingest_failures(&mut conn, None, None, cursor, 3)
            .await
            .unwrap();
        if page.is_empty() {
            break;
        }
        cursor = page.last().map(|r| (r.last_seen_at, r.id));
        seen.extend(page.iter().map(|r| r.id));
        if page.len() < 3 {
            break;
        }
    }
    drop(conn);

    seen.sort();
    let before = seen.len();
    seen.dedup();
    assert_eq!(before, seen.len(), "paging must not repeat a group");
    assert_eq!(seen.len(), 7, "paging must not skip a group");

    db.cleanup().await;
}
