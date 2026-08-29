//! Journey API: a step-indexed transition graph over user event streams, shaped
//! for a Sankey diagram (nodes per step + weighted links between adjacent steps).

use axum::extract::{Path, Query, RawQuery, State};
use axum::Json;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use sauron_auth::{perm, AuthUser};
use sauron_db::repo;
use sauron_db::repo::{JourneyLink, JourneyNode};

use super::db;
use crate::error::ApiError;
use crate::openapi::ErrorResponse;
use crate::AppState;

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct JourneyQuery {
    #[serde(default = "default_days")]
    pub since_days: i64,
    /// Absolute window bounds, `from` INCLUSIVE and `to` EXCLUSIVE, overriding
    /// `since_days` when either is present. See `analytics::RangeQuery` for why
    /// these are two plain fields rather than a flattened shared struct.
    pub from: Option<chrono::DateTime<chrono::Utc>>,
    pub to: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default = "default_depth")]
    pub depth: i64,
    // `environment_id` is deliberately NOT a field here — it is read from the
    // raw query string via `RawQuery` + `scope::authorized_read_scope`
    // instead of this `Query<T>` extractor. See `routes::scope`'s module docs
    // for the extractor trap this avoids.
}

fn default_days() -> i64 {
    30
}
fn default_depth() -> i64 {
    5
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct Journey {
    pub depth: i64,
    pub nodes: Vec<JourneyNode>,
    pub links: Vec<JourneyLink>,
}

#[utoipa::path(
    get, path = "/v1/apps/{app_id}/journeys", tag = "Analytics",
    summary = "Explore user journeys",
    description = "Common paths users take through the app, as a tree of steps rooted at a chosen starting event.",
    params(("app_id" = Uuid, Path, description = "The app."), JourneyQuery, super::search::TimeFilterQuery), security(("bearerAuth" = [])),
    responses((status = 200, description = "The journey tree.", body = Journey),
              (status = 400, description = "Malformed step selection.", body = ErrorResponse), (status = 401, description = "Missing or invalid access token.", body = ErrorResponse), (status = 403, description = "No grant covers this scope.", body = ErrorResponse), (status = 503, description = "The query exceeded its time budget, or a required rollup has not been backfilled. The message names which.", body = ErrorResponse)),
)]
pub async fn explore(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<JourneyQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<Journey>, ApiError> {
    let mut conn = db(&state).await?;
    // One query for both halves of the graph: the step-indexed window CTE is
    // the expensive part and was previously evaluated once per result set.
    let scope = super::scope::authorized_read_scope(
        &mut conn,
        auth.user_id,
        app_id,
        perm::EVENT_READ,
        raw_query.as_deref(),
    )
    .await?;
    let win =
        super::search::resolve_range("occurred_at", q.from, q.to, q.since_days, Utc::now(), 365)?;
    let depth = q.depth.clamp(2, 10);

    let (nodes, links) = repo::journey_graph(&mut conn, scope, win, depth).await?;

    Ok(Json(Journey {
        depth,
        nodes,
        links,
    }))
}
