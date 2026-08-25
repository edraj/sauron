//! The tier/rollup boundary: once `sauron-tier` has dropped a day's raw
//! partitions to Parquet, the consistency sweep must SKIP that day. Raw
//! `count(*)` is no longer the source of truth there, and the automatic
//! "heal" (`fold_day_from_raw` with `delete_first`) would wipe good rollups
//! and refill them from whatever raw is left — i.e. nothing.
//!
//! The counter-factual arm at the end is the point of the test: the same
//! missing-raw state MUST read as drift once the `tiering_state` row is gone,
//! which proves the earlier silence came from the guard, not from the sweep
//! failing to look.

mod common;

use chrono::{Duration, Utc};
use common::TestDb;
use diesel::sql_types::{BigInt, Date, Text, Timestamptz, Uuid as SqlUuid};
use diesel_async::RunQueryDsl;
use sauron_db::repo;
use sauron_db::rollups::fold;
use uuid::Uuid;

#[tokio::test]
async fn sweep_skips_days_dropped_to_cold_tier() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut conn = db.conn().await;

    let suffix = Uuid::new_v4().simple().to_string();
    let org = repo::create_org(&mut conn, "tier guard org", &format!("tier-guard-{suffix}"))
        .await
        .expect("create org");
    let project = repo::create_project(
        &mut conn,
        org.id,
        "tier guard project",
        &format!("tier-guard-{suffix}"),
    )
    .await
    .expect("create project");
    let app = repo::create_app(
        &mut conn,
        project.id,
        "tier guard app",
        &format!("tier-guard-{suffix}"),
        "web",
    )
    .await
    .expect("create app");

    // 20 events yesterday: comfortably above the sweep's 5-row absolute
    // tolerance, so a raw-vs-rollup gap of the full day is always reportable.
    let day = Utc::now().date_naive() - Duration::days(1);
    let noon = day.and_hms_opt(12, 0, 0).expect("valid").and_utc();
    for i in 0..20 {
        diesel::sql_query(
            "INSERT INTO analytics_events (app_id, name, distinct_id, occurred_at) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind::<SqlUuid, _>(app.id)
        .bind::<Text, _>("tier-guard-event")
        .bind::<Text, _>(format!("tier-guard-user-{i}"))
        .bind::<Timestamptz, _>(noon + Duration::seconds(i))
        .execute(&mut conn)
        .await
        .expect("insert analytics event");
    }
    fold::fold_day_from_raw(&mut conn, day, None, false, 50)
        .await
        .expect("fold day");

    let drifts = fold::consistency_check_trailing(&mut conn)
        .await
        .expect("sweep after fold");
    assert!(
        drifts.iter().all(|(d, _)| *d != day),
        "raw and rollups agree, sweep must be clean: {drifts:?}"
    );

    // Simulate the tier: the day's raw rows leave Postgres, and
    // `tiering_state.dropped_thru` records the boundary — exactly what
    // `sauron-tier` writes after `detach_and_drop_partition`.
    diesel::sql_query("DELETE FROM analytics_events WHERE app_id = $1")
        .bind::<SqlUuid, _>(app.id)
        .execute(&mut conn)
        .await
        .expect("simulate partition drop");
    let dropped_thru = (day + Duration::days(1))
        .and_hms_opt(0, 0, 0)
        .expect("valid")
        .and_utc();
    diesel::sql_query(
        "INSERT INTO tiering_state (table_name, watermark, dropped_thru, updated_at) \
         VALUES ('analytics_events', $1, $1, now())",
    )
    .bind::<Timestamptz, _>(dropped_thru)
    .execute(&mut conn)
    .await
    .expect("record tier boundary");

    let drifts = fold::consistency_check_trailing(&mut conn)
        .await
        .expect("sweep with tier boundary");
    assert!(
        drifts.iter().all(|(d, _)| *d != day),
        "a tiered-out day must be skipped, not reported as drift: {drifts:?}"
    );

    // Counter-factual: same missing raw, no tier boundary → the sweep MUST
    // report it. This is what makes the silence above the guard's doing.
    diesel::sql_query("DELETE FROM tiering_state WHERE table_name = 'analytics_events'")
        .execute(&mut conn)
        .await
        .expect("remove tier boundary");
    let drifts = fold::consistency_check_trailing(&mut conn)
        .await
        .expect("sweep without tier boundary");
    assert!(
        drifts
            .iter()
            .any(|(d, s)| *d == day && s.contains("analytics")),
        "without the tier boundary the missing raw must read as drift: {drifts:?}"
    );

    db.cleanup().await;
}

#[derive(diesel::QueryableByName)]
struct SumRow {
    #[diesel(sql_type = BigInt)]
    n: i64,
}

