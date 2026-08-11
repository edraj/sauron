//! Ingest failure recovery: inspect, replay, or discard events that never
//! persisted.
//!
//! Gated on `require_deployment_admin` — the same
//! `org:manage`-in-every-org shape Storage and the tier policy use. Not merely
//! for symmetry: the dominant failure is a payload that never decoded, so it
//! carries no `org_id` and there is nothing to scope an org-level grant
//! against. `org_id` IS stored where it is known, so an org-scoped view can be
//! added later without a migration.

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use sauron_db::models::{IngestFailurePayload, IngestFailureRow};
use sauron_db::repo;

use crate::error::ApiError;
use crate::AppState;

/// Page size ceiling. Groups are cheap to render but each carries a message and
/// an app name, and an unbounded `limit` is a memory amplifier pointed at the
/// API by anyone who can reach this route.
const MAX_LIMIT: i64 = 200;
const DEFAULT_LIMIT: i64 = 50;

/// Retained payloads returned in one page of the drill-down.
const MAX_PAYLOAD_LIMIT: i64 = 100;

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    pub status: Option<String>,
    pub error_kind: Option<String>,
    pub limit: Option<i64>,
    /// Opaque `<rfc3339>|<uuid>` keyset cursor from `next_cursor`.
    pub cursor: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ListResponse {
    pub failures: Vec<IngestFailureRow>,
    /// Absent on the last page. Opaque to the client by contract — it encodes
    /// the tiebroken keyset position, and a client that parsed and rebuilt it
    /// would silently skip rows at page boundaries.
    pub next_cursor: Option<String>,
}

fn parse_cursor(raw: &str) -> Result<(chrono::DateTime<chrono::Utc>, Uuid), ApiError> {
    let (ts, id) = raw
        .split_once('|')
        .ok_or_else(|| ApiError::BadRequest("malformed cursor".into()))?;
    let ts = chrono::DateTime::parse_from_rfc3339(ts)
        .map_err(|_| ApiError::BadRequest("malformed cursor timestamp".into()))?
        .with_timezone(&chrono::Utc);
    let id = Uuid::parse_str(id).map_err(|_| ApiError::BadRequest("malformed cursor id".into()))?;
    Ok((ts, id))
}

