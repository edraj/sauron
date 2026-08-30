//! `person_days` — the exact per-(person, environment, day) rollup behind
//! cohort retention, lifecycle and churn.
//!
//! See `docs/superpowers/specs/2026-08-28-retention-and-cohorts-design.md`.
//!
//! # Why this table's gate is NOT closed by `close_rollup_gate`
//!
//! `create_test_database` pushes `rollup_epoch` a decade out so tests exercise
//! the LEGACY (raw-scan) paths of the migration-71 aggregates. Retention has no
//! legacy path — there is nothing to fall back to — so the same treatment would
//! make `person_days::is_ready` false in every test, every assertion below
//! would compare empty against empty, and this file would pass having verified
//! nothing. `person_days_epoch` is therefore left at its migration stamp, which
//! predates every app a test creates, so the gate is OPEN by default here and
//! the tests that care about it re-pin it themselves.

mod common;

use common::TestDb;
use diesel::sql_types::Bool;
use diesel_async::RunQueryDsl;

#[derive(diesel::QueryableByName)]
struct BoolRow {
    #[diesel(sql_type = Bool)]
    present: bool,
}

#[tokio::test]
async fn migration_74_creates_person_days_with_env_sentinel_index() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut conn = db.conn().await;

    let r: BoolRow = diesel::sql_query("SELECT to_regclass('person_days') IS NOT NULL AS present")
        .get_result(&mut conn)
        .await
        .unwrap();
    assert!(r.present, "person_days table missing");

    // The unique index leads with the cohort probe (app, env, person) rather
    // than a day scan, and spells the nil-uuid sentinel rather than leaving a
    // bare nullable column in the key — NULL never equals NULL, so the bare
    // form would let one person accumulate unlimited unattributed rows whose
    // counters silently stop accumulating.
    let r: BoolRow = diesel::sql_query(
        "SELECT EXISTS (SELECT 1 FROM pg_indexes \
           WHERE tablename = 'person_days' \
             AND indexdef LIKE '%UNIQUE%' \
             AND indexdef LIKE '%distinct_id%' \
             AND indexdef LIKE '%00000000-0000-0000-0000-000000000000%') AS present",
    )
    .get_result(&mut conn)
    .await
    .unwrap();
    assert!(
        r.present,
        "person_days unique index missing, or it lacks the environment sentinel"
    );

    // The epoch is stamped BY THE MIGRATION. A stamp taken later lies about
    // every row that arrived in between, and that instant is not recoverable
    // after the fact (the migration-70 lesson).
    let r: BoolRow =
        diesel::sql_query("SELECT EXISTS (SELECT 1 FROM person_days_epoch) AS present")
            .get_result(&mut conn)
            .await
            .unwrap();
    assert!(
        r.present,
        "person_days_epoch was not stamped by the migration"
    );

    // The readiness marker table exists and starts empty: no app is claimed as
    // backfilled until a backfill actually runs.
    let r: BoolRow =
        diesel::sql_query("SELECT NOT EXISTS (SELECT 1 FROM person_days_backfill) AS present")
            .get_result(&mut conn)
            .await
            .unwrap();
    assert!(r.present, "person_days_backfill must start empty");

    db.cleanup().await;
}

#[derive(diesel::QueryableByName)]
struct CountRow {
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    n: i64,
}

/// The fold must agree with counting the raw rows directly.
///
/// This is the case that catches double-counting: a person active twice in one
/// day is ONE person-day, and the additive upsert must land both events on that
/// single row rather than inventing a second.
#[tokio::test]
async fn folded_person_days_equal_direct_count() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs().await;

    // Wind the watermark back so the fold's `received_at > wm` selection
    // actually reaches the fixture's rows, and hand it an `upto` in the future
    // so a fixture pinned to noon is still inside the window when this runs
    // before noon UTC. Getting either wrong yields an empty fold and a test
    // that compares 0 against 0 — which the `direct.n > 0` assertion below is
    // there to catch.
    diesel::sql_query("UPDATE rollup_watermarks SET watermark = now() - interval '3650 days'")
        .execute(&mut conn)
        .await
        .unwrap();

    let upto = chrono::Utc::now() + chrono::Duration::days(1);
    sauron_db::rollups::fold::fold_analytics(&mut conn, upto, 1000)
        .await
        .unwrap();

    let folded: CountRow =
        diesel::sql_query("SELECT count(*) AS n FROM person_days WHERE app_id = $1")
            .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
            .get_result(&mut conn)
            .await
            .unwrap();

    let direct: CountRow = diesel::sql_query(
        "SELECT count(*) AS n FROM ( \
           SELECT DISTINCT app_id, \
                  COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid) AS env, \
                  distinct_id, occurred_at::date \
           FROM analytics_events \
           WHERE app_id = $1 AND distinct_id <> '' \
         ) t",
    )
    .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
    .get_result(&mut conn)
    .await
    .unwrap();

    assert!(
        direct.n > 0,
        "the fixture seeded no analytics rows — this test would prove nothing"
    );
    assert_eq!(
        folded.n, direct.n,
        "folded person-days disagree with the raw rows behind them"
    );

    // And the counters, not just the row count: a double count shows here even
    // when the row count happens to match.
    let mismatched: CountRow = diesel::sql_query(
        "SELECT count(*) AS n FROM person_days p \
          WHERE p.app_id = $1 \
            AND p.events <> (SELECT count(*) FROM analytics_events a \
                              WHERE a.app_id = p.app_id \
                                AND a.distinct_id = p.distinct_id \
                                AND a.occurred_at::date = p.day \
                                AND a.environment_id IS NOT DISTINCT FROM p.environment_id)",
    )
    .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
    .get_result(&mut conn)
    .await
    .unwrap();
    assert_eq!(
        mismatched.n, 0,
        "a person-day's event counter disagrees with the raw rows behind it"
    );

    db.cleanup().await;
}

