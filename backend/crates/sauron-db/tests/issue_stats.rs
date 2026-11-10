//! `issue_stats_hourly` (migration 84): per-issue hourly event counts behind
//! MCP trending.
//!
//! The contract under test is exactly-once counting across the three writers:
//! the live fold (its own `issue_stats` watermark, additive), the consistency
//! rebuild (`fold_day_from_raw(day, None, true, …)`: REPLACE the day from raw,
//! bounded by `received_at <=` that watermark) and the base rollups' additive
//! backfill path, which must never write this table at all.
//!
//! Test DBs leave `issue_stats_epoch` at the template's migration time
//! (controller ruling R3), so every test app is "ready". Tests that care about
//! the epoch or watermark set both themselves via [`open_issue_stats_gate`].

mod common;

use chrono::{DateTime, Duration, DurationRound, NaiveDate, Utc};
use common::TestDb;
use diesel::sql_types::{Array, BigInt, Nullable, Text, Timestamptz, Uuid as SqlUuid};
use diesel::QueryableByName;
use diesel_async::RunQueryDsl;
use sauron_db::rollups::{self, fold};
use uuid::Uuid;

async fn exec(conn: &mut sauron_db::PgConn, sql: &str) {
    diesel::sql_query(sql).execute(conn).await.expect(sql);
}

/// Epoch a day back, watermark at the epoch: everything a test inserts from
/// here on is "received after the epoch" and therefore the live fold's.
async fn open_issue_stats_gate(conn: &mut sauron_db::PgConn) {
    exec(
        conn,
        "UPDATE issue_stats_epoch SET started_at = now() - interval '1 day'",
    )
    .await;
    exec(
        conn,
        "UPDATE rollup_watermarks SET watermark = (SELECT started_at FROM issue_stats_epoch) \
         WHERE source = 'issue_stats'",
    )
    .await;
}

async fn issue(conn: &mut sauron_db::PgConn, app: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    diesel::sql_query(
        "INSERT INTO issues (id, app_id, fingerprint, type, title, culprit, level, status, \
                             first_seen, last_seen, times_seen, users_seen, last_event_at) \
         VALUES ($1, $2, $3, 'error', 't', 'c', 'error', 'unresolved', now(), now(), 1, 1, now())",
    )
    .bind::<SqlUuid, _>(id)
    .bind::<SqlUuid, _>(app)
    .bind::<Text, _>(id.to_string())
    .execute(conn)
    .await
    .expect("insert issue");
    id
}

/// One error event on an EXISTING issue; `received_at` defaults to `now()`.
async fn error(
    conn: &mut sauron_db::PgConn,
    app: Uuid,
    env: Option<Uuid>,
    issue: Uuid,
    release: Option<&str>,
    at: DateTime<Utc>,
) {
    diesel::sql_query(
        "INSERT INTO error_events (app_id, environment_id, issue_id, fingerprint, release, occurred_at) \
         VALUES ($1, $2, $3, 'issue-stats-fp', $4, $5)",
    )
    .bind::<SqlUuid, _>(app)
    .bind::<Nullable<SqlUuid>, _>(env)
    .bind::<SqlUuid, _>(issue)
    .bind::<Nullable<Text>, _>(release.map(str::to_string))
    .bind::<Timestamptz, _>(at)
    .execute(conn)
    .await
    .expect("insert error event");
}

/// [`error`] with an explicit `received_at`, for tests that must place a row
/// on a known side of the watermark without racing the clock.
async fn error_received(
    conn: &mut sauron_db::PgConn,
    app: Uuid,
    env: Option<Uuid>,
    issue: Uuid,
    release: Option<&str>,
    at: DateTime<Utc>,
    received: DateTime<Utc>,
) {
    diesel::sql_query(
        "INSERT INTO error_events (app_id, environment_id, issue_id, fingerprint, release, occurred_at, received_at) \
         VALUES ($1, $2, $3, 'issue-stats-fp', $4, $5, $6)",
    )
    .bind::<SqlUuid, _>(app)
    .bind::<Nullable<SqlUuid>, _>(env)
    .bind::<SqlUuid, _>(issue)
    .bind::<Nullable<Text>, _>(release.map(str::to_string))
    .bind::<Timestamptz, _>(at)
    .bind::<Timestamptz, _>(received)
    .execute(conn)
    .await
    .expect("insert error event (explicit received_at)");
}

fn hour_of(t: DateTime<Utc>) -> DateTime<Utc> {
    t.duration_trunc(Duration::hours(1)).expect("hour trunc")
}

/// Noon UTC on the day `days_back` days ago. Anchoring at noon (rather than
/// `now - N days`) keeps a few seconds of per-row offsets on one UTC day
/// whatever the wall clock, so a test that runs just before midnight cannot
/// spill rows into the next day.
fn noon_utc(days_back: i64) -> DateTime<Utc> {
    (Utc::now() - Duration::days(days_back))
        .date_naive()
        .and_hms_opt(12, 0, 0)
        .expect("noon is valid")
        .and_utc()
}

#[derive(QueryableByName, Debug, PartialEq, Eq, Clone, PartialOrd, Ord)]
struct StatRow {
    #[diesel(sql_type = SqlUuid)]
    app_id: Uuid,
    #[diesel(sql_type = Nullable<SqlUuid>)]
    environment_id: Option<Uuid>,
    #[diesel(sql_type = SqlUuid)]
    issue_id: Uuid,
    #[diesel(sql_type = Text)]
    release: String,
    #[diesel(sql_type = Timestamptz)]
    hour: DateTime<Utc>,
    #[diesel(sql_type = BigInt)]
    events: i64,
}

/// The whole table, sorted — the thing every "unchanged" assertion compares.
async fn snapshot(conn: &mut sauron_db::PgConn) -> Vec<StatRow> {
    let mut rows: Vec<StatRow> = diesel::sql_query(
        "SELECT app_id, environment_id, issue_id, release, hour, events FROM issue_stats_hourly",
    )
    .get_results(conn)
    .await
    .expect("snapshot issue_stats_hourly");
    rows.sort();
    rows
}

/// The same shape computed straight from raw `error_events` — the oracle.
async fn raw_oracle(conn: &mut sauron_db::PgConn) -> Vec<StatRow> {
    let mut rows: Vec<StatRow> = diesel::sql_query(
        "SELECT app_id, environment_id, issue_id, COALESCE(release, '') AS release, \
                date_trunc('hour', occurred_at, 'UTC') AS hour, count(*)::bigint AS events \
         FROM error_events GROUP BY 1, 2, 3, 4, 5",
    )
    .get_results(conn)
    .await
    .expect("raw oracle");
    rows.sort();
    rows
}

#[derive(QueryableByName)]
struct SumRow {
    #[diesel(sql_type = BigInt)]
    n: i64,
}

async fn issue_events(conn: &mut sauron_db::PgConn, issue: Uuid) -> i64 {
    let r: SumRow = diesel::sql_query(
        "SELECT COALESCE(sum(events), 0)::bigint AS n FROM issue_stats_hourly WHERE issue_id = $1",
    )
    .bind::<SqlUuid, _>(issue)
    .get_result(conn)
    .await
    .expect("sum issue events");
    r.n
}

/// `(issue, environment, release key, hour, events)` — the brief's tuple.
type Cell = (Uuid, Option<Uuid>, String, DateTime<Utc>, i64);

fn issue_stats_drift_on(drifts: &[(NaiveDate, String)], day: NaiveDate) -> bool {
    drifts
        .iter()
        .any(|(d, what)| *d == day && what.starts_with(&format!("{}:", rollups::SRC_ISSUE_STATS)))
}

/// The brief's fixture: issue A gets 3× (env_a, "1.0", H) and 2× (env_b,
/// NULL release, H); issue B gets 1× (env_a, "1.0", H+1). Timestamps sit on
/// both edges of the hour so Rust's `hour_of` and Postgres'
/// `date_trunc('hour', …, 'UTC')` must agree on every one of them.
struct Fixture {
    app: Uuid,
    env_a: Uuid,
    env_b: Uuid,
    a: Uuid,
    b: Uuid,
    h: DateTime<Utc>,
    h1: DateTime<Utc>,
}

