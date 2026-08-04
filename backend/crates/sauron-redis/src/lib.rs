//! `sauron-redis` — the only crate that talks to Redis.
//!
//! Wraps a cloneable multiplexed [`ConnectionManager`] and exposes exactly the
//! operations the ingest edge and worker need: a DSN→project cache, a
//! fixed-window rate limiter, the ingest stream (producer + consumer group),
//! a breadcrumb buffer, and HyperLogLog affected-user counters. Commands use
//! the low-level `redis::cmd` builder so they stay stable across redis-rs
//! versions.

use redis::aio::{ConnectionManager, ConnectionManagerConfig, MultiplexedConnection};
use redis::streams::{StreamReadOptions, StreamReadReply};
use redis::{AsyncCommands, AsyncConnectionConfig};

/// Redis key names / conventions in one place.
pub mod keys {
    use sha2::{Digest, Sha256};

    pub const INGEST_STREAM: &str = "sauron:ingest:stream";
    pub const INGEST_DLQ: &str = "sauron:ingest:dlq";
    pub const CONSUMER_GROUP: &str = "workers";

    /// Truncated SHA-256 of a DSN public key, so the credential itself never
    /// becomes a Redis key name (they show up in `KEYS`/`MONITOR` output and
    /// slow-log entries).
    ///
    /// Callers should not need this directly — [`dsn_cache`] and
    /// [`key_rate_limit`] apply it themselves. It is public only for callers
    /// that must build a related key.
    pub fn key_fingerprint(public_key: &str) -> String {
        let mut h = Sha256::new();
        h.update(public_key.as_bytes());
        hex::encode(&h.finalize()[..16])
    }

    /// Cache slot for a resolved ingest key. Takes the **raw** public key and
    /// fingerprints it internally.
    ///
    /// The `v2` segment is load-bearing. The cached value changed shape when the
    /// key moved from apps to environments; without a new prefix, entries written
    /// by the previous binary would deserialize into the wrong struct (or fail
    /// and silently fall through to Postgres) for the full 300s TTL after deploy.
    pub fn dsn_cache(public_key: &str) -> String {
        format!("sauron:dsn:v2:{}", key_fingerprint(public_key))
    }
    /// Per-DSN-key ingest rate-limit counter. Takes the **raw** public key.
    pub fn key_rate_limit(public_key: &str) -> String {
        format!("sauron:rl:key:{}", key_fingerprint(public_key))
    }
    pub fn rate_limit(project_id: &str) -> String {
        format!("sauron:rl:{project_id}")
    }
    pub fn breadcrumbs(project_id: &str, distinct_id: &str) -> String {
        format!("sauron:bc:{project_id}:{distinct_id}")
    }
    pub fn issue_users(issue_id: &str) -> String {
        format!("sauron:issue:{issue_id}:users")
    }
}

/// A single stream entry: `(stream_id, payload)`.
pub type StreamEntry = (String, String);

#[derive(Clone)]
pub struct RedisStore {
    conn: ConnectionManager,
    client: redis::Client,
}

impl RedisStore {
    pub async fn connect(url: &str) -> anyhow::Result<Self> {
        let client = redis::Client::open(url)?;
        // Disable the 500ms default response timeout: it is fatal for blocking
        // XREADGROUP and would spuriously fail large writes.
        let config = ConnectionManagerConfig::new().set_response_timeout(None);
        let conn = ConnectionManager::new_with_config(client.clone(), config).await?;
        Ok(Self { conn, client })
    }

    /// A fresh, dedicated multiplexed connection with no response timeout — used
    /// by each worker for its blocking XREADGROUP so the blocking read never
    /// stalls the shared command path.
    pub async fn blocking_connection(&self) -> anyhow::Result<MultiplexedConnection> {
        let config = AsyncConnectionConfig::new().set_response_timeout(None);
        let conn = self
            .client
            .get_multiplexed_async_connection_with_config(&config)
            .await?;
        Ok(conn)
    }

