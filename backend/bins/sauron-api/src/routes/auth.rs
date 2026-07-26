//! Authentication: register, login, refresh (rotating), logout, self-service
//! password change, and `/me`.

use axum::extract::{ConnectInfo, State};
use axum::Json;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use sauron_auth::{
    hash_password_async, hash_token, spend_dummy_verify, verify_password_async, AuthError, AuthUser,
};
use sauron_db::models::User;
use sauron_db::repo;

use super::{db, issue_tokens, slugify, TokenPair};
use crate::error::ApiError;
use crate::AppState;

/// Upper bound on an accepted password. Argon2 cost grows with input length, so
/// an unbounded password is a cheap way to buy expensive server-side work.
const MAX_PASSWORD_LEN: usize = 256;
/// Per-account login attempts allowed per window.
const LOGIN_ATTEMPTS_PER_MIN: u32 = 10;
/// Per-IP registrations allowed per window. Registration is unauthenticated and
/// each call runs a memory-hard Argon2 hash plus three inserts.
const REGISTER_ATTEMPTS_PER_HOUR: u32 = 10;
/// Per-IP refresh attempts allowed per window.
const REFRESH_ATTEMPTS_PER_MIN: u32 = 60;
/// How long after a rotation a second presentation of the same token is treated
/// as a concurrent refresh by the same client rather than a replay. Long enough
/// to cover two tabs racing on one timer, short enough that a stolen token is
/// still caught essentially immediately.
const REFRESH_RACE_GRACE: chrono::Duration = chrono::Duration::seconds(10);

/// The address to attribute a request to for per-IP limiting.
///
/// Falls back to the socket peer, so a limiter key always exists. `X-Forwarded-For`
/// is honoured only when `API_TRUST_FORWARDED_HEADERS` is set: the header is
/// client-controlled, so trusting it on a directly-exposed service would let a
/// caller pick a fresh bucket per request and bypass the limiter entirely.
///
/// Without this the limiters keyed on the proxy's address behind the shipped
/// nginx config, making "10 registrations/hour per IP" mean 10 per hour for the
/// entire deployment.
fn client_addr(headers: &axum::http::HeaderMap, peer: &SocketAddr, state: &AppState) -> String {
    if state.cfg.api_trust_forwarded_headers {
        let forwarded = headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            // Left-most entry is the original client; the rest are proxy hops.
            .and_then(|s| s.split(',').next())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                headers
                    .get("x-real-ip")
                    .and_then(|v| v.to_str().ok())
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
            });
        if let Some(ip) = forwarded {
            return ip.to_string();
        }
    }
    peer.ip().to_string()
}

/// Largest number of distinct keys the fallback limiter tracks at once. Keys
/// embed caller-controlled values (email, client IP), so the map has to be
/// bounded or a Redis outage turns into a memory-exhaustion vector.
const LOCAL_LIMITER_MAX_KEYS: usize = 50_000;

/// Per-process fixed-window counters, consulted only while Redis is unreachable.
type LocalWindows = Mutex<HashMap<String, (Instant, u32)>>;
static LOCAL_LIMITER: OnceLock<LocalWindows> = OnceLock::new();

/// Fallback limiter used when Redis errors.
///
/// Enforces the same limit/window, but per API process. With N replicas an
/// attacker gets N× the budget rather than 1× — a real but bounded loss, and
/// far preferable to the alternatives: unlimited attempts (fail open) or a
/// total authentication outage (fail closed).
fn local_rate_limit_ok(key: &str, limit: u32, window: Duration) -> bool {
    let windows = LOCAL_LIMITER.get_or_init(Default::default);
    // A poisoned lock only means some caller panicked mid-update; the counter is
    // still sound, and refusing every login over it would recreate the outage
    // this function exists to prevent.
    let mut map = windows.lock().unwrap_or_else(|p| p.into_inner());
    let now = Instant::now();

    if map.len() >= LOCAL_LIMITER_MAX_KEYS {
        map.retain(|_, (start, _)| now.duration_since(*start) < window);
        // Still full after pruning means every entry is live, i.e. the map is
        // under active spray. Reset wholesale rather than grow without bound.
        if map.len() >= LOCAL_LIMITER_MAX_KEYS {
            map.clear();
        }
    }

    let entry = map.entry(key.to_string()).or_insert((now, 0));
    if now.duration_since(entry.0) >= window {
        *entry = (now, 0);
    }
    entry.1 = entry.1.saturating_add(1);
    entry.1 <= limit
}