async fn seed_fixture(db: &TestDb, conn: &mut sauron_db::PgConn) -> Fixture {
    let ids = db.seed_two_envs().await;
    let (app, env_a, env_b) = (ids.app_id, ids.env_a, ids.env_b);
    let a = issue(conn, app).await;
    let b = issue(conn, app).await;
    // Five hours back: inside the pull's occurred_at floor (≥ midnight two
    // days back) whatever the wall clock, and H+1 is still in the past.
    let h = hour_of(Utc::now() - Duration::hours(5));
    let h1 = h + Duration::hours(1);
    let last_micro = Duration::microseconds(3_599_999_999);
    for at in [h, h + Duration::minutes(30), h + last_micro] {
        error(conn, app, Some(env_a), a, Some("1.0"), at).await;
    }
    for at in [h + Duration::seconds(1), h + Duration::minutes(45)] {
        error(conn, app, Some(env_b), a, None, at).await;
    }
    // Exactly on the next hour's first instant: belongs to H+1, not H.
    error(conn, app, Some(env_a), b, Some("1.0"), h1).await;
    Fixture {
        app,
        env_a,
        env_b,
        a,
        b,
        h,
        h1,
    }
}

#[tokio::test]
async fn fold_counts_match_raw_per_hour_env_release() {
    let Some(db) = TestDb::setup().await else {
        panic!("TEST_DATABASE_URL unset — this test must not silently skip");
    };
    let mut conn = db.conn().await;
    open_issue_stats_gate(&mut conn).await;
    let f = seed_fixture(&db, &mut conn).await;

    let out = fold::fold_issue_stats(&mut conn, Utc::now())
        .await
        .expect("fold issue stats")
        .expect("watermark was behind upto");
    assert!(out.caught_up);
    assert!(out.rows_read >= 6, "read {} rows", out.rows_read);

    let rows: Vec<Cell> = diesel::sql_query(
        "SELECT app_id, environment_id, issue_id, release, hour, events FROM issue_stats_hourly \
         WHERE issue_id = ANY($1) ORDER BY hour, release DESC",
    )
    .bind::<Array<SqlUuid>, _>(vec![f.a, f.b])
    .get_results::<StatRow>(&mut conn)
    .await
    .expect("read rows")
    .into_iter()
    .inspect(|r| assert_eq!(r.app_id, f.app))
    .map(|r| (r.issue_id, r.environment_id, r.release, r.hour, r.events))
    .collect();
    let (a, b, env_a, env_b, h, h1) = (f.a, f.b, f.env_a, f.env_b, f.h, f.h1);
    assert_eq!(
        rows,
        vec![
            (a, Some(env_a), "1.0".to_string(), h, 3),
            (a, Some(env_b), "".to_string(), h, 2),
            (b, Some(env_a), "1.0".to_string(), h1, 1),
        ]
    );
    // And the whole table (the harness's own seeded errors included) is the
    // raw grouping, cell for cell.
    assert_eq!(snapshot(&mut conn).await, raw_oracle(&mut conn).await);

    db.cleanup().await;
}

#[tokio::test]
async fn second_fold_reads_nothing() {
    let Some(db) = TestDb::setup().await else {
        panic!("TEST_DATABASE_URL unset — this test must not silently skip");
    };
    let mut conn = db.conn().await;
    open_issue_stats_gate(&mut conn).await;
    let f = seed_fixture(&db, &mut conn).await;

    let upto = Utc::now();
    let first = fold::fold_issue_stats(&mut conn, upto)
        .await
        .expect("first fold")
        .expect("first fold ran");
    assert!(first.rows_read >= 6);
    assert_eq!(first.new_watermark, upto);
    let before = snapshot(&mut conn).await;

    let second = fold::fold_issue_stats(&mut conn, upto + Duration::seconds(1))
        .await
        .expect("second fold");
    assert!(
        matches!(&second, Some(o) if o.rows_read == 0),
        "second fold must read nothing, got {:?}",
        second.as_ref().map(|o| o.rows_read)
    );
    // At (or behind) the watermark the fold is a no-op that returns None.
    assert!(fold::fold_issue_stats(&mut conn, upto)
        .await
        .expect("third fold")
        .is_none());
    assert_eq!(snapshot(&mut conn).await, before);
    assert_eq!(issue_events(&mut conn, f.a).await, 5);
    assert_eq!(issue_events(&mut conn, f.b).await, 1);

    db.cleanup().await;
}

#[tokio::test]
async fn rebuild_day_equals_fold() {
    rebuild_day_equals_fold_in(None).await;
}

/// The same proof under a half-hour-offset session TimeZone. The rebuild's
/// `date_trunc('hour', occurred_at, 'UTC')` must ignore the session zone; the
/// two-argument form would truncate in Asia/Kolkata (+05:30) and land every
/// bucket on :30 UTC, which the comparison with the Rust-folded rows (and
/// the raw oracle, also three-argument) would catch.
#[tokio::test]
async fn rebuild_day_equals_fold_under_non_utc_session_timezone() {
    rebuild_day_equals_fold_in(Some("Asia/Kolkata")).await;
}

async fn rebuild_day_equals_fold_in(session_tz: Option<&str>) {
    let Some(db) = TestDb::setup().await else {
        panic!("TEST_DATABASE_URL unset — this test must not silently skip");
    };
    let mut conn = db.conn().await;
    if let Some(tz) = session_tz {
        // Session-level, on the one connection that folds, rebuilds and reads.
        exec(&mut conn, &format!("SET TimeZone = '{tz}'")).await;
        #[derive(QueryableByName)]
        struct TzRow {
            #[diesel(sql_type = Text)]
            tz: String,
        }
        let shown: TzRow = diesel::sql_query("SELECT current_setting('TimeZone') AS tz")
            .get_result(&mut conn)
            .await
            .expect("read TimeZone");
        assert_eq!(shown.tz, tz, "the session zone must actually be in effect");
    }
    open_issue_stats_gate(&mut conn).await;
    let f = seed_fixture(&db, &mut conn).await;

    fold::fold_issue_stats(&mut conn, Utc::now())
        .await
        .expect("fold")
        .expect("fold ran");
    let folded = snapshot(&mut conn).await;
    assert_eq!(folded, raw_oracle(&mut conn).await);

    // Rebuild H's day, the next hour's day (they differ when H is 23:00), and
    // today (where the harness's own seeded errors live).
    let mut days = vec![f.h.date_naive(), f.h1.date_naive(), Utc::now().date_naive()];
    days.dedup();
    for day in days {
        fold::fold_day_from_raw(&mut conn, day, None, true, 2000)
            .await
            .expect("rebuild day");
        assert_eq!(
            snapshot(&mut conn).await,
            folded,
            "rebuilding {day} must leave the folded rows exactly as they were"
        );
    }
    // A second rebuild of the same day is a no-op too (REPLACE, not add).
    fold::fold_day_from_raw(&mut conn, f.h.date_naive(), None, true, 2000)
        .await
        .expect("rebuild again");
    assert_eq!(snapshot(&mut conn).await, folded);

    db.cleanup().await;
}

#[tokio::test]
async fn base_additive_backfill_does_not_touch_issue_stats() {
    let Some(db) = TestDb::setup().await else {
        panic!("TEST_DATABASE_URL unset — this test must not silently skip");
    };
    let mut conn = db.conn().await;
    open_issue_stats_gate(&mut conn).await;
    let f = seed_fixture(&db, &mut conn).await;

    let upto = Utc::now();
    fold::fold_issue_stats(&mut conn, upto)
        .await
        .expect("fold")
        .expect("fold ran");
    // One row past the issue_stats watermark, not yet folded. An additive
    // write would double the folded cells; a REPLACE bounded by anything but
    // the issue_stats watermark would pick this row up. Either changes the
    // snapshot.
    error_received(
        &mut conn,
        f.app,
        Some(f.env_a),
        f.a,
        Some("1.0"),
        f.h + Duration::minutes(10),
        upto + Duration::seconds(1),
    )
    .await;
    let before = snapshot(&mut conn).await;
    let epoch = rollups::epoch(&mut conn).await.expect("rollup epoch");

    fold::fold_day_from_raw(&mut conn, f.h.date_naive(), Some(epoch), false, 2000)
        .await
        .expect("base additive backfill day");
    assert_eq!(snapshot(&mut conn).await, before);
    assert_eq!(issue_events(&mut conn, f.a).await, 5);

    db.cleanup().await;
}

