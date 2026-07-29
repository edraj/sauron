//! Devices API, scoped to an app: fleet inventory and a per-device deep-dive
//! (recent sessions, crash history, and its performance profile).

use axum::extract::{Path, Query, State};
use axum::Json;
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use sauron_auth::{authorize_app, perm, AuthUser};
use sauron_db::models::{ErrorEvent, Session};
use sauron_db::repo;
use sauron_db::repo::{DeviceRow, PerfSummaryRow};

use super::db;
use crate::error::ApiError;
use crate::AppState;

#[derive(Deserialize)]
pub struct ListQuery {
    #[serde(default = "default_days")]
    pub since_days: i64,
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
    pub search: Option<String>,
    pub environment_id: Option<String>,
}

fn default_days() -> i64 {
    30
}
fn default_limit() -> i64 {
    50
}

pub async fn list(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Vec<DeviceRow>>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::EVENT_READ).await?;
    let since = Utc::now() - Duration::days(q.since_days.clamp(1, 365));
    let limit = q.limit.clamp(1, 200);
    let search = q.search.as_deref().filter(|s| !s.is_empty());
    let scope = super::scope::read_scope(app_id, q.environment_id.as_deref())?;
    Ok(Json(
        repo::list_devices(
            &mut conn,
            scope,
            since,
            limit,
            super::clamp_offset(q.offset),
            search,
        )
        .await?,
    ))
}

#[derive(Deserialize)]
pub struct DetailQuery {
    /// The device key (passed as a query param — keys can contain `/` and spaces).
    pub key: String,
    pub environment_id: Option<String>,
}

#[derive(Serialize)]
pub struct DeviceDetail {
    /// Environment-scoped, not the raw `devices` row — see `get_device`'s doc
    /// comment. `events_count`/`errors_count` read the durable `devices`
    /// columns under `All` and an environment-scoped LATERAL under `One`/
    /// `Unattributed`, matching `sessions`/`errors`/`perf` below rather than
    /// showing cross-environment, all-time totals above a scoped list.
    pub device: DeviceRow,
    pub sessions: Vec<Session>,
    pub errors: Vec<ErrorEvent>,
    pub perf: Vec<PerfSummaryRow>,
}

pub async fn detail(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(dq): Query<DetailQuery>,
) -> Result<Json<DeviceDetail>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::EVENT_READ).await?;
    let device_key = dq.key;
    let scope = super::scope::read_scope(app_id, dq.environment_id.as_deref())?;

    let device = repo::get_device(&mut conn, scope, &device_key)
        .await?
        .ok_or(ApiError::NotFound)?;

    let since = Utc::now() - Duration::days(90);
    let sessions =
        repo::list_sessions(&mut conn, scope, since, 50, 0, None, Some(&device_key)).await?;
    let errors = repo::errors_for_device(&mut conn, scope, &device_key, 50).await?;
    let perf = repo::performance_summary(&mut conn, scope, since, None, Some(&device_key)).await?;

    Ok(Json(DeviceDetail {
        device,
        sessions,
        errors,
        perf,
    }))
}
