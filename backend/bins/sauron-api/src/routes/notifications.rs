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
use sauron_auth::{authorize_app, authorize_org, authorize_project, perm, AuthUser};
use sauron_db::models::{NewAlertRule, NewNotificationChannel, NotificationChannel};
use sauron_db::repo;

use super::db;
use crate::error::ApiError;
use crate::AppState;

const SEVERITIES: [&str; 3] = ["info", "warning", "critical"];
/// Bounds for the per-rule throttle window.
const MIN_THROTTLE_SECS: i32 = 0;
const MAX_THROTTLE_SECS: i32 = 7 * 24 * 3600;

/// The part of a channel's config it is safe to hand back to an `alert:read`
/// caller.
///
/// Encrypting `config` at rest closes the *storage* half of the leak and not one
/// byte of the API half: the row is decrypted server-side anyway, and
/// `alert:read` is held by the Developer preset. So the projection has to happen
/// here too.
///
/// It is an ALLOWLIST per kind, deliberately. A denylist ("strip `headers` and
/// `url`") leaks every field added later by default, which is precisely the
/// mistake migration 000019 made when it declared `config` "non-secret". An
/// unknown key here is simply not returned — a visible gap in the UI, which
/// someone fixes, rather than a silent disclosure that nobody sees.
///
/// The generic webhook keeps its ORIGIN but loses its path: `hooks.slack.com`
/// identifies the vendor, the path segment is the entire credential. Header
/// names survive, header values never do — an `Authorization: Bearer …` is
/// exactly what lives in that map.
fn redacted_config(kind: ChannelKind, config: &Value) -> Value {
    let mut out = serde_json::Map::new();
    let keep = |out: &mut serde_json::Map<String, Value>, key: &str| {
        if let Some(v) = config.get(key) {
            out.insert(key.to_string(), v.clone());
        }
    };
    match kind {
        // The relay, the envelope and the login name: the operator's own mail
        // settings. The password is the credential and lives in the secret.
        ChannelKind::Email => {
            for k in ["host", "port", "from", "to", "username", "implicit_tls"] {
                keep(&mut out, k);
            }
        }
        // The access token is the credential; the homeserver and room are the
        // address, and hiding them would make the channel unidentifiable.
        ChannelKind::Matrix => {
            for k in ["homeserver", "room_id"] {
                keep(&mut out, k);
            }
        }
        ChannelKind::Telegram => keep(&mut out, "chat_id"),
        // The incoming-webhook URL *is* the credential for these two.
        ChannelKind::Slack | ChannelKind::Discord => {
            out.insert(
                "has_webhook_url".into(),
                json!(config.get("webhook_url").is_some()),
            );
        }
        ChannelKind::Webhook => {
            out.insert(
                "url_origin".into(),
                json!(channel::credential_binding(kind, config)),
            );
            out.insert("has_url".into(), json!(config.get("url").is_some()));
            let names: Vec<&String> = match config.get("headers") {
                Some(Value::Object(h)) => h.keys().collect(),
                _ => Vec::new(),
            };
            out.insert("header_names".into(), json!(names));
        }
    }
    Value::Object(out)
}

