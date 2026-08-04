//! The ingest worker: a pool of tasks consuming the Redis stream consumer
//! group, processing each job, and acking (or dead-lettering) it.

use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinHandle;
use tracing::{error, info, warn};

use sauron_core::envelope::{IngestBatch, IngestJob};
use sauron_db::PgPool;
use sauron_redis::RedisStore;

use crate::mask::PolicyCache;
use crate::process::process_job;
use crate::symbolize::SymbolizeCtx;

/// How long an entry may sit unacked in another consumer's PEL before this
/// worker claims it. Covers a worker that died mid-job.
const RECLAIM_IDLE_MS: usize = 60_000;
/// How often a worker sweeps the pending-entries list.
const RECLAIM_EVERY: Duration = Duration::from_secs(30);
/// Backoff after a worker loop dies before it is restarted.
const RESPAWN_BACKOFF: Duration = Duration::from_secs(1);
/// Entries requested per `XREADGROUP`, and therefore the size of one batched
/// write. With `crate::batch` this is the single biggest lever on the write
/// path: every statement's fixed cost is amortized across this many entries.
///
/// **200, raised from the 50 this shipped with.** 50 dated from the per-item
/// write path, where it only bounded how long a worker went between Redis
/// reads and had no effect on how many statements ran. A 2-D sweep against
/// `WORKER_CONCURRENCY` measured 50 at less than half the throughput of 200 at
/// every concurrency tried; 500 was within noise of 200, and 2000 regressed.
///
/// Raising it further is not free — the whole batch is buffered in memory, one
/// bad row rolls back more work, and the bind-parameter ceiling (65535 per
/// statement, against ~30 columns for an error event) puts a hard limit near
/// 2000. Env overridable so it can be swept without a rebuild.
///
/// Since one entry became a whole envelope this is only the **ceiling** on a
/// read; what the worker actually asks for is derived from [`ReadSizer`] so the
/// batch is bounded in items rather than in entries.
fn read_count() -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("INGEST_BATCH_SIZE")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|n| (1..=2000).contains(n))
            .unwrap_or(200)
    })
}

/// Items a worker aims to hold in one batch.
///
/// The knob that matters is items, not entries — items are what occupy memory,
/// what bind parameters, and what the per-batch statement cost is amortized
/// across. While one entry was one item the distinction did not exist and
/// `INGEST_BATCH_SIZE` served as both.
///
/// Folding an envelope into one entry broke that. Measured at twice capacity,
/// where the stream is deep enough that every read returns a full 200 entries,
/// a 15-item envelope turned a 200-item batch into a 3,000-item one and took
/// the ingest's resident set from **214 MB to 2,164 MB** — a tenfold memory
/// regression bought by a knob nobody had touched, on a workload the operator
/// did not change.
fn batch_items() -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("INGEST_BATCH_ITEMS")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|n| (1..=20_000).contains(n))
            .unwrap_or(1_000)
    })
}

/// Converts the item budget into an entry count to ask `XREADGROUP` for.
///
/// The conversion factor is whatever this worker has actually been seeing,
/// because it is a property of the traffic: an SDK that does not batch sends
/// one item per envelope, one that batches hard may send hundreds, and the same
/// deployment carries both. A fixed divisor would be wrong for one of them.
///
/// Starts at one item per entry, which is both the legacy shape and the
/// conservative direction: the first read of a worker's life asks for the full
/// ceiling and then corrects, rather than under-reading forever if the estimate
/// starts too high.
struct ReadSizer {
    items_per_entry: f64,
}

impl ReadSizer {
    fn new() -> ReadSizer {
        ReadSizer {
            items_per_entry: 1.0,
        }
    }

    fn entries(&self) -> usize {
        let want = (batch_items() as f64 / self.items_per_entry).ceil() as usize;
        want.clamp(1, read_count())
    }