    // --- generic key/value ------------------------------------------------

    pub async fn get(&self, key: &str) -> anyhow::Result<Option<String>> {
        let mut c = self.conn.clone();
        let v: Option<String> = redis::cmd("GET").arg(key).query_async(&mut c).await?;
        Ok(v)
    }

    pub async fn set_ex(&self, key: &str, value: &str, ttl_secs: u64) -> anyhow::Result<()> {
        let mut c = self.conn.clone();
        redis::cmd("SET")
            .arg(key)
            .arg(value)
            .arg("EX")
            .arg(ttl_secs)
            .query_async::<()>(&mut c)
            .await?;
        Ok(())
    }

    pub async fn del(&self, key: &str) -> anyhow::Result<()> {
        let mut c = self.conn.clone();
        redis::cmd("DEL").arg(key).query_async::<()>(&mut c).await?;
        Ok(())
    }

    /// Liveness check for readiness probes.
    ///
    /// Ingest cannot accept a single envelope without Redis — the stream is the
    /// queue — so a readiness probe that only checks Postgres reports healthy
    /// while every request fails, and an orchestrator keeps routing to it.
    pub async fn ping(&self) -> anyhow::Result<()> {
        let mut c = self.conn.clone();
        redis::cmd("PING").query_async::<()>(&mut c).await?;
        Ok(())
    }

    /// `SET key value NX EX ttl` — atomically claim `key` for `ttl_secs` if it is
    /// not already set. Returns `true` when the caller won the claim (key was
    /// absent), `false` when it already existed. Used as a cross-process throttle
    /// / dedup guard for alert delivery.
    pub async fn set_nx_ex(&self, key: &str, value: &str, ttl_secs: u64) -> anyhow::Result<bool> {
        let mut c = self.conn.clone();
        // A successful SET NX replies +OK; a rejected one replies nil.
        let res: Option<String> = redis::cmd("SET")
            .arg(key)
            .arg(value)
            .arg("NX")
            .arg("EX")
            .arg(ttl_secs.max(1))
            .query_async(&mut c)
            .await?;
        Ok(res.is_some())
    }

    // --- rate limiting (fixed window) -------------------------------------

    /// Increment the per-project window counter and report whether the request
    /// is under `limit`. First hit in a window sets the expiry.
    pub async fn rate_limit_ok(
        &self,
        key: &str,
        limit: u32,
        window_secs: u64,
    ) -> anyhow::Result<bool> {
        let mut c = self.conn.clone();
        let count: i64 = redis::cmd("INCR").arg(key).query_async(&mut c).await?;
        if count == 1 {
            redis::cmd("EXPIRE")
                .arg(key)
                .arg(window_secs)
                .query_async::<()>(&mut c)
                .await?;
        }
        Ok(count as u64 <= limit as u64)
    }

