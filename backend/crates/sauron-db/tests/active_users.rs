//! The Active Users metric: distinct people per UTC day.
//!
//! The definition is one sentence and every test here defends one clause of it:
//!
//!   **Count distinct `analytics_events.distinct_id` per UTC day.**
//!
//! Two things make it easy to get subtly wrong and hard to notice:
//!
//!  1. `count(DISTINCT distinct_id)` written as `count(*)` passes every smoke
//!     test on a fixture with one event per person, and then over-reports by the
//!     average events-per-user ratio on real traffic — a number that looks like
//!     growth. `one_person_active_twice_in_a_day_counts_once` is the guard.
//!  2. The metric is HOLISTIC, so it cannot be summed. A per-day series is safe
//!     to concatenate across tiers only because a day belongs to one tier; a
//!     total over the range is not. Nothing here may be added up.
//!
//! Skips (does not fail) when `TEST_DATABASE_URL` is unset — see `common/mod.rs`.

mod common;

use chrono::{TimeZone, Utc};
use sauron_db::repo;
use sauron_db::scope::{EnvFilter, ReadScope};

use common::{seed_signal_event, TestDb};

/// Midday on a fixed day, so nothing here depends on when the suite runs and no
/// event can drift across a UTC midnight.
fn day(d: u32, hour: u32) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 5, d, hour, 0, 0).unwrap()
}

fn app_scope(app_id: uuid::Uuid) -> ReadScope {
    ReadScope {
        app_id,
        env: EnvFilter::All,
    }
}

#[tokio::test]
async fn counts_distinct_people_per_day_not_events() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut c = db.conn().await;

    // Day 10: alice twice, bob once  -> 2 people, 3 events.
    // Day 11: alice once            -> 1 person.
    seed_signal_event(&mut c, ids.app_id, None, "alice", day(10, 9)).await;
    seed_signal_event(&mut c, ids.app_id, None, "alice", day(10, 17)).await;
    seed_signal_event(&mut c, ids.app_id, None, "bob", day(10, 12)).await;
    seed_signal_event(&mut c, ids.app_id, None, "alice", day(11, 9)).await;

    let series =
        repo::active_users_by_day_hot(&mut c, app_scope(ids.app_id), day(10, 0), day(12, 0))
            .await
            .unwrap();

    let got: Vec<(String, i64)> = series
        .iter()
        .map(|r| (r.day.to_string(), r.count))
        .collect();
    assert_eq!(
        got,
        vec![("2026-05-10".to_string(), 2), ("2026-05-11".to_string(), 1),],
        "2 people on day 10 despite 3 events, and alice counts again on day 11"
    );
}

/// The clause that separates this metric from an event count. `count(*)` would
/// report 3 here.
#[tokio::test]
async fn one_person_active_twice_in_a_day_counts_once() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut c = db.conn().await;

    for h in [8, 12, 20] {
        seed_signal_event(&mut c, ids.app_id, None, "solo", day(10, h)).await;
    }
    let series =
        repo::active_users_by_day_hot(&mut c, app_scope(ids.app_id), day(10, 0), day(11, 0))
            .await
            .unwrap();
    assert_eq!(series.len(), 1);
    assert_eq!(series[0].count, 1, "three events, one person");
}

/// A person active on two days counts on BOTH. Stated explicitly because the
/// tempting "distinct users in the range" reading gives 1, and the two readings
/// are different metrics that agree on most fixtures.
#[tokio::test]
async fn the_same_person_counts_on_every_day_they_are_active() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut c = db.conn().await;

    for d in [10u32, 11, 12] {
        seed_signal_event(&mut c, ids.app_id, None, "repeat", day(d, 10)).await;
    }
    let series =
        repo::active_users_by_day_hot(&mut c, app_scope(ids.app_id), day(10, 0), day(13, 0))
            .await
            .unwrap();
    assert_eq!(series.len(), 3, "one row per active day");
    assert!(series.iter().all(|r| r.count == 1));
    // Deliberately NOT asserting a sum of 3 as a "total users" figure — that sum
    // is 1 person, and writing the assertion that way is how a holistic metric
    // gets turned into an additive one by the next reader.
}

/// `distinct_id` is `NOT NULL DEFAULT ''`, so empty means "this client sent no
/// identity at all" — server SDKs by design, and mobile clients predating the
/// anonymous id. Those rows are excluded rather than counted as one shared
/// person, which is what an unguarded `count(DISTINCT distinct_id)` would do:
/// every anonymous event in the deployment would collapse into a single
/// perpetual user.
#[tokio::test]
async fn events_with_no_identity_are_excluded_not_counted_as_one_person() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut c = db.conn().await;

    seed_signal_event(&mut c, ids.app_id, None, "", day(10, 9)).await;
    seed_signal_event(&mut c, ids.app_id, None, "", day(10, 10)).await;
    let empty_only =
        repo::active_users_by_day_hot(&mut c, app_scope(ids.app_id), day(10, 0), day(11, 0))
            .await
            .unwrap();
    assert!(
        empty_only.is_empty(),
        "a day of identity-less events has no active users, not one"
    );

    // And a real person on the same day is unaffected by their presence.
    seed_signal_event(&mut c, ids.app_id, None, "alice", day(10, 11)).await;
    let mixed =
        repo::active_users_by_day_hot(&mut c, app_scope(ids.app_id), day(10, 0), day(11, 0))
            .await
            .unwrap();
    assert_eq!(mixed.len(), 1);
    assert_eq!(mixed[0].count, 1);
}