/// Serialize a channel for API consumption.
///
/// Both stored payload columns are `skip_serializing` on the model, so nothing
/// leaves except what this function puts back: a `has_secret` flag and a
/// [`redacted_config`] projection.
///
/// A config that will not decrypt degrades this ONE row to
/// `config: null, config_error: true` instead of failing the request. A single
/// unreadable channel must not make the whole Alerts page unloadable — that is
/// how an operator loses the ability to delete the broken row and start over.
/// Writes take the opposite line and refuse; see `update_channel`.
fn channel_view(cipher: &sauron_alerts::SecretCipher, ch: &NotificationChannel) -> Value {
    let mut v = serde_json::to_value(ch).unwrap_or_else(|_| json!({}));
    if let Some(o) = v.as_object_mut() {
        o.insert("has_secret".into(), json!(ch.secret_enc.is_some()));
        match (
            ChannelKind::parse(&ch.kind),
            sauron_alerts::crypto::open_channel_config(cipher, ch),
        ) {
            (Some(kind), Ok(cfg)) => {
                o.insert("config".into(), redacted_config(kind, &cfg));
                o.insert("config_error".into(), json!(false));
            }
            _ => {
                o.insert("config".into(), Value::Null);
                o.insert("config_error".into(), json!(true));
            }
        }
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
        .map(|ch| channel_view(&state.alerts.cipher, ch))
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
    // The config is a credential too: for the generic webhook kind it holds the
    // target URL and an arbitrary header map (where an `Authorization: Bearer …`
    // ends up), and for Slack/Discord a `webhook_url` here IS the credential.
    // New rows therefore only ever carry ciphertext — `NewNotificationChannel`
    // has no plaintext `config` field to fill in.
    let config_enc = state
        .alerts
        .cipher
        .encrypt_json(&config)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let ch = repo::create_channel(
        &mut conn,
        NewNotificationChannel {
            org_id,
            name: req.name.trim(),
            kind: kind.as_str(),
            config_enc: Some(config_enc),
            secret_enc,
            created_by: Some(auth.user_id),
        },
    )
    .await?;
    Ok(Json(channel_view(&state.alerts.cipher, &ch)))
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
    Ok(Json(channel_view(&state.alerts.cipher, &ch)))
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

    // A channel's config IS its destination, so changing it re-aims every rule
    // attached to it. That is `update_rule`'s `channel_ids` exfiltration from the
    // other end: instead of pointing your rule at a channel you may not reach,
    // you point a channel someone else's rule already uses at a webhook you own.
    // `alert:write` at the org authorizes configuring alerting; it does not
    // authorize redirecting telemetry drawn from projects you cannot read.
    //
    // Scoped to config/secret changes on purpose. `name` and `enabled` move no
    // data anywhere — enabling a channel resumes delivery to a destination an
    // authorized caller chose — and gating them too would 403 an unrelated
    // rename, the same over-reach `authorize_rule_target` declines to make when
    // it widens an app-narrowed monitor rule instead of rejecting it.
    //
    // `secret` counts because for Slack and Discord the secret bundle IS the
    // destination (`webhook_url`), so gating `config` alone would leave the two
    // most common kinds redirectable.
    if new_config.is_some() || !req.secret.is_null() {
        authorize_channel_retarget(&mut conn, auth.user_id, ch.org_id, channel_id).await?;
    }

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
    //
    // Both reads are hard failures, never silent degradation. The previous
    // `.ok().and_then(…).unwrap_or(Value::Null)` on the secret turned an
    // undecryptable channel into "this channel has no secret", which surfaced as
    // a baffling `matrix: access_token is required` — and, once `config` moved
    // behind the same cipher, the equivalent swallow would let an admin's edit
    // overwrite an unreadable channel with a blank config. A key mismatch must
    // say so.
    let stored_config = sauron_alerts::crypto::open_channel_config(&state.alerts.cipher, &ch)
        .map_err(|e| {
            ApiError::Internal(format!(
                "this channel's stored config cannot be decrypted ({e}); \
                 NOTIFY_SECRET_KEY does not match the key it was written with"
            ))
        })?;
    let effective_config = new_config.clone().unwrap_or_else(|| stored_config.clone());
    let effective_secret: Value = match (&req.secret, &ch.secret_enc) {
        (Value::Null, Some(blob)) => state.alerts.cipher.decrypt_json(blob).map_err(|e| {
            ApiError::Internal(format!(
                "this channel's stored secret cannot be decrypted ({e}); \
                     NOTIFY_SECRET_KEY does not match the key it was written with"
            ))
        })?,
        (Value::Null, None) => Value::Null,
        (v, _) => v.clone(),
    };
    // A stored secret was issued for ONE destination. `config` holds the
    // destination (`email.host`, `matrix.homeserver`, `webhook.url`) while the
    // credential sits in the encrypted bundle, and this handler lets the two be
    // changed independently — so without this guard a caller holding only
    // `alert:write` (which never confers *reading* the secret: see
    // `perm::ALERT_READ`'s "secrets always redacted") can repoint the channel at
    // a host they control, hit `POST .../test`, and have the server hand over
    // the SMTP password or the Matrix access token. The SSRF guard does not
    // help: the attacker's host is public, which it permits by design.
    //
    // Only an OMITTED `secret` is dangerous. `{}` clears the credential and a
    // replacement bundle means the caller supplied it for the new host
    // knowingly; both fall through.
    if req.secret.is_null() && ch.secret_enc.is_some() {
        if channel::credential_binding(kind, &stored_config)
            != channel::credential_binding(kind, &effective_config)
        {
            return Err(ApiError::BadRequest(
                "changing a channel's destination requires re-supplying its secret".into(),
            ));
        }
        // Adjacent silent no-op, same invariant: for Slack/Discord `resolve`
        // prefers the stored secret, so a `config.webhook_url` edit returns 200,
        // shows the new URL in every GET, and keeps delivering to the old
        // endpoint forever. Refuse rather than lie.
        if channel::config_claims_shadowed_webhook_url(kind, &effective_config)
            && effective_config.get("webhook_url") != stored_config.get("webhook_url")
        {
            return Err(ApiError::BadRequest(
                "this channel's webhook URL is stored as its secret; send it in `secret`, \
                 not `config`"
                    .into(),
            ));
        }
    }

    channel::validate(kind, &effective_config, &effective_secret).map_err(ApiError::BadRequest)?;

    // Re-seal when the caller edited the config — and also when the row is a
    // pre-000046 one that the boot conversion has not reached (a stale replica,
    // a row created by an older binary mid-rollout). Any write leaves the row
    // encrypted; there is no path that keeps plaintext alive past an edit.
    let config_enc = if new_config.is_some() || ch.config_enc.is_none() {
        Some(
            state
                .alerts
                .cipher
                .encrypt_json(&effective_config)
                .map_err(|e| ApiError::Internal(e.to_string()))?,
        )
    } else {
        None
    };

    let updated = repo::update_channel(
        &mut conn,
        channel_id,
        req.name.as_deref().map(str::trim),
        config_enc,
        secret_update,
        req.enabled,
    )
    .await?
    .ok_or(ApiError::NotFound)?;
    Ok(Json(channel_view(&state.alerts.cipher, &updated)))
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
    /// Narrow a monitor trigger to ONE monitor. `None` = every monitor in scope.
    #[serde(default)]
    pub monitor_id: Option<Uuid>,
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

/// Validate that a rule's narrowing scope belongs to `org_id`; returns
/// `(project_id, app_id, monitor_id)` with the project id implied by an app or
/// monitor narrowing, so all three stay consistent with each other.
async fn check_rule_scope(
    conn: &mut sauron_db::AsyncPgConnection,
    org_id: Uuid,
    project_id: Option<Uuid>,
    app_id: Option<Uuid>,
    monitor_id: Option<Uuid>,
) -> Result<(Option<Uuid>, Option<Uuid>, Option<Uuid>), ApiError> {
    // A monitor pins the rule to exactly one target, and `monitors` carries only
    // `project_id` — so the monitor DERIVES the project, exactly as an app does
    // below. Deriving rather than trusting the caller's `project_id` is what
    // makes `authorize_rule_target` check `monitor:read` at the radius the rule
    // will actually fire over.
    if let Some(m) = monitor_id {
        // A single message for "no such monitor" and "monitor in another org":
        // splitting them would let any org alert:write holder probe UUIDs to
        // learn which ones exist in a foreign org, the same oracle the project
        // arm below avoids with "project is not in this org".
        let not_in_org = || ApiError::BadRequest("monitor is not in this org".into());
        let proj = repo::monitor_project(conn, m)
            .await?
            .ok_or_else(not_in_org)?;
        if repo::project_org(conn, proj).await? != Some(org_id) {
            return Err(not_in_org());
        }
        if let Some(p) = project_id {
            if p != proj {
                return Err(ApiError::BadRequest(
                    "monitor does not belong to the given project".into(),
                ));
            }
        }
        // A monitor trigger has no app dimension; carrying one would be a
        // narrowing that never applies.
        return Ok((Some(proj), None, Some(m)));
    }
    match (project_id, app_id) {
        (None, None) => Ok((None, None, None)),
        (Some(p), None) => {
            if repo::project_org(conn, p).await? != Some(org_id) {
                return Err(ApiError::BadRequest("project is not in this org".into()));
            }
            Ok((Some(p), None, None))
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
                Ok((Some(proj), Some(a), None))
            }
            _ => Err(ApiError::BadRequest("app is not in this org".into())),
        },
    }
}

/// The read permission a rule's notifications actually disclose.
///
/// Alert bodies are not bare counts. Issue triggers embed the verbatim issue
/// title (`sauron-alerts`' evaluator builds `"{verb}: {issue.title}"`), monitor
/// triggers embed the probed `target` — frequently an internal hostname. So the
/// permission a rule must be authorized against is the one that governs reading
/// *that* signal, not `alert:write`.
fn rule_read_permission(trigger: TriggerType) -> &'static str {
    match trigger {
        // Discloses monitor names and probed URLs, so it is monitor reach —
        // folding this under `issue:read` would let an issue reader enumerate
        // the org's monitored endpoints.
        TriggerType::MonitorDown | TriggerType::MonitorUp => perm::MONITOR_READ,
        // Analytics-event and latency signal, both governed by `event:read`
        // (`perf_degradation` reads spans, not issues).
        TriggerType::EventThreshold | TriggerType::PerfDegradation => perm::EVENT_READ,
        TriggerType::IssueNew
        | TriggerType::IssueRegression
        | TriggerType::ErrorThreshold
        | TriggerType::ErrorSpike => perm::ISSUE_READ,
    }
}

