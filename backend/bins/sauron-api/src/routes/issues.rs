//! The issues API, scoped to an app: list, detail (with occurrences chart +
//! latest event), status updates, and per-issue occurrences.

use axum::extract::{Path, RawQuery, State};
use axum::Json;
use axum_extra::extract::Query;
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use sauron_auth::{authorize_app, perm, AuthUser};
use sauron_db::models::{ErrorEvent, Issue};
use sauron_db::repo;
use sauron_db::repo::SeriesPoint;

use super::db;
use crate::error::ApiError;
use crate::AppState;

#[derive(Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    pub filter: Vec<String>,
    pub q: Option<String>,
    #[serde(default = "default_since_days")]
    pub since_days: i64,
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
    // `environment_id` is deliberately NOT a field here — it is read from the
    // raw query string via `RawQuery` + `scope::authorized_read_scope` instead
    // of this `Query<T>` extractor. See `routes::scope`'s module docs for why:
    // an `Option<String>` field on this struct would go through
    // `axum_extra::extract::Query` (needed for this struct's own `Vec<String>`
    // `filter` field), whose codec silently collapses `?environment_id=` to
    // `None`.
}

fn default_limit() -> i64 {
    50
}
fn default_since_days() -> i64 {
    3650
} // effectively "all" unless narrowed

/// Refuse the filters that are predicates over a WITHHELD column.
///
/// `tag` and `workflow`. `tag` is the free-text `?q=` bypass wearing a
/// different hat. `symbolicate::strip_event_body` nulls `tags` for a caller
/// holding `issue:read` without `event:read`; `filter=tag:eq:k=v` then asks the
/// database whether an occurrence carries that exact tag, and
/// `filter=tag:contains:k=v` gives a per-key ILIKE — a *sharper* oracle than
/// `?q=`, which can only probe the whole `contexts||extra||tags` blob. Both are
/// reachable on `issues::list`, `issues::events` and `issues::event_stats`, all
/// three of which authorize on `issue:read` alone.
///
/// **Refused, not silently dropped.** Dropping the predicate is what
/// [`sauron_db::repo::TextSearchReach`] does to the `q` payload scan, and that
/// is right there: a free-text term is a request to find rows, so matching
/// fewer columns still answers it honestly. A `tag` filter is the opposite — an
/// explicit narrowing — and ignoring it returns MORE rows than were asked for,
/// every one of them presented under a chip claiming they match it. A page that
/// shows non-matching rows beside an active filter is not a smaller answer, it
/// is a wrong one. 403 with the reason is the only non-lying option.
///
/// `workflow` is the second entry, and the reasoning that first left it out was
/// wrong in an instructive way. It argued from `strip_event_body`: `workflow_name`
/// is not in the withheld set, indeed not part of `ErrorEvent`'s wire shape at
/// all, therefore not a leak. But "absent from the event body" is not the same as
/// "public". The endpoints that exist to serve workflow names —
/// `/v1/apps/{id}/workflows` and its `detail`/`runs` siblings — all authorize on
/// `event:read`. So a caller holding `issue:read` alone is not entitled to learn
/// workflow names through *any* route, and `filter=workflow:contains:` handed
/// them an ILIKE to enumerate them one prefix at a time. The correct test is
/// "which permission governs this column", not "does this column appear in the
/// body I already strip".
///
/// Every remaining whitelisted field is genuinely shell:
/// `level`/`status`/`type`/`culprit`/`times_seen`/`users_seen` are issue-level,
/// which is precisely what `issue:read` is the gate for. Keep this in lockstep
/// with `symbolicate::strip_event_body` *and* with the permission on whatever
/// endpoint owns each filterable column.
fn reject_body_filters(
    filters: &[sauron_db::filter::ParsedFilter],
    reach: sauron_db::repo::TextSearchReach,
) -> Result<(), ApiError> {
    if reach.includes_body() {
        return Ok(());
    }
    if filters.iter().any(|f| f.field == "tag") {
        return Err(ApiError::Forbidden(
            "filtering by tag requires event:read: an event's tags are withheld from a caller \
             holding only issue:read, and a filter over them would disclose their contents"
                .into(),
        ));
    }
    if filters.iter().any(|f| f.field == "workflow") {
        return Err(ApiError::Forbidden(
            "filtering by workflow requires event:read: workflow names are served only by the \
             workflows endpoints, which require that permission, so a filter over them would \
             disclose names this caller cannot otherwise read"
                .into(),
        ));
    }
    Ok(())
}

