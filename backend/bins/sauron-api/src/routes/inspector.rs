//! PII inspector: policies, scans, findings, reveal, masking and the audit
//! trail. Gated on `pii:read` / `pii:manage`, which Owner and Admin hold and
//! Developer and Viewer deliberately do not — `pii:read` is bulk PII
//! disclosure and `pii:manage` is irreversible bulk destruction, and neither
//! should be inherited by the role every engineer gets by default.
//!
//! There is no `authorize_env` in this product and this module does not invent
//! one: `require_permission`/`effective_at` have no env parameter and always
//! resolve with `env: None`, so an env-scoped grant can never satisfy them. An
//! `app_env`-scoped POLICY is therefore authorized at its PARENT APP — a
//! member holding `pii:manage` on one environment only cannot edit that
//! environment's policy. Same documented gap `orgs::delete_grant` carries.

use axum::extract::{Path, Query, State};
use axum::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use sauron_auth::rbac::{grants_from_rows, reach_for};
use sauron_auth::{authorize_app, authorize_org, authorize_project, perm, AuthUser};
// No `NewInspectorScan` here on purpose: scans are only ever created through
// `repo::enqueue_scan_for_policy`, so the API cannot freeze a scan the
// scheduler would have frozen differently.
use sauron_db::models::{InspectorPolicyPatch, NewInspectorPolicy};
use sauron_db::repo;
use sauron_inspector::path::finding_path_to_mask_path;
use sauron_inspector::targets::{expand_targets, validate_target, MaskTarget};
use sauron_inspector::{detect, matching};

use super::db;
use crate::error::ApiError;
use crate::openapi::ErrorResponse;
use crate::AppState;

/// The one message every `/v1/apps/{app_id}/inspector/*` route rejects
/// `environment_id` with. Findings carry their own environment dimension in
/// the payload and masking is app-scoped, so one consistent rule beats one
/// exception.
pub(crate) const ENV_SCOPE_MESSAGE: &str =
    "the inspector is app-scoped; masking cannot be limited to one environment";

/// Ceiling on any list endpoint here. Findings and audit rows are both
/// unbounded in principle.
const MAX_LIMIT: i64 = 500;

fn clamp_limit(raw: Option<i64>) -> i64 {
    raw.unwrap_or(100).clamp(1, MAX_LIMIT)
}

/// Resolve a policy row to the scope its permission is checked at.
///
/// `app_env` authorizes at the PARENT APP (see the module header). The
/// enrollment id is resolved to its app rather than refused, so an
/// environment-scoped policy is still manageable by whoever manages the app.
async fn authorize_policy(
    conn: &mut sauron_db::AsyncPgConnection,
    user_id: Uuid,
    target_type: &str,
    target_id: Uuid,
    permission: &str,
) -> Result<(), ApiError> {
    match target_type {
        "project" => {
            authorize_project(conn, user_id, target_id, permission).await?;
            Ok(())
        }
        // authorize_app, NEVER authorize_app_reachable: the latter is
        // read-only by explicit contract, and an env-scoped grant must not see
        // app-wide findings.
        "app" => {
            authorize_app(conn, user_id, target_id, permission).await?;
            Ok(())
        }
        "app_env" => {
            let app_id = repo::app_id_for_enrollment(conn, target_id)
                .await
                .map_err(|e| ApiError::Internal(e.to_string()))?
                .ok_or(ApiError::NotFound)?;
            authorize_app(conn, user_id, app_id, permission).await?;
            Ok(())
        }
        _ => Err(ApiError::BadRequest("unknown policy target type".into())),
    }
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct CreatePolicyReq {
    pub target_type: String,
    pub target_id: Uuid,
    #[serde(default)]
    pub tracked_keys: Value,
    #[serde(default)]
    pub detectors: Value,
    #[serde(default)]
    pub scan_columns: Option<Value>,
    #[serde(default)]
    pub rollups: Option<Value>,
    #[serde(default)]
    pub window_days: Option<i32>,
    #[serde(default)]
    pub schedule_enabled: Option<bool>,
    #[serde(default)]
    pub schedule_days: Option<i16>,
    /// `HH:MM` local wall clock.
    #[serde(default)]
    pub schedule_time: Option<String>,
    #[serde(default)]
    pub schedule_tz: Option<String>,
}

/// Normalize and validate the two matcher fields together.
///
/// A policy with NEITHER tracked keys NOR detectors is rejected with 400.
/// Without that, the single most likely first configuration — "I don't know my
/// payload shape, turn on the email detector" — combined with the prefilter
/// being built only from the key list produces a scan that reads zero rows and
/// finishes `succeeded`, `coverage='full'`, zero findings. A confident false
/// negative on a privacy scan is the worst thing this feature can emit.
fn normalize_matchers(keys_in: &Value, dets_in: &Value) -> Result<(Value, Value), ApiError> {
    let keys = matching::parse_tracked_keys(keys_in);
    let dets = detect::parse_detectors(dets_in);
    if keys.is_empty() && dets.is_empty() {
        return Err(ApiError::BadRequest(
            "a policy needs at least one tracked key or one detector; \
             a policy with neither scans nothing and reports a false negative"
                .into(),
        ));
    }
    // Keys are lowercased at write so the stored row and the matcher agree.
    let keys_json = serde_json::to_value(&keys).map_err(|e| ApiError::Internal(e.to_string()))?;
    let dets_json = Value::Array(dets.iter().map(|d| json!(d.id())).collect());
    Ok((keys_json, dets_json))
}

fn parse_hhmm(raw: &str) -> Result<chrono::NaiveTime, ApiError> {
    chrono::NaiveTime::parse_from_str(raw, "%H:%M")
        .or_else(|_| chrono::NaiveTime::parse_from_str(raw, "%H:%M:%S"))
        .map_err(|_| ApiError::BadRequest("schedule_time must be HH:MM".into()))
}

#[utoipa::path(
    post, path = "/v1/orgs/{org_id}/inspector/policies", tag = "Inspector",
    summary = "Create a privacy-inspector policy",
    description = "\
A policy declares which payload keys count as sensitive and where to look.

**A policy with no matchers is refused.** A matcher-less policy would scan \
successfully, report `coverage: full` and zero findings — a confident false \
negative, and the worst thing this feature can emit.",
    params(("org_id" = Uuid, Path, description = "The organization.")), security(("bearerAuth" = [])),
    request_body(content = CreatePolicyReq),
    responses((status = 200, description = "The created policy.", body = serde_json::Value),
              (status = 400, description = "No matchers, or a malformed matcher.", body = ErrorResponse), (status = 401, description = "Missing or invalid access token.", body = ErrorResponse), (status = 403, description = "No grant covers this scope.", body = ErrorResponse)),
)]
pub async fn create_policy(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(org_id): Path<Uuid>,
    Query(env): Query<super::scope::RejectEnvQuery>,
    Json(req): Json<CreatePolicyReq>,
) -> Result<Json<Value>, ApiError> {
    super::scope::reject_environment_id(env.environment_id.as_deref())?;
    let mut conn = db(&state).await?;
    authorize_org(&mut conn, auth.user_id, org_id, perm::PII_MANAGE).await?;

    if !matches!(req.target_type.as_str(), "project" | "app" | "app_env") {
        return Err(ApiError::BadRequest(
            "target_type must be project, app or app_env".into(),
        ));
    }
    // `target_id` has NO foreign key, so without this any authenticated user
    // can mint an org where they hold org:manage (POST /v1/orgs requires only
    // AuthUser), POST a policy naming a victim's app_id, and have the worker
    // scan the victim's error_events into rows carrying the ATTACKER's org_id
    // — which is exactly what every list query filters on. 404, not 403, so it
    // is not an existence oracle.
    if !repo::validate_scope_in_org(&mut conn, org_id, &req.target_type, req.target_id).await? {
        return Err(ApiError::NotFound);
    }

    let (keys, dets) = normalize_matchers(&req.tracked_keys, &req.detectors)?;
    let tz = req.schedule_tz.unwrap_or_else(|| "UTC".to_string());
    if !repo::timezone_is_valid(&mut conn, &tz).await {
        return Err(ApiError::BadRequest(format!("unknown timezone {tz:?}")));
    }
    let time = match req.schedule_time.as_deref() {
        Some(s) => parse_hhmm(s)?,
        None => chrono::NaiveTime::from_hms_opt(3, 0, 0).unwrap(),
    };
    let days = req.schedule_days.unwrap_or(0);
    if !(0..=127).contains(&days) {
        return Err(ApiError::BadRequest(
            "schedule_days is a 0..127 weekday bitmask".into(),
        ));
    }
    let window_days = req.window_days.unwrap_or(30);
    if !(1..=400).contains(&window_days) {
        return Err(ApiError::BadRequest(
            "window_days must be between 1 and 400".into(),
        ));
    }
    let rollups = req
        .rollups
        .unwrap_or_else(|| json!(["issues", "event_users"]));

    let policy = repo::create_inspector_policy(
        &mut conn,
        NewInspectorPolicy {
            org_id,
            target_type: &req.target_type,
            target_id: req.target_id,
            enabled: true,
            tracked_keys: &keys,
            detectors: &dets,
            scan_columns: req.scan_columns.as_ref(),
            rollups: &rollups,
            window_days,
            schedule_enabled: req.schedule_enabled.unwrap_or(false),
            schedule_days: days,
            schedule_time: time,
            schedule_tz: &tz,
            created_by: Some(auth.user_id),
        },
    )
    .await
    .map_err(|e| match e {
        diesel::result::Error::DatabaseError(
            diesel::result::DatabaseErrorKind::UniqueViolation,
            _,
        ) => ApiError::Conflict("a policy already exists for this target".into()),
        other => ApiError::Internal(other.to_string()),
    })?;

    // Called after EVERY schedule-field write so `next_run_at` is never stale.
    repo::reschedule_policy(&mut conn, policy.id).await?;
    let fresh = repo::get_inspector_policy(&mut conn, policy.id).await?;

    crate::audit::record(
        &mut conn,
        auth.user_id,
        crate::audit::Entry::new(
            org_id,
            crate::audit::action::INSPECTOR_POLICY_CREATE,
            crate::audit::entity::INSPECTOR_POLICY,
        )
        .target(policy.id, &req.target_type)
        .changes(crate::audit::created(
            crate::audit::entity::INSPECTOR_POLICY,
            &[("enabled", json!(policy.enabled))],
        )),
    )
    .await;
    Ok(Json(json!(fresh)))
}

