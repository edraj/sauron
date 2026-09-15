//! `list_persons` / `count_persons` under `EnvFilter::All` must page
//! `event_users` BEFORE touching any per-person aggregate.
//!
//! The Users Explorer's default request is exactly this shape (`?sort=last_seen`,
//! no `environment_id`). Both query shapes used to do O(persons-in-app) work
//! ahead of `LIMIT`: the live shape ran three LATERALs per admitted person and
//! sorted afterwards, the rollup shape `GROUP BY`-ed every
//! `event_user_environments` row of the app and hash-joined every person before
//! the top-N sort. At 3.5M persons that is the 60 s request timeout, and
//! `count_persons` wrapped the same SQL so the page paid it twice.
//!
//! These tests read the PLAN, not the clock: a fixture small enough for CI
//! cannot time out, but it can prove where `Limit` sits.

mod common;

use common::TestDb;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use sauron_db::repo::{self, SortSpec, TimeWindow};
use sauron_db::scope::{EnvFilter, ReadScope};

const SIGNAL_TABLES: &[&str] = &[
    "analytics_events",
    "error_events",
    "sessions",
    "event_user_environments",
];

#[derive(QueryableByName)]
struct PlanRow {
    #[diesel(sql_type = diesel::sql_types::Json, column_name = "QUERY PLAN")]
    plan: serde_json::Value,
}

/// Runs `EXPLAIN (FORMAT JSON)` over the exact string a repo builder emits,
/// with its binds replaced by literals. Descending order so `$1` can never
/// clobber a `$10`.
async fn explain(conn: &mut sauron_db::PgConn, sql: &str, app_id: uuid::Uuid) -> serde_json::Value {
    let lit = sql
        .replace("$6", "NULL::timestamptz")
        .replace("$5", "(now() - interval '3650 days')")
        .replace("$4", "0")
        .replace("$3", "51")
        .replace("$2", "'%'")
        .replace("$1", &format!("'{app_id}'::uuid"));
    let row: PlanRow = diesel::sql_query(format!("EXPLAIN (FORMAT JSON) {lit}"))
        .get_result(conn)
        .await
        .unwrap_or_else(|e| panic!("EXPLAIN failed: {e}\n{lit}"));
    let mut v = row.plan;
    v[0]["Plan"].take()
}

/// Every relation name (partition or parent) under `node`, inclusive.
fn relations(node: &serde_json::Value, out: &mut Vec<String>) {
    if let Some(r) = node.get("Relation Name").and_then(|r| r.as_str()) {
        out.push(r.to_string());
    }
    if let Some(kids) = node.get("Plans").and_then(|p| p.as_array()) {
        for k in kids {
            relations(k, out);
        }
    }
}

/// Every `Limit` node in the plan.
fn limits<'a>(node: &'a serde_json::Value, out: &mut Vec<&'a serde_json::Value>) {
    if node.get("Node Type").and_then(|t| t.as_str()) == Some("Limit") {
        out.push(node);
    }
    if let Some(kids) = node.get("Plans").and_then(|p| p.as_array()) {
        for k in kids {
            limits(k, out);
        }
    }
}

fn touches_signal(rel: &str) -> bool {
    SIGNAL_TABLES.iter().any(|t| rel.starts_with(t))
}

/// The invariant: some `Limit` node's subtree reads ONLY `event_users`. That is
/// what "page first, then enrich" looks like in a plan, whatever the planner
/// does above it.
fn assert_limit_precedes_enrichment(label: &str, plan: &serde_json::Value) {
    let mut ls = Vec::new();
    limits(plan, &mut ls);
    assert!(!ls.is_empty(), "{label}: no Limit node at all:\n{plan:#}");
    let ok = ls.iter().any(|l| {
        let mut rels = Vec::new();
        relations(l, &mut rels);
        !rels.is_empty() && rels.iter().all(|r| !touches_signal(r))
    });
    assert!(
        ok,
        "{label}: every Limit sits above the per-person enrichment, so the page \
         does O(persons) work before LIMIT:\n{plan:#}"
    );
}

fn sort(column: &'static str) -> SortSpec {
    SortSpec {
        column,
        descending: true,
        tiebreak: "eu.distinct_id",
        nulls_last: false,
    }
}

