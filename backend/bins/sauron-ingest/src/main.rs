//! `sauron-ingest` — the SDK-facing edge.
//!
//! Authenticates by the DSN public key, rate-limits per project, validates the
//! envelope, and enqueues each item onto the Redis ingest stream. Worker tasks
//! (spawned here, co-located) drain the stream and write durable rows.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use serde::Deserialize;
use serde_json::json;
use tower_http::cors::{Any, CorsLayer};
use tower_http::decompression::RequestDecompressionLayer;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::trace::TraceLayer;
use tracing::{info, warn};
use uuid::Uuid;

use sauron_core::envelope::{Envelope, IngestBatch};
use sauron_core::Config;
use sauron_db::models::EnvRef;
use sauron_db::PgPool;
use sauron_redis::{keys, RedisStore};

#[derive(Clone)]
struct AppState {
    pool: PgPool,
    redis: RedisStore,
    cfg: Arc<Config>,
    dsn: Arc<DsnCache>,
}

/// Process-local, short-TTL cache of resolved ingest keys.
///
/// `resolve_env` already caches in Redis, but that is still a network round
/// trip on **every single request**, and it is the round trip that has to
/// finish before the app's rate-limit key is even known. Holding the answer in
/// process removes it and collapses the two limiter `INCR`s into one pipelined
/// round trip, taking the steady-state edge from four sequential Redis waits to
/// two.
///
/// ## The cost, stated plainly
///
/// Revocation is currently PROMPT: rotating a key, disabling an app or deleting
/// an environment all `DEL` the Redis cache entry (see
/// `sauron-api/src/routes/environments.rs` and `apps.rs`), so the next request
/// re-resolves. A process-local copy cannot be invalidated that way, so a
/// revoked key keeps ingesting for up to `ttl` on each replica. The default is
/// therefore deliberately small, and `INGEST_DSN_CACHE_SECS=0` disables the
/// cache entirely for deployments that need revocation to remain immediate —
/// the pipelined limiter still applies, so that setting costs one round trip,
/// not the whole change.
///
/// Only POSITIVE results are cached here. Unknown keys keep going to Redis,
/// whose 30-second negative cache is what stops them reaching Postgres, and a
/// key created moments after being rejected still starts working on schedule.
struct DsnCache {
    ttl: Duration,
    /// Bounded so a deployment with a pathological number of live keys cannot
    /// grow this without limit. Entries are only ever inserted for keys that
    /// resolved, so in practice the bound is the number of environments.
    inner: RwLock<HashMap<String, (Instant, EnvRef)>>,
}

/// Beyond this many live entries the cache stops accepting new ones rather than
/// growing. Far above any realistic environment count.
const DSN_CACHE_MAX: usize = 10_000;

/// Entries the ingest stream is trimmed to.
///
/// This is a DATA-LOSS control, not a tuning knob: when the worker falls behind
/// far enough for the backlog to reach it, Redis discards the oldest entries —
/// including ones no worker has read. Nothing logs it, nothing alerts on it,
/// and the edge has already told the SDK `202`. A soak caught it destroying
/// 47% of accepted events, and a 60-second saturation run reproduces it.
///
/// It was a literal at the call site, so an operator with the memory to spare
/// had no way to buy headroom. Default unchanged; `INGEST_STREAM_MAXLEN` now
/// raises it. Note the trim is `~` (approximate) — Redis trims at node
/// boundaries, so the stream sits slightly above whatever is set here.
///
/// Sizing it is a function of RATE, not of events: one entry now holds a whole
/// envelope, so the same cap covers roughly `items_per_envelope` times more
/// telemetry than it did when each item was its own entry.
///
/// The default is shared with the worker's retry drain and the API's manual
/// replay via `sauron_redis::INGEST_STREAM_MAXLEN_DEFAULT`. Three processes now
/// append to this stream and none can see the others' parse of the env var; a
/// re-enqueue with a smaller bound would trim live entries this binary had
/// already answered `202` to.
fn stream_maxlen() -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("INGEST_STREAM_MAXLEN")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|n| *n > 0)
            .unwrap_or(sauron_redis::INGEST_STREAM_MAXLEN_DEFAULT)
    })
}

/// Last successful reading of the Redis ingest stream, or `None`.
///
/// `None` covers "not sampled yet", "the last probe returned an error" and "the
/// last probe did not answer inside its deadline" — see [`probe_stream`], which
/// is what makes the third case reachable. All three render the stream gauges as
/// ABSENT rather than as zeroes. A stale reading served as if it were current
/// would be worse than no reading: these gauges exist to describe a condition
/// happening right now.
type StreamCache = Arc<RwLock<Option<sauron_telemetry::metrics::StreamSnapshot>>>;

/// Where `GET /metrics` is served: `INGEST_METRICS_ADDR`, defaulting to
/// `127.0.0.1:<INGEST_PORT + 10000>` (so `127.0.0.1:18081` for a stock ingest).
/// `INGEST_METRICS_ADDR=off` serves it nowhere; an EMPTY value means unset, see
/// [`resolve_metrics_addr`].
///
/// Loopback, and on a listener of its own rather than as a route on the
/// SDK-facing router. `packaging/rpm/SETUP.md` documents opening the ingest port
/// in the firewall for SDKs and only *prefers* fronting it with TLS, so a
/// `/metrics` route there would hand per-deployment event volumes to any caller
/// who knows the path. The separate listener also keeps scrapes outside the
/// body-limit / decompression / CORS / trace stack, so the hot router is
/// unchanged.
fn metrics_addr(ingest_port: u16) -> Option<SocketAddr> {
    let configured = std::env::var("INGEST_METRICS_ADDR").ok();
    resolve_metrics_addr(configured.as_deref(), ingest_port)
}

