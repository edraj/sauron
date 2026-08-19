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
    // Part of the cache key, NOT just a statistic — see below.
    let deployment_orgs = repo::org_count(&mut conn).await?;
    drop(conn);
    if org_ids.is_empty() {
        return Err(ApiError::Auth(AuthError::Forbidden));
    }

    // Key the cache on the exact visible scope so two callers with different
    // org sets can never be served each other's report.
    //
    // `deployment_orgs` is in the key because the report's `full_scope` flag —
    // and with it whether real `pg_database_size` bytes are disclosed — depends
    // on the caller's org set covering *every* org. That comparison can flip
    // without the caller's own grants changing at all: create a second org and
    // a previously-full-scope caller becomes partial-scope, yet their org set
    // (and so the rest of this key) is byte-identical. Measured before this was
    // added: a full-scope report kept serving deployment-wide physical bytes for
    // 35s after a second tenant appeared, i.e. the whole TTL is a disclosure
    // window. Including the count invalidates on exactly that transition.
    let mut sorted = org_ids.clone();
    sorted.sort();
    let key = format!(
        "sauron:storage:v2:{}:{}",
        deployment_orgs,
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
pub(crate) async fn require_deployment_admin(
    state: &AppState,
    auth: &AuthUser,
) -> Result<(), ApiError> {
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
    /// Within the warning window and not yet lapsed. The UI surfaces these so a
    /// restore never simply disappears — the whole point of warn-before-expiry
    /// is that the operator gets a chance to extend before the rows go.
    pub expiring_soon: bool,
    /// Whole hours until expiry, negative once lapsed. Server-computed for the
    /// same clock-skew reason as `expired`.
    pub expires_in_hours: i64,
}

/// Matches `PIN_WARN_DAYS` in `sauron-tier`, which does the log-side warning.
/// Both are 7 to pair with the dashboard's 30-day default pin.
pub const PIN_WARN_DAYS: i64 = 7;

impl TierPinView {
    fn from_pin(p: sauron_db::models::TierPin, now: chrono::DateTime<chrono::Utc>) -> Self {
        let expired = p.expires_at <= now;
        Self {
            expiring_soon: !expired && p.expires_at <= now + chrono::Duration::days(PIN_WARN_DAYS),
            expires_in_hours: (p.expires_at - now).num_hours(),
            expired,
            id: p.id,
            table_name: p.table_name,
            range_start: p.range_start,
            range_end: p.range_end,
            expires_at: p.expires_at,
            created_at: p.created_at,
            reason: p.reason,
        }
    }
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
            .map(|p| TierPinView::from_pin(p, now))
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

    // Deployment-wide: this one value decides when EVERY tenant's data leaves
    // Postgres, so it is recorded into every org's trail rather than one.
    crate::audit::record_all_orgs(
        &mut conn,
        auth.user_id,
        crate::audit::Entry::new(
            uuid::Uuid::nil(),
            crate::audit::action::TIER_POLICY_UPDATE,
            crate::audit::entity::TIER,
        )
        .target_named("cold-tier rotation age")
        .changes(crate::audit::created(
            crate::audit::entity::TIER,
            &[("hot_days", serde_json::json!(body.hot_days))],
        )),
    )
    .await;

    drop(conn);
    get_tier_policy(auth, State(state)).await
}

// ===========================================================================
// Cold-data restore
// ===========================================================================

/// Default life of a restore pin, and the ceiling an operator may ask for.
/// A restore is temporary by design: the point is to look at old data for a
/// while, not to opt a range out of tiering permanently.
pub const RESTORE_DEFAULT_DAYS: i64 = 30;
pub const RESTORE_MAX_DAYS: i64 = 365;
/// Widest single restore. Not a storage limit — a blast-radius limit. Restoring
/// a year of every app in one job is almost always a mistake, and the range is
/// the one input where a typo is silent and expensive.
pub const RESTORE_MAX_RANGE_DAYS: i64 = 400;

#[derive(serde::Serialize)]
pub struct RestoreJobView {
    pub id: uuid::Uuid,
    pub table_name: String,
    pub app_id: Option<uuid::Uuid>,
    pub range_start: chrono::DateTime<chrono::Utc>,
    pub range_end: chrono::DateTime<chrono::Utc>,
    pub status: String,
    pub pin_id: Option<uuid::Uuid>,
    pub pin_expires_at: chrono::DateTime<chrono::Utc>,
    pub rows_estimated: i64,
    pub rows_restored: i64,
    pub attempts: i32,
    pub error: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl From<sauron_db::models::RestoreJob> for RestoreJobView {
    fn from(j: sauron_db::models::RestoreJob) -> Self {
        Self {
            id: j.id,
            table_name: j.table_name,
            app_id: j.app_id,
            range_start: j.range_start,
            range_end: j.range_end,
            status: j.status,
            pin_id: j.pin_id,
            pin_expires_at: j.pin_expires_at,
            rows_estimated: j.rows_estimated,
            rows_restored: j.rows_restored,
            attempts: j.attempts,
            error: j.error,
            created_at: j.created_at,
            started_at: j.started_at,
            finished_at: j.finished_at,
        }
    }
}

#[derive(serde::Deserialize)]
pub struct CreateRestore {
    pub table_name: String,
    /// `None` restores every app in the range.
    pub app_id: Option<uuid::Uuid>,
    pub range_start: chrono::DateTime<chrono::Utc>,
    pub range_end: chrono::DateTime<chrono::Utc>,
    pub expires_in_days: Option<i64>,
}

/// Queue a restore of cold data back into Postgres.
///
/// Returns immediately with a queued job; `sauron-tier` picks it up within
/// `RESTORE_POLL_SECS` and the client polls `GET /v1/admin/restore/{id}`. The
/// copy itself can take minutes on a wide range, which is far past any sensible
/// request timeout.
pub async fn create_restore(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(body): Json<CreateRestore>,
) -> Result<Json<RestoreJobView>, ApiError> {
    require_deployment_admin(&state, &auth).await?;

    if !repo::is_restorable_table(&body.table_name) {
        return Err(ApiError::BadRequest(format!(
            "table must be one of {}",
            repo::RESTORABLE_TABLES.join(", ")
        )));
    }
    if body.range_end <= body.range_start {
        return Err(ApiError::BadRequest(
            "range_end must be after range_start".to_string(),
        ));
    }
    if (body.range_end - body.range_start) > chrono::Duration::days(RESTORE_MAX_RANGE_DAYS) {
        return Err(ApiError::BadRequest(format!(
            "range may not exceed {RESTORE_MAX_RANGE_DAYS} days"
        )));
    }
    let days = body.expires_in_days.unwrap_or(RESTORE_DEFAULT_DAYS);
    if !(1..=RESTORE_MAX_DAYS).contains(&days) {
        return Err(ApiError::BadRequest(format!(
            "expires_in_days must be between 1 and {RESTORE_MAX_DAYS}"
        )));
    }

    let mut conn = crate::routes::db(&state).await?;
    // Two overlapping restores would each insert the same Parquet rows under a
    // different pin, and because a pin only ever deletes its OWN rows, the
    // duplicates would outlive the first expiry. Refuse rather than deduplicate.
    if let Some(existing) = repo::overlapping_active_restore(
        &mut conn,
        &body.table_name,
        body.range_start,
        body.range_end,
    )
    .await?
    {
        return Err(ApiError::Conflict(format!(
            "restore {} already covers an overlapping range for {}",
            existing.id, existing.table_name
        )));
    }

    let job = repo::create_restore_job(
        &mut conn,
        &body.table_name,
        body.app_id,
        body.range_start,
        body.range_end,
        chrono::Utc::now() + chrono::Duration::days(days),
        Some(auth.user_id),
    )
    .await?;

    crate::audit::record_all_orgs(
        &mut conn,
        auth.user_id,
        crate::audit::Entry::new(
            uuid::Uuid::nil(),
            crate::audit::action::TIER_RESTORE_CREATE,
            crate::audit::entity::TIER,
        )
        .target(job.id, &body.table_name)
        .changes(crate::audit::created(
            crate::audit::entity::TIER,
            &[
                ("table_name", serde_json::json!(body.table_name)),
                ("expires_at", serde_json::json!(job.pin_expires_at)),
            ],
        )),
    )
    .await;
    Ok(Json(job.into()))
}

pub async fn list_restores(
    auth: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<Vec<RestoreJobView>>, ApiError> {
    require_deployment_admin(&state, &auth).await?;
    let mut conn = crate::routes::db(&state).await?;
    let jobs = repo::list_restore_jobs(&mut conn, 50).await?;
    Ok(Json(jobs.into_iter().map(Into::into).collect()))
}

pub async fn get_restore(
    auth: AuthUser,
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<uuid::Uuid>,
) -> Result<Json<RestoreJobView>, ApiError> {
    require_deployment_admin(&state, &auth).await?;
    let mut conn = crate::routes::db(&state).await?;
    match repo::get_restore_job(&mut conn, id).await? {
        Some(j) => Ok(Json(j.into())),
        None => Err(ApiError::NotFound),
    }
}

#[derive(serde::Serialize)]
pub struct ReleasedPin {
    pub id: uuid::Uuid,
    pub table_name: String,
    /// Rows removed from Postgres. They remain in Parquet — this frees hot
    /// storage, it does not delete data.
    pub rows_deleted: i64,
}

/// Release a pin now: delete the rows it restored and drop the pin.
///
/// Deliberately NOT a bare delete of the pin row. See `repo::release_tier_pin`.
pub async fn release_pin(
    auth: AuthUser,
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<uuid::Uuid>,
) -> Result<Json<ReleasedPin>, ApiError> {
    require_deployment_admin(&state, &auth).await?;
    let mut conn = crate::routes::db(&state).await?;
    match repo::release_tier_pin(&mut conn, id).await? {
        Some(e) => {
            // Releasing a pin DELETES the restored rows. That is the most
            // destructive operation this endpoint set offers, so the row count
            // is part of the record.
            crate::audit::record_all_orgs(
                &mut conn,
                auth.user_id,
                crate::audit::Entry::new(
                    uuid::Uuid::nil(),
                    crate::audit::action::TIER_PIN_RELEASE,
                    crate::audit::entity::TIER,
                )
                .target(e.id, &e.table_name)
                .changes(crate::audit::created(
                    crate::audit::entity::TIER,
                    &[
                        ("table_name", serde_json::json!(e.table_name)),
                        ("rows_deleted", serde_json::json!(e.rows_deleted)),
                    ],
                )),
            )
            .await;
            Ok(Json(ReleasedPin {
                id: e.id,
                table_name: e.table_name,
                rows_deleted: e.rows_deleted,
            }))
        }
        None => Err(ApiError::NotFound),
    }
}

#[derive(serde::Deserialize)]
pub struct ExtendPin {
    pub days: i64,
}

/// Push a pin's expiry out — the answer to an expiry warning when the
/// investigation is not finished. Measured from now, not from the current
/// expiry, so extending a nearly-lapsed pin and a fresh one give the same
/// predictable result.
pub async fn extend_pin(
    auth: AuthUser,
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<uuid::Uuid>,
    Json(body): Json<ExtendPin>,
) -> Result<Json<TierPinView>, ApiError> {
    require_deployment_admin(&state, &auth).await?;
    if !(1..=RESTORE_MAX_DAYS).contains(&body.days) {
        return Err(ApiError::BadRequest(format!(
            "days must be between 1 and {RESTORE_MAX_DAYS}"
        )));
    }
    let mut conn = crate::routes::db(&state).await?;
    let new_expiry = chrono::Utc::now() + chrono::Duration::days(body.days);
    match repo::extend_tier_pin(&mut conn, id, new_expiry).await? {
        Some(p) => {
            crate::audit::record_all_orgs(
                &mut conn,
                auth.user_id,
                crate::audit::Entry::new(
                    uuid::Uuid::nil(),
                    crate::audit::action::TIER_PIN_EXTEND,
                    crate::audit::entity::TIER,
                )
                .target(p.id, &p.table_name)
                .changes(crate::audit::created(
                    crate::audit::entity::TIER,
                    &[
                        ("table_name", serde_json::json!(p.table_name)),
                        ("expires_at", serde_json::json!(new_expiry)),
                    ],
                )),
            )
            .await;
            Ok(Json(TierPinView::from_pin(p, chrono::Utc::now())))
        }
        None => Err(ApiError::NotFound),
    }
}
