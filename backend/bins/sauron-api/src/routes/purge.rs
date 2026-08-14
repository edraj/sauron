//! Admin data purge: preview, confirm, cancel, read.
//!
//! The purge removes product signal data within one app, bounded by
//! environment and time range, and then repairs every rollup the deletion
//! touched. It is irreversible.
//!
//! ## Why preview is a job and not a response
//!
//! `preview` does not count anything. It validates the scope, freezes it into
//! a `purge_jobs` row with `status = 'previewing'`, and returns 202; the worker
//! counts and moves the row to `previewed`, which the client polls for. A
//! count over three partitioned tables on a badly-polluted app is exactly the
//! workload that would sit past the 30s `TimeoutLayer`, and the app that most
//! needs purging is the one where counting is slowest.
//!
//! ## Why confirm cannot widen anything
//!
//! `confirm` takes no scope fields at all — only the typed slug. Everything
//! the worker acts on was written by `preview` and displayed to the operator.
//! There is therefore no request shape in which the thing executed differs
//! from the thing counted.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};
use sauron_auth::AuthUser;
use sauron_db::models::NewPurgeJob;
use sauron_db::{purge as purge_repo, repo};
use sauron_purge::{PurgeKind, Window};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::audit;
use crate::error::ApiError;
use crate::AppState;

use super::admin::require_deployment_admin;

#[derive(Deserialize)]
pub struct PreviewReq {
    pub app_id: Uuid,
    /// Absent or `null` = every environment, including unattributed rows.
    /// An explicitly empty array is refused rather than silently treated as
    /// "all": the two mean opposite things and must not be spelled alike.
    #[serde(default)]
    pub environment_ids: Option<Vec<Uuid>>,
    pub kinds: Vec<String>,
    #[serde(default)]
    pub range_start: Option<DateTime<Utc>>,
    #[serde(default)]
    pub range_end: Option<DateTime<Utc>>,
    /// Must be `true` to purge without bounds. A blank date field can never
    /// mean "everything" — that has to be an affirmative choice.
    #[serde(default)]
    pub all_time: bool,
}

#[derive(Serialize)]
pub struct PurgeJobView {
    #[serde(flatten)]
    pub job: Value,
    /// Echoed so the client does not have to know the server's TTL to render a
    /// countdown, and cannot drift from it.
    pub preview_ttl_secs: i64,
}

fn view(job: &sauron_db::models::PurgeJob, ttl: i64) -> Result<PurgeJobView, ApiError> {
    Ok(PurgeJobView {
        job: serde_json::to_value(job).map_err(|e| ApiError::Internal(e.to_string()))?,
        preview_ttl_secs: ttl,
    })
}

/// Parse and validate the requested kinds.
fn parse_kinds(raw: &[String]) -> Result<Vec<PurgeKind>, ApiError> {
    let mut out = Vec::with_capacity(raw.len());
    for k in raw {
        let kind = PurgeKind::parse(k)
            .ok_or_else(|| ApiError::BadRequest(format!("unknown purge kind '{k}'")))?;
        if !out.contains(&kind) {
            out.push(kind);
        }
    }
    Ok(out)
}

