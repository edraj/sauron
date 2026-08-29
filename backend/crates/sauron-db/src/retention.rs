//! Cohort retention and lifecycle, over [`crate::rollups::person_days`].
//!
//! Two queries, both bounded: the caller caps `cohorts x periods` before
//! reaching here, so neither can walk more of `person_days` than the API's cell
//! budget allows.
//!
//! # The two multi-environment traps
//!
//! `event_user_environments` holds one row per (person, ENVIRONMENT), and
//! `person_days` one row per (person, environment, DAY). On an unscoped
//! (all-environments) request that means a person appears several times, and
//! two distinct things go wrong if it is not handled:
//!
//! * **Cohort assignment** takes `MIN(first_seen)` across the person's rows.
//!   Reading a bare `first_seen` would place the same person in a different
//!   cohort depending on which row the planner reached first — not merely
//!   imprecise, but unstable between runs of the same query.
//! * **Cell counts** are `count(DISTINCT distinct_id)`, never `count(*)`. A
//!   person active in two environments on one day has two rows, and summing
//!   them reports retention above 100%.
//!
//! Under an environment-scoped request both collapse to the single matching row
//! and the distinction costs nothing — which is exactly why it is easy to write
//! the wrong thing and never see it in a scoped test.

use chrono::NaiveDate;
use diesel::sql_types::{Array, Uuid as SqlUuid};
use diesel::sql_types::{BigInt, Date, Integer, Text};
use diesel::QueryableByName;
use diesel_async::{AsyncPgConnection, RunQueryDsl};

use crate::scope::{EnvFilter, ReadScope};

/// Cohort and period bucketing. `Week` is ISO — Monday start, per
/// `date_trunc('week', …)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Granularity {
    Day,
    Week,
}

impl Granularity {
    /// The SQL literal `date_trunc` takes, and the arithmetic step in days.
    fn trunc(self) -> &'static str {
        match self {
            Granularity::Day => "day",
            Granularity::Week => "week",
        }
    }

    pub fn step_days(self) -> i64 {
        match self {
            Granularity::Day => 1,
            Granularity::Week => 7,
        }
    }
}

/// One (cohort, period) cell. `size` repeats per row — the caller folds these
/// into dense per-cohort vectors.
#[derive(QueryableByName, Debug, Clone)]
pub struct CohortRow {
    #[diesel(sql_type = Date)]
    pub cohort: NaiveDate,
    #[diesel(sql_type = BigInt)]
    pub size: i64,
    #[diesel(sql_type = Integer)]
    pub period: i32,
    #[diesel(sql_type = BigInt)]
    pub users: i64,
}

/// Which half of the error split to compute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorSplit {
    /// Everyone.
    All,
    /// Only people who saw an error in PERIOD 0.
    Exposed,
    /// Only people who did not.
    Clean,
}

impl ErrorSplit {
    /// The predicate applied to the cohort set.
    ///
    /// Exposure is measured in period 0 ALONE, and that is what keeps the
    /// comparison honest rather than circular: a user who churns immediately
    /// cannot accumulate later error exposure, so splitting over the whole
    /// window would sort short-lived users into the "clean" bucket by
    /// construction and manufacture the very correlation the chart claims to
    /// find.
    fn predicate(self) -> &'static str {
        match self {
            ErrorSplit::All => "",
            ErrorSplit::Exposed => " AND w.distinct_id IN (SELECT distinct_id FROM exposed)",
            ErrorSplit::Clean => " AND w.distinct_id NOT IN (SELECT distinct_id FROM exposed)",
        }
    }
}