    /// Exponential moving average, weighted toward history so one unusually
    /// fat or thin envelope does not swing the next read.
    fn observe(&mut self, entries: usize, items: usize) {
        if entries == 0 {
            return;
        }
        let seen = items as f64 / entries as f64;
        self.items_per_entry = 0.75 * self.items_per_entry + 0.25 * seen.max(1.0);
    }
}

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
    policies: Arc<PolicyCache>,
) -> anyhow::Result<Vec<JoinHandle<()>>> {
    redis.ensure_group().await?;
    let mut handles = Vec::with_capacity(concurrency);
    for i in 0..concurrency.max(1) {
        let pool = pool.clone();
        let redis = redis.clone();
        let sym = sym.clone();
        let policies = policies.clone();
        let consumer = format!("worker-{i}");
        info!(consumer, "starting ingest worker");
        handles.push(tokio::spawn(async move {
            loop {
                worker_loop(
                    pool.clone(),
                    redis.clone(),
                    sym.clone(),
                    policies.clone(),
                    consumer.clone(),
                )
                .await;
                warn!(consumer, "ingest worker exited; restarting");
                tokio::time::sleep(RESPAWN_BACKOFF).await;
            }
        }));
    }
    Ok(handles)
}

async fn worker_loop(
    pool: PgPool,
    redis: RedisStore,
    sym: SymbolizeCtx,
    policies: Arc<PolicyCache>,
    consumer: String,
) {
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
    // Per worker, not shared: each one observes its own slice of the traffic
    // and there is nothing to gain from contending on a common estimate.
    let mut sizer = ReadSizer::new();
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
                    process_entries(&pool, &redis, &sym, &policies, &consumer, claimed).await;
                }
                Ok(_) => {}
                Err(e) => warn!(consumer, error = %e, "PEL reclaim failed"),
            }
        }

        let want = sizer.entries();
        let entries = match redis.read_group(&mut blocking, &consumer, want, 5000).await {
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

        let n = entries.len();
        let items = process_entries(&pool, &redis, &sym, &policies, &consumer, entries).await;
        sizer.observe(n, items);
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

/// Whether to write a read's worth of entries as one batch. Batching is the
/// point of `crate::batch`, but a single env switch back to the per-item path
/// is what makes the two comparable on the same binary — and is the escape
/// hatch if a batch-only defect ever surfaces in the field.
fn batching_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        !matches!(
            std::env::var("INGEST_BATCH_WRITES").as_deref(),
            Ok("0") | Ok("false") | Ok("off")
        )
    })
}

