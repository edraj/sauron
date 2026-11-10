//! Personal-API-key authentication for `/v1/mcp`.
//!
//! A key authenticates this endpoint and nothing else: the `AuthUser`
//! extractor on every REST route decodes JWTs only, and this middleware
//! accepts `sauron_pat_` keys only.
//!
//! Every refusal uses the dashboard's `{"error":{code,message}}` body. A
//! missing, malformed, unknown, revoked or expired key is the SAME 401, so the
//! response never says whether a key exists. The owner-state refusals (403)
//! are only reachable with a live key, so they disclose nothing to someone who
//! does not already hold one.

use axum::extract::{Request, State};
use axum::http::header::{AUTHORIZATION, WWW_AUTHENTICATE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use uuid::Uuid;

use sauron_auth::AuthError;
use sauron_db::api_keys::{self, KeyOwner};

use crate::error::ApiError;
use crate::routes::auth::within_budget;
use crate::AppState;

/// The challenge on every 401, so an MCP client recognises an auth failure
/// (and not a server fault) and tells the developer to fix the key.
const BEARER_CHALLENGE: &str = r#"Bearer realm="sauron""#;

/// Fixed-window length of the per-key limiter, seconds.
const RATE_WINDOW_SECS: u64 = 60;

/// Who is calling, inserted into the request extensions by
/// [`require_api_key`]. rmcp forwards the request's `http::request::Parts`
/// (extensions included) to every tool; read it back with [`caller`].
#[derive(Clone, Debug)]
pub(crate) struct McpCaller {
    pub user_id: Uuid,
    /// Which key made the call: its id, logged with every tool failure
    /// (`ToolError::into_result`). The token itself is never carried here.
    pub key_id: Uuid,
}

/// The caller of a tool, from the parts rmcp forwards. A tool takes them as
/// `Extension(parts): Extension<http::request::Parts>`.
///
/// Missing means the tool was reached without passing [`require_api_key`],
/// which the router makes impossible; it is reported as an internal error, and
/// never treated as an anonymous caller.
pub(crate) fn caller(parts: &axum::http::request::Parts) -> Result<McpCaller, rmcp::ErrorData> {
    parts
        .extensions
        .get::<McpCaller>()
        .cloned()
        .ok_or_else(|| rmcp::ErrorData::internal_error("request is not authenticated", None))
}

/// Authenticate a personal API key, then hand the request to the MCP service.
pub(crate) async fn require_api_key(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Response {
    match authenticate(&state, req.headers()).await {
        Ok(caller) => {
            req.extensions_mut().insert(caller);
            next.run(req).await
        }
        Err(e) => refusal(e),
    }
}

/// `ApiError::Auth(AuthError::InvalidToken)`: the one 401 for a missing,
/// malformed, unknown, revoked or expired key.
const UNAUTHENTICATED: ApiError = ApiError::Auth(AuthError::InvalidToken);

async fn authenticate(state: &AppState, headers: &HeaderMap) -> Result<McpCaller, ApiError> {
    // Shape first: a dashboard JWT (or anything else that is not a key) never
    // reaches the database, and gets the same answer as a wrong key.
    let token = bearer_token(headers)
        .filter(|t| sauron_auth::looks_like_api_key(t))
        .ok_or(UNAUTHENTICATED)?;
    let hash = sauron_auth::hash_token(token);

    let mut conn = crate::routes::db(state).await?;
    let owner = api_keys::authenticate(&mut conn, &hash)
        .await?
        .ok_or(UNAUTHENTICATED)?;

    owner_may_call(&owner)?;

    let bucket = format!("sauron:mcp:calls:{}", owner.key_id);
    if !within_budget(
        state,
        &bucket,
        state.cfg.mcp_calls_per_min,
        RATE_WINDOW_SECS,
    )
    .await
    {
        return Err(ApiError::RateLimited);
    }

    // Bookkeeping only. The write is throttled to once a minute in SQL, and a
    // failure must not turn a valid call into an error.
    if let Err(e) = api_keys::touch_last_used(&mut conn, owner.key_id).await {
        tracing::warn!(key_id = %owner.key_id, error = %e, "mcp: could not record api key use");
    }

    Ok(McpCaller {
        user_id: owner.user_id,
        key_id: owner.key_id,
    })
}

/// The login rules, re-checked on every call because a key outlives any one
/// session. Order follows `/v1/auth/login`: deactivation, then an
/// admin-forced reset, then a temp password. A temp-password holder is refused
/// outright, mirroring `password_change_gate`: MCP is not one of the two paths
/// it allows.
///
/// None of these revokes the key. Reactivating the account, finishing the
/// reset or choosing a real password makes the same key work again.
fn owner_may_call(owner: &KeyOwner) -> Result<(), AuthError> {
    if !owner.is_active {
        return Err(AuthError::AccountDeactivated);
    }
    if owner.credentials_invalidated {
        return Err(AuthError::PasswordResetRequired);
    }
    if owner.must_change_password {
        return Err(AuthError::PasswordChangeRequired);
    }
    Ok(())
}

/// The bearer credential, parsed the way the `AuthUser` extractor parses it.
fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let header = headers.get(AUTHORIZATION)?.to_str().ok()?;
    header
        .strip_prefix("Bearer ")
        .or_else(|| header.strip_prefix("bearer "))
}

/// The refusal as a response. Every 401 gets the bearer challenge here, in
/// one place, whichever branch produced it.
fn refusal(e: ApiError) -> Response {
    let mut resp = e.into_response();
    if resp.status() == StatusCode::UNAUTHORIZED {
        resp.headers_mut()
            .insert(WWW_AUTHENTICATE, HeaderValue::from_static(BEARER_CHALLENGE));
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owner(is_active: bool, must_change: bool, invalidated: bool) -> KeyOwner {
        KeyOwner {
            key_id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            is_active,
            must_change_password: must_change,
            credentials_invalidated: invalidated,
        }
    }

    fn headers(authorization: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(AUTHORIZATION, HeaderValue::from_str(authorization).unwrap());
        h
    }

    #[test]
    fn bearer_token_accepts_both_scheme_spellings_like_auth_user() {
        assert_eq!(bearer_token(&headers("Bearer abc")), Some("abc"));
        assert_eq!(bearer_token(&headers("bearer abc")), Some("abc"));
        assert_eq!(bearer_token(&headers("Basic abc")), None);
        assert_eq!(bearer_token(&HeaderMap::new()), None);
    }

    #[test]
    fn every_401_carries_the_bearer_challenge_and_nothing_else_does() {
        let resp = refusal(UNAUTHENTICATED);
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            resp.headers().get(WWW_AUTHENTICATE).unwrap(),
            r#"Bearer realm="sauron""#
        );
        for e in [
            ApiError::Auth(AuthError::AccountDeactivated),
            ApiError::RateLimited,
        ] {
            let resp = refusal(e);
            assert_ne!(resp.status(), StatusCode::UNAUTHORIZED);
            assert!(resp.headers().get(WWW_AUTHENTICATE).is_none());
        }
    }

    #[test]
    fn owner_state_maps_to_the_login_refusals_in_login_order() {
        assert!(owner_may_call(&owner(true, false, false)).is_ok());
        assert!(matches!(
            owner_may_call(&owner(false, true, true)),
            Err(AuthError::AccountDeactivated)
        ));
        assert!(matches!(
            owner_may_call(&owner(true, true, true)),
            Err(AuthError::PasswordResetRequired)
        ));
        assert!(matches!(
            owner_may_call(&owner(true, true, false)),
            Err(AuthError::PasswordChangeRequired)
        ));
    }

    #[test]
    fn caller_reads_the_middleware_extension_and_refuses_without_it() {
        let (mut parts, ()) = axum::http::Request::new(()).into_parts();
        let err = caller(&parts).expect_err("no extension, no caller");
        assert_eq!(err.code, rmcp::model::ErrorCode::INTERNAL_ERROR);

        let inserted = McpCaller {
            user_id: Uuid::new_v4(),
            key_id: Uuid::new_v4(),
        };
        parts.extensions.insert(inserted.clone());
        let got = caller(&parts).expect("extension present");
        assert_eq!(got.user_id, inserted.user_id);
        assert_eq!(got.key_id, inserted.key_id);
    }
}
