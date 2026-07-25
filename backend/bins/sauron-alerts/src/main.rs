//! `sauron-alerts` — the metric-rule evaluator.
//!
//! Event-driven triggers (monitor up/down) fire inline from `sauron-monitor`.
//! Everything else is *polled* here rather than evaluated on the ingest hot
//! path: a threshold rule asks "how many errors in the last N minutes", which
//! is one indexed, window-bounded aggregate per rule per tick — instead of
//! re-checking every rule on every ingested event. Ingest throughput therefore
//! stays independent of how many alert rules an org has configured.
//!
//! Each tick:
//!   1. load enabled metric rules,
//!   2. resolve each rule's app scope,
//!   3. run its bounded window query,
//!   4. hand firing rules to the shared [`AlertEngine`] (throttle → render →
//!      deliver → record).
//!
//! A rule's evaluation failure is logged and skipped; it never stops the loop.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::json;
use tokio::sync::Semaphore;
use tracing::{info, warn};

use sauron_alerts::rule::{Conditions, TriggerType};
use sauron_alerts::{AlertContext, AlertEngine, SecretCipher, Severity};
use sauron_core::Config;
use sauron_db::models::AlertRule;
use sauron_db::{repo, PgPool};
use sauron_redis::RedisStore;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    sauron_telemetry::init("sauron-alerts");
    let cfg = Arc::new(Config::from_env()?);

    let pool = sauron_db::build_pool(&cfg.database_url, 8)?;
    let redis = RedisStore::connect(&cfg.redis_url).await?;

    let notify_key = match &cfg.notify_secret_key {
        Some(k) => k.clone(),
        None => {
            warn!(
                "NOTIFY_SECRET_KEY not set; deriving channel-secret key from JWT_SECRET \
                 (must match sauron-api's derivation or stored secrets will not decrypt)"
            );
            cfg.require_jwt_secret()?.to_string()
        }
    };
    let engine = Arc::new(AlertEngine::new(
        SecretCipher::new(&notify_key),
        cfg.alerts_allow_private,
        cfg.alerts_deliver_timeout_ms,
    ));

    let tick = Duration::from_secs(cfg.alerts_tick_secs.clamp(5, 3600));
    info!(
        tick_secs = tick.as_secs(),
        "sauron-alerts evaluator started"
    );

    // Dated so the first tick prunes: a fresh deploy should reclaim whatever
    // accumulated while nothing was reaping, not wait an hour to start.
    let mut last_prune = Utc::now() - chrono::Duration::days(1);
    loop {
        if let Err(e) = evaluate_all(&pool, &redis, &engine).await {
            warn!(error = %e, "alert evaluation tick failed");
        }
        // `alert_events` gains a row per evaluation — including every suppressed
        // one — so without a reaper a throttled rule grows it without bound.
        if (Utc::now() - last_prune).num_minutes() >= 60 {
            match sauron_db::conn(&pool).await {
                Ok(mut conn) => {
                    match repo::prune_alert_events(&mut conn, cfg.alert_event_retention_days).await
                    {
                        Ok(n) if n > 0 => info!(pruned = n, "pruned old alert events"),
                        Ok(_) => {}
                        Err(e) => warn!(error = %e, "pruning alert events failed"),
                    }
                }
                Err(e) => warn!(error = %e, "prune: no database connection"),
            }
            last_prune = Utc::now();
        }
        tokio::time::sleep(tick).await;
    }
}

/// Evaluate every enabled metric rule once.
async fn evaluate_all(
    pool: &PgPool,
    redis: &RedisStore,
    engine: &Arc<AlertEngine>,
) -> anyhow::Result<()> {
    let mut conn = sauron_db::conn(pool).await?;
    let rules = repo::enabled_metric_alert_rules(&mut conn).await?;
    drop(conn); // don't hold a pooled connection across the fan-out

    if rules.is_empty() {
        return Ok(());
    }

    // Bound concurrent rule evaluations so a large rule set cannot exhaust the
    // connection pool or stampede the database.
    let sem = Arc::new(Semaphore::new(4));
    let mut handles = Vec::with_capacity(rules.len());
    for rule in rules {
        let pool = pool.clone();
        let redis = redis.clone();
        let engine = engine.clone();
        let sem = sem.clone();
        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await;
            let rule_id = rule.id;
            if let Err(e) = evaluate_rule(&pool, &redis, &engine, rule).await {
                warn!(rule = %rule_id, error = %e, "rule evaluation failed");
            }
        }));
    }
    for h in handles {
        if let Err(e) = h.await {
            warn!(error = %e, "rule evaluation task panicked");
        }
    }
    Ok(())
}

