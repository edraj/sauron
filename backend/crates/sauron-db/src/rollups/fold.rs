//! The fold: raw firehose rows → rollup deltas, incrementally and exactly
//! once.
//!
//! Selection is by `received_at` behind a per-source watermark (BRIN-served);
//! bucketing is by `occurred_at`, so a late event lands in its correct
//! historical bucket. Two aggregate shapes need memory across folds and carry
//! it in state tables: a screen view's dwell ends at the session's NEXT
//! analytics event (`rollup_session_state`), and a user's first-10-of-day
//! journey position spans folds (`rollup_journey_state`).
//!
//! The pure per-row logic lives in `fold_*_rows` functions that take plain
//! slices and state maps — unit-tested without Postgres, then exercised
//! against the real schema by `tests/rollup_equivalence.rs`.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Duration, DurationRound, NaiveDate, TimeZone, Utc};
use diesel::sql_types::{
    Array, BigInt, Date, Nullable, SmallInt, Text, Timestamptz, Uuid as SqlUuid,
};
use diesel::QueryableByName;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use uuid::Uuid;

use super::{
    add_event_top, add_journey_links, add_journey_nodes, add_perf_agg, add_screen_stats,
    add_user_activity, begin_locked, env_key, replace_session_days, rollback_quietly,
    set_watermark, watermark, DayKey, PerfDelta, ScreenDelta, SessionDayRow, UserActivityDelta,
    OTHER_NAME, SRC_ANALYTICS, SRC_ERRORS, SRC_SESSIONS, SRC_TRANSACTIONS,
};
use crate::sketch::LatencyHistogram;

/// The synthetic screen-view event the mobile SDKs emit.
const SCREEN_EVENT: &str = "$screen";
/// Dwell cap, mirroring repo::screen_ctes' LEAST(raw_ms, 1800000).
const DWELL_CAP_MS: f64 = 1_800_000.0;
/// Journey depth ceiling — the API's own max `depth` is 10.
const JOURNEY_MAX_STEPS: i16 = 10;
/// Rows per fold transaction; a longer backlog is drained by looping.
pub const FOLD_MAX_ROWS: usize = 500_000;

pub struct FoldOutcome {
    pub rows_read: usize,
    pub new_watermark: DateTime<Utc>,
    /// False when the pull hit [`FOLD_MAX_ROWS`] — call again to keep draining.
    pub caught_up: bool,
}

// ---------------------------------------------------------------------------
// Raw-row shapes pulled from the firehose tables.
// ---------------------------------------------------------------------------

#[derive(QueryableByName, Clone)]
pub(crate) struct ARow {
    #[diesel(sql_type = SqlUuid)]
    pub app_id: Uuid,
    #[diesel(sql_type = Nullable<SqlUuid>)]
    pub environment_id: Option<Uuid>,
    #[diesel(sql_type = Timestamptz)]
    pub occurred_at: DateTime<Utc>,
    #[diesel(sql_type = Timestamptz)]
    pub received_at: DateTime<Utc>,
    #[diesel(sql_type = Text)]
    pub name: String,
    #[diesel(sql_type = Nullable<Text>)]
    pub screen: Option<String>,
    #[diesel(sql_type = Text)]
    pub distinct_id: String,
    #[diesel(sql_type = Nullable<Text>)]
    pub session_id: Option<String>,
}

#[derive(QueryableByName, Clone)]
pub(crate) struct ERow {
    #[diesel(sql_type = SqlUuid)]
    pub app_id: Uuid,
    #[diesel(sql_type = Nullable<SqlUuid>)]
    pub environment_id: Option<Uuid>,
    #[diesel(sql_type = Timestamptz)]
    pub occurred_at: DateTime<Utc>,
    #[diesel(sql_type = Timestamptz)]
    pub received_at: DateTime<Utc>,
    #[diesel(sql_type = Nullable<Text>)]
    pub screen: Option<String>,
    #[diesel(sql_type = Nullable<Text>)]
    pub distinct_id: Option<String>,
}

#[derive(QueryableByName, Clone)]
pub(crate) struct TRow {
    #[diesel(sql_type = SqlUuid)]
    pub app_id: Uuid,
    #[diesel(sql_type = Nullable<SqlUuid>)]
    pub environment_id: Option<Uuid>,
    #[diesel(sql_type = Timestamptz)]
    pub occurred_at: DateTime<Utc>,
    #[diesel(sql_type = Timestamptz)]
    pub received_at: DateTime<Utc>,
    #[diesel(sql_type = Text)]
    pub name: String,
    #[diesel(sql_type = Text)]
    pub op: String,
    #[diesel(sql_type = diesel::sql_types::Double)]
    pub duration_ms: f64,
    #[diesel(sql_type = Nullable<Text>)]
    pub status: Option<String>,
    #[diesel(sql_type = Nullable<diesel::sql_types::Integer>)]
    pub http_status: Option<i32>,
}

// ---------------------------------------------------------------------------
// Cross-fold state.
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
pub(crate) struct SessState {
    pub env: Option<Uuid>,
    pub pending_screen: Option<String>,
    pub pending_at: Option<DateTime<Utc>>,
}

#[derive(Clone)]
pub(crate) struct JourState {
    pub steps: i16,
    pub last_name: String,
}

pub(crate) type SessKey = (Uuid, String);
pub(crate) type JourKey = (Uuid, NaiveDate, String, Uuid);

#[derive(Default)]
pub(crate) struct AnalyticsDeltas {
    pub screens: BTreeMap<(DayKey, String), ScreenDelta>,
    pub nodes: BTreeMap<(DayKey, i16, String), i64>,
    pub links: BTreeMap<(DayKey, i16, String, String), i64>,
    pub top: BTreeMap<(DayKey, String), i64>,
    pub activity: BTreeMap<DayKey, UserActivityDelta>,
}

fn day_of(t: DateTime<Utc>) -> NaiveDate {
    t.date_naive()
}

fn hour_of(t: DateTime<Utc>) -> DateTime<Utc> {
    t.duration_trunc(Duration::hours(1)).unwrap_or(t)
}

/// Fold newly-seen analytics rows into deltas + state. Pure: no I/O.
///
/// `rows` may arrive in any order; the two walks sort their own views. State
/// maps are read-modify-write — entries the caller loaded from
/// `rollup_*_state` for the touched keys, mutated here, persisted after.
pub(crate) fn fold_analytics_rows(
    rows: &[ARow],
    sess: &mut HashMap<SessKey, SessState>,
    jour: &mut HashMap<JourKey, JourState>,
    name_cap: usize,
) -> AnalyticsDeltas {
    let mut d = AnalyticsDeltas::default();

    // Per-row aggregates: event_top, user_activity, screen views/events/users.
    for r in rows {
        let key: DayKey = (r.app_id, r.environment_id, day_of(r.occurred_at));
        *d.top.entry((key, r.name.clone())).or_insert(0) += 1;
        let a = d.activity.entry(key).or_default();
        a.events += 1;
        if !r.distinct_id.is_empty() {
            a.hll_all.insert(&r.distinct_id);
            a.hll_analytics.insert(&r.distinct_id);
        }
        if let Some(scr) = &r.screen {
            let s = d.screens.entry((key, scr.clone())).or_default();
            if r.name == SCREEN_EVENT {
                s.views += 1;
            } else {
                s.events += 1;
            }
            if !r.distinct_id.is_empty() {
                s.users.insert(&r.distinct_id);
            }
        }
    }

    // Dwell walk, mirroring repo::screen_ctes' `dw` CTE exactly: the chain is
    // over every SCREEN-CARRYING analytics event in the session (any name,
    // not only '$screen'), each event's dwell is the gap to the session's
    // next screen-carrying event, credited to ITS OWN screen and env, capped
    // at 30 min, and zero/negative gaps are dropped (`raw_ms > 0`).
    let mut by_session: Vec<&ARow> = rows
        .iter()
        .filter(|r| r.session_id.is_some() && r.screen.is_some())
        .collect();
    by_session.sort_by(|a, b| {
        (a.app_id, a.session_id.as_deref(), a.occurred_at).cmp(&(
            b.app_id,
            b.session_id.as_deref(),
            b.occurred_at,
        ))
    });
    for r in by_session {
        let sk: SessKey = (r.app_id, r.session_id.clone().expect("filtered"));
        let st = sess.entry(sk).or_default();
        if let (Some(scr), Some(at)) = (st.pending_screen.take(), st.pending_at.take()) {
            let gap_ms = (r.occurred_at - at).num_milliseconds() as f64;
            if gap_ms > 0.0 {
                let key: DayKey = (r.app_id, st.env, day_of(at));
                d.screens.entry((key, scr)).or_default().dwell_ms += gap_ms.min(DWELL_CAP_MS);
            }
        }
        // The pending row's OWN env rides along so its dwell lands in the
        // same (screen, env) bucket its views/events count under.
        st.env = r.environment_id;
        st.pending_screen = r.screen.clone();
        st.pending_at = Some(r.occurred_at);
    }

    // Journey walk: first JOURNEY_MAX_STEPS analytics events per (user, day,
    // env). Anonymous rows are excluded — '' would otherwise be one giant
    // shared "user". node.step is 0-based; link.step is the FROM node's step.
    let mut by_user: Vec<&ARow> = rows.iter().filter(|r| !r.distinct_id.is_empty()).collect();
    by_user.sort_by(|a, b| {
        (a.app_id, a.distinct_id.as_str(), a.occurred_at).cmp(&(
            b.app_id,
            b.distinct_id.as_str(),
            b.occurred_at,
        ))
    });
    for r in by_user {
        let day = day_of(r.occurred_at);
        let jk: JourKey = (
            r.app_id,
            day,
            r.distinct_id.clone(),
            env_key(&r.environment_id),
        );
        let key: DayKey = (r.app_id, r.environment_id, day);
        let st = jour.entry(jk).or_insert(JourState {
            steps: 0,
            last_name: String::new(),
        });
        if st.steps >= JOURNEY_MAX_STEPS {
            continue;
        }
        *d.nodes.entry((key, st.steps, r.name.clone())).or_insert(0) += 1;
        if st.steps > 0 {
            *d.links
                .entry((key, st.steps - 1, st.last_name.clone(), r.name.clone()))
                .or_insert(0) += 1;
        }
        st.last_name = r.name.clone();
        st.steps += 1;
    }

    cap_names(&mut d.top, name_cap);
    d
}

