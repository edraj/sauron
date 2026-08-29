//! Screen analytics: per-screen views/events/users/exceptions + on-read dwell.
use axum::extract::{Path, Query, RawQuery, State};
use axum::Json;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use sauron_auth::{perm, AuthUser};
use sauron_db::models::{AnalyticsEvent, ErrorEvent};
use sauron_db::repo;
use sauron_db::repo::SortSpec;
use sauron_db::scope::Range;

use super::db;
use crate::error::ApiError;
use crate::openapi::ErrorResponse;
use crate::AppState;

/// The list's `keys` CTE is `SELECT screen FROM ev UNION SELECT screen FROM
/// ex` — a `UNION`, which de-duplicates — so one row per distinct screen and
/// therefore unique across the result set. This is the tiebreak the hard-coded
/// `ORDER BY views DESC, k.screen ASC` already used; [`SortSpec`] expresses
/// that same pairing rather than a new one.
const SCREEN_TIEBREAK: &str = "k.screen";

/// What `?sort=` accepts on [`list`].
const SCREEN_SORTS: &[&str] = &[
    "views",
    "screen",
    "events",
    "exceptions",
    "users",
    "avg_dwell_ms",
];

/// The wire `?sort=` value for the screens list, resolved to a validated
/// [`SortSpec`].
///
/// A free function for the reason `devices::device_sort_spec`'s doc comment
/// gives — see it first. Every arm's `&'static str` is what reaches the SQL;
/// the `String` `parse_sort` returns never does.
///
/// Every name but `screen` resolves to an OUTPUT ALIAS of the outer select,
/// not to a CTE column. That matters for `avg_dwell_ms`, which exists ONLY as
/// the alias: it is `total_dwell_ms / views` computed in the select list, and
/// no CTE has a column of that name to fall back on.
pub(crate) fn screen_sort_spec(raw: Option<&str>) -> Result<SortSpec, ApiError> {
    let (column, descending) = super::search::parse_sort(raw, SCREEN_SORTS, "views")?;
    let (column, nulls_last) = match column.as_str() {
        // The grouping key. Qualified, because `k.screen` is where it comes
        // from and the bare alias would resolve to the same column anyway.
        "screen" => ("k.screen", false),
        "events" => ("events", false),
        "exceptions" => ("exceptions", false),
        "users" => ("users", false),
        "avg_dwell_ms" => ("avg_dwell_ms", false),
        // `parse_sort` refused everything else, so this is the default.
        _ => ("views", false),
    };
    // Every one of these is `COALESCE`d in the select list (and `k.screen` is
    // filtered `IS NOT NULL` in all four CTEs), so none can be NULL and
    // `nulls_last` is uniformly false rather than defensively true.
    Ok(SortSpec {
        column,
        descending,
        tiebreak: SCREEN_TIEBREAK,
        nulls_last,
    })
}

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ScreenListQuery {
    #[serde(default = "days30")]
    pub since_days: i64,
    /// Absolute window bounds, `from` INCLUSIVE and `to` EXCLUSIVE, overriding
    /// `since_days` when either is present. See `analytics::RangeQuery` for why
    /// these are two plain fields rather than a flattened shared struct.
    pub from: Option<chrono::DateTime<chrono::Utc>>,
    pub to: Option<chrono::DateTime<chrono::Utc>>,
    pub q: Option<String>,
    #[serde(default = "lim50")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
    /// A column name from [`SCREEN_SORTS`], optionally `-`-prefixed to ascend.
    /// Absent means `views` descending.
    pub sort: Option<String>,
    // `environment_id` is deliberately NOT a field here — it is read from the
    // raw query string via `RawQuery` + `scope::authorized_read_scope`
    // instead of this `Query<T>` extractor. See `routes::scope`'s module docs
    // for the extractor trap this avoids.
}
fn days30() -> i64 {
    30
}
fn lim50() -> i64 {
    50
}

