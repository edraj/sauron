//! Devices API, scoped to an app: fleet inventory and a per-device deep-dive
//! (recent sessions, crash history, and its performance profile).

use axum::extract::{Path, Query, RawQuery, State};
use axum::Json;
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use sauron_auth::{perm, AuthUser};
use sauron_db::models::{ErrorEvent, Session};
use sauron_db::repo;
use sauron_db::repo::{DeviceGroupRow, DeviceRow, PerfSummaryRow, SortSpec};

use super::db;
use crate::error::ApiError;
use crate::AppState;

/// Unique per device within an app, which `id` is not for the grouped query —
/// so both lists can be reasoned about against the same key. Every ordering in
/// [`list`] appends it, which is what makes OFFSET paging total.
const DEVICE_TIEBREAK: &str = "d.device_key";

/// The grouping key itself: one row per distinct tuple, so the tuple is unique
/// across [`groups`]' result set in the way `device_key` is across [`list`]'s.
/// Rendered as the trailing ORDER BY terms, all ascending.
const GROUP_TIEBREAK: &str = "d.family, d.model, d.os_name, d.os_version";

/// The ordering [`detail`]'s "recent sessions" panel is served with — its
/// behaviour before Slice 3, restored and pinned.
///
/// Spelled out here rather than taken from `sessions::session_sort_spec(None)`,
/// and that is the entire point: the two consumers of `repo::list_sessions`
/// must order INDEPENDENTLY. The sessions *list* moved its default to
/// `started_at` in Slice 3 because "Started" is one of its sortable headers and
/// `last_event_at` is not a column it displays. This panel is neither — it is a
/// recency strip on a device drill-down, it exposes no `?sort=` of its own, and
/// it was never in Slice 3's scope. Sharing the list's default silently
/// reordered it.
///
/// THE PLAN, measured with `EXPLAIN` over the statement diesel actually emits
/// for this call (captured, not transcribed), against 7,000 sessions. It is
/// NOT uniform, and the difference is the device's own session count:
///
/// ```text
///   device with 50 of 7,000 sessions
///     last_event_at DESC, id ASC   Sort <- Bitmap Index Scan
///                                    sessions_app_device_started_idx  cost  82.9
///     started_at DESC, id ASC      Incremental Sort <- same index     cost  83.2
///
///   device with 5,000 of 7,000 sessions
///     last_event_at DESC, id ASC   Incremental Sort
///                                    <- Index Scan
///                                       sessions_app_last_event_idx   cost   9.5
///     started_at DESC, id ASC      Incremental Sort <- Bitmap Index Scan
///                                    sessions_app_device_started_idx  cost 308.5
/// ```
///
/// So on a quiet device the ordering barely matters (the `device_key`
/// predicate is selective enough that the planner probes
/// `sessions_app_device_started_idx` either way), and on a BUSY device — the
/// one whose panel a human is actually reading — `last_event_at` is served by
/// `sessions_app_last_event_idx` for **32x less** estimated cost. That is the
/// real reason to pin it, and it is narrower than "the list default costs an
/// index", which is what an earlier draft of this comment claimed without
/// having measured this path.
///
/// `id` is the same tiebreak the list uses (`sessions.id` is the primary key),
/// because the reason for a tiebreak — OFFSET paging over tied rows — applies
/// to any ordering, and this panel's `last_event_at` ties as readily as
/// anything else. Adding it is a fix, not a regression: before Slice 3 this
/// query had no tiebreaker at all. It is not free — the bare
/// `last_event_at DESC` needed no sort node at all on the busy device
/// (cost 7.1 vs 9.5), and the tiebreak turns that into an `Incremental Sort`
/// over `Presorted Key: last_event_at`. Correctness is worth 2.4 cost units.
///
/// A `const`, so a reader sees the ordering at the call site and
/// `mod tests` can assert it without a database. `SortSpec`'s fields are all
/// `&'static str`/`bool`, so each use instantiates a fresh value.
const DEVICE_SESSION_SORT: SortSpec = SortSpec {
    column: "last_event_at",
    descending: true,
    tiebreak: DEVICE_SESSION_TIEBREAK,
    nulls_last: false,
};