#[tokio::test]
async fn all_scope_pages_event_users_before_the_laterals_in_the_live_shape() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    for col in ["last_seen", "first_seen", "eu.distinct_id"] {
        let sql = repo::persons_list_sql(&EnvFilter::All, false, &sort(col), "last_seen", false);
        let plan = explain(&mut conn, &sql, ids.app_id).await;
        assert_limit_precedes_enrichment(&format!("live All sort={col}"), &plan);
    }
    drop(conn);
    db.cleanup().await;
}

#[tokio::test]
async fn all_scope_pages_event_users_before_the_rollup_join_in_the_rollup_shape() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    for col in ["last_seen", "first_seen", "eu.distinct_id"] {
        let sql = repo::persons_list_sql(&EnvFilter::All, true, &sort(col), "last_seen", false);
        let plan = explain(&mut conn, &sql, ids.app_id).await;
        assert_limit_precedes_enrichment(&format!("rollup All sort={col}"), &plan);
    }
    drop(conn);
    db.cleanup().await;
}

/// The count never depends on the sort, and under `All` the admitted set is
/// exactly the `event_users` rows the search and window admit — so the count
/// must not read a signal table at all, in either shape, for any sort.
#[tokio::test]
async fn all_scope_count_reads_only_event_users() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    for backfilled in [false, true] {
        for col in ["last_seen", "events_count"] {
            let sql = repo::persons_count_sql(
                &EnvFilter::All,
                backfilled,
                &sort(col),
                "last_seen",
                false,
            );
            let plan = explain(&mut conn, &sql, ids.app_id).await;
            let mut rels = Vec::new();
            relations(&plan, &mut rels);
            assert!(
                rels.iter().any(|r| r == "event_users"),
                "count (backfilled={backfilled}, sort={col}) must read event_users:\n{plan:#}"
            );
            assert!(
                rels.iter().all(|r| !touches_signal(r)),
                "count (backfilled={backfilled}, sort={col}) reads a signal table:\n{plan:#}"
            );
        }
    }
    drop(conn);
    db.cleanup().await;
}

/// The scoped shapes are NOT changed by this fix; pin that the count still
/// applies membership there (it must read the rollup or the signal tables).
#[tokio::test]
async fn scoped_count_still_applies_membership() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let sql = repo::persons_count_sql(
        &EnvFilter::One(ids.env_a),
        true,
        &sort("last_seen"),
        "last_seen",
        false,
    );
    // Scoped: $5 is the environment, the window moves to $6/$7.
    let lit = sql
        .replace("$7", "NULL::timestamptz")
        .replace("$6", "(now() - interval '3650 days')")
        .replace("$5", &format!("'{}'::uuid", ids.env_a));
    let plan = explain(&mut conn, &lit, ids.app_id).await;
    let mut rels = Vec::new();
    relations(&plan, &mut rels);
    assert!(
        rels.iter().any(|r| r == "event_user_environments"),
        "scoped rollup count must read the rollup:\n{plan:#}"
    );
    drop(conn);
    db.cleanup().await;
}

type Key = (
    String,
    i64,
    i64,
    i64,
    chrono::DateTime<chrono::Utc>,
    chrono::DateTime<chrono::Utc>,
);

fn key(r: &repo::PersonRow) -> Key {
    (
        r.distinct_id.clone(),
        r.events_count,
        r.errors_count,
        r.sessions_count,
        r.first_seen,
        r.last_seen,
    )
}

