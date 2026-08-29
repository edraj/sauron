//! Workflows API, scoped to an app: a rollup list (one row per workflow
//! name), one name's full detail, its individual runs, and — for the session
//! timeline — the workflow spans within one session.
//!
//! Workflows are entirely optional: an app that never calls `startWorkflow`
//! has no rows in `workflows` and every handler here simply returns empty
//! results (or 404 for a name that was never seen) — no existing route's
//! behaviour changes.
//!
//! Follows `screens.rs`'s template: `authorized_read_scope` does authorization
//! AND scope resolution in one call, sourcing `environment_id` from the raw
//! query string rather than a `Query<T>`-deserialized field (see
//! `routes::scope`'s module docs for why — the "extractor trap" this avoids).
//! [`detail`] is the one exception, and uses the `_with_perms` variant: its
//! `top_issues` is issue metadata rather than workflow signal, so it asks a
//! second permission question. See that function's doc comment.

use axum::extract::{Path, Query, RawQuery, State};
use axum::Json;
use chrono::Utc;
use serde::Deserialize;
use uuid::Uuid;

use sauron_auth::{perm, AuthUser};
use sauron_db::repo;
use sauron_db::repo::SortSpec;

use super::db;
use crate::error::ApiError;
use crate::openapi::ErrorResponse;
use crate::AppState;

fn days30() -> i32 {
    30
}
fn lim50() -> i64 {
    50
}

/// `workflow_list` is `GROUP BY w.name`, so it emits one row per name and the
/// name is unique across the result set by construction — no constraint
/// needed. This is the tiebreak the hard-coded `ORDER BY started DESC, w.name
/// ASC` already used; [`SortSpec`] expresses that same pairing.
const WORKFLOW_TIEBREAK: &str = "w.name";

/// What `?sort=` accepts on [`list`] — the ten columns the Workflows table
/// displays.
const WORKFLOW_SORTS: &[&str] = &[
    "started",
    "name",
    "completed",
    "cancelled",
    "abandoned",
    "completion_rate",
    "median_duration_ms",
    "p95_duration_ms",
    "users",
    "last_seen",
];

/// `completion_rate` has no column: the dashboard computes `completed /
/// started` client-side (`lib/workflows.ts`), and this is that same ratio as
/// an ORDER BY expression over the group.
///
/// Written out rather than added to `WORKFLOW_OUTCOME_SELECT`, because that
/// select list is shared verbatim with `workflow_detail` and decoded into
/// `WorkflowRow` — adding a column there would change the detail response and
/// the wire shape for a sort key nobody reads back.
///
/// No `NULLIF` on the divisor: `COUNT(*)` is at least 1 in any group `GROUP
/// BY` produces, so the ratio can neither divide by zero nor be NULL, which is
/// why `nulls_last` is false for it below.
const COMPLETION_RATE_SQL: &str =
    "(COUNT(*) FILTER (WHERE w.eff = 'completed')::double precision / COUNT(*))";

/// The wire `?sort=` value for the workflows list, resolved to a validated
/// [`SortSpec`].
///
/// A free function for the reason `devices::device_sort_spec`'s doc comment
/// gives — see it first. Every arm's `&'static str` is what reaches the SQL;
/// the `String` `parse_sort` returns never does.
///
/// Two arms deliberately do not echo their wire name: `users` selects as
/// `unique_users`, and `completion_rate` is [`COMPLETION_RATE_SQL`]. The rest
/// are output aliases of the grouped select.
pub(crate) fn workflow_sort_spec(raw: Option<&str>) -> Result<SortSpec, ApiError> {
    let (column, descending) = super::search::parse_sort(raw, WORKFLOW_SORTS, "started")?;
    let (column, nulls_last) = match column.as_str() {
        "name" => ("w.name", false),
        "completed" => ("completed", false),
        "cancelled" => ("cancelled", false),
        "abandoned" => ("abandoned", false),
        "completion_rate" => (COMPLETION_RATE_SQL, false),
        // The only two nullable ones: `percentile_cont` over `w.dur` is NULL
        // for a group whose runs have all not ended yet, which is the intended
        // semantic (see `WorkflowRow::median_duration_ms`). Pinned NULLS LAST
        // so "no finished run" sorts to the bottom in BOTH directions rather
        // than to the top when ascending.
        "median_duration_ms" => ("median_duration_ms", true),
        "p95_duration_ms" => ("p95_duration_ms", true),
        "users" => ("unique_users", false),
        "last_seen" => ("last_seen", false),
        // `parse_sort` refused everything else, so this is the default.
        _ => ("started", false),
    };
    Ok(SortSpec {
        column,
        descending,
        tiebreak: WORKFLOW_TIEBREAK,
        nulls_last,
    })
}