    /// Apply several fixed-window counters in ONE round trip.
    ///
    /// The ingest edge checks two of these per request — one per key, one per
    /// app — and they were two sequential Redis waits because the second one's
    /// key was not known until the first had returned and the DSN had been
    /// resolved. Once the app is resolved from a process-local cache both keys
    /// are known up front, and nothing is decided between them, so the waits
    /// were the only thing two round trips bought.
    ///
    /// Every counter is incremented even when an earlier one is already over
    /// its limit. That is deliberate and matches the sequential version's
    /// observable behaviour closely enough: a fixed window counts attempts, and
    /// a rejected request having also counted against the app's window is the
    /// safe direction to err in.
    ///
    /// The expiry costs a second round trip, but only on the request that opens
    /// a window — one in `limit`, not one in one.
    pub async fn rate_limit_ok_many(
        &self,
        counters: &[(&str, u32)],
        window_secs: u64,
    ) -> anyhow::Result<Vec<bool>> {
        if counters.is_empty() {
            return Ok(Vec::new());
        }
        let mut c = self.conn.clone();
        let mut pipe = redis::pipe();
        for (key, _) in counters {
            pipe.cmd("INCR").arg(*key);
        }
        let counts: Vec<i64> = pipe.query_async(&mut c).await?;

        let fresh: Vec<&str> = counters
            .iter()
            .zip(&counts)
            .filter(|(_, n)| **n == 1)
            .map(|((key, _), _)| *key)
            .collect();
        if !fresh.is_empty() {
            let mut exp = redis::pipe();
            for key in &fresh {
                exp.cmd("EXPIRE").arg(*key).arg(window_secs);
            }
            // A counter that outlives its window would rate-limit the app
            // forever, so this failing is worth surfacing rather than ignoring
            // — the caller treats it the same way it treats any limiter error.
            exp.query_async::<()>(&mut c).await?;
        }

        Ok(counters
            .iter()
            .zip(counts)
            .map(|((_, limit), n)| n as u64 <= *limit as u64)
            .collect())
    }

    // --- ingest stream ----------------------------------------------------

    /// Enqueue a JSON job onto the ingest stream (trimmed to ~`maxlen`).
    pub async fn xadd_job(&self, payload: &str, maxlen: usize) -> anyhow::Result<String> {
        let mut ids = self
            .xadd_jobs(std::slice::from_ref(&payload), maxlen)
            .await?;
        // Exactly one command went out, so exactly one reply came back; the
        // per-entry error is flattened into the outer one for this shape.
        Ok(ids.remove(0)?)
    }

    /// Enqueue many jobs in ONE round trip.
    ///
    /// One `XADD` per payload, pipelined. The commands were always issued
    /// back-to-back with nothing decided in between, so this changes only how
    /// many times the caller waits for the network — an envelope carrying N
    /// items used to pay N sequential round trips.
    ///
    /// **The ingest edge no longer calls this with more than one payload.** It
    /// now enqueues an envelope as a single entry carrying every item, which
    /// subsumes the saving this method was written for and adds the ones
    /// pipelining could not reach: the envelope header is serialized, stored
    /// and parsed once instead of N times, and `MAXLEN` counts one entry rather
    /// than N. Kept because the many-payload shape is still the correct
    /// primitive for any caller that has genuinely independent jobs to enqueue.
    ///
    /// Deliberately NOT `.atomic()`: a plain pipeline is a batched send, not
    /// `MULTI`/`EXEC`, so entries from concurrent requests still interleave in
    /// the stream exactly as they did when this was a loop of awaits. The
    /// stream has no ordering requirement across envelopes, and making it a
    /// transaction would make a single bad entry discard the whole envelope.
    ///
    /// Returns one entry per payload, in order, so a caller can still report
    /// exactly which items were accepted: `ignore_errors` keeps a rejected
    /// `XADD` from discarding its neighbours' replies, which is what the
    /// sequential loop did. The outer `Err` is transport failure, where
    /// nothing is known to have landed.
    pub async fn xadd_jobs(
        &self,
        payloads: &[&str],
        maxlen: usize,
    ) -> anyhow::Result<Vec<redis::RedisResult<String>>> {
        if payloads.is_empty() {
            return Ok(Vec::new());
        }
        let mut pipe = redis::pipe();
        pipe.ignore_errors();
        for payload in payloads {
            pipe.cmd("XADD")
                .arg(keys::INGEST_STREAM)
                .arg("MAXLEN")
                .arg("~")
                .arg(maxlen)
                .arg("*")
                .arg("d")
                .arg(*payload);
        }
        let mut c = self.conn.clone();
        Ok(pipe.query_async(&mut c).await?)
    }

