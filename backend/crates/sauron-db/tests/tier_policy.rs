//! `runtime_settings` and `tier_pins`: the two pieces that make the cold-tier
//! rotation age operator-tunable and make a cold-data restore survive.
//!
//! Both are small tables, but each carries one rule that is easy to get subtly
//! wrong and impossible to notice until data goes missing:
//!
//!  1. `effective_tier_hot_days` must fall back to the configured value for
//!     EVERY invalid stored value, including ones that parse. A stored `0` parses
//!     fine and would drive the tier cutoff to `now`, making the current day's
//!     partitions immediately eligible while they are still being written to.
//!     The floor is what stops a typo in psql from tiering live data.
//!
//!  2. `is_range_pinned` must match on OVERLAP, not containment. The tier worker
//!     asks the question per whole partition; a pin covering part of one still has
//!     to block the drop, because dropping the partition takes the pinned rows
//!     with it. A containment test would silently discard exactly the rows a
//!     restore was performed to obtain.
//!
//! Skips (does not fail) when `TEST_DATABASE_URL` is unset — see `common/mod.rs`.

mod common;

use chrono::{Duration, Utc};
use sauron_db::repo;

use common::TestDb;

#[tokio::test]
async fn absent_setting_uses_the_configured_value() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut c = db.conn().await;

    // Nothing seeds this table, so absence is the normal state on a fresh install
    // and must read as "use the process's configured value", not as an error.
    assert_eq!(
        repo::get_runtime_setting(&mut c, repo::TIER_HOT_DAYS_KEY)
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        repo::effective_tier_hot_days(&mut c, 30).await.unwrap(),
        30,
        "no override must resolve to the configured value"
    );

    db.cleanup().await;
}

#[tokio::test]
async fn a_valid_override_wins_over_the_configured_value() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut c = db.conn().await;

    repo::set_runtime_setting(&mut c, repo::TIER_HOT_DAYS_KEY, "7", None)
        .await
        .unwrap();
    assert_eq!(repo::effective_tier_hot_days(&mut c, 30).await.unwrap(), 7);

    // Raising it works the same way — the override is authoritative in both
    // directions, not merely a floor or a ceiling on the configured value.
    repo::set_runtime_setting(&mut c, repo::TIER_HOT_DAYS_KEY, "365", None)
        .await
        .unwrap();
    assert_eq!(
        repo::effective_tier_hot_days(&mut c, 30).await.unwrap(),
        365
    );

    db.cleanup().await;
}

#[tokio::test]
async fn clearing_the_override_reverts_to_configured() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut c = db.conn().await;

    repo::set_runtime_setting(&mut c, repo::TIER_HOT_DAYS_KEY, "7", None)
        .await
        .unwrap();
    assert_eq!(repo::effective_tier_hot_days(&mut c, 30).await.unwrap(), 7);

    let deleted = repo::delete_runtime_setting(&mut c, repo::TIER_HOT_DAYS_KEY)
        .await
        .unwrap();
    assert_eq!(deleted, 1);
    assert_eq!(repo::effective_tier_hot_days(&mut c, 30).await.unwrap(), 30);

    // Deleting again is a no-op, not an error: the UI's "revert to default"
    // action must be idempotent.
    assert_eq!(
        repo::delete_runtime_setting(&mut c, repo::TIER_HOT_DAYS_KEY)
            .await
            .unwrap(),
        0
    );

    db.cleanup().await;
}

#[tokio::test]
async fn invalid_stored_values_fall_back_instead_of_breaking_tiering() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut c = db.conn().await;

    // `0` and negatives are the dangerous ones: they PARSE, so a naive
    // `parse().unwrap_or(configured)` would accept them and put the tier cutoff
    // at or after `now`, making live partitions immediately eligible for export
    // and drop. The floor is the guard, not the parse.
    for bad in ["0", "-1", "-9999"] {
        repo::set_runtime_setting(&mut c, repo::TIER_HOT_DAYS_KEY, bad, None)
            .await
            .unwrap();
        assert_eq!(
            repo::effective_tier_hot_days(&mut c, 30).await.unwrap(),
            30,
            "stored {bad:?} must fall back to the configured value, not be honoured"
        );
    }

    // Unparseable values fall back too — a hand-edited row must never be able to
    // stop tiering deployment-wide.
    for bad in ["", "   ", "abc", "7.5", "7 days", "1e3", "٣٠"] {
        repo::set_runtime_setting(&mut c, repo::TIER_HOT_DAYS_KEY, bad, None)
            .await
            .unwrap();
        assert_eq!(
            repo::effective_tier_hot_days(&mut c, 30).await.unwrap(),
            30,
            "stored {bad:?} must fall back to the configured value"
        );
    }

    db.cleanup().await;
}

