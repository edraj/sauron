//! Paging stability for the offset-paged lists.
//!
//! The defect these exist to catch: OFFSET paging re-runs the query for each
//! page, so an ORDER BY that does not fully determine row order lets Postgres
//! return two tied rows in either order on either page. One row is served
//! twice and another is never served at all — and nothing in the response says
//! so. `last_seen` ties whenever two devices were last seen in the same
//! millisecond, which on a seeded fixture is all of them.
//!
//! Skips (does not fail) when `TEST_DATABASE_URL` is unset, mirroring
//! `device_groups.rs` and `keyset_plan.rs`. CI has no database service.

mod common;

use chrono::{DateTime, Duration, Utc};
use common::{far_past, seed_env, TestDb};
use diesel::sql_types::{Text, Timestamptz, Uuid as SqlUuid};
use diesel_async::RunQueryDsl;
use sauron_db::models::NewAnalyticsEvent;
use sauron_db::repo;
use sauron_db::repo::TimeWindow;
use sauron_db::repo::{DeviceGroupRow, DeviceRow, SortSpec, WorkflowAction};
use sauron_db::scope::Range;
use sauron_db::scope::ReadScope;
use serde_json::json;
use uuid::Uuid;

const PAGE: i64 = 7;
const ROWS: usize = 40;

/// `since_days` for [`repo::workflow_list`], whose window is
/// `started_at >= now() - make_interval(days => $2)` rather than a
/// `DateTime` — so it cannot take [`far_past`] like the others. 365 is the
/// route's own clamp ceiling.
const WORKFLOW_DAYS: i32 = 365;

/// Devices seeded under a SECOND app in the same database. Slice 2 of this
/// programme shipped a cross-tenant leak that a single-app fixture could not
/// see: with only one app, a predicate that escapes its `app_id` WHERE clause
/// returns exactly the same rows as one that does not, so the fixture itself
/// is blind to it. These rows exist to be absent.
const OTHER_APP_ROWS: usize = 6;

/// Bundles the connection and the two app ids the tests page over, so a test
/// body reads as "seed, page repeatedly, assert" rather than re-deriving the
/// scope at every call site. Modelled on `keyset_plan.rs`'s `Harness`.
struct Harness {
    // Never read directly again after `harness()` builds it — held only so its
    // `Drop` doesn't fire (and warn about a leaked database) until the test
    // calls `h.db.cleanup().await` itself.
    db: TestDb,
    conn: sauron_db::PgConn,
    app_id: Uuid,
    /// A second app under the same org. Nothing seeded here may ever appear in
    /// a listing of [`Harness::app_id`].
    other_app_id: Uuid,
    /// `workflows.environment_id` is `NOT NULL REFERENCES environments(id)`
    /// (migration `2026-07-29-000032`), so — unlike devices, persons, screens
    /// and sessions, all of which tolerate a NULL environment — the workflow
    /// fixture cannot be seeded without a real enrollment. One per app, so the
    /// cross-tenant assertion is over two genuinely separate environments too.
    env_id: Uuid,
    other_env_id: Uuid,
}

/// The device keys [`seed_devices_all_same_last_seen`] created, split by tenant.
struct Seeded {
    /// Under `h.app_id` — the app under test.
    app: Vec<String>,
    /// Under `h.other_app_id` — the rows that must never surface.
    other: Vec<String>,
}

/// `None` when no database is reachable — the `let Some(db) = TestDb::setup()
/// .await else { return }` convention every other DB-backed test in this crate
/// uses, folded into one early return here.
///
/// Seeds a bare org/project/two-apps of its own, deliberately NOT
/// `TestDb::seed_two_envs()`: the assertions below need to know the EXACT row
/// count for each app, and that fixture leaves devices of its own behind.
async fn harness() -> Option<Harness> {
    let db = TestDb::setup().await?;
    let mut conn = db.conn().await;
    let suffix = Uuid::new_v4().simple().to_string();

    let org = repo::create_org(&mut conn, "sort harness org", &format!("sort-org-{suffix}"))
        .await
        .expect("create org");
    let project = repo::create_project(
        &mut conn,
        org.id,
        "sort harness project",
        &format!("sort-project-{suffix}"),
    )
    .await
    .expect("create project");
    let app = repo::create_app(
        &mut conn,
        project.id,
        "sort harness app",
        &format!("sort-app-{suffix}"),
        "web",
    )
    .await
    .expect("create app");
    let other_app = repo::create_app(
        &mut conn,
        project.id,
        "sort harness other app",
        &format!("sort-other-app-{suffix}"),
        "web",
    )
    .await
    .expect("create the second app");

    let env_id = seed_env(
        &mut conn,
        project.id,
        app.id,
        "sort-harness-env",
        &format!("pk-sort-{suffix}"),
        true,
    )
    .await;
    let other_env_id = seed_env(
        &mut conn,
        project.id,
        other_app.id,
        "sort-harness-other-env",
        &format!("pk-sort-other-{suffix}"),
        true,
    )
    .await;

    Some(Harness {
        db,
        conn,
        app_id: app.id,
        other_app_id: other_app.id,
        env_id,
        other_env_id,
    })
}

/// `n` devices under `h.app_id` sharing one exact `last_seen`, plus
/// [`OTHER_APP_ROWS`] under `h.other_app_id` sharing the same instant.
///
/// Every device gets a distinct `events_count` (`i + 1`) and zero sessions, so
/// one fixture serves all three shapes the tests need: a column that ties
/// totally (`last_seen`), a LATERAL-computed column that ties totally
/// (`sessions_count`, 0 everywhere), and a column with a strict order
/// (`events_count`).
///
/// The other app's devices carry a family of their own so a leak is visible in
/// the grouped listing too, where distinct keys alone would fold into the same
/// group row and only inflate its `device_count`.
async fn seed_devices_all_same_last_seen(h: &mut Harness, n: usize) -> Seeded {
    let suffix = Uuid::new_v4().simple().to_string();
    // One instant for every device: `bump_device`'s upsert takes
    // `GREATEST(last_seen, EXCLUDED.last_seen)`, and each key is written once,
    // so this value is what lands in the column verbatim.
    let at = Utc::now() - Duration::seconds(30);

    let mut app = Vec::with_capacity(n);
    for i in 0..n {
        let key = format!("sort-{suffix}-a-{i:03}");
        seed_device(&mut h.conn, h.app_id, &key, "TenantA", at, (i + 1) as i64).await;
        app.push(key);
    }

    let mut other = Vec::with_capacity(OTHER_APP_ROWS);
    for i in 0..OTHER_APP_ROWS {
        let key = format!("sort-{suffix}-b-{i:03}");
        seed_device(&mut h.conn, h.other_app_id, &key, "TenantB", at, 1_000).await;
        other.push(key);
    }

    Seeded { app, other }
}