#[tokio::test]
async fn late_row_heals_via_consistency_rebuild() {
    let Some(db) = TestDb::setup().await else {
        panic!("TEST_DATABASE_URL unset — this test must not silently skip");
    };
    let mut conn = db.conn().await;
    open_issue_stats_gate(&mut conn).await;
    let ids = db.seed_two_envs().await;
    let late_issue = issue(&mut conn, ids.app_id).await;

    // Occurred three days ago (noon UTC), received now: below the pull's
    // occurred_at floor (midnight two days back), so the live fold steps over
    // it. Eight of them, because the sweep tolerates an absolute drift of up
    // to 5 rows per day (`consistency_check_day`) — a single straggler would
    // be (deliberately) ignored.
    let late_at = noon_utc(3);
    let day = late_at.date_naive();
    for i in 0..8 {
        error(
            &mut conn,
            ids.app_id,
            Some(ids.env_a),
            late_issue,
            Some("2.0"),
            late_at + Duration::seconds(i),
        )
        .await;
    }
    fold::fold_issue_stats(&mut conn, Utc::now())
        .await
        .expect("fold")
        .expect("fold ran");
    assert_eq!(
        issue_events(&mut conn, late_issue).await,
        0,
        "the occurred_at floor must keep the incremental pull off old partitions"
    );

    let drifts = fold::consistency_check_trailing(&mut conn)
        .await
        .expect("sweep");
    assert!(
        issue_stats_drift_on(&drifts, day),
        "the sweep must report the late day under the issue_stats label: {drifts:?}"
    );

    fold::fold_day_from_raw(&mut conn, day, None, true, 2000)
        .await
        .expect("heal day");
    assert_eq!(issue_events(&mut conn, late_issue).await, 8);

    // Healed for good: the sweep is clean for this label, and neither another
    // fold nor another rebuild counts the stragglers again.
    let drifts = fold::consistency_check_trailing(&mut conn)
        .await
        .expect("sweep after heal");
    assert!(
        !issue_stats_drift_on(&drifts, day),
        "healed day must not drift: {drifts:?}"
    );
    fold::fold_issue_stats(&mut conn, Utc::now())
        .await
        .expect("fold after heal");
    fold::fold_day_from_raw(&mut conn, day, None, true, 2000)
        .await
        .expect("rebuild again");
    assert_eq!(issue_events(&mut conn, late_issue).await, 8);
    assert_eq!(snapshot(&mut conn).await, raw_oracle(&mut conn).await);

    db.cleanup().await;
}

/// Row versions (`ctid`, `xmin`) of every base rollup row of `day`: a rewrite
/// of the day's rollups (DELETE + INSERT) changes them, a table left alone
/// keeps them.
async fn base_rollup_versions(
    conn: &mut sauron_db::PgConn,
    day: NaiveDate,
) -> Vec<(String, String, String)> {
    #[derive(QueryableByName)]
    struct V {
        #[diesel(sql_type = Text)]
        t: String,
        #[diesel(sql_type = Text)]
        c: String,
        #[diesel(sql_type = Text)]
        x: String,
    }
    let mut sql = String::new();
    for tbl in [
        "screen_stats_daily",
        "journey_nodes_daily",
        "journey_links_daily",
        "user_activity_daily",
        "user_activity_release_daily",
        "event_top_daily",
    ] {
        sql.push_str(&format!(
            "SELECT '{tbl}' AS t, ctid::text AS c, xmin::text AS x FROM {tbl} \
             WHERE day = $1::date UNION ALL "
        ));
    }
    sql.push_str(
        "SELECT 'perf_agg_hourly', ctid::text, xmin::text FROM perf_agg_hourly \
         WHERE hour >= $2 AND hour < $3 ORDER BY 1, 2, 3",
    );
    let start = day.and_hms_opt(0, 0, 0).expect("valid").and_utc();
    let rows: Vec<V> = diesel::sql_query(sql)
        .bind::<diesel::sql_types::Date, _>(day)
        .bind::<Timestamptz, _>(start)
        .bind::<Timestamptz, _>(start + Duration::days(1))
        .get_results(conn)
        .await
        .expect("base rollup row versions");
    rows.into_iter().map(|v| (v.t, v.c, v.x)).collect()
}

/// The maintenance heal for a day on which ONLY `issue_stats` drifted:
/// `replace_day_locked` repairs `issue_stats_hourly` and leaves every other
/// rollup table of the day untouched (same row versions), where the full
/// `fold_day_from_raw` rebuild would rewrite them all.
#[tokio::test]
async fn replace_day_locked_heals_issue_stats_only_drift_and_nothing_else() {
    let Some(db) = TestDb::setup().await else {
        panic!("TEST_DATABASE_URL unset — this test must not silently skip");
    };
    let mut conn = db.conn().await;
    open_issue_stats_gate(&mut conn).await;
    let ids = db.seed_two_envs().await;
    let late_issue = issue(&mut conn, ids.app_id).await;

    // Late rows, as in `late_row_heals_via_consistency_rebuild`: the live
    // fold's occurred_at floor steps over them, so issue_stats drifts.
    let late_at = noon_utc(3);
    let day = late_at.date_naive();
    for i in 0..8 {
        error(
            &mut conn,
            ids.app_id,
            Some(ids.env_a),
            late_issue,
            Some("2.0"),
            late_at + Duration::seconds(i),
        )
        .await;
    }
    fold::fold_issue_stats(&mut conn, Utc::now())
        .await
        .expect("fold")
        .expect("fold ran");
    assert_eq!(issue_events(&mut conn, late_issue).await, 0);
    // The base rollups of the day, folded additively (never writes
    // issue_stats_hourly): the base sources agree with raw, so the sweep sees
    // an issue_stats-only drift.
    fold::fold_day_from_raw(&mut conn, day, None, false, 2000)
        .await
        .expect("base fold of the day");
    let drifts = fold::consistency_check_trailing(&mut conn)
        .await
        .expect("sweep");
    let on_day: Vec<&String> = drifts
        .iter()
        .filter(|(d, _)| *d == day)
        .map(|(_, w)| w)
        .collect();
    assert!(
        !on_day.is_empty()
            && on_day
                .iter()
                .all(|w| w.starts_with(&format!("{}:", rollups::SRC_ISSUE_STATS))),
        "precondition: only issue_stats drifts on the day: {drifts:?}"
    );
    let before = base_rollup_versions(&mut conn, day).await;
    assert!(
        !before.is_empty(),
        "the base fold must have written rows, or 'untouched' proves nothing"
    );

    let inserted = rollups::issue_stats::replace_day_locked(&mut conn, day)
        .await
        .expect("replace day");
    assert!(inserted.is_some_and(|n| n > 0), "{inserted:?}");
    assert_eq!(issue_events(&mut conn, late_issue).await, 8);
    assert_eq!(
        base_rollup_versions(&mut conn, day).await,
        before,
        "the targeted replace must not rewrite any other rollup table"
    );
    let drifts = fold::consistency_check_trailing(&mut conn)
        .await
        .expect("sweep after replace");
    assert!(
        !issue_stats_drift_on(&drifts, day),
        "healed day must not drift: {drifts:?}"
    );
    assert_eq!(snapshot(&mut conn).await, raw_oracle(&mut conn).await);

    // Counter-factual: the full rebuild DOES rewrite them, so the probe above
    // can see a rewrite when one happens.
    fold::fold_day_from_raw(&mut conn, day, None, true, 2000)
        .await
        .expect("full rebuild");
    assert_ne!(base_rollup_versions(&mut conn, day).await, before);
    assert_eq!(issue_events(&mut conn, late_issue).await, 8);

    db.cleanup().await;
}

/// Below the `error_events` tier floor the targeted replace touches nothing.
#[tokio::test]
async fn replace_day_locked_refuses_days_below_the_tier_floor() {
    let Some(db) = TestDb::setup().await else {
        panic!("TEST_DATABASE_URL unset — this test must not silently skip");
    };
    let mut conn = db.conn().await;
    open_issue_stats_gate(&mut conn).await;
    let ids = db.seed_two_envs().await;
    let iss = issue(&mut conn, ids.app_id).await;
    let at = noon_utc(4);
    let day = at.date_naive();
    error(&mut conn, ids.app_id, None, iss, None, at).await;
    let dropped_thru = (day + Duration::days(1))
        .and_hms_opt(0, 0, 0)
        .expect("valid")
        .and_utc();
    diesel::sql_query(
        "INSERT INTO tiering_state (table_name, watermark, dropped_thru, updated_at) \
         VALUES ('error_events', $1, $1, now())",
    )
    .bind::<Timestamptz, _>(dropped_thru)
    .execute(&mut conn)
    .await
    .expect("record tier boundary");
    let before = snapshot(&mut conn).await;
    assert_eq!(
        rollups::issue_stats::replace_day_locked(&mut conn, day)
            .await
            .expect("replace below floor"),
        None
    );
    assert_eq!(snapshot(&mut conn).await, before);

    db.cleanup().await;
}