/// Org-level policy LIST. Deliberately NOT `authorize_org`.
///
/// A fixed-scope check can never be satisfied by a narrower grant — the
/// historical 403-for-scoped-members bug. This is the house discovery pattern:
/// load the caller's grants, 403 on empty, compute their reach for `pii:read`,
/// and filter, lifting env grants to their app.
#[utoipa::path(
    get, path = "/v1/orgs/{org_id}/inspector/policies", tag = "Inspector",
    summary = "List privacy-inspector policies",
    params(("org_id" = Uuid, Path, description = "The organization."), ListLimit), security(("bearerAuth" = [])),
    responses((status = 200, description = "Policies.", body = serde_json::Value), (status = 401, description = "Missing or invalid access token.", body = ErrorResponse), (status = 403, description = "No grant covers this scope.", body = ErrorResponse)),
)]
pub async fn list_policies(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(org_id): Path<Uuid>,
    Query(env): Query<super::scope::RejectEnvQuery>,
) -> Result<Json<Value>, ApiError> {
    super::scope::reject_environment_id(env.environment_id.as_deref())?;
    let mut conn = db(&state).await?;
    let rows = repo::user_grants_in_org(&mut conn, auth.user_id, org_id).await?;
    if rows.is_empty() {
        return Err(ApiError::Forbidden("no grants in this organization".into()));
    }
    let grants = grants_from_rows(rows);
    let reach = reach_for(&grants, perm::PII_READ);
    if !reach.org && reach.projects.is_empty() && reach.apps.is_empty() && reach.envs.is_empty() {
        return Err(ApiError::Forbidden("pii:read is required".into()));
    }
    let all = repo::list_inspector_policies_for_org(&mut conn, org_id).await?;

    // Matching the caller's reach against a policy needs BOTH ancestries, not
    // just the caller's. Comparing bare ids made every arm below narrower than
    // the `authorize_policy` that guards `GET /v1/inspector/policies/{id}`, so
    // a policy could 200 by id and be absent from this list — verified over
    // HTTP for an app-scoped member and an `app_env` policy under that app.
    //
    // That gap is not cosmetic. An `app_env` policy SUBTRACTS from a coarser
    // policy's scan targets (`sauron_inspector::targets::resolve_targets`), so
    // a member who sees app A's policy but not the environment-level policy
    // under it reads A's findings as covering the whole app when they do not —
    // the confident-false-picture failure this feature exists to prevent.
    //
    // Two batched queries, both over deduped id sets: the caller's env grants
    // (lifted UP to their app) and the policies' own `app`/`app_env` targets
    // (so a coarser grant matches DOWN onto a narrower policy).
    let mut env_ids: Vec<Uuid> = reach.envs.clone();
    env_ids.extend(
        all.iter()
            .filter(|p| p.target_type == "app_env")
            .map(|p| p.target_id),
    );
    env_ids.sort_unstable();
    env_ids.dedup();

    let mut app_ids: Vec<Uuid> = all
        .iter()
        .filter(|p| p.target_type == "app")
        .map(|p| p.target_id)
        .collect();
    app_ids.sort_unstable();
    app_ids.dedup();

    // `(env, app, project, org)` and `(app, project, org)`.
    let env_anc = repo::env_ancestries(&mut conn, &env_ids).await?;
    let app_anc = repo::app_ancestries(&mut conn, &app_ids).await?;

    // The apps the caller's ENV grants sit under. An env grant cannot satisfy
    // `authorize_app` (`grant_applies` compares `Scope::Env` against the
    // check's `env`, which `authorize_app` passes as `None`), so this lift is
    // deliberately WIDER than `authorize_policy` — it keeps an env-scoped
    // member able to see their app's policy exists. Same lift
    // `projects::list_apps` performs one level down.
    let reach_env_apps: Vec<Uuid> = env_anc
        .iter()
        .filter(|(env_id, ..)| reach.envs.contains(env_id))
        .map(|(_, app_id, _, _)| *app_id)
        .collect();

    let visible: Vec<_> = all
        .iter()
        .filter(|p| {
            if reach.org {
                return true;
            }
            match p.target_type.as_str() {
                // NOT widened. `authorize_project` resolves at `(org, project,
                // None, None)`, which no app- or env-scoped grant can satisfy,
                // so anything broader here would list rows that 403 on open.
                "project" => reach.projects.contains(&p.target_id),

                // `authorize_app` accepts org, PARENT-PROJECT or app scope.
                "app" => {
                    reach.apps.contains(&p.target_id)
                        || app_anc.iter().any(|(app_id, project_id, _)| {
                            *app_id == p.target_id && reach.projects.contains(project_id)
                        })
                        || reach_env_apps.contains(&p.target_id)
                }

                // `authorize_policy`'s `app_env` arm resolves the enrollment to
                // its parent app and calls `authorize_app`, so the SAME three
                // scopes admit it — plus a grant on the enrollment itself.
                "app_env" => env_anc.iter().any(|(env_id, app_id, project_id, _)| {
                    *env_id == p.target_id
                        && (reach.envs.contains(env_id)
                            || reach.apps.contains(app_id)
                            || reach.projects.contains(project_id))
                }),

                _ => false,
            }
        })
        .collect();
    Ok(Json(json!(visible)))
}

