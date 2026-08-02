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
use uuid::Uuid;

use sauron_auth::{
    hash_password_async, hash_token, spend_dummy_verify, verify_password_async, AuthError, AuthUser,
};
use sauron_core::ids::opaque_token;
use sauron_db::models::User;
use sauron_db::repo;
use sauron_mail::MailKind;

use super::{db, issue_tokens, sanitize_ip, sanitize_ua, slugify, SessionContext, TokenPair};
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

/// How long a self-service reset link lives.
///
/// A mailbox is a password-equivalent credential for exactly as long as the
/// link does, and one hour is the modern norm. Compile-time rather than
/// `Config`: two env knobs cost three files of documentation each, for values
/// nobody tunes.
pub(crate) const SELF_RESET_TTL_SECS: i64 = 3_600;
/// How long an admin-initiated reset link lives.
///
/// The account cannot be signed into at all until this link is used, so a
/// one-hour window would turn "read the mail after lunch" into a second round
/// trip through an administrator. A day is what makes the lockout survivable.
pub(crate) const ADMIN_RESET_TTL_SECS: i64 = 86_400;

/// Self-service reset **sends** per email address per hour.
///
/// A cap on mail, NOT a lockout on the request. Exhausting it used to answer
/// 429, which made an anonymous caller's three requests a one-hour denial of
/// self-service reset against any address they could name — and on a small
/// deployment at 2am the named victim is usually the only admin. Nobody could
/// shorten it: the admin remedy refuses on a member holding grants outside the
/// org, and for everyone else the "remedy" was destroying their credential.
///
/// Past the cap the request still answers 200 and still mints and mails a link
/// **unless the address already holds a live self-service link** — i.e. the cap
/// suppresses only redundant mail. The anti-flood property survives, because
/// once the budget is gone an attacker can cause at most one further send per
/// `SELF_RESET_TTL_SECS`, and the person being targeted is never locked out.
///
/// Residual: someone who deletes the attacker-triggered mails unread, and only
/// then asks for a reset, is suppressed until the last of those links expires.
/// They wait out the token TTL (1h), not a limiter window, and the link they
/// deleted was valid the whole time.
pub(crate) const FORGOT_ATTEMPTS_PER_EMAIL_PER_HOUR: u32 = 3;
/// Self-service reset requests per IP per **60 seconds** — a burst limiter, not
/// a lockout.
///
/// `API_TRUST_FORWARDED_HEADERS` defaults to false and the shipped nginx sits in
/// front, so `client_addr` returns the proxy's address and this bucket is the
/// **entire deployment**. An hour-long budget would let an anonymous attacker
/// burn it in about a second, after which every legitimate link-holder gets 429
/// for the remaining 59 minutes. Copy login's window, not just its arithmetic.
pub(crate) const FORGOT_ATTEMPTS_PER_MIN_PER_IP: u32 = 60;
/// Reset submissions per IP per 60 seconds. Same deployment-wide-bucket
/// reasoning as `FORGOT_ATTEMPTS_PER_MIN_PER_IP`, and worse here: this route
/// rejects a malformed token before any DB work, so the budget is cheap to burn.
pub(crate) const RESET_ATTEMPTS_PER_MIN_PER_IP: u32 = 60;
/// Reset submissions per **link** per hour — a key an attacker cannot share.
///
/// The reuse check returns 400 *without* consuming the token when the new
/// password equals the current one, so a link-holder could otherwise loop that
/// branch at 60/min, each iteration costing an Argon2 verify. Ten per hour is
/// generous for a human and useless as an amplifier.
pub(crate) const RESET_ATTEMPTS_PER_TOKEN_PER_HOUR: u32 = 10;
/// Admin-initiated resets per calling admin per hour. Bounds the fan-out.
pub(crate) const ADMIN_RESET_PER_CALLER_PER_HOUR: u32 = 20;
/// Admin-initiated resets per target per hour. The one that matters: an
/// unbounded loop is an unbounded mail bomb aimed at one member's inbox, and an
/// unbounded re-lock of an account somebody is trying to recover.
pub(crate) const ADMIN_RESET_PER_TARGET_PER_HOUR: u32 = 5;

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
pub(crate) fn client_addr(
    headers: &axum::http::HeaderMap,
    peer: &SocketAddr,
    state: &AppState,
) -> String {
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
///
/// Limiter keys follow `sauron:{area}:{action}:{principal}` — e.g.
/// `sauron:auth:login:ada@example.com`, `sauron:auth:sessions:{user_id}`. The
/// principal is the thing being protected *from*: a user id for authenticated
/// routes, `client_addr(..)` for anonymous ones. Pick one and keep it stable;
/// changing a key silently resets everyone's budget.
pub(crate) async fn rate_limit(
    state: &AppState,
    key: &str,
    limit: u32,
    window: u64,
) -> Result<(), ApiError> {
    if within_budget(state, key, limit, window).await {
        Ok(())
    } else {
        Err(ApiError::RateLimited)
    }
}

/// Consume one unit of budget and report whether it was there, instead of
/// refusing the request.
///
/// For limiters whose exhaustion should change what a handler *does* rather than
/// deny it outright — see `forgot_password`, where a refusal is a denial-of-
/// service an anonymous caller can aim at a named victim.
pub(crate) async fn within_budget(state: &AppState, key: &str, limit: u32, window: u64) -> bool {
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
            true
        } else {
            false
        }
    };

    match tokio::time::timeout(
        LIMITER_TIMEOUT,
        state.redis.rate_limit_ok(key, limit, window),
    )
    .await
    {
        Ok(Ok(within)) => within,
        Ok(Err(e)) => degrade(&e.to_string()),
        Err(_elapsed) => degrade("timed out"),
    }
}

