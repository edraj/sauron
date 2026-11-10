//! `issue_stats_hourly` (migration 84): events per (app, environment, issue,
//! release, UTC hour), so MCP trending can rank issues by what happened IN a
//! window rather than by lifetime totals.
//!
//! # Exactly once
//!
//! Three writers touch this table, and every one of them runs inside the
//! module-wide rollup advisory lock ([`super::begin_locked`]):
//!
//! - **The live fold** ([`super::fold::fold_issue_stats`]) ADDS rows with
//!   `received_at` in `(wm, new_wm]` and advances the `issue_stats` watermark
//!   to `new_wm` in the same transaction ([`add_issue_stats`]).
//! - **The consistency rebuild** (`fold_day_body` with `delete_first` and no
//!   `received_upto`, or [`replace_day_locked`] when only this table drifted
//!   that day) and **the operator backfill** (Task 8, per app) REPLACE a day:
//!   delete it, then count raw rows with `received_at <= wm`, reading `wm`
//!   inside that same locked transaction ([`replace_day`]).
//!
//! Invariant, per cell: the stored count is the number of DISTINCT raw rows
//! in that cell with `received_at <= wm` that some writer has counted —
//! never one received after `wm`, never one twice. The fold preserves it
//! (it only adds rows above `wm`, then raises `wm` past them; `wm` never moves
//! back, `set_watermark` is `GREATEST`). A replace re-establishes it from
//! scratch for its scope, and whatever the fold adds afterwards is strictly
//! newer than the bound it used. The lock serialises the two, so the
//! watermark cannot move between a replace reading it and counting under it.
//!
//! The base rollups' additive backfill (`received_upto = Some(epoch)`,
//! `delete_first = false`) never writes this table: an additive pass over
//! rows the fold may already hold is exactly the double count this design
//! avoids.

use std::collections::HashMap;

use chrono::{DateTime, Duration, NaiveDate, Utc};
use diesel::sql_types::Bool;
use diesel::sql_types::{Array, BigInt, Nullable, Text, Timestamptz, Uuid as SqlUuid};
use diesel::QueryableByName;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use uuid::Uuid;

use super::fold::finish;
use super::fold::hour_of;
use super::CHUNK;
use super::{begin_locked, watermark, SRC_ISSUE_STATS};
use crate::batch::release_key;
use crate::scope::{Range, ReadScope};

/// One raw `error_events` row as the issue_stats pull reads it.
#[derive(QueryableByName, Clone, Debug)]
pub struct IRow {
    #[diesel(sql_type = SqlUuid)]
    pub app_id: Uuid,
    #[diesel(sql_type = Nullable<SqlUuid>)]
    pub environment_id: Option<Uuid>,
    #[diesel(sql_type = SqlUuid)]
    pub issue_id: Uuid,
    #[diesel(sql_type = Nullable<Text>)]
    pub release: Option<String>,
    #[diesel(sql_type = Timestamptz)]
    pub occurred_at: DateTime<Utc>,
    #[diesel(sql_type = Timestamptz)]
    pub received_at: DateTime<Utc>,
}

/// `(app, environment-as-stored, issue, release key, hour start)`. The
/// release key is `''` for a NULL release ([`release_key`]), the same
/// sentinel the table and [`replace_day`]'s `COALESCE(release, '')` use.
pub type IssueHourKey = (Uuid, Option<Uuid>, Uuid, String, DateTime<Utc>);

/// Count rows per cell. Pure: no I/O.
pub fn fold_issue_rows(rows: &[IRow]) -> HashMap<IssueHourKey, i64> {
    let mut out: HashMap<IssueHourKey, i64> = HashMap::new();
    for r in rows {
        let key: IssueHourKey = (
            r.app_id,
            r.environment_id,
            r.issue_id,
            release_key(r.release.as_deref()),
            hour_of(r.occurred_at),
        );
        *out.entry(key).or_insert(0) += 1;
    }
    out
}

