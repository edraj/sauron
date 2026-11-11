//! Personal API keys (MCP server). Rows hold only the SHA-256 of the token.

use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel::sql_types::Bool;
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};
use uuid::Uuid;

use std::collections::HashMap;

use crate::models::{ApiKey, NewApiKey};
use crate::schema::{api_keys, users};

/// An admin with `member:credential` revoked it: "End all sessions" or
/// "Revoke API keys" on the Members page.
pub use crate::repo::REVOKE_ADMIN;
/// The owner reset a forgotten password through the emailed link.
pub use crate::repo::REVOKE_PASSWORD_RESET;
/// An admin forced a password reset on the account.
pub use crate::repo::REVOKE_RESET_FORCED;
/// `api_keys.revoked_reason` values: the `auth_sessions.revoked_reason`
/// vocabulary for the same four events (migration 85's CHECK lists them).
/// The owner revoked it from their Account page.
pub use crate::repo::REVOKE_USER_REVOKED;

/// Revoked keys `list_for_user` returns at most, newest revocation first.
pub const MAX_REVOKED_LISTED: i64 = 50;

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

/// [`insert`], but only while the user holds fewer than `cap` unrevoked keys
/// (expired ones count, as in [`count_live_for_user`]). `None` when the cap
/// is reached.
///
/// One transaction that first locks the owner's `users` row, so concurrent
/// creates for the same user queue behind each other and each counts what
/// the previous one inserted. A bare count-then-insert lets every one of
/// them count the same total and pass. Other users are never blocked.
pub async fn insert_within_cap(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    name: &str,
    prefix: &str,
    hash: &str,
    expires_at: Option<DateTime<Utc>>,
    cap: i64,
) -> QueryResult<Option<ApiKey>> {
    conn.transaction::<_, diesel::result::Error, _>(async |conn| {
        users::table
            .find(user_id)
            .select(users::id)
            .for_update()
            .first::<Uuid>(conn)
            .await?;
        if count_live_for_user(conn, user_id).await? >= cap {
            return Ok(None);
        }
        insert(conn, user_id, name, prefix, hash, expires_at)
            .await
            .map(Some)
    })
    .await
}

/// Unrevoked keys, newest first. Expired keys are included so the owner can
/// see and revoke them. With `include_revoked`, the most recently revoked
/// keys follow (at most [`MAX_REVOKED_LISTED`], newest revocation first), so
/// the owner can see when and why each one stopped working.
pub async fn list_for_user(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    include_revoked: bool,
) -> QueryResult<Vec<ApiKey>> {
    let mut rows = api_keys::table
        .filter(api_keys::user_id.eq(user_id))
        .filter(api_keys::revoked_at.is_null())
        .order((api_keys::created_at.desc(), api_keys::id.desc()))
        .select(ApiKey::as_select())
        .load(conn)
        .await?;
    if include_revoked {
        // Served by `api_keys_user_idx (user_id, revoked_at DESC)`.
        let revoked = api_keys::table
            .filter(api_keys::user_id.eq(user_id))
            .filter(api_keys::revoked_at.is_not_null())
            .order((api_keys::revoked_at.desc(), api_keys::id.desc()))
            .limit(MAX_REVOKED_LISTED)
            .select(ApiKey::as_select())
            .load(conn)
            .await?;
        rows.extend(revoked);
    }
    Ok(rows)
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

/// Revoke the caller's own unrevoked key ([`REVOKE_USER_REVOKED`], by them).
/// `None` when it is not theirs, does not exist or is already revoked.
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
    .set((
        api_keys::revoked_at.eq(Utc::now()),
        api_keys::revoked_reason.eq(Some(REVOKE_USER_REVOKED)),
        api_keys::revoked_by.eq(Some(user_id)),
    ))
    .returning(ApiKey::as_returning())
    .get_result(conn)
    .await
    .optional()
}

/// Revoke every live key of a user, recording `reason` (one of the
/// `REVOKE_*` above) and `by` (the admin or the owner; `None` for the emailed
/// reset). Returns how many were live.
pub async fn revoke_all_for_user(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    reason: &str,
    by: Option<Uuid>,
) -> QueryResult<usize> {
    diesel::update(
        api_keys::table
            .filter(api_keys::user_id.eq(user_id))
            .filter(api_keys::revoked_at.is_null()),
    )
    .set((
        api_keys::revoked_at.eq(Utc::now()),
        api_keys::revoked_reason.eq(Some(reason)),
        api_keys::revoked_by.eq(by),
    ))
    .execute(conn)
    .await
}

/// What the Members page shows about one member's keys.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeySummary {
    /// Keys that work right now: unrevoked and unexpired.
    pub active: i64,
    /// The latest instant a reset through the emailed link revoked keys of
    /// this account ([`REVOKE_PASSWORD_RESET`]), if one ever did.
    pub revoked_by_password_reset_at: Option<DateTime<Utc>>,
}

#[derive(QueryableByName)]
struct KeySummaryRow {
    #[diesel(sql_type = diesel::sql_types::Uuid)]
    user_id: Uuid,
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    active: i64,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Timestamptz>)]
    reset_at: Option<DateTime<Utc>>,
}

/// [`KeySummary`] per user, for the users in `user_ids` that ever held a key
/// (anyone absent has none: the default summary). One grouped read, served by
/// `api_keys_user_idx`.
pub async fn summaries_for_users(
    conn: &mut AsyncPgConnection,
    user_ids: &[Uuid],
) -> QueryResult<HashMap<Uuid, KeySummary>> {
    if user_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows: Vec<KeySummaryRow> = diesel::sql_query(
        "SELECT user_id, \
                count(*) FILTER (WHERE revoked_at IS NULL \
                                   AND (expires_at IS NULL OR expires_at > now()))::bigint \
                    AS active, \
                max(revoked_at) FILTER (WHERE revoked_reason = $2) AS reset_at \
         FROM api_keys WHERE user_id = ANY($1) GROUP BY user_id",
    )
    .bind::<diesel::sql_types::Array<diesel::sql_types::Uuid>, _>(user_ids.to_vec())
    .bind::<diesel::sql_types::Text, _>(REVOKE_PASSWORD_RESET)
    .load(conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            (
                r.user_id,
                KeySummary {
                    active: r.active,
                    revoked_by_password_reset_at: r.reset_at,
                },
            )
        })
        .collect())
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
