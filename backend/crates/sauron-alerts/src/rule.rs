//! Trigger types and the (pure) evaluation of an admin-defined rule's
//! `conditions` bag. Keeping evaluation pure and I/O-free makes the whole
//! decision surface unit-testable; the evaluator binary supplies the measured
//! metric value and this module decides whether to fire.

use serde_json::Value;

/// What causes a rule to fire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerType {
    /// A monitor transitioned to `down` (event-driven, from the prober).
    MonitorDown,
    /// A monitor recovered to `up` (event-driven, from the prober).
    MonitorUp,
    /// A brand-new issue (error group) was first seen (evaluator).
    IssueNew,
    /// A resolved/ignored issue started erroring again (evaluator).
    IssueRegression,
    /// Error-event count in a window crossed a threshold (evaluator).
    ErrorThreshold,
    /// Error-event count spiked vs the previous window (evaluator).
    ErrorSpike,
    /// Analytics-event count in a window crossed a threshold (evaluator).
    EventThreshold,
    /// A latency percentile in a window crossed a threshold (evaluator).
    PerfDegradation,
}

impl TriggerType {
    pub fn parse(s: &str) -> Option<TriggerType> {
        Some(match s {
            "monitor_down" => TriggerType::MonitorDown,
            "monitor_up" => TriggerType::MonitorUp,
            "issue_new" => TriggerType::IssueNew,
            "issue_regression" => TriggerType::IssueRegression,
            "error_threshold" => TriggerType::ErrorThreshold,
            "error_spike" => TriggerType::ErrorSpike,
            "event_threshold" => TriggerType::EventThreshold,
            "perf_degradation" => TriggerType::PerfDegradation,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            TriggerType::MonitorDown => "monitor_down",
            TriggerType::MonitorUp => "monitor_up",
            TriggerType::IssueNew => "issue_new",
            TriggerType::IssueRegression => "issue_regression",
            TriggerType::ErrorThreshold => "error_threshold",
            TriggerType::ErrorSpike => "error_spike",
            TriggerType::EventThreshold => "event_threshold",
            TriggerType::PerfDegradation => "perf_degradation",
        }
    }

    /// Event-driven triggers are dispatched inline by the prober; metric-driven
    /// triggers are polled by the evaluator loop.
    pub fn is_metric(self) -> bool {
        !matches!(self, TriggerType::MonitorDown | TriggerType::MonitorUp)
    }

    pub const ALL: [TriggerType; 8] = [
        TriggerType::MonitorDown,
        TriggerType::MonitorUp,
        TriggerType::IssueNew,
        TriggerType::IssueRegression,
        TriggerType::ErrorThreshold,
        TriggerType::ErrorSpike,
        TriggerType::EventThreshold,
        TriggerType::PerfDegradation,
    ];
}

/// How a measured value is compared to the rule's threshold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Comparator {
    Gte,
    Gt,
    Lte,
    Lt,
    Eq,
}

impl Comparator {
    pub fn parse(s: &str) -> Option<Comparator> {
        Some(match s {
            "gte" | ">=" => Comparator::Gte,
            "gt" | ">" => Comparator::Gt,
            "lte" | "<=" => Comparator::Lte,
            "lt" | "<" => Comparator::Lt,
            "eq" | "==" => Comparator::Eq,
            _ => return None,
        })
    }

    pub fn compare(self, value: f64, threshold: f64) -> bool {
        match self {
            Comparator::Gte => value >= threshold,
            Comparator::Gt => value > threshold,
            Comparator::Lte => value <= threshold,
            Comparator::Lt => value < threshold,
            Comparator::Eq => (value - threshold).abs() < f64::EPSILON,
        }
    }
}

/// Optional narrowing filters applied to the metric query.
#[derive(Debug, Clone, Default)]
pub struct Filters {
    pub level: Option<String>,
    pub environment: Option<String>,
    pub event_name: Option<String>,
    pub tag_key: Option<String>,
    pub tag_value: Option<String>,
    pub op: Option<String>,
}

impl Filters {
    pub fn from_value(conditions: &Value) -> Filters {
        let f = conditions.get("filters").unwrap_or(&Value::Null);
        let get = |k: &str| {
            f.get(k)
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .filter(|s| !s.is_empty())
        };
        Filters {
            level: get("level"),
            environment: get("environment"),
            event_name: get("event_name"),
            tag_key: get("tag_key"),
            tag_value: get("tag_value"),
            op: get("op"),
        }
    }
}

/// The parsed, validated condition bag with per-trigger defaults applied.
#[derive(Debug, Clone)]
pub struct Conditions {
    pub comparator: Comparator,
    pub threshold: f64,
    pub window_seconds: i64,
    pub spike_factor: f64,
    /// Latency percentile/metric for perf triggers: p50/p75/p90/p95/p99/avg/max.
    pub metric: String,
    pub filters: Filters,
}

/// The largest window we will ever aggregate over, to bound evaluator query cost.
pub const MAX_WINDOW_SECONDS: i64 = 24 * 3600;
const MIN_WINDOW_SECONDS: i64 = 60;

