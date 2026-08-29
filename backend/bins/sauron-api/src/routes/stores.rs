//! App-store credential CRUD and the Overview chart feed.
//!
//! Secrets are WRITE-ONLY. No response type in this module carries a credential
//! or its ciphertext, and `sauron_db::models::AppStoreConnection` deliberately
//! derives no `Serialize` — so returning a stored row from a handler is a
//! compile error rather than a leak.
//!
//! Nothing here is environment-scoped. Google and Apple key their data to a
//! package name or bundle id and report no environment dimension; the
//! `store_environment_id` designation on the app is purely what decides where
//! the Overview section is *shown*.

use axum::extract::{Path, Query, RawQuery, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use sauron_auth::{authorize_app, perm, AuthUser};
use sauron_db::models::AppStoreConnection;
use sauron_db::repo;
use sauron_store::{apple::AppleIdentifiers, google::GoogleIdentifiers, StoreKind};

use super::db;
use crate::error::ApiError;
use crate::openapi::{ErrorResponse, OkResponse};
use crate::AppState;

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

#[derive(Serialize, utoipa::ToSchema)]
pub struct StoreConnectionOut {
    pub store: String,
    pub enabled: bool,
    pub identifiers: serde_json::Value,
    /// Whether a credential is stored. The credential itself is never returned.
    pub has_secret: bool,
    pub secret_updated_at: Option<DateTime<Utc>>,
    /// `never_synced` | `pending` | `ok` | `error`
    pub state: &'static str,
    pub last_synced_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
}

/// Derive the display state from the stored bookkeeping.
///
/// `pending` is Apple-only and means "the ongoing report request exists, the
/// sync ran cleanly, and Apple has not published an instance yet" — its normal
/// 24-48h startup window. It is deliberately not an error: a red badge that is
/// wrong every time teaches admins to ignore the badge.
fn connection_state(c: &AppStoreConnection) -> &'static str {
    if c.last_error.is_some() {
        return "error";
    }
    match c.last_synced_at {
        None => "never_synced",
        Some(_) => {
            let apple = c.store == StoreKind::AppStore.as_str();
            let requested = c.sync_state.get("report_request_id").is_some();
            // A clean Apple sync that produced no rows at all is still waiting
            // on Apple. `installs_seen` is set once the first rows land.
            let produced = c
                .sync_state
                .get("installs_seen")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if apple && requested && !produced {
                "pending"
            } else {
                "ok"
            }
        }
    }
}

fn to_out(c: AppStoreConnection) -> StoreConnectionOut {
    let state = connection_state(&c);
    StoreConnectionOut {
        store: c.store,
        enabled: c.enabled,
        identifiers: c.identifiers,
        has_secret: c.secret_enc.is_some(),
        secret_updated_at: Some(c.updated_at),
        state,
        last_synced_at: c.last_synced_at,
        last_error: c.last_error,
    }
}

// ---------------------------------------------------------------------------
// Connections
// ---------------------------------------------------------------------------

fn parse_store(store: &str) -> Result<StoreKind, ApiError> {
    StoreKind::parse(store).ok_or_else(|| {
        ApiError::BadRequest("unknown store; expected google_play or app_store".into())
    })
}

/// Refuse `environment_id` on every read in this module.
///
/// Store credentials and store metrics have no environment dimension at all —
/// Google and Apple report per package/bundle and have never heard of an
/// environment. Silently ignoring the parameter is the failure mode
/// `http_env_scoping.rs`'s router enumeration exists to catch: the caller
/// believes they narrowed the read and they did not.
///
/// Called BEFORE authorization, matching `apps::get_app`. Ordering it after
/// would answer 403 to a principal who cannot read the app, hiding a malformed
/// request behind a permission error.
fn reject_env(raw_query: Option<&str>) -> Result<(), ApiError> {
    super::scope::reject_environment_id_with_message(
        super::scope::raw_environment_id(raw_query).as_deref(),
        "store metrics are not partitioned by environment — the stores report per package, \
         not per environment",
    )
}