/// Authorize the *telemetry* a rule will emit, at the scope it will cover.
///
/// `alert:write` authorizes CONFIGURING alerting; it does not authorize the data
/// a rule ships out. The exfiltration path is self-service — the same permission
/// creates a webhook channel pointing anywhere — and the disclosure is durable:
/// `AlertEngine::log_event` persists title/body into `alert_events`, which
/// `list_history` serves to any org-scoped `alert:read` holder, so deleting the
/// rule afterwards does not undo it.
///
/// `(None, None)` is the **widest** scope, not the narrowest: `apps_in_alert_scope`
/// expands an un-narrowed rule to every app in the org, so that arm demands
/// org-scoped read rather than "no target, no check". A fix that only guarded
/// `req.app_id` would leave the cheaper exploit — target nothing — wide open.
///
/// Matches `notification_prefs::authorize_subscription_scope`, which has
/// enforced exactly this on personal subscriptions since that slice shipped;
/// org alert rules carry the same telemetry to a broader audience.
///
/// Uses `authorize_app`, never `authorize_app_reachable`: the latter is
/// read-only by explicit contract (an environment grant must not authorize an
/// app-wide rule), the same rule `inspector::authorize_policy` spells out.
///
/// Called from BOTH `create_rule` (with `check_rule_scope`'s output) and
/// `update_rule` (with the stored columns). One helper on purpose: the two must
/// not be able to drift, because a create-time-only gate is bypassed by
/// creating a rule you may target and then editing it.
async fn authorize_rule_target(
    conn: &mut sauron_db::AsyncPgConnection,
    user_id: Uuid,
    org_id: Uuid,
    project_id: Option<Uuid>,
    app_id: Option<Uuid>,
    trigger: TriggerType,
) -> Result<(), ApiError> {
    let read_perm = rule_read_permission(trigger);
    // Monitor triggers have no APP dimension at firing time, so authorizing one
    // at app scope would be narrower than what the rule actually delivers.
    // `monitors` carries only `project_id` (no `app_id`, no `environment_id`),
    // so the app narrowing is dropped here to check at the radius that applies.
    //
    // Monitor narrowing is different and IS honoured: `alert_rules.monitor_id`
    // is filtered by `repo::alert_rules_for_monitor`, and `check_rule_scope`
    // derives `project_id` from the pinned monitor — so a pinned rule arrives
    // here on the `(None, Some(project))` arm. That is strictly narrower than
    // the org arm it would otherwise take, never looser, which is why pinning
    // needs no additional gate of its own.
    //
    // Same fact `SubKind::allows_app_scope` encodes for personal uptime
    // subscriptions, but the remedy differs: subscriptions refuse app scope
    // outright, while rules accept-and-widen. Refusing would 400 every
    // app-narrowed monitor rule already stored — including on an unrelated
    // rename — and the widened check is already the strict reading.
    //
    // A hand-inserted row with `app_id` but a NULL `project_id` (nothing the
    // API can produce: `check_rule_scope` derives the project from the app)
    // falls through to the org arm, which is stricter still. Fail-safe.
    let app_id = match trigger {
        TriggerType::MonitorDown | TriggerType::MonitorUp => None,
        _ => app_id,
    };
    // App narrowing is checked first: `check_rule_scope` returns
    // `(Some(project), Some(app))` for an app-narrowed rule, so matching on
    // `project_id` first would settle for the looser project-level check.
    match (app_id, project_id) {
        (Some(a), _) => {
            authorize_app(conn, user_id, a, read_perm).await?;
        }
        (None, Some(p)) => {
            authorize_project(conn, user_id, p, read_perm).await?;
        }
        (None, None) => {
            authorize_org(conn, user_id, org_id, read_perm).await?;
        }
    }
    Ok(())
}