/// Process a batch of stream entries, acking or dead-lettering each. Returns
/// how many ITEMS those entries carried, which is what sizes the next read.
async fn process_entries(
    pool: &PgPool,
    redis: &RedisStore,
    sym: &SymbolizeCtx,
    policies: &PolicyCache,
    consumer: &str,
    entries: Vec<(String, String)>,
) -> usize {
    // Masking is per item; DECODING is now per entry, and an entry is a whole
    // envelope. A payload that fails to deserialize dead-letters raw here
    // rather than travelling into the batch — a small, permanent, named hole.
    // §1 of the design lists it.
    let mut decoded: Vec<crate::batch::Decoded> = Vec::with_capacity(entries.len());
    for (id, payload) in entries {
        let batch = match decode_entry(&payload) {
            Ok(b) => b,
            Err(e) => {
                warn!(consumer, id, error = %e, "malformed job; dead-lettering");
                let _ = redis.dead_letter(&id, &payload).await;
                continue;
            }
        };
        // The edge never enqueues an item-less envelope, but an entry that
        // somehow carries none would otherwise contribute nothing to `decoded`,
        // never be acked, and be reclaimed from the PEL forever.
        if batch.items.is_empty() {
            warn!(consumer, id, "entry carries no items; acking");
            let _ = redis.ack(&id).await;
            continue;
        }
        // Resolved ONCE per envelope now rather than once per item — every item
        // in an entry shares one `app_id` by construction.
        let masks = policies.get(batch.app_id).await;
        let jobs = batch.into_jobs();
        let last = jobs.len() - 1;
        for (i, mut job) in jobs.into_iter().enumerate() {
            // Mask the owned wire payload before anything is persisted or
            // re-queued.
            crate::mask::apply_wire(&masks, &mut job);
            decoded.push(crate::batch::Decoded {
                id: id.clone(),
                job,
                masks: masks.clone(),
                entry_tail: i == last,
            });
        }
    }
    let items = decoded.len();
    if decoded.is_empty() {
        return 0;
    }

    if batching_enabled() {
        // One id per ENTRY, not per item: `entry_tail` marks the last item of
        // each entry, so filtering on it both de-duplicates the ack list and
        // keeps it in stream order.
        let ids: Vec<String> = decoded
            .iter()
            .filter(|d| d.entry_tail)
            .map(|d| d.id.clone())
            .collect();
        // No defensive copy: `process_batch` BORROWS the batch, so `decoded` is
        // still owned here and the fallback can simply be handed the original.
        // The clone that used to stand in this spot duplicated every job and
        // every masked payload on the happy path to protect an arm that already
        // had access to them.
        match crate::batch::process_batch(pool, redis, sym, &decoded).await {
            Ok(()) => {
                if let Err(e) = redis.ack_many(&ids).await {
                    // Unacked entries stay in the PEL and are reclaimed after
                    // RECLAIM_IDLE_MS, so this costs a duplicate write, not a
                    // loss. Loud because duplicates are the visible symptom.
                    warn!(consumer, error = %e, n = ids.len(), "batch ack failed; entries will be redelivered");
                }
                return items;
            }
            Err(e) => {
                // One bad row fails the whole statement. Replay the batch
                // item-by-item so the offender dead-letters alone and its
                // neighbours still land.
                warn!(
                    consumer,
                    error = %e,
                    n = decoded.len(),
                    "batched write failed; falling back to per-item processing"
                );
                process_one_by_one(pool, redis, sym, consumer, decoded).await;
                return items;
            }
        }
    }

    process_one_by_one(pool, redis, sym, consumer, decoded).await;
    items
}

/// The original per-item path: one job at a time, acked or dead-lettered on its
/// own. Still the fallback for a failed batch, and still what
/// `INGEST_BATCH_WRITES=0` selects.
async fn process_one_by_one(
    pool: &PgPool,
    redis: &RedisStore,
    sym: &SymbolizeCtx,
    consumer: &str,
    decoded: Vec<crate::batch::Decoded>,
) {
    for d in decoded {
        // Serialized HERE rather than at decode time. `process_job` consumes
        // the job, so the payload must exist before the call — but ONLY this
        // path can dead-letter, and the batch path above never reaches it. Done
        // eagerly for every entry, this was a whole extra `to_string` per event
        // in the steady state, spent on an arm that almost never runs.
        //
        // Empty on failure, never the raw wire payload: the DLQ has no MAXLEN,
        // no TTL and no reaper, so anything written here is permanent and must
        // already be masked.
        let masked_payload = serde_json::to_string(&d.job).unwrap_or_default();
        if let Err(e) = process_job(pool, redis, sym, &d.masks, d.job).await {
            warn!(consumer, id = d.id, error = %e, "job processing failed; dead-lettering");
            // Deliberately NOT `dead_letter`, which acks: one failing item must
            // not retire the entry while its siblings are still unwritten. A
            // crash before the tail then replays the whole envelope — duplicate
            // writes rather than a silent partial loss.
            let _ = redis.dlq_push(&masked_payload).await;
        }
        // Acked once per ENTRY, after its last item — whether that item landed
        // or dead-lettered. Both outcomes are terminal for the item; what must
        // not happen is retiring the entry with items still to come.
        if d.entry_tail {
            let _ = redis.ack(&d.id).await;
        }
    }
}