/// [`metrics_addr`] with the environment lifted out, so the empty-value and
/// `off` cases can be tested without mutating process-global state.
fn resolve_metrics_addr(configured: Option<&str>, ingest_port: u16) -> Option<SocketAddr> {
    // An EMPTY value counts as unset, not as `off`. `docker-compose.yml` passes
    // `${INGEST_METRICS_ADDR:-}` so that a `.env` override reaches a service
    // with no `env_file:`, which delivers an empty string on every stock `up`;
    // `sauron_core::config::var` filters empty the same way, and treating it as
    // "disabled" here would have turned the endpoint off for the whole compose
    // stack while the comment next to it claimed the binary's default applied.
    let configured = configured.map(str::trim).filter(|v| !v.is_empty());

    let raw = match configured {
        Some(v) => v.to_string(),
        // Derived from the ingest port rather than a fixed number, for a reason
        // that showed up while verifying this: the first fixed port tried, 9101,
        // was ALREADY LISTENING on the development machine — held by Flutter's
        // `dart:dartdev` tooling, per `ss -ltnp`. A fixed default loses to
        // whatever got there first, and quietly: the bind failure in
        // `spawn_metrics` is non-fatal, so the symptom is a missing metric
        // rather than a crash. Deriving it also gives two replicas on different
        // ingest ports different metrics ports for free.
        None => match ingest_port.checked_add(10_000) {
            Some(p) => format!("127.0.0.1:{p}"),
            None => {
                warn!(
                    ingest_port,
                    "no room for the default metrics port (ingest port + 10000 overflows); \
                     set INGEST_METRICS_ADDR to enable /metrics"
                );
                return None;
            }
        },
    };

    if raw.eq_ignore_ascii_case("off") || raw == "0" {
        return None;
    }
    match raw.parse::<SocketAddr>() {
        Ok(a) => Some(a),
        Err(e) => {
            warn!(value = raw, error = %e, "INGEST_METRICS_ADDR is not host:port; /metrics disabled");
            None
        }
    }
}

/// How often the Redis stream probe runs. `0` disables the probe; the process's
/// own counters are still served.
///
/// A fixed interval rather than a probe per scrape, for two reasons that are
/// properties of the reply rather than of the scraper: `XINFO STREAM` echoes
/// `first-entry` and `last-entry` with their full payloads, so its reply is
/// bounded by `INGEST_MAX_BODY_BYTES` rather than by anything small, and a
/// Redis stall would otherwise hang the scrape instead of just ageing the
/// numbers.
fn metrics_sample_secs() -> u64 {
    std::env::var("INGEST_METRICS_SAMPLE_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(15)
}

/// The deadline on ONE probe: half the sample interval, never below a second.
///
/// Without a deadline a probe against a hung Redis never returns at all, so it
/// is neither a success nor an error and the cache keeps its last reading
/// forever. Measured on this machine against a throwaway `redis:7` container
/// (`docker pause`, `INGEST_METRICS_SAMPLE_SECS=2`): 30 seconds of hang served
/// all six probe gauges from a snapshot taken before the pause and logged zero
/// warnings, while `docker stop` — where the socket actually breaks — withheld
/// them correctly. `set_response_timeout(None)` in `sauron_redis` is why: the
/// client waits for a reply that never comes.
///
/// **Half the interval**, so the deadline expires — and the hang is reported —
/// BEFORE the tick that would otherwise have refreshed the gauges. That
/// ordering is the whole property, and it is what the hang test asserts.
///
/// Deliberately NOT claimed here: that an over-running probe "delays later
/// ticks". `tokio::time::interval` defaults to `MissedTickBehavior::Burst`,
/// which fires missed ticks immediately rather than shifting the schedule, so
/// the effect would be bunching, not delay. The ordering above needs no such
/// claim.
///
/// How fast a hang surfaces depends on where in the interval it starts, so this
/// is stated as a mechanism rather than a latency: at a 2s interval and the
/// resulting 1s deadline, one run saw the gauges absent at the first poll after
/// `docker pause` and another after two polls. From then on a warning arrived
/// once per interval for as long as the hang lasted.
///
/// **Never below a second** because the interval is operator-set and can be 1.
/// One second is 482x the slowest healthy probe measured here, in the worst
/// reply-size case there is: `XINFO STREAM` echoes `first-entry` and
/// `last-entry` with their full payloads, so 20 `stream_stats` calls were timed
/// against a stream whose first AND last entry were both 1 MiB (the default
/// `INGEST_MAX_BODY_BYTES`) — min 965us, p50 1.57ms, max 2.07ms. The same 20
/// against 3 small entries ran 58-73us and against an empty stream 25-48us. A
/// second is also twice the 500ms `DEFAULT_RESPONSE_TIMEOUT` redis-rs would have
/// applied (redis 1.3.0, `src/client.rs:180`) and that `RedisStore::connect`
/// turns off for the whole shared connection. The strongest argument that the
/// floor is safe is that the one command a 1s timeout could genuinely harm — the
/// workers' blocking XREADGROUP — does not run on this connection at all; see
/// `probe_stream`.
///
/// KNOWN CORNER at the floor: when `INGEST_METRICS_SAMPLE_SECS=1` the floor makes
/// the deadline EQUAL the interval, so the "expires before the next tick"
/// ordering above does not hold, and probes are continuously in flight with no
/// idle gap. Measured with Redis paused for 42s at `SAMPLE_SECS=1`: 41 deadline
/// warnings (~1/s) and RSS 19,216 kB -> 19,532 kB across those 42 abandoned
/// probes, i.e. one abandoned request retained per interval (~0.3 MB / 42s).
/// Gauges stayed absent and recovery still worked. Negligible at the 15s default;
/// worth knowing before anyone lowers the interval to 1 in production.
fn probe_timeout(interval: Duration) -> Duration {
    (interval / 2).max(Duration::from_secs(1))
}

/// One bounded probe. `None` means the reading is UNKNOWN — error or no answer —
/// and the caller must withhold the gauges rather than keep the old ones.
///
/// The deadline is local to this call, deliberately. Making
/// `RedisStore::connect` set a response timeout instead would put one on the
/// shared `ConnectionManager`, which is also what the ingest hot path
/// (`xadd_job`/`xadd_jobs`) and the workers' reclaim sweeps (`claim_stale`'s
/// XAUTOCLAIM) use — both verified to run on `self.conn` — so a timeout there
/// could spuriously fail a large write on the accept path.
///
/// Note the comment at `sauron-redis/src/lib.rs:119` also calls a response
/// timeout "fatal for blocking XREADGROUP". That half no longer bears on the
/// shared connection and is NOT a reason for this decision: `read_group` takes a
/// caller-supplied connection (`lib.rs:353`, the crate's only `.block(...)` at
/// :363), and its sole production caller `worker.rs:166` obtains one from
/// `blocking_connection()`, which builds an INDEPENDENT connection with its own
/// `set_response_timeout(None)` (`lib.rs:129-136`). A `ConnectionManagerConfig`
/// timeout could never have reached XREADGROUP. That comment predates
/// `blocking_connection`.
///
/// Cancelling the future is safe for the shared connection.
/// `redis::aio::MultiplexedConnection` documents itself as "cancellation-safe,
/// and the user can drop request future without polling them to completion"
/// (redis 1.3.0, `src/aio/multiplexed_connection.rs:527`), and the reply
/// bookkeeping bears that out: the queue of in-flight requests lives in the
/// background sink task rather than in the caller's future, so an abandoned
/// request either keeps its slot and has its reply consumed and dropped
/// (`multiplexed_connection.rs:285`) or, if the sink had not written it yet, is
/// skipped entirely because its receiver is already closed
/// (`multiplexed_connection.rs:336`). Either way the framing stays aligned and no
/// stray reply can be handed to the next caller. `ConnectionManager` adds only an
/// `ArcSwap` load and a cloned shared connection future around that
/// (`src/aio/connection_manager.rs:691`). The same docs note the request itself
/// is NOT cancelled server-side, which is fine here: all three commands
/// (`XINFO STREAM`, `XINFO GROUPS`, `XLEN`) are read-only.
///
/// Verified end to end rather than only read: with this deadline in place, a
/// `docker pause`/`docker unpause` cycle on a throwaway `redis:7` — two probes
/// abandoned mid-flight — was followed by the gauges coming back on that same
/// shared connection, and by the co-located worker draining and dead-lettering a
/// newly appended entry over its own blocking `XREADGROUP`.
async fn probe_stream(
    redis: &RedisStore,
    deadline: Duration,
) -> Option<sauron_telemetry::metrics::StreamSnapshot> {
    let probe = redis.stream_stats(keys::INGEST_STREAM, keys::CONSUMER_GROUP, keys::INGEST_DLQ);
    match tokio::time::timeout(deadline, probe).await {
        Ok(Ok(s)) => Some(s),
        Ok(Err(e)) => {
            warn!(error = %e, "ingest stream probe failed; stream gauges withheld");
            None
        }
        // A healthy Redis too busy to answer in time lands here and is treated
        // exactly like a dead one: the gauges go absent (never to zero, never
        // stale) until a probe answers again. The item counters are untouched —
        // they are process-local and never involve Redis — so the loss
        // accounting keeps working while the corroborating live reading is
        // missing.
        Err(_) => {
            warn!(
                timeout_ms = deadline.as_millis(),
                "ingest stream probe did not answer within its deadline; stream gauges withheld"
            );
            None
        }
    }
}

/// Serve `/metrics` on its own listener, and keep the Redis reading fresh.
///
/// Both tasks are best-effort: a metrics listener that cannot bind (a port
/// already taken by another replica on the same host, say) must not stop the
/// process from ingesting, so the failure is logged and ingest carries on
/// without the endpoint.
fn spawn_metrics(addr: SocketAddr, redis: RedisStore) {
    let cache: StreamCache = Arc::new(RwLock::new(None));

    let secs = metrics_sample_secs();
    if secs > 0 {
        let cache = cache.clone();
        let interval = Duration::from_secs(secs);
        let deadline = probe_timeout(interval);
        info!(
            sample_secs = secs,
            probe_timeout_ms = deadline.as_millis(),
            "ingest stream probe"
        );
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(interval);
            loop {
                tick.tick().await;
                let next = probe_stream(&redis, deadline).await;
                if let Ok(mut slot) = cache.write() {
                    *slot = next;
                }
            }
        });
    } else {
        info!("INGEST_METRICS_SAMPLE_SECS=0; serving counters without the Redis stream gauges");
    }

    tokio::spawn(async move {
        let app = metrics_router(cache);
        match tokio::net::TcpListener::bind(addr).await {
            Ok(listener) => {
                info!(%addr, "sauron-ingest metrics listening");
                if let Err(e) = axum::serve(listener, app).await {
                    warn!(%addr, error = %e, "metrics listener stopped");
                }
            }
            Err(e) => {
                warn!(%addr, error = %e, "could not bind the metrics listener; /metrics unavailable")
            }
        }
    });
}