/// Soft per-pass cardinality guard: within one fold, an (app, env, day) whose
/// distinct names exceed the cap keeps its top `cap-1` and folds the tail
/// into [`OTHER_NAME`]. Bounds growth per pass rather than absolutely — the
/// honest trade against re-reading the day's stored cardinality every fold.
fn cap_names(top: &mut BTreeMap<(DayKey, String), i64>, cap: usize) {
    if cap == 0 {
        return;
    }
    let mut per_key: HashMap<DayKey, usize> = HashMap::new();
    for k in top.keys() {
        *per_key.entry(k.0).or_insert(0) += 1;
    }
    let over: Vec<DayKey> = per_key
        .into_iter()
        .filter(|(_, n)| *n > cap)
        .map(|(k, _)| k)
        .collect();
    for day_key in over {
        let mut named: Vec<(String, i64)> = top
            .iter()
            .filter(|((k, _), _)| *k == day_key)
            .map(|((_, n), c)| (n.clone(), *c))
            .collect();
        named.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let mut other = 0i64;
        for (name, count) in named.into_iter().skip(cap.saturating_sub(1)) {
            top.remove(&(day_key, name));
            other += count;
        }
        tracing::warn!(
            app = %day_key.0, day = %day_key.2, folded = other,
            "event-name cardinality cap engaged; tail folded into ~other"
        );
        *top.entry((day_key, OTHER_NAME.to_string())).or_insert(0) += other;
    }
}

#[derive(Default)]
pub(crate) struct ErrorDeltas {
    pub screens: BTreeMap<(DayKey, String), ScreenDelta>,
    pub activity: BTreeMap<DayKey, UserActivityDelta>,
}

pub(crate) fn fold_error_rows(rows: &[ERow]) -> ErrorDeltas {
    let mut d = ErrorDeltas::default();
    for r in rows {
        let key: DayKey = (r.app_id, r.environment_id, day_of(r.occurred_at));
        let a = d.activity.entry(key).or_default();
        a.errors += 1;
        let did = r.distinct_id.as_deref().filter(|s| !s.is_empty());
        if let Some(did) = did {
            a.hll_all.insert(did);
        }
        if let Some(scr) = &r.screen {
            let s = d.screens.entry((key, scr.clone())).or_default();
            s.exceptions += 1;
            if let Some(did) = did {
                s.users.insert(did);
            }
        }
    }
    d
}

pub(crate) use super::PerfKey;

pub(crate) fn fold_transaction_rows(
    rows: &[TRow],
    name_cap: usize,
) -> BTreeMap<PerfKey, PerfDelta> {
    let mut out: BTreeMap<PerfKey, PerfDelta> = BTreeMap::new();
    for r in rows {
        let key: PerfKey = (
            (r.app_id, r.environment_id, hour_of(r.occurred_at)),
            r.name.clone(),
            r.op.clone(),
        );
        let d = out.entry(key).or_default();
        d.count += 1;
        if r.status.as_deref() == Some("error") || r.http_status.is_some_and(|s| s >= 500) {
            d.error_count += 1;
        }
        d.duration_sum += r.duration_ms;
        d.hist.record(r.duration_ms);
    }
    // Same soft cap as event names, per (app, env, hour): keep top cap-1
    // (name, op) pairs, fold the tail into (~other, ~other).
    if name_cap > 0 {
        let mut per_key: HashMap<(Uuid, Option<Uuid>, DateTime<Utc>), usize> = HashMap::new();
        for k in out.keys() {
            *per_key.entry(k.0).or_insert(0) += 1;
        }
        for (hour_key, n) in per_key {
            if n <= name_cap {
                continue;
            }
            let mut named: Vec<(String, String, i64)> = out
                .iter()
                .filter(|((k, _, _), _)| *k == hour_key)
                .map(|((_, name, op), d)| (name.clone(), op.clone(), d.count))
                .collect();
            named.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)).then(a.1.cmp(&b.1)));
            let mut folded = PerfDelta::default();
            for (name, op, _) in named.into_iter().skip(name_cap.saturating_sub(1)) {
                if let Some(d) = out.remove(&(hour_key, name, op)) {
                    folded.count += d.count;
                    folded.error_count += d.error_count;
                    folded.duration_sum += d.duration_sum;
                    folded.hist.merge_counts(&d.hist.counts());
                }
            }
            tracing::warn!(app = %hour_key.0, hour = %hour_key.2, folded = folded.count,
                "transaction-name cardinality cap engaged; tail folded into ~other");
            let slot = out
                .entry((hour_key, OTHER_NAME.to_string(), OTHER_NAME.to_string()))
                .or_default();
            slot.count += folded.count;
            slot.error_count += folded.error_count;
            slot.duration_sum += folded.duration_sum;
            slot.hist.merge_counts(&folded.hist.counts());
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Watermarked pulls: chunked so a long backlog cannot balloon memory.
// ---------------------------------------------------------------------------

/// Resolve the keep-boundary for a pull of `max+1` rows sorted by
/// received_at: keep rows strictly older than the overflow row's timestamp so
/// the next pull (`> new_wm`) picks the split timestamp up whole. Returns the
/// number of rows to keep; 0 means the whole pull shares one timestamp and
/// the caller must fetch that timestamp unbounded.
fn split_at_boundary(received: &[DateTime<Utc>], max: usize) -> usize {
    if received.len() <= max {
        return received.len();
    }
    let boundary = received[max];
    received.partition_point(|t| *t < boundary)
}

/// How far back (in whole UTC days) the incremental pull looks by
/// `occurred_at`. The bound exists for PARTITION PRUNING: without it every
/// cycle interrogates all ~90 day-partitions about brand-new receipts, and
/// the planner seq-scans the large mixed ones — measured 1.3M buffers
/// (~10 GB) for a ZERO-row pull. With it a cycle touches ≤3 partitions by
/// range constraint, planner-proof. Rows received more than this many days
/// after they occurred are NOT lost: the daily consistency sweep re-counts
/// the trailing [`CONSISTENCY_DAYS`] days and rebuilds any drifted day from
/// raw (which pulls unbounded), so a true straggler is folded within a day
/// rather than within a minute — the disclosed freshness contract covers
/// live traffic, not month-late backfills.
const PULL_OCCURRED_LOOKBACK_DAYS: i64 = 2;