/// One page of failure groups, newest activity first.
pub async fn list(
    auth: sauron_auth::AuthUser,
    State(state): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Result<Json<ListResponse>, ApiError> {
    super::admin::require_deployment_admin(&state, &auth).await?;

    let limit = q.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let cursor = match q.cursor.as_deref() {
        Some(c) => Some(parse_cursor(c)?),
        None => None,
    };

    let mut conn = crate::routes::db(&state).await?;
    // One extra row, then trimmed: this is how the response knows whether a
    // next page exists without a second COUNT over the table.
    let mut rows = repo::list_ingest_failures(
        &mut conn,
        q.status.as_deref(),
        q.error_kind.as_deref(),
        cursor,
        limit + 1,
    )
    .await?;
    drop(conn);

    let next_cursor = if rows.len() as i64 > limit {
        rows.truncate(limit as usize);
        rows.last()
            .map(|r| format!("{}|{}", r.last_seen_at.to_rfc3339(), r.id))
    } else {
        None
    };

    Ok(Json(ListResponse {
        failures: rows,
        next_cursor,
    }))
}

#[derive(Debug, Deserialize)]
pub struct PayloadQuery {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

/// The retained payloads behind one group.
pub async fn payloads(
    auth: sauron_auth::AuthUser,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(q): Query<PayloadQuery>,
) -> Result<Json<Vec<IngestFailurePayload>>, ApiError> {
    super::admin::require_deployment_admin(&state, &auth).await?;
    let limit = q.limit.unwrap_or(20).clamp(1, MAX_PAYLOAD_LIMIT);
    let offset = q.offset.unwrap_or(0).max(0);

    let mut conn = crate::routes::db(&state).await?;
    let rows = repo::list_ingest_failure_payloads(&mut conn, id, limit, offset).await?;
    Ok(Json(rows))
}

#[derive(Debug, Serialize)]
pub struct RetryResponse {
    /// Payloads actually put back on the ingest stream.
    pub requeued: usize,
    /// Retained payloads the re-injection could not place. Reported rather than
    /// folded into `requeued`, because a partial retry that reads as a complete
    /// one is how an operator concludes the problem is fixed when it is not.
    pub failed: usize,
    /// Occurrences that were never retained and so can never be replayed. Zero
    /// for most groups; large for exactly the mass failures where an operator
    /// most needs to know that "Retry" does not mean "recover everything".
    pub unrecoverable: i64,
}

/// Replay every retained payload in a group through the real pipeline.
///
/// Re-injected onto the ingest stream rather than processed here, so a retry
/// exercises the same path production ingest takes. Processing inline would
/// test a subtly different one, and would hold an HTTP request open for the
/// duration of a database write storm.
pub async fn retry(
    auth: sauron_auth::AuthUser,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<RetryResponse>, ApiError> {
    super::admin::require_deployment_admin(&state, &auth).await?;

    let mut conn = crate::routes::db(&state).await?;
    let group = repo::get_ingest_failure(&mut conn, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let items = repo::start_ingest_failure_retry(&mut conn, id).await?;

    let mut requeued = 0usize;
    let mut failed = 0usize;
    for item in &items {
        let payload = match serde_json::to_string(&item.payload) {
            Ok(p) => p,
            Err(_) => {
                // Stored as JSONB, so this cannot normally happen; if it does,
                // the row is not replayable and must not be reported as sent.
                failed += 1;
                let _ = repo::fail_ingest_failure_payload(
                    &mut conn,
                    item.id,
                    "payload could not be serialized for replay",
                )
                .await;
                continue;
            }
        };
        match state
            .redis
            .xadd_job(&payload, sauron_redis::INGEST_STREAM_MAXLEN_DEFAULT)
            .await
        {
            Ok(_) => requeued += 1,
            Err(e) => {
                failed += 1;
                // Returned to the pool immediately: leaving it stamped
                // `requeued_at` would mark it in flight forever, and the group
                // would sit in `requeued` awaiting a verdict nobody will send.
                let _ = repo::fail_ingest_failure_payload(
                    &mut conn,
                    item.id,
                    &format!("re-enqueue failed: {e}"),
                )
                .await;
            }
        }
    }

    crate::audit::record_all_orgs(
        &mut conn,
        auth.user_id,
        crate::audit::Entry::new(
            Uuid::nil(),
            crate::audit::action::INGEST_FAILURE_RETRY,
            crate::audit::entity::INGEST_FAILURE,
        )
        .target(id, &group.error_kind)
        .changes(crate::audit::created(
            crate::audit::entity::INGEST_FAILURE,
            &[
                ("fingerprint", serde_json::json!(group.fingerprint)),
                ("error_kind", serde_json::json!(group.error_kind)),
                ("retained", serde_json::json!(requeued)),
            ],
        )),
    )
    .await;

    Ok(Json(RetryResponse {
        requeued,
        failed,
        unrecoverable: group.dropped,
    }))
}

/// Discard a failure group permanently.
///
/// A hard DELETE. The audit entry is written FIRST and is the only intended
/// survivor — writing it afterwards would lose the record entirely if the
/// delete succeeded and the process died, which is the one ordering that leaves
/// no trace of an irreversible action.
pub async fn drop_group(
    auth: sauron_auth::AuthUser,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    super::admin::require_deployment_admin(&state, &auth).await?;

    let mut conn = crate::routes::db(&state).await?;
    let group = repo::get_ingest_failure(&mut conn, id)
        .await?
        .ok_or(ApiError::NotFound)?;

    crate::audit::record_all_orgs(
        &mut conn,
        auth.user_id,
        crate::audit::Entry::new(
            Uuid::nil(),
            crate::audit::action::INGEST_FAILURE_DROP,
            crate::audit::entity::INGEST_FAILURE,
        )
        .target(id, &group.error_kind)
        .changes(crate::audit::created(
            crate::audit::entity::INGEST_FAILURE,
            &[
                ("fingerprint", serde_json::json!(group.fingerprint)),
                ("error_kind", serde_json::json!(group.error_kind)),
                ("error_message", serde_json::json!(group.error_message)),
                ("occurrences", serde_json::json!(group.occurrences)),
                ("retained", serde_json::json!(group.retained)),
                ("dropped", serde_json::json!(group.dropped)),
            ],
        )),
    )
    .await;

    let deleted = repo::delete_ingest_failure(&mut conn, id).await?;
    if deleted == 0 {
        return Err(ApiError::NotFound);
    }

    Ok(Json(serde_json::json!({
        "deleted": deleted,
        "payloads_discarded": group.retained,
    })))
}
