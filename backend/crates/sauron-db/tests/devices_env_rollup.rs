//! `list_devices` / `count_devices` under an environment scope must read the
//! `device_environments` rollup once the app's device backfill has run —
//! the same swap `list_device_groups` already makes — and must page BEFORE
//! resolving each device's latest distinct_id.
//!
//! Why: the live scoped shape decides membership with four unbounded EXISTS
//! per device and computes counts with three LATERALs per device, all before
//! `LIMIT`. Measured at 30M rows / 50k devices: the Devices page with an
//! environment selected (the dashboard default) hit the 60 s request timeout
//! even AFTER every backfill, because this endpoint had no rollup path at all.
//!
//! Skips (does not fail) when `TEST_DATABASE_URL` is unset — see `common`.

mod common;

use chrono::{DateTime, Duration, Utc};
use common::TestDb;
use sauron_db::repo::{self, DeviceRow, SortSpec, TimeWindow};
use sauron_db::scope::{EnvFilter, ReadScope};

fn device_sort() -> SortSpec {
    SortSpec {
        column: "last_seen",
        descending: true,
        tiebreak: "d.device_key",
        nulls_last: false,
    }
}

fn far_past() -> DateTime<Utc> {
    Utc::now() - Duration::days(3650)
}

fn key(
    r: &DeviceRow,
) -> (
    String,
    i64,
    i64,
    i64,
    DateTime<Utc>,
    DateTime<Utc>,
    Option<String>,
) {
    (
        r.device_key.clone(),
        r.events_count,
        r.errors_count,
        r.sessions_count,
        r.first_seen,
        r.last_seen,
        r.last_distinct_id.clone(),
    )
}

#[test]
fn scoped_rollup_shape_reads_device_environments_and_pages_before_the_latest_id_probe() {
    let sql = repo::list_devices_sql_for_test(EnvFilter::One(uuid::Uuid::nil()), true);
    assert!(
        sql.contains("FROM device_environments"),
        "membership + counts come from the rollup:\n{sql}"
    );
    assert!(
        !sql.contains("SELECT count(*) AS cnt, min(occurred_at) AS min_occurred"),
        "no per-device LATERAL aggregates on the rollup shape:\n{sql}"
    );
    let limit = sql.find("LIMIT $4 OFFSET $5").expect("paged");
    let probe = sql
        .find("SELECT distinct_id FROM (")
        .expect("latest-id probe present");
    assert!(
        limit < probe,
        "the latest-id probe must run on the page, not on every device:\n{sql}"
    );

    let live = repo::list_devices_sql_for_test(EnvFilter::One(uuid::Uuid::nil()), false);
    assert!(
        !live.contains("FROM device_environments"),
        "live shape untouched:\n{live}"
    );
}

#[tokio::test]
async fn scoped_rollup_and_live_shapes_agree_page_for_page() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let scopes = [
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_a)),
        ReadScope::new(ids.app_id, EnvFilter::One(ids.env_b)),
        ReadScope::new(ids.app_id, EnvFilter::Unattributed),
        ReadScope::all(ids.app_id),
    ];
    let mut live = Vec::new();
    for scope in &scopes {
        for (limit, offset) in [(50, 0), (2, 0), (2, 2)] {
            let rows = repo::list_devices(
                &mut conn,
                scope.clone(),
                TimeWindow::since("last_seen", far_past()),
                limit,
                offset,
                device_sort(),
                None,
                None,
            )
            .await
            .expect("live list");
            let count = repo::count_devices(
                &mut conn,
                scope.clone(),
                TimeWindow::since("last_seen", far_past()),
                None,
                None,
                1000,
            )
            .await
            .expect("live count");
            live.push((rows.iter().map(key).collect::<Vec<_>>(), count));
        }
    }
    assert!(
        live.iter().any(|(rows, _)| !rows.is_empty()),
        "fixture must have devices"
    );

    sauron_db::device_env_backfill::backfill_all(db.pool())
        .await
        .expect("device env backfill");
    assert!(
        sauron_db::device_env_backfill::is_backfilled(&mut conn, ids.app_id)
            .await
            .unwrap()
    );

    let mut i = 0;
    for scope in &scopes {
        for (limit, offset) in [(50, 0), (2, 0), (2, 2)] {
            let rows = repo::list_devices(
                &mut conn,
                scope.clone(),
                TimeWindow::since("last_seen", far_past()),
                limit,
                offset,
                device_sort(),
                None,
                None,
            )
            .await
            .expect("rollup list");
            let count = repo::count_devices(
                &mut conn,
                scope.clone(),
                TimeWindow::since("last_seen", far_past()),
                None,
                None,
                1000,
            )
            .await
            .expect("rollup count");
            let got = (rows.iter().map(key).collect::<Vec<_>>(), count);
            assert_eq!(
                got, live[i],
                "scope {:?} limit {limit} offset {offset}",
                scope.env
            );
            i += 1;
        }
    }
    drop(conn);
    db.cleanup().await;
}