/// [`authorize_rule_target`] as a question instead of an assertion.
///
/// A denial is a legitimate answer here, not an error: both callers below have to
/// evaluate *many* rules and act on the pattern of answers, so they cannot use a
/// helper that returns early on the first `Forbidden`.
///
/// Only `Forbidden`/`NotFound`/`Auth` become `false`. Anything else — a dropped
/// connection, a failed query — propagates, because a DB error silently read as
/// "not authorized" turns an outage into a history page that quietly renders
/// empty, and an operator would have no way to tell that from "no alerts fired".
async fn may_read_rule_target(
    conn: &mut sauron_db::AsyncPgConnection,
    user_id: Uuid,
    org_id: Uuid,
    project_id: Option<Uuid>,
    app_id: Option<Uuid>,
    trigger: TriggerType,
) -> Result<bool, ApiError> {
    match authorize_rule_target(conn, user_id, org_id, project_id, app_id, trigger).await {
        Ok(()) => Ok(true),
        Err(ApiError::Forbidden(_)) | Err(ApiError::NotFound) | Err(ApiError::Auth(_)) => Ok(false),
        Err(e) => Err(e),
    }
}

/// Authorize re-aiming a channel: the caller must be able to read the telemetry
/// of **every** rule currently delivering to it.
///
/// Every, not any. A channel shared by ten rules carries the union of their
/// disclosures, so being authorized for one of them does not license redirecting
/// the other nine. The refusal names the offending rule, because an admin who
/// legitimately cannot edit a shared channel needs to know which attachment is
/// the obstacle — otherwise the only recovery is guesswork.
///
/// A channel with no rules attached is freely editable: it delivers nothing, so
/// there is no telemetry to redirect. That is also the ordinary create-then-
/// configure path, which must not require read on data the channel will never
/// carry.
async fn authorize_channel_retarget(
    conn: &mut sauron_db::AsyncPgConnection,
    user_id: Uuid,
    org_id: Uuid,
    channel_id: Uuid,
) -> Result<(), ApiError> {
    let rules = repo::rules_using_channel(conn, channel_id).await?;
    for r in &rules {
        // An unparseable stored trigger is treated as the strictest reading
        // rather than skipped: a row the code cannot classify is exactly the row
        // whose disclosure it cannot bound.
        let trigger = TriggerType::parse(&r.trigger_type).ok_or_else(|| {
            ApiError::Internal(format!("stored trigger type invalid: {}", r.trigger_type))
        })?;
        if !may_read_rule_target(conn, user_id, org_id, r.project_id, r.app_id, trigger).await? {
            return Err(ApiError::Forbidden(format!(
                "changing this channel's destination would redirect alerts from rule \"{}\", \
                 whose data you do not have read access to",
                r.name
            )));
        }
    }
    Ok(())
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
    if req.monitor_id.is_some()
        && !matches!(trigger, TriggerType::MonitorDown | TriggerType::MonitorUp)
    {
        return Err(ApiError::BadRequest(
            "monitor_id applies only to monitor_down / monitor_up triggers".into(),
        ));
    }
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
    let (project_id, app_id, monitor_id) = check_rule_scope(
        &mut conn,
        org_id,
        req.project_id,
        req.app_id,
        req.monitor_id,
    )
    .await?;
    authorize_rule_target(&mut conn, auth.user_id, org_id, project_id, app_id, trigger).await?;
    check_channels_in_org(&mut conn, org_id, &req.channel_ids).await?;

    let rule = repo::create_alert_rule(
        &mut conn,
        NewAlertRule {
            org_id,
            project_id,
            app_id,
            monitor_id,
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
    // The same telemetry gate `create_rule` applies, against the STORED target —
    // `UpdateRuleReq` deliberately has no `project_id`/`app_id`, so a rule
    // cannot be re-aimed (pinned by `the_scope_of_an_existing_rule_stays_immutable`
    // in `tests/http_alerting.rs`). Re-aiming is not the bypass; *re-routing* is.
    // `load_rule_authorized` checks org-scoped `alert:write` only, and every
    // remaining field of this request changes what that fixed target discloses
    // and to whom: `channel_ids` points the rule's alerts at channels the caller
    // controls, `enabled` revives a switched-off rule, `conditions` lowers the
    // threshold or drops a filter so it fires, `message_template` rewrites the
    // body. A caller who could not have CREATED this rule must not be able to
    // arrive at it by editing one, so the check belongs on both routes.
    //
    // Placed before body validation on purpose: a caller with no read on the
    // target gets one answer, not a validation oracle over channel ids
    // (`check_channels_in_org` reveals org membership of an id) or over what
    // `validate_conditions` accepts.
    authorize_rule_target(
        &mut conn,
        auth.user_id,
        rule.org_id,
        rule.project_id,
        rule.app_id,
        trigger,
    )
    .await?;

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

/// No `authorize_rule_target` here, unlike `create_rule`/`update_rule`, and
/// that asymmetry is deliberate: that helper gates *disclosure*, and deleting a
/// rule discloses nothing — it only stops future notifications. Requiring read
/// on the target to delete would leave an alerting operator unable to clean up
/// rules for projects they do not read, which is what `alert:write` is for.
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
    // `alert:read` gates *reaching* the history. It does not decide what is in
    // it: a row's `title`/`body` hold the same issue title or probed monitor
    // target that `authorize_rule_target` refuses to let an unauthorized caller
    // route anywhere, and this table is where those strings come to rest.
    authorize_org(&mut conn, auth.user_id, org_id, perm::ALERT_READ).await?;
    let (visible_rule_ids, orphan_triggers) =
        visible_history_keys(&mut conn, auth.user_id, org_id).await?;
    let rows = repo::list_alert_events_visible(
        &mut conn,
        org_id,
        &visible_rule_ids,
        &orphan_triggers,
        q.limit.unwrap_or(50),
        q.offset.unwrap_or(0),
    )
    .await?;
    Ok(Json(json!(rows)))
}

/// Resolve what of an org's alert history this caller may see.
///
/// Returns `(visible rule ids, trigger types whose orphaned rows are visible)`.
///
/// Two arms because `alert_events.rule_id` is `ON DELETE SET NULL`: a row whose
/// rule was deleted has no target left to check, and dropping those rows
/// outright would hide history an org owner is plainly entitled to. Instead an
/// orphan is treated as the **widest** possible target — org-scoped read of the
/// permission its own `trigger_type` implies — which is the same reasoning
/// `authorize_rule_target` applies to a rule narrowed to nothing. `trigger_type`
/// is a column on the event itself, so it survives the rule's deletion.
///
/// Cost is one authorization per *distinct* `(project, app, permission)` triple
/// rather than per rule, so a 200-rule org that targets four projects asks four
/// questions. `list_rules` already loads every rule in the org for its own
/// response, so the rule scan itself is no new burden.
async fn visible_history_keys(
    conn: &mut sauron_db::AsyncPgConnection,
    user_id: Uuid,
    org_id: Uuid,
) -> Result<(Vec<Uuid>, Vec<String>), ApiError> {
    use std::collections::HashMap;

    let rules = repo::list_alert_rules_for_org(conn, org_id).await?;
    let mut memo: HashMap<(Option<Uuid>, Option<Uuid>, &'static str), bool> = HashMap::new();
    let mut visible = Vec::new();
    for r in &rules {
        // An unparseable stored trigger yields no visibility rather than an
        // error: one bad row must not blank the whole page, but it must not be
        // shown either, since its disclosure cannot be classified.
        let Some(trigger) = TriggerType::parse(&r.trigger_type) else {
            continue;
        };
        // Keyed on the read permission, not the trigger: `rule_read_permission`
        // maps several triggers onto one permission, and asking the same
        // question again per trigger would multiply the queries for no answer.
        //
        // Safe despite `authorize_rule_target` DROPPING `app_id` for monitor
        // triggers — which would otherwise make two rules with identical key
        // fields ask different questions. It cannot happen here: the only triggers
        // that drop the app are `MonitorDown`/`MonitorUp`, and they are also the
        // only two that map to `monitor:read`. So every trigger sharing a
        // permission also shares the app-narrowing behaviour. Adding a
        // non-monitor trigger to `monitor:read`, or app-dropping under any other
        // permission, would break that and the key must then include the trigger.
        let key = (r.project_id, r.app_id, rule_read_permission(trigger));
        let allowed = match memo.get(&key) {
            Some(v) => *v,
            None => {
                let v =
                    may_read_rule_target(conn, user_id, org_id, r.project_id, r.app_id, trigger)
                        .await?;
                memo.insert(key, v);
                v
            }
        };
        if allowed {
            visible.push(r.id);
        }
    }

    // Orphans, by trigger type, at org scope — the widest reading.
    //
    // Goes through `may_read_rule_target` with `(None, None)` rather than calling
    // `authorize_org(...).is_ok()` directly: `is_ok()` would fold a dropped
    // connection into "not authorized" and render an empty page during an
    // outage, which is the failure `may_read_rule_target` documents refusing.
    //
    // Asked once per distinct PERMISSION, not once per trigger: the eight triggers
    // map onto three permissions, so this is three queries rather than eight for
    // an answer that cannot differ within a permission.
    let mut orphan_triggers = Vec::new();
    let mut org_perm: HashMap<&'static str, bool> = HashMap::new();
    for t in TriggerType::ALL {
        let perm_needed = rule_read_permission(t);
        let allowed = match org_perm.get(perm_needed) {
            Some(v) => *v,
            None => {
                let v = may_read_rule_target(conn, user_id, org_id, None, None, t).await?;
                org_perm.insert(perm_needed, v);
                v
            }
        };
        if allowed {
            orphan_triggers.push(t.as_str().to_string());
        }
    }
    Ok((visible, orphan_triggers))
}

/// Per-kind metadata for personal notification subscriptions.
///
/// Published here rather than hardcoded in Svelte for the same reason
/// `trigger_types` is: a kind added to `SubKind` without a matching dashboard
/// edit shows up as a missing option, not as a silently wrong form.
fn subscription_kinds_meta() -> serde_json::Value {
    use sauron_alerts::subscription::{SubConditions, SubKind};
    serde_json::Value::Array(
        SubKind::ALL
            .iter()
            .map(|k| {
                let scope_types = if k.allows_app_scope() {
                    json!(["project", "app"])
                } else {
                    json!(["project"])
                };
                let (defaults, clamps) = match k {
                    SubKind::Uptime => (json!({}), json!({})),
                    SubKind::ErrorSpike => (
                        json!({
                            "window_seconds": SubConditions::DEFAULT_WINDOW_SECONDS,
                            "factor": SubConditions::DEFAULT_FACTOR,
                            "min_count": SubConditions::DEFAULT_MIN_COUNT,
                            "level": serde_json::Value::Null,
                        }),
                        json!({
                            "window_seconds": [
                                SubConditions::MIN_WINDOW_SECONDS,
                                SubConditions::MAX_WINDOW_SECONDS
                            ],
                            "factor": [SubConditions::MIN_FACTOR, SubConditions::MAX_FACTOR],
                            "min_count": [
                                SubConditions::MIN_MIN_COUNT,
                                SubConditions::MAX_MIN_COUNT
                            ],
                        }),
                    ),
                    _ => (json!({ "level": "error" }), json!({})),
                };
                json!({
                    "key": k.as_str(),
                    "scope_types": scope_types,
                    "env_filter": k.supports_env_filter(),
                    "defaults": defaults,
                    "clamps": clamps,
                })
            })
            .collect(),
    )
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
        "subscription_kinds": subscription_kinds_meta(),
    })))
}