pub async fn list(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<ListQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<Vec<Issue>>, ApiError> {
    let mut conn = db(&state).await?;
    // `_with_perms` rather than the plain `authorized_read_scope`: `issue:read`
    // authorizes the list, and the caller's `event:read` at that same resolved
    // scope decides how far `?q=` may reach. One ancestry+grant resolution
    // answers both — see `detail`'s identical call for the `source:read` case.
    let (scope, perms) = super::scope::authorized_read_scope_with_perms(
        &mut conn,
        auth.user_id,
        app_id,
        perm::ISSUE_READ,
        raw_query.as_deref(),
    )
    .await?;
    let filters = sauron_db::filter::parse_filters(&q.filter, sauron_db::filter::ISSUE_FILTERS)?;
    let search = q.q.as_deref().filter(|s| !s.is_empty());
    // A search predicate is a read: without `event:read` the free-text term is
    // matched against `title`/`type`/`culprit` only, never the
    // `contexts`/`extra`/`tags` payload scan whose contents this caller's event
    // bodies would arrive with nulled. See `symbolicate::text_search_reach`.
    //
    // **This response carries no "your search was narrowed" field, and that is a
    // decision, not an omission.** It cannot: the route answers a bare JSON
    // array (`Vec<Issue>`), so there is no envelope to put one in, and adding
    // one would break every existing client (`dashboard/src/lib/api/issues.ts`'
    // `listIssues` parses `Issue[]` directly). A response header is no better —
    // in both shipped topologies the dashboard is cross-origin, so a header not
    // in `main.rs`' `expose_headers` is invisible to the browser, which is the
    // definition of silent.
    //
    // What makes that acceptable is that the narrowing carries NO information
    // the caller does not already hold. It is a pure function of the caller's
    // own permission set at the resolved scope — no data dependence, no
    // per-request component — so "was my search narrowed?" is answerable as
    // `!can('event:read', …)` from grants the dashboard has already fetched
    // (`sessionStore.access`, see `dashboard/src/lib/stores/session.svelte.ts`),
    // and answerable BEFORE the search runs rather than after a fruitless one.
    // A flag would restate it. Contrast a truncation or tier-miss flag, which
    // depends on the data and genuinely cannot be derived — those get fields.
    //
    // The sibling `event_stats` route DOES carry `payload_searched`, because its
    // response is an object with room for it. Do not read this route's silence
    // as "not narrowed"; derive it from permissions.
    let reach = crate::symbolicate::text_search_reach(&perms);
    reject_body_filters(&filters, reach)?;
    let since = Utc::now() - Duration::days(q.since_days.clamp(1, 3650));
    let limit = q.limit.clamp(1, 200);
    Ok(Json(
        repo::list_issues_with_reach(
            &mut conn,
            scope,
            &filters,
            search,
            reach,
            since,
            limit,
            super::clamp_offset(q.offset),
        )
        .await?,
    ))
}

#[derive(Serialize)]
pub struct IssueDetail {
    #[serde(flatten)]
    pub issue: Issue,
    pub latest_event: Option<ErrorEvent>,
    pub series: Vec<SeriesPoint>,
}

// No bespoke query struct: `detail` takes only `environment_id`, which comes
// from `RawQuery` (see `list`'s comment above), not a `Query<T>` extractor.

pub async fn detail(
    auth: AuthUser,
    State(state): State<AppState>,
    Path((app_id, issue_id)): Path<(Uuid, Uuid)>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<IssueDetail>, ApiError> {
    let mut conn = db(&state).await?;
    // One ancestry+grant resolution authorizes the read, resolves its scope,
    // and answers the second permission question at that same scope.
    let (scope, perms) = super::scope::authorized_read_scope_with_perms(
        &mut conn,
        auth.user_id,
        app_id,
        perm::ISSUE_READ,
        raw_query.as_deref(),
    )
    .await?;
    // Viewing de-obfuscated source code needs source:read; symbol/file/line don't.
    // Evaluated at the resolved environment, not app-wide — an env-scoped
    // caller holds `source:read` (if at all) on their environment.
    let include_source = perms.contains(perm::SOURCE_READ);
    // `issue:read` (which authorized this call) is the COARSE gate; the event
    // BODY additionally needs `event:read`. Asked here only to skip work whose
    // product `gate_event_body` would immediately throw away — the gate itself,
    // not this flag, is what withholds it.
    let include_body = crate::symbolicate::may_read_event_body(&perms);

    let issue = repo::get_issue(&mut conn, scope.clone(), issue_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let mut latest_event = repo::latest_error_event(&mut conn, scope.clone(), issue_id).await?;
    let since = Utc::now() - Duration::days(30);
    let series = repo::issue_occurrence_series(&mut conn, scope, issue_id, since).await?;
    drop(conn); // release the pooled conn; symbolication checks out its own

    if let Some(ev) = latest_event.as_mut() {
        // Symbolication decompresses a blob and parses a source map (or walks
        // DWARF) — pointless for a caller who will receive no frames.
        if include_body {
            crate::symbolicate::symbolicate_event(&state, app_id, ev).await;
            if !include_source {
                crate::symbolicate::strip_source_context(ev);
            }
        }
        // The occurrence stays (timestamp, release, user, device); its payload
        // does not. Keeping the shell is what lets the issue page still render
        // "last seen on 1.2.3" for a coarse-gated caller.
        crate::symbolicate::gate_event_body(&perms, std::slice::from_mut(ev));
    }

    Ok(Json(IssueDetail {
        issue,
        latest_event,
        series,
    }))
}

#[derive(Deserialize)]
pub struct UpdateReq {
    pub status: String,
}

pub async fn update(
    auth: AuthUser,
    State(state): State<AppState>,
    Path((app_id, issue_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<UpdateReq>,
) -> Result<Json<Issue>, ApiError> {
    if !matches!(req.status.as_str(), "unresolved" | "resolved" | "ignored") {
        return Err(ApiError::BadRequest(
            "status must be unresolved, resolved, or ignored".into(),
        ));
    }
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::ISSUE_WRITE).await?;
    let issue = repo::update_issue_status(&mut conn, app_id, issue_id, &req.status)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(issue))
}

#[derive(Deserialize)]
pub struct EventsQuery {
    #[serde(default)]
    pub filter: Vec<String>,
    pub q: Option<String>,
    #[serde(default = "default_events_since_days")]
    pub since_days: i64,
    #[serde(default = "default_events_limit")]
    pub limit: i64,
    // `environment_id` comes from `RawQuery`, not this struct — see `list`'s
    // comment above.
}

fn default_events_limit() -> i64 {
    30
}
fn default_events_since_days() -> i64 {
    3650
}

pub async fn events(
    auth: AuthUser,
    State(state): State<AppState>,
    Path((app_id, issue_id)): Path<(Uuid, Uuid)>,
    Query(q): Query<EventsQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<Vec<ErrorEvent>>, ApiError> {
    let mut conn = db(&state).await?;
    // One ancestry+grant resolution authorizes the read, resolves its scope,
    // and answers the second permission question at that same scope.
    let (scope, perms) = super::scope::authorized_read_scope_with_perms(
        &mut conn,
        auth.user_id,
        app_id,
        perm::ISSUE_READ,
        raw_query.as_deref(),
    )
    .await?;
    // Evaluated at the resolved environment — see `detail`'s comment above.
    let include_source = perms.contains(perm::SOURCE_READ);
    // Bodies need `event:read` on top of the `issue:read` that authorized this
    // call — see `detail`. This route is *nothing but* bodies, so a caller
    // without it gets the occurrence rows (when/who/where) and no payloads,
    // rather than a 403: the occurrences table is issue-level information and
    // `issue:read` is exactly what entitles them to it.
    let include_body = crate::symbolicate::may_read_event_body(&perms);
    // Confirm the issue belongs to this app before returning its events (prevents
    // reading another app's events by passing a foreign issue_id).
    repo::get_issue(&mut conn, scope.clone(), issue_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let filters =
        sauron_db::filter::parse_filters(&q.filter, sauron_db::filter::ERROR_EVENT_FILTERS)?;
    let search = q.q.as_deref().filter(|s| !s.is_empty());
    // Same reach as the body gate above, from the same predicate: this route's
    // `?q=` used to ILIKE `contexts`/`extra`/`tags` on rows whose those very
    // columns `gate_event_body` then nulled on the way out — the response
    // withheld the value while the query confirmed it. See
    // `symbolicate::text_search_reach`.
    //
    // Also unenvelopable, so also no explicit flag — see `list`'s comment for
    // the full reasoning. This route has one extra tell on top of it: every row
    // a narrowed caller receives arrives with `contexts`, `extra` and `tags`
    // literally `null`, from the same permission bit that narrowed the search.
    // "The payload was not searched" is visible in the rows as "the payload is
    // not here".
    let reach = crate::symbolicate::text_search_reach(&perms);
    reject_body_filters(&filters, reach)?;
    let since = Utc::now() - Duration::days(q.since_days.clamp(1, 3650));
    let limit = q.limit.clamp(1, 100);
    let mut events = repo::list_error_events_for_issue_with_reach(
        &mut conn,
        scope,
        issue_id,
        &filters,
        search,
        reach,
        Some(since),
        limit,
    )
    .await?;
    drop(conn); // release before per-event symbolication (checks out its own)
    if include_body {
        // One shared blob-fetcher for the whole page: the artifact lookup is
        // memoized across events instead of repeated per event.
        crate::symbolicate::symbolicate_events(&state, app_id, &mut events).await;
        if !include_source {
            for ev in events.iter_mut() {
                crate::symbolicate::strip_source_context(ev);
            }
        }
    }
    crate::symbolicate::gate_event_body(&perms, &mut events);
    Ok(Json(events))
}

/// Totals for the occurrences the sibling `events` route would list.
///
/// A separate route rather than an envelope around `events` for two reasons:
/// the list response is `Vec<ErrorEvent>` and several SDK-adjacent callers
/// already parse it as a bare array, and the counts skip symbolication
/// entirely, so making them their own request keeps the cheap query cheap.
///
/// Reuses `EventsQuery` so `filter`/`q`/`since_days` are parsed by the exact
/// same code as the list; `limit` is accepted and ignored, since a total that
/// stopped at the page size would be worse than useless.
/// [`event_stats`]' response: the counts, plus the one thing the two sibling
/// list routes have no room to say.
///
/// `#[serde(flatten)]`, so the counts stay top-level keys and the existing
/// `IssueEventStats` type in `dashboard/src/lib/api/issues.ts` keeps parsing
/// unchanged — same shape as [`IssueStats`] below.
#[derive(Serialize)]
pub struct IssueEventStats {
    #[serde(flatten)]
    pub counts: repo::IssueEventStatsRow,
    /// Whether the free-text `q` was matched against the event payload
    /// (`contexts`/`extra`/`tags`) as well as `message`/`exception_*`.
    ///
    /// **Three states, and the third is why this is an `Option`.** `None` (JSON
    /// `null`) means no free-text search ran at all, `Some(false)` that one ran
    /// and the payload columns were excluded because the caller lacks
    /// `event:read`, `Some(true)` that it ran over everything. Collapsing "no
    /// search" into `false` would report a narrowing on every unfiltered request
    /// — the same "absent is not empty is not false" distinction the dashboard's
    /// `environmentsError`/`accessError` flags exist for.
    ///
    /// This is the explicit answer to "a caller whose search was narrowed should
    /// be able to tell". It lives here and not on `list`/`events` because those
    /// answer bare JSON arrays with nowhere to put it; see `list`'s comment for
    /// why that is acceptable there and how a client derives the same fact.
    pub payload_searched: Option<bool>,
}

pub async fn event_stats(
    auth: AuthUser,
    State(state): State<AppState>,
    Path((app_id, issue_id)): Path<(Uuid, Uuid)>,
    Query(q): Query<EventsQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<IssueEventStats>, ApiError> {
    let mut conn = db(&state).await?;
    // `_with_perms`, like the `events` list it describes: these counts are
    // rendered as a description of those rows, so both must be computed over the
    // same predicate — and after D4 the predicate depends on `event:read`.
    let (scope, perms) = super::scope::authorized_read_scope_with_perms(
        &mut conn,
        auth.user_id,
        app_id,
        perm::ISSUE_READ,
        raw_query.as_deref(),
    )
    .await?;
    // Same cross-app guard as `events`: never let a foreign issue_id disclose
    // another app's counts.
    repo::get_issue(&mut conn, scope.clone(), issue_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let filters =
        sauron_db::filter::parse_filters(&q.filter, sauron_db::filter::ERROR_EVENT_FILTERS)?;
    let search = q.q.as_deref().filter(|s| !s.is_empty());
    // The sharpest form of the bypass, and the reason this route needed the fix
    // as much as the list did: a COUNT over a withheld column is a cleaner
    // oracle than a page of rows. The list at least caps at 100 rows and could
    // return zero for paging reasons; `events` here is an exact total, so a
    // single request answers "does any occurrence's `extra` contain this
    // substring" with no ambiguity at all.
    let reach = crate::symbolicate::text_search_reach(&perms);
    reject_body_filters(&filters, reach)?;
    let since = Utc::now() - Duration::days(q.since_days.clamp(1, 3650));
    let counts = repo::error_event_stats_for_issue_with_reach(
        &mut conn,
        scope,
        issue_id,
        &filters,
        search,
        reach,
        Some(since),
    )
    .await?;
    Ok(Json(IssueEventStats {
        counts,
        // `search`, not `q.q`: an empty `?q=` was already normalized to `None`
        // above and did not narrow anything, so it must not be reported as a
        // search that ran.
        payload_searched: search.map(|_| reach.includes_body()),
    }))
}

// ---------------------------------------------------------------------------
// Exceptions dashboard header — status/level breakdown + occurrence series.
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct StatsQuery {
    #[serde(default = "default_stats_days")]
    pub since_days: i64,
    // `environment_id` comes from `RawQuery`, not this struct — see `list`'s
    // comment above.
}

fn default_stats_days() -> i64 {
    30
}

#[derive(Serialize)]
pub struct IssueStats {
    #[serde(flatten)]
    pub counts: repo::IssueStatsRow,
    pub series: Vec<SeriesPoint>,
}

pub async fn stats(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<StatsQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<IssueStats>, ApiError> {
    let mut conn = db(&state).await?;
    let scope = super::scope::authorized_read_scope(
        &mut conn,
        auth.user_id,
        app_id,
        perm::ISSUE_READ,
        raw_query.as_deref(),
    )
    .await?;
    let counts = repo::issue_stats(&mut conn, scope.clone()).await?;
    let since = Utc::now() - Duration::days(q.since_days.clamp(1, 365));
    let series = repo::error_series(&mut conn, scope, since).await?;
    Ok(Json(IssueStats { counts, series }))
}
