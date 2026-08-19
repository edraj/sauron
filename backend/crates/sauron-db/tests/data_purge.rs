//! The admin data purge, against a real Postgres.
//!
//! Everything here defends a claim that cannot be checked without a database:
//! the delete statements really do prune to the requested scope, the touched
//! keys really are harvested before the rows vanish, and the recompute really
//! does restore the counters to the truth rather than to something plausible.
//!
//! The failure mode this file exists for is a purge that *looks* like it
//! worked. Every assertion is therefore on the surviving data, never on a
//! return count — a delete that removed the wrong rows and a delete that
//! removed the right ones report the same number.
//!
//! Skips (does not fail) when `TEST_DATABASE_URL` is unset — see `common/mod.rs`.
//! A silent skip prints `ok`, so a green run here proves nothing on its own.

mod common;

use chrono::{DateTime, TimeZone, Utc};
use diesel::sql_types::{Nullable, Text, Timestamptz, Uuid as SqlUuid};
use diesel_async::RunQueryDsl;
use sauron_db::models::NewPurgeJob;
use sauron_db::purge::{self, Scope};
use sauron_purge::recompute::{Counts, SourceCounts};
use sauron_purge::PurgeKind;
use serde_json::json;
use uuid::Uuid;

use common::TestDb;

fn at(d: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 5, d, 12, 0, 0).unwrap()
}

/// The cold boundary used by every test here: far enough in the past that the
/// whole fixture is "hot", so these tests isolate the delete/recompute logic
/// from the tiering boundary.
fn all_hot() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2000, 1, 1, 0, 0, 0).unwrap()
}

// ---------------------------------------------------------------------------
// Seeding, done in raw SQL for precise control over the rollup keys
// ---------------------------------------------------------------------------

async fn seed_analytics(
    c: &mut sauron_db::PgConn,
    app: Uuid,
    env: Option<Uuid>,
    session: Option<&str>,
    device: Option<&str>,
    distinct: &str,
    when: DateTime<Utc>,
) {
    diesel::sql_query(
        "INSERT INTO analytics_events \
           (id, app_id, environment_id, name, distinct_id, properties, context, \
            session_id, device_key, occurred_at, received_at) \
         VALUES (gen_random_uuid(), $1, $2, 'ev', $5, '{}', '{}', $3, $4, $6, $6)",
    )
    .bind::<SqlUuid, _>(app)
    .bind::<Nullable<SqlUuid>, _>(env)
    .bind::<Nullable<Text>, _>(session)
    .bind::<Nullable<Text>, _>(device)
    .bind::<Text, _>(distinct)
    .bind::<Timestamptz, _>(when)
    .execute(c)
    .await
    .expect("seed analytics");
}

async fn seed_transaction(
    c: &mut sauron_db::PgConn,
    app: Uuid,
    env: Option<Uuid>,
    session: Option<&str>,
    when: DateTime<Utc>,
) {
    diesel::sql_query(
        "INSERT INTO transactions \
           (id, app_id, environment_id, name, op, duration_ms, session_id, occurred_at, received_at) \
         VALUES (gen_random_uuid(), $1, $2, 'tx', 'http', 10, $3, $4, $4)",
    )
    .bind::<SqlUuid, _>(app)
    .bind::<Nullable<SqlUuid>, _>(env)
    .bind::<Nullable<Text>, _>(session)
    .bind::<Timestamptz, _>(when)
    .execute(c)
    .await
    .expect("seed transaction");
}

#[allow(clippy::too_many_arguments)]
async fn seed_error_env(
    c: &mut sauron_db::PgConn,
    app: Uuid,
    env: Option<Uuid>,
    issue: Uuid,
    session: Option<&str>,
    distinct: Option<&str>,
    when: DateTime<Utc>,
) {
    diesel::sql_query(
        "INSERT INTO error_events \
           (id, app_id, environment_id, issue_id, fingerprint, level, message, exception_type, \
            exception_value, stacktrace, breadcrumbs, context, tags, distinct_id, \
            session_id, occurred_at, received_at, symbolication_status, contexts, extra) \
         VALUES (gen_random_uuid(), $1, $6, $2, 'fp', 'error', 'm', 'E', 'v', '[]', '[]', \
                 '{}', '{}', $4, $3, $5, $5, 'not_applicable', '{}', '{}')",
    )
    .bind::<SqlUuid, _>(app)
    .bind::<SqlUuid, _>(issue)
    .bind::<Nullable<Text>, _>(session)
    .bind::<Nullable<Text>, _>(distinct)
    .bind::<Timestamptz, _>(when)
    .bind::<Nullable<SqlUuid>, _>(env)
    .execute(c)
    .await
    .expect("seed error");
}

/// Unattributed by default — most tests do not care which environment.
async fn seed_error(
    c: &mut sauron_db::PgConn,
    app: Uuid,
    issue: Uuid,
    session: Option<&str>,
    distinct: Option<&str>,
    when: DateTime<Utc>,
) {
    seed_error_env(c, app, None, issue, session, distinct, when).await;
}

async fn seed_issue(c: &mut sauron_db::PgConn, app: Uuid, times: i64, users: i64) -> Uuid {
    let id = Uuid::new_v4();
    diesel::sql_query(
        "INSERT INTO issues \
           (id, app_id, fingerprint, type, title, culprit, level, status, \
            first_seen, last_seen, times_seen, users_seen, last_event_at) \
         VALUES ($1, $2, $3, 'error', 't', 'c', 'error', 'unresolved', \
                 now(), now(), $4, $5, now())",
    )
    .bind::<SqlUuid, _>(id)
    .bind::<SqlUuid, _>(app)
    .bind::<Text, _>(Uuid::new_v4().to_string())
    .bind::<diesel::sql_types::BigInt, _>(times)
    .bind::<diesel::sql_types::BigInt, _>(users)
    .execute(c)
    .await
    .expect("seed issue");
    id
}

