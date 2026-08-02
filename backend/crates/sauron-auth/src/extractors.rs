//! The `AuthUser` axum extractor, the `AuthError` response type, and the
//! org/project authorization helpers handlers call after extracting the user.

use axum::extract::{FromRef, FromRequestParts};
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use uuid::Uuid;

use crate::jwt::{Claims, JwtKeys};
use crate::revocations::SessionRevocations;

/// The only paths a temp-password holder may reach.
///
/// The allowlist is matched on the exact path. If the API is ever mounted
/// under a prefix, this has to become a suffix match; noted rather than
/// generalised now.
///
/// `/v1/auth/logout` is listed defensively only: it currently takes the
/// refresh token in the body and no `AuthUser`, so it never reaches this
/// gate. Listing it means it stays reachable if it later gains the
/// extractor. `/v1/auth/refresh` is deliberately absent for the same reason
/// inverted — it is likewise unauthenticated, and listing it would wrongly
/// suggest a temp-password holder can rotate into a clean token.
fn password_change_allowed_path(path: &str) -> bool {
    matches!(path, "/v1/auth/password" | "/v1/auth/logout")
}

/// Pure decision behind the `must_change_password` gate: does this caller,
/// on this path, get through? Extracted so the reject branch is unit
/// testable without an axum `Parts` fixture — deleting the gate's logic
/// should turn the test suite red.
fn password_change_gate(must_change_password: bool, path: &str) -> Result<(), AuthError> {
    if must_change_password && !password_change_allowed_path(path) {
        return Err(AuthError::PasswordChangeRequired);
    }
    Ok(())
}

/// Authentication / authorization failure, rendered as a JSON error response.
#[derive(Debug, Clone, Copy)]
pub enum AuthError {
    MissingToken,
    InvalidToken,
    /// Wrong email/password at login. Kept distinct from `InvalidToken` so the
    /// client sees an accurate "invalid email or password" instead of a
    /// misleading "invalid or expired token". Deliberately does not reveal
    /// whether the email exists (no user-enumeration).
    InvalidCredentials,
    Forbidden,
    /// The account exists and the password was correct, but an admin disabled
    /// it. Only ever returned *after* a successful password verification.
    AccountDeactivated,
    /// The caller holds a temp password and must replace it before doing
    /// anything else.
    PasswordChangeRequired,
    /// The password was correct, but an admin invalidated this credential and
    /// the replacement has not been chosen yet. Only ever returned *after* a
    /// successful password verification — placing it before would answer in
    /// microseconds for a reset-pending account and in tens of milliseconds for
    /// every other one, handing back the enumeration oracle `spend_dummy_verify`
    /// was written to close, and leaking to anyone who can type an address that
    /// a particular person is mid-lockout.
    PasswordResetRequired,
    NotFound,
    Internal,
}

impl AuthError {
    fn parts(self) -> (StatusCode, &'static str, &'static str) {
        match self {
            AuthError::MissingToken => (
                StatusCode::UNAUTHORIZED,
                "missing_token",
                "authorization required",
            ),
            AuthError::InvalidToken => (
                StatusCode::UNAUTHORIZED,
                "invalid_token",
                "invalid or expired token",
            ),
            AuthError::InvalidCredentials => (
                StatusCode::UNAUTHORIZED,
                "invalid_credentials",
                "invalid email or password",
            ),
            AuthError::Forbidden => (StatusCode::FORBIDDEN, "forbidden", "you do not have access"),
            AuthError::AccountDeactivated => (
                StatusCode::FORBIDDEN,
                "account_deactivated",
                "this account has been deactivated",
            ),
            AuthError::PasswordChangeRequired => (
                StatusCode::FORBIDDEN,
                "password_change_required",
                "you must change your password before continuing",
            ),
            AuthError::PasswordResetRequired => (
                StatusCode::FORBIDDEN,
                "password_reset_required",
                "an administrator reset this password — check your email for the link",
            ),
            AuthError::NotFound => (StatusCode::NOT_FOUND, "not_found", "resource not found"),
            AuthError::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "internal error",
            ),
        }
    }
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        let (status, code, message) = self.parts();
        (
            status,
            Json(json!({ "error": { "code": code, "message": message } })),
        )
            .into_response()
    }
}

/// The authenticated user, extracted from a `Bearer` access token. Any axum
/// state that exposes [`JwtKeys`] via [`FromRef`] can use this extractor.
#[derive(Debug, Clone)]
pub struct AuthUser {
    pub user_id: Uuid,
    pub claims: Claims,
}

