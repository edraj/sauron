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
use sauron_db::repo::DropOutcome;
use sauron_db::{conn, repo, PgPool};
use sauron_tier::disk::Pressure;
use sauron_tier::duck::{DuckEngine, Verdict};
use sauron_tier::{
    bucket_bounds, cold_copy_dir, cold_partition_glob, partition_suffix, quarantine, Granularity,
    TieredTable, TIERED_TABLES,
};
use uuid::Uuid;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    sauron_telemetry::init("sauron-tier");
    let cfg = Config::from_env()?;
    let pool = sauron_db::build_pool(&cfg.database_url, 4)?;
    let gran = Granularity::from_str_or(&cfg.tier_granularity, Granularity::Day);
    info!(hot_days = cfg.tier_hot_days, granularity = ?gran, "sauron-tier started");

    // Exports are written to a staging directory and only moved into cold
    // storage once complete (see `DuckEngine::copy_to_cold`). Anything still
    // there now was left by a process that died mid-export, was never visible
    // to a reader, and is safe to delete. Done before the first cycle, which
    // is the only writer.
    match sauron_tier::duck::clear_staging(&cfg.tier_cold_path) {
        Ok(0) => {}
        Ok(n) => info!(
            removed = n,
            "removed uncommitted exports left by a previous process"
        ),
        Err(e) => warn!(error = %e, "could not clear the export staging directory"),
    }

    // Set on SIGTERM. The tiering loop checks it between partitions, so a
    // stop waits for the export in progress instead of killing it.
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);

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

    let mut tiering = tokio::spawn(async move { tier_loop(pool, cfg, gran, stop_rx).await });

    // No loop returns on its own. If any task dies the process should too,
    // rather than silently continuing with part of its job undone.
    //
    // A stop request is the one orderly exit. Only the tiering loop is waited
    // for: it is the one writing files that readers trust. A restore or purge
    // in flight runs inside Postgres transactions, which roll back cleanly
    // when the process exits.
    tokio::select! {
        r = restore => warn!(?r, "restore loop exited"),
        r = &mut tiering => warn!(?r, "tiering loop exited"),
        r = purging => warn!(?r, "purge loop exited"),
        () = shutdown_signal() => {
            info!("stop requested; finishing the partition in progress");
            let _ = stop_tx.send(true);
            match tiering.await {
                Ok(()) => info!("tiering stopped cleanly"),
                Err(e) => warn!(error = %e, "tiering task failed while stopping"),
            }
        }
    }
    Ok(())
}

/// Resolves on SIGTERM (what systemd sends on stop and restart) or Ctrl-C.
async fn shutdown_signal() {
    use tokio::signal::unix::{signal, SignalKind};
    match signal(SignalKind::terminate()) {
        Ok(mut term) => {
            tokio::select! {
                _ = term.recv() => {}
                _ = tokio::signal::ctrl_c() => {}
            }
        }
        Err(e) => {
            warn!(error = %e, "cannot listen for SIGTERM; only Ctrl-C stops gracefully");
            let _ = tokio::signal::ctrl_c().await;
        }
    }
}

async fn tier_loop(
    pool: PgPool,
    cfg: Config,
    gran: Granularity,
    mut stop: tokio::sync::watch::Receiver<bool>,
) {
    loop {
        if let Err(e) = cycle(&pool, &cfg, gran, &stop).await {
            // `{:#}`, not `%e`: anyhow's Display prints only the outermost
            // context ("export failed; removed 1 partial file(s)") and drops
            // the cause — which is how a production export failed for a day
            // with no reason in the journal.
            warn!(error = %format_args!("{e:#}"), "tier cycle failed; backing off");
        }
        if *stop.borrow() {
            return;
        }
        tokio::select! {
            () = tokio::time::sleep(Duration::from_secs(cfg.tier_tick_secs)) => {}
            _ = stop.changed() => return,
        }
    }
}

/// How many days behind its rotation age a table's watermark may fall before
/// every cycle warns about it. One day of slack covers a cycle that runs just
/// before midnight UTC and the rollup interlock holding the newest partition.
const TIER_LAG_WARN_DAYS: i64 = 2;

