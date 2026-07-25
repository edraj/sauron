//! The ingest worker: a pool of tasks consuming the Redis stream consumer
//! group, processing each job, and acking (or dead-lettering) it.

use std::time::Duration;

use tokio::task::JoinHandle;
use tracing::{error, info, warn};

use sauron_core::envelope::IngestJob;
use sauron_db::PgPool;
use sauron_redis::RedisStore;

use crate::process::process_job;
use crate::symbolize::SymbolizeCtx;

/// How long an entry may sit unacked in another consumer's PEL before this
/// worker claims it. Covers a worker that died mid-job.
const RECLAIM_IDLE_MS: usize = 60_000;
/// How often a worker sweeps the pending-entries list.
const RECLAIM_EVERY: Duration = Duration::from_secs(30);
/// Backoff after a worker loop dies before it is restarted.
const RESPAWN_BACKOFF: Duration = Duration::from_secs(1);

/// Spawn `concurrency` supervised worker tasks. Returns their handles; the
/// caller keeps them alive for the process lifetime.
///
/// Each handle is a *supervisor* that restarts its worker loop if it ever
/// returns. Previously a worker that hit an unrecoverable Redis error simply
/// returned and was never replaced — the pool silently shrank toward zero and
/// ingest stopped draining with no outward signal.
pub async fn spawn_workers(
    pool: PgPool,
    redis: RedisStore,
    concurrency: usize,
    sym: SymbolizeCtx,
) -> anyhow::Result<Vec<JoinHandle<()>>> {
    redis.ensure_group().await?;
    let mut handles = Vec::with_capacity(concurrency);
    for i in 0..concurrency.max(1) {
        let pool = pool.clone();
        let redis = redis.clone();
        let sym = sym.clone();
        let consumer = format!("worker-{i}");
        info!(consumer, "starting ingest worker");
        handles.push(tokio::spawn(async move {
            loop {
                worker_loop(pool.clone(), redis.clone(), sym.clone(), consumer.clone()).await;
                warn!(consumer, "ingest worker exited; restarting");
                tokio::time::sleep(RESPAWN_BACKOFF).await;
            }
        }));
    }
    Ok(handles)
}

async fn worker_loop(pool: PgPool, redis: RedisStore, sym: SymbolizeCtx, consumer: String) {
    // Each worker owns a dedicated blocking connection so its BLOCK read never
    // stalls the shared command path.
    let mut blocking = match redis.blocking_connection().await {
        Ok(c) => c,
        Err(e) => {
            warn!(consumer, error = %e, "could not open blocking connection; worker exiting");
            return;
        }
    };

    let mut last_reclaim = tokio::time::Instant::now();
    loop {
        // Periodically adopt entries abandoned by a dead worker. Without this
        // they stay in the consumer group's pending-entries list forever: they
        // are neither redelivered (reads use ">") nor acked, so those events are
        // silently lost and the PEL grows without bound.
        if last_reclaim.elapsed() >= RECLAIM_EVERY {
            last_reclaim = tokio::time::Instant::now();
            match redis.claim_stale(&consumer, RECLAIM_IDLE_MS, 50).await {
                Ok(claimed) if !claimed.is_empty() => {
                    info!(
                        consumer,
                        n = claimed.len(),
                        "reclaimed stale stream entries"
                    );
                    process_entries(&pool, &redis, &sym, &consumer, claimed).await;
                }
                Ok(_) => {}
                Err(e) => warn!(consumer, error = %e, "PEL reclaim failed"),
            }
        }

        let entries = match redis.read_group(&mut blocking, &consumer, 50, 5000).await {
            Ok(entries) => entries,
            Err(e) => {
                // A missing consumer group never heals on its own: every read
                // fails identically and the worker spins forever while producers
                // keep appending. This is what a Redis restart without
                // persistence (or a failover to a replica that lost the group)
                // looks like from here, so recreate it rather than stall ingest
                // indefinitely.
                if is_missing_group(&e) {
                    match redis.ensure_group().await {
                        Ok(()) => error!(
                            consumer,
                            "ingest consumer group was missing and has been recreated; \
                             entries appended while it was absent are not replayed"
                        ),
                        Err(e) => {
                            warn!(consumer, error = %e, "could not recreate consumer group")
                        }
                    }
                } else {
                    // The blocking handle is a `MultiplexedConnection`, which
                    // does NOT re-establish itself. Once its socket dies —
                    // a Redis restart, a failover, an idle-timeout reap — every
                    // subsequent read fails identically ("broken pipe") and the
                    // worker spins here forever. The supervisor in
                    // `spawn_workers` cannot help: the loop never returns, so it
                    // is never respawned. Meanwhile the gateway keeps accepting
                    // envelopes with 202 and the stream grows unbounded, so
                    // ingest silently stops persisting with no outward signal.
                    // Reconnect instead of reusing the dead handle.
                    warn!(consumer, error = %e, "stream read failed; reconnecting");
                    match redis.blocking_connection().await {
                        Ok(fresh) => {
                            blocking = fresh;
                            info!(consumer, "reopened blocking connection after read failure");
                        }
                        Err(e) => {
                            warn!(consumer, error = %e, "could not reopen blocking connection")
                        }
                    }
                }
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                continue;
            }
        };

        process_entries(&pool, &redis, &sym, &consumer, entries).await;
    }
}

/// Whether a stream error means the consumer group (or stream) is gone.
///
/// Redis reports this as a `NOGROUP` error code; the message text is matched as
/// a fallback for client versions that do not surface the code.
fn is_missing_group(e: &anyhow::Error) -> bool {
    let msg = e.to_string();
    msg.contains("NOGROUP") || msg.contains("No such key")
}

/// Process a batch of stream entries, acking or dead-lettering each.
async fn process_entries(
    pool: &PgPool,
    redis: &RedisStore,
    sym: &SymbolizeCtx,
    consumer: &str,
    entries: Vec<(String, String)>,
) {
    for (id, payload) in entries {
        match serde_json::from_str::<IngestJob>(&payload) {
            Ok(job) => match process_job(pool, redis, sym, job).await {
                Ok(()) => {
                    let _ = redis.ack(&id).await;
                }
                Err(e) => {
                    warn!(consumer, id, error = %e, "job processing failed; dead-lettering");
                    let _ = redis.dead_letter(&id, &payload).await;
                }
            },
            Err(e) => {
                warn!(consumer, id, error = %e, "malformed job; dead-lettering");
                let _ = redis.dead_letter(&id, &payload).await;
            }
        }
    }
}
