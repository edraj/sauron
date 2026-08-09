//! Product-analytics queries, scoped to an app: top events, time series, and
//! the unified person profile (a person's events + errors).

use axum::extract::{Path, RawQuery, State};
use axum::Json;
use axum_extra::extract::Query;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use sauron_auth::{authorize_app, perm, AuthUser};
use sauron_db::models::{AnalyticsEvent, ErrorEvent, Issue};
use sauron_db::repo;
use sauron_db::repo::{EventCount, PersonRow, SeriesPoint};

use super::db;
use crate::error::ApiError;
use crate::AppState;

#[derive(Deserialize)]
pub struct RangeQuery {
    #[serde(default = "default_days")]
    pub since_days: i64,
    #[serde(default = "default_top")]
    pub limit: i64,
    pub name: Option<String>,
    // `environment_id` is deliberately NOT a field here — it is read from the
    // raw query string via `RawQuery` + `scope::authorized_read_scope` instead
    // of this `Query<T>` extractor. See `routes::scope`'s module docs for why:
    // an `Option<String>` field on this struct would go through
    // `axum_extra::extract::Query` (needed for other handlers' `Vec<String>`
    // fields), whose codec silently collapses `?environment_id=` to `None`.
}

fn default_days() -> i64 {
    30
}
fn default_top() -> i64 {
    20
}

