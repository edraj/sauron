//! The rollup fold task: folds newly-committed firehose rows into the
//! dashboard rollup tables every `ROLLUP_FOLD_SECS`, immediately on an
//! operator kick, and runs the daily consistency/prune maintenance.
//!
//! # Coordination
//!
//! Correctness never depends on this task being a singleton: every fold
//! transaction takes `pg_advisory_xact_lock` and advances its watermark in
//! the same transaction, so two replicas folding concurrently serialize and
//! never double-count. The Redis leader key exists purely so N replicas don't
//! burn N× the work — and therefore a DEAD Redis demotes to "everyone folds"
//! rather than "nobody folds": availability of fresh dashboards beats saving
//! duplicate cycles.
//!
//! # The kick key
//!
//! `POST /v1/apps/{app}/rollups/refresh` sets [`KICK_KEY`]; the task polls it
//! every tick (there is no pub/sub in `sauron-redis`, and the house pattern
//! for api→ingest signalling is a Redis key — see the DSN-cache DEL). A
//! kicked fold uses the much shorter `rollup_kick_lag_secs` so "Refresh"
//! really means now; the daily consistency check is the net under the
//! slightly riskier lag.

use std::time::{Duration as StdDuration, Instant};

use chrono::{Duration, NaiveDate, Utc};
use tokio::task::JoinHandle;
use tracing::{info, warn};

use sauron_db::{rollups, PgPool};
use sauron_redis::RedisStore;

pub use sauron_db::rollups::KICK_KEY;
const LEADER_KEY: &str = "sauron:rollups:leader";
const LEADER_TTL_SECS: u64 = 90;
const TICK: StdDuration = StdDuration::from_secs(2);
/// Bound on drain iterations per cycle: 20 × FOLD_MAX_ROWS = 10M rows, after
/// which the next cycle continues — a backlog cannot wedge the loop forever.
const MAX_DRAIN_PASSES: usize = 20;

#[derive(Clone, Copy)]
pub struct RollupCfg {
    pub fold_secs: u64,
    pub lag_secs: i64,
    pub kick_lag_secs: i64,
    pub name_cap: usize,
    /// Days of raw `sessions` rows to keep; `0` = keep forever. Resolved
    /// against the `sessions.retention_days` runtime override each pass.
    pub session_retention_days: i64,
}

pub fn spawn_rollup_task(pool: PgPool, redis: RedisStore, cfg: RollupCfg) -> JoinHandle<()> {
    tokio::spawn(async move {
        let me = uuid::Uuid::new_v4().to_string();
        let mut last_fold = Instant::now();
        let mut maintained_on: Option<NaiveDate> = None;
        info!(fold_secs = cfg.fold_secs, "rollup fold task running");
        loop {
            tokio::time::sleep(TICK).await;
            // Kicks are honored regardless of leadership: they are explicit,
            // rare, and correctness is the advisory lock's job — while a
            // just-restarted replica would otherwise ignore the Refresh
            // button for up to LEADER_TTL_SECS until the dead holder's key
            // expires (observed live). Only the SCHEDULED cadence is
            // leader-gated, since that is where N replicas would burn N×.
            let lead = acquire_leader(&redis, &me).await;
            let kicked = take_kick(&redis).await;
            let due = lead && last_fold.elapsed() >= StdDuration::from_secs(cfg.fold_secs);
            if !kicked && !due {
                continue;
            }
            let lag = if kicked {
                cfg.kick_lag_secs
            } else {
                cfg.lag_secs
            };
            let upto = Utc::now() - Duration::seconds(lag.max(0));
            last_fold = Instant::now();
            run_cycle(&pool, upto, cfg.name_cap).await;
            let today = Utc::now().date_naive();
            if maintained_on != Some(today) {
                maintenance(&pool, cfg).await;
                maintained_on = Some(today);
            }
        }
    })
}

/// Leader for efficiency only (see module docs): hold or take the key; on any
/// Redis failure act as leader so folding never stops with Redis.
async fn acquire_leader(redis: &RedisStore, me: &str) -> bool {
    match redis.set_nx_ex(LEADER_KEY, me, LEADER_TTL_SECS).await {
        Ok(true) => true,
        Ok(false) => match redis.get(LEADER_KEY).await {
            Ok(Some(v)) if v == me => {
                let _ = redis.set_ex(LEADER_KEY, me, LEADER_TTL_SECS).await;
                true
            }
            Ok(_) => false,
            Err(e) => {
                warn!(error = %e, "rollup leader read failed; folding anyway");
                true
            }
        },
        Err(e) => {
            warn!(error = %e, "rollup leader acquire failed; folding anyway");
            true
        }
    }
}

async fn take_kick(redis: &RedisStore) -> bool {
    match redis.get(KICK_KEY).await {
        Ok(Some(_)) => {
            let _ = redis.del(KICK_KEY).await;
            true
        }
        _ => false,
    }
}

