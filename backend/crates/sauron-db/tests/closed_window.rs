//! The upper bound, exercised against a real Postgres.
//!
//! # Why this file exists
//!
//! Every analytics read in `repo.rs` gained an optional `col < $n` predicate,
//! and in the raw-SQL ones `$n` is computed by hand — `let limit_idx = if
//! scope.env.consumes_bind() { 5 } else { 4 }` and its neighbours. A wrong
//! index does not fail to compile, does not trip clippy, and does not
//! necessarily error at runtime: bind one timestamp where another was meant and
//! Postgres runs the query and returns the wrong rows.
//!
//! So the assertions here are behavioural and comparative. Each function is
//! asked the same question twice — once over a window open above, once over the
//! same window closed before a row that exists — and the two answers must
//! DIFFER by exactly that row. A test that only checked the closed window would
//! pass against a predicate that excluded everything.
//!
//! # Why every environment filter
//!
//! The bind index the upper bound lands on depends on `EnvFilter::consumes_bind
//! ()`, which is true for `One`/`Subset` and false for `All`/`Unattributed`.
//! Running only under `All` would leave the other arithmetic branch — the one
//! that shifts — completely unexercised, which is the half more likely to be
//! wrong. Each case therefore runs under `All` and under `One`.

mod common;

use chrono::{DateTime, Duration, Utc};
use common::TestDb;
use diesel_async::RunQueryDsl;
use sauron_db::models::{NewErrorEvent, NewIssue, NewTransaction};
use sauron_db::repo;
use sauron_db::scope::{EnvFilter, Range, ReadScope};
use serde_json::json;
use uuid::Uuid;

/// Fixed instants, well clear of the fixture `seed_two_envs` lays down.
///
/// All three are in the PAST — an `occurred_at` in the future is clamped by the
/// ingest path and would make this file test the clamp instead of the window.
/// `LATE` is the row every closed-window assertion must exclude.
fn early() -> DateTime<Utc> {
    Utc::now() - Duration::days(40)
}
fn late() -> DateTime<Utc> {
    Utc::now() - Duration::days(2)
}
/// The exclusive upper bound: after `early`, before `late`.
fn cutoff() -> DateTime<Utc> {
    Utc::now() - Duration::days(20)
}
/// A lower bound comfortably before `early`.
fn floor() -> DateTime<Utc> {
    Utc::now() - Duration::days(60)
}

fn open() -> Range {
    Range::since(floor())
}
fn closed() -> Range {
    Range::new(floor(), Some(cutoff()))
}

