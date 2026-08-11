//! The Wall of Shame's read side: `GET /v1/admin/audit`.
//!
//! Org-partitioned by construction. `org_id` is required and validated against
//! the caller's grants, so there is no deployment-wide view and one tenant can
//! never observe another's activity — the same line the storage report draws.

use axum::extract::{Query, State};
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use sauron_auth::{authorize_org, perm, AuthUser};
use sauron_db::repo::{self, AuditFilter};

use crate::error::ApiError;
use crate::AppState;

/// Page size. The default keeps the first paint small; the cap stops a client
/// asking for the whole trail in one request.
const DEFAULT_LIMIT: i64 = 50;
const MAX_LIMIT: i64 = 200;

#[derive(Debug, Deserialize)]
pub struct AuditQuery {
    /// Required: the org whose history to read.
    pub org_id: Uuid,
    pub project_id: Option<Uuid>,
    pub app_id: Option<Uuid>,
    pub environment_id: Option<Uuid>,
    pub actor_id: Option<Uuid>,
    pub action: Option<String>,
    pub entity_type: Option<String>,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    /// Opaque keyset cursor from a previous response's `next_cursor`.
    pub cursor: Option<String>,
    pub limit: Option<i64>,
    /// Include sign-in activity. Defaults to **false**: auth events are a
    /// separate stream, kept out of the admin feed so logins cannot bury the
    /// member, role and key events the Wall exists to surface.
    pub include_auth: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct AuditEntryView {
    pub id: Uuid,
    pub actor_id: Option<Uuid>,
    pub actor_email: String,
    pub action: String,
    pub entity_type: String,
    pub entity_id: Option<Uuid>,
    pub entity_name: String,
    pub project_id: Option<Uuid>,
    pub project_name: String,
    pub app_id: Option<Uuid>,
    pub app_name: String,
    pub environment_id: Option<Uuid>,
    pub environment_name: String,
    pub changes: Value,
    pub created_at: DateTime<Utc>,
    /// `"audit"` or `"inspector"`. The drawer uses this to explain why an
    /// inspector-sourced row carries no before/after diff.
    pub source: String,
}

#[derive(Debug, Serialize)]
pub struct FacetView {
    pub id: Option<Uuid>,
    pub label: String,
}

#[derive(Debug, Serialize)]
pub struct Facets {
    pub actors: Vec<FacetView>,
    pub actions: Vec<FacetView>,
    pub projects: Vec<FacetView>,
    pub apps: Vec<FacetView>,
    pub environments: Vec<FacetView>,
}

#[derive(Debug, Serialize)]
pub struct AuditResponse {
    pub entries: Vec<AuditEntryView>,
    /// `None` on the last page.
    pub next_cursor: Option<String>,
    pub facets: Facets,
}

/// Encode a keyset cursor as `<rfc3339>|<uuid>`.
///
/// Both halves travel, because the sort is on the tuple. A cursor carrying only
/// the timestamp would skip or repeat rows that share one — which entries
/// written by a single request always do.
fn encode_cursor(created_at: DateTime<Utc>, id: Uuid) -> String {
    format!("{}|{}", created_at.to_rfc3339(), id)
}

fn decode_cursor(raw: &str) -> Result<(DateTime<Utc>, Uuid), ApiError> {
    let (ts, id) = raw
        .split_once('|')
        .ok_or_else(|| ApiError::BadRequest("malformed cursor".into()))?;
    let ts = DateTime::parse_from_rfc3339(ts)
        .map_err(|_| ApiError::BadRequest("malformed cursor timestamp".into()))?
        .with_timezone(&Utc);
    let id = Uuid::parse_str(id).map_err(|_| ApiError::BadRequest("malformed cursor id".into()))?;
    Ok((ts, id))
}

/// Refuse an unknown entity family rather than filtering for it and returning
/// an empty page. An empty page is indistinguishable from "this org did
/// nothing", which is exactly the wrong answer to give someone auditing it — a
/// typo'd filter would read as an all-clear. Shared by both routes so the JSON
/// and CSV views cannot disagree about what is a valid filter.
fn validate_entity_type(entity_type: Option<&str>) -> Result<(), ApiError> {
    if let Some(et) = entity_type.filter(|s| !s.is_empty()) {
        if !crate::audit::entity::ALL.contains(&et) {
            return Err(ApiError::BadRequest(format!(
                "unknown entity_type: {et}; must be one of {}",
                crate::audit::entity::ALL.join(", ")
            )));
        }
    }
    Ok(())
}

/// The administrative trail for one org, newest first.
///
/// Requires an **org-scoped** `org:manage` grant in the requested org. No new
/// permission was introduced: the people who may read who did what are the
/// people who administer the org, which is the same line `/v1/admin/storage`
/// already draws.
pub async fn list(
    auth: AuthUser,
    State(state): State<AppState>,
    Query(q): Query<AuditQuery>,
) -> Result<Json<AuditResponse>, ApiError> {
    let mut conn = crate::routes::db(&state).await?;

    // The gate. `authorize_org` 403s when the caller holds no org-scoped
    // `org:manage` here, so passing another tenant's org_id cannot read it.
    authorize_org(&mut conn, auth.user_id, q.org_id, perm::ORG_MANAGE).await?;

    let limit = q.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);

