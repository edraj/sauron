//! Product-analytics queries, scoped to an app: top events, time series, and
//! the unified person profile (a person's events + errors).

use axum::extract::{Path, State};
use axum::Json;
use axum_extra::extract::Query;
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use sauron_auth::{authorize_app, perm, AuthUser};
use sauron_db::models::{AnalyticsEvent, ErrorEvent, EventUser, Issue};
use sauron_db::repo;
use sauron_db::repo::{EventCount, PersonRow, SeriesPoint};

use super::db;
use crate::error::ApiError;
use crate::AppState;

#[derive(Deserialize)]
pub struct RangeQuery {
    #[serde(default = "default_days")]
    pub since_days: i64,
    #[serde(default = "default_top")]
    pub limit: i64,
    pub name: Option<String>,
}

fn default_days() -> i64 {
    30
}
fn default_top() -> i64 {
    20
}

pub async fn top_events(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<RangeQuery>,
) -> Result<Json<Vec<EventCount>>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::EVENT_READ).await?;
    let since = Utc::now() - Duration::days(q.since_days.clamp(1, 365));
    let limit = q.limit.clamp(1, 100);
    Ok(Json(
        repo::top_events(&mut conn, app_id, since, limit).await?,
    ))
}

pub async fn event_series(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<RangeQuery>,
) -> Result<Json<Vec<SeriesPoint>>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::EVENT_READ).await?;
    let since = Utc::now() - Duration::days(q.since_days.clamp(1, 365));
    Ok(Json(
        repo::event_series(&mut conn, app_id, q.name.as_deref(), since).await?,
    ))
}

#[derive(Deserialize)]
pub struct PersonQuery {
    #[serde(default = "default_person_limit")]
    pub limit: i64,
}

fn default_person_limit() -> i64 {
    50
}

#[derive(Serialize)]
pub struct PersonProfile {
    pub distinct_id: String,
    pub user: Option<EventUser>,
    pub events: Vec<AnalyticsEvent>,
    pub errors: Vec<ErrorEvent>,
}

pub async fn person(
    auth: AuthUser,
    State(state): State<AppState>,
    Path((app_id, distinct_id)): Path<(Uuid, String)>,
    Query(q): Query<PersonQuery>,
) -> Result<Json<PersonProfile>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::EVENT_READ).await?;
    let limit = q.limit.clamp(1, 200);

    let user = repo::get_event_user(&mut conn, app_id, &distinct_id).await?;
    let events = repo::events_for_person(&mut conn, app_id, &distinct_id, limit).await?;
    let errors = repo::error_events_for_person(&mut conn, app_id, &distinct_id, limit).await?;

    Ok(Json(PersonProfile {
        distinct_id,
        user,
        events,
        errors,
    }))
}

// ---------------------------------------------------------------------------
// Users Explorer — searchable directory of people with activity counts.
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct PersonsQuery {
    pub search: Option<String>,
    #[serde(default = "default_persons_list_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}

fn default_persons_list_limit() -> i64 {
    50
}

pub async fn persons_list(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<PersonsQuery>,
) -> Result<Json<Vec<PersonRow>>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::EVENT_READ).await?;
    let search = q.search.as_deref().filter(|s| !s.is_empty());
    Ok(Json(
        repo::list_persons(
            &mut conn,
            app_id,
            search,
            q.limit.clamp(1, 200),
            q.offset.max(0),
        )
        .await?,
    ))
}

// ---------------------------------------------------------------------------
// Event Explorer — the raw analytics event stream with filters.
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct EventsListQuery {
    #[serde(default)]
    pub filter: Vec<String>,
    pub q: Option<String>,
    #[serde(default = "default_events_since_days")]
    pub since_days: i64,
    #[serde(default = "default_events_list_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}

fn default_events_list_limit() -> i64 {
    50
}
fn default_events_since_days() -> i64 {
    3650
}

pub async fn events_list(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<EventsListQuery>,
) -> Result<Json<Vec<AnalyticsEvent>>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::EVENT_READ).await?;
    let filters = sauron_db::filter::parse_filters(&q.filter, sauron_db::filter::EVENT_FILTERS)?;
    let search = q.q.as_deref().filter(|s| !s.is_empty());
    let since = Utc::now() - Duration::days(q.since_days.clamp(1, 3650));
    Ok(Json(
        repo::list_analytics_events(
            &mut conn,
            app_id,
            &filters,
            search,
            Some(since),
            q.limit.clamp(1, 200),
            q.offset.max(0),
        )
        .await?,
    ))
}

// ---------------------------------------------------------------------------
// Overview — a single composite health + activity snapshot for the app.
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct Overview {
    pub totals: repo::OverviewTotals,
    pub error_rate: f64,
    pub crash_free_sessions: f64,
    pub events_series: Vec<SeriesPoint>,
    pub errors_series: Vec<SeriesPoint>,
    pub top_issues: Vec<Issue>,
    pub top_events: Vec<EventCount>,
}