impl<S> FromRequestParts<S> for AuthUser
where
    S: Send + Sync,
    JwtKeys: FromRef<S>,
    SessionRevocations: FromRef<S>,
{
    type Rejection = AuthError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let keys = JwtKeys::from_ref(state);
        let header = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .ok_or(AuthError::MissingToken)?;
        let token = header
            .strip_prefix("Bearer ")
            .or_else(|| header.strip_prefix("bearer "))
            .ok_or(AuthError::InvalidToken)?;
        let claims = keys
            .decode_access(token)
            .map_err(|_| AuthError::InvalidToken)?;
        let user_id = Uuid::parse_str(&claims.sub).map_err(|_| AuthError::InvalidToken)?;

        // Before the password gate, not after: a revoked session must 401 on
        // EVERY path including `/v1/auth/password`, or a revoked temp-password
        // holder could still change the password. `AuthError::InvalidToken` is
        // the right code and needs no dashboard change — the 401 interceptor
        // calls `runRefreshOnce()`, whose refresh row is also revoked, so
        // `/v1/auth/refresh` 401s and `onRefreshFailure()` sends the user to
        // `#/login`.
        //
        // A token with no `sid` predates migration 000035 and is accepted
        // unchanged; that cannot last more than `JWT_ACCESS_TTL_SECS` past the
        // deploy, because `validate_exp` is on and every login and refresh mints
        // one. Rejecting them instead would sign out every logged-in user at
        // deploy.
        if let Some(sid) = claims.sid {
            if SessionRevocations::from_ref(state).contains(&sid) {
                return Err(AuthError::InvalidToken);
            }
        }

        // A temp password may do exactly one thing: become a real one.
        // Enforcing this in the extractor rather than in the dashboard is the
        // point — a UI redirect is bypassable with curl, which would leave the
        // admin who generated the password holding a working credential for
        // somebody else's account.
        password_change_gate(claims.must_change_password, parts.uri.path())?;

        Ok(AuthUser { user_id, claims })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_credentials_maps_to_401_with_accurate_message() {
        let (status, code, message) = AuthError::InvalidCredentials.parts();
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(code, "invalid_credentials");
        assert_eq!(message, "invalid email or password");
    }

    #[test]
    fn credentials_and_token_errors_are_distinct() {
        // A login failure must not masquerade as a token problem.
        assert_ne!(
            AuthError::InvalidCredentials.parts().1,
            AuthError::InvalidToken.parts().1
        );
    }

    #[test]
    fn password_change_allowlist_is_exactly_two_paths() {
        assert!(password_change_allowed_path("/v1/auth/password"));
        assert!(password_change_allowed_path("/v1/auth/logout"));
        // Everything a temp-password holder might otherwise reach.
        for p in [
            "/v1/orgs",
            "/v1/auth/refresh",
            "/v1/projects",
            "/v1/admin/storage",
            "/v1/auth/passwordx",
            // Deliberately unauthenticated, so `password_change_gate` is never
            // reached for either and neither must ever need the allowlist. A
            // future change that bolts an extractor onto one of them turns this
            // red instead of silently 403ing every reset for exactly the
            // population that needs one.
            "/v1/auth/forgot-password",
            "/v1/auth/reset-password",
        ] {
            assert!(
                !password_change_allowed_path(p),
                "{p} must not be reachable"
            );
        }
    }

    #[test]
    fn password_change_gate_blocks_temp_password_outside_allowlist() {
        // The case that matters: a temp-password holder hitting an arbitrary
        // endpoint must be rejected. This exercises the gate function the
        // extractor actually calls, not a re-declared copy of its predicate —
        // deleting the gate's `if` in `from_request_parts` turns this red.
        assert!(matches!(
            password_change_gate(true, "/v1/orgs"),
            Err(AuthError::PasswordChangeRequired)
        ));
    }

    #[test]
    fn password_change_gate_allows_temp_password_on_change_endpoint() {
        assert!(password_change_gate(true, "/v1/auth/password").is_ok());
    }

    #[test]
    fn password_change_gate_allows_normal_user_everywhere() {
        assert!(password_change_gate(false, "/v1/orgs").is_ok());
    }

    #[test]
    fn deactivated_and_change_required_are_distinct_forbidden_codes() {
        let (s1, c1, _) = AuthError::AccountDeactivated.parts();
        let (s2, c2, _) = AuthError::PasswordChangeRequired.parts();
        assert_eq!(s1, StatusCode::FORBIDDEN);
        assert_eq!(s2, StatusCode::FORBIDDEN);
        assert_ne!(c1, c2);
        // The dashboard routes on these codes; a rename is a breaking change.
        assert_eq!(c1, "account_deactivated");
        assert_eq!(c2, "password_change_required");
    }

    #[test]
    fn password_reset_required_maps_to_403_with_its_own_code() {
        let (status, code, message) = AuthError::PasswordResetRequired.parts();
        assert_eq!(status, StatusCode::FORBIDDEN);
        // The dashboard's `isPasswordResetRequired` branches on this exact
        // string to swap the login form for the emailed-link panel. A rename
        // would otherwise only be caught by a human clicking through a
        // locked-out login.
        assert_eq!(code, "password_reset_required");
        assert_eq!(
            message,
            "an administrator reset this password — check your email for the link"
        );
        // Must not collide with the temp-password gate: the two names invite
        // exactly this confusion and the two panels say opposite things.
        assert_ne!(code, AuthError::PasswordChangeRequired.parts().1);
        assert_ne!(code, AuthError::AccountDeactivated.parts().1);
    }
}
