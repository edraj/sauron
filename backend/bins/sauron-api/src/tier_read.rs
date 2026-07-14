//! Cross-tier read router. Splits a query window at the tier watermark and runs
//! the hot (Postgres) and cold (Parquet/DuckDB) halves concurrently, then glues
//! the additive per-day partials.

use chrono::{DateTime, Utc};
use uuid::Uuid;

use sauron_db::{conn, repo};
use sauron_tier::duck::DuckEngine;
use sauron_tier::{cold_partition_glob, merge_day_counts, plan, DayCount};

use crate::AppState;

/// Error counts per day for `[from, to)`, spanning hot + cold as needed.
pub async fn error_counts_by_day(
    state: &AppState,
    app_id: Uuid,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> anyhow::Result<Vec<DayCount>> {
    // No watermark yet ⇒ everything is hot (nothing tiered).
    let wm = {
        let mut c = conn(&state.pool).await?;
        repo::get_watermark(&mut c, "error_events").await?
    };
    let watermark = match wm {
        Some(w) => w,
        None => {
            let mut c = conn(&state.pool).await?;
            let rows = repo::error_counts_by_day_hot(&mut c, app_id, from, to).await?;
            return Ok(rows.into_iter().map(|r| DayCount { day: r.day, count: r.count }).collect());
        }
    };

    let split = plan(watermark, from, to);

    // PG branch: the HOT half (explicit partitions, occurred_at >= watermark) plus,
    // for the COLD half, late-arriving rows in the _default partition (their
    // explicit partition was already tiered+dropped, so they're NOT in Parquet).
    // Both run on one pooled connection (peak one PG conn per request).
    let pool = state.pool.clone();
    let pg = async move {
        let mut c = conn(&pool).await?;
        let hot_rows = if let Some(r) = split.hot {
            repo::error_counts_by_day_hot(&mut c, app_id, r.start, r.end).await?
        } else {
            Vec::new()
        };
        let cold_default_rows = if let Some(r) = split.cold {
            repo::default_partition_counts_by_day(&mut c, "error_events_default", app_id, r.start, r.end).await?
        } else {
            Vec::new()
        };
        Ok::<_, anyhow::Error>((hot_rows, cold_default_rows))
    };

    // COLD Parquet branch: DuckDB is blocking → spawn_blocking, runs concurrently.
    let cold_path = state.cfg.tier_cold_path.clone();
    let cold_parquet = async move {
        if let Some(r) = split.cold {
            let glob = cold_partition_glob(&cold_path, "error_events", app_id);
            tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<DayCount>> {
                let eng = DuckEngine::open()?;
                eng.counts_by_day(&glob, app_id, r.start, r.end)
            })
            .await?
        } else {
            Ok(Vec::new())
        }
    };

    let (pg_res, parquet_res) = tokio::join!(pg, cold_parquet);
    let (hot_rows, cold_default_rows) = pg_res?;
    let parquet_rows = parquet_res?;
    let to_dc = |rows: Vec<repo::DayCountRow>| -> Vec<DayCount> {
        rows.into_iter().map(|r| DayCount { day: r.day, count: r.count }).collect()
    };
    // COLD = Parquet (exported) + _default (late arrivals); then + HOT. All additive,
    // and the three sets are disjoint (a row is in exactly one of: parquet, _default, hot).
    let cold = merge_day_counts(parquet_rows, to_dc(cold_default_rows));
    Ok(merge_day_counts(to_dc(hot_rows), cold))
}

