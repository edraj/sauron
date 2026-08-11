//! `list_device_groups` against a real Postgres.
//!
//! Skips (does not fail) when `TEST_DATABASE_URL` is unset, mirroring
//! `env_scoping.rs` and `sessions.rs`. CI has no database service.
//!
//! Deliberately does NOT use `TestDb::seed_two_envs`: that fixture is asserted
//! on by a dozen tests in `env_scoping.rs`, including exact `devices` row
//! counts, and grouping needs devices with controlled, deliberately colliding
//! descriptor tuples.

mod common;

use chrono::{DateTime, Duration, SubsecRound, Utc};
use common::{far_past, seed_env, TestDb};
use sauron_db::models::NewAnalyticsEvent;
use sauron_db::repo;
use sauron_db::repo::DeviceGroupKey;
use sauron_db::scope::{EnvFilter, ReadScope};
use serde_json::json;
use uuid::Uuid;

/// One `analytics_events` row keyed by `device_key`, tagged `env`.
///
/// `common::seed_signal_event` cannot be reused here: it takes `distinct_id`,
/// not `device_key`, and always inserts `device_key: None` (see its doc
/// comment — it deliberately seeds a device-less signal for
/// `active_users_combined`'s tests). `device_membership_sql`'s `EXISTS` legs
/// correlate on `ae.device_key = devices.device_key`, so a NULL-`device_key`
/// row never satisfies membership for any device — confirmed empirically:
/// swapping this call for `common::seed_signal_event(&mut conn, ids.app_id,
/// Some(ids.env_a), &ids.iphone_a, ...)` makes `rows_a` come back empty and
/// the `.expect("the iOS 17.4.1 group under env_a")` below panic. Modelled on
/// `env_scoping.rs`'s `seed_cross_env_session_child_rows`, which inserts
/// `NewAnalyticsEvent` directly for the same reason.
async fn seed_device_signal(
    conn: &mut sauron_db::PgConn,
    app_id: Uuid,
    env: Option<Uuid>,
    device_key: &str,
    occurred_at: DateTime<Utc>,
) {
    repo::insert_analytics_event(
        conn,
        NewAnalyticsEvent {
            id: Uuid::new_v4(),
            app_id,
            environment_id: env,
            name: "fleet.signal".to_string(),
            distinct_id: format!("distinct-{device_key}"),
            properties: json!({}),
            context: json!({}),
            session_id: None,
            release: None,
            ip_address: None,
            occurred_at,
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
    .expect("insert device signal event");
}

/// One `sessions` row pinned to an exact `started_at`/`last_event_at` (both
/// equal `at`): this is the row's only `bump_session` call, so the upsert's
/// `LEAST(started_at)`/`GREATEST(last_event_at)` never see a second write to
/// reconcile against. No `common` helper exposes `device_key` plus a
/// caller-chosen timestamp together for `sessions` — same gap
/// `seed_device_signal` above fills for `analytics_events`.
async fn seed_group_session(
    conn: &mut sauron_db::PgConn,
    app_id: Uuid,
    env: Option<Uuid>,
    session_id: &str,
    device_key: &str,
    at: DateTime<Utc>,
) {
    repo::bump_session(
        conn,
        app_id,
        session_id,
        None,
        Some(device_key),
        at,
        &json!({}),
        None,
        env,
        None,
        1,
        0,
    )
    .await
    .expect("bump session");
}

/// Ids from [`seed_device_fleet`].
///
/// Task 1's own tests (`devices_sharing_model_and_os_collapse_into_one_group`,
/// `devices_with_null_descriptors_form_one_unknown_group`) find their rows by
/// descriptor (`os_version`/`model`/`os_name`), not by device key, so
/// `iphone_b`/`iphone_older`/`unknown` went unread there. Task 2's drill-down
/// tests read them directly (`list_devices`' group filter returns member rows
/// keyed by `device_key`, not a descriptor tuple). `#[allow(dead_code)]`
/// stays regardless — the fixture is shared across this file and future
/// tasks may add fields before every one has a reader.
#[allow(dead_code)]
struct FleetIds {
    app_id: Uuid,
    env_a: Uuid,
    env_b: Uuid,
    /// The two `iPhone / iPhone15,2 / iOS / 17.4.1` devices — the collapse case.
    iphone_a: String,
    iphone_b: String,
    /// Same model, one patch version apart — must NOT collapse into the above.
    iphone_older: String,
    /// Every descriptor column NULL — the "Unknown device" group.
    unknown: String,
    /// Only ever active in `env_b`.
    pixel_b_only: String,
    pinned_now: DateTime<Utc>,
}

/// Five devices across two environments, with deliberately colliding
/// descriptors. Counts are asymmetric per device so a summed aggregate cannot
/// accidentally match a wrong one.
async fn seed_device_fleet(db: &TestDb) -> FleetIds {
    let mut conn = db.conn().await;
    let suffix = Uuid::new_v4().simple().to_string();
    let now = Utc::now();

    let org = repo::create_org(&mut conn, "fleet org", &format!("fleet-org-{suffix}"))
        .await
        .expect("create org");
    let project = repo::create_project(
        &mut conn,
        org.id,
        "fleet project",
        &format!("fleet-proj-{suffix}"),
    )
    .await
    .expect("create project");
    let app = repo::create_app(
        &mut conn,
        project.id,
        "fleet app",
        &format!("fleet-app-{suffix}"),
        "web",
    )
    .await
    .expect("create app");
    let env_a = seed_env(
        &mut conn,
        project.id,
        app.id,
        "env_a",
        &format!("pk_fleet_a_{suffix}"),
        true,
    )
    .await;
    let env_b = seed_env(
        &mut conn,
        project.id,
        app.id,
        "env_b",
        &format!("pk_fleet_b_{suffix}"),
        false,
    )
    .await;

    let iphone_a = format!("fleet-{suffix}-iphone-a");
    let iphone_b = format!("fleet-{suffix}-iphone-b");
    let iphone_older = format!("fleet-{suffix}-iphone-older");
    let unknown = format!("fleet-{suffix}-unknown");
    let pixel_b_only = format!("fleet-{suffix}-pixel-b");

    // (device_key, family, model, os_name, os_version, events, errors)
    #[allow(clippy::type_complexity)]
    let fleet: [(
        &str,
        Option<&str>,
        Option<&str>,
        Option<&str>,
        Option<&str>,
        i64,
        i64,
    ); 5] = [
        (
            &iphone_a,
            Some("iPhone"),
            Some("iPhone15,2"),
            Some("iOS"),
            Some("17.4.1"),
            3,
            1,
        ),
        (
            &iphone_b,
            Some("iPhone"),
            Some("iPhone15,2"),
            Some("iOS"),
            Some("17.4.1"),
            5,
            2,
        ),
        (
            &iphone_older,
            Some("iPhone"),
            Some("iPhone15,2"),
            Some("iOS"),
            Some("17.4.0"),
            7,
            4,
        ),
        (&unknown, None, None, None, None, 2, 1),
        (
            &pixel_b_only,
            Some("Pixel"),
            Some("Pixel 8"),
            Some("Android"),
            Some("14"),
            9,
            3,
        ),
    ];

    for (key, family, model, os_name, os_version, events, errors) in fleet {
        repo::bump_device(
            &mut conn,
            app.id,
            key,
            family,
            model,
            os_name,
            os_version,
            None,
            None,
            None,
            now - Duration::seconds(30),
            events,
            errors,
        )
        .await
        .expect("bump_device");
    }

    drop(conn);
    FleetIds {
        app_id: app.id,
        env_a,
        env_b,
        iphone_a,
        iphone_b,
        iphone_older,
        unknown,
        pixel_b_only,
        pinned_now: now,
    }
}

#[tokio::test]
async fn devices_sharing_model_and_os_collapse_into_one_group() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = seed_device_fleet(&db).await;
    let mut conn = db.conn().await;

    let rows = repo::list_device_groups(
        &mut conn,
        ReadScope::all(ids.app_id),
        far_past(),
        50,
        0,
        None,
    )
    .await
    .expect("list_device_groups");

    let collapsed = rows
        .iter()
        .find(|r| r.os_version.as_deref() == Some("17.4.1"))
        .expect("the iOS 17.4.1 group");
    assert_eq!(
        collapsed.device_count, 2,
        "iphone_a and iphone_b are one group"
    );
    assert_eq!(collapsed.model.as_deref(), Some("iPhone15,2"));
    assert_eq!(collapsed.events_count, 8, "3 + 5 summed across the group");
    assert_eq!(collapsed.errors_count, 3, "1 + 2 summed across the group");

    // A one-patch-version difference is its own group (locked decision 2).
    let older = rows
        .iter()
        .find(|r| r.os_version.as_deref() == Some("17.4.0"))
        .expect("the iOS 17.4.0 group");
    assert_eq!(older.device_count, 1);
    assert_eq!(older.events_count, 7);

    drop(conn);
    db.cleanup().await;
}

#[tokio::test]
async fn devices_with_null_descriptors_form_one_unknown_group() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = seed_device_fleet(&db).await;
    let mut conn = db.conn().await;

    let rows = repo::list_device_groups(
        &mut conn,
        ReadScope::all(ids.app_id),
        far_past(),
        50,
        0,
        None,
    )
    .await
    .expect("list_device_groups");

    let unknown = rows
        .iter()
        .find(|r| r.model.is_none() && r.os_name.is_none())
        .expect("the all-NULL group");
    assert_eq!(unknown.device_count, 1);
    assert_eq!(unknown.events_count, 2);
    assert!(unknown.family.is_none());
    assert!(unknown.os_version.is_none());

    // Five seeded devices, four distinct descriptor tuples.
    assert_eq!(
        rows.len(),
        4,
        "17.4.1, 17.4.0, Android 14, and the NULL group"
    );

    drop(conn);
    db.cleanup().await;
}