/// The cohort x period grid.
///
/// `from`/`to` bound which COHORTS are returned (half-open on the cohort's
/// start), and `periods` bounds how far each cohort is followed. Rows are
/// emitted only for (cohort, period) pairs that had at least one returner; the
/// caller fills the gaps, because it — not this layer — knows which of those
/// gaps are true zeroes and which are periods that have not elapsed.
pub async fn retention_grid(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    g: Granularity,
    from: NaiveDate,
    to: NaiveDate,
    periods: i32,
    split: ErrorSplit,
) -> diesel::QueryResult<Vec<CohortRow>> {
    // $1 app, $2 from, $3 to, $4 periods, then the env binds.
    let env_e = scope.env.sql_fragment_for("e", 5);
    // Both env fragments reference the SAME bind, so the second must not claim
    // a new index — `sql_fragment_for` is called twice with 5 on purpose.
    let env_d = scope.env.sql_fragment_for("d", 5);
    let trunc = g.trunc();
    let step = g.step_days();

    let sql = format!(
        "WITH cohort AS ( \
             SELECT e.distinct_id, (date_trunc('{trunc}', MIN(e.first_seen)))::date AS c \
               FROM event_user_environments e \
              WHERE e.app_id = $1{env_e} \
              GROUP BY e.distinct_id), \
         windowed AS ( \
             SELECT * FROM cohort WHERE c >= $2 AND c < $3), \
         exposed AS ( \
             SELECT w.distinct_id \
               FROM windowed w JOIN person_days d \
                 ON d.app_id = $1 AND d.distinct_id = w.distinct_id \
                AND d.day >= w.c AND d.day < w.c + {step}{env_d} \
              GROUP BY w.distinct_id \
             HAVING sum(d.errors) > 0), \
         picked AS ( \
             SELECT w.* FROM windowed w WHERE true{pred}), \
         sized AS ( \
             SELECT c, count(*) AS size FROM picked GROUP BY c), \
         ret AS ( \
             SELECT p.c, \
                    (((d.day - p.c) / {step})::int) AS period, \
                    count(DISTINCT d.distinct_id) AS users \
               FROM picked p JOIN person_days d \
                 ON d.app_id = $1 AND d.distinct_id = p.distinct_id \
                AND d.day >= p.c AND d.day < p.c + ($4::int * {step}){env_d} \
              GROUP BY 1, 2) \
         SELECT s.c AS cohort, s.size, r.period, r.users \
           FROM sized s JOIN ret r ON r.c = s.c \
          ORDER BY s.c, r.period",
        pred = split.predicate(),
    );

    let q = diesel::sql_query(sql)
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Date, _>(from)
        .bind::<Date, _>(to)
        .bind::<Integer, _>(periods);

    // `One` renders as `= $n` and `Subset` as `= ANY($n)`, so the BIND TYPE
    // differs per variant. Collapsing both to `bind_uuids()` + `Array` is a
    // 500 at runtime -- `operator does not exist: uuid = uuid[]` -- and only on
    // the environment-scoped path, which an unscoped test never reaches.
    match &scope.env {
        EnvFilter::One(id) => q.bind::<SqlUuid, _>(*id).get_results(conn).await,
        EnvFilter::Subset(ids) => {
            q.bind::<Array<SqlUuid>, _>(ids.clone())
                .get_results(conn)
                .await
        }
        EnvFilter::All | EnvFilter::Unattributed => q.get_results(conn).await,
    }
}

/// One period's lifecycle split.
#[derive(QueryableByName, Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct LifecyclePoint {
    #[diesel(sql_type = Date)]
    pub start: NaiveDate,
    #[diesel(sql_type = BigInt)]
    pub new_users: i64,
    #[diesel(sql_type = BigInt)]
    pub returning_users: i64,
    #[diesel(sql_type = BigInt)]
    pub resurrected_users: i64,
    #[diesel(sql_type = BigInt)]
    pub dormant_users: i64,
}

