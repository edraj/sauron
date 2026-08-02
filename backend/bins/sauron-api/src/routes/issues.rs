//! The issues API, scoped to an app: list, detail (with occurrences chart +
//! latest event), status updates, and per-issue occurrences.

use axum::extract::{Path, RawQuery, State};
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
    // `environment_id` is deliberately NOT a field here — it is read from the
    // raw query string via `RawQuery` + `scope::authorized_read_scope` instead
    // of this `Query<T>` extractor. See `routes::scope`'s module docs for why:
    // an `Option<String>` field on this struct would go through
    // `axum_extra::extract::Query` (needed for this struct's own `Vec<String>`
    // `filter` field), whose codec silently collapses `?environment_id=` to
    // `None`.
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
    RawQuery(raw_query): RawQuery,
) -> Result<Json<Vec<Issue>>, ApiError> {
    let mut conn = db(&state).await?;
    let scope = super::scope::authorized_read_scope(
        &mut conn,
        auth.user_id,
        app_id,
        perm::ISSUE_READ,
        raw_query.as_deref(),
    )
    .await?;
    let filters = sauron_db::filter::parse_filters(&q.filter, sauron_db::filter::ISSUE_FILTERS)?;
    let search = q.q.as_deref().filter(|s| !s.is_empty());
    let since = Utc::now() - Duration::days(q.since_days.clamp(1, 3650));
    let limit = q.limit.clamp(1, 200);
    Ok(Json(
        repo::list_issues(
            &mut conn,
            scope,
            &filters,
            search,
            since,
            limit,
            super::clamp_offset(q.offset),
        )
        .await?,
    ))
}

#[derive(Serialize)]
pub struct IssueDetail {
    #[serde(flatten)]
    pub issue: Issue,
    pub latest_event: Option<ErrorEvent>,
    pub series: Vec<SeriesPoint>,
}

// No bespoke query struct: `detail` takes only `environment_id`, which comes
// from `RawQuery` (see `list`'s comment above), not a `Query<T>` extractor.