/// The gate must be CLOSED for an app that predates this table's epoch and has
/// no marker. Reporting ready there is how the API ends up answering 0%
/// retention confidently — the failure this whole epoch table exists to stop.
#[tokio::test]
async fn gate_is_closed_for_unbackfilled_app_and_opens_on_marker() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs().await;

    // Push the epoch past the app's creation, so only a marker can open it.
    diesel::sql_query("UPDATE person_days_epoch SET started_at = now() + interval '1 hour'")
        .execute(&mut conn)
        .await
        .unwrap();

    let ready = sauron_db::rollups::person_days::is_ready(&mut conn, ids.app_id)
        .await
        .unwrap();
    assert!(
        !ready,
        "an app predating the epoch with no marker must NOT report ready"
    );

    sauron_db::rollups::person_days::mark_all_backfilled(&mut conn)
        .await
        .unwrap();
    let ready = sauron_db::rollups::person_days::is_ready(&mut conn, ids.app_id)
        .await
        .unwrap();
    assert!(ready, "marker written — the app must now report ready");

    db.cleanup().await;
}

/// Regression guard for the trap named in this file's header.
///
/// `close_rollup_gate` pins `rollup_epoch` ten years out for every test
/// database. If that treatment is ever extended to `person_days_epoch`, this
/// assertion fails — which is the point. Retention has no legacy path, so a
/// closed gate here does not select a different code path; it makes every
/// retention assertion in this suite compare empty against empty and pass.
#[tokio::test]
async fn gate_is_open_by_default_for_apps_created_in_tests() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs().await;

    let ready = sauron_db::rollups::person_days::is_ready(&mut conn, ids.app_id)
        .await
        .unwrap();
    assert!(
        ready,
        "person_days_epoch must stay at its migration stamp so test-created \
         apps are implicitly ready — if this fails, someone added it to \
         close_rollup_gate and every retention test below is now vacuous"
    );

    db.cleanup().await;
}

/// Pruning drops what no endpoint can still ask about, and nothing else.
#[tokio::test]
async fn prune_drops_rows_past_the_horizon_and_keeps_the_rest() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs().await;

    for off in [-500i32, -10] {
        diesel::sql_query(
            "INSERT INTO person_days (app_id, environment_id, distinct_id, day, events) \
             VALUES ($1, NULL, 'p', current_date + $2, 1)",
        )
        .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
        .bind::<diesel::sql_types::Integer, _>(off)
        .execute(&mut conn)
        .await
        .unwrap();
    }

    sauron_db::rollups::person_days::prune(&mut conn, 400)
        .await
        .unwrap();

    let left: CountRow = diesel::sql_query(
        "SELECT count(*) AS n FROM person_days WHERE app_id = $1 AND distinct_id = 'p'",
    )
    .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
    .get_result(&mut conn)
    .await
    .unwrap();
    assert_eq!(
        left.n, 1,
        "the 500-day-old row goes, the 10-day-old row stays"
    );

    db.cleanup().await;
}

#[derive(diesel::QueryableByName, Debug)]
struct PersonDayRow {
    #[diesel(sql_type = diesel::sql_types::Date)]
    day: chrono::NaiveDate,
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    events: i64,
}