/// `All` and `One` — the two sides of the `consumes_bind()` branch every
/// hand-computed bind index turns on.
fn both_filters(ids: &common::SeedIds) -> [(&'static str, EnvFilter); 2] {
    [("all", EnvFilter::All), ("one", EnvFilter::One(ids.env_a))]
}

/// An issue owned by this file alone.
///
/// NOT `SeedIds::issue_id`: that one already carries six fixture occurrences
/// spread across the fixture's own instants, so an exact `open - closed == 1`
/// against it would be measuring the fixture rather than the bound.
const CW_FINGERPRINT: &str = "closed-window-own-issue";

async fn cw_issue(conn: &mut sauron_db::PgConn, ids: &common::SeedIds, at: DateTime<Utc>) -> Uuid {
    repo::upsert_issue(
        conn,
        NewIssue {
            app_id: ids.app_id,
            fingerprint: CW_FINGERPRINT,
            type_: "Error",
            title: "closed window",
            culprit: "harness::closed_window",
            level: "error",
            first_seen: at,
            last_seen: at,
            times_seen: 1,
        },
    )
    .await
    .expect("upsert issue")
}

/// Seed one of everything at `at`, all attributed to `env_a` so both filters
/// see the same rows.
async fn seed_at(
    conn: &mut sauron_db::PgConn,
    ids: &common::SeedIds,
    at: DateTime<Utc>,
    tag: &str,
) {
    let distinct = format!("cw-{tag}");
    common::seed_identified_user(conn, ids.app_id, &distinct).await;
    // `touch_event_user` stamps `now()`, so without this the row's `last_seen`
    // is TODAY no matter which instant this call is seeding — and every
    // `event_users`-backed assertion below would be measuring the clock rather
    // than the window.
    diesel::sql_query(
        "UPDATE event_users SET first_seen = $2, last_seen = $2 \
         WHERE app_id = $1 AND distinct_id = $3",
    )
    .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
    .bind::<diesel::sql_types::Timestamptz, _>(at)
    .bind::<diesel::sql_types::Text, _>(distinct.clone())
    .execute(conn)
    .await
    .expect("backdate event_users");

    // Two analytics events: the synthetic `$screen` view the screens stats
    // count, plus a named event the funnel and journey walk.
    for name in ["$screen", "cw_step_a"] {
        repo::insert_analytics_event(
            conn,
            sauron_db::models::NewAnalyticsEvent {
                id: Uuid::new_v4(),
                app_id: ids.app_id,
                environment_id: Some(ids.env_a),
                name: name.to_string(),
                distinct_id: distinct.clone(),
                properties: json!({}),
                context: json!({}),
                session_id: Some(format!("cw-sess-{tag}")),
                release: None,
                ip_address: None,
                occurred_at: at,
                device_key: Some(format!("cw-dev-{tag}")),
                screen: Some("CwScreen".into()),
                workflow_id: None,
                workflow_name: None,
                tags: json!({}),
                contexts: json!({}),
                extra: json!({}),
            },
        )
        .await
        .expect("insert analytics event");
    }

    // Written out rather than going through `common::seed_signal_error`,
    // which leaves `screen` NULL — and a NULL screen is invisible to every
    // `screen_ctes` assertion below, so the exceptions column would read 0 in
    // both windows and the test would pass without testing anything.
    let issue_id = cw_issue(conn, ids, at).await;
    repo::insert_error_event(
        conn,
        NewErrorEvent {
            id: Uuid::new_v4(),
            app_id: ids.app_id,
            environment_id: Some(ids.env_a),
            issue_id,
            fingerprint: CW_FINGERPRINT.to_string(),
            level: "error".into(),
            message: "closed window".into(),
            exception_type: "CwError".into(),
            exception_value: "seeded".into(),
            stacktrace: json!([]),
            breadcrumbs: json!([]),
            context: json!({}),
            tags: json!({}),
            release: None,
            distinct_id: Some(distinct.clone()),
            event_user: None,
            sdk: None,
            ip_address: None,
            occurred_at: at,
            session_id: Some(format!("cw-sess-{tag}")),
            device_key: Some(format!("cw-dev-{tag}")),
            screen: Some("CwScreen".into()),
            workflow_id: None,
            workflow_name: None,
            stacktrace_symbolicated: None,
            symbolication_status: "not_applicable".into(),
            debug_meta: None,
            contexts: json!({}),
            extra: json!({}),
            handled: Some(false),
            title: None,
            culprit: None,
            stacktrace_sha256: None,
        },
    )
    .await
    .expect("insert error event");

    repo::bump_session(
        conn,
        ids.app_id,
        &format!("cw-sess-{tag}"),
        Some(&distinct),
        Some(&format!("cw-dev-{tag}")),
        at,
        &json!({}),
        None,
        Some(ids.env_a),
        None,
        1,
        1,
        1,
    )
    .await
    .expect("bump session");

    repo::insert_transaction(
        conn,
        NewTransaction {
            id: Uuid::new_v4(),
            app_id: ids.app_id,
            environment_id: Some(ids.env_a),
            name: "cw /checkout".into(),
            op: "http.server".into(),
            duration_ms: 120.0,
            status: Some("ok".into()),
            http_method: Some("GET".into()),
            http_status: Some(200),
            url: None,
            distinct_id: Some(distinct.clone()),
            session_id: None,
            device_key: None,
            workflow_id: None,
            workflow_name: None,
            release: None,
            ip_address: None,
            occurred_at: at,
            finished_at: None,
            tags: json!({}),
            extra: json!({}),
        },
    )
    .await
    .expect("insert transaction");
}

/// Assert that closing the window strictly reduces a count, and by the amount
/// the single `LATE` row accounts for.
fn drops(label: &str, opened: i64, shut: i64, expected_drop: i64) {
    assert_eq!(
        opened - shut,
        expected_drop,
        "{label}: open={opened} closed={shut}, expected the closed window to \
         exclude exactly {expected_drop} row(s)"
    );
}

/// [`drops`] for a query whose result also contains rows this file did not
/// seed — `seed_two_envs` lays down its own recent signal, so an exact drop is
/// not knowable there.
///
/// BOTH halves are asserted, and the second is the one that earns its keep: a
/// bound that excluded everything would satisfy `drop >= n` on its own, and
/// `remaining >= m` is what rules it out.
fn drops_at_least(label: &str, opened: i64, shut: i64, min_drop: i64, min_remaining: i64) {
    assert!(
        opened - shut >= min_drop,
        "{label}: open={opened} closed={shut}, expected the closed window to \
         exclude at least {min_drop} row(s)"
    );
    assert!(
        shut >= min_remaining,
        "{label}: closed={shut}, expected at least {min_remaining} row(s) to \
         survive — a bound that excludes everything is not a window"
    );
}

#[tokio::test]
async fn the_upper_bound_excludes_late_rows_under_every_environment_filter() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    seed_at(&mut conn, &ids, early(), "early").await;
    seed_at(&mut conn, &ids, late(), "late").await;

    for (label, env) in both_filters(&ids) {
        let scope = ReadScope::new(ids.app_id, env);

        // --- analytics_events -------------------------------------------
        let count_named = |rows: Vec<repo::EventCount>| {
            rows.iter()
                .find(|r| r.name == "cw_step_a")
                .map(|r| r.count)
                .unwrap_or(0)
        };
        let o = repo::top_events(&mut conn, scope.clone(), open(), 100)
            .await
            .expect("top_events open");
        let c = repo::top_events(&mut conn, scope.clone(), closed(), 100)
            .await
            .expect("top_events closed");
        drops(
            &format!("{label} top_events"),
            count_named(o),
            count_named(c),
            1,
        );

        let sum = |pts: Vec<repo::SeriesPoint>| pts.iter().map(|p| p.count).sum::<i64>();
        let o = repo::event_series(&mut conn, scope.clone(), Some("cw_step_a"), open())
            .await
            .expect("event_series open");
        let c = repo::event_series(&mut conn, scope.clone(), Some("cw_step_a"), closed())
            .await
            .expect("event_series closed");
        drops(&format!("{label} event_series named"), sum(o), sum(c), 1);

        // The unnamed arm takes a different bind layout (no `$3` name), so it
        // is a separate assertion rather than the same one twice.
        let o = repo::event_series(&mut conn, scope.clone(), None, open())
            .await
            .expect("event_series all open");
        let c = repo::event_series(&mut conn, scope.clone(), None, closed())
            .await
            .expect("event_series all closed");
        drops_at_least(&format!("{label} event_series all"), sum(o), sum(c), 2, 2);

        // --- error_events -----------------------------------------------
        let o = repo::error_series(&mut conn, scope.clone(), open())
            .await
            .expect("error_series open");
        let c = repo::error_series(&mut conn, scope.clone(), closed())
            .await
            .expect("error_series closed");
        drops_at_least(&format!("{label} error_series"), sum(o), sum(c), 1, 1);

        // --- overview_totals: five differently-bounded columns in one SQL --
        let o = repo::overview_totals(&mut conn, scope.clone(), open())
            .await
            .expect("overview_totals open");
        let c = repo::overview_totals(&mut conn, scope.clone(), closed())
            .await
            .expect("overview_totals closed");
        drops_at_least(&format!("{label} totals.events"), o.events, c.events, 2, 2);
        drops_at_least(&format!("{label} totals.errors"), o.errors, c.errors, 1, 1);
        drops_at_least(
            &format!("{label} totals.sessions"),
            o.sessions,
            c.sessions,
            1,
            1,
        );
        assert!(
            c.users < o.users,
            "{label}: `users` reads event_users.last_seen, which the upper \
             bound must reach too (open={}, closed={})",
            o.users,
            c.users
        );

        // --- sessions ----------------------------------------------------
        let o = repo::session_stats(&mut conn, scope.clone(), open())
            .await
            .expect("session_stats open");
        let c = repo::session_stats(&mut conn, scope.clone(), closed())
            .await
            .expect("session_stats closed");
        drops_at_least(
            &format!("{label} session_stats"),
            o.sessions,
            c.sessions,
            1,
            1,
        );
        drops_at_least(
            &format!("{label} session crashed"),
            o.crashed,
            c.crashed,
            1,
            1,
        );

        let bucket_sum = |rows: Vec<repo::HistoBucket>| rows.iter().map(|r| r.count).sum::<i64>();
        let o = repo::session_duration_histogram(&mut conn, scope.clone(), open())
            .await
            .expect("histogram open");
        let c = repo::session_duration_histogram(&mut conn, scope.clone(), closed())
            .await
            .expect("histogram closed");
        drops_at_least(
            &format!("{label} duration_histogram"),
            bucket_sum(o),
            bucket_sum(c),
            1,
            1,
        );

        // `session_duration_series` windows `started_at`, not `last_event_at`
        // — a different column from its neighbour above, and the upper bound
        // has to follow each one.
        let o = repo::session_duration_series(&mut conn, scope.clone(), open())
            .await
            .expect("duration_series open");
        let c = repo::session_duration_series(&mut conn, scope.clone(), closed())
            .await
            .expect("duration_series closed");
        drops_at_least(
            &format!("{label} duration_series buckets"),
            o.len() as i64,
            c.len() as i64,
            1,
            1,
        );

        // --- event_users: three cutoffs bound BEFORE the window's own ------
        let now = Utc::now();
        let o = repo::user_stats(&mut conn, scope.clone(), open(), now)
            .await
            .expect("user_stats open");
        let c = repo::user_stats(&mut conn, scope.clone(), closed(), now)
            .await
            .expect("user_stats closed");
        drops_at_least(
            &format!("{label} user_stats.active_in_range"),
            o.active_in_range,
            c.active_in_range,
            1,
            1,
        );
        assert_eq!(
            o.total_users, c.total_users,
            "{label}: total_users is all-time by definition and must not move"
        );

        let user_sum =
            |rows: Vec<repo::UserSeriesPoint>| rows.iter().map(|r| r.active).sum::<i64>();
        let o = repo::active_user_series(&mut conn, scope.clone(), open())
            .await
            .expect("active_user_series open");
        let c = repo::active_user_series(&mut conn, scope.clone(), closed())
            .await
            .expect("active_user_series closed");
        drops_at_least(
            &format!("{label} active_user_series"),
            user_sum(o),
            user_sum(c),
            1,
            1,
        );

        // --- transactions: env binds at $5, so the upper bound is $6 -------
        let pick = |rows: Vec<repo::PerfSummaryRow>| {
            rows.iter()
                .find(|r| r.name == "cw /checkout")
                .map(|r| r.count)
                .unwrap_or(0)
        };
        let o = repo::performance_summary(&mut conn, scope.clone(), open(), None, None)
            .await
            .expect("perf summary open");
        let c = repo::performance_summary(&mut conn, scope.clone(), closed(), None, None)
            .await
            .expect("perf summary closed");
        drops(&format!("{label} performance_summary"), pick(o), pick(c), 1);

        let tp = |rows: Vec<repo::PerfSeriesPoint>| rows.iter().map(|r| r.throughput).sum::<i64>();
        let o = repo::performance_series(&mut conn, scope.clone(), open(), None, None)
            .await
            .expect("perf series open");
        let c = repo::performance_series(&mut conn, scope.clone(), closed(), None, None)
            .await
            .expect("perf series closed");
        drops_at_least(&format!("{label} performance_series"), tp(o), tp(c), 1, 1);
    }

    db.cleanup().await;
}

