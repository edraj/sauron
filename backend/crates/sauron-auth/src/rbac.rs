//! Role-based access control.
//!
//! The resolution core ([`effective_permissions`] / [`has_permission`]) is a
//! pure function over a user's grants, so it is exhaustively unit-tested without
//! a database. The `authorize_*` helpers fetch grants and enforce.
//!
//! Cascade semantics: to check permission `P` on a resource, we pass the
//! resource's `(org, project?, app?, env?)` ids. A grant contributes its
//! permissions when its scope matches one of those ids — so an **org** grant
//! satisfies any check in the org, a **project** grant satisfies checks on that
//! project, its apps, and their environments (but not sibling projects), an
//! **app** grant satisfies that app and every environment under it, and an
//! **env** grant satisfies only that one environment. The result is a union
//! down the tree with strict sibling isolation.

use std::collections::HashSet;

use serde_json::Value;
use uuid::Uuid;

use sauron_db::models::{App, Project};
use sauron_db::scope::{EnvFilter, ReadScope};
use sauron_db::{repo, AsyncPgConnection};

use crate::extractors::AuthError;

/// Canonical permission strings.
pub mod perm {
    pub const ISSUE_READ: &str = "issue:read";
    pub const ISSUE_WRITE: &str = "issue:write";
    pub const EVENT_READ: &str = "event:read";
    pub const FUNNEL_WRITE: &str = "funnel:write";
    pub const ARTIFACT_WRITE: &str = "artifact:write";
    /// View de-obfuscated **source code** (symbolication context lines). Symbol
    /// names / file / line are visible with `issue:read`; this gates the code.
    pub const SOURCE_READ: &str = "source:read";
    pub const MONITOR_READ: &str = "monitor:read";
    pub const MONITOR_WRITE: &str = "monitor:write";
    pub const APP_READ: &str = "app:read";
    pub const APP_CREATE: &str = "app:create";
    pub const APP_UPDATE: &str = "app:update";
    pub const APP_DELETE: &str = "app:delete";
    /// Environments own the ingest credential, so they carry their own family
    /// rather than borrowing the app's. These name *what* is managed; `Scope::Env`
    /// is the new *where* it can be granted, but enforcement (the `authorize_*`
    /// call sites) does not target it at an env yet.
    pub const ENV_READ: &str = "env:read";
    pub const ENV_CREATE: &str = "env:create";
    pub const ENV_UPDATE: &str = "env:update";
    pub const ENV_DELETE: &str = "env:delete";
    pub const ENV_ROTATE_KEY: &str = "env:rotate_key";
    pub const PROJECT_READ: &str = "project:read";
    pub const PROJECT_CREATE: &str = "project:create";
    pub const PROJECT_UPDATE: &str = "project:update";
    pub const PROJECT_DELETE: &str = "project:delete";
    pub const MEMBER_READ: &str = "member:read";
    pub const MEMBER_MANAGE: &str = "member:manage";
    pub const ROLE_MANAGE: &str = "role:manage";
    pub const ORG_MANAGE: &str = "org:manage";
    /// View alert rules, notification channels (secrets always redacted), and
    /// alert delivery history.
    pub const ALERT_READ: &str = "alert:read";
    /// Create/update/delete channels + rules, and send channel test messages.
    pub const ALERT_WRITE: &str = "alert:write";

    /// Every permission, in canonical order.
    pub const ALL: [&str; 27] = [
        ISSUE_READ,
        ISSUE_WRITE,
        EVENT_READ,
        FUNNEL_WRITE,
        ARTIFACT_WRITE,
        SOURCE_READ,
        MONITOR_READ,
        MONITOR_WRITE,
        APP_READ,
        APP_CREATE,
        APP_UPDATE,
        APP_DELETE,
        ENV_READ,
        ENV_CREATE,
        ENV_UPDATE,
        ENV_DELETE,
        ENV_ROTATE_KEY,
        PROJECT_READ,
        PROJECT_CREATE,
        PROJECT_UPDATE,
        PROJECT_DELETE,
        MEMBER_READ,
        MEMBER_MANAGE,
        ROLE_MANAGE,
        ORG_MANAGE,
        ALERT_READ,
        ALERT_WRITE,
    ];
}

/// A seeded, non-editable role.
pub struct PresetRole {
    pub name: &'static str,
    pub description: &'static str,
    pub permissions: &'static [&'static str],
}

pub const OWNER: PresetRole = PresetRole {
    name: "Owner",
    description: "Full control including organization settings",
    permissions: &perm::ALL,
};

pub const ADMIN: PresetRole = PresetRole {
    name: "Admin",
    description: "Manage projects, apps, members and roles",
    permissions: &[
        perm::ISSUE_READ,
        perm::ISSUE_WRITE,
        perm::EVENT_READ,
        perm::FUNNEL_WRITE,
        perm::ARTIFACT_WRITE,
        perm::SOURCE_READ,
        perm::MONITOR_READ,
        perm::MONITOR_WRITE,
        perm::APP_READ,
        perm::APP_CREATE,
        perm::APP_UPDATE,
        perm::APP_DELETE,
        perm::ENV_READ,
        perm::ENV_CREATE,
        perm::ENV_UPDATE,
        perm::ENV_DELETE,
        perm::ENV_ROTATE_KEY,
        perm::PROJECT_READ,
        perm::PROJECT_CREATE,
        perm::PROJECT_UPDATE,
        perm::PROJECT_DELETE,
        perm::MEMBER_READ,
        perm::MEMBER_MANAGE,
        perm::ROLE_MANAGE,
        perm::ALERT_READ,
        perm::ALERT_WRITE,
    ],
};

pub const DEVELOPER: PresetRole = PresetRole {
    name: "Developer",
    description: "Work with issues and apps",
    permissions: &[
        perm::ISSUE_READ,
        perm::ISSUE_WRITE,
        perm::EVENT_READ,
        perm::FUNNEL_WRITE,
        perm::ARTIFACT_WRITE,
        perm::SOURCE_READ,
        perm::MONITOR_READ,
        perm::MONITOR_WRITE,
        perm::APP_READ,
        perm::APP_CREATE,
        perm::APP_UPDATE,
        perm::ENV_READ,
        perm::ENV_CREATE,
        perm::ENV_UPDATE,
        perm::ENV_ROTATE_KEY,
        perm::PROJECT_READ,
        perm::MEMBER_READ,
        perm::ALERT_READ,
    ],
};

pub const VIEWER: PresetRole = PresetRole {
    name: "Viewer",
    description: "Read-only access",
    permissions: &[
        perm::ISSUE_READ,
        perm::EVENT_READ,
        perm::MONITOR_READ,
        perm::APP_READ,
        perm::ENV_READ,
        perm::PROJECT_READ,
        perm::MEMBER_READ,
    ],
};

pub const PRESETS: [PresetRole; 4] = [OWNER, ADMIN, DEVELOPER, VIEWER];

/// The level a grant applies at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Org(Uuid),
    Project(Uuid),
    App(Uuid),
    Env(Uuid),
}

impl Scope {
    /// The `(scope_type, scope_id)` pair as stored in `role_grants`.
    ///
    /// Round-trips [`grants_from_rows`], and lets a caller name the offending
    /// scope in an error message without rebuilding the string form itself.
    pub fn parts(self) -> (&'static str, Uuid) {
        match self {
            Scope::Org(id) => ("org", id),
            Scope::Project(id) => ("project", id),
            Scope::App(id) => ("app", id),
            Scope::Env(id) => ("env", id),
        }
    }
}

