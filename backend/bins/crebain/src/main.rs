//! crebain — a load/benchmark generator for the Sauron ingest write path.
//!
//! `crebain --isolated` spins up an isolated, self-cleaning ephemeral stack
//! (database + dedicated ingest) and hammers it; `crebain --dsn <DSN>` points at
//! an already-running edge. Both exercise all five envelope signal types.

mod cli;
mod db_url;
mod dsn;
mod engine;
mod generator;
mod harness;
mod metrics;
mod netlimit;
mod procstat;
mod report;
mod report_html;
mod schedule;
mod transport;
mod user;

use std::future::Future;
use std::process::ExitCode;

use anyhow::Result;
use clap::Parser;

use cli::{Args, Mode, RunConfig};
use dsn::Target;
use metrics::Summary;
use report_html::ReportMeta;

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("crebain: error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<ExitCode> {
    let (cfg, mode) = Args::parse().resolve()?;
    print_banner(&cfg, &mode);

    match mode {
        Mode::Direct { dsn } => {
            let target = dsn::parse_dsn(&dsn)?;
            print_target(&target);
            let plan = if matches!(cfg.transport, cli::Transport::Uds) {
                // UDS has no ephemeral-port wall to fan out across; concurrency
                // is fd-bound, not port-bound, so just hand back the requested
                // ceiling as-is.
                netlimit::FanoutPlan {
                    source_ips: Vec::new(),
                    effective: cfg.max_inflight,
                    warning: None,
                }
            } else {
                build_plan(&cfg, netlimit::is_loopback_host(host_of(&target)))
            };
            let fd = netlimit::raise_nofile(plan.effective as u64 + 1024);
            print_concurrency(cfg.max_inflight, &plan, &fd);
            if let Some(w) = &plan.warning {
                eprintln!("crebain: WARNING {w}");
            }
            finish(
                run_with_signals(engine::run(&cfg, &target, None, &plan)).await,
                &cfg,
                "direct",
            )
        }
        Mode::Isolated(icfg) => run_isolated(&cfg, &icfg).await,
    }
}

/// Isolated mode: provision an ephemeral stack, run the load, tear it all down.
///
/// A background task turns Ctrl-C/SIGTERM into a `cancel` flag installed BEFORE
/// any I/O, so provisioning is never hit by the default-SIGINT process kill.
/// Provisioning is cancelled only *between* steps (never mid-`CREATE DATABASE`),
/// and `teardown` runs on every path — completion, error, or interrupt — so the
/// bench database is always dropped.
async fn run_isolated(cfg: &RunConfig, icfg: &cli::IsolatedConfig) -> Result<ExitCode> {
    let (prepared, mut guard) = harness::prepare(icfg)?;

    // Isolated targets are always loopback (127.0.0.1): compute the fan-out plan
    // and raise the fd limit BEFORE `provision` spawns the ingest child, so the
    // child inherits the raised limit.
    let plan = if matches!(cfg.transport, cli::Transport::Uds) {
        // UDS has no ephemeral-port wall to fan out across; concurrency is
        // fd-bound, not port-bound, so just hand back the requested ceiling.
        netlimit::FanoutPlan {
            source_ips: Vec::new(),
            effective: cfg.max_inflight,
            warning: None,
        }
    } else {
        build_plan(cfg, true)
    };
    let fd = netlimit::raise_nofile(plan.effective as u64 + 1024);
    print_concurrency(cfg.max_inflight, &plan, &fd);
    if let Some(w) = &plan.warning {
        eprintln!("crebain: WARNING {w}");
    }

    let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    let watcher = tokio::spawn(async move {
        shutdown_signal().await;
        let _ = cancel_tx.send(true);
    });

    let ran = isolated_body(cfg, icfg, &prepared, &mut guard, cancel_rx, &plan).await;

    watcher.abort();
    guard.teardown().await;
    finish(ran, cfg, "isolated (ephemeral, self-cleaning)")
}

async fn isolated_body(
    cfg: &RunConfig,
    icfg: &cli::IsolatedConfig,
    prepared: &harness::Prepared,
    guard: &mut harness::HarnessGuard,
    mut cancel: tokio::sync::watch::Receiver<bool>,
    plan: &netlimit::FanoutPlan,
) -> Option<Result<Summary>> {
    match harness::provision(icfg, prepared, guard, &cancel).await {
        Err(e) => Some(Err(e)),
        Ok(None) => None, // interrupted during provisioning
        Ok(Some(target)) => {
            print_target(&target);
            let target_pid = guard.child_pid();
            // engine::run is safe to cancel mid-flight (it just aborts user tasks).
            tokio::select! {
                r = engine::run(cfg, &target, target_pid, plan) => Some(r),
                _ = cancel.changed() => None,
            }
        }
    }
}

