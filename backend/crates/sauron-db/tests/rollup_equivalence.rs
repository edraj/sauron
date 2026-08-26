//! Rollup-vs-legacy equivalence for the migration-71 dashboard rollups.
//!
//! The same seeded activity is read twice through the SAME `repo` functions:
//! once with the rollup gate forced closed (epoch pushed into the future →
//! `is_ready` false → legacy raw queries), once after folding with the gate
//! open (rollup reads). Counts must match exactly; sketch-derived numbers
//! (distinct users, percentiles) within their disclosed tolerance. Journeys
//! are asserted structurally — their day-scoped semantics deliberately differ
//! from the legacy window-scoped shape (see `rollups::read` module docs).

mod common;

use chrono::{DateTime, Duration, Utc};
use common::TestDb;
use diesel::sql_types::{Double, Nullable, Text, Timestamptz, Uuid as SqlUuid};
use diesel_async::RunQueryDsl;
use sauron_db::repo;
use sauron_db::rollups::{self, fold};
use sauron_db::scope::{EnvFilter, Range, ReadScope};
use uuid::Uuid;

async fn exec(conn: &mut sauron_db::PgConn, sql: &str) {
    diesel::sql_query(sql).execute(conn).await.expect(sql);
}

#[allow(clippy::too_many_arguments)]
async fn insert_event(
    conn: &mut sauron_db::PgConn,
    app: Uuid,
    env: Option<Uuid>,
    session: Option<&str>,
    distinct: &str,
    name: &str,
    screen: Option<&str>,
    at: DateTime<Utc>,
) {
    diesel::sql_query(
        "INSERT INTO analytics_events \
             (app_id, environment_id, session_id, distinct_id, name, screen, occurred_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind::<SqlUuid, _>(app)
    .bind::<Nullable<SqlUuid>, _>(env)
    .bind::<Nullable<Text>, _>(session.map(str::to_string))
    .bind::<Text, _>(distinct.to_string())
    .bind::<Text, _>(name.to_string())
    .bind::<Nullable<Text>, _>(screen.map(str::to_string))
    .bind::<Timestamptz, _>(at)
    .execute(conn)
    .await
    .expect("insert analytics event");
}

async fn insert_error(
    conn: &mut sauron_db::PgConn,
    app: Uuid,
    env: Option<Uuid>,
    issue: Uuid,
    distinct: Option<&str>,
    screen: Option<&str>,
    at: DateTime<Utc>,
) {
    diesel::sql_query(
        "INSERT INTO error_events \
             (app_id, environment_id, issue_id, fingerprint, distinct_id, screen, occurred_at) \
         VALUES ($1, $2, $3, 'rollup-eq-fp', $4, $5, $6)",
    )
    .bind::<SqlUuid, _>(app)
    .bind::<Nullable<SqlUuid>, _>(env)
    .bind::<SqlUuid, _>(issue)
    .bind::<Nullable<Text>, _>(distinct.map(str::to_string))
    .bind::<Nullable<Text>, _>(screen.map(str::to_string))
    .bind::<Timestamptz, _>(at)
    .execute(conn)
    .await
    .expect("insert error event");
}

#[allow(clippy::too_many_arguments)]
async fn insert_txn(
    conn: &mut sauron_db::PgConn,
    app: Uuid,
    env: Option<Uuid>,
    name: &str,
    op: &str,
    duration_ms: f64,
    http_status: Option<i32>,
    at: DateTime<Utc>,
) {
    diesel::sql_query(
        "INSERT INTO transactions \
             (app_id, environment_id, name, op, duration_ms, http_status, occurred_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind::<SqlUuid, _>(app)
    .bind::<Nullable<SqlUuid>, _>(env)
    .bind::<Text, _>(name.to_string())
    .bind::<Text, _>(op.to_string())
    .bind::<Double, _>(duration_ms)
    .bind::<Nullable<diesel::sql_types::Integer>, _>(http_status)
    .bind::<Timestamptz, _>(at)
    .execute(conn)
    .await
    .expect("insert transaction");
}

#[allow(clippy::too_many_arguments)]
async fn insert_session(
    conn: &mut sauron_db::PgConn,
    app: Uuid,
    env: Option<Uuid>,
    sid: &str,
    device: Option<&str>,
    started: DateTime<Utc>,
    last: DateTime<Utc>,
    unhandled: i32,
) {
    diesel::sql_query(
        "INSERT INTO sessions \
             (app_id, environment_id, session_id, device_key, started_at, last_event_at, unhandled_errors_count) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind::<SqlUuid, _>(app)
    .bind::<Nullable<SqlUuid>, _>(env)
    .bind::<Text, _>(sid.to_string())
    .bind::<Nullable<Text>, _>(device.map(str::to_string))
    .bind::<Timestamptz, _>(started)
    .bind::<Timestamptz, _>(last)
    .bind::<diesel::sql_types::Integer, _>(unhandled)
    .execute(conn)
    .await
    .expect("insert session");
}

async fn insert_device(
    conn: &mut sauron_db::PgConn,
    app: Uuid,
    key: &str,
    family: &str,
    model: &str,
    seen: DateTime<Utc>,
) {
    diesel::sql_query(
        "INSERT INTO devices (app_id, device_key, family, model, first_seen, last_seen) \
         VALUES ($1, $2, $3, $4, $5, $5)",
    )
    .bind::<SqlUuid, _>(app)
    .bind::<Text, _>(key.to_string())
    .bind::<Text, _>(family.to_string())
    .bind::<Text, _>(model.to_string())
    .bind::<Timestamptz, _>(seen)
    .execute(conn)
    .await
    .expect("insert device");
}

async fn drain_folds(conn: &mut sauron_db::PgConn, upto: DateTime<Utc>) {
    for _ in 0..5 {
        match fold::fold_analytics(conn, upto, 2000)
            .await
            .expect("fold analytics")
        {
            Some(o) if !o.caught_up => continue,
            _ => break,
        }
    }
    fold::fold_errors(conn, upto).await.expect("fold errors");
    fold::fold_transactions(conn, upto, 2000)
        .await
        .expect("fold transactions");
    fold::recompute_sessions(conn, Some(upto - Duration::days(40)))
        .await
        .expect("recompute sessions");
}

fn assert_close(label: &str, a: f64, b: f64, rel: f64) {
    let denom = a.abs().max(b.abs()).max(1e-9);
    assert!(
        (a - b).abs() / denom <= rel,
        "{label}: legacy {a} vs rollup {b} beyond rel tolerance {rel}"
    );
}

#[tokio::test]
async fn rollup_reads_match_legacy_reads() {
    let Some(db) = TestDb::setup().await else {
        panic!("TEST_DATABASE_URL unset — this test must not silently skip");
    };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs().await;
    let app = ids.app_id;
    let env_a = ids.env_a;
    let now = Utc::now();

    // Controlled activity. Day placement matters for the dau/wau semantics
    // difference: "yesterday" data goes 26h back so BOTH the legacy rolling
    // 24h window and the rollup calendar-today bucket exclude it, and "today"
    // data goes 5 minutes back so both include it.
    let day_a = now - Duration::days(2);
    let day_b = now - Duration::hours(26);
    let today = now - Duration::minutes(5);
    // u1, session s1, day A: Home(5s dwell) -> Cart(10s dwell, capped path unused).
    insert_event(
        &mut conn,
        app,
        Some(env_a),
        Some("eq-s1"),
        "eq-u1",
        "$screen",
        Some("Home"),
        day_a,
    )
    .await;
    insert_event(
        &mut conn,
        app,
        Some(env_a),
        Some("eq-s1"),
        "eq-u1",
        "tap",
        Some("Home"),
        day_a + Duration::seconds(5),
    )
    .await;
    insert_event(
        &mut conn,
        app,
        Some(env_a),
        Some("eq-s1"),
        "eq-u1",
        "$screen",
        Some("Cart"),
        day_a + Duration::seconds(10),
    )
    .await;
    insert_event(
        &mut conn,
        app,
        Some(env_a),
        Some("eq-s1"),
        "eq-u1",
        "checkout",
        None,
        day_a + Duration::seconds(20),
    )
    .await;
    // u2, session s2, day A, env NULL (unattributed).
    insert_event(
        &mut conn,
        app,
        None,
        Some("eq-s2"),
        "eq-u2",
        "$screen",
        Some("Home"),
        day_a + Duration::seconds(1),
    )
    .await;
    // u1, session s3, day B: Home dwell 3s, Cart left pending (no dwell).
    insert_event(
        &mut conn,
        app,
        Some(env_a),
        Some("eq-s3"),
        "eq-u1",
        "$screen",
        Some("Home"),
        day_b,
    )
    .await;
    insert_event(
        &mut conn,
        app,
        Some(env_a),
        Some("eq-s3"),
        "eq-u1",
        "$screen",
        Some("Cart"),
        day_b + Duration::seconds(3),
    )
    .await;
    // u1 today, keeps dau equal under both semantics.
    insert_event(
        &mut conn,
        app,
        Some(env_a),
        Some("eq-s4"),
        "eq-u1",
        "tap",
        None,
        today,
    )
    .await;
    // Errors: two on Home (day A), one without screen (day B), one anonymous.
    insert_error(
        &mut conn,
        app,
        Some(env_a),
        ids.issue_id,
        Some("eq-u1"),
        Some("Home"),
        day_a + Duration::seconds(7),
    )
    .await;
    insert_error(
        &mut conn,
        app,
        None,
        ids.issue_id,
        Some("eq-u2"),
        Some("Home"),
        day_a + Duration::seconds(8),
    )
    .await;
    insert_error(
        &mut conn,
        app,
        Some(env_a),
        ids.issue_id,
        None,
        None,
        day_b + Duration::seconds(1),
    )
    .await;
    // Transactions: two hours, one 5xx.
    insert_txn(
        &mut conn,
        app,
        Some(env_a),
        "GET /api",
        "http.server",
        100.0,
        Some(200),
        day_a,
    )
    .await;
    insert_txn(
        &mut conn,
        app,
        Some(env_a),
        "GET /api",
        "http.server",
        200.0,
        Some(200),
        day_a + Duration::minutes(5),
    )
    .await;
    insert_txn(
        &mut conn,
        app,
        Some(env_a),
        "GET /api",
        "http.server",
        400.0,
        Some(503),
        day_a + Duration::hours(1),
    )
    .await;
    insert_txn(
        &mut conn,
        app,
        None,
        "queue.job",
        "task",
        1500.0,
        None,
        day_b,
    )
    .await;
    // Sessions: two on day A (one crashed), one on day B.
    insert_session(
        &mut conn,
        app,
        Some(env_a),
        "eq-s1",
        None,
        day_a,
        day_a + Duration::seconds(20),
        0,
    )
    .await;
    insert_session(
        &mut conn,
        app,
        None,
        "eq-s2",
        None,
        day_a + Duration::seconds(1),
        day_a + Duration::seconds(71),
        2,
    )
    .await;
    insert_session(
        &mut conn,
        app,
        Some(env_a),
        "eq-s3",
        None,
        day_b,
        day_b + Duration::seconds(3),
        0,
    )
    .await;

    // Device-groups fixtures: two groups, keyed sessions, so the windowed
    // sessions_count can be compared between the live LATERAL (gate closed)
    // and the device_sessions_daily join (gate open).
    insert_device(
        &mut conn,
        app,
        "eq-dev-1",
        "Android",
        "Pixel 9",
        now - Duration::hours(20),
    )
    .await;
    insert_device(
        &mut conn,
        app,
        "eq-dev-2",
        "iOS",
        "iPhone 15",
        now - Duration::hours(20),
    )
    .await;
    insert_session(
        &mut conn,
        app,
        Some(env_a),
        "eq-ds1",
        Some("eq-dev-1"),
        day_a + Duration::seconds(30),
        day_a + Duration::seconds(50),
        0,
    )
    .await;
    insert_session(
        &mut conn,
        app,
        None,
        "eq-ds2",
        Some("eq-dev-1"),
        day_a + Duration::seconds(40),
        day_a + Duration::seconds(60),
        0,
    )
    .await;
    insert_session(
        &mut conn,
        app,
        Some(env_a),
        "eq-ds3",
        Some("eq-dev-1"),
        day_b + Duration::seconds(9),
        day_b + Duration::seconds(19),
        0,
    )
    .await;
    insert_session(
        &mut conn,
        app,
        Some(env_a),
        "eq-ds4",
        Some("eq-dev-2"),
        day_a + Duration::seconds(55),
        day_a + Duration::seconds(75),
        0,
    )
    .await;
    // The rollup SHAPE (vs the live shape) needs the device_environments
    // marker; run its backfill so both captures use the rollup shape and the
    // ONLY varying piece is the sessions source under test.
    sauron_db::device_env_backfill::backfill_all(db.pool())
        .await
        .expect("device env backfill");

    let range = Range::since(now - Duration::days(30));
    let scope = || ReadScope::all(app);
    let scope_a = || ReadScope::new(app, EnvFilter::One(env_a));

    // ------------------------------------------------------------------
    // Legacy capture: force the gate closed by pushing the epoch forward.
    // ------------------------------------------------------------------
    exec(
        &mut conn,
        "UPDATE rollup_epoch SET started_at = now() + interval '1 hour'",
    )
    .await;
    assert!(
        !rollups::is_ready(&mut conn, app).await.expect("is_ready"),
        "gate must be closed"
    );

    let l_top = repo::top_events(&mut conn, scope(), range, 20)
        .await
        .expect("legacy top");
    let l_top_a = repo::top_events(&mut conn, scope_a(), range, 20)
        .await
        .expect("legacy top env");
    let mut l_screens = repo::screen_list(
        &mut conn,
        scope(),
        range,
        "%",
        50,
        0,
        common::default_screen_sort(),
    )
    .await
    .expect("legacy screens");
    let l_count = repo::count_screens(&mut conn, scope(), range, "%", 10_000)
        .await
        .expect("legacy count");
    let l_perf = repo::performance_summary(&mut conn, scope(), range, None, None)
        .await
        .expect("legacy perf");
    let l_pseries = repo::performance_series(&mut conn, scope(), range, None, None)
        .await
        .expect("legacy pseries");
    let l_users = repo::user_stats(&mut conn, scope(), range, now)
        .await
        .expect("legacy users");
    let l_useries = repo::active_user_series(&mut conn, scope(), range)
        .await
        .expect("legacy useries");
    let l_sess = repo::session_stats(&mut conn, scope(), range)
        .await
        .expect("legacy sess");
    let l_sess_a = repo::session_stats(&mut conn, scope_a(), range)
        .await
        .expect("legacy sess env");
    let l_sseries = repo::session_duration_series(&mut conn, scope(), range)
        .await
        .expect("legacy sseries");
    let l_shist = repo::session_duration_histogram(&mut conn, scope(), range)
        .await
        .expect("legacy shist");
    let l_totals = repo::overview_totals(&mut conn, scope(), range)
        .await
        .expect("legacy totals");
    let l_eseries = repo::event_series(&mut conn, scope(), None, range)
        .await
        .expect("legacy eseries");
    let l_errseries = repo::error_series(&mut conn, scope(), range)
        .await
        .expect("legacy errseries");
    // The day-count comparison is the ONE read here with an explicit upper
    // bound, and the bucket it compares is fed by TWO different clocks: this
    // test's own rows are anchored to the real `now` above, while
    // `seed_two_envs` pins its rows to TODAY AT 12:00 UTC — a fixed instant
    // that sits in the FUTURE for any run before ~noon. The rollup's
    // whole-day bucket includes a future-noon row regardless (the disclosed
    // upper-edge semantic; the fold admits it by `received_at`), but the
    // legacy point-bound excludes it, so a bound derived from the wall clock
    // alone made the two sides disagree on today's bucket for every run in
    // the first half of the UTC day — which is exactly when CI runs. (`now +
    // 1h` was written for an older seed that pinned rows only seconds ahead;
    // the seed moved, the slack silently stopped covering it, and this test
    // became green-after-lunch.) Anchoring to BOTH clocks puts every seeded
    // row inside both windows at any time of day: the fixture's largest
    // offset past its anchor is +5 s, this test seeds nothing after `now`,
    // and nothing occupies the gap up to the bound, so the extra width
    // admits no row asymmetrically.
    let day_upper = now.max(ids.pinned_now) + Duration::hours(1);
    let l_days = repo::active_users_by_day_hot(&mut conn, scope(), range.from, day_upper)
        .await
        .expect("legacy days");

    // ------------------------------------------------------------------
    // Open the gate: epoch behind the data's received_at, watermarks reset,
    // fold everything. The app was created after the (moved) epoch, so it is
    // implicitly ready with no marker row.
    // ------------------------------------------------------------------
    exec(
        &mut conn,
        "UPDATE rollup_epoch SET started_at = now() - interval '1 day'",
    )
    .await;
    exec(
        &mut conn,
        "UPDATE rollup_watermarks SET watermark = (SELECT started_at FROM rollup_epoch)",
    )
    .await;
    let upto = Utc::now() + Duration::seconds(1);
    drain_folds(&mut conn, upto).await;
    assert!(
        rollups::is_ready(&mut conn, app).await.expect("is_ready"),
        "gate must be open"
    );
    let as_of = rollups::as_of(&mut conn, &rollups::EVENT_SOURCES)
        .await
        .expect("as_of");
    assert!(as_of.is_some(), "as_of must report a watermark once folded");

    let r_top = repo::top_events(&mut conn, scope(), range, 20)
        .await
        .expect("rollup top");
    let r_top_a = repo::top_events(&mut conn, scope_a(), range, 20)
        .await
        .expect("rollup top env");
    let mut r_screens = repo::screen_list(
        &mut conn,
        scope(),
        range,
        "%",
        50,
        0,
        common::default_screen_sort(),
    )
    .await
    .expect("rollup screens");
    let r_count = repo::count_screens(&mut conn, scope(), range, "%", 10_000)
        .await
        .expect("rollup count");
    let r_perf = repo::performance_summary(&mut conn, scope(), range, None, None)
        .await
        .expect("rollup perf");
    let r_pseries = repo::performance_series(&mut conn, scope(), range, None, None)
        .await
        .expect("rollup pseries");
    let r_users = repo::user_stats(&mut conn, scope(), range, now)
        .await
        .expect("rollup users");
    let r_useries = repo::active_user_series(&mut conn, scope(), range)
        .await
        .expect("rollup useries");
    let r_sess = repo::session_stats(&mut conn, scope(), range)
        .await
        .expect("rollup sess");
    let r_sess_a = repo::session_stats(&mut conn, scope_a(), range)
        .await
        .expect("rollup sess env");
    let r_sseries = repo::session_duration_series(&mut conn, scope(), range)
        .await
        .expect("rollup sseries");
    let r_shist = repo::session_duration_histogram(&mut conn, scope(), range)
        .await
        .expect("rollup shist");
    let r_totals = repo::overview_totals(&mut conn, scope(), range)
        .await
        .expect("rollup totals");
    let r_eseries = repo::event_series(&mut conn, scope(), None, range)
        .await
        .expect("rollup eseries");
    let r_errseries = repo::error_series(&mut conn, scope(), range)
        .await
        .expect("rollup errseries");
    // Same `day_upper` as the legacy call above, by construction — two
    // hand-computed bounds here would eventually disagree the same way the
    // two clocks did.
    let r_days = rollups::read::active_users_by_day(&mut conn, &scope(), range.from, day_upper)
        .await
        .expect("rollup days");

    // ------------------------------------------------------------------
    // Exact comparisons.
    // ------------------------------------------------------------------
    assert_eq!(l_top, r_top, "top_events");
    assert_eq!(l_top_a, r_top_a, "top_events under One(env_a)");
    assert_eq!(l_count, r_count, "count_screens");
    for (label, l, r) in [
        ("event_series", &l_eseries, &r_eseries),
        ("error_series", &l_errseries, &r_errseries),
    ] {
        assert_eq!(l.len(), r.len(), "{label} length");
        for (a, b) in l.iter().zip(r.iter()) {
            assert_eq!((a.bucket, a.count), (b.bucket, b.count), "{label} point");
        }
    }
    assert_eq!(l_days.len(), r_days.len(), "active days length");
    for (a, b) in l_days.iter().zip(r_days.iter()) {
        assert_eq!((a.day, a.count), (b.day, b.count), "active users by day");
    }

    l_screens.sort_by(|a, b| a.screen.cmp(&b.screen));
    r_screens.sort_by(|a, b| a.screen.cmp(&b.screen));
    assert_eq!(l_screens.len(), r_screens.len(), "screen row count");
    for (l, r) in l_screens.iter().zip(r_screens.iter()) {
        assert_eq!(l.screen, r.screen);
        assert_eq!(l.views, r.views, "views for {}", l.screen);
        assert_eq!(l.events, r.events, "events for {}", l.screen);
        assert_eq!(l.exceptions, r.exceptions, "exceptions for {}", l.screen);
        assert!(
            (l.users - r.users).abs() <= 2,
            "users for {}: {} vs {}",
            l.screen,
            l.users,
            r.users
        );
        assert_close(
            &format!("avg_dwell {}", l.screen),
            l.avg_dwell_ms,
            r.avg_dwell_ms,
            1e-6,
        );
    }

    assert_eq!(l_perf.len(), r_perf.len(), "perf row count");
    for (l, r) in l_perf.iter().zip(r_perf.iter()) {
        assert_eq!(
            (l.name.clone(), l.op.clone()),
            (r.name.clone(), r.op.clone())
        );
        assert_eq!(l.count, r.count, "perf count {}", l.name);
        assert_close(&format!("perf avg {}", l.name), l.avg, r.avg, 1e-6);
        assert_close(
            &format!("perf error_rate {}", l.name),
            l.error_rate,
            r.error_rate,
            1e-6,
        );
        assert_close(&format!("perf p50 {}", l.name), l.p50, r.p50, 0.30);
        assert_close(&format!("perf p95 {}", l.name), l.p95, r.p95, 0.30);
    }
    assert_eq!(l_pseries.len(), r_pseries.len(), "perf series length");
    for (l, r) in l_pseries.iter().zip(r_pseries.iter()) {
        assert_eq!(l.bucket, r.bucket);
        assert_eq!(l.throughput, r.throughput, "throughput @{}", l.bucket);
        assert_close(&format!("series p50 @{}", l.bucket), l.p50, r.p50, 0.30);
    }

    assert_eq!(l_users.total_users, r_users.total_users, "total_users");
    assert_eq!(
        l_users.active_in_range, r_users.active_in_range,
        "active_in_range"
    );
    assert_eq!(l_users.new_in_range, r_users.new_in_range, "new_in_range");
    assert!(
        (l_users.dau - r_users.dau).abs() <= 1,
        "dau {} vs {}",
        l_users.dau,
        r_users.dau
    );
    assert!(
        (l_users.wau - r_users.wau).abs() <= 1,
        "wau {} vs {}",
        l_users.wau,
        r_users.wau
    );
    assert!(
        (l_users.mau - r_users.mau).abs() <= 1,
        "mau {} vs {}",
        l_users.mau,
        r_users.mau
    );
    assert_close(
        "avg_session_ms",
        l_users.avg_session_ms,
        r_users.avg_session_ms,
        1e-6,
    );
    assert_close(
        "median_session_ms",
        l_users.median_session_ms,
        r_users.median_session_ms,
        0.30,
    );

    assert_eq!(l_useries.len(), r_useries.len(), "user series length");
    for (l, r) in l_useries.iter().zip(r_useries.iter()) {
        assert_eq!(l.bucket, r.bucket, "user series bucket");
        assert!((l.active - r.active).abs() <= 1, "active @{}", l.bucket);
        assert_eq!(l.new_users, r.new_users, "new users @{}", l.bucket);
    }

    assert_eq!(l_sess.sessions, r_sess.sessions, "sessions");
    assert_eq!(l_sess.crashed, r_sess.crashed, "crashed");
    assert_close(
        "sess avg",
        l_sess.avg_session_ms,
        r_sess.avg_session_ms,
        1e-6,
    );
    assert_close(
        "sess median",
        l_sess.median_session_ms,
        r_sess.median_session_ms,
        0.30,
    );
    assert_eq!(
        l_sess_a.sessions, r_sess_a.sessions,
        "sessions under One(env_a)"
    );
    assert_eq!(
        l_sess_a.crashed, r_sess_a.crashed,
        "crashed under One(env_a)"
    );

    assert_eq!(l_sseries.len(), r_sseries.len(), "session series length");
    for (l, r) in l_sseries.iter().zip(r_sseries.iter()) {
        assert_eq!(l.bucket, r.bucket);
        assert_close(&format!("sess avg @{}", l.bucket), l.avg_ms, r.avg_ms, 1e-6);
    }
    assert_eq!(l_shist.len(), r_shist.len(), "histogram buckets");
    for (l, r) in l_shist.iter().zip(r_shist.iter()) {
        assert_eq!(
            (l.bucket.clone(), l.count),
            (r.bucket.clone(), r.count),
            "histogram bucket"
        );
    }

    assert_eq!(l_totals.events, r_totals.events, "totals events");
    assert_eq!(l_totals.errors, r_totals.errors, "totals errors");
    assert_eq!(l_totals.sessions, r_totals.sessions, "totals sessions");
    assert_eq!(l_totals.users, r_totals.users, "totals users");
    assert_eq!(l_totals.new_users, r_totals.new_users, "totals new_users");
    assert_eq!(
        l_totals.crashed_sessions, r_totals.crashed_sessions,
        "totals crashed"
    );
    assert_eq!(
        l_totals.has_crash_signal, r_totals.has_crash_signal,
        "totals crash signal"
    );

    // ------------------------------------------------------------------
    // Journeys: day-scoped by design, so structural assertions only.
    // ------------------------------------------------------------------
    let (nodes, links) = repo::journey_graph(&mut conn, scope(), range, 5)
        .await
        .expect("rollup journey");
    assert!(!nodes.is_empty(), "journey nodes present");
    assert!(nodes.iter().all(|n| n.step < 5), "depth respected");
    let step0: i64 = nodes.iter().filter(|n| n.step == 0).map(|n| n.count).sum();
    // eq-u1 has three (day, env) journey groups (day A env_a, day B env_a,
    // today env_a) and eq-u2 one — plus whatever seed_two_envs contributed.
    assert!(
        step0 >= 4,
        "step-0 node mass covers seeded user-days, got {step0}"
    );
    for l in &links {
        let from_mass: i64 = nodes
            .iter()
            .filter(|n| n.step == l.from_step && n.event == l.from_event)
            .map(|n| n.count)
            .sum();
        assert!(
            l.count <= from_mass,
            "link {} -> {} exceeds its from-node",
            l.from_event,
            l.to_event
        );
    }

    // ------------------------------------------------------------------
    // Device groups: live sessions LATERAL vs device_sessions_daily join.
    // Under EnvFilter::All the sessions source is the ONLY difference
    // between the two captures.
    // ------------------------------------------------------------------
    let group_sort = || repo::SortSpec {
        column: "last_seen",
        descending: true,
        tiebreak: "d.family, d.model, d.os_name, d.os_version",
        nulls_last: false,
    };
    let window = repo::TimeWindow::since("last_seen", now - Duration::days(30));
    exec(
        &mut conn,
        "UPDATE rollup_epoch SET started_at = now() + interval '1 hour'",
    )
    .await;
    let mut l_groups =
        repo::list_device_groups(&mut conn, scope(), window, 50, 0, group_sort(), None)
            .await
            .expect("legacy device groups");
    exec(
        &mut conn,
        "UPDATE rollup_epoch SET started_at = now() - interval '1 day'",
    )
    .await;
    let mut r_groups =
        repo::list_device_groups(&mut conn, scope(), window, 50, 0, group_sort(), None)
            .await
            .expect("rollup device groups");
    let key = |g: &repo::DeviceGroupRow| (g.family.clone(), g.model.clone());
    l_groups.sort_by_key(key);
    r_groups.sort_by_key(key);
    assert_eq!(l_groups.len(), r_groups.len(), "device group count");
    let mut seen_sessions = 0i64;
    for (l, r) in l_groups.iter().zip(r_groups.iter()) {
        assert_eq!(key(l), key(r), "group identity");
        assert_eq!(
            l.device_count, r.device_count,
            "device_count for {:?}",
            l.model
        );
        assert_eq!(
            l.sessions_count, r.sessions_count,
            "sessions_count for {:?}",
            l.model
        );
        seen_sessions += r.sessions_count;
    }
    assert!(
        seen_sessions >= 4,
        "the keyed fixture sessions are counted, got {seen_sessions}"
    );

    // ------------------------------------------------------------------
    // Idempotence + rebuild equivalence.
    // ------------------------------------------------------------------
    let again = fold::fold_analytics(&mut conn, Utc::now() + Duration::seconds(1), 2000)
        .await
        .expect("re-fold");
    if let Some(o) = again {
        assert_eq!(o.rows_read, 0, "second fold must read nothing");
    }
    let day = day_a.date_naive();
    fold::fold_day_from_raw(&mut conn, day, None, true, 2000)
        .await
        .expect("rebuild day");
    let mut rebuilt = repo::screen_list(
        &mut conn,
        scope(),
        range,
        "%",
        50,
        0,
        common::default_screen_sort(),
    )
    .await
    .expect("screens after rebuild");
    rebuilt.sort_by(|a, b| a.screen.cmp(&b.screen));
    for (l, r) in r_screens.iter().zip(rebuilt.iter()) {
        assert_eq!(l.screen, r.screen, "rebuild keeps screens");
        assert_eq!(l.views, r.views, "rebuild keeps views for {}", l.screen);
        assert_eq!(
            l.exceptions, r.exceptions,
            "rebuild keeps exceptions for {}",
            l.screen
        );
    }

    drop(conn);
    db.cleanup().await;
}
