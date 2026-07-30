//! Storage & records report endpoint.

use axum::extract::{Query, State};
use axum::Json;

use sauron_auth::{perm, AuthError, AuthUser};
use sauron_db::repo;

use crate::admin_storage::{collect_storage_cached, StorageReport};
use crate::error::ApiError;
use crate::AppState;

/// Storage & record report for the orgs the caller administers.
///
/// Requires an **org-scoped** `org:manage` grant: the report enumerates apps
/// with their data volumes and cold-file paths, which is administrative
/// information about a tenant rather than ordinary product data. Callers see
/// only the orgs they hold that permission in — there is no deployment-wide
/// view, so one tenant can never observe another's existence or scale.
pub async fn storage(
    auth: AuthUser,
    State(state): State<AppState>,
    Query(env): Query<super::scope::RejectEnvQuery>,
) -> Result<Json<StorageReport>, ApiError> {
    // The report is a per-org rollup across every app; there is no single
    // environment to scope it to, so the parameter is rejected rather than
    // silently accepted-and-ignored.
    super::scope::reject_environment_id(env.environment_id.as_deref())?;
    let mut conn = crate::routes::db(&state).await?;
    let org_ids = repo::orgs_with_permission(&mut conn, auth.user_id, perm::ORG_MANAGE).await?;
    drop(conn);
    if org_ids.is_empty() {
        return Err(ApiError::Auth(AuthError::Forbidden));
    }

    // Key the cache on the exact visible scope so two callers with different
    // org sets can never be served each other's report.
    let mut sorted = org_ids.clone();
    sorted.sort();
    let key = format!(
        "sauron:storage:{}",
        sauron_auth::hash_token(
            &sorted
                .iter()
                .map(|id| id.to_string())
                .collect::<Vec<_>>()
                .join(",")
        )
    );

    let report = collect_storage_cached(&state, &org_ids, &key).await?;
    Ok(Json(report))
}
