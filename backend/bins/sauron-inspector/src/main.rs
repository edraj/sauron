//! `sauron-inspector` — the PII scanner, retro-masker and audit reaper.
//!
//! FOUR independent loops, one 4-connection pool.
//!
//! The single-task shape every other worker in this repo uses does not work
//! here. A project-scoped scan can run for hours, and a scheduler folded into
//! the same loop would not execute for that whole time — so when the worker
//! finally returned, everything queued behind it would be more than
//! `INSPECTOR_CATCHUP_GRACE_HOURS` stale and get SKIPPED. Enabling one large
//! policy would silently disable scheduling for every other policy, with the
//! only signal buried in a column. Likewise, routing previews through the mask
//! FIFO means a preview requested while a multi-hour mask runs expires before
//! it is ever computed, and confirm becomes permanently impossible.
//!
//! ONE pool, not two. Today's peak pooled demand is sauron-api 16 +
//! sauron-ingest 8 + sauron-alerts 8 + sauron-tier 4 + sauron-monitor (50 + 8)
//! = 94, against `postgres:16` with no tuning — the default `max_connections`
//! of 100 with 3 reserved for superusers. A second pool here pushes the
//! shipped deployment over the edge, and connection exhaustion surfaces as API
//! 500s and ingest 202-then-drop, not as an inspector error.

mod mask;
mod preview;
mod reap;
mod scan;

use std::sync::Arc;
use std::time::Duration;

use sauron_core::Config;
use sauron_db::{PgConn, PgPool};
use tracing::{info, warn};

/// Executor cadence. Deliberately much shorter than the scheduler's tick: an
/// executor does ONE unit or ONE batch per iteration and re-enters, so the
/// lease heartbeat is frequent and cancellation is observed quickly.
const EXECUTOR_INTERVAL: Duration = Duration::from_secs(1);
const REAPER_INTERVAL: Duration = Duration::from_secs(3600);

/// Check out a connection AND bound every statement it will run.
///
/// Always paired with [`release`]: deadpool's recycle does not reset session
/// state, so a leaked `SET statement_timeout` silently poisons a later
/// checkout in the same process.
pub async fn checkout(pool: &PgPool, cfg: &Config) -> anyhow::Result<PgConn> {
    let mut conn = sauron_db::conn(pool).await?;
    sauron_db::repo::set_statement_timeout(&mut conn, cfg.inspector_statement_timeout_ms).await?;
    Ok(conn)
}

/// Reset the session setting, then drop. Never hold a pooled connection across
/// the inter-batch sleep — the pool is 4 for the whole process.
pub async fn release(mut conn: PgConn) {
    if let Err(e) = sauron_db::repo::reset_statement_timeout(&mut conn).await {
        // A failed RESET means this connection is poisoned for whoever gets it
        // next, and the failure mode (a 30s timeout on an unrelated query) is
        // untraceable, so say so loudly rather than dropping silently.
        warn!(error = %e, "could not reset statement_timeout; connection returned poisoned");
    }
    drop(conn);
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    sauron_telemetry::init("sauron-inspector");
    let cfg = Arc::new(Config::from_env()?);

    if !cfg.inspector_enabled {
        info!("INSPECTOR_ENABLED is false; sauron-inspector is idle");
        // Sleep forever rather than exit: systemd's Restart=on-failure would
        // not restart a clean exit, but an operator flipping the flag expects
        // `systemctl restart` to be the whole procedure, and a unit in
        // `inactive (dead)` looks like a crash in `systemctl status`.
        loop {
            tokio::time::sleep(Duration::from_secs(3600)).await;
        }
    }

    let pool = sauron_db::build_pool(&cfg.database_url, 4)?;
    // Distinct per process AND per restart: the worker-id fence on every flush
    // exists so a worker whose lease expired cannot double-count after coming
    // back, and reusing a stable id across restarts would defeat it.
    let worker_id = format!(
        "inspector-{}-{}",
        std::process::id(),
        sauron_core::ids::random_hex(4)
    );
    info!(
        worker_id,
        tick_secs = cfg.inspector_tick_secs,
        tail_sweep_secs = cfg.inspector_tail_sweep_secs,
        policy_cache_secs = cfg.inspector_policy_cache_secs,
        "sauron-inspector started"
    );

    let scheduler = spawn_loop(
        "scheduler",
        Duration::from_secs(cfg.inspector_tick_secs),
        pool.clone(),
        cfg.clone(),
        worker_id.clone(),
        |p, c, w| Box::pin(async move { schedule_tick(&p, &c, &w).await }),
    );
    let scans = spawn_loop(
        "scan",
        EXECUTOR_INTERVAL,
        pool.clone(),
        cfg.clone(),
        worker_id.clone(),
        |p, c, w| Box::pin(async move { scan::tick(&p, &c, &w).await }),
    );
    let masks = spawn_loop(
        "mask",
        EXECUTOR_INTERVAL,
        pool.clone(),
        cfg.clone(),
        worker_id.clone(),
        |p, c, w| Box::pin(async move { mask::tick(&p, &c, &w).await }),
    );
    let previews = spawn_loop(
        "preview",
        EXECUTOR_INTERVAL,
        pool.clone(),
        cfg.clone(),
        worker_id.clone(),
        |p, c, w| Box::pin(async move { preview::tick(&p, &c, &w).await }),
    );
    let reaper = spawn_loop(
        "reap",
        REAPER_INTERVAL,
        pool.clone(),
        cfg.clone(),
        worker_id.clone(),
        |p, c, w| Box::pin(async move { reap::tick(&p, &c, &w).await }),
    );

    let _ = tokio::join!(scheduler, scans, masks, previews, reaper);
    Ok(())
}

