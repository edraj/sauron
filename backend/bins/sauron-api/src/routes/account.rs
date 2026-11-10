//! The caller's own account: which sessions exist, and how to end them.
//!
//! Everything here is gated on `AuthUser` and nothing else — a user's own
//! sessions are the definition of "any authenticated user", the same class as
//! `/v1/me`. The admin surface deliberately returns no session data at all.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use sauron_auth::{AuthError, AuthUser};
use sauron_db::models::AuthSession;
use sauron_db::repo;

use super::auth::rate_limit;
use super::db;
use crate::audit::{self, action, entity, Entry};
use crate::error::ApiError;
use crate::openapi::{ErrorResponse, OkResponse};
use crate::AppState;

/// Session actions allowed per user per window. Generous enough for a user
/// working through a list of devices, tight enough that the endpoint is not a
/// free write loop.
const SESSION_ACTIONS_PER_MIN: u32 = 20;

/// One row of the caller's session list.
///
/// Hand-built, never the `AuthSession` model: `revoked_by` is **never**
/// serialized. Surfacing it would tell a member which admin signed them out.
/// Surfacing `revoked_at` and `revoked_reason` is what makes the destructive
/// action observable to the person it happened to, which is the whole point of
/// writing those columns.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct SessionView {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
    pub last_used_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    /// Marked server-side so the dashboard never has to decode a JWT — it has no
    /// decoder dependency and should not gain one.
    pub current: bool,
    pub user_agent: Option<String>,
    pub browser: Option<String>,
    pub os: Option<String>,
    pub device_kind: Option<String>,
    /// Returned **unmasked**, not through the telemetry IP masker. This is the
    /// caller's own data, and `192.168.x.x` defeats the entire "was that login
    /// me?" purpose.
    pub ip: Option<String>,
    /// Present only when `?include_revoked=1`; NULL for live rows.
    pub revoked_at: Option<DateTime<Utc>>,
    pub revoked_reason: Option<String>,
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListSessionsQuery {
    /// Accepts `1` or `true`. Deliberately a `String` and not an `Option<bool>`:
    /// axum's `Query` deserializes through `serde_urlencoded`, which rejects `1`
    /// for a bool and 400s the whole request — and `?include_revoked=1` is the
    /// form the docs use.
    #[serde(default)]
    pub include_revoked: Option<String>,
}

fn truthy(v: Option<&String>) -> bool {
    matches!(v.map(String::as_str), Some("1") | Some("true"))
}

/// woothee returns the literal string `"UNKNOWN"` (and sometimes `""`) for
/// fields it cannot determine, so without this every unrecognised UA renders as
/// "UNKNOWN on UNKNOWN".
fn norm(s: &str) -> Option<String> {
    let t = s.trim();
    if t.is_empty() || t == "UNKNOWN" {
        None
    } else {
        Some(t.to_string())
    }
}

/// `(browser, os, device_kind)` for a user-agent string.
///
/// Pure, so it is unit tested here. Instantiating the parser per call is fine at
/// <=200 rows on a rarely-hit endpoint; do not add a `OnceLock` for it.
pub fn parse_ua(ua: Option<&str>) -> (Option<String>, Option<String>, Option<String>) {
    let Some(ua) = ua else {
        return (None, None, None);
    };
    let parser = woothee::parser::Parser::new();
    match parser.parse(ua) {
        Some(r) => (norm(r.name), norm(r.os), norm(r.category)),
        None => (None, None, None),
    }
}

fn to_view(row: AuthSession, current_sid: Option<Uuid>) -> SessionView {
    let (browser, os, device_kind) = parse_ua(row.user_agent.as_deref());
    SessionView {
        id: row.id,
        created_at: row.created_at,
        last_used_at: row.last_used_at,
        expires_at: row.expires_at,
        current: current_sid == Some(row.id),
        user_agent: row.user_agent,
        browser,
        os,
        device_kind,
        ip: row.ip,
        revoked_at: row.revoked_at,
        revoked_reason: row.revoked_reason,
    }
}

/// `GET /v1/me/sessions`
#[utoipa::path(
    get,
    path = "/v1/me/sessions",
    tag = "Account",
    summary = "List your own sign-in sessions",
    description = "\
Every live session for the calling user, newest first. The session the request \
was made with is flagged `current`, so a client never has to decode the JWT to \
identify it.