/// A guest active on two days identifies into a person already active on one
/// of them. The survivor must hold the UNION of the day sets — three rows
/// collapsing to two — with the shared day's counters summed.
///
/// Without this hook every `identify()` inflates retention, and guest-then-
/// identify is the normal path, not an edge case. A plain `UPDATE distinct_id`
/// would not merely miscount: it would raise a unique violation on exactly the
/// overlapping day, which is the common case, since an identify typically
/// happens on a day the guest was already active.
#[tokio::test]
async fn identity_merge_unions_person_days() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs().await;

    let d1 = (chrono::Utc::now() - chrono::Duration::days(2)).date_naive();
    let d2 = (chrono::Utc::now() - chrono::Duration::days(1)).date_naive();

    for (who, day, events) in [
        ("guest_abc", d1, 3i64),
        ("guest_abc", d2, 2),
        ("person_1", d2, 5),
    ] {
        diesel::sql_query(
            "INSERT INTO person_days (app_id, environment_id, distinct_id, day, events) \
             VALUES ($1, NULL, $2, $3, $4)",
        )
        .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
        .bind::<diesel::sql_types::Text, _>(who)
        .bind::<diesel::sql_types::Date, _>(day)
        .bind::<diesel::sql_types::BigInt, _>(events)
        .execute(&mut conn)
        .await
        .unwrap();
    }

    sauron_db::identity_merge::fold_rollups(&mut conn, ids.app_id, "guest_abc", "person_1", 7)
        .await
        .unwrap();

    let rows: Vec<PersonDayRow> = diesel::sql_query(
        "SELECT day, events FROM person_days \
          WHERE app_id = $1 AND distinct_id = 'person_1' ORDER BY day",
    )
    .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
    .get_results(&mut conn)
    .await
    .unwrap();

    assert_eq!(
        rows.len(),
        2,
        "the day sets must UNION to two, not sum to three"
    );
    assert_eq!(rows[0].day, d1);
    assert_eq!(rows[0].events, 3, "the guest-only day carries over intact");
    assert_eq!(rows[1].day, d2);
    assert_eq!(rows[1].events, 7, "the shared day sums 2 + 5");

    let leftover: CountRow = diesel::sql_query(
        "SELECT count(*) AS n FROM person_days WHERE app_id = $1 AND distinct_id = 'guest_abc'",
    )
    .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
    .get_result(&mut conn)
    .await
    .unwrap();
    assert_eq!(leftover.n, 0, "alias rows must not survive the merge");

    db.cleanup().await;
}

/// Erasure must reach `person_days`. It is keyed by `distinct_id`, so it is
/// personal data in its own right, and a surviving row would still describe
/// which days an erased person was active.
///
/// Both branches of `apply_recomputed_rollup` are exercised: nothing survives
/// (the person is erased outright) and something survives (a time-ranged purge,
/// where the remaining days must be re-derived rather than blanket-deleted).
#[tokio::test]
async fn erasure_removes_person_days_and_a_partial_purge_recomputes_them() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs().await;

    // --- nothing survives: the rows go ------------------------------------
    diesel::sql_query(
        "INSERT INTO person_days (app_id, environment_id, distinct_id, day, events) \
         VALUES ($1, NULL, 'erase_me', current_date, 4)",
    )
    .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
    .execute(&mut conn)
    .await
    .unwrap();

    sauron_db::purge::apply_recomputed_rollup(
        &mut conn,
        sauron_purge::PurgeKind::Persons,
        ids.app_id,
        "erase_me",
        sauron_purge::recompute::Counts::EMPTY,
    )
    .await
    .unwrap();

    let left: CountRow = diesel::sql_query(
        "SELECT count(*) AS n FROM person_days WHERE app_id = $1 AND distinct_id = 'erase_me'",
    )
    .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
    .get_result(&mut conn)
    .await
    .unwrap();
    assert_eq!(left.n, 0, "person_days survived a full erasure");

    // --- something survives: the rows are re-derived ----------------------
    // A stale person-day claiming activity on a day whose raw rows were purged
    // is the same misleading residue, one row down.
    let survivor = "partial_purge_person";
    diesel::sql_query(
        "INSERT INTO analytics_events (id, app_id, environment_id, distinct_id, name, occurred_at, received_at) \
         VALUES (gen_random_uuid(), $1, NULL, $2, 'view', now(), now())",
    )
    .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
    .bind::<diesel::sql_types::Text, _>(survivor)
    .execute(&mut conn)
    .await
    .unwrap();
    // A person-day for a day with NO surviving raw rows — the purge took them.
    diesel::sql_query(
        "INSERT INTO person_days (app_id, environment_id, distinct_id, day, events) \
         VALUES ($1, NULL, $2, current_date - 30, 9)",
    )
    .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
    .bind::<diesel::sql_types::Text, _>(survivor)
    .execute(&mut conn)
    .await
    .unwrap();

    let mut counts = sauron_purge::recompute::Counts::EMPTY;
    counts.evidence = 1;
    counts.events = 1;
    counts.first = Some(chrono::Utc::now());
    counts.last = Some(chrono::Utc::now());
    sauron_db::purge::apply_recomputed_rollup(
        &mut conn,
        sauron_purge::PurgeKind::Persons,
        ids.app_id,
        survivor,
        counts,
    )
    .await
    .unwrap();

    let rows: Vec<PersonDayRow> = diesel::sql_query(
        "SELECT day, events FROM person_days \
          WHERE app_id = $1 AND distinct_id = $2 ORDER BY day",
    )
    .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
    .bind::<diesel::sql_types::Text, _>(survivor)
    .get_results(&mut conn)
    .await
    .unwrap();
    assert_eq!(
        rows.len(),
        1,
        "the purged day must be gone and today's re-derived — got {rows:?} rows"
    );
    assert_eq!(rows[0].day, chrono::Utc::now().date_naive());
    assert_eq!(
        rows[0].events, 1,
        "re-derived from the one surviving raw row"
    );

    db.cleanup().await;
}

