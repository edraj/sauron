//! Unattended history backfill for the rollup-served dashboard pages.
//!
//! Every analytics page has a rollup fast path gated on a per-app marker that,
//! until now, only an operator running `sauron-migrate backfill-*` by hand
//! could write. No packaging step ran it, so an upgraded deployment stayed on
//! the legacy O(history) queries indefinitely: measured on 30M rows with stock
//! Postgres settings, the Users page's three calls each hit the 60 s request
//! timeout, device-groups too, and screens/journeys took 26–42 s. With the
//! four backfills applied the same calls answer in 5–170 ms.
//!
//! This task runs them here, in the process that already owns the rollup
//! fold, whenever any gate is still closed:
//!
//! * once at startup, and again on each daily maintenance pass while anything
//!   is pending (a failed attempt gets retried, not forgotten);
//! * under a session-level advisory lock, so N ingest replicas run one
//!   backfill, not N — the per-day fold transactions additionally serialize
//!   against the live fold on the module-wide xact lock;
//! * in an order that opens the cheapest gates first: the two environment
//!   rollups (seconds — they clear the Users and Devices pages), then
//!   person-days (retention), then the day-by-day event rollup (minutes to an
//!   hour on a long history), which is resumable: an interruption resumes from
//!   the first unfinished day on the next pass.
//!
//! `ROLLUP_AUTO_BACKFILL=0` disables it for operators who prefer the manual
//! runbook; the `sauron-migrate backfill-*` commands remain and are safe to
//! combine with this (markers and the progress cursor make either side a no-op
//! once the other has finished).

use tokio::task::JoinHandle;
use tracing::{info, warn};

use sauron_db::rollups::fold::{self, BackfillOutcome};
use sauron_db::{device_env_backfill, person_days_backfill, person_env_backfill, rollups, PgPool};

/// Whether any of the four gates still has an app without its marker.
pub async fn any_pending(conn: &mut sauron_db::PgConn) -> diesel::QueryResult<bool> {
    Ok(person_env_backfill::backfill_pending(conn).await?
        || device_env_backfill::backfill_pending(conn).await?
        || person_days_backfill::backfill_pending(conn).await?
        || rollups::backfill_pending(conn).await?)
}

/// Spawn one backfill pass; returns immediately. The pass itself logs and
/// returns on any error — the next maintenance pass retries.
pub fn spawn(pool: PgPool, name_cap: usize) -> JoinHandle<()> {
    tokio::spawn(async move {
        run(&pool, name_cap).await;
    })
}

async fn run(pool: &PgPool, name_cap: usize) {
    let mut conn = match sauron_db::conn(pool).await {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "history backfill: no database connection");
            return;
        }
    };
    match any_pending(&mut conn).await {
        Ok(true) => {}
        Ok(false) => return,
        Err(e) => {
            warn!(error = %e, "history backfill: pending probe failed");
            return;
        }
    }
    let got = match rollups::try_backfill_run_lock(&mut conn).await {
        Ok(g) => g,
        Err(e) => {
            warn!(error = %e, "history backfill: lock probe failed");
            return;
        }
    };
    if !got {
        info!("history backfill: another runner holds the lock; leaving it to them");
        return;
    }
    info!("history backfill starting (unattended; see ROLLUP_AUTO_BACKFILL)");
    let started = std::time::Instant::now();
    let outcome = run_locked(pool, &mut conn, name_cap).await;
    if let Err(e) = rollups::release_backfill_run_lock(&mut conn).await {
        warn!(error = %e, "history backfill: unlock failed (the session drop releases it)");
    }
    match outcome {
        Ok(()) => info!(
            elapsed_s = started.elapsed().as_secs(),
            "history backfill complete"
        ),
        Err(e) => {
            warn!(error = %e, elapsed_s = started.elapsed().as_secs(), "history backfill stopped; will retry on the next maintenance pass")
        }
    }
}

async fn run_locked(
    pool: &PgPool,
    conn: &mut sauron_db::PgConn,
    name_cap: usize,
) -> anyhow::Result<()> {
    if person_env_backfill::backfill_pending(conn).await? {
        person_env_backfill::backfill_all(pool).await?;
    }
    if device_env_backfill::backfill_pending(conn).await? {
        device_env_backfill::backfill_all(pool).await?;
    }
    if person_days_backfill::backfill_pending(conn).await? {
        person_days_backfill::backfill_all(pool).await?;
    }
    if rollups::backfill_pending(conn).await? {
        let outcome = fold::backfill_all_resumable(conn, name_cap, |day| {
            info!(%day, "history backfill: day complete");
            true
        })
        .await?;
        if outcome == BackfillOutcome::Interrupted {
            anyhow::bail!("rollup backfill interrupted");
        }
    }
    if let Err(e) = rollups::analyze_rollup_tables(conn).await {
        warn!(error = %e, "history backfill: ANALYZE of the rollup tables failed");
    }
    Ok(())
}
