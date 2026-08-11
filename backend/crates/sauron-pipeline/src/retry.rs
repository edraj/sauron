//! The backoff tier: parking a transiently-failed job and putting it back.
//!
//! Before this existed a job that failed was dead-lettered on its FIRST
//! attempt, so a two-second Postgres hiccup permanently lost every event in
//! flight while the edge had already answered `202`.
//!
//! Jobs wait in a Redis sorted set scored by the instant they become due, and
//! worker-0's existing 30s tick puts the due ones back on the ingest stream.
//! Nothing waits in process: an in-process sleep would lose every in-flight
//! backoff on deploy, and would race `RECLAIM_IDLE_MS` — which is also 60s —
//! for ownership of the same entry.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::{info, warn};

use sauron_redis::RedisStore;

/// Wait between attempts.
///
/// Realised granularity is the worker tick, not this number: the drain runs
/// every 30s, so a job parked for 60s comes back in 60–90s. Named honestly
/// rather than documented as exact, because a test asserting 60s precision
/// would be asserting something the loop cannot deliver.
pub const RETRY_BACKOFF_SECS: i64 = 60;

/// Ceiling on jobs re-injected per tick.
///
/// A mass transient failure — a Postgres restart, say — can park tens of
/// thousands of jobs in one minute. Without a cap the first tick afterwards
/// would re-inject all of them at once and re-create the outage it is
/// recovering from.
pub const RETRY_DRAIN_LIMIT: usize = 500;

/// How long a job's attempt counter outlives its last failure.
///
/// Comfortably longer than `MAX_ATTEMPTS × RETRY_BACKOFF_SECS` so a counter
/// never expires mid-sequence and hands a job a fresh set of retries, and short
/// enough that counters for jobs that eventually succeeded do not accumulate.
pub const ATTEMPT_TTL_SECS: u64 = 900;

/// One parked job.
///
/// The `nonce` exists solely so two byte-identical payloads occupy two distinct
/// members. A sorted set de-duplicates by member, so without it the second of
/// two identical failing events would silently overwrite the first and one of
/// them would never be retried — a silent loss that no counter would show.
#[derive(Debug, Serialize, Deserialize)]
struct Parked {
    nonce: String,
    payload: String,
}

/// A stable handle for a job's bytes, used to key its attempt counter.
pub fn job_hash(payload: &str) -> String {
    let mut h = Sha256::new();
    h.update(payload.as_bytes());
    hex::encode(&h.finalize()[..16])
}

/// Park a job to be retried after [`RETRY_BACKOFF_SECS`].
///
/// The caller MUST have acked the stream entry first — see
/// [`RedisStore::retry_schedule`] for what happens if it has not.
pub async fn park(redis: &RedisStore, payload: &str, now_ms: i64) -> anyhow::Result<()> {
    park_to(redis, sauron_redis::keys::INGEST_RETRY, payload, now_ms).await
}

/// [`park`] against an arbitrary backoff set.
///
/// Keys are parameters here for the same reason `stream_stats` takes them:
/// tests must drive the real code path without touching the live set, and two
/// concurrent tests sharing one key would silently see each other's jobs — a
/// race that passes when run alone and fails under load.
pub async fn park_to(
    redis: &RedisStore,
    retry_key: &str,
    payload: &str,
    now_ms: i64,
) -> anyhow::Result<()> {
    let parked = Parked {
        nonce: uuid::Uuid::new_v4().to_string(),
        payload: payload.to_string(),
    };
    let member = serde_json::to_string(&parked)?;
    redis
        .retry_schedule_to(retry_key, &member, now_ms + RETRY_BACKOFF_SECS * 1000)
        .await
}

