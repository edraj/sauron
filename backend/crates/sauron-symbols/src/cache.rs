//! A byte-bounded LRU with per-key single-flight.
//!
//! This is the hot tier of symbolication: parsed source-map indexes and DWARF
//! contexts are expensive to build but tiny to look up, so we keep the most
//! recently used ones resident, bounded by a **byte** budget (not a count —
//! `sourcesContent` makes entries wildly different sizes). Single-flight ensures
//! a freshly-deployed release doesn't make every worker parse the same 60 MB
//! file at once: concurrent misses for one key build exactly once.

use std::collections::{HashMap, VecDeque};
use std::hash::Hash;
use std::sync::{Arc, Mutex};

struct Entry<V> {
    value: Arc<V>,
    weight: usize,
}

struct Inner<K, V> {
    map: HashMap<K, Entry<V>>,
    /// LRU order; least-recently-used at the front, most-recently-used at back.
    order: VecDeque<K>,
    /// Per-key build locks so concurrent misses collapse to one build.
    flight: HashMap<K, Arc<tokio::sync::Mutex<()>>>,
    current_bytes: usize,
    budget: usize,
}

pub struct ByteLru<K, V> {
    inner: Arc<Mutex<Inner<K, V>>>,
}

impl<K, V> Clone for ByteLru<K, V> {
    fn clone(&self) -> Self {
        ByteLru {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<K, V> ByteLru<K, V>
where
    K: Eq + Hash + Clone + Send + 'static,
    V: Send + Sync + 'static,
{
    pub fn new(budget_bytes: usize) -> Self {
        ByteLru {
            inner: Arc::new(Mutex::new(Inner {
                map: HashMap::new(),
                order: VecDeque::new(),
                flight: HashMap::new(),
                current_bytes: 0,
                budget: budget_bytes,
            })),
        }
    }

    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Return the cached value for `key`, building it once on miss. `weight`
    /// reports the value's byte cost (used for budget eviction).
    pub async fn get_or_insert<W, F, Fut>(&self, key: K, weight: W, build: F) -> Arc<V>
    where
        W: FnOnce(&V) -> usize,
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = V>,
    {
        // Fast path: hit.
        if let Some(v) = self.touch_get(&key) {
            return v;
        }

        // Miss: acquire (or create) the per-key build lock, then serialize.
        let flight = {
            let mut g = self.inner.lock().unwrap();
            g.flight
                .entry(key.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        let _build_guard = flight.lock().await;

        // Another builder for this key may have finished while we waited.
        if let Some(v) = self.touch_get(&key) {
            return v;
        }

        // We are the sole builder. Build outside any lock.
        let value = build().await;
        let w = weight(&value);
        let arc = Arc::new(value);

        {
            let mut g = self.inner.lock().unwrap();
            // Evict LRU entries until the newcomer fits (always insert, even if a
            // single value exceeds the whole budget).
            while g.current_bytes + w > g.budget {
                let Some(lru_key) = g.order.pop_front() else {
                    break;
                };
                if let Some(e) = g.map.remove(&lru_key) {
                    g.current_bytes -= e.weight;
                }
            }
            g.map.insert(
                key.clone(),
                Entry {
                    value: Arc::clone(&arc),
                    weight: w,
                },
            );
            g.order.push_back(key.clone());
            g.current_bytes += w;
            // Value is now visible; drop the flight entry so later callers hit
            // the fast path (any current waiters already hold their own Arc).
            g.flight.remove(&key);
        }

        arc
    }

    /// Hit path: clone the value and mark it most-recently-used.
    fn touch_get(&self, key: &K) -> Option<Arc<V>> {
        let mut g = self.inner.lock().unwrap();
        if let Some(entry) = g.map.get(key) {
            let v = Arc::clone(&entry.value);
            // Move to MRU (back). O(n) over a small resident set.
            if let Some(pos) = g.order.iter().position(|k| k == key) {
                g.order.remove(pos);
            }
            g.order.push_back(key.clone());
            Some(v)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn evicts_by_byte_budget() {
        let lru: ByteLru<u32, Vec<u8>> = ByteLru::new(10);
        for k in 0..5u32 {
            lru.get_or_insert(k, |v| v.len(), || async move { vec![0u8; 4] })
                .await;
        }
        // budget 10, each value weighs 4 -> at most 2 resident.
        assert!(lru.len() <= 2, "resident {} should be <= 2", lru.len());
    }

    #[tokio::test]
    async fn single_flights_concurrent_misses() {
        let calls = Arc::new(AtomicUsize::new(0));
        let lru: ByteLru<u32, u32> = ByteLru::new(1000);
        let mut hs = vec![];
        for _ in 0..8 {
            let lru = lru.clone();
            let calls = calls.clone();
            hs.push(tokio::spawn(async move {
                lru.get_or_insert(
                    1u32,
                    |_| 1,
                    || {
                        let calls = calls.clone();
                        async move {
                            // Simulate a slow build so the misses overlap.
                            tokio::task::yield_now().await;
                            calls.fetch_add(1, Ordering::SeqCst);
                            42u32
                        }
                    },
                )
                .await
            }));
        }
        for h in hs {
            assert_eq!(*h.await.unwrap(), 42);
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1, "built more than once");
    }

    #[tokio::test]
    async fn returns_cached_value_on_hit() {
        let lru: ByteLru<&str, String> = ByteLru::new(1000);
        let a = lru
            .get_or_insert("k", |s| s.len(), || async { "hello".to_string() })
            .await;
        let b = lru
            .get_or_insert("k", |s| s.len(), || async { "world".to_string() })
            .await;
        assert_eq!(*a, "hello");
        assert_eq!(*b, "hello"); // second build never ran
    }
}
