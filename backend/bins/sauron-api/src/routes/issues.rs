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
    /// The query language. Wins over `filter`/`q` when non-empty.
    pub query: Option<String>,
    /// `column` or `-column`. Restricted to keyset-backed orderings — see
    /// `search::parse_sort`.
    pub sort: Option<String>,
    /// Opaque token from the previous page's `next_cursor`.
    pub cursor: Option<String>,
    #[serde(default = "default_since_days")]
    pub since_days: i64,
    #[serde(default = "default_limit")]
    pub limit: i64,
    /// Rows to skip — an explicit page JUMP, and nothing else.
    ///
    /// Keyset paging remains the mechanism for STEPPING: `cursor` always wins,
    /// and an `offset` sent alongside one is ignored by the repo layer rather
    /// than combined with it. See `repo::jump_offset` — an offset laid on top
    /// of a keyset predicate skips rows *within* the already-narrowed set,
    /// which is a wrong page rather than an error.
    ///
    /// Offset exists because a page nobody has walked to has no cursor to ask
    /// for, which is what made a numbered pager impossible. Clamped to
    /// `COUNT_CAP`, so the deepest reachable jump costs the same row budget the
    /// count on this very request already spends.
    ///
    /// This supersedes the "accepted and IGNORED" contract from S2c: a
    /// bookmarked `?offset=50` returns the rows it names again.
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

/// The furthest back either list on this resource will look.
///
/// The default `since_days` below is deliberately this same number, and must
/// stay so: a default that exceeds the ceiling is silently narrowed on every
/// unparameterised request, which is precisely the defect that made
/// `analytics::events_list` serve 365 days while its default claimed 3650.
pub(super) const ISSUES_MAX_SINCE_DAYS: i64 = 3650;

fn default_since_days() -> i64 {
    ISSUES_MAX_SINCE_DAYS
} // effectively "all" unless narrowed

// `reject_body_filters` lived here until S2c Task 6 and is now gone, not
// moved: it asked "does this raw `filter=` string name a withheld column",
// and all three routes that used to call it ([`list`], [`events`],
// [`event_stats`]) are bridged onto the query language, where the same
// question is asked of the RESOLVED AST by
// [`super::search::reject_withheld_dimensions`]. The AST form is strictly
// stronger — it sees `filter=tag:eq:k=v` and its `query=k:v` spelling as the
// one predicate they lower to, so it cannot be bypassed by rewriting the URL,
// and it reaches the `Store::JsonRoot` dimensions no `filter=` string could
// name. Its reasoning (why `tag` and `workflow` are withheld from a bare
// `issue:read` caller, and why a withheld predicate is REFUSED rather than
// silently dropped) is preserved in full on that function. Do not reintroduce
// a string-level check beside it.