/// A user's grant: a scope plus the permissions its role confers.
#[derive(Clone, Debug)]
pub struct Grant {
    pub scope: Scope,
    pub permissions: Vec<String>,
}

fn grant_applies(
    scope: Scope,
    org: Uuid,
    project: Option<Uuid>,
    app: Option<Uuid>,
    env: Option<Uuid>,
) -> bool {
    match scope {
        Scope::Org(o) => o == org,
        Scope::Project(p) => Some(p) == project,
        Scope::App(a) => Some(a) == app,
        Scope::Env(e) => Some(e) == env,
    }
}

/// The union of all permissions the user has on the target `(org, project?, app?, env?)`.
pub fn effective_permissions(
    grants: &[Grant],
    org: Uuid,
    project: Option<Uuid>,
    app: Option<Uuid>,
    env: Option<Uuid>,
) -> HashSet<String> {
    let mut set = HashSet::new();
    for g in grants {
        if grant_applies(g.scope, org, project, app, env) {
            for p in &g.permissions {
                set.insert(p.clone());
            }
        }
    }
    set
}

/// Whether the user holds `permission` on the target (short-circuits).
pub fn has_permission(
    grants: &[Grant],
    permission: &str,
    org: Uuid,
    project: Option<Uuid>,
    app: Option<Uuid>,
    env: Option<Uuid>,
) -> bool {
    grants.iter().any(|g| {
        grant_applies(g.scope, org, project, app, env)
            && g.permissions.iter().any(|p| p == permission)
    })
}

/// Where a set of grants confers `permission`, decomposed for discovery queries.
///
/// `authorize_*` answers "may this caller act on THIS resource". Listing needs the
/// inverse — "which resources may this caller see" — and that cannot be expressed as a
/// single check at a fixed scope: a grant narrower than the check can never satisfy it,
/// which is why an app-scoped member used to get 403 from every listing endpoint.
#[derive(Debug, Default, PartialEq)]
pub struct Reach {
    /// Held at org scope — everything in the org is visible.
    pub org: bool,
    pub projects: Vec<Uuid>,
    pub apps: Vec<Uuid>,
    pub envs: Vec<Uuid>,
}

/// Callers MUST pass grants already filtered to a single organization (as
/// `repo::user_grants_in_org` does). The `Scope::Org` arm does not compare the
/// grant's org id, so an unfiltered grant list would leak another org's
/// visibility.
pub fn reach_for(grants: &[Grant], permission: &str) -> Reach {
    let mut reach = Reach::default();
    for g in grants {
        if !g.permissions.iter().any(|p| p == permission) {
            continue;
        }
        match g.scope {
            Scope::Org(_) => reach.org = true,
            Scope::Project(p) => reach.projects.push(p),
            Scope::App(a) => reach.apps.push(a),
            Scope::Env(e) => reach.envs.push(e),
        }
    }
    reach
}

