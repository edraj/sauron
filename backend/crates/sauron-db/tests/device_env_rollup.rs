//! `device_environments` — the per-(device, environment) rollup that makes
//! `list_device_groups` bounded by page size instead of by device count.

mod common;

use common::TestDb;
use diesel_async::RunQueryDsl;
use sauron_db::repo::TimeWindow;
use sauron_db::scope::{EnvFilter, ReadScope};

/// `environment_id` is NULLABLE because `EnvFilter::Unattributed` is a real
/// scope, and NULL never equals NULL — so a plain `UNIQUE (app_id, device_key,
/// environment_id)` would let one device accumulate unlimited unattributed rows
/// and every upsert against them would INSERT instead of UPDATE. Counters would
/// silently stop accumulating for exactly the scope that has no environment.
#[tokio::test]
async fn unattributed_rollup_rows_are_unique_per_device() {
    let Some(db) = TestDb::setup().await else {
        panic!("TEST_DATABASE_URL unset — this test must not silently skip");
    };
    let mut conn = db.conn().await;
    let app_id = db.seed_two_envs().await.app_id;

    let insert = || {
        diesel::sql_query(
            "INSERT INTO device_environments \
               (app_id, device_key, environment_id, first_seen, last_seen) \
             VALUES ($1, 'dev-1', NULL, now(), now())",
        )
        .bind::<diesel::sql_types::Uuid, _>(app_id)
    };

    insert().execute(&mut conn).await.expect("first insert");
    assert!(
        insert().execute(&mut conn).await.is_err(),
        "a second NULL-environment row for the same device must be rejected"
    );

    drop(conn);
    db.cleanup().await;
}

/// Two bumps for the same (app, device, env) must accumulate into one row, not
/// two — and `first_seen`/`last_seen` must widen rather than overwrite. The
/// `ON CONFLICT` names an EXPRESSION (COALESCE(environment_id, nil)); naming
/// the bare column list instead still compiles and still runs, it just stops
/// matching the index and inserts duplicates.
#[tokio::test]
async fn device_env_bumps_accumulate_into_one_row() {
    let Some(db) = TestDb::setup().await else {
        panic!("TEST_DATABASE_URL unset — this test must not silently skip");
    };
    let mut conn = db.conn().await;
    let app_id = db.seed_two_envs().await.app_id;
    let t0 = chrono::Utc::now() - chrono::Duration::hours(2);
    let t1 = chrono::Utc::now();

    let bump = |at: chrono::DateTime<chrono::Utc>, ev: i64| sauron_db::batch::DeviceEnvBump {
        app_id,
        device_key: "dev-1".into(),
        environment_id: None,
        first_at: at,
        last_at: at,
        events_delta: ev,
        errors_delta: 0,
        sessions_delta: 0,
    };

    // The EARLIER bump lands FIRST (the INSERT) and the LATER bump lands
    // second (the UPDATE) — the only order that can tell `LEAST` apart from a
    // plain overwrite. The update here always carries the LARGER value, so
    // `LEAST(existing, new)` must keep `first_seen` pinned at t0 while a bare
    // overwrite would wrongly drag it forward to t1. The reverse order can't
    // distinguish the two: the update would always carry the smaller value,
    // so `LEAST(prev, new)` and a bare overwrite compute the same result.
    sauron_db::batch::bump_device_envs(&mut conn, &[bump(t0, 4)])
        .await
        .expect("first bump");
    sauron_db::batch::bump_device_envs(&mut conn, &[bump(t1, 3)])
        .await
        .expect("second bump");

    #[derive(diesel::QueryableByName)]
    struct Row {
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        n: i64,
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        events_count: i64,
        #[diesel(sql_type = diesel::sql_types::Timestamptz)]
        first_seen: chrono::DateTime<chrono::Utc>,
    }
    let r: Row = diesel::sql_query(
        "SELECT count(*) AS n, max(events_count) AS events_count, min(first_seen) AS first_seen \
         FROM device_environments WHERE app_id=$1 AND device_key='dev-1'",
    )
    .bind::<diesel::sql_types::Uuid, _>(app_id)
    .get_result(&mut conn)
    .await
    .expect("read back");

    assert_eq!(r.n, 1, "both bumps must land on one row");
    assert_eq!(r.events_count, 7, "events_count must accumulate");
    assert!(
        (r.first_seen - t0).num_seconds().abs() < 2,
        "first_seen must stay at the earlier bump (t0), not be dragged forward by the later update"
    );

    drop(conn);
    db.cleanup().await;
}

/// The backfill is ADDITIVE against a cutoff, never `ON CONFLICT DO NOTHING`.
/// The write path bumps this table from the moment the migration lands, so a
/// live bump can create a row before the backfill reaches that device; DO
/// NOTHING would then skip it and drop that device's entire history, silently
/// and permanently. Live bumps carry signals at or after the cutoff and the
/// backfill aggregates strictly before it, so the two sets are disjoint and
/// adding them is exact.
#[tokio::test]
async fn backfill_adds_to_a_row_the_write_path_already_created() {
    let Some(db) = TestDb::setup().await else {
        panic!("TEST_DATABASE_URL unset — this test must not silently skip");
    };
    let mut conn = db.conn().await;
    let app_id = db.seed_two_envs().await.app_id;
    let cutoff = chrono::Utc::now();

    // A live bump lands first, as it would on a running deployment.
    sauron_db::batch::bump_device_envs(
        &mut conn,
        &[sauron_db::batch::DeviceEnvBump {
            app_id,
            device_key: "dev-1".into(),
            environment_id: None,
            first_at: cutoff,
            last_at: cutoff,
            events_delta: 5,
            errors_delta: 0,
            sessions_delta: 0,
        }],
    )
    .await
    .expect("live bump");

    // Two historical analytics rows, strictly before the cutoff.
    for _ in 0..2 {
        diesel::sql_query(
            "INSERT INTO analytics_events \
               (id, app_id, environment_id, name, distinct_id, properties, context, \
                occurred_at, received_at, device_key, tags, contexts, extra) \
             VALUES (gen_random_uuid(), $1, NULL, 'evt', 'p-1', '{}', '{}', \
                     $2 - interval '1 hour', now(), 'dev-1', '{}', '{}', '{}')",
        )
        .bind::<diesel::sql_types::Uuid, _>(app_id)
        .bind::<diesel::sql_types::Timestamptz, _>(cutoff)
        .execute(&mut conn)
        .await
        .expect("seed historical event");
    }

    sauron_db::device_env_backfill::backfill_app(&mut conn, app_id, cutoff)
        .await
        .expect("backfill");

    #[derive(diesel::QueryableByName)]
    struct N {
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        events_count: i64,
    }
    let r: N = diesel::sql_query(
        "SELECT events_count FROM device_environments WHERE app_id=$1 AND device_key='dev-1'",
    )
    .bind::<diesel::sql_types::Uuid, _>(app_id)
    .get_result(&mut conn)
    .await
    .expect("read back");

    assert_eq!(
        r.events_count, 7,
        "backfill must ADD its 2 historical events to the 5 the write path already recorded"
    );
    assert!(
        sauron_db::device_env_backfill::is_backfilled(&mut conn, app_id)
            .await
            .expect("marker"),
        "the marker must be set in the same transaction as the aggregate"
    );

    drop(conn);
    db.cleanup().await;
}

