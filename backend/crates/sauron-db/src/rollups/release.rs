//! Release-keyed twins of the Users page's rollups (migration 82), so the
//! release switcher can narrow `/users/summary`, `/persons` and
//! `/counts/persons` without a raw scan.
//!
//! Design: `docs/superpowers/specs/2026-10-15-release-users-design.md`.
//!
//! Three tables, each a copy of an existing one plus a `release` column:
//!
//! * `person_releases` — `event_user_environments` + release. Written on the
//!   ingest path (`batch::bump_person_releases`, `repo::bump_person_release`),
//!   read here for the exact Total / Active / New legs and the persons list.
//! * `user_activity_release_daily` — `user_activity_daily`'s `hll_all` and
//!   `hll_identified` + release, folded by the same transactions as their base
//!   table ([`super::fold`]). DAU/WAU/MAU and the active-per-day chart.
//! * `session_stats_release_daily` — `session_stats_daily`'s duration columns
//!   + release, REPLACEd by `fold::recompute_sessions`.
//!
//! `release` is `''` for "Unknown release" (the telemetry tables' NULL;
//! `batch::release_key`) — see migration 82's header for why the sentinel.
//!
//! # Representative rows
//!
//! `person_releases` holds one row per (person, environment). Under a scope
//! that admits several environments a person can have several rows, and
//! grouping them before paging is O(release audience) on every request — the
//! shape the persons-page-first fix removed from the release-blind page. So
//! every read here instead picks ONE row per person with a `NOT EXISTS`
//! probe ([`rep_sql`]): the row holding the person's latest `last_seen`, or
//! earliest `first_seen`, or (for plain membership) the lowest environment.
//! That row's own column then IS the person's aggregate, so it can drive an
//! index range (`person_releases_{last,first}_seen_idx`) and a `LIMIT` stops
//! the scan after one page. Under a single-environment scope the probe is
//! omitted: every person has exactly one row there.
//!
//! Reads are gated by [`is_ready`] (backfill marker, or app created at/after
//! the release epoch). There is deliberately NO raw fallback: an unready app
//! answers 503 at the API, because the raw shape is the 30 s timeout these
//! tables exist to avoid.

use std::collections::{BTreeMap, HashSet};

use chrono::{DateTime, Duration, NaiveDate, Utc};
use diesel::sql_types::{
    Array, BigInt, Bytea, Date, Double, Nullable, Text, Timestamptz, Uuid as SqlUuid,
};
use diesel::{QueryResult, QueryableByName};
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use uuid::Uuid;

use super::read::{day_bucket, day_hlls, duration_sums, merged_hist, merged_hll, ratio, DayHllRow};
use super::{begin_locked, rollback_quietly, BoolRow, TsRow, CHUNK, NIL_ENV};
use crate::batch::release_key;
use crate::repo::{self, PersonRow, SeriesPoint, SortSpec, TimeWindow, UserSeriesPoint, UserStats};
use crate::scope::{EnvFilter, Range, ReadScope};
use crate::sketch::{Hll, LatencyHistogram};

const ACTIVITY_TABLE: &str = "user_activity_release_daily";
const SESSION_TABLE: &str = "session_stats_release_daily";

/// `(app, env-as-stored, release-key, day)`.
pub type ReleaseDayKey = (Uuid, Option<Uuid>, String, NaiveDate);

/// One fold's contribution to a `user_activity_release_daily` row. Both
/// sketches are always written (NOT NULL columns): unlike the base table there
/// is no pre-migration history whose identified split is unknown.
#[derive(Default, Clone)]
pub struct ReleaseActivityDelta {
    pub hll_all: Hll,
    pub hll_identified: Hll,
}

impl ReleaseActivityDelta {
    pub(crate) fn merge(&mut self, other: &ReleaseActivityDelta) {
        self.hll_all.merge(&other.hll_all);
        self.hll_identified.merge(&other.hll_identified);
    }
}

/// Record one person's analytics or error signal into the release sketches.
pub(crate) fn record(
    map: &mut BTreeMap<ReleaseDayKey, ReleaseActivityDelta>,
    k: ReleaseDayKey,
    distinct_id: &str,
    identified: bool,
) {
    let d = map.entry(k).or_default();
    d.hll_all.insert(distinct_id);
    if identified {
        d.hll_identified.insert(distinct_id);
    }
}

#[derive(QueryableByName)]
struct ExistingReleaseActivity {
    #[diesel(sql_type = SqlUuid)]
    app_id: Uuid,
    #[diesel(sql_type = SqlUuid)]
    env_key: Uuid,
    #[diesel(sql_type = Text)]
    release: String,
    #[diesel(sql_type = Date)]
    day: NaiveDate,
    #[diesel(sql_type = Bytea)]
    hll_all: Vec<u8>,
    #[diesel(sql_type = Bytea)]
    hll_identified: Vec<u8>,
}