/// The backfill owns `(-inf, cutoff]` and the live fold owns `(cutoff, inf)`.
///
/// Their disjointness is a property of the CUTOFF being the instant the live
/// path started counting — which is why the backfill reads `person_days_epoch`
/// and not `Utc::now()`. A day straddling both halves must be counted ONCE.
#[tokio::test]
async fn backfill_is_additive_and_does_not_double_count() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs().await;

    // Pretend the live path started an hour ago: the fixture's rows are older,
    // so they belong to the backfill's half of history.
    diesel::sql_query("UPDATE person_days_epoch SET started_at = now() - interval '1 hour'")
        .execute(&mut conn)
        .await
        .unwrap();
    diesel::sql_query(
        "UPDATE rollup_watermarks SET watermark = (SELECT started_at FROM person_days_epoch)",
    )
    .execute(&mut conn)
    .await
    .unwrap();

    let upto = chrono::Utc::now() + chrono::Duration::days(1);
    sauron_db::rollups::fold::fold_analytics(&mut conn, upto, 1000)
        .await
        .unwrap();
    sauron_db::rollups::fold::fold_errors(&mut conn, upto)
        .await
        .unwrap();
    sauron_db::person_days_backfill::backfill_all(db.pool())
        .await
        .unwrap();

    let seeded: CountRow =
        diesel::sql_query("SELECT count(*) AS n FROM person_days WHERE app_id = $1")
            .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
            .get_result(&mut conn)
            .await
            .unwrap();
    assert!(
        seeded.n > 0,
        "no person-days at all — this test would prove nothing"
    );

    // The unique index makes duplicate ROWS structurally impossible, so assert
    // the COUNTERS, which is where a double count actually shows.
    let bad: CountRow = diesel::sql_query(
        "SELECT count(*) AS n FROM person_days p \
          WHERE p.app_id = $1 \
            AND p.events <> (SELECT count(*) FROM analytics_events a \
                              WHERE a.app_id = p.app_id AND a.distinct_id = p.distinct_id \
                                AND a.occurred_at::date = p.day \
                                AND a.environment_id IS NOT DISTINCT FROM p.environment_id)",
    )
    .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
    .get_result(&mut conn)
    .await
    .unwrap();
    assert_eq!(
        bad.n, 0,
        "a person-day's event counter disagrees with the raw rows — the two \
         halves of history overlapped"
    );

    let ready = sauron_db::rollups::person_days::is_ready(&mut conn, ids.app_id)
        .await
        .unwrap();
    assert!(ready, "the backfill must write its readiness marker");

    db.cleanup().await;
}

/// Seed a person's cohort anchor and a set of active days.
async fn seed_person(
    conn: &mut sauron_db::PgConn,
    app_id: uuid::Uuid,
    who: &str,
    first: chrono::NaiveDate,
    days: &[chrono::NaiveDate],
) {
    diesel::sql_query(
        "INSERT INTO event_user_environments \
           (app_id, distinct_id, environment_id, first_seen, last_seen) \
         VALUES ($1, $2, NULL, $3, $3)",
    )
    .bind::<diesel::sql_types::Uuid, _>(app_id)
    .bind::<diesel::sql_types::Text, _>(who)
    .bind::<diesel::sql_types::Timestamptz, _>(first.and_hms_opt(0, 0, 0).unwrap().and_utc())
    .execute(conn)
    .await
    .unwrap();
    for d in days {
        diesel::sql_query(
            "INSERT INTO person_days (app_id, environment_id, distinct_id, day, events) \
             VALUES ($1, NULL, $2, $3, 1) \
             ON CONFLICT (app_id, COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid), distinct_id, day) \
             DO UPDATE SET events = person_days.events + 1",
        )
        .bind::<diesel::sql_types::Uuid, _>(app_id)
        .bind::<diesel::sql_types::Text, _>(who)
        .bind::<diesel::sql_types::Date, _>(*d)
        .execute(conn)
        .await
        .unwrap();
    }
}

/// Two people join on day 0; one returns on day 2, the other never does.
///
/// Period 0 must equal the cohort size, period 2 must be 1, and period 1 —
/// which nobody was active in — must be ABSENT from the rows rather than
/// present as a zero. That distinction is what lets the API tell a true zero
/// apart from a period that has not elapsed.
#[tokio::test]
async fn retention_grid_counts_returners_by_period() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs().await;

    let d0 = (chrono::Utc::now() - chrono::Duration::days(6)).date_naive();
    seed_person(
        &mut conn,
        ids.app_id,
        "r1",
        d0,
        &[d0, d0 + chrono::Duration::days(2)],
    )
    .await;
    seed_person(&mut conn, ids.app_id, "r2", d0, &[d0]).await;

    let rows = sauron_db::retention::retention_grid(
        &mut conn,
        sauron_db::scope::ReadScope::all(ids.app_id),
        sauron_db::retention::Granularity::Day,
        d0,
        d0 + chrono::Duration::days(1),
        7,
        sauron_db::retention::ErrorSplit::All,
        sauron_db::retention::Audience::Everyone,
    )
    .await
    .unwrap();

    let p0 = rows
        .iter()
        .find(|r| r.period == 0)
        .expect("period 0 missing");
    assert_eq!(p0.size, 2, "cohort size");
    assert_eq!(
        p0.users, 2,
        "everyone is active in period 0 by construction"
    );
    let p2 = rows
        .iter()
        .find(|r| r.period == 2)
        .expect("period 2 missing");
    assert_eq!(p2.users, 1, "only r1 returned on day 2");
    assert!(
        rows.iter().all(|r| r.period != 1),
        "period 1 had no returners and must not be emitted as a zero"
    );

    db.cleanup().await;
}

