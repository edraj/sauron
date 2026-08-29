//! Performance API, scoped to an app: percentile summaries per operation and a
//! latency/throughput time series, computed over the `transactions` signal.

use axum::extract::{Path, Query, RawQuery, State};
use axum::Json;
use chrono::Utc;
use serde::Deserialize;
use uuid::Uuid;

use sauron_auth::{perm, AuthUser};
use sauron_db::repo;
use sauron_db::repo::{PerfSeriesPoint, PerfSummaryRow};

use super::db;
use crate::error::ApiError;
use crate::openapi::ErrorResponse;
use crate::AppState;

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SummaryQuery {
    #[serde(default = "default_days")]
    pub since_days: i64,
    /// Absolute window bounds, `from` INCLUSIVE and `to` EXCLUSIVE, overriding
    /// `since_days` when either is present. See `analytics::RangeQuery` for why
    /// these are two plain fields rather than a flattened shared struct.
    pub from: Option<chrono::DateTime<chrono::Utc>>,
    pub to: Option<chrono::DateTime<chrono::Utc>>,
    pub op: Option<String>,
    // `environment_id` is deliberately NOT a field here — it is read from the
    // raw query string via `RawQuery` + `scope::authorized_read_scope`
    // instead of this `Query<T>` extractor. See `routes::scope`'s module docs
    // for the extractor trap this avoids.
}

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SeriesQuery {
    #[serde(default = "default_days")]
    pub since_days: i64,
    /// Absolute window bounds, `from` INCLUSIVE and `to` EXCLUSIVE, overriding
    /// `since_days` when either is present. See `analytics::RangeQuery` for why
    /// these are two plain fields rather than a flattened shared struct.
    pub from: Option<chrono::DateTime<chrono::Utc>>,
    pub to: Option<chrono::DateTime<chrono::Utc>>,
    pub name: Option<String>,
    pub op: Option<String>,
    // `environment_id` comes from `RawQuery` — see `SummaryQuery`'s comment
    // above.
}

fn default_days() -> i64 {
    7
}

/// Longest window a percentile query may span.
///
/// `percentile_cont` is an exact aggregate: it sorts every matching row, so cost
/// grows linearly with the window while the answer barely changes past a few
/// weeks. 365 days let one request sort an app's entire transaction history.
const MAX_PERF_WINDOW_DAYS: i64 = 90;

#[utoipa::path(
    get, path = "/v1/apps/{app_id}/performance/summary", tag = "Performance",
    summary = "Performance summary by operation",
    description = "\
Aggregates per `(name, op)` **pair** — the pair is the identity, so two \
operations sharing a name but differing in op are distinct rows and must be \
filtered on both.",
    params(("app_id" = Uuid, Path, description = "The app."), SummaryQuery, super::search::TimeFilterQuery), security(("bearerAuth" = [])),
    responses((status = 200, description = "Rows per operation.", body = Vec<PerfSummaryRow>), (status = 401, description = "Missing or invalid access token.", body = ErrorResponse), (status = 403, description = "No grant covers this scope.", body = ErrorResponse), (status = 503, description = "The query exceeded its time budget, or a required rollup has not been backfilled. The message names which.", body = ErrorResponse)),
)]
pub async fn summary(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<SummaryQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<Vec<PerfSummaryRow>>, ApiError> {
    let mut conn = db(&state).await?;
    let scope = super::scope::authorized_read_scope(
        &mut conn,
        auth.user_id,
        app_id,
        perm::EVENT_READ,
        raw_query.as_deref(),
    )
    .await?;
    let win = super::search::resolve_range(
        "occurred_at",
        q.from,
        q.to,
        q.since_days,
        Utc::now(),
        MAX_PERF_WINDOW_DAYS,
    )?;
    let op = q.op.as_deref().filter(|s| !s.is_empty());
    Ok(Json(
        repo::performance_summary(&mut conn, scope, win, op, None).await?,
    ))
}

#[utoipa::path(
    get, path = "/v1/apps/{app_id}/performance/series", tag = "Performance",
    summary = "Performance over time",
    description = "A time series for one operation or the whole app. Durations are milliseconds.",
    params(("app_id" = Uuid, Path, description = "The app."), SeriesQuery, super::search::TimeFilterQuery), security(("bearerAuth" = [])),
    responses((status = 200, description = "Time series points.", body = Vec<PerfSeriesPoint>), (status = 401, description = "Missing or invalid access token.", body = ErrorResponse), (status = 403, description = "No grant covers this scope.", body = ErrorResponse), (status = 503, description = "The query exceeded its time budget, or a required rollup has not been backfilled. The message names which.", body = ErrorResponse)),
)]
pub async fn series(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<SeriesQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<Vec<PerfSeriesPoint>>, ApiError> {
    let mut conn = db(&state).await?;
    let scope = super::scope::authorized_read_scope(
        &mut conn,
        auth.user_id,
        app_id,
        perm::EVENT_READ,
        raw_query.as_deref(),
    )
    .await?;
    let win = super::search::resolve_range(
        "occurred_at",
        q.from,
        q.to,
        q.since_days,
        Utc::now(),
        MAX_PERF_WINDOW_DAYS,
    )?;
    let name = q.name.as_deref().filter(|s| !s.is_empty());
    let op = q.op.as_deref().filter(|s| !s.is_empty());
    Ok(Json(
        repo::performance_series(&mut conn, scope, win, name, op).await?,
    ))
}
