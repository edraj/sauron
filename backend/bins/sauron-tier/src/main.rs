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
        let cold_rows = tokio::task::spawn_blocking(move || -> anyhow::Result<i64> {
            let eng = DuckEngine::open()?;
            eng.export_from_postgres(&pg_url, &table, rs, re, &cold_dir_c)?;
            eng.count_range(&base_glob_c, rs, re)
        })
        .await??;

        if cold_rows != pg_rows {
            warn!(child = %child, pg_rows, cold_rows, "count mismatch; leaving partition for retry");
            break;
        }
        repo::advance_watermark(&mut c, t.name, range.end).await?;
        info!(child = %child, rows = pg_rows, "exported partition to Parquet");
    }

    // 4. Drop partitions strictly below the watermark AND past the drop lag.
    let wm = repo::get_watermark(&mut c, t.name).await?;
    if let Some(w) = wm {
        let lag = chrono::Duration::hours(cfg.tier_drop_lag_hours);
        for child in repo::list_child_partitions(&mut c, t.name).await? {
            let Some(start) = parse_suffix_start(&child, t.name) else { continue };
            let range = bucket_bounds(start, gran);
            if range.end <= w && (now - range.end) >= lag {
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