/// Additive upsert of fold deltas. Only the live fold calls this — anything
/// that recounts from raw must go through [`replace_day`] instead.
///
/// Keys are applied sorted in CONFLICT-KEY order — `(app, hour, issue,
/// release, COALESCE(env, nil))`, the column order of `issue_stats_hourly_key`
/// — which is the house lock-ordering rule (every writer takes row locks in
/// one index order). They are unique per statement by construction (map
/// keys), which `ON CONFLICT DO UPDATE` requires.
pub async fn add_issue_stats(
    conn: &mut AsyncPgConnection,
    deltas: &HashMap<IssueHourKey, i64>,
) -> diesel::QueryResult<()> {
    let mut rows: Vec<(&IssueHourKey, &i64)> = deltas.iter().filter(|(_, n)| **n != 0).collect();
    rows.sort_by(|(a, _), (b, _)| conflict_order(a).cmp(&conflict_order(b)));
    for chunk in rows.chunks(CHUNK) {
        diesel::sql_query(
            "INSERT INTO issue_stats_hourly \
                 (app_id, environment_id, issue_id, release, hour, events, updated_at) \
             SELECT app_id, env, issue_id, release, hour, events, now() \
             FROM unnest($1::uuid[], $2::uuid[], $3::uuid[], $4::text[], $5::timestamptz[], $6::bigint[]) \
                  AS t(app_id, env, issue_id, release, hour, events) \
             ON CONFLICT (app_id, hour, issue_id, release, \
                          COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid)) \
             DO UPDATE SET events = issue_stats_hourly.events + EXCLUDED.events, \
                           updated_at = now()",
        )
        .bind::<Array<SqlUuid>, _>(chunk.iter().map(|(k, _)| k.0).collect::<Vec<_>>())
        .bind::<Array<Nullable<SqlUuid>>, _>(chunk.iter().map(|(k, _)| k.1).collect::<Vec<_>>())
        .bind::<Array<SqlUuid>, _>(chunk.iter().map(|(k, _)| k.2).collect::<Vec<_>>())
        .bind::<Array<Text>, _>(chunk.iter().map(|(k, _)| k.3.clone()).collect::<Vec<_>>())
        .bind::<Array<Timestamptz>, _>(chunk.iter().map(|(k, _)| k.4).collect::<Vec<_>>())
        .bind::<Array<BigInt>, _>(chunk.iter().map(|(_, n)| **n).collect::<Vec<_>>())
        .execute(conn)
        .await?;
    }
    Ok(())
}

/// An [`IssueHourKey`] rearranged into `issue_stats_hourly_key`'s column
/// order, NULL environment as the nil sentinel the index uses.
fn conflict_order(k: &IssueHourKey) -> (Uuid, DateTime<Utc>, Uuid, &str, Uuid) {
    (k.0, k.4, k.2, k.3.as_str(), k.1.unwrap_or(super::NIL_ENV))
}

/// REPLACE one UTC day (optionally one app's slice of it) from raw
/// `error_events`, counting only rows with `received_at <= received_upto`.
/// Returns the number of rows inserted.
///
/// Must run INSIDE a transaction that holds the rollup advisory lock, with
/// `received_upto` = the `issue_stats` watermark read in that same
/// transaction. That pairing is what makes it safe against the live fold:
/// everything the fold has added is `<=` the watermark (and is deleted and
/// recounted here), everything it will add is `>` it.
///
/// Both bounds are computed in Rust rather than as `$1 + interval '1 day'`,
/// which would follow the session TimeZone across a DST change.
pub(crate) async fn replace_day(
    conn: &mut AsyncPgConnection,
    day: NaiveDate,
    app_id: Option<Uuid>,
    received_upto: DateTime<Utc>,
) -> diesel::QueryResult<u64> {
    let start = day.and_hms_opt(0, 0, 0).expect("valid").and_utc();
    let end = start + Duration::days(1);
    let inserted = match app_id {
        None => {
            diesel::sql_query("DELETE FROM issue_stats_hourly WHERE hour >= $1 AND hour < $2")
                .bind::<Timestamptz, _>(start)
                .bind::<Timestamptz, _>(end)
                .execute(conn)
                .await?;
            diesel::sql_query(
                "INSERT INTO issue_stats_hourly \
                     (app_id, issue_id, environment_id, release, hour, events) \
                 SELECT app_id, issue_id, environment_id, COALESCE(release, ''), \
                        date_trunc('hour', occurred_at, 'UTC'), count(*) \
                 FROM error_events \
                 WHERE occurred_at >= $1 AND occurred_at < $2 AND received_at <= $3 \
                 GROUP BY 1, 2, 3, 4, 5",
            )
            .bind::<Timestamptz, _>(start)
            .bind::<Timestamptz, _>(end)
            .bind::<Timestamptz, _>(received_upto)
            .execute(conn)
            .await?
        }
        Some(app) => {
            diesel::sql_query(
                "DELETE FROM issue_stats_hourly WHERE app_id = $3 AND hour >= $1 AND hour < $2",
            )
            .bind::<Timestamptz, _>(start)
            .bind::<Timestamptz, _>(end)
            .bind::<SqlUuid, _>(app)
            .execute(conn)
            .await?;
            diesel::sql_query(
                "INSERT INTO issue_stats_hourly \
                     (app_id, issue_id, environment_id, release, hour, events) \
                 SELECT app_id, issue_id, environment_id, COALESCE(release, ''), \
                        date_trunc('hour', occurred_at, 'UTC'), count(*) \
                 FROM error_events \
                 WHERE app_id = $4 AND occurred_at >= $1 AND occurred_at < $2 \
                   AND received_at <= $3 \
                 GROUP BY 1, 2, 3, 4, 5",
            )
            .bind::<Timestamptz, _>(start)
            .bind::<Timestamptz, _>(end)
            .bind::<Timestamptz, _>(received_upto)
            .bind::<SqlUuid, _>(app)
            .execute(conn)
            .await?
        }
    };
    Ok(inserted as u64)
}