/// Half-open `[from, to)`, matching every other range in this codebase. An
/// inclusive end would double-count the boundary day whenever a caller pages
/// through a series.
#[tokio::test]
async fn the_range_is_half_open() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut c = db.conn().await;

    seed_signal_event(&mut c, ids.app_id, None, "a", day(10, 12)).await;
    // EXACTLY at the exclusive end, not merely after it. An earlier version put
    // this at 12:00 on day 11, which an inclusive `<= to` still excluded — so the
    // test passed either way and a mutation run caught that it proved nothing.
    seed_signal_event(&mut c, ids.app_id, None, "b", day(11, 0)).await;

    let series =
        repo::active_users_by_day_hot(&mut c, app_scope(ids.app_id), day(10, 0), day(11, 0))
            .await
            .unwrap();
    let days: Vec<String> = series.iter().map(|r| r.day.to_string()).collect();
    assert_eq!(
        series.len(),
        1,
        "the instant `to` itself is outside [from, to); got {days:?}"
    );
    assert_eq!(series[0].day.to_string(), "2026-05-10");
}

/// Environment scoping, because the metric is reachable by the lowest-privileged
/// role and an env-scoped member must not see another environment's population.
#[tokio::test]
async fn the_series_is_scoped_to_one_environment() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut c = db.conn().await;

    seed_signal_event(&mut c, ids.app_id, Some(ids.env_a), "only-in-a", day(10, 9)).await;
    seed_signal_event(
        &mut c,
        ids.app_id,
        Some(ids.env_b),
        "only-in-b",
        day(10, 10),
    )
    .await;

    let scoped = repo::active_users_by_day_hot(
        &mut c,
        ReadScope {
            app_id: ids.app_id,
            env: EnvFilter::One(ids.env_a),
        },
        day(10, 0),
        day(11, 0),
    )
    .await
    .unwrap();
    assert_eq!(scoped.len(), 1);
    assert_eq!(scoped[0].count, 1, "only the person active in env A");

    let all = repo::active_users_by_day_hot(&mut c, app_scope(ids.app_id), day(10, 0), day(11, 0))
        .await
        .unwrap();
    assert_eq!(
        all[0].count, 2,
        "app-wide sees both, so the scoping is real"
    );
}

/// A range with no activity is an empty series, not a row of zeroes — the chart
/// distinguishes "no data" from "zero users", and inventing zeroes here would
/// make a gap in ingestion look like a real cliff.
#[tokio::test]
async fn a_quiet_range_yields_no_rows() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut c = db.conn().await;
    seed_signal_event(&mut c, ids.app_id, None, "a", day(10, 12)).await;

    let series =
        repo::active_users_by_day_hot(&mut c, app_scope(ids.app_id), day(20, 0), day(25, 0))
            .await
            .unwrap();
    assert!(series.is_empty());
}

/// The day bucket is UTC regardless of the DATABASE's timezone.
///
/// `occurred_at` is `timestamptz`, so a bare `occurred_at::date` buckets by the
/// SESSION timezone — which is UTC on this test container and on most
/// deployments, and therefore agrees with the correct expression right up until
/// someone runs Postgres with `timezone = 'America/New_York'`. Then every event
/// in the first hours of a UTC day silently lands on the previous day, the hot
/// side stops agreeing with the cold side (`DuckEngine::open` pins UTC), and the
/// series grows a seam at the watermark that looks like real data.
///
/// This test forces a non-UTC session timezone so the two expressions diverge and
/// the assertion can tell them apart. Without it the check is untestable — a
/// mutation run showed the UTC pin could be deleted with every other test still
/// green.
#[tokio::test]
async fn day_buckets_are_utc_even_when_the_session_timezone_is_not() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut c = db.conn().await;

    use diesel_async::{RunQueryDsl, SimpleAsyncConnection};
    // UTC-5 (no DST on this date), so 02:00 UTC on day 10 is 21:00 on day 9 local.
    c.batch_execute("SET TIME ZONE 'America/New_York'")
        .await
        .expect("set session timezone");

    seed_signal_event(&mut c, ids.app_id, None, "earlybird", day(10, 2)).await;

    let series =
        repo::active_users_by_day_hot(&mut c, app_scope(ids.app_id), day(10, 0), day(11, 0))
            .await
            .unwrap();
    assert_eq!(series.len(), 1, "the event is inside the UTC day-10 window");
    assert_eq!(
        series[0].day.to_string(),
        "2026-05-10",
        "must bucket on the UTC day, not the session's local day (which is 05-09)"
    );

    // Prove the timezone really is in force, so the assertion above is not
    // passing because the SET silently failed.
    #[derive(diesel::QueryableByName)]
    struct LocalDay {
        #[diesel(sql_type = diesel::sql_types::Text)]
        d: String,
    }
    let local: LocalDay =
        diesel::sql_query("SELECT (TIMESTAMPTZ '2026-05-10 02:00:00+00')::date::text AS d")
            .get_result(&mut c)
            .await
            .unwrap();
    assert_eq!(
        local.d, "2026-05-09",
        "session timezone is not actually applied, so this test cannot discriminate"
    );
}