/// `(app, env, release, day) -> delta`, merged with what is stored then
/// replaced — `add_user_activity`'s shape (sketches cannot merge in SQL).
/// Must run inside the module's advisory-locked transaction.
pub async fn add_release_activity(
    conn: &mut AsyncPgConnection,
    deltas: &mut BTreeMap<ReleaseDayKey, ReleaseActivityDelta>,
) -> diesel::QueryResult<()> {
    if deltas.is_empty() {
        return Ok(());
    }
    let keys: Vec<ReleaseDayKey> = deltas.keys().cloned().collect();
    for kchunk in keys.chunks(CHUNK) {
        let existing: Vec<ExistingReleaseActivity> = diesel::sql_query(
            "SELECT u.app_id, COALESCE(u.environment_id, '00000000-0000-0000-0000-000000000000'::uuid) AS env_key, \
                    u.release, u.day, u.hll_all, u.hll_identified \
             FROM user_activity_release_daily u \
             JOIN unnest($1::uuid[], $2::uuid[], $3::text[], $4::date[]) AS k(app_id, env_key, release, day) \
               ON u.app_id = k.app_id AND u.release = k.release AND u.day = k.day \
              AND COALESCE(u.environment_id, '00000000-0000-0000-0000-000000000000'::uuid) = k.env_key \
             FOR UPDATE OF u",
        )
        .bind::<Array<SqlUuid>, _>(kchunk.iter().map(|k| k.0).collect::<Vec<_>>())
        .bind::<Array<SqlUuid>, _>(
            kchunk
                .iter()
                .map(|k| k.1.unwrap_or(NIL_ENV))
                .collect::<Vec<_>>(),
        )
        .bind::<Array<Text>, _>(kchunk.iter().map(|k| k.2.clone()).collect::<Vec<_>>())
        .bind::<Array<Date>, _>(kchunk.iter().map(|k| k.3).collect::<Vec<_>>())
        .get_results(conn)
        .await?;
        for ex in existing {
            let env = (ex.env_key != NIL_ENV).then_some(ex.env_key);
            if let Some(d) = deltas.get_mut(&(ex.app_id, env, ex.release, ex.day)) {
                d.hll_all.merge(&Hll::from_opt(Some(&ex.hll_all)));
                d.hll_identified
                    .merge(&Hll::from_opt(Some(&ex.hll_identified)));
            }
        }
    }
    let rows: Vec<_> = deltas.iter().collect();
    for chunk in rows.chunks(CHUNK) {
        diesel::sql_query(
            "INSERT INTO user_activity_release_daily \
                 (app_id, environment_id, release, day, hll_all, hll_identified, updated_at) \
             SELECT app_id, env, release, day, hll_all, hll_identified, now() \
             FROM unnest($1::uuid[], $2::uuid[], $3::text[], $4::date[], $5::bytea[], $6::bytea[]) \
                  AS t(app_id, env, release, day, hll_all, hll_identified) \
             ON CONFLICT (app_id, release, day, COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid)) \
             DO UPDATE SET hll_all = EXCLUDED.hll_all, \
                           hll_identified = EXCLUDED.hll_identified, updated_at = now()",
        )
        .bind::<Array<SqlUuid>, _>(chunk.iter().map(|(k, _)| k.0).collect::<Vec<_>>())
        .bind::<Array<Nullable<SqlUuid>>, _>(chunk.iter().map(|(k, _)| k.1).collect::<Vec<_>>())
        .bind::<Array<Text>, _>(chunk.iter().map(|(k, _)| k.2.clone()).collect::<Vec<_>>())
        .bind::<Array<Date>, _>(chunk.iter().map(|(k, _)| k.3).collect::<Vec<_>>())
        .bind::<Array<Bytea>, _>(
            chunk
                .iter()
                .map(|(_, d)| d.hll_all.to_bytes())
                .collect::<Vec<_>>(),
        )
        .bind::<Array<Bytea>, _>(
            chunk
                .iter()
                .map(|(_, d)| d.hll_identified.to_bytes())
                .collect::<Vec<_>>(),
        )
        .execute(conn)
        .await?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

/// The columns of one `sessions` row the release twin needs — the backfill's
/// pull shape. The live recompute passes its own rows by reference instead
/// (see [`sessions_to_release_days`]).
#[derive(QueryableByName)]
pub(crate) struct SessionSample {
    #[diesel(sql_type = SqlUuid)]
    pub app_id: Uuid,
    #[diesel(sql_type = Nullable<SqlUuid>)]
    pub environment_id: Option<Uuid>,
    #[diesel(sql_type = Nullable<Text>)]
    pub release: Option<String>,
    #[diesel(sql_type = Timestamptz)]
    pub started_at: DateTime<Utc>,
    #[diesel(sql_type = Timestamptz)]
    pub last_event_at: DateTime<Utc>,
}

/// `(app, env, release, started_at, last_event_at)` of one session, borrowed.
pub(crate) type SessionRef<'a> = (
    Uuid,
    Option<Uuid>,
    Option<&'a str>,
    DateTime<Utc>,
    DateTime<Utc>,
);

/// One recomputed (app, env, release, started-day) bucket — REPLACE, not add.
pub struct SessionReleaseDayRow {
    pub app: Uuid,
    pub env: Option<Uuid>,
    pub release: String,
    pub day: NaiveDate,
    pub sessions: i64,
    pub duration_ms_sum: f64,
    pub hist: LatencyHistogram,
}

/// Bucket sessions by started day and by the session row's `release` — the
/// base table's `session_rows_to_days` with one more key column and only the
/// duration columns the Users page reads.
///
/// NOTE what that `release` is: `bump_sessions` / `repo::bump_session` keep the
/// LATEST non-null release a session reported (`COALESCE(new, old)`), so a
/// session that spans an app update is bucketed under the newer release. The
/// persons list's per-release `sessions_count` credits the release the session
/// was CREATED under instead; the two differ only for such sessions. Disclosed
/// in docs/approximate-analytics.md.
pub(crate) fn sessions_to_release_days<'a>(
    rows: impl Iterator<Item = SessionRef<'a>>,
) -> Vec<SessionReleaseDayRow> {
    let mut map: BTreeMap<(Uuid, Option<Uuid>, String, NaiveDate), SessionReleaseDayRow> =
        BTreeMap::new();
    for (app, env, release, started, last) in rows {
        let k = (app, env, release_key(release), started.date_naive());
        let e = map
            .entry(k.clone())
            .or_insert_with(|| SessionReleaseDayRow {
                app: k.0,
                env: k.1,
                release: k.2.clone(),
                day: k.3,
                sessions: 0,
                duration_ms_sum: 0.0,
                hist: LatencyHistogram::new(),
            });
        let ms = ((last - started).num_milliseconds() as f64).max(0.0);
        e.sessions += 1;
        e.duration_ms_sum += ms;
        e.hist.record(ms);
    }
    map.into_values().collect()
}