/// The one definition of "an app whose `issue_stats_hourly` history is
/// complete": it has a backfill marker, or it was created at-or-after the
/// epoch (every row it will ever have is post-epoch and folded live). A
/// subquery yielding `app_id`; [`is_ready`] and both consistency-check SQL
/// strings in `fold.rs` embed it so they cannot drift apart.
pub(crate) const READY_APPS_SQL: &str = "(SELECT app_id FROM issue_stats_backfill \
     UNION SELECT a.id FROM apps a, issue_stats_epoch e WHERE a.created_at >= e.started_at)";

#[derive(diesel::QueryableByName)]
struct BoolRow {
    #[diesel(sql_type = Bool)]
    present: bool,
}

#[derive(diesel::QueryableByName)]
struct OptTsRow {
    #[diesel(sql_type = Nullable<Timestamptz>)]
    t: Option<DateTime<Utc>>,
}

#[derive(diesel::QueryableByName)]
struct TsRow {
    #[diesel(sql_type = Timestamptz)]
    t: DateTime<Utc>,
}

#[derive(diesel::QueryableByName)]
struct AppRow {
    #[diesel(sql_type = SqlUuid)]
    id: Uuid,
}

/// The instant the live fold started counting (`issue_stats_epoch`).
pub async fn epoch(conn: &mut AsyncPgConnection) -> diesel::QueryResult<DateTime<Utc>> {
    let r: TsRow = diesel::sql_query("SELECT started_at AS t FROM issue_stats_epoch")
        .get_result(conn)
        .await?;
    Ok(r.t)
}

/// Whether `app_id` has complete per-issue hourly history ([`READY_APPS_SQL`]).
pub async fn is_ready(conn: &mut AsyncPgConnection, app_id: Uuid) -> diesel::QueryResult<bool> {
    let sql =
        format!("SELECT EXISTS (SELECT 1 FROM {READY_APPS_SQL} r WHERE r.app_id = $1) AS present");
    let r: BoolRow = diesel::sql_query(sql)
        .bind::<SqlUuid, _>(app_id)
        .get_result(conn)
        .await?;
    Ok(r.present)
}

/// From when this app's hourly counts are complete, for a reader comparing
/// two windows: `None` means complete for every hour.
///
/// - **Marker-less app created at or after the epoch: `None`.** The live
///   fold has counted every row such an app has ever received, so an hour
///   before its `created_at` holds nothing because nothing happened, not
///   because it was not counted. (A device clock running behind can stamp
///   an event before `created_at`; the fold counts that too.)
/// - **App with a backfill marker: its `covered_from`.** Hours before it
///   were never replaced from raw rows.
/// - **App not ready at all: the epoch.** Only rows received since then have
///   been counted. Readers check [`is_ready`] first and should not get here.
pub async fn comparison_floor(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
) -> diesel::QueryResult<Option<DateTime<Utc>>> {
    let r: OptTsRow = diesel::sql_query(
        "SELECT CASE \
             WHEN EXISTS (SELECT 1 FROM issue_stats_backfill WHERE app_id = $1) \
                 THEN (SELECT covered_from FROM issue_stats_backfill WHERE app_id = $1) \
             WHEN EXISTS (SELECT 1 FROM apps a, issue_stats_epoch e \
                           WHERE a.id = $1 AND a.created_at >= e.started_at) \
                 THEN NULL \
             ELSE (SELECT started_at FROM issue_stats_epoch) \
         END AS t",
    )
    .bind::<SqlUuid, _>(app_id)
    .get_result(conn)
    .await?;
    Ok(r.t)
}

