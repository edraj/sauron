//! Watermark split: given a query window and the tier watermark, decide which
//! sub-range is served hot (Postgres, `occurred_at >= watermark`) and which is
//! served cold (Parquet, `occurred_at < watermark`). Half-open ranges.

use chrono::{DateTime, Utc};

/// Half-open time range `[start, end)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeRange {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

/// Which tiers a query window touches, with the exact sub-range for each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TierPlan {
    pub hot: Option<TimeRange>,
    pub cold: Option<TimeRange>,
}

/// Split the half-open window `[from, to)` at `watermark`.
/// Everything `< watermark` is cold; everything `>= watermark` is hot. The two
/// sub-ranges are complementary → no overlap, no gap (exactly-once).
pub fn plan(watermark: DateTime<Utc>, from: DateTime<Utc>, to: DateTime<Utc>) -> TierPlan {
    if to <= from {
        return TierPlan { hot: None, cold: None };
    }
    let cold = if from < watermark {
        Some(TimeRange { start: from, end: to.min(watermark) })
    } else {
        None
    };
    let hot = if to > watermark {
        Some(TimeRange { start: from.max(watermark), end: to })
    } else {
        None
    };
    TierPlan { hot, cold }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t(y: i32, mo: u32, d: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, 0, 0, 0).unwrap()
    }

    #[test]
    fn fully_hot_when_window_after_watermark() {
        let p = plan(t(2026, 6, 1), t(2026, 6, 10), t(2026, 6, 20));
        assert_eq!(p.hot, Some(TimeRange { start: t(2026, 6, 10), end: t(2026, 6, 20) }));
        assert_eq!(p.cold, None);
    }

    #[test]
    fn fully_cold_when_window_before_watermark() {
        let p = plan(t(2026, 6, 1), t(2026, 5, 1), t(2026, 5, 20));
        assert_eq!(p.cold, Some(TimeRange { start: t(2026, 5, 1), end: t(2026, 5, 20) }));
        assert_eq!(p.hot, None);
    }

    #[test]
    fn straddle_splits_at_watermark_with_no_overlap() {
        let p = plan(t(2026, 6, 1), t(2026, 5, 15), t(2026, 6, 15));
        assert_eq!(p.cold, Some(TimeRange { start: t(2026, 5, 15), end: t(2026, 6, 1) }));
        assert_eq!(p.hot, Some(TimeRange { start: t(2026, 6, 1), end: t(2026, 6, 15) }));
    }

    #[test]
    fn boundary_exactly_at_watermark_is_hot_side_empty() {
        // window [from, watermark): entirely cold, hot omitted (to == watermark).
        let p = plan(t(2026, 6, 1), t(2026, 5, 1), t(2026, 6, 1));
        assert_eq!(p.cold, Some(TimeRange { start: t(2026, 5, 1), end: t(2026, 6, 1) }));
        assert_eq!(p.hot, None);
    }

    #[test]
    fn empty_or_inverted_window_yields_nothing() {
        assert_eq!(plan(t(2026, 6, 1), t(2026, 6, 5), t(2026, 6, 5)), TierPlan { hot: None, cold: None });
        assert_eq!(plan(t(2026, 6, 1), t(2026, 6, 9), t(2026, 6, 5)), TierPlan { hot: None, cold: None });
    }
}
