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
    /// A search-language string, narrowing the metric to the events it
    /// matches — `extra.title=noInternetConnectionTitle`, `os.name:Android`,
    /// anything the Occurrences vocabulary accepts.
    ///
    /// Stored as the TEXT the admin typed rather than a serialized AST: it is
    /// what the rule dialog shows back, what a shared link carries, and the
    /// only form that survives a grammar gaining new spellings. It is parsed
    /// on write (see [`validate_conditions`]) so a rule that cannot resolve is
    /// refused rather than silently counting zero forever, and parsed again
    /// per evaluation, which costs nothing measurable against one aggregate
    /// query.
    pub query: Option<String>,
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
            query: get("query"),
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
        if let Some(q) = c.filters.query.as_deref() {
            validate_query(q)?;
        }
    }
    Ok(())
}

/// Parse and resolve a `filters.query` string, so an unusable one is a 400 on
/// the rule rather than a rule that counts zero.
///
/// Resolved against [`sauron_query::Resource::Occurrences`] because that is the
/// resource whose lowering runs over `error_events` — the table every
/// metric-driven error trigger counts. A rule on a different metric that names
/// an occurrence-only field is still refused here, which is the honest answer:
/// the predicate could not have narrowed that metric anyway.
pub fn validate_query(q: &str) -> Result<(), String> {
    parse_query(q).map(|_| ())
}

/// Parse, resolve and vet a `filters.query`, returning the tree the evaluator
/// lowers.
///
/// **The single place the resource is chosen**, and it has to stay that way:
/// validating against one resource and evaluating against another would accept
/// rules at write time that can never match, which is the failure mode this
/// whole path exists to avoid.
pub fn parse_query(q: &str) -> Result<sauron_query::ResolvedNode, String> {
    let ast = sauron_query::parse(q).map_err(|e| format!("query is not valid: {e}"))?;
    let node = sauron_query::resolve(&ast, sauron_query::Resource::Occurrences)
        .map_err(|e| format!("query is not valid: {e}"))?;
    if names_an_environment(&node) {
        return Err(
            "query may not filter on `environment` — use the rule's own \
                    environment filter, which resolves the name across every app \
                    the rule covers"
                .into(),
        );
    }
    Ok(node)
}

/// Whether any leaf addresses the environment column.
///
/// Matched on the STORE rather than the dimension name, exactly as
/// `sauron-api`'s `reject_withheld_environment` is: a name test would miss an
/// alias and would also catch a same-named dimension over a different column.
fn names_an_environment(node: &sauron_query::ResolvedNode) -> bool {
    match node {
        sauron_query::ResolvedNode::Pred(p) => {
            matches!(p.dim.store, sauron_query::Store::Column("environment_id"))
        }
        sauron_query::ResolvedNode::Text(_) => false,
        sauron_query::ResolvedNode::Not(inner) => names_an_environment(inner),
        sauron_query::ResolvedNode::And(v) | sauron_query::ResolvedNode::Or(v) => {
            v.iter().any(names_an_environment)
        }
    }
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
    fn a_query_filter_is_parsed_and_blank_is_treated_as_absent() {
        let c = Conditions::from_value(
            TriggerType::ErrorThreshold,
            &json!({ "filters": { "query": "extra.title=noInternetConnectionTitle" } }),
        );
        assert_eq!(
            c.filters.query.as_deref(),
            Some("extra.title=noInternetConnectionTitle")
        );

        // An empty string is a cleared input, not a query that matches
        // nothing — the same rule every other filter here follows.
        let blank = Conditions::from_value(
            TriggerType::ErrorThreshold,
            &json!({ "filters": { "query": "" } }),
        );
        assert_eq!(blank.filters.query, None);
    }

    /// A rule whose query does not parse must be refused at WRITE time. Left
    /// to evaluation it becomes a rule that quietly counts zero every 30s
    /// forever — indistinguishable from "nothing is wrong", which is the worst
    /// possible failure for an alert.
    /// Note the example: `extra.title:` with an EMPTY value is not an error —
    /// the lexer reads it as free text, deliberately. An unmatched paren is a
    /// real parse failure, and is what this asserts on.
    #[test]
    fn an_unparseable_query_is_rejected_on_write() {
        let err = validate_conditions(
            TriggerType::ErrorThreshold,
            &json!({ "filters": { "query": "(level:error" } }),
        )
        .unwrap_err();
        assert!(err.contains("query"), "{err}");

        // …and the empty-value form really does pass, so the line above is
        // not accidentally asserting on the wrong failure.
        assert!(validate_conditions(
            TriggerType::ErrorThreshold,
            &json!({ "filters": { "query": "extra.title:" } }),
        )
        .is_ok());
    }

    #[test]
    fn a_query_naming_an_unknown_field_is_rejected_on_write() {
        let err = validate_conditions(
            TriggerType::ErrorThreshold,
            &json!({ "filters": { "query": "extar.title=x" } }),
        )
        .unwrap_err();
        assert!(
            err.contains("extar"),
            "the message must name the field: {err}"
        );
    }

    /// `environment` is already a first-class rule filter, and that one
    /// resolves enrollment ids across every app the rule covers. The query
    /// language's version resolves against a SINGLE app, so allowing both
    /// would give one rule two environment filters with different meanings.
    #[test]
    fn an_environment_predicate_in_the_query_is_rejected_with_a_pointer() {
        let err = validate_conditions(
            TriggerType::ErrorThreshold,
            &json!({ "filters": { "query": "environment:staging" } }),
        )
        .unwrap_err();
        assert!(err.contains("environment"), "{err}");
    }

    #[test]
    fn a_valid_query_passes_validation() {
        assert!(validate_conditions(
            TriggerType::ErrorThreshold,
            &json!({ "filters": { "query": "extra.title=noInternetConnectionTitle level:error" } }),
        )
        .is_ok());
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