/// One `devices` row, written exactly once so no upsert reconciliation runs.
async fn seed_device(
    conn: &mut sauron_db::PgConn,
    app_id: Uuid,
    key: &str,
    family: &str,
    at: DateTime<Utc>,
    events: i64,
) {
    repo::bump_device(
        conn,
        app_id,
        key,
        Some(family),
        Some("SortModel"),
        Some("SortOS"),
        Some("1"),
        None,
        None,
        None,
        at,
        events,
        0,
    )
    .await
    .expect("bump_device");
}

/// The `SortSpec` `routes::devices::list` builds for a wire `sort` value, so
/// these tests page through the real specs rather than an ordering invented
/// here. `sauron-db` cannot depend on the API binary, so this mirrors that
/// route's `match` by hand — only the columns these tests use are mapped, and
/// anything else panics rather than silently ordering by something else.
///
/// The wire spelling is `parse_sort`'s: a bare name descends, a `-` prefix
/// ascends.
/// `parse_sort`'s wire spelling, shared by every `*_sort` mirror below: a bare
/// name descends, a `-` prefix ascends. Split out so the five mirrors cannot
/// disagree about the direction rule while each keeps its own column mapping.
fn split_spec(spec: &str) -> (&str, bool) {
    match spec.strip_prefix('-') {
        Some(rest) => (rest, false),
        None => (spec, true),
    }
}

fn device_sort(spec: &str) -> SortSpec {
    let (column, descending) = split_spec(spec);
    let column = match column {
        "last_seen" => "last_seen",
        "events_count" => "events_count",
        "sessions_count" => "sessions_count",
        other => panic!("`{other}` is not mapped here; add it when a test needs it"),
    };
    SortSpec {
        column,
        descending,
        tiebreak: "d.device_key",
        nulls_last: false,
    }
}

/// The default grouped ordering, matching `routes::devices::groups`.
///
/// The tiebreak is the WHOLE four-column grouping key and every column of it
/// is load-bearing — see
/// [`device_groups_page_stably_when_the_family_ties`], which fails if this is
/// shortened to `"d.family"`. The shipped constant is pinned to this same
/// string by `routes::devices::the_grouped_tiebreak_is_the_whole_four_column_key`
/// in the API crate; the pair is what closes the drift this hand-written
/// mirror would otherwise leave open.
fn group_sort() -> SortSpec {
    SortSpec {
        column: "last_seen",
        descending: true,
        tiebreak: "d.family, d.model, d.os_name, d.os_version",
        nulls_last: false,
    }
}

/// One page of `h.app_id`'s devices under `sort`.
async fn device_page(h: &mut Harness, sort: &str, limit: i64, offset: i64) -> Vec<DeviceRow> {
    repo::list_devices(
        &mut h.conn,
        ReadScope::all(h.app_id),
        TimeWindow::since("last_seen", far_past()),
        limit,
        offset,
        device_sort(sort),
        None,
        None,
    )
    .await
    .expect("list_devices page")
}

/// The `id` of every row on one page — the identity these tests page over.
async fn device_ids(h: &mut Harness, sort: &str, limit: i64, offset: i64) -> Vec<Uuid> {
    device_page(h, sort, limit, offset)
        .await
        .into_iter()
        .map(|r| r.id)
        .collect()
}

/// One page of `h.app_id`'s device groups under the default ordering.
async fn device_group_page(h: &mut Harness, limit: i64, offset: i64) -> Vec<DeviceGroupRow> {
    repo::list_device_groups(
        &mut h.conn,
        ReadScope::all(h.app_id),
        TimeWindow::since("last_seen", far_past()),
        limit,
        offset,
        group_sort(),
        None,
    )
    .await
    .expect("list_device_groups page")
}

/// Walk every page and assert the union is exactly the seeded set.
///
/// A `rows.len()` check alone would not catch the defect: a duplicate-and-
/// omission swap leaves the total unchanged. Both halves are needed.
async fn assert_pages_cover_every_row(label: &str, h: &mut Harness, sort: &str, expected: usize) {
    let mut seen = Vec::new();
    for page in 0..20 {
        let rows = device_ids(h, sort, PAGE, page * PAGE).await;
        if rows.is_empty() {
            break;
        }
        seen.extend(rows);
    }
    let mut unique = seen.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(
        unique.len(),
        seen.len(),
        "{label}: a row was served on more than one page ({} rows over all pages, {} distinct)",
        seen.len(),
        unique.len()
    );
    assert_eq!(
        seen.len(),
        expected,
        "{label}: paging did not reach every row"
    );
}

/// The default ordering, over a fixture where it decides nothing on its own.
///
/// Do NOT read a pass here as proof that the tiebreak works: measured with
/// `EXPLAIN` on this exact fixture, `ORDER BY last_seen DESC` alone is served
/// straight out of `devices_app_last_seen_idx` (`Index Scan`, no `Sort` node
/// at all), so Postgres returns tied rows in index order — identical on every
/// page, tiebreak or not. Adding `d.device_key` is what turns that into an
/// `Incremental Sort` with `Presorted Key: devices.last_seen`. This test
/// therefore covers "the default ordering reaches every row exactly once",
/// and [`devices_page_stably_when_a_computed_column_ties`] — whose column no
/// index can presort — is the one that actually fails when the tiebreak is
/// removed. Both are kept: the index that makes this one stable today is not
/// a promise, and a plan flip on a large table would land here first.
#[tokio::test]
async fn devices_page_stably_when_last_seen_ties() {
    let Some(mut h) = harness().await else { return };
    // Every device shares one `last_seen`, so the sort column alone decides
    // nothing and only the tiebreak makes the order total.
    seed_devices_all_same_last_seen(&mut h, ROWS).await;
    assert_pages_cover_every_row("devices by last_seen", &mut h, "last_seen", ROWS).await;

    h.db.cleanup().await;
}

