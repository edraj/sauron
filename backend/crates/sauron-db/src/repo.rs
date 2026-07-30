//! Repository functions. Each takes `&mut AsyncPgConnection` and returns a
//! `QueryResult`. Grouped by domain.

use chrono::{DateTime, Utc};
use diesel::dsl::sql;
use diesel::prelude::*;
use diesel::sql_types::{
    Array, BigInt, Bool, Double, Integer, Jsonb, Nullable, Text, Timestamptz, Uuid as SqlUuid,
};
use diesel::upsert::excluded;
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl, SimpleAsyncConnection};
use serde_json::Value;
use std::collections::HashMap;
use uuid::Uuid;

use crate::models::*;
use crate::schema::*;
use crate::scope::{EnvFilter, ReadScope};

// ===========================================================================
// Users & refresh tokens
// ===========================================================================

pub async fn create_user(
    conn: &mut AsyncPgConnection,
    email: &str,
    password_hash: &str,
    name: &str,
) -> QueryResult<User> {
    let email = email.to_lowercase();
    diesel::insert_into(users::table)
        .values(NewUser {
            email: &email,
            password_hash,
            name,
        })
        .returning(User::as_returning())
        .get_result(conn)
        .await
}

#[derive(Debug, QueryableByName)]
pub struct NewMemberRow {
    #[diesel(sql_type = SqlUuid)]
    pub user_id: Uuid,
    #[diesel(sql_type = SqlUuid)]
    pub grant_id: Uuid,
}

/// Create a user and all of their initial grants in one statement.
///
/// A single data-modifying CTE rather than a transaction: Postgres runs both
/// INSERTs atomically within the statement, so a grant failure rolls the user
/// back for free. This avoids `conn.transaction`, whose diesel-async 0.9
/// signature needs async closures (Rust 1.85) and would push the workspace
/// MSRV past the 1.82 the RPM spec builds against. The scopes travel as two
/// parallel arrays unnested into rows, so N grants stay one round trip; they
/// must be the same length, as multi-argument `unnest` pads the shorter one
/// with NULLs that then fail `role_grants`' NOT NULL.
///
/// The caller must de-duplicate `(scope_type, scope_id)` pairs first: a repeat
/// trips `role_grants`' UNIQUE key, and that `UniqueViolation` is
/// indistinguishable here from the duplicate-email one below.
///
/// A duplicate email surfaces as `DatabaseError(UniqueViolation)` from
/// `users_email_lower_key`; the caller maps that to 409.
#[allow(clippy::too_many_arguments)]
pub async fn create_member_with_grants(
    conn: &mut AsyncPgConnection,
    email: &str,
    password_hash: &str,
    name: &str,
    org_id: Uuid,
    role_id: Uuid,
    scope_types: &[String],
    scope_ids: &[Uuid],
) -> QueryResult<Vec<NewMemberRow>> {
    let email = email.to_lowercase();
    diesel::sql_query(
        "WITH new_user AS ( \
             INSERT INTO users (email, password_hash, name, must_change_password) \
             VALUES ($1, $2, $3, true) \
             RETURNING id \
         ) \
         INSERT INTO role_grants (org_id, user_id, role_id, scope_type, scope_id) \
         SELECT $4, new_user.id, $5, s.scope_type, s.scope_id \
         FROM new_user, unnest($6::text[], $7::uuid[]) AS s(scope_type, scope_id) \
         RETURNING user_id, id AS grant_id",
    )
    .bind::<Text, _>(email)
    .bind::<Text, _>(password_hash)
    .bind::<Text, _>(name)
    .bind::<SqlUuid, _>(org_id)
    .bind::<SqlUuid, _>(role_id)
    .bind::<Array<Text>, _>(scope_types.to_vec())
    .bind::<Array<SqlUuid>, _>(scope_ids.to_vec())
    .get_results(conn)
    .await
}

pub async fn find_user_by_email(
    conn: &mut AsyncPgConnection,
    email: &str,
) -> QueryResult<Option<User>> {
    let email = email.to_lowercase();
    users::table
        .filter(users::email.eq(email))
        .select(User::as_select())
        .first(conn)
        .await
        .optional()
}

pub async fn find_user_by_id(conn: &mut AsyncPgConnection, id: Uuid) -> QueryResult<Option<User>> {
    users::table
        .find(id)
        .select(User::as_select())
        .first(conn)
        .await
        .optional()
}

pub async fn touch_last_login(conn: &mut AsyncPgConnection, id: Uuid) -> QueryResult<usize> {
    diesel::update(users::table.find(id))
        .set(users::last_login_at.eq(Utc::now()))
        .execute(conn)
        .await
}

pub async fn get_user(conn: &mut AsyncPgConnection, id: Uuid) -> QueryResult<Option<User>> {
    users::table
        .find(id)
        .select(User::as_select())
        .first(conn)
        .await
        .optional()
}

pub async fn set_user_active(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    active: bool,
) -> QueryResult<usize> {
    diesel::update(users::table.find(user_id))
        .set((
            users::is_active.eq(active),
            users::updated_at.eq(Utc::now()),
        ))
        .execute(conn)
        .await
}

/// Set a new password and clear the forced-change flag. Always clears it: the
/// only way to reach this is the self-service change endpoint, where the user
/// chose the password themselves.
pub async fn set_user_password(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    password_hash: &str,
) -> QueryResult<usize> {
    diesel::update(users::table.find(user_id))
        .set((
            users::password_hash.eq(password_hash),
            users::must_change_password.eq(false),
            users::updated_at.eq(Utc::now()),
        ))
        .execute(conn)
        .await
}

pub async fn insert_refresh_token(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    token_hash: String,
    expires_at: DateTime<Utc>,
    user_agent: Option<String>,
) -> QueryResult<usize> {
    diesel::insert_into(refresh_tokens::table)
        .values(NewRefreshToken {
            user_id,
            token_hash,
            expires_at,
            user_agent,
        })
        .execute(conn)
        .await
}

pub async fn find_active_refresh_token(
    conn: &mut AsyncPgConnection,
    token_hash: &str,
) -> QueryResult<Option<RefreshToken>> {
    refresh_tokens::table
        .filter(refresh_tokens::token_hash.eq(token_hash))
        .filter(refresh_tokens::revoked_at.is_null())
        .filter(refresh_tokens::expires_at.gt(Utc::now()))
        .select(RefreshToken::as_select())
        .first(conn)
        .await
        .optional()
}

/// Revoked because it was exchanged for a successor — the normal refresh path.
/// Only this reason is eligible for the concurrent-refresh grace window.
pub const REVOKE_ROTATED: &str = "rotated";
/// Revoked by an explicit logout.
pub const REVOKE_LOGOUT: &str = "logout";
/// Revoked as part of a token-family kill after replay was detected.
pub const REVOKE_REUSE: &str = "reuse";
/// Refresh tokens killed because an admin deactivated the account. Distinct
/// from `REVOKE_REUSE` so the rotation grace window (which exists to survive
/// two dashboard tabs racing) can never resurrect a deactivated session.
pub const REVOKE_DEACTIVATED: &str = "deactivated";
/// Refresh tokens rotated out because the user changed their own password.
pub const REVOKE_PASSWORD_CHANGED: &str = "password_changed";

pub async fn revoke_refresh_token(
    conn: &mut AsyncPgConnection,
    token_hash: &str,
    reason: &str,
) -> QueryResult<usize> {
    diesel::update(refresh_tokens::table.filter(refresh_tokens::token_hash.eq(token_hash)))
        .set((
            refresh_tokens::revoked_at.eq(Utc::now()),
            refresh_tokens::revoked_reason.eq(reason),
        ))
        .execute(conn)
        .await
}

/// Revocation metadata for a token hash, whatever its state.
///
/// Returns `(user_id, revoked_at, revoked_reason)`. The handler needs all three
/// to tell a benign concurrent refresh from a genuine replay.
pub async fn refresh_token_revocation(
    conn: &mut AsyncPgConnection,
    token_hash: &str,
) -> QueryResult<Option<(Uuid, Option<DateTime<Utc>>, Option<String>)>> {
    refresh_tokens::table
        .filter(refresh_tokens::token_hash.eq(token_hash))
        .select((
            refresh_tokens::user_id,
            refresh_tokens::revoked_at,
            refresh_tokens::revoked_reason,
        ))
        .first(conn)
        .await
        .optional()
}

/// Whether the user still holds any usable refresh token.
///
/// After a family kill there are none, which is what stops the grace window
/// from resurrecting a session that was just revoked for replay.
pub async fn user_has_active_refresh_token(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
) -> QueryResult<bool> {
    use diesel::dsl::exists;
    diesel::select(exists(
        refresh_tokens::table
            .filter(refresh_tokens::user_id.eq(user_id))
            .filter(refresh_tokens::revoked_at.is_null())
            .filter(refresh_tokens::expires_at.gt(Utc::now())),
    ))
    .get_result(conn)
    .await
}

/// The owner of a refresh-token hash **regardless of revocation/expiry**.
///
/// Used to detect replay of an already-rotated token: `find_active_refresh_token`
/// cannot distinguish "never existed" from "already used", but that difference
/// is the whole theft signal in a rotating-refresh scheme.
pub async fn refresh_token_owner(
    conn: &mut AsyncPgConnection,
    token_hash: &str,
) -> QueryResult<Option<Uuid>> {
    refresh_tokens::table
        .filter(refresh_tokens::token_hash.eq(token_hash))
        .select(refresh_tokens::user_id)
        .first(conn)
        .await
        .optional()
}

/// Revoke every still-active refresh token for a user (token-family kill).
pub async fn revoke_all_refresh_tokens_for_user(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
) -> QueryResult<usize> {
    diesel::update(
        refresh_tokens::table
            .filter(refresh_tokens::user_id.eq(user_id))
            .filter(refresh_tokens::revoked_at.is_null()),
    )
    .set((
        refresh_tokens::revoked_at.eq(Utc::now()),
        refresh_tokens::revoked_reason.eq(REVOKE_REUSE),
    ))
    .execute(conn)
    .await
}

pub async fn revoke_all_refresh_tokens_for_user_with_reason(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    reason: &str,
) -> QueryResult<usize> {
    diesel::update(
        refresh_tokens::table
            .filter(refresh_tokens::user_id.eq(user_id))
            .filter(refresh_tokens::revoked_at.is_null()),
    )
    .set((
        refresh_tokens::revoked_at.eq(Utc::now()),
        refresh_tokens::revoked_reason.eq(reason),
    ))
    .execute(conn)
    .await
}

// ===========================================================================
// Organizations
// ===========================================================================

pub async fn create_org(
    conn: &mut AsyncPgConnection,
    name: &str,
    slug: &str,
) -> QueryResult<Organization> {
    diesel::insert_into(organizations::table)
        .values(NewOrganization { name, slug })
        .returning(Organization::as_returning())
        .get_result(conn)
        .await
}

pub async fn get_org(conn: &mut AsyncPgConnection, id: Uuid) -> QueryResult<Option<Organization>> {
    organizations::table
        .find(id)
        .select(Organization::as_select())
        .first(conn)
        .await
        .optional()
}

/// Orgs the user has any grant in.
pub async fn list_orgs_for_user(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
) -> QueryResult<Vec<Organization>> {
    let org_ids: Vec<Uuid> = role_grants::table
        .filter(role_grants::user_id.eq(user_id))
        .select(role_grants::org_id)
        .distinct()
        .load(conn)
        .await?;
    organizations::table
        .filter(organizations::id.eq_any(org_ids))
        .select(Organization::as_select())
        .order(organizations::created_at.asc())
        .load(conn)
        .await
}

// ===========================================================================
// RBAC: roles & grants
// ===========================================================================

/// System presets + this org's custom roles.
pub async fn list_roles(conn: &mut AsyncPgConnection, org_id: Uuid) -> QueryResult<Vec<Role>> {
    roles::table
        .filter(roles::org_id.is_null().or(roles::org_id.eq(org_id)))
        .select(Role::as_select())
        .order((roles::is_system.desc(), roles::name.asc()))
        .load(conn)
        .await
}

pub async fn get_role(conn: &mut AsyncPgConnection, id: Uuid) -> QueryResult<Option<Role>> {
    roles::table
        .find(id)
        .select(Role::as_select())
        .first(conn)
        .await
        .optional()
}

pub async fn get_system_role(
    conn: &mut AsyncPgConnection,
    name: &str,
) -> QueryResult<Option<Role>> {
    roles::table
        .filter(roles::org_id.is_null())
        .filter(roles::name.eq(name))
        .select(Role::as_select())
        .first(conn)
        .await
        .optional()
}

pub async fn create_role(
    conn: &mut AsyncPgConnection,
    org_id: Uuid,
    name: &str,
    description: &str,
    permissions: Value,
) -> QueryResult<Role> {
    diesel::insert_into(roles::table)
        .values(NewRole {
            org_id: Some(org_id),
            name,
            description,
            is_system: false,
            permissions,
        })
        .returning(Role::as_returning())
        .get_result(conn)
        .await
}

/// Update a custom role. Scoped by `org_id` as well as `role_id` so a mistaken
/// call cannot reach across orgs, and filtered on `is_system` so a preset can
/// never be written even if a caller-side check is missed.
pub async fn update_role(
    conn: &mut AsyncPgConnection,
    org_id: Uuid,
    role_id: Uuid,
    name: &str,
    description: &str,
    permissions: Value,
) -> QueryResult<Role> {
    diesel::update(
        roles::table
            .filter(roles::id.eq(role_id))
            .filter(roles::org_id.eq(org_id))
            .filter(roles::is_system.eq(false)),
    )
    .set((
        roles::name.eq(name),
        roles::description.eq(description),
        roles::permissions.eq(permissions),
    ))
    .returning(Role::as_returning())
    .get_result(conn)
    .await
}

/// Idempotently upsert a system preset role (keeps DB in sync with code).
pub async fn upsert_preset_role(
    conn: &mut AsyncPgConnection,
    name: &str,
    description: &str,
    permissions: &Value,
) -> QueryResult<usize> {
    diesel::sql_query(
        "INSERT INTO roles (org_id, name, description, is_system, permissions) \
         VALUES (NULL, $1, $2, true, $3) \
         ON CONFLICT (name) WHERE org_id IS NULL \
         DO UPDATE SET permissions = EXCLUDED.permissions, description = EXCLUDED.description",
    )
    .bind::<Text, _>(name)
    .bind::<Text, _>(description)
    .bind::<diesel::sql_types::Jsonb, _>(permissions.clone())
    .execute(conn)
    .await
}

pub async fn create_grant(
    conn: &mut AsyncPgConnection,
    grant: NewRoleGrant,
) -> QueryResult<RoleGrant> {
    diesel::insert_into(role_grants::table)
        .values(&grant)
        .on_conflict((
            role_grants::user_id,
            role_grants::role_id,
            role_grants::scope_type,
            role_grants::scope_id,
        ))
        .do_update()
        .set(role_grants::org_id.eq(excluded(role_grants::org_id)))
        .returning(RoleGrant::as_returning())
        .get_result(conn)
        .await
}

/// Upsert a batch of grants in one statement, same idempotent semantics as
/// `create_grant`: re-granting an existing `(user, role, scope)` just re-points
/// its `org_id`. Because that is a DO UPDATE rather than DO NOTHING, every row
/// comes back, so the caller can rely on `ids.len() == rows.len()`.
pub async fn create_grants(
    conn: &mut AsyncPgConnection,
    rows: Vec<NewRoleGrant>,
) -> QueryResult<Vec<Uuid>> {
    // An empty VALUES list is not valid SQL; nothing to insert either way.
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    diesel::insert_into(role_grants::table)
        .values(&rows)
        .on_conflict((
            role_grants::user_id,
            role_grants::role_id,
            role_grants::scope_type,
            role_grants::scope_id,
        ))
        .do_update()
        .set(role_grants::org_id.eq(excluded(role_grants::org_id)))
        .returning(role_grants::id)
        .get_results(conn)
        .await
}

pub async fn delete_grant(
    conn: &mut AsyncPgConnection,
    org_id: Uuid,
    grant_id: Uuid,
) -> QueryResult<usize> {
    diesel::delete(
        role_grants::table
            .filter(role_grants::id.eq(grant_id))
            .filter(role_grants::org_id.eq(org_id)),
    )
    .execute(conn)
    .await
}

pub async fn update_grant(
    conn: &mut AsyncPgConnection,
    grant_id: Uuid,
    role_id: Uuid,
    scope_type: &str,
    scope_id: Uuid,
) -> QueryResult<RoleGrant> {
    diesel::update(role_grants::table.find(grant_id))
        .set((
            role_grants::role_id.eq(role_id),
            role_grants::scope_type.eq(scope_type),
            role_grants::scope_id.eq(scope_id),
        ))
        .returning(RoleGrant::as_returning())
        .get_result(conn)
        .await
}

/// The org a grant belongs to (for authorizing its deletion).
pub async fn grant_org(conn: &mut AsyncPgConnection, grant_id: Uuid) -> QueryResult<Option<Uuid>> {
    role_grants::table
        .find(grant_id)
        .select(role_grants::org_id)
        .first(conn)
        .await
        .optional()
}

/// The full grant row, so the caller can evaluate its role and scope before
/// allowing a deletion.
pub async fn get_grant(
    conn: &mut AsyncPgConnection,
    grant_id: Uuid,
) -> QueryResult<Option<RoleGrant>> {
    role_grants::table
        .find(grant_id)
        .select(RoleGrant::as_select())
        .first(conn)
        .await
        .optional()
}

/// How many grants in `org_id` — other than `exclude_id` — confer `org:manage`.
///
/// Guards against deleting the last administrator: with no `org:manage` left,
/// the anti-escalation rule in `create_grant` makes it impossible for anyone to
/// grant it again.
pub async fn count_org_manage_grants_excluding(
    conn: &mut AsyncPgConnection,
    org_id: Uuid,
    exclude_id: Uuid,
) -> QueryResult<i64> {
    let row: GrantCountRow = diesel::sql_query(
        "SELECT count(*)::bigint AS n \
         FROM role_grants g JOIN roles r ON g.role_id = r.id \
         WHERE g.org_id = $1 AND g.id <> $2 AND g.scope_type = 'org' \
           AND r.permissions @> to_jsonb('org:manage'::text)",
    )
    .bind::<SqlUuid, _>(org_id)
    .bind::<SqlUuid, _>(exclude_id)
    .get_result(conn)
    .await?;
    Ok(row.n)
}

#[derive(Debug, QueryableByName)]
pub struct GrantCountRow {
    #[diesel(sql_type = BigInt)]
    pub n: i64,
}

/// How many grants would still confer `org:manage` in this org if `role_id`
/// stopped conferring it.
///
/// Editing a role affects every grant that holds it at once, unlike deleting
/// one grant or deactivating one user, so the exclusion here is by role
/// rather than by grant id or user id.
pub async fn count_org_manage_grants_excluding_role(
    conn: &mut AsyncPgConnection,
    org_id: Uuid,
    role_id: Uuid,
) -> QueryResult<i64> {
    let row: GrantCountRow = diesel::sql_query(
        "SELECT count(*)::bigint AS n \
         FROM role_grants g JOIN roles r ON g.role_id = r.id \
         WHERE g.org_id = $1 AND g.role_id <> $2 AND g.scope_type = 'org' \
           AND r.permissions @> to_jsonb('org:manage'::text)",
    )
    .bind::<SqlUuid, _>(org_id)
    .bind::<SqlUuid, _>(role_id)
    .get_result(conn)
    .await?;
    Ok(row.n)
}

/// How many grants this user holds in orgs *other* than `org_id`.
///
/// Deactivation is account-global, but `member:manage` is org-scoped. If the
/// target belongs to another org too, this org's admin has no authority to
/// disable their login there, so a non-zero count blocks the operation.
pub async fn count_user_grants_outside_org(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    org_id: Uuid,
) -> QueryResult<i64> {
    let row: GrantCountRow = diesel::sql_query(
        "SELECT count(*)::bigint AS n FROM role_grants \
         WHERE user_id = $1 AND org_id <> $2",
    )
    .bind::<SqlUuid, _>(user_id)
    .bind::<SqlUuid, _>(org_id)
    .get_result(conn)
    .await?;
    Ok(row.n)
}

/// How many grants conferring `org:manage` this org would still have if every
/// grant belonging to `user_id` were ignored.
///
/// `count_org_manage_grants_excluding` excludes a single grant, which is right
/// for deleting one. Deactivation disables a whole person, who may hold several
/// org:manage grants at once, so the exclusion has to be by user.
///
/// Unlike its two siblings this one joins `users.is_active`: it guards a
/// *deactivation*, and a holder who is already deactivated cannot administer
/// anything, so counting them would let an admin walk the org's owners down one
/// at a time — each deactivation kept legal by the ones already performed. The
/// other three clauses stay identical to the siblings on purpose; they must all
/// agree on what "a grant conferring org:manage" is.
pub async fn count_org_manage_grants_for_user_excluding_user(
    conn: &mut AsyncPgConnection,
    org_id: Uuid,
    user_id: Uuid,
) -> QueryResult<i64> {
    let row: GrantCountRow = diesel::sql_query(
        "SELECT count(*)::bigint AS n \
         FROM role_grants g JOIN roles r ON g.role_id = r.id \
         JOIN users u ON u.id = g.user_id AND u.is_active \
         WHERE g.org_id = $1 AND g.user_id <> $2 AND g.scope_type = 'org' \
           AND r.permissions @> to_jsonb('org:manage'::text)",
    )
    .bind::<SqlUuid, _>(org_id)
    .bind::<SqlUuid, _>(user_id)
    .get_result(conn)
    .await?;
    Ok(row.n)
}

/// All grants in an org with the user email/name/active-status and role name,
/// for the members page.
pub async fn list_org_grants(
    conn: &mut AsyncPgConnection,
    org_id: Uuid,
) -> QueryResult<Vec<(RoleGrant, String, String, String, bool)>> {
    role_grants::table
        .inner_join(users::table.on(users::id.eq(role_grants::user_id)))
        .inner_join(roles::table.on(roles::id.eq(role_grants::role_id)))
        .filter(role_grants::org_id.eq(org_id))
        .select((
            RoleGrant::as_select(),
            users::email,
            users::name,
            roles::name,
            users::is_active,
        ))
        .order(role_grants::created_at.asc())
        .load(conn)
        .await
}

/// `(scope_type, scope_id, permissions)` for every grant the user holds in the
/// org — the raw material for permission resolution.
pub async fn user_grants_in_org(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    org_id: Uuid,
) -> QueryResult<Vec<(String, Uuid, Value)>> {
    role_grants::table
        .inner_join(roles::table.on(roles::id.eq(role_grants::role_id)))
        .filter(role_grants::user_id.eq(user_id))
        .filter(role_grants::org_id.eq(org_id))
        .select((
            role_grants::scope_type,
            role_grants::scope_id,
            roles::permissions,
        ))
        .load(conn)
        .await
}

// ===========================================================================
// Projects (grouping)
// ===========================================================================

pub async fn create_project(
    conn: &mut AsyncPgConnection,
    org_id: Uuid,
    name: &str,
    slug: &str,
) -> QueryResult<Project> {
    diesel::insert_into(projects::table)
        .values(NewProject { org_id, name, slug })
        .returning(Project::as_returning())
        .get_result(conn)
        .await
}

pub async fn list_projects_for_org(
    conn: &mut AsyncPgConnection,
    org_id: Uuid,
) -> QueryResult<Vec<Project>> {
    projects::table
        .filter(projects::org_id.eq(org_id))
        .select(Project::as_select())
        .order(projects::created_at.asc())
        .load(conn)
        .await
}

pub async fn get_project(conn: &mut AsyncPgConnection, id: Uuid) -> QueryResult<Option<Project>> {
    projects::table
        .find(id)
        .select(Project::as_select())
        .first(conn)
        .await
        .optional()
}

pub async fn rename_project(
    conn: &mut AsyncPgConnection,
    id: Uuid,
    name: &str,
) -> QueryResult<Option<Project>> {
    diesel::update(projects::table.find(id))
        .set((projects::name.eq(name), projects::updated_at.eq(Utc::now())))
        .returning(Project::as_returning())
        .get_result(conn)
        .await
        .optional()
}

pub async fn delete_project(conn: &mut AsyncPgConnection, id: Uuid) -> QueryResult<usize> {
    diesel::delete(projects::table.find(id)).execute(conn).await
}

/// The org that owns a project.
pub async fn project_org(
    conn: &mut AsyncPgConnection,
    project_id: Uuid,
) -> QueryResult<Option<Uuid>> {
    projects::table
        .find(project_id)
        .select(projects::org_id)
        .first(conn)
        .await
        .optional()
}

/// The projects among `ids` that belong to `org_id` — the discovery-query
/// counterpart to `list_projects_for_org`: a caller whose reach is a handful of
/// scoped grants (rather than the whole org) only gets those projects back,
/// not every project in the org.
pub async fn list_projects_by_ids_in_org(
    conn: &mut AsyncPgConnection,
    org_id: Uuid,
    ids: &[Uuid],
) -> QueryResult<Vec<Project>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    projects::table
        .filter(projects::org_id.eq(org_id))
        .filter(projects::id.eq_any(ids.to_vec()))
        .select(Project::as_select())
        .order(projects::created_at.asc())
        .load(conn)
        .await
}

/// Which of `ids` are projects in `org_id`. Used to validate a batch of
/// scopes without one round trip per scope.
pub async fn projects_in_org(
    conn: &mut AsyncPgConnection,
    org_id: Uuid,
    ids: &[Uuid],
) -> QueryResult<Vec<Uuid>> {
    projects::table
        .filter(projects::org_id.eq(org_id))
        .filter(projects::id.eq_any(ids.to_vec()))
        .select(projects::id)
        .load(conn)
        .await
}

// ===========================================================================
// Apps (ingest unit)
// ===========================================================================

pub async fn create_app(
    conn: &mut AsyncPgConnection,
    project_id: Uuid,
    name: &str,
    slug: &str,
    app_type: &str,
) -> QueryResult<App> {
    diesel::insert_into(apps::table)
        .values(NewApp {
            project_id,
            name,
            slug,
            app_type,
        })
        .returning(App::as_returning())
        .get_result(conn)
        .await
}

pub async fn list_apps_for_project(
    conn: &mut AsyncPgConnection,
    project_id: Uuid,
) -> QueryResult<Vec<App>> {
    apps::table
        .filter(apps::project_id.eq(project_id))
        .select(App::as_select())
        .order(apps::created_at.asc())
        .load(conn)
        .await
}

pub async fn get_app(conn: &mut AsyncPgConnection, id: Uuid) -> QueryResult<Option<App>> {
    apps::table
        .find(id)
        .select(App::as_select())
        .first(conn)
        .await
        .optional()
}

pub async fn update_app(
    conn: &mut AsyncPgConnection,
    id: Uuid,
    name: &str,
    ingest_enabled: bool,
) -> QueryResult<Option<App>> {
    diesel::update(apps::table.find(id))
        .set((
            apps::name.eq(name),
            apps::ingest_enabled.eq(ingest_enabled),
            apps::updated_at.eq(Utc::now()),
        ))
        .returning(App::as_returning())
        .get_result(conn)
        .await
        .optional()
}

pub async fn delete_app(conn: &mut AsyncPgConnection, id: Uuid) -> QueryResult<usize> {
    diesel::delete(apps::table.find(id)).execute(conn).await
}

/// `(project_id, org_id)` ancestry of an app — for permission resolution.
pub async fn app_ancestry(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
) -> QueryResult<Option<(Uuid, Uuid)>> {
    apps::table
        .inner_join(projects::table.on(projects::id.eq(apps::project_id)))
        .filter(apps::id.eq(app_id))
        .select((apps::project_id, projects::org_id))
        .first(conn)
        .await
        .optional()
}

/// `(app_id, project_id, org_id)` for each of `ids` that resolves — the
/// batched `app_ancestry`, so validating a batch of scopes costs one query.
/// Ids that are not apps are simply absent from the result.
pub async fn app_ancestries(
    conn: &mut AsyncPgConnection,
    ids: &[Uuid],
) -> QueryResult<Vec<(Uuid, Uuid, Uuid)>> {
    apps::table
        .inner_join(projects::table.on(projects::id.eq(apps::project_id)))
        .filter(apps::id.eq_any(ids.to_vec()))
        .select((apps::id, apps::project_id, projects::org_id))
        .load(conn)
        .await
}

/// `(env_id, app_id, project_id, org_id)` for each of `ids` that resolves —
/// the batched `env_ancestry`, mirroring `app_ancestries` exactly so
/// validating a batch of scopes that mixes apps and envs still costs one
/// query per kind. Ids that are not environments are simply absent from the
/// result.
pub async fn env_ancestries(
    conn: &mut AsyncPgConnection,
    ids: &[Uuid],
) -> QueryResult<Vec<(Uuid, Uuid, Uuid, Uuid)>> {
    app_environments::table
        .inner_join(apps::table.on(apps::id.eq(app_environments::app_id)))
        .inner_join(projects::table.on(projects::id.eq(apps::project_id)))
        .filter(app_environments::id.eq_any(ids.to_vec()))
        .select((
            app_environments::id,
            app_environments::app_id,
            apps::project_id,
            projects::org_id,
        ))
        .load(conn)
        .await
}

// --- environments: the project-level catalogue -------------------------------

/// Cap on how many live environments a project may hold. Creation is an
/// authenticated admin action rather than a side effect of ingest, so this is a
/// sanity bound rather than an abuse control.
///
/// The cap moved from per-app to per-project along with the environments
/// themselves. It also now bounds a *fan-out*: creating one environment enrolls
/// every app in the project, so the real ceiling on rows created is this times
/// the app count.
pub const MAX_ENVIRONMENTS_PER_PROJECT: i64 = 500;

pub async fn create_project_environment(
    conn: &mut AsyncPgConnection,
    project_id: Uuid,
    name: &str,
) -> QueryResult<Environment> {
    diesel::insert_into(environments::table)
        .values(NewEnvironment { project_id, name })
        .returning(Environment::as_returning())
        .get_result(conn)
        .await
}

pub async fn list_project_environments(
    conn: &mut AsyncPgConnection,
    project_id: Uuid,
    include_retired: bool,
) -> QueryResult<Vec<Environment>> {
    let mut q = environments::table
        .filter(environments::project_id.eq(project_id))
        .into_boxed();
    if !include_retired {
        q = q.filter(environments::retired_at.is_null());
    }
    q.select(Environment::as_select())
        .order(environments::name.asc())
        .limit(MAX_ENVIRONMENTS_PER_PROJECT)
        .load(conn)
        .await
}

pub async fn get_project_environment(
    conn: &mut AsyncPgConnection,
    id: Uuid,
) -> QueryResult<Option<Environment>> {
    environments::table
        .find(id)
        .select(Environment::as_select())
        .first(conn)
        .await
        .optional()
}

/// `(project_id, org_id)` ancestry of a catalogue environment, for authorizing
/// the project-level CRUD routes.
pub async fn project_env_ancestry(
    conn: &mut AsyncPgConnection,
    env_id: Uuid,
) -> QueryResult<Option<(Uuid, Uuid)>> {
    environments::table
        .inner_join(projects::table.on(projects::id.eq(environments::project_id)))
        .filter(environments::id.eq(env_id))
        .select((environments::project_id, projects::org_id))
        .first(conn)
        .await
        .optional()
}

/// Live catalogue entries only — the cap must not be consumed by retired rows.
pub async fn count_active_project_environments(
    conn: &mut AsyncPgConnection,
    project_id: Uuid,
) -> QueryResult<i64> {
    environments::table
        .filter(environments::project_id.eq(project_id))
        .filter(environments::retired_at.is_null())
        .count()
        .get_result(conn)
        .await
}

/// Take a project-level lock. Every mutation that reads the project's
/// environment-set invariants (how many are live, is anything still defaulting
/// to this one) and then writes must hold this, for the same reason
/// [`lock_app_for_update`] exists one level down.
pub async fn lock_project_for_update(
    conn: &mut AsyncPgConnection,
    project_id: Uuid,
) -> QueryResult<()> {
    projects::table
        .find(project_id)
        .select(projects::id)
        .for_update()
        .first::<Uuid>(conn)
        .await
        .map(|_| ())
}

pub async fn rename_project_environment(
    conn: &mut AsyncPgConnection,
    id: Uuid,
    name: &str,
) -> QueryResult<Environment> {
    diesel::update(environments::table.find(id))
        .set((
            environments::name.eq(name),
            environments::updated_at.eq(Utc::now()),
        ))
        .returning(Environment::as_returning())
        .get_result(conn)
        .await
}

/// Retire a catalogue entry and every enrollment in it, in one transaction.
///
/// Retire, never delete. The rows are kept so historical events — including any
/// already exported to cold Parquet, which no FK can reach — stay attributable.
/// Returns the public keys of the enrollments that were retired, so the caller
/// can invalidate their Redis DSN cache slots; a key whose row is retired must
/// stop resolving, and `find_env_by_public_key` filters `retired_at IS NULL`.
pub async fn retire_project_environment(
    conn: &mut AsyncPgConnection,
    id: Uuid,
) -> QueryResult<(Environment, Vec<String>)> {
    conn.transaction::<_, diesel::result::Error, _>(async |conn| {
        let now = Utc::now();
        let keys: Vec<String> = diesel::update(
            app_environments::table
                .filter(app_environments::environment_id.eq(id))
                .filter(app_environments::retired_at.is_null()),
        )
        .set((
            app_environments::retired_at.eq(Some(now)),
            app_environments::ingest_enabled.eq(false),
            // Clear the flag too. The caller refuses to retire an environment
            // that is still some app's default, so this is normally a no-op —
            // but leaving it set would put a retired row in an app's default
            // slot, which `promote_app_environment_default` can never move.
            app_environments::is_default.eq(false),
            app_environments::updated_at.eq(now),
        ))
        .returning(app_environments::public_key)
        .get_results(conn)
        .await?;

        let env = diesel::update(environments::table.find(id))
            .set((
                environments::retired_at.eq(Some(now)),
                environments::updated_at.eq(now),
            ))
            .returning(Environment::as_returning())
            .get_result(conn)
            .await?;

        Ok((env, keys))
    })
    .await
}

/// Ids of every app in a project — the fan-out target when an environment is
/// added to the catalogue. A freshly created environment has no enrollments at
/// all, so this needs no "missing" predicate.
pub async fn app_ids_in_project(
    conn: &mut AsyncPgConnection,
    project_id: Uuid,
) -> QueryResult<Vec<Uuid>> {
    apps::table
        .filter(apps::project_id.eq(project_id))
        .select(apps::id)
        .load(conn)
        .await
}

/// `(id, public_key)` of an app's live enrollments — for invalidating Redis DSN
/// cache slots when something app-wide changes.
///
/// Retired enrollments are excluded: they are already unresolvable
/// (`find_env_by_public_key` filters `retired_at IS NULL`), and excluding them
/// keeps them from consuming the row cap ahead of the live keys that matter.
pub async fn live_app_environment_keys(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
) -> QueryResult<Vec<(Uuid, String)>> {
    app_environments::table
        .filter(app_environments::app_id.eq(app_id))
        .filter(app_environments::retired_at.is_null())
        .select((app_environments::id, app_environments::public_key))
        .limit(MAX_ENVIRONMENTS_PER_PROJECT)
        .load(conn)
        .await
}

// --- app_environments: one app's enrollment in one environment ---------------

/// Batch-insert enrollments. Keys are minted by the caller rather than here so
/// that every key in the system comes from `ids::public_key()`; `sauron-db` has
/// no dependency on `sauron-core`.
pub async fn create_app_environments(
    conn: &mut AsyncPgConnection,
    rows: &[NewAppEnvironment<'_>],
) -> QueryResult<Vec<AppEnvironment>> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    diesel::insert_into(app_environments::table)
        .values(rows)
        .returning(AppEnvironment::as_returning())
        .get_results(conn)
        .await
}

/// An app's enrollments joined to their catalogue names.
///
/// A retired *catalogue* entry implies retired enrollments (they are retired in
/// the same transaction), so filtering on the enrollment's own `retired_at` is
/// sufficient and avoids a second predicate that could disagree with it.
pub async fn list_app_environments(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    include_retired: bool,
) -> QueryResult<Vec<AppEnvironmentView>> {
    let mut q = app_environments::table
        .inner_join(environments::table.on(environments::id.eq(app_environments::environment_id)))
        .filter(app_environments::app_id.eq(app_id))
        .into_boxed();
    if !include_retired {
        q = q.filter(app_environments::retired_at.is_null());
    }
    let rows: Vec<(AppEnvironment, String)> = q
        .select((AppEnvironment::as_select(), environments::name))
        .order(environments::name.asc())
        .limit(MAX_ENVIRONMENTS_PER_PROJECT)
        .load(conn)
        .await?;
    Ok(rows
        .into_iter()
        .map(|(enrollment, name)| AppEnvironmentView { enrollment, name })
        .collect())
}

pub async fn get_app_environment(
    conn: &mut AsyncPgConnection,
    id: Uuid,
) -> QueryResult<Option<AppEnvironment>> {
    app_environments::table
        .find(id)
        .select(AppEnvironment::as_select())
        .first(conn)
        .await
        .optional()
}

