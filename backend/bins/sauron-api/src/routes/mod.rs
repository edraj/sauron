//! HTTP route handlers, grouped by domain, plus shared helpers.

pub mod account;
pub mod active_users;
pub mod admin;
pub mod analytics;
pub mod apps;
pub mod artifacts;
pub mod auth;
pub mod devices;
pub mod environments;
pub mod funnels;
pub mod inspector;
pub mod issues;
pub mod journeys;
pub mod monitors;
pub mod notification_prefs;
pub mod notifications;
pub mod orgs;
pub mod performance;
pub mod projects;
pub mod scope;
pub mod screens;
pub mod search;
pub mod sessions;
pub mod stores;
pub mod workflows;

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

/// Upper bound on `OFFSET` for any list endpoint.
///
/// Postgres must walk and discard every skipped row, so an unbounded offset
/// turns a cheap request into a full ordered scan. Anything past this depth is
/// not a real browsing pattern — it is either a bug or an attempt to amplify
/// query cost.
pub(crate) const MAX_LIST_OFFSET: i64 = 50_000;

/// Clamp a caller-supplied offset into the allowed range.
pub(crate) fn clamp_offset(offset: i64) -> i64 {
    offset.clamp(0, MAX_LIST_OFFSET)
}

// `authorize_app_perms` lived here: it authorized a permission on an app and
// returned the caller's whole effective set, so a handler gating on a *second*
// permission (`source:read`, deciding whether to include source context) paid
// one ancestry+grant resolution instead of two. Deleted in Slice 3 Task 6 (fix
// round 1), and not merely because it became unused: it resolved permissions
// through `sauron_auth::effective_at`, which hardcodes `env: None`, and
// `rbac::grant_applies`'s `Scope::Env` arm is `Some(e) == env` — so an
// environment-scoped grant could never satisfy it. Its two callers
// (`issues::detail` / `issues::events`) therefore returned `403` to an
// env-scoped caller even for their own environment: they could list issues but
// not open one. `scope::authorized_read_scope_with_perms` replaces it, keeping
// the single-resolution property (see `rbac::authorize_env_read_inner`) while
// evaluating the second permission at the environment the read resolved to.
// Recorded here rather than silently removed so a future handler with the same
// two-permission shape reaches for the env-aware helper instead of
// reintroducing this one.

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

/// Everything a token mint needs to know about *where* it is happening.
///
/// A struct rather than three more positional parameters: seven arguments sits
/// on `clippy::too_many_arguments`' threshold, and
/// `Option<Uuid>, Option<String>, Option<String>` in a row is a call-site
/// transposition waiting to happen.
pub(crate) struct SessionContext {
    /// `None` starts a new session; `Some` continues one across a rotation.
    pub session_id: Option<Uuid>,
    pub user_agent: Option<String>,
    pub ip: Option<String>,
}

/// Longest user agent stored. Long enough for every real browser string, short
/// enough that a caller cannot use the header as free storage.
pub(crate) const MAX_USER_AGENT_LEN: usize = 400;

/// The request's `User-Agent`, trimmed, non-empty, and bounded.
///
/// Truncation is by `chars`, not bytes: slicing a UTF-8 string mid-codepoint
/// panics, and the header is caller-controlled.
pub(crate) fn sanitize_ua(headers: &axum::http::HeaderMap) -> Option<String> {
    let raw = headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())?
        .trim();
    if raw.is_empty() {
        return None;
    }
    Some(raw.chars().take(MAX_USER_AGENT_LEN).collect())
}

/// A caller address, canonicalised, or `None` if it is not an address at all.
///
/// With `API_TRUST_FORWARDED_HEADERS=1` this value comes from a
/// client-controlled `X-Forwarded-For`, so parsing it as an `IpAddr` and storing
/// the canonical form (else NULL) removes an arbitrary-string-into-the-database
/// vector. It is also what makes the stored value safe to render unmasked on the
/// owner's own account page.
pub(crate) fn sanitize_ip(raw: &str) -> Option<String> {
    raw.parse::<std::net::IpAddr>().ok().map(|a| a.to_string())
}

