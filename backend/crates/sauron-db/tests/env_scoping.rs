mod common;

use chrono::{Duration, Utc};
use common::{count_in_env, count_rows, distinct_envs_for_identity, far_past, TestDb};
use diesel::prelude::*;
use diesel::sql_types::{BigInt, Text, Uuid as SqlUuid};
use diesel_async::RunQueryDsl;
use sauron_auth::{authorize_env_read, perm, AuthError};
use sauron_db::models::{NewAnalyticsEvent, NewErrorEvent, NewTransaction, Workflow};
use sauron_db::repo::SortSpec;
use sauron_db::schema::workflows;
use sauron_db::scope::{EnvFilter, ReadScope};
use serde_json::json;
use uuid::Uuid;

#[derive(QueryableByName)]
struct CountRow {
    #[diesel(sql_type = BigInt)]
    n: i64,
}

/// The default ordering `routes::devices::list` builds. This file asserts on
/// environment scoping, never on order, so every device call site below takes
/// the default — spelled as a function because `SortSpec` is passed by value.
fn device_sort() -> SortSpec {
    SortSpec {
        column: "last_seen",
        descending: true,
        tiebreak: "d.device_key",
        nulls_last: false,
    }
}

/// The default ordering `routes::devices::groups` builds. Same reasoning as
/// [`device_sort`].
fn group_sort() -> SortSpec {
    SortSpec {
        column: "last_seen",
        descending: true,
        tiebreak: "d.family, d.model, d.os_name, d.os_version",
        nulls_last: false,
    }
}

/// The harness itself works: it can reach a database, seed two environments,
/// and read back exactly what it wrote. Everything else in this file depends on
/// this being true, so it is asserted first and separately.
#[tokio::test]
async fn harness_seeds_two_isolated_environments() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    assert_ne!(ids.env_a, ids.env_b);

    let mut conn = db.conn().await;

    // The asymmetric, per-table-distinct seed counts are the whole point of
    // this harness: identical tuples across tables would let a swapped
    // sub-select or an off-by-one environment filter pass every later task's
    // tests silently. See the doc comment on `SeedIds` for why each table's
    // tuple is different from the others.
    for (table, a, b, none) in [
        ("analytics_events", 5, 5, 1),
        ("error_events", 4, 2, 1),
        ("sessions", 3, 3, 1),
        ("transactions", 5, 2, 1),
    ] {
        assert_eq!(
            count_in_env(&mut conn, table, ids.app_id, Some(ids.env_a)).await,
            a,
            "{table}: expected {a} rows in env_a"
        );
        assert_eq!(
            count_in_env(&mut conn, table, ids.app_id, Some(ids.env_b)).await,
            b,
            "{table}: expected {b} rows in env_b"
        );
        assert_eq!(
            count_in_env(&mut conn, table, ids.app_id, None).await,
            none,
            "{table}: expected {none} row(s) with environment_id NULL"
        );
    }

    // `issue_id` spans all three buckets: 4 of env_a's error_events, 1 of
    // env_b's 2, and the 1 unattributed row — 6 total, matching its stored
    // `times_seen`. `issue_env_b_only` is the other row in env_b — 1 total,
    // confined to env_b alone. Task 9's membership bug (a `LEFT JOIN LATERAL`
    // instead of an inner join/`EXISTS`) is invisible unless a second issue
    // that must NOT appear under `One(env_a)` actually exists.
    let issue_id_count: CountRow =
        diesel::sql_query("SELECT count(*)::bigint AS n FROM error_events WHERE issue_id = $1")
            .bind::<SqlUuid, _>(ids.issue_id)
            .get_result(&mut conn)
            .await
            .expect("count error events for issue_id");
    assert_eq!(
        issue_id_count.n, 6,
        "issue_id should have 6 error_events (matches its times_seen)"
    );

    let issue_env_b_only_count: CountRow =
        diesel::sql_query("SELECT count(*)::bigint AS n FROM error_events WHERE issue_id = $1")
            .bind::<SqlUuid, _>(ids.issue_env_b_only)
            .get_result(&mut conn)
            .await
            .expect("count error events for issue_env_b_only");
    assert_eq!(
        issue_env_b_only_count.n, 1,
        "issue_env_b_only should have exactly 1 error event"
    );

    let issue_env_b_only_outside_b: CountRow = diesel::sql_query(
        "SELECT count(*)::bigint AS n FROM error_events \
         WHERE issue_id = $1 AND (environment_id IS DISTINCT FROM $2)",
    )
    .bind::<SqlUuid, _>(ids.issue_env_b_only)
    .bind::<SqlUuid, _>(ids.env_b)
    .get_result(&mut conn)
    .await
    .expect("count issue_env_b_only rows outside env_b");
    assert_eq!(
        issue_env_b_only_outside_b.n, 0,
        "issue_env_b_only must never appear in env_a or unattributed"
    );

    // These three guard the fixture six later tasks depend on. Without them a refactor
    // of `note_identity` could silently empty event_users/devices, and every Task 8
    // assertion would revert to `0 == 0` — passing identically for correct code and for
    // a read with no environment filter at all.
    //
    // 8, not 7: a Task 8 review round added `session_only_distinct_id`/
    // `session_only_device_key` — see `SeedIds`'s doc comment — to close a gap
    // where no identity qualified for an environment solely via `sessions`.
    assert_eq!(count_rows(&mut conn, "event_users", ids.app_id).await, 8);
    assert_eq!(count_rows(&mut conn, "devices", ids.app_id).await, 8);
    // The load-bearing one: the shared identity must really appear in BOTH environments.
    assert_eq!(
        distinct_envs_for_identity(&mut conn, ids.app_id, &ids.shared_distinct_id).await,
        2,
    );

    drop(conn);
    db.cleanup().await;
}

/// `list_sessions` (a boxed-diesel read) must honor `ReadScope` for every one
/// of the four cases: a single environment, the other single environment,
/// the unattributed bucket, and `All`. `sessions` is seeded 3/3/1 (see the
/// `SeedIds` doc comment) — `env_a` and `env_b` hold the SAME count, so a
/// length-only assertion could not tell a correct filter from a swapped one;
/// every branch below also asserts `environment_id` on each returned row.
#[tokio::test]
async fn list_sessions_returns_only_the_selected_environment() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let a = sauron_db::repo::list_sessions(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        far_past(),
        100,
        0,
        common::default_session_sort(),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(a.len(), 3, "env_a was seeded with 3 sessions");
    assert!(a.iter().all(|s| s.environment_id == Some(ids.env_a)));

    let b = sauron_db::repo::list_sessions(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
        far_past(),
        100,
        0,
        common::default_session_sort(),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(b.len(), 3, "env_b was seeded with 3 sessions");
    assert!(b.iter().all(|s| s.environment_id == Some(ids.env_b)));

    let none = sauron_db::repo::list_sessions(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::Unattributed),
        far_past(),
        100,
        0,
        common::default_session_sort(),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(none.len(), 1);
    assert!(none.iter().all(|s| s.environment_id.is_none()));

    let all = sauron_db::repo::list_sessions(
        &mut conn,
        ReadScope::all(ids.app_id),
        far_past(),
        100,
        0,
        common::default_session_sort(),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        all.len(),
        7,
        "All must equal the sum of the parts, including unattributed"
    );

    // NOTE: sessions are seeded 3/3/1, so env_a and env_b have the SAME count. A test that
    // asserts only lengths cannot tell a swapped filter from a correct one here — assert on
    // `environment_id` per row (as above) or on `avg_session_ms`, which differs by design.
    // The per-table counts are deliberately NOT uniform; see the `SeedIds` doc comment for
    // the authoritative table.

    // `conn` holds the pool's only connection (`TestDb`'s pool is size 1) — it must be
    // dropped before `cleanup()`, which needs its own connection to run the DELETE, or the
    // checkout deadlocks until the 5s pool-wait timeout (see `harness_seeds_two_isolated_environments`).
    drop(conn);
    db.cleanup().await;
}

/// Seeds one child row of each kind (`analytics_events`/`error_events`/`transactions`)
/// sharing `session_id`, tagged `env`. Helper for
/// `session_detail_reads_are_scoped_independently_of_the_sessions_own_label` only.
#[allow(clippy::too_many_arguments)]
async fn seed_cross_env_session_child_rows(
    conn: &mut sauron_db::PgConn,
    app_id: Uuid,
    env: Uuid,
    issue_id: Uuid,
    session_id: &str,
    distinct_id: &str,
    device_key: &str,
    at: chrono::DateTime<chrono::Utc>,
) {
    sauron_db::repo::insert_analytics_event(
        conn,
        NewAnalyticsEvent {
            id: Uuid::new_v4(),
            app_id,
            environment_id: Some(env),
            name: "cross.flip".to_string(),
            distinct_id: distinct_id.to_string(),
            properties: json!({}),
            context: json!({}),
            session_id: Some(session_id.to_string()),
            release: None,
            ip_address: None,
            occurred_at: at,
            device_key: Some(device_key.to_string()),
            screen: None,
            workflow_id: None,
            workflow_name: None,
            tags: json!({}),
            contexts: json!({}),
            extra: json!({}),
        },
    )
    .await
    .expect("insert cross-env analytics event");

    sauron_db::repo::insert_error_event(
        conn,
        NewErrorEvent {
            id: Uuid::new_v4(),
            app_id,
            environment_id: Some(env),
            issue_id,
            fingerprint: "harness-fingerprint".to_string(),
            level: "error".into(),
            message: format!("cross flip error {}", Uuid::new_v4().simple()),
            exception_type: "HarnessError".into(),
            exception_value: "seeded".into(),
            stacktrace: json!([]),
            breadcrumbs: json!([]),
            context: json!({}),
            tags: json!({}),
            release: None,
            distinct_id: Some(distinct_id.to_string()),
            event_user: None,
            sdk: None,
            ip_address: None,
            occurred_at: at,
            session_id: Some(session_id.to_string()),
            device_key: Some(device_key.to_string()),
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
        },
    )
    .await
    .expect("insert cross-env error event");

    sauron_db::repo::insert_transaction(
        conn,
        NewTransaction {
            id: Uuid::new_v4(),
            app_id,
            environment_id: Some(env),
            name: "cross.flip".into(),
            op: "test".into(),
            duration_ms: 1.0,
            status: None,
            http_method: None,
            http_status: None,
            url: None,
            distinct_id: None,
            session_id: Some(session_id.to_string()),
            device_key: None,
            workflow_id: None,
            workflow_name: None,
            release: None,
            ip_address: None,
            occurred_at: at,
            finished_at: None,
            tags: serde_json::json!({}),
            extra: serde_json::json!({}),
        },
    )
    .await
    .expect("insert cross-env transaction");
}

/// The review that added `ReadScope` to `get_session`/`events_for_session`/
/// `errors_for_session`/`transactions_for_session` found the schema does not support Task 6's
/// "a session belongs to one environment so its children are already disambiguated" reasoning:
/// only `sessions` has `UNIQUE (app_id, session_id)`; the three child tables' `session_id` is
/// nullable free text with no uniqueness and no environment linkage, and `bump_session`'s
/// `environment_id = COALESCE(EXCLUDED.environment_id, sessions.environment_id)` lets the most
/// recent non-null write flip a session's own label while its already-written children stay put
/// — the shape a device repointed from staging to prod without a fresh session id produces.
///
/// This seeds exactly that: one session bumped first with `env_a`, then again with `env_b` (so
/// its *current* label is `env_b`, even though it started in `env_a`), plus one child row of
/// each kind tagged `env_a` and one tagged `env_b`, all sharing the same `session_id`.
#[tokio::test]
async fn session_detail_reads_are_scoped_independently_of_the_sessions_own_label() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let session_id = format!("cross-env-flip-{}", Uuid::new_v4().simple());
    let t0 = far_past() + Duration::days(1);
    let t1 = t0 + Duration::seconds(60);

    // First bump labels the session env_a; the second, later bump relabels it env_b via
    // `bump_session`'s COALESCE — the session's row is env_b now, though it started in env_a.
    sauron_db::repo::bump_session(
        &mut conn,
        ids.app_id,
        &session_id,
        Some("cross-flip-user"),
        Some("cross-flip-device"),
        t0,
        &json!({}),
        None,
        Some(ids.env_a),
        None,
        1,
        0,
        0,
    )
    .await
    .expect("bump session into env_a");
    sauron_db::repo::bump_session(
        &mut conn,
        ids.app_id,
        &session_id,
        None,
        None,
        t1,
        &json!({}),
        None,
        Some(ids.env_b),
        None,
        1,
        0,
        0,
    )
    .await
    .expect("bump session into env_b");

    seed_cross_env_session_child_rows(
        &mut conn,
        ids.app_id,
        ids.env_a,
        ids.issue_id,
        &session_id,
        "cross-flip-user",
        "cross-flip-device",
        t0,
    )
    .await;
    seed_cross_env_session_child_rows(
        &mut conn,
        ids.app_id,
        ids.env_b,
        ids.issue_id,
        &session_id,
        "cross-flip-user",
        "cross-flip-device",
        t1,
    )
    .await;

    // -- get_session: fails narrow, does not fall back to any child's environment ----------
    let by_b = sauron_db::repo::get_session(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
        &session_id,
    )
    .await
    .unwrap();
    assert_eq!(
        by_b.map(|s| s.environment_id),
        Some(Some(ids.env_b)),
        "the session's CURRENT label is env_b — the later bump wins"
    );

    let by_a = sauron_db::repo::get_session(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        &session_id,
    )
    .await
    .unwrap();
    assert!(
        by_a.is_none(),
        "the session's label is env_b now, even though it started in env_a and has an \
         env_a-tagged child — env_a scope must not see it (fail narrow -> 404, not fall back)"
    );

    let by_none = sauron_db::repo::get_session(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::Unattributed),
        &session_id,
    )
    .await
    .unwrap();
    assert!(by_none.is_none());

    let by_all = sauron_db::repo::get_session(&mut conn, ReadScope::all(ids.app_id), &session_id)
        .await
        .unwrap();
    assert!(by_all.is_some());

    // -- children: scoped on their OWN environment_id, independent of the session's label ---
    let events_a = sauron_db::repo::events_for_session(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        &session_id,
        10,
    )
    .await
    .unwrap();
    assert_eq!(
        events_a.len(),
        1,
        "env_a scope returns the env_a-tagged child even though the session's own current \
         label is env_b — proves the filter is not inherited from the session row"
    );
    assert_eq!(events_a[0].environment_id, Some(ids.env_a));

    let events_b = sauron_db::repo::events_for_session(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
        &session_id,
        10,
    )
    .await
    .unwrap();
    assert_eq!(events_b.len(), 1);
    assert_eq!(events_b[0].environment_id, Some(ids.env_b));

    let events_all =
        sauron_db::repo::events_for_session(&mut conn, ReadScope::all(ids.app_id), &session_id, 10)
            .await
            .unwrap();
    assert_eq!(events_all.len(), 2, "All must equal the sum of the parts");

    let errors_a = sauron_db::repo::errors_for_session(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        &session_id,
        10,
    )
    .await
    .unwrap();
    assert_eq!(errors_a.len(), 1);
    assert_eq!(errors_a[0].environment_id, Some(ids.env_a));

    let errors_b = sauron_db::repo::errors_for_session(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
        &session_id,
        10,
    )
    .await
    .unwrap();
    assert_eq!(errors_b.len(), 1);
    assert_eq!(errors_b[0].environment_id, Some(ids.env_b));

    let errors_all =
        sauron_db::repo::errors_for_session(&mut conn, ReadScope::all(ids.app_id), &session_id, 10)
            .await
            .unwrap();
    assert_eq!(errors_all.len(), 2);

    let txns_a = sauron_db::repo::transactions_for_session(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        &session_id,
        10,
    )
    .await
    .unwrap();
    assert_eq!(txns_a.len(), 1);
    assert_eq!(txns_a[0].environment_id, Some(ids.env_a));

    let txns_b = sauron_db::repo::transactions_for_session(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
        &session_id,
        10,
    )
    .await
    .unwrap();
    assert_eq!(txns_b.len(), 1);
    assert_eq!(txns_b[0].environment_id, Some(ids.env_b));

    let txns_all = sauron_db::repo::transactions_for_session(
        &mut conn,
        ReadScope::all(ids.app_id),
        &session_id,
        10,
    )
    .await
    .unwrap();
    assert_eq!(txns_all.len(), 2);

    drop(conn);
    db.cleanup().await;
}

/// `list_analytics_events` (the other boxed-diesel read this task threads
/// `ReadScope` through) must honor all four cases too.
///
/// The raw `analytics_events` table is seeded 5/5/1 (see the `SeedIds` doc
/// comment) — but `list_analytics_events` itself excludes synthetic
/// `name = '$screen'` rows (those belong to the Screens section, not the
/// event stream), and the seed's `'$screen'` rows are NOT split evenly: one
/// in `env_a` (of its 5) and two in `env_b` (of its 5). So what this
/// function returns is 4 / 3 / 1 / 8, not 5 / 5 / 1 / 11 — the `$screen`
/// exclusion already makes `env_a` and `env_b`'s lengths differ, but every
/// row is still checked for `environment_id` too, both for defense in depth
/// and because the underlying table counts (5/5) are the ones that are
/// actually equal.
#[tokio::test]
async fn list_analytics_events_returns_only_the_selected_environment() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let a = sauron_db::repo::list_analytics_events(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        &[],
        None,
        Some(far_past()),
        100,
        0,
    )
    .await
    .unwrap();
    assert_eq!(
        a.len(),
        4,
        "env_a was seeded with 5 analytics events, 1 of them '$screen' (excluded)"
    );
    assert!(a.iter().all(|e| e.environment_id == Some(ids.env_a)));

    let b = sauron_db::repo::list_analytics_events(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
        &[],
        None,
        Some(far_past()),
        100,
        0,
    )
    .await
    .unwrap();
    assert_eq!(
        b.len(),
        3,
        "env_b was seeded with 5 analytics events, 2 of them '$screen' (excluded)"
    );
    assert!(b.iter().all(|e| e.environment_id == Some(ids.env_b)));

    let none = sauron_db::repo::list_analytics_events(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::Unattributed),
        &[],
        None,
        Some(far_past()),
        100,
        0,
    )
    .await
    .unwrap();
    assert_eq!(none.len(), 1);
    assert!(none.iter().all(|e| e.environment_id.is_none()));

    let all = sauron_db::repo::list_analytics_events(
        &mut conn,
        ReadScope::all(ids.app_id),
        &[],
        None,
        Some(far_past()),
        100,
        0,
    )
    .await
    .unwrap();
    assert_eq!(
        all.len(),
        8,
        "All must equal the sum of the parts, including unattributed (11 raw rows minus 3 '$screen')"
    );

    // See the comment in `list_sessions_returns_only_the_selected_environment`: `conn`
    // must be dropped before `cleanup()` or the single-connection pool deadlocks.
    drop(conn);
    db.cleanup().await;
}

/// `top_events` (a hand-written raw-SQL read) must honor `ReadScope`. `analytics_events`
/// is seeded 5/5/1 — env_a and env_b hold the SAME total, so the total alone cannot tell a
/// correct filter from a swapped one; the seed also gives the two environments different
/// event-name mixes (see `SeedIds`/`seed_two_envs`), so the per-name breakdown must differ.
#[tokio::test]
async fn top_events_counts_only_the_selected_environment() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let a = sauron_db::repo::top_events(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        far_past(),
        50,
    )
    .await
    .unwrap();
    let a_total: i64 = a.iter().map(|r| r.count).sum();
    assert_eq!(a_total, 5, "analytics_events is seeded 5/5/1");

    let all = sauron_db::repo::top_events(&mut conn, ReadScope::all(ids.app_id), far_past(), 50)
        .await
        .unwrap();
    let all_total: i64 = all.iter().map(|r| r.count).sum();
    assert_eq!(all_total, 11, "All includes the unattributed row");

    // env_a and env_b both hold 5 analytics rows, so this total alone cannot tell a
    // correct filter from a swapped one. Task 15 (F9,
    // `.superpowers/sdd/s2-final-review.md`): a bare `assert_ne!(a, b)` here would
    // pass for ANY difference, including a swapped filter that still produced *some*
    // other wrong-but-different breakdown — so assert each environment's exact
    // name→count map instead. Collected into a `BTreeMap` rather than compared as the
    // raw `ORDER BY count DESC` vecs: several names tie at count 1 within one
    // environment, and Postgres does not guarantee tie order without a secondary
    // `ORDER BY` key, which would make a vec-order comparison flaky.
    let a_map: std::collections::BTreeMap<String, i64> =
        a.iter().map(|r| (r.name.clone(), r.count)).collect();
    assert_eq!(
        a_map,
        std::collections::BTreeMap::from([
            ("$screen".to_string(), 1),
            ("harness.event".to_string(), 1),
            ("harness.funnel.step1".to_string(), 2),
            ("harness.funnel.step2".to_string(), 1),
        ]),
        "env_a's exact name→count map"
    );

    let b = sauron_db::repo::top_events(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
        far_past(),
        50,
    )
    .await
    .unwrap();
    let b_map: std::collections::BTreeMap<String, i64> =
        b.iter().map(|r| (r.name.clone(), r.count)).collect();
    assert_eq!(
        b_map,
        std::collections::BTreeMap::from([
            ("$screen".to_string(), 2),
            ("harness.funnel.step1".to_string(), 1),
            ("harness.funnel.step2".to_string(), 2),
        ]),
        "env_b's exact name→count map — different keys AND counts from env_a, not just a \
         different total"
    );

    let none = sauron_db::repo::top_events(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::Unattributed),
        far_past(),
        50,
    )
    .await
    .unwrap();
    let none_total: i64 = none.iter().map(|r| r.count).sum();
    assert_eq!(none_total, 1);

    drop(conn);
    db.cleanup().await;
}

/// `event_series` has NO `$screen` exclusion (unlike `list_analytics_events`), so it must
/// see the full 5/5/1/11 counts. Exercises both match arms (`name: Some(_)` and `None`) since
/// each builds a differently-shaped SQL string with its own bind sequence — the env fragment
/// must land at the right index in both.
#[tokio::test]
async fn event_series_counts_only_the_selected_environment() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    // `name: None` arm.
    let a = sauron_db::repo::event_series(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        None,
        far_past(),
    )
    .await
    .unwrap();
    let a_total: i64 = a.iter().map(|p| p.count).sum();
    assert_eq!(
        a_total, 5,
        "analytics_events is seeded 5/5/1, no $screen filter here"
    );

    let b = sauron_db::repo::event_series(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
        None,
        far_past(),
    )
    .await
    .unwrap();
    let b_total: i64 = b.iter().map(|p| p.count).sum();
    assert_eq!(b_total, 5);

    let none = sauron_db::repo::event_series(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::Unattributed),
        None,
        far_past(),
    )
    .await
    .unwrap();
    let none_total: i64 = none.iter().map(|p| p.count).sum();
    assert_eq!(none_total, 1);

    let all =
        sauron_db::repo::event_series(&mut conn, ReadScope::all(ids.app_id), None, far_past())
            .await
            .unwrap();
    let all_total: i64 = all.iter().map(|p| p.count).sum();
    assert_eq!(
        all_total, 11,
        "All must equal the sum of the parts, including unattributed"
    );

    // `name: Some(_)` arm — a different SQL string with its own bind sequence. "harness.event"
    // is only ever seeded in env_a (the baseline row); env_b has none, which is a much
    // stronger signal than a total (a broken filter would surface as a nonzero count instead
    // of a swapped value).
    let named_a = sauron_db::repo::event_series(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        Some("harness.event"),
        far_past(),
    )
    .await
    .unwrap();
    let named_a_total: i64 = named_a.iter().map(|p| p.count).sum();
    assert_eq!(named_a_total, 1, "'harness.event' is only seeded in env_a");

    let named_b = sauron_db::repo::event_series(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
        Some("harness.event"),
        far_past(),
    )
    .await
    .unwrap();
    let named_b_total: i64 = named_b.iter().map(|p| p.count).sum();
    assert_eq!(
        named_b_total, 0,
        "'harness.event' was never seeded in env_b"
    );

    drop(conn);
    db.cleanup().await;
}

/// `error_events` is seeded 4/2/1 — unlike `analytics_events`/`sessions`, env_a and env_b
/// hold DIFFERENT counts, so a total-only assertion already discriminates a swapped filter.
#[tokio::test]
async fn error_series_counts_only_the_selected_environment() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let a = sauron_db::repo::error_series(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        far_past(),
    )
    .await
    .unwrap();
    let a_total: i64 = a.iter().map(|p| p.count).sum();
    assert_eq!(a_total, 4, "error_events is seeded 4/2/1");

    let b = sauron_db::repo::error_series(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
        far_past(),
    )
    .await
    .unwrap();
    let b_total: i64 = b.iter().map(|p| p.count).sum();
    assert_eq!(b_total, 2);

    let none = sauron_db::repo::error_series(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::Unattributed),
        far_past(),
    )
    .await
    .unwrap();
    let none_total: i64 = none.iter().map(|p| p.count).sum();
    assert_eq!(none_total, 1);

    let all = sauron_db::repo::error_series(&mut conn, ReadScope::all(ids.app_id), far_past())
        .await
        .unwrap();
    let all_total: i64 = all.iter().map(|p| p.count).sum();
    assert_eq!(
        all_total, 7,
        "All must equal the sum of the parts, including unattributed"
    );

    drop(conn);
    db.cleanup().await;
}

/// `journey_graph` step-numbers each `distinct_id`'s events by `occurred_at` order, so the
/// SUM of node counts always equals the row count passing the WHERE/depth filter regardless
/// of how those rows get bucketed into (step, name) groups — that sum is what a missing/
/// swapped env fragment would get wrong (env_a-scoped would leak to the full 11-row app-wide
/// graph instead of just its own 5). `analytics_events` is 5/5/1 (equal env_a/env_b totals),
/// so beyond the sum this also asserts the actual node SET differs: env_a's step1 lands on
/// "harness.event" (`shared_distinct_id`'s baseline event), which env_b's timeline never has.
#[tokio::test]
async fn journey_graph_counts_only_the_selected_environment() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let (nodes_a, _) = sauron_db::repo::journey_graph(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        far_past(),
        10,
    )
    .await
    .unwrap();
    let a_total: i64 = nodes_a.iter().map(|n| n.count).sum();
    assert_eq!(
        a_total, 5,
        "analytics_events is seeded 5/5/1, no filter excludes rows here"
    );

    let (nodes_b, _) = sauron_db::repo::journey_graph(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
        far_past(),
        10,
    )
    .await
    .unwrap();
    let b_total: i64 = nodes_b.iter().map(|n| n.count).sum();
    assert_eq!(b_total, 5);

    // env_a and env_b both sum to 5 — assert the node SET differs too, not just the total.
    let mut a_pairs: Vec<(i64, String)> =
        nodes_a.iter().map(|n| (n.step, n.event.clone())).collect();
    let mut b_pairs: Vec<(i64, String)> =
        nodes_b.iter().map(|n| (n.step, n.event.clone())).collect();
    a_pairs.sort();
    b_pairs.sort();
    assert_ne!(
        a_pairs, b_pairs,
        "env_a and env_b must differ by node set, not just by total"
    );
    assert!(
        a_pairs.iter().any(|(_, e)| e == "harness.event"),
        "env_a's timeline includes the 'harness.event' baseline row: {a_pairs:?}"
    );
    assert!(
        !b_pairs.iter().any(|(_, e)| e == "harness.event"),
        "env_b never saw 'harness.event': {b_pairs:?}"
    );

    let (nodes_none, _) = sauron_db::repo::journey_graph(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::Unattributed),
        far_past(),
        10,
    )
    .await
    .unwrap();
    let none_total: i64 = nodes_none.iter().map(|n| n.count).sum();
    assert_eq!(none_total, 1);

    let (nodes_all, _) =
        sauron_db::repo::journey_graph(&mut conn, ReadScope::all(ids.app_id), far_past(), 10)
            .await
            .unwrap();
    let all_total: i64 = nodes_all.iter().map(|n| n.count).sum();
    assert_eq!(
        all_total, 11,
        "All must equal the sum of the parts, including unattributed"
    );

    drop(conn);
    db.cleanup().await;
}

/// `performance_summary` — `transactions` is seeded 5/2/1, both `count` and `error_rate`
/// (0.2 vs 0.5) discriminate env_a from env_b. `op`/`device_key` stay `None` here so this
/// exercises the env fragment appended after the pre-existing `($3::text IS NULL OR op=$3)`
/// idiom without colliding with it.
#[tokio::test]
async fn performance_summary_counts_only_the_selected_environment() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let a = sauron_db::repo::performance_summary(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        far_past(),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        a.len(),
        1,
        "one (name, op) group: 'harness.transaction'/'test'"
    );
    assert_eq!(a[0].count, 5, "transactions is seeded 5/2/1");
    assert!(
        (a[0].error_rate - 0.2).abs() < 1e-9,
        "1 error out of 5: {}",
        a[0].error_rate
    );
    assert!(
        (a[0].avg - 30.0).abs() < 1e-9,
        "avg of 10/20/30/40/50: {}",
        a[0].avg
    );

    let b = sauron_db::repo::performance_summary(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
        far_past(),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(b.len(), 1);
    assert_eq!(b[0].count, 2);
    assert!(
        (b[0].error_rate - 0.5).abs() < 1e-9,
        "1 error out of 2: {}",
        b[0].error_rate
    );
    assert!(
        (b[0].avg - 150.0).abs() < 1e-9,
        "avg of 100/200: {}",
        b[0].avg
    );

    let none = sauron_db::repo::performance_summary(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::Unattributed),
        far_past(),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(none.len(), 1);
    assert_eq!(none[0].count, 1);

    let all = sauron_db::repo::performance_summary(
        &mut conn,
        ReadScope::all(ids.app_id),
        far_past(),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(
        all[0].count, 8,
        "All must equal the sum of the parts, including unattributed"
    );

    drop(conn);
    db.cleanup().await;
}

/// `performance_series` — same 5/2/1 transactions seed as `performance_summary`, bucketed by
/// hour. `name`/`op` stay `None` so this exercises the env fragment appended after that
/// function's own pre-existing optional-filter idiom too.
#[tokio::test]
async fn performance_series_counts_only_the_selected_environment() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let a = sauron_db::repo::performance_series(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        far_past(),
        None,
        None,
    )
    .await
    .unwrap();
    let a_total: i64 = a.iter().map(|p| p.throughput).sum();
    assert_eq!(a_total, 5, "transactions is seeded 5/2/1");

    let b = sauron_db::repo::performance_series(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
        far_past(),
        None,
        None,
    )
    .await
    .unwrap();
    let b_total: i64 = b.iter().map(|p| p.throughput).sum();
    assert_eq!(b_total, 2);

    let none = sauron_db::repo::performance_series(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::Unattributed),
        far_past(),
        None,
        None,
    )
    .await
    .unwrap();
    let none_total: i64 = none.iter().map(|p| p.throughput).sum();
    assert_eq!(none_total, 1);

    let all = sauron_db::repo::performance_series(
        &mut conn,
        ReadScope::all(ids.app_id),
        far_past(),
        None,
        None,
    )
    .await
    .unwrap();
    let all_total: i64 = all.iter().map(|p| p.throughput).sum();
    assert_eq!(
        all_total, 8,
        "All must equal the sum of the parts, including unattributed"
    );

    drop(conn);
    db.cleanup().await;
}