    // Filtering FOR auth implies including it — otherwise selecting "Sign-in
    // activity" in the entity filter would return an empty page, which reads as
    // "nothing happened" rather than as two settings contradicting each other.
    let include_auth = q.include_auth.unwrap_or(false)
        || q.entity_type.as_deref() == Some(crate::audit::entity::AUTH);

    validate_entity_type(q.entity_type.as_deref())?;

    let filter = AuditFilter {
        project_id: q.project_id,
        app_id: q.app_id,
        environment_id: q.environment_id,
        actor_id: q.actor_id,
        action: q.action.clone().filter(|s| !s.is_empty()),
        entity_type: q.entity_type.clone().filter(|s| !s.is_empty()),
        from: q.from,
        to: q.to,
        cursor: match q.cursor.as_deref().filter(|s| !s.is_empty()) {
            Some(raw) => Some(decode_cursor(raw)?),
            None => None,
        },
        include_auth,
    };

    // Fetch one more than asked for: if it comes back, there is another page.
    // Counting instead would cost a second full scan of the unified feed to
    // answer a question the extra row answers for free.
    let mut rows = repo::list_audit_feed(&mut conn, q.org_id, &filter, limit + 1).await?;
    let has_more = rows.len() as i64 > limit;
    rows.truncate(limit as usize);
    let next_cursor = has_more
        .then(|| rows.last().map(|r| encode_cursor(r.created_at, r.id)))
        .flatten();

    let entries: Vec<AuditEntryView> = rows
        .into_iter()
        .map(|r| AuditEntryView {
            id: r.id,
            actor_id: r.actor_id,
            actor_email: r.actor_email,
            action: r.action,
            entity_type: r.entity_type,
            entity_id: r.entity_id,
            entity_name: r.entity_name,
            project_id: r.project_id,
            project_name: r.project_name,
            app_id: r.app_id,
            app_name: r.app_name,
            environment_id: r.environment_id,
            environment_name: r.environment_name,
            changes: r.changes,
            created_at: r.created_at,
            source: r.source,
        })
        .collect();

    // Facets take the same flag as the feed: a dropdown must only ever offer a
    // value that returns results.
    let facets = facets_for(&mut conn, q.org_id, include_auth).await?;

    Ok(Json(AuditResponse {
        entries,
        next_cursor,
        facets,
    }))
}

/// Cap on a single export.
///
/// High enough that a real org's whole trail fits, low enough that one request
/// cannot pin a connection building an unbounded string in memory — the export
/// is buffered, not streamed, for the same reason `active_users_csv` is:
/// `backend/Cargo.toml` carries neither `futures` nor `tokio-util`.
const MAX_EXPORT_ROWS: i64 = 10_000;

