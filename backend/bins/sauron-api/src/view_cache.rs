//! The reusable half of the server-side result cache.
//!
//! Extracted from `overview_cache`, which proved the shape and remains its
//! largest caller. The property worth preserving is the one that removes a
//! whole class of 503: a request that reads this cache does three cheap things
//! — authorize, read Redis, maybe enqueue — and never awaits an aggregate, so
//! its latency is bounded well under `REQUEST_TIMEOUT_SECS` no matter how
//! expensive the underlying query is.
//!
//! What lives here is everything that is not about a particular page: the
//! freshness decision, the envelope, the single-flight claim, the concurrency
//! ceiling, and the Redis get/put pairs with their timeouts. What deliberately
//! does NOT live here is the per-route `enqueue`, because what a route does
//! after a recompute genuinely differs — Overview fans the result out over SSE
//! to open dashboards, and nothing else does. Sharing the primitives and
//! letting each route own its own twenty-line spawn is a better trade than a
//! callback parameter threaded through for one caller's benefit.
//!
//! # Two numbers, never conflated
//!
//! [`CachePolicy::fresh_for`] decides whether a recompute is ALSO kicked;
//! [`CachePolicy::ttl_secs`] decides when the entry disappears. The TTL must be
//! much the larger of the two. Set equal, the first read after the freshness
//! horizon finds nothing and renders a skeleton behind a slow aggregate —
//! precisely the behaviour this exists to remove.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Semaphore;

use sauron_db::scope::EnvFilter;
use sauron_redis::RedisStore;

/// Redis is a different host; a hung cache read must not become a hung request.
pub const CACHE_OP_TIMEOUT: StdDuration = StdDuration::from_millis(500);

/// Freshness of what is being returned, as the dashboard sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Freshness {
    /// Computed within the policy's freshness window. No recompute triggered.
    Fresh,
    /// Older than that window. Served as-is; a recompute is running.
    Stale,
    /// Nothing cached. `data` is `null`; a recompute is running.
    Computing,
}

/// What a cached endpoint returns.
///
/// `data` is `Option` because "computing" is a real, expected, 200-worthy
/// state — the whole point is that a cold read answers immediately instead of
/// occupying a request until the timeout. A caller that treats a missing `data`
/// as an error has misread the contract; the dashboard renders a skeleton.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Envelope {
    pub state: Freshness,
    /// When the query behind `data` actually ran. `None` iff `data` is `None`.
    /// This is the stamp the dashboard renders as "as of 14:32".
    pub computed_at: Option<DateTime<Utc>>,
    pub data: Option<Value>,
    /// Set when the most recent recompute FAILED. Independent of `data`: a
    /// failure must never erase a good stale value, so both can be present —
    /// "here are yesterday's numbers, and the refresh is currently broken".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The cached document. `computed_at` travels with the payload rather than
/// being inferred from a Redis TTL, because the TTL is far longer and says
/// nothing about the freshness question.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheEntry {
    pub data: Value,
    pub computed_at: DateTime<Utc>,
}

/// Per-route cache timings.
#[derive(Debug, Clone, Copy)]
pub struct CachePolicy {
    /// Younger than this is served without kicking a recompute.
    pub fresh_for: Duration,
    /// How long the entry survives at all. Must be MUCH larger than
    /// `fresh_for` — see the module docs.
    pub ttl_secs: u64,
    /// How long a failed recompute suppresses re-enqueue.
    pub fail_backoff_secs: u64,
}

/// Process-wide recompute coordinator. Cheap to clone (all `Arc` inside).
#[derive(Clone)]
pub struct ViewCache {
    /// Cache keys with a recompute already running. THE deduplication: without
    /// it, N concurrent viewers of one dashboard is N concurrent slow queries.
    inflight: Arc<Mutex<HashSet<String>>>,
    /// Global ceiling on concurrent recomputes, independent of how many
    /// distinct keys are in flight.
    permits: Arc<Semaphore>,
}

