//! The tag-key sampler's SQL shape.
//!
//! Pinned without a database, following the `query_plan` precedent: the two
//! properties that matter here are structural, and both are invisible to a
//! smoke test that merely returns rows.
//!
//! 1. The scan is BOUNDED. An unbounded `jsonb_each_text` over a partitioned
//!    parent is a seq scan across every partition — the exact shape recorded
//!    where a time-unbounded correlated subquery measured 190x slower than its
//!    bounded twin, with a cost that scales with retained data rather than with
//!    the question asked.
//! 2. Every caller-supplied value is a bind parameter, never interpolated.

use sauron_db::repo::{tag_keys_sql, TagSource};

#[test]
fn the_sample_is_bounded_by_both_a_window_and_a_row_limit() {
    let sql = tag_keys_sql(TagSource::ErrorEvents);
    assert!(
        sql.contains("LIMIT"),
        "the inner sample must be row-bounded: {sql}"
    );
    assert!(
        sql.contains("occurred_at >"),
        "the inner sample must be time-bounded: {sql}"
    );
    assert!(
        sql.contains("ORDER BY occurred_at DESC"),
        "the sample must be the MOST RECENT rows, not an arbitrary page: {sql}"
    );
}

#[test]
fn the_lateral_runs_over_the_sample_not_the_table() {
    let sql = tag_keys_sql(TagSource::ErrorEvents);
    let lateral = sql.find("LATERAL").expect("uses a LATERAL");
    let limit = sql.find("LIMIT").expect("has a LIMIT");
    assert!(
        limit < lateral,
        "the LIMIT must bound the subquery the LATERAL reads, not follow it: {sql}"
    );
}

#[test]
fn every_user_value_is_a_bind_parameter() {
    for source in [TagSource::ErrorEvents, TagSource::AnalyticsEvents] {
        let sql = tag_keys_sql(source);
        assert!(sql.contains("$1"), "app_id must be bound: {sql}");
        assert!(sql.contains("$2"), "the window must be bound: {sql}");
        assert!(sql.contains("$3"), "the row limit must be bound: {sql}");
        // Nothing that looks like an inlined literal uuid or timestamp.
        assert!(
            !sql.contains('\''),
            "no literal may appear in the SQL: {sql}"
        );
    }
}

#[test]
fn the_two_sources_address_their_own_table() {
    assert!(tag_keys_sql(TagSource::ErrorEvents).contains("error_events"));
    assert!(tag_keys_sql(TagSource::AnalyticsEvents).contains("analytics_events"));
    // …and not each other's: `analytics_events` contains no `error_events`
    // substring, but the converse assertion is what catches a copy-paste.
    assert!(!tag_keys_sql(TagSource::ErrorEvents).contains("analytics_events"));
}

/// The sample must skip rows with no tags at all, or the row budget is spent
/// on rows that can contribute no keys — on an app that tags a minority of its
/// events, that is most of the budget.
#[test]
fn rows_without_tags_do_not_consume_the_row_budget() {
    let sql = tag_keys_sql(TagSource::ErrorEvents);
    assert!(
        sql.contains("tags IS NOT NULL"),
        "untagged rows must be excluded before the LIMIT: {sql}"
    );
    let null_check = sql.find("tags IS NOT NULL").unwrap();
    let limit = sql.find("LIMIT").unwrap();
    assert!(
        null_check < limit,
        "the exclusion must be inside the bounded subquery: {sql}"
    );
}