#[allow(clippy::too_many_arguments)]
async fn seed_session(
    c: &mut sauron_db::PgConn,
    app: Uuid,
    env: Option<Uuid>,
    session: &str,
    started: DateTime<Utc>,
    last: DateTime<Utc>,
    events: i64,
    errors: i64,
) {
    diesel::sql_query(
        "INSERT INTO sessions \
           (id, app_id, session_id, started_at, last_event_at, events_count, \
            errors_count, context, environment_id) \
         VALUES (gen_random_uuid(), $1, $2, $3, $4, $5, $6, '{}', $7)",
    )
    .bind::<SqlUuid, _>(app)
    .bind::<Text, _>(session)
    .bind::<Timestamptz, _>(started)
    .bind::<Timestamptz, _>(last)
    .bind::<diesel::sql_types::BigInt, _>(events)
    .bind::<diesel::sql_types::BigInt, _>(errors)
    .bind::<Nullable<SqlUuid>, _>(env)
    .execute(c)
    .await
    .expect("seed session");
}

#[derive(diesel::QueryableByName)]
struct Count {
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    n: i64,
}

async fn count_rows(c: &mut sauron_db::PgConn, sql: &str, app: Uuid) -> i64 {
    let r: Count = diesel::sql_query(sql)
        .bind::<SqlUuid, _>(app)
        .get_result(c)
        .await
        .expect("count");
    r.n
}

/// Analytics rows seeded by THIS file only.
///
/// `seed_two_envs` populates its own `analytics_events` fixture, so an
/// unqualified `count(*)` over the app mixes those in and turns every
/// survivor assertion into a comparison against a number that has nothing to
/// do with the purge. The local seeder writes `name = 'ev'`; the harness
/// writes `name = 'signal'`.
const MINE: &str =
    "SELECT count(*)::bigint AS n FROM analytics_events WHERE app_id = $1 AND name = 'ev'";

#[derive(diesel::QueryableByName)]
struct SessionRow {
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    events_count: i64,
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    errors_count: i64,
}

async fn session_counters(c: &mut sauron_db::PgConn, app: Uuid, sid: &str) -> Option<(i64, i64)> {
    let r: Option<SessionRow> = diesel::sql_query(
        "SELECT events_count, errors_count FROM sessions WHERE app_id = $1 AND session_id = $2",
    )
    .bind::<SqlUuid, _>(app)
    .bind::<Text, _>(sid)
    .get_result(c)
    .await
    .ok();
    r.map(|r| (r.events_count, r.errors_count))
}

/// Create a job row so the delete statements have something to flush into —
/// they update `purge_jobs` in the same statement as the data change.
async fn make_job(
    c: &mut sauron_db::PgConn,
    ids: &common::SeedIds,
    kinds: &[PurgeKind],
    range: Option<(DateTime<Utc>, DateTime<Utc>)>,
    envs: Option<Vec<Uuid>>,
    worker: &str,
) -> sauron_db::models::PurgeJob {
    let job = purge::insert_purge_job(
        c,
        NewPurgeJob {
            org_id: ids.org_id,
            app_id: ids.app_id,
            app_slug: "slug",
            app_name: "name",
            environment_ids: envs
                .map(|v| json!(v.iter().map(|u| u.to_string()).collect::<Vec<_>>())),
            kinds: json!(kinds.iter().map(|k| k.slug()).collect::<Vec<_>>()),
            range_start: range.map(|r| r.0),
            range_end: range.map(|r| r.1),
            all_time: range.is_none(),
            requested_by: None,
            requested_by_email: "t@example.com",
        },
    )
    .await
    .expect("insert job");

    // Claim it so the worker fence on every flush matches.
    diesel::sql_query("UPDATE purge_jobs SET status='running', worker_id=$2 WHERE id=$1")
        .bind::<SqlUuid, _>(job.id)
        .bind::<Text, _>(worker)
        .execute(c)
        .await
        .expect("claim");
    purge::get_purge_job(c, job.id).await.unwrap().unwrap()
}

/// Run the delete phase for one raw kind to completion.
async fn drain_delete(
    c: &mut sauron_db::PgConn,
    kind: PurgeKind,
    scope: &Scope,
    job_id: Uuid,
    worker: &str,
    limit: i64,
) -> i64 {
    let mut cursor = None;
    let mut total = 0;
    loop {
        let b = purge::delete_raw_batch(c, kind, scope, cursor, limit, job_id, worker, true)
            .await
            .expect("batch")
            .expect("job row updated");
        total += b.deleted;
        match b.next_cursor {
            Some(n) => cursor = Some(n),
            None => return total,
        }
    }
}

// ---------------------------------------------------------------------------
// Scope
// ---------------------------------------------------------------------------

/// A range purge must leave rows outside the window completely alone. This is
/// the assertion a wrong predicate fails loudest on, and the one an operator
/// would notice last.
#[tokio::test]
async fn deletes_only_rows_inside_the_range() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut c = db.conn().await;

    seed_analytics(&mut c, ids.app_id, None, None, None, "u", at(1)).await;
    seed_analytics(&mut c, ids.app_id, None, None, None, "u", at(5)).await;
    seed_analytics(&mut c, ids.app_id, None, None, None, "u", at(9)).await;

    let job = make_job(
        &mut c,
        &ids,
        &[PurgeKind::AnalyticsEvents],
        Some((at(4), at(6))),
        None,
        "w1",
    )
    .await;
    let scope = Scope::from_job(&job, all_hot()).unwrap();
    let deleted = drain_delete(
        &mut c,
        PurgeKind::AnalyticsEvents,
        &scope,
        job.id,
        "w1",
        100,
    )
    .await;

    assert_eq!(deleted, 1);
    assert_eq!(
        count_rows(&mut c, MINE, ids.app_id).await,
        2,
        "rows outside the window must survive"
    );
    db.cleanup().await;
}

