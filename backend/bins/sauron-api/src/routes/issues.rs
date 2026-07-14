//! The issues API, scoped to an app: list, detail (with occurrences chart +
//! latest event), status updates, and per-issue occurrences.

use axum::extract::{Path, State};
use axum::Json;
use axum_extra::extract::Query;
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use sauron_auth::{authorize_app, perm, AuthUser};
use sauron_db::models::{ErrorEvent, Issue};
use sauron_db::repo;
use sauron_db::repo::SeriesPoint;

use super::db;
use crate::error::ApiError;
use crate::AppState;

#[derive(Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    pub filter: Vec<String>,
    pub q: Option<String>,
    #[serde(default = "default_since_days")]
    pub since_days: i64,
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}

fn default_limit() -> i64 {
    50
}
fn default_since_days() -> i64 {
    3650
} // effectively "all" unless narrowed

pub async fn list(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Vec<Issue>>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::ISSUE_READ).await?;
    let filters = sauron_db::filter::parse_filters(&q.filter, sauron_db::filter::ISSUE_FILTERS)?;
    let search = q.q.as_deref().filter(|s| !s.is_empty());
    let since = Utc::now() - Duration::days(q.since_days.clamp(1, 3650));
    let limit = q.limit.clamp(1, 200);
    Ok(Json(
        repo::list_issues(&mut conn, app_id, &filters, search, Some(since), limit, q.offset.max(0)).await?,
    ))
}

#[derive(Serialize)]
pub struct IssueDetail {
    #[serde(flatten)]
    pub issue: Issue,
    pub latest_event: Option<ErrorEvent>,
    pub series: Vec<SeriesPoint>,
}

pub async fn detail(
    auth: AuthUser,
    State(state): State<AppState>,
    Path((app_id, issue_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<IssueDetail>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::ISSUE_READ).await?;

    let issue = repo::get_issue(&mut conn, app_id, issue_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let latest_event = repo::latest_error_event(&mut conn, issue_id).await?;
    let since = Utc::now() - Duration::days(30);
    let series = repo::issue_occurrence_series(&mut conn, issue_id, since).await?;

    Ok(Json(IssueDetail {
        issue,
        latest_event,
        series,
    }))
}

#[derive(Deserialize)]
pub struct UpdateReq {
    pub status: String,
}

pub async fn update(
    auth: AuthUser,
    State(state): State<AppState>,
    Path((app_id, issue_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<UpdateReq>,
) -> Result<Json<Issue>, ApiError> {
    if !matches!(req.status.as_str(), "unresolved" | "resolved" | "ignored") {
        return Err(ApiError::BadRequest(
            "status must be unresolved, resolved, or ignored".into(),
        ));
    }
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::ISSUE_WRITE).await?;
    let issue = repo::update_issue_status(&mut conn, app_id, issue_id, &req.status)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(issue))
}

#[derive(Deserialize)]
pub struct EventsQuery {
    #[serde(default = "default_events_limit")]
    pub limit: i64,
}

fn default_events_limit() -> i64 {
    30
}

pub async fn events(
    auth: AuthUser,
    State(state): State<AppState>,
    Path((app_id, issue_id)): Path<(Uuid, Uuid)>,
    Query(q): Query<EventsQuery>,
) -> Result<Json<Vec<ErrorEvent>>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::ISSUE_READ).await?;
    // Confirm the issue belongs to this app before returning its events (prevents
    // reading another app's events by passing a foreign issue_id).
    repo::get_issue(&mut conn, app_id, issue_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let limit = q.limit.clamp(1, 100);
    Ok(Json(
        repo::list_error_events_for_issue(&mut conn, issue_id, limit).await?,
    ))
}

// ---------------------------------------------------------------------------
// Exceptions dashboard header — status/level breakdown + occurrence series.
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct StatsQuery {
    #[serde(default = "default_stats_days")]
    pub since_days: i64,
}

fn default_stats_days() -> i64 {
    30
}

#[derive(Serialize)]
pub struct IssueStats {
    #[serde(flatten)]
    pub counts: repo::IssueStatsRow,
    pub series: Vec<SeriesPoint>,
}

pub async fn stats(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<StatsQuery>,
) -> Result<Json<IssueStats>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::ISSUE_READ).await?;
    let counts = repo::issue_stats(&mut conn, app_id).await?;
    let since = Utc::now() - Duration::days(q.since_days.clamp(1, 365));
    let series = repo::error_series(&mut conn, app_id, since).await?;
    Ok(Json(IssueStats { counts, series }))
}
