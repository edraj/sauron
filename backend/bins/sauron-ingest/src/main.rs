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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    sauron_telemetry::init("sauron-ingest");
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
    let _workers = sauron_pipeline::spawn_workers(
        pool.clone(),
        redis.clone(),
        cfg.worker_concurrency,
        sym,
    )
    .await?;

    let port = cfg.ingest_port;
    let max_body = cfg.ingest_max_body_bytes;
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
        .layer(RequestDecompressionLayer::new())
        .layer(RequestBodyLimitLayer::new(max_body))
        .layer(cors)
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    info!(%addr, "sauron-ingest listening");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn ready(State(state): State<AppState>) -> impl IntoResponse {
    match sauron_db::conn(&state.pool).await {
        Ok(_) => (StatusCode::OK, "ready"),
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, "db unavailable"),
    }
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

    // 2. Resolve the app (cache → Postgres).
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

    // 3. Rate limit (fixed 60s window, per app).
    let rl_key = keys::rate_limit(&app.app_id.to_string());
    match state
        .redis
        .rate_limit_ok(&rl_key, state.cfg.ingest_rate_limit_per_min, 60)
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                [("retry-after", "60")],
                Json(json!({ "error": { "code": "rate_limited", "message": "quota exceeded" } })),
            )
                .into_response();
        }
        Err(e) => warn!(error = %e, "rate limit check failed; allowing"),
    }

    // 4. Parse the (already-decompressed) envelope.
    let envelope: Envelope = match serde_json::from_slice(&body) {
        Ok(e) => e,
        Err(e) => return error(StatusCode::BAD_REQUEST, "invalid_envelope", &e.to_string()),
    };

    // 5. Enqueue one job per item.
    let ip = client_ip(&headers);
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

/// Resolve an app by public key, caching the result in Redis for 5 minutes.
async fn resolve_app(state: &AppState, key: &str) -> anyhow::Result<Option<AppRef>> {
    let cache_key = keys::dsn_cache(key);
    if let Some(cached) = state.redis.get(&cache_key).await? {
        if let Ok(a) = serde_json::from_str::<AppRef>(&cached) {
            return Ok(Some(a));
        }
    }

    let mut conn = sauron_db::conn(&state.pool).await?;
    let Some(app) = sauron_db::repo::find_app_by_public_key(&mut conn, key).await? else {
        return Ok(None);
    };
    let Some((project_id, org_id)) = sauron_db::repo::app_ancestry(&mut conn, app.id).await? else {
        return Ok(None);
    };
    let aref = AppRef {
        app_id: app.id,
        project_id,
        org_id,
        ingest_enabled: app.ingest_enabled,
    };
    if let Ok(json) = serde_json::to_string(&aref) {
        let _ = state.redis.set_ex(&cache_key, &json, 300).await;
    }
    Ok(Some(aref))
}

fn client_ip(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .map(|s| s.trim().to_string())
        .or_else(|| {
            headers
                .get("x-real-ip")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
        })
}

fn error(status: StatusCode, code: &str, message: &str) -> axum::response::Response {
    (
        status,
        Json(json!({ "error": { "code": code, "message": message } })),
    )
        .into_response()
}