/// `overview_totals` aggregates across four tables in one statement. `events` (analytics,
/// 5/5/1), `sessions` (3/3/1) and `crashed_sessions` (1/1/0) all tie between env_a/env_b, so
/// `errors` (error_events, 4/2/1 — the one field that actually differs) is the load-bearing
/// discriminator for a swapped filter.
///
/// `users`/`new_users` come from `event_users`, which carries no `environment_id` column at
/// all, so they are scoped by membership (activity in analytics_events/error_events/sessions
/// for this environment) — the gap Task 8 deferred, closed by this fix. Membership per
/// identity, hand-derived from `seed_two_envs` (see `SeedIds`'s doc comment):
///   - `shared_distinct_id`: env_a (analytics+error+session) AND env_b (error+session)
///   - `distinct_id_cross_env`: env_a (analytics+error) AND env_b (analytics)
///   - `distinct_id_env_b_only`: env_b only
///   - the two env_a-only error identities (`a-er-1`, `a-er-3`): env_a only
///   - `session_only_distinct_id`: env_a only (sessions leg alone)
///   - the two unattributed-only identities (`none-an-0`, `none-er-0`): Unattributed only
/// So `One(env_a)` = {shared, cross_env, a-er-1, a-er-3, session_only} = 5;
/// `One(env_b)` = {shared, cross_env, env_b_only} = 3; `Unattributed` = {none-an-0,
/// none-er-0} = 2; `All` = all 8 (union, not a sum — shared/cross_env are each counted once
/// despite belonging to two environments, unlike a naive `5+3+2=10`).
///
/// `since` is `far_past()`, so every identity's `last_seen`/`first_seen` (both wall-clock
/// timestamps stamped by `note_identity` at seed time, not derived from the seed's own
/// `occurred_at` offsets) clears the bound — `users`/`new_users` therefore land on the same
/// per-scope numbers as the membership counts above.
#[tokio::test]
async fn overview_totals_counts_only_the_selected_environment() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let a = sauron_db::repo::overview_totals(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        far_past(),
    )
    .await
    .unwrap();
    assert_eq!(a.events, 5, "analytics_events is seeded 5/5/1");
    assert_eq!(
        a.errors, 4,
        "error_events is seeded 4/2/1 — the discriminating field"
    );
    assert_eq!(a.sessions, 3, "sessions is seeded 3/3/1");
    assert_eq!(a.crashed_sessions, 1);
    assert_eq!(
        a.users, 5,
        "env_a members: shared, cross_env, a-er-1, a-er-3, session_only"
    );
    assert_eq!(a.new_users, 5, "same 5 — all clear far_past()'s bound");

    let b = sauron_db::repo::overview_totals(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
        far_past(),
    )
    .await
    .unwrap();
    assert_eq!(b.events, 5);
    assert_eq!(
        b.errors, 2,
        "the field that actually differs from env_a's 4"
    );
    assert_eq!(b.sessions, 3);
    assert_eq!(b.crashed_sessions, 1);
    assert_eq!(
        b.users, 3,
        "env_b members: shared, cross_env, distinct_id_env_b_only"
    );
    assert_eq!(b.new_users, 3);

    let none = sauron_db::repo::overview_totals(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::Unattributed),
        far_past(),
    )
    .await
    .unwrap();
    assert_eq!(none.events, 1);
    assert_eq!(none.errors, 1);
    assert_eq!(none.sessions, 1);
    assert_eq!(none.crashed_sessions, 0);
    assert_eq!(
        none.users, 2,
        "unattributed-only members: none-an-0, none-er-0"
    );
    assert_eq!(none.new_users, 2);

    let all = sauron_db::repo::overview_totals(&mut conn, ReadScope::all(ids.app_id), far_past())
        .await
        .unwrap();
    assert_eq!(
        all.events, 11,
        "All must equal the sum of the parts, including unattributed"
    );
    assert_eq!(all.errors, 7);
    assert_eq!(all.sessions, 7);
    assert_eq!(all.crashed_sessions, 2);
    assert_eq!(
        all.users, 8,
        "matches the harness's own event_users row count; NOT 5+3+2=10 — shared/cross_env \
         double-count across environments"
    );
    assert_eq!(all.new_users, 8);

    drop(conn);
    db.cleanup().await;
}

/// `user_stats` mixes tables that carry `environment_id` (analytics_events/error_events for
/// dau/wau/mau, sessions for avg/median_session_ms) with `event_users` (total_users/
/// active_in_range/new_in_range), which does not. The latter three are scoped by membership
/// (see `event_user_membership_exists`'s doc comment in `repo.rs`) — the same gap
/// `overview_totals` closes, hand-derived counts identical to that test's doc comment:
/// `One(env_a)`=5, `One(env_b)`=3, `Unattributed`=2, `All`=8.
///
/// `dau` (distinct_id union of analytics_events ∪ error_events, both env-scoped) is NOT a
/// simple env_a + env_b + unattributed partition: `shared_distinct_id` and
/// `distinct_id_cross_env` are seeded to appear in BOTH environments (see `SeedIds`'s doc
/// comment), so summing the per-environment counts double-counts them. env_a=4, env_b=3,
/// unattributed=2, All=7 (not 9) is the correct, directly-derived shape — asserted as such
/// rather than via a sum-of-parts invariant that would be actively wrong here. This is a
/// *different* 7-vs-9 double-count than `total_users`' 8-vs-10 above: `dau` and
/// `total_users` are governed by different tables (a distinct_id union vs. `event_users` row
/// membership) that merely share the same double-counting shape, not the same numbers.
#[tokio::test]
async fn user_stats_covers_only_the_selected_environment() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    // `Utc::now()` preserves the pre-re-anchoring behaviour these assertions
    // were written against (every seeded row lands within minutes of it).
    // `user_stats_dau_wau_are_anchored_to_the_supplied_now` is the one that
    // pins a fixed instant.
    let a = sauron_db::repo::user_stats(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        far_past(),
        Utc::now(),
    )
    .await
    .unwrap();
    assert_eq!(
        a.dau, 4,
        "distinct analytics/error identities scoped to env_a"
    );
    assert!(
        (a.avg_session_ms - 120000.0).abs() < 1e-9,
        "env_a sessions average 120s: {}",
        a.avg_session_ms
    );
    assert_eq!(
        a.total_users, 5,
        "env_a members: shared, cross_env, a-er-1, a-er-3, session_only"
    );
    assert_eq!(
        a.active_in_range, 5,
        "same 5 — all clear far_past()'s bound"
    );
    assert_eq!(a.new_in_range, 5);
    // Task 15 (F9): `wau`/`mau`/`median_session_ms` each carry their own `{env_sql}`
    // interpolation (repo.rs) independent of `dau`/`avg_session_ms`'s, and none was
    // asserted before this — a regression that widened `wau`'s window and dropped its
    // own `{env_sql}` (or a bind-index slip on `median_session_ms`'s `percentile_cont`
    // sub-select) would have stayed green. Every seeded row lands within minutes of
    // `pinned_now`, so `wau`/`mau` (7-/30-day rolling windows off real `now()`, per
    // the doc comment above — `since` does not bound them) see the identical
    // identities `dau`'s 1-day window does; `median_session_ms` of env_a's three
    // symmetric session durations (60000/120000/180000ms) lands on the same 120000
    // as the mean.
    assert_eq!(a.wau, 4);
    assert_eq!(a.mau, 4);
    assert!(
        (a.median_session_ms - 120000.0).abs() < 1e-9,
        "env_a session-duration median: {}",
        a.median_session_ms
    );

    let b = sauron_db::repo::user_stats(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
        far_past(),
        Utc::now(),
    )
    .await
    .unwrap();
    assert_eq!(b.dau, 3);
    assert!(
        (b.avg_session_ms - 400000.0).abs() < 1e-9,
        "env_b sessions average 400s: {}",
        b.avg_session_ms
    );
    assert_eq!(
        b.total_users, 3,
        "env_b members: shared, cross_env, distinct_id_env_b_only"
    );
    assert_eq!(b.active_in_range, 3);
    assert_eq!(b.new_in_range, 3);
    assert_eq!(b.wau, 3);
    assert_eq!(b.mau, 3);
    assert!(
        (b.median_session_ms - 400000.0).abs() < 1e-9,
        "env_b session-duration median (300000/400000/500000ms, symmetric like env_a): {}",
        b.median_session_ms
    );

    let none = sauron_db::repo::user_stats(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::Unattributed),
        far_past(),
        Utc::now(),
    )
    .await
    .unwrap();
    assert_eq!(none.dau, 2);
    assert_eq!(
        none.total_users, 2,
        "unattributed-only members: none-an-0, none-er-0"
    );
    assert_eq!(none.active_in_range, 2);
    assert_eq!(none.new_in_range, 2);
    assert_eq!(none.wau, 2);
    assert_eq!(none.mau, 2);
    assert!(
        (none.median_session_ms - 200000.0).abs() < 1e-9,
        "the lone unattributed session's own 200000ms duration: {}",
        none.median_session_ms
    );

    let all = sauron_db::repo::user_stats(
        &mut conn,
        ReadScope::all(ids.app_id),
        far_past(),
        Utc::now(),
    )
    .await
    .unwrap();
    assert_eq!(
        all.dau, 7,
        "NOT env_a+env_b+unattributed (9) — shared_distinct_id/distinct_id_cross_env double-count across environments"
    );
    assert_eq!(
        all.total_users, 8,
        "matches the harness's own event_users row count; NOT 5+3+2=10"
    );
    assert_eq!(all.active_in_range, 8);
    assert_eq!(all.new_in_range, 8);
    assert_eq!(
        all.wau, 7,
        "same double-counting shape as dau, not 5+3+2=10"
    );
    assert_eq!(all.mau, 7);

    drop(conn);
    db.cleanup().await;
}

/// `active_user_series`'s `active` component sources the same analytics_events ∪ error_events
/// distinct-id union as `user_stats.dau` (env_a=4, env_b=3, unattributed=2, all seeded rows
/// fall in one day-bucket) — see that test's doc comment for why `All` is 7, not 9. Its
/// `new_users` component reads `event_users` (no `environment_id`), scoped by membership like
/// `overview_totals.new_users`/`user_stats.new_in_range` — same per-scope counts as those two
/// (5/3/2/8), since all three read the identical `event_users` membership + `first_seen`
/// bound.
#[tokio::test]
async fn active_user_series_covers_only_the_selected_environment() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let a = sauron_db::repo::active_user_series(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        far_past(),
    )
    .await
    .unwrap();
    let a_active: i64 = a.iter().map(|p| p.active).sum();
    let a_new: i64 = a.iter().map(|p| p.new_users).sum();
    assert_eq!(
        a_active, 4,
        "distinct analytics/error identities scoped to env_a"
    );
    assert_eq!(
        a_new, 5,
        "env_a members: shared, cross_env, a-er-1, a-er-3, session_only"
    );

    let b = sauron_db::repo::active_user_series(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
        far_past(),
    )
    .await
    .unwrap();
    let b_active: i64 = b.iter().map(|p| p.active).sum();
    let b_new: i64 = b.iter().map(|p| p.new_users).sum();
    assert_eq!(b_active, 3);
    assert_eq!(
        b_new, 3,
        "env_b members: shared, cross_env, distinct_id_env_b_only"
    );

    let none = sauron_db::repo::active_user_series(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::Unattributed),
        far_past(),
    )
    .await
    .unwrap();
    let none_active: i64 = none.iter().map(|p| p.active).sum();
    let none_new: i64 = none.iter().map(|p| p.new_users).sum();
    assert_eq!(none_active, 2);
    assert_eq!(
        none_new, 2,
        "unattributed-only members: none-an-0, none-er-0"
    );

    let all =
        sauron_db::repo::active_user_series(&mut conn, ReadScope::all(ids.app_id), far_past())
            .await
            .unwrap();
    let all_active: i64 = all.iter().map(|p| p.active).sum();
    let all_new: i64 = all.iter().map(|p| p.new_users).sum();
    assert_eq!(
        all_active, 7,
        "NOT env_a+env_b+unattributed (9) — shared identities double-count across environments"
    );
    assert_eq!(
        all_new, 8,
        "matches the harness's own event_users row count; NOT 5+3+2=10 — shared/cross_env \
         double-count across environments, same union shape as overview_totals/user_stats"
    );

    drop(conn);
    db.cleanup().await;
}

/// `session_stats` — `sessions` is seeded 3/3/1, so env_a and env_b tie on BOTH `sessions`
/// and `crashed` (1/1). `avg_session_ms` (120000 vs 400000) is the only field that actually
/// discriminates a swapped filter here, per the brief.
#[tokio::test]
async fn session_stats_covers_only_the_selected_environment() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let a = sauron_db::repo::session_stats(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        far_past(),
    )
    .await
    .unwrap();
    assert_eq!(a.sessions, 3, "sessions is seeded 3/3/1 — ties with env_b");
    assert_eq!(
        a.crashed, 1,
        "ties with env_b too — not discriminating alone"
    );
    assert!(
        (a.avg_session_ms - 120000.0).abs() < 1e-9,
        "the field that actually discriminates: {}",
        a.avg_session_ms
    );
    // Task 15 (F9): `median_session_ms` carries its own `{env_sql}` interpolation,
    // separate from `avg_session_ms`'s, and was never asserted before this — a
    // regression that dropped just this sub-select's predicate would have leaked the
    // app-wide median (200000ms, the middle of all 7 sessions' durations sorted) into
    // a One(env_a) read undetected. env_a's three durations (60000/120000/180000ms)
    // are symmetric, so the median coincides with the mean asserted above.
    assert!(
        (a.median_session_ms - 120000.0).abs() < 1e-9,
        "env_a session-duration median, not the app-wide 200000: {}",
        a.median_session_ms
    );

    let b = sauron_db::repo::session_stats(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
        far_past(),
    )
    .await
    .unwrap();
    assert_eq!(b.sessions, 3);
    assert_eq!(b.crashed, 1);
    assert!(
        (b.avg_session_ms - 400000.0).abs() < 1e-9,
        "{}",
        b.avg_session_ms
    );
    assert!(
        (b.median_session_ms - 400000.0).abs() < 1e-9,
        "env_b session-duration median (300000/400000/500000ms, symmetric like env_a): {}",
        b.median_session_ms
    );

    let none = sauron_db::repo::session_stats(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::Unattributed),
        far_past(),
    )
    .await
    .unwrap();
    assert_eq!(none.sessions, 1);
    assert_eq!(none.crashed, 0);
    assert!(
        (none.median_session_ms - 200000.0).abs() < 1e-9,
        "the lone unattributed session's own 200000ms duration: {}",
        none.median_session_ms
    );

    let all = sauron_db::repo::session_stats(&mut conn, ReadScope::all(ids.app_id), far_past())
        .await
        .unwrap();
    assert_eq!(
        all.sessions, 7,
        "All must equal the sum of the parts, including unattributed"
    );
    assert_eq!(all.crashed, 2);

    drop(conn);
    db.cleanup().await;
}

/// `bump_session` folds every signal into one row per `(app_id, session_id)` and
/// sets `environment_id = COALESCE(EXCLUDED.environment_id, sessions.environment_id)`
/// — the most recent non-null value wins — while `errors_count` accumulates across
/// every environment that ever touched this session id. So a session labelled
/// `env_a` can carry an `errors_count` incremented by an `env_b` error, and
/// `crashed`/`crashed_sessions` count it under the label's environment even though
/// that environment never saw the error. See `bump_session`'s own doc comment and
/// `common::CrossEnvSessionIds`'s doc comment for the exact shape.
///
/// MEASURED AND DECLINED — see Task 10's report
/// (`.superpowers/sdd/2026-07-29-environment-rbac-scope/task-10-report.md`) for the
/// full `EXPLAIN (ANALYZE, BUFFERS)` numbers. The `EXISTS` semi-join this test
/// requires (deriving `crashed` from `error_events` instead of the accumulated
/// `errors_count` column) cost roughly 11x the column predicate's total planning +
/// execution time on the largest dev app's session table (~1000 sessions), even
/// with a purpose-built `error_events (app_id, session_id, environment_id)` index
/// — the index barely moved the number, because the cost is structural: a
/// correlated per-session subquery re-probed against all 22 `error_events`
/// partitions (partition pruning cannot help, since neither `session_id` nor
/// `environment_id` is the partition key). Left `#[ignore]`d rather than deleted so
/// the documented gap stays visible and re-runnable if a future task revisits this
/// with a cheaper derivation.
#[tokio::test]
#[ignore = "measured+declined: EXISTS semi-join costs ~11x the column predicate on \
            the dev app even with a dedicated index (structural, not missing-index \
            cost) — see task-10-report.md. Fix not shipped; bump_session's doc \
            comment documents the read-side consequence instead."]
async fn crashed_sessions_are_counted_only_in_the_environment_that_crashed() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_cross_env_session().await;
    let mut conn = db.conn().await;
    let since = ids.pinned_now - Duration::days(30);

    // The fixture itself, before trusting any derived count: the row's own
    // label is env_a, its errors_count is 1, and it is unreachable under
    // env_b's own scope filter even though env_b is where the error happened
    // — the exact shape `CrossEnvSessionIds`'s doc comment describes.
    let labelled_a = sauron_db::repo::get_session(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        &ids.session_id,
    )
    .await
    .unwrap()
    .expect("session must be visible under env_a, its own label");
    assert_eq!(labelled_a.environment_id, Some(ids.env_a));
    assert_eq!(labelled_a.errors_count, 1);
    assert!(
        sauron_db::repo::get_session(
            &mut conn,
            ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
            &ids.session_id,
        )
        .await
        .unwrap()
        .is_none(),
        "the session's own label is env_a, not env_b, despite env_b owning the error"
    );

    let a = sauron_db::repo::session_stats(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        since,
    )
    .await
    .unwrap();
    let b = sauron_db::repo::session_stats(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
        since,
    )
    .await
    .unwrap();

    // The shared session errored ONLY in env_b.
    assert_eq!(b.crashed, 1, "env_b saw the error and must count the crash");
    assert_eq!(
        a.crashed, 0,
        "env_a never saw an error on this session and must not count it as crashed"
    );

    // Same for the overview card, which reads a different query.
    let ov_a = sauron_db::repo::overview_totals(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        since,
    )
    .await
    .unwrap();
    assert_eq!(ov_a.crashed_sessions, 0);

    drop(conn);
    db.cleanup().await;
}

/// `session_duration_series` — env_a's 60/120/180s sessions average 120000ms, env_b's
/// 300/400/500s average 400000ms; all fall in the same day-bucket, so the single bucket's
/// `avg_ms` is the discriminator (sessions is 3/3/1, tied on count).
#[tokio::test]
async fn session_duration_series_covers_only_the_selected_environment() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let a = sauron_db::repo::session_duration_series(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        far_past(),
    )
    .await
    .unwrap();
    assert_eq!(a.len(), 1, "all seeded sessions fall in one day-bucket");
    assert!((a[0].avg_ms - 120000.0).abs() < 1e-9, "{}", a[0].avg_ms);

    let b = sauron_db::repo::session_duration_series(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
        far_past(),
    )
    .await
    .unwrap();
    assert_eq!(b.len(), 1);
    assert!((b[0].avg_ms - 400000.0).abs() < 1e-9, "{}", b[0].avg_ms);

    let none = sauron_db::repo::session_duration_series(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::Unattributed),
        far_past(),
    )
    .await
    .unwrap();
    assert_eq!(none.len(), 1);
    assert!(
        (none[0].avg_ms - 200000.0).abs() < 1e-9,
        "the unattributed session is 200s: {}",
        none[0].avg_ms
    );

    let all =
        sauron_db::repo::session_duration_series(&mut conn, ReadScope::all(ids.app_id), far_past())
            .await
            .unwrap();
    assert_eq!(all.len(), 1, "still one bucket under All");

    drop(conn);
    db.cleanup().await;
}

/// `session_duration_histogram` — env_a's sessions (60/120/180s) all land in the `1-5m` bin,
/// env_b's (300/400/500s) in `5-30m`. A swapped filter would move rows to the wrong bin
/// LABEL, not just change a number in the same bin — the strongest possible signal here.
#[tokio::test]
async fn session_duration_histogram_covers_only_the_selected_environment() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    fn bucket_count(rows: &[sauron_db::repo::HistoBucket], label: &str) -> i64 {
        rows.iter()
            .find(|r| r.bucket == label)
            .map(|r| r.count)
            .unwrap_or(0)
    }

    let a = sauron_db::repo::session_duration_histogram(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        far_past(),
    )
    .await
    .unwrap();
    assert_eq!(
        bucket_count(&a, "1-5m"),
        3,
        "env_a's 60/120/180s sessions all land here"
    );
    assert_eq!(bucket_count(&a, "5-30m"), 0, "must NOT see env_b's bin");

    let b = sauron_db::repo::session_duration_histogram(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
        far_past(),
    )
    .await
    .unwrap();
    assert_eq!(
        bucket_count(&b, "5-30m"),
        3,
        "env_b's 300/400/500s sessions all land here"
    );
    assert_eq!(bucket_count(&b, "1-5m"), 0, "must NOT see env_a's bin");

    let none = sauron_db::repo::session_duration_histogram(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::Unattributed),
        far_past(),
    )
    .await
    .unwrap();
    assert_eq!(
        bucket_count(&none, "1-5m"),
        1,
        "the unattributed session is 200s"
    );

    let all = sauron_db::repo::session_duration_histogram(
        &mut conn,
        ReadScope::all(ids.app_id),
        far_past(),
    )
    .await
    .unwrap();
    assert_eq!(
        bucket_count(&all, "1-5m"),
        4,
        "All must equal the sum of the parts, including unattributed: env_a's 3 + unattributed's 1"
    );
    assert_eq!(bucket_count(&all, "5-30m"), 3, "env_b's 3");

    drop(conn);
    db.cleanup().await;
}

/// `funnel` is the sharpest test of the "env filter on every CTE, not just s0" risk the task
/// brief calls out. `distinct_id_cross_env` does step1 (`harness.funnel.step1`) in env_a and
/// step2 (`harness.funnel.step2`) in env_b, never both in the same environment (see
/// `SeedIds`'s doc comment) — specifically so that a funnel which scopes `s0` correctly but
/// forgets the env filter on `s1` disagrees with a correctly-scoped one: under `One(env_a)`,
/// `distinct_id_cross_env` IS a step-0 candidate (its step1 is in env_a) but must NOT clear
/// step1 (its step2 is in env_b only). A broken `s1` filter would let it through anyway,
/// making `step1`'s count 2 instead of the correct 1.
#[tokio::test]
async fn funnel_counts_only_the_selected_environment() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let steps = vec![
        "harness.funnel.step1".to_string(),
        "harness.funnel.step2".to_string(),
    ];

    let a = sauron_db::repo::funnel(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        &steps,
        far_past(),
    )
    .await
    .unwrap();
    assert_eq!(
        a[0].count, 2,
        "shared_distinct_id + distinct_id_cross_env both did step1 in env_a"
    );
    assert_eq!(
        a[1].count, 1,
        "only shared_distinct_id completes step2 in env_a — distinct_id_cross_env's step2 is env_b-only; \
         a filter missing on s1 would leak it through and give 2"
    );

    let b = sauron_db::repo::funnel(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
        &steps,
        far_past(),
    )
    .await
    .unwrap();
    assert_eq!(
        b[0].count, 1,
        "only distinct_id_env_b_only did step1 in env_b"
    );
    assert_eq!(b[1].count, 1);

    let none = sauron_db::repo::funnel(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::Unattributed),
        &steps,
        far_past(),
    )
    .await
    .unwrap();
    assert_eq!(
        none[0].count, 0,
        "no unattributed row uses the funnel step names"
    );
    assert_eq!(none[1].count, 0);

    let all = sauron_db::repo::funnel(&mut conn, ReadScope::all(ids.app_id), &steps, far_past())
        .await
        .unwrap();
    assert_eq!(
        all[0].count, 3,
        "all three identities did step1 somewhere: shared, cross_env, b_only"
    );
    assert_eq!(
        all[1].count, 3,
        "under All, distinct_id_cross_env's step2 (env_b) counts against its own step1 (env_a) too"
    );

    drop(conn);
    db.cleanup().await;
}

/// The legacy `filter=environment:eq:<name>` chip (kept for API back-compat,
/// see `EVENT_FILTERS`) must compose with `ReadScope` rather than replace it:
/// the topbar scope is the outer boundary, the chip narrows within it.
#[tokio::test]
async fn list_analytics_events_legacy_chip_composes_with_scope() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    // The seed names its environments literally "env_a"/"env_b" (see
    // `TestDb::seed_two_envs`'s `repo::create_environment` calls), so the chip
    // can name one directly without a lookup helper.
    let chip_env_b = vec![sauron_db::filter::ParsedFilter {
        field: "environment",
        op: sauron_db::filter::Op::Eq,
        value: "env_b".to_string(),
    }];

    // All-scope + chip=env_b must narrow down to exactly env_b's rows — the
    // chip does real work on top of a scope that otherwise covers everything.
    let narrowed = sauron_db::repo::list_analytics_events(
        &mut conn,
        ReadScope::all(ids.app_id),
        &chip_env_b,
        None,
        Some(far_past()),
        100,
        0,
    )
    .await
    .unwrap();
    assert_eq!(
        narrowed.len(),
        3,
        "All + chip=env_b narrows to env_b's 3 non-'$screen' rows"
    );
    assert!(narrowed.iter().all(|e| e.environment_id == Some(ids.env_b)));

    // scope=One(env_a) + chip=env_b must compose as AND (0 rows), not have the
    // chip silently override or bypass the outer scope.
    let conflicting = sauron_db::repo::list_analytics_events(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        &chip_env_b,
        None,
        Some(far_past()),
        100,
        0,
    )
    .await
    .unwrap();
    assert_eq!(
        conflicting.len(),
        0,
        "scope=env_a AND chip=env_b must yield nothing — the chip cannot escape the outer scope"
    );

    drop(conn);
    db.cleanup().await;
}

/// The onboarding UI builds its DSN from one specific environment, then polls this app-wide
/// existence check — so an unscoped "has ANY environment sent anything" answer can report
/// success purely from a *different* environment's traffic. Concretely: an app with existing
/// `env_a` traffic gets a fresh `env_c` added; onboarding shows `env_c`'s DSN and must NOT
/// report "received" until `env_c` itself has events, even though the app overall (and `env_a`)
/// already does — the false-positive this review found in `dashboard/src/pages/Onboarding.svelte`.
#[tokio::test]
async fn app_has_events_reports_only_the_selected_environment() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let env_c = common::seed_env(
        &mut conn,
        ids.project_id,
        ids.app_id,
        "env_c",
        &format!("pk_test_c_{}", Uuid::new_v4().simple()),
        false,
    )
    .await;

    let brand_new = sauron_db::repo::app_has_events(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(env_c)),
    )
    .await
    .unwrap();
    assert_eq!(
        brand_new,
        (false, false),
        "env_c is brand new and has received nothing — must not report true just because \
         env_a/env_b have traffic"
    );

    let a = sauron_db::repo::app_has_events(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
    )
    .await
    .unwrap();
    assert_eq!(
        a,
        (true, true),
        "env_a was seeded with both error and analytics events"
    );

    let none = sauron_db::repo::app_has_events(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::Unattributed),
    )
    .await
    .unwrap();
    assert_eq!(none, (true, true), "the unattributed bucket has 1 of each");

    let all = sauron_db::repo::app_has_events(&mut conn, ReadScope::all(ids.app_id))
        .await
        .unwrap();
    assert_eq!(all, (true, true), "All must still see the app-wide traffic");

    drop(conn);
    db.cleanup().await;
}

/// `event_series`'s `Some(name)` arm builds a distinct SQL string with the env fragment at a
/// different bind index ($4, not $3 — see the function's own comments). The pre-existing test
/// above only exercises it under `One`; both other variants went through the `None` arm. This
/// closes that gap so the named arm's `All`/`Unattributed` bind layout is covered too.
#[tokio::test]
async fn event_series_named_arm_covers_all_and_unattributed() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    // "harness.event" is seeded once in env_a and once unattributed (see `seed_two_envs`) —
    // never in env_b — so `All` must be 2 (the sum of the parts) and `Unattributed` must be 1.
    let all = sauron_db::repo::event_series(
        &mut conn,
        ReadScope::all(ids.app_id),
        Some("harness.event"),
        far_past(),
    )
    .await
    .unwrap();
    let all_total: i64 = all.iter().map(|p| p.count).sum();
    assert_eq!(
        all_total, 2,
        "'harness.event' is seeded once in env_a and once unattributed"
    );

    let unattributed = sauron_db::repo::event_series(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::Unattributed),
        Some("harness.event"),
        far_past(),
    )
    .await
    .unwrap();
    let unattributed_total: i64 = unattributed.iter().map(|p| p.count).sum();
    assert_eq!(
        unattributed_total, 1,
        "the unattributed 'harness.event' row must still be found through the $4 bind layout"
    );

    drop(conn);
    db.cleanup().await;
}

/// Task 7: there is no `screens` table — `screen_ctes` derives every column from
/// `analytics_events`/`error_events`, both of which carry `environment_id`, across **four**
/// CTEs (`ev`, `ex`, `us`'s two `UNION ALL` arms, and `dw`'s window subquery). The env
/// fragment must reach all four, or one column (or one arm of `us`) silently mixes
/// environments while the rest of the row still looks plausible — which is why every field
/// below is asserted independently, never just presence or a total.
///
/// `home` is the discriminating screen: seeded with a `'$screen'`/dwell pair in **both**
/// environments with different views/dwell counts (env_a: 1 view, 60s dwell; env_b: 2 views,
/// 90s dwell — see `SeedIds`'s doc comment on `seed_two_envs`), specifically so a `dw` CTE
/// that omits its environment fragment pools dwell across environments (wrong nonzero number)
/// instead of merely going to zero (which would be an obvious, not silent, failure).
#[tokio::test]
async fn screen_stats_covers_only_the_selected_environment() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let a = sauron_db::repo::screen_stats(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        far_past(),
        "home",
    )
    .await
    .unwrap();
    assert_eq!(a.views, 1, "env_a's own '$screen' row on home");
    assert_eq!(
        a.events, 3,
        "harness.event + env_a's own funnel step1 + the cross-env identity's step1"
    );
    assert_eq!(
        a.exceptions, 2,
        "shared_distinct_id's + distinct_id_cross_env's error, both on home in env_a"
    );
    assert_eq!(
        a.users, 2,
        "shared_distinct_id + distinct_id_cross_env, from analytics ∪ error"
    );
    assert!(
        (a.total_dwell_ms - 60000.0).abs() < 1e-9,
        "env_a's paired '$screen' row: 60s gap; {}",
        a.total_dwell_ms
    );
    assert!(
        (a.avg_dwell_ms - 60000.0).abs() < 1e-9,
        "60000 / 1 view; {}",
        a.avg_dwell_ms
    );

    let b = sauron_db::repo::screen_stats(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
        far_past(),
        "home",
    )
    .await
    .unwrap();
    assert_eq!(b.views, 2, "env_b's two '$screen' rows on home");
    assert_eq!(
        b.events, 1,
        "funnel step1 only — env_b's '$screen' rows don't count as events"
    );
    assert_eq!(
        b.exceptions, 1,
        "shared_distinct_id's env_b error, on home; must NOT see env_a's 2"
    );
    assert_eq!(
        b.users, 2,
        "distinct_id_env_b_only (analytics) + shared_distinct_id (its env_b error is on home too)"
    );
    assert!(
        (b.total_dwell_ms - 90000.0).abs() < 1e-9,
        "env_b's paired '$screen' row: 90s gap; must NOT see env_a's 60s or pool with it; {}",
        b.total_dwell_ms
    );
    assert!(
        (b.avg_dwell_ms - 45000.0).abs() < 1e-9,
        "90000 / 2 views; {}",
        b.avg_dwell_ms
    );

    let none = sauron_db::repo::screen_stats(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::Unattributed),
        far_past(),
        "home",
    )
    .await
    .unwrap();
    assert_eq!(
        none.views, 0,
        "the unattributed home row is 'harness.event', not '$screen'"
    );
    assert_eq!(none.events, 1);
    assert_eq!(none.exceptions, 1);
    assert_eq!(
        none.users, 2,
        "the unattributed analytics identity + the unattributed error identity"
    );
    assert_eq!(
        none.total_dwell_ms, 0.0,
        "the unattributed analytics row has no session_id, so no dwell partner"
    );

    let all =
        sauron_db::repo::screen_stats(&mut conn, ReadScope::all(ids.app_id), far_past(), "home")
            .await
            .unwrap();
    assert_eq!(all.views, 3, "All is the sum of the parts: 1 + 2 + 0");
    assert_eq!(all.events, 5, "3 + 1 + 1");
    assert_eq!(all.exceptions, 4, "2 + 1 + 1");
    // NOT 6 (2+2+2): shared_distinct_id has a home error row in BOTH env_a and
    // env_b, so it is counted once in each per-scope "2" — the per-scope sum
    // double-counts it. `All`'s own query has no environment predicate at
    // all, so it computes the true distinct count directly; there is nothing
    // to double-count. Same "shared cross-environment identity" math Task 6
    // found for `user_stats.dau`/`active_user_series.active` — asserted
    // directly here rather than as a (here-incorrect) sum-of-parts.
    assert_eq!(
        all.users, 5,
        "the true distinct count across environments, not the per-scope sum (6)"
    );
    assert!(
        (all.total_dwell_ms - 150000.0).abs() < 1e-9,
        "All must equal the sum of the parts: env_a's 60000 + env_b's 90000; {}",
        all.total_dwell_ms
    );
    assert!(
        (all.avg_dwell_ms - 50000.0).abs() < 1e-9,
        "150000 / 3 views; {}",
        all.avg_dwell_ms
    );

    drop(conn);
    db.cleanup().await;
}

