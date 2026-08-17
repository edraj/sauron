//! Server-side result cache, background recompute and SSE fan-out for the
//! Overview page.
//!
//! # The problem this exists to solve
//!
//! The five Overview sections are live aggregates over the partitioned event
//! tables, so their cost scales with retained data rather than with the
//! caller's window. Measured on the reporting app that motivated this module:
//! top-issues ~5.7 s, top-events ~6.1 s, series ~7.0 s, active-users ~13.4 s
//! and totals past 30 s — where `main`'s `TimeoutLayer` maps a 30 s request
//! onto `SERVICE_UNAVAILABLE`. The KPI tiles therefore did not render at all:
//! not slowly, not partially, but as a 503 with nothing behind it.
//!
//! Splitting `/overview` into five sections (see `routes::analytics`) already
//! bought the MAX rather than the SUM. That is exhausted — the slowest section
//! alone is over the limit, so no amount of further parallelism helps.
//!
//! # The shape
//!
//! The query moves OFF the request path entirely. An HTTP request now only ever
//! does three cheap things — authorize, read Redis, maybe enqueue — so it
//! cannot reach the timeout no matter how slow the underlying aggregate is.
//! The aggregate runs in a background task and its result is pushed to the
//! browser over SSE.
//!
//! ```text
//!   GET /overview/totals ──▶ authorize ──▶ Redis GET ──┬─ fresh  ─▶ 200 {state:"fresh", data}
//!                                                      ├─ stale  ─▶ 200 {state:"stale", data}  ──┐
//!                                                      └─ miss   ─▶ 200 {state:"computing"}    ──┤
//!                                                                                                │ enqueue
//!   GET /overview/stream ─▶ subscribe to bus ─────────────────────── SSE ◀── recompute worker ◀──┘
//! ```
//!
//! # Why the Redis TTL is 24 h and the freshness threshold is 1 h
//!
//! These are two different numbers and conflating them breaks the design. The
//! product contract is "numbers may be up to an hour old, and the page says how
//! old" — that is the FRESHNESS threshold, read off `computed_at`. If the Redis
//! entry also *expired* at an hour, then the first request after the hour would
//! find nothing and be back to rendering a skeleton for 30 s, which is exactly
//! the stale-while-revalidate behaviour this is meant to provide. The entry
//! therefore survives 24 h and the 1 h mark only decides whether a recompute is
//! ALSO kicked off while the old value is served.
//!
//! # Why single-flight is load-bearing, not an optimization
//!
//! The DB pool is 16 connections for the whole process. Without deduplication,
//! five people looking at the same dashboard is five concurrent 30 s aggregates
//! holding five connections, and unrelated endpoints — including
//! `/v1/auth/login` and `/health` — start failing on pool checkout. Before this
//! module a slow query was at least bounded by the client giving up at the
//! timeout; a detached background task has no such backstop, so the bound has
//! to be built rather than inherited. Hence both a per-key in-flight set AND a
//! global permit count.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{broadcast, Semaphore};
use uuid::Uuid;

use sauron_db::repo;
use sauron_db::scope::{EnvFilter, ReadScope};

use crate::AppState;

/// How old a cached section may be before a background refresh is triggered.
///
/// Serving continues from the stale entry meanwhile — this is the "revalidate"
/// half of stale-while-revalidate, not an expiry. See the module docs.
const FRESH_FOR: Duration = Duration::hours(1);

/// How long an entry survives in Redis. Deliberately far longer than
/// [`FRESH_FOR`] so there is always something to serve instantly.
const REDIS_TTL_SECS: u64 = 24 * 60 * 60;

/// Backoff after a failed recompute.
///
/// Without this, a section whose query is broken (a bad plan, a missing index,
/// a genuinely un-runnable window) is retried on every single page load, and
/// the failure mode is a permanent background load spike that looks like
/// nothing in the request logs. One minute is long enough to stop the hammering
/// and short enough that a fix is picked up without an operator intervening.
const FAIL_BACKOFF_SECS: u64 = 60;