/// Apps that still treat `environment_id` as their default. A catalogue entry
/// with any of these cannot be retired — doing so would leave those apps with
/// nowhere to report by default.
pub async fn apps_defaulting_to_environment(
    conn: &mut AsyncPgConnection,
    environment_id: Uuid,
) -> QueryResult<i64> {
    app_environments::table
        .filter(app_environments::environment_id.eq(environment_id))
        .filter(app_environments::is_default.eq(true))
        .filter(app_environments::retired_at.is_null())
        .count()
        .get_result(conn)
        .await
}

pub async fn set_app_environment_ingest(
    conn: &mut AsyncPgConnection,
    id: Uuid,
    enabled: bool,
) -> QueryResult<AppEnvironment> {
    diesel::update(app_environments::table.find(id))
        .set((
            app_environments::ingest_enabled.eq(enabled),
            app_environments::updated_at.eq(Utc::now()),
        ))
        .returning(AppEnvironment::as_returning())
        .get_result(conn)
        .await
}

/// Take an app-level lock. Every mutation that reads an app's environment-set
/// invariants (how many are live, which is default) and then writes must hold this,
/// otherwise two such transactions on DIFFERENT rows of the same app never serialize:
/// each locks only its own environment row and both read a pre-commit count.
pub async fn lock_app_for_update(conn: &mut AsyncPgConnection, app_id: Uuid) -> QueryResult<()> {
    apps::table
        .find(app_id)
        .select(apps::id)
        .for_update()
        .first::<Uuid>(conn)
        .await
        .map(|_| ())
}

/// Move the default flag within an app. Both statements run in one transaction
/// because `app_environments_default_key` is a partial unique index on
/// `(app_id) WHERE is_default` — setting the new default before clearing the old
/// one violates it mid-statement.
pub async fn promote_app_environment_default(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    id: Uuid,
) -> QueryResult<AppEnvironment> {
    conn.transaction::<_, diesel::result::Error, _>(async |conn| {
        lock_app_for_update(conn, app_id).await?;
        diesel::update(app_environments::table)
            .filter(app_environments::app_id.eq(app_id))
            .filter(app_environments::is_default.eq(true))
            .set((
                app_environments::is_default.eq(false),
                app_environments::updated_at.eq(Utc::now()),
            ))
            .execute(conn)
            .await?;
        // `app_id` is re-asserted here rather than trusting `find(id)` alone: a caller
        // that authorized on app A but passed app B's enrollment id would otherwise
        // leave A with zero defaults and silently give B one.
        diesel::update(
            app_environments::table
                .find(id)
                .filter(app_environments::app_id.eq(app_id))
                // A retired enrollment can never become the default. Without this the
                // row lock alone is insufficient: a concurrent retire commits first, and
                // this UPDATE's WHERE still matches (retire changes neither id nor
                // app_id), flagging a retired row and leaving the app with zero live
                // defaults. The partial index cannot catch it — retired rows are not in it.
                .filter(app_environments::retired_at.is_null()),
        )
        .set((
            app_environments::is_default.eq(true),
            app_environments::updated_at.eq(Utc::now()),
        ))
        .returning(AppEnvironment::as_returning())
        .get_result(conn)
        .await
    })
    .await
}

/// Retire ONE app's enrollment. Retire, never delete: the row is kept so
/// historical events — including any already exported to cold Parquet, which no
/// FK can reach — stay attributable.
///
/// Deliberately NOT exposed over HTTP. Withdrawing a single app from an
/// environment would be a one-way door, because enrollment happens only when an
/// environment or an app is created; muting via `ingest_enabled` says the same
/// thing reversibly. The only production path that retires an enrollment is
/// [`retire_project_environment`]'s cascade, which retires all of them at once —
/// this single-row form is what lets that end state be constructed directly in
/// tests.
pub async fn retire_app_environment(
    conn: &mut AsyncPgConnection,
    id: Uuid,
) -> QueryResult<AppEnvironment> {
    let now = Utc::now();
    diesel::update(app_environments::table.find(id))
        .set((
            app_environments::retired_at.eq(Some(now)),
            app_environments::ingest_enabled.eq(false),
            // Clear the flag too. The retire handler refuses to retire a live default,
            // so this is normally a no-op — but leaving it set would make
            // `list_app_environments(include_retired = true)` return two rows flagged
            // default, and the settings UI would render two "Default" badges.
            app_environments::is_default.eq(false),
            app_environments::updated_at.eq(now),
        ))
        .returning(AppEnvironment::as_returning())
        .get_result(conn)
        .await
}

pub async fn rotate_app_environment_key(
    conn: &mut AsyncPgConnection,
    id: Uuid,
    new_key: &str,
) -> QueryResult<AppEnvironment> {
    diesel::update(app_environments::table.find(id))
        .set((
            app_environments::public_key.eq(new_key),
            app_environments::updated_at.eq(Utc::now()),
        ))
        .returning(AppEnvironment::as_returning())
        .get_result(conn)
        .await
}

/// Resolve an ingest key to its environment and full ancestry in one query.
/// Retired environments are excluded, so a retired key is indistinguishable from
/// an unknown one and falls through to the existing `invalid_key` path.
pub async fn find_env_by_public_key(
    conn: &mut AsyncPgConnection,
    public_key: &str,
) -> QueryResult<Option<EnvRef>> {
    app_environments::table
        .inner_join(apps::table.on(apps::id.eq(app_environments::app_id)))
        .inner_join(projects::table.on(projects::id.eq(apps::project_id)))
        .filter(app_environments::public_key.eq(public_key))
        .filter(app_environments::retired_at.is_null())
        .select((
            app_environments::id,
            apps::id,
            apps::project_id,
            projects::org_id,
            app_environments::ingest_enabled,
            apps::ingest_enabled,
        ))
        .first::<(Uuid, Uuid, Uuid, Uuid, bool, bool)>(conn)
        .await
        .optional()
        .map(|row| {
            row.map(
                |(env_id, app_id, project_id, org_id, env_ingest_enabled, app_ingest_enabled)| {
                    EnvRef {
                        env_id,
                        app_id,
                        project_id,
                        org_id,
                        env_ingest_enabled,
                        app_ingest_enabled,
                    }
                },
            )
        })
}

/// `(app_id, project_id, org_id)` ancestry of an environment — for permission
/// resolution, mirroring `app_ancestry`. Slice 3's `authorize_env` reuses this.
pub async fn env_ancestry(
    conn: &mut AsyncPgConnection,
    env_id: Uuid,
) -> QueryResult<Option<(Uuid, Uuid, Uuid)>> {
    app_environments::table
        .inner_join(apps::table.on(apps::id.eq(app_environments::app_id)))
        .inner_join(projects::table.on(projects::id.eq(apps::project_id)))
        .filter(app_environments::id.eq(env_id))
        .select((app_environments::app_id, apps::project_id, projects::org_id))
        .first(conn)
        .await
        .optional()
}

/// Every enrollment id of an app, **including retired ones**.
///
/// Retired enrollments are included deliberately: their history stays
/// readable (an app's data does not disappear because an environment was
/// retired), so a caller's readable subset must be able to contain one.
/// `list_app_environments` excludes them because they must not be *selectable*;
/// that is a different question from whether they are *readable*.
///
/// These are `app_environments` ids, which is what `role_grants.scope_id` holds
/// for `scope_type = 'env'` — an env grant names one app's enrollment, never the
/// catalogue entry, so that granting "staging" cannot silently span sibling apps.
pub async fn env_ids_for_app(conn: &mut AsyncPgConnection, app_id: Uuid) -> QueryResult<Vec<Uuid>> {
    app_environments::table
        .filter(app_environments::app_id.eq(app_id))
        .select(app_environments::id)
        .load(conn)
        .await
}

// ===========================================================================
// Issues & error events (app-scoped)
// ===========================================================================

pub async fn upsert_issue(conn: &mut AsyncPgConnection, new: NewIssue<'_>) -> QueryResult<Uuid> {
    diesel::insert_into(issues::table)
        .values(&new)
        .on_conflict((issues::app_id, issues::fingerprint))
        .do_update()
        .set((
            issues::last_seen.eq(excluded(issues::last_seen)),
            issues::times_seen.eq(issues::times_seen + 1),
            issues::level.eq(excluded(issues::level)),
            issues::title.eq(excluded(issues::title)),
            issues::culprit.eq(excluded(issues::culprit)),
            issues::updated_at.eq(Utc::now()),
            // Ingest-side watermark for the regression trigger. Set here and
            // nowhere else: keying regression off `last_seen` (client clock)
            // let a poll tick advance past a just-ingested event and drop the
            // alert, and keying it off `updated_at` would fire a bogus
            // "regressed" alert every time someone resolved an issue.
            issues::last_event_at.eq(Utc::now()),
        ))
        .returning(issues::id)
        .get_result(conn)
        .await
}

pub async fn insert_error_event(
    conn: &mut AsyncPgConnection,
    ev: NewErrorEvent,
) -> QueryResult<usize> {
    diesel::insert_into(error_events::table)
        .values(&ev)
        .execute(conn)
        .await
}

/// Raw-SQL row shape for [`list_issues`]/[`get_issue`]/[`top_issues`] under
/// `EnvFilter::One`/`Unattributed`, where `times_seen`/`users_seen`/
/// `first_seen`/`last_seen` come from a per-environment aggregate rather than
/// `issues`' own (app-wide) columns. A local `QueryableByName` struct rather
/// than widening the shared `Issue` model — `Issue` derives `Queryable`/
/// `Selectable` for its many other diesel-query-builder call sites, and this
/// file's convention for a raw-SQL-only row shape (`IssueStatsRow`,
/// `PersonRow`, `DeviceRow`) is a dedicated struct with an explicit
/// `sql_type` per field.
#[derive(Debug, QueryableByName)]
struct IssueRow {
    #[diesel(sql_type = SqlUuid)]
    id: Uuid,
    #[diesel(sql_type = SqlUuid)]
    app_id: Uuid,
    #[diesel(sql_type = Text)]
    fingerprint: String,
    #[diesel(sql_type = Text)]
    type_: String,
    #[diesel(sql_type = Text)]
    title: String,
    #[diesel(sql_type = Text)]
    culprit: String,
    #[diesel(sql_type = Text)]
    level: String,
    #[diesel(sql_type = Text)]
    status: String,
    #[diesel(sql_type = Timestamptz)]
    first_seen: DateTime<Utc>,
    #[diesel(sql_type = Timestamptz)]
    last_seen: DateTime<Utc>,
    #[diesel(sql_type = BigInt)]
    times_seen: i64,
    #[diesel(sql_type = BigInt)]
    users_seen: i64,
    #[diesel(sql_type = Nullable<SqlUuid>)]
    assignee_id: Option<Uuid>,
    #[diesel(sql_type = Timestamptz)]
    created_at: DateTime<Utc>,
    #[diesel(sql_type = Timestamptz)]
    updated_at: DateTime<Utc>,
    #[diesel(sql_type = Timestamptz)]
    last_event_at: DateTime<Utc>,
}

impl From<IssueRow> for Issue {
    fn from(r: IssueRow) -> Self {
        Issue {
            id: r.id,
            app_id: r.app_id,
            fingerprint: r.fingerprint,
            type_: r.type_,
            title: r.title,
            culprit: r.culprit,
            level: r.level,
            status: r.status,
            first_seen: r.first_seen,
            last_seen: r.last_seen,
            times_seen: r.times_seen,
            users_seen: r.users_seen,
            assignee_id: r.assignee_id,
            created_at: r.created_at,
            updated_at: r.updated_at,
            last_event_at: r.last_event_at,
        }
    }
}

/// Lists issues for an app, optionally scoped to one environment.
///
/// `issues` has no `environment_id` and — per Task 1's write-path measurement
/// — no `issue_environments` rollup either (see the design doc's "No new
/// table"). `EnvFilter::All` therefore reads `issues` directly: no join, no
/// subquery, the same query this function ran before Slice 2. That is the
/// path almost every request takes, and it must not regress.
///
/// `EnvFilter::One`/`Unattributed` cannot use `issues`' own `times_seen`/
/// `users_seen`/`first_seen`/`last_seen` — they are app-wide. So: page the
/// issues first (bounded by `limit`/`offset`, ordered by the issue's own
/// `last_seen`, the same order `All` uses — no derived value exists yet to
/// order by), then `JOIN LATERAL` each returned row against `error_events`
/// (`error_events_issue_env_idx` makes this an index scan) to derive the
/// four returned aggregate values.
///
/// That inner paging order is necessarily app-wide (see above), but the
/// **outer**, final `ORDER BY agg.last_seen DESC` is not — it runs after the
/// LATERAL, over at most one page of already-materialized rows, where the
/// derived, per-environment `last_seen` exists. Ordering the returned page by
/// `i.last_seen` (app-wide) instead would sort by a column the caller is not
/// even shown: an issue last seen in `env_b` an hour ago but in `env_a` a
/// month ago would outrank one last seen in `env_a` ten minutes ago, on a
/// page whose every displayed timestamp is `env_a`-scoped. Same shape as
/// `top_issues`' own fix (`ORDER BY agg.times_seen DESC`, below) — read that
/// function's doc comment for the identical reasoning applied to a different
/// column.
///
/// Membership — "does this issue actually belong to the selected
/// environment at all" — is enforced *twice*, deliberately:
/// 1. The paging subquery itself carries `AND EXISTS (SELECT 1 FROM
///    error_events m WHERE m.issue_id = issues.id{env predicate})`. Without
///    this, `LIMIT`/`OFFSET` page by the issue's *app-wide* `last_seen`
///    before membership is known at all — an issue whose only activity is
///    in a different environment can still consume a page slot ahead of a
///    genuine member, producing non-monotonic pages and even an empty first
///    page while a later page returns real rows. Reproduced against the real
///    dev app (`One(demo)`, `limit 5`): `offset 0` returned 0 rows, `offset
///    5` returned 2, `offset 10` returned 5 — see
///    `.superpowers/sdd/s2-task-9-report.md`'s "Critical findings fixed"
///    section for the fixed timings.
/// 2. The `JOIN LATERAL` carries `HAVING count(*) > 0`: an issue with zero
///    occurrences in the selected environment produces zero rows from the
///    LATERAL and is dropped by the inner join. Without it, an aggregate
///    with no `GROUP BY` always returns exactly one row (`count = 0`,
///    `min`/`max` = `NULL`) even when nothing matches, which would silently
///    turn the inner join into a no-op `LEFT JOIN` in every practical sense.
///
/// Do not "simplify" either check away: the seed's `issue_env_b_only`
/// (confined to `env_b` alone) exists specifically to catch a regression —
/// it must not appear at all under `One(env_a)`, regardless of which of the
/// two checks would otherwise have let it through.
///
/// The `tag`/free-text `q` `EXISTS` fragments below carry the identical
/// environment predicate (reusing the single `$3` env bind — see the
/// bind-layout comment further down for why it is allocated early enough for
/// them to reach it). Without it, a tag or payload match that exists only in
/// a *different* environment could surface an issue under a scope that
/// excludes it, or — worse — let a free-text `q` extract characters from
/// that other environment's `tags`/`contexts`/`extra`, exactly where PII and
/// secrets live, even though the row's own displayed counts stayed correctly
/// scoped. See `list_issues_tag_and_q_do_not_leak_across_environments` in
/// `env_scoping.rs` for the regression test.
///
/// `since` is pushed into the LATERAL's own `WHERE e.occurred_at >= $2`
/// rather than only checked against the result afterward — so under
/// `One`/`Unattributed`, the returned `times_seen`/`users_seen`/
/// `first_seen`/`last_seen` are counts *within the requested window*, not
/// lifetime, and will not match `issues.times_seen` (lifetime, incremented
/// at ingest) even for the same environment under `All`. Deliberate, not a
/// bug: a list already filtered to "seen in the last N days" showing
/// lifetime counts beside it would be incoherent, and windowing restores
/// partition pruning on `error_events` (time-partitioned; an unbounded scan
/// cannot prune) — measured on the real 210k-event dev app at `LIMIT 50`,
/// see the report section above for the before/after. The outer `WHERE
/// agg.last_seen >= $2` is now provably redundant given the pushed-in bound
/// (every row the LATERAL emits already has `occurred_at >= $2`, so its
/// `max(occurred_at)` does too) — kept anyway as a second, harmless check,
/// same "verify membership twice" philosophy as above. One consequence:
/// because a paged issue can still fail the LATERAL's own window/`HAVING`
/// (a genuine member with no occurrence inside `since` specifically), the
/// page can come back shorter than `limit` even when more genuinely-matching
/// issues exist past the current `OFFSET`. Accepted in exchange for never
/// aggregating more than one page of issues per request — the cost trade
/// this whole design exists to make (see the design doc).
///
/// Three further discrepancies, neither a bug:
/// 1. Per-environment `users_seen` is an exact `count(DISTINCT distinct_id)`
///    over `error_events`; the app-wide `issues.users_seen` is maintained
///    from a Redis HyperLogLog and is approximate. They will disagree
///    slightly — the per-environment number is the more accurate one.
/// 2. Per-environment counts cannot see tiered data: once `sauron-tier`
///    exports a partition older than `TIER_HOT_DAYS` to Parquet and drops it,
///    those occurrences leave `error_events`, so a per-environment count over
///    an older window under-reports. `issues.times_seen` does not, because it
///    was incremented at ingest — which is also why `All` keeps reading it
///    directly rather than switching to the same derivation.
/// 3. Per-environment counts are windowed by `since` (see above); app-wide
///    counts under `All` are not windowed the same way (`All`'s own `since`
///    filters which issues survive, via `issues.last_seen`, but the counts
///    it returns are still lifetime). A `One(env)` view and an `All` view of
///    the same request can therefore report different numbers for the same
///    issue even setting the first two discrepancies aside.
///
/// **Task 9: `title`/`culprit`/`level` are now derived per environment too**,
/// the same shape as the four aggregate values above. A second `LEFT JOIN
/// LATERAL` (`latest`) beside `agg` selects `title`/`culprit`/`level` from
/// the single newest `error_events` row in the selected environment
/// (`ORDER BY e.occurred_at DESC LIMIT 1`); the outer select list reads
/// `COALESCE(latest.title, i.title)` etc. `LEFT JOIN` + `COALESCE`, not an
/// inner join: a row written before migration 30 has `title`/`culprit =
/// NULL` (they were only added then, not backfilled), and must fall back to
/// the app-wide `issues` column rather than vanishing from the page. The
/// `latest` LATERAL's own `WHERE e.issue_id = i.id{env}` fragment reuses the
/// **same bound env value** (`env_bind_idx`, `$3`) `agg` already uses — no
/// new bind is allocated; see the bind-layout comment below, which this
/// doesn't change. `error_events.level` is `NOT NULL`, and — because
/// `agg`'s own `HAVING count(*) > 0` (or the paging subquery's membership
/// `EXISTS`) already guarantees at least one in-environment row exists —
/// `latest` always finds a row too; `COALESCE` here is purely for the
/// `title`/`culprit` legacy-NULL case, not a "no row" case.
///
/// **Task 9 also moved four filters onto those derived values.** `level`,
/// `culprit`, `times_seen`, `users_seen` used to sit inside the paging
/// subquery, compared against `issues`' stored (app-wide) columns, while the
/// row displayed the derived ones — so `?level=error` and the level shown on
/// the row could disagree, the same class of bug the tag/`q` `EXISTS`
/// fragments had before the S2 Task 10 review fixed *their* environment
/// leak. Those four now live in the **outer** query, compared against
/// `latest.level` / `latest.culprit` / `agg.times_seen` / `agg.users_seen`.
/// `status` and `type` stay in the subquery, comparing against `issues`'
/// own columns — they are genuinely app-wide attributes with no
/// per-environment meaning (`issue_stats` makes the identical call for
/// `status`), so there is no derived value to move them to. `tag`'s `EXISTS`
/// fragments are unaffected either way (already environment-scoped, not
/// stored-column-compared).
///
/// **Trade accepted, not overlooked:** moving those four filters out of the
/// subquery means the subquery can no longer pre-filter on them, so it now
/// pages a *wider* candidate set (bounded only by `app_id`/`status`/`type`/
/// `tag`/environment membership) before the outer query narrows it by
/// `level`/`culprit`/`times_seen`/`users_seen`. A page can therefore come
/// back with fewer than `limit` rows even when more genuinely-matching
/// issues exist past `offset` — OFFSET-based paging combined with one of
/// these four filters is no longer exact beyond the first page. Accepted
/// because the only real caller, `Issues.svelte`, always requests `limit:
/// 100` with no `offset` (see `dashboard/src/pages/Issues.svelte`'s `load()`)
/// — chosen deliberately, not left as an unnoticed regression; revisit if a
/// second caller ever pages past offset 0 with one of these filters set.
pub async fn list_issues(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    filters: &[ParsedFilter],
    q: Option<&str>,
    since: chrono::DateTime<chrono::Utc>,
    limit: i64,
    offset: i64,
) -> QueryResult<Vec<Issue>> {
    if matches!(scope.env, EnvFilter::All) {
        let mut query = issues::table
            .filter(issues::app_id.eq(scope.app_id))
            .filter(issues::last_seen.ge(since))
            .into_boxed();
        for f in filters {
            query = match (f.field, f.op) {
                ("level", Op::Eq) => query.filter(issues::level.eq(f.value.clone())),
                ("level", Op::Neq) => query.filter(issues::level.ne(f.value.clone())),
                ("status", Op::Eq) => query.filter(issues::status.eq(f.value.clone())),
                ("status", Op::Neq) => query.filter(issues::status.ne(f.value.clone())),
                ("type", Op::Eq) => query.filter(issues::type_.eq(f.value.clone())),
                ("type", Op::Neq) => query.filter(issues::type_.ne(f.value.clone())),
                ("type", Op::Contains) => {
                    query.filter(issues::type_.ilike(like_contains(&f.value)))
                }
                ("culprit", Op::Eq) => query.filter(issues::culprit.eq(f.value.clone())),
                ("culprit", Op::Neq) => query.filter(issues::culprit.ne(f.value.clone())),
                ("culprit", Op::Contains) => {
                    query.filter(issues::culprit.ilike(like_contains(&f.value)))
                }
                ("times_seen", Op::Eq) => query.filter(issues::times_seen.eq(as_i64(&f.value))),
                ("times_seen", Op::Gt) => query.filter(issues::times_seen.gt(as_i64(&f.value))),
                ("times_seen", Op::Lt) => query.filter(issues::times_seen.lt(as_i64(&f.value))),
                ("users_seen", Op::Eq) => query.filter(issues::users_seen.eq(as_i64(&f.value))),
                ("users_seen", Op::Gt) => query.filter(issues::users_seen.gt(as_i64(&f.value))),
                ("users_seen", Op::Lt) => query.filter(issues::users_seen.lt(as_i64(&f.value))),
                ("tag", Op::Eq) => {
                    let (k, v) = tag_kv(&f.value);
                    query.filter(
                        sql::<Bool>(
                            "EXISTS (SELECT 1 FROM error_events e \
                             WHERE e.issue_id = issues.id AND e.app_id = issues.app_id AND e.tags @> ",
                        )
                        .bind::<Jsonb, _>(tag_object(k, v))
                        .sql(")"),
                    )
                }
                ("tag", Op::Contains) => {
                    let (k, v) = tag_kv(&f.value);
                    query.filter(
                        sql::<Bool>(
                            "EXISTS (SELECT 1 FROM error_events e \
                             WHERE e.issue_id = issues.id AND e.app_id = issues.app_id AND e.tags ->> ",
                        )
                        .bind::<Text, _>(k)
                        .sql(" ILIKE ")
                        .bind::<Text, _>(like_contains(&v))
                        .sql(")"),
                    )
                }
                // `issues` itself carries no `workflow_name` column — narrow via
                // an EXISTS against the environment-stamped child row, same
                // idiom as `tag` just above.
                //
                // `e.workflow_id IS NOT NULL` is here for the partial-index
                // reason Task 4 measured and documented on `workflow_detail`'s
                // `top_events`/`top_issues` queries: migration
                // 2026-07-29-000032's `error_events_app_workflow_idx` is
                // PARTIAL (`WHERE workflow_id IS NOT NULL`), and Postgres uses
                // a partial index only when the query's WHERE *implies* the
                // index predicate — `workflow_name = $N` does not, they are
                // different columns. Semantically a no-op (the pipeline stamps
                // id and name together, so a row with a name always has an id).
                // Milder here than in `workflow_detail`, because this EXISTS is
                // correlated on `e.issue_id = issues.id` and so has
                // `error_events_issue_idx` as a bounded fallback — but the term
                // is free and is the only thing that lets the planner consider
                // the workflow index at all.
                ("workflow", Op::Eq) => query.filter(
                    sql::<Bool>(
                        "EXISTS (SELECT 1 FROM error_events e \
                         WHERE e.issue_id = issues.id AND e.app_id = issues.app_id \
                         AND e.workflow_id IS NOT NULL AND e.workflow_name = ",
                    )
                    .bind::<Text, _>(f.value.clone())
                    .sql(")"),
                ),
                // NOT EXISTS, so an issue whose occurrences are all unstamped
                // (no workflow at all) DOES match `neq` — "not part of workflow
                // X" is true of a row that is part of no workflow. See
                // `list_error_events_for_issue`'s `workflow` arms for the
                // occurrence-level predicate deliberately built to agree with
                // this, and why that one departs from its own file's
                // `session_id`/`release` precedent to do so.
                ("workflow", Op::Neq) => query.filter(
                    sql::<Bool>(
                        "NOT EXISTS (SELECT 1 FROM error_events e \
                         WHERE e.issue_id = issues.id AND e.app_id = issues.app_id \
                         AND e.workflow_id IS NOT NULL AND e.workflow_name = ",
                    )
                    .bind::<Text, _>(f.value.clone())
                    .sql(")"),
                ),
                ("workflow", Op::Contains) => query.filter(
                    sql::<Bool>(
                        "EXISTS (SELECT 1 FROM error_events e \
                         WHERE e.issue_id = issues.id AND e.app_id = issues.app_id \
                         AND e.workflow_id IS NOT NULL AND e.workflow_name ILIKE ",
                    )
                    .bind::<Text, _>(like_contains(&f.value))
                    .sql(")"),
                ),
                _ => query, // unreachable: Task 1 whitelists field+op
            };
        }
        if let Some(term) = q {
            let p = like_contains(term);
            query = query.filter(
                issues::title
                    .ilike(p.clone())
                    .or(issues::type_.ilike(p.clone()))
                    .or(issues::culprit.ilike(p.clone()))
                    // Payload search casts jsonb to text, which no index can serve.
                    // Bounding the correlated scan by time is what keeps it viable:
                    // without it, an issue with no match forces a full scan of that
                    // issue's entire event history — for EVERY issue in the app.
                    // `since` is always supplied by the caller (see `list()` in
                    // `routes/issues.rs`) — there used to be a `MAX_PAYLOAD_SEARCH_
                    // DAYS` fallback for when it wasn't, but every route already
                    // passed `Some(since)`, so that fallback never fired. Deleted
                    // rather than kept as a guard that reads as protection but
                    // isn't one.
                    .or(sql::<Bool>(
                        "EXISTS (SELECT 1 FROM error_events e \
                         WHERE e.issue_id = issues.id AND e.app_id = issues.app_id \
                         AND e.occurred_at >= ",
                    )
                    .bind::<Timestamptz, _>(since)
                    .sql(" AND (e.contexts::text ILIKE ")
                    .bind::<Text, _>(p.clone())
                    .sql(" OR e.extra::text ILIKE ")
                    .bind::<Text, _>(p.clone())
                    .sql(" OR e.tags::text ILIKE ")
                    .bind::<Text, _>(p)
                    .sql("))")),
            );
        }
        return query
            .select(Issue::as_select())
            .order(issues::last_seen.desc())
            .limit(limit)
            .offset(offset)
            .load(conn)
            .await;
    }

    // ----- One / Unattributed: page first, aggregate via an inner-join LATERAL -----
    //
    // Bind layout: $1 app_id, $2 since. $3 is `env`, allocated *before* the
    // filter loop — unlike every other raw-SQL function in this file, where
    // env is last — because the tag/q `EXISTS` fragments and the paging
    // subquery's own membership `EXISTS` all need to reference the same
    // bound value too, alongside the LATERAL's own; one bind reused
    // everywhere it's needed, same idiom as reusing `$2` for `since`. Under
    // `One`/`Subset`, $3 is bound (`scope.env.consumes_bind()` is `true`) and filters
    // start at $4; under `Unattributed`, $3 is never referenced in the SQL
    // text at all (a literal `IS NULL` needs no bind) and no bind is pushed
    // for it, so filters start at $3 instead — `next_bind`'s initial value
    // is computed from whether `env` actually consumed a bind, specifically
    // so the two cases can never disagree about which placeholder is next.
    // Filters/tag/`q` consume the following numbers dynamically, one bind
    // per distinct value — a value referenced several times in the text
    // reuses its one placeholder, same idiom as `list_persons`' `$5`.
    // limit/offset follow last. Placeholders can appear out of numeric order
    // in the SQL text itself (`$3`'s env fragment sits inside the LATERAL,
    // textually after `$4`'s filter fragment in the paging subquery) —
    // Postgres only requires that the *n*th `.bind()` call supply `$n`, not
    // that `$n` appear before `$n+1` in the text.
    // Every filter/tag/q fragment below is textually inside `SELECT * FROM
    // issues WHERE app_id = $1{filter_sql}` — the *inner* paging subquery,
    // one nesting level below where the `i` alias applies (that alias names
    // the subquery's own *result*, not any scope visible inside it; see
    // `list_persons`' doc comment for the identical situation). So these use
    // bare column names / the literal table name `issues`, never `i.`.
    let env_bind_idx = 3usize;
    let mut next_bind = if scope.env.consumes_bind() {
        4usize
    } else {
        3usize
    };
    let env_sql = scope.env.sql_fragment_for("e", env_bind_idx);
    let member_env_sql = scope.env.sql_fragment_for("m", env_bind_idx);
    // Task 9: split in two. `filter_sql` stays textually inside the paging
    // subquery (`status`/`type`/`tag` — genuinely app-wide columns, or
    // already-environment-scoped `EXISTS`es); `outer_filter_sql` moves to
    // the outer query, after the `agg`/`latest` LATERALs exist to be
    // compared against (`level`/`culprit`/`times_seen`/`users_seen` — see
    // this function's doc comment for why). Both still consume `next_bind`
    // from the same single counter, in the same `filters` iteration order,
    // as before — only *where* each fragment's text lands changed, not the
    // bind numbering, so the bind loop below (which binds by `f.field`/
    // `f.op`, not by which string it went into) needs no change.
    let mut filter_sql = String::new();
    let mut outer_filter_sql = String::new();
    for f in filters {
        match (f.field, f.op) {
            ("level", Op::Eq) => {
                outer_filter_sql += &format!(" AND latest.level = ${next_bind}");
                next_bind += 1;
            }
            ("level", Op::Neq) => {
                outer_filter_sql += &format!(" AND latest.level <> ${next_bind}");
                next_bind += 1;
            }
            ("status", Op::Eq) => {
                filter_sql += &format!(" AND status = ${next_bind}");
                next_bind += 1;
            }
            ("status", Op::Neq) => {
                filter_sql += &format!(" AND status <> ${next_bind}");
                next_bind += 1;
            }
            ("type", Op::Eq) => {
                filter_sql += &format!(" AND type = ${next_bind}");
                next_bind += 1;
            }
            ("type", Op::Neq) => {
                filter_sql += &format!(" AND type <> ${next_bind}");
                next_bind += 1;
            }
            ("type", Op::Contains) => {
                filter_sql += &format!(" AND type ILIKE ${next_bind}");
                next_bind += 1;
            }
            ("culprit", Op::Eq) => {
                outer_filter_sql += &format!(" AND latest.culprit = ${next_bind}");
                next_bind += 1;
            }
            ("culprit", Op::Neq) => {
                outer_filter_sql += &format!(" AND latest.culprit <> ${next_bind}");
                next_bind += 1;
            }
            ("culprit", Op::Contains) => {
                outer_filter_sql += &format!(" AND latest.culprit ILIKE ${next_bind}");
                next_bind += 1;
            }
            ("times_seen", Op::Eq) => {
                outer_filter_sql += &format!(" AND agg.times_seen = ${next_bind}");
                next_bind += 1;
            }
            ("times_seen", Op::Gt) => {
                outer_filter_sql += &format!(" AND agg.times_seen > ${next_bind}");
                next_bind += 1;
            }
            ("times_seen", Op::Lt) => {
                outer_filter_sql += &format!(" AND agg.times_seen < ${next_bind}");
                next_bind += 1;
            }
            ("users_seen", Op::Eq) => {
                outer_filter_sql += &format!(" AND agg.users_seen = ${next_bind}");
                next_bind += 1;
            }
            ("users_seen", Op::Gt) => {
                outer_filter_sql += &format!(" AND agg.users_seen > ${next_bind}");
                next_bind += 1;
            }
            ("users_seen", Op::Lt) => {
                outer_filter_sql += &format!(" AND agg.users_seen < ${next_bind}");
                next_bind += 1;
            }
            ("tag", Op::Eq) => {
                let te_env = scope.env.sql_fragment_for("te", env_bind_idx);
                filter_sql += &format!(
                    " AND EXISTS (SELECT 1 FROM error_events te WHERE te.issue_id = issues.id \
                      AND te.app_id = issues.app_id AND te.tags @> ${next_bind}{te_env})"
                );
                next_bind += 1;
            }
            ("tag", Op::Contains) => {
                let te_env = scope.env.sql_fragment_for("te", env_bind_idx);
                filter_sql += &format!(
                    " AND EXISTS (SELECT 1 FROM error_events te WHERE te.issue_id = issues.id \
                      AND te.app_id = issues.app_id AND te.tags ->> ${a} ILIKE ${b}{te_env})",
                    a = next_bind,
                    b = next_bind + 1
                );
                next_bind += 2;
            }
            // `issues` itself carries no `workflow_name` column — same EXISTS
            // idiom as `tag` just above, against a distinct alias (`we`) so it
            // cannot collide with a `tag`/`q` EXISTS in the same query text.
            // `we.workflow_id IS NOT NULL` is the partial-index term; see the
            // `EnvFilter::All` branch's `workflow` arms above for the full
            // reasoning, and for why `Neq` is `NOT EXISTS` (unstamped rows
            // match) rather than a `<>` that would drop them.
            ("workflow", Op::Eq) => {
                let we_env = scope.env.sql_fragment_for("we", env_bind_idx);
                filter_sql += &format!(
                    " AND EXISTS (SELECT 1 FROM error_events we WHERE we.issue_id = issues.id \
                      AND we.app_id = issues.app_id AND we.workflow_id IS NOT NULL \
                      AND we.workflow_name = ${next_bind}{we_env})"
                );
                next_bind += 1;
            }
            ("workflow", Op::Neq) => {
                let we_env = scope.env.sql_fragment_for("we", env_bind_idx);
                filter_sql += &format!(
                    " AND NOT EXISTS (SELECT 1 FROM error_events we WHERE we.issue_id = issues.id \
                      AND we.app_id = issues.app_id AND we.workflow_id IS NOT NULL \
                      AND we.workflow_name = ${next_bind}{we_env})"
                );
                next_bind += 1;
            }
            ("workflow", Op::Contains) => {
                let we_env = scope.env.sql_fragment_for("we", env_bind_idx);
                filter_sql += &format!(
                    " AND EXISTS (SELECT 1 FROM error_events we WHERE we.issue_id = issues.id \
                      AND we.app_id = issues.app_id AND we.workflow_id IS NOT NULL \
                      AND we.workflow_name ILIKE ${next_bind}{we_env})"
                );
                next_bind += 1;
            }
            _ => {} // unreachable: Task 1 whitelists field+op
        }
    }
    let q_bind = q.map(|_| {
        let b = next_bind;
        next_bind += 1;
        b
    });
    if let Some(b) = q_bind {
        let qe_env = scope.env.sql_fragment_for("qe", env_bind_idx);
        filter_sql += &format!(
            " AND (title ILIKE ${b} OR type ILIKE ${b} OR culprit ILIKE ${b} \
              OR EXISTS (SELECT 1 FROM error_events qe WHERE qe.issue_id = issues.id \
              AND qe.app_id = issues.app_id AND qe.occurred_at >= $2 \
              AND (qe.contexts::text ILIKE ${b} OR qe.extra::text ILIKE ${b} OR qe.tags::text ILIKE ${b}){qe_env}))"
        );
    }
    let limit_bind = next_bind;
    next_bind += 1;
    let offset_bind = next_bind;

    let sql_text = format!(
        "SELECT i.id, i.app_id, i.fingerprint, i.type AS type_, \
                COALESCE(latest.title, i.title)     AS title, \
                COALESCE(latest.culprit, i.culprit) AS culprit, \
                COALESCE(latest.level, i.level)     AS level, \
                i.status, \
                agg.first_seen, agg.last_seen, agg.times_seen, agg.users_seen, \
                i.assignee_id, i.created_at, i.updated_at, i.last_event_at \
         FROM ( \
             SELECT * FROM issues \
             WHERE app_id = $1{filter_sql} \
               AND EXISTS (SELECT 1 FROM error_events m WHERE m.issue_id = issues.id{member_env_sql}) \
             ORDER BY last_seen DESC \
             LIMIT ${limit_bind} OFFSET ${offset_bind} \
         ) i \
         JOIN LATERAL ( \
             SELECT count(*)::bigint AS times_seen, \
                    count(DISTINCT distinct_id)::bigint AS users_seen, \
                    min(occurred_at) AS first_seen, \
                    max(occurred_at) AS last_seen \
             FROM error_events e \
             WHERE e.issue_id = i.id AND e.occurred_at >= $2{env_sql} \
             HAVING count(*) > 0 \
         ) agg ON TRUE \
         LEFT JOIN LATERAL ( \
             SELECT e.title, e.culprit, e.level \
             FROM error_events e \
             WHERE e.issue_id = i.id{env_sql} \
             ORDER BY e.occurred_at DESC \
             LIMIT 1 \
         ) latest ON TRUE \
         WHERE agg.last_seen >= $2{outer_filter_sql} \
         ORDER BY agg.last_seen DESC"
    );

    let mut stmt = diesel::sql_query(sql_text)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Timestamptz, _>(since);
    stmt = crate::bind_env!(stmt, &scope.env);
    for f in filters {
        stmt = match (f.field, f.op) {
            ("level", Op::Eq)
            | ("level", Op::Neq)
            | ("status", Op::Eq)
            | ("status", Op::Neq)
            | ("type", Op::Eq)
            | ("type", Op::Neq)
            | ("culprit", Op::Eq)
            | ("culprit", Op::Neq) => stmt.bind::<Text, _>(f.value.clone()),
            ("type", Op::Contains) | ("culprit", Op::Contains) => {
                stmt.bind::<Text, _>(like_contains(&f.value))
            }
            ("times_seen", Op::Eq)
            | ("times_seen", Op::Gt)
            | ("times_seen", Op::Lt)
            | ("users_seen", Op::Eq)
            | ("users_seen", Op::Gt)
            | ("users_seen", Op::Lt) => stmt.bind::<BigInt, _>(as_i64(&f.value)),
            ("tag", Op::Eq) => {
                let (k, v) = tag_kv(&f.value);
                stmt.bind::<Jsonb, _>(tag_object(k, v))
            }
            ("tag", Op::Contains) => {
                let (k, v) = tag_kv(&f.value);
                stmt.bind::<Text, _>(k).bind::<Text, _>(like_contains(&v))
            }
            ("workflow", Op::Eq) | ("workflow", Op::Neq) => stmt.bind::<Text, _>(f.value.clone()),
            ("workflow", Op::Contains) => stmt.bind::<Text, _>(like_contains(&f.value)),
            _ => stmt,
        };
    }
    if let Some(term) = q {
        stmt = stmt.bind::<Text, _>(like_contains(term));
    }
    stmt = stmt.bind::<BigInt, _>(limit).bind::<BigInt, _>(offset);

    let rows: Vec<IssueRow> = stmt.get_results(conn).await?;
    Ok(rows.into_iter().map(Issue::from).collect())
}