/// Consume one token from a Redis fixed-window limiter.
///
/// A limiter that simply skips its check when Redis is unreachable silently
/// removes brute-force protection exactly when the system is already degraded.
/// But failing closed outright made Redis a single point of failure for
/// authentication: login, refresh and register all returned 429, so a brief
/// Redis blip locked every user out of the dashboard within one access-token
/// lifetime and blocked new sign-ins entirely — a self-inflicted outage on a
/// path that otherwise does not need Redis at all.
///
/// So: enforce through Redis when it is reachable, and degrade to a per-process
/// window when it is not. Attempts stay bounded either way.
async fn rate_limit(state: &AppState, key: &str, limit: u32, window: u64) -> Result<(), ApiError> {
    // The shared connection is built with `set_response_timeout(None)` (the
    // blocking stream read needs that), so a command issued against a dead
    // Redis sits through the manager's reconnect attempts instead of erroring.
    // Measured at 9-19s per login during an outage — long enough that the
    // in-flight cap fills and the whole API stalls, which is the outage this
    // fallback exists to prevent. This check is a fast path by nature: if Redis
    // cannot answer promptly, treat it as unavailable.
    const LIMITER_TIMEOUT: Duration = Duration::from_millis(250);

    let degrade = |reason: &str| {
        if local_rate_limit_ok(key, limit, Duration::from_secs(window)) {
            tracing::warn!(
                key,
                reason,
                "rate limiter degraded to per-process fallback (Redis unavailable)"
            );
            Ok(())
        } else {
            Err(ApiError::RateLimited)
        }
    };

    match tokio::time::timeout(
        LIMITER_TIMEOUT,
        state.redis.rate_limit_ok(key, limit, window),
    )
    .await
    {
        Ok(Ok(true)) => Ok(()),
        Ok(Ok(false)) => Err(ApiError::RateLimited),
        Ok(Err(e)) => degrade(&e.to_string()),
        Err(_elapsed) => degrade("timed out"),
    }
}

#[derive(Deserialize)]
pub struct RegisterReq {
    pub email: String,
    pub password: String,
    #[serde(default)]
    pub name: String,
    pub org_name: String,
}

#[derive(Serialize)]
pub struct AuthResponse {
    #[serde(flatten)]
    pub tokens: TokenPair,
    pub user: User,
}

pub async fn register(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: axum::http::HeaderMap,
    Json(req): Json<RegisterReq>,
) -> Result<Json<AuthResponse>, ApiError> {
    // Throttle before any expensive work: this endpoint is unauthenticated and
    // each call runs a ~19 MiB Argon2 hash and creates user/org/grant rows.
    rate_limit(
        &state,
        &format!(
            "sauron:auth:register:{}",
            client_addr(&headers, &peer, &state)
        ),
        REGISTER_ATTEMPTS_PER_HOUR,
        3600,
    )
    .await?;

    if !req.email.contains('@') {
        return Err(ApiError::BadRequest("a valid email is required".into()));
    }
    if req.password.len() < 8 {
        return Err(ApiError::BadRequest(
            "password must be at least 8 characters".into(),
        ));
    }
    if req.password.len() > MAX_PASSWORD_LEN {
        return Err(ApiError::BadRequest(format!(
            "password must be at most {MAX_PASSWORD_LEN} characters"
        )));
    }
    if req.org_name.trim().is_empty() {
        return Err(ApiError::BadRequest("organization name is required".into()));
    }

    let mut conn = db(&state).await?;
    if repo::find_user_by_email(&mut conn, &req.email)
        .await?
        .is_some()
    {
        return Err(ApiError::Conflict("email is already registered".into()));
    }

    let hash = hash_password_async(req.password.clone())
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let user = repo::create_user(&mut conn, &req.email, &hash, &req.name).await?;
    let org = repo::create_org(&mut conn, &req.org_name, &slugify(&req.org_name)).await?;

    // Grant the creator the Owner role at org scope.
    let owner = repo::get_system_role(&mut conn, "Owner")
        .await?
        .ok_or_else(|| ApiError::Internal("Owner preset role missing".into()))?;
    repo::create_grant(
        &mut conn,
        sauron_db::models::NewRoleGrant {
            org_id: org.id,
            user_id: user.id,
            role_id: owner.id,
            scope_type: "org".into(),
            scope_id: org.id,
        },
    )
    .await?;

    // The user chose their own password, so nothing is owed.
    let tokens = issue_tokens(&state, &mut conn, user.id, None, false).await?;
    Ok(Json(AuthResponse { tokens, user }))
}