/// `screen_list` shares `screen_ctes` with `screen_stats` but has its own bind sequence
/// ($3 is a LIKE pattern, not an exact name, and $4/$5 are limit/offset that shift to $5/$6
/// when the env fragment consumes $4). This test exercises that shift directly: `home` sorts
/// first under every scope (higher `views`), so `limit=1/offset=0` and `limit=1/offset=1`
/// must return `home` then `checkout` — if limit and offset were ever swapped (both are
/// `BigInt`, so a swap would NOT be a type error, unlike a Uuid/BigInt mix-up), this would
/// come back in the wrong order silently rather than fail loudly.
#[tokio::test]
async fn screen_list_covers_only_the_selected_environment() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    fn find<'a>(
        rows: &'a [sauron_db::repo::ScreenRow],
        screen: &str,
    ) -> &'a sauron_db::repo::ScreenRow {
        rows.iter()
            .find(|r| r.screen == screen)
            .unwrap_or_else(|| panic!("no '{screen}' row in {rows:?}"))
    }

    let a = sauron_db::repo::screen_list(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        far_past(),
        "%",
        50,
        0,
        common::default_screen_sort(),
    )
    .await
    .unwrap();
    assert_eq!(a.len(), 2, "home + checkout, both present in env_a");
    let a_home = find(&a, "home");
    assert_eq!(a_home.views, 1);
    assert_eq!(a_home.events, 3);
    assert_eq!(a_home.exceptions, 2);
    assert_eq!(a_home.users, 2);
    assert!(
        (a_home.avg_dwell_ms - 60000.0).abs() < 1e-9,
        "{}",
        a_home.avg_dwell_ms
    );
    let a_checkout = find(&a, "checkout");
    assert_eq!(
        a_checkout.views, 0,
        "no '$screen' row is ever named checkout"
    );
    assert_eq!(
        a_checkout.events, 1,
        "env_a's own funnel step2 only — the cross-env identity's step2 is in env_b, not env_a \
         (see SeedIds' doc comment: it does step1 in env_a, step2 in env_b, never both in one)"
    );
    assert_eq!(a_checkout.exceptions, 2);
    assert_eq!(
        a_checkout.users, 3,
        "shared_distinct_id (step2) + the two error-only identities (a-er-1, a-er-3)"
    );
    assert_eq!(a_checkout.avg_dwell_ms, 0.0, "views=0 guards the division");

    let b = sauron_db::repo::screen_list(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
        far_past(),
        "%",
        50,
        0,
        common::default_screen_sort(),
    )
    .await
    .unwrap();
    assert_eq!(b.len(), 2, "home + checkout, both present in env_b");
    let b_home = find(&b, "home");
    assert_eq!(b_home.views, 2);
    assert_eq!(b_home.events, 1);
    assert_eq!(b_home.exceptions, 1);
    assert_eq!(b_home.users, 2);
    assert!(
        (b_home.avg_dwell_ms - 45000.0).abs() < 1e-9,
        "{}",
        b_home.avg_dwell_ms
    );
    let b_checkout = find(&b, "checkout");
    assert_eq!(b_checkout.views, 0);
    assert_eq!(
        b_checkout.events, 2,
        "env_b's own funnel step2 + the cross-env identity's step2 (its step1 was env_a's, on home)"
    );
    assert_eq!(b_checkout.exceptions, 1);
    assert_eq!(
        b_checkout.users, 2,
        "distinct_id_env_b_only + distinct_id_cross_env"
    );

    let none = sauron_db::repo::screen_list(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::Unattributed),
        far_past(),
        "%",
        50,
        0,
        common::default_screen_sort(),
    )
    .await
    .unwrap();
    assert_eq!(
        none.len(),
        1,
        "checkout has no unattributed rows at all, so it must not appear as a key"
    );
    let none_home = find(&none, "home");
    assert_eq!(none_home.views, 0);
    assert_eq!(none_home.events, 1);
    assert_eq!(none_home.exceptions, 1);
    assert_eq!(none_home.users, 2);

    let all = sauron_db::repo::screen_list(
        &mut conn,
        ReadScope::all(ids.app_id),
        far_past(),
        "%",
        50,
        0,
        common::default_screen_sort(),
    )
    .await
    .unwrap();
    assert_eq!(all.len(), 2);
    let all_home = find(&all, "home");
    assert_eq!(all_home.views, 3);
    assert_eq!(all_home.events, 5);
    assert_eq!(all_home.exceptions, 4);
    assert_eq!(
        all_home.users, 5,
        "true distinct count, not the per-scope sum — see screen_stats' test"
    );
    assert!(
        (all_home.avg_dwell_ms - 50000.0).abs() < 1e-9,
        "{}",
        all_home.avg_dwell_ms
    );
    let all_checkout = find(&all, "checkout");
    assert_eq!(all_checkout.views, 0);
    assert_eq!(all_checkout.events, 3);
    assert_eq!(all_checkout.exceptions, 3);
    assert_eq!(all_checkout.users, 5);

    // Bind-index-shift proof: under `One(env_a)` the env fragment consumes $4, pushing
    // limit/offset to $5/$6. `home` (views=1) must sort before `checkout` (views=0).
    let page1 = sauron_db::repo::screen_list(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        far_past(),
        "%",
        1,
        0,
        common::default_screen_sort(),
    )
    .await
    .unwrap();
    assert_eq!(page1.len(), 1);
    assert_eq!(
        page1[0].screen, "home",
        "limit=1 offset=0 must return the higher-views row"
    );

    let page2 = sauron_db::repo::screen_list(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        far_past(),
        "%",
        1,
        1,
        common::default_screen_sort(),
    )
    .await
    .unwrap();
    assert_eq!(page2.len(), 1);
    assert_eq!(
        page2[0].screen, "checkout",
        "limit=1 offset=1 must skip 'home', not return it again — proves limit/offset \
         landed on $5/$6, not swapped or double-bound"
    );

    drop(conn);
    db.cleanup().await;
}

/// Distinct `device_key`s with signal on `screen`, computed independently of
/// `screen_signal_union`.
///
/// Deliberately NOT built from the helper it checks: an oracle that reuses the
/// implementation's own query reproduces its bugs and asserts nothing. This is
/// hand-written SQL over the same two tables, with the environment filter
/// spelled out separately.
async fn distinct_device_count_for_screen(
    conn: &mut diesel_async::AsyncPgConnection,
    scope: &ReadScope,
    screen: &str,
) -> i64 {
    use diesel::sql_types::BigInt;
    use diesel_async::RunQueryDsl;

    #[derive(diesel::QueryableByName)]
    struct N {
        #[diesel(sql_type = BigInt)]
        n: i64,
    }

    let env = match &scope.env {
        EnvFilter::All => String::new(),
        EnvFilter::One(id) => format!(" AND environment_id = '{id}'"),
        EnvFilter::Unattributed => " AND environment_id IS NULL".to_string(),
        EnvFilter::Subset(ids) => {
            let list = ids
                .iter()
                .map(|i| format!("'{i}'"))
                .collect::<Vec<_>>()
                .join(",");
            format!(" AND environment_id IN ({list})")
        }
    };
    let sql = format!(
        "SELECT count(*)::bigint AS n FROM ( \
           SELECT device_key FROM analytics_events \
            WHERE app_id='{app}' AND screen='{screen}' \
              AND device_key IS NOT NULL AND device_key<>''{env} \
           UNION \
           SELECT device_key FROM error_events \
            WHERE app_id='{app}' AND screen='{screen}' \
              AND device_key IS NOT NULL AND device_key<>''{env} \
         ) d",
        app = scope.app_id,
    );
    let row: N = diesel::sql_query(sql).get_result(conn).await.unwrap();
    row.n
}

/// `users_for_screen`/`devices_for_screen` are RAW SQL — the shape where a
/// mistake costs a 500 at runtime and nothing earlier. Three things are checked
/// that only a real database can answer:
///
/// 1. **The SQL parses and its types line up at all.** `UNION ALL` across
///    `analytics_events` and `error_events`, a `NULL::text` stand-in for the
///    column `error_events` does not have, `FILTER (WHERE …)` aggregates, and a
///    `LEFT JOIN` per query — none of it is checked by `cargo check`.
/// 2. **The env bind landed on `$4` and did not shift `LIMIT`/`OFFSET`.** The
///    fragment is interpolated into BOTH union branches referencing the same
///    placeholder, which is the specific thing a bind-index slip breaks.
/// 3. **The row count agrees with `screen_stats.users`.** Both derive the
///    screen's audience from the same union, so a divergence means one of them
///    is answering a different question than the tile beside it claims.
#[tokio::test]
async fn users_and_devices_for_screen_are_scoped_and_agree_with_screen_stats() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    for (label, env) in [
        ("env_a", EnvFilter::One(ids.env_a)),
        ("env_b", EnvFilter::One(ids.env_b)),
        ("all", EnvFilter::All),
    ] {
        let scope = ReadScope::new(ids.app_id, env);

        let stats = sauron_db::repo::screen_stats(&mut conn, scope.clone(), far_past(), "home")
            .await
            .unwrap();

        // A limit far above the fixture, so this is every row, not a page.
        let users =
            sauron_db::repo::users_for_screen(&mut conn, scope.clone(), "home", far_past(), 100, 0)
                .await
                .unwrap();

        assert_eq!(
            users.len() as i64,
            stats.users,
            "{label}: users_for_screen must return exactly the identities \
             screen_stats counts for the same screen and scope"
        );

        // Every row belongs to the screen: its per-screen signal cannot be zero
        // on all three counters, or the union admitted a row it should not have.
        for u in &users {
            assert!(
                u.views_on_screen + u.events_on_screen + u.exceptions_on_screen > 0,
                "{label}: {} has no signal on 'home' yet was listed",
                u.distinct_id
            );
            assert!(
                u.first_seen_on_screen <= u.last_seen_on_screen,
                "{label}: {} has first_seen after last_seen",
                u.distinct_id
            );
        }

        let devices = sauron_db::repo::devices_for_screen(
            &mut conn,
            scope.clone(),
            "home",
            far_past(),
            100,
            0,
        )
        .await
        .unwrap();

        // The devices list must be env-scoped INDEPENDENTLY of the users list.
        // Without this, the only devices assertion was the per-row
        // `signal > 0` loop below, which a leak cannot fail: drop `env_sql`
        // from the device branch of `screen_signal_union` and every leaked row
        // still carries counters > 0, because the same unfiltered union feeds
        // the aggregate. The suite printed `ok` on that mutation.
        //
        // Derived from the same union `screen_stats` counts users over, rather
        // than hardcoded, so it states the invariant instead of a fixture
        // constant: a screen's device set is bounded by its identity set, and
        // under a narrower environment filter it can only shrink.
        let expected_devices = distinct_device_count_for_screen(&mut conn, &scope, "home").await;
        assert_eq!(
            devices.len() as i64,
            expected_devices,
            "{label}: devices_for_screen must list exactly the distinct device_keys              with signal on this screen in this scope"
        );

        for d in &devices {
            assert!(
                d.views_on_screen + d.events_on_screen + d.exceptions_on_screen > 0,
                "{label}: device {} has no signal on 'home' yet was listed",
                d.device_key
            );
        }

        // Paging must partition the list, not resample it. This is the check
        // that fails when the ORDER BY has no unique tiebreak: each page looks
        // correct alone while a row appears twice across the boundary.
        if users.len() >= 2 {
            let page1 = sauron_db::repo::users_for_screen(
                &mut conn,
                scope.clone(),
                "home",
                far_past(),
                1,
                0,
            )
            .await
            .unwrap();
            let page2 = sauron_db::repo::users_for_screen(
                &mut conn,
                scope.clone(),
                "home",
                far_past(),
                1,
                1,
            )
            .await
            .unwrap();
            assert_eq!(page1.len(), 1, "{label}: limit=1 must return one row");
            assert_eq!(
                page2.len(),
                1,
                "{label}: offset=1 must return the second row"
            );
            assert_ne!(
                page1[0].distinct_id, page2[0].distinct_id,
                "{label}: offset=1 returned the same row as offset=0 — LIMIT/OFFSET \
                 bound to the wrong placeholders, or the ORDER BY has no unique tiebreak"
            );
            assert_eq!(
                page1[0].distinct_id, users[0].distinct_id,
                "{label}: the first page must match the head of the unpaged list"
            );
        }
    }

    // Guard against this test passing vacuously. Every assertion above is
    // inside a loop over scopes, and several are inside `if users.len() >= 2`;
    // a fixture that stopped seeding `home` would satisfy all of them with
    // empty vectors and still print `ok`. Pinned to the same number
    // `screen_stats_covers_only_the_selected_environment` asserts for env_a,
    // so if the fixture changes, both fail together and say so.
    let env_a_users = sauron_db::repo::users_for_screen(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        "home",
        far_past(),
        100,
        0,
    )
    .await
    .unwrap();
    assert_eq!(
        env_a_users.len(),
        2,
        "the fixture must seed 2 users on 'home' in env_a, or the loop above          asserted nothing — matches screen_stats' `a.users == 2`"
    );

    // A screen nobody visited returns nothing rather than erroring — the empty
    // card, not a 500.
    let none = sauron_db::repo::users_for_screen(
        &mut conn,
        ReadScope::all(ids.app_id),
        "no-such-screen",
        far_past(),
        100,
        0,
    )
    .await
    .unwrap();
    assert!(none.is_empty(), "an unvisited screen must list no users");

    drop(conn);
    db.cleanup().await;
}

/// `recent_events_for_screen`/`recent_exceptions_for_screen` are boxed-diesel reads (not raw
/// SQL, unlike `screen_ctes`' four callers above), scoped via the ordinary `scope_env!` macro.
/// Counts must match `screen_stats`' `events`/`exceptions` columns for the same screen+scope.
#[tokio::test]
async fn recent_events_and_exceptions_for_screen_are_scoped() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let a_events = sauron_db::repo::recent_events_for_screen(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        "home",
        far_past(),
        20,
        0,
    )
    .await
    .unwrap();
    assert_eq!(
        a_events.len(),
        3,
        "matches screen_stats' env_a events=3 for home"
    );

    let b_events = sauron_db::repo::recent_events_for_screen(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
        "home",
        far_past(),
        20,
        0,
    )
    .await
    .unwrap();
    assert_eq!(
        b_events.len(),
        1,
        "matches screen_stats' env_b events=1 for home"
    );

    let a_exceptions = sauron_db::repo::recent_exceptions_for_screen(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        "home",
        far_past(),
        20,
        0,
    )
    .await
    .unwrap();
    assert_eq!(
        a_exceptions.len(),
        2,
        "matches screen_stats' env_a exceptions=2 for home"
    );

    let b_exceptions = sauron_db::repo::recent_exceptions_for_screen(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
        "home",
        far_past(),
        20,
        0,
    )
    .await
    .unwrap();
    assert_eq!(
        b_exceptions.len(),
        1,
        "matches screen_stats' env_b exceptions=1 for home"
    );

    let all_events = sauron_db::repo::recent_events_for_screen(
        &mut conn,
        ReadScope::all(ids.app_id),
        "home",
        far_past(),
        20,
        0,
    )
    .await
    .unwrap();
    assert_eq!(all_events.len(), 5, "3 + 1 + 1 (unattributed)");

    drop(conn);
    db.cleanup().await;
}

// ===========================================================================
// Task 8: Persons and devices — the LATERAL reads
// ===========================================================================

/// `list_persons`' counts come from three LATERAL subqueries over
/// `analytics_events`/`error_events`/`sessions` (all of which carry
/// `environment_id`); its outer page comes from `event_users`, which does
/// not, so membership in a specific environment is derived via an `EXISTS`
/// over the same three tables.
///
/// `shared_distinct_id` has activity in both environments, asymmetrically:
/// env_a has all 4 of its analytics_events rows, env_b has none — only 1
/// error and 1 session. That zero-analytics-events case is exactly what a
/// membership filter naively written as "has an analytics event in this
/// environment" would get wrong (it would drop this row under `One(env_b)`
/// even though the person is genuinely active there).
///
/// `distinct_id_env_b_only` has activity in env_b alone and must not appear
/// at all under `One(env_a)` — not "appear with all-zero counts", which is
/// what a `LEFT JOIN LATERAL` alone (with no membership filter) would do.
///
/// `session_only_distinct_id` (see `SeedIds`' doc comment) has **zero**
/// `analytics_events`/`error_events` rows anywhere — its only row in any
/// signal table is one `sessions` row in env_a. It is what proves the third,
/// `sessions`-only leg of the membership `EXISTS` actually matters: delete
/// that leg and this identity has nothing left to qualify on in env_a, so it
/// silently disappears from `One(env_a)` instead of appearing with
/// `events_count: 0, errors_count: 0, sessions_count: 1`. Verified live
/// during review — see `.superpowers/sdd/s2-task-8-report.md`'s "Review
/// findings applied" section for the delete/fail/restore/pass proof.
#[tokio::test]
async fn list_persons_covers_only_the_selected_environment() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let rows_a = sauron_db::repo::list_persons(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        None,
        50,
        0,
        common::default_person_sort(),
        TimeWindow::since(
            "last_seen",
            chrono::Utc::now() - chrono::Duration::days(3650),
        ),
    )
    .await
    .unwrap();
    assert_eq!(
        rows_a.len(),
        5,
        "env_a: shared_distinct_id, the cross-env funnel identity, two error-only identities, \
         and session_only_distinct_id (sessions-leg-only membership)"
    );
    assert!(
        rows_a
            .iter()
            .all(|r| r.distinct_id != ids.distinct_id_env_b_only),
        "distinct_id_env_b_only has zero activity in env_a and must not appear at all"
    );
    let shared_a = rows_a
        .iter()
        .find(|r| r.distinct_id == ids.shared_distinct_id)
        .expect("shared_distinct_id must appear under One(env_a)");
    assert_eq!(shared_a.events_count, 4);
    assert_eq!(shared_a.errors_count, 1);
    assert_eq!(shared_a.sessions_count, 1);
    let session_only_a = rows_a
        .iter()
        .find(|r| r.distinct_id == ids.session_only_distinct_id)
        .expect(
            "session_only_distinct_id must appear under One(env_a) via the sessions leg of the \
             membership EXISTS alone — it has no analytics/error activity anywhere",
        );
    assert_eq!(session_only_a.events_count, 0);
    assert_eq!(session_only_a.errors_count, 0);
    assert_eq!(session_only_a.sessions_count, 1);
    assert_eq!(
        rows_a.iter().map(|r| r.events_count).sum::<i64>(),
        5,
        "matches analytics_events' env_a total"
    );
    assert_eq!(
        rows_a.iter().map(|r| r.errors_count).sum::<i64>(),
        4,
        "matches error_events' env_a total"
    );

    let rows_b = sauron_db::repo::list_persons(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
        None,
        50,
        0,
        common::default_person_sort(),
        TimeWindow::since(
            "last_seen",
            chrono::Utc::now() - chrono::Duration::days(3650),
        ),
    )
    .await
    .unwrap();
    assert_eq!(
        rows_b.len(),
        3,
        "env_b: shared_distinct_id, distinct_id_env_b_only, and the cross-env funnel identity"
    );
    assert!(
        rows_b
            .iter()
            .all(|r| r.distinct_id != ids.session_only_distinct_id),
        "session_only_distinct_id's one session is in env_a, not env_b — must not appear here"
    );
    let shared_b = rows_b
        .iter()
        .find(|r| r.distinct_id == ids.shared_distinct_id)
        .expect(
            "shared_distinct_id must appear under One(env_b) via its error/session activity \
             alone — the zero-analytics-events case",
        );
    assert_eq!(shared_b.events_count, 0);
    assert_eq!(shared_b.errors_count, 1);
    assert_eq!(shared_b.sessions_count, 1);
    let b_only = rows_b
        .iter()
        .find(|r| r.distinct_id == ids.distinct_id_env_b_only)
        .expect("distinct_id_env_b_only must appear under One(env_b)");
    assert_eq!(b_only.events_count, 4);
    assert_eq!(b_only.errors_count, 1);
    assert_eq!(b_only.sessions_count, 1);
    assert_eq!(rows_b.iter().map(|r| r.events_count).sum::<i64>(), 5);
    assert_eq!(rows_b.iter().map(|r| r.errors_count).sum::<i64>(), 2);

    let rows_none = sauron_db::repo::list_persons(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::Unattributed),
        None,
        50,
        0,
        common::default_person_sort(),
        TimeWindow::since(
            "last_seen",
            chrono::Utc::now() - chrono::Duration::days(3650),
        ),
    )
    .await
    .unwrap();
    assert_eq!(
        rows_none.len(),
        2,
        "only the two identities with an unattributed analytics/error row"
    );
    assert!(rows_none
        .iter()
        .all(|r| r.distinct_id != ids.shared_distinct_id
            && r.distinct_id != ids.distinct_id_env_b_only
            && r.distinct_id != ids.session_only_distinct_id));

    let rows_all = sauron_db::repo::list_persons(
        &mut conn,
        ReadScope::all(ids.app_id),
        None,
        50,
        0,
        common::default_person_sort(),
        TimeWindow::since(
            "last_seen",
            chrono::Utc::now() - chrono::Duration::days(3650),
        ),
    )
    .await
    .unwrap();
    assert_eq!(rows_all.len(), 8, "all 8 event_users identities");
    let shared_all = rows_all
        .iter()
        .find(|r| r.distinct_id == ids.shared_distinct_id)
        .unwrap();
    assert_eq!(shared_all.events_count, 4);
    assert_eq!(shared_all.errors_count, 2);
    assert_eq!(shared_all.sessions_count, 2);
    assert_eq!(
        rows_all.iter().map(|r| r.events_count).sum::<i64>(),
        11,
        "matches analytics_events' total across all three buckets"
    );
    assert_eq!(
        rows_all.iter().map(|r| r.errors_count).sum::<i64>(),
        7,
        "matches error_events' total across all three buckets"
    );
    // NOT 7 (sessions' own table total): 3 of the 7 seeded sessions still belong to
    // distinct_ids never registered in `event_users` via `note_identity` (session_only_a-2
    // and b-se-2's sibling `-a-se-2`/`-b-se-2`, plus the unattributed `-none-se-0`) — so
    // they have no person row to attach to at all, scoped or not.
    // session_only_distinct_id's 1 IS counted here (unlike before this task's seed change):
    // it is now registered, so its session ties to a real event_users row.
    assert_eq!(
        rows_all.iter().map(|r| r.sessions_count).sum::<i64>(),
        4,
        "the 4 sessions tied to a registered event_users identity"
    );

    drop(conn);
    db.cleanup().await;
}

/// `list_devices` mirrors `list_persons` exactly: every seeded row that
/// carries a `distinct_id` also carries a paired `device_key` (see
/// `seed_two_envs`), so the membership/count math is identical, keyed by
/// `device_key` instead.
///
/// `events_count`/`errors_count` come from a different source depending on
/// `scope.env` — see `list_devices`' (the function, not this test's) doc
/// comment for the full reasoning. Under `One`/`Unattributed` they come from
/// environment-scoped LATERALs (asserted below via `shared_a`/`shared_b`/
/// `b_only`, and via `session_only_a`, which proves the sessions leg of the
/// membership `EXISTS` — see that assertion's own comment). Under `All` they
/// come from the denormalized `devices` columns directly (asserted below via
/// `rows_all`'s sums, which must therefore match `devices`' own real,
/// cross-environment totals — not just `analytics_events`/`error_events`'
/// totals by coincidence of them being equal, which they are here).
#[tokio::test]
async fn list_devices_covers_only_the_selected_environment() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let rows_a = sauron_db::repo::list_devices(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        TimeWindow::since("last_seen", far_past()),
        50,
        0,
        device_sort(),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(rows_a.len(), 5);
    assert!(rows_a
        .iter()
        .all(|r| r.device_key != ids.device_key_env_b_only));
    let shared_a = rows_a
        .iter()
        .find(|r| r.device_key == ids.shared_device_key)
        .expect("shared_device_key must appear under One(env_a)");
    assert_eq!(shared_a.events_count, 4);
    assert_eq!(shared_a.errors_count, 1);
    assert_eq!(shared_a.sessions_count, 1);
    let session_only_a = rows_a
        .iter()
        .find(|r| r.device_key == ids.session_only_device_key)
        .expect(
            "session_only_device_key must appear under One(env_a) via the sessions leg of the \
             membership EXISTS alone — it has no analytics/error activity anywhere",
        );
    assert_eq!(session_only_a.events_count, 0);
    assert_eq!(session_only_a.errors_count, 0);
    assert_eq!(session_only_a.sessions_count, 1);

    let rows_b = sauron_db::repo::list_devices(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
        TimeWindow::since("last_seen", far_past()),
        50,
        0,
        device_sort(),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(rows_b.len(), 3);
    assert!(
        rows_b
            .iter()
            .all(|r| r.device_key != ids.session_only_device_key),
        "session_only_device_key's one session is in env_a, not env_b — must not appear here"
    );
    let shared_b = rows_b
        .iter()
        .find(|r| r.device_key == ids.shared_device_key)
        .expect("shared_device_key must appear under One(env_b) via error/session activity alone");
    assert_eq!(shared_b.events_count, 0);
    // 2, not 1: F4's seed extension repoints `issue_env_b_only`'s one error
    // event (`distinct_id_env_b_only`) onto `shared_device_key` (see
    // `SeedIds`'s doc comment) — this is device-level, keyed by `device_key`,
    // so it credits `shared_device_key`'s count even though the identity
    // behind it is not `shared_distinct_id`'s own.
    assert_eq!(shared_b.errors_count, 2);
    assert_eq!(shared_b.sessions_count, 1);
    let b_only = rows_b
        .iter()
        .find(|r| r.device_key == ids.device_key_env_b_only)
        .expect("device_key_env_b_only must appear under One(env_b)");
    assert_eq!(b_only.events_count, 4);
    // 0, not 1: its one error row was repointed to `shared_device_key` above —
    // `device_key_env_b_only` keeps its 2 analytics events (unaffected, still
    // its own device_key) and so still has membership, just zero errors now.
    assert_eq!(b_only.errors_count, 0);
    assert_eq!(b_only.sessions_count, 1);

    let rows_none = sauron_db::repo::list_devices(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::Unattributed),
        TimeWindow::since("last_seen", far_past()),
        50,
        0,
        device_sort(),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(rows_none.len(), 2);

    let rows_all = sauron_db::repo::list_devices(
        &mut conn,
        ReadScope::all(ids.app_id),
        TimeWindow::since("last_seen", far_past()),
        50,
        0,
        device_sort(),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(rows_all.len(), 8);
    // These now come straight off `devices.events_count`/`errors_count` (fix 1's
    // per-variant source rule), not a LATERAL — still 11/7 because every
    // analytics/error event that drives an env-scoped LATERAL also drove exactly
    // one `bump_device` call with a matching events_delta/errors_delta (see
    // `note_identity` in `tests/common/mod.rs`), so the two sources agree here.
    assert_eq!(rows_all.iter().map(|r| r.events_count).sum::<i64>(), 11);
    assert_eq!(rows_all.iter().map(|r| r.errors_count).sum::<i64>(), 7);
    // sessions_count stays a LATERAL under every variant (no durable column to
    // fall back to — see `list_devices`' doc comment), so this is unaffected by
    // fix 1. 4, not 3: session_only_device_key's one session is now tied to a
    // registered `devices` row (it wasn't before this task's seed change), and
    // 3 of the 7 seeded sessions still belong to distinct_ids never registered
    // via `note_identity` (a-se-2, b-se-2, and the unattributed none-se-0).
    assert_eq!(rows_all.iter().map(|r| r.sessions_count).sum::<i64>(), 4);

    drop(conn);
    db.cleanup().await;
}

/// `get_event_user`/`get_device` are single-identity lookups over tables with
/// no `environment_id` of their own — same membership derivation as
/// `list_persons`/`list_devices`, but returning `Option::None` (rather than
/// omitting a row from a page) when the identity has no activity in scope.
///
/// `get_device` also returns [`sauron_db::repo::DeviceRow`], not the raw
/// `Device` model — this is fix 2's core claim, so it is checked directly
/// below (`shared_device_key` under `All` vs. `One(env_a)`), not just
/// inferred from `Option::is_some()`.
#[tokio::test]
async fn get_event_user_and_get_device_are_scoped_by_membership() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    // shared_distinct_id/shared_device_key: present under One(env_a) and
    // One(env_b) (env_b via error/session activity alone — the
    // zero-analytics-events case), absent under Unattributed.
    for (env, expect_present) in [
        (EnvFilter::One(ids.env_a), true),
        (EnvFilter::One(ids.env_b), true),
        (EnvFilter::Unattributed, false),
    ] {
        let scope = ReadScope::new(ids.app_id, env.clone());
        let user =
            sauron_db::repo::get_event_user(&mut conn, scope.clone(), &ids.shared_distinct_id)
                .await
                .unwrap();
        assert_eq!(
            user.is_some(),
            expect_present,
            "get_event_user under {env:?}"
        );
        let device = sauron_db::repo::get_device(&mut conn, scope, &ids.shared_device_key)
            .await
            .unwrap();
        assert_eq!(device.is_some(), expect_present, "get_device under {env:?}");
    }
    // fix 2's per-variant source rule, checked directly: under One(env_a) the
    // LATERAL sees only env_a's activity (4 events, 1 error); under All the
    // durable `devices` columns see shared_device_key's whole lifetime (4
    // events, 2 errors — env_a's 4 analytics + 0 from env_b, 1 error each).
    let device_a = sauron_db::repo::get_device(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        &ids.shared_device_key,
    )
    .await
    .unwrap()
    .expect("shared_device_key must resolve under One(env_a)");
    assert_eq!(device_a.events_count, 4);
    assert_eq!(device_a.errors_count, 1);
    let device_all = sauron_db::repo::get_device(
        &mut conn,
        ReadScope::all(ids.app_id),
        &ids.shared_device_key,
    )
    .await
    .unwrap()
    .expect("shared_device_key must resolve under All");
    assert_eq!(
        device_all.events_count, 4,
        "durable devices.events_count: all of shared_device_key's analytics activity is in env_a"
    );
    assert_eq!(
        device_all.errors_count, 3,
        "durable devices.errors_count: env_a's own error, env_b's own error, plus \
         issue_env_b_only's error — F4's seed extension repoints that row's \
         device_key onto shared_device_key (see SeedIds's doc comment), and \
         bump_device credits whatever device_key note_identity is actually \
         called with, so the durable counter picks it up too"
    );
    assert!(sauron_db::repo::get_event_user(
        &mut conn,
        ReadScope::all(ids.app_id),
        &ids.shared_distinct_id
    )
    .await
    .unwrap()
    .is_some());

    // distinct_id_env_b_only/device_key_env_b_only: confined to env_b alone —
    // must resolve to None under One(env_a), not a row with zeroed activity.
    let none_a = sauron_db::repo::get_event_user(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        &ids.distinct_id_env_b_only,
    )
    .await
    .unwrap();
    assert!(
        none_a.is_none(),
        "distinct_id_env_b_only must not resolve under One(env_a)"
    );
    let some_b = sauron_db::repo::get_event_user(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
        &ids.distinct_id_env_b_only,
    )
    .await
    .unwrap();
    assert!(some_b.is_some());
    let device_none_a = sauron_db::repo::get_device(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        &ids.device_key_env_b_only,
    )
    .await
    .unwrap();
    assert!(
        device_none_a.is_none(),
        "device_key_env_b_only must not resolve under One(env_a)"
    );

    // session_only_device_key: resolves under One(env_a) via the sessions leg
    // of the membership EXISTS alone (zero analytics/error activity anywhere)
    // — must NOT resolve under One(env_b) or Unattributed, where it has no
    // row at all.
    let session_only_a = sauron_db::repo::get_device(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        &ids.session_only_device_key,
    )
    .await
    .unwrap()
    .expect("session_only_device_key must resolve under One(env_a) via the sessions leg alone");
    assert_eq!(session_only_a.events_count, 0);
    assert_eq!(session_only_a.errors_count, 0);
    assert_eq!(session_only_a.sessions_count, 1);
    let session_only_b = sauron_db::repo::get_device(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
        &ids.session_only_device_key,
    )
    .await
    .unwrap();
    assert!(session_only_b.is_none());

    drop(conn);
    db.cleanup().await;
}

/// `events_for_person`/`error_events_for_person`/`errors_for_device` read
/// tables that carry `environment_id` directly (`analytics_events`,
/// `error_events`) — no LATERAL/EXISTS involved, just an ordinary
/// `scope_env!` filter, unlike their `event_users`/`devices`-backed
/// siblings above.
#[tokio::test]
async fn events_and_errors_for_person_and_device_are_scoped_directly() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let events_a = sauron_db::repo::events_for_person(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        &ids.shared_distinct_id,
        50,
    )
    .await
    .unwrap();
    assert_eq!(
        events_a.len(),
        4,
        "shared_distinct_id's whole analytics history is in env_a"
    );
    let events_b = sauron_db::repo::events_for_person(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
        &ids.shared_distinct_id,
        50,
    )
    .await
    .unwrap();
    assert_eq!(
        events_b.len(),
        0,
        "shared_distinct_id has zero analytics events in env_b"
    );

    let errors_a = sauron_db::repo::error_events_for_person(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        &ids.shared_distinct_id,
        50,
    )
    .await
    .unwrap();
    assert_eq!(errors_a.len(), 1);
    let errors_b = sauron_db::repo::error_events_for_person(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
        &ids.shared_distinct_id,
        50,
    )
    .await
    .unwrap();
    assert_eq!(errors_b.len(), 1);
    let errors_all = sauron_db::repo::error_events_for_person(
        &mut conn,
        ReadScope::all(ids.app_id),
        &ids.shared_distinct_id,
        50,
    )
    .await
    .unwrap();
    assert_eq!(errors_all.len(), 2);

    let device_errors_a = sauron_db::repo::errors_for_device(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        &ids.shared_device_key,
        50,
    )
    .await
    .unwrap();
    assert_eq!(device_errors_a.len(), 1);
    let device_errors_b = sauron_db::repo::errors_for_device(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
        &ids.shared_device_key,
        50,
    )
    .await
    .unwrap();
    // 2, not 1: F4's seed extension repoints `issue_env_b_only`'s error event
    // onto `shared_device_key` (see `SeedIds`'s doc comment) — `errors_for_device`
    // reads `error_events` directly by `device_key`, so it picks up both rows.
    assert_eq!(device_errors_b.len(), 2);
    // Task 15 (F9): no earlier test exercised `errors_for_device` under `All` — add
    // it. `shared_device_key` has no unattributed `error_events` row (that bucket's
    // one row is on a different, `none`-prefixed device_key), so `All` must be
    // exactly env_a's 1 plus env_b's 2, with no double-count and no missing row.
    let device_errors_all = sauron_db::repo::errors_for_device(
        &mut conn,
        ReadScope::all(ids.app_id),
        &ids.shared_device_key,
        50,
    )
    .await
    .unwrap();
    assert_eq!(
        device_errors_all.len(),
        3,
        "All = env_a's 1 + env_b's 2 (repointed); no unattributed errors on this device_key"
    );

    drop(conn);
    db.cleanup().await;
}

