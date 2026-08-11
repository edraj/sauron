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
/// Default `XADD MAXLEN ~` bound on the ingest stream.
///
/// Lives here rather than in `sauron-ingest` because three separate processes
/// now append to that stream — the edge, the worker's retry drain, and the
/// API's manual replay — and they are separate binaries that cannot see each
/// other's parse of `INGEST_STREAM_MAXLEN`. A re-enqueue using a SMALLER bound
/// than the edge would trim live entries the edge had already answered `202`
/// to, turning the recovery path into a data-loss path. One constant is what
/// makes that impossible rather than merely unlikely.
pub const INGEST_STREAM_MAXLEN_DEFAULT: usize = 1_000_000;

pub mod keys {
    use sha2::{Digest, Sha256};

    pub const INGEST_STREAM: &str = "sauron:ingest:stream";
    pub const INGEST_DLQ: &str = "sauron:ingest:dlq";
    /// Jobs waiting out a retry backoff, scored by the unix-millis instant they
    /// become due.
    ///
    /// A sorted set rather than a stream because the access pattern is "give me
    /// everything due by now", which is exactly `ZRANGEBYSCORE`. It holds only
    /// jobs mid-backoff, so it is bounded by the failure *rate*, not by failure
    /// *history* the way the dead-letter stream is.
    pub const INGEST_RETRY: &str = "sauron:ingest:retry";
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

/// Re-exported so a caller of [`RedisStore::stream_stats`] needs no direct
/// dependency on `sauron-telemetry`. The struct lives there because that is
/// where it is rendered, and defining it twice would mean deriving
/// `unread_trimmed` twice.
pub use sauron_telemetry::metrics::StreamSnapshot;

/// The `field, value` pairs of an `XINFO` map reply.
///
/// Written against both shapes deliberately: RESP2 returns a flat array of
/// alternating field and value, RESP3 returns a real map, and which one arrives
/// depends on the connection's protocol rather than on anything this crate
/// controls.
fn field_pairs(v: &redis::Value) -> Vec<(String, redis::Value)> {
    match v {
        redis::Value::Map(pairs) => pairs
            .iter()
            .filter_map(|(k, v)| Some((as_string(k)?, v.clone())))
            .collect(),
        redis::Value::Array(flat) => flat
            .chunks_exact(2)
            .filter_map(|kv| Some((as_string(&kv[0])?, kv[1].clone())))
            .collect(),
        _ => Vec::new(),
    }
}

fn as_string(v: &redis::Value) -> Option<String> {
    match v {
        redis::Value::BulkString(b) => String::from_utf8(b.clone()).ok(),
        redis::Value::SimpleString(s) => Some(s.clone()),
        _ => None,
    }
}

/// A non-negative integer from an `XINFO` field, or `None` for nil — which is a
/// value Redis genuinely returns for `entries-read` and `lag`, and which must
/// stay distinguishable from zero all the way to the rendered metric.
fn as_u64(v: &redis::Value) -> Option<u64> {
    match v {
        redis::Value::Nil => None,
        redis::Value::Int(i) => u64::try_from(*i).ok(),
        redis::Value::BulkString(b) => std::str::from_utf8(b).ok()?.parse().ok(),
        redis::Value::SimpleString(s) => s.parse().ok(),
        _ => None,
    }
}

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
        self.xadd_job_to(keys::INGEST_STREAM, payload, maxlen).await
    }

    /// [`xadd_job`](Self::xadd_job) against an arbitrary stream key.
    ///
    /// Exists for the same reason `stream_stats` takes its keys as parameters:
    /// the retry drain's tests must be able to exercise the real re-enqueue
    /// path without appending synthetic payloads to the live ingest stream, and
    /// without two concurrent tests sharing one key and clobbering each other.
    pub async fn xadd_job_to(
        &self,
        stream_key: &str,
        payload: &str,
        maxlen: usize,
    ) -> anyhow::Result<String> {
        let mut ids = self
            .xadd_jobs_to(stream_key, std::slice::from_ref(&payload), maxlen)
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
        self.xadd_jobs_to(keys::INGEST_STREAM, payloads, maxlen)
            .await
    }

    /// [`xadd_jobs`](Self::xadd_jobs) against an arbitrary stream key.
    pub async fn xadd_jobs_to(
        &self,
        stream_key: &str,
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
                .arg(stream_key)
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
    ///
    /// The ack is deliberately AFTER the DLQ write and behind `?`: if the DLQ
    /// write fails, the entry is not acked and is redelivered, rather than
    /// being retired with no surviving copy anywhere.
    pub async fn dead_letter(&self, id: &str, payload: &str, maxlen: usize) -> anyhow::Result<()> {
        self.dlq_push(payload, maxlen).await?;
        self.ack(id).await
    }

    /// Write to the dead-letter queue WITHOUT acking the stream entry.
    ///
    /// One stream entry now carries a whole envelope, so a single item failing
    /// must not retire the entry its untouched siblings are still waiting in.
    /// The caller acks once, after the last item of the entry has been handled;
    /// until then a crash costs a redelivery (duplicate writes) rather than
    /// silently dropping the remainder of the envelope.
    /// The DLQ is BOUNDED (`MAXLEN ~`), where it used to be unbounded.
    ///
    /// An unbounded dead-letter stream is a slow-motion outage: a poison payload
    /// or a Postgres outage writes one entry per failing item, forever, until
    /// Redis hits `maxmemory` — at which point the INGEST stream stops accepting
    /// writes too and the edge starts refusing live traffic. Bounding the
    /// wreckage is what keeps a failure of the recovery path from becoming a
    /// failure of the ingest path.
    ///
    /// Trimming is EXACT, unlike `xadd_jobs`, which uses `~`. That difference is
    /// deliberate and was forced by a test: `MAXLEN ~ n` only evicts whole radix
    /// nodes, so at realistic dead-letter volumes it evicts nothing at all — 50
    /// pushes with `MAXLEN ~ 5` left all 50 entries. Approximate trimming buys
    /// throughput on the hot ingest path, where XADD is the bottleneck; here the
    /// stream is written only on failure, so the O(n) cost is irrelevant and an
    /// approximate bound that does not actually bound is strictly worse than an
    /// exact one.
    ///
    /// Errors are the caller's to handle and MUST NOT be discarded — see
    /// `dlq_write_failures` in `sauron-telemetry`.
    pub async fn dlq_push(&self, payload: &str, maxlen: usize) -> anyhow::Result<()> {
        self.dlq_push_to(keys::INGEST_DLQ, payload, maxlen).await
    }

    /// [`dlq_push`] against an arbitrary key, so a test can exercise the real
    /// command against a stream of its own instead of the live dead-letter
    /// queue. Same reasoning as [`stream_stats`]' parameters.
    pub async fn dlq_push_to(&self, key: &str, payload: &str, maxlen: usize) -> anyhow::Result<()> {
        let mut c = self.conn.clone();
        redis::cmd("XADD")
            .arg(key)
            .arg("MAXLEN")
            .arg(maxlen)
            .arg("*")
            .arg("d")
            .arg(payload)
            .query_async::<()>(&mut c)
            .await?;
        Ok(())
    }

    /// Drop dead-letter entries older than `retention`, returning how many went.
    ///
    /// `MAXLEN` alone bounds the stream by COUNT, which is the wrong unit for a
    /// privacy question. A dead-lettered payload is a copy of a real event — the
    /// masked copy, but still one that outlives every retention window the
    /// product otherwise enforces, sitting in Redis where no deletion request
    /// and no tier rotation reaches it. On a quiet deployment `MAXLEN` would let
    /// a single entry sit there for years.
    ///
    /// `XTRIM MINID` rather than a scan-and-delete: Redis stream ids are
    /// `<unix-millis>-<seq>`, so an age cutoff IS an id, and the trim is one
    /// O(deleted) command with no read-back. Exact (no `~`) because the whole
    /// point is a hard age guarantee.
    pub async fn dlq_reap(&self, retention: std::time::Duration) -> anyhow::Result<u64> {
        self.dlq_reap_from(keys::INGEST_DLQ, retention).await
    }

    /// [`dlq_reap`] against an arbitrary key — see [`dlq_push_to`].
    pub async fn dlq_reap_from(
        &self,
        key: &str,
        retention: std::time::Duration,
    ) -> anyhow::Result<u64> {
        // `SystemTime`, not chrono: this crate deliberately has no date-time
        // dependency, and a stream id only needs unix millis.
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let cutoff_ms = now_ms - retention.as_millis() as i64;
        if cutoff_ms <= 0 {
            return Ok(0);
        }
        let mut c = self.conn.clone();
        let removed: i64 = redis::cmd("XTRIM")
            .arg(key)
            .arg("MINID")
            .arg(cutoff_ms)
            .query_async(&mut c)
            .await?;
        Ok(removed.max(0) as u64)
    }

    // --- retry backoff -----------------------------------------------------

    /// Park a job until `due_at_ms`, so a transient failure gets another go.
    ///
    /// The caller MUST have acked the job's stream entry before calling this.
    /// Leaving it in the pending list would let `claim_stale` reclaim it after
    /// `RECLAIM_IDLE_MS` — 60s, i.e. almost exactly when the drain re-injects
    /// it — and every transient failure would double-write.
    pub async fn retry_schedule(&self, member: &str, due_at_ms: i64) -> anyhow::Result<()> {
        self.retry_schedule_to(keys::INGEST_RETRY, member, due_at_ms)
            .await
    }

    /// [`retry_schedule`] against an arbitrary key — see [`dlq_push_to`] for why
    /// these variants exist: a test must be able to drive the backoff set
    /// without touching the live one.
    ///
    /// [`retry_schedule`]: Self::retry_schedule
    /// [`dlq_push_to`]: Self::dlq_push_to
    pub async fn retry_schedule_to(
        &self,
        key: &str,
        member: &str,
        due_at_ms: i64,
    ) -> anyhow::Result<()> {
        let mut c = self.conn.clone();
        redis::cmd("ZADD")
            .arg(key)
            .arg(due_at_ms)
            .arg(member)
            .query_async::<()>(&mut c)
            .await?;
        Ok(())
    }

    /// Take up to `limit` jobs that are due, removing them from the set.
    ///
    /// Read-then-remove rather than a single atomic pop, because there is no
    /// Redis primitive that pops from a ZSET *and* appends to a stream. The
    /// caller re-enqueues each member and only then calls [`retry_forget`], so
    /// a crash mid-drain costs a duplicate rather than a lost job — the correct
    /// side to fail toward for an ingest pipeline.
    ///
    /// `limit` is what keeps one mass failure from turning a single tick into
    /// an unbounded re-injection storm.
    pub async fn retry_due(&self, now_ms: i64, limit: usize) -> anyhow::Result<Vec<String>> {
        self.retry_due_from(keys::INGEST_RETRY, now_ms, limit).await
    }

    /// [`retry_due`](Self::retry_due) against an arbitrary key.
    pub async fn retry_due_from(
        &self,
        key: &str,
        now_ms: i64,
        limit: usize,
    ) -> anyhow::Result<Vec<String>> {
        let mut c = self.conn.clone();
        let due: Vec<String> = redis::cmd("ZRANGEBYSCORE")
            .arg(key)
            .arg(0)
            .arg(now_ms)
            .arg("LIMIT")
            .arg(0)
            .arg(limit)
            .query_async(&mut c)
            .await?;
        Ok(due)
    }

    /// Drop a member once it has been successfully re-enqueued.
    pub async fn retry_forget(&self, member: &str) -> anyhow::Result<()> {
        self.retry_forget_from(keys::INGEST_RETRY, member).await
    }

    /// [`retry_forget`](Self::retry_forget) against an arbitrary key.
    pub async fn retry_forget_from(&self, key: &str, member: &str) -> anyhow::Result<()> {
        let mut c = self.conn.clone();
        redis::cmd("ZREM")
            .arg(key)
            .arg(member)
            .query_async::<()>(&mut c)
            .await?;
        Ok(())
    }

    /// Count one failed attempt for a job, returning the running total.
    ///
    /// The attempt count cannot travel with the job. A drained retry is
    /// re-injected onto the ingest stream as an ordinary payload, and teaching
    /// the stream format to carry a counter would change the decode path every
    /// healthy event takes, for the benefit of the rare failing one. So the
    /// count lives beside the job, keyed by a hash of its bytes.
    ///
    /// Without this the retry loop is INFINITE: every re-injected job would
    /// fail at attempt 1 forever, and a permanently-broken payload would cycle
    /// through the backoff set for as long as the deployment lives.
    ///
    /// `INCR` then `EXPIRE`, pipelined. The TTL is what makes the key
    /// self-cleaning — a job that succeeds on retry never comes back to clear
    /// its counter, so the counter has to forget on its own.
    pub async fn bump_attempt(&self, job_hash: &str, ttl_secs: u64) -> anyhow::Result<i32> {
        let key = format!("sauron:ingest:att:{job_hash}");
        let mut c = self.conn.clone();
        let mut pipe = redis::pipe();
        pipe.cmd("INCR").arg(&key);
        pipe.cmd("EXPIRE").arg(&key).arg(ttl_secs).ignore();
        let (n,): (i32,) = pipe.query_async(&mut c).await?;
        Ok(n)
    }

    /// Forget a job's attempt count, once it has succeeded or gone terminal.
    pub async fn clear_attempt(&self, job_hash: &str) -> anyhow::Result<()> {
        let mut c = self.conn.clone();
        redis::cmd("DEL")
            .arg(format!("sauron:ingest:att:{job_hash}"))
            .query_async::<()>(&mut c)
            .await?;
        Ok(())
    }

    /// How many jobs are currently waiting out a backoff. For the gauge.
    pub async fn retry_depth(&self) -> anyhow::Result<u64> {
        self.retry_depth_of(keys::INGEST_RETRY).await
    }

    /// [`retry_depth`](Self::retry_depth) against an arbitrary key.
    pub async fn retry_depth_of(&self, key: &str) -> anyhow::Result<u64> {
        let mut c = self.conn.clone();
        let n: u64 = redis::cmd("ZCARD").arg(key).query_async(&mut c).await?;
        Ok(n)
    }

    // --- stream observability ---------------------------------------------

    /// A strictly read-only reading of a stream, its consumer group and a
    /// dead-letter stream, in ENTRIES.
    ///
    /// `XINFO STREAM`, `XINFO GROUPS` and `XLEN` in one pipeline. Nothing is
    /// written and no key is modified.
    ///
    /// Keys and group name are parameters rather than the [`keys`] constants so
    /// a test can probe a stream of its own and never touch the live ingest one.
    ///
    /// A missing stream is not an error: the pipeline ignores per-command
    /// errors, so a stream that does not exist yet reads back as zeroes with
    /// `entries_read`/`lag` absent, which is what is actually known about it.
    ///
    /// ## Reply size
    ///
    /// Measured on redis 7.4.10, against a stream of 3 entries whose payloads
    /// were 1783 bytes each: plain `XINFO STREAM` replied **3941 bytes**,
    /// because it echoes `first-entry` and `last-entry` with their full
    /// payloads — so the worst case is bounded by two
    /// `INGEST_MAX_BODY_BYTES`, not by anything small. `FULL COUNT 0` is not
    /// the fix: **6147 bytes** on the same stream, because `COUNT 0` means "no
    /// limit" and it returns every entry plus the pending-entries list.
    /// `XINFO GROUPS` was 150 bytes and carries no payload at all.
    ///
    /// Hence the caller samples this on a fixed interval rather than per
    /// scrape.
    pub async fn stream_stats(
        &self,
        stream_key: &str,
        group: &str,
        dlq_key: &str,
    ) -> anyhow::Result<StreamSnapshot> {
        self.stream_stats_with_retry(stream_key, group, dlq_key, keys::INGEST_RETRY)
            .await
    }

    /// [`stream_stats`](Self::stream_stats) with the backoff set named
    /// explicitly, so a test can probe its own keys.
    pub async fn stream_stats_with_retry(
        &self,
        stream_key: &str,
        group: &str,
        dlq_key: &str,
        retry_key: &str,
    ) -> anyhow::Result<StreamSnapshot> {
        let mut pipe = redis::pipe();
        pipe.ignore_errors();
        pipe.cmd("XINFO").arg("STREAM").arg(stream_key);
        pipe.cmd("XINFO").arg("GROUPS").arg(stream_key);
        pipe.cmd("XLEN").arg(dlq_key);
        // Folded into the SAME pipeline rather than a separate round trip: this
        // probe runs on the metrics path, and an extra RTT there is paid on
        // every scrape forever.
        pipe.cmd("ZCARD").arg(retry_key);

        let mut c = self.conn.clone();
        let replies: Vec<redis::RedisResult<redis::Value>> = pipe.query_async(&mut c).await?;

        let reply =
            |i: usize| -> Option<&redis::Value> { replies.get(i).and_then(|r| r.as_ref().ok()) };

        let mut stats = StreamSnapshot::default();

        if let Some(v) = reply(0) {
            for (field, value) in field_pairs(v) {
                match field.as_str() {
                    "length" => stats.length = as_u64(&value).unwrap_or(0),
                    "entries-added" => stats.entries_added = as_u64(&value).unwrap_or(0),
                    _ => {}
                }
            }
        }

        // One group among several; match by name rather than taking the first.
        if let Some(redis::Value::Array(groups)) = reply(1) {
            for g in groups {
                let pairs = field_pairs(g);
                let is_ours = pairs
                    .iter()
                    .any(|(f, v)| f == "name" && as_string(v).as_deref() == Some(group));
                if !is_ours {
                    continue;
                }
                for (field, value) in pairs {
                    match field.as_str() {
                        // Deliberately `as_u64`, which answers `None` for a nil
                        // reply. Redis really does return nil for both of these
                        // once its exact bookkeeping is broken — measured on
                        // 7.4.10: `XDEL` of an undelivered entry nils `lag`,
                        // and `XGROUP SETID` without `ENTRIESREAD` nils both.
                        // A nil rendered as 0 would announce "nothing was
                        // trimmed" using a number Redis refused to give.
                        "entries-read" => stats.entries_read = as_u64(&value),
                        "lag" => stats.lag = as_u64(&value),
                        _ => {}
                    }
                }
            }
        }

        if let Some(v) = reply(3) {
            stats.retry_length = as_u64(v).unwrap_or(0);
        }

        if let Some(v) = reply(2) {
            stats.dlq_length = as_u64(v).unwrap_or(0);
        }

        Ok(stats)
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

    /// The dead-letter queue used to be unbounded, with no MAXLEN, no TTL and
    /// no reaper — a poison payload or a Postgres outage writes one entry per
    /// failing item until Redis hits `maxmemory`, at which point the INGEST
    /// stream stops accepting writes too and the edge refuses live traffic. A
    /// failure of the recovery path became a failure of the ingest path.
    #[tokio::test]
    async fn dlq_push_bounds_the_stream() {
        let Some(redis) = store().await else {
            eprintln!("TEST_REDIS_URL unset — skipping");
            return;
        };
        let key = format!("sauron:test:dlq:{}", uuid::Uuid::new_v4());
        for i in 0..50 {
            redis
                .dlq_push_to(&key, &format!("{{\"i\":{i}}}"), 5)
                .await
                .unwrap();
        }
        let mut c = redis.conn.clone();
        let len: i64 = redis::cmd("XLEN")
            .arg(&key)
            .query_async(&mut c)
            .await
            .unwrap();
        // EXACTLY 5, not merely "fewer than 50". This assertion is why the DLQ
        // uses exact trimming: written with `MAXLEN ~ 5` this test failed with
        // len == 50, because approximate trimming evicts whole radix nodes and
        // 50 small entries fit in one. An approximate bound that does not bound
        // is worse than no bound, because it reads as protection.
        assert_eq!(len, 5, "MAXLEN must bound the DLQ exactly");
        let _: () = redis::cmd("DEL")
            .arg(&key)
            .query_async(&mut c)
            .await
            .unwrap();
    }

    /// MAXLEN bounds by COUNT, which is the wrong unit for a privacy question:
    /// a dead-lettered payload is a copy of a real event that outlives every
    /// retention window the product enforces, and on a quiet deployment a count
    /// bound would let one sit there for years.
    #[tokio::test]
    async fn dlq_reap_drops_only_entries_older_than_the_retention() {
        let Some(redis) = store().await else {
            eprintln!("TEST_REDIS_URL unset — skipping");
            return;
        };
        let key = format!("sauron:test:dlq:{}", uuid::Uuid::new_v4());
        let mut c = redis.conn.clone();
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;

        // Explicit ids, so "old" and "new" are facts rather than a sleep: a
        // stream id IS a unix-millis timestamp, which is the whole reason the
        // reaper can use XTRIM MINID instead of scanning.
        for (n, age_ms) in [
            ("old1", 7_200_000i64),
            ("old2", 3_700_000),
            ("new1", 60_000),
        ] {
            let _: String = redis::cmd("XADD")
                .arg(&key)
                .arg(format!("{}-0", now_ms - age_ms))
                .arg("d")
                .arg(n)
                .query_async(&mut c)
                .await
                .unwrap();
        }

        let removed = redis
            .dlq_reap_from(&key, std::time::Duration::from_secs(3600))
            .await
            .unwrap();
        assert_eq!(
            removed, 2,
            "both entries older than an hour, and only those"
        );
        let len: i64 = redis::cmd("XLEN")
            .arg(&key)
            .query_async(&mut c)
            .await
            .unwrap();
        assert_eq!(len, 1);

        // Idempotent: a second pass with nothing newly aged removes nothing.
        assert_eq!(
            redis
                .dlq_reap_from(&key, std::time::Duration::from_secs(3600))
                .await
                .unwrap(),
            0
        );
        let _: () = redis::cmd("DEL")
            .arg(&key)
            .query_async(&mut c)
            .await
            .unwrap();
    }

    // --- retry backoff, against a real Redis -----------------------------

    /// Only jobs whose due time has passed come back, and taking them does not
    /// remove them — the drain removes each one only after re-enqueueing it.
    #[tokio::test]
    async fn retry_due_returns_only_what_is_due_and_removes_nothing() {
        let Some(redis) = store().await else {
            eprintln!("TEST_REDIS_URL unset — skipping");
            return;
        };
        let key = format!("sauron:test:retry:{}", uuid::Uuid::new_v4());
        let now = 1_700_000_000_000i64;

        redis
            .retry_schedule_to(&key, "due-a", now - 1000)
            .await
            .unwrap();
        redis.retry_schedule_to(&key, "due-b", now).await.unwrap();
        redis
            .retry_schedule_to(&key, "later", now + 60_000)
            .await
            .unwrap();

        let due = redis.retry_due_from(&key, now, 10).await.unwrap();
        assert_eq!(
            due.len(),
            2,
            "the future job must not be handed back: {due:?}"
        );
        assert!(due.contains(&"due-a".to_string()));
        assert!(due.contains(&"due-b".to_string()));

        // Read must not consume. If it did, a crash between the read and the
        // re-enqueue would lose the job outright rather than duplicate it.
        assert_eq!(redis.retry_depth_of(&key).await.unwrap(), 3);

        redis.retry_forget_from(&key, "due-a").await.unwrap();
        assert_eq!(redis.retry_depth_of(&key).await.unwrap(), 2);

        let mut c = redis.conn.clone();
        let _: () = redis::cmd("DEL")
            .arg(&key)
            .query_async(&mut c)
            .await
            .unwrap();
    }

    /// The per-tick ceiling actually binds. Without it, one mass transient
    /// failure turns the first tick afterwards into a re-injection storm that
    /// recreates the outage it is recovering from.
    #[tokio::test]
    async fn retry_due_respects_its_limit() {
        let Some(redis) = store().await else {
            eprintln!("TEST_REDIS_URL unset — skipping");
            return;
        };
        let key = format!("sauron:test:retry:{}", uuid::Uuid::new_v4());
        let now = 1_700_000_000_000i64;
        for i in 0..25 {
            redis
                .retry_schedule_to(&key, &format!("m{i}"), now - 1)
                .await
                .unwrap();
        }
        assert_eq!(redis.retry_due_from(&key, now, 10).await.unwrap().len(), 10);
        assert_eq!(
            redis.retry_due_from(&key, now, 100).await.unwrap().len(),
            25
        );

        let mut c = redis.conn.clone();
        let _: () = redis::cmd("DEL")
            .arg(&key)
            .query_async(&mut c)
            .await
            .unwrap();
    }

    /// A sorted set keys on the MEMBER, so two byte-identical payloads would
    /// collapse into one and the second event would be silently dropped. This
    /// is why the parked envelope carries a nonce — asserted here against a
    /// real Redis rather than inferred from the serializer.
    #[tokio::test]
    async fn identical_members_collapse_which_is_why_the_nonce_exists() {
        let Some(redis) = store().await else {
            eprintln!("TEST_REDIS_URL unset — skipping");
            return;
        };
        let key = format!("sauron:test:retry:{}", uuid::Uuid::new_v4());
        let now = 1_700_000_000_000i64;

        redis
            .retry_schedule_to(&key, "same-bytes", now)
            .await
            .unwrap();
        redis
            .retry_schedule_to(&key, "same-bytes", now)
            .await
            .unwrap();
        assert_eq!(
            redis.retry_depth_of(&key).await.unwrap(),
            1,
            "identical members DO collapse — the nonce in `retry::Parked` is \
             what keeps two identical payloads from becoming one"
        );

        redis
            .retry_schedule_to(&key, "{\"n\":\"1\",\"d\":\"x\"}", now)
            .await
            .unwrap();
        redis
            .retry_schedule_to(&key, "{\"n\":\"2\",\"d\":\"x\"}", now)
            .await
            .unwrap();
        assert_eq!(redis.retry_depth_of(&key).await.unwrap(), 3);

        let mut c = redis.conn.clone();
        let _: () = redis::cmd("DEL")
            .arg(&key)
            .query_async(&mut c)
            .await
            .unwrap();
    }

    /// The guard against an infinite retry loop: the counter must actually
    /// increment across calls, and must carry a TTL so a job that succeeds on
    /// retry (and so never comes back to clear it) cannot leak a key forever.
    #[tokio::test]
    async fn attempt_counter_increments_and_expires() {
        let Some(redis) = store().await else {
            eprintln!("TEST_REDIS_URL unset — skipping");
            return;
        };
        let hash = format!("test{}", uuid::Uuid::new_v4().simple());

        assert_eq!(redis.bump_attempt(&hash, 900).await.unwrap(), 1);
        assert_eq!(redis.bump_attempt(&hash, 900).await.unwrap(), 2);
        assert_eq!(redis.bump_attempt(&hash, 900).await.unwrap(), 3);

        let mut c = redis.conn.clone();
        let ttl: i64 = redis::cmd("TTL")
            .arg(format!("sauron:ingest:att:{hash}"))
            .query_async(&mut c)
            .await
            .unwrap();
        assert!(
            ttl > 0 && ttl <= 900,
            "the counter must expire on its own (ttl was {ttl}); a -1 here means \
             every job that ever failed leaks a key for the life of the server"
        );

        // A distinct job must not inherit another's attempts.
        let other = format!("test{}", uuid::Uuid::new_v4().simple());
        assert_eq!(redis.bump_attempt(&other, 900).await.unwrap(), 1);

        redis.clear_attempt(&hash).await.unwrap();
        assert_eq!(
            redis.bump_attempt(&hash, 900).await.unwrap(),
            1,
            "clearing must reset, or a later identical payload starts terminal"
        );

        redis.clear_attempt(&hash).await.unwrap();
        redis.clear_attempt(&other).await.unwrap();
    }

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

#[cfg(test)]
mod stream_stats_tests {
    use super::*;

    /// The backoff-set depth must come from the RIGHT pipeline reply.
    ///
    /// `ZCARD` was appended as the fourth command, so it is read at index 3. An
    /// off-by-one here does not error — it silently reports the DLQ's length as
    /// the retry depth (or zero), which is a plausible number that would send an
    /// operator hunting the wrong tier during an incident.
    #[tokio::test]
    async fn retry_length_reads_the_backoff_set_not_the_dlq() {
        let Some(redis) = store().await else {
            eprintln!("TEST_REDIS_URL unset — skipping");
            return;
        };
        let key = unique("stream");
        let dlq = unique("dlq");
        let retry = unique("retry");
        let group = "test-workers";

        // Deliberately DIFFERENT counts in every key, so a mixed-up index
        // cannot coincidentally produce the right answer.
        let mut c0 = redis.conn.clone();
        let _: String = redis::cmd("XADD")
            .arg(&key)
            .arg("*")
            .arg("d")
            .arg("{}")
            .query_async(&mut c0)
            .await
            .unwrap();
        for i in 0..3 {
            redis
                .dlq_push_to(&dlq, &format!("{{\"i\":{i}}}"), 100)
                .await
                .unwrap();
        }
        for i in 0..7 {
            redis
                .retry_schedule_to(&retry, &format!("m{i}"), 1_700_000_000_000)
                .await
                .unwrap();
        }

        let s = redis
            .stream_stats_with_retry(&key, group, &dlq, &retry)
            .await
            .unwrap();
        assert_eq!(s.dlq_length, 3, "dlq_length must still be the DLQ");
        assert_eq!(s.retry_length, 7, "retry_length must be the backoff set");

        let mut c = redis.conn.clone();
        for k in [&key, &dlq, &retry] {
            let _: () = redis::cmd("DEL").arg(k).query_async(&mut c).await.unwrap();
        }
    }

    /// Every key this module touches carries a per-test UUID, so it can run
    /// against a shared Redis without going anywhere near
    /// `keys::INGEST_STREAM`. That is also why `stream_stats` takes its keys as
    /// parameters instead of reading the constants.
    fn unique(kind: &str) -> String {
        format!("sauron:test:{kind}:{}", uuid::Uuid::new_v4())
    }

    async fn store() -> Option<RedisStore> {
        let url = std::env::var("TEST_REDIS_URL").ok()?;
        RedisStore::connect(&url).await.ok()
    }

    async fn xadd(redis: &RedisStore, key: &str, n: usize, maxlen: Option<usize>) {
        let mut c = redis.conn.clone();
        for i in 0..n {
            let mut cmd = redis::cmd("XADD");
            cmd.arg(key);
            if let Some(m) = maxlen {
                cmd.arg("MAXLEN").arg(m);
            }
            cmd.arg("*").arg("d").arg(format!("payload-{i}"));
            cmd.query_async::<String>(&mut c).await.unwrap();
        }
    }

    async fn read_group(redis: &RedisStore, key: &str, group: &str, count: usize) -> usize {
        let mut c = redis.conn.clone();
        let opts = StreamReadOptions::default()
            .group(group, "test-consumer")
            .count(count);
        let reply: StreamReadReply = c.xread_options(&[key], &[">"], &opts).await.unwrap();
        reply.keys.iter().map(|k| k.ids.len()).sum()
    }

    /// The whole point of the derived gauge, against a trim we cause on purpose.
    ///
    /// Also pins the property that makes it a LIVE gauge and not a ledger: once
    /// the group reaches the tail, Redis folds the trimmed gap into
    /// `entries-read` and the gauge reads 0 again. Anyone who later builds the
    /// loss accounting out of Redis instead of out of our own counters gets a
    /// failing test here.
    #[tokio::test]
    async fn detects_entries_trimmed_before_the_group_read_them() {
        let Some(redis) = store().await else {
            eprintln!("TEST_REDIS_URL unset — skipping detects_entries_trimmed_before_the_group_read_them");
            return;
        };
        let key = unique("stream");
        let dlq = unique("dlq");
        let group = "test-workers";

        // 10 entries, group starting at the head, 3 delivered.
        xadd(&redis, &key, 10, None).await;
        let mut c = redis.conn.clone();
        redis::cmd("XGROUP")
            .arg("CREATE")
            .arg(&key)
            .arg(group)
            .arg("0")
            .query_async::<()>(&mut c)
            .await
            .unwrap();
        assert_eq!(read_group(&redis, &key, group, 3).await, 3);

        let before = redis.stream_stats(&key, group, &dlq).await.unwrap();
        assert_eq!(before.entries_added, 10);
        assert_eq!(before.entries_read, Some(3));
        assert_eq!(before.lag, Some(7));
        assert_eq!(
            before.unread_trimmed(),
            Some(0),
            "nothing has been trimmed yet"
        );

        // One more entry, with an exact MAXLEN of 2 — this is the trim. Six of
        // the eight entries the group had not been delivered are now gone.
        xadd(&redis, &key, 1, Some(2)).await;

        let after = redis.stream_stats(&key, group, &dlq).await.unwrap();
        assert_eq!(after.length, 2, "MAXLEN 2 must leave exactly two entries");
        assert_eq!(after.entries_added, 11);
        assert_eq!(after.entries_read, Some(3));
        assert_eq!(
            after.lag,
            Some(2),
            "a trim does not nil the lag, it silently recomputes it downward — \
             which is exactly why lag alone is not a loss signal"
        );
        assert_eq!(
            after.unread_trimmed(),
            Some(6),
            "11 added - 3 read - 2 still pending = the 6 undelivered entries MAXLEN dropped"
        );

        // Drain to the tail. Two entries are actually delivered, and Redis
        // raises entries-read all the way to 11 to close the gap.
        assert_eq!(read_group(&redis, &key, group, 100).await, 2);
        let drained = redis.stream_stats(&key, group, &dlq).await.unwrap();
        assert_eq!(drained.entries_read, Some(11));
        assert_eq!(drained.lag, Some(0));
        assert_eq!(
            drained.unread_trimmed(),
            Some(0),
            "the Redis-side gap evaporates on catch-up; the durable loss record is the \
             item counters, not this gauge"
        );

        // The DLQ is a separate stream, counted separately.
        xadd(&redis, &dlq, 2, None).await;
        let with_dlq = redis.stream_stats(&key, group, &dlq).await.unwrap();
        assert_eq!(with_dlq.dlq_length, 2);

        let _: () = redis::cmd("DEL")
            .arg(&key)
            .arg(&dlq)
            .query_async(&mut c)
            .await
            .unwrap();
        // Printed so a run can prove the body executed rather than taking the
        // skip path above and still reporting "ok".
        println!("STREAM_STATS_TRIM_TEST_RAN");
    }

    /// A stream that does not exist is not an error: the numbers are simply
    /// zero, and `entries_read`/`lag` are absent because there is no group to
    /// report them.
    #[tokio::test]
    async fn missing_stream_reads_as_zero_with_absent_group_fields() {
        let Some(redis) = store().await else {
            eprintln!(
                "TEST_REDIS_URL unset — skipping missing_stream_reads_as_zero_with_absent_group_fields"
            );
            return;
        };
        let stats = redis
            .stream_stats(&unique("stream"), "test-workers", &unique("dlq"))
            .await
            .unwrap();
        assert_eq!(stats.length, 0);
        assert_eq!(stats.entries_added, 0);
        assert_eq!(stats.entries_read, None);
        assert_eq!(stats.lag, None);
        assert_eq!(stats.dlq_length, 0);
        assert_eq!(
            stats.unread_trimmed(),
            None,
            "no group means the derived gauge is unknown, not zero"
        );
        println!("STREAM_STATS_MISSING_TEST_RAN");
    }
}
