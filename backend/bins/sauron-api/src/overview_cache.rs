//! Server-side result cache, background recompute and SSE fan-out for the
//! Overview page.
//!
//! # The problem this exists to solve
//!
//! The five Overview sections are live aggregates over the partitioned event
//! tables, so their cost scales with retained data rather than with the
//! caller's window. Measured on the reporting app that motivated this module:
//! top-issues ~5.7 s, top-events ~6.1 s, series ~7.0 s, active-users ~13.4 s
//! and totals past the request budget (30 s then; see `REQUEST_TIMEOUT_SECS`)
//! — where `main`'s `TimeoutLayer` maps a timed-out request onto
//! `SERVICE_UNAVAILABLE`. The KPI tiles therefore did not render at all:
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

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::broadcast;
use uuid::Uuid;

use sauron_db::repo;
use sauron_db::scope::{EnvFilter, Range, ReadScope};

use crate::view_cache::{self, env_token, CacheEntry, CachePolicy, InflightGuard, ViewCache};
// Re-exported: these were defined here before the mechanism was extracted, and
// every route signature and utoipa schema still names them through this module.
pub use crate::view_cache::{Envelope, Freshness};
use crate::AppState;

/// How old a cached section may be before a background refresh is triggered.
///
/// Serving continues from the stale entry meanwhile — this is the "revalidate"
/// half of stale-while-revalidate, not an expiry. See the module docs.
///
/// Was 1 hour when every recompute was a multi-second raw aggregate. The
/// migration-71 rollup gates make a ready app's recompute a few milliseconds,
/// so revalidation can afford the dashboard's ~minute-fresh contract. For a
/// NOT-yet-backfilled app the old expensive query still runs — at worst 30×
/// more often than before, on access only, still bounded by single-flight,
/// the 3-permit ceiling and the failure backoff.
const FRESH_FOR: Duration = Duration::minutes(2);

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
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

/// One recompute result, fanned out to every SSE subscriber.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
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
    /// Single-flight + the concurrency ceiling, shared with every other cached
    /// route (`crate::view_cache`).
    inner: ViewCache,
    /// The one thing Overview does that no other cached route does: push each
    /// recompute to dashboards that are already open.
    bus: broadcast::Sender<SectionUpdate>,
}

/// Overview's timings. `FRESH_FOR` decides whether to ALSO recompute;
/// `REDIS_TTL_SECS` decides when the entry disappears. See `view_cache`'s
/// module docs for why they must not be the same number.
const POLICY: CachePolicy = CachePolicy {
    fresh_for: FRESH_FOR,
    ttl_secs: REDIS_TTL_SECS,
    fail_backoff_secs: FAIL_BACKOFF_SECS,
};

impl Default for OverviewCache {
    fn default() -> Self {
        Self::new()
    }
}

impl OverviewCache {
    pub fn new() -> Self {
        let (bus, _) = broadcast::channel(BUS_CAPACITY);
        Self {
            inner: ViewCache::new(RECOMPUTE_PERMITS),
            bus,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<SectionUpdate> {
        self.bus.subscribe()
    }

    /// Claim the right to recompute `key` — see [`ViewCache::claim`].
    fn claim(&self, key: &str) -> Option<InflightGuard> {
        self.inner.claim(key)
    }
}

// ---------------------------------------------------------------------------
// Cache keys
// ---------------------------------------------------------------------------

/// `overview:v2:{section}:{app}:{env}:{days}`
///
/// # Bump the version whenever a section's PAYLOAD or its MEANING changes
///
/// Entries live in Redis for 24 h, so a deploy that changes what a number means
/// keeps serving the old meaning under the old key until every entry ages out —
/// per app, per environment, per window, at different times. Nothing errors and
/// nothing logs; the dashboard just shows the previous answer and then quietly
/// starts showing the new one.
///
/// `v1` → `v2`: `crash_free_sessions` changed from "sessions with any error"
/// to "sessions with an UNCAUGHT error", and became nullable for apps whose SDK
/// never reports handledness (migration 0069). A `v1` entry carries the old
/// formula's number in a field the new client still accepts, so without this
/// bump the fix would appear not to work for a day.
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
pub fn cache_key(section: Section, app_id: Uuid, env: &EnvFilter, window: &Window) -> String {
    format!(
        "overview:v3:{}:{}:{}:{}",
        section.wire_name(),
        app_id,
        env_token(env),
        window.token()
    )
}

/// The window one Overview view covers.
///
/// Three variants rather than a plain `Range`, and the distinction is exactly
/// what keeps this cacheable. A `Range` built from `since_days` carries
/// `now - days`, which is a different value on every request; keying on it is
/// the 0%-hit-rate trap documented on [`cache_key`]. Every variant here has a
/// token that does NOT move with the clock:
///
/// - `Last(n)`  — the discrete day count, resolved against the clock only at
///   compute time, where it never reaches a key.
/// - `Since(t)` / `Between(f, t)` — instants the user picked, so they are
///   stable across requests and across users, which is what makes an absolute
///   custom range safe to cache at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Window {
    Last(i64),
    Since(DateTime<Utc>),
    /// `from` INCLUSIVE, `to` EXCLUSIVE.
    Between(DateTime<Utc>, DateTime<Utc>),
}

impl Window {
    /// `Last`, with the day count already through [`clamp_days`].
    ///
    /// The clamp has to happen BEFORE the token is built or a request for 4000
    /// days writes its answer under `4000d` and reads it back under `365d`
    /// forever after — the reason `clamp_days` exists as one function.
    pub fn last(since_days: i64) -> Self {
        Window::Last(clamp_days(since_days))
    }