// ===========================================================================
// F4 (final whole-branch review, pre-Slice-3 fix round): PersonRow/DeviceRow
// no longer mix per-environment counts with app-wide identity fields
// ===========================================================================

/// `PersonRow`/`DeviceRow` used to return the env-scoped `events_count`/
/// `errors_count`/`sessions_count` (Task 8) alongside `first_seen`/
/// `last_seen` read straight off the app-wide `event_users`/`devices` row —
/// and, on devices, `last_distinct_id` too — the same mixed-scope shape the
/// slice's own `overview_totals`/`user_stats` already document and guard
/// against elsewhere, on fields Task 8's own sweep didn't look at because it
/// only checked counts. `list_persons`/`list_devices`/`get_device` now derive
/// `first_seen`/`last_seen` as `LEAST`/`GREATEST` over the same
/// per-environment LATERALs that already produce the three counts, and
/// devices additionally derive `last_distinct_id` — the concrete disclosure
/// vector the review named: a device whose most recent identity is
/// production-only must not surface that identity under a staging scope.
/// `PersonRow::properties` is the one field deliberately left un-derived —
/// see its own doc comment for why that is a decision, not a gap.
///
/// Needs the F4 seed extension documented on `SeedIds` (`pinned_now`, the
/// `env_b` timestamp shifts on `shared_distinct_id`'s error/session, and the
/// `shared_device_key` repoint of `issue_env_b_only`'s error event) —
/// without it, `env_a` and `env_b` tied at exactly `now` for `last_seen`, and
/// no device in the seed was ever touched by two different identities, so
/// neither half of this could be asserted against a genuinely discriminating
/// case. See that doc comment for the full reasoning and exact offsets.
#[tokio::test]
async fn person_and_device_seen_and_identity_are_derived_per_environment() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let now = ids.pinned_now;

    // ----- PersonRow (list_persons): first_seen/last_seen, One(env_a) vs One(env_b) -----
    let persons_a = sauron_db::repo::list_persons(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        None,
        50,
        0,
        common::default_person_sort(),
        TimeWindow::since(
            "last_seen",
            chrono::Utc::now() - chrono::Duration::days(3650),
        ),
    )
    .await
    .unwrap();
    let shared_a = persons_a
        .iter()
        .find(|r| r.distinct_id == ids.shared_distinct_id)
        .expect("shared_distinct_id must appear under One(env_a)");

    let persons_b = sauron_db::repo::list_persons(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
        None,
        50,
        0,
        common::default_person_sort(),
        TimeWindow::since(
            "last_seen",
            chrono::Utc::now() - chrono::Duration::days(3650),
        ),
    )
    .await
    .unwrap();
    let shared_b = persons_b
        .iter()
        .find(|r| r.distinct_id == ids.shared_distinct_id)
        .expect("shared_distinct_id must appear under One(env_b)");

    assert_eq!(
        shared_a.first_seen,
        now - Duration::seconds(240),
        "env_a's earliest signal for shared_distinct_id is its own '$screen' analytics row"
    );
    assert_eq!(
        shared_a.last_seen, now,
        "env_a's most recent signal is its error/session tie, both at `now`"
    );
    assert_eq!(
        shared_b.first_seen,
        now - Duration::seconds(345),
        "env_b's earliest signal is session_b0's started_at (now - 300s duration, ends at now - 45s)"
    );
    assert_eq!(
        shared_b.last_seen,
        now - Duration::seconds(30),
        "env_b's most recent signal is its own error event, now at now - 30s (Task 8: retimed \
         off session_b0's now - 45s to also carry issue_shared's env_b title/culprit — see \
         SeedIds' doc comment)"
    );
    // The whole point: under the old app-wide `eu.first_seen`/`eu.last_seen`
    // read, all four of the values above would have been identical regardless
    // of scope. They must differ by environment now.
    assert_ne!(shared_a.first_seen, shared_b.first_seen);
    assert_ne!(shared_a.last_seen, shared_b.last_seen);

    // `properties` stays app-wide and un-derived by design (see `PersonRow`'s
    // doc comment) — confirming the documented no-change path is still true,
    // not asserting new behaviour.
    assert_eq!(shared_a.properties, shared_b.properties);

    // ----- DeviceRow (list_devices/get_device): first_seen/last_seen + last_distinct_id -----
    let devices_a = sauron_db::repo::list_devices(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        TimeWindow::since("last_seen", far_past()),
        50,
        0,
        device_sort(),
        None,
        None,
    )
    .await
    .unwrap();
    let device_shared_a = devices_a
        .iter()
        .find(|r| r.device_key == ids.shared_device_key)
        .expect("shared_device_key must appear under One(env_a)");
    assert_eq!(device_shared_a.first_seen, now - Duration::seconds(240));
    assert_eq!(device_shared_a.last_seen, now);
    assert_eq!(
        device_shared_a.last_distinct_id,
        Some(ids.shared_distinct_id.clone()),
        "env_a's only identity ever seen on this device is shared_distinct_id itself"
    );

    // get_device must agree with list_devices — same derivation, different
    // function (and, under One(env_a), no `since`-vs-unbounded discrepancy to
    // worry about either: see get_device's doc comment).
    let device_a = sauron_db::repo::get_device(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        &ids.shared_device_key,
    )
    .await
    .unwrap()
    .expect("shared_device_key must resolve under One(env_a)");
    assert_eq!(device_a.first_seen, device_shared_a.first_seen);
    assert_eq!(device_a.last_seen, device_shared_a.last_seen);
    assert_eq!(device_a.last_distinct_id, device_shared_a.last_distinct_id);

    // The disclosure case: under One(env_b), shared_device_key's most recent
    // signal is the F4 seed extension's repointed error — a DIFFERENT
    // identity (distinct_id_env_b_only) that has never touched env_a at all.
    let device_b = sauron_db::repo::get_device(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
        &ids.shared_device_key,
    )
    .await
    .unwrap()
    .expect("shared_device_key must resolve under One(env_b)");
    assert_eq!(device_b.first_seen, now - Duration::seconds(345));
    assert_eq!(device_b.last_seen, now - Duration::seconds(10));
    assert_eq!(
        device_b.last_distinct_id,
        Some(ids.distinct_id_env_b_only.clone()),
        "env_b's most recent signal on this device is the repointed error, whose identity \
         is distinct_id_env_b_only, not shared_distinct_id"
    );

    // The assertion that matters most: under One(env_a), this device's
    // derived identity must NOT be the one that appears only in env_b. Under
    // the old code (`devices.last_distinct_id`, read regardless of scope)
    // this would have been `distinct_id_env_b_only` under BOTH env_a and
    // env_b — that repointed write is the last one `bump_device` sees for
    // this device_key during seeding, and `bump_device`'s `last_distinct_id`
    // column carries no notion of "as of which environment" at all.
    assert_ne!(
        device_shared_a.last_distinct_id,
        Some(ids.distinct_id_env_b_only.clone()),
        "a device scoped to env_a must never surface an identity that appears only in env_b"
    );

    drop(conn);
    db.cleanup().await;
}

/// S2 follow-up (`.superpowers/sdd/s2-get-event-user-fix.md`): `get_event_user`
/// was the one instance of this same bug class the F4 review above named
/// `PersonRow`/`DeviceRow` for but not for — its raw `EventUser` return still
/// carried `first_seen`/`last_seen` straight off the app-wide `event_users`
/// row, rendered by the Person Profile page directly beside an
/// environment-scoped events/errors list. `get_event_user` now returns
/// [`sauron_db::repo::PersonRow`] (not the raw `EventUser` model), deriving
/// `first_seen`/`last_seen` exactly as `list_persons` does — see
/// `repo::get_event_user`'s doc comment.
///
/// Reuses the exact per-environment values the test above already established
/// for `shared_distinct_id` via `list_persons` (`pinned_now`-relative, from
/// the F4 seed extension documented on `SeedIds`): `get_event_user` must
/// *agree* with `list_persons` for the same identity/scope, not just
/// independently look plausible.
#[tokio::test]
async fn get_event_user_seen_is_derived_per_environment_not_app_wide() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let now = ids.pinned_now;

    let user_a = sauron_db::repo::get_event_user(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        &ids.shared_distinct_id,
    )
    .await
    .unwrap()
    .expect("shared_distinct_id must resolve under One(env_a)");
    assert_eq!(
        user_a.first_seen,
        now - Duration::seconds(240),
        "env_a's earliest signal for shared_distinct_id is its own '$screen' analytics \
         row — the same value list_persons derives for the identical identity/scope"
    );
    assert_eq!(
        user_a.last_seen, now,
        "env_a's most recent signal is its error/session tie, both at `now`"
    );

    let user_b = sauron_db::repo::get_event_user(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
        &ids.shared_distinct_id,
    )
    .await
    .unwrap()
    .expect("shared_distinct_id must resolve under One(env_b)");
    assert_eq!(user_b.first_seen, now - Duration::seconds(345));
    // Task 8 retimed this occurrence from now - 45s to now - 30s (see
    // SeedIds' doc comment on `issue_shared`) — must agree with
    // `person_and_device_seen_and_identity_are_derived_per_environment`'s
    // `shared_b.last_seen`.
    assert_eq!(user_b.last_seen, now - Duration::seconds(30));

    // The whole point: under the old app-wide `eu.first_seen`/`eu.last_seen`
    // read, `user_a` and `user_b` would have been identical regardless of
    // scope. They must differ by environment now.
    assert_ne!(user_a.first_seen, user_b.first_seen);
    assert_ne!(user_a.last_seen, user_b.last_seen);

    // The disclosure case the task names directly: a person first seen in
    // production a year ago must not carry that timestamp into a
    // staging-only view. `All`'s exact value depends on the wall-clock time
    // `note_identity` ran at (the `event_users` row's real `first_seen`
    // DEFAULT / `last_seen` bump), not a `pinned_now` offset, so this asserts
    // inequality against the scoped values rather than a third exact value —
    // see `SeedIds`'s doc comment on `pinned_now` for why the two clocks are
    // independent.
    let user_all = sauron_db::repo::get_event_user(
        &mut conn,
        ReadScope::all(ids.app_id),
        &ids.shared_distinct_id,
    )
    .await
    .unwrap()
    .expect("shared_distinct_id must resolve under All");
    assert_ne!(
        user_all.first_seen, user_a.first_seen,
        "All's app-wide first_seen must not leak into a One(env_a)-scoped read"
    );
    assert_ne!(
        user_all.last_seen, user_a.last_seen,
        "All's app-wide last_seen must not leak into a One(env_a)-scoped read"
    );

    drop(conn);
    db.cleanup().await;
}

// ===========================================================================
// Task 9: issue reads compute per-environment counts
// ===========================================================================

/// The assertion that matters is the *counts*, not mere presence — a
/// membership-only filter would return `issue_id` in all three buckets with
/// `times_seen == 6` and this would still pass a presence-only check.
#[tokio::test]
async fn list_issues_reports_per_environment_counts_not_app_wide() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let a = sauron_db::repo::list_issues(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        &[],
        None,
        far_past(),
        50,
        0,
    )
    .await
    .unwrap();
    let issue = a
        .iter()
        .find(|i| i.id == ids.issue_id)
        .expect("issue appears under env_a");
    assert_eq!(
        issue.times_seen, 4,
        "must be env_a's count, not the app-wide 6"
    );

    let b = sauron_db::repo::list_issues(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
        &[],
        None,
        far_past(),
        50,
        0,
    )
    .await
    .unwrap();
    assert_eq!(
        b.iter().find(|i| i.id == ids.issue_id).unwrap().times_seen,
        1
    );

    let none = sauron_db::repo::list_issues(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::Unattributed),
        &[],
        None,
        far_past(),
        50,
        0,
    )
    .await
    .unwrap();
    assert_eq!(
        none.iter()
            .find(|i| i.id == ids.issue_id)
            .unwrap()
            .times_seen,
        1
    );

    let all = sauron_db::repo::list_issues(
        &mut conn,
        ReadScope::all(ids.app_id),
        &[],
        None,
        far_past(),
        50,
        0,
    )
    .await
    .unwrap();
    assert_eq!(
        all.iter()
            .find(|i| i.id == ids.issue_id)
            .unwrap()
            .times_seen,
        6
    );

    drop(conn); // required before cleanup(): the pool is sized 1 and would deadlock
    db.cleanup().await;
}

/// Proves the LATERAL is an *inner* join, not a `LEFT JOIN`. `issue_env_b_only`
/// has its one error event confined to `env_b` alone (see `SeedIds`'s doc
/// comment) — it must not appear at all under `One(env_a)` or `Unattributed`.
/// A `LEFT JOIN LATERAL` would still return the row (with `agg.last_seen` NULL,
/// which combined with `far_past()` as `since` would make `agg.last_seen >=
/// since` false and coincidentally also drop it — so this test additionally
/// pins `issue_env_b_only`'s presence under `One(env_b)` with the *correct*
/// count, which a `LEFT JOIN` cannot forge since the aggregate is only ever
/// wrong, never fabricated with the right value by accident).
#[tokio::test]
async fn list_issues_membership_is_an_inner_join_not_a_left_join() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let a = sauron_db::repo::list_issues(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        &[],
        None,
        far_past(),
        50,
        0,
    )
    .await
    .unwrap();
    assert!(
        a.iter().all(|i| i.id != ids.issue_env_b_only),
        "issue_env_b_only has zero occurrences in env_a and must not appear at all"
    );

    let none = sauron_db::repo::list_issues(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::Unattributed),
        &[],
        None,
        far_past(),
        50,
        0,
    )
    .await
    .unwrap();
    assert!(
        none.iter().all(|i| i.id != ids.issue_env_b_only),
        "issue_env_b_only has no unattributed occurrence and must not appear at all"
    );

    let b = sauron_db::repo::list_issues(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
        &[],
        None,
        far_past(),
        50,
        0,
    )
    .await
    .unwrap();
    let b_only = b
        .iter()
        .find(|i| i.id == ids.issue_env_b_only)
        .expect("issue_env_b_only must appear under its own environment");
    assert_eq!(b_only.times_seen, 1);

    drop(conn);
    db.cleanup().await;
}

/// The brief's required test and the two above all call `list_issues` with
/// `filters: &[]`/`q: None`, which never exercises the scoped path's dynamic
/// two-pass bind bookkeeping (build the `$N` placeholder text, then a second
/// pass applies `.bind()` calls in the same order) — a mismatch there is a
/// runtime bind-count error, not a compile error, so it is invisible unless a
/// real query with filters actually runs. Exercises `level`/`times_seen`
/// (plain column filters), `tag:eq`/`tag:contains` (their own `EXISTS`
/// sub-binds), and free-text `q` (reuses the `$2` `since` bind plus its own
/// pattern bind) together in one call, under `One`.
#[tokio::test]
async fn list_issues_filters_tag_and_free_text_compose_with_scope() {
    use sauron_db::filter::{Op, ParsedFilter};

    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let now = Utc::now();
    let issue_id = sauron_db::repo::upsert_issue(
        &mut conn,
        sauron_db::models::NewIssue {
            app_id: ids.app_id,
            fingerprint: "filter-compose-test-fingerprint",
            type_: "Error",
            title: "filter-compose distinctive-marker issue",
            culprit: "harness::filter_compose",
            level: "warning",
            first_seen: now,
            last_seen: now,
            times_seen: 1,
        },
    )
    .await
    .expect("create filter-compose issue");
    sauron_db::repo::insert_error_event(
        &mut conn,
        NewErrorEvent {
            id: Uuid::new_v4(),
            app_id: ids.app_id,
            environment_id: Some(ids.env_a),
            issue_id,
            fingerprint: "filter-compose-test-fingerprint".into(),
            level: "warning".into(),
            message: "filter-compose distinctive-marker message".into(),
            exception_type: "HarnessError".into(),
            exception_value: "seeded".into(),
            stacktrace: json!([]),
            breadcrumbs: json!([]),
            context: json!({}),
            tags: json!({"release": "1.2.3"}),
            release: None,
            distinct_id: Some("filter-compose-user".into()),
            event_user: None,
            sdk: None,
            ip_address: None,
            occurred_at: now,
            session_id: None,
            device_key: None,
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
        },
    )
    .await
    .expect("insert filter-compose error event");

    let scope_a = ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a));

    // Plain column filters (level eq, times_seen gt) — bind types Text/BigInt.
    let by_level = sauron_db::repo::list_issues(
        &mut conn,
        scope_a.clone(),
        &[ParsedFilter {
            field: "level",
            op: Op::Eq,
            value: "warning".to_string(),
        }],
        None,
        far_past(),
        50,
        0,
    )
    .await
    .unwrap();
    assert!(
        by_level.iter().any(|i| i.id == issue_id),
        "level:eq:warning must match the filter-compose issue"
    );
    assert!(
        by_level.iter().all(|i| i.id != ids.issue_id),
        "level:eq:warning must not match the seed's level=error issue"
    );

    // tag:eq — own EXISTS sub-bind (Jsonb).
    let by_tag_eq = sauron_db::repo::list_issues(
        &mut conn,
        scope_a.clone(),
        &[ParsedFilter {
            field: "tag",
            op: Op::Eq,
            value: "release=1.2.3".to_string(),
        }],
        None,
        far_past(),
        50,
        0,
    )
    .await
    .unwrap();
    assert!(by_tag_eq.iter().any(|i| i.id == issue_id));

    // tag:contains — two EXISTS sub-binds (Text key, Text ILIKE pattern).
    let by_tag_contains = sauron_db::repo::list_issues(
        &mut conn,
        scope_a.clone(),
        &[ParsedFilter {
            field: "tag",
            op: Op::Contains,
            value: "release=1.2".to_string(),
        }],
        None,
        far_past(),
        50,
        0,
    )
    .await
    .unwrap();
    assert!(by_tag_contains.iter().any(|i| i.id == issue_id));

    // Free-text q — reuses the $2 since bind inside its own EXISTS, plus its
    // own pattern bind, combined with a plain filter in the same call so the
    // two-pass bind bookkeeping is exercised together, not in isolation.
    let by_q_and_filter = sauron_db::repo::list_issues(
        &mut conn,
        scope_a,
        &[ParsedFilter {
            field: "times_seen",
            op: Op::Gt,
            value: "0".to_string(),
        }],
        Some("distinctive-marker"),
        far_past(),
        50,
        0,
    )
    .await
    .unwrap();
    assert!(
        by_q_and_filter.iter().any(|i| i.id == issue_id),
        "q free-text match (title) combined with a times_seen filter must still find the issue"
    );
    assert!(
        by_q_and_filter.iter().all(|i| i.id != ids.issue_env_b_only),
        "issue_env_b_only has no occurrence in env_a regardless of filters/q"
    );

    drop(conn);
    db.cleanup().await;
}

/// `since` must be checked against the environment-*derived* `last_seen`, not
/// `issues.last_seen` (app-wide). Builds a dedicated issue (outside
/// `seed_two_envs`'s shared fixture, whose own `issue_id` has all six error
/// events pinned to the identical seed timestamp and so cannot exercise this)
/// with one occurrence 40 days ago in `env_a` and one occurrence 1 day ago in
/// `env_b`. Under a 7-day `since`, `env_a`'s view must NOT surface it (its
/// only env_a activity is 40 days stale) even though the issue's app-wide
/// `last_seen` (driven by the env_b row) is recent; `env_b`'s view must.
#[tokio::test]
async fn list_issues_since_applies_to_the_derived_last_seen_not_the_issues_own() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let old = Utc::now() - Duration::days(40);
    let recent = Utc::now() - Duration::days(1);
    let since_7d = Utc::now() - Duration::days(7);

    let issue_id = sauron_db::repo::upsert_issue(
        &mut conn,
        sauron_db::models::NewIssue {
            app_id: ids.app_id,
            fingerprint: "since-derived-test-fingerprint",
            type_: "HarnessError",
            title: "since-derived test issue",
            culprit: "harness",
            level: "error",
            first_seen: old,
            last_seen: old,
            times_seen: 1,
        },
    )
    .await
    .expect("create dedicated issue");

    // env_a: one occurrence, 40 days ago.
    sauron_db::repo::insert_error_event(
        &mut conn,
        NewErrorEvent {
            id: Uuid::new_v4(),
            app_id: ids.app_id,
            environment_id: Some(ids.env_a),
            issue_id,
            fingerprint: "since-derived-test-fingerprint".into(),
            level: "error".into(),
            message: "old env_a occurrence".into(),
            exception_type: "HarnessError".into(),
            exception_value: "seeded".into(),
            stacktrace: json!([]),
            breadcrumbs: json!([]),
            context: json!({}),
            tags: json!({}),
            release: None,
            distinct_id: Some("since-derived-user-a".into()),
            event_user: None,
            sdk: None,
            ip_address: None,
            occurred_at: old,
            session_id: None,
            device_key: None,
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
        },
    )
    .await
    .expect("insert old env_a error event");

    // env_b: one occurrence, 1 day ago — this is what makes the issue's
    // app-wide `issues.last_seen` recent, via the ON CONFLICT upsert's
    // `last_seen = excluded(last_seen)` — even though `env_a` has never seen
    // anything but the 40-day-old row.
    sauron_db::repo::insert_error_event(
        &mut conn,
        NewErrorEvent {
            id: Uuid::new_v4(),
            app_id: ids.app_id,
            environment_id: Some(ids.env_b),
            issue_id,
            fingerprint: "since-derived-test-fingerprint".into(),
            level: "error".into(),
            message: "recent env_b occurrence".into(),
            exception_type: "HarnessError".into(),
            exception_value: "seeded".into(),
            stacktrace: json!([]),
            breadcrumbs: json!([]),
            context: json!({}),
            tags: json!({}),
            release: None,
            distinct_id: Some("since-derived-user-b".into()),
            event_user: None,
            sdk: None,
            ip_address: None,
            occurred_at: recent,
            session_id: None,
            device_key: None,
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
        },
    )
    .await
    .expect("insert recent env_b error event");
    // Mirror `upsert_issue`'s own ON CONFLICT semantics so `issues.last_seen`
    // really is the recent env_b timestamp, exactly like ingest would leave
    // it (`process_error` calls `upsert_issue` per event; a second call here
    // mimics the env_b event's own ingest-time upsert).
    sauron_db::repo::upsert_issue(
        &mut conn,
        sauron_db::models::NewIssue {
            app_id: ids.app_id,
            fingerprint: "since-derived-test-fingerprint",
            type_: "HarnessError",
            title: "since-derived test issue",
            culprit: "harness",
            level: "error",
            first_seen: old,
            last_seen: recent,
            times_seen: 1,
        },
    )
    .await
    .expect("bump issue to the recent env_b timestamp");

    let a = sauron_db::repo::list_issues(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        &[],
        None,
        since_7d,
        50,
        0,
    )
    .await
    .unwrap();
    assert!(
        a.iter().all(|i| i.id != issue_id),
        "env_a's only occurrence is 40 days stale — must not surface in a 7-day view \
         merely because the issue's app-wide last_seen (driven by env_b) is recent"
    );

    let b = sauron_db::repo::list_issues(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
        &[],
        None,
        since_7d,
        50,
        0,
    )
    .await
    .unwrap();
    assert!(
        b.iter().any(|i| i.id == issue_id),
        "env_b's occurrence is 1 day old — must surface in a 7-day view"
    );

    drop(conn);
    db.cleanup().await;
}

