//! The ingest worker: a pool of tasks consuming the Redis stream consumer
//! group, processing each job, and acking (or dead-lettering) it.

use tokio::task::JoinHandle;
use tracing::{info, warn};

use sauron_core::envelope::IngestJob;
use sauron_db::PgPool;
use sauron_redis::RedisStore;

use crate::process::process_job;

/// Spawn `concurrency` worker tasks. Returns their handles; the caller keeps
/// them alive for the process lifetime.
pub async fn spawn_workers(
    pool: PgPool,
    redis: RedisStore,
    concurrency: usize,
) -> anyhow::Result<Vec<JoinHandle<()>>> {
    redis.ensure_group().await?;
    let mut handles = Vec::with_capacity(concurrency);
    for i in 0..concurrency.max(1) {
        let pool = pool.clone();
        let redis = redis.clone();
        let consumer = format!("worker-{i}");
        info!(consumer, "starting ingest worker");
        handles.push(tokio::spawn(worker_loop(pool, redis, consumer)));
    }
    Ok(handles)
}

async fn worker_loop(pool: PgPool, redis: RedisStore, consumer: String) {
    // Each worker owns a dedicated blocking connection so its BLOCK read never
    // stalls the shared command path.
    let mut blocking = match redis.blocking_connection().await {
        Ok(c) => c,
        Err(e) => {
            warn!(consumer, error = %e, "could not open blocking connection; worker exiting");
            return;
        }
    };

    loop {
        let entries = match redis.read_group(&mut blocking, &consumer, 50, 5000).await {
            Ok(entries) => entries,
            Err(e) => {
                warn!(consumer, error = %e, "stream read failed; backing off");
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                continue;
            }
        };

        for (id, payload) in entries {
            match serde_json::from_str::<IngestJob>(&payload) {
                Ok(job) => match process_job(&pool, &redis, job).await {
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
}