/// Naming environments must exclude unattributed rows; naming none must
/// include them. The two spellings mean opposite things and the SQL has to
/// keep them apart.
#[tokio::test]
async fn env_filter_excludes_unattributed_and_no_filter_includes_it() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut c = db.conn().await;

    seed_analytics(&mut c, ids.app_id, Some(ids.env_a), None, None, "u", at(5)).await;
    seed_analytics(&mut c, ids.app_id, Some(ids.env_b), None, None, "u", at(5)).await;
    seed_analytics(&mut c, ids.app_id, None, None, None, "u", at(5)).await;

    // Filtered to env_a: only that row goes.
    let job = make_job(
        &mut c,
        &ids,
        &[PurgeKind::AnalyticsEvents],
        Some((at(4), at(6))),
        Some(vec![ids.env_a]),
        "w1",
    )
    .await;
    let scope = Scope::from_job(&job, all_hot()).unwrap();
    drain_delete(
        &mut c,
        PurgeKind::AnalyticsEvents,
        &scope,
        job.id,
        "w1",
        100,
    )
    .await;
    assert_eq!(
        count_rows(&mut c, MINE, ids.app_id).await,
        2,
        "the unattributed row and env_b must survive an env_a-scoped purge"
    );

    // Unfiltered: the remaining two, including the unattributed one, go.
    let job2 = make_job(
        &mut c,
        &ids,
        &[PurgeKind::AnalyticsEvents],
        Some((at(4), at(6))),
        None,
        "w2",
    )
    .await;
    let scope2 = Scope::from_job(&job2, all_hot()).unwrap();
    drain_delete(
        &mut c,
        PurgeKind::AnalyticsEvents,
        &scope2,
        job2.id,
        "w2",
        100,
    )
    .await;
    assert_eq!(
        count_rows(&mut c, MINE, ids.app_id).await,
        0,
        "no env filter must include unattributed rows"
    );
    db.cleanup().await;
}

/// Keyset paging must cover every row exactly once. A cursor that failed to
/// advance would loop forever; one that skipped would leave rows behind and
/// still report success.
#[tokio::test]
async fn multi_batch_delete_covers_every_row_once() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut c = db.conn().await;

    for i in 0..10u32 {
        seed_analytics(
            &mut c,
            ids.app_id,
            None,
            None,
            None,
            "u",
            at(5) + chrono::Duration::seconds(i as i64),
        )
        .await;
    }

    let job = make_job(
        &mut c,
        &ids,
        &[PurgeKind::AnalyticsEvents],
        Some((at(4), at(6))),
        None,
        "w1",
    )
    .await;
    let scope = Scope::from_job(&job, all_hot()).unwrap();
    // Batch size 3 over 10 rows: four batches, the last one short.
    let deleted = drain_delete(&mut c, PurgeKind::AnalyticsEvents, &scope, job.id, "w1", 3).await;

    assert_eq!(deleted, 10);
    assert_eq!(count_rows(&mut c, MINE, ids.app_id).await, 0);
    db.cleanup().await;
}

// ---------------------------------------------------------------------------
// Touched keys
// ---------------------------------------------------------------------------

/// The keys must be harvested in the SAME statement that deletes the rows.
/// Once the rows are gone there is no way to discover which rollups they fed,
/// so a rollup whose key went unrecorded stays overcounting forever with
/// nothing left to detect it.
#[tokio::test]
async fn deleting_rows_records_their_rollup_keys() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut c = db.conn().await;

    seed_analytics(
        &mut c,
        ids.app_id,
        None,
        Some("s1"),
        Some("d1"),
        "alice",
        at(5),
    )
    .await;

    let job = make_job(
        &mut c,
        &ids,
        &[PurgeKind::AnalyticsEvents],
        Some((at(4), at(6))),
        None,
        "w1",
    )
    .await;
    let scope = Scope::from_job(&job, all_hot()).unwrap();
    drain_delete(
        &mut c,
        PurgeKind::AnalyticsEvents,
        &scope,
        job.id,
        "w1",
        100,
    )
    .await;

    for (kind, expected) in [
        (PurgeKind::Sessions, "s1"),
        (PurgeKind::Devices, "d1"),
        (PurgeKind::Persons, "alice"),
    ] {
        let keys = purge::next_touched_keys(&mut c, job.id, kind, None, 100)
            .await
            .unwrap();
        assert_eq!(
            keys.iter().map(|k| k.key.as_str()).collect::<Vec<_>>(),
            vec![expected],
            "{kind:?} key not harvested"
        );
    }
    db.cleanup().await;
}

/// Transactions move no counter but DO create rollup rows, so their keys must
/// still be harvested — otherwise a session whose only signals were
/// transactions survives the purge as an orphan.
#[tokio::test]
async fn transaction_keys_are_harvested_despite_moving_no_counter() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut c = db.conn().await;

    seed_transaction(&mut c, ids.app_id, None, Some("s-tx"), at(5)).await;

    let job = make_job(
        &mut c,
        &ids,
        &[PurgeKind::Transactions],
        Some((at(4), at(6))),
        None,
        "w1",
    )
    .await;
    let scope = Scope::from_job(&job, all_hot()).unwrap();
    drain_delete(&mut c, PurgeKind::Transactions, &scope, job.id, "w1", 100).await;

    let keys = purge::next_touched_keys(&mut c, job.id, PurgeKind::Sessions, None, 100)
        .await
        .unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].key, "s-tx");
    db.cleanup().await;
}

// ---------------------------------------------------------------------------
// Recompute
// ---------------------------------------------------------------------------