/// The four effective-status values `workflow_runs` accepts as a `status`
/// filter — compared against the *derived* projection (see
/// `repo::workflow_effective_status_sql`'s doc comment), not the raw stored
/// column, which is what makes `abandoned` a filterable value at all even
/// though it never appears as a stored value.
const WORKFLOW_STATUSES: &[&str] = &["active", "completed", "cancelled", "abandoned"];

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct WorkflowListQuery {
    #[serde(default = "days30")]
    pub since_days: i32,
    /// Absolute window bounds, `from` INCLUSIVE and `to` EXCLUSIVE, overriding
    /// `since_days` when either is present. See `analytics::RangeQuery` for why
    /// these are two plain fields rather than a flattened shared struct.
    pub from: Option<chrono::DateTime<chrono::Utc>>,
    pub to: Option<chrono::DateTime<chrono::Utc>>,
    pub search: Option<String>,
    #[serde(default = "lim50")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
    /// A column name from [`WORKFLOW_SORTS`], optionally `-`-prefixed to
    /// ascend. Absent means `started` descending.
    pub sort: Option<String>,
    // `environment_id` is deliberately NOT a field here — it is read from the
    // raw query string via `RawQuery` + `scope::authorized_read_scope`
    // instead of this `Query<T>` extractor. See `routes::scope`'s module docs
    // for the extractor trap this avoids.
}

/// One row per workflow name: started/completed/cancelled/abandoned/active
/// counts, unique users, median/p95 duration and last seen — paginated,
/// optionally substring-filtered by name.
#[utoipa::path(
    get, path = "/v1/apps/{app_id}/workflows", tag = "Analytics",
    summary = "Workflows observed in this app",
    description = "Named multi-step flows, with completion and failure counts.",
    params(("app_id" = Uuid, Path, description = "The app."), WorkflowListQuery, super::search::TimeFilterQuery), security(("bearerAuth" = [])),
    responses((status = 200, description = "Workflows.", body = Vec<repo::WorkflowRow>),
              (status = 400, description = "Malformed query or sort.", body = ErrorResponse), (status = 401, description = "Missing or invalid access token.", body = ErrorResponse), (status = 403, description = "No grant covers this scope.", body = ErrorResponse), (status = 503, description = "Query exceeded its time budget, or a required rollup is missing.", body = ErrorResponse)),
)]
pub async fn list(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<WorkflowListQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<Vec<repo::WorkflowRow>>, ApiError> {
    let mut conn = db(&state).await?;
    let scope = super::scope::authorized_read_scope(
        &mut conn,
        auth.user_id,
        app_id,
        perm::EVENT_READ,
        raw_query.as_deref(),
    )
    .await?;
    // `started_at` for the workflow queries, `occurred_at` for the two signal
    // reads inside `workflow_detail` — one window, whichever column each
    // statement bounds. The field name only labels a disclosure this route has
    // no envelope to carry, so it names the primary one.
    let win = super::search::resolve_range(
        "started_at",
        q.from,
        q.to,
        i64::from(q.since_days),
        Utc::now(),
        365,
    )?;
    let search = q.search.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let sort = workflow_sort_spec(q.sort.as_deref())?;
    let rows = repo::workflow_list(
        &mut conn,
        scope,
        win,
        search,
        q.limit.clamp(1, 200),
        super::clamp_offset(q.offset),
        sort,
    )
    .await?;
    Ok(Json(rows))
}

/// `GET /v1/apps/{app_id}/counts/workflows` — how many rows ``list`` would page.
///
/// Takes ``WorkflowListQuery`` verbatim, so the count and the list are built from ONE
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
    get, path = "/v1/apps/{app_id}/counts/workflows", tag = "Analytics",
    summary = "Count matching workflows",
    params(("app_id" = Uuid, Path, description = "The app."), WorkflowListQuery, super::search::TimeFilterQuery), security(("bearerAuth" = [])),
    responses((status = 200, description = "The count.", body = super::search::CountEnvelope), (status = 401, description = "Missing or invalid access token.", body = ErrorResponse), (status = 403, description = "No grant covers this scope.", body = ErrorResponse), (status = 503, description = "Query exceeded its time budget, or a required rollup is missing.", body = ErrorResponse)),
)]
pub async fn count(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<WorkflowListQuery>,
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
    // `started_at` for the workflow queries, `occurred_at` for the two signal
    // reads inside `workflow_detail` — one window, whichever column each
    // statement bounds. The field name only labels a disclosure this route has
    // no envelope to carry, so it names the primary one.
    let win = super::search::resolve_range(
        "started_at",
        q.from,
        q.to,
        i64::from(q.since_days),
        Utc::now(),
        365,
    )?;
    let search = q.search.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let (total, total_is_capped) =
        repo::count_workflows(&mut conn, scope, win, search, super::search::COUNT_CAP).await?;
    Ok(Json(super::search::CountEnvelope {
        total,
        total_is_capped,
    }))
}

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct WorkflowDetailQuery {
    #[serde(default = "days30")]
    pub since_days: i32,
    /// Absolute window bounds, `from` INCLUSIVE and `to` EXCLUSIVE, overriding
    /// `since_days` when either is present. See `analytics::RangeQuery` for why
    /// these are two plain fields rather than a flattened shared struct.
    pub from: Option<chrono::DateTime<chrono::Utc>>,
    pub to: Option<chrono::DateTime<chrono::Utc>>,
    // `environment_id` is deliberately NOT a field here — see
    // `WorkflowListQuery`'s comment above.
}