#[derive(Deserialize)]
pub struct LoginReq {
    pub email: String,
    pub password: String,
}

pub async fn login(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: axum::http::HeaderMap,
    Json(req): Json<LoginReq>,
) -> Result<Json<AuthResponse>, ApiError> {
    if req.password.len() > MAX_PASSWORD_LEN {
        return Err(ApiError::Auth(AuthError::InvalidCredentials));
    }
    // Throttle per account (credential stuffing against one user) and per IP
    // (spraying one password across many accounts). Both fail closed.
    rate_limit(
        &state,
        &format!("sauron:auth:login:{}", req.email.to_lowercase()),
        LOGIN_ATTEMPTS_PER_MIN,
        60,
    )
    .await?;
    rate_limit(
        &state,
        &format!(
            "sauron:auth:login:ip:{}",
            client_addr(&headers, &peer, &state)
        ),
        LOGIN_ATTEMPTS_PER_MIN * 6,
        60,
    )
    .await?;

    let mut conn = db(&state).await?;
    let found = repo::find_user_by_email(&mut conn, &req.email).await?;

    // Always spend one Argon2 verification, whether or not the email exists.
    // Short-circuiting on a missing user would make "no such account" return in
    // microseconds and "wrong password" in tens of milliseconds, which is a
    // reliable user-enumeration oracle despite the identical error body.
    let user = match found {
        Some(u) => {
            if verify_password_async(req.password.clone(), u.password_hash.clone()).await {
                u
            } else {
                return Err(ApiError::Auth(AuthError::InvalidCredentials));
            }
        }
        None => {
            spend_dummy_verify(req.password.clone()).await;
            return Err(ApiError::Auth(AuthError::InvalidCredentials));
        }
    };

    // Checked here, not earlier: an is_active branch before the password
    // verification would answer in microseconds for a deactivated account and
    // tens of milliseconds for an active one, reintroducing exactly the
    // user-enumeration oracle the dummy-verify above exists to close. Someone
    // who does not know the password learns nothing.
    if !user.is_active {
        return Err(ApiError::Auth(AuthError::AccountDeactivated));
    }

    let _ = repo::touch_last_login(&mut conn, user.id).await;
    let tokens = issue_tokens(&state, &mut conn, user.id, None, user.must_change_password).await?;
    Ok(Json(AuthResponse { tokens, user }))
}

#[derive(Deserialize)]
pub struct RefreshReq {
    pub refresh_token: String,
}

