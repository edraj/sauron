//! Personal notification subscriptions: `/v1/me/notification-subscriptions*`,
//! `/v1/me/notifications`, and the unauthenticated unsubscribe endpoint.
//!
//! `routes/account.rs` already owns `/v1/me/*` for sessions and profile; this
//! surface is large enough to justify its own module while sharing the
//! namespace.
//!
//! **There is no `org_id` field on any request body and there never will be
//! one.** The org is always re-derived from the scope itself, because
//! `reach_for`'s org arm sets `reach.org = true` without comparing the org id —
//! a caller-supplied org would be a cross-tenant escalation.
//!
//! None of these routes is added to the password-change allowlist in
//! `sauron-auth`'s `extractors.rs`: a temp-password holder must not reach them.

use axum::extract::{Path, Query, State};
use axum::Json;
use sauron_alerts::subscription::{covers, QueueTarget, SubConditions, SubKind};
use sauron_auth::rbac::{grants_from_rows, perm, reach_for};
use sauron_auth::AuthUser;
use sauron_db::{repo, AsyncPgConnection};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::error::ApiError;
use crate::routes::db;
use crate::AppState;

/// A compile-time ceiling, enforced at write time with a 409.
///
/// A guess, not a measurement: the per-org probe ceiling should be re-derived
/// from measured probe latency once there is data.
pub const MAX_SUBSCRIPTIONS_PER_USER: i64 = 50;

/// Mirrors the table CHECK `(quiet_start_min IS NULL) = (quiet_end_min IS NULL)`
/// so a half-specified window is a 400 with a readable message rather than a
/// constraint violation surfacing as a 500.
fn validate_quiet(start: Option<i16>, end: Option<i16>) -> Result<(), ApiError> {
    match (start, end) {
        (None, None) => Ok(()),
        (Some(s), Some(e)) => {
            if !(0..=1439).contains(&s) || !(0..=1439).contains(&e) {
                Err(ApiError::BadRequest(
                    "quiet_start_min and quiet_end_min must be minutes of day (0-1439)".into(),
                ))
            } else {
                Ok(())
            }
        }
        _ => Err(ApiError::BadRequest(
            "quiet_start_min and quiet_end_min must both be set or both be omitted".into(),
        )),
    }
}

/// `monitors` carries only `project_id`, so an app-scoped uptime subscription
/// could never fire. Refusing it is better than accepting one that is silently
/// inert.
fn validate_scope_kind(scope_type: &str, kind: SubKind) -> Result<(), ApiError> {
    match scope_type {
        "project" => Ok(()),
        "app" if kind.allows_app_scope() => Ok(()),
        "app" => Err(ApiError::BadRequest(
            "uptime subscriptions are project-scoped: monitors have no app dimension".into(),
        )),
        _ => Err(ApiError::BadRequest(
            "scope_type must be 'project' or 'app'".into(),
        )),
    }
}

/// Resolve a scope to `(org_id, project_id, app_ids)`. **No authorization
/// happens here** — see [`authorize_subscription_scope`].
///
/// 1. The org comes from the SCOPE, never from the request body.
/// 2. A project scope resolving to ZERO apps is a 422, not a success. The
///    error-kind check is "covers() holds for every app in scope", and over an
///    empty set that is vacuously true — which would let any org member
///    subscribe to anything in a project that has no apps yet.
/// 3. Uptime resolves to an EMPTY app set on purpose: `monitors` carries only
///    `project_id`, so there is no app dimension to expand, and the 422 above
///    must not fire for a project whose monitors exist but whose apps do not.
async fn resolve_subscription_scope(
    conn: &mut AsyncPgConnection,
    scope_type: &str,
    scope_id: Uuid,
    kind: SubKind,
) -> Result<(Uuid, Uuid, Vec<Uuid>), ApiError> {
    let (project_id, org_id) = match scope_type {
        "project" => {
            let org = repo::project_org(conn, scope_id)
                .await?
                .ok_or(ApiError::NotFound)?;
            (scope_id, org)
        }
        "app" => repo::app_ancestry(conn, scope_id)
            .await?
            .ok_or(ApiError::NotFound)?,
        _ => {
            return Err(ApiError::BadRequest(
                "scope_type must be 'project' or 'app'".into(),
            ))
        }
    };

    if kind == SubKind::Uptime {
        return Ok((org_id, project_id, Vec::new()));
    }

    let app_ids: Vec<Uuid> = if scope_type == "app" {
        vec![scope_id]
    } else {
        repo::list_apps_for_project(conn, project_id)
            .await?
            .into_iter()
            .map(|a| a.id)
            .collect()
    };
    if app_ids.is_empty() {
        return Err(ApiError::Unprocessable(
            "this project has no apps yet, so there is nothing to subscribe to".into(),
        ));
    }
    Ok((org_id, project_id, app_ids))
}