    /// The window a caller's `since_days`/`from`/`to` describe.
    ///
    /// Precedence matches `search::resolve_range` exactly — explicit bounds
    /// win outright — because the two must agree: this decides what is CACHED
    /// and that decides what is QUERIED, and a disagreement would file one
    /// window's answer under another window's key.
    pub fn from_query(
        since_days: i64,
        from: Option<DateTime<Utc>>,
        to: Option<DateTime<Utc>>,
    ) -> Self {
        match (from, to) {
            (Some(f), Some(t)) => Window::Between(f, t),
            (Some(f), None) => Window::Since(f),
            // `to` alone is refused by `resolve_range` before it reaches here;
            // falling back to the relative window keeps this total without
            // inventing a second answer for a request that will 400 anyway.
            (None, _) => Window::last(since_days),
        }
    }

    /// The cache-key and SSE-filter component. Stable across requests.
    pub fn token(&self) -> String {
        match self {
            Window::Last(n) => format!("{n}d"),
            Window::Since(f) => format!("{}..", iso(*f)),
            Window::Between(f, t) => format!("{}..{}", iso(*f), iso(*t)),
        }
    }

    /// The bounds the query actually runs over. `now` is passed in rather than
    /// read here so the clock is visible at the one call site that needs it.
    pub fn range(&self, now: DateTime<Utc>) -> Range {
        match self {
            Window::Last(n) => Range::since(now - Duration::days(*n)),
            Window::Since(f) => Range::since(*f),
            Window::Between(f, t) => Range::new(*f, Some(*t)),
        }
    }
}

/// Second-precision RFC3339, always in `Z`.
///
/// Truncated deliberately: the dashboard sends local midnights, which carry no
/// sub-second component, and a format that could vary in precision would let
/// two spellings of the same instant key two entries.
fn iso(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Key of the short-lived failure marker paired with `key`. See
/// [`FAIL_BACKOFF_SECS`].
/// The scope component shared by all five sections of one dashboard view.
///
/// The SSE stream filters on this so a subscriber only receives sections for
/// the app, environment and window it actually asked for.
pub fn scope_token(app_id: Uuid, env: &EnvFilter, window: &Window) -> String {
    format!("{}:{}:{}", app_id, env_token(env), window.token())
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
    window: Window,
    force: bool,
) -> Envelope {
    let key = cache_key(section, scope.app_id, &scope.env, &window);
    // The freshness decision, the envelope and the failure-marker backoff are
    // identical for every cached route and live in `view_cache::read`; what is
    // Overview's own is only what happens on enqueue, below.
    let (envelope, should_recompute) = view_cache::read(&state.redis, &key, &POLICY, force).await;
    if should_recompute {
        enqueue(state, section, scope.clone(), window, &key);
    }
    envelope
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
    window: Window,
) -> Vec<SectionUpdate> {
    let scope_tok = scope_token(scope.app_id, &scope.env, &window);
    let mut out = Vec::with_capacity(sections.len());
    for &section in sections {
        // `force = false`: opening a stream is not a refresh request. It still
        // enqueues anything stale or missing, which is what makes a cold page
        // load start work without a separate kick.
        let env = read_section(state, section, scope, window, false).await;
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
fn enqueue(state: &AppState, section: Section, scope: ReadScope, window: Window, key: &str) {
    let Some(guard) = state.overview_cache.claim(key) else {
        return; // already running; the SSE push will serve both callers
    };
    let scope_tok = scope_token(scope.app_id, &scope.env, &window);
    let state = state.clone();
    let key = key.to_string();
    tokio::spawn(async move {
        // Moved in so the claim outlives the whole recompute, including the
        // permit wait.
        let _guard = guard;
        let Ok(_permit) = state.overview_cache.inner.permits().acquire_owned().await else {
            return; // semaphore closed — process is shutting down
        };

        let started = Utc::now();
        let update = match compute(&state, section, scope, window).await {
            Ok(data) => {
                let entry = CacheEntry {
                    data: data.clone(),
                    computed_at: started,
                };
                view_cache::cache_put(&state.redis, &key, &entry, POLICY.ttl_secs).await;
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
                view_cache::fail_put(&state.redis, &key, &msg, POLICY.fail_backoff_secs).await;
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
    window: Window,
) -> Result<Value, String> {
    let now = Utc::now();
    let range = window.range(now);
    // `active_users_by_day` predates `Range` and still takes two instants; an
    // open-above window ends at "now" for it, which is where the data ends.
    let to = range.to.unwrap_or(now);

    let value = match section {
        Section::Totals => {
            let mut conn = conn(state).await?;
            let totals = repo::overview_totals(&mut conn, scope, range)
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
            let crash_free_sessions = crate::routes::analytics::crash_free_rate(&totals);
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
            let events_series = repo::event_series(&mut conn, scope.clone(), None, range)
                .await
                .map_err(|e| e.to_string())?;
            let errors_series = repo::error_series(&mut conn, scope, range)
                .await
                .map_err(|e| e.to_string())?;
            to_value(crate::routes::analytics::OverviewSeriesSection {
                events_series,
                errors_series,
            })?
        }
        Section::TopIssues => {
            let mut conn = conn(state).await?;
            let rows = repo::top_issues(&mut conn, scope, range, 5)
                .await
                .map_err(|e| e.to_string())?;
            to_value(rows)?
        }
        Section::TopEvents => {
            let mut conn = conn(state).await?;
            let rows = repo::top_events(&mut conn, scope, range, 5)
                .await
                .map_err(|e| e.to_string())?;
            to_value(rows)?
        }
        Section::ActiveUsers => {
            // Goes through `tier_read`, not `repo`: this series can be served
            // partly from the cold Parquet tier, and bypassing the router would
            // silently drop every rotated day.
            let (series, partial_days) =
                crate::tier_read::active_users_by_day(state, scope, range.from, to)
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

    // -----------------------------------------------------------------------
    // Window tokens
    // -----------------------------------------------------------------------

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    /// The whole `since_days` trap, as a test rather than a comment. A key
    /// built from `now - days` differs on every request, so the entry written
    /// by one request is never read by the next — a 0% hit rate that compiles,
    /// passes every other test, and shows a plausible `computed_at` in the UI.
    #[test]
    fn a_relative_window_tokenizes_to_the_discrete_day_count() {
        let w = Window::Last(30);
        assert_eq!(w.token(), "30d");
        // Two "requests" a clock tick apart must agree.
        assert_eq!(Window::Last(30).token(), Window::Last(30).token());
        assert_ne!(Window::Last(30).token(), Window::Last(7).token());
    }

    /// Absolute bounds are chosen by the user, not derived from the clock, so
    /// they are stable across requests AND across users — which is what makes
    /// them safe to key on at all.
    #[test]
    fn absolute_windows_tokenize_to_their_bounds() {
        let w = Window::Between(at("2026-08-01T00:00:00Z"), at("2026-08-08T00:00:00Z"));
        assert_eq!(w.token(), "2026-08-01T00:00:00Z..2026-08-08T00:00:00Z");
        let open = Window::Since(at("2026-08-01T00:00:00Z"));
        assert_eq!(open.token(), "2026-08-01T00:00:00Z..");
        assert_ne!(w.token(), open.token());
    }

    /// `Last(30)` and an absolute window that happens to span thirty days are
    /// different questions — one moves with the clock and one does not — so
    /// they must never share an entry.
    #[test]
    fn a_relative_and_an_absolute_window_never_collide() {
        let rel = cache_key(Section::Totals, uuid(1), &EnvFilter::All, &Window::Last(30));
        let abs = cache_key(
            Section::Totals,
            uuid(1),
            &EnvFilter::All,
            &Window::Between(at("2026-07-09T00:00:00Z"), at("2026-08-08T00:00:00Z")),
        );
        assert_ne!(rel, abs);
    }

    /// `v2` keys ended in a bare `{days}`; `v3` ends in `{days}d`. Without the
    /// bump a `v2` entry written under `…:30` would be read back under the new
    /// code for up to 24 h — the same silent staleness the `v1` → `v2` bump
    /// exists to prevent.
    #[test]
    fn the_key_version_moved_with_the_format() {
        let k = cache_key(Section::Totals, uuid(1), &EnvFilter::All, &Window::Last(30));
        assert!(k.starts_with("overview:v3:"), "{k}");
        assert!(k.ends_with(":30d"), "{k}");
    }

    /// The SSE filter has to partition exactly as the cache key does, or a
    /// browser watching an absolute window is pushed a section computed for a
    /// relative one.
    #[test]
    fn the_scope_token_partitions_windows_too() {
        let a = scope_token(uuid(1), &EnvFilter::All, &Window::Last(30));
        let b = scope_token(uuid(1), &EnvFilter::All, &Window::Last(7));
        let c = scope_token(
            uuid(1),
            &EnvFilter::All,
            &Window::Between(at("2026-07-09T00:00:00Z"), at("2026-08-08T00:00:00Z")),
        );
        assert_ne!(a, b);
        assert_ne!(a, c);
    }

    /// `Last` is resolved against the clock at COMPUTE time, which is the one
    /// place a derived timestamp belongs: it never reaches a key.
    #[test]
    fn a_window_resolves_to_the_range_the_query_runs() {
        let now = at("2026-08-20T12:00:00Z");
        assert_eq!(Window::Last(7).range(now).from, now - Duration::days(7));
        assert_eq!(Window::Last(7).range(now).to, None);

        let f = at("2026-08-01T00:00:00Z");
        let t = at("2026-08-08T00:00:00Z");
        assert_eq!(Window::Between(f, t).range(now).from, f);
        assert_eq!(Window::Between(f, t).range(now).to, Some(t));
        assert_eq!(Window::Since(f).range(now).to, None);
    }

    /// The clamp is the one place a day count is bounded, and `Window::Last`
    /// must go through it — a request for 4000 days that wrote under key
    /// `4000d` and read back under `365d` would never hit.
    #[test]
    fn a_relative_window_is_clamped_before_it_becomes_a_token() {
        assert_eq!(Window::last(4000).token(), "365d");
        assert_eq!(Window::last(0).token(), "1d");
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
        let a = cache_key(Section::Totals, uuid(1), &EnvFilter::All, &Window::Last(30));
        // Nothing here reads the clock, which is the property under test:
        // rebuilding the key later must reproduce it byte for byte.
        let b = cache_key(Section::Totals, uuid(1), &EnvFilter::All, &Window::Last(30));
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
        let one = cache_key(
            Section::Totals,
            uuid(1),
            &EnvFilter::One(uuid(9)),
            &Window::Last(30),
        );
        let sub = cache_key(
            Section::Totals,
            uuid(1),
            &EnvFilter::Subset(vec![uuid(9)]),
            &Window::Last(30),
        );
        assert_ne!(one, sub);
    }

    /// `All` includes `environment_id IS NULL`; `Unattributed` is only those
    /// rows. Colliding them would serve the whole app's numbers as one
    /// environment's.
    #[test]
    fn all_and_unattributed_are_distinct_keys() {
        assert_ne!(
            cache_key(Section::Totals, uuid(1), &EnvFilter::All, &Window::Last(30)),
            cache_key(
                Section::Totals,
                uuid(1),
                &EnvFilter::Unattributed,
                &Window::Last(30)
            )
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
            cache_key(Section::Totals, uuid(7), &forward, &Window::Last(7)),
            cache_key(Section::Totals, uuid(7), &reversed, &Window::Last(7))
        );
    }

    /// Two apps, two environments and two windows must never share an entry.
    #[test]
    fn every_scope_component_separates_keys() {
        let base = cache_key(
            Section::Totals,
            uuid(1),
            &EnvFilter::One(uuid(5)),
            &Window::Last(30),
        );
        assert_ne!(
            base,
            cache_key(
                Section::Totals,
                uuid(2),
                &EnvFilter::One(uuid(5)),
                &Window::Last(30)
            )
        );
        assert_ne!(
            base,
            cache_key(
                Section::Totals,
                uuid(1),
                &EnvFilter::One(uuid(6)),
                &Window::Last(30)
            )
        );
        assert_ne!(
            base,
            cache_key(
                Section::Totals,
                uuid(1),
                &EnvFilter::One(uuid(5)),
                &Window::Last(7)
            )
        );
        assert_ne!(
            base,
            cache_key(
                Section::Series,
                uuid(1),
                &EnvFilter::One(uuid(5)),
                &Window::Last(30)
            )
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
}
