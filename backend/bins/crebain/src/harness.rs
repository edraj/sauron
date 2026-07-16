//! Isolated mode: create + migrate + seed an ephemeral database, spawn a
//! dedicated ingest against it, and tear it all down afterwards.
//!
//! [`setup`] returns a [`Target`] plus a [`HarnessGuard`]. The caller MUST call
//! `guard.teardown().await` in every exit path (success, error, Ctrl-C). The
//! guard is idempotent and also kills the child ingest on drop
//! (`kill_on_drop`), so a stray process is never orphaned.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

use tokio::process::{Child, Command};

use crate::cli::IsolatedConfig;
use crate::db_url::{bench_db_name, swap_database, swap_redis_db};
use crate::dsn::Target;

/// Owns the ephemeral resources and tears them down once.
pub struct HarnessGuard {
    admin_url: String,
    bench_db: String,
    bench_redis_url: String,
    child: Option<Child>,
    keep: bool,
    torn_down: bool,
}

/// The bench names/URLs, computed up front.
pub struct Prepared {
    pub bench_db: String,
    pub bench_pg_url: String,
    pub bench_redis_url: String,
}

/// Compute names/URLs and construct the (empty) guard — pure, no I/O. The guard
/// is created BEFORE any database exists so the caller can tear down from any
/// interruption point, including a Ctrl-C mid-[`provision`] or even during
/// `CREATE DATABASE` (teardown drops `IF EXISTS`, so it is safe either way).
pub fn prepare(icfg: &IsolatedConfig) -> anyhow::Result<(Prepared, HarnessGuard)> {
    let bench_db = bench_db_name();
    let bench_pg_url = swap_database(&icfg.admin_database_url, &bench_db)?;
    let bench_redis_url = swap_redis_db(&icfg.redis_url, icfg.redis_bench_db)?;
    let guard = HarnessGuard {
        admin_url: icfg.admin_database_url.clone(),
        bench_db: bench_db.clone(),
        bench_redis_url: bench_redis_url.clone(),
        child: None,
        keep: icfg.keep,
        torn_down: false,
    };
    Ok((
        Prepared {
            bench_db,
            bench_pg_url,
            bench_redis_url,
        },
        guard,
    ))
}

/// Create + migrate + seed the database, spawn the ingest, wait for it to be
/// ready. The guard is mutated as resources come up (the child is stored the
/// instant it spawns), so the caller's `teardown` covers a failure at ANY step.
///
/// Cancellation is cooperative, NOT via future-drop: each step runs to
/// completion and the `cancel` flag is checked only *between* steps. This is
/// deliberate — dropping a mid-flight `CREATE DATABASE` future can leave Postgres
/// committing the database *after* teardown's `DROP … IF EXISTS` already ran,
/// leaking it. Running the statement to completion guarantees teardown sees and
/// drops it. Returns `Ok(None)` if cancelled at a checkpoint.
pub async fn provision(
    icfg: &IsolatedConfig,
    prepared: &Prepared,
    guard: &mut HarnessGuard,
    cancel: &tokio::sync::watch::Receiver<bool>,
) -> anyhow::Result<Option<Target>> {
    macro_rules! bail_if_cancelled {
        () => {
            if *cancel.borrow() {
                return Ok(None);
            }
        };
    }

    bail_if_cancelled!();
    eprintln!("crebain: creating bench database {}", prepared.bench_db);
    sauron_db::create_database(&icfg.admin_database_url, &prepared.bench_db).await?;

    bail_if_cancelled!();
    eprintln!("crebain: applying migrations");
    sauron_db::run_pending_migrations(&prepared.bench_pg_url).await?;

    bail_if_cancelled!();
    // Seed org → project → app, minting a public key we control.
    let public_key = format!("pk_crebain_{}", uuid::Uuid::new_v4().simple());
    let app_id = seed(&prepared.bench_pg_url, &public_key).await?;

    bail_if_cancelled!();
    // Isolate + clean the bench Redis index.
    flush_redis(&prepared.bench_redis_url)
        .await
        .map_err(|e| anyhow::anyhow!("flush bench redis {}: {e}", prepared.bench_redis_url))?;

    bail_if_cancelled!();
    // Spawn the dedicated ingest.
    let bin = locate_ingest(&icfg.ingest_bin)?;
    eprintln!(
        "crebain: launching ingest ({}) on port {}",
        bin.display(),
        icfg.ingest_port
    );
    guard.child = Some(spawn_ingest(
        &bin,
        icfg,
        &prepared.bench_pg_url,
        &prepared.bench_redis_url,
    )?);

    // Wait for it to accept traffic (interruptible each poll).
    let base_url = format!("http://127.0.0.1:{}", icfg.ingest_port);
    if !wait_ready(&base_url, guard, cancel).await? {
        return Ok(None);
    }
    eprintln!("crebain: ingest ready; bench app {app_id} seeded");

    Ok(Some(Target {
        base_url,
        app_id: app_id.to_string(),
        public_key,
    }))
}