/// The screens family, which shares two SQL builders (`screen_ctes`,
/// `screen_signal_union`) across five functions and pushes the upper bound past
/// `limit`/`offset` in three of them.
#[tokio::test]
async fn the_upper_bound_reaches_every_screen_query() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    seed_at(&mut conn, &ids, early(), "early").await;
    seed_at(&mut conn, &ids, late(), "late").await;

    for (label, env) in both_filters(&ids) {
        let scope = ReadScope::new(ids.app_id, env);

        let views = |rows: Vec<repo::ScreenRow>| {
            rows.iter()
                .find(|r| r.screen == "CwScreen")
                .map(|r| r.views)
                .unwrap_or(0)
        };
        let o = repo::screen_list(
            &mut conn,
            scope.clone(),
            open(),
            "%",
            500,
            0,
            common::default_screen_sort(),
        )
        .await
        .expect("screen_list open");
        let c = repo::screen_list(
            &mut conn,
            scope.clone(),
            closed(),
            "%",
            500,
            0,
            common::default_screen_sort(),
        )
        .await
        .expect("screen_list closed");
        drops(&format!("{label} screen_list views"), views(o), views(c), 1);

        let o = repo::screen_stats(&mut conn, scope.clone(), open(), "CwScreen")
            .await
            .expect("screen_stats open");
        let c = repo::screen_stats(&mut conn, scope.clone(), closed(), "CwScreen")
            .await
            .expect("screen_stats closed");
        drops(&format!("{label} screen_stats views"), o.views, c.views, 1);
        drops(
            &format!("{label} screen_stats exceptions"),
            o.exceptions,
            c.exceptions,
            1,
        );

        let o =
            repo::recent_events_for_screen(&mut conn, scope.clone(), "CwScreen", open(), 100, 0)
                .await
                .expect("recent events open");
        let c =
            repo::recent_events_for_screen(&mut conn, scope.clone(), "CwScreen", closed(), 100, 0)
                .await
                .expect("recent events closed");
        drops(
            &format!("{label} recent_events_for_screen"),
            o.len() as i64,
            c.len() as i64,
            1,
        );

        let o = repo::recent_exceptions_for_screen(
            &mut conn,
            scope.clone(),
            "CwScreen",
            open(),
            100,
            0,
        )
        .await
        .expect("recent exceptions open");
        let c = repo::recent_exceptions_for_screen(
            &mut conn,
            scope.clone(),
            "CwScreen",
            closed(),
            100,
            0,
        )
        .await
        .expect("recent exceptions closed");
        drops(
            &format!("{label} recent_exceptions_for_screen"),
            o.len() as i64,
            c.len() as i64,
            1,
        );

        // Both actor lists group the signal union by a key, so the late row
        // shows up as one fewer ACTOR — `seed_at` gives each instant its own
        // `distinct_id`/`device_key`.
        let o = repo::users_for_screen(&mut conn, scope.clone(), "CwScreen", open(), 100, 0)
            .await
            .expect("users_for_screen open");
        let c = repo::users_for_screen(&mut conn, scope.clone(), "CwScreen", closed(), 100, 0)
            .await
            .expect("users_for_screen closed");
        drops_at_least(
            &format!("{label} users_for_screen"),
            o.len() as i64,
            c.len() as i64,
            1,
            1,
        );

        let o = repo::devices_for_screen(&mut conn, scope.clone(), "CwScreen", open(), 100, 0)
            .await
            .expect("devices_for_screen open");
        let c = repo::devices_for_screen(&mut conn, scope.clone(), "CwScreen", closed(), 100, 0)
            .await
            .expect("devices_for_screen closed");
        drops_at_least(
            &format!("{label} devices_for_screen"),
            o.len() as i64,
            c.len() as i64,
            1,
            1,
        );

        // `count_screens` has TWO shapes — the candidate-probe fast path and
        // the aggregate fallback — and only the cap decides which runs. Both
        // are exercised: a cap above the fixture takes the fast path, a cap of
        // 1 forces the fallback.
        for cap in [10_000i64, 1] {
            let (o, _) = repo::count_screens(&mut conn, scope.clone(), open(), "%", cap)
                .await
                .expect("count_screens open");
            let (c, _) = repo::count_screens(&mut conn, scope.clone(), closed(), "%", cap)
                .await
                .expect("count_screens closed");
            // The screen exists in BOTH windows, so the count itself must not
            // move — what is being pinned here is that the extra bind did not
            // break either shape.
            assert!(
                o >= c,
                "{label} count_screens cap={cap}: a closed window cannot find \
                 more screens than an open one (open={o}, closed={c})"
            );
        }
    }

    db.cleanup().await;
}