/// `list_issues` must page the returned rows by the *derived*, per-environment
/// `last_seen` (`agg.last_seen`) — the value actually displayed — not by
/// `issues.last_seen` (app-wide). Same shape as
/// `list_issues_and_top_issues_page_by_environment_membership_not_app_wide_ranking`'s
/// coverage of `top_issues`' identical bug for `times_seen`; this is the
/// `list_issues` counterpart for `last_seen`.
///
/// Reuses the discriminator from
/// `list_issues_since_applies_to_the_derived_last_seen_not_the_issues_own`
/// (an occurrence 40 days stale in `env_a`, 1 day fresh in `env_b`, so the
/// issue's app-wide `last_seen` is recent while its `env_a`-derived one is
/// not) — confirmed against that test rather than assumed to be part of
/// `seed_two_envs`'s shared fixture, which pins every occurrence to the
/// identical seed timestamp and so cannot tell these two orderings apart.
/// Pairs it with a second issue whose *only* activity, anywhere, is a single
/// `env_a` occurrence 5 days ago: its app-wide `last_seen` (5 days ago) is
/// therefore *older* than the first issue's app-wide `last_seen` (1 day ago,
/// driven by `env_b`), but its `env_a`-derived `last_seen` (5 days ago) is
/// *more recent* than the first issue's `env_a`-derived one (40 days ago).
/// The two orderings disagree, which is what makes this discriminating:
/// pre-fix (`ORDER BY i.last_seen DESC`) ranks the 40-day-stale issue first;
/// post-fix (`ORDER BY agg.last_seen DESC`) ranks the 5-day-old one first.
#[tokio::test]
async fn list_issues_orders_by_the_derived_last_seen_not_the_issues_own() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let old_40d = Utc::now() - Duration::days(40);
    let recent_1d = Utc::now() - Duration::days(1);
    let mid_5d = Utc::now() - Duration::days(5);

    // Issue A: only env_a activity is 40 days stale; app-wide `last_seen` is
    // dragged recent (1 day ago) by an env_b occurrence.
    let stale_in_env_a = sauron_db::repo::upsert_issue(
        &mut conn,
        sauron_db::models::NewIssue {
            app_id: ids.app_id,
            fingerprint: "order-test-stale-in-env-a",
            type_: "HarnessError",
            title: "order test: stale in env_a, recent app-wide",
            culprit: "harness",
            level: "error",
            first_seen: old_40d,
            last_seen: old_40d,
            times_seen: 1,
        },
    )
    .await
    .expect("create stale_in_env_a issue");

    sauron_db::repo::insert_error_event(
        &mut conn,
        NewErrorEvent {
            id: Uuid::new_v4(),
            app_id: ids.app_id,
            environment_id: Some(ids.env_a),
            issue_id: stale_in_env_a,
            fingerprint: "order-test-stale-in-env-a".into(),
            level: "error".into(),
            message: "old env_a occurrence".into(),
            exception_type: "HarnessError".into(),
            exception_value: "seeded".into(),
            stacktrace: json!([]),
            breadcrumbs: json!([]),
            context: json!({}),
            tags: json!({}),
            release: None,
            distinct_id: Some("order-test-user-a".into()),
            event_user: None,
            sdk: None,
            ip_address: None,
            occurred_at: old_40d,
            session_id: None,
            device_key: None,
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
        },
    )
    .await
    .expect("insert old env_a error event");

    sauron_db::repo::insert_error_event(
        &mut conn,
        NewErrorEvent {
            id: Uuid::new_v4(),
            app_id: ids.app_id,
            environment_id: Some(ids.env_b),
            issue_id: stale_in_env_a,
            fingerprint: "order-test-stale-in-env-a".into(),
            level: "error".into(),
            message: "recent env_b occurrence".into(),
            exception_type: "HarnessError".into(),
            exception_value: "seeded".into(),
            stacktrace: json!([]),
            breadcrumbs: json!([]),
            context: json!({}),
            tags: json!({}),
            release: None,
            distinct_id: Some("order-test-user-b".into()),
            event_user: None,
            sdk: None,
            ip_address: None,
            occurred_at: recent_1d,
            session_id: None,
            device_key: None,
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
        },
    )
    .await
    .expect("insert recent env_b error event");
    // Mirror the ON CONFLICT upsert `process_error` does at real ingest, so
    // `issues.last_seen` really is the recent env_b timestamp.
    sauron_db::repo::upsert_issue(
        &mut conn,
        sauron_db::models::NewIssue {
            app_id: ids.app_id,
            fingerprint: "order-test-stale-in-env-a",
            type_: "HarnessError",
            title: "order test: stale in env_a, recent app-wide",
            culprit: "harness",
            level: "error",
            first_seen: old_40d,
            last_seen: recent_1d,
            times_seen: 1,
        },
    )
    .await
    .expect("bump stale_in_env_a to the recent env_b timestamp");

    // Issue B: only activity anywhere is one env_a occurrence, 5 days ago.
    // App-wide and env_a-derived `last_seen` coincide (both 5 days ago).
    let recent_in_env_a = sauron_db::repo::upsert_issue(
        &mut conn,
        sauron_db::models::NewIssue {
            app_id: ids.app_id,
            fingerprint: "order-test-recent-in-env-a",
            type_: "HarnessError",
            title: "order test: recent in env_a only",
            culprit: "harness",
            level: "error",
            first_seen: mid_5d,
            last_seen: mid_5d,
            times_seen: 1,
        },
    )
    .await
    .expect("create recent_in_env_a issue");

    sauron_db::repo::insert_error_event(
        &mut conn,
        NewErrorEvent {
            id: Uuid::new_v4(),
            app_id: ids.app_id,
            environment_id: Some(ids.env_a),
            issue_id: recent_in_env_a,
            fingerprint: "order-test-recent-in-env-a".into(),
            level: "error".into(),
            message: "env_a occurrence".into(),
            exception_type: "HarnessError".into(),
            exception_value: "seeded".into(),
            stacktrace: json!([]),
            breadcrumbs: json!([]),
            context: json!({}),
            tags: json!({}),
            release: None,
            distinct_id: Some("order-test-user-c".into()),
            event_user: None,
            sdk: None,
            ip_address: None,
            occurred_at: mid_5d,
            session_id: None,
            device_key: None,
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
        },
    )
    .await
    .expect("insert env_a occurrence for recent_in_env_a");

    let page = sauron_db::repo::list_issues(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        &[],
        None,
        far_past(),
        50,
        0,
    )
    .await
    .unwrap();

    let position_of = |id: Uuid| {
        page.iter()
            .position(|i| i.id == id)
            .unwrap_or_else(|| panic!("issue {id} missing from the One(env_a) page"))
    };
    let stale_pos = position_of(stale_in_env_a);
    let recent_pos = position_of(recent_in_env_a);
    assert!(
        recent_pos < stale_pos,
        "recent_in_env_a (env_a-derived last_seen: 5 days ago) must sort ahead of \
         stale_in_env_a (env_a-derived last_seen: 40 days ago) under One(env_a), even \
         though stale_in_env_a's app-wide last_seen (dragged recent by an env_b \
         occurrence) is the more recent of the two app-wide. The page must be ordered \
         by the *displayed*, per-environment agg.last_seen, not issues.last_seen — \
         pre-fix (ORDER BY i.last_seen DESC) this ranks stale_in_env_a first instead."
    );

    // The displayed value itself must be the env_a occurrence (40 days ago),
    // not the app-wide one (1 day ago) — a future change to the ordering
    // alone, without also fixing what's selected, would still fail here.
    let stale_row = page.iter().find(|i| i.id == stale_in_env_a).unwrap();
    assert!(
        (stale_row.last_seen - old_40d).num_seconds().abs() < 5,
        "stale_in_env_a's displayed last_seen must be its env_a occurrence (40 days \
         ago), not its app-wide one (1 day ago): got {}",
        stale_row.last_seen
    );

    drop(conn);
    db.cleanup().await;
}

/// `get_issue`, `top_issues`, `issue_stats`, `issue_occurrence_series`,
/// `list_error_events_for_issue` and `latest_error_event` all share the same
/// membership/derivation rules `list_issues` established — asserted here
/// together against the same seeded `issue_id`/`issue_env_b_only` pair.
#[tokio::test]
async fn issue_detail_reads_are_scoped_by_environment() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    // get_issue: counts derived per environment; out-of-scope is None.
    let issue_a = sauron_db::repo::get_issue(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        ids.issue_id,
    )
    .await
    .unwrap()
    .expect("issue_id appears under env_a");
    assert_eq!(issue_a.times_seen, 4);
    let issue_b = sauron_db::repo::get_issue(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
        ids.issue_id,
    )
    .await
    .unwrap()
    .expect("issue_id appears under env_b");
    assert_eq!(issue_b.times_seen, 1);
    let issue_all = sauron_db::repo::get_issue(&mut conn, ReadScope::all(ids.app_id), ids.issue_id)
        .await
        .unwrap()
        .expect("issue_id appears under All");
    assert_eq!(issue_all.times_seen, 6);
    let b_only_under_a = sauron_db::repo::get_issue(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        ids.issue_env_b_only,
    )
    .await
    .unwrap();
    assert!(
        b_only_under_a.is_none(),
        "issue_env_b_only has no occurrence in env_a — get_issue must return None, not a \
         zero-count row"
    );

    // top_issues: membership + derived times_seen, same rules.
    let top_a = sauron_db::repo::top_issues(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        far_past(),
        50,
    )
    .await
    .unwrap();
    let top_issue_a = top_a
        .iter()
        .find(|i| i.id == ids.issue_id)
        .expect("issue_id appears in top_issues under env_a");
    assert_eq!(top_issue_a.times_seen, 4);
    assert!(
        top_a.iter().all(|i| i.id != ids.issue_env_b_only),
        "issue_env_b_only must not appear in top_issues under env_a"
    );

    // issue_stats: membership-only (status/level are issue-level, not
    // per-environment) — both issues count toward env_b's total (issue_id has
    // an env_b occurrence too), only issue_id counts toward env_a's.
    let stats_a = sauron_db::repo::issue_stats(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
    )
    .await
    .unwrap();
    assert_eq!(stats_a.total, 1, "only issue_id has occurrences in env_a");
    let stats_b = sauron_db::repo::issue_stats(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
    )
    .await
    .unwrap();
    assert_eq!(
        stats_b.total, 2,
        "both issue_id and issue_env_b_only have occurrences in env_b"
    );
    let stats_all = sauron_db::repo::issue_stats(&mut conn, ReadScope::all(ids.app_id))
        .await
        .unwrap();
    assert_eq!(stats_all.total, 2, "both issues exist app-wide");

    // issue_occurrence_series: error_events carries environment_id directly.
    let series_a = sauron_db::repo::issue_occurrence_series(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        ids.issue_id,
        far_past(),
    )
    .await
    .unwrap();
    assert_eq!(series_a.iter().map(|p| p.count).sum::<i64>(), 4);
    let series_b = sauron_db::repo::issue_occurrence_series(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
        ids.issue_id,
        far_past(),
    )
    .await
    .unwrap();
    assert_eq!(series_b.iter().map(|p| p.count).sum::<i64>(), 1);

    // list_error_events_for_issue: same direct scoping.
    let events_a = sauron_db::repo::list_error_events_for_issue(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        ids.issue_id,
        &[],
        None,
        None,
        50,
    )
    .await
    .unwrap();
    assert_eq!(events_a.len(), 4);
    let events_b = sauron_db::repo::list_error_events_for_issue(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
        ids.issue_id,
        &[],
        None,
        None,
        50,
    )
    .await
    .unwrap();
    assert_eq!(events_b.len(), 1);

    // latest_error_event: the function `grep "app_id: Uuid"` cannot find —
    // must still respect scope. Its one env_b occurrence must be returned
    // under One(env_b) and nothing must be returned for issue_env_b_only
    // under One(env_a).
    let latest_b = sauron_db::repo::latest_error_event(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
        ids.issue_id,
    )
    .await
    .unwrap()
    .expect("issue_id has one env_b error event");
    assert_eq!(latest_b.environment_id, Some(ids.env_b));

    // Task 15 (F9, `.superpowers/sdd/s2-final-review.md`): `latest_b` above scopes to
    // env_b, where `issue_id` has only ONE occurrence — `ORDER BY occurred_at DESC
    // LIMIT 1` is trivial there and pins no actual ordering. env_a has FOUR
    // occurrences for `issue_id`, so scope there instead: the true latest is
    // `a-er-1` (Task 9's retime to `pinned_now + 5s` — see `SeedIds`' doc comment on
    // `issue_shared`), strictly after the other three env_a rows, which all land on
    // the literal `pinned_now`. A regression that picked any of those three instead
    // (e.g. an unstable tie-break, or a predicate that silently widened past env_a)
    // would return a row at `pinned_now`, not `pinned_now + 5s`.
    let latest_a = sauron_db::repo::latest_error_event(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        ids.issue_id,
    )
    .await
    .unwrap()
    .expect("issue_id has four env_a error events");
    assert_eq!(latest_a.environment_id, Some(ids.env_a));
    assert_eq!(
        latest_a.occurred_at,
        ids.pinned_now + Duration::seconds(5),
        "the true latest of env_a's four occurrences, not a tie among the other three"
    );

    let latest_b_only_under_a = sauron_db::repo::latest_error_event(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        ids.issue_env_b_only,
    )
    .await
    .unwrap();
    assert!(
        latest_b_only_under_a.is_none(),
        "issue_env_b_only has no error event in env_a"
    );

    drop(conn);
    db.cleanup().await;
}

/// Regression test for Task 10's "Critical 1": the `tag`/free-text `q`
/// `EXISTS` fragments in `list_issues`' scoped path carried no environment
/// predicate, so a tag or payload match in *any* environment could surface —
/// or, for `q`, extract characters from — an issue under a scope that
/// excludes that environment, as long as the issue happened to be a genuine
/// member of the selected environment via some *other*, unrelated
/// occurrence. `issue_env_b_only` (the existing seed fixture used by
/// `list_issues_filters_tag_and_free_text_compose_with_scope`) cannot catch
/// this: it isn't an `env_a` member at all, so Critical 2's membership
/// `EXISTS` alone already excludes it under `One(env_a)`, regardless of
/// whether the tag/q predicate itself is scoped — which is exactly why this
/// bug shipped past a test written to cover this code (see
/// `.superpowers/sdd/s2-task-9-report.md`). This fixture is a genuine
/// `env_a` member (a plain, untagged occurrence) that *also* has a second,
/// `env_b`-only occurrence carrying a distinguishing tag and payload
/// string — mirroring a reviewer's live reproduction on the dev app, where a
/// staging-scoped issue's production-only `extra.prod_secret` was
/// extractable character-by-character through `q`.
#[tokio::test]
async fn list_issues_tag_and_q_do_not_leak_across_environments() {
    use sauron_db::filter::{Op, ParsedFilter};

    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let now = Utc::now();
    let issue_id = sauron_db::repo::upsert_issue(
        &mut conn,
        sauron_db::models::NewIssue {
            app_id: ids.app_id,
            fingerprint: "cross-env-leak-fingerprint",
            type_: "Error",
            title: "cross-env leak test issue",
            culprit: "harness::cross_env_leak",
            level: "error",
            first_seen: now,
            last_seen: now,
            times_seen: 1,
        },
    )
    .await
    .expect("create cross-env-leak issue");

    // Genuine env_a occurrence — plain, no distinguishing tag/payload. This
    // alone is what makes the issue a real env_a member, independent of the
    // env_b occurrence below.
    sauron_db::repo::insert_error_event(
        &mut conn,
        NewErrorEvent {
            id: Uuid::new_v4(),
            app_id: ids.app_id,
            environment_id: Some(ids.env_a),
            issue_id,
            fingerprint: "cross-env-leak-fingerprint".into(),
            level: "error".into(),
            message: "plain env_a occurrence".into(),
            exception_type: "HarnessError".into(),
            exception_value: "seeded".into(),
            stacktrace: json!([]),
            breadcrumbs: json!([]),
            context: json!({}),
            tags: json!({}),
            release: None,
            distinct_id: Some("cross-env-leak-user-a".into()),
            event_user: None,
            sdk: None,
            ip_address: None,
            occurred_at: now,
            session_id: None,
            device_key: None,
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
        },
    )
    .await
    .expect("insert plain env_a occurrence");

    // env_b-only occurrence carrying the distinguishing tag and payload
    // string — the "secret" a cross-environment match must not surface.
    sauron_db::repo::insert_error_event(
        &mut conn,
        NewErrorEvent {
            id: Uuid::new_v4(),
            app_id: ids.app_id,
            environment_id: Some(ids.env_b),
            issue_id,
            fingerprint: "cross-env-leak-fingerprint".into(),
            level: "error".into(),
            message: "env_b occurrence carrying the secret".into(),
            exception_type: "HarnessError".into(),
            exception_value: "seeded".into(),
            stacktrace: json!([]),
            breadcrumbs: json!([]),
            context: json!({}),
            tags: json!({"release": "canary"}),
            release: None,
            distinct_id: Some("cross-env-leak-user-b".into()),
            event_user: None,
            sdk: None,
            ip_address: None,
            occurred_at: now,
            session_id: None,
            device_key: None,
            screen: None,
            // Stamped with an env_b-only workflow, for the `workflow:eq` leg
            // below — the `workflow` filter's `EXISTS` has the identical
            // shape to the `tag`/`q` ones this test was built for, so it must
            // be held to the identical standard on the same fixture.
            workflow_id: Some("cross-env-leak-workflow-id".into()),
            workflow_name: Some("prod-only-checkout".into()),
            stacktrace_symbolicated: None,
            symbolication_status: "not_applicable".into(),
            debug_meta: None,
            contexts: json!({}),
            extra: json!({"prod_secret": "ACME-INTERNAL-42"}),
            handled: Some(true),
            title: None,
            culprit: None,
            stacktrace_sha256: None,
        },
    )
    .await
    .expect("insert env_b-only occurrence carrying the secret tag/payload/workflow");

    let scope_a = ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a));
    let scope_b = ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b));

    // Sanity: the issue really is a genuine env_a member (no filter/q).
    let plain_a =
        sauron_db::repo::list_issues(&mut conn, scope_a.clone(), &[], None, far_past(), 50, 0)
            .await
            .unwrap();
    assert!(
        plain_a.iter().any(|i| i.id == issue_id),
        "issue must be a genuine env_a member via its plain occurrence"
    );

    // tag:eq — the tag lives only in env_b; must not match under env_a even
    // though the issue is a genuine env_a member.
    let by_tag_a = sauron_db::repo::list_issues(
        &mut conn,
        scope_a.clone(),
        &[ParsedFilter {
            field: "tag",
            op: Op::Eq,
            value: "release=canary".to_string(),
        }],
        None,
        far_past(),
        50,
        0,
    )
    .await
    .unwrap();
    assert!(
        by_tag_a.iter().all(|i| i.id != issue_id),
        "tag:release=canary lives only in env_b — must not match under One(env_a)"
    );

    // Same tag under env_b, where it actually lives, must still match, with
    // the correct env_b-derived count.
    let by_tag_b = sauron_db::repo::list_issues(
        &mut conn,
        scope_b.clone(),
        &[ParsedFilter {
            field: "tag",
            op: Op::Eq,
            value: "release=canary".to_string(),
        }],
        None,
        far_past(),
        50,
        0,
    )
    .await
    .unwrap();
    let tag_b_hit = by_tag_b
        .iter()
        .find(|i| i.id == issue_id)
        .expect("tag:release=canary must match under its real environment, One(env_b)");
    assert_eq!(tag_b_hit.times_seen, 1, "must be env_b's own derived count");

    // tag:contains — the same predicate's other bind shape.
    let by_tag_contains_a = sauron_db::repo::list_issues(
        &mut conn,
        scope_a.clone(),
        &[ParsedFilter {
            field: "tag",
            op: Op::Contains,
            value: "release=can".to_string(),
        }],
        None,
        far_past(),
        50,
        0,
    )
    .await
    .unwrap();
    assert!(
        by_tag_contains_a.iter().all(|i| i.id != issue_id),
        "tag:contains release=can must not match under One(env_a) either"
    );

    // workflow:eq — Task 5's filter, whose `EXISTS` fragment has the same
    // shape (and therefore the same failure mode) as the `tag` ones above.
    // The workflow stamp lives only on the env_b occurrence, so it must not
    // match under env_a even though the issue is a genuine env_a member.
    //
    // This leg is what exercises the raw-SQL (`One`/`Subset`) branch of the
    // `workflow` filter at all: an app-wide caller resolves to `EnvFilter::All`
    // and only ever runs the diesel branch, leaving the `${next_bind}`
    // bookkeeping and the `we` env fragment — precisely the parts that can
    // silently return another environment's data — covered by nothing.
    let by_workflow_a = sauron_db::repo::list_issues(
        &mut conn,
        scope_a.clone(),
        &[ParsedFilter {
            field: "workflow",
            op: Op::Eq,
            value: "prod-only-checkout".to_string(),
        }],
        None,
        far_past(),
        50,
        0,
    )
    .await
    .unwrap();
    assert!(
        by_workflow_a.iter().all(|i| i.id != issue_id),
        "workflow:eq prod-only-checkout is stamped only on the env_b occurrence — \
         must not match under One(env_a)"
    );

    // Same workflow under env_b, where it actually lives, must still match,
    // with the correct env_b-derived count — the positive leg, without which
    // an `EXISTS` that never matches anything would pass the assertion above
    // for the wrong reason.
    let by_workflow_b = sauron_db::repo::list_issues(
        &mut conn,
        scope_b.clone(),
        &[ParsedFilter {
            field: "workflow",
            op: Op::Eq,
            value: "prod-only-checkout".to_string(),
        }],
        None,
        far_past(),
        50,
        0,
    )
    .await
    .unwrap();
    let workflow_b_hit = by_workflow_b
        .iter()
        .find(|i| i.id == issue_id)
        .expect("workflow:eq must match under its real environment, One(env_b)");
    assert_eq!(
        workflow_b_hit.times_seen, 1,
        "must be env_b's own derived count"
    );

    // workflow:contains — the same predicate's other bind shape (a
    // `like_contains` pattern rather than an exact value), so a bind-index
    // shift that only affects the ILIKE arm cannot hide here.
    let by_workflow_contains_a = sauron_db::repo::list_issues(
        &mut conn,
        scope_a.clone(),
        &[ParsedFilter {
            field: "workflow",
            op: Op::Contains,
            value: "prod-only".to_string(),
        }],
        None,
        far_past(),
        50,
        0,
    )
    .await
    .unwrap();
    assert!(
        by_workflow_contains_a.iter().all(|i| i.id != issue_id),
        "workflow:contains prod-only must not match under One(env_a) either"
    );

    // Free-text q, the character-extendable oracle: a substring of the
    // env_b-only `extra.prod_secret` must not match under env_a...
    let by_q_a = sauron_db::repo::list_issues(
        &mut conn,
        scope_a,
        &[],
        Some("ACME-INTERNAL-4"),
        far_past(),
        50,
        0,
    )
    .await
    .unwrap();
    assert!(
        by_q_a.iter().all(|i| i.id != issue_id),
        "q=ACME-INTERNAL-4 matches a payload string that exists only in env_b — \
         must not match, and must not leak, under One(env_a)"
    );

    // ...but must match under env_b, where it really lives.
    let by_q_b = sauron_db::repo::list_issues(
        &mut conn,
        scope_b,
        &[],
        Some("ACME-INTERNAL-4"),
        far_past(),
        50,
        0,
    )
    .await
    .unwrap();
    assert!(
        by_q_b.iter().any(|i| i.id == issue_id),
        "q=ACME-INTERNAL-4 must match under One(env_b), where the payload actually lives"
    );

    drop(conn);
    db.cleanup().await;
}

/// Regression test for Task 10's "Critical 2": neither `list_issues`' nor
/// `top_issues`' paging subquery carried an environment membership
/// predicate, so `LIMIT`/`OFFSET` (and, for `top_issues`, the ranking
/// itself) operated over the issue's *app-wide* `last_seen`/`times_seen`
/// before membership was known — an issue confined to a *different*
/// environment could still consume a page slot (or a top-N rank) ahead of a
/// genuine member, producing non-monotonic pages and even an empty first
/// page. Reproduced live on the dev app: `list_issues(One(demo), limit 5,
/// offset 0)` returned 0 rows while `offset 5`/`offset 10` returned real
/// ones, and `top_issues(One(demo), since=30d)` returned 0 rows where `All`
/// returned 5.
///
/// This fixture mints three `env_b`-only "noise" issues with a `last_seen`
/// strictly more recent than every other issue in the app (including the
/// seed's own `issue_id`/`issue_env_b_only`, created moments earlier) — so
/// under the pre-fix behaviour, a `LIMIT` of 3 issues under `One(env_a)`
/// would page in exactly these three (ranked purely by app-wide
/// `last_seen`), none of which are `env_a` members, yielding an empty page.
/// It also mints two genuine `env_a` members, `r1`/`r2`, with a
/// *reversed* relationship between their app-wide and env_a-derived
/// `times_seen` (`r1`: app-wide 10, env_a-derived 1; `r2`: app-wide 1,
/// env_a-derived 5) — chosen specifically so that ranking by the app-wide
/// count (the pre-fix `top_issues` behaviour) and ranking by the derived
/// count (Task 10's "Important 3" fix) disagree on the order, not just on
/// the displayed numbers.
#[tokio::test]
async fn list_issues_and_top_issues_page_by_environment_membership_not_app_wide_ranking() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    // Three env_b-only "noise" issues, no env_a occurrence at all — not
    // members of env_a under any definition. `last_seen` is a fresh
    // `Utc::now()` read strictly after `seed_two_envs()` returned, so it is
    // guaranteed more recent than every issue that fixture created.
    let noise_now = Utc::now();
    let mut noise_ids = Vec::new();
    for n in 0..3 {
        let id = sauron_db::repo::upsert_issue(
            &mut conn,
            sauron_db::models::NewIssue {
                app_id: ids.app_id,
                fingerprint: &format!("c2-noise-{n}"),
                type_: "Error",
                title: "c2 noise issue (env_b only, never env_a)",
                culprit: "harness::c2_noise",
                level: "error",
                first_seen: noise_now,
                last_seen: noise_now,
                times_seen: 1,
            },
        )
        .await
        .expect("create noise issue");
        noise_ids.push(id);
    }

    // r1: genuine env_a member (1 occurrence, env_a-derived times_seen=1),
    // but app-wide times_seen artificially inflated to 10 via repeated
    // upserts, and last_seen held older than the noise issues throughout.
    let r1_last_seen = noise_now - Duration::hours(2);
    let r1 = sauron_db::repo::upsert_issue(
        &mut conn,
        sauron_db::models::NewIssue {
            app_id: ids.app_id,
            fingerprint: "c2-real-r1",
            type_: "Error",
            title: "c2 real issue r1 (low env_a count, inflated app-wide count)",
            culprit: "harness::c2_real",
            level: "error",
            first_seen: r1_last_seen,
            last_seen: r1_last_seen,
            times_seen: 1,
        },
    )
    .await
    .expect("create r1");
    for _ in 0..9 {
        sauron_db::repo::upsert_issue(
            &mut conn,
            sauron_db::models::NewIssue {
                app_id: ids.app_id,
                fingerprint: "c2-real-r1",
                type_: "Error",
                title: "c2 real issue r1 (low env_a count, inflated app-wide count)",
                culprit: "harness::c2_real",
                level: "error",
                first_seen: r1_last_seen,
                last_seen: r1_last_seen,
                times_seen: 1,
            },
        )
        .await
        .expect("inflate r1's app-wide times_seen");
    }
    sauron_db::repo::insert_error_event(
        &mut conn,
        NewErrorEvent {
            id: Uuid::new_v4(),
            app_id: ids.app_id,
            environment_id: Some(ids.env_a),
            issue_id: r1,
            fingerprint: "c2-real-r1".into(),
            level: "error".into(),
            message: "r1's one env_a occurrence".into(),
            exception_type: "HarnessError".into(),
            exception_value: "seeded".into(),
            stacktrace: json!([]),
            breadcrumbs: json!([]),
            context: json!({}),
            tags: json!({}),
            release: None,
            distinct_id: Some("c2-r1-user".into()),
            event_user: None,
            sdk: None,
            ip_address: None,
            occurred_at: r1_last_seen,
            session_id: None,
            device_key: None,
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
        },
    )
    .await
    .expect("insert r1's env_a occurrence");

    // r2: genuine env_a member with 5 occurrences (env_a-derived
    // times_seen=5), app-wide times_seen left at 1 (no extra upserts),
    // last_seen held older than r1 so the paging order among real members is
    // deterministic: issue_id (seed, recent) > r1 > r2.
    let r2_last_seen = noise_now - Duration::hours(3);
    let r2 = sauron_db::repo::upsert_issue(
        &mut conn,
        sauron_db::models::NewIssue {
            app_id: ids.app_id,
            fingerprint: "c2-real-r2",
            type_: "Error",
            title: "c2 real issue r2 (high env_a count, low app-wide count)",
            culprit: "harness::c2_real",
            level: "error",
            first_seen: r2_last_seen,
            last_seen: r2_last_seen,
            times_seen: 1,
        },
    )
    .await
    .expect("create r2");
    for n in 0..5 {
        sauron_db::repo::insert_error_event(
            &mut conn,
            NewErrorEvent {
                id: Uuid::new_v4(),
                app_id: ids.app_id,
                environment_id: Some(ids.env_a),
                issue_id: r2,
                fingerprint: "c2-real-r2".into(),
                level: "error".into(),
                message: format!("r2 env_a occurrence {n}"),
                exception_type: "HarnessError".into(),
                exception_value: "seeded".into(),
                stacktrace: json!([]),
                breadcrumbs: json!([]),
                context: json!({}),
                tags: json!({}),
                release: None,
                distinct_id: Some(format!("c2-r2-user-{n}")),
                event_user: None,
                sdk: None,
                ip_address: None,
                occurred_at: r2_last_seen,
                session_id: None,
                device_key: None,
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
            },
        )
        .await
        .expect("insert r2 env_a occurrence");
    }

    let scope_a = ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a));

    // The only genuine env_a members in this app are the seed's own
    // `issue_id`, plus `r1` and `r2` — never the three noise issues, never
    // `issue_env_b_only`.
    let real_members: std::collections::HashSet<Uuid> =
        [ids.issue_id, r1, r2].into_iter().collect();

    // A page of exactly 3, at offset 0, must be exactly the 3 real members —
    // not the 3 (more recent) noise issues, and not empty. Pre-fix, this
    // would return 0 rows: the paging subquery would pick the 3 noise
    // issues by app-wide `last_seen` alone, and the LATERAL's `HAVING`
    // would then drop all three for having no env_a occurrence.
    let page =
        sauron_db::repo::list_issues(&mut conn, scope_a.clone(), &[], None, far_past(), 3, 0)
            .await
            .unwrap();
    let page_ids: std::collections::HashSet<Uuid> = page.iter().map(|i| i.id).collect();
    assert_eq!(
        page_ids, real_members,
        "limit=3 offset=0 under One(env_a) must be exactly the 3 real env_a members, \
         not the more-recent noise issues and not empty"
    );

    // Monotonic single-row paging across the 3 real members: three distinct
    // pages, no repeats, page 4 exhausted (empty, not spilling into noise).
    let mut seen_via_paging: std::collections::HashSet<Uuid> = std::collections::HashSet::new();
    for offset in 0..3i64 {
        let one = sauron_db::repo::list_issues(
            &mut conn,
            scope_a.clone(),
            &[],
            None,
            far_past(),
            1,
            offset,
        )
        .await
        .unwrap();
        assert_eq!(
            one.len(),
            1,
            "offset {offset} of 3 real members must return exactly one row"
        );
        assert!(
            real_members.contains(&one[0].id),
            "offset {offset} must be one of the real members, not noise"
        );
        assert!(
            seen_via_paging.insert(one[0].id),
            "offset {offset} returned a duplicate of an earlier page — paging is not monotonic"
        );
    }
    assert_eq!(seen_via_paging, real_members);
    let exhausted =
        sauron_db::repo::list_issues(&mut conn, scope_a.clone(), &[], None, far_past(), 1, 3)
            .await
            .unwrap();
    assert!(
        exhausted.is_empty(),
        "offset 3 is past all 3 real members and must come back empty, not noise"
    );

    // top_issues: must be non-empty (pre-fix: 0 rows, all slots taken by
    // app-wide-ranked non-members before the LATERAL even runs) and must
    // rank by the *derived* count, not the app-wide one — r2 (derived 5)
    // ahead of issue_id (derived 4) ahead of r1 (derived 1), the exact
    // reverse of ranking by app-wide times_seen (r1=10, issue_id=6, r2=1).
    let top = sauron_db::repo::top_issues(&mut conn, scope_a, far_past(), 10)
        .await
        .unwrap();
    assert!(
        !top.is_empty(),
        "top_issues under One(env_a) must not be empty"
    );
    let top_ids: std::collections::HashSet<Uuid> = top.iter().map(|i| i.id).collect();
    assert_eq!(
        top_ids, real_members,
        "top_issues under One(env_a) must be exactly the 3 real env_a members"
    );
    let ranked_ids: Vec<Uuid> = top.iter().map(|i| i.id).collect();
    assert_eq!(
        ranked_ids,
        vec![r2, ids.issue_id, r1],
        "top_issues must be ordered by the *derived* times_seen (5, 4, 1), \
         the reverse of ranking by the app-wide count (r1=10, issue_id=6, r2=1)"
    );
    let r2_row = top.iter().find(|i| i.id == r2).unwrap();
    assert_eq!(
        r2_row.times_seen, 5,
        "r2's displayed count must be its env_a-derived one"
    );
    let issue_id_row = top.iter().find(|i| i.id == ids.issue_id).unwrap();
    assert_eq!(issue_id_row.times_seen, 4);
    let r1_row = top.iter().find(|i| i.id == r1).unwrap();
    assert_eq!(
        r1_row.times_seen, 1,
        "r1's displayed count must be its env_a-derived one, not its inflated app-wide 10"
    );

    drop(conn);
    db.cleanup().await;
}

// ===========================================================================
// Task 15 (F8, `.superpowers/sdd/s2-final-review.md`): `top_issues`'
// `EnvFilter::All` and `EnvFilter::Unattributed` branches, neither of which
// any earlier test executed.
// ===========================================================================