/// Full detail for one workflow name: outcome/duration aggregate, duration
/// histogram, top contained events, top contained issues.
///
/// `repo::workflow_detail` returns `Err(diesel::result::Error::NotFound)` when
/// `name` has no matching row in scope within `since_days` (mirroring
/// `screen_stats`'s "vanishes rather than zero-fills" behaviour) — `?` maps
/// that to `ApiError::NotFound`, i.e. a 404, via `ApiError`'s
/// `From<diesel::result::Error>` impl (see `error.rs`), not the 500 an
/// unmapped diesel error would otherwise become.
///
/// `top_issues` is the one part of this response that is not workflow signal:
/// `WorkflowIssue` carries an issue id and title, which `issue:read` — the
/// coarse error gate — is what entitles a caller to. It comes back empty for a
/// caller holding only `event:read`, the same carve-out `analytics::overview`
/// makes for its own `top_issues`.
#[utoipa::path(
    get, path = "/v1/apps/{app_id}/workflows/{name}", tag = "Analytics",
    summary = "One workflow's aggregate shape",
    params(("app_id" = Uuid, Path, description = "The app."), ("name" = String, Path, description = "Workflow name as reported by the SDK."), WorkflowDetailQuery, super::search::TimeFilterQuery),
    security(("bearerAuth" = [])),
    responses((status = 200, description = "The workflow.", body = repo::WorkflowDetail), (status = 401, description = "Missing or invalid access token.", body = ErrorResponse), (status = 403, description = "No grant covers this scope.", body = ErrorResponse),
              (status = 404, description = "No such workflow.", body = ErrorResponse), (status = 503, description = "Query exceeded its time budget, or a required rollup is missing.", body = ErrorResponse)),
)]
pub async fn detail(
    auth: AuthUser,
    State(state): State<AppState>,
    Path((app_id, name)): Path<(Uuid, String)>,
    Query(q): Query<WorkflowDetailQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<repo::WorkflowDetail>, ApiError> {
    let mut conn = db(&state).await?;
    let (scope, perms) = super::scope::authorized_read_scope_with_perms(
        &mut conn,
        auth.user_id,
        app_id,
        perm::EVENT_READ,
        raw_query.as_deref(),
    )
    .await?;
    // `started_at` for the workflow queries, `occurred_at` for the two signal
    // reads inside `workflow_detail` — one window, whichever column each
    // statement bounds. The field name only labels a disclosure this route has
    // no envelope to carry, so it names the primary one.
    let win = super::search::resolve_range(
        "started_at",
        q.from,
        q.to,
        i64::from(q.since_days),
        Utc::now(),
        365,
    )?;
    let mut detail = repo::workflow_detail(&mut conn, scope, &name, win).await?;
    // Cleared rather than skipped: `workflow_detail` runs its four queries as
    // one unit, so there is no query to omit here — unlike `overview`, where
    // `top_issues` is a separate call.
    if !perms.contains(perm::ISSUE_READ) {
        detail.top_issues.clear();
    }
    Ok(Json(detail))
}

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct WorkflowRunsQuery {
    #[serde(default = "days30")]
    pub since_days: i32,
    /// Absolute window bounds, `from` INCLUSIVE and `to` EXCLUSIVE, overriding
    /// `since_days` when either is present. See `analytics::RangeQuery` for why
    /// these are two plain fields rather than a flattened shared struct.
    pub from: Option<chrono::DateTime<chrono::Utc>>,
    pub to: Option<chrono::DateTime<chrono::Utc>>,
    pub status: Option<String>,
    #[serde(default = "lim50")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
    // `environment_id` is deliberately NOT a field here — see
    // `WorkflowListQuery`'s comment above.
}

/// Individual runs of one workflow name, newest first, optionally filtered by
/// effective status.
#[utoipa::path(
    get, path = "/v1/apps/{app_id}/workflows/{name}/runs", tag = "Analytics",
    summary = "Individual runs of one workflow",
    params(("app_id" = Uuid, Path, description = "The app."), ("name" = String, Path, description = "Workflow name."), WorkflowRunsQuery, super::search::TimeFilterQuery),
    security(("bearerAuth" = [])),
    responses((status = 200, description = "Runs.", body = Vec<repo::WorkflowRun>), (status = 401, description = "Missing or invalid access token.", body = ErrorResponse), (status = 403, description = "No grant covers this scope.", body = ErrorResponse), (status = 503, description = "Query exceeded its time budget, or a required rollup is missing.", body = ErrorResponse)),
)]
pub async fn runs(
    auth: AuthUser,
    State(state): State<AppState>,
    Path((app_id, name)): Path<(Uuid, String)>,
    Query(q): Query<WorkflowRunsQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<Vec<repo::WorkflowRun>>, ApiError> {
    if let Some(s) = q.status.as_deref() {
        if !WORKFLOW_STATUSES.contains(&s) {
            return Err(ApiError::BadRequest(format!(
                "status must be one of: {}",
                WORKFLOW_STATUSES.join(", ")
            )));
        }
    }
    let mut conn = db(&state).await?;
    let scope = super::scope::authorized_read_scope(
        &mut conn,
        auth.user_id,
        app_id,
        perm::EVENT_READ,
        raw_query.as_deref(),
    )
    .await?;
    // `started_at` for the workflow queries, `occurred_at` for the two signal
    // reads inside `workflow_detail` — one window, whichever column each
    // statement bounds. The field name only labels a disclosure this route has
    // no envelope to carry, so it names the primary one.
    let win = super::search::resolve_range(
        "started_at",
        q.from,
        q.to,
        i64::from(q.since_days),
        Utc::now(),
        365,
    )?;
    let rows = repo::workflow_runs(
        &mut conn,
        scope,
        &name,
        win,
        q.status.as_deref(),
        q.limit.clamp(1, 200),
        super::clamp_offset(q.offset),
    )
    .await?;
    Ok(Json(rows))
}

// No bespoke query struct: `session_spans` takes no query parameters of its
// own — `environment_id` comes from `RawQuery` (see `WorkflowListQuery`'s
// comment above), not a `Query<T>` extractor.

/// Every workflow span within one session, oldest first — feeds the session
/// timeline lane. Lives here (grouped with the other `repo::workflow_*`
/// consumers) rather than in `sessions.rs`, even though the route sits under
/// `/sessions/{session_id}/workflows`.
#[utoipa::path(
    get, path = "/v1/apps/{app_id}/sessions/{session_id}/workflows", tag = "Analytics",
    summary = "Workflow spans within one session",
    description = "The workflow activity belonging to a single session, for the session timeline view.",
    params(("app_id" = Uuid, Path, description = "The app."), ("session_id" = String, Path, description = "The session id.")),
    security(("bearerAuth" = [])),
    responses((status = 200, description = "Spans.", body = Vec<repo::WorkflowSpan>), (status = 401, description = "Missing or invalid access token.", body = ErrorResponse), (status = 403, description = "No grant covers this scope.", body = ErrorResponse),
              (status = 410, description = "The session's partition has been tiered or dropped.", body = ErrorResponse)),
)]
pub async fn session_spans(
    auth: AuthUser,
    State(state): State<AppState>,
    Path((app_id, session_id)): Path<(Uuid, String)>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<Vec<repo::WorkflowSpan>>, ApiError> {
    let mut conn = db(&state).await?;
    let scope = super::scope::authorized_read_scope(
        &mut conn,
        auth.user_id,
        app_id,
        perm::EVENT_READ,
        raw_query.as_deref(),
    )
    .await?;
    let spans = repo::workflow_spans_for_session(&mut conn, scope, &session_id).await?;
    Ok(Json(spans))
}

#[cfg(test)]
mod workflow_sort_tests {
    use super::*;

    /// `(wire name, expected column, expected nulls_last)` for [`list`].
    ///
    /// Written out rather than derived from the name. Three entries are not
    /// their own wire name: `name` is `w.`-qualified, `users` selects as
    /// `unique_users`, and `completion_rate` is an expression with no alias at
    /// all.
    ///
    /// The `completion_rate` row repeats [`COMPLETION_RATE_SQL`]'s text as a
    /// LITERAL, and the duplication is the entire point — do not "tidy" it
    /// back into a reference to the constant. Referencing it put the same
    /// `&'static str` on both sides of
    /// [`every_whitelisted_name_maps_to_its_own_column`]'s `assert_eq!`, so
    /// that row passed for any value the constant took: editing
    /// `'completed'` to `'cancelled'` inside it silently sorted the
    /// Completion rate column by cancellation rate with the whole backend
    /// suite green. Nothing else could catch it either — `offset_sort.rs`'
    /// workflows ordering test drives `started`/`completed`/`users`, not
    /// `completion_rate`, and `http_env_scoping.rs`' all-columns test asserts
    /// status only, which a different-but-valid aggregate still returns.
    const WORKFLOW_EXPECTED: &[(&str, &str, bool)] = &[
        ("started", "started", false),
        ("name", "w.name", false),
        ("completed", "completed", false),
        ("cancelled", "cancelled", false),
        ("abandoned", "abandoned", false),
        (
            "completion_rate",
            "(COUNT(*) FILTER (WHERE w.eff = 'completed')::double precision / COUNT(*))",
            false,
        ),
        ("median_duration_ms", "median_duration_ms", true),
        ("p95_duration_ms", "p95_duration_ms", true),
        ("users", "unique_users", false),
        ("last_seen", "last_seen", false),
    ];

    /// The finding this exists for: with five sibling `COUNT(*) FILTER`
    /// aliases (`started`/`completed`/`cancelled`/`abandoned`/`active`) an arm
    /// mapping one to another compiles, returns 200, and silently sorts the
    /// table by the wrong outcome.
    #[test]
    fn every_whitelisted_name_maps_to_its_own_column() {
        for (name, column, nulls_last) in WORKFLOW_EXPECTED {
            let spec = workflow_sort_spec(Some(name)).expect("whitelisted");
            assert_eq!(spec.column, *column, "workflow sort `{name}` mapped wrong");
            assert_eq!(
                spec.nulls_last, *nulls_last,
                "workflow sort `{name}`: wrong nulls_last"
            );
            assert_eq!(spec.tiebreak, WORKFLOW_TIEBREAK, "workflow sort `{name}`");
        }
    }

    /// A column added to `WORKFLOW_SORTS` without a matching arm falls through
    /// to `_ =>` and sorts by `started` — a 200 and a wrong table.
    #[test]
    fn the_expected_table_covers_exactly_the_whitelist() {
        let names: Vec<&str> = WORKFLOW_EXPECTED.iter().map(|(n, _, _)| *n).collect();
        assert_eq!(names, WORKFLOW_SORTS.to_vec());
    }

    #[test]
    fn no_two_sort_names_share_a_column() {
        let mut columns: Vec<&str> = WORKFLOW_SORTS
            .iter()
            .map(|n| workflow_sort_spec(Some(n)).expect("whitelisted").column)
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

    /// Only the two `percentile_cont` columns are nullable (a group whose runs
    /// have none ended yet has no duration), and both must pin NULLS LAST so
    /// "no finished run" sorts to the bottom ascending as well as descending.
    ///
    /// BOTH loops re-resolve through [`workflow_sort_spec`], which is the
    /// whole test. The negative loop used to destructure `nulls_last` straight
    /// out of [`WORKFLOW_EXPECTED`] and never call the function at all, so it
    /// asserted `!false` eight times against its own hand-written table:
    /// changing a production arm to `"last_seen" => ("last_seen", true)` left
    /// it green. `sessions.rs`' `only_the_nullable_columns_pin_nulls_last` is
    /// the sibling this now matches.
    #[test]
    fn only_the_duration_percentiles_pin_nulls_last() {
        for name in ["median_duration_ms", "p95_duration_ms"] {
            let spec = workflow_sort_spec(Some(name)).expect("whitelisted");
            assert!(
                spec.nulls_last,
                "`{name}` is nullable and must pin NULLS LAST"
            );
            assert!(spec.order_by().contains(" NULLS LAST,"));
        }
        for (name, _, _) in WORKFLOW_EXPECTED {
            if *name == "median_duration_ms" || *name == "p95_duration_ms" {
                continue;
            }
            let spec = workflow_sort_spec(Some(name)).expect("whitelisted");
            assert!(!spec.nulls_last, "`{name}` is NOT NULL — no NULLS LAST");
            // The rendered clause too, for the reason the percentile half
            // above checks it: `nulls_last` is the flag, `order_by()` is what
            // reaches the SQL.
            assert!(
                !spec.order_by().contains("NULLS LAST"),
                "`{name}` rendered a NULLS LAST it should not have: `{}`",
                spec.order_by()
            );
        }
    }

    #[test]
    fn a_dash_prefix_ascends_and_the_tiebreak_does_not_follow() {
        let desc = workflow_sort_spec(Some("completed")).expect("bare");
        assert!(desc.descending);
        let asc = workflow_sort_spec(Some("-completed")).expect("dash");
        assert!(!asc.descending);
        assert_eq!(desc.column, asc.column);
        assert!(desc.order_by().ends_with(", w.name ASC"));
        assert!(asc.order_by().ends_with(", w.name ASC"));
    }

    #[test]
    fn absent_means_the_default_ordering() {
        for raw in [None, Some(""), Some("  ")] {
            let spec = workflow_sort_spec(raw).expect("default");
            assert_eq!(spec.column, "started");
            assert!(spec.descending);
            assert_eq!(spec.tiebreak, WORKFLOW_TIEBREAK);
        }
    }

    /// `active` is the trap worth pinning: it IS an alias of
    /// `WORKFLOW_OUTCOME_SELECT` and would produce perfectly valid SQL, so
    /// nothing but the whitelist stops it — it is simply not one of the ten
    /// columns the table offers, and Task 4 must not grow a header for it
    /// without adding it here first.
    #[test]
    fn an_unlisted_column_is_refused() {
        for bad in [
            "started; DROP TABLE workflows",
            "active",
            "unique_users",
            "ended_at",
        ] {
            assert!(
                workflow_sort_spec(Some(bad)).is_err(),
                "the workflows list must refuse `{bad}`"
            );
        }
    }
}
