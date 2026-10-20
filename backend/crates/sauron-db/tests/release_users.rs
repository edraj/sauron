//! Release-scoped Users reads (migration 82): `rollups::release`.
//!
//! One small fixture with two environments, three releases and an Unknown
//! (NULL) release, every expected number worked out by hand in the comments,
//! read back through the same functions the API calls. Plus the backfill's
//! two contracts: additive against the epoch (never `DO NOTHING`) and a no-op
//! on a re-run.

mod common;

use chrono::{DateTime, Duration, Utc};
use common::TestDb;
use diesel::sql_types::{BigInt, Nullable, Text, Timestamptz, Uuid as SqlUuid};
use diesel::QueryableByName;
use diesel_async::RunQueryDsl;
use sauron_db::repo::{self, SortSpec, TimeWindow};
use sauron_db::rollups::fold;
use sauron_db::rollups::release::{self, BackfillOutcome, ReleaseMode};
use sauron_db::scope::{EnvFilter, Range, ReadScope};
use uuid::Uuid;

async fn exec(conn: &mut sauron_db::PgConn, sql: &str) {
    diesel::sql_query(sql).execute(conn).await.expect(sql);
}

struct Fixture {
    app: Uuid,
    env_a: Uuid,
    env_b: Uuid,
}

async fn seed_app(conn: &mut sauron_db::PgConn) -> Fixture {
    let suffix = Uuid::new_v4().simple().to_string();
    let org = repo::create_org(conn, "rel org", &format!("rel-org-{suffix}"))
        .await
        .expect("org");
    let project = repo::create_project(conn, org.id, "rel project", &format!("rel-p-{suffix}"))
        .await
        .expect("project");
    let app = repo::create_app(
        conn,
        project.id,
        "rel app",
        &format!("rel-a-{suffix}"),
        "web",
    )
    .await
    .expect("app");
    let env_a = common::seed_env(
        conn,
        project.id,
        app.id,
        "prod",
        &format!("pk_ra_{suffix}"),
        true,
    )
    .await;
    let env_b = common::seed_env(
        conn,
        project.id,
        app.id,
        "stage",
        &format!("pk_rb_{suffix}"),
        false,
    )
    .await;
    Fixture {
        app: app.id,
        env_a,
        env_b,
    }
}

async fn event(
    conn: &mut sauron_db::PgConn,
    app: Uuid,
    env: Uuid,
    distinct: &str,
    release: Option<&str>,
    at: DateTime<Utc>,
) {
    diesel::sql_query(
        "INSERT INTO analytics_events (app_id, environment_id, distinct_id, name, release, occurred_at) \
         VALUES ($1, $2, $3, 'tap', $4, $5)",
    )
    .bind::<SqlUuid, _>(app)
    .bind::<SqlUuid, _>(env)
    .bind::<Text, _>(distinct.to_string())
    .bind::<Nullable<Text>, _>(release.map(str::to_string))
    .bind::<Timestamptz, _>(at)
    .execute(conn)
    .await
    .expect("insert analytics event");
}

async fn error(
    conn: &mut sauron_db::PgConn,
    app: Uuid,
    env: Uuid,
    distinct: &str,
    release: Option<&str>,
    at: DateTime<Utc>,
) {
    let issue = Uuid::new_v4();
    diesel::sql_query(
        "INSERT INTO issues (id, app_id, fingerprint, type, title, culprit, level, status, \
                             first_seen, last_seen, times_seen, users_seen, last_event_at) \
         VALUES ($1, $2, $3, 'error', 't', 'c', 'error', 'unresolved', now(), now(), 1, 1, now())",
    )
    .bind::<SqlUuid, _>(issue)
    .bind::<SqlUuid, _>(app)
    .bind::<Text, _>(issue.to_string())
    .execute(conn)
    .await
    .expect("insert issue");
    diesel::sql_query(
        "INSERT INTO error_events (app_id, environment_id, issue_id, fingerprint, distinct_id, release, occurred_at) \
         VALUES ($1, $2, $6, 'rel-fp', $3, $4, $5)",
    )
    .bind::<SqlUuid, _>(app)
    .bind::<SqlUuid, _>(env)
    .bind::<Text, _>(distinct.to_string())
    .bind::<Nullable<Text>, _>(release.map(str::to_string))
    .bind::<Timestamptz, _>(at)
    .bind::<SqlUuid, _>(issue)
    .execute(conn)
    .await
    .expect("insert error event");
}

