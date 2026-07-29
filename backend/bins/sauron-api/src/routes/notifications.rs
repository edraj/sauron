//! Alerting administration: notification channels, alert rules, delivery
//! history, and channel test-sends. Org-scoped, gated by `alert:read` /
//! `alert:write`. Channel secrets are encrypted at rest and never returned.

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use sauron_alerts::channel::{self, ChannelKind};
use sauron_alerts::rule::{self, TriggerType};
use sauron_alerts::{AlertContext, Severity};
use sauron_auth::{authorize_org, perm, AuthUser};
use sauron_db::models::{NewAlertRule, NewNotificationChannel, NotificationChannel};
use sauron_db::repo;

use super::db;
use crate::error::ApiError;
use crate::AppState;

const SEVERITIES: [&str; 3] = ["info", "warning", "critical"];
/// Bounds for the per-rule throttle window.
const MIN_THROTTLE_SECS: i32 = 0;
const MAX_THROTTLE_SECS: i32 = 7 * 24 * 3600;

/// Serialize a channel for API consumption: adds a `has_secret` flag; the
/// model's `secret_enc` is `skip_serializing` so ciphertext never leaves.
fn channel_view(ch: &NotificationChannel) -> Value {
    let mut v = serde_json::to_value(ch).unwrap_or_else(|_| json!({}));
    if let Some(o) = v.as_object_mut() {
        o.insert("has_secret".into(), json!(ch.secret_enc.is_some()));
    }
    v
}

/// Parse + validate a secret bundle: must be a JSON object of string values.
fn parse_secret(secret: &Value) -> Result<Option<String>, ApiError> {
    match secret {
        Value::Null => Ok(None),
        Value::Object(o) => {
            if o.values().any(|v| !v.is_string()) {
                return Err(ApiError::BadRequest("secret values must be strings".into()));
            }
            if o.is_empty() {
                return Ok(None);
            }
            serde_json::to_string(secret)
                .map(Some)
                .map_err(|e| ApiError::Internal(e.to_string()))
        }
        _ => Err(ApiError::BadRequest("secret must be an object".into())),
    }
}

// --- channels ---------------------------------------------------------------

pub async fn list_channels(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(org_id): Path<Uuid>,
    Query(env): Query<super::scope::RejectEnvQuery>,
) -> Result<Json<Value>, ApiError> {
    super::scope::reject_environment_id(env.environment_id.as_deref())?;
    let mut conn = db(&state).await?;
    authorize_org(&mut conn, auth.user_id, org_id, perm::ALERT_READ).await?;
    let rows = repo::list_channels_for_org(&mut conn, org_id).await?;
    Ok(Json(json!(rows
        .iter()
        .map(channel_view)
        .collect::<Vec<_>>())))
}

#[derive(Deserialize)]
pub struct CreateChannelReq {
    pub name: String,
    pub kind: String,
    #[serde(default)]
    pub config: Value,
    /// Secret bundle (e.g. {"password": "..."} / {"webhook_url": "..."}).
    #[serde(default)]
    pub secret: Value,
}