#[utoipa::path(
    get, path = "/v1/apps/{app_id}/store-connections", tag = "Stores",
    summary = "List app-store connections",
    description = "Configured App Store / Play Console connections. Credentials are never returned.",
    params(("app_id" = Uuid, Path, description = "The app.")), security(("bearerAuth" = [])),
    responses((status = 200, description = "Connections.", body = Vec<StoreConnectionOut>), (status = 401, description = "Missing or invalid access token.", body = ErrorResponse), (status = 403, description = "No grant covers this scope.", body = ErrorResponse)),
)]
pub async fn list(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<Vec<StoreConnectionOut>>, ApiError> {
    reject_env(raw_query.as_deref())?;
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::APP_READ).await?;
    let rows = repo::list_store_connections(&mut conn, app_id).await?;
    Ok(Json(rows.into_iter().map(to_out).collect()))
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct UpsertReq {
    pub identifiers: serde_json::Value,
    /// Absent = leave the stored credential alone. `null` = clear it. Present
    /// = replace it.
    ///
    /// The double `Option` is the whole point: collapsing "field absent" into
    /// "field null" means saving an edited package name silently wipes the
    /// service-account key, and the only symptom is a sync that starts failing
    /// hours later.
    #[serde(default, deserialize_with = "double_option")]
    pub secret: Option<Option<String>>,
}

fn double_option<'de, D>(d: D) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Some(Option::deserialize(d)?))
}

/// Reject identifiers that do not match the store slot they were posted to, and
/// normalise what is stored.
///
/// Storing them unvalidated turns a typo into a daemon error six hours later,
/// with nothing in the UI to connect the two.
fn validate_identifiers(
    kind: StoreKind,
    v: &serde_json::Value,
) -> Result<serde_json::Value, ApiError> {
    match kind {
        StoreKind::GooglePlay => {
            let ids: GoogleIdentifiers = serde_json::from_value(v.clone()).map_err(|e| {
                ApiError::BadRequest(format!("invalid Google Play identifiers: {e}"))
            })?;
            if ids.package_name.trim().is_empty() || ids.gcs_bucket.trim().is_empty() {
                return Err(ApiError::BadRequest(
                    "package_name and gcs_bucket are required".into(),
                ));
            }
            // Operators paste `gs://bucket`; store the bare name so the object
            // URL cannot end up with a doubled scheme.
            let bucket = ids
                .gcs_bucket
                .trim()
                .trim_start_matches("gs://")
                .trim_end_matches('/')
                .to_string();
            Ok(serde_json::json!({
                "package_name": ids.package_name.trim(),
                "gcs_bucket": bucket,
            }))
        }
        StoreKind::AppStore => {
            let ids: AppleIdentifiers = serde_json::from_value(v.clone())
                .map_err(|e| ApiError::BadRequest(format!("invalid App Store identifiers: {e}")))?;
            for (name, val) in [
                ("bundle_id", &ids.bundle_id),
                ("apple_app_id", &ids.apple_app_id),
                ("issuer_id", &ids.issuer_id),
                ("key_id", &ids.key_id),
                ("vendor_number", &ids.vendor_number),
            ] {
                if val.trim().is_empty() {
                    return Err(ApiError::BadRequest(format!("{name} is required")));
                }
            }
            Ok(serde_json::json!({
                "bundle_id": ids.bundle_id.trim(),
                "apple_app_id": ids.apple_app_id.trim(),
                "issuer_id": ids.issuer_id.trim(),
                "key_id": ids.key_id.trim(),
                "vendor_number": ids.vendor_number.trim(),
            }))
        }
    }
}