/// Exclusive upper bound of `error_events` raw rows the tier has dropped from
/// Postgres (`tiering_state.dropped_thru`), if any. The error_events-specific
/// floor, not [`super::fold::tier_dropped_floor`]'s max across three tables:
/// this table's replace only reads `error_events`. Readers of raw
/// `error_events` (the MCP tools' on-read user counts and breakdowns) use it
/// to say when a window reaches below what Postgres still holds.
pub async fn error_events_floor(
    conn: &mut AsyncPgConnection,
) -> diesel::QueryResult<Option<DateTime<Utc>>> {
    let r: OptTsRow = diesel::sql_query(
        "SELECT max(dropped_thru) AS t FROM tiering_state WHERE table_name = 'error_events'",
    )
    .get_result(conn)
    .await?;
    Ok(r.t)
}

fn day_start(day: NaiveDate) -> DateTime<Utc> {
    day.and_hms_opt(0, 0, 0).expect("valid").and_utc()
}

/// First UTC day whose raw rows are fully hot: the floor's day when the floor
/// sits exactly on midnight, else the next one.
fn first_hot_day(floor: DateTime<Utc>) -> NaiveDate {
    let d = floor.date_naive();
    if day_start(d) < floor {
        d.succ_opt().expect("date in range")
    } else {
        d
    }
}

/// [`replace_day`] for one app as ONE transaction that takes the rollup
/// advisory lock and reads the `issue_stats` watermark under it. This is the
/// only safe way to call the replace from outside this module: the lock and
/// the in-lock watermark read are what make it exactly-once against the fold.
///
/// Returns `Ok(None)` without touching anything when the day starts below the
/// `error_events` tier floor, re-read under the lock (the tier can advance
/// during a long run): part of that day's raw rows live only in cold storage,
/// and recounting from the strays left in Postgres would delete good counts.
pub async fn replace_app_day_locked(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    day: NaiveDate,
) -> diesel::QueryResult<Option<u64>> {
    begin_locked(conn).await?;
    let out = async {
        if let Some(floor) = error_events_floor(conn).await? {
            if day_start(day) < floor {
                return Ok(None);
            }
        }
        let wm = watermark(conn, SRC_ISSUE_STATS).await?;
        replace_day(conn, day, Some(app_id), wm).await.map(Some)
    }
    .await;
    finish(conn, out).await
}

/// [`replace_day`] for EVERY app, as one locked transaction: the consistency
/// heal for a day on which only `issue_stats` drifted. It rewrites
/// `issue_stats_hourly` for that day and nothing else, where
/// `fold::fold_day_from_raw` would rebuild every rollup table of the day to
/// fix this one.
///
/// Same contract as [`replace_app_day_locked`]: the rollup advisory lock, the
/// `issue_stats` watermark read under it, and `Ok(None)` without touching
/// anything when the day starts below the `error_events` tier floor
/// (re-read under the lock).
pub async fn replace_day_locked(
    conn: &mut AsyncPgConnection,
    day: NaiveDate,
) -> diesel::QueryResult<Option<u64>> {
    begin_locked(conn).await?;
    let out = async {
        if let Some(floor) = error_events_floor(conn).await? {
            if day_start(day) < floor {
                return Ok(None);
            }
        }
        let wm = watermark(conn, SRC_ISSUE_STATS).await?;
        replace_day(conn, day, None, wm).await.map(Some)
    }
    .await;
    finish(conn, out).await
}

/// What [`backfill_all`] reports as it goes.
#[derive(Clone, Debug)]
pub enum Progress {
    /// Apps awaiting backfill, once, at the start.
    Pending(usize),
    /// One (app, day) replaced.
    Day(Uuid, NaiveDate),
    /// One (app, day) skipped: below the `error_events` tier floor.
    Skipped(Uuid, NaiveDate),
    /// The app's marker was written.
    Ready(Uuid, DateTime<Utc>),
}

