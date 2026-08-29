//! App management: read, update, delete, and the onboarding first-event poll.
//!
//! Key rotation and environment listing moved to `routes::environments` — the
//! ingest key now belongs to the environment, not the app.

use axum::extract::{Path, RawQuery, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use sauron_auth::{authorize_app, authorize_app_reachable, perm, AuthUser};
use sauron_db::models::App;
use sauron_db::repo;
use sauron_redis::keys;

use super::db;
use crate::error::ApiError;
use crate::openapi::{ErrorResponse, OkResponse};
use crate::AppState;

/// Uses `authorize_app_reachable`, not `authorize_app`: an env-scoped member
/// has no org/project/app grant, so `authorize_app` always 403s them here —
/// and without this app's own metadata, the dashboard has nothing to render
/// on the way to their environment. Reading the app object itself is safe to
/// widen this way; `update_app`/`delete_app` below deliberately are not.
///
/// App metadata (name, ingest_enabled) has no environment dimension, so
/// `environment_id` is rejected outright rather than silently ignored —
/// Task 14's router-enumeration test (`http_env_scoping.rs`) walks every
/// app-scoped GET and treats a `200` on a malformed `environment_id` as the
/// same defect class as the four `environment_id`-handling regressions this
/// feature has already had. This handler used to accept no query extractor
/// at all, which meant a bogus value was silently dropped instead of
/// refused.
#[utoipa::path(
    get, path = "/v1/apps/{app_id}", tag = "Apps",
    summary = "Fetch an app",
    description = "Returns the app record. Refused before the id is looked up when the caller holds no covering grant, so a 403 does not confirm the app exists.",
    params(("app_id" = Uuid, Path, description = "The app. The caller must hold a grant covering it.")),
    security(("bearerAuth" = [])),
    responses(
        (status = 200, description = "The app.", body = App),
        (status = 401, description = "Missing or invalid access token.", body = ErrorResponse),
        (status = 403, description = "No grant covers this app.", body = ErrorResponse),
        (status = 404, description = "No such app.", body = ErrorResponse),
    ),
)]
pub async fn get_app(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<App>, ApiError> {
    super::scope::reject_environment_id(
        super::scope::raw_environment_id(raw_query.as_deref()).as_deref(),
    )?;
    let mut conn = db(&state).await?;
    let app = authorize_app_reachable(&mut conn, auth.user_id, app_id, perm::APP_READ).await?;
    Ok(Json(app))
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct UpdateAppReq {
    pub name: String,
    #[serde(default = "default_true")]
    pub ingest_enabled: bool,
    /// Which environment represents the build that ships to the app stores.
    ///
    /// Absent leaves the designation alone; an explicit `null` clears it and
    /// hides the Overview store section. The double `Option` matters because
    /// every other field on this request is mandatory — without it, any PATCH
    /// that renamed an app would also silently clear the designation.
    #[serde(default, deserialize_with = "double_option_uuid")]
    pub store_environment_id: Option<Option<Uuid>>,
}

fn default_true() -> bool {
    true
}

fn double_option_uuid<'de, D>(d: D) -> Result<Option<Option<Uuid>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Some(Option::deserialize(d)?))
}

#[utoipa::path(
    patch, path = "/v1/apps/{app_id}", tag = "Apps",
    summary = "Update an app",
    description = "\
Partial update: omitted fields are left alone. `project_id` is a *nullable* \
field with three states — absent (leave as-is), `null` (detach from its \
project), or an id (move it) — which is why the request body distinguishes \
missing from null.",
    params(("app_id" = Uuid, Path, description = "The app. The caller must hold a grant covering it.")),
    security(("bearerAuth" = [])),
    request_body(content = UpdateAppReq, example = json!({ "name": "Checkout (iOS)" })),
    responses(
        (status = 200, description = "The updated app.", body = App),
        (status = 400, description = "Malformed field.", body = ErrorResponse),
        (status = 401, description = "Missing or invalid access token.", body = ErrorResponse),
        (status = 403, description = "No grant covers this app, or the target project.", body = ErrorResponse),
        (status = 404, description = "No such app.", body = ErrorResponse),
    ),
)]
pub async fn update_app(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Json(req): Json<UpdateAppReq>,
) -> Result<Json<App>, ApiError> {
    let mut conn = db(&state).await?;
    // Read the pre-update state so we only pay for cache invalidation when
    // `ingest_enabled` actually flips — the request always carries a value
    // (defaulted to `true`), so it can't be used as a "did it change" signal
    // on its own. `authorize_app` already fetches the row, so bind it instead
    // of re-querying it via `repo::get_app`.
    let existing = authorize_app(&mut conn, auth.user_id, app_id, perm::APP_UPDATE).await?;
    let (_, org_id) = repo::app_ancestry(&mut conn, app_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if req.name.trim().is_empty() {
        return Err(ApiError::BadRequest("app name is required".into()));
    }
    let ingest_changed = existing.ingest_enabled != req.ingest_enabled;

    if let Some(env) = req.store_environment_id {
        if let Some(env_id) = env {
            // Must be an enrollment OF THIS APP. Accepting any UUID would store
            // a designation that can never equal the environment switcher's
            // value, hiding the Overview section forever with no error to
            // explain why.
            if !repo::app_environment_belongs_to_app(&mut conn, env_id, app_id).await? {
                return Err(ApiError::BadRequest(
                    "store_environment_id is not an environment of this app".into(),
                ));
            }
        }
        repo::set_app_store_environment(&mut conn, app_id, env).await?;
    }

    let app = repo::update_app(&mut conn, app_id, &req.name, req.ingest_enabled)
        .await?
        .ok_or(ApiError::NotFound)?;

    if ingest_changed {
        // Cache slots are keyed by environment key, not app key — but the cached
        // EnvRef carries `app_ingest_enabled`, so a mute (or unmute) toggled at
        // app level would otherwise take up to the full 300s positive TTL to
        // bite. Drop every one of this app's environment cache slots so it takes
        // effect immediately. Retired environments are already unresolvable
        // (`find_env_by_public_key` filters `retired_at IS NULL`), so only live
        // ones need revoking — and excluding retired ones keeps them from
        // consuming this call's 500-row cap ahead of the live keys that matter.
        let envs = repo::live_app_environment_keys(&mut conn, app_id).await?;
        for (env_id, public_key) in &envs {
            if let Err(e) = state.redis.del(&keys::dsn_cache(public_key)).await {
                tracing::warn!(error = %e, env_id = %env_id, "failed to invalidate ingest key cache");
            }
        }
    }

    // `store_environment_id` is deliberately not recorded: it names an
    // enrollment id, which is not a value a reader of the trail could
    // interpret without a lookup this table is designed to avoid.
    let entry = crate::audit::with_app_scope(
        &mut conn,
        crate::audit::Entry::new(
            org_id,
            crate::audit::action::APP_UPDATE,
            crate::audit::entity::APP,
        )
        .target(app.id, &app.name)
        .changes(crate::audit::diff(
            crate::audit::entity::APP,
            &[
                (
                    "name",
                    serde_json::json!(existing.name),
                    serde_json::json!(app.name),
                ),
                (
                    "ingest_enabled",
                    serde_json::json!(existing.ingest_enabled),
                    serde_json::json!(app.ingest_enabled),
                ),
            ],
        )),
        app_id,
    )
    .await;
    crate::audit::record(&mut conn, auth.user_id, entry).await;

    Ok(Json(app))
}

#[utoipa::path(
    delete, path = "/v1/apps/{app_id}", tag = "Apps",
    summary = "Delete an app",
    description = "\
Removes the app and its enrollments. Telemetry already ingested is **not** \
deleted synchronously — use the admin purge endpoints for that, which are \
auditable and support a confirm/cancel handshake.",
    params(("app_id" = Uuid, Path, description = "The app. The caller must hold a grant covering it.")),
    security(("bearerAuth" = [])),
    responses(
        (status = 200, description = "Deleted.", body = OkResponse),
        (status = 401, description = "Missing or invalid access token.", body = ErrorResponse),
        (status = 403, description = "No grant covers this app.", body = ErrorResponse),
        (status = 404, description = "No such app.", body = ErrorResponse),
    ),
)]
pub async fn delete_app(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut conn = db(&state).await?;
    let app = authorize_app(&mut conn, auth.user_id, app_id, perm::APP_DELETE).await?;
    let (_, org_id) = repo::app_ancestry(&mut conn, app_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    // Resolved BEFORE the delete: `audit_app_scope` joins `apps` to `projects`,
    // and after the row is gone it would return None and the entry would name
    // no project at all.
    let scope = crate::audit::with_app_scope(
        &mut conn,
        crate::audit::Entry::new(
            org_id,
            crate::audit::action::APP_DELETE,
            crate::audit::entity::APP,
        )
        .target(app.id, &app.name),
        app_id,
    )
    .await;
    // The environments cascade away with the app, but their keys can still be
    // sitting in the ingest cache for the full positive TTL — during which a
    // deleted app keeps returning 202 for events that are then dropped on an FK
    // failure. Revoke every one of them. Retired environments are already
    // unresolvable (`find_env_by_public_key` filters `retired_at IS NULL`), so
    // only live ones need revoking — and excluding retired ones keeps them from
    // consuming this call's 500-row cap ahead of the live keys that matter.
    let envs = repo::live_app_environment_keys(&mut conn, app_id).await?;
    repo::delete_app(&mut conn, app_id).await?;
    for (env_id, public_key) in &envs {
        if let Err(e) = state.redis.del(&keys::dsn_cache(public_key)).await {
            tracing::warn!(error = %e, env_id = %env_id, "failed to invalidate ingest key cache");
        }
    }
    crate::audit::record(&mut conn, auth.user_id, scope).await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct FirstEventResp {
    pub received: bool,
    /// Presence flags, not counts — the onboarding poll only needs "yet?".
    pub errors: bool,
    pub events: bool,
}

// No bespoke query struct: `first_event` takes only `environment_id`, which
// comes from `RawQuery` + `scope::authorized_read_scope`, not a `Query<T>`
// extractor. See `routes::scope`'s module docs for the extractor trap this
// avoids.
//
// Left untouched by the `authorize_app_reachable` change above:
// `authorized_read_scope` already resolves through `sauron_auth::authorize_env_read`
// (`env`-aware from Task 6 on), not the strict `authorize_app`, so an
// env-scoped member's own environment was already reachable here — the
// `EnvFilter::All` request auto-narrows to `Subset([their env, ...])` the
// same way it does for `issues::list`. This handler was never on the gap
// this task closes.

#[utoipa::path(
    get, path = "/v1/apps/{app_id}/first-event", tag = "Apps",
    summary = "When this app first sent telemetry",
    description = "\
Powers the onboarding \"waiting for your first event\" state. Answers 200 with \
a null timestamp when nothing has arrived yet — not a 404, because \
\"no data yet\" is a normal state for a correctly-configured new app.",
    params(("app_id" = Uuid, Path, description = "The app. The caller must hold a grant covering it.")),
    security(("bearerAuth" = [])),
    responses(
        (status = 200, description = "The first event's timestamp, or null if none has arrived.", body = FirstEventResp),
        (status = 401, description = "Missing or invalid access token.", body = ErrorResponse),
        (status = 403, description = "No grant covers this app.", body = ErrorResponse),
    ),
)]
pub async fn first_event(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<FirstEventResp>, ApiError> {
    let mut conn = db(&state).await?;
    // Existence check only — this is polled every few seconds during onboarding,
    // and counting would scan every partition of the two largest tables.
    //
    // Gated on `APP_READ`, not `EVENT_READ` — this is the one scoped read
    // authorized on an app-level permission rather than the read permission
    // the underlying signal tables would otherwise imply, and that is
    // deliberate (onboarding polls this before the caller may hold any
    // narrower read grant at all).
    let scope = super::scope::authorized_read_scope(
        &mut conn,
        auth.user_id,
        app_id,
        perm::APP_READ,
        raw_query.as_deref(),
    )
    .await?;
    let (has_errors, has_events) = repo::app_has_events(&mut conn, scope).await?;
    Ok(Json(FirstEventResp {
        received: has_errors || has_events,
        errors: has_errors,
        events: has_events,
    }))
}