async fn cycle(
    pool: &PgPool,
    cfg: &Config,
    gran: Granularity,
    stop: &tokio::sync::watch::Receiver<bool>,
) -> anyhow::Result<()> {
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

    let (hot_days, drop_lag_hours) = disk_adjusted(cfg, hot_days);

    for t in TIERED_TABLES {
        if *stop.borrow() {
            break;
        }
        let result = tier_table(pool, cfg, gran, t, hot_days, drop_lag_hours, stop).await;
        record_health(pool, t.name, &result).await;
        if let Err(e) = &result {
            warn!(table = t.name, error = %format_args!("{e:#}"), "tiering table failed");
        }
        warn_if_behind(pool, gran, t.name, hot_days).await;
    }
    Ok(())
}

/// The rotation age and drop lag for this cycle, after looking at free space
/// on the cold-storage filesystem (usually also the Postgres disk).
///
/// Below `tier_disk_emergency_pct` free, the cycle tiers on
/// `tier_emergency_hot_days` and skips the drop lag. Every safety check on a
/// drop still applies -- the cold copy is verified by key first and pinned
/// ranges are kept -- so the only thing traded away is how long a partition
/// sits in both tiers. That is the right trade when the alternative is
/// Postgres aborting on a full disk.
fn disk_adjusted(cfg: &Config, hot_days: i64) -> (i64, i64) {
    let normal = (hot_days, cfg.tier_drop_lag_hours);
    // The cold directory does not exist before the first export; its nearest
    // existing ancestor is on the same filesystem in every sane layout.
    let path = std::path::Path::new(&cfg.tier_cold_path);
    let probe = path.ancestors().find(|p| p.exists()).unwrap_or(path);
    let reading = match sauron_tier::disk::usage(probe) {
        Ok(r) => r,
        Err(e) => {
            warn!(path = %probe.display(), error = %e, "cannot read free disk space");
            return normal;
        }
    };
    let free_pct = reading.free_pct();
    let free_mb = reading.free_bytes / (1024 * 1024);
    match sauron_tier::disk::classify(
        free_pct,
        cfg.tier_disk_warn_pct,
        cfg.tier_disk_emergency_pct,
    ) {
        Pressure::Normal => normal,
        Pressure::Low => {
            warn!(
                path = %probe.display(),
                free_pct,
                free_mb,
                threshold_pct = cfg.tier_disk_warn_pct,
                "disk space is low on the cold-storage filesystem"
            );
            normal
        }
        Pressure::Critical => {
            let emergency = cfg.tier_emergency_hot_days.max(1).min(hot_days);
            warn!(
                path = %probe.display(),
                free_pct,
                free_mb,
                threshold_pct = cfg.tier_disk_emergency_pct,
                hot_days = emergency,
                "disk space is critical; tiering in emergency mode this cycle (shorter rotation age, no drop lag)"
            );
            (emergency, 0)
        }
    }
}

/// Persist how `table`'s turn in this cycle went (see migration 000081).
/// Best effort: failing to record health must not fail the cycle.
async fn record_health(pool: &PgPool, table: &str, result: &anyhow::Result<()>) {
    let now = Utc::now();
    let mut c = match conn(pool).await {
        Ok(c) => c,
        Err(e) => {
            warn!(table, error = %e, "cannot record tiering health");
            return;
        }
    };
    let recorded = match result {
        Ok(()) => repo::record_tiering_success(&mut c, table, now)
            .await
            .map(|_| ()),
        Err(e) => match repo::record_tiering_failure(&mut c, table, now, &format!("{e:#}")).await {
            Ok(n) if n >= 3 => {
                warn!(
                    table,
                    consecutive_failures = n,
                    "tiering has failed repeatedly for this table"
                );
                Ok(())
            }
            other => other.map(|_| ()),
        },
    };
    if let Err(e) = recorded {
        warn!(table, error = %e, "cannot record tiering health");
    }
}

/// Warn when `table`'s watermark has fallen more than [`TIER_LAG_WARN_DAYS`]
/// behind where the rotation age says it should be.
///
/// This is the signal that was missing when one table's tiering stopped for
/// weeks: every cycle "worked" for the other tables, and the stalled one
/// filled the disk without anything saying it had stopped moving.
async fn warn_if_behind(pool: &PgPool, gran: Granularity, table: &str, hot_days: i64) {
    let Ok(mut c) = conn(pool).await else {
        return;
    };
    let Ok(Some(wm)) = repo::get_watermark(&mut c, table).await else {
        return;
    };
    let expected = bucket_bounds(Utc::now() - chrono::Duration::days(hot_days), gran).start;
    let behind = expected - wm;
    if behind >= chrono::Duration::days(TIER_LAG_WARN_DAYS) {
        warn!(
            table,
            watermark = %wm,
            expected = %expected,
            behind_days = behind.num_days(),
            "tiering is behind: partitions past the rotation age are still in Postgres"
        );
    }
}

