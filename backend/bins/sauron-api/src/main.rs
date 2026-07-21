//! `sauron-api` — the JWT-authed dashboard API.
//!
//! Auth (register/login/refresh/logout), org/project management, the issues
//! API, and product-analytics queries. Every data route is scoped to the
//! caller's org/project membership.

mod admin_storage;
mod error;
mod routes;
mod symbolicate;
mod tier_read;

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, FromRef};
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE};
use axum::http::{HeaderValue, Method};
use axum::routing::{delete, get, patch, post};
use axum::Router;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use tracing::info;

use sauron_auth::JwtKeys;
use sauron_core::Config;
use sauron_db::PgPool;
use sauron_redis::{RedisStore, SymbolBlobCache};

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub redis: RedisStore,
    pub keys: JwtKeys,
    pub cfg: Arc<Config>,
    /// Isolated warm-blob cache for symbol artifacts (no-op when unconfigured).
    pub symbols: SymbolBlobCache,
    /// Shared symbolication engine (holds the in-process parsed-map cache).
    pub symbolicator: Arc<sauron_symbols::Symbolicator>,
}

impl FromRef<AppState> for JwtKeys {
    fn from_ref(state: &AppState) -> JwtKeys {
        state.keys.clone()
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    sauron_telemetry::init("sauron-api");
    let cfg = Config::from_env()?;

    let pool = sauron_db::build_pool(&cfg.database_url, 16)?;
    let redis = RedisStore::connect(&cfg.redis_url).await?;
    let keys = JwtKeys::new(&cfg.jwt_secret, cfg.jwt_access_ttl_secs);
    let symbols = SymbolBlobCache::connect(
        cfg.symbols_redis_url.as_deref(),
        cfg.symbols_redis_max_blob_mb * 1024 * 1024,
    )
    .await;
    let symbolicator = Arc::new(sauron_symbols::Symbolicator::new(
        cfg.symbols_cache_mb * 1024 * 1024,
    ));
    // Allow artifact uploads well above axum's 2 MB default body limit.
    let artifact_body_limit = (cfg.symbols_max_artifact_mb + 8) * 1024 * 1024;

    // Keep the seeded preset roles in sync with code.
    {
        let mut conn = sauron_db::conn(&pool).await?;
        sauron_auth::ensure_preset_roles(&mut conn).await?;
    }

    let port = cfg.api_port;
    let origins: Vec<HeaderValue> = cfg
        .cors_allowed_origins
        .iter()
        .filter_map(|o| o.parse().ok())
        .collect();

    let state = AppState {
        pool,
        redis,
        keys,
        cfg: Arc::new(cfg),
        symbols,
        symbolicator,
    };

    // Symbol-artifact routes carry large binary uploads, so they get their own
    // raised body limit (merged separately from the JSON API).
    let artifact_routes = Router::new()
        .route(
            "/v1/apps/{app_id}/artifacts",
            post(routes::artifacts::upload).get(routes::artifacts::list),
        )
        .route(
            "/v1/apps/{app_id}/artifacts/{artifact_id}",
            delete(routes::artifacts::delete),
        )
        .layer(DefaultBodyLimit::max(artifact_body_limit));

    let cors = CorsLayer::new()
        .allow_origin(origins)
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([AUTHORIZATION, CONTENT_TYPE]);

    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        // --- auth ---
        .route("/v1/auth/register", post(routes::auth::register))
        .route("/v1/auth/login", post(routes::auth::login))
        .route("/v1/auth/refresh", post(routes::auth::refresh))
        .route("/v1/auth/logout", post(routes::auth::logout))
        .route("/v1/me", get(routes::auth::me))
        // --- orgs, members, grants, roles ---
        .route(
            "/v1/orgs",
            get(routes::orgs::list_orgs).post(routes::orgs::create_org),
        )
        .route("/v1/orgs/{org_id}/access", get(routes::orgs::access))
        .route("/v1/orgs/{org_id}/members", get(routes::orgs::list_members))
        .route("/v1/orgs/{org_id}/grants", post(routes::orgs::create_grant))
        .route("/v1/grants/{grant_id}", delete(routes::orgs::delete_grant))
        .route(
            "/v1/orgs/{org_id}/roles",
            get(routes::orgs::list_roles).post(routes::orgs::create_role),
        )
        // --- projects (grouping) ---
        .route(
            "/v1/orgs/{org_id}/projects",
            get(routes::projects::list_projects).post(routes::projects::create_project),
        )
        .route(
            "/v1/projects/{project_id}",
            get(routes::projects::get_project)
                .patch(routes::projects::update_project)
                .delete(routes::projects::delete_project),
        )
        .route(
            "/v1/projects/{project_id}/apps",
            get(routes::projects::list_apps).post(routes::projects::create_app),
        )
        // --- apps ---
        .route(
            "/v1/apps/{app_id}",
            get(routes::apps::get_app)
                .patch(routes::apps::update_app)
                .delete(routes::apps::delete_app),
        )
        .route(
            "/v1/apps/{app_id}/rotate-key",
            post(routes::apps::rotate_key),
        )
        .route(
            "/v1/apps/{app_id}/environments",
            get(routes::apps::list_environments),
        )
        .route(
            "/v1/apps/{app_id}/first-event",
            get(routes::apps::first_event),
        )
        // --- issues (app-scoped) ---
        .route("/v1/apps/{app_id}/issues", get(routes::issues::list))
        .route(
            "/v1/apps/{app_id}/issues/{issue_id}",
            get(routes::issues::detail).patch(routes::issues::update),
        )
        .route(
            "/v1/apps/{app_id}/issues/{issue_id}/events",
            get(routes::issues::events),
        )
        // --- analytics (app-scoped) ---
        .route(
            "/v1/apps/{app_id}/events/top",
            get(routes::analytics::top_events),
        )
        .route(
            "/v1/apps/{app_id}/events/series",
            get(routes::analytics::event_series),
        )
        .route(
            "/v1/apps/{app_id}/events/list",
            get(routes::analytics::events_list),
        )
        .route(
            "/v1/apps/{app_id}/persons",
            get(routes::analytics::persons_list),
        )
        .route(
            "/v1/apps/{app_id}/persons/{distinct_id}",
            get(routes::analytics::person),
        )
        .route(
            "/v1/apps/{app_id}/overview",
            get(routes::analytics::overview),
        )
        .route(
            "/v1/apps/{app_id}/users/summary",
            get(routes::analytics::users_summary),
        )
        .route(
            "/v1/apps/{app_id}/errors/timeseries",
            get(routes::analytics::error_timeseries),
        )
        .route(
            "/v1/apps/{app_id}/events/timeseries",
            get(routes::analytics::event_timeseries),
        )
        .route(
            "/v1/apps/{app_id}/transactions/timeseries",
            get(routes::analytics::transaction_timeseries),
        )
        // --- exceptions dashboard ---
        .route("/v1/apps/{app_id}/issues/stats", get(routes::issues::stats))
        // --- sessions (app-scoped) ---
        .route("/v1/apps/{app_id}/sessions", get(routes::sessions::list))
        .route(
            "/v1/apps/{app_id}/sessions/summary",
            get(routes::analytics::sessions_summary),
        )
        .route(
            "/v1/apps/{app_id}/sessions/{session_id}",
            get(routes::sessions::detail),
        )
        // --- devices (app-scoped) ---
        .route("/v1/apps/{app_id}/devices", get(routes::devices::list))
        .route("/v1/apps/{app_id}/device", get(routes::devices::detail))
        // --- screens (app-scoped) ---
        .route("/v1/apps/{app_id}/screens", get(routes::screens::list))
        .route(
            "/v1/apps/{app_id}/screens/detail",
            get(routes::screens::detail),
        )
        // --- funnels & journeys ---
        .route("/v1/apps/{app_id}/funnel", post(routes::funnels::compute))
        .route(
            "/v1/apps/{app_id}/funnels",
            get(routes::funnels::list_saved).post(routes::funnels::create_saved),
        )
        .route(
            "/v1/apps/{app_id}/funnels/{funnel_id}",
            patch(routes::funnels::update_saved).delete(routes::funnels::delete_saved),
        )
        .route("/v1/apps/{app_id}/journeys", get(routes::journeys::explore))
        // --- uptime monitors (project-scoped) ---
        .route(
            "/v1/projects/{project_id}/monitors",
            get(routes::monitors::list).post(routes::monitors::create),
        )
        .route(
            "/v1/monitors/{monitor_id}",
            get(routes::monitors::detail)
                .patch(routes::monitors::update)
                .delete(routes::monitors::delete),
        )
        .route(
            "/v1/monitors/{monitor_id}/checks",
            get(routes::monitors::checks),
        )
        .route(
            "/v1/monitors/{monitor_id}/incidents",
            get(routes::monitors::incidents),
        )
        // --- performance (app-scoped) ---
        .route(
            "/v1/apps/{app_id}/performance/summary",
            get(routes::performance::summary),
        )
        .route(
            "/v1/apps/{app_id}/performance/series",
            get(routes::performance::series),
        )
        // --- storage & records (any authenticated user) ---
        .route("/v1/admin/storage", get(routes::admin::storage))
        .merge(artifact_routes)
        .layer(cors)
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    info!(%addr, "sauron-api listening");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
