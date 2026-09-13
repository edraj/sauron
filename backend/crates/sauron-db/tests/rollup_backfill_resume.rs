//! The rollup backfill must survive being interrupted and finish on a later
//! run WITHOUT double-counting the days that already landed.
//!
//! Why: the backfill is additive (each day's fold ADDS to the rollup rows) and
//! only writes the per-app readiness markers at the very end. Before
//! `rollup_backfill_progress` existed, an interrupted run left added days with
//! no marker and the next run re-added all of them — which is why it could
//! only ever be run by hand, in tmux, and why upgraded deployments that never
//! ran it served the legacy O(history) queries until they timed out.
//!
//! Skips (does not fail) when `TEST_DATABASE_URL` is unset — see `common`.

mod common;

use chrono::{Duration, NaiveDate, Utc};
use common::TestDb;
use diesel::sql_types::{BigInt, Nullable, Text, Timestamptz, Uuid as SqlUuid};
use diesel_async::RunQueryDsl;
use sauron_db::rollups;
use sauron_db::rollups::fold::{backfill_all_resumable, BackfillOutcome};
use uuid::Uuid;

#[derive(diesel::QueryableByName)]
struct Count {
    #[diesel(sql_type = BigInt)]
    n: i64,
}

async fn insert_event(
    conn: &mut sauron_db::PgConn,
    app: Uuid,
    env: Uuid,
    distinct: &str,
    at: chrono::DateTime<Utc>,
) {
    diesel::sql_query(
        "INSERT INTO analytics_events \
             (app_id, environment_id, session_id, distinct_id, name, screen, occurred_at) \
         VALUES ($1, $2, $3, $4, 'tap', NULL, $5)",
    )
    .bind::<SqlUuid, _>(app)
    .bind::<Nullable<SqlUuid>, _>(Some(env))
    .bind::<Nullable<Text>, _>(Some(format!("s-{distinct}")))
    .bind::<Text, _>(distinct.to_string())
    .bind::<Timestamptz, _>(at)
    .execute(conn)
    .await
    .expect("insert analytics event");
}

async fn top_count(conn: &mut sauron_db::PgConn, app: Uuid) -> i64 {
    diesel::sql_query(
        "SELECT COALESCE(sum(count), 0)::bigint AS n FROM event_top_daily WHERE app_id = $1 AND name = 'tap'",
    )
    .bind::<SqlUuid, _>(app)
    .get_result::<Count>(conn)
    .await
    .expect("sum event_top_daily")
    .n
}

async fn days_done(conn: &mut sauron_db::PgConn) -> i64 {
    diesel::sql_query(
        "SELECT COALESCE((SELECT (next_day - first_day)::bigint FROM rollup_backfill_progress), 0) AS n",
    )
    .get_result::<Count>(conn)
    .await
    .expect("progress")
    .n
}