/// `top_issues`' `EnvFilter::All` arm (repo.rs) is a *separate* boxed-diesel
/// branch ranking by the stored `issues.times_seen` column — every existing
/// `top_issues` test scopes with `One`/`Subset`, which take the raw-SQL path
/// instead, so nothing exercised `All`'s own branch before this. Mirrors
/// `list_issues_and_top_issues_page_by_environment_membership_not_app_wide_
/// ranking`'s `r1`/`r2` fixture shape (stored count inflated via repeated
/// `upsert_issue` calls, independent of the real `error_events` count) but
/// under `EnvFilter::All` rather than `One(env_a)`: `high_stored_low_real`'s
/// stored `times_seen` is bumped to 10 via 9 extra upserts while it has only
/// ONE real `error_events` row; `low_stored_high_real`'s stored `times_seen`
/// stays at 1 (a single upsert) while it accumulates 5 real occurrences. If
/// the `All` arm ever derived its ranking/count from `error_events` instead
/// of trusting the stored column — e.g. accidentally routed through the
/// raw-SQL branch the other three `EnvFilter` variants share — both the
/// displayed `times_seen` and the ranking order would flip.
#[tokio::test]
async fn top_issues_all_ranks_by_the_stored_times_seen_column() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let high_stored_low_real = sauron_db::repo::upsert_issue(
        &mut conn,
        sauron_db::models::NewIssue {
            app_id: ids.app_id,
            fingerprint: "f8-high-stored-low-real",
            type_: "Error",
            title: "f8 high stored, low real",
            culprit: "harness::f8",
            level: "error",
            first_seen: ids.pinned_now,
            last_seen: ids.pinned_now,
            times_seen: 1,
        },
    )
    .await
    .expect("create high_stored_low_real issue");
    for _ in 0..9 {
        sauron_db::repo::upsert_issue(
            &mut conn,
            sauron_db::models::NewIssue {
                app_id: ids.app_id,
                fingerprint: "f8-high-stored-low-real",
                type_: "Error",
                title: "f8 high stored, low real",
                culprit: "harness::f8",
                level: "error",
                first_seen: ids.pinned_now,
                last_seen: ids.pinned_now,
                times_seen: 1,
            },
        )
        .await
        .expect("inflate high_stored_low_real's stored times_seen to 10");
    }
    sauron_db::repo::insert_error_event(
        &mut conn,
        NewErrorEvent {
            id: Uuid::new_v4(),
            app_id: ids.app_id,
            environment_id: Some(ids.env_a),
            issue_id: high_stored_low_real,
            fingerprint: "f8-high-stored-low-real".into(),
            level: "error".into(),
            message: "high_stored_low_real's one real occurrence".into(),
            exception_type: "HarnessError".into(),
            exception_value: "seeded".into(),
            stacktrace: json!([]),
            breadcrumbs: json!([]),
            context: json!({}),
            tags: json!({}),
            release: None,
            distinct_id: Some("f8-high-stored-user".into()),
            event_user: None,
            sdk: None,
            ip_address: None,
            occurred_at: ids.pinned_now,
            session_id: None,
            device_key: None,
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
        },
    )
    .await
    .expect("insert high_stored_low_real's one error event");

    let low_stored_high_real = sauron_db::repo::upsert_issue(
        &mut conn,
        sauron_db::models::NewIssue {
            app_id: ids.app_id,
            fingerprint: "f8-low-stored-high-real",
            type_: "Error",
            title: "f8 low stored, high real",
            culprit: "harness::f8",
            level: "error",
            first_seen: ids.pinned_now,
            last_seen: ids.pinned_now,
            times_seen: 1,
        },
    )
    .await
    .expect("create low_stored_high_real issue");
    for n in 0..5 {
        sauron_db::repo::insert_error_event(
            &mut conn,
            NewErrorEvent {
                id: Uuid::new_v4(),
                app_id: ids.app_id,
                environment_id: Some(ids.env_a),
                issue_id: low_stored_high_real,
                fingerprint: "f8-low-stored-high-real".into(),
                level: "error".into(),
                message: format!("low_stored_high_real occurrence {n}"),
                exception_type: "HarnessError".into(),
                exception_value: "seeded".into(),
                stacktrace: json!([]),
                breadcrumbs: json!([]),
                context: json!({}),
                tags: json!({}),
                release: None,
                distinct_id: Some(format!("f8-low-stored-user-{n}")),
                event_user: None,
                sdk: None,
                ip_address: None,
                occurred_at: ids.pinned_now,
                session_id: None,
                device_key: None,
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
            },
        )
        .await
        .expect("insert low_stored_high_real occurrence");
    }

    let top = sauron_db::repo::top_issues(&mut conn, ReadScope::all(ids.app_id), far_past(), 50)
        .await
        .unwrap();

    let high_row = top
        .iter()
        .find(|i| i.id == high_stored_low_real)
        .expect("high_stored_low_real appears under All");
    assert_eq!(
        high_row.times_seen, 10,
        "All must show the stored issues.times_seen (10), not the derived real count (1)"
    );
    let low_row = top
        .iter()
        .find(|i| i.id == low_stored_high_real)
        .expect("low_stored_high_real appears under All");
    assert_eq!(
        low_row.times_seen, 1,
        "All must show the stored issues.times_seen (1), not the derived real count (5)"
    );

    // The ranking order itself: high_stored_low_real (stored 10) must rank
    // strictly ahead of low_stored_high_real (stored 1) under All. If the All
    // arm ever derived its ranking from error_events instead, the order would
    // be exactly reversed (1 real occurrence vs. 5).
    let ranked: Vec<Uuid> = top
        .iter()
        .map(|i| i.id)
        .filter(|id| *id == high_stored_low_real || *id == low_stored_high_real)
        .collect();
    assert_eq!(
        ranked,
        vec![high_stored_low_real, low_stored_high_real],
        "top_issues under All must rank by the stored times_seen column (10 ahead of 1), \
         the reverse of ranking by real error_events count (1 ahead of 5)"
    );

    drop(conn);
    db.cleanup().await;
}

/// `top_issues`' `EnvFilter::Unattributed` arm shares the raw-SQL path with
/// `One`/`Subset`, but is the one variant of the three that binds no `$4`
/// parameter at all — `sql_fragment_for` emits a literal `IS NULL` for
/// `Unattributed` (see `EnvFilter::sql_fragment_for`'s own doc comment), so a
/// bind-index regression on this path would surface only as a Postgres
/// parameter-count error at runtime, and no earlier test ever executed this
/// branch for `top_issues`. `q_unattributed_only` has 3 real `error_events`
/// rows, all unattributed, and none anywhere else; the seed's own `issue_id`
/// has exactly 1 unattributed row (out of 6 total across all three buckets —
/// see `SeedIds`' doc comment). Ranking by the Unattributed-derived count (3
/// vs. 1) is the exact reverse of ranking by either the stored `times_seen`
/// column (`issue_id`=6, `q_unattributed_only`=1) or the real
/// cross-environment total (`issue_id`=6, `q_unattributed_only`=3) — so this
/// also proves the branch derives its count from the unattributed rows
/// alone, not from either of those.
#[tokio::test]
async fn top_issues_unattributed_ranks_by_the_unattributed_derived_count() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let q_unattributed_only = sauron_db::repo::upsert_issue(
        &mut conn,
        sauron_db::models::NewIssue {
            app_id: ids.app_id,
            fingerprint: "f8-unattributed-only",
            type_: "Error",
            title: "f8 unattributed-only issue",
            culprit: "harness::f8",
            level: "error",
            first_seen: ids.pinned_now,
            last_seen: ids.pinned_now,
            times_seen: 1,
        },
    )
    .await
    .expect("create q_unattributed_only issue");
    for n in 0..3 {
        sauron_db::repo::insert_error_event(
            &mut conn,
            NewErrorEvent {
                id: Uuid::new_v4(),
                app_id: ids.app_id,
                environment_id: None,
                issue_id: q_unattributed_only,
                fingerprint: "f8-unattributed-only".into(),
                level: "error".into(),
                message: format!("q_unattributed_only occurrence {n}"),
                exception_type: "HarnessError".into(),
                exception_value: "seeded".into(),
                stacktrace: json!([]),
                breadcrumbs: json!([]),
                context: json!({}),
                tags: json!({}),
                release: None,
                distinct_id: Some(format!("f8-unattributed-user-{n}")),
                event_user: None,
                sdk: None,
                ip_address: None,
                occurred_at: ids.pinned_now,
                session_id: None,
                device_key: None,
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
            },
        )
        .await
        .expect("insert q_unattributed_only occurrence");
    }

    let top = sauron_db::repo::top_issues(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::Unattributed),
        far_past(),
        50,
    )
    .await
    .unwrap();

    let q_row = top
        .iter()
        .find(|i| i.id == q_unattributed_only)
        .expect("q_unattributed_only appears under Unattributed");
    assert_eq!(q_row.times_seen, 3);
    let issue_id_row = top
        .iter()
        .find(|i| i.id == ids.issue_id)
        .expect("issue_id's one unattributed occurrence appears under Unattributed");
    assert_eq!(issue_id_row.times_seen, 1);
    assert!(
        top.iter().all(|i| i.id != ids.issue_env_b_only),
        "issue_env_b_only has no unattributed occurrence at all"
    );

    let ranked: Vec<Uuid> = top
        .iter()
        .map(|i| i.id)
        .filter(|id| *id == q_unattributed_only || *id == ids.issue_id)
        .collect();
    assert_eq!(
        ranked,
        vec![q_unattributed_only, ids.issue_id],
        "top_issues under Unattributed must rank by the unattributed-derived count \
         (3 ahead of 1), the reverse of both the stored times_seen (6 ahead of 1) and \
         the real cross-environment total (6 ahead of 3)"
    );

    drop(conn);
    db.cleanup().await;
}

/// Slice 3, Task 2: `role_grants.scope_type` must accept `'env'`, making an
/// environment a fourth grantable scope level alongside org/project/app.
/// Before migration 2026-07-29-000029 lands, this insert raises `new row for
/// relation "role_grants" violates check constraint
/// "role_grants_scope_type_check"` — the CHECK inherited from
/// 2026-07-12-000002_projects_apps_rbac only allows `('org', 'project',
/// 'app')`. `ids.org_id`/`ids.owner_email` are `seed_two_envs`'s own org and a
/// real `users` row it creates alongside it (see `SeedIds`'s doc comment);
/// `'Viewer'` is one of the four system preset roles that migration
/// 2026-07-12-000002 seeds itself (`org_id IS NULL`), so it exists on every
/// freshly migrated database without this harness seeding it.
#[tokio::test]
async fn role_grants_accepts_the_env_scope_type() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let inserted = diesel::sql_query(
        "INSERT INTO role_grants (org_id, user_id, role_id, scope_type, scope_id)
         SELECT $1, u.id, r.id, 'env', $2
         FROM users u, roles r
         WHERE u.email = $3 AND r.name = 'Viewer'
         LIMIT 1",
    )
    .bind::<diesel::sql_types::Uuid, _>(ids.org_id)
    .bind::<diesel::sql_types::Uuid, _>(ids.env_a)
    .bind::<diesel::sql_types::Text, _>(ids.owner_email.clone())
    .execute(&mut conn)
    .await
    .expect("env-scoped grant must be accepted by the CHECK constraint");
    assert_eq!(inserted, 1);

    drop(conn);
    db.cleanup().await;
}

// ===========================================================================
// Task 4: `EnvFilter::Subset` threaded through every environment-scoped read
// ===========================================================================

/// `Subset([a, b])` must equal `One(a)` ∪ `One(b)` for counts, and must
/// EXCLUDE unattributed rows — `= ANY(array)` never matches NULL. If a
/// function's bind arithmetic is wrong for `Subset`, it either errors at
/// runtime (bind count mismatch) or silently returns `One`'s answer.
#[tokio::test]
async fn subset_equals_the_union_of_its_environments_and_excludes_unattributed() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let since = ids.pinned_now - Duration::days(30);

    let both = ReadScope::new(ids.app_id, EnvFilter::Subset(vec![ids.env_a, ids.env_b]));
    let only_a = ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a));
    let only_b = ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b));
    let unattributed = ReadScope::new(ids.app_id, EnvFilter::Unattributed);
    let all = ReadScope::all(ids.app_id);

    let t_both = sauron_db::repo::overview_totals(&mut conn, both, since)
        .await
        .unwrap();
    let t_a = sauron_db::repo::overview_totals(&mut conn, only_a, since)
        .await
        .unwrap();
    let t_b = sauron_db::repo::overview_totals(&mut conn, only_b, since)
        .await
        .unwrap();
    let t_un = sauron_db::repo::overview_totals(&mut conn, unattributed, since)
        .await
        .unwrap();
    let t_all = sauron_db::repo::overview_totals(&mut conn, all, since)
        .await
        .unwrap();

    assert_eq!(
        t_both.events,
        t_a.events + t_b.events,
        "Subset events must be the exact union of its two environments"
    );
    assert_eq!(t_both.errors, t_a.errors + t_b.errors);
    assert_eq!(t_both.sessions, t_a.sessions + t_b.sessions);

    assert!(
        t_un.events > 0,
        "seed must contain unattributed rows for this test to mean anything"
    );
    assert_eq!(
        t_all.events,
        t_both.events + t_un.events,
        "All = Subset(every env) + Unattributed; Subset must NOT include NULLs"
    );

    // A single-element Subset must agree exactly with One.
    let single = ReadScope::new(ids.app_id, EnvFilter::Subset(vec![ids.env_a]));
    let t_single = sauron_db::repo::overview_totals(&mut conn, single, since)
        .await
        .unwrap();
    assert_eq!(t_single.events, t_a.events);
    assert_eq!(t_single.errors, t_a.errors);

    drop(conn);
    db.cleanup().await;
}

/// Every raw-SQL read function must survive `Subset` without a bind mismatch.
/// A wrong bind index raises `bind message supplies N parameters, but prepared
/// statement requires M` at runtime — invisible to any unit test. Argument
/// lists below are each function's REAL signature in `repo.rs`, not the
/// brief's sketch (which invented arguments several of these functions don't
/// take, or omitted ones they do): `list_persons` has no `since` parameter;
/// `list_devices`/`list_sessions` take a trailing filter/search argument the
/// brief's snippet dropped.
#[tokio::test]
async fn every_scoped_read_accepts_subset_without_a_bind_mismatch() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let since = ids.pinned_now - Duration::days(30);
    let scope = ReadScope::new(ids.app_id, EnvFilter::Subset(vec![ids.env_a, ids.env_b]));

    sauron_db::repo::list_issues(&mut conn, scope.clone(), &[], None, since, 50, 0)
        .await
        .expect("list_issues under Subset");
    sauron_db::repo::issue_stats(&mut conn, scope.clone())
        .await
        .expect("issue_stats under Subset");
    sauron_db::repo::top_issues(&mut conn, scope.clone(), since, 10)
        .await
        .expect("top_issues under Subset");
    sauron_db::repo::list_persons(
        &mut conn,
        scope.clone(),
        None,
        50,
        0,
        common::default_person_sort(),
        TimeWindow::since(
            "last_seen",
            chrono::Utc::now() - chrono::Duration::days(3650),
        ),
    )
    .await
    .expect("list_persons under Subset");
    sauron_db::repo::list_devices(
        &mut conn,
        scope.clone(),
        TimeWindow::since("last_seen", since),
        50,
        0,
        device_sort(),
        None,
        None,
    )
    .await
    .expect("list_devices under Subset");
    // The bind-index hazard, exercised for real: under `Subset`,
    // `scope.env.consumes_bind()` is `true`, so `env_sql` binds `$6` and the
    // four group predicates must start at `$7`. `device_groups.rs`'s
    // `Some(key)` tests only ever run under `ReadScope::all` (`consumes_bind()
    // == false`, group binds start at `$6`) — this is the only place the
    // `$7` branch of `group_base` gets a real Postgres round trip. A wrong
    // base here raises "bind message supplies N parameters, but prepared
    // statement requires M" at runtime, not a compile error.
    sauron_db::repo::list_devices(
        &mut conn,
        scope.clone(),
        TimeWindow::since("last_seen", since),
        50,
        0,
        device_sort(),
        None,
        Some(sauron_db::repo::DeviceGroupKey {
            family: Some("iPhone"),
            model: None,
            os_name: Some("iOS"),
            os_version: None,
        }),
    )
    .await
    .expect("list_devices with a group filter under Subset");
    sauron_db::repo::list_device_groups(
        &mut conn,
        scope.clone(),
        TimeWindow::since("last_seen", since),
        50,
        0,
        group_sort(),
        None,
    )
    .await
    .expect("list_device_groups under Subset");
    sauron_db::repo::list_sessions(
        &mut conn,
        scope.clone(),
        since,
        50,
        0,
        common::default_session_sort(),
        None,
        None,
    )
    .await
    .expect("list_sessions under Subset");
    sauron_db::repo::session_stats(&mut conn, scope.clone(), since)
        .await
        .expect("session_stats under Subset");
    sauron_db::repo::user_stats(&mut conn, scope.clone(), since, Utc::now())
        .await
        .expect("user_stats under Subset");
    sauron_db::repo::active_user_series(&mut conn, scope.clone(), since)
        .await
        .expect("active_user_series under Subset");
    sauron_db::repo::session_duration_series(&mut conn, scope.clone(), since)
        .await
        .expect("session_duration_series under Subset");
    sauron_db::repo::session_duration_histogram(&mut conn, scope.clone(), since)
        .await
        .expect("session_duration_histogram under Subset");
    sauron_db::repo::journey_graph(&mut conn, scope.clone(), since, 20)
        .await
        .expect("journey_graph under Subset");

    drop(conn);
    db.cleanup().await;
}

/// The two `list_devices` group-filter × bind-consuming-variant cells the
/// Subset test above does not reach: `One` and `Unattributed`.
///
/// `group_base`'s two arms are `if scope.env.consumes_bind() { 7 } else { 6
/// }`. `consumes_bind()` groups `One`/`Subset` together and `All`/
/// `Unattributed` together, so it is tempting to conclude that testing one
/// representative of each pair (as the Subset test above does for the `true`
/// arm, and `device_groups.rs`'s `Some(key)` tests do for the `false` arm
/// under `All`) proves both members of each pair. That holds for
/// `group_base` itself, but NOT for the SQL text the group predicate is
/// spliced into:
///
/// - `device_membership_sql` (repo.rs) returns `""` — skips the membership
///   `EXISTS` entirely — **only** for `All`. `Unattributed` builds the full
///   three-leg `EXISTS` block, at `bind_index = 6`, same as the group binds.
/// - The `scoped_select`/`scoped_join` split (repo.rs) keys on
///   `matches!(scope.env, EnvFilter::All)`. `Unattributed` takes the *other*
///   branch — the one with both count LATERALs and
///   `device_last_distinct_id_join`, all built at `bind_index = 6`.
///
/// So `All × Some(key)` (the only `false`-arm case under test elsewhere)
/// never executes any of that SQL — under `All` none of it is emitted. The
/// composition of the group binds (`$6..$9`) alongside `Unattributed`'s full
/// membership/LATERAL text is safe today only because every `Unattributed`
/// fragment (`EnvFilter::sql_fragment_for`) is a literal ` AND alias.
/// environment_id IS NULL` that consumes no bind of its own — but nothing
/// enforces that invariant at compile time, and no test before this one
/// exercised it against real Postgres with a group filter active.
///
/// `One` gets its own coverage for a different reason: it shares
/// `consumes_bind() == true` with `Subset`, but binds a scalar `Uuid` (`$1
/// AND environment_id = $6`) where `Subset` binds an array (`= ANY($6)`) —
/// a different bind type at the same position. `bins/sauron-api/src/routes/
/// scope.rs:88` maps a request's `environment_id=<uuid>` query param to
/// `One` and `environment_id=none`/absent-with-no-app-wide-grant to
/// `Unattributed`, so once Task 3 exposes the drill-down over HTTP both
/// become live, caller-reachable paths carrying `Some(key)`.
#[tokio::test]
async fn list_devices_group_filter_binds_correctly_under_one_and_unattributed() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let since = ids.pinned_now - Duration::days(30);

    // `One`: consumes_bind() == true, so group_base must be 7 — same value
    // Subset needed, reached via a different bind type (scalar Uuid, not an
    // array). A `None` field (`model`) exercises `IS NOT DISTINCT FROM`
    // under a variant that also reserves a bind of its own.
    sauron_db::repo::list_devices(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        TimeWindow::since("last_seen", since),
        50,
        0,
        device_sort(),
        None,
        Some(sauron_db::repo::DeviceGroupKey {
            family: Some("iPhone"),
            model: None,
            os_name: Some("iOS"),
            os_version: None,
        }),
    )
    .await
    .expect("list_devices with a group filter under One");

    // `Unattributed`: consumes_bind() == false, so group_base is 6, same as
    // `All` — but unlike `All`, `Unattributed` still emits the full
    // membership EXISTS block and both count/last-distinct-id LATERALs, all
    // built at bind_index = 6. `DeviceGroupKey::default()` (every field
    // `None`) both exercises `IS NOT DISTINCT FROM` on all four columns and
    // matches this crate's other all-NULL-group coverage
    // (`device_groups.rs::group_filter_matches_the_all_null_group`, under
    // `All` rather than `Unattributed`).
    sauron_db::repo::list_devices(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::Unattributed),
        TimeWindow::since("last_seen", since),
        50,
        0,
        device_sort(),
        None,
        Some(sauron_db::repo::DeviceGroupKey::default()),
    )
    .await
    .expect("list_devices with a group filter under Unattributed");

    drop(conn);
    db.cleanup().await;
}

// ===========================================================================
// Task 5: `env_ids_for_app` — every environment of an app, retired or not
// ===========================================================================

/// `env_ids_for_app` must return both seeded environments, and must keep
/// returning a retired one — unlike `list_environments`, which excludes
/// retired rows because they must not be *selectable*. `resolve_env_filter`
/// (Slice 3 Task 5, `sauron-auth`) relies on this: a caller's readable
/// `Subset` must be able to contain a retired environment id, or an app's
/// history would narrow the moment an environment was retired.
#[tokio::test]
async fn env_ids_for_app_includes_a_retired_environment() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    // Both live environments are present before either is touched.
    let mut before = sauron_db::repo::env_ids_for_app(&mut conn, ids.app_id)
        .await
        .expect("env_ids_for_app before retiring");
    before.sort();
    let mut want = vec![ids.env_a, ids.env_b];
    want.sort();
    assert_eq!(before, want);

    // `env_b` is not the app's default (seed_two_envs makes env_a the
    // default), so it is retireable without first promoting another.
    sauron_db::repo::retire_app_environment(&mut conn, ids.env_b)
        .await
        .expect("retire env_b");

    let mut after = sauron_db::repo::env_ids_for_app(&mut conn, ids.app_id)
        .await
        .expect("env_ids_for_app after retiring");
    after.sort();
    assert_eq!(
        after, want,
        "a retired environment must still be included — its history stays readable"
    );

    // `list_app_environments(include_retired = false)` is the function this is
    // deliberately NOT — it must now exclude env_b, or the two functions would
    // be indistinguishable and the retired-inclusion behavior above would be
    // accidental rather than intentional.
    let selectable = sauron_db::repo::list_app_environments(&mut conn, ids.app_id, false)
        .await
        .expect("list_app_environments live-only");
    assert!(
        selectable.iter().all(|e| e.enrollment.id != ids.env_b),
        "list_app_environments must exclude the retired environment"
    );

    drop(conn);
    db.cleanup().await;
}

// ===========================================================================
// Task 5 review fix round 1: `authorize_env_read` — DB-backed coverage for
// the wrapper itself, not just the pure `resolve_env_filter` core
// ===========================================================================
//
// `resolve_env_filter` is exhaustively unit-tested (no database needed —
// that is the whole point of splitting it out). Nothing before this section
// actually drove `authorize_env_read` end to end: its app-wide fast path
// (which deliberately *skips* the `env_ids_for_app` query), its
// `AuthError::NotFound` branch, and its argument wiring into
// `resolve_env_filter` had zero direct coverage. These seven tests close
// that gap against a real Postgres connection. `sauron-auth` has no
// `tests/` integration harness of its own (no `tests/` directory, no
// dev-dependency on anything that could seed a database), so — per this
// round's instructions — they live here instead, alongside the harness that
// already exists (`sauron-db/tests/common/mod.rs`'s `TestDb`/
// `seed_two_envs`). `sauron-db/Cargo.toml` gained a `[dev-dependencies]`
// section with `sauron-auth` to make `authorize_env_read` callable from this
// file; see that Cargo.toml's comment for why the resulting dev-dependency
// cycle (sauron-auth -> sauron-db normally, sauron-db -> sauron-auth in
// dev-dependencies only) is fine.

/// Insert one `role_grants` row at an arbitrary scope, for a caller shaped
/// however a test needs. `role_name` must be one of the four system presets
/// (`"Owner"`, `"Admin"`, `"Developer"`, `"Viewer"`) — seeded by migration
/// 2026-07-12-000002 with `org_id IS NULL`, so every freshly migrated
/// ephemeral database already has them; nothing here creates a role. Mirrors
/// `role_grants_accepts_the_env_scope_type`'s raw-SQL shape above, generalized
/// over scope and taking a resolved `user_id` instead of an email since
/// several of the tests below need more than one caller.
async fn grant_role(
    conn: &mut sauron_db::PgConn,
    org_id: Uuid,
    user_id: Uuid,
    role_name: &str,
    scope_type: &str,
    scope_id: Uuid,
) {
    let inserted = diesel::sql_query(
        "INSERT INTO role_grants (org_id, user_id, role_id, scope_type, scope_id)
         SELECT $1, $2, r.id, $3, $4
         FROM roles r
         WHERE r.name = $5
         LIMIT 1",
    )
    .bind::<SqlUuid, _>(org_id)
    .bind::<SqlUuid, _>(user_id)
    .bind::<Text, _>(scope_type)
    .bind::<SqlUuid, _>(scope_id)
    .bind::<Text, _>(role_name)
    .execute(conn)
    .await
    .expect("insert role grant");
    assert_eq!(
        inserted, 1,
        "role grant insert must affect exactly one row -- role_name must be a real preset"
    );
}

/// The app-wide fast path (`authorize_env_read`'s early return for an
/// `EnvFilter::All` request from a caller with app/project/org reach) must
/// answer `Ok(EnvFilter::All)` WITHOUT ever running the `env_ids_for_app`
/// lookup below it — that is its entire reason to exist ("today's callers
/// pay exactly today's cost", per its own doc comment).
///
/// Equality of the *result* alone cannot prove that: `resolve_env_filter`'s
/// own app-wide branch also answers the bare `EnvFilter::All` sentinel for an
/// `All` request (`if app_wide { match requested { EnvFilter::All =>
/// Ok(EnvFilter::All), ... } }` in rbac.rs) — it never consults
/// `app_env_ids` for that arm either. So a caller with genuine app-wide
/// reach gets `Ok(All)` whether the fast path fires or whether the code fell
/// through to the full `resolve_env_filter` call; output-equality alone
/// cannot tell those two cases apart.
///
/// What DOES tell them apart is denying the lookup a table to query: this
/// test renames `environments` out from under the connection before calling
/// `authorize_env_read`, and renames it back immediately after regardless of
/// outcome. If `env_ids_for_app`'s `SELECT ... FROM environments` ran at
/// all, it would hit `relation "environments" does not exist` and
/// `authorize_env_read` would come back `Err(AuthError::Internal)` instead
/// of `Ok`. Only a genuinely skipped lookup can return `Ok` here. The app
/// has two real environments (from `seed_two_envs`) the whole time this
/// runs, so this is not "the lookup would have found nothing anyway" — there
/// was something real to find, and the test proves it was never looked for.
#[tokio::test]
async fn authorize_env_read_fast_path_never_queries_environments() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let user = sauron_db::repo::find_user_by_email(&mut conn, &ids.owner_email)
        .await
        .expect("query owner")
        .expect("seed_two_envs must create the owner user");

    grant_role(&mut conn, ids.org_id, user.id, "Viewer", "app", ids.app_id).await;

    diesel::sql_query("ALTER TABLE environments RENAME TO environments_hidden_for_test")
        .execute(&mut conn)
        .await
        .expect("hide the environments table for the duration of the call");

    let result = authorize_env_read(
        &mut conn,
        user.id,
        ids.app_id,
        perm::ISSUE_READ,
        EnvFilter::All,
    )
    .await;

    diesel::sql_query("ALTER TABLE environments_hidden_for_test RENAME TO environments")
        .execute(&mut conn)
        .await
        .expect("restore the environments table");

    let scope = result.expect(
        "app-wide EnvFilter::All must succeed without ever touching `environments` -- an \
         Err here means the fast path fell through to env_ids_for_app, which just hit a \
         table that did not exist",
    );
    assert_eq!(scope.app_id, ids.app_id);
    assert_eq!(scope.env, EnvFilter::All);

    drop(conn);
    db.cleanup().await;
}

/// The narrowing path, end to end: a caller holding only an `env`-scoped
/// grant, asking for `EnvFilter::All`, gets back `Subset([their one
/// environment])` — not `All` (they have no app-wide reach) and not
/// `Forbidden` (they do have real reach, just a narrower one).
#[tokio::test]
async fn env_scoped_caller_asking_for_all_gets_subset_of_their_own_environment() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let user = sauron_db::repo::find_user_by_email(&mut conn, &ids.owner_email)
        .await
        .expect("query owner")
        .expect("seed_two_envs must create the owner user");
    grant_role(&mut conn, ids.org_id, user.id, "Viewer", "env", ids.env_a).await;

    let scope = authorize_env_read(
        &mut conn,
        user.id,
        ids.app_id,
        perm::ISSUE_READ,
        EnvFilter::All,
    )
    .await
    .expect("env-scoped caller has real reach and must not be refused");

    assert_eq!(scope.app_id, ids.app_id);
    assert_eq!(scope.env, EnvFilter::Subset(vec![ids.env_a]));

    drop(conn);
    db.cleanup().await;
}

/// An env-scoped caller asking for a *sibling* environment they hold no
/// grant on must be refused outright — `Forbidden`, not an empty result set.
/// A caller who could tell "zero rows" apart from "not allowed to ask" would
/// learn something about the sibling environment's existence they should
/// not.
#[tokio::test]
async fn env_scoped_caller_asking_for_sibling_environment_is_forbidden() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let user = sauron_db::repo::find_user_by_email(&mut conn, &ids.owner_email)
        .await
        .expect("query owner")
        .expect("seed_two_envs must create the owner user");
    grant_role(&mut conn, ids.org_id, user.id, "Viewer", "env", ids.env_a).await;

    let result = authorize_env_read(
        &mut conn,
        user.id,
        ids.app_id,
        perm::ISSUE_READ,
        EnvFilter::One(ids.env_b),
    )
    .await;

    assert!(
        matches!(result, Err(AuthError::Forbidden)),
        "expected Forbidden for a sibling environment, got {result:?}"
    );

    drop(conn);
    db.cleanup().await;
}

