//! Sessions API, scoped to an app: a filterable list, and the flagship
//! per-session timeline that merges analytics events, errors, and performance
//! transactions into one chronological stream.

use axum::extract::{Path, Query, RawQuery, State};
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use sauron_auth::{perm, AuthUser};
use sauron_db::models::{AnalyticsEvent, ErrorEvent, Session, Transaction};
use sauron_db::repo;
use sauron_db::repo::SortSpec;

use super::db;
use crate::error::ApiError;
use crate::AppState;

/// `sessions.id` is the table's primary key, so it is unique across any result
/// set this list can produce. Every ordering in [`list`] appends it, which is
/// what makes OFFSET paging total — before this the list had no tiebreaker at
/// all.
const SESSION_TIEBREAK: &str = "id";

/// What `?sort=` accepts on [`list`] — six of the seven columns the Sessions
/// table displays.
///
/// The seventh is **Session** itself, the `session_key` cell, and it is left
/// out on purpose: it renders an opaque key that no user orders by, and the
/// `an_unlisted_column_is_refused` test below pins `session_id` as refused.
/// The design spec listed it as sortable, which was wrong and is corrected —
/// wiring a header for it would 400 the page.
///
/// `last_event_at`, the *old* hard-coded ordering, is deliberately absent for
/// a different reason: it is not a displayed column at all, so a user who
/// sorted away from it could never click their way back.
const SESSION_SORTS: &[&str] = &[
    "started_at",
    "distinct_id",
    "device_key",
    "duration_ms",
    "events_count",
    "errors_count",
];

/// The wire `?sort=` value for the sessions list, resolved to a validated
/// [`SortSpec`].
///
/// A free function, not inlined into [`list`], for the reason
/// `devices::device_sort_spec`'s doc comment gives: an arm naming a
/// valid-but-WRONG column compiles, returns 200, and sorts by the wrong data.
/// Lifting it here is what lets `mod tests` below pin every arm without a
/// database.
///
/// The `&'static str` on the right of each arm is what reaches the SQL; the
/// `String` `parse_sort` returns never does — see [`SortSpec`]'s doc comment.
///
/// Bare column names, unqualified: the query is a single-table read of
/// `sessions` with no joins, so there is nothing to qualify against.
pub(crate) fn session_sort_spec(raw: Option<&str>) -> Result<SortSpec, ApiError> {
    let (column, descending) = super::search::parse_sort(raw, SESSION_SORTS, "started_at")?;
    let (column, nulls_last) = match column.as_str() {
        "distinct_id" => ("distinct_id", true),
        "device_key" => ("device_key", true),
        // There is no stored duration. The table renders
        // `last_event_at - started_at`; ordering by the interval itself sorts
        // identically to the milliseconds the dashboard formats, without a
        // float conversion. Both operands are NOT NULL, so the difference is
        // too.
        "duration_ms" => ("(last_event_at - started_at)", false),
        "events_count" => ("events_count", false),
        "errors_count" => ("errors_count", false),
        // `parse_sort` refused everything else, so this is the default.
        _ => ("started_at", false),
    };
    Ok(SortSpec {
        column,
        descending,
        tiebreak: SESSION_TIEBREAK,
        nulls_last,
    })
}

#[derive(Deserialize)]
pub struct ListQuery {
    pub query: Option<String>,
    #[serde(default)]
    pub filter: Vec<String>,
    pub q: Option<String>,
    /// `time_field` / `from` / `to` / `since_days`. Flattened so the
    /// precedence between them is decided once, in `resolve_time_filter`.
    #[serde(flatten)]
    pub window: super::search::TimeFilterQuery,
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
    pub distinct_id: Option<String>,
    pub device_key: Option<String>,
    /// A column name from [`SESSION_SORTS`], optionally `-`-prefixed to
    /// ascend. Absent means `started_at` descending.
    pub sort: Option<String>,
    // `environment_id` is deliberately NOT a field here — it is read from the
    // raw query string via `RawQuery` + `scope::authorized_read_scope`
    // instead of this `Query<T>` extractor. See `routes::scope`'s module docs
    // for the extractor trap this avoids.
}

/// The columns this list will window on.
///
/// `last_event_at` is surfaced in the UI as **"Last activity"**, not "Ended":
/// `sessions` has no `ended_at` column at all and duration is derived, so
/// "Ended" would name something the data does not hold.
pub const TIME_FIELDS: &[&str] = &["last_event_at", "started_at"];

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
) -> Result<Json<super::search::SearchEnvelope<Session>>, ApiError> {
    let mut conn = db(&state).await?;
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
        sauron_query::Resource::Sessions,
    )?;

    super::search::reject_withheld_dimensions(
        &node,
        repo::TextSearchReach::IncludingBody,
        super::search::EnvNameReach::for_perms(&perms),
    )?;

    let prepared = sauron_db::query_plan::prepare::prepare(&node, app_id, Utc::now(), &mut conn)
        .await
        .map_err(super::search::map_plan_error)?;

    let sort = session_sort_spec(q.sort.as_deref())?;
    let window = super::search::resolve_time_filter(
        // `last_event_at`, NOT `started_at`. This list has always filtered on
        // `last_event_at` (`repo::session_search_base`); the old
        // `resolve_window("started_at", …)` call named the other column in the
        // envelope's `clamped.field` while the predicate used this one.
        // Defaulting to `started_at` here would fix the label by silently
        // changing which sessions an unparameterised request returns, which is
        // the larger of the two changes.
        "last_event_at",
        TIME_FIELDS,
        &q.window,
        Utc::now(),
        // 30 days, unchanged: `default_days` above used to supply it.
        default_days(),
        super::search::MAX_WINDOW_DAYS,
        prepared.clamp,
    )?;
    let limit = q.limit.clamp(1, 200);
    let offset = super::clamp_offset(q.offset);

    let search = repo::SessionSearch {
        node: &node,
        ctx: &prepared.ctx,
        window: repo::TimeWindow {
            column: window.column,
            from: window.from,
            to: window.to,
        },
        sort,
        limit,
        offset,
        distinct_id: q.distinct_id,
        device_key: q.device_key,
    };

    let data = repo::search_sessions(&mut conn, &scope, &search)
        .await
        .map_err(super::search::map_plan_error)?;

    let (total, total_is_capped) =
        repo::count_sessions(&mut conn, &scope, &search, super::search::COUNT_CAP)
            .await
            .map_err(super::search::map_plan_error)?;

    Ok(Json(super::search::SearchEnvelope {
        data,
        total,
        total_is_capped,
        next_cursor: None,
        clamped: window.clamped,
    }))
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

