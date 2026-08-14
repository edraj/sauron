//! Cross-tier read router. Splits a query window at the tier watermark, moves
//! any restored ranges back to the hot side, and runs the Postgres and
//! Parquet halves concurrently before gluing the additive per-day partials.
//!
//! The three public entry points were three near-identical 90-line copies. They
//! are one generic path now, because the restore work had to change the same
//! six things in each of them and a fourth tiered table would have meant a
//! fourth copy.

use chrono::{DateTime, Utc};
use uuid::Uuid;

use sauron_db::{conn, repo};
use sauron_tier::duck::DuckEngine;
use sauron_tier::{cold_partition_glob, merge_day_counts, plan_with_restores, DayCount, TimeRange};

use crate::AppState;

/// Which tiered table a cross-tier read is against. Carries the table name and
/// selects the hot-side query, which is the only thing that actually differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tiered {
    Errors,
    Events,
    Transactions,
}

impl Tiered {
    fn table(self) -> &'static str {
        match self {
            Tiered::Errors => "error_events",
            Tiered::Events => "analytics_events",
            Tiered::Transactions => "transactions",
        }
    }

    fn default_partition(self) -> &'static str {
        match self {
            Tiered::Errors => "error_events_default",
            Tiered::Events => "analytics_events_default",
            Tiered::Transactions => "transactions_default",
        }
    }
}

