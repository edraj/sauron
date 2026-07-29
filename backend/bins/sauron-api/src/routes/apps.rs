//! App management: read, update, delete, and the onboarding first-event poll.
//!
//! Key rotation and environment listing moved to `routes::environments` — the
//! ingest key now belongs to the environment, not the app.

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use sauron_auth::{authorize_app, perm, AuthUser};
use sauron_db::models::App;
use sauron_db::repo;
use sauron_redis::keys;

use super::db;
use crate::error::ApiError;
use crate::AppState;

pub async fn get_app(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
) -> Result<Json<App>, ApiError> {
    let mut conn = db(&state).await?;
    let app = authorize_app(&mut conn, auth.user_id, app_id, perm::APP_READ).await?;
    Ok(Json(app))
}

#[derive(Deserialize)]
pub struct UpdateAppReq {
    pub name: String,
    #[serde(default = "default_true")]
    pub ingest_enabled: bool,
}

fn default_true() -> bool {
    true
}

pub async fn update_app(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Json(req): Json<UpdateAppReq>,
) -> Result<Json<App>, ApiError> {
    let mut conn = db(&state).await?;
    // Read the pre-update state so we only pay for cache invalidation when
    // `ingest_enabled` actually flips — the request always carries a value
    // (defaulted to `true`), so it can't be used as a "did it change" signal
    // on its own. `authorize_app` already fetches the row, so bind it instead
    // of re-querying it via `repo::get_app`.
    let existing = authorize_app(&mut conn, auth.user_id, app_id, perm::APP_UPDATE).await?;
    if req.name.trim().is_empty() {
        return Err(ApiError::BadRequest("app name is required".into()));
    }
    let ingest_changed = existing.ingest_enabled != req.ingest_enabled;

    let app = repo::update_app(&mut conn, app_id, &req.name, req.ingest_enabled)
        .await?
        .ok_or(ApiError::NotFound)?;

    if ingest_changed {
        // Cache slots are keyed by environment key, not app key — but the cached
        // EnvRef carries `app_ingest_enabled`, so a mute (or unmute) toggled at
        // app level would otherwise take up to the full 300s positive TTL to
        // bite. Drop every one of this app's environment cache slots so it takes
        // effect immediately. Retired environments are already unresolvable
        // (`find_env_by_public_key` filters `retired_at IS NULL`), so only live
        // ones need revoking — and excluding retired ones keeps them from
        // consuming this call's 500-row cap ahead of the live keys that matter.
        let envs = repo::list_environments(&mut conn, app_id, false).await?;
        for env in &envs {
            if let Err(e) = state.redis.del(&keys::dsn_cache(&env.public_key)).await {
                tracing::warn!(error = %e, env_id = %env.id, "failed to invalidate ingest key cache");
            }
        }
    }
    Ok(Json(app))
}

pub async fn delete_app(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::APP_DELETE).await?;
    // The environments cascade away with the app, but their keys can still be
    // sitting in the ingest cache for the full positive TTL — during which a
    // deleted app keeps returning 202 for events that are then dropped on an FK
    // failure. Revoke every one of them. Retired environments are already
    // unresolvable (`find_env_by_public_key` filters `retired_at IS NULL`), so
    // only live ones need revoking — and excluding retired ones keeps them from
    // consuming this call's 500-row cap ahead of the live keys that matter.
    let envs = repo::list_environments(&mut conn, app_id, false).await?;
    repo::delete_app(&mut conn, app_id).await?;
    for env in &envs {
        if let Err(e) = state.redis.del(&keys::dsn_cache(&env.public_key)).await {
            tracing::warn!(error = %e, env_id = %env.id, "failed to invalidate ingest key cache");
        }
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Serialize)]
pub struct FirstEventResp {
    pub received: bool,
    /// Presence flags, not counts — the onboarding poll only needs "yet?".
    pub errors: bool,
    pub events: bool,
}

#[derive(Deserialize)]
pub struct FirstEventQuery {
    pub environment_id: Option<String>,
}

pub async fn first_event(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<FirstEventQuery>,
) -> Result<Json<FirstEventResp>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::APP_READ).await?;
    // Existence check only — this is polled every few seconds during onboarding,
    // and counting would scan every partition of the two largest tables.
    let scope = super::scope::read_scope(app_id, q.environment_id.as_deref())?;
    let (has_errors, has_events) = repo::app_has_events(&mut conn, scope).await?;
    Ok(Json(FirstEventResp {
        received: has_errors || has_events,
        errors: has_errors,
        events: has_events,
    }))
}