/// Budget for one Redis command.
///
/// Copied deliberately from `routes::active_users` rather than using an untimed
/// `get`/`set_ex`: `sauron-redis` builds its connection with
/// `set_response_timeout(None)`, so against a DEAD Redis a command hangs for
/// 9-19 s instead of erroring. "The cache is best-effort and we fall through to
/// the query" is only true for an error; an outage is a hang, and here it would
/// be a hang on the one path that is supposed to be unconditionally fast.
const CACHE_OP_TIMEOUT: StdDuration = StdDuration::from_millis(500);

/// Concurrent background recomputes across the whole process.
///
/// Three, matching `active_users_gate`, and for the same reason: the pool is 16
/// and everything else in the API has to keep working while these run.
const RECOMPUTE_PERMITS: usize = 3;

/// Capacity of the SSE fan-out channel.
///
/// A slow client that falls this far behind gets `RecvError::Lagged` and is
/// skipped forward rather than stalling the sender. That is the correct trade:
/// the browser re-reads the section over plain HTTP on reconnect, so a dropped
/// push costs a round trip, never correctness.
const BUS_CAPACITY: usize = 256;

/// The five independently-cacheable Overview sections.
///
/// The wire name is also the cache-key component and the SSE event name, so the
/// three cannot drift apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Section {
    Totals,
    Series,
    TopIssues,
    TopEvents,
    ActiveUsers,
}

impl Section {
    pub const ALL: [Section; 5] = [
        Section::Totals,
        Section::Series,
        Section::TopIssues,
        Section::TopEvents,
        Section::ActiveUsers,
    ];

    pub fn wire_name(self) -> &'static str {
        match self {
            Section::Totals => "totals",
            Section::Series => "series",
            Section::TopIssues => "top-issues",
            Section::TopEvents => "top-events",
            Section::ActiveUsers => "active-users",
        }
    }
}

/// Freshness of what is being returned, as the dashboard sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Freshness {
    /// Computed within [`FRESH_FOR`]. No recompute triggered.
    Fresh,
    /// Older than [`FRESH_FOR`]. Served as-is; a recompute is running.
    Stale,
    /// Nothing cached. `data` is `null`; a recompute is running and the answer
    /// will arrive over SSE.
    Computing,
}

/// What every section endpoint now returns.
///
/// `data` is `Option` because "computing" is a real, expected, 200-worthy
/// state — the whole point is that a cold read answers immediately instead of
/// occupying a request for 30 s. A caller that treats a missing `data` as an
/// error has misread the contract; the dashboard renders a skeleton.
#[derive(Debug, Clone, Serialize)]
pub struct Envelope {
    pub state: Freshness,
    /// When the query behind `data` actually ran. `None` iff `data` is `None`.
    /// This is what the Overview header renders as "Updated 14:32 · 42m ago".
    pub computed_at: Option<DateTime<Utc>>,
    pub data: Option<Value>,
    /// Set when the most recent recompute FAILED. Independent of `data`: a
    /// failure must never erase a good stale value, so both can be present —
    /// "here are yesterday's numbers, and the refresh is currently broken".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The cached document. `computed_at` travels with the payload rather than
/// being inferred from a Redis TTL, because the TTL is 24 h and says nothing
/// about the 1 h freshness question.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheEntry {
    data: Value,
    computed_at: DateTime<Utc>,
}

