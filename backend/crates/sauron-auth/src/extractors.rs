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
    Forbidden,
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
            AuthError::Forbidden => (StatusCode::FORBIDDEN, "forbidden", "you do not have access"),
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
        Ok(AuthUser { user_id, claims })
    }
}