/// Create a preview job.
///
/// Deployment-admin only: `org:manage` in every org that exists. This is
/// stricter than the app-scoped alternative on purpose — in a multi-tenant
/// deployment a single tenant's admin cannot purge, only a global operator
/// can. In the common single-tenant self-hosted case it is simply "the admin".
pub async fn preview(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(req): Json<PreviewReq>,
) -> Result<(StatusCode, Json<PurgeJobView>), ApiError> {
    require_deployment_admin(&state, &auth).await?;

    let kinds = parse_kinds(&req.kinds)?;

    // `all_time` and a range are mutually exclusive, matching the CHECK
    // constraint on the table. Rejecting here as well gives a readable message
    // instead of a constraint violation surfacing as a 500.
    let window = if req.all_time {
        if req.range_start.is_some() || req.range_end.is_some() {
            return Err(ApiError::BadRequest(
                "all_time cannot be combined with a range".into(),
            ));
        }
        Window::All
    } else {
        match (req.range_start, req.range_end) {
            (Some(start), Some(end)) => Window::Range { start, end },
            _ => {
                return Err(ApiError::BadRequest(
                    "supply both range_start and range_end, or set all_time".into(),
                ))
            }
        }
    };

    let env_filter_active = req.environment_ids.is_some();
    let env_count = req.environment_ids.as_ref().map(|v| v.len()).unwrap_or(0);
    sauron_purge::validate_scope(&kinds, env_filter_active, env_count, window)
        .map_err(|e| ApiError::BadRequest(e.message()))?;

    let mut conn = crate::routes::db(&state).await?;

    let app = repo::get_app(&mut conn, req.app_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let (project_id, org_id) = repo::app_ancestry(&mut conn, req.app_id)
        .await?
        .ok_or(ApiError::NotFound)?;

    // Environments must belong to THIS app. Without this an operator could
    // name another app's environment id and the predicate would simply match
    // nothing — a purge that silently does less than it says.
    //
    // These are ENROLLMENT ids (`app_environments.id`), not catalogue ids
    // (`environments.id`), and the distinction is not cosmetic. The migration
    // text for `analytics_events` / `error_events` says
    // `REFERENCES environments(id)`, but migration 000033 renamed the tables
    // while keeping their OIDs, so the DDL text lies about its own target.
    // VERIFIED against `pg_constraint` on a freshly migrated database: the real
    // referent is `app_environments`. Validating against
    // `enrollment.environment_id` would accept the catalogue id, which then
    // matches no event row at all — an env-scoped purge that reports success
    // and deletes nothing. The enrollment id is also what `?environment_id=`
    // means everywhere else in this API.
    if let Some(ids) = &req.environment_ids {
        let known = repo::list_app_environments(&mut conn, req.app_id, false).await?;
        for id in ids {
            if !known.iter().any(|e| e.enrollment.id == *id) {
                return Err(ApiError::BadRequest(format!(
                    "environment {id} is not enrolled in this app"
                )));
            }
        }
    }

    let email = repo::user_email(&mut conn, auth.user_id)
        .await?
        .unwrap_or_default();

    let job = purge_repo::insert_purge_job(
        &mut conn,
        NewPurgeJob {
            org_id,
            app_id: req.app_id,
            app_slug: &app.slug,
            app_name: &app.name,
            environment_ids: req
                .environment_ids
                .as_ref()
                .map(|v| json!(v.iter().map(|u| u.to_string()).collect::<Vec<_>>())),
            kinds: json!(kinds.iter().map(|k| k.slug()).collect::<Vec<_>>()),
            range_start: req.range_start,
            range_end: req.range_end,
            all_time: req.all_time,
            requested_by: Some(auth.user_id),
            requested_by_email: &email,
        },
    )
    .await?;

    audit::record(
        &mut conn,
        auth.user_id,
        audit::Entry::new(
            org_id,
            audit::action::DATA_PURGE_PREVIEW,
            audit::entity::DATA_PURGE,
        )
        .target(job.id, &app.name)
        .project(project_id, String::new())
        .app(req.app_id, &app.name)
        .changes(json!({
            "kinds": req.kinds,
            "all_time": req.all_time,
            "range_start": req.range_start,
            "range_end": req.range_end,
            "environment_ids": req.environment_ids,
        })),
    )
    .await;

    Ok((
        StatusCode::ACCEPTED,
        Json(view(&job, state.cfg.purge_preview_ttl_secs)?),
    ))
}

#[derive(Deserialize)]
pub struct ConfirmReq {
    /// Must equal the app's slug.
    pub confirm_text: String,
}

/// Promote `previewed` -> `pending`.
///
/// Typing the SLUG is the only confirmation that forces attention onto the
/// thing that actually goes wrong. The realistic failure is not a mis-click —
/// it is purging the WRONG APP, because the operator saw a problem and forgot
/// which app was selected. A typed literal like `PURGE` proves intent and
/// proves nothing about scope; the slug proves scope.
pub async fn confirm(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(job_id): Path<Uuid>,
    Json(req): Json<ConfirmReq>,
) -> Result<Json<PurgeJobView>, ApiError> {
    require_deployment_admin(&state, &auth).await?;
    let mut conn = crate::routes::db(&state).await?;

    let job = purge_repo::get_purge_job(&mut conn, job_id)
        .await?
        .ok_or(ApiError::NotFound)?;

    if job.status != "previewed" {
        return Err(ApiError::BadRequest(format!(
            "job is '{}', not 'previewed'",
            job.status
        )));
    }
    // Compared against the SNAPSHOT on the job, not against a freshly-read
    // app row: if the app were renamed between preview and confirm, the
    // operator would be typing the name they were shown.
    if req.confirm_text.trim() != job.app_slug {
        return Err(ApiError::BadRequest(
            "confirmation text does not match the app slug".into(),
        ));
    }

    let n = purge_repo::confirm_purge_job(
        &mut conn,
        job_id,
        state.cfg.purge_preview_ttl_secs,
        &job.requested_by_email,
    )
    .await?;
    if n == 0 {
        // The TTL is enforced in the UPDATE's predicate, so zero rows here
        // means it lapsed between the read above and the write.
        return Err(ApiError::BadRequest(
            "this preview has expired; run it again to see current counts".into(),
        ));
    }

    audit::record(
        &mut conn,
        auth.user_id,
        audit::Entry::new(
            job.org_id,
            audit::action::DATA_PURGE_CONFIRM,
            audit::entity::DATA_PURGE,
        )
        .target(job.id, &job.app_name)
        .app(job.app_id, &job.app_name)
        .changes(json!({
            "kinds": job.kinds,
            "all_time": job.all_time,
            "range_start": job.range_start,
            "range_end": job.range_end,
            "estimated_counts": job.estimated_counts,
            "cold_rows_skipped": job.cold_rows_skipped,
        })),
    )
    .await;

    let fresh = purge_repo::get_purge_job(&mut conn, job_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(view(&fresh, state.cfg.purge_preview_ttl_secs)?))
}

/// Request cancellation.
///
/// A `pending` job is cancelled outright. A `running` one is marked
/// `cancelling`; the worker observes that on a write it was making anyway and
/// stops after the current batch. **Rows already deleted are not restored** —
/// the report shows how far it got.
pub async fn cancel(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(job_id): Path<Uuid>,
) -> Result<Json<PurgeJobView>, ApiError> {
    require_deployment_admin(&state, &auth).await?;
    let mut conn = crate::routes::db(&state).await?;

    let job = purge_repo::get_purge_job(&mut conn, job_id)
        .await?
        .ok_or(ApiError::NotFound)?;

    let email = repo::user_email(&mut conn, auth.user_id)
        .await?
        .unwrap_or_default();
    let n = purge_repo::cancel_purge_job(&mut conn, job_id, Some(auth.user_id), &email).await?;
    if n == 0 {
        return Err(ApiError::BadRequest(format!(
            "job is '{}' and can no longer be cancelled",
            job.status
        )));
    }

    audit::record(
        &mut conn,
        auth.user_id,
        audit::Entry::new(
            job.org_id,
            audit::action::DATA_PURGE_CANCEL,
            audit::entity::DATA_PURGE,
        )
        .target(job.id, &job.app_name)
        .app(job.app_id, &job.app_name)
        .changes(json!({ "status_before": job.status })),
    )
    .await;

    let fresh = purge_repo::get_purge_job(&mut conn, job_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(view(&fresh, state.cfg.purge_preview_ttl_secs)?))
}

pub async fn get_job(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(job_id): Path<Uuid>,
) -> Result<Json<PurgeJobView>, ApiError> {
    require_deployment_admin(&state, &auth).await?;
    let mut conn = crate::routes::db(&state).await?;
    let job = purge_repo::get_purge_job(&mut conn, job_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(view(&job, state.cfg.purge_preview_ttl_secs)?))
}

#[derive(Serialize)]
pub struct PurgeCatalog {
    pub kinds: Vec<KindView>,
    pub jobs: Vec<Value>,
}

#[derive(Serialize)]
pub struct KindView {
    pub slug: &'static str,
    /// `raw` rows are deleted; `rollup` rows are recomputed and deleted only
    /// when nothing survives.
    pub class: &'static str,
    /// False for kinds whose table has no `environment_id`. The UI must
    /// disable these when an environment filter is active — accepting the tick
    /// and quietly doing something narrower would be worse than refusing it.
    pub env_scoped: bool,
}

/// Job history plus the kind vocabulary.
///
/// The vocabulary is served rather than hardcoded in the client so the two
/// cannot drift: a kind added to `sauron-purge` appears in the UI, and one
/// removed disappears, without a matching frontend change.
pub async fn list_jobs(
    auth: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<PurgeCatalog>, ApiError> {
    require_deployment_admin(&state, &auth).await?;
    let mut conn = crate::routes::db(&state).await?;
    let org_ids =
        repo::orgs_with_permission(&mut conn, auth.user_id, sauron_auth::perm::ORG_MANAGE).await?;
    let jobs = purge_repo::list_purge_jobs(&mut conn, &org_ids, 100).await?;
    drop(conn);

    Ok(Json(PurgeCatalog {
        kinds: sauron_purge::ALL
            .iter()
            .map(|k| KindView {
                slug: k.slug(),
                class: if k.is_raw() { "raw" } else { "rollup" },
                env_scoped: k.env_scoped(),
            })
            .collect(),
        jobs: jobs
            .iter()
            .map(serde_json::to_value)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| ApiError::Internal(e.to_string()))?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_kinds_are_refused_rather_than_ignored() {
        assert!(parse_kinds(&["sessions".into(), "nope".into()]).is_err());
    }

    #[test]
    fn duplicate_kinds_collapse() {
        let k = parse_kinds(&["sessions".into(), "sessions".into()]).unwrap();
        assert_eq!(k, vec![PurgeKind::Sessions]);
    }

    #[test]
    fn every_kind_slug_is_accepted() {
        for k in sauron_purge::ALL {
            assert_eq!(
                parse_kinds(&[k.slug().to_string()]).unwrap(),
                vec![*k],
                "slug {} not accepted",
                k.slug()
            );
        }
    }
}
