//! Rollup-backed read functions.
//!
//! Each mirrors a legacy `repo` aggregate exactly in OUTPUT SHAPE (it returns
//! the same struct) while reading the migration-71 rollup tables instead of
//! raw events, so cost is proportional to (keys × days), never to event
//! volume. The legacy functions branch here behind `rollups::is_ready` — the
//! device-groups gate pattern — and keep their raw path as the fallback.
//!
//! # Disclosed semantics (docs/approximate-analytics.md)
//!
//! * Distinct users come from merged HLL sketches (±~2%).
//! * Percentiles come from merged √2 log-bucket histograms.
//! * Windows match whole buckets: a mid-day `from` includes its full UTC day
//!   (full hour for performance), so edge days differ slightly from the raw
//!   queries' point-in-time bounds.
//! * DAU/WAU/MAU are calendar-day windows (today, last 7, last 30 UTC days),
//!   not rolling 24h/7d/30d instants.
//! * Journeys are day-scoped: first ≤10 events per user per UTC day, summed
//!   across the window, per environment.

use chrono::{DateTime, Duration, DurationRound, NaiveDate, TimeZone, Utc};
use diesel::sql_types::{
    Array, BigInt, Bytea, Date, Double, Nullable, Text, Timestamptz, Uuid as SqlUuid,
};
use diesel::QueryResult;
use diesel::QueryableByName;
use diesel_async::{AsyncPgConnection, RunQueryDsl};

use crate::repo::{
    self, DayCountRow, EventCount, HistoBucket, JourneyLink, JourneyNode, OverviewTotals,
    PerfSeriesPoint, PerfSummaryRow, SeriesAvgPoint, SeriesPoint, SessionStats, SortSpec,
    UserSeriesPoint, UserStats, DURATION_BUCKETS,
};
use crate::scope::{Range, ReadScope};
use crate::sketch::{Hll, LatencyHistogram};

/// Inclusive day bounds covering the range, whole-bucket semantics.
fn day_bounds(range: &Range) -> (NaiveDate, NaiveDate) {
    let lo = range.from.date_naive();
    let hi = range
        .to
        .map(|t| (t - Duration::microseconds(1)).date_naive())
        .unwrap_or_else(|| NaiveDate::from_ymd_opt(9999, 1, 1).expect("valid"));
    (lo, hi.max(lo))
}

fn far_future() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(9999, 1, 1, 0, 0, 0)
        .single()
        .expect("valid")
}

fn day_bucket(day: NaiveDate) -> DateTime<Utc> {
    day.and_hms_opt(0, 0, 0).expect("valid").and_utc()
}

fn merged_hll(blobs: &[Option<Vec<u8>>]) -> Hll {
    let mut h = Hll::new();
    for b in blobs.iter().flatten() {
        if let Some(other) = Hll::from_bytes(b) {
            h.merge(&other);
        }
    }
    h
}

fn merged_hist(blobs: &[Vec<u8>]) -> LatencyHistogram {
    let mut h = LatencyHistogram::new();
    for b in blobs {
        h.merge_counts(&LatencyHistogram::counts_from_bytes(b));
    }
    h
}

fn ratio(num: f64, den: i64) -> f64 {
    if den > 0 {
        num / den as f64
    } else {
        0.0
    }
}

// ---------------------------------------------------------------------------
// Screens
// ---------------------------------------------------------------------------

#[derive(QueryableByName)]
struct ScreenAggRow {
    #[diesel(sql_type = Text)]
    screen: String,
    #[diesel(sql_type = BigInt)]
    views: i64,
    #[diesel(sql_type = BigInt)]
    events: i64,
    #[diesel(sql_type = BigInt)]
    exceptions: i64,
    #[diesel(sql_type = Double)]
    dwell: f64,
    #[diesel(sql_type = Array<Nullable<Bytea>>)]
    hlls: Vec<Option<Vec<u8>>>,
}

