//! Screen analytics: per-screen views/events/users/exceptions + on-read dwell.
use axum::extract::{Path, Query, RawQuery, State};
use axum::Json;
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use sauron_auth::{perm, AuthUser};
use sauron_db::models::{AnalyticsEvent, ErrorEvent};
use sauron_db::repo;

use super::db;
use crate::error::ApiError;
use crate::AppState;

#[derive(Deserialize)]
pub struct ScreenListQuery {
    #[serde(default = "days30")]
    pub since_days: i64,
    pub q: Option<String>,
    #[serde(default = "lim50")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
    // `environment_id` is deliberately NOT a field here — it is read from the
    // raw query string via `RawQuery` + `scope::authorized_read_scope`
    // instead of this `Query<T>` extractor. See `routes::scope`'s module docs
    // for the extractor trap this avoids.
}
fn days30() -> i64 {
    30
}
fn lim50() -> i64 {
    50
}

pub async fn list(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<ScreenListQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<Vec<repo::ScreenRow>>, ApiError> {
    let mut conn = db(&state).await?;
    let scope = super::scope::authorized_read_scope(
        &mut conn,
        auth.user_id,
        app_id,
        perm::EVENT_READ,
        raw_query.as_deref(),
    )
    .await?;
    let since = Utc::now() - Duration::days(q.since_days.clamp(1, 365));
    let pattern = match q.q.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(term) => repo::like_contains(term),
        None => "%".to_string(),
    };
    let rows = repo::screen_list(
        &mut conn,
        scope,
        since,
        &pattern,
        q.limit.clamp(1, 200),
        super::clamp_offset(q.offset),
    )
    .await?;
    Ok(Json(rows))
}

#[derive(Deserialize)]
pub struct ScreenDetailQuery {
    pub name: String,
    #[serde(default = "days30")]
    pub since_days: i64,
    // `environment_id` is deliberately NOT a field here — see
    // `ScreenListQuery`'s comment above.
}

#[derive(Serialize)]
pub struct ScreenDetail {
    pub stats: repo::ScreenStats,
    pub recent_events: Vec<AnalyticsEvent>,
    pub recent_exceptions: Vec<ErrorEvent>,
}

pub async fn detail(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<ScreenDetailQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<ScreenDetail>, ApiError> {
    if q.name.trim().is_empty() {
        return Err(ApiError::BadRequest("name is required".into()));
    }
    let mut conn = db(&state).await?;
    // `_with_perms`: `recent_exceptions` below is whole `ErrorEvent` rows, whose
    // `stacktrace_symbolicated` holds the de-obfuscated source lines
    // `perm::SOURCE_READ` gates. See `sessions::detail` for the same note.
    let (scope, perms) = super::scope::authorized_read_scope_with_perms(
        &mut conn,
        auth.user_id,
        app_id,
        perm::EVENT_READ,
        raw_query.as_deref(),
    )
    .await?;
    let since = Utc::now() - Duration::days(q.since_days.clamp(1, 365));
    let stats = repo::screen_stats(&mut conn, scope.clone(), since, &q.name).await?;
    let recent_events =
        repo::recent_events_for_screen(&mut conn, scope.clone(), &q.name, since, 20).await?;
    let mut recent_exceptions =
        repo::recent_exceptions_for_screen(&mut conn, scope, &q.name, since, 20).await?;
    crate::symbolicate::gate_source_context(&perms, &mut recent_exceptions);
    Ok(Json(ScreenDetail {
        stats,
        recent_events,
        recent_exceptions,
    }))
}