pub async fn overview(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<RangeQuery>,
) -> Result<Json<Overview>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::EVENT_READ).await?;
    let since = Utc::now() - Duration::days(q.since_days.clamp(1, 365));

    let totals = repo::overview_totals(&mut conn, app_id, since).await?;
    let events_series = repo::event_series(&mut conn, app_id, None, since).await?;
    let errors_series = repo::error_series(&mut conn, app_id, since).await?;
    let top_issues = repo::top_issues(&mut conn, app_id, since, 5).await?;
    let top_events = repo::top_events(&mut conn, app_id, since, 5).await?;

    let error_rate = {
        let denom = totals.events + totals.errors;
        if denom > 0 {
            totals.errors as f64 / denom as f64
        } else {
            0.0
        }
    };
    let crash_free_sessions = if totals.sessions > 0 {
        1.0 - (totals.crashed_sessions as f64 / totals.sessions as f64)
    } else {
        1.0
    };

    Ok(Json(Overview {
        totals,
        error_rate,
        crash_free_sessions,
        events_series,
        errors_series,
        top_issues,
        top_events,
    }))
}

// ---------------------------------------------------------------------------
// Audience analytics — GET /users/summary.
// ---------------------------------------------------------------------------

/// DAU / MAU, guarding division by zero. Pure.
pub fn stickiness(dau: i64, mau: i64) -> f64 {
    if mau > 0 {
        dau as f64 / mau as f64
    } else {
        0.0
    }
}

#[derive(Serialize)]
pub struct UsersAnalytics {
    pub stats: repo::UserStats,
    pub stickiness: f64,
    pub series: Vec<repo::UserSeriesPoint>,
}

pub async fn users_summary(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<RangeQuery>,
) -> Result<Json<UsersAnalytics>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::EVENT_READ).await?;
    let since = Utc::now() - Duration::days(q.since_days.clamp(1, 365));

    let stats = repo::user_stats(&mut conn, app_id, since).await?;
    let series = repo::active_user_series(&mut conn, app_id, since).await?;
    let stickiness = stickiness(stats.dau, stats.mau);

    Ok(Json(UsersAnalytics {
        stats,
        stickiness,
        series,
    }))
}

// ---------------------------------------------------------------------------
// Session-engagement analytics — GET /sessions/summary.
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct SessionsAnalytics {
    pub stats: repo::SessionStats,
    pub duration_series: Vec<repo::SeriesAvgPoint>,
    pub duration_histogram: Vec<repo::HistoBucket>,
}

pub async fn sessions_summary(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<RangeQuery>,
) -> Result<Json<SessionsAnalytics>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::EVENT_READ).await?;
    let since = Utc::now() - Duration::days(q.since_days.clamp(1, 365));

    let stats = repo::session_stats(&mut conn, app_id, since).await?;
    let duration_series = repo::session_duration_series(&mut conn, app_id, since).await?;
    let duration_histogram = repo::session_duration_histogram(&mut conn, app_id, since).await?;

    Ok(Json(SessionsAnalytics {
        stats,
        duration_series,
        duration_histogram,
    }))
}

// ---------------------------------------------------------------------------
// Cross-tier errors timeseries — GET /errors/timeseries.
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct TimeseriesQuery {
    pub from: chrono::DateTime<chrono::Utc>,
    pub to: chrono::DateTime<chrono::Utc>,
}

#[derive(Serialize)]
pub struct DayCountOut {
    pub day: chrono::NaiveDate,
    pub count: i64,
}

pub async fn error_timeseries(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<TimeseriesQuery>,
) -> Result<Json<Vec<DayCountOut>>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::ISSUE_READ).await?;
    drop(conn); // release the pooled conn before the router checks out its own
    let series = crate::tier_read::error_counts_by_day(&state, app_id, q.from, q.to).await?;
    Ok(Json(
        series
            .into_iter()
            .map(|d| DayCountOut {
                day: d.day,
                count: d.count,
            })
            .collect(),
    ))
}

// ---------------------------------------------------------------------------
// Cross-tier analytics-events timeseries — GET /events/timeseries.
// ---------------------------------------------------------------------------

pub async fn event_timeseries(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<TimeseriesQuery>,
) -> Result<Json<Vec<DayCountOut>>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::EVENT_READ).await?;
    drop(conn); // release the pooled conn before the router checks out its own
    let series = crate::tier_read::event_counts_by_day(&state, app_id, q.from, q.to).await?;
    Ok(Json(
        series
            .into_iter()
            .map(|d| DayCountOut {
                day: d.day,
                count: d.count,
            })
            .collect(),
    ))
}

// ---------------------------------------------------------------------------
// Cross-tier transactions timeseries — GET /transactions/timeseries.
// ADDITIVE (count/throughput) only; percentiles are holistic and served
// hot-only (Postgres) — see repo::transaction_counts_by_day_hot.
// ---------------------------------------------------------------------------

pub async fn transaction_timeseries(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<TimeseriesQuery>,
) -> Result<Json<Vec<DayCountOut>>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::EVENT_READ).await?;
    drop(conn); // release the pooled conn before the router checks out its own
    let series = crate::tier_read::transaction_counts_by_day(&state, app_id, q.from, q.to).await?;
    Ok(Json(
        series
            .into_iter()
            .map(|d| DayCountOut {
                day: d.day,
                count: d.count,
            })
            .collect(),
    ))
}

#[cfg(test)]
mod stickiness_tests {
    use super::stickiness;

    #[test]
    fn ratio_of_dau_to_mau() {
        assert!((stickiness(5, 20) - 0.25).abs() < 1e-9);
    }

    #[test]
    fn zero_mau_is_zero_not_nan() {
        assert_eq!(stickiness(3, 0), 0.0);
    }
}