pub async fn create_channel(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(org_id): Path<Uuid>,
    Query(env): Query<super::scope::RejectEnvQuery>,
    Json(req): Json<CreateChannelReq>,
) -> Result<Json<Value>, ApiError> {
    // Notification channels are org-scoped, no environment dimension; rejected
    // here too, matching `list_channels`/`get_channel` in this same file
    // rather than silently discarding it on writes alone.
    super::scope::reject_environment_id(env.environment_id.as_deref())?;
    if req.name.trim().is_empty() {
        return Err(ApiError::BadRequest("channel name is required".into()));
    }
    let kind = ChannelKind::parse(&req.kind)
        .ok_or_else(|| ApiError::BadRequest(format!("unknown channel kind: {}", req.kind)))?;
    let config = if req.config.is_null() {
        json!({})
    } else {
        req.config.clone()
    };
    if !config.is_object() {
        return Err(ApiError::BadRequest("config must be an object".into()));
    }
    channel::validate(kind, &config, &req.secret).map_err(ApiError::BadRequest)?;
    let secret_plain = parse_secret(&req.secret)?;

    let mut conn = db(&state).await?;
    authorize_org(&mut conn, auth.user_id, org_id, perm::ALERT_WRITE).await?;

    let secret_enc = match secret_plain {
        Some(s) => Some(
            state
                .alerts
                .cipher
                .encrypt_str(&s)
                .map_err(|e| ApiError::Internal(e.to_string()))?,
        ),
        None => None,
    };
    let ch = repo::create_channel(
        &mut conn,
        NewNotificationChannel {
            org_id,
            name: req.name.trim(),
            kind: kind.as_str(),
            config: &config,
            secret_enc,
            created_by: Some(auth.user_id),
        },
    )
    .await?;
    Ok(Json(channel_view(&ch)))
}

