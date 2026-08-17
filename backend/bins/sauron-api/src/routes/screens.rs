//! Screen analytics: per-screen views/events/users/exceptions + on-read dwell.
use axum::extract::{Path, Query, RawQuery, State};
use axum::Json;
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use sauron_auth::{perm, AuthUser};
use sauron_db::models::{AnalyticsEvent, ErrorEvent};
use sauron_db::repo;
use sauron_db::repo::SortSpec;

use super::db;
use crate::error::ApiError;
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

#[derive(Deserialize)]
pub struct ScreenListQuery {
    #[serde(default = "days30")]
    pub since_days: i64,
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
    let since = Utc::now() - Duration::days(q.since_days.clamp(1, 365));
    let pattern = match q.q.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(term) => repo::like_contains(term),
        None => "%".to_string(),
    };
    let sort = screen_sort_spec(q.sort.as_deref())?;
    let rows = repo::screen_list(
        &mut conn,
        scope,
        since,
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
    let since = Utc::now() - Duration::days(q.since_days.clamp(1, 365));
    let pattern = match q.q.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(term) => repo::like_contains(term),
        None => "%".to_string(),
    };
    let (total, total_is_capped) =
        repo::count_screens(&mut conn, scope, since, &pattern, super::search::COUNT_CAP).await?;
    Ok(Json(super::search::CountEnvelope {
        total,
        total_is_capped,
    }))
}

#[derive(Deserialize)]
pub struct ScreenDetailQuery {
    pub name: String,
    #[serde(default = "days30")]
    pub since_days: i64,
    // `environment_id` is deliberately NOT a field here — see
    // `ScreenListQuery`'s comment above.
}

#[derive(Serialize)]
pub struct ScreenDetail {
    pub stats: repo::ScreenStats,
    pub recent_events: Vec<AnalyticsEvent>,
    pub recent_exceptions: Vec<ErrorEvent>,
}

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
    // `_with_perms`: `recent_exceptions` below is whole `ErrorEvent` rows, which
    // carry two further permission questions — `perm::ISSUE_READ` for the body
    // at all and `perm::SOURCE_READ` for the de-obfuscated lines inside it. See
    // `sessions::detail` for the same note.
    let (scope, perms) = super::scope::authorized_read_scope_with_perms(
        &mut conn,
        auth.user_id,
        app_id,
        perm::EVENT_READ,
        raw_query.as_deref(),
    )
    .await?;
    let since = Utc::now() - Duration::days(q.since_days.clamp(1, 365));
    let stats = repo::screen_stats(&mut conn, scope.clone(), since, &q.name).await?;
    let recent_events =
        repo::recent_events_for_screen(&mut conn, scope.clone(), &q.name, since, 20).await?;
    let mut recent_exceptions =
        repo::recent_exceptions_for_screen(&mut conn, scope, &q.name, since, 20).await?;
    crate::symbolicate::gate_source_context(&perms, &mut recent_exceptions);
    crate::symbolicate::gate_event_body(&perms, &mut recent_exceptions);
    Ok(Json(ScreenDetail {
        stats,
        recent_events,
        recent_exceptions,
    }))
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