/// Issue an access token and a persisted (rotating) refresh token for a user,
/// starting or continuing the caller's session.
///
/// The order is deliberate and **changed** from the original: the session row is
/// written first and the JWT is minted last, so a token is never handed out for
/// a session that failed to persist. The session id is generated in Rust rather
/// than by the database default because the JWT needs it, and a `RETURNING`
/// round trip would not be atomic with the token insert.
pub(crate) async fn issue_tokens(
    state: &AppState,
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    sess: SessionContext,
    must_change_password: bool,
) -> Result<TokenPair, ApiError> {
    let continuing = sess.session_id.is_some();
    let session_id = sess.session_id.unwrap_or_else(Uuid::new_v4);
    let raw = sauron_core::ids::opaque_token();
    let hash = sauron_auth::hash_token(&raw);
    let expires_at = Utc::now() + Duration::seconds(state.cfg.jwt_refresh_ttl_secs);

    let rows = sauron_db::repo::start_or_continue_session(
        conn,
        session_id,
        user_id,
        hash,
        expires_at,
        sess.user_agent,
        sess.ip,
    )
    .await?;
    if rows == 0 {
        // Continuing: the session was revoked (or re-owned) between the caller
        // presenting its refresh token and this write. That is a 401, and it is
        // the guard that stops the 10-second refresh-race window from
        // resurrecting a session the user just killed.
        // Starting: a fresh INSERT with a brand-new uuid cannot conflict, so
        // zero rows means something is genuinely wrong.
        return Err(if continuing {
            ApiError::Auth(sauron_auth::AuthError::InvalidToken)
        } else {
            ApiError::Internal("new session insert affected no rows".into())
        });
    }

    let (access, exp) = state
        .keys
        .issue_access(user_id, must_change_password, Some(session_id))
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(TokenPair {
        access_token: access,
        refresh_token: raw,
        expires_at: exp,
    })
}

#[cfg(test)]
mod sanitize_tests {
    use super::*;
    use axum::http::header::USER_AGENT;
    use axum::http::HeaderMap;

    #[test]
    fn sanitize_ua_trims_rejects_empty_and_truncates() {
        let mut headers = HeaderMap::new();
        assert_eq!(sanitize_ua(&headers), None);

        headers.insert(USER_AGENT, "   ".parse().unwrap());
        assert_eq!(sanitize_ua(&headers), None);

        headers.insert(USER_AGENT, "  Mozilla/5.0  ".parse().unwrap());
        assert_eq!(sanitize_ua(&headers).as_deref(), Some("Mozilla/5.0"));

        let long = "a".repeat(MAX_USER_AGENT_LEN + 50);
        headers.insert(USER_AGENT, long.parse().unwrap());
        assert_eq!(
            sanitize_ua(&headers).map(|s| s.len()),
            Some(MAX_USER_AGENT_LEN)
        );
    }

    #[test]
    fn sanitize_ip_stores_only_a_canonical_address() {
        // Not cosmetic: with API_TRUST_FORWARDED_HEADERS=1 this value comes from
        // a client-controlled X-Forwarded-For, so parsing it removes an
        // arbitrary-string-into-the-database vector.
        assert_eq!(sanitize_ip("203.0.113.7").as_deref(), Some("203.0.113.7"));
        assert_eq!(
            sanitize_ip("2001:0db8:0000:0000:0000:0000:0000:0001").as_deref(),
            Some("2001:db8::1")
        );
        assert_eq!(sanitize_ip("not-an-ip"), None);
        assert_eq!(sanitize_ip(""), None);
        assert_eq!(sanitize_ip("203.0.113.7, 198.51.100.1"), None);
    }
}

#[cfg(test)]
mod revocation_call_site_tests {
    /// `auth_sessions` and `refresh_tokens` must never disagree, so every site
    /// in this crate that ends someone's tokens goes through a session-aware
    /// repo function. A sixth site added later would desync the two tables
    /// silently: the session list would show rows with no token behind them, and
    /// the revocation snapshot would never learn about them.
    ///
    /// The needle is assembled at runtime so this test does not match its own
    /// source file.
    #[test]
    fn no_session_blind_mass_revoke_remains_in_this_crate() {
        let needle = format!("revoke_all_{}", "refresh_tokens_for_user");
        let mut offenders = Vec::new();
        let mut stack = vec![std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src")];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("read src dir") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                    continue;
                }
                let src = std::fs::read_to_string(&path).expect("read source file");
                for (i, line) in src.lines().enumerate() {
                    if line.contains(&needle) && !line.trim_start().starts_with("//") {
                        offenders.push(format!("{}:{}", path.display(), i + 1));
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "these call sites revoke tokens without revoking their sessions: {offenders:?}"
        );
    }
}