/// `funnel` and `journey_graph` build their bind sequence in a LOOP, so the
/// upper bound's index depends on the step count. Two steps and four steps
/// therefore land it in different places, and both are asked.
#[tokio::test]
async fn the_upper_bound_survives_a_variable_length_bind_sequence() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    seed_at(&mut conn, &ids, early(), "early").await;
    seed_at(&mut conn, &ids, late(), "late").await;

    for (label, env) in both_filters(&ids) {
        let scope = ReadScope::new(ids.app_id, env);

        for steps in [
            vec!["cw_step_a".to_string()],
            vec!["cw_step_a".to_string(), "cw_step_a".to_string()],
            vec![
                "cw_step_a".to_string(),
                "cw_step_a".to_string(),
                "cw_step_a".to_string(),
                "cw_step_a".to_string(),
            ],
        ] {
            let n = steps.len();
            let o = repo::funnel(&mut conn, scope.clone(), &steps, open())
                .await
                .unwrap_or_else(|e| panic!("{label} funnel({n}) open: {e}"));
            let c = repo::funnel(&mut conn, scope.clone(), &steps, closed())
                .await
                .unwrap_or_else(|e| panic!("{label} funnel({n}) closed: {e}"));
            drops(
                &format!("{label} funnel({n}) step 0"),
                o[0].count,
                c[0].count,
                1,
            );
        }

        let (o_nodes, _) = repo::journey_graph(&mut conn, scope.clone(), open(), 5)
            .await
            .expect("journey open");
        let (c_nodes, _) = repo::journey_graph(&mut conn, scope.clone(), closed(), 5)
            .await
            .expect("journey closed");
        let count = |ns: Vec<repo::JourneyNode>| {
            ns.iter()
                .filter(|n| n.event == "cw_step_a")
                .map(|n| n.count)
                .sum::<i64>()
        };
        drops(
            &format!("{label} journey_graph"),
            count(o_nodes),
            count(c_nodes),
            1,
        );
    }

    db.cleanup().await;
}