/// The same property for a column computed in the OUTER query by a LATERAL
/// join, which the inner paging subquery cannot address at all. No device has
/// a session, so `sessions_count` is 0 for all of them — a total tie, exactly
/// as `last_seen` is above.
///
/// This is the discriminating one. No index can presort an aggregate, so the
/// plan carries a real `Sort` node whose output for tied rows differs between
/// a small `OFFSET` and a large one. Deleting `, {tiebreak} ASC` from
/// `SortSpec::order_by` fails this test — 39 distinct rows over 40 served —
/// on every run.
#[tokio::test]
async fn devices_page_stably_when_a_computed_column_ties() {
    let Some(mut h) = harness().await else { return };
    seed_devices_all_same_last_seen(&mut h, ROWS).await;
    assert_pages_cover_every_row("devices by sessions_count", &mut h, "sessions_count", ROWS).await;

    h.db.cleanup().await;
}

/// Stability is not enough on its own: an ORDER BY that ignored the requested
/// column entirely would pass every assertion above. `events_count` is seeded
/// strictly increasing, so the two directions have exactly one correct answer
/// each and must be exact reverses of one another.
#[tokio::test]
async fn devices_sort_by_a_computed_column_orders_both_ways() {
    let Some(mut h) = harness().await else { return };
    seed_devices_all_same_last_seen(&mut h, ROWS).await;

    let descending: Vec<i64> = device_page(&mut h, "events_count", ROWS as i64, 0)
        .await
        .into_iter()
        .map(|r| r.events_count)
        .collect();
    let expected_desc: Vec<i64> = (1..=ROWS as i64).rev().collect();
    assert_eq!(
        descending, expected_desc,
        "a bare sort name means descending"
    );

    let ascending: Vec<i64> = device_page(&mut h, "-events_count", ROWS as i64, 0)
        .await
        .into_iter()
        .map(|r| r.events_count)
        .collect();
    let expected_asc: Vec<i64> = (1..=ROWS as i64).collect();
    assert_eq!(ascending, expected_asc, "a `-` prefix means ascending");

    h.db.cleanup().await;
}

/// A listing of one app must never show another app's devices, flat or
/// grouped. See [`OTHER_APP_ROWS`] for why a single-app fixture cannot decide
/// this.
#[tokio::test]
async fn devices_never_leak_from_another_app() {
    let Some(mut h) = harness().await else { return };
    let seeded = seed_devices_all_same_last_seen(&mut h, ROWS).await;

    let mut keys = Vec::new();
    for page in 0..20 {
        let rows = device_page(&mut h, "last_seen", PAGE, page * PAGE).await;
        if rows.is_empty() {
            break;
        }
        keys.extend(rows.into_iter().map(|r| r.device_key));
    }
    assert_eq!(
        keys.len(),
        seeded.app.len(),
        "only this app's devices may be paged over"
    );
    for leaked in &seeded.other {
        assert!(
            !keys.contains(leaked),
            "device `{leaked}` belongs to another app and must not appear: {keys:?}"
        );
    }

    // The grouped listing shares the same qualifying-devices subquery, so it
    // needs the same proof. `TenantB` is the other app's family and folds into
    // a group of its own, which a leak would make visible here.
    let groups = device_group_page(&mut h, 50, 0).await;
    let families: Vec<Option<String>> = groups.iter().map(|g| g.family.clone()).collect();
    assert_eq!(
        families,
        vec![Some("TenantA".to_string())],
        "the grouped listing must show this app's one group and nothing else"
    );
    assert_eq!(
        groups[0].device_count, ROWS as i64,
        "the group must count this app's devices only"
    );

    h.db.cleanup().await;
}