/// New / returning / resurrected / dormant per period.
///
/// The first three PARTITION the active set — every active person falls in
/// exactly one — which is what makes the stacked bar honest:
///
/// * `new` — this is the person's first period.
/// * `returning` — not new, and active in the previous period.
/// * `resurrected` — not new, and NOT active in the previous period. No
///   "active sometime before" subquery is needed: not being new already means
///   their first period precedes this one.
///
/// `dormant` is counted separately and is deliberately NOT part of that
/// partition — it counts people active in the PREVIOUS period who are silent in
/// this one, and is drawn below the axis.
///
/// # Every bucket in the window is emitted, actives or not
///
/// The output joins off a `generate_series` of buckets rather than off the
/// active set. The distinction only matters for the one period a product team
/// most needs to see: the period in which EVERYBODY went dormant. Derived from
/// the active set, that period has no row at all — its dormant count silently
/// vanishes and the chart shows a gap where it should show the churn cliff.
///
/// The oldest generated bucket is dropped from the output: it is the primer
/// the caller fetches solely so the second-oldest bucket can classify
/// returning-versus-resurrected, and it cannot classify ITSELF (its own
/// predecessor is outside the window, so all its non-new actives would read
/// as resurrected).
///
/// # One window sort, not four disk sorts
///
/// This function has been through three shapes, each measured on 51k persons /
/// 1.4M person-day rows:
///
/// 1. A correlated `EXISTS` in the SELECT list — a per-row SubPlan over the
///    materialized CTE, quadratic. Never run at scale on purpose.
/// 2. Self-LEFT-JOINs on (distinct_id, bucket) — hashable in principle, but
///    the planner's ~3× underestimate of the DISTINCT bucket set pushed both
///    probes to merge joins, each externally sorting the 481k-row CTE to disk:
///    four 16 MB spills, **4.7 s** for the default 14-day window.
/// 3. `LAG`/`LEAD` window functions over ONE sort of the bucket set, from
///    which both "active in the previous bucket" and "silent in the next"
///    fall out as column comparisons. Same answers, one sort.
pub async fn lifecycle(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    g: Granularity,
    from: NaiveDate,
    to: NaiveDate,
) -> diesel::QueryResult<Vec<LifecyclePoint>> {
    let env_p = scope.env.sql_fragment_for("p", 4);
    let env_e = scope.env.sql_fragment_for("e", 4);
    let trunc = g.trunc();
    let step = g.step_days();

    let sql = format!(
        "WITH buckets AS ( \
             SELECT b::date AS b \
               FROM generate_series( \
                      date_trunc('{trunc}', $2::timestamp), \
                      date_trunc('{trunc}', ($3::date - 1)::timestamp), \
                      interval '{step} days') AS b), \
         w AS ( \
             SELECT distinct_id, b, \
                    LAG(b) OVER (PARTITION BY distinct_id ORDER BY b) AS prev_b, \
                    LEAD(b) OVER (PARTITION BY distinct_id ORDER BY b) AS next_b \
               FROM (SELECT DISTINCT p.distinct_id, \
                            (date_trunc('{trunc}', p.day::timestamp))::date AS b \
                       FROM person_days p \
                      WHERE p.app_id = $1 AND p.day >= $2 AND p.day < $3{env_p}) ab), \
         fb AS ( \
             SELECT e.distinct_id, (date_trunc('{trunc}', MIN(e.first_seen)))::date AS b \
               FROM event_user_environments e \
              WHERE e.app_id = $1{env_e} \
              GROUP BY e.distinct_id), \
         active AS ( \
             SELECT w.b, \
                    count(*) FILTER (WHERE fb.b = w.b) AS new_users, \
                    count(*) FILTER (WHERE fb.b <> w.b AND w.prev_b = w.b - {step}) \
                        AS returning_users, \
                    count(*) FILTER (WHERE fb.b <> w.b \
                                       AND (w.prev_b IS NULL OR w.prev_b <> w.b - {step})) \
                        AS resurrected_users \
               FROM w \
               JOIN fb ON fb.distinct_id = w.distinct_id \
              GROUP BY w.b), \
         dorm AS ( \
             SELECT (w.b + {step}) AS b, count(*) AS n \
               FROM w \
              WHERE w.next_b IS NULL OR w.next_b <> w.b + {step} \
              GROUP BY 1) \
         SELECT k.b AS start, \
                COALESCE(a.new_users, 0) AS new_users, \
                COALESCE(a.returning_users, 0) AS returning_users, \
                COALESCE(a.resurrected_users, 0) AS resurrected_users, \
                COALESCE(d.n, 0) AS dormant_users \
           FROM buckets k \
           LEFT JOIN active a ON a.b = k.b \
           LEFT JOIN dorm d ON d.b = k.b \
          WHERE k.b > (SELECT min(b) FROM buckets) \
          ORDER BY k.b"
    );

    let q = diesel::sql_query(sql)
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Date, _>(from)
        .bind::<Date, _>(to);

    match &scope.env {
        EnvFilter::One(id) => q.bind::<SqlUuid, _>(*id).get_results(conn).await,
        EnvFilter::Subset(ids) => {
            q.bind::<Array<SqlUuid>, _>(ids.clone())
                .get_results(conn)
                .await
        }
        EnvFilter::All | EnvFilter::Unattributed => q.get_results(conn).await,
    }
}

/// One at-risk person, with the whole per-person aggregate the risk detail
/// panel shows — first/last seen plus the three counters. All from the same
/// `event_user_environments` scan the original three-column row cost.
#[derive(QueryableByName, Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct ChurnRow {
    #[diesel(sql_type = Text)]
    pub distinct_id: String,
    #[diesel(sql_type = diesel::sql_types::Timestamptz)]
    pub last_seen: chrono::DateTime<chrono::Utc>,
    #[diesel(sql_type = diesel::sql_types::Timestamptz)]
    pub first_seen: chrono::DateTime<chrono::Utc>,
    #[diesel(sql_type = BigInt)]
    pub events_count: i64,
    #[diesel(sql_type = BigInt)]
    pub errors_count: i64,
    #[diesel(sql_type = BigInt)]
    pub sessions_count: i64,
}

/// The columns churn may be ordered by. A closed enum rather than a pass-through
/// string because the column name is interpolated into SQL — the whitelist IS
/// the injection guard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChurnSort {
    LastSeen,
    FirstSeen,
    Events,
    Errors,
    Sessions,
}