/// Single-issue lookup. `EnvFilter::All` reads `issues` directly (unchanged);
/// `One`/`Unattributed` reuse [`list_issues`]' derivation (inner-join
/// LATERAL, `HAVING count(*) > 0` for membership) as a single-row query, with
/// no `since`/paging concern — mirrors `get_device`'s precedent. Out-of-scope
/// (issue doesn't exist, or has no occurrence in the selected environment)
/// returns `None` either way, so a caller cannot distinguish "wrong id" from
/// "not in this environment" — the same non-disclosure `get_device`/
/// `get_event_user` chose.
///
/// Task 9: also reuses [`list_issues`]' `title`/`culprit`/`level` derivation
/// — a second `LEFT JOIN LATERAL` (`latest`), `COALESCE`d against `issues`'
/// own columns, reusing the identical `$3` env bind `agg` already consumes.
/// See `list_issues`' doc comment for the full reasoning (legacy-NULL
/// fallback, why `LEFT JOIN` not inner).
pub async fn get_issue(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    issue_id: Uuid,
) -> QueryResult<Option<Issue>> {
    if matches!(scope.env, EnvFilter::All) {
        return issues::table
            .filter(issues::app_id.eq(scope.app_id))
            .filter(issues::id.eq(issue_id))
            .select(Issue::as_select())
            .first(conn)
            .await
            .optional();
    }

    let env_sql = scope.env.sql_fragment_for("e", 3);
    let sql_text = format!(
        "SELECT i.id, i.app_id, i.fingerprint, i.type AS type_, \
                COALESCE(latest.title, i.title)     AS title, \
                COALESCE(latest.culprit, i.culprit) AS culprit, \
                COALESCE(latest.level, i.level)     AS level, \
                i.status, \
                agg.first_seen, agg.last_seen, agg.times_seen, agg.users_seen, \
                i.assignee_id, i.created_at, i.updated_at, i.last_event_at \
         FROM ( \
             SELECT * FROM issues WHERE app_id = $1 AND id = $2 \
         ) i \
         JOIN LATERAL ( \
             SELECT count(*)::bigint AS times_seen, \
                    count(DISTINCT distinct_id)::bigint AS users_seen, \
                    min(occurred_at) AS first_seen, \
                    max(occurred_at) AS last_seen \
             FROM error_events e \
             WHERE e.issue_id = i.id{env_sql} \
             HAVING count(*) > 0 \
         ) agg ON TRUE \
         LEFT JOIN LATERAL ( \
             SELECT e.title, e.culprit, e.level \
             FROM error_events e \
             WHERE e.issue_id = i.id{env_sql} \
             ORDER BY e.occurred_at DESC \
             LIMIT 1 \
         ) latest ON TRUE"
    );
    let mut stmt = diesel::sql_query(sql_text)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<SqlUuid, _>(issue_id);
    stmt = crate::bind_env!(stmt, &scope.env);
    let row: Option<IssueRow> = stmt.get_result(conn).await.optional()?;
    Ok(row.map(Issue::from))
}

pub async fn update_issue_status(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    issue_id: Uuid,
    status: &str,
) -> QueryResult<Option<Issue>> {
    diesel::update(
        issues::table
            .filter(issues::app_id.eq(app_id))
            .filter(issues::id.eq(issue_id)),
    )
    .set((
        issues::status.eq(status.to_string()),
        issues::updated_at.eq(Utc::now()),
    ))
    .returning(Issue::as_returning())
    .get_result(conn)
    .await
    .optional()
}

pub async fn set_issue_users_seen(
    conn: &mut AsyncPgConnection,
    issue_id: Uuid,
    count: i64,
) -> QueryResult<usize> {
    diesel::update(issues::table.find(issue_id))
        .set(issues::users_seen.eq(count))
        .execute(conn)
        .await
}

/// `error_events` carries its own `environment_id` directly, so this is an
/// ordinary `scope_env!` filter — unlike `list_issues`, which has to derive
/// membership because `issues` itself carries none. Also filters on
/// `scope.app_id` as defense in depth: every caller already resolves
/// `issue_id` through `get_issue(scope, ...)` first, so this is redundant in
/// practice, but matches the rest of this slice's idiom of never trusting an
/// id alone to imply tenant scope.
pub async fn list_error_events_for_issue(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    issue_id: Uuid,
    filters: &[ParsedFilter],
    q: Option<&str>,
    since: Option<chrono::DateTime<chrono::Utc>>,
    limit: i64,
) -> QueryResult<Vec<ErrorEvent>> {
    error_events_for_issue_query(&scope, issue_id, filters, q, since)
        .select(ErrorEvent::as_select())
        .order(error_events::occurred_at.desc())
        .limit(limit)
        .load(conn)
        .await
}

/// The shared `WHERE` clause behind both the occurrences list and its
/// [`error_event_stats_for_issue`] counts, returned unselected and unordered so
/// each caller can append its own projection.
///
/// Extracted so the two CANNOT drift. The stat strip above the occurrences
/// table claims to describe the rows below it; if these predicates were written
/// twice, any future filter added to one and forgotten in the other would show
/// a user count that silently disagrees with the visible rows — and the
/// `workflow`/`Neq` arm below is exactly the kind of subtlety a second copy
/// would get wrong.
fn error_events_for_issue_query<'a>(
    scope: &'a ReadScope,
    issue_id: Uuid,
    filters: &[ParsedFilter],
    q: Option<&str>,
    since: Option<chrono::DateTime<chrono::Utc>>,
) -> error_events::BoxedQuery<'a, diesel::pg::Pg> {
    let mut query = error_events::table
        .filter(error_events::app_id.eq(scope.app_id))
        .filter(error_events::issue_id.eq(issue_id))
        .into_boxed();
    if let Some(s) = since {
        query = query.filter(error_events::occurred_at.ge(s));
    }
    for f in filters {
        query = match (f.field, f.op) {
            ("tag", Op::Eq) => {
                let (k, v) = tag_kv(&f.value);
                query
                    .filter(sql::<Bool>("error_events.tags @> ").bind::<Jsonb, _>(tag_object(k, v)))
            }
            ("tag", Op::Contains) => {
                let (k, v) = tag_kv(&f.value);
                query.filter(
                    sql::<Bool>("error_events.tags ->> ")
                        .bind::<Text, _>(k)
                        .sql(" ILIKE ")
                        .bind::<Text, _>(like_contains(&v)),
                )
            }
            // Unlike `list_issues`, `error_events` carries `workflow_name`
            // directly — an ordinary column predicate, no EXISTS needed.
            ("workflow", Op::Eq) => query.filter(error_events::workflow_name.eq(f.value.clone())),
            // `OR workflow_name IS NULL` — **deliberately NOT the plain `.ne()`
            // this file uses for `Neq` on every other nullable column.**
            //
            // SQL's three-valued logic makes a bare `workflow_name <> 'x'`
            // drop every unstamped row, because `NULL <> 'x'` is NULL, not
            // true. For `session_id`/`release`/`distinct_id` in
            // `list_analytics_events` that is tolerable: each is a
            // single-level filter, so whatever it means, it means one thing.
            //
            // `workflow` is not single-level — the same chip is offered on
            // Issues (via `list_issues`' `NOT EXISTS`, where an issue with no
            // stamped occurrence DOES match `neq`) and here on that issue's
            // occurrences. A bare `<>` here would make one chip mean two
            // opposite things at two levels of the same drill-down: filter
            // Issues by `workflow:neq:checkout`, see your unattributed issues,
            // open one, and every unattributed occurrence silently vanishes.
            // A filter key must mean one thing wherever the user types it, so
            // this level bends to match the issue level rather than to match
            // its own file's precedent.
            ("workflow", Op::Neq) => query.filter(
                error_events::workflow_name
                    .ne(f.value.clone())
                    .or(error_events::workflow_name.is_null()),
            ),
            ("workflow", Op::Contains) => {
                query.filter(error_events::workflow_name.ilike(like_contains(&f.value)))
            }
            _ => query,
        };
    }
    if let Some(term) = q {
        let p = like_contains(term);
        query = query.filter(
            error_events::message
                .ilike(p.clone())
                .or(error_events::exception_value.ilike(p.clone()))
                .or(error_events::exception_type.ilike(p.clone()))
                .or(sql::<Bool>("error_events.contexts::text ILIKE ").bind::<Text, _>(p.clone()))
                .or(sql::<Bool>("error_events.extra::text ILIKE ").bind::<Text, _>(p.clone()))
                .or(sql::<Bool>("error_events.tags::text ILIKE ").bind::<Text, _>(p)),
        );
    }
    crate::scope_env!(query, error_events, &scope.env)
}

/// Occurrence totals for one issue over the same predicate the list uses.
///
/// HOT-TIER ONLY. `count(DISTINCT …)` is holistic, not additive, so it cannot
/// be split at the tier watermark and summed the way `tier_read.rs` merges
/// per-day counts — the same reason transaction percentiles stay on Postgres.
/// Once partitions age out to Parquet, a wide range under-reports here exactly
/// as it already does for the per-environment `users_seen` in `list_issues`.
pub async fn error_event_stats_for_issue(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    issue_id: Uuid,
    filters: &[ParsedFilter],
    q: Option<&str>,
    since: Option<chrono::DateTime<chrono::Utc>>,
) -> QueryResult<IssueEventStatsRow> {
    // `distinct_id`/`session_id` are both nullable: an anonymous occurrence and
    // a session-less one contribute a NULL, which `count(DISTINCT …)` skips.
    // That is the intent — "3 users" should not count "no user" as a user.
    let (events, users, sessions) =
        error_events_for_issue_query(&scope, issue_id, filters, q, since)
            .select((
                diesel::dsl::count_star(),
                sql::<BigInt>("count(DISTINCT error_events.distinct_id)"),
                sql::<BigInt>("count(DISTINCT error_events.session_id)"),
            ))
            .get_result::<(i64, i64, i64)>(conn)
            .await?;
    Ok(IssueEventStatsRow {
        events,
        users,
        sessions,
    })
}

/// Counts behind the occurrences stat strip. Raw-shape row, so it lives here
/// beside `IssueStatsRow` rather than in `models.rs`.
#[derive(Debug, serde::Serialize)]
pub struct IssueEventStatsRow {
    pub events: i64,
    pub users: i64,
    pub sessions: i64,
}

/// Reads `error_events` filtered only by `issue_id` — no `app_id: Uuid`
/// parameter for a text-based sweep to catch. Easy to miss for exactly that
/// reason: left unscoped, an issue detail page scoped to one environment
/// renders another environment's stack trace, release string and device
/// context with no error and no marker. `error_events` carries its own
/// `environment_id`, so this is an ordinary `scope_env!` filter, same
/// reasoning as [`list_error_events_for_issue`].
pub async fn latest_error_event(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    issue_id: Uuid,
) -> QueryResult<Option<ErrorEvent>> {
    let query = error_events::table
        .filter(error_events::app_id.eq(scope.app_id))
        .filter(error_events::issue_id.eq(issue_id))
        .into_boxed();
    crate::scope_env!(query, error_events, &scope.env)
        .select(ErrorEvent::as_select())
        .order(error_events::occurred_at.desc())
        .first(conn)
        .await
        .optional()
}

/// `error_events` carries its own `environment_id` directly, so this is an
/// ordinary `scope_env!` filter — unlike `get_event_user`, which has to derive
/// membership because `event_users` itself carries none.
pub async fn error_events_for_person(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    distinct_id: &str,
    limit: i64,
) -> QueryResult<Vec<ErrorEvent>> {
    let q = error_events::table
        .filter(error_events::app_id.eq(scope.app_id))
        .filter(error_events::distinct_id.eq(distinct_id))
        .into_boxed();
    crate::scope_env!(q, error_events, &scope.env)
        .select(ErrorEvent::as_select())
        .order(error_events::occurred_at.desc())
        .limit(limit)
        .load(conn)
        .await
}

// ===========================================================================
// Analytics events & people (app-scoped)
// ===========================================================================

use crate::filter::{Op, ParsedFilter};