/// `/metrics` and nothing else. Carries none of the SDK router's layers, and
/// deliberately not `/health` — the two surfaces stay on separate ports so
/// neither's exposure decision implies the other's.
fn metrics_router(cache: StreamCache) -> Router {
    Router::new()
        .route("/metrics", get(metrics_text))
        .with_state(cache)
}

async fn metrics_text(State(cache): State<StreamCache>) -> impl IntoResponse {
    // The guard is dropped before rendering: `StreamSnapshot` is `Copy`, so
    // nothing needs the lock held across the (allocating) render.
    let stream = cache.read().ok().and_then(|slot| *slot);
    (
        StatusCode::OK,
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        sauron_telemetry::metrics::render(stream.as_ref()),
    )
}

impl DsnCache {
    fn new(ttl_secs: u64) -> DsnCache {
        DsnCache {
            ttl: Duration::from_secs(ttl_secs),
            inner: RwLock::new(HashMap::new()),
        }
    }

    fn enabled(&self) -> bool {
        !self.ttl.is_zero()
    }

    fn get(&self, key: &str) -> Option<EnvRef> {
        if !self.enabled() {
            return None;
        }
        let map = self.inner.read().ok()?;
        let (at, env) = map.get(key)?;
        (at.elapsed() < self.ttl).then(|| env.clone())
    }

    fn put(&self, key: &str, env: &EnvRef) {
        if !self.enabled() {
            return;
        }
        let Ok(mut map) = self.inner.write() else {
            return;
        };
        if map.len() >= DSN_CACHE_MAX && !map.contains_key(key) {
            // Drop everything expired before refusing. Doing it only when the
            // cap is reached keeps the hot path a plain hash lookup instead of
            // a scan.
            map.retain(|_, (at, _)| at.elapsed() < self.ttl);
            if map.len() >= DSN_CACHE_MAX {
                return;
            }
        }
        map.insert(key.to_string(), (Instant::now(), env.clone()));
    }
}

#[derive(Deserialize)]
struct IngestQuery {
    /// Beacon fallback key (sendBeacon cannot set headers).
    k: Option<String>,
}

/// Maximum items accepted in a single envelope. The per-app rate limit counts
/// requests, so an unbounded item list would let one request enqueue arbitrarily
/// many jobs and bypass the quota.
const MAX_ENVELOPE_ITEMS: usize = 1000;

