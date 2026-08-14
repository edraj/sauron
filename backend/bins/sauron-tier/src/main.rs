//! `sauron-tier` — moves aged partitions from Postgres to Parquet.
//!
//! Each cycle, per tiered table: pre-create upcoming partitions, export aged
//! partitions to Parquet (copy → verify counts → advance watermark), then drop
//! partitions that are below the watermark AND older than the drop lag. Nothing
//! is ever deleted: a partition is dropped only after its rows are verified in
//! Parquet, which is the permanent copy.

mod purge;

use std::time::Duration;

use chrono::{DateTime, Utc};
use tracing::{info, warn};

use sauron_core::Config;
use sauron_db::{conn, repo, PgPool};
use sauron_tier::duck::DuckEngine;
use sauron_tier::{
    bucket_bounds, cold_copy_dir, cold_partition_glob, partition_suffix, Granularity, TieredTable,
    TIERED_TABLES,
};
use uuid::Uuid;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    sauron_telemetry::init("sauron-tier");
    let cfg = Config::from_env()?;
    let pool = sauron_db::build_pool(&cfg.database_url, 4)?;
    let gran = Granularity::from_str_or(&cfg.tier_granularity, Granularity::Day);
    info!(hot_days = cfg.tier_hot_days, granularity = ?gran, "sauron-tier started");

    // Two independent loops, not one. Tiering runs hourly by default; a restore
    // is a human waiting on a button and needs a seconds-scale poll. Folding the
    // restore check into the tier cycle would make "restore" mean "some time in
    // the next hour", which is not a feature anyone would use.
    let restore = {
        let pool = pool.clone();
        let cfg = cfg.clone();
        tokio::spawn(async move { restore_loop(pool, cfg).await })
    };

    // The admin data purge. Here rather than in `sauron-inspector` (where the
    // mask job it is modelled on lives) because its recompute phase MUST read
    // cold Parquet, and that binary is explicitly built not to link DuckDB.
    // This process already links it and already owns the watermark the purge
    // derives its boundary from.
    let purging = {
        let pool = pool.clone();
        let cfg = cfg.clone();
        tokio::spawn(async move { purge::purge_loop(pool, cfg).await })
    };

    let tiering = tokio::spawn(async move {
        loop {
            if let Err(e) = cycle(&pool, &cfg, gran).await {
                warn!(error = %e, "tier cycle failed; backing off");
            }
            tokio::time::sleep(Duration::from_secs(cfg.tier_tick_secs)).await;
        }
    });

    // No loop returns. If any task dies the process should too, rather than
    // silently continuing with part of its job undone.
    tokio::select! {
        r = restore => warn!(?r, "restore loop exited"),
        r = tiering => warn!(?r, "tiering loop exited"),
        r = purging => warn!(?r, "purge loop exited"),
    }
    Ok(())
}

async fn cycle(pool: &PgPool, cfg: &Config, gran: Granularity) -> anyhow::Result<()> {
    // Resolve the rotation age once per cycle, not once per process. The value is
    // operator-tunable at runtime (`runtime_settings['tier.hot_days']`), so a
    // process-start read would mean a change only took effect on restart — and
    // this worker is the component that actually moves data, so its reading of
    // the setting IS the deployment's hot/cold boundary.
    //
    // Once per cycle rather than once per table so every table in a single cycle
    // uses the same cutoff. Re-reading per table would let a mid-cycle edit tier
    // `error_events` at 30 days and `analytics_events` at 7 in the same pass.
    let mut c = conn(pool).await?;
    let hot_days = repo::effective_tier_hot_days(&mut c, cfg.tier_hot_days).await?;

    // Warn BEFORE the data goes, not after. A restore that simply vanishes is
    // the same silent-disappearance failure the pin exists to prevent, just
    // deferred to the expiry date — so the operator gets a window in which the
    // pin is visibly about to lapse and can be extended.
    match repo::pins_expiring_before(&mut c, Utc::now() + chrono::Duration::days(PIN_WARN_DAYS))
        .await
    {
        Ok(soon) => {
            for pin in soon {
                warn!(
                    pin = %pin.id,
                    table = %pin.table_name,
                    expires_at = %pin.expires_at,
                    range_start = %pin.range_start,
                    range_end = %pin.range_end,
                    "restored data expires soon; it will be deleted from Postgres (the Parquet copy is untouched)"
                );
            }
        }
        Err(e) => warn!(error = %e, "checking for expiring pins failed"),
    }

    // Expiry DELETES the restored rows, then the pin, as one statement each.
    // This is not housekeeping: restored rows live in `<table>_default`, which
    // the drop step never touches, so failing to delete them here would leak
    // storage AND double-count every chart against the Parquet copy.
    match repo::expire_tier_pins(&mut c).await {
        Ok(expired) => {
            for e in expired {
                info!(
                    pin = %e.id,
                    table = %e.table_name,
                    rows = e.rows_deleted,
                    "pin expired; removed restored rows (still durable in Parquet)"
                );
            }
        }
        Err(e) => warn!(error = %e, "expiring tier pins failed"),
    }
    drop(c);
    if hot_days != cfg.tier_hot_days {
        info!(
            configured = cfg.tier_hot_days,
            effective = hot_days,
            "rotation age overridden by runtime setting"
        );
    }

    for t in TIERED_TABLES {
        if let Err(e) = tier_table(pool, cfg, gran, t, hot_days).await {
            warn!(table = t.name, error = %e, "tiering table failed");
        }
    }
    Ok(())
}

