//! Projects (the grouping level) and the apps that live under them.

use axum::extract::{Path, State};
use axum::Json;
use serde::Deserialize;
use uuid::Uuid;

use sauron_auth::{authorize_org, authorize_project, perm, AuthUser};
use sauron_core::ids;
use sauron_db::models::{App, Project};
use sauron_db::repo;

use super::{db, slugify};
use crate::error::ApiError;
use crate::AppState;

const APP_TYPES: [&str; 8] = [
    "web",
    "flutter",
    "ios",
    "android",
    "react_native",
    "node",
    "python",
    "csharp",
];

pub async fn list_projects(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(org_id): Path<Uuid>,
) -> Result<Json<Vec<Project>>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_org(&mut conn, auth.user_id, org_id, perm::PROJECT_READ).await?;
    Ok(Json(repo::list_projects_for_org(&mut conn, org_id).await?))
}

#[derive(Deserialize)]
pub struct CreateProjectReq {
    pub name: String,
}

pub async fn create_project(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(org_id): Path<Uuid>,
    Json(req): Json<CreateProjectReq>,
) -> Result<Json<Project>, ApiError> {
    if req.name.trim().is_empty() {
        return Err(ApiError::BadRequest("project name is required".into()));
    }
    let mut conn = db(&state).await?;
    authorize_org(&mut conn, auth.user_id, org_id, perm::PROJECT_CREATE).await?;
    let project = repo::create_project(&mut conn, org_id, &req.name, &slugify(&req.name)).await?;
    Ok(Json(project))
}

pub async fn get_project(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(project_id): Path<Uuid>,
) -> Result<Json<Project>, ApiError> {
    let mut conn = db(&state).await?;
    let project =
        authorize_project(&mut conn, auth.user_id, project_id, perm::PROJECT_READ).await?;
    Ok(Json(project))
}

#[derive(Deserialize)]
pub struct UpdateProjectReq {
    pub name: String,
}

pub async fn update_project(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(project_id): Path<Uuid>,
    Json(req): Json<UpdateProjectReq>,
) -> Result<Json<Project>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_project(&mut conn, auth.user_id, project_id, perm::PROJECT_UPDATE).await?;
    if req.name.trim().is_empty() {
        return Err(ApiError::BadRequest("project name is required".into()));
    }
    let project = repo::rename_project(&mut conn, project_id, &req.name)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(project))
}

pub async fn delete_project(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(project_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_project(&mut conn, auth.user_id, project_id, perm::PROJECT_DELETE).await?;
    repo::delete_project(&mut conn, project_id).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

// --- apps under a project ---------------------------------------------------

pub async fn list_apps(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(project_id): Path<Uuid>,
) -> Result<Json<Vec<App>>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_project(&mut conn, auth.user_id, project_id, perm::APP_READ).await?;
    Ok(Json(
        repo::list_apps_for_project(&mut conn, project_id).await?,
    ))
}

#[derive(Deserialize)]
pub struct CreateAppReq {
    pub name: String,
    pub app_type: String,
}

pub async fn create_app(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(project_id): Path<Uuid>,
    Json(req): Json<CreateAppReq>,
) -> Result<Json<App>, ApiError> {
    if req.name.trim().is_empty() {
        return Err(ApiError::BadRequest("app name is required".into()));
    }
    if !APP_TYPES.contains(&req.app_type.as_str()) {
        return Err(ApiError::BadRequest(format!(
            "invalid app_type; must be one of {}",
            APP_TYPES.join(", ")
        )));
    }
    let mut conn = db(&state).await?;
    authorize_project(&mut conn, auth.user_id, project_id, perm::APP_CREATE).await?;

    let public_key = ids::public_key();
    let app = repo::create_app(
        &mut conn,
        project_id,
        &req.name,
        &slugify(&req.name),
        &req.app_type,
        &public_key,
    )
    .await?;
    // Seed a default environment.
    let _ = repo::upsert_environment(&mut conn, app.id, "production").await;
    Ok(Json(app))
}
