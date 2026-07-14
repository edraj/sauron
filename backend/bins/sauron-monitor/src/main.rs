//! `sauron-monitor` — the active uptime prober.
//!
//! A scheduler loop claims due monitors (FOR UPDATE SKIP LOCKED), probes them
//! concurrently, applies the state machine, persists checks/incidents, and
//! fires webhooks. State lives entirely in Postgres; no Redis.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Semaphore;
use tracing::{info, warn};

use sauron_core::Config;
use sauron_db::models::Monitor;
use sauron_db::{repo, PgPool};
use sauron_monitor_core::{
    apply, probe, status_str, Kind, MonitorState, ProbeSpec, ProbeResult, Status, TransitionKind,
    WebhookPayload,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    sauron_telemetry::init("sauron-monitor");
    let cfg = Arc::new(Config::from_env()?);
    // Up to `monitor_max_concurrency` probe tasks each check out a connection to
    // persist results; size the pool to match, with headroom for the claim/prune
    // connection used on the main loop.
    let pool_size = cfg.monitor_max_concurrency + 4; // build_pool's max_size is `usize`
    let pool = sauron_db::build_pool(&cfg.database_url, pool_size)?;

    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(10))
        .user_agent("Sauron-Monitor/1.0")
        .build()?;

    info!(tick_ms = cfg.monitor_tick_ms, "sauron-monitor started");

    let mut last_prune = chrono::Utc::now();
    loop {
        if let Err(e) = tick(&pool, &http, &cfg).await {
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

async fn tick(pool: &PgPool, http: &reqwest::Client, cfg: &Config) -> anyhow::Result<()> {
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
        let allow_private = cfg.monitor_ssrf_allow_private;
        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await;
            if let Err(e) = process_monitor(&pool, &http, &m, allow_private).await {
                warn!(monitor = %m.id, error = %e, "monitor processing failed");
            }
        }));
    }
    for h in handles {
        if let Err(e) = h.await {
            warn!(error = %e, "monitor task panicked");
        }
    }
    Ok(())
}

fn spec_of(m: &Monitor) -> ProbeSpec {
    let cfg = &m.config;
    let headers = cfg.get("headers").and_then(|h| h.as_object()).map(|o| {
        o.iter().filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string()))).collect()
    }).unwrap_or_default();
    ProbeSpec {
        kind: if m.kind == "tcp" { Kind::Tcp } else { Kind::Http },
        target: m.target.clone(),
        method: m.method.clone(),
        headers,
        body: cfg.get("body").and_then(|b| b.as_str()).map(|s| s.to_string()),
        expected_status: cfg.get("expected_status").and_then(|s| s.as_str()).unwrap_or("200-399").to_string(),
        body_assertion: cfg.get("body_assertion").and_then(|s| s.as_str()).map(|s| s.to_string()),
        // Carried for forward-compat; NOT enforced per-monitor in the MVP. The shared
        // `http` client applies a fixed `Policy::limited(10)`, and per-request redirect
        // overrides aren't supported yet.
        follow_redirects: cfg.get("follow_redirects").and_then(|b| b.as_bool()).unwrap_or(true),
        timeout: Duration::from_millis(m.timeout_ms.max(1) as u64),
    }
}

async fn process_monitor(
    pool: &PgPool,
    http: &reqwest::Client,
    m: &Monitor,
    allow_private: bool,
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
            let cause = result.error.clone().unwrap_or_else(|| "check failed".into());
            incident_id = Some(repo::open_incident(&mut conn, m.id, &cause, result.error.as_deref()).await?);
        }
        TransitionKind::WentUp => {
            repo::resolve_incident(&mut conn, m.id).await?;
        }
        TransitionKind::None => {}
    }
    drop(conn);

    if changed {
        if let Some(url) = &m.webhook_url {
            fire_webhook(http, url, m, status_str(cur), status_str(outcome.new_status), incident_id, result.error.as_deref()).await;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn fire_webhook(
    http: &reqwest::Client,
    url: &str,
    m: &Monitor,
    previous: &str,
    status: &str,
    incident_id: Option<uuid::Uuid>,
    cause: Option<&str>,
) {
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
    for attempt in 0..3 {
        let res = http.post(url).timeout(Duration::from_secs(5)).json(&payload).send().await;
        match res {
            Ok(r) if r.status().is_success() => return,
            Ok(r) => warn!(status = %r.status(), "webhook non-2xx"),
            Err(e) => warn!(error = %e, "webhook post failed"),
        }
        tokio::time::sleep(Duration::from_millis(300 * (attempt + 1))).await;
    }
}