/// An env-scoped caller asking for the unattributed bucket
/// (`?environment_id=none`) must be refused — only app-wide reach may read
/// rows with no environment at all. `Forbidden`, not an empty result:
/// "matches nothing" and "you may not ask" are different answers, and only
/// the second is true here.
#[tokio::test]
async fn env_scoped_caller_asking_for_unattributed_is_forbidden() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let user = sauron_db::repo::find_user_by_email(&mut conn, &ids.owner_email)
        .await
        .expect("query owner")
        .expect("seed_two_envs must create the owner user");
    grant_role(&mut conn, ids.org_id, user.id, "Viewer", "env", ids.env_a).await;

    let result = authorize_env_read(
        &mut conn,
        user.id,
        ids.app_id,
        perm::ISSUE_READ,
        EnvFilter::Unattributed,
    )
    .await;

    assert!(
        matches!(result, Err(AuthError::Forbidden)),
        "expected Forbidden for Unattributed from an env-scoped caller, got {result:?}"
    );

    drop(conn);
    db.cleanup().await;
}

/// A caller who holds no `role_grants` row anywhere in the org gets
/// `Forbidden`, not a panic and not a silently empty scope.
/// `seed_two_envs`'s own owner user is exactly this shape by construction
/// (see `SeedIds`'s doc comment: "Not otherwise a member of the org") until
/// a test grants it something, so this test grants it nothing.
#[tokio::test]
async fn caller_with_no_grants_in_org_is_forbidden() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let user = sauron_db::repo::find_user_by_email(&mut conn, &ids.owner_email)
        .await
        .expect("query owner")
        .expect("seed_two_envs must create the owner user");

    let result = authorize_env_read(
        &mut conn,
        user.id,
        ids.app_id,
        perm::ISSUE_READ,
        EnvFilter::All,
    )
    .await;

    assert!(
        matches!(result, Err(AuthError::Forbidden)),
        "expected Forbidden for a caller with zero grants, got {result:?}"
    );

    drop(conn);
    db.cleanup().await;
}

/// A nonexistent `app_id` must fail the `app_ancestry` lookup with
/// `NotFound` — distinct from `Forbidden`, and reached before any grant is
/// even fetched. No grant is inserted for this test; if one were needed for
/// it to pass, that would itself mean `NotFound` was not really reached
/// first.
#[tokio::test]
async fn nonexistent_app_id_yields_not_found_not_forbidden() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let user = sauron_db::repo::find_user_by_email(&mut conn, &ids.owner_email)
        .await
        .expect("query owner")
        .expect("seed_two_envs must create the owner user");

    let result = authorize_env_read(
        &mut conn,
        user.id,
        Uuid::new_v4(),
        perm::ISSUE_READ,
        EnvFilter::All,
    )
    .await;

    assert!(
        matches!(result, Err(AuthError::NotFound)),
        "expected NotFound for a nonexistent app_id, got {result:?}"
    );

    drop(conn);
    db.cleanup().await;
}

/// `authorize_env_read` maps every `EnvDenied` variant to the single
/// `AuthError::Forbidden` on purpose (see its own doc comment: distinguishing
/// them over HTTP would let a caller enumerate environment ids). This drives
/// all four `EnvDenied`-producing shapes through the real wrapper and checks
/// each collapses to the same `Forbidden` — not merely that they fail, but
/// that they fail identically, so nothing downstream of this boundary can
/// tell them apart.
#[tokio::test]
async fn every_env_denied_variant_collapses_to_forbidden() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    // Three distinct callers, so each `EnvDenied` case is isolated from the
    // others' grants rather than accumulating on one user.
    let app_wide_user = sauron_db::repo::create_user(
        &mut conn,
        &format!("harness-appwide-{}@example.com", Uuid::new_v4().simple()),
        "harness-hash",
        "App Wide Caller",
    )
    .await
    .expect("create app-wide caller");
    grant_role(
        &mut conn,
        ids.org_id,
        app_wide_user.id,
        "Viewer",
        "app",
        ids.app_id,
    )
    .await;

    let env_scoped_user = sauron_db::repo::create_user(
        &mut conn,
        &format!("harness-envscoped-{}@example.com", Uuid::new_v4().simple()),
        "harness-hash",
        "Env Scoped Caller",
    )
    .await
    .expect("create env-scoped caller");
    grant_role(
        &mut conn,
        ids.org_id,
        env_scoped_user.id,
        "Viewer",
        "env",
        ids.env_a,
    )
    .await;

    let foreign_reach_user = sauron_db::repo::create_user(
        &mut conn,
        &format!("harness-foreign-{}@example.com", Uuid::new_v4().simple()),
        "harness-hash",
        "Foreign Reach Caller",
    )
    .await
    .expect("create foreign-reach caller");
    // A grant that is real (passes the CHECK constraint, joins to a real
    // role) but scoped to an environment id that belongs to no app at all --
    // `reach_for` collects it, but intersecting it with this app's own
    // `app_env_ids` leaves nothing. This is `EnvDenied::NoReach`.
    grant_role(
        &mut conn,
        ids.org_id,
        foreign_reach_user.id,
        "Viewer",
        "env",
        Uuid::new_v4(),
    )
    .await;

    // EnvDenied::EnvNotInApp -- app-wide reach, but the requested
    // environment id is not one of this app's.
    let not_in_app = authorize_env_read(
        &mut conn,
        app_wide_user.id,
        ids.app_id,
        perm::ISSUE_READ,
        EnvFilter::One(Uuid::new_v4()),
    )
    .await;
    assert!(
        matches!(not_in_app, Err(AuthError::Forbidden)),
        "EnvNotInApp must collapse to Forbidden, got {not_in_app:?}"
    );

    // EnvDenied::EnvNotGranted -- the environment is real and this app's,
    // but the caller holds no grant on it.
    let not_granted = authorize_env_read(
        &mut conn,
        env_scoped_user.id,
        ids.app_id,
        perm::ISSUE_READ,
        EnvFilter::One(ids.env_b),
    )
    .await;
    assert!(
        matches!(not_granted, Err(AuthError::Forbidden)),
        "EnvNotGranted must collapse to Forbidden, got {not_granted:?}"
    );

    // EnvDenied::UnattributedNeedsAppReach -- env-scoped reach cannot read
    // the unattributed bucket.
    let unattributed = authorize_env_read(
        &mut conn,
        env_scoped_user.id,
        ids.app_id,
        perm::ISSUE_READ,
        EnvFilter::Unattributed,
    )
    .await;
    assert!(
        matches!(unattributed, Err(AuthError::Forbidden)),
        "UnattributedNeedsAppReach must collapse to Forbidden, got {unattributed:?}"
    );

    // EnvDenied::NoReach -- the caller's only grant contributes nothing to
    // this app at all.
    let no_reach = authorize_env_read(
        &mut conn,
        foreign_reach_user.id,
        ids.app_id,
        perm::ISSUE_READ,
        EnvFilter::All,
    )
    .await;
    assert!(
        matches!(no_reach, Err(AuthError::Forbidden)),
        "NoReach must collapse to Forbidden, got {no_reach:?}"
    );

    drop(conn);
    db.cleanup().await;
}

// ===========================================================================
// Task 8: error_events carries the issue strings
// ===========================================================================

/// Ingest must persist the same title/culprit it hands to `upsert_issue`. If
/// these are NULL, the per-environment derivation in Task 9 silently falls
/// back to the app-wide string for every row and the whole fix is inert.
///
/// Only two of the seed's seven `error_events` rows carry a title/culprit
/// (see `SeedIds`' doc comment on `issue_shared`) — the other five are left
/// `NULL` on purpose, to also exercise the pre-migration-30 row shape Task
/// 9's `COALESCE` fallback has to handle. This test only needs "at least
/// one" to prove ingest writes the columns at all; the exact two rows are
/// pinned down by `issue_shared_carries_different_title_culprit_per_
/// environment_in_the_seed` below.
#[tokio::test]
async fn ingested_error_events_carry_their_own_title_and_culprit() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    #[derive(QueryableByName)]
    struct Row {
        #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
        #[allow(dead_code)]
        title: Option<String>,
        #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
        #[allow(dead_code)]
        culprit: Option<String>,
    }

    let rows: Vec<Row> = diesel::sql_query(
        "SELECT title, culprit FROM error_events WHERE app_id = $1 AND title IS NOT NULL",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .load(&mut conn)
    .await
    .unwrap();

    assert!(
        !rows.is_empty(),
        "the seed must write per-occurrence titles, or Task 9 cannot be tested"
    );

    drop(conn);
    db.cleanup().await;
}

/// Locks down the exact shape `SeedIds`' doc comment on `issue_shared`
/// promises: two `error_events` rows for the same issue, one per
/// environment, with different `title`/`culprit` — and that `issues.title`/
/// `culprit` under `EnvFilter::All` (which reads the stored row directly,
/// set by the seed's one literal `upsert_issue` call, not a per-environment
/// derivation; see `get_issue`'s `All`-scope branch) land on the env_b
/// strings regardless of either row's `occurred_at`. A future seed edit that
/// drifts an offset or a string without updating the doc comment is caught
/// here, before it silently invalidates Task 9's own assertions (which
/// address this fixture by name via `issue_shared`).
#[tokio::test]
async fn issue_shared_carries_different_title_culprit_per_environment_in_the_seed() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let now = ids.pinned_now;

    #[derive(QueryableByName)]
    struct Row {
        #[diesel(sql_type = diesel::sql_types::Nullable<Text>)]
        title: Option<String>,
        #[diesel(sql_type = diesel::sql_types::Nullable<Text>)]
        culprit: Option<String>,
        #[diesel(sql_type = diesel::sql_types::Timestamptz)]
        occurred_at: chrono::DateTime<Utc>,
    }

    let a: Row = diesel::sql_query(
        "SELECT title, culprit, occurred_at FROM error_events \
         WHERE issue_id = $1 AND environment_id = $2 AND title IS NOT NULL",
    )
    .bind::<SqlUuid, _>(ids.issue_shared)
    .bind::<SqlUuid, _>(ids.env_a)
    .get_result(&mut conn)
    .await
    .expect("issue_shared's env_a occurrence carries a title");
    assert_eq!(a.title.as_deref(), Some("TypeError: staging cart is empty"));
    assert_eq!(a.culprit.as_deref(), Some("checkout (staging/cart.ts)"));
    assert_eq!(a.occurred_at, now + Duration::seconds(5));

    let b: Row = diesel::sql_query(
        "SELECT title, culprit, occurred_at FROM error_events \
         WHERE issue_id = $1 AND environment_id = $2 AND title IS NOT NULL",
    )
    .bind::<SqlUuid, _>(ids.issue_shared)
    .bind::<SqlUuid, _>(ids.env_b)
    .get_result(&mut conn)
    .await
    .expect("issue_shared's env_b occurrence carries a title");
    assert_eq!(b.title.as_deref(), Some("TypeError: prod cart is empty"));
    assert_eq!(b.culprit.as_deref(), Some("checkout (prod/cart.ts)"));
    assert_eq!(b.occurred_at, now - Duration::seconds(30));

    // Each environment's own newest occurrence is unambiguous WITHIN that
    // environment (that's what each row's own membership/derivation reads —
    // see `issue_title_culprit_and_level_are_derived_per_environment`), which
    // is all Task 9's per-environment derivation needs. `a.occurred_at` and
    // `b.occurred_at` are NOT required to order any particular way against
    // each other — they're independent environments — and in fact do not:
    // `a` (`pinned_now + 5s`, Task 9's retime — see the seed's own comment
    // on `a-er-1`) is later in wall-clock terms than `b` (`pinned_now -
    // 30s`), which has no bearing on `issues.title` below (a fixed literal,
    // not derived from either row's timestamp).
    assert_ne!(a.occurred_at, b.occurred_at);

    // `EnvFilter::All` reads `issues` directly rather than deriving from
    // `error_events` (see `get_issue`'s `All`-scope branch) — its
    // `title`/`culprit` are the seed's one literal `upsert_issue` call,
    // independent of either row's `occurred_at` above (see that call's own
    // comment for why: the seed never re-calls `upsert_issue` per-row the
    // way real ingest does).
    let issue_all =
        sauron_db::repo::get_issue(&mut conn, ReadScope::all(ids.app_id), ids.issue_shared)
            .await
            .unwrap()
            .expect("issue_shared exists");
    assert_eq!(issue_all.title, "TypeError: prod cart is empty");
    assert_eq!(issue_all.culprit, "checkout (prod/cart.ts)");

    drop(conn);
    db.cleanup().await;
}

// ===========================================================================
// Task 9: title/culprit/level are derived per environment
// ===========================================================================

/// `upsert_issue`'s `ON CONFLICT (app_id, fingerprint)` has no environment in
/// the key, so `title`/`culprit`/`level` are whatever the most recent
/// occurrence in ANY environment wrote. A caller scoped to staging must see
/// staging's own strings — not production's, sitting beside a correctly
/// staging-scoped `last_seen` that says the issue has been quiet in staging.
#[tokio::test]
async fn issue_title_culprit_and_level_are_derived_per_environment() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let all = sauron_db::repo::get_issue(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::All),
        ids.issue_shared,
    )
    .await
    .unwrap()
    .unwrap();
    let a = sauron_db::repo::get_issue(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        ids.issue_shared,
    )
    .await
    .unwrap()
    .unwrap();
    let b = sauron_db::repo::get_issue(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
        ids.issue_shared,
    )
    .await
    .unwrap()
    .unwrap();

    // The two environments must disagree — under the old code all three of
    // these were byte-identical (whichever environment's occurrence was
    // processed last by `upsert_issue`).
    assert_ne!(a.title, b.title, "each environment must show its own title");
    assert_ne!(
        a.culprit, b.culprit,
        "each environment must show its own culprit"
    );

    assert_eq!(a.title, "TypeError: staging cart is empty");
    assert_eq!(a.culprit, "checkout (staging/cart.ts)");
    assert_eq!(b.title, "TypeError: prod cart is empty");
    assert_eq!(b.culprit, "checkout (prod/cart.ts)");

    // `All` keeps reading the durable `issues` column — the fast-path
    // convention every fix in this series follows. env_b's occurrence is the
    // newer one, so the stored row carries env_b's string (see `SeedIds`'
    // doc comment on `issue_shared`).
    assert_eq!(all.title, b.title, "All must read the stored issues column");
    assert_eq!(
        all.culprit, b.culprit,
        "All must read the stored issues column"
    );

    // And the staging-scoped title sits beside a staging-scoped last_seen —
    // the whole point: before this fix, a correct per-environment
    // `last_seen` sat beside a wrong, other-environment's title/culprit.
    // `a.last_seen` is `agg.last_seen` (max `occurred_at` over ALL four
    // env_a occurrences, not just `a-er-1`) — `pinned_now + 5s`, `a-er-1`'s
    // own retimed offset, since it is the newest of the four (see
    // `SeedIds`' doc comment on `issue_shared`).
    assert_eq!(a.last_seen, ids.pinned_now + Duration::seconds(5));
    assert_eq!(b.last_seen, ids.pinned_now - Duration::seconds(30));

    drop(conn);
    db.cleanup().await;
}

/// `issue_stats` counts `FILTER (WHERE level = ...)` over a derived,
/// per-environment level as of this task — under the old code (a plain
/// membership `EXISTS` filtering `issues.level`, app-wide) an environment's
/// fatal/error/warning split reflected whichever environment sent the last
/// event, so the numbers on the Issues page header disagreed with the list
/// beneath them. This also exercises that the inner `JOIN LATERAL` replacing
/// the `EXISTS` preserves membership exactly: `issue_env_b_only` (confined to
/// `env_b` alone) must count under `One(env_b)` and not under `One(env_a)`,
/// and every bucket must still sum to `total` (no row double-counted or
/// silently dropped by the join).
#[tokio::test]
async fn issue_stats_level_breakdown_is_per_environment() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let a = sauron_db::repo::issue_stats(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
    )
    .await
    .unwrap();
    let b = sauron_db::repo::issue_stats(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
    )
    .await
    .unwrap();

    assert_ne!(
        (a.fatal, a.error, a.warning),
        (b.fatal, b.error, b.warning),
        "the level breakdown must differ between environments"
    );
    // env_a: only `issue_shared` is a member (1 issue, derived level=error).
    // env_b: `issue_shared` AND `issue_env_b_only` are both members (2
    // issues, both derived level=error) — see `SeedIds`' doc comment.
    assert_eq!(a.total, 1, "env_a has exactly one member issue");
    assert_eq!(b.total, 2, "env_b has exactly two member issues");
    assert_eq!(a.error, 1);
    assert_eq!(b.error, 2);
    // Each environment's breakdown must sum to the issues it can actually see.
    assert_eq!(a.fatal + a.error + a.warning + a.info, a.total);
    assert_eq!(b.fatal + b.error + b.warning + b.info, b.total);

    drop(conn);
    db.cleanup().await;
}

/// `list_issues` used to filter on the STORED `issues` columns inside its
/// paging subquery while returning DERIVED values in the outer select list,
/// so `?level=error` and the level actually shown on the row could disagree.
/// Filter and display must now agree.
///
/// The shared seed's own `issue_shared`/`issue_env_b_only` can't discriminate
/// this on their own — every seeded `error_events` row hardcodes
/// `level = "error"`, identical to both issues' stored `issues.level`, so a
/// `level:eq:error` filter would happen to match under the OLD (stored-column)
/// code too. So this test builds a bespoke issue, mirroring
/// `list_issues_filters_tag_and_free_text_compose_with_scope`'s pattern,
/// whose stored `issues.level` (`"warning"`, set once at creation)
/// deliberately disagrees with its one real `env_a` occurrence's own
/// `level` (`"error"`) — the exact shape "last occurrence processed in some
/// OTHER environment wins the app-wide column" produces in production.
#[tokio::test]
async fn list_issues_filters_agree_with_what_it_displays() {
    use sauron_db::filter::{Op, ParsedFilter};

    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let now = Utc::now();
    let issue_id = sauron_db::repo::upsert_issue(
        &mut conn,
        sauron_db::models::NewIssue {
            app_id: ids.app_id,
            fingerprint: "filter-display-agreement-fingerprint",
            type_: "Error",
            title: "filter/display agreement test issue",
            culprit: "harness::filter_display_agreement",
            // Stored, app-wide level — deliberately "warning", disagreeing
            // with the one real env_a occurrence's own level below. Stands
            // in for "some other environment's more-recent occurrence wrote
            // this app-wide column last."
            level: "warning",
            first_seen: now,
            last_seen: now,
            times_seen: 1,
        },
    )
    .await
    .expect("create filter-display-agreement issue");
    sauron_db::repo::insert_error_event(
        &mut conn,
        NewErrorEvent {
            id: Uuid::new_v4(),
            app_id: ids.app_id,
            environment_id: Some(ids.env_a),
            issue_id,
            fingerprint: "filter-display-agreement-fingerprint".into(),
            // The real, per-environment level — "error", disagreeing with
            // the issue's own stored "warning" above.
            level: "error".into(),
            message: "filter/display agreement occurrence".into(),
            exception_type: "HarnessError".into(),
            exception_value: "seeded".into(),
            stacktrace: json!([]),
            breadcrumbs: json!([]),
            context: json!({}),
            tags: json!({}),
            release: None,
            distinct_id: Some("filter-display-agreement-user".into()),
            event_user: None,
            sdk: None,
            ip_address: None,
            occurred_at: now,
            session_id: None,
            device_key: None,
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
        },
    )
    .await
    .expect("insert filter-display-agreement occurrence");

    let scope = ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a));

    // The brief's own required shape: filter level=error under One(env_a),
    // assert the seed produces a matching row and every returned row's
    // displayed level really is "error".
    let filters = [ParsedFilter {
        field: "level",
        op: Op::Eq,
        value: "error".to_string(),
    }];
    let rows =
        sauron_db::repo::list_issues(&mut conn, scope.clone(), &filters, None, far_past(), 100, 0)
            .await
            .unwrap();
    assert!(!rows.is_empty(), "the seed must produce a matching row");
    for r in &rows {
        assert_eq!(
            r.level, "error",
            "every returned row must actually carry the level that was filtered on"
        );
    }
    assert!(
        rows.iter().any(|r| r.id == issue_id),
        "level:eq:error must match on the DERIVED (env_a) level, not the stored \
         issues.level='warning' — under the old, stored-column-filtered code this issue \
         would have been excluded"
    );

    // The decisive, discriminating half: level:eq:warning matches the
    // issue's stored `issues.level`, but env_a has no occurrence whose own
    // level is "warning" — so under the NEW, derived-value filter this must
    // NOT match, even though it would have under the old code.
    let warning_filters = [ParsedFilter {
        field: "level",
        op: Op::Eq,
        value: "warning".to_string(),
    }];
    let warning_rows =
        sauron_db::repo::list_issues(&mut conn, scope, &warning_filters, None, far_past(), 100, 0)
            .await
            .unwrap();
    assert!(
        warning_rows.iter().all(|r| r.id != issue_id),
        "level:eq:warning must NOT match on the stored issues.level='warning' — env_a's \
         only real occurrence has level='error', and the filter must agree with what's \
         displayed, not with the app-wide column"
    );

    drop(conn);
    db.cleanup().await;
}

// ---------------------------------------------------------------------------
// Workflow grouping, Task 1: `workflows` table + stamped columns.
//
// Diesel's plain `#[derive(Queryable)]` deserializes a result row
// positionally: field N of a struct is bound to column N of whatever
// `SqlType` tuple the query produced. `cargo check`/`check_for_backend`
// verify that each field's Rust type is compatible with *some* column of
// that name in the table -- they do not verify that the struct's declared
// field order matches the table!'s declared column order. A transposition
// (e.g. swapping `events_count`/`errors_count`, or `started_at`/`ended_at`,
// or any two of the five `Option<String>` fields) compiles cleanly on both
// counts and would silently return garbage at runtime. Since `schema.rs`'s
// `workflows` block and `models.rs`'s `Workflow` struct were both hand-edited
// (no `diesel print-schema` in this repo -- see task-1-report.md), this test
// exists to catch exactly that mismatch rather than trust the two hand-edits
// agree.
// ---------------------------------------------------------------------------

/// Selects via `workflows::all_columns` -- the tuple of columns in
/// `schema.rs`'s *declared* order -- rather than `Workflow::as_select()`
/// (which selects by field name and would mask a pure ordering bug), then
/// deserializes into `Workflow` positionally. Every same-typed field is given
/// a distinct, recognisable value so a swap between any two of them flips a
/// concrete assertion rather than silently passing.
#[tokio::test]
async fn workflow_row_round_trips_in_declared_column_order() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;

    let suffix = Uuid::new_v4().simple().to_string();
    let org =
        sauron_db::repo::create_org(&mut conn, "workflow test org", &format!("wf-org-{suffix}"))
            .await
            .expect("create org");
    let project = sauron_db::repo::create_project(
        &mut conn,
        org.id,
        "workflow test project",
        &format!("wf-project-{suffix}"),
    )
    .await
    .expect("create project");
    let app = sauron_db::repo::create_app(
        &mut conn,
        project.id,
        "workflow test app",
        &format!("wf-app-{suffix}"),
        "web",
    )
    .await
    .expect("create app");
    let env_id = common::seed_env(
        &mut conn,
        project.id,
        app.id,
        "production",
        &format!("pk_wf_{suffix}"),
        true,
    )
    .await;

    // Pinned, zero-subsecond base (mirrors `seed_two_envs`'s own `now`) so
    // Postgres's microsecond `timestamptz` round-trips byte-for-byte against
    // whole-second `Duration` offsets -- no precision-loss false failure.
    let now = Utc::now()
        .date_naive()
        .and_hms_opt(12, 0, 0)
        .expect("12:00:00 is a valid time")
        .and_utc();
    // Three distinct timestamps: a swap between any two of `started_at`/
    // `ended_at`/`last_event_at` (all three `Timestamptz`) must fail at
    // least one assertion below.
    let started_at = now - Duration::minutes(10);
    let ended_at = now - Duration::minutes(5);
    let last_event_at = now - Duration::minutes(4);

    let client_workflow_id = format!("wf-client-{suffix}");
    let session_id = format!("wf-session-{suffix}");
    let distinct_id = format!("wf-distinct-{suffix}");
    let device_key = format!("wf-device-{suffix}");

    // INSERT via named `.eq()` pairs -- immune to ordering by construction
    // (each column is named explicitly), so this half of the test cannot
    // itself hide the bug; only the SELECT below can.
    diesel::insert_into(workflows::table)
        .values((
            workflows::app_id.eq(app.id),
            workflows::environment_id.eq(env_id),
            workflows::workflow_id.eq(client_workflow_id.clone()),
            workflows::name.eq("checkout"),
            workflows::session_id.eq(Some(session_id.clone())),
            workflows::distinct_id.eq(Some(distinct_id.clone())),
            workflows::device_key.eq(Some(device_key.clone())),
            workflows::release.eq(Some("1.2.3")),
            workflows::status.eq("cancelled"),
            workflows::cancel_reason.eq(Some("superseded")),
            workflows::started_at.eq(started_at),
            workflows::ended_at.eq(Some(ended_at)),
            workflows::last_event_at.eq(last_event_at),
            workflows::events_count.eq(7),
            workflows::errors_count.eq(3),
        ))
        .execute(&mut conn)
        .await
        .expect("insert workflow row");

    // SELECT via `all_columns` (schema.rs's declared order) + plain
    // `Queryable` positional decode -- the order-sensitive path.
    let row: Workflow = workflows::table
        .filter(workflows::app_id.eq(app.id))
        .select(workflows::all_columns)
        .first(&mut conn)
        .await
        .expect("select workflow row back via all_columns");

    assert_eq!(row.app_id, app.id, "app_id");
    assert_eq!(row.environment_id, env_id, "environment_id");
    assert_eq!(row.workflow_id, client_workflow_id, "workflow_id");
    assert_eq!(row.name, "checkout", "name");
    assert_eq!(
        row.session_id.as_deref(),
        Some(session_id.as_str()),
        "session_id"
    );
    assert_eq!(
        row.distinct_id.as_deref(),
        Some(distinct_id.as_str()),
        "distinct_id"
    );
    assert_eq!(
        row.device_key.as_deref(),
        Some(device_key.as_str()),
        "device_key"
    );
    assert_eq!(row.release.as_deref(), Some("1.2.3"), "release");
    assert_eq!(row.status, "cancelled", "status");
    assert_eq!(
        row.cancel_reason.as_deref(),
        Some("superseded"),
        "cancel_reason"
    );
    assert_eq!(row.started_at, started_at, "started_at");
    assert_eq!(row.ended_at, Some(ended_at), "ended_at");
    assert_eq!(row.last_event_at, last_event_at, "last_event_at");
    assert_eq!(row.events_count, 7, "events_count");
    assert_eq!(row.errors_count, 3, "errors_count");

    // Every one of the five Option<String> fields, and all three
    // timestamps, and the two counters, carries a value distinct from every
    // other field of the same type -- so the assertions above are not
    // vacuously satisfiable by a swap (e.g. events_count/errors_count both
    // being 5 would let a transposition pass unnoticed).
    assert_ne!(row.events_count, row.errors_count);
    assert!(row.started_at < row.ended_at.unwrap());
    assert!(row.ended_at.unwrap() < row.last_event_at);
    assert_ne!(row.session_id, row.distinct_id);
    assert_ne!(row.distinct_id, row.device_key);
    assert_ne!(row.device_key.as_deref(), row.release.as_deref());
    assert_ne!(row.release.as_deref(), row.cancel_reason.as_deref());

    // `id`/`created_at`/`updated_at` were left to their column DEFAULTs
    // (`gen_random_uuid()`/`now()`); assert only that they came back at all,
    // as evidence the row-boundary itself (the very first/last columns of
    // the table!) decoded rather than erroring.
    assert_ne!(row.id, Uuid::nil());
    assert!(row.created_at <= Utc::now());
    assert!(row.updated_at <= Utc::now());

    drop(conn);
    db.cleanup().await;
}

// ===========================================================================
// Ingest key resolution after environments moved to the project (000033)
// ===========================================================================

/// `find_env_by_public_key` is the single query every SDK event passes through:
/// it turns a presented key into the `(env, app, project, org)` tuple that the
/// rest of ingest attributes the event by. Moving environments to the project
/// re-tabled it, and nothing else in the test suite touches it — so the
/// property that makes the whole migration invisible to already-deployed SDKs
/// had no coverage at all before this test.
#[tokio::test]
async fn an_ingest_key_proves_both_its_app_and_its_environment() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;

    let suffix = Uuid::new_v4().simple().to_string();
    let org = sauron_db::repo::create_org(&mut conn, "key org", &format!("key-org-{suffix}"))
        .await
        .expect("create org");
    let project = sauron_db::repo::create_project(
        &mut conn,
        org.id,
        "key project",
        &format!("key-project-{suffix}"),
    )
    .await
    .expect("create project");

    // Two apps sharing ONE environment — the shape that only exists because the
    // catalogue is project-level, and the one that makes the credential's
    // placement load-bearing.
    let app_a = sauron_db::repo::create_app(
        &mut conn,
        project.id,
        "key app a",
        &format!("key-app-a-{suffix}"),
        "web",
    )
    .await
    .expect("create app a");
    let app_b = sauron_db::repo::create_app(
        &mut conn,
        project.id,
        "key app b",
        &format!("key-app-b-{suffix}"),
        "web",
    )
    .await
    .expect("create app b");

    let shared_env = sauron_db::repo::create_project_environment(&mut conn, project.id, "shared")
        .await
        .expect("create shared environment");

    let key_a = format!("pk_shared_a_{suffix}");
    let key_b = format!("pk_shared_b_{suffix}");
    let enrollment_a = sauron_db::repo::create_app_environments(
        &mut conn,
        &[sauron_db::models::NewAppEnvironment {
            app_id: app_a.id,
            environment_id: shared_env.id,
            public_key: &key_a,
            is_default: true,
        }],
    )
    .await
    .expect("enroll app a")
    .remove(0);
    let enrollment_b = sauron_db::repo::create_app_environments(
        &mut conn,
        &[sauron_db::models::NewAppEnvironment {
            app_id: app_b.id,
            environment_id: shared_env.id,
            public_key: &key_b,
            is_default: true,
        }],
    )
    .await
    .expect("enroll app b")
    .remove(0);

    // Each key resolves to ITS OWN app, even though both name the same
    // environment. This is the property the credential was put on the
    // enrollment to preserve: if the key hung off the catalogue entry, both of
    // these would resolve to the same (or an ambiguous) app, and one app's key
    // could write events attributed to its sibling.
    let ref_a = sauron_db::repo::find_env_by_public_key(&mut conn, &key_a)
        .await
        .expect("resolve key a")
        .expect("key a must resolve");
    assert_eq!(ref_a.app_id, app_a.id, "key a must name app a");
    assert_eq!(ref_a.env_id, enrollment_a.id, "env_id is the enrollment id");
    assert_eq!(ref_a.project_id, project.id);
    assert_eq!(ref_a.org_id, org.id);

    let ref_b = sauron_db::repo::find_env_by_public_key(&mut conn, &key_b)
        .await
        .expect("resolve key b")
        .expect("key b must resolve");
    assert_eq!(ref_b.app_id, app_b.id, "key b must name app b");
    assert_eq!(ref_b.env_id, enrollment_b.id);
    assert_ne!(
        ref_a.env_id, ref_b.env_id,
        "two apps in one environment must resolve to two distinct enrollments"
    );

    // An unknown key resolves to nothing rather than erroring.
    assert!(
        sauron_db::repo::find_env_by_public_key(&mut conn, &format!("pk_nope_{suffix}"))
            .await
            .expect("resolve unknown key")
            .is_none(),
        "an unknown key must not resolve"
    );

    // A retired enrollment stops resolving, which is what makes retirement a
    // real revocation rather than a display-only flag — and is why the retire
    // paths invalidate the Redis DSN cache slot.
    sauron_db::repo::retire_app_environment(&mut conn, enrollment_b.id)
        .await
        .expect("retire app b's enrollment");
    assert!(
        sauron_db::repo::find_env_by_public_key(&mut conn, &key_b)
            .await
            .expect("resolve retired key")
            .is_none(),
        "a retired enrollment's key must stop resolving"
    );
    // ...and retiring one app's enrollment must not disturb its sibling's.
    assert!(
        sauron_db::repo::find_env_by_public_key(&mut conn, &key_a)
            .await
            .expect("resolve key a again")
            .is_some(),
        "retiring one app's enrollment must not revoke a sibling's key"
    );

    drop(conn);
    db.cleanup().await;
}