/// A person active in TWO environments on the same day is one person, not two.
///
/// `count(*)` here would report 2/1 = 200% retention. The unscoped read is the
/// only place this shows, which is why it gets its own case.
#[tokio::test]
async fn retention_grid_does_not_double_count_across_environments() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs().await;

    let d0 = (chrono::Utc::now() - chrono::Duration::days(3)).date_naive();
    seed_person(&mut conn, ids.app_id, "multi", d0, &[d0]).await;
    // The same person, same day, second environment.
    diesel::sql_query(
        "INSERT INTO person_days (app_id, environment_id, distinct_id, day, events) \
         VALUES ($1, $2, 'multi', $3, 1)",
    )
    .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
    .bind::<diesel::sql_types::Uuid, _>(ids.env_a)
    .bind::<diesel::sql_types::Date, _>(d0)
    .execute(&mut conn)
    .await
    .unwrap();

    let rows = sauron_db::retention::retention_grid(
        &mut conn,
        sauron_db::scope::ReadScope::all(ids.app_id),
        sauron_db::retention::Granularity::Day,
        d0,
        d0 + chrono::Duration::days(1),
        3,
        sauron_db::retention::ErrorSplit::All,
        sauron_db::retention::Audience::Everyone,
    )
    .await
    .unwrap();

    let p0 = rows
        .iter()
        .find(|r| r.period == 0)
        .expect("period 0 missing");
    assert_eq!(p0.size, 1, "one person, however many environments");
    assert_eq!(
        p0.users, 1,
        "count(DISTINCT distinct_id), not count(*) — otherwise this reads 200%"
    );

    db.cleanup().await;
}

/// The three active classes must PARTITION today's actives — each person is
/// counted once and only once — and dormant is counted separately.
#[tokio::test]
async fn lifecycle_classifies_each_person_exactly_once() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs().await;

    let today = chrono::Utc::now().date_naive();
    let d = |o: i64| today + chrono::Duration::days(o);

    seed_person(&mut conn, ids.app_id, "u_new", today, &[d(0)]).await;
    seed_person(&mut conn, ids.app_id, "u_ret", d(-1), &[d(-1), d(0)]).await;
    seed_person(&mut conn, ids.app_id, "u_res", d(-3), &[d(-3), d(0)]).await;
    seed_person(&mut conn, ids.app_id, "u_dorm", d(-1), &[d(-1)]).await;

    let pts = sauron_db::retention::lifecycle(
        &mut conn,
        sauron_db::scope::ReadScope::all(ids.app_id),
        sauron_db::retention::Granularity::Day,
        d(-4),
        d(1),
        sauron_db::retention::Audience::Everyone,
    )
    .await
    .unwrap();

    let t = pts
        .iter()
        .find(|p| p.start == today)
        .unwrap_or_else(|| panic!("today missing from {pts:?}"));
    assert_eq!(t.new_users, 1, "u_new");
    assert_eq!(t.returning_users, 1, "u_ret");
    assert_eq!(t.resurrected_users, 1, "u_res");
    assert_eq!(
        t.dormant_users, 1,
        "u_dorm was active yesterday and is silent today"
    );
    assert_eq!(
        t.new_users + t.returning_users + t.resurrected_users,
        3,
        "the three active classes partition today's actives — no double count"
    );

    // The bucket in which EVERYBODY was silent must still be emitted, carrying
    // its dormant count. u_res was active on d(-3) and silent on d(-2); before
    // the generate_series rewrite that bucket had no row at all, so the one
    // period showing a total churn cliff rendered as a gap in the chart.
    let gap = pts
        .iter()
        .find(|p| p.start == d(-2))
        .expect("the all-dormant bucket must be emitted, not skipped");
    assert_eq!(
        gap.new_users + gap.returning_users + gap.resurrected_users,
        0
    );
    assert_eq!(gap.dormant_users, 1, "u_res went silent entering d(-2)");

    // And the primer bucket is dropped: it exists only to classify its
    // successor and cannot classify itself.
    assert_eq!(
        pts.len(),
        4,
        "buckets d(-3)..d(0); the d(-4) primer is dropped"
    );
    assert!(pts.iter().all(|p| p.start != d(-4)));

    db.cleanup().await;
}