#[utoipa::path(
    get, path = "/v1/apps/{app_id}/screens", tag = "Analytics",
    summary = "Screens seen in this app",
    params(("app_id" = Uuid, Path, description = "The app."), ScreenListQuery, super::search::TimeFilterQuery), security(("bearerAuth" = [])),
    responses((status = 200, description = "Screens with their view counts.", body = Vec<repo::ScreenRow>),
              (status = 400, description = "Malformed query or sort.", body = ErrorResponse), (status = 401, description = "Missing or invalid access token.", body = ErrorResponse), (status = 403, description = "No grant covers this scope.", body = ErrorResponse), (status = 503, description = "Query exceeded its time budget, or a required rollup is missing.", body = ErrorResponse)),
)]
pub async fn list(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<ScreenListQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<Vec<repo::ScreenRow>>, ApiError> {
    let mut conn = db(&state).await?;
    let scope = super::scope::authorized_read_scope(
        &mut conn,
        auth.user_id,
        app_id,
        perm::EVENT_READ,
        raw_query.as_deref(),
    )
    .await?;
    let win =
        super::search::resolve_range("occurred_at", q.from, q.to, q.since_days, Utc::now(), 365)?;
    let pattern = match q.q.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(term) => repo::like_contains(term),
        None => "%".to_string(),
    };
    let sort = screen_sort_spec(q.sort.as_deref())?;
    let rows = repo::screen_list(
        &mut conn,
        scope,
        win,
        &pattern,
        q.limit.clamp(1, 200),
        super::clamp_offset(q.offset),
        sort,
    )
    .await?;
    Ok(Json(rows))
}

/// `GET /v1/apps/{app_id}/counts/screens` — how many rows ``list`` would page.
///
/// Takes ``ScreenListQuery`` verbatim, so the count and the list are built from ONE
/// predicate description: a count that answered over a different window, search
/// term or environment than the table beside it would be a number nobody could
/// act on. The page fields it receives (`limit`/`offset`/`sort`) are ignored —
/// no page boundary and no ordering changes a total.
///
/// Same permission and the same `RawQuery` environment handling as the list.
/// That is a disclosure property, not a consistency nicety: a count resolved
/// over a wider scope than the list would leak the SIZE of data the caller
/// cannot read.
#[utoipa::path(
    get, path = "/v1/apps/{app_id}/counts/screens", tag = "Analytics",
    summary = "Count matching screens",
    params(("app_id" = Uuid, Path, description = "The app."), ScreenListQuery, super::search::TimeFilterQuery), security(("bearerAuth" = [])),
    responses((status = 200, description = "The count.", body = super::search::CountEnvelope), (status = 401, description = "Missing or invalid access token.", body = ErrorResponse), (status = 403, description = "No grant covers this scope.", body = ErrorResponse), (status = 503, description = "Query exceeded its time budget, or a required rollup is missing.", body = ErrorResponse)),
)]
pub async fn count(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<ScreenListQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<super::search::CountEnvelope>, ApiError> {
    let mut conn = db(&state).await?;
    let scope = super::scope::authorized_read_scope(
        &mut conn,
        auth.user_id,
        app_id,
        perm::EVENT_READ,
        raw_query.as_deref(),
    )
    .await?;
    let win =
        super::search::resolve_range("occurred_at", q.from, q.to, q.since_days, Utc::now(), 365)?;
    let pattern = match q.q.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(term) => repo::like_contains(term),
        None => "%".to_string(),
    };
    let (total, total_is_capped) =
        repo::count_screens(&mut conn, scope, win, &pattern, super::search::COUNT_CAP).await?;
    Ok(Json(super::search::CountEnvelope {
        total,
        total_is_capped,
    }))
}

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ScreenDetailQuery {
    pub name: String,
    #[serde(default = "days30")]
    pub since_days: i64,
    /// Absolute window bounds, `from` INCLUSIVE and `to` EXCLUSIVE, overriding
    /// `since_days` when either is present. See `analytics::RangeQuery` for why
    /// these are two plain fields rather than a flattened shared struct.
    pub from: Option<chrono::DateTime<chrono::Utc>>,
    pub to: Option<chrono::DateTime<chrono::Utc>>,
    // `environment_id` is deliberately NOT a field here — see
    // `ScreenListQuery`'s comment above.
}

