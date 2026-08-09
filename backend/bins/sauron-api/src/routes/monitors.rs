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

/// Serialize a monitor for API consumption: adds the derived flags that
/// replace the credentials the model redacts.
///
/// `Monitor::webhook_url` is `skip_serializing` and `Monitor::config` is
/// projected down to `sauron_db::models::PUBLIC_PROBE_CONFIG_KEYS` by the field
/// serializer (see its doc for why the probe's `headers` and `body` are
/// credentials and not settings). What a caller legitimately needs is the
/// *existence* signal, not the value: `has_webhook` answers "is state-change
/// notification wired up?", `probe_header_names` answers "which headers does
/// this probe send?" and `has_probe_body` answers "does it POST a payload?" —
/// none of them hands over a value. Header names are not secrets; their values,
/// and the body they accompany, are.
///
/// These three are the reason the config projection can be an allowlist without
/// the omission being invisible: a key that vanishes silently is a gap nobody
/// investigates, so anything dropped for credential reasons gets a signal here.
///
/// Deliberately not a masked/partial value. For a Slack hook the host alone
/// identifies the vendor and the path *is* the secret, and a partial reveal
/// only invites "show a bit more" drift. Same shape as `channel_view`'s
/// `has_secret` in `routes/notifications.rs`.
fn monitor_view(m: &Monitor) -> Value {
    let mut v = serde_json::to_value(m).unwrap_or_else(|_| json!({}));
    if let Some(o) = v.as_object_mut() {
        o.insert("has_webhook".into(), json!(m.webhook_url.is_some()));
        // Sorted so the field is stable across requests — JSON object order
        // out of `serde_json` follows insertion, and an unstable list would
        // make the dashboard's cache diff churn for no reason.
        let mut names: Vec<&str> = m
            .config
            .get("headers")
            .and_then(|h| h.as_object())
            .map(|h| h.keys().map(String::as_str).collect())
            .unwrap_or_default();
        names.sort_unstable();
        o.insert("probe_header_names".into(), json!(names));
        // Existence only, and read from the ROW rather than from `v`: `config`
        // in the serialized value has already had `body` removed, so deriving
        // this from `v` would hard-code `false`.
        o.insert(
            "has_probe_body".into(),
            json!(m.config.get("body").is_some()),
        );
    }
    v
}

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
    /// Monitors are project-scoped with no app/environment link at all;
    /// rejected rather than silently accepted-and-ignored — see
    /// `routes::scope::reject_environment_id`.
    pub environment_id: Option<String>,
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
    Query(env): Query<super::scope::RejectEnvQuery>,
) -> Result<Json<Value>, ApiError> {
    super::scope::reject_environment_id(env.environment_id.as_deref())?;
    let mut conn = db(&state).await?;
    authorize_project(&mut conn, auth.user_id, project_id, perm::MONITOR_READ).await?;
    let rows = repo::list_monitors_for_project(&mut conn, project_id).await?;
    Ok(Json(json!(rows)))
}