/// Analytics-event counts per day for `[from, to)`, spanning hot + cold as needed.
pub async fn event_counts_by_day(
    state: &AppState,
    app_id: Uuid,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> anyhow::Result<Vec<DayCount>> {
    // No watermark yet ⇒ everything is hot (nothing tiered).
    let wm = {
        let mut c = conn(&state.pool).await?;
        repo::get_watermark(&mut c, "analytics_events").await?
    };
    let watermark = match wm {
        Some(w) => w,
        None => {
            let mut c = conn(&state.pool).await?;
            let rows = repo::event_counts_by_day_hot(&mut c, app_id, from, to).await?;
            return Ok(rows.into_iter().map(|r| DayCount { day: r.day, count: r.count }).collect());
        }
    };

    let split = plan(watermark, from, to);

    // PG branch: the HOT half (explicit partitions, occurred_at >= watermark) plus,
    // for the COLD half, late-arriving rows in the _default partition (their
    // explicit partition was already tiered+dropped, so they're NOT in Parquet).
    // Both run on one pooled connection (peak one PG conn per request).
    let pool = state.pool.clone();
    let pg = async move {
        let mut c = conn(&pool).await?;
        let hot_rows = if let Some(r) = split.hot {
            repo::event_counts_by_day_hot(&mut c, app_id, r.start, r.end).await?
        } else {
            Vec::new()
        };
        let cold_default_rows = if let Some(r) = split.cold {
            repo::default_partition_counts_by_day(&mut c, "analytics_events_default", app_id, r.start, r.end).await?
        } else {
            Vec::new()
        };
        Ok::<_, anyhow::Error>((hot_rows, cold_default_rows))
    };

    // COLD Parquet branch: DuckDB is blocking → spawn_blocking, runs concurrently.
    let cold_path = state.cfg.tier_cold_path.clone();
    let cold_parquet = async move {
        if let Some(r) = split.cold {
            let glob = cold_partition_glob(&cold_path, "analytics_events", app_id);
            tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<DayCount>> {
                let eng = DuckEngine::open()?;
                eng.counts_by_day(&glob, app_id, r.start, r.end)
            })
            .await?
        } else {
            Ok(Vec::new())
        }
    };

    let (pg_res, parquet_res) = tokio::join!(pg, cold_parquet);
    let (hot_rows, cold_default_rows) = pg_res?;
    let parquet_rows = parquet_res?;
    let to_dc = |rows: Vec<repo::DayCountRow>| -> Vec<DayCount> {
        rows.into_iter().map(|r| DayCount { day: r.day, count: r.count }).collect()
    };
    // COLD = Parquet (exported) + _default (late arrivals); then + HOT. All additive,
    // and the three sets are disjoint (a row is in exactly one of: parquet, _default, hot).
    let cold = merge_day_counts(parquet_rows, to_dc(cold_default_rows));
    Ok(merge_day_counts(to_dc(hot_rows), cold))
}

/// Transaction counts (throughput) per day for `[from, to)`, spanning hot + cold
/// as needed. ADDITIVE metric only — safe to sum across tiers. Transaction
/// PERCENTILES (p50/p95 of duration_ms) are HOLISTIC and are NOT merged across
/// tiers; those endpoints stay hot-only (Postgres).
pub async fn transaction_counts_by_day(
    state: &AppState,
    app_id: Uuid,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> anyhow::Result<Vec<DayCount>> {
    // No watermark yet ⇒ everything is hot (nothing tiered).
    let wm = {
        let mut c = conn(&state.pool).await?;
        repo::get_watermark(&mut c, "transactions").await?
    };
    let watermark = match wm {
        Some(w) => w,
        None => {
            let mut c = conn(&state.pool).await?;
            let rows = repo::transaction_counts_by_day_hot(&mut c, app_id, from, to).await?;
            return Ok(rows.into_iter().map(|r| DayCount { day: r.day, count: r.count }).collect());
        }
    };

    let split = plan(watermark, from, to);

    // PG branch: the HOT half (explicit partitions, occurred_at >= watermark) plus,
    // for the COLD half, late-arriving rows in the _default partition (their
    // explicit partition was already tiered+dropped, so they're NOT in Parquet).
    // Both run on one pooled connection (peak one PG conn per request).
    let pool = state.pool.clone();
    let pg = async move {
        let mut c = conn(&pool).await?;
        let hot_rows = if let Some(r) = split.hot {
            repo::transaction_counts_by_day_hot(&mut c, app_id, r.start, r.end).await?
        } else {
            Vec::new()
        };
        let cold_default_rows = if let Some(r) = split.cold {
            repo::default_partition_counts_by_day(&mut c, "transactions_default", app_id, r.start, r.end).await?
        } else {
            Vec::new()
        };
        Ok::<_, anyhow::Error>((hot_rows, cold_default_rows))
    };

    // COLD Parquet branch: DuckDB is blocking → spawn_blocking, runs concurrently.
    let cold_path = state.cfg.tier_cold_path.clone();
    let cold_parquet = async move {
        if let Some(r) = split.cold {
            let glob = cold_partition_glob(&cold_path, "transactions", app_id);
            tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<DayCount>> {
                let eng = DuckEngine::open()?;
                eng.counts_by_day(&glob, app_id, r.start, r.end)
            })
            .await?
        } else {
            Ok(Vec::new())
        }
    };

    let (pg_res, parquet_res) = tokio::join!(pg, cold_parquet);
    let (hot_rows, cold_default_rows) = pg_res?;
    let parquet_rows = parquet_res?;
    let to_dc = |rows: Vec<repo::DayCountRow>| -> Vec<DayCount> {
        rows.into_iter().map(|r| DayCount { day: r.day, count: r.count }).collect()
    };
    // COLD = Parquet (exported) + _default (late arrivals); then + HOT. All additive,
    // and the three sets are disjoint (a row is in exactly one of: parquet, _default, hot).
    let cold = merge_day_counts(parquet_rows, to_dc(cold_default_rows));
    Ok(merge_day_counts(to_dc(hot_rows), cold))
}