#[tokio::test]
async fn surrounding_whitespace_is_tolerated() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut c = db.conn().await;

    // A value pasted into psql with a trailing newline is still the operator's
    // clear intent; rejecting it would silently revert to the configured value.
    repo::set_runtime_setting(&mut c, repo::TIER_HOT_DAYS_KEY, "  14\n", None)
        .await
        .unwrap();
    assert_eq!(repo::effective_tier_hot_days(&mut c, 30).await.unwrap(), 14);

    db.cleanup().await;
}

#[tokio::test]
async fn the_minimum_itself_is_accepted() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut c = db.conn().await;

    // Boundary: the floor is inclusive. An off-by-one here would reject the
    // smallest legitimate setting and silently use the configured value instead.
    repo::set_runtime_setting(
        &mut c,
        repo::TIER_HOT_DAYS_KEY,
        &repo::TIER_HOT_DAYS_MIN.to_string(),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        repo::effective_tier_hot_days(&mut c, 30).await.unwrap(),
        repo::TIER_HOT_DAYS_MIN
    );

    db.cleanup().await;
}

#[tokio::test]
async fn setting_is_an_upsert_not_an_insert() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut c = db.conn().await;

    repo::set_runtime_setting(&mut c, repo::TIER_HOT_DAYS_KEY, "7", None)
        .await
        .unwrap();
    // A second write must update rather than violate the primary key.
    repo::set_runtime_setting(&mut c, repo::TIER_HOT_DAYS_KEY, "9", None)
        .await
        .unwrap();
    assert_eq!(repo::effective_tier_hot_days(&mut c, 30).await.unwrap(), 9);

    db.cleanup().await;
}

#[tokio::test]
async fn keys_do_not_bleed_into_each_other() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut c = db.conn().await;

    repo::set_runtime_setting(&mut c, "some.other.key", "999", None)
        .await
        .unwrap();
    assert_eq!(
        repo::effective_tier_hot_days(&mut c, 30).await.unwrap(),
        30,
        "an unrelated key must not be read as the rotation age"
    );

    db.cleanup().await;
}

// ===========================================================================
// Pins
// ===========================================================================

#[tokio::test]
async fn no_pins_means_not_pinned() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut c = db.conn().await;

    let now = Utc::now();
    assert!(!repo::is_range_pinned(
        &mut c,
        "error_events",
        now - Duration::days(2),
        now - Duration::days(1)
    )
    .await
    .unwrap());

    db.cleanup().await;
}

#[tokio::test]
async fn a_pin_blocks_its_own_range_and_only_its_own_table() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut c = db.conn().await;

    let now = Utc::now();
    let (start, end) = (now - Duration::days(10), now - Duration::days(9));
    repo::create_tier_pin(
        &mut c,
        "error_events",
        start,
        end,
        now + Duration::days(7),
        None,
        Some("restored for incident review"),
    )
    .await
    .unwrap();

    assert!(repo::is_range_pinned(&mut c, "error_events", start, end)
        .await
        .unwrap());
    // A pin on one tiered table must not protect the same window in another —
    // they are exported and dropped independently.
    assert!(
        !repo::is_range_pinned(&mut c, "analytics_events", start, end)
            .await
            .unwrap(),
        "a pin must be scoped to its table"
    );

    db.cleanup().await;
}

#[tokio::test]
async fn pin_matching_is_overlap_not_containment() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut c = db.conn().await;

    let now = Utc::now();
    // Pin the middle of a day; the worker will ask about the whole day.
    let pin_start = now - Duration::days(5) + Duration::hours(6);
    let pin_end = now - Duration::days(5) + Duration::hours(18);
    repo::create_tier_pin(
        &mut c,
        "error_events",
        pin_start,
        pin_end,
        now + Duration::days(1),
        None,
        None,
    )
    .await
    .unwrap();

    let day_start = now - Duration::days(5);
    let day_end = now - Duration::days(4);
    assert!(
        repo::is_range_pinned(&mut c, "error_events", day_start, day_end)
            .await
            .unwrap(),
        "a pin inside the partition must block the whole partition — dropping it \
         would take the pinned rows too"
    );

    // Partially overlapping on each side also blocks.
    assert!(repo::is_range_pinned(
        &mut c,
        "error_events",
        pin_start - Duration::hours(2),
        pin_start + Duration::hours(1)
    )
    .await
    .unwrap());
    assert!(repo::is_range_pinned(
        &mut c,
        "error_events",
        pin_end - Duration::hours(1),
        pin_end + Duration::hours(2)
    )
    .await
    .unwrap());

    db.cleanup().await;
}