/// The searched issues list.
///
/// **Answers a [`SearchEnvelope`](super::search::SearchEnvelope), not a bare
/// array, since S2c.** The array had nowhere to put `total`, `next_cursor` or
/// the planner's `clamped` notice, and a header is invisible to a
/// cross-origin dashboard unless it is in `main.rs`' `expose_headers` — which
/// is the definition of silent. `dashboard/src/lib/api/issues.ts` moves with
/// it in Task 7.
///
/// Three input spellings, one execution path: `query=` (the language),
/// `filter=`+`q=` (the pre-language wire format, bridged through
/// `from_legacy`), or nothing. That is what makes an old bookmark and its
/// `query=` rewrite provably select the same rows — there is no second
/// planner for them to disagree in.
pub async fn list(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<ListQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<super::search::SearchEnvelope<Issue>>, ApiError> {
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
    // A search predicate is a read: without `event:read` the free-text term is
    // matched against `title`/`type`/`culprit` only, never the
    // `contexts`/`extra`/`tags` payload scan whose contents this caller's event
    // bodies would arrive with nulled. The reach now travels INTO the lowerer
    // (`IssuesLower.text_reach`) rather than being a separate repo argument —
    // see `symbolicate::text_search_reach` and `TextSearchReach`.
    //
    // **This response still carries no "your search was narrowed" field, even
    // though it is now an envelope with room for one, and that is a decision.**
    // The narrowing carries NO information the caller does not already hold: it
    // is a pure function of their own permission set at the resolved scope — no
    // data dependence, no per-request component — so "was my search narrowed?"
    // is answerable as `!can('event:read', …)` from grants the dashboard has
    // already fetched (`sessionStore.access`, see
    // `dashboard/src/lib/stores/session.svelte.ts`), and answerable BEFORE the
    // search runs rather than after a fruitless one. A flag would restate it.
    // Contrast `clamped` below, which depends on the *query* and genuinely
    // cannot be derived client-side — that one gets a field.
    //
    // The sibling `event_stats` route DOES carry `payload_searched`. Do not
    // read this route's silence as "not narrowed"; derive it from permissions.
    let reach = crate::symbolicate::text_search_reach(&perms);

    let node = super::search::resolve_query(
        q.query.as_deref(),
        &q.filter,
        q.q.as_deref().filter(|s| !s.is_empty()),
        sauron_query::Resource::Issues,
    )?;
    // Must run on the resolved AST, not on the raw `filter=` strings: the
    // `query=` spelling of a tag or workflow probe produces the identical
    // predicate, so a string-level check would be bypassed by rewriting the
    // URL. See `search::reject_withheld_dimensions`.
    //
    // The `env:read` axis is a no-op on THIS route — `R_ISSUES`' `environment`
    // is `Store::Rollup`, which `prepare` rejects as `NotYetSupported` before
    // it could resolve a name — and is passed anyway so the two handlers stay
    // one shape, and so the day Issues gains a real environment column it is
    // already gated.
    super::search::reject_withheld_dimensions(
        &node,
        reach,
        super::search::EnvNameReach::for_perms(&perms),
    )?;

    let prepared = sauron_db::query_plan::prepare::prepare(&node, app_id, Utc::now(), &mut conn)
        .await
        .map_err(super::search::map_plan_error)?;
    let (sort_col, descending) =
        super::search::parse_sort(q.sort.as_deref(), &["last_seen", "first_seen"], "last_seen")?;
    // `parse_sort` already refused anything outside the whitelist, so this
    // cannot be `None` — but the two lists are in different modules and an
    // `expect` here would be a panic when they drift. A 400 naming the column
    // is the same answer `parse_sort` would have given.
    let sort = repo::IssueSort::from_column(&sort_col).ok_or_else(|| {
        ApiError::BadRequest(format!(
            "cannot sort by `{sort_col}`; no keyset index backs it"
        ))
    })?;
    let after = match q.cursor.as_deref() {
        Some(c) => Some(
            // Literal `true`, not a call through `IssueSort` — there is no
            // `IssueSort::is_temporal` to call. Unlike `EventSort`/
            // `OccurrenceSort` (see Task 2's doc comment on those), Issues
            // sorting has not been widened past `last_seen`/`first_seen`,
            // both of which `cursor_ts` below always wraps in
            // `CursorValue::Ts`, so every cursor this route mints or reads is
            // temporal, unconditionally.
            sauron_db::query_plan::cursor::decode(c, &sort_col, true)
                .map_err(|e| ApiError::BadRequest(e.to_string()))?,
        ),
        None => None,
    };

    // `Clamp.field` is the GENERIC name "since" — `prepare` does not know
    // which resource it ran for, so mapping the window onto this resource's
    // real column is the caller's job. On Issues that column is `last_seen`.
    // `resolve_window` owns the tightening rule and, crucially, the matching
    // disclosure: the window reported in `clamped` is the window served.
    let window = super::search::resolve_window(
        "last_seen",
        Utc::now(),
        q.since_days,
        ISSUES_MAX_SINCE_DAYS,
        prepared.clamp,
    );
    let since = window.since;
    let limit = q.limit.clamp(1, 200);
    // Jump-only, and clamped to the same ceiling the count on this request
    // stops at: pages past `COUNT_CAP` are not numberable anyway, so an offset
    // beyond it addresses nothing while costing the planner real rows. Clamped
    // rather than rejected — a stale bookmark deep in a list that has since
    // shrunk should land on the last reachable page, not 400.
    let offset = q.offset.clamp(0, super::search::COUNT_CAP);

    let search = repo::IssueSearch {
        node: &node,
        ctx: &prepared.ctx,
        since,
        sort,
        descending,
        after,
        limit,
        offset,
        text_reach: reach,
    };
    let mut rows = repo::search_issues(&mut conn, &scope, &search)
        .await
        .map_err(super::search::map_plan_error)?;
    let (total, total_is_capped) =
        repo::count_issues(&mut conn, &scope, &search, super::search::COUNT_CAP)
            .await
            .map_err(super::search::map_plan_error)?;

    // `limit + 1` rows were fetched; the surplus one is the has-more probe and
    // must not be served.
    let has_more = rows.len() as i64 > limit;
    rows.truncate(limit as usize);
    // The cursor's timestamp is read through `sort`, not off `last_seen`
    // directly: a cursor carrying `last_seen` while the walk orders by
    // `first_seen` would skip and repeat whole pages.
    //
    // `IssueSort` carries no `cursor_value` (unlike `EventSort`/
    // `OccurrenceSort` — see Task 2's doc comment on those): Issues sorting is
    // deliberately not widened past `last_seen`/`first_seen` here, so there is
    // no nullable-column coalescing rule to centralise yet. `cursor_ts` is
    // wrapped in `CursorValue::Ts` directly; the `key` is new (it is what lets
    // `decode` refuse a cursor minted under the other of the two columns).
    //
    // **This mint must stay ABOVE the Phase 2 `apply_issue_env_stats` call
    // below** — see "Position is load bearing, twice over" on that call for
    // the full reasoning; in short, Phase 2 overwrites `last_seen`/
    // `first_seen` on `rows` with per-environment values, and the keyset walk
    // above ordered on the STORED ones, so a cursor built from an
    // already-overwritten row would aim the next page at an unrelated point
    // in the ordering — skipping rows on some pages and repeating them on
    // others, the exact defect this slice removed. The sibling occurrences
    // route (`events` below) states the same "build the cursor before
    // anything else can rewrite the row" rule at its own mint site, even
    // though nothing there is load-bearing YET. Whoever widens `IssueSort`
    // past `last_seen`/`first_seen` and is tempted to move this block: don't,
    // without moving Phase 2 with it.
    let next_cursor = has_more.then(|| {
        let last = rows.last().expect("has_more implies a row");
        sauron_db::query_plan::cursor::encode(&sauron_db::query_plan::cursor::Cursor {
            key: sort_col.clone(),
            value: sauron_db::query_plan::cursor::CursorValue::Ts(sort.cursor_ts(last)),
            id: last.id,
        })
    });

    // Phase 2 — re-derive the statistics for the environment actually
    // selected. `issues` has no `environment_id`, so its stored `times_seen`/
    // `users_seen`/`first_seen`/`last_seen`/`level`/`culprit`/`title` are
    // APP-WIDE: under an environment selection they describe events this
    // caller is not being shown, and the dashboard auto-selects an
    // environment, so that is most real requests. See
    // `repo::issue_env_stats`.
    //
    // **Position is load bearing, twice over.**
    //
    // 1. AFTER `next_cursor`. The keyset walk filters and orders on `issues`'
    //    STORED `last_seen`/`first_seen`, so the cursor must carry the stored
    //    value. Building it from a row whose timestamp had already been
    //    overwritten with the per-environment one would aim the next page at
    //    an unrelated point in the ordering — skipping rows on some pages and
    //    repeating them on others, the exact defect this slice removed.
    //    `tests/http_search.rs`' `env_scoped_paging_still_reaches_every_row`
    //    is the regression test, and its fixture inverts the two orderings so
    //    a reversed sequence here cannot pass.
    // 2. AFTER `rows.truncate(limit)`. The surplus `limit + 1`th row is only
    //    the has-more probe and is never served, so deriving statistics for it
    //    is wasted work on every full page.
    //
    // Skipped entirely on `EnvFilter::All`: the stored columns already are the
    // app-wide truth there (and are maintained at ingest, so they can see
    // data `sauron-tier` has since exported out of `error_events`), which
    // makes a second query on the commonest path pure cost.
    if !matches!(scope.env, sauron_db::scope::EnvFilter::All) {
        let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
        let stats = repo::issue_env_stats(&mut conn, &scope, &ids, since)
            .await
            .map_err(super::search::map_plan_error)?;
        // Values only — never which rows are returned, so `total` (counted
        // over the same predicate `search_issues` paged) still describes the
        // same set. `apply_issue_env_stats` documents what happens to an id
        // the derivation found no row for.
        repo::apply_issue_env_stats(&mut rows, &stats);
    }

    Ok(Json(super::search::SearchEnvelope {
        data: rows,
        total,
        total_is_capped,
        next_cursor,
        clamped: window.clamped,
    }))
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
    /// The query language. Wins over `filter`/`q` when non-empty.
    ///
    /// **Honoured by [`events`] AND by [`event_stats`], over one shared
    /// `resolve_query` call.** It used to be refused by `event_stats`, and this
    /// comment said so until S2c Task 6 bridged that handler onto the language
    /// too. The refusal existed because honouring `query=` on one side only
    /// would have put a total computed over a wider predicate above a narrowed
    /// list — the dashboard builds ONE `occurrenceParams` object and sends it
    /// to both. Now that both resolve against `Resource::Occurrences` through
    /// the same function, that divergence is not expressible and the refusal
    /// has nothing left to protect. See [`event_stats`] for the full history,
    /// including the vocabulary split it closed at the same time.
    pub query: Option<String>,
    /// `column` or `-column`. Restricted to keyset-backed orderings — see
    /// `search::parse_sort`. Ignored by [`event_stats`], which returns totals:
    /// no ordering changes a count.
    pub sort: Option<String>,
    /// Opaque token from the previous page's `next_cursor`. Ignored by
    /// [`event_stats`] for the same reason as `sort`.
    pub cursor: Option<String>,
    #[serde(default = "default_events_since_days")]
    pub since_days: i64,
    #[serde(default = "default_events_limit")]
    pub limit: i64,
    /// Rows to skip — an explicit page JUMP, and nothing else.
    ///
    /// Keyset paging remains the mechanism for STEPPING: `cursor` always wins,
    /// and an `offset` sent alongside one is ignored by the repo layer rather
    /// than combined with it. See `repo::jump_offset` — an offset laid on top
    /// of a keyset predicate skips rows *within* the already-narrowed set,
    /// which is a wrong page rather than an error.
    ///
    /// Offset exists because a page nobody has walked to has no cursor to ask
    /// for, which is what made a numbered pager impossible. Clamped to
    /// `COUNT_CAP`, so the deepest reachable jump costs the same row budget the
    /// count on this very request already spends.
    ///
    /// This supersedes the "accepted and IGNORED" contract from S2c: a
    /// bookmarked `?offset=50` returns the rows it names again.
    #[serde(default)]
    pub offset: i64,
    // `environment_id` comes from `RawQuery`, not this struct — see `list`'s
    // comment above.
}

fn default_events_limit() -> i64 {
    30
}
fn default_events_since_days() -> i64 {
    ISSUES_MAX_SINCE_DAYS
}

/// One issue's occurrences, searched and keyset-paged.
///
/// **Answers a [`SearchEnvelope`](super::search::SearchEnvelope), not a bare
/// array, since S2c** — same change, and the same reasons, as [`list`]. The
/// array had nowhere to put `total`, `next_cursor` or the planner's `clamped`
/// notice. `dashboard/src/lib/api/issues.ts`' `listIssueEvents` moves with it
/// in Task 7.
///
/// Three input spellings, one execution path: `query=` (the language),
/// `filter=`+`q=` (the pre-language wire format, bridged through
/// `from_legacy`), or nothing.
pub async fn events(
    auth: AuthUser,
    State(state): State<AppState>,
    Path((app_id, issue_id)): Path<(Uuid, Uuid)>,
    Query(q): Query<EventsQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<super::search::SearchEnvelope<ErrorEvent>>, ApiError> {
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
    // reading another app's events by passing a foreign issue_id). The WHERE
    // clauses below carry `app_id` too — this is the first of two layers, not
    // a substitute for the second.
    repo::get_issue(&mut conn, scope.clone(), issue_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    // Same reach as the body gate above, from the same predicate: this route's
    // `?q=` used to ILIKE `contexts`/`extra`/`tags` on rows whose those very
    // columns `gate_event_body` then nulled on the way out — the response
    // withheld the value while the query confirmed it. See
    // `symbolicate::text_search_reach`, and `OccurrencesLower::text`, which is
    // where the narrowing now happens.
    //
    // No explicit "your search was narrowed" field even though this is now an
    // envelope with room for one — see `list`'s comment for the full reasoning.
    // This route has one extra tell on top of it: every row a narrowed caller
    // receives arrives with `contexts`, `extra` and `tags` literally `null`,
    // from the same permission bit that narrowed the search.
    let reach = crate::symbolicate::text_search_reach(&perms);

    let node = super::search::resolve_query(
        q.query.as_deref(),
        &q.filter,
        q.q.as_deref().filter(|s| !s.is_empty()),
        sauron_query::Resource::Occurrences,
    )?;
    // **The refusal that matters most on this route.** `reject_body_filters`
    // below only ever saw the raw `filter=` strings, and `filter=` could name
    // just `tag`/`workflow`. Now that `query=` reaches the same planner,
    // `contexts`, `extra`, `user`, `stack`, `os`/`browser`/`device`/`app` and
    // `sdk` are all addressable as `Store::JsonRoot` dimensions over exactly
    // the columns `symbolicate::strip_event_body` nulls for a caller holding
    // `issue:read` alone — and this route authorizes on `issue:read` alone.
    // `extra.token:~sk_live_` is a sharper oracle than anything `filter=` could
    // spell. `reject_withheld_dimensions` runs on the RESOLVED AST, so it sees
    // the `filter=` and `query=` spellings of the same probe identically.
    //
    // The SECOND axis is live here and, so far, only here: `environment` is a
    // real `Store::Column("environment_id")` on Occurrences, so
    // `query=environment:staging` resolves a NAME against the whole app — and
    // environment names are `env:read` territory, which neither `issue:read`
    // (what authorized this call) nor `event:read` (what `reach` answers for)
    // covers. See `search::reject_withheld_environment`.
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
        &["occurred_at", "distinct_id", "session_id", "device_key"],
        "occurred_at",
    )?;
    // `parse_sort` already refused anything outside the list, so this cannot be
    // None; the expect states that rather than inventing a fallback ordering
    // that would page unstably if the two lists ever drifted apart.
    let sort = sauron_db::repo::OccurrenceSort::from_column(&sort_col)
        .expect("parse_sort whitelist and OccurrenceSort::from_column must agree");
    let after = match q.cursor.as_deref() {
        Some(c) => Some(
            // See `analytics.rs`'s identical comment on its own `decode`
            // call: `sort.is_temporal()` is what stops a cursor whose key
            // names a text column (`distinct_id`/`session_id`/`device_key`)
            // from sneaking a `t:` value tag past the key check alone.
            sauron_db::query_plan::cursor::decode(c, &sort_col, sort.is_temporal())
                .map_err(|e| ApiError::BadRequest(e.to_string()))?,
        ),
        None => None,
    };

    // `Clamp.field` is the GENERIC name "since" — `prepare` does not know which
    // resource it ran for. On THIS resource the window column is
    // `occurred_at`, not Issues' `last_seen`.
    let window = super::search::resolve_window(
        "occurred_at",
        Utc::now(),
        q.since_days,
        ISSUES_MAX_SINCE_DAYS,
        prepared.clamp,
    );
    let since = window.since;
    let limit = q.limit.clamp(1, 100);
    // Jump-only, and clamped to the same ceiling the count on this request
    // stops at: pages past `COUNT_CAP` are not numberable anyway, so an offset
    // beyond it addresses nothing while costing the planner real rows. Clamped
    // rather than rejected — a stale bookmark deep in a list that has since
    // shrunk should land on the last reachable page, not 400.
    let offset = q.offset.clamp(0, super::search::COUNT_CAP);

    let search = repo::OccurrenceSearch {
        node: &node,
        ctx: &prepared.ctx,
        since,
        sort,
        descending,
        after,
        limit,
        offset,
        text_reach: reach,
    };
    let mut events = repo::search_occurrences(&mut conn, &scope, issue_id, &search)
        .await
        .map_err(super::search::map_plan_error)?;
    let (total, total_is_capped) = repo::count_occurrences(
        &mut conn,
        &scope,
        issue_id,
        &search,
        super::search::COUNT_CAP,
    )
    .await
    .map_err(super::search::map_plan_error)?;
    drop(conn); // release before per-event symbolication (checks out its own)

    // `limit + 1` rows were fetched; the surplus one is the has-more probe and
    // must not be served — nor symbolicated.
    let has_more = events.len() as i64 > limit;
    events.truncate(limit as usize);
    // Built BEFORE symbolication and gating. Neither touches `occurred_at` or
    // `id` today, so this is not load bearing the way Task 4b's phase-2
    // ordering is — but building the cursor from the row as the keyset walk
    // ordered it, before anything else rewrites that row, is the rule that made
    // Task 4b's bug possible to have. Keep the two adjacent.
    //
    // The cursor's value is read through `sort.cursor_value`, not off
    // `occurred_at` directly, for the same reason as the Events list: a page
    // sorted by `distinct_id`/`session_id`/`device_key` must mint a cursor
    // carrying that column's value, coalesced exactly as the keyset predicate
    // coalesces it — `cursor_value` is the one place that rule is spelled.
    let next_cursor = has_more.then(|| {
        let last = events.last().expect("has_more implies a row");
        sauron_db::query_plan::cursor::encode(&sauron_db::query_plan::cursor::Cursor {
            key: sort_col.clone(),
            value: sort.cursor_value(last),
            id: last.id,
        })
    });

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

    Ok(Json(super::search::SearchEnvelope {
        data: events,
        total,
        total_is_capped,
        next_cursor,
        clamped: window.clamped,
    }))
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
    /// be able to tell". It lives here and not on `list`/`events` — which since
    /// S2c answer a `SearchEnvelope` that would have room for it — because the
    /// narrowing there is a pure function of the caller's own permissions and
    /// carries no information they do not already hold; see `list`'s comment for
    /// the full reasoning and how a client derives the same fact.
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
    // The sharpest form of the bypass, and the reason this route needed the fix
    // as much as the list did: a COUNT over a withheld column is a cleaner
    // oracle than a page of rows. The list at least caps at 100 rows and could
    // return zero for paging reasons; `events` here is an exact total, so a
    // single request answers "does any occurrence's `extra` contain this
    // substring" with no ambiguity at all.
    let reach = crate::symbolicate::text_search_reach(&perms);

    // **Bridged onto the query language in S2c Task 6**, which is what makes
    // the contract in this route's doc comment true rather than aspirational.
    //
    // Until then this handler ran `parse_filters(…, ERROR_EVENT_FILTERS)` while
    // the list beside it resolved against `Resource::Occurrences`, so the two
    // accepted DIFFERENT vocabularies from the one `occurrenceParams` object
    // `dashboard/src/lib/api/issues.ts` builds for both:
    // `filter=level:eq:error` was a 200 on the list and a 400 here. It also had
    // to refuse `query=` outright, because honouring it on one side only would
    // have put a total computed over a wider predicate above a narrowed list.
    // One `resolve_query` for both routes removes the possibility of either.
    let node = super::search::resolve_query(
        q.query.as_deref(),
        &q.filter,
        q.q.as_deref().filter(|s| !s.is_empty()),
        sauron_query::Resource::Occurrences,
    )?;
    super::search::reject_withheld_dimensions(
        &node,
        reach,
        super::search::EnvNameReach::for_perms(&perms),
    )?;
    let prepared = sauron_db::query_plan::prepare::prepare(&node, app_id, Utc::now(), &mut conn)
        .await
        .map_err(super::search::map_plan_error)?;

    // Identical window arithmetic to `events`, including the clamp, because a
    // total over a different window is the same lie as a total over a different
    // predicate. Sharing `resolve_window` is what makes "identical" structural
    // rather than a promise this comment has to keep on its own. The disclosure
    // is dropped, not omitted by oversight: this route answers a bare stats
    // object with nowhere to put a `clamped`, and the list it captions carries
    // the same notice for the same window.
    let since = super::search::resolve_window(
        "occurred_at",
        Utc::now(),
        q.since_days,
        ISSUES_MAX_SINCE_DAYS,
        prepared.clamp,
    )
    .since;

    // `sort`/`descending`/`after`/`limit`/`offset` are the five fields a count
    // ignores — no ordering and no page boundary changes a total, and
    // `occurrence_stats`/`count_occurrences` both build off
    // `occurrence_search_base` alone, which never reads `search.sort` — but the
    // STRUCT is what is passed, so the fields that do matter cannot be
    // forgotten. `limit` is set to this route's own default, and `sort` to the
    // same default the sibling list uses, rather than either being something
    // meaningless: a value that looked deliberate is easier to read than a
    // magic `0`.
    //
    // `offset` is the one exception to that, and 0 IS the deliberate value:
    // this route captions the whole result set, so a page boundary is not
    // merely ignored here, it would be wrong. A stats object that described
    // "the matching rows from 200 onwards" is not what the caption beside the
    // list claims to be.
    let search = repo::OccurrenceSearch {
        node: &node,
        ctx: &prepared.ctx,
        since,
        sort: repo::OccurrenceSort::OccurredAt,
        descending: true,
        after: None,
        limit: default_events_limit(),
        offset: 0,
        text_reach: reach,
    };
    let counts = repo::occurrence_stats(&mut conn, &scope, issue_id, &search)
        .await
        .map_err(super::search::map_plan_error)?;
    Ok(Json(IssueEventStats {
        counts,
        // Derived from the resolved TREE, not from `q.q`: since the bridge,
        // free text reaches the planner from both spellings, and a bare `boom`
        // term inside `query=` narrows exactly as `?q=boom` does. Reporting
        // `null` ("no search ran") for it would restate the absent/empty/false
        // conflation this field's three states exist to avoid. An empty `?q=`
        // was normalized to `None` above and contributes no `Text` node, so it
        // still reports as no search.
        payload_searched: super::search::has_free_text(&node).then(|| reach.includes_body()),
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