macro_rules! pull_source {
    ($fn_name:ident, $row:ty, $sql:expr, $eq_sql:expr) => {
        async fn $fn_name(
            conn: &mut AsyncPgConnection,
            wm: DateTime<Utc>,
            upto: DateTime<Utc>,
        ) -> diesel::QueryResult<(Vec<$row>, DateTime<Utc>, bool)> {
            let occurred_floor = (upto - Duration::days(PULL_OCCURRED_LOOKBACK_DAYS))
                .date_naive()
                .and_hms_opt(0, 0, 0)
                .expect("valid")
                .and_utc();
            let mut rows: Vec<$row> = diesel::sql_query($sql)
                .bind::<Timestamptz, _>(wm)
                .bind::<Timestamptz, _>(upto)
                .bind::<Timestamptz, _>(occurred_floor)
                .bind::<BigInt, _>((FOLD_MAX_ROWS + 1) as i64)
                .get_results(conn)
                .await?;
            if rows.len() <= FOLD_MAX_ROWS {
                return Ok((rows, upto, true));
            }
            let received: Vec<_> = rows.iter().map(|r| r.received_at).collect();
            let keep = split_at_boundary(&received, FOLD_MAX_ROWS);
            if keep == 0 {
                // Pathological: >FOLD_MAX_ROWS rows share one received_at
                // (bulk inserts). Fold that timestamp whole instead.
                let ts = rows[0].received_at;
                let all: Vec<$row> = diesel::sql_query($eq_sql)
                    .bind::<Timestamptz, _>(ts)
                    .get_results(conn)
                    .await?;
                return Ok((all, ts, false));
            }
            let new_wm = rows[keep - 1].received_at;
            rows.truncate(keep);
            Ok((rows, new_wm, false))
        }
    };
}

pull_source!(
    pull_analytics,
    ARow,
    "SELECT app_id, environment_id, occurred_at, received_at, name, screen, distinct_id, session_id \
     FROM analytics_events WHERE received_at > $1 AND received_at <= $2 AND occurred_at >= $3 \
     ORDER BY received_at, id LIMIT $4",
    "SELECT app_id, environment_id, occurred_at, received_at, name, screen, distinct_id, session_id \
     FROM analytics_events WHERE received_at = $1"
);
pull_source!(
    pull_errors,
    ERow,
    "SELECT app_id, environment_id, occurred_at, received_at, screen, distinct_id \
     FROM error_events WHERE received_at > $1 AND received_at <= $2 AND occurred_at >= $3 \
     ORDER BY received_at, id LIMIT $4",
    "SELECT app_id, environment_id, occurred_at, received_at, screen, distinct_id \
     FROM error_events WHERE received_at = $1"
);
pull_source!(
    pull_transactions,
    TRow,
    "SELECT app_id, environment_id, occurred_at, received_at, name, op, duration_ms, status, http_status \
     FROM transactions WHERE received_at > $1 AND received_at <= $2 AND occurred_at >= $3 \
     ORDER BY received_at, id LIMIT $4",
    "SELECT app_id, environment_id, occurred_at, received_at, name, op, duration_ms, status, http_status \
     FROM transactions WHERE received_at = $1"
);

// ---------------------------------------------------------------------------
// State-table I/O (inside the fold transaction).
// ---------------------------------------------------------------------------

#[derive(QueryableByName)]
struct SessStateRow {
    #[diesel(sql_type = SqlUuid)]
    app_id: Uuid,
    #[diesel(sql_type = Text)]
    session_id: String,
    #[diesel(sql_type = Nullable<SqlUuid>)]
    environment_id: Option<Uuid>,
    #[diesel(sql_type = Nullable<Text>)]
    pending_screen: Option<String>,
    #[diesel(sql_type = Nullable<Timestamptz>)]
    pending_at: Option<DateTime<Utc>>,
}

async fn load_session_state(
    conn: &mut AsyncPgConnection,
    keys: &[SessKey],
) -> diesel::QueryResult<HashMap<SessKey, SessState>> {
    let mut out = HashMap::new();
    for chunk in keys.chunks(5000) {
        let rows: Vec<SessStateRow> = diesel::sql_query(
            "SELECT s.app_id, s.session_id, s.environment_id, s.pending_screen, s.pending_at \
             FROM rollup_session_state s \
             JOIN unnest($1::uuid[], $2::text[]) AS k(app_id, session_id) \
               ON s.app_id = k.app_id AND s.session_id = k.session_id \
             FOR UPDATE OF s",
        )
        .bind::<Array<SqlUuid>, _>(chunk.iter().map(|(a, _)| *a).collect::<Vec<_>>())
        .bind::<Array<Text>, _>(chunk.iter().map(|(_, s)| s.clone()).collect::<Vec<_>>())
        .get_results(conn)
        .await?;
        for r in rows {
            out.insert(
                (r.app_id, r.session_id),
                SessState {
                    env: r.environment_id,
                    pending_screen: r.pending_screen,
                    pending_at: r.pending_at,
                },
            );
        }
    }
    Ok(out)
}

async fn save_session_state(
    conn: &mut AsyncPgConnection,
    state: &HashMap<SessKey, SessState>,
) -> diesel::QueryResult<()> {
    let mut rows: Vec<(&SessKey, &SessState)> = state.iter().collect();
    rows.sort_by(|a, b| a.0.cmp(b.0));
    for chunk in rows.chunks(5000) {
        diesel::sql_query(
            "INSERT INTO rollup_session_state \
                 (app_id, session_id, environment_id, pending_screen, pending_at, updated_at) \
             SELECT app_id, session_id, env, scr, at, now() \
             FROM unnest($1::uuid[], $2::text[], $3::uuid[], $4::text[], $5::timestamptz[]) \
                  AS t(app_id, session_id, env, scr, at) \
             ON CONFLICT (app_id, session_id) \
             DO UPDATE SET environment_id = EXCLUDED.environment_id, \
                           pending_screen = EXCLUDED.pending_screen, \
                           pending_at = EXCLUDED.pending_at, updated_at = now()",
        )
        .bind::<Array<SqlUuid>, _>(chunk.iter().map(|(k, _)| k.0).collect::<Vec<_>>())
        .bind::<Array<Text>, _>(chunk.iter().map(|(k, _)| k.1.clone()).collect::<Vec<_>>())
        .bind::<Array<Nullable<SqlUuid>>, _>(chunk.iter().map(|(_, s)| s.env).collect::<Vec<_>>())
        .bind::<Array<Nullable<Text>>, _>(
            chunk
                .iter()
                .map(|(_, s)| s.pending_screen.clone())
                .collect::<Vec<_>>(),
        )
        .bind::<Array<Nullable<Timestamptz>>, _>(
            chunk.iter().map(|(_, s)| s.pending_at).collect::<Vec<_>>(),
        )
        .execute(conn)
        .await?;
    }
    Ok(())
}

#[derive(QueryableByName)]
struct JourStateRow {
    #[diesel(sql_type = SqlUuid)]
    app_id: Uuid,
    #[diesel(sql_type = Date)]
    day: NaiveDate,
    #[diesel(sql_type = Text)]
    distinct_id: String,
    #[diesel(sql_type = SqlUuid)]
    env_key: Uuid,
    #[diesel(sql_type = SmallInt)]
    steps: i16,
    #[diesel(sql_type = Text)]
    last_name: String,
}

async fn load_journey_state(
    conn: &mut AsyncPgConnection,
    keys: &[JourKey],
) -> diesel::QueryResult<HashMap<JourKey, JourState>> {
    let mut out = HashMap::new();
    for chunk in keys.chunks(5000) {
        let rows: Vec<JourStateRow> = diesel::sql_query(
            "SELECT j.app_id, j.day, j.distinct_id, j.env_key, j.steps, j.last_name \
             FROM rollup_journey_state j \
             JOIN unnest($1::uuid[], $2::date[], $3::text[], $4::uuid[]) \
                  AS k(app_id, day, distinct_id, env_key) \
               ON j.app_id = k.app_id AND j.day = k.day AND j.distinct_id = k.distinct_id \
              AND j.env_key = k.env_key \
             FOR UPDATE OF j",
        )
        .bind::<Array<SqlUuid>, _>(chunk.iter().map(|k| k.0).collect::<Vec<_>>())
        .bind::<Array<Date>, _>(chunk.iter().map(|k| k.1).collect::<Vec<_>>())
        .bind::<Array<Text>, _>(chunk.iter().map(|k| k.2.clone()).collect::<Vec<_>>())
        .bind::<Array<SqlUuid>, _>(chunk.iter().map(|k| k.3).collect::<Vec<_>>())
        .get_results(conn)
        .await?;
        for r in rows {
            out.insert(
                (r.app_id, r.day, r.distinct_id, r.env_key),
                JourState {
                    steps: r.steps,
                    last_name: r.last_name,
                },
            );
        }
    }
    Ok(out)
}

