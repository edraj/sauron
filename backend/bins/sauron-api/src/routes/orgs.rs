//! Organizations, membership (grants), roles, and the `/access` endpoint the
//! dashboard uses to gate its UI.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Path, State};
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use sauron_auth::guard::{
    check_no_escalation, check_no_escalation_at_scopes, check_role_edit, drops_org_manage,
    generate_temp_password, role_permissions, scope_parts, union_permissions, ResolvedScope,
};
use sauron_auth::hash_password_async;
use sauron_auth::rbac::{grants_from_rows, Scope};
use sauron_auth::{authorize_org, perm, AuthError, AuthUser};
use sauron_db::models::{NewRoleGrant, Organization, Role};
use sauron_db::repo;
use sauron_db::AsyncPgConnection;
use sauron_mail::MailKind;

use super::auth::{
    client_addr, rate_limit, render_password_reset_mail, reset_link, ResetMailVars, ResetMode,
    ADMIN_RESET_PER_CALLER_PER_HOUR, ADMIN_RESET_PER_TARGET_PER_HOUR, ADMIN_RESET_TTL_SECS,
};
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

    // The org's own creation is the first row of its trail — and, because
    // `audit_log.org_id` CASCADEs, the last one to survive its deletion.
    crate::audit::record(
        &mut conn,
        auth.user_id,
        crate::audit::Entry::new(
            org.id,
            crate::audit::action::ORG_CREATE,
            crate::audit::entity::ORG,
        )
        .target(org.id, &org.name)
        .changes(crate::audit::created(
            crate::audit::entity::ORG,
            &[("name", serde_json::json!(org.name))],
        )),
    )
    .await;
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
        sauron_auth::rbac::effective_permissions(&parsed, org_id, None, None, None)
            .into_iter()
            .collect();
    permissions.sort();
    let grants: Vec<GrantView> = parsed
        .into_iter()
        .map(|g| {
            let (scope_type, scope_id) = g.scope.parts();
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
    pub is_active: bool,
    /// Non-null while an admin-forced reset is outstanding on this account.
    ///
    /// `GET /v1/orgs/{org}/members` is the only place the dashboard learns
    /// anything about a member's account state, and without this field the
    /// cancel action exists on the server and is unreachable from the UI —
    /// which is the same as not existing, since the admin who needs it is
    /// looking at a members table, not at `curl`.
    pub credentials_invalidated_at: Option<DateTime<Utc>>,
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
        .map(
            |(g, email, name, role_name, is_active, credentials_invalidated_at)| MemberGrant {
                id: g.id,
                user_id: g.user_id,
                email,
                name,
                role_id: g.role_id,
                role_name,
                scope_type: g.scope_type,
                scope_id: g.scope_id,
                is_active,
                credentials_invalidated_at,
            },
        )
        .collect();
    Ok(Json(members))
}

/// One scope target in a grant request.
#[derive(Deserialize, Clone)]
pub struct ScopeRef {
    pub scope_type: String,
    pub scope_id: Uuid,
}

/// Largest batch one grant request may carry.
const MAX_SCOPES: usize = 200;

#[derive(Deserialize)]
pub struct CreateGrantReq {
    pub email: String,
    pub role_id: Uuid,
    #[serde(default)]
    pub scopes: Vec<ScopeRef>,
    #[serde(default)]
    pub scope_type: Option<String>,
    #[serde(default)]
    pub scope_id: Option<Uuid>,
}

/// Collapse the batch and legacy singular request shapes into one scope list.
///
/// The singular `scope_type`/`scope_id` pair is still accepted because the RPM
/// ships the api and the dashboard as separate subpackages: after a partial
/// upgrade an older dashboard still posts the old shape, and refusing it would
/// 422 every member creation until both halves are updated.
///
/// Exact duplicate pairs are dropped, first-seen order preserved. That is
/// load-bearing rather than tidiness: a repeat trips `role_grants`' UNIQUE key,
/// which `create_member`'s UniqueViolation arm would then report as "a user
/// with that email already exists".
fn normalize_scopes(
    scopes: &[ScopeRef],
    scope_type: Option<&String>,
    scope_id: Option<Uuid>,
) -> Result<Vec<ScopeRef>, ApiError> {
    let requested: Vec<ScopeRef> = if scopes.is_empty() {
        match (scope_type, scope_id) {
            (Some(t), Some(id)) => vec![ScopeRef {
                scope_type: t.clone(),
                scope_id: id,
            }],
            _ => Vec::new(),
        }
    } else {
        scopes.to_vec()
    };

    if requested.is_empty() {
        return Err(ApiError::BadRequest(
            "at least one scope is required".into(),
        ));
    }
    if requested.len() > MAX_SCOPES {
        return Err(ApiError::BadRequest(format!(
            "too many scopes (max {MAX_SCOPES})"
        )));
    }

    let mut seen: HashSet<(String, Uuid)> = HashSet::with_capacity(requested.len());
    let mut out = Vec::with_capacity(requested.len());
    for s in requested {
        if !matches!(s.scope_type.as_str(), "org" | "project" | "app" | "env") {
            return Err(ApiError::BadRequest("invalid scope_type".into()));
        }
        if seen.insert((s.scope_type.clone(), s.scope_id)) {
            out.push(s);
        }
    }
    Ok(out)
}

/// Validate a batch of scope targets against `org_id`, resolving each app's
/// parent project. At most two queries regardless of batch size.
///
/// This is the cross-tenant boundary for grants: `scope_id` carries no foreign
/// key, so nothing in the database stops a grant pointing at another tenant's
/// project or app. One implementation, shared by every handler that accepts
/// caller-supplied scopes.
async fn validate_scopes_in_org(
    conn: &mut sauron_db::AsyncPgConnection,
    org_id: Uuid,
    scopes: &[ScopeRef],
) -> Result<Vec<ResolvedScope>, ApiError> {
    let not_in_org = || ApiError::BadRequest("scope target is not in this org".into());

    let project_ids: Vec<Uuid> = scopes
        .iter()
        .filter(|s| s.scope_type == "project")
        .map(|s| s.scope_id)
        .collect();
    let app_ids: Vec<Uuid> = scopes
        .iter()
        .filter(|s| s.scope_type == "app")
        .map(|s| s.scope_id)
        .collect();
    let env_ids: Vec<Uuid> = scopes
        .iter()
        .filter(|s| s.scope_type == "env")
        .map(|s| s.scope_id)
        .collect();

    let projects_here: HashSet<Uuid> = if project_ids.is_empty() {
        HashSet::new()
    } else {
        repo::projects_in_org(conn, org_id, &project_ids)
            .await?
            .into_iter()
            .collect()
    };
    // Apps are matched through their project's org, so an app whose ancestry
    // lands in another tenant simply never enters the map and is refused below.
    let app_parents: HashMap<Uuid, Uuid> = if app_ids.is_empty() {
        HashMap::new()
    } else {
        repo::app_ancestries(conn, &app_ids)
            .await?
            .into_iter()
            .filter(|(_, _, owner_org)| *owner_org == org_id)
            .map(|(app_id, project_id, _)| (app_id, project_id))
            .collect()
    };
    // Envs are matched through their app's project's org, exactly like apps
    // above — `scope_id` has no foreign key, so this filter is the only thing
    // stopping a caller-supplied env id from another tenant from resolving
    // here at all. One batched query regardless of how many env scopes the
    // request carries, keeping the documented two-query budget (one for
    // apps, one for envs).
    let env_parents: HashMap<Uuid, (Uuid, Uuid)> = if env_ids.is_empty() {
        HashMap::new()
    } else {
        repo::env_ancestries(conn, &env_ids)
            .await?
            .into_iter()
            .filter(|(_, _, _, owner_org)| *owner_org == org_id)
            .map(|(env_id, app_id, project_id, _)| (env_id, (project_id, app_id)))
            .collect()
    };

    scopes
        .iter()
        .map(|s| match s.scope_type.as_str() {
            "org" => {
                if s.scope_id != org_id {
                    return Err(not_in_org());
                }
                Ok(ResolvedScope {
                    scope: Scope::Org(s.scope_id),
                    project_of_app: None,
                    app_of_env: None,
                })
            }
            "project" => {
                if !projects_here.contains(&s.scope_id) {
                    return Err(not_in_org());
                }
                Ok(ResolvedScope {
                    scope: Scope::Project(s.scope_id),
                    project_of_app: None,
                    app_of_env: None,
                })
            }
            "app" => match app_parents.get(&s.scope_id) {
                Some(project_id) => Ok(ResolvedScope {
                    scope: Scope::App(s.scope_id),
                    project_of_app: Some(*project_id),
                    app_of_env: None,
                }),
                None => Err(not_in_org()),
            },
            "env" => match env_parents.get(&s.scope_id) {
                Some((project_id, app_id)) => Ok(ResolvedScope {
                    scope: Scope::Env(s.scope_id),
                    project_of_app: Some(*project_id),
                    app_of_env: Some(*app_id),
                }),
                None => Err(not_in_org()),
            },
            _ => Err(ApiError::BadRequest("invalid scope_type".into())),
        })
        .collect()
}

/// Refuse the batch unless the caller outranks `role_perms` at **every** scope.
///
/// The caller's grants are read once and evaluated per scope in memory:
/// `effective_at` reloads them on every call, which would turn a 200-scope
/// request into 200 round-trips. Collapsing this to one org-level check would
/// be a silent widening — granting at project A and at project B are
/// independent authorization decisions.
async fn check_batch_escalation(
    conn: &mut sauron_db::AsyncPgConnection,
    caller_id: Uuid,
    org_id: Uuid,
    scopes: &[ResolvedScope],
    role_perms: &[String],
) -> Result<(), ApiError> {
    let caller = grants_from_rows(repo::user_grants_in_org(conn, caller_id, org_id).await?);
    check_no_escalation_at_scopes(&caller, org_id, scopes, role_perms).map_err(|scope| {
        // Name the refused scope. A batch can carry many, and a bare "forbidden"
        // leaves the admin re-ticking boxes to find the one that failed. It
        // discloses nothing: the caller already cleared `authorize_org` and
        // picked this scope out of a tree we rendered for them.
        let (scope_type, scope_id) = scope.parts();
        tracing::warn!(
            user_id = %caller_id,
            %org_id,
            scope_type,
            %scope_id,
            "refusing grant: caller lacks the role's permissions at this scope"
        );
        ApiError::Forbidden(format!(
            "you do not hold every permission in that role on one of the selected scopes \
             ({scope_type} {scope_id}) — choose a narrower role, or deselect that scope"
        ))
    })
}

/// Validate that a scope target belongs to `org_id`, returning
/// `(project_of_app, app_of_env)` — the ancestry `scope_parts` needs — for
/// the scope types that have any.
///
/// This is the cross-tenant boundary for grants: without it a caller could
/// name a project, app, or env in someone else's org and have a grant
/// created against it. The single-scope counterpart of
/// `validate_scopes_in_org`, for the handlers that edit one grant at a time.
async fn validate_scope_in_org(
    conn: &mut sauron_db::AsyncPgConnection,
    org_id: Uuid,
    scope_type: &str,
    scope_id: Uuid,
) -> Result<(Option<Uuid>, Option<Uuid>), ApiError> {
    let not_in_org = || ApiError::BadRequest("scope target is not in this org".into());
    match scope_type {
        "org" => {
            if scope_id != org_id {
                return Err(not_in_org());
            }
            Ok((None, None))
        }
        "project" => {
            if repo::project_org(conn, scope_id).await? != Some(org_id) {
                return Err(not_in_org());
            }
            Ok((None, None))
        }
        "app" => match repo::app_ancestry(conn, scope_id).await? {
            Some((project_id, o)) if o == org_id => Ok((Some(project_id), None)),
            _ => Err(not_in_org()),
        },
        "env" => match repo::env_ancestry(conn, scope_id).await? {
            Some((app_id, project_id, o)) if o == org_id => Ok((Some(project_id), Some(app_id))),
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

    let scopes = normalize_scopes(&req.scopes, req.scope_type.as_ref(), req.scope_id)?;

    // Target user must already exist.
    let user = repo::find_user_by_email(&mut conn, &req.email)
        .await?
        .ok_or_else(|| {
            ApiError::BadRequest("no user with that email (ask them to sign up)".into())
        })?;

    // A deactivated account cannot log in, so the grant would do nothing — and
    // worse, it would strand the account. `set_member_active` refuses to touch
    // anyone holding grants outside the org, in both directions, so adding a
    // second org to a deactivated user makes them un-reactivatable from either
    // one; recovery would need direct database access. Keeping "a deactivated
    // user's set of orgs never grows" true here is what keeps that guard
    // reversible.
    if !user.is_active {
        return Err(ApiError::Conflict(
            "that account is deactivated — reactivate it where it is already a member, then grant access here"
                .into(),
        ));
    }

    // The same rule as the one above, generalised from deactivated accounts to
    // every account: this endpoint never leaves anyone belonging to two orgs.
    //
    // A grant is how you join an org, and the lookup above is by email with no
    // org filter, so this handler reaches every account in the deployment.
    // There is no invitation or consent step anywhere in the stack, and
    // `/v1/auth/register` is open — so without this check any stranger can
    // register, become Owner of their own fresh org, and unilaterally attach a
    // named person in someone else's tenancy to it.
    //
    // The damage is not that the stranger gains anything; it is what the victim
    // loses. `guard_member_admin_action` refuses to deactivate, force-logout or
    // force-reset any member holding a grant outside the org — deliberately and
    // unwaivably, because `member:manage` is org-scoped while an account is
    // global. A planted grant therefore disables all three incident-response
    // verbs for that person *in their real org*, and neither side can undo it:
    // `delete_grant` authorises against the grant's own org, so the victim's
    // admins get 403 and the victim (a Viewer there) has no standing either.
    // Turning that blast-radius guard into a stranger's lever is the whole
    // attack, and it costs zero privilege to mount.
    //
    // Scoped to *creating* the cross-org state, not to multi-org membership as
    // such: adding another scope to someone already a member here is untouched,
    // and an account holding no grants at all can still be attached. What it
    // does cost is that a genuinely multi-org human — a consultant, or the
    // deployment admin who holds `org:manage` in every org — can no longer be
    // added by the org that wants them; they must create the org themselves via
    // `POST /v1/orgs`, which self-grants Owner. The correct answer is an
    // invitation the target accepts, which is a feature and a migration, not a
    // guard, and this is the stopgap that makes the deployment safe meanwhile.
    let already_a_member = !repo::user_grants_in_org(&mut conn, user.id, org_id)
        .await?
        .is_empty();
    if !already_a_member
        && repo::count_user_grants_outside_org(&mut conn, user.id, org_id).await? > 0
    {
        return Err(ApiError::Conflict(
            "that account already belongs to another organization and cannot be added from here"
                .into(),
        ));
    }

    // One role for the whole batch, so it is loaded and checked once: it must
    // be a preset or belong to this org.
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

    // Every scope target must belong to this org (prevents cross-org grants).
    // Also resolves each app's parent project for the escalation check below.
    let resolved = validate_scopes_in_org(&mut conn, org_id, &scopes).await?;

    // No privilege escalation: the granter must themselves hold every permission
    // the granted role confers, at every scope in the batch. (Stops an Admin from
    // granting Owner to gain org:manage.)
    let role_perms = role_permissions(&role.permissions);
    check_batch_escalation(&mut conn, auth.user_id, org_id, &resolved, &role_perms).await?;

    let rows: Vec<NewRoleGrant> = scopes
        .into_iter()
        .map(|s| NewRoleGrant {
            org_id,
            user_id: user.id,
            role_id: req.role_id,
            scope_type: s.scope_type,
            scope_id: s.scope_id,
        })
        .collect();

    // Same convention as update_grant_handler: re-granting a role a member
    // already holds at that scope is a conflict the user can act on, not a 500.
    let ids = repo::create_grants(&mut conn, rows)
        .await
        .map_err(|e| match e {
            diesel::result::Error::DatabaseError(
                diesel::result::DatabaseErrorKind::UniqueViolation,
                _,
            ) => ApiError::Conflict("this member already has that role at that scope".into()),
            other => ApiError::from(other),
        })?;

    // `id` repeats the first id purely so a dashboard build older than this api
    // keeps working after a partial RPM upgrade — they are separate subpackages.
    // The target is the PERSON, not the grant row: an administrator asks "what
    // was this member given", and `resolved` may hold several grants from this
    // one call. `permissions` records what the role conferred at grant time, so
    // a later edit to that role cannot silently rewrite history.
    crate::audit::record(
        &mut conn,
        auth.user_id,
        crate::audit::Entry::new(
            org_id,
            crate::audit::action::GRANT_CREATE,
            crate::audit::entity::GRANT,
        )
        .target(user.id, &user.email)
        .changes(crate::audit::created(
            crate::audit::entity::GRANT,
            &[
                ("role_id", serde_json::json!(role.id)),
                ("role_name", serde_json::json!(role.name)),
                ("permissions", serde_json::json!(role_perms)),
                (
                    "scopes",
                    serde_json::json!(resolved
                        .iter()
                        .map(|s| {
                            // `Scope::parts` is the same (scope_type, scope_id)
                            // spelling `role_grants` stores, so a reader can
                            // match an entry against the grants table directly.
                            let (scope_type, scope_id) = s.scope.parts();
                            serde_json::json!({
                                "scope_type": scope_type,
                                "scope_id": scope_id,
                            })
                        })
                        .collect::<Vec<_>>()),
                ),
            ],
        )),
    )
    .await;

    Ok(Json(serde_json::json!({ "ids": ids, "id": ids.first() })))
}

#[derive(Deserialize)]
pub struct CreateMemberReq {
    pub email: String,
    #[serde(default)]
    pub name: String,
    pub role_id: Uuid,
    #[serde(default)]
    pub scopes: Vec<ScopeRef>,
    #[serde(default)]
    pub scope_type: Option<String>,
    #[serde(default)]
    pub scope_id: Option<Uuid>,
}

/// Create a user account and its initial grants in one step.
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

    let scopes = normalize_scopes(&req.scopes, req.scope_type.as_ref(), req.scope_id)?;

    if !req.email.contains('@') {
        return Err(ApiError::BadRequest("a valid email is required".into()));
    }
    if repo::find_user_by_email(&mut conn, &req.email)
        .await?
        .is_some()
    {
        return Err(ApiError::Conflict(
            "a user with that email already exists — use Grant access instead".into(),
        ));
    }

    // One role for the whole batch, so it is loaded and checked once: it must
    // be a preset or belong to this org.
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

    // Every scope target must belong to this org, and each app's parent project
    // is what the escalation check needs. Shared helper — the org-containment
    // check has one implementation, not one per handler.
    let resolved = validate_scopes_in_org(&mut conn, org_id, &scopes).await?;

    // Creating a user must not be a way around the grant escalation check, at
    // any of the scopes the new account is being handed.
    let role_perms = role_permissions(&role.permissions);
    check_batch_escalation(&mut conn, auth.user_id, org_id, &resolved, &role_perms).await?;

    let temp_password = generate_temp_password();
    let hash = hash_password_async(temp_password.clone())
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    // Parallel arrays, same length: the repo unnests them together, and a
    // shorter one would pad the other with NULLs and fail role_grants' NOT NULL.
    let scope_types: Vec<String> = scopes.iter().map(|s| s.scope_type.clone()).collect();
    let scope_ids: Vec<Uuid> = scopes.iter().map(|s| s.scope_id).collect();

    // One statement, atomic: a grant failure must not leave an account that
    // holds the email but has no access and appears in no list.
    let created = repo::create_member_with_grants(
        &mut conn,
        &req.email,
        &hash,
        &req.name,
        org_id,
        req.role_id,
        &scope_types,
        &scope_ids,
    )
    .await
    .map_err(|e| match e {
        // Unambiguous because normalize_scopes de-duplicated the pairs: the only
        // unique key a well-formed batch can trip is the one on the email.
        diesel::result::Error::DatabaseError(
            diesel::result::DatabaseErrorKind::UniqueViolation,
            _,
        ) => ApiError::Conflict(
            "a user with that email already exists — use Grant access instead".into(),
        ),
        other => ApiError::from(other),
    })?;

    let user_id = created
        .first()
        .map(|row| row.user_id)
        .ok_or_else(|| ApiError::Internal("member insert returned no grants".into()))?;
    let grant_ids: Vec<Uuid> = created.iter().map(|row| row.grant_id).collect();

    // The generated temp password is NOT recorded — it is a live credential,
    // and `audit::created`'s allowlist has no key that could carry it. What is
    // worth knowing is that an account was created, for whom, and with how
    // many scopes; the grants themselves are individually auditable through
    // `grant.create`.
    crate::audit::record(
        &mut conn,
        auth.user_id,
        crate::audit::Entry::new(
            org_id,
            crate::audit::action::MEMBER_CREATE,
            crate::audit::entity::MEMBER,
        )
        .target(user_id, &req.email)
        .changes(crate::audit::created(
            crate::audit::entity::MEMBER,
            &[
                ("email", serde_json::json!(req.email)),
                ("name", serde_json::json!(req.name)),
            ],
        )),
    )
    .await;

    // `grant_id` repeats the first id purely so a dashboard build older than
    // this api keeps working after a partial RPM upgrade.
    Ok(Json(serde_json::json!({
        "user_id": user_id,
        "grant_ids": grant_ids,
        "grant_id": grant_ids.first(),
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
    let (project_of_app, app_of_env) = match grant.scope_type.as_str() {
        "app" => (
            repo::app_ancestry(&mut conn, grant.scope_id)
                .await?
                .map(|(project_id, _)| project_id),
            None,
        ),
        "env" => match repo::env_ancestry(&mut conn, grant.scope_id).await? {
            Some((app_id, project_id, _)) => (Some(project_id), Some(app_id)),
            None => (None, None),
        },
        _ => (None, None),
    };
    // `effective_at` takes (project, app) only — it has no env parameter (see
    // its doc comment) — so an env grant is evaluated at its parent app, the
    // most precise level available here. That still correctly recognizes an
    // org/project/app-scoped remover, which is the case this guard exists
    // for; the one gap is a remover who holds the required permission *only*
    // on this exact environment, who would need it at the parent app too.
    let (scope_project, scope_app, _scope_env) = scope_parts(
        &grant.scope_type,
        grant.scope_id,
        project_of_app,
        app_of_env,
    );
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
    // A daily sweep alone leaves a 24-hour window in which a revoked member
    // keeps receiving telemetry by email. Run it here, after the grant change
    // has committed, for the paths a human actually takes.
    if let Err(e) =
        sauron_alerts::sweep::sweep_user_subscriptions(&mut conn, grant.user_id, org_id).await
    {
        tracing::warn!(error = ?e, "notification subscription sweep failed after grant delete");
    }

    let target_email = repo::user_email(&mut conn, grant.user_id)
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    crate::audit::record(
        &mut conn,
        auth.user_id,
        crate::audit::Entry::new(
            org_id,
            crate::audit::action::GRANT_DELETE,
            crate::audit::entity::GRANT,
        )
        .target(grant.user_id, &target_email)
        .changes(crate::audit::diff(
            crate::audit::entity::GRANT,
            &[
                (
                    "scope_type",
                    serde_json::json!(grant.scope_type),
                    serde_json::Value::Null,
                ),
                (
                    "scope_id",
                    serde_json::json!(grant.scope_id),
                    serde_json::Value::Null,
                ),
                (
                    "permissions",
                    serde_json::json!(role_perms),
                    serde_json::Value::Null,
                ),
            ],
        )),
    )
    .await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct SetMemberActiveReq {
    pub is_active: bool,
}

/// The guard stack every destructive admin action against another member's
/// *account* must pass, in order, before it touches anything.
///
/// Returns the target's grant rows in this org so callers do not re-query — the
/// same rows the escalation check reads, which is why the membership test and
/// the escalation input are one query rather than two.
///
/// Exactly one of the six guards is waivable, because exactly one caller
/// genuinely differs: `set_member_active` passes `allow_self: true` and keeps
/// its own narrower 409, because self-*reactivation* is legal there and the
/// refusal it does need ("you cannot deactivate your own account") is not the
/// sentence below. `allow_self` stays a parameter rather than a hard-coded
/// refusal because self-target is an *ergonomic* rule about which surface owns a
/// verb, and a future admin action may legitimately want the other answer. The
/// cross-org refusal is not a parameter: that is a blast-radius boundary, and a
/// flag there is an invitation — the next slice wanting the easy answer sets it
/// to `true` and the refusal quietly stops applying to the account it most
/// protects.
///
/// The last-`org:manage` guard deliberately stays **outside** this helper. That
/// concern is specific to deactivation: it is irreversible without an admin,
/// whereas a forced logout is reversible by the victim simply logging in again
/// and so cannot orphan an org.
async fn guard_member_admin_action(
    conn: &mut AsyncPgConnection,
    caller_id: Uuid,
    org_id: Uuid,
    target_user_id: Uuid,
    allow_self: bool,
) -> Result<Vec<(String, Uuid, Value)>, ApiError> {
    // Org-scoped by construction, so a project-scoped Admin cannot reach it.
    authorize_org(conn, caller_id, org_id, perm::MEMBER_MANAGE).await?;

    let _user = repo::get_user(conn, target_user_id)
        .await?
        .ok_or(ApiError::NotFound)?;

    // The target must actually be a member of this org, or any admin could act
    // on any account in the deployment by guessing a uuid. The rows are also
    // what the escalation check below reads, so this is one query, not two.
    let target_grants = repo::user_grants_in_org(conn, target_user_id, org_id).await?;
    if target_grants.is_empty() {
        return Err(ApiError::NotFound);
    }

    // Refused before anything else so it always gets the explanatory 409 rather
    // than tripping one of the general guards below.
    if !allow_self && target_user_id == caller_id {
        return Err(ApiError::Conflict(
            "use your account page to manage your own sessions".into(),
        ));
    }

    // You may not act on someone who outranks you — the same rule delete_grant
    // and update_grant_handler already apply to a single grant, and this is
    // strictly more severe than either: it reaches the whole account rather than
    // one scope. Without it an Admin (member:manage, no org:manage) could work
    // through every Owner in turn.
    //
    // The target's side is the union over every grant they hold here, not their
    // org-scoped subset, because the account is not scoped either. The caller's
    // side is deliberately their *org*-scope permissions: an account-global act
    // takes org-level standing, which a project grant does not confer.
    let target_perms = union_permissions(&grants_from_rows(target_grants.clone()));
    let caller = sauron_auth::effective_at_org(conn, caller_id, org_id).await?;
    check_no_escalation(&caller, &target_perms).map_err(ApiError::Auth)?;

    // member:manage is org-scoped; the account is global. An org-A admin acting
    // on a member who is also an org-B Owner is reaching outside their blast
    // radius, and no caller of this helper has a reason to.
    //
    // Because the refusal is unwaivable, whoever can *create* the multi-org
    // state decides who gets locked out of it, which is why `create_grant`
    // refuses to attach an account that already belongs elsewhere. The state is
    // still reachable — self-created orgs (`POST /v1/orgs` self-grants Owner),
    // rows predating that check, direct database writes — so this stays a real
    // guard rather than dead code, but no stranger can arrange it any more.
    if repo::count_user_grants_outside_org(conn, target_user_id, org_id).await? > 0 {
        return Err(ApiError::Conflict(
            "this member belongs to another organization and cannot be administered from here"
                .into(),
        ));
    }

    Ok(target_grants)
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

    // `allow_self: true`, and the self-check stays BELOW this call. Both halves
    // are load-bearing. The self-check this endpoint has always carried is
    // guarded by `!req.is_active`, so self-REACTIVATION succeeds today; passing
    // `false` here would refuse it with the helper's generic "use your account
    // page to manage your own sessions", which is not advice about a
    // reactivation and which `Members.svelte`'s `toggleActive` prints verbatim.
    // And hoisting the self-check above this call would answer a caller holding
    // no `member:manage` in this org with 409 instead of 403 -- deciding
    // something about the target before authorizing the caller at all.
    let _target_grants =
        guard_member_admin_action(&mut conn, auth.user_id, org_id, user_id, true).await?;

    // Self-deactivation gets its own 409 rather than the helper's generic one,
    // because the honest advice differs: there is no "manage your own sessions"
    // answer to "I tried to disable my own login".
    if !req.is_active && user_id == auth.user_id {
        return Err(ApiError::Conflict(
            "you cannot deactivate your own account".into(),
        ));
    }

    if !req.is_active {
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
        // Session-aware, not token-only. `AuthUser` reads claims, not
        // `users.is_active`, so a token-only revoke leaves the deactivated
        // member with full API access for up to 900 seconds — making the most
        // severe admin action the weakest one, next to a reversible "Sign out"
        // in the same UI that takes effect in about five. It would also leave
        // their `auth_sessions` rows live for up to 30 days, so their own
        // session list would report devices that cannot actually refresh.
        let revoked = repo::revoke_sessions_for_user(
            &mut conn,
            user_id,
            None,
            repo::REVOKE_DEACTIVATED,
            Some(auth.user_id),
        )
        .await?;
        state.revocations.mark_revoked(&revoked);
    }
    // A deactivated member's QUEUED mask actions must not execute. Confirm
    // re-authorizes, but the action then sits in `pending` — with one slot per
    // worker and a 200 ms inter-batch pause, a backlog can be hours deep — and
    // deactivation revokes refresh tokens while touching nothing queued. The
    // worker re-checks authorization at claim too; this is the fast path so the
    // action never runs at all.
    if !req.is_active {
        let cancelled = repo::cancel_pending_mask_actions_for_user(&mut conn, user_id).await?;
        if cancelled > 0 {
            tracing::info!(
                user_id = %user_id,
                cancelled,
                "cancelled queued PII mask actions for a deactivated member"
            );
        }
    }
    // A daily sweep alone leaves a 24-hour window in which a revoked member
    // keeps receiving telemetry by email. Run it here, after the grant change
    // has committed, for the paths a human actually takes.
    if let Err(e) = sauron_alerts::sweep::sweep_user_subscriptions(&mut conn, user_id, org_id).await
    {
        tracing::warn!(
            error = ?e,
            "notification subscription sweep failed after member active change"
        );
    }

    // Two distinct actions rather than one `member.update` with a boolean in
    // the diff: deactivation is a security event that an administrator filters
    // for by name, and burying it inside a generic update would mean finding it
    // required reading every entry's diff.
    let target_email = repo::user_email(&mut conn, user_id)
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    crate::audit::record(
        &mut conn,
        auth.user_id,
        crate::audit::Entry::new(
            org_id,
            if req.is_active {
                crate::audit::action::MEMBER_ACTIVATE
            } else {
                crate::audit::action::MEMBER_DEACTIVATE
            },
            crate::audit::entity::MEMBER,
        )
        .target(user_id, &target_email)
        .changes(crate::audit::diff(
            crate::audit::entity::MEMBER,
            &[(
                "is_active",
                serde_json::json!(!req.is_active),
                serde_json::json!(req.is_active),
            )],
        )),
    )
    .await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// Sign a member out of every device.
///
/// Gated on **both** `member:credential` (checked here, first) and
/// `member:manage` (re-checked inside the shared guard stack). That is the
/// carve-out working as intended: `member:credential` narrows `member:manage`,
/// it does not stand in for it, and a role that can end a member's sessions
/// without otherwise being able to see or administer that member is not a shape
/// anyone asked for.
///
/// Deliberately omits the last-`org:manage` guard `set_member_active` carries:
/// deactivation is irreversible without an admin, whereas a forced logout is
/// reversible by the victim simply logging in again, so it cannot orphan an org.
///
/// Does **not** set `must_change_password` — "force login" is not "force
/// password reset", and `repo::set_user_password` clears that flag
/// unconditionally anyway — and does not touch `is_active`.
///
/// `allow_self` is `false`: this endpoint passes `except: None`, so a
/// self-target would log the admin out of the page they are standing on. "Sign
/// out my other devices" is a different verb, lives on `/account`, and spares
/// the current session.
pub async fn revoke_member_sessions(
    auth: AuthUser,
    State(state): State<AppState>,
    Path((org_id, user_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_org(&mut conn, auth.user_id, org_id, perm::MEMBER_CREDENTIAL).await?;
    let _target_grants =
        guard_member_admin_action(&mut conn, auth.user_id, org_id, user_id, false).await?;

    let ids = repo::revoke_sessions_for_user(
        &mut conn,
        user_id,
        None,
        repo::REVOKE_ADMIN,
        Some(auth.user_id),
    )
    .await?;

    // Before `drop(conn)` — the connection is released below and re-acquiring
    // one just to log would be a second pool checkout on a path that already
    // holds the answer.
    let target_email = repo::user_email(&mut conn, user_id)
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    crate::audit::record(
        &mut conn,
        auth.user_id,
        crate::audit::Entry::new(
            org_id,
            crate::audit::action::MEMBER_REVOKE_SESSIONS,
            crate::audit::entity::MEMBER,
        )
        .target(user_id, &target_email)
        .changes(crate::audit::created(
            crate::audit::entity::MEMBER,
            &[("revoked_sessions", serde_json::json!(ids.len()))],
        )),
    )
    .await;
    drop(conn);

    state.revocations.mark_revoked(&ids);
    tracing::warn!(
        actor = %auth.user_id,
        %user_id,
        %org_id,
        revoked = ids.len(),
        "admin revoked all sessions for a member"
    );
    Ok(Json(
        serde_json::json!({ "ok": true, "revoked": ids.len() }),
    ))
}

#[derive(Deserialize)]
pub struct ResetMemberPasswordReq {
    /// `"reset"` or `"cancel"`. The default is the forward action; an
    /// unrecognised value is a 400, never a silent reset.
    ///
    /// This `#[serde(default)]` only covers `{}` — a body that parses but omits
    /// the key. It does **not** cover a body-less `POST`, because `Json`
    /// rejects that before serde is ever called. The handler takes
    /// `Option<Json<…>>` for that case; see its signature.
    #[serde(default = "default_reset_action")]
    pub action: String,
}

fn default_reset_action() -> String {
    "reset".to_string()
}

impl Default for ResetMemberPasswordReq {
    fn default() -> Self {
        Self {
            action: default_reset_action(),
        }
    }
}

/// Force a password reset on a member, or cancel one already forced.
///
/// `reset` is destructive and says so: the target's current password stops
/// authenticating at the login form, every session ends within a few seconds,
/// and the emailed link is the only way back in. There is deliberately no
/// second, non-destructive "just send them a link" mode — shipping both puts an
/// admin holding a suspected leak in front of two adjacent buttons, one of
/// which stops the leaked password and one of which looks like it does.
///
/// `cancel` is the undo. It exists because this action is destructive *and*
/// gated on a mail relay the deployment may have misconfigured; without an undo
/// that does not itself depend on the relay, one bounced message is an account
/// nobody can reach.
///
/// There is deliberately no last-`org:manage` guard: a forced reset removes
/// nobody's permission — the target regains their account by using the link —
/// so an org can never be orphaned by it.
pub async fn reset_member_password(
    auth: AuthUser,
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: axum::http::HeaderMap,
    Path((org_id, user_id)): Path<(Uuid, Uuid)>,
    // `Option<Json<…>>`, not `Json<…>`, and that is the whole reason the
    // body-less `curl -X POST` documented above works. A bare `Json` extractor
    // rejects a request with no `content-type: application/json` with 415 and an
    // empty body with 400 — both *before* serde runs, so `#[serde(default)]`
    // never gets a chance. axum 0.8's `OptionalFromRequest for Json` hands back
    // `Ok(None)` when the header is absent, which is exactly the shape an
    // operator's `curl` sends. Body-consuming extractors must stay last.
    req: Option<Json<ResetMemberPasswordReq>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let req = req.map(|Json(r)| r).unwrap_or_default();

    let mut conn = db(&state).await?;
    // `member:credential` in ADDITION to `member:manage`, which
    // `guard_member_admin_action` demands as its first step. `member:manage` is
    // the routine permission for handing out and revoking grants; forcing a
    // reset combined with control of the mail relay is a path to account
    // takeover, and an org that hands out the former has not agreed to the
    // latter. The narrower permission never stands in for the broader one.
    authorize_org(&mut conn, auth.user_id, org_id, perm::MEMBER_CREDENTIAL).await?;

    let cancel = match req.action.as_str() {
        "reset" => false,
        "cancel" => true,
        _ => {
            return Err(ApiError::BadRequest(
                "action must be \"reset\" or \"cancel\"".into(),
            ))
        }
    };

    // Resolved BEFORE anything is applied, and that ordering is the whole
    // guarantee: a destructive change must never land when the message carrying
    // its remedy cannot be sent. `cancel` is deliberately exempt — gating the
    // undo on the same configuration that motivates it would make it
    // unreachable in precisely the deployment that needs it. The response never
    // carries the token or the link under any condition: that link is an
    // account-takeover primitive, and `member:credential` lets its holder deny
    // a member their account, not sign in as them.
    let mail_and_url = if cancel {
        None
    } else {
        let mail = state.mail.as_ref().cloned().ok_or_else(|| {
            ApiError::Unavailable(
                "unavailable",
                "SMTP is not configured on this server".into(),
            )
        })?;
        let url = state
            .cfg
            .require_dashboard_url()
            .map_err(|e| ApiError::Unavailable("unavailable", e.to_string()))?
            .to_string();
        Some((mail, url))
    };

    // `member:credential` is in the Admin preset, not just Owner, and an
    // unbounded loop here is an unbounded mail bomb aimed at one member's inbox
    // and an unbounded re-lock of an account somebody is trying to recover.
    rate_limit(
        &state,
        &format!("sauron:auth:adminreset:{}", auth.user_id),
        ADMIN_RESET_PER_CALLER_PER_HOUR,
        3600,
    )
    .await?;
    if !cancel {
        // `cancel` spends the per-caller bucket ONLY. It sends no mail and can
        // only ever restore access, so charging it to the per-target bucket
        // would mean an admin who forced five resets in an hour cannot undo the
        // fifth — a limiter blocking the remedy for the thing it was limiting.
        rate_limit(
            &state,
            &format!("sauron:auth:adminreset:target:{user_id}"),
            ADMIN_RESET_PER_TARGET_PER_HOUR,
            3600,
        )
        .await?;
    }

    // Carries the whole shared stack: `member:manage`, user-exists 404,
    // grant-in-this-org 404, self-target 409, no-escalation against the
    // target's full union with the caller's org-scope set, and the
    // unconditional cross-org refusal. `allow_self` is false: resetting
    // yourself is redundant (`/v1/auth/password` exists) and it lets an admin
    // lock themselves out over a relay they may have just broken, leaving
    // nobody with standing to cancel it. No local copy of any of those checks
    // is added here — a second copy of the cross-org rule is one more place for
    // the two to drift apart.
    let _target_grants =
        guard_member_admin_action(&mut conn, auth.user_id, org_id, user_id, false).await?;

    let user = repo::get_user(&mut conn, user_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    // Same spirit as `create_grant`'s refusal to grant to an inactive account:
    // a deactivated user's authority never grows.
    if !user.is_active {
        return Err(ApiError::Conflict(
            "reactivate this member before resetting their password".into(),
        ));
    }

    if cancel {
        repo::set_user_credentials_invalidated(&mut conn, user_id, None).await?;
        // Killing the outstanding links is the other half of a cancel. Leaving
        // them live means the mail everyone had written off can be delivered a
        // day later, and whoever opens it sets a password for an account whose
        // owner has been using their old one since — a second, unannounced
        // sign-out days after the incident was closed.
        //
        // `must_change_password` is deliberately NOT cleared. Cancelling
        // restores the ability to sign in; it does not pretend the admin never
        // had a reason. It may also have been set long before this reset, by
        // `create_member`'s reveal-once temp password, and cancel has no way to
        // tell the two apart.
        repo::invalidate_password_reset_tokens_for_user(
            &mut conn,
            user_id,
            repo::RESET_INVALIDATED_SUPERSEDED,
        )
        .await?;
        crate::audit::record(
            &mut conn,
            auth.user_id,
            crate::audit::Entry::new(
                org_id,
                crate::audit::action::MEMBER_RESET_PASSWORD,
                crate::audit::entity::MEMBER,
            )
            .target(user_id, &user.email)
            .changes(crate::audit::created(
                crate::audit::entity::MEMBER,
                &[("reset_action", serde_json::json!("cancel"))],
            )),
        )
        .await;
        return Ok(Json(serde_json::json!({
            "ok": true,
            "action": "cancel",
            "expires_at": serde_json::Value::Null,
        })));
    }

    let (mail, dashboard_url) = mail_and_url.expect("the reset branch always resolves mail config");

    // Gates before revoke, and fail-safe in that direction: `routes::auth::refresh`
    // re-reads `user.must_change_password` and bakes it into the next access
    // token, so even if the revocation write fails the target's next refresh
    // mints a gated token within one access-token lifetime. The reverse order
    // leaves a window with sessions killed and no gate.
    repo::set_user_must_change_password(&mut conn, user_id, true).await?;
    repo::set_user_credentials_invalidated(&mut conn, user_id, Some(Utc::now())).await?;

    // `actor` is the admin, which is the only way `auth_sessions.revoked_by`
    // ever records who forced the reset — `password_reset_tokens.initiated_by`
    // answers that for the link, but not for the sessions.
    let ids = repo::revoke_sessions_for_user(
        &mut conn,
        user_id,
        None,
        repo::REVOKE_RESET_FORCED,
        Some(auth.user_id),
    )
    .await?;
    // Turns the dialog's "within a few seconds" into a statement about this
    // replica rather than about its next poll.
    state.revocations.mark_revoked(&ids);

    // Unlike self-service, an admin trigger supersedes outstanding links: this
    // is an authoritative act by an identified principal, the admin means *this*
    // link now, and a re-issue after a bounce must not leave two live links.
    repo::invalidate_password_reset_tokens_for_user(
        &mut conn,
        user_id,
        repo::RESET_INVALIDATED_SUPERSEDED,
    )
    .await?;

    let raw = sauron_core::ids::opaque_token();
    let expires_at = Utc::now() + chrono::Duration::seconds(ADMIN_RESET_TTL_SECS);
    repo::insert_password_reset_token(
        &mut conn,
        user_id,
        sauron_auth::hash_token(&raw),
        sauron_auth::hash_token(&user.password_hash),
        expires_at,
        ResetMode::Admin.as_str(),
        Some(auth.user_id),
        // Populated here and not on the self-service path's behalf: self-service
        // rows only ever record an anonymous stranger's proxy address, so admin
        // rows are the half of the audit trail that matters.
        Some(client_addr(&headers, &peer, &state)),
    )
    .await?;

    let org = repo::get_org(&mut conn, org_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let org_name = org.name.clone();
    let display_name = if user.name.trim().is_empty() {
        user.email.clone()
    } else {
        user.name.clone()
    };
    let email = user.email.clone();

    // Recorded while the connection is still held, and before the mail is
    // enqueued: by this point the credential is already invalidated and every
    // session revoked, so the account is altered whether or not the mail ever
    // goes out. Auditing after the enqueue would lose exactly the case worth
    // investigating — a reset that took effect but never arrived. Neither the
    // token nor its hash is recorded; only that a reset was issued and when it
    // lapses.
    crate::audit::record(
        &mut conn,
        auth.user_id,
        crate::audit::Entry::new(
            org_id,
            crate::audit::action::MEMBER_RESET_PASSWORD,
            crate::audit::entity::MEMBER,
        )
        .target(user_id, &email)
        .changes(crate::audit::created(
            crate::audit::entity::MEMBER,
            &[
                ("reset_action", serde_json::json!("reset")),
                ("expires_at", serde_json::json!(expires_at)),
                ("revoked_sessions", serde_json::json!(ids.len())),
            ],
        )),
    )
    .await;

    // `MailSender` checks out its own pooled connection; see the identical drop
    // and its full reasoning in `routes::auth::forgot_password`.
    drop(conn);

    let content = render_password_reset_mail(ResetMailVars {
        mode: ResetMode::Admin,
        display_name: &display_name,
        reset_url: &reset_link(&dashboard_url, &raw),
        org_name: &org_name,
    })
    // Unreachable rather than merely unlikely, and that is what makes it safe to
    // discover this late: the only fallible step is `Cta::new` refusing a
    // non-http(s) href, and the precondition block above already took
    // `require_dashboard_url()`'s `Ok`, which is only ever returned for a URL
    // that starts with `http://` or `https://`. If that invariant is ever
    // broken, this must NOT become a 503: by here the account is already locked
    // and a 503 claims nothing was applied.
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    // The recipient is known here, so this path uses `enqueue` rather than
    // `enqueue_or_discard` — there is no branch to hide. The TTL passed here
    // becomes the mail row's own expires_at, so the message and the link it
    // carries die together.
    mail.enqueue(
        MailKind::PasswordReset,
        &email,
        &content,
        Some(user_id),
        std::time::Duration::from_secs(ADMIN_RESET_TTL_SECS as u64),
    )
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;

    Ok(Json(serde_json::json!({
        "ok": true,
        "action": "reset",
        "expires_at": expires_at.to_rfc3339(),
    })))
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

    if !matches!(new_scope_type.as_str(), "org" | "project" | "app" | "env") {
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
    let (new_project_of_app, new_app_of_env) =
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

    let (old_project_of_app, old_app_of_env) = match grant.scope_type.as_str() {
        "app" => (
            repo::app_ancestry(&mut conn, grant.scope_id)
                .await?
                .map(|(project_id, _)| project_id),
            None,
        ),
        "env" => match repo::env_ancestry(&mut conn, grant.scope_id).await? {
            Some((app_id, project_id, _)) => (Some(project_id), Some(app_id)),
            None => (None, None),
        },
        _ => (None, None),
    };
    // Same caveat as delete_grant: effective_at has no env parameter, so an
    // env-scoped grant (old or new) is evaluated at its parent app, the most
    // precise level available here.
    let (old_sp, old_sa, _old_se) = scope_parts(
        &grant.scope_type,
        grant.scope_id,
        old_project_of_app,
        old_app_of_env,
    );
    let caller_at_old =
        sauron_auth::effective_at(&mut conn, auth.user_id, org_id, old_sp, old_sa).await?;
    check_no_escalation(&caller_at_old, &old_perms).map_err(ApiError::Auth)?;

    let (new_sp, new_sa, _new_se) = scope_parts(
        &new_scope_type,
        new_scope_id,
        new_project_of_app,
        new_app_of_env,
    );
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

    // A daily sweep alone leaves a 24-hour window in which a revoked member
    // keeps receiving telemetry by email. Run it here, after the grant change
    // has committed, for the paths a human actually takes.
    if let Err(e) =
        sauron_alerts::sweep::sweep_user_subscriptions(&mut conn, grant.user_id, org_id).await
    {
        tracing::warn!(error = ?e, "notification subscription sweep failed after grant update");
    }

    let target_email = repo::user_email(&mut conn, grant.user_id)
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    crate::audit::record(
        &mut conn,
        auth.user_id,
        crate::audit::Entry::new(
            org_id,
            crate::audit::action::GRANT_UPDATE,
            crate::audit::entity::GRANT,
        )
        .target(grant.user_id, &target_email)
        .changes(crate::audit::diff(
            crate::audit::entity::GRANT,
            &[
                (
                    "role_id",
                    serde_json::json!(grant.role_id),
                    serde_json::json!(new_role_id),
                ),
                (
                    "role_name",
                    serde_json::Value::Null,
                    serde_json::json!(new_role.name),
                ),
                (
                    "scope_type",
                    serde_json::json!(grant.scope_type),
                    serde_json::json!(new_scope_type),
                ),
                (
                    "scope_id",
                    serde_json::json!(grant.scope_id),
                    serde_json::json!(new_scope_id),
                ),
            ],
        )),
    )
    .await;
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

    crate::audit::record(
        &mut conn,
        auth.user_id,
        crate::audit::Entry::new(
            org_id,
            crate::audit::action::ROLE_CREATE,
            crate::audit::entity::ROLE,
        )
        .target(role.id, &role.name)
        .changes(crate::audit::created(
            crate::audit::entity::ROLE,
            &[
                ("name", serde_json::json!(role.name)),
                ("permissions", serde_json::json!(req.permissions)),
            ],
        )),
    )
    .await;
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

    // The permission diff is the reason this feature exists: "someone widened
    // a role" is only actionable if the trail says WHICH permission was added.
    // `old_perms`/`new_perms` are already sorted-and-normalized by
    // `role_permissions`, so a reorder alone does not register as a change.
    crate::audit::record(
        &mut conn,
        auth.user_id,
        crate::audit::Entry::new(
            org_id,
            crate::audit::action::ROLE_UPDATE,
            crate::audit::entity::ROLE,
        )
        .target(updated.id, &updated.name)
        .changes(crate::audit::diff(
            crate::audit::entity::ROLE,
            &[
                (
                    "name",
                    serde_json::json!(role.name),
                    serde_json::json!(updated.name),
                ),
                (
                    "permissions",
                    serde_json::json!(old_perms),
                    serde_json::json!(new_perms),
                ),
            ],
        )),
    )
    .await;
    Ok(Json(updated))
}

/// Delete a role this org owns.
///
/// Presets are refused for the same reason edits are: `ensure_preset_roles`
/// re-creates them from rbac.rs at every API boot, so a delete would silently
/// come back on the next restart.
///
/// `role_grants.role_id` is ON DELETE CASCADE, so this revokes the role from
/// every holder at once. The response reports how many grants went with it —
/// the rows are gone by the time the delete returns, so the count is taken
/// first.
pub async fn delete_role_handler(
    auth: AuthUser,
    State(state): State<AppState>,
    Path((org_id, role_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_org(&mut conn, auth.user_id, org_id, perm::ROLE_MANAGE).await?;

    let role = repo::get_role(&mut conn, role_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    // Presets first — their existence is already public via list_roles, so a
    // clear refusal is correct here, not a 404.
    if role.is_system {
        return Err(ApiError::BadRequest(
            "system roles cannot be deleted".into(),
        ));
    }
    // A role owned by another org is not public; NotFound avoids confirming it
    // exists.
    if role.org_id != Some(org_id) {
        return Err(ApiError::NotFound);
    }

    let old_perms = role_permissions(&role.permissions);

    // Deleting a role IS removing all of its permissions, so it takes the same
    // guard the edit path takes: you may not strip a permission you do not
    // hold. Without this, DELETE achieves in one call the sabotage that
    // check_role_edit and delete_grant both refuse — an Admin (role:manage,
    // no org:manage) dissolving a role that confers org:manage.
    let own = sauron_auth::effective_at_org(&mut conn, auth.user_id, org_id).await?;
    check_role_edit(&own, &old_perms, &[]).map_err(ApiError::Auth)?;

    // Deleting a role revokes it from every holder at once. If it is the org's
    // only source of org:manage, that orphans the org exactly as deleting the
    // last owner grant would. Not redundant with the guard above: that one
    // stops a caller who lacks org:manage, this one stops an Owner who holds
    // it and so passes straight through.
    if old_perms.iter().any(|p| p == perm::ORG_MANAGE) {
        let remaining =
            repo::count_org_manage_grants_excluding_role(&mut conn, org_id, role_id).await?;
        if remaining == 0 {
            return Err(ApiError::Conflict(
                "this is the org's last role granting org:manage — grant it elsewhere first".into(),
            ));
        }
    }

    let revoked = repo::count_grants_for_role(&mut conn, role_id).await?;
    let deleted = repo::delete_role(&mut conn, org_id, role_id).await?;
    if deleted == 0 {
        return Err(ApiError::NotFound);
    }

    // Records what the role could do at the moment it was deleted. Without
    // that, the trail would say a role vanished but not what access vanished
    // with it — and the role row is gone, so this is the only chance to say.
    crate::audit::record(
        &mut conn,
        auth.user_id,
        crate::audit::Entry::new(
            org_id,
            crate::audit::action::ROLE_DELETE,
            crate::audit::entity::ROLE,
        )
        .target(role.id, &role.name)
        .changes(crate::audit::diff(
            crate::audit::entity::ROLE,
            &[(
                "permissions",
                serde_json::json!(old_perms),
                serde_json::Value::Null,
            )],
        )),
    )
    .await;
    Ok(Json(json!({ "revoked_grants": revoked })))
}