#[tokio::test]
async fn rebuild_is_bounded_by_issue_stats_watermark() {
    let Some(db) = TestDb::setup().await else {
        panic!("TEST_DATABASE_URL unset — this test must not silently skip");
    };
    let mut conn = db.conn().await;
    open_issue_stats_gate(&mut conn).await;
    let f = seed_fixture(&db, &mut conn).await;

    // Microsecond-truncated: that is what the watermark column stores, and
    // the assertion below reads it back.
    let wm = Utc::now()
        .duration_trunc(Duration::microseconds(1))
        .expect("micro trunc");
    fold::fold_issue_stats(&mut conn, wm)
        .await
        .expect("fold")
        .expect("fold ran");
    assert_eq!(
        rollups::watermark(&mut conn, rollups::SRC_ISSUE_STATS)
            .await
            .expect("watermark"),
        wm
    );
    assert_eq!(issue_events(&mut conn, f.a).await, 5);

    // Two rows received AFTER the watermark: the live fold's, not the
    // rebuild's.
    for i in 1..=2 {
        error_received(
            &mut conn,
            f.app,
            Some(f.env_a),
            f.a,
            Some("1.0"),
            f.h + Duration::minutes(20),
            wm + Duration::seconds(i),
        )
        .await;
    }
    fold::fold_day_from_raw(&mut conn, f.h.date_naive(), None, true, 2000)
        .await
        .expect("rebuild");
    assert_eq!(
        issue_events(&mut conn, f.a).await,
        5,
        "the rebuild must not count rows received after the issue_stats watermark"
    );

    let out = fold::fold_issue_stats(&mut conn, wm + Duration::seconds(5))
        .await
        .expect("fold after rebuild")
        .expect("fold ran");
    assert_eq!(out.rows_read, 2);
    assert_eq!(issue_events(&mut conn, f.a).await, 7);

    // And once the watermark has passed them, a rebuild counts them — once.
    fold::fold_day_from_raw(&mut conn, f.h.date_naive(), None, true, 2000)
        .await
        .expect("rebuild after fold");
    assert_eq!(issue_events(&mut conn, f.a).await, 7);
    assert_eq!(snapshot(&mut conn).await, raw_oracle(&mut conn).await);

    db.cleanup().await;
}

