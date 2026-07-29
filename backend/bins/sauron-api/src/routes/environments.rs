//! Environment management: create, rename, mute, promote, rotate, retire.
//!
//! Environments are app-scoped resources, so every check resolves through
//! `authorize_app` against the parent app. The `env:*` permissions name what is
//! being managed, not a new scope level — `Scope::Env` arrives in Slice 3.

/// Cap on a stored environment name. Was 64 when the value arrived from the
/// envelope; kept at 64 now that it is admin-supplied so existing rows stay valid.
const MAX_ENV_NAME_LEN: usize = 64;

/// The environment every new app is born with.
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

use axum::extract::{Path, Query, State};
use axum::Json;
use diesel_async::AsyncConnection;
use serde::Deserialize;
use uuid::Uuid;

use sauron_auth::{authorize_app, perm, AuthUser};
use sauron_core::ids;
use sauron_db::models::Environment;
use sauron_db::repo;
use sauron_redis::keys;

use super::db;
use crate::error::ApiError;
use crate::AppState;

#[derive(Deserialize)]
pub struct ListEnvQuery {
    #[serde(default)]
    pub include_retired: bool,
}

pub async fn list_environments(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<ListEnvQuery>,
) -> Result<Json<Vec<Environment>>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::ENV_READ).await?;
    Ok(Json(
        repo::list_environments(&mut conn, app_id, q.include_retired).await?,
    ))
}

#[derive(Deserialize)]
pub struct CreateEnvReq {
    pub name: String,
}

pub async fn create_environment(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Json(req): Json<CreateEnvReq>,
) -> Result<Json<Environment>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::ENV_CREATE).await?;
    let name = validate_env_name(&req.name).map_err(ApiError::BadRequest)?;

    if repo::count_active_environments(&mut conn, app_id).await? >= repo::MAX_ENVIRONMENTS_PER_APP {
        return Err(ApiError::Conflict(format!(
            "app already has {} environments",
            repo::MAX_ENVIRONMENTS_PER_APP
        )));
    }

    let key = ids::public_key();
    // `environments_app_name_active_key` turns a duplicate live name into a
    // unique violation; map it rather than pre-checking, so a concurrent create
    // cannot slip between the check and the insert.
    match repo::create_environment(&mut conn, app_id, name, &key, false).await {
        Ok(env) => Ok(Json(env)),
        Err(diesel::result::Error::DatabaseError(
            diesel::result::DatabaseErrorKind::UniqueViolation,
            _,
        )) => Err(ApiError::Conflict(format!(
            "an environment named \"{name}\" already exists"
        ))),
        Err(e) => Err(e.into()),
    }
}

#[derive(Deserialize)]
pub struct UpdateEnvReq {
    pub name: Option<String>,
    pub ingest_enabled: Option<bool>,
    pub is_default: Option<bool>,
}

