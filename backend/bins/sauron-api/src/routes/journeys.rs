//! Journey API: a step-indexed transition graph over user event streams, shaped
//! for a Sankey diagram (nodes per step + weighted links between adjacent steps).

use axum::extract::{Path, Query, RawQuery, State};
use axum::Json;
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use sauron_auth::{perm, AuthUser};
use sauron_db::repo;
use sauron_db::repo::{JourneyLink, JourneyNode};

use super::db;
use crate::error::ApiError;
use crate::AppState;

#[derive(Deserialize)]
pub struct JourneyQuery {
    #[serde(default = "default_days")]
    pub since_days: i64,
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

#[derive(Serialize)]
pub struct Journey {
    pub depth: i64,
    pub nodes: Vec<JourneyNode>,
    pub links: Vec<JourneyLink>,
}

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
    let since = Utc::now() - Duration::days(q.since_days.clamp(1, 365));
    let depth = q.depth.clamp(2, 10);

    let (nodes, links) = repo::journey_graph(&mut conn, scope, since, depth).await?;

    Ok(Json(Journey {
        depth,
        nodes,
        links,
    }))
}