/// A group's counts must not include a device active only in another
/// environment. `pixel_b_only` gets its only signals in `env_b`; under
/// `One(env_a)` its group must not appear at all.
#[tokio::test]
async fn groups_exclude_devices_from_other_environments() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = seed_device_fleet(&db).await;
    let mut conn = db.conn().await;

    // Give iphone_a signals in env_a and pixel_b_only signals in env_b, so
    // membership differs by environment. See `seed_device_signal`'s doc
    // comment for why this is not `common::seed_signal_event`.
    seed_device_signal(
        &mut conn,
        ids.app_id,
        Some(ids.env_a),
        &ids.iphone_a,
        ids.pinned_now - Duration::seconds(20),
    )
    .await;
    seed_device_signal(
        &mut conn,
        ids.app_id,
        Some(ids.env_b),
        &ids.pixel_b_only,
        ids.pinned_now - Duration::seconds(20),
    )
    .await;

    let rows_a = repo::list_device_groups(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        far_past(),
        50,
        0,
        None,
    )
    .await
    .expect("list_device_groups env_a");

    assert!(
        rows_a
            .iter()
            .all(|r| r.os_name.as_deref() != Some("Android")),
        "pixel_b_only is env_b-only and must not surface under One(env_a)"
    );
    let iphone = rows_a
        .iter()
        .find(|r| r.os_version.as_deref() == Some("17.4.1"))
        .expect("the iOS 17.4.1 group under env_a");
    assert_eq!(
        iphone.device_count, 1,
        "only iphone_a has env_a activity; iphone_b must not be counted"
    );
    // The scoped branch must read `ae.cnt` (the one env_a-tagged event this
    // test seeded), never `devices.events_count` (3 — iphone_a's app-wide,
    // all-environment lifetime counter from `bump_device` in
    // `seed_device_fleet`). Reading the durable column here is exactly the
    // cross-environment disclosure the `All`-vs-scoped split exists to
    // prevent, and it would still pass every other assertion in this test.
    assert_eq!(
        iphone.events_count, 1,
        "scoped events_count must come from the env-scoped LATERAL (1 seeded event), \
         not devices.events_count (3, the app-wide counter)"
    );

    drop(conn);
    db.cleanup().await;
}