async fn tier_table(
    pool: &PgPool,
    cfg: &Config,
    gran: Granularity,
    t: &TieredTable,
    hot_days: i64,
) -> anyhow::Result<()> {
    let now = Utc::now();
    let mut c = conn(pool).await?;

    // Snapshot the watermark BEFORE this cycle's exports advance it. Step 4 gates
    // the drop on THIS value, so a partition exported in this cycle is not dropped
    // until a LATER cycle — a real grace window (>= one tick) during which the
    // partition is durable in BOTH tiers. This closes the cross-tier read race
    // where a reader holding a slightly stale watermark would otherwise miss rows
    // in a just-exported-and-dropped partition.
    let wm_at_cycle_start = repo::get_watermark(&mut c, t.name).await?;

    // 1. Pre-create partitions for now .. now + partition_ahead buckets.
    let mut b = bucket_bounds(now, gran);
    for _ in 0..cfg.tier_partition_ahead {
        repo::create_range_partition(&mut c, t.name, &partition_suffix(b.start), b.start, b.end)
            .await?;
        b = bucket_bounds(b.end, gran);
    }

    // 2. Eligibility cutoff: partitions whose END <= (now - hot_days) may tier.
    let cutoff = now - chrono::Duration::days(hot_days);
    let cold_dir = cold_copy_dir(&cfg.tier_cold_path, t.name);
    let base_glob = format!("{}/**/*.parquet", cold_dir);

    // 3. Export eligible partitions oldest-first; stop on the first failure so
    //    the watermark never skips a gap.
    let children = repo::list_child_partitions(&mut c, t.name).await?;
    for child in children {
        let Some(start) = parse_suffix_start(&child, t.name) else {
            continue;
        };
        let range = bucket_bounds(start, gran);
        if range.end > cutoff {
            continue; // still hot
        }
        let wm = repo::get_watermark(&mut c, t.name).await?;
        if let Some(w) = wm {
            if range.start < w {
                continue; // already exported
            }
        }
        let pg_rows = repo::count_child_rows(&mut c, &child).await?;

        let pg_url = cfg.database_url.clone();
        let table = t.name.to_string();
        let cold_dir_c = cold_dir.clone();
        let base_glob_c = base_glob.clone();
        let (rs, re) = (range.start, range.end);
        let pg_rows_c = pg_rows;
        // Idempotency pre-check: only export when cold has NOTHING for this range.
        // `APPEND` is not idempotent, so re-exporting a range that already has data
        // would duplicate rows. `already`: rows already in cold for [rs, re).
        //   already == pg_rows  → already exported (a prior watermark-advance didn't
        //                         stick); skip export, just advance.
        //   already == 0        → fresh export, then verify.
        //   0 < already != pg   → partial/corrupt cold data; do NOT append more.
        let (already, exported_cold) =
            tokio::task::spawn_blocking(move || -> anyhow::Result<(i64, Option<i64>)> {
                let eng = DuckEngine::open()?;
                let already = eng.count_range(&base_glob_c, rs, re)?;
                if already != 0 || pg_rows_c == 0 {
                    // Already present, partial, or nothing to export — decided by caller.
                    return Ok((already, None));
                }
                eng.export_from_postgres(&pg_url, &table, rs, re, &cold_dir_c)?;
                let cold = eng.count_range(&base_glob_c, rs, re)?;
                Ok((already, Some(cold)))
            })
            .await??;

        match exported_cold {
            Some(cold_rows) => {
                if cold_rows != pg_rows {
                    warn!(child = %child, pg_rows, cold_rows, "count mismatch after export; leaving partition for retry");
                    break;
                }
                repo::advance_watermark(&mut c, t.name, range.end).await?;
                info!(child = %child, rows = pg_rows, "exported partition to Parquet");
            }
            None if already == pg_rows => {
                // Rows already durable in cold from a prior attempt — idempotent advance.
                repo::advance_watermark(&mut c, t.name, range.end).await?;
                info!(child = %child, rows = pg_rows, "partition already in cold; advanced watermark");
            }
            None => {
                warn!(child = %child, pg_rows, already, "partial cold data for range; skipping re-export (manual clear needed)");
                break;
            }
        }
    }

    // 4. Drop partitions at/below the PRE-CYCLE watermark AND past the drop lag.
    //    Using wm_at_cycle_start (not a fresh read) guarantees a partition exported
    //    THIS cycle waits until a later cycle to be dropped (the grace window).
    if let Some(w) = wm_at_cycle_start {
        let lag = chrono::Duration::hours(cfg.tier_drop_lag_hours);
        for child in repo::list_child_partitions(&mut c, t.name).await? {
            let Some(start) = parse_suffix_start(&child, t.name) else {
                continue;
            };
            let range = bucket_bounds(start, gran);
            if range.end <= w && (now - range.end) >= lag {
                // A restored range is pinned. Without this check the restore is
                // undone on the very next cycle: the rows are back in Postgres but
                // also still in Parquet, so `pg_now == cold_now` and the
                // late-write guard below does NOT fire — it only retains a
                // partition that GREW. Checked before the row counts because it is
                // one indexed query against a tiny table, versus a COUNT(*) on the
                // partition plus a DuckDB scan of the cold copy.
                if repo::is_range_pinned(&mut c, t.name, range.start, range.end).await? {
                    info!(child = %child, "partition pinned (restored data); not dropping");
                    continue;
                }
                // Late-write safety: a client-supplied occurred_at can route a NEW
                // row into this already-exported-but-not-yet-dropped partition (the
                // grace window). Such a row is NOT in Parquet, so dropping would lose
                // it. Re-count the partition against its cold copy; if it grew, retain
                // the partition instead of deleting un-exported data ("never delete").
                let pg_now = repo::count_child_rows(&mut c, &child).await?;
                let (rs, re) = (range.start, range.end);
                let base_glob_c = base_glob.clone();
                let cold_now = tokio::task::spawn_blocking(move || -> anyhow::Result<i64> {
                    let eng = DuckEngine::open()?;
                    eng.count_range(&base_glob_c, rs, re)
                })
                .await??;
                if pg_now > cold_now {
                    warn!(child = %child, pg_now, cold_now, "partition grew after export (late arrivals); retaining to avoid data loss");
                    continue;
                }
                repo::detach_and_drop_partition(&mut c, t.name, &child).await?;
                repo::set_dropped_thru(&mut c, t.name, range.end).await?;
                info!(child = %child, "dropped Postgres partition (now cold-only)");
            }
        }
    }
    Ok(())
}

