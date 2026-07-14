//! The API error type: uniform JSON error envelopes with proper status codes.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use sauron_auth::AuthError;

#[derive(Debug)]
pub enum ApiError {
    Auth(AuthError),
    BadRequest(String),
    NotFound,
    Conflict(String),
    RateLimited,
    Internal(String),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            ApiError::Auth(e) => e.into_response(),
            ApiError::BadRequest(m) => body(StatusCode::BAD_REQUEST, "bad_request", &m),
            ApiError::NotFound => body(StatusCode::NOT_FOUND, "not_found", "resource not found"),
            ApiError::Conflict(m) => body(StatusCode::CONFLICT, "conflict", &m),
            ApiError::RateLimited => body(
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limited",
                "too many attempts; please try again shortly",
            ),
            ApiError::Internal(m) => {
                tracing::error!(error = %m, "internal error");
                body(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal",
                    "internal error",
                )
            }
        }
    }
}

fn body(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(json!({ "error": { "code": code, "message": message } })),
    )
        .into_response()
}

impl From<AuthError> for ApiError {
    fn from(e: AuthError) -> Self {
        ApiError::Auth(e)
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        ApiError::Internal(e.to_string())
    }
}

impl From<diesel::result::Error> for ApiError {
    fn from(e: diesel::result::Error) -> Self {
        match e {
            diesel::result::Error::NotFound => ApiError::NotFound,
            other => ApiError::Internal(other.to_string()),
        }
    }
}

impl From<sauron_db::filter::FilterError> for ApiError {
    fn from(e: sauron_db::filter::FilterError) -> Self {
        ApiError::BadRequest(e.to_string())
    }
}