#[cfg(test)]
mod meta_tests {
    use super::*;

    /// The house convention is to publish enum/option metadata from
    /// `/v1/alert-meta` rather than hardcode lists in Svelte, so the dialog's
    /// conditional fields and its per-kind "the environment filter does not
    /// apply" notice both come from here.
    #[test]
    fn subscription_kinds_metadata_matches_the_enum() {
        let meta = subscription_kinds_meta();
        let arr = meta.as_array().expect("an array");
        assert_eq!(arr.len(), 4);

        let uptime = arr.iter().find(|k| k["key"] == "uptime").expect("uptime");
        assert_eq!(uptime["env_filter"], serde_json::json!(false));
        assert_eq!(uptime["scope_types"], serde_json::json!(["project"]));

        let spike = arr
            .iter()
            .find(|k| k["key"] == "error_spike")
            .expect("spike");
        assert_eq!(spike["env_filter"], serde_json::json!(true));
        assert_eq!(spike["scope_types"], serde_json::json!(["project", "app"]));
        assert_eq!(spike["defaults"]["window_seconds"], serde_json::json!(900));
        assert_eq!(spike["defaults"]["factor"], serde_json::json!(3.0));
        assert_eq!(spike["defaults"]["min_count"], serde_json::json!(10));
        assert_eq!(spike["clamps"]["factor"], serde_json::json!([1.5, 100.0]));

        let new_issue = arr
            .iter()
            .find(|k| k["key"] == "error_new_issue")
            .expect("new issue");
        assert_eq!(new_issue["defaults"]["level"], serde_json::json!("error"));
    }
}
