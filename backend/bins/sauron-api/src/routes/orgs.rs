//! Organizations, membership (grants), roles, and the `/access` endpoint the
//! dashboard uses to gate its UI.

use axum::extract::{Path, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use sauron_auth::rbac::grants_from_rows;
use sauron_auth::{authorize_org, effective_at_org, perm, AuthError, AuthUser};
use sauron_db::models::{NewRoleGrant, Organization, Role};
use sauron_db::repo;

use super::{db, slugify};
use crate::error::ApiError;
use crate::AppState;

// --- orgs -------------------------------------------------------------------

pub async fn list_orgs(
    auth: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<Vec<Organization>>, ApiError> {
    let mut conn = db(&state).await?;
    Ok(Json(
        repo::list_orgs_for_user(&mut conn, auth.user_id).await?,
    ))
}

#[derive(Deserialize)]
pub struct CreateOrgReq {
    pub name: String,
}

pub async fn create_org(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(req): Json<CreateOrgReq>,
) -> Result<Json<Organization>, ApiError> {
    if req.name.trim().is_empty() {
        return Err(ApiError::BadRequest("organization name is required".into()));
    }
    let mut conn = db(&state).await?;
    let org = repo::create_org(&mut conn, &req.name, &slugify(&req.name)).await?;
    let owner = repo::get_system_role(&mut conn, "Owner")
        .await?
        .ok_or_else(|| ApiError::Internal("Owner preset role missing".into()))?;
    repo::create_grant(
        &mut conn,
        NewRoleGrant {
            org_id: org.id,
            user_id: auth.user_id,
            role_id: owner.id,
            scope_type: "org".into(),
            scope_id: org.id,
        },
    )
    .await?;
    Ok(Json(org))
}

// --- access (UI gating) -----------------------------------------------------

#[derive(Serialize)]
pub struct GrantView {
    pub scope_type: String,
    pub scope_id: Uuid,
    pub permissions: Vec<String>,
}

#[derive(Serialize)]
pub struct AccessResponse {
    /// Org-level effective permissions.
    pub permissions: Vec<String>,
    /// The caller's raw grants, so the UI can evaluate project/app scopes too.
    pub grants: Vec<GrantView>,
}

pub async fn access(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(org_id): Path<Uuid>,
) -> Result<Json<AccessResponse>, ApiError> {
    let mut conn = db(&state).await?;
    let rows = repo::user_grants_in_org(&mut conn, auth.user_id, org_id).await?;
    if rows.is_empty() {
        return Err(ApiError::Auth(AuthError::Forbidden));
    }
    let grants: Vec<GrantView> = grants_from_rows(rows)
        .into_iter()
        .map(|g| {
            let (scope_type, scope_id) = match g.scope {
                sauron_auth::rbac::Scope::Org(id) => ("org", id),
                sauron_auth::rbac::Scope::Project(id) => ("project", id),
                sauron_auth::rbac::Scope::App(id) => ("app", id),
            };
            GrantView {
                scope_type: scope_type.to_string(),
                scope_id,
                permissions: g.permissions,
            }
        })
        .collect();
    let org_perms = effective_at_org(&mut conn, auth.user_id, org_id).await?;
    let mut permissions: Vec<String> = org_perms.into_iter().collect();
    permissions.sort();
    Ok(Json(AccessResponse {
        permissions,
        grants,
    }))
}

// --- members / grants -------------------------------------------------------

#[derive(Serialize)]
pub struct MemberGrant {
    pub id: Uuid,
    pub user_id: Uuid,
    pub email: String,
    pub name: String,
    pub role_id: Uuid,
    pub role_name: String,
    pub scope_type: String,
    pub scope_id: Uuid,
}

pub async fn list_members(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(org_id): Path<Uuid>,
) -> Result<Json<Vec<MemberGrant>>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_org(&mut conn, auth.user_id, org_id, perm::MEMBER_READ).await?;
    let rows = repo::list_org_grants(&mut conn, org_id).await?;
    let members = rows
        .into_iter()
        .map(|(g, email, name, role_name)| MemberGrant {
            id: g.id,
            user_id: g.user_id,
            email,
            name,
            role_id: g.role_id,
            role_name,
            scope_type: g.scope_type,
            scope_id: g.scope_id,
        })
        .collect();
    Ok(Json(members))
}

#[derive(Deserialize)]
pub struct CreateGrantReq {
    pub email: String,
    pub role_id: Uuid,
    pub scope_type: String,
    pub scope_id: Uuid,
}

pub async fn create_grant(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(org_id): Path<Uuid>,
    Json(req): Json<CreateGrantReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_org(&mut conn, auth.user_id, org_id, perm::MEMBER_MANAGE).await?;

    if !matches!(req.scope_type.as_str(), "org" | "project" | "app") {
        return Err(ApiError::BadRequest("invalid scope_type".into()));
    }

    // Target user must already exist.
    let user = repo::find_user_by_email(&mut conn, &req.email)
        .await?
        .ok_or_else(|| {
            ApiError::BadRequest("no user with that email (ask them to sign up)".into())
        })?;

    // Role must be a preset or belong to this org.
    let role = repo::get_role(&mut conn, req.role_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if let Some(role_org) = role.org_id {
        if role_org != org_id {
            return Err(ApiError::BadRequest(
                "role does not belong to this org".into(),
            ));
        }
    }

    // Scope target must belong to this org (prevents cross-org grants). Also
    // capture the scope's (project, app) for the escalation check below.
    let (scope_project, scope_app): (Option<Uuid>, Option<Uuid>) = match req.scope_type.as_str() {
        "org" => {
            if req.scope_id != org_id {
                return Err(ApiError::BadRequest(
                    "scope target is not in this org".into(),
                ));
            }
            (None, None)
        }
        "project" => {
            if repo::project_org(&mut conn, req.scope_id).await? != Some(org_id) {
                return Err(ApiError::BadRequest(
                    "scope target is not in this org".into(),
                ));
            }
            (Some(req.scope_id), None)
        }
        "app" => match repo::app_ancestry(&mut conn, req.scope_id).await? {
            Some((project_id, o)) if o == org_id => (Some(project_id), Some(req.scope_id)),
            _ => {
                return Err(ApiError::BadRequest(
                    "scope target is not in this org".into(),
                ))
            }
        },
        _ => unreachable!("scope_type validated above"),
    };

    // No privilege escalation: the granter must themselves hold every permission
    // the granted role confers, at the grant's scope. (Stops an Admin from
    // granting Owner to gain org:manage.)
    let role_perms: Vec<String> = match &role.permissions {
        Value::Array(a) => a
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect(),
        _ => Vec::new(),
    };
    let granter =
        sauron_auth::effective_at(&mut conn, auth.user_id, org_id, scope_project, scope_app)
            .await?;
    for p in &role_perms {
        if !granter.contains(p) {
            return Err(ApiError::Auth(AuthError::Forbidden));
        }
    }

    let grant = repo::create_grant(
        &mut conn,
        NewRoleGrant {
            org_id,
            user_id: user.id,
            role_id: req.role_id,
            scope_type: req.scope_type,
            scope_id: req.scope_id,
        },
    )
    .await?;
    Ok(Json(serde_json::json!({ "id": grant.id })))
}

pub async fn delete_grant(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(grant_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut conn = db(&state).await?;
    let org_id = repo::grant_org(&mut conn, grant_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    authorize_org(&mut conn, auth.user_id, org_id, perm::MEMBER_MANAGE).await?;
    repo::delete_grant(&mut conn, org_id, grant_id).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

// --- roles ------------------------------------------------------------------

pub async fn list_roles(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(org_id): Path<Uuid>,
) -> Result<Json<Vec<Role>>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_org(&mut conn, auth.user_id, org_id, perm::MEMBER_READ).await?;
    Ok(Json(repo::list_roles(&mut conn, org_id).await?))
}

#[derive(Deserialize)]
pub struct CreateRoleReq {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub permissions: Vec<String>,
}

pub async fn create_role(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(org_id): Path<Uuid>,
    Json(req): Json<CreateRoleReq>,
) -> Result<Json<Role>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_org(&mut conn, auth.user_id, org_id, perm::ROLE_MANAGE).await?;
    if req.name.trim().is_empty() {
        return Err(ApiError::BadRequest("role name is required".into()));
    }
    // Only known permissions are accepted.
    for p in &req.permissions {
        if !perm::ALL.contains(&p.as_str()) {
            return Err(ApiError::BadRequest(format!("unknown permission: {p}")));
        }
    }
    // No privilege escalation: a role may only contain permissions the creator
    // themselves holds at org scope.
    let own = sauron_auth::effective_at_org(&mut conn, auth.user_id, org_id).await?;
    for p in &req.permissions {
        if !own.contains(p) {
            return Err(ApiError::Auth(AuthError::Forbidden));
        }
    }
    let perms = Value::Array(
        req.permissions
            .iter()
            .map(|p| Value::String(p.clone()))
            .collect(),
    );
    let role = repo::create_role(&mut conn, org_id, &req.name, &req.description, perms).await?;
    Ok(Json(role))
}