/// [`DEVICE_SESSION_SORT`]'s tiebreak. Named separately only so the test below
/// can assert against something other than a repeated literal.
const DEVICE_SESSION_TIEBREAK: &str = "id";

/// What `?sort=` accepts on [`list`].
const DEVICE_SORTS: &[&str] = &[
    "last_seen",
    "family",
    "os_name",
    "browser",
    "distinct_id",
    "sessions_count",
    "events_count",
    "errors_count",
];

/// What `?sort=` accepts on [`groups`]. Deliberately NOT [`DEVICE_SORTS`]: a
/// group has no `browser` and no `last_distinct_id` (see `DeviceGroupRow`'s
/// doc comment for why), and has a `device_count` that no single device does.
/// Sharing one list would accept `distinct_id` here and emit SQL naming a
/// column the grouped select does not expose.
const GROUP_SORTS: &[&str] = &[
    "last_seen",
    "family",
    "os_name",
    "device_count",
    "sessions_count",
    "events_count",
    "errors_count",
];

/// The wire `?sort=` value for the flat device list, resolved to a validated
/// [`SortSpec`].
///
/// A free function, not inlined into [`list`], for one reason: a `match` arm
/// that names a valid-but-WRONG column (`"browser" => column: "d.os_name"`)
/// compiles, returns 200, and sorts the table by the wrong data. Nothing about
/// the handler is testable without a database and a spawned server, so the
/// mapping is lifted here where `mod tests` below can assert every arm's
/// `column`/`tiebreak`/`nulls_last` directly.
///
/// The `&'static str` on the right of each arm is what reaches the SQL; the
/// `String` `parse_sort` returns never does — see [`SortSpec`]'s doc comment.
///
/// `d` is the qualifying-devices subquery; a bare name is an output alias of
/// the outer select. The difference is load-bearing under a scoped read, where
/// the outer `last_seen`/`events_count`/`errors_count`/`last_distinct_id` are
/// LATERAL-derived and `d.*` is the app-wide column.
pub(crate) fn device_sort_spec(raw: Option<&str>) -> Result<SortSpec, ApiError> {
    let (column, descending) = super::search::parse_sort(raw, DEVICE_SORTS, "last_seen")?;
    let (column, nulls_last) = match column.as_str() {
        "family" => ("d.family", true),
        "os_name" => ("d.os_name", true),
        "browser" => ("d.browser", true),
        "distinct_id" => ("last_distinct_id", true),
        "sessions_count" => ("sessions_count", false),
        "events_count" => ("events_count", false),
        "errors_count" => ("errors_count", false),
        // `parse_sort` refused everything else, so this is the default.
        _ => ("last_seen", false),
    };
    Ok(SortSpec {
        column,
        descending,
        tiebreak: DEVICE_TIEBREAK,
        nulls_last,
    })
}

/// [`device_sort_spec`] for the grouped list. `d.family`/`d.os_name` are
/// `GROUP BY` columns; everything else is an output alias over an aggregate.
pub(crate) fn group_sort_spec(raw: Option<&str>) -> Result<SortSpec, ApiError> {
    let (column, descending) = super::search::parse_sort(raw, GROUP_SORTS, "last_seen")?;
    let (column, nulls_last) = match column.as_str() {
        "family" => ("d.family", true),
        "os_name" => ("d.os_name", true),
        "device_count" => ("device_count", false),
        "sessions_count" => ("sessions_count", false),
        "events_count" => ("events_count", false),
        "errors_count" => ("errors_count", false),
        _ => ("last_seen", false),
    };
    Ok(SortSpec {
        column,
        descending,
        tiebreak: GROUP_TIEBREAK,
        nulls_last,
    })
}

