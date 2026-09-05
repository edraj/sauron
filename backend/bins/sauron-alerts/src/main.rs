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

mod drain;
mod subs;

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::json;
use tokio::sync::Semaphore;
use tracing::{info, warn};

use sauron_alerts::rule::{self, Conditions, TriggerType};
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

    // Fail-closed, no JWT_SECRET fallback. Booting with a key derived from
    // something else would not fail here — it would fail at delivery time, as a
    // per-channel "secret decrypt failed" buried in `alert_events`.
    let engine = Arc::new(AlertEngine::new(
        SecretCipher::new(cfg.require_notify_secret_key()?),
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
    let mut last_subs_eval = Utc::now() - chrono::Duration::days(1);
    // NOT dated into the past like the others: the sweep is the expensive
    // whole-table pass, and running it during boot — before the process has
    // even proven it can reach Postgres — buys nothing. The synchronous sweeps
    // in `routes/orgs.rs` already cover every deliberate grant change; this slot
    // exists only for the paths nobody remembered.
    let mut last_sweep = Utc::now();
    let mut tick_counter: u64 = 0;
    loop {
        tick_counter = tick_counter.wrapping_add(1);
        if let Err(e) = evaluate_all(&pool, &redis, &engine).await {
            warn!(error = %e, "alert evaluation tick failed");
        }

        // 120s by default, deliberately slower than the 30s org tick: personal
        // email does not need 30s latency, and cadence is the single largest
        // cost lever in this subsystem.
        let subs_tick = cfg.notify_subs_tick_secs.clamp(30, 3600) as i64;
        if (Utc::now() - last_subs_eval).num_seconds() >= subs_tick {
            if let Err(e) = subs::evaluate_subscriptions(&pool, &redis, &cfg, tick_counter).await {
                warn!(error = %e, "subscription evaluation tick failed");
            }
            last_subs_eval = Utc::now();
        }

        // Every tick, not on the subscription cadence, so `immediate` really is
        // immediate.
        if let Err(e) = drain::drain_notification_queue(&pool, &cfg).await {
            warn!(error = %e, "notification drain failed");
        }

        // The daily backstop for revocations no handler caught: a role's
        // permission list edited, a project deleted, an app removed. The
        // synchronous sweeps in `routes/orgs.rs` close the 24-hour window for
        // the three deliberate grant-mutation paths; this closes it for
        // everything else.
        if (Utc::now() - last_sweep).num_hours() >= 24 {
            match sauron_db::conn(&pool).await {
                Ok(mut conn) => {
                    match sauron_alerts::sweep::sweep_revoked_subscriptions(&mut conn).await {
                        Ok(n) if n > 0 => {
                            info!(disabled = n, "subscriptions disabled: owner lost reach")
                        }
                        Ok(_) => {}
                        Err(e) => warn!(error = %e, "revocation sweep failed"),
                    }
                }
                Err(e) => warn!(error = %e, "revocation sweep: no database connection"),
            }
            last_sweep = Utc::now();
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
                    // A queue's reaper runs in the process that DRAINS it, and
                    // `notification_queue` is drained right here.
                    match repo::prune_notification_queue(
                        &mut conn,
                        cfg.notify_queue_retention_days.clamp(1, 365) as i32,
                    )
                    .await
                    {
                        Ok(n) if n > 0 => info!(pruned = n, "pruned finished notifications"),
                        Ok(_) => {}
                        Err(e) => warn!(error = %e, "pruning notification queue failed"),
                    }
                    // No graceful shutdown exists anywhere in this codebase, so
                    // a process killed mid-drain leaves rows `claimed` forever.
                    match repo::requeue_stuck_notifications(
                        &mut conn,
                        repo::STUCK_CLAIM_SECS,
                        repo::MAX_QUEUE_ATTEMPTS,
                    )
                    .await
                    {
                        Ok(n) if n > 0 => info!(requeued = n, "requeued stuck notifications"),
                        Ok(_) => {}
                        Err(e) => warn!(error = %e, "requeueing stuck notifications failed"),
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

    // The rule's search-language narrowing, parsed once per tick. `rule::
    // parse_query` is the same call the write path validates with, so the two
    // cannot disagree about what resolves.
    //
    // An unparseable query **skips the rule** rather than counting unfiltered.
    // Both outcomes are bad — a rule that never fires, or a rule that fires on
    // events it was never asked about — and the second is worse: it pages
    // someone with a wrong answer, whereas this one is loud in the log and
    // cannot be reached by any rule the API accepted.
    let query_node = match cond.filters.query.as_deref() {
        Some(q) => match rule::parse_query(q) {
            Ok(node) => Some(node),
            Err(e) => {
                warn!(
                    rule_id = %rule.id,
                    query = q,
                    error = %e,
                    "alert rule carries an unusable query — rule skipped this tick"
                );
                return Ok(());
            }
        },
        None => None,
    };
    // No environment map: `parse_query` refuses an `environment` predicate, so
    // there is never a name here to resolve. Were one to arrive anyway it
    // would lower to `Uuid::nil()` and match nothing — a rule that does not
    // fire, never a rule that fires wrongly.
    let prep_ctx = sauron_db::query_plan::PrepCtx {
        environments: std::collections::HashMap::new(),
        now,
    };
    let alert_query = query_node.as_ref().map(|node| repo::AlertQuery {
        node,
        ctx: &prep_ctx,
    });
    let alert_query = alert_query.as_ref();

    // The admin-facing input is an environment NAME, which is the right thing
    // to type into a rule dialog — but `error_events.environment_id` holds an
    // `app_environments` ENROLLMENT id, and before this the count compared it
    // against the project-level catalogue, so every environment-filtered rule
    // in the product had been counting zero since migration 33. Resolve here,
    // once, and pass ids down. A misspelled name resolves to an empty set and
    // keeps counting zero — now deliberately, and visibly, rather than by
    // accident.
    let env_ids: Option<Vec<uuid::Uuid>> = match cond.filters.environment.as_deref() {
        Some(name) => Some(repo::enrollment_ids_for_env_name(&mut conn, &app_ids, name).await?),
        None => None,
    };
    let env_ids_ref = env_ids.as_deref();

    match trigger {
        TriggerType::IssueNew | TriggerType::IssueRegression => {
            let issues = if trigger == TriggerType::IssueNew {
                repo::alert_new_issues(
                    &mut conn,
                    &app_ids,
                    since,
                    now,
                    cond.filters.level.as_deref(),
                    20,
                )
                .await?
            } else {
                repo::alert_regressed_issues(
                    &mut conn,
                    &app_ids,
                    since,
                    now,
                    cond.filters.level.as_deref(),
                    20,
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
                engine
                    .fire(pool, redis, &rule, &channels, &ctx, &dedup)
                    .await;
            }
        }
        TriggerType::ErrorThreshold => {
            let from = now - window;
            let count = repo::alert_count_errors(
                &mut conn,
                repo::AlertErrorCount {
                    app_ids: &app_ids,
                    from,
                    to: now,
                    level: cond.filters.level.as_deref(),
                    env_ids: env_ids_ref,
                    tag: tag.as_ref(),
                    query: alert_query,
                },
            )
            .await?;
            if cond.fires(count as f64) {
                let mins = cond.window_seconds / 60;
                let mut ctx = AlertContext::new(severity, trigger.as_str())
                    .var("count", count.to_string())
                    .var("threshold", fmt_num(cond.threshold))
                    .var("window_minutes", mins.to_string())
                    .var("query", cond.filters.query.clone().unwrap_or_default());
                ctx.title = format!("Error threshold crossed ({count} in {mins}m)");
                ctx.summary = format!(
                    "{count} error event(s){} in the last {mins} minute(s) (threshold {}).",
                    match_clause(cond.filters.query.as_deref()),
                    fmt_num(cond.threshold)
                );
                let dedup = format!("rule:{}:error_threshold", rule.id);
                engine
                    .fire(pool, redis, &rule, &channels, &ctx, &dedup)
                    .await;
            }
        }
        TriggerType::ErrorSpike => {
            let from = now - window;
            let prev_from = from - window;
            let current = repo::alert_count_errors(
                &mut conn,
                repo::AlertErrorCount {
                    app_ids: &app_ids,
                    from,
                    to: now,
                    level: cond.filters.level.as_deref(),
                    env_ids: env_ids_ref,
                    tag: tag.as_ref(),
                    query: alert_query,
                },
            )
            .await?;
            let previous = repo::alert_count_errors(
                &mut conn,
                repo::AlertErrorCount {
                    app_ids: &app_ids,
                    from: prev_from,
                    to: from,
                    level: cond.filters.level.as_deref(),
                    env_ids: env_ids_ref,
                    tag: tag.as_ref(),
                    query: alert_query,
                },
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
                    .var("window_minutes", mins.to_string())
                    .var("query", cond.filters.query.clone().unwrap_or_default());
                ctx.title = format!("Error spike: {factor:.1}× in {mins}m");
                ctx.summary = format!(
                    "{current} error event(s){} in the last {mins} minute(s) vs {previous} in \
                     the previous {mins} — a {factor:.1}× increase.",
                    match_clause(cond.filters.query.as_deref())
                );
                let dedup = format!("rule:{}:error_spike", rule.id);
                engine
                    .fire(pool, redis, &rule, &channels, &ctx, &dedup)
                    .await;
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
                env_ids_ref,
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
                engine
                    .fire(pool, redis, &rule, &channels, &ctx, &dedup)
                    .await;
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
                    engine
                        .fire(pool, redis, &rule, &channels, &ctx, &dedup)
                        .await;
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
/// The " matching `<query>`" clause an alert body carries when its rule is
/// narrowed, and nothing at all when it is not.
///
/// A rule that fires on one specific exception must SAY which, or the
/// notification is indistinguishable from the unfiltered rule beside it — the
/// recipient sees "3 error events in the last minute" either way and cannot
/// tell which alarm went off.
fn match_clause(query: Option<&str>) -> String {
    match query {
        Some(q) => format!(" matching `{q}`"),
        None => String::new(),
    }
}

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

    // =======================================================================
    // `evaluate_rule` against a real database.
    //
    // The two tests above are the whole of this binary's previous coverage,
    // and both are pure helpers — the evaluation path itself had never
    // executed under test. Everything AROUND it was proven separately (the
    // count query in `sauron-db`'s `notifications.rs`, the write-time
    // validation in `sauron-alerts`' `rule.rs`, the authorization in
    // `sauron-api`'s `http_alerting.rs`) and nothing had ever connected them
    // and watched an alert appear.
    //
    // A rule with NO channels still writes its `alert_events` row — status
    // `skipped`, title and body intact (`AlertEngine::fire`) — so the whole
    // decision is observable without a delivery destination, a network stub,
    // or a webhook that could fail for its own reasons.
    // =======================================================================

    use sauron_db::models::{NewAlertRule, NewErrorEvent, NewIssue};
    use serde_json::json as j;

    struct Rig {
        /// `Option` so [`Rig::cleanup`] can take it and drop every connection
        /// before asking the server to drop the database — and so `Drop` can
        /// tell a cleaned rig from a leaked one without a second flag.
        pool: Option<sauron_db::PgPool>,
        redis: RedisStore,
        engine: AlertEngine,
        org_id: uuid::Uuid,
        app_id: uuid::Uuid,
        admin_url: String,
        db_name: String,
    }

    impl Rig {
        /// Seeds three error events: two carrying
        /// `extra.title = noInternetConnectionTitle`, one carrying a different
        /// title. Three, not two, so "narrowed" and "unfiltered" produce
        /// DIFFERENT counts — with an all-matching fixture a predicate that
        /// silently dropped would still look right.
        async fn setup() -> Option<Rig> {
            let admin_url = std::env::var("TEST_DATABASE_URL").ok()?;
            let redis_url = std::env::var("TEST_REDIS_URL").ok()?;
            let db_name = format!("sauron_test_eval_{}", uuid::Uuid::new_v4().simple());
            sauron_db::create_test_database(&admin_url, &db_name)
                .await
                .expect("create migrated ephemeral database");
            let db_url = {
                let (base, _) = admin_url.rsplit_once('/').expect("database url has a path");
                format!("{base}/{db_name}")
            };
            let pool = sauron_db::build_pool(&db_url, 4).expect("build pool");
            let redis = RedisStore::connect(&redis_url).await.expect("redis");
            let engine = AlertEngine::new(SecretCipher::new("evaluator-test-key"), false, 1000);

            let mut conn = sauron_db::conn(&pool).await.expect("checkout");
            let suffix = uuid::Uuid::new_v4().simple().to_string();
            let org = repo::create_org(&mut conn, "eval org", &format!("eval-org-{suffix}"))
                .await
                .expect("org");
            let project = repo::create_project(
                &mut conn,
                org.id,
                "eval project",
                &format!("eval-p-{suffix}"),
            )
            .await
            .expect("project");
            let app = repo::create_app(
                &mut conn,
                project.id,
                "eval app",
                &format!("eval-a-{suffix}"),
                "flutter",
            )
            .await
            .expect("app");
            let issue = repo::upsert_issue(
                &mut conn,
                NewIssue {
                    app_id: app.id,
                    fingerprint: &format!("eval-fp-{suffix}"),
                    type_: "SocketException",
                    title: "no internet",
                    culprit: "eval::seed",
                    level: "error",
                    first_seen: Utc::now(),
                    last_seen: Utc::now(),
                    times_seen: 3,
                },
            )
            .await
            .expect("issue");

            for title in [
                "noInternetConnectionTitle",
                "noInternetConnectionTitle",
                "someOtherTitle",
            ] {
                repo::insert_error_event(
                    &mut conn,
                    NewErrorEvent {
                        id: uuid::Uuid::new_v4(),
                        app_id: app.id,
                        environment_id: None,
                        issue_id: issue,
                        fingerprint: format!("eval-fp-{suffix}"),
                        level: "error".into(),
                        message: "boom".into(),
                        exception_type: "SocketException".into(),
                        exception_value: "boom".into(),
                        stacktrace: j!([]),
                        breadcrumbs: j!([]),
                        context: j!({}),
                        tags: j!({}),
                        release: None,
                        distinct_id: None,
                        event_user: None,
                        sdk: None,
                        ip_address: None,
                        // Inside every window under test, and safely clear of
                        // the `(from, to]` upper bound.
                        occurred_at: Utc::now() - chrono::Duration::seconds(5),
                        session_id: None,
                        device_key: None,
                        screen: None,
                        workflow_id: None,
                        workflow_name: None,
                        stacktrace_symbolicated: None,
                        symbolication_status: "not_applicable".into(),
                        debug_meta: None,
                        contexts: j!({}),
                        extra: j!({ "title": title }),
                        handled: Some(false),
                        title: None,
                        culprit: None,
                        stacktrace_sha256: None,
                    },
                )
                .await
                .expect("insert error event");
            }
            drop(conn);

            Some(Rig {
                pool: Some(pool),
                redis,
                engine,
                org_id: org.id,
                app_id: app.id,
                admin_url,
                db_name,
            })
        }

        /// A channel-less `error_threshold` rule over this app, carrying
        /// `conditions` verbatim — including a query the API would have
        /// refused, which is how the "stored rule is unusable" branch is
        /// reachable at all.
        async fn rule(&self, name: &str, conditions: serde_json::Value) -> AlertRule {
            let mut conn = sauron_db::conn(self.pool.as_ref().expect("pool taken"))
                .await
                .expect("checkout");
            repo::create_alert_rule(
                &mut conn,
                NewAlertRule {
                    org_id: self.org_id,
                    project_id: None,
                    app_id: Some(self.app_id),
                    monitor_id: None,
                    name,
                    trigger_type: "error_threshold",
                    conditions: &conditions,
                    severity: "warning",
                    // No throttle: two rules in one test must not suppress
                    // each other, and a throttled row would be a THIRD status
                    // to disambiguate for no benefit.
                    throttle_seconds: 0,
                    message_template: None,
                    last_evaluated_at: None,
                    created_by: None,
                },
            )
            .await
            .expect("create rule")
        }

        /// Every `alert_events` body written for one rule.
        ///
        /// Read through the same repo function `list_history` serves, rather
        /// than a hand-written query: this binary depends on `sauron-db`, not
        /// on diesel, and the shipped reader is the honest thing to assert on
        /// anyway — an event this cannot see is one an operator cannot see.
        async fn bodies(&self, rule_id: uuid::Uuid) -> Vec<String> {
            let mut conn = sauron_db::conn(self.pool.as_ref().expect("pool taken"))
                .await
                .expect("checkout");
            repo::list_alert_events_visible(&mut conn, self.org_id, &[rule_id], &[], 200, 0)
                .await
                .expect("load alert events")
                .into_iter()
                .map(|e| e.body)
                .collect()
        }

        /// Explicit, not `Drop`: async work cannot run there, and a leaked
        /// ephemeral database is the failure this project has already been
        /// bitten by once.
        async fn cleanup(mut self) {
            // Every connection must be back before the server will drop the
            // database out from under them.
            drop(self.pool.take());
            sauron_db::drop_database(&self.admin_url, &self.db_name)
                .await
                .expect("drop ephemeral test database");
        }
    }

    impl Drop for Rig {
        /// Async work cannot run in `Drop`, so a test that panicked before
        /// reaching `cleanup()` leaks its database. Say so loudly rather than
        /// attempt a runtime-in-`Drop` workaround — the same ruling
        /// `sauron-db`'s `TestDb` makes, and for the same reason: this project
        /// has already had a silent leak sit unnoticed for a whole session.
        fn drop(&mut self) {
            // A taken pool means `cleanup` ran and the database is gone.
            if self.pool.is_none() {
                return;
            }
            eprintln!(
                "WARNING: ephemeral test database {} leaked (the test panicked before \
                 cleanup). Drop it with: DROP DATABASE \"{}\" WITH (FORCE);",
                self.db_name, self.db_name
            );
        }
    }

    /// **The test this feature was missing.** Three events are in the window,
    /// two of which match the query, and the narrowed rule must count two.
    ///
    /// Counting THREE would mean the predicate never reached the query —
    /// exactly what a mis-wired `AlertQuery` produces, and exactly what every
    /// other test in the stack would have kept passing through.
    #[tokio::test]
    async fn a_query_narrowed_rule_counts_only_matching_events() {
        let Some(rig) = Rig::setup().await else {
            eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping");
            return;
        };

        let narrowed = rig
            .rule(
                "narrowed",
                j!({
                    "threshold": 1,
                    "window_seconds": 60,
                    "filters": { "query": "extra.title=noInternetConnectionTitle" }
                }),
            )
            .await;
        let unfiltered = rig
            .rule("unfiltered", j!({ "threshold": 1, "window_seconds": 60 }))
            .await;

        evaluate_rule(
            rig.pool.as_ref().expect("pool"),
            &rig.redis,
            &rig.engine,
            narrowed.clone(),
        )
        .await
        .expect("evaluate narrowed");
        evaluate_rule(
            rig.pool.as_ref().expect("pool"),
            &rig.redis,
            &rig.engine,
            unfiltered.clone(),
        )
        .await
        .expect("evaluate unfiltered");

        let narrowed_bodies = rig.bodies(narrowed.id).await;
        assert_eq!(narrowed_bodies.len(), 1, "the narrowed rule must fire once");
        assert!(
            narrowed_bodies[0].contains("2 error event(s)"),
            "must count only the two matching rows: {}",
            narrowed_bodies[0]
        );
        assert!(
            narrowed_bodies[0].contains("matching `extra.title=noInternetConnectionTitle`"),
            "the body must name the query, or it reads like the unfiltered rule: {}",
            narrowed_bodies[0]
        );

        // The control, and the reason the number above means something.
        let all = rig.bodies(unfiltered.id).await;
        assert_eq!(all.len(), 1);
        assert!(
            all[0].contains("3 error event(s)"),
            "the unfiltered rule sees all three: {}",
            all[0]
        );
        assert!(
            !all[0].contains("matching"),
            "and carries no match clause: {}",
            all[0]
        );

        rig.cleanup().await;
    }

    /// A query that matches nothing must leave the rule SILENT — not fire with
    /// a zero count, and not fire on the unfiltered population.
    #[tokio::test]
    async fn a_query_matching_nothing_fires_no_alert() {
        let Some(rig) = Rig::setup().await else {
            eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping");
            return;
        };
        let rule = rig
            .rule(
                "no match",
                j!({
                    "threshold": 1,
                    "window_seconds": 60,
                    "filters": { "query": "extra.title=neverHappens" }
                }),
            )
            .await;

        evaluate_rule(
            rig.pool.as_ref().expect("pool"),
            &rig.redis,
            &rig.engine,
            rule.clone(),
        )
        .await
        .expect("evaluate");

        assert!(
            rig.bodies(rule.id).await.is_empty(),
            "a rule whose query matches nothing must not fire"
        );
        rig.cleanup().await;
    }

    /// A stored rule whose query cannot resolve is SKIPPED, not counted
    /// unfiltered.
    ///
    /// Unreachable through the API — `validate_conditions` refuses it on write
    /// — so the row is inserted directly, which is also how it could arrive in
    /// production: a rule saved before the grammar changed under it. The
    /// failure this pins is the tempting one: falling back to an unfiltered
    /// count would page someone about events the rule never asked about.
    #[tokio::test]
    async fn a_rule_whose_stored_query_is_unusable_is_skipped_not_widened() {
        let Some(rig) = Rig::setup().await else {
            eprintln!("TEST_DATABASE_URL / TEST_REDIS_URL unset — skipping");
            return;
        };
        let rule = rig
            .rule(
                "unusable",
                j!({
                    "threshold": 1,
                    "window_seconds": 60,
                    "filters": { "query": "nonsenseField=1" }
                }),
            )
            .await;

        evaluate_rule(
            rig.pool.as_ref().expect("pool"),
            &rig.redis,
            &rig.engine,
            rule.clone(),
        )
        .await
        .expect("an unusable query is a skip, never an Err that kills the tick");

        assert!(
            rig.bodies(rule.id).await.is_empty(),
            "an unusable query must not fall back to an unfiltered count"
        );
        rig.cleanup().await;
    }
}