async fn rolled_sessions(conn: &mut sauron_db::PgConn, app: Uuid, day: chrono::NaiveDate) -> i64 {
    let r: SumRow = diesel::sql_query(
        "SELECT COALESCE(sum(sessions), 0)::bigint AS n \
         FROM session_stats_daily WHERE app_id = $1 AND day = $2",
    )
    .bind::<SqlUuid, _>(app)
    .bind::<Date, _>(day)
    .get_result(conn)
    .await
    .expect("sum session_stats_daily");
    r.n
}

/// Retention drops the partition but not the day's rollups, and the
/// `recompute_sessions` clamp keeps a stray late row in `sessions_default`
/// from re-dirtying the dropped day. The counter-factual arm at the end
/// (boundary row removed → the stray DOES wipe the day) is what proves the
/// clamp is load-bearing rather than the day merely never resurfacing.
#[tokio::test]
async fn session_retention_drops_partitions_and_spares_rollups() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut conn = db.conn().await;

    let suffix = Uuid::new_v4().simple().to_string();
    let org = repo::create_org(&mut conn, "retention org", &format!("retention-{suffix}"))
        .await
        .expect("create org");
    let project = repo::create_project(
        &mut conn,
        org.id,
        "retention project",
        &format!("retention-{suffix}"),
    )
    .await
    .expect("create project");
    let app = repo::create_app(
        &mut conn,
        project.id,
        "retention app",
        &format!("retention-{suffix}"),
        "web",
    )
    .await
    .expect("create app");

    // A real partition for a day 40 days back, the way migration 73 /
    // `ensure_session_partitions` would have made it.
    let old_day = Utc::now().date_naive() - Duration::days(40);
    let part = format!("sessions_{}", old_day.format("%Y_%m_%d"));
    let lo = old_day.and_hms_opt(0, 0, 0).expect("valid").and_utc();
    let hi = lo + Duration::days(1);
    diesel::sql_query(format!(
        "CREATE TABLE {part} PARTITION OF sessions \
         FOR VALUES FROM ('{}') TO ('{}')",
        lo.to_rfc3339(),
        hi.to_rfc3339()
    ))
    .execute(&mut conn)
    .await
    .expect("create old sessions partition");

    for i in 0..5 {
        diesel::sql_query(
            "INSERT INTO sessions (app_id, session_id, started_at, last_event_at) \
             VALUES ($1, $2, $3, $3)",
        )
        .bind::<SqlUuid, _>(app.id)
        .bind::<Text, _>(format!("retention-session-{i}"))
        .bind::<Timestamptz, _>(lo + Duration::hours(1 + i))
        .execute(&mut conn)
        .await
        .expect("insert session");
    }
    fold::recompute_sessions(&mut conn, None)
        .await
        .expect("recompute");
    assert_eq!(rolled_sessions(&mut conn, app.id, old_day).await, 5);

    // 40 days old, 30-day retention: exactly one partition goes.
    let dropped = fold::enforce_session_retention(&mut conn, 30)
        .await
        .expect("enforce retention");
    assert_eq!(dropped, 1, "the old partition must be dropped");
    let gone: SumRow =
        diesel::sql_query("SELECT count(*)::bigint AS n FROM pg_class WHERE relname = $1")
            .bind::<Text, _>(part.clone())
            .get_result(&mut conn)
            .await
            .expect("pg_class probe");
    assert_eq!(gone.n, 0, "partition {part} must no longer exist");
    assert_eq!(
        rolled_sessions(&mut conn, app.id, old_day).await,
        5,
        "retention must not touch the day's rollups"
    );
    assert_eq!(
        fold::sessions_dropped_floor(&mut conn)
            .await
            .expect("floor read"),
        Some(hi),
        "boundary must be recorded in tiering_state"
    );

    // A late arrival for the dropped day lands in `sessions_default`. With
    // the boundary recorded, a full recompute must leave the day untouched.
    diesel::sql_query(
        "INSERT INTO sessions (app_id, session_id, started_at, last_event_at) \
         VALUES ($1, $2, $3, $3)",
    )
    .bind::<SqlUuid, _>(app.id)
    .bind::<Text, _>("retention-stray".to_string())
    .bind::<Timestamptz, _>(lo + Duration::hours(12))
    .execute(&mut conn)
    .await
    .expect("insert stray session");
    fold::recompute_sessions(&mut conn, None)
        .await
        .expect("recompute with boundary");
    assert_eq!(
        rolled_sessions(&mut conn, app.id, old_day).await,
        5,
        "a stray must not re-dirty a retention-dropped day"
    );

    // Counter-factual: remove the boundary and the same stray replaces the
    // whole day — 5 becomes 1. This is the wipe the clamp exists to prevent.
    diesel::sql_query("DELETE FROM tiering_state WHERE table_name = 'sessions'")
        .execute(&mut conn)
        .await
        .expect("remove boundary");
    fold::recompute_sessions(&mut conn, None)
        .await
        .expect("recompute without boundary");
    assert_eq!(
        rolled_sessions(&mut conn, app.id, old_day).await,
        1,
        "without the boundary the stray must wipe the day (proves the clamp)"
    );

    db.cleanup().await;
}