    /// Ensure the consumer group exists (idempotent; ignores BUSYGROUP).
    pub async fn ensure_group(&self) -> anyhow::Result<()> {
        let mut c = self.conn.clone();
        let res: redis::RedisResult<()> = redis::cmd("XGROUP")
            .arg("CREATE")
            .arg(keys::INGEST_STREAM)
            .arg(keys::CONSUMER_GROUP)
            .arg("$")
            .arg("MKSTREAM")
            .query_async(&mut c)
            .await;
        match res {
            Ok(()) => Ok(()),
            Err(e) if e.code() == Some("BUSYGROUP") => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// Read pending jobs for this consumer on a dedicated blocking connection,
    /// blocking up to `block_ms`.
    pub async fn read_group(
        &self,
        blocking: &mut MultiplexedConnection,
        consumer: &str,
        count: usize,
        block_ms: usize,
    ) -> anyhow::Result<Vec<StreamEntry>> {
        let opts = StreamReadOptions::default()
            .group(keys::CONSUMER_GROUP, consumer)
            .count(count)
            .block(block_ms);
        let reply: StreamReadReply = blocking
            .xread_options(&[keys::INGEST_STREAM], &[">"], &opts)
            .await?;

        let mut out = Vec::new();
        for key in reply.keys {
            for entry in key.ids {
                if let Some(v) = entry.map.get("d") {
                    let payload: String = redis::from_redis_value(v.clone()).unwrap_or_default();
                    out.push((entry.id, payload));
                }
            }
        }
        Ok(out)
    }

    /// Adopt entries that have sat unacknowledged in the consumer group's
    /// pending-entries list for longer than `min_idle_ms`.
    ///
    /// `read_group` always reads with `>` (never-delivered entries only), so an
    /// entry whose consumer died after delivery but before ack is never
    /// redelivered on its own — it would be stranded in the PEL forever. This
    /// `XAUTOCLAIM` sweep is what makes at-least-once delivery actually hold
    /// across a worker crash.
    pub async fn claim_stale(
        &self,
        consumer: &str,
        min_idle_ms: usize,
        count: usize,
    ) -> anyhow::Result<Vec<StreamEntry>> {
        let mut c = self.conn.clone();
        let reply: redis::streams::StreamAutoClaimReply = redis::cmd("XAUTOCLAIM")
            .arg(keys::INGEST_STREAM)
            .arg(keys::CONSUMER_GROUP)
            .arg(consumer)
            .arg(min_idle_ms)
            .arg("0-0")
            .arg("COUNT")
            .arg(count)
            .query_async(&mut c)
            .await?;

        let mut out = Vec::new();
        for entry in reply.claimed {
            if let Some(v) = entry.map.get("d") {
                let payload: String = redis::from_redis_value(v.clone()).unwrap_or_default();
                out.push((entry.id, payload));
            }
        }
        Ok(out)
    }

    /// Acknowledge a processed entry.
    pub async fn ack(&self, id: &str) -> anyhow::Result<()> {
        let mut c = self.conn.clone();
        redis::cmd("XACK")
            .arg(keys::INGEST_STREAM)
            .arg(keys::CONSUMER_GROUP)
            .arg(id)
            .query_async::<()>(&mut c)
            .await?;
        Ok(())
    }

    /// Acknowledge a whole batch. `XACK` is variadic, so N entries cost one
    /// round trip instead of N — which matters once the write path stops being
    /// the bottleneck and per-entry Redis chatter becomes visible.
    pub async fn ack_many(&self, ids: &[String]) -> anyhow::Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let mut c = self.conn.clone();
        let mut cmd = redis::cmd("XACK");
        cmd.arg(keys::INGEST_STREAM).arg(keys::CONSUMER_GROUP);
        for id in ids {
            cmd.arg(id);
        }
        cmd.query_async::<i64>(&mut c).await?;
        Ok(())
    }

    /// Dead-letter a permanently failing job, then ack it off the main stream.
    pub async fn dead_letter(&self, id: &str, payload: &str) -> anyhow::Result<()> {
        self.dlq_push(payload).await?;
        self.ack(id).await
    }

    /// Write to the dead-letter queue WITHOUT acking the stream entry.
    ///
    /// One stream entry now carries a whole envelope, so a single item failing
    /// must not retire the entry its untouched siblings are still waiting in.
    /// The caller acks once, after the last item of the entry has been handled;
    /// until then a crash costs a redelivery (duplicate writes) rather than
    /// silently dropping the remainder of the envelope.
    pub async fn dlq_push(&self, payload: &str) -> anyhow::Result<()> {
        let mut c = self.conn.clone();
        redis::cmd("XADD")
            .arg(keys::INGEST_DLQ)
            .arg("*")
            .arg("d")
            .arg(payload)
            .query_async::<()>(&mut c)
            .await?;
        Ok(())
    }

    // --- affected-user HyperLogLog ---------------------------------------

    /// Add a member, returning whether the estimate actually CHANGED.
    ///
    /// The bool is what makes the caller's follow-up `PFCOUNT` + `issues
    /// .users_seen` write skippable. `PFADD` replies 1 only when the register
    /// was modified, so once a person has been seen on an issue every later
    /// occurrence answers 0 and there is nothing to recompute. Discarding this
    /// meant re-writing an unchanged count on every single error event, and
    /// that `UPDATE issues` deadlocked against the issue upsert often enough to
    /// dominate the write path.
    pub async fn pf_add(&self, key: &str, member: &str) -> anyhow::Result<bool> {
        self.pf_add_many(key, std::slice::from_ref(&member)).await
    }

    /// The batched form. `PFADD` is variadic and replies 1 when ANY register
    /// moved, which is exactly the per-ISSUE signal the caller acts on — so a
    /// whole batch's members for one issue collapse into a single round trip
    /// without changing the answer. Members need not be distinct.
    ///
    /// Empty `members` returns false rather than issuing `PFADD key` with no
    /// arguments, which Redis rejects.
    pub async fn pf_add_many(&self, key: &str, members: &[&str]) -> anyhow::Result<bool> {
        if members.is_empty() {
            return Ok(false);
        }
        let mut c = self.conn.clone();
        let added: i64 = redis::cmd("PFADD")
            .arg(key)
            .arg(members)
            .query_async(&mut c)
            .await?;
        Ok(added == 1)
    }

    pub async fn pf_count(&self, key: &str) -> anyhow::Result<i64> {
        let mut c = self.conn.clone();
        let n: i64 = redis::cmd("PFCOUNT").arg(key).query_async(&mut c).await?;
        Ok(n)
    }

    // --- breadcrumb buffer ------------------------------------------------

    /// Push breadcrumbs (JSON) onto a capped, expiring per-person list.
    pub async fn push_breadcrumbs(
        &self,
        key: &str,
        json: &str,
        cap: isize,
        ttl_secs: u64,
    ) -> anyhow::Result<()> {
        // One round trip, not three. The three commands were always issued
        // back-to-back against the same key with nothing decided in between, so
        // pipelining changes only how many times the worker waits for the
        // network.
        //
        // Deliberately NOT `.atomic()`. A plain pipeline is a batched send, not
        // `MULTI`/`EXEC` — other clients still interleave between these three,
        // exactly as they could when this was three separate awaits. That keeps
        // the observable behaviour identical to what it replaced. Interleaving
        // is harmless here: concurrent pushes to one key just mean `LTRIM` runs
        // twice against the same cap, which is idempotent.
        let mut c = self.conn.clone();
        redis::pipe()
            .cmd("LPUSH")
            .arg(key)
            .arg(json)
            .ignore()
            .cmd("LTRIM")
            .arg(key)
            .arg(0)
            .arg(cap - 1)
            .ignore()
            .cmd("EXPIRE")
            .arg(key)
            .arg(ttl_secs)
            .ignore()
            .query_async::<()>(&mut c)
            .await?;
        Ok(())
    }
}

/// Isolated warm-blob cache for symbol artifacts.
///
/// Runs against a **dedicated** Redis (its own `maxmemory`/eviction policy) so
/// cached symbol blobs can never evict ingest-stream state. Disabled — every op
/// a no-op — when no URL is configured. Blobs larger than `max_blob_bytes` are
/// never cached (they stay in the in-process parsed-index tier only). All errors
/// are swallowed: the cache is strictly best-effort and never fails a caller.
#[derive(Clone)]
pub struct SymbolBlobCache {
    conn: Option<ConnectionManager>,
    max_blob_bytes: usize,
}

impl SymbolBlobCache {
    /// Connect to the isolated cache, or return a disabled cache when `url` is
    /// `None` (or the connection can't be established).
    pub async fn connect(url: Option<&str>, max_blob_bytes: usize) -> Self {
        let conn = match url {
            Some(u) => match redis::Client::open(u) {
                Ok(client) => {
                    let config = ConnectionManagerConfig::new().set_response_timeout(None);
                    match ConnectionManager::new_with_config(client, config).await {
                        Ok(c) => Some(c),
                        Err(e) => {
                            tracing::warn!(error = %e, "symbol blob cache disabled: connect failed");
                            None
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "symbol blob cache disabled: bad url");
                    None
                }
            },
            None => None,
        };
        Self {
            conn,
            max_blob_bytes,
        }
    }

    pub fn enabled(&self) -> bool {
        self.conn.is_some()
    }

    fn key(sha_hex: &str) -> String {
        format!("sauron:sym:{sha_hex}")
    }

    /// Fetch compressed blob bytes; any error (or disabled cache) is a miss.
    pub async fn get(&self, sha_hex: &str) -> Option<Vec<u8>> {
        let mut c = self.conn.clone()?;
        match redis::cmd("GET")
            .arg(Self::key(sha_hex))
            .query_async::<Option<Vec<u8>>>(&mut c)
            .await
        {
            Ok(v) => v,
            Err(e) => {
                tracing::debug!(error = %e, "symbol blob cache get failed");
                None
            }
        }
    }

    /// Store compressed blob bytes, skipping blobs over the size cap.
    pub async fn put(&self, sha_hex: &str, compressed: &[u8]) {
        if compressed.len() > self.max_blob_bytes {
            return;
        }
        let Some(mut c) = self.conn.clone() else {
            return;
        };
        if let Err(e) = redis::cmd("SET")
            .arg(Self::key(sha_hex))
            .arg(compressed)
            .query_async::<()>(&mut c)
            .await
        {
            tracing::debug!(error = %e, "symbol blob cache put failed");
        }
    }
}

#[cfg(test)]
mod hll_tests {
    use super::*;