impl ViewCache {
    pub fn new(permits: usize) -> Self {
        Self {
            inflight: Arc::new(Mutex::new(HashSet::new())),
            permits: Arc::new(Semaphore::new(permits)),
        }
    }

    /// The semaphore, for acquiring INSIDE a spawned recompute.
    ///
    /// Acquire it in the task and before the database checkout, never around
    /// one from the caller: a permit held while waiting for a pool connection
    /// (or the reverse) is a two-resource ordering that deadlocks under load.
    pub fn permits(&self) -> Arc<Semaphore> {
        Arc::clone(&self.permits)
    }

    /// Claim the right to recompute `key`, or discover someone already has.
    ///
    /// Returns a guard that releases the claim on drop, so a panicking or
    /// early-returning recompute cannot wedge a key permanently — the failure
    /// mode of a bare `insert`/`remove` pair, and one that presents as "this
    /// section never refreshes again until the process restarts".
    pub fn claim(&self, key: &str) -> Option<InflightGuard> {
        let mut set = self.inflight.lock().unwrap_or_else(|e| e.into_inner());
        if !set.insert(key.to_string()) {
            return None;
        }
        Some(InflightGuard {
            inflight: Arc::clone(&self.inflight),
            key: key.to_string(),
        })
    }
}

pub struct InflightGuard {
    inflight: Arc<Mutex<HashSet<String>>>,
    key: String,
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        let mut set = self.inflight.lock().unwrap_or_else(|e| e.into_inner());
        set.remove(&self.key);
    }
}

/// Injective token for a resolved environment filter.
///
/// Every variant gets a distinct PREFIX, which is the whole point: `One(x)` and
/// `Subset([x])` are different queries — `Subset` compiles to `= ANY(...)`,
/// which never matches `NULL`, while `All` deliberately includes unattributed
/// rows — so they must never collide on a cache key.
///
/// `Subset` is sorted before formatting. The readable set comes out of an RBAC
/// join whose row order is not contractual, so an unsorted token would mint a
/// different cache key for the same caller depending on how Postgres felt about
/// the plan that day — a 0% hit rate with every test still green.
pub fn env_token(env: &EnvFilter) -> String {
    match env {
        EnvFilter::All => "all".to_string(),
        EnvFilter::One(id) => format!("one:{id}"),
        EnvFilter::Unattributed => "none".to_string(),
        EnvFilter::Subset(ids) => {
            let mut sorted: Vec<String> = ids.iter().map(|i| i.to_string()).collect();
            sorted.sort();
            format!("sub:{}", sorted.join(","))
        }
    }
}

pub fn fail_key(key: &str) -> String {
    format!("{key}:fail")
}

pub async fn cache_get(redis: &RedisStore, key: &str) -> Option<CacheEntry> {
    match tokio::time::timeout(CACHE_OP_TIMEOUT, redis.get(key)).await {
        Ok(Ok(Some(json))) => serde_json::from_str(&json).ok(),
        Ok(Ok(None)) => None,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, key, "view cache read failed");
            None
        }
        Err(_elapsed) => {
            tracing::warn!(key, "view cache read timed out");
            None
        }
    }
}

pub async fn cache_put(redis: &RedisStore, key: &str, entry: &CacheEntry, ttl_secs: u64) {
    let Ok(json) = serde_json::to_string(entry) else {
        return;
    };
    match tokio::time::timeout(CACHE_OP_TIMEOUT, redis.set_ex(key, &json, ttl_secs)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!(error = %e, key, "view cache write failed"),
        Err(_elapsed) => tracing::warn!(key, "view cache write timed out"),
    }
}

pub async fn fail_get(redis: &RedisStore, key: &str) -> Option<String> {
    match tokio::time::timeout(CACHE_OP_TIMEOUT, redis.get(&fail_key(key))).await {
        Ok(Ok(v)) => v,
        _ => None,
    }
}

pub async fn fail_put(redis: &RedisStore, key: &str, msg: &str, backoff_secs: u64) {
    let _ = tokio::time::timeout(
        CACHE_OP_TIMEOUT,
        redis.set_ex(&fail_key(key), msg, backoff_secs),
    )
    .await;
}