// ===========================================================================
// Restore executor (Parquet -> Postgres)
// ===========================================================================

/// How far ahead of expiry a pin starts warning. Paired with the 30-day default
/// pin the dashboard offers, this gives a week's notice.
const PIN_WARN_DAYS: i64 = 7;

/// Claims past which a job is declared poison and failed. A restore that has
/// crashed three times will crash a fourth; looping forever would keep
/// re-deleting and re-inserting the same rows.
const RESTORE_MAX_ATTEMPTS: i32 = 3;

async fn restore_loop(pool: PgPool, cfg: Config) {
    // Distinct per process. The claim's "this worker's own running job" arm
    // keys on it, so two workers sharing an id would each think the other's job
    // was theirs to resume.
    let worker_id = format!("sauron-tier-{}-{}", std::process::id(), Uuid::new_v4());
    info!(worker = %worker_id, poll_secs = cfg.restore_poll_secs, "restore executor started");
    loop {
        match run_one_restore(&pool, &cfg, &worker_id).await {
            // Did work — look again immediately rather than sleeping, so a
            // queue of restores drains back to back.
            Ok(true) => continue,
            Ok(false) => {}
            Err(e) => warn!(error = %e, "restore job failed"),
        }
        tokio::time::sleep(Duration::from_secs(cfg.restore_poll_secs)).await;
    }
}