/// Turn the outcome of a run into an exit code. `None` means the run was
/// interrupted (Ctrl-C / SIGTERM) before it produced a summary.
fn finish(ran: Option<Result<Summary>>, cfg: &RunConfig, mode_label: &str) -> Result<ExitCode> {
    match ran {
        Some(Ok(summary)) => {
            report::print_summary(&summary, &cfg.expected());
            if cfg.live_sockets {
                eprintln!("crebain: --live-sockets: this run is a connection-capacity demo — read PEAK CONNECTIONS, not req/s.");
            }
            if let Some(path) = &cfg.report_path {
                let meta = ReportMeta {
                    mode_label: mode_label.to_string(),
                    users: cfg.users,
                    duration_secs: cfg.duration.as_secs(),
                    events_per_min: cfg.events_per_min,
                    issues_per_min: cfg.issues_per_min,
                    gzip: cfg.gzip,
                    generated_at: chrono::Utc::now()
                        .format("%Y-%m-%d %H:%M:%S UTC")
                        .to_string(),
                    ncpus: std::thread::available_parallelism()
                        .map(|n| n.get())
                        .unwrap_or(1),
                };
                match report_html::write(path, &summary, &cfg.expected(), &meta) {
                    Ok(()) => eprintln!("crebain: wrote report to {}", path.display()),
                    Err(e) => eprintln!("crebain: WARNING failed to write report: {e:#}"),
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Some(Err(e)) => Err(e),
        None => {
            eprintln!("crebain: interrupted");
            Ok(ExitCode::from(130))
        }
    }
}

/// Race the load against Ctrl-C / SIGTERM so an interrupt still returns control
/// to the caller (which then runs teardown). `None` on interrupt.
async fn run_with_signals(fut: impl Future<Output = Result<Summary>>) -> Option<Result<Summary>> {
    tokio::select! {
        r = fut => Some(r),
        _ = shutdown_signal() => None,
    }
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

fn print_banner(cfg: &RunConfig, mode: &Mode) {
    let mode_label = match mode {
        Mode::Direct { .. } => "direct",
        Mode::Isolated(_) => "isolated (ephemeral, self-cleaning)",
    };
    eprintln!("crebain — Sauron load generator");
    eprintln!("  mode         {mode_label}");
    eprintln!("  users        {}", cfg.users);
    eprintln!("  duration     {}s", cfg.duration.as_secs());
    eprintln!("  events/user  {}/min", cfg.events_per_min);
    eprintln!("  issues/user  {}/min", cfg.issues_per_min);
    eprintln!("  gzip         {}", if cfg.gzip { "on" } else { "off" });
}

fn print_target(t: &Target) {
    eprintln!("  target       {}", t.dsn());
    eprintln!("  endpoint     {}", t.envelope_url());
    eprintln!();
}

/// Build the fan-out plan for a run: how many loopback source IPs to bind and
/// the concurrency ceiling the ephemeral-port budget actually allows.
fn build_plan(cfg: &RunConfig, loopback: bool) -> netlimit::FanoutPlan {
    netlimit::plan_fanout(
        cfg.max_inflight,
        loopback,
        netlimit::ephemeral_port_budget(),
        512,
        cfg.source_ips,
    )
}

/// Extract the bare host from a `scheme://host:port` base URL (no port, no scheme).
fn host_of(t: &Target) -> &str {
    t.base_url
        .split("://")
        .nth(1)
        .and_then(|s| s.split(':').next())
        .unwrap_or("")
}

/// Print the resolved concurrency plan and fd-limit status, matching the
/// banner's style.
fn print_concurrency(requested: usize, plan: &netlimit::FanoutPlan, fd: &netlimit::NofileStatus) {
    eprintln!(
        "  concurrency  requested {}  effective {}",
        requested, plan.effective
    );
    eprintln!("  source IPs   {}", plan.source_ips.len());
    eprintln!(
        "  fd limit     soft {} / hard {}{}",
        fd.soft,
        fd.hard,
        if fd.capped {
            "  (CAPPED — raise the hard limit via ulimit/limits.conf for higher concurrency)"
        } else {
            ""
        }
    );
}