/// Whether a cached entry still counts as fresh under `policy`.
pub fn is_fresh(entry: Option<&CacheEntry>, policy: &CachePolicy, now: DateTime<Utc>) -> bool {
    entry.is_some_and(|e| now - e.computed_at < policy.fresh_for)
}

/// The read half, shared by every cached route.
///
/// Returns the envelope AND whether the caller should enqueue a recompute.
/// Splitting the decision from the spawn is what lets this be shared: the
/// decision is identical everywhere, while the spawn differs per route.
///
/// A failure marker suppresses re-enqueue even under `force`. Otherwise the
/// Refresh button becomes a way to bypass the backoff and hammer a query that
/// is already known to be failing — precisely when hammering hurts most.
pub async fn read(
    redis: &RedisStore,
    key: &str,
    policy: &CachePolicy,
    force: bool,
) -> (Envelope, bool) {
    let entry = cache_get(redis, key).await;
    let error = fail_get(redis, key).await;
    let fresh = is_fresh(entry.as_ref(), policy, Utc::now());
    let should_recompute = (force || !fresh) && error.is_none();

    let envelope = match entry {
        Some(e) => Envelope {
            state: if fresh {
                Freshness::Fresh
            } else {
                Freshness::Stale
            },
            computed_at: Some(e.computed_at),
            data: Some(e.data),
            error,
        },
        None => Envelope {
            state: Freshness::Computing,
            computed_at: None,
            data: None,
            error,
        },
    };
    (envelope, should_recompute)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn uuid(n: u8) -> Uuid {
        Uuid::from_bytes([n; 16])
    }

    fn policy() -> CachePolicy {
        CachePolicy {
            fresh_for: Duration::minutes(2),
            ttl_secs: 86_400,
            fail_backoff_secs: 60,
        }
    }

    #[test]
    fn a_claimed_key_cannot_be_claimed_twice() {
        let c = ViewCache::new(3);
        let first = c.claim("k");
        assert!(first.is_some());
        assert!(c.claim("k").is_none(), "a second claim must be refused");
        drop(first);
        assert!(
            c.claim("k").is_some(),
            "dropping the guard must release the key — otherwise one panicking \
             recompute wedges it until the process restarts"
        );
    }

    #[test]
    fn distinct_keys_claim_independently() {
        let c = ViewCache::new(3);
        let _a = c.claim("a");
        assert!(c.claim("b").is_some());
    }

    /// `One(x)` and `Subset([x])` are different SQL and must never share an
    /// entry: `Subset` compiles to `= ANY(...)`, which never matches `NULL`.
    #[test]
    fn every_env_variant_gets_a_distinct_token() {
        let x = uuid(1);
        let tokens = [
            env_token(&EnvFilter::All),
            env_token(&EnvFilter::One(x)),
            env_token(&EnvFilter::Unattributed),
            env_token(&EnvFilter::Subset(vec![x])),
        ];
        let unique: HashSet<&String> = tokens.iter().collect();
        assert_eq!(unique.len(), tokens.len(), "collision in {tokens:?}");
    }

    #[test]
    fn subset_order_does_not_change_the_token() {
        let (a, b) = (uuid(1), uuid(2));
        assert_eq!(
            env_token(&EnvFilter::Subset(vec![a, b])),
            env_token(&EnvFilter::Subset(vec![b, a])),
        );
    }

    #[test]
    fn freshness_is_decided_against_the_policy_window() {
        let now = Utc::now();
        let entry = CacheEntry {
            data: Value::Null,
            computed_at: now - Duration::minutes(1),
        };
        assert!(is_fresh(Some(&entry), &policy(), now));
        let old = CacheEntry {
            data: Value::Null,
            computed_at: now - Duration::minutes(5),
        };
        assert!(!is_fresh(Some(&old), &policy(), now));
        assert!(!is_fresh(None, &policy(), now), "a miss is never fresh");
    }
}