#[cfg(test)]
mod session_sort_tests {
    use super::*;

    /// `(wire name, expected column, expected nulls_last)` for [`list`].
    ///
    /// Written out rather than derived from the name. Only one entry is not
    /// its own wire name: `duration_ms` has no stored column and resolves to
    /// the interval expression.
    const SESSION_EXPECTED: &[(&str, &str, bool)] = &[
        ("started_at", "started_at", false),
        ("distinct_id", "distinct_id", true),
        ("device_key", "device_key", true),
        ("duration_ms", "(last_event_at - started_at)", false),
        ("events_count", "events_count", false),
        ("errors_count", "errors_count", false),
    ];

    /// The finding this exists for: an arm mapping a whitelisted name to a
    /// valid-but-wrong column (`"started_at" => "last_event_at"` — both real
    /// `sessions` columns, and near-identical on most rows) compiles, returns
    /// 200, and silently sorts by the wrong data.
    #[test]
    fn every_whitelisted_name_maps_to_its_own_column() {
        for (name, column, nulls_last) in SESSION_EXPECTED {
            let spec = session_sort_spec(Some(name)).expect("whitelisted");
            assert_eq!(spec.column, *column, "session sort `{name}` mapped wrong");
            assert_eq!(
                spec.nulls_last, *nulls_last,
                "session sort `{name}`: wrong nulls_last"
            );
            assert_eq!(spec.tiebreak, SESSION_TIEBREAK, "session sort `{name}`");
        }
    }

    /// A column added to `SESSION_SORTS` without a matching arm falls through
    /// to `_ =>` and sorts by `started_at` — a 200 and a wrong table.
    #[test]
    fn the_expected_table_covers_exactly_the_whitelist() {
        let names: Vec<&str> = SESSION_EXPECTED.iter().map(|(n, _, _)| *n).collect();
        assert_eq!(names, SESSION_SORTS.to_vec());
    }

    #[test]
    fn no_two_sort_names_share_a_column() {
        let mut columns: Vec<&str> = SESSION_SORTS
            .iter()
            .map(|n| session_sort_spec(Some(n)).expect("whitelisted").column)
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

    /// The two nullable columns must pin NULLS LAST, and the rest must not:
    /// `sessions.distinct_id`/`device_key` are both nullable, and without the
    /// pin an ascending sort would float every anonymous session to the top.
    #[test]
    fn only_the_nullable_columns_pin_nulls_last() {
        for name in ["distinct_id", "device_key"] {
            let spec = session_sort_spec(Some(name)).expect("whitelisted");
            assert!(
                spec.nulls_last,
                "`{name}` is nullable and must pin NULLS LAST"
            );
            assert!(spec.order_by().contains(" NULLS LAST,"));
        }
        for name in ["started_at", "duration_ms", "events_count", "errors_count"] {
            let spec = session_sort_spec(Some(name)).expect("whitelisted");
            assert!(!spec.nulls_last, "`{name}` is NOT NULL — no NULLS LAST");
        }
    }

    #[test]
    fn a_dash_prefix_ascends_and_the_tiebreak_does_not_follow() {
        let desc = session_sort_spec(Some("events_count")).expect("bare");
        assert!(desc.descending);
        let asc = session_sort_spec(Some("-events_count")).expect("dash");
        assert!(!asc.descending);
        assert_eq!(desc.column, asc.column);
        assert!(desc.order_by().ends_with(", id ASC"));
        assert!(asc.order_by().ends_with(", id ASC"));
    }

    /// The default is `started_at`, NOT the `last_event_at` this list used to
    /// order by. Pinned so the change cannot be undone silently — see
    /// `repo::list_sessions`' doc comment for why it moved.
    #[test]
    fn absent_means_started_at_not_the_old_last_event_at() {
        for raw in [None, Some(""), Some("  ")] {
            let spec = session_sort_spec(raw).expect("default");
            assert_eq!(spec.column, "started_at");
            assert!(spec.descending);
            assert_eq!(spec.tiebreak, SESSION_TIEBREAK);
        }
    }

    /// `last_event_at` is the trap worth pinning: it is a real `sessions`
    /// column and the list's own former ordering, so accepting it would look
    /// entirely reasonable — but it is not a column the table displays.
    #[test]
    fn an_unlisted_column_is_refused() {
        for bad in [
            "started_at; DROP TABLE sessions",
            "last_event_at",
            "session_id",
            "ip_address",
        ] {
            assert!(
                session_sort_spec(Some(bad)).is_err(),
                "the sessions list must refuse `{bad}`"
            );
        }
    }
}
