//! The admin data purge's executor.
//!
//! Lives here rather than in `sauron-inspector` — the obvious sibling, since
//! the mask job it is modelled on runs there — because recompute MUST read the
//! cold half and that binary's `Cargo.toml` forbids linking DuckDB ("this
//! binary must not inherit the unbundled libduckdb constraint across a fourth
//! build path"). `sauron-tier` already links it, already owns the hot/cold
//! watermark the purge boundary is derived from, and already runs a
//! claim/lease loop of exactly this shape.
//!
//! ## Two loops, not one
//!
//! Counting a preview and executing a purge are separate claim slots, with
//! separate partial indexes behind them. One FIFO would let a multi-hour purge
//! starve every preview past its TTL, making confirm permanently impossible on
//! a busy app — the same starvation the mask table documents.
//!
//! ## Phase order is load-bearing
//!
//! `delete` runs every RAW kind to completion before `recompute` starts.
//! Recompute reads what SURVIVES, so interleaving would repair a rollup against
//! rows a later batch then deletes, leaving counters wrong in a way no
//! subsequent pass corrects.

use std::time::Duration;

use chrono::{DateTime, Utc};
use sauron_core::Config;
use sauron_db::purge::{self as purge_repo, Scope};
use sauron_db::{conn, repo, AsyncPgConnection, PgPool};
use sauron_purge::recompute::{Counts, SourceCounts};
use sauron_purge::{Class, PurgeKind};
use sauron_tier::duck::DuckEngine;
use sauron_tier::layout::cold_partition_glob;
use serde_json::{json, Map, Value};
use tracing::{info, warn};
use uuid::Uuid;

/// The three partitioned raw tables, in the order the recompute sums them.
const RAW_TABLES: [(&str, PurgeKind); 3] = [
    ("analytics_events", PurgeKind::AnalyticsEvents),
    ("error_events", PurgeKind::ErrorEvents),
    ("transactions", PurgeKind::Transactions),
];

pub async fn purge_loop(pool: PgPool, cfg: Config) {
    // Distinct per process AND per restart: the worker-id fence on every flush
    // exists so a worker whose lease expired cannot double-count after coming
    // back, and reusing a stable id across restarts would defeat it.
    let worker_id = format!("sauron-purge-{}-{}", std::process::id(), Uuid::new_v4());
    info!(worker = %worker_id, "purge executor started");
    loop {
        // Counting first: a preview is short and its TTL is running.
        match run_one_count(&pool, &cfg, &worker_id).await {
            Ok(true) => continue,
            Ok(false) => {}
            Err(e) => warn!(error = %e, "purge preview failed"),
        }
        match run_one_purge(&pool, &cfg, &worker_id).await {
            Ok(true) => continue,
            Ok(false) => {}
            Err(e) => warn!(error = %e, "purge job failed"),
        }
        tokio::time::sleep(Duration::from_secs(cfg.restore_poll_secs)).await;
    }
}

/// The oldest instant a purge may delete from.
///
/// Reuses `sauron-inspector`'s `day_floor` reasoning verbatim: a floor computed
/// from `tier_hot_days` alone is NOT sufficient however long the window is,
/// because `sauron-tier` defers the partition DROP to a later cycle than the
/// export. The watermark plus one tier tick is the real boundary. Below it the
/// rows are either already gone from Postgres or on this very worker's critical
/// path.
///
/// Computed as the MINIMUM across the three tiered tables. They rotate
/// independently, so taking one table's watermark would let a purge of another
/// table reach into a range that table had already exported.
async fn cold_boundary(
    c: &mut AsyncPgConnection,
    cfg: &Config,
    now: DateTime<Utc>,
) -> anyhow::Result<DateTime<Utc>> {
    let hot_days = repo::effective_tier_hot_days(c, cfg.tier_hot_days).await?;
    let hot = now - chrono::Duration::days(hot_days);
    let mut boundary = hot;
    for (table, _) in RAW_TABLES {
        if let Some(w) = repo::get_watermark(c, table).await? {
            boundary = boundary.max(w + chrono::Duration::seconds(cfg.tier_tick_secs as i64));
        }
    }
    Ok(boundary)
}

fn kinds_of(job: &sauron_db::models::PurgeJob) -> Vec<PurgeKind> {
    job.kinds
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .filter_map(PurgeKind::parse)
                .collect()
        })
        .unwrap_or_default()
}

