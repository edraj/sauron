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

        // A temp password may do exactly one thing: become a real one.
        // Enforcing this in the extractor rather than in the dashboard is the
        // point — a UI redirect is bypassable with curl, which would leave the
        // admin who generated the password holding a working credential for
        // somebody else's account.
        //
        // The allowlist is matched on the exact path. If the API is ever mounted
        // under a prefix, this has to become a suffix match; noted rather than
        // generalised now.
        //
        // `/v1/auth/logout` is listed defensively only: it currently takes the
        // refresh token in the body and no `AuthUser`, so it never reaches this
        // gate. Listing it means it stays reachable if it later gains the
        // extractor. `/v1/auth/refresh` is deliberately absent for the same
        // reason inverted — it is likewise unauthenticated, and listing it would
        // wrongly suggest a temp-password holder can rotate into a clean token.
        if claims.must_change_password {
            let path = parts.uri.path();
            let allowed = matches!(path, "/v1/auth/password" | "/v1/auth/logout");
            if !allowed {
                return Err(AuthError::PasswordChangeRequired);
            }
        }

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
        let allowed = |p: &str| matches!(p, "/v1/auth/password" | "/v1/auth/logout");
        assert!(allowed("/v1/auth/password"));
        assert!(allowed("/v1/auth/logout"));
        // Everything a temp-password holder might otherwise reach.
        for p in [
            "/v1/orgs",
            "/v1/auth/refresh",
            "/v1/projects",
            "/v1/admin/storage",
            "/v1/auth/passwordx",
        ] {
            assert!(!allowed(p), "{p} must not be reachable");
        }
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
}
