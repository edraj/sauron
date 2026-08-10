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
    /// The query language. Wins over `filter`/`q` when non-empty.
    pub query: Option<String>,
    /// `column` or `-column`. Restricted to keyset-backed orderings — see
    /// `search::parse_sort`.
    pub sort: Option<String>,
    /// Opaque token from the previous page's `next_cursor`.
    pub cursor: Option<String>,
    #[serde(default = "default_events_since_days")]
    pub since_days: i64,
    #[serde(default = "default_events_list_limit")]
    pub limit: i64,
    /// Accepted and IGNORED since S2c — see `issues::ListQuery::offset` for the
    /// full reasoning. Short version: keyset paging replaced it, and dropping
    /// the field would turn every bookmarked `?offset=50` into a `400` from an
    /// unknown parameter. This route DID have a working `offset`, so unlike the
    /// occurrences list it genuinely has such bookmarks. Follow `next_cursor`.
    #[allow(dead_code)]
    #[serde(default)]
    pub offset: i64,
    // See `RangeQuery`'s comment: `environment_id` comes from `RawQuery`, not
    // this struct.
}

fn default_events_list_limit() -> i64 {
    50
}

/// The furthest back this list will look — see `events_list` for why it is a
/// tenth of the sibling lists' 3650.
///
/// The default below is this same number, and must stay so. It read 3650 until
/// this was fixed, so **every unparameterised request was silently narrowed
/// tenfold**: the handler served 365 days while `clamped` was `null`, meaning
/// the envelope actively asserted that no narrowing had taken place. A default
/// above the ceiling cannot be honoured, only misreported.
const EVENTS_MAX_SINCE_DAYS: i64 = 365;

fn default_events_since_days() -> i64 {
    EVENTS_MAX_SINCE_DAYS
}