    async fn store() -> Option<RedisStore> {
        let url = std::env::var("TEST_REDIS_URL").ok()?;
        RedisStore::connect(&url).await.ok()
    }

    /// The batched write path folds a whole batch's members for one issue into
    /// a single `PFADD`. If the slice were sent as ONE argument instead of many
    /// the call would still succeed, still return 1, and still leave a usable
    /// HyperLogLog — it would just be counting the concatenation as one person.
    /// Nothing downstream could tell. So the count is asserted, not the reply.
    #[tokio::test]
    async fn pf_add_many_counts_each_member_separately() {
        let Some(redis) = store().await else {
            eprintln!("TEST_REDIS_URL unset — skipping");
            return;
        };
        let key = format!("sauron:test:hll:{}", uuid::Uuid::new_v4());

        assert!(
            redis.pf_add_many(&key, &["a", "b", "c"]).await.unwrap(),
            "first insert must report the estimate moved"
        );
        assert_eq!(
            redis.pf_count(&key).await.unwrap(),
            3,
            "three distinct members must count as three, not as one concatenated blob"
        );

        // The whole point of the bool: a batch of people already seen must do
        // no `issues.users_seen` write at all.
        assert!(
            !redis.pf_add_many(&key, &["a", "b"]).await.unwrap(),
            "re-adding known members must report no change"
        );
        assert!(
            redis.pf_add_many(&key, &["a", "d"]).await.unwrap(),
            "one new member among known ones must report a change"
        );
        assert_eq!(redis.pf_count(&key).await.unwrap(), 4);

        // The single-member form is the same call, and must still agree.
        assert!(!redis.pf_add(&key, "a").await.unwrap());
        assert!(redis.pf_add(&key, "e").await.unwrap());
        assert_eq!(redis.pf_count(&key).await.unwrap(), 5);

        // Empty is a no-op, not a malformed `PFADD key` with no arguments.
        assert!(!redis.pf_add_many(&key, &[]).await.unwrap());
        assert_eq!(redis.pf_count(&key).await.unwrap(), 5);

        let mut c = redis.conn.clone();
        let _: () = redis::cmd("DEL")
            .arg(&key)
            .query_async(&mut c)
            .await
            .unwrap();
    }