/// REPLACE every dirty (app, day)'s release buckets: delete, then insert the
/// caller's full recomputed set — `replace_session_days`' contract.
pub async fn replace_session_release_days(
    conn: &mut AsyncPgConnection,
    dirty: &[(Uuid, NaiveDate)],
    rows: &[SessionReleaseDayRow],
) -> diesel::QueryResult<()> {
    for chunk in dirty.chunks(CHUNK * 4) {
        diesel::sql_query(
            "DELETE FROM session_stats_release_daily s \
             USING unnest($1::uuid[], $2::date[]) AS d(app_id, day) \
             WHERE s.app_id = d.app_id AND s.day = d.day",
        )
        .bind::<Array<SqlUuid>, _>(chunk.iter().map(|(a, _)| *a).collect::<Vec<_>>())
        .bind::<Array<Date>, _>(chunk.iter().map(|(_, d)| *d).collect::<Vec<_>>())
        .execute(conn)
        .await?;
    }
    for chunk in rows.chunks(CHUNK) {
        diesel::sql_query(
            "INSERT INTO session_stats_release_daily \
                 (app_id, environment_id, release, day, sessions, duration_ms_sum, duration_hist, updated_at) \
             SELECT app_id, env, release, day, sessions, dsum, dhist, now() \
             FROM unnest($1::uuid[], $2::uuid[], $3::text[], $4::date[], $5::bigint[], $6::float8[], $7::bytea[]) \
                  AS t(app_id, env, release, day, sessions, dsum, dhist) \
             ON CONFLICT (app_id, release, day, COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid)) \
             DO UPDATE SET sessions = EXCLUDED.sessions, duration_ms_sum = EXCLUDED.duration_ms_sum, \
                           duration_hist = EXCLUDED.duration_hist, updated_at = now()",
        )
        .bind::<Array<SqlUuid>, _>(chunk.iter().map(|r| r.app).collect::<Vec<_>>())
        .bind::<Array<Nullable<SqlUuid>>, _>(chunk.iter().map(|r| r.env).collect::<Vec<_>>())
        .bind::<Array<Text>, _>(chunk.iter().map(|r| r.release.clone()).collect::<Vec<_>>())
        .bind::<Array<Date>, _>(chunk.iter().map(|r| r.day).collect::<Vec<_>>())
        .bind::<Array<BigInt>, _>(chunk.iter().map(|r| r.sessions).collect::<Vec<_>>())
        .bind::<Array<Double>, _>(chunk.iter().map(|r| r.duration_ms_sum).collect::<Vec<_>>())
        .bind::<Array<Bytea>, _>(chunk.iter().map(|r| r.hist.to_bytes()).collect::<Vec<_>>())
        .execute(conn)
        .await?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Gate
// ---------------------------------------------------------------------------

/// Whether the release-scoped Users reads may answer for this app: its
/// backfill marker exists, or it was created at/after the release epoch (so
/// every row it will ever have was written by a release-aware binary) — the
/// `rollups::is_ready` shape.
pub async fn is_ready(conn: &mut AsyncPgConnection, app_id: Uuid) -> QueryResult<bool> {
    let r: BoolRow = diesel::sql_query(
        "SELECT EXISTS (SELECT 1 FROM release_rollup_backfill WHERE app_id = $1) \
             OR EXISTS (SELECT 1 FROM apps a, release_rollup_epoch e \
                        WHERE a.id = $1 AND a.created_at >= e.started_at) AS present",
    )
    .bind::<SqlUuid, _>(app_id)
    .get_result(conn)
    .await?;
    Ok(r.present)
}

/// The instant migration 82 applied — when the live path started counting
/// `person_releases`. The backfill adds only signals strictly before it.
pub async fn epoch(conn: &mut AsyncPgConnection) -> QueryResult<DateTime<Utc>> {
    let r: TsRow = diesel::sql_query("SELECT started_at AS t FROM release_rollup_epoch")
        .get_result(conn)
        .await?;
    Ok(r.t)
}

/// Whether an app that predates the release epoch still lacks its marker.
pub async fn backfill_pending(conn: &mut AsyncPgConnection) -> QueryResult<bool> {
    let r: BoolRow = diesel::sql_query(
        "SELECT EXISTS (SELECT 1 FROM apps a, release_rollup_epoch e \
                        WHERE a.created_at < e.started_at \
                          AND NOT EXISTS (SELECT 1 FROM release_rollup_backfill b WHERE b.app_id = a.id)) \
                AS present",
    )
    .get_result(conn)
    .await?;
    Ok(r.present)
}

// ---------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------

/// Which of a person's rows on one release stands for them — see the module
/// docs' "Representative rows".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rep {
    /// The row with the person's latest `last_seen`.
    LastSeen,
    /// The row with the person's earliest `first_seen`.
    FirstSeen,
    /// Any one row (membership only).
    Any,
}

impl Rep {
    /// The column whose order this representative preserves, on alias `r`.
    fn column(self) -> &'static str {
        match self {
            Rep::LastSeen => "r.last_seen",
            Rep::FirstSeen => "r.first_seen",
            Rep::Any => "r.distinct_id",
        }
    }
}