#[derive(Deserialize)]
pub struct ListQuery {
    /// `time_field` / `from` / `to` / `since_days`, flattened so the
    /// precedence between them is decided once, in `resolve_time_filter`.
    #[serde(flatten)]
    pub window: super::search::TimeFilterQuery,
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
    pub search: Option<String>,
    /// A column name from the list's own whitelist, optionally `-`-prefixed to
    /// ascend. Absent means the list's default. Shared by [`list`] and
    /// [`groups`], which validate it against DIFFERENT whitelists — the two
    /// tables show different columns.
    pub sort: Option<String>,
    // `environment_id` is deliberately NOT a field here — it is read from the
    // raw query string via `RawQuery` + `scope::authorized_read_scope`
    // instead of this `Query<T>` extractor. See `routes::scope`'s module docs
    // for the extractor trap this avoids.
    /// Sentinel for the drill-down. The check is "non-empty", not "present" —
    /// any non-empty value (including `"0"`) turns the filter on; the
    /// dashboard always sends `"1"`. When enabled, all four descriptor fields
    /// below apply, with an ABSENT field meaning SQL NULL. Absent or empty
    /// means the four are ignored entirely and `list` behaves exactly as it
    /// always has.
    ///
    /// The sentinel exists because absent and "filter to NULL" are the same
    /// wire shape otherwise — an omitted query parameter — and the all-NULL
    /// group is a real group that must be drillable.
    pub group: Option<String>,
    pub family: Option<String>,
    pub model: Option<String>,
    pub os_name: Option<String>,
    pub os_version: Option<String>,
}

/// The columns this list will window on. Both indexed on `devices` AND on
/// `device_environments` as of migration 000062.
///
/// **The window decides WHICH DEVICES ARE LISTED**, via the durable `devices`
/// column — it is not a predicate on the value each row displays. Under a
/// scoped read the displayed `first_seen`/`last_seen` are per-environment
/// extrema derived from LATERALs, and a device's per-environment first sighting
/// can postdate its app-level one. That predates this parameter (it is exactly
/// what `since_days` always did), it is the only form an index can serve, and
/// it is the OPPOSITE of what `analytics::PERSON_TIME_FIELDS` means by the same
/// two words. See `repo::device_window_sql` and `repo::person_seen_expr`.
pub const TIME_FIELDS: &[&str] = &["last_seen", "first_seen"];

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
) -> Result<Json<Vec<DeviceRow>>, ApiError> {
    let mut conn = db(&state).await?;
    let scope = super::scope::authorized_read_scope(
        &mut conn,
        auth.user_id,
        app_id,
        perm::EVENT_READ,
        raw_query.as_deref(),
    )
    .await?;
    let window = super::search::resolve_time_filter(
        "last_seen",
        TIME_FIELDS,
        &q.window,
        Utc::now(),
        default_days(),
        super::search::MAX_WINDOW_DAYS,
        // No planner clamp: this list is not query-planner wired.
        None,
    )?;
    let window = repo::TimeWindow {
        column: window.column,
        from: window.from,
        to: window.to,
    };
    let limit = q.limit.clamp(1, 200);
    let search = q.search.as_deref().filter(|s| !s.is_empty());
    // Any non-empty `group` value turns the filter on; the dashboard sends "1".
    let group = q
        .group
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(|_| repo::DeviceGroupKey {
            family: q.family.as_deref(),
            model: q.model.as_deref(),
            os_name: q.os_name.as_deref(),
            os_version: q.os_version.as_deref(),
        });
    let sort = device_sort_spec(q.sort.as_deref())?;
    Ok(Json(
        repo::list_devices(
            &mut conn,
            scope,
            window,
            limit,
            super::clamp_offset(q.offset),
            sort,
            search,
            group,
        )
        .await?,
    ))
}