`ip` is returned **unmasked** here, unlike telemetry IPs elsewhere in the API. \
This is the caller's own data, and a masked address defeats the only question \
the list exists to answer — \"was that login me?\"",
    params(ListSessionsQuery),
    security(("bearerAuth" = [])),
    responses(
        (status = 200, description = "The caller's sessions.", body = Vec<SessionView>),
        (status = 401, description = "Missing, expired or revoked access token.", body = ErrorResponse),
    ),
)]
pub async fn list_sessions(
    auth: AuthUser,
    State(state): State<AppState>,
    Query(q): Query<ListSessionsQuery>,
) -> Result<Json<Vec<SessionView>>, ApiError> {
    let include_revoked = truthy(q.include_revoked.as_ref());
    let mut conn = db(&state).await?;
    let rows = repo::list_auth_sessions(&mut conn, auth.user_id, include_revoked).await?;
    drop(conn);
    let sid = auth.claims.sid;
    Ok(Json(rows.into_iter().map(|r| to_view(r, sid)).collect()))
}

/// `DELETE /v1/me/sessions/{session_id}`
#[utoipa::path(
    delete,
    path = "/v1/me/sessions/{session_id}",
    tag = "Account",
    summary = "Revoke one of your own sessions",
    description = "\
Ends a single session immediately. Revoking the current session is allowed and \
signs the caller out.

Revocation propagates to other API replicas on their next revocation poll \
rather than instantly, so a token may survive for a few seconds on a replica \
that has not yet refreshed.",
    params(("session_id" = Uuid, Path, description = "Session to revoke, from `GET /v1/me/sessions`.")),
    security(("bearerAuth" = [])),
    responses(
        (status = 200, description = "Session revoked.", body = OkResponse),
        (status = 401, description = "Missing, expired or revoked access token.", body = ErrorResponse),
        (status = 404, description = "No such session, or it belongs to another user.", body = ErrorResponse),
        (status = 429, description = "Too many revocations in a short window.", body = ErrorResponse),
    ),
)]
pub async fn revoke_session(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    rate_limit(
        &state,
        &format!("sauron:auth:sessions:{}", auth.user_id),
        SESSION_ACTIONS_PER_MIN,
        60,
    )
    .await?;

    // Refused rather than treated as a logout. Defensible either way, but an
    // identical-looking button in an identical-looking row would do something
    // categorically different, and Log out already exists in the Topbar.
    if auth.claims.sid == Some(session_id) {
        return Err(ApiError::Conflict(
            "cannot revoke the session you are using — use Log out instead".into(),
        ));
    }

    let mut conn = db(&state).await?;
    let ids = repo::revoke_session(
        &mut conn,
        session_id,
        auth.user_id,
        repo::REVOKE_USER_REVOKED,
        Some(auth.user_id),
    )
    .await?;
    drop(conn);

    // Absent, already revoked, or someone else's — all 404, never 403, so the
    // response cannot be used to probe which session ids exist.
    if ids.is_empty() {
        return Err(ApiError::NotFound);
    }

    state.revocations.mark_revoked(&ids);
    // Not optional. This is a destructive account action, and without the log
    // the only trace of a session ending is a row that stops being listed.
    tracing::warn!(user_id = %auth.user_id, %session_id, "session revoked by user");
    Ok(Json(serde_json::json!({ "ok": true, "revoked": 1 })))
}

/// `POST /v1/me/sessions/revoke-others`
///
/// No request body — the dashboard sends `{}` and axum needs no `Json`
/// extractor to ignore it.
#[utoipa::path(
    post,
    path = "/v1/me/sessions/revoke-others",
    tag = "Account",
    summary = "Revoke every session except this one",
    description = "\
The \"sign out everywhere else\" action. Keeps the session that made the \
request alive, so the caller is not signed out by their own cleanup.

Returns how many sessions were ended, which is what lets a client show \
\"3 other devices signed out\" rather than a bare acknowledgement.",
    security(("bearerAuth" = [])),
    responses(
        (status = 200, description = "Other sessions revoked; `revoked` counts them.",
         body = OkResponse, example = json!({ "ok": true, "revoked": 3 })),
        (status = 401, description = "Missing, expired or revoked access token.", body = ErrorResponse),
    ),
)]
pub async fn revoke_other_sessions(
    auth: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    rate_limit(
        &state,
        &format!("sauron:auth:sessions:{}", auth.user_id),
        SESSION_ACTIONS_PER_MIN,
        60,
    )
    .await?;

    // A sid-less legacy token cannot name the session to spare, and sparing
    // nothing would log the caller out of the tab they are looking at. This
    // cannot outlive `JWT_ACCESS_TTL_SECS` past the deploy.
    let Some(sid) = auth.claims.sid else {
        return Err(ApiError::BadRequest(
            "your session predates this feature; reload the dashboard and try again".into(),
        ));
    };

    let mut conn = db(&state).await?;
    let ids = repo::revoke_sessions_for_user(
        &mut conn,
        auth.user_id,
        Some(sid),
        repo::REVOKE_USER_REVOKED_OTHERS,
        Some(auth.user_id),
    )
    .await?;
    drop(conn);

    state.revocations.mark_revoked(&ids);
    tracing::warn!(user_id = %auth.user_id, revoked = ids.len(), "user revoked other sessions");
    Ok(Json(
        serde_json::json!({ "ok": true, "revoked": ids.len() }),
    ))
}

