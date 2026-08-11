//! Projects (the grouping level) and the apps that live under them.

use std::collections::HashSet;

use axum::extract::{Path, State};
use axum::Json;
use diesel_async::AsyncConnection;
use serde::Deserialize;
use uuid::Uuid;

use sauron_auth::rbac::{grants_from_rows, reach_for};
use sauron_auth::{authorize_org, authorize_project, perm, AuthError, AuthUser};
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
    // `authorize_org` checks a single fixed scope and can never be satisfied by
    // a project/app-scoped grant (see `grant_applies`) — that's the exact bug
    // this endpoint used to have. Listing needs the inverse of an authorize
    // check: load the grants once, then ask `reach_for` which resources they
    // actually cover, rather than gating on a check no scoped grant can pass.
    let rows = repo::user_grants_in_org(&mut conn, auth.user_id, org_id).await?;
    if rows.is_empty() {
        // Not a member of this org at all — distinct from "a member who can't
        // see anything", which returns 200 with an empty/partial list below.
        return Err(ApiError::Auth(AuthError::Forbidden));
    }
    let grants = grants_from_rows(rows);
    let reach = reach_for(&grants, perm::PROJECT_READ);
    if reach.org {
        return Ok(Json(repo::list_projects_for_org(&mut conn, org_id).await?));
    }

    let mut project_ids = reach.projects;
    if !reach.apps.is_empty() {
        let ancestries = repo::app_ancestries(&mut conn, &reach.apps).await?;
        project_ids.extend(
            ancestries
                .into_iter()
                .filter(|(_, _, ancestor_org)| *ancestor_org == org_id)
                .map(|(_, project_id, _)| project_id),
        );
    }
    // Same lift, one level further down: an env-only grant (no ancestor org/
    // project/app grant) must still surface its project in the switcher, or
    // there is no path from login to the one app that grant is for. One
    // batched `env_ancestries` call, not one query per environment — filtered
    // to this org exactly like the app lift above, so a stray env grant
    // belonging to another org (should never happen, `role_grants.scope_id`
    // has no FK, so "should" is doing work) still cannot widen this list.
    if !reach.envs.is_empty() {
        let env_ancestries = repo::env_ancestries(&mut conn, &reach.envs).await?;
        project_ids.extend(
            env_ancestries
                .into_iter()
                .filter(|(_, _, _, ancestor_org)| *ancestor_org == org_id)
                .map(|(_, _, project_id, _)| project_id),
        );
    }
    project_ids.sort();
    project_ids.dedup();
    Ok(Json(
        repo::list_projects_by_ids_in_org(&mut conn, org_id, &project_ids).await?,
    ))
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
    // Both inserts run in one transaction. A project with an empty environment
    // catalogue cannot have a usable app created in it — `enroll_new_app` has
    // nothing to enroll into — so a project that lost its default environment to
    // a partial failure would be permanently broken rather than merely empty.
    let project = conn
        .transaction::<_, ApiError, _>(async |conn| {
            let project =
                repo::create_project(conn, org_id, &req.name, &slugify(&req.name)).await?;
            repo::create_project_environment(
                conn,
                project.id,
                crate::routes::environments::DEFAULT_ENV_NAME,
            )
            .await?;
            Ok(project)
        })
        .await?;

    crate::audit::record(
        &mut conn,
        auth.user_id,
        crate::audit::Entry::new(
            org_id,
            crate::audit::action::PROJECT_CREATE,
            crate::audit::entity::PROJECT,
        )
        .target(project.id, &project.name)
        .project(project.id, &project.name)
        .changes(crate::audit::created(
            crate::audit::entity::PROJECT,
            &[
                ("name", serde_json::json!(project.name)),
                ("slug", serde_json::json!(project.slug)),
            ],
        )),
    )
    .await;
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
    let before =
        authorize_project(&mut conn, auth.user_id, project_id, perm::PROJECT_UPDATE).await?;
    if req.name.trim().is_empty() {
        return Err(ApiError::BadRequest("project name is required".into()));
    }
    let project = repo::rename_project(&mut conn, project_id, &req.name)
        .await?
        .ok_or(ApiError::NotFound)?;

    crate::audit::record(
        &mut conn,
        auth.user_id,
        crate::audit::Entry::new(
            project.org_id,
            crate::audit::action::PROJECT_UPDATE,
            crate::audit::entity::PROJECT,
        )
        .target(project.id, &project.name)
        .project(project.id, &project.name)
        .changes(crate::audit::diff(
            crate::audit::entity::PROJECT,
            &[
                (
                    "name",
                    serde_json::json!(before.name),
                    serde_json::json!(project.name),
                ),
                (
                    "slug",
                    serde_json::json!(before.slug),
                    serde_json::json!(project.slug),
                ),
            ],
        )),
    )
    .await;
    Ok(Json(project))
}