#[utoipa::path(
    put, path = "/v1/apps/{app_id}/store-connections/{store}", tag = "Stores",
    summary = "Create or replace a store connection",
    description = "\
Idempotent by `(app, store)`. Credentials are stored encrypted and never read \
back — the response describes the connection, not its secret.",
    params(("app_id" = Uuid, Path, description = "The app."), ("store" = String, Path, description = "Store identifier, e.g. `apple` or `google`.")), security(("bearerAuth" = [])),
    request_body(content = UpsertReq),
    responses((status = 200, description = "The stored connection.", body = StoreConnectionOut),
              (status = 400, description = "Unknown store, or malformed credentials.", body = ErrorResponse), (status = 401, description = "Missing or invalid access token.", body = ErrorResponse), (status = 403, description = "No grant covers this scope.", body = ErrorResponse)),
)]
pub async fn upsert(
    auth: AuthUser,
    State(state): State<AppState>,
    Path((app_id, store)): Path<(Uuid, String)>,
    Json(req): Json<UpsertReq>,
) -> Result<Json<StoreConnectionOut>, ApiError> {
    let kind = parse_store(&store)?;
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::APP_UPDATE).await?;

    let identifiers = validate_identifiers(kind, &req.identifiers)?;
    let secret_enc = match req.secret {
        None => None,
        Some(None) => Some(None),
        Some(Some(plain)) => {
            if plain.trim().is_empty() {
                return Err(ApiError::BadRequest(
                    "secret must not be empty; omit the field to leave it unchanged".into(),
                ));
            }
            Some(Some(
                state
                    .alerts
                    .cipher
                    .encrypt_str(&plain)
                    // Deliberately not `{e}`: a cipher error message can echo
                    // key-shaped detail into a response body.
                    .map_err(|_| {
                        ApiError::Internal("could not encrypt the store credential".into())
                    })?,
            ))
        }
    };

    let row =
        repo::upsert_store_connection(&mut conn, app_id, kind.as_str(), &identifiers, secret_enc)
            .await?;

    // The store credential is encrypted at rest and never recorded: the store
    // allowlist carries no key that could hold it.
    let (_, org_id) = repo::app_ancestry(&mut conn, app_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let entry = crate::audit::with_app_scope(
        &mut conn,
        crate::audit::Entry::new(
            org_id,
            crate::audit::action::STORE_UPSERT,
            crate::audit::entity::STORE,
        )
        .target(app_id, kind.as_str())
        .changes(crate::audit::created(
            crate::audit::entity::STORE,
            &[("store", serde_json::json!(kind.as_str()))],
        )),
        app_id,
    )
    .await;
    crate::audit::record(&mut conn, auth.user_id, entry).await;
    Ok(Json(to_out(row)))
}

#[utoipa::path(
    delete, path = "/v1/apps/{app_id}/store-connections/{store}", tag = "Stores",
    summary = "Remove a store connection",
    description = "Deletes the connection and its credentials. Metrics already synced are retained.",
    params(("app_id" = Uuid, Path, description = "The app."), ("store" = String, Path, description = "Store identifier, e.g. `apple` or `google`.")), security(("bearerAuth" = [])),
    responses((status = 200, description = "Removed.", body = OkResponse), (status = 401, description = "Missing or invalid access token.", body = ErrorResponse), (status = 403, description = "No grant covers this scope.", body = ErrorResponse), (status = 404, description = "No such connection.", body = ErrorResponse)),
)]
pub async fn delete(
    auth: AuthUser,
    State(state): State<AppState>,
    Path((app_id, store)): Path<(Uuid, String)>,
) -> Result<StatusCode, ApiError> {
    let kind = parse_store(&store)?;
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::APP_UPDATE).await?;
    // Collected history in `store_daily_metrics` is deliberately kept: it is
    // not a credential, and re-adding the connection resumes against it.
    repo::delete_store_connection(&mut conn, app_id, kind.as_str()).await?;

    let (_, org_id) = repo::app_ancestry(&mut conn, app_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let entry = crate::audit::with_app_scope(
        &mut conn,
        crate::audit::Entry::new(
            org_id,
            crate::audit::action::STORE_DELETE,
            crate::audit::entity::STORE,
        )
        .target(app_id, kind.as_str()),
        app_id,
    )
    .await;
    crate::audit::record(&mut conn, auth.user_id, entry).await;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post, path = "/v1/apps/{app_id}/store-connections/{store}/sync", tag = "Stores",
    summary = "Queue a store metrics sync",
    description = "Asynchronous — queues work for `sauron-storesync` and returns immediately. Store APIs publish with a lag of a day or more, so a sync will not surface same-day numbers.",
    params(("app_id" = Uuid, Path, description = "The app."), ("store" = String, Path, description = "Store identifier, e.g. `apple` or `google`.")), security(("bearerAuth" = [])),
    responses((status = 200, description = "Sync queued.", body = OkResponse), (status = 401, description = "Missing or invalid access token.", body = ErrorResponse), (status = 403, description = "No grant covers this scope.", body = ErrorResponse), (status = 404, description = "No such connection.", body = ErrorResponse)),
)]
pub async fn queue_sync(
    auth: AuthUser,
    State(state): State<AppState>,
    Path((app_id, store)): Path<(Uuid, String)>,
) -> Result<StatusCode, ApiError> {
    let kind = parse_store(&store)?;
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::APP_UPDATE).await?;
    // Only moves `next_sync_at`. `sauron-storesync` does the work — Apple's
    // report walk takes minutes and must never run inside an HTTP request.
    // 202, not 200, because nothing has been fetched yet.
    repo::queue_store_sync(&mut conn, app_id, kind.as_str()).await?;

    let (_, org_id) = repo::app_ancestry(&mut conn, app_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let entry = crate::audit::with_app_scope(
        &mut conn,
        crate::audit::Entry::new(
            org_id,
            crate::audit::action::STORE_SYNC,
            crate::audit::entity::STORE,
        )
        .target(app_id, kind.as_str()),
        app_id,
    )
    .await;
    crate::audit::record(&mut conn, auth.user_id, entry).await;
    Ok(StatusCode::ACCEPTED)
}

