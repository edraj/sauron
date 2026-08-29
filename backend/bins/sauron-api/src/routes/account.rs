//! The caller's own account: which sessions exist, and how to end them.
//!
//! Everything here is gated on `AuthUser` and nothing else — a user's own
//! sessions are the definition of "any authenticated user", the same class as
//! `/v1/me`. The admin surface deliberately returns no session data at all.

use axum::extract::{Path, Query, State};
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use sauron_auth::{AuthError, AuthUser};
use sauron_db::models::AuthSession;
use sauron_db::repo;

use super::auth::rate_limit;
use super::db;
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

    #[test]
    fn include_revoked_accepts_both_spellings_and_nothing_else() {
        assert!(truthy(Some(&"1".to_string())));
        assert!(truthy(Some(&"true".to_string())));
        assert!(!truthy(Some(&"0".to_string())));
        assert!(!truthy(Some(&"yes".to_string())));
        assert!(!truthy(None));
    }
}
