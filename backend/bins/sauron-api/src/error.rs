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
    /// A 403 that can say *why*. `AuthError::Forbidden` carries no message, so
    /// a batch operation refused at one of many scopes could only answer
    /// "forbidden" — leaving the admin to guess which of the scopes they ticked
    /// was the problem.
    Forbidden(String),
    NotFound,
    Conflict(String),
    /// Syntactically valid, semantically impossible. Used where a 400 would be
    /// misleading: the request parsed, the ids exist, and the operation is
    /// still refused — e.g. a project scope that resolves to zero apps, where
    /// the `for every app, covers()` test would otherwise succeed vacuously.
    Unprocessable(String),
    /// The locator resolved to nothing: the partition was dropped by
    /// `sauron-tier`, the rollup row was replaced, or the tenant did not match.
    /// Distinct from 404 so the UI can say "this data has aged out" rather than
    /// "no such finding".
    Gone(String),
    RateLimited,
    /// A dependency the route needs is not configured on this deployment, and
    /// the route refuses **before applying anything**. The message carries the
    /// `require_*()` text so an operator learns which setting is missing.
    ///
    /// The first field is the machine-readable code, so a caller can tell
    /// "the schema is behind the binary, run sauron-migrate" from "the server
    /// is shedding load, retry". A bare 500 from a missing column tells the
    /// operator nothing and looks like a product bug.
    Unavailable(&'static str, String),
    Internal(String),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            ApiError::Auth(e) => e.into_response(),
            ApiError::BadRequest(m) => body(StatusCode::BAD_REQUEST, "bad_request", &m),
            ApiError::Forbidden(m) => body(StatusCode::FORBIDDEN, "forbidden", &m),
            ApiError::NotFound => body(StatusCode::NOT_FOUND, "not_found", "resource not found"),
            ApiError::Conflict(m) => body(StatusCode::CONFLICT, "conflict", &m),
            ApiError::Unprocessable(m) => {
                body(StatusCode::UNPROCESSABLE_ENTITY, "unprocessable", &m)
            }
            ApiError::Gone(m) => body(StatusCode::GONE, "gone", &m),
            ApiError::RateLimited => body(
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limited",
                "too many attempts; please try again shortly",
            ),
            ApiError::Unavailable(code, m) => body(StatusCode::SERVICE_UNAVAILABLE, code, &m),
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