pub async fn refresh(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: axum::http::HeaderMap,
    Json(req): Json<RefreshReq>,
) -> Result<Json<TokenPair>, ApiError> {
    rate_limit(
        &state,
        &format!(
            "sauron:auth:refresh:{}",
            client_addr(&headers, &peer, &state)
        ),
        REFRESH_ATTEMPTS_PER_MIN,
        60,
    )
    .await?;

    let hash = hash_token(&req.refresh_token);
    let mut conn = db(&state).await?;
    let Some(token) = repo::find_active_refresh_token(&mut conn, &hash).await? else {
        // The token is not active. If we have nonetheless *seen* this hash, the
        // presented token was already rotated away — which usually means two
        // parties hold the same secret and one is replaying a stolen token, so
        // the whole family is revoked.
        //
        // "Usually", because clients race with themselves: two dashboard tabs
        // share one token in localStorage and refresh on the same 15-minute
        // timer, so both present it at once. The loser is indistinguishable
        // from a replay, and killing the family logged the user out of every
        // tab — including the winner that had just been issued a good token.
        //
        // A rotation that happened moments ago is therefore treated as that
        // race and re-issued instead. The window is deliberately narrow, and
        // gated on the revocation *reason* so a token killed by logout or by a
        // previous replay never qualifies, plus a check that the user still has
        // a live token so a family kill cannot be undone.
        if let Some((user_id, revoked_at, reason)) =
            repo::refresh_token_revocation(&mut conn, &hash).await?
        {
            let raced = reason.as_deref() == Some(repo::REVOKE_ROTATED)
                && revoked_at.is_some_and(|at| Utc::now() - at < REFRESH_RACE_GRACE)
                && repo::user_has_active_refresh_token(&mut conn, user_id).await?;

            if raced {
                tracing::info!(
                    %user_id,
                    "concurrent refresh of a just-rotated token; re-issuing instead of \
                     revoking the family"
                );
                // Load the user for the forced-change flag, and refuse a
                // deactivated account here too: otherwise a member an admin just
                // disabled keeps minting fresh access tokens from the refresh
                // token still sitting in localStorage, and the deactivation
                // never takes effect.
                let user = repo::get_user(&mut conn, user_id)
                    .await?
                    .ok_or(ApiError::Auth(AuthError::InvalidToken))?;
                if !user.is_active {
                    return Err(ApiError::Auth(AuthError::AccountDeactivated));
                }
                let tokens =
                    issue_tokens(&state, &mut conn, user_id, None, user.must_change_password)
                        .await?;
                return Ok(Json(tokens));
            }

            // A deactivated account's tokens are mass-revoked with
            // REVOKE_DEACTIVATED (see set_active in orgs.rs), so a routine
            // deactivation lands here too: the reason is not REVOKE_ROTATED,
            // so `raced` is false above, and without this check a disabled
            // user presenting their old refresh token would trip the "reuse
            // detected" WARN below. That signal exists to flag actual token
            // theft; poisoning it with routine deactivations makes it
            // unreliable exactly when it matters. Report the mundane cause
            // and skip the alarm.
            if reason.as_deref() == Some(repo::REVOKE_DEACTIVATED) {
                // The reason reflects the revocation at the time it happened,
                // not whether the account is still deactivated: an admin may
                // have reactivated it since without touching this already-
                // revoked row. Re-check the user's current state so a
                // reactivated user falls through to the ordinary
                // reuse-detection path below instead of being told their
                // (now-active) account is deactivated.
                let user = repo::get_user(&mut conn, user_id)
                    .await?
                    .ok_or(ApiError::Auth(AuthError::InvalidToken))?;
                if !user.is_active {
                    return Err(ApiError::Auth(AuthError::AccountDeactivated));
                }
            }

            let revoked = repo::revoke_all_refresh_tokens_for_user(&mut conn, user_id).await?;
            tracing::warn!(
                %user_id,
                peer = %peer.ip(),
                revoked,
                "refresh token reuse detected; revoked all sessions for the user"
            );
        }
        return Err(ApiError::Auth(AuthError::InvalidToken));
    };

    // Rotate: revoke the presented token, issue a fresh pair. Same reasoning as
    // the race path above — a deactivated account must not be able to refresh
    // its way to a live session.
    let user = repo::get_user(&mut conn, token.user_id)
        .await?
        .ok_or(ApiError::Auth(AuthError::InvalidToken))?;
    if !user.is_active {
        return Err(ApiError::Auth(AuthError::AccountDeactivated));
    }
    repo::revoke_refresh_token(&mut conn, &hash, repo::REVOKE_ROTATED).await?;
    let tokens = issue_tokens(
        &state,
        &mut conn,
        token.user_id,
        None,
        user.must_change_password,
    )
    .await?;
    Ok(Json(tokens))
}

#[derive(Deserialize)]
pub struct LogoutReq {
    pub refresh_token: String,
}

