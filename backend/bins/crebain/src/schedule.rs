//! Pure open-model rate math: how many items are "due" by a given elapsed time.

/// Items due since start for a constant rate, minus those already emitted.
pub fn items_due(rate_per_sec: f64, elapsed_secs: f64, already_emitted: u64) -> u64 {
    if rate_per_sec <= 0.0 || elapsed_secs <= 0.0 {
        return 0;
    }
    let target = (rate_per_sec * elapsed_secs).floor() as u64;
    target.saturating_sub(already_emitted)
}

/// Identifies due under a linear ramp: `total × (elapsed/ramp)` clamped to `total`.
pub fn ramp_identifies_due(total: u64, ramp_secs: f64, elapsed_secs: f64, already: u64) -> u64 {
    let target = if ramp_secs <= 0.0 {
        total
    } else {
        ((total as f64) * (elapsed_secs / ramp_secs))
            .floor()
            .min(total as f64) as u64
    };
    target.saturating_sub(already)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn items_due_tracks_cumulative_rate() {
        assert_eq!(items_due(100.0, 0.05, 0), 5); // 100/s × 50ms = 5
        assert_eq!(items_due(100.0, 0.05, 5), 0); // already caught up
        assert_eq!(items_due(100.0, 1.0, 5), 95); // 100 due − 5 sent
    }
    #[test]
    fn ramp_spreads_identifies_linearly_then_completes() {
        assert_eq!(ramp_identifies_due(1000, 5.0, 0.0, 0), 0);
        assert_eq!(ramp_identifies_due(1000, 5.0, 2.5, 0), 500); // halfway
        assert_eq!(ramp_identifies_due(1000, 5.0, 10.0, 400), 600); // clamps to total
        assert_eq!(ramp_identifies_due(1000, 0.0, 0.0, 0), 1000); // zero ramp = all now
    }
}