/// `n` descriptor groups under `h.app_id` that share ONE `family`, one
/// `os_name`, one `os_version` and one exact `last_seen`, differing only in
/// `model`. Returns the models, which are the groups' only distinguishing
/// value and therefore their identity here.
///
/// Deliberately not `device_groups.rs`' `seed_tied_groups`, which gives every
/// group a `family` of its own: under that fixture `d.family` alone already
/// makes the ordering total, so it cannot tell the whole four-column grouping
/// key apart from its first column. Sharing the family is what makes the
/// remaining three load-bearing.
async fn seed_groups_sharing_one_family(h: &mut Harness, n: usize) -> Vec<String> {
    let suffix = Uuid::new_v4().simple().to_string();
    // One instant for every device, as `seed_devices_all_same_last_seen` does
    // and for the same reason: the sort column must decide nothing on its own.
    let at = Utc::now() - Duration::seconds(30);

    let mut models = Vec::with_capacity(n);
    for i in 0..n {
        let model = format!("SharedFamModel-{i:03}");
        repo::bump_device(
            &mut h.conn,
            h.app_id,
            &format!("grpfam-{suffix}-{i:03}"),
            Some("OneFamily"),
            Some(&model),
            Some("SortOS"),
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
        models.push(model);
    }
    models
}

/// The grouped list pages stably when the sort column AND the first column of
/// its tiebreak both tie — the property the flat list gets from
/// [`devices_page_stably_when_a_computed_column_ties`], for the one list in
/// this slice whose tiebreak is a four-column tuple rather than a single
/// unique column.
///
/// Why this exists when `device_groups.rs`
/// `group_pagination_is_stable_across_last_seen_ties` already pages the
/// grouped list: that fixture gives every group a distinct `family`, so its
/// ordering is total after the tiebreak's FIRST column and it still passes
/// with the other three deleted. It proves "the grouped list has *a*
/// tiebreak", not "the tiebreak is the whole grouping key". Here all `n`
/// groups share one family, one os_name and one os_version, so `last_seen`,
/// `d.family`, `d.os_name` and `d.os_version` all tie completely and only
/// `d.model` separates them.
///
/// `n = 30` at `GROUP_PAGE = 5` deliberately mirrors that test's shape, whose
/// own doc comment records the measurement behind it: a 4-group fixture did
/// NOT reproduce the divergence and 30 did. A wide tie is what makes the
/// planner's tuplesort disagree between a small `OFFSET` and a large one; a
/// narrow one hides the bug.
///
/// Shortening [`group_sort`]'s tiebreak to `"d.family"` fails this test. The
/// matching value check on the shipped constant lives in the API crate —
/// `routes::devices::the_grouped_tiebreak_is_the_whole_four_column_key` —
/// because [`group_sort`] is a hand-written mirror and `sauron-db` cannot
/// depend on the API binary to read `GROUP_TIEBREAK` directly.
#[tokio::test]
async fn device_groups_page_stably_when_the_family_ties() {
    let Some(mut h) = harness().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    const GROUPS: usize = 30;
    const GROUP_PAGE: i64 = 5;
    let seeded = seed_groups_sharing_one_family(&mut h, GROUPS).await;

    let mut seen: Vec<String> = Vec::new();
    for page in 0..20 {
        let rows = device_group_page(&mut h, GROUP_PAGE, page * GROUP_PAGE).await;
        if rows.is_empty() {
            break;
        }
        for r in &rows {
            assert_eq!(
                r.family.as_deref(),
                Some("OneFamily"),
                "the fixture seeds exactly one family; a different one means \
                 the harness leaked rows into this test"
            );
            seen.push(r.model.clone().expect("every seeded group has a model"));
        }
    }

    // Both halves, for `assert_pages_cover_every_row`'s reason: a
    // duplicate-and-omission swap — the defect the tiebreak prevents — leaves
    // the total unchanged, so only the distinct count sees it, and only the
    // total sees a truncated walk.
    let mut unique = seen.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(
        unique.len(),
        seen.len(),
        "a group was served on more than one page ({} rows over all pages, {} \
         distinct) — the grouped ORDER BY is not unique across groups that \
         share a family",
        seen.len(),
        unique.len()
    );
    assert_eq!(seen.len(), GROUPS, "paging did not reach every group");
    let mut expected = seeded;
    expected.sort();
    assert_eq!(unique, expected, "the pages served the wrong groups");

    h.db.cleanup().await;
}

// ===========================================================================
// Shared assertion for the four OFFSET lists below
// ===========================================================================

/// Assert that walking every page served each of `expected` exactly once, and
/// none of `forbidden`.
///
/// Three assertions rather than one, because each is blind to the others'
/// defect: a duplicate-and-omission swap (the tiebreak defect) leaves
/// `seen.len()` unchanged, a cross-tenant leak inflates the set without
/// duplicating anything, and a truncated walk omits without duplicating.
fn assert_covers_exactly(label: &str, seen: &[String], expected: &[String], forbidden: &[String]) {
    let mut unique = seen.to_vec();
    unique.sort();
    unique.dedup();
    assert_eq!(
        unique.len(),
        seen.len(),
        "{label}: a row was served on more than one page ({} rows over all pages, {} distinct)",
        seen.len(),
        unique.len()
    );
    for leaked in forbidden {
        assert!(
            !seen.contains(leaked),
            "{label}: `{leaked}` belongs to another app and must not appear: {seen:?}"
        );
    }
    let mut want = expected.to_vec();
    want.sort();
    assert_eq!(
        unique, want,
        "{label}: paging did not reach exactly the seeded rows"
    );
}

// ===========================================================================
// Users (`list_persons`)
// ===========================================================================

/// The `SortSpec` `routes::analytics::persons_list` builds for a wire `sort`
/// value. Mirrors that route's `match` by hand for the same reason
/// [`device_sort`] does — `sauron-db` cannot depend on the API binary — and
/// panics on an unmapped name rather than silently ordering by something else.
fn person_sort(spec: &str) -> SortSpec {
    let (column, descending) = split_spec(spec);
    let column = match column {
        "last_seen" => "last_seen",
        "first_seen" => "first_seen",
        "sessions_count" => "sessions_count",
        "events_count" => "events_count",
        other => panic!("`{other}` is not mapped here; add it when a test needs it"),
    };
    SortSpec {
        column,
        descending,
        // `UNIQUE (app_id, distinct_id)` on `event_users` (migration
        // `2026-07-13-000003`), and the query pages one app, so this is unique
        // across the result set.
        tiebreak: "eu.distinct_id",
        nulls_last: false,
    }
}

/// One `event_users` row with `first_seen`/`last_seen` written explicitly.
///
/// Raw SQL rather than `repo::touch_event_user`: that upsert stamps
/// `last_seen = now()`, and these fixtures need every row to share one EXACT
/// instant so the default ordering decides nothing on its own.
async fn seed_person(
    conn: &mut sauron_db::PgConn,
    app_id: Uuid,
    distinct_id: &str,
    first_seen: DateTime<Utc>,
    last_seen: DateTime<Utc>,
) {
    diesel::sql_query(
        "INSERT INTO event_users (app_id, distinct_id, properties, first_seen, last_seen) \
         VALUES ($1, $2, '{}'::jsonb, $3, $4)",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Text, _>(distinct_id.to_string())
    .bind::<Timestamptz, _>(first_seen)
    .bind::<Timestamptz, _>(last_seen)
    .execute(conn)
    .await
    .expect("insert event_users row");
}

/// `n` persons under `h.app_id` sharing one exact `last_seen`, plus
/// [`OTHER_APP_ROWS`] under `h.other_app_id` sharing the same instant.
///
/// No analytics events, sessions or errors: `list_persons` under
/// `ReadScope::all` emits no membership `EXISTS`, so a person with no signal
/// still lists — and `sessions_count` is then 0 for every row, a total tie on
/// an aggregate no index can presort.
async fn seed_persons_all_same_last_seen(h: &mut Harness, n: usize) -> Seeded {
    let suffix = Uuid::new_v4().simple().to_string();
    let at = Utc::now() - Duration::seconds(30);

    let mut app = Vec::with_capacity(n);
    for i in 0..n {
        let id = format!("sortp-{suffix}-a-{i:03}");
        seed_person(&mut h.conn, h.app_id, &id, at, at).await;
        app.push(id);
    }
    let mut other = Vec::with_capacity(OTHER_APP_ROWS);
    for i in 0..OTHER_APP_ROWS {
        let id = format!("sortp-{suffix}-b-{i:03}");
        seed_person(&mut h.conn, h.other_app_id, &id, at, at).await;
        other.push(id);
    }
    Seeded { app, other }
}

async fn person_page(h: &mut Harness, sort: &str, limit: i64, offset: i64) -> Vec<repo::PersonRow> {
    repo::list_persons(
        &mut h.conn,
        ReadScope::all(h.app_id),
        None,
        limit,
        offset,
        person_sort(sort),
        TimeWindow::since(
            "last_seen",
            chrono::Utc::now() - chrono::Duration::days(3650),
        ),
    )
    .await
    .expect("list_persons page")
}

/// Every `distinct_id` served across every page of `h.app_id`'s persons.
async fn all_person_ids(h: &mut Harness, sort: &str) -> Vec<String> {
    let mut seen = Vec::new();
    for page in 0..20 {
        let rows = person_page(h, sort, PAGE, page * PAGE).await;
        if rows.is_empty() {
            break;
        }
        seen.extend(rows.into_iter().map(|r| r.distinct_id));
    }
    seen
}

/// The default ordering over a fixture where `last_seen` decides nothing.
///
/// Unlike [`devices_page_stably_when_last_seen_ties`], this one DOES
/// discriminate the tiebreak — measured, see the task report's `EXPLAIN`
/// section. `event_users_app_last_seen_idx` cannot serve this ORDER BY,
/// because after Task 3 the ordering is applied on the OUTER query, above the
/// three `ae`/`ee`/`se` LATERALs, so the plan carries a real blocking `Sort`
/// whose output for tied rows differs between a small and a large `OFFSET`.
#[tokio::test]
async fn persons_page_stably_when_last_seen_ties() {
    let Some(mut h) = harness().await else { return };
    let seeded = seed_persons_all_same_last_seen(&mut h, ROWS).await;
    let seen = all_person_ids(&mut h, "last_seen").await;
    assert_covers_exactly("persons by last_seen", &seen, &seeded.app, &seeded.other);

    h.db.cleanup().await;
}

/// Stability alone would pass on an ORDER BY that ignored the requested
/// column entirely, so this pins that the column is honoured and that the two
/// directions are exact reverses.
///
/// Three persons whose `events_count` and `first_seen` orderings are
/// DIFFERENT from one another, so no single expected sequence can be produced
/// by sorting on the wrong one of the two.
#[tokio::test]
async fn persons_sort_by_a_computed_column_orders_both_ways() {
    let Some(mut h) = harness().await else { return };
    let suffix = Uuid::new_v4().simple().to_string();
    let now = Utc::now() - Duration::seconds(30);

    // (name, events, first_seen offset in minutes — larger is older)
    let probes = [("a", 3usize, 30i64), ("b", 1, 10), ("c", 2, 20)];
    let mut ids = Vec::new();
    for (n, events, older_by) in probes {
        let id = format!("sortpp-{suffix}-{n}");
        seed_person(
            &mut h.conn,
            h.app_id,
            &id,
            now - Duration::minutes(older_by),
            now,
        )
        .await;
        for _ in 0..events {
            seed_screen_event(&mut h.conn, h.app_id, &id, None, "probe", now).await;
        }
        ids.push(id);
    }
    let (a, b, c) = (ids[0].clone(), ids[1].clone(), ids[2].clone());

    let by_events: Vec<String> = person_page(&mut h, "events_count", 10, 0)
        .await
        .into_iter()
        .map(|r| r.distinct_id)
        .collect();
    assert_eq!(
        by_events,
        vec![a.clone(), c.clone(), b.clone()],
        "persons by events_count: a bare sort name means descending"
    );
    let ascending: Vec<String> = person_page(&mut h, "-events_count", 10, 0)
        .await
        .into_iter()
        .map(|r| r.distinct_id)
        .collect();
    assert_eq!(
        ascending,
        vec![b.clone(), c.clone(), a.clone()],
        "persons by -events_count: a `-` prefix means ascending"
    );

    // A DIFFERENT expected sequence, so an arm that mapped `first_seen` to
    // `events_count` (or the reverse) cannot satisfy both assertions.
    let by_first_seen: Vec<String> = person_page(&mut h, "first_seen", 10, 0)
        .await
        .into_iter()
        .map(|r| r.distinct_id)
        .collect();
    assert_eq!(
        by_first_seen,
        vec![b, c, a],
        "persons by first_seen: newest first"
    );

    h.db.cleanup().await;
}

// ===========================================================================
// Screens (`screen_list`)
// ===========================================================================

/// The `SortSpec` `routes::screens::list` builds. See [`person_sort`].
fn screen_sort(spec: &str) -> SortSpec {
    let (column, descending) = split_spec(spec);
    let column = match column {
        "views" => "views",
        "events" => "events",
        "users" => "users",
        other => panic!("`{other}` is not mapped here; add it when a test needs it"),
    };
    SortSpec {
        column,
        descending,
        // `keys` is `SELECT screen FROM ev UNION SELECT screen FROM ex` — a
        // `UNION`, which de-duplicates, so `k.screen` is one row per distinct
        // screen and therefore unique across the result set.
        tiebreak: "k.screen",
        nulls_last: false,
    }
}

/// One `analytics_events` row on `screen`, named `name`.
///
/// `name = "$screen"` is what `screen_ctes` counts as a *view*; anything else
/// counts as an *event*. Both fixtures below depend on that split.
async fn seed_screen_event(
    conn: &mut sauron_db::PgConn,
    app_id: Uuid,
    distinct_id: &str,
    screen: Option<&str>,
    name: &str,
    at: DateTime<Utc>,
) {
    repo::insert_analytics_event(
        conn,
        NewAnalyticsEvent {
            id: Uuid::new_v4(),
            app_id,
            environment_id: None,
            name: name.to_string(),
            distinct_id: distinct_id.to_string(),
            properties: json!({}),
            context: json!({}),
            session_id: None,
            release: None,
            ip_address: None,
            occurred_at: at,
            device_key: None,
            screen: screen.map(str::to_string),
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

/// `n` screens under `h.app_id` with exactly one view each — so `views` ties
/// across every row — plus [`OTHER_APP_ROWS`] screens under
/// `h.other_app_id`.
async fn seed_screens_all_same_views(h: &mut Harness, n: usize) -> Seeded {
    let suffix = Uuid::new_v4().simple().to_string();
    let at = Utc::now() - Duration::seconds(30);

    let mut app = Vec::with_capacity(n);
    for i in 0..n {
        let screen = format!("sorts-{suffix}-a-{i:03}");
        seed_screen_event(
            &mut h.conn,
            h.app_id,
            "sort-user",
            Some(&screen),
            "$screen",
            at,
        )
        .await;
        app.push(screen);
    }
    let mut other = Vec::with_capacity(OTHER_APP_ROWS);
    for i in 0..OTHER_APP_ROWS {
        let screen = format!("sorts-{suffix}-b-{i:03}");
        seed_screen_event(
            &mut h.conn,
            h.other_app_id,
            "sort-user",
            Some(&screen),
            "$screen",
            at,
        )
        .await;
        other.push(screen);
    }
    Seeded { app, other }
}

async fn screen_page(h: &mut Harness, sort: &str, limit: i64, offset: i64) -> Vec<repo::ScreenRow> {
    repo::screen_list(
        &mut h.conn,
        ReadScope::all(h.app_id),
        Range::since(far_past()),
        "%",
        limit,
        offset,
        screen_sort(sort),
    )
    .await
    .expect("screen_list page")
}

async fn all_screen_names(h: &mut Harness, sort: &str) -> Vec<String> {
    let mut seen = Vec::new();
    for page in 0..20 {
        let rows = screen_page(h, sort, PAGE, page * PAGE).await;
        if rows.is_empty() {
            break;
        }
        seen.extend(rows.into_iter().map(|r| r.screen));
    }
    seen
}

/// The default ordering (`views`) over a fixture where every screen has
/// exactly one view. `views` is `count(*) FILTER (...)` inside a CTE — an
/// aggregate no index can presort — so the plan carries a real `Sort` and this
/// test does discriminate the tiebreak.
#[tokio::test]
async fn screens_page_stably_when_views_tie() {
    let Some(mut h) = harness().await else { return };
    let seeded = seed_screens_all_same_views(&mut h, ROWS).await;
    let seen = all_screen_names(&mut h, "views").await;
    assert_covers_exactly("screens by views", &seen, &seeded.app, &seeded.other);

    h.db.cleanup().await;
}

/// Three screens whose `views`, `events` and `users` orderings are three
/// DIFFERENT sequences, so a `match` arm that mapped any one of them to
/// another cannot satisfy all three assertions.
///
/// | screen | `views` | `events` | `users` |
/// |---|---|---|---|
/// | `a` | 2 | 3 | 1 |
/// | `b` | 3 | 1 | 3 |
/// | `c` | 2 | 2 | 2 |
///
/// descending: `views` → `[b, a, c]` · `events` → `[a, c, b]` · `users` →
/// `[b, c, a]`. (`a` and `c` tie at 2 views and fall to the `k.screen ASC`
/// tiebreak, which puts `…-a` before `…-c`.)
///
/// `a`'s SECOND view is what makes the `users` assertion mean anything, and it
/// is load-bearing rather than incidental. Seeding one `$screen` event per
/// distinct user — the obvious fixture, and this test's first shape — makes
/// `views` (`count(*) FILTER (WHERE name='$screen')`) and `users`
/// (`count(DISTINCT distinct_id)`) equal on EVERY row, so a spec resolving the
/// wire name `users` to the column `views` produced the identical `[b, c, a]`
/// and the assertion could not fail. The extra view is by an existing user of
/// `a` (`sort-user-0`), so it moves `views` without moving `users`, and it is
/// a `$screen` event, so it does not move `events` either — `ev` splits the
/// two by `name<>'$screen'`.
///
/// The `events`/`users` counts are seeded from independent knobs for the same
/// family of reason: driving `users` off the tap events instead would cap it
/// at the tap count, which is exactly the mistake that made this test's first
/// draft assert a distribution the fixture could not produce.
#[tokio::test]
async fn screens_sort_by_a_computed_column_orders_both_ways() {
    let Some(mut h) = harness().await else { return };
    let suffix = Uuid::new_v4().simple().to_string();
    let at = Utc::now() - Duration::seconds(30);

    // (suffix, non-view "tap" events, distinct users, EXTRA views by user 0)
    let probes = [
        ("a", 3usize, 1usize, 1usize),
        ("b", 1, 3, 0),
        ("c", 2, 2, 0),
    ];
    let mut names = Vec::new();
    for (n, events, users, extra_views) in probes {
        let screen = format!("sortsp-{suffix}-{n}");
        // One `$screen` view per distinct user — `us` counts distinct
        // `distinct_id` across BOTH tables, so this is what sets `users`.
        for u in 0..users {
            let who = format!("sort-user-{u}");
            seed_screen_event(&mut h.conn, h.app_id, &who, Some(&screen), "$screen", at).await;
        }
        // Repeat views by an ALREADY-counted user: `views` moves, `users` does
        // not. See this test's doc comment — without these the two columns are
        // indistinguishable on this fixture.
        for _ in 0..extra_views {
            seed_screen_event(
                &mut h.conn,
                h.app_id,
                "sort-user-0",
                Some(&screen),
                "$screen",
                at,
            )
            .await;
        }
        // Taps all by user 0, so they move `events` without moving `users`.
        for _ in 0..events {
            seed_screen_event(
                &mut h.conn,
                h.app_id,
                "sort-user-0",
                Some(&screen),
                "tap",
                at,
            )
            .await;
        }
        names.push(screen);
    }
    let (a, b, c) = (names[0].clone(), names[1].clone(), names[2].clone());

    let by_events: Vec<String> = screen_page(&mut h, "events", 10, 0)
        .await
        .into_iter()
        .map(|r| r.screen)
        .collect();
    assert_eq!(
        by_events,
        vec![a.clone(), c.clone(), b.clone()],
        "screens by events: a bare sort name means descending"
    );
    let ascending: Vec<String> = screen_page(&mut h, "-events", 10, 0)
        .await
        .into_iter()
        .map(|r| r.screen)
        .collect();
    assert_eq!(
        ascending,
        vec![b.clone(), c.clone(), a.clone()],
        "screens by -events: a `-` prefix means ascending"
    );

    // `users` is a different CTE (`us`, a `count(DISTINCT ...)` over a
    // `UNION ALL`) and a different expected sequence — `b` has the most users
    // and the fewest events.
    let by_users: Vec<String> = screen_page(&mut h, "users", 10, 0)
        .await
        .into_iter()
        .map(|r| r.screen)
        .collect();
    assert_eq!(
        by_users,
        vec![b.clone(), c.clone(), a.clone()],
        "screens by users: most users first"
    );

    // `views` is a THIRD sequence, and asserting it is what proves the `users`
    // assertion above discriminates: `views` and `users` are both counts over
    // `$screen` rows and differ only because `a` was viewed twice by one user.
    // If a spec resolved `users` to the column `views` this pair of
    // assertions could not both hold.
    let by_views: Vec<String> = screen_page(&mut h, "views", 10, 0)
        .await
        .into_iter()
        .map(|r| r.screen)
        .collect();
    assert_eq!(
        by_views,
        vec![b, a, c],
        "screens by views: most views first, `a` and `c` tied at 2 and split \
         by the `k.screen ASC` tiebreak"
    );

    h.db.cleanup().await;
}

// ===========================================================================
// Sessions (`list_sessions`)
// ===========================================================================

/// The `SortSpec` `routes::sessions::list` builds. See [`person_sort`].
fn session_sort(spec: &str) -> SortSpec {
    let (column, descending) = split_spec(spec);
    let column = match column {
        "started_at" => "started_at",
        "events_count" => "events_count",
        "duration_ms" => "(last_event_at - started_at)",
        other => panic!("`{other}` is not mapped here; add it when a test needs it"),
    };
    SortSpec {
        column,
        descending,
        // `sessions.id` is the table's primary key.
        tiebreak: "id",
        nulls_last: false,
    }
}

/// `n` sessions under `h.app_id` sharing one exact `started_at`, plus
/// [`OTHER_APP_ROWS`] under `h.other_app_id`.
async fn seed_sessions_all_same_started_at(h: &mut Harness, n: usize) -> Seeded {
    let suffix = Uuid::new_v4().simple().to_string();
    let at = Utc::now() - Duration::seconds(30);

    let mut app = Vec::with_capacity(n);
    for i in 0..n {
        let key = format!("sortses-{suffix}-a-{i:03}");
        seed_session(&mut h.conn, h.app_id, &key, at, at, 1).await;
        app.push(key);
    }
    let mut other = Vec::with_capacity(OTHER_APP_ROWS);
    for i in 0..OTHER_APP_ROWS {
        let key = format!("sortses-{suffix}-b-{i:03}");
        seed_session(&mut h.conn, h.other_app_id, &key, at, at, 1).await;
        other.push(key);
    }
    Seeded { app, other }
}

/// One `sessions` row. `bump_session` writes `started_at` and `last_event_at`
/// from the same bind, so a session with a real duration needs a second call
/// at the later instant — `LEAST`/`GREATEST` on conflict then spread the two
/// apart.
async fn seed_session(
    conn: &mut sauron_db::PgConn,
    app_id: Uuid,
    session_id: &str,
    started_at: DateTime<Utc>,
    last_event_at: DateTime<Utc>,
    events: i64,
) {
    repo::bump_session(
        conn,
        app_id,
        session_id,
        Some("sort-user"),
        Some("sort-device"),
        started_at,
        &json!({}),
        None,
        None,
        None,
        events,
        0,
        0,
    )
    .await
    .expect("bump_session start");
    if last_event_at > started_at {
        repo::bump_session(
            conn,
            app_id,
            session_id,
            Some("sort-user"),
            Some("sort-device"),
            last_event_at,
            &json!({}),
            None,
            None,
            None,
            0,
            0,
            0,
        )
        .await
        .expect("bump_session end");
    }
}

async fn session_page(
    h: &mut Harness,
    sort: &str,
    limit: i64,
    offset: i64,
) -> Vec<sauron_db::models::Session> {
    repo::list_sessions(
        &mut h.conn,
        ReadScope::all(h.app_id),
        far_past(),
        limit,
        offset,
        session_sort(sort),
        None,
        None,
    )
    .await
    .expect("list_sessions page")
}

async fn all_session_ids(h: &mut Harness, sort: &str) -> Vec<String> {
    let mut seen = Vec::new();
    for page in 0..20 {
        let rows = session_page(h, sort, PAGE, page * PAGE).await;
        if rows.is_empty() {
            break;
        }
        seen.extend(rows.into_iter().map(|r| r.session_id));
    }
    seen
}

/// The default ordering (`started_at`) over a fixture where every session
/// starts at the same instant.
///
/// `sessions` has no `(app_id, started_at)` index — the only `started_at`
/// index is `sessions_app_device_started_idx`, which is partial and leads with
/// `device_key` — so this ordering plans a real `Sort` and the test
/// discriminates. Before Task 3 this list had NO tiebreaker at all
/// (`ORDER BY last_event_at DESC` alone).
#[tokio::test]
async fn sessions_page_stably_when_started_at_ties() {
    let Some(mut h) = harness().await else { return };
    let seeded = seed_sessions_all_same_started_at(&mut h, ROWS).await;
    let seen = all_session_ids(&mut h, "started_at").await;
    assert_covers_exactly("sessions by started_at", &seen, &seeded.app, &seeded.other);

    h.db.cleanup().await;
}

/// Three sessions whose `events_count` and `duration_ms` orderings differ, so
/// a `match` arm that mapped one to the other cannot satisfy both.
#[tokio::test]
async fn sessions_sort_by_a_computed_column_orders_both_ways() {
    let Some(mut h) = harness().await else { return };
    let suffix = Uuid::new_v4().simple().to_string();
    let at = Utc::now() - Duration::seconds(300);

    // (suffix, events, duration in seconds)
    let probes = [("a", 3i64, 1i64), ("b", 1, 3), ("c", 2, 2)];
    let mut ids = Vec::new();
    for (n, events, secs) in probes {
        let key = format!("sortsesp-{suffix}-{n}");
        seed_session(
            &mut h.conn,
            h.app_id,
            &key,
            at,
            at + Duration::seconds(secs),
            events,
        )
        .await;
        ids.push(key);
    }
    let (a, b, c) = (ids[0].clone(), ids[1].clone(), ids[2].clone());

    let by_events: Vec<String> = session_page(&mut h, "events_count", 10, 0)
        .await
        .into_iter()
        .map(|r| r.session_id)
        .collect();
    assert_eq!(
        by_events,
        vec![a.clone(), c.clone(), b.clone()],
        "sessions by events_count: a bare sort name means descending"
    );
    let ascending: Vec<String> = session_page(&mut h, "-events_count", 10, 0)
        .await
        .into_iter()
        .map(|r| r.session_id)
        .collect();
    assert_eq!(
        ascending,
        vec![b.clone(), c.clone(), a.clone()],
        "sessions by -events_count: a `-` prefix means ascending"
    );

    let by_duration: Vec<String> = session_page(&mut h, "duration_ms", 10, 0)
        .await
        .into_iter()
        .map(|r| r.session_id)
        .collect();
    assert_eq!(
        by_duration,
        vec![b, c, a],
        "sessions by duration_ms: longest first"
    );

    h.db.cleanup().await;
}

// ===========================================================================
// Workflows (`workflow_list`)
// ===========================================================================

/// The `SortSpec` `routes::workflows::list` builds. See [`person_sort`].
fn workflow_sort(spec: &str) -> SortSpec {
    let (column, descending) = split_spec(spec);
    let column = match column {
        "started" => "started",
        "completed" => "completed",
        "users" => "unique_users",
        other => panic!("`{other}` is not mapped here; add it when a test needs it"),
    };
    SortSpec {
        column,
        descending,
        // The query is `GROUP BY w.name`, so one row per name — `w.name` is
        // unique across the result set by construction.
        tiebreak: "w.name",
        nulls_last: false,
    }
}

/// One workflow run: a `Start`, optionally followed by an `End`.
#[allow(clippy::too_many_arguments)]
async fn seed_workflow_run(
    conn: &mut sauron_db::PgConn,
    app_id: Uuid,
    env_id: Uuid,
    name: &str,
    run_id: &str,
    distinct_id: &str,
    at: DateTime<Utc>,
    complete: bool,
) {
    repo::apply_workflow_lifecycle(
        conn,
        app_id,
        env_id,
        run_id,
        name,
        WorkflowAction::Start,
        None,
        None,
        Some(distinct_id),
        at,
    )
    .await
    .expect("workflow start");
    if complete {
        repo::apply_workflow_lifecycle(
            conn,
            app_id,
            env_id,
            run_id,
            name,
            WorkflowAction::End,
            None,
            None,
            Some(distinct_id),
            at + Duration::seconds(30),
        )
        .await
        .expect("workflow end");
    }
}

/// `n` workflow NAMES under `h.app_id` with exactly one run each — so
/// `started` is 1 for every row — plus [`OTHER_APP_ROWS`] names under
/// `h.other_app_id`.
async fn seed_workflows_all_same_started(h: &mut Harness, n: usize) -> Seeded {
    let suffix = Uuid::new_v4().simple().to_string();
    // Recent, so `workflow_effective_status_sql`'s 30-minute staleness rule
    // reads these as `active` rather than `abandoned`. Neither affects
    // `started`, but a fixture whose meaning depends on wall-clock drift is
    // one a reader cannot check.
    let at = Utc::now() - Duration::minutes(1);

    let mut app = Vec::with_capacity(n);
    for i in 0..n {
        let name = format!("sortwf-{suffix}-a-{i:03}");
        let run = format!("{name}-run");
        seed_workflow_run(
            &mut h.conn,
            h.app_id,
            h.env_id,
            &name,
            &run,
            "sort-user",
            at,
            false,
        )
        .await;
        app.push(name);
    }
    let mut other = Vec::with_capacity(OTHER_APP_ROWS);
    for i in 0..OTHER_APP_ROWS {
        let name = format!("sortwf-{suffix}-b-{i:03}");
        let run = format!("{name}-run");
        seed_workflow_run(
            &mut h.conn,
            h.other_app_id,
            h.other_env_id,
            &name,
            &run,
            "sort-user",
            at,
            false,
        )
        .await;
        other.push(name);
    }
    Seeded { app, other }
}

async fn workflow_page(
    h: &mut Harness,
    sort: &str,
    limit: i64,
    offset: i64,
) -> Vec<repo::WorkflowRow> {
    repo::workflow_list(
        &mut h.conn,
        ReadScope::all(h.app_id),
        Range::since(Utc::now() - chrono::Duration::days(i64::from(WORKFLOW_DAYS))),
        None,
        limit,
        offset,
        workflow_sort(sort),
    )
    .await
    .expect("workflow_list page")
}

async fn all_workflow_names(h: &mut Harness, sort: &str) -> Vec<String> {
    let mut seen = Vec::new();
    for page in 0..20 {
        let rows = workflow_page(h, sort, PAGE, page * PAGE).await;
        if rows.is_empty() {
            break;
        }
        seen.extend(rows.into_iter().map(|r| r.name));
    }
    seen
}

/// The default ordering (`started`) over a fixture where every name has
/// exactly one run. `started` is `COUNT(*)` over a `GROUP BY` — no index can
/// presort it — so the plan carries a real `Sort` and this test discriminates.
#[tokio::test]
async fn workflows_page_stably_when_started_ties() {
    let Some(mut h) = harness().await else { return };
    let seeded = seed_workflows_all_same_started(&mut h, ROWS).await;
    let seen = all_workflow_names(&mut h, "started").await;
    assert_covers_exactly("workflows by started", &seen, &seeded.app, &seeded.other);

    h.db.cleanup().await;
}

/// Three names whose `started`, `completed` and `users` orderings are each
/// different, so no one expected sequence can come from sorting on the wrong
/// one of the three.
///
/// | name | runs (`started`) | `completed` | `users` |
/// |---|---|---|---|
/// | `a` | 3 | 1 | 1 |
/// | `b` | 2 | 2 | 2 |
/// | `c` | 3 | 0 | 3 |
///
/// descending: `started` → a, c, b (a and c tie at 3, broken by name) ·
/// `completed` → b, a, c · `users` → c, b, a.
#[tokio::test]
async fn workflows_sort_by_a_computed_column_orders_both_ways() {
    let Some(mut h) = harness().await else { return };
    let suffix = Uuid::new_v4().simple().to_string();
    let at = Utc::now() - Duration::minutes(1);

    // (suffix, runs, completed runs, distinct users)
    let probes = [
        ("a", 3usize, 1usize, 1usize),
        ("b", 2, 2, 2),
        ("c", 3, 0, 3),
    ];
    let mut names = Vec::new();
    for (n, runs, completed, users) in probes {
        let name = format!("sortwfp-{suffix}-{n}");
        for i in 0..runs {
            seed_workflow_run(
                &mut h.conn,
                h.app_id,
                h.env_id,
                &name,
                &format!("{name}-run-{i}"),
                &format!("wf-user-{}", i % users),
                at,
                i < completed,
            )
            .await;
        }
        names.push(name);
    }
    let (a, b, c) = (names[0].clone(), names[1].clone(), names[2].clone());

    let by_completed: Vec<String> = workflow_page(&mut h, "completed", 10, 0)
        .await
        .into_iter()
        .map(|r| r.name)
        .collect();
    assert_eq!(
        by_completed,
        vec![b.clone(), a.clone(), c.clone()],
        "workflows by completed: a bare sort name means descending"
    );
    let ascending: Vec<String> = workflow_page(&mut h, "-completed", 10, 0)
        .await
        .into_iter()
        .map(|r| r.name)
        .collect();
    assert_eq!(
        ascending,
        vec![c.clone(), a.clone(), b.clone()],
        "workflows by -completed: a `-` prefix means ascending"
    );

    let by_users: Vec<String> = workflow_page(&mut h, "users", 10, 0)
        .await
        .into_iter()
        .map(|r| r.name)
        .collect();
    assert_eq!(
        by_users,
        vec![c, b, a],
        "workflows by users: most users first"
    );

    h.db.cleanup().await;
}
