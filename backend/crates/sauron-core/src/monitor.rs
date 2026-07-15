//! Uptime-monitor domain constants shared across services.
//!
//! This is the single source of truth for the intervals a monitor may run at:
//! the API rejects any `interval_seconds` outside this set, and the dashboard
//! offers exactly these as options. Keep it in sync with
//! `dashboard/src/lib/constants/monitorIntervals.ts`.

/// Allowed monitor check intervals, in seconds:
/// 1s, 5s, 15s, 30s, 1m, 3m, 5m, 15m, 30m, 1h, 3h, 6h, 12h, 24h.
pub const MONITOR_INTERVAL_PRESETS: [i32; 14] = [
    1, 5, 15, 30, 60, 180, 300, 900, 1800, 3600, 10800, 21600, 43200, 86400,
];

/// Whether `secs` is one of the allowed monitor intervals.
pub fn is_valid_monitor_interval(secs: i32) -> bool {
    MONITOR_INTERVAL_PRESETS.contains(&secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_every_preset() {
        for s in MONITOR_INTERVAL_PRESETS {
            assert!(is_valid_monitor_interval(s), "{s} should be valid");
        }
    }

    #[test]
    fn rejects_off_list_values() {
        for s in [i32::MIN, -1, 0, 2, 45, 61, 100, 7200, 100_000, i32::MAX] {
            assert!(!is_valid_monitor_interval(s), "{s} should be invalid");
        }
    }
}