/// `sessions_count`, `first_seen`, and `last_seen` all depend on data this
/// file's other three tests never seed: `seed_device_fleet` inserts zero
/// `sessions` rows, so `se.cnt`/`min_started`/`max_last_event` were entirely
/// uncovered, and none of the other tests here asserts on `first_seen`/
/// `last_seen` at all.
///
/// Every timestamp is pinned relative to `ids.pinned_now` (`t0`) rather than
/// compared with "earlier than"/"later than", so every expected aggregate
/// below is an exact value, not a range.
///
/// `since` is deliberately NOT `far_past()`: the point is to put one
/// session's `started_at` on each side of it, exercising
/// `count(*) FILTER (WHERE started_at >= $2)` — the reason that bound lives
/// in a `FILTER` on the session LATERAL rather than in that LATERAL's own
/// `WHERE`, unlike the outer `devices` predicate.
#[tokio::test]
async fn group_sessions_and_first_last_seen_use_pinned_timestamps() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = seed_device_fleet(&db).await;
    let mut conn = db.conn().await;

    // Truncated to microseconds — Postgres `timestamptz` has no finer
    // resolution, so a round-tripped `d.first_seen`/`d.last_seen` compared
    // against a nanosecond-precision Rust `DateTime` would fail `assert_eq!`
    // on the sub-microsecond remainder alone, independent of any real bug.
    let t0 = ids.pinned_now.trunc_subsecs(6);
    // `seed_device_fleet` bumps every device's `last_seen` to `t0 - 30s`;
    // `since` must stay <= that or the outer `devices` WHERE excludes them.
    let since = t0 - Duration::seconds(40);

    // iphone_a: one session BEFORE `since` (excluded from the FILTER'd
    // count, but not from the unbounded `min(started_at)`), one after.
    seed_group_session(
        &mut conn,
        ids.app_id,
        Some(ids.env_a),
        "grp-a-before",
        &ids.iphone_a,
        t0 - Duration::seconds(50),
    )
    .await;
    seed_group_session(
        &mut conn,
        ids.app_id,
        Some(ids.env_a),
        "grp-a-after",
        &ids.iphone_a,
        t0 - Duration::seconds(20),
    )
    .await;
    // iphone_b: two sessions, both after `since`.
    seed_group_session(
        &mut conn,
        ids.app_id,
        Some(ids.env_a),
        "grp-b-1",
        &ids.iphone_b,
        t0 - Duration::seconds(25),
    )
    .await;
    seed_group_session(
        &mut conn,
        ids.app_id,
        Some(ids.env_a),
        "grp-b-2",
        &ids.iphone_b,
        t0 - Duration::seconds(15),
    )
    .await;

    // Under `All`: sessions_count is unaffected by environment (no `env_sql`
    // filter on the session LATERAL), but first_seen/last_seen read the
    // durable `devices` columns — both iphone_a and iphone_b were bumped
    // exactly once, at `t0 - 30s` (`seed_device_fleet`) — and so are
    // untouched by any of the four session timestamps above.
    let rows_all =
        repo::list_device_groups(&mut conn, ReadScope::all(ids.app_id), since, 50, 0, None)
            .await
            .expect("list_device_groups All");
    let group_all = rows_all
        .iter()
        .find(|r| r.os_version.as_deref() == Some("17.4.1"))
        .expect("the iOS 17.4.1 group under All");
    assert_eq!(group_all.device_count, 2, "iphone_a and iphone_b");
    assert_eq!(
        group_all.sessions_count, 3,
        "grp-a-after (1) + grp-b-1 + grp-b-2 (2); grp-a-before is before `since`"
    );
    assert_eq!(
        group_all.first_seen,
        t0 - Duration::seconds(30),
        "All reads the durable devices.first_seen, not any session"
    );
    assert_eq!(
        group_all.last_seen,
        t0 - Duration::seconds(30),
        "All reads the durable devices.last_seen, not any session"
    );

    // Under `One(env_a)`: first_seen/last_seen instead derive from the
    // session LATERAL's unbounded min/max — which DOES see the
    // before-`since` session, even though it is excluded from sessions_count.
    let rows_scoped = repo::list_device_groups(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        since,
        50,
        0,
        None,
    )
    .await
    .expect("list_device_groups One(env_a)");
    let group_scoped = rows_scoped
        .iter()
        .find(|r| r.os_version.as_deref() == Some("17.4.1"))
        .expect("the iOS 17.4.1 group under One(env_a)");
    assert_eq!(
        group_scoped.device_count, 2,
        "iphone_a and iphone_b, both env_a-tagged"
    );
    assert_eq!(
        group_scoped.sessions_count, 3,
        "grp-a-before is excluded by the FILTER; the other 3 sessions are counted"
    );
    assert_eq!(
        group_scoped.first_seen,
        t0 - Duration::seconds(50),
        "grp-a-before's started_at (excluded from sessions_count) still sets first_seen — \
         the FILTER bounds the count, not the min/max"
    );
    assert_eq!(
        group_scoped.last_seen,
        t0 - Duration::seconds(15),
        "grp-b-2 has the latest last_event_at across the group"
    );

    drop(conn);
    db.cleanup().await;
}