fn env_ids_of(job: &sauron_db::models::PurgeJob) -> Vec<Uuid> {
    job.environment_ids
        .as_ref()
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str())
                .filter_map(|s| Uuid::parse_str(s).ok())
                .collect()
        })
        .unwrap_or_default()
}

// ===========================================================================
// Counting
// ===========================================================================

/// Claim and count at most one preview. Returns whether one was claimed.
async fn run_one_count(pool: &PgPool, cfg: &Config, worker_id: &str) -> anyhow::Result<bool> {
    let mut c = conn(pool).await?;
    let Some(job) = purge_repo::claim_purge_job(
        &mut c,
        "previewing",
        "previewing",
        worker_id,
        cfg.purge_claim_stale_secs,
    )
    .await?
    else {
        return Ok(false);
    };

    let now = Utc::now();
    let boundary = cold_boundary(&mut c, cfg, now).await?;
    let kinds = kinds_of(&job);
    let env_ids = env_ids_of(&job);

    let mut estimated = Map::new();

    // A range entirely inside cold has no hot work at all. Reported as zeros
    // rather than refused, so the operator sees WHY nothing will be deleted
    // alongside the cold count that explains it.
    if let Some(scope) = Scope::from_job(&job, boundary) {
        for kind in &kinds {
            let n = match kind.class() {
                Class::Raw if *kind != PurgeKind::Inspector => {
                    purge_repo::count_raw_in_scope(&mut c, *kind, &scope).await?
                }
                Class::Raw => 0,
                Class::Rollup => purge_repo::count_rollup_contained(&mut c, *kind, &scope).await?,
            };
            estimated.insert(kind.slug().to_string(), json!(n));
        }
    } else {
        for kind in &kinds {
            estimated.insert(kind.slug().to_string(), json!(0));
        }
    }

    // The cold half: rows the operator asked for that will SURVIVE. The window
    // is [requested_start, boundary) — the part of the request that has already
    // rotated out of Postgres.
    let cold_from = job.range_start.unwrap_or(DateTime::<Utc>::MIN_UTC);
    let cold_skipped = if cold_from < boundary {
        let base = cfg.tier_cold_path.clone();
        let app = job.app_id;
        let envs = env_ids.clone();
        let wanted: Vec<&'static str> = RAW_TABLES
            .iter()
            .filter(|(_, k)| kinds.contains(k))
            .map(|(t, _)| *t)
            .collect();
        // DuckDB is synchronous; never block the runtime with it.
        tokio::task::spawn_blocking(move || -> anyhow::Result<i64> {
            if wanted.is_empty() {
                return Ok(0);
            }
            let eng = DuckEngine::open()?;
            let mut total = 0;
            for table in wanted {
                let glob = cold_partition_glob(&base, table, app);
                total += eng.count_in_purge_scope(&glob, app, &envs, cold_from, boundary)?;
            }
            Ok(total)
        })
        .await?
        // A cold read that fails must not fail the preview: the hot counts are
        // still correct and useful. Reported as zero with a warning, which is
        // the honest degradation — the alternative is no preview at all.
        .unwrap_or_else(|e| {
            warn!(error = %e, job = %job.id, "cold survivor count unavailable");
            0
        })
    } else {
        0
    };

    purge_repo::finish_preview(
        &mut c,
        job.id,
        worker_id,
        &Value::Object(estimated),
        cold_skipped,
        boundary,
    )
    .await?;
    info!(job = %job.id, cold_skipped, "purge preview counted");
    Ok(true)
}

// ===========================================================================
// Execution
// ===========================================================================

/// Claim and run at most one purge. Returns whether one was claimed.
async fn run_one_purge(pool: &PgPool, cfg: &Config, worker_id: &str) -> anyhow::Result<bool> {
    let mut c = conn(pool).await?;
    let Some(job) = purge_repo::claim_purge_job(
        &mut c,
        "pending",
        "running",
        worker_id,
        cfg.purge_claim_stale_secs,
    )
    .await?
    else {
        return Ok(false);
    };

    let now = Utc::now();
    let boundary = cold_boundary(&mut c, cfg, now).await?;
    let kinds = kinds_of(&job);

    // Recorded, never used to block. Recompute against live ingest drifts the
    // moment it is written; this makes a confusing result explainable
    // afterwards instead of a mystery.
    if let Ok(active) =
        purge_repo::app_ingest_active(&mut c, job.app_id, cfg.purge_ingest_active_secs).await
    {
        if active {
            warn!(job = %job.id, "app is still ingesting; recomputed counters may drift");
        }
    }

    let Some(scope) = Scope::from_job(&job, boundary) else {
        // Nothing hot to do. Finishing cleanly is right: the operator was told
        // at preview that everything in range was cold.
        purge_repo::finish_purge_job(&mut c, job.id, worker_id, job.cold_rows_skipped).await?;
        return Ok(true);
    };

    let result = execute(&mut c, cfg, &job, &scope, &kinds, worker_id).await;

    match result {
        Ok(()) => {
            purge_repo::finish_purge_job(&mut c, job.id, worker_id, job.cold_rows_skipped).await?;
            purge_repo::clear_touched_keys(&mut c, job.id).await?;
            info!(job = %job.id, "purge finished");
        }
        Err(e) => {
            warn!(job = %job.id, error = %e, "purge failed");
            purge_repo::fail_purge_job(&mut c, job.id, worker_id, &e.to_string()).await?;
        }
    }
    Ok(true)
}