/// The core claim of the whole feature: after a partial delete, the rollup
/// counters equal what actually survives.
///
/// Seeded deliberately WRONG-looking — the session starts at 5 events / 3
/// errors, we delete some of each, and the recompute has to land on the true
/// remainder rather than on "old minus deleted", which would be right here by
/// coincidence and wrong the moment anything else touched the row.
#[tokio::test]
async fn recompute_restores_counters_to_what_survives() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut c = db.conn().await;
    let issue = seed_issue(&mut c, ids.app_id, 99, 99).await;

    // In range (will be deleted): 2 analytics, 1 error.
    seed_analytics(&mut c, ids.app_id, None, Some("s1"), None, "alice", at(5)).await;
    seed_analytics(&mut c, ids.app_id, None, Some("s1"), None, "alice", at(5)).await;
    seed_error(&mut c, ids.app_id, issue, Some("s1"), Some("alice"), at(5)).await;
    // Out of range (survives): 3 analytics, 2 errors.
    for _ in 0..3 {
        seed_analytics(&mut c, ids.app_id, None, Some("s1"), None, "alice", at(20)).await;
    }
    for _ in 0..2 {
        seed_error(&mut c, ids.app_id, issue, Some("s1"), Some("alice"), at(20)).await;
    }

    // The stored counters describe the pre-purge world.
    seed_session(&mut c, ids.app_id, None, "s1", at(5), at(20), 5, 3).await;

    let job = make_job(
        &mut c,
        &ids,
        &[PurgeKind::AnalyticsEvents, PurgeKind::ErrorEvents],
        Some((at(4), at(6))),
        None,
        "w1",
    )
    .await;
    let scope = Scope::from_job(&job, all_hot()).unwrap();
    drain_delete(
        &mut c,
        PurgeKind::AnalyticsEvents,
        &scope,
        job.id,
        "w1",
        100,
    )
    .await;
    drain_delete(&mut c, PurgeKind::ErrorEvents, &scope, job.id, "w1", 100).await;

    // Recompute the one touched session from the hot half (this fixture has no
    // cold data, so hot IS the whole truth here).
    let hot = purge::hot_counts_for_key(&mut c, ids.app_id, PurgeKind::Sessions, "s1")
        .await
        .unwrap();
    let counts = Counts::from_sources(
        SourceCounts {
            analytics: hot.analytics,
            errors: hot.errors,
            transactions: hot.transactions,
        },
        hot.first,
        hot.last,
    );
    purge::apply_recomputed_rollup(&mut c, PurgeKind::Sessions, ids.app_id, "s1", counts)
        .await
        .unwrap();

    assert_eq!(
        session_counters(&mut c, ids.app_id, "s1").await,
        Some((3, 2)),
        "counters must equal the surviving rows, not the stored values"
    );
    db.cleanup().await;
}

/// The bug the `evidence` field exists to prevent.
///
/// A session whose only signals are transactions sits at 0 events / 0 errors in
/// normal operation — the pipeline creates the row and bumps neither counter.
/// If emptiness were read off the counters, recompute would DELETE it, and the
/// operator would lose data no purge had selected.
#[tokio::test]
async fn a_transaction_only_session_survives_recompute() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut c = db.conn().await;

    seed_transaction(&mut c, ids.app_id, None, Some("s-tx"), at(20)).await;
    seed_session(&mut c, ids.app_id, None, "s-tx", at(20), at(20), 0, 0).await;

    let hot = purge::hot_counts_for_key(&mut c, ids.app_id, PurgeKind::Sessions, "s-tx")
        .await
        .unwrap();
    assert_eq!((hot.analytics, hot.errors, hot.transactions), (0, 0, 1));

    let counts = Counts::from_sources(
        SourceCounts {
            analytics: hot.analytics,
            errors: hot.errors,
            transactions: hot.transactions,
        },
        hot.first,
        hot.last,
    );
    assert!(!counts.is_empty(), "a transaction is surviving evidence");

    let deleted =
        purge::apply_recomputed_rollup(&mut c, PurgeKind::Sessions, ids.app_id, "s-tx", counts)
            .await
            .unwrap();
    assert!(!deleted);
    assert!(
        session_counters(&mut c, ids.app_id, "s-tx").await.is_some(),
        "the session must still exist"
    );
    db.cleanup().await;
}

/// With nothing left at all, the rollup is deleted rather than kept at zero. A
/// row describing occurrences that no longer exist is actively misleading.
#[tokio::test]
async fn a_rollup_with_no_surviving_evidence_is_deleted() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut c = db.conn().await;

    seed_session(&mut c, ids.app_id, None, "gone", at(5), at(5), 4, 0).await;

    let hot = purge::hot_counts_for_key(&mut c, ids.app_id, PurgeKind::Sessions, "gone")
        .await
        .unwrap();
    let counts = Counts::from_sources(
        SourceCounts {
            analytics: hot.analytics,
            errors: hot.errors,
            transactions: hot.transactions,
        },
        hot.first,
        hot.last,
    );
    assert!(counts.is_empty());

    let deleted =
        purge::apply_recomputed_rollup(&mut c, PurgeKind::Sessions, ids.app_id, "gone", counts)
            .await
            .unwrap();
    assert!(deleted);
    assert!(session_counters(&mut c, ids.app_id, "gone").await.is_none());
    db.cleanup().await;
}