pub async fn top_events(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<RangeQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<Vec<EventCount>>, ApiError> {
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
    let limit = q.limit.clamp(1, 100);
    Ok(Json(
        repo::top_events(&mut conn, scope, since, limit).await?,
    ))
}

pub async fn event_series(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<RangeQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<Vec<SeriesPoint>>, ApiError> {
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
    Ok(Json(
        repo::event_series(&mut conn, scope, q.name.as_deref(), since).await?,
    ))
}

#[derive(Deserialize)]
pub struct PersonQuery {
    #[serde(default = "default_person_limit")]
    pub limit: i64,
    // See `RangeQuery`'s comment: `environment_id` comes from `RawQuery`, not
    // this struct.
}

fn default_person_limit() -> i64 {
    50
}

#[derive(Serialize)]
pub struct PersonProfile {
    pub distinct_id: String,
    // `PersonRow`, not the raw `EventUser` model — see `repo::get_event_user`'s
    // doc comment: `first_seen`/`last_seen` here are environment-scoped, the
    // same fix F4 made for `list_persons`.
    pub user: Option<PersonRow>,
    pub events: Vec<AnalyticsEvent>,
    pub errors: Vec<ErrorEvent>,
}

pub async fn person(
    auth: AuthUser,
    State(state): State<AppState>,
    Path((app_id, distinct_id)): Path<(Uuid, String)>,
    Query(q): Query<PersonQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<PersonProfile>, ApiError> {
    let mut conn = db(&state).await?;
    // `_with_perms`: `errors` below is whole `ErrorEvent` rows (up to `limit`,
    // which clamps at 200), which carry two further permission questions —
    // `perm::ISSUE_READ` for the body at all and `perm::SOURCE_READ` for the
    // de-obfuscated lines inside it. The body gate matters most here: these
    // rows are already keyed to one identified person, so their payloads are
    // that person's crash data. See `sessions::detail` for the same note.
    let (scope, perms) = super::scope::authorized_read_scope_with_perms(
        &mut conn,
        auth.user_id,
        app_id,
        perm::EVENT_READ,
        raw_query.as_deref(),
    )
    .await?;
    let limit = q.limit.clamp(1, 200);

    let user = repo::get_event_user(&mut conn, scope.clone(), &distinct_id).await?;
    let events = repo::events_for_person(&mut conn, scope.clone(), &distinct_id, limit).await?;
    let mut errors = repo::error_events_for_person(&mut conn, scope, &distinct_id, limit).await?;
    crate::symbolicate::gate_source_context(&perms, &mut errors);
    crate::symbolicate::gate_event_body(&perms, &mut errors);

    Ok(Json(PersonProfile {
        distinct_id,
        user,
        events,
        errors,
    }))
}

// ---------------------------------------------------------------------------
// Users Explorer — searchable directory of people with activity counts.
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct PersonsQuery {
    pub search: Option<String>,
    #[serde(default = "default_persons_list_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
    // See `RangeQuery`'s comment: `environment_id` comes from `RawQuery`, not
    // this struct.
}

fn default_persons_list_limit() -> i64 {
    50
}

pub async fn persons_list(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<PersonsQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<Vec<PersonRow>>, ApiError> {
    let mut conn = db(&state).await?;
    let scope = super::scope::authorized_read_scope(
        &mut conn,
        auth.user_id,
        app_id,
        perm::EVENT_READ,
        raw_query.as_deref(),
    )
    .await?;
    let search = q.search.as_deref().filter(|s| !s.is_empty());
    Ok(Json(
        repo::list_persons(
            &mut conn,
            scope,
            search,
            q.limit.clamp(1, 200),
            super::clamp_offset(q.offset),
        )
        .await?,
    ))
}

// ---------------------------------------------------------------------------
// Event Explorer — the raw analytics event stream with filters.
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct EventsListQuery {
    #[serde(default)]
    pub filter: Vec<String>,
    pub q: Option<String>,
    #[serde(default = "default_events_since_days")]
    pub since_days: i64,
    #[serde(default = "default_events_list_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
    // See `RangeQuery`'s comment: `environment_id` comes from `RawQuery`, not
    // this struct.
}

fn default_events_list_limit() -> i64 {
    50
}
fn default_events_since_days() -> i64 {
    3650
}

pub async fn events_list(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<EventsListQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<Vec<AnalyticsEvent>>, ApiError> {
    let mut conn = db(&state).await?;
    let scope = super::scope::authorized_read_scope(
        &mut conn,
        auth.user_id,
        app_id,
        perm::EVENT_READ,
        raw_query.as_deref(),
    )
    .await?;
    let filters = sauron_db::filter::parse_filters(&q.filter, sauron_db::filter::EVENT_FILTERS)?;
    let search = q.q.as_deref().filter(|s| !s.is_empty());
    // Free-text search scans jsonb::text, which no index can serve; keep the
    // window bounded rather than defaulting to effectively all history.
    let since = Utc::now() - Duration::days(q.since_days.clamp(1, 365));
    Ok(Json(
        repo::list_analytics_events(
            &mut conn,
            scope,
            &filters,
            search,
            Some(since),
            q.limit.clamp(1, 200),
            super::clamp_offset(q.offset),
        )
        .await?,
    ))
}

// ---------------------------------------------------------------------------
// Overview — a single composite health + activity snapshot for the app.
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct Overview {
    pub totals: repo::OverviewTotals,
    pub error_rate: f64,
    pub crash_free_sessions: f64,
    pub events_series: Vec<SeriesPoint>,
    pub errors_series: Vec<SeriesPoint>,
    /// Empty — not absent — for a caller without `issue:read`; see `overview`.
    pub top_issues: Vec<Issue>,
    pub top_events: Vec<EventCount>,
}

pub async fn overview(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<RangeQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<Overview>, ApiError> {
    let mut conn = db(&state).await?;
    // `_with_perms`: this response mixes two gates. The aggregates are signal
    // data (`event:read`, which authorizes the call), but `top_issues` is
    // `Issue` rows — title, culprit, fingerprint, times_seen — i.e. exactly the
    // payload `issue:read` is the coarse gate for. Serving them off
    // `event:read` alone was the inverse of the body leak the same ruling
    // closed: the coarse gate is not a gate if a composite route routes around
    // it.
    let (scope, perms) = super::scope::authorized_read_scope_with_perms(
        &mut conn,
        auth.user_id,
        app_id,
        perm::EVENT_READ,
        raw_query.as_deref(),
    )
    .await?;
    let include_issues = perms.contains(perm::ISSUE_READ);
    let since = Utc::now() - Duration::days(q.since_days.clamp(1, 365));

    let totals = repo::overview_totals(&mut conn, scope.clone(), since).await?;
    let events_series = repo::event_series(&mut conn, scope.clone(), None, since).await?;
    // Deliberately `event:read`, even though the sibling `error_timeseries`
    // route gates the same signal on `issue:read`: both are per-day counts with
    // no issue identity attached, and the coarse gate is about *which issues
    // exist*, not *how many errors happened*. The inconsistency is real but
    // benign; recorded here so it is not "fixed" in the wrong direction.
    let errors_series = repo::error_series(&mut conn, scope.clone(), since).await?;
    // Skipped, not fetched-then-cleared: an omitted query is one fewer round
    // trip, and there is no way to accidentally serialize what was never read.
    let top_issues = if include_issues {
        repo::top_issues(&mut conn, scope.clone(), since, 5).await?
    } else {
        Vec::new()
    };
    let top_events = repo::top_events(&mut conn, scope, since, 5).await?;

    let error_rate = {
        let denom = totals.events + totals.errors;
        if denom > 0 {
            totals.errors as f64 / denom as f64
        } else {
            0.0
        }
    };
    let crash_free_sessions = if totals.sessions > 0 {
        1.0 - (totals.crashed_sessions as f64 / totals.sessions as f64)
    } else {
        1.0
    };

    Ok(Json(Overview {
        totals,
        error_rate,
        crash_free_sessions,
        events_series,
        errors_series,
        top_issues,
        top_events,
    }))
}

// ---------------------------------------------------------------------------
// Overview, split into independently-loadable sections
// ---------------------------------------------------------------------------
//
// `overview` above runs FIVE aggregates sequentially on ONE pooled connection
// and returns nothing until the last finishes, so its latency is their SUM.
// Measured against the 210k-event app on this machine: ~165 ms for the events
// count, ~160 ms for the errors count, ~180 ms for top-issues, plus the series —
// and every one of those scales with the range and the row count, so on a large
// deployment the page simply sits blank for seconds.
//
// The sections below are the same queries, addressable one at a time. Nothing is
// faster in isolation; what changes is that the browser issues them in PARALLEL,
// so wall-clock becomes the MAX rather than the sum, and each card paints the
// moment its own answer lands instead of waiting for the slowest.
//
// The split is along the seams that already exist: `overview_totals` is one
// statement (six sub-selects) and cannot be divided without multiplying round
// trips, whereas the series pair, top-issues and top-events are separate queries
// already and cost nothing to separate.
//
// `overview` is deliberately KEPT. It is a supported response shape, removing it
// would be a breaking API change for anyone scripting against it, and it remains
// the cheaper choice for a caller that genuinely wants all of it in one request
// (one round trip, one connection checkout, one authorization).

/// Derived scalars that used to be computed inside `overview`.
///
/// Kept next to the totals rather than in their own section: both are pure
/// arithmetic over `totals`, so serving them separately would mean either
/// re-running that query or making the client duplicate the formulas — and a
/// crash-free rate computed two ways eventually disagrees.
#[derive(Serialize)]
pub struct OverviewTotalsSection {
    pub totals: repo::OverviewTotals,
    pub error_rate: f64,
    pub crash_free_sessions: f64,
}

#[derive(Serialize)]
pub struct OverviewSeriesSection {
    pub events_series: Vec<SeriesPoint>,
    pub errors_series: Vec<SeriesPoint>,
}

/// Resolve the read scope for an overview section.
///
/// Every section authorizes independently and identically to `overview`'s own
/// check. That is not redundant work to be optimized away: each section is its
/// own HTTP request, so each must prove the caller may read this app in this
/// environment. Sharing a decision across them would mean trusting the client to
/// tell us it had already been authorized.
async fn overview_scope(
    state: &AppState,
    auth: &AuthUser,
    app_id: Uuid,
    raw_query: Option<&str>,
) -> Result<
    (
        sauron_db::scope::ReadScope,
        std::collections::HashSet<String>,
    ),
    ApiError,
> {
    let mut conn = db(state).await?;
    super::scope::authorized_read_scope_with_perms(
        &mut conn,
        auth.user_id,
        app_id,
        perm::EVENT_READ,
        raw_query,
    )
    .await
}

fn since_of(q: &RangeQuery) -> DateTime<Utc> {
    Utc::now() - Duration::days(q.since_days.clamp(1, 365))
}

/// The KPI tiles: totals plus the two rates derived from them.
pub async fn overview_totals(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<RangeQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<OverviewTotalsSection>, ApiError> {
    let (scope, _) = overview_scope(&state, &auth, app_id, raw_query.as_deref()).await?;
    let mut conn = db(&state).await?;
    let totals = repo::overview_totals(&mut conn, scope, since_of(&q)).await?;

    // Same formulas as `overview`, deliberately not extracted into a shared
    // helper: they are three lines each and the two call sites are in one file.
    let error_rate = {
        let denom = totals.events + totals.errors;
        if denom > 0 {
            totals.errors as f64 / denom as f64
        } else {
            0.0
        }
    };
    let crash_free_sessions = if totals.sessions > 0 {
        1.0 - (totals.crashed_sessions as f64 / totals.sessions as f64)
    } else {
        1.0
    };
    Ok(Json(OverviewTotalsSection {
        totals,
        error_rate,
        crash_free_sessions,
    }))
}

/// The two per-day series, together.
///
/// One section rather than two because the chart plots them on shared axes: a
/// request that delivered events without errors would render a graph that is
/// wrong rather than incomplete, and the two queries are comparable in cost so
/// there is no fast half to show early.
pub async fn overview_series(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<RangeQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<OverviewSeriesSection>, ApiError> {
    let (scope, _) = overview_scope(&state, &auth, app_id, raw_query.as_deref()).await?;
    let since = since_of(&q);
    let mut conn = db(&state).await?;
    let events_series = repo::event_series(&mut conn, scope.clone(), None, since).await?;
    let errors_series = repo::error_series(&mut conn, scope, since).await?;
    Ok(Json(OverviewSeriesSection {
        events_series,
        errors_series,
    }))
}

/// Top issues by occurrence count.
///
/// Requires `issue:read` IN ADDITION to the `event:read` that authorizes the
/// call, matching the D4 ruling: these are `Issue` rows — title, culprit,
/// fingerprint, counts — which is exactly what the coarse gate covers.
///
/// Returns 403, where `overview` returns an empty list. The composite route has
/// to degrade because one missing permission must not fail the whole response;
/// a section addressed on its own has no such constraint, and an empty array is
/// indistinguishable from "this app has no issues" — which would leave the UI
/// showing a reassuring blank card instead of saying the caller cannot see it.
pub async fn overview_top_issues(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<RangeQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<Vec<Issue>>, ApiError> {
    let (scope, perms) = overview_scope(&state, &auth, app_id, raw_query.as_deref()).await?;
    if !perms.contains(perm::ISSUE_READ) {
        return Err(ApiError::Auth(sauron_auth::AuthError::Forbidden));
    }
    let mut conn = db(&state).await?;
    let rows = repo::top_issues(&mut conn, scope, since_of(&q), 5).await?;
    Ok(Json(rows))
}

/// Top analytics events by count.
pub async fn overview_top_events(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<RangeQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<Vec<EventCount>>, ApiError> {
    let (scope, _) = overview_scope(&state, &auth, app_id, raw_query.as_deref()).await?;
    let mut conn = db(&state).await?;
    let rows = repo::top_events(&mut conn, scope, since_of(&q), 5).await?;
    Ok(Json(rows))
}

// ---------------------------------------------------------------------------
// Audience analytics — GET /users/summary.
// ---------------------------------------------------------------------------

/// DAU / MAU, guarding division by zero. Pure.
pub fn stickiness(dau: i64, mau: i64) -> f64 {
    if mau > 0 {
        dau as f64 / mau as f64
    } else {
        0.0
    }
}

#[derive(Serialize)]
pub struct UsersAnalytics {
    pub stats: repo::UserStats,
    pub stickiness: f64,
    pub series: Vec<repo::UserSeriesPoint>,
}

pub async fn users_summary(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<RangeQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<UsersAnalytics>, ApiError> {
    let mut conn = db(&state).await?;
    let scope = super::scope::authorized_read_scope(
        &mut conn,
        auth.user_id,
        app_id,
        perm::EVENT_READ,
        raw_query.as_deref(),
    )
    .await?;
    let now = Utc::now();
    let since = now - Duration::days(q.since_days.clamp(1, 365));

    let stats = repo::user_stats(&mut conn, scope.clone(), since, now).await?;
    let series = repo::active_user_series(&mut conn, scope, since).await?;
    let stickiness = stickiness(stats.dau, stats.mau);

    Ok(Json(UsersAnalytics {
        stats,
        stickiness,
        series,
    }))
}

// ---------------------------------------------------------------------------
// Session-engagement analytics — GET /sessions/summary.
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct SessionsAnalytics {
    pub stats: repo::SessionStats,
    pub duration_series: Vec<repo::SeriesAvgPoint>,
    pub duration_histogram: Vec<repo::HistoBucket>,
}

pub async fn sessions_summary(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<RangeQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<SessionsAnalytics>, ApiError> {
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

    let stats = repo::session_stats(&mut conn, scope.clone(), since).await?;
    let duration_series = repo::session_duration_series(&mut conn, scope.clone(), since).await?;
    let duration_histogram = repo::session_duration_histogram(&mut conn, scope, since).await?;

    Ok(Json(SessionsAnalytics {
        stats,
        duration_series,
        duration_histogram,
    }))
}

// ---------------------------------------------------------------------------
// Cross-tier errors timeseries — GET /errors/timeseries.
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct TimeseriesQuery {
    pub from: chrono::DateTime<chrono::Utc>,
    pub to: chrono::DateTime<chrono::Utc>,
    // `environment_id` is NOT a field here, same reasoning as `RangeQuery`
    // above — but doubly so for this struct: these three handlers must
    // reject *any* `environment_id`, including `?environment_id=`, and
    // `axum_extra::extract::Query` silently turning that into a missing
    // field is exactly the bug that let it slip through as a bare
    // `q.environment_id.is_some()` check (that check was `false` for
    // `?environment_id=`, so the "not supported yet" 400 below never fired).
    // Each handler instead reads `environment_id` via `RawQuery` +
    // `scope::raw_environment_id`, then rejects it through
    // `scope::reject_environment_id_with_message` — the same
    // `reject_environment_id*` call every other rejecting endpoint in this
    // crate makes, so `dashboard/src/lib/api/scope.ts`'s reconciliation grep
    // (`grep reject_environment_id`) finds these three too, instead of an
    // inline `.is_some()` check it cannot see.
}

/// Longest span a cross-tier timeseries may cover.
///
/// These endpoints route across hot Postgres and cold Parquet; an unbounded
/// `from`/`to` lets one request scan an app's entire cold dataset.
const MAX_TIMESERIES_DAYS: i64 = 400;

/// The reason all three cross-tier timeseries handlers below reject any
/// `environment_id` at all, named once so the three call sites can't drift
/// apart in wording.
const TIMESERIES_ENV_SCOPING_NOT_SUPPORTED: &str = "environment scoping is not available on \
     cross-tier timeseries yet — cold storage is not partitioned by environment";

impl TimeseriesQuery {
    /// Validate and clamp the requested window.
    fn range(
        &self,
    ) -> Result<(chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>), ApiError> {
        if self.to < self.from {
            return Err(ApiError::BadRequest("`to` must not precede `from`".into()));
        }
        let max = Duration::days(MAX_TIMESERIES_DAYS);
        if self.to - self.from > max {
            return Err(ApiError::BadRequest(format!(
                "time range must not exceed {MAX_TIMESERIES_DAYS} days"
            )));
        }
        Ok((self.from, self.to))
    }
}

#[derive(Serialize)]
pub struct DayCountOut {
    pub day: chrono::NaiveDate,
    pub count: i64,
}

impl From<sauron_tier::DayCount> for DayCountOut {
    fn from(d: sauron_tier::DayCount) -> Self {
        DayCountOut {
            day: d.day,
            count: d.count,
        }
    }
}

pub async fn error_timeseries(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<TimeseriesQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<Vec<DayCountOut>>, ApiError> {
    super::scope::reject_environment_id_with_message(
        super::scope::raw_environment_id(raw_query.as_deref()).as_deref(),
        TIMESERIES_ENV_SCOPING_NOT_SUPPORTED,
    )?;
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::ISSUE_READ).await?;
    drop(conn); // release the pooled conn before the router checks out its own
    let (from, to) = q.range()?;
    let series = crate::tier_read::error_counts_by_day(&state, app_id, from, to).await?;
    Ok(Json(
        series
            .into_iter()
            .map(|d| DayCountOut {
                day: d.day,
                count: d.count,
            })
            .collect(),
    ))
}

// ---------------------------------------------------------------------------
// Cross-tier analytics-events timeseries — GET /events/timeseries.
// ---------------------------------------------------------------------------

pub async fn event_timeseries(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<TimeseriesQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<Vec<DayCountOut>>, ApiError> {
    super::scope::reject_environment_id_with_message(
        super::scope::raw_environment_id(raw_query.as_deref()).as_deref(),
        TIMESERIES_ENV_SCOPING_NOT_SUPPORTED,
    )?;
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::EVENT_READ).await?;
    drop(conn); // release the pooled conn before the router checks out its own
    let (from, to) = q.range()?;
    let series = crate::tier_read::event_counts_by_day(&state, app_id, from, to).await?;
    Ok(Json(
        series
            .into_iter()
            .map(|d| DayCountOut {
                day: d.day,
                count: d.count,
            })
            .collect(),
    ))
}

// ---------------------------------------------------------------------------
// Cross-tier transactions timeseries — GET /transactions/timeseries.
// ADDITIVE (count/throughput) only; percentiles are holistic and served
// hot-only (Postgres) — see repo::transaction_counts_by_day_hot.
// ---------------------------------------------------------------------------

pub async fn transaction_timeseries(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<TimeseriesQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<Vec<DayCountOut>>, ApiError> {
    super::scope::reject_environment_id_with_message(
        super::scope::raw_environment_id(raw_query.as_deref()).as_deref(),
        TIMESERIES_ENV_SCOPING_NOT_SUPPORTED,
    )?;
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::EVENT_READ).await?;
    drop(conn); // release the pooled conn before the router checks out its own
    let (from, to) = q.range()?;
    let series = crate::tier_read::transaction_counts_by_day(&state, app_id, from, to).await?;
    Ok(Json(
        series
            .into_iter()
            .map(|d| DayCountOut {
                day: d.day,
                count: d.count,
            })
            .collect(),
    ))
}

#[cfg(test)]
mod stickiness_tests {
    use super::stickiness;

    #[test]
    fn ratio_of_dau_to_mau() {
        assert!((stickiness(5, 20) - 0.25).abs() < 1e-9);
    }

    #[test]
    fn zero_mau_is_zero_not_nan() {
        assert_eq!(stickiness(3, 0), 0.0);
    }
}

// ---------------------------------------------------------------------------
// Active Users — distinct people per UTC day
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct ActiveUsersSeries {
    /// `DayCountOut`, not `sauron_tier::DayCount`: the tier crate's type is
    /// deliberately serde-free (it is shared with the worker, which has no HTTP
    /// surface), and this is the same wire shape the other chart endpoints use.
    pub series: Vec<DayCountOut>,
    /// Days deliberately omitted from `series` because their count could not be
    /// computed exactly. Empty in the default configuration; see
    /// `tier_read::active_users_by_day`.
    pub partial_days: Vec<crate::tier_read::PartialDay>,
}

/// Distinct people per UTC day.
///
/// An AGGREGATE, so under the D4 ruling it needs only the `event:read` that
/// authorizes the call: it exposes no event body and no issue metadata, just a
/// count per day.
pub async fn active_users_series(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<RangeQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<ActiveUsersSeries>, ApiError> {
    let scope = {
        let mut conn = db(&state).await?;
        super::scope::authorized_read_scope(
            &mut conn,
            auth.user_id,
            app_id,
            perm::EVENT_READ,
            raw_query.as_deref(),
        )
        .await?
    };
    let to = Utc::now();
    let from = to - Duration::days(q.since_days.clamp(1, 365));
    let (series, partial_days) = crate::tier_read::active_users_by_day(&state, scope, from, to)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(ActiveUsersSeries {
        series: series.into_iter().map(DayCountOut::from).collect(),
        partial_days,
    }))
}