/// Claim and run at most one restore. Returns whether a job was claimed.
async fn run_one_restore(pool: &PgPool, cfg: &Config, worker_id: &str) -> anyhow::Result<bool> {
    let mut c = conn(pool).await?;
    let Some(job) = repo::claim_one_restore_job(&mut c, worker_id, cfg.restore_lease_secs).await?
    else {
        return Ok(false);
    };

    // Both of these are already enforced by the `restore_jobs.table_name` CHECK,
    // but the value is interpolated into SQL downstream and a defence that only
    // exists in the database is one schema edit away from being gone.
    if !repo::is_restorable_table(&job.table_name) {
        repo::finish_restore_job(
            &mut c,
            job.id,
            worker_id,
            "failed",
            0,
            &format!("table {} is not restorable", job.table_name),
        )
        .await?;
        return Ok(true);
    }
    if job.attempts > RESTORE_MAX_ATTEMPTS {
        repo::finish_restore_job(
            &mut c,
            job.id,
            worker_id,
            "failed",
            job.rows_restored,
            &format!("gave up after {} attempts", job.attempts),
        )
        .await?;
        return Ok(true);
    }

    // The pin is created BEFORE a single row is written, and recorded on the job
    // in the same breath. Ordering matters: a crash after the pin but before the
    // rows leaves an empty pin that expires harmlessly, whereas rows written
    // before their pin existed would carry a NULL marker and become
    // indistinguishable from genuine late arrivals — unreclaimable, and
    // double-counted forever.
    let pin_id = match job.pin_id {
        Some(existing) => {
            // Resume path. Delete whatever the crashed attempt managed to
            // insert; this is exactly what makes a retry idempotent, and it is
            // safe because the marker can only match rows this job wrote.
            let removed = repo::delete_restored_rows(
                &mut c,
                &job.table_name,
                existing,
                job.range_start,
                job.range_end,
            )
            .await?;
            if removed > 0 {
                info!(job = %job.id, rows = removed, "resuming restore; discarded partial output");
            }
            existing
        }
        None => {
            let pin = repo::create_tier_pin(
                &mut c,
                &job.table_name,
                job.range_start,
                job.range_end,
                job.pin_expires_at,
                job.requested_by,
                Some("cold restore"),
            )
            .await?;
            repo::set_restore_job_pin(&mut c, job.id, pin.id).await?;
            pin.id
        }
    };

    // One app's cold data is a much smaller glob than every app's, because the
    // Parquet is hive-partitioned by app_id.
    let cold_dir = cold_copy_dir(&cfg.tier_cold_path, &job.table_name);
    let glob = match job.app_id {
        Some(a) => cold_partition_glob(&cfg.tier_cold_path, &job.table_name, a),
        None => format!("{cold_dir}/**/*.parquet"),
    };

    let pg_url = cfg.database_url.clone();
    let table = job.table_name.clone();
    let (rs, re, app) = (job.range_start, job.range_end, job.app_id);
    let glob_c = glob.clone();

    // Estimate first so the UI has a denominator while the insert runs.
    let estimate = {
        let glob_e = glob.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<i64> {
            DuckEngine::open()?.count_restorable(&glob_e, app, rs, re)
        })
        .await??
    };
    repo::set_restore_job_estimate(&mut c, job.id, estimate).await?;
    if estimate == 0 {
        // Nothing in cold for this range. Succeed with zero rather than fail:
        // "there was nothing there" is a legitimate answer to a restore request,
        // and the empty pin expires on its own.
        repo::finish_restore_job(&mut c, job.id, worker_id, "succeeded", 0, "").await?;
        info!(job = %job.id, "restore found no cold rows for range");
        return Ok(true);
    }
    info!(job = %job.id, table = %table, rows = estimate, "restoring cold rows into Postgres");

    // DuckDB is synchronous and this is the long part. The insert is ONE
    // statement, so there is no mid-flight progress to report — the heartbeat
    // below is what keeps another worker from stealing the lease meanwhile.
    let inserted = tokio::task::spawn_blocking(move || -> anyhow::Result<i64> {
        DuckEngine::open()?.restore_to_postgres(&pg_url, &table, &glob_c, app, rs, re, pin_id)
    })
    .await?;

    match inserted {
        Ok(n) => {
            // Heartbeat FIRST, before the repair — not after. The insert
            // above was already the "no mid-flight progress" case this
            // heartbeat exists for; the repair is a second, potentially heavy
            // join-UPDATE over the same range, so leaving the heartbeat after
            // it would extend the un-heartbeated window to insert+repair
            // against a single `restore_lease_secs` lease, letting another
            // worker claim it as lapsed and re-enter the resume path while
            // this repair is still running.
            repo::beat_restore_job(&mut c, job.id, worker_id, n).await?;

            // Repair BEFORE marking the job finished, not after. A CRASH here
            // leaves the job `running`; its lease lapses and the resume path
            // above (`Some(existing) => ...`) deletes every row this pin id
            // wrote — repaired or not — before re-inserting, so a crash
            // mid-repair can never leave a partially-repaired range behind.
            // Once the job is marked `succeeded` it is never reclaimed, so
            // the repair MUST land before that point or it would have no
            // recovery path at all.
            //
            // A HANDLED repair error (not a crash) is deliberately NOT left to
            // propagate into the shared poison path below. That path's
            // `job.attempts > RESTORE_MAX_ATTEMPTS` check runs BEFORE the
            // resume block's delete, so a repair that fails on every one of
            // the last allowed attempt's retries would otherwise strand that
            // attempt's inserted-but-unrepaired rows live and pinned until the
            // pin's own (operator-set, day-scale) expiry — every reader
            // double-counting that guest for the whole window. Handled here
            // instead: delete exactly what this pin wrote, then fail the job
            // outright, the same immediate-failure shape the insert error arm
            // below already uses (this is not a crash-recovery case, so it
            // does not need the attempts-based retry machinery at all).
            match repo::repair_restored_rows(&mut c, &job.table_name, pin_id, rs, re).await {
                Ok(repaired) => {
                    info!(job = %job.id, rows = n, repaired, "resolved restored guest ids at the source");
                    repo::finish_restore_job(&mut c, job.id, worker_id, "succeeded", n, "").await?;
                    info!(job = %job.id, rows = n, estimate, "restore complete");
                }
                Err(e) => {
                    let removed =
                        match repo::delete_restored_rows(&mut c, &job.table_name, pin_id, rs, re)
                            .await
                        {
                            Ok(removed) => removed,
                            Err(del_err) => {
                                warn!(
                                    job = %job.id, error = %del_err,
                                    "failed to clean up after a repair error; rows may remain \
                                     live and unrepaired"
                                );
                                0
                            }
                        };
                    repo::finish_restore_job(
                        &mut c,
                        job.id,
                        worker_id,
                        "failed",
                        0,
                        &format!("repair failed: {e}"),
                    )
                    .await?;
                    warn!(
                        job = %job.id, error = %e, removed,
                        "restore repair failed; discarded partial output, job failed"
                    );
                }
            }
        }
        Err(e) => {
            // Leave the pin: the next attempt reuses it and deletes whatever this
            // attempt wrote before the failure. Dropping the pin here would
            // orphan those rows.
            repo::finish_restore_job(&mut c, job.id, worker_id, "failed", 0, &e.to_string())
                .await?;
            warn!(job = %job.id, error = %e, "restore failed");
        }
    }
    Ok(true)
}

/// `error_events_2026_05_01` → 2026-05-01T00:00:00Z.
fn parse_suffix_start(child: &str, table: &str) -> Option<DateTime<Utc>> {
    let suffix = child.strip_prefix(&format!("{table}_"))?;
    let parts: Vec<&str> = suffix.split('_').collect();
    if parts.len() != 3 {
        return None;
    }
    let (y, m, d) = (
        parts[0].parse().ok()?,
        parts[1].parse().ok()?,
        parts[2].parse().ok()?,
    );
    chrono::TimeZone::with_ymd_and_hms(&Utc, y, m, d, 0, 0, 0).single()
}