pub async fn update_environment(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(env_id): Path<Uuid>,
    Json(req): Json<UpdateEnvReq>,
) -> Result<Json<Environment>, ApiError> {
    let mut conn = db(&state).await?;
    let (app_id, _, _) = repo::env_ancestry(&mut conn, env_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::ENV_UPDATE).await?;

    let env = repo::get_environment(&mut conn, env_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if env.retired_at.is_some() {
        return Err(ApiError::Conflict(
            "this environment is retired and can no longer be edited".into(),
        ));
    }

    // Validate everything before mutating anything. `is_default: false` is
    // meaningless (a default is moved, never unset) and must not be discovered
    // after a rename has already committed.
    if req.is_default == Some(false) {
        return Err(ApiError::BadRequest(
            "a default environment is moved, not unset — promote another environment instead"
                .into(),
        ));
    }

    let ingest_changed = req.ingest_enabled.is_some();

    let current = conn
        .transaction::<_, ApiError, _>(async |conn| {
            let mut current = env;

            if let Some(raw) = req.name.as_deref() {
                let name = validate_env_name(raw).map_err(ApiError::BadRequest)?;
                current = match repo::rename_environment(conn, env_id, name).await {
                    Ok(e) => e,
                    Err(diesel::result::Error::DatabaseError(
                        diesel::result::DatabaseErrorKind::UniqueViolation,
                        _,
                    )) => {
                        return Err(ApiError::Conflict(format!(
                            "an environment named \"{name}\" already exists"
                        )))
                    }
                    Err(e) => return Err(e.into()),
                };
            }

            if let Some(enabled) = req.ingest_enabled {
                current = repo::set_environment_ingest(conn, env_id, enabled).await?;
            }

            if req.is_default == Some(true) {
                current =
                    match repo::promote_environment_default(conn, app_id, env_id).await {
                        Ok(e) => e,
                        // Two concurrent promotions within the same app: under READ
                        // COMMITTED, the second transaction's `WHERE is_default = true`
                        // scans a snapshot that doesn't yet see the first transaction's
                        // clear, so it clears nothing and then collides with the first
                        // transaction's new default on `environments_default_key`. The
                        // transaction itself is atomic and non-corrupting — the caller
                        // just needs to retry.
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
            tracing::warn!(error = %e, env_id = %env_id, "failed to invalidate ingest key cache");
        }
    }

    Ok(Json(current))
}

pub async fn rotate_environment_key(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(env_id): Path<Uuid>,
) -> Result<Json<Environment>, ApiError> {
    let mut conn = db(&state).await?;
    let (app_id, _, _) = repo::env_ancestry(&mut conn, env_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::ENV_ROTATE_KEY).await?;

    let env = repo::get_environment(&mut conn, env_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if env.retired_at.is_some() {
        return Err(ApiError::Conflict(
            "this environment is retired and can no longer be edited".into(),
        ));
    }

    let new_key = ids::public_key();
    let updated = repo::rotate_environment_key(&mut conn, env_id, &new_key).await?;
    // Invalidate the OLD key's slot, captured before the update. `rotate-key` is
    // the platform's revocation button; a silently-failed invalidation means a
    // revoked key keeps working for the full positive-cache TTL with no signal.
    if let Err(e) = state.redis.del(&keys::dsn_cache(&env.public_key)).await {
        tracing::warn!(error = %e, env_id = %env_id, "failed to invalidate ingest key cache");
    }
    Ok(Json(updated))
}

pub async fn retire_environment(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(env_id): Path<Uuid>,
) -> Result<Json<Environment>, ApiError> {
    let mut conn = db(&state).await?;
    let (app_id, _, _) = repo::env_ancestry(&mut conn, env_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::ENV_DELETE).await?;

    // Both invariants below are read and then acted on in the same transaction,
    // starting with an app-level lock, so two concurrent env-set mutations on the
    // SAME app but DIFFERENT rows (e.g. this retire racing a promote) serialize
    // against each other rather than each locking only its own row and reading a
    // pre-commit count/default. `lock_environment_for_update` still gives this
    // handler the specific row it reads and updates below.
    let (retired, did_retire) = conn
        .transaction::<_, ApiError, _>(async |conn| {
            repo::lock_app_for_update(conn, app_id).await?;
            let env = repo::lock_environment_for_update(conn, env_id)
                .await?
                .ok_or(ApiError::NotFound)?;
            if env.retired_at.is_some() {
                return Ok((env, false)); // idempotent
            }
            // Check the count guard first: an app with exactly one live environment
            // (which is therefore necessarily the default) would otherwise report
            // "promote another one first" — impossible advice, since there is
            // nothing to promote. The more fundamental reason must win.
            if repo::count_active_environments(conn, app_id).await? <= 1 {
                return Err(ApiError::Conflict(
                    "cannot retire the last environment — an app must have somewhere to report"
                        .into(),
                ));
            }
            if env.is_default {
                return Err(ApiError::Conflict(
                    "cannot retire the default environment — promote another one first".into(),
                ));
            }
            let retired = repo::retire_environment(conn, env_id).await?;
            Ok((retired, true))
        })
        .await?;

    // Only invalidate on an actual state transition — the idempotent (already
    // retired) path already invalidated the cache the first time it retired.
    if did_retire {
        if let Err(e) = state.redis.del(&keys::dsn_cache(&retired.public_key)).await {
            tracing::warn!(error = %e, env_id = %env_id, "failed to invalidate ingest key cache");
        }
    }
    Ok(Json(retired))
}