/// A purge must not deflate `times_seen` for an issue with cold-tier history.
///
/// `sauron-tier` DETACHes and DROPs an exported partition, so the hot table
/// holds only the retention window. The recompute used to derive `times_seen`
/// from a bare `count(*) FROM error_events`, which discards every exported
/// occurrence — and `issues.times_seen` is the ONLY aggregate for a repeated
/// exception, so the number the UI shows silently drops.
///
/// The caller merges hot and cold before calling here (and fails the job if the
/// cold side is unreadable), so the contract this asserts is simply: the merged
/// `counts.errors` is what gets written, NOT whatever survives in Postgres.
/// Passing `errors` GREATER than the seeded hot rows is what makes cold history
/// present — the sibling test above passes a figure equal to the hot count, so
/// it cannot tell the two implementations apart.
#[tokio::test]
async fn recompute_keeps_cold_tier_occurrences_in_times_seen() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut c = db.conn().await;
    let issue = seed_issue(&mut c, ids.app_id, 1_000, 7).await;

    // 3 occurrences still hot; 97 already exported to Parquet and dropped with
    // their partition, so they are unreachable from `error_events`.
    seed_error(&mut c, ids.app_id, issue, None, Some("alice"), at(20)).await;
    seed_error(&mut c, ids.app_id, issue, None, Some("alice"), at(20)).await;
    seed_error(&mut c, ids.app_id, issue, None, Some("bob"), at(20)).await;

    let merged = Counts {
        events: 0,
        errors: 100,
        evidence: 100,
        first: Some(at(2)),
        last: Some(at(20)),
    };
    purge::apply_recomputed_rollup(
        &mut c,
        PurgeKind::Issues,
        ids.app_id,
        &issue.to_string(),
        merged,
    )
    .await
    .unwrap();

    #[derive(diesel::QueryableByName)]
    struct Row {
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        times_seen: i64,
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        users_seen: i64,
        #[diesel(sql_type = diesel::sql_types::Timestamptz)]
        first_seen: DateTime<Utc>,
        #[diesel(sql_type = diesel::sql_types::Timestamptz)]
        last_event_at: DateTime<Utc>,
    }
    let r: Row = diesel::sql_query(
        "SELECT times_seen, users_seen, first_seen, last_event_at FROM issues WHERE id = $1",
    )
    .bind::<SqlUuid, _>(issue)
    .get_result(&mut c)
    .await
    .unwrap();

    assert_eq!(
        r.times_seen, 100,
        "times_seen must be the MERGED count; 3 means the cold rows were discarded"
    );
    // The span is merged too: the earliest occurrence is cold, so a hot-only
    // min(occurred_at) would drag first_seen forward to at(20).
    assert_eq!(
        r.first_seen,
        at(2),
        "first_seen must come from the merged span"
    );
    assert_eq!(
        r.last_event_at,
        at(20),
        "last_event_at must come from the merged span"
    );
    // `users_seen` cannot be merged — Counts carries no distinct-user figure and
    // distinct counts do not sum across tiers. With cold history present it must
    // therefore be LEFT ALONE (overcounted, the recoverable direction) rather
    // than rewritten to the hot-only 2.
    assert_eq!(
        r.users_seen, 7,
        "users_seen must be preserved when cold history exists, not deflated to the hot DISTINCT"
    );
    db.cleanup().await;
}

/// `issues.users_seen` is a DISTINCT count, not a sum. This is the concrete
/// reason the design recomputes instead of decrementing: subtracting a deleted
/// row count from a distinct count is simply wrong.
#[tokio::test]
async fn issue_users_seen_is_recomputed_as_a_distinct_count() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut c = db.conn().await;
    let issue = seed_issue(&mut c, ids.app_id, 99, 99).await;

    // 4 surviving occurrences from 2 distinct people.
    seed_error(&mut c, ids.app_id, issue, None, Some("alice"), at(20)).await;
    seed_error(&mut c, ids.app_id, issue, None, Some("alice"), at(20)).await;
    seed_error(&mut c, ids.app_id, issue, None, Some("bob"), at(20)).await;
    seed_error(&mut c, ids.app_id, issue, None, Some("bob"), at(20)).await;

    let counts = Counts {
        events: 0,
        errors: 4,
        evidence: 4,
        first: Some(at(20)),
        last: Some(at(20)),
    };
    purge::apply_recomputed_rollup(
        &mut c,
        PurgeKind::Issues,
        ids.app_id,
        &issue.to_string(),
        counts,
    )
    .await
    .unwrap();

    #[derive(diesel::QueryableByName)]
    struct IssueRow {
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        times_seen: i64,
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        users_seen: i64,
    }
    let r: IssueRow = diesel::sql_query("SELECT times_seen, users_seen FROM issues WHERE id = $1")
        .bind::<SqlUuid, _>(issue)
        .get_result(&mut c)
        .await
        .unwrap();
    assert_eq!(r.times_seen, 4, "occurrences");
    assert_eq!(r.users_seen, 2, "DISTINCT people, not occurrences");
    db.cleanup().await;
}

// ---------------------------------------------------------------------------
// Rollup containment
// ---------------------------------------------------------------------------

/// Containment, not overlap. A session that merely brushes the window must
/// survive — deleting it would destroy evidence outside the requested scope,
/// the one failure a purge must never have.
#[tokio::test]
async fn only_fully_contained_rollups_are_deleted() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut c = db.conn().await;

    seed_session(&mut c, ids.app_id, None, "inside", at(5), at(6), 1, 0).await;
    seed_session(
        &mut c,
        ids.app_id,
        None,
        "straddle-start",
        at(1),
        at(5),
        1,
        0,
    )
    .await;
    seed_session(
        &mut c,
        ids.app_id,
        None,
        "straddle-end",
        at(6),
        at(20),
        1,
        0,
    )
    .await;
    seed_session(&mut c, ids.app_id, None, "encloses", at(1), at(20), 1, 0).await;

    let job = make_job(
        &mut c,
        &ids,
        &[PurgeKind::Sessions],
        Some((at(4), at(7))),
        None,
        "w1",
    )
    .await;
    let scope = Scope::from_job(&job, all_hot()).unwrap();
    let n = purge::delete_contained_rollups(&mut c, PurgeKind::Sessions, &scope, job.id, "w1")
        .await
        .unwrap();

    assert_eq!(n, 1, "only the fully-contained session");
    assert!(session_counters(&mut c, ids.app_id, "inside")
        .await
        .is_none());
    for survivor in ["straddle-start", "straddle-end", "encloses"] {
        assert!(
            session_counters(&mut c, ids.app_id, survivor)
                .await
                .is_some(),
            "{survivor} must survive and be recomputed instead"
        );
    }
    db.cleanup().await;
}

