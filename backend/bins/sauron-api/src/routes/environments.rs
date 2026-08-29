//! Environment management, split across the two levels the data model has.
//!
//! The **catalogue** (`environments`) is owned by a project: an admin defines
//! "we ship to dev, staging, production" once, and every app in the project
//! shares those names. Catalogue mutations resolve through `authorize_project`.
//!
//! The **enrollment** (`app_environments`) is one app's membership in one of
//! those environments. It holds the ingest key and the switches that are
//! genuinely per-app — whether ingest is muted, and which environment this app
//! reports to by default. Enrollment mutations resolve through `authorize_app`.
//!
//! `list_app_environments` is the one endpoint that resolves neither: it is a
//! discovery endpoint (see `routes::projects::list_apps` for the same pattern
//! one level up), so it resolves the caller's env-scoped `Reach` instead and can
//! be satisfied by an env-scoped grant that `authorize_app` never could. Env
//! grants name an *enrollment* id, which is why `Reach::envs` can be compared
//! against these rows directly.

/// Cap on a stored environment name. Was 64 when the value arrived from the
/// envelope; kept at 64 now that it is admin-supplied so existing rows stay valid.
const MAX_ENV_NAME_LEN: usize = 64;

/// The environment every new project is born with.
pub const DEFAULT_ENV_NAME: &str = "dev";

/// Trim and bounds-check an admin-supplied environment name.
pub fn validate_env_name(raw: &str) -> Result<&str, String> {
    let name = raw.trim();
    if name.is_empty() {
        return Err("environment name is required".into());
    }
    if name.chars().count() > MAX_ENV_NAME_LEN {
        return Err(format!(
            "environment name must be at most {MAX_ENV_NAME_LEN} characters"
        ));
    }
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_and_whitespace() {
        assert!(validate_env_name("").is_err());
        assert!(validate_env_name("   ").is_err());
    }

    #[test]
    fn trims_surrounding_whitespace() {
        assert_eq!(validate_env_name("  staging  ").unwrap(), "staging");
    }

    #[test]
    fn rejects_overlong_name() {
        let long = "x".repeat(MAX_ENV_NAME_LEN + 1);
        assert!(validate_env_name(&long).is_err());
        let ok = "x".repeat(MAX_ENV_NAME_LEN);
        assert!(validate_env_name(&ok).is_ok());
    }

    #[test]
    fn counts_characters_not_bytes() {
        // 64 multi-byte characters is 64 characters, not 192 bytes.
        let multibyte = "é".repeat(MAX_ENV_NAME_LEN);
        assert!(validate_env_name(&multibyte).is_ok());
    }

    #[test]
    fn default_env_is_dev() {
        assert_eq!(DEFAULT_ENV_NAME, "dev");
    }
}

use std::collections::HashSet;

use axum::extract::{Path, Query, RawQuery, State};
use axum::Json;
use diesel_async::AsyncConnection;
use serde::Deserialize;
use uuid::Uuid;

use sauron_auth::rbac::{grants_from_rows, reach_for};
use sauron_auth::{authorize_app, authorize_project, perm, AuthError, AuthUser};
use sauron_core::ids;
use sauron_db::models::{AppEnvironment, AppEnvironmentView, Environment, NewAppEnvironment};
use sauron_db::repo;
use sauron_redis::keys;

use super::db;
use crate::error::ApiError;
use crate::openapi::ErrorResponse;
use crate::AppState;

#[derive(Deserialize, utoipa::IntoParams)]
pub struct ListEnvQuery {
    #[serde(default)]
    pub include_retired: bool,
}

/// Which environment a newly created app should treat as its default.
///
/// Deterministic and deliberately the same preference order migration 000026
/// used when it had to pick a default for every existing app: `production`
/// first because an app seeded with it is actively reporting there, then `dev`,
/// then alphabetically. Returning `None` is possible only for a project whose
/// every environment is retired, which the create paths prevent.
fn pick_default_env(envs: &[Environment]) -> Option<Uuid> {
    envs.iter()
        .min_by_key(|e| {
            (
                e.name != "production",
                e.name != DEFAULT_ENV_NAME,
                e.name.clone(),
            )
        })
        .map(|e| e.id)
}

