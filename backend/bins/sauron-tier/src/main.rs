//! `sauron-tier` — moves aged partitions from Postgres to Parquet.
//!
//! Each cycle, per tiered table: pre-create upcoming partitions, export aged
//! partitions to Parquet (copy → verify counts → advance watermark), then drop
//! partitions that are below the watermark AND older than the drop lag. Nothing
//! is ever deleted: a partition is dropped only after its rows are verified in
//! Parquet, which is the permanent copy.

use std::time::Duration;

use chrono::{DateTime, Utc};
use tracing::{info, warn};

use sauron_core::Config;
use sauron_db::{conn, repo, PgPool};
use sauron_tier::{bucket_bounds, cold_copy_dir, partition_suffix, Granularity, TieredTable, TIERED_TABLES};
use sauron_tier::duck::DuckEngine;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    sauron_telemetry::init("sauron-tier");
    let cfg = Config::from_env()?;
    let pool = sauron_db::build_pool(&cfg.database_url, 4)?;
    let gran = Granularity::from_str_or(&cfg.tier_granularity, Granularity::Day);
    info!(hot_days = cfg.tier_hot_days, granularity = ?gran, "sauron-tier started");

    loop {
        if let Err(e) = cycle(&pool, &cfg, gran).await {
            warn!(error = %e, "tier cycle failed; backing off");
        }
        tokio::time::sleep(Duration::from_secs(cfg.tier_tick_secs)).await;
    }
}

async fn cycle(pool: &PgPool, cfg: &Config, gran: Granularity) -> anyhow::Result<()> {
    for t in TIERED_TABLES {
        if let Err(e) = tier_table(pool, cfg, gran, t).await {
            warn!(table = t.name, error = %e, "tiering table failed");
        }
    }
    Ok(())
}

async fn tier_table(pool: &PgPool, cfg: &Config, gran: Granularity, t: &TieredTable) -> anyhow::Result<()> {
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
        repo::create_range_partition(&mut c, t.name, &partition_suffix(b.start), b.start, b.end).await?;
        b = bucket_bounds(b.end, gran);
    }

    // 2. Eligibility cutoff: partitions whose END <= (now - hot_days) may tier.
    let cutoff = now - chrono::Duration::days(cfg.tier_hot_days);
    let cold_dir = cold_copy_dir(&cfg.tier_cold_path, t.name);
    let base_glob = format!("{}/**/*.parquet", cold_dir);

    // 3. Export eligible partitions oldest-first; stop on the first failure so
    //    the watermark never skips a gap.
    let children = repo::list_child_partitions(&mut c, t.name).await?;
    for child in children {
        let Some(start) = parse_suffix_start(&child, t.name) else { continue };
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
            let Some(start) = parse_suffix_start(&child, t.name) else { continue };
            let range = bucket_bounds(start, gran);
            if range.end <= w && (now - range.end) >= lag {
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

/// `error_events_2026_05_01` → 2026-05-01T00:00:00Z.
fn parse_suffix_start(child: &str, table: &str) -> Option<DateTime<Utc>> {
    let suffix = child.strip_prefix(&format!("{table}_"))?;
    let parts: Vec<&str> = suffix.split('_').collect();
    if parts.len() != 3 {
        return None;
    }
    let (y, m, d) = (parts[0].parse().ok()?, parts[1].parse().ok()?, parts[2].parse().ok()?);
    chrono::TimeZone::with_ymd_and_hms(&Utc, y, m, d, 0, 0, 0).single()
}