// ---------------------------------------------------------------------------
// Chart feed
// ---------------------------------------------------------------------------

#[derive(Serialize, utoipa::ToSchema)]
pub struct StoreCounts {
    pub installs: i64,
    pub uninstalls: i64,
}

/// One day. A store key is ABSENT when that store published nothing for the
/// day — deliberately not `{installs: 0}`, because zero is a real value that
/// means something different.
#[derive(Serialize, utoipa::ToSchema)]
pub struct StoreDayOut {
    pub day: NaiveDate,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub google_play: Option<StoreCounts>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_store: Option<StoreCounts>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct PendingDay {
    pub day: NaiveDate,
    /// Rendered verbatim by the dashboard.
    pub reason: String,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct StoreMetricsOut {
    pub series: Vec<StoreDayOut>,
    pub pending_days: Vec<PendingDay>,
    pub stores: Vec<StoreConnectionOut>,
}

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct MetricsQuery {
    #[serde(default = "default_since_days")]
    pub since_days: i64,
}

fn default_since_days() -> i64 {
    30
}

/// Both stores lag 1-3 days. Days inside that lag are excluded from
/// `pending_days` because listing them would flag the normal case forever.
const REPORTING_LAG_DAYS: i64 = 2;

#[utoipa::path(
    get, path = "/v1/apps/{app_id}/store-metrics", tag = "Stores",
    summary = "Installs and store metrics",
    description = "Synced install/uninstall figures by day. Days the store has not yet published appear as pending rather than zero — a zero would misread as \"nobody installed it\".",
    params(("app_id" = Uuid, Path, description = "The app."), MetricsQuery), security(("bearerAuth" = [])),
    responses((status = 200, description = "Store metrics with pending days marked.", body = StoreMetricsOut), (status = 401, description = "Missing or invalid access token.", body = ErrorResponse), (status = 403, description = "No grant covers this scope.", body = ErrorResponse)),
)]
pub async fn metrics(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    RawQuery(raw_query): RawQuery,
    Query(q): Query<MetricsQuery>,
) -> Result<Json<StoreMetricsOut>, ApiError> {
    reject_env(raw_query.as_deref())?;
    let days = q.since_days.clamp(1, 365);
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::APP_READ).await?;

    let today = Utc::now().date_naive();
    let since = today - chrono::Duration::days(days);
    let rows = repo::store_metrics_range(&mut conn, app_id, since).await?;
    let connections = repo::list_store_connections(&mut conn, app_id).await?;

    let mut by_day: std::collections::BTreeMap<NaiveDate, StoreDayOut> = Default::default();
    for r in rows {
        let e = by_day.entry(r.day).or_insert(StoreDayOut {
            day: r.day,
            google_play: None,
            app_store: None,
        });
        let counts = StoreCounts {
            installs: r.installs,
            uninstalls: r.uninstalls,
        };
        if r.store == StoreKind::GooglePlay.as_str() {
            e.google_play = Some(counts);
        } else {
            e.app_store = Some(counts);
        }
    }

    // Days inside the window with no row are PENDING, never zero-filled. A zero
    // bar asserts "nobody installed the app that day"; the truth is "the store
    // has not published that day yet". Same reasoning as `partial_days` on the
    // active-users series.
    let mut pending_days = Vec::new();
    if !connections.is_empty() {
        let mut d = since;
        let cutoff = today - chrono::Duration::days(REPORTING_LAG_DAYS);
        while d <= cutoff {
            if !by_day.contains_key(&d) {
                pending_days.push(PendingDay {
                    day: d,
                    reason: "the store has not published this day yet".to_string(),
                });
            }
            d += chrono::Duration::days(1);
        }
    }

    Ok(Json(StoreMetricsOut {
        series: by_day.into_values().collect(),
        pending_days,
        stores: connections.into_iter().map(to_out).collect(),
    }))
}
