//! Repository functions. Each takes `&mut AsyncPgConnection` and returns a
//! `QueryResult`. Grouped by domain.

use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel::sql_types::{
    BigInt, Bool, Double, Integer, Jsonb, Nullable, Text, Timestamptz, Uuid as SqlUuid,
};
use diesel::upsert::excluded;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use serde_json::Value;
use uuid::Uuid;

use crate::models::*;
use crate::schema::*;

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

pub async fn revoke_refresh_token(
    conn: &mut AsyncPgConnection,
    token_hash: &str,
) -> QueryResult<usize> {
    diesel::update(refresh_tokens::table.filter(refresh_tokens::token_hash.eq(token_hash)))
        .set(refresh_tokens::revoked_at.eq(Utc::now()))
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

/// The org a grant belongs to (for authorizing its deletion).
pub async fn grant_org(conn: &mut AsyncPgConnection, grant_id: Uuid) -> QueryResult<Option<Uuid>> {
    role_grants::table
        .find(grant_id)
        .select(role_grants::org_id)
        .first(conn)
        .await
        .optional()
}

/// All grants in an org with the user email/name and role name, for the
/// members page.
pub async fn list_org_grants(
    conn: &mut AsyncPgConnection,
    org_id: Uuid,
) -> QueryResult<Vec<(RoleGrant, String, String, String)>> {
    role_grants::table
        .inner_join(users::table.on(users::id.eq(role_grants::user_id)))
        .inner_join(roles::table.on(roles::id.eq(role_grants::role_id)))
        .filter(role_grants::org_id.eq(org_id))
        .select((
            RoleGrant::as_select(),
            users::email,
            users::name,
            roles::name,
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

// ===========================================================================
// Apps (ingest unit)
// ===========================================================================

pub async fn create_app(
    conn: &mut AsyncPgConnection,
    project_id: Uuid,
    name: &str,
    slug: &str,
    app_type: &str,
    public_key: &str,
) -> QueryResult<App> {
    diesel::insert_into(apps::table)
        .values(NewApp {
            project_id,
            name,
            slug,
            app_type,
            public_key,
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

pub async fn find_app_by_public_key(
    conn: &mut AsyncPgConnection,
    public_key: &str,
) -> QueryResult<Option<App>> {
    apps::table
        .filter(apps::public_key.eq(public_key))
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

pub async fn rotate_app_key(
    conn: &mut AsyncPgConnection,
    id: Uuid,
    new_key: &str,
) -> QueryResult<App> {
    diesel::update(apps::table.find(id))
        .set((
            apps::public_key.eq(new_key),
            apps::updated_at.eq(Utc::now()),
        ))
        .returning(App::as_returning())
        .get_result(conn)
        .await
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

// --- environments -----------------------------------------------------------

pub async fn upsert_environment(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    name: &str,
) -> QueryResult<Environment> {
    diesel::insert_into(environments::table)
        .values(NewEnvironment { app_id, name })
        .on_conflict((environments::app_id, environments::name))
        .do_update()
        .set(environments::name.eq(excluded(environments::name)))
        .returning(Environment::as_returning())
        .get_result(conn)
        .await
}

pub async fn list_environments(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
) -> QueryResult<Vec<Environment>> {
    environments::table
        .filter(environments::app_id.eq(app_id))
        .select(Environment::as_select())
        .order(environments::name.asc())
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

pub async fn list_issues(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    filters: &[ParsedFilter],
    q: Option<&str>,
    since: Option<chrono::DateTime<chrono::Utc>>,
    limit: i64,
    offset: i64,
) -> QueryResult<Vec<Issue>> {
    let mut query = issues::table.filter(issues::app_id.eq(app_id)).into_boxed();
    if let Some(s) = since {
        query = query.filter(issues::last_seen.ge(s));
    }
    for f in filters {
        query = match (f.field, f.op) {
            ("level", Op::Eq) => query.filter(issues::level.eq(f.value.clone())),
            ("level", Op::Neq) => query.filter(issues::level.ne(f.value.clone())),
            ("status", Op::Eq) => query.filter(issues::status.eq(f.value.clone())),
            ("status", Op::Neq) => query.filter(issues::status.ne(f.value.clone())),
            ("type", Op::Eq) => query.filter(issues::type_.eq(f.value.clone())),
            ("type", Op::Neq) => query.filter(issues::type_.ne(f.value.clone())),
            ("type", Op::Contains) => query.filter(issues::type_.ilike(like_contains(&f.value))),
            ("culprit", Op::Eq) => query.filter(issues::culprit.eq(f.value.clone())),
            ("culprit", Op::Neq) => query.filter(issues::culprit.ne(f.value.clone())),
            ("culprit", Op::Contains) => query.filter(issues::culprit.ilike(like_contains(&f.value))),
            ("times_seen", Op::Eq) => query.filter(issues::times_seen.eq(as_i64(&f.value))),
            ("times_seen", Op::Gt) => query.filter(issues::times_seen.gt(as_i64(&f.value))),
            ("times_seen", Op::Lt) => query.filter(issues::times_seen.lt(as_i64(&f.value))),
            ("users_seen", Op::Eq) => query.filter(issues::users_seen.eq(as_i64(&f.value))),
            ("users_seen", Op::Gt) => query.filter(issues::users_seen.gt(as_i64(&f.value))),
            ("users_seen", Op::Lt) => query.filter(issues::users_seen.lt(as_i64(&f.value))),
            _ => query, // unreachable: Task 1 whitelists field+op
        };
    }
    if let Some(term) = q {
        let p = like_contains(term);
        query = query.filter(
            issues::title.ilike(p.clone())
                .or(issues::type_.ilike(p.clone()))
                .or(issues::culprit.ilike(p)),
        );
    }
    query
        .select(Issue::as_select())
        .order(issues::last_seen.desc())
        .limit(limit)
        .offset(offset)
        .load(conn)
        .await
}

pub async fn get_issue(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    issue_id: Uuid,
) -> QueryResult<Option<Issue>> {
    issues::table
        .filter(issues::app_id.eq(app_id))
        .filter(issues::id.eq(issue_id))
        .select(Issue::as_select())
        .first(conn)
        .await
        .optional()
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

pub async fn list_error_events_for_issue(
    conn: &mut AsyncPgConnection,
    issue_id: Uuid,
    limit: i64,
) -> QueryResult<Vec<ErrorEvent>> {
    error_events::table
        .filter(error_events::issue_id.eq(issue_id))
        .select(ErrorEvent::as_select())
        .order(error_events::occurred_at.desc())
        .limit(limit)
        .load(conn)
        .await
}

pub async fn latest_error_event(
    conn: &mut AsyncPgConnection,
    issue_id: Uuid,
) -> QueryResult<Option<ErrorEvent>> {
    error_events::table
        .filter(error_events::issue_id.eq(issue_id))
        .select(ErrorEvent::as_select())
        .order(error_events::occurred_at.desc())
        .first(conn)
        .await
        .optional()
}

pub async fn error_events_for_person(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    distinct_id: &str,
    limit: i64,
) -> QueryResult<Vec<ErrorEvent>> {
    error_events::table
        .filter(error_events::app_id.eq(app_id))
        .filter(error_events::distinct_id.eq(distinct_id))
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

pub fn like_contains(v: &str) -> String { format!("%{}%", escape_like(v)) }
fn as_i64(v: &str) -> i64 { v.parse().unwrap_or_default() } // parser guarantees numeric

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

/// Resolve an environment name to its id for this app (None if unknown).
pub async fn environment_id_by_name(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    name: &str,
) -> Option<Uuid> {
    environments::table
        .filter(environments::app_id.eq(app_id))
        .filter(environments::name.eq(name))
        .select(environments::id)
        .first::<Uuid>(conn)
        .await
        .ok()
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

pub async fn get_event_user(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    distinct_id: &str,
) -> QueryResult<Option<EventUser>> {
    event_users::table
        .filter(event_users::app_id.eq(app_id))
        .filter(event_users::distinct_id.eq(distinct_id))
        .select(EventUser::as_select())
        .first(conn)
        .await
        .optional()
}

pub async fn events_for_person(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    distinct_id: &str,
    limit: i64,
) -> QueryResult<Vec<AnalyticsEvent>> {
    analytics_events::table
        .filter(analytics_events::app_id.eq(app_id))
        .filter(analytics_events::distinct_id.eq(distinct_id))
        .select(AnalyticsEvent::as_select())
        .order(analytics_events::occurred_at.desc())
        .limit(limit)
        .load(conn)
        .await
}

#[derive(Debug, QueryableByName, serde::Serialize)]
pub struct EventCount {
    #[diesel(sql_type = Text)]
    pub name: String,
    #[diesel(sql_type = BigInt)]
    pub count: i64,
}

pub async fn top_events(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    since: DateTime<Utc>,
    limit: i64,
) -> QueryResult<Vec<EventCount>> {
    diesel::sql_query(
        "SELECT name, count(*)::bigint AS count FROM analytics_events \
         WHERE app_id = $1 AND occurred_at >= $2 \
         GROUP BY name ORDER BY count DESC LIMIT $3",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Timestamptz, _>(since)
    .bind::<BigInt, _>(limit)
    .get_results(conn)
    .await
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
    app_id: Uuid,
    name: Option<&str>,
    since: DateTime<Utc>,
) -> QueryResult<Vec<SeriesPoint>> {
    match name {
        Some(n) => {
            diesel::sql_query(
                "SELECT date_trunc('day', occurred_at) AS bucket, count(*)::bigint AS count \
                 FROM analytics_events \
                 WHERE app_id = $1 AND occurred_at >= $2 AND name = $3 \
                 GROUP BY bucket ORDER BY bucket",
            )
            .bind::<SqlUuid, _>(app_id)
            .bind::<Timestamptz, _>(since)
            .bind::<Text, _>(n)
            .get_results(conn)
            .await
        }
        None => {
            diesel::sql_query(
                "SELECT date_trunc('day', occurred_at) AS bucket, count(*)::bigint AS count \
                 FROM analytics_events \
                 WHERE app_id = $1 AND occurred_at >= $2 \
                 GROUP BY bucket ORDER BY bucket",
            )
            .bind::<SqlUuid, _>(app_id)
            .bind::<Timestamptz, _>(since)
            .get_results(conn)
            .await
        }
    }
}

pub async fn issue_occurrence_series(
    conn: &mut AsyncPgConnection,
    issue_id: Uuid,
    since: DateTime<Utc>,
) -> QueryResult<Vec<SeriesPoint>> {
    diesel::sql_query(
        "SELECT date_trunc('day', occurred_at) AS bucket, count(*)::bigint AS count \
         FROM error_events \
         WHERE issue_id = $1 AND occurred_at >= $2 \
         GROUP BY bucket ORDER BY bucket",
    )
    .bind::<SqlUuid, _>(issue_id)
    .bind::<Timestamptz, _>(since)
    .get_results(conn)
    .await
}

// ===========================================================================
// Sessions & devices (roll-ups upserted by the pipeline)
// ===========================================================================

/// Upsert a session row, folding one signal into it: bump last/first seen and
/// the event/error counters. `context` snapshots the device/os block (only
/// written when non-empty). Idempotent per `(app_id, session_id)`.
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
pub async fn app_event_counts(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
) -> QueryResult<(i64, i64)> {
    let errors: i64 = error_events::table
        .filter(error_events::app_id.eq(app_id))
        .count()
        .get_result(conn)
        .await?;
    let events: i64 = analytics_events::table
        .filter(analytics_events::app_id.eq(app_id))
        .count()
        .get_result(conn)
        .await?;
    Ok((errors, events))
}

pub async fn error_series(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    since: DateTime<Utc>,
) -> QueryResult<Vec<SeriesPoint>> {
    diesel::sql_query(
        "SELECT date_trunc('day', occurred_at) AS bucket, count(*)::bigint AS count \
         FROM error_events WHERE app_id = $1 AND occurred_at >= $2 \
         GROUP BY bucket ORDER BY bucket",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Timestamptz, _>(since)
    .get_results(conn)
    .await
}

// ===========================================================================
// Sessions (list + per-session signal streams for the timeline)
// ===========================================================================

#[allow(clippy::too_many_arguments)]
pub async fn list_sessions(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    since: DateTime<Utc>,
    limit: i64,
    offset: i64,
    distinct_id: Option<&str>,
    device_key: Option<&str>,
) -> QueryResult<Vec<Session>> {
    let mut q = sessions::table
        .filter(sessions::app_id.eq(app_id))
        .filter(sessions::last_event_at.ge(since))
        .into_boxed();
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

pub async fn get_session(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    session_id: &str,
) -> QueryResult<Option<Session>> {
    sessions::table
        .filter(sessions::app_id.eq(app_id))
        .filter(sessions::session_id.eq(session_id.to_string()))
        .select(Session::as_select())
        .first(conn)
        .await
        .optional()
}

pub async fn events_for_session(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    session_id: &str,
    limit: i64,
) -> QueryResult<Vec<AnalyticsEvent>> {
    analytics_events::table
        .filter(analytics_events::app_id.eq(app_id))
        .filter(analytics_events::session_id.eq(session_id.to_string()))
        .select(AnalyticsEvent::as_select())
        .order(analytics_events::occurred_at.asc())
        .limit(limit)
        .load(conn)
        .await
}

pub async fn errors_for_session(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    session_id: &str,
    limit: i64,
) -> QueryResult<Vec<ErrorEvent>> {
    error_events::table
        .filter(error_events::app_id.eq(app_id))
        .filter(error_events::session_id.eq(session_id.to_string()))
        .select(ErrorEvent::as_select())
        .order(error_events::occurred_at.asc())
        .limit(limit)
        .load(conn)
        .await
}

pub async fn transactions_for_session(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    session_id: &str,
    limit: i64,
) -> QueryResult<Vec<Transaction>> {
    transactions::table
        .filter(transactions::app_id.eq(app_id))
        .filter(transactions::session_id.eq(session_id.to_string()))
        .select(Transaction::as_select())
        .order(transactions::occurred_at.asc())
        .limit(limit)
        .load(conn)
        .await
}

// ===========================================================================
// Devices (inventory + per-device errors)
// ===========================================================================

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

pub async fn list_devices(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    since: DateTime<Utc>,
    limit: i64,
    offset: i64,
    search: Option<&str>,
) -> QueryResult<Vec<DeviceRow>> {
    let pattern = search
        .map(|s| format!("%{}%", s))
        .unwrap_or_else(|| "%".to_string());
    diesel::sql_query(
        "SELECT d.id, d.device_key, d.family, d.model, d.os_name, d.os_version, d.arch, \
                d.browser, d.last_distinct_id, d.first_seen, d.last_seen, \
                d.events_count, d.errors_count, COALESCE(s.cnt, 0)::bigint AS sessions_count \
         FROM devices d \
         LEFT JOIN (SELECT device_key, count(*) AS cnt FROM sessions WHERE app_id = $1 \
                    GROUP BY device_key) s ON s.device_key = d.device_key \
         WHERE d.app_id = $1 AND d.last_seen >= $2 \
           AND (COALESCE(d.family,'') || ' ' || COALESCE(d.model,'') || ' ' || \
                COALESCE(d.os_name,'') || ' ' || COALESCE(d.device_key,'')) ILIKE $3 \
         ORDER BY d.last_seen DESC LIMIT $4 OFFSET $5",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Timestamptz, _>(since)
    .bind::<Text, _>(pattern)
    .bind::<BigInt, _>(limit)
    .bind::<BigInt, _>(offset)
    .get_results(conn)
    .await
}

pub async fn get_device(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    device_key: &str,
) -> QueryResult<Option<Device>> {
    devices::table
        .filter(devices::app_id.eq(app_id))
        .filter(devices::device_key.eq(device_key.to_string()))
        .select(Device::as_select())
        .first(conn)
        .await
        .optional()
}

pub async fn errors_for_device(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    device_key: &str,
    limit: i64,
) -> QueryResult<Vec<ErrorEvent>> {
    error_events::table
        .filter(error_events::app_id.eq(app_id))
        .filter(error_events::device_key.eq(device_key.to_string()))
        .select(ErrorEvent::as_select())
        .order(error_events::occurred_at.desc())
        .limit(limit)
        .load(conn)
        .await
}

// ===========================================================================
// Persons (Users Explorer — event_user + activity counts)
// ===========================================================================

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
    app_id: Uuid,
    search: Option<&str>,
    limit: i64,
    offset: i64,
) -> QueryResult<Vec<PersonRow>> {
    let pattern = search
        .map(|s| format!("%{}%", s))
        .unwrap_or_else(|| "%".to_string());
    diesel::sql_query(
        "SELECT eu.distinct_id, eu.properties, eu.first_seen, eu.last_seen, \
                COALESCE(ae.cnt,0)::bigint AS events_count, \
                COALESCE(ee.cnt,0)::bigint AS errors_count, \
                COALESCE(se.cnt,0)::bigint AS sessions_count \
         FROM event_users eu \
         LEFT JOIN (SELECT distinct_id, count(*) cnt FROM analytics_events \
                    WHERE app_id=$1 GROUP BY distinct_id) ae ON ae.distinct_id = eu.distinct_id \
         LEFT JOIN (SELECT distinct_id, count(*) cnt FROM error_events \
                    WHERE app_id=$1 AND distinct_id IS NOT NULL GROUP BY distinct_id) ee \
                    ON ee.distinct_id = eu.distinct_id \
         LEFT JOIN (SELECT distinct_id, count(*) cnt FROM sessions \
                    WHERE app_id=$1 AND distinct_id IS NOT NULL GROUP BY distinct_id) se \
                    ON se.distinct_id = eu.distinct_id \
         WHERE eu.app_id=$1 AND (eu.distinct_id ILIKE $2 OR eu.properties::text ILIKE $2) \
         ORDER BY eu.last_seen DESC LIMIT $3 OFFSET $4",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Text, _>(pattern)
    .bind::<BigInt, _>(limit)
    .bind::<BigInt, _>(offset)
    .get_results(conn)
    .await
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

pub async fn overview_totals(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    since: DateTime<Utc>,
) -> QueryResult<OverviewTotals> {
    diesel::sql_query(
        "SELECT \
           (SELECT count(*) FROM analytics_events WHERE app_id=$1 AND occurred_at>=$2)::bigint AS events, \
           (SELECT count(*) FROM error_events WHERE app_id=$1 AND occurred_at>=$2)::bigint AS errors, \
           (SELECT count(*) FROM sessions WHERE app_id=$1 AND last_event_at>=$2)::bigint AS sessions, \
           (SELECT count(*) FROM event_users WHERE app_id=$1 AND last_seen>=$2)::bigint AS users, \
           (SELECT count(*) FROM event_users WHERE app_id=$1 AND first_seen>=$2)::bigint AS new_users, \
           (SELECT count(*) FROM sessions WHERE app_id=$1 AND last_event_at>=$2 AND errors_count>0)::bigint AS crashed_sessions",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Timestamptz, _>(since)
    .get_result(conn)
    .await
}

pub async fn top_issues(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    since: DateTime<Utc>,
    limit: i64,
) -> QueryResult<Vec<Issue>> {
    issues::table
        .filter(issues::app_id.eq(app_id))
        .filter(issues::last_seen.ge(since))
        .select(Issue::as_select())
        .order(issues::times_seen.desc())
        .limit(limit)
        .load(conn)
        .await
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

pub async fn issue_stats(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
) -> QueryResult<IssueStatsRow> {
    diesel::sql_query(
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
    .bind::<SqlUuid, _>(app_id)
    .get_result(conn)
    .await
}

// ===========================================================================
// Event Explorer (raw analytics event stream with filters)
// ===========================================================================

#[allow(clippy::too_many_arguments)]
pub async fn list_analytics_events(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    filters: &[ParsedFilter],
    q: Option<&str>,
    since: Option<chrono::DateTime<chrono::Utc>>,
    limit: i64,
    offset: i64,
) -> QueryResult<Vec<AnalyticsEvent>> {
    // Environment filters need a name->id lookup before the query is built.
    let mut env_eq: Option<Option<Uuid>> = None;   // Some(id) filter present
    let mut env_neq: Option<Option<Uuid>> = None;
    for f in filters {
        if f.field == "environment" {
            let id = environment_id_by_name(conn, app_id, &f.value).await;
            match f.op { Op::Eq => env_eq = Some(id), Op::Neq => env_neq = Some(id), _ => {} }
        }
    }

    let mut query = analytics_events::table
        .filter(analytics_events::app_id.eq(app_id))
        // Synthetic screen-view events belong to the Screens section, not the stream.
        .filter(analytics_events::name.ne("$screen"))
        .into_boxed();
    if let Some(s) = since {
        query = query.filter(analytics_events::occurred_at.ge(s));
    }
    for f in filters {
        query = match (f.field, f.op) {
            ("name", Op::Eq) => query.filter(analytics_events::name.eq(f.value.clone())),
            ("name", Op::Neq) => query.filter(analytics_events::name.ne(f.value.clone())),
            ("name", Op::Contains) => query.filter(analytics_events::name.ilike(like_contains(&f.value))),
            ("distinct_id", Op::Eq) => query.filter(analytics_events::distinct_id.eq(f.value.clone())),
            ("distinct_id", Op::Neq) => query.filter(analytics_events::distinct_id.ne(f.value.clone())),
            ("distinct_id", Op::Contains) => query.filter(analytics_events::distinct_id.ilike(like_contains(&f.value))),
            ("session_id", Op::Eq) => query.filter(analytics_events::session_id.eq(f.value.clone())),
            ("session_id", Op::Neq) => query.filter(analytics_events::session_id.ne(f.value.clone())),
            ("session_id", Op::Contains) => query.filter(analytics_events::session_id.ilike(like_contains(&f.value))),
            ("release", Op::Eq) => query.filter(analytics_events::release.eq(f.value.clone())),
            ("release", Op::Neq) => query.filter(analytics_events::release.ne(f.value.clone())),
            ("release", Op::Contains) => query.filter(analytics_events::release.ilike(like_contains(&f.value))),
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
            analytics_events::name.ilike(p.clone())
                .or(analytics_events::distinct_id.ilike(p)),
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
    app_id: Uuid,
    steps: &[String],
    since: DateTime<Utc>,
) -> QueryResult<Vec<FunnelStepCount>> {
    // $1 = app_id, $2 = since, $3.. = each step name in order.
    let mut ctes: Vec<String> = Vec::new();
    let mut selects: Vec<String> = Vec::new();
    for i in 0..steps.len() {
        let name_param = i + 3;
        if i == 0 {
            ctes.push(format!(
                "s0 AS (SELECT distinct_id, min(occurred_at) AS t FROM analytics_events \
                 WHERE app_id=$1 AND occurred_at>=$2 AND name=${name_param} GROUP BY distinct_id)"
            ));
        } else {
            let prev = i - 1;
            ctes.push(format!(
                "s{i} AS (SELECT a.distinct_id, min(a.occurred_at) AS t FROM analytics_events a \
                 JOIN s{prev} ON s{prev}.distinct_id = a.distinct_id \
                 WHERE a.app_id=$1 AND a.name=${name_param} AND a.occurred_at >= s{prev}.t \
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
        .bind::<SqlUuid, _>(app_id)
        .bind::<Timestamptz, _>(since);
    for step in steps {
        query = query.bind::<Text, _>(step.clone());
    }
    query.get_results(conn).await
}

// ===========================================================================
// Journeys (step-indexed transition graph for a Sankey)
// ===========================================================================

#[derive(Debug, QueryableByName, serde::Serialize)]
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

#[derive(Debug, QueryableByName, serde::Serialize)]
pub struct JourneyNode {
    #[diesel(sql_type = BigInt)]
    pub step: i64,
    #[diesel(sql_type = Text)]
    pub event: String,
    #[diesel(sql_type = BigInt)]
    pub count: i64,
}

pub async fn journey_links(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    since: DateTime<Utc>,
    depth: i64,
) -> QueryResult<Vec<JourneyLink>> {
    diesel::sql_query(
        "WITH ordered AS ( \
           SELECT distinct_id, name, \
             (row_number() OVER (PARTITION BY distinct_id ORDER BY occurred_at) - 1) AS step \
           FROM analytics_events WHERE app_id=$1 AND occurred_at>=$2), \
         capped AS (SELECT * FROM ordered WHERE step < $3) \
         SELECT a.step AS from_step, a.name AS from_event, b.name AS to_event, \
                count(*)::bigint AS count \
         FROM capped a JOIN capped b ON b.distinct_id=a.distinct_id AND b.step=a.step+1 \
         GROUP BY a.step, a.name, b.name ORDER BY a.step, count DESC",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Timestamptz, _>(since)
    .bind::<BigInt, _>(depth)
    .get_results(conn)
    .await
}

pub async fn journey_nodes(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    since: DateTime<Utc>,
    depth: i64,
) -> QueryResult<Vec<JourneyNode>> {
    diesel::sql_query(
        "WITH ordered AS ( \
           SELECT distinct_id, name, \
             (row_number() OVER (PARTITION BY distinct_id ORDER BY occurred_at) - 1) AS step \
           FROM analytics_events WHERE app_id=$1 AND occurred_at>=$2), \
         capped AS (SELECT * FROM ordered WHERE step < $3) \
         SELECT step, name AS event, count(*)::bigint AS count \
         FROM capped GROUP BY step, name ORDER BY step, count DESC",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Timestamptz, _>(since)
    .bind::<BigInt, _>(depth)
    .get_results(conn)
    .await
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
    app_id: Uuid,
    since: DateTime<Utc>,
    op: Option<&str>,
    device_key: Option<&str>,
) -> QueryResult<Vec<PerfSummaryRow>> {
    diesel::sql_query(
        "SELECT name, op, count(*)::bigint AS count, \
           percentile_cont(0.5)  WITHIN GROUP (ORDER BY duration_ms) AS p50, \
           percentile_cont(0.75) WITHIN GROUP (ORDER BY duration_ms) AS p75, \
           percentile_cont(0.95) WITHIN GROUP (ORDER BY duration_ms) AS p95, \
           percentile_cont(0.99) WITHIN GROUP (ORDER BY duration_ms) AS p99, \
           avg(duration_ms) AS avg, \
           (count(*) FILTER (WHERE status='error' OR http_status>=500))::float8 \
             / NULLIF(count(*),0) AS error_rate \
         FROM transactions \
         WHERE app_id=$1 AND occurred_at>=$2 AND ($3='' OR op=$3) AND ($4='' OR device_key=$4) \
         GROUP BY name, op ORDER BY count DESC LIMIT 100",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Timestamptz, _>(since)
    .bind::<Text, _>(op.unwrap_or(""))
    .bind::<Text, _>(device_key.unwrap_or(""))
    .get_results(conn)
    .await
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
    app_id: Uuid,
    since: DateTime<Utc>,
    name: Option<&str>,
    op: Option<&str>,
) -> QueryResult<Vec<PerfSeriesPoint>> {
    diesel::sql_query(
        "SELECT date_trunc('hour', occurred_at) AS bucket, \
           percentile_cont(0.5)  WITHIN GROUP (ORDER BY duration_ms) AS p50, \
           percentile_cont(0.95) WITHIN GROUP (ORDER BY duration_ms) AS p95, \
           count(*)::bigint AS throughput \
         FROM transactions \
         WHERE app_id=$1 AND occurred_at>=$2 AND ($3='' OR name=$3) AND ($4='' OR op=$4) \
         GROUP BY bucket ORDER BY bucket",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Timestamptz, _>(since)
    .bind::<Text, _>(name.unwrap_or(""))
    .bind::<Text, _>(op.unwrap_or(""))
    .get_results(conn)
    .await
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
pub async fn user_stats(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    since: DateTime<Utc>,
) -> QueryResult<UserStats> {
    diesel::sql_query(
        "SELECT \
           (SELECT count(*) FROM event_users WHERE app_id=$1)::bigint AS total_users, \
           (SELECT count(*) FROM event_users WHERE app_id=$1 AND last_seen>=$2)::bigint AS active_in_range, \
           (SELECT count(*) FROM event_users WHERE app_id=$1 AND first_seen>=$2)::bigint AS new_in_range, \
           (SELECT count(DISTINCT distinct_id) FROM ( \
              SELECT distinct_id FROM analytics_events WHERE app_id=$1 AND occurred_at >= now() - interval '1 day' AND distinct_id IS NOT NULL AND distinct_id <> '' \
              UNION ALL \
              SELECT distinct_id FROM error_events WHERE app_id=$1 AND occurred_at >= now() - interval '1 day' AND distinct_id IS NOT NULL AND distinct_id <> '' \
            ) d1)::bigint AS dau, \
           (SELECT count(DISTINCT distinct_id) FROM ( \
              SELECT distinct_id FROM analytics_events WHERE app_id=$1 AND occurred_at >= now() - interval '7 days' AND distinct_id IS NOT NULL AND distinct_id <> '' \
              UNION ALL \
              SELECT distinct_id FROM error_events WHERE app_id=$1 AND occurred_at >= now() - interval '7 days' AND distinct_id IS NOT NULL AND distinct_id <> '' \
            ) d7)::bigint AS wau, \
           (SELECT count(DISTINCT distinct_id) FROM ( \
              SELECT distinct_id FROM analytics_events WHERE app_id=$1 AND occurred_at >= now() - interval '30 days' AND distinct_id IS NOT NULL AND distinct_id <> '' \
              UNION ALL \
              SELECT distinct_id FROM error_events WHERE app_id=$1 AND occurred_at >= now() - interval '30 days' AND distinct_id IS NOT NULL AND distinct_id <> '' \
            ) d30)::bigint AS mau, \
           COALESCE((SELECT avg(EXTRACT(EPOCH FROM (last_event_at - started_at)) * 1000) \
                     FROM sessions WHERE app_id=$1 AND last_event_at>=$2), 0)::double precision AS avg_session_ms, \
           COALESCE((SELECT percentile_cont(0.5) WITHIN GROUP (ORDER BY EXTRACT(EPOCH FROM (last_event_at - started_at)) * 1000) \
                     FROM sessions WHERE app_id=$1 AND last_event_at>=$2), 0)::double precision AS median_session_ms",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Timestamptz, _>(since)
    .get_result(conn)
    .await
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
        .map(|(bucket, (active, new_users))| UserSeriesPoint { bucket, active, new_users })
        .collect()
}

/// Per-day distinct active users (analytics ∪ errors) and per-day new users,
/// merged. Both scoped to `since`.
pub async fn active_user_series(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    since: DateTime<Utc>,
) -> QueryResult<Vec<UserSeriesPoint>> {
    let active: Vec<SeriesPoint> = diesel::sql_query(
        "SELECT date_trunc('day', occurred_at) AS bucket, count(DISTINCT distinct_id)::bigint AS count \
         FROM ( \
            SELECT occurred_at, distinct_id FROM analytics_events \
              WHERE app_id=$1 AND occurred_at>=$2 AND distinct_id IS NOT NULL AND distinct_id <> '' \
            UNION ALL \
            SELECT occurred_at, distinct_id FROM error_events \
              WHERE app_id=$1 AND occurred_at>=$2 AND distinct_id IS NOT NULL AND distinct_id <> '' \
         ) u \
         GROUP BY bucket ORDER BY bucket",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Timestamptz, _>(since)
    .get_results(conn)
    .await?;

    let new: Vec<SeriesPoint> = diesel::sql_query(
        "SELECT date_trunc('day', first_seen) AS bucket, count(*)::bigint AS count \
         FROM event_users WHERE app_id=$1 AND first_seen>=$2 \
         GROUP BY bucket ORDER BY bucket",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Timestamptz, _>(since)
    .get_results(conn)
    .await?;

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

/// Reorder DB histogram rows into the fixed bucket order, 0-filling gaps. Pure.
pub fn order_histogram(rows: Vec<HistoBucket>) -> Vec<HistoBucket> {
    DURATION_BUCKETS
        .iter()
        .map(|label| {
            let count = rows.iter().find(|r| r.bucket == *label).map(|r| r.count).unwrap_or(0);
            HistoBucket { bucket: (*label).to_string(), count }
        })
        .collect()
}

pub async fn session_stats(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    since: DateTime<Utc>,
) -> QueryResult<SessionStats> {
    diesel::sql_query(
        "SELECT \
           (SELECT count(*) FROM sessions WHERE app_id=$1 AND last_event_at>=$2)::bigint AS sessions, \
           (SELECT count(*) FROM sessions WHERE app_id=$1 AND last_event_at>=$2 AND errors_count>0)::bigint AS crashed, \
           COALESCE((SELECT avg(EXTRACT(EPOCH FROM (last_event_at - started_at)) * 1000) \
                     FROM sessions WHERE app_id=$1 AND last_event_at>=$2), 0)::double precision AS avg_session_ms, \
           COALESCE((SELECT percentile_cont(0.5) WITHIN GROUP (ORDER BY EXTRACT(EPOCH FROM (last_event_at - started_at)) * 1000) \
                     FROM sessions WHERE app_id=$1 AND last_event_at>=$2), 0)::double precision AS median_session_ms",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Timestamptz, _>(since)
    .get_result(conn)
    .await
}

pub async fn session_duration_series(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    since: DateTime<Utc>,
) -> QueryResult<Vec<SeriesAvgPoint>> {
    diesel::sql_query(
        "SELECT date_trunc('day', started_at) AS bucket, \
                COALESCE(avg(EXTRACT(EPOCH FROM (last_event_at - started_at)) * 1000), 0)::double precision AS avg_ms \
         FROM sessions WHERE app_id=$1 AND started_at>=$2 \
         GROUP BY bucket ORDER BY bucket",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Timestamptz, _>(since)
    .get_results(conn)
    .await
}

pub async fn session_duration_histogram(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    since: DateTime<Utc>,
) -> QueryResult<Vec<HistoBucket>> {
    let rows: Vec<HistoBucket> = diesel::sql_query(
        "SELECT bucket, count(*)::bigint AS count FROM ( \
           SELECT CASE \
             WHEN d < 10000  THEN '<10s' \
             WHEN d < 60000  THEN '10-60s' \
             WHEN d < 300000 THEN '1-5m' \
             WHEN d < 1800000 THEN '5-30m' \
             ELSE '30m+' END AS bucket \
           FROM (SELECT EXTRACT(EPOCH FROM (last_event_at - started_at)) * 1000 AS d \
                 FROM sessions WHERE app_id=$1 AND last_event_at>=$2) s \
         ) b GROUP BY bucket",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Timestamptz, _>(since)
    .get_results(conn)
    .await?;
    Ok(order_histogram(rows))
}

#[cfg(test)]
mod user_series_tests {
    use super::{merge_user_series, SeriesPoint};
    use chrono::{TimeZone, Utc};

    fn pt(day: u32, count: i64) -> SeriesPoint {
        SeriesPoint { bucket: Utc.with_ymd_and_hms(2026, 7, day, 0, 0, 0).unwrap(), count }
    }

    #[test]
    fn merges_active_and_new_by_day_zero_filling() {
        let active = vec![pt(1, 10), pt(2, 8)];
        let new = vec![pt(2, 3), pt(3, 5)]; // day 1 has no new; day 3 has no active
        let out = merge_user_series(active, new);
        let got: Vec<(u32, i64, i64)> = out
            .iter()
            .map(|p| (p.bucket.format("%d").to_string().parse().unwrap(), p.active, p.new_users))
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
        HistoBucket { bucket: bucket.to_string(), count }
    }

    #[test]
    fn fills_missing_buckets_in_fixed_order() {
        let rows = vec![b("30m+", 2), b("<10s", 5)];
        let out = order_histogram(rows);
        let got: Vec<(&str, i64)> = out.iter().map(|h| (h.bucket.as_str(), h.count)).collect();
        assert_eq!(
            got,
            vec![("<10s", 5), ("10-60s", 0), ("1-5m", 0), ("5-30m", 0), ("30m+", 2)]
        );
        assert_eq!(out.len(), DURATION_BUCKETS.len());
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
    diesel::sql_query(format!(
        "{SAVED_FUNNEL_SELECT} WHERE sf.app_id=$1 ORDER BY sf.updated_at DESC"
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
const SCREEN_CTES: &str = "\
  WITH ev AS ( \
    SELECT screen, \
      count(*) FILTER (WHERE name='$screen')::bigint AS views, \
      count(*) FILTER (WHERE name<>'$screen')::bigint AS events \
    FROM analytics_events WHERE app_id=$1 AND occurred_at>=$2 AND screen IS NOT NULL GROUP BY screen), \
  ex AS ( \
    SELECT screen, count(*)::bigint AS exceptions \
    FROM error_events WHERE app_id=$1 AND occurred_at>=$2 AND screen IS NOT NULL GROUP BY screen), \
  us AS ( \
    SELECT screen, count(DISTINCT distinct_id)::bigint AS users FROM ( \
      SELECT screen, distinct_id FROM analytics_events WHERE app_id=$1 AND occurred_at>=$2 AND screen IS NOT NULL AND distinct_id IS NOT NULL AND distinct_id<>'' \
      UNION ALL \
      SELECT screen, distinct_id FROM error_events WHERE app_id=$1 AND occurred_at>=$2 AND screen IS NOT NULL AND distinct_id IS NOT NULL AND distinct_id<>'' \
    ) u GROUP BY screen), \
  dw AS ( \
    SELECT screen, sum(LEAST(raw_ms, 1800000))::double precision AS total_dwell_ms FROM ( \
      SELECT screen, EXTRACT(EPOCH FROM ( \
        LEAD(occurred_at) OVER (PARTITION BY session_id ORDER BY occurred_at) - occurred_at)) * 1000 AS raw_ms \
      FROM analytics_events WHERE app_id=$1 AND occurred_at>=$2 AND session_id IS NOT NULL AND screen IS NOT NULL) g \
    WHERE raw_ms IS NOT NULL AND raw_ms > 0 GROUP BY screen), \
  keys AS (SELECT screen FROM ev UNION SELECT screen FROM ex) ";

pub async fn screen_list(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    since: DateTime<Utc>,
    q_pattern: &str, // '%' for no filter, else like_contains(term)
    limit: i64,
    offset: i64,
) -> QueryResult<Vec<ScreenRow>> {
    diesel::sql_query(format!(
        "{SCREEN_CTES} \
         SELECT k.screen, \
           COALESCE(ev.views,0)::bigint AS views, \
           COALESCE(ev.events,0)::bigint AS events, \
           COALESCE(ex.exceptions,0)::bigint AS exceptions, \
           COALESCE(us.users,0)::bigint AS users, \
           COALESCE(COALESCE(dw.total_dwell_ms,0) / NULLIF(COALESCE(ev.views,0),0), 0)::double precision AS avg_dwell_ms \
         FROM keys k \
         LEFT JOIN ev ON ev.screen=k.screen LEFT JOIN ex ON ex.screen=k.screen \
         LEFT JOIN us ON us.screen=k.screen LEFT JOIN dw ON dw.screen=k.screen \
         WHERE k.screen ILIKE $3 \
         ORDER BY views DESC, k.screen ASC LIMIT $4 OFFSET $5"
    ))
    .bind::<SqlUuid, _>(app_id)
    .bind::<Timestamptz, _>(since)
    .bind::<Text, _>(q_pattern)
    .bind::<BigInt, _>(limit)
    .bind::<BigInt, _>(offset)
    .get_results(conn)
    .await
}

pub async fn screen_stats(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    since: DateTime<Utc>,
    name: &str,
) -> QueryResult<ScreenStats> {
    diesel::sql_query(format!(
        "{SCREEN_CTES} \
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
         WHERE k.screen = $3"
    ))
    .bind::<SqlUuid, _>(app_id)
    .bind::<Timestamptz, _>(since)
    .bind::<Text, _>(name)
    .get_result(conn)
    .await
}

pub async fn recent_events_for_screen(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    screen: &str,
    since: DateTime<Utc>,
    limit: i64,
) -> QueryResult<Vec<AnalyticsEvent>> {
    analytics_events::table
        .filter(analytics_events::app_id.eq(app_id))
        .filter(analytics_events::screen.eq(screen))
        .filter(analytics_events::occurred_at.ge(since))
        .filter(analytics_events::name.ne("$screen"))
        .select(AnalyticsEvent::as_select())
        .order(analytics_events::occurred_at.desc())
        .limit(limit)
        .load(conn)
        .await
}

pub async fn recent_exceptions_for_screen(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    screen: &str,
    since: DateTime<Utc>,
    limit: i64,
) -> QueryResult<Vec<ErrorEvent>> {
    error_events::table
        .filter(error_events::app_id.eq(app_id))
        .filter(error_events::screen.eq(screen))
        .filter(error_events::occurred_at.ge(since))
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
    monitors::table.find(id).select(Monitor::as_select()).first(conn).await.optional()
}

pub async fn monitor_project(conn: &mut AsyncPgConnection, id: Uuid) -> QueryResult<Option<Uuid>> {
    monitors::table.find(id).select(monitors::project_id).first(conn).await.optional()
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
struct PctRow { #[diesel(sql_type = Nullable<Double>)] pct: Option<f64> }

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

pub async fn prune_checks(conn: &mut AsyncPgConnection, older_than_days: i64) -> QueryResult<usize> {
    diesel::sql_query(
        "DELETE FROM monitor_checks WHERE checked_at < now() - ($1 || ' days')::interval",
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
struct IdRow { #[diesel(sql_type = SqlUuid)] id: Uuid }

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
            next_check_at = CASE WHEN $4 = 'unknown' THEN now() ELSE next_check_at END, \
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
            tiering_state::watermark.eq(diesel::dsl::sql::<Timestamptz>("GREATEST(tiering_state.watermark, EXCLUDED.watermark)")),
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
    let sql = format!(
        "BEGIN; ALTER TABLE {table} DETACH PARTITION {child}; DROP TABLE {child}; COMMIT;"
    );
    diesel::sql_query(sql).execute(conn).await?;
    Ok(())
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