/// Build the enrollment rows that put `app_id` into every environment in
/// `envs`, minting one key per row.
///
/// Keys are generated here rather than in `sauron-db` so that every key in the
/// system comes from `ids::public_key()`. The keys are returned alongside so the
/// caller can keep them alive for the borrow in `NewAppEnvironment`.
fn enrollment_keys(envs: &[Environment]) -> Vec<String> {
    envs.iter().map(|_| ids::public_key()).collect()
}

fn enrollment_rows<'a>(
    app_id: Uuid,
    envs: &[Environment],
    keys: &'a [String],
    default_env: Option<Uuid>,
) -> Vec<NewAppEnvironment<'a>> {
    envs.iter()
        .zip(keys)
        .map(|(env, key)| NewAppEnvironment {
            app_id,
            environment_id: env.id,
            public_key: key,
            is_default: Some(env.id) == default_env,
        })
        .collect()
}

/// Enroll a brand-new app in every live environment of its project.
///
/// Called from the app-create path. Runs inside the caller's transaction: an app
/// with no enrollment holds no ingest key and is unreachable by any SDK, so a
/// partial result here must not survive.
pub async fn enroll_new_app(
    conn: &mut diesel_async::AsyncPgConnection,
    app_id: Uuid,
    project_id: Uuid,
) -> Result<Vec<AppEnvironment>, ApiError> {
    let envs = repo::list_project_environments(conn, project_id, false).await?;
    if envs.is_empty() {
        // Every project is created with `dev`, and the last live environment
        // cannot be retired, so this is unreachable rather than merely unlikely.
        // Failing loudly beats minting an app that no SDK can ever reach.
        return Err(ApiError::Conflict(
            "project has no live environments to enroll this app in".into(),
        ));
    }
    let default_env = pick_default_env(&envs);
    let keys = enrollment_keys(&envs);
    let rows = enrollment_rows(app_id, &envs, &keys, default_env);
    Ok(repo::create_app_environments(conn, &rows).await?)
}

// ---------------------------------------------------------------------------
// Catalogue: environments as a project defines them
// ---------------------------------------------------------------------------

