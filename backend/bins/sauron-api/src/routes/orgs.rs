//! Organizations, membership (grants), roles, and the `/access` endpoint the
//! dashboard uses to gate its UI.

use axum::extract::{Path, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use sauron_auth::guard::{
    check_no_escalation, check_role_edit, drops_org_manage, generate_temp_password,
    role_permissions, scope_parts,
};
use sauron_auth::hash_password_async;
use sauron_auth::rbac::grants_from_rows;
use sauron_auth::{authorize_org, perm, AuthError, AuthUser};
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
    // Parse once and derive both outputs locally. `effective_at_org` would
    // re-query the identical grant rows, doubling DB round-trips on the endpoint
    // the dashboard hits on every page load and org switch.
    let parsed = grants_from_rows(rows);
    let mut permissions: Vec<String> =
        sauron_auth::rbac::effective_permissions(&parsed, org_id, None, None)
            .into_iter()
            .collect();
    permissions.sort();
    let grants: Vec<GrantView> = parsed
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

/// Validate that a scope target belongs to `org_id`, returning the app's
/// parent project when the scope is an app (which `scope_parts` needs).
///
/// This is the cross-tenant boundary for grants: without it a caller could
/// name a project or app in someone else's org and have a grant created
/// against it. One implementation, called by every handler that accepts a
/// caller-supplied scope.
async fn validate_scope_in_org(
    conn: &mut sauron_db::AsyncPgConnection,
    org_id: Uuid,
    scope_type: &str,
    scope_id: Uuid,
) -> Result<Option<Uuid>, ApiError> {
    let not_in_org = || ApiError::BadRequest("scope target is not in this org".into());
    match scope_type {
        "org" => {
            if scope_id != org_id {
                return Err(not_in_org());
            }
            Ok(None)
        }
        "project" => {
            if repo::project_org(conn, scope_id).await? != Some(org_id) {
                return Err(not_in_org());
            }
            Ok(None)
        }
        "app" => match repo::app_ancestry(conn, scope_id).await? {
            Some((project_id, o)) if o == org_id => Ok(Some(project_id)),
            _ => Err(not_in_org()),
        },
        _ => Err(ApiError::BadRequest("invalid scope_type".into())),
    }
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
    let project_of_app =
        validate_scope_in_org(&mut conn, org_id, &req.scope_type, req.scope_id).await?;
    let (scope_project, scope_app) = scope_parts(&req.scope_type, req.scope_id, project_of_app);

    // No privilege escalation: the granter must themselves hold every permission
    // the granted role confers, at the grant's scope. (Stops an Admin from
    // granting Owner to gain org:manage.)
    let role_perms = role_permissions(&role.permissions);
    let granter =
        sauron_auth::effective_at(&mut conn, auth.user_id, org_id, scope_project, scope_app)
            .await?;
    check_no_escalation(&granter, &role_perms).map_err(ApiError::Auth)?;

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

#[derive(Deserialize)]
pub struct CreateMemberReq {
    pub email: String,
    #[serde(default)]
    pub name: String,
    pub role_id: Uuid,
    pub scope_type: String,
    pub scope_id: Uuid,
}

/// Create a user account and its first grant in one step.
///
/// The password is generated, never supplied by the caller: an admin who could
/// choose it would hold a working durable credential for somebody else's
/// account. It is returned exactly once, here, and `must_change_password`
/// makes it useless for anything but being replaced.
pub async fn create_member(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(org_id): Path<Uuid>,
    Json(req): Json<CreateMemberReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_org(&mut conn, auth.user_id, org_id, perm::MEMBER_MANAGE).await?;

    if !req.email.contains('@') {
        return Err(ApiError::BadRequest("a valid email is required".into()));
    }
    if !matches!(req.scope_type.as_str(), "org" | "project" | "app") {
        return Err(ApiError::BadRequest("invalid scope_type".into()));
    }
    if repo::find_user_by_email(&mut conn, &req.email)
        .await?
        .is_some()
    {
        return Err(ApiError::Conflict(
            "a user with that email already exists — use Grant access instead".into(),
        ));
    }

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

    // Scope target must belong to this org, and gives us the (project, app)
    // pair the escalation check needs. Shared helper from Task 4 Step 3a — the
    // org-containment check has one implementation, not one per handler.
    let project_of_app =
        validate_scope_in_org(&mut conn, org_id, &req.scope_type, req.scope_id).await?;
    let (scope_project, scope_app) = scope_parts(&req.scope_type, req.scope_id, project_of_app);

    // Creating a user must not be a way around the grant escalation check.
    let role_perms = role_permissions(&role.permissions);
    let creator =
        sauron_auth::effective_at(&mut conn, auth.user_id, org_id, scope_project, scope_app)
            .await?;
    check_no_escalation(&creator, &role_perms).map_err(ApiError::Auth)?;

    let temp_password = generate_temp_password();
    let hash = hash_password_async(temp_password.clone())
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    // One statement, atomic: a grant failure must not leave an account that
    // holds the email but has no access and appears in no list.
    let created = repo::create_member_with_grant(
        &mut conn,
        &req.email,
        &hash,
        &req.name,
        org_id,
        req.role_id,
        &req.scope_type,
        req.scope_id,
    )
    .await
    .map_err(|e| match e {
        diesel::result::Error::DatabaseError(
            diesel::result::DatabaseErrorKind::UniqueViolation,
            _,
        ) => ApiError::Conflict(
            "a user with that email already exists — use Grant access instead".into(),
        ),
        other => ApiError::from(other),
    })?;

    Ok(Json(serde_json::json!({
        "user_id": created.user_id,
        "grant_id": created.grant_id,
        "temp_password": temp_password,
    })))
}

pub async fn delete_grant(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(grant_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut conn = db(&state).await?;
    let grant = repo::get_grant(&mut conn, grant_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let org_id = grant.org_id;
    authorize_org(&mut conn, auth.user_id, org_id, perm::MEMBER_MANAGE).await?;

    let role = repo::get_role(&mut conn, grant.role_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let role_perms = role_permissions(&role.permissions);

    // Symmetry with create_grant: you may not remove a grant conferring
    // permissions you do not hold yourself at that scope. Without this, an Admin
    // (member:manage but not org:manage) could delete the Owner's grant and
    // evict them from their own org.
    let project_of_app = if grant.scope_type == "app" {
        repo::app_ancestry(&mut conn, grant.scope_id)
            .await?
            .map(|(project_id, _)| project_id)
    } else {
        None
    };
    let (scope_project, scope_app) = scope_parts(&grant.scope_type, grant.scope_id, project_of_app);
    let remover =
        sauron_auth::effective_at(&mut conn, auth.user_id, org_id, scope_project, scope_app)
            .await?;
    check_no_escalation(&remover, &role_perms).map_err(ApiError::Auth)?;

    // Never let the org lose its last administrator: once no grant confers
    // org:manage, create_grant's own escalation check makes it impossible for
    // anyone to ever re-create one, permanently orphaning the org.
    if role_perms.iter().any(|p| p == perm::ORG_MANAGE) {
        let remaining =
            repo::count_org_manage_grants_excluding(&mut conn, org_id, grant_id).await?;
        if remaining == 0 {
            return Err(ApiError::Conflict(
                "cannot remove the last grant with org:manage — assign it to another member first"
                    .into(),
            ));
        }
    }

    repo::delete_grant(&mut conn, org_id, grant_id).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct SetMemberActiveReq {
    pub is_active: bool,
}

/// Enable or disable a member's ability to log in.
///
/// Deliberately leaves `role_grants` untouched. This is not a delete: the
/// member stays in the list, badged and reversible. Removing access to one
/// scope is what `DELETE /v1/grants/{id}` is for.
pub async fn set_member_active(
    auth: AuthUser,
    State(state): State<AppState>,
    Path((org_id, user_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<SetMemberActiveReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_org(&mut conn, auth.user_id, org_id, perm::MEMBER_MANAGE).await?;

    let _user = repo::get_user(&mut conn, user_id)
        .await?
        .ok_or(ApiError::NotFound)?;

    // The target must actually be a member of this org, or any admin could
    // toggle any account in the deployment by guessing a uuid.
    if repo::user_grants_in_org(&mut conn, user_id, org_id)
        .await?
        .is_empty()
    {
        return Err(ApiError::NotFound);
    }

    if !req.is_active {
        if user_id == auth.user_id {
            return Err(ApiError::Conflict(
                "you cannot deactivate your own account".into(),
            ));
        }
        // member:manage is org-scoped; deactivation is account-global. Allowing
        // it for someone who also belongs to another org would let this org's
        // admin lock them out of an org they have no authority over.
        if repo::count_user_grants_outside_org(&mut conn, user_id, org_id).await? > 0 {
            return Err(ApiError::Conflict(
                "this member belongs to another organization and cannot be deactivated from here"
                    .into(),
            ));
        }
        // Same reasoning as delete_grant's last-owner guard: an org with no
        // org:manage holder can never regain one, because create_grant's
        // escalation check makes it ungrantable.
        if repo::count_org_manage_grants_for_user_excluding_user(&mut conn, org_id, user_id).await?
            == 0
        {
            return Err(ApiError::Conflict(
                "cannot deactivate the last member with org:manage — assign it to someone else first"
                    .into(),
            ));
        }
    }

    repo::set_user_active(&mut conn, user_id, req.is_active).await?;
    if !req.is_active {
        repo::revoke_all_refresh_tokens_for_user_with_reason(
            &mut conn,
            user_id,
            repo::REVOKE_DEACTIVATED,
        )
        .await?;
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct UpdateGrantReq {
    pub role_id: Option<Uuid>,
    pub scope_type: Option<String>,
    pub scope_id: Option<Uuid>,
}

/// Change a member's role and/or scope in place.
///
/// One statement rather than a client-side delete-then-recreate: a recreate
/// that failed would silently strand the member with no access, and the
/// last-owner guard has to judge the final state, not the intermediate one
/// where the grant is already gone.
pub async fn update_grant_handler(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(grant_id): Path<Uuid>,
    Json(req): Json<UpdateGrantReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut conn = db(&state).await?;
    let grant = repo::get_grant(&mut conn, grant_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let org_id = grant.org_id;
    authorize_org(&mut conn, auth.user_id, org_id, perm::MEMBER_MANAGE).await?;

    let new_role_id = req.role_id.unwrap_or(grant.role_id);
    let new_scope_type = req
        .scope_type
        .clone()
        .unwrap_or_else(|| grant.scope_type.clone());
    let new_scope_id = req.scope_id.unwrap_or(grant.scope_id);

    if !matches!(new_scope_type.as_str(), "org" | "project" | "app") {
        return Err(ApiError::BadRequest("invalid scope_type".into()));
    }

    let new_role = repo::get_role(&mut conn, new_role_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if let Some(role_org) = new_role.org_id {
        if role_org != org_id {
            return Err(ApiError::BadRequest(
                "role does not belong to this org".into(),
            ));
        }
    }

    // New scope must be inside this org. Shared helper from Task 4 Step 3a.
    let new_project_of_app =
        validate_scope_in_org(&mut conn, org_id, &new_scope_type, new_scope_id).await?;

    // Both directions, mirroring create_grant + delete_grant: the caller must
    // outrank what they are granting AND what they are taking away. Checking
    // only the new role would let an Admin rewrite the Owner's grant down to
    // Viewer — a delete they are already forbidden from performing.
    let old_role = repo::get_role(&mut conn, grant.role_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let old_perms = role_permissions(&old_role.permissions);
    let new_perms = role_permissions(&new_role.permissions);

    let old_project_of_app = if grant.scope_type == "app" {
        repo::app_ancestry(&mut conn, grant.scope_id)
            .await?
            .map(|(project_id, _)| project_id)
    } else {
        None
    };
    let (old_sp, old_sa) = scope_parts(&grant.scope_type, grant.scope_id, old_project_of_app);
    let caller_at_old =
        sauron_auth::effective_at(&mut conn, auth.user_id, org_id, old_sp, old_sa).await?;
    check_no_escalation(&caller_at_old, &old_perms).map_err(ApiError::Auth)?;

    let (new_sp, new_sa) = scope_parts(&new_scope_type, new_scope_id, new_project_of_app);
    let caller_at_new =
        sauron_auth::effective_at(&mut conn, auth.user_id, org_id, new_sp, new_sa).await?;
    check_no_escalation(&caller_at_new, &new_perms).map_err(ApiError::Auth)?;

    // If this grant currently carries org:manage and the edit drops it, the org
    // must retain another holder.
    let loses_org_manage = drops_org_manage(&old_perms, &new_perms);
    let leaves_org_scope = grant.scope_type == "org" && new_scope_type != "org";
    if loses_org_manage || (leaves_org_scope && old_perms.iter().any(|p| p == perm::ORG_MANAGE)) {
        let remaining =
            repo::count_org_manage_grants_excluding(&mut conn, org_id, grant_id).await?;
        if remaining == 0 {
            return Err(ApiError::Conflict(
                "cannot remove the last grant with org:manage — assign it to another member first"
                    .into(),
            ));
        }
    }

    let updated = repo::update_grant(
        &mut conn,
        grant_id,
        new_role_id,
        &new_scope_type,
        new_scope_id,
    )
    .await
    .map_err(|e| match e {
        diesel::result::Error::DatabaseError(
            diesel::result::DatabaseErrorKind::UniqueViolation,
            _,
        ) => ApiError::Conflict("this member already has that role at that scope".into()),
        other => ApiError::from(other),
    })?;

    Ok(Json(serde_json::json!({ "id": updated.id })))
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
    let role = repo::create_role(&mut conn, org_id, &req.name, &req.description, perms)
        .await
        .map_err(|e| match e {
            diesel::result::Error::DatabaseError(
                diesel::result::DatabaseErrorKind::UniqueViolation,
                _,
            ) => ApiError::Conflict("a role with that name already exists in this org".into()),
            other => ApiError::from(other),
        })?;
    Ok(Json(role))
}

#[derive(Deserialize)]
pub struct UpdateRoleReq {
    pub name: Option<String>,
    pub description: Option<String>,
    pub permissions: Option<Vec<String>>,
}

/// Edit a role this org owns.
///
/// Presets are refused: `ensure_preset_roles` re-syncs them from rbac.rs at
/// every API boot, so an edit would silently revert on the next restart —
/// worse than not offering it.
pub async fn update_role_handler(
    auth: AuthUser,
    State(state): State<AppState>,
    Path((org_id, role_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<UpdateRoleReq>,
) -> Result<Json<Role>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_org(&mut conn, auth.user_id, org_id, perm::ROLE_MANAGE).await?;

    let role = repo::get_role(&mut conn, role_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    // Check presets first. Presets (org_id NULL) are listed to every org
    // member via repo::list_roles, so their existence is already public; a
    // clear "cannot be edited" refusal is correct here, not a 404.
    if role.is_system {
        return Err(ApiError::BadRequest("system roles cannot be edited".into()));
    }
    // Reached only for custom (non-system) roles. A role owned by another
    // org is not public, so returning NotFound rather than Forbidden avoids
    // confirming it exists.
    if role.org_id != Some(org_id) {
        return Err(ApiError::NotFound);
    }

    let name = req.name.clone().unwrap_or_else(|| role.name.clone());
    if name.trim().is_empty() {
        return Err(ApiError::BadRequest("role name is required".into()));
    }
    let description = req
        .description
        .clone()
        .unwrap_or_else(|| role.description.clone());

    let old_perms = role_permissions(&role.permissions);
    let new_perms = req.permissions.clone().unwrap_or_else(|| old_perms.clone());

    for p in &new_perms {
        if !perm::ALL.contains(&p.as_str()) {
            return Err(ApiError::BadRequest(format!("unknown permission: {p}")));
        }
    }

    // Both directions. Adding a permission you lack is escalation; removing one
    // you lack is sabotage — a Developer holding role:manage could otherwise
    // strip org:manage from the Admin role and disable everyone above them.
    let own = sauron_auth::effective_at_org(&mut conn, auth.user_id, org_id).await?;
    check_role_edit(&own, &old_perms, &new_perms).map_err(ApiError::Auth)?;

    // A role edit changes every holder's access at once. If this role is the
    // only source of org:manage in the org, dropping it orphans the org exactly
    // as deleting the last owner grant would.
    if drops_org_manage(&old_perms, &new_perms) {
        let remaining =
            repo::count_org_manage_grants_excluding_role(&mut conn, org_id, role_id).await?;
        if remaining == 0 {
            return Err(ApiError::Conflict(
                "this is the org's last role granting org:manage — grant it elsewhere first".into(),
            ));
        }
    }

    let perms = Value::Array(new_perms.iter().map(|p| Value::String(p.clone())).collect());
    let updated = repo::update_role(&mut conn, org_id, role_id, &name, &description, perms)
        .await
        .map_err(|e| match e {
            diesel::result::Error::DatabaseError(
                diesel::result::DatabaseErrorKind::UniqueViolation,
                _,
            ) => ApiError::Conflict("a role with that name already exists in this org".into()),
            other => ApiError::from(other),
        })?;
    Ok(Json(updated))
}