async fn txn(
    conn: &mut sauron_db::PgConn,
    app: Uuid,
    env: Uuid,
    distinct: &str,
    release: Option<&str>,
    at: DateTime<Utc>,
) {
    diesel::sql_query(
        "INSERT INTO transactions (app_id, environment_id, name, op, duration_ms, distinct_id, release, occurred_at) \
         VALUES ($1, $2, 'GET /', 'http', 12.0, $3, $4, $5)",
    )
    .bind::<SqlUuid, _>(app)
    .bind::<SqlUuid, _>(env)
    .bind::<Text, _>(distinct.to_string())
    .bind::<Nullable<Text>, _>(release.map(str::to_string))
    .bind::<Timestamptz, _>(at)
    .execute(conn)
    .await
    .expect("insert transaction");
}

#[allow(clippy::too_many_arguments)]
async fn session(
    conn: &mut sauron_db::PgConn,
    app: Uuid,
    env: Uuid,
    sid: &str,
    distinct: &str,
    release: Option<&str>,
    started: DateTime<Utc>,
    last: DateTime<Utc>,
) {
    diesel::sql_query(
        "INSERT INTO sessions (app_id, environment_id, session_id, distinct_id, release, started_at, last_event_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind::<SqlUuid, _>(app)
    .bind::<SqlUuid, _>(env)
    .bind::<Text, _>(sid.to_string())
    .bind::<Text, _>(distinct.to_string())
    .bind::<Nullable<Text>, _>(release.map(str::to_string))
    .bind::<Timestamptz, _>(started)
    .bind::<Timestamptz, _>(last)
    .execute(conn)
    .await
    .expect("insert session");
}

fn last_seen_sort() -> SortSpec {
    SortSpec {
        column: "last_seen",
        descending: true,
        tiebreak: "eu.distinct_id",
        nulls_last: false,
    }
}

async fn ids(
    conn: &mut sauron_db::PgConn,
    scope: &ReadScope,
    release: &str,
    mode: ReleaseMode,
    from: DateTime<Utc>,
) -> Vec<String> {
    let window = TimeWindow {
        column: "last_seen",
        from,
        to: None,
    };
    let rows = release::list_persons(
        conn,
        scope,
        release,
        mode,
        None,
        50,
        0,
        last_seen_sort(),
        window,
    )
    .await
    .expect("list persons");
    let (n, capped) = release::count_persons(
        conn,
        scope,
        release,
        mode,
        None,
        last_seen_sort(),
        TimeWindow {
            column: "last_seen",
            from,
            to: None,
        },
        10_000,
    )
    .await
    .expect("count persons");
    assert!(!capped);
    assert_eq!(n as usize, rows.len(), "count and list disagree");
    let mut v: Vec<String> = rows.into_iter().map(|r| r.distinct_id).collect();
    v.sort();
    v
}

/// The whole read surface over one fixture.
///
/// | person | signals (release, env, when) |
/// |---|---|
/// | alice | 1.0 prod: event today + 60 s session today. identified |
/// | bob   | 1.0 stage: error 26 h ago. 2.0 stage: event today |
/// | carol | Unknown prod: event 26 h ago. identified |
/// | dave  | 2.0 prod: event 26 h ago. 1.0 prod: transaction today |
#[tokio::test]
async fn release_reads_answer_per_release() {
    let Some(db) = TestDb::setup().await else {
        panic!("TEST_DATABASE_URL unset — this test must not silently skip");
    };
    let mut conn = db.conn().await;
    let f = seed_app(&mut conn).await;
    // Every fixture row predates the epoch, so the backfill below counts them
    // all (the live path never ran) and the app starts NOT ready.
    exec(
        &mut conn,
        "UPDATE release_rollup_epoch SET started_at = now() + interval '1 hour'",
    )
    .await;

    let now = Utc::now();
    let today = now - Duration::minutes(10);
    let yday = now - Duration::hours(26);
    for p in ["alice", "bob", "carol", "dave"] {
        repo::touch_event_user(&mut conn, f.app, p)
            .await
            .expect("touch");
    }
    common::seed_identified_user(&mut conn, f.app, "alice").await;
    common::seed_identified_user(&mut conn, f.app, "carol").await;

    event(&mut conn, f.app, f.env_a, "alice", Some("1.0"), today).await;
    session(
        &mut conn,
        f.app,
        f.env_a,
        "rel-s1",
        "alice",
        Some("1.0"),
        today,
        today + Duration::seconds(60),
    )
    .await;
    error(&mut conn, f.app, f.env_b, "bob", Some("1.0"), yday).await;
    event(&mut conn, f.app, f.env_b, "bob", Some("2.0"), today).await;
    event(&mut conn, f.app, f.env_a, "carol", None, yday).await;
    event(&mut conn, f.app, f.env_a, "dave", Some("2.0"), yday).await;
    txn(&mut conn, f.app, f.env_a, "dave", Some("1.0"), today).await;

    assert!(
        !release::is_ready(&mut conn, f.app).await.unwrap(),
        "pre-epoch app must start unready"
    );
    assert!(release::backfill_pending(&mut conn).await.unwrap());

    // Sketches and session buckets: the live fold (rows are < 2 days old, so
    // the incremental pull sees them) plus the session recompute.
    let upto = Utc::now() + Duration::seconds(1);
    fold::fold_analytics(&mut conn, upto, 2000)
        .await
        .expect("fold analytics");
    fold::fold_errors(&mut conn, upto)
        .await
        .expect("fold errors");
    fold::recompute_sessions(&mut conn, Some(now - Duration::days(3)))
        .await
        .expect("sessions");
    // person_releases + the marker.
    let cutoff = release::epoch(&mut conn).await.unwrap();
    release::backfill_app_persons(&mut conn, f.app, cutoff)
        .await
        .expect("persons backfill");
    assert!(
        release::is_ready(&mut conn, f.app).await.unwrap(),
        "marker must open the gate"
    );

    let all = ReadScope {
        app_id: f.app,
        env: EnvFilter::All,
    };
    let stage = ReadScope {
        app_id: f.app,
        env: EnvFilter::One(f.env_b),
    };
    let range = Range::since(now - Duration::days(7));

    // 1.0, all envs: alice, bob, dave are users of it; only alice identified.
    // dau = alice (dave's 1.0 signal is a transaction — not in the sketch,
    // exactly as on the release-blind page); wau = mau = alice + bob.
    let s = release::user_stats(&mut conn, &all, "1.0", range, now)
        .await
        .unwrap();
    assert_eq!(
        (s.total_users, s.active_in_range, s.new_in_range),
        (3, 3, 3)
    );
    assert_eq!(
        (s.total_identified, s.active_identified, s.new_identified),
        (1, 1, 1)
    );
    assert_eq!((s.dau, s.wau, s.mau), (1, 2, 2));
    assert_eq!(
        (s.dau_identified, s.wau_identified, s.mau_identified),
        (Some(1), Some(1), Some(1))
    );
    assert_eq!(s.avg_session_ms, 60_000.0);
    assert!(s.median_session_ms > 0.0);

    // 1.0, stage only: bob alone, active yesterday.
    let s = release::user_stats(&mut conn, &stage, "1.0", range, now)
        .await
        .unwrap();
    assert_eq!((s.total_users, s.dau, s.wau), (1, 0, 1));
    assert_eq!(s.avg_session_ms, 0.0, "alice's session is in prod");

    // Unknown release: carol only.
    let s = release::user_stats(&mut conn, &all, "", range, now)
        .await
        .unwrap();
    assert_eq!(
        (s.total_users, s.total_identified, s.dau, s.wau),
        (1, 1, 0, 1)
    );

    // 2.0: bob today, dave yesterday.
    let s = release::user_stats(&mut conn, &all, "2.0", range, now)
        .await
        .unwrap();
    assert_eq!((s.total_users, s.dau, s.wau), (2, 1, 2));

    // A window that ends before today: alice and dave are no longer "active"
    // on 1.0 (their 1.0 signals are today), bob still is.
    let s = release::user_stats(
        &mut conn,
        &all,
        "1.0",
        Range::new(now - Duration::days(7), Some(now - Duration::hours(1))),
        now,
    )
    .await
    .unwrap();
    assert_eq!(
        (s.total_users, s.active_in_range, s.new_in_range),
        (3, 1, 1)
    );

    // Series: today's active on 1.0 = alice; new-per-day sums to 3.
    let series = release::active_user_series(&mut conn, &all, "1.0", range)
        .await
        .unwrap();
    assert_eq!(series.iter().map(|p| p.new_users).sum::<i64>(), 3);
    assert_eq!(series.last().map(|p| p.active), Some(1));

    // Persons: "used it" vs "currently on it".
    let from = now - Duration::days(30);
    assert_eq!(
        ids(&mut conn, &all, "1.0", ReleaseMode::Used, from).await,
        ["alice", "bob", "dave"]
    );
    // bob moved on to 2.0 today; dave's latest signal (today's transaction) is 1.0.
    assert_eq!(
        ids(&mut conn, &all, "1.0", ReleaseMode::Current, from).await,
        ["alice", "dave"]
    );
    assert_eq!(
        ids(&mut conn, &all, "2.0", ReleaseMode::Current, from).await,
        ["bob"]
    );
    assert_eq!(
        ids(&mut conn, &all, "", ReleaseMode::Used, from).await,
        ["carol"]
    );
    // Env scope narrows "current" too: within stage, bob's latest is 2.0.
    assert_eq!(
        ids(&mut conn, &stage, "1.0", ReleaseMode::Current, from).await,
        Vec::<String>::new()
    );

    // Per-release counters on the row the list shows.
    let rows = release::list_persons(
        &mut conn,
        &all,
        "1.0",
        ReleaseMode::Used,
        Some("alice"),
        50,
        0,
        last_seen_sort(),
        TimeWindow {
            column: "last_seen",
            from,
            to: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (
            rows[0].events_count,
            rows[0].errors_count,
            rows[0].sessions_count
        ),
        (1, 0, 1)
    );

    db.cleanup().await;
}

#[derive(QueryableByName)]
struct N {
    #[diesel(sql_type = BigInt)]
    n: i64,
}

async fn scalar(conn: &mut sauron_db::PgConn, sql: &str, app: Uuid) -> i64 {
    let r: N = diesel::sql_query(sql)
        .bind::<SqlUuid, _>(app)
        .get_result(conn)
        .await
        .expect(sql);
    r.n
}

/// The orchestrated backfill: a pre-epoch day the live fold never saw lands in
/// the release tables, the counters ADD to a row the live path already wrote
/// (a `DO NOTHING` would have dropped the history), and a second run changes
/// nothing.
#[tokio::test]
async fn backfill_is_additive_resumable_and_idempotent() {
    let Some(db) = TestDb::setup().await else {
        panic!("TEST_DATABASE_URL unset — this test must not silently skip");
    };
    let mut conn = db.conn().await;
    let f = seed_app(&mut conn).await;
    exec(
        &mut conn,
        "UPDATE release_rollup_epoch SET started_at = now() + interval '1 hour'",
    )
    .await;
    repo::touch_event_user(&mut conn, f.app, "erin")
        .await
        .expect("touch");

    let old = Utc::now() - Duration::days(5);
    for i in 0..3 {
        event(
            &mut conn,
            f.app,
            f.env_a,
            "erin",
            Some("3.1"),
            old + Duration::minutes(i),
        )
        .await;
    }
    session(
        &mut conn,
        f.app,
        f.env_a,
        "rel-old",
        "erin",
        Some("3.1"),
        old,
        old + Duration::seconds(30),
    )
    .await;

    // The base rollups know the day (as they do after `backfill-rollups`)...
    let day = old.date_naive();
    fold::fold_day_from_raw(&mut conn, day, None, true, 2000)
        .await
        .expect("base day");
    fold::recompute_sessions(&mut conn, None)
        .await
        .expect("base sessions");
    // ...but the release twins do not (as on an upgraded deployment).
    exec(&mut conn, "DELETE FROM user_activity_release_daily").await;
    exec(&mut conn, "DELETE FROM session_stats_release_daily").await;
    exec(&mut conn, "DELETE FROM release_rollup_backfill_days").await;

    // A live-path write after the epoch, for a signal with no raw row here.
    repo::bump_person_release(
        &mut conn,
        f.app,
        "erin",
        Some(f.env_a),
        Some("3.1"),
        Utc::now(),
        1,
        0,
        0,
    )
    .await
    .expect("live bump");

    assert!(release::backfill_pending(&mut conn).await.unwrap());
    let mut seen = Vec::new();
    let out = release::backfill_all(&mut conn, |d| {
        seen.push(d);
        true
    })
    .await
    .expect("backfill");
    assert_eq!(out, BackfillOutcome::Completed);
    assert!(seen.contains(&day), "the pre-epoch day was not folded");
    assert!(release::is_ready(&mut conn, f.app).await.unwrap());
    assert!(!release::backfill_pending(&mut conn).await.unwrap());

    let events_sql =
        "SELECT events_count AS n FROM person_releases WHERE app_id = $1 AND release = '3.1'";
    // 3 raw pre-epoch events ADDED to the live row's 1.
    assert_eq!(scalar(&mut conn, events_sql, f.app).await, 4);
    assert_eq!(
        scalar(
            &mut conn,
            "SELECT sessions_count AS n FROM person_releases WHERE app_id = $1",
            f.app
        )
        .await,
        1
    );
    assert_eq!(
        scalar(
            &mut conn,
            "SELECT count(*) AS n FROM user_activity_release_daily WHERE app_id = $1 AND release = '3.1'",
            f.app
        )
        .await,
        1
    );
    assert_eq!(
        scalar(
            &mut conn,
            "SELECT COALESCE(sum(sessions), 0)::bigint AS n FROM session_stats_release_daily WHERE app_id = $1",
            f.app
        )
        .await,
        1
    );
    let all = ReadScope {
        app_id: f.app,
        env: EnvFilter::All,
    };
    let s = release::user_stats(
        &mut conn,
        &all,
        "3.1",
        Range::since(Utc::now() - Duration::days(30)),
        Utc::now(),
    )
    .await
    .unwrap();
    assert_eq!((s.total_users, s.mau), (1, 1));
    assert_eq!(s.avg_session_ms, 30_000.0);

    // Re-run: nothing pending, nothing added.
    let mut again = Vec::new();
    let out = release::backfill_all(&mut conn, |d| {
        again.push(d);
        true
    })
    .await
    .expect("re-run");
    assert_eq!(out, BackfillOutcome::Completed);
    assert!(again.is_empty(), "a finished day was re-folded: {again:?}");
    assert_eq!(
        scalar(&mut conn, events_sql, f.app).await,
        4,
        "re-run double-counted"
    );

    db.cleanup().await;
}

/// An interrupted day pass writes no marker, and resumes where it stopped.
#[tokio::test]
async fn interrupted_backfill_writes_no_marker() {
    let Some(db) = TestDb::setup().await else {
        panic!("TEST_DATABASE_URL unset — this test must not silently skip");
    };
    let mut conn = db.conn().await;
    let f = seed_app(&mut conn).await;
    exec(
        &mut conn,
        "UPDATE release_rollup_epoch SET started_at = now() + interval '1 hour'",
    )
    .await;
    let base = Utc::now() - Duration::days(6);
    for d in 0..3 {
        let at = base + Duration::days(d);
        event(&mut conn, f.app, f.env_a, "fay", Some("9"), at).await;
        fold::fold_day_from_raw(&mut conn, at.date_naive(), None, true, 2000)
            .await
            .expect("base day");
    }
    exec(&mut conn, "DELETE FROM release_rollup_backfill_days").await;

    let out = release::backfill_all(&mut conn, |_| false)
        .await
        .expect("first day");
    assert_eq!(out, BackfillOutcome::Interrupted);
    assert!(
        !release::is_ready(&mut conn, f.app).await.unwrap(),
        "marker before the data"
    );
    assert_eq!(release::days_pending(&mut conn).await.unwrap().len(), 2);

    let out = release::backfill_all(&mut conn, |_| true)
        .await
        .expect("resume");
    assert_eq!(out, BackfillOutcome::Completed);
    assert!(release::is_ready(&mut conn, f.app).await.unwrap());

    db.cleanup().await;
}

/// Identity merge moves the alias's per-release rows onto the person: a
/// colliding (release, env) key sums and widens its span, a release only the
/// alias had moves over whole, and nothing is left under the alias.
#[tokio::test]
async fn identity_merge_moves_release_rows_onto_the_person() {
    let Some(db) = TestDb::setup().await else {
        panic!("TEST_DATABASE_URL unset — this test must not silently skip");
    };
    let mut conn = db.conn().await;
    let f = seed_app(&mut conn).await;
    let t = Utc::now() - Duration::days(3);
    let env = Some(f.env_a);
    for (who, rel, at, ev) in [
        ("guest_1", "1.0", t, 3i64),
        ("guest_1", "2.0", t + Duration::hours(1), 1),
        ("person_1", "1.0", t + Duration::hours(2), 5),
    ] {
        repo::bump_person_release(&mut conn, f.app, who, env, Some(rel), at, ev, 0, 0)
            .await
            .expect("bump");
    }

    sauron_db::identity_merge::fold_rollups(&mut conn, f.app, "guest_1", "person_1", 7)
        .await
        .expect("fold");

    assert_eq!(
        scalar(
            &mut conn,
            "SELECT count(*) AS n FROM person_releases WHERE app_id = $1 AND distinct_id = 'guest_1'",
            f.app
        )
        .await,
        0,
        "alias rows must move, not copy"
    );
    assert_eq!(
        scalar(
            &mut conn,
            "SELECT events_count AS n FROM person_releases \
             WHERE app_id = $1 AND distinct_id = 'person_1' AND release = '1.0'",
            f.app
        )
        .await,
        8,
        "colliding key sums"
    );
    assert_eq!(
        scalar(
            &mut conn,
            "SELECT events_count AS n FROM person_releases \
             WHERE app_id = $1 AND distinct_id = 'person_1' AND release = '2.0'",
            f.app
        )
        .await,
        1,
        "alias-only release moves whole"
    );
    #[derive(QueryableByName)]
    struct Span {
        #[diesel(sql_type = Timestamptz)]
        first_seen: DateTime<Utc>,
    }
    let s: Span = diesel::sql_query(
        "SELECT first_seen FROM person_releases \
         WHERE app_id = $1 AND distinct_id = 'person_1' AND release = '1.0'",
    )
    .bind::<SqlUuid, _>(f.app)
    .get_result(&mut conn)
    .await
    .unwrap();
    assert_eq!(
        s.first_seen.timestamp(),
        t.timestamp(),
        "span widens to the alias's first signal"
    );
    db.cleanup().await;
}

#[derive(QueryableByName)]
struct PlanRow {
    #[diesel(sql_type = diesel::sql_types::Json, column_name = "QUERY PLAN")]
    plan: serde_json::Value,
}

/// Rows the executor actually pulled from `person_releases`, under ANY alias
/// (`Actual Rows` × `Actual Loops`, summed) — the page-first claim, measured.
/// Every alias, not just the paging scan's: the grouped shape reads the table
/// under another name, and a metric keyed on one alias passed against it.
fn rows_read_as_r(node: &serde_json::Value) -> f64 {
    let mut n = 0.0;
    if node.get("Relation Name").and_then(|a| a.as_str()) == Some("person_releases") {
        let rows = node
            .get("Actual Rows")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let loops = node
            .get("Actual Loops")
            .and_then(|v| v.as_f64())
            .unwrap_or(1.0);
        n += rows * loops;
    }
    if let Some(kids) = node.get("Plans").and_then(|p| p.as_array()) {
        for k in kids {
            n += rows_read_as_r(k);
        }
    }
    n
}

async fn explain_analyze(conn: &mut sauron_db::PgConn, sql: &str) -> serde_json::Value {
    let row: PlanRow = diesel::sql_query(format!("EXPLAIN (ANALYZE, FORMAT JSON) {sql}"))
        .get_result(conn)
        .await
        .unwrap_or_else(|e| panic!("EXPLAIN failed: {e}\n{sql}"));
    let mut v = row.plan;
    v[0]["Plan"].take()
}

/// The persons list and its count must NOT aggregate a release's whole
/// audience before paging (the persons-page-first lesson, applied to the
/// release twin). 40,000 people on one release, half of them in two
/// environments; the default page reads O(page) rows, a narrow-window count
/// reads O(window) rows. Measured with EXPLAIN ANALYZE rather than asserted on
/// plan shape, so a planner that ignores the index fails here too.
///
/// The size is load-bearing: at 4,000 people the planner (correctly, for that
/// size) hash-anti-joins the whole release instead of probing per person, so
/// a smaller fixture measures the planner's toy-scale costing, not the shape.
#[tokio::test]
async fn release_persons_list_and_count_read_a_page_not_the_audience() {
    let Some(db) = TestDb::setup().await else {
        panic!("TEST_DATABASE_URL unset — this test must not silently skip");
    };
    let mut conn = db.conn().await;
    let f = seed_app(&mut conn).await;
    let app = f.app;
    exec(
        &mut conn,
        &format!(
            "INSERT INTO event_users (id, app_id, distinct_id, properties, first_seen, last_seen) \
             SELECT gen_random_uuid(), '{app}', 'p' || g, '{{}}', now(), now() \
             FROM generate_series(1, 40000) g"
        ),
    )
    .await;
    for (env, every) in [(f.env_a, 1), (f.env_b, 2)] {
        exec(
            &mut conn,
            &format!(
                "INSERT INTO person_releases (app_id, environment_id, release, distinct_id, \
                    first_seen, last_seen, events_count) \
                 SELECT '{app}', '{env}', '5.0', 'p' || g, \
                        now() - (g || ' minutes')::interval - interval '1 day', \
                        now() - (g || ' minutes')::interval, 1 \
                 FROM generate_series(1, 40000) g WHERE g % {every} = 0"
            ),
        )
        .await;
    }
    exec(&mut conn, "ANALYZE person_releases").await;
    exec(&mut conn, "ANALYZE event_users").await;

    let lit = |sql: String, from: &str, limit: i64| {
        sql.replace("$8", "NULL::timestamptz")
            .replace("$7", "NULL::timestamptz")
            .replace("$6", from)
            .replace("$5", "'%'")
            .replace("$4", "0")
            .replace("$3", &limit.to_string())
            .replace("$2", "'5.0'")
            .replace("$1", &format!("'{app}'::uuid"))
    };
    for mode in [ReleaseMode::Used, ReleaseMode::Current] {
        let sql =
            release::persons_list_sql(&EnvFilter::All, mode, &last_seen_sort(), "last_seen", false);
        let plan = explain_analyze(&mut conn, &lit(sql, "now() - interval '3650 days'", 51)).await;
        let read = rows_read_as_r(&plan);
        assert!(
            read <= 400.0,
            "{mode:?}: the default page read {read} person_releases rows of 60000 — it is \
             aggregating the audience, not paging. Plan: {plan:#}"
        );
    }

    // A one-hour window holds 60 people: the count walks that slice only.
    let count_sql = format!(
        "SELECT count(*) FROM ({}) c",
        release::persons_list_sql(
            &EnvFilter::All,
            ReleaseMode::Used,
            &last_seen_sort(),
            "last_seen",
            false
        )
    );
    let plan = explain_analyze(
        &mut conn,
        &lit(count_sql, "now() - interval '1 hour'", 10_001),
    )
    .await;
    let read = rows_read_as_r(&plan);
    assert!(
        read <= 400.0,
        "the 1h-window count read {read} person_releases rows of 60000. Plan: {plan:#}"
    );

    // And the answers are right: one row per person, newest first.
    let scope = ReadScope {
        app_id: app,
        env: EnvFilter::All,
    };
    let page = release::list_persons(
        &mut conn,
        &scope,
        "5.0",
        ReleaseMode::Used,
        None,
        3,
        0,
        last_seen_sort(),
        TimeWindow {
            column: "last_seen",
            from: Utc::now() - Duration::days(3650),
            to: None,
        },
    )
    .await
    .unwrap();
    let got: Vec<(String, i64)> = page
        .into_iter()
        .map(|p| (p.distinct_id, p.events_count))
        .collect();
    assert_eq!(
        got,
        [
            ("p1".to_string(), 1),
            ("p2".to_string(), 2),
            ("p3".to_string(), 1)
        ],
        "p2 has a row in both environments: one line, summed"
    );
    let (n, capped) = release::count_persons(
        &mut conn,
        &scope,
        "5.0",
        ReleaseMode::Used,
        None,
        last_seen_sort(),
        TimeWindow {
            column: "last_seen",
            from: Utc::now() - Duration::days(3650),
            to: None,
        },
        10_000,
    )
    .await
    .unwrap();
    assert_eq!(
        (n, capped),
        (10_000, true),
        "40,000 people, capped at the cap"
    );
    let s = release::user_stats(
        &mut conn,
        &scope,
        "5.0",
        Range::since(Utc::now() - Duration::minutes(90)),
        Utc::now(),
    )
    .await
    .unwrap();
    // last_seen = now - g minutes: g in 1..=89 (or 90) is inside 90 minutes;
    // first_seen is a day earlier, so nobody is new in the window.
    assert_eq!(s.total_users, 40_000);
    assert!(
        (89..=90).contains(&s.active_in_range),
        "{}",
        s.active_in_range
    );
    assert_eq!(s.new_in_range, 0);
    db.cleanup().await;
}