async fn evaluate_rule(
    pool: &PgPool,
    redis: &RedisStore,
    engine: &AlertEngine,
    rule: AlertRule,
) -> anyhow::Result<()> {
    let Some(trigger) = TriggerType::parse(&rule.trigger_type) else {
        return Ok(()); // unknown trigger (shouldn't happen; CHECK-constrained)
    };
    let cond = Conditions::from_value(trigger, &rule.conditions);
    let severity = Severity::parse(&rule.severity);

    let mut conn = sauron_db::conn(pool).await?;
    let app_ids =
        repo::apps_in_alert_scope(&mut conn, rule.org_id, rule.project_id, rule.app_id).await?;
    if app_ids.is_empty() {
        // Nothing in scope yet — still advance the watermark so the rule does
        // not later replay a huge backlog once an app appears.
        repo::touch_rule_evaluated(&mut conn, rule.id, Utc::now()).await?;
        return Ok(());
    }

    // Loaded once per rule rather than per fired alert: a rule can fire for up
    // to 20 issues in a tick, and the channel list is the same for all of them.
    let channels = repo::channels_for_rule(&mut conn, rule.id).await?;

    let now = Utc::now();
    let window = chrono::Duration::seconds(cond.window_seconds);
    // Discrete triggers consume a half-open interval anchored on the last
    // evaluation so nothing is missed or double-reported across ticks; the
    // window length caps how far back a first/stalled evaluation may reach.
    let since = rule
        .last_evaluated_at
        .unwrap_or(now - window)
        .max(now - window);

    let tag = match (&cond.filters.tag_key, &cond.filters.tag_value) {
        (Some(k), Some(v)) => Some(json!({ k.clone(): v.clone() })),
        _ => None,
    };

    match trigger {
        TriggerType::IssueNew | TriggerType::IssueRegression => {
            let issues = if trigger == TriggerType::IssueNew {
                repo::alert_new_issues(
                    &mut conn,
                    &app_ids,
                    since,
                    now,
                    cond.filters.level.as_deref(),
                )
                .await?
            } else {
                repo::alert_regressed_issues(
                    &mut conn,
                    &app_ids,
                    since,
                    now,
                    cond.filters.level.as_deref(),
                )
                .await?
            };
            for issue in issues {
                let verb = if trigger == TriggerType::IssueNew {
                    "New issue"
                } else {
                    "Issue regressed"
                };
                let mut ctx = AlertContext::new(severity, trigger.as_str())
                    .var("issue_title", issue.title.clone())
                    .var("issue_level", issue.level.clone())
                    .var("app_id", issue.app_id.to_string())
                    .var("times_seen", issue.times_seen.to_string());
                ctx.title = format!("{verb}: {}", issue.title);
                ctx.summary = format!(
                    "{verb} ({}) in app {} — seen {} time(s).",
                    issue.level, issue.app_id, issue.times_seen
                );
                // Per-issue dedup: each distinct issue alerts once per throttle.
                let dedup = format!("rule:{}:issue:{}", rule.id, issue.id);
                engine.fire(pool, redis, &rule, &channels, &ctx, &dedup).await;
            }
        }
        TriggerType::ErrorThreshold => {
            let from = now - window;
            let count = repo::alert_count_errors(
                &mut conn,
                &app_ids,
                from,
                now,
                cond.filters.level.as_deref(),
                cond.filters.environment.as_deref(),
                tag.as_ref(),
            )
            .await?;
            if cond.fires(count as f64) {
                let mins = cond.window_seconds / 60;
                let mut ctx = AlertContext::new(severity, trigger.as_str())
                    .var("count", count.to_string())
                    .var("threshold", fmt_num(cond.threshold))
                    .var("window_minutes", mins.to_string());
                ctx.title = format!("Error threshold crossed ({count} in {mins}m)");
                ctx.summary = format!(
                    "{count} error event(s) in the last {mins} minute(s) (threshold {}).",
                    fmt_num(cond.threshold)
                );
                let dedup = format!("rule:{}:error_threshold", rule.id);
                engine.fire(pool, redis, &rule, &channels, &ctx, &dedup).await;
            }
        }
        TriggerType::ErrorSpike => {
            let from = now - window;
            let prev_from = from - window;
            let current = repo::alert_count_errors(
                &mut conn,
                &app_ids,
                from,
                now,
                cond.filters.level.as_deref(),
                cond.filters.environment.as_deref(),
                tag.as_ref(),
            )
            .await?;
            let previous = repo::alert_count_errors(
                &mut conn,
                &app_ids,
                prev_from,
                from,
                cond.filters.level.as_deref(),
                cond.filters.environment.as_deref(),
                tag.as_ref(),
            )
            .await?;
            // Require a real baseline and a real current volume, so 0→2 events
            // on a quiet app is not reported as an "infinite" spike.
            let spiked = previous > 0
                && current as f64 >= previous as f64 * cond.spike_factor
                && current as f64 >= cond.threshold.max(1.0);
            if spiked {
                let mins = cond.window_seconds / 60;
                let factor = current as f64 / previous as f64;
                let mut ctx = AlertContext::new(severity, trigger.as_str())
                    .var("count", current.to_string())
                    .var("previous_count", previous.to_string())
                    .var("factor", format!("{factor:.1}"))
                    .var("window_minutes", mins.to_string());
                ctx.title = format!("Error spike: {factor:.1}× in {mins}m");
                ctx.summary = format!(
                    "{current} error event(s) in the last {mins} minute(s) vs {previous} in the \
                     previous {mins} — a {factor:.1}× increase."
                );
                let dedup = format!("rule:{}:error_spike", rule.id);
                engine.fire(pool, redis, &rule, &channels, &ctx, &dedup).await;
            }
        }
        TriggerType::EventThreshold => {
            let from = now - window;
            let count = repo::alert_count_events(
                &mut conn,
                &app_ids,
                from,
                now,
                cond.filters.event_name.as_deref(),
                cond.filters.environment.as_deref(),
                tag.as_ref(),
            )
            .await?;
            if cond.fires(count as f64) {
                let mins = cond.window_seconds / 60;
                let name = cond
                    .filters
                    .event_name
                    .clone()
                    .unwrap_or_else(|| "any".into());
                let mut ctx = AlertContext::new(severity, trigger.as_str())
                    .var("count", count.to_string())
                    .var("threshold", fmt_num(cond.threshold))
                    .var("window_minutes", mins.to_string())
                    .var("event_name", name.clone());
                ctx.title = format!("Event threshold crossed: {name} ({count} in {mins}m)");
                ctx.summary = format!(
                    "{count} '{name}' event(s) in the last {mins} minute(s) (threshold {}).",
                    fmt_num(cond.threshold)
                );
                let dedup = format!("rule:{}:event_threshold", rule.id);
                engine.fire(pool, redis, &rule, &channels, &ctx, &dedup).await;
            }
        }
        TriggerType::PerfDegradation => {
            let from = now - window;
            let pct = percentile_of(&cond.metric);
            let value = repo::alert_latency_metric(
                &mut conn,
                &app_ids,
                from,
                now,
                pct,
                cond.filters.op.as_deref(),
            )
            .await?;
            if let Some(v) = value {
                if cond.fires(v) {
                    let mins = cond.window_seconds / 60;
                    let mut ctx = AlertContext::new(severity, trigger.as_str())
                        .var("value_ms", format!("{v:.0}"))
                        .var("threshold_ms", fmt_num(cond.threshold))
                        .var("metric", cond.metric.clone())
                        .var("window_minutes", mins.to_string());
                    ctx.title = format!("Latency {} = {v:.0}ms", cond.metric);
                    ctx.summary = format!(
                        "{} latency is {v:.0}ms over the last {mins} minute(s) (threshold {}ms).",
                        cond.metric,
                        fmt_num(cond.threshold)
                    );
                    let dedup = format!("rule:{}:perf", rule.id);
                    engine.fire(pool, redis, &rule, &channels, &ctx, &dedup).await;
                }
            }
        }
        // Dispatched inline by the prober, never polled here.
        TriggerType::MonitorDown | TriggerType::MonitorUp => {}
    }

    repo::touch_rule_evaluated(&mut conn, rule.id, now).await?;
    Ok(())
}