/// Best-effort raise of the process's open-file-descriptor soft limit to the
/// hard limit. A large connect burst (e.g. crebain hammering over UDS) can
/// otherwise exhaust the default 1024-fd soft limit well before any real
/// resource pressure. Failure here is non-fatal; we just keep the inherited
/// limit.
fn raise_nofile() {
    #[cfg(unix)]
    unsafe {
        let mut lim = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) == 0 && lim.rlim_cur < lim.rlim_max {
            let new = libc::rlimit {
                rlim_cur: lim.rlim_max,
                rlim_max: lim.rlim_max,
            };
            let _ = libc::setrlimit(libc::RLIMIT_NOFILE, &new);
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    sauron_telemetry::init("sauron-ingest");
    raise_nofile();
    let cfg = Config::from_env()?;

    // Pool size must be >= WORKER_CONCURRENCY or the workers queue on
    // checkout rather than on Postgres, and raising the worker count alone
    // does nothing. Env-overridable so the two can be swept together.
    let pool_size: usize = std::env::var("INGEST_DB_POOL")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8);
    let pool = sauron_db::build_pool(&cfg.database_url, pool_size)?;

    // Does this binary's embedded migration set match what the database has
    // applied? An upgrade that replaced the binaries without re-running
    // `sauron-migrate` is otherwise invisible here and far more destructive
    // than on the API: the edge still answers the SDK `202`, and the worker
    // then fails every write, so telemetry is DESTROYED while every client
    // believes it was delivered. One query at boot; refuses to start when the
    // database is behind (see `sauron_db::require_current_schema`).
    //
    // The unreachable-database case is deliberately NOT fatal here, unlike in
    // `sauron-api`. `build_pool` is lazy and the edge resolves DSNs from Redis,
    // so today an ingest replica survives a Postgres outage by buffering into
    // the Redis stream for the workers to drain later. Turning this probe into
    // an eager connect requirement would throw that away and trade a silent
    // failure mode for an availability regression. Unknown status therefore
    // warns and continues; a KNOWN-behind schema still refuses.
    match sauron_db::conn(&pool).await {
        Ok(mut conn) => {
            sauron_db::require_current_schema(&mut conn, "sauron-ingest").await?;
        }
        Err(e) => warn!(
            error = %e,
            "could not reach Postgres to verify the schema against this binary; starting anyway \
             (the edge buffers to Redis through a database outage). If this replica was just \
             upgraded, run sauron-migrate."
        ),
    }

    let redis = RedisStore::connect(&cfg.redis_url).await?;

    // Shared symbolication resources for the hybrid write path (isolated cache +
    // in-process parsed-map LRU).
    let sym = sauron_pipeline::SymbolizeCtx::new(
        std::sync::Arc::new(sauron_symbols::Symbolicator::new(
            cfg.symbols_cache_mb * 1024 * 1024,
        )),
        sauron_redis::SymbolBlobCache::connect(
            cfg.symbols_redis_url.as_deref(),
            cfg.symbols_redis_max_blob_mb * 1024 * 1024,
        )
        .await,
        cfg.symbols_ingest_timeout_ms,
        cfg.symbols_max_uncompressed_mb * 1024 * 1024,
    );

    // One cache per process, shared by every worker task. `sauron-ingest` never
    // reads `inspector.env`, which is why INSPECTOR_POLICY_CACHE_SECS lives in
    // `sauron.env` — the "about 30 seconds" the API reports to the UI would
    // otherwise silently diverge from what the enforcer actually uses.
    let policies = std::sync::Arc::new(sauron_pipeline::mask::PolicyCache::new(
        pool.clone(),
        cfg.inspector_policy_cache_secs,
    ));

    // Spawn the co-located worker pool.
    let _workers = sauron_pipeline::spawn_workers(
        pool.clone(),
        redis.clone(),
        cfg.worker_concurrency,
        sym,
        policies,
    )
    .await?;

    // The guest → identified merge drain. One per process; several replicas
    // share the queue safely via FOR UPDATE SKIP LOCKED.
    //
    // No database read here on purpose, unlike the first cut of this call —
    // deliberately NOT eager, matching the schema probe above: `build_pool`
    // is lazy and the edge resolves DSNs from Redis, so an ingest replica
    // must survive a Postgres outage by buffering into the Redis stream
    // rather than failing to start. `cfg.tier_hot_days` is passed through
    // as the drain's fallback; `spawn_merge_worker`/`drain_once` resolve the
    // real, operator-overridable value themselves on every pass (see their
    // doc comments) rather than once here at boot.
    let _merge = sauron_pipeline::merge::spawn_merge_worker(pool.clone(), cfg.tier_hot_days);

    let port = cfg.ingest_port;
    let max_body = cfg.ingest_max_body_bytes;
    let uds_path = cfg.ingest_uds_path.clone();
    let backlog = cfg.ingest_backlog;
    // Short by design: this is the window in which a revoked key still ingests
    // on this replica. See `DsnCache`. `0` disables the cache and restores
    // immediate revocation at the cost of one Redis round trip per request.
    let dsn_ttl: u64 = std::env::var("INGEST_DSN_CACHE_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5);
    info!(dsn_cache_secs = dsn_ttl, "ingest key cache");

    let state = AppState {
        pool,
        redis,
        cfg: Arc::new(cfg),
        dsn: Arc::new(DsnCache::new(dsn_ttl)),
    };

    if let Some(addr) = metrics_addr(port) {
        spawn_metrics(addr, state.redis.clone());
    }

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/ready", get(ready))
        .route("/api/{project_id}/envelope", post(ingest))
        // Layer order matters. `Router::layer` makes the LAST-added layer the
        // outermost, so the limit must be added AFTER decompression to sit
        // outside it... which would only bound the compressed bytes. Instead we
        // apply a limit on both sides: the outer one bounds what we read off the
        // wire, the inner one bounds what decompression can expand it into, so a
        // zip bomb cannot inflate past the configured cap.
        .layer(RequestBodyLimitLayer::new(max_body))
        .layer(RequestDecompressionLayer::new())
        .layer(RequestBodyLimitLayer::new(max_body))
        .layer(cors)
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    if let Some(path) = uds_path {
        let _ = std::fs::remove_file(&path); // clear a stale socket file
        let listener = tokio::net::UnixListener::bind(&path)?;
        info!(path = %path, "sauron-ingest listening on UDS");
        axum::serve(listener, app).await?;
    } else {
        let addr = SocketAddr::from(([0, 0, 0, 0], port));
        let socket = tokio::net::TcpSocket::new_v4()?;
        socket.set_reuseaddr(true)?;
        socket.bind(addr)?;
        let listener = socket.listen(backlog)?;
        info!(%addr, "sauron-ingest listening");
        axum::serve(listener, app).await?;
    }
    Ok(())
}

async fn ready(State(state): State<AppState>) -> impl IntoResponse {
    if sauron_db::conn(&state.pool).await.is_err() {
        return (StatusCode::SERVICE_UNAVAILABLE, "db unavailable");
    }
    // Redis is not optional here: every accepted envelope is enqueued on the
    // stream, so without it this instance 500s on every request. Reporting
    // ready on the strength of Postgres alone kept load balancers sending
    // traffic to an instance that could not ingest anything.
    if state.redis.ping().await.is_err() {
        return (StatusCode::SERVICE_UNAVAILABLE, "redis unavailable");
    }
    (StatusCode::OK, "ready")
}

async fn ingest(
    State(state): State<AppState>,
    Path(_project_id): Path<Uuid>,
    Query(q): Query<IngestQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    // 1. Resolve the DSN public key.
    let key = headers
        .get("x-sauron-key")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .or(q.k);
    let Some(key) = key else {
        return error(StatusCode::UNAUTHORIZED, "missing_key", "no ingest key");
    };

    // 2-4. Rate-limit the key, resolve the environment, rate-limit the app.
    //
    // Three sequential Redis waits, because each step's input came out of the
    // one before it. The middle one is removable: `resolve_env`'s answer is the
    // same for ~every request in a window, so a short process-local cache turns
    // it into a hash lookup — and once the app is known WITHOUT asking Redis,
    // the two limiter counters can go out together in one pipeline.
    //
    // Steady state is therefore ONE round trip here, against three before. The
    // cold path below is unchanged, including the property that matters: an
    // unknown key is rate-limited before anything can reach Postgres.
    let key_rl = keys::key_rate_limit(&key);
    let limit = state.cfg.ingest_rate_limit_per_min;

    let env = if let Some(env) = state.dsn.get(&key) {
        let app_rl = keys::rate_limit(&env.app_id.to_string());
        match state
            .redis
            .rate_limit_ok_many(&[(key_rl.as_str(), limit), (app_rl.as_str(), limit)], 60)
            .await
        {
            Ok(verdicts) if verdicts.iter().any(|ok| !ok) => return rate_limited(),
            Ok(_) => {}
            // Same fail-open as the sequential version. A limiter that cannot
            // reach Redis must not take ingest down with it.
            Err(e) => warn!(error = %e, "rate limit check failed; allowing"),
        }
        // Re-checked on every request, not just on the miss: a cached entry
        // whose app was disabled mid-TTL must still be refused.
        if !(env.env_ingest_enabled && env.app_ingest_enabled) {
            return error(StatusCode::FORBIDDEN, "ingest_disabled", "ingest disabled");
        }
        env
    } else {
        // Rate-limit the KEY before resolving it. An unknown key would otherwise
        // miss the DSN cache on every request and hit Postgres unauthenticated,
        // letting anyone drain the small ingest pool with garbage keys.
        match state.redis.rate_limit_ok(&key_rl, limit, 60).await {
            Ok(true) => {}
            Ok(false) => return rate_limited(),
            Err(e) => warn!(error = %e, "key rate limit check failed; allowing"),
        }

        // Resolve the environment (cache → Postgres). Unknown keys are negatively
        // cached inside `resolve_env` so a repeat miss never reaches the database.
        let env = match resolve_env(&state, &key).await {
            Ok(Some(e)) if e.env_ingest_enabled && e.app_ingest_enabled => e,
            Ok(Some(_)) => {
                return error(StatusCode::FORBIDDEN, "ingest_disabled", "ingest disabled")
            }
            Ok(None) => {
                return error(
                    StatusCode::UNAUTHORIZED,
                    "invalid_key",
                    "unknown ingest key",
                )
            }
            Err(e) => {
                warn!(error = %e, "environment resolution failed");
                return error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal",
                    "resolution failed",
                );
            }
        };

        // Rate limit (fixed 60s window, per app).
        let app_rl = keys::rate_limit(&env.app_id.to_string());
        match state.redis.rate_limit_ok(&app_rl, limit, 60).await {
            Ok(true) => {}
            Ok(false) => return rate_limited(),
            Err(e) => warn!(error = %e, "rate limit check failed; allowing"),
        }

        // Cached only after it has passed every check, so a disabled app is
        // never the thing held in process.
        state.dsn.put(&key, &env);
        env
    };

    // 5. Parse the (already-decompressed) envelope.
    let envelope: Envelope = match serde_json::from_slice(&body) {
        Ok(e) => e,
        Err(e) => return error(StatusCode::BAD_REQUEST, "invalid_envelope", &e.to_string()),
    };
    // One request must not be able to enqueue an unbounded number of jobs: the
    // rate limit counts REQUESTS, so without this a single body could fan out to
    // tens of thousands of stream entries and bypass the quota entirely.
    if envelope.items.len() > MAX_ENVELOPE_ITEMS {
        return error(
            StatusCode::BAD_REQUEST,
            "too_many_items",
            &format!("envelope carries more than {MAX_ENVELOPE_ITEMS} items"),
        );
    }

    // 6. Enqueue the envelope as ONE stream entry.
    //
    // This used to be one entry per item, and then — once the round trips were
    // pipelined — one entry per item sent in a single round trip. Both shapes
    // wrote the envelope header (tenancy, release, ip, user agent, sdk, and the
    // unbounded `context` block) once PER ITEM, for information the envelope
    // states once. An 8-item batch paid 8x the serialization, 8x the bytes in
    // Redis, 8x the parse in the worker, and — because `MAXLEN` counts entries,
    // not events — consumed the stream's 1,000,000-entry budget 8x faster.
    //
    // The worker expands this back into per-item jobs, so nothing downstream of
    // the decode changed.
    let n = envelope.items.len();

    // An envelope with no items is ACCEPTED but not enqueued.
    //
    // `items` is `#[serde(default)]` (sauron-core/src/envelope.rs), so a body of
    // just `{"header":{...}}` parses fine, and the check above bounds the count
    // only from ABOVE. Measured before this change: such a POST returned 202
    // `{"accepted":0}` and still moved `XLEN` by 1 — an entry carrying nothing,
    // occupying one of `INGEST_STREAM_MAXLEN`'s slots, and displacing a real
    // event once the stream is at its cap. Under load that is not a curiosity:
    // the trim is silent, so the displaced events are simply gone.
    //
    // Returning 202 with the SAME body rather than a 400 is deliberate. Nothing
    // an SDK can observe changes, so no deployed client alters its behaviour —
    // and a 4xx here would be read by the retry/queue logic in several of our
    // own SDKs as a transient send failure worth resending, turning a harmless
    // no-op into a retry loop. That shape has bitten this project once already
    // (the 413 wedge).
    if n == 0 {
        sauron_telemetry::metrics::items_accepted(0);
        sauron_telemetry::metrics::empty_envelopes_dropped(1);
        return (StatusCode::ACCEPTED, Json(json!({ "accepted": 0 }))).into_response();
    }

    let ip = client_ip(&headers, &state.cfg);
    let user_agent = headers
        .get("user-agent")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let batch = IngestBatch {
        app_id: env.app_id,
        project_id: env.project_id,
        org_id: env.org_id,
        environment_id: env.env_id,
        release: envelope.header.release,
        received_at: Utc::now(),
        ip,
        user_agent,
        context: envelope.context,
        sdk: Some(envelope.header.sdk),
        items: envelope.items,
    };

    // An envelope is now all-or-nothing, where before a single unserializable
    // item was skipped and its neighbours still landed. That is a narrower hole
    // than it looks: every field here either came from `serde_json::from_slice`
    // moments ago or was built from a validated header, and JSON that parsed
    // cannot contain the map keys or non-finite floats that make `to_string`
    // fail. Reporting `accepted: 0` and letting the SDK retry the whole
    // envelope is the correct answer if it ever does.
    let payload = match serde_json::to_string(&batch) {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %e, items = n, "failed to serialize envelope; nothing enqueued");
            return (StatusCode::ACCEPTED, Json(json!({ "accepted": 0 }))).into_response();
        }
    };

    let accepted = match state.redis.xadd_job(&payload, stream_maxlen()).await {
        Ok(_) => n,
        // Nothing is known to have landed, so nothing is counted. The SDK sees
        // `accepted: 0` and retries, which is what it did before for an
        // envelope whose every XADD failed.
        Err(e) => {
            warn!(error = %e, items = n, "failed to enqueue envelope");
            0
        }
    };

    // The accounting half of `accepted-vs-persisted`. Counted from `accepted`
    // and never from `n`: the two paths above that answer 202 having enqueued
    // nothing both report `accepted: 0`, and using the envelope's item count
    // would inflate the counter on exactly the requests that lost the data.
    //
    // Per envelope rather than per item — `accepted` is added in one call, not
    // once per item. The exact counts, measured rather than reasoned:
    //   * enqueued with >= 1 item  -> TWO adds (this one + `envelopes_accepted`)
    //   * enqueued with ZERO items -> ONE add. `accepted` is 0, so the guard
    //     below is skipped.
    //   * `XADD` failed            -> ONE add, since `items_accepted(0)` still adds.
    // So an enqueued empty envelope and a failed XADD are indistinguishable in
    // both the add count and the counters.
    //
    // A zero-item envelope really is reachable: `items` is `#[serde(default)]`
    // (sauron-core/src/envelope.rs:46) and the check at line 694 bounds it only
    // from ABOVE. Measured against an isolated ingest: POSTing
    // `{"header":{"sdk":{"name":"x","version":"1"}}}` returned 202
    // `{"accepted":0}` and still moved XLEN by 1.
    //
    // Rejections earlier in this handler, and the serialization failure above,
    // return before reaching either call and so pay none.
    sauron_telemetry::metrics::items_accepted(accepted as u64);
    if accepted > 0 {
        // One envelope is one `XADD`, but this is NOT the count of stream entries
        // appended: a zero-item envelope appends an entry and never reaches this
        // line. Measured on one isolated process, `stream_entries_added` 607
        // against `envelopes_accepted_total` 603. Whether the edge should reject
        // empty envelopes instead (they also consume an `INGEST_STREAM_MAXLEN`
        // slot) changes accept-path behaviour and is not decided here.
        sauron_telemetry::metrics::envelopes_accepted(1);
    }

    (StatusCode::ACCEPTED, Json(json!({ "accepted": accepted }))).into_response()
}