/// `backfill_all`'s cutoff must be [`sauron_db::device_env_backfill::rollup_epoch`]
/// (the instant migration 59 applied), never `Utc::now()` evaluated at
/// backfill time. A signal ingested AFTER the epoch has already been counted
/// by the live write path (`bump_device_envs`); if the backfill's cutoff is
/// `now()` — strictly later than that signal's `occurred_at` — its raw
/// `analytics_events` row also matches `occurred_at < cutoff` and gets
/// aggregated a second time. Reproduces the reviewer's probe: a live-bumped
/// signal read back as 2 instead of 1 under a `Utc::now()` cutoff.
#[tokio::test]
async fn backfill_epoch_cutoff_does_not_double_count_a_live_bumped_signal() {
    let Some(db) = TestDb::setup().await else {
        panic!("TEST_DATABASE_URL unset — this test must not silently skip");
    };
    let mut conn = db.conn().await;
    let app_id = db.seed_two_envs().await.app_id;

    // Recorded by migration 59 when this ephemeral database was created —
    // strictly before the seeding above, which took several real round trips.
    let epoch = sauron_db::device_env_backfill::rollup_epoch(&mut conn)
        .await
        .expect("read rollup epoch");

    // A signal timestamped well AFTER the epoch: squarely inside the window
    // the live write path owns. `Utc::now()` evaluated any time later in this
    // test is guaranteed to be later still, which is what makes the
    // reverted-to-`now()` cutoff below double-count it.
    let occurred_at = chrono::Utc::now();
    assert!(
        occurred_at > epoch,
        "test setup must take measurable time so occurred_at lands after epoch"
    );

    // What the live write path actually leaves behind for one ingested event:
    // the raw analytics_events row AND the matching device_environments bump,
    // in the same shape `write_rows_once` would produce.
    diesel::sql_query(
        "INSERT INTO analytics_events \
           (id, app_id, environment_id, name, distinct_id, properties, context, \
            occurred_at, received_at, device_key, tags, contexts, extra) \
         VALUES (gen_random_uuid(), $1, NULL, 'evt', 'p-1', '{}', '{}', \
                 $2, now(), 'dev-2', '{}', '{}', '{}')",
    )
    .bind::<diesel::sql_types::Uuid, _>(app_id)
    .bind::<diesel::sql_types::Timestamptz, _>(occurred_at)
    .execute(&mut conn)
    .await
    .expect("seed live-window event");

    sauron_db::batch::bump_device_envs(
        &mut conn,
        &[sauron_db::batch::DeviceEnvBump {
            app_id,
            device_key: "dev-2".into(),
            environment_id: None,
            first_at: occurred_at,
            last_at: occurred_at,
            events_delta: 1,
            errors_delta: 0,
            sessions_delta: 0,
        }],
    )
    .await
    .expect("live bump for the same signal");

    // The FIX under test: cutoff is the epoch, not `Utc::now()`. Since
    // `occurred_at > epoch`, the backfill's `occurred_at < cutoff` leg must
    // NOT match this row — it belongs to the live write path's window, not
    // the backfill's.
    sauron_db::device_env_backfill::backfill_app(&mut conn, app_id, epoch)
        .await
        .expect("backfill with the epoch cutoff");

    #[derive(diesel::QueryableByName)]
    struct N {
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        events_count: i64,
    }
    let r: N = diesel::sql_query(
        "SELECT events_count FROM device_environments WHERE app_id=$1 AND device_key='dev-2'",
    )
    .bind::<diesel::sql_types::Uuid, _>(app_id)
    .get_result(&mut conn)
    .await
    .expect("read back");

    assert_eq!(
        r.events_count, 1,
        "the epoch cutoff must not re-aggregate a signal the live write path already counted"
    );

    drop(conn);
    db.cleanup().await;
}

/// The previous test proved the SQL is correct by calling `backfill_app` with
/// an explicit cutoff — it says nothing about what cutoff the real entry
/// point, `backfill_all`, actually passes. This one drives `backfill_all`
/// itself, so it fails if `backfill_all`'s `let cutoff = rollup_epoch(...)`
/// is ever reverted to `Utc::now()` (reviewer-verified: it was, and the rest
/// of the suite stayed green).
///
/// Deliberately does NOT mark the app backfilled first — `backfill_all` skips
/// any app that already has a `device_env_backfill` row — and asserts on this
/// test's own app/device specifically rather than on a global count, since
/// `backfill_all` iterates every unbackfilled app in the database.
#[tokio::test]
async fn backfill_all_sources_its_cutoff_from_the_epoch_not_now() {
    let Some(db) = TestDb::setup().await else {
        panic!("TEST_DATABASE_URL unset — this test must not silently skip");
    };
    let mut conn = db.conn().await;
    let app_id = db.seed_two_envs().await.app_id;

    let epoch = sauron_db::device_env_backfill::rollup_epoch(&mut conn)
        .await
        .expect("read rollup epoch");

    // A signal timestamped AFTER the epoch: squarely inside the live write
    // path's window, exactly like the previous test — only an after-epoch
    // timestamp can distinguish an epoch cutoff from a `Utc::now()` cutoff.
    // (A before-epoch timestamp is `< cutoff` under EITHER choice, since
    // `epoch <= ` any `now()` evaluated afterward, so it cannot tell the two
    // apart — confirmed empirically while building this test.)
    let occurred_at = chrono::Utc::now();
    assert!(
        occurred_at > epoch,
        "test setup must take measurable time so occurred_at lands after epoch"
    );

    diesel::sql_query(
        "INSERT INTO analytics_events \
           (id, app_id, environment_id, name, distinct_id, properties, context, \
            occurred_at, received_at, device_key, tags, contexts, extra) \
         VALUES (gen_random_uuid(), $1, NULL, 'evt', 'p-1', '{}', '{}', \
                 $2, now(), 'dev-3', '{}', '{}', '{}')",
    )
    .bind::<diesel::sql_types::Uuid, _>(app_id)
    .bind::<diesel::sql_types::Timestamptz, _>(occurred_at)
    .execute(&mut conn)
    .await
    .expect("seed live-window event");

    sauron_db::batch::bump_device_envs(
        &mut conn,
        &[sauron_db::batch::DeviceEnvBump {
            app_id,
            device_key: "dev-3".into(),
            environment_id: None,
            first_at: occurred_at,
            last_at: occurred_at,
            events_delta: 1,
            errors_delta: 0,
            sessions_delta: 0,
        }],
    )
    .await
    .expect("live bump for the same signal");

    // The real production entry point — not backfill_app directly.
    sauron_db::device_env_backfill::backfill_all(db.pool())
        .await
        .expect("backfill_all");

    #[derive(diesel::QueryableByName)]
    struct N {
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        events_count: i64,
    }
    let r: N = diesel::sql_query(
        "SELECT events_count FROM device_environments WHERE app_id=$1 AND device_key='dev-3'",
    )
    .bind::<diesel::sql_types::Uuid, _>(app_id)
    .get_result(&mut conn)
    .await
    .expect("read back");

    assert_eq!(
        r.events_count, 1,
        "backfill_all must source its cutoff from rollup_epoch, not Utc::now(), \
         or this live-counted signal is re-aggregated a second time"
    );

    drop(conn);
    db.cleanup().await;
}

/// The subtlest of the three `UNION ALL` legs: a session's `sessions_count`
/// is credited once, at row creation, and its `started_at` only ever moves
/// EARLIER (via `LEAST` in `bump_sessions`), while `last_event_at` only ever
/// moves LATER (via `GREATEST`). So a session that STARTED before the epoch
/// stays claimed by the backfill's `started_at < cutoff` leg no matter how
/// far `last_event_at` has since been dragged past the epoch by later
/// activity. Must contribute exactly 1 to `sessions_count` — not 0 (wrongly
/// excluded because activity continued past the epoch) and not 2 (double
/// counted against a live bump this test never makes).
#[tokio::test]
async fn backfill_sessions_leg_counts_a_session_straddling_the_epoch_once() {
    let Some(db) = TestDb::setup().await else {
        panic!("TEST_DATABASE_URL unset — this test must not silently skip");
    };
    let mut conn = db.conn().await;
    let app_id = db.seed_two_envs().await.app_id;

    let epoch = sauron_db::device_env_backfill::rollup_epoch(&mut conn)
        .await
        .expect("read rollup epoch");

    let started_at = epoch - chrono::Duration::hours(1);
    let last_event_at = epoch + chrono::Duration::minutes(30);

    diesel::sql_query(
        "INSERT INTO sessions (app_id, session_id, device_key, started_at, last_event_at) \
         VALUES ($1, 'sess-straddle', 'dev-4', $2, $3)",
    )
    .bind::<diesel::sql_types::Uuid, _>(app_id)
    .bind::<diesel::sql_types::Timestamptz, _>(started_at)
    .bind::<diesel::sql_types::Timestamptz, _>(last_event_at)
    .execute(&mut conn)
    .await
    .expect("seed straddling session");

    sauron_db::device_env_backfill::backfill_app(&mut conn, app_id, epoch)
        .await
        .expect("backfill");

    #[derive(diesel::QueryableByName)]
    struct N {
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        sessions_count: i64,
    }
    let r: N = diesel::sql_query(
        "SELECT sessions_count FROM device_environments WHERE app_id=$1 AND device_key='dev-4'",
    )
    .bind::<diesel::sql_types::Uuid, _>(app_id)
    .get_result(&mut conn)
    .await
    .expect("read back");

    assert_eq!(
        r.sessions_count, 1,
        "a session started before the epoch must contribute exactly 1 to sessions_count, \
         regardless of how far its last_event_at has been dragged past the epoch"
    );

    drop(conn);
    db.cleanup().await;
}

