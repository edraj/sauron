//! Sessions API, scoped to an app: a filterable list, and the flagship
//! per-session timeline that merges analytics events, errors, and performance
//! transactions into one chronological stream.

use axum::extract::{Path, Query, State};
use axum::Json;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use sauron_auth::{authorize_app, perm, AuthUser};
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
) -> Result<Json<Vec<Session>>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::EVENT_READ).await?;
    let since = Utc::now() - Duration::days(q.since_days.clamp(1, 365));
    let limit = q.limit.clamp(1, 200);
    Ok(Json(
        repo::list_sessions(
            &mut conn,
            app_id,
            since,
            limit,
            q.offset.max(0),
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
        error: ErrorEvent,
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

pub async fn detail(
    auth: AuthUser,
    State(state): State<AppState>,
    Path((app_id, session_id)): Path<(Uuid, String)>,
) -> Result<Json<SessionDetail>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::EVENT_READ).await?;

    let session = repo::get_session(&mut conn, app_id, &session_id)
        .await?
        .ok_or(ApiError::NotFound)?;

    let events = repo::events_for_session(&mut conn, app_id, &session_id, 500).await?;
    let errors = repo::errors_for_session(&mut conn, app_id, &session_id, 500).await?;
    let txns = repo::transactions_for_session(&mut conn, app_id, &session_id, 500).await?;

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
            error: e,
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
