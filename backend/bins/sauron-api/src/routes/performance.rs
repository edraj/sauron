//! Performance API, scoped to an app: percentile summaries per operation and a
//! latency/throughput time series, computed over the `transactions` signal.

use axum::extract::{Path, Query, State};
use axum::Json;
use chrono::{Duration, Utc};
use serde::Deserialize;
use uuid::Uuid;

use sauron_auth::{authorize_app, perm, AuthUser};
use sauron_db::repo;
use sauron_db::repo::{PerfSeriesPoint, PerfSummaryRow};

use super::db;
use crate::error::ApiError;
use crate::AppState;

#[derive(Deserialize)]
pub struct SummaryQuery {
    #[serde(default = "default_days")]
    pub since_days: i64,
    pub op: Option<String>,
}

#[derive(Deserialize)]
pub struct SeriesQuery {
    #[serde(default = "default_days")]
    pub since_days: i64,
    pub name: Option<String>,
    pub op: Option<String>,
}

fn default_days() -> i64 {
    7
}

pub async fn summary(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<SummaryQuery>,
) -> Result<Json<Vec<PerfSummaryRow>>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::EVENT_READ).await?;
    let since = Utc::now() - Duration::days(q.since_days.clamp(1, 365));
    let op = q.op.as_deref().filter(|s| !s.is_empty());
    Ok(Json(
        repo::performance_summary(&mut conn, app_id, since, op, None).await?,
    ))
}

pub async fn series(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<SeriesQuery>,
) -> Result<Json<Vec<PerfSeriesPoint>>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::EVENT_READ).await?;
    let since = Utc::now() - Duration::days(q.since_days.clamp(1, 365));
    let name = q.name.as_deref().filter(|s| !s.is_empty());
    let op = q.op.as_deref().filter(|s| !s.is_empty());
    Ok(Json(
        repo::performance_series(&mut conn, app_id, since, name, op).await?,
    ))
}