async fn screen_rows(
    conn: &mut AsyncPgConnection,
    scope: &ReadScope,
    range: Range,
    q_pattern: &str,
) -> QueryResult<Vec<ScreenAggRow>> {
    let (lo, hi) = day_bounds(&range);
    let env_sql = scope.env.sql_fragment(5);
    let q = format!(
        "SELECT screen, sum(views)::bigint AS views, sum(events)::bigint AS events, \
                sum(exceptions)::bigint AS exceptions, sum(dwell_ms_sum)::float8 AS dwell, \
                array_agg(users_hll) AS hlls \
         FROM screen_stats_daily \
         WHERE app_id=$1 AND day>=$2 AND day<=$3 AND ($4 = '%' OR screen ILIKE $4){env_sql} \
         GROUP BY screen"
    );
    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Date, _>(lo)
        .bind::<Date, _>(hi)
        .bind::<Text, _>(q_pattern.to_string());
    stmt = crate::bind_env!(stmt, &scope.env);
    stmt.get_results(conn).await
}

/// Rollup twin of `repo::screen_list`. Sorting and paging happen in Rust: the
/// `users` sort key only exists after the per-screen sketch merge, and the
/// whole result is at most a few hundred screens.
pub async fn screens(
    conn: &mut AsyncPgConnection,
    scope: &ReadScope,
    range: Range,
    q_pattern: &str,
    limit: i64,
    offset: i64,
    sort: &SortSpec,
) -> QueryResult<Vec<repo::ScreenRow>> {
    let rows = screen_rows(conn, scope, range, q_pattern).await?;
    let mut out: Vec<repo::ScreenRow> = rows
        .into_iter()
        .map(|r| repo::ScreenRow {
            avg_dwell_ms: repo::avg_dwell(r.dwell, r.views),
            users: merged_hll(&r.hlls).estimate(),
            screen: r.screen,
            views: r.views,
            events: r.events,
            exceptions: r.exceptions,
        })
        .collect();
    let key = |r: &repo::ScreenRow| -> (f64, i64) {
        match sort.column {
            "views" => (r.views as f64, 0),
            "events" => (r.events as f64, 0),
            "exceptions" => (r.exceptions as f64, 0),
            "users" => (r.users as f64, 0),
            "avg_dwell_ms" => (r.avg_dwell_ms, 0),
            _ => (r.views as f64, 0),
        }
    };
    out.sort_by(|a, b| {
        let (ka, kb) = (key(a), key(b));
        let ord = ka.0.partial_cmp(&kb.0).unwrap_or(std::cmp::Ordering::Equal);
        let ord = if sort.descending { ord.reverse() } else { ord };
        ord.then_with(|| a.screen.cmp(&b.screen))
    });
    Ok(out
        .into_iter()
        .skip(offset.max(0) as usize)
        .take(limit.max(0) as usize)
        .collect())
}

#[derive(QueryableByName)]
struct CountRow {
    #[diesel(sql_type = BigInt)]
    n: i64,
}

/// Rollup twin of `repo::count_screens`, same `(count, capped)` contract.
pub async fn count_screens(
    conn: &mut AsyncPgConnection,
    scope: &ReadScope,
    range: Range,
    q_pattern: &str,
    cap: i64,
) -> QueryResult<(i64, bool)> {
    let (lo, hi) = day_bounds(&range);
    let env_sql = scope.env.sql_fragment(5);
    let q = format!(
        "SELECT count(DISTINCT screen)::bigint AS n FROM screen_stats_daily \
         WHERE app_id=$1 AND day>=$2 AND day<=$3 AND ($4 = '%' OR screen ILIKE $4){env_sql}"
    );
    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Date, _>(lo)
        .bind::<Date, _>(hi)
        .bind::<Text, _>(q_pattern.to_string());
    stmt = crate::bind_env!(stmt, &scope.env);
    let r: CountRow = stmt.get_result(conn).await?;
    Ok((r.n.min(cap), r.n > cap))
}

