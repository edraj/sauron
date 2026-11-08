//! The Postgres lookups behind reading a person's or a session's archived rows
//! (`sauron_tier::rows` → sauron-api's `tier_read`), and the rollup-served
//! person summary that now feeds the profile's tiles.
//!
//! Each is a small query whose mistakes would not show in the end-to-end test
//! (`bins/sauron-api/tests/http_archived_rows.rs`), which exercises one
//! environment scope and one completed merge: a wrong bind slot under
//! `Subset`, a pending merge treated as completed, a chain edge followed.

mod common;

use chrono::{DateTime, Duration, TimeZone, Utc};
use common::TestDb;
use diesel::sql_types::{Nullable, Text, Timestamptz, Uuid as SqlUuid};
use diesel_async::RunQueryDsl;
use sauron_db::scope::{EnvFilter, ReadScope};
use uuid::Uuid;

fn t(h: i64) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap() + Duration::hours(h)
}

#[allow(clippy::too_many_arguments)]
async fn rollup_row(
    conn: &mut sauron_db::PgConn,
    app: Uuid,
    did: &str,
    env: Option<Uuid>,
    first: DateTime<Utc>,
    last: DateTime<Utc>,
    counts: (i64, i64, i64),
) {
    diesel::sql_query(
        "INSERT INTO event_user_environments \
           (app_id, distinct_id, environment_id, first_seen, last_seen, \
            events_count, errors_count, sessions_count) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind::<SqlUuid, _>(app)
    .bind::<Text, _>(did)
    .bind::<Nullable<SqlUuid>, _>(env)
    .bind::<Timestamptz, _>(first)
    .bind::<Timestamptz, _>(last)
    .bind::<diesel::sql_types::BigInt, _>(counts.0)
    .bind::<diesel::sql_types::BigInt, _>(counts.1)
    .bind::<diesel::sql_types::BigInt, _>(counts.2)
    .execute(conn)
    .await
    .expect("insert rollup row");
}

async fn mark_backfilled(conn: &mut sauron_db::PgConn, app: Uuid) {
    diesel::sql_query(
        "INSERT INTO event_user_env_backfill (app_id, completed_at) VALUES ($1, now())",
    )
    .bind::<SqlUuid, _>(app)
    .execute(conn)
    .await
    .expect("backfill marker");
}

fn scope(app: Uuid, env: EnvFilter) -> ReadScope {
    ReadScope { app_id: app, env }
}

/// The summary's rollup branch answers every environment scope with exactly
/// the rows that scope admits — and is only taken once the app is backfilled.
#[tokio::test]
async fn the_rollup_summary_honours_every_environment_scope() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let (app, a, b) = (ids.app_id, ids.env_a, ids.env_b);
    let mut conn = db.conn().await;
    sauron_db::repo::upsert_event_user(&mut conn, app, "ra-p", &serde_json::json!({ "plan": "x" }))
        .await
        .unwrap();
    sauron_db::repo::upsert_event_user(&mut conn, app, "ra-only-a", &serde_json::json!({}))
        .await
        .unwrap();
    rollup_row(&mut conn, app, "ra-p", Some(a), t(1), t(2), (5, 1, 1)).await;
    rollup_row(&mut conn, app, "ra-p", Some(b), t(3), t(4), (2, 0, 0)).await;
    rollup_row(&mut conn, app, "ra-p", None, t(5), t(6), (1, 1, 0)).await;
    rollup_row(&mut conn, app, "ra-only-a", Some(a), t(1), t(1), (3, 0, 0)).await;

    // Before the marker the live branch counts raw Postgres rows, of which this
    // person has none — the contrast that proves which branch answered below.
    let live = sauron_db::repo::get_event_user(&mut conn, scope(app, EnvFilter::All), "ra-p")
        .await
        .unwrap()
        .expect("unscoped: the event_users row alone admits the person");
    assert_eq!((live.events_count, live.errors_count), (0, 0));

    mark_backfilled(&mut conn, app).await;
    async fn get(c: &mut sauron_db::PgConn, s: ReadScope) -> Option<sauron_db::repo::PersonRow> {
        sauron_db::repo::get_event_user(c, s, "ra-p").await.unwrap()
    }

    let all = get(&mut conn, scope(app, EnvFilter::All))
        .await
        .expect("all");
    assert_eq!(
        (all.events_count, all.errors_count, all.sessions_count),
        (8, 2, 1)
    );
    assert_eq!(all.properties, serde_json::json!({ "plan": "x" }));

    let one = get(&mut conn, scope(app, EnvFilter::One(a)))
        .await
        .expect("env a");
    assert_eq!(
        (one.events_count, one.errors_count, one.sessions_count),
        (5, 1, 1)
    );
    assert_eq!((one.first_seen, one.last_seen), (t(1), t(2)));

    let subset = get(&mut conn, scope(app, EnvFilter::Subset(vec![a, b])))
        .await
        .expect("subset");
    assert_eq!(
        (
            subset.events_count,
            subset.errors_count,
            subset.sessions_count
        ),
        (7, 1, 1)
    );
    assert_eq!((subset.first_seen, subset.last_seen), (t(1), t(4)));

    let unattributed = get(&mut conn, scope(app, EnvFilter::Unattributed))
        .await
        .expect("unattributed");
    assert_eq!(
        (
            unattributed.events_count,
            unattributed.errors_count,
            unattributed.sessions_count
        ),
        (1, 1, 0)
    );
    assert_eq!(
        (unattributed.first_seen, unattributed.last_seen),
        (t(5), t(6))
    );

    // A person with no signal in the scope is not a member of it.
    let none =
        sauron_db::repo::get_event_user(&mut conn, scope(app, EnvFilter::One(b)), "ra-only-a")
            .await
            .unwrap();
    assert!(none.is_none(), "{none:?}");

    drop(conn);
    db.cleanup().await;
}