/// Which copy a reset email uses, and how long its link lives.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResetMode {
    SelfService,
    Admin,
}

impl ResetMode {
    /// The value stored in `password_reset_tokens.mode`, which is TEXT + CHECK.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ResetMode::SelfService => "self",
            ResetMode::Admin => "admin",
        }
    }

    pub(crate) fn ttl_secs(self) -> i64 {
        match self {
            ResetMode::SelfService => SELF_RESET_TTL_SECS,
            ResetMode::Admin => ADMIN_RESET_TTL_SECS,
        }
    }
}

/// The URL that goes in the email.
///
/// The token sits inside the **hash fragment**. Browsers never put a fragment
/// in a request line or a `Referer`, so it reaches no server log, proxy log or
/// analytics beacon — a real property here, not just house convention.
///
/// Deliberately takes the `&str` from `Config::require_dashboard_url` rather
/// than reusing `sauron_mail::Branding::link`: `Branding` lives inside
/// `MailSender`, which is absent on a deployment with no relay, and the reset
/// URL still has to be built there so the token row and the enqueue-or-discard
/// path cost the same whether or not SMTP is configured.
pub(crate) fn reset_link(dashboard_url: &str, raw_token: &str) -> String {
    format!(
        "{}/#/reset-password?token={}",
        dashboard_url.trim_end_matches('/'),
        raw_token
    )
}

/// "1 hour" / "24 hours", derived from the TTL constants rather than typed by
/// hand, so changing a constant cannot leave the email claiming the old number.
pub(crate) fn expiry_wording(ttl_secs: i64) -> String {
    let hours = ttl_secs / 3_600;
    if hours == 1 {
        "1 hour".to_string()
    } else {
        format!("{hours} hours")
    }
}

/// `opaque_token()` is `random_hex(32)`, so a real token is 64 hex characters.
///
/// Rejecting garbage here keeps a spray off the database and, more importantly,
/// out of Redis: the per-token limiter would otherwise mint one key per guess.
fn is_reset_token_shape(token: &str) -> bool {
    token.len() == 64 && token.chars().all(|c| c.is_ascii_hexdigit())
}

/// Shipped alongside the button in every reset email.
///
/// A client that strips the bulletproof-button table — or a recipient reading in
/// plain text — otherwise has no way back into the account at all.
const PASTE_FALLBACK: &str = "If the button does not open, copy and paste this link into your \
                              browser:";

pub(crate) struct ResetMailVars<'a> {
    pub mode: ResetMode,
    /// The recipient's display name, already falling back to their email.
    pub display_name: &'a str,
    pub reset_url: &'a str,
    /// Only read in `ResetMode::Admin`. The acting admin is deliberately not
    /// named anywhere: the org is what a recipient needs to judge legitimacy,
    /// and naming an individual invites a reply to a person rather than a route
    /// back into the account.
    pub org_name: &'a str,
}

