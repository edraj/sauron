//! The deferred sessions cutover (migration 0073's second half): per-day
//! resumable copy from `sessions_old_73`, DEFAULT-parked live rows evicted
//! with live-wins semantics, old table dropped only when drained.
//!
//! The template database predates the schema-only 0073 shape, so the old
//! table is recreated here the way the migration leaves it (same columns,
//! same global UNIQUE) — the finisher only ever reads and deletes from it.

mod common;

use chrono::{Duration, Utc};
use common::TestDb;
use diesel::sql_types::{BigInt, Text, Timestamptz, Uuid as SqlUuid};
use diesel_async::RunQueryDsl;
use sauron_db::repo;
use sauron_db::sessions_cutover::{finish_sessions_partitioning, FinishOutcome};
use uuid::Uuid;

#[derive(diesel::QueryableByName)]
struct CountRow {
    #[diesel(sql_type = BigInt)]
    n: i64,
}

async fn count(conn: &mut sauron_db::PgConn, sql: &str) -> i64 {
    let r: CountRow = diesel::sql_query(sql).get_result(conn).await.expect(sql);
    r.n
}

#[tokio::test]
async fn finisher_moves_days_and_lets_live_rows_win() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut conn = db.conn().await;

    let suffix = Uuid::new_v4().simple().to_string();
    let org = repo::create_org(&mut conn, "cutover org", &format!("cutover-{suffix}"))
        .await
        .expect("org");
    let project = repo::create_project(&mut conn, org.id, "cutover", &format!("cutover-{suffix}"))
        .await
        .expect("project");
    let app = repo::create_app(
        &mut conn,
        project.id,
        "cutover",
        &format!("cutover-{suffix}"),
        "web",
    )
    .await
    .expect("app");

    diesel::sql_query(
        "CREATE TABLE sessions_old_73 (LIKE sessions INCLUDING DEFAULTS, \
         UNIQUE (app_id, session_id))",
    )
    .execute(&mut conn)
    .await
    .expect("create old table");

    // Three days: an ancient stray, and two real days ~40 back.
    let old_day = Utc::now().date_naive() - Duration::days(40);
    for (sid, at) in [
        (
            "cut-ancient",
            chrono::DateTime::parse_from_rfc3339("1971-02-03T12:00:00Z")
                .expect("ts")
                .with_timezone(&Utc),
        ),
        (
            "cut-a",
            old_day.and_hms_opt(6, 0, 0).expect("valid").and_utc(),
        ),
        (
            "cut-b",
            old_day.and_hms_opt(7, 0, 0).expect("valid").and_utc(),
        ),
        (
            "cut-c",
            (old_day + Duration::days(1))
                .and_hms_opt(6, 0, 0)
                .expect("valid")
                .and_utc(),
        ),
    ] {
        diesel::sql_query(
            "INSERT INTO sessions_old_73 (app_id, session_id, started_at, last_event_at) \
             VALUES ($1, $2, $3, $3)",
        )
        .bind::<SqlUuid, _>(app.id)
        .bind::<Text, _>(sid.to_string())
        .bind::<Timestamptz, _>(at)
        .execute(&mut conn)
        .await
        .expect("insert old session");
    }
    // A LIVE twin of cut-a parked in DEFAULT (its day has no partition in the
    // template) — the exact state that made partition creation fail in the
    // smoke drive, and the row whose newer state must win.
    diesel::sql_query(
        "INSERT INTO sessions (app_id, session_id, started_at, last_event_at, events_count) \
         SELECT app_id, session_id, started_at, last_event_at, 999 \
         FROM sessions_old_73 WHERE session_id = 'cut-a'",
    )
    .execute(&mut conn)
    .await
    .expect("insert live twin");

    match finish_sessions_partitioning(&mut conn, |_, _| {})
        .await
        .expect("finisher")
    {
        FinishOutcome::Finished { days, rows } => {
            assert_eq!(days, 3, "three distinct days");
            assert_eq!(rows, 4, "all four sessions moved (twin resolved at evict)");
        }
        FinishOutcome::AlreadyDone => panic!("old table existed; must not report AlreadyDone"),
    }

    assert_eq!(
        count(
            &mut conn,
            "SELECT count(*)::bigint AS n FROM pg_class WHERE relname='sessions_old_73'"
        )
        .await,
        0,
        "old table dropped"
    );
    let twin: CountRow = diesel::sql_query(
        "SELECT events_count::bigint AS n FROM sessions WHERE session_id = 'cut-a'",
    )
    .get_result(&mut conn)
    .await
    .expect("twin row");
    assert_eq!(twin.n, 999, "the live (parked) version must win");
    assert_eq!(
        count(&mut conn, "SELECT count(*)::bigint AS n FROM (SELECT 1 FROM sessions GROUP BY app_id, session_id HAVING count(*)>1) d").await,
        0,
        "no duplicate sessions"
    );

    // Idempotent second run.
    assert!(matches!(
        finish_sessions_partitioning(&mut conn, |_, _| {})
            .await
            .expect("rerun"),
        FinishOutcome::AlreadyDone
    ));

    db.cleanup().await;
}