async fn merge(
    conn: &mut sauron_db::PgConn,
    app: Uuid,
    alias: &str,
    person: &str,
    state: &str,
    completed: bool,
) {
    diesel::sql_query(
        "INSERT INTO identity_merges (id, app_id, alias_id, distinct_id, state, attempts, \
             cold_stale, next_attempt_at, created_at, completed_at) \
         VALUES (gen_random_uuid(), $1, $2, $3, $4, 0, true, now(), now(), \
                 CASE WHEN $5 THEN now() END)",
    )
    .bind::<SqlUuid, _>(app)
    .bind::<Text, _>(alias)
    .bind::<Text, _>(person)
    .bind::<Text, _>(state)
    .bind::<diesel::sql_types::Bool, _>(completed)
    .execute(conn)
    .await
    .expect("insert merge");
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

/// A guest's archived rows belong to the person exactly when its Postgres rows
/// do: once a fold has completed. Pending and dead merges keep them with the
/// guest; a chain edge is ignored, as in `cold_alias_map`.
#[tokio::test]
async fn archived_ids_follow_completed_merges_only() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let app = db.seed_two_envs().await.app_id;
    let mut conn = db.conn().await;
    merge(&mut conn, app, "g-done", "p", "done", true).await;
    // Re-armed to sweep stragglers: `state` is back to pending, but its bulk
    // has moved — `completed_at` is what says so.
    merge(&mut conn, app, "g-rearmed", "p", "pending", true).await;
    merge(&mut conn, app, "g-pending", "p", "pending", false).await;
    merge(&mut conn, app, "g-dead", "p", "dead", false).await;
    // A chain: x → y, y → z. The x → y edge must not be followed from either end.
    merge(&mut conn, app, "y", "z", "done", true).await;
    merge(&mut conn, app, "x", "y", "done", true).await;

    async fn ids(c: &mut sauron_db::PgConn, app: Uuid, person: &str) -> Vec<String> {
        sorted(
            sauron_db::identity_merge::archived_ids_for_person(c, app, person)
                .await
                .unwrap(),
        )
    }
    let c = &mut conn;
    assert_eq!(ids(c, app, "p").await, vec!["g-done", "g-rearmed", "p"]);
    assert_eq!(
        ids(c, app, "g-pending").await,
        vec!["g-pending"],
        "not yet the person's"
    );
    assert_eq!(
        ids(c, app, "g-dead").await,
        vec!["g-dead"],
        "never became the person's"
    );
    assert!(
        ids(c, app, "g-done").await.is_empty(),
        "a completed guest is no longer its own person"
    );
    assert_eq!(ids(c, app, "z").await, vec!["y", "z"]);
    assert!(ids(c, app, "y").await.is_empty());
    assert_eq!(ids(c, app, "nobody").await, vec!["nobody"]);

    let owners = sauron_db::identity_merge::archived_alias_owners(
        &mut conn,
        app,
        &["g-done", "g-pending", "g-dead", "p", "x", "y", "unknown"].map(String::from),
    )
    .await
    .unwrap();
    let mut owners: Vec<(String, String)> = owners.into_iter().collect();
    owners.sort();
    assert_eq!(
        owners,
        vec![
            ("g-done".to_string(), "p".to_string()),
            ("y".to_string(), "z".to_string())
        ]
    );

    drop(conn);
    db.cleanup().await;
}