/// Build the reset email's content.
///
/// Returns `MailContent` — structural prose, not markup — because
/// `sauron_mail::render` owns every escape site and the HTML shell. A renderer
/// that handed over its own markup would be a renderer that can be talked into
/// handing over someone else's.
pub(crate) fn render_password_reset_mail(
    vars: ResetMailVars<'_>,
) -> Result<sauron_mail::MailContent, sauron_mail::TemplateError> {
    let expiry = expiry_wording(vars.mode.ttl_secs());
    let name = vars.display_name;
    let org = vars.org_name;

    let (subject, heading, cta_label, paragraphs, footnotes) = match vars.mode {
        ResetMode::SelfService => (
            "Reset your Sauron password",
            "Reset your password",
            "Choose a new password",
            vec![
                format!("Hi {name},"),
                "Someone asked to reset the password for this address.".to_string(),
                format!("The link below expires in {expiry}."),
            ],
            vec![
                PASTE_FALLBACK.to_string(),
                vars.reset_url.to_string(),
                "If this wasn't you, nothing has changed and you can ignore this email."
                    .to_string(),
            ],
        ),
        ResetMode::Admin => (
            "Set a new Sauron password",
            "Set a new password",
            "Set a new password",
            vec![
                format!("Hi {name},"),
                format!("An administrator of {org} reset your password."),
                "Your old password no longer works and you have been signed out on all devices. \
                 This link is how you get back in."
                    .to_string(),
                format!("The link below expires in {expiry}."),
            ],
            vec![
                PASTE_FALLBACK.to_string(),
                vars.reset_url.to_string(),
                format!(
                    "If the link has expired, ask an administrator of {org} to send you another."
                ),
            ],
        ),
    };

    Ok(sauron_mail::MailContent {
        subject: subject.to_string(),
        heading: heading.to_string(),
        paragraphs,
        // The only fallible step, and the reason this function returns `Result`:
        // an origin that is not http(s) must never reach an anchor href.
        cta: Some(sauron_mail::Cta::new(cta_label, vars.reset_url)?),
        footnotes,
    })
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
    let tokens = issue_tokens(
        &state,
        &mut conn,
        user.id,
        SessionContext {
            // A new session per login.
            session_id: None,
            user_agent: sanitize_ua(&headers),
            ip: sanitize_ip(&client_addr(&headers, &peer, &state)),
        },
        false,
    )
    .await?;
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

    // Placed here, after the Argon2 verification, for exactly the reason the
    // is_active check above is: a branch ahead of the verification answers in
    // microseconds for a reset-pending account and tens of milliseconds for
    // every other one, which is the enumeration oracle `spend_dummy_verify`
    // exists to close — and it would additionally leak, to anyone who can type
    // an address, that a particular person is mid-lockout.
    //
    // This is what makes an admin-initiated reset mean what it says: the
    // suspect password stops working at the login form, rather than merely
    // producing a gated session. Completing the reset clears the column, in the
    // same statement that writes the new password.
    if user.credentials_invalidated_at.is_some() {
        return Err(ApiError::Auth(AuthError::PasswordResetRequired));
    }

    let _ = repo::touch_last_login(&mut conn, user.id).await;
    let tokens = issue_tokens(
        &state,
        &mut conn,
        user.id,
        SessionContext {
            // A new session per login.
            session_id: None,
            user_agent: sanitize_ua(&headers),
            ip: sanitize_ip(&client_addr(&headers, &peer, &state)),
        },
        user.must_change_password,
    )
    .await?;
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
        if let Some((user_id, session_id, revoked_at, reason)) =
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
                // A pre-migration token has no session to continue, and letting
                // it degrade to a fresh login defeats the whole guard: the
                // `WHERE revoked_at IS NULL` check inside
                // `start_or_continue_session` is only reachable when a session
                // id is present. The reachable case is a rolling upgrade — the
                // RPM ships api and dashboard as separate subpackages — where an
                // old replica mints a session-less token after 000035 has run,
                // rotates it at T, the user presses "sign out other devices" at
                // T+2s, and at T+5s that device's other tab lands in the grace
                // window and gets a brand-new live session. The kill would have
                // silently failed for that device, and the new session would
                // appear in the list looking like a legitimate login. Rejecting
                // cannot harm the legitimate pre-migration case: a row revoked
                // before the migration is by definition more than 10 seconds old
                // and never satisfies the grace condition.
                let Some(sid) = session_id else {
                    return Err(ApiError::Auth(AuthError::InvalidToken));
                };
                let tokens = issue_tokens(
                    &state,
                    &mut conn,
                    user_id,
                    SessionContext {
                        session_id: Some(sid),
                        user_agent: sanitize_ua(&headers),
                        ip: sanitize_ip(&client_addr(&headers, &peer, &state)),
                    },
                    user.must_change_password,
                )
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

            // A session the user or an admin deliberately ended is not evidence
            // of theft. Without this, the killed device's next refresh lands
            // here on its existing 15-minute timer, trips the family kill, and
            // logs the user out of the session they explicitly chose to KEEP —
            // turning "sign out my other devices" into "sign out all my devices,
            // on a delay". The comment above records this exact class of bug
            // happening before, with routine deactivations.
            //
            // The scope is surgical: only the three deliberate reasons.
            // REVOKE_LOGOUT keeps its current family-kill behaviour — changing
            // it is a separate decision this slice does not make — and
            // REVOKE_DEACTIVATED keeps its existing re-check branch above.
            if reason
                .as_deref()
                .is_some_and(|r| repo::DELIBERATE_REVOKE_REASONS.contains(&r))
            {
                return Err(ApiError::Auth(AuthError::InvalidToken));
            }

            let revoked = repo::revoke_sessions_for_user(
                &mut conn,
                user_id,
                None,
                repo::REVOKE_REUSE,
                // No actor: nobody chose this, replay detection did.
                None,
            )
            .await?;
            state.revocations.mark_revoked(&revoked);
            tracing::warn!(
                %user_id,
                peer = %peer.ip(),
                revoked = revoked.len(),
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
        SessionContext {
            session_id: token.session_id,
            user_agent: sanitize_ua(&headers),
            ip: sanitize_ip(&client_addr(&headers, &peer, &state)),
        },
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
    // Takes the session with the token. Without this the logged-out session
    // stays live in the owner's own list forever — dead token, live row.
    // Deliberately still unauthenticated: this revokes purely by token hash, and
    // whoever holds the raw refresh token could already revoke it.
    let revoked =
        repo::revoke_refresh_token_and_session(&mut conn, &hash, repo::REVOKE_LOGOUT).await?;
    drop(conn);
    if let Some(sid) = revoked {
        state.revocations.mark_revoked(&[sid]);
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct ForgotPasswordReq {
    pub email: String,
}

/// Start a self-service password reset.
///
/// **Every input that parses gets a byte-identical `200 {"ok": true}`** —
/// unknown address, deactivated account, happy path, and a deployment with no
/// SMTP alike. This route is fully unauthenticated (no `AuthUser`, so it never
/// reaches the `must_change_password` gate), and any answer that varies with
/// account existence or with deployment configuration is an oracle handed to an
/// anonymous caller.
///
/// Constant time is achieved structurally rather than by padding: the handler
/// never touches a socket (S0's outbox owns the SMTP dial), the preflight puts
/// two round trips on both branches, and `enqueue_or_discard` deliberately
/// spends the render, the round trip and the drain nudge on the discard branch
/// too. What is left on the found branch is one local INSERT and two SHA-256
/// hashes — sub-millisecond against network jitter orders of magnitude larger.
/// If instrumentation ever shows a signal, the fix is a fixed-cost pad, not a
/// dummy INSERT, which would be visible in the table.
pub async fn forgot_password(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: axum::http::HeaderMap,
    Json(req): Json<ForgotPasswordReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // Shape only. Allowed to be a distinguishable answer because it does not
    // depend on whether an account exists.
    if req.email.len() > 320 || !req.email.contains('@') {
        return Err(ApiError::BadRequest("a valid email is required".into()));
    }

    let addr = client_addr(&headers, &peer, &state);

    // A 503 here would be a config-state oracle for an anonymous caller on the
    // one endpoint whose entire contract is that every input gets the same
    // answer. The operator learns from S0's startup WARN and from the admin
    // route's 503; the caller learns nothing.
    let (mail, dashboard_url) = match (state.mail.as_ref(), state.cfg.require_dashboard_url()) {
        (Some(m), Ok(url)) => (m.clone(), url.to_string()),
        _ => {
            tracing::error!(
                "forgot-password: SMTP or DASHBOARD_URL is unconfigured; answering 200 and \
                 sending nothing"
            );
            return Ok(Json(serde_json::json!({ "ok": true })));
        }
    };

    // Note the asymmetry with the per-IP limiter below: this one reports, that
    // one refuses. The per-IP budget is a burst limiter over 60 seconds and
    // costs a legitimate caller a retry; the per-email budget is an hour long
    // and is the one an attacker can aim at somebody.
    let within_send_budget = within_budget(
        &state,
        &format!("sauron:auth:forgot:{}", req.email.to_lowercase()),
        FORGOT_ATTEMPTS_PER_EMAIL_PER_HOUR,
        3600,
    )
    .await;
    rate_limit(
        &state,
        &format!("sauron:auth:forgot:ip:{addr}"),
        FORGOT_ATTEMPTS_PER_MIN_PER_IP,
        60,
    )
    .await?;

    let mut conn = db(&state).await?;
    repo::password_reset_preflight(&mut conn).await?;

    // Over budget, and they already hold a working link — so this request would
    // only add a duplicate to an inbox somebody is already flooding. Resolved
    // BEFORE the lookup below and keyed on the address rather than a user id,
    // so it is the same single statement whether or not the account exists;
    // running it inside the found arm would reopen the enumeration oracle in
    // exactly the state an attacker controls.
    let suppress_send = !within_send_budget
        && {
            match repo::has_live_self_service_reset_token(&mut conn, &req.email).await {
                Ok(held) => held,
                // Fail towards sending. This route's whole contract is that a
                // caller always gets the same answer and a legitimate one always
                // gets their link; a lookup failure must not become the lockout
                // the cap was rewritten to remove.
                Err(e) => {
                    tracing::error!(error = %e, "forgot-password: live-token check failed; sending anyway");
                    false
                }
            }
        };

    // Minted before the branch, so both branches have a URL to render and pay
    // the same cost. On the discard branch it corresponds to no row anywhere
    // and the message it lands in is never inserted.
    let raw = opaque_token();
    let reset_url = reset_link(&dashboard_url, &raw);

    let mut recipient: Option<String> = None;
    let mut display_name = String::new();
    let mut user_id: Option<Uuid> = None;

    // Past the preflight, an error from the lookup or the INSERT is logged and
    // still answered 200: both branches have already touched both tables, so a
    // failure is no longer correlated with account existence, and a 500 that
    // fired only on the account-exists branch would be exactly the oracle the
    // preflight just closed.
    match repo::find_user_by_email(&mut conn, &req.email).await {
        // Suppressed requests mint NOTHING. Recording a token we never mail
        // would latch the suppression: each spam request would refresh the
        // "already holds a live link" answer, so the hour after an attacker
        // stopped would still be silent for the person actually asking.
        Ok(Some(user)) if user.is_active && suppress_send => {
            tracing::info!(
                user_id = %user.id,
                "forgot-password: send cap reached and a live link is already outstanding"
            );
        }
        Ok(Some(user)) if user.is_active => {
            // Self-service deliberately does NOT invalidate the user's other
            // outstanding links: an attacker spamming forgot-password against a
            // known address would otherwise kill the link the victim is about
            // to click, turning the anti-abuse limiter into the abuse.
            let fingerprint = hash_token(&user.password_hash);
            match repo::insert_password_reset_token(
                &mut conn,
                user.id,
                hash_token(&raw),
                fingerprint,
                Utc::now() + chrono::Duration::seconds(SELF_RESET_TTL_SECS),
                ResetMode::SelfService.as_str(),
                None,
                Some(addr.clone()),
            )
            .await
            {
                Ok(_) => {
                    display_name = if user.name.trim().is_empty() {
                        user.email.clone()
                    } else {
                        user.name.clone()
                    };
                    user_id = Some(user.id);
                    recipient = Some(user.email);
                }
                Err(e) => {
                    tracing::error!(error = %e, "forgot-password: could not record the reset token")
                }
            }
        }
        Ok(Some(user)) => {
            // Mailing a deactivated account "your account is disabled" is an
            // information leak dressed as helpfulness, and they cannot log in
            // anyway.
            tracing::info!(user_id = %user.id, "forgot-password ignored for a deactivated account");
        }
        Ok(None) => tracing::debug!("forgot-password for an address with no account"),
        Err(e) => tracing::error!(error = %e, "forgot-password: user lookup failed"),
    }

    // `MailSender` owns a `PgPool` and checks out its OWN connection for the
    // enqueue INSERT. The API pool is 16 for the whole process with a 5s
    // checkout timeout, so a handler still holding one while enqueueing needs a
    // seventeenth to make progress: sixteen concurrent resets then hold all
    // sixteen, every one stalls for the full timeout, and every other endpoint
    // in the process 500s alongside them.
    drop(conn);

    // Fallible only because `Cta::new` refuses a link whose scheme is not
    // http(s), i.e. a malformed `DASHBOARD_URL`. That is deployment state, not
    // account state, so it takes the same silent-200 exit as the unconfigured
    // branch above rather than a 500 that would vary this route's answer.
    let content = match render_password_reset_mail(ResetMailVars {
        mode: ResetMode::SelfService,
        display_name: &display_name,
        reset_url: &reset_url,
        org_name: "",
    }) {
        Ok(content) => content,
        Err(e) => {
            tracing::error!(error = %e, "forgot-password: could not render the reset email");
            return Ok(Json(serde_json::json!({ "ok": true })));
        }
    };

    // Rendered and enqueued on BOTH branches. `enqueue_or_discard` runs the
    // same statement against the same index and commits nothing when the
    // recipient is None. An `if let Some(user)` around this is what would
    // rebuild the enumeration oracle S0 went to the trouble of closing.
    if let Err(e) = mail
        .enqueue_or_discard(
            MailKind::PasswordReset,
            recipient.as_deref(),
            &content,
            user_id,
            Duration::from_secs(SELF_RESET_TTL_SECS as u64),
        )
        .await
    {
        tracing::error!(error = %e, "forgot-password: could not enqueue the reset email");
    }

    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct ResetPasswordReq {
    pub token: String,
    pub new_password: String,
}

/// Consume a reset link and set a new password.
///
/// Unauthenticated; the token travels in the body exactly like `logout`'s
/// refresh token. **Every dead-token state answers identically** — unknown,
/// consumed, invalidated, expired, stale fingerprint, and lost the
/// compare-and-swap all collapse to `401 invalid_token`. Distinct codes would
/// tell an attacker spraying the token space whether a guess ever corresponded
/// to a real link, and would leak one user's activity.
///
/// A successful reset deliberately does **not** log the caller in. They proved
/// control of a mailbox, not of a credential; auto-login would make a forwarded
/// or archived message session-equivalent, and it would contradict the step
/// immediately before it, which revoked every session the account had.
///
/// No preflight is needed here, unlike `forgot_password`: every input path
/// touches `password_reset_tokens` unconditionally, so an unmigrated schema
/// 500s uniformly.
pub async fn reset_password(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: axum::http::HeaderMap,
    Json(req): Json<ResetPasswordReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    rate_limit(
        &state,
        &format!(
            "sauron:auth:reset:ip:{}",
            client_addr(&headers, &peer, &state)
        ),
        RESET_ATTEMPTS_PER_MIN_PER_IP,
        60,
    )
    .await?;

    // Copied verbatim from `change_password` so the *length* half of password
    // policy keeps one definition and one set of user-visible strings.
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

    if !is_reset_token_shape(&req.token) {
        return Err(ApiError::Auth(AuthError::InvalidToken));
    }

    let hash = hash_token(&req.token);
    // Keyed on the hash, so nothing sensitive lands in Redis.
    rate_limit(
        &state,
        &format!("sauron:auth:reset:tok:{hash}"),
        RESET_ATTEMPTS_PER_TOKEN_PER_HOUR,
        3600,
    )
    .await?;

    let mut conn = db(&state).await?;
    let row = repo::find_live_password_reset_token(&mut conn, &hash)
        .await?
        .ok_or(ApiError::Auth(AuthError::InvalidToken))?;
    let user = repo::get_user(&mut conn, row.user_id)
        .await?
        .ok_or(ApiError::Auth(AuthError::InvalidToken))?;
    // The holder controls the mailbox, so telling them is honest rather than a
    // leak.
    if !user.is_active {
        return Err(ApiError::Auth(AuthError::AccountDeactivated));
    }

    // "The password changed since this link was issued." Checked at the point
    // of use so it cannot be forgotten by future code, rather than swept from
    // every password-writing path.
    if hash_token(&user.password_hash) != row.password_fingerprint {
        return Err(ApiError::Auth(AuthError::InvalidToken));
    }

    // `change_password` compares plaintexts; there is no current password on
    // this request, so verifying against the stored hash is the only rule this
    // endpoint can enforce. Strictly stronger, one extra Argon2 op, and the
    // user-visible string is the shipped one verbatim so the two endpoints stay
    // consistent. Note this branch returns WITHOUT consuming the token — which
    // is what `RESET_ATTEMPTS_PER_TOKEN_PER_HOUR` exists to bound.
    if verify_password_async(req.new_password.clone(), user.password_hash.clone()).await {
        return Err(ApiError::BadRequest(
            "the new password must be different from the current one".into(),
        ));
    }

    let new_hash = hash_password_async(req.new_password.clone())
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    // The expensive work sits BEFORE the burn: a failed Argon2 must not eat the
    // user's only link, and a crash between the burn and the password write
    // must leave the account unchanged. The burn itself is one atomic
    // `UPDATE … RETURNING`, which is how single-use is enforced without
    // `conn.transaction` (async closures need Rust 1.85; workspace MSRV is
    // 1.82 per packaging/rpm/sauron.spec).
    let Some((user_id, _fingerprint, _mode)) =
        repo::consume_password_reset_token(&mut conn, &hash).await?
    else {
        return Err(ApiError::Auth(AuthError::InvalidToken));
    };

    // Revoke, then write, in `change_password`'s order and for its reason: on a
    // partial failure the account must never end up with a new password and
    // live old sessions. `except` is None because a reset kills every session
    // including any the caller holds; `actor` is None because this path is
    // unauthenticated — nobody has proved who they are, only that they hold the
    // mailbox, and `auth_sessions.revoked_by` must not name the victim as the
    // person who did it.
    let ids =
        repo::revoke_sessions_for_user(&mut conn, user_id, None, repo::REVOKE_PASSWORD_RESET, None)
            .await?;
    // Without this the kill is invisible on THIS replica until its next
    // AUTH_REVOCATION_POLL_SECS tick — the one place the delay is visible.
    state.revocations.mark_revoked(&ids);

    // Zero rows means someone else set the password between the fingerprint
    // read and here, two Argon2 ops of wall clock later. Failing at this point
    // rather than earlier is the acceptable direction: the link is correctly
    // spent, the sessions are revoked, and their new password stands.
    if repo::set_user_password_if_hash_matches(&mut conn, user_id, &user.password_hash, &new_hash)
        .await?
        == 0
    {
        return Err(ApiError::Auth(AuthError::InvalidToken));
    }

    // Bookkeeping, not the guarantee: the fingerprint above already kills every
    // sibling link. Doing it explicitly keeps the table's state legible.
    repo::invalidate_password_reset_tokens_for_user(
        &mut conn,
        user_id,
        repo::RESET_INVALIDATED_PASSWORD_SET,
    )
    .await?;

    // No tokens, no user object.
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
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: axum::http::HeaderMap,
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
    let revoked = repo::revoke_sessions_for_user(
        &mut conn,
        auth.user_id,
        // No exception: the caller's own session dies too. Keeping it would not
        // work anyway — its access token still carries must_change_password, so
        // the extractor gate would keep rejecting the user until it expired.
        None,
        repo::REVOKE_PASSWORD_CHANGED,
        Some(auth.user_id),
    )
    .await?;
    state.revocations.mark_revoked(&revoked);
    repo::set_user_password(&mut conn, auth.user_id, &hash).await?;
    let tokens = issue_tokens(
        &state,
        &mut conn,
        auth.user_id,
        SessionContext {
            session_id: None,
            user_agent: sanitize_ua(&headers),
            ip: sanitize_ip(&client_addr(&headers, &peer, &state)),
        },
        false,
    )
    .await?;

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

#[cfg(test)]
mod password_reset_render_tests {
    use super::*;

    /// `render_password_reset_mail` returns a structural `MailContent`; the two
    /// wire parts only exist after `sauron_mail::render` applies deployment
    /// branding. Asserting on the rendered output rather than the struct is what
    /// keeps these tests honest about what actually reaches an inbox.
    fn branding() -> sauron_mail::Branding {
        sauron_mail::Branding {
            product_name: "Sauron".to_string(),
            dashboard_url: Some("https://s.example".to_string()),
            footer: "Sent by Sauron.".to_string(),
        }
    }

    fn rendered(vars: ResetMailVars<'_>) -> sauron_mail::RenderedMail {
        let content = render_password_reset_mail(vars).expect("content renders");
        sauron_mail::render(&branding(), &content).expect("mail renders")
    }

    #[test]
    fn reset_link_puts_the_token_in_the_fragment() {
        // Browsers never send a fragment in a request line or a Referer, so the
        // token reaches no server log, proxy log or analytics beacon. A
        // pre-hash query string would not have that property.
        assert_eq!(
            reset_link("https://sauron.example.com", "abc123"),
            "https://sauron.example.com/#/reset-password?token=abc123"
        );
    }

    #[test]
    fn reset_link_tolerates_a_trailing_slash() {
        assert_eq!(
            reset_link("https://sauron.example.com/", "abc123"),
            "https://sauron.example.com/#/reset-password?token=abc123"
        );
    }

    #[test]
    fn expiry_wording_is_derived_from_the_constants() {
        // Derived, never hand-typed: changing a TTL must not leave the email
        // claiming the old number.
        assert_eq!(expiry_wording(SELF_RESET_TTL_SECS), "1 hour");
        assert_eq!(expiry_wording(ADMIN_RESET_TTL_SECS), "24 hours");
    }

    #[test]
    fn token_shape_accepts_a_real_token_and_rejects_near_misses() {
        let real = sauron_core::ids::opaque_token();
        assert!(is_reset_token_shape(&real));
        assert!(!is_reset_token_shape(&real[..63]));
        assert!(!is_reset_token_shape(&format!("{real}a")));
        assert!(!is_reset_token_shape(&"z".repeat(64)));
        assert!(!is_reset_token_shape(""));
    }

    #[test]
    fn self_mode_text_carries_the_raw_url_and_the_reassurance() {
        let out = rendered(ResetMailVars {
            mode: ResetMode::SelfService,
            display_name: "Ada",
            reset_url: "https://s.example/#/reset-password?token=deadbeef",
            org_name: "",
        });
        assert_eq!(out.subject, "Reset your Sauron password");
        assert!(out
            .text
            .contains("\nhttps://s.example/#/reset-password?token=deadbeef\n"));
        assert!(out.text.contains("expires in 1 hour"));
        assert!(out.text.contains("If this wasn't you, nothing has changed"));
    }

    #[test]
    fn admin_mode_names_the_org_and_omits_the_ignore_it_reassurance() {
        let out = rendered(ResetMailVars {
            mode: ResetMode::Admin,
            display_name: "Ada",
            reset_url: "https://s.example/#/reset-password?token=deadbeef",
            org_name: "Acme",
        });
        assert_eq!(out.subject, "Set a new Sauron password");
        assert!(out
            .text
            .contains("An administrator of Acme reset your password"));
        assert!(out.text.contains("expires in 24 hours"));
        // Ignoring it is not an option, and a recipient told otherwise will not
        // act until they next try to sign in.
        assert!(!out.text.contains("nothing has changed"));
    }

    #[test]
    fn html_escapes_variables_and_text_does_not() {
        let out = rendered(ResetMailVars {
            mode: ResetMode::SelfService,
            display_name: "<script>alert(1)</script>",
            reset_url: "https://s.example/#/reset-password?token=a&b=c",
            org_name: "",
        });
        assert!(out.text.contains("<script>alert(1)</script>"));
        assert!(!out.html.contains("<script>alert(1)</script>"));
        assert!(out.html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        // Every attribute in these templates is double-quoted, which is what
        // makes `html_escape` safe despite not escaping `'`.
        assert!(out
            .html
            .contains("href=\"https://s.example/#/reset-password?token=a&amp;b=c\""));
    }

    #[test]
    fn an_absent_variable_renders_blank_rather_than_echoing_its_placeholder() {
        // This module builds its prose with `format!`, but the layout it renders
        // into is `substitute`-driven, so this pins the S0 contract it *rests*
        // on. If `substitute` echoed `{{key}}` for an absent one, the first
        // template to gain a fifth variable would mail a literal
        // `{{support_url}}` to a user and nothing else in this file would have
        // noticed.
        let vars: std::collections::BTreeMap<String, String> =
            std::collections::BTreeMap::from([("name".to_string(), "Ada".to_string())]);
        assert_eq!(
            sauron_mail::text::substitute("Hi {{name}}, org {{org_name}}.", &vars),
            "Hi Ada, org ."
        );

        // And the shipped self-service output carries no unsubstituted
        // placeholder of its own — the path that passes `org_name: ""`.
        let out = rendered(ResetMailVars {
            mode: ResetMode::SelfService,
            display_name: "Ada",
            reset_url: "https://s.example/#/reset-password?token=deadbeef",
            org_name: "",
        });
        assert!(!out.text.contains("{{"), "text: {}", out.text);
        assert!(!out.html.contains("{{"), "html: {}", out.html);
    }

    #[test]
    fn modes_carry_their_wire_values_and_ttls() {
        assert_eq!(ResetMode::SelfService.as_str(), "self");
        assert_eq!(ResetMode::Admin.as_str(), "admin");
        assert_eq!(ResetMode::SelfService.ttl_secs(), SELF_RESET_TTL_SECS);
        assert_eq!(ResetMode::Admin.ttl_secs(), ADMIN_RESET_TTL_SECS);
    }

    #[test]
    fn a_url_that_is_not_http_never_becomes_an_href() {
        // `Cta::new` is the only fallible step here, and it is fallible for this
        // reason: a `javascript:` origin reaching an anchor in an email is stored
        // XSS in an inbox. Swallowing the error and shipping a button-less mail
        // would hide an operator's broken DASHBOARD_URL instead of reporting it.
        assert!(render_password_reset_mail(ResetMailVars {
            mode: ResetMode::SelfService,
            display_name: "Ada",
            reset_url: "javascript:alert(1)",
            org_name: "",
        })
        .is_err());
    }
}