#[utoipa::path(
    get, path = "/v1/inspector/policies/{policy_id}", tag = "Inspector",
    summary = "Fetch a policy",
    params(("policy_id" = Uuid, Path, description = "The inspector policy.")), security(("bearerAuth" = [])),
    responses((status = 200, description = "The policy.", body = serde_json::Value), (status = 401, description = "Missing or invalid access token.", body = ErrorResponse), (status = 403, description = "No grant covers this scope.", body = ErrorResponse), (status = 404, description = "No such policy.", body = ErrorResponse)),
)]
pub async fn get_policy(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = db(&state).await?;
    let p = repo::get_inspector_policy(&mut conn, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    authorize_policy(
        &mut conn,
        auth.user_id,
        &p.target_type,
        p.target_id,
        perm::PII_READ,
    )
    .await?;
    Ok(Json(json!(p)))
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct PatchPolicyReq {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub tracked_keys: Option<Value>,
    #[serde(default)]
    pub detectors: Option<Value>,
    #[serde(default)]
    pub scan_columns: Option<Value>,
    #[serde(default)]
    pub rollups: Option<Value>,
    #[serde(default)]
    pub window_days: Option<i32>,
    #[serde(default)]
    pub schedule_enabled: Option<bool>,
    #[serde(default)]
    pub schedule_days: Option<i16>,
    #[serde(default)]
    pub schedule_time: Option<String>,
    #[serde(default)]
    pub schedule_tz: Option<String>,
}

#[utoipa::path(
    patch, path = "/v1/inspector/policies/{policy_id}", tag = "Inspector",
    summary = "Update a policy",
    description = "Same matcher validation as creation: an edit that would leave the policy with no matchers is refused.",
    params(("policy_id" = Uuid, Path, description = "The inspector policy.")), security(("bearerAuth" = [])),
    request_body(content = PatchPolicyReq),
    responses((status = 200, description = "The updated policy.", body = serde_json::Value),
              (status = 400, description = "The edit would leave no matchers, or a matcher is malformed.", body = ErrorResponse),
              (status = 401, description = "Missing or invalid access token.", body = ErrorResponse), (status = 403, description = "No grant covers this scope.", body = ErrorResponse), (status = 404, description = "No such policy.", body = ErrorResponse)),
)]
pub async fn patch_policy(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<PatchPolicyReq>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = db(&state).await?;
    let existing = repo::get_inspector_policy(&mut conn, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    authorize_policy(
        &mut conn,
        auth.user_id,
        &existing.target_type,
        existing.target_id,
        perm::PII_MANAGE,
    )
    .await?;
    // Re-validated on every PATCH as well as create: grants outlive targets.
    if !repo::validate_scope_in_org(
        &mut conn,
        existing.org_id,
        &existing.target_type,
        existing.target_id,
    )
    .await?
    {
        return Err(ApiError::NotFound);
    }

    // The two matcher fields are validated TOGETHER, against the merge of the
    // request and the stored row — patching only `detectors` to `[]` on a
    // policy with no keys must be refused, not silently accepted.
    let keys_in = req
        .tracked_keys
        .clone()
        .unwrap_or(existing.tracked_keys.clone());
    let dets_in = req.detectors.clone().unwrap_or(existing.detectors.clone());
    let (keys, dets) = normalize_matchers(&keys_in, &dets_in)?;

    if let Some(tz) = req.schedule_tz.as_deref() {
        if !repo::timezone_is_valid(&mut conn, tz).await {
            return Err(ApiError::BadRequest(format!("unknown timezone {tz:?}")));
        }
    }
    if let Some(d) = req.schedule_days {
        if !(0..=127).contains(&d) {
            return Err(ApiError::BadRequest(
                "schedule_days is a 0..127 weekday bitmask".into(),
            ));
        }
    }
    if let Some(w) = req.window_days {
        if !(1..=400).contains(&w) {
            return Err(ApiError::BadRequest(
                "window_days must be between 1 and 400".into(),
            ));
        }
    }
    let time = match req.schedule_time.as_deref() {
        Some(s) => Some(parse_hhmm(s)?),
        None => None,
    };
    let now = chrono::Utc::now();
    let patched = repo::patch_inspector_policy(
        &mut conn,
        id,
        InspectorPolicyPatch {
            enabled: req.enabled,
            tracked_keys: Some(&keys),
            detectors: Some(&dets),
            scan_columns: req.scan_columns.as_ref().map(Some),
            rollups: req.rollups.as_ref(),
            window_days: req.window_days,
            schedule_enabled: req.schedule_enabled,
            schedule_days: req.schedule_days,
            schedule_time: time,
            schedule_tz: req.schedule_tz.as_deref(),
            updated_at: Some(now),
        },
    )
    .await?
    .ok_or(ApiError::NotFound)?;
    repo::reschedule_policy(&mut conn, patched.id).await?;
    let fresh = repo::get_inspector_policy(&mut conn, id).await?;

    crate::audit::record(
        &mut conn,
        auth.user_id,
        crate::audit::Entry::new(
            patched.org_id,
            crate::audit::action::INSPECTOR_POLICY_UPDATE,
            crate::audit::entity::INSPECTOR_POLICY,
        )
        .target(patched.id, &patched.target_type)
        .changes(crate::audit::diff(
            crate::audit::entity::INSPECTOR_POLICY,
            &[("enabled", json!(existing.enabled), json!(patched.enabled))],
        )),
    )
    .await;
    Ok(Json(json!(fresh)))
}

/// Delete a policy, including one whose target is already gone.
///
/// The orphan case is the reason for the branch. `authorize_policy` resolves
/// through the target — `authorize_app` for `app`, and for `app_env` an
/// enrollment lookup first — so once the app is deleted every arm answers 404
/// and the row becomes UNDELETABLE while still being listed by
/// `list_policies`. `repo::delete_app`/`delete_project` and the reaper now stop
/// that happening, but a row already on disk, or one orphaned by a route
/// neither covers, still needs a way out.
///
/// The fallback is `authorize_org(PII_MANAGE)`, not a free pass: `PII_MANAGE`
/// is the same permission the normal path demands, only checked at the org the
/// policy row itself names. It cannot widen anything — a policy whose target
/// still resolves never reaches this branch.
#[utoipa::path(
    delete, path = "/v1/inspector/policies/{policy_id}", tag = "Inspector",
    summary = "Delete a policy",
    params(("policy_id" = Uuid, Path, description = "The inspector policy.")), security(("bearerAuth" = [])),
    responses((status = 200, description = "Deleted.", body = serde_json::Value), (status = 401, description = "Missing or invalid access token.", body = ErrorResponse), (status = 403, description = "No grant covers this scope.", body = ErrorResponse), (status = 404, description = "No such policy.", body = ErrorResponse)),
)]
pub async fn delete_policy(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let mut conn = db(&state).await?;
    let p = repo::get_inspector_policy(&mut conn, id)
        .await?
        .ok_or(ApiError::NotFound)?;

    if repo::validate_scope_in_org(&mut conn, p.org_id, &p.target_type, p.target_id).await? {
        authorize_policy(
            &mut conn,
            auth.user_id,
            &p.target_type,
            p.target_id,
            perm::PII_MANAGE,
        )
        .await?;
    } else {
        authorize_org(&mut conn, auth.user_id, p.org_id, perm::PII_MANAGE).await?;
    }

    repo::delete_inspector_policy(&mut conn, id).await?;

    crate::audit::record(
        &mut conn,
        auth.user_id,
        crate::audit::Entry::new(
            p.org_id,
            crate::audit::action::INSPECTOR_POLICY_DELETE,
            crate::audit::entity::INSPECTOR_POLICY,
        )
        .target(p.id, &p.target_type)
        .changes(crate::audit::diff(
            crate::audit::entity::INSPECTOR_POLICY,
            &[("enabled", json!(p.enabled), Value::Null)],
        )),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

/// The policy that actually governs this app, for the app picker.
///
/// Also reports the enforcement latency the pipeline really uses, so the UI
/// states a number rather than hardcoding "30 seconds" — the key lives in
/// `sauron.env` precisely so the API and the enforcer cannot diverge.
#[utoipa::path(
    get, path = "/v1/apps/{app_id}/inspector/policy", tag = "Inspector",
    summary = "Effective policy for an app",
    description = "The policy that actually applies to this app after org-level inheritance is resolved.",
    params(("app_id" = Uuid, Path, description = "The app.")), security(("bearerAuth" = [])),
    responses((status = 200, description = "The effective policy.", body = serde_json::Value), (status = 401, description = "Missing or invalid access token.", body = ErrorResponse), (status = 403, description = "No grant covers this scope.", body = ErrorResponse)),
)]
pub async fn effective_policy(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(env): Query<super::scope::RejectEnvQuery>,
) -> Result<Json<Value>, ApiError> {
    super::scope::reject_environment_id_with_message(
        env.environment_id.as_deref(),
        ENV_SCOPE_MESSAGE,
    )?;
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::PII_READ).await?;
    let policy = repo::effective_policy_for_app(&mut conn, app_id).await?;
    let masked_keys = repo::list_masked_keys(&mut conn, app_id).await?;
    Ok(Json(json!({
        "policy": policy,
        "masked_keys": masked_keys,
        "enforcement_latency_secs": state.cfg.inspector_policy_cache_secs,
        "hot_window_days": state.cfg.tier_hot_days,
    })))
}

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListLimit {
    #[serde(default)]
    pub limit: Option<i64>,
}