async fn run_cycle(pool: &PgPool, upto: chrono::DateTime<Utc>, name_cap: usize) {
    let mut conn = match sauron_db::conn(pool).await {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "rollup fold: no database connection");
            return;
        }
    };
    for _ in 0..MAX_DRAIN_PASSES {
        match rollups::fold::fold_analytics(&mut conn, upto, name_cap).await {
            Ok(Some(o)) if !o.caught_up => continue,
            Ok(_) => break,
            Err(e) => {
                warn!(error = %e, "analytics fold failed");
                break;
            }
        }
    }
    for _ in 0..MAX_DRAIN_PASSES {
        match rollups::fold::fold_errors(&mut conn, upto).await {
            Ok(Some(o)) if !o.caught_up => continue,
            Ok(_) => break,
            Err(e) => {
                warn!(error = %e, "error fold failed");
                break;
            }
        }
    }
    for _ in 0..MAX_DRAIN_PASSES {
        match rollups::fold::fold_transactions(&mut conn, upto, name_cap).await {
            Ok(Some(o)) if !o.caught_up => continue,
            Ok(_) => break,
            Err(e) => {
                warn!(error = %e, "transaction fold failed");
                break;
            }
        }
    }
    if let Err(e) =
        rollups::fold::recompute_sessions(&mut conn, Some(Utc::now() - Duration::hours(36))).await
    {
        warn!(error = %e, "session recompute failed");
    }
}

/// Daily: compare yesterday's rollups against raw counts, rebuild drifted
/// days, prune dead state cursors. Skipped entirely while a backfill is still
/// pending — rebuilding a day the backfill has not covered would write that
/// day with `received_upto = ∞` and the later backfill would double-add it.
async fn maintenance(pool: &PgPool, cfg: RollupCfg) {
    let name_cap = cfg.name_cap;
    let mut conn = match sauron_db::conn(pool).await {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "rollup maintenance: no database connection");
            return;
        }
    };
    match rollups::backfill_pending(&mut conn).await {
        Ok(true) => {
            info!("rollup maintenance skipped: backfill pending (run `sauron-migrate backfill-rollups`)");
            return;
        }
        Ok(false) => {}
        Err(e) => {
            warn!(error = %e, "rollup maintenance: backfill-pending probe failed");
            return;
        }
    }
    match rollups::fold::consistency_check_trailing(&mut conn).await {
        Ok(drifts) if drifts.is_empty() => info!("rollup consistency: clean"),
        Ok(drifts) => match rollups::fold::tier_dropped_floor(&mut conn).await {
            // Fail closed: with the tier boundary unknown, a rebuild could hit
            // a day whose raw rows are cold-only Parquet and wipe its rollups.
            // Deferring costs a day — the sweep re-detects tomorrow.
            Err(e) => warn!(
                error = %e,
                deferred = drifts.len(),
                "tier floor probe failed; deferring day rebuilds"
            ),
            Ok(floor) => {
                for (day, what) in drifts {
                    // Belt to the sweep's own clamp: a partition can be
                    // dropped between the check and this rebuild.
                    let start = day.and_hms_opt(0, 0, 0).expect("valid").and_utc();
                    if floor.is_some_and(|f| start < f) {
                        warn!(day = %day, drift = %what, "drift on a tiered-out day; NOT rebuilding (raw is cold-only)");
                        continue;
                    }
                    warn!(day = %day, drift = %what, "rollup drift detected; rebuilding day");
                    if let Err(e) =
                        rollups::fold::fold_day_from_raw(&mut conn, day, None, true, name_cap).await
                    {
                        warn!(day = %day, error = %e, "rollup day rebuild failed");
                    }
                }
            }
        },
        Err(e) => warn!(error = %e, "rollup consistency check failed"),
    }
    match rollups::fold::ensure_session_partitions(&mut conn).await {
        Ok(n) if n > 0 => info!(created = n, "session partitions pre-created"),
        Ok(_) => {}
        Err(e) => warn!(error = %e, "session partition pre-create failed"),
    }
    match sauron_db::repo::effective_session_retention_days(&mut conn, cfg.session_retention_days)
        .await
    {
        Err(e) => warn!(error = %e, "session retention: settings probe failed; skipping this pass"),
        Ok(0) => {}
        Ok(days) => match rollups::fold::enforce_session_retention(&mut conn, days).await {
            Ok(0) => {}
            Ok(n) => info!(
                dropped = n,
                retention_days = days,
                "session partitions past retention dropped (aggregates remain in the rollups)"
            ),
            Err(e) => warn!(error = %e, "session retention failed"),
        },
    }
    match rollups::fold::duplicate_session_probe(&mut conn).await {
        Ok(0) => {}
        Ok(n) => warn!(dupes = n, "DUPLICATE sessions detected — the migration-73 advisory-lock write path has a regression"),
        Err(e) => warn!(error = %e, "duplicate-session probe failed"),
    }
    match rollups::fold::prune_state(&mut conn).await {
        Ok(n) if n > 0 => info!(pruned = n, "rollup state cursors pruned"),
        Ok(_) => {}
        Err(e) => warn!(error = %e, "rollup state prune failed"),
    }
}