/// Churn lists the silent, and excludes anyone still active.
#[tokio::test]
async fn churn_lists_only_the_silent() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs().await;

    let today = chrono::Utc::now().date_naive();
    seed_person(&mut conn, ids.app_id, "still_here", today, &[today]).await;
    seed_person(
        &mut conn,
        ids.app_id,
        "gone_quiet",
        today - chrono::Duration::days(40),
        &[today - chrono::Duration::days(40)],
    )
    .await;

    // A NON-ZERO counter, deliberately. `sum(bigint)` returns numeric, and a
    // numeric zero fits in the eight bytes Diesel's i64 decoder accepts — so a
    // fixture left at the default 0 passes while the real endpoint 500s.
    diesel::sql_query(
        "UPDATE event_user_environments SET events_count = 4321 \
          WHERE app_id = $1 AND distinct_id = 'gone_quiet'",
    )
    .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
    .execute(&mut conn)
    .await
    .unwrap();

    let rows = sauron_db::retention::churn(
        &mut conn,
        sauron_db::scope::ReadScope::all(ids.app_id),
        30,
        sauron_db::retention::ChurnSort::LastSeen,
        true,
        None,
        50,
        sauron_db::retention::Audience::Everyone,
    )
    .await
    .unwrap();

    let ours: Vec<&str> = rows
        .iter()
        .map(|r| r.distinct_id.as_str())
        .filter(|d| *d == "still_here" || *d == "gone_quiet")
        .collect();
    assert_eq!(ours, vec!["gone_quiet"], "only the silent person is listed");
    let quiet = rows
        .iter()
        .find(|r| r.distinct_id == "gone_quiet")
        .expect("listed above");
    assert_eq!(
        quiet.events_count, 4321,
        "the counter must decode — this is the assertion that catches the \
         numeric-vs-bigint sum"
    );

    db.cleanup().await;
}

/// Every query must work ENVIRONMENT-SCOPED, not just unscoped.
///
/// `EnvFilter::One` renders as `= $n` while `Subset` renders as `= ANY($n)`, so
/// the two need different BIND TYPES. Binding an array for `One` is a runtime
/// 500 — `operator does not exist: uuid = uuid[]` — that no unscoped test can
/// reach, because `EnvFilter::All` emits no fragment and consumes no bind at
/// all. Every unscoped assertion in this file passed while all three endpoints
/// were broken for the environment picker, which is the only way most people
/// use them.
#[tokio::test]
async fn every_query_works_environment_scoped() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs().await;

    let d0 = (chrono::Utc::now() - chrono::Duration::days(3)).date_naive();
    diesel::sql_query(
        "INSERT INTO event_user_environments \
           (app_id, distinct_id, environment_id, first_seen, last_seen, events_count) \
         VALUES ($1, 'scoped_u', $2, $3, $3, 7)",
    )
    .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
    .bind::<diesel::sql_types::Uuid, _>(ids.env_a)
    .bind::<diesel::sql_types::Timestamptz, _>(d0.and_hms_opt(0, 0, 0).unwrap().and_utc())
    .execute(&mut conn)
    .await
    .unwrap();
    diesel::sql_query(
        "INSERT INTO person_days (app_id, environment_id, distinct_id, day, events) \
         VALUES ($1, $2, 'scoped_u', $3, 1)",
    )
    .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
    .bind::<diesel::sql_types::Uuid, _>(ids.env_a)
    .bind::<diesel::sql_types::Date, _>(d0)
    .execute(&mut conn)
    .await
    .unwrap();

    use sauron_db::scope::{EnvFilter, ReadScope};
    let scoped = ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a));
    let subset = ReadScope::new(ids.app_id, EnvFilter::Subset(vec![ids.env_a, ids.env_b]));
    let unattributed = ReadScope::new(ids.app_id, EnvFilter::Unattributed);

    // All four EnvFilter variants must at minimum EXECUTE. Three of them were
    // untested before this case and one of them 500'd.
    for (name, sc) in [
        ("One", scoped.clone()),
        ("Subset", subset),
        ("Unattributed", unattributed),
        ("All", ReadScope::all(ids.app_id)),
    ] {
        sauron_db::retention::retention_grid(
            &mut conn,
            sc.clone(),
            sauron_db::retention::Granularity::Day,
            d0,
            d0 + chrono::Duration::days(1),
            5,
            sauron_db::retention::ErrorSplit::All,
            sauron_db::retention::Audience::Everyone,
        )
        .await
        .unwrap_or_else(|e| panic!("retention_grid failed for EnvFilter::{name}: {e}"));

        sauron_db::retention::lifecycle(
            &mut conn,
            sc.clone(),
            sauron_db::retention::Granularity::Day,
            d0 - chrono::Duration::days(2),
            d0 + chrono::Duration::days(2),
            sauron_db::retention::Audience::Everyone,
        )
        .await
        .unwrap_or_else(|e| panic!("lifecycle failed for EnvFilter::{name}: {e}"));

        sauron_db::retention::churn(
            &mut conn,
            sc.clone(),
            1,
            sauron_db::retention::ChurnSort::LastSeen,
            true,
            None,
            10,
            sauron_db::retention::Audience::Everyone,
        )
        .await
        .unwrap_or_else(|e| panic!("churn failed for EnvFilter::{name}: {e}"));

        // With a TIME cursor, which shifts every later bind index by one —
        // and with a COUNT cursor on an ascending counter sort, so both
        // cursor bind types and both directions execute under every filter.
        sauron_db::retention::churn(
            &mut conn,
            sc.clone(),
            1,
            sauron_db::retention::ChurnSort::LastSeen,
            true,
            Some(sauron_db::retention::ChurnCursor::Time(
                chrono::Utc::now(),
                "zzz".into(),
            )),
            10,
            sauron_db::retention::Audience::Everyone,
        )
        .await
        .unwrap_or_else(|e| panic!("churn+time-cursor failed for EnvFilter::{name}: {e}"));
        sauron_db::retention::churn(
            &mut conn,
            sc,
            1,
            sauron_db::retention::ChurnSort::Events,
            false,
            Some(sauron_db::retention::ChurnCursor::Count(0, String::new())),
            10,
            sauron_db::retention::Audience::Everyone,
        )
        .await
        .unwrap_or_else(|e| panic!("churn+count-cursor failed for EnvFilter::{name}: {e}"));
    }

    // Scoping must actually FILTER, not merely execute.
    let rows = sauron_db::retention::retention_grid(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
        sauron_db::retention::Granularity::Day,
        d0,
        d0 + chrono::Duration::days(1),
        5,
        sauron_db::retention::ErrorSplit::All,
        sauron_db::retention::Audience::Everyone,
    )
    .await
    .unwrap();
    assert!(
        rows.iter().all(|r| r.cohort != d0 || r.users == 0),
        "env_b holds none of scoped_u's activity, so it must not appear there"
    );

    db.cleanup().await;
}