/// Authorize an already-resolved scope. Called EXACTLY ONCE per write, with the
/// enrollment ids the subscription will actually carry.
///
/// Order matters and each step is load-bearing:
/// 1. No grants at all in that org is a 403 (non-membership), decided before
///    any permission arithmetic.
/// 2. Uptime is accepted only on org or project reach — every monitor read in
///    the product resolves with `app: None, env: None`, so an app- or
///    env-scoped member gets 403 from the monitor API and must not be able to
///    subscribe around it.
/// 3. Error kinds require `covers()` for every app in `app_ids`.
///
/// `env_enrollments` holds **enrollment** ids (`app_environments.id`) — the id
/// space `Reach.envs` is in. A catalogue id passed here matches nothing and the
/// failure is silent at every layer, which is the trap this whole slice exists
/// to avoid. Use [`enrollments_for`] to cross the two spaces.
///
/// An EMPTY `env_enrollments` means "this subscription does not narrow by
/// environment", which needs app-level reach — never pass `&[]` as a
/// placeholder to discover something else.
async fn authorize_subscription_scope(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    org_id: Uuid,
    project_id: Uuid,
    kind: SubKind,
    app_ids: &[Uuid],
    env_enrollments: &[Uuid],
) -> Result<(), ApiError> {
    let rows = repo::user_grants_in_org(conn, user_id, org_id).await?;
    if rows.is_empty() {
        return Err(ApiError::Forbidden(
            "you are not a member of the organization that owns this scope".into(),
        ));
    }
    let grants = grants_from_rows(rows);
    let reach = reach_for(&grants, kind.permission());

    if kind == SubKind::Uptime {
        if reach.org || reach.projects.contains(&project_id) {
            return Ok(());
        }
        return Err(ApiError::Forbidden(format!(
            "you cannot read monitors for project {project_id}"
        )));
    }

    for app_id in app_ids {
        let target = QueueTarget {
            project_id,
            app_id: Some(*app_id),
            env_enrollments,
            includes_unattributed: env_enrollments.is_empty(),
        };
        if !covers(&reach, &target) {
            return Err(ApiError::Forbidden(format!(
                "you cannot read issues for app {app_id} in the scope you selected"
            )));
        }
    }
    Ok(())
}

/// Resolve a subscription's CATALOGUE environment ids to the ENROLLMENT ids the
/// coverage predicate compares against `Reach.envs`.
///
/// The two id spaces are disjoint; comparing a catalogue id against
/// `Reach.envs` would silently never match and the subscriber would be refused
/// with no explanation.
async fn enrollments_for(
    conn: &mut AsyncPgConnection,
    app_ids: &[Uuid],
    catalogue_envs: &[Uuid],
) -> Result<Vec<Uuid>, ApiError> {
    if catalogue_envs.is_empty() {
        return Ok(Vec::new());
    }
    Ok(repo::live_enrollments_for_apps(conn, app_ids)
        .await?
        .into_iter()
        .filter(|(_, _, catalogue)| catalogue_envs.contains(catalogue))
        .map(|(enrollment, _, _)| enrollment)
        .collect())
}

#[derive(Debug, Deserialize)]
pub struct UpsertSubscriptionReq {
    pub scope_type: String,
    pub scope_id: Uuid,
    pub kind: String,
    /// CATALOGUE environment ids. `[]` means all environments, including
    /// unattributed events.
    #[serde(default)]
    pub environment_ids: Vec<Uuid>,
    #[serde(default)]
    pub conditions: Value,
    #[serde(default = "default_delivery")]
    pub delivery: String,
    #[serde(default = "default_throttle")]
    pub throttle_seconds: i32,
    pub quiet_start_min: Option<i16>,
    pub quiet_end_min: Option<i16>,
    #[serde(default = "default_tz")]
    pub quiet_tz: String,
}

fn default_delivery() -> String {
    "immediate".to_string()
}
fn default_throttle() -> i32 {
    900
}
fn default_tz() -> String {
    "UTC".to_string()
}