impl Conditions {
    /// Parse from the stored JSONB, clamping to safe ranges and applying
    /// per-trigger defaults. Never fails: unknown values fall back to defaults.
    pub fn from_value(trigger: TriggerType, v: &Value) -> Conditions {
        let comparator = v
            .get("comparator")
            .and_then(|x| x.as_str())
            .and_then(Comparator::parse)
            .unwrap_or(Comparator::Gte);
        let threshold = v.get("threshold").and_then(num).unwrap_or(match trigger {
            TriggerType::PerfDegradation => 1000.0,
            _ => 1.0,
        });
        let default_window = match trigger {
            TriggerType::PerfDegradation => 900,
            TriggerType::ErrorSpike => 300,
            _ => 300,
        };
        let window_seconds = v
            .get("window_seconds")
            .and_then(|x| x.as_i64())
            .unwrap_or(default_window)
            .clamp(MIN_WINDOW_SECONDS, MAX_WINDOW_SECONDS);
        let spike_factor = v.get("spike_factor").and_then(num).unwrap_or(3.0).max(1.0);
        let metric = v
            .get("metric")
            .and_then(|x| x.as_str())
            .filter(|m| matches!(*m, "p50" | "p75" | "p90" | "p95" | "p99" | "avg" | "max"))
            .unwrap_or("p95")
            .to_string();
        Conditions {
            comparator,
            threshold,
            window_seconds,
            spike_factor,
            metric,
            filters: Filters::from_value(v),
        }
    }

    /// Decide whether a measured metric value fires this rule.
    pub fn fires(&self, value: f64) -> bool {
        self.comparator.compare(value, self.threshold)
    }
}

fn num(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_i64().map(|i| i as f64))
}

/// Validate a rule's `conditions` on write. Returns a human-readable error.
pub fn validate_conditions(trigger: TriggerType, v: &Value) -> Result<(), String> {
    if let Some(c) = v.get("comparator").and_then(|x| x.as_str()) {
        if Comparator::parse(c).is_none() {
            return Err(format!("unknown comparator: {c}"));
        }
    }
    if let Some(t) = v.get("threshold") {
        if num(t).is_none() {
            return Err("threshold must be a number".into());
        }
    }
    // Metric-driven rules need a positive threshold to be meaningful.
    if trigger.is_metric() {
        let c = Conditions::from_value(trigger, v);
        if c.threshold < 0.0 {
            return Err("threshold must be non-negative".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn comparator_semantics() {
        assert!(Comparator::Gte.compare(5.0, 5.0));
        assert!(!Comparator::Gt.compare(5.0, 5.0));
        assert!(Comparator::Lt.compare(3.0, 5.0));
        assert!(Comparator::Eq.compare(5.0, 5.0));
    }

    #[test]
    fn defaults_and_clamps() {
        let c = Conditions::from_value(TriggerType::ErrorThreshold, &json!({}));
        assert_eq!(c.comparator, Comparator::Gte);
        assert_eq!(c.threshold, 1.0);
        assert_eq!(c.window_seconds, 300);

        // Window is clamped to the max.
        let big = Conditions::from_value(
            TriggerType::ErrorThreshold,
            &json!({ "window_seconds": 999999999 }),
        );
        assert_eq!(big.window_seconds, MAX_WINDOW_SECONDS);

        // And to the min.
        let small =
            Conditions::from_value(TriggerType::ErrorThreshold, &json!({ "window_seconds": 1 }));
        assert_eq!(small.window_seconds, 60);
    }

    #[test]
    fn perf_defaults_to_p95_1000ms() {
        let c = Conditions::from_value(TriggerType::PerfDegradation, &json!({}));
        assert_eq!(c.metric, "p95");
        assert_eq!(c.threshold, 1000.0);
        assert_eq!(c.window_seconds, 900);
    }

    #[test]
    fn filters_parse() {
        let c = Conditions::from_value(
            TriggerType::ErrorThreshold,
            &json!({ "filters": { "level": "error", "environment": "prod", "tag_key": "region", "tag_value": "eu" } }),
        );
        assert_eq!(c.filters.level.as_deref(), Some("error"));
        assert_eq!(c.filters.environment.as_deref(), Some("prod"));
        assert_eq!(c.filters.tag_key.as_deref(), Some("region"));
    }

    #[test]
    fn fires_respects_comparator() {
        let c = Conditions::from_value(
            TriggerType::ErrorThreshold,
            &json!({ "comparator": "gte", "threshold": 10 }),
        );
        assert!(c.fires(10.0));
        assert!(c.fires(11.0));
        assert!(!c.fires(9.0));
    }

    #[test]
    fn validate_rejects_bad_comparator() {
        assert!(validate_conditions(
            TriggerType::ErrorThreshold,
            &json!({ "comparator": "bogus" })
        )
        .is_err());
        assert!(
            validate_conditions(TriggerType::ErrorThreshold, &json!({ "threshold": "x" })).is_err()
        );
        assert!(
            validate_conditions(TriggerType::ErrorThreshold, &json!({ "threshold": 5 })).is_ok()
        );
    }
}