/// Delete phase, then rollup deletes, then recompute.
async fn execute(
    c: &mut AsyncPgConnection,
    cfg: &Config,
    job: &sauron_db::models::PurgeJob,
    scope: &Scope,
    kinds: &[PurgeKind],
    worker_id: &str,
) -> anyhow::Result<()> {
    let ordered = sauron_purge::execution_order(kinds);

    // --- phase 1: raw deletions -------------------------------------------
    purge_repo::set_purge_phase(c, job.id, worker_id, "delete", None).await?;

    for kind in ordered.iter().filter(|k| k.is_raw()) {
        if *kind == PurgeKind::Inspector {
            purge_repo::delete_inspector_in_scope(c, scope, job.id, worker_id).await?;
            continue;
        }
        purge_repo::set_purge_phase(c, job.id, worker_id, "delete", Some(kind.slug())).await?;

        // Transactions move no counter, but they DO create rollup rows, so
        // their keys must still be harvested — otherwise a session whose only
        // signals were transactions survives as an orphan describing
        // occurrences that no longer exist.
        let mut cursor = None;
        loop {
            let Some(batch) = purge_repo::delete_raw_batch(
                c,
                *kind,
                scope,
                cursor,
                cfg.purge_batch_rows,
                job.id,
                worker_id,
                true,
            )
            .await?
            else {
                // Zero rows updated means the lease was stolen. Stop rather
                // than keep deleting under another worker's job.
                anyhow::bail!("lost the claim on this job (lease expired or stolen)");
            };

            if batch.status == "cancelling" {
                info!(job = %job.id, kind = kind.slug(), "purge cancelled mid-delete");
                return Ok(());
            }
            match batch.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
            tokio::time::sleep(Duration::from_millis(cfg.purge_batch_pause_ms)).await;
        }
    }

    // --- phase 2: fully-contained rollup rows ------------------------------
    // Before recompute: a row deleted here needs no repair, and repairing it
    // first would be wasted work on a row about to go.
    for kind in ordered.iter().filter(|k| !k.is_raw()) {
        purge_repo::delete_contained_rollups(c, *kind, scope, job.id, worker_id).await?;
    }

    // --- phase 3: recompute -------------------------------------------------
    // Independent of what the operator ticked: a rollup touched by a raw
    // deletion is repaired whether or not its own kind was selected.
    let raw_ticked: Vec<PurgeKind> = ordered.iter().copied().filter(|k| k.is_raw()).collect();
    let to_repair = sauron_purge::rollups_to_recompute(&raw_ticked);
    if to_repair.is_empty() {
        return Ok(());
    }
    purge_repo::set_purge_phase(c, job.id, worker_id, "recompute", None).await?;

    for kind in to_repair {
        purge_repo::set_purge_phase(c, job.id, worker_id, "recompute", Some(kind.slug())).await?;
        recompute_kind(c, cfg, job, kind, worker_id).await?;
    }
    Ok(())
}