/// `top_issues` has two entirely separate implementations — a boxed-diesel fast
/// path under `All`, and a LATERAL-joined raw query otherwise — so a bound
/// added to one proves nothing about the other.
#[tokio::test]
async fn both_top_issues_shapes_honour_the_upper_bound() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    seed_at(&mut conn, &ids, early(), "early").await;
    seed_at(&mut conn, &ids, late(), "late").await;

    // The raw arm reports the WINDOWED occurrence count, so closing the window
    // must lower it. The `All` arm reports the issue's own app-wide
    // `times_seen` and filters on `last_seen`, so what moves there is which
    // issues appear at all — asserted as "no more than the open window".
    let one = ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a));
    let o = repo::top_issues(&mut conn, one.clone(), open(), 50)
        .await
        .expect("top_issues one open");
    let c = repo::top_issues(&mut conn, one, closed(), 50)
        .await
        .expect("top_issues one closed");
    let seen = |rows: &[sauron_db::models::Issue]| {
        rows.iter()
            .find(|i| i.fingerprint == CW_FINGERPRINT)
            .map(|i| i.times_seen)
            .unwrap_or(0)
    };
    drops("one top_issues.times_seen", seen(&o), seen(&c), 1);

    let all = ReadScope::new(ids.app_id, EnvFilter::All);
    let o = repo::top_issues(&mut conn, all.clone(), open(), 50)
        .await
        .expect("top_issues all open");
    let c = repo::top_issues(&mut conn, all, closed(), 50)
        .await
        .expect("top_issues all closed");
    assert!(
        c.len() <= o.len(),
        "all: a closed window cannot surface MORE issues than an open one \
         (open={}, closed={})",
        o.len(),
        c.len()
    );

    db.cleanup().await;
}