/// ` AND NOT EXISTS (…)` keeping only `r`, alias of `person_releases`, when it
/// is `rep` for its person among the rows `env` admits (env bound at
/// `env_idx`); ties on the column fall to the lowest environment, so exactly
/// one row survives. Empty under a single-environment scope, where every
/// person has one row. The probe is a prefix of `person_releases_key`.
fn rep_sql(rep: Rep, env: &EnvFilter, env_idx: usize) -> String {
    if matches!(env, EnvFilter::One(_) | EnvFilter::Unattributed) {
        return String::new();
    }
    const NIL: &str = "'00000000-0000-0000-0000-000000000000'::uuid";
    let lower_env =
        format!("COALESCE(o.environment_id, {NIL}) < COALESCE(r.environment_id, {NIL})");
    let beats = match rep {
        Rep::LastSeen => {
            format!("(o.last_seen > r.last_seen OR (o.last_seen = r.last_seen AND {lower_env}))")
        }
        Rep::FirstSeen => format!(
            "(o.first_seen < r.first_seen OR (o.first_seen = r.first_seen AND {lower_env}))"
        ),
        Rep::Any => lower_env,
    };
    let o_env = env.sql_fragment_for("o", env_idx);
    format!(
        " AND NOT EXISTS (SELECT 1 FROM person_releases o \
           WHERE o.app_id = r.app_id AND o.release = r.release \
             AND o.distinct_id = r.distinct_id{o_env} AND {beats})"
    )
}

/// `col` within `[$from_idx, $to_idx)`, the upper bound optional.
fn window_sql(col: &str, from_idx: usize, to_idx: usize) -> String {
    format!("{col} >= ${from_idx} AND (${to_idx}::timestamptz IS NULL OR {col} < ${to_idx})")
}

const IDENTIFIED: &str = " AND EXISTS (SELECT 1 FROM event_users eu \
     WHERE eu.app_id = r.app_id AND eu.distinct_id = r.distinct_id AND eu.identified_at IS NOT NULL)";

#[derive(QueryableByName)]
struct Legs {
    #[diesel(sql_type = BigInt)]
    total_users: i64,
    #[diesel(sql_type = BigInt)]
    active_in_range: i64,
    #[diesel(sql_type = BigInt)]
    new_in_range: i64,
    #[diesel(sql_type = BigInt)]
    total_identified: i64,
    #[diesel(sql_type = BigInt)]
    active_identified: i64,
    #[diesel(sql_type = BigInt)]
    new_identified: i64,
}

/// `rollups::read::user_stats` for one release. Total / Active / New and their
/// identified splits are exact over `person_releases` (first/last seen ON this
/// release); dau/wau/mau are the release sketches; the session legs read the
/// release session buckets.
///
/// Cost: Active and New walk only the window's slice of
/// `person_releases_{last,first}_seen_idx` (their representative rows ARE the
/// person's max / min); Total is O(people on the release) — the same class as
/// the release-blind page's `count(*)` over `event_users`.
pub async fn user_stats(
    conn: &mut AsyncPgConnection,
    scope: &ReadScope,
    release: &str,
    range: Range,
    now: DateTime<Utc>,
) -> QueryResult<UserStats> {
    // $1 app, $2 release, $3 from, $4 to (nullable), env at $5.
    let env = scope.env.sql_fragment_for("r", 5);
    let leg = |rep: Rep, window: bool, identified: bool| {
        let w = if window {
            format!(" AND {}", window_sql(rep.column(), 3, 4))
        } else {
            String::new()
        };
        let rep_sql = rep_sql(rep, &scope.env, 5);
        let idf = if identified { IDENTIFIED } else { "" };
        format!(
            "(SELECT count(*) FROM person_releases r \
              WHERE r.app_id=$1 AND r.release=$2{env}{w}{rep_sql}{idf})::bigint"
        )
    };
    let q = format!(
        "SELECT {} AS total_users, {} AS active_in_range, {} AS new_in_range, \
                {} AS total_identified, {} AS active_identified, {} AS new_identified",
        leg(Rep::Any, false, false),
        leg(Rep::LastSeen, true, false),
        leg(Rep::FirstSeen, true, false),
        leg(Rep::Any, false, true),
        leg(Rep::LastSeen, true, true),
        leg(Rep::FirstSeen, true, true),
    );
    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Text, _>(release.to_string())
        .bind::<Timestamptz, _>(range.from)
        .bind::<Nullable<Timestamptz>, _>(range.to);
    stmt = crate::bind_env!(stmt, &scope.env);
    let legs: Legs = stmt.get_result(conn).await?;

    // dau/wau/mau: the base twin's calendar-day spans over the release sketches.
    let today = now.date_naive();
    let lo = today - Duration::days(29);
    let all = day_hlls(
        conn,
        ACTIVITY_TABLE,
        scope,
        Some(release),
        lo,
        today,
        "hll_all",
    )
    .await?;
    let idf = day_hlls(
        conn,
        ACTIVITY_TABLE,
        scope,
        Some(release),
        lo,
        today,
        "hll_identified",
    )
    .await?;
    let spans = |rows: &[DayHllRow]| {
        let (mut d, mut w, mut m) = (Hll::new(), Hll::new(), Hll::new());
        for r in rows {
            let h = merged_hll(&r.hlls);
            if r.day == today {
                d.merge(&h);
            }
            if r.day > today - Duration::days(7) {
                w.merge(&h);
            }
            m.merge(&h);
        }
        (d.estimate(), w.estimate(), m.estimate())
    };
    let (dau, wau, mau) = spans(&all);
    let (dau_i, wau_i, mau_i) = spans(&idf);

    let s = duration_sums(conn, SESSION_TABLE, scope, Some(release), range).await?;
    Ok(UserStats {
        total_users: legs.total_users,
        active_in_range: legs.active_in_range,
        new_in_range: legs.new_in_range,
        total_identified: legs.total_identified,
        active_identified: legs.active_identified,
        new_identified: legs.new_identified,
        dau,
        wau,
        mau,
        dau_identified: Some(dau_i.min(dau)),
        wau_identified: Some(wau_i.min(wau)),
        mau_identified: Some(mau_i.min(mau)),
        avg_session_ms: ratio(s.dsum, s.sessions),
        median_session_ms: merged_hist(&s.hists).percentile(0.5),
    })
}

