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

// ===========================================================================
// Cold-tier rotation policy
// ===========================================================================

/// Who may read or change the deployment's rotation policy.
///
/// The rotation age is a single deployment-wide value: `sauron-tier` runs one
/// cutoff across every tenant's data. So an org-scoped `org:manage` grant is NOT
/// sufficient authority to change it — an admin of one tenant would be able to
/// force every other tenant's data out of Postgres. There is no super-admin flag
/// in this schema (one was added and then removed), so "operator" is expressed
/// with the primitives that do exist: hold org-scoped `org:manage` in EVERY org
/// that exists.
///
/// In the common single-tenant self-hosted deployment that is simply "the admin".
/// In a multi-tenant one it correctly refuses a single-tenant admin. A deployment
/// with zero orgs is refused rather than trivially satisfied — `0 >= 0` would
/// otherwise let any authenticated user through on a fresh install.
async fn require_deployment_admin(state: &AppState, auth: &AuthUser) -> Result<(), ApiError> {
    let mut conn = crate::routes::db(state).await?;
    let held = repo::orgs_with_permission(&mut conn, auth.user_id, perm::ORG_MANAGE).await?;
    let total = repo::count_all_orgs(&mut conn).await?;
    drop(conn);
    if total == 0 || (held.len() as i64) < total {
        return Err(ApiError::Auth(AuthError::Forbidden));
    }
    Ok(())
}

#[derive(serde::Serialize)]
pub struct TierPinView {
    pub id: uuid::Uuid,
    pub table_name: String,
    pub range_start: chrono::DateTime<chrono::Utc>,
    pub range_end: chrono::DateTime<chrono::Utc>,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub reason: Option<String>,
    /// Precomputed so the client does not have to decide what "expired" means
    /// against its own clock, which can differ from the server's.
    pub expired: bool,
}

#[derive(serde::Serialize)]
pub struct TierPolicy {
    /// From `TIER_HOT_DAYS` (or its built-in default) in this API process.
    pub configured_hot_days: i64,
    /// What `sauron-tier` will use on its next cycle.
    pub effective_hot_days: i64,
    pub overridden: bool,
    pub min_hot_days: i64,
    pub updated_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Components that pick the override up without a restart.
    pub follows_immediately: Vec<String>,
    /// Components still reading their start-time configuration. Reported rather
    /// than hidden: until these are restarted they can disagree with the worker
    /// about where the hot/cold boundary is, and an operator changing the policy
    /// deserves to know that from the UI instead of from a support ticket.
    pub follows_on_restart: Vec<String>,
    pub pins: Vec<TierPinView>,
}

/// Current rotation policy plus the pins protecting restored ranges.
pub async fn get_tier_policy(
    auth: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<TierPolicy>, ApiError> {
    require_deployment_admin(&state, &auth).await?;
    let mut conn = crate::routes::db(&state).await?;
    let row = repo::get_runtime_setting_row(&mut conn, repo::TIER_HOT_DAYS_KEY).await?;
    let effective = repo::effective_tier_hot_days(&mut conn, state.cfg.tier_hot_days).await?;
    let pins = repo::list_tier_pins(&mut conn).await?;
    drop(conn);

    let now = chrono::Utc::now();
    Ok(Json(TierPolicy {
        configured_hot_days: state.cfg.tier_hot_days,
        effective_hot_days: effective,
        // `row.is_some()` and not `effective != configured`: an override set to the
        // same number as the configured value IS an override, and clearing it is a
        // distinct action the UI has to be able to offer.
        overridden: row.is_some(),
        min_hot_days: repo::TIER_HOT_DAYS_MIN,
        updated_at: row.map(|(_, at)| at),
        // Verified against the call sites, not assumed. `sauron-tier` resolves the
        // setting once per cycle (bins/sauron-tier/src/main.rs), so it is the only
        // component that tracks a change without a restart — and it is the one that
        // actually moves data, which is why the override is meaningful at all.
        follows_immediately: vec!["sauron-tier (rotation cutoff)".to_string()],
        // Everything below still reads its start-time configuration and can
        // therefore disagree with the worker about where the boundary is until
        // restarted. Listed explicitly because the alternative is a UI that
        // implies a change is fully in force when it is not.
        follows_on_restart: vec![
            "symbolication write-back guard (sauron-api)".to_string(),
            "search scan clamp (sauron-db query planner reads TIER_HOT_DAYS from the environment directly)".to_string(),
            "PII inspector mask/preview windows (sauron-inspector)".to_string(),
        ],
        pins: pins
            .into_iter()
            .map(|p| TierPinView {
                id: p.id,
                table_name: p.table_name,
                range_start: p.range_start,
                range_end: p.range_end,
                expires_at: p.expires_at,
                created_at: p.created_at,
                reason: p.reason,
                expired: p.expires_at <= now,
            })
            .collect(),
    }))
}

#[derive(serde::Deserialize)]
pub struct SetTierPolicy {
    /// `None` clears the override, reverting to the process's configured value.
    /// Distinguished from "absent" by `Option<Option<i64>>` at the call site being
    /// unnecessary here: the field is required in the body, and `null` means clear.
    pub hot_days: Option<i64>,
}

/// Set or clear the rotation-age override.
///
/// Lowering the value is effectively irreversible from this endpoint alone: the
/// next tier cycle exports and then drops the newly-eligible partitions, and
/// raising the number back does NOT return them to Postgres — that needs a
/// restore. The UI says so; this is the same warning in the place that enforces it.
pub async fn set_tier_policy(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(body): Json<SetTierPolicy>,
) -> Result<Json<TierPolicy>, ApiError> {
    require_deployment_admin(&state, &auth).await?;
    let mut conn = crate::routes::db(&state).await?;
    match body.hot_days {
        Some(v) => {
            if v < repo::TIER_HOT_DAYS_MIN {
                return Err(ApiError::BadRequest(format!(
                    "hot_days must be at least {}",
                    repo::TIER_HOT_DAYS_MIN
                )));
            }
            repo::set_runtime_setting(
                &mut conn,
                repo::TIER_HOT_DAYS_KEY,
                &v.to_string(),
                Some(auth.user_id),
            )
            .await?;
        }
        None => {
            repo::delete_runtime_setting(&mut conn, repo::TIER_HOT_DAYS_KEY).await?;
        }
    }
    drop(conn);
    get_tier_policy(auth, State(state)).await
}