type TickFuture = std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<bool>> + Send>>;

/// Spawn one supervised loop. Errors are logged and swallowed: a loop that
/// returns is a loop that silently stops doing its job, and there is no
/// graceful shutdown anywhere in this product to distinguish that from a
/// deliberate stop.
fn spawn_loop<F>(
    name: &'static str,
    interval: Duration,
    pool: PgPool,
    cfg: Arc<Config>,
    worker_id: String,
    f: F,
) -> tokio::task::JoinHandle<()>
where
    F: Fn(PgPool, Arc<Config>, String) -> TickFuture + Send + 'static,
{
    tokio::spawn(async move {
        loop {
            match f(pool.clone(), cfg.clone(), worker_id.clone()).await {
                // Work was done, so come straight back: a backlog must drain at
                // the batch pause, not at the loop interval.
                Ok(true) => tokio::time::sleep(Duration::from_millis(10)).await,
                Ok(false) => tokio::time::sleep(interval).await,
                Err(e) => {
                    warn!(loop_name = name, error = %e, "inspector loop tick failed");
                    tokio::time::sleep(interval).await;
                }
            }
        }
    })
}

/// Claim due policies and enqueue a scan for each. NEVER blocked by execution.
///
/// Catch-up fires ONCE on recovery and never replays missed runs: a scan is a
/// snapshot over a window, not an event stream, so three replayed runs produce
/// three near-identical finding sets at 3x the load. And a 03:00 scan firing
/// at 09:00 on a Monday is precisely the production load spike the schedule
/// existed to avoid — so a run more than `INSPECTOR_CATCHUP_GRACE_HOURS` stale
/// is skipped with the reason recorded in `last_skip_reason`.
async fn schedule_tick(pool: &PgPool, cfg: &Config, _worker_id: &str) -> anyhow::Result<bool> {
    let mut conn = checkout(pool, cfg).await?;
    let due = sauron_db::repo::claim_due_policies(&mut conn, 50).await?;
    release(conn).await;
    if due.is_empty() {
        return Ok(false);
    }
    let mut started = 0usize;
    for policy in due {
        let mut conn = checkout(pool, cfg).await?;
        let stale_hours = policy
            .last_run_at
            .map(|t| (chrono::Utc::now() - t).num_hours())
            .unwrap_or(0);
        if stale_hours > cfg.inspector_catchup_grace_hours {
            // Recorded through a dedicated statement so the reason string is
            // not a lifetime puzzle inside `InspectorPolicyPatch`, whose
            // borrowed fields would force this `format!` to outlive the call.
            let _ = sauron_db::repo::record_policy_skip(
                &mut conn,
                policy.id,
                &format!("catch-up skipped: {stale_hours}h stale"),
            )
            .await;
            release(conn).await;
            continue;
        }
        match scan::enqueue_for_policy(&mut conn, cfg, &policy, "scheduled", None).await {
            Ok(true) => started += 1,
            Ok(false) => {}
            Err(e) => warn!(policy_id = %policy.id, error = %e, "could not enqueue scheduled scan"),
        }
        release(conn).await;
    }
    info!(started, "scheduler tick");
    Ok(started > 0)
}