/// The screen detail header: stat tiles and nothing else.
///
/// It used to also carry `recent_events` and `recent_exceptions` — 20 rows
/// each, the exceptions being whole `ErrorEvent`s with `stacktrace` and
/// `breadcrumbs` attached. The dashboard stopped rendering them when the four
/// paged sections replaced the static cards, so every load of this page was
/// shipping (and symbolicating, and permission-gating) two payloads nobody
/// read. Removed rather than left dead: the gating cost is real, and a
/// response field with no consumer is one a future reader will assume is live.
///
/// The rows now come from `/v1/apps/{app_id}/screens/{events,exceptions}`,
/// which page properly instead of truncating at 20.
#[derive(Serialize, utoipa::ToSchema)]
pub struct ScreenDetail {
    pub stats: repo::ScreenStats,
}

#[utoipa::path(
    get, path = "/v1/apps/{app_id}/screens/detail", tag = "Analytics",
    summary = "Headline metrics for one screen",
    description = "\
The screen is named by a query parameter, not a path segment, because screen \
names are arbitrary caller-supplied text.

The four breakdown cards (events, exceptions, devices, users) are **separate \
fetch-on-demand endpoints** rather than part of this response, so opening a \
screen does not pay for panels nobody expands.",
    params(("app_id" = Uuid, Path, description = "The app."), ScreenDetailQuery, super::search::TimeFilterQuery), security(("bearerAuth" = [])),
    responses((status = 200, description = "Headline metrics.", body = ScreenDetail),
              (status = 400, description = "Missing screen name.", body = ErrorResponse), (status = 401, description = "Missing or invalid access token.", body = ErrorResponse), (status = 403, description = "No grant covers this scope.", body = ErrorResponse), (status = 503, description = "Query exceeded its time budget, or a required rollup is missing.", body = ErrorResponse)),
)]
pub async fn detail(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<ScreenDetailQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<ScreenDetail>, ApiError> {
    if q.name.trim().is_empty() {
        return Err(ApiError::BadRequest("name is required".into()));
    }
    let mut conn = db(&state).await?;
    // Plain `authorized_read_scope`, not `_with_perms`: this response is now
    // aggregate counts only. The `ErrorEvent` bodies that needed
    // `perm::ISSUE_READ`/`perm::SOURCE_READ` gating moved to
    // `section_exceptions`, which still resolves and applies them.
    let scope = super::scope::authorized_read_scope(
        &mut conn,
        auth.user_id,
        app_id,
        perm::EVENT_READ,
        raw_query.as_deref(),
    )
    .await?;
    let win =
        super::search::resolve_range("occurred_at", q.from, q.to, q.since_days, Utc::now(), 365)?;
    let stats = repo::screen_stats(&mut conn, scope, win, &q.name).await?;
    Ok(Json(ScreenDetail { stats }))
}

// ===========================================================================
// Screen detail sections — Events / Exceptions / Devices / Users
// ===========================================================================
//
// Four sibling routes behind the four collapsible cards on `#/screens/:name`.
// They are separate endpoints rather than `?filter=screen:eq:…` on the
// existing lists because the query language reaches none of them: the `screen`
// dimension in `sauron_query::catalog` is scoped to Issues+Occurrences, there
// is no app-wide occurrences route for it to land on, and "devices/users on a
// screen" is not a column filter at all — it is an aggregate over a different
// table. See the design note in
// `docs/superpowers/specs/2026-08-18-screen-detail-sections-design.md`.
//
// All four answer a BARE ARRAY of at most `limit` rows. The dashboard requests
// `limit + 1` and treats the surplus row as its has-more probe (the house
// `overFetched` pattern), so no count endpoint is needed and none is offered.

/// Shared query for all four section routes.
///
/// Deliberately NOT `#[serde(flatten)]`-composed out of smaller structs:
/// `flatten` routes every field through `serde`'s untyped content buffer,
/// where a query-string `limit=25` arrives as the STRING `"25"` and fails to
/// deserialize into `i64` — a 422 on a request that looks correct.
#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ScreenSectionQuery {
    pub name: String,
    #[serde(default = "days30")]
    pub since_days: i64,
    /// Absolute window bounds, `from` INCLUSIVE and `to` EXCLUSIVE, overriding
    /// `since_days` when either is present. See `analytics::RangeQuery` for why
    /// these are two plain fields rather than a flattened shared struct.
    pub from: Option<chrono::DateTime<chrono::Utc>>,
    pub to: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default = "lim25")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
    // `environment_id` is deliberately NOT a field here — see
    // `ScreenListQuery`'s comment above.
}
fn lim25() -> i64 {
    25
}