/// The row plus everything the card needs, joined on read. The environment list
/// and the best-effort scope name live here rather than on the row struct.
#[derive(Debug, Serialize)]
pub struct SubscriptionView {
    pub id: Uuid,
    pub scope_type: String,
    pub scope_id: Uuid,
    /// Best effort: `scope_id` has no FK, so a row can outlive its target.
    pub scope_name: Option<String>,
    pub project_id: Option<Uuid>,
    pub kind: String,
    pub enabled: bool,
    pub disabled_reason: Option<String>,
    pub environment_ids: Vec<Uuid>,
    pub conditions: Value,
    pub delivery: String,
    /// What delivery the user will ACTUALLY get. The per-user hourly cap
    /// degrades to digests, and a user permanently over it would otherwise
    /// never learn that their configured `immediate` is not what happens.
    pub effective_delivery: String,
    pub throttle_seconds: i32,
    pub quiet_start_min: Option<i16>,
    pub quiet_end_min: Option<i16>,
    pub quiet_tz: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

async fn views_for(
    conn: &mut AsyncPgConnection,
    subs: Vec<sauron_db::models::NotificationSubscription>,
    over_cap: bool,
) -> Result<Vec<SubscriptionView>, ApiError> {
    let ids: Vec<Uuid> = subs.iter().map(|s| s.id).collect();
    let env_rows = repo::subscription_envs_for(conn, &ids).await?;

    let project_scopes: Vec<Uuid> = subs
        .iter()
        .filter(|s| s.scope_type == "project")
        .map(|s| s.scope_id)
        .collect();
    let app_scopes: Vec<Uuid> = subs
        .iter()
        .filter(|s| s.scope_type == "app")
        .map(|s| s.scope_id)
        .collect();
    let projects = repo::list_projects_by_ids(conn, &project_scopes).await?;
    let apps = repo::apps_by_ids(conn, &app_scopes).await?;

    Ok(subs
        .into_iter()
        .map(|s| {
            let environment_ids = env_rows
                .iter()
                .filter(|(sid, _)| *sid == s.id)
                .map(|(_, e)| *e)
                .collect();
            let (scope_name, project_id) = if s.scope_type == "project" {
                (
                    projects
                        .iter()
                        .find(|p| p.id == s.scope_id)
                        .map(|p| p.name.clone()),
                    Some(s.scope_id),
                )
            } else {
                let app = apps.iter().find(|a| a.id == s.scope_id);
                (app.map(|a| a.name.clone()), app.map(|a| a.project_id))
            };
            let effective_delivery = if over_cap && s.delivery == "immediate" {
                "hourly".to_string()
            } else {
                s.delivery.clone()
            };
            SubscriptionView {
                id: s.id,
                scope_type: s.scope_type,
                scope_id: s.scope_id,
                scope_name,
                project_id,
                kind: s.kind,
                enabled: s.enabled,
                disabled_reason: s.disabled_reason,
                environment_ids,
                conditions: s.conditions,
                delivery: s.delivery,
                effective_delivery,
                throttle_seconds: s.throttle_seconds,
                quiet_start_min: s.quiet_start_min,
                quiet_end_min: s.quiet_end_min,
                quiet_tz: s.quiet_tz,
                created_at: s.created_at,
            }
        })
        .collect())
}

pub async fn list_subscriptions(
    auth: AuthUser,
    State(state): State<AppState>,
    Query(env): Query<super::scope::RejectEnvQuery>,
) -> Result<Json<Vec<SubscriptionView>>, ApiError> {
    // Not an `/v1/apps/{id}/…` route, so the dashboard interceptor never adds
    // the parameter — but silently ignoring an unsupported query parameter is
    // treated as a bug in this codebase even on a static endpoint.
    super::scope::reject_environment_id(env.environment_id.as_deref())?;
    let mut conn = db(&state).await?;
    let subs = repo::list_subscriptions_for_user(&mut conn, auth.user_id).await?;
    let sent = repo::sent_messages_last_hour(&mut conn, auth.user_id).await?;
    let over_cap = sent >= state.cfg.notify_max_emails_per_user_per_hour.clamp(1, 1000);
    let views = views_for(&mut conn, subs, over_cap).await?;
    Ok(Json(views))
}

pub async fn create_subscription(
    auth: AuthUser,
    State(state): State<AppState>,
    Query(env): Query<super::scope::RejectEnvQuery>,
    Json(req): Json<UpsertSubscriptionReq>,
) -> Result<Json<SubscriptionView>, ApiError> {
    super::scope::reject_environment_id(env.environment_id.as_deref())?;
    let kind = SubKind::parse(&req.kind)
        .ok_or_else(|| ApiError::BadRequest(format!("unknown kind '{}'", req.kind)))?;
    validate_scope_kind(&req.scope_type, kind)?;
    validate_quiet(req.quiet_start_min, req.quiet_end_min)?;

    let mut conn = db(&state).await?;

    if !repo::timezone_exists(&mut conn, &req.quiet_tz).await? {
        return Err(ApiError::BadRequest(format!(
            "'{}' is not a timezone this server knows",
            req.quiet_tz
        )));
    }

    // Uptime ignores the environment filter entirely, so accepting a set for it
    // would store something that silently does nothing.
    let catalogue_envs: Vec<Uuid> = if kind.supports_env_filter() {
        req.environment_ids.clone()
    } else {
        Vec::new()
    };

    // Resolve the scope FIRST so environment validation and the coverage test
    // both run against a project we have already confirmed exists. Resolution
    // does NOT authorize — see the two-function split in Step 6.
    let (org_id, project_id, app_ids) =
        resolve_subscription_scope(&mut conn, &req.scope_type, req.scope_id, kind).await?;

    // Validate the submitted catalogue ids and cross them into enrollment ids
    // BEFORE authorizing, because the coverage test is decided against the
    // enrollments this subscription will actually carry.
    let enrollments: Vec<Uuid> = if catalogue_envs.is_empty() {
        Vec::new()
    } else {
        let live = repo::live_catalogue_envs_for_project(&mut conn, project_id).await?;
        // Catches the commonest paste error by far: an ENROLLMENT id copied out
        // of a dashboard URL into a field that wants a catalogue id.
        if let Some(bad) = catalogue_envs.iter().find(|e| !live.contains(e)) {
            return Err(ApiError::BadRequest(format!(
                "{bad} is not a live environment of this project"
            )));
        }
        enrollments_for(&mut conn, &app_ids, &catalogue_envs).await?
    };

    // ONE authorization pass. A second pass with `&[]` would refuse every
    // env-scoped member before this one ever ran.
    //
    // If the selected catalogue environments resolve to zero enrollments (the
    // apps in scope are not enrolled in any of them), `enrollments` is empty and
    // this degrades to the unnarrowed, app-level test — the fail-closed
    // direction, and the subscription would have matched nothing anyway.
    authorize_subscription_scope(
        &mut conn,
        auth.user_id,
        org_id,
        project_id,
        kind,
        &app_ids,
        &enrollments,
    )
    .await?;

    // Counted before the write, and the upsert may be an update — so an
    // existing subscriber editing their 50th is not refused. The rows are
    // fetched rather than counted because `is_update` needs them anyway; a
    // separate `COUNT(*)` would be a second round trip for the same answer.
    let existing = repo::list_subscriptions_for_user(&mut conn, auth.user_id).await?;
    let is_update = existing.iter().any(|s| {
        s.scope_type == req.scope_type && s.scope_id == req.scope_id && s.kind == req.kind
    });
    if !is_update && existing.len() as i64 >= MAX_SUBSCRIPTIONS_PER_USER {
        return Err(ApiError::Conflict(format!(
            "you already have {MAX_SUBSCRIPTIONS_PER_USER} subscriptions; delete one first"
        )));
    }

    let cond = SubConditions::from_value(kind, &req.conditions);
    let stored = cond.to_value(kind);
    let sub = repo::upsert_subscription(
        &mut conn,
        auth.user_id,
        org_id,
        &req.scope_type,
        req.scope_id,
        kind.as_str(),
        &stored,
        parse_delivery(req.delivery.as_str())?,
        req.throttle_seconds.clamp(0, 604_800),
        req.quiet_start_min,
        req.quiet_end_min,
        &req.quiet_tz,
        &catalogue_envs,
    )
    .await?;

    let sent = repo::sent_messages_last_hour(&mut conn, auth.user_id).await?;
    let over_cap = sent >= state.cfg.notify_max_emails_per_user_per_hour.clamp(1, 1000);
    let mut views = views_for(&mut conn, vec![sub], over_cap).await?;
    Ok(Json(views.remove(0)))
}

/// `deny_unknown_fields` is load-bearing, not tidiness. `scope_type`, `scope_id`
/// and `kind` are immutable and deliberately absent below; without this
/// attribute serde drops them on the floor and the handler answers 200 with the
/// row untouched, so a client re-pointing a subscription at another app is told
/// it succeeded. A 422 naming the field is the honest answer.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchSubscriptionReq {
    pub enabled: Option<bool>,
    #[serde(default)]
    pub environment_ids: Option<Vec<Uuid>>,
    pub conditions: Option<Value>,
    pub delivery: Option<String>,
    pub throttle_seconds: Option<i32>,
    /// A field present with a `null` value clears the window; absent leaves it.
    #[serde(default, deserialize_with = "double_option")]
    pub quiet_start_min: Option<Option<i16>>,
    #[serde(default, deserialize_with = "double_option")]
    pub quiet_end_min: Option<Option<i16>>,
    pub quiet_tz: Option<String>,
}

