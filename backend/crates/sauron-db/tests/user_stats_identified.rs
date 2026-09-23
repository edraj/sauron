//! `user_stats` reports how many of `total_users` / `active_in_range` /
//! `new_in_range` are IDENTIFIED (`event_users.identified_at IS NOT NULL`), so
//! the Audience tiles can show the identified/guest split beside each count.
//! Guests are the remainder; the wire carries only the identified figure.

mod common;

use common::TestDb;
use diesel::prelude::*;
use diesel::sql_types::{BigInt, Uuid as SqlUuid};
use diesel_async::RunQueryDsl;
use sauron_db::repo;
use sauron_db::scope::Range;
use sauron_db::scope::ReadScope;

#[derive(QueryableByName)]
struct Oracle {
    #[diesel(sql_type = BigInt)]
    total: i64,
    #[diesel(sql_type = BigInt)]
    identified: i64,
}

async fn oracle(conn: &mut sauron_db::PgConn, app_id: uuid::Uuid) -> Oracle {
    diesel::sql_query(
        "SELECT count(*)::bigint AS total, \
                count(*) FILTER (WHERE identified_at IS NOT NULL)::bigint AS identified \
         FROM event_users WHERE app_id = $1",
    )
    .bind::<SqlUuid, _>(app_id)
    .get_result(conn)
    .await
    .expect("oracle")
}

#[tokio::test]
async fn identified_legs_follow_the_identified_flag() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let now = chrono::Utc::now();
    let range = Range::since(now - chrono::Duration::days(3650));

    let before = oracle(&mut conn, ids.app_id).await;
    let s = repo::user_stats(&mut conn, ReadScope::all(ids.app_id), range, now)
        .await
        .expect("user_stats");
    assert_eq!(s.total_users, before.total, "fixture total");
    assert_eq!(s.total_identified, before.identified, "total_identified");
    // Every fixture person is inside a 10-year window, so the three legs agree.
    assert_eq!(s.active_identified, before.identified, "active_identified");
    assert_eq!(s.new_identified, before.identified, "new_identified");
    assert!(
        s.total_identified < s.total_users,
        "fixture must hold at least one guest, or the split is untested"
    );

    // Identify a brand-new person: every leg moves by exactly one, and the
    // identified count can never exceed the total.
    common::seed_identified_user(&mut conn, ids.app_id, "identified-probe").await;
    let s2 = repo::user_stats(&mut conn, ReadScope::all(ids.app_id), range, now)
        .await
        .expect("user_stats");
    assert_eq!(s2.total_users, s.total_users + 1);
    assert_eq!(s2.total_identified, s.total_identified + 1);
    assert_eq!(s2.active_identified, s.active_identified + 1);
    assert_eq!(s2.new_identified, s.new_identified + 1);
    assert!(s2.total_identified <= s2.total_users);

    // A guest moves the totals but not the identified legs.
    repo::touch_event_user(&mut conn, ids.app_id, "guest-probe")
        .await
        .expect("guest");
    let s3 = repo::user_stats(&mut conn, ReadScope::all(ids.app_id), range, now)
        .await
        .expect("user_stats");
    assert_eq!(s3.total_users, s2.total_users + 1);
    assert_eq!(s3.total_identified, s2.total_identified);
    assert_eq!(s3.new_identified, s2.new_identified);

    drop(conn);
    db.cleanup().await;
}

#[derive(QueryableByName)]
struct DauOracle {
    #[diesel(sql_type = BigInt)]
    identified: i64,
}

/// DAU/WAU/MAU's identified share on the legacy (exact) path: distinct ids
/// active in the span whose `event_users` row is identified.
async fn dau_oracle(
    conn: &mut sauron_db::PgConn,
    app_id: uuid::Uuid,
    since: chrono::DateTime<chrono::Utc>,
) -> i64 {
    diesel::sql_query(
        "SELECT count(DISTINCT d.distinct_id)::bigint AS identified FROM ( \
            SELECT distinct_id FROM analytics_events WHERE app_id=$1 AND occurred_at >= $2 AND distinct_id <> '' \
            UNION ALL \
            SELECT distinct_id FROM error_events WHERE app_id=$1 AND occurred_at >= $2 AND distinct_id IS NOT NULL AND distinct_id <> '' \
         ) d JOIN event_users eu ON eu.app_id=$1 AND eu.distinct_id = d.distinct_id AND eu.identified_at IS NOT NULL",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<diesel::sql_types::Timestamptz, _>(since)
    .get_result::<DauOracle>(conn)
    .await
    .expect("dau oracle")
    .identified
}

#[tokio::test]
async fn dau_wau_mau_carry_an_identified_share_on_the_legacy_path() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    // Anchored to the fixture's clock so every seeded signal is inside `dau`.
    let now = ids.pinned_now + chrono::Duration::hours(1);
    let range = Range::since(now - chrono::Duration::days(3650));
    let s = repo::user_stats(&mut conn, ReadScope::all(ids.app_id), range, now)
        .await
        .expect("user_stats");
    let d1 = dau_oracle(&mut conn, ids.app_id, now - chrono::Duration::days(1)).await;
    let d7 = dau_oracle(&mut conn, ids.app_id, now - chrono::Duration::days(7)).await;
    let d30 = dau_oracle(&mut conn, ids.app_id, now - chrono::Duration::days(30)).await;
    assert!(s.dau > 0, "fixture must have active people today");
    assert_eq!(s.dau_identified, Some(d1), "dau_identified");
    assert_eq!(s.wau_identified, Some(d7), "wau_identified");
    assert_eq!(s.mau_identified, Some(d30), "mau_identified");
    assert!(s.dau_identified.unwrap() <= s.dau);
    assert!(
        s.dau_identified.unwrap() < s.dau,
        "fixture must hold an active guest, or the split is untested"
    );
    drop(conn);
    db.cleanup().await;
}
