//! `sauron-monitor` — the active uptime prober.
//!
//! A scheduler loop claims due monitors (FOR UPDATE SKIP LOCKED), probes them
//! concurrently, applies the state machine, persists checks/incidents, and
//! notifies on transitions (the legacy per-monitor `webhook_url` plus any
//! matching alert rules). State lives in Postgres; Redis is used only for the
//! cross-process alert throttle.
//!
//! Notification delivery is deliberately **off the tick's critical path**: it is
//! dispatched to a detached task that the tick does not join, so one dead
//! webhook host cannot stall probing for every other tenant.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Semaphore;
use tracing::{info, warn};

use sauron_alerts::{AlertContext, AlertEngine, SecretCipher, Severity};
use sauron_core::Config;
use sauron_db::models::Monitor;
use sauron_db::{repo, PgPool};
use sauron_monitor_core::{
    apply, probe, status_str, Kind, MonitorState, ProbeResult, ProbeSpec, Status, TransitionKind,
    WebhookPayload,
};
use sauron_redis::RedisStore;

/// Shared handles the notification path needs.
#[derive(Clone)]
struct Notifier {
    pool: PgPool,
    redis: RedisStore,
    engine: Arc<AlertEngine>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    sauron_telemetry::init("sauron-monitor");
    let cfg = Arc::new(Config::from_env()?);
    // Up to `monitor_max_concurrency` probe tasks each check out a connection to
    // persist results; size the pool to match, with headroom for the claim/prune
    // connection used on the main loop and for detached notification tasks.
    let pool_size = cfg.monitor_max_concurrency + 8; // build_pool's max_size is `usize`
    let pool = sauron_db::build_pool(&cfg.database_url, pool_size)?;
    let redis = RedisStore::connect(&cfg.redis_url).await?;

    // The SSRF-guarding resolver validates addresses inside the resolution the
    // client connects with, so a rebinding DNS answer cannot redirect a probe or
    // a webhook to a private address.
    let http = sauron_monitor_core::guarded_client_builder(cfg.monitor_ssrf_allow_private)
        .user_agent("Sauron-Monitor/1.0")
        .build()?;

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
    let notifier = Notifier {
        pool: pool.clone(),
        redis,
        engine: Arc::new(AlertEngine::new(
            SecretCipher::new(&notify_key),
            cfg.alerts_allow_private,
            cfg.alerts_deliver_timeout_ms,
        )),
    };

    info!(tick_ms = cfg.monitor_tick_ms, "sauron-monitor started");

    let mut last_prune = chrono::Utc::now();
    loop {
        if let Err(e) = tick(&pool, &http, &cfg, &notifier).await {
            warn!(error = %e, "monitor tick failed; backing off");
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        // Prune old checks roughly hourly.
        if (chrono::Utc::now() - last_prune).num_minutes() >= 60 {
            if let Ok(mut conn) = sauron_db::conn(&pool).await {
                match repo::prune_checks(&mut conn, cfg.monitor_check_retention_days).await {
                    Ok(n) if n > 0 => info!(pruned = n, "pruned old monitor checks"),
                    _ => {}
                }
            }
            last_prune = chrono::Utc::now();
        }
        tokio::time::sleep(Duration::from_millis(cfg.monitor_tick_ms)).await;
    }
}

async fn tick(
    pool: &PgPool,
    http: &reqwest::Client,
    cfg: &Config,
    notifier: &Notifier,
) -> anyhow::Result<()> {
    let mut conn = sauron_db::conn(pool).await?;
    let due = repo::claim_due_monitors(&mut conn, cfg.monitor_batch).await?;
    drop(conn); // release the pooled connection while probing
    if due.is_empty() {
        return Ok(());
    }
    let sem = Arc::new(Semaphore::new(cfg.monitor_max_concurrency));
    let mut handles = Vec::new();
    for m in due {
        let pool = pool.clone();
        let http = http.clone();
        let sem = sem.clone();
        let notifier = notifier.clone();
        let allow_private = cfg.monitor_ssrf_allow_private;
        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await;
            if let Err(e) = process_monitor(&pool, &http, &m, allow_private, &notifier).await {
                warn!(monitor = %m.id, error = %e, "monitor processing failed");
            }
        }));
    }
    // Only probe+persist tasks are joined here. Notification delivery is
    // dispatched detached (see `process_monitor`), so a slow webhook endpoint
    // never delays the next claim batch.
    for h in handles {
        if let Err(e) = h.await {
            warn!(error = %e, "monitor task panicked");
        }
    }
    Ok(())
}

