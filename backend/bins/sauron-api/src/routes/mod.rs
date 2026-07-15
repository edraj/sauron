//! HTTP route handlers, grouped by domain, plus shared helpers.

pub mod admin;
pub mod analytics;
pub mod apps;
pub mod artifacts;
pub mod auth;
pub mod devices;
pub mod funnels;
pub mod issues;
pub mod journeys;
pub mod monitors;
pub mod orgs;
pub mod performance;
pub mod projects;
pub mod screens;
pub mod sessions;

use chrono::{Duration, Utc};
use serde::Serialize;
use uuid::Uuid;

use sauron_db::{AsyncPgConnection, PgConn};

use crate::error::ApiError;
use crate::AppState;

/// Access + refresh token pair returned by auth endpoints.
#[derive(Debug, Serialize)]
pub struct TokenPair {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: i64,
}

/// Check out a pooled connection, mapping errors to `ApiError`.
pub(crate) async fn db(state: &AppState) -> Result<PgConn, ApiError> {
    sauron_db::conn(&state.pool)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))
}

/// Build a URL-safe slug from a display name, with a short random suffix so the
/// (unique) slug never collides.
pub(crate) fn slugify(name: &str) -> String {
    let cleaned: String = name
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    let mut s = cleaned;
    while s.contains("--") {
        s = s.replace("--", "-");
    }
    let base = s.trim_matches('-');
    let base = if base.is_empty() { "item" } else { base };
    format!("{base}-{}", sauron_core::ids::random_hex(3))
}

/// Issue an access token and a persisted (rotating) refresh token for a user.
pub(crate) async fn issue_tokens(
    state: &AppState,
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    user_agent: Option<String>,
) -> Result<TokenPair, ApiError> {
    let (access, exp) = state
        .keys
        .issue_access(user_id)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let raw = sauron_core::ids::opaque_token();
    let hash = sauron_auth::hash_token(&raw);
    let expires_at = Utc::now() + Duration::seconds(state.cfg.jwt_refresh_ttl_secs);
    sauron_db::repo::insert_refresh_token(conn, user_id, hash, expires_at, user_agent).await?;
    Ok(TokenPair {
        access_token: access,
        refresh_token: raw,
        expires_at: exp,
    })
}