/// Rebuild hot (Postgres) history for every app created before the epoch that
/// has no marker: one locked transaction per (app, day) from the day of the
/// app's earliest eligible `error_events` row through today (UTC), then the
/// marker.
///
/// Eligible means hot: rows restored from cold storage
/// (`restored_pin_id IS NOT NULL`) and anything below the `error_events` tier
/// floor are ignored, and `covered_from` = max(first day 00:00 UTC, the first
/// fully hot day, re-read after the last day). With no eligible rows it is
/// the epoch (or the floor, if later). Idempotent: a crashed or repeated run
/// just replaces the days again. Returns the number of apps completed.
/// Operator-run only, never automatic.
pub async fn backfill_all(
    pool: &crate::PgPool,
    mut progress: impl FnMut(Progress),
) -> anyhow::Result<usize> {
    let mut conn = crate::conn(pool).await?;
    let epoch = epoch(&mut conn).await?;
    let apps: Vec<AppRow> = diesel::sql_query(format!(
        "SELECT a.id FROM apps a WHERE a.id NOT IN {READY_APPS_SQL} ORDER BY a.created_at, a.id"
    ))
    .load(&mut conn)
    .await?;
    progress(Progress::Pending(apps.len()));
    let mut done = 0;
    for AppRow { id: app } in apps {
        let floor_start = error_events_floor(&mut conn)
            .await?
            .map(|f| day_start(first_hot_day(f)));
        let first: OptTsRow = diesel::sql_query(
            "SELECT min(occurred_at) AS t FROM error_events \
              WHERE app_id = $1 AND restored_pin_id IS NULL \
                AND ($2::timestamptz IS NULL OR occurred_at >= $2)",
        )
        .bind::<SqlUuid, _>(app)
        .bind::<Nullable<Timestamptz>, _>(floor_start)
        .get_result(&mut conn)
        .await?;
        let mut covered_from = match first.t {
            None => epoch,
            Some(min) => {
                let today = Utc::now().date_naive();
                let mut day = min.date_naive();
                while day <= today {
                    match replace_app_day_locked(&mut conn, app, day).await? {
                        Some(_) => progress(Progress::Day(app, day)),
                        None => progress(Progress::Skipped(app, day)),
                    }
                    day = day.succ_opt().expect("date in range");
                }
                day_start(min.date_naive())
            }
        };
        // The tier may have advanced during the run: never claim a day it
        // dropped (and this run therefore skipped or replaced too early).
        if let Some(f) = error_events_floor(&mut conn).await? {
            covered_from = covered_from.max(day_start(first_hot_day(f)));
        }
        diesel::sql_query(
            "INSERT INTO issue_stats_backfill (app_id, covered_from) VALUES ($1, $2) \
             ON CONFLICT (app_id) DO NOTHING",
        )
        .bind::<SqlUuid, _>(app)
        .bind::<Timestamptz, _>(covered_from)
        .execute(&mut conn)
        .await?;
        progress(Progress::Ready(app, covered_from));
        done += 1;
    }
    Ok(done)
}

// ---------------------------------------------------------------------------
// Reads: MCP `trending_issues`
//
// Callers check [`is_ready`] first: an app that is not ready has incomplete
// history here, and a ranking over it would be silently wrong.
// ---------------------------------------------------------------------------

/// One issue's events in a window, as [`ranked`] returns it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RankedIssue {
    pub issue_id: Uuid,
    pub events: i64,
}

#[derive(diesel::QueryableByName)]
struct IssueEventsRow {
    #[diesel(sql_type = SqlUuid)]
    issue_id: Uuid,
    #[diesel(sql_type = BigInt)]
    events: i64,
}

/// The `hour` bounds a window reads: every bucket that overlaps
/// `[range.from, range.to)`. Below, the bucket holding `from`; above, `to`
/// itself, or `now + 1h` when the range is open, which takes in the current
/// partial hour. Pass an hour-aligned `to` and two adjacent ranges read
/// disjoint buckets (the trending window and the one before it do).
fn hour_bounds(range: Range) -> (DateTime<Utc>, DateTime<Utc>) {
    (
        hour_of(range.from),
        range.to.unwrap_or_else(|| Utc::now() + Duration::hours(1)),
    )
}