/// Distinguishes "absent" from "present and null" — without it, clearing a
/// quiet-hours window is indistinguishable from leaving it alone.
fn double_option<'de, D, T>(de: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(de).map(Some)
}

/// The three values `notification_subscriptions.delivery`'s CHECK constraint
/// allows.
///
/// Both handlers used to inline a `match` ending in `_ => "immediate"`, which
/// meant an unrecognised value did not fail — it silently became immediate
/// mail. On a PATCH that is destructive rather than merely wrong: a typo in
/// `delivery` rewrote a subscriber's daily digest into per-event email and the
/// response still said 200, so the first sign of trouble was the inbox.
fn parse_delivery(raw: &str) -> Result<&'static str, ApiError> {
    match raw {
        "immediate" => Ok("immediate"),
        "hourly" => Ok("hourly"),
        "daily" => Ok("daily"),
        other => Err(ApiError::BadRequest(format!(
            "'{other}' is not a delivery mode; expected immediate, hourly or daily"
        ))),
    }
}

pub async fn patch_subscription(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(env): Query<super::scope::RejectEnvQuery>,
    Json(req): Json<PatchSubscriptionReq>,
) -> Result<Json<SubscriptionView>, ApiError> {
    super::scope::reject_environment_id(env.environment_id.as_deref())?;
    let mut conn = db(&state).await?;

    // 404, never 403: confirming that someone else's subscription exists is
    // itself a disclosure.
    let existing = repo::get_subscription(&mut conn, id)
        .await?
        .filter(|s| s.user_id == auth.user_id)
        .ok_or(ApiError::NotFound)?;

    let kind = SubKind::parse(&existing.kind).ok_or(ApiError::NotFound)?;

    let quiet_start = req.quiet_start_min.unwrap_or(existing.quiet_start_min);
    let quiet_end = req.quiet_end_min.unwrap_or(existing.quiet_end_min);
    validate_quiet(quiet_start, quiet_end)?;

    let quiet_tz = req.quiet_tz.clone().unwrap_or(existing.quiet_tz.clone());
    if !repo::timezone_exists(&mut conn, &quiet_tz).await? {
        return Err(ApiError::BadRequest(format!(
            "'{quiet_tz}' is not a timezone this server knows"
        )));
    }

    // An enable/disable-only PATCH must not silently re-run the write-time
    // authorization, because a member who legitimately lost reach should still
    // be able to turn their own stale subscription off.
    if let Some(enabled) = req.enabled {
        if !enabled {
            repo::set_subscription_enabled(&mut conn, id, auth.user_id, false).await?;
            let sub = repo::get_subscription(&mut conn, id)
                .await?
                .ok_or(ApiError::NotFound)?;
            let mut views = views_for(&mut conn, vec![sub], false).await?;
            return Ok(Json(views.remove(0)));
        }
    }

    let catalogue_envs: Vec<Uuid> = if kind.supports_env_filter() {
        match &req.environment_ids {
            Some(v) => v.clone(),
            None => repo::subscription_envs_for(&mut conn, &[id])
                .await?
                .into_iter()
                .map(|(_, e)| e)
                .collect(),
        }
    } else {
        Vec::new()
    };

    // Same two-phase shape as `create_subscription`: resolve, cross the env id
    // spaces, then authorize ONCE against the resolved enrollments. Calling the
    // authorizer with `&[]` first to learn `app_ids` would 403 every env-scoped
    // member before the narrowed call could run.
    let (org_id, project_id, app_ids) =
        resolve_subscription_scope(&mut conn, &existing.scope_type, existing.scope_id, kind)
            .await?;

    let enrollments: Vec<Uuid> = if catalogue_envs.is_empty() {
        Vec::new()
    } else {
        let live = repo::live_catalogue_envs_for_project(&mut conn, project_id).await?;
        if let Some(bad) = catalogue_envs.iter().find(|e| !live.contains(e)) {
            return Err(ApiError::BadRequest(format!(
                "{bad} is not a live environment of this project"
            )));
        }
        enrollments_for(&mut conn, &app_ids, &catalogue_envs).await?
    };

    authorize_subscription_scope(
        &mut conn,
        auth.user_id,
        org_id,
        project_id,
        kind,
        &app_ids,
        &enrollments,
    )
    .await?;

    let cond = SubConditions::from_value(
        kind,
        req.conditions.as_ref().unwrap_or(&existing.conditions),
    );
    let sub = repo::upsert_subscription(
        &mut conn,
        auth.user_id,
        org_id,
        &existing.scope_type,
        existing.scope_id,
        kind.as_str(),
        &cond.to_value(kind),
        parse_delivery(req.delivery.as_deref().unwrap_or(&existing.delivery))?,
        req.throttle_seconds
            .unwrap_or(existing.throttle_seconds)
            .clamp(0, 604_800),
        quiet_start,
        quiet_end,
        &quiet_tz,
        &catalogue_envs,
    )
    .await?;

    let sent = repo::sent_messages_last_hour(&mut conn, auth.user_id).await?;
    let over_cap = sent >= state.cfg.notify_max_emails_per_user_per_hour.clamp(1, 1000);
    let mut views = views_for(&mut conn, vec![sub], over_cap).await?;
    Ok(Json(views.remove(0)))
}