/// The default group sort `routes::devices::group_sort_spec` builds for an
/// absent `sort` parameter. A function, not a constant: `SortSpec` is
/// deliberately NOT `Clone` (its fields are `&'static str` so no caller string
/// can ever be interpolated), so each call site builds its own.
fn group_sort() -> sauron_db::repo::SortSpec {
    sauron_db::repo::SortSpec {
        column: "last_seen",
        descending: true,
        tiebreak: "d.family, d.model, d.os_name, d.os_version",
        nulls_last: false,
    }
}

/// The rollup shape must NOT source `sessions_count` from the rollup.
///
/// `list_device_groups` windows that one count (`count(*) FILTER (WHERE
/// started_at >= $2)`) while `events_count`/`errors_count` are lifetime, so a
/// lifetime rollup column changes the displayed number. Measured: reading it
/// from the rollup was 36ms instead of 105ms and differed on 40 of 40 rows.
/// This is a SHAPE assertion because both variants return plausible numbers —
/// which is exactly why the fast-and-wrong one is easy to ship.
#[test]
fn rollup_shape_keeps_sessions_count_live() {
    let sql =
        sauron_db::repo::list_device_groups_rollup_sql_for_test(EnvFilter::One(uuid::Uuid::nil()));
    assert!(
        sql.contains("count(*) FILTER (WHERE started_at >="),
        "sessions_count must stay a windowed live aggregate, got:\n{sql}"
    );
    assert!(
        !sql.contains("sum(de.sessions_count)"),
        "sessions_count must NOT be summed from the rollup, got:\n{sql}"
    );
    assert!(
        !sql.contains("LEFT JOIN LATERAL ( SELECT count(*) AS cnt, min(occurred_at)"),
        "the analytics/error LATERALs must be gone, got:\n{sql}"
    );
}

/// Under `All` the rollup shape must keep reading the durable `devices`
/// counters, exactly as the live shape does. Deriving them from the rollup
/// would silently change what an unscoped page displays on the day an operator
/// runs the backfill — a number moving with no code deploy behind it.
#[test]
fn rollup_shape_under_all_reads_durable_device_columns() {
    let sql = sauron_db::repo::list_device_groups_rollup_sql_for_test(EnvFilter::All);
    assert!(
        sql.contains("sum(d.events_count)"),
        "All must read devices.events_count, got:\n{sql}"
    );
    assert!(
        !sql.contains("device_environments"),
        "All must not consult the rollup at all, got:\n{sql}"
    );
}

/// The rollup join must be pre-aggregated per device before it reaches the
/// `GROUP BY`.
///
/// `device_environments` holds one row per (device, environment). Joining it
/// raw is correct for `One`/`Unattributed` (one row admitted per device) and
/// silently WRONG for `Subset` — a real scope, produced by `authorize_env` for
/// a caller holding environment grants: a device active in two admitted
/// environments joins twice, `count(*)` counts join output rows rather than
/// devices, and the `se` LATERAL re-runs and re-sums per copy. `events_count`
/// and `errors_count` stay right either way, which is what makes it a quiet
/// wrong answer instead of an obviously broken page. This is a SHAPE assertion
/// for the same reason as `rollup_shape_keeps_sessions_count_live`, and
/// `rollup_and_live_shapes_return_identical_rows` is the behavioural half.
#[test]
fn rollup_shape_pre_aggregates_the_rollup_per_device() {
    for env in [
        EnvFilter::One(uuid::Uuid::nil()),
        EnvFilter::Subset(vec![uuid::Uuid::nil()]),
        EnvFilter::Unattributed,
    ] {
        let sql = sauron_db::repo::list_device_groups_rollup_sql_for_test(env.clone());
        assert!(
            sql.contains("FROM device_environments") && sql.contains("GROUP BY app_id, device_key"),
            "{env:?}: the rollup must be grouped per device before the join, got:\n{sql}"
        );
    }
}