/// The Devices inventory's default read: one row per
/// `(family, model, os_name, os_version)`. Same scope handling as [`list`] —
/// `environment_id` comes from the raw query string, never from `ListQuery`.
pub async fn groups(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<ListQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<Vec<DeviceGroupRow>>, ApiError> {
    let mut conn = db(&state).await?;
    let scope = super::scope::authorized_read_scope(
        &mut conn,
        auth.user_id,
        app_id,
        perm::EVENT_READ,
        raw_query.as_deref(),
    )
    .await?;
    let window = super::search::resolve_time_filter(
        "last_seen",
        TIME_FIELDS,
        &q.window,
        Utc::now(),
        default_days(),
        super::search::MAX_WINDOW_DAYS,
        // No planner clamp: this list is not query-planner wired.
        None,
    )?;
    let window = repo::TimeWindow {
        column: window.column,
        from: window.from,
        to: window.to,
    };
    let limit = q.limit.clamp(1, 200);
    let search = q.search.as_deref().filter(|s| !s.is_empty());
    let sort = group_sort_spec(q.sort.as_deref())?;
    Ok(Json(
        repo::list_device_groups(
            &mut conn,
            scope,
            window,
            limit,
            super::clamp_offset(q.offset),
            sort,
            search,
        )
        .await?,
    ))
}

/// `GET /v1/apps/{app_id}/counts/devices` — how many rows the Devices
/// inventory would page.
///
/// Answers for whichever of the two lists the same parameters would have hit:
/// `group=` non-empty means the drill-down (`list`, one row per device), absent
/// means the default grouped inventory (`groups`, one row per descriptor
/// tuple). Choosing here from the SAME field `list`/`groups` choose from is
/// what keeps the number attached to the table actually on screen — a count
/// that answered for devices while the table showed groups would be wrong by a
/// large factor and look merely surprising.
///
/// Page fields (`limit`/`offset`/`sort`) are ignored; no page boundary changes
/// a total. `sort` is still resolved for the grouped shape because
/// `count_device_groups` wraps the list's own SQL, which embeds an ORDER BY —
/// it cannot change the count, and passing the caller's own spec keeps the
/// wrapped query identical to the one being counted.
///
/// Same permission and `RawQuery` environment handling as both lists: a count
/// resolved over a wider scope than the list leaks the SIZE of data the caller
/// cannot read.
pub async fn count(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<ListQuery>,
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
    let window = super::search::resolve_time_filter(
        "last_seen",
        TIME_FIELDS,
        &q.window,
        Utc::now(),
        default_days(),
        super::search::MAX_WINDOW_DAYS,
        // No planner clamp: this list is not query-planner wired.
        None,
    )?;
    let window = repo::TimeWindow {
        column: window.column,
        from: window.from,
        to: window.to,
    };
    let search = q.search.as_deref().filter(|s| !s.is_empty());
    let group = q
        .group
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(|_| repo::DeviceGroupKey {
            family: q.family.as_deref(),
            model: q.model.as_deref(),
            os_name: q.os_name.as_deref(),
            os_version: q.os_version.as_deref(),
        });
    let (total, total_is_capped) = match group {
        Some(_) => {
            repo::count_devices(
                &mut conn,
                scope,
                window,
                search,
                group,
                super::search::COUNT_CAP,
            )
            .await?
        }
        None => {
            repo::count_device_groups(
                &mut conn,
                scope,
                window,
                search,
                group_sort_spec(q.sort.as_deref())?,
                super::search::COUNT_CAP,
            )
            .await?
        }
    };
    Ok(Json(super::search::CountEnvelope {
        total,
        total_is_capped,
    }))
}

#[derive(Deserialize)]
pub struct DetailQuery {
    /// The device key (passed as a query param — keys can contain `/` and spaces).
    pub key: String,
    // `environment_id` is deliberately NOT a field here — see `ListQuery`'s
    // comment above.
}

#[derive(Serialize)]
pub struct DeviceDetail {
    /// Environment-scoped, not the raw `devices` row — see `get_device`'s doc
    /// comment. `events_count`/`errors_count` read the durable `devices`
    /// columns under `All` and an environment-scoped LATERAL under `One`/
    /// `Unattributed`, matching `sessions`/`errors`/`perf` below rather than
    /// showing cross-environment, all-time totals above a scoped list.
    pub device: DeviceRow,
    pub sessions: Vec<Session>,
    pub errors: Vec<ErrorEvent>,
    pub perf: Vec<PerfSummaryRow>,
}

pub async fn detail(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(dq): Query<DetailQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<DeviceDetail>, ApiError> {
    let mut conn = db(&state).await?;
    // `_with_perms`: `errors` below is whole `ErrorEvent` rows, which carry two
    // further permission questions — `perm::ISSUE_READ` for the body at all and
    // `perm::SOURCE_READ` for the de-obfuscated lines inside it. See
    // `sessions::detail` for the same note.
    let (scope, perms) = super::scope::authorized_read_scope_with_perms(
        &mut conn,
        auth.user_id,
        app_id,
        perm::EVENT_READ,
        raw_query.as_deref(),
    )
    .await?;
    let device_key = dq.key;

    let device = repo::get_device(&mut conn, scope.clone(), &device_key)
        .await?
        .ok_or(ApiError::NotFound)?;

    let since = Utc::now() - Duration::days(90);
    let sessions = repo::list_sessions(
        &mut conn,
        scope.clone(),
        since,
        50,
        0,
        // Pinned here, NOT `sessions::session_sort_spec(None)` — see
        // [`DEVICE_SESSION_SORT`]. The two consumers of `list_sessions` order
        // independently on purpose.
        DEVICE_SESSION_SORT,
        None,
        Some(&device_key),
    )
    .await?;
    let mut errors = repo::errors_for_device(&mut conn, scope.clone(), &device_key, 50).await?;
    crate::symbolicate::gate_source_context(&perms, &mut errors);
    crate::symbolicate::gate_event_body(&perms, &mut errors);
    let perf = repo::performance_summary(&mut conn, scope, since, None, Some(&device_key)).await?;

    Ok(Json(DeviceDetail {
        device,
        sessions,
        errors,
        perf,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `(wire name, expected column, expected nulls_last)` for [`list`].
    ///
    /// Written out rather than derived from the name, because deriving it
    /// would reproduce whatever rule the implementation used and stop being
    /// an independent check. Two of these are deliberately NOT the wire name:
    /// `distinct_id` selects as `last_distinct_id`, and the three plain
    /// `devices` columns carry the `d.` qualifier while the LATERAL-derived
    /// ones must not.
    const DEVICE_EXPECTED: &[(&str, &str, bool)] = &[
        ("last_seen", "last_seen", false),
        ("family", "d.family", true),
        ("os_name", "d.os_name", true),
        ("browser", "d.browser", true),
        ("distinct_id", "last_distinct_id", true),
        ("sessions_count", "sessions_count", false),
        ("events_count", "events_count", false),
        ("errors_count", "errors_count", false),
    ];

    /// The same for [`groups`].
    const GROUP_EXPECTED: &[(&str, &str, bool)] = &[
        ("last_seen", "last_seen", false),
        ("family", "d.family", true),
        ("os_name", "d.os_name", true),
        ("device_count", "device_count", false),
        ("sessions_count", "sessions_count", false),
        ("events_count", "events_count", false),
        ("errors_count", "errors_count", false),
    ];

    /// The finding this exists for: an arm mapping a whitelisted name to a
    /// valid-but-wrong column (`"browser" => "d.os_name"`) compiles, returns
    /// 200, and silently sorts the table by the wrong data. Only an assertion
    /// on the resolved `column` can see it.
    #[test]
    fn every_whitelisted_name_maps_to_its_own_column() {
        for (name, column, nulls_last) in DEVICE_EXPECTED {
            let spec = device_sort_spec(Some(name)).expect("whitelisted");
            assert_eq!(spec.column, *column, "device sort `{name}` mapped wrong");
            assert_eq!(
                spec.nulls_last, *nulls_last,
                "device sort `{name}`: wrong nulls_last"
            );
            assert_eq!(spec.tiebreak, DEVICE_TIEBREAK, "device sort `{name}`");
        }
        for (name, column, nulls_last) in GROUP_EXPECTED {
            let spec = group_sort_spec(Some(name)).expect("whitelisted");
            assert_eq!(spec.column, *column, "group sort `{name}` mapped wrong");
            assert_eq!(
                spec.nulls_last, *nulls_last,
                "group sort `{name}`: wrong nulls_last"
            );
            assert_eq!(spec.tiebreak, GROUP_TIEBREAK, "group sort `{name}`");
        }
    }

    /// The table above and the whitelist must not drift apart: a column added
    /// to `DEVICE_SORTS`/`GROUP_SORTS` without a matching `match` arm silently
    /// falls through to the `_ =>` default and sorts by `last_seen` instead,
    /// which is a 200 and a wrong table. The expected tables are the same
    /// length and the same names, so this catches an addition on either side.
    #[test]
    fn the_expected_tables_cover_exactly_the_whitelists() {
        let device_names: Vec<&str> = DEVICE_EXPECTED.iter().map(|(n, _, _)| *n).collect();
        assert_eq!(device_names, DEVICE_SORTS.to_vec());
        let group_names: Vec<&str> = GROUP_EXPECTED.iter().map(|(n, _, _)| *n).collect();
        assert_eq!(group_names, GROUP_SORTS.to_vec());
    }

    /// No two names may resolve to the same column. This is the invariant a
    /// copy-paste mis-map violates first — duplicating the `os_name` arm and
    /// relabelling it `browser` leaves both pointing at `d.os_name` — and it
    /// holds without restating the mapping.
    #[test]
    fn no_two_sort_names_share_a_column() {
        for (names, resolve) in [
            (
                DEVICE_SORTS,
                &device_sort_spec as &dyn Fn(Option<&str>) -> Result<SortSpec, ApiError>,
            ),
            (GROUP_SORTS, &group_sort_spec),
        ] {
            let mut columns: Vec<&str> = names
                .iter()
                .map(|n| resolve(Some(n)).expect("whitelisted").column)
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
    }

    /// Direction is `parse_sort`'s: bare descends, `-` ascends. The tiebreak
    /// never reverses with it — that is what makes the two directions exact
    /// reverses of each other rather than merely opposite on the sort column.
    #[test]
    fn a_dash_prefix_ascends_and_the_tiebreak_does_not_follow() {
        let desc = device_sort_spec(Some("events_count")).expect("bare");
        assert!(desc.descending);
        let asc = device_sort_spec(Some("-events_count")).expect("dash");
        assert!(!asc.descending);
        assert_eq!(desc.column, asc.column);
        assert!(desc.order_by().ends_with(", d.device_key ASC"));
        assert!(asc.order_by().ends_with(", d.device_key ASC"));
    }

    /// [`GROUP_TIEBREAK`]'s value, pinned against a LITERAL rather than
    /// against the constant itself.
    ///
    /// Every other tiebreak in this slice already had a literal backstop of
    /// this shape somewhere in its module — `", d.device_key ASC"` above,
    /// `", eu.distinct_id ASC"`, `", k.screen ASC"`, `", id ASC"`,
    /// `", w.name ASC"`. The grouped one had none: its only assertion was the
    /// `assert_eq!(spec.tiebreak, GROUP_TIEBREAK)` in
    /// [`every_whitelisted_name_maps_to_its_own_column`], which compares the
    /// resolved value against the very constant that produced it and therefore
    /// passes for ANY value the constant takes.
    ///
    /// What that cost, measured rather than argued: shortening
    /// [`GROUP_TIEBREAK`] to `"d.family"` leaves the grouped ORDER BY
    /// non-unique — two groups sharing a family then tie completely — so
    /// OFFSET paging over the grouped list can serve one group twice and never
    /// serve another, and the entire backend suite stayed green. That is
    /// precisely the defect this slice exists to prevent, and it was
    /// unprotected on one of the six lists.
    ///
    /// This is the value check; `sauron-db`'s `offset_sort.rs`
    /// `device_groups_page_stably_when_the_family_ties` is the behaviour proof
    /// that this exact four-column key is what makes grouped paging total. Both
    /// are needed: that test pages through a hand-written mirror of this spec
    /// (`sauron-db` cannot depend on the API binary), so it cannot see this
    /// constant, and this assertion cannot see a query plan.
    #[test]
    fn the_grouped_tiebreak_is_the_whole_four_column_key() {
        const RENDERED: &str = ", d.family, d.model, d.os_name, d.os_version ASC";
        for name in GROUP_SORTS {
            let spec = group_sort_spec(Some(name)).expect("whitelisted");
            let rendered = spec.order_by();
            assert!(
                rendered.ends_with(RENDERED),
                "group sort `{name}`: the grouped ORDER BY must end with the \
                 whole four-column grouping key `{RENDERED}` — anything \
                 shorter is not unique across groups and lets OFFSET paging \
                 duplicate and skip rows — got `{rendered}`"
            );
        }
        // Ascending too: the tiebreak must not reverse with the direction, or
        // the two directions stop being exact row-for-row reverses.
        let asc = group_sort_spec(Some("-device_count"))
            .expect("dash")
            .order_by();
        assert!(asc.ends_with(RENDERED), "ascending: got `{asc}`");
    }

    /// Absent and empty both mean the list's default, and the default is a
    /// real arm of the whitelist rather than an unvalidated fallthrough.
    #[test]
    fn absent_means_the_default_ordering() {
        for raw in [None, Some(""), Some("  ")] {
            let spec = device_sort_spec(raw).expect("default");
            assert_eq!(spec.column, "last_seen");
            assert!(spec.descending);
            assert_eq!(spec.tiebreak, DEVICE_TIEBREAK);
        }
        assert_eq!(group_sort_spec(None).expect("default").column, "last_seen");
    }

    /// [`detail`]'s sessions panel must keep ordering by `last_event_at`, and
    /// must NOT track the sessions list's default.
    ///
    /// Slice 3 moved that default from `last_event_at` to `started_at` for the
    /// LIST, which is correct there and wrong here (see
    /// [`DEVICE_SESSION_SORT`]). Because both consumers call the same repo
    /// function, nothing but this assertion stops a future edit from
    /// collapsing them back together — and the symptom would be a silently
    /// reordered panel, not a failure.
    #[test]
    fn the_device_detail_session_panel_pins_last_event_at() {
        assert_eq!(DEVICE_SESSION_SORT.column, "last_event_at");
        assert_eq!(DEVICE_SESSION_SORT.tiebreak, DEVICE_SESSION_TIEBREAK);
        // The rendered clause, not `assert!(…descending)`/`assert!(!…
        // nulls_last)`: on a `const` those two are constant assertions that
        // fold away and prove nothing at runtime (clippy says so). This string
        // subsumes both — `DESC` is `descending`, and no ` NULLS LAST` is
        // `!nulls_last` — and it is what actually reaches the SQL.
        assert_eq!(
            DEVICE_SESSION_SORT.order_by(),
            "last_event_at DESC, id ASC",
            "the panel's ordering is its pre-Slice-3 one, plus the tiebreak it \
             never had"
        );

        // The independence itself, not just the value: these two must differ,
        // and `last_event_at` is deliberately absent from the list's whitelist.
        let list_default = super::super::sessions::session_sort_spec(None)
            .expect("the sessions list default is always resolvable");
        assert_eq!(list_default.column, "started_at");
        assert_ne!(
            DEVICE_SESSION_SORT.column, list_default.column,
            "the drill-down panel must not inherit the sessions list's default"
        );
        assert!(
            super::super::sessions::session_sort_spec(Some("last_event_at")).is_err(),
            "`last_event_at` is this panel's ordering, not a wire-selectable \
             column of the sessions list"
        );
    }

    /// Each list refuses the other's exclusive columns, and refuses junk.
    #[test]
    fn an_unlisted_column_is_refused_by_both_lists() {
        for bad in [
            "last_seen; DROP TABLE devices",
            "id",
            "arch",
            "device_count",
        ] {
            assert!(
                device_sort_spec(Some(bad)).is_err(),
                "device list must refuse `{bad}`"
            );
        }
        for bad in ["last_seen; DROP TABLE devices", "browser", "distinct_id"] {
            assert!(
                group_sort_spec(Some(bad)).is_err(),
                "group list must refuse `{bad}`"
            );
        }
    }
}