/// The coverage floor: the earliest day for which person-days exist at all.
///
/// This is the denominator of honesty for the grid. Cohorts are assigned from
/// `event_user_environments.first_seen`, which is never pruned; activity comes
/// from `person_days`, which begins whenever ingest or the backfill began. A
/// person whose `first_seen` predates the person-day history has periods with
/// NO data behind them, and the API must render those as unknown rather than
/// as 0% — the difference between "nobody came back" and "we cannot say".
#[tokio::test]
async fn coverage_floor_reports_the_earliest_person_day() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs().await;
    let scope = sauron_db::scope::ReadScope::all(ids.app_id);

    // Empty table: nothing is knowable, and the caller must get None rather
    // than a date that would silently un-blank every period.
    let empty = sauron_db::rollups::person_days::coverage_floor(&mut conn, &scope)
        .await
        .unwrap();
    assert_eq!(empty, None, "an app with no person-days has no floor");

    let d = |off: i64| (chrono::Utc::now() - chrono::Duration::days(off)).date_naive();
    for (who, off) in [("cf_a", 5i64), ("cf_b", 12), ("cf_c", 2)] {
        diesel::sql_query(
            "INSERT INTO person_days (app_id, environment_id, distinct_id, day, events) \
             VALUES ($1, NULL, $2, $3, 1)",
        )
        .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
        .bind::<diesel::sql_types::Text, _>(who)
        .bind::<diesel::sql_types::Date, _>(d(off))
        .execute(&mut conn)
        .await
        .unwrap();
    }

    let floor = sauron_db::rollups::person_days::coverage_floor(&mut conn, &scope)
        .await
        .unwrap();
    assert_eq!(
        floor,
        Some(d(12)),
        "the floor is the EARLIEST person-day, not the latest"
    );

    // Environment-scoped: the floor must follow the scope, or an env whose
    // history starts later would have its early periods reported as 0%.
    diesel::sql_query(
        "INSERT INTO person_days (app_id, environment_id, distinct_id, day, events) \
         VALUES ($1, $2, 'cf_env', $3, 1)",
    )
    .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
    .bind::<diesel::sql_types::Uuid, _>(ids.env_a)
    .bind::<diesel::sql_types::Date, _>(d(3))
    .execute(&mut conn)
    .await
    .unwrap();

    let scoped = sauron_db::rollups::person_days::coverage_floor(
        &mut conn,
        &sauron_db::scope::ReadScope::new(ids.app_id, sauron_db::scope::EnvFilter::One(ids.env_a)),
    )
    .await
    .unwrap();
    assert_eq!(
        scoped,
        Some(d(3)),
        "an environment-scoped floor must reflect THAT environment's history"
    );

    db.cleanup().await;
}