/// Convert `(scope_type, scope_id, permissions_json)` rows into [`Grant`]s.
pub fn grants_from_rows(rows: Vec<(String, Uuid, Value)>) -> Vec<Grant> {
    rows.into_iter()
        .filter_map(|(scope_type, scope_id, perms)| {
            let scope = match scope_type.as_str() {
                "org" => Scope::Org(scope_id),
                "project" => Scope::Project(scope_id),
                "app" => Scope::App(scope_id),
                "env" => Scope::Env(scope_id),
                _ => return None,
            };
            let permissions = match perms {
                Value::Array(a) => a
                    .into_iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect(),
                _ => Vec::new(),
            };
            Some(Grant { scope, permissions })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Enforcement (DB-backed)
// ---------------------------------------------------------------------------

/// Load the user's grants in an org and check a permission at a target scope.
pub async fn require_permission(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    permission: &str,
    org: Uuid,
    project: Option<Uuid>,
    app: Option<Uuid>,
) -> Result<(), AuthError> {
    let rows = repo::user_grants_in_org(conn, user_id, org)
        .await
        .map_err(|_| AuthError::Internal)?;
    let grants = grants_from_rows(rows);
    if has_permission(&grants, permission, org, project, app, None) {
        Ok(())
    } else {
        Err(AuthError::Forbidden)
    }
}

/// The user's effective permission set at an arbitrary target scope.
pub async fn effective_at(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    org: Uuid,
    project: Option<Uuid>,
    app: Option<Uuid>,
) -> Result<HashSet<String>, AuthError> {
    let rows = repo::user_grants_in_org(conn, user_id, org)
        .await
        .map_err(|_| AuthError::Internal)?;
    let grants = grants_from_rows(rows);
    Ok(effective_permissions(&grants, org, project, app, None))
}

/// The user's effective permission set at an org (for `GET /me/access`).
pub async fn effective_at_org(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    org: Uuid,
) -> Result<HashSet<String>, AuthError> {
    effective_at(conn, user_id, org, None, None).await
}

/// Authorize an **org**-scoped action.
pub async fn authorize_org(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    org_id: Uuid,
    permission: &str,
) -> Result<(), AuthError> {
    require_permission(conn, user_id, permission, org_id, None, None).await
}

/// Authorize a **project**-scoped action; returns the project.
pub async fn authorize_project(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    project_id: Uuid,
    permission: &str,
) -> Result<Project, AuthError> {
    let project = repo::get_project(conn, project_id)
        .await
        .map_err(|_| AuthError::Internal)?
        .ok_or(AuthError::NotFound)?;
    require_permission(
        conn,
        user_id,
        permission,
        project.org_id,
        Some(project_id),
        None,
    )
    .await?;
    Ok(project)
}

/// Authorize an **app**-scoped action; returns the app.
pub async fn authorize_app(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    app_id: Uuid,
    permission: &str,
) -> Result<App, AuthError> {
    let app = repo::get_app(conn, app_id)
        .await
        .map_err(|_| AuthError::Internal)?
        .ok_or(AuthError::NotFound)?;
    let (project_id, org_id) = repo::app_ancestry(conn, app_id)
        .await
        .map_err(|_| AuthError::Internal)?
        .ok_or(AuthError::NotFound)?;
    require_permission(
        conn,
        user_id,
        permission,
        org_id,
        Some(project_id),
        Some(app_id),
    )
    .await?;
    Ok(app)
}

/// [`authorize_app`]'s reach-aware sibling: succeeds when `permission` is held
/// at org, project, or app scope — exactly what `authorize_app` already
/// accepts — **or** at any environment under this app. Returns the app.
///
/// This exists for **reads that exist so a caller can navigate to their own
/// environment**, not as a looser drop-in for `authorize_app` generally. An
/// environment grant is strictly narrower than its app: it must let a caller
/// fetch the app's metadata (`GET /v1/apps/{id}`, so the dashboard can render
/// something to navigate into), but it must **not** let them rename, mute, or
/// delete the app — those stay on `authorize_app`'s strict `env: None`
/// resolution, which an env-scoped grant's `Scope::Env` arm can never satisfy
/// (see this module's doc comment on cascade semantics). Call this only at a
/// site where "read the app so I can get to my environment" is the intent;
/// every mutation site must keep calling `authorize_app` directly.
///
/// Cost: identical to `authorize_app` when the caller already has org/
/// project/app reach (the overwhelmingly common case) — the environment
/// lookup below only runs when that first check fails.
pub async fn authorize_app_reachable(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    app_id: Uuid,
    permission: &str,
) -> Result<App, AuthError> {
    let app = repo::get_app(conn, app_id)
        .await
        .map_err(|_| AuthError::Internal)?
        .ok_or(AuthError::NotFound)?;
    let (project_id, org_id) = repo::app_ancestry(conn, app_id)
        .await
        .map_err(|_| AuthError::Internal)?
        .ok_or(AuthError::NotFound)?;

    let rows = repo::user_grants_in_org(conn, user_id, org_id)
        .await
        .map_err(|_| AuthError::Internal)?;
    let grants = grants_from_rows(rows);

    // Fast path: same check `authorize_app` makes. Org/project/app-scoped
    // grants are resolved here and never need the environment lookup below.
    if has_permission(
        &grants,
        permission,
        org_id,
        Some(project_id),
        Some(app_id),
        None,
    ) {
        return Ok(app);
    }

    // No grant reaches the app itself — the only way left in is a grant on
    // one of its own environments. `reach_for` is not app-scoped by
    // construction (an env grant on a DIFFERENT app's environment would also
    // show up in `reach.envs`), so intersect with this app's actual
    // environment ids before accepting — the same guard
    // `resolve_env_filter`'s `an_env_grant_from_another_app_contributes_nothing`
    // test pins for the read-scope path.
    let reach = reach_for(&grants, permission);
    if reach.envs.is_empty() {
        return Err(AuthError::Forbidden);
    }
    let app_env_ids: HashSet<Uuid> = repo::env_ids_for_app(conn, app_id)
        .await
        .map_err(|_| AuthError::Internal)?
        .into_iter()
        .collect();
    if reach.envs.iter().any(|e| app_env_ids.contains(e)) {
        Ok(app)
    } else {
        Err(AuthError::Forbidden)
    }
}

/// Why an environment-scoped read was refused. Mapped to HTTP by the caller;
/// kept separate from `AuthError` so the pure decision function stays free of
/// transport concerns and stays unit-testable without a database.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvDenied {
    /// No grant carrying this permission reaches this app or any of its
    /// environments.
    NoReach,
    /// The requested environment id is not one of this app's environments —
    /// it does not exist, or it belongs to a different app.
    EnvNotInApp,
    /// The environment exists on this app, but the caller holds no grant on it.
    EnvNotGranted,
    /// `?environment_id=none` selects rows attributed to no environment, which
    /// only a caller with app-wide reach may read.
    UnattributedNeedsAppReach,
}

/// Resolve what the caller asked for into what they are allowed to have.
///
/// Pure: no I/O, no clock. `app_env_ids` is every environment of the app,
/// **including retired ones** — a retired environment's history stays readable
/// (Slice 1's invariant), so excluding them here would make `Subset` narrower
/// than the `All` it stands in for.
///
/// The order of the checks is load-bearing. Ownership (`EnvNotInApp`) is
/// tested before grant-holding (`EnvNotGranted`) so that a caller probing for
/// which environment ids exist learns nothing they could not learn from
/// `list_environments` — both refusals are a 403 at the HTTP layer.
pub fn resolve_env_filter(
    grants: &[Grant],
    permission: &str,
    org: Uuid,
    project: Uuid,
    app: Uuid,
    app_env_ids: &[Uuid],
    requested: EnvFilter,
) -> Result<EnvFilter, EnvDenied> {
    let app_wide = has_permission(grants, permission, org, Some(project), Some(app), None);

    if app_wide {
        return match requested {
            EnvFilter::All => Ok(EnvFilter::All),
            EnvFilter::Unattributed => Ok(EnvFilter::Unattributed),
            EnvFilter::One(id) => {
                if app_env_ids.contains(&id) {
                    Ok(EnvFilter::One(id))
                } else {
                    Err(EnvDenied::EnvNotInApp)
                }
            }
            // A caller cannot ask for a Subset over the wire; it is only ever
            // produced here. Treat it as All rather than trusting the input.
            EnvFilter::Subset(_) => Ok(EnvFilter::All),
        };
    }

    let reach = reach_for(grants, permission);
    let mut readable: Vec<Uuid> = app_env_ids
        .iter()
        .copied()
        .filter(|e| reach.envs.contains(e))
        .collect();
    readable.sort();
    readable.dedup();

    if readable.is_empty() {
        return Err(EnvDenied::NoReach);
    }

    match requested {
        EnvFilter::All | EnvFilter::Subset(_) => Ok(EnvFilter::Subset(readable)),
        EnvFilter::Unattributed => Err(EnvDenied::UnattributedNeedsAppReach),
        EnvFilter::One(id) => {
            if !app_env_ids.contains(&id) {
                Err(EnvDenied::EnvNotInApp)
            } else if readable.contains(&id) {
                Ok(EnvFilter::One(id))
            } else {
                Err(EnvDenied::EnvNotGranted)
            }
        }
    }
}

/// The caller's effective permission set over everything a resolved
/// [`EnvFilter`] can read — the scope-aware counterpart to
/// [`effective_permissions`], for a **second** permission question asked at the
/// same scope as the read itself (today: `source:read` deciding whether an
/// issue's de-obfuscated source context is included).
///
/// Pure. Why each arm is what it is:
///
/// - `All` / `Unattributed` — both are only ever resolved for a caller with
///   app-wide reach, so the question is an app-level one: `env: None`, exactly
///   what [`effective_at`] has always computed. An environment-scoped grant
///   deliberately does **not** contribute here: it would let a `source:read`
///   held on one environment unlock source across every other one.
/// - `One(id)` — evaluated at that environment. This can only ever *add* to the
///   app-level answer, never subtract: `grant_applies`'s `Org`/`Project`/`App`
///   arms ignore the `env` argument entirely, so every grant that satisfied
///   `env: None` still satisfies `env: Some(id)`.
/// - `Subset(ids)` — the read spans several environments and a single boolean
///   gate has to cover all of them, so this is the **intersection**: a
///   permission counts only if it is held in every environment the response
///   could draw a row from. Fail-closed, and it cannot leak source from an
///   environment where the caller lacks the grant. (Anything held app-wide or
///   above survives the intersection untouched, per the `One` note above.)
pub fn effective_permissions_for_filter(
    grants: &[Grant],
    org: Uuid,
    project: Uuid,
    app: Uuid,
    env: &EnvFilter,
) -> HashSet<String> {
    let at = |e: Option<Uuid>| effective_permissions(grants, org, Some(project), Some(app), e);
    match env {
        EnvFilter::All | EnvFilter::Unattributed => at(None),
        EnvFilter::One(id) => at(Some(*id)),
        EnvFilter::Subset(ids) => {
            let mut per_env = ids.iter().map(|id| at(Some(*id)));
            match per_env.next() {
                Some(first) => per_env.fold(first, |acc, next| {
                    acc.intersection(&next).cloned().collect()
                }),
                // `Subset` is never empty by construction (`resolve_env_filter`
                // returns `Err(NoReach)` rather than an empty set), so this arm
                // is unreachable — but an empty set is the fail-closed answer
                // if it ever became reachable.
                None => HashSet::new(),
            }
        }
    }
}

/// Shared core of [`authorize_env_read`] and [`authorize_env_read_with_perms`].
///
/// Returns the authorized scope **plus** the grants and ancestry it already had
/// to load, so a caller that needs a second permission answer at the same scope
/// can have it without re-running the ancestry and grant queries. That sharing
/// is the point: `routes::mod::authorize_app_perms` existed precisely because
/// asking two permission questions used to mean two full resolutions ("six
/// queries where two suffice", as its doc comment put it), and this keeps that
/// property while making the second question environment-aware.
async fn authorize_env_read_inner(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    app_id: Uuid,
    permission: &str,
    requested: EnvFilter,
) -> Result<(ReadScope, Vec<Grant>, Uuid, Uuid), AuthError> {
    let (project_id, org_id) = repo::app_ancestry(conn, app_id)
        .await
        .map_err(|_| AuthError::Internal)?
        .ok_or(AuthError::NotFound)?;

    let rows = repo::user_grants_in_org(conn, user_id, org_id)
        .await
        .map_err(|_| AuthError::Internal)?;
    let grants = grants_from_rows(rows);

    // Fast path: app-wide reach over every environment needs no environment
    // lookup at all, so today's callers pay exactly today's cost.
    if matches!(requested, EnvFilter::All)
        && has_permission(
            &grants,
            permission,
            org_id,
            Some(project_id),
            Some(app_id),
            None,
        )
    {
        return Ok((
            ReadScope::new(app_id, EnvFilter::All),
            grants,
            org_id,
            project_id,
        ));
    }

    let app_env_ids = repo::env_ids_for_app(conn, app_id)
        .await
        .map_err(|_| AuthError::Internal)?;

    let resolved = resolve_env_filter(
        &grants,
        permission,
        org_id,
        project_id,
        app_id,
        &app_env_ids,
        requested,
    )
    .map_err(|_| AuthError::Forbidden)?;

    Ok((ReadScope::new(app_id, resolved), grants, org_id, project_id))
}

/// Authorize an environment-scoped **read** and produce its `ReadScope`.
///
/// Replaces the `authorize_app(...)` + `read_scope_raw(...)` pair. They are one
/// call because they were two decisions that had to agree, and four separate
/// defects in this feature came from two things that had to agree by hand.
///
/// Cost: identical to `authorize_app` for the overwhelmingly common case —
/// a caller with app-wide reach asking for every environment never triggers the
/// `env_ids_for_app` lookup.
pub async fn authorize_env_read(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    app_id: Uuid,
    permission: &str,
    requested: EnvFilter,
) -> Result<ReadScope, AuthError> {
    let (scope, ..) =
        authorize_env_read_inner(conn, user_id, app_id, permission, requested).await?;
    Ok(scope)
}

/// [`authorize_env_read`], plus the caller's full effective permission set at
/// the **resolved** scope — for a handler that gates a second capability on top
/// of the read itself (`issues::detail`/`issues::events` and `source:read`).
///
/// Supersedes `routes::mod::authorize_app_perms` for environment-scoped reads.
/// That helper resolved permissions through [`effective_at`], which hardcodes
/// `env: None`; since `grant_applies`'s `Scope::Env` arm is `Some(e) == env`, an
/// environment-scoped grant could never satisfy it, so those two handlers
/// returned `403` to an env-scoped caller **even for their own environment** —
/// they could list issues but not open one. Authorization still happens first
/// (this returns `Err` before computing any permission set if the caller has no
/// reach), and the second question is then answered at the scope the read was
/// actually narrowed to, via [`effective_permissions_for_filter`].
pub async fn authorize_env_read_with_perms(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    app_id: Uuid,
    permission: &str,
    requested: EnvFilter,
) -> Result<(ReadScope, HashSet<String>), AuthError> {
    let (scope, grants, org_id, project_id) =
        authorize_env_read_inner(conn, user_id, app_id, permission, requested).await?;
    let perms = effective_permissions_for_filter(&grants, org_id, project_id, app_id, &scope.env);
    Ok((scope, perms))
}

/// Idempotently sync the seeded preset roles from code (called at startup).
pub async fn ensure_preset_roles(conn: &mut AsyncPgConnection) -> anyhow::Result<()> {
    for preset in PRESETS {
        let perms = Value::Array(
            preset
                .permissions
                .iter()
                .map(|s| Value::String((*s).to_string()))
                .collect(),
        );
        repo::upsert_preset_role(conn, preset.name, preset.description, &perms)
            .await
            .map_err(|e| anyhow::anyhow!("seed preset {}: {e}", preset.name))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn org() -> Uuid {
        Uuid::from_u128(1)
    }
    fn proj_a() -> Uuid {
        Uuid::from_u128(10)
    }
    fn proj_b() -> Uuid {
        Uuid::from_u128(11)
    }
    fn app_a1() -> Uuid {
        Uuid::from_u128(100)
    }
    fn app_a2() -> Uuid {
        Uuid::from_u128(101)
    }
    fn app_b1() -> Uuid {
        Uuid::from_u128(110)
    }

    fn grant(scope: Scope, perms: &[&str]) -> Grant {
        Grant {
            scope,
            permissions: perms.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn preset_grant(scope: Scope, p: &PresetRole) -> Grant {
        Grant {
            scope,
            permissions: p.permissions.iter().map(|s| s.to_string()).collect(),
        }
    }

    // --- preset role permission sets ------------------------------------

    #[test]
    fn owner_has_every_permission() {
        for p in perm::ALL {
            assert!(OWNER.permissions.contains(&p), "Owner missing {p}");
        }
        assert_eq!(OWNER.permissions.len(), 27);
    }

    #[test]
    fn admin_is_all_except_org_manage() {
        assert!(!ADMIN.permissions.contains(&perm::ORG_MANAGE));
        assert_eq!(ADMIN.permissions.len(), 26);
        for p in perm::ALL {
            if p != perm::ORG_MANAGE {
                assert!(ADMIN.permissions.contains(&p), "Admin missing {p}");
            }
        }
    }

    #[test]
    fn developer_can_write_issues_not_manage_members() {
        assert!(DEVELOPER.permissions.contains(&perm::ISSUE_WRITE));
        assert!(DEVELOPER.permissions.contains(&perm::ENV_ROTATE_KEY));
        assert!(!DEVELOPER.permissions.contains(&perm::MEMBER_MANAGE));
        assert!(!DEVELOPER.permissions.contains(&perm::PROJECT_DELETE));
        assert!(!DEVELOPER.permissions.contains(&perm::ROLE_MANAGE));
        assert!(DEVELOPER.permissions.contains(&perm::FUNNEL_WRITE));
        assert!(DEVELOPER.permissions.contains(&perm::ARTIFACT_WRITE));
        assert!(DEVELOPER.permissions.contains(&perm::SOURCE_READ));
        assert_eq!(DEVELOPER.permissions.len(), 18);
    }

    /// Developer manages environments day to day but cannot retire one, mirroring
    /// how it holds `app:update` without `app:delete`.
    #[test]
    fn developer_manages_envs_but_cannot_retire() {
        assert!(DEVELOPER.permissions.contains(&perm::ENV_READ));
        assert!(DEVELOPER.permissions.contains(&perm::ENV_CREATE));
        assert!(DEVELOPER.permissions.contains(&perm::ENV_UPDATE));
        assert!(!DEVELOPER.permissions.contains(&perm::ENV_DELETE));
    }

    #[test]
    fn viewer_cannot_write_funnels() {
        assert!(VIEWER.permissions.contains(&perm::EVENT_READ));
        assert!(!VIEWER.permissions.contains(&perm::FUNNEL_WRITE));
    }

    #[test]
    fn viewer_is_read_only() {
        for p in VIEWER.permissions {
            assert!(p.ends_with(":read"), "Viewer has non-read perm {p}");
        }
        assert!(VIEWER.permissions.contains(&perm::ISSUE_READ));
        assert!(!VIEWER.permissions.contains(&perm::ISSUE_WRITE));
        assert_eq!(VIEWER.permissions.len(), 7);
    }

    #[test]
    fn preset_names_are_unique() {
        let names: HashSet<_> = PRESETS.iter().map(|p| p.name).collect();
        assert_eq!(names.len(), PRESETS.len());
    }

    #[test]
    fn all_permissions_are_unique() {
        let set: HashSet<_> = perm::ALL.iter().collect();
        assert_eq!(set.len(), perm::ALL.len(), "duplicate in perm::ALL");
        assert_eq!(perm::ALL.len(), 27);
    }

    #[test]
    fn every_preset_permission_is_a_known_permission() {
        for preset in PRESETS {
            for p in preset.permissions {
                assert!(
                    perm::ALL.contains(p),
                    "{} has unknown permission {p}",
                    preset.name
                );
            }
            // no duplicate perms within a preset
            let set: HashSet<_> = preset.permissions.iter().collect();
            assert_eq!(
                set.len(),
                preset.permissions.len(),
                "{} has dupes",
                preset.name
            );
        }
    }

    #[test]
    fn roles_form_a_strict_ladder() {
        // Viewer ⊂ Developer ⊂ Admin ⊂ Owner
        let v: HashSet<_> = VIEWER.permissions.iter().collect();
        let d: HashSet<_> = DEVELOPER.permissions.iter().collect();
        let a: HashSet<_> = ADMIN.permissions.iter().collect();
        let o: HashSet<_> = OWNER.permissions.iter().collect();
        assert!(v.is_subset(&d), "Viewer not ⊆ Developer");
        assert!(d.is_subset(&a), "Developer not ⊆ Admin");
        assert!(a.is_subset(&o), "Admin not ⊆ Owner");
    }

    #[test]
    fn multiple_grants_at_same_scope_union() {
        let g = vec![
            grant(Scope::App(app_a1()), &[perm::ISSUE_READ]),
            grant(Scope::App(app_a1()), &[perm::ISSUE_WRITE]),
        ];
        let eff = effective_permissions(&g, org(), Some(proj_a()), Some(app_a1()), None);
        assert!(eff.contains(perm::ISSUE_READ));
        assert!(eff.contains(perm::ISSUE_WRITE));
        assert_eq!(eff.len(), 2);
    }

    #[test]
    fn permission_match_is_exact_not_prefix_or_substring() {
        let g = vec![grant(Scope::Org(org()), &["issue:rea"])];
        assert!(!has_permission(
            &g,
            perm::ISSUE_READ,
            org(),
            None,
            None,
            None
        ));
        let g2 = vec![grant(Scope::Org(org()), &[perm::ISSUE_READ])];
        assert!(!has_permission(&g2, "issue", org(), None, None, None));
        assert!(!has_permission(
            &g2,
            "issue:read:extra",
            org(),
            None,
            None,
            None
        ));
    }

    #[test]
    fn org_scope_check_ignores_lower_scoped_grants() {
        // A user with only project/app grants has NO org-level permissions.
        let g = vec![
            preset_grant(Scope::Project(proj_a()), &OWNER),
            preset_grant(Scope::App(app_a1()), &OWNER),
        ];
        assert!(effective_permissions(&g, org(), None, None, None).is_empty());
        assert!(!has_permission(
            &g,
            perm::MEMBER_READ,
            org(),
            None,
            None,
            None
        ));
    }

    // --- org-scope grant cascades to everything -------------------------

    #[test]
    fn org_grant_applies_at_every_level() {
        let g = vec![preset_grant(Scope::Org(org()), &DEVELOPER)];
        // org-level check
        assert!(has_permission(
            &g,
            perm::ISSUE_READ,
            org(),
            None,
            None,
            None
        ));
        // project-level check
        assert!(has_permission(
            &g,
            perm::ISSUE_READ,
            org(),
            Some(proj_a()),
            None,
            None
        ));
        // app-level check
        assert!(has_permission(
            &g,
            perm::ISSUE_WRITE,
            org(),
            Some(proj_a()),
            Some(app_a1()),
            None
        ));
        // but not a permission the role lacks
        assert!(!has_permission(
            &g,
            perm::ORG_MANAGE,
            org(),
            None,
            None,
            None
        ));
    }

    #[test]
    fn org_grant_for_a_different_org_never_applies() {
        let other = Uuid::from_u128(999);
        let g = vec![preset_grant(Scope::Org(other), &OWNER)];
        assert!(!has_permission(
            &g,
            perm::ISSUE_READ,
            org(),
            None,
            None,
            None
        ));
        assert!(!has_permission(
            &g,
            perm::ISSUE_READ,
            org(),
            Some(proj_a()),
            Some(app_a1()),
            None
        ));
    }

    // --- project-scope grant: its apps yes, siblings no -----------------

    #[test]
    fn project_grant_covers_its_apps_only() {
        let g = vec![preset_grant(Scope::Project(proj_a()), &DEVELOPER)];
        // app in project A
        assert!(has_permission(
            &g,
            perm::ISSUE_WRITE,
            org(),
            Some(proj_a()),
            Some(app_a1()),
            None
        ));
        // another app in project A
        assert!(has_permission(
            &g,
            perm::ISSUE_WRITE,
            org(),
            Some(proj_a()),
            Some(app_a2()),
            None
        ));
        // app in project B — DENIED (sibling isolation)
        assert!(!has_permission(
            &g,
            perm::ISSUE_WRITE,
            org(),
            Some(proj_b()),
            Some(app_b1()),
            None
        ));
        // project A itself
        assert!(has_permission(
            &g,
            perm::ISSUE_READ,
            org(),
            Some(proj_a()),
            None,
            None
        ));
        // project B itself — DENIED
        assert!(!has_permission(
            &g,
            perm::ISSUE_READ,
            org(),
            Some(proj_b()),
            None,
            None
        ));
        // org level — DENIED (project grant doesn't grant org-wide)
        assert!(!has_permission(
            &g,
            perm::ISSUE_READ,
            org(),
            None,
            None,
            None
        ));
    }

    // --- app-scope grant: that app only ---------------------------------

    #[test]
    fn app_grant_covers_that_app_only() {
        let g = vec![preset_grant(Scope::App(app_a1()), &VIEWER)];
        assert!(has_permission(
            &g,
            perm::ISSUE_READ,
            org(),
            Some(proj_a()),
            Some(app_a1()),
            None
        ));
        // sibling app — DENIED
        assert!(!has_permission(
            &g,
            perm::ISSUE_READ,
            org(),
            Some(proj_a()),
            Some(app_a2()),
            None
        ));
        // project-level op — DENIED (app grant can't authorize project ops)
        assert!(!has_permission(
            &g,
            perm::ISSUE_READ,
            org(),
            Some(proj_a()),
            None,
            None
        ));
        // org-level op — DENIED
        assert!(!has_permission(
            &g,
            perm::ISSUE_READ,
            org(),
            None,
            None,
            None
        ));
    }

    // --- union of multiple grants ---------------------------------------

    #[test]
    fn permissions_union_across_grants() {
        let g = vec![
            grant(Scope::App(app_a1()), &[perm::ISSUE_READ]),
            grant(Scope::Project(proj_a()), &[perm::EVENT_READ]),
            grant(Scope::Org(org()), &[perm::APP_READ]),
        ];
        // app check sees all three levels unioned
        let eff = effective_permissions(&g, org(), Some(proj_a()), Some(app_a1()), None);
        assert!(eff.contains(perm::ISSUE_READ));
        assert!(eff.contains(perm::EVENT_READ));
        assert!(eff.contains(perm::APP_READ));
        assert_eq!(eff.len(), 3);

        // a sibling app in the SAME project inherits the project + org grants,
        // but NOT the app_a1-specific grant.
        let eff2 = effective_permissions(&g, org(), Some(proj_a()), Some(app_a2()), None);
        assert!(eff2.contains(perm::APP_READ)); // org grant
        assert!(eff2.contains(perm::EVENT_READ)); // project-A grant
        assert!(!eff2.contains(perm::ISSUE_READ)); // app_a1-specific grant does NOT apply
        assert_eq!(eff2.len(), 2);

        // an app in a DIFFERENT project inherits only the org grant.
        let eff3 = effective_permissions(&g, org(), Some(proj_b()), Some(app_b1()), None);
        assert!(eff3.contains(perm::APP_READ));
        assert!(!eff3.contains(perm::EVENT_READ));
        assert!(!eff3.contains(perm::ISSUE_READ));
        assert_eq!(eff3.len(), 1);
    }

    #[test]
    fn viewer_denied_write_but_allowed_read() {
        let g = vec![preset_grant(Scope::Org(org()), &VIEWER)];
        assert!(has_permission(
            &g,
            perm::ISSUE_READ,
            org(),
            Some(proj_a()),
            Some(app_a1()),
            None
        ));
        assert!(!has_permission(
            &g,
            perm::ISSUE_WRITE,
            org(),
            Some(proj_a()),
            Some(app_a1()),
            None
        ));
        assert!(!has_permission(
            &g,
            perm::MEMBER_MANAGE,
            org(),
            None,
            None,
            None
        ));
    }

    #[test]
    fn empty_grants_deny_everything() {
        let g: Vec<Grant> = vec![];
        for p in perm::ALL {
            assert!(!has_permission(
                &g,
                p,
                org(),
                Some(proj_a()),
                Some(app_a1()),
                None
            ));
        }
        assert!(effective_permissions(&g, org(), Some(proj_a()), Some(app_a1()), None).is_empty());
    }

    #[test]
    fn monitor_perms_are_registered_and_seeded() {
        // Both perms exist in the canonical list.
        assert!(perm::ALL.contains(&perm::MONITOR_READ));
        assert!(perm::ALL.contains(&perm::MONITOR_WRITE));
        // Owner (=ALL) has both.
        assert!(OWNER.permissions.contains(&perm::MONITOR_WRITE));
        // Viewer reads but cannot write.
        assert!(VIEWER.permissions.contains(&perm::MONITOR_READ));
        assert!(!VIEWER.permissions.contains(&perm::MONITOR_WRITE));
        // Developer can write.
        assert!(DEVELOPER.permissions.contains(&perm::MONITOR_WRITE));
    }

    #[test]
    fn scope_parts_round_trips_the_stored_column_values() {
        assert_eq!(Scope::Org(org()).parts(), ("org", org()));
        assert_eq!(Scope::Project(proj_a()).parts(), ("project", proj_a()));
        assert_eq!(Scope::App(app_a1()).parts(), ("app", app_a1()));
        // The strings must match what grants_from_rows accepts, or a scope
        // named in an error could not be looked up again.
        for scope in [
            Scope::Org(org()),
            Scope::Project(proj_a()),
            Scope::App(app_a1()),
        ] {
            let (scope_type, scope_id) = scope.parts();
            let rows = vec![(
                scope_type.to_string(),
                scope_id,
                serde_json::json!(["issue:read"]),
            )];
            assert_eq!(grants_from_rows(rows)[0].scope, scope);
        }
    }

    // --- reach_for: decompose grants for discovery -----------------------

    #[test]
    fn reach_for_org_grant_sets_org_flag_and_leaves_vectors_empty() {
        let g = vec![grant(Scope::Org(org()), &[perm::PROJECT_READ])];
        let reach = reach_for(&g, perm::PROJECT_READ);
        assert!(reach.org);
        assert!(reach.projects.is_empty());
        assert!(reach.apps.is_empty());
    }

    #[test]
    fn reach_for_project_grant_collects_only_that_project() {
        let g = vec![grant(Scope::Project(proj_a()), &[perm::PROJECT_READ])];
        let reach = reach_for(&g, perm::PROJECT_READ);
        assert!(!reach.org);
        assert_eq!(reach.projects, vec![proj_a()]);
        assert!(reach.apps.is_empty());
    }

    #[test]
    fn reach_for_app_grant_collects_only_that_app() {
        let g = vec![grant(Scope::App(app_a1()), &[perm::APP_READ])];
        let reach = reach_for(&g, perm::APP_READ);
        assert!(!reach.org);
        assert!(reach.projects.is_empty());
        assert_eq!(reach.apps, vec![app_a1()]);
    }

    #[test]
    fn reach_for_grant_lacking_permission_contributes_nothing() {
        let g = vec![
            grant(Scope::Org(org()), &[perm::ISSUE_READ]),
            grant(Scope::Project(proj_a()), &[perm::ISSUE_READ]),
            grant(Scope::App(app_a1()), &[perm::ISSUE_READ]),
        ];
        let reach = reach_for(&g, perm::PROJECT_READ);
        assert_eq!(reach, Reach::default());
    }

    #[test]
    fn reach_for_mixed_grants_accumulate_across_all_three_scopes() {
        let g = vec![
            grant(Scope::Org(org()), &[perm::PROJECT_READ]),
            grant(Scope::Project(proj_a()), &[perm::PROJECT_READ]),
            grant(Scope::Project(proj_b()), &[perm::PROJECT_READ]),
            grant(Scope::App(app_a1()), &[perm::PROJECT_READ]),
            grant(Scope::App(app_a2()), &[perm::PROJECT_READ]),
            // a grant that doesn't carry the permission contributes nothing
            grant(Scope::App(app_b1()), &[perm::ISSUE_READ]),
        ];
        let reach = reach_for(&g, perm::PROJECT_READ);
        assert!(reach.org);
        assert_eq!(reach.projects, vec![proj_a(), proj_b()]);
        assert_eq!(reach.apps, vec![app_a1(), app_a2()]);
    }

    #[test]
    fn reach_for_empty_grants_yields_default() {
        let g: Vec<Grant> = vec![];
        assert_eq!(reach_for(&g, perm::PROJECT_READ), Reach::default());
    }

    /// `reach_for` trusts its caller to have filtered grants to one org: an org-scoped
    /// grant sets `org` regardless of WHICH org it names. This is the documented
    /// contract, not an oversight — pinning it here so a future change that starts
    /// comparing ids has to update this test deliberately.
    #[test]
    fn reach_for_org_arm_does_not_compare_the_org_id() {
        let other_org = Uuid::from_u128(999);
        let grants = vec![grant(Scope::Org(other_org), &[perm::PROJECT_READ])];
        assert!(reach_for(&grants, perm::PROJECT_READ).org);
    }

    #[test]
    fn grants_from_rows_parses_scopes_and_perms() {
        let rows = vec![
            (
                "org".to_string(),
                org(),
                serde_json::json!(["issue:read", "app:read"]),
            ),
            ("project".to_string(), proj_a(), serde_json::json!([])),
            (
                "app".to_string(),
                app_a1(),
                serde_json::json!(["issue:write"]),
            ),
            ("bogus".to_string(), org(), serde_json::json!(["x"])), // dropped
        ];
        let grants = grants_from_rows(rows);
        assert_eq!(grants.len(), 3); // bogus dropped
        assert!(has_permission(
            &grants,
            perm::ISSUE_READ,
            org(),
            None,
            None,
            None
        ));
        assert!(has_permission(
            &grants,
            perm::ISSUE_WRITE,
            org(),
            Some(proj_a()),
            Some(app_a1()),
            None
        ));
    }

    // --- Scope::Env and the four-level cascade --------------------------

    fn env_a1p() -> Uuid {
        Uuid::from_u128(1000)
    }
    fn env_a1s() -> Uuid {
        Uuid::from_u128(1001)
    }

    /// An app grant covers every environment under it, including ones created
    /// after the grant was written.
    #[test]
    fn app_grant_covers_every_environment_under_it() {
        let g = vec![preset_grant(Scope::App(app_a1()), &VIEWER)];
        for env in [env_a1p(), env_a1s()] {
            assert!(has_permission(
                &g,
                perm::ISSUE_READ,
                org(),
                Some(proj_a()),
                Some(app_a1()),
                Some(env)
            ));
        }
    }

    /// An env grant covers that environment only — not a sibling environment in
    /// the same app, and not the app-level check itself.
    #[test]
    fn env_grant_covers_that_environment_only() {
        let g = vec![preset_grant(Scope::Env(env_a1p()), &VIEWER)];
        assert!(has_permission(
            &g,
            perm::ISSUE_READ,
            org(),
            Some(proj_a()),
            Some(app_a1()),
            Some(env_a1p())
        ));
        // sibling environment — DENIED
        assert!(!has_permission(
            &g,
            perm::ISSUE_READ,
            org(),
            Some(proj_a()),
            Some(app_a1()),
            Some(env_a1s())
        ));
        // app-level check (env = None) — DENIED: an env grant cannot authorize an
        // app-wide action.
        assert!(!has_permission(
            &g,
            perm::ISSUE_READ,
            org(),
            Some(proj_a()),
            Some(app_a1()),
            None
        ));
        // project and org level — DENIED
        assert!(!has_permission(
            &g,
            perm::ISSUE_READ,
            org(),
            Some(proj_a()),
            None,
            None
        ));
        assert!(!has_permission(
            &g,
            perm::ISSUE_READ,
            org(),
            None,
            None,
            None
        ));
    }

    #[test]
    fn org_and_project_grants_still_reach_environments() {
        let og = vec![preset_grant(Scope::Org(org()), &DEVELOPER)];
        assert!(has_permission(
            &og,
            perm::ISSUE_WRITE,
            org(),
            Some(proj_a()),
            Some(app_a1()),
            Some(env_a1p())
        ));
        let pg = vec![preset_grant(Scope::Project(proj_a()), &DEVELOPER)];
        assert!(has_permission(
            &pg,
            perm::ISSUE_WRITE,
            org(),
            Some(proj_a()),
            Some(app_a1()),
            Some(env_a1p())
        ));
        // sibling project's app+env — DENIED
        assert!(!has_permission(
            &pg,
            perm::ISSUE_WRITE,
            org(),
            Some(proj_b()),
            Some(app_b1()),
            Some(env_a1p())
        ));
    }

    #[test]
    fn reach_for_collects_environments() {
        let g = vec![
            grant(Scope::Env(env_a1p()), &[perm::ISSUE_READ]),
            grant(Scope::Env(env_a1s()), &[perm::ISSUE_READ]),
            grant(Scope::Env(Uuid::from_u128(1002)), &[perm::EVENT_READ]),
        ];
        let reach = reach_for(&g, perm::ISSUE_READ);
        assert!(!reach.org);
        assert!(reach.projects.is_empty());
        assert!(reach.apps.is_empty());
        assert_eq!(reach.envs, vec![env_a1p(), env_a1s()]);
    }

    /// `grants_from_rows` silently drops unknown scope strings (`_ => return None`).
    /// Adding 'env' to the DB CHECK without this arm makes every environment grant
    /// vanish at read time with no signal at all. Delete the "env" arm and this
    /// test fails.
    #[test]
    fn grants_from_rows_parses_the_env_scope() {
        let rows = vec![(
            "env".to_string(),
            env_a1p(),
            serde_json::json!(["issue:read"]),
        )];
        let grants = grants_from_rows(rows);
        assert_eq!(grants.len(), 1, "env scope_type must not be dropped");
        assert_eq!(grants[0].scope, Scope::Env(env_a1p()));
    }

    #[test]
    fn scope_parts_round_trips_env() {
        assert_eq!(Scope::Env(env_a1p()).parts(), ("env", env_a1p()));
        let (scope_type, scope_id) = Scope::Env(env_a1p()).parts();
        let rows = vec![(
            scope_type.to_string(),
            scope_id,
            serde_json::json!(["issue:read"]),
        )];
        assert_eq!(grants_from_rows(rows)[0].scope, Scope::Env(env_a1p()));
    }

    // --- resolve_env_filter: the decision table --------------------------

    fn app_envs() -> Vec<Uuid> {
        vec![env_a1p(), env_a1s()]
    }

    fn resolve(grants: &[Grant], requested: EnvFilter) -> Result<EnvFilter, EnvDenied> {
        resolve_env_filter(
            grants,
            perm::ISSUE_READ,
            org(),
            proj_a(),
            app_a1(),
            &app_envs(),
            requested,
        )
    }

    // --- row 1: app-wide reach resolves exactly as it does today ---------------

    #[test]
    fn app_wide_reach_passes_every_filter_through_unchanged() {
        let g = vec![preset_grant(Scope::App(app_a1()), &VIEWER)];
        assert_eq!(resolve(&g, EnvFilter::All), Ok(EnvFilter::All));
        assert_eq!(
            resolve(&g, EnvFilter::One(env_a1p())),
            Ok(EnvFilter::One(env_a1p()))
        );
        assert_eq!(
            resolve(&g, EnvFilter::Unattributed),
            Ok(EnvFilter::Unattributed)
        );
    }

    #[test]
    fn org_and_project_reach_are_also_app_wide() {
        for g in [
            vec![preset_grant(Scope::Org(org()), &VIEWER)],
            vec![preset_grant(Scope::Project(proj_a()), &VIEWER)],
        ] {
            assert_eq!(resolve(&g, EnvFilter::All), Ok(EnvFilter::All));
            assert_eq!(
                resolve(&g, EnvFilter::Unattributed),
                Ok(EnvFilter::Unattributed)
            );
        }
    }

    /// Even with app-wide reach, an environment id that is not this app's is
    /// refused — this is the existence + ownership check `parse_env`'s doc comment
    /// has been asking for since Slice 2.
    #[test]
    fn app_wide_reach_still_refuses_a_foreign_environment_id() {
        let g = vec![preset_grant(Scope::App(app_a1()), &VIEWER)];
        let foreign = Uuid::from_u128(9999);
        assert_eq!(
            resolve(&g, EnvFilter::One(foreign)),
            Err(EnvDenied::EnvNotInApp)
        );
    }

    // --- row 2: partial reach auto-narrows ------------------------------------

    #[test]
    fn partial_reach_narrows_all_to_the_held_environments() {
        let g = vec![preset_grant(Scope::Env(env_a1p()), &VIEWER)];
        assert_eq!(
            resolve(&g, EnvFilter::All),
            Ok(EnvFilter::Subset(vec![env_a1p()]))
        );
    }

    #[test]
    fn partial_reach_with_two_environments_narrows_to_both() {
        let g = vec![
            preset_grant(Scope::Env(env_a1p()), &VIEWER),
            preset_grant(Scope::Env(env_a1s()), &VIEWER),
        ];
        match resolve(&g, EnvFilter::All) {
            Ok(EnvFilter::Subset(mut ids)) => {
                ids.sort();
                let mut want = vec![env_a1p(), env_a1s()];
                want.sort();
                assert_eq!(ids, want);
            }
            other => panic!("expected Subset of both environments, got {other:?}"),
        }
    }

    #[test]
    fn partial_reach_allows_a_held_environment_and_refuses_a_sibling() {
        let g = vec![preset_grant(Scope::Env(env_a1p()), &VIEWER)];
        assert_eq!(
            resolve(&g, EnvFilter::One(env_a1p())),
            Ok(EnvFilter::One(env_a1p()))
        );
        assert_eq!(
            resolve(&g, EnvFilter::One(env_a1s())),
            Err(EnvDenied::EnvNotGranted)
        );
    }

    /// Unattributed rows belong to no environment, so they belong to nobody's
    /// readable set. An env-scoped caller asking for them is refused, not given an
    /// empty list — "matches nothing" is not "you may not ask".
    #[test]
    fn partial_reach_refuses_unattributed() {
        let g = vec![preset_grant(Scope::Env(env_a1p()), &VIEWER)];
        assert_eq!(
            resolve(&g, EnvFilter::Unattributed),
            Err(EnvDenied::UnattributedNeedsAppReach)
        );
    }

    /// An env grant on ANOTHER app's environment must not widen this app's
    /// readable set. `reach.envs` is intersected with the app's own environments.
    #[test]
    fn an_env_grant_from_another_app_contributes_nothing() {
        let other_app_env = Uuid::from_u128(7777);
        let g = vec![preset_grant(Scope::Env(other_app_env), &VIEWER)];
        assert_eq!(resolve(&g, EnvFilter::All), Err(EnvDenied::NoReach));
    }

    // --- row 3: no reach at all ------------------------------------------------

    #[test]
    fn no_reach_is_denied_for_every_filter() {
        let g = vec![preset_grant(Scope::App(app_a2()), &VIEWER)];
        assert_eq!(resolve(&g, EnvFilter::All), Err(EnvDenied::NoReach));
        assert_eq!(
            resolve(&g, EnvFilter::One(env_a1p())),
            Err(EnvDenied::NoReach)
        );
        assert_eq!(
            resolve(&g, EnvFilter::Unattributed),
            Err(EnvDenied::NoReach)
        );
    }

    /// Holding the wrong permission is the same as holding nothing. A Viewer's
    /// env grant does not confer `issue:write`.
    #[test]
    fn a_grant_lacking_the_permission_confers_no_reach() {
        let g = vec![preset_grant(Scope::Env(env_a1p()), &VIEWER)];
        let got = resolve_env_filter(
            &g,
            perm::ISSUE_WRITE,
            org(),
            proj_a(),
            app_a1(),
            &app_envs(),
            EnvFilter::All,
        );
        assert_eq!(got, Err(EnvDenied::NoReach));
    }

    // --- effective_permissions_for_filter: the second-permission question ----
    //
    // These pin the semantics `issues::detail`/`issues::events` depend on for
    // the `source:read` gate. The bug they exist to prevent recurring is the
    // fix-round-1 one: resolving that gate at `env: None`, which no
    // environment-scoped grant can ever satisfy.

    fn perms_for(grants: &[Grant], env: &EnvFilter) -> Vec<String> {
        let mut v: Vec<String> =
            effective_permissions_for_filter(grants, org(), proj_a(), app_a1(), env)
                .into_iter()
                .collect();
        v.sort();
        v
    }

    /// The regression this whole fix round is about: an env grant MUST
    /// contribute under `One(that env)`. At `env: None` it contributes nothing,
    /// which is what made `issues::detail` 403 for an env-scoped caller.
    #[test]
    fn an_env_grant_contributes_at_its_own_environment_but_not_app_wide() {
        let g = vec![grant(
            Scope::Env(env_a1p()),
            &[perm::ISSUE_READ, perm::SOURCE_READ],
        )];
        assert_eq!(
            perms_for(&g, &EnvFilter::One(env_a1p())),
            vec![perm::ISSUE_READ.to_string(), perm::SOURCE_READ.to_string()]
        );
        // App-wide (`All`) must NOT pick it up: a source:read held on one
        // environment cannot unlock source across every other one.
        assert!(perms_for(&g, &EnvFilter::All).is_empty());
        assert!(perms_for(&g, &EnvFilter::Unattributed).is_empty());
        // And not a sibling environment either.
        assert!(perms_for(&g, &EnvFilter::One(env_a1s())).is_empty());
    }

    /// Evaluating at an environment can only ADD to the app-level answer, never
    /// subtract — `grant_applies`'s Org/Project/App arms ignore `env` entirely.
    /// An app-wide caller asking `?environment_id=X` must not lose anything.
    #[test]
    fn app_and_higher_grants_survive_being_evaluated_at_an_environment() {
        for g in [
            vec![grant(Scope::App(app_a1()), &[perm::SOURCE_READ])],
            vec![grant(Scope::Project(proj_a()), &[perm::SOURCE_READ])],
            vec![grant(Scope::Org(org()), &[perm::SOURCE_READ])],
        ] {
            for env in [
                EnvFilter::All,
                EnvFilter::Unattributed,
                EnvFilter::One(env_a1p()),
                EnvFilter::Subset(vec![env_a1p(), env_a1s()]),
            ] {
                assert_eq!(
                    perms_for(&g, &env),
                    vec![perm::SOURCE_READ.to_string()],
                    "an app-or-higher grant must hold at {env:?}"
                );
            }
        }
    }

    /// `Subset` is the INTERSECTION: a permission counts only where it is held
    /// in every environment the response could draw a row from. Held on one of
    /// the two environments is not enough — that would leak source context from
    /// the environment where the caller lacks the grant.
    #[test]
    fn subset_intersects_rather_than_unions_env_specific_permissions() {
        let g = vec![
            grant(
                Scope::Env(env_a1p()),
                &[perm::ISSUE_READ, perm::SOURCE_READ],
            ),
            // Same read permission on the sibling, but NOT source:read.
            grant(Scope::Env(env_a1s()), &[perm::ISSUE_READ]),
        ];
        let both = EnvFilter::Subset(vec![env_a1p(), env_a1s()]);
        assert_eq!(
            perms_for(&g, &both),
            vec![perm::ISSUE_READ.to_string()],
            "source:read is held on only one of the two environments, so it must \
             not survive the intersection"
        );
        // Narrowed to just the environment that does carry it, it comes back.
        assert_eq!(
            perms_for(&g, &EnvFilter::Subset(vec![env_a1p()])),
            vec![perm::ISSUE_READ.to_string(), perm::SOURCE_READ.to_string()]
        );
    }

    #[test]
    fn no_grants_confer_nothing_at_any_filter() {
        let g: Vec<Grant> = vec![];
        for env in [
            EnvFilter::All,
            EnvFilter::Unattributed,
            EnvFilter::One(env_a1p()),
            EnvFilter::Subset(vec![env_a1p()]),
        ] {
            assert!(perms_for(&g, &env).is_empty(), "{env:?}");
        }
    }
}