/// The drill-down returns exactly the members of one group.
#[tokio::test]
async fn group_filter_returns_only_that_groups_devices() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = seed_device_fleet(&db).await;
    let mut conn = db.conn().await;

    let rows = repo::list_devices(
        &mut conn,
        ReadScope::all(ids.app_id),
        far_past(),
        50,
        0,
        None,
        Some(DeviceGroupKey {
            family: Some("iPhone"),
            model: Some("iPhone15,2"),
            os_name: Some("iOS"),
            os_version: Some("17.4.1"),
        }),
    )
    .await
    .expect("list_devices with group filter");

    let keys: Vec<&str> = rows.iter().map(|r| r.device_key.as_str()).collect();
    assert_eq!(keys.len(), 2, "exactly the two 17.4.1 devices");
    assert!(keys.contains(&ids.iphone_a.as_str()));
    assert!(keys.contains(&ids.iphone_b.as_str()));
    assert!(
        !keys.contains(&ids.iphone_older.as_str()),
        "17.4.0 is a different group"
    );

    drop(conn);
    db.cleanup().await;
}

/// The NULL case: an all-NULL group must drill down to its member, not to
/// nothing. `=` would return zero rows here; `IS NOT DISTINCT FROM` is what
/// makes this work.
#[tokio::test]
async fn group_filter_matches_the_all_null_group() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = seed_device_fleet(&db).await;
    let mut conn = db.conn().await;

    let rows = repo::list_devices(
        &mut conn,
        ReadScope::all(ids.app_id),
        far_past(),
        50,
        0,
        None,
        Some(DeviceGroupKey::default()),
    )
    .await
    .expect("list_devices with all-NULL group filter");

    assert_eq!(rows.len(), 1, "only the descriptor-less device");
    assert_eq!(rows[0].device_key, ids.unknown);

    drop(conn);
    db.cleanup().await;
}