/// Map a whitelisted metric name to the fraction `percentile_cont` wants.
/// `None` = average; `Some(-1.0)` = max (see `repo::alert_latency_metric`).
fn percentile_of(metric: &str) -> Option<f64> {
    match metric {
        "p50" => Some(0.50),
        "p75" => Some(0.75),
        "p90" => Some(0.90),
        "p95" => Some(0.95),
        "p99" => Some(0.99),
        "max" => Some(-1.0),
        _ => None, // "avg"
    }
}

/// Render a threshold without a trailing `.0` when it is a whole number.
fn fmt_num(v: f64) -> String {
    if (v.fract()).abs() < f64::EPSILON {
        format!("{v:.0}")
    } else {
        format!("{v}")
    }
}

/// Unused today, but kept alongside the evaluator so the watermark type stays
/// explicit at call sites.
#[allow(dead_code)]
fn _assert_time_types(_: DateTime<Utc>) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_mapping() {
        assert_eq!(percentile_of("p95"), Some(0.95));
        assert_eq!(percentile_of("p50"), Some(0.50));
        assert_eq!(percentile_of("max"), Some(-1.0));
        assert_eq!(percentile_of("avg"), None);
        assert_eq!(percentile_of("bogus"), None);
    }

    #[test]
    fn fmt_num_drops_trailing_zero() {
        assert_eq!(fmt_num(10.0), "10");
        assert_eq!(fmt_num(2.5), "2.5");
    }
}