/// Marker stored in the DSN cache for a key that resolved to nothing.
const NEGATIVE_CACHE_MARKER: &str = "\u{0}none";

/// Resolve an ingest key to its environment, caching the result in Redis.
///
/// Unknown keys are cached too (briefly). Without that, every request bearing a
/// bogus key is a guaranteed cache miss and therefore a database round-trip on
/// an unauthenticated path — a cheap way to exhaust the ingest pool. A retired
/// environment is excluded by the query, so its key is indistinguishable from an
/// unknown one and lands on the same path.
async fn resolve_env(state: &AppState, key: &str) -> anyhow::Result<Option<EnvRef>> {
    let cache_key = keys::dsn_cache(key);
    if let Some(cached) = state.redis.get(&cache_key).await? {
        if cached == NEGATIVE_CACHE_MARKER {
            return Ok(None);
        }
        if let Ok(e) = serde_json::from_str::<EnvRef>(&cached) {
            return Ok(Some(e));
        }
    }

    let mut conn = sauron_db::conn(&state.pool).await?;
    let resolved = sauron_db::repo::find_env_by_public_key(&mut conn, key).await?;
    drop(conn);

    match resolved {
        Some(eref) => {
            if let Ok(json) = serde_json::to_string(&eref) {
                let _ = state.redis.set_ex(&cache_key, &json, 300).await;
            }
            Ok(Some(eref))
        }
        None => {
            // Short TTL so a key that is created moments later still works.
            let _ = state
                .redis
                .set_ex(&cache_key, NEGATIVE_CACHE_MARKER, 30)
                .await;
            Ok(None)
        }
    }
}