/// Seeds `n` devices, each its own descriptor group (a distinct `family`,
/// fixed `model`/`os_name`/`os_version`), all sharing the exact same
/// `last_seen`. Standalone rather than a variant of `seed_device_fleet`:
/// that fixture's 4 tied groups turned out too narrow to reliably expose the
/// bug this file's pagination test guards against (see that test's doc
/// comment) — reproducing it needs a wide tie, not a specific one.
async fn seed_tied_groups(db: &TestDb, n: usize, at: DateTime<Utc>) -> Uuid {
    let mut conn = db.conn().await;
    let suffix = Uuid::new_v4().simple().to_string();
    let org = repo::create_org(&mut conn, "tie org", &format!("tie-org-{suffix}"))
        .await
        .expect("create org");
    let project = repo::create_project(
        &mut conn,
        org.id,
        "tie project",
        &format!("tie-proj-{suffix}"),
    )
    .await
    .expect("create project");
    let app = repo::create_app(
        &mut conn,
        project.id,
        "tie app",
        &format!("tie-app-{suffix}"),
        "web",
    )
    .await
    .expect("create app");

    for i in 0..n {
        let key = format!("tie-{suffix}-{i}");
        let family = format!("Fam-{i}");
        repo::bump_device(
            &mut conn,
            app.id,
            &key,
            Some(&family),
            Some("M1"),
            Some("OS"),
            Some("1"),
            None,
            None,
            None,
            at,
            1,
            0,
        )
        .await
        .expect("bump_device");
    }
    app.id
}