pub async fn detail(
    auth: AuthUser,
    State(state): State<AppState>,
    Path((app_id, issue_id)): Path<(Uuid, Uuid)>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<IssueDetail>, ApiError> {
    let mut conn = db(&state).await?;
    // One ancestry+grant resolution authorizes the read, resolves its scope,
    // and answers the second permission question at that same scope.
    let (scope, perms) = super::scope::authorized_read_scope_with_perms(
        &mut conn,
        auth.user_id,
        app_id,
        perm::ISSUE_READ,
        raw_query.as_deref(),
    )
    .await?;
    // Viewing de-obfuscated source code needs source:read; symbol/file/line don't.
    // Evaluated at the resolved environment, not app-wide — an env-scoped
    // caller holds `source:read` (if at all) on their environment.
    let include_source = perms.contains(perm::SOURCE_READ);

    let issue = repo::get_issue(&mut conn, scope.clone(), issue_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let mut latest_event = repo::latest_error_event(&mut conn, scope.clone(), issue_id).await?;
    let since = Utc::now() - Duration::days(30);
    let series = repo::issue_occurrence_series(&mut conn, scope, issue_id, since).await?;
    drop(conn); // release the pooled conn; symbolication checks out its own

    if let Some(ev) = latest_event.as_mut() {
        crate::symbolicate::symbolicate_event(&state, app_id, ev).await;
        if !include_source {
            crate::symbolicate::strip_source_context(ev);
        }
    }

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
    #[serde(default)]
    pub filter: Vec<String>,
    pub q: Option<String>,
    #[serde(default = "default_events_since_days")]
    pub since_days: i64,
    #[serde(default = "default_events_limit")]
    pub limit: i64,
    // `environment_id` comes from `RawQuery`, not this struct — see `list`'s
    // comment above.
}

fn default_events_limit() -> i64 {
    30
}
fn default_events_since_days() -> i64 {
    3650
}

pub async fn events(
    auth: AuthUser,
    State(state): State<AppState>,
    Path((app_id, issue_id)): Path<(Uuid, Uuid)>,
    Query(q): Query<EventsQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<Vec<ErrorEvent>>, ApiError> {
    let mut conn = db(&state).await?;
    // One ancestry+grant resolution authorizes the read, resolves its scope,
    // and answers the second permission question at that same scope.
    let (scope, perms) = super::scope::authorized_read_scope_with_perms(
        &mut conn,
        auth.user_id,
        app_id,
        perm::ISSUE_READ,
        raw_query.as_deref(),
    )
    .await?;
    // Evaluated at the resolved environment — see `detail`'s comment above.
    let include_source = perms.contains(perm::SOURCE_READ);
    // Confirm the issue belongs to this app before returning its events (prevents
    // reading another app's events by passing a foreign issue_id).
    repo::get_issue(&mut conn, scope.clone(), issue_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let filters =
        sauron_db::filter::parse_filters(&q.filter, sauron_db::filter::ERROR_EVENT_FILTERS)?;
    let search = q.q.as_deref().filter(|s| !s.is_empty());
    let since = Utc::now() - Duration::days(q.since_days.clamp(1, 3650));
    let limit = q.limit.clamp(1, 100);
    let mut events = repo::list_error_events_for_issue(
        &mut conn,
        scope,
        issue_id,
        &filters,
        search,
        Some(since),
        limit,
    )
    .await?;
    drop(conn); // release before per-event symbolication (checks out its own)
                // One shared blob-fetcher for the whole page: the artifact lookup is
                // memoized across events instead of repeated per event.
    crate::symbolicate::symbolicate_events(&state, app_id, &mut events).await;
    if !include_source {
        for ev in events.iter_mut() {
            crate::symbolicate::strip_source_context(ev);
        }
    }
    Ok(Json(events))
}

/// Totals for the occurrences the sibling `events` route would list.
///
/// A separate route rather than an envelope around `events` for two reasons:
/// the list response is `Vec<ErrorEvent>` and several SDK-adjacent callers
/// already parse it as a bare array, and the counts skip symbolication
/// entirely, so making them their own request keeps the cheap query cheap.
///
/// Reuses `EventsQuery` so `filter`/`q`/`since_days` are parsed by the exact
/// same code as the list; `limit` is accepted and ignored, since a total that
/// stopped at the page size would be worse than useless.
pub async fn event_stats(
    auth: AuthUser,
    State(state): State<AppState>,
    Path((app_id, issue_id)): Path<(Uuid, Uuid)>,
    Query(q): Query<EventsQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<repo::IssueEventStatsRow>, ApiError> {
    let mut conn = db(&state).await?;
    let scope = super::scope::authorized_read_scope(
        &mut conn,
        auth.user_id,
        app_id,
        perm::ISSUE_READ,
        raw_query.as_deref(),
    )
    .await?;
    // Same cross-app guard as `events`: never let a foreign issue_id disclose
    // another app's counts.
    repo::get_issue(&mut conn, scope.clone(), issue_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let filters =
        sauron_db::filter::parse_filters(&q.filter, sauron_db::filter::ERROR_EVENT_FILTERS)?;
    let search = q.q.as_deref().filter(|s| !s.is_empty());
    let since = Utc::now() - Duration::days(q.since_days.clamp(1, 3650));
    let stats = repo::error_event_stats_for_issue(
        &mut conn,
        scope,
        issue_id,
        &filters,
        search,
        Some(since),
    )
    .await?;
    Ok(Json(stats))
}

// ---------------------------------------------------------------------------
// Exceptions dashboard header — status/level breakdown + occurrence series.
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct StatsQuery {
    #[serde(default = "default_stats_days")]
    pub since_days: i64,
    // `environment_id` comes from `RawQuery`, not this struct — see `list`'s
    // comment above.
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
    RawQuery(raw_query): RawQuery,
) -> Result<Json<IssueStats>, ApiError> {
    let mut conn = db(&state).await?;
    let scope = super::scope::authorized_read_scope(
        &mut conn,
        auth.user_id,
        app_id,
        perm::ISSUE_READ,
        raw_query.as_deref(),
    )
    .await?;
    let counts = repo::issue_stats(&mut conn, scope.clone()).await?;
    let since = Utc::now() - Duration::days(q.since_days.clamp(1, 365));
    let series = repo::error_series(&mut conn, scope, since).await?;
    Ok(Json(IssueStats { counts, series }))
}
