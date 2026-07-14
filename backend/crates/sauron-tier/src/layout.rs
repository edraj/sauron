//! Partition boundary math and cold-storage path layout. The cold path uses a
//! FIXED hive pruning key (`app_id`, `year`, `month`) regardless of partition
//! granularity, so changing day/week/month later never breaks read globs.

use chrono::{DateTime, Datelike, Duration, TimeZone, Utc};
use uuid::Uuid;

use crate::plan::TimeRange;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Granularity {
    Day,
    Week,
    Month,
}

impl Granularity {
    /// Parse `"day" | "week" | "month"`, falling back to `default` for anything else.
    pub fn from_str_or(s: &str, default: Granularity) -> Granularity {
        match s.to_ascii_lowercase().as_str() {
            "day" => Granularity::Day,
            "week" => Granularity::Week,
            "month" => Granularity::Month,
            _ => default,
        }
    }
}

fn start_of_day(ts: DateTime<Utc>) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(ts.year(), ts.month(), ts.day(), 0, 0, 0).unwrap()
}

/// The partition `[start, end)` that contains `ts` at the given granularity.
pub fn bucket_bounds(ts: DateTime<Utc>, g: Granularity) -> TimeRange {
    match g {
        Granularity::Day => {
            let start = start_of_day(ts);
            TimeRange { start, end: start + Duration::days(1) }
        }
        Granularity::Week => {
            // ISO week: Monday start.
            let sod = start_of_day(ts);
            let dow = sod.weekday().num_days_from_monday() as i64;
            let start = sod - Duration::days(dow);
            TimeRange { start, end: start + Duration::days(7) }
        }
        Granularity::Month => {
            let start = Utc.with_ymd_and_hms(ts.year(), ts.month(), 1, 0, 0, 0).unwrap();
            let (ny, nm) = if ts.month() == 12 { (ts.year() + 1, 1) } else { (ts.year(), ts.month() + 1) };
            let end = Utc.with_ymd_and_hms(ny, nm, 1, 0, 0, 0).unwrap();
            TimeRange { start, end }
        }
    }
}

/// PG child-partition name suffix, from the partition start. `2026-05-01` → `2026_05_01`.
pub fn partition_suffix(start: DateTime<Utc>) -> String {
    format!("{:04}_{:02}_{:02}", start.year(), start.month(), start.day())
}

/// Directory DuckDB writes the cold Parquet under (hive-partitioned inside).
pub fn cold_copy_dir(base: &str, table: &str) -> String {
    format!("{}/{}", base.trim_end_matches('/'), table)
}

/// Read glob for all of one app's cold Parquet for `table`.
pub fn cold_partition_glob(base: &str, table: &str, app_id: Uuid) -> String {
    format!(
        "{}/{}/app_id={}/year=*/month=*/*.parquet",
        base.trim_end_matches('/'),
        table,
        app_id
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(y: i32, mo: u32, d: u32, h: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, 30, 0).unwrap()
    }

    #[test]
    fn day_bounds() {
        let b = bucket_bounds(t(2026, 5, 15, 13), Granularity::Day);
        assert_eq!(b.start, Utc.with_ymd_and_hms(2026, 5, 15, 0, 0, 0).unwrap());
        assert_eq!(b.end, Utc.with_ymd_and_hms(2026, 5, 16, 0, 0, 0).unwrap());
    }

    #[test]
    fn week_starts_monday() {
        // 2026-05-15 is a Friday → week starts Mon 2026-05-11.
        let b = bucket_bounds(t(2026, 5, 15, 13), Granularity::Week);
        assert_eq!(b.start, Utc.with_ymd_and_hms(2026, 5, 11, 0, 0, 0).unwrap());
        assert_eq!(b.end, Utc.with_ymd_and_hms(2026, 5, 18, 0, 0, 0).unwrap());
    }

    #[test]
    fn month_bounds_roll_over_year() {
        let b = bucket_bounds(t(2026, 12, 20, 9), Granularity::Month);
        assert_eq!(b.start, Utc.with_ymd_and_hms(2026, 12, 1, 0, 0, 0).unwrap());
        assert_eq!(b.end, Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).unwrap());
    }

    #[test]
    fn suffix_and_paths() {
        let start = Utc.with_ymd_and_hms(2026, 5, 1, 0, 0, 0).unwrap();
        assert_eq!(partition_suffix(start), "2026_05_01");
        assert_eq!(cold_copy_dir("/cold/", "error_events"), "/cold/error_events");
        let app = Uuid::nil();
        assert_eq!(
            cold_partition_glob("/cold", "error_events", app),
            format!("/cold/error_events/app_id={}/year=*/month=*/*.parquet", app)
        );
    }

    #[test]
    fn granularity_parse() {
        assert_eq!(Granularity::from_str_or("WEEK", Granularity::Day), Granularity::Week);
        assert_eq!(Granularity::from_str_or("nonsense", Granularity::Day), Granularity::Day);
    }
}