/// `rollups::read::active_user_series` for one release: per-day actives from
/// the release sketches, per-day new = people whose FIRST signal on this
/// release falls on that day (their `FirstSeen` representative rows, read off
/// the window's slice of `person_releases_first_seen_idx`).
pub async fn active_user_series(
    conn: &mut AsyncPgConnection,
    scope: &ReadScope,
    release: &str,
    range: Range,
) -> QueryResult<Vec<UserSeriesPoint>> {
    let (lo, hi) = super::read::day_bounds(&range);
    let rows = day_hlls(
        conn,
        ACTIVITY_TABLE,
        scope,
        Some(release),
        lo,
        hi,
        "hll_all",
    )
    .await?;
    let active: Vec<SeriesPoint> = rows
        .iter()
        .map(|r| SeriesPoint {
            bucket: day_bucket(r.day),
            count: merged_hll(&r.hlls).estimate(),
        })
        .collect();

    let env = scope.env.sql_fragment_for("r", 5);
    let window = window_sql("r.first_seen", 3, 4);
    let rep = rep_sql(Rep::FirstSeen, &scope.env, 5);
    let q = format!(
        "SELECT date_trunc('day', r.first_seen) AS bucket, count(*)::bigint AS count \
         FROM person_releases r \
         WHERE r.app_id=$1 AND r.release=$2{env} AND {window}{rep} \
         GROUP BY bucket ORDER BY bucket"
    );
    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Text, _>(release.to_string())
        .bind::<Timestamptz, _>(range.from)
        .bind::<Nullable<Timestamptz>, _>(range.to);
    stmt = crate::bind_env!(stmt, &scope.env);
    let new: Vec<SeriesPoint> = stmt.get_results(conn).await?;
    Ok(repo::merge_user_series(active, new))
}

/// Which people a release-scoped persons list admits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseMode {
    /// Anyone with a signal on the release (in the env scope).
    Used,
    /// Only people whose MOST RECENT signal (in the env scope) is on the
    /// release — "who is on it now". A tie with another release keeps both.
    Current,
}

/// The representative that pages `sort` page-first, or `None` for the count
/// sorts, whose key does not exist until a person's rows are summed.
fn paged_rep(sort: &SortSpec) -> Option<Rep> {
    match sort.column {
        "last_seen" => Some(Rep::LastSeen),
        "first_seen" => Some(Rep::FirstSeen),
        "eu.distinct_id" => Some(Rep::Any),
        _ => None,
    }
}

