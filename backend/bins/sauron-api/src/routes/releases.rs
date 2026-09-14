//! `GET /v1/apps/{app_id}/releases` — the release switcher's list. Reads
//! `app_releases` (via `sauron_db::releases::list_for_app`), grouped per
//! release across the caller's environment reach.
//!
//! Not on the `release_guard` allowlist (`RELEASE_ACCEPTING_PATHS`): this
//! route has no `?release=` filter of its own — release is the dimension it
//! *lists*, not one it narrows by. It does honor `?environment_id=`, exactly
//! like any other `authorized_read_scope` handler — see `routes::scope`'s
//! module docs.

use std::collections::BTreeMap;

use axum::extract::{Path, RawQuery, State};
use axum::Json;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use sauron_auth::{perm, AuthUser};

use super::db;
use crate::error::ApiError;
use crate::openapi::ErrorResponse;
use crate::AppState;

/// One release, folded across every (app, environment) pair the caller may
/// see it in.
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub struct ReleaseView {
    pub release: String,
    /// Enrollment ids that have seen this release; `null` = unattributed.
    pub environment_ids: Vec<Option<Uuid>>,
    pub first_seen_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
}

#[utoipa::path(
    get, path = "/v1/apps/{app_id}/releases", tag = "Releases",
    summary = "List an app's observed releases",
    description = "\
The release switcher's source: every `release` value seen in this app's telemetry, \
one row per release, folded across the caller's environment reach. `environment_ids` \
names every enrollment (see `GET /v1/apps/{app_id}/environments`) that has reported \
this release; a `null` entry means events reported it with no environment attributed. \
Sorted by `last_seen_at` descending.

Honors `?environment_id=` exactly like the analytics routes — an enrollment id \
narrows to that environment, `none` selects unattributed rows, and a malformed \
value is refused rather than silently widened.",
    // `environment_id` is spelled out here rather than inherited from an
    // `IntoParams` struct because this handler reads it off `RawQuery` (see
    // `routes::scope`'s module docs on why `Query<T>` cannot be trusted with
    // it) — there is no struct for utoipa to derive it from, so an
    // undeclared parameter would simply be missing from the document while
    // the handler honours it.
    params(("app_id" = Uuid, Path, description = "The app."),
           ("environment_id" = Option<String>, Query,
            description = "Enrollment id or `none`; narrows the list to that environment")),
    security(("bearerAuth" = [])),
    responses((status = 200, description = "Observed releases, newest last_seen_at first.", body = Vec<ReleaseView>),
              (status = 400, description = "Malformed environment_id.", body = ErrorResponse),
              (status = 401, description = "Missing or invalid access token.", body = ErrorResponse),
              (status = 403, description = "No grant covers this scope.", body = ErrorResponse)),
)]
pub async fn list_app_releases(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<Vec<ReleaseView>>, ApiError> {
    let mut conn = db(&state).await?;
    let scope = super::scope::authorized_read_scope(
        &mut conn,
        auth.user_id,
        app_id,
        perm::EVENT_READ,
        raw_query.as_deref(),
    )
    .await?;
    let rows = sauron_db::releases::list_for_app(&mut conn, &scope).await?;

    let mut grouped: BTreeMap<String, ReleaseView> = BTreeMap::new();
    for r in rows {
        // `entry(r.release)` moves the row's `String` into the map key, and
        // `or_insert_with_key` then clones it ONCE — only when this release is
        // new. The earlier form cloned twice on every row: once for the key
        // and once for the view, on an app with one environment per release
        // that is two allocations per row to build a map with one entry per
        // row. The three `Copy` fields are lifted out first because the key
        // move consumes `r`.
        let (env, first, last) = (r.environment_id, r.first_seen_at, r.last_seen_at);
        let entry = grouped
            .entry(r.release)
            .or_insert_with_key(|release| ReleaseView {
                release: release.clone(),
                environment_ids: Vec::new(),
                first_seen_at: first,
                last_seen_at: last,
            });
        entry.environment_ids.push(env);
        entry.first_seen_at = entry.first_seen_at.min(first);
        entry.last_seen_at = entry.last_seen_at.max(last);
    }
    let mut out: Vec<ReleaseView> = grouped.into_values().collect();
    out.sort_by(|a, b| {
        b.last_seen_at
            .cmp(&a.last_seen_at)
            .then_with(|| a.release.cmp(&b.release))
    });
    Ok(Json(out))
}