/// Seed one person across all three of the tables `PurgeKind::Persons` covers.
async fn seed_person(
    c: &mut sauron_db::PgConn,
    app: Uuid,
    env: Option<Uuid>,
    distinct: &str,
    first: DateTime<Utc>,
    last: DateTime<Utc>,
) {
    diesel::sql_query(
        "INSERT INTO event_users (id, app_id, distinct_id, properties, first_seen, last_seen) \
         VALUES (gen_random_uuid(), $1, $2, '{}', $3, $4)",
    )
    .bind::<SqlUuid, _>(app)
    .bind::<Text, _>(distinct)
    .bind::<Timestamptz, _>(first)
    .bind::<Timestamptz, _>(last)
    .execute(c)
    .await
    .expect("seed event_users");
    diesel::sql_query(
        "INSERT INTO event_user_environments \
           (app_id, distinct_id, environment_id, first_seen, last_seen, \
            events_count, errors_count, sessions_count) \
         VALUES ($1, $2, $3, $4, $5, 7, 7, 7)",
    )
    .bind::<SqlUuid, _>(app)
    .bind::<Text, _>(distinct)
    .bind::<Nullable<SqlUuid>, _>(env)
    .bind::<Timestamptz, _>(first)
    .bind::<Timestamptz, _>(last)
    .execute(c)
    .await
    .expect("seed event_user_environments");
    diesel::sql_query("INSERT INTO identities (app_id, alias_id, distinct_id) VALUES ($1, $2, $3)")
        .bind::<SqlUuid, _>(app)
        .bind::<Text, _>(format!("alias-{distinct}"))
        .bind::<Text, _>(distinct)
        .execute(c)
        .await
        .expect("seed identities");
}

async fn rows_for_person(c: &mut sauron_db::PgConn, app: Uuid, table: &str, distinct: &str) -> i64 {
    let r: Count = diesel::sql_query(format!(
        "SELECT count(*)::bigint AS n FROM {table} WHERE app_id = $1 AND distinct_id = $2"
    ))
    .bind::<SqlUuid, _>(app)
    .bind::<Text, _>(distinct)
    .get_result(c)
    .await
    .expect("count");
    r.n
}

/// A person is three tables, and the CONTAINED-ROLLUP delete has to know that
/// too — not just the recompute.
///
/// `rollup_table(Persons)` names `event_users` alone, so phase 2 of the worker
/// deleted the person's identity row and left `event_user_environments` and
/// `identities` behind. Neither has a foreign key to `event_users` (they
/// reference `apps`/`app_environments` only), so nothing cascaded and nothing
/// ever came back for them: the recompute phase walks `purge_touched_keys`,
/// and a person deleted here is by definition not touched by a surviving raw
/// row.
///
/// The visible symptom is the exact staleness the purge exists to repair —
/// `list_persons` reads `event_user_environments` on a backfilled app, so the
/// purged person keeps their row in the Users Explorer with the pre-purge
/// `events_count`/`errors_count`/`sessions_count` intact.
#[tokio::test]
async fn a_contained_person_takes_its_companion_tables_with_it() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut c = db.conn().await;

    seed_person(&mut c, ids.app_id, Some(ids.env_a), "ghost", at(5), at(6)).await;
    seed_person(&mut c, ids.app_id, Some(ids.env_a), "keeper", at(1), at(20)).await;

    let job = make_job(
        &mut c,
        &ids,
        &[PurgeKind::Persons],
        Some((at(4), at(7))),
        None,
        "w1",
    )
    .await;
    let scope = Scope::from_job(&job, all_hot()).unwrap();
    let n = purge::delete_contained_rollups(&mut c, PurgeKind::Persons, &scope, job.id, "w1")
        .await
        .unwrap();

    assert_eq!(n, 1, "only the fully-contained person");
    for table in ["event_users", "event_user_environments", "identities"] {
        assert_eq!(
            rows_for_person(&mut c, ids.app_id, table, "ghost").await,
            0,
            "{table} must not keep an orphan row for a deleted person"
        );
        assert_eq!(
            rows_for_person(&mut c, ids.app_id, table, "keeper").await,
            1,
            "{table} must keep the straddling person, who is recomputed instead"
        );
    }
    db.cleanup().await;
}

// ---------------------------------------------------------------------------
// The worker fence
// ---------------------------------------------------------------------------

/// Every flush is fenced on `worker_id`. A worker whose lease was stolen must
/// update zero rows — and therefore delete nothing — rather than carry on
/// double-counting under another worker's job.
#[tokio::test]
async fn a_stolen_lease_stops_the_delete() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut c = db.conn().await;

    seed_analytics(&mut c, ids.app_id, None, None, None, "u", at(5)).await;
    let job = make_job(
        &mut c,
        &ids,
        &[PurgeKind::AnalyticsEvents],
        Some((at(4), at(6))),
        None,
        "w1",
    )
    .await;
    let scope = Scope::from_job(&job, all_hot()).unwrap();

    // Another worker takes it.
    diesel::sql_query("UPDATE purge_jobs SET worker_id='w2' WHERE id=$1")
        .bind::<SqlUuid, _>(job.id)
        .execute(&mut c)
        .await
        .unwrap();

    let out = purge::delete_raw_batch(
        &mut c,
        PurgeKind::AnalyticsEvents,
        &scope,
        None,
        100,
        job.id,
        "w1",
        true,
    )
    .await
    .unwrap();

    assert!(out.is_none(), "the fenced statement must report no job row");
    assert_eq!(
        count_rows(&mut c, MINE, ids.app_id).await,
        1,
        "nothing may be deleted once the lease is gone"
    );
    db.cleanup().await;
}