/// The rollup must return byte-identical rows to the live query, under every
/// scope. This is the test the whole plan rests on: both shapes return
/// plausible numbers, so nothing else in the suite can tell a correct rollup
/// from a subtly wrong one — the failure it catches is a page whose numbers
/// move on the day an operator runs the backfill, with no deploy behind them.
///
/// `seed_mixed_device_activity` seeds every combination that has bitten this
/// table's persons twin: a device with events but no sessions, one with
/// sessions but no events, one with only errors, one present in two
/// environments, and one with a NULL environment.
#[tokio::test]
async fn rollup_and_live_shapes_return_identical_rows() {
    let Some(db) = TestDb::setup().await else {
        panic!("TEST_DATABASE_URL unset — this test must not silently skip");
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    common::seed_mixed_device_activity(&mut conn, &ids).await;

    let since = chrono::Utc::now() - chrono::Duration::days(30);
    let scopes = [
        ("All", EnvFilter::All),
        ("One(env_a)", EnvFilter::One(ids.env_a)),
        ("One(env_b)", EnvFilter::One(ids.env_b)),
        ("Subset(a,b)", EnvFilter::Subset(vec![ids.env_a, ids.env_b])),
        ("Unattributed", EnvFilter::Unattributed),
    ];

    // The backfill is the ONLY thing that may populate the rollup here, which
    // is what lets the cutoff below be later than every seeded timestamp
    // without risking a double count. If the ingest write path ever starts
    // bumping `device_environments` from these seed helpers, this fires loudly
    // instead of the comparison quietly measuring doubled counters.
    assert_eq!(
        rollup_rows(&mut conn, ids.app_id).await,
        0,
        "precondition: nothing but the backfill may populate device_environments in this test"
    );

    let mut live = Vec::new();
    for (name, env) in &scopes {
        let rows = sauron_db::repo::list_device_groups(
            &mut conn,
            ReadScope::new(ids.app_id, env.clone()),
            TimeWindow::since("last_seen", since),
            200,
            0,
            group_sort(),
            None,
        )
        .await
        .unwrap_or_else(|e| panic!("live list_device_groups under {name}: {e}"));
        live.push(rows);
    }

    // Without these the whole comparison could pass vacuously — two empty
    // result sets are equal.
    for ((name, _), rows) in scopes.iter().zip(live.iter()) {
        assert!(
            rows.len() >= 2,
            "fixture precondition: {name} must return groups to compare, got {}",
            rows.len()
        );
    }
    // The two assertions below index `live` positionally; say which scope that
    // is, so reordering `scopes` fails here rather than silently checking the
    // preconditions against a different environment.
    assert_eq!(scopes[1].0, "One(env_a)");
    // MX-1 is the events-only + sessions-only pair: a group that only has the
    // right `device_count` if the sessions leg of the membership predicate
    // admits a device with no analytics row at all.
    assert_eq!(
        group(&live[1], "MX-1").device_count,
        2,
        "fixture precondition: One(env_a) must see both the events-only and the \
         sessions-only device"
    );
    // The ONE fixture shape that can tell this endpoint's WINDOWED
    // `sessions_count` from the rollup's lifetime one: MX-4 has an in-window
    // analytics event (so it qualifies under both shapes) and a single session
    // ~400 days back (so the `count(*) FILTER (WHERE started_at >= $2)` excludes
    // it). Verified by mutation: sourcing `sessions_count` from the rollup left
    // the whole comparison below GREEN until this device existed, because every
    // other seeded session is inside the window and the two numbers coincide.
    assert_eq!(
        group(&live[1], "MX-4").sessions_count,
        0,
        "fixture precondition: the out-of-window session must not be counted"
    );

    // Cutoff AFTER every seeded timestamp, including `seed_two_envs`' rows,
    // which are pinned to noon UTC of the current day and are therefore in the
    // FUTURE for any run before noon — `Utc::now()` as the cutoff would leave
    // them out of the aggregate and this test would fail for a reason that has
    // nothing to do with the read path. Safe precisely because of the
    // precondition asserted above: with no live bumps in this database, there
    // is no second counter for a late cutoff to double up against.
    let cutoff = ids.pinned_now.max(chrono::Utc::now()) + chrono::Duration::days(1);
    sauron_db::device_env_backfill::backfill_app(&mut conn, ids.app_id, cutoff)
        .await
        .expect("backfill");
    // And the branch must actually have switched, or this compares live to live.
    assert!(
        sauron_db::device_env_backfill::is_backfilled(&mut conn, ids.app_id)
            .await
            .expect("marker"),
        "the marker must be set, otherwise every call below takes the live shape again \
         and the comparison proves nothing"
    );
    // The other half of the MX-4 precondition: the rollup must really carry a
    // session for that device, or "live says 0 and the rollup agrees" would be
    // two zeros rather than a windowed count beating a lifetime one.
    assert_eq!(
        rollup_sessions_for_model(&mut conn, ids.app_id, "MX-4").await,
        1,
        "fixture precondition: the rollup must carry the out-of-window session, \
         otherwise the sessions_count comparison below is vacuous"
    );

    for (before, (name, env)) in live.iter().zip(scopes.iter()) {
        let after = sauron_db::repo::list_device_groups(
            &mut conn,
            ReadScope::new(ids.app_id, env.clone()),
            TimeWindow::since("last_seen", since),
            200,
            0,
            group_sort(),
            None,
        )
        .await
        .unwrap_or_else(|e| panic!("rollup list_device_groups under {name}: {e}"));

        assert_eq!(
            before.len(),
            after.len(),
            "{name}: the rollup shape returned a different number of groups\nlive:   {:?}\nrollup: {:?}",
            before.iter().map(|r| &r.model).collect::<Vec<_>>(),
            after.iter().map(|r| &r.model).collect::<Vec<_>>(),
        );
        for (l, r) in before.iter().zip(after.iter()) {
            let who = format!("{name}/{:?}/{:?}", l.family, l.model);
            assert_eq!(
                (&l.family, &l.model, &l.os_name, &l.os_version),
                (&r.family, &r.model, &r.os_name, &r.os_version),
                "{who}: group key / ordering diverged"
            );
            assert_eq!(l.device_count, r.device_count, "{who}: device_count");
            assert_eq!(l.events_count, r.events_count, "{who}: events_count");
            assert_eq!(l.errors_count, r.errors_count, "{who}: errors_count");
            assert_eq!(l.sessions_count, r.sessions_count, "{who}: sessions_count");
            assert_eq!(l.first_seen, r.first_seen, "{who}: first_seen");
            assert_eq!(l.last_seen, r.last_seen, "{who}: last_seen");
        }

        // The one number a raw (un-pre-aggregated) rollup join gets wrong, named
        // explicitly rather than left to the equality above: under `Subset` the
        // both-envs device would join twice and MX-2 would report 3 devices.
        if *name == "Subset(a,b)" {
            assert_eq!(
                group(&after, "MX-2").device_count,
                2,
                "Subset: the both-envs device must be counted ONCE, not once per environment"
            );
        }
    }

    drop(conn);
    db.cleanup().await;
}

/// How many rollup rows exist for `app_id` — used as a precondition, so the
/// equivalence test above fails loudly rather than silently comparing doubled
/// counters if anything but its own backfill starts writing this table.
async fn rollup_rows(conn: &mut sauron_db::PgConn, app_id: uuid::Uuid) -> i64 {
    #[derive(diesel::QueryableByName)]
    struct N {
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        n: i64,
    }
    let r: N = diesel::sql_query("SELECT count(*) AS n FROM device_environments WHERE app_id=$1")
        .bind::<diesel::sql_types::Uuid, _>(app_id)
        .get_result(conn)
        .await
        .expect("count rollup rows");
    r.n
}

/// The rollup's lifetime `sessions_count` for the devices in one descriptor
/// group — the number `list_device_groups` must NOT display.
async fn rollup_sessions_for_model(
    conn: &mut sauron_db::PgConn,
    app_id: uuid::Uuid,
    model: &str,
) -> i64 {
    #[derive(diesel::QueryableByName)]
    struct N {
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        n: i64,
    }
    let r: N = diesel::sql_query(
        "SELECT COALESCE(sum(de.sessions_count), 0)::bigint AS n \
         FROM device_environments de \
         JOIN devices d ON d.app_id = de.app_id AND d.device_key = de.device_key \
         WHERE de.app_id = $1 AND d.model = $2",
    )
    .bind::<diesel::sql_types::Uuid, _>(app_id)
    .bind::<diesel::sql_types::Text, _>(model.to_string())
    .get_result(conn)
    .await
    .expect("read rollup sessions_count");
    r.n
}

/// The one group whose `model` is `model`, or a panic naming what was there —
/// an `Option` here would let a typo'd model silently skip an assertion.
fn group<'a>(
    rows: &'a [sauron_db::repo::DeviceGroupRow],
    model: &str,
) -> &'a sauron_db::repo::DeviceGroupRow {
    rows.iter()
        .find(|r| r.model.as_deref() == Some(model))
        .unwrap_or_else(|| {
            panic!(
                "no group with model {model:?} — got {:?}",
                rows.iter().map(|r| &r.model).collect::<Vec<_>>()
            )
        })
}

