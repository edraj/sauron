//! Funnel API: given an ordered list of event names, compute how many distinct
//! people progressed through each step (each step counted at-or-after the
//! previous step's time), plus conversion ratios.

use axum::extract::{Path, State};
use axum::Json;
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use sauron_auth::{authorize_app, perm, AuthUser};
use sauron_db::repo;

use super::db;
use crate::error::ApiError;
use crate::AppState;

#[derive(Deserialize)]
pub struct FunnelReq {
    pub steps: Vec<String>,
    #[serde(default = "default_days")]
    pub since_days: i64,
}

fn default_days() -> i64 {
    30
}

#[derive(Serialize)]
pub struct FunnelStep {
    pub name: String,
    pub count: i64,
    /// Conversion from the very first step (0..=1).
    pub conv_from_start: f64,
    /// Conversion from the immediately preceding step (0..=1).
    pub conv_from_prev: f64,
}

#[derive(Serialize)]
pub struct FunnelResult {
    pub total_entered: i64,
    pub steps: Vec<FunnelStep>,
}

pub async fn compute(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Json(req): Json<FunnelReq>,
) -> Result<Json<FunnelResult>, ApiError> {
    if req.steps.len() < 2 {
        return Err(ApiError::BadRequest(
            "a funnel needs at least 2 steps".into(),
        ));
    }
    if req.steps.len() > 10 {
        return Err(ApiError::BadRequest("at most 10 steps".into()));
    }

    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::EVENT_READ).await?;
    let since = Utc::now() - Duration::days(req.since_days.clamp(1, 365));

    let rows = repo::funnel(&mut conn, app_id, &req.steps, since).await?;
    // rows come back ordered by step; index defensively by step id.
    let mut counts = vec![0i64; req.steps.len()];
    for r in rows {
        if let Some(slot) = counts.get_mut(r.step as usize) {
            *slot = r.count;
        }
    }

    let total = counts.first().copied().unwrap_or(0);
    let steps = req
        .steps
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let count = counts[i];
            let prev = if i == 0 { count } else { counts[i - 1] };
            FunnelStep {
                name: name.clone(),
                count,
                conv_from_start: ratio(count, total),
                conv_from_prev: ratio(count, prev),
            }
        })
        .collect();

    Ok(Json(FunnelResult {
        total_entered: total,
        steps,
    }))
}

fn ratio(num: i64, den: i64) -> f64 {
    if den <= 0 {
        0.0
    } else {
        num as f64 / den as f64
    }
}

// ---------------------------------------------------------------------------
// Saved funnel templates (CRUD)
// ---------------------------------------------------------------------------

/// Shared 2..=10 step-count validation (matches `compute`).
pub fn validate_steps(steps: &[String]) -> Result<(), String> {
    if steps.len() < 2 {
        return Err("a funnel needs at least 2 steps".into());
    }
    if steps.len() > 10 {
        return Err("at most 10 steps".into());
    }
    if steps.iter().any(|s| s.trim().is_empty()) {
        return Err("steps cannot be empty".into());
    }
    Ok(())
}

#[derive(Deserialize)]
pub struct SaveFunnelReq {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub steps: Vec<String>,
}

pub async fn list_saved(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
) -> Result<Json<Vec<repo::SavedFunnelRow>>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::EVENT_READ).await?;
    Ok(Json(repo::list_saved_funnels(&mut conn, app_id).await?))
}

pub async fn create_saved(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Json(req): Json<SaveFunnelReq>,
) -> Result<Json<repo::SavedFunnelRow>, ApiError> {
    if req.name.trim().is_empty() {
        return Err(ApiError::BadRequest("name is required".into()));
    }
    validate_steps(&req.steps).map_err(ApiError::BadRequest)?;
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::FUNNEL_WRITE).await?;
    let steps = serde_json::json!(req.steps);
    let row = repo::create_saved_funnel(
        &mut conn,
        app_id,
        auth.user_id,
        req.name.trim(),
        req.description.as_deref(),
        &steps,
    )
    .await?;
    Ok(Json(row))
}

pub async fn update_saved(
    auth: AuthUser,
    State(state): State<AppState>,
    Path((app_id, funnel_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<SaveFunnelReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if req.name.trim().is_empty() {
        return Err(ApiError::BadRequest("name is required".into()));
    }
    validate_steps(&req.steps).map_err(ApiError::BadRequest)?;
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::FUNNEL_WRITE).await?;
    let steps = serde_json::json!(req.steps);
    let n = repo::update_saved_funnel(
        &mut conn,
        app_id,
        funnel_id,
        req.name.trim(),
        req.description.as_deref(),
        &steps,
    )
    .await?;
    if n == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

pub async fn delete_saved(
    auth: AuthUser,
    State(state): State<AppState>,
    Path((app_id, funnel_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::FUNNEL_WRITE).await?;
    let n = repo::delete_saved_funnel(&mut conn, app_id, funnel_id).await?;
    if n == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[cfg(test)]
mod validate_steps_tests {
    use super::validate_steps;

    #[test]
    fn rejects_too_few() {
        assert!(validate_steps(&["a".into()]).is_err());
    }

    #[test]
    fn rejects_too_many() {
        let steps: Vec<String> = (0..11).map(|i| i.to_string()).collect();
        assert!(validate_steps(&steps).is_err());
    }

    #[test]
    fn accepts_two_to_ten() {
        assert!(validate_steps(&["a".into(), "b".into()]).is_ok());
    }
}
