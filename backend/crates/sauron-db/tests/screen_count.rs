mod common;

use chrono::{Duration, Utc};
use common::{far_past, TestDb};
use sauron_db::models::{NewAnalyticsEvent, NewErrorEvent, NewIssue};
use sauron_db::repo;
use sauron_db::scope::{EnvFilter, Range, ReadScope};
use serde_json::json;
use uuid::Uuid;

// `count_screens` against the list it captions.
//
// The count and the list are now DIFFERENT QUERIES — the list still decides
// membership by aggregating every row in the window (`screen_ctes`' `ev UNION
// ex`), while the count enumerates candidate names off
// `(app_id, screen, occurred_at DESC)` and probes each one for a single row.
// Two shapes answering one question is exactly the arrangement that drifts
// silently: a caption reading "12" over 11 rows is not an error anywhere, and
// no type or plan check can see it. Every test here therefore asserts the two
// against EACH OTHER, not against a hand-copied number, so a future edit to
// either shape has to keep them equal.

const CAP: i64 = 10_000;

/// The list length under the same scope, window and pattern the count was given.
async fn list_len(
    conn: &mut sauron_db::PgConn,
    scope: ReadScope,
    since: chrono::DateTime<Utc>,
    pattern: &str,
) -> usize {
    repo::screen_list(
        conn,
        scope,
        Range::since(since),
        pattern,
        // Far above any screen count these fixtures produce: a truncated list
        // would make the comparison pass by measuring the limit instead.
        500,
        0,
        common::default_screen_sort(),
    )
    .await
    .expect("screen_list")
    .len()
}

#[tokio::test]
async fn count_matches_the_list_under_every_environment_scope() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    for (label, env) in [
        ("all", EnvFilter::All),
        ("env_a", EnvFilter::One(ids.env_a)),
        ("env_b", EnvFilter::One(ids.env_b)),
        ("subset", EnvFilter::Subset(vec![ids.env_a, ids.env_b])),
        ("unattributed", EnvFilter::Unattributed),
    ] {
        let scope = ReadScope::new(ids.app_id, env);
        let (total, capped) =
            repo::count_screens(&mut conn, scope.clone(), Range::since(far_past()), "%", CAP)
                .await
                .expect("count_screens");
        let rows = list_len(&mut conn, scope, far_past(), "%").await;
        assert!(!capped, "{label}: the fixture is far below the cap");
        assert_eq!(
            total, rows as i64,
            "{label}: the caption and the table it captions disagree"
        );
    }

    drop(conn);
    db.cleanup().await;
}

/// The window, which is the one thing the candidate enumeration does NOT apply.
///
/// Candidates come off the index with no time bound — deliberately, because
/// within one screen the index runs newest-first and a time predicate there
/// would walk every entry a dead screen ever had. The window is the probe's
/// job, so a screen that exists only OUTSIDE it is precisely the case that
/// would count one too many if the probe ever lost its `occurred_at` bound.
#[tokio::test]
async fn a_screen_seen_only_before_the_window_is_not_counted() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let scope = ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a));
    let since = Utc::now() - Duration::days(7);
    let (before, _) = repo::count_screens(&mut conn, scope.clone(), Range::since(since), "%", CAP)
        .await
        .expect("count before");

    // 90 days old: outside the 7-day window, inside the retained partitions.
    let ancient = Utc::now() - Duration::days(90);
    repo::insert_analytics_event(
        &mut conn,
        NewAnalyticsEvent {
            id: Uuid::new_v4(),
            app_id: ids.app_id,
            environment_id: Some(ids.env_a),
            name: "harness.retired".to_string(),
            distinct_id: "screen-count-user".to_string(),
            properties: json!({}),
            context: json!({}),
            session_id: Some("screen-count-session".to_string()),
            release: None,
            ip_address: None,
            occurred_at: ancient,
            device_key: Some("screen-count-device".to_string()),
            screen: Some("zz-retired-screen".to_string()),
            workflow_id: None,
            workflow_name: None,
            tags: json!({}),
            contexts: json!({}),
            extra: json!({}),
        },
    )
    .await
    .expect("insert out-of-window event");

    let (after, _) = repo::count_screens(&mut conn, scope.clone(), Range::since(since), "%", CAP)
        .await
        .expect("count after");
    assert_eq!(
        after, before,
        "a screen whose only row predates the window must not be counted"
    );
    assert_eq!(
        after,
        list_len(&mut conn, scope.clone(), since, "%").await as i64,
        "and the list must not show it either"
    );

    // The same screen INSIDE the window is counted — otherwise the assertion
    // above would also pass on a count that had simply stopped seeing new
    // screens at all.
    repo::insert_analytics_event(
        &mut conn,
        NewAnalyticsEvent {
            id: Uuid::new_v4(),
            app_id: ids.app_id,
            environment_id: Some(ids.env_a),
            name: "harness.revived".to_string(),
            distinct_id: "screen-count-user".to_string(),
            properties: json!({}),
            context: json!({}),
            session_id: Some("screen-count-session".to_string()),
            release: None,
            ip_address: None,
            occurred_at: Utc::now() - Duration::hours(1),
            device_key: Some("screen-count-device".to_string()),
            screen: Some("zz-retired-screen".to_string()),
            workflow_id: None,
            workflow_name: None,
            tags: json!({}),
            contexts: json!({}),
            extra: json!({}),
        },
    )
    .await
    .expect("insert in-window event");

    let (revived, _) = repo::count_screens(&mut conn, scope.clone(), Range::since(since), "%", CAP)
        .await
        .expect("count revived");
    assert_eq!(
        revived,
        before + 1,
        "the same screen, now inside the window"
    );
    assert_eq!(
        revived,
        list_len(&mut conn, scope, since, "%").await as i64,
        "list and count still agree"
    );

    drop(conn);
    db.cleanup().await;
}