/// Paging must be stable across groups that tie exactly on `last_seen`.
///
/// Exact ties are not contrived: bulk/backfilled ingest and second-resolution
/// SDK timestamps both produce them routinely. Postgres does not order ties
/// on its own, and worse, its plan can differ between a small `OFFSET`
/// (bounded top-N heapsort) and a large one (full sort), so without a full
/// tiebreaker in the `ORDER BY`, paging with a `LIMIT` smaller than the total
/// can show the same group on two pages while never showing another at all.
///
/// `n = 30` tied groups, `page_size = 5` (6 pages): a first attempt at this
/// test with only `seed_device_fleet`'s 4 tied groups and `page_size = 2`
/// passed even with the `ORDER BY` tiebreaker reverted — that fixture was too
/// narrow to make the planner's tuplesort disagree with itself across
/// offsets. Widening the tie to 30 groups makes the divergence reliable: with
/// the tiebreaker reverted, this failed on every run below with the SAME
/// group appearing on two different pages while another was dropped
/// entirely; with the tiebreaker restored it passes every run. See this
/// module's report for the exact reverted-run output.
///
/// A `rows.len()` check alone would not catch this — a duplicate-and-omission
/// swap leaves the total row count across all pages unchanged. This test
/// instead collects every page's `family` values (each group's identity here)
/// and asserts the union is exactly the `n` seeded groups, with no repeats.
#[tokio::test]
async fn group_pagination_is_stable_across_last_seen_ties() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let n = 30usize;
    let at = Utc::now() - Duration::seconds(30);
    let app_id = seed_tied_groups(&db, n, at).await;
    let mut conn = db.conn().await;

    let page_size = 5;
    let mut seen: Vec<String> = Vec::new();
    let mut offset = 0i64;
    loop {
        let page = repo::list_device_groups(
            &mut conn,
            ReadScope::all(app_id),
            far_past(),
            page_size,
            offset,
            None,
        )
        .await
        .expect("list_device_groups page");
        if page.is_empty() {
            break;
        }
        for r in &page {
            seen.push(r.family.clone().expect("every seeded group has a family"));
        }
        offset += page_size;
        assert!(
            offset <= (n as i64) * 3,
            "pagination did not terminate — runaway loop: {seen:?}"
        );
    }

    assert_eq!(
        seen.len(),
        n,
        "must see exactly the {n} seeded groups across all pages, no duplicates or omissions: {seen:?}"
    );
    let mut unique = seen.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(
        unique.len(),
        n,
        "every group must appear exactly once across pages, not repeated on two pages: {seen:?}"
    );

    drop(conn);
    db.cleanup().await;
}

/// `None` must leave the pre-existing behaviour byte for byte.
#[tokio::test]
async fn no_group_filter_returns_every_device() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = seed_device_fleet(&db).await;
    let mut conn = db.conn().await;

    let rows = repo::list_devices(
        &mut conn,
        ReadScope::all(ids.app_id),
        far_past(),
        50,
        0,
        None,
        None,
    )
    .await
    .expect("list_devices unfiltered");
    assert_eq!(rows.len(), 5, "all five seeded devices");

    drop(conn);
    db.cleanup().await;
}