/// The searched analytics event stream.
///
/// **Answers a [`SearchEnvelope`](super::search::SearchEnvelope), not a bare
/// array, since S2c** — the last of the slice's three lists, and the same
/// change for the same reasons as `issues::list` and `issues::events`: the
/// array had nowhere to put `total`, `next_cursor` or the planner's `clamped`
/// notice. `dashboard/src/lib/api/events.ts`' `listEvents` moves with it in
/// Task 7, alongside `issues.ts`' `listIssues`/`listIssueEvents` — all three
/// clients still read a bare array today and must migrate together.
///
/// Three input spellings, one execution path: `query=` (the language),
/// `filter=`+`q=` (the pre-language wire format, bridged through
/// `from_legacy`), or nothing.
pub async fn events_list(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<EventsListQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<super::search::SearchEnvelope<AnalyticsEvent>>, ApiError> {
    let mut conn = db(&state).await?;
    // `_with_perms` rather than the plain `authorized_read_scope`: `event:read`
    // authorizes the list, and the caller's `env:read` at that same resolved
    // scope decides whether an `environment:<name>` predicate is answerable.
    // One ancestry+grant resolution answers both.
    let (scope, perms) = super::scope::authorized_read_scope_with_perms(
        &mut conn,
        auth.user_id,
        app_id,
        perm::EVENT_READ,
        raw_query.as_deref(),
    )
    .await?;

    let node = super::search::resolve_query(
        q.query.as_deref(),
        &q.filter,
        q.q.as_deref().filter(|s| !s.is_empty()),
        sauron_query::Resource::Events,
    )?;
    // **`IncludingBody`, unconditionally, and that is a decision rather than an
    // oversight — read this before "fixing" it to `text_search_reach(&perms)`.**
    //
    // `TextSearchReach` and `reject_withheld_body` answer one question: is the
    // caller filtering over a column their own rows arrive with NULLED? That
    // nulling is `symbolicate::strip_event_body`, which takes an
    // `&mut ErrorEvent` and is applied by `gate_event_body` on the issues and
    // occurrences routes. Nothing on this route strips anything: an
    // `AnalyticsEvent`'s `properties`/`contexts`/`extra`/`tags` are serialized
    // whole to every caller `event:read` admits. There is no withheld half
    // here, so there is nothing for a narrowed reach to protect —
    // `EventsLower::text` reflects the same fact by taking no reach at all and
    // scanning all four payload columns unconditionally.
    //
    // Deriving it from `symbolicate::text_search_reach` would be actively
    // wrong, and not subtly: that predicate is `issue:read AND event:read`, so
    // a perfectly ordinary analytics-only custom role — `event:read`, no
    // `issue:read` — would come back `ShellOnly`, and
    // `reject_withheld_dimensions` would then 403 their `filter=tag:eq:k=v`,
    // their `properties.plan:pro` and their `workflow:` chip. Those are
    // filters over their own product analytics, which the same request returns
    // to them in full. It would also be incoherent: it refuses the SHARP
    // per-key probe while `?q=` — the blunt one — still ILIKEs
    // `contexts`/`extra`/`properties`/`tags::text` for the same caller.
    //
    // This is also the answer to the deferred `NON_WITHHELD_JSON_COLUMNS` item.
    // That list is keyed on the COLUMN name and holds only `properties`, so a
    // `ShellOnly` reach here would refuse `extra.*` and `contexts.*` on this
    // resource. The list is not wrong — `error_events.extra` really is nulled
    // one level up, and the catalog declares ONE `extra` dimension shared by
    // both resources (`R_OCC_EVENTS`), so adding `extra`/`contexts` to it would
    // open a genuine hole on the occurrences route while fixing a phantom one
    // here. The fix belongs at the caller, which knows which table it is
    // reading. Fail-closed default preserved; see
    // `search::NON_WITHHELD_JSON_COLUMNS`.
    let reach = sauron_db::repo::TextSearchReach::IncludingBody;
    // The SECOND axis is emphatically NOT a no-op here. `environment` is a real
    // `Store::Column("environment_id")` on Events, so `query=environment:staging`
    // resolves a NAME app-wide (`prepare::resolve_environments` is keyed on
    // `app_id` alone) and the answer to "does that environment exist" is
    // readable straight off `total`. Environment names are `env:read` territory,
    // which `event:read` — all this route requires — does not cover. Runs BEFORE
    // `prepare`, so the name is never even looked up. See
    // `search::reject_withheld_environment`.
    //
    // Not a regression this task introduces: `repo::list_analytics_events`
    // resolved `filter=environment:eq:<name>` app-wide with no `env:read` check
    // at all, so this closes a hole rather than opening one.
    super::search::reject_withheld_dimensions(
        &node,
        reach,
        super::search::EnvNameReach::for_perms(&perms),
    )?;

    let prepared = sauron_db::query_plan::prepare::prepare(&node, app_id, Utc::now(), &mut conn)
        .await
        .map_err(super::search::map_plan_error)?;
    let (sort_col, descending) = super::search::parse_sort(
        q.sort.as_deref(),
        &["occurred_at", "name", "distinct_id", "session_id"],
        "occurred_at",
    )?;
    // `parse_sort` already refused anything outside the list, so this cannot be
    // None; the expect states that rather than inventing a fallback ordering
    // that would page unstably if the two lists ever drifted apart.
    let sort = sauron_db::repo::EventSort::from_column(&sort_col)
        .expect("parse_sort whitelist and EventSort::from_column must agree");
    let after = match q.cursor.as_deref() {
        Some(c) => Some(
            // `sort.is_temporal()` closes a forged-type-tag gap: the cursor's
            // `key` and its `t`/`s` value tag are independent fields on the
            // wire, so matching the key alone (as this call used to) let a
            // `session_id|<uuid>|t:…` cursor through carrying the wrong kind
            // of value. `EventSort` stays the single source of truth for
            // which kind each column needs; `decode` is where it is now
            // enforced, rather than trusting this call site to ask.
            sauron_db::query_plan::cursor::decode(c, &sort_col, sort.is_temporal())
                .map_err(|e| ApiError::BadRequest(e.to_string()))?,
        ),
        None => None,
    };

    // `Clamp.field` is the GENERIC name "since" — `prepare` does not know which
    // resource it ran for. On THIS resource the window column is `occurred_at`.
    //
    // The outer bound is 365 days, not the other two lists' 3650, and that is
    // pre-existing and deliberate: free text here scans `jsonb::text` over the
    // largest table in the system, which no index can serve, so the window
    // stays bounded rather than defaulting to effectively all history. Widening
    // it to match the siblings would be a performance change wearing a
    // consistency costume. What it must NOT do is stay silent: a caller who
    // asks for 3650 is served 365, and `resolve_window` says so in `clamped`.
    let window = super::search::resolve_window(
        "occurred_at",
        Utc::now(),
        q.since_days,
        EVENTS_MAX_SINCE_DAYS,
        prepared.clamp,
    );
    let since = window.since;
    let limit = q.limit.clamp(1, 200);

    let search = repo::EventSearch {
        node: &node,
        ctx: &prepared.ctx,
        since,
        sort,
        descending,
        after,
        limit,
    };
    let mut rows = repo::search_events(&mut conn, &scope, &search)
        .await
        .map_err(super::search::map_plan_error)?;
    let (total, total_is_capped) =
        repo::count_events(&mut conn, &scope, &search, super::search::COUNT_CAP)
            .await
            .map_err(super::search::map_plan_error)?;

    // `limit + 1` rows were fetched; the surplus one is the has-more probe and
    // must not be served.
    let has_more = rows.len() as i64 > limit;
    rows.truncate(limit as usize);
    // The cursor's value is read through `sort`, not off `occurred_at`
    // directly: a page sorted by `name` (or any other column) must mint a
    // cursor carrying THAT column's value, or the next request pages against
    // an ordering the cursor was never a position within. `cursor_value`
    // (Task 2) is the one place the nullable-column coalescing rule is
    // spelled — see its doc comment — so it is called here rather than
    // re-derived.
    let next_cursor = has_more.then(|| {
        let last = rows.last().expect("has_more implies a row");
        sauron_db::query_plan::cursor::encode(&sauron_db::query_plan::cursor::Cursor {
            key: sort_col.clone(),
            value: sort.cursor_value(last),
            id: last.id,
        })
    });

    Ok(Json(super::search::SearchEnvelope {
        data: rows,
        total,
        total_is_capped,
        next_cursor,
        clamped: window.clamped,
    }))
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