// ---------------------------------------------------------------------------
// Journeys (day-scoped semantics — see module docs)
// ---------------------------------------------------------------------------

pub async fn journey(
    conn: &mut AsyncPgConnection,
    scope: &ReadScope,
    range: Range,
    depth: i64,
) -> QueryResult<(Vec<JourneyNode>, Vec<JourneyLink>)> {
    let (lo, hi) = day_bounds(&range);
    let env_sql = scope.env.sql_fragment(5);
    let nodes_q = format!(
        "SELECT step::bigint AS step, name AS event, sum(count)::bigint AS count \
         FROM journey_nodes_daily \
         WHERE app_id=$1 AND day>=$2 AND day<=$3 AND step < $4{env_sql} \
         GROUP BY step, name ORDER BY step, count DESC LIMIT 500"
    );
    let mut stmt = diesel::sql_query(nodes_q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Date, _>(lo)
        .bind::<Date, _>(hi)
        .bind::<BigInt, _>(depth);
    stmt = crate::bind_env!(stmt, &scope.env);
    let nodes: Vec<JourneyNode> = stmt.get_results(conn).await?;

    let links_q = format!(
        "SELECT step::bigint AS from_step, from_name AS from_event, to_name AS to_event, \
                sum(count)::bigint AS count \
         FROM journey_links_daily \
         WHERE app_id=$1 AND day>=$2 AND day<=$3 AND step < $4{env_sql} \
         GROUP BY step, from_name, to_name ORDER BY step, count DESC LIMIT 500"
    );
    let mut stmt = diesel::sql_query(links_q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Date, _>(lo)
        .bind::<Date, _>(hi)
        .bind::<BigInt, _>(depth - 1);
    stmt = crate::bind_env!(stmt, &scope.env);
    let links: Vec<JourneyLink> = stmt.get_results(conn).await?;
    Ok((nodes, links))
}

// ---------------------------------------------------------------------------
// Performance
// ---------------------------------------------------------------------------

#[derive(QueryableByName)]
struct PerfRow {
    #[diesel(sql_type = Timestamptz)]
    hour: DateTime<Utc>,
    #[diesel(sql_type = Text)]
    name: String,
    #[diesel(sql_type = Text)]
    op: String,
    #[diesel(sql_type = BigInt)]
    count: i64,
    #[diesel(sql_type = BigInt)]
    error_count: i64,
    #[diesel(sql_type = Double)]
    duration_sum: f64,
    #[diesel(sql_type = Bytea)]
    duration_hist: Vec<u8>,
}

async fn perf_rows(
    conn: &mut AsyncPgConnection,
    scope: &ReadScope,
    range: Range,
    name: Option<&str>,
    op: Option<&str>,
) -> QueryResult<Vec<PerfRow>> {
    let lo = range
        .from
        .duration_trunc(Duration::hours(1))
        .unwrap_or(range.from);
    let hi = range.to.unwrap_or_else(far_future);
    let env_sql = scope.env.sql_fragment(6);
    let q = format!(
        "SELECT hour, name, op, count, error_count, duration_sum, duration_hist \
         FROM perf_agg_hourly \
         WHERE app_id=$1 AND hour>=$2 AND hour<$3 \
           AND ($4::text IS NULL OR name=$4) AND ($5::text IS NULL OR op=$5){env_sql}"
    );
    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Timestamptz, _>(lo)
        .bind::<Timestamptz, _>(hi)
        .bind::<Nullable<Text>, _>(name.map(|s| s.to_string()))
        .bind::<Nullable<Text>, _>(op.map(|s| s.to_string()));
    stmt = crate::bind_env!(stmt, &scope.env);
    stmt.get_results(conn).await
}

/// Rollup twin of `repo::performance_summary` (the `device_key = None` shape —
/// the gate falls back to raw when a device filter is present).
pub async fn perf_summary(
    conn: &mut AsyncPgConnection,
    scope: &ReadScope,
    range: Range,
    op: Option<&str>,
) -> QueryResult<Vec<PerfSummaryRow>> {
    use std::collections::BTreeMap;
    let rows = perf_rows(conn, scope, range, None, op).await?;
    let mut by_op: BTreeMap<(String, String), (i64, i64, f64, LatencyHistogram)> = BTreeMap::new();
    for r in rows {
        let e = by_op
            .entry((r.name, r.op))
            .or_insert_with(|| (0, 0, 0.0, LatencyHistogram::new()));
        e.0 += r.count;
        e.1 += r.error_count;
        e.2 += r.duration_sum;
        e.3.merge_counts(&LatencyHistogram::counts_from_bytes(&r.duration_hist));
    }
    let mut out: Vec<PerfSummaryRow> = by_op
        .into_iter()
        .map(|((name, op), (count, errs, dsum, hist))| PerfSummaryRow {
            name,
            op,
            count,
            p50: hist.percentile(0.50),
            p75: hist.percentile(0.75),
            p95: hist.percentile(0.95),
            p99: hist.percentile(0.99),
            avg: ratio(dsum, count),
            error_rate: ratio(errs as f64, count),
        })
        .collect();
    out.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.name.cmp(&b.name)));
    out.truncate(100);
    Ok(out)
}