/// `fragment(n)` for the next free bind index `n`, which it then consumes,
/// when the filter is present; `""` (and no index) when it is not.
fn bind_slot(next: &mut usize, present: bool, fragment: impl FnOnce(usize) -> String) -> String {
    if !present {
        return String::new();
    }
    let sql = fragment(*next);
    *next += 1;
    sql
}

/// The issues with the most events in `range`, most first (`events DESC,
/// issue_id`), at most `limit`.
///
/// - `scope`: the tenant, and the environment filter in any of its four
///   shapes (an env-scoped caller's `Subset` included).
/// - `release`: the rollup key, `""` for events without a release (see
///   `mcp::access::release_key_opt`); `None` for every release.
/// - `status`: one `issues.status`, or `None` for every status. `issues` is
///   joined for it (and to re-assert the tenant) AFTER the sum, so it costs
///   one probe per issue rather than one per hourly cell.
/// - `candidates`: when `Some`, only these issues (the `query` argument's
///   matches); `Some(&[])` matches nothing.
pub async fn ranked(
    conn: &mut AsyncPgConnection,
    scope: &ReadScope,
    release: Option<&str>,
    range: Range,
    status: Option<&str>,
    candidates: Option<&[Uuid]>,
    limit: i64,
) -> diesel::QueryResult<Vec<RankedIssue>> {
    if candidates.is_some_and(<[Uuid]>::is_empty) {
        return Ok(Vec::new());
    }
    let (from, to) = hour_bounds(range);
    // Bind layout: $1 app, $2 from hour, $3 to; then, each only when present
    // and in this order, environment (One/Subset; `Unattributed` is a literal
    // IS NULL), release, status, candidates; the limit last. The binds below
    // follow the same order.
    let mut next = 4;
    let env_sql = scope.env.sql_fragment_for("h", next);
    if scope.env.consumes_bind() {
        next += 1;
    }
    let release_sql = bind_slot(&mut next, release.is_some(), |i| {
        format!(" AND h.release = ${i}")
    });
    let status_sql = bind_slot(&mut next, status.is_some(), |i| {
        format!(" WHERE i.status = ${i}")
    });
    let candidates_sql = bind_slot(&mut next, candidates.is_some(), |i| {
        format!(" AND h.issue_id = ANY(${i})")
    });
    let limit_idx = next;
    // The tenant is asserted twice: on the rollup (its unique key leads with
    // app_id and hour, which serves the range) and on the issues join.
    let sql = format!(
        "SELECT t.issue_id, t.events \
         FROM ( \
             SELECT h.issue_id, sum(h.events)::bigint AS events \
             FROM issue_stats_hourly h \
             WHERE h.app_id = $1 AND h.hour >= $2 AND h.hour < $3\
                   {env_sql}{release_sql}{candidates_sql} \
             GROUP BY h.issue_id \
         ) t \
         JOIN issues i ON i.id = t.issue_id AND i.app_id = $1\
         {status_sql} \
         ORDER BY t.events DESC, t.issue_id \
         LIMIT ${limit_idx}"
    );
    let mut stmt = diesel::sql_query(sql)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Timestamptz, _>(from)
        .bind::<Timestamptz, _>(to);
    stmt = crate::bind_env!(stmt, &scope.env);
    if let Some(r) = release {
        stmt = stmt.bind::<Text, _>(r.to_string());
    }
    if let Some(s) = status {
        stmt = stmt.bind::<Text, _>(s.to_string());
    }
    if let Some(c) = candidates {
        stmt = stmt.bind::<Array<SqlUuid>, _>(c.to_vec());
    }
    let rows: Vec<IssueEventsRow> = stmt.bind::<BigInt, _>(limit).get_results(conn).await?;
    Ok(rows
        .into_iter()
        .map(|r| RankedIssue {
            issue_id: r.issue_id,
            events: r.events,
        })
        .collect())
}