/// Silences an unused-import warning when the module compiles without touching
/// `AuthError` directly; the type is part of this module's error vocabulary via
/// `ApiError::Auth`.
const _: fn(AuthError) -> ApiError = ApiError::Auth;

// ---------------------------------------------------------------------------
// Personal API keys (MCP access)
// ---------------------------------------------------------------------------

/// Live (unrevoked) keys one user may hold.
const MAX_LIVE_API_KEYS: i64 = 25;

/// One row of the caller's key list. Hand-built from `ApiKey`, which has no
/// `Serialize` on purpose: the hash can never be reached by a derive.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ApiKeyView {
    pub id: Uuid,
    pub name: String,
    /// First 17 characters of the token, enough to recognise a key.
    pub prefix: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub last_used_at: Option<DateTime<Utc>>,
    /// `expires_at <= now`. Expired keys stay listed until revoked.
    pub expired: bool,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct CreateApiKeyReq {
    /// 1-64 characters after trimming.
    pub name: String,
    /// One of `30d`, `90d`, `1y`, `never`.
    pub expiry: String,
}

/// Returned once, at creation. The only response that ever carries `token`.
///
/// `Debug` is hand-written so the plaintext token can never reach a log line
/// through `{:?}` (a `tracing` field, a panic message, a test failure).
#[derive(Serialize, utoipa::ToSchema)]
pub struct CreatedApiKey {
    pub key: ApiKeyView,
    pub token: String,
}

impl std::fmt::Debug for CreatedApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CreatedApiKey")
            .field("key", &self.key)
            .field("token", &sauron_auth::api_key::REDACTED_TOKEN)
            .finish()
    }
}

fn api_key_view(k: &sauron_db::models::ApiKey) -> ApiKeyView {
    ApiKeyView {
        id: k.id,
        name: k.name.clone(),
        prefix: k.token_prefix.clone(),
        created_at: k.created_at,
        expires_at: k.expires_at,
        last_used_at: k.last_used_at,
        expired: k.expires_at.is_some_and(|t| t <= Utc::now()),
    }
}

fn parse_api_key_expiry(expiry: &str) -> Result<Option<DateTime<Utc>>, ApiError> {
    let days = match expiry {
        "30d" => 30,
        "90d" => 90,
        "1y" => 365,
        "never" => return Ok(None),
        _ => {
            return Err(ApiError::BadRequest(
                "expiry must be one of 30d, 90d, 1y, never".into(),
            ))
        }
    };
    Ok(Some(Utc::now() + chrono::Duration::days(days)))
}

/// `access_key.create`'s changes: the key's fields as `{from: null, to: …}`.
fn api_key_created_changes(k: &sauron_db::models::ApiKey) -> serde_json::Value {
    audit::created(
        entity::ACCESS_KEY,
        &[
            ("name", serde_json::json!(k.name)),
            ("prefix", serde_json::json!(k.token_prefix)),
            ("expires_at", serde_json::json!(k.expires_at)),
        ],
    )
}

/// `access_key.revoke`'s changes: the same fields as `{from: …, to: null}`,
/// the removal shape every delete in the trail uses (`role.delete`,
/// `grant.delete`, `monitor.delete`), so the Wall of Shame shows the key
/// going away rather than a second creation. `diff` drops a pair whose sides
/// are equal, so a never-expiring key's `expires_at` (null before and after)
/// is left out.
fn api_key_revoked_changes(k: &sauron_db::models::ApiKey) -> serde_json::Value {
    audit::diff(
        entity::ACCESS_KEY,
        &[
            ("name", serde_json::json!(k.name), serde_json::Value::Null),
            (
                "prefix",
                serde_json::json!(k.token_prefix),
                serde_json::Value::Null,
            ),
            (
                "expires_at",
                serde_json::json!(k.expires_at),
                serde_json::Value::Null,
            ),
        ],
    )
}