/// One recompute result, fanned out to every SSE subscriber.
#[derive(Debug, Clone, Serialize)]
pub struct SectionUpdate {
    /// `{app}:{env}:{days}` — see [`scope_token`]. SSE subscribers filter on
    /// an EXACT match of this, so a stream opened for app A / env X / 30 days
    /// can never be handed app B's payload, nor env Y's, nor the 7-day window's.
    ///
    /// Filtering on the scope rather than on the app alone is the whole
    /// safeguard: the bus is process-wide and carries every tenant's
    /// recomputes, so a substring or prefix match here would be a cross-tenant
    /// data leak with no error and no log.
    #[serde(skip)]
    pub scope: String,
    pub section: &'static str,
    pub state: Freshness,
    pub computed_at: Option<DateTime<Utc>>,
    pub data: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Process-wide recompute coordinator. Cheap to clone (all `Arc` inside); lives
/// in `AppState`.
#[derive(Clone)]
pub struct OverviewCache {
    /// Cache keys with a recompute already running. THE deduplication: without
    /// it, N concurrent viewers of one dashboard is N concurrent 30 s queries.
    inflight: Arc<Mutex<HashSet<String>>>,
    /// Global ceiling on concurrent recomputes, independent of how many
    /// distinct keys are in flight.
    permits: Arc<Semaphore>,
    bus: broadcast::Sender<SectionUpdate>,
}

impl Default for OverviewCache {
    fn default() -> Self {
        Self::new()
    }
}

impl OverviewCache {
    pub fn new() -> Self {
        let (bus, _) = broadcast::channel(BUS_CAPACITY);
        Self {
            inflight: Arc::new(Mutex::new(HashSet::new())),
            permits: Arc::new(Semaphore::new(RECOMPUTE_PERMITS)),
            bus,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<SectionUpdate> {
        self.bus.subscribe()
    }

    /// Claim the right to recompute `key`, or discover someone already has.
    ///
    /// Returns a guard that releases the claim on drop, so a panicking or
    /// early-returning recompute cannot wedge a key permanently — the failure
    /// mode of a bare `insert`/`remove` pair, and one that would present as
    /// "this section never refreshes again until the process restarts".
    fn claim(&self, key: &str) -> Option<InflightGuard> {
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

struct InflightGuard {
    inflight: Arc<Mutex<HashSet<String>>>,
    key: String,
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        let mut set = self.inflight.lock().unwrap_or_else(|e| e.into_inner());
        set.remove(&self.key);
    }
}

// ---------------------------------------------------------------------------
// Cache keys
// ---------------------------------------------------------------------------

/// Injective token for a resolved environment filter.
///
/// Every variant gets a distinct PREFIX, which is the whole point: `One(x)` and
/// `Subset([x])` are different queries — `Subset` compiles to `= ANY(...)`,
/// which never matches `NULL`, while `All` deliberately includes unattributed
/// rows — so they must never collide on a cache key. `routes::active_users`
/// carries a regression test for exactly this collision
/// (`all_and_a_full_subset_are_distinct_cache_keys`); the same hazard, so the
/// same guard, tested below.
///
/// `Subset` is sorted before formatting. The readable set comes out of an RBAC
/// join whose row order is not contractual, so an unsorted token would mint a
/// different cache key for the same caller depending on how Postgres felt about
/// the plan that day — a 0% hit rate with every test still green.
fn env_token(env: &EnvFilter) -> String {
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

/// `overview:v1:{section}:{app}:{env}:{days}`
///
/// # The `since_days` trap
///
/// This keys on `since_days` — the DISCRETE selector value — and never on the
/// derived `since` timestamp. `routes::analytics::since_of` computes
/// `Utc::now() - days`, which is a different value on every single request. A
/// cache keyed on that would mint a fresh entry per request and hit 0% of the
/// time, while compiling, passing every test, and showing a plausible
/// `computed_at` in the UI. The dashboard shipped this exact bug once already
/// on the client side (`CachedView`'s clock-derived `viewKey`); it is silent in
/// both directions, so the guard is a test, not a comment.
pub fn cache_key(section: Section, app_id: Uuid, env: &EnvFilter, since_days: i64) -> String {
    format!(
        "overview:v1:{}:{}:{}:{}",
        section.wire_name(),
        app_id,
        env_token(env),
        since_days
    )
}

/// Key of the short-lived failure marker paired with `key`. See
/// [`FAIL_BACKOFF_SECS`].
fn fail_key(key: &str) -> String {
    format!("{key}:fail")
}

/// The scope component shared by all five sections of one dashboard view.
///
/// The SSE stream filters on this so a subscriber only receives sections for
/// the app, environment and window it actually asked for.
pub fn scope_token(app_id: Uuid, env: &EnvFilter, since_days: i64) -> String {
    format!("{}:{}:{}", app_id, env_token(env), since_days)
}

/// The one place `since_days` is clamped.
///
/// Both the cache key and the query must clamp identically or a request for
/// 4000 days writes its answer under key `4000` and reads it back under `365`
/// forever after. Callers use this instead of clamping inline.
pub fn clamp_days(since_days: i64) -> i64 {
    since_days.clamp(1, 365)
}

// ---------------------------------------------------------------------------
// Redis I/O
// ---------------------------------------------------------------------------

async fn cache_get(state: &AppState, key: &str) -> Option<CacheEntry> {
    match tokio::time::timeout(CACHE_OP_TIMEOUT, state.redis.get(key)).await {
        Ok(Ok(Some(json))) => serde_json::from_str(&json).ok(),
        Ok(Ok(None)) => None,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, key, "overview cache read failed");
            None
        }
        Err(_elapsed) => {
            tracing::warn!(key, "overview cache read timed out");
            None
        }
    }
}

async fn cache_put(state: &AppState, key: &str, entry: &CacheEntry) {
    let Ok(json) = serde_json::to_string(entry) else {
        return;
    };
    match tokio::time::timeout(
        CACHE_OP_TIMEOUT,
        state.redis.set_ex(key, &json, REDIS_TTL_SECS),
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!(error = %e, key, "overview cache write failed"),
        Err(_elapsed) => tracing::warn!(key, "overview cache write timed out"),
    }
}

async fn fail_get(state: &AppState, key: &str) -> Option<String> {
    match tokio::time::timeout(CACHE_OP_TIMEOUT, state.redis.get(&fail_key(key))).await {
        Ok(Ok(v)) => v,
        _ => None,
    }
}

async fn fail_put(state: &AppState, key: &str, msg: &str) {
    let _ = tokio::time::timeout(
        CACHE_OP_TIMEOUT,
        state.redis.set_ex(&fail_key(key), msg, FAIL_BACKOFF_SECS),
    )
    .await;
}

// ---------------------------------------------------------------------------
// The handler entry point
// ---------------------------------------------------------------------------

/// Read a section from cache, kicking off a background recompute if what is
/// there is stale or absent.
///
/// Never runs the aggregate itself and never awaits one, so its latency is a
/// Redis round trip plus the caller's own authorization — bounded well under
/// the request timeout regardless of how expensive `section` is.
///
/// `force` skips the freshness check and always enqueues; it is what the
/// Refresh button sends. It still respects single-flight and the permit count,
/// so holding the button down cannot multiply load.
pub async fn read_section(
    state: &AppState,
    section: Section,
    scope: &ReadScope,
    since_days: i64,
    force: bool,
) -> Envelope {
    let days = clamp_days(since_days);
    let key = cache_key(section, scope.app_id, &scope.env, days);
    let entry = cache_get(state, &key).await;
    let error = fail_get(state, &key).await;

    let fresh = entry
        .as_ref()
        .is_some_and(|e| Utc::now() - e.computed_at < FRESH_FOR);

    // A failure marker suppresses re-enqueue even under `force`. Otherwise the
    // Refresh button becomes a way to bypass the backoff and hammer a query
    // that is already known to be failing — which is precisely when hammering
    // hurts most.
    let should_recompute = (force || !fresh) && error.is_none();
    if should_recompute {
        enqueue(state, section, scope.clone(), days, &key);
    }

    match entry {
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
    }
}

/// Every section's current state, as SSE frames, for a stream that has just
/// opened.
///
/// # Why the stream re-sends what the client already fetched
///
/// Without this there is a race the client cannot close: it GETs a section,
/// gets `computing`, and only then opens the stream. If the recompute finished
/// in that gap the push has already been fanned out to nobody, the value sits
/// in Redis unread, and the tile shows a skeleton forever — until something
/// unrelated triggers a reload. The window is small and the failure is
/// permanent, which is the worst combination to debug.
///
/// Emitting a snapshot on connect makes the stream self-sufficient: whatever
/// order the client does things in, it converges on the current value.
///
/// `sections` is passed in rather than assumed to be [`Section::ALL`] because
/// `top-issues` is `issue:read`-gated and must be omitted for callers who
/// cannot see it — the same check the section endpoint makes, which a snapshot
/// would otherwise route around.
pub async fn snapshot(
    state: &AppState,
    sections: &[Section],
    scope: &ReadScope,
    since_days: i64,
) -> Vec<SectionUpdate> {
    let days = clamp_days(since_days);
    let scope_tok = scope_token(scope.app_id, &scope.env, days);
    let mut out = Vec::with_capacity(sections.len());
    for &section in sections {
        // `force = false`: opening a stream is not a refresh request. It still
        // enqueues anything stale or missing, which is what makes a cold page
        // load start work without a separate kick.
        let env = read_section(state, section, scope, days, false).await;
        out.push(SectionUpdate {
            scope: scope_tok.clone(),
            section: section.wire_name(),
            state: env.state,
            computed_at: env.computed_at,
            data: env.data,
            error: env.error,
        });
    }
    out
}

/// Spawn a recompute unless one is already running for this key.
///
/// The permit is acquired INSIDE the task and before the DB checkout, never
/// around it from the caller: a permit held while waiting for a pool connection
/// (or vice versa) is a two-resource ordering that deadlocks under load, which
/// is the failure the ingest path already hit once.
fn enqueue(state: &AppState, section: Section, scope: ReadScope, days: i64, key: &str) {
    let Some(guard) = state.overview_cache.claim(key) else {
        return; // already running; the SSE push will serve both callers
    };
    let scope_tok = scope_token(scope.app_id, &scope.env, days);
    let state = state.clone();
    let key = key.to_string();
    tokio::spawn(async move {
        // Moved in so the claim outlives the whole recompute, including the
        // permit wait.
        let _guard = guard;
        let Ok(_permit) = state.overview_cache.permits.clone().acquire_owned().await else {
            return; // semaphore closed — process is shutting down
        };

        let started = Utc::now();
        let update = match compute(&state, section, scope, days).await {
            Ok(data) => {
                let entry = CacheEntry {
                    data: data.clone(),
                    computed_at: started,
                };
                cache_put(&state, &key, &entry).await;
                tracing::info!(
                    section = section.wire_name(),
                    elapsed_ms = (Utc::now() - started).num_milliseconds(),
                    "overview section recomputed"
                );
                SectionUpdate {
                    scope: scope_tok.clone(),
                    section: section.wire_name(),
                    state: Freshness::Fresh,
                    computed_at: Some(started),
                    data: Some(data),
                    error: None,
                }
            }
            Err(msg) => {
                tracing::warn!(section = section.wire_name(), error = %msg, "overview section recompute failed");
                fail_put(&state, &key, &msg).await;
                SectionUpdate {
                    scope: scope_tok.clone(),
                    section: section.wire_name(),
                    state: Freshness::Computing,
                    computed_at: None,
                    data: None,
                    error: Some(msg),
                }
            }
        };
        // A send with no subscribers is an error, not a problem: every viewer
        // may have navigated away while the query ran. The value is in Redis
        // either way, so the next reader gets it over plain HTTP.
        let _ = state.overview_cache.bus.send(update);
    });
}

// ---------------------------------------------------------------------------
// The queries
// ---------------------------------------------------------------------------

/// Run one section's aggregate and serialize it.
///
/// Returns `Value` rather than five different typed responses so the cache, the
/// SSE bus and the envelope have ONE shape to carry. The typed structs are
/// still the source of truth — they are what is serialized here — so the wire
/// format is unchanged from what each endpoint returned before; it has only
/// moved inside `data`.
async fn compute(
    state: &AppState,
    section: Section,
    scope: ReadScope,
    days: i64,
) -> Result<Value, String> {
    let to = Utc::now();
    let since = to - Duration::days(days);

    let value = match section {
        Section::Totals => {
            let mut conn = conn(state).await?;
            let totals = repo::overview_totals(&mut conn, scope, since)
                .await
                .map_err(|e| e.to_string())?;
            // Same two formulas the handler used to apply inline. Kept here
            // rather than on the client so a crash-free rate computed two ways
            // cannot start disagreeing.
            let error_rate = {
                let denom = totals.events + totals.errors;
                if denom > 0 {
                    totals.errors as f64 / denom as f64
                } else {
                    0.0
                }
            };
            let crash_free_sessions = if totals.sessions > 0 {
                1.0 - (totals.crashed_sessions as f64 / totals.sessions as f64)
            } else {
                1.0
            };
            // Serialized through the section's own struct, not a `json!`
            // literal. The struct stays the single definition of the wire
            // shape, so a field renamed there cannot silently keep working
            // here and start returning a differently-named key to the
            // dashboard.
            to_value(crate::routes::analytics::OverviewTotalsSection {
                totals,
                error_rate,
                crash_free_sessions,
            })?
        }
        Section::Series => {
            let mut conn = conn(state).await?;
            let events_series = repo::event_series(&mut conn, scope.clone(), None, since)
                .await
                .map_err(|e| e.to_string())?;
            let errors_series = repo::error_series(&mut conn, scope, since)
                .await
                .map_err(|e| e.to_string())?;
            to_value(crate::routes::analytics::OverviewSeriesSection {
                events_series,
                errors_series,
            })?
        }
        Section::TopIssues => {
            let mut conn = conn(state).await?;
            let rows = repo::top_issues(&mut conn, scope, since, 5)
                .await
                .map_err(|e| e.to_string())?;
            to_value(rows)?
        }
        Section::TopEvents => {
            let mut conn = conn(state).await?;
            let rows = repo::top_events(&mut conn, scope, since, 5)
                .await
                .map_err(|e| e.to_string())?;
            to_value(rows)?
        }
        Section::ActiveUsers => {
            // Goes through `tier_read`, not `repo`: this series can be served
            // partly from the cold Parquet tier, and bypassing the router would
            // silently drop every rotated day.
            let (series, partial_days) =
                crate::tier_read::active_users_by_day(state, scope, since, to)
                    .await
                    .map_err(|e| e.to_string())?;
            to_value(crate::routes::analytics::ActiveUsersSeries {
                series: series.into_iter().map(Into::into).collect(),
                partial_days,
            })?
        }
    };
    Ok(value)
}

/// `serde_json::to_value` with the error already flattened to the `String` the
/// recompute path carries.
fn to_value<T: Serialize>(v: T) -> Result<Value, String> {
    serde_json::to_value(v).map_err(|e| e.to_string())
}

/// Pool checkout.
///
/// Goes to `sauron_db::conn` rather than `routes::db` because the error has to
/// end up as a `String` for the SSE frame, and `ApiError` is deliberately not
/// `Display` — it renders as an HTTP response, which is exactly what a
/// detached background task has no way to return.
async fn conn(state: &AppState) -> Result<sauron_db::PgConn, String> {
    sauron_db::conn(&state.pool)
        .await
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Datelike;

    fn uuid(n: u8) -> Uuid {
        Uuid::from_bytes([n; 16])
    }

    /// The bug that makes a cache silently useless: keying on a clock-derived
    /// value. Two requests for the same selection, made at different instants,
    /// MUST share a key.
    ///
    /// This is the one test that would have caught the `CachedView` moving-key
    /// defect, and it cannot be written as an assertion about `since` — it has
    /// to assert about what the key is built FROM.
    #[test]
    fn the_key_is_stable_across_time() {
        let a = cache_key(Section::Totals, uuid(1), &EnvFilter::All, 30);
        // Nothing here reads the clock, which is the property under test:
        // rebuilding the key later must reproduce it byte for byte.
        let b = cache_key(Section::Totals, uuid(1), &EnvFilter::All, 30);
        assert_eq!(a, b);
        assert!(
            !a.contains(&Utc::now().year().to_string()),
            "key must not embed a timestamp: {a}"
        );
    }

    /// `One(x)` and `Subset([x])` are different SQL — `= ANY(...)` never
    /// matches NULL — so they must not share an entry.
    #[test]
    fn one_and_a_singleton_subset_are_distinct_keys() {
        let one = cache_key(Section::Totals, uuid(1), &EnvFilter::One(uuid(9)), 30);
        let sub = cache_key(
            Section::Totals,
            uuid(1),
            &EnvFilter::Subset(vec![uuid(9)]),
            30,
        );
        assert_ne!(one, sub);
    }

    /// `All` includes `environment_id IS NULL`; `Unattributed` is only those
    /// rows. Colliding them would serve the whole app's numbers as one
    /// environment's.
    #[test]
    fn all_and_unattributed_are_distinct_keys() {
        assert_ne!(
            cache_key(Section::Totals, uuid(1), &EnvFilter::All, 30),
            cache_key(Section::Totals, uuid(1), &EnvFilter::Unattributed, 30)
        );
    }

    /// The RBAC join's row order is not contractual, so an unsorted subset
    /// token would mint a new key per request for the same caller — a 0% hit
    /// rate that nothing else observes.
    #[test]
    fn subset_order_does_not_change_the_key() {
        let forward = EnvFilter::Subset(vec![uuid(1), uuid(2), uuid(3)]);
        let reversed = EnvFilter::Subset(vec![uuid(3), uuid(2), uuid(1)]);
        assert_eq!(
            cache_key(Section::Totals, uuid(7), &forward, 7),
            cache_key(Section::Totals, uuid(7), &reversed, 7)
        );
    }

    /// Two apps, two environments and two windows must never share an entry.
    #[test]
    fn every_scope_component_separates_keys() {
        let base = cache_key(Section::Totals, uuid(1), &EnvFilter::One(uuid(5)), 30);
        assert_ne!(
            base,
            cache_key(Section::Totals, uuid(2), &EnvFilter::One(uuid(5)), 30)
        );
        assert_ne!(
            base,
            cache_key(Section::Totals, uuid(1), &EnvFilter::One(uuid(6)), 30)
        );
        assert_ne!(
            base,
            cache_key(Section::Totals, uuid(1), &EnvFilter::One(uuid(5)), 7)
        );
        assert_ne!(
            base,
            cache_key(Section::Series, uuid(1), &EnvFilter::One(uuid(5)), 30)
        );
    }

    /// Both the key and the query clamp, and they must clamp identically —
    /// otherwise a 4000-day request writes under `4000` and reads under `365`.
    #[test]
    fn clamping_is_shared_by_key_and_query() {
        assert_eq!(clamp_days(4000), 365);
        assert_eq!(clamp_days(0), 1);
        assert_eq!(clamp_days(-5), 1);
        assert_eq!(clamp_days(30), 30);
    }

    /// A key claimed once cannot be claimed again — the single-flight property
    /// the pool depends on.
    #[test]
    fn a_claimed_key_cannot_be_claimed_twice() {
        let cache = OverviewCache::new();
        let first = cache.claim("k").expect("first claim succeeds");
        assert!(cache.claim("k").is_none(), "second claim must be refused");
        drop(first);
        assert!(
            cache.claim("k").is_some(),
            "claim must be releasable, or the key wedges forever"
        );
    }

    /// Distinct keys must not block each other — single-flight is per key, not
    /// a global mutex.
    #[test]
    fn distinct_keys_claim_independently() {
        let cache = OverviewCache::new();
        let _a = cache.claim("a").expect("a");
        assert!(cache.claim("b").is_some(), "b must not be blocked by a");
    }
}