/// The release-scoped persons list SQL. Bind order: `$1` app, `$2` release,
/// `$3` limit, `$4` offset, `$5` search pattern, env at `$6` when it consumes
/// a bind, then the window's `from` / `to`.
///
/// Keeps `list_persons_rollup_sql`'s output contract: `eu` is the person alias
/// (the sort tiebreak is `eu.distinct_id`) and the five displayed columns keep
/// their names; first/last seen and the counts are per-release, summed across
/// the environments `env` admits by one `LATERAL` probe per paged person.
///
/// Two shapes, the base page's split:
///
/// * a durable sort (`last_seen`, `first_seen`, `distinct_id`) walks the
///   sort's representative rows in index order, so `LIMIT` ends the scan after
///   one page — O(page), however large the release's audience;
/// * a count sort (`events_count`, …) has no key before the sums, so it groups
///   the release's rows first and sorts — O(people on the release), the same
///   trade the release-blind page makes for those sorts.
pub fn persons_list_sql(
    env: &EnvFilter,
    mode: ReleaseMode,
    sort: &SortSpec,
    window_column: &'static str,
    has_search: bool,
) -> String {
    let env_r = env.sql_fragment_for("r", 6);
    let env_p = env.sql_fragment_for("p", 6);
    let from_idx = if env.consumes_bind() { 7 } else { 6 };
    let search = if has_search {
        "(eu.distinct_id ILIKE $5 OR eu.properties::text ILIKE $5)"
    } else {
        "eu.distinct_id ILIKE $5"
    };
    let window = window_sql(&format!("a.{window_column}"), from_idx, from_idx + 1);
    let current = match mode {
        ReleaseMode::Used => String::new(),
        ReleaseMode::Current => {
            let o_env = env.sql_fragment_for("o", 6);
            format!(
                " AND NOT EXISTS (SELECT 1 FROM person_releases o \
                   WHERE o.app_id = $1 AND o.distinct_id = a.distinct_id \
                     AND o.release <> $2{o_env} AND o.last_seen > a.last_seen)"
            )
        }
    };
    let select = "SELECT eu.distinct_id, eu.properties, a.first_seen AS first_seen, \
                         a.last_seen AS last_seen, a.events_count AS events_count, \
                         a.errors_count AS errors_count, a.sessions_count AS sessions_count";
    let sums = "min(p.first_seen) AS first_seen, max(p.last_seen) AS last_seen, \
                sum(p.events_count)::bigint AS events_count, \
                sum(p.errors_count)::bigint AS errors_count, \
                sum(p.sessions_count)::bigint AS sessions_count";
    match paged_rep(sort) {
        Some(rep) => {
            let rep_sql = rep_sql(rep, env, 6);
            // ORDER BY the representative's OWN column, not the `first_seen` /
            // `last_seen` output aliases (the LATERAL's aggregates): only the
            // former is an index order the scan can stop early on. Equal by
            // construction, so the result order is the same.
            let dir = if sort.descending { "DESC" } else { "ASC" };
            let col = rep.column();
            // The same window again on `r` when it is the representative's own
            // column (equal to `a`'s there): an index RANGE, so a count — or a
            // page past the window's last person — stops at the window's edge
            // instead of walking the rest of the release.
            let r_window = if col == format!("r.{window_column}") {
                format!(" AND {}", window_sql(col, from_idx, from_idx + 1))
            } else {
                String::new()
            };
            format!(
                "{select} \
                 FROM person_releases r \
                 JOIN event_users eu ON eu.app_id = r.app_id AND eu.distinct_id = r.distinct_id \
                 CROSS JOIN LATERAL ( \
                     SELECT p.distinct_id, {sums} FROM person_releases p \
                     WHERE p.app_id = r.app_id AND p.release = r.release \
                       AND p.distinct_id = r.distinct_id{env_p} \
                     GROUP BY p.distinct_id \
                 ) a \
                 WHERE r.app_id=$1 AND r.release=$2{env_r}{r_window}{rep_sql} \
                   AND {search} AND {window}{current} \
                 ORDER BY {col} {dir}, eu.distinct_id ASC \
                 LIMIT $3 OFFSET $4"
            )
        }
        None => {
            let order_by = sort.order_by();
            format!(
                "{select} \
                 FROM ( \
                     SELECT p.app_id, p.distinct_id, {sums} FROM person_releases p \
                     WHERE p.app_id=$1 AND p.release=$2{env_p} \
                     GROUP BY p.app_id, p.distinct_id \
                 ) a \
                 JOIN event_users eu ON eu.app_id = a.app_id AND eu.distinct_id = a.distinct_id \
                 WHERE {search} AND {window}{current} \
                 ORDER BY {order_by} \
                 LIMIT $3 OFFSET $4"
            )
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn list_persons(
    conn: &mut AsyncPgConnection,
    scope: &ReadScope,
    release: &str,
    mode: ReleaseMode,
    search: Option<&str>,
    limit: i64,
    offset: i64,
    sort: SortSpec,
    window: TimeWindow,
) -> QueryResult<Vec<PersonRow>> {
    let q = persons_list_sql(&scope.env, mode, &sort, window.column, search.is_some());
    let pattern = search
        .map(repo::like_contains)
        .unwrap_or_else(|| "%".to_string());
    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Text, _>(release.to_string())
        .bind::<BigInt, _>(limit)
        .bind::<BigInt, _>(offset)
        .bind::<Text, _>(pattern);
    stmt = crate::bind_env!(stmt, &scope.env);
    stmt.bind::<Timestamptz, _>(window.from)
        .bind::<Nullable<Timestamptz>, _>(window.to)
        .get_results(conn)
        .await
}

/// `(total, capped)` over the people [`list_persons`] pages: the list's own SQL
/// wrapped with `limit` = `cap + 1`, so the two never disagree. Always built
/// with the WINDOW column's durable sort, whatever the list is sorted by — a
/// count does not depend on order, and that shape walks only the window's
/// slice of the index and stops at the cap.
#[allow(clippy::too_many_arguments)]
pub async fn count_persons(
    conn: &mut AsyncPgConnection,
    scope: &ReadScope,
    release: &str,
    mode: ReleaseMode,
    search: Option<&str>,
    sort: SortSpec,
    window: TimeWindow,
    cap: i64,
) -> QueryResult<(i64, bool)> {
    let paged = SortSpec {
        column: window.column,
        descending: true,
        ..sort
    };
    let inner = persons_list_sql(&scope.env, mode, &paged, window.column, search.is_some());
    let q = format!("SELECT count(*)::bigint AS total FROM ({inner}) c");
    let pattern = search
        .map(repo::like_contains)
        .unwrap_or_else(|| "%".to_string());
    let mut stmt = diesel::sql_query(q)
        .into_boxed()
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Text, _>(release.to_string())
        .bind::<BigInt, _>(cap + 1)
        .bind::<BigInt, _>(0i64)
        .bind::<Text, _>(pattern);
    stmt = crate::bind_env!(stmt, &scope.env);
    let row: repo::BoundedCount = stmt
        .bind::<Timestamptz, _>(window.from)
        .bind::<Nullable<Timestamptz>, _>(window.to)
        .get_result(conn)
        .await?;
    Ok(repo::capped(row.total, cap))
}

// ---------------------------------------------------------------------------
// Backfill
// ---------------------------------------------------------------------------

#[derive(QueryableByName)]
struct DistinctSignal {
    #[diesel(sql_type = SqlUuid)]
    app_id: Uuid,
    #[diesel(sql_type = Nullable<SqlUuid>)]
    environment_id: Option<Uuid>,
    #[diesel(sql_type = Text)]
    release: String,
    #[diesel(sql_type = Text)]
    distinct_id: String,
}

/// Days the sketch + session pass still has to cover, oldest first: every day
/// the base rollups know about (they cover all of Postgres-resident history
/// once `backfill-rollups` has run — see [`backfill_all`]'s precondition)
/// that has no row in the resume cursor yet.
pub async fn days_pending(conn: &mut AsyncPgConnection) -> QueryResult<Vec<NaiveDate>> {
    #[derive(QueryableByName)]
    struct D {
        #[diesel(sql_type = Date)]
        day: NaiveDate,
    }
    let rows: Vec<D> = diesel::sql_query(
        "SELECT day FROM ( \
             SELECT DISTINCT day FROM user_activity_daily \
             UNION SELECT DISTINCT day FROM session_stats_daily \
         ) d \
         WHERE day <= (now() AT TIME ZONE 'UTC')::date \
           AND NOT EXISTS (SELECT 1 FROM release_rollup_backfill_days b WHERE b.day = d.day) \
         ORDER BY day",
    )
    .get_results(conn)
    .await?;
    Ok(rows.into_iter().map(|r| r.day).collect())
}

/// Fold one UTC day of raw rows into the release sketches and REPLACE its
/// release session buckets, recording the day in the resume cursor in the
/// SAME advisory-locked transaction.
///
/// Sketch merge is idempotent, so this may overlap rows the live fold has
/// already folded without inflating anything — which is what lets the
/// backfill cover the gap between migration 82 and the first release-aware
/// fold without an epoch split. Reads `DISTINCT (app, env, release, person)`
/// rather than every event: one row per active person-day, not per event.
pub async fn backfill_day(conn: &mut AsyncPgConnection, day: NaiveDate) -> QueryResult<()> {
    begin_locked(conn).await?;
    let out = backfill_day_body(conn, day).await;
    match out {
        Ok(()) => {
            use diesel_async::SimpleAsyncConnection;
            conn.batch_execute("COMMIT").await
        }
        Err(e) => {
            rollback_quietly(conn).await;
            Err(e)
        }
    }
}

async fn backfill_day_body(conn: &mut AsyncPgConnection, day: NaiveDate) -> QueryResult<()> {
    let lo = day.and_hms_opt(0, 0, 0).expect("valid").and_utc();
    let hi = lo + Duration::days(1);
    let mut deltas: BTreeMap<ReleaseDayKey, ReleaseActivityDelta> = BTreeMap::new();
    for table in ["analytics_events", "error_events"] {
        let rows: Vec<DistinctSignal> = diesel::sql_query(format!(
            "SELECT DISTINCT app_id, environment_id, COALESCE(release, '') AS release, distinct_id \
             FROM {table} \
             WHERE occurred_at >= $1 AND occurred_at < $2 \
               AND distinct_id IS NOT NULL AND distinct_id <> ''"
        ))
        .bind::<Timestamptz, _>(lo)
        .bind::<Timestamptz, _>(hi)
        .get_results(conn)
        .await?;
        let identified: HashSet<(Uuid, String)> = super::fold::load_identified(
            conn,
            rows.iter().map(|r| (r.app_id, r.distinct_id.as_str())),
        )
        .await?;
        for r in &rows {
            let is_id = identified.contains(&(r.app_id, r.distinct_id.clone()));
            record(
                &mut deltas,
                (r.app_id, r.environment_id, r.release.clone(), day),
                &r.distinct_id,
                is_id,
            );
        }
    }
    add_release_activity(conn, &mut deltas).await?;

    // Sessions: REPLACE the day's buckets — unless session retention has
    // dropped the day, `recompute_sessions`' clamp: the partition is gone, so
    // the only rows left are strays in `sessions_default`, and a bucket built
    // from strays alone would be a confident wrong number.
    let dropped = super::fold::sessions_dropped_floor(conn)
        .await?
        .is_some_and(|floor| day < floor.date_naive());
    if !dropped {
        let sessions: Vec<SessionSample> = diesel::sql_query(
            "SELECT app_id, environment_id, release, started_at, last_event_at \
             FROM sessions WHERE started_at >= $1 AND started_at < $2",
        )
        .bind::<Timestamptz, _>(lo)
        .bind::<Timestamptz, _>(hi)
        .get_results(conn)
        .await?;
        let rows = sessions_to_release_days(sessions.iter().map(|s| {
            (
                s.app_id,
                s.environment_id,
                s.release.as_deref(),
                s.started_at,
                s.last_event_at,
            )
        }));
        let mut dirty: Vec<(Uuid, NaiveDate)> = rows.iter().map(|r| (r.app, r.day)).collect();
        dirty.sort();
        dirty.dedup();
        replace_session_release_days(conn, &dirty, &rows).await?;
    }

    diesel::sql_query(
        "INSERT INTO release_rollup_backfill_days (day) VALUES ($1) ON CONFLICT DO NOTHING",
    )
    .bind::<Date, _>(day)
    .execute(conn)
    .await?;
    Ok(())
}

/// Aggregate one app's history into `person_releases` and mark it ready, in
/// one transaction (the marker must never be visible before its data).
///
/// Two clocks, deliberately:
///
/// * membership and `first_seen`/`last_seen` take EVERY raw row. `LEAST` /
///   `GREATEST` are idempotent, so overlapping what the live path already
///   wrote is harmless — and it closes the window between migration 82 and
///   the first release-aware ingest binary, in which an old binary wrote
///   events nothing counted.
/// * the three counters add only signals strictly BEFORE `cutoff` (the
///   release epoch): the live path has counted everything after it, so the
///   two sets are disjoint and the addition is exact — the
///   `person_env_backfill` rule, never `DO NOTHING`.
pub async fn backfill_app_persons(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    cutoff: DateTime<Utc>,
) -> QueryResult<usize> {
    use diesel_async::SimpleAsyncConnection;
    conn.batch_execute("BEGIN").await?;
    let r = async {
        let n = diesel::sql_query(
            "INSERT INTO person_releases \
               (app_id, environment_id, release, distinct_id, first_seen, last_seen, \
                events_count, errors_count, sessions_count) \
             SELECT $1, environment_id, rel, distinct_id, min(first_at), max(last_at), \
                    sum(ev), sum(er), sum(se) \
             FROM ( \
                 SELECT environment_id, COALESCE(release, '') AS rel, distinct_id, \
                        occurred_at AS first_at, occurred_at AS last_at, \
                        (occurred_at < $2)::int::bigint AS ev, 0::bigint AS er, 0::bigint AS se \
                 FROM analytics_events WHERE app_id = $1 AND distinct_id <> '' \
                 UNION ALL \
                 SELECT environment_id, COALESCE(release, ''), distinct_id, occurred_at, occurred_at, \
                        0::bigint, (occurred_at < $2)::int::bigint, 0::bigint \
                 FROM error_events WHERE app_id = $1 AND distinct_id <> '' \
                 UNION ALL \
                 SELECT environment_id, COALESCE(release, ''), distinct_id, occurred_at, occurred_at, \
                        0::bigint, 0::bigint, 0::bigint \
                 FROM transactions WHERE app_id = $1 AND distinct_id <> '' \
                 UNION ALL \
                 SELECT environment_id, COALESCE(release, ''), distinct_id, started_at, last_event_at, \
                        0::bigint, 0::bigint, (started_at < $2)::int::bigint \
                 FROM sessions WHERE app_id = $1 AND distinct_id IS NOT NULL AND distinct_id <> '' \
             ) t \
             GROUP BY environment_id, rel, distinct_id \
             ON CONFLICT (app_id, release, distinct_id, \
                          COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid)) \
             DO UPDATE SET \
                first_seen = LEAST(person_releases.first_seen, EXCLUDED.first_seen), \
                last_seen = GREATEST(person_releases.last_seen, EXCLUDED.last_seen), \
                events_count = person_releases.events_count + EXCLUDED.events_count, \
                errors_count = person_releases.errors_count + EXCLUDED.errors_count, \
                sessions_count = person_releases.sessions_count + EXCLUDED.sessions_count, \
                updated_at = now()",
        )
        .bind::<SqlUuid, _>(app_id)
        .bind::<Timestamptz, _>(cutoff)
        .execute(conn)
        .await?;
        diesel::sql_query(
            "INSERT INTO release_rollup_backfill (app_id, completed_at) VALUES ($1, now()) \
             ON CONFLICT (app_id) DO UPDATE SET completed_at = now()",
        )
        .bind::<SqlUuid, _>(app_id)
        .execute(conn)
        .await?;
        Ok(n)
    }
    .await;
    match r {
        Ok(n) => {
            conn.batch_execute("COMMIT").await?;
            Ok(n)
        }
        Err(e) => {
            let _ = conn.batch_execute("ROLLBACK").await;
            Err(e)
        }
    }
}

/// Result of a [`backfill_all`] run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackfillOutcome {
    Completed,
    /// `on_day` asked to stop; finished days are recorded and the next run
    /// resumes at the first unrecorded one. No marker was written.
    Interrupted,
}

/// The whole release backfill: the resumable day pass, then `person_releases`
/// for every app without a marker (one transaction each, marker inside).
///
/// PRECONDITION: the base rollups are backfilled (`rollups::backfill_pending`
/// is false). The day list is read off `user_activity_daily` /
/// `session_stats_daily`, which only cover pre-epoch history after
/// `backfill-rollups`; run before it and pre-epoch days would be skipped and
/// then marked done. `auto_backfill` orders the steps; the CLI refuses.
pub async fn backfill_all(
    conn: &mut AsyncPgConnection,
    mut on_day: impl FnMut(NaiveDate) -> bool,
) -> QueryResult<BackfillOutcome> {
    for day in days_pending(conn).await? {
        backfill_day(conn, day).await?;
        if !on_day(day) {
            return Ok(BackfillOutcome::Interrupted);
        }
    }
    let cutoff = epoch(conn).await?;
    #[derive(QueryableByName)]
    struct AppId {
        #[diesel(sql_type = SqlUuid)]
        id: Uuid,
    }
    // Every app without a marker — post-epoch apps too: they are ready by
    // creation date already, but the membership pass is what repairs any of
    // their rows an older binary wrote before the upgrade, and it is cheap.
    let apps: Vec<AppId> = diesel::sql_query(
        "SELECT id FROM apps WHERE id NOT IN (SELECT app_id FROM release_rollup_backfill) ORDER BY id",
    )
    .get_results(conn)
    .await?;
    for a in apps {
        let n = backfill_app_persons(conn, a.id, cutoff).await?;
        tracing::info!(app_id = %a.id, rows = n, "release users backfill: app done");
    }
    Ok(BackfillOutcome::Completed)
}