fn spec_of(m: &Monitor) -> ProbeSpec {
    let cfg = &m.config;
    let headers = cfg
        .get("headers")
        .and_then(|h| h.as_object())
        .map(|o| {
            o.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();
    ProbeSpec {
        kind: if m.kind == "tcp" {
            Kind::Tcp
        } else {
            Kind::Http
        },
        target: m.target.clone(),
        method: m.method.clone(),
        headers,
        body: cfg
            .get("body")
            .and_then(|b| b.as_str())
            .map(|s| s.to_string()),
        expected_status: cfg
            .get("expected_status")
            .and_then(|s| s.as_str())
            .unwrap_or("200-399")
            .to_string(),
        body_assertion: cfg
            .get("body_assertion")
            .and_then(|s| s.as_str())
            .map(|s| s.to_string()),
        // Carried for forward-compat; NOT enforced per-monitor in the MVP. For SSRF
        // safety the prober does not follow redirects at all: the shared `http`
        // client uses `Policy::none()`, so a redirect response is simply recorded
        // as the probe result rather than followed. This field is retained but not
        // honored.
        follow_redirects: cfg
            .get("follow_redirects")
            .and_then(|b| b.as_bool())
            .unwrap_or(true),
        // Never let a probe outlive its own cadence: a 1s monitor with the
        // default 10s timeout would otherwise stack ~10 overlapping in-flight
        // probes (the claim advances next_check_at by the interval before a slow
        // probe returns). Cap the effective timeout at the interval.
        timeout: Duration::from_millis(
            m.timeout_ms
                .min(m.interval_seconds.saturating_mul(1000))
                .max(1) as u64,
        ),
    }
}

async fn process_monitor(
    pool: &PgPool,
    http: &reqwest::Client,
    m: &Monitor,
    allow_private: bool,
    notifier: &Notifier,
) -> anyhow::Result<()> {
    let spec = spec_of(m);
    let result: ProbeResult = probe(&spec, http, allow_private).await;

    let cur = match m.status.as_str() {
        "up" => Status::Up,
        "down" => Status::Down,
        "paused" => Status::Paused,
        _ => Status::Unknown,
    };
    let state = MonitorState {
        status: cur,
        consecutive_failures: m.consecutive_failures,
        consecutive_successes: m.consecutive_successes,
        failure_threshold: m.failure_threshold.max(1),
        recovery_threshold: m.recovery_threshold.max(1),
    };
    let outcome = apply(&state, &result);
    let changed = outcome.transition != TransitionKind::None;

    let mut conn = sauron_db::conn(pool).await?;
    repo::record_check_and_state(
        &mut conn,
        m.id,
        result.up,
        result.status_code,
        result.response_time_ms,
        result.error.as_deref(),
        status_str(outcome.new_status),
        outcome.consecutive_failures,
        outcome.consecutive_successes,
        changed,
    )
    .await?;

    let mut incident_id = None;
    match outcome.transition {
        TransitionKind::WentDown => {
            let cause = result
                .error
                .clone()
                .unwrap_or_else(|| "check failed".into());
            incident_id =
                Some(repo::open_incident(&mut conn, m.id, &cause, result.error.as_deref()).await?);
        }
        TransitionKind::WentUp => {
            repo::resolve_incident(&mut conn, m.id).await?;
        }
        TransitionKind::None => {}
    }
    drop(conn);

    if changed {
        // Detached: the tick does NOT join this task, and the probe's
        // concurrency permit is released when process_monitor returns, so a
        // slow or blackholed endpoint cannot stall probing for other monitors.
        let http = http.clone();
        let notifier = notifier.clone();
        let monitor = m.clone();
        let previous = status_str(cur).to_string();
        let status = status_str(outcome.new_status).to_string();
        let cause = result.error.clone();
        let transition = outcome.transition;
        tokio::spawn(async move {
            notify_transition(
                &http,
                &notifier,
                &monitor,
                &previous,
                &status,
                incident_id,
                cause.as_deref(),
                allow_private,
                transition,
            )
            .await;
        });
    }
    Ok(())
}

/// Fan a monitor transition out to the legacy per-monitor webhook and to every
/// matching alert rule. Runs detached from the probe loop.
#[allow(clippy::too_many_arguments)]
async fn notify_transition(
    http: &reqwest::Client,
    notifier: &Notifier,
    m: &Monitor,
    previous: &str,
    status: &str,
    incident_id: Option<uuid::Uuid>,
    cause: Option<&str>,
    allow_private: bool,
    transition: TransitionKind,
) {
    // 1. Legacy single-URL webhook (unchanged behaviour, still supported).
    if let Some(url) = &m.webhook_url {
        fire_webhook(
            http,
            url,
            m,
            previous,
            status,
            incident_id,
            cause,
            allow_private,
        )
        .await;
    }

    // 2. Admin-configured alert rules for this project.
    let trigger = match transition {
        TransitionKind::WentDown => "monitor_down",
        TransitionKind::WentUp => "monitor_up",
        TransitionKind::None => return,
    };
    let mut conn = match sauron_db::conn(&notifier.pool).await {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "alert dispatch: no db connection");
            return;
        }
    };
    let rules = match repo::alert_rules_for_monitor(&mut conn, m.project_id, trigger).await {
        Ok(r) => r,
        Err(e) => {
            warn!(error = %e, "alert dispatch: loading rules failed");
            return;
        }
    };
    if rules.is_empty() {
        return;
    }
    // Load each rule's channels up front, while the connection is still held.
    let mut channels_by_rule = Vec::with_capacity(rules.len());
    for rule in &rules {
        match repo::channels_for_rule(&mut conn, rule.id).await {
            Ok(cs) => channels_by_rule.push(cs),
            Err(e) => {
                warn!(rule = %rule.id, error = %e, "alert dispatch: loading channels failed");
                channels_by_rule.push(Vec::new());
            }
        }
    }
    // Release before the delivery loop: `fire` takes its own short-lived
    // connections, and this pool is shared with the probe writers.
    drop(conn);

    let down = transition == TransitionKind::WentDown;
    for (rule, channels) in rules.into_iter().zip(channels_by_rule) {
        let severity = sauron_alerts::rule::TriggerType::parse(&rule.trigger_type)
            .map(|_| Severity::parse(&rule.severity))
            .unwrap_or(Severity::Warning);
        let mut ctx = AlertContext::new(severity, trigger)
            .var("monitor", m.name.clone())
            .var("target", m.target.clone())
            .var("status", status.to_string())
            .var("previous_status", previous.to_string())
            .var("project_id", m.project_id.to_string());
        if let Some(c) = cause {
            ctx = ctx.var("cause", c.to_string());
        }
        ctx.title = if down {
            format!("Monitor down: {}", m.name)
        } else {
            format!("Monitor recovered: {}", m.name)
        };
        ctx.summary = if down {
            format!(
                "{} ({}) is DOWN — {}",
                m.name,
                m.target,
                cause.unwrap_or("check failed")
            )
        } else {
            format!("{} ({}) recovered and is UP again.", m.name, m.target)
        };

        // Dedup per incident so a flapping monitor cannot re-alert for the same
        // outage; recovery keys on the transition itself.
        let dedup = match incident_id {
            Some(id) => format!("rule:{}:incident:{}:{}", rule.id, id, trigger),
            None => format!("rule:{}:monitor:{}:{}", rule.id, m.id, trigger),
        };
        notifier
            .engine
            .fire(
                &notifier.pool,
                &notifier.redis,
                &rule,
                &channels,
                &ctx,
                &dedup,
            )
            .await;
    }
}

