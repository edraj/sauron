//! Personal API keys (MCP server). Rows hold only the SHA-256 of the token.

use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel::sql_types::Bool;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use uuid::Uuid;

use crate::models::{ApiKey, NewApiKey};
use crate::schema::{api_keys, users};

pub async fn insert(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    name: &str,
    prefix: &str,
    hash: &str,
    expires_at: Option<DateTime<Utc>>,
) -> QueryResult<ApiKey> {
    diesel::insert_into(api_keys::table)
        .values(NewApiKey {
            user_id,
            name,
            token_prefix: prefix,
            token_hash: hash,
            expires_at,
        })
        .returning(ApiKey::as_returning())
        .get_result(conn)
        .await
}

/// Unrevoked keys, newest first. Expired keys are included so the owner can
/// see and revoke them.
pub async fn list_for_user(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
) -> QueryResult<Vec<ApiKey>> {
    api_keys::table
        .filter(api_keys::user_id.eq(user_id))
        .filter(api_keys::revoked_at.is_null())
        .order((api_keys::created_at.desc(), api_keys::id.desc()))
        .select(ApiKey::as_select())
        .load(conn)
        .await
}

/// Unrevoked keys, the quantity the per-user cap counts.
pub async fn count_live_for_user(conn: &mut AsyncPgConnection, user_id: Uuid) -> QueryResult<i64> {
    api_keys::table
        .filter(api_keys::user_id.eq(user_id))
        .filter(api_keys::revoked_at.is_null())
        .count()
        .get_result(conn)
        .await
}

/// Revoke the caller's own unrevoked key. `None` when it is not theirs, does
/// not exist or is already revoked.
pub async fn revoke(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    key_id: Uuid,
) -> QueryResult<Option<ApiKey>> {
    diesel::update(
        api_keys::table
            .filter(api_keys::id.eq(key_id))
            .filter(api_keys::user_id.eq(user_id))
            .filter(api_keys::revoked_at.is_null()),
    )
    .set(api_keys::revoked_at.eq(Utc::now()))
    .returning(ApiKey::as_returning())
    .get_result(conn)
    .await
    .optional()
}

/// Revoke every live key of a user; returns how many were live.
pub async fn revoke_all_for_user(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
) -> QueryResult<usize> {
    diesel::update(
        api_keys::table
            .filter(api_keys::user_id.eq(user_id))
            .filter(api_keys::revoked_at.is_null()),
    )
    .set(api_keys::revoked_at.eq(Utc::now()))
    .execute(conn)
    .await
}

/// The owner state a key authenticates against.
#[derive(Debug, Clone)]
pub struct KeyOwner {
    pub key_id: Uuid,
    pub user_id: Uuid,
    pub is_active: bool,
    pub must_change_password: bool,
    pub credentials_invalidated: bool,
}

/// `(key id, user id, is_active, must_change_password, credentials_invalidated_at)`.
type AuthRow = (Uuid, Uuid, bool, bool, Option<DateTime<Utc>>);

/// Look up a live (unrevoked, unexpired) key by token hash.
pub async fn authenticate(
    conn: &mut AsyncPgConnection,
    hash: &str,
) -> QueryResult<Option<KeyOwner>> {
    let row: Option<AuthRow> = api_keys::table
        .inner_join(users::table)
        .filter(api_keys::token_hash.eq(hash))
        .filter(api_keys::revoked_at.is_null())
        .filter(
            api_keys::expires_at
                .is_null()
                .or(api_keys::expires_at.gt(diesel::dsl::now)),
        )
        .select((
            api_keys::id,
            users::id,
            users::is_active,
            users::must_change_password,
            users::credentials_invalidated_at,
        ))
        .first(conn)
        .await
        .optional()?;
    Ok(row.map(
        |(key_id, user_id, is_active, must_change_password, invalidated_at)| KeyOwner {
            key_id,
            user_id,
            is_active,
            must_change_password,
            credentials_invalidated: invalidated_at.is_some(),
        },
    ))
}

/// Record a use, at most once a minute per key.
pub async fn touch_last_used(conn: &mut AsyncPgConnection, key_id: Uuid) -> QueryResult<()> {
    diesel::update(
        api_keys::table.filter(api_keys::id.eq(key_id)).filter(
            api_keys::last_used_at
                .is_null()
                .or(diesel::dsl::sql::<Bool>(
                    "last_used_at < now() - interval '60 seconds'",
                )),
        ),
    )
    .set(api_keys::last_used_at.eq(Utc::now()))
    .execute(conn)
    .await?;
    Ok(())
}