/// Events in `range` for each of `ids`, under the same filters as
/// [`ranked`] (minus status, which does not change an issue's count). An
/// issue with no events there, or not in `scope`'s app, is absent. Served by
/// `issue_stats_hourly_issue_idx (issue_id, hour)`; this is how the previous
/// window is counted for the issues a tool shows.
pub async fn events_for(
    conn: &mut AsyncPgConnection,
    scope: &ReadScope,
    release: Option<&str>,
    range: Range,
    ids: &[Uuid],
) -> diesel::QueryResult<HashMap<Uuid, i64>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let (from, to) = hour_bounds(range);
    // Bind layout: $1 app, $2 ids, $3 from hour, $4 to; then environment
    // (One/Subset only) and release, each only when present.
    let mut next = 5;
    let env_sql = scope.env.sql_fragment_for("h", next);
    if scope.env.consumes_bind() {
        next += 1;
    }
    let release_sql = bind_slot(&mut next, release.is_some(), |i| {
        format!(" AND h.release = ${i}")
    });
    let sql = format!(
        "SELECT h.issue_id, sum(h.events)::bigint AS events \
         FROM issue_stats_hourly h \
         WHERE h.app_id = $1 AND h.issue_id = ANY($2) AND h.hour >= $3 AND h.hour < $4\
               {env_sql}{release_sql} \
         GROUP BY h.issue_id"
    );
    let mut stmt = diesel::sql_query(sql)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Array<SqlUuid>, _>(ids.to_vec())
        .bind::<Timestamptz, _>(from)
        .bind::<Timestamptz, _>(to);
    stmt = crate::bind_env!(stmt, &scope.env);
    if let Some(r) = release {
        stmt = stmt.bind::<Text, _>(r.to_string());
    }
    let rows: Vec<IssueEventsRow> = stmt.get_results(conn).await?;
    Ok(rows.into_iter().map(|r| (r.issue_id, r.events)).collect())
}

/// One release's error activity in a window, as [`release_error_counts`]
/// returns it. `release` is `''` for events that carried none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseErrors {
    pub release: String,
    pub events: i64,
    pub issues: i64,
}

#[derive(diesel::QueryableByName)]
struct ReleaseErrorsRow {
    #[diesel(sql_type = Text)]
    release: String,
    #[diesel(sql_type = BigInt)]
    events: i64,
    #[diesel(sql_type = BigInt)]
    issues: i64,
}

/// Error events and distinct issues per release in `range`, most events
/// first (`events DESC, release`). Same hour buckets and environment filter
/// shapes as [`ranked`]. Callers check [`is_ready`] first.
///
/// `issues` counts distinct issues with at least one event of that release in
/// the window; an issue seen in two releases counts once in each.
pub async fn release_error_counts(
    conn: &mut AsyncPgConnection,
    scope: &ReadScope,
    range: Range,
) -> diesel::QueryResult<Vec<ReleaseErrors>> {
    let (from, to) = hour_bounds(range);
    // $1 app, $2 from hour, $3 to; environment (One/Subset) at $4.
    let env_sql = scope.env.sql_fragment_for("h", 4);
    let sql = format!(
        "SELECT h.release, sum(h.events)::bigint AS events, \
                count(DISTINCT h.issue_id)::bigint AS issues \
         FROM issue_stats_hourly h \
         WHERE h.app_id = $1 AND h.hour >= $2 AND h.hour < $3{env_sql} \
         GROUP BY h.release \
         ORDER BY events DESC, h.release"
    );
    let mut stmt = diesel::sql_query(sql)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Timestamptz, _>(from)
        .bind::<Timestamptz, _>(to);
    stmt = crate::bind_env!(stmt, &scope.env);
    let rows: Vec<ReleaseErrorsRow> = stmt.get_results(conn).await?;
    Ok(rows
        .into_iter()
        .map(|r| ReleaseErrors {
            release: r.release,
            events: r.events,
            issues: r.issues,
        })
        .collect())
}

#[derive(diesel::QueryableByName)]
struct NewIssuesRow {
    #[diesel(sql_type = Text)]
    release: String,
    #[diesel(sql_type = BigInt)]
    n: i64,
}