/// Escape Postgres ILIKE wildcards (`\`, `%`, `_`) in a user-supplied value so
/// `contains`/free-text search matches it literally, then wrap it in `%…%`.
/// Postgres' default LIKE/ILIKE escape character is `\`.
fn escape_like(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for c in v.chars() {
        if matches!(c, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

pub fn like_contains(v: &str) -> String {
    format!("%{}%", escape_like(v))
}
fn as_i64(v: &str) -> i64 {
    v.parse().unwrap_or_default()
} // parser guarantees numeric

#[cfg(test)]
mod like_contains_tests {
    use super::like_contains;

    #[test]
    fn escapes_percent_wildcard() {
        assert_eq!(like_contains("50%"), "%50\\%%");
    }

    #[test]
    fn escapes_underscore_wildcard() {
        assert_eq!(like_contains("a_b"), "%a\\_b%");
    }

    #[test]
    fn escapes_backslash() {
        assert_eq!(like_contains("a\\b"), "%a\\\\b%");
    }

    #[test]
    fn passes_through_plain_value() {
        assert_eq!(like_contains("hello"), "%hello%");
    }
}

pub async fn insert_analytics_event(
    conn: &mut AsyncPgConnection,
    ev: NewAnalyticsEvent,
) -> QueryResult<usize> {
    diesel::insert_into(analytics_events::table)
        .values(&ev)
        .execute(conn)
        .await
}

pub async fn upsert_event_user(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    distinct_id: &str,
    traits: &Value,
) -> QueryResult<usize> {
    diesel::sql_query(
        "INSERT INTO event_users (app_id, distinct_id, properties) \
         VALUES ($1, $2, $3) \
         ON CONFLICT (app_id, distinct_id) \
         DO UPDATE SET properties = event_users.properties || EXCLUDED.properties, \
                       last_seen = now(), updated_at = now()",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Text, _>(distinct_id)
    .bind::<diesel::sql_types::Jsonb, _>(traits.clone())
    .execute(conn)
    .await
}

pub async fn touch_event_user(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    distinct_id: &str,
) -> QueryResult<usize> {
    diesel::sql_query(
        "INSERT INTO event_users (app_id, distinct_id) VALUES ($1, $2) \
         ON CONFLICT (app_id, distinct_id) DO UPDATE SET last_seen = now(), updated_at = now()",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Text, _>(distinct_id)
    .execute(conn)
    .await
}

pub async fn insert_identity(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    alias_id: &str,
    distinct_id: &str,
) -> QueryResult<usize> {
    diesel::insert_into(identities::table)
        .values((
            identities::app_id.eq(app_id),
            identities::alias_id.eq(alias_id),
            identities::distinct_id.eq(distinct_id),
        ))
        .on_conflict((identities::app_id, identities::alias_id))
        .do_nothing()
        .execute(conn)
        .await
}

/// `event_users` carries no `environment_id`, so membership in a specific
/// environment is derived the same way [`list_persons`]' membership `EXISTS`
/// derives it — activity in `analytics_events`/`error_events`/`sessions`, any
/// one of which is enough. Omitted under `All`, same reasoning as
/// `list_persons`: every `event_users` row exists only because some event
/// registered it, so an unfiltered `EXISTS` would add three subquery lookups
/// for no narrowing effect.
///
/// Returns [`PersonRow`], not the raw [`EventUser`] model — the same move F4
/// made for [`list_persons`]/[`list_devices`]/[`get_device`], and for the
/// identical reason: this is the Person Profile page's single-identity
/// counterpart to `list_persons`' paged rows, and `first_seen`/`last_seen`
/// need a different source depending on `scope.env` (the durable
/// `event_users` columns under `All`, an environment-scoped
/// `LEAST`/`GREATEST` LATERAL under `One`/`Unattributed` — see
/// `list_persons`' doc comment for the full derivation, mirrored here
/// verbatim). `EventUser` has no way to carry two different answers for the
/// same field depending on scope, and raw SQL is what lets a single query
/// switch a selected column's source per branch the way the diesel query
/// builder cannot.
///
/// Before this change the Person Profile page
/// (`bins/sauron-api/src/routes/analytics.rs`'s `PersonProfile`) rendered
/// `EventUser`'s raw, cross-environment, all-time `first_seen`/`last_seen`
/// directly beside an events/errors list that Task 8 already scoped — a
/// person viewed under `One(staging)` would show a production-derived "first
/// seen a year ago" above a list containing one day of staging activity.
/// That is the bug this function exists to not have.
///
/// `events_count`/`errors_count`/`sessions_count` ride along because they're
/// `PersonRow`'s other fields, computed by the same LATERALs regardless of
/// scope (no durable-column fast path, same as `list_persons` — see that
/// function's doc comment). `properties` is read straight off `event_users`
/// unconditionally, same non-derivation and same reasoning as `list_persons`'
/// own `properties` field.
pub async fn get_event_user(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    distinct_id: &str,
) -> QueryResult<Option<PersonRow>> {
    let env_sql = scope.env.sql_fragment(3);

    // See `list_persons`' membership `EXISTS` doc comment: each leg is
    // aliased and the correlated column qualified with that alias
    // (`ae.distinct_id`, not bare `distinct_id`) — an unqualified name
    // colliding with the outer `event_users` row would silently bind to the
    // outer table instead of failing, turning the predicate into a
    // tautology rather than a hard query error.
    let membership_sql = if matches!(scope.env, EnvFilter::All) {
        String::new()
    } else {
        let ae_env = scope.env.sql_fragment_for("ae", 3);
        let ee_env = scope.env.sql_fragment_for("ee", 3);
        let se_env = scope.env.sql_fragment_for("se", 3);
        format!(
            " AND ( \
                EXISTS (SELECT 1 FROM analytics_events ae WHERE ae.app_id=$1 AND ae.distinct_id = event_users.distinct_id{ae_env}) \
                OR EXISTS (SELECT 1 FROM error_events ee WHERE ee.app_id=$1 AND ee.distinct_id = event_users.distinct_id{ee_env}) \
                OR EXISTS (SELECT 1 FROM sessions se WHERE se.app_id=$1 AND se.distinct_id = event_users.distinct_id{se_env}) \
              )"
        )
    };

    // Same `All`-vs-scoped split as `list_persons` for `first_seen`/`last_seen`
    // — see that function's doc comment for the full reasoning, including why
    // `LEAST`/`GREATEST` skipping `NULL` legs is safe given membership already
    // guarantees at least one leg is non-null.
    let seen_select = if matches!(scope.env, EnvFilter::All) {
        "eu.first_seen AS first_seen, eu.last_seen AS last_seen".to_string()
    } else {
        "LEAST(ae.min_occurred, ee.min_occurred, se.min_started) AS first_seen, \
         GREATEST(ae.max_occurred, ee.max_occurred, se.max_last_event) AS last_seen"
            .to_string()
    };

    let q = format!(
        "SELECT eu.distinct_id, eu.properties, {seen_select}, \
                COALESCE(ae.cnt,0)::bigint AS events_count, \
                COALESCE(ee.cnt,0)::bigint AS errors_count, \
                COALESCE(se.cnt,0)::bigint AS sessions_count \
         FROM ( \
             SELECT distinct_id, properties, first_seen, last_seen FROM event_users \
             WHERE app_id=$1 AND distinct_id=$2{membership_sql} \
         ) eu \
         LEFT JOIN LATERAL (SELECT count(*) cnt, min(occurred_at) min_occurred, \
                    max(occurred_at) max_occurred FROM analytics_events \
                    WHERE app_id=$1 AND distinct_id = eu.distinct_id{env_sql}) ae ON TRUE \
         LEFT JOIN LATERAL (SELECT count(*) cnt, min(occurred_at) min_occurred, \
                    max(occurred_at) max_occurred FROM error_events \
                    WHERE app_id=$1 AND distinct_id = eu.distinct_id{env_sql}) ee ON TRUE \
         LEFT JOIN LATERAL (SELECT count(*) cnt, min(started_at) min_started, \
                    max(last_event_at) max_last_event FROM sessions \
                    WHERE app_id=$1 AND distinct_id = eu.distinct_id{env_sql}) se ON TRUE"
    );
    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Text, _>(distinct_id.to_string());
    stmt = crate::bind_env!(stmt, &scope.env);
    stmt.get_result(conn).await.optional()
}

/// `analytics_events` carries its own `environment_id` directly, so this is an
/// ordinary `scope_env!` filter — unlike `get_event_user`, which has to derive
/// membership because `event_users` itself carries none.
pub async fn events_for_person(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    distinct_id: &str,
    limit: i64,
) -> QueryResult<Vec<AnalyticsEvent>> {
    let q = analytics_events::table
        .filter(analytics_events::app_id.eq(scope.app_id))
        .filter(analytics_events::distinct_id.eq(distinct_id))
        .into_boxed();
    crate::scope_env!(q, analytics_events, &scope.env)
        .select(AnalyticsEvent::as_select())
        .order(analytics_events::occurred_at.desc())
        .limit(limit)
        .load(conn)
        .await
}

#[derive(Debug, PartialEq, QueryableByName, serde::Serialize)]
pub struct EventCount {
    #[diesel(sql_type = Text)]
    pub name: String,
    #[diesel(sql_type = BigInt)]
    pub count: i64,
}

pub async fn top_events(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    since: DateTime<Utc>,
    limit: i64,
) -> QueryResult<Vec<EventCount>> {
    // The env fragment takes $3 when it needs a bind; `limit` therefore lands on
    // $4 in that case and $3 otherwise. Deriving both from the same `EnvFilter`
    // is what keeps the string and the bind sequence in agreement — see
    // `EnvFilter::sql_fragment`'s doc for why only `One` consumes an index.
    let env_sql = scope.env.sql_fragment(3);
    let limit_idx = if scope.env.consumes_bind() { 4 } else { 3 };

    let q = format!(
        "SELECT name, count(*)::bigint AS count FROM analytics_events \
         WHERE app_id = $1 AND occurred_at >= $2{env_sql} \
         GROUP BY name ORDER BY count DESC LIMIT ${limit_idx}"
    );
    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Timestamptz, _>(since);
    stmt = crate::bind_env!(stmt, &scope.env);
    stmt.bind::<BigInt, _>(limit).get_results(conn).await
}

#[derive(Debug, QueryableByName, serde::Serialize)]
pub struct SeriesPoint {
    #[diesel(sql_type = Timestamptz)]
    pub bucket: DateTime<Utc>,
    #[diesel(sql_type = BigInt)]
    pub count: i64,
}

pub async fn event_series(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    name: Option<&str>,
    since: DateTime<Utc>,
) -> QueryResult<Vec<SeriesPoint>> {
    match name {
        Some(n) => {
            // $1 app_id, $2 since, $3 name — env takes $4 when it needs a bind.
            let env_sql = scope.env.sql_fragment(4);
            let q = format!(
                "SELECT date_trunc('day', occurred_at) AS bucket, count(*)::bigint AS count \
                 FROM analytics_events \
                 WHERE app_id = $1 AND occurred_at >= $2 AND name = $3{env_sql} \
                 GROUP BY bucket ORDER BY bucket"
            );
            let mut stmt = diesel::sql_query(q)
                .into_boxed()
                .bind::<SqlUuid, _>(scope.app_id)
                .bind::<Timestamptz, _>(since)
                .bind::<Text, _>(n);
            stmt = crate::bind_env!(stmt, &scope.env);
            stmt.get_results(conn).await
        }
        None => {
            // $1 app_id, $2 since — env takes $3 when it needs a bind.
            let env_sql = scope.env.sql_fragment(3);
            let q = format!(
                "SELECT date_trunc('day', occurred_at) AS bucket, count(*)::bigint AS count \
                 FROM analytics_events \
                 WHERE app_id = $1 AND occurred_at >= $2{env_sql} \
                 GROUP BY bucket ORDER BY bucket"
            );
            let mut stmt = diesel::sql_query(q)
                .into_boxed()
                .bind::<SqlUuid, _>(scope.app_id)
                .bind::<Timestamptz, _>(since);
            stmt = crate::bind_env!(stmt, &scope.env);
            stmt.get_results(conn).await
        }
    }
}

/// `error_events` carries its own `environment_id` directly, so this is a
/// plain predicate fragment — no LATERAL/`EXISTS` needed, unlike the
/// `issues`-table-level reads above.
pub async fn issue_occurrence_series(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    issue_id: Uuid,
    since: DateTime<Utc>,
) -> QueryResult<Vec<SeriesPoint>> {
    // $1 issue_id, $2 app_id, $3 since — env takes $4 when it needs a bind.
    let env_sql = scope.env.sql_fragment(4);
    let mut stmt = diesel::sql_query(format!(
        "SELECT date_trunc('day', occurred_at) AS bucket, count(*)::bigint AS count \
         FROM error_events \
         WHERE issue_id = $1 AND app_id = $2 AND occurred_at >= $3{env_sql} \
         GROUP BY bucket ORDER BY bucket"
    ))
    .into_boxed()
    .bind::<SqlUuid, _>(issue_id)
    .bind::<SqlUuid, _>(scope.app_id)
    .bind::<Timestamptz, _>(since);
    stmt = crate::bind_env!(stmt, &scope.env);
    stmt.get_results(conn).await
}

// ===========================================================================
// Sessions & devices (roll-ups upserted by the pipeline)
// ===========================================================================

/// Upsert a session row, folding one signal into it: bump last/first seen and
/// the event/error counters. `context` snapshots the device/os block (only
/// written when non-empty). Idempotent per `(app_id, session_id)`.
///
/// `environment_id` is `COALESCE(EXCLUDED.environment_id,
/// sessions.environment_id)` — the most recent non-null value wins — and
/// `events_count`/`errors_count` accumulate across every environment that
/// touched this session id. The row's own environment label therefore cannot
/// disambiguate its counters. Readers that need per-environment truth derive
/// it from the environment-stamped child tables instead; see
/// `events_for_session`, which says the same thing for the same reason.
///
/// Two readers do NOT do this today: `overview_totals`'s `crashed_sessions`
/// and `session_stats`'s `crashed` both count `errors_count > 0 AND
/// environment_id = $env` directly off this row, trusting the label. Task 10
/// (Slice 3) measured the fix — deriving `crashed` from an `EXISTS` against
/// `error_events` in the selected environment instead — against the largest
/// dev app and declined to ship it: the semi-join cost roughly 11x the
/// column predicate's planning+execution time even with a purpose-built
/// `error_events (app_id, session_id, environment_id)` index, because the
/// cost is a correlated per-session subquery re-probed against every
/// partition of `error_events` (partition pruning cannot help — neither
/// `session_id` nor `environment_id` is the partition key), not a missing
/// index. See
/// `.superpowers/sdd/2026-07-29-environment-rbac-scope/task-10-report.md`
/// for the full measurement. Until a cheaper derivation exists (e.g. a
/// per-session/environment crash flag maintained by this function itself,
/// rather than computed at read time), this is a *mislabelling*, not just an
/// over-count: a session that errored in `env_a` but was last touched by a
/// signal from `env_b` shows as crashed under `One(env_b)` (which never saw
/// the error) and simultaneously invisible — not merely "not crashed" —
/// under `One(env_a)` (which did), because the row's single label can only
/// point at one environment at a time. `errors_count` itself never
/// under-counts (it only grows), but which environment gets credited for it
/// can be wrong in either direction.
#[allow(clippy::too_many_arguments)]
pub async fn bump_session(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    session_id: &str,
    distinct_id: Option<&str>,
    device_key: Option<&str>,
    at: DateTime<Utc>,
    context: &Value,
    release: Option<&str>,
    environment_id: Option<Uuid>,
    ip: Option<&str>,
    events_delta: i64,
    errors_delta: i64,
) -> QueryResult<usize> {
    diesel::sql_query(
        "INSERT INTO sessions \
           (app_id, session_id, distinct_id, device_key, started_at, last_event_at, \
            events_count, errors_count, context, release, environment_id, ip_address) \
         VALUES ($1, $2, $3, $4, $5, $5, $6, $7, $8, $9, $10, $11) \
         ON CONFLICT (app_id, session_id) DO UPDATE SET \
            last_event_at = GREATEST(sessions.last_event_at, EXCLUDED.last_event_at), \
            started_at = LEAST(sessions.started_at, EXCLUDED.started_at), \
            events_count = sessions.events_count + EXCLUDED.events_count, \
            errors_count = sessions.errors_count + EXCLUDED.errors_count, \
            distinct_id = COALESCE(EXCLUDED.distinct_id, sessions.distinct_id), \
            device_key = COALESCE(EXCLUDED.device_key, sessions.device_key), \
            context = CASE WHEN EXCLUDED.context <> '{}'::jsonb THEN EXCLUDED.context ELSE sessions.context END, \
            release = COALESCE(EXCLUDED.release, sessions.release), \
            environment_id = COALESCE(EXCLUDED.environment_id, sessions.environment_id), \
            ip_address = COALESCE(EXCLUDED.ip_address, sessions.ip_address), \
            updated_at = now()",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Text, _>(session_id)
    .bind::<Nullable<Text>, _>(distinct_id)
    .bind::<Nullable<Text>, _>(device_key)
    .bind::<Timestamptz, _>(at)
    .bind::<BigInt, _>(events_delta)
    .bind::<BigInt, _>(errors_delta)
    .bind::<Jsonb, _>(context.clone())
    .bind::<Nullable<Text>, _>(release)
    .bind::<Nullable<SqlUuid>, _>(environment_id)
    .bind::<Nullable<Text>, _>(ip)
    .execute(conn)
    .await
}

/// Upsert a device row, folding one signal into it. Idempotent per
/// `(app_id, device_key)`; descriptor fields only overwrite when non-null.
#[allow(clippy::too_many_arguments)]
pub async fn bump_device(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    device_key: &str,
    family: Option<&str>,
    model: Option<&str>,
    os_name: Option<&str>,
    os_version: Option<&str>,
    arch: Option<&str>,
    browser: Option<&str>,
    distinct_id: Option<&str>,
    at: DateTime<Utc>,
    events_delta: i64,
    errors_delta: i64,
) -> QueryResult<usize> {
    diesel::sql_query(
        "INSERT INTO devices \
           (app_id, device_key, family, model, os_name, os_version, arch, browser, \
            last_distinct_id, first_seen, last_seen, events_count, errors_count) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $10, $11, $12) \
         ON CONFLICT (app_id, device_key) DO UPDATE SET \
            last_seen = GREATEST(devices.last_seen, EXCLUDED.last_seen), \
            first_seen = LEAST(devices.first_seen, EXCLUDED.first_seen), \
            events_count = devices.events_count + EXCLUDED.events_count, \
            errors_count = devices.errors_count + EXCLUDED.errors_count, \
            last_distinct_id = COALESCE(EXCLUDED.last_distinct_id, devices.last_distinct_id), \
            family = COALESCE(EXCLUDED.family, devices.family), \
            model = COALESCE(EXCLUDED.model, devices.model), \
            os_name = COALESCE(EXCLUDED.os_name, devices.os_name), \
            os_version = COALESCE(EXCLUDED.os_version, devices.os_version), \
            arch = COALESCE(EXCLUDED.arch, devices.arch), \
            browser = COALESCE(EXCLUDED.browser, devices.browser), \
            updated_at = now()",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Text, _>(device_key)
    .bind::<Nullable<Text>, _>(family)
    .bind::<Nullable<Text>, _>(model)
    .bind::<Nullable<Text>, _>(os_name)
    .bind::<Nullable<Text>, _>(os_version)
    .bind::<Nullable<Text>, _>(arch)
    .bind::<Nullable<Text>, _>(browser)
    .bind::<Nullable<Text>, _>(distinct_id)
    .bind::<Timestamptz, _>(at)
    .bind::<BigInt, _>(events_delta)
    .bind::<BigInt, _>(errors_delta)
    .execute(conn)
    .await
}

// ===========================================================================
// Workflows (optional, explicitly-bounded spans; roll-up upserted by the
// pipeline, keyed by `(app_id, workflow_id)` rather than `session_id` — see
// migration 2026-07-29-000032_workflows' own doc comment for why)
// ===========================================================================

/// A workflow still `active` with no activity for this long is reported as
/// abandoned (derived on read, never stored — there is no fourth `status`
/// value and no sweeper job). Matches the breadcrumb-buffer TTL. Declared
/// once, here, so no second copy of the threshold can drift from it.
pub const WORKFLOW_STALE_MINUTES: i64 = 30;

/// The three reserved lifecycle transitions a `$workflow_start` /
/// `$workflow_end` / `$workflow_cancel` event drives via
/// [`apply_workflow_lifecycle`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowAction {
    Start,
    End,
    Cancel,
}

/// Upsert a `workflows` row, folding one stamped signal into it: bump
/// last-activity/earliest-start and the event/error counters. Idempotent per
/// `(app_id, workflow_id)` — mirrors [`bump_session`] with `session_id`
/// swapped for `workflow_id`; read that function's doc comment first.
///
/// Deliberately never touches `status`, `ended_at` or `cancel_reason`: those
/// three columns are [`apply_workflow_lifecycle`]'s alone to set, which is
/// what protects a terminal (`completed`/`cancelled`) workflow from being
/// reopened by a late-arriving stamped event that has nothing to do with the
/// lifecycle itself (e.g. an ordinary analytics event stamped with a
/// `workflow_id` whose `$workflow_end` was already processed).
///
/// ## `environment_id` is the FIRST writer's, and is NOT authoritative
///
/// The unique key is `(app_id, workflow_id)` — **app-wide**. `environment_id`
/// is in the INSERT column list but in no `DO UPDATE SET` clause, so whichever
/// signal creates the row labels it forever, while `events_count`/
/// `errors_count` accumulate from every environment that touches that
/// `workflow_id`. A workflow can therefore be *labelled* one environment while
/// its counters were incremented by signals from another — the same hazard
/// [`bump_session`]'s own doc comment describes for sessions, arrived at by a
/// different route (it lets the label flip to the latest non-null value; this
/// pins it to the first).
///
/// **Read-side scoping filters on `workflows.environment_id` directly** —
/// see `workflow_list`/`workflow_detail`/`workflow_runs`/
/// `workflow_spans_for_session` below, which splice `ReadScope`'s SQL
/// fragment against this table exactly as they would against any other
/// environment-stamped one. An earlier version of this comment said the
/// opposite — "a read-side caller must not treat this as an environment
/// filter... derive environment from the environment-stamped child rows
/// instead". That instruction was overstated and is superseded by this
/// paragraph (see
/// `.superpowers/sdd/2026-07-29-workflow-grouping/task-4-report.md` for the
/// ruling).
///
/// Why the column is trustworthy enough to filter on directly, despite being
/// the first writer's rather than the most recent: `workflow_id` is a
/// **client-generated UUID** (see the design doc), so a workflow row belongs
/// to exactly one app+environment in practice — cross-environment
/// mislabelling requires a client to violate that contract by reusing an id.
/// That is a narrower exposure than `sessions.environment_id`'s identical
/// trade-off, where every reader already trusts the column even though a
/// real session can legitimately span environments (this one, keyed by a
/// UUID, is not expected to).
///
/// The alternative — deriving environment from the stamped child rows via an
/// `EXISTS` semi-join instead of trusting this column — **has not been
/// measured against this table.** What *was* measured is the analogous
/// semi-join on `sessions`: Slice 3's Task 10 replaced `sessions.crashed`'s
/// column predicate with an `EXISTS` against `error_events` and found it
/// ~11x more expensive (11.3x / 11.0x on the two shapes it timed), even with
/// a purpose-built index, because it becomes a correlated per-row subquery
/// re-probed against every partition of `error_events` rather than a
/// missing-index problem. See
/// `.superpowers/sdd/2026-07-29-environment-rbac-scope/task-10-report.md` —
/// note that it predates this table and mentions workflows nowhere. The same
/// shape and therefore a similar cost is *expected* here (same child tables,
/// same partitioning, same correlated-subquery structure), but that is an
/// inference from the `sessions` result, not a workflows measurement. Anyone
/// designing a stricter scoping fix should re-measure against `workflows`
/// rather than treat ~11x as an established number for this table.
///
/// None of this makes the label infallible, and it is still not "fixed" with
/// a `COALESCE` refresh on every `bump_workflow` call: that would make the
/// label flip-flop between environments on every signal, which is strictly
/// worse than a stable-but-occasionally-wrong one. The residual risk — a
/// hand-rolled client reusing one `workflow_id` across two environments — is
/// accepted rather than paid for with a materially more expensive read on
/// every workflow query.
///
/// ## `COALESCE` argument order is inverted vs. [`bump_session`], on purpose
///
/// `session_id`/`distinct_id`/`device_key`/`release`/`name` here are
/// `COALESCE(workflows.<col>, EXCLUDED.<col>)` — **first** non-null wins —
/// whereas `bump_session` writes `COALESCE(EXCLUDED.<col>, sessions.<col>)`,
/// i.e. **last** non-null wins. Not a typo in either place. A session is a
/// long-lived, re-identified thing whose newest attribution is its best one
/// (a user logging in mid-session should re-label it); an explicitly-bounded
/// workflow's owning session/device/release is a property of where it *began*,
/// and letting a later straggler signal repoint it would rewrite history the
/// UI already showed. `name` follows the same rule via `NULLIF`, treating `''`
/// as absent — see [`apply_workflow_lifecycle`] for the clobber that guards
/// against.
#[allow(clippy::too_many_arguments)]
pub async fn bump_workflow(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    environment_id: Uuid,
    workflow_id: &str,
    workflow_name: &str,
    session_id: Option<&str>,
    distinct_id: Option<&str>,
    device_key: Option<&str>,
    release: Option<&str>,
    occurred_at: DateTime<Utc>,
    events_delta: i32,
    errors_delta: i32,
) -> QueryResult<usize> {
    diesel::sql_query(
        "INSERT INTO workflows \
           (app_id, environment_id, workflow_id, name, session_id, distinct_id, \
            device_key, release, started_at, last_event_at, events_count, errors_count) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $9, $10, $11) \
         ON CONFLICT (app_id, workflow_id) DO UPDATE SET \
            last_event_at = GREATEST(workflows.last_event_at, EXCLUDED.last_event_at), \
            started_at    = LEAST(workflows.started_at, EXCLUDED.started_at), \
            events_count  = workflows.events_count + EXCLUDED.events_count, \
            errors_count  = workflows.errors_count + EXCLUDED.errors_count, \
            name          = COALESCE(NULLIF(workflows.name, ''), EXCLUDED.name), \
            session_id    = COALESCE(workflows.session_id, EXCLUDED.session_id), \
            distinct_id   = COALESCE(workflows.distinct_id, EXCLUDED.distinct_id), \
            device_key    = COALESCE(workflows.device_key, EXCLUDED.device_key), \
            release       = COALESCE(workflows.release, EXCLUDED.release), \
            updated_at    = now()",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<SqlUuid, _>(environment_id)
    .bind::<Text, _>(workflow_id)
    .bind::<Text, _>(workflow_name)
    .bind::<Nullable<Text>, _>(session_id)
    .bind::<Nullable<Text>, _>(distinct_id)
    .bind::<Nullable<Text>, _>(device_key)
    .bind::<Nullable<Text>, _>(release)
    .bind::<Timestamptz, _>(occurred_at)
    .bind::<Integer, _>(events_delta)
    .bind::<Integer, _>(errors_delta)
    .execute(conn)
    .await
}

/// Apply a `$workflow_start` / `$workflow_end` / `$workflow_cancel` lifecycle
/// event to its `workflows` row, upserting it if this is the first signal
/// seen for the workflow.
///
/// `Start` deliberately does **not** set `status = 'active'` on conflict —
/// only backfilling `name`/`started_at` — so a start event that arrives
/// *after* the end/cancel event (out-of-order delivery) cannot reopen an
/// already-terminal workflow. `End`/`Cancel` guard every field they touch
/// (`status`, `ended_at`, `cancel_reason`) with `CASE WHEN workflows.status =
/// 'active'`: the first terminal transition wins and a second one — of
/// either kind — is silently ignored, matching `bump_workflow`'s own
/// never-reopen guarantee.
///
/// `Start`'s `name` clause is `COALESCE(NULLIF(EXCLUDED.name, ''),
/// workflows.name)`, not a bare `EXCLUDED.name`. `workflows.name` is `TEXT NOT
/// NULL` with no emptiness CHECK, and the caller resolves an absent name to
/// `""` — so a bare assignment would let the exact client this function's
/// property-fallback exists for (a hand-rolled one posting `$workflow_start`
/// with a `workflow_id` but no name) permanently destroy a good display name
/// that stamped SDK events had already established. `NULLIF` makes `''` mean
/// "I have nothing to offer", which is what it actually means here.
///
/// The caller is deliberately NOT made to skip the lifecycle call when no name
/// resolves: dropping the transition would strand the workflow `active`
/// forever and it would then be misreported as abandoned once
/// [`WORKFLOW_STALE_MINUTES`] elapsed. A missing name is cosmetic; a missing
/// terminal transition is not. The row can still be *created* with `name = ''`
/// when a nameless lifecycle event is the very first signal for a workflow —
/// there is genuinely nothing better to store at that point — and
/// [`bump_workflow`]'s own `COALESCE(NULLIF(workflows.name, ''), …)` upgrades
/// it as soon as any named signal arrives.
#[allow(clippy::too_many_arguments)]
pub async fn apply_workflow_lifecycle(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    environment_id: Uuid,
    workflow_id: &str,
    workflow_name: &str,
    action: WorkflowAction,
    cancel_reason: Option<&str>,
    session_id: Option<&str>,
    distinct_id: Option<&str>,
    occurred_at: DateTime<Utc>,
) -> QueryResult<usize> {
    match action {
        WorkflowAction::Start => {
            diesel::sql_query(
                "INSERT INTO workflows \
                   (app_id, environment_id, workflow_id, name, session_id, distinct_id, \
                    status, started_at, last_event_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, 'active', $7, $7) \
                 ON CONFLICT (app_id, workflow_id) DO UPDATE SET \
                    name       = COALESCE(NULLIF(EXCLUDED.name, ''), workflows.name), \
                    started_at = LEAST(workflows.started_at, EXCLUDED.started_at), \
                    updated_at = now()",
            )
            .bind::<SqlUuid, _>(app_id)
            .bind::<SqlUuid, _>(environment_id)
            .bind::<Text, _>(workflow_id)
            .bind::<Text, _>(workflow_name)
            .bind::<Nullable<Text>, _>(session_id)
            .bind::<Nullable<Text>, _>(distinct_id)
            .bind::<Timestamptz, _>(occurred_at)
            .execute(conn)
            .await
        }
        WorkflowAction::End | WorkflowAction::Cancel => {
            let status = if action == WorkflowAction::End {
                "completed"
            } else {
                "cancelled"
            };
            diesel::sql_query(
                "INSERT INTO workflows \
                   (app_id, environment_id, workflow_id, name, session_id, distinct_id, \
                    status, cancel_reason, started_at, ended_at, last_event_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $9, $9) \
                 ON CONFLICT (app_id, workflow_id) DO UPDATE SET \
                    status        = CASE WHEN workflows.status = 'active' THEN EXCLUDED.status ELSE workflows.status END, \
                    ended_at      = CASE WHEN workflows.status = 'active' THEN EXCLUDED.ended_at ELSE workflows.ended_at END, \
                    cancel_reason = CASE WHEN workflows.status = 'active' THEN EXCLUDED.cancel_reason ELSE workflows.cancel_reason END, \
                    last_event_at = GREATEST(workflows.last_event_at, EXCLUDED.last_event_at), \
                    started_at    = LEAST(workflows.started_at, EXCLUDED.started_at), \
                    updated_at    = now()",
            )
            .bind::<SqlUuid, _>(app_id)
            .bind::<SqlUuid, _>(environment_id)
            .bind::<Text, _>(workflow_id)
            .bind::<Text, _>(workflow_name)
            .bind::<Nullable<Text>, _>(session_id)
            .bind::<Nullable<Text>, _>(distinct_id)
            .bind::<Text, _>(status)
            .bind::<Nullable<Text>, _>(cancel_reason)
            .bind::<Timestamptz, _>(occurred_at)
            .execute(conn)
            .await
        }
    }
}

// ---------------------------------------------------------------------------
// Workflows (read side, Task 4): on-read aggregation over `workflows` plus
// the environment-stamped child tables it names in its own doc comment
// (`analytics_events`/`error_events`), following the `screen_list`/
// `screen_stats` template — raw `sql_query`, `ReadScope::sql_fragment(_for)`
// spliced into the WHERE clause, positional binds in matching order,
// `QueryableByName` result structs. `workflows.environment_id` is filtered on
// directly (see `bump_workflow`'s doc comment above for why that is now the
// correct call, not an oversight).
// ---------------------------------------------------------------------------

/// The `eff` (effective status) projection every workflow read function
/// shares: a workflow's own `status`, unless it is still `active` and has had
/// no activity for `WORKFLOW_STALE_MINUTES`, in which case it reads as
/// `'abandoned'` — derived on read, never stored (see `WORKFLOW_STALE_MINUTES`'s
/// own doc comment). A function rather than a `const &str`, because the
/// threshold has to be spliced in via `format!`; safe to do so —
/// `WORKFLOW_STALE_MINUTES` is a compile-time integer constant, never user
/// input. Defined once so `workflow_list`/`workflow_detail`/`workflow_runs`/
/// `workflow_spans_for_session` cannot drift from one another or from the
/// constant itself. Assumes the `workflows` row is aliased `w`, which every
/// call site below does.
fn workflow_effective_status_sql() -> String {
    format!(
        "CASE WHEN w.status = 'active' AND w.last_event_at < now() - make_interval(mins => {WORKFLOW_STALE_MINUTES}) \
         THEN 'abandoned' ELSE w.status END"
    )
}

/// Shared inner subquery for the workflow outcome/duration aggregate, reused
/// by `workflow_list` (grouped by name, an outer `ILIKE` narrows which names
/// survive) and `workflow_detail` (narrowed to one name *inside* this
/// subquery, so the outer aggregate collapses to a single row) — the same
/// "shared CTE, different predicate" idiom `screen_ctes` uses for
/// `screen_list`/`screen_stats`.
///
/// `name_pred` is a compile-time SQL fragment, never user data: `""` for the
/// list (name filtering happens outside, against the derived table's own
/// `w.name`) or `" AND w.name = $3"` for the detail view (a *bound* param at
/// a fixed index, never interpolated). `env_sql` is an
/// `EnvFilter::sql_fragment_for("w", _)` output, and MUST be applied inside
/// this subquery rather than the outer query — same reasoning as
/// `screen_ctes`'s own doc comment: the outer query only sees the columns
/// this subquery selects, and even if `environment_id` were visible there,
/// filtering after the aggregate `eff`/`dur` projection would still compute
/// them from every environment's rows first.
fn workflow_outcome_subquery(env_sql: &str, name_pred: &str) -> String {
    let eff = workflow_effective_status_sql();
    format!(
        "SELECT w.*, {eff} AS eff, \
                CASE WHEN w.ended_at IS NOT NULL \
                     THEN EXTRACT(EPOCH FROM (w.ended_at - w.started_at)) * 1000 END AS dur \
         FROM workflows w \
         WHERE w.app_id = $1 AND w.started_at >= now() - make_interval(days => $2){env_sql}{name_pred}"
    )
}

/// One row per workflow **name** — the outcome/duration aggregate
/// `workflow_list` and `workflow_detail` both produce (`workflow_detail`
/// reuses this exact shape for its own single-name aggregate before folding
/// in the other three pieces).
#[derive(Debug, QueryableByName, serde::Serialize)]
pub struct WorkflowRow {
    #[diesel(sql_type = Text)]
    pub name: String,
    #[diesel(sql_type = BigInt)]
    pub started: i64,
    #[diesel(sql_type = BigInt)]
    pub completed: i64,
    #[diesel(sql_type = BigInt)]
    pub cancelled: i64,
    #[diesel(sql_type = BigInt)]
    pub abandoned: i64,
    #[diesel(sql_type = BigInt)]
    pub active: i64,
    #[diesel(sql_type = BigInt)]
    pub unique_users: i64,
    /// `NULL` when no run in the window has an `ended_at` yet — only
    /// `completed`/`cancelled` rows have one, so an all-`active`/`abandoned`
    /// group has no duration to report. That is the intended semantic:
    /// duration describes finished runs.
    #[diesel(sql_type = Nullable<Double>)]
    pub median_duration_ms: Option<f64>,
    #[diesel(sql_type = Nullable<Double>)]
    pub p95_duration_ms: Option<f64>,
    #[diesel(sql_type = Timestamptz)]
    pub last_seen: DateTime<Utc>,
}

/// The SELECT list shared by `workflow_list`'s and `workflow_detail`'s
/// outcome/duration aggregate — factored out so the two cannot drift on a
/// column, an alias, or a FILTER predicate. Operates over
/// `workflow_outcome_subquery`'s derived table, aliased `w`.
const WORKFLOW_OUTCOME_SELECT: &str = "\
    w.name, \
    COUNT(*)::bigint AS started, \
    COUNT(*) FILTER (WHERE w.eff = 'completed')::bigint AS completed, \
    COUNT(*) FILTER (WHERE w.eff = 'cancelled')::bigint AS cancelled, \
    COUNT(*) FILTER (WHERE w.eff = 'abandoned')::bigint AS abandoned, \
    COUNT(*) FILTER (WHERE w.eff = 'active')::bigint AS active, \
    COUNT(DISTINCT w.distinct_id)::bigint AS unique_users, \
    percentile_cont(0.5) WITHIN GROUP (ORDER BY w.dur)::double precision AS median_duration_ms, \
    percentile_cont(0.95) WITHIN GROUP (ORDER BY w.dur)::double precision AS p95_duration_ms, \
    MAX(w.last_event_at) AS last_seen";

/// One row per workflow name: started/completed/cancelled/abandoned/active
/// counts, unique users, median/p95 duration (finished runs only) and last
/// seen — paginated, optionally substring-filtered by name.
///
/// Bind layout: `$1` app_id, `$2` since_days — env takes `$3` when it needs a
/// bind (reserved against the inner subquery's `w` alias). `search` always
/// binds (`Nullable<Text>`) at the next free index — `$4` if env consumed
/// `$3`, else `$3` — and `limit`/`offset` trail it. Same "trailing-index
/// shift" idiom as `screen_list`/`top_events`: every index downstream of the
/// env fragment is computed *from* `scope.env.consumes_bind()`, not
/// hard-coded, so the two can never drift apart.
///
/// `search` is matched with [`like_contains`], so a term containing `%` or
/// `_` matches those characters *literally* rather than as wildcards —
/// matching every other free-text search in this file. The `%…%` wrapping
/// happens in Rust, not via SQL-side `||` concatenation, which is what lets
/// the escaping apply at all.
pub async fn workflow_list(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    since_days: i32,
    search: Option<&str>,
    limit: i64,
    offset: i64,
) -> QueryResult<Vec<WorkflowRow>> {
    let env_sql = scope.env.sql_fragment_for("w", 3);
    let search_idx = if scope.env.consumes_bind() { 4 } else { 3 };
    let limit_idx = search_idx + 1;
    let offset_idx = limit_idx + 1;
    let search_pattern = search.map(like_contains);

    let q = format!(
        "SELECT {WORKFLOW_OUTCOME_SELECT} \
         FROM ({}) w \
         WHERE (${search_idx}::text IS NULL OR w.name ILIKE ${search_idx}) \
         GROUP BY w.name \
         ORDER BY started DESC, w.name ASC \
         LIMIT ${limit_idx} OFFSET ${offset_idx}",
        workflow_outcome_subquery(&env_sql, "")
    );

    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Integer, _>(since_days);
    stmt = crate::bind_env!(stmt, &scope.env);
    stmt.bind::<Nullable<Text>, _>(search_pattern)
        .bind::<BigInt, _>(limit)
        .bind::<BigInt, _>(offset)
        .get_results(conn)
        .await
}

/// One contained event name and its count within a workflow — `top_events`'
/// row shape.
#[derive(Debug, QueryableByName, serde::Serialize)]
pub struct NameCount {
    #[diesel(sql_type = Text)]
    pub name: String,
    #[diesel(sql_type = BigInt)]
    pub count: i64,
}

/// One contained issue and its occurrence count within a workflow —
/// `top_issues`' row shape.
#[derive(Debug, QueryableByName, serde::Serialize)]
pub struct WorkflowIssue {
    #[diesel(sql_type = SqlUuid)]
    pub issue_id: Uuid,
    #[diesel(sql_type = Text)]
    pub title: String,
    #[diesel(sql_type = BigInt)]
    pub count: i64,
}

/// A single workflow name's full detail: the same outcome/duration aggregate
/// as `workflow_list` (one name's worth), a duration histogram, the top
/// contained (non-lifecycle) event names, and the top contained issues.
#[derive(Debug, serde::Serialize)]
pub struct WorkflowDetail {
    pub name: String,
    pub started: i64,
    pub completed: i64,
    pub cancelled: i64,
    pub abandoned: i64,
    pub active: i64,
    pub unique_users: i64,
    pub median_duration_ms: Option<f64>,
    pub p95_duration_ms: Option<f64>,
    pub duration_buckets: Vec<HistoBucket>,
    pub top_events: Vec<NameCount>,
    pub top_issues: Vec<WorkflowIssue>,
}

/// The three reserved lifecycle event names' shared prefix — `NOT LIKE
/// '$workflow%'` in `workflow_detail`'s `top_events` query excludes all three
/// (`$workflow_start`/`_end`/`_cancel`), which would otherwise dominate every
/// contained-event list. A compile-time literal, not user data, so it is
/// written directly into the SQL text rather than bound.
const WORKFLOW_LIFECYCLE_EVENT_PATTERN: &str = "$workflow%";

/// Full detail for one workflow name: outcome/duration aggregate, duration
/// histogram, top contained events, top contained issues.
///
/// Every one of the four queries below binds in the same order — `$1`
/// app_id, `$2` since_days, `$3` name, `$4` env (only when
/// `scope.env.consumes_bind()`) — a deliberate uniformity (none of them
/// *needs* to share a layout, since each is an independent prepared
/// statement) that exists purely to make this function's own bind-index
/// bookkeeping easy to verify by inspection.
///
/// Returns `Err(NotFound)` if `name` never had a matching `workflows` row in
/// `scope`'s environment(s) within `since_days` — the outcome aggregate's
/// `GROUP BY w.name` yields zero rows in that case, same "vanishes rather
/// than zero-fills" behaviour `screen_stats` has for a screen never seen.
///
/// ## The four queries use two *different* time windows, on purpose
///
/// The outcome aggregate and the duration histogram bound
/// `workflows.started_at`; `top_events` and `top_issues` bound the child
/// tables' `occurred_at`. These do not describe the same set of runs, and
/// the totals are not expected to reconcile: a run that *started* just
/// outside the window but emitted events inside it contributes to
/// `top_events`/`top_issues` while contributing nothing to
/// `started`/`completed`/the histogram. The reverse also happens — a run
/// that started inside the window but whose events all predate it.
///
/// This is deliberate rather than an oversight. Bounding the child tables by
/// their owning workflow's `started_at` instead would require joining
/// `workflows` into both queries, which is exactly the correlated-subquery
/// shape `bump_workflow`'s doc comment records being measured at ~11x on the
/// analogous `sessions` case — a large cost to make two independently
/// meaningful numbers agree at the edges. "Events seen inside this window
/// that belonged to this workflow" is a defensible reading in its own right.
/// Dashboard note: do not present `sum(top_events.count)` as "events in the
/// runs counted above" — it is not that number.
pub async fn workflow_detail(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    name: &str,
    since_days: i32,
) -> QueryResult<WorkflowDetail> {
    const NAME_PRED: &str = " AND w.name = $3";

    // --- outcome/duration aggregate (one row) ------------------------------
    let env_sql = scope.env.sql_fragment_for("w", 4);
    let outcome_q = format!(
        "SELECT {WORKFLOW_OUTCOME_SELECT} FROM ({}) w GROUP BY w.name",
        workflow_outcome_subquery(&env_sql, NAME_PRED)
    );
    let mut outcome_stmt = diesel::sql_query(outcome_q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Integer, _>(since_days)
        .bind::<Text, _>(name);
    outcome_stmt = crate::bind_env!(outcome_stmt, &scope.env);
    let outcome: WorkflowRow = outcome_stmt.get_result(conn).await?;

    // --- duration histogram (finished runs only) ---------------------------
    // Reuses the whole existing duration-histogram scheme —
    // `DURATION_BUCKET_CASE_SQL` (the bucketing SQL itself),
    // `DURATION_BUCKETS`, `order_histogram` and `HistoBucket` — rather than a
    // fresh `width_bucket` scheme or a second copy of the CASE.
    let env_sql = scope.env.sql_fragment_for("w", 4);
    let buckets_q = format!(
        "SELECT bucket, count(*)::bigint AS count FROM ( \
           SELECT {DURATION_BUCKET_CASE_SQL} AS bucket \
           FROM (SELECT EXTRACT(EPOCH FROM (w.ended_at - w.started_at)) * 1000 AS d \
                 FROM workflows w \
                 WHERE w.app_id = $1 AND w.started_at >= now() - make_interval(days => $2) \
                   AND w.name = $3 AND w.ended_at IS NOT NULL{env_sql}) s \
         ) b GROUP BY bucket"
    );
    let mut buckets_stmt = diesel::sql_query(buckets_q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Integer, _>(since_days)
        .bind::<Text, _>(name);
    buckets_stmt = crate::bind_env!(buckets_stmt, &scope.env);
    let bucket_rows: Vec<HistoBucket> = buckets_stmt.get_results(conn).await?;
    let duration_buckets = order_histogram(bucket_rows);

    // --- top contained events -----------------------------------------------
    // `analytics_events` is the only table in this FROM clause, so the env
    // fragment is unqualified — same as `top_events` above.
    //
    // `workflow_id IS NOT NULL` is load-bearing for performance, not
    // semantics: migration 2026-07-29-000032's index on this table is
    // PARTIAL (`... WHERE workflow_id IS NOT NULL`), and Postgres only uses a
    // partial index when the query's WHERE clause *implies* the index
    // predicate. `workflow_name = $3` does not imply it — they are different
    // columns — so without this term the index is unusable and the planner
    // falls back to scanning every partition of the largest table in the
    // system, filtering `workflow_name` as a post-scan qual. Measured on the
    // dev app (212k events, 22 partitions): 52,744 buffers / cost 56,190
    // without it, 14 buffers / cost 2,025 with it. Semantically a no-op — the
    // pipeline stamps `workflow_id` and `workflow_name` together, so a row
    // with a name always has an id. See task-4-report.md's "Fix round 1" for
    // both full plans.
    let env_sql = scope.env.sql_fragment(4);
    let events_q = format!(
        "SELECT name, COUNT(*)::bigint AS count \
         FROM analytics_events \
         WHERE app_id = $1 AND occurred_at >= now() - make_interval(days => $2) \
           AND workflow_name = $3 AND workflow_id IS NOT NULL \
           AND name NOT LIKE '{WORKFLOW_LIFECYCLE_EVENT_PATTERN}'{env_sql} \
         GROUP BY name ORDER BY count DESC LIMIT 10"
    );
    let mut events_stmt = diesel::sql_query(events_q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Integer, _>(since_days)
        .bind::<Text, _>(name);
    events_stmt = crate::bind_env!(events_stmt, &scope.env);
    let top_events: Vec<NameCount> = events_stmt.get_results(conn).await?;

    // --- top contained issues -----------------------------------------------
    // `error_events` (aliased `e`) joined to `issues` (aliased `i`) — env
    // must qualify `e`, the environment-stamped side of the join.
    //
    // `e.workflow_id IS NOT NULL` is here for the same partial-index reason
    // as the `top_events` query above — `error_events` carries the identical
    // partial index. Measured on the dev app (210k error events): 52,559
    // buffers / cost 56,267 without it, 14 buffers / cost 2,043 with it.
    let env_sql = scope.env.sql_fragment_for("e", 4);
    let issues_q = format!(
        "SELECT i.id AS issue_id, i.title, COUNT(*)::bigint AS count \
         FROM error_events e \
         JOIN issues i ON i.id = e.issue_id \
         WHERE e.app_id = $1 AND e.occurred_at >= now() - make_interval(days => $2) \
           AND e.workflow_name = $3 AND e.workflow_id IS NOT NULL{env_sql} \
         GROUP BY i.id, i.title \
         ORDER BY count DESC LIMIT 10"
    );
    let mut issues_stmt = diesel::sql_query(issues_q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Integer, _>(since_days)
        .bind::<Text, _>(name);
    issues_stmt = crate::bind_env!(issues_stmt, &scope.env);
    let top_issues: Vec<WorkflowIssue> = issues_stmt.get_results(conn).await?;

    Ok(WorkflowDetail {
        name: outcome.name,
        started: outcome.started,
        completed: outcome.completed,
        cancelled: outcome.cancelled,
        abandoned: outcome.abandoned,
        active: outcome.active,
        unique_users: outcome.unique_users,
        median_duration_ms: outcome.median_duration_ms,
        p95_duration_ms: outcome.p95_duration_ms,
        duration_buckets,
        top_events,
        top_issues,
    })
}

/// One individual workflow run — `workflow_runs`' row shape, linking to
/// session detail via `session_id`.
#[derive(Debug, QueryableByName, serde::Serialize)]
pub struct WorkflowRun {
    #[diesel(sql_type = Text)]
    pub workflow_id: String,
    #[diesel(sql_type = Nullable<Text>)]
    pub session_id: Option<String>,
    #[diesel(sql_type = Nullable<Text>)]
    pub distinct_id: Option<String>,
    /// The effective status (see `workflow_effective_status_sql`), not the
    /// raw column — an `active` row past `WORKFLOW_STALE_MINUTES` reports as
    /// `abandoned` here too.
    #[diesel(sql_type = Text)]
    pub status: String,
    #[diesel(sql_type = Timestamptz)]
    pub started_at: DateTime<Utc>,
    #[diesel(sql_type = Nullable<Timestamptz>)]
    pub ended_at: Option<DateTime<Utc>>,
    #[diesel(sql_type = Nullable<BigInt>)]
    pub duration_ms: Option<i64>,
    #[diesel(sql_type = Integer)]
    pub events_count: i32,
    #[diesel(sql_type = Integer)]
    pub errors_count: i32,
}

/// Individual runs of one workflow name, newest first, optionally filtered by
/// effective status (`active`/`completed`/`cancelled`/`abandoned` — compared
/// against the *projection*, not the raw `status` column, which is what makes
/// `abandoned` a filterable value at all, since it never appears as a stored
/// value).
///
/// Bind layout: `$1` app_id, `$2` since_days, `$3` name — env takes `$4` when
/// it needs a bind. `status` always binds (`Nullable<Text>`) at the next free
/// index — `$5` if env consumed `$4`, else `$4` — and `limit`/`offset` trail
/// it, same trailing-index-shift idiom as `workflow_list`.
pub async fn workflow_runs(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    name: &str,
    since_days: i32,
    status: Option<&str>,
    limit: i64,
    offset: i64,
) -> QueryResult<Vec<WorkflowRun>> {
    let env_sql = scope.env.sql_fragment_for("w", 4);
    let status_idx = if scope.env.consumes_bind() { 5 } else { 4 };
    let limit_idx = status_idx + 1;
    let offset_idx = limit_idx + 1;
    let eff = workflow_effective_status_sql();

    let q = format!(
        "SELECT w.workflow_id, w.session_id, w.distinct_id, {eff} AS status, \
                w.started_at, w.ended_at, \
                CASE WHEN w.ended_at IS NOT NULL \
                     THEN (EXTRACT(EPOCH FROM (w.ended_at - w.started_at)) * 1000)::bigint END AS duration_ms, \
                w.events_count, w.errors_count \
         FROM workflows w \
         WHERE w.app_id = $1 AND w.started_at >= now() - make_interval(days => $2) \
           AND w.name = $3{env_sql} \
           AND (${status_idx}::text IS NULL OR {eff} = ${status_idx}) \
         ORDER BY w.started_at DESC \
         LIMIT ${limit_idx} OFFSET ${offset_idx}"
    );

    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Integer, _>(since_days)
        .bind::<Text, _>(name);
    stmt = crate::bind_env!(stmt, &scope.env);
    stmt.bind::<Nullable<Text>, _>(status)
        .bind::<BigInt, _>(limit)
        .bind::<BigInt, _>(offset)
        .get_results(conn)
        .await
}

/// One workflow span within a session — for the session timeline lane.
#[derive(Debug, QueryableByName, serde::Serialize)]
pub struct WorkflowSpan {
    #[diesel(sql_type = Text)]
    pub workflow_id: String,
    #[diesel(sql_type = Text)]
    pub name: String,
    #[diesel(sql_type = Text)]
    pub status: String,
    #[diesel(sql_type = Timestamptz)]
    pub started_at: DateTime<Utc>,
    #[diesel(sql_type = Nullable<Timestamptz>)]
    pub ended_at: Option<DateTime<Utc>>,
}

/// Every workflow span within one session, oldest first — feeds the session
/// timeline lane.
///
/// Bind layout: `$1` app_id, `$2` session_id — env takes `$3` when it needs a
/// bind.
pub async fn workflow_spans_for_session(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    session_id: &str,
) -> QueryResult<Vec<WorkflowSpan>> {
    let env_sql = scope.env.sql_fragment_for("w", 3);
    let eff = workflow_effective_status_sql();
    let q = format!(
        "SELECT w.workflow_id, w.name, {eff} AS status, w.started_at, w.ended_at \
         FROM workflows w \
         WHERE w.app_id = $1 AND w.session_id = $2{env_sql} \
         ORDER BY w.started_at ASC"
    );
    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Text, _>(session_id);
    stmt = crate::bind_env!(stmt, &scope.env);
    stmt.get_results(conn).await
}

pub async fn insert_transaction(
    conn: &mut AsyncPgConnection,
    tx: NewTransaction,
) -> QueryResult<usize> {
    diesel::insert_into(transactions::table)
        .values(&tx)
        .execute(conn)
        .await
}

/// `(error_event_count, analytics_event_count)` for an app — onboarding poll.
/// Whether the app has received any error / analytics events yet.
///
/// Deliberately `EXISTS` rather than `count(*)`: the only consumer is the
/// onboarding "have we seen your first event?" poll, which needs a boolean.
/// Counting scanned every partition of the two largest tables on each poll.
///
/// Takes `ReadScope`, not a bare `app_id`: onboarding builds its DSN from one
/// specific environment, so a poll that answered "has ANY environment sent
/// anything" could report success purely from a *different* environment's
/// traffic (e.g. an app with existing prod events, where a user adds a
/// staging environment and revisits onboarding — the staging DSN would
/// immediately show "received" from prod rows alone).
pub async fn app_has_events(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
) -> QueryResult<(bool, bool)> {
    let has_errors: bool = diesel::select(diesel::dsl::exists(crate::scope_env!(
        error_events::table
            .filter(error_events::app_id.eq(scope.app_id))
            .into_boxed(),
        error_events,
        &scope.env
    )))
    .get_result(conn)
    .await?;
    let has_events: bool = diesel::select(diesel::dsl::exists(crate::scope_env!(
        analytics_events::table
            .filter(analytics_events::app_id.eq(scope.app_id))
            .into_boxed(),
        analytics_events,
        &scope.env
    )))
    .get_result(conn)
    .await?;
    Ok((has_errors, has_events))
}

pub async fn error_series(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    since: DateTime<Utc>,
) -> QueryResult<Vec<SeriesPoint>> {
    let env_sql = scope.env.sql_fragment(3);
    let q = format!(
        "SELECT date_trunc('day', occurred_at) AS bucket, count(*)::bigint AS count \
         FROM error_events WHERE app_id = $1 AND occurred_at >= $2{env_sql} \
         GROUP BY bucket ORDER BY bucket"
    );
    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Timestamptz, _>(since);
    stmt = crate::bind_env!(stmt, &scope.env);
    stmt.get_results(conn).await
}

// ===========================================================================
// Sessions (list + per-session signal streams for the timeline)
// ===========================================================================

#[allow(clippy::too_many_arguments)]
pub async fn list_sessions(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    since: DateTime<Utc>,
    limit: i64,
    offset: i64,
    distinct_id: Option<&str>,
    device_key: Option<&str>,
) -> QueryResult<Vec<Session>> {
    let mut q = sessions::table
        .filter(sessions::app_id.eq(scope.app_id))
        .filter(sessions::last_event_at.ge(since))
        .into_boxed();
    q = crate::scope_env!(q, sessions, &scope.env);
    if let Some(d) = distinct_id {
        q = q.filter(sessions::distinct_id.eq(d.to_string()));
    }
    if let Some(dk) = device_key {
        q = q.filter(sessions::device_key.eq(dk.to_string()));
    }
    q.select(Session::as_select())
        .order(sessions::last_event_at.desc())
        .limit(limit)
        .offset(offset)
        .load(conn)
        .await
}

/// A session outside `scope` returns `None`, not the row — the handler turns
/// that into a 404 (fail narrow). `sessions` is the only one of these four
/// tables with a `UNIQUE (app_id, session_id)` constraint, and `bump_session`
/// lets `environment_id` flip to the most recent non-null value on conflict
/// (see its own doc comment), so the session's own label cannot be trusted to
/// disambiguate its children — this function's scope check exists
/// independently of theirs, not as a shortcut for them.
pub async fn get_session(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    session_id: &str,
) -> QueryResult<Option<Session>> {
    let q = sessions::table
        .filter(sessions::app_id.eq(scope.app_id))
        .filter(sessions::session_id.eq(session_id.to_string()))
        .into_boxed();
    crate::scope_env!(q, sessions, &scope.env)
        .select(Session::as_select())
        .first(conn)
        .await
        .optional()
}

/// `analytics_events.session_id` is nullable free text with no uniqueness and
/// no environment linkage — unlike `sessions`, a session's own environment
/// label does not disambiguate which environment its child rows belong to
/// (e.g. a device repointed from staging to prod without a fresh session id).
/// The environment predicate is applied here directly rather than inherited
/// from the session.
pub async fn events_for_session(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    session_id: &str,
    limit: i64,
) -> QueryResult<Vec<AnalyticsEvent>> {
    let q = analytics_events::table
        .filter(analytics_events::app_id.eq(scope.app_id))
        .filter(analytics_events::session_id.eq(session_id.to_string()))
        .into_boxed();
    crate::scope_env!(q, analytics_events, &scope.env)
        .select(AnalyticsEvent::as_select())
        .order(analytics_events::occurred_at.asc())
        .limit(limit)
        .load(conn)
        .await
}

/// See [`events_for_session`]'s doc comment — same reasoning, `error_events`.
pub async fn errors_for_session(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    session_id: &str,
    limit: i64,
) -> QueryResult<Vec<ErrorEvent>> {
    let q = error_events::table
        .filter(error_events::app_id.eq(scope.app_id))
        .filter(error_events::session_id.eq(session_id.to_string()))
        .into_boxed();
    crate::scope_env!(q, error_events, &scope.env)
        .select(ErrorEvent::as_select())
        .order(error_events::occurred_at.asc())
        .limit(limit)
        .load(conn)
        .await
}

/// See [`events_for_session`]'s doc comment — same reasoning, `transactions`.
pub async fn transactions_for_session(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    session_id: &str,
    limit: i64,
) -> QueryResult<Vec<Transaction>> {
    let q = transactions::table
        .filter(transactions::app_id.eq(scope.app_id))
        .filter(transactions::session_id.eq(session_id.to_string()))
        .into_boxed();
    crate::scope_env!(q, transactions, &scope.env)
        .select(Transaction::as_select())
        .order(transactions::occurred_at.asc())
        .limit(limit)
        .load(conn)
        .await
}

// ===========================================================================
// Devices (inventory + per-device errors)
// ===========================================================================

/// F4 (final whole-branch review, `.superpowers/sdd/s2-final-review.md`):
/// `events_count`/`errors_count`/`sessions_count` were already
/// environment-scoped (Task 8); `first_seen`/`last_seen`/`last_distinct_id`
/// are now derived per-environment too, under `One`/`Unattributed` — see
/// [`list_devices`]'s doc comment for how. Under `EnvFilter::All` all three
/// still read the stored `devices` row directly (the durable fast path,
/// unchanged).
///
/// `last_distinct_id` was the concrete disclosure vector F4 named: a device
/// whose most recent identity is a production-only user must not surface
/// that identity under a staging scope, because `bump_device`'s
/// `last_distinct_id` column folds every environment's writes into one
/// app-wide value with no notion of "as of this environment".
///
/// `family`/`model`/`os_name`/`os_version`/`arch`/`browser` are, like
/// `PersonRow::properties`, deliberately left app-wide and undocumented no
/// longer — a physical device has one descriptor, not one per environment
/// it happens to report telemetry from, so there is no per-environment
/// reading to derive these from any more than there is for a person's
/// property bag.
#[derive(Debug, QueryableByName, serde::Serialize)]
pub struct DeviceRow {
    #[diesel(sql_type = SqlUuid)]
    pub id: Uuid,
    #[diesel(sql_type = Text)]
    pub device_key: String,
    #[diesel(sql_type = Nullable<Text>)]
    pub family: Option<String>,
    #[diesel(sql_type = Nullable<Text>)]
    pub model: Option<String>,
    #[diesel(sql_type = Nullable<Text>)]
    pub os_name: Option<String>,
    #[diesel(sql_type = Nullable<Text>)]
    pub os_version: Option<String>,
    #[diesel(sql_type = Nullable<Text>)]
    pub arch: Option<String>,
    #[diesel(sql_type = Nullable<Text>)]
    pub browser: Option<String>,
    #[diesel(sql_type = Nullable<Text>)]
    pub last_distinct_id: Option<String>,
    #[diesel(sql_type = Timestamptz)]
    pub first_seen: DateTime<Utc>,
    #[diesel(sql_type = Timestamptz)]
    pub last_seen: DateTime<Utc>,
    #[diesel(sql_type = BigInt)]
    pub events_count: i64,
    #[diesel(sql_type = BigInt)]
    pub errors_count: i64,
    #[diesel(sql_type = BigInt)]
    pub sessions_count: i64,
}

/// The `distinct_id` of the most recent (by time) signal in the selected
/// environment, across all three tables that carry one — used by
/// [`list_devices`]/[`get_device`] to derive `last_distinct_id` per
/// environment instead of reading `devices.last_distinct_id` directly (see
/// `DeviceRow`'s doc comment for why that column is the disclosure vector F4
/// named: `bump_device`'s `COALESCE(EXCLUDED.last_distinct_id,
/// devices.last_distinct_id)` folds every environment's writes into one
/// app-wide value, last-write-wins, with no per-environment reading at all).
///
/// A `distinct_id IS NOT NULL` guard on the two nullable-`distinct_id` legs
/// (`error_events`, `sessions` — `analytics_events.distinct_id` is `NOT
/// NULL` in the schema, so it needs none) mirrors `bump_device`'s own
/// `COALESCE(EXCLUDED.last_distinct_id, devices.last_distinct_id)`: a NULL
/// write never overwrites a known identity, so an anonymous event must never
/// win over an identified one that is merely slightly older.
///
/// Aliased `lae`/`lee`/`lse` rather than reusing `ae`/`ee`/`se` — the names
/// the sibling count/min/max LATERALs already use in the same query. Postgres
/// scopes them correctly either way (each is local to its own subquery), but
/// a human skimming the SQL text next to those siblings should not have to
/// check.
fn device_last_distinct_id_join(env: EnvFilter, bind_index: usize) -> String {
    let ae_env = env.sql_fragment_for("lae", bind_index);
    let ee_env = env.sql_fragment_for("lee", bind_index);
    let se_env = env.sql_fragment_for("lse", bind_index);
    format!(
        " LEFT JOIN LATERAL ( \
             SELECT distinct_id FROM ( \
                 SELECT distinct_id, occurred_at FROM analytics_events lae \
                 WHERE lae.app_id = $1 AND lae.device_key = d.device_key{ae_env} \
                 UNION ALL \
                 SELECT distinct_id, occurred_at FROM error_events lee \
                 WHERE lee.app_id = $1 AND lee.device_key = d.device_key \
                   AND lee.distinct_id IS NOT NULL{ee_env} \
                 UNION ALL \
                 SELECT distinct_id, last_event_at AS occurred_at FROM sessions lse \
                 WHERE lse.app_id = $1 AND lse.device_key = d.device_key \
                   AND lse.distinct_id IS NOT NULL{se_env} \
             ) recent \
             ORDER BY occurred_at DESC LIMIT 1 \
         ) ld ON TRUE"
    )
}

pub async fn list_devices(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    since: DateTime<Utc>,
    limit: i64,
    offset: i64,
    search: Option<&str>,
) -> QueryResult<Vec<DeviceRow>> {
    // Escape LIKE metacharacters: an unescaped `%`/`_` makes a literal search
    // term match the wrong rows, and a pattern of many wildcards makes ILIKE
    // matching super-linear per scanned row.
    let pattern = search.map(like_contains).unwrap_or_else(|| "%".to_string());

    // $1 app_id, $2 since, $3 pattern, $4 limit, $5 offset — env takes $6 when
    // it needs a bind, reused across the count LATERALs that are actually
    // emitted (see the `counts_select`/`counts_join` comment below — `events`/
    // `errors` only under `One`/`Unattributed`, `sessions` always) and the
    // membership `EXISTS` (only emitted when `scope.env != All`). Same idiom
    // as `list_persons`.
    let env_sql = scope.env.sql_fragment(6);

    // See `list_persons`' doc comment: this is a WHERE-clause predicate on the
    // paging subquery, not a join, so it does not disturb where LIMIT is
    // applied. Omitted entirely under `All` — same reasoning as `list_persons`.
    //
    // Each leg aliases its subquery and qualifies the correlated column with
    // that alias (`ae.device_key`, not bare `device_key`). Demonstrated live
    // during review: with no alias, an unqualified name that happens to also
    // exist on the inner table resolves there only by luck — if a future copy
    // of this pattern targets a table with no `device_key` column, Postgres
    // silently binds the bare name to the *outer* `devices` row instead,
    // collapsing the whole `EXISTS` into `devices.device_key =
    // devices.device_key` (always true, no error). Qualifying turns that
    // mistake into a hard query error instead.
    //
    // The sessions leg also carries `started_at >= $2`, matching the `se`
    // LATERAL below (and matching the `se` LATERAL's own pre-existing time
    // bound, which predates this task). Without it, a device whose only
    // env_a session is older than `since` — but whose `devices.last_seen` is
    // recent from unrelated env_b activity — would still pass membership and
    // render an all-zero row under `One(env_a)`, the exact bug this filter
    // exists to prevent.
    let membership_sql = if matches!(scope.env, EnvFilter::All) {
        String::new()
    } else {
        let ae_env = scope.env.sql_fragment_for("ae", 6);
        let ee_env = scope.env.sql_fragment_for("ee", 6);
        let se_env = scope.env.sql_fragment_for("se", 6);
        format!(
            " AND ( \
                EXISTS (SELECT 1 FROM analytics_events ae WHERE ae.app_id=$1 AND ae.device_key = devices.device_key{ae_env}) \
                OR EXISTS (SELECT 1 FROM error_events ee WHERE ee.app_id=$1 AND ee.device_key = devices.device_key{ee_env}) \
                OR EXISTS (SELECT 1 FROM sessions se WHERE se.app_id=$1 AND se.device_key = devices.device_key AND se.started_at >= $2{se_env}) \
              )"
        )
    };

    // `devices.events_count`/`errors_count` are lifetime counters that
    // `bump_device` increments on every event regardless of environment —
    // durable, because `devices` is never partitioned and never dropped.
    // `analytics_events`/`error_events` ARE partitioned by `sauron-tier`
    // (`bins/sauron-tier/src/main.rs`), which exports aged partitions (past
    // `TIER_HOT_DAYS`, default 30 days) to Parquet and then drops them from
    // Postgres. The `ae`/`ee` LATERALs below can only see rows still in
    // Postgres, so for a device whose activity has aged out of the hot window
    // they under-report — all the way down to 0 for a device with a real,
    // large lifetime count. This is the same tiering blind spot the design
    // doc records for per-environment issue counts (see "No new table" in
    // `docs/superpowers/specs/2026-07-28-environment-scoped-reads-design.md`:
    // `issues.times_seen` vs. a per-environment LATERAL over `error_events`)
    // — the scoped count cannot see tiered data, and that is accepted rather
    // than solved here.
    //
    // So `All` — "every environment, all time" — reads the durable columns
    // directly, no join, no subquery, matching that design's precedent for
    // `All`. `One`/`Unattributed` have no alternative but the LATERALs: they
    // are the only thing that *can* be scoped to a single environment,
    // tiering blind spot and all. `sessions_count` has no durable column to
    // fall back to (`devices` was never denormalized for it, and `sessions`
    // itself is not one of `sauron-tier`'s tiered tables), so it stays a
    // LATERAL under every variant, exactly as it already was before this
    // task — do not read this as an oversight; the two fields are computed
    // differently on purpose.
    //
    // F4: `first_seen`/`last_seen`/`last_distinct_id` follow the identical
    // `All`-vs-scoped split, folded into this same variable rather than a
    // parallel one — under `All` they read straight off `d`; under
    // `One`/`Unattributed` they extend the very `ae`/`ee` LATERALs this
    // fixes' counts already join, adding `min`/`max(occurred_at)`, plus
    // [`device_last_distinct_id_join`] for `last_distinct_id` (see its own
    // doc comment). `LEAST`/`GREATEST` ignore `NULL` arguments (Postgres's
    // documented behaviour), so a device that qualifies via only one of
    // `ae`/`ee`/`se` (e.g. `session_only_device_key`, sessions alone) still
    // gets a real value from the other two `NULL` legs.
    let (scoped_select, scoped_join) = if matches!(scope.env, EnvFilter::All) {
        (
            "d.events_count AS events_count, d.errors_count AS errors_count, \
             d.first_seen AS first_seen, d.last_seen AS last_seen, \
             d.last_distinct_id AS last_distinct_id"
                .to_string(),
            String::new(),
        )
    } else {
        // `.clone()`, not a move: `env_sql`/the final `bind_env!` call below both
        // still need `scope.env` after this — `device_last_distinct_id_join` keeps
        // its pre-existing owned-`EnvFilter` signature (not reshaped to take `&`),
        // per the rule of adding a clone at the call site instead.
        let ld_join = device_last_distinct_id_join(scope.env.clone(), 6);
        (
            "COALESCE(ae.cnt, 0)::bigint AS events_count, \
             COALESCE(ee.cnt, 0)::bigint AS errors_count, \
             LEAST(ae.min_occurred, ee.min_occurred, se.min_started) AS first_seen, \
             GREATEST(ae.max_occurred, ee.max_occurred, se.max_last_event) AS last_seen, \
             ld.distinct_id AS last_distinct_id"
                .to_string(),
            format!(
                " LEFT JOIN LATERAL ( \
                     SELECT count(*) AS cnt, min(occurred_at) AS min_occurred, \
                            max(occurred_at) AS max_occurred FROM analytics_events \
                     WHERE app_id = $1 AND device_key = d.device_key{env_sql} \
                 ) ae ON TRUE \
                 LEFT JOIN LATERAL ( \
                     SELECT count(*) AS cnt, min(occurred_at) AS min_occurred, \
                            max(occurred_at) AS max_occurred FROM error_events \
                     WHERE app_id = $1 AND device_key = d.device_key{env_sql} \
                 ) ee ON TRUE{ld_join}"
            ),
        )
    };

    // Page FIRST, then count per returned device via LATERAL subqueries — same
    // reasoning as `list_persons` (Postgres cannot push the outer LIMIT into a
    // grouped subquery).
    //
    // The `se` LATERAL's `since` bound moved from a `WHERE` clause to a
    // `count(*) FILTER (...)` — F4 needs `min(started_at)`/`max(last_event_at)`
    // over *all* of this device's env-scoped sessions, not just the ones
    // after `since` (a device's true per-environment `first_seen` can predate
    // the page's window; `since` only decides which devices are listed, via
    // the outer `WHERE ... last_seen >= $2`, unchanged). Filtering only the
    // count aggregate is equivalent to the old `WHERE started_at >= $2` for
    // `cnt` specifically (same rows excluded, same count), while leaving the
    // two new aggregates unbounded.
    let q = format!(
        "SELECT d.id, d.device_key, d.family, d.model, d.os_name, d.os_version, d.arch, \
                d.browser, \
                {scoped_select}, \
                COALESCE(se.cnt, 0)::bigint AS sessions_count \
         FROM ( \
             SELECT * FROM devices \
             WHERE app_id = $1 AND last_seen >= $2 \
               AND (COALESCE(family,'') || ' ' || COALESCE(model,'') || ' ' || \
                    COALESCE(os_name,'') || ' ' || COALESCE(device_key,'')) ILIKE $3{membership_sql} \
             ORDER BY last_seen DESC LIMIT $4 OFFSET $5 \
         ) d{scoped_join} \
         LEFT JOIN LATERAL ( \
             SELECT count(*) FILTER (WHERE started_at >= $2) AS cnt, \
                    min(started_at) AS min_started, max(last_event_at) AS max_last_event \
             FROM sessions \
             WHERE app_id = $1 AND device_key = d.device_key{env_sql} \
         ) se ON TRUE \
         ORDER BY d.last_seen DESC"
    );
    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Timestamptz, _>(since)
        .bind::<Text, _>(pattern)
        .bind::<BigInt, _>(limit)
        .bind::<BigInt, _>(offset);
    stmt = crate::bind_env!(stmt, &scope.env);
    stmt.get_results(conn).await
}

/// `devices` carries no `environment_id`, so membership is derived the same
/// way [`list_devices`]' membership `EXISTS` derives it — activity in
/// `analytics_events`/`error_events`/`sessions`, keyed by `device_key`.
/// Omitted under `All`, same reasoning.
///
/// Returns [`DeviceRow`], not the raw [`Device`] model, and is raw SQL rather
/// than the diesel query builder `list_devices` used to be — both follow from
/// the same fact: `events_count`/`errors_count` need a different source
/// depending on `scope.env` (the durable `devices` columns under `All`, an
/// environment-scoped LATERAL under `One`/`Unattributed` — see `list_devices`'
/// doc comment for the full tiering reasoning this works around), and
/// `Device` has no way to carry two different answers for the same field
/// depending on scope; diesel's query builder has no easy way to switch a
/// selected column's source per branch either. Before this change the Device
/// Detail page (`bins/sauron-api/src/routes/devices.rs`'s `DeviceDetail`)
/// rendered `Device`'s raw, cross-environment, all-time counters directly
/// above a sessions/errors/performance list that Task 8 *did* scope — a
/// device viewed under `One(staging)` would show prod+staging all-time totals
/// above a handful of staging-only rows. That is the bug this function exists
/// to not have.
pub async fn get_device(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    device_key: &str,
) -> QueryResult<Option<DeviceRow>> {
    let env_sql = scope.env.sql_fragment(3);

    // See `list_devices`' membership `EXISTS` doc comment: each leg is
    // aliased and the correlated column qualified with that alias, so an
    // unqualified name colliding with the outer `devices` row is a hard query
    // error rather than a silent always-true tautology. No `started_at` bound
    // on the sessions leg — unlike `list_devices`, this function has no
    // `since` parameter to bound it against; a single-identity lookup has no
    // notion of a page's time window.
    let membership_sql = if matches!(scope.env, EnvFilter::All) {
        String::new()
    } else {
        let ae_env = scope.env.sql_fragment_for("ae", 3);
        let ee_env = scope.env.sql_fragment_for("ee", 3);
        let se_env = scope.env.sql_fragment_for("se", 3);
        format!(
            " AND ( \
                EXISTS (SELECT 1 FROM analytics_events ae WHERE ae.app_id=$1 AND ae.device_key = devices.device_key{ae_env}) \
                OR EXISTS (SELECT 1 FROM error_events ee WHERE ee.app_id=$1 AND ee.device_key = devices.device_key{ee_env}) \
                OR EXISTS (SELECT 1 FROM sessions se WHERE se.app_id=$1 AND se.device_key = devices.device_key{se_env}) \
              )"
        )
    };

    // Same `All`-vs-scoped source split as `list_devices` — see that
    // function's doc comment for the full reasoning, including why
    // `first_seen`/`last_seen`/`last_distinct_id` (F4) join the same split.
    // No `since` bound anywhere in this function (single-identity lookup, no
    // page window), so — unlike `list_devices` — the `se` LATERAL needs no
    // `FILTER` trick: its `min`/`max` were already unbounded.
    let (scoped_select, scoped_join) = if matches!(scope.env, EnvFilter::All) {
        (
            "d.events_count AS events_count, d.errors_count AS errors_count, \
             d.first_seen AS first_seen, d.last_seen AS last_seen, \
             d.last_distinct_id AS last_distinct_id"
                .to_string(),
            String::new(),
        )
    } else {
        // `.clone()`, not a move — see `list_devices`' identical call for why.
        let ld_join = device_last_distinct_id_join(scope.env.clone(), 3);
        (
            "COALESCE(ae.cnt, 0)::bigint AS events_count, \
             COALESCE(ee.cnt, 0)::bigint AS errors_count, \
             LEAST(ae.min_occurred, ee.min_occurred, se.min_started) AS first_seen, \
             GREATEST(ae.max_occurred, ee.max_occurred, se.max_last_event) AS last_seen, \
             ld.distinct_id AS last_distinct_id"
                .to_string(),
            format!(
                " LEFT JOIN LATERAL ( \
                     SELECT count(*) AS cnt, min(occurred_at) AS min_occurred, \
                            max(occurred_at) AS max_occurred FROM analytics_events \
                     WHERE app_id = $1 AND device_key = d.device_key{env_sql} \
                 ) ae ON TRUE \
                 LEFT JOIN LATERAL ( \
                     SELECT count(*) AS cnt, min(occurred_at) AS min_occurred, \
                            max(occurred_at) AS max_occurred FROM error_events \
                     WHERE app_id = $1 AND device_key = d.device_key{env_sql} \
                 ) ee ON TRUE{ld_join}"
            ),
        )
    };

    let q = format!(
        "SELECT d.id, d.device_key, d.family, d.model, d.os_name, d.os_version, d.arch, \
                d.browser, \
                {scoped_select}, \
                COALESCE(se.cnt, 0)::bigint AS sessions_count \
         FROM ( \
             SELECT * FROM devices \
             WHERE app_id = $1 AND device_key = $2{membership_sql} \
         ) d{scoped_join} \
         LEFT JOIN LATERAL ( \
             SELECT count(*) AS cnt, min(started_at) AS min_started, \
                    max(last_event_at) AS max_last_event FROM sessions \
             WHERE app_id = $1 AND device_key = d.device_key{env_sql} \
         ) se ON TRUE"
    );
    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Text, _>(device_key.to_string());
    stmt = crate::bind_env!(stmt, &scope.env);
    stmt.get_result(conn).await.optional()
}

/// `error_events` carries its own `environment_id` directly, so this is an
/// ordinary `scope_env!` filter — unlike `get_device`, which has to derive
/// membership because `devices` itself carries none.
pub async fn errors_for_device(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    device_key: &str,
    limit: i64,
) -> QueryResult<Vec<ErrorEvent>> {
    let q = error_events::table
        .filter(error_events::app_id.eq(scope.app_id))
        .filter(error_events::device_key.eq(device_key.to_string()))
        .into_boxed();
    crate::scope_env!(q, error_events, &scope.env)
        .select(ErrorEvent::as_select())
        .order(error_events::occurred_at.desc())
        .limit(limit)
        .load(conn)
        .await
}

// ===========================================================================
// Persons (Users Explorer — event_user + activity counts)
// ===========================================================================

/// F4 (final whole-branch review, `.superpowers/sdd/s2-final-review.md`):
/// `events_count`/`errors_count`/`sessions_count` were already environment-scoped
/// (Task 8); `first_seen`/`last_seen` are now derived per-environment too — see
/// [`list_persons`]'s doc comment for how (the same `ae`/`ee`/`se` LATERALs that
/// already compute the three counts, extended with `min`/`max(occurred_at)`).
/// Under `EnvFilter::All` they still read `event_users.first_seen`/`last_seen`
/// directly — the durable fast path, unchanged.
///
/// `properties` is the one field on this struct that is **not** derived, and
/// that is a decision, not an oversight. `event_users` carries no
/// `environment_id` at all, and a person has exactly one property bag — unlike
/// `first_seen`/`last_seen` (a `min`/`max` over a set of per-environment rows),
/// there is no per-environment *copy* of `properties` to fall back to; the
/// value either is app-wide or does not exist. Membership already gates
/// whether this row is visible at all (see the membership `EXISTS` below): a
/// person only appears because they have real activity in the selected
/// environment, so showing their one property bag is showing the properties
/// of someone the caller is legitimately looking at, not a cross-environment
/// leak the way a *different* person's `last_distinct_id` on someone else's
/// device would be (see `DeviceRow`). Slice 3, where environment becomes an
/// access boundary rather than a read-scoping dimension, should make this
/// choice explicitly — does a property bag stay visible to a caller scoped to
/// an environment the person merely also happens to appear in, or should
/// `properties` require broader access? — rather than inherit it silently.
#[derive(Debug, QueryableByName, serde::Serialize)]
pub struct PersonRow {
    #[diesel(sql_type = Text)]
    pub distinct_id: String,
    #[diesel(sql_type = Jsonb)]
    pub properties: Value,
    #[diesel(sql_type = Timestamptz)]
    pub first_seen: DateTime<Utc>,
    #[diesel(sql_type = Timestamptz)]
    pub last_seen: DateTime<Utc>,
    #[diesel(sql_type = BigInt)]
    pub events_count: i64,
    #[diesel(sql_type = BigInt)]
    pub errors_count: i64,
    #[diesel(sql_type = BigInt)]
    pub sessions_count: i64,
}

pub async fn list_persons(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    search: Option<&str>,
    limit: i64,
    offset: i64,
) -> QueryResult<Vec<PersonRow>> {
    // Escape LIKE metacharacters: an unescaped `%`/`_` makes a literal search
    // term match the wrong rows, and a pattern of many wildcards makes ILIKE
    // matching super-linear per scanned row.
    let pattern = search.map(like_contains).unwrap_or_else(|| "%".to_string());

    // $1 app_id, $2 pattern, $3 limit, $4 offset — env takes $5 when it needs a
    // bind, reused across the three count LATERALs (always emitted, `""` under
    // `All`) and the membership `EXISTS` (only emitted when `scope.env != All`).
    // Same "one bind, several textual occurrences" idiom as `user_stats`' `$3`.
    //
    // Unlike `list_devices` (see that function's doc comment), `events_count`/
    // `errors_count` here have no `All`-only fast path onto a durable column:
    // `event_users` was never denormalized with lifetime counters the way
    // `devices` was — checked directly against `EventUser`'s fields, not
    // assumed — so every variant, including `All`, reads these two LATERALs
    // unconditionally. That also means `list_persons` carries none of
    // `list_devices`' tiering blind spot under `All`; it already had it, and
    // still has it, under every scope.
    let env_sql = scope.env.sql_fragment(5);

    // `event_users` carries no `environment_id` at all, so a person's
    // membership in a specific environment can only be derived from whether
    // they have any row in one of the three tables that do carry it. This is a
    // WHERE-clause predicate on the *inner* paging subquery (not a join), so it
    // does not disturb where LIMIT is applied — see the paging comment below.
    // Omitted entirely under `All`: every `event_users` row exists only because
    // `note_identity` registered it from a real analytics/error event, so an
    // unfiltered `EXISTS` would add three subquery lookups per candidate row
    // for no narrowing effect.
    //
    // Each leg aliases its subquery and qualifies the correlated column with
    // that alias (`ae.distinct_id`, not bare `distinct_id`) — see
    // `list_devices`' membership `EXISTS` doc comment for why an unqualified
    // name is a live footgun, not a style nit.
    let membership_sql = if matches!(scope.env, EnvFilter::All) {
        String::new()
    } else {
        let ae_env = scope.env.sql_fragment_for("ae", 5);
        let ee_env = scope.env.sql_fragment_for("ee", 5);
        let se_env = scope.env.sql_fragment_for("se", 5);
        format!(
            " AND ( \
                EXISTS (SELECT 1 FROM analytics_events ae WHERE ae.app_id=$1 AND ae.distinct_id = event_users.distinct_id{ae_env}) \
                OR EXISTS (SELECT 1 FROM error_events ee WHERE ee.app_id=$1 AND ee.distinct_id = event_users.distinct_id{ee_env}) \
                OR EXISTS (SELECT 1 FROM sessions se WHERE se.app_id=$1 AND se.distinct_id = event_users.distinct_id{se_env}) \
              )"
        )
    };

    // Page FIRST, then count per returned person via LATERAL subqueries.
    //
    // The previous form used three grouped subqueries over analytics_events,
    // error_events and sessions filtered only by app_id. Postgres cannot push
    // the outer LIMIT into a GROUP BY subquery, so every page load aggregated
    // the app's entire history across the two largest tables and then discarded
    // all but ~50 rows. Counting per-page turns that into a handful of
    // index lookups on (app_id, distinct_id). Adding `membership_sql` above
    // preserves this shape — it narrows the *same* inner subquery's WHERE
    // clause, before LIMIT/OFFSET are applied, rather than adding a join stage
    // — confirmed with `EXPLAIN`, see the task report.
    //
    // F4: `ae`/`ee`/`se` also compute `min`/`max(occurred_at)` now (sessions'
    // own analogue is `started_at`/`last_event_at` — it has no single
    // `occurred_at` column), extending the same three LATERALs rather than
    // adding a fourth. `first_seen`/`last_seen` under `All` still read
    // `eu.first_seen`/`eu.last_seen` directly (the durable fast path,
    // unaffected by this fix); under `One`/`Unattributed` they are
    // `LEAST`/`GREATEST` over the three per-source extrema. Postgres's
    // `LEAST`/`GREATEST` skip `NULL` arguments (documented behaviour, not an
    // assumption) rather than propagating them, so a person who qualifies via
    // only one of the three tables (e.g. `session_only_distinct_id`, sessions
    // alone) still gets a real value out of the other two `NULL` legs instead
    // of `NULL` itself — membership already guarantees at least one leg is
    // non-null for any row that reaches this point.
    let seen_select = if matches!(scope.env, EnvFilter::All) {
        "eu.first_seen AS first_seen, eu.last_seen AS last_seen".to_string()
    } else {
        "LEAST(ae.min_occurred, ee.min_occurred, se.min_started) AS first_seen, \
         GREATEST(ae.max_occurred, ee.max_occurred, se.max_last_event) AS last_seen"
            .to_string()
    };
    let q = format!(
        "SELECT eu.distinct_id, eu.properties, {seen_select}, \
                COALESCE(ae.cnt,0)::bigint AS events_count, \
                COALESCE(ee.cnt,0)::bigint AS errors_count, \
                COALESCE(se.cnt,0)::bigint AS sessions_count \
         FROM ( \
             SELECT distinct_id, properties, first_seen, last_seen FROM event_users \
             WHERE app_id=$1 AND (distinct_id ILIKE $2 OR properties::text ILIKE $2){membership_sql} \
             ORDER BY last_seen DESC LIMIT $3 OFFSET $4 \
         ) eu \
         LEFT JOIN LATERAL (SELECT count(*) cnt, min(occurred_at) min_occurred, \
                    max(occurred_at) max_occurred FROM analytics_events \
                    WHERE app_id=$1 AND distinct_id = eu.distinct_id{env_sql}) ae ON TRUE \
         LEFT JOIN LATERAL (SELECT count(*) cnt, min(occurred_at) min_occurred, \
                    max(occurred_at) max_occurred FROM error_events \
                    WHERE app_id=$1 AND distinct_id = eu.distinct_id{env_sql}) ee ON TRUE \
         LEFT JOIN LATERAL (SELECT count(*) cnt, min(started_at) min_started, \
                    max(last_event_at) max_last_event FROM sessions \
                    WHERE app_id=$1 AND distinct_id = eu.distinct_id{env_sql}) se ON TRUE \
         ORDER BY eu.last_seen DESC"
    );
    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Text, _>(pattern)
        .bind::<BigInt, _>(limit)
        .bind::<BigInt, _>(offset);
    stmt = crate::bind_env!(stmt, &scope.env);
    stmt.get_results(conn).await
}

// ===========================================================================
// Overview (composite health snapshot)
// ===========================================================================

#[derive(Debug, QueryableByName, serde::Serialize)]
pub struct OverviewTotals {
    #[diesel(sql_type = BigInt)]
    pub events: i64,
    #[diesel(sql_type = BigInt)]
    pub errors: i64,
    #[diesel(sql_type = BigInt)]
    pub sessions: i64,
    #[diesel(sql_type = BigInt)]
    pub users: i64,
    #[diesel(sql_type = BigInt)]
    pub new_users: i64,
    #[diesel(sql_type = BigInt)]
    pub crashed_sessions: i64,
}

/// `event_users` carries no `environment_id` column, so membership in a specific environment
/// can only be derived from whether an identity has any row in one of the three signal
/// tables that do carry it — the same `EXISTS`-over-three-tables idiom `list_persons` uses
/// for its own membership filter (see that function's doc comment for the full reasoning;
/// this is a shared helper rather than a third copy of the same fragment, one per caller
/// below).
///
/// Every one of this fragment's callers has `app_id` bound at `$1` (checked at each call
/// site, not assumed generically), so the three `EXISTS` legs hardcode `$1` rather than
/// taking it as a parameter. `bind_index` is the *environment*'s bind — reused verbatim from
/// the caller's own `env_sql`, since it is the identical value, not a second bind
/// (`list_persons`' "one bind, many textual occurrences" idiom).
///
/// Returns `""` under `EnvFilter::All`, omitting the `EXISTS` entirely rather than narrowing
/// it to a tautology: every `event_users` row exists only because some event registered it,
/// so an unfiltered membership check would add three subquery lookups per row for no
/// narrowing effect.
///
/// Each leg aliases its subquery and qualifies the correlated column with that alias
/// (`ae.distinct_id`, not bare `distinct_id`) — an unqualified name that happens to also
/// exist on the outer table resolves there silently instead of erroring, turning the
/// predicate into an always-true tautology with no query error to catch it. Demonstrated
/// live during Task 8's review; see `list_persons`' doc comment.
fn event_user_membership_exists(env: EnvFilter, bind_index: usize) -> String {
    if matches!(env, EnvFilter::All) {
        return String::new();
    }
    let ae_env = env.sql_fragment_for("ae", bind_index);
    let ee_env = env.sql_fragment_for("ee", bind_index);
    let se_env = env.sql_fragment_for("se", bind_index);
    format!(
        " AND ( \
            EXISTS (SELECT 1 FROM analytics_events ae WHERE ae.app_id=$1 AND ae.distinct_id = event_users.distinct_id{ae_env}) \
            OR EXISTS (SELECT 1 FROM error_events ee WHERE ee.app_id=$1 AND ee.distinct_id = event_users.distinct_id{ee_env}) \
            OR EXISTS (SELECT 1 FROM sessions se WHERE se.app_id=$1 AND se.distinct_id = event_users.distinct_id{se_env}) \
          )"
    )
}

pub async fn overview_totals(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    since: DateTime<Utc>,
) -> QueryResult<OverviewTotals> {
    // $1 app_id, $2 since, reused across all six sub-selects (as before). Env takes $3 when
    // it needs a bind, reused across the four sub-selects whose table actually carries
    // `environment_id` (analytics_events, error_events, sessions x2) AND, as of this fix,
    // across `users`/`new_users`' membership `EXISTS` legs below — same environment, same
    // bind, not a second one. Each analytics/error/sessions sub-select is un-aliased (no
    // join), so `sql_fragment_for` is passed the table's own name rather than a shortened
    // alias — purely for self-documentation, since a bare `sql_fragment` would emit
    // identical SQL here.
    let env_sql_analytics = scope.env.sql_fragment_for("analytics_events", 3);
    let env_sql_errors = scope.env.sql_fragment_for("error_events", 3);
    let env_sql_sessions = scope.env.sql_fragment_for("sessions", 3);

    // `users`/`new_users` read `event_users`, which carries no `environment_id` — scoped by
    // membership (see `event_user_membership_exists`'s doc comment), the gap Task 8 deferred
    // and this fix closes.
    //
    // `new_users` keeps its existing `first_seen>=$2` predicate — "globally-first-seen in
    // the window" — and ANDs membership onto it. This is reading (a) from the two documented
    // in this fix's spec: "globally-first-seen in the window AND has activity in this
    // environment", not reading (b) ("first activity *in this environment* falls in the
    // window", which needs a per-(distinct_id, environment) `min(occurred_at)` derived from
    // the three signal tables — materially more expensive, and not what `list_persons`/
    // `user_stats`/`active_user_series` do). Taken for consistency with those three.
    // Consequence: a user who first appeared in production last year and reached staging
    // today counts as "new" in *neither* environment's window under this reading — their
    // global `first_seen` predates `since` regardless of which environment's membership is
    // checked.
    // `.clone()`, not a move: the final `bind_env!` call below still needs
    // `scope.env` — `event_user_membership_exists` keeps its pre-existing
    // owned-`EnvFilter` signature (not reshaped to take `&`), per the rule of
    // adding a clone at the call site instead.
    let membership_sql = event_user_membership_exists(scope.env.clone(), 3);

    // `crashed_sessions` trusts `sessions.errors_count`/`environment_id` directly —
    // known to be able to mislabel which environment a crash counts against (not
    // just over/under by a fixed amount). See `bump_session`'s doc comment for the
    // mechanism and `.superpowers/sdd/2026-07-29-environment-rbac-scope/
    // task-10-report.md` for why the `EXISTS`-against-`error_events` fix was
    // measured and declined rather than shipped.
    let q = format!(
        "SELECT \
           (SELECT count(*) FROM analytics_events WHERE app_id=$1 AND occurred_at>=$2{env_sql_analytics})::bigint AS events, \
           (SELECT count(*) FROM error_events WHERE app_id=$1 AND occurred_at>=$2{env_sql_errors})::bigint AS errors, \
           (SELECT count(*) FROM sessions WHERE app_id=$1 AND last_event_at>=$2{env_sql_sessions})::bigint AS sessions, \
           (SELECT count(*) FROM event_users WHERE app_id=$1 AND last_seen>=$2{membership_sql})::bigint AS users, \
           (SELECT count(*) FROM event_users WHERE app_id=$1 AND first_seen>=$2{membership_sql})::bigint AS new_users, \
           (SELECT count(*) FROM sessions WHERE app_id=$1 AND last_event_at>=$2 AND errors_count>0{env_sql_sessions})::bigint AS crashed_sessions"
    );
    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Timestamptz, _>(since);
    stmt = crate::bind_env!(stmt, &scope.env);
    stmt.get_result(conn).await
}

/// Same derivation as [`list_issues`] under `One`/`Unattributed` — see its
/// doc comment for the full reasoning (membership via inner-join LATERAL +
/// `HAVING`, `since` pushed into the LATERAL's own bound rather than only
/// checked against the result afterward). No filters/`q`/`offset` here, so
/// the bind layout is fixed: $1 app_id, $2 since, $3 limit, $4 env (only
/// under `One`; last, so `Unattributed` leaves no gap — unlike
/// `list_issues`, nothing here needs `env` allocated early, since there are
/// no filter/tag/`q` fragments to share it with).
///
/// Unlike `list_issues`, the candidate set cannot be paged by `LIMIT` before
/// the join runs: the whole point of "top issues" is *ranking by the
/// per-environment count*, which does not exist until after the LATERAL
/// computes it. So the paging subquery only pre-filters — `app_id`,
/// `last_seen >= since` (a sound bound: the derived, windowed `last_seen`
/// can never exceed the issue's own app-wide `last_seen`, so this can only
/// drop rows the outer `WHERE` would have dropped anyway), and environment
/// membership via the same `EXISTS` `list_issues` uses. The LATERAL then
/// computes every surviving candidate's derived `times_seen`, and `ORDER BY
/// agg.times_seen DESC LIMIT $3` ranks and pages *after* that.
///
/// This replaces the previous shape, which paged `ORDER BY i.times_seen DESC
/// LIMIT $3` — the issue's own *app-wide* count — before the join, then
/// relabelled the page with `agg.times_seen` for display. That made the
/// top-N *selection* wrong, not just the display: an issue with 1,000,000
/// app-wide occurrences and 1 in the selected environment would permanently
/// outrank one with 5,000 in that environment, and the displayed numbers
/// were not even guaranteed to be in descending order (still sorted by the
/// app-wide count). Trading the "never aggregate more than one page" cost
/// property `list_issues` keeps for a correct ranking here — see
/// `.superpowers/sdd/s2-task-9-report.md`'s "Critical findings fixed"
/// section for the measured cost on the real dev app.
pub async fn top_issues(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    since: DateTime<Utc>,
    limit: i64,
) -> QueryResult<Vec<Issue>> {
    if matches!(scope.env, EnvFilter::All) {
        return issues::table
            .filter(issues::app_id.eq(scope.app_id))
            .filter(issues::last_seen.ge(since))
            .select(Issue::as_select())
            .order(issues::times_seen.desc())
            .limit(limit)
            .load(conn)
            .await;
    }

    let env_bind_idx = 4usize;
    let env_sql = scope.env.sql_fragment_for("e", env_bind_idx);
    let member_env_sql = scope.env.sql_fragment_for("m", env_bind_idx);
    // Task 9: same `latest` LATERAL as `list_issues`/`get_issue`, reusing
    // the identical `$4` env bind `agg` already consumes — see
    // `list_issues`' doc comment for the full title/culprit/level
    // derivation reasoning.
    let sql_text = format!(
        "SELECT i.id, i.app_id, i.fingerprint, i.type AS type_, \
                COALESCE(latest.title, i.title)     AS title, \
                COALESCE(latest.culprit, i.culprit) AS culprit, \
                COALESCE(latest.level, i.level)     AS level, \
                i.status, \
                agg.first_seen, agg.last_seen, agg.times_seen, agg.users_seen, \
                i.assignee_id, i.created_at, i.updated_at, i.last_event_at \
         FROM ( \
             SELECT * FROM issues \
             WHERE app_id = $1 AND last_seen >= $2 \
               AND EXISTS (SELECT 1 FROM error_events m WHERE m.issue_id = issues.id{member_env_sql}) \
         ) i \
         JOIN LATERAL ( \
             SELECT count(*)::bigint AS times_seen, \
                    count(DISTINCT distinct_id)::bigint AS users_seen, \
                    min(occurred_at) AS first_seen, \
                    max(occurred_at) AS last_seen \
             FROM error_events e \
             WHERE e.issue_id = i.id AND e.occurred_at >= $2{env_sql} \
             HAVING count(*) > 0 \
         ) agg ON TRUE \
         LEFT JOIN LATERAL ( \
             SELECT e.title, e.culprit, e.level \
             FROM error_events e \
             WHERE e.issue_id = i.id{env_sql} \
             ORDER BY e.occurred_at DESC \
             LIMIT 1 \
         ) latest ON TRUE \
         WHERE agg.last_seen >= $2 \
         ORDER BY agg.times_seen DESC \
         LIMIT $3"
    );
    let mut stmt = diesel::sql_query(sql_text)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Timestamptz, _>(since)
        .bind::<BigInt, _>(limit);
    stmt = crate::bind_env!(stmt, &scope.env);
    let rows: Vec<IssueRow> = stmt.get_results(conn).await?;
    Ok(rows.into_iter().map(Issue::from).collect())
}

// ===========================================================================
// Issue stats (Exceptions dashboard header)
// ===========================================================================

#[derive(Debug, QueryableByName, serde::Serialize)]
pub struct IssueStatsRow {
    #[diesel(sql_type = BigInt)]
    pub total: i64,
    #[diesel(sql_type = BigInt)]
    pub unresolved: i64,
    #[diesel(sql_type = BigInt)]
    pub resolved: i64,
    #[diesel(sql_type = BigInt)]
    pub ignored: i64,
    #[diesel(sql_type = BigInt)]
    pub fatal: i64,
    #[diesel(sql_type = BigInt)]
    pub error: i64,
    #[diesel(sql_type = BigInt)]
    pub warning: i64,
    #[diesel(sql_type = BigInt)]
    pub info: i64,
}

/// `issues` carries no `environment_id`. Under `EnvFilter::All` this reads
/// `issues` directly and is unchanged — same query this function ran before
/// Slice 2/Task 9, no join.
///
/// Under `One`/`Subset`/`Unattributed`, Task 9 replaced the plain membership
/// `EXISTS` with an **inner** `JOIN LATERAL` (`lvl`) that derives each
/// issue's `level` from its single newest `error_events` row in the selected
/// environment (`ORDER BY e.occurred_at DESC LIMIT 1`) — the identical
/// `latest` shape `list_issues`/`get_issue`/`top_issues` use, minus
/// `title`/`culprit` (unneeded here). The `level` `FILTER (WHERE ...)`
/// clauses move onto `lvl.level`, so the fatal/error/warning/info split
/// matches this environment's own occurrences, not whichever environment
/// last overwrote `issues.level`. The join is deliberately **inner, not
/// left**: an issue with no occurrence in this environment has nothing to
/// derive a level from and must not be counted at all — exactly what the
/// membership `EXISTS` it replaces was already doing. `status` stays
/// app-wide (`i.status`) in both branches — issue triage is an app-wide act
/// by design, not a per-environment one, same call `list_issues` makes for
/// the identical two columns.
pub async fn issue_stats(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
) -> QueryResult<IssueStatsRow> {
    if matches!(scope.env, EnvFilter::All) {
        return diesel::sql_query(
            "SELECT count(*)::bigint AS total, \
               count(*) FILTER (WHERE status='unresolved')::bigint AS unresolved, \
               count(*) FILTER (WHERE status='resolved')::bigint AS resolved, \
               count(*) FILTER (WHERE status='ignored')::bigint AS ignored, \
               count(*) FILTER (WHERE level='fatal')::bigint AS fatal, \
               count(*) FILTER (WHERE level='error')::bigint AS error, \
               count(*) FILTER (WHERE level='warning')::bigint AS warning, \
               count(*) FILTER (WHERE level IN ('info','debug'))::bigint AS info \
             FROM issues WHERE app_id=$1",
        )
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .get_result(conn)
        .await;
    }

    // $1 app_id, $2 env — always consumed here (`One`/`Subset` bind it;
    // `Unattributed`'s `IS NULL` fragment needs no bind, and `bind_env!`
    // below correspondingly binds nothing for it), reused by `lvl`'s own
    // `WHERE` fragment, same one-bind-reused-everywhere idiom as
    // `list_issues`.
    let env_sql = scope.env.sql_fragment_for("e", 2);
    let mut stmt = diesel::sql_query(format!(
        "SELECT count(*)::bigint AS total, \
           count(*) FILTER (WHERE i.status='unresolved')::bigint AS unresolved, \
           count(*) FILTER (WHERE i.status='resolved')::bigint AS resolved, \
           count(*) FILTER (WHERE i.status='ignored')::bigint AS ignored, \
           count(*) FILTER (WHERE lvl.level='fatal')::bigint AS fatal, \
           count(*) FILTER (WHERE lvl.level='error')::bigint AS error, \
           count(*) FILTER (WHERE lvl.level='warning')::bigint AS warning, \
           count(*) FILTER (WHERE lvl.level IN ('info','debug'))::bigint AS info \
         FROM issues i \
         JOIN LATERAL ( \
             SELECT e.level \
             FROM error_events e \
             WHERE e.issue_id = i.id{env_sql} \
             ORDER BY e.occurred_at DESC \
             LIMIT 1 \
         ) lvl ON TRUE \
         WHERE i.app_id=$1"
    ))
    .into_boxed()
    .bind::<SqlUuid, _>(scope.app_id);
    stmt = crate::bind_env!(stmt, &scope.env);
    stmt.get_result(conn).await
}

// ===========================================================================
// Event Explorer (raw analytics event stream with filters)
// ===========================================================================

/// Split a `parse_filters`-validated tag value (`key=value`) on the first `=`.
/// The value slot always contains exactly one leading `key=`, guaranteed by
/// `FieldType::Tag` validation, so the `None` arm is defensive only.
fn tag_kv(value: &str) -> (String, String) {
    match value.split_once('=') {
        Some((k, v)) => (k.to_string(), v.to_string()),
        None => (value.to_string(), String::new()),
    }
}

/// A single-key JSONB object `{key: value}` for a `tags @> …` containment bind.
fn tag_object(key: String, value: String) -> serde_json::Value {
    let mut m = serde_json::Map::new();
    m.insert(key, serde_json::Value::String(value));
    serde_json::Value::Object(m)
}

#[allow(clippy::too_many_arguments)]
pub async fn list_analytics_events(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    filters: &[ParsedFilter],
    q: Option<&str>,
    since: Option<chrono::DateTime<chrono::Utc>>,
    limit: i64,
    offset: i64,
) -> QueryResult<Vec<AnalyticsEvent>> {
    // Environment filters need a name->id lookup before the query is built.
    let mut env_eq: Option<Option<Uuid>> = None; // Some(id) filter present
    let mut env_neq: Option<Option<Uuid>> = None;
    for f in filters {
        if f.field == "environment" {
            // The id wanted here is the ENROLLMENT's, because that is what the
            // event tables store in `environment_id` — the catalogue entry is
            // only how a human names it.
            //
            // `retired_at IS NULL` on the enrollment is load-bearing: a name is
            // only unique among LIVE rows, so retiring `staging` and creating a
            // fresh `staging` leaves two enrollments reachable by that name.
            // Without this filter `.first()` returns an arbitrary one, and a
            // filter on the current `staging` could silently show only
            // pre-retirement events. Retiring a catalogue entry retires its
            // enrollments in the same transaction, so this single predicate
            // covers both levels and cannot disagree with itself.
            let id: Option<Uuid> = app_environments::table
                .inner_join(
                    environments::table.on(environments::id.eq(app_environments::environment_id)),
                )
                .filter(app_environments::app_id.eq(scope.app_id))
                .filter(environments::name.eq(&f.value))
                .filter(app_environments::retired_at.is_null())
                .select(app_environments::id)
                .first::<Uuid>(conn)
                .await
                .ok();
            match f.op {
                Op::Eq => env_eq = Some(id),
                Op::Neq => env_neq = Some(id),
                _ => {}
            }
        }
    }

    let mut query = analytics_events::table
        .filter(analytics_events::app_id.eq(scope.app_id))
        // Synthetic screen-view events belong to the Screens section, not the stream.
        .filter(analytics_events::name.ne("$screen"))
        .into_boxed();
    // The scope and the legacy `environment:eq/neq` chip (handled via `env_eq`/`env_neq`
    // below, after the per-filter loop) are both `.filter()` calls on the same boxed
    // query, so both are ANDed: the chip can only narrow within the scope, never widen
    // past it. That property comes from the AND, not from the order — applying the chip
    // first would emit identical SQL. It sits here simply to keep it adjacent to the
    // `app_id` filter it belongs with.
    //
    // Non-widening matters beyond tidiness: Slice 3 makes the environment scope an
    // access boundary, at which point a filter that could widen past it would be a
    // data leak rather than a wrong result.
    query = crate::scope_env!(query, analytics_events, &scope.env);
    if let Some(s) = since {
        query = query.filter(analytics_events::occurred_at.ge(s));
    }
    for f in filters {
        query = match (f.field, f.op) {
            ("name", Op::Eq) => query.filter(analytics_events::name.eq(f.value.clone())),
            ("name", Op::Neq) => query.filter(analytics_events::name.ne(f.value.clone())),
            ("name", Op::Contains) => {
                query.filter(analytics_events::name.ilike(like_contains(&f.value)))
            }
            ("distinct_id", Op::Eq) => {
                query.filter(analytics_events::distinct_id.eq(f.value.clone()))
            }
            ("distinct_id", Op::Neq) => {
                query.filter(analytics_events::distinct_id.ne(f.value.clone()))
            }
            ("distinct_id", Op::Contains) => {
                query.filter(analytics_events::distinct_id.ilike(like_contains(&f.value)))
            }
            ("session_id", Op::Eq) => {
                query.filter(analytics_events::session_id.eq(f.value.clone()))
            }
            ("session_id", Op::Neq) => {
                query.filter(analytics_events::session_id.ne(f.value.clone()))
            }
            ("session_id", Op::Contains) => {
                query.filter(analytics_events::session_id.ilike(like_contains(&f.value)))
            }
            ("release", Op::Eq) => query.filter(analytics_events::release.eq(f.value.clone())),
            ("release", Op::Neq) => query.filter(analytics_events::release.ne(f.value.clone())),
            ("release", Op::Contains) => {
                query.filter(analytics_events::release.ilike(like_contains(&f.value)))
            }
            ("tag", Op::Eq) => {
                let (k, v) = tag_kv(&f.value);
                query.filter(
                    sql::<Bool>("analytics_events.tags @> ").bind::<Jsonb, _>(tag_object(k, v)),
                )
            }
            ("tag", Op::Contains) => {
                let (k, v) = tag_kv(&f.value);
                query.filter(
                    sql::<Bool>("analytics_events.tags ->> ")
                        .bind::<Text, _>(k)
                        .sql(" ILIKE ")
                        .bind::<Text, _>(like_contains(&v)),
                )
            }
            // `workflow_id IS NOT NULL` alongside the name predicate is the
            // partial-index term: migration 2026-07-29-000032's
            // `analytics_events_app_workflow_idx` is
            // `WHERE workflow_id IS NOT NULL`, and Postgres uses a partial
            // index only when the query's WHERE *implies* that predicate —
            // `workflow_name = $N` does not. Semantically a no-op (the
            // pipeline stamps id and name together). This is the case Task 4
            // measured on the largest table in the system: 14 buffers / cost
            // 2,025 with the term vs 52,744 / 56,190 without.
            ("workflow", Op::Eq) => query
                .filter(analytics_events::workflow_id.is_not_null())
                .filter(analytics_events::workflow_name.eq(f.value.clone())),
            // `OR workflow_name IS NULL`, and deliberately NO
            // `workflow_id IS NOT NULL` term — see
            // `list_error_events_for_issue`'s `workflow` arms for the full
            // reasoning. Short version: `workflow` is one chip offered at
            // three levels (Events, Issues, occurrences), so it must mean the
            // same thing at each, and at the issue level `NOT EXISTS` already
            // makes an unstamped row match `neq`. The partial index is
            // deliberately forgone here: this arm's whole purpose is to RETURN
            // the unstamped rows, which are exactly the rows that index
            // excludes.
            ("workflow", Op::Neq) => query.filter(
                analytics_events::workflow_name
                    .ne(f.value.clone())
                    .or(analytics_events::workflow_name.is_null()),
            ),
            ("workflow", Op::Contains) => query
                .filter(analytics_events::workflow_id.is_not_null())
                .filter(analytics_events::workflow_name.ilike(like_contains(&f.value))),
            _ => query, // environment handled below; others unreachable
        };
    }
    // environment eq: unknown name -> no rows (filter on the impossible nil id).
    if let Some(id) = env_eq {
        query = match id {
            Some(id) => query.filter(analytics_events::environment_id.eq(id)),
            None => query.filter(analytics_events::environment_id.eq(Uuid::nil())),
        };
    }
    // environment neq: unknown name -> nothing to exclude.
    if let Some(Some(id)) = env_neq {
        query = query.filter(analytics_events::environment_id.ne(id));
    }
    if let Some(term) = q {
        let p = like_contains(term);
        query = query.filter(
            analytics_events::name
                .ilike(p.clone())
                .or(analytics_events::distinct_id.ilike(p.clone()))
                .or(sql::<Bool>("analytics_events.contexts::text ILIKE ")
                    .bind::<Text, _>(p.clone()))
                .or(sql::<Bool>("analytics_events.extra::text ILIKE ").bind::<Text, _>(p.clone()))
                .or(sql::<Bool>("analytics_events.properties::text ILIKE ")
                    .bind::<Text, _>(p.clone()))
                .or(sql::<Bool>("analytics_events.tags::text ILIKE ").bind::<Text, _>(p)),
        );
    }
    query
        .select(AnalyticsEvent::as_select())
        .order(analytics_events::occurred_at.desc())
        .limit(limit)
        .offset(offset)
        .load(conn)
        .await
}

// ===========================================================================
// Funnel (ordered multi-step conversion)
// ===========================================================================

#[derive(Debug, QueryableByName, serde::Serialize)]
pub struct FunnelStepCount {
    #[diesel(sql_type = BigInt)]
    pub step: i64,
    #[diesel(sql_type = BigInt)]
    pub count: i64,
}

/// Ordered funnel: how many distinct people did step 0, then step 1 at-or-after
/// their step-0 time, and so on. Built as a chained-CTE query over the steps.
pub async fn funnel(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    steps: &[String],
    since: DateTime<Utc>,
) -> QueryResult<Vec<FunnelStepCount>> {
    // $1 = app_id, $2 = since, $3 = env (only when scope.env is One), then each step name in
    // order starting at the next free index.
    //
    // The env predicate must apply to EVERY step's CTE, not just s0: each s{i} independently
    // re-reads `analytics_events`, so scoping only s0 would let a step-0 candidate whose
    // later step happened in a *different* environment count anyway — silently widening the
    // funnel past the selected environment instead of erroring. `s0` has no table alias
    // (bare `analytics_events`); `s{i>0}` aliases it `a` — `sql_fragment` (unqualified) is
    // right for the former, `sql_fragment_for("a", ..)` for the rest.
    let base_idx = if scope.env.consumes_bind() { 4 } else { 3 };
    let env_sql_bare = scope.env.sql_fragment(3);
    let env_sql_aliased = scope.env.sql_fragment_for("a", 3);

    let mut ctes: Vec<String> = Vec::new();
    let mut selects: Vec<String> = Vec::new();
    for i in 0..steps.len() {
        let name_param = i + base_idx;
        if i == 0 {
            ctes.push(format!(
                "s0 AS (SELECT distinct_id, min(occurred_at) AS t FROM analytics_events \
                 WHERE app_id=$1 AND occurred_at>=$2 AND name=${name_param}{env_sql_bare} GROUP BY distinct_id)"
            ));
        } else {
            let prev = i - 1;
            ctes.push(format!(
                "s{i} AS (SELECT a.distinct_id, min(a.occurred_at) AS t FROM analytics_events a \
                 JOIN s{prev} ON s{prev}.distinct_id = a.distinct_id \
                 WHERE a.app_id=$1 AND a.name=${name_param}{env_sql_aliased} AND a.occurred_at >= s{prev}.t \
                 GROUP BY a.distinct_id)"
            ));
        }
        selects.push(format!(
            "SELECT {i}::bigint AS step, (SELECT count(*) FROM s{i})::bigint AS count"
        ));
    }
    let sql = format!(
        "WITH {} {} ORDER BY step",
        ctes.join(", "),
        selects.join(" UNION ALL ")
    );

    let mut query = diesel::sql_query(sql)
        .into_boxed::<diesel::pg::Pg>()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Timestamptz, _>(since);
    query = crate::bind_env!(query, &scope.env);
    for step in steps {
        query = query.bind::<Text, _>(step.clone());
    }
    query.get_results(conn).await
}

// ===========================================================================
// Journeys (step-indexed transition graph for a Sankey)
// ===========================================================================

#[derive(Debug, QueryableByName, serde::Serialize, serde::Deserialize)]
pub struct JourneyLink {
    #[diesel(sql_type = BigInt)]
    pub from_step: i64,
    #[diesel(sql_type = Text)]
    pub from_event: String,
    #[diesel(sql_type = Text)]
    pub to_event: String,
    #[diesel(sql_type = BigInt)]
    pub count: i64,
}

#[derive(Debug, QueryableByName, serde::Serialize, serde::Deserialize)]
pub struct JourneyNode {
    #[diesel(sql_type = BigInt)]
    pub step: i64,
    #[diesel(sql_type = Text)]
    pub event: String,
    #[diesel(sql_type = BigInt)]
    pub count: i64,
}

/// Maximum node/link rows returned by a journey query.
///
/// Both result sets grow with event-name cardinality, which is caller-supplied
/// (every distinct `name` an SDK ever sent). Without a cap a high-cardinality
/// app produces an unbounded response.
const JOURNEY_MAX_ROWS: i64 = 500;

#[derive(Debug, QueryableByName)]
struct JourneyGraphRow {
    #[diesel(sql_type = Jsonb)]
    data: Value,
}

/// Nodes + links for the journey Sankey, computed in ONE query.
///
/// The step-indexed CTE (`row_number() OVER (PARTITION BY distinct_id ORDER BY
/// occurred_at)`) is the expensive part. Running separate node and link queries
/// evaluated it twice per page load; because `capped` is referenced more than
/// once here, Postgres materializes it and both aggregates read the same
/// intermediate. The `(app_id, distinct_id, occurred_at)` index lets the window
/// be satisfied by an ordered index scan rather than a full sort.
pub async fn journey_graph(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    since: DateTime<Utc>,
    depth: i64,
) -> QueryResult<(Vec<JourneyNode>, Vec<JourneyLink>)> {
    // $1 app_id, $2 since — env takes $3 when it needs a bind, which pushes depth/max_rows
    // from $3/$4 to $4/$5. Both indices are derived from the same `env_bind`/`env_sql` pair
    // so the string and the bind chain can't drift apart.
    let env_sql = scope.env.sql_fragment(3);
    let depth_idx = if scope.env.consumes_bind() { 4 } else { 3 };
    let max_rows_idx = depth_idx + 1;

    let q = format!(
        "WITH ordered AS ( \
           SELECT distinct_id, name, \
             (row_number() OVER (PARTITION BY distinct_id ORDER BY occurred_at) - 1) AS step \
           FROM analytics_events WHERE app_id=$1 AND occurred_at>=$2{env_sql}), \
         capped AS (SELECT * FROM ordered WHERE step < ${depth_idx}), \
         nodes AS ( \
           SELECT step, name AS event, count(*)::bigint AS count \
           FROM capped GROUP BY step, name ORDER BY step, count DESC LIMIT ${max_rows_idx}), \
         links AS ( \
           SELECT a.step AS from_step, a.name AS from_event, b.name AS to_event, \
                  count(*)::bigint AS count \
           FROM capped a JOIN capped b ON b.distinct_id=a.distinct_id AND b.step=a.step+1 \
           GROUP BY a.step, a.name, b.name ORDER BY a.step, count DESC LIMIT ${max_rows_idx}) \
         SELECT jsonb_build_object( \
           'nodes', COALESCE((SELECT jsonb_agg(to_jsonb(n)) FROM nodes n), '[]'::jsonb), \
           'links', COALESCE((SELECT jsonb_agg(to_jsonb(l)) FROM links l), '[]'::jsonb) \
         ) AS data"
    );

    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Timestamptz, _>(since);
    stmt = crate::bind_env!(stmt, &scope.env);
    let row: JourneyGraphRow = stmt
        .bind::<BigInt, _>(depth)
        .bind::<BigInt, _>(JOURNEY_MAX_ROWS)
        .get_result(conn)
        .await?;

    let nodes = row
        .data
        .get("nodes")
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    let links = row
        .data
        .get("links")
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    Ok((nodes, links))
}

// ===========================================================================
// Performance (percentile aggregates over transactions)
// ===========================================================================

#[derive(Debug, QueryableByName, serde::Serialize)]
pub struct PerfSummaryRow {
    #[diesel(sql_type = Text)]
    pub name: String,
    #[diesel(sql_type = Text)]
    pub op: String,
    #[diesel(sql_type = BigInt)]
    pub count: i64,
    #[diesel(sql_type = Double)]
    pub p50: f64,
    #[diesel(sql_type = Double)]
    pub p75: f64,
    #[diesel(sql_type = Double)]
    pub p95: f64,
    #[diesel(sql_type = Double)]
    pub p99: f64,
    #[diesel(sql_type = Double)]
    pub avg: f64,
    #[diesel(sql_type = Double)]
    pub error_rate: f64,
}

pub async fn performance_summary(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    since: DateTime<Utc>,
    op: Option<&str>,
    device_key: Option<&str>,
) -> QueryResult<Vec<PerfSummaryRow>> {
    // $1 app_id, $2 since, $3 op, $4 device_key (the pre-existing `(...::text IS NULL OR
    // ...)` optional-filter idiom — left untouched). Env is appended AFTER those, at the
    // next free index ($5), rather than interleaved among them, so $3/$4 never renumber and
    // there's no collision to reason about.
    let env_sql = scope.env.sql_fragment(5);
    let q = format!(
        "SELECT name, op, count(*)::bigint AS count, \
           percentile_cont(0.5)  WITHIN GROUP (ORDER BY duration_ms) AS p50, \
           percentile_cont(0.75) WITHIN GROUP (ORDER BY duration_ms) AS p75, \
           percentile_cont(0.95) WITHIN GROUP (ORDER BY duration_ms) AS p95, \
           percentile_cont(0.99) WITHIN GROUP (ORDER BY duration_ms) AS p99, \
           avg(duration_ms) AS avg, \
           (count(*) FILTER (WHERE status='error' OR http_status>=500))::float8 \
             / NULLIF(count(*),0) AS error_rate \
         FROM transactions \
         WHERE app_id=$1 AND occurred_at>=$2 \
           AND ($3::text IS NULL OR op=$3) AND ($4::text IS NULL OR device_key=$4){env_sql} \
         GROUP BY name, op ORDER BY count DESC LIMIT 100"
    );
    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Timestamptz, _>(since)
        .bind::<Nullable<Text>, _>(op)
        .bind::<Nullable<Text>, _>(device_key);
    stmt = crate::bind_env!(stmt, &scope.env);
    stmt.get_results(conn).await
}

#[derive(Debug, QueryableByName, serde::Serialize)]
pub struct PerfSeriesPoint {
    #[diesel(sql_type = Timestamptz)]
    pub bucket: DateTime<Utc>,
    #[diesel(sql_type = Double)]
    pub p50: f64,
    #[diesel(sql_type = Double)]
    pub p95: f64,
    #[diesel(sql_type = BigInt)]
    pub throughput: i64,
}

pub async fn performance_series(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    since: DateTime<Utc>,
    name: Option<&str>,
    op: Option<&str>,
) -> QueryResult<Vec<PerfSeriesPoint>> {
    // Same shape as `performance_summary`: env appended after the pre-existing $3/$4
    // optional-filter idiom, at the next free index ($5), so those two never renumber.
    let env_sql = scope.env.sql_fragment(5);
    let q = format!(
        "SELECT date_trunc('hour', occurred_at) AS bucket, \
           percentile_cont(0.5)  WITHIN GROUP (ORDER BY duration_ms) AS p50, \
           percentile_cont(0.95) WITHIN GROUP (ORDER BY duration_ms) AS p95, \
           count(*)::bigint AS throughput \
         FROM transactions \
         WHERE app_id=$1 AND occurred_at>=$2 \
           AND ($3::text IS NULL OR name=$3) AND ($4::text IS NULL OR op=$4){env_sql} \
         GROUP BY bucket ORDER BY bucket LIMIT 5000"
    );
    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Timestamptz, _>(since)
        .bind::<Nullable<Text>, _>(name)
        .bind::<Nullable<Text>, _>(op);
    stmt = crate::bind_env!(stmt, &scope.env);
    stmt.get_results(conn).await
}

// ---------------------------------------------------------------------------
// Audience & session-engagement analytics (feature A).
// ---------------------------------------------------------------------------

#[derive(Debug, QueryableByName, serde::Serialize)]
pub struct UserStats {
    #[diesel(sql_type = BigInt)]
    pub total_users: i64,
    #[diesel(sql_type = BigInt)]
    pub active_in_range: i64,
    #[diesel(sql_type = BigInt)]
    pub new_in_range: i64,
    #[diesel(sql_type = BigInt)]
    pub dau: i64,
    #[diesel(sql_type = BigInt)]
    pub wau: i64,
    #[diesel(sql_type = BigInt)]
    pub mau: i64,
    #[diesel(sql_type = Double)]
    pub avg_session_ms: f64,
    #[diesel(sql_type = Double)]
    pub median_session_ms: f64,
}

/// Aggregate audience stats for an app. `total_users`/`wau`/`mau` ignore `since`
/// (all-time / rolling-from-now); the rest are scoped to `since`.
///
/// `total_users`/`active_in_range`/`new_in_range` read `event_users`, which carries no
/// `environment_id` column at all — scoped by membership (see
/// `event_user_membership_exists`'s doc comment), the gap Task 8 deferred and this fix
/// closes. `total_users` has no `since` bound of its own, so membership is the *only*
/// predicate added to it under `One`/`Unattributed`. `new_in_range`'s existing
/// `first_seen>=$2` combined with membership is reading (a) — "globally-first-seen in the
/// window AND has activity in this environment" — not (b) ("first activity *in this
/// environment* falls in the window"); see `overview_totals`'s doc comment for the full
/// rationale and consequence, which applies identically here. Every other sub-select reads a
/// table that does carry `environment_id` (analytics_events/error_events for dau/wau/mau,
/// sessions for the two `*_session_ms` fields) and gets the real predicate, reused across
/// all 8 of those sub-selects via the same bind ($3, only when `scope.env` is `One`) that
/// `event_user_membership_exists` also reuses for its three `EXISTS` legs.
pub async fn user_stats(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    since: DateTime<Utc>,
) -> QueryResult<UserStats> {
    let env_sql = scope.env.sql_fragment(3);
    // `.clone()`, not a move — the final `bind_env!` call below still needs
    // `scope.env`; see `overview_totals`'s identical call for why.
    let membership_sql = event_user_membership_exists(scope.env.clone(), 3);
    let q = format!(
        "SELECT \
           (SELECT count(*) FROM event_users WHERE app_id=$1{membership_sql})::bigint AS total_users, \
           (SELECT count(*) FROM event_users WHERE app_id=$1 AND last_seen>=$2{membership_sql})::bigint AS active_in_range, \
           (SELECT count(*) FROM event_users WHERE app_id=$1 AND first_seen>=$2{membership_sql})::bigint AS new_in_range, \
           (SELECT count(DISTINCT distinct_id) FROM ( \
              SELECT distinct_id FROM analytics_events WHERE app_id=$1 AND occurred_at >= now() - interval '1 day'{env_sql} AND distinct_id IS NOT NULL AND distinct_id <> '' \
              UNION ALL \
              SELECT distinct_id FROM error_events WHERE app_id=$1 AND occurred_at >= now() - interval '1 day'{env_sql} AND distinct_id IS NOT NULL AND distinct_id <> '' \
            ) d1)::bigint AS dau, \
           (SELECT count(DISTINCT distinct_id) FROM ( \
              SELECT distinct_id FROM analytics_events WHERE app_id=$1 AND occurred_at >= now() - interval '7 days'{env_sql} AND distinct_id IS NOT NULL AND distinct_id <> '' \
              UNION ALL \
              SELECT distinct_id FROM error_events WHERE app_id=$1 AND occurred_at >= now() - interval '7 days'{env_sql} AND distinct_id IS NOT NULL AND distinct_id <> '' \
            ) d7)::bigint AS wau, \
           (SELECT count(DISTINCT distinct_id) FROM ( \
              SELECT distinct_id FROM analytics_events WHERE app_id=$1 AND occurred_at >= now() - interval '30 days'{env_sql} AND distinct_id IS NOT NULL AND distinct_id <> '' \
              UNION ALL \
              SELECT distinct_id FROM error_events WHERE app_id=$1 AND occurred_at >= now() - interval '30 days'{env_sql} AND distinct_id IS NOT NULL AND distinct_id <> '' \
            ) d30)::bigint AS mau, \
           COALESCE((SELECT avg(EXTRACT(EPOCH FROM (last_event_at - started_at)) * 1000) \
                     FROM sessions WHERE app_id=$1 AND last_event_at>=$2{env_sql}), 0)::double precision AS avg_session_ms, \
           COALESCE((SELECT percentile_cont(0.5) WITHIN GROUP (ORDER BY EXTRACT(EPOCH FROM (last_event_at - started_at)) * 1000) \
                     FROM sessions WHERE app_id=$1 AND last_event_at>=$2{env_sql}), 0)::double precision AS median_session_ms"
    );
    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Timestamptz, _>(since);
    stmt = crate::bind_env!(stmt, &scope.env);
    stmt.get_result(conn).await
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct UserSeriesPoint {
    pub bucket: DateTime<Utc>,
    pub active: i64,
    pub new_users: i64,
}

/// Merge per-day active + per-day new counts into one sorted series, 0-filling
/// days present in only one input. Pure — unit-tested.
pub fn merge_user_series(active: Vec<SeriesPoint>, new: Vec<SeriesPoint>) -> Vec<UserSeriesPoint> {
    use std::collections::BTreeMap;
    let mut map: BTreeMap<DateTime<Utc>, (i64, i64)> = BTreeMap::new();
    for p in active {
        map.entry(p.bucket).or_default().0 = p.count;
    }
    for p in new {
        map.entry(p.bucket).or_default().1 = p.count;
    }
    map.into_iter()
        .map(|(bucket, (active, new_users))| UserSeriesPoint {
            bucket,
            active,
            new_users,
        })
        .collect()
}

/// Per-day distinct active users (analytics ∪ errors) and per-day new users,
/// merged. Both scoped to `since`.
///
/// `active` reads analytics_events/error_events (both carry `environment_id`) and gets the
/// real predicate. `new` reads `event_users`, which does not — scoped by membership (see
/// `event_user_membership_exists`'s doc comment), the gap Task 8 deferred and this fix
/// closes. Same reading-(a) semantics as `overview_totals.new_users`/`user_stats.new_in_range`
/// (globally-first-seen in the window AND a member of this environment, not first-seen-in-
/// this-environment) — see `overview_totals`'s doc comment for the full rationale.
pub async fn active_user_series(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    since: DateTime<Utc>,
) -> QueryResult<Vec<UserSeriesPoint>> {
    let env_sql = scope.env.sql_fragment(3);
    let active_q = format!(
        "SELECT date_trunc('day', occurred_at) AS bucket, count(DISTINCT distinct_id)::bigint AS count \
         FROM ( \
            SELECT occurred_at, distinct_id FROM analytics_events \
              WHERE app_id=$1 AND occurred_at>=$2{env_sql} AND distinct_id IS NOT NULL AND distinct_id <> '' \
            UNION ALL \
            SELECT occurred_at, distinct_id FROM error_events \
              WHERE app_id=$1 AND occurred_at>=$2{env_sql} AND distinct_id IS NOT NULL AND distinct_id <> '' \
         ) u \
         GROUP BY bucket ORDER BY bucket"
    );
    let mut active_stmt = diesel::sql_query(active_q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Timestamptz, _>(since);
    active_stmt = crate::bind_env!(active_stmt, &scope.env);
    let active: Vec<SeriesPoint> = active_stmt.get_results(conn).await?;

    // `.clone()`, not a move: the second `bind_env!` call below (for
    // `new_stmt`) still needs `scope.env` — see `overview_totals`'s identical
    // call for why `event_user_membership_exists` itself is not reshaped to
    // take `&EnvFilter` instead.
    let membership_sql = event_user_membership_exists(scope.env.clone(), 3);
    let new_q = format!(
        "SELECT date_trunc('day', first_seen) AS bucket, count(*)::bigint AS count \
         FROM event_users WHERE app_id=$1 AND first_seen>=$2{membership_sql} \
         GROUP BY bucket ORDER BY bucket"
    );
    let mut new_stmt = diesel::sql_query(new_q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Timestamptz, _>(since);
    new_stmt = crate::bind_env!(new_stmt, &scope.env);
    let new: Vec<SeriesPoint> = new_stmt.get_results(conn).await?;

    Ok(merge_user_series(active, new))
}

#[derive(Debug, QueryableByName, serde::Serialize)]
pub struct SessionStats {
    #[diesel(sql_type = BigInt)]
    pub sessions: i64,
    #[diesel(sql_type = BigInt)]
    pub crashed: i64,
    #[diesel(sql_type = Double)]
    pub avg_session_ms: f64,
    #[diesel(sql_type = Double)]
    pub median_session_ms: f64,
}

#[derive(Debug, QueryableByName, serde::Serialize)]
pub struct SeriesAvgPoint {
    #[diesel(sql_type = Timestamptz)]
    pub bucket: DateTime<Utc>,
    #[diesel(sql_type = Double)]
    pub avg_ms: f64,
}

#[derive(Debug, QueryableByName, serde::Serialize)]
pub struct HistoBucket {
    #[diesel(sql_type = Text)]
    pub bucket: String,
    #[diesel(sql_type = BigInt)]
    pub count: i64,
}

/// Duration-histogram bucket labels, in display order.
pub const DURATION_BUCKETS: [&str; 5] = ["<10s", "10-60s", "1-5m", "5-30m", "30m+"];

/// The SQL `CASE` mapping a duration in milliseconds (aliased `d`) onto
/// [`DURATION_BUCKETS`]' labels.
///
/// Shared by every duration histogram — `session_duration_histogram` and
/// `workflow_detail` — rather than copied into each, because the failure
/// mode of a divergent copy is *silent*: [`order_histogram`] matches these
/// labels against `DURATION_BUCKETS` by string equality and 0-fills anything
/// it doesn't recognise, so a single typo'd label here produces a
/// permanently all-zero bucket that no error, no type check and no `NULL`
/// ever reveals. `duration_bucket_case_emits_exactly_the_declared_labels`
/// below locks the SQL and the Rust array together.
const DURATION_BUCKET_CASE_SQL: &str = "CASE \
             WHEN d < 10000  THEN '<10s' \
             WHEN d < 60000  THEN '10-60s' \
             WHEN d < 300000 THEN '1-5m' \
             WHEN d < 1800000 THEN '5-30m' \
             ELSE '30m+' END";

/// Reorder DB histogram rows into the fixed bucket order, 0-filling gaps. Pure.
pub fn order_histogram(rows: Vec<HistoBucket>) -> Vec<HistoBucket> {
    DURATION_BUCKETS
        .iter()
        .map(|label| {
            let count = rows
                .iter()
                .find(|r| r.bucket == *label)
                .map(|r| r.count)
                .unwrap_or(0);
            HistoBucket {
                bucket: (*label).to_string(),
                count,
            }
        })
        .collect()
}

pub async fn session_stats(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    since: DateTime<Utc>,
) -> QueryResult<SessionStats> {
    // $1 app_id, $2 since, reused across all four sub-selects, all against `sessions` — env
    // takes $3 when it needs a bind, reused the same way.
    //
    // `crashed` has the same known mislabelling gap as `overview_totals`'
    // `crashed_sessions` — see `bump_session`'s doc comment and
    // `.superpowers/sdd/2026-07-29-environment-rbac-scope/task-10-report.md`.
    let env_sql = scope.env.sql_fragment(3);
    let q = format!(
        "SELECT \
           (SELECT count(*) FROM sessions WHERE app_id=$1 AND last_event_at>=$2{env_sql})::bigint AS sessions, \
           (SELECT count(*) FROM sessions WHERE app_id=$1 AND last_event_at>=$2 AND errors_count>0{env_sql})::bigint AS crashed, \
           COALESCE((SELECT avg(EXTRACT(EPOCH FROM (last_event_at - started_at)) * 1000) \
                     FROM sessions WHERE app_id=$1 AND last_event_at>=$2{env_sql}), 0)::double precision AS avg_session_ms, \
           COALESCE((SELECT percentile_cont(0.5) WITHIN GROUP (ORDER BY EXTRACT(EPOCH FROM (last_event_at - started_at)) * 1000) \
                     FROM sessions WHERE app_id=$1 AND last_event_at>=$2{env_sql}), 0)::double precision AS median_session_ms"
    );
    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Timestamptz, _>(since);
    stmt = crate::bind_env!(stmt, &scope.env);
    stmt.get_result(conn).await
}

pub async fn session_duration_series(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    since: DateTime<Utc>,
) -> QueryResult<Vec<SeriesAvgPoint>> {
    let env_sql = scope.env.sql_fragment(3);
    let q = format!(
        "SELECT date_trunc('day', started_at) AS bucket, \
                COALESCE(avg(EXTRACT(EPOCH FROM (last_event_at - started_at)) * 1000), 0)::double precision AS avg_ms \
         FROM sessions WHERE app_id=$1 AND started_at>=$2{env_sql} \
         GROUP BY bucket ORDER BY bucket"
    );
    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Timestamptz, _>(since);
    stmt = crate::bind_env!(stmt, &scope.env);
    stmt.get_results(conn).await
}

pub async fn session_duration_histogram(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    since: DateTime<Utc>,
) -> QueryResult<Vec<HistoBucket>> {
    let env_sql = scope.env.sql_fragment(3);
    let q = format!(
        "SELECT bucket, count(*)::bigint AS count FROM ( \
           SELECT {DURATION_BUCKET_CASE_SQL} AS bucket \
           FROM (SELECT EXTRACT(EPOCH FROM (last_event_at - started_at)) * 1000 AS d \
                 FROM sessions WHERE app_id=$1 AND last_event_at>=$2{env_sql}) s \
         ) b GROUP BY bucket"
    );
    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Timestamptz, _>(since);
    stmt = crate::bind_env!(stmt, &scope.env);
    let rows: Vec<HistoBucket> = stmt.get_results(conn).await?;
    Ok(order_histogram(rows))
}

#[cfg(test)]
mod user_series_tests {
    use super::{merge_user_series, SeriesPoint};
    use chrono::{TimeZone, Utc};

    fn pt(day: u32, count: i64) -> SeriesPoint {
        SeriesPoint {
            bucket: Utc.with_ymd_and_hms(2026, 7, day, 0, 0, 0).unwrap(),
            count,
        }
    }

    #[test]
    fn merges_active_and_new_by_day_zero_filling() {
        let active = vec![pt(1, 10), pt(2, 8)];
        let new = vec![pt(2, 3), pt(3, 5)]; // day 1 has no new; day 3 has no active
        let out = merge_user_series(active, new);
        let got: Vec<(u32, i64, i64)> = out
            .iter()
            .map(|p| {
                (
                    p.bucket.format("%d").to_string().parse().unwrap(),
                    p.active,
                    p.new_users,
                )
            })
            .collect();
        assert_eq!(got, vec![(1, 10, 0), (2, 8, 3), (3, 0, 5)]);
    }

    #[test]
    fn empty_inputs_yield_empty() {
        assert!(merge_user_series(vec![], vec![]).is_empty());
    }
}

#[cfg(test)]
mod histogram_tests {
    use super::{order_histogram, HistoBucket, DURATION_BUCKETS};

    fn b(bucket: &str, count: i64) -> HistoBucket {
        HistoBucket {
            bucket: bucket.to_string(),
            count,
        }
    }

    #[test]
    fn fills_missing_buckets_in_fixed_order() {
        let rows = vec![b("30m+", 2), b("<10s", 5)];
        let out = order_histogram(rows);
        let got: Vec<(&str, i64)> = out.iter().map(|h| (h.bucket.as_str(), h.count)).collect();
        assert_eq!(
            got,
            vec![
                ("<10s", 5),
                ("10-60s", 0),
                ("1-5m", 0),
                ("5-30m", 0),
                ("30m+", 2)
            ]
        );
        assert_eq!(out.len(), DURATION_BUCKETS.len());
    }

    /// The SQL `CASE` and the Rust label array must agree exactly.
    ///
    /// This is the only thing standing between a typo'd SQL label and a
    /// permanently all-zero histogram bucket: `order_histogram` matches by
    /// string equality and 0-fills whatever it doesn't recognise, so a
    /// divergence produces no error, no `NULL`, and no type failure — just a
    /// bucket that is always empty, in every environment, forever. Asserting
    /// on the extracted string literals (rather than eyeballing the two) is
    /// what makes the coupling mechanical.
    #[test]
    fn duration_bucket_case_emits_exactly_the_declared_labels() {
        // Pull every `'...'` literal out of the CASE's THEN/ELSE arms.
        let emitted: Vec<&str> = super::DURATION_BUCKET_CASE_SQL
            .match_indices('\'')
            .collect::<Vec<_>>()
            .chunks(2)
            .filter(|pair| pair.len() == 2)
            .map(|pair| {
                let start = pair[0].0 + 1;
                let end = pair[1].0;
                &super::DURATION_BUCKET_CASE_SQL[start..end]
            })
            .collect();
        assert_eq!(
            emitted,
            DURATION_BUCKETS.to_vec(),
            "the CASE's labels must match DURATION_BUCKETS exactly, in order — \
             a mismatch silently 0-fills that bucket instead of erroring"
        );
    }
}

// ===========================================================================
// Saved funnels (persisted, app-scoped funnel templates)
// ===========================================================================

#[derive(Debug, QueryableByName, serde::Serialize)]
pub struct SavedFunnelRow {
    #[diesel(sql_type = SqlUuid)]
    pub id: Uuid,
    #[diesel(sql_type = SqlUuid)]
    pub app_id: Uuid,
    #[diesel(sql_type = Text)]
    pub name: String,
    #[diesel(sql_type = Nullable<Text>)]
    pub description: Option<String>,
    #[diesel(sql_type = Jsonb)]
    pub steps: Value,
    #[diesel(sql_type = Nullable<Text>)]
    pub created_by_name: Option<String>,
    #[diesel(sql_type = Timestamptz)]
    pub created_at: DateTime<Utc>,
    #[diesel(sql_type = Timestamptz)]
    pub updated_at: DateTime<Utc>,
}

const SAVED_FUNNEL_SELECT: &str = "SELECT sf.id, sf.app_id, sf.name, sf.description, sf.steps, \
    u.name AS created_by_name, sf.created_at, sf.updated_at \
    FROM saved_funnels sf LEFT JOIN users u ON u.id = sf.created_by ";

pub async fn list_saved_funnels(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
) -> QueryResult<Vec<SavedFunnelRow>> {
    // Bounded: saved funnels are user-created and otherwise unlimited.
    diesel::sql_query(format!(
        "{SAVED_FUNNEL_SELECT} WHERE sf.app_id=$1 ORDER BY sf.updated_at DESC LIMIT 500"
    ))
    .bind::<SqlUuid, _>(app_id)
    .get_results(conn)
    .await
}

pub async fn create_saved_funnel(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    created_by: Uuid,
    name: &str,
    description: Option<&str>,
    steps: &Value,
) -> QueryResult<SavedFunnelRow> {
    diesel::sql_query(format!(
        "WITH ins AS ( \
           INSERT INTO saved_funnels (app_id, name, description, steps, created_by) \
           VALUES ($1, $2, $3, $4, $5) RETURNING * \
         ) {} FROM ins sf LEFT JOIN users u ON u.id = sf.created_by",
        // reuse the same projection but from the CTE
        "SELECT sf.id, sf.app_id, sf.name, sf.description, sf.steps, u.name AS created_by_name, sf.created_at, sf.updated_at"
    ))
    .bind::<SqlUuid, _>(app_id)
    .bind::<Text, _>(name)
    .bind::<Nullable<Text>, _>(description)
    .bind::<Jsonb, _>(steps)
    .bind::<SqlUuid, _>(created_by)
    .get_result(conn)
    .await
}

/// Returns number of rows updated (0 → not found / wrong app).
pub async fn update_saved_funnel(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    id: Uuid,
    name: &str,
    description: Option<&str>,
    steps: &Value,
) -> QueryResult<usize> {
    diesel::sql_query(
        "UPDATE saved_funnels SET name=$3, description=$4, steps=$5, updated_at=now() \
         WHERE app_id=$1 AND id=$2",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<SqlUuid, _>(id)
    .bind::<Text, _>(name)
    .bind::<Nullable<Text>, _>(description)
    .bind::<Jsonb, _>(steps)
    .execute(conn)
    .await
}

/// Returns number of rows deleted (0 → not found / wrong app).
pub async fn delete_saved_funnel(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    id: Uuid,
) -> QueryResult<usize> {
    diesel::sql_query("DELETE FROM saved_funnels WHERE app_id=$1 AND id=$2")
        .bind::<SqlUuid, _>(app_id)
        .bind::<SqlUuid, _>(id)
        .execute(conn)
        .await
}

// ===========================================================================
// Screens (on-read per-screen metrics + capped dwell, app-scoped)
// ===========================================================================

#[derive(Debug, QueryableByName, serde::Serialize)]
pub struct ScreenRow {
    #[diesel(sql_type = Text)]
    pub screen: String,
    #[diesel(sql_type = BigInt)]
    pub views: i64,
    #[diesel(sql_type = BigInt)]
    pub events: i64,
    #[diesel(sql_type = BigInt)]
    pub exceptions: i64,
    #[diesel(sql_type = BigInt)]
    pub users: i64,
    #[diesel(sql_type = Double)]
    pub avg_dwell_ms: f64,
}

#[derive(Debug, QueryableByName, serde::Serialize)]
pub struct ScreenStats {
    #[diesel(sql_type = Text)]
    pub screen: String,
    #[diesel(sql_type = BigInt)]
    pub views: i64,
    #[diesel(sql_type = BigInt)]
    pub events: i64,
    #[diesel(sql_type = BigInt)]
    pub exceptions: i64,
    #[diesel(sql_type = BigInt)]
    pub users: i64,
    #[diesel(sql_type = Double)]
    pub total_dwell_ms: f64,
    #[diesel(sql_type = Double)]
    pub avg_dwell_ms: f64,
}

/// total dwell / views, guarding views=0. Pure.
pub fn avg_dwell(total_ms: f64, views: i64) -> f64 {
    if views > 0 {
        total_ms / views as f64
    } else {
        0.0
    }
}

// Shared CTE fragment: per-screen views/events/users/exceptions/dwell. $1 app, $2 since.

/// Build the screen CTEs with `pred` (a compile-time SQL fragment, never user
/// data) narrowing which screens are aggregated, and `env_sql` (an
/// [`EnvFilter::sql_fragment`]/`sql_fragment_for` output, e.g. `" AND
/// environment_id = $4"` or `""`) narrowing which environment's rows feed
/// them. There is no `screens` table — every column here derives from
/// `analytics_events`/`error_events`, both of which carry `environment_id`,
/// so `env_sql` must reach all four CTEs or a scoped read silently mixes
/// environments in whichever one it missed.
///
/// `ev`/`ex`/`us` push both predicates into their own WHERE clauses — `us`
/// has **two** arms (one per table) inside its `UNION ALL`, and both need
/// `env_sql` independently. Previously both callers aggregated **every**
/// screen in the app and filtered only in the outer query — so the
/// single-screen detail view computed the whole app's stats to return one
/// row, and the list paginated after full aggregation.
///
/// `dw` is deliberately NOT narrowed by `pred` (the screen filter) inside the
/// window: dwell is measured to the next event in the session *whatever
/// screen it is on*, so restricting the window input by screen would compute
/// the wrong gaps. `pred` is applied after `LEAD`, on the outer query, which
/// preserves the value while still shrinking the grouping.
///
/// `env_sql`, however, MUST go inside the inner subquery that computes
/// `raw_ms`, not the outer `WHERE` — unlike `pred`. Two reasons, one loud and
/// one silent:
/// - The outer query only has `g.screen`/`g.raw_ms` in scope (that's all the
///   inner subquery selects), so `environment_id = $N` in the outer `WHERE`
///   is a hard, self-detecting SQL error (no such column).
/// - Even if it *could* resolve, filtering after `LEAD` would still compute
///   dwell gaps using next-events from every environment, then merely hide
///   the *result* rows outside the requested one — the boundary itself would
///   already be crossed. Filtering the rows `LEAD` sees, before the window
///   runs, is what keeps a session's dwell gaps from crossing environments in
///   the first place (a session's own events are expected to share one
///   environment, matching `pred`'s screen-membership semantics: restrict
///   *inputs*, not just outputs, for correctness).
fn screen_ctes(pred: &str, env_sql: &str) -> String {
    format!(
        "WITH ev AS ( \
        SELECT screen, \
          count(*) FILTER (WHERE name='$screen')::bigint AS views, \
          count(*) FILTER (WHERE name<>'$screen')::bigint AS events \
        FROM analytics_events WHERE app_id=$1 AND occurred_at>=$2 AND screen IS NOT NULL AND {pred}{env_sql} GROUP BY screen), \
      ex AS ( \
        SELECT screen, count(*)::bigint AS exceptions \
        FROM error_events WHERE app_id=$1 AND occurred_at>=$2 AND screen IS NOT NULL AND {pred}{env_sql} GROUP BY screen), \
      us AS ( \
        SELECT screen, count(DISTINCT distinct_id)::bigint AS users FROM ( \
          SELECT screen, distinct_id FROM analytics_events WHERE app_id=$1 AND occurred_at>=$2 AND screen IS NOT NULL AND {pred}{env_sql} AND distinct_id IS NOT NULL AND distinct_id<>'' \
          UNION ALL \
          SELECT screen, distinct_id FROM error_events WHERE app_id=$1 AND occurred_at>=$2 AND screen IS NOT NULL AND {pred}{env_sql} AND distinct_id IS NOT NULL AND distinct_id<>'' \
        ) u GROUP BY screen), \
      dw AS ( \
        SELECT screen, sum(LEAST(raw_ms, 1800000))::double precision AS total_dwell_ms FROM ( \
          SELECT screen, EXTRACT(EPOCH FROM ( \
            LEAD(occurred_at) OVER (PARTITION BY session_id ORDER BY occurred_at) - occurred_at)) * 1000 AS raw_ms \
          FROM analytics_events WHERE app_id=$1 AND occurred_at>=$2 AND session_id IS NOT NULL AND screen IS NOT NULL{env_sql}) g \
        WHERE raw_ms IS NOT NULL AND raw_ms > 0 AND {pred} GROUP BY screen), \
      keys AS (SELECT screen FROM ev UNION SELECT screen FROM ex) "
    )
}

/// Predicate for the single-screen detail view.
const SCREEN_PRED_EXACT: &str = "screen = $3";
/// Predicate for the paginated list (`$3` is an escaped ILIKE pattern).
const SCREEN_PRED_LIKE: &str = "screen ILIKE $3";

pub async fn screen_list(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    since: DateTime<Utc>,
    q_pattern: &str, // '%' for no filter, else like_contains(term)
    limit: i64,
    offset: i64,
) -> QueryResult<Vec<ScreenRow>> {
    // $1 app_id, $2 since, $3 q_pattern (SCREEN_PRED_LIKE's own bind) — env
    // takes $4 when it needs a bind, which pushes limit/offset from $4/$5 to
    // $5/$6. Both indices derive from the same `env_bind`/`env_sql` pair, the
    // same "trailing-index shift" idiom `top_events`/`journey_graph` use.
    let env_sql = scope.env.sql_fragment(4);
    let limit_idx = if scope.env.consumes_bind() { 5 } else { 4 };
    let offset_idx = limit_idx + 1;
    let q = format!(
        "{} \
         SELECT k.screen, \
           COALESCE(ev.views,0)::bigint AS views, \
           COALESCE(ev.events,0)::bigint AS events, \
           COALESCE(ex.exceptions,0)::bigint AS exceptions, \
           COALESCE(us.users,0)::bigint AS users, \
           COALESCE(COALESCE(dw.total_dwell_ms,0) / NULLIF(COALESCE(ev.views,0),0), 0)::double precision AS avg_dwell_ms \
         FROM keys k \
         LEFT JOIN ev ON ev.screen=k.screen LEFT JOIN ex ON ex.screen=k.screen \
         LEFT JOIN us ON us.screen=k.screen LEFT JOIN dw ON dw.screen=k.screen \
         ORDER BY views DESC, k.screen ASC LIMIT ${limit_idx} OFFSET ${offset_idx}",
        screen_ctes(SCREEN_PRED_LIKE, &env_sql)
    );
    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Timestamptz, _>(since)
        .bind::<Text, _>(q_pattern);
    stmt = crate::bind_env!(stmt, &scope.env);
    stmt.bind::<BigInt, _>(limit)
        .bind::<BigInt, _>(offset)
        .get_results(conn)
        .await
}

pub async fn screen_stats(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    since: DateTime<Utc>,
    name: &str,
) -> QueryResult<ScreenStats> {
    // $1 app_id, $2 since, $3 name (SCREEN_PRED_EXACT's own bind) — env takes
    // $4 when it needs a bind. No trailing binds after it, so unlike
    // `screen_list` nothing needs to shift.
    let env_sql = scope.env.sql_fragment(4);
    let q = format!(
        "{} \
         SELECT k.screen, \
           COALESCE(ev.views,0)::bigint AS views, \
           COALESCE(ev.events,0)::bigint AS events, \
           COALESCE(ex.exceptions,0)::bigint AS exceptions, \
           COALESCE(us.users,0)::bigint AS users, \
           COALESCE(dw.total_dwell_ms,0)::double precision AS total_dwell_ms, \
           COALESCE(COALESCE(dw.total_dwell_ms,0) / NULLIF(COALESCE(ev.views,0),0), 0)::double precision AS avg_dwell_ms \
         FROM keys k \
         LEFT JOIN ev ON ev.screen=k.screen LEFT JOIN ex ON ex.screen=k.screen \
         LEFT JOIN us ON us.screen=k.screen LEFT JOIN dw ON dw.screen=k.screen \
         WHERE k.screen = $3",
        screen_ctes(SCREEN_PRED_EXACT, &env_sql)
    );
    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Timestamptz, _>(since)
        .bind::<Text, _>(name);
    stmt = crate::bind_env!(stmt, &scope.env);
    stmt.get_result(conn).await
}

pub async fn recent_events_for_screen(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    screen: &str,
    since: DateTime<Utc>,
    limit: i64,
) -> QueryResult<Vec<AnalyticsEvent>> {
    let q = analytics_events::table
        .filter(analytics_events::app_id.eq(scope.app_id))
        .filter(analytics_events::screen.eq(screen))
        .filter(analytics_events::occurred_at.ge(since))
        .filter(analytics_events::name.ne("$screen"))
        .into_boxed();
    crate::scope_env!(q, analytics_events, &scope.env)
        .select(AnalyticsEvent::as_select())
        .order(analytics_events::occurred_at.desc())
        .limit(limit)
        .load(conn)
        .await
}

pub async fn recent_exceptions_for_screen(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    screen: &str,
    since: DateTime<Utc>,
    limit: i64,
) -> QueryResult<Vec<ErrorEvent>> {
    let q = error_events::table
        .filter(error_events::app_id.eq(scope.app_id))
        .filter(error_events::screen.eq(screen))
        .filter(error_events::occurred_at.ge(since))
        .into_boxed();
    crate::scope_env!(q, error_events, &scope.env)
        .select(ErrorEvent::as_select())
        .order(error_events::occurred_at.desc())
        .limit(limit)
        .load(conn)
        .await
}

#[cfg(test)]
mod avg_dwell_tests {
    use super::avg_dwell;

    #[test]
    fn divides_total_by_views() {
        assert!((avg_dwell(9000.0, 3) - 3000.0).abs() < 1e-9);
    }

    #[test]
    fn zero_views_is_zero() {
        assert_eq!(avg_dwell(9000.0, 0), 0.0);
    }
}

// ===========================================================================
// Monitors (uptime checks, keyed by project_id)
// ===========================================================================

#[derive(QueryableByName, serde::Serialize)]
pub struct MonitorListRow {
    #[diesel(sql_type = SqlUuid)]
    pub id: Uuid,
    #[diesel(sql_type = Text)]
    pub name: String,
    #[diesel(sql_type = Text)]
    pub kind: String,
    #[diesel(sql_type = Text)]
    pub target: String,
    #[diesel(sql_type = Text)]
    pub status: String,
    #[diesel(sql_type = Bool)]
    pub enabled: bool,
    #[diesel(sql_type = Nullable<Integer>)]
    pub last_response_time_ms: Option<i32>,
    #[diesel(sql_type = Nullable<Timestamptz>)]
    pub last_checked_at: Option<DateTime<Utc>>,
    #[diesel(sql_type = Nullable<Double>)]
    pub uptime_24h: Option<f64>,
}

#[derive(QueryableByName, serde::Serialize)]
pub struct CheckPoint {
    #[diesel(sql_type = Timestamptz)]
    pub checked_at: DateTime<Utc>,
    #[diesel(sql_type = Bool)]
    pub up: bool,
    #[diesel(sql_type = Nullable<Integer>)]
    pub response_time_ms: Option<i32>,
    #[diesel(sql_type = Nullable<Integer>)]
    pub status_code: Option<i32>,
    #[diesel(sql_type = Nullable<Text>)]
    pub error: Option<String>,
}

/// How many monitors a single project may have.
///
/// Each enabled monitor is polled on its own interval by every prober, so the
/// count directly sets sustained load on the prober fleet and the database.
pub const MAX_MONITORS_PER_PROJECT: i64 = 100;

/// Current monitor count for a project.
pub async fn count_monitors_for_project(
    conn: &mut AsyncPgConnection,
    project_id: Uuid,
) -> QueryResult<i64> {
    monitors::table
        .filter(monitors::project_id.eq(project_id))
        .count()
        .get_result(conn)
        .await
}

pub async fn create_monitor(
    conn: &mut AsyncPgConnection,
    m: NewMonitor<'_>,
) -> QueryResult<Monitor> {
    diesel::insert_into(monitors::table)
        .values(m)
        .returning(Monitor::as_returning())
        .get_result(conn)
        .await
}

pub async fn get_monitor(conn: &mut AsyncPgConnection, id: Uuid) -> QueryResult<Option<Monitor>> {
    monitors::table
        .find(id)
        .select(Monitor::as_select())
        .first(conn)
        .await
        .optional()
}

pub async fn monitor_project(conn: &mut AsyncPgConnection, id: Uuid) -> QueryResult<Option<Uuid>> {
    monitors::table
        .find(id)
        .select(monitors::project_id)
        .first(conn)
        .await
        .optional()
}

pub async fn delete_monitor(conn: &mut AsyncPgConnection, id: Uuid) -> QueryResult<usize> {
    diesel::delete(monitors::table.find(id)).execute(conn).await
}

pub async fn list_incidents(
    conn: &mut AsyncPgConnection,
    monitor_id: Uuid,
    limit: i64,
) -> QueryResult<Vec<MonitorIncidentRow>> {
    monitor_incidents::table
        .filter(monitor_incidents::monitor_id.eq(monitor_id))
        .select(MonitorIncidentRow::as_select())
        .order(monitor_incidents::started_at.desc())
        .limit(limit)
        .load(conn)
        .await
}

pub async fn list_monitors_for_project(
    conn: &mut AsyncPgConnection,
    project_id: Uuid,
) -> QueryResult<Vec<MonitorListRow>> {
    diesel::sql_query(
        "SELECT m.id, m.name, m.kind, m.target, m.status, m.enabled, \
                lc.response_time_ms AS last_response_time_ms, m.last_checked_at, \
                up.pct AS uptime_24h \
         FROM monitors m \
         LEFT JOIN LATERAL ( \
             SELECT response_time_ms FROM monitor_checks c \
             WHERE c.monitor_id = m.id ORDER BY c.checked_at DESC LIMIT 1 \
         ) lc ON TRUE \
         LEFT JOIN LATERAL ( \
             SELECT (100.0 * avg(CASE WHEN c.up THEN 1 ELSE 0 END))::double precision AS pct \
             FROM monitor_checks c \
             WHERE c.monitor_id = m.id AND c.checked_at >= now() - interval '24 hours' \
         ) up ON TRUE \
         WHERE m.project_id = $1 \
         ORDER BY m.created_at ASC",
    )
    .bind::<SqlUuid, _>(project_id)
    .get_results(conn)
    .await
}

#[derive(QueryableByName)]
struct PctRow {
    #[diesel(sql_type = Nullable<Double>)]
    pct: Option<f64>,
}

pub async fn uptime_pct(
    conn: &mut AsyncPgConnection,
    monitor_id: Uuid,
    since_hours: i64,
) -> QueryResult<Option<f64>> {
    let row: PctRow = diesel::sql_query(
        "SELECT (100.0 * avg(CASE WHEN up THEN 1 ELSE 0 END))::double precision AS pct FROM monitor_checks \
         WHERE monitor_id = $1 AND checked_at >= now() - ($2 || ' hours')::interval",
    )
    .bind::<SqlUuid, _>(monitor_id)
    .bind::<Text, _>(since_hours.to_string())
    .get_result(conn)
    .await?;
    Ok(row.pct)
}

pub async fn latency_series(
    conn: &mut AsyncPgConnection,
    monitor_id: Uuid,
    since_hours: i64,
) -> QueryResult<Vec<CheckPoint>> {
    diesel::sql_query(
        "SELECT checked_at, up, response_time_ms, status_code, error FROM monitor_checks \
         WHERE monitor_id = $1 AND checked_at >= now() - ($2 || ' hours')::interval \
         ORDER BY checked_at ASC",
    )
    .bind::<SqlUuid, _>(monitor_id)
    .bind::<Text, _>(since_hours.to_string())
    .get_results(conn)
    .await
}

pub async fn prune_checks(
    conn: &mut AsyncPgConnection,
    older_than_days: i64,
) -> QueryResult<usize> {
    diesel::sql_query(
        "DELETE FROM monitor_checks WHERE checked_at < now() - ($1 || ' days')::interval",
    )
    .bind::<Text, _>(older_than_days.to_string())
    .execute(conn)
    .await
}

/// Delete `alert_events` rows older than `older_than_days`.
///
/// This table is an audit log that grows on every *evaluation*, not just every
/// delivery: a throttled rule writes a `throttled` row each tick it suppresses.
/// A 30s tick on a handful of flapping rules is millions of rows a year, with
/// nothing reclaiming them.
pub async fn prune_alert_events(
    conn: &mut AsyncPgConnection,
    older_than_days: i64,
) -> QueryResult<usize> {
    diesel::sql_query(
        "DELETE FROM alert_events WHERE created_at < now() - ($1 || ' days')::interval",
    )
    .bind::<Text, _>(older_than_days.to_string())
    .execute(conn)
    .await
}

/// Atomically claim due monitors and push their next_check_at forward so no
/// other prober picks the same rows. Returns the claimed rows to probe.
pub async fn claim_due_monitors(
    conn: &mut AsyncPgConnection,
    batch: i64,
) -> QueryResult<Vec<Monitor>> {
    diesel::sql_query(
        "UPDATE monitors SET next_check_at = now() + make_interval(secs => interval_seconds), \
                last_checked_at = now() \
         WHERE id IN ( \
             SELECT id FROM monitors \
             WHERE enabled AND status <> 'paused' AND next_check_at <= now() \
             ORDER BY next_check_at FOR UPDATE SKIP LOCKED LIMIT $1 \
         ) RETURNING *",
    )
    .bind::<BigInt, _>(batch)
    .get_results(conn)
    .await
}

/// Persist one probe result: insert the check row and update the monitor's
/// counters + status. `new_status` is the state machine's decision.
#[allow(clippy::too_many_arguments)]
pub async fn record_check_and_state(
    conn: &mut AsyncPgConnection,
    monitor_id: Uuid,
    up: bool,
    status_code: Option<i32>,
    response_time_ms: Option<i32>,
    error: Option<&str>,
    new_status: &str,
    consecutive_failures: i32,
    consecutive_successes: i32,
    status_changed: bool,
) -> QueryResult<()> {
    diesel::sql_query(
        "INSERT INTO monitor_checks (monitor_id, up, status_code, response_time_ms, error) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind::<SqlUuid, _>(monitor_id)
    .bind::<Bool, _>(up)
    .bind::<Nullable<Integer>, _>(status_code)
    .bind::<Nullable<Integer>, _>(response_time_ms)
    .bind::<Nullable<Text>, _>(error)
    .execute(conn)
    .await?;

    diesel::sql_query(
        "UPDATE monitors SET status = $2, consecutive_failures = $3, consecutive_successes = $4, \
                updated_at = now(), \
                last_status_changed_at = CASE WHEN $5 THEN now() ELSE last_status_changed_at END \
         WHERE id = $1",
    )
    .bind::<SqlUuid, _>(monitor_id)
    .bind::<Text, _>(new_status)
    .bind::<Integer, _>(consecutive_failures)
    .bind::<Integer, _>(consecutive_successes)
    .bind::<Bool, _>(status_changed)
    .execute(conn)
    .await?;
    Ok(())
}

#[derive(QueryableByName)]
struct IdRow {
    #[diesel(sql_type = SqlUuid)]
    id: Uuid,
}

pub async fn open_incident(
    conn: &mut AsyncPgConnection,
    monitor_id: Uuid,
    cause: &str,
    last_error: Option<&str>,
) -> QueryResult<Uuid> {
    // ON CONFLICT on the partial unique index: if an incident is already open,
    // keep it and just refresh last_error.
    let row: IdRow = diesel::sql_query(
        "INSERT INTO monitor_incidents (monitor_id, cause, last_error) VALUES ($1, $2, $3) \
         ON CONFLICT (monitor_id) WHERE resolved_at IS NULL \
         DO UPDATE SET last_error = EXCLUDED.last_error RETURNING id",
    )
    .bind::<SqlUuid, _>(monitor_id)
    .bind::<Text, _>(cause)
    .bind::<Nullable<Text>, _>(last_error)
    .get_result(conn)
    .await?;
    Ok(row.id)
}

pub async fn resolve_incident(conn: &mut AsyncPgConnection, monitor_id: Uuid) -> QueryResult<()> {
    diesel::sql_query(
        "UPDATE monitor_incidents SET resolved_at = now() \
         WHERE monitor_id = $1 AND resolved_at IS NULL",
    )
    .bind::<SqlUuid, _>(monitor_id)
    .execute(conn)
    .await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn update_monitor(
    conn: &mut AsyncPgConnection,
    id: Uuid,
    name: Option<&str>,
    enabled: Option<bool>,
    status: Option<&str>,
    interval_seconds: Option<i32>,
    webhook_url: Option<Option<&str>>, // outer None = leave; inner None = set NULL
) -> QueryResult<Option<Monitor>> {
    // webhook: encode "leave" as a sentinel by splitting into two binds.
    let (set_webhook, webhook_val) = match webhook_url {
        None => (false, None),
        Some(v) => (true, v),
    };
    diesel::sql_query(
        "UPDATE monitors SET \
            name = COALESCE($2, name), \
            enabled = COALESCE($3, enabled), \
            status = COALESCE($4, status), \
            interval_seconds = COALESCE($5, interval_seconds), \
            webhook_url = CASE WHEN $6 THEN $7 ELSE webhook_url END, \
            next_check_at = CASE \
                WHEN $4 = 'unknown' THEN now() \
                WHEN $5 IS NOT NULL THEN now() + make_interval(secs => $5) \
                ELSE next_check_at END, \
            updated_at = now() \
         WHERE id = $1 RETURNING *",
    )
    .bind::<SqlUuid, _>(id)
    .bind::<Nullable<Text>, _>(name)
    .bind::<Nullable<Bool>, _>(enabled)
    .bind::<Nullable<Text>, _>(status)
    .bind::<Nullable<Integer>, _>(interval_seconds)
    .bind::<Bool, _>(set_webhook)
    .bind::<Nullable<Text>, _>(webhook_val)
    .get_result(conn)
    .await
    .optional()
}

// ===========================================================================
// Tiering (hot/cold watermark)
// ===========================================================================

pub async fn get_watermark(
    conn: &mut AsyncPgConnection,
    table: &str,
) -> QueryResult<Option<DateTime<Utc>>> {
    tiering_state::table
        .find(table)
        .select(tiering_state::watermark)
        .first(conn)
        .await
        .optional()
}

/// Upsert the watermark; never moves it backward.
pub async fn advance_watermark(
    conn: &mut AsyncPgConnection,
    table: &str,
    wm: DateTime<Utc>,
) -> QueryResult<()> {
    diesel::insert_into(tiering_state::table)
        .values((
            tiering_state::table_name.eq(table),
            tiering_state::watermark.eq(wm),
            tiering_state::updated_at.eq(Utc::now()),
        ))
        .on_conflict(tiering_state::table_name)
        .do_update()
        .set((
            tiering_state::watermark.eq(diesel::dsl::sql::<Timestamptz>(
                "GREATEST(tiering_state.watermark, EXCLUDED.watermark)",
            )),
            tiering_state::updated_at.eq(Utc::now()),
        ))
        .execute(conn)
        .await?;
    Ok(())
}

pub async fn get_dropped_thru(
    conn: &mut AsyncPgConnection,
    table: &str,
) -> QueryResult<Option<DateTime<Utc>>> {
    tiering_state::table
        .find(table)
        .select(tiering_state::dropped_thru)
        .first::<Option<DateTime<Utc>>>(conn)
        .await
        .optional()
        .map(|o| o.flatten())
}

pub async fn set_dropped_thru(
    conn: &mut AsyncPgConnection,
    table: &str,
    t: DateTime<Utc>,
) -> QueryResult<()> {
    diesel::update(tiering_state::table.find(table))
        .set((
            tiering_state::dropped_thru.eq(Some(t)),
            tiering_state::updated_at.eq(Utc::now()),
        ))
        .execute(conn)
        .await?;
    Ok(())
}

// ===========================================================================
// Partition maintenance
// ===========================================================================

/// Create a range partition if it does not already exist. `table`/`suffix` are
/// internal identifiers (never user input); timestamps are formatted as ISO
/// literals because partition bounds cannot be bound parameters in DDL.
pub async fn create_range_partition(
    conn: &mut AsyncPgConnection,
    table: &str,
    suffix: &str,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> QueryResult<()> {
    let sql = format!(
        "CREATE TABLE IF NOT EXISTS {table}_{suffix} PARTITION OF {table} \
         FOR VALUES FROM ('{start}') TO ('{end}')",
        table = table,
        suffix = suffix,
        start = start.to_rfc3339(),
        end = end.to_rfc3339(),
    );
    diesel::sql_query(sql).execute(conn).await?;
    Ok(())
}

#[derive(diesel::QueryableByName)]
struct ChildName {
    #[diesel(sql_type = Text)]
    child: String,
}

/// Child partition relation names for `table`, excluding the DEFAULT partition.
pub async fn list_child_partitions(
    conn: &mut AsyncPgConnection,
    table: &str,
) -> QueryResult<Vec<String>> {
    let rows: Vec<ChildName> = diesel::sql_query(
        "SELECT c.relname AS child \
         FROM pg_inherits i \
         JOIN pg_class c ON c.oid = i.inhrelid \
         JOIN pg_class p ON p.oid = i.inhparent \
         WHERE p.relname = $1 AND c.relname <> ($1 || '_default') \
         ORDER BY c.relname",
    )
    .bind::<Text, _>(table)
    .load(conn)
    .await?;
    Ok(rows.into_iter().map(|r| r.child).collect())
}

#[derive(diesel::QueryableByName)]
struct CountRow {
    #[diesel(sql_type = BigInt)]
    n: i64,
}

pub async fn count_child_rows(conn: &mut AsyncPgConnection, child: &str) -> QueryResult<i64> {
    // `child` is an internal relation name derived from our own suffix, not user input.
    let row: CountRow = diesel::sql_query(format!("SELECT count(*)::bigint AS n FROM {child}"))
        .get_result(conn)
        .await?;
    Ok(row.n)
}

/// Detach then drop a partition in one transaction. Detach first so the parent
/// is never briefly missing the range.
pub async fn detach_and_drop_partition(
    conn: &mut AsyncPgConnection,
    table: &str,
    child: &str,
) -> QueryResult<()> {
    // Multiple statements in one command require the SIMPLE query protocol.
    // diesel-async's `sql_query(...).execute()` uses the EXTENDED protocol, which
    // rejects "cannot insert multiple commands into a prepared statement".
    // `batch_execute` (SimpleAsyncConnection) sends the BEGIN/DETACH/DROP/COMMIT
    // block via the simple protocol; the explicit transaction keeps it atomic.
    let sql =
        format!("BEGIN; ALTER TABLE {table} DETACH PARTITION {child}; DROP TABLE {child}; COMMIT;");
    conn.batch_execute(&sql).await
}

// ===========================================================================
// Cross-tier reads (hot side)
// ===========================================================================

#[derive(diesel::QueryableByName)]
pub struct DayCountRow {
    #[diesel(sql_type = diesel::sql_types::Date)]
    pub day: chrono::NaiveDate,
    #[diesel(sql_type = BigInt)]
    pub count: i64,
}

/// Per-day error counts from the HOT (Postgres) tier for `[from, to)`.
pub async fn error_counts_by_day_hot(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> QueryResult<Vec<DayCountRow>> {
    diesel::sql_query(
        "SELECT (occurred_at AT TIME ZONE 'UTC')::date AS day, count(*)::bigint AS count \
         FROM error_events \
         WHERE app_id = $1 AND occurred_at >= $2 AND occurred_at < $3 \
         GROUP BY 1 ORDER BY 1",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Timestamptz, _>(from)
    .bind::<Timestamptz, _>(to)
    .load(conn)
    .await
}

/// Per-day counts from ONLY a table's DEFAULT partition, for `[from, to)`.
/// Late-arriving events whose explicit partition was already tiered+dropped land
/// in `<table>_default` (never exported to Parquet). The cross-tier reader adds
/// these to the COLD half so they aren't lost. `default_table` is an INTERNAL
/// identifier (e.g. "error_events_default"), never user input.
pub async fn default_partition_counts_by_day(
    conn: &mut AsyncPgConnection,
    default_table: &str,
    app_id: Uuid,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> QueryResult<Vec<DayCountRow>> {
    diesel::sql_query(format!(
        "SELECT (occurred_at AT TIME ZONE 'UTC')::date AS day, count(*)::bigint AS count \
         FROM {default_table} \
         WHERE app_id = $1 AND occurred_at >= $2 AND occurred_at < $3 \
         GROUP BY 1 ORDER BY 1"
    ))
    .bind::<SqlUuid, _>(app_id)
    .bind::<Timestamptz, _>(from)
    .bind::<Timestamptz, _>(to)
    .load(conn)
    .await
}

/// Per-day analytics-event counts from the HOT (Postgres) tier for `[from, to)`.
pub async fn event_counts_by_day_hot(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> QueryResult<Vec<DayCountRow>> {
    diesel::sql_query(
        "SELECT (occurred_at AT TIME ZONE 'UTC')::date AS day, count(*)::bigint AS count \
         FROM analytics_events \
         WHERE app_id = $1 AND occurred_at >= $2 AND occurred_at < $3 \
         GROUP BY 1 ORDER BY 1",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Timestamptz, _>(from)
    .bind::<Timestamptz, _>(to)
    .load(conn)
    .await
}

/// Per-day transaction THROUGHPUT (count) from the HOT (Postgres) tier for `[from, to)`.
/// ADDITIVE metric only — safe to sum across tiers. Transaction PERCENTILES
/// (p50/p95 of duration_ms) are HOLISTIC and are NOT merged across tiers; those
/// endpoints stay hot-only (Postgres). Do not add percentiles to the cold path
/// without mergeable sketches (t-digest/DDSketch).
pub async fn transaction_counts_by_day_hot(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> QueryResult<Vec<DayCountRow>> {
    diesel::sql_query(
        "SELECT (occurred_at AT TIME ZONE 'UTC')::date AS day, count(*)::bigint AS count \
         FROM transactions \
         WHERE app_id = $1 AND occurred_at >= $2 AND occurred_at < $3 \
         GROUP BY 1 ORDER BY 1",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Timestamptz, _>(from)
    .bind::<Timestamptz, _>(to)
    .load(conn)
    .await
}

// ===========================================================================
// Storage (admin) — sizes and per-app row counts. `table` args are internal
// identifiers from sauron_tier::TIERED_TABLES, never user input.
// ===========================================================================

#[derive(diesel::QueryableByName)]
struct BytesRow {
    #[diesel(sql_type = BigInt)]
    bytes: i64,
}

pub async fn db_total_bytes(conn: &mut AsyncPgConnection) -> QueryResult<i64> {
    let row: BytesRow =
        diesel::sql_query("SELECT pg_database_size(current_database())::bigint AS bytes")
            .get_result(conn)
            .await?;
    Ok(row.bytes)
}

pub async fn table_total_bytes(conn: &mut AsyncPgConnection, table: &str) -> QueryResult<i64> {
    // A partitioned parent has no storage of its own; sum the whole partition
    // tree (parent + children). Works for a non-partitioned table too (tree = self).
    let row: BytesRow = diesel::sql_query(format!(
        "SELECT COALESCE(sum(pg_total_relation_size(relid)), 0)::bigint AS bytes \
         FROM pg_partition_tree('{table}'::regclass)"
    ))
    .get_result(conn)
    .await?;
    Ok(row.bytes)
}

pub async fn table_avg_row_width(conn: &mut AsyncPgConnection, table: &str) -> QueryResult<i64> {
    // pg_stats for a partitioned PARENT is empty until inherited stats exist, so
    // read the whole partition tree. avg_width is per-column; take one representative
    // width per column (max across partitions) then sum → estimated bytes/row.
    let row: BytesRow = diesel::sql_query(
        "SELECT COALESCE(sum(w), 0)::bigint AS bytes FROM ( \
           SELECT s.attname, max(s.avg_width) AS w \
           FROM pg_partition_tree($1::regclass) t \
           JOIN pg_class c ON c.oid = t.relid \
           JOIN pg_namespace n ON n.oid = c.relnamespace \
           JOIN pg_stats s ON s.schemaname = n.nspname AND s.tablename = c.relname \
           GROUP BY s.attname \
         ) x",
    )
    .bind::<Text, _>(table)
    .get_result(conn)
    .await?;
    Ok(row.bytes)
}

#[derive(diesel::QueryableByName)]
pub struct AppCountRow {
    #[diesel(sql_type = SqlUuid)]
    pub app_id: Uuid,
    #[diesel(sql_type = BigInt)]
    pub n: i64,
}

pub async fn hot_rows_by_app(
    conn: &mut AsyncPgConnection,
    table: &str,
) -> QueryResult<Vec<AppCountRow>> {
    diesel::sql_query(format!(
        "SELECT app_id, count(*)::bigint AS n FROM {table} GROUP BY app_id"
    ))
    .load(conn)
    .await
}

#[derive(diesel::QueryableByName)]
pub struct AppOrgRow {
    #[diesel(sql_type = SqlUuid)]
    pub app_id: Uuid,
    #[diesel(sql_type = Text)]
    pub app_name: String,
    #[diesel(sql_type = Text)]
    pub project_name: String,
    #[diesel(sql_type = Text)]
    pub org_name: String,
}

pub async fn list_apps_with_org(conn: &mut AsyncPgConnection) -> QueryResult<Vec<AppOrgRow>> {
    diesel::sql_query(
        "SELECT a.id AS app_id, a.name AS app_name, p.name AS project_name, o.name AS org_name \
         FROM apps a JOIN projects p ON a.project_id = p.id \
         JOIN organizations o ON p.org_id = o.id \
         ORDER BY o.name, p.name, a.name",
    )
    .load(conn)
    .await
}

/// Apps belonging to `org_ids` only — the tenant-scoped form of
/// [`list_apps_with_org`], used by the storage report so a caller never sees
/// apps outside the orgs they administer.
pub async fn list_apps_with_org_scoped(
    conn: &mut AsyncPgConnection,
    org_ids: &[Uuid],
) -> QueryResult<Vec<AppOrgRow>> {
    diesel::sql_query(
        "SELECT a.id AS app_id, a.name AS app_name, p.name AS project_name, o.name AS org_name \
         FROM apps a JOIN projects p ON a.project_id = p.id \
         JOIN organizations o ON p.org_id = o.id \
         WHERE o.id = ANY($1) \
         ORDER BY o.name, p.name, a.name",
    )
    .bind::<diesel::sql_types::Array<SqlUuid>, _>(org_ids)
    .load(conn)
    .await
}

/// Per-app hot row counts restricted to `app_ids`.
///
/// The unscoped [`hot_rows_by_app`] scans every partition of the largest tables
/// in the deployment; restricting by `app_id` lets the planner use the app-keyed
/// indexes and bounds the work to the caller's own data.
pub async fn hot_rows_by_app_scoped(
    conn: &mut AsyncPgConnection,
    table: &str,
    app_ids: &[Uuid],
) -> QueryResult<Vec<AppCountRow>> {
    // `table` is never user input: callers pass a literal from TIERED_TABLES.
    diesel::sql_query(format!(
        "SELECT app_id, count(*)::bigint AS n FROM {table} WHERE app_id = ANY($1) GROUP BY app_id"
    ))
    .bind::<diesel::sql_types::Array<SqlUuid>, _>(app_ids)
    .load(conn)
    .await
}

/// The orgs in which `user_id` holds an **org-scoped** grant carrying `permission`.
///
/// Used to scope deployment-wide reports to the tenants a caller actually
/// administers.
pub async fn orgs_with_permission(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    permission: &str,
) -> QueryResult<Vec<Uuid>> {
    diesel::sql_query(
        "SELECT DISTINCT g.org_id AS id \
         FROM role_grants g JOIN roles r ON g.role_id = r.id \
         WHERE g.user_id = $1 AND g.scope_type = 'org' \
           AND r.permissions @> to_jsonb($2::text)",
    )
    .bind::<SqlUuid, _>(user_id)
    .bind::<Text, _>(permission)
    .load::<IdRow>(conn)
    .await
    .map(|rows| rows.into_iter().map(|r| r.id).collect())
}

// ===========================================================================
// Symbol artifacts (source maps / Dart debug-info), content-addressed
// ===========================================================================

/// Insert a content-addressed blob, or bump its refcount if it already exists.
pub async fn put_blob(
    conn: &mut AsyncPgConnection,
    sha: &[u8],
    compressed: &[u8],
    uncompressed_size: i64,
    compressed_size: i64,
) -> QueryResult<()> {
    diesel::insert_into(symbol_blobs::table)
        .values(NewSymbolBlob {
            sha256: sha,
            content: compressed,
            uncompressed_size,
            compressed_size,
            refcount: 1,
        })
        .on_conflict(symbol_blobs::sha256)
        .do_update()
        .set(symbol_blobs::refcount.eq(symbol_blobs::refcount + 1))
        .execute(conn)
        .await?;
    Ok(())
}

/// Cheap indexed check: does this app have ANY symbol artifacts uploaded? Lets
/// the ingest path skip a per-error artifact lookup for apps that use no symbols.
pub async fn app_has_symbol_artifacts(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
) -> QueryResult<bool> {
    diesel::select(diesel::dsl::exists(
        symbol_artifacts::table.filter(symbol_artifacts::app_id.eq(app_id)),
    ))
    .get_result(conn)
    .await
}

/// Fetch the compressed bytes of a blob by content hash.
pub async fn get_blob(conn: &mut AsyncPgConnection, sha: &[u8]) -> QueryResult<Option<Vec<u8>>> {
    symbol_blobs::table
        .filter(symbol_blobs::sha256.eq(sha))
        .select(symbol_blobs::content)
        .first::<Vec<u8>>(conn)
        .await
        .optional()
}

/// Persist symbolicated frames + status onto an error event (by its composite
/// PK: id + occurred_at). Used by the on-read backfill for hot partitions.
pub async fn update_event_symbolication(
    conn: &mut AsyncPgConnection,
    event_id: Uuid,
    occurred_at: DateTime<Utc>,
    frames: Value,
    status: &str,
) -> QueryResult<usize> {
    diesel::update(
        error_events::table
            .filter(error_events::id.eq(event_id))
            .filter(error_events::occurred_at.eq(occurred_at)),
    )
    .set((
        error_events::stacktrace_symbolicated.eq(Some(frames)),
        error_events::symbolication_status.eq(status.to_string()),
    ))
    .execute(conn)
    .await
}

pub async fn insert_symbol_artifact(
    conn: &mut AsyncPgConnection,
    art: NewSymbolArtifact,
) -> QueryResult<SymbolArtifact> {
    diesel::insert_into(symbol_artifacts::table)
        .values(&art)
        .returning(SymbolArtifact::as_returning())
        .get_result(conn)
        .await
}

pub async fn list_symbol_artifacts(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
) -> QueryResult<Vec<SymbolArtifact>> {
    symbol_artifacts::table
        .filter(symbol_artifacts::app_id.eq(app_id))
        .select(SymbolArtifact::as_select())
        .order(symbol_artifacts::created_at.desc())
        .load(conn)
        .await
}

/// List artifacts for an app joined to their blob sizes (uncompressed, compressed).
pub async fn list_artifacts_with_sizes(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
) -> QueryResult<Vec<(SymbolArtifact, i64, i64)>> {
    symbol_artifacts::table
        .inner_join(symbol_blobs::table.on(symbol_artifacts::blob_sha256.eq(symbol_blobs::sha256)))
        .filter(symbol_artifacts::app_id.eq(app_id))
        .select((
            SymbolArtifact::as_select(),
            symbol_blobs::uncompressed_size,
            symbol_blobs::compressed_size,
        ))
        .order(symbol_artifacts::created_at.desc())
        .load(conn)
        .await
}

pub async fn get_symbol_artifact(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    id: Uuid,
) -> QueryResult<Option<SymbolArtifact>> {
    symbol_artifacts::table
        .filter(symbol_artifacts::app_id.eq(app_id))
        .filter(symbol_artifacts::id.eq(id))
        .select(SymbolArtifact::as_select())
        .first(conn)
        .await
        .optional()
}

/// Idempotency lookup by Dart build-id.
pub async fn find_artifact_by_debug_id(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    debug_id: &str,
) -> QueryResult<Option<SymbolArtifact>> {
    symbol_artifacts::table
        .filter(symbol_artifacts::app_id.eq(app_id))
        .filter(symbol_artifacts::debug_id.eq(debug_id))
        .select(SymbolArtifact::as_select())
        .first(conn)
        .await
        .optional()
}

/// Idempotency lookup by (release, name, blob) for JS uploads.
pub async fn find_artifact_by_release_name(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    release: Option<&str>,
    name: Option<&str>,
    blob_sha: &[u8],
) -> QueryResult<Option<SymbolArtifact>> {
    let mut q = symbol_artifacts::table
        .filter(symbol_artifacts::app_id.eq(app_id))
        .filter(symbol_artifacts::blob_sha256.eq(blob_sha.to_vec()))
        .into_boxed();
    q = match release {
        Some(r) => q.filter(symbol_artifacts::release.eq(r.to_string())),
        None => q.filter(symbol_artifacts::release.is_null()),
    };
    q = match name {
        Some(n) => q.filter(symbol_artifacts::name.eq(n.to_string())),
        None => q.filter(symbol_artifacts::name.is_null()),
    };
    q.select(SymbolArtifact::as_select())
        .first(conn)
        .await
        .optional()
}

/// All artifacts uploaded for a release (used by the JS matcher). Newest first,
/// so re-uploads with the same (release, name) win deterministically.
pub async fn find_artifacts_for_release(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    release: &str,
) -> QueryResult<Vec<SymbolArtifact>> {
    symbol_artifacts::table
        .filter(symbol_artifacts::app_id.eq(app_id))
        .filter(symbol_artifacts::release.eq(release))
        .select(SymbolArtifact::as_select())
        .order(symbol_artifacts::created_at.desc())
        .load(conn)
        .await
}

/// Delete an artifact (scoped to `app_id`), decrement referenced blob refcounts,
/// and GC any blob that reaches zero. Returns false if the artifact wasn't found.
///
/// Not wrapped in a transaction: a crash mid-way can leave a blob with a stale
/// refcount (orphaned, harmless) — acceptable for the MVP artifact store.
pub async fn delete_symbol_artifact(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    id: Uuid,
) -> QueryResult<bool> {
    let art = match get_symbol_artifact(conn, app_id, id).await? {
        Some(a) => a,
        None => return Ok(false),
    };
    diesel::delete(
        symbol_artifacts::table
            .filter(symbol_artifacts::app_id.eq(app_id))
            .filter(symbol_artifacts::id.eq(id)),
    )
    .execute(conn)
    .await?;

    let mut hashes = vec![art.blob_sha256];
    if let Some(idx) = art.prebuilt_index_sha256 {
        if !hashes.contains(&idx) {
            hashes.push(idx);
        }
    }
    for h in hashes {
        diesel::update(symbol_blobs::table.filter(symbol_blobs::sha256.eq(&h)))
            .set(symbol_blobs::refcount.eq(symbol_blobs::refcount - 1))
            .execute(conn)
            .await?;
        diesel::delete(
            symbol_blobs::table
                .filter(symbol_blobs::sha256.eq(&h))
                .filter(symbol_blobs::refcount.le(0)),
        )
        .execute(conn)
        .await?;
    }
    Ok(true)
}

// ===========================================================================
// Alerting: notification channels, rules, deliveries
// ===========================================================================

pub async fn create_channel(
    conn: &mut AsyncPgConnection,
    ch: NewNotificationChannel<'_>,
) -> QueryResult<NotificationChannel> {
    diesel::insert_into(notification_channels::table)
        .values(ch)
        .returning(NotificationChannel::as_returning())
        .get_result(conn)
        .await
}

pub async fn list_channels_for_org(
    conn: &mut AsyncPgConnection,
    org_id: Uuid,
) -> QueryResult<Vec<NotificationChannel>> {
    notification_channels::table
        .filter(notification_channels::org_id.eq(org_id))
        .order(notification_channels::created_at.desc())
        .limit(500)
        .select(NotificationChannel::as_select())
        .load(conn)
        .await
}

pub async fn get_channel(
    conn: &mut AsyncPgConnection,
    id: Uuid,
) -> QueryResult<Option<NotificationChannel>> {
    notification_channels::table
        .filter(notification_channels::id.eq(id))
        .select(NotificationChannel::as_select())
        .first(conn)
        .await
        .optional()
}

/// Update a channel's mutable fields. `secret_enc`: `None` = leave unchanged,
/// `Some(None)` = clear, `Some(Some(blob))` = replace.
pub async fn update_channel(
    conn: &mut AsyncPgConnection,
    id: Uuid,
    name: Option<&str>,
    config: Option<&Value>,
    secret_enc: Option<Option<Vec<u8>>>,
    enabled: Option<bool>,
) -> QueryResult<Option<NotificationChannel>> {
    let mut any = false;
    if let Some(n) = name {
        diesel::update(notification_channels::table.filter(notification_channels::id.eq(id)))
            .set(notification_channels::name.eq(n))
            .execute(conn)
            .await?;
        any = true;
    }
    if let Some(c) = config {
        diesel::update(notification_channels::table.filter(notification_channels::id.eq(id)))
            .set(notification_channels::config.eq(c))
            .execute(conn)
            .await?;
        any = true;
    }
    if let Some(s) = secret_enc {
        diesel::update(notification_channels::table.filter(notification_channels::id.eq(id)))
            .set(notification_channels::secret_enc.eq(s))
            .execute(conn)
            .await?;
        any = true;
    }
    if let Some(e) = enabled {
        diesel::update(notification_channels::table.filter(notification_channels::id.eq(id)))
            .set(notification_channels::enabled.eq(e))
            .execute(conn)
            .await?;
        any = true;
    }
    if any {
        diesel::update(notification_channels::table.filter(notification_channels::id.eq(id)))
            .set(notification_channels::updated_at.eq(Utc::now()))
            .execute(conn)
            .await?;
    }
    get_channel(conn, id).await
}

pub async fn delete_channel(conn: &mut AsyncPgConnection, id: Uuid) -> QueryResult<usize> {
    diesel::delete(notification_channels::table.filter(notification_channels::id.eq(id)))
        .execute(conn)
        .await
}

pub async fn create_alert_rule(
    conn: &mut AsyncPgConnection,
    rule: NewAlertRule<'_>,
) -> QueryResult<AlertRule> {
    diesel::insert_into(alert_rules::table)
        .values(rule)
        .returning(AlertRule::as_returning())
        .get_result(conn)
        .await
}

pub async fn list_alert_rules_for_org(
    conn: &mut AsyncPgConnection,
    org_id: Uuid,
) -> QueryResult<Vec<AlertRule>> {
    alert_rules::table
        .filter(alert_rules::org_id.eq(org_id))
        .order(alert_rules::created_at.desc())
        .limit(500)
        .select(AlertRule::as_select())
        .load(conn)
        .await
}

pub async fn get_alert_rule(
    conn: &mut AsyncPgConnection,
    id: Uuid,
) -> QueryResult<Option<AlertRule>> {
    alert_rules::table
        .filter(alert_rules::id.eq(id))
        .select(AlertRule::as_select())
        .first(conn)
        .await
        .optional()
}

#[allow(clippy::too_many_arguments)]
pub async fn update_alert_rule(
    conn: &mut AsyncPgConnection,
    id: Uuid,
    name: Option<&str>,
    enabled: Option<bool>,
    conditions: Option<&Value>,
    severity: Option<&str>,
    throttle_seconds: Option<i32>,
    message_template: Option<Option<&str>>,
) -> QueryResult<Option<AlertRule>> {
    if let Some(n) = name {
        diesel::update(alert_rules::table.filter(alert_rules::id.eq(id)))
            .set(alert_rules::name.eq(n))
            .execute(conn)
            .await?;
    }
    if let Some(e) = enabled {
        diesel::update(alert_rules::table.filter(alert_rules::id.eq(id)))
            .set(alert_rules::enabled.eq(e))
            .execute(conn)
            .await?;
    }
    if let Some(c) = conditions {
        diesel::update(alert_rules::table.filter(alert_rules::id.eq(id)))
            .set(alert_rules::conditions.eq(c))
            .execute(conn)
            .await?;
    }
    if let Some(s) = severity {
        diesel::update(alert_rules::table.filter(alert_rules::id.eq(id)))
            .set(alert_rules::severity.eq(s))
            .execute(conn)
            .await?;
    }
    if let Some(t) = throttle_seconds {
        diesel::update(alert_rules::table.filter(alert_rules::id.eq(id)))
            .set(alert_rules::throttle_seconds.eq(t))
            .execute(conn)
            .await?;
    }
    if let Some(m) = message_template {
        diesel::update(alert_rules::table.filter(alert_rules::id.eq(id)))
            .set(alert_rules::message_template.eq(m))
            .execute(conn)
            .await?;
    }
    diesel::update(alert_rules::table.filter(alert_rules::id.eq(id)))
        .set(alert_rules::updated_at.eq(Utc::now()))
        .execute(conn)
        .await?;
    get_alert_rule(conn, id).await
}

pub async fn delete_alert_rule(conn: &mut AsyncPgConnection, id: Uuid) -> QueryResult<usize> {
    diesel::delete(alert_rules::table.filter(alert_rules::id.eq(id)))
        .execute(conn)
        .await
}

/// Replace a rule's channel attachments with `channel_ids` (already validated
/// as belonging to the rule's org by the route layer).
pub async fn set_rule_channels(
    conn: &mut AsyncPgConnection,
    rule_id: Uuid,
    channel_ids: &[Uuid],
) -> QueryResult<()> {
    diesel::delete(alert_rule_channels::table.filter(alert_rule_channels::rule_id.eq(rule_id)))
        .execute(conn)
        .await?;
    for cid in channel_ids {
        diesel::insert_into(alert_rule_channels::table)
            .values((
                alert_rule_channels::rule_id.eq(rule_id),
                alert_rule_channels::channel_id.eq(*cid),
            ))
            .on_conflict_do_nothing()
            .execute(conn)
            .await?;
    }
    Ok(())
}

pub async fn rule_channel_ids(
    conn: &mut AsyncPgConnection,
    rule_id: Uuid,
) -> QueryResult<Vec<Uuid>> {
    alert_rule_channels::table
        .filter(alert_rule_channels::rule_id.eq(rule_id))
        .select(alert_rule_channels::channel_id)
        .load(conn)
        .await
}

/// Channel ids for many rules at once, grouped by rule.
///
/// The rules list rendered one `rule_channel_ids` query per rule, so an org with
/// 200 rules issued 201 queries per page load.
pub async fn rule_channel_ids_for_rules(
    conn: &mut AsyncPgConnection,
    rule_ids: &[Uuid],
) -> QueryResult<HashMap<Uuid, Vec<Uuid>>> {
    let rows: Vec<(Uuid, Uuid)> = alert_rule_channels::table
        .filter(alert_rule_channels::rule_id.eq_any(rule_ids))
        .select((
            alert_rule_channels::rule_id,
            alert_rule_channels::channel_id,
        ))
        .load(conn)
        .await?;
    let mut out: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
    for (rule_id, channel_id) in rows {
        out.entry(rule_id).or_default().push(channel_id);
    }
    Ok(out)
}

/// The (enabled or not) channels attached to a rule.
pub async fn channels_for_rule(
    conn: &mut AsyncPgConnection,
    rule_id: Uuid,
) -> QueryResult<Vec<NotificationChannel>> {
    alert_rule_channels::table
        .inner_join(notification_channels::table)
        .filter(alert_rule_channels::rule_id.eq(rule_id))
        .select(NotificationChannel::as_select())
        .load(conn)
        .await
}

pub async fn insert_alert_event(
    conn: &mut AsyncPgConnection,
    ev: NewAlertEvent<'_>,
) -> QueryResult<usize> {
    diesel::insert_into(alert_events::table)
        .values(ev)
        .execute(conn)
        .await
}

/// Durable throttle backstop: was an alert with this dedup key *sent* within
/// the last `within_seconds`? (Used when Redis is unavailable.)
pub async fn alert_recently_sent(
    conn: &mut AsyncPgConnection,
    dedup_key: &str,
    within_seconds: i32,
) -> QueryResult<bool> {
    let cutoff = Utc::now() - chrono::Duration::seconds(within_seconds.max(0) as i64);
    let n: i64 = alert_events::table
        .filter(alert_events::dedup_key.eq(dedup_key))
        .filter(alert_events::status.eq("sent"))
        .filter(alert_events::created_at.gt(cutoff))
        .count()
        .get_result(conn)
        .await?;
    Ok(n > 0)
}

/// Paginated alert history for an org (bounded).
pub async fn list_alert_events(
    conn: &mut AsyncPgConnection,
    org_id: Uuid,
    limit: i64,
    offset: i64,
) -> QueryResult<Vec<AlertEventRow>> {
    alert_events::table
        .filter(alert_events::org_id.eq(org_id))
        .order(alert_events::created_at.desc())
        .limit(limit.clamp(1, 200))
        .offset(offset.clamp(0, 100_000))
        .select(AlertEventRow::as_select())
        .load(conn)
        .await
}

/// Enabled rules the evaluator polls (all metric trigger types).
pub async fn enabled_metric_alert_rules(
    conn: &mut AsyncPgConnection,
) -> QueryResult<Vec<AlertRule>> {
    alert_rules::table
        .filter(alert_rules::enabled.eq(true))
        .filter(alert_rules::trigger_type.ne_all(vec!["monitor_down", "monitor_up"]))
        .select(AlertRule::as_select())
        .load(conn)
        .await
}

/// Enabled monitor-transition rules that apply to `project_id` (org-wide rules
/// plus rules narrowed to exactly this project).
pub async fn alert_rules_for_monitor(
    conn: &mut AsyncPgConnection,
    project_id: Uuid,
    trigger_type: &str,
) -> QueryResult<Vec<AlertRule>> {
    let org: Option<Uuid> = projects::table
        .filter(projects::id.eq(project_id))
        .select(projects::org_id)
        .first(conn)
        .await
        .optional()?;
    let Some(org_id) = org else {
        return Ok(Vec::new());
    };
    alert_rules::table
        .filter(alert_rules::enabled.eq(true))
        .filter(alert_rules::trigger_type.eq(trigger_type))
        .filter(alert_rules::org_id.eq(org_id))
        .filter(
            alert_rules::project_id
                .is_null()
                .or(alert_rules::project_id.eq(project_id)),
        )
        .select(AlertRule::as_select())
        .load(conn)
        .await
}

pub async fn touch_rule_evaluated(
    conn: &mut AsyncPgConnection,
    rule_id: Uuid,
    at: DateTime<Utc>,
) -> QueryResult<usize> {
    diesel::update(alert_rules::table.filter(alert_rules::id.eq(rule_id)))
        .set(alert_rules::last_evaluated_at.eq(at))
        .execute(conn)
        .await
}

/// The app ids a rule's scope covers (org-wide, project-narrowed, or one app).
pub async fn apps_in_alert_scope(
    conn: &mut AsyncPgConnection,
    org_id: Uuid,
    project_id: Option<Uuid>,
    app_id: Option<Uuid>,
) -> QueryResult<Vec<Uuid>> {
    let mut q = apps::table
        .inner_join(projects::table)
        .filter(projects::org_id.eq(org_id))
        .into_boxed();
    if let Some(p) = project_id {
        q = q.filter(apps::project_id.eq(p));
    }
    if let Some(a) = app_id {
        q = q.filter(apps::id.eq(a));
    }
    q.select(apps::id).load(conn).await
}

#[derive(Debug, QueryableByName)]
pub struct AlertCountRow {
    #[diesel(sql_type = BigInt)]
    pub n: i64,
}

#[derive(Debug, QueryableByName)]
pub struct AlertValueRow {
    #[diesel(sql_type = Nullable<Double>)]
    pub v: Option<f64>,
}

/// Count error events across `app_ids` in `(from, to]`, with optional
/// level/environment/tag filters. All values are bound parameters.
pub async fn alert_count_errors(
    conn: &mut AsyncPgConnection,
    app_ids: &[Uuid],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    level: Option<&str>,
    environment: Option<&str>,
    tag: Option<&Value>,
) -> QueryResult<i64> {
    // `retired_at IS NULL` is load-bearing: (app_id, name) is only unique among
    // LIVE environments, so retiring `staging` and creating a fresh `staging`
    // leaves two rows with that name. Without this filter the subquery returns
    // both ids and the count silently includes the retired environment's events
    // too. The partial unique index guarantees at most one live match per name,
    // so this is deterministic.
    let row: AlertCountRow = diesel::sql_query(
        "SELECT count(*) AS n FROM error_events \
         WHERE app_id = ANY($1) AND occurred_at > $2 AND occurred_at <= $3 \
           AND ($4::text IS NULL OR level = $4) \
           AND ($5::text IS NULL OR environment_id IN (SELECT id FROM environments WHERE name = $5 AND retired_at IS NULL)) \
           AND ($6::jsonb IS NULL OR tags @> $6)",
    )
    .bind::<diesel::sql_types::Array<SqlUuid>, _>(app_ids)
    .bind::<Timestamptz, _>(from)
    .bind::<Timestamptz, _>(to)
    .bind::<Nullable<Text>, _>(level)
    .bind::<Nullable<Text>, _>(environment)
    .bind::<Nullable<Jsonb>, _>(tag)
    .get_result(conn)
    .await?;
    Ok(row.n)
}

/// Count analytics events across `app_ids` in `(from, to]`, with optional
/// name/environment/tag filters.
pub async fn alert_count_events(
    conn: &mut AsyncPgConnection,
    app_ids: &[Uuid],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    name: Option<&str>,
    environment: Option<&str>,
    tag: Option<&Value>,
) -> QueryResult<i64> {
    // `retired_at IS NULL` is load-bearing: (app_id, name) is only unique among
    // LIVE environments, so retiring `staging` and creating a fresh `staging`
    // leaves two rows with that name. Without this filter the subquery returns
    // both ids and the count silently includes the retired environment's events
    // too. The partial unique index guarantees at most one live match per name,
    // so this is deterministic.
    let row: AlertCountRow = diesel::sql_query(
        "SELECT count(*) AS n FROM analytics_events \
         WHERE app_id = ANY($1) AND occurred_at > $2 AND occurred_at <= $3 \
           AND ($4::text IS NULL OR name = $4) \
           AND ($5::text IS NULL OR environment_id IN (SELECT id FROM environments WHERE name = $5 AND retired_at IS NULL)) \
           AND ($6::jsonb IS NULL OR tags @> $6)",
    )
    .bind::<diesel::sql_types::Array<SqlUuid>, _>(app_ids)
    .bind::<Timestamptz, _>(from)
    .bind::<Timestamptz, _>(to)
    .bind::<Nullable<Text>, _>(name)
    .bind::<Nullable<Text>, _>(environment)
    .bind::<Nullable<Jsonb>, _>(tag)
    .get_result(conn)
    .await?;
    Ok(row.n)
}

/// A latency metric over transactions in the window. `percentile` is the
/// fraction for percentile_cont; `None` means avg, `Some(-1.0)` means max
/// (the caller maps the whitelisted metric string).
pub async fn alert_latency_metric(
    conn: &mut AsyncPgConnection,
    app_ids: &[Uuid],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    percentile: Option<f64>,
    op: Option<&str>,
) -> QueryResult<Option<f64>> {
    let row: AlertValueRow = match percentile {
        Some(p) if p >= 0.0 => {
            diesel::sql_query(
                "SELECT percentile_cont($4) WITHIN GROUP (ORDER BY duration_ms)::double precision AS v \
                 FROM transactions \
                 WHERE app_id = ANY($1) AND occurred_at > $2 AND occurred_at <= $3 \
                   AND ($5::text IS NULL OR op = $5)",
            )
            .bind::<diesel::sql_types::Array<SqlUuid>, _>(app_ids)
            .bind::<Timestamptz, _>(from)
            .bind::<Timestamptz, _>(to)
            .bind::<Double, _>(p)
            .bind::<Nullable<Text>, _>(op)
            .get_result(conn)
            .await?
        }
        Some(_) => {
            diesel::sql_query(
                "SELECT max(duration_ms)::double precision AS v FROM transactions \
                 WHERE app_id = ANY($1) AND occurred_at > $2 AND occurred_at <= $3 \
                   AND ($4::text IS NULL OR op = $4)",
            )
            .bind::<diesel::sql_types::Array<SqlUuid>, _>(app_ids)
            .bind::<Timestamptz, _>(from)
            .bind::<Timestamptz, _>(to)
            .bind::<Nullable<Text>, _>(op)
            .get_result(conn)
            .await?
        }
        None => {
            diesel::sql_query(
                "SELECT avg(duration_ms)::double precision AS v FROM transactions \
                 WHERE app_id = ANY($1) AND occurred_at > $2 AND occurred_at <= $3 \
                   AND ($4::text IS NULL OR op = $4)",
            )
            .bind::<diesel::sql_types::Array<SqlUuid>, _>(app_ids)
            .bind::<Timestamptz, _>(from)
            .bind::<Timestamptz, _>(to)
            .bind::<Nullable<Text>, _>(op)
            .get_result(conn)
            .await?
        }
    };
    Ok(row.v)
}

#[derive(Debug, QueryableByName, serde::Serialize)]
pub struct AlertIssueBrief {
    #[diesel(sql_type = SqlUuid)]
    pub id: Uuid,
    #[diesel(sql_type = SqlUuid)]
    pub app_id: Uuid,
    #[diesel(sql_type = Text)]
    pub title: String,
    #[diesel(sql_type = Text)]
    pub level: String,
    #[diesel(sql_type = BigInt)]
    pub times_seen: i64,
}

/// Issues first seen in `(from, to]` (new-issue trigger). Bounded.
pub async fn alert_new_issues(
    conn: &mut AsyncPgConnection,
    app_ids: &[Uuid],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    level: Option<&str>,
) -> QueryResult<Vec<AlertIssueBrief>> {
    diesel::sql_query(
        // `created_at`, not `first_seen`: the latter is the SDK-supplied event
        // timestamp, while the evaluator's watermark moves on its own clock and
        // the row only lands after pipeline latency. A tick landing in that gap
        // advanced the watermark past `first_seen` and the issue was never
        // alerted; backdated/offline batches lost the same way. `created_at` is
        // Postgres `now()` at INSERT, so it can never predate the watermark.
        "SELECT id, app_id, title, level, times_seen FROM issues \
         WHERE app_id = ANY($1) AND created_at > $2 AND created_at <= $3 \
           AND ($4::text IS NULL OR level = $4) \
         ORDER BY created_at DESC LIMIT 20",
    )
    .bind::<diesel::sql_types::Array<SqlUuid>, _>(app_ids)
    .bind::<Timestamptz, _>(from)
    .bind::<Timestamptz, _>(to)
    .bind::<Nullable<Text>, _>(level)
    .load(conn)
    .await
}

/// Resolved/ignored issues that saw new events in `(from, to]` (regression
/// trigger). `upsert_issue` advances `last_seen` without resetting `status`,
/// so this catches the recurrence. Bounded.
pub async fn alert_regressed_issues(
    conn: &mut AsyncPgConnection,
    app_ids: &[Uuid],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    level: Option<&str>,
) -> QueryResult<Vec<AlertIssueBrief>> {
    diesel::sql_query(
        // `last_event_at` is the ingest-side twin of `last_seen`, advanced only
        // by `upsert_issue`. See `alert_new_issues` for why the client-supplied
        // column loses the race with the poll tick.
        "SELECT id, app_id, title, level, times_seen FROM issues \
         WHERE app_id = ANY($1) AND status IN ('resolved','ignored') \
           AND last_event_at > $2 AND last_event_at <= $3 \
           AND ($4::text IS NULL OR level = $4) \
         ORDER BY last_event_at DESC LIMIT 20",
    )
    .bind::<diesel::sql_types::Array<SqlUuid>, _>(app_ids)
    .bind::<Timestamptz, _>(from)
    .bind::<Timestamptz, _>(to)
    .bind::<Nullable<Text>, _>(level)
    .load(conn)
    .await
}