#[tokio::test]
async fn abutting_ranges_do_not_overlap() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut c = db.conn().await;

    let now = Utc::now();
    let (start, end) = (now - Duration::days(3), now - Duration::days(2));
    repo::create_tier_pin(
        &mut c,
        "error_events",
        start,
        end,
        now + Duration::days(1),
        None,
        None,
    )
    .await
    .unwrap();

    // Ranges are half-open [start, end). The partition immediately after the pin
    // begins exactly where the pin ends and shares no instant with it, so it must
    // remain droppable — otherwise one pin would freeze its neighbours too.
    assert!(
        !repo::is_range_pinned(&mut c, "error_events", end, end + Duration::days(1))
            .await
            .unwrap(),
        "the range starting where the pin ends must not be pinned"
    );
    assert!(
        !repo::is_range_pinned(&mut c, "error_events", start - Duration::days(1), start)
            .await
            .unwrap(),
        "the range ending where the pin starts must not be pinned"
    );

    db.cleanup().await;
}

#[tokio::test]
async fn an_expired_pin_stops_protecting() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut c = db.conn().await;

    let now = Utc::now();
    let (start, end) = (now - Duration::days(10), now - Duration::days(9));
    repo::create_tier_pin(
        &mut c,
        "error_events",
        start,
        end,
        // Already expired. This is the whole point of a mandatory expiry: a pin
        // nobody renews must stop holding disk, or one incident investigation
        // freezes a range forever.
        now - Duration::minutes(1),
        None,
        None,
    )
    .await
    .unwrap();

    assert!(
        !repo::is_range_pinned(&mut c, "error_events", start, end)
            .await
            .unwrap(),
        "an expired pin must not block the drop"
    );
    // It is still listed, so the UI can show a lapsed restore rather than making
    // it vanish silently.
    assert_eq!(repo::list_tier_pins(&mut c).await.unwrap().len(), 1);

    db.cleanup().await;
}

#[tokio::test]
async fn the_longest_lived_overlapping_pin_wins() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut c = db.conn().await;

    let now = Utc::now();
    let (start, end) = (now - Duration::days(10), now - Duration::days(9));
    // Two restores of the same range, one already lapsed. Pins are not merged;
    // the check asks whether ANY is live, so the live one must still protect.
    repo::create_tier_pin(
        &mut c,
        "error_events",
        start,
        end,
        now - Duration::minutes(1),
        None,
        Some("old"),
    )
    .await
    .unwrap();
    repo::create_tier_pin(
        &mut c,
        "error_events",
        start,
        end,
        now + Duration::days(3),
        None,
        Some("current"),
    )
    .await
    .unwrap();

    assert!(repo::is_range_pinned(&mut c, "error_events", start, end)
        .await
        .unwrap());

    db.cleanup().await;
}

#[tokio::test]
async fn deleting_a_pin_unprotects_immediately() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut c = db.conn().await;

    let now = Utc::now();
    let (start, end) = (now - Duration::days(10), now - Duration::days(9));
    let pin = repo::create_tier_pin(
        &mut c,
        "error_events",
        start,
        end,
        now + Duration::days(7),
        None,
        None,
    )
    .await
    .unwrap();
    assert!(repo::is_range_pinned(&mut c, "error_events", start, end)
        .await
        .unwrap());

    assert_eq!(repo::delete_tier_pin(&mut c, pin.id).await.unwrap(), 1);
    assert!(!repo::is_range_pinned(&mut c, "error_events", start, end)
        .await
        .unwrap());

    db.cleanup().await;
}

#[tokio::test]
async fn purge_removes_only_expired_pins() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut c = db.conn().await;

    let now = Utc::now();
    let (start, end) = (now - Duration::days(10), now - Duration::days(9));
    repo::create_tier_pin(
        &mut c,
        "error_events",
        start,
        end,
        now - Duration::minutes(1),
        None,
        Some("expired"),
    )
    .await
    .unwrap();
    repo::create_tier_pin(
        &mut c,
        "error_events",
        start,
        end,
        now + Duration::days(1),
        None,
        Some("live"),
    )
    .await
    .unwrap();

    assert_eq!(repo::purge_expired_tier_pins(&mut c).await.unwrap(), 1);
    let left = repo::list_tier_pins(&mut c).await.unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].reason.as_deref(), Some("live"));

    db.cleanup().await;
}

#[tokio::test]
async fn a_pin_with_end_before_start_is_rejected_by_the_database() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut c = db.conn().await;

    let now = Utc::now();
    // The CHECK constraint is the last line of defence against an inverted range,
    // which would match nothing and silently protect nothing.
    let res = repo::create_tier_pin(
        &mut c,
        "error_events",
        now,
        now - Duration::days(1),
        now + Duration::days(1),
        None,
        None,
    )
    .await;
    assert!(res.is_err(), "an inverted range must be rejected");

    db.cleanup().await;
}