/// The companion deletes are fenced too, and by the same `EXISTS` — measured,
/// because this module's header says this class of claim cannot be reasoned
/// about.
///
/// The companion CTEs carry no `fence` predicate of their own; they prune by
/// `IN (SELECT k FROM del)`, so a fenced-off `del` returning zero keys is what
/// makes them no-ops. That is an argument, and an argument is exactly what
/// produced the bug the header documents. The failure it would hide is the
/// nastier direction of the original: the person's identity row survives
/// (correctly fenced) while their counters and aliases are destroyed anyway,
/// leaving a purge that reported "I lost the claim" having half-deleted a
/// person no later pass will repair.
#[tokio::test]
async fn a_stolen_lease_stops_the_companion_deletes_too() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut c = db.conn().await;

    seed_person(&mut c, ids.app_id, Some(ids.env_a), "ghost", at(5), at(6)).await;
    let job = make_job(
        &mut c,
        &ids,
        &[PurgeKind::Persons],
        Some((at(4), at(7))),
        None,
        "w1",
    )
    .await;
    let scope = Scope::from_job(&job, all_hot()).unwrap();

    diesel::sql_query("UPDATE purge_jobs SET worker_id='w2' WHERE id=$1")
        .bind::<SqlUuid, _>(job.id)
        .execute(&mut c)
        .await
        .unwrap();

    let n = purge::delete_contained_rollups(&mut c, PurgeKind::Persons, &scope, job.id, "w1")
        .await
        .unwrap();

    assert_eq!(n, 0, "no deletion is reported once the lease is gone");
    for table in ["event_users", "event_user_environments", "identities"] {
        assert_eq!(
            rows_for_person(&mut c, ids.app_id, table, "ghost").await,
            1,
            "{table} must be untouched once the lease is gone"
        );
    }
    db.cleanup().await;
}

/// Every rollup kind must survive a real `hot_counts_for_key` call.
///
/// The bug this exists for: `issue_id` exists ONLY on `error_events`, so the
/// original statement — which probed all three raw tables for whatever the key
/// column was — failed the entire purge job at the recompute phase with
/// `column "issue_id" does not exist`. The delete phase had already run, so the
/// job ended `failed` with rows deleted and every counter left overcounting:
/// the exact half-finished state the two-phase design exists to prevent.
///
/// It reached a live drive because the issues test called
/// `apply_recomputed_rollup` directly with hand-built counts and never went
/// through this function. Looping over the real vocabulary is what makes that
/// class of gap impossible — a kind added tomorrow is covered automatically.
#[tokio::test]
async fn every_rollup_kind_can_be_counted_for_real() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut c = db.conn().await;
    let issue = seed_issue(&mut c, ids.app_id, 1, 1).await;

    for kind in sauron_purge::ALL {
        if kind.class() != sauron_purge::Class::Rollup {
            continue;
        }
        // A uuid for issues, an arbitrary string for everything else — the
        // statement casts per kind and must accept both.
        let key = if *kind == PurgeKind::Issues {
            issue.to_string()
        } else {
            "some-key".to_string()
        };
        let got = purge::hot_counts_for_key(&mut c, ids.app_id, *kind, &key).await;
        assert!(
            got.is_ok(),
            "hot_counts_for_key failed for {kind:?}: {:?}",
            got.err()
        );
    }
    db.cleanup().await;
}

/// And the issue key really does count only `error_events`.
#[tokio::test]
async fn issue_counts_come_from_error_events_alone() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut c = db.conn().await;
    let issue = seed_issue(&mut c, ids.app_id, 0, 0).await;

    seed_error(&mut c, ids.app_id, issue, None, Some("alice"), at(20)).await;
    seed_error(&mut c, ids.app_id, issue, None, Some("bob"), at(20)).await;

    let got = purge::hot_counts_for_key(&mut c, ids.app_id, PurgeKind::Issues, &issue.to_string())
        .await
        .unwrap();
    assert_eq!(got.errors, 2);
    assert_eq!(got.analytics, 0, "analytics_events has no issue_id at all");
    assert_eq!(got.transactions, 0, "transactions has no issue_id at all");
    db.cleanup().await;
}