/// Decode one stream entry into an envelope.
///
/// Tries the current shape first and falls back to the legacy single-item
/// [`IngestJob`]. The fallback is not decoration: the stream survives a deploy,
/// so entries written by the previous binary are still pending when the new one
/// starts reading, and the PEL can hand one back long afterwards. Decoding them
/// as malformed would dead-letter real, already-accepted telemetry.
///
/// The two shapes cannot be confused — `items` and `item` are both required and
/// neither struct accepts the other's — so the fallback costs a second parse
/// only on entries that genuinely are the old shape.
fn decode_entry(payload: &str) -> Result<IngestBatch, serde_json::Error> {
    match serde_json::from_str::<IngestBatch>(payload) {
        Ok(b) => Ok(b),
        Err(current) => match serde_json::from_str::<IngestJob>(payload) {
            Ok(j) => Ok(IngestBatch::from(j)),
            // Report the CURRENT shape's error. A genuinely malformed payload
            // fails both, and the legacy parser's complaint would send whoever
            // reads the log looking at a struct the edge no longer writes.
            Err(_) => Err(current),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = r#""app_id":"00000000-0000-0000-0000-000000000001",
        "project_id":"00000000-0000-0000-0000-000000000002",
        "org_id":"00000000-0000-0000-0000-000000000003",
        "environment_id":"00000000-0000-0000-0000-000000000004",
        "received_at":"2026-07-29T00:00:00Z""#;
    const ITEM: &str = r#"{"type":"event","name":"a","distinct_id":"u1",
        "timestamp":"2026-07-29T00:00:00Z"}"#;

    #[test]
    fn decodes_the_current_envelope_shape() {
        let b = decode_entry(&format!("{{{HEADER},\"items\":[{ITEM},{ITEM}]}}"))
            .expect("current shape decodes");
        assert_eq!(b.items.len(), 2);
    }

    /// The stream outlives a deploy. An entry written by the previous binary is
    /// still pending when the new one starts reading — and the PEL can hand one
    /// back long after that. Decoding it as malformed would dead-letter real,
    /// already-accepted telemetry, permanently: the DLQ has no reaper.
    #[test]
    fn decodes_a_legacy_single_item_entry() {
        let b = decode_entry(&format!("{{{HEADER},\"item\":{ITEM}}}"))
            .expect("legacy shape must still decode");
        assert_eq!(b.items.len(), 1);
        assert_eq!(b.app_id, uuid::Uuid::from_u128(1));
    }

    /// The sizer must converge on an ENTRY count that delivers roughly the item
    /// budget, whatever the envelope size is. This is the guard on the tenfold
    /// memory regression that folding envelopes into entries introduced: with a
    /// fixed entry count, 15-item envelopes silently made every batch 15x
    /// bigger than the one the default was measured against.
    #[test]
    fn the_read_size_converges_on_the_item_budget() {
        let budget = batch_items();
        let ceiling = read_count();

        // First read of a worker's life: no evidence yet, so ask for the
        // ceiling rather than guess low and stay there.
        let mut s = ReadSizer::new();
        assert_eq!(s.entries(), budget.min(ceiling));

        // Fat envelopes. Feed it what it would actually see and let it settle.
        for _ in 0..40 {
            let n = s.entries();
            s.observe(n, n * 15);
        }
        let fat = s.entries();
        assert!(
            (fat as f64 * 15.0) < budget as f64 * 1.35,
            "15-item envelopes should settle near the item budget, got {fat} entries \
             = {} items against a budget of {budget}",
            fat * 15
        );

        // One item per entry — the legacy shape, and an SDK that does not
        // batch. Must climb back to the ceiling rather than stay small.
        for _ in 0..40 {
            let n = s.entries();
            s.observe(n, n);
        }
        assert_eq!(s.entries(), budget.min(ceiling));

        // A read that returned nothing must not divide by zero or poison the
        // estimate.
        let before = s.entries();
        s.observe(0, 0);
        assert_eq!(s.entries(), before);
    }

    #[test]
    fn genuinely_malformed_payloads_still_fail() {
        assert!(decode_entry("not json").is_err());
        // Parses as JSON, but is neither shape — no `item`, no `items`.
        assert!(decode_entry(&format!("{{{HEADER}}}")).is_err());
    }
}