#[allow(clippy::too_many_arguments)]
async fn tier_table(
    pool: &PgPool,
    cfg: &Config,
    gran: Granularity,
    t: &TieredTable,
    hot_days: i64,
    drop_lag_hours: i64,
    stop: &tokio::sync::watch::Receiver<bool>,
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

    // 1. Pre-create partitions for now .. now + partition_ahead buckets. A
    //    failure here (a full disk refuses CREATE TABLE) must not skip the
    //    drops below, so it is kept and returned at the end like an export
    //    error.
    let precreated: anyhow::Result<()> = async {
        let mut b = bucket_bounds(now, gran);
        for _ in 0..cfg.tier_partition_ahead {
            repo::create_range_partition(
                &mut c,
                t.name,
                &partition_suffix(b.start),
                b.start,
                b.end,
            )
            .await?;
            b = bucket_bounds(b.end, gran);
        }
        Ok(())
    }
    .await;

    // 2. Eligibility cutoff: partitions whose END <= (now - hot_days) may tier.
    let cutoff = now - chrono::Duration::days(hot_days);
    let cold_dir = cold_copy_dir(&cfg.tier_cold_path, t.name);
    let base_glob = format!("{}/**/*.parquet", cold_dir);

    // 2b. Set aside cold files DuckDB cannot read, BEFORE anything below reads
    //     the glob. One such file fails every read of the table — a 0-byte file
    //     from a killed export stopped `error_events` tiering for 18 days on a
    //     production host, and broke the Storage page with it. See
    //     `sauron_tier::quarantine` for what is checked and what is never
    //     touched. Renamed, not deleted.
    let sweep_dir = std::path::PathBuf::from(&cold_dir);
    let sweep = tokio::task::spawn_blocking(move || {
        quarantine::quarantine_unreadable(
            &sweep_dir,
            quarantine::MIN_AGE,
            std::time::SystemTime::now(),
        )
    })
    .await?;
    for q in &sweep.moved {
        warn!(
            table = t.name,
            file = %q.from.display(),
            moved_to = %q.to.display(),
            bytes = q.bytes,
            reason = q.reason,
            "set aside an unreadable cold Parquet file"
        );
    }
    for (file, err) in &sweep.failed {
        warn!(table = t.name, file = %file.display(), error = %err, "could not set aside an unreadable cold Parquet file");
    }

    // 3. Export eligible partitions oldest-first; stop on the first failure so
    //    the watermark never skips a gap.
    //
    //    A failure here stops EXPORTING, not the cycle: it ends this block, and
    //    the drop step below still runs. Drops only touch partitions already
    //    below the watermark and do not depend on this loop at all, and on a
    //    nearly full disk they are what frees space, so nothing in here may
    //    skip them -- not an export error, and not a failed watermark read or
    //    row count either. The error is returned after the drops, so the cycle
    //    still reports it.
    //
    //    A count mismatch or a refused reconcile is an error too. Both used to
    //    stop the loop silently, which is how a table can stall for weeks
    //    while every cycle looks clean.
    let exported = export_eligible(&mut c, cfg, gran, t, cutoff, &cold_dir, &base_glob, stop).await;

    // 4. Drop partitions at/below the PRE-CYCLE watermark AND past the drop lag.
    //    Using wm_at_cycle_start (not a fresh read) guarantees a partition exported
    //    THIS cycle waits until a later cycle to be dropped (the grace window).
    if let Some(w) = wm_at_cycle_start {
        let lag = chrono::Duration::hours(drop_lag_hours);
        for child in repo::list_child_partitions(&mut c, t.name).await? {
            if *stop.borrow() {
                info!(
                    table = t.name,
                    "shutdown requested; leaving remaining drops to the next run"
                );
                break;
            }
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
                // grace window, or long after it: an SDK flushing an offline
                // buffer). Such a row is NOT in Parquet, so dropping would lose it.
                //
                // This used to stop there — count, and retain the partition if it
                // grew — and nothing ever exported the late rows, so a partition
                // that took one late row was retained FOREVER. On a production
                // host that was 44 partitions and a full disk. `reconcile_range`
                // now appends exactly the Postgres rows cold lacks (matched by
                // primary key) and says whether cold then holds all of them.
                let pg_url = cfg.database_url.clone();
                let table = t.name.to_string();
                let cold_dir_c = cold_dir.clone();
                let base_glob_c = base_glob.clone();
                let (rs, re) = (range.start, range.end);
                let rec = tokio::task::spawn_blocking(move || {
                    let eng = DuckEngine::open()?;
                    eng.reconcile_range(&pg_url, &table, rs, re, &base_glob_c, &cold_dir_c)
                })
                .await?;
                // One partition failing (an append that runs out of disk, say)
                // must not stop the ones after it: those drops are what free the
                // space. The append already removed its partial files.
                let rec = match rec {
                    Ok(rec) => rec,
                    Err(e) => {
                        warn!(child = %child, error = %format_args!("{e:#}"), "late-arrival reconcile failed; retaining");
                        continue;
                    }
                };
                if rec.exported > 0 {
                    info!(child = %child, exported = rec.exported, "exported late arrivals to Parquet");
                }
                let expected = match rec.verdict {
                    Verdict::Ready { pg_rows } => pg_rows,
                    Verdict::Retain(why) => {
                        warn!(child = %child, pg_rows = rec.pg_rows, cold_rows = rec.cold_rows, missing = rec.missing, reason = %why, "partition retained");
                        continue;
                    }
                };
                // The check above ran seconds ago on another connection. The drop
                // re-counts under its own locks and refuses if anything changed.
                match repo::drop_partition_if_unchanged(&mut c, t.name, &child, expected).await? {
                    DropOutcome::Dropped => {
                        repo::set_dropped_thru(&mut c, t.name, range.end).await?;
                        info!(child = %child, rows = expected, "dropped Postgres partition (now cold-only)");
                    }
                    DropOutcome::Changed { rows } => {
                        warn!(child = %child, verified = expected, now = rows, "partition changed during the drop; retaining until next cycle");
                    }
                    DropOutcome::LockBusy => {
                        warn!(child = %child, "partition busy; drop deferred to next cycle");
                    }
                }
            }
        }
    }

    {
        // Tier 1 stack-pool GC. Partition DROP just above is the event that
        // orphans `error_stack_blobs` rows — a trace's referencing events age
        // out wholesale with their partition, and nothing decrements anything
        // (there is deliberately no refcount; see `stack_pool`'s module doc).
        // The sweep deletes what no surviving partition references, the
        // partial index makes the probe cheap, and the FK downgrades any bug
        // here to a loud constraint error instead of data loss. Cold files
        // materialized their traces at export time, so a swept blob is never
        // needed again.
        let mut c = sauron_db::conn(pool).await?;
        let swept = sauron_db::stack_pool::sweep_orphan_stack_blobs(
            &mut c,
            sauron_db::stack_pool::STACK_BLOB_SWEEP_GRACE_HOURS,
        )
        .await?;
        if swept > 0 {
            info!(swept, "swept unreferenced error_stack_blobs");
        }
    }
    match (precreated, exported) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(e), Ok(())) => Err(e.context("pre-creating partitions")),
        (Ok(()), Err(e)) => Err(e),
        (Err(pre), Err(e)) => {
            Err(e.context(format!("also failed pre-creating partitions: {pre:#}")))
        }
    }
}

