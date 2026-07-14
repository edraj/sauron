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

    // HOT branch: Postgres via diesel (async).
    let pool = state.pool.clone();
    let hot = async move {
        if let Some(r) = split.hot {
            let mut c = conn(&pool).await?;
            let rows = repo::error_counts_by_day_hot(&mut c, app_id, r.start, r.end).await?;
            Ok::<_, anyhow::Error>(rows.into_iter().map(|r| DayCount { day: r.day, count: r.count }).collect())
        } else {
            Ok(Vec::new())
        }
    };

    // COLD branch: DuckDB is blocking → spawn_blocking, runs concurrently.
    let cold_path = state.cfg.tier_cold_path.clone();
    let cold = async move {
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

    let (hot_rows, cold_rows) = tokio::join!(hot, cold);
    Ok(merge_day_counts(hot_rows?, cold_rows?))
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

    // HOT branch: Postgres via diesel (async).
    let pool = state.pool.clone();
    let hot = async move {
        if let Some(r) = split.hot {
            let mut c = conn(&pool).await?;
            let rows = repo::event_counts_by_day_hot(&mut c, app_id, r.start, r.end).await?;
            Ok::<_, anyhow::Error>(rows.into_iter().map(|r| DayCount { day: r.day, count: r.count }).collect())
        } else {
            Ok(Vec::new())
        }
    };

    // COLD branch: DuckDB is blocking → spawn_blocking, runs concurrently.
    let cold_path = state.cfg.tier_cold_path.clone();
    let cold = async move {
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

    let (hot_rows, cold_rows) = tokio::join!(hot, cold);
    Ok(merge_day_counts(hot_rows?, cold_rows?))
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

    // HOT branch: Postgres via diesel (async).
    let pool = state.pool.clone();
    let hot = async move {
        if let Some(r) = split.hot {
            let mut c = conn(&pool).await?;
            let rows = repo::transaction_counts_by_day_hot(&mut c, app_id, r.start, r.end).await?;
            Ok::<_, anyhow::Error>(rows.into_iter().map(|r| DayCount { day: r.day, count: r.count }).collect())
        } else {
            Ok(Vec::new())
        }
    };

    // COLD branch: DuckDB is blocking → spawn_blocking, runs concurrently.
    let cold_path = state.cfg.tier_cold_path.clone();
    let cold = async move {
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

    let (hot_rows, cold_rows) = tokio::join!(hot, cold);
    Ok(merge_day_counts(hot_rows?, cold_rows?))
}