/// New issues per release: every issue of the app with
/// `issues.first_seen >= coverage_from`, attributed to the release of its
/// EARLIEST `issue_stats_hourly` row (`''` when that row had none), counted
/// per release.
///
/// Rollup rows outside the scope's environment filter are not considered, so
/// an env-scoped caller sees an issue's first bucket among THEIR
/// environments. `DISTINCT ON (issue_id) ... ORDER BY issue_id, hour` is
/// served by `(issue_id, hour)`; ties on the hour break by release so the
/// answer is deterministic. An issue with no rollup row (all of its events
/// outside the scope) is not counted.
pub async fn new_issues_by_release(
    conn: &mut AsyncPgConnection,
    scope: &ReadScope,
    coverage_from: DateTime<Utc>,
) -> diesel::QueryResult<HashMap<String, i64>> {
    // $1 app, $2 coverage_from; environment (One/Subset) at $3.
    let env_sql = scope.env.sql_fragment_for("h", 3);
    let sql = format!(
        "SELECT f.release, count(*)::bigint AS n \
         FROM ( \
             SELECT DISTINCT ON (h.issue_id) h.issue_id, h.release \
             FROM issue_stats_hourly h \
             JOIN issues i ON i.id = h.issue_id AND i.app_id = $1 AND i.first_seen >= $2 \
             WHERE h.app_id = $1 AND h.hour >= date_trunc('hour', $2::timestamptz, 'UTC'){env_sql} \
             ORDER BY h.issue_id, h.hour, h.release \
         ) f \
         GROUP BY f.release"
    );
    let mut stmt = diesel::sql_query(sql)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Timestamptz, _>(coverage_from);
    stmt = crate::bind_env!(stmt, &scope.env);
    let rows: Vec<NewIssuesRow> = stmt.get_results(conn).await?;
    Ok(rows.into_iter().map(|r| (r.release, r.n)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: &str) -> DateTime<Utc> {
        s.parse().expect("ts")
    }

    fn irow(app: Uuid, env: Option<Uuid>, issue: Uuid, release: Option<&str>, at: &str) -> IRow {
        IRow {
            app_id: app,
            environment_id: env,
            issue_id: issue,
            release: release.map(str::to_string),
            occurred_at: ts(at),
            received_at: ts("2026-11-09T12:00:00Z"),
        }
    }

    /// Bucketing is the UTC hour start (both edges), a NULL release keys as
    /// '' (and stays distinct from a named one), and a NULL environment is
    /// its own cell.
    #[test]
    fn fold_issue_rows_buckets_by_utc_hour_env_and_release_key() {
        let (app, issue, env) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let rows = vec![
            irow(app, Some(env), issue, Some("1.0"), "2026-11-09T10:00:00Z"),
            irow(
                app,
                Some(env),
                issue,
                Some("1.0"),
                "2026-11-09T10:59:59.999999Z",
            ),
            irow(app, Some(env), issue, None, "2026-11-09T10:30:00Z"),
            irow(app, None, issue, None, "2026-11-09T10:30:00Z"),
            irow(app, Some(env), issue, Some("1.0"), "2026-11-09T11:00:00Z"),
            // A non-UTC offset still lands in its UTC hour.
            irow(
                app,
                Some(env),
                issue,
                Some("1.0"),
                "2026-11-09T16:15:00+05:30",
            ),
        ];
        let got = fold_issue_rows(&rows);
        let h10 = ts("2026-11-09T10:00:00Z");
        let h11 = ts("2026-11-09T11:00:00Z");
        let mut want = HashMap::new();
        want.insert((app, Some(env), issue, "1.0".to_string(), h10), 3);
        want.insert((app, Some(env), issue, String::new(), h10), 1);
        want.insert((app, None, issue, String::new(), h10), 1);
        want.insert((app, Some(env), issue, "1.0".to_string(), h11), 1);
        assert_eq!(got, want);
    }

    /// The upsert order follows `issue_stats_hourly_key` (app, hour, issue,
    /// release, env-or-nil), not the tuple order of `IssueHourKey`.
    #[test]
    fn conflict_order_follows_the_unique_index_columns() {
        let app = Uuid::new_v4();
        let (lo, hi) = {
            let (x, y) = (Uuid::new_v4(), Uuid::new_v4());
            if x < y {
                (x, y)
            } else {
                (y, x)
            }
        };
        let h10 = ts("2026-11-09T10:00:00Z");
        let h11 = ts("2026-11-09T11:00:00Z");
        // Earlier hour wins over a smaller issue id (tuple order would say
        // the opposite: issue precedes hour in IssueHourKey).
        let early: IssueHourKey = (app, None, hi, String::new(), h10);
        let late: IssueHourKey = (app, None, lo, String::new(), h11);
        assert!(conflict_order(&early) < conflict_order(&late));
        assert!(early > late);
        // A NULL environment sorts as the nil uuid, i.e. first.
        let null_env: IssueHourKey = (app, None, lo, String::new(), h10);
        let some_env: IssueHourKey = (app, Some(hi), lo, String::new(), h10);
        assert!(conflict_order(&null_env) < conflict_order(&some_env));
    }
}