fn rate_limited() -> axum::response::Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        [("retry-after", "60")],
        Json(json!({ "error": { "code": "rate_limited", "message": "quota exceeded" } })),
    )
        .into_response()
}

/// The client IP, honouring forwarding headers **only** when configured to.
///
/// `X-Forwarded-For` / `X-Real-IP` are trivially spoofable by any client. When
/// the service is exposed directly, trusting them lets a caller attribute events
/// to arbitrary addresses; `INGEST_TRUST_FORWARDED_HEADERS=1` is the operator's
/// assertion that a trusted reverse proxy sets them.
fn client_ip(headers: &HeaderMap, cfg: &Config) -> Option<String> {
    if !cfg.ingest_trust_forwarded_headers {
        return None;
    }
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            headers
                .get("x-real-ip")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
}

fn error(status: StatusCode, code: &str, message: &str) -> axum::response::Response {
    (
        status,
        Json(json!({ "error": { "code": code, "message": message } })),
    )
        .into_response()
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{UnixListener, UnixStream};

    /// Proves axum can serve a router over a Unix-domain socket in this axum
    /// version, independent of the full `AppState` (no PG/Redis needed).
    #[tokio::test]
    async fn serves_health_over_uds() {
        let path =
            std::env::temp_dir().join(format!("sauron-ingest-test-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);

        let router = axum::Router::new().route("/health", axum::routing::get(|| async { "ok" }));

        let listener = UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });

        let mut stream = UnixStream::connect(&path).await.unwrap();
        stream
            .write_all(b"GET /health HTTP/1.1\r\nHost: x\r\nconnection: close\r\n\r\n")
            .await
            .unwrap();

        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();

        assert!(response.contains("200"), "response was: {response}");
        assert!(response.contains("ok"), "response was: {response}");

        let _ = std::fs::remove_file(&path);
    }
}

