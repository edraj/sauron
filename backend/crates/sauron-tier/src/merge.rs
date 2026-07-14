//! Merge additive per-day partials from the hot and cold tiers into one series.
//! Days are usually disjoint across the watermark, but a watermark mid-day can
//! put the same day in both tiers, so we sum by day rather than concatenate.

use std::collections::BTreeMap;

use chrono::NaiveDate;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayCount {
    pub day: NaiveDate,
    pub count: i64,
}

/// Sum `hot` and `cold` per-day counts into one ascending-by-day series.
pub fn merge_day_counts(hot: Vec<DayCount>, cold: Vec<DayCount>) -> Vec<DayCount> {
    let mut acc: BTreeMap<NaiveDate, i64> = BTreeMap::new();
    for dc in hot.into_iter().chain(cold.into_iter()) {
        *acc.entry(dc.day).or_insert(0) += dc.count;
    }
    acc.into_iter().map(|(day, count)| DayCount { day, count }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn disjoint_days_concatenate_sorted() {
        let hot = vec![DayCount { day: d(2026, 6, 2), count: 5 }, DayCount { day: d(2026, 6, 1), count: 3 }];
        let cold = vec![DayCount { day: d(2026, 5, 30), count: 9 }];
        let out = merge_day_counts(hot, cold);
        assert_eq!(
            out,
            vec![
                DayCount { day: d(2026, 5, 30), count: 9 },
                DayCount { day: d(2026, 6, 1), count: 3 },
                DayCount { day: d(2026, 6, 2), count: 5 },
            ]
        );
    }

    #[test]
    fn same_day_in_both_tiers_is_summed() {
        let hot = vec![DayCount { day: d(2026, 6, 1), count: 4 }];
        let cold = vec![DayCount { day: d(2026, 6, 1), count: 6 }];
        assert_eq!(merge_day_counts(hot, cold), vec![DayCount { day: d(2026, 6, 1), count: 10 }]);
    }

    #[test]
    fn empty_sides() {
        assert_eq!(merge_day_counts(vec![], vec![]), vec![]);
        let only = vec![DayCount { day: d(2026, 6, 1), count: 1 }];
        assert_eq!(merge_day_counts(only.clone(), vec![]), only);
    }
}
