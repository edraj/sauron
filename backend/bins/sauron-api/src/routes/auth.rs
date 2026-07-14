//! Authentication: register, login, refresh (rotating), logout, and `/me`.

use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};

use sauron_auth::{hash_password, hash_token, verify_password, AuthError, AuthUser};
use sauron_db::models::User;
use sauron_db::repo;

use super::{db, issue_tokens, slugify, TokenPair};
use crate::error::ApiError;
use crate::AppState;

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
    Json(req): Json<RegisterReq>,
) -> Result<Json<AuthResponse>, ApiError> {
    if !req.email.contains('@') {
        return Err(ApiError::BadRequest("a valid email is required".into()));
    }
    if req.password.len() < 8 {
        return Err(ApiError::BadRequest(
            "password must be at least 8 characters".into(),
        ));
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

    let hash = hash_password(&req.password).map_err(|e| ApiError::Internal(e.to_string()))?;
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

    let tokens = issue_tokens(&state, &mut conn, user.id, None).await?;
    Ok(Json(AuthResponse { tokens, user }))
}

#[derive(Deserialize)]
pub struct LoginReq {
    pub email: String,
    pub password: String,
}

pub async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginReq>,
) -> Result<Json<AuthResponse>, ApiError> {
    // Throttle password attempts per account (brute-force protection).
    let rl_key = format!("sauron:auth:login:{}", req.email.to_lowercase());
    if let Ok(false) = state.redis.rate_limit_ok(&rl_key, 10, 60).await {
        return Err(ApiError::RateLimited);
    }

    let mut conn = db(&state).await?;
    let user = repo::find_user_by_email(&mut conn, &req.email)
        .await?
        .filter(|u| verify_password(&req.password, &u.password_hash))
        .ok_or(ApiError::Auth(AuthError::InvalidToken))?;

    let _ = repo::touch_last_login(&mut conn, user.id).await;
    let tokens = issue_tokens(&state, &mut conn, user.id, None).await?;
    Ok(Json(AuthResponse { tokens, user }))
}

#[derive(Deserialize)]
pub struct RefreshReq {
    pub refresh_token: String,
}

pub async fn refresh(
    State(state): State<AppState>,
    Json(req): Json<RefreshReq>,
) -> Result<Json<TokenPair>, ApiError> {
    let hash = hash_token(&req.refresh_token);
    let mut conn = db(&state).await?;
    let token = repo::find_active_refresh_token(&mut conn, &hash)
        .await?
        .ok_or(ApiError::Auth(AuthError::InvalidToken))?;

    // Rotate: revoke the presented token, issue a fresh pair.
    repo::revoke_refresh_token(&mut conn, &hash).await?;
    let tokens = issue_tokens(&state, &mut conn, token.user_id, None).await?;
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
    repo::revoke_refresh_token(&mut conn, &hash).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

pub async fn me(auth: AuthUser, State(state): State<AppState>) -> Result<Json<User>, ApiError> {
    let mut conn = db(&state).await?;
    let user = repo::find_user_by_id(&mut conn, auth.user_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(user))
}
