//! Workflows API, scoped to an app: a rollup list (one row per workflow
//! name), one name's full detail, its individual runs, and — for the session
//! timeline — the workflow spans within one session.
//!
//! Workflows are entirely optional: an app that never calls `startWorkflow`
//! has no rows in `workflows` and every handler here simply returns empty
//! results (or 404 for a name that was never seen) — no existing route's
//! behaviour changes.
//!
//! Follows `screens.rs`'s template exactly: `authorized_read_scope` does
//! authorization AND scope resolution in one call, sourcing `environment_id`
//! from the raw query string rather than a `Query<T>`-deserialized field (see
//! `routes::scope`'s module docs for why — the "extractor trap" this avoids).

use axum::extract::{Path, Query, RawQuery, State};
use axum::Json;
use serde::Deserialize;
use uuid::Uuid;

use sauron_auth::{perm, AuthUser};
use sauron_db::repo;

use super::db;
use crate::error::ApiError;
use crate::AppState;

fn days30() -> i32 {
    30
}
fn lim50() -> i64 {
    50
}

/// The four effective-status values `workflow_runs` accepts as a `status`
/// filter — compared against the *derived* projection (see
/// `repo::workflow_effective_status_sql`'s doc comment), not the raw stored
/// column, which is what makes `abandoned` a filterable value at all even
/// though it never appears as a stored value.
const WORKFLOW_STATUSES: &[&str] = &["active", "completed", "cancelled", "abandoned"];

#[derive(Deserialize)]
pub struct WorkflowListQuery {
    #[serde(default = "days30")]
    pub since_days: i32,
    pub search: Option<String>,
    #[serde(default = "lim50")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
    // `environment_id` is deliberately NOT a field here — it is read from the
    // raw query string via `RawQuery` + `scope::authorized_read_scope`
    // instead of this `Query<T>` extractor. See `routes::scope`'s module docs
    // for the extractor trap this avoids.
}

/// One row per workflow name: started/completed/cancelled/abandoned/active
/// counts, unique users, median/p95 duration and last seen — paginated,
/// optionally substring-filtered by name.
pub async fn list(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<WorkflowListQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<Vec<repo::WorkflowRow>>, ApiError> {
    let mut conn = db(&state).await?;
    let scope = super::scope::authorized_read_scope(
        &mut conn,
        auth.user_id,
        app_id,
        perm::EVENT_READ,
        raw_query.as_deref(),
    )
    .await?;
    let since_days = q.since_days.clamp(1, 365);
    let search = q.search.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let rows = repo::workflow_list(
        &mut conn,
        scope,
        since_days,
        search,
        q.limit.clamp(1, 200),
        super::clamp_offset(q.offset),
    )
    .await?;
    Ok(Json(rows))
}

#[derive(Deserialize)]
pub struct WorkflowDetailQuery {
    #[serde(default = "days30")]
    pub since_days: i32,
    // `environment_id` is deliberately NOT a field here — see
    // `WorkflowListQuery`'s comment above.
}

/// Full detail for one workflow name: outcome/duration aggregate, duration
/// histogram, top contained events, top contained issues.
///
/// `repo::workflow_detail` returns `Err(diesel::result::Error::NotFound)` when
/// `name` has no matching row in scope within `since_days` (mirroring
/// `screen_stats`'s "vanishes rather than zero-fills" behaviour) — `?` maps
/// that to `ApiError::NotFound`, i.e. a 404, via `ApiError`'s
/// `From<diesel::result::Error>` impl (see `error.rs`), not the 500 an
/// unmapped diesel error would otherwise become.
pub async fn detail(
    auth: AuthUser,
    State(state): State<AppState>,
    Path((app_id, name)): Path<(Uuid, String)>,
    Query(q): Query<WorkflowDetailQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<repo::WorkflowDetail>, ApiError> {
    let mut conn = db(&state).await?;
    let scope = super::scope::authorized_read_scope(
        &mut conn,
        auth.user_id,
        app_id,
        perm::EVENT_READ,
        raw_query.as_deref(),
    )
    .await?;
    let since_days = q.since_days.clamp(1, 365);
    let detail = repo::workflow_detail(&mut conn, scope, &name, since_days).await?;
    Ok(Json(detail))
}

#[derive(Deserialize)]
pub struct WorkflowRunsQuery {
    #[serde(default = "days30")]
    pub since_days: i32,
    pub status: Option<String>,
    #[serde(default = "lim50")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
    // `environment_id` is deliberately NOT a field here — see
    // `WorkflowListQuery`'s comment above.
}

/// Individual runs of one workflow name, newest first, optionally filtered by
/// effective status.
pub async fn runs(
    auth: AuthUser,
    State(state): State<AppState>,
    Path((app_id, name)): Path<(Uuid, String)>,
    Query(q): Query<WorkflowRunsQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<Vec<repo::WorkflowRun>>, ApiError> {
    if let Some(s) = q.status.as_deref() {
        if !WORKFLOW_STATUSES.contains(&s) {
            return Err(ApiError::BadRequest(format!(
                "status must be one of: {}",
                WORKFLOW_STATUSES.join(", ")
            )));
        }
    }
    let mut conn = db(&state).await?;
    let scope = super::scope::authorized_read_scope(
        &mut conn,
        auth.user_id,
        app_id,
        perm::EVENT_READ,
        raw_query.as_deref(),
    )
    .await?;
    let since_days = q.since_days.clamp(1, 365);
    let rows = repo::workflow_runs(
        &mut conn,
        scope,
        &name,
        since_days,
        q.status.as_deref(),
        q.limit.clamp(1, 200),
        super::clamp_offset(q.offset),
    )
    .await?;
    Ok(Json(rows))
}

// No bespoke query struct: `session_spans` takes no query parameters of its
// own — `environment_id` comes from `RawQuery` (see `WorkflowListQuery`'s
// comment above), not a `Query<T>` extractor.

/// Every workflow span within one session, oldest first — feeds the session
/// timeline lane. Lives here (grouped with the other `repo::workflow_*`
/// consumers) rather than in `sessions.rs`, even though the route sits under
/// `/sessions/{session_id}/workflows`.
pub async fn session_spans(
    auth: AuthUser,
    State(state): State<AppState>,
    Path((app_id, session_id)): Path<(Uuid, String)>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<Vec<repo::WorkflowSpan>>, ApiError> {
    let mut conn = db(&state).await?;
    let scope = super::scope::authorized_read_scope(
        &mut conn,
        auth.user_id,
        app_id,
        perm::EVENT_READ,
        raw_query.as_deref(),
    )
    .await?;
    let spans = repo::workflow_spans_for_session(&mut conn, scope, &session_id).await?;
    Ok(Json(spans))
}