#[cfg(test)]
mod metrics_endpoint_tests {
    use super::*;

    /// The empty case is the one that matters: compose always delivers
    /// `INGEST_METRICS_ADDR=""`, and reading that as `off` would silently
    /// disable the endpoint for every stock `docker compose up`.
    #[test]
    fn an_empty_value_means_unset_not_off() {
        assert_eq!(
            resolve_metrics_addr(None, 8081),
            Some("127.0.0.1:18081".parse().unwrap()),
            "unset must derive the default from the ingest port"
        );
        assert_eq!(
            resolve_metrics_addr(Some(""), 8081),
            Some("127.0.0.1:18081".parse().unwrap()),
            "an empty value must behave exactly like unset"
        );
        assert_eq!(
            resolve_metrics_addr(Some("   "), 8081),
            Some("127.0.0.1:18081".parse().unwrap()),
            "whitespace-only must behave like unset too"
        );

        // Derived, so a non-default ingest port moves the metrics port with it.
        assert_eq!(
            resolve_metrics_addr(None, 8095),
            Some("127.0.0.1:18095".parse().unwrap())
        );

        // Explicit opt-out, and an explicit override.
        assert_eq!(resolve_metrics_addr(Some("off"), 8081), None);
        assert_eq!(resolve_metrics_addr(Some("OFF"), 8081), None);
        assert_eq!(resolve_metrics_addr(Some("0"), 8081), None);
        assert_eq!(
            resolve_metrics_addr(Some("0.0.0.0:9999"), 8081),
            Some("0.0.0.0:9999".parse().unwrap())
        );

        // Unparseable disables rather than panicking at startup.
        assert_eq!(resolve_metrics_addr(Some("not-an-address"), 8081), None);

        // No room above 55535 for the +10000 default.
        assert_eq!(resolve_metrics_addr(None, 60000), None);
    }