pub async fn create(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(project_id): Path<Uuid>,
    Query(env): Query<super::scope::RejectEnvQuery>,
    Json(req): Json<CreateMonitorReq>,
) -> Result<Json<Value>, ApiError> {
    // Monitors have no environment dimension at all (see `RangeQuery`'s doc
    // comment above); rejected here too, matching `list`/`detail`/`checks`/
    // `incidents` in this same file rather than silently discarding it on
    // writes alone.
    super::scope::reject_environment_id(env.environment_id.as_deref())?;
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

    // Bound sustained prober/DB load: monitors are polled forever once created.
    if repo::count_monitors_for_project(&mut conn, project_id).await?
        >= sauron_db::repo::MAX_MONITORS_PER_PROJECT
    {
        return Err(ApiError::Conflict(format!(
            "project already has the maximum of {} monitors",
            sauron_db::repo::MAX_MONITORS_PER_PROJECT
        )));
    }

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
    Ok(Json(monitor_view(&m)))
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
    let m = repo::get_monitor(&mut conn, monitor_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok((conn, m))
}

pub async fn detail(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(monitor_id): Path<Uuid>,
    Query(env): Query<super::scope::RejectEnvQuery>,
) -> Result<Json<Value>, ApiError> {
    super::scope::reject_environment_id(env.environment_id.as_deref())?;
    let (mut conn, m) =
        load_authorized(&state, auth.user_id, monitor_id, perm::MONITOR_READ).await?;
    let uptime_24h = repo::uptime_pct(&mut conn, monitor_id, 24).await?;
    let uptime_7d = repo::uptime_pct(&mut conn, monitor_id, 24 * 7).await?;
    let uptime_30d = repo::uptime_pct(&mut conn, monitor_id, 24 * 30).await?;
    let incidents = repo::list_incidents(&mut conn, monitor_id, 20).await?;
    Ok(Json(json!({
        "monitor": monitor_view(&m),
        "uptime": { "h24": uptime_24h, "d7": uptime_7d, "d30": uptime_30d },
        "incidents": incidents,
    })))
}

#[derive(Deserialize)]
pub struct UpdateMonitorReq {
    pub name: Option<String>,
    pub enabled: Option<bool>,
    pub interval_seconds: Option<i32>,
    /// Three-state on purpose: absent = leave the stored URL alone, `null` =
    /// clear it, a string = replace it. That is what lets an edit form work
    /// without ever reading the current value back — which it cannot, since
    /// the model never serializes it.
    ///
    /// `deserialize_with` is what makes the middle state reachable: see
    /// `double_option`. `repo::update_monitor` has always implemented all
    /// three (it splits the value across a `set_webhook` boolean and a
    /// nullable bind precisely so `NULL` can mean "write NULL"), but the
    /// request could only ever express two of them.
    #[serde(default, deserialize_with = "double_option")]
    pub webhook_url: Option<Option<String>>,
}

/// Deserialize a field that must distinguish *absent* from *explicitly null*.
///
/// Serde collapses a JSON `null` into `None` for a plain `Option<T>`, so a
/// bare `Option<Option<T>>` field cannot tell the two apart — every `null`
/// arrives as the outer `None` and is read downstream as "leave it alone".
/// A client asking to clear its webhook therefore got a 200 and no change.
/// Deserializing the inner `Option` first and wrapping it in `Some`
/// unconditionally keeps `null` distinct, and `#[serde(default)]` supplies
/// the outer `None` for the genuinely-absent case.
///
/// This matters more since the URL stopped being serialized: a caller can no
/// longer re-read the field to notice that its clear was ignored.
fn double_option<'de, D, T>(de: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::deserialize(de).map(Some)
}

pub async fn update(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(monitor_id): Path<Uuid>,
    Query(env): Query<super::scope::RejectEnvQuery>,
    Json(req): Json<UpdateMonitorReq>,
) -> Result<Json<Value>, ApiError> {
    super::scope::reject_environment_id(env.environment_id.as_deref())?;
    if let Some(i) = req.interval_seconds {
        if !sauron_core::is_valid_monitor_interval(i) {
            return Err(ApiError::BadRequest(invalid_interval_msg()));
        }
    }
    // Reuse the connection `load_authorized` already checked out rather than
    // taking a second one from the pool for the same request.
    let (mut conn, _m) =
        load_authorized(&state, auth.user_id, monitor_id, perm::MONITOR_WRITE).await?;
    // Pausing/enabling flips status too.
    let status = req.enabled.map(|e| if e { "unknown" } else { "paused" });
    let interval = req.interval_seconds;
    let webhook = req.webhook_url.map(|w| {
        w.as_deref()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
    });
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
    Ok(Json(monitor_view(&m)))
}

pub async fn delete(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(monitor_id): Path<Uuid>,
    Query(env): Query<super::scope::RejectEnvQuery>,
) -> Result<Json<Value>, ApiError> {
    super::scope::reject_environment_id(env.environment_id.as_deref())?;
    let (mut conn, _m) =
        load_authorized(&state, auth.user_id, monitor_id, perm::MONITOR_WRITE).await?;
    repo::delete_monitor(&mut conn, monitor_id).await?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn checks(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(monitor_id): Path<Uuid>,
    Query(q): Query<RangeQuery>,
) -> Result<Json<Value>, ApiError> {
    super::scope::reject_environment_id(q.environment_id.as_deref())?;
    let (mut conn, _m) =
        load_authorized(&state, auth.user_id, monitor_id, perm::MONITOR_READ).await?;
    let hours = q.hours.unwrap_or(24).clamp(1, 24 * 90);
    let series = repo::latency_series(&mut conn, monitor_id, hours).await?;
    Ok(Json(json!(series)))
}

pub async fn incidents(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(monitor_id): Path<Uuid>,
    Query(env): Query<super::scope::RejectEnvQuery>,
) -> Result<Json<Value>, ApiError> {
    super::scope::reject_environment_id(env.environment_id.as_deref())?;
    let (mut conn, _m) =
        load_authorized(&state, auth.user_id, monitor_id, perm::MONITOR_READ).await?;
    let rows = repo::list_incidents(&mut conn, monitor_id, 50).await?;
    Ok(Json(json!(rows)))
}