/// The 000038 backfill, run against the three row shapes it has to
/// discriminate. It reads the statement out of the migration file rather than
/// re-typing it, because a hand-copy would keep passing after the shipped SQL
/// changed — the same source-not-copy rule `http_env_scoping.rs` follows for
/// the route table.
///
/// The statement is re-run here rather than observed during `TestDb::setup()`
/// because migrations run against an empty database: at the moment 000038
/// executes for real there is nothing to back-fill, so its own run proves
/// nothing.
#[tokio::test]
async fn migration_000038_backfills_only_rows_with_traits_or_an_alias() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let with_traits = format!("backfill-traits-{}", Uuid::new_v4().simple());
    let with_alias = format!("backfill-alias-{}", Uuid::new_v4().simple());
    let bare = format!("backfill-bare-{}", Uuid::new_v4().simple());

    sauron_db::repo::upsert_event_user(
        &mut conn,
        ids.app_id,
        &with_traits,
        &json!({ "plan": "pro" }),
    )
    .await
    .expect("seed the traits-bearing row");
    sauron_db::repo::touch_event_user(&mut conn, ids.app_id, &with_alias)
        .await
        .expect("seed the alias-bearing row");
    // `repo::insert_identity` was removed in favour of
    // `identity_merge::claim_identity` (Task 2 of the guest-identity-merge
    // work); this seed only needs a plain row, so it inserts directly rather
    // than pull in claim/chain semantics this test isn't exercising.
    diesel::sql_query(
        "INSERT INTO identities (app_id, alias_id, distinct_id) VALUES ($1, $2, $3) \
         ON CONFLICT (app_id, alias_id) DO NOTHING",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .bind::<Text, _>("anon_abc")
    .bind::<Text, _>(&with_alias)
    .execute(&mut conn)
    .await
    .expect("seed the identities alias");
    sauron_db::repo::touch_event_user(&mut conn, ids.app_id, &bare)
        .await
        .expect("seed the bare row");

    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../migrations/2026-08-01-000038_event_users_identified/up.sql"
    );
    let src =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("could not read {path}: {e}"));
    let begin = src
        .find("-- BACKFILL-BEGIN")
        .unwrap_or_else(|| panic!("{path} lost its -- BACKFILL-BEGIN sentinel"));
    let end = src
        .find("-- BACKFILL-END")
        .unwrap_or_else(|| panic!("{path} lost its -- BACKFILL-END sentinel"));
    let backfill = &src[begin + "-- BACKFILL-BEGIN".len()..end];
    diesel::sql_query(backfill)
        .execute(&mut conn)
        .await
        .expect("run the 000038 backfill statement");

    #[derive(QueryableByName)]
    struct FlagRow {
        #[diesel(sql_type = Text)]
        distinct_id: String,
        #[diesel(sql_type = diesel::sql_types::Nullable<Text>)]
        identified_source: Option<String>,
    }
    let rows: Vec<FlagRow> = diesel::sql_query(
        "SELECT distinct_id, identified_source FROM event_users \
         WHERE app_id = $1 AND distinct_id = ANY($2) ORDER BY distinct_id",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .bind::<diesel::sql_types::Array<Text>, _>(vec![
        with_traits.clone(),
        with_alias.clone(),
        bare.clone(),
    ])
    .load(&mut conn)
    .await
    .expect("read back the three rows");

    let flagged: Vec<&str> = rows
        .iter()
        .filter(|r| r.identified_source.is_some())
        .map(|r| r.distinct_id.as_str())
        .collect();
    let mut expected = vec![with_alias.as_str(), with_traits.as_str()];
    expected.sort();
    let mut got = flagged.clone();
    got.sort();
    assert_eq!(
        got, expected,
        "exactly the traits-bearing and alias-bearing rows are backfilled; {bare} must stay a guest"
    );
    for r in &rows {
        if r.identified_source.is_some() {
            assert_eq!(
                r.identified_source.as_deref(),
                Some("backfill"),
                "the backfill must stamp its own source so a poisoned cohort stays repairable"
            );
        }
    }

    drop(conn);
    db.cleanup().await;
}

/// First-write-wins, and an unidentified touch can never clear the flag.
/// This is the property the whole guest/identified split rests on: a single
/// anonymous event arriving after an identify() must not move a person back
/// into the guest column, retroactively, for every day already reported.
#[tokio::test]
async fn identified_at_is_first_write_wins_and_never_cleared() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let did = format!("first-write-{}", Uuid::new_v4().simple());
    sauron_db::repo::touch_event_user(&mut conn, ids.app_id, &did)
        .await
        .expect("create the row");

    let n = sauron_db::repo::mark_event_user_identified(
        &mut conn,
        ids.app_id,
        &did,
        sauron_db::repo::IDENTIFIED_SOURCE_IDENTIFY,
    )
    .await
    .expect("first mark");
    assert_eq!(n, 1, "the first mark writes the flag");

    let n = sauron_db::repo::mark_event_user_identified(
        &mut conn,
        ids.app_id,
        &did,
        sauron_db::repo::IDENTIFIED_SOURCE_CONTEXT_USER,
    )
    .await
    .expect("second mark");
    assert_eq!(
        n, 0,
        "a later mark is a primary-key no-op, not an overwrite"
    );

    sauron_db::repo::touch_event_user(&mut conn, ids.app_id, &did)
        .await
        .expect("anonymous touch after identification");

    #[derive(QueryableByName)]
    struct SourceRow {
        #[diesel(sql_type = diesel::sql_types::Nullable<Text>)]
        identified_source: Option<String>,
    }
    let row: SourceRow = diesel::sql_query(
        "SELECT identified_source FROM event_users WHERE app_id = $1 AND distinct_id = $2",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .bind::<Text, _>(did.as_str())
    .get_result(&mut conn)
    .await
    .expect("read back");
    assert_eq!(
        row.identified_source.as_deref(),
        Some("identify"),
        "the original source survives both a losing mark and a later anonymous touch"
    );

    assert!(
        sauron_db::repo::probe_event_users_identified(&mut conn)
            .await
            .is_ok(),
        "the probe must succeed against a migrated schema"
    );

    drop(conn);
    db.cleanup().await;
}

/// Every identity `seed_two_envs()` produces is a GUEST unless a test asks
/// otherwise. Left as it was — `note_identity` calling `upsert_event_user`,
/// the identify() write shape — every seeded distinct_id would key as
/// `'u:'‖distinct_id`, merge across apps, drive `active_guest` to zero in
/// every test, and make the two anonymity tests below inexpressible against
/// the harness at all. The split would look correct and be untested.
#[tokio::test]
async fn the_harness_seeds_guests_not_identified_users() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let row: CountRow = diesel::sql_query(
        "SELECT count(*)::bigint AS n FROM event_users \
         WHERE app_id = $1 AND identified_at IS NOT NULL",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .get_result(&mut conn)
    .await
    .expect("count identified");
    assert_eq!(row.n, 0, "the ordinary event seed must not identify anyone");

    common::seed_identified_user(&mut conn, ids.app_id, &ids.shared_distinct_id).await;
    let row: CountRow = diesel::sql_query(
        "SELECT count(*)::bigint AS n FROM event_users \
         WHERE app_id = $1 AND identified_at IS NOT NULL",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .get_result(&mut conn)
    .await
    .expect("count identified after an explicit identify seed");
    assert_eq!(row.n, 1, "an explicit identify seed is the only way in");

    drop(conn);
    db.cleanup().await;
}

// ===========================================================================
// active_users_combined
// ===========================================================================

use sauron_db::repo::AppEnvScope;
use sauron_db::repo::TimeWindow;

/// A second app in the same project, with one environment enrollment.
/// Returns `(app_id, env_id, issue_id)`.
async fn second_app(
    conn: &mut sauron_db::PgConn,
    project_id: Uuid,
    label: &str,
) -> (Uuid, Uuid, Uuid) {
    let suffix = Uuid::new_v4().simple().to_string();
    let app =
        sauron_db::repo::create_app(conn, project_id, label, &format!("{label}-{suffix}"), "web")
            .await
            .expect("create second app");
    let env = sauron_db::repo::create_project_environment(conn, project_id, &format!("e-{suffix}"))
        .await
        .expect("create catalogue env");
    let enrollment = sauron_db::repo::create_app_environments(
        conn,
        &[sauron_db::models::NewAppEnvironment {
            app_id: app.id,
            environment_id: env.id,
            public_key: &format!("pk_{label}_{suffix}"),
            is_default: true,
        }],
    )
    .await
    .expect("enroll second app")
    .remove(0)
    .id;
    let issue = sauron_db::repo::upsert_issue(
        conn,
        sauron_db::models::NewIssue {
            app_id: app.id,
            fingerprint: "second-app-fingerprint",
            type_: "Error",
            title: "seeded",
            culprit: "seeded",
            level: "error",
            first_seen: far_past(),
            last_seen: far_past(),
            times_seen: 1,
        },
    )
    .await
    .expect("create second app issue");
    (app.id, enrollment, issue)
}

fn day_at(day: &str, hhmmss: &str) -> chrono::DateTime<Utc> {
    chrono::DateTime::parse_from_rfc3339(&format!("{day}T{hhmmss}Z"))
        .expect("valid RFC3339")
        .with_timezone(&Utc)
}

fn window(from_day: &str, to_day: &str) -> (chrono::DateTime<Utc>, chrono::DateTime<Utc>) {
    (day_at(from_day, "00:00:00"), day_at(to_day, "00:00:00"))
}

#[tokio::test]
async fn active_users_combined_merges_identified_users_across_apps() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let (app_b, env_b2, _issue_b) = second_app(&mut conn, ids.project_id, "merge-b").await;

    let did = format!("person-{}", Uuid::new_v4().simple());
    common::seed_identified_user(&mut conn, ids.app_id, &did).await;
    common::seed_identified_user(&mut conn, app_b, &did).await;
    common::seed_signal_event(
        &mut conn,
        ids.app_id,
        Some(ids.env_a),
        &did,
        day_at("2026-05-04", "09:00:00"),
    )
    .await;
    common::seed_signal_event(
        &mut conn,
        app_b,
        Some(env_b2),
        &did,
        day_at("2026-05-04", "21:00:00"),
    )
    .await;

    let (from, to) = window("2026-05-04", "2026-05-05");
    let rows = sauron_db::repo::active_users_combined(
        &mut conn,
        &[
            AppEnvScope {
                app_id: ids.app_id,
                env: EnvFilter::One(ids.env_a),
            },
            AppEnvScope {
                app_id: app_b,
                env: EnvFilter::One(env_b2),
            },
        ],
        from,
        to,
    )
    .await
    .expect("query");

    assert_eq!(rows.len(), 1, "one day in the window");
    assert_eq!(rows[0].active_total, 1, "one person, not two");
    assert_eq!(rows[0].active_identified, 1);
    assert_eq!(rows[0].active_guest, 0);

    drop(conn);
    db.cleanup().await;
}

/// The anti-test for the `'a:'‖app_id‖':'` prefix. Without `app_id` in the
/// guest key this silently returns 1, and the number would then change
/// depending on which OTHER apps happened to be selected.
#[tokio::test]
async fn active_users_combined_keeps_anonymous_ids_app_local() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let (app_b, env_b2, _issue_b) = second_app(&mut conn, ids.project_id, "guest-b").await;

    let did = format!("anon-{}", Uuid::new_v4().simple());
    common::seed_signal_event(
        &mut conn,
        ids.app_id,
        Some(ids.env_a),
        &did,
        day_at("2026-05-04", "09:00:00"),
    )
    .await;
    common::seed_signal_event(
        &mut conn,
        app_b,
        Some(env_b2),
        &did,
        day_at("2026-05-04", "09:00:00"),
    )
    .await;

    let (from, to) = window("2026-05-04", "2026-05-05");
    let rows = sauron_db::repo::active_users_combined(
        &mut conn,
        &[
            AppEnvScope {
                app_id: ids.app_id,
                env: EnvFilter::One(ids.env_a),
            },
            AppEnvScope {
                app_id: app_b,
                env: EnvFilter::One(env_b2),
            },
        ],
        from,
        to,
    )
    .await
    .expect("query");

    assert_eq!(
        rows[0].active_total, 2,
        "identical strings, two apps, no merge"
    );
    assert_eq!(rows[0].active_identified, 0);
    assert_eq!(rows[0].active_guest, 2);

    drop(conn);
    db.cleanup().await;
}

/// Under-merging is intentional and has to stay pinned: identified in one app
/// only means two keys, one in each bucket.
#[tokio::test]
async fn active_users_combined_does_not_merge_an_identified_id_with_an_unidentified_copy() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let (app_b, env_b2, _issue_b) = second_app(&mut conn, ids.project_id, "half-b").await;

    let did = format!("half-{}", Uuid::new_v4().simple());
    common::seed_identified_user(&mut conn, ids.app_id, &did).await;
    common::seed_signal_event(
        &mut conn,
        ids.app_id,
        Some(ids.env_a),
        &did,
        day_at("2026-05-04", "09:00:00"),
    )
    .await;
    common::seed_signal_event(
        &mut conn,
        app_b,
        Some(env_b2),
        &did,
        day_at("2026-05-04", "09:00:00"),
    )
    .await;

    let (from, to) = window("2026-05-04", "2026-05-05");
    let rows = sauron_db::repo::active_users_combined(
        &mut conn,
        &[
            AppEnvScope {
                app_id: ids.app_id,
                env: EnvFilter::One(ids.env_a),
            },
            AppEnvScope {
                app_id: app_b,
                env: EnvFilter::One(env_b2),
            },
        ],
        from,
        to,
    )
    .await
    .expect("query");

    assert_eq!(rows[0].active_total, 2);
    assert_eq!(rows[0].active_identified, 1);
    assert_eq!(rows[0].active_guest, 1);

    drop(conn);
    db.cleanup().await;
}

/// The one invariant the page renders as three tiles side by side. If the two
/// halves were ever computed as separate subqueries this would start drifting.
#[tokio::test]
async fn active_users_combined_split_always_sums_to_the_total() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let (app_b, env_b2, issue_b) = second_app(&mut conn, ids.project_id, "sum-b").await;

    for i in 0..5 {
        let did = format!("mix-{i}-{}", Uuid::new_v4().simple());
        if i % 2 == 0 {
            common::seed_identified_user(&mut conn, ids.app_id, &did).await;
            common::seed_identified_user(&mut conn, app_b, &did).await;
        }
        common::seed_signal_event(
            &mut conn,
            ids.app_id,
            Some(ids.env_a),
            &did,
            day_at("2026-05-04", "01:00:00"),
        )
        .await;
        common::seed_signal_error(
            &mut conn,
            app_b,
            Some(env_b2),
            issue_b,
            Some(&did),
            day_at("2026-05-05", "01:00:00"),
        )
        .await;
    }

    let (from, to) = window("2026-05-04", "2026-05-07");
    let rows = sauron_db::repo::active_users_combined(
        &mut conn,
        &[
            AppEnvScope {
                app_id: ids.app_id,
                env: EnvFilter::One(ids.env_a),
            },
            AppEnvScope {
                app_id: app_b,
                env: EnvFilter::One(env_b2),
            },
        ],
        from,
        to,
    )
    .await
    .expect("query");

    assert_eq!(rows.len(), 3, "three whole days in [from, to)");
    for r in &rows {
        assert_eq!(
            r.active_total,
            r.active_identified + r.active_guest,
            "day {} does not add up",
            r.day
        );
    }

    drop(conn);
    db.cleanup().await;
}

/// Mixed `One`/`All` selection, so the bind-index walk is the thing under
/// test: deriving the env bind from anything but `consumes_bind()` silently
/// pairs an environment with the wrong app.
#[tokio::test]
async fn active_users_combined_respects_per_app_environment_filters() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let (app_b, env_b2, _issue_b) = second_app(&mut conn, ids.project_id, "envfilter-b").await;

    let only_in_env_b = format!("only-b-{}", Uuid::new_v4().simple());
    let in_env_a = format!("in-a-{}", Uuid::new_v4().simple());
    let in_app_b = format!("in-appb-{}", Uuid::new_v4().simple());
    common::seed_signal_event(
        &mut conn,
        ids.app_id,
        Some(ids.env_b),
        &only_in_env_b,
        day_at("2026-05-04", "09:00:00"),
    )
    .await;
    common::seed_signal_event(
        &mut conn,
        ids.app_id,
        Some(ids.env_a),
        &in_env_a,
        day_at("2026-05-04", "09:00:00"),
    )
    .await;
    common::seed_signal_event(
        &mut conn,
        app_b,
        Some(env_b2),
        &in_app_b,
        day_at("2026-05-04", "09:00:00"),
    )
    .await;

    let (from, to) = window("2026-05-04", "2026-05-05");
    let rows = sauron_db::repo::active_users_combined(
        &mut conn,
        &[
            AppEnvScope {
                app_id: ids.app_id,
                env: EnvFilter::One(ids.env_a),
            },
            AppEnvScope {
                app_id: app_b,
                env: EnvFilter::All,
            },
        ],
        from,
        to,
    )
    .await
    .expect("query");

    // env_a's identity plus app B's, but NOT the env_b-only one. The harness's
    // own seeded env_a rows land far in the past, outside this window.
    assert_eq!(
        rows[0].active_total, 2,
        "the env_b-only identity must not appear"
    );

    drop(conn);
    db.cleanup().await;
}

/// UTC calendar days, proven independent of the session `TimeZone` GUC — the
/// exact hazard `date_trunc('day', timestamptz)` has.
#[tokio::test]
async fn active_user_days_are_utc_calendar_days() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    diesel::sql_query("SET TimeZone = 'America/New_York'")
        .execute(&mut conn)
        .await
        .expect("move the session clock off UTC");

    let did = format!("midnight-{}", Uuid::new_v4().simple());
    common::seed_signal_event(
        &mut conn,
        ids.app_id,
        Some(ids.env_a),
        &did,
        day_at("2026-05-04", "23:30:00"),
    )
    .await;
    common::seed_signal_event(
        &mut conn,
        ids.app_id,
        Some(ids.env_a),
        &did,
        day_at("2026-05-05", "00:30:00"),
    )
    .await;

    let (from, to) = window("2026-05-04", "2026-05-06");
    let rows = sauron_db::repo::active_users_combined(
        &mut conn,
        &[AppEnvScope {
            app_id: ids.app_id,
            env: EnvFilter::One(ids.env_a),
        }],
        from,
        to,
    )
    .await
    .expect("query");

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].day.to_string(), "2026-05-04");
    assert_eq!(rows[0].active_total, 1);
    assert_eq!(rows[1].day.to_string(), "2026-05-05");
    assert_eq!(rows[1].active_total, 1);

    drop(conn);
    db.cleanup().await;
}

/// A gap day is present with three zeros, not absent. The CSV's row count is
/// checked against this grid.
#[tokio::test]
async fn active_users_combined_returns_zero_rows_for_days_with_no_signal() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let did = format!("gap-{}", Uuid::new_v4().simple());
    common::seed_signal_event(
        &mut conn,
        ids.app_id,
        Some(ids.env_a),
        &did,
        day_at("2026-05-04", "09:00:00"),
    )
    .await;
    common::seed_signal_event(
        &mut conn,
        ids.app_id,
        Some(ids.env_a),
        &did,
        day_at("2026-05-06", "09:00:00"),
    )
    .await;

    let (from, to) = window("2026-05-04", "2026-05-07");
    let rows = sauron_db::repo::active_users_combined(
        &mut conn,
        &[AppEnvScope {
            app_id: ids.app_id,
            env: EnvFilter::One(ids.env_a),
        }],
        from,
        to,
    )
    .await
    .expect("query");

    assert_eq!(rows.len(), 3);
    assert_eq!(rows[1].day.to_string(), "2026-05-05");
    assert_eq!(rows[1].active_total, 0);
    assert_eq!(rows[1].active_identified, 0);
    assert_eq!(rows[1].active_guest, 0);

    drop(conn);
    db.cleanup().await;
}

/// The empty string is a REAL value on this wire — server SDKs deliberately
/// let the three `$workflow_*` events through with one — so it has to be
/// excluded explicitly, not assumed away.
#[tokio::test]
async fn active_users_combined_excludes_empty_and_null_distinct_ids() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    common::seed_signal_event(
        &mut conn,
        ids.app_id,
        Some(ids.env_a),
        "",
        day_at("2026-05-04", "09:00:00"),
    )
    .await;
    common::seed_signal_error(
        &mut conn,
        ids.app_id,
        Some(ids.env_a),
        ids.issue_id,
        None,
        day_at("2026-05-04", "10:00:00"),
    )
    .await;

    let (from, to) = window("2026-05-04", "2026-05-05");
    let rows = sauron_db::repo::active_users_combined(
        &mut conn,
        &[AppEnvScope {
            app_id: ids.app_id,
            env: EnvFilter::One(ids.env_a),
        }],
        from,
        to,
    )
    .await
    .expect("query");

    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].active_total, 0,
        "neither an empty nor a NULL distinct_id is a person"
    );

    drop(conn);
    db.cleanup().await;
}

/// The batched `env_ids_for_app`, keyed so a caller can build a per-app map.
/// A FLAT set of these ids is meaningless — `role_grants.scope_id` for
/// `scope_type='env'` holds an `app_environments.id`, which is per-app — and
/// handing the union to `resolve_env_filter` breaks both of its decisions in
/// the granting direction.
#[tokio::test]
async fn env_ids_for_apps_keys_every_enrollment_by_its_app() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let (app_b, env_b2, _issue_b) = second_app(&mut conn, ids.project_id, "envids-b").await;

    let mut rows = sauron_db::repo::env_ids_for_apps(&mut conn, &[ids.app_id, app_b])
        .await
        .expect("query");
    rows.sort();

    assert!(rows.contains(&(ids.app_id, ids.env_a)));
    assert!(rows.contains(&(ids.app_id, ids.env_b)));
    assert!(rows.contains(&(app_b, env_b2)));
    assert!(
        !rows.contains(&(ids.app_id, env_b2)),
        "app B's enrollment must never be attributed to app A"
    );

    assert!(
        sauron_db::repo::env_ids_for_apps(&mut conn, &[])
            .await
            .expect("empty input")
            .is_empty(),
        "an empty input must not produce a query with an empty ANY()"
    );

    drop(conn);
    db.cleanup().await;
}

/// Impossible to write before the re-anchoring, which is the point: the three
/// windows were three separate `now()` calls evaluated by Postgres inside one
/// statement, so they were three different instants and no test could place a
/// row relative to them without freezing the server clock.
#[tokio::test]
async fn user_stats_dau_wau_are_anchored_to_the_supplied_now() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    // A FRESH app, not the harness app: `seed_two_envs` pins every signal row to
    // TODAY at noon UTC, and the three windows are lower-bounded only
    // (`occurred_at >= now - interval`, no upper bound). Against the harness app
    // those rows sit two months *after* `pinned`, so they fall inside all three
    // windows and `dau` reads 4 instead of 0 — the assertions below then measure
    // the harness's seed, not the anchoring.
    let (app, env, _issue) = second_app(&mut conn, ids.project_id, "anchored").await;

    let pinned = day_at("2026-05-10", "12:00:00");
    let did = format!("anchored-{}", Uuid::new_v4().simple());
    common::seed_signal_event(&mut conn, app, Some(env), &did, pinned - Duration::days(2)).await;

    let s = sauron_db::repo::user_stats(
        &mut conn,
        ReadScope::new(app, EnvFilter::One(env)),
        far_past(),
        pinned,
    )
    .await
    .expect("user_stats");

    // This app has no other signal at all, so only the row seeded above can fall
    // inside any of the three windows.
    assert_eq!(
        s.dau, 0,
        "two days before `now` is outside the 1-day window"
    );
    assert_eq!(s.wau, 1, "…inside the 7-day window");
    assert_eq!(s.mau, 1, "…and inside the 30-day window");

    drop(conn);
    db.cleanup().await;
}

/// The `distinct_id` twin of migration 53's device indexes. `list_persons`'
/// three LATERALs and its three membership legs all probe
/// `(app_id, distinct_id)` filtered by `environment_id`, but before migration
/// 55 the only usable index was `analytics_distinct_idx (app_id, distinct_id,
/// occurred_at DESC)` — no `environment_id` — so every probe matched on the
/// first two columns and then heap-fetched to test the environment, once per
/// person, across every partition.
#[tokio::test]
async fn env_person_indexes_exist() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;

    #[derive(QueryableByName)]
    struct Name {
        #[diesel(sql_type = Text)]
        indexname: String,
    }

    let rows: Vec<Name> = diesel::sql_query(
        "SELECT indexname FROM pg_indexes \
         WHERE indexname IN ('analytics_events_app_distinct_env_idx', \
                             'error_events_app_distinct_env_idx', \
                             'sessions_app_distinct_env_idx') \
         ORDER BY indexname",
    )
    .get_results(&mut conn)
    .await
    .expect("pg_indexes query");

    let found: Vec<String> = rows.into_iter().map(|r| r.indexname).collect();
    assert_eq!(
        found,
        vec![
            "analytics_events_app_distinct_env_idx".to_string(),
            "error_events_app_distinct_env_idx".to_string(),
            "sessions_app_distinct_env_idx".to_string(),
        ],
        "migration 55 must create all three env-person indexes"
    );

    drop(conn);
    db.cleanup().await;
}

/// `list_persons` used to open-code the same three correlated `EXISTS` that
/// `event_user_membership_exists` was rewritten away from (measured 32.6s ->
/// 3.5s on `overview_totals`). A correlated `EXISTS` is probed per candidate
/// row across every partition; the uncorrelated `IN (… UNION …)` builds the
/// membership set once per leg. Asserted on the emitted SQL because both shapes
/// return identical rows — which is exactly why the duplicate survived.
#[tokio::test]
async fn list_persons_membership_is_uncorrelated() {
    let sql = sauron_db::repo::list_persons_sql_for_test(EnvFilter::One(Uuid::nil()));
    assert!(
        sql.contains("event_users.distinct_id IN ("),
        "membership must be the uncorrelated IN (… UNION …) form, got:\n{sql}"
    );
    assert!(
        !sql.contains("EXISTS (SELECT 1 FROM analytics_events ae"),
        "the open-coded correlated EXISTS block must be gone, got:\n{sql}"
    );
    // `All` still emits no membership predicate at all — every `event_users`
    // row exists because a real signal registered it, so an unfiltered test
    // would narrow nothing.
    let all = sauron_db::repo::list_persons_sql_for_test(EnvFilter::All);
    assert!(
        !all.contains("event_users.distinct_id IN ("),
        "EnvFilter::All must emit no membership predicate, got:\n{all}"
    );
}

/// Every funnel step must be bounded by `since`, not just step 0.
///
/// `s0` reads `analytics_events` under `occurred_at>=$2`; each `s{i>0}` re-reads it under
/// only the *correlated* `a.occurred_at >= s{i-1}.t`. A correlated bound is not a constant,
/// so it prunes nothing: without an explicit `>=$2` on each step, every step past 0 scanned
/// EVERY partition — the app's entire retained history of that event name — while step 0
/// read only the window. That makes the funnel's cost scale with total retained data instead
/// of `since_days`, which is what eventually crosses `sauron-api`'s 30s `TimeoutLayer` and
/// returns a 503 to the dashboard.
///
/// This is invisible to a counts-based test: the added predicate is implied by the chain
/// (`s{i}.t >= .. >= s0.t >= since`), so it can never change a result — which is exactly why
/// it is easy to delete as "redundant" and why the guard has to read the PLAN.
#[tokio::test]
async fn funnel_prunes_every_step_to_the_since_window() {
    #[derive(diesel::QueryableByName)]
    struct Plan {
        #[diesel(sql_type = Text, column_name = "QUERY PLAN")]
        line: String,
    }

    // Cheap half of the guard: no database needed, and it fails the instant the predicate
    // is dropped, naming the reason. The EXPLAIN below proves it actually buys pruning.
    for env in [EnvFilter::All, EnvFilter::One(Uuid::new_v4())] {
        let sql = sauron_db::repo::funnel_sql(&env, 4);
        assert_eq!(
            sql.matches("occurred_at>=$2").count(),
            4,
            "all 4 steps must carry the constant `since` bound, not just s0; got:\n{sql}"
        );
    }

    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    // Two explicit partitions in ranges the seed never touches, so creating them cannot
    // collide with rows already sitting in the default partition (Postgres refuses to
    // attach a partition whose range would capture existing default rows).
    for (suffix, from, to) in [
        ("probe_old", "2019-01-01T00:00:00Z", "2019-02-01T00:00:00Z"),
        ("probe_new", "2020-01-01T00:00:00Z", "2020-02-01T00:00:00Z"),
    ] {
        diesel::sql_query(format!(
            "CREATE TABLE analytics_events_{suffix} PARTITION OF analytics_events \
             FOR VALUES FROM ('{from}') TO ('{to}')"
        ))
        .execute(&mut conn)
        .await
        .unwrap();
    }

    // One person clearing both steps inside the NEW partition, plus the same event names
    // sitting in the OLD one. `since` lands between the two partitions.
    for (occurred, part) in [
        ("2019-01-15T00:00:00Z", "old"),
        ("2020-01-15T00:00:00Z", "new"),
    ] {
        for step in ["harness.funnel.step1", "harness.funnel.step2"] {
            diesel::sql_query(
                "INSERT INTO analytics_events (app_id, environment_id, name, distinct_id, occurred_at) \
                 VALUES ($1, $2, $3, $4, $5::timestamptz)",
            )
            .bind::<SqlUuid, _>(ids.app_id)
            .bind::<SqlUuid, _>(ids.env_a)
            .bind::<Text, _>(step)
            .bind::<Text, _>(format!("prune_probe_{part}"))
            .bind::<Text, _>(occurred)
            .execute(&mut conn)
            .await
            .unwrap();
        }
    }

    // EXPLAIN the REAL query the repo runs, not a retyped lookalike — retyping would
    // measure the copy and stay green while the shipped SQL regressed.
    let sql = sauron_db::repo::funnel_sql(&EnvFilter::One(ids.env_a), 2);
    let plan: Vec<Plan> = diesel::sql_query(format!("EXPLAIN {sql}"))
        .bind::<SqlUuid, _>(ids.app_id)
        .bind::<diesel::sql_types::Timestamptz, _>(
            "2019-06-01T00:00:00Z"
                .parse::<chrono::DateTime<Utc>>()
                .unwrap(),
        )
        .bind::<SqlUuid, _>(ids.env_a)
        .bind::<Text, _>("harness.funnel.step1")
        .bind::<Text, _>("harness.funnel.step2")
        .load(&mut conn)
        .await
        .unwrap();
    let text = plan
        .iter()
        .map(|p| p.line.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        text.contains("analytics_events_probe_new"),
        "sanity: the in-window partition must be read, else the assertion below passes \
         vacuously on a plan that touches nothing; plan was:\n{text}"
    );
    assert!(
        !text.contains("analytics_events_probe_old"),
        "no step may read a partition entirely older than `since` — an unpruned step \
         scans the whole retained history of its event name on every funnel request; \
         plan was:\n{text}"
    );

    drop(conn);
    db.cleanup().await;
}