/// Rollup twin of `repo::performance_series` (hourly buckets on the wire).
pub async fn perf_series(
    conn: &mut AsyncPgConnection,
    scope: &ReadScope,
    range: Range,
    name: Option<&str>,
    op: Option<&str>,
) -> QueryResult<Vec<PerfSeriesPoint>> {
    use std::collections::BTreeMap;
    let rows = perf_rows(conn, scope, range, name, op).await?;
    let mut by_hour: BTreeMap<DateTime<Utc>, (i64, LatencyHistogram)> = BTreeMap::new();
    for r in rows {
        let e = by_hour
            .entry(r.hour)
            .or_insert_with(|| (0, LatencyHistogram::new()));
        e.0 += r.count;
        e.1.merge_counts(&LatencyHistogram::counts_from_bytes(&r.duration_hist));
    }
    let mut out: Vec<PerfSeriesPoint> = by_hour
        .into_iter()
        .map(|(bucket, (throughput, hist))| PerfSeriesPoint {
            bucket,
            p50: hist.percentile(0.50),
            p95: hist.percentile(0.95),
            throughput,
        })
        .collect();
    out.truncate(5000);
    Ok(out)
}

// ---------------------------------------------------------------------------
// Users
// ---------------------------------------------------------------------------

#[derive(QueryableByName)]
struct DayHllRow {
    #[diesel(sql_type = Date)]
    day: NaiveDate,
    #[diesel(sql_type = Array<Nullable<Bytea>>)]
    hlls: Vec<Option<Vec<u8>>>,
}

async fn activity_hlls(
    conn: &mut AsyncPgConnection,
    scope: &ReadScope,
    lo: NaiveDate,
    hi: NaiveDate,
    column: &'static str,
) -> QueryResult<Vec<DayHllRow>> {
    let env_sql = scope.env.sql_fragment(4);
    let q = format!(
        "SELECT day, array_agg({column}) AS hlls FROM user_activity_daily \
         WHERE app_id=$1 AND day>=$2 AND day<=$3{env_sql} GROUP BY day ORDER BY day"
    );
    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Date, _>(lo)
        .bind::<Date, _>(hi);
    stmt = crate::bind_env!(stmt, &scope.env);
    stmt.get_results(conn).await
}

#[derive(QueryableByName)]
struct EventUserLegs {
    #[diesel(sql_type = BigInt)]
    total_users: i64,
    #[diesel(sql_type = BigInt)]
    active_in_range: i64,
    #[diesel(sql_type = BigInt)]
    new_in_range: i64,
}

