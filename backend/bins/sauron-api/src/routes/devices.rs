//! Devices API, scoped to an app: fleet inventory and a per-device deep-dive
//! (recent sessions, crash history, and its performance profile).

use axum::extract::{Path, Query, RawQuery, State};
use axum::Json;
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use sauron_auth::{perm, AuthUser};
use sauron_db::models::{ErrorEvent, Session};
use sauron_db::repo;
use sauron_db::repo::{DeviceGroupRow, DeviceRow, PerfSummaryRow};

use super::db;
use crate::error::ApiError;
use crate::AppState;

#[derive(Deserialize)]
pub struct ListQuery {
    #[serde(default = "default_days")]
    pub since_days: i64,
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
    pub search: Option<String>,
    // `environment_id` is deliberately NOT a field here — it is read from the
    // raw query string via `RawQuery` + `scope::authorized_read_scope`
    // instead of this `Query<T>` extractor. See `routes::scope`'s module docs
    // for the extractor trap this avoids.
    /// Sentinel for the drill-down. The check is "non-empty", not "present" —
    /// any non-empty value (including `"0"`) turns the filter on; the
    /// dashboard always sends `"1"`. When enabled, all four descriptor fields
    /// below apply, with an ABSENT field meaning SQL NULL. Absent or empty
    /// means the four are ignored entirely and `list` behaves exactly as it
    /// always has.
    ///
    /// The sentinel exists because absent and "filter to NULL" are the same
    /// wire shape otherwise — an omitted query parameter — and the all-NULL
    /// group is a real group that must be drillable.
    pub group: Option<String>,
    pub family: Option<String>,
    pub model: Option<String>,
    pub os_name: Option<String>,
    pub os_version: Option<String>,
}

fn default_days() -> i64 {
    30
}
fn default_limit() -> i64 {
    50
}

pub async fn list(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<ListQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<Vec<DeviceRow>>, ApiError> {
    let mut conn = db(&state).await?;
    let scope = super::scope::authorized_read_scope(
        &mut conn,
        auth.user_id,
        app_id,
        perm::EVENT_READ,
        raw_query.as_deref(),
    )
    .await?;
    let since = Utc::now() - Duration::days(q.since_days.clamp(1, 365));
    let limit = q.limit.clamp(1, 200);
    let search = q.search.as_deref().filter(|s| !s.is_empty());
    // Any non-empty `group` value turns the filter on; the dashboard sends "1".
    let group = q
        .group
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(|_| repo::DeviceGroupKey {
            family: q.family.as_deref(),
            model: q.model.as_deref(),
            os_name: q.os_name.as_deref(),
            os_version: q.os_version.as_deref(),
        });
    Ok(Json(
        repo::list_devices(
            &mut conn,
            scope,
            since,
            limit,
            super::clamp_offset(q.offset),
            search,
            group,
        )
        .await?,
    ))
}

/// The Devices inventory's default read: one row per
/// `(family, model, os_name, os_version)`. Same scope handling as [`list`] —
/// `environment_id` comes from the raw query string, never from `ListQuery`.
pub async fn groups(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<ListQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<Vec<DeviceGroupRow>>, ApiError> {
    let mut conn = db(&state).await?;
    let scope = super::scope::authorized_read_scope(
        &mut conn,
        auth.user_id,
        app_id,
        perm::EVENT_READ,
        raw_query.as_deref(),
    )
    .await?;
    let since = Utc::now() - Duration::days(q.since_days.clamp(1, 365));
    let limit = q.limit.clamp(1, 200);
    let search = q.search.as_deref().filter(|s| !s.is_empty());
    Ok(Json(
        repo::list_device_groups(
            &mut conn,
            scope,
            since,
            limit,
            super::clamp_offset(q.offset),
            search,
        )
        .await?,
    ))
}

#[derive(Deserialize)]
pub struct DetailQuery {
    /// The device key (passed as a query param — keys can contain `/` and spaces).
    pub key: String,
    // `environment_id` is deliberately NOT a field here — see `ListQuery`'s
    // comment above.
}

#[derive(Serialize)]
pub struct DeviceDetail {
    /// Environment-scoped, not the raw `devices` row — see `get_device`'s doc
    /// comment. `events_count`/`errors_count` read the durable `devices`
    /// columns under `All` and an environment-scoped LATERAL under `One`/
    /// `Unattributed`, matching `sessions`/`errors`/`perf` below rather than
    /// showing cross-environment, all-time totals above a scoped list.
    pub device: DeviceRow,
    pub sessions: Vec<Session>,
    pub errors: Vec<ErrorEvent>,
    pub perf: Vec<PerfSummaryRow>,
}

pub async fn detail(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(dq): Query<DetailQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<DeviceDetail>, ApiError> {
    let mut conn = db(&state).await?;
    // `_with_perms`: `errors` below is whole `ErrorEvent` rows, which carry two
    // further permission questions — `perm::ISSUE_READ` for the body at all and
    // `perm::SOURCE_READ` for the de-obfuscated lines inside it. See
    // `sessions::detail` for the same note.
    let (scope, perms) = super::scope::authorized_read_scope_with_perms(
        &mut conn,
        auth.user_id,
        app_id,
        perm::EVENT_READ,
        raw_query.as_deref(),
    )
    .await?;
    let device_key = dq.key;

    let device = repo::get_device(&mut conn, scope.clone(), &device_key)
        .await?
        .ok_or(ApiError::NotFound)?;

    let since = Utc::now() - Duration::days(90);
    let sessions = repo::list_sessions(
        &mut conn,
        scope.clone(),
        since,
        50,
        0,
        None,
        Some(&device_key),
    )
    .await?;
    let mut errors = repo::errors_for_device(&mut conn, scope.clone(), &device_key, 50).await?;
    crate::symbolicate::gate_source_context(&perms, &mut errors);
    crate::symbolicate::gate_event_body(&perms, &mut errors);
    let perf = repo::performance_summary(&mut conn, scope, since, None, Some(&device_key)).await?;

    Ok(Json(DeviceDetail {
        device,
        sessions,
        errors,
        perf,
    }))
}