    /// The metrics router carries `/metrics` and nothing else. Needs no
    /// `AppState`, no Postgres and no Redis — the same shape as
    /// `serves_health_over_uds`.
    #[tokio::test]
    async fn metrics_router_serves_only_metrics() {
        let cache: StreamCache = Arc::new(RwLock::new(None));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, metrics_router(cache)).await.unwrap();
        });

        let body = get(&addr, "/metrics").await;
        assert!(body.starts_with("HTTP/1.1 200"), "response was: {body}");
        assert!(
            body.contains("text/plain; version=0.0.4"),
            "Prometheus content type missing: {body}"
        );
        assert!(
            body.contains("# TYPE sauron_ingest_items_accepted_total counter"),
            "counters missing: {body}"
        );
        // No probe has run, so the entries-unit gauges must be ABSENT rather
        // than a row of zeroes claiming an empty, healthy stream.
        assert!(
            !body.contains("sauron_ingest_stream_"),
            "unprobed render leaked stream gauges: {body}"
        );

        let health = get(&addr, "/health").await;
        assert!(
            health.starts_with("HTTP/1.1 404"),
            "the metrics listener must not serve /health: {health}"
        );
    }

    /// Minimal HTTP/1.1 GET, so the test needs no HTTP client dependency.
    async fn get(addr: &SocketAddr, path: &str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(
                format!("GET {path} HTTP/1.1\r\nHost: x\r\nconnection: close\r\n\r\n").as_bytes(),
            )
            .await
            .unwrap();
        let mut out = String::new();
        stream.read_to_string(&mut out).await.unwrap();
        out
    }

    /// Half the interval, floored at a second — and, for every interval above
    /// the floor, strictly INSIDE the interval, which is the property that makes
    /// a hang visible before the tick that would have refreshed the gauges.
    #[test]
    fn the_probe_deadline_is_half_the_interval_with_a_one_second_floor() {
        assert_eq!(
            probe_timeout(Duration::from_secs(15)),
            Duration::from_millis(7500),
            "the default 15s interval must give a 7.5s deadline"
        );
        assert_eq!(
            probe_timeout(Duration::from_secs(2)),
            Duration::from_secs(1)
        );
        assert_eq!(
            probe_timeout(Duration::from_secs(1)),
            Duration::from_secs(1),
            "the floor must hold when half the interval falls below it"
        );
        for secs in [2u64, 3, 5, 15, 60, 3600] {
            let interval = Duration::from_secs(secs);
            assert!(
                probe_timeout(interval) < interval,
                "a {secs}s interval must leave its deadline inside the interval"
            );
        }
    }

    /// The `docker pause` case: the socket stays open and Redis never answers.
    ///
    /// This is the defect this deadline exists for. Measured before the fix, on
    /// a throwaway `redis:7` container at `INGEST_METRICS_SAMPLE_SECS=2`:
    /// 30 seconds of `docker pause` left all six probe gauges being served from
    /// a snapshot taken before the pause, and `grep -c 'stream probe failed'`
    /// returned 0. Nothing errored, because with
    /// `set_response_timeout(None)` there is nothing to error.
    ///
    /// The hang is produced by a real TCP proxy in front of a real Redis that
    /// stops forwarding, so every layer under test is the shipping one — the
    /// same `RedisStore`, the same `stream_stats` pipeline, the same socket.
    /// Nothing is stubbed and no loader is injected. The `probe_under_watchdog`
    /// helper is what makes a missing deadline FAIL rather than hang: delete the
    /// `tokio::time::timeout` from `probe_stream` and this test panics with the
    /// watchdog's message instead of running forever.
    #[tokio::test]
    async fn a_hung_redis_withholds_the_gauges_instead_of_serving_a_stale_reading() {
        let Some(url) = std::env::var("TEST_REDIS_URL").ok() else {
            eprintln!(
                "TEST_REDIS_URL unset — skipping \
                 a_hung_redis_withholds_the_gauges_instead_of_serving_a_stale_reading"
            );
            return;
        };
        let backend = redis_host_port(&url)
            .unwrap_or_else(|| panic!("TEST_REDIS_URL is not redis://host:port: {url}"));
        let (proxy, forwarding) = spawn_pausable_proxy(backend).await;
        let redis = RedisStore::connect(&format!("redis://{proxy}"))
            .await
            .expect("connect through the proxy");

        // 300ms rather than the shipped deadline: this test has to fit in a test
        // run, and what is under test is the presence of a deadline, not its
        // value (that is `the_probe_deadline_is_half_the_interval...`).
        let deadline = Duration::from_millis(300);

        // A healthy probe must still produce a reading, or "gauges absent"
        // proves nothing — a fix that withheld them always would pass the hang
        // assertion below and be useless.
        assert!(
            probe_under_watchdog(&redis, deadline).await.is_some(),
            "the probe must read a healthy Redis through the proxy"
        );

        // The pause. The socket stays open; the command is simply never
        // delivered, so no error will ever come back.
        forwarding.store(false, std::sync::atomic::Ordering::SeqCst);

        let started = Instant::now();
        let hung = probe_under_watchdog(&redis, deadline).await;
        assert!(
            hung.is_none(),
            "a hung Redis must yield no reading at all, got {hung:?}"
        );
        // `deadline * 2`, not `* 4`. At the 300ms deadline this test uses, a 4x
        // bound is 1.2s — loose enough that a `probe_stream` ignoring its
        // `deadline` argument entirely still passed. Measured: substituting a
        // hardcoded `Duration::from_millis(1100)` for `deadline` in the
        // `tokio::time::timeout` left all 5 tests GREEN, the only visible
        // difference being suite runtime (0.65s -> 2.25s). At 2x the same
        // substitution fails here, so this assertion now ties the probe's
        // effective deadline to the value it was handed.
        assert!(
            started.elapsed() < deadline * 2,
            "the probe took {:?}, which is not bounded by its {deadline:?} deadline",
            started.elapsed()
        );
        // A hang that persists must keep being reported, not reported once.
        assert!(probe_under_watchdog(&redis, deadline).await.is_none());

        // Unpause. The reading must come back on the SAME shared connection:
        // this is the cancellation-safety claim in `probe_stream` under test,
        // because the abandoned pipelines above were already on the wire.
        forwarding.store(true, std::sync::atomic::Ordering::SeqCst);
        let mut recovered = None;
        for _ in 0..20 {
            if let Some(s) = probe_under_watchdog(&redis, Duration::from_secs(2)).await {
                recovered = Some(s);
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(
            recovered.is_some(),
            "the probe never recovered after the hang ended; the abandoned request \
             left the shared connection unusable"
        );

        // Printed so a run can prove the body executed rather than taking the
        // TEST_REDIS_URL skip path above and still reporting "ok".
        println!("PROBE_HANG_TEST_RAN");
    }

    /// `probe_stream` under an outer watchdog, so a probe with no deadline of its
    /// own fails the test with a diagnosis instead of hanging the run forever.
    async fn probe_under_watchdog(
        redis: &RedisStore,
        deadline: Duration,
    ) -> Option<sauron_telemetry::metrics::StreamSnapshot> {
        let watchdog = deadline * 4 + Duration::from_secs(1);
        match tokio::time::timeout(watchdog, probe_stream(redis, deadline)).await {
            Ok(v) => v,
            Err(_) => panic!(
                "probe_stream did not return within {watchdog:?} for a {deadline:?} deadline: \
                 it has no deadline of its own, so a hung Redis wedges the probe loop and \
                 /metrics keeps serving the last snapshot forever"
            ),
        }
    }

    /// `host:port` out of a `redis://` URL, enough for the test proxy's backend.
    fn redis_host_port(url: &str) -> Option<String> {
        let rest = url
            .strip_prefix("redis://")
            .or_else(|| url.strip_prefix("rediss://"))?;
        let rest = rest.rsplit('@').next()?;
        let hostport = rest.split(['/', '?']).next()?;
        if hostport.is_empty() {
            return None;
        }
        Some(if hostport.contains(':') {
            hostport.to_string()
        } else {
            format!("{hostport}:6379")
        })
    }

    /// A TCP proxy in front of the real Redis whose client-to-server direction
    /// can be shut off — `docker pause` without pausing anything shared.
    ///
    /// Only that direction is gated, and gated bytes are HELD rather than
    /// dropped, so flipping the flag back delivers them and the connection
    /// carries on exactly as an unpaused container's would.
    async fn spawn_pausable_proxy(
        backend: String,
    ) -> (SocketAddr, Arc<std::sync::atomic::AtomicBool>) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let forwarding = Arc::new(AtomicBool::new(true));

        let gate = forwarding.clone();
        tokio::spawn(async move {
            while let Ok((client, _)) = listener.accept().await {
                let backend = backend.clone();
                let gate = gate.clone();
                tokio::spawn(async move {
                    let Ok(server) = tokio::net::TcpStream::connect(&backend).await else {
                        return;
                    };
                    let (mut from_client, mut to_client) = client.into_split();
                    let (mut from_server, mut to_server) = server.into_split();

                    let up = tokio::spawn(async move {
                        let mut buf = [0u8; 16 * 1024];
                        loop {
                            let n = match from_client.read(&mut buf).await {
                                Ok(0) | Err(_) => break,
                                Ok(n) => n,
                            };
                            while !gate.load(Ordering::SeqCst) {
                                tokio::time::sleep(Duration::from_millis(10)).await;
                            }
                            if to_server.write_all(&buf[..n]).await.is_err() {
                                break;
                            }
                        }
                    });
                    let down = tokio::spawn(async move {
                        let mut buf = [0u8; 16 * 1024];
                        loop {
                            let n = match from_server.read(&mut buf).await {
                                Ok(0) | Err(_) => break,
                                Ok(n) => n,
                            };
                            if to_client.write_all(&buf[..n]).await.is_err() {
                                break;
                            }
                        }
                    });
                    let _ = up.await;
                    let _ = down.await;
                });
            }
        });

        (addr, forwarding)
    }
}