#[derive(QueryableByName)]
struct SessionSums {
    #[diesel(sql_type = BigInt)]
    sessions: i64,
    #[diesel(sql_type = BigInt)]
    crashed: i64,
    #[diesel(sql_type = Double)]
    dsum: f64,
    #[diesel(sql_type = Array<Bytea>)]
    hists: Vec<Vec<u8>>,
    #[diesel(sql_type = BigInt)]
    b0: i64,
    #[diesel(sql_type = BigInt)]
    b1: i64,
    #[diesel(sql_type = BigInt)]
    b2: i64,
    #[diesel(sql_type = BigInt)]
    b3: i64,
    #[diesel(sql_type = BigInt)]
    b4: i64,
}

async fn session_sums(
    conn: &mut AsyncPgConnection,
    scope: &ReadScope,
    range: Range,
) -> QueryResult<SessionSums> {
    let (lo, hi) = day_bounds(&range);
    let env_sql = scope.env.sql_fragment(4);
    let q = format!(
        "SELECT COALESCE(sum(sessions),0)::bigint AS sessions, \
                COALESCE(sum(crashed),0)::bigint AS crashed, \
                COALESCE(sum(duration_ms_sum),0)::float8 AS dsum, \
                COALESCE(array_agg(duration_hist), ARRAY[]::bytea[]) AS hists, \
                COALESCE(sum(d_lt10s),0)::bigint AS b0, COALESCE(sum(d_10_60s),0)::bigint AS b1, \
                COALESCE(sum(d_1_5m),0)::bigint AS b2, COALESCE(sum(d_5_30m),0)::bigint AS b3, \
                COALESCE(sum(d_gte30m),0)::bigint AS b4 \
         FROM session_stats_daily WHERE app_id=$1 AND day>=$2 AND day<=$3{env_sql}"
    );
    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Date, _>(lo)
        .bind::<Date, _>(hi);
    stmt = crate::bind_env!(stmt, &scope.env);
    stmt.get_result(conn).await
}

/// Rollup twin of `repo::user_stats`. The three `event_users` legs stay live
/// (small table) — but their env-membership filter rides the person-env
/// rollup once that marker is present (`event_user_membership_sql`): the raw
/// membership UNION scans the full history of three tables and was the one
/// full-scan this twin still carried under `environment_id=`. dau/wau/mau
/// become calendar-day sketch merges; the session duration legs read the
/// sessions rollup.
pub async fn user_stats(
    conn: &mut AsyncPgConnection,
    scope: &ReadScope,
    range: Range,
    now: DateTime<Utc>,
) -> QueryResult<UserStats> {
    let membership_sql = repo::event_user_membership_sql(conn, scope.app_id, &scope.env, 3).await?;
    let upper_idx = if scope.env.consumes_bind() { 4 } else { 3 };
    let up_last_seen = range.upper_sql("last_seen", upper_idx);
    let up_first_seen = range.upper_sql("first_seen", upper_idx);
    let q = format!(
        "SELECT \
           (SELECT count(*) FROM event_users WHERE app_id=$1{membership_sql})::bigint AS total_users, \
           (SELECT count(*) FROM event_users WHERE app_id=$1 AND last_seen>=$2{membership_sql}{up_last_seen})::bigint AS active_in_range, \
           (SELECT count(*) FROM event_users WHERE app_id=$1 AND first_seen>=$2{membership_sql}{up_first_seen})::bigint AS new_in_range"
    );
    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Timestamptz, _>(range.from);
    stmt = crate::bind_env!(stmt, &scope.env);
    let legs: EventUserLegs = crate::bind_range!(stmt, range).get_result(conn).await?;

    let today = now.date_naive();
    let rows = activity_hlls(conn, scope, today - Duration::days(29), today, "hll_all").await?;
    let (mut dau, mut wau, mut mau) = (Hll::new(), Hll::new(), Hll::new());
    for r in &rows {
        let h = merged_hll(&r.hlls);
        if r.day == today {
            dau.merge(&h);
        }
        if r.day > today - Duration::days(7) {
            wau.merge(&h);
        }
        mau.merge(&h);
    }

    let s = session_sums(conn, scope, range).await?;
    Ok(UserStats {
        total_users: legs.total_users,
        active_in_range: legs.active_in_range,
        new_in_range: legs.new_in_range,
        dau: dau.estimate(),
        wau: wau.estimate(),
        mau: mau.estimate(),
        avg_session_ms: ratio(s.dsum, s.sessions),
        median_session_ms: merged_hist(&s.hists).percentile(0.5),
    })
}