/// Drain one rollup kind's touched keys, repairing or deleting each.
async fn recompute_kind(
    c: &mut AsyncPgConnection,
    cfg: &Config,
    job: &sauron_db::models::PurgeJob,
    kind: PurgeKind,
    worker_id: &str,
) -> anyhow::Result<()> {
    let Some(key_col) = purge_repo::rollup_key_column(kind) else {
        return Ok(());
    };
    let mut after: Option<String> = None;

    loop {
        let page = purge_repo::next_touched_keys(
            c,
            job.id,
            kind,
            after.as_deref(),
            cfg.purge_recompute_batch,
        )
        .await?;
        if page.is_empty() {
            return Ok(());
        }
        let keys: Vec<String> = page.iter().map(|k| k.key.clone()).collect();
        after = keys.last().cloned();

        // The cold half for the whole page, one DuckDB query per raw table.
        // Batched rather than per-key because the touched set reaches millions
        // on the purges this feature exists for.
        //
        // FAIL the job if the cold side could not be read. Continuing with the
        // hot half alone is the single most damaging thing this worker could
        // do: every recomputed counter would be short by the exported portion,
        // and any rollup whose only surviving evidence is cold would be DELETED
        // as empty. Both look like success. A failed job leaves the counters
        // overcounting, which is the state the operator already had and can
        // retry from.
        let Some(cold) = cold_counts_for_page(cfg, job.app_id, kind, key_col, &keys).await else {
            anyhow::bail!(
                "cold tier unreadable during recompute of '{}'; refusing to write \
                 counters that would be short by the exported rows",
                kind.slug()
            );
        };

        let mut recomputed = 0i64;
        let mut deleted = 0i64;
        for key in &keys {
            let hot = purge_repo::hot_counts_for_key(c, job.app_id, kind, key).await?;
            let hot_counts = Counts::from_sources(
                SourceCounts {
                    analytics: hot.analytics,
                    errors: hot.errors,
                    transactions: hot.transactions,
                },
                hot.first,
                hot.last,
            );
            // A key ABSENT from the map genuinely has no surviving cold rows —
            // `counts_by_key` omits keys it found nothing for. That is why the
            // unavailable case had to be distinguished above rather than folded
            // in here as another empty.
            let cold_counts = cold.get(key).copied().unwrap_or(Counts::EMPTY);
            let merged = Counts::merge(hot_counts, cold_counts);

            if purge_repo::apply_recomputed_rollup(c, kind, job.app_id, key, merged).await? {
                deleted += 1;
            } else {
                recomputed += 1;
            }
        }
        purge_repo::record_recompute_progress(c, job.id, worker_id, recomputed, deleted).await?;
        tokio::time::sleep(Duration::from_millis(cfg.purge_batch_pause_ms)).await;
    }
}

/// Cold counts for one page of keys, summed across the three raw tables.
///
/// Returns `None` when the cold side could not be read at all. That is
/// deliberately distinct from "all zeros": a caller must not silently treat an
/// unavailable cold tier as an empty one, because doing so would UNDERCOUNT
/// every rollup by the exported portion and, worse, delete rollups whose only
/// surviving evidence is cold.
async fn cold_counts_for_page(
    cfg: &Config,
    app_id: Uuid,
    kind: PurgeKind,
    key_col: &str,
    keys: &[String],
) -> Option<std::collections::HashMap<String, Counts>> {
    let base = cfg.tier_cold_path.clone();
    let keys = keys.to_vec();
    let key_col = key_col.to_string();

    let res = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let eng = DuckEngine::open()?;
        let mut out: std::collections::HashMap<String, Counts> = std::collections::HashMap::new();
        for (table, source) in RAW_TABLES {
            // Only probe tables that actually carry this key. `issue_id` lives
            // on error_events alone; asking analytics_events for it is a hard
            // error, not a zero — the same trap the hot half's `key_tables`
            // documents.
            if kind == PurgeKind::Issues && source != PurgeKind::ErrorEvents {
                continue;
            }
            let glob = cold_partition_glob(&base, table, app_id);
            for row in eng.counts_by_key(&glob, app_id, &key_col, &keys)? {
                let e = out.entry(row.key.clone()).or_insert(Counts::EMPTY);
                // Apply the SAME delta table the hot side uses: analytics feed
                // events, errors feed errors, transactions feed neither — but
                // all three feed `evidence`.
                match source {
                    PurgeKind::AnalyticsEvents => e.events += row.count,
                    PurgeKind::ErrorEvents => e.errors += row.count,
                    _ => {}
                }
                e.evidence += row.count;
                e.first = match (e.first, row.first) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (x, None) | (None, x) => x,
                };
                e.last = match (e.last, row.last) {
                    (Some(a), Some(b)) => Some(a.max(b)),
                    (x, None) | (None, x) => x,
                };
            }
        }
        Ok(out)
    })
    .await;

    match res {
        Ok(Ok(m)) => Some(m),
        Ok(Err(e)) => {
            warn!(error = %e, "cold recompute half unavailable");
            None
        }
        Err(e) => {
            warn!(error = %e, "cold recompute task panicked");
            None
        }
    }
}
