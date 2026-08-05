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
fn stream_maxlen() -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("INGEST_STREAM_MAXLEN")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|n| *n > 0)
            .unwrap_or(1_000_000)
    })
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
    let ip = client_ip(&headers, &state.cfg);
    let user_agent = headers
        .get("user-agent")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let n = envelope.items.len();
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