    /// The breadcrumb push became one pipeline instead of three awaits. A
    /// pipeline that silently dropped a command would still return `Ok(())`,
    /// so all three effects are asserted: the entry is at the head, the list
    /// is capped, and the key carries a TTL.
    #[tokio::test]
    async fn push_breadcrumbs_pushes_trims_and_expires_in_one_pipeline() {
        let Some(redis) = store().await else {
            eprintln!("TEST_REDIS_URL unset — skipping");
            return;
        };
        let key = format!("sauron:test:bc:{}", uuid::Uuid::new_v4());
        let mut c = redis.conn.clone();

        for i in 0..5 {
            redis
                .push_breadcrumbs(&key, &format!("[{i}]"), 3, 60)
                .await
                .unwrap();
        }

        let len: i64 = redis::cmd("LLEN")
            .arg(&key)
            .query_async(&mut c)
            .await
            .unwrap();
        assert_eq!(len, 3, "LTRIM must hold the list at the cap");

        let head: String = redis::cmd("LINDEX")
            .arg(&key)
            .arg(0)
            .query_async(&mut c)
            .await
            .unwrap();
        assert_eq!(
            head, "[4]",
            "LPUSH must put the newest breadcrumb at the head"
        );
        let tail: String = redis::cmd("LINDEX")
            .arg(&key)
            .arg(2)
            .query_async(&mut c)
            .await
            .unwrap();
        assert_eq!(
            tail, "[2]",
            "the three most recent must survive, oldest-first at the tail"
        );

        let ttl: i64 = redis::cmd("TTL")
            .arg(&key)
            .query_async(&mut c)
            .await
            .unwrap();
        assert!(
            (1..=60).contains(&ttl),
            "EXPIRE must be applied; got TTL {ttl}"
        );

        let _: () = redis::cmd("DEL")
            .arg(&key)
            .query_async(&mut c)
            .await
            .unwrap();
    }