async fn hot_counts(
    kind: Tiered,
    c: &mut diesel_async::AsyncPgConnection,
    app_id: Uuid,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<Vec<repo::DayCountRow>, diesel::result::Error> {
    match kind {
        Tiered::Errors => repo::error_counts_by_day_hot(c, app_id, from, to).await,
        Tiered::Events => repo::event_counts_by_day_hot(c, app_id, from, to).await,
        Tiered::Transactions => repo::transaction_counts_by_day_hot(c, app_id, from, to).await,
    }
}

fn to_dc(rows: Vec<repo::DayCountRow>) -> Vec<DayCount> {
    rows.into_iter()
        .map(|r| DayCount {
            day: r.day,
            count: r.count,
        })
        .collect()
}

fn merge_all(parts: Vec<Vec<DayCount>>) -> Vec<DayCount> {
    parts.into_iter().fold(Vec::new(), merge_day_counts)
}

/// Per-day counts for `[from, to)` across both tiers.
async fn counts_by_day(
    kind: Tiered,
    state: &AppState,
    app_id: Uuid,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> anyhow::Result<Vec<DayCount>> {
    let table = kind.table();

    // Watermark and restored ranges are read together: they are the two halves
    // of one question ("which parts of this window live where?"), and reading
    // them in separate round trips would let a restore land between them.
    let (wm, restored) = {
        let mut c = conn(&state.pool).await?;
        let wm = repo::get_watermark(&mut c, table).await?;
        let restored = repo::restored_ranges(&mut c, table).await?;
        (wm, restored)
    };

    // No watermark ⇒ nothing has ever been tiered, so everything is hot and
    // there is nothing for a restore to have moved.
    let Some(watermark) = wm else {
        let mut c = conn(&state.pool).await?;
        return Ok(to_dc(hot_counts(kind, &mut c, app_id, from, to).await?));
    };

    let restored: Vec<TimeRange> = restored
        .into_iter()
        .map(|(start, end)| TimeRange { start, end })
        .collect();
    let split = plan_with_restores(watermark, from, to, &restored);

    // PG branch: every hot sub-range (which now includes the restored islands)
    // plus, for each COLD sub-range, late arrivals sitting in the `_default`
    // partition — their explicit partition was tiered and dropped, so they are
    // NOT in Parquet. All on one pooled connection.
    let pool = state.pool.clone();
    let hot_ranges = split.hot.clone();
    let cold_ranges = split.cold.clone();
    let default_table = kind.default_partition();
    let pg = async move {
        let mut c = conn(&pool).await?;
        let mut hot_rows = Vec::new();
        for r in &hot_ranges {
            hot_rows.extend(hot_counts(kind, &mut c, app_id, r.start, r.end).await?);
        }
        let mut cold_default_rows = Vec::new();
        for r in &cold_ranges {
            cold_default_rows.extend(
                repo::default_partition_counts_by_day(
                    &mut c,
                    default_table,
                    app_id,
                    r.start,
                    r.end,
                )
                .await?,
            );
        }
        Ok::<_, anyhow::Error>((hot_rows, cold_default_rows))
    };

    // COLD Parquet branch: DuckDB is blocking → spawn_blocking, runs concurrently.
    let cold_path = state.cfg.tier_cold_path.clone();
    let cold_ranges2 = split.cold.clone();
    let cold_parquet = async move {
        if cold_ranges2.is_empty() {
            return Ok(Vec::new());
        }
        let glob = cold_partition_glob(&cold_path, table, app_id);
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<DayCount>> {
            let eng = DuckEngine::open()?;
            let mut out = Vec::new();
            for r in &cold_ranges2 {
                out = merge_day_counts(out, eng.counts_by_day(&glob, app_id, r.start, r.end)?);
            }
            Ok(out)
        })
        .await?
    };

    let (pg_res, parquet_res) = tokio::join!(pg, cold_parquet);
    let (hot_rows, cold_default_rows) = pg_res?;
    let parquet_rows = parquet_res?;

    // The three sets are disjoint by construction: a row is in exactly one of
    // Parquet (cold sub-ranges only), `_default` (cold sub-ranges only), or an
    // explicit/hot partition (hot sub-ranges only). Restored rows are counted
    // once, on the hot side, because `plan_with_restores` removed their range
    // from the cold side entirely.
    Ok(merge_all(vec![
        to_dc(hot_rows),
        parquet_rows,
        to_dc(cold_default_rows),
    ]))
}

/// Error counts per day for `[from, to)`, spanning hot + cold as needed.
pub async fn error_counts_by_day(
    state: &AppState,
    app_id: Uuid,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> anyhow::Result<Vec<DayCount>> {
    counts_by_day(Tiered::Errors, state, app_id, from, to).await
}

/// Analytics-event counts per day for `[from, to)`, spanning hot + cold as needed.
pub async fn event_counts_by_day(
    state: &AppState,
    app_id: Uuid,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> anyhow::Result<Vec<DayCount>> {
    counts_by_day(Tiered::Events, state, app_id, from, to).await
}

/// Transaction counts (throughput) per day for `[from, to)`, spanning hot +
/// cold as needed. ADDITIVE metric only — safe to sum across tiers. Transaction
/// PERCENTILES (p50/p95 of duration_ms) are HOLISTIC and are NOT merged across
/// tiers; those endpoints stay hot-only (Postgres).
pub async fn transaction_counts_by_day(
    state: &AppState,
    app_id: Uuid,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> anyhow::Result<Vec<DayCount>> {
    counts_by_day(Tiered::Transactions, state, app_id, from, to).await
}

// ===========================================================================
// Active Users — the first HOLISTIC cross-tier metric here
// ===========================================================================
//
// Everything above is additive: a row count from Postgres plus a row count from
// Parquet is the right answer, which is what lets `merge_day_counts` glue them.
//
// `COUNT(DISTINCT distinct_id)` is NOT. Two independent totals cannot be combined
// — a person active either side of the watermark is counted once in each, and
// nothing in the two numbers reveals the overlap. **Do not route this through
// `merge_day_counts`, and do not add a total-over-the-range variant that sums
// these per-day values.** (`transaction_counts_by_day`'s doc records the same
// distinction for percentiles, which stay hot-only for exactly this reason.)
//
// What makes a per-DAY distinct count safe is that a day belongs entirely to one
// tier — true because the watermark only advances to a partition END, and
// partitions are day-granular under the default `TIER_GRANULARITY=day`. Set it to
// `week` or `month` and the watermark stops being a day boundary, so one day at
// the seam gets rows from both tiers. That day is REPORTED rather than silently
// halved or double-counted.

/// A day whose count could not be computed exactly, and why.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PartialDay {
    pub day: chrono::NaiveDate,
    pub reason: &'static str,
}

/// Distinct people per UTC day for `[from, to)`, spanning hot + cold.
///
/// Returns the series plus any day excluded from it. A day appearing in BOTH
/// tiers is excluded: its true distinct count is unknowable from two partial
/// counts, and a missing point an operator can see beats a wrong point they
/// cannot.
pub async fn active_users_by_day(
    state: &AppState,
    scope: sauron_db::scope::ReadScope,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> anyhow::Result<(Vec<DayCount>, Vec<PartialDay>)> {
    let app_id = scope.app_id;
    let (wm, restored) = {
        let mut c = conn(&state.pool).await?;
        let wm = repo::get_watermark(&mut c, "analytics_events").await?;
        let restored = repo::restored_ranges(&mut c, "analytics_events").await?;
        (wm, restored)
    };

    // Nothing tiered ⇒ entirely hot, and no seam is possible.
    let Some(watermark) = wm else {
        let mut c = conn(&state.pool).await?;
        let rows = repo::active_users_by_day_hot(&mut c, scope, from, to).await?;
        return Ok((to_dc(rows), Vec::new()));
    };

    let restored: Vec<TimeRange> = restored
        .into_iter()
        .map(|(start, end)| TimeRange { start, end })
        .collect();
    let split = plan_with_restores(watermark, from, to, &restored);

    let mut hot: Vec<DayCount> = Vec::new();
    {
        let mut c = conn(&state.pool).await?;
        for r in &split.hot {
            hot.extend(to_dc(
                repo::active_users_by_day_hot(&mut c, scope.clone(), r.start, r.end).await?,
            ));
        }
    }

    let cold_path = state.cfg.tier_cold_path.clone();
    let cold_ranges = split.cold.clone();
    let cold: Vec<DayCount> = if cold_ranges.is_empty() {
        Vec::new()
    } else {
        // The bounded cold overlay: guest ids Parquet still holds because cold
        // is immutable and the hot rewrite could never reach them. Fetched over
        // the FULL query window (a safe superset of the cold sub-ranges below),
        // not per sub-range — see `sauron_db::identity_merge::cold_alias_map`'s
        // doc comment for why the map is already bounded and an extra row here
        // just costs a slightly bigger overlay, never a wrong answer.
        let aliases: Vec<(String, String)> = {
            let mut c = conn(&state.pool).await?;
            sauron_db::identity_merge::cold_alias_map(&mut c, app_id, from, to)
                .await?
                .into_iter()
                .map(|e| (e.alias, e.person))
                .collect()
        };
        let glob = cold_partition_glob(&cold_path, "analytics_events", app_id);
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<DayCount>> {
            let eng = DuckEngine::open()?;
            let mut out = Vec::new();
            for r in &cold_ranges {
                out.extend(eng.distinct_users_by_day(&glob, app_id, r.start, r.end, &aliases)?);
            }
            Ok(out)
        })
        .await??
    };

    // Concatenate, not merge. A day present on both sides is the seam case: drop
    // it from the series and name it.
    use std::collections::{BTreeMap, BTreeSet};
    let hot_days: BTreeSet<chrono::NaiveDate> = hot.iter().map(|d| d.day).collect();
    let mut partial: Vec<PartialDay> = Vec::new();
    let mut series: BTreeMap<chrono::NaiveDate, i64> = BTreeMap::new();
    for d in hot {
        series.insert(d.day, d.count);
    }
    for d in cold {
        if hot_days.contains(&d.day) {
            // Only reachable when the watermark is not on a day boundary, i.e.
            // TIER_GRANULARITY is week/month, or a restore covered part of a day.
            series.remove(&d.day);
            partial.push(PartialDay {
                day: d.day,
                reason: "the hot/cold boundary falls inside this day, so a distinct \
                         count cannot be computed exactly",
            });
        } else {
            series.insert(d.day, d.count);
        }
    }
    partial.dedup_by_key(|p| p.day);
    Ok((
        series
            .into_iter()
            .map(|(day, count)| DayCount { day, count })
            .collect(),
        partial,
    ))
}