/// Insert one org → project → app and return the app id.
async fn seed(bench_pg_url: &str, public_key: &str) -> anyhow::Result<uuid::Uuid> {
    let pool = sauron_db::build_pool(bench_pg_url, 2)?;
    let app_id = {
        let mut conn = sauron_db::conn(&pool).await?;
        let org = sauron_db::repo::create_org(&mut conn, "crebain", "crebain").await?;
        let project =
            sauron_db::repo::create_project(&mut conn, org.id, "crebain", "crebain").await?;
        let app = sauron_db::repo::create_app(
            &mut conn, project.id, "crebain", "crebain", "web", public_key,
        )
        .await?;
        app.id
    };
    // Drop the pool so no seed connection lingers to block DROP DATABASE.
    drop(pool);
    Ok(app_id)
}

fn spawn_ingest(
    bin: &PathBuf,
    icfg: &IsolatedConfig,
    bench_pg_url: &str,
    bench_redis_url: &str,
) -> anyhow::Result<Child> {
    Command::new(bin)
        .env("DATABASE_URL", bench_pg_url)
        .env("REDIS_URL", bench_redis_url)
        .env("INGEST_PORT", icfg.ingest_port.to_string())
        .env("INGEST_RATE_LIMIT_PER_MIN", icfg.rate_limit.to_string())
        .env("WORKER_CONCURRENCY", "8")
        .env(
            "RUST_LOG",
            std::env::var("RUST_LOG").unwrap_or_else(|_| "warn".to_string()),
        )
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to spawn ingest {}: {e}", bin.display()))
}

/// Find the sauron-ingest binary: an explicit path, else a sibling of this exe.
fn locate_ingest(explicit: &Option<String>) -> anyhow::Result<PathBuf> {
    if let Some(p) = explicit {
        let pb = PathBuf::from(p);
        if pb.exists() {
            return Ok(pb);
        }
        anyhow::bail!("--ingest-bin {p:?} does not exist");
    }
    let exe = std::env::current_exe()?;
    let dir = exe
        .parent()
        .ok_or_else(|| anyhow::anyhow!("cannot locate crebain's own directory"))?;
    let cand = dir.join("sauron-ingest");
    if cand.exists() {
        return Ok(cand);
    }
    anyhow::bail!(
        "sauron-ingest binary not found at {}. Build it first: cargo build -p sauron-ingest",
        cand.display()
    )
}

/// Poll `/ready` until the ingest accepts traffic. `Ok(true)` = ready,
/// `Ok(false)` = cancelled while waiting, `Err` = child died or 30s timeout.
async fn wait_ready(
    base_url: &str,
    guard: &mut HarnessGuard,
    cancel: &tokio::sync::watch::Receiver<bool>,
) -> anyhow::Result<bool> {
    let url = format!("{base_url}/ready");
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        // The target is our own loopback ingest; never route it via an ambient proxy.
        .no_proxy()
        .build()?;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if *cancel.borrow() {
            return Ok(false);
        }
        // If the ingest died during startup, fail fast rather than poll for 30s.
        if let Some(child) = guard.child.as_mut() {
            if let Ok(Some(status)) = child.try_wait() {
                anyhow::bail!("ingest exited during startup ({status})");
            }
        }
        if let Ok(resp) = http.get(&url).send().await {
            if resp.status().is_success() {
                return Ok(true);
            }
        }
        if Instant::now() >= deadline {
            anyhow::bail!("ingest did not become ready within 30s at {url}");
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn flush_redis(url: &str) -> anyhow::Result<()> {
    let client = redis::Client::open(url)?;
    let mut con = client.get_multiplexed_async_connection().await?;
    redis::cmd("FLUSHDB").query_async::<()>(&mut con).await?;
    Ok(())
}

impl HarnessGuard {
    /// PID of the spawned ingest child, if it is running. Used to sample the
    /// server's CPU/RAM during the run. `None` before spawn or after teardown.
    pub fn child_pid(&self) -> Option<u32> {
        self.child.as_ref().and_then(|c| c.id())
    }

    /// Kill the ingest, drop the database, flush Redis. Idempotent.
    pub async fn teardown(&mut self) {
        if self.torn_down {
            return;
        }
        self.torn_down = true;

        if let Some(mut child) = self.child.take() {
            let _ = child.kill().await;
        }

        if self.keep {
            eprintln!(
                "crebain: --keep set; retaining bench database {} (drop it manually when done)",
                self.bench_db
            );
            return;
        }

        eprintln!("crebain: dropping bench database {}", self.bench_db);
        if let Err(e) = sauron_db::drop_database(&self.admin_url, &self.bench_db).await {
            eprintln!(
                "crebain: WARNING failed to drop bench db {}: {e}\n  drop manually: DROP DATABASE \"{}\";",
                self.bench_db, self.bench_db
            );
        }
        let _ = flush_redis(&self.bench_redis_url).await;
    }
}

impl Drop for HarnessGuard {
    fn drop(&mut self) {
        // `kill_on_drop` handles the child. We can't run async DB work in Drop, so
        // if teardown never ran, make the leftover loud.
        if !self.torn_down && !self.keep {
            eprintln!(
                "crebain: WARNING bench database {} may remain (teardown did not run).\n  drop manually: DROP DATABASE \"{}\";",
                self.bench_db, self.bench_db
            );
        }
    }
}
