//! The list time window — a caller-chosen timestamp column, and both bounds.
//!
//! Skips (does not fail) when `TEST_DATABASE_URL` is unset, mirroring
//! `env_scoping.rs` and `device_groups.rs`.
//!
//! **The fixture's whole point is that `first_seen` and `last_seen` DISAGREE.**
//! A row first seen long ago but active yesterday matches a `last_seen` window
//! and must MISS a `first_seen` one. A fixture where the two orderings
//! correlate cannot tell a working column selector from one that ignores the
//! parameter — which is the single thing these tests exist to check. The same
//! trap the table-sorting work hit: `last_seen` got away with ordering by one
//! value and displaying another precisely because they correlated.
//!
//! **Two apps, always.** A single-app fixture returns the same rows whether or
//! not a predicate is correctly scoped, which is how the slice-2 cross-tenant
//! leak reached a passing suite. Every assertion below therefore also asserts
//! the second app's row never appears.

mod common;

use chrono::{DateTime, Duration, Utc};
use common::TestDb;
use sauron_db::repo;
use sauron_db::repo::{SortSpec, TimeWindow};
use sauron_db::scope::ReadScope;
use uuid::Uuid;

fn device_sort() -> SortSpec {
    SortSpec {
        column: "last_seen",
        descending: true,
        tiebreak: "d.device_key",
        nulls_last: false,
    }
}

fn person_sort() -> SortSpec {
    SortSpec {
        column: "last_seen",
        descending: true,
        tiebreak: "eu.distinct_id",
        nulls_last: false,
    }
}

fn ago(days: i64) -> DateTime<Utc> {
    Utc::now() - Duration::days(days)
}

/// A `devices` row with both timestamps pinned independently.
///
/// Written with `diesel::sql_query` rather than a repo helper because no helper
/// takes `first_seen` and `last_seen` as separate caller-chosen values — the
/// ingest path derives both from one event, which is exactly the correlation
/// this fixture has to break.
async fn seed_device(
    conn: &mut sauron_db::PgConn,
    app_id: Uuid,
    key: &str,
    first_seen: DateTime<Utc>,
    last_seen: DateTime<Utc>,
) {
    use diesel::sql_types::{Text, Timestamptz, Uuid as SqlUuid};
    use diesel_async::RunQueryDsl;
    diesel::sql_query(
        "INSERT INTO devices (id, app_id, device_key, first_seen, last_seen, \
                              events_count, errors_count) \
         VALUES (gen_random_uuid(), $1, $2, $3, $4, 0, 0)",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Text, _>(key.to_string())
    .bind::<Timestamptz, _>(first_seen)
    .bind::<Timestamptz, _>(last_seen)
    .execute(conn)
    .await
    .expect("seed device");
}

/// An `event_users` row with both timestamps pinned independently. Same
/// reasoning as [`seed_device`].
async fn seed_person(
    conn: &mut sauron_db::PgConn,
    app_id: Uuid,
    distinct_id: &str,
    first_seen: DateTime<Utc>,
    last_seen: DateTime<Utc>,
) {
    use diesel::sql_types::{Text, Timestamptz, Uuid as SqlUuid};
    use diesel_async::RunQueryDsl;
    diesel::sql_query(
        "INSERT INTO event_users (id, app_id, distinct_id, properties, first_seen, last_seen) \
         VALUES (gen_random_uuid(), $1, $2, '{}'::jsonb, $3, $4)",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Text, _>(distinct_id.to_string())
    .bind::<Timestamptz, _>(first_seen)
    .bind::<Timestamptz, _>(last_seen)
    .execute(conn)
    .await
    .expect("seed person");
}

/// A second app under the same project, to catch a predicate that forgot its
/// `app_id`.
async fn second_app(db: &TestDb, project_id: Uuid) -> Uuid {
    let mut conn = db.conn().await;
    let suffix = Uuid::new_v4().simple().to_string();
    repo::create_app(
        &mut conn,
        project_id,
        "other app",
        &format!("other-app-{suffix}"),
        "web",
    )
    .await
    .expect("create second app")
    .id
}

#[tokio::test]
async fn devices_window_selects_by_the_named_column() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let other = second_app(&db, ids.project_id).await;
    let mut conn = db.conn().await;

    // `loyal` breaks the correlation: old first sighting, recent activity.
    seed_device(&mut conn, ids.app_id, "tw-loyal", ago(200), ago(1)).await;
    seed_device(&mut conn, ids.app_id, "tw-newbie", ago(2), ago(1)).await;
    seed_device(&mut conn, ids.app_id, "tw-dormant", ago(300), ago(120)).await;
    // Must never appear under `ids.app_id`, whatever the window says.
    seed_device(&mut conn, other, "tw-other-app", ago(2), ago(1)).await;

    let keys = |rows: Vec<repo::DeviceRow>| {
        let mut k: Vec<String> = rows
            .into_iter()
            .map(|r| r.device_key)
            .filter(|k: &String| k.starts_with("tw-"))
            .collect();
        k.sort();
        k
    };

    let by_last = repo::list_devices(
        &mut conn,
        ReadScope::all(ids.app_id),
        TimeWindow::since("last_seen", ago(7)),
        50,
        0,
        device_sort(),
        None,
        None,
    )
    .await
    .expect("list by last_seen");
    assert_eq!(
        keys(by_last),
        vec!["tw-loyal".to_string(), "tw-newbie".to_string()],
        "last_seen admits the long-lived device; and never the other app's"
    );

    let by_first = repo::list_devices(
        &mut conn,
        ReadScope::all(ids.app_id),
        TimeWindow::since("first_seen", ago(7)),
        50,
        0,
        device_sort(),
        None,
        None,
    )
    .await
    .expect("list by first_seen");
    assert_eq!(
        keys(by_first),
        vec!["tw-newbie".to_string()],
        "first_seen is the 'new devices' question and must drop tw-loyal"
    );

    db.cleanup().await;
}

#[tokio::test]
async fn devices_window_upper_bound_is_exclusive() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let other = second_app(&db, ids.project_id).await;
    let mut conn = db.conn().await;

    seed_device(&mut conn, ids.app_id, "tw-old", ago(200), ago(200)).await;
    seed_device(&mut conn, ids.app_id, "tw-mid", ago(60), ago(60)).await;
    seed_device(&mut conn, ids.app_id, "tw-new", ago(1), ago(1)).await;
    seed_device(&mut conn, other, "tw-other-app", ago(60), ago(60)).await;

    let rows = repo::list_devices(
        &mut conn,
        ReadScope::all(ids.app_id),
        TimeWindow {
            column: "last_seen",
            from: ago(120),
            to: Some(ago(7)),
        },
        50,
        0,
        device_sort(),
        None,
        None,
    )
    .await
    .expect("bounded window");
    let keys: Vec<String> = rows
        .into_iter()
        .map(|r| r.device_key)
        .filter(|k: &String| k.starts_with("tw-"))
        .collect();
    assert_eq!(
        keys,
        vec!["tw-mid".to_string()],
        "a bounded window excludes both ends, and the other app entirely"
    );

    db.cleanup().await;
}