/// A screen that only ever appears on `error_events`.
///
/// Membership is `ev UNION ex`, and the new shape enumerates candidates from
/// both tables and probes both. Dropping either half is invisible to any
/// fixture whose screens appear in both — which is every screen `seed_two_envs`
/// creates.
#[tokio::test]
async fn a_screen_seen_only_on_an_error_is_counted() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let scope = ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a));
    let (before, _) =
        repo::count_screens(&mut conn, scope.clone(), Range::since(far_past()), "%", CAP)
            .await
            .expect("count before");

    let now = Utc::now();
    let issue_id = repo::upsert_issue(
        &mut conn,
        NewIssue {
            app_id: ids.app_id,
            fingerprint: "screen-count-error-only",
            type_: "Error",
            title: "error-only screen",
            culprit: "harness::screen_count",
            level: "error",
            first_seen: now,
            last_seen: now,
            times_seen: 1,
        },
    )
    .await
    .expect("upsert issue");
    repo::insert_error_event(
        &mut conn,
        NewErrorEvent {
            id: Uuid::new_v4(),
            app_id: ids.app_id,
            environment_id: Some(ids.env_a),
            issue_id,
            fingerprint: "screen-count-error-only".into(),
            level: "error".into(),
            message: "error-only screen".into(),
            exception_type: "HarnessError".into(),
            exception_value: "seeded".into(),
            stacktrace: json!([]),
            breadcrumbs: json!([]),
            context: json!({}),
            tags: json!({}),
            release: None,
            distinct_id: Some("screen-count-user".to_string()),
            event_user: None,
            sdk: None,
            ip_address: None,
            occurred_at: now,
            session_id: Some("screen-count-session".to_string()),
            device_key: Some("screen-count-device".to_string()),
            screen: Some("zz-crash-only-screen".to_string()),
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
        },
    )
    .await
    .expect("insert error-only screen");

    let (after, _) =
        repo::count_screens(&mut conn, scope.clone(), Range::since(far_past()), "%", CAP)
            .await
            .expect("count after");
    assert_eq!(after, before + 1, "an error-only screen is still a screen");
    assert_eq!(
        after,
        list_len(&mut conn, scope, far_past(), "%").await as i64,
        "list and count agree on it"
    );

    drop(conn);
    db.cleanup().await;
}

/// The search term, which the candidate enumeration applies and the probe does
/// not. A pattern dropped on either path shows up as a count that ignores the
/// box the user typed in.
#[tokio::test]
async fn the_search_pattern_narrows_the_count_the_way_it_narrows_the_list() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let scope = ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a));
    for pattern in [
        repo::like_contains("home"),
        repo::like_contains("out"), // 'checkout', by substring
        repo::like_contains("no-such-screen"),
    ] {
        let (total, _) = repo::count_screens(
            &mut conn,
            scope.clone(),
            Range::since(far_past()),
            &pattern,
            CAP,
        )
        .await
        .expect("count_screens");
        assert_eq!(
            total,
            list_len(&mut conn, scope.clone(), far_past(), &pattern).await as i64,
            "pattern {pattern:?}: caption and table disagree"
        );
    }

    drop(conn);
    db.cleanup().await;
}

/// The fallback, reached by making the cap smaller than the fixture.
///
/// With `cap = 1` the candidate enumeration is asked for 2 names, gets them,
/// and hands over to the aggregate shape — the pre-2026-08-18 query, kept
/// precisely so a pathological screen cardinality still gets an exact, capped
/// answer instead of a slow one. Without a test the fallback is unreachable
/// code that compiles.
#[tokio::test]
async fn a_cap_below_the_candidate_count_falls_back_and_still_caps_honestly() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let scope = ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a));
    let rows = list_len(&mut conn, scope.clone(), far_past(), "%").await as i64;
    assert!(
        rows >= 2,
        "fixture must carry at least two screens for this to exercise the fallback"
    );

    let (total, capped) =
        repo::count_screens(&mut conn, scope.clone(), Range::since(far_past()), "%", 1)
            .await
            .expect("count_screens at cap 1");
    assert_eq!(total, 1, "capped counts report the cap, not the true total");
    assert!(capped, "and say so");

    // A cap at exactly the true total is the boundary the `cap + 1` sentinel
    // exists for: it must NOT report capped, on either path.
    let (exact, exact_capped) =
        repo::count_screens(&mut conn, scope, Range::since(far_past()), "%", rows)
            .await
            .expect("count_screens at the exact total");
    assert_eq!(exact, rows);
    assert!(
        !exact_capped,
        "a total that exactly fills the cap is not truncated"
    );

    drop(conn);
    db.cleanup().await;
}