/// Upper bound on a section page. The dashboard asks for 26 (25 + the has-more
/// probe); this leaves room for a caller that wants larger pages without
/// letting one request pull an unbounded slice of a partitioned table.
const SECTION_LIMIT_MAX: i64 = 100;

/// Validate and clamp the parts every section route shares.
///
/// One function so the four routes cannot drift into disagreeing about what a
/// window or a page is — a section answering over a different `since` than its
/// siblings would put four mutually inconsistent lists under one set of stat
/// tiles.
fn section_bounds(q: &ScreenSectionQuery) -> Result<(Range, i64, i64), ApiError> {
    if q.name.trim().is_empty() {
        return Err(ApiError::BadRequest("name is required".into()));
    }
    let win =
        super::search::resolve_range("occurred_at", q.from, q.to, q.since_days, Utc::now(), 365)?;
    let limit = q.limit.clamp(1, SECTION_LIMIT_MAX);
    Ok((win, limit, super::clamp_offset(q.offset)))
}

/// `GET /v1/apps/{app_id}/screens/events` — a screen's analytics events, paged.
#[utoipa::path(
    get, path = "/v1/apps/{app_id}/screens/events", tag = "Analytics",
    summary = "Events recorded on one screen",
    params(("app_id" = Uuid, Path, description = "The app."), ScreenSectionQuery, super::search::TimeFilterQuery), security(("bearerAuth" = [])),
    responses((status = 200, description = "Events for the screen.", body = Vec<AnalyticsEvent>),
              (status = 400, description = "Missing screen name.", body = ErrorResponse), (status = 401, description = "Missing or invalid access token.", body = ErrorResponse), (status = 403, description = "No grant covers this scope.", body = ErrorResponse), (status = 503, description = "Query exceeded its time budget, or a required rollup is missing.", body = ErrorResponse)),
)]
pub async fn section_events(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<ScreenSectionQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<Vec<AnalyticsEvent>>, ApiError> {
    let (range, limit, offset) = section_bounds(&q)?;
    let mut conn = db(&state).await?;
    let scope = super::scope::authorized_read_scope(
        &mut conn,
        auth.user_id,
        app_id,
        perm::EVENT_READ,
        raw_query.as_deref(),
    )
    .await?;
    let rows =
        repo::recent_events_for_screen(&mut conn, scope, &q.name, range, limit, offset).await?;
    Ok(Json(rows))
}

/// `GET /v1/apps/{app_id}/screens/exceptions` — a screen's exceptions, paged.
///
/// `_with_perms` and the two `gate_*` calls, for the reason [`detail`]
/// documents: `ErrorEvent` rows carry `perm::ISSUE_READ` (the body) and
/// `perm::SOURCE_READ` (de-obfuscated frames) questions that `EVENT_READ` does
/// not answer. Gating REDACTS rather than refuses, matching `detail` — the
/// dashboard hides the card outright for a role without `issue:read`, so a
/// 403 here would only turn a hidden card into a broken one for anyone
/// calling the API directly.
#[utoipa::path(
    get, path = "/v1/apps/{app_id}/screens/exceptions", tag = "Analytics",
    summary = "Exceptions raised on one screen",
    params(("app_id" = Uuid, Path, description = "The app."), ScreenSectionQuery, super::search::TimeFilterQuery), security(("bearerAuth" = [])),
    responses((status = 200, description = "Error events for the screen.", body = Vec<ErrorEvent>),
              (status = 400, description = "Missing screen name.", body = ErrorResponse), (status = 401, description = "Missing or invalid access token.", body = ErrorResponse), (status = 403, description = "No grant covers this scope.", body = ErrorResponse), (status = 503, description = "Query exceeded its time budget, or a required rollup is missing.", body = ErrorResponse)),
)]
pub async fn section_exceptions(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<ScreenSectionQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<Vec<ErrorEvent>>, ApiError> {
    let (range, limit, offset) = section_bounds(&q)?;
    let mut conn = db(&state).await?;
    let (scope, perms) = super::scope::authorized_read_scope_with_perms(
        &mut conn,
        auth.user_id,
        app_id,
        perm::EVENT_READ,
        raw_query.as_deref(),
    )
    .await?;
    let mut rows =
        repo::recent_exceptions_for_screen(&mut conn, scope, &q.name, range, limit, offset).await?;
    crate::symbolicate::gate_source_context(&perms, &mut rows);
    crate::symbolicate::gate_event_body(&perms, &mut rows);
    Ok(Json(rows))
}

/// `GET /v1/apps/{app_id}/screens/devices` — the devices seen on a screen.
///
/// `perm::EVENT_READ`, matching `devices::list`: this exposes no device a
/// caller could not already page from the inventory.
#[utoipa::path(
    get, path = "/v1/apps/{app_id}/screens/devices", tag = "Analytics",
    summary = "Devices that reached one screen",
    params(("app_id" = Uuid, Path, description = "The app."), ScreenSectionQuery, super::search::TimeFilterQuery), security(("bearerAuth" = [])),
    responses((status = 200, description = "Devices for the screen.", body = Vec<repo::ScreenDeviceRow>),
              (status = 400, description = "Missing screen name.", body = ErrorResponse), (status = 401, description = "Missing or invalid access token.", body = ErrorResponse), (status = 403, description = "No grant covers this scope.", body = ErrorResponse), (status = 503, description = "Query exceeded its time budget, or a required rollup is missing.", body = ErrorResponse)),
)]
pub async fn section_devices(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<ScreenSectionQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<Vec<repo::ScreenDeviceRow>>, ApiError> {
    let (range, limit, offset) = section_bounds(&q)?;
    let mut conn = db(&state).await?;
    let scope = super::scope::authorized_read_scope(
        &mut conn,
        auth.user_id,
        app_id,
        perm::EVENT_READ,
        raw_query.as_deref(),
    )
    .await?;
    let rows = repo::devices_for_screen(&mut conn, scope, &q.name, range, limit, offset).await?;
    Ok(Json(rows))
}

/// `GET /v1/apps/{app_id}/screens/users` — the users seen on a screen.
///
/// `perm::EVENT_READ`, matching `analytics::persons_list`.
#[utoipa::path(
    get, path = "/v1/apps/{app_id}/screens/users", tag = "Analytics",
    summary = "Users who reached one screen",
    params(("app_id" = Uuid, Path, description = "The app."), ScreenSectionQuery, super::search::TimeFilterQuery), security(("bearerAuth" = [])),
    responses((status = 200, description = "Users for the screen.", body = Vec<repo::ScreenUserRow>),
              (status = 400, description = "Missing screen name.", body = ErrorResponse), (status = 401, description = "Missing or invalid access token.", body = ErrorResponse), (status = 403, description = "No grant covers this scope.", body = ErrorResponse), (status = 503, description = "Query exceeded its time budget, or a required rollup is missing.", body = ErrorResponse)),
)]
pub async fn section_users(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<ScreenSectionQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<Vec<repo::ScreenUserRow>>, ApiError> {
    let (range, limit, offset) = section_bounds(&q)?;
    let mut conn = db(&state).await?;
    let scope = super::scope::authorized_read_scope(
        &mut conn,
        auth.user_id,
        app_id,
        perm::EVENT_READ,
        raw_query.as_deref(),
    )
    .await?;
    let rows = repo::users_for_screen(&mut conn, scope, &q.name, range, limit, offset).await?;
    Ok(Json(rows))
}

#[cfg(test)]
mod section_bounds_tests {
    use super::*;
    use chrono::Duration;

    fn q(name: &str, since_days: i64, limit: i64, offset: i64) -> ScreenSectionQuery {
        ScreenSectionQuery {
            name: name.to_string(),
            since_days,
            from: None,
            to: None,
            limit,
            offset,
        }
    }

    /// An empty or whitespace `name` would otherwise reach
    /// `screen = $3` as `''`, which is a valid query returning an empty list —
    /// a 200 and a blank card, indistinguishable from "this screen has no
    /// users" and impossible to tell from the UI.
    #[test]
    fn a_blank_name_is_refused() {
        for blank in ["", "   ", "\t", "\n"] {
            assert!(
                section_bounds(&q(blank, 30, 25, 0)).is_err(),
                "a blank name must be refused, got ok for {blank:?}"
            );
        }
        assert!(section_bounds(&q("Home", 30, 25, 0)).is_ok());
    }

    /// The clamp must bound BOTH ends. A `limit=0` returns an empty page the
    /// dashboard reads as "no more rows", silently truncating the list; a
    /// negative limit is a Postgres error.
    #[test]
    fn limit_is_clamped_to_a_usable_page() {
        let (_, lo, _) = section_bounds(&q("Home", 30, 0, 0)).expect("ok");
        assert_eq!(lo, 1, "limit=0 must clamp up, not through");
        let (_, neg, _) = section_bounds(&q("Home", 30, -5, 0)).expect("ok");
        assert_eq!(neg, 1);
        let (_, hi, _) = section_bounds(&q("Home", 30, 10_000, 0)).expect("ok");
        assert_eq!(hi, SECTION_LIMIT_MAX);
        let (_, exact, _) = section_bounds(&q("Home", 30, 26, 0)).expect("ok");
        assert_eq!(exact, 26, "the dashboard's 25+1 probe must pass through");
    }

    /// A negative offset reaches `OFFSET -1` as a Postgres error, i.e. a 500
    /// on a request a user can produce by hand.
    #[test]
    fn offset_is_never_negative() {
        let (_, _, off) = section_bounds(&q("Home", 30, 25, -1)).expect("ok");
        assert_eq!(off, 0);
    }

    /// `since_days` shares `detail`'s 1..=365 clamp. A 0 would make `since`
    /// equal to now and every card render empty; an unbounded value would
    /// widen the scan past the retention window for no extra rows.
    #[test]
    fn the_window_matches_the_rest_of_the_page() {
        let before = Utc::now();
        let (zero, _, _) = section_bounds(&q("Home", 0, 25, 0)).expect("ok");
        assert!(
            zero.from <= before - Duration::days(1) + Duration::seconds(1),
            "since_days=0 must clamp to at least one day"
        );
        let (huge, _, _) = section_bounds(&q("Home", 100_000, 25, 0)).expect("ok");
        assert!(
            huge.from >= before - Duration::days(366),
            "since_days must clamp at 365"
        );
        // Open above unless the caller asked for a closed window — the whole
        // point of `to` being an `Option`.
        assert_eq!(huge.to, None);
    }
}

#[cfg(test)]
mod screen_sort_tests {
    use super::*;

    /// `(wire name, expected column, expected nulls_last)` for [`list`].
    ///
    /// Written out rather than derived from the name — deriving it would
    /// reproduce the implementation's own rule and stop being an independent
    /// check. Only `screen` is qualified; the rest are output aliases.
    const SCREEN_EXPECTED: &[(&str, &str, bool)] = &[
        ("views", "views", false),
        ("screen", "k.screen", false),
        ("events", "events", false),
        ("exceptions", "exceptions", false),
        ("users", "users", false),
        ("avg_dwell_ms", "avg_dwell_ms", false),
    ];

    /// The finding this exists for: an arm mapping a whitelisted name to a
    /// valid-but-wrong column (`"events" => "views"` — both real columns of
    /// this select list) compiles, returns 200, and silently sorts by the
    /// wrong data.
    #[test]
    fn every_whitelisted_name_maps_to_its_own_column() {
        for (name, column, nulls_last) in SCREEN_EXPECTED {
            let spec = screen_sort_spec(Some(name)).expect("whitelisted");
            assert_eq!(spec.column, *column, "screen sort `{name}` mapped wrong");
            assert_eq!(
                spec.nulls_last, *nulls_last,
                "screen sort `{name}`: wrong nulls_last"
            );
            assert_eq!(spec.tiebreak, SCREEN_TIEBREAK, "screen sort `{name}`");
        }
    }

    /// A column added to `SCREEN_SORTS` without a matching arm falls through
    /// to `_ =>` and sorts by `views` — a 200 and a wrong table.
    #[test]
    fn the_expected_table_covers_exactly_the_whitelist() {
        let names: Vec<&str> = SCREEN_EXPECTED.iter().map(|(n, _, _)| *n).collect();
        assert_eq!(names, SCREEN_SORTS.to_vec());
    }

    #[test]
    fn no_two_sort_names_share_a_column() {
        let mut columns: Vec<&str> = SCREEN_SORTS
            .iter()
            .map(|n| screen_sort_spec(Some(n)).expect("whitelisted").column)
            .collect();
        let total = columns.len();
        columns.sort_unstable();
        columns.dedup();
        assert_eq!(
            columns.len(),
            total,
            "two sort names resolved to the same column: {columns:?}"
        );
    }

    #[test]
    fn a_dash_prefix_ascends_and_the_tiebreak_does_not_follow() {
        let desc = screen_sort_spec(Some("events")).expect("bare");
        assert!(desc.descending);
        let asc = screen_sort_spec(Some("-events")).expect("dash");
        assert!(!asc.descending);
        assert_eq!(desc.column, asc.column);
        assert!(desc.order_by().ends_with(", k.screen ASC"));
        assert!(asc.order_by().ends_with(", k.screen ASC"));
    }

    #[test]
    fn absent_means_the_default_ordering() {
        for raw in [None, Some(""), Some("  ")] {
            let spec = screen_sort_spec(raw).expect("default");
            assert_eq!(spec.column, "views");
            assert!(spec.descending);
            assert_eq!(spec.tiebreak, SCREEN_TIEBREAK);
        }
    }

    /// `total_dwell_ms` is the trap worth pinning: `screen_stats` selects it,
    /// this list does NOT (it divides it into `avg_dwell_ms`), so accepting it
    /// would emit SQL naming a column that does not exist here — a 500.
    #[test]
    fn an_unlisted_column_is_refused() {
        for bad in [
            "views; DROP TABLE analytics_events",
            "total_dwell_ms",
            "last_seen",
            "id",
        ] {
            assert!(
                screen_sort_spec(Some(bad)).is_err(),
                "the screens list must refuse `{bad}`"
            );
        }
    }
}