/// A person is three tables, and only one of them has counters.
///
/// `event_users` holds identity and span; `event_user_environments` holds
/// `events_count` / `errors_count` / `sessions_count` PER ENVIRONMENT. Writing
/// a counter to `event_users` fails with `column "events_count" does not
/// exist` — found on a live drive, after the delete phase had already run, so
/// the job ended `failed` with rows gone and every counter stale.
#[tokio::test]
async fn recomputing_a_person_repairs_per_environment_counters() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut c = db.conn().await;

    // Two surviving analytics + one error for alice in env_a.
    seed_analytics(
        &mut c,
        ids.app_id,
        Some(ids.env_a),
        Some("s1"),
        None,
        "alice",
        at(20),
    )
    .await;
    seed_analytics(
        &mut c,
        ids.app_id,
        Some(ids.env_a),
        Some("s1"),
        None,
        "alice",
        at(21),
    )
    .await;
    let issue = seed_issue(&mut c, ids.app_id, 1, 1).await;
    seed_error_env(
        &mut c,
        ids.app_id,
        Some(ids.env_a),
        issue,
        Some("s1"),
        Some("alice"),
        at(20),
    )
    .await;

    diesel::sql_query(
        "INSERT INTO event_users (id, app_id, distinct_id, properties, first_seen, last_seen) \
         VALUES (gen_random_uuid(), $1, 'alice', '{}', $2, $2) ON CONFLICT DO NOTHING",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .bind::<Timestamptz, _>(at(20))
    .execute(&mut c)
    .await
    .unwrap();
    // Deliberately wrong stored counters, as a purge would leave them.
    diesel::sql_query(
        "INSERT INTO event_user_environments \
           (app_id, distinct_id, environment_id, first_seen, last_seen, \
            events_count, errors_count, sessions_count) \
         VALUES ($1, 'alice', $2, $3, $3, 99, 99, 99)",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .bind::<Nullable<SqlUuid>, _>(Some(ids.env_a))
    .bind::<Timestamptz, _>(at(20))
    .execute(&mut c)
    .await
    .unwrap();

    let hot = purge::hot_counts_for_key(&mut c, ids.app_id, PurgeKind::Persons, "alice")
        .await
        .unwrap();
    let counts = Counts::from_sources(
        SourceCounts {
            analytics: hot.analytics,
            errors: hot.errors,
            transactions: hot.transactions,
        },
        hot.first,
        hot.last,
    );
    purge::apply_recomputed_rollup(&mut c, PurgeKind::Persons, ids.app_id, "alice", counts)
        .await
        .unwrap();

    #[derive(diesel::QueryableByName)]
    struct EnvRow {
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        events_count: i64,
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        errors_count: i64,
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        sessions_count: i64,
    }
    let r: EnvRow = diesel::sql_query(
        "SELECT events_count, errors_count, sessions_count FROM event_user_environments \
          WHERE app_id = $1 AND distinct_id = 'alice'",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .get_result(&mut c)
    .await
    .unwrap();
    assert_eq!(r.events_count, 2, "analytics only");
    assert_eq!(r.errors_count, 1, "errors only");
    assert_eq!(r.sessions_count, 1, "DISTINCT sessions, not rows");
    db.cleanup().await;
}

/// An environment where nothing of the person survives loses its counter row —
/// the per-environment form of the rule that a rollup describing occurrences
/// that no longer exist is worse than no row at all.
#[tokio::test]
async fn a_person_environment_with_no_survivors_is_removed() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut c = db.conn().await;

    // alice has rows in env_a only, but a counter row in env_b too.
    seed_analytics(
        &mut c,
        ids.app_id,
        Some(ids.env_a),
        Some("s1"),
        None,
        "alice",
        at(20),
    )
    .await;
    diesel::sql_query(
        "INSERT INTO event_users (id, app_id, distinct_id, properties, first_seen, last_seen) \
         VALUES (gen_random_uuid(), $1, 'alice', '{}', $2, $2) ON CONFLICT DO NOTHING",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .bind::<Timestamptz, _>(at(20))
    .execute(&mut c)
    .await
    .unwrap();
    for env in [ids.env_a, ids.env_b] {
        diesel::sql_query(
            "INSERT INTO event_user_environments \
               (app_id, distinct_id, environment_id, first_seen, last_seen, \
                events_count, errors_count, sessions_count) \
             VALUES ($1, 'alice', $2, $3, $3, 5, 5, 5)",
        )
        .bind::<SqlUuid, _>(ids.app_id)
        .bind::<Nullable<SqlUuid>, _>(Some(env))
        .bind::<Timestamptz, _>(at(20))
        .execute(&mut c)
        .await
        .unwrap();
    }

    let counts = Counts {
        events: 1,
        errors: 0,
        evidence: 1,
        first: Some(at(20)),
        last: Some(at(20)),
    };
    purge::apply_recomputed_rollup(&mut c, PurgeKind::Persons, ids.app_id, "alice", counts)
        .await
        .unwrap();

    let n = count_rows(
        &mut c,
        "SELECT count(*)::bigint AS n FROM event_user_environments \
          WHERE app_id = $1 AND distinct_id = 'alice'",
        ids.app_id,
    )
    .await;
    assert_eq!(n, 1, "only the environment with surviving rows keeps a row");
    db.cleanup().await;
}

/// The recompute path's own delete covers all three tables too.
///
/// This is the sibling of `a_contained_person_takes_its_companion_tables_with_it`:
/// a person can be removed by EITHER path — phase 2 when their whole span is
/// inside the window, or here when the raw deletion left them with nothing
/// surviving — and an orphan from either one is equally invisible, because the
/// job reports success in both cases.
#[tokio::test]
async fn a_person_with_nothing_surviving_loses_all_three_tables() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut c = db.conn().await;

    // No raw rows at all for `ghost` — the state the delete phase leaves behind
    // when it removed every event they ever had.
    seed_person(&mut c, ids.app_id, Some(ids.env_a), "ghost", at(5), at(6)).await;
    seed_person(&mut c, ids.app_id, Some(ids.env_a), "keeper", at(5), at(6)).await;

    let hot = purge::hot_counts_for_key(&mut c, ids.app_id, PurgeKind::Persons, "ghost")
        .await
        .unwrap();
    let counts = Counts::from_sources(
        SourceCounts {
            analytics: hot.analytics,
            errors: hot.errors,
            transactions: hot.transactions,
        },
        hot.first,
        hot.last,
    );
    assert!(counts.is_empty(), "fixture must leave nothing surviving");

    let deleted =
        purge::apply_recomputed_rollup(&mut c, PurgeKind::Persons, ids.app_id, "ghost", counts)
            .await
            .unwrap();

    assert!(deleted, "reported as a delete, not a recompute");
    for table in ["event_users", "event_user_environments", "identities"] {
        assert_eq!(
            rows_for_person(&mut c, ids.app_id, table, "ghost").await,
            0,
            "{table} must not keep an orphan row for a deleted person"
        );
        assert_eq!(
            rows_for_person(&mut c, ids.app_id, table, "keeper").await,
            1,
            "{table} must be pruned by key, never wholesale for the app"
        );
    }
    db.cleanup().await;
}