/// Total budget for a webhook delivery including all retries. Bounds how long a
/// single unresponsive endpoint can occupy a notification task.
const WEBHOOK_TOTAL_BUDGET: Duration = Duration::from_secs(15);
const WEBHOOK_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(5);
const WEBHOOK_ATTEMPTS: u32 = 3;

#[allow(clippy::too_many_arguments)]
async fn fire_webhook(
    http: &reqwest::Client,
    url: &str,
    m: &Monitor,
    previous: &str,
    status: &str,
    incident_id: Option<uuid::Uuid>,
    cause: Option<&str>,
    allow_private: bool,
) {
    let host = reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|s| s.to_string()));
    let host = match host {
        Some(h) => h,
        None => {
            warn!("webhook url has no host, skipping");
            return;
        }
    };
    // Pre-flight rejects IP-literal targets (which hyper resolves internally,
    // bypassing the client's guarding resolver). For hostnames the resolver on
    // `http` is authoritative and cannot be bypassed by a rebinding answer.
    if let Err(e) = sauron_monitor_core::ssrf::resolve_checked(&host, allow_private).await {
        warn!(error = %e, "webhook target blocked by SSRF guard, skipping");
        return;
    }

    let payload = WebhookPayload {
        monitor_id: m.id,
        name: &m.name,
        project_id: m.project_id,
        status,
        previous_status: previous,
        at: chrono::Utc::now(),
        incident_id,
        cause,
        target: &m.target,
    };

    let deadline = tokio::time::Instant::now() + WEBHOOK_TOTAL_BUDGET;
    for attempt in 0..WEBHOOK_ATTEMPTS {
        let res = http
            .post(url)
            .timeout(WEBHOOK_ATTEMPT_TIMEOUT)
            .json(&payload)
            .send()
            .await;
        match res {
            Ok(r) if r.status().is_success() => return,
            Ok(r) => warn!(status = %r.status(), "webhook non-2xx"),
            Err(e) => warn!(error = %e, "webhook post failed"),
        }
        // No sleep after the final attempt, and never sleep past the budget.
        if attempt + 1 == WEBHOOK_ATTEMPTS {
            break;
        }
        let backoff = Duration::from_millis(300 * (attempt as u64 + 1));
        if tokio::time::Instant::now() + backoff >= deadline {
            warn!("webhook retry budget exhausted, giving up");
            break;
        }
        tokio::time::sleep(backoff).await;
    }
}
