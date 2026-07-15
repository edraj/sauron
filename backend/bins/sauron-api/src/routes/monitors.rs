//! Project-scoped uptime monitors: CRUD + read (checks, incidents).

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use sauron_auth::{authorize_project, perm, AuthUser};
use sauron_db::models::Monitor;
use sauron_db::repo;

use super::db;
use crate::error::ApiError;
use crate::AppState;

const KINDS: [&str; 2] = ["http", "tcp"];

/// Error message for an interval outside the allowed preset set.
fn invalid_interval_msg() -> String {
    let allowed = sauron_core::MONITOR_INTERVAL_PRESETS
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    format!("interval_seconds must be one of (seconds): {allowed}")
}

#[derive(Deserialize)]
pub struct RangeQuery {
    pub hours: Option<i64>,
}

#[derive(Deserialize)]
pub struct CreateMonitorReq {
    pub name: String,
    pub kind: String,
    pub target: String,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub config: Option<Value>,
    #[serde(default)]
    pub interval_seconds: Option<i32>,
    #[serde(default)]
    pub timeout_ms: Option<i32>,
    #[serde(default)]
    pub failure_threshold: Option<i32>,
    #[serde(default)]
    pub recovery_threshold: Option<i32>,
    #[serde(default)]
    pub webhook_url: Option<String>,
}

pub async fn list(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(project_id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_project(&mut conn, auth.user_id, project_id, perm::MONITOR_READ).await?;
    let rows = repo::list_monitors_for_project(&mut conn, project_id).await?;
    Ok(Json(json!(rows)))
}

pub async fn create(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(project_id): Path<Uuid>,
    Json(req): Json<CreateMonitorReq>,
) -> Result<Json<Monitor>, ApiError> {
    if req.name.trim().is_empty() {
        return Err(ApiError::BadRequest("monitor name is required".into()));
    }
    if !KINDS.contains(&req.kind.as_str()) {
        return Err(ApiError::BadRequest("kind must be 'http' or 'tcp'".into()));
    }
    if req.target.trim().is_empty() {
        return Err(ApiError::BadRequest("target is required".into()));
    }
    let interval = req.interval_seconds.unwrap_or(60);
    if !sauron_core::is_valid_monitor_interval(interval) {
        return Err(ApiError::BadRequest(invalid_interval_msg()));
    }

    let mut conn = db(&state).await?;
    authorize_project(&mut conn, auth.user_id, project_id, perm::MONITOR_WRITE).await?;

    let config = req.config.unwrap_or_else(|| json!({}));
    let new = sauron_db::models::NewMonitor {
        project_id,
        name: req.name.trim(),
        kind: &req.kind,
        target: req.target.trim(),
        method: req.method.as_deref().unwrap_or("GET"),
        config: &config,
        interval_seconds: interval,
        timeout_ms: req.timeout_ms.unwrap_or(10000).clamp(500, 120_000),
        failure_threshold: req.failure_threshold.unwrap_or(2).max(1),
        recovery_threshold: req.recovery_threshold.unwrap_or(1).max(1),
        webhook_url: req.webhook_url.as_deref().filter(|s| !s.is_empty()),
        created_by: Some(auth.user_id),
    };
    let m = repo::create_monitor(&mut conn, new).await?;
    Ok(Json(m))
}

async fn load_authorized(
    state: &AppState,
    user_id: Uuid,
    monitor_id: Uuid,
    perm: &str,
) -> Result<(sauron_db::PgConn, Monitor), ApiError> {
    let mut conn = db(state).await?;
    let project_id = repo::monitor_project(&mut conn, monitor_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    authorize_project(&mut conn, user_id, project_id, perm).await?;
    let m = repo::get_monitor(&mut conn, monitor_id).await?.ok_or(ApiError::NotFound)?;
    Ok((conn, m))
}

pub async fn detail(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(monitor_id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let (mut conn, m) = load_authorized(&state, auth.user_id, monitor_id, perm::MONITOR_READ).await?;
    let uptime_24h = repo::uptime_pct(&mut conn, monitor_id, 24).await?;
    let uptime_7d = repo::uptime_pct(&mut conn, monitor_id, 24 * 7).await?;
    let uptime_30d = repo::uptime_pct(&mut conn, monitor_id, 24 * 30).await?;
    let incidents = repo::list_incidents(&mut conn, monitor_id, 20).await?;
    Ok(Json(json!({
        "monitor": m,
        "uptime": { "h24": uptime_24h, "d7": uptime_7d, "d30": uptime_30d },
        "incidents": incidents,
    })))
}

#[derive(Deserialize)]
pub struct UpdateMonitorReq {
    pub name: Option<String>,
    pub enabled: Option<bool>,
    pub interval_seconds: Option<i32>,
    pub webhook_url: Option<Option<String>>,
}

pub async fn update(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(monitor_id): Path<Uuid>,
    Json(req): Json<UpdateMonitorReq>,
) -> Result<Json<Monitor>, ApiError> {
    if let Some(i) = req.interval_seconds {
        if !sauron_core::is_valid_monitor_interval(i) {
            return Err(ApiError::BadRequest(invalid_interval_msg()));
        }
    }
    let _ = load_authorized(&state, auth.user_id, monitor_id, perm::MONITOR_WRITE).await?;
    let mut conn = db(&state).await?;
    // Pausing/enabling flips status too.
    let status = req.enabled.map(|e| if e { "unknown" } else { "paused" });
    let interval = req.interval_seconds;
    let webhook = req.webhook_url.map(|w| w.as_deref().filter(|s| !s.is_empty()).map(|s| s.to_string()));
    let webhook_ref = webhook.as_ref().map(|w| w.as_deref());
    let m = repo::update_monitor(
        &mut conn,
        monitor_id,
        req.name.as_deref(),
        req.enabled,
        status,
        interval,
        webhook_ref,
    )
    .await?
    .ok_or(ApiError::NotFound)?;
    Ok(Json(m))
}

pub async fn delete(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(monitor_id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let _ = load_authorized(&state, auth.user_id, monitor_id, perm::MONITOR_WRITE).await?;
    let mut conn = db(&state).await?;
    repo::delete_monitor(&mut conn, monitor_id).await?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn checks(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(monitor_id): Path<Uuid>,
    Query(q): Query<RangeQuery>,
) -> Result<Json<Value>, ApiError> {
    let (mut conn, _m) = load_authorized(&state, auth.user_id, monitor_id, perm::MONITOR_READ).await?;
    let hours = q.hours.unwrap_or(24).clamp(1, 24 * 90);
    let series = repo::latency_series(&mut conn, monitor_id, hours).await?;
    Ok(Json(json!(series)))
}

pub async fn incidents(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(monitor_id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let (mut conn, _m) = load_authorized(&state, auth.user_id, monitor_id, perm::MONITOR_READ).await?;
    let rows = repo::list_incidents(&mut conn, monitor_id, 50).await?;
    Ok(Json(json!(rows)))
}
