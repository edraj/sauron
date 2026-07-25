//! `sauron-ingest` — the SDK-facing edge.
//!
//! Authenticates by the DSN public key, rate-limits per project, validates the
//! envelope, and enqueues each item onto the Redis ingest stream. Worker tasks
//! (spawned here, co-located) drain the stream and write durable rows.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tower_http::cors::{Any, CorsLayer};
use tower_http::decompression::RequestDecompressionLayer;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::trace::TraceLayer;
use tracing::{info, warn};
use uuid::Uuid;

use sauron_core::envelope::{Envelope, IngestJob};
use sauron_core::Config;
use sauron_db::PgPool;
use sauron_redis::{keys, RedisStore};

#[derive(Clone)]
struct AppState {
    pool: PgPool,
    redis: RedisStore,
    cfg: Arc<Config>,
}

/// Cached app resolution keyed by DSN public key.
#[derive(Serialize, Deserialize, Clone)]
struct AppRef {
    app_id: Uuid,
    project_id: Uuid,
    org_id: Uuid,
    ingest_enabled: bool,
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

    let pool = sauron_db::build_pool(&cfg.database_url, 8)?;
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

    // Spawn the co-located worker pool.
    let _workers =
        sauron_pipeline::spawn_workers(pool.clone(), redis.clone(), cfg.worker_concurrency, sym)
            .await?;

    let port = cfg.ingest_port;
    let max_body = cfg.ingest_max_body_bytes;
    let uds_path = cfg.ingest_uds_path.clone();
    let backlog = cfg.ingest_backlog;
    let state = AppState {
        pool,
        redis,
        cfg: Arc::new(cfg),
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

    // 2. Rate-limit the KEY before resolving it. An unknown key would otherwise
    //    miss the DSN cache on every request and hit Postgres unauthenticated,
    //    letting anyone drain the small ingest pool with garbage keys.
    match state
        .redis
        .rate_limit_ok(
            &keys::key_rate_limit(&key),
            state.cfg.ingest_rate_limit_per_min,
            60,
        )
        .await
    {
        Ok(true) => {}
        Ok(false) => return rate_limited(),
        Err(e) => warn!(error = %e, "key rate limit check failed; allowing"),
    }

    // 3. Resolve the app (cache → Postgres). Unknown keys are negatively cached
    //    inside `resolve_app` so a repeat miss never reaches the database.
    let app = match resolve_app(&state, &key).await {
        Ok(Some(a)) if a.ingest_enabled => a,
        Ok(Some(_)) => return error(StatusCode::FORBIDDEN, "ingest_disabled", "ingest disabled"),
        Ok(None) => {
            return error(
                StatusCode::UNAUTHORIZED,
                "invalid_key",
                "unknown ingest key",
            )
        }
        Err(e) => {
            warn!(error = %e, "app resolution failed");
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "resolution failed",
            );
        }
    };

    // 4. Rate limit (fixed 60s window, per app).
    let rl_key = keys::rate_limit(&app.app_id.to_string());
    match state
        .redis
        .rate_limit_ok(&rl_key, state.cfg.ingest_rate_limit_per_min, 60)
        .await
    {
        Ok(true) => {}
        Ok(false) => return rate_limited(),
        Err(e) => warn!(error = %e, "rate limit check failed; allowing"),
    }

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

    // 6. Enqueue one job per item.
    let ip = client_ip(&headers, &state.cfg);
    let user_agent = headers
        .get("user-agent")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let received_at = Utc::now();
    let mut accepted = 0usize;

    for item in envelope.items {
        let job = IngestJob {
            app_id: app.app_id,
            project_id: app.project_id,
            org_id: app.org_id,
            environment: envelope.header.environment.clone(),
            release: envelope.header.release.clone(),
            received_at,
            ip: ip.clone(),
            user_agent: user_agent.clone(),
            context: envelope.context.clone(),
            item,
        };
        match serde_json::to_string(&job) {
            Ok(payload) => {
                if let Err(e) = state.redis.xadd_job(&payload, 1_000_000).await {
                    warn!(error = %e, "failed to enqueue job");
                } else {
                    accepted += 1;
                }
            }
            Err(e) => warn!(error = %e, "failed to serialize job"),
        }
    }

    (StatusCode::ACCEPTED, Json(json!({ "accepted": accepted }))).into_response()
}

/// Marker stored in the DSN cache for a key that resolved to nothing.
const NEGATIVE_CACHE_MARKER: &str = "\u{0}none";

/// Resolve an app by public key, caching the result in Redis.
///
/// Unknown keys are cached too (briefly). Without that, every request bearing a
/// bogus key is a guaranteed cache miss and therefore a database round-trip on
/// an unauthenticated path — a cheap way to exhaust the ingest pool.
async fn resolve_app(state: &AppState, key: &str) -> anyhow::Result<Option<AppRef>> {
    let cache_key = keys::dsn_cache(key);
    if let Some(cached) = state.redis.get(&cache_key).await? {
        if cached == NEGATIVE_CACHE_MARKER {
            return Ok(None);
        }
        if let Ok(a) = serde_json::from_str::<AppRef>(&cached) {
            return Ok(Some(a));
        }
    }

    let mut conn = sauron_db::conn(&state.pool).await?;
    let resolved =
        match sauron_db::repo::find_app_by_public_key(&mut conn, key).await? {
            Some(app) => sauron_db::repo::app_ancestry(&mut conn, app.id).await?.map(
                |(project_id, org_id)| AppRef {
                    app_id: app.id,
                    project_id,
                    org_id,
                    ingest_enabled: app.ingest_enabled,
                },
            ),
            None => None,
        };
    drop(conn);

    match resolved {
        Some(aref) => {
            if let Ok(json) = serde_json::to_string(&aref) {
                let _ = state.redis.set_ex(&cache_key, &json, 300).await;
            }
            Ok(Some(aref))
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
