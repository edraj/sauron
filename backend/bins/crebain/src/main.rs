//! crebain — a load/benchmark generator for the Sauron ingest write path.
//!
//! `crebain --isolated` spins up an isolated, self-cleaning ephemeral stack
//! (database + dedicated ingest) and hammers it; `crebain --dsn <DSN>` points at
//! an already-running edge. Both exercise all five envelope signal types.

mod cli;
mod client;
mod db_url;
mod dsn;
mod engine;
mod generator;
mod harness;
mod metrics;
mod report;
mod user;

use std::future::Future;
use std::process::ExitCode;

use anyhow::Result;
use clap::Parser;

use cli::{Args, Mode, RunConfig};
use dsn::Target;
use metrics::Summary;

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
            finish(run_with_signals(engine::run(&cfg, &target)).await, &cfg)
        }
        Mode::Isolated(icfg) => {
            let (target, mut guard) = harness::setup(&icfg).await?;
            print_target(&target);
            let ran = run_with_signals(engine::run(&cfg, &target)).await;
            // Teardown runs in EVERY path: completion, engine error, and interrupt.
            guard.teardown().await;
            finish(ran, &cfg)
        }
    }
}

/// Turn the outcome of a run into an exit code. `None` means the run was
/// interrupted (Ctrl-C / SIGTERM) before it produced a summary.
fn finish(ran: Option<Result<Summary>>, cfg: &RunConfig) -> Result<ExitCode> {
    match ran {
        Some(Ok(summary)) => {
            report::print_summary(&summary, &cfg.expected());
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
