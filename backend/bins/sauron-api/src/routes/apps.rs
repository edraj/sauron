//! App management: read, update, delete, key rotation, environments, and the
//! onboarding first-event poll.

use axum::extract::{Path, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use sauron_auth::{authorize_app, perm, AuthUser};
use sauron_core::ids;
use sauron_db::models::{App, Environment};
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
    authorize_app(&mut conn, auth.user_id, app_id, perm::APP_UPDATE).await?;
    if req.name.trim().is_empty() {
        return Err(ApiError::BadRequest("app name is required".into()));
    }
    let app = repo::update_app(&mut conn, app_id, &req.name, req.ingest_enabled)
        .await?
        .ok_or(ApiError::NotFound)?;
    // Ingest state changed → drop the DSN cache so it re-resolves.
    let _ = state.redis.del(&keys::dsn_cache(&app.public_key)).await;
    Ok(Json(app))
}

pub async fn delete_app(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut conn = db(&state).await?;
    let app = authorize_app(&mut conn, auth.user_id, app_id, perm::APP_DELETE).await?;
    repo::delete_app(&mut conn, app_id).await?;
    let _ = state.redis.del(&keys::dsn_cache(&app.public_key)).await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

pub async fn rotate_key(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
) -> Result<Json<App>, ApiError> {
    let mut conn = db(&state).await?;
    let app = authorize_app(&mut conn, auth.user_id, app_id, perm::APP_ROTATE_KEY).await?;
    let new_key = ids::public_key();
    let updated = repo::rotate_app_key(&mut conn, app_id, &new_key).await?;
    // Invalidate the old key's cache entry.
    let _ = state.redis.del(&keys::dsn_cache(&app.public_key)).await;
    Ok(Json(updated))
}

pub async fn list_environments(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
) -> Result<Json<Vec<Environment>>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::APP_READ).await?;
    Ok(Json(repo::list_environments(&mut conn, app_id).await?))
}

#[derive(Serialize)]
pub struct FirstEventResp {
    pub received: bool,
    pub errors: i64,
    pub events: i64,
}

pub async fn first_event(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
) -> Result<Json<FirstEventResp>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::APP_READ).await?;
    let (errors, events) = repo::app_event_counts(&mut conn, app_id).await?;
    Ok(Json(FirstEventResp {
        received: errors + events > 0,
        errors,
        events,
    }))
}