#[utoipa::path(
    get, path = "/v1/inspector/policies/{policy_id}/scans", tag = "Inspector",
    summary = "List a policy's scans",
    params(("policy_id" = Uuid, Path, description = "The inspector policy."), ListLimit), security(("bearerAuth" = [])),
    responses((status = 200, description = "Scans, newest first.", body = serde_json::Value), (status = 401, description = "Missing or invalid access token.", body = ErrorResponse), (status = 403, description = "No grant covers this scope.", body = ErrorResponse), (status = 404, description = "No such policy.", body = ErrorResponse)),
)]
pub async fn list_scans(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(policy_id): Path<Uuid>,
    Query(q): Query<ListLimit>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = db(&state).await?;
    let p = repo::get_inspector_policy(&mut conn, policy_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    authorize_policy(
        &mut conn,
        auth.user_id,
        &p.target_type,
        p.target_id,
        perm::PII_READ,
    )
    .await?;
    let rows = repo::list_scans_for_policy(&mut conn, policy_id, clamp_limit(q.limit)).await?;
    Ok(Json(json!(rows)))
}

/// Queue a manual scan.
///
/// The 409 comes from the partial unique index `inspector_scans_active_key`,
/// not from a handler pre-check: two clients racing must produce one scan, and
/// a check-then-insert cannot promise that.
#[utoipa::path(
    post, path = "/v1/inspector/policies/{policy_id}/scans", tag = "Inspector",
    summary = "Start a scan",
    description = "Asynchronous — returns the queued scan. Poll `GET /v1/inspector/scans/{scan_id}` for progress and `coverage`, which states whether the scan saw everything it intended to.",
    params(("policy_id" = Uuid, Path, description = "The inspector policy.")), security(("bearerAuth" = [])),
    responses((status = 200, description = "The queued scan.", body = serde_json::Value), (status = 401, description = "Missing or invalid access token.", body = ErrorResponse), (status = 403, description = "No grant covers this scope.", body = ErrorResponse), (status = 404, description = "No such policy.", body = ErrorResponse),
              (status = 409, description = "A scan for this policy is already running.", body = ErrorResponse)),
)]
pub async fn start_scan(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(policy_id): Path<Uuid>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let mut conn = db(&state).await?;
    let p = repo::get_inspector_policy(&mut conn, policy_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    authorize_policy(
        &mut conn,
        auth.user_id,
        &p.target_type,
        p.target_id,
        perm::PII_MANAGE,
    )
    .await?;
    if !repo::validate_scope_in_org(&mut conn, p.org_id, &p.target_type, p.target_id).await? {
        return Err(ApiError::NotFound);
    }

    // ONE enqueue, shared with the scheduler. Re-deriving `params`, `targets`
    // and `units_total` here is how a manual scan comes to walk environments
    // a narrower disabled policy excluded, scan a table list that omits every
    // rollup, and record `units_total = 0` so the progress bar never moves.
    // Every one of those is invisible until someone reads a finding set and
    // trusts it.
    match repo::enqueue_scan_for_policy(&mut conn, &state.cfg, &p, "manual", Some(auth.user_id))
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
    {
        repo::EnqueueOutcome::Queued(scan) => Ok((StatusCode::ACCEPTED, Json(json!(scan)))),
        repo::EnqueueOutcome::AlreadyActive => {
            let active = repo::active_scan_for_policy(&mut conn, policy_id).await?;
            Err(ApiError::Conflict(format!(
                "a scan is already queued or running for this policy (id {})",
                active.map(|s| s.id.to_string()).unwrap_or_default()
            )))
        }
        repo::EnqueueOutcome::NoMatchers => Err(ApiError::BadRequest(
            "this policy has neither tracked keys nor detectors; it would report a false negative"
                .into(),
        )),
        repo::EnqueueOutcome::TargetGone => Err(ApiError::NotFound),
        repo::EnqueueOutcome::FullySubtracted => Err(ApiError::BadRequest(
            "every app and environment under this policy is covered by a more specific policy; \
             there is nothing left for it to scan"
                .into(),
        )),
    }
}

#[utoipa::path(
    get, path = "/v1/inspector/scans/{scan_id}", tag = "Inspector",
    summary = "Fetch a scan",
    description = "Read `coverage` before trusting a zero-finding result: a partial scan with no findings is not the same claim as a full one.",
    params(("scan_id" = Uuid, Path, description = "The scan.")), security(("bearerAuth" = [])),
    responses((status = 200, description = "The scan with its state and coverage.", body = serde_json::Value), (status = 401, description = "Missing or invalid access token.", body = ErrorResponse), (status = 403, description = "No grant covers this scope.", body = ErrorResponse), (status = 404, description = "No such scan.", body = ErrorResponse)),
)]
pub async fn get_scan(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(scan_id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = db(&state).await?;
    let s = repo::get_inspector_scan(&mut conn, scan_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let p = repo::get_inspector_policy(&mut conn, s.policy_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    authorize_policy(
        &mut conn,
        auth.user_id,
        &p.target_type,
        p.target_id,
        perm::PII_READ,
    )
    .await?;
    Ok(Json(json!(s)))
}

#[utoipa::path(
    post, path = "/v1/inspector/scans/{scan_id}/cancel", tag = "Inspector",
    summary = "Cancel a running scan",
    description = "Findings already recorded are kept, and coverage reflects that the scan stopped early.",
    params(("scan_id" = Uuid, Path, description = "The scan.")), security(("bearerAuth" = [])),
    responses((status = 200, description = "Cancelled.", body = serde_json::Value), (status = 401, description = "Missing or invalid access token.", body = ErrorResponse), (status = 403, description = "No grant covers this scope.", body = ErrorResponse), (status = 404, description = "No such scan.", body = ErrorResponse),
              (status = 409, description = "The scan has already finished.", body = ErrorResponse)),
)]
pub async fn cancel_scan(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(scan_id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = db(&state).await?;
    let s = repo::get_inspector_scan(&mut conn, scan_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let p = repo::get_inspector_policy(&mut conn, s.policy_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    // PII_MANAGE, not the group's PII_READ: inheriting the read permission
    // would let every audit reader block a queued scan.
    authorize_policy(
        &mut conn,
        auth.user_id,
        &p.target_type,
        p.target_id,
        perm::PII_MANAGE,
    )
    .await?;
    let n = repo::request_scan_cancel(&mut conn, scan_id).await?;
    if n == 0 {
        return Err(ApiError::Conflict("this scan is already finished".into()));
    }
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct FindingsQuery {
    #[serde(default)]
    pub limit: Option<i64>,
    /// Keyset position: the previous page's last `(match_count, id)`.
    #[serde(default)]
    pub after_count: Option<i64>,
    #[serde(default)]
    pub after_id: Option<Uuid>,
    #[serde(default)]
    pub format: Option<String>,
}

/// Page size for the buffered CSV export.
///
/// `repo::list_findings_for_scan` clamps its own `limit` argument to 1..=1000,
/// so asking it for `total` rows in one call answers with a 1000-row PREFIX of
/// any larger scan — silently, with a 200 and a friendly filename. That is
/// exactly the truncation the ceiling check refuses a few lines below, arriving
/// by the other door, so the export walks the same keyset the UI pages with
/// instead of trusting one oversized limit.
const CSV_PAGE: i64 = 1_000;

/// Build a buffered CSV response.
///
/// The CORS layer needs `.expose_headers([CONTENT_DISPOSITION])` for the
/// split-origin topology the product ships, or the browser cannot read the
/// filename — S4 added that line; this route only depends on it.
fn csv_response(filename: &str, body: String) -> Response {
    (
        [
            (CONTENT_TYPE, "text/csv; charset=utf-8".to_string()),
            (
                CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            ),
        ],
        body,
    )
        .into_response()
}

#[utoipa::path(
    get, path = "/v1/inspector/scans/{scan_id}/findings", tag = "Inspector",
    summary = "List a scan's findings",
    description = "Findings are **redacted by default** — the matched value is not included. Use the reveal endpoint, which is audited, to see one.",
    params(("scan_id" = Uuid, Path, description = "The scan."), FindingsQuery), security(("bearerAuth" = [])),
    responses((status = 200, description = "Findings, values redacted.", body = serde_json::Value), (status = 401, description = "Missing or invalid access token.", body = ErrorResponse), (status = 403, description = "No grant covers this scope.", body = ErrorResponse), (status = 404, description = "No such scan.", body = ErrorResponse)),
)]
pub async fn list_findings(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(scan_id): Path<Uuid>,
    Query(q): Query<FindingsQuery>,
) -> Result<Response, ApiError> {
    let mut conn = db(&state).await?;
    let scan = repo::get_inspector_scan(&mut conn, scan_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let policy = repo::get_inspector_policy(&mut conn, scan.policy_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    authorize_policy(
        &mut conn,
        auth.user_id,
        &policy.target_type,
        policy.target_id,
        perm::PII_READ,
    )
    .await?;

    if q.format.as_deref() == Some("csv") {
        let total = repo::count_findings_for_scan(&mut conn, scan_id).await?;
        // A buffered export cannot be truncated honestly, so refuse rather
        // than silently ship a prefix of the answer.
        if total > state.cfg.inspector_export_max_rows {
            return Err(ApiError::BadRequest(format!(
                "too_many_rows: {total} findings exceeds INSPECTOR_EXPORT_MAX_ROWS \
                 ({}); narrow the scan or raise the ceiling",
                state.cfg.inspector_export_max_rows
            )));
        }
        let mut out = String::new();
        crate::csv::write_row(
            &mut out,
            &[
                "finding_id",
                "scan_id",
                "detected_at",
                "app_id",
                "environment_id",
                "env_scope",
                "table",
                "column",
                "json_path",
                "matched_key",
                "detector",
                "match_count",
                "match_count_exact",
                "first_seen_at",
                "last_seen_at",
                "partition_kind",
                "value_type",
            ],
        );
        let mut after: Option<(i64, Uuid)> = None;
        loop {
            let rows = repo::list_findings_for_scan(&mut conn, scan_id, CSV_PAGE, after).await?;
            let Some(last) = rows.last() else { break };
            after = Some((last.match_count, last.id));
            let final_page = (rows.len() as i64) < CSV_PAGE;
            for f in &rows {
                // The formula-injection guard applies to `json_path` and
                // `matched_key` too, not only to free text: both are
                // DEV-CONTROLLED BYTES, so a key literally named `=cmd|'...'` is a
                // spreadsheet payload.
                crate::csv::write_row(
                    &mut out,
                    &[
                        &f.id.to_string(),
                        &f.scan_id.to_string(),
                        &f.created_at.to_rfc3339(),
                        &f.app_id.to_string(),
                        &f.environment_id.map(|e| e.to_string()).unwrap_or_default(),
                        &f.env_scope,
                        &f.source_table,
                        &f.source_column,
                        &f.key_path,
                        &f.matched_key,
                        &f.detector,
                        &f.match_count.to_string(),
                        &f.match_count_exact.to_string(),
                        &f.first_seen_at.map(|t| t.to_rfc3339()).unwrap_or_default(),
                        &f.last_seen_at.map(|t| t.to_rfc3339()).unwrap_or_default(),
                        &f.partition_kind,
                        &f.value_type,
                    ],
                );
            }
            if final_page {
                break;
            }
        }
        let name = format!(
            "sauron-inspector-findings_{}_{}.csv",
            scan_id,
            scan.window_to.format("%Y-%m-%d")
        );
        return Ok(csv_response(&name, out));
    }

    let after = match (q.after_count, q.after_id) {
        (Some(c), Some(i)) => Some((c, i)),
        _ => None,
    };
    let rows =
        repo::list_findings_for_scan(&mut conn, scan_id, clamp_limit(q.limit), after).await?;
    Ok(Json(json!({
        "findings": rows,
        "coverage": scan.coverage,
        "coverage_note": scan.coverage_note,
        // Non-dismissible in the UI. The phase-1 prefilter greps the JSON TEXT
        // for the quoted key name, so a key serialized with a unicode escape
        // evades it, as does anything inside a base64 or URL-encoded blob.
        "detection_caveat": "Detection is best-effort, not a compliance guarantee. \
                             Keys hidden by unicode escapes, base64 or URL encoding are not found.",
    }))
    .into_response())
}

/// The ONLY place a raw value is ever produced.
///
/// POST rather than GET so the identifier does not land in access logs and so
/// the audit row has a request body to record. The audit row is written BEFORE
/// the value is returned, so a failure to audit is a failure to reveal.
#[utoipa::path(
    post, path = "/v1/inspector/findings/{finding_id}/reveal", tag = "Inspector",
    summary = "Reveal a finding's matched value — audited",
    description = "\
Returns the actual matched text, which is by definition suspected personal \
data. **Every reveal writes an audit record** naming the caller.

Answers 410 rather than 404 when the underlying partition has since been tiered \
or dropped, so a caller can tell \"aged out\" from \"no such finding\".",
    params(("finding_id" = Uuid, Path, description = "The finding.")),
    security(("bearerAuth" = [])),
    responses((status = 200, description = "The revealed value.", body = serde_json::Value), (status = 401, description = "Missing or invalid access token.", body = ErrorResponse), (status = 403, description = "No grant covers this scope.", body = ErrorResponse), (status = 404, description = "No such finding.", body = ErrorResponse),
              (status = 410, description = "The data has aged out of hot storage.", body = ErrorResponse)),
)]
pub async fn reveal_finding(
    auth: AuthUser,
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Path(finding_id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = db(&state).await?;
    let f = repo::get_inspector_finding(&mut conn, finding_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    authorize_app(&mut conn, auth.user_id, f.app_id, perm::PII_READ).await?;

    // `stacktrace_symbolicated` frames carry context_line/pre_context/
    // post_context — verbatim customer source — which `strip_source_context`
    // removes from RESPONSES only when the caller lacks `source:read`. A
    // pii:read holder without source:read could otherwise track the key
    // `pre_context`, reveal, and receive de-obfuscated proprietary source.
    let entry =
        sauron_inspector::columns::find(&f.source_table, &f.source_column).ok_or_else(|| {
            ApiError::BadRequest("this finding's column is not in the inventory".into())
        })?;
    if !entry.reveal_ok {
        return Err(ApiError::BadRequest(format!(
            "{}.{} is not reveal-eligible; the redacted preview is all this endpoint returns",
            f.source_table, f.source_column
        )));
    }
    let Some(row_id) = f.sample_row_id else {
        return Err(ApiError::NotFound);
    };

    let source = crate::routes::auth::client_addr(&headers, &peer, &state);
    let email = repo::user_email(&mut conn, auth.user_id)
        .await?
        .unwrap_or_default();
    repo::insert_reveal_audit(
        &mut conn,
        sauron_db::models::NewInspectorRevealAudit {
            app_id: f.app_id,
            org_id: f.org_id,
            finding_id: Some(f.id),
            user_id: Some(auth.user_id),
            user_email: &email,
            source_table: &f.source_table,
            source_column: &f.source_column,
            key_path: &f.key_path,
            request_source: &source,
        },
    )
    .await?;

    let value = repo::reveal_one_value(
        &mut conn,
        entry.table,
        entry.column,
        row_id,
        f.sample_occurred_at,
        f.app_id,
    )
    .await?;
    // 410 when the row is absent — its partition was dropped by `sauron-tier`,
    // or a rollup row was replaced. Also 410 on an app_id mismatch, so an
    // attribution bug becomes a benign miss rather than a cross-tenant
    // disclosure.
    let Some(doc) = value else {
        return Err(ApiError::Gone(
            "the row this finding points at is gone".into(),
        ));
    };

    // Extract exactly the one key_path in Rust. Nothing is persisted.
    let mut cur = &doc;
    // An EMPTY key_path is not a degenerate case, it is every TEXT column:
    // the scanner synthesizes `Leaf { path: "", key: <column name> }` for a
    // `to_jsonb(text)` scalar, so `error_events.title`, `culprit`, `message`,
    // `exception_value`, `transactions.url` and the rest all land here. Walking
    // `"".split('.')` would look up the key `""` in a JSON string, miss, and
    // answer 410 "the path no longer exists" for the whole class of columns the
    // Issues list actually renders — after having written the audit row.
    if !f.key_path.is_empty() {
        for seg in f.key_path.split('.') {
            let seg = seg.strip_suffix("[]").unwrap_or(seg);
            match cur.get(seg) {
                Some(next) => cur = next,
                None => {
                    return Err(ApiError::Gone(
                        "the path no longer exists in this row".into(),
                    ))
                }
            }
        }
    }
    Ok(Json(json!({
        "path": f.key_path,
        "value": cur,
        "type": sauron_inspector::redact::value_type(cur),
    })))
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct MaskPreviewReq {
    /// Preferred form: derive the targets from a finding, so the paths the
    /// scanner actually saw are the paths the mask writes.
    #[serde(default)]
    pub finding_id: Option<Uuid>,
    /// Explicit form, for a target an admin knows about without a scan.
    #[serde(default)]
    pub targets: Option<Vec<MaskTarget>>,
}

/// Start a counting pass. Returns 202 and an id the dashboard polls.
///
/// The count is NOT run here. `col #> path IS NOT NULL` over an app's hot
/// window is a Parallel Append seq scan — 184 ms per 210k rows measured — with
/// no index that can serve it, since the tags GIN is `jsonb_path_ops` and
/// answers `@>` only. Running that on the API's 16-connection pool is how the
/// whole dashboard goes down.
#[utoipa::path(
    post, path = "/v1/apps/{app_id}/inspector/mask-preview", tag = "Inspector",
    summary = "Preview what a mask would change",
    description = "Counts what a mask action would rewrite, without changing anything. Masking is irreversible, so preview first.",
    params(("app_id" = Uuid, Path, description = "The app.")), security(("bearerAuth" = [])),
    request_body(content = MaskPreviewReq),
    responses((status = 200, description = "Estimated impact.", body = serde_json::Value),
              (status = 400, description = "Malformed target.", body = ErrorResponse), (status = 401, description = "Missing or invalid access token.", body = ErrorResponse), (status = 403, description = "No grant covers this scope.", body = ErrorResponse)),
)]
pub async fn mask_preview(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(env): Query<super::scope::RejectEnvQuery>,
    Json(req): Json<MaskPreviewReq>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    super::scope::reject_environment_id_with_message(
        env.environment_id.as_deref(),
        ENV_SCOPE_MESSAGE,
    )?;
    let mut conn = db(&state).await?;
    let app = authorize_app(&mut conn, auth.user_id, app_id, perm::PII_MANAGE).await?;

    let (mut base, finding_id, scan_id) = match (req.finding_id, req.targets) {
        (Some(fid), _) => {
            let f = repo::get_inspector_finding(&mut conn, fid)
                .await?
                .ok_or(ApiError::NotFound)?;
            // Both `finding_id` and `scan_id` are validated against `app_id`
            // here, at preview: the audit row outlives finding pruning through
            // ON DELETE SET NULL, so this is the last moment the link is
            // checkable.
            if f.app_id != app_id {
                return Err(ApiError::NotFound);
            }
            let table = sauron_inspector::targets::TargetTable::from_sql(&f.source_table)
                .ok_or_else(|| {
                    ApiError::BadRequest(format!("{} is not maskable", f.source_table))
                })?;
            let column = sauron_inspector::targets::TargetColumn::from_sql(&f.source_column)
                .ok_or_else(|| {
                    ApiError::BadRequest(format!("{} is not maskable", f.source_column))
                })?;
            let entry = sauron_inspector::columns::find(&f.source_table, &f.source_column)
                .ok_or_else(|| ApiError::BadRequest("unknown column".into()))?;
            let path = if entry.kind == sauron_inspector::columns::ColumnKind::Text {
                String::new()
            } else {
                finding_path_to_mask_path(&f.key_path).map_err(|e| {
                    ApiError::BadRequest(format!(
                        "this finding's path cannot be expressed as a mask path ({e:?})"
                    ))
                })?
            };
            (
                vec![MaskTarget {
                    table,
                    column,
                    path,
                }],
                Some(f.id),
                Some(f.scan_id),
            )
        }
        (None, Some(t)) if !t.is_empty() => (t, None, None),
        _ => {
            return Err(ApiError::BadRequest(
                "supply either finding_id or a non-empty targets array".into(),
            ))
        }
    };

    // Companion expansion happens HERE, at preview, and is frozen into
    // `targets` — confirm cannot supply targets at all, so it can never widen
    // what was counted and shown.
    let mut expanded: Vec<MaskTarget> = Vec::new();
    for t in base.drain(..) {
        for e in expand_targets(&t) {
            // `expand_targets` is a pure map and can produce entries the
            // allowlist refuses (stacktrace_symbolicated); validation is the
            // gate, and a refused companion is dropped rather than failing the
            // whole request.
            if validate_target(&e).is_ok() && !expanded.contains(&e) {
                expanded.push(e);
            }
        }
    }
    if expanded.is_empty() {
        return Err(ApiError::BadRequest(
            "no maskable target survived validation".into(),
        ));
    }

    let targets_json =
        serde_json::to_value(&expanded).map_err(|e| ApiError::Internal(e.to_string()))?;
    let email = repo::user_email(&mut conn, auth.user_id)
        .await?
        .unwrap_or_default();
    let (project_id, org_id) = repo::app_ancestry(&mut conn, app_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or(ApiError::NotFound)?;
    let _ = project_id;
    let action = repo::insert_mask_action(
        &mut conn,
        sauron_db::models::NewInspectorMaskAction {
            org_id,
            app_id,
            kind: "preview",
            finding_id,
            scan_id,
            targets: &targets_json,
            requested_by: Some(auth.user_id),
            requested_by_email: &email,
        },
    )
    .await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({
            "action": action,
            "app_slug": app.slug,
            "preview_ttl_secs": state.cfg.inspector_preview_ttl_secs,
            "mask_max_rows": state.cfg.inspector_mask_max_rows,
            "enforcement_latency_secs": state.cfg.inspector_policy_cache_secs,
        })),
    ))
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct ConfirmReq {
    /// Must equal the app's slug.
    pub confirm_text: String,
}

/// Promote `previewed` -> `pending`.
///
/// Typing the SLUG is the only confirmation that forces attention onto the
/// thing that actually goes wrong. The realistic failure is not a mis-click —
/// it is masking the WRONG APP, because the operator saw a finding and forgot
/// which app was selected. A typed literal like `MASK` proves intent and
/// proves nothing about scope, and `ConfirmDialog` has no text input at all.
#[utoipa::path(
    post, path = "/v1/inspector/mask-actions/{action_id}/confirm", tag = "Inspector",
    summary = "Confirm a mask action — irreversible",
    description = "\
**Rewrites stored payloads in place. The original values are gone.** Confirm \
requires echoing the token from the preview, so it cannot be issued from the \
action id alone.",
    params(("action_id" = Uuid, Path, description = "The mask action.")), security(("bearerAuth" = [])),
    request_body(content = ConfirmReq),
    responses((status = 200, description = "The executed mask action.", body = serde_json::Value),
              (status = 400, description = "Missing or mismatched confirmation token.", body = ErrorResponse),
              (status = 401, description = "Missing or invalid access token.", body = ErrorResponse), (status = 403, description = "No grant covers this scope.", body = ErrorResponse), (status = 404, description = "No such mask action.", body = ErrorResponse),
              (status = 409, description = "Already confirmed or cancelled.", body = ErrorResponse)),
)]
pub async fn confirm_mask(
    auth: AuthUser,
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Path(action_id): Path<Uuid>,
    Json(req): Json<ConfirmReq>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = db(&state).await?;
    let action = repo::get_mask_action(&mut conn, action_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    // A FRESH authorization, not the one preview did: an operator can lose
    // pii:manage between counting and confirming.
    let app = authorize_app(&mut conn, auth.user_id, action.app_id, perm::PII_MANAGE).await?;
    if req.confirm_text.trim() != app.slug {
        return Err(ApiError::BadRequest(
            "confirm_text must be the app slug exactly".into(),
        ));
    }

    // `client_addr` records its own trust decision, because
    // API_TRUST_FORWARDED_HEADERS defaults to FALSE in Config::from_env, in
    // packaging/rpm/config/api.env and in docker-compose, and the RPM ships
    // nginx in front of the API — so behind the only packaged topology this
    // field records the same constant for every actor unless the operator
    // turns the flag on.
    let source = format!(
        "{} ua={}",
        crate::routes::auth::client_addr(&headers, &peer, &state),
        headers
            .get(axum::http::header::USER_AGENT)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.chars().take(120).collect::<String>())
            .unwrap_or_default()
    );

    // Every gate — status, TTL from `previewed_at`, and the row ceiling — is
    // IN THE STATEMENT, so a double-clicked confirm and a concurrent second
    // confirm both resolve to "0 rows updated" instead of racing.
    let n = repo::confirm_mask_action(
        &mut conn,
        action_id,
        &source,
        state.cfg.inspector_preview_ttl_secs,
        state.cfg.inspector_mask_max_rows,
    )
    .await?;
    if n == 0 {
        let fresh = repo::get_mask_action(&mut conn, action_id)
            .await?
            .ok_or(ApiError::NotFound)?;
        if fresh.estimated_rows > state.cfg.inspector_mask_max_rows {
            return Err(ApiError::Conflict(format!(
                "this mask would rewrite {} rows, above INSPECTOR_MASK_MAX_ROWS ({}); \
                 raise the ceiling explicitly if that is intended",
                fresh.estimated_rows, state.cfg.inspector_mask_max_rows
            )));
        }
        return Err(ApiError::Conflict(
            "the preview is not ready or has expired; run it again".into(),
        ));
    }
    let fresh = repo::get_mask_action(&mut conn, action_id).await?;
    Ok(Json(json!({
        "action": fresh,
        // The literal number the enforcer uses, so the UI never hardcodes it.
        "enforcement_latency_secs": state.cfg.inspector_policy_cache_secs,
    })))
}

/// Stop a queued or running mask.
///
/// PII_MANAGE, NOT the group's PII_READ: inheriting the read permission would
/// let every audit reader block a queued redaction. And the actor is recorded,
/// because in an audit table whose whole justification is "who did it", the
/// one adversarial action the design permits must not be the one it cannot
/// attribute.
#[utoipa::path(
    post, path = "/v1/inspector/mask-actions/{action_id}/cancel", tag = "Inspector",
    summary = "Cancel an unconfirmed mask action",
    params(("action_id" = Uuid, Path, description = "The mask action.")), security(("bearerAuth" = [])),
    responses((status = 200, description = "Cancelled.", body = serde_json::Value), (status = 401, description = "Missing or invalid access token.", body = ErrorResponse), (status = 403, description = "No grant covers this scope.", body = ErrorResponse), (status = 404, description = "No such mask action.", body = ErrorResponse),
              (status = 409, description = "Already confirmed.", body = ErrorResponse)),
)]
pub async fn cancel_mask(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(action_id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = db(&state).await?;
    let action = repo::get_mask_action(&mut conn, action_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    authorize_app(&mut conn, auth.user_id, action.app_id, perm::PII_MANAGE).await?;
    let email = repo::user_email(&mut conn, auth.user_id)
        .await?
        .unwrap_or_default();
    let n = repo::cancel_mask_action(&mut conn, action_id, Some(auth.user_id), &email).await?;
    if n == 0 {
        return Err(ApiError::Conflict("this action is already finished".into()));
    }
    let fresh = repo::get_mask_action(&mut conn, action_id).await?;
    Ok(Json(json!(fresh)))
}

#[utoipa::path(
    get, path = "/v1/inspector/mask-actions/{action_id}", tag = "Inspector",
    summary = "Fetch a mask action",
    params(("action_id" = Uuid, Path, description = "The mask action.")), security(("bearerAuth" = [])),
    responses((status = 200, description = "The mask action.", body = serde_json::Value), (status = 401, description = "Missing or invalid access token.", body = ErrorResponse), (status = 403, description = "No grant covers this scope.", body = ErrorResponse), (status = 404, description = "No such mask action.", body = ErrorResponse)),
)]
pub async fn get_mask_action_handler(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(action_id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = db(&state).await?;
    let a = repo::get_mask_action(&mut conn, action_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    // pii:read only — deliberately readable by someone other than the actor,
    // which is affordable precisely because the row stores PATHS AND COUNTS
    // and never a value.
    authorize_app(&mut conn, auth.user_id, a.app_id, perm::PII_READ).await?;
    Ok(Json(json!(a)))
}

fn audit_csv(rows: &[sauron_db::models::InspectorMaskAction], label: &str) -> Response {
    let mut out = String::new();
    crate::csv::write_row(
        &mut out,
        &[
            "action_id",
            "requested_at",
            "confirmed_at",
            "finished_at",
            "requested_by_email",
            "cancelled_by_email",
            "app_id",
            "status",
            "targets",
            "estimated_rows",
            "rows_masked",
            "cold_rows_skipped",
            "cold_boundary_at",
            "error",
        ],
    );
    for a in rows {
        // Semicolon-joined `table.column.path`. Paths only — never values.
        let targets = a
            .targets
            .as_array()
            .map(|arr| {
                arr.iter()
                    .map(|t| {
                        format!(
                            "{}.{}{}",
                            t.get("table").and_then(|v| v.as_str()).unwrap_or(""),
                            t.get("column").and_then(|v| v.as_str()).unwrap_or(""),
                            match t.get("path").and_then(|v| v.as_str()) {
                                Some(p) if !p.is_empty() => format!(".{p}"),
                                _ => String::new(),
                            }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(";")
            })
            .unwrap_or_default();
        crate::csv::write_row(
            &mut out,
            &[
                &a.id.to_string(),
                &a.requested_at.to_rfc3339(),
                &a.confirmed_at.map(|t| t.to_rfc3339()).unwrap_or_default(),
                &a.finished_at.map(|t| t.to_rfc3339()).unwrap_or_default(),
                &a.requested_by_email,
                &a.cancelled_by_email,
                &a.app_id.to_string(),
                &a.status,
                &targets,
                &a.estimated_rows.to_string(),
                &a.rows_masked.to_string(),
                &a.cold_rows_skipped.to_string(),
                &a.cold_boundary_at
                    .map(|t| t.to_rfc3339())
                    .unwrap_or_default(),
                &a.error,
            ],
        );
    }
    csv_response(&format!("sauron-inspector-mask-actions_{label}.csv"), out)
}

/// The findings query struct has no `environment_id` field, and passing
/// `None` to the rejection helper is a call that can never reject — so this
/// route needs its own struct that actually carries the parameter.
#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct AuditQuery {
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub environment_id: Option<String>,
}

#[utoipa::path(
    get, path = "/v1/apps/{app_id}/inspector/mask-actions", tag = "Inspector",
    summary = "Mask actions for an app",
    params(("app_id" = Uuid, Path, description = "The app."), ListLimit), security(("bearerAuth" = [])),
    responses((status = 200, description = "Mask actions.", body = serde_json::Value), (status = 401, description = "Missing or invalid access token.", body = ErrorResponse), (status = 403, description = "No grant covers this scope.", body = ErrorResponse)),
)]
pub async fn list_app_mask_actions(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<AuditQuery>,
) -> Result<Response, ApiError> {
    super::scope::reject_environment_id_with_message(
        q.environment_id.as_deref(),
        ENV_SCOPE_MESSAGE,
    )?;
    let mut conn = db(&state).await?;
    let app = authorize_app(&mut conn, auth.user_id, app_id, perm::PII_READ).await?;
    let limit = if q.format.as_deref() == Some("csv") {
        state.cfg.inspector_export_max_rows
    } else {
        clamp_limit(q.limit)
    };
    let rows = repo::list_mask_actions_for_app(&mut conn, app_id, limit).await?;
    if q.format.as_deref() == Some("csv") {
        if rows.len() as i64 >= state.cfg.inspector_export_max_rows {
            return Err(ApiError::BadRequest(
                "too_many_rows: narrow the range or raise INSPECTOR_EXPORT_MAX_ROWS".into(),
            ));
        }
        return Ok(audit_csv(&rows, &app.slug));
    }
    Ok(Json(json!(rows)).into_response())
}

/// Org-wide audit export.
///
/// Note this exports `requested_by_email` for every action, which makes a
/// downloadable STAFF-EMAIL ROSTER available to any org-scoped pii:read
/// holder. That is a deliberate trade for an audit trail, it is bounded by the
/// pseudonymization reaper, and it is stated in the wiki.
#[utoipa::path(
    get, path = "/v1/orgs/{org_id}/inspector/mask-actions", tag = "Inspector",
    summary = "Mask actions across an organization",
    params(("org_id" = Uuid, Path, description = "The organization."), ListLimit), security(("bearerAuth" = [])),
    responses((status = 200, description = "Mask actions.", body = serde_json::Value), (status = 401, description = "Missing or invalid access token.", body = ErrorResponse), (status = 403, description = "No grant covers this scope.", body = ErrorResponse)),
)]
pub async fn list_org_mask_actions(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(org_id): Path<Uuid>,
    Query(q): Query<FindingsQuery>,
) -> Result<Response, ApiError> {
    let mut conn = db(&state).await?;
    authorize_org(&mut conn, auth.user_id, org_id, perm::PII_READ).await?;
    let limit = if q.format.as_deref() == Some("csv") {
        state.cfg.inspector_export_max_rows
    } else {
        clamp_limit(q.limit)
    };
    let rows = repo::list_mask_actions_for_org(&mut conn, org_id, limit).await?;
    if q.format.as_deref() == Some("csv") {
        if rows.len() as i64 >= state.cfg.inspector_export_max_rows {
            return Err(ApiError::BadRequest(
                "too_many_rows: narrow the range or raise INSPECTOR_EXPORT_MAX_ROWS".into(),
            ));
        }
        return Ok(audit_csv(&rows, &org_id.to_string()));
    }
    Ok(Json(json!(rows)).into_response())
}

#[utoipa::path(
    get, path = "/v1/apps/{app_id}/inspector/masked-keys", tag = "Inspector",
    summary = "Keys currently masked for an app",
    params(("app_id" = Uuid, Path, description = "The app."), ListLimit), security(("bearerAuth" = [])),
    responses((status = 200, description = "Masked keys.", body = serde_json::Value), (status = 401, description = "Missing or invalid access token.", body = ErrorResponse), (status = 403, description = "No grant covers this scope.", body = ErrorResponse)),
)]
pub async fn list_app_masked_keys(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(env): Query<super::scope::RejectEnvQuery>,
) -> Result<Json<Value>, ApiError> {
    super::scope::reject_environment_id_with_message(
        env.environment_id.as_deref(),
        ENV_SCOPE_MESSAGE,
    )?;
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::PII_READ).await?;
    let rows = repo::list_masked_keys(&mut conn, app_id).await?;
    Ok(Json(json!({
        "masked_keys": rows,
        "enforcement_latency_secs": state.cfg.inspector_policy_cache_secs,
    })))
}