/// The sweep's issue_stats entry counts only issue-stats-READY apps, on BOTH
/// sides: an app that predates the epoch has partial rows (live fold only)
/// until Task 8's backfill marks it, and must not drive daily rebuilds
/// meanwhile.
///
/// Two days exercise the two predicates separately:
/// - yesterday: rows the live fold DOES pick up, so the not-ready app has
///   rolled rows but (restricted) raw is 0 — only the ROLLED-side predicate
///   keeps that from reading as drift;
/// - three days back: stragglers the fold steps over, so raw has rows and
///   rolled has none — only the RAW-side predicate keeps that quiet, and the
///   gap must surface once the app is marked ready.
#[tokio::test]
async fn consistency_ignores_apps_not_yet_ready() {
    let Some(db) = TestDb::setup().await else {
        panic!("TEST_DATABASE_URL unset — this test must not silently skip");
    };
    let mut conn = db.conn().await;
    open_issue_stats_gate(&mut conn).await;
    let ids = db.seed_two_envs().await;
    let old_issue = issue(&mut conn, ids.app_id).await;
    diesel::sql_query("UPDATE apps SET created_at = now() - interval '30 days' WHERE id = $1")
        .bind::<SqlUuid, _>(ids.app_id)
        .execute(&mut conn)
        .await
        .expect("age the app past the epoch");

    let folded_at = noon_utc(1);
    let folded_day = folded_at.date_naive();
    let folded_day_start = folded_day.and_hms_opt(0, 0, 0).expect("valid").and_utc();
    let late_at = noon_utc(3);
    let late_day = late_at.date_naive();
    for i in 0..8 {
        let off = Duration::seconds(i);
        error(
            &mut conn,
            ids.app_id,
            None,
            old_issue,
            None,
            folded_at + off,
        )
        .await;
        error(&mut conn, ids.app_id, None, old_issue, None, late_at + off).await;
    }
    fold::fold_issue_stats(&mut conn, Utc::now())
        .await
        .expect("fold")
        .expect("fold ran");
    // The not-ready app really does have rolled rows on the folded day (and
    // none on the straggler day).
    let r: SumRow = diesel::sql_query(
        "SELECT COALESCE(sum(events), 0)::bigint AS n FROM issue_stats_hourly \
         WHERE app_id = $1 AND hour >= $2 AND hour < $3",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .bind::<Timestamptz, _>(folded_day_start)
    .bind::<Timestamptz, _>(folded_day_start + Duration::days(1))
    .get_result(&mut conn)
    .await
    .expect("rolled rows on the folded day");
    assert_eq!(r.n, 8);
    assert_eq!(issue_events(&mut conn, old_issue).await, 8);

    let drifts = fold::consistency_check_trailing(&mut conn)
        .await
        .expect("sweep");
    assert!(
        !issue_stats_drift_on(&drifts, folded_day),
        "a not-ready app's ROLLED rows must not register as drift: {drifts:?}"
    );
    assert!(
        !issue_stats_drift_on(&drifts, late_day),
        "a not-ready app's RAW rows must not register as drift: {drifts:?}"
    );

    diesel::sql_query(
        "INSERT INTO issue_stats_backfill (app_id, covered_from) VALUES ($1, now() - interval '30 days')",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .execute(&mut conn)
    .await
    .expect("mark backfilled");
    let drifts = fold::consistency_check_trailing(&mut conn)
        .await
        .expect("sweep after marker");
    assert!(
        !issue_stats_drift_on(&drifts, folded_day),
        "once ready, the folded day agrees on both sides: {drifts:?}"
    );
    assert!(
        issue_stats_drift_on(&drifts, late_day),
        "once marked ready the straggler gap must drift: {drifts:?}"
    );

    db.cleanup().await;
}

/// `replace_day`'s per-app form (Task 8's backfill unit of work): it replaces
/// exactly that app's slice of the day and leaves every other app's rows —
/// even wrong ones — untouched.
#[tokio::test]
async fn replace_day_for_one_app_leaves_other_apps_alone() {
    let Some(db) = TestDb::setup().await else {
        panic!("TEST_DATABASE_URL unset — this test must not silently skip");
    };
    let mut conn = db.conn().await;
    open_issue_stats_gate(&mut conn).await;
    let ids = db.seed_two_envs().await;
    let suffix = Uuid::new_v4().simple().to_string();
    let app2 = sauron_db::repo::create_app(
        &mut conn,
        ids.project_id,
        "issue stats app 2",
        &format!("issue-stats-2-{suffix}"),
        "web",
    )
    .await
    .expect("second app")
    .id;
    let issue1 = issue(&mut conn, ids.app_id).await;
    let issue2 = issue(&mut conn, app2).await;
    let h = hour_of(Utc::now() - Duration::hours(5));
    for i in 0..3 {
        error(
            &mut conn,
            ids.app_id,
            None,
            issue1,
            None,
            h + Duration::minutes(i),
        )
        .await;
    }
    for i in 0..2 {
        error(
            &mut conn,
            app2,
            None,
            issue2,
            Some("9.9"),
            h + Duration::minutes(i),
        )
        .await;
    }
    fold::fold_issue_stats(&mut conn, Utc::now())
        .await
        .expect("fold")
        .expect("fold ran");
    assert_eq!(snapshot(&mut conn).await, raw_oracle(&mut conn).await);

    // Corrupt BOTH apps, then replace only app 1's day.
    exec(
        &mut conn,
        "UPDATE issue_stats_hourly SET events = events + 100",
    )
    .await;
    let day = h.date_naive();
    let inserted = rollups::issue_stats::replace_app_day_locked(&mut conn, ids.app_id, day)
        .await
        .expect("replace app 1's day")
        .expect("day is hot");

    let oracle = raw_oracle(&mut conn).await;
    let in_day = |r: &StatRow| r.hour.date_naive() == day;
    let app1_oracle: Vec<_> = oracle
        .iter()
        .filter(|r| r.app_id == ids.app_id && in_day(r))
        .cloned()
        .collect();
    let after = snapshot(&mut conn).await;
    let app1_after: Vec<_> = after
        .iter()
        .filter(|r| r.app_id == ids.app_id && in_day(r))
        .cloned()
        .collect();
    assert_eq!(app1_after, app1_oracle, "app 1's day is recounted from raw");
    assert_eq!(inserted, app1_oracle.len() as u64);
    assert_eq!(issue_events(&mut conn, issue1).await, 3);
    assert_eq!(
        issue_events(&mut conn, issue2).await,
        102,
        "app 2's rows must be left exactly as they were"
    );

    db.cleanup().await;
}

/// Make every existing app pre-epoch: epoch one hour ahead of now, watermark
/// at the epoch. Rows with `received_at` before it belong to the backfill,
/// later ones to the live fold.
async fn make_apps_old(conn: &mut sauron_db::PgConn) -> DateTime<Utc> {
    // The seeded app's own error rows would blur every count below.
    exec(conn, "DELETE FROM error_events").await;
    exec(
        conn,
        "UPDATE issue_stats_epoch SET started_at = now() + interval '1 hour'",
    )
    .await;
    exec(
        conn,
        "UPDATE rollup_watermarks SET watermark = (SELECT started_at FROM issue_stats_epoch) \
         WHERE source = 'issue_stats'",
    )
    .await;
    rollups::issue_stats::epoch(conn).await.expect("epoch")
}

async fn total_events(conn: &mut sauron_db::PgConn, app: Uuid) -> i64 {
    snapshot(conn)
        .await
        .iter()
        .filter(|r| r.app_id == app)
        .map(|r| r.events)
        .sum()
}

async fn raw_count(conn: &mut sauron_db::PgConn, app: Uuid) -> i64 {
    #[derive(QueryableByName)]
    struct N {
        #[diesel(sql_type = BigInt)]
        n: i64,
    }
    let r: N =
        diesel::sql_query("SELECT count(*)::bigint AS n FROM error_events WHERE app_id = $1")
            .bind::<SqlUuid, _>(app)
            .get_result(conn)
            .await
            .expect("count");
    r.n
}

#[tokio::test]
async fn apps_created_after_epoch_are_ready_without_backfill() {
    let Some(db) = TestDb::setup().await else {
        panic!("TEST_DATABASE_URL unset");
    };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs().await;
    exec(
        &mut conn,
        "UPDATE issue_stats_epoch SET started_at = now() - interval '1 day'",
    )
    .await;
    assert!(rollups::issue_stats::is_ready(&mut conn, ids.app_id)
        .await
        .expect("ready"));
    // Ready by epoch, no marker: complete for every hour (ruling R19).
    assert_eq!(
        rollups::issue_stats::comparison_floor(&mut conn, ids.app_id)
            .await
            .expect("floor"),
        None
    );
    db.cleanup().await;
}

#[tokio::test]
async fn old_app_not_ready_until_backfill_then_ready_with_coverage() {
    let Some(db) = TestDb::setup().await else {
        panic!("TEST_DATABASE_URL unset");
    };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs().await;
    let epoch = make_apps_old(&mut conn).await;
    let issue1 = issue(&mut conn, ids.app_id).await;
    let at = noon_utc(3);
    error_received(
        &mut conn,
        ids.app_id,
        None,
        issue1,
        None,
        at,
        epoch - Duration::days(3),
    )
    .await;

    assert!(!rollups::issue_stats::is_ready(&mut conn, ids.app_id)
        .await
        .expect("r"));
    // Not ready: only rows received since the epoch are counted.
    assert_eq!(
        rollups::issue_stats::comparison_floor(&mut conn, ids.app_id)
            .await
            .expect("c"),
        Some(epoch)
    );

    let mut days = Vec::new();
    let mut pending = None;
    let n = rollups::issue_stats::backfill_all(db.pool(), |p| match p {
        rollups::issue_stats::Progress::Pending(k) => pending = Some(k),
        rollups::issue_stats::Progress::Day(a, d) if a == ids.app_id => days.push(d),
        _ => {}
    })
    .await
    .expect("backfill");
    assert!(pending.is_some_and(|k| k >= 1), "{pending:?}");
    assert!(n >= 1);
    assert!(!days.is_empty());
    assert_eq!(days[0], at.date_naive());
    assert_eq!(days.last().copied(), Some(Utc::now().date_naive()));

    assert!(rollups::issue_stats::is_ready(&mut conn, ids.app_id)
        .await
        .expect("r"));
    assert_eq!(
        rollups::issue_stats::comparison_floor(&mut conn, ids.app_id)
            .await
            .expect("c"),
        Some(at.date_naive().and_hms_opt(0, 0, 0).unwrap().and_utc())
    );
    // Nothing left to backfill: a second run finds no pending app.
    let mut pending = None;
    let again = rollups::issue_stats::backfill_all(db.pool(), |p| {
        if let rollups::issue_stats::Progress::Pending(k) = p {
            pending = Some(k)
        }
    })
    .await
    .expect("second backfill");
    assert_eq!((again, pending), (0, Some(0)));
    assert_eq!(total_events(&mut conn, ids.app_id).await, 1);
    db.cleanup().await;
}

#[tokio::test]
async fn backfill_with_no_rows_marks_ready_at_epoch() {
    let Some(db) = TestDb::setup().await else {
        panic!("TEST_DATABASE_URL unset");
    };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs().await;
    let epoch = make_apps_old(&mut conn).await;
    rollups::issue_stats::backfill_all(db.pool(), |_| {})
        .await
        .expect("backfill");
    assert_eq!(
        rollups::issue_stats::comparison_floor(&mut conn, ids.app_id)
            .await
            .expect("c"),
        Some(epoch)
    );
    db.cleanup().await;
}

#[tokio::test]
async fn backfill_then_live_fold_counts_each_row_once() {
    let Some(db) = TestDb::setup().await else {
        panic!("TEST_DATABASE_URL unset");
    };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs().await;
    let epoch = make_apps_old(&mut conn).await;
    let issue1 = issue(&mut conn, ids.app_id).await;
    let at = noon_utc(1);
    // Received before the epoch: the backfill's.
    for i in 0..4 {
        error_received(
            &mut conn,
            ids.app_id,
            None,
            issue1,
            Some("1.0"),
            at + Duration::minutes(i),
            epoch - Duration::hours(5),
        )
        .await;
    }
    // Received after the epoch: the live fold's.
    for i in 0..3 {
        error_received(
            &mut conn,
            ids.app_id,
            Some(ids.env_a),
            issue1,
            None,
            at + Duration::minutes(i),
            epoch + Duration::minutes(10),
        )
        .await;
    }
    // Fold first (counts the 3 post-epoch rows), then backfill (must replace,
    // not add), then fold again (nothing new).
    fold::fold_issue_stats(&mut conn, epoch + Duration::hours(1))
        .await
        .expect("fold");
    rollups::issue_stats::backfill_all(db.pool(), |_| {})
        .await
        .expect("backfill");
    fold::fold_issue_stats(&mut conn, epoch + Duration::hours(1))
        .await
        .expect("fold");
    assert_eq!(raw_count(&mut conn, ids.app_id).await, 7);
    assert_eq!(total_events(&mut conn, ids.app_id).await, 7);
    db.cleanup().await;
}

#[tokio::test]
async fn backfill_is_idempotent() {
    let Some(db) = TestDb::setup().await else {
        panic!("TEST_DATABASE_URL unset");
    };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs().await;
    let epoch = make_apps_old(&mut conn).await;
    let issue1 = issue(&mut conn, ids.app_id).await;
    let at = noon_utc(2);
    for i in 0..5 {
        error_received(
            &mut conn,
            ids.app_id,
            None,
            issue1,
            None,
            at + Duration::minutes(i),
            epoch - Duration::hours(2),
        )
        .await;
    }
    let p = db.pool();
    rollups::issue_stats::backfill_all(p, |_| {})
        .await
        .expect("1");
    let first = snapshot(&mut conn).await;
    exec(&mut conn, "DELETE FROM issue_stats_backfill").await;
    let n = rollups::issue_stats::backfill_all(p, |_| {})
        .await
        .expect("2");
    assert!(n >= 1);
    assert_eq!(snapshot(&mut conn).await, first);
    assert_eq!(total_events(&mut conn, ids.app_id).await, 5);
    db.cleanup().await;
}

#[tokio::test]
async fn backfill_ignores_rows_received_after_watermark_until_fold() {
    let Some(db) = TestDb::setup().await else {
        panic!("TEST_DATABASE_URL unset");
    };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs().await;
    let epoch = make_apps_old(&mut conn).await;
    let issue1 = issue(&mut conn, ids.app_id).await;
    let at = noon_utc(1);
    for i in 0..2 {
        error_received(
            &mut conn,
            ids.app_id,
            None,
            issue1,
            None,
            at + Duration::minutes(i),
            epoch - Duration::hours(1),
        )
        .await;
    }
    for i in 0..3 {
        error_received(
            &mut conn,
            ids.app_id,
            None,
            issue1,
            None,
            at + Duration::minutes(i),
            epoch + Duration::minutes(5),
        )
        .await;
    }
    rollups::issue_stats::backfill_all(db.pool(), |_| {})
        .await
        .expect("backfill");
    assert_eq!(
        total_events(&mut conn, ids.app_id).await,
        2,
        "post-watermark rows wait for the fold"
    );
    fold::fold_issue_stats(&mut conn, epoch + Duration::hours(1))
        .await
        .expect("fold");
    assert_eq!(total_events(&mut conn, ids.app_id).await, 5);
    db.cleanup().await;
}

/// R18: days below the error_events tier floor are never replaced. A stray
/// below-floor row and a restored row must not pull `from_day` down, and a
/// fold-counted rollup row on a dropped day must survive.
#[tokio::test]
async fn backfill_never_replaces_days_below_the_tier_floor() {
    let Some(db) = TestDb::setup().await else {
        panic!("TEST_DATABASE_URL unset");
    };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs().await;
    let epoch = make_apps_old(&mut conn).await;
    let issue1 = issue(&mut conn, ids.app_id).await;
    let floor_day = noon_utc(5).date_naive();
    let floor = floor_day.and_hms_opt(0, 0, 0).unwrap().and_utc();
    exec(
        &mut conn,
        &format!(
            "INSERT INTO tiering_state (table_name, watermark, dropped_thru, updated_at) \
             VALUES ('error_events', now(), '{}', now()) \
             ON CONFLICT (table_name) DO UPDATE SET dropped_thru = EXCLUDED.dropped_thru",
            floor.to_rfc3339()
        ),
    )
    .await;
    // Stray below the floor (a late arrival into a dropped range).
    let dropped_at = floor - Duration::days(2) + Duration::hours(12);
    error_received(
        &mut conn,
        ids.app_id,
        None,
        issue1,
        None,
        dropped_at,
        epoch - Duration::days(1),
    )
    .await;
    // The fold-counted rollup row for that dropped day, whose raw rows are gone.
    diesel::sql_query(
        "INSERT INTO issue_stats_hourly (app_id, issue_id, environment_id, release, hour, events) \
         VALUES ($1, $2, NULL, '', $3, 40)",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .bind::<SqlUuid, _>(issue1)
    .bind::<Timestamptz, _>(hour_of(dropped_at))
    .execute(&mut conn)
    .await
    .expect("rollup row");
    // A hot row after the floor.
    let hot_at = floor + Duration::hours(12);
    error_received(
        &mut conn,
        ids.app_id,
        None,
        issue1,
        None,
        hot_at,
        epoch - Duration::days(1),
    )
    .await;

    rollups::issue_stats::backfill_all(db.pool(), |_| {})
        .await
        .expect("backfill");

    assert_eq!(
        issue_events_in_hour(&mut conn, issue1, hour_of(dropped_at)).await,
        40,
        "the fold-counted row on a dropped day survives"
    );
    assert_eq!(
        issue_events_in_hour(&mut conn, issue1, hour_of(hot_at)).await,
        1
    );
    assert_eq!(
        rollups::issue_stats::comparison_floor(&mut conn, ids.app_id)
            .await
            .expect("c"),
        Some(floor)
    );
    db.cleanup().await;
}

async fn issue_events_in_hour(
    conn: &mut sauron_db::PgConn,
    issue: Uuid,
    hour: DateTime<Utc>,
) -> i64 {
    #[derive(QueryableByName)]
    struct N {
        #[diesel(sql_type = BigInt)]
        n: i64,
    }
    let r: N = diesel::sql_query(
        "SELECT COALESCE(sum(events), 0)::bigint AS n FROM issue_stats_hourly \
         WHERE issue_id = $1 AND hour = $2",
    )
    .bind::<SqlUuid, _>(issue)
    .bind::<Timestamptz, _>(hour)
    .get_result(conn)
    .await
    .expect("hour sum");
    r.n
}

// --- reads: `ranked`, `events_for`, `issue_users_in_window`, `issues_by_ids` --
//
// What MCP `trending_issues` reads. Each test gets its own fresh app with two
// enrolled environments and no telemetry, so every sum below is exact (the
// harness app from `seed_two_envs` carries seeded errors of its own).

struct ReadFixture {
    app: Uuid,
    env_a: Uuid,
    env_b: Uuid,
}

async fn empty_app_with_two_envs(conn: &mut sauron_db::PgConn) -> ReadFixture {
    let s = Uuid::new_v4().simple().to_string();
    let org = sauron_db::repo::create_org(conn, "reads org", &format!("reads-org-{s}"))
        .await
        .expect("create org");
    let project = sauron_db::repo::create_project(conn, org.id, "reads", &format!("reads-{s}"))
        .await
        .expect("create project");
    let app = sauron_db::repo::create_app(
        conn,
        project.id,
        "reads app",
        &format!("reads-app-{s}"),
        "web",
    )
    .await
    .expect("create app");
    let env_a = common::seed_env(
        conn,
        project.id,
        app.id,
        "production",
        &format!("pk_a_{s}"),
        true,
    )
    .await;
    let env_b = common::seed_env(
        conn,
        project.id,
        app.id,
        "staging",
        &format!("pk_b_{s}"),
        false,
    )
    .await;
    ReadFixture {
        app: app.id,
        env_a,
        env_b,
    }
}

/// [`error`] with a `distinct_id`, for the users count.
async fn error_by(
    conn: &mut sauron_db::PgConn,
    app: Uuid,
    env: Option<Uuid>,
    issue: Uuid,
    release: Option<&str>,
    at: DateTime<Utc>,
    distinct_id: Option<&str>,
) {
    diesel::sql_query(
        "INSERT INTO error_events (app_id, environment_id, issue_id, fingerprint, release, occurred_at, distinct_id) \
         VALUES ($1, $2, $3, 'issue-stats-fp', $4, $5, $6)",
    )
    .bind::<SqlUuid, _>(app)
    .bind::<Nullable<SqlUuid>, _>(env)
    .bind::<SqlUuid, _>(issue)
    .bind::<Nullable<Text>, _>(release.map(str::to_string))
    .bind::<Timestamptz, _>(at)
    .bind::<Nullable<Text>, _>(distinct_id.map(str::to_string))
    .execute(conn)
    .await
    .expect("insert error event (with distinct_id)");
}

async fn fold_everything(conn: &mut sauron_db::PgConn) {
    fold::fold_issue_stats(conn, Utc::now() + Duration::seconds(1))
        .await
        .expect("fold issue stats")
        .expect("watermark was behind upto");
}

fn pairs(v: Vec<rollups::issue_stats::RankedIssue>) -> Vec<(Uuid, i64)> {
    v.into_iter().map(|r| (r.issue_id, r.events)).collect()
}

/// `ranked` sums the window's hourly cells per issue, narrowed by every
/// filter the tool passes (environment in all four shapes, release with the
/// `''` = unknown key, status through `issues`, the query's candidate set),
/// ordered `events DESC, issue_id`. `events_for` reads the same cells for a
/// given set of issues, which is how the previous window is counted.
#[tokio::test]
async fn ranked_sums_window_by_env_release_status_and_candidates() {
    use rollups::issue_stats::{events_for, ranked};
    use sauron_db::scope::{EnvFilter, Range, ReadScope};

    let Some(db) = TestDb::setup().await else {
        panic!("TEST_DATABASE_URL unset — this test must not silently skip");
    };
    let mut conn = db.conn().await;
    open_issue_stats_gate(&mut conn).await;
    let f = empty_app_with_two_envs(&mut conn).await;
    let (a, b, c) = (
        issue(&mut conn, f.app).await,
        issue(&mut conn, f.app).await,
        issue(&mut conn, f.app).await,
    );
    exec(
        &mut conn,
        &format!("UPDATE issues SET status = 'resolved' WHERE id = '{c}'"),
    )
    .await;

    let now = Utc::now();
    let in_window = now - Duration::hours(2);
    for _ in 0..3 {
        error(&mut conn, f.app, Some(f.env_a), a, Some("1.0"), in_window).await;
    }
    for _ in 0..2 {
        error(&mut conn, f.app, Some(f.env_b), a, None, in_window).await;
    }
    for _ in 0..3 {
        error(&mut conn, f.app, Some(f.env_a), b, Some("2.0"), in_window).await;
    }
    error(&mut conn, f.app, None, b, Some("2.0"), in_window).await;
    for _ in 0..6 {
        error(&mut conn, f.app, Some(f.env_a), c, Some("1.0"), in_window).await;
    }
    // The previous 24h only: never part of the current window's sums.
    error(
        &mut conn,
        f.app,
        Some(f.env_a),
        a,
        Some("1.0"),
        now - Duration::hours(30),
    )
    .await;
    fold_everything(&mut conn).await;

    let ws = hour_of(now - Duration::hours(24));
    let cur = Range::since(ws);
    let prev = Range::new(ws - Duration::hours(24), Some(ws));
    let all = ReadScope::all(f.app);
    let unresolved = Some("unresolved");

    let got = ranked(&mut conn, &all, None, cur, unresolved, None, 10).await;
    assert_eq!(pairs(got.expect("ranked")), vec![(a, 5), (b, 4)]);
    let got = ranked(&mut conn, &all, None, cur, None, None, 10).await;
    assert_eq!(pairs(got.expect("ranked")), vec![(c, 6), (a, 5), (b, 4)]);
    let got = ranked(&mut conn, &all, None, cur, None, None, 1).await;
    assert_eq!(pairs(got.expect("limit")), vec![(c, 6)]);

    // Environments. A tie (3 and 3 in env_a) orders by issue id.
    let one_a = ReadScope::new(f.app, EnvFilter::One(f.env_a));
    let mut tied = vec![(a, 3), (b, 3)];
    tied.sort_by_key(|(id, _)| *id);
    let mut want = vec![(c, 6)];
    want.extend(tied);
    let got = ranked(&mut conn, &one_a, None, cur, None, None, 10).await;
    assert_eq!(pairs(got.expect("env One")), want);
    let subset_b = ReadScope::new(f.app, EnvFilter::Subset(vec![f.env_b]));
    let got = ranked(&mut conn, &subset_b, None, cur, None, None, 10).await;
    assert_eq!(pairs(got.expect("env Subset")), vec![(a, 2)]);
    let unattributed = ReadScope::new(f.app, EnvFilter::Unattributed);
    let got = ranked(&mut conn, &unattributed, None, cur, None, None, 10).await;
    assert_eq!(pairs(got.expect("env Unattributed")), vec![(b, 1)]);
    // Environment and release together: both binds land in the right slots.
    let got = ranked(&mut conn, &one_a, Some("1.0"), cur, unresolved, None, 10).await;
    assert_eq!(pairs(got.expect("env + release")), vec![(a, 3)]);

    // Releases: a named one, and '' for "no release".
    let got = ranked(&mut conn, &all, Some("1.0"), cur, unresolved, None, 10).await;
    assert_eq!(pairs(got.expect("release 1.0")), vec![(a, 3)]);
    let got = ranked(&mut conn, &all, Some(""), cur, unresolved, None, 10).await;
    assert_eq!(pairs(got.expect("unknown release")), vec![(a, 2)]);
    let got = ranked(&mut conn, &all, Some("2.0"), cur, None, None, 10).await;
    assert_eq!(pairs(got.expect("release 2.0")), vec![(b, 4)]);

    // Candidates (the `query` argument's matches), with every other bind set.
    let got = ranked(&mut conn, &all, None, cur, None, Some(&[b, c]), 10).await;
    assert_eq!(pairs(got.expect("candidates")), vec![(c, 6), (b, 4)]);
    let got = ranked(
        &mut conn,
        &one_a,
        Some("2.0"),
        cur,
        unresolved,
        Some(&[b]),
        10,
    )
    .await;
    assert_eq!(pairs(got.expect("every bind")), vec![(b, 3)]);
    let got = ranked(&mut conn, &all, None, cur, None, Some(&[]), 10).await;
    assert_eq!(pairs(got.expect("no candidates")), vec![]);

    // The previous window: the one row 30h back, and nothing else.
    let got = ranked(&mut conn, &all, None, prev, None, None, 10).await;
    assert_eq!(pairs(got.expect("prev ranked")), vec![(a, 1)]);
    let got = events_for(&mut conn, &all, None, prev, &[a, b, c])
        .await
        .expect("prev events_for");
    assert_eq!(got, [(a, 1)].into_iter().collect());
    let got = events_for(&mut conn, &one_a, Some("1.0"), cur, &[a, c])
        .await
        .expect("events_for env + release");
    assert_eq!(got, [(a, 3), (c, 6)].into_iter().collect());
    assert!(events_for(&mut conn, &all, None, cur, &[])
        .await
        .expect("events_for no ids")
        .is_empty());

    // Another app's issue never comes back, whatever ids are asked for.
    let other = empty_app_with_two_envs(&mut conn).await;
    let foreign = issue(&mut conn, other.app).await;
    error(&mut conn, other.app, None, foreign, None, in_window).await;
    fold_everything(&mut conn).await;
    let got = events_for(&mut conn, &all, None, cur, &[a, foreign])
        .await
        .expect("events_for foreign id");
    assert_eq!(got, [(a, 5)].into_iter().collect());
    let mut rows: Vec<Uuid> = sauron_db::repo::issues_by_ids(&mut conn, f.app, &[a, c, foreign])
        .await
        .expect("issues_by_ids")
        .into_iter()
        .map(|i| i.id)
        .collect();
    rows.sort();
    let mut want = vec![a, c];
    want.sort();
    assert_eq!(rows, want);
    assert!(sauron_db::repo::issues_by_ids(&mut conn, f.app, &[])
        .await
        .expect("issues_by_ids no ids")
        .is_empty());

    db.cleanup().await;
}

/// Users are counted on read from `error_events` (distinct `distinct_id`,
/// NULL not a user), under the same filters as the events. A count that
/// outlives its statement timeout is `Ok(None)` ("unknown"), not an error,
/// and leaves the connection exactly as it found it.
#[tokio::test]
async fn issue_users_in_window_counts_distinct_and_times_out_to_none() {
    use diesel_async::SimpleAsyncConnection;
    use sauron_db::repo::issue_users_in_window;
    use sauron_db::scope::{EnvFilter, Range, ReadScope};

    let Some(db) = TestDb::setup().await else {
        panic!("TEST_DATABASE_URL unset — this test must not silently skip");
    };
    let mut conn = db.conn().await;
    let f = empty_app_with_two_envs(&mut conn).await;
    let (a, b) = (issue(&mut conn, f.app).await, issue(&mut conn, f.app).await);
    let now = Utc::now();
    let in_window = now - Duration::hours(2);
    for _ in 0..3 {
        error_by(
            &mut conn,
            f.app,
            Some(f.env_a),
            a,
            None,
            in_window,
            Some("u1"),
        )
        .await;
    }
    error_by(
        &mut conn,
        f.app,
        Some(f.env_b),
        a,
        None,
        in_window,
        Some("u2"),
    )
    .await;
    error_by(&mut conn, f.app, Some(f.env_a), a, None, in_window, None).await;
    // Before the window: not counted.
    error_by(
        &mut conn,
        f.app,
        Some(f.env_a),
        a,
        None,
        now - Duration::hours(30),
        Some("u9"),
    )
    .await;
    error_by(
        &mut conn,
        f.app,
        Some(f.env_a),
        b,
        Some("2.0"),
        in_window,
        Some("u1"),
    )
    .await;

    let cur = Range::since(hour_of(now - Duration::hours(24)));
    let all = ReadScope::all(f.app);
    let users = |m: Option<std::collections::HashMap<Uuid, i64>>| {
        let m = m.expect("counted in time");
        (m.get(&a).copied(), m.get(&b).copied())
    };

    let got = issue_users_in_window(&mut conn, &all, None, &[a, b], cur, 5000).await;
    assert_eq!(users(got.expect("all")), (Some(2), Some(1)));
    let one_a = ReadScope::new(f.app, EnvFilter::One(f.env_a));
    let got = issue_users_in_window(&mut conn, &one_a, None, &[a, b], cur, 5000).await;
    assert_eq!(users(got.expect("env")), (Some(1), Some(1)));
    let got = issue_users_in_window(&mut conn, &all, Some("2.0"), &[a, b], cur, 5000).await;
    assert_eq!(users(got.expect("release")), (Some(0), Some(1)));
    let got = issue_users_in_window(&mut conn, &all, Some(""), &[a, b], cur, 5000).await;
    assert_eq!(users(got.expect("unknown release")), (Some(2), Some(0)));
    let wide = Range::since(now - Duration::hours(48));
    let got = issue_users_in_window(&mut conn, &all, None, &[a], wide, 5000).await;
    assert_eq!(users(got.expect("wider window")).0, Some(3));
    let got = issue_users_in_window(&mut conn, &all, None, &[], cur, 5000).await;
    assert!(got.expect("no ids").expect("no ids").is_empty());

    #[derive(QueryableByName)]
    struct Setting {
        #[diesel(sql_type = Text)]
        statement_timeout: String,
    }
    let before: Setting = diesel::sql_query("SHOW statement_timeout")
        .get_result(&mut conn)
        .await
        .expect("show before");

    // A second session holds an exclusive lock on `issues`, so the count
    // waits past its 200 ms statement timeout and Postgres cancels it.
    let mut locker = db.extra_conn().await;
    locker
        .batch_execute("BEGIN; LOCK TABLE issues IN ACCESS EXCLUSIVE MODE")
        .await
        .expect("take the lock");
    let got = issue_users_in_window(&mut conn, &all, None, &[a], cur, 200).await;
    assert!(
        matches!(got, Ok(None)),
        "a cancelled count is unknown, not an error: {got:?}"
    );
    locker.batch_execute("ROLLBACK").await.expect("release");
    drop(locker);

    // Rolled back, and the LOCAL timeout went with the transaction.
    let after: Setting = diesel::sql_query("SHOW statement_timeout")
        .get_result(&mut conn)
        .await
        .expect("show after");
    assert_eq!(after.statement_timeout, before.statement_timeout);
    let got = issue_users_in_window(&mut conn, &all, None, &[a, b], cur, 5000).await;
    assert_eq!(users(got.expect("after")), (Some(2), Some(1)));

    db.cleanup().await;
}

/// What a two-window comparison may trust (ruling R19). An app created after
/// the epoch is complete for every hour (`None`); a backfilled app is
/// complete from its marker's `covered_from`; an app that is not ready at
/// all only from the epoch.
#[tokio::test]
async fn comparison_floor_trusts_post_epoch_apps_fully() {
    use rollups::issue_stats::{comparison_floor, epoch};

    let Some(db) = TestDb::setup().await else {
        panic!("TEST_DATABASE_URL unset — this test must not silently skip");
    };
    let mut conn = db.conn().await;
    open_issue_stats_gate(&mut conn).await;
    let fresh = empty_app_with_two_envs(&mut conn).await.app;
    let backfilled = empty_app_with_two_envs(&mut conn).await.app;
    let pending = empty_app_with_two_envs(&mut conn).await.app;
    exec(
        &mut conn,
        &format!(
            "UPDATE apps SET created_at = now() - interval '10 days' \
             WHERE id IN ('{backfilled}', '{pending}')"
        ),
    )
    .await;
    let covered_from = noon_utc(5);
    diesel::sql_query("INSERT INTO issue_stats_backfill (app_id, covered_from) VALUES ($1, $2)")
        .bind::<SqlUuid, _>(backfilled)
        .bind::<Timestamptz, _>(covered_from)
        .execute(&mut conn)
        .await
        .expect("insert marker");
    let epoch = epoch(&mut conn).await.expect("epoch");

    assert_eq!(
        comparison_floor(&mut conn, fresh).await.expect("fresh"),
        None
    );
    assert_eq!(
        comparison_floor(&mut conn, backfilled)
            .await
            .expect("backfilled"),
        Some(covered_from)
    );
    assert_eq!(
        comparison_floor(&mut conn, pending).await.expect("pending"),
        Some(epoch)
    );

    db.cleanup().await;
}

/// `release_error_counts` sums events and counts distinct issues per release
/// in the window; `new_issues_by_release` attributes each issue first seen
/// since the bound to the release of its earliest rollup row. Both honour the
/// environment filter, and a pre-bound issue is never "new".
#[tokio::test]
async fn release_error_counts_and_new_issues_by_release() {
    use rollups::issue_stats::{new_issues_by_release, release_error_counts, ReleaseErrors};
    use sauron_db::scope::{EnvFilter, Range, ReadScope};

    let Some(db) = TestDb::setup().await else {
        panic!("TEST_DATABASE_URL unset — this test must not silently skip");
    };
    let mut conn = db.conn().await;
    open_issue_stats_gate(&mut conn).await;
    let f = empty_app_with_two_envs(&mut conn).await;
    let (a, b, c) = (
        issue(&mut conn, f.app).await,
        issue(&mut conn, f.app).await,
        issue(&mut conn, f.app).await,
    );
    exec(
        &mut conn,
        &format!("UPDATE issues SET first_seen = now() - interval '10 days' WHERE id = '{c}'"),
    )
    .await;
    let now = Utc::now();
    // a: first seen in 1.0 (env_a), later also 2.0 (env_b). b: only 2.0.
    // c: an old issue, active in 2.0 inside the window.
    for _ in 0..2 {
        error(
            &mut conn,
            f.app,
            Some(f.env_a),
            a,
            Some("1.0"),
            now - Duration::hours(6),
        )
        .await;
    }
    error(
        &mut conn,
        f.app,
        Some(f.env_b),
        a,
        Some("2.0"),
        now - Duration::hours(5),
    )
    .await;
    for _ in 0..3 {
        error(
            &mut conn,
            f.app,
            Some(f.env_a),
            b,
            Some("2.0"),
            now - Duration::hours(2),
        )
        .await;
    }
    for _ in 0..4 {
        error(
            &mut conn,
            f.app,
            Some(f.env_a),
            c,
            Some("2.0"),
            now - Duration::hours(1),
        )
        .await;
    }
    fold_everything(&mut conn).await;

    let re = |release: &str, events, issues| ReleaseErrors {
        release: release.into(),
        events,
        issues,
    };
    let all = ReadScope::all(f.app);
    let day = Range::since(hour_of(now - Duration::hours(24)));
    let got = release_error_counts(&mut conn, &all, day).await;
    assert_eq!(
        got.expect("all"),
        vec![re("2.0", 8, 3), re("1.0", 2, 1)],
        "events DESC; an issue in two releases counts once in each"
    );
    let subset_b = ReadScope::new(f.app, EnvFilter::Subset(vec![f.env_b]));
    let got = release_error_counts(&mut conn, &subset_b, day).await;
    assert_eq!(got.expect("env"), vec![re("2.0", 1, 1)]);
    let recent = Range::since(hour_of(now - Duration::hours(3)));
    let got = release_error_counts(&mut conn, &all, recent).await;
    assert_eq!(got.expect("recent"), vec![re("2.0", 7, 2)]);

    let bound = now - Duration::days(1);
    let got = new_issues_by_release(&mut conn, &all, bound).await;
    assert_eq!(
        got.expect("new"),
        [("1.0".to_string(), 1), ("2.0".to_string(), 1)]
            .into_iter()
            .collect(),
        "a -> 1.0, b -> 2.0, c is older than the bound"
    );
    // Only env_b rows count for this scope: a's first row THERE is 2.0, and b
    // has none.
    let got = new_issues_by_release(&mut conn, &subset_b, bound).await;
    assert_eq!(
        got.expect("new env"),
        [("2.0".to_string(), 1)].into_iter().collect()
    );
    // A bound before c's first_seen takes it in.
    let got = new_issues_by_release(&mut conn, &all, now - Duration::days(30)).await;
    assert_eq!(
        got.expect("new wide"),
        [("1.0".to_string(), 1), ("2.0".to_string(), 2)]
            .into_iter()
            .collect()
    );
    db.cleanup().await;
}