pub async fn delete_project(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(project_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut conn = db(&state).await?;
    let project =
        authorize_project(&mut conn, auth.user_id, project_id, perm::PROJECT_DELETE).await?;
    repo::delete_project(&mut conn, project_id).await?;

    // Recorded AFTER the delete, naming a project row that no longer exists.
    // Only possible because this table carries no FK on `project_id` — with
    // one, this insert would fail and the deletion would be the single action
    // guaranteed to go unrecorded. See migration 50.
    crate::audit::record(
        &mut conn,
        auth.user_id,
        crate::audit::Entry::new(
            project.org_id,
            crate::audit::action::PROJECT_DELETE,
            crate::audit::entity::PROJECT,
        )
        .target(project.id, &project.name)
        .project(project.id, &project.name),
    )
    .await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

// --- apps under a project ---------------------------------------------------

pub async fn list_apps(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(project_id): Path<Uuid>,
) -> Result<Json<Vec<App>>, ApiError> {
    let mut conn = db(&state).await?;
    // Same shape as `list_projects` above: `authorize_project` gates on a
    // fixed (org, project, None) target that an app-scoped grant can never
    // satisfy, one level below where `list_projects` used to break. Resolve
    // the project's org first (needed to load grants), then decompose reach.
    let org_id = repo::project_org(&mut conn, project_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let rows = repo::user_grants_in_org(&mut conn, auth.user_id, org_id).await?;
    if rows.is_empty() {
        return Err(ApiError::Auth(AuthError::Forbidden));
    }
    let grants = grants_from_rows(rows);
    let reach = reach_for(&grants, perm::APP_READ);
    if reach.org || reach.projects.contains(&project_id) {
        return Ok(Json(
            repo::list_apps_for_project(&mut conn, project_id).await?,
        ));
    }

    let mut allowed: HashSet<Uuid> = reach.apps.into_iter().collect();
    // Same lift as `list_projects` above, one level down: an env-only grant
    // must surface its own parent app in the switcher. Filtered to THIS
    // project — an env grant whose app belongs to a sibling project must not
    // widen this listing, mirroring the app-grant sibling-isolation the
    // `reach.projects.contains(&project_id)` fast path above already relies on.
    if !reach.envs.is_empty() {
        let env_ancestries = repo::env_ancestries(&mut conn, &reach.envs).await?;
        allowed.extend(
            env_ancestries
                .into_iter()
                .filter(|(_, _, ancestor_project, _)| *ancestor_project == project_id)
                .map(|(_, app_id, _, _)| app_id),
        );
    }
    let apps = repo::list_apps_for_project(&mut conn, project_id)
        .await?
        .into_iter()
        .filter(|a| allowed.contains(&a.id))
        .collect();
    Ok(Json(apps))
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
    let project =
        authorize_project(&mut conn, auth.user_id, project_id, perm::APP_CREATE).await?;

    // Both inserts run in one transaction: an app is unreachable by any SDK
    // without at least one enrollment holding an ingest key, so if the
    // enrollment insert fails, the app row must not survive either.
    //
    // The new app is enrolled in EVERY live environment of its project, not just
    // a default one — environments are a project-wide catalogue now, so an app
    // that only joined `dev` would be missing from the `staging` and `production`
    // pickers its siblings already appear in.
    let app = conn
        .transaction::<_, ApiError, _>(async |conn| {
            let app = repo::create_app(
                conn,
                project_id,
                &req.name,
                &slugify(&req.name),
                &req.app_type,
            )
            .await?;
            crate::routes::environments::enroll_new_app(conn, app.id, project_id).await?;
            Ok(app)
        })
        .await?;

    // The enrollment keys minted by `enroll_new_app` are deliberately absent:
    // `app` is not the allowlisted-field source, `audit::created` is, and
    // `public_key` is not on that list.
    crate::audit::record(
        &mut conn,
        auth.user_id,
        crate::audit::Entry::new(
            project.org_id,
            crate::audit::action::APP_CREATE,
            crate::audit::entity::APP,
        )
        .target(app.id, &app.name)
        .project(project.id, &project.name)
        .app(app.id, &app.name)
        .changes(crate::audit::created(
            crate::audit::entity::APP,
            &[
                ("name", serde_json::json!(app.name)),
                ("platform", serde_json::json!(app.app_type)),
            ],
        )),
    )
    .await;
    Ok(Json(app))
}