/// Put every due job back on the ingest stream. Returns how many moved.
///
/// `XADD` first, `ZREM` only after it succeeds. A crash between the two yields
/// a duplicate event, never a lost one, and for an ingest pipeline that is the
/// correct side to fail toward — a duplicate is visible and de-duplicable
/// downstream, a loss is neither.
pub async fn drain_due(redis: &RedisStore, now_ms: i64, stream_maxlen: usize) -> usize {
    drain_due_between(
        redis,
        sauron_redis::keys::INGEST_RETRY,
        sauron_redis::keys::INGEST_STREAM,
        now_ms,
        stream_maxlen,
    )
    .await
}

/// [`drain_due`] between an arbitrary backoff set and stream — see [`park_to`].
pub async fn drain_due_between(
    redis: &RedisStore,
    retry_key: &str,
    stream_key: &str,
    now_ms: i64,
    stream_maxlen: usize,
) -> usize {
    let due = match redis
        .retry_due_from(retry_key, now_ms, RETRY_DRAIN_LIMIT)
        .await
    {
        Ok(d) if d.is_empty() => return 0,
        Ok(d) => d,
        Err(e) => {
            warn!(error = %e, "retry drain read failed");
            return 0;
        }
    };

    let total = due.len();
    let mut moved = 0usize;
    for member in due {
        let parked: Parked = match serde_json::from_str(&member) {
            Ok(p) => p,
            Err(e) => {
                // Unparseable members can only come from a previous binary with
                // a different shape. Dropping is correct — leaving it would
                // make it a permanent resident that every tick re-reads and
                // re-fails, crowding out the drain limit forever.
                warn!(error = %e, "discarding unparseable retry member");
                let _ = redis.retry_forget_from(retry_key, &member).await;
                continue;
            }
        };
        match redis
            .xadd_job_to(stream_key, &parked.payload, stream_maxlen)
            .await
        {
            Ok(_) => {
                if let Err(e) = redis.retry_forget_from(retry_key, &member).await {
                    // The job is already back on the stream, so this costs a
                    // duplicate on the next tick, not a loss.
                    warn!(error = %e, "retry re-enqueued but not removed; will duplicate");
                }
                moved += 1;
            }
            Err(e) => {
                // Left in the set deliberately: the next tick tries again.
                warn!(error = %e, "retry re-enqueue failed; job stays parked");
            }
        }
    }

    if moved > 0 {
        info!(moved, total, "re-enqueued due retries");
    }
    if total == RETRY_DRAIN_LIMIT {
        // Never silent: a drain that hit its ceiling has left work behind, and
        // a quiet full-limit drain reads exactly like a quiet empty one.
        info!(
            limit = RETRY_DRAIN_LIMIT,
            "retry drain hit its per-tick limit; more remain"
        );
    }
    moved
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The de-duplication trap: a ZSET keys on the member, so two identical
    /// payloads must not serialize to the same member or one is silently lost.
    #[test]
    fn identical_payloads_park_as_distinct_members() {
        let a = Parked {
            nonce: uuid::Uuid::new_v4().to_string(),
            payload: "{\"same\":1}".into(),
        };
        let b = Parked {
            nonce: uuid::Uuid::new_v4().to_string(),
            payload: "{\"same\":1}".into(),
        };
        assert_ne!(
            serde_json::to_string(&a).unwrap(),
            serde_json::to_string(&b).unwrap(),
            "identical payloads must occupy distinct ZSET members"
        );
    }

    #[test]
    fn job_hash_is_stable_and_distinguishing() {
        assert_eq!(job_hash("{\"a\":1}"), job_hash("{\"a\":1}"));
        assert_ne!(job_hash("{\"a\":1}"), job_hash("{\"a\":2}"));
    }

    /// The counter must outlive a full retry sequence, or an expiry mid-flight
    /// silently grants the job a fresh set of attempts and the loop never ends.
    #[test]
    fn attempt_ttl_outlives_a_full_retry_sequence() {
        let sequence = (crate::classify::MAX_ATTEMPTS as i64) * RETRY_BACKOFF_SECS;
        assert!(
            (ATTEMPT_TTL_SECS as i64) > sequence * 2,
            "TTL {ATTEMPT_TTL_SECS}s must comfortably exceed the {sequence}s sequence"
        );
    }
}