/// The archive window's source: ingest-time first/last seen across every id,
/// `None` when none has a row.
#[tokio::test]
async fn ingest_span_covers_every_id_and_is_none_without_rows() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let app = db.seed_two_envs().await.app_id;
    let mut conn = db.conn().await;
    for (did, first, last) in [("s-p", t(10), t(20)), ("s-g", t(5), t(12))] {
        sauron_db::repo::upsert_event_user(&mut conn, app, did, &serde_json::json!({}))
            .await
            .unwrap();
        diesel::sql_query(
            "UPDATE event_users SET first_seen = $3, last_seen = $4 \
             WHERE app_id = $1 AND distinct_id = $2",
        )
        .bind::<SqlUuid, _>(app)
        .bind::<Text, _>(did)
        .bind::<Timestamptz, _>(first)
        .bind::<Timestamptz, _>(last)
        .execute(&mut conn)
        .await
        .unwrap();
    }
    let span = sauron_db::repo::person_ingest_span(&mut conn, app, &["s-p".into(), "s-g".into()])
        .await
        .unwrap();
    assert_eq!(span, Some((t(5), t(20))));
    let none = sauron_db::repo::person_ingest_span(&mut conn, app, &["s-unknown".into()])
        .await
        .unwrap();
    assert_eq!(none, None);
    assert_eq!(
        sauron_db::repo::person_ingest_span(&mut conn, app, &[])
            .await
            .unwrap(),
        None
    );

    drop(conn);
    db.cleanup().await;
}

/// The reader casts exactly these back to JSON; a text column must not be one.
#[tokio::test]
async fn jsonb_columns_lists_each_tables_json_columns() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;
    let cols = sauron_db::repo::jsonb_columns(&mut conn, "analytics_events")
        .await
        .unwrap();
    for c in ["properties", "context", "tags", "contexts", "extra"] {
        assert!(cols.iter().any(|x| x == c), "{c} missing from {cols:?}");
    }
    assert!(
        !cols.iter().any(|x| x == "name" || x == "distinct_id"),
        "{cols:?}"
    );
    let err_cols = sauron_db::repo::jsonb_columns(&mut conn, "error_events")
        .await
        .unwrap();
    for c in [
        "stacktrace",
        "breadcrumbs",
        "event_user",
        "sdk",
        "debug_meta",
    ] {
        assert!(
            err_cols.iter().any(|x| x == c),
            "{c} missing from {err_cols:?}"
        );
    }
    drop(conn);
    db.cleanup().await;
}