/// One human logging out and back in all day stays ONE person.
///
/// Each `reset()` mints a fresh anonymous id, so a day of logout/login cycles
/// creates a *transient* anonymous person per cycle — the question is whether
/// they accumulate. They must not: each `identify()` aliases that cycle's
/// anonymous id onto the same named user, and the merge folds the rows over
/// and deletes the alias's own `event_users` row.
///
/// The failure this pins is unbounded person growth for a single human, which
/// would inflate cohort sizes, crush retention, and bill by the login.
#[tokio::test]
async fn repeated_logout_login_collapses_to_one_person() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs().await;
    const PERSON: &str = "user-42";
    const CYCLES: usize = 8;

    // The named user exists from the first login.
    sauron_db::repo::upsert_event_user(&mut conn, ids.app_id, PERSON, &serde_json::json!({}))
        .await
        .unwrap();

    let today = chrono::Utc::now().date_naive();
    for cycle in 0..CYCLES {
        // Logged out: a fresh anon id, and the user browses under it — which
        // is what makes it a real alias rather than an unused id.
        let anon = format!("anon_cycle_{cycle}");
        sauron_db::repo::upsert_event_user(&mut conn, ids.app_id, &anon, &serde_json::json!({}))
            .await
            .unwrap();
        diesel::sql_query(
            "INSERT INTO person_days (app_id, environment_id, distinct_id, day, events) \
             VALUES ($1, NULL, $2, $3, 1) \
             ON CONFLICT (app_id, COALESCE(environment_id, \
               '00000000-0000-0000-0000-000000000000'::uuid), distinct_id, day) \
             DO UPDATE SET events = person_days.events + 1",
        )
        .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
        .bind::<diesel::sql_types::Text, _>(anon.clone())
        .bind::<diesel::sql_types::Date, _>(today)
        .execute(&mut conn)
        .await
        .unwrap();

        // Logs back in: identify() aliases this cycle's anon id onto the same
        // person, and the merge runs.
        sauron_db::identity_merge::rewrite_hot_rows(&mut conn, ids.app_id, &anon, PERSON)
            .await
            .unwrap();
        sauron_db::identity_merge::fold_rollups(&mut conn, ids.app_id, &anon, PERSON, 7)
            .await
            .unwrap();
    }

    let people: CountRow = diesel::sql_query(
        "SELECT count(*) AS n FROM event_users WHERE app_id = $1 AND distinct_id LIKE 'anon\\_cycle%'",
    )
    .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
    .get_result(&mut conn)
    .await
    .unwrap();
    assert_eq!(
        people.n, 0,
        "every cycle's anonymous person must be folded away, not left behind"
    );

    // And the day is ONE person-day for the named user, not one per login:
    // the union means eight logins on one day are one active day.
    let days: CountRow = diesel::sql_query(
        "SELECT count(*) AS n FROM person_days WHERE app_id = $1 AND distinct_id = $2",
    )
    .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
    .bind::<diesel::sql_types::Text, _>(PERSON)
    .get_result(&mut conn)
    .await
    .unwrap();
    assert_eq!(days.n, 1, "eight logins in one day are one active day");

    db.cleanup().await;
}

/// One human on many devices is ONE person — but still many devices.
///
/// After `identify()`, every device writes under the SAME app-supplied
/// `distinct_id`, and each device's own anonymous id is aliased onto it. So the
/// person count must not scale with devices, and neither must the person-DAY
/// count: three devices active on one day is one active day for that human, or
/// retention would read three returns where there was one.
///
/// The device dimension is deliberately NOT collapsed — `devices` stays one row
/// per device, which is what the Devices inventory is for.
#[tokio::test]
async fn one_user_on_many_devices_is_one_person_with_many_devices() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs().await;
    const PERSON: &str = "user-42";
    const DEVICES: usize = 3;

    sauron_db::repo::upsert_event_user(&mut conn, ids.app_id, PERSON, &serde_json::json!({}))
        .await
        .unwrap();

    let today = chrono::Utc::now().date_naive();
    for d in 0..DEVICES {
        // Each device carries its OWN anonymous id (device-local storage), used
        // before the user logs in on that device.
        let anon = format!("anon_device_{d}");
        sauron_db::repo::upsert_event_user(&mut conn, ids.app_id, &anon, &serde_json::json!({}))
            .await
            .unwrap();
        diesel::sql_query(
            "INSERT INTO person_days (app_id, environment_id, distinct_id, day, events) \
             VALUES ($1, NULL, $2, $3, 1) \
             ON CONFLICT (app_id, COALESCE(environment_id, \
               '00000000-0000-0000-0000-000000000000'::uuid), distinct_id, day) \
             DO UPDATE SET events = person_days.events + 1",
        )
        .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
        .bind::<diesel::sql_types::Text, _>(anon.clone())
        .bind::<diesel::sql_types::Date, _>(today)
        .execute(&mut conn)
        .await
        .unwrap();

        // Logging in on that device aliases its anonymous id onto the same
        // person. Different alias each time, same target — no conflict, because
        // an anonymous id binds to a person once and these are distinct ids.
        sauron_db::identity_merge::rewrite_hot_rows(&mut conn, ids.app_id, &anon, PERSON)
            .await
            .unwrap();
        sauron_db::identity_merge::fold_rollups(&mut conn, ids.app_id, &anon, PERSON, 7)
            .await
            .unwrap();
    }

    let leftover: CountRow = diesel::sql_query(
        "SELECT count(*) AS n FROM event_users \
          WHERE app_id = $1 AND distinct_id LIKE 'anon\\_device%'",
    )
    .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
    .get_result(&mut conn)
    .await
    .unwrap();
    assert_eq!(leftover.n, 0, "each device's guest identity must fold away");

    let days: CountRow = diesel::sql_query(
        "SELECT count(*) AS n FROM person_days WHERE app_id = $1 AND distinct_id = $2",
    )
    .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
    .bind::<diesel::sql_types::Text, _>(PERSON)
    .get_result(&mut conn)
    .await
    .unwrap();
    assert_eq!(
        days.n, 1,
        "three devices on one day is ONE active day, not three"
    );

    // The counters still add up: the day carries all three devices' events.
    let events: CountRow = diesel::sql_query(
        "SELECT events AS n FROM person_days WHERE app_id = $1 AND distinct_id = $2",
    )
    .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
    .bind::<diesel::sql_types::Text, _>(PERSON)
    .get_result(&mut conn)
    .await
    .unwrap();
    assert_eq!(
        events.n, DEVICES as i64,
        "activity is summed, days are unioned"
    );

    db.cleanup().await;
}