pub async fn delete_subscription_route(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(env): Query<super::scope::RejectEnvQuery>,
) -> Result<Json<Value>, ApiError> {
    super::scope::reject_environment_id(env.environment_id.as_deref())?;
    let mut conn = db(&state).await?;
    let n = repo::delete_subscription(&mut conn, id, auth.user_id).await?;
    if n == 0 {
        // 404 rather than 403 — do not confirm that the id exists.
        return Err(ApiError::NotFound);
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Debug, Deserialize)]
pub struct HistoryQuery {
    pub limit: Option<i64>,
    pub environment_id: Option<String>,
}

fn history_limit(raw: Option<i64>) -> i64 {
    raw.unwrap_or(50).clamp(1, 200)
}

#[derive(Debug, Serialize)]
pub struct NotificationView {
    pub id: Uuid,
    pub kind: String,
    pub severity: String,
    pub title: Option<String>,
    pub body: Option<String>,
    pub link: Option<String>,
    pub status: String,
    pub occurred_at: chrono::DateTime<chrono::Utc>,
    pub sent_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// A user's own notification history.
///
/// Ownership alone is NOT a sufficient gate. Each row was written with a title
/// and body at enqueue time, so a member whose grant was revoked afterwards
/// would otherwise authenticate here and read exactly the issue titles and
/// counts the drain refused to mail them. Blanking on `dropped_no_access`
/// covers the rows the drain caught; this filter covers the rows whose access
/// changed after they were already sent.
pub async fn list_notifications(
    auth: AuthUser,
    State(state): State<AppState>,
    Query(q): Query<HistoryQuery>,
) -> Result<Json<Vec<NotificationView>>, ApiError> {
    super::scope::reject_environment_id(q.environment_id.as_deref())?;
    let mut conn = db(&state).await?;
    let rows = repo::notification_history_for_user(&mut conn, auth.user_id, history_limit(q.limit))
        .await?;
    if rows.is_empty() {
        return Ok(Json(Vec::new()));
    }

    let queue_ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    let env_rows = repo::queue_envs_for(&mut conn, &queue_ids).await?;
    let project_ids: Vec<Uuid> = rows.iter().map(|r| r.project_id).collect();
    let orgs = repo::project_org_batch(&mut conn, &project_ids).await?;

    // One grant load per distinct org, never per row.
    let mut org_ids: Vec<Uuid> = orgs.iter().map(|(_, o)| *o).collect();
    org_ids.sort_unstable();
    org_ids.dedup();
    let mut reaches: Vec<(Uuid, sauron_auth::rbac::Reach, sauron_auth::rbac::Reach)> = Vec::new();
    for org_id in org_ids {
        let grants =
            grants_from_rows(repo::user_grants_in_org(&mut conn, auth.user_id, org_id).await?);
        reaches.push((
            org_id,
            reach_for(&grants, perm::ISSUE_READ),
            reach_for(&grants, perm::MONITOR_READ),
        ));
    }
    drop(conn);

    let out = rows
        .into_iter()
        .filter(|r| {
            let Some((_, org_id)) = orgs.iter().find(|(p, _)| *p == r.project_id) else {
                return false;
            };
            let Some((_, issue_reach, monitor_reach)) =
                reaches.iter().find(|(o, _, _)| o == org_id)
            else {
                return false;
            };
            let envs: Vec<Uuid> = env_rows
                .iter()
                .filter(|(q, _)| *q == r.id)
                .map(|(_, e)| *e)
                .collect();
            let reach = if r.kind == "uptime" {
                monitor_reach
            } else {
                issue_reach
            };
            covers(
                reach,
                &QueueTarget {
                    project_id: r.project_id,
                    app_id: r.app_id,
                    env_enrollments: &envs,
                    includes_unattributed: r.includes_unattributed,
                },
            )
        })
        .map(|r| NotificationView {
            id: r.id,
            kind: r.kind,
            severity: r.severity,
            title: r.title,
            body: r.body,
            link: r.link,
            status: r.status,
            occurred_at: r.occurred_at,
            sent_at: r.sent_at,
        })
        .collect();
    Ok(Json(out))
}

#[derive(Debug, Deserialize)]
pub struct UnsubscribeReq {
    pub token: String,
}

/// The one and only unsubscribe response body.
///
/// Returned whether the token verified, was forged, named a subscription that
/// no longer exists, or was already disabled — anything else turns this
/// endpoint into an oracle for which subscription ids exist.
fn generic_unsubscribe_ok() -> Value {
    serde_json::json!({ "ok": true })
}

/// The signing key for unsubscribe tokens.
///
/// Derived, never raw. `NOTIFY_SECRET_KEY` is the AES-GCM key that encrypts
/// stored channel secrets, so "rotate it to invalidate outstanding links" is
/// not an available mitigation: rotating it makes every stored Slack webhook
/// URL and SMTP password undecryptable. Domain separation keeps the two uses
/// independent.
///
/// The fallback is `require_jwt_secret()`, not the field: `Config::jwt_secret`
/// is private on purpose (`sauron-core/src/config.rs:20` — "reach it through
/// `Config::require_jwt_secret`"), so touching it directly is E0616 from
/// `sauron-api`.
///
/// **This expression must stay byte-for-byte identical to `unsub_key` in
/// `sauron-alerts`' drain (Task 18 Step 5).** The drain mints the tokens and
/// this endpoint verifies them, in two different processes; if the two
/// derivations ever diverge, every link fails verification and — because this
/// endpoint deliberately returns the same body whatever happened — every
/// unsubscribe silently no-ops with no error anywhere.
pub(crate) fn unsub_signing_key(state: &AppState) -> String {
    let base = state.cfg.notify_secret_key.clone().unwrap_or_else(|| {
        state
            .cfg
            .require_jwt_secret()
            .map(String::from)
            .unwrap_or_default()
    });
    sauron_alerts::crypto::derive_unsub_key(base.as_bytes())
}

/// Disable exactly one subscription from a signed link.
///
/// Unauthenticated by necessity — the recipient is reading mail, not logged in.
/// The compensating controls are: a rate limiter consumed BEFORE any database
/// read, a constant-time signature compare, a 90-day token TTL, a constant
/// response body, a structured `info!` line, and a confirmation email to the
/// owner. This repo has no audit table, so those last two are the only
/// repudiation control there is.
pub async fn unsubscribe(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Json(req): Json<UnsubscribeReq>,
) -> Result<Json<Value>, ApiError> {
    // Consumed before ANY database read, so a flood of forged tokens costs one
    // Redis round trip each rather than a row lookup each.
    let addr = super::auth::client_addr(&headers, &peer, &state);
    super::auth::rate_limit(&state, &format!("sauron:notify:unsub:{addr}"), 30, 60).await?;

    let key = unsub_signing_key(&state);
    let today = sauron_alerts::crypto::days_since_epoch(chrono::Utc::now());

    let mut conn = db(&state).await?;
    // The owner lookup is inside the closure so a malformed token never reaches
    // the database at all.
    let owner: std::cell::Cell<Option<Uuid>> = std::cell::Cell::new(None);
    let sub_id = {
        // Two-step because the closure cannot be async: parse the id first,
        // then resolve its owner, then verify.
        let parsed: Option<Uuid> = req.token.split('.').next().and_then(|s| s.parse().ok());
        match parsed {
            Some(id) => match repo::get_subscription(&mut conn, id).await? {
                Some(s) => {
                    owner.set(Some(s.user_id));
                    sauron_alerts::crypto::verify_unsubscribe_token(
                        key.as_bytes(),
                        &req.token,
                        today,
                        |_| owner.get(),
                    )
                }
                None => None,
            },
            None => None,
        }
    };

    let Some(sub_id) = sub_id else {
        drop(conn);
        return Ok(Json(generic_unsubscribe_ok()));
    };

    repo::disable_subscription(&mut conn, sub_id, "unsubscribed").await?;
    let user_id = owner.get();
    tracing::info!(
        subscription = %sub_id,
        user = ?user_id,
        "personal notification subscription disabled via unsubscribe link"
    );

    // A confirmation to the owner is the ONLY evidence a silencing happened,
    // and it is the sharpest case for `PersonalNotification`'s zero dedup
    // window: a confirmation suppressed because a notification reached the same
    // address minutes earlier would erase exactly that evidence.
    if let Some(user_id) = user_id {
        if let Some(user) = repo::find_user_by_id(&mut conn, user_id).await? {
            // `AppState` exposes no branding accessor: `MailSender` keeps its
            // own copy private and is `None` whenever SMTP is unconfigured,
            // while this path has to behave identically either way.
            let branding = sauron_mail::Branding {
                product_name: "Sauron".to_string(),
                // `.ok()` on purpose: an unset DASHBOARD_URL costs this mail its
                // button, never the unsubscribe itself.
                dashboard_url: state.cfg.require_dashboard_url().ok().map(String::from),
                footer: "You are receiving this because you subscribed to notifications in Sauron."
                    .to_string(),
            };
            let manage = branding.link("/account").ok();
            let mut content = sauron_mail::MailContent {
                subject: "A notification subscription was turned off".to_string(),
                heading: "Subscription disabled".to_string(),
                paragraphs: vec![
                    "Someone used an unsubscribe link in one of your notification emails, \
                     and that subscription is now off."
                        .to_string(),
                    "If this was not you, turn it back on from your account page.".to_string(),
                ],
                cta: None,
                footnotes: Vec::new(),
            };
            if let Some(url) = manage {
                content.cta = sauron_mail::Cta::new("Manage subscriptions", url).ok();
            }
            match sauron_mail::render(&branding, &content) {
                Ok(rendered) => {
                    let recipient_key = user.email.trim().to_lowercase();
                    let _ = repo::enqueue_mail(
                        &mut conn,
                        sauron_db::models::NewMailOutbox {
                            kind: sauron_mail::MailKind::PersonalNotification.as_str(),
                            recipient: &user.email,
                            recipient_key: &recipient_key,
                            subject: &rendered.subject,
                            body_text: &rendered.text,
                            body_html: &rendered.html,
                            user_id: Some(user.id),
                        },
                        86_400,
                        0,
                        true,
                    )
                    .await;
                }
                Err(e) => tracing::warn!(error = %e, "unsubscribe confirmation did not render"),
            }
        }
    }
    drop(conn);
    Ok(Json(generic_unsubscribe_ok()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_subscription_cap_is_fifty() {
        assert_eq!(MAX_SUBSCRIPTIONS_PER_USER, 50);
    }

    #[test]
    fn quiet_hours_must_be_supplied_as_a_pair() {
        // Mirrors the table CHECK: `(quiet_start_min IS NULL) = (quiet_end_min IS NULL)`.
        assert!(validate_quiet(None, None).is_ok());
        assert!(validate_quiet(Some(1320), Some(360)).is_ok());
        assert!(validate_quiet(Some(1320), None).is_err());
        assert!(validate_quiet(None, Some(360)).is_err());
        assert!(
            validate_quiet(Some(1440), Some(360)).is_err(),
            "out of range"
        );
        assert!(validate_quiet(Some(-1), Some(360)).is_err(), "out of range");
    }

    #[test]
    fn history_limit_is_clamped_not_trusted() {
        assert_eq!(history_limit(None), 50);
        assert_eq!(history_limit(Some(0)), 1);
        assert_eq!(history_limit(Some(10)), 10);
        assert_eq!(history_limit(Some(100_000)), 200);
        assert_eq!(history_limit(Some(-5)), 1);
    }

    #[test]
    fn uptime_refuses_app_scope() {
        assert!(validate_scope_kind("project", SubKind::Uptime).is_ok());
        assert!(validate_scope_kind("app", SubKind::Uptime).is_err());
        assert!(validate_scope_kind("app", SubKind::ErrorSpike).is_ok());
        assert!(validate_scope_kind("org", SubKind::ErrorSpike).is_err());
    }

    #[test]
    fn patch_refuses_the_immutable_fields_instead_of_dropping_them() {
        // Without `deny_unknown_fields` these three deserialize happily into a
        // struct that has no such fields, the handler updates nothing, and the
        // caller is told 200. The dashboard shipped exactly that body once: a
        // user re-pointing a subscription at another app got a success toast
        // and an unchanged row. Positive confirmation of a change that never
        // happened is worse than an error, so each of them must now fail.
        for field in ["scope_type", "scope_id", "kind"] {
            let body = format!(r#"{{"delivery":"immediate","{field}":"whatever"}}"#);
            let parsed = serde_json::from_str::<PatchSubscriptionReq>(&body);
            assert!(
                parsed.is_err(),
                "{field} was accepted and silently ignored: {body}"
            );
        }

        // The mutable half still parses, including the present-but-null case
        // that clears a quiet-hours window.
        let ok = serde_json::from_str::<PatchSubscriptionReq>(
            r#"{"enabled":false,"throttle_seconds":300,"quiet_start_min":null}"#,
        )
        .expect("the fields PATCH does own must still deserialize");
        assert_eq!(ok.enabled, Some(false));
        assert_eq!(ok.throttle_seconds, Some(300));
        assert_eq!(ok.quiet_start_min, Some(None));
    }

    #[test]
    fn an_unknown_delivery_mode_is_refused_not_coerced() {
        for good in ["immediate", "hourly", "daily"] {
            assert_eq!(parse_delivery(good).unwrap(), good);
        }
        // The destructive case the old `_ => "immediate"` catch-all allowed:
        // this is a plausible typo for "daily", and silently honouring it turns
        // one digest a day into an email per event while reporting success.
        for bad in ["dayly", "daily_digest", "DAILY", "", "weekly"] {
            assert!(
                parse_delivery(bad).is_err(),
                "{bad:?} was coerced instead of refused"
            );
        }
    }

    #[test]
    fn the_unsubscribe_response_is_identical_whatever_happened() {
        // A caller must not be able to distinguish "token valid" from "token
        // forged" from "subscription already disabled". The body is a constant.
        assert_eq!(
            serde_json::to_string(&generic_unsubscribe_ok()).unwrap(),
            r#"{"ok":true}"#
        );
    }
}