#[tokio::test]
async fn interrupted_backfill_resumes_without_double_counting() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs().await;
    let app = ids.app_id;

    // Three consecutive days of history, well behind today so the fold never
    // treats them as live. Two events per day, six in total.
    let base = Utc::now() - Duration::days(10);
    let mut expected_days: Vec<NaiveDate> = Vec::new();
    for d in 0..3 {
        let at = base + Duration::days(d);
        expected_days.push(at.date_naive());
        insert_event(&mut conn, app, ids.env_a, &format!("u{d}a"), at).await;
        insert_event(
            &mut conn,
            app,
            ids.env_a,
            &format!("u{d}b"),
            at + Duration::minutes(1),
        )
        .await;
    }
    assert!(
        !rollups::is_ready(&mut conn, app).await.expect("gate"),
        "gate must start closed"
    );

    // Run 1: the caller asks to stop after the first day (what a SIGTERM
    // between two day-transactions amounts to).
    let mut seen = Vec::new();
    let outcome = backfill_all_resumable(&mut conn, 2000, |day| {
        seen.push(day);
        seen.is_empty() // stop right after day one
    })
    .await
    .expect("first run");
    assert_eq!(outcome, BackfillOutcome::Interrupted);
    assert_eq!(seen, vec![expected_days[0]]);
    assert_eq!(
        days_done(&mut conn).await,
        1,
        "exactly one day recorded as landed"
    );
    assert!(
        !rollups::is_ready(&mut conn, app).await.expect("gate"),
        "no marker before completion"
    );
    assert_eq!(
        top_count(&mut conn, app).await,
        2,
        "day one's two events, once"
    );

    // Run 2: resumes at day two, finishes, marks the app.
    let mut seen = Vec::new();
    let outcome = backfill_all_resumable(&mut conn, 2000, |day| {
        seen.push(day);
        true
    })
    .await
    .expect("second run");
    assert_eq!(outcome, BackfillOutcome::Completed);
    // The run covers every day from the first unfinished one up to today
    // (empty days included — the bound is the calendar, not the data).
    assert_eq!(
        <[_]>::first(&seen),
        Some(&expected_days[1]),
        "resumed from the first unfinished day"
    );
    assert_eq!(
        <[_]>::last(&seen),
        Some(&Utc::now().date_naive()),
        "ran through today"
    );
    assert!(
        !seen.contains(&expected_days[0]),
        "day one must not be folded twice"
    );
    assert!(
        rollups::is_ready(&mut conn, app).await.expect("gate"),
        "marker written at the end"
    );
    assert_eq!(
        top_count(&mut conn, app).await,
        6,
        "six events, each counted exactly once"
    );
    assert!(!rollups::backfill_pending(&mut conn).await.expect("pending"));

    // Run 3: markers present — must not add again.
    let outcome = backfill_all_resumable(&mut conn, 2000, |_| true)
        .await
        .expect("third run");
    assert_eq!(outcome, BackfillOutcome::AlreadyDone);
    assert_eq!(
        top_count(&mut conn, app).await,
        6,
        "a re-run on a marked instance adds nothing"
    );

    drop(conn);
    db.cleanup().await;
}

#[tokio::test]
async fn person_days_backfill_is_idempotent_once_marked() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs().await;
    let app = ids.app_id;
    // The harness pins `rollup_epoch` into the future so gates start closed,
    // but not `person_days_epoch` — pin it the same way so the seeded app
    // predates it and a backfill is actually pending.
    diesel::sql_query("UPDATE person_days_epoch SET started_at = now() + interval '10 years'")
        .execute(&mut conn)
        .await
        .expect("pin person_days epoch");
    let at = Utc::now() - Duration::days(5);
    insert_event(&mut conn, app, ids.env_a, "p1", at).await;
    insert_event(&mut conn, app, ids.env_a, "p1", at + Duration::minutes(2)).await;

    assert!(sauron_db::person_days_backfill::backfill_pending(&mut conn)
        .await
        .expect("probe"));
    sauron_db::person_days_backfill::backfill_all(db.pool())
        .await
        .expect("first");
    let once: i64 = diesel::sql_query(
        "SELECT COALESCE(sum(events), 0)::bigint AS n FROM person_days WHERE app_id = $1 AND distinct_id = 'p1'",
    )
    .bind::<SqlUuid, _>(app)
    .get_result::<Count>(&mut conn)
    .await
    .expect("sum")
    .n;
    assert_eq!(once, 2);
    assert!(
        !sauron_db::person_days_backfill::backfill_pending(&mut conn)
            .await
            .expect("probe")
    );

    sauron_db::person_days_backfill::backfill_all(db.pool())
        .await
        .expect("second");
    let twice: i64 = diesel::sql_query(
        "SELECT COALESCE(sum(events), 0)::bigint AS n FROM person_days WHERE app_id = $1 AND distinct_id = 'p1'",
    )
    .bind::<SqlUuid, _>(app)
    .get_result::<Count>(&mut conn)
    .await
    .expect("sum")
    .n;
    assert_eq!(
        twice, 2,
        "a second run on a marked instance must add nothing"
    );

    drop(conn);
    db.cleanup().await;
}