    /// A pipelined `XADD` batch must land every entry, in order, and report one
    /// result per payload — the edge counts those results to tell the SDK how
    /// many items it accepted, so a reply vector that is short, reordered, or
    /// collapsed to a single verdict would silently mis-report delivery.
    #[tokio::test]
    async fn xadd_jobs_enqueues_every_payload_in_order() {
        let Some(redis) = store().await else {
            eprintln!("TEST_REDIS_URL unset — skipping");
            return;
        };
        let mut c = redis.conn.clone();
        // The stream name is a const, so isolate by draining it first rather
        // than by using a unique key.
        let _: () = redis::cmd("DEL")
            .arg(keys::INGEST_STREAM)
            .query_async(&mut c)
            .await
            .unwrap();

        let payloads = ["{\"n\":1}", "{\"n\":2}", "{\"n\":3}"];
        let results = redis.xadd_jobs(&payloads, 100).await.unwrap();
        assert_eq!(results.len(), 3, "one result per payload");
        assert!(results.iter().all(|r| r.is_ok()), "all three accepted");

        let len: i64 = redis::cmd("XLEN")
            .arg(keys::INGEST_STREAM)
            .query_async(&mut c)
            .await
            .unwrap();
        assert_eq!(len, 3, "every payload must reach the stream");

        // Stream order must match argument order: the worker reads entries in
        // stream order, and an envelope's items are not independent (a
        // breadcrumb batch preceding its error, for one).
        let entries: Vec<(String, Vec<String>)> = redis::cmd("XRANGE")
            .arg(keys::INGEST_STREAM)
            .arg("-")
            .arg("+")
            .query_async(&mut c)
            .await
            .unwrap();
        let bodies: Vec<&str> = entries.iter().map(|(_, kv)| kv[1].as_str()).collect();
        assert_eq!(bodies, payloads);

        // Empty is a no-op, not an `XADD` with no field/value pair (which
        // Redis rejects) and not a spurious entry.
        assert!(redis.xadd_jobs(&[], 100).await.unwrap().is_empty());
        let len: i64 = redis::cmd("XLEN")
            .arg(keys::INGEST_STREAM)
            .query_async(&mut c)
            .await
            .unwrap();
        assert_eq!(len, 3);

        let _: () = redis::cmd("DEL")
            .arg(keys::INGEST_STREAM)
            .query_async(&mut c)
            .await
            .unwrap();
    }

