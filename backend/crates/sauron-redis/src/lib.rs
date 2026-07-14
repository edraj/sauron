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
    pub const INGEST_STREAM: &str = "sauron:ingest:stream";
    pub const INGEST_DLQ: &str = "sauron:ingest:dlq";
    pub const CONSUMER_GROUP: &str = "workers";

    pub fn dsn_cache(public_key: &str) -> String {
        format!("sauron:dsn:{public_key}")
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

    // --- ingest stream ----------------------------------------------------

    /// Enqueue a JSON job onto the ingest stream (trimmed to ~`maxlen`).
    pub async fn xadd_job(&self, payload: &str, maxlen: usize) -> anyhow::Result<String> {
        let mut c = self.conn.clone();
        let id: String = redis::cmd("XADD")
            .arg(keys::INGEST_STREAM)
            .arg("MAXLEN")
            .arg("~")
            .arg(maxlen)
            .arg("*")
            .arg("d")
            .arg(payload)
            .query_async(&mut c)
            .await?;
        Ok(id)
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

    /// Dead-letter a permanently failing job, then ack it off the main stream.
    pub async fn dead_letter(&self, id: &str, payload: &str) -> anyhow::Result<()> {
        let mut c = self.conn.clone();
        redis::cmd("XADD")
            .arg(keys::INGEST_DLQ)
            .arg("*")
            .arg("d")
            .arg(payload)
            .query_async::<()>(&mut c)
            .await?;
        self.ack(id).await
    }

    // --- affected-user HyperLogLog ---------------------------------------

    pub async fn pf_add(&self, key: &str, member: &str) -> anyhow::Result<()> {
        let mut c = self.conn.clone();
        redis::cmd("PFADD")
            .arg(key)
            .arg(member)
            .query_async::<i64>(&mut c)
            .await?;
        Ok(())
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
        let mut c = self.conn.clone();
        redis::cmd("LPUSH")
            .arg(key)
            .arg(json)
            .query_async::<i64>(&mut c)
            .await?;
        redis::cmd("LTRIM")
            .arg(key)
            .arg(0)
            .arg(cap - 1)
            .query_async::<()>(&mut c)
            .await?;
        redis::cmd("EXPIRE")
            .arg(key)
            .arg(ttl_secs)
            .query_async::<()>(&mut c)
            .await?;
        Ok(())
    }
}