async fn save_journey_state(
    conn: &mut AsyncPgConnection,
    state: &HashMap<JourKey, JourState>,
) -> diesel::QueryResult<()> {
    let mut rows: Vec<(&JourKey, &JourState)> = state.iter().collect();
    rows.sort_by(|a, b| a.0.cmp(b.0));
    for chunk in rows.chunks(5000) {
        diesel::sql_query(
            "INSERT INTO rollup_journey_state \
                 (app_id, day, distinct_id, env_key, steps, last_name, updated_at) \
             SELECT app_id, day, distinct_id, env_key, steps, last_name, now() \
             FROM unnest($1::uuid[], $2::date[], $3::text[], $4::uuid[], $5::smallint[], $6::text[]) \
                  AS t(app_id, day, distinct_id, env_key, steps, last_name) \
             ON CONFLICT (app_id, day, distinct_id, env_key) \
             DO UPDATE SET steps = EXCLUDED.steps, last_name = EXCLUDED.last_name, updated_at = now()",
        )
        .bind::<Array<SqlUuid>, _>(chunk.iter().map(|(k, _)| k.0).collect::<Vec<_>>())
        .bind::<Array<Date>, _>(chunk.iter().map(|(k, _)| k.1).collect::<Vec<_>>())
        .bind::<Array<Text>, _>(chunk.iter().map(|(k, _)| k.2.clone()).collect::<Vec<_>>())
        .bind::<Array<SqlUuid>, _>(chunk.iter().map(|(k, _)| k.3).collect::<Vec<_>>())
        .bind::<Array<SmallInt>, _>(chunk.iter().map(|(_, s)| s.steps).collect::<Vec<_>>())
        .bind::<Array<Text>, _>(chunk.iter().map(|(_, s)| s.last_name.clone()).collect::<Vec<_>>())
        .execute(conn)
        .await?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Public fold entry points. One advisory-locked transaction each.
// ---------------------------------------------------------------------------

/// Fold analytics rows received in `(watermark, upto]`. `Ok(None)` when the
/// watermark is already at or past `upto`.
pub async fn fold_analytics(
    conn: &mut AsyncPgConnection,
    upto: DateTime<Utc>,
    name_cap: usize,
) -> diesel::QueryResult<Option<FoldOutcome>> {
    begin_locked(conn).await?;
    let out = async {
        let wm = watermark(conn, SRC_ANALYTICS).await?;
        if wm >= upto {
            return Ok(None);
        }
        let (rows, new_wm, caught_up) = pull_analytics(conn, wm, upto).await?;
        if rows.is_empty() {
            set_watermark(conn, SRC_ANALYTICS, upto).await?;
            return Ok(Some(FoldOutcome {
                rows_read: 0,
                new_watermark: upto,
                caught_up: true,
            }));
        }
        let mut sess_keys: Vec<SessKey> = rows
            .iter()
            .filter_map(|r| r.session_id.clone().map(|s| (r.app_id, s)))
            .collect();
        sess_keys.sort();
        sess_keys.dedup();
        let mut jour_keys: Vec<JourKey> = rows
            .iter()
            .filter(|r| !r.distinct_id.is_empty())
            .map(|r| {
                (
                    r.app_id,
                    day_of(r.occurred_at),
                    r.distinct_id.clone(),
                    env_key(&r.environment_id),
                )
            })
            .collect();
        jour_keys.sort();
        jour_keys.dedup();
        let mut sess = load_session_state(conn, &sess_keys).await?;
        let mut jour = load_journey_state(conn, &jour_keys).await?;
        let mut d = fold_analytics_rows(&rows, &mut sess, &mut jour, name_cap);
        add_event_top(conn, &d.top).await?;
        add_journey_nodes(conn, &d.nodes).await?;
        add_journey_links(conn, &d.links).await?;
        add_user_activity(conn, &mut d.activity).await?;
        add_screen_stats(conn, &mut d.screens).await?;
        save_session_state(conn, &sess).await?;
        save_journey_state(conn, &jour).await?;
        set_watermark(conn, SRC_ANALYTICS, new_wm).await?;
        Ok(Some(FoldOutcome {
            rows_read: rows.len(),
            new_watermark: new_wm,
            caught_up,
        }))
    }
    .await;
    finish(conn, out).await
}

pub async fn fold_errors(
    conn: &mut AsyncPgConnection,
    upto: DateTime<Utc>,
) -> diesel::QueryResult<Option<FoldOutcome>> {
    begin_locked(conn).await?;
    let out = async {
        let wm = watermark(conn, SRC_ERRORS).await?;
        if wm >= upto {
            return Ok(None);
        }
        let (rows, new_wm, caught_up) = pull_errors(conn, wm, upto).await?;
        let n = rows.len();
        let mut d = fold_error_rows(&rows);
        add_user_activity(conn, &mut d.activity).await?;
        add_screen_stats(conn, &mut d.screens).await?;
        set_watermark(conn, SRC_ERRORS, new_wm).await?;
        Ok(Some(FoldOutcome {
            rows_read: n,
            new_watermark: new_wm,
            caught_up,
        }))
    }
    .await;
    finish(conn, out).await
}

pub async fn fold_transactions(
    conn: &mut AsyncPgConnection,
    upto: DateTime<Utc>,
    name_cap: usize,
) -> diesel::QueryResult<Option<FoldOutcome>> {
    begin_locked(conn).await?;
    let out = async {
        let wm = watermark(conn, SRC_TRANSACTIONS).await?;
        if wm >= upto {
            return Ok(None);
        }
        let (rows, new_wm, caught_up) = pull_transactions(conn, wm, upto).await?;
        let n = rows.len();
        let mut d = fold_transaction_rows(&rows, name_cap);
        add_perf_agg(conn, &mut d).await?;
        set_watermark(conn, SRC_TRANSACTIONS, new_wm).await?;
        Ok(Some(FoldOutcome {
            rows_read: n,
            new_watermark: new_wm,
            caught_up,
        }))
    }
    .await;
    finish(conn, out).await
}

async fn finish<T>(
    conn: &mut AsyncPgConnection,
    out: diesel::QueryResult<T>,
) -> diesel::QueryResult<T> {
    match out {
        Ok(v) => {
            use diesel_async::SimpleAsyncConnection;
            conn.batch_execute("COMMIT").await?;
            Ok(v)
        }
        Err(e) => {
            rollback_quietly(conn).await;
            Err(e)
        }
    }
}

// ---------------------------------------------------------------------------
// Sessions: rolling recompute (sessions mutate in place; folds can't add).
// ---------------------------------------------------------------------------

#[derive(QueryableByName)]
struct SessionRow {
    #[diesel(sql_type = SqlUuid)]
    app_id: Uuid,
    #[diesel(sql_type = Nullable<SqlUuid>)]
    environment_id: Option<Uuid>,
    #[diesel(sql_type = Nullable<Text>)]
    device_key: Option<String>,
    #[diesel(sql_type = Timestamptz)]
    started_at: DateTime<Utc>,
    #[diesel(sql_type = Timestamptz)]
    last_event_at: DateTime<Utc>,
    #[diesel(sql_type = BigInt)]
    unhandled: i64,
}

fn fixed_bucket(ms: f64) -> usize {
    // Mirrors repo::DURATION_BUCKET_CASE_SQL: <10s, 10-60s, 1-5m, 5-30m, 30m+.
    if ms < 10_000.0 {
        0
    } else if ms < 60_000.0 {
        1
    } else if ms < 300_000.0 {
        2
    } else if ms < 1_800_000.0 {
        3
    } else {
        4
    }
}

fn session_rows_to_days(rows: &[SessionRow]) -> Vec<SessionDayRow> {
    let mut map: BTreeMap<DayKey, SessionDayRow> = BTreeMap::new();
    for r in rows {
        let key: DayKey = (r.app_id, r.environment_id, day_of(r.started_at));
        let e = map.entry(key).or_insert_with(|| SessionDayRow {
            app: key.0,
            env: key.1,
            day: key.2,
            sessions: 0,
            crashed: 0,
            duration_ms_sum: 0.0,
            hist: LatencyHistogram::new(),
            fixed: [0; 5],
        });
        e.sessions += 1;
        if r.unhandled > 0 {
            e.crashed += 1;
        }
        let ms = ((r.last_event_at - r.started_at).num_milliseconds() as f64).max(0.0);
        e.duration_ms_sum += ms;
        e.hist.record(ms);
        e.fixed[fixed_bucket(ms)] += 1;
    }
    map.into_values().collect()
}

/// The device-day sibling: per (app, device, env, started-day) session
/// counts, feeding /device-groups' windowed `sessions_count` without its old
/// per-device LATERAL. Keyless sessions carry no device page presence and are
/// skipped.
fn session_rows_to_device_days(rows: &[SessionRow]) -> Vec<super::DeviceDayRow> {
    let mut map: BTreeMap<(Uuid, String, Option<Uuid>, NaiveDate), i64> = BTreeMap::new();
    for r in rows {
        let Some(dk) = &r.device_key else { continue };
        *map.entry((r.app_id, dk.clone(), r.environment_id, day_of(r.started_at)))
            .or_insert(0) += 1;
    }
    map.into_iter()
        .map(
            |((app, device_key, env, day), sessions)| super::DeviceDayRow {
                app,
                device_key,
                env,
                day,
                sessions,
            },
        )
        .collect()
}

/// Recompute every (app, started-day) bucket that saw session activity since
/// `since` — `None` recomputes ALL days (backfill). Whole dirty days are
/// re-read so buckets whose older sessions went quiet are not undercounted.
pub async fn recompute_sessions(
    conn: &mut AsyncPgConnection,
    since: Option<DateTime<Utc>>,
) -> diesel::QueryResult<usize> {
    // UNIX_EPOCH, not chrono's MIN_UTC: MIN_UTC's year -262144 is outside
    // Postgres' timestamptz range and the bind itself would error.
    let since = since.unwrap_or(DateTime::UNIX_EPOCH);
    #[derive(QueryableByName)]
    struct DayRow {
        #[diesel(sql_type = Date)]
        day: NaiveDate,
    }
    // Day list first (small), then chunks of 7 consecutive days per
    // transaction: a full recompute over millions of sessions must not hold
    // one giant transaction or one giant Vec, and per-chunk commits make an
    // interruption cost a week of days, not the run.
    let days: Vec<DayRow> = diesel::sql_query(
        "SELECT DISTINCT (started_at AT TIME ZONE 'UTC')::date AS day \
         FROM sessions WHERE last_event_at >= $1 ORDER BY 1",
    )
    .bind::<Timestamptz, _>(since)
    .get_results(conn)
    .await?;
    let mut days: Vec<NaiveDate> = days.into_iter().map(|d| d.day).collect();
    // Retention clamp, [`tier_dropped_floor`]'s twin for sessions: a day at or
    // below the dropped boundary must never be re-REPLACED. Its partition is
    // gone, so it can only enter this list through a stray late row in
    // `sessions_default` — and replacing the day from strays alone would wipe
    // a real rollup day down to the stray. Fully-dropped days cannot appear
    // here at all (no rows); this guards exactly the stray case.
    if let Some(floor) = sessions_dropped_floor(conn).await? {
        let floor_day = floor.date_naive();
        days.retain(|d| *d >= floor_day);
    }
    let mut total = 0usize;
    for chunk in days.chunks(7) {
        let lo = chunk[0].and_hms_opt(0, 0, 0).expect("valid").and_utc();
        let hi = (*chunk.last().expect("non-empty") + Duration::days(1))
            .and_hms_opt(0, 0, 0)
            .expect("valid")
            .and_utc();
        begin_locked(conn).await?;
        let out = async {
            let rows: Vec<SessionRow> = diesel::sql_query(
                "SELECT app_id, environment_id, device_key, started_at, last_event_at, \
                        unhandled_errors_count::bigint AS unhandled \
                 FROM sessions WHERE started_at >= $1 AND started_at < $2",
            )
            .bind::<Timestamptz, _>(lo)
            .bind::<Timestamptz, _>(hi)
            .get_results(conn)
            .await?;
            let day_rows = session_rows_to_days(&rows);
            let dev_rows = session_rows_to_device_days(&rows);
            let mut dirty: Vec<(Uuid, NaiveDate)> =
                day_rows.iter().map(|r| (r.app, r.day)).collect();
            dirty.sort();
            dirty.dedup();
            replace_session_days(conn, &dirty, &day_rows).await?;
            super::replace_device_session_days(conn, &dirty, &dev_rows).await?;
            Ok(dirty.len())
        }
        .await;
        total += finish(conn, out).await?;
    }
    // Freshness stamp last, plain autocommit — by now every chunk is durable.
    set_watermark(conn, SRC_SESSIONS, Utc::now()).await?;
    Ok(total)
}

// ---------------------------------------------------------------------------
// Day rebuild (backfill + consistency repair).
// ---------------------------------------------------------------------------

#[derive(QueryableByName)]
struct CountRow {
    #[diesel(sql_type = BigInt)]
    n: i64,
}

/// Re-aggregate one UTC day of the three event sources.
///
/// * `received_upto = Some(epoch)` + `delete_first = false`: backfill — adds
///   pre-epoch contributions, disjoint from the live fold by the epoch split.
/// * `received_upto = None` + `delete_first = true`: consistency repair —
///   replaces the day outright.
///
/// Streams hour-by-hour so a production-density day never sits in memory
/// whole; journey/dwell cursors live across the hours, which is exactly the
/// ordering guarantee the hour loop provides. State is persisted only for
/// days young enough for the live fold to continue (>= today-2).
pub async fn fold_day_from_raw(
    conn: &mut AsyncPgConnection,
    day: NaiveDate,
    received_upto: Option<DateTime<Utc>>,
    delete_first: bool,
    name_cap: usize,
) -> diesel::QueryResult<()> {
    begin_locked(conn).await?;
    let out = async {
        let day_start = day.and_hms_opt(0, 0, 0).expect("valid").and_utc();
        // Year 9999, not chrono's MAX_UTC — same Postgres-range caution as the
        // UNIX_EPOCH note in recompute_sessions.
        let received = received_upto
            .unwrap_or_else(|| Utc.with_ymd_and_hms(9999, 1, 1, 0, 0, 0).single().expect("valid"));
        if delete_first {
            for tbl in [
                "screen_stats_daily",
                "journey_nodes_daily",
                "journey_links_daily",
                "user_activity_daily",
                "event_top_daily",
            ] {
                diesel::sql_query(format!("DELETE FROM {tbl} WHERE day = $1"))
                    .bind::<Date, _>(day)
                    .execute(conn)
                    .await?;
            }
            diesel::sql_query("DELETE FROM perf_agg_hourly WHERE hour >= $1 AND hour < $1 + interval '1 day'")
                .bind::<Timestamptz, _>(day_start)
                .execute(conn)
                .await?;
        }
        let mut sess: HashMap<SessKey, SessState> = HashMap::new();
        let mut jour: HashMap<JourKey, JourState> = HashMap::new();
        let mut acc = AnalyticsDeltas::default();
        let mut err_acc = ErrorDeltas::default();
        let mut perf_acc: BTreeMap<PerfKey, PerfDelta> = BTreeMap::new();
        for h in 0..24 {
            let lo = day_start + Duration::hours(h);
            let hi = lo + Duration::hours(1);
            let a: Vec<ARow> = diesel::sql_query(
                "SELECT app_id, environment_id, occurred_at, received_at, name, screen, distinct_id, session_id \
                 FROM analytics_events \
                 WHERE occurred_at >= $1 AND occurred_at < $2 AND received_at <= $3 \
                 ORDER BY occurred_at, id",
            )
            .bind::<Timestamptz, _>(lo)
            .bind::<Timestamptz, _>(hi)
            .bind::<Timestamptz, _>(received)
            .get_results(conn)
            .await?;
            let d = fold_analytics_rows(&a, &mut sess, &mut jour, name_cap);
            merge_analytics(&mut acc, d);
            let e: Vec<ERow> = diesel::sql_query(
                "SELECT app_id, environment_id, occurred_at, received_at, screen, distinct_id \
                 FROM error_events \
                 WHERE occurred_at >= $1 AND occurred_at < $2 AND received_at <= $3",
            )
            .bind::<Timestamptz, _>(lo)
            .bind::<Timestamptz, _>(hi)
            .bind::<Timestamptz, _>(received)
            .get_results(conn)
            .await?;
            merge_errors(&mut err_acc, fold_error_rows(&e));
            let t: Vec<TRow> = diesel::sql_query(
                "SELECT app_id, environment_id, occurred_at, received_at, name, op, duration_ms, status, http_status \
                 FROM transactions \
                 WHERE occurred_at >= $1 AND occurred_at < $2 AND received_at <= $3",
            )
            .bind::<Timestamptz, _>(lo)
            .bind::<Timestamptz, _>(hi)
            .bind::<Timestamptz, _>(received)
            .get_results(conn)
            .await?;
            merge_perf(&mut perf_acc, fold_transaction_rows(&t, name_cap));
        }
        cap_names(&mut acc.top, name_cap);
        add_event_top(conn, &acc.top).await?;
        add_journey_nodes(conn, &acc.nodes).await?;
        add_journey_links(conn, &acc.links).await?;
        add_user_activity(conn, &mut acc.activity).await?;
        add_screen_stats(conn, &mut acc.screens).await?;
        add_user_activity(conn, &mut err_acc.activity).await?;
        add_screen_stats(conn, &mut err_acc.screens).await?;
        add_perf_agg(conn, &mut perf_acc).await?;
        if day >= Utc::now().date_naive() - Duration::days(2) {
            save_session_state(conn, &sess).await?;
            save_journey_state(conn, &jour).await?;
        }
        Ok(())
    }
    .await;
    finish(conn, out).await
}

fn merge_analytics(acc: &mut AnalyticsDeltas, d: AnalyticsDeltas) {
    for (k, v) in d.top {
        *acc.top.entry(k).or_insert(0) += v;
    }
    for (k, v) in d.nodes {
        *acc.nodes.entry(k).or_insert(0) += v;
    }
    for (k, v) in d.links {
        *acc.links.entry(k).or_insert(0) += v;
    }
    for (k, v) in d.activity {
        let e = acc.activity.entry(k).or_default();
        e.events += v.events;
        e.errors += v.errors;
        e.hll_all.merge(&v.hll_all);
        e.hll_analytics.merge(&v.hll_analytics);
    }
    for (k, v) in d.screens {
        let e = acc.screens.entry(k).or_default();
        e.views += v.views;
        e.events += v.events;
        e.exceptions += v.exceptions;
        e.dwell_ms += v.dwell_ms;
        e.users.merge(&v.users);
    }
}

fn merge_errors(acc: &mut ErrorDeltas, d: ErrorDeltas) {
    for (k, v) in d.activity {
        let e = acc.activity.entry(k).or_default();
        e.errors += v.errors;
        e.hll_all.merge(&v.hll_all);
    }
    for (k, v) in d.screens {
        let e = acc.screens.entry(k).or_default();
        e.exceptions += v.exceptions;
        e.users.merge(&v.users);
    }
}

fn merge_perf(acc: &mut BTreeMap<PerfKey, PerfDelta>, d: BTreeMap<PerfKey, PerfDelta>) {
    for (k, v) in d {
        let e = acc.entry(k).or_default();
        e.count += v.count;
        e.error_count += v.error_count;
        e.duration_sum += v.duration_sum;
        e.hist.merge_counts(&v.hist.counts());
    }
}

// ---------------------------------------------------------------------------
// Consistency check + state pruning (the daily maintenance pass).
// ---------------------------------------------------------------------------

/// Days the daily reconciliation re-counts (excluding today, which is still
/// folding). Wide enough to catch anything [`PULL_OCCURRED_LOOKBACK_DAYS`]
/// skips for up to a month of lateness; each day is one partition-pruned
/// count per source, so the whole sweep is cheap.
pub const CONSISTENCY_DAYS: i64 = 35;

/// Compare trailing days' raw counts against the rollup sums. Returns human
/// descriptions of drifted (day, source) pairs (>0.5% relative, or any
/// absolute drift beyond 5 rows). Counters are derived, never trusted.
pub async fn consistency_check_trailing(
    conn: &mut AsyncPgConnection,
) -> diesel::QueryResult<Vec<(NaiveDate, String)>> {
    let floor = tier_dropped_floor(conn).await?;
    let mut all = Vec::new();
    for back in 1..=CONSISTENCY_DAYS {
        let day = Utc::now().date_naive() - Duration::days(back);
        if let Some(f) = floor {
            if day.and_hms_opt(0, 0, 0).expect("valid").and_utc() < f {
                // At least part of this day's raw rows now live ONLY in the
                // cold tier (Parquet). Counting Postgres would report false
                // drift, and the maintenance "heal" would then rebuild the
                // day from what little raw remains — deleting good rollups.
                // The cold copy is immutable, so a day that was consistent
                // when it was exported stays consistent.
                continue;
            }
        }
        all.extend(consistency_check_day(conn, day).await?);
    }
    Ok(all)
}

/// Exclusive upper bound of raw rows `sauron-tier` has DROPPED from Postgres,
/// taken across the tiered tables. MAX, not MIN, because
/// [`fold_day_from_raw`]'s rebuild deletes and rewrites EVERY source's rollup
/// rows for the day — a day is only rebuildable while its raw rows are still
/// fully hot in ALL of them.
pub async fn tier_dropped_floor(
    conn: &mut AsyncPgConnection,
) -> diesel::QueryResult<Option<DateTime<Utc>>> {
    let r: MinDayRow = diesel::sql_query(
        "SELECT max(dropped_thru) AS t FROM tiering_state \
         WHERE table_name IN ('analytics_events', 'error_events', 'transactions')",
    )
    .get_result(conn)
    .await?;
    Ok(r.t)
}

async fn consistency_check_day(
    conn: &mut AsyncPgConnection,
    day: NaiveDate,
) -> diesel::QueryResult<Vec<(NaiveDate, String)>> {
    let start = day.and_hms_opt(0, 0, 0).expect("valid").and_utc();
    // (day parameter comes from consistency_check_trailing's loop.)
    let mut drifts = Vec::new();
    let checks: [(&str, &str, &str); 3] = [
        (
            SRC_ANALYTICS,
            "SELECT count(*)::bigint AS n FROM analytics_events WHERE occurred_at >= $1 AND occurred_at < $2",
            "SELECT COALESCE(sum(count), 0)::bigint AS n FROM event_top_daily WHERE day = $3",
        ),
        (
            SRC_ERRORS,
            "SELECT count(*)::bigint AS n FROM error_events WHERE occurred_at >= $1 AND occurred_at < $2",
            "SELECT COALESCE(sum(errors), 0)::bigint AS n FROM user_activity_daily WHERE day = $3",
        ),
        (
            SRC_TRANSACTIONS,
            "SELECT count(*)::bigint AS n FROM transactions WHERE occurred_at >= $1 AND occurred_at < $2",
            "SELECT COALESCE(sum(count), 0)::bigint AS n FROM perf_agg_hourly WHERE hour >= $1 AND hour < $2",
        ),
    ];
    for (source, raw_sql, rolled_sql) in checks {
        let raw: CountRow = diesel::sql_query(raw_sql)
            .bind::<Timestamptz, _>(start)
            .bind::<Timestamptz, _>(start + Duration::days(1))
            .get_result(conn)
            .await?;
        let rolled: CountRow = if rolled_sql.contains("$3") {
            diesel::sql_query(rolled_sql.replace("$3", "$1"))
                .bind::<Date, _>(day)
                .get_result(conn)
                .await?
        } else {
            diesel::sql_query(rolled_sql)
                .bind::<Timestamptz, _>(start)
                .bind::<Timestamptz, _>(start + Duration::days(1))
                .get_result(conn)
                .await?
        };
        let diff = (raw.n - rolled.n).abs();
        let tolerated = diff <= 5 || (raw.n > 0 && (diff as f64 / raw.n as f64) <= 0.005);
        if !tolerated {
            drifts.push((
                day,
                format!("{source}: raw {} vs rollup {}", raw.n, rolled.n),
            ));
        }
    }
    Ok(drifts)
}

/// Keep `sessions` partitions ahead of the clock. The tier worker pre-creates
/// firehose partitions but does not own `sessions`; this task is the always-on
/// process, so it owns them. Idempotent; the DEFAULT partition is the net for
/// anything that outruns it.
pub async fn ensure_session_partitions(conn: &mut AsyncPgConnection) -> diesel::QueryResult<usize> {
    let mut made = 0usize;
    let today = Utc::now().date_naive();
    for off in 0..=7 {
        let d = today + Duration::days(off);
        let name = format!("sessions_{}", d.format("%Y_%m_%d"));
        let exists: CountRow =
            diesel::sql_query("SELECT count(*)::bigint AS n FROM pg_class WHERE relname = $1")
                .bind::<Text, _>(name.clone())
                .get_result(conn)
                .await?;
        if exists.n > 0 {
            continue;
        }
        let lo = format!("{d} 00:00:00+00");
        let hi = format!("{} 00:00:00+00", d + Duration::days(1));
        diesel::sql_query(format!(
            "CREATE TABLE {name} PARTITION OF sessions FOR VALUES FROM ('{lo}') TO ('{hi}') \
             WITH (autovacuum_vacuum_scale_factor = 0.0, autovacuum_vacuum_threshold = 5000, \
             autovacuum_analyze_scale_factor = 0.0, autovacuum_analyze_threshold = 5000)",
        ))
        .execute(conn)
        .await?;
        made += 1;
    }
    Ok(made)
}

/// `tiering_state.dropped_thru` for `sessions`: the boundary below which raw
/// session rows have been retention-dropped. Unlike the firehose tables there
/// is NO cold copy — the session-day rollups are the surviving record — so
/// both `recompute_sessions` and the maintenance pass treat this as a hard
/// floor. [`tier_dropped_floor`]'s twin.
pub async fn sessions_dropped_floor(
    conn: &mut AsyncPgConnection,
) -> diesel::QueryResult<Option<DateTime<Utc>>> {
    let r: MinDayRow = diesel::sql_query(
        "SELECT max(dropped_thru) AS t FROM tiering_state WHERE table_name = 'sessions'",
    )
    .get_result(conn)
    .await?;
    Ok(r.t)
}

/// Drop whole `sessions` partitions older than `retention_days`, oldest
/// first. There is no cold copy; `session_stats_daily` /
/// `device_sessions_daily` are what remains of a dropped day, which is why
/// the fold-watermark interlock refuses anything the session recompute has
/// not passed — same shape as the tier worker's rollup interlock: enforced,
/// not assumed, because a stopped fold is exactly the state in which
/// partitions keep aging while rollups fall behind. Returns partitions
/// dropped.
pub async fn enforce_session_retention(
    conn: &mut AsyncPgConnection,
    retention_days: i64,
) -> diesel::QueryResult<usize> {
    let boundary = (Utc::now().date_naive() - Duration::days(retention_days))
        .and_hms_opt(0, 0, 0)
        .expect("valid")
        .and_utc();
    let Some(wm) = super::as_of(conn, &[SRC_SESSIONS]).await? else {
        tracing::warn!("session retention: no sessions rollup watermark; not dropping anything");
        return Ok(0);
    };
    let mut dropped = 0usize;
    for child in crate::repo::list_child_partitions(conn, "sessions").await? {
        let Some(start) = parse_sessions_suffix(&child) else {
            continue;
        };
        let end = start + Duration::days(1);
        if end > boundary {
            continue; // inside the retention window
        }
        if end > wm {
            tracing::warn!(
                partition = %child, watermark = %wm,
                "session retention: partition not folded into rollups yet; retained"
            );
            continue;
        }
        crate::repo::detach_and_drop_partition(conn, "sessions", &child).await?;
        crate::repo::advance_watermark(conn, "sessions", end).await?;
        crate::repo::set_dropped_thru(conn, "sessions", end).await?;
        dropped += 1;
    }
    // Strays: a late `bump_sessions` INSERT for an already-dropped day lands
    // in `sessions_default` (its partition no longer exists). Below the
    // recorded boundary such a row is out of retention by definition, and
    // leaving it would hand `recompute_sessions` a day made of strays.
    if let Some(f) = sessions_dropped_floor(conn).await? {
        diesel::sql_query("DELETE FROM sessions_default WHERE started_at < $1")
            .bind::<Timestamptz, _>(f)
            .execute(conn)
            .await?;
    }
    Ok(dropped)
}

/// `sessions_2026_07_26` → 2026-07-26T00:00:00Z — `ensure_session_partitions`'
/// naming, inverted.
fn parse_sessions_suffix(child: &str) -> Option<DateTime<Utc>> {
    let suffix = child.strip_prefix("sessions_")?;
    let parts: Vec<&str> = suffix.split('_').collect();
    if parts.len() != 3 {
        return None;
    }
    let d = NaiveDate::from_ymd_opt(
        parts[0].parse().ok()?,
        parts[1].parse().ok()?,
        parts[2].parse().ok()?,
    )?;
    Some(d.and_hms_opt(0, 0, 0).expect("valid").and_utc())
}

/// The alarm behind migration 73's design: cross-day duplicate sessions are
/// prevented by advisory locks in the write path, not by a DB constraint, so
/// this probe is what turns a lock-logic regression into a warning instead of
/// silent double-counting. Index-only over (app_id, session_id, started_at).
pub async fn duplicate_session_probe(conn: &mut AsyncPgConnection) -> diesel::QueryResult<i64> {
    let r: CountRow = diesel::sql_query(
        "SELECT count(*)::bigint AS n FROM ( \
             SELECT 1 FROM sessions GROUP BY app_id, session_id HAVING count(*) > 1 LIMIT 100 \
         ) d",
    )
    .get_result(conn)
    .await?;
    Ok(r.n)
}

/// Drop cross-fold cursors nothing will read again. 2 days matches the
/// rebuild horizon in [`fold_day_from_raw`].
pub async fn prune_state(conn: &mut AsyncPgConnection) -> diesel::QueryResult<usize> {
    let a = diesel::sql_query(
        "DELETE FROM rollup_session_state WHERE updated_at < now() - interval '2 days'",
    )
    .execute(conn)
    .await?;
    let b = diesel::sql_query(
        "DELETE FROM rollup_journey_state WHERE updated_at < now() - interval '2 days'",
    )
    .execute(conn)
    .await?;
    Ok(a + b)
}

// ---------------------------------------------------------------------------
// Backfill: pre-epoch history, day by day.
// ---------------------------------------------------------------------------

#[derive(QueryableByName)]
struct MinDayRow {
    #[diesel(sql_type = Nullable<Timestamptz>)]
    t: Option<DateTime<Utc>>,
}

/// One-shot, operator-run (`sauron-migrate backfill-rollups`). Aggregates
/// `received_at <= epoch` day by day, recomputes all session days, then marks
/// every existing app ready in the same final transaction as the last write.
/// Idempotent in effect only when rollups are empty for the covered range —
/// re-running against already-backfilled tables double-adds, which is why the
/// runner refuses when any marker row exists.
pub async fn backfill_all(
    conn: &mut AsyncPgConnection,
    name_cap: usize,
    mut progress: impl FnMut(NaiveDate),
) -> diesel::QueryResult<()> {
    let already: CountRow = diesel::sql_query("SELECT count(*)::bigint AS n FROM rollup_backfill")
        .get_result(conn)
        .await?;
    if already.n > 0 {
        tracing::warn!(
            "rollup_backfill markers already present; skipping the ADDITIVE event backfill \
             (a second run would double-count) — refreshing the session-day rollups only, \
             which are REPLACE-semantics and safe to recompute in place"
        );
        recompute_sessions(conn, None).await?;
        return Ok(());
    }
    let epoch = super::epoch(conn).await?;
    let mut min_day: Option<NaiveDate> = None;
    for tbl in ["analytics_events", "error_events", "transactions"] {
        let r: MinDayRow = diesel::sql_query(format!("SELECT min(occurred_at) AS t FROM {tbl}"))
            .get_result(conn)
            .await?;
        if let Some(t) = r.t {
            let d = t.date_naive();
            min_day = Some(min_day.map_or(d, |m: NaiveDate| m.min(d)));
        }
    }
    let Some(mut day) = min_day else {
        // No events at all: nothing to aggregate, but the apps still need
        // their markers so reads take the rollup path from here on.
        begin_locked(conn).await?;
        let out = super::mark_all_backfilled(conn).await.map(|_| ());
        return finish(conn, out).await;
    };
    let last = epoch.date_naive();
    while day <= last {
        fold_day_from_raw(conn, day, Some(epoch), false, name_cap).await?;
        progress(day);
        day += Duration::days(1);
    }
    recompute_sessions(conn, None).await?;
    begin_locked(conn).await?;
    let out = super::mark_all_backfilled(conn).await.map(|_| ());
    finish(conn, out).await
}

#[cfg(test)]
mod tests {
    use super::super::NIL_ENV;
    use super::*;

    fn arow(
        app: Uuid,
        session: &str,
        distinct: &str,
        name: &str,
        screen: Option<&str>,
        at: DateTime<Utc>,
    ) -> ARow {
        ARow {
            app_id: app,
            environment_id: None,
            occurred_at: at,
            received_at: at,
            name: name.to_string(),
            screen: screen.map(|s| s.to_string()),
            distinct_id: distinct.to_string(),
            session_id: Some(session.to_string()),
        }
    }

    fn ts(s: &str) -> DateTime<Utc> {
        s.parse().expect("ts")
    }

    #[test]
    fn dwell_chains_screen_carrying_events_and_caps() {
        let app = Uuid::new_v4();
        let mut sess = HashMap::new();
        let mut jour = HashMap::new();
        let rows = vec![
            arow(
                app,
                "s1",
                "u1",
                "$screen",
                Some("Home"),
                ts("2026-08-25T10:00:00Z"),
            ),
            // Screen-carrying non-view event: closes Home's first gap AND
            // opens its own (still credited to Home).
            arow(
                app,
                "s1",
                "u1",
                "tap",
                Some("Home"),
                ts("2026-08-25T10:00:05Z"),
            ),
            arow(
                app,
                "s1",
                "u1",
                "$screen",
                Some("Cart"),
                ts("2026-08-25T10:00:10Z"),
            ),
            // Screen-less event: invisible to the dwell chain.
            arow(
                app,
                "s1",
                "u1",
                "checkout",
                None,
                ts("2026-08-25T10:00:20Z"),
            ),
            // 45 min later: closes Cart's gap, capped at 30 min.
            arow(
                app,
                "s1",
                "u1",
                "tap",
                Some("Cart"),
                ts("2026-08-25T10:45:10Z"),
            ),
        ];
        let d = fold_analytics_rows(&rows, &mut sess, &mut jour, 2000);
        let day = ts("2026-08-25T10:00:00Z").date_naive();
        let home = &d.screens[&((app, None, day), "Home".to_string())];
        let cart = &d.screens[&((app, None, day), "Cart".to_string())];
        assert_eq!(home.dwell_ms, 10_000.0); // 0->5s and 5s->10s, both Home's
        assert_eq!(home.views, 1);
        assert_eq!(home.events, 1);
        assert_eq!(cart.dwell_ms, 1_800_000.0);
        // The trailing screen-carrying tap is the new pending row.
        assert_eq!(
            sess[&(app, "s1".to_string())].pending_screen.as_deref(),
            Some("Cart")
        );
    }

    #[test]
    fn dwell_survives_a_fold_boundary_via_state() {
        let app = Uuid::new_v4();
        let mut sess = HashMap::new();
        let mut jour = HashMap::new();
        let first = vec![arow(
            app,
            "s1",
            "u1",
            "$screen",
            Some("Home"),
            ts("2026-08-25T10:00:00Z"),
        )];
        let d1 = fold_analytics_rows(&first, &mut sess, &mut jour, 2000);
        let day = ts("2026-08-25T10:00:00Z").date_naive();
        assert_eq!(
            d1.screens[&((app, None, day), "Home".to_string())].dwell_ms,
            0.0
        );
        assert_eq!(
            sess[&(app, "s1".to_string())].pending_screen.as_deref(),
            Some("Home")
        );
        // Next fold: a screen-carrying event closes the pending gap.
        let second = vec![arow(
            app,
            "s1",
            "u1",
            "tap",
            Some("Home"),
            ts("2026-08-25T10:00:07Z"),
        )];
        let d2 = fold_analytics_rows(&second, &mut sess, &mut jour, 2000);
        assert_eq!(
            d2.screens[&((app, None, day), "Home".to_string())].dwell_ms,
            7_000.0
        );
    }

    #[test]
    fn journey_steps_continue_across_folds_and_stop_at_ten() {
        let app = Uuid::new_v4();
        let mut sess = HashMap::new();
        let mut jour = HashMap::new();
        let day = ts("2026-08-25T00:00:00Z").date_naive();
        let first: Vec<ARow> = (0..3)
            .map(|i| {
                arow(
                    app,
                    "s1",
                    "u1",
                    &format!("e{i}"),
                    None,
                    ts("2026-08-25T10:00:00Z") + Duration::seconds(i),
                )
            })
            .collect();
        let d1 = fold_analytics_rows(&first, &mut sess, &mut jour, 2000);
        assert_eq!(d1.nodes[&((app, None, day), 0, "e0".to_string())], 1);
        assert_eq!(
            d1.links[&((app, None, day), 1, "e1".to_string(), "e2".to_string())],
            1
        );
        // Second fold continues at step 3 and links from e2.
        let second: Vec<ARow> = (3..12)
            .map(|i| {
                arow(
                    app,
                    "s1",
                    "u1",
                    &format!("e{i}"),
                    None,
                    ts("2026-08-25T10:01:00Z") + Duration::seconds(i),
                )
            })
            .collect();
        let d2 = fold_analytics_rows(&second, &mut sess, &mut jour, 2000);
        assert_eq!(d2.nodes[&((app, None, day), 3, "e3".to_string())], 1);
        assert_eq!(
            d2.links[&((app, None, day), 2, "e2".to_string(), "e3".to_string())],
            1
        );
        // Steps 10+ are dropped: nodes exist only for steps 3..=9 here.
        assert!(d2.nodes.keys().all(|(_, s, _)| *s < 10));
        assert_eq!(jour[&(app, day, "u1".to_string(), NIL_ENV)].steps, 10);
    }

    #[test]
    fn anonymous_rows_join_counts_but_not_journeys_or_users() {
        let app = Uuid::new_v4();
        let mut sess = HashMap::new();
        let mut jour = HashMap::new();
        let rows = vec![arow(
            app,
            "s1",
            "",
            "boot",
            Some("Home"),
            ts("2026-08-25T10:00:00Z"),
        )];
        let d = fold_analytics_rows(&rows, &mut sess, &mut jour, 2000);
        let day = ts("2026-08-25T10:00:00Z").date_naive();
        let a = &d.activity[&(app, None, day)];
        assert_eq!(a.events, 1);
        assert_eq!(a.hll_all.estimate(), 0);
        assert!(d.nodes.is_empty());
        assert_eq!(
            d.screens[&((app, None, day), "Home".to_string())]
                .users
                .estimate(),
            0
        );
    }

    #[test]
    fn name_cap_folds_tail_into_other() {
        let app = Uuid::new_v4();
        let mut sess = HashMap::new();
        let mut jour = HashMap::new();
        let rows: Vec<ARow> = (0..10)
            .map(|i| {
                arow(
                    app,
                    "s1",
                    "u1",
                    &format!("n{i}"),
                    None,
                    ts("2026-08-25T10:00:00Z"),
                )
            })
            .collect();
        let d = fold_analytics_rows(&rows, &mut sess, &mut jour, 4);
        let day = ts("2026-08-25T10:00:00Z").date_naive();
        let names: Vec<&String> = d.top.keys().map(|(_, n)| n).collect();
        assert_eq!(names.len(), 4);
        assert_eq!(d.top[&((app, None, day), OTHER_NAME.to_string())], 7);
    }

    #[test]
    fn split_boundary_keeps_strictly_older_rows() {
        let t0 = ts("2026-08-25T10:00:00Z");
        let recv: Vec<DateTime<Utc>> =
            vec![t0, t0, t0 + Duration::seconds(1), t0 + Duration::seconds(2)];
        // max=3: overflow row is t0+2s; keep everything strictly older.
        assert_eq!(split_at_boundary(&recv, 3), 3);
        // All rows share one timestamp: keep resolves to 0 → caller fetches whole ts.
        let same = vec![t0, t0, t0, t0];
        assert_eq!(split_at_boundary(&same, 3), 0);
    }

    #[test]
    fn transactions_fold_errors_and_histogram() {
        let app = Uuid::new_v4();
        let rows = vec![
            TRow {
                app_id: app,
                environment_id: None,
                occurred_at: ts("2026-08-25T10:15:00Z"),
                received_at: ts("2026-08-25T10:15:00Z"),
                name: "GET /api".into(),
                op: "http.server".into(),
                duration_ms: 120.0,
                status: None,
                http_status: Some(200),
            },
            TRow {
                app_id: app,
                environment_id: None,
                occurred_at: ts("2026-08-25T10:45:00Z"),
                received_at: ts("2026-08-25T10:45:00Z"),
                name: "GET /api".into(),
                op: "http.server".into(),
                duration_ms: 300.0,
                status: None,
                http_status: Some(503),
            },
        ];
        let d = fold_transaction_rows(&rows, 2000);
        let hour = ts("2026-08-25T10:00:00Z");
        let e = &d[&(
            (app, None, hour),
            "GET /api".to_string(),
            "http.server".to_string(),
        )];
        assert_eq!(e.count, 2);
        assert_eq!(e.error_count, 1);
        assert_eq!(e.duration_sum, 420.0);
        assert_eq!(e.hist.total(), 2);
    }

    #[test]
    fn session_rows_bucket_by_started_day() {
        let app = Uuid::new_v4();
        let rows = vec![SessionRow {
            app_id: app,
            environment_id: None,
            device_key: Some("dev-1".into()),
            started_at: ts("2026-08-25T23:50:00Z"),
            last_event_at: ts("2026-08-26T00:20:00Z"),
            unhandled: 1,
        }];
        let days = session_rows_to_days(&rows);
        assert_eq!(days.len(), 1);
        assert_eq!(days[0].day, ts("2026-08-25T00:00:00Z").date_naive());
        assert_eq!(days[0].crashed, 1);
        assert_eq!(days[0].duration_ms_sum, 1_800_000.0);
        assert_eq!(days[0].fixed, [0, 0, 0, 0, 1]);
    }
}
