//! Sessions API, scoped to an app: a filterable list, and the flagship
//! per-session timeline that merges analytics events, errors, and performance
//! transactions into one chronological stream.

use axum::extract::{Path, Query, RawQuery, State};
use axum::Json;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use sauron_auth::{perm, AuthUser};
use sauron_db::models::{AnalyticsEvent, ErrorEvent, Session, Transaction};
use sauron_db::repo;

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
    pub distinct_id: Option<String>,
    pub device_key: Option<String>,
    // `environment_id` is deliberately NOT a field here — it is read from the
    // raw query string via `RawQuery` + `scope::authorized_read_scope`
    // instead of this `Query<T>` extractor. See `routes::scope`'s module docs
    // for the extractor trap this avoids.
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
) -> Result<Json<Vec<Session>>, ApiError> {
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
    Ok(Json(
        repo::list_sessions(
            &mut conn,
            scope,
            since,
            limit,
            super::clamp_offset(q.offset),
            q.distinct_id.as_deref(),
            q.device_key.as_deref(),
        )
        .await?,
    ))
}

/// One entry on the session timeline. Tagged by `kind` so the frontend can
/// render events, errors and transactions with distinct treatments.
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum TimelineItem {
    Event {
        at: DateTime<Utc>,
        event: AnalyticsEvent,
    },
    Error {
        at: DateTime<Utc>,
        // Boxed: an inline ErrorEvent is 716 bytes against 420 for the next
        // largest variant, which would bloat every TimelineItem in the vec.
        error: Box<ErrorEvent>,
    },
    Transaction {
        at: DateTime<Utc>,
        transaction: Transaction,
    },
}

impl TimelineItem {
    fn at(&self) -> DateTime<Utc> {
        match self {
            TimelineItem::Event { at, .. }
            | TimelineItem::Error { at, .. }
            | TimelineItem::Transaction { at, .. } => *at,
        }
    }
}

#[derive(Serialize)]
pub struct SessionDetail {
    pub session: Session,
    pub timeline: Vec<TimelineItem>,
}

// No bespoke query struct: `detail` takes no query parameters of its own —
// `environment_id` comes from `RawQuery` (see `ListQuery`'s comment above),
// not a `Query<T>` extractor.

pub async fn detail(
    auth: AuthUser,
    State(state): State<AppState>,
    Path((app_id, session_id)): Path<(Uuid, String)>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<SessionDetail>, ApiError> {
    let mut conn = db(&state).await?;
    // `_with_perms` rather than `authorized_read_scope`: the timeline carries
    // whole `ErrorEvent` rows, and two further permissions apply to them —
    // `perm::ISSUE_READ` (an error BODY needs both halves of the pair, and
    // `event:read` below is only one) and `perm::SOURCE_READ` (the
    // de-obfuscated lines inside `stacktrace_symbolicated`). Both are the same
    // second permission question the issues routes ask, answered at the
    // resolved environment.
    let (scope, perms) = super::scope::authorized_read_scope_with_perms(
        &mut conn,
        auth.user_id,
        app_id,
        perm::EVENT_READ,
        raw_query.as_deref(),
    )
    .await?;

    let session = repo::get_session(&mut conn, scope.clone(), &session_id)
        .await?
        .ok_or(ApiError::NotFound)?;

    let events = repo::events_for_session(&mut conn, scope.clone(), &session_id, 500).await?;
    let mut errors = repo::errors_for_session(&mut conn, scope.clone(), &session_id, 500).await?;
    let txns = repo::transactions_for_session(&mut conn, scope, &session_id, 500).await?;
    drop(conn); // release the pooled conn; symbolication checks out its own

    // On-read symbolication, as `issues::detail` and `issues::events` do it. An
    // error symbolicated at ingest arrives already resolved and takes the fast
    // path inside; one that predates its source map (or whose upload landed
    // after the crash) is resolved here and persisted for hot partitions.
    // Without this the timeline is the one place a symbolicated app still
    // reads as minified frames.
    //
    // Guarded on the body pair for the reason `issues::detail` guards on it:
    // symbolication decompresses a blob and parses a source map (or walks
    // DWARF), and `gate_event_body` two lines down would throw the frames away
    // for a caller who lacks it.
    if crate::symbolicate::may_read_event_body(&perms) {
        crate::symbolicate::symbolicate_events(&state, app_id, &mut errors).await;
    }
    // Both gates before the boxed moves into `TimelineItem::Error` below — they
    // work on `[ErrorEvent]`, and once these are inside the enum they are no
    // longer a slice. `gate_source_context` must also stay AFTER the call
    // above: symbolication is what puts the context lines on the response in
    // the first place, so stripping first would strip nothing.
    crate::symbolicate::gate_source_context(&perms, &mut errors);
    crate::symbolicate::gate_event_body(&perms, &mut errors);

    let mut timeline: Vec<TimelineItem> =
        Vec::with_capacity(events.len() + errors.len() + txns.len());
    for e in events {
        timeline.push(TimelineItem::Event {
            at: e.occurred_at,
            event: e,
        });
    }
    for e in errors {
        timeline.push(TimelineItem::Error {
            at: e.occurred_at,
            error: Box::new(e),
        });
    }
    for t in txns {
        timeline.push(TimelineItem::Transaction {
            at: t.occurred_at,
            transaction: t,
        });
    }
    timeline.sort_by_key(|i| i.at());

    Ok(Json(SessionDetail { session, timeline }))
}