#[tokio::test]
async fn persons_window_selects_by_the_named_column() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let other = second_app(&db, ids.project_id).await;
    let mut conn = db.conn().await;

    seed_person(&mut conn, ids.app_id, "tw-loyal", ago(200), ago(1)).await;
    seed_person(&mut conn, ids.app_id, "tw-newbie", ago(2), ago(1)).await;
    seed_person(&mut conn, ids.app_id, "tw-dormant", ago(300), ago(120)).await;
    seed_person(&mut conn, other, "tw-other-app", ago(2), ago(1)).await;

    let ours = |rows: Vec<repo::PersonRow>| {
        let mut k: Vec<String> = rows
            .into_iter()
            .map(|r| r.distinct_id)
            .filter(|d: &String| d.starts_with("tw-"))
            .collect();
        k.sort();
        k
    };

    let by_last = repo::list_persons(
        &mut conn,
        ReadScope::all(ids.app_id),
        None,
        200,
        0,
        person_sort(),
        TimeWindow::since("last_seen", ago(7)),
    )
    .await
    .expect("persons by last_seen");
    assert_eq!(
        ours(by_last),
        vec!["tw-loyal".to_string(), "tw-newbie".to_string()],
    );

    let by_first = repo::list_persons(
        &mut conn,
        ReadScope::all(ids.app_id),
        None,
        200,
        0,
        person_sort(),
        TimeWindow::since("first_seen", ago(7)),
    )
    .await
    .expect("persons by first_seen");
    assert_eq!(
        ours(by_first),
        vec!["tw-newbie".to_string()],
        "'new users' — the question the Users page's own stat tiles have always \
         drawn but the table could never reproduce"
    );

    db.cleanup().await;
}

/// The scoped read takes a DIFFERENT code path: `list_persons`' live shape
/// applies the predicate on the OUTER query against `LEAST`/`GREATEST` over
/// three LATERALs, where `EnvFilter::All` applies it inside the subquery on the
/// durable column. A test that only ever runs unscoped leaves half the SQL
/// unexecuted — and the two halves differ by more than an alias, so a string
/// that composes is not evidence either runs.
#[tokio::test]
async fn persons_window_applies_under_an_environment_scope() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    // Only assert that the scoped path EXECUTES and stays consistent: the
    // seeded env fixture's own rows decide the contents, and pinning them here
    // would duplicate `env_scoping.rs`'s assertions and break with that
    // fixture rather than with this feature.
    let scoped = repo::list_persons(
        &mut conn,
        ReadScope::new(ids.app_id, sauron_db::scope::EnvFilter::One(ids.env_a)),
        None,
        200,
        0,
        person_sort(),
        TimeWindow::since("last_seen", ago(3650)),
    )
    .await
    .expect("scoped persons window must execute");

    let narrow = repo::list_persons(
        &mut conn,
        ReadScope::new(ids.app_id, sauron_db::scope::EnvFilter::One(ids.env_a)),
        None,
        200,
        0,
        person_sort(),
        // A window that ends before the fixture was written admits nobody.
        TimeWindow {
            column: "last_seen",
            from: ago(3650),
            to: Some(ago(3600)),
        },
    )
    .await
    .expect("scoped bounded window must execute");

    assert!(
        narrow.len() < scoped.len(),
        "the scoped path must actually apply the window: {} vs {}",
        narrow.len(),
        scoped.len()
    );
    assert!(
        narrow.is_empty(),
        "a window ending 3600 days ago admits nobody"
    );

    db.cleanup().await;
}