/// Step 3 of [`tier_table`]: export eligible partitions oldest-first, stopping
/// at the first failure so the watermark never skips a gap. See the comment at
/// the call site for why every failure is returned rather than `?`-ed out of
/// `tier_table`.
#[allow(clippy::too_many_arguments)]
async fn export_eligible(
    c: &mut sauron_db::PgConn,
    cfg: &Config,
    gran: Granularity,
    t: &TieredTable,
    cutoff: DateTime<Utc>,
    cold_dir: &str,
    base_glob: &str,
    stop: &tokio::sync::watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let children = repo::list_child_partitions(c, t.name).await?;
    for child in children {
        if *stop.borrow() {
            info!(
                table = t.name,
                "shutdown requested; not starting another export"
            );
            break;
        }
        let Some(start) = parse_suffix_start(&child, t.name) else {
            continue;
        };
        let range = bucket_bounds(start, gran);
        if range.end > cutoff {
            continue; // still hot
        }
        // Rollup interlock: never export a partition the fold has not fully
        // passed. The rollups are what keep aggregates answerable after the
        // raw rows leave Postgres, so exporting ahead of the watermark would
        // tier out rows that never reached them. Trivially satisfied in
        // steady state (fold lag ~1 min vs day-old exports) — enforced, not
        // assumed, because a stopped ingest process is exactly the state in
        // which both "fold is behind" and "partitions keep aging" hold.
        match sauron_db::rollups::as_of(c, &sauron_db::rollups::EVENT_SOURCES).await? {
            Some(ro_wm) if range.end <= ro_wm => {}
            ro => {
                tracing::info!(
                    table = t.name, partition = %child, rollup_watermark = ?ro,
                    "tier: partition retained until the rollup fold passes it"
                );
                continue;
            }
        }
        let wm = repo::get_watermark(c, t.name).await?;
        if let Some(w) = wm {
            if range.start < w {
                continue; // already exported
            }
        }
        let pg_rows = repo::count_child_rows(c, &child).await?;

        let pg_url = cfg.database_url.clone();
        let table = t.name.to_string();
        let cold_dir_c = cold_dir.to_string();
        let base_glob_c = base_glob.to_string();
        let (rs, re) = (range.start, range.end);
        let pg_rows_c = pg_rows;
        // Idempotency pre-check: only export when cold has NOTHING for this range.
        // `APPEND` is not idempotent, so re-exporting a range that already has data
        // would duplicate rows. `already`: rows already in cold for [rs, re).
        //   already == pg_rows  → already exported (a prior watermark-advance didn't
        //                         stick); skip export, just advance.
        //   already == 0        → fresh export, then verify.
        //   0 < already != pg   → partial/corrupt cold data; do NOT append more.
        let exported =
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
            .await?;
        let (already, exported_cold) = match exported {
            Ok(v) => v,
            Err(e) => return Err(e.context(format!("exporting {child}"))),
        };

        match exported_cold {
            Some(cold_rows) => {
                if cold_rows != pg_rows {
                    anyhow::bail!(
                        "count mismatch after exporting {child}: {pg_rows} row(s) in Postgres, \
                         {cold_rows} in cold; leaving the partition for retry"
                    );
                }
                repo::advance_watermark(c, t.name, range.end).await?;
                info!(child = %child, rows = pg_rows, "exported partition to Parquet");
            }
            None if already == pg_rows => {
                // Rows already durable in cold from a prior attempt — idempotent advance.
                repo::advance_watermark(c, t.name, range.end).await?;
                info!(child = %child, rows = pg_rows, "partition already in cold; advanced watermark");
            }
            None => {
                // Cold holds some of this range but not all: an earlier export
                // whose watermark advance did not happen, plus rows that landed
                // since. This used to demand a manual clear and `break` — every
                // later partition of the table stuck behind one late row. The
                // same key-matched reconcile the drop step uses appends exactly
                // what cold lacks, never a row it already holds, and refuses the
                // one shape where appending could duplicate (see
                // `plan_reconcile`).
                let pg_url = cfg.database_url.clone();
                let table = t.name.to_string();
                let cold_dir_c = cold_dir.to_string();
                let base_glob_c = base_glob.to_string();
                let rec = tokio::task::spawn_blocking(move || {
                    let eng = DuckEngine::open()?;
                    eng.reconcile_range(&pg_url, &table, rs, re, &base_glob_c, &cold_dir_c)
                })
                .await?;
                match rec {
                    Ok(rec) => match rec.verdict {
                        Verdict::Ready { pg_rows } => {
                            repo::advance_watermark(c, t.name, range.end).await?;
                            info!(child = %child, rows = pg_rows, appended = rec.exported, "completed a partial export; advanced watermark");
                        }
                        Verdict::Retain(why) => {
                            anyhow::bail!(
                                "partial cold data for {child} ({} in Postgres, {} in cold, \
                                 {} missing); not advancing: {why}",
                                rec.pg_rows,
                                rec.cold_rows,
                                rec.missing
                            );
                        }
                    },
                    Err(e) => return Err(e.context(format!("completing the export of {child}"))),
                }
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
            Err(e) => warn!(error = %format_args!("{e:#}"), "restore job failed"),
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
            warn!(job = %job.id, error = %format_args!("{e:#}"), "restore failed");
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