/// Every durable-column sort, both directions, paged two at a time, against
/// `reference` re-sorted in Rust; and the count agrees with the list.
async fn check_pages(
    conn: &mut sauron_db::PgConn,
    label: &str,
    app_id: uuid::Uuid,
    reference: &[Key],
    window: TimeWindow,
) {
    for (col, descending) in [
        ("last_seen", true),
        ("last_seen", false),
        ("first_seen", true),
        ("first_seen", false),
        ("eu.distinct_id", true),
        ("eu.distinct_id", false),
    ] {
        let mut want = reference.to_vec();
        want.sort_by(|a, b| {
            let o = match col {
                "last_seen" => a.5.cmp(&b.5),
                "first_seen" => a.4.cmp(&b.4),
                _ => a.0.cmp(&b.0),
            };
            let o = if descending { o.reverse() } else { o };
            o.then_with(|| a.0.cmp(&b.0))
        });
        let spec = || SortSpec {
            column: col,
            descending,
            tiebreak: "eu.distinct_id",
            nulls_last: false,
        };
        let mut have = Vec::new();
        let mut off = 0;
        loop {
            let page =
                repo::list_persons(conn, ReadScope::all(app_id), None, 2, off, spec(), window)
                    .await
                    .expect("page");
            if page.is_empty() {
                break;
            }
            off += page.len() as i64;
            have.extend(page.iter().map(key));
        }
        assert_eq!(have, want, "{label}: sort={col} desc={descending}");
        let (n, capped) =
            repo::count_persons(conn, ReadScope::all(app_id), None, spec(), window, 10_000)
                .await
                .expect("count");
        assert_eq!(
            (n as usize, capped),
            (want.len(), false),
            "{label}: count sort={col}"
        );
    }
}

/// Behaviour, not plan: paging first must not change WHAT is served. The
/// reference is the count-column sort (the shape this fix leaves untouched).
#[tokio::test]
async fn page_first_serves_the_same_rows_in_the_same_order() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let window = TimeWindow::since(
        "last_seen",
        chrono::Utc::now() - chrono::Duration::days(3650),
    );

    let all = repo::list_persons(
        &mut conn,
        ReadScope::all(ids.app_id),
        None,
        1000,
        0,
        sort("events_count"),
        window,
    )
    .await
    .expect("reference list");
    assert!(
        all.len() >= 5,
        "fixture must hold people, got {}",
        all.len()
    );
    let reference: Vec<Key> = all.iter().map(key).collect();

    check_pages(&mut conn, "live", ids.app_id, &reference, window).await;
    sauron_db::person_env_backfill::backfill_app(
        &mut conn,
        ids.app_id,
        ids.pinned_now + chrono::Duration::hours(1),
    )
    .await
    .expect("backfill");
    assert!(
        sauron_db::person_env_backfill::is_backfilled(&mut conn, ids.app_id)
            .await
            .unwrap()
    );
    check_pages(&mut conn, "rollup", ids.app_id, &reference, window).await;

    drop(conn);
    db.cleanup().await;
}

/// The search term and the window still narrow the paged set under `All`,
/// and the count agrees with the list — pinned because both now live inside
/// the paging subquery rather than on the outer query.
#[tokio::test]
async fn search_and_window_still_narrow_the_paged_set() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let window = TimeWindow::since(
        "last_seen",
        chrono::Utc::now() - chrono::Duration::days(3650),
    );
    let all = repo::list_persons(
        &mut conn,
        ReadScope::all(ids.app_id),
        None,
        1000,
        0,
        sort("last_seen"),
        window,
    )
    .await
    .expect("all");
    let probe = all[0].distinct_id.clone();
    let hits = repo::list_persons(
        &mut conn,
        ReadScope::all(ids.app_id),
        Some(&probe),
        1000,
        0,
        sort("last_seen"),
        window,
    )
    .await
    .expect("search");
    assert!(hits.iter().any(|r| r.distinct_id == probe));
    assert!(
        hits.len() < all.len(),
        "search must narrow ({} vs {})",
        hits.len(),
        all.len()
    );
    let (n, _) = repo::count_persons(
        &mut conn,
        ReadScope::all(ids.app_id),
        Some(&probe),
        sort("last_seen"),
        window,
        10_000,
    )
    .await
    .expect("count");
    assert_eq!(n as usize, hits.len());

    // A window in the future admits nobody, list and count alike.
    let future = TimeWindow::since("last_seen", chrono::Utc::now() + chrono::Duration::days(1));
    let none = repo::list_persons(
        &mut conn,
        ReadScope::all(ids.app_id),
        None,
        1000,
        0,
        sort("last_seen"),
        future,
    )
    .await
    .expect("future");
    assert!(none.is_empty());
    let (n, _) = repo::count_persons(
        &mut conn,
        ReadScope::all(ids.app_id),
        None,
        sort("last_seen"),
        future,
        10_000,
    )
    .await
    .expect("count");
    assert_eq!(n, 0);
    drop(conn);
    db.cleanup().await;
}