/// `GET /v1/me/api-keys`
#[utoipa::path(
    get,
    path = "/v1/me/api-keys",
    tag = "Account",
    summary = "List your personal API keys",
    description = "\
Unrevoked keys for the calling user (expired ones included), newest first. \
Only the display prefix is returned; the token itself is shown once, at \
creation.",
    security(("bearerAuth" = [])),
    responses(
        (status = 200, description = "The caller's unrevoked keys (expired ones included).", body = Vec<ApiKeyView>),
        (status = 401, description = "Missing, expired or revoked access token.", body = ErrorResponse),
    ),
)]
pub async fn list_api_keys(
    auth: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<Vec<ApiKeyView>>, ApiError> {
    let mut conn = db(&state).await?;
    let rows = sauron_db::api_keys::list_for_user(&mut conn, auth.user_id).await?;
    Ok(Json(rows.iter().map(api_key_view).collect()))
}

/// `POST /v1/me/api-keys`
#[utoipa::path(
    post,
    path = "/v1/me/api-keys",
    tag = "Account",
    summary = "Create a personal API key",
    description = "\
Mints a key for the read-only MCP endpoint. The `token` in the response is \
shown **once** and cannot be retrieved later. At most 25 unrevoked keys per user \
(expired keys count until revoked).",
    request_body = CreateApiKeyReq,
    security(("bearerAuth" = [])),
    responses(
        (status = 201, description = "Key created.", body = CreatedApiKey),
        (status = 400, description = "Bad name or expiry.", body = ErrorResponse),
        (status = 401, description = "Missing, expired or revoked access token.", body = ErrorResponse),
        (status = 409, description = "The caller already has 25 unrevoked keys (expired ones included).", body = ErrorResponse),
        (status = 429, description = "Too many key actions in a short window.", body = ErrorResponse),
    ),
)]
pub async fn create_api_key(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(req): Json<CreateApiKeyReq>,
) -> Result<(StatusCode, Json<CreatedApiKey>), ApiError> {
    rate_limit(
        &state,
        &format!("sauron:auth:apikeys:{}", auth.user_id),
        SESSION_ACTIONS_PER_MIN,
        60,
    )
    .await?;

    let name = req.name.trim();
    let len = name.chars().count();
    if !(1..=64).contains(&len) {
        return Err(ApiError::BadRequest("name must be 1-64 characters".into()));
    }
    let expires_at = parse_api_key_expiry(&req.expiry)?;

    let mut conn = db(&state).await?;
    if sauron_db::api_keys::count_live_for_user(&mut conn, auth.user_id).await? >= MAX_LIVE_API_KEYS
    {
        // `count_live_for_user` counts unrevoked keys, expired ones included
        // (they stay listed until revoked), so the message says so.
        return Err(ApiError::Conflict(
            "you already have 25 unrevoked keys (expired keys count too); revoke one first".into(),
        ));
    }
    let minted = sauron_auth::api_key::mint_api_key();
    let key = sauron_db::api_keys::insert(
        &mut conn,
        auth.user_id,
        name,
        &minted.prefix,
        &minted.hash,
        expires_at,
    )
    .await?;

    audit::record_for_user_orgs(
        &mut conn,
        auth.user_id,
        Entry::new(Uuid::nil(), action::ACCESS_KEY_CREATE, entity::ACCESS_KEY)
            .target(key.id, &key.name)
            .changes(api_key_created_changes(&key)),
    )
    .await;
    drop(conn);

    Ok((
        StatusCode::CREATED,
        Json(CreatedApiKey {
            key: api_key_view(&key),
            token: minted.token,
        }),
    ))
}

