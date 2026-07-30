//! Performance API, scoped to an app: percentile summaries per operation and a
//! latency/throughput time series, computed over the `transactions` signal.

use axum::extract::{Path, Query, RawQuery, State};
use axum::Json;
use chrono::{Duration, Utc};
use serde::Deserialize;
use uuid::Uuid;

use sauron_auth::{perm, AuthUser};
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
    // `environment_id` is deliberately NOT a field here — it is read from the
    // raw query string via `RawQuery` + `scope::authorized_read_scope`
    // instead of this `Query<T>` extractor. See `routes::scope`'s module docs
    // for the extractor trap this avoids.
}

#[derive(Deserialize)]
pub struct SeriesQuery {
    #[serde(default = "default_days")]
    pub since_days: i64,
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
    let since = Utc::now() - Duration::days(q.since_days.clamp(1, MAX_PERF_WINDOW_DAYS));
    let op = q.op.as_deref().filter(|s| !s.is_empty());
    Ok(Json(
        repo::performance_summary(&mut conn, scope, since, op, None).await?,
    ))
}

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
    let since = Utc::now() - Duration::days(q.since_days.clamp(1, MAX_PERF_WINDOW_DAYS));
    let name = q.name.as_deref().filter(|s| !s.is_empty());
    let op = q.op.as_deref().filter(|s| !s.is_empty());
    Ok(Json(
        repo::performance_series(&mut conn, scope, since, name, op).await?,
    ))
}