/// `GET /v1/admin/audit.csv` — the current filtered view, as a file.
///
/// A separate route rather than `?format=csv`, matching `active_users_csv`:
/// with a format parameter the success type collapses to `Response` for both
/// shapes and content negotiation via a query param is easy to mis-validate.
/// Both routes build their rows from the same `list_audit_feed` call, so they
/// cannot disagree about what the filter means.
///
/// Exports EVERY matching row up to the cap, not the page the browser happens
/// to be showing. An export that silently covered only the first fifty rows
/// would be worse than no export: it looks complete.
pub async fn export_csv(
    auth: AuthUser,
    State(state): State<AppState>,
    Query(q): Query<AuditQuery>,
) -> Result<axum::response::Response, ApiError> {
    let mut conn = crate::routes::db(&state).await?;
    // Same gate as `list`, resolved the same way. A CSV route that forgot this
    // would be an unauthenticated dump of the whole trail.
    authorize_org(&mut conn, auth.user_id, q.org_id, perm::ORG_MANAGE).await?;

    let include_auth = q.include_auth.unwrap_or(false)
        || q.entity_type.as_deref() == Some(crate::audit::entity::AUTH);
    validate_entity_type(q.entity_type.as_deref())?;

    let filter = AuditFilter {
        project_id: q.project_id,
        app_id: q.app_id,
        environment_id: q.environment_id,
        actor_id: q.actor_id,
        action: q.action.clone().filter(|s| !s.is_empty()),
        entity_type: q.entity_type.clone().filter(|s| !s.is_empty()),
        from: q.from,
        to: q.to,
        // Deliberately ignores `cursor`: an export is of the whole filtered
        // set, not of the page the caller has scrolled to.
        cursor: None,
        include_auth,
    };

    // One over the cap, so hitting it is detectable rather than inferred from a
    // suspiciously round row count.
    let mut rows = repo::list_audit_feed(&mut conn, q.org_id, &filter, MAX_EXPORT_ROWS + 1).await?;
    let truncated = rows.len() as i64 > MAX_EXPORT_ROWS;
    rows.truncate(MAX_EXPORT_ROWS as usize);

    let mut out = String::new();
    crate::csv::write_row(
        &mut out,
        &[
            "when_utc",
            "actor_email",
            "action",
            "entity_type",
            "target",
            "project",
            "app",
            "environment",
            "source",
            "changes_json",
        ],
    );
    for r in &rows {
        // RFC 3339 in UTC rather than a locale-formatted local time: a
        // spreadsheet is exactly where someone re-reads this months later in a
        // different timezone, and an unmarked local timestamp is unrecoverable.
        let when = r.created_at.to_rfc3339();
        let changes = if r.changes.is_null() {
            String::new()
        } else {
            r.changes.to_string()
        };
        crate::csv::write_row(
            &mut out,
            &[
                &when,
                &r.actor_email,
                &r.action,
                &r.entity_type,
                &r.entity_name,
                &r.project_name,
                &r.app_name,
                &r.environment_name,
                &r.source,
                &changes,
            ],
        );
    }

    // The truncation marker rides in the FILENAME, not only in a header: the
    // file outlives the response, and a header nobody sees cannot warn the
    // person who opens the spreadsheet six months later.
    let filename = format!(
        "sauron-audit-{}{}.csv",
        q.org_id,
        if truncated { "-truncated" } else { "" }
    );

    axum::response::Response::builder()
        .header(axum::http::header::CONTENT_TYPE, "text/csv; charset=utf-8")
        .header(
            axum::http::header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{filename}\""),
        )
        // For a caller that IS looking, e.g. the dashboard warning the user.
        .header("x-sauron-truncated", truncated.to_string())
        .body(axum::body::Body::from(out))
        .map_err(|e| ApiError::Internal(e.to_string()))
}

/// Filter options, sourced from the trail itself.
///
/// Deliberately not from the org's live projects/apps: an option that returns
/// no results is noise, and — more importantly — the entries an administrator
/// most wants are often about things that have since been DELETED. Building the
/// dropdowns from live rows would hide exactly those.
async fn facets_for(
    conn: &mut sauron_db::AsyncPgConnection,
    org_id: Uuid,
    include_auth: bool,
) -> Result<Facets, ApiError> {
    let actors = repo::audit_actor_facets(conn, org_id, include_auth).await?;
    let actions = repo::audit_action_facets(conn, org_id, include_auth).await?;
    let scopes = repo::audit_scope_facets(conn, org_id).await?;

    let map = |v: Vec<repo::AuditFacet>| -> Vec<FacetView> {
        v.into_iter()
            .map(|f| FacetView {
                id: f.id,
                label: f.label,
            })
            .collect()
    };

    Ok(Facets {
        actors: map(actors),
        actions: map(actions),
        projects: map(scopes.projects),
        apps: map(scopes.apps),
        environments: map(scopes.environments),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_round_trips() {
        let ts = DateTime::parse_from_rfc3339("2026-08-11T03:04:05.123456Z")
            .unwrap()
            .with_timezone(&Utc);
        let id = Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap();
        let (ts2, id2) = decode_cursor(&encode_cursor(ts, id)).unwrap();
        // Microsecond precision must survive: entries written by one request
        // differ only below the second, and a cursor that rounded would skip
        // or repeat them.
        assert_eq!(ts, ts2);
        assert_eq!(id, id2);
    }

    #[test]
    fn malformed_cursors_are_rejected_not_ignored() {
        // Silently ignoring a bad cursor would restart pagination from the top
        // and quietly re-serve page one forever.
        for bad in [
            "",
            "not-a-cursor",
            "2026-08-11T03:04:05Z",
            "2026-08-11T03:04:05Z|not-a-uuid",
            "nonsense|11111111-2222-3333-4444-555555555555",
        ] {
            assert!(decode_cursor(bad).is_err(), "accepted malformed cursor {bad:?}");
        }
    }
}
