//! Journey API: a step-indexed transition graph over user event streams, shaped
//! for a Sankey diagram (nodes per step + weighted links between adjacent steps).

use axum::extract::{Path, Query, State};
use axum::Json;
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use sauron_auth::{authorize_app, perm, AuthUser};
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
) -> Result<Json<Journey>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::EVENT_READ).await?;
    let since = Utc::now() - Duration::days(q.since_days.clamp(1, 365));
    let depth = q.depth.clamp(2, 10);

    let nodes = repo::journey_nodes(&mut conn, app_id, since, depth).await?;
    let links = repo::journey_links(&mut conn, app_id, since, depth).await?;

    Ok(Json(Journey {
        depth,
        nodes,
        links,
    }))
}
