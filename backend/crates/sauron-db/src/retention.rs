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

/// One at-risk person.
#[derive(QueryableByName, Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct ChurnRow {
    #[diesel(sql_type = Text)]
    pub distinct_id: String,
    #[diesel(sql_type = diesel::sql_types::Timestamptz)]
    pub last_seen: chrono::DateTime<chrono::Utc>,
    #[diesel(sql_type = BigInt)]
    pub events_count: i64,
}

/// People whose last signal is older than `silent_days`, newest-silent first.
///
/// Reads `event_user_environments`, not `person_days`: the question is "when
/// did this person last do anything", which that table already answers in one
/// indexed row per person, where `person_days` would need an aggregate over
/// every day they were ever active.
///
/// Keyset-paginated on `last_seen` with a hard limit, like every other list
/// endpoint here.
///
/// # The `::bigint` cast is load-bearing
///
/// Postgres `sum(bigint)` returns NUMERIC, not bigint, and Diesel decodes this
/// column as `BigInt`. Without the cast the endpoint 500s with "Received more
/// than 8 bytes while decoding an i64" — but only once some person has a
/// non-zero `events_count`, because a numeric zero happens to fit in eight
/// bytes and decodes without complaint. A fixture whose counters are all zero
/// therefore passes while the real endpoint fails.
///
/// # Bind numbering
///
/// `sql_query` has no boxed builder, so placeholders are numbered by hand and
/// the environment filter is OPTIONAL — `EnvFilter::All` and `Unattributed`
/// reserve no bind at all. The cursor's index therefore depends on whether the
/// environment one exists, which is precisely the "a caller that assumes an
/// index is always consumed shifts every subsequent bind by one" trap
/// `EnvFilter::sql_fragment` warns about. `next` below is the single place that
/// decides, so the two cannot drift.
pub async fn churn(
    conn: &mut AsyncPgConnection,
    scope: ReadScope,
    silent_days: i64,
    before: Option<chrono::DateTime<chrono::Utc>>,
    limit: i64,
) -> diesel::QueryResult<Vec<ChurnRow>> {
    // See `retention_grid` for why the bind type differs per variant rather
    // than going through `bind_uuids()`.
    let consumes = scope.env.consumes_bind();
    // $1 app, $2 silent_days, $3 limit, then whichever optionals apply.
    let env_e = scope.env.sql_fragment_for("e", 4);
    let next = if consumes { 5 } else { 4 };
    let cursor = if before.is_some() {
        format!(" AND agg.last_seen < ${next}")
    } else {
        String::new()
    };

    let sql = format!(
        "WITH agg AS ( \
             SELECT e.distinct_id, max(e.last_seen) AS last_seen, \
                    COALESCE(sum(e.events_count), 0)::bigint AS events_count \
               FROM event_user_environments e \
              WHERE e.app_id = $1{env_e} \
              GROUP BY e.distinct_id) \
         SELECT agg.distinct_id, agg.last_seen, agg.events_count \
           FROM agg \
          WHERE agg.last_seen < now() - make_interval(days => $2::int){cursor} \
          ORDER BY agg.last_seen DESC \
          LIMIT $3"
    );

    let days = silent_days.clamp(1, 3650) as i32;
    let lim = limit.clamp(1, 500);

    macro_rules! finish {
        ($q:expr) => {
            match before {
                Some(cur) => {
                    $q.bind::<diesel::sql_types::Timestamptz, _>(cur)
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