/// Load a channel and authorize `perm` against its org.
async fn load_channel_authorized(
    state: &AppState,
    user_id: Uuid,
    channel_id: Uuid,
    perm: &str,
) -> Result<(sauron_db::PgConn, NotificationChannel), ApiError> {
    let mut conn = db(state).await?;
    let ch = repo::get_channel(&mut conn, channel_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    authorize_org(&mut conn, user_id, ch.org_id, perm).await?;
    Ok((conn, ch))
}

pub async fn get_channel(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(channel_id): Path<Uuid>,
    Query(env): Query<super::scope::RejectEnvQuery>,
) -> Result<Json<Value>, ApiError> {
    super::scope::reject_environment_id(env.environment_id.as_deref())?;
    let (_conn, ch) =
        load_channel_authorized(&state, auth.user_id, channel_id, perm::ALERT_READ).await?;
    Ok(Json(channel_view(&ch)))
}

#[derive(Deserialize)]
pub struct UpdateChannelReq {
    pub name: Option<String>,
    pub config: Option<Value>,
    /// `null` leaves the secret unchanged; `{}` clears it; a non-empty object
    /// replaces it.
    #[serde(default)]
    pub secret: Value,
    pub enabled: Option<bool>,
}

pub async fn update_channel(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(channel_id): Path<Uuid>,
    Query(env): Query<super::scope::RejectEnvQuery>,
    Json(req): Json<UpdateChannelReq>,
) -> Result<Json<Value>, ApiError> {
    super::scope::reject_environment_id(env.environment_id.as_deref())?;
    let (mut conn, ch) =
        load_channel_authorized(&state, auth.user_id, channel_id, perm::ALERT_WRITE).await?;
    let kind = ChannelKind::parse(&ch.kind)
        .ok_or_else(|| ApiError::Internal(format!("stored channel kind invalid: {}", ch.kind)))?;

    if let Some(n) = &req.name {
        if n.trim().is_empty() {
            return Err(ApiError::BadRequest("channel name cannot be empty".into()));
        }
    }
    let new_config = match &req.config {
        Some(c) if !c.is_object() => {
            return Err(ApiError::BadRequest("config must be an object".into()))
        }
        Some(c) => Some(c.clone()),
        None => None,
    };

    // Validate the channel as it will exist after the update. For the secret we
    // can only re-validate when the caller supplies one (we can't merge into the
    // stored ciphertext without decrypting — do that only when config changes).
    let secret_update: Option<Option<Vec<u8>>> = match &req.secret {
        Value::Null => None,
        v => {
            let plain = parse_secret(v)?;
            match plain {
                Some(s) => Some(Some(
                    state
                        .alerts
                        .cipher
                        .encrypt_str(&s)
                        .map_err(|e| ApiError::Internal(e.to_string()))?,
                )),
                None => Some(None), // {} → clear
            }
        }
    };

    // Effective post-update config/secret for validation.
    let effective_config = new_config.clone().unwrap_or_else(|| ch.config.clone());
    let effective_secret: Value = match (&req.secret, &ch.secret_enc) {
        (Value::Null, Some(blob)) => state
            .alerts
            .cipher
            .decrypt_str(blob)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(Value::Null),
        (Value::Null, None) => Value::Null,
        (v, _) => v.clone(),
    };
    channel::validate(kind, &effective_config, &effective_secret).map_err(ApiError::BadRequest)?;

    let updated = repo::update_channel(
        &mut conn,
        channel_id,
        req.name.as_deref().map(str::trim),
        new_config.as_ref(),
        secret_update,
        req.enabled,
    )
    .await?
    .ok_or(ApiError::NotFound)?;
    Ok(Json(channel_view(&updated)))
}

pub async fn delete_channel(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(channel_id): Path<Uuid>,
    Query(env): Query<super::scope::RejectEnvQuery>,
) -> Result<Json<Value>, ApiError> {
    super::scope::reject_environment_id(env.environment_id.as_deref())?;
    let (mut conn, _ch) =
        load_channel_authorized(&state, auth.user_id, channel_id, perm::ALERT_WRITE).await?;
    repo::delete_channel(&mut conn, channel_id).await?;
    Ok(Json(json!({ "ok": true })))
}

/// Send a test message through a channel so the admin can verify wiring.
pub async fn test_channel(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(channel_id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let (_conn, ch) =
        load_channel_authorized(&state, auth.user_id, channel_id, perm::ALERT_WRITE).await?;

    let mut ctx = AlertContext::new(Severity::Info, "test")
        .var("channel", ch.name.clone())
        .var("kind", ch.kind.clone());
    ctx.title = format!("Test alert from Sauron ({})", ch.name);
    ctx.summary = "This is a test notification — your channel is wired up correctly.".into();

    match state.alerts.deliver_channel(&ch, &ctx, &ctx.summary).await {
        Ok(attempts) => Ok(Json(json!({ "ok": true, "attempts": attempts }))),
        Err((attempts, err)) => Ok(Json(json!({
            "ok": false,
            "attempts": attempts,
            "error": err,
        }))),
    }
}

// --- rules ------------------------------------------------------------------

/// Rule + its channel ids, as the UI consumes it.
async fn rule_view(
    conn: &mut sauron_db::AsyncPgConnection,
    rule: &sauron_db::models::AlertRule,
) -> Result<Value, ApiError> {
    let channel_ids = repo::rule_channel_ids(conn, rule.id).await?;
    let mut v = serde_json::to_value(rule).unwrap_or_else(|_| json!({}));
    if let Some(o) = v.as_object_mut() {
        o.insert("channel_ids".into(), json!(channel_ids));
    }
    Ok(v)
}

pub async fn list_rules(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(org_id): Path<Uuid>,
    Query(env): Query<super::scope::RejectEnvQuery>,
) -> Result<Json<Value>, ApiError> {
    super::scope::reject_environment_id(env.environment_id.as_deref())?;
    let mut conn = db(&state).await?;
    authorize_org(&mut conn, auth.user_id, org_id, perm::ALERT_READ).await?;
    let rules = repo::list_alert_rules_for_org(&mut conn, org_id).await?;
    // One grouped lookup for the page rather than `rule_view` per rule, which
    // was a query each — a 200-rule org issued 201 queries per page load.
    let rule_ids: Vec<Uuid> = rules.iter().map(|r| r.id).collect();
    let mut by_rule = repo::rule_channel_ids_for_rules(&mut conn, &rule_ids).await?;
    let out: Vec<Value> = rules
        .iter()
        .map(|r| {
            let channel_ids = by_rule.remove(&r.id).unwrap_or_default();
            let mut v = serde_json::to_value(r).unwrap_or_else(|_| json!({}));
            if let Some(o) = v.as_object_mut() {
                o.insert("channel_ids".into(), json!(channel_ids));
            }
            v
        })
        .collect();
    Ok(Json(json!(out)))
}

#[derive(Deserialize)]
pub struct CreateRuleReq {
    pub name: String,
    pub trigger_type: String,
    #[serde(default)]
    pub project_id: Option<Uuid>,
    #[serde(default)]
    pub app_id: Option<Uuid>,
    #[serde(default)]
    pub conditions: Value,
    #[serde(default)]
    pub severity: Option<String>,
    #[serde(default)]
    pub throttle_seconds: Option<i32>,
    #[serde(default)]
    pub message_template: Option<String>,
    #[serde(default)]
    pub channel_ids: Vec<Uuid>,
}

/// Validate that a rule's narrowing scope belongs to `org_id`; returns the
/// project id implied by an app narrowing so `(project_id, app_id)` stay
/// consistent.
async fn check_rule_scope(
    conn: &mut sauron_db::AsyncPgConnection,
    org_id: Uuid,
    project_id: Option<Uuid>,
    app_id: Option<Uuid>,
) -> Result<(Option<Uuid>, Option<Uuid>), ApiError> {
    match (project_id, app_id) {
        (None, None) => Ok((None, None)),
        (Some(p), None) => {
            if repo::project_org(conn, p).await? != Some(org_id) {
                return Err(ApiError::BadRequest("project is not in this org".into()));
            }
            Ok((Some(p), None))
        }
        (maybe_p, Some(a)) => match repo::app_ancestry(conn, a).await? {
            Some((proj, o)) if o == org_id => {
                if let Some(p) = maybe_p {
                    if p != proj {
                        return Err(ApiError::BadRequest(
                            "app does not belong to the given project".into(),
                        ));
                    }
                }
                Ok((Some(proj), Some(a)))
            }
            _ => Err(ApiError::BadRequest("app is not in this org".into())),
        },
    }
}

/// Validate that every channel id exists and belongs to `org_id`.
async fn check_channels_in_org(
    conn: &mut sauron_db::AsyncPgConnection,
    org_id: Uuid,
    channel_ids: &[Uuid],
) -> Result<(), ApiError> {
    for cid in channel_ids {
        match repo::get_channel(conn, *cid).await? {
            Some(c) if c.org_id == org_id => {}
            _ => {
                return Err(ApiError::BadRequest(
                    "channel does not belong to this org".into(),
                ))
            }
        }
    }
    Ok(())
}

pub async fn create_rule(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(org_id): Path<Uuid>,
    Query(env): Query<super::scope::RejectEnvQuery>,
    Json(req): Json<CreateRuleReq>,
) -> Result<Json<Value>, ApiError> {
    // Alert rules narrow by project/app, not by environment; rejected here
    // too, matching `list_rules`/`get_rule` in this same file rather than
    // silently discarding it on writes alone.
    super::scope::reject_environment_id(env.environment_id.as_deref())?;
    if req.name.trim().is_empty() {
        return Err(ApiError::BadRequest("rule name is required".into()));
    }
    let trigger = TriggerType::parse(&req.trigger_type).ok_or_else(|| {
        ApiError::BadRequest(format!("unknown trigger_type: {}", req.trigger_type))
    })?;
    let conditions = if req.conditions.is_null() {
        json!({})
    } else {
        req.conditions.clone()
    };
    if !conditions.is_object() {
        return Err(ApiError::BadRequest("conditions must be an object".into()));
    }
    rule::validate_conditions(trigger, &conditions).map_err(ApiError::BadRequest)?;
    let severity = req.severity.as_deref().unwrap_or("warning");
    if !SEVERITIES.contains(&severity) {
        return Err(ApiError::BadRequest(
            "severity must be info|warning|critical".into(),
        ));
    }
    let throttle = req
        .throttle_seconds
        .unwrap_or(300)
        .clamp(MIN_THROTTLE_SECS, MAX_THROTTLE_SECS);

    let mut conn = db(&state).await?;
    authorize_org(&mut conn, auth.user_id, org_id, perm::ALERT_WRITE).await?;
    let (project_id, app_id) =
        check_rule_scope(&mut conn, org_id, req.project_id, req.app_id).await?;
    check_channels_in_org(&mut conn, org_id, &req.channel_ids).await?;

    let rule = repo::create_alert_rule(
        &mut conn,
        NewAlertRule {
            org_id,
            project_id,
            app_id,
            name: req.name.trim(),
            trigger_type: trigger.as_str(),
            conditions: &conditions,
            severity,
            throttle_seconds: throttle,
            message_template: req
                .message_template
                .as_deref()
                .filter(|s| !s.trim().is_empty()),
            // Metric rules start evaluating from "now" — no retroactive storm.
            last_evaluated_at: trigger.is_metric().then(chrono::Utc::now),
            created_by: Some(auth.user_id),
        },
    )
    .await?;
    repo::set_rule_channels(&mut conn, rule.id, &req.channel_ids).await?;
    Ok(Json(rule_view(&mut conn, &rule).await?))
}

async fn load_rule_authorized(
    state: &AppState,
    user_id: Uuid,
    rule_id: Uuid,
    perm: &str,
) -> Result<(sauron_db::PgConn, sauron_db::models::AlertRule), ApiError> {
    let mut conn = db(state).await?;
    let rule = repo::get_alert_rule(&mut conn, rule_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    authorize_org(&mut conn, user_id, rule.org_id, perm).await?;
    Ok((conn, rule))
}

pub async fn get_rule(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(rule_id): Path<Uuid>,
    Query(env): Query<super::scope::RejectEnvQuery>,
) -> Result<Json<Value>, ApiError> {
    super::scope::reject_environment_id(env.environment_id.as_deref())?;
    let (mut conn, rule) =
        load_rule_authorized(&state, auth.user_id, rule_id, perm::ALERT_READ).await?;
    Ok(Json(rule_view(&mut conn, &rule).await?))
}

#[derive(Deserialize)]
pub struct UpdateRuleReq {
    pub name: Option<String>,
    pub enabled: Option<bool>,
    pub conditions: Option<Value>,
    pub severity: Option<String>,
    pub throttle_seconds: Option<i32>,
    /// Send an empty string to clear the template (mirrors monitors'
    /// webhook_url update semantics).
    #[serde(default)]
    pub message_template: Option<Option<String>>,
    pub channel_ids: Option<Vec<Uuid>>,
}

pub async fn update_rule(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(rule_id): Path<Uuid>,
    Query(env): Query<super::scope::RejectEnvQuery>,
    Json(req): Json<UpdateRuleReq>,
) -> Result<Json<Value>, ApiError> {
    super::scope::reject_environment_id(env.environment_id.as_deref())?;
    let (mut conn, rule) =
        load_rule_authorized(&state, auth.user_id, rule_id, perm::ALERT_WRITE).await?;
    let trigger = TriggerType::parse(&rule.trigger_type)
        .ok_or_else(|| ApiError::Internal("stored trigger_type invalid".into()))?;

    if let Some(n) = &req.name {
        if n.trim().is_empty() {
            return Err(ApiError::BadRequest("rule name cannot be empty".into()));
        }
    }
    if let Some(c) = &req.conditions {
        if !c.is_object() {
            return Err(ApiError::BadRequest("conditions must be an object".into()));
        }
        rule::validate_conditions(trigger, c).map_err(ApiError::BadRequest)?;
    }
    if let Some(s) = &req.severity {
        if !SEVERITIES.contains(&s.as_str()) {
            return Err(ApiError::BadRequest(
                "severity must be info|warning|critical".into(),
            ));
        }
    }
    if let Some(ids) = &req.channel_ids {
        check_channels_in_org(&mut conn, rule.org_id, ids).await?;
    }

    let template_update: Option<Option<&str>> = req
        .message_template
        .as_ref()
        .map(|opt| opt.as_deref().filter(|s| !s.trim().is_empty()));

    let updated = repo::update_alert_rule(
        &mut conn,
        rule_id,
        req.name.as_deref().map(str::trim),
        req.enabled,
        req.conditions.as_ref(),
        req.severity.as_deref(),
        req.throttle_seconds
            .map(|t| t.clamp(MIN_THROTTLE_SECS, MAX_THROTTLE_SECS)),
        template_update,
    )
    .await?
    .ok_or(ApiError::NotFound)?;
    if let Some(ids) = &req.channel_ids {
        repo::set_rule_channels(&mut conn, rule_id, ids).await?;
    }
    Ok(Json(rule_view(&mut conn, &updated).await?))
}

pub async fn delete_rule(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(rule_id): Path<Uuid>,
    Query(env): Query<super::scope::RejectEnvQuery>,
) -> Result<Json<Value>, ApiError> {
    super::scope::reject_environment_id(env.environment_id.as_deref())?;
    let (mut conn, _rule) =
        load_rule_authorized(&state, auth.user_id, rule_id, perm::ALERT_WRITE).await?;
    repo::delete_alert_rule(&mut conn, rule_id).await?;
    Ok(Json(json!({ "ok": true })))
}

// --- history + metadata -----------------------------------------------------

#[derive(Deserialize)]
pub struct HistoryQuery {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    /// Alert history is org-scoped, not environment-scoped; rejected rather
    /// than silently accepted-and-ignored.
    pub environment_id: Option<String>,
}

pub async fn list_history(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(org_id): Path<Uuid>,
    Query(q): Query<HistoryQuery>,
) -> Result<Json<Value>, ApiError> {
    super::scope::reject_environment_id(q.environment_id.as_deref())?;
    let mut conn = db(&state).await?;
    authorize_org(&mut conn, auth.user_id, org_id, perm::ALERT_READ).await?;
    let rows = repo::list_alert_events(
        &mut conn,
        org_id,
        q.limit.unwrap_or(50),
        q.offset.unwrap_or(0),
    )
    .await?;
    Ok(Json(json!(rows)))
}

/// Static metadata the rule-builder UI needs: trigger types, channel kinds,
/// comparators, and the template variables each trigger exposes.
///
/// Takes (and rejects) `environment_id` purely for consistency with every
/// other read in this group (`list_channels`, `list_rules`, `list_history`):
/// this response is static enums, with no environment dimension to even
/// silently ignore, but a caller passing the parameter here and having it
/// vanish without complaint — while every sibling endpoint 400s — is the same
/// "did my filter apply?" trap as the scoping bug itself.
pub async fn meta(
    _auth: AuthUser,
    Query(env): Query<super::scope::RejectEnvQuery>,
) -> Result<Json<Value>, ApiError> {
    super::scope::reject_environment_id(env.environment_id.as_deref())?;
    Ok(Json(json!({
        "channel_kinds": ChannelKind::ALL.iter().map(|k| k.as_str()).collect::<Vec<_>>(),
        "trigger_types": TriggerType::ALL.iter().map(|t| json!({
            "key": t.as_str(),
            "metric": t.is_metric(),
        })).collect::<Vec<_>>(),
        "comparators": ["gte", "gt", "lte", "lt", "eq"],
        "severities": SEVERITIES,
        "metrics": ["p50", "p75", "p90", "p95", "p99", "avg", "max"],
        "template_vars": {
            "monitor_down": ["monitor", "target", "status", "previous_status", "cause", "project_id"],
            "monitor_up": ["monitor", "target", "status", "previous_status", "project_id"],
            "issue_new": ["issue_title", "issue_level", "app_id", "times_seen"],
            "issue_regression": ["issue_title", "issue_level", "app_id", "times_seen"],
            "error_threshold": ["count", "threshold", "window_minutes"],
            "error_spike": ["count", "previous_count", "factor", "window_minutes"],
            "event_threshold": ["count", "threshold", "window_minutes", "event_name"],
            "perf_degradation": ["value_ms", "threshold_ms", "metric", "window_minutes"],
        },
    })))
}