pub async fn logout(
    State(state): State<AppState>,
    Json(req): Json<LogoutReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let hash = hash_token(&req.refresh_token);
    let mut conn = db(&state).await?;
    repo::revoke_refresh_token(&mut conn, &hash, repo::REVOKE_LOGOUT).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct ChangePasswordReq {
    pub current_password: String,
    pub new_password: String,
}

/// Self-service password change. The only endpoint a temp-password holder can
/// reach.
pub async fn change_password(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(req): Json<ChangePasswordReq>,
) -> Result<Json<AuthResponse>, ApiError> {
    // Throttle before any hashing: this handler runs a verify plus a fresh
    // hash (two ~19 MiB Argon2 ops) per call, and it is the one endpoint
    // deliberately left reachable by a temp-password holder — the least-
    // trusted principal in this feature. Keyed per user id rather than per
    // IP since the caller is already authenticated; same budget as login,
    // since both gate an Argon2 verify against a caller-supplied guess.
    rate_limit(
        &state,
        &format!("sauron:auth:password:{}", auth.user_id),
        LOGIN_ATTEMPTS_PER_MIN,
        60,
    )
    .await?;

    if req.current_password.len() > MAX_PASSWORD_LEN {
        return Err(ApiError::Auth(AuthError::InvalidCredentials));
    }
    if req.new_password.len() < 8 {
        return Err(ApiError::BadRequest(
            "password must be at least 8 characters".into(),
        ));
    }
    if req.new_password.len() > MAX_PASSWORD_LEN {
        return Err(ApiError::BadRequest(format!(
            "password must be at most {MAX_PASSWORD_LEN} characters"
        )));
    }
    if req.new_password == req.current_password {
        return Err(ApiError::BadRequest(
            "the new password must be different from the current one".into(),
        ));
    }

    let mut conn = db(&state).await?;
    let user = repo::get_user(&mut conn, auth.user_id)
        .await?
        .ok_or(ApiError::Auth(AuthError::InvalidToken))?;
    if !user.is_active {
        return Err(ApiError::Auth(AuthError::AccountDeactivated));
    }
    if !verify_password_async(req.current_password.clone(), user.password_hash.clone()).await {
        return Err(ApiError::Auth(AuthError::InvalidCredentials));
    }

    let hash = hash_password_async(req.new_password.clone())
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    // Revoke everything, including the caller's own session, before setting the
    // new password. Keeping the current session would not work anyway: its
    // access token still carries must_change_password, so the extractor gate
    // would keep rejecting the user until it expired — immediately after they
    // did the one thing it was demanding. Re-issuing also logs out every other
    // device, which is correct when the old credential may be known to whoever
    // generated it.
    //
    // The order (revoke, then set password, then issue) is deliberate and not
    // a transaction: `conn.transaction` needs async closures, which need Rust
    // 1.85+, and this workspace's MSRV is 1.82 per packaging/rpm/sauron.spec.
    // If the revoke fails, the caller is still on the temp password —
    // recoverable, since they log in again with it and remain flagged. The
    // reverse order would leave the password changed but every old session
    // (including one held by whoever handed out the temp credential) still
    // valid, which is the direction that must never happen on a partial
    // failure.
    repo::revoke_all_refresh_tokens_for_user_with_reason(
        &mut conn,
        auth.user_id,
        repo::REVOKE_PASSWORD_CHANGED,
    )
    .await?;
    repo::set_user_password(&mut conn, auth.user_id, &hash).await?;
    let tokens = issue_tokens(&state, &mut conn, auth.user_id, None, false).await?;

    // Re-read so the returned user has must_change_password already false; the
    // dashboard's stored user object is then correct without a second round trip.
    let fresh = repo::get_user(&mut conn, auth.user_id)
        .await?
        .ok_or_else(|| ApiError::Internal("user vanished mid-request".into()))?;
    Ok(Json(AuthResponse {
        tokens,
        user: fresh,
    }))
}

pub async fn me(auth: AuthUser, State(state): State<AppState>) -> Result<Json<User>, ApiError> {
    let mut conn = db(&state).await?;
    let user = repo::find_user_by_id(&mut conn, auth.user_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(user))
}