/// `DELETE /v1/me/api-keys/{key_id}`
#[utoipa::path(
    delete,
    path = "/v1/me/api-keys/{key_id}",
    tag = "Account",
    summary = "Revoke one of your personal API keys",
    description = "Revokes the key immediately. A missing, foreign or already-revoked key is a 404.",
    params(("key_id" = Uuid, Path, description = "Key to revoke, from `GET /v1/me/api-keys`.")),
    security(("bearerAuth" = [])),
    responses(
        (status = 200, description = "Key revoked.", body = OkResponse),
        (status = 401, description = "Missing, expired or revoked access token.", body = ErrorResponse),
        (status = 404, description = "No such live key, or it belongs to another user.", body = ErrorResponse),
        (status = 429, description = "Too many key actions in a short window.", body = ErrorResponse),
    ),
)]
pub async fn delete_api_key(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(key_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    rate_limit(
        &state,
        &format!("sauron:auth:apikeys:{}", auth.user_id),
        SESSION_ACTIONS_PER_MIN,
        60,
    )
    .await?;

    let mut conn = db(&state).await?;
    let key = sauron_db::api_keys::revoke(&mut conn, auth.user_id, key_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    audit::record_for_user_orgs(
        &mut conn,
        auth.user_id,
        Entry::new(Uuid::nil(), action::ACCESS_KEY_REVOKE, entity::ACCESS_KEY)
            .target(key.id, &key.name)
            .changes(api_key_revoked_changes(&key)),
    )
    .await;
    drop(conn);
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHROME_MAC: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
                              (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";
    const SAFARI_IOS: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_2 like Mac OS X) \
                              AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.2 \
                              Mobile/15E148 Safari/604.1";

    #[test]
    fn parse_ua_names_a_real_desktop_browser() {
        let (browser, os, kind) = parse_ua(Some(CHROME_MAC));
        assert_eq!(browser.as_deref(), Some("Chrome"));
        assert_eq!(os.as_deref(), Some("Mac OSX"));
        assert_eq!(kind.as_deref(), Some("pc"));
    }

    #[test]
    fn parse_ua_names_a_real_mobile_browser() {
        let (browser, os, kind) = parse_ua(Some(SAFARI_IOS));
        assert_eq!(browser.as_deref(), Some("Safari"));
        assert_eq!(os.as_deref(), Some("iPhone"));
        assert_eq!(kind.as_deref(), Some("smartphone"));
    }

    #[test]
    fn woothees_unknown_sentinels_become_none() {
        // The gotcha this function exists for: woothee answers with the literal
        // string "UNKNOWN" rather than an absence, so a naive mapping renders
        // every unrecognised device as "UNKNOWN on UNKNOWN".
        for ua in [Some("qwertyuiop"), Some(""), None] {
            let (browser, os, kind) = parse_ua(ua);
            assert_eq!(browser, None, "browser for {ua:?}");
            assert_eq!(os, None, "os for {ua:?}");
            assert_eq!(kind, None, "device_kind for {ua:?}");
        }
    }

    fn api_key(expires_at: Option<DateTime<Utc>>) -> sauron_db::models::ApiKey {
        let now = Utc::now();
        sauron_db::models::ApiKey {
            id: Uuid::nil(),
            user_id: Uuid::nil(),
            name: "laptop".into(),
            token_prefix: "sauron_pat_abc123".into(),
            token_hash: "0".repeat(64),
            created_at: now,
            expires_at,
            last_used_at: None,
            revoked_at: None,
        }
    }

    /// A revoke reads as a removal (`{from: value, to: null}`), the shape
    /// every delete in the trail uses, not as a second creation.
    #[test]
    fn api_key_revoke_is_audited_as_a_removal_not_a_creation() {
        let exp: DateTime<Utc> = "2027-01-01T00:00:00Z".parse().unwrap();
        let k = api_key(Some(exp));
        let created = api_key_created_changes(&k);
        assert_eq!(
            created["name"],
            serde_json::json!({"from": null, "to": "laptop"})
        );
        let revoked = api_key_revoked_changes(&k);
        assert_eq!(
            revoked["name"],
            serde_json::json!({"from": "laptop", "to": null})
        );
        assert_eq!(
            revoked["prefix"],
            serde_json::json!({"from": "sauron_pat_abc123", "to": null})
        );
        assert_eq!(revoked["expires_at"]["from"], serde_json::json!(exp));
        assert!(revoked["expires_at"]["to"].is_null());
        // A never-expiring key has nothing to say about expiry on revoke.
        let never = api_key_revoked_changes(&api_key(None));
        assert!(never.get("expires_at").is_none(), "{never}");
        assert_eq!(never["name"]["to"], serde_json::Value::Null);
    }

    #[test]
    fn created_api_key_debug_redacts_the_token() {
        let minted = sauron_auth::api_key::mint_api_key();
        let created = CreatedApiKey {
            key: api_key_view(&api_key(None)),
            token: minted.token.clone(),
        };
        for shown in [format!("{created:?}"), format!("{created:#?}")] {
            assert!(!shown.contains(&minted.token), "{shown}");
            assert!(shown.contains("sauron_pat_…[redacted]"), "{shown}");
            assert!(shown.contains("laptop"), "the rest still prints: {shown}");
        }
    }

    #[test]
    fn include_revoked_accepts_both_spellings_and_nothing_else() {
        assert!(truthy(Some(&"1".to_string())));
        assert!(truthy(Some(&"true".to_string())));
        assert!(!truthy(Some(&"0".to_string())));
        assert!(!truthy(Some(&"yes".to_string())));
        assert!(!truthy(None));
    }
}