    /// Two limiters in one round trip must give each key its OWN count and its
    /// own verdict. Sharing a counter, or returning one verdict for both, would
    /// make an app inherit its key's traffic (or the reverse) and start
    /// rejecting at half the configured rate.
    #[tokio::test]
    async fn pipelined_rate_limits_are_independent_and_expire() {
        let Some(redis) = store().await else {
            eprintln!("TEST_REDIS_URL unset — skipping");
            return;
        };
        let mut c = redis.conn.clone();
        let a = format!("sauron:test:rl:a:{}", uuid::Uuid::new_v4());
        let b = format!("sauron:test:rl:b:{}", uuid::Uuid::new_v4());

        // First call opens both windows: counts are 1, both under their limits.
        let v = redis
            .rate_limit_ok_many(&[(a.as_str(), 2), (b.as_str(), 100)], 60)
            .await
            .unwrap();
        assert_eq!(v, vec![true, true]);

        // The window must actually expire. A counter with no TTL never resets,
        // which rate-limits the app permanently after `limit` requests.
        for key in [&a, &b] {
            let ttl: i64 = redis::cmd("TTL")
                .arg(key)
                .query_async(&mut c)
                .await
                .unwrap();
            assert!((1..=60).contains(&ttl), "EXPIRE must be applied; got {ttl}");
        }

        // Second call: `a` reaches its limit of 2 and is still allowed.
        let v = redis
            .rate_limit_ok_many(&[(a.as_str(), 2), (b.as_str(), 100)], 60)
            .await
            .unwrap();
        assert_eq!(v, vec![true, true]);

        // Third: `a` is over, `b` is nowhere near. Independent verdicts.
        let v = redis
            .rate_limit_ok_many(&[(a.as_str(), 2), (b.as_str(), 100)], 60)
            .await
            .unwrap();
        assert_eq!(
            v,
            vec![false, true],
            "each key must be judged on its own count"
        );

        let count: i64 = redis::cmd("GET").arg(&b).query_async(&mut c).await.unwrap();
        assert_eq!(count, 3, "the second key must have its own counter");

        assert!(redis.rate_limit_ok_many(&[], 60).await.unwrap().is_empty());

        let _: () = redis::cmd("DEL")
            .arg(&a)
            .arg(&b)
            .query_async(&mut c)
            .await
            .unwrap();
    }
}