impl ChurnSort {
    /// Wire name → column, `None` for anything not whitelisted.
    pub fn parse(name: &str) -> Option<ChurnSort> {
        match name {
            "last_seen" => Some(ChurnSort::LastSeen),
            "first_seen" => Some(ChurnSort::FirstSeen),
            "events" => Some(ChurnSort::Events),
            "errors" => Some(ChurnSort::Errors),
            "sessions" => Some(ChurnSort::Sessions),
            _ => None,
        }
    }

    fn col(self) -> &'static str {
        match self {
            ChurnSort::LastSeen => "last_seen",
            ChurnSort::FirstSeen => "first_seen",
            ChurnSort::Events => "events_count",
            ChurnSort::Errors => "errors_count",
            ChurnSort::Sessions => "sessions_count",
        }
    }

    /// Whether the keyset cursor value for this column is a timestamp.
    pub fn is_time(self) -> bool {
        matches!(self, ChurnSort::LastSeen | ChurnSort::FirstSeen)
    }
}

/// The keyset cursor: the ACTIVE sort column's value on the last row of the
/// previous page, plus that row's `distinct_id` as the tiebreak. Two variants
/// because the two column families bind different SQL types — the same
/// scalar-vs-array lesson as `EnvFilter`, one layer down.
#[derive(Debug, Clone)]
pub enum ChurnCursor {
    Time(chrono::DateTime<chrono::Utc>, String),
    Count(i64, String),
}

/// The silent: last seen before the horizon, ordered by any [`ChurnSort`].
///
/// Pagination is a ROW-VALUE keyset — `(col, distinct_id) < ($v, $id)` — with
/// the tiebreak in the same direction as the sort, so ties on the sort column
/// (common for counters) page deterministically instead of repeating or
/// skipping rows. The caller fetches `limit + 1` and peels the probe row; a
/// bare `len == limit` check would advertise a next page exactly when the
/// result set ends on a page boundary.
pub async fn churn(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    silent_days: i64,
    sort: ChurnSort,
    descending: bool,
    cursor: Option<ChurnCursor>,
    limit: i64,
) -> diesel::QueryResult<Vec<ChurnRow>> {
    // See `retention_grid` for why the bind type differs per variant rather
    // than going through `bind_uuids()`.
    let consumes = scope.env.consumes_bind();
    // $1 app, $2 silent_days, $3 limit, then whichever optionals apply.
    let env_e = scope.env.sql_fragment_for("e", 4);
    let next = if consumes { 5 } else { 4 };
    let col = sort.col();
    let (op, dir) = if descending {
        ("<", "DESC")
    } else {
        (">", "ASC")
    };
    let keyset = if cursor.is_some() {
        format!(
            " AND (agg.{col}, agg.distinct_id) {op} (${next}, ${})",
            next + 1
        )
    } else {
        String::new()
    };

    let sql = format!(
        "WITH agg AS ( \
             SELECT e.distinct_id, \
                    max(e.last_seen) AS last_seen, \
                    min(e.first_seen) AS first_seen, \
                    COALESCE(sum(e.events_count), 0)::bigint AS events_count, \
                    COALESCE(sum(e.errors_count), 0)::bigint AS errors_count, \
                    COALESCE(sum(e.sessions_count), 0)::bigint AS sessions_count \
               FROM event_user_environments e \
              WHERE e.app_id = $1{env_e} \
              GROUP BY e.distinct_id) \
         SELECT agg.distinct_id, agg.last_seen, agg.first_seen, \
                agg.events_count, agg.errors_count, agg.sessions_count \
           FROM agg \
          WHERE agg.last_seen < now() - make_interval(days => $2::int){keyset} \
          ORDER BY agg.{col} {dir}, agg.distinct_id {dir} \
          LIMIT $3"
    );

    let days = silent_days.clamp(1, 3650) as i32;
    let lim = limit.clamp(1, 501);

    macro_rules! finish {
        ($q:expr) => {
            match &cursor {
                Some(ChurnCursor::Time(v, id)) => {
                    $q.bind::<diesel::sql_types::Timestamptz, _>(*v)
                        .bind::<Text, _>(id.clone())
                        .get_results(conn)
                        .await
                }
                Some(ChurnCursor::Count(v, id)) => {
                    $q.bind::<BigInt, _>(*v)
                        .bind::<Text, _>(id.clone())
                        .get_results(conn)
                        .await
                }
                None => $q.get_results(conn).await,
            }
        };
    }

    let q = diesel::sql_query(sql)
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Integer, _>(days)
        .bind::<BigInt, _>(lim);

    match &scope.env {
        EnvFilter::One(id) => finish!(q.bind::<SqlUuid, _>(*id)),
        EnvFilter::Subset(ids) => finish!(q.bind::<Array<SqlUuid>, _>(ids.clone())),
        EnvFilter::All | EnvFilter::Unattributed => finish!(q),
    }
}