/// Rollup twin of `repo::active_user_series`: per-day actives from sketches,
/// per-day new users from the (small, live) `event_users` table.
pub async fn active_user_series(
    conn: &mut AsyncPgConnection,
    scope: &ReadScope,
    range: Range,
) -> QueryResult<Vec<UserSeriesPoint>> {
    let (lo, hi) = day_bounds(&range);
    let rows = activity_hlls(conn, scope, lo, hi, "hll_all").await?;
    let active: Vec<SeriesPoint> = rows
        .iter()
        .map(|r| SeriesPoint {
            bucket: day_bucket(r.day),
            count: merged_hll(&r.hlls).estimate(),
        })
        .collect();

    let membership_sql = repo::event_user_membership_sql(conn, scope.app_id, &scope.env, 3).await?;
    let upper_idx = if scope.env.consumes_bind() { 4 } else { 3 };
    let up = range.upper_sql("first_seen", upper_idx);
    let q = format!(
        "SELECT date_trunc('day', first_seen) AS bucket, count(*)::bigint AS count \
         FROM event_users WHERE app_id=$1 AND first_seen>=$2{membership_sql}{up} \
         GROUP BY bucket ORDER BY bucket"
    );
    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Timestamptz, _>(range.from);
    stmt = crate::bind_env!(stmt, &scope.env);
    let new: Vec<SeriesPoint> = crate::bind_range!(stmt, range).get_results(conn).await?;
    Ok(repo::merge_user_series(active, new))
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

pub async fn session_stats(
    conn: &mut AsyncPgConnection,
    scope: &ReadScope,
    range: Range,
) -> QueryResult<SessionStats> {
    let s = session_sums(conn, scope, range).await?;
    Ok(SessionStats {
        sessions: s.sessions,
        crashed: s.crashed,
        avg_session_ms: ratio(s.dsum, s.sessions),
        median_session_ms: merged_hist(&s.hists).percentile(0.5),
    })
}

#[derive(QueryableByName)]
struct DaySessionRow {
    #[diesel(sql_type = Date)]
    day: NaiveDate,
    #[diesel(sql_type = BigInt)]
    sessions: i64,
    #[diesel(sql_type = Double)]
    dsum: f64,
}

pub async fn session_duration_series(
    conn: &mut AsyncPgConnection,
    scope: &ReadScope,
    range: Range,
) -> QueryResult<Vec<SeriesAvgPoint>> {
    let (lo, hi) = day_bounds(&range);
    let env_sql = scope.env.sql_fragment(4);
    let q = format!(
        "SELECT day, COALESCE(sum(sessions),0)::bigint AS sessions, \
                COALESCE(sum(duration_ms_sum),0)::float8 AS dsum \
         FROM session_stats_daily WHERE app_id=$1 AND day>=$2 AND day<=$3{env_sql} \
         GROUP BY day ORDER BY day"
    );
    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Date, _>(lo)
        .bind::<Date, _>(hi);
    stmt = crate::bind_env!(stmt, &scope.env);
    let rows: Vec<DaySessionRow> = stmt.get_results(conn).await?;
    Ok(rows
        .into_iter()
        .map(|r| SeriesAvgPoint {
            bucket: day_bucket(r.day),
            avg_ms: ratio(r.dsum, r.sessions),
        })
        .collect())
}

pub async fn session_duration_histogram(
    conn: &mut AsyncPgConnection,
    scope: &ReadScope,
    range: Range,
) -> QueryResult<Vec<HistoBucket>> {
    let s = session_sums(conn, scope, range).await?;
    let counts = [s.b0, s.b1, s.b2, s.b3, s.b4];
    Ok(DURATION_BUCKETS
        .iter()
        .zip(counts)
        .map(|(bucket, count)| HistoBucket {
            bucket: bucket.to_string(),
            count,
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Events + Overview
// ---------------------------------------------------------------------------

pub async fn top_events(
    conn: &mut AsyncPgConnection,
    scope: &ReadScope,
    range: Range,
    limit: i64,
) -> QueryResult<Vec<EventCount>> {
    let (lo, hi) = day_bounds(&range);
    let env_sql = scope.env.sql_fragment(4);
    let limit_idx = if scope.env.consumes_bind() { 5 } else { 4 };
    let q = format!(
        "SELECT name, sum(count)::bigint AS count FROM event_top_daily \
         WHERE app_id=$1 AND day>=$2 AND day<=$3{env_sql} \
         GROUP BY name ORDER BY count DESC LIMIT ${limit_idx}"
    );
    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Date, _>(lo)
        .bind::<Date, _>(hi);
    stmt = crate::bind_env!(stmt, &scope.env);
    stmt.bind::<BigInt, _>(limit).get_results(conn).await
}

#[derive(QueryableByName)]
struct DayCount {
    #[diesel(sql_type = Date)]
    day: NaiveDate,
    #[diesel(sql_type = BigInt)]
    count: i64,
}

async fn day_counts(
    conn: &mut AsyncPgConnection,
    scope: &ReadScope,
    range: Range,
    table: &'static str,
    column: &'static str,
    name: Option<&str>,
) -> QueryResult<Vec<DayCount>> {
    let (lo, hi) = day_bounds(&range);
    let (name_sql, env_idx) = match name {
        Some(_) => (" AND name=$4", 5),
        None => ("", 4),
    };
    let env_sql = scope.env.sql_fragment(env_idx);
    let q = format!(
        "SELECT day, COALESCE(sum({column}),0)::bigint AS count FROM {table} \
         WHERE app_id=$1 AND day>=$2 AND day<=$3{name_sql}{env_sql} \
         GROUP BY day ORDER BY day"
    );
    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Date, _>(lo)
        .bind::<Date, _>(hi);
    if let Some(n) = name {
        stmt = stmt.bind::<Text, _>(n.to_string());
    }
    stmt = crate::bind_env!(stmt, &scope.env);
    stmt.get_results(conn).await
}

/// Rollup twin of `repo::event_series`.
pub async fn event_series(
    conn: &mut AsyncPgConnection,
    scope: &ReadScope,
    name: Option<&str>,
    range: Range,
) -> QueryResult<Vec<SeriesPoint>> {
    let rows = match name {
        Some(n) => day_counts(conn, scope, range, "event_top_daily", "count", Some(n)).await?,
        None => day_counts(conn, scope, range, "user_activity_daily", "events", None).await?,
    };
    Ok(rows
        .into_iter()
        .map(|r| SeriesPoint {
            bucket: day_bucket(r.day),
            count: r.count,
        })
        .collect())
}

/// Rollup twin of `repo::error_series`.
pub async fn error_series(
    conn: &mut AsyncPgConnection,
    scope: &ReadScope,
    range: Range,
) -> QueryResult<Vec<SeriesPoint>> {
    let rows = day_counts(conn, scope, range, "user_activity_daily", "errors", None).await?;
    Ok(rows
        .into_iter()
        .map(|r| SeriesPoint {
            bucket: day_bucket(r.day),
            count: r.count,
        })
        .collect())
}

#[derive(QueryableByName)]
struct TwoCounts {
    #[diesel(sql_type = BigInt)]
    events: i64,
    #[diesel(sql_type = BigInt)]
    errors: i64,
}

#[derive(QueryableByName)]
struct UsersLegsAndSignal {
    #[diesel(sql_type = BigInt)]
    users: i64,
    #[diesel(sql_type = BigInt)]
    new_users: i64,
    #[diesel(sql_type = diesel::sql_types::Bool)]
    has_crash_signal: bool,
}

/// Rollup twin of `repo::overview_totals`: events/errors/sessions/crashed come
/// from rollups; the `event_users` legs and the crash-signal EXISTS stay live
/// (small table / short-circuiting probe).
pub async fn overview_totals(
    conn: &mut AsyncPgConnection,
    scope: &ReadScope,
    range: Range,
) -> QueryResult<OverviewTotals> {
    let (lo, hi) = day_bounds(&range);
    let env_sql = scope.env.sql_fragment(4);
    let q = format!(
        "SELECT COALESCE(sum(events),0)::bigint AS events, COALESCE(sum(errors),0)::bigint AS errors \
         FROM user_activity_daily WHERE app_id=$1 AND day>=$2 AND day<=$3{env_sql}"
    );
    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Date, _>(lo)
        .bind::<Date, _>(hi);
    stmt = crate::bind_env!(stmt, &scope.env);
    let counts: TwoCounts = stmt.get_result(conn).await?;
    let s = session_sums(conn, scope, range).await?;

    let membership_sql = repo::event_user_membership_sql(conn, scope.app_id, &scope.env, 3).await?;
    let env_sql_errors = scope.env.sql_fragment_for("error_events", 3);
    let upper_idx = if scope.env.consumes_bind() { 4 } else { 3 };
    let up_last_seen = range.upper_sql("last_seen", upper_idx);
    let up_first_seen = range.upper_sql("first_seen", upper_idx);
    let up_occurred = range.upper_sql("occurred_at", upper_idx);
    let q = format!(
        "SELECT \
           (SELECT count(*) FROM event_users WHERE app_id=$1 AND last_seen>=$2{membership_sql}{up_last_seen})::bigint AS users, \
           (SELECT count(*) FROM event_users WHERE app_id=$1 AND first_seen>=$2{membership_sql}{up_first_seen})::bigint AS new_users, \
           EXISTS(SELECT 1 FROM error_events WHERE app_id=$1 AND occurred_at>=$2 AND handled IS NOT NULL{env_sql_errors}{up_occurred}) AS has_crash_signal"
    );
    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Timestamptz, _>(range.from);
    stmt = crate::bind_env!(stmt, &scope.env);
    let legs: UsersLegsAndSignal = crate::bind_range!(stmt, range).get_result(conn).await?;

    Ok(OverviewTotals {
        events: counts.events,
        errors: counts.errors,
        sessions: s.sessions,
        users: legs.users,
        new_users: legs.new_users,
        crashed_sessions: s.crashed,
        has_crash_signal: legs.has_crash_signal,
    })
}

/// Rollup replacement for the whole hot/cold `active_users_by_day` split:
/// rollups never tier out, so one read covers any window. Matches the raw
/// query's analytics-only, `distinct_id <> ''` semantics via `hll_analytics`.
pub async fn active_users_by_day(
    conn: &mut AsyncPgConnection,
    scope: &ReadScope,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> QueryResult<Vec<DayCountRow>> {
    let lo = from.date_naive();
    let hi = (to - Duration::microseconds(1)).date_naive().max(lo);
    let rows = activity_hlls(conn, scope, lo, hi, "hll_analytics").await?;
    Ok(rows
        .into_iter()
        .map(|r| DayCountRow {
            day: r.day,
            count: merged_hll(&r.hlls).estimate(),
        })
        .collect())
}