#[utoipa::path(
    get, path = "/v1/projects/{project_id}/environments", tag = "Environments",
    summary = "List a project's environment catalogue",
    description = "\
The **catalogue** entries (production, staging, ...), not the per-app \
enrollments. Telemetry is posted against an enrollment; see \
`GET /v1/apps/{app_id}/environments` for those.",
    params(("project_id" = Uuid, Path, description = "The project."), ListEnvQuery), security(("bearerAuth" = [])),
    responses((status = 200, description = "Catalogue environments.", body = Vec<Environment>), (status = 401, description = "Missing or invalid access token.", body = ErrorResponse),
              (status = 403, description = "No grant covers this project.", body = ErrorResponse)),
)]
pub async fn list_project_environments(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(project_id): Path<Uuid>,
    Query(q): Query<ListEnvQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<Vec<Environment>>, ApiError> {
    // This endpoint IS the environment list; scoping the request that enumerates
    // environments to one of them is circular, so `environment_id` is rejected
    // rather than silently ignored — the same defect class `http_env_scoping.rs`
    // guards against on every other scoped GET.
    super::scope::reject_environment_id(
        super::scope::raw_environment_id(raw_query.as_deref()).as_deref(),
    )?;
    let mut conn = db(&state).await?;
    // Unlike the per-app list below, this is the settings/admin view of the
    // catalogue, so it takes the ordinary project-scoped check. A member with
    // only an app- or env-scoped grant populates their picker from
    // `list_app_environments`, which is the list that actually carries their
    // keys and reflects what that app is enrolled in.
    authorize_project(&mut conn, auth.user_id, project_id, perm::ENV_READ).await?;
    let envs = repo::list_project_environments(&mut conn, project_id, q.include_retired).await?;
    Ok(Json(envs))
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct CreateEnvReq {
    pub name: String,
}

#[utoipa::path(
    post, path = "/v1/projects/{project_id}/environments", tag = "Environments",
    summary = "Add an environment to the catalogue",
    description = "Creates the catalogue entry and enrolls every app in the project, so each app gains a fresh ingest key for it.",
    params(("project_id" = Uuid, Path, description = "The project.")), security(("bearerAuth" = [])),
    request_body(content = CreateEnvReq, example = json!({ "name": "staging" })),
    responses(
        (status = 200, description = "The created environment.", body = Environment),
        (status = 400, description = "Name empty, too long, or not in the accepted character set.", body = ErrorResponse),
        (status = 401, description = "Missing or invalid access token.", body = ErrorResponse), (status = 403, description = "No grant covers this project.", body = ErrorResponse),
        (status = 409, description = "An environment of that name already exists here.", body = ErrorResponse),
    ),
)]
pub async fn create_project_environment(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(project_id): Path<Uuid>,
    Json(req): Json<CreateEnvReq>,
) -> Result<Json<Environment>, ApiError> {
    let mut conn = db(&state).await?;
    let project = authorize_project(&mut conn, auth.user_id, project_id, perm::ENV_CREATE).await?;
    let name = validate_env_name(&req.name).map_err(ApiError::BadRequest)?;

    if repo::count_active_project_environments(&mut conn, project_id).await?
        >= repo::MAX_ENVIRONMENTS_PER_PROJECT
    {
        return Err(ApiError::Conflict(format!(
            "project already has {} environments",
            repo::MAX_ENVIRONMENTS_PER_PROJECT
        )));
    }

    // Catalogue row and enrollments commit together. An environment that exists
    // in the catalogue but that no app is enrolled in is invisible to every SDK
    // while still occupying its name, so a half-applied create would look like a
    // working environment that silently drops everything sent to it.
    let env = conn
        .transaction::<_, ApiError, _>(async |conn| {
            // `environments_project_name_active_key` turns a duplicate live name
            // into a unique violation; map it rather than pre-checking, so a
            // concurrent create cannot slip between the check and the insert.
            let env = match repo::create_project_environment(conn, project_id, name).await {
                Ok(env) => env,
                Err(diesel::result::Error::DatabaseError(
                    diesel::result::DatabaseErrorKind::UniqueViolation,
                    _,
                )) => {
                    return Err(ApiError::Conflict(format!(
                        "an environment named \"{name}\" already exists in this project"
                    )))
                }
                Err(e) => return Err(e.into()),
            };

            // Fan out to every app in the project. `is_default` is false for all
            // of them: each app already has its default, and
            // `app_environments_default_key` would reject a second.
            let app_ids = repo::app_ids_in_project(conn, project_id).await?;
            let keys: Vec<String> = app_ids.iter().map(|_| ids::public_key()).collect();
            let rows: Vec<NewAppEnvironment> = app_ids
                .iter()
                .zip(&keys)
                .map(|(app_id, key)| NewAppEnvironment {
                    app_id: *app_id,
                    environment_id: env.id,
                    public_key: key,
                    is_default: false,
                })
                .collect();
            repo::create_app_environments(conn, &rows).await?;
            Ok(env)
        })
        .await?;

    // After the transaction, never inside it: a swallowed audit error raised
    // within `conn.transaction` would still poison it and roll the whole
    // create back, which is the opposite of `audit::record`'s fail-open
    // contract. The enrollment keys minted above are deliberately not
    // recorded — see `audit::FORBIDDEN_FIELDS`.
    crate::audit::record(
        &mut conn,
        auth.user_id,
        crate::audit::Entry::new(
            project.org_id,
            crate::audit::action::ENV_CREATE,
            crate::audit::entity::ENVIRONMENT,
        )
        .target(env.id, &env.name)
        .project(project.id, &project.name)
        .environment(env.id, &env.name)
        .changes(crate::audit::created(
            crate::audit::entity::ENVIRONMENT,
            &[("name", serde_json::json!(env.name))],
        )),
    )
    .await;

    Ok(Json(env))
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct UpdateProjectEnvReq {
    pub name: Option<String>,
}

#[utoipa::path(
    patch, path = "/v1/environments/{env_id}", tag = "Environments",
    summary = "Rename a catalogue environment",
    description = "\
A rename keeps the same identity, so existing enrollments and their keys keep \
working and historical telemetry stays attributed to it.",
    params(("env_id" = Uuid, Path, description = "Catalogue environment id (NOT an enrollment id).")), security(("bearerAuth" = [])),
    request_body(content = UpdateProjectEnvReq),
    responses(
        (status = 200, description = "The updated environment.", body = Environment),
        (status = 400, description = "Invalid name.", body = ErrorResponse),
        (status = 401, description = "Missing or invalid access token.", body = ErrorResponse), (status = 403, description = "No grant covers this environment.", body = ErrorResponse),
        (status = 404, description = "No such environment.", body = ErrorResponse),
        (status = 409, description = "That name is taken in this project.", body = ErrorResponse),
    ),
)]
pub async fn update_project_environment(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(env_id): Path<Uuid>,
    Json(req): Json<UpdateProjectEnvReq>,
) -> Result<Json<Environment>, ApiError> {
    let mut conn = db(&state).await?;
    let (project_id, _) = repo::project_env_ancestry(&mut conn, env_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let project = authorize_project(&mut conn, auth.user_id, project_id, perm::ENV_UPDATE).await?;

    let env = repo::get_project_environment(&mut conn, env_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if env.retired_at.is_some() {
        return Err(ApiError::Conflict(
            "this environment is retired and can no longer be edited".into(),
        ));
    }

    let Some(raw) = req.name.as_deref() else {
        return Ok(Json(env));
    };
    let name = validate_env_name(raw).map_err(ApiError::BadRequest)?;
    // No cache invalidation: the name is not part of `EnvRef`, and renaming
    // changes no key and no ingest switch. Every enrollment keeps resolving.
    let previous_name = env.name.clone();
    match repo::rename_project_environment(&mut conn, env_id, name).await {
        Ok(renamed) => {
            crate::audit::record(
                &mut conn,
                auth.user_id,
                crate::audit::Entry::new(
                    project.org_id,
                    crate::audit::action::ENV_UPDATE,
                    crate::audit::entity::ENVIRONMENT,
                )
                .target(renamed.id, &renamed.name)
                .project(project.id, &project.name)
                .environment(renamed.id, &renamed.name)
                .changes(crate::audit::diff(
                    crate::audit::entity::ENVIRONMENT,
                    &[(
                        "name",
                        serde_json::json!(previous_name),
                        serde_json::json!(renamed.name),
                    )],
                )),
            )
            .await;
            Ok(Json(renamed))
        }
        Err(diesel::result::Error::DatabaseError(
            diesel::result::DatabaseErrorKind::UniqueViolation,
            _,
        )) => Err(ApiError::Conflict(format!(
            "an environment named \"{name}\" already exists in this project"
        ))),
        Err(e) => Err(e.into()),
    }
}

#[utoipa::path(
    delete, path = "/v1/environments/{env_id}", tag = "Environments",
    summary = "Retire a catalogue environment",
    description = "\
Retires rather than deletes: its enrollments stop accepting telemetry, and \
already-ingested data stays queryable and attributed. Use the admin purge \
endpoints to actually remove data.",
    params(("env_id" = Uuid, Path, description = "Catalogue environment id (NOT an enrollment id).")), security(("bearerAuth" = [])),
    responses(
        (status = 200, description = "The retired environment.", body = Environment),
        (status = 401, description = "Missing or invalid access token.", body = ErrorResponse), (status = 403, description = "No grant covers this environment.", body = ErrorResponse),
        (status = 404, description = "No such environment.", body = ErrorResponse),
    ),
)]
pub async fn retire_project_environment(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(env_id): Path<Uuid>,
) -> Result<Json<Environment>, ApiError> {
    let mut conn = db(&state).await?;
    let (project_id, _) = repo::project_env_ancestry(&mut conn, env_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let project = authorize_project(&mut conn, auth.user_id, project_id, perm::ENV_DELETE).await?;

    // Both invariants are read and acted on in one transaction that starts with a
    // project-level lock, so two concurrent retires of DIFFERENT environments in
    // the same project serialize rather than each reading a pre-commit count.
    let (retired, keys, did_retire) = conn
        .transaction::<_, ApiError, _>(async |conn| {
            repo::lock_project_for_update(conn, project_id).await?;
            let env = repo::get_project_environment(conn, env_id)
                .await?
                .ok_or(ApiError::NotFound)?;
            if env.retired_at.is_some() {
                return Ok((env, Vec::new(), false)); // idempotent
            }
            // Count guard first: a project with exactly one live environment
            // would otherwise be told "promote another one first" — impossible
            // advice, since there is nothing to promote. The more fundamental
            // reason must win.
            if repo::count_active_project_environments(conn, project_id).await? <= 1 {
                return Err(ApiError::Conflict(
                    "cannot retire the last environment — apps must have somewhere to report"
                        .into(),
                ));
            }
            let defaulting = repo::apps_defaulting_to_environment(conn, env_id).await?;
            if defaulting > 0 {
                return Err(ApiError::Conflict(format!(
                    "{defaulting} app(s) still default to this environment — promote another one for them first"
                )));
            }
            let (retired, keys) = repo::retire_project_environment(conn, env_id).await?;
            Ok((retired, keys, true))
        })
        .await?;

    // Only invalidate on an actual state transition — the idempotent path already
    // invalidated the first time it retired. Every enrollment's key must stop
    // resolving now that its row is retired, and a silently-failed invalidation
    // leaves a revoked key working for the full positive-cache TTL.
    if did_retire {
        for key in &keys {
            if let Err(e) = state.redis.del(&keys::dsn_cache(key)).await {
                tracing::warn!(error = %e, env_id = %env_id, "failed to invalidate ingest key cache");
            }
        }
        // Gated on the real state transition, for the same reason the cache
        // invalidation above is: the idempotent re-retire changed nothing, and
        // recording it would fill the trail with events that never happened.
        crate::audit::record(
            &mut conn,
            auth.user_id,
            crate::audit::Entry::new(
                project.org_id,
                crate::audit::action::ENV_RETIRE,
                crate::audit::entity::ENVIRONMENT,
            )
            .target(retired.id, &retired.name)
            .project(project.id, &project.name)
            .environment(retired.id, &retired.name)
            .changes(crate::audit::diff(
                crate::audit::entity::ENVIRONMENT,
                &[(
                    "retired_at",
                    serde_json::Value::Null,
                    serde_json::json!(retired.retired_at),
                )],
            )),
        )
        .await;
    }
    Ok(Json(retired))
}

// ---------------------------------------------------------------------------
// Enrollment: one app's membership in one environment
// ---------------------------------------------------------------------------

#[utoipa::path(
    get, path = "/v1/apps/{app_id}/environments", tag = "Environments",
    summary = "List an app's environment enrollments",
    description = "\
The enrollments telemetry is actually posted against. Each carries the public \
ingest key for one (app, environment) pair.

**These ids are what `?environment_id=` takes** on the analytics routes — \
passing a catalogue id there is refused.",
    params(("app_id" = Uuid, Path, description = "The app."), ListEnvQuery), security(("bearerAuth" = [])),
    responses((status = 200, description = "Enrollments with their public keys.", body = Vec<AppEnvironmentView>), (status = 401, description = "Missing or invalid access token.", body = ErrorResponse),
              (status = 403, description = "No grant covers this app.", body = ErrorResponse)),
)]
pub async fn list_app_environments(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<ListEnvQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<Vec<AppEnvironmentView>>, ApiError> {
    // This endpoint IS the environment list; scoping the request that
    // enumerates environments to one of them is circular, so `environment_id`
    // is rejected rather than silently ignored. Task 14's router-enumeration
    // test (`http_env_scoping.rs`) treats a `200` on a malformed
    // `environment_id` on ANY app-scoped GET as the same defect class as the
    // four `environment_id`-handling regressions this feature has already
    // had — this handler used to accept no such query at all (unknown query
    // params are dropped by `axum::extract::Query`'s deserializer, not
    // rejected), so a bogus value was silently swallowed instead of refused.
    super::scope::reject_environment_id(
        super::scope::raw_environment_id(raw_query.as_deref()).as_deref(),
    )?;
    let mut conn = db(&state).await?;
    // Same shape as `list_apps`, one level down: `authorize_app` gates on a
    // fixed (org, project, app, None) target that an env-scoped grant can
    // never satisfy, so an env-scoped member would 403 from the very endpoint
    // that populates their environment picker.
    let (project_id, org_id) = repo::app_ancestry(&mut conn, app_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let rows = repo::user_grants_in_org(&mut conn, auth.user_id, org_id).await?;
    if rows.is_empty() {
        return Err(ApiError::Auth(AuthError::Forbidden));
    }
    let grants = grants_from_rows(rows);
    let reach = reach_for(&grants, perm::ENV_READ);
    let all = repo::list_app_environments(&mut conn, app_id, q.include_retired).await?;
    if reach.org || reach.projects.contains(&project_id) || reach.apps.contains(&app_id) {
        return Ok(Json(all));
    }

    // `reach.envs` holds enrollment ids, which is exactly what these rows are
    // keyed by — an env grant names one app's enrollment, never the catalogue
    // entry, so this comparison cannot leak a sibling app's environment.
    let allowed: HashSet<Uuid> = reach.envs.into_iter().collect();
    let mine: Vec<AppEnvironmentView> = all
        .into_iter()
        .filter(|e| allowed.contains(&e.enrollment.id))
        .collect();
    if mine.is_empty() {
        return Err(ApiError::Auth(AuthError::Forbidden));
    }
    Ok(Json(mine))
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct UpdateAppEnvReq {
    pub ingest_enabled: Option<bool>,
    pub is_default: Option<bool>,
}

#[utoipa::path(
    patch, path = "/v1/app-environments/{id}", tag = "Environments",
    summary = "Update an enrollment",
    description = "Enable or disable an enrollment. A disabled enrollment rejects telemetry without deleting its key.",
    params(("id" = Uuid, Path, description = "App-environment *enrollment* id — the id an SDK DSN carries.")), security(("bearerAuth" = [])),
    request_body(content = UpdateAppEnvReq),
    responses(
        (status = 200, description = "The updated enrollment.", body = AppEnvironment),
        (status = 400, description = "Malformed field.", body = ErrorResponse),
        (status = 401, description = "Missing or invalid access token.", body = ErrorResponse), (status = 403, description = "No grant covers this enrollment.", body = ErrorResponse),
        (status = 404, description = "No such enrollment.", body = ErrorResponse),
    ),
)]
pub async fn update_app_environment(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateAppEnvReq>,
) -> Result<Json<AppEnvironment>, ApiError> {
    let mut conn = db(&state).await?;
    let (app_id, _, org_id) = repo::env_ancestry(&mut conn, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::ENV_UPDATE).await?;

    let env = repo::get_app_environment(&mut conn, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if env.retired_at.is_some() {
        return Err(ApiError::Conflict(
            "this environment is retired and can no longer be edited".into(),
        ));
    }

    // Validate everything before mutating anything. `is_default: false` is
    // meaningless (a default is moved, never unset) and must not be discovered
    // after an ingest toggle has already committed.
    if req.is_default == Some(false) {
        return Err(ApiError::BadRequest(
            "a default environment is moved, not unset — promote another environment instead"
                .into(),
        ));
    }

    let ingest_changed = req.ingest_enabled.is_some();
    // Captured before the transaction consumes `env`. These are the diff's
    // "from" side; `audit::diff` drops any that did not actually move, so a
    // no-op PATCH records an entry with an empty `changes` rather than a
    // fictitious one.
    let (was_ingest_enabled, was_default) = (env.ingest_enabled, env.is_default);

    let current = conn
        .transaction::<_, ApiError, _>(async |conn| {
            let mut current = env;

            if let Some(enabled) = req.ingest_enabled {
                current = repo::set_app_environment_ingest(conn, id, enabled).await?;
            }

            if req.is_default == Some(true) {
                current =
                    match repo::promote_app_environment_default(conn, app_id, id).await {
                        Ok(e) => e,
                        // Two concurrent promotions within the same app: under READ
                        // COMMITTED, the second transaction's `WHERE is_default = true`
                        // scans a snapshot that doesn't yet see the first transaction's
                        // clear, so it clears nothing and then collides with the first
                        // transaction's new default on `app_environments_default_key`.
                        // The transaction itself is atomic and non-corrupting — the
                        // caller just needs to retry.
                        Err(diesel::result::Error::DatabaseError(
                            diesel::result::DatabaseErrorKind::UniqueViolation,
                            _,
                        )) => return Err(ApiError::Conflict(
                            "another environment was just promoted to default for this app — please retry"
                                .into(),
                        )),
                        Err(e) => return Err(e.into()),
                    };
            }

            Ok(current)
        })
        .await?;

    if ingest_changed {
        // The cached EnvRef carries the ingest flags, so it must be dropped —
        // but only now that the transaction above is known to have committed.
        // Use the post-commit row's key, not one captured before the transaction:
        // a concurrent rotate could commit in between, in which case the
        // pre-transaction key names a dead slot while the new key's slot may
        // already have cached `ingest_enabled: true` from before this update.
        if let Err(e) = state.redis.del(&keys::dsn_cache(&current.public_key)).await {
            tracing::warn!(error = %e, env_id = %id, "failed to invalidate ingest key cache");
        }
    }

    // The enrollment carries no name of its own — it lives on the catalogue row
    // this enrollment points at.
    let env_name = repo::get_project_environment(&mut conn, current.environment_id)
        .await
        .ok()
        .flatten()
        .map(|e| e.name)
        .unwrap_or_default();

    // `entity_id` is the ENROLLMENT (that is what changed); `environment_id` is
    // the CATALOGUE entry. They are deliberately different ids. The Wall's
    // environment filter is org-wide, and a user filtering for "staging" means
    // the catalogue environment across every app — an enrollment id would match
    // exactly one app and silently hide the rest.
    let entry = crate::audit::with_app_scope(
        &mut conn,
        crate::audit::Entry::new(
            org_id,
            crate::audit::action::ENV_ENROLLMENT_UPDATE,
            crate::audit::entity::ENVIRONMENT,
        )
        .target(current.id, &env_name)
        .environment(current.environment_id, &env_name)
        .changes(crate::audit::diff(
            crate::audit::entity::ENVIRONMENT,
            &[
                (
                    "ingest_enabled",
                    serde_json::json!(was_ingest_enabled),
                    serde_json::json!(current.ingest_enabled),
                ),
                (
                    "is_default",
                    serde_json::json!(was_default),
                    serde_json::json!(current.is_default),
                ),
            ],
        )),
        app_id,
    )
    .await;
    crate::audit::record(&mut conn, auth.user_id, entry).await;

    Ok(Json(current))
}

#[utoipa::path(
    post, path = "/v1/app-environments/{id}/rotate-key", tag = "Environments",
    summary = "Rotate an enrollment's ingest key",
    description = "\
Issues a new public key and **invalidates the old one immediately**. Every \
deployed SDK still carrying the previous DSN stops being accepted at once, so \
ship the new key before rotating.

The key is write-only and non-secret by design — it can identify an \
environment and accept telemetry, and can read nothing.",
    params(("id" = Uuid, Path, description = "App-environment *enrollment* id — the id an SDK DSN carries.")), security(("bearerAuth" = [])),
    responses(
        (status = 200, description = "The enrollment with its new key.", body = AppEnvironment),
        (status = 401, description = "Missing or invalid access token.", body = ErrorResponse), (status = 403, description = "No grant covers this enrollment.", body = ErrorResponse),
        (status = 404, description = "No such enrollment.", body = ErrorResponse),
    ),
)]
pub async fn rotate_app_environment_key(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<AppEnvironment>, ApiError> {
    let mut conn = db(&state).await?;
    let (app_id, _, org_id) = repo::env_ancestry(&mut conn, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::ENV_ROTATE_KEY).await?;

    let env = repo::get_app_environment(&mut conn, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if env.retired_at.is_some() {
        return Err(ApiError::Conflict(
            "this environment is retired and can no longer be edited".into(),
        ));
    }

    let new_key = ids::public_key();
    let updated = repo::rotate_app_environment_key(&mut conn, id, &new_key).await?;
    // Invalidate the OLD key's slot, captured before the update. `rotate-key` is
    // the platform's revocation button; a silently-failed invalidation means a
    // revoked key keeps working for the full positive-cache TTL with no signal.
    if let Err(e) = state.redis.del(&keys::dsn_cache(&env.public_key)).await {
        tracing::warn!(error = %e, env_id = %id, "failed to invalidate ingest key cache");
    }

    let env_name = repo::get_project_environment(&mut conn, updated.environment_id)
        .await
        .ok()
        .flatten()
        .map(|e| e.name)
        .unwrap_or_default();
    // `changes` is deliberately EMPTY. This is the platform's revocation
    // button, so that it happened, to which enrollment, and by whom is the
    // whole record — recording either key would put a live ingest credential
    // into a table org admins can read and that is never pruned.
    let entry = crate::audit::with_app_scope(
        &mut conn,
        crate::audit::Entry::new(
            org_id,
            crate::audit::action::ENV_ROTATE_KEY,
            crate::audit::entity::ENVIRONMENT,
        )
        .target(updated.id, &env_name)
        .environment(updated.environment_id, &env_name),
        app_id,
    )
    .await;
    crate::audit::record(&mut conn, auth.user_id, entry).await;

    Ok(Json(updated))
}