/// Task 11: a device whose ONLY environment-scoped signal is a `transactions`
/// row — no analytics event, no error, no session. Before Task 11, neither
/// `device_membership_sql` (the live shape's membership predicate) nor the
/// backfill's `UNION ALL` knew about `transactions`, even though the write
/// path already did: `sauron-pipeline`'s `Acc::rollup` folds
/// `device_environments` from THREE call sites — analytics events, error
/// events, AND transactions (with `0,0` deltas) — so in production a
/// transaction-only device already has a live rollup row from the moment it
/// is ingested, while the live query's three-leg predicate could never see
/// it. Measured on the real bug: `device_count` live=1, rollup=2.
///
/// `seed_mixed_device_activity` never seeds a transaction, which is exactly
/// why `rollup_and_live_shapes_return_identical_rows` cannot see this gap.
/// Dedicated test rather than a 7th device folded into that fixture, to
/// leave its already-dense doc comment and model-keyed assertions (MX-1..
/// MX-4) untouched.
///
/// Cannot drive the real `Acc::rollup` fold here — `sauron-pipeline` is out
/// of scope for Task 11 and off limits to edit — so this reproduces the
/// divergence the way the rest of this file already does: seed the raw
/// signal rows directly and drive BOTH shapes through `list_device_groups`
/// itself, exactly like `rollup_and_live_shapes_return_identical_rows` —
/// LIVE before backfilling, ROLLUP after.
///
/// The transaction-only device shares its descriptor tuple ("TX-ONLY") with
/// an ANCHOR device that has two ordinary analytics events. A LONE
/// transaction-only device would leave `ae`/`ee`/`se` all NULL for its own
/// row in the live shape; Postgres's `LEAST`/`GREATEST` return NULL only when
/// EVERY argument is NULL, and `DeviceGroupRow` declares `first_seen`/
/// `last_seen` non-nullable, so a lone all-NULL group fails to DESERIALIZE
/// rather than fail an assertion — that all-NULL case is covered separately
/// by `device_group_where_every_device_is_transaction_only_matches_live_and_rollup`
/// (fix round 1), which is the one that reproduces the 500. This test keeps
/// a real anchor so it stays a distinct scenario: a group where the
/// transaction-only device sits ALONGSIDE a device with real signals.
///
/// UPDATED in fix round 1: `tx_at` is now placed BEFORE `anchor_first` —
/// earlier than the anchor's own earliest event — rather than inside its
/// range. Originally this test deliberately kept the transaction
/// non-extremal, because at the time Task 11's fix widened only the
/// MEMBERSHIP predicate, not the live shape's `ae`/`ee`/`se` LATERALs, so a
/// transaction that WAS the extremum made live and rollup genuinely disagree
/// on `first_seen`/`last_seen` (flagged as a residual gap in this task's
/// first report). Fix round 1 closed that gap: both live shapes now carry a
/// fourth `tx` LATERAL folded into the same `LEAST`/`GREATEST`. This test now
/// exercises exactly the case it used to avoid — the transaction IS the
/// group's `first_seen` extremum, and both shapes must agree it moved —
/// while `last_seen` stays the anchor's own later event on both shapes,
/// since the transaction is not extremal in THAT direction. Proving both
/// (one value that moves, one that doesn't) is what tells a correctly wired
/// `tx` LATERAL apart from one that is merely present but unused. The
/// backfill leg's own timestamp handling is additionally checked directly
/// below, against `device_environments`, independent of the anchor.
#[tokio::test]
async fn device_with_only_a_transaction_matches_live_and_rollup_shapes() {
    let Some(db) = TestDb::setup().await else {
        panic!("TEST_DATABASE_URL unset — this test must not silently skip");
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let anchor_key = format!("tx-anchor-{suffix}");
    let tx_only_key = format!("tx-only-{suffix}");
    let model = "TX-ONLY";

    let now = chrono::Utc::now();
    let anchor_first = now - chrono::Duration::hours(3);
    let anchor_last = now - chrono::Duration::hours(1);
    // BEFORE anchor_first — the group's new first_seen extremum, now that
    // fix round 1 added the `tx` LATERAL. See the doc comment above.
    let tx_at = now - chrono::Duration::hours(4);

    for key in [&anchor_key, &tx_only_key] {
        sauron_db::repo::bump_device(
            &mut conn,
            ids.app_id,
            key,
            Some("TxBrand"),
            Some(model),
            Some("TxOS"),
            Some("1.0"),
            None,
            None,
            None,
            now,
            0,
            0,
        )
        .await
        .expect("seed devices row");
    }

    // The anchor's two ordinary analytics events — see doc comment for why.
    for at in [anchor_first, anchor_last] {
        diesel::sql_query(
            "INSERT INTO analytics_events \
               (id, app_id, environment_id, name, distinct_id, properties, context, \
                occurred_at, received_at, device_key, tags, contexts, extra) \
             VALUES (gen_random_uuid(), $1, $2, 'tx-fixture.evt', 'tx-fixture-person', '{}', '{}', \
                     $3, now(), $4, '{}', '{}', '{}')",
        )
        .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
        .bind::<diesel::sql_types::Uuid, _>(ids.env_a)
        .bind::<diesel::sql_types::Timestamptz, _>(at)
        .bind::<diesel::sql_types::Text, _>(anchor_key.clone())
        .execute(&mut conn)
        .await
        .expect("seed anchor analytics event");
    }

    // The device under test's ONLY environment-scoped signal: a raw
    // `transactions` row. No analytics_events, no error_events, no sessions
    // row exists for this device_key at all.
    diesel::sql_query(
        "INSERT INTO transactions \
           (id, app_id, environment_id, name, op, duration_ms, device_key, \
            occurred_at, received_at) \
         VALUES (gen_random_uuid(), $1, $2, 'tx.only', 'test', 12.5, $3, $4, now())",
    )
    .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
    .bind::<diesel::sql_types::Uuid, _>(ids.env_a)
    .bind::<diesel::sql_types::Text, _>(tx_only_key.clone())
    .bind::<diesel::sql_types::Timestamptz, _>(tx_at)
    .execute(&mut conn)
    .await
    .expect("seed the transaction-only signal");

    let since = now - chrono::Duration::days(30);

    // Same precondition as `rollup_and_live_shapes_return_identical_rows`:
    // nothing but the backfill below may populate the rollup.
    assert_eq!(
        rollup_rows(&mut conn, ids.app_id).await,
        0,
        "precondition: device_environments must be empty before the backfill runs"
    );

    let live = sauron_db::repo::list_device_groups(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        TimeWindow::since("last_seen", since),
        200,
        0,
        group_sort(),
        None,
    )
    .await
    .expect("live list_device_groups");

    let cutoff = now + chrono::Duration::days(1);
    sauron_db::device_env_backfill::backfill_app(&mut conn, ids.app_id, cutoff)
        .await
        .expect("backfill");
    assert!(
        sauron_db::device_env_backfill::is_backfilled(&mut conn, ids.app_id)
            .await
            .expect("marker"),
        "the marker must be set, or the comparison below re-reads the live shape again \
         and the test proves nothing"
    );

    let rollup = sauron_db::repo::list_device_groups(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        TimeWindow::since("last_seen", since),
        200,
        0,
        group_sort(),
        None,
    )
    .await
    .expect("rollup list_device_groups");

    // Fallback to 0 rather than `group()`'s panic: this is the assertion that
    // must fail with a `device_count` MISMATCH — not a panic on a missing
    // group — when the transactions leg is missing from
    // `device_membership_sql`. In that broken state the live shape sees only
    // the anchor (1) while the rollup shape (fed by the backfill's still-intact
    // transactions leg) sees both (2), reproducing this task's own measured
    // numbers: `device_count` live=1, rollup=2.
    let live_count = live
        .iter()
        .find(|r| r.model.as_deref() == Some(model))
        .map(|r| r.device_count)
        .unwrap_or(0);
    let rollup_count = rollup
        .iter()
        .find(|r| r.model.as_deref() == Some(model))
        .map(|r| r.device_count)
        .unwrap_or(0);
    assert_eq!(
        (live_count, rollup_count),
        (2, 2),
        "device_count (live, rollup) for the transaction-only device's group — both \
         the anchor and the transaction-only device must be visible under BOTH shapes"
    );

    // Reached only once both counts are confirmed 2, so both groups exist —
    // `group()`'s panic path is no longer reachable here.
    let l = group(&live, model);
    let r = group(&rollup, model);
    assert_eq!(l.events_count, r.events_count, "events_count");
    assert_eq!(l.errors_count, r.errors_count, "errors_count");
    assert_eq!(l.sessions_count, r.sessions_count, "sessions_count");
    assert_eq!(l.first_seen, r.first_seen, "first_seen");
    assert_eq!(l.last_seen, r.last_seen, "last_seen");
    assert!(
        (l.first_seen - tx_at).num_seconds().abs() < 2,
        "first_seen must be PERTURBED to the transaction's earlier timestamp — \
         proves the new `tx` LATERAL is actually wired into LEAST(), not just \
         present; got {:?}, expected close to {:?}",
        l.first_seen,
        tx_at
    );
    assert!(
        (l.last_seen - anchor_last).num_seconds().abs() < 2,
        "last_seen must stay the anchor's later event — the transaction is not \
         extremal in this direction, proving the `tx` LATERAL doesn't over-widen; \
         got {:?}, expected close to {:?}",
        l.last_seen,
        anchor_last
    );

    // Directly confirms the backfill leg's own row shape — timestamps from
    // `occurred_at`, all three counters zero — independent of the group-level
    // comparison above, which cannot see this by construction (the
    // transaction is deliberately not the group's extremum).
    let tx_row = rollup_row_for_device(&mut conn, ids.app_id, &tx_only_key).await;
    assert!(
        (tx_row.first_seen - tx_at).num_seconds().abs() < 2,
        "the backfill's transactions leg must set first_seen to the transaction's occurred_at"
    );
    assert!(
        (tx_row.last_seen - tx_at).num_seconds().abs() < 2,
        "the backfill's transactions leg must set last_seen to the transaction's occurred_at"
    );
    assert_eq!(
        tx_row.events_count, 0,
        "a transaction must not add to events_count"
    );
    assert_eq!(
        tx_row.errors_count, 0,
        "a transaction must not add to errors_count"
    );
    assert_eq!(
        tx_row.sessions_count, 0,
        "a transaction must not add to sessions_count"
    );

    drop(conn);
    db.cleanup().await;
}

/// The one `device_environments` row for `device_key` — used by
/// [`device_with_only_a_transaction_matches_live_and_rollup_shapes`] to check
/// the backfill's transactions leg directly, since the group-level comparison
/// in that test cannot see it (the fixture deliberately keeps the transaction
/// from being the group's extremum).
struct RollupRow {
    first_seen: chrono::DateTime<chrono::Utc>,
    last_seen: chrono::DateTime<chrono::Utc>,
    events_count: i64,
    errors_count: i64,
    sessions_count: i64,
}

async fn rollup_row_for_device(
    conn: &mut sauron_db::PgConn,
    app_id: uuid::Uuid,
    device_key: &str,
) -> RollupRow {
    #[derive(diesel::QueryableByName)]
    struct Row {
        #[diesel(sql_type = diesel::sql_types::Timestamptz)]
        first_seen: chrono::DateTime<chrono::Utc>,
        #[diesel(sql_type = diesel::sql_types::Timestamptz)]
        last_seen: chrono::DateTime<chrono::Utc>,
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        events_count: i64,
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        errors_count: i64,
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        sessions_count: i64,
    }
    let r: Row = diesel::sql_query(
        "SELECT first_seen, last_seen, events_count, errors_count, sessions_count \
         FROM device_environments WHERE app_id=$1 AND device_key=$2",
    )
    .bind::<diesel::sql_types::Uuid, _>(app_id)
    .bind::<diesel::sql_types::Text, _>(device_key.to_string())
    .get_result(conn)
    .await
    .expect("read back the transaction-only device's rollup row");
    RollupRow {
        first_seen: r.first_seen,
        last_seen: r.last_seen,
        events_count: r.events_count,
        errors_count: r.errors_count,
        sessions_count: r.sessions_count,
    }
}

/// Fix round 1 (Task 11): EVERY device in the group has ONLY a `transactions`
/// row — no analytics event, no error, no session, for ANY member. This is
/// what actually reproduces the 500 the coordinator found and reported: Task
/// 11's original `device_membership_sql` leg alone is enough to admit these
/// devices into `d` (the live shape's qualifying-devices subquery), but
/// until fix round 1 added the `tx` LATERAL, `ae`/`ee`/`se` were ALL NULL for
/// every one of them. Postgres's `LEAST`/`GREATEST` return NULL only when
/// EVERY argument is NULL — exactly this case — and `DeviceGroupRow`
/// declares `first_seen`/`last_seen` non-nullable `Timestamptz`, so the row
/// FAILS TO DESERIALIZE (`QueryResult::Err`), not "renders a wrong number".
/// `device_with_only_a_transaction_matches_live_and_rollup_shapes` (Task 11's
/// original test, above) deliberately avoided this by pairing the
/// transaction-only device with a real-signal anchor in the same group —
/// this test is the one that removes the anchor and hits it head-on.
///
/// Two devices, deliberately different transaction timestamps, so the
/// group's `first_seen`/`last_seen` prove the new `tx` LATERAL's `min`/`max`
/// are wired correctly rather than merely non-NULL by accident. Also asserts
/// `events_count`/`errors_count`/`sessions_count` all stay 0 — the `tx`
/// LATERAL must supply NO count (a transaction is neither an event nor an
/// error); a `count(*)` accidentally wired into either counter would pass
/// every OTHER assertion here and only this one would catch it.
#[tokio::test]
async fn device_group_where_every_device_is_transaction_only_matches_live_and_rollup() {
    let Some(db) = TestDb::setup().await else {
        panic!("TEST_DATABASE_URL unset — this test must not silently skip");
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let dev_a = format!("tx-all-a-{suffix}");
    let dev_b = format!("tx-all-b-{suffix}");
    let model = "TX-ALL";

    let now = chrono::Utc::now();
    let tx_a_at = now - chrono::Duration::hours(5);
    let tx_b_at = now - chrono::Duration::hours(1);

    for key in [&dev_a, &dev_b] {
        sauron_db::repo::bump_device(
            &mut conn,
            ids.app_id,
            key,
            Some("TxBrand"),
            Some(model),
            Some("TxOS"),
            Some("1.0"),
            None,
            None,
            None,
            now,
            0,
            0,
        )
        .await
        .expect("seed devices row");
    }

    // The ONLY environment-scoped signal for either device: no
    // analytics_events, no error_events, no sessions row for either at all.
    for (key, at) in [(&dev_a, tx_a_at), (&dev_b, tx_b_at)] {
        diesel::sql_query(
            "INSERT INTO transactions \
               (id, app_id, environment_id, name, op, duration_ms, device_key, \
                occurred_at, received_at) \
             VALUES (gen_random_uuid(), $1, $2, 'tx.only', 'test', 12.5, $3, $4, now())",
        )
        .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
        .bind::<diesel::sql_types::Uuid, _>(ids.env_a)
        .bind::<diesel::sql_types::Text, _>(key.clone())
        .bind::<diesel::sql_types::Timestamptz, _>(at)
        .execute(&mut conn)
        .await
        .expect("seed transaction-only signal");
    }

    let since = now - chrono::Duration::days(30);

    assert_eq!(
        rollup_rows(&mut conn, ids.app_id).await,
        0,
        "precondition: device_environments must be empty before the backfill runs"
    );

    // THE PROOF: this call must not fail to deserialize. Without the `tx`
    // LATERAL, `first_seen`/`last_seen` are NULL for this all-transaction-only
    // group and `.expect()` panics on a diesel deserialization error here —
    // exactly the 500 this fix round closes.
    let live = sauron_db::repo::list_device_groups(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        TimeWindow::since("last_seen", since),
        200,
        0,
        group_sort(),
        None,
    )
    .await
    .expect("live list_device_groups must not fail to deserialize");

    let l = group(&live, model);
    assert_eq!(
        l.device_count, 2,
        "both transaction-only devices must be admitted"
    );
    assert_eq!(
        l.events_count, 0,
        "a transaction must not add to events_count"
    );
    assert_eq!(
        l.errors_count, 0,
        "a transaction must not add to errors_count"
    );
    assert_eq!(
        l.sessions_count, 0,
        "a transaction must not add to sessions_count"
    );
    assert!(
        (l.first_seen - tx_a_at).num_seconds().abs() < 2,
        "first_seen must be the earlier transaction's occurred_at, got {:?}, expected close to {:?}",
        l.first_seen, tx_a_at
    );
    assert!(
        (l.last_seen - tx_b_at).num_seconds().abs() < 2,
        "last_seen must be the later transaction's occurred_at, got {:?}, expected close to {:?}",
        l.last_seen,
        tx_b_at
    );

    let cutoff = now + chrono::Duration::days(1);
    sauron_db::device_env_backfill::backfill_app(&mut conn, ids.app_id, cutoff)
        .await
        .expect("backfill");
    assert!(
        sauron_db::device_env_backfill::is_backfilled(&mut conn, ids.app_id)
            .await
            .expect("marker"),
        "the marker must be set, or the comparison below re-reads the live shape again"
    );

    let rollup = sauron_db::repo::list_device_groups(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        TimeWindow::since("last_seen", since),
        200,
        0,
        group_sort(),
        None,
    )
    .await
    .expect("rollup list_device_groups");
    let r = group(&rollup, model);

    assert_eq!(l.device_count, r.device_count, "device_count");
    assert_eq!(l.events_count, r.events_count, "events_count");
    assert_eq!(l.errors_count, r.errors_count, "errors_count");
    assert_eq!(l.sessions_count, r.sessions_count, "sessions_count");
    assert_eq!(l.first_seen, r.first_seen, "first_seen");
    assert_eq!(l.last_seen, r.last_seen, "last_seen");

    drop(conn);
    db.cleanup().await;
}

/// The default ordering `routes::devices::list` builds for an absent `sort`
/// parameter — matches `device_groups.rs`'s own `device_sort()`. A function,
/// not a constant, for the same reason as `group_sort()` above: `SortSpec` is
/// deliberately not `Clone`.
fn device_sort() -> sauron_db::repo::SortSpec {
    sauron_db::repo::SortSpec {
        column: "last_seen",
        descending: true,
        tiebreak: "d.device_key",
        nulls_last: false,
    }
}

/// Fix round 1 (Task 11): `list_devices` shares `device_membership_sql` with
/// `list_device_groups`, but unlike it, `list_devices` has only ONE shape —
/// its own doc comment says so explicitly ("THIS function has NOT been moved
/// onto the rollup") and there is no `is_backfilled` branch or `_rollup_sql`
/// variant anywhere in its body. So there is no second shape to compare
/// against here, and no backfill step in this test — the proof is simpler
/// and sharper than the group-level one above: a SINGLE transaction-only
/// device anywhere on the page is enough to hit the same NULL-`first_seen`/
/// `last_seen` deserialization failure, no anchor and no second device
/// required, because this function is per-DEVICE rather than per-group.
/// `DeviceRow.first_seen`/`last_seen` are non-nullable `Timestamptz`
/// (`repo.rs:6732-6735` as of this fix round), exactly like `DeviceGroupRow`.
#[tokio::test]
async fn list_devices_with_a_transaction_only_device_does_not_fail_to_deserialize() {
    let Some(db) = TestDb::setup().await else {
        panic!("TEST_DATABASE_URL unset — this test must not silently skip");
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let device_key = format!("tx-solo-{suffix}");
    let now = chrono::Utc::now();
    let tx_at = now - chrono::Duration::hours(2);

    sauron_db::repo::bump_device(
        &mut conn,
        ids.app_id,
        &device_key,
        Some("TxBrand"),
        Some("TX-SOLO"),
        Some("TxOS"),
        Some("1.0"),
        None,
        None,
        None,
        now,
        0,
        0,
    )
    .await
    .expect("seed devices row");

    // The device's ONLY environment-scoped signal: no analytics_events, no
    // error_events, no sessions row at all.
    diesel::sql_query(
        "INSERT INTO transactions \
           (id, app_id, environment_id, name, op, duration_ms, device_key, \
            occurred_at, received_at) \
         VALUES (gen_random_uuid(), $1, $2, 'tx.only', 'test', 12.5, $3, $4, now())",
    )
    .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
    .bind::<diesel::sql_types::Uuid, _>(ids.env_a)
    .bind::<diesel::sql_types::Text, _>(device_key.clone())
    .bind::<diesel::sql_types::Timestamptz, _>(tx_at)
    .execute(&mut conn)
    .await
    .expect("seed transaction-only signal");

    let since = now - chrono::Duration::days(30);

    // THE PROOF: this call must not fail to deserialize. Without the `tx`
    // LATERAL, first_seen/last_seen are NULL for this device and `.expect()`
    // panics on a diesel deserialization error — exactly the 500 this fix
    // round closes, and the coordinator's report called out as WORSE than the
    // group-level case: a single such device anywhere on the page is enough.
    let rows = sauron_db::repo::list_devices(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        TimeWindow::since("last_seen", since),
        200,
        0,
        device_sort(),
        None,
        None,
    )
    .await
    .expect("list_devices must not fail to deserialize a transaction-only device");

    let d = rows
        .iter()
        .find(|d| d.device_key == device_key)
        .unwrap_or_else(|| {
            panic!(
                "transaction-only device missing from list_devices — got {:?}",
                rows.iter().map(|d| &d.device_key).collect::<Vec<_>>()
            )
        });

    assert_eq!(
        d.events_count, 0,
        "a transaction must not add to events_count"
    );
    assert_eq!(
        d.errors_count, 0,
        "a transaction must not add to errors_count"
    );
    assert_eq!(
        d.sessions_count, 0,
        "a transaction must not add to sessions_count"
    );
    assert!(
        (d.first_seen - tx_at).num_seconds().abs() < 2,
        "first_seen must be the transaction's occurred_at, got {:?}, expected close to {:?}",
        d.first_seen,
        tx_at
    );
    assert!(
        (d.last_seen - tx_at).num_seconds().abs() < 2,
        "last_seen must be the transaction's occurred_at, got {:?}, expected close to {:?}",
        d.last_seen,
        tx_at
    );

    drop(conn);
    db.cleanup().await;
}

/// Fix round 2 (Task 11): `get_device` — the Device Detail page's
/// single-identity lookup — carries its OWN independently hand-written copy
/// of the membership predicate `list_devices`/`list_device_groups` share via
/// `device_membership_sql`. Fix round 1 gave the shared function a
/// transactions leg but could not touch this copy (round 1's instructions
/// named only the other two); the result was a genuinely worse regression
/// than before round 1 existed: a transaction-only device now shows up in
/// the `/devices` list (shared predicate, admits it) and 404s when clicked
/// (this stale copy, still three legs, rejects it) — before round 1 it was
/// consistently invisible in both places. Round 2 closes that gap: the same
/// two-part fix (a fourth `EXISTS` leg for membership, a fourth
/// `LEFT JOIN LATERAL` for `first_seen`/`last_seen`, no counts) applied here
/// too.
///
/// Single test, two independent mutations, because `get_device`'s two halves
/// fail in two DIFFERENT, diagnostically distinct ways when only one is
/// missing:
/// - membership leg missing, LATERAL present: the device fails every leg of
///   `membership_sql`'s `WHERE`, so the outer subquery returns zero rows;
///   `get_result().optional()` turns diesel's `NotFound` into `Ok(None)` —
///   `get_device` reports the device does not exist. NOT an error, so the
///   `.expect(...)` below on the outer `QueryResult` succeeds and the
///   `.unwrap_or_else(...)` on the inner `Option` is what panics.
/// - LATERAL missing, membership leg present: the device NOW passes
///   membership (thanks to the leg), so the outer subquery returns exactly
///   one row — but `ae`/`ee`/`se` are all NULL for it (no analytics/error/
///   session row exists), and without the `tx` LATERAL nothing else can
///   supply `first_seen`/`last_seen`. `LEAST`/`GREATEST` return NULL, and
///   `DeviceRow` declares both columns non-nullable `Timestamptz`, so diesel
///   fails to deserialize the one row it got: `Err(DeserializationError(
///   UnexpectedNullError))`, propagated THROUGH `.optional()` unchanged
///   (`.optional()` only intercepts `NotFound`) — the `.expect(...)` on the
///   outer `QueryResult` is what panics this time, with the exact 500-class
///   error round 1 already reproduced for the other two functions.
#[tokio::test]
async fn get_device_with_only_a_transaction_signal_is_found_with_non_null_timestamps() {
    let Some(db) = TestDb::setup().await else {
        panic!("TEST_DATABASE_URL unset — this test must not silently skip");
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let device_key = format!("tx-getdev-{suffix}");
    let now = chrono::Utc::now();
    let tx_at = now - chrono::Duration::hours(2);

    sauron_db::repo::bump_device(
        &mut conn,
        ids.app_id,
        &device_key,
        Some("TxBrand"),
        Some("TX-GETDEV"),
        Some("TxOS"),
        Some("1.0"),
        None,
        None,
        None,
        now,
        0,
        0,
    )
    .await
    .expect("seed devices row");

    // The device's ONLY environment-scoped signal: no analytics_events, no
    // error_events, no sessions row at all.
    diesel::sql_query(
        "INSERT INTO transactions \
           (id, app_id, environment_id, name, op, duration_ms, device_key, \
            occurred_at, received_at) \
         VALUES (gen_random_uuid(), $1, $2, 'tx.only', 'test', 12.5, $3, $4, now())",
    )
    .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
    .bind::<diesel::sql_types::Uuid, _>(ids.env_a)
    .bind::<diesel::sql_types::Text, _>(device_key.clone())
    .bind::<diesel::sql_types::Timestamptz, _>(tx_at)
    .execute(&mut conn)
    .await
    .expect("seed transaction-only signal");

    // THE PROOF: this chain must not fail to deserialize (LATERAL present)
    // AND must find the device (membership leg present).
    let row = sauron_db::repo::get_device(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        &device_key,
    )
    .await
    .expect("get_device must not fail to deserialize a transaction-only device")
    .unwrap_or_else(|| {
        panic!("transaction-only device must be found by get_device under One(env_a)")
    });

    assert_eq!(row.device_key, device_key);
    assert_eq!(
        row.events_count, 0,
        "a transaction must not add to events_count"
    );
    assert_eq!(
        row.errors_count, 0,
        "a transaction must not add to errors_count"
    );
    assert_eq!(
        row.sessions_count, 0,
        "a transaction must not add to sessions_count"
    );
    assert!(
        (row.first_seen - tx_at).num_seconds().abs() < 2,
        "first_seen must be the transaction's occurred_at, got {:?}, expected close to {:?}",
        row.first_seen,
        tx_at
    );
    assert!(
        (row.last_seen - tx_at).num_seconds().abs() < 2,
        "last_seen must be the transaction's occurred_at, got {:?}, expected close to {:?}",
        row.last_seen,
        tx_at
    );

    // Under `Unattributed`, this device must NOT be found — its only signal
    // is attributed to env_a, not NULL. Cheap extra confirmation that the new
    // leg's env predicate is wired to the real bind, not a tautology (an
    // unqualified or unbound `tx` leg would incorrectly match here too).
    let unattributed = sauron_db::repo::get_device(
        &mut conn,
        ReadScope::new(ids.app_id, EnvFilter::Unattributed),
        &device_key,
    )
    .await
    .expect("get_device under Unattributed must not fail to deserialize");
    assert!(
        unattributed.is_none(),
        "a device whose only signal is attributed to env_a must not appear under Unattributed"
    );

    drop(conn);
    db.cleanup().await;
}

/// Not an assertion — a printer. `cargo test -p sauron-db --test device_env_rollup
/// print_device_group_sql -- --nocapture --ignored` emits the exact strings
/// `list_device_groups` executes, so a measurement runs the real query rather
/// than a hand-transcribed lookalike (EXPLAIN over a retyped query measures the
/// wrong thing — the trap the persons twin's design doc records).
#[tokio::test]
#[ignore]
async fn print_device_group_sql() {
    let env = EnvFilter::One(uuid::Uuid::nil());
    println!("=====LIVE=====");
    println!(
        "{}",
        sauron_db::repo::list_device_groups_sql_for_test(env.clone())
    );
    println!("=====ROLLUP=====");
    println!(
        "{}",
        sauron_db::repo::list_device_groups_rollup_sql_for_test(env)
    );
}

/// A session is bumped again by every batch carrying a signal for it, so
/// `sessions_count` may only be credited from the keys `bump_sessions` actually
/// INSERTED. Crediting per bump instead counts one session once per batch it
/// spans — an error that grows with session length and that a single-batch test
/// cannot see, which is why this drives the SAME session through two batches.
#[tokio::test]
async fn sessions_count_counts_each_session_once_across_batches() {
    let Some(db) = TestDb::setup().await else {
        panic!("TEST_DATABASE_URL unset — this test must not silently skip");
    };
    let mut conn = db.conn().await;
    let app_id = db.seed_two_envs().await.app_id;
    let now = chrono::Utc::now();

    let session = |ev: i64| sauron_db::batch::SessionBump {
        app_id,
        session_id: "s-1".into(),
        distinct_id: Some("p-1".into()),
        device_key: Some("dev-1".into()),
        first_at: now,
        last_at: now,
        context: serde_json::json!({}),
        release: None,
        environment_id: None,
        ip: None,
        events_delta: ev,
        errors_delta: 0,
    };

    for _ in 0..2 {
        let s = [session(1)];
        sauron_db::batch::write_rows(
            &mut conn,
            sauron_db::batch::WriteSet {
                errors: &[],
                analytics: &[],
                transactions: &[],
                sessions: &s,
                devices: &[],
                touch_users: &[],
                identified: &[],
                person_envs: &[],
                device_envs: &[],
            },
        )
        .await
        .expect("write batch");
    }

    #[derive(diesel::QueryableByName)]
    struct N {
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        sessions_count: i64,
    }
    let r: N = diesel::sql_query(
        "SELECT sessions_count FROM device_environments WHERE app_id=$1 AND device_key='dev-1'",
    )
    .bind::<diesel::sql_types::Uuid, _>(app_id)
    .get_result(&mut conn)
    .await
    .expect("rollup row must exist");

    assert_eq!(
        r.sessions_count, 1,
        "one session spanning two batches must count once, not twice"
    );

    drop(conn);
    db.cleanup().await;
}

/// `credit_device_sessions` is fed by TWO producers within a single write —
/// the pipeline's device/environment fold (arriving here via `device_envs`)
/// and the session that same batch newly inserted (arriving via `sessions`) —
/// and a device with both in one batch is one conflict key reached from two
/// directions. Passing both as separate rows to `bump_device_envs` raises `ON
/// CONFLICT DO UPDATE command cannot affect row a second time` and aborts the
/// whole batch, so this pins that the crediting step MERGES the session
/// credit into the row the fold already produced instead of pushing a second
/// row that shares its key.
///
/// No `distinct_id` on either row: an anonymous device — a device_key with no
/// person attached — is exactly the case the device rollup exists to cover on
/// its own, so the test stays true to that rather than smuggling a person in.
#[tokio::test]
async fn write_rows_merges_an_event_and_a_new_session_for_one_device() {
    let Some(db) = TestDb::setup().await else {
        panic!("TEST_DATABASE_URL unset — this test must not silently skip");
    };
    let mut conn = db.conn().await;
    let ids = db.seed_two_envs().await;
    let now = chrono::Utc::now();
    let dk = "wr-device".to_string();

    let session = sauron_db::batch::SessionBump {
        app_id: ids.app_id,
        session_id: "wr-device-session".into(),
        distinct_id: None,
        device_key: Some(dk.clone()),
        first_at: now,
        last_at: now,
        context: serde_json::json!({}),
        release: None,
        environment_id: Some(ids.env_a),
        ip: None,
        events_delta: 1,
        errors_delta: 0,
    };
    let device_env = sauron_db::batch::DeviceEnvBump {
        app_id: ids.app_id,
        device_key: dk.clone(),
        environment_id: Some(ids.env_a),
        first_at: now,
        last_at: now,
        events_delta: 1,
        errors_delta: 0,
        sessions_delta: 0,
    };

    sauron_db::batch::write_rows(
        &mut conn,
        sauron_db::batch::WriteSet {
            errors: &[],
            analytics: &[],
            transactions: &[],
            sessions: std::slice::from_ref(&session),
            devices: &[],
            touch_users: &[],
            identified: &[],
            person_envs: &[],
            device_envs: std::slice::from_ref(&device_env),
        },
    )
    .await
    .expect("write_rows must not abort on the two-producer conflict key");

    #[derive(diesel::QueryableByName)]
    struct Row {
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        n: i64,
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        events_count: i64,
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        sessions_count: i64,
    }
    let r: Row = diesel::sql_query(
        "SELECT count(*) AS n, COALESCE(max(events_count),0) AS events_count, \
                COALESCE(max(sessions_count),0) AS sessions_count \
         FROM device_environments WHERE app_id=$1 AND device_key=$2 AND environment_id=$3",
    )
    .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
    .bind::<diesel::sql_types::Text, _>(dk.clone())
    .bind::<diesel::sql_types::Uuid, _>(ids.env_a)
    .get_result(&mut conn)
    .await
    .expect("exactly one rollup row");

    assert_eq!(
        r.n, 1,
        "the event bump and the session credit must land on ONE row, not two"
    );
    assert_eq!(r.events_count, 1, "the event bump's own delta");
    assert_eq!(
        r.sessions_count, 1,
        "the newly-inserted session must be credited onto the row the event bump \
         folded, not dropped or split into a second row"
    );

    drop(conn);
    db.cleanup().await;
}
