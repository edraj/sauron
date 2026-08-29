//! Dashboard rollups: watermarked incremental aggregates over the firehose
//! tables, so no request-path query ever aggregates raw event rows.
//!
//! # Shape
//!
//! Seven small tables (migration 71) hold per-day (per-hour for performance)
//! aggregates keyed by `(app, environment, bucket, …)`. A background task in
//! the ingest process ([`fold`]) folds newly *committed* rows — selected by
//! `received_at` behind a watermark, bucketed by `occurred_at` — so late
//! events land in their correct historical bucket exactly once. Reads branch
//! behind [`is_ready`] the same way `device_env_backfill::is_backfilled`
//! gates `/device-groups`.
//!
//! # Concurrency contract
//!
//! Every writing transaction here takes `pg_advisory_xact_lock` on one key,
//! so the fold task, an operator-run `sauron-migrate backfill-rollups`, and a
//! consistency rebuild can never interleave their read-merge-write cycles.
//! The Redis leader key in `sauron-pipeline::rollup_task` is an efficiency
//! (don't duplicate work across replicas); the advisory lock is the
//! correctness. HLL/histogram columns cannot be merged in SQL, so
//! sketch-bearing upserts are read-FOR UPDATE → merge in Rust → full-replace
//! write; count-only tables use plain additive `ON CONFLICT` arms.
//!
//! # Epoch split
//!
//! `rollup_epoch.started_at` (stamped by migration 71, the migration-70
//! lesson) divides history: the live fold owns `received_at > epoch`, the
//! one-shot backfill owns `received_at <= epoch`. The two never double-count
//! by construction.

use std::collections::BTreeMap;

use chrono::{DateTime, NaiveDate, Utc};
use diesel::sql_types::{
    Array, BigInt, Bytea, Date, Double, Nullable, SmallInt, Text, Timestamptz, Uuid as SqlUuid,
};
use diesel::QueryableByName;
use diesel_async::{AsyncPgConnection, RunQueryDsl, SimpleAsyncConnection};
use uuid::Uuid;

use crate::sketch::{Hll, LatencyHistogram};

pub mod fold;
pub mod person_days;
pub mod read;

pub use person_days::{add_person_days, PersonDayDelta, PersonKey};

pub const SRC_ANALYTICS: &str = "analytics_events";
pub const SRC_ERRORS: &str = "error_events";
pub const SRC_TRANSACTIONS: &str = "transactions";
pub const SRC_SESSIONS: &str = "sessions";
/// The received_at-watermarked sources (sessions is a recompute stamp).
pub const EVENT_SOURCES: [&str; 3] = [SRC_ANALYTICS, SRC_ERRORS, SRC_TRANSACTIONS];

/// The COALESCE key for NULL environments, mirroring every rollup unique
/// index. Rows store real NULL; only keys and state PKs use this.
pub const NIL_ENV: Uuid = Uuid::nil();

/// The fold's own overflow bucket for pathological name cardinality.
pub const OTHER_NAME: &str = "~other";

/// Redis key the API sets to ask the fold task for an immediate cycle
/// (the Refresh button). Shared here because both sauron-api (writer) and
/// sauron-pipeline (poller) must agree on it.
pub const KICK_KEY: &str = "sauron:rollups:kick";

const ADVISORY_LOCK: &str = "SELECT pg_advisory_xact_lock(hashtext('sauron_rollups'))";

/// Rows per upsert statement. Sketch-bearing rows carry ~4.5 KiB each, so the
/// house INSERT_CHUNK of 1000 keeps a statement under ~5 MiB.
const CHUNK: usize = 1000;

#[derive(QueryableByName)]
struct TsRow {
    #[diesel(sql_type = Timestamptz)]
    t: DateTime<Utc>,
}

#[derive(QueryableByName)]
struct OptTsRow {
    #[diesel(sql_type = Nullable<Timestamptz>)]
    t: Option<DateTime<Utc>>,
}

#[derive(QueryableByName)]
struct BoolRow {
    #[diesel(sql_type = diesel::sql_types::Bool)]
    present: bool,
}

/// `BEGIN` + the module-wide advisory lock. Pair with `COMMIT`/`ROLLBACK` via
/// [`rollback_quietly`] on the error path.
pub(crate) async fn begin_locked(conn: &mut AsyncPgConnection) -> diesel::QueryResult<()> {
    conn.batch_execute("BEGIN").await?;
    diesel::sql_query(ADVISORY_LOCK).execute(conn).await?;
    Ok(())
}

/// Best-effort rollback: the original error is what the caller reports, and a
/// rollback failure on an already-broken connection adds nothing.
pub(crate) async fn rollback_quietly(conn: &mut AsyncPgConnection) {
    let _ = conn.batch_execute("ROLLBACK").await;
}

pub async fn epoch(conn: &mut AsyncPgConnection) -> diesel::QueryResult<DateTime<Utc>> {
    let r: TsRow = diesel::sql_query("SELECT started_at AS t FROM rollup_epoch")
        .get_result(conn)
        .await?;
    Ok(r.t)
}

pub async fn watermark(
    conn: &mut AsyncPgConnection,
    source: &str,
) -> diesel::QueryResult<DateTime<Utc>> {
    let r: TsRow =
        diesel::sql_query("SELECT watermark AS t FROM rollup_watermarks WHERE source = $1")
            .bind::<Text, _>(source)
            .get_result(conn)
            .await?;
    Ok(r.t)
}

/// Advance a source's watermark. Runs inside the caller's fold transaction so
/// a crash re-folds nothing and loses nothing. `GREATEST` for the same reason
/// tiering's `advance_watermark` has it: a watermark never moves backward.
pub async fn set_watermark(
    conn: &mut AsyncPgConnection,
    source: &str,
    wm: DateTime<Utc>,
) -> diesel::QueryResult<()> {
    diesel::sql_query(
        "UPDATE rollup_watermarks SET watermark = GREATEST(watermark, $2), updated_at = now() \
         WHERE source = $1",
    )
    .bind::<Text, _>(source)
    .bind::<Timestamptz, _>(wm)
    .execute(conn)
    .await?;
    Ok(())
}

/// The freshness stamp responses expose as `as_of`: the oldest watermark of
/// the sources feeding the answer. `None` when a source row is missing —
/// treated as not-ready rather than pretending freshness.
pub async fn as_of(
    conn: &mut AsyncPgConnection,
    sources: &[&str],
) -> diesel::QueryResult<Option<DateTime<Utc>>> {
    let r: OptTsRow = diesel::sql_query(
        "SELECT CASE WHEN count(*) = cardinality($1::text[]) THEN min(watermark) END AS t \
         FROM rollup_watermarks WHERE source = ANY($1)",
    )
    .bind::<Array<Text>, _>(sources.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    .get_result(conn)
    .await?;
    Ok(r.t)
}

/// The query-shape gate, `device_env_backfill::is_backfilled`'s twin. An app
/// created at-or-after the epoch is implicitly ready: every row it will ever
/// have is post-epoch and therefore folded live.
pub async fn is_ready(conn: &mut AsyncPgConnection, app_id: Uuid) -> diesel::QueryResult<bool> {
    let r: BoolRow = diesel::sql_query(
        "SELECT EXISTS (SELECT 1 FROM rollup_backfill WHERE app_id = $1) \
             OR EXISTS (SELECT 1 FROM apps a, rollup_epoch e \
                        WHERE a.id = $1 AND a.created_at >= e.started_at) AS present",
    )
    .bind::<SqlUuid, _>(app_id)
    .get_result(conn)
    .await?;
    Ok(r.present)
}

/// Whether pre-epoch history still awaits `sauron-migrate backfill-rollups`:
/// at least one app predates the epoch and no marker rows exist yet. The
/// maintenance pass gates on this — rebuilding a day before backfill covers
/// it would double-count once the backfill adds its pre-epoch contributions.
pub async fn backfill_pending(conn: &mut AsyncPgConnection) -> diesel::QueryResult<bool> {
    let r: BoolRow = diesel::sql_query(
        "SELECT NOT EXISTS (SELECT 1 FROM rollup_backfill)             AND EXISTS (SELECT 1 FROM apps a, rollup_epoch e WHERE a.created_at < e.started_at)             AS present",
    )
    .get_result(conn)
    .await?;
    Ok(r.present)
}

/// Marker write for every app that exists right now. Called by the backfill
/// inside its final transaction — the marker must never be visible before the
/// aggregates it claims (device_env_backfill:88 rule).
pub async fn mark_all_backfilled(conn: &mut AsyncPgConnection) -> diesel::QueryResult<usize> {
    diesel::sql_query(
        "INSERT INTO rollup_backfill (app_id) SELECT id FROM apps \
         ON CONFLICT (app_id) DO UPDATE SET completed_at = now()",
    )
    .execute(conn)
    .await
}

// ---------------------------------------------------------------------------
// Delta types. Fold passes accumulate these in maps (deduped by construction)
// and the upsert helpers below apply them sorted by key (lock-ordering rule).
// ---------------------------------------------------------------------------

#[derive(Default, Clone)]
pub struct ScreenDelta {
    pub views: i64,
    pub events: i64,
    pub exceptions: i64,
    pub users: Hll,
    pub dwell_ms: f64,
}

#[derive(Default, Clone)]
pub struct UserActivityDelta {
    pub hll_all: Hll,
    pub hll_analytics: Hll,
    pub events: i64,
    pub errors: i64,
}

#[derive(Default, Clone)]
pub struct PerfDelta {
    pub count: i64,
    pub error_count: i64,
    pub duration_sum: f64,
    pub hist: LatencyHistogram,
}

/// One recomputed (app, env, started-day) sessions bucket — REPLACE, not add.
pub struct SessionDayRow {
    pub app: Uuid,
    pub env: Option<Uuid>,
    pub day: NaiveDate,
    pub sessions: i64,
    pub crashed: i64,
    pub duration_ms_sum: f64,
    pub hist: LatencyHistogram,
    /// `[<10s, 10-60s, 1-5m, 5-30m, 30m+]`, mirroring DURATION_BUCKET_CASE_SQL.
    pub fixed: [i64; 5],
}

/// (app, env-as-stored, day) — env is the real nullable column value.
pub type DayKey = (Uuid, Option<Uuid>, NaiveDate);

/// ((app, env, hour), name, op) — the perf_agg_hourly conflict key.
pub type PerfKey = ((Uuid, Option<Uuid>, DateTime<Utc>), String, String);

fn env_key(env: &Option<Uuid>) -> Uuid {
    env.unwrap_or(NIL_ENV)
}

fn opt_hll(h: &Hll) -> Option<Vec<u8>> {
    (!h.is_empty()).then(|| h.to_bytes())
}

// ---------------------------------------------------------------------------
// Additive upserts (count-only tables).
// ---------------------------------------------------------------------------

/// `(app, env, day, name) -> count`
pub async fn add_event_top(
    conn: &mut AsyncPgConnection,
    deltas: &BTreeMap<(DayKey, String), i64>,
) -> diesel::QueryResult<()> {
    for chunk in deltas.iter().collect::<Vec<_>>().chunks(CHUNK) {
        diesel::sql_query(
            "INSERT INTO event_top_daily (app_id, environment_id, day, name, count) \
             SELECT app_id, env, day, name, count \
             FROM unnest($1::uuid[], $2::uuid[], $3::date[], $4::text[], $5::bigint[]) \
                  AS t(app_id, env, day, name, count) \
             ON CONFLICT (app_id, day, name, COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid)) \
             DO UPDATE SET count = event_top_daily.count + EXCLUDED.count",
        )
        .bind::<Array<SqlUuid>, _>(chunk.iter().map(|((k, _), _)| k.0).collect::<Vec<_>>())
        .bind::<Array<Nullable<SqlUuid>>, _>(chunk.iter().map(|((k, _), _)| k.1).collect::<Vec<_>>())
        .bind::<Array<Date>, _>(chunk.iter().map(|((k, _), _)| k.2).collect::<Vec<_>>())
        .bind::<Array<Text>, _>(chunk.iter().map(|((_, n), _)| n.clone()).collect::<Vec<_>>())
        .bind::<Array<BigInt>, _>(chunk.iter().map(|(_, c)| **c).collect::<Vec<_>>())
        .execute(conn)
        .await?;
    }
    Ok(())
}

/// `(app, env, day, step, name) -> count`
pub async fn add_journey_nodes(
    conn: &mut AsyncPgConnection,
    deltas: &BTreeMap<(DayKey, i16, String), i64>,
) -> diesel::QueryResult<()> {
    for chunk in deltas.iter().collect::<Vec<_>>().chunks(CHUNK) {
        diesel::sql_query(
            "INSERT INTO journey_nodes_daily (app_id, environment_id, day, step, name, count) \
             SELECT app_id, env, day, step, name, count \
             FROM unnest($1::uuid[], $2::uuid[], $3::date[], $4::smallint[], $5::text[], $6::bigint[]) \
                  AS t(app_id, env, day, step, name, count) \
             ON CONFLICT (app_id, day, step, name, COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid)) \
             DO UPDATE SET count = journey_nodes_daily.count + EXCLUDED.count",
        )
        .bind::<Array<SqlUuid>, _>(chunk.iter().map(|((k, _, _), _)| k.0).collect::<Vec<_>>())
        .bind::<Array<Nullable<SqlUuid>>, _>(
            chunk.iter().map(|((k, _, _), _)| k.1).collect::<Vec<_>>(),
        )
        .bind::<Array<Date>, _>(chunk.iter().map(|((k, _, _), _)| k.2).collect::<Vec<_>>())
        .bind::<Array<SmallInt>, _>(chunk.iter().map(|((_, s, _), _)| *s).collect::<Vec<_>>())
        .bind::<Array<Text>, _>(chunk.iter().map(|((_, _, n), _)| n.clone()).collect::<Vec<_>>())
        .bind::<Array<BigInt>, _>(chunk.iter().map(|(_, c)| **c).collect::<Vec<_>>())
        .execute(conn)
        .await?;
    }
    Ok(())
}

/// `(app, env, day, step, from, to) -> count`
pub async fn add_journey_links(
    conn: &mut AsyncPgConnection,
    deltas: &BTreeMap<(DayKey, i16, String, String), i64>,
) -> diesel::QueryResult<()> {
    for chunk in deltas.iter().collect::<Vec<_>>().chunks(CHUNK) {
        diesel::sql_query(
            "INSERT INTO journey_links_daily (app_id, environment_id, day, step, from_name, to_name, count) \
             SELECT app_id, env, day, step, from_name, to_name, count \
             FROM unnest($1::uuid[], $2::uuid[], $3::date[], $4::smallint[], $5::text[], $6::text[], $7::bigint[]) \
                  AS t(app_id, env, day, step, from_name, to_name, count) \
             ON CONFLICT (app_id, day, step, from_name, to_name, COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid)) \
             DO UPDATE SET count = journey_links_daily.count + EXCLUDED.count",
        )
        .bind::<Array<SqlUuid>, _>(chunk.iter().map(|((k, ..), _)| k.0).collect::<Vec<_>>())
        .bind::<Array<Nullable<SqlUuid>>, _>(chunk.iter().map(|((k, ..), _)| k.1).collect::<Vec<_>>())
        .bind::<Array<Date>, _>(chunk.iter().map(|((k, ..), _)| k.2).collect::<Vec<_>>())
        .bind::<Array<SmallInt>, _>(chunk.iter().map(|((_, s, _, _), _)| *s).collect::<Vec<_>>())
        .bind::<Array<Text>, _>(
            chunk.iter().map(|((_, _, f, _), _)| f.clone()).collect::<Vec<_>>(),
        )
        .bind::<Array<Text>, _>(
            chunk.iter().map(|((_, _, _, t), _)| t.clone()).collect::<Vec<_>>(),
        )
        .bind::<Array<BigInt>, _>(chunk.iter().map(|(_, c)| **c).collect::<Vec<_>>())
        .execute(conn)
        .await?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Read-merge-write upserts (sketch-bearing tables). Caller holds the advisory
// lock; FOR UPDATE is belt-and-braces against a future second writer.
// ---------------------------------------------------------------------------

#[derive(QueryableByName)]
struct ExistingScreen {
    #[diesel(sql_type = SqlUuid)]
    app_id: Uuid,
    #[diesel(sql_type = SqlUuid)]
    env_key: Uuid,
    #[diesel(sql_type = Date)]
    day: NaiveDate,
    #[diesel(sql_type = Text)]
    screen: String,
    #[diesel(sql_type = BigInt)]
    views: i64,
    #[diesel(sql_type = BigInt)]
    events: i64,
    #[diesel(sql_type = BigInt)]
    exceptions: i64,
    #[diesel(sql_type = Nullable<Bytea>)]
    users_hll: Option<Vec<u8>>,
    #[diesel(sql_type = Double)]
    dwell_ms_sum: f64,
}

/// `(app, env, day, screen) -> delta`, merged over current rows then replaced.
pub async fn add_screen_stats(
    conn: &mut AsyncPgConnection,
    deltas: &mut BTreeMap<(DayKey, String), ScreenDelta>,
) -> diesel::QueryResult<()> {
    let keys: Vec<_> = deltas.keys().cloned().collect();
    for kchunk in keys.chunks(CHUNK) {
        let existing: Vec<ExistingScreen> = diesel::sql_query(
            "SELECT s.app_id, COALESCE(s.environment_id, '00000000-0000-0000-0000-000000000000'::uuid) AS env_key, \
                    s.day, s.screen, s.views, s.events, s.exceptions, s.users_hll, s.dwell_ms_sum \
             FROM screen_stats_daily s \
             JOIN unnest($1::uuid[], $2::uuid[], $3::date[], $4::text[]) AS k(app_id, env_key, day, screen) \
               ON s.app_id = k.app_id AND s.day = k.day AND s.screen = k.screen \
              AND COALESCE(s.environment_id, '00000000-0000-0000-0000-000000000000'::uuid) = k.env_key \
             FOR UPDATE OF s",
        )
        .bind::<Array<SqlUuid>, _>(kchunk.iter().map(|(k, _)| k.0).collect::<Vec<_>>())
        .bind::<Array<SqlUuid>, _>(kchunk.iter().map(|(k, _)| env_key(&k.1)).collect::<Vec<_>>())
        .bind::<Array<Date>, _>(kchunk.iter().map(|(k, _)| k.2).collect::<Vec<_>>())
        .bind::<Array<Text>, _>(kchunk.iter().map(|(_, s)| s.clone()).collect::<Vec<_>>())
        .get_results(conn)
        .await?;
        for ex in existing {
            let env = (ex.env_key != NIL_ENV).then_some(ex.env_key);
            if let Some(d) = deltas.get_mut(&((ex.app_id, env, ex.day), ex.screen.clone())) {
                d.views += ex.views;
                d.events += ex.events;
                d.exceptions += ex.exceptions;
                d.dwell_ms += ex.dwell_ms_sum;
                d.users.merge(&Hll::from_opt(ex.users_hll.as_ref()));
            }
        }
    }
    let rows: Vec<_> = deltas.iter().collect();
    for chunk in rows.chunks(CHUNK) {
        diesel::sql_query(
            "INSERT INTO screen_stats_daily \
                 (app_id, environment_id, day, screen, views, events, exceptions, users_hll, dwell_ms_sum, updated_at) \
             SELECT app_id, env, day, screen, views, events, exceptions, users_hll, dwell, now() \
             FROM unnest($1::uuid[], $2::uuid[], $3::date[], $4::text[], $5::bigint[], $6::bigint[], $7::bigint[], $8::bytea[], $9::float8[]) \
                  AS t(app_id, env, day, screen, views, events, exceptions, users_hll, dwell) \
             ON CONFLICT (app_id, day, screen, COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid)) \
             DO UPDATE SET views = EXCLUDED.views, events = EXCLUDED.events, \
                           exceptions = EXCLUDED.exceptions, users_hll = EXCLUDED.users_hll, \
                           dwell_ms_sum = EXCLUDED.dwell_ms_sum, updated_at = now()",
        )
        .bind::<Array<SqlUuid>, _>(chunk.iter().map(|((k, _), _)| k.0).collect::<Vec<_>>())
        .bind::<Array<Nullable<SqlUuid>>, _>(chunk.iter().map(|((k, _), _)| k.1).collect::<Vec<_>>())
        .bind::<Array<Date>, _>(chunk.iter().map(|((k, _), _)| k.2).collect::<Vec<_>>())
        .bind::<Array<Text>, _>(chunk.iter().map(|((_, s), _)| s.clone()).collect::<Vec<_>>())
        .bind::<Array<BigInt>, _>(chunk.iter().map(|(_, d)| d.views).collect::<Vec<_>>())
        .bind::<Array<BigInt>, _>(chunk.iter().map(|(_, d)| d.events).collect::<Vec<_>>())
        .bind::<Array<BigInt>, _>(chunk.iter().map(|(_, d)| d.exceptions).collect::<Vec<_>>())
        .bind::<Array<Nullable<Bytea>>, _>(chunk.iter().map(|(_, d)| opt_hll(&d.users)).collect::<Vec<_>>())
        .bind::<Array<Double>, _>(chunk.iter().map(|(_, d)| d.dwell_ms).collect::<Vec<_>>())
        .execute(conn)
        .await?;
    }
    Ok(())
}

#[derive(QueryableByName)]
struct ExistingActivity {
    #[diesel(sql_type = SqlUuid)]
    app_id: Uuid,
    #[diesel(sql_type = SqlUuid)]
    env_key: Uuid,
    #[diesel(sql_type = Date)]
    day: NaiveDate,
    #[diesel(sql_type = Nullable<Bytea>)]
    hll_all: Option<Vec<u8>>,
    #[diesel(sql_type = Nullable<Bytea>)]
    hll_analytics: Option<Vec<u8>>,
    #[diesel(sql_type = BigInt)]
    events: i64,
    #[diesel(sql_type = BigInt)]
    errors: i64,
}

/// `(app, env, day) -> delta`, merged then replaced.
pub async fn add_user_activity(
    conn: &mut AsyncPgConnection,
    deltas: &mut BTreeMap<DayKey, UserActivityDelta>,
) -> diesel::QueryResult<()> {
    let keys: Vec<_> = deltas.keys().cloned().collect();
    for kchunk in keys.chunks(CHUNK) {
        let existing: Vec<ExistingActivity> = diesel::sql_query(
            "SELECT u.app_id, COALESCE(u.environment_id, '00000000-0000-0000-0000-000000000000'::uuid) AS env_key, \
                    u.day, u.hll_all, u.hll_analytics, u.events, u.errors \
             FROM user_activity_daily u \
             JOIN unnest($1::uuid[], $2::uuid[], $3::date[]) AS k(app_id, env_key, day) \
               ON u.app_id = k.app_id AND u.day = k.day \
              AND COALESCE(u.environment_id, '00000000-0000-0000-0000-000000000000'::uuid) = k.env_key \
             FOR UPDATE OF u",
        )
        .bind::<Array<SqlUuid>, _>(kchunk.iter().map(|k| k.0).collect::<Vec<_>>())
        .bind::<Array<SqlUuid>, _>(kchunk.iter().map(|k| env_key(&k.1)).collect::<Vec<_>>())
        .bind::<Array<Date>, _>(kchunk.iter().map(|k| k.2).collect::<Vec<_>>())
        .get_results(conn)
        .await?;
        for ex in existing {
            let env = (ex.env_key != NIL_ENV).then_some(ex.env_key);
            if let Some(d) = deltas.get_mut(&(ex.app_id, env, ex.day)) {
                d.events += ex.events;
                d.errors += ex.errors;
                d.hll_all.merge(&Hll::from_opt(ex.hll_all.as_ref()));
                d.hll_analytics
                    .merge(&Hll::from_opt(ex.hll_analytics.as_ref()));
            }
        }
    }
    let rows: Vec<_> = deltas.iter().collect();
    for chunk in rows.chunks(CHUNK) {
        diesel::sql_query(
            "INSERT INTO user_activity_daily \
                 (app_id, environment_id, day, hll_all, hll_analytics, events, errors, updated_at) \
             SELECT app_id, env, day, hll_all, hll_analytics, events, errors, now() \
             FROM unnest($1::uuid[], $2::uuid[], $3::date[], $4::bytea[], $5::bytea[], $6::bigint[], $7::bigint[]) \
                  AS t(app_id, env, day, hll_all, hll_analytics, events, errors) \
             ON CONFLICT (app_id, day, COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid)) \
             DO UPDATE SET hll_all = EXCLUDED.hll_all, hll_analytics = EXCLUDED.hll_analytics, \
                           events = EXCLUDED.events, errors = EXCLUDED.errors, updated_at = now()",
        )
        .bind::<Array<SqlUuid>, _>(chunk.iter().map(|(k, _)| k.0).collect::<Vec<_>>())
        .bind::<Array<Nullable<SqlUuid>>, _>(chunk.iter().map(|(k, _)| k.1).collect::<Vec<_>>())
        .bind::<Array<Date>, _>(chunk.iter().map(|(k, _)| k.2).collect::<Vec<_>>())
        .bind::<Array<Nullable<Bytea>>, _>(chunk.iter().map(|(_, d)| opt_hll(&d.hll_all)).collect::<Vec<_>>())
        .bind::<Array<Nullable<Bytea>>, _>(
            chunk.iter().map(|(_, d)| opt_hll(&d.hll_analytics)).collect::<Vec<_>>(),
        )
        .bind::<Array<BigInt>, _>(chunk.iter().map(|(_, d)| d.events).collect::<Vec<_>>())
        .bind::<Array<BigInt>, _>(chunk.iter().map(|(_, d)| d.errors).collect::<Vec<_>>())
        .execute(conn)
        .await?;
    }
    Ok(())
}

#[derive(QueryableByName)]
struct ExistingPerf {
    #[diesel(sql_type = SqlUuid)]
    app_id: Uuid,
    #[diesel(sql_type = SqlUuid)]
    env_key: Uuid,
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

/// `(app, env, hour, name, op) -> delta`, merged then replaced.
pub async fn add_perf_agg(
    conn: &mut AsyncPgConnection,
    deltas: &mut BTreeMap<PerfKey, PerfDelta>,
) -> diesel::QueryResult<()> {
    let keys: Vec<_> = deltas.keys().cloned().collect();
    for kchunk in keys.chunks(CHUNK) {
        let existing: Vec<ExistingPerf> = diesel::sql_query(
            "SELECT p.app_id, COALESCE(p.environment_id, '00000000-0000-0000-0000-000000000000'::uuid) AS env_key, \
                    p.hour, p.name, p.op, p.count, p.error_count, p.duration_sum, p.duration_hist \
             FROM perf_agg_hourly p \
             JOIN unnest($1::uuid[], $2::uuid[], $3::timestamptz[], $4::text[], $5::text[]) \
                  AS k(app_id, env_key, hour, name, op) \
               ON p.app_id = k.app_id AND p.hour = k.hour AND p.name = k.name AND p.op = k.op \
              AND COALESCE(p.environment_id, '00000000-0000-0000-0000-000000000000'::uuid) = k.env_key \
             FOR UPDATE OF p",
        )
        .bind::<Array<SqlUuid>, _>(kchunk.iter().map(|(k, _, _)| k.0).collect::<Vec<_>>())
        .bind::<Array<SqlUuid>, _>(kchunk.iter().map(|(k, _, _)| env_key(&k.1)).collect::<Vec<_>>())
        .bind::<Array<Timestamptz>, _>(kchunk.iter().map(|(k, _, _)| k.2).collect::<Vec<_>>())
        .bind::<Array<Text>, _>(kchunk.iter().map(|(_, n, _)| n.clone()).collect::<Vec<_>>())
        .bind::<Array<Text>, _>(kchunk.iter().map(|(_, _, o)| o.clone()).collect::<Vec<_>>())
        .get_results(conn)
        .await?;
        for ex in existing {
            let env = (ex.env_key != NIL_ENV).then_some(ex.env_key);
            if let Some(d) =
                deltas.get_mut(&((ex.app_id, env, ex.hour), ex.name.clone(), ex.op.clone()))
            {
                d.count += ex.count;
                d.error_count += ex.error_count;
                d.duration_sum += ex.duration_sum;
                d.hist
                    .merge_counts(&LatencyHistogram::counts_from_bytes(&ex.duration_hist));
            }
        }
    }
    let rows: Vec<_> = deltas.iter().collect();
    for chunk in rows.chunks(CHUNK) {
        diesel::sql_query(
            "INSERT INTO perf_agg_hourly \
                 (app_id, environment_id, hour, name, op, count, error_count, duration_sum, duration_hist, updated_at) \
             SELECT app_id, env, hour, name, op, count, error_count, duration_sum, duration_hist, now() \
             FROM unnest($1::uuid[], $2::uuid[], $3::timestamptz[], $4::text[], $5::text[], $6::bigint[], $7::bigint[], $8::float8[], $9::bytea[]) \
                  AS t(app_id, env, hour, name, op, count, error_count, duration_sum, duration_hist) \
             ON CONFLICT (app_id, hour, name, op, COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid)) \
             DO UPDATE SET count = EXCLUDED.count, error_count = EXCLUDED.error_count, \
                           duration_sum = EXCLUDED.duration_sum, duration_hist = EXCLUDED.duration_hist, \
                           updated_at = now()",
        )
        .bind::<Array<SqlUuid>, _>(chunk.iter().map(|((k, _, _), _)| k.0).collect::<Vec<_>>())
        .bind::<Array<Nullable<SqlUuid>>, _>(chunk.iter().map(|((k, _, _), _)| k.1).collect::<Vec<_>>())
        .bind::<Array<Timestamptz>, _>(chunk.iter().map(|((k, _, _), _)| k.2).collect::<Vec<_>>())
        .bind::<Array<Text>, _>(chunk.iter().map(|((_, n, _), _)| n.clone()).collect::<Vec<_>>())
        .bind::<Array<Text>, _>(chunk.iter().map(|((_, _, o), _)| o.clone()).collect::<Vec<_>>())
        .bind::<Array<BigInt>, _>(chunk.iter().map(|(_, d)| d.count).collect::<Vec<_>>())
        .bind::<Array<BigInt>, _>(chunk.iter().map(|(_, d)| d.error_count).collect::<Vec<_>>())
        .bind::<Array<Double>, _>(chunk.iter().map(|(_, d)| d.duration_sum).collect::<Vec<_>>())
        .bind::<Array<Bytea>, _>(chunk.iter().map(|(_, d)| d.hist.to_bytes()).collect::<Vec<_>>())
        .execute(conn)
        .await?;
    }
    Ok(())
}

/// One recomputed (app, device, env, started-day) bucket for
/// `device_sessions_daily` — REPLACE, like its session_stats sibling.
pub struct DeviceDayRow {
    pub app: Uuid,
    pub device_key: String,
    pub env: Option<Uuid>,
    pub day: NaiveDate,
    pub sessions: i64,
}

/// REPLACE recomputed device-day session buckets: delete every dirty
/// (app, day), insert fresh. Same contract as [`replace_session_days`].
pub async fn replace_device_session_days(
    conn: &mut AsyncPgConnection,
    dirty: &[(Uuid, NaiveDate)],
    rows: &[DeviceDayRow],
) -> diesel::QueryResult<()> {
    for chunk in dirty.chunks(CHUNK * 4) {
        diesel::sql_query(
            "DELETE FROM device_sessions_daily s \
             USING unnest($1::uuid[], $2::date[]) AS d(app_id, day) \
             WHERE s.app_id = d.app_id AND s.day = d.day",
        )
        .bind::<Array<SqlUuid>, _>(chunk.iter().map(|(a, _)| *a).collect::<Vec<_>>())
        .bind::<Array<Date>, _>(chunk.iter().map(|(_, d)| *d).collect::<Vec<_>>())
        .execute(conn)
        .await?;
    }
    for chunk in rows.chunks(CHUNK * 4) {
        diesel::sql_query(
            "INSERT INTO device_sessions_daily \
                 (app_id, device_key, environment_id, day, sessions, updated_at) \
             SELECT app_id, device_key, env, day, sessions, now() \
             FROM unnest($1::uuid[], $2::text[], $3::uuid[], $4::date[], $5::bigint[]) \
                  AS t(app_id, device_key, env, day, sessions) \
             ON CONFLICT (app_id, day, device_key, COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid)) \
             DO UPDATE SET sessions = EXCLUDED.sessions, updated_at = now()",
        )
        .bind::<Array<SqlUuid>, _>(chunk.iter().map(|r| r.app).collect::<Vec<_>>())
        .bind::<Array<Text>, _>(chunk.iter().map(|r| r.device_key.clone()).collect::<Vec<_>>())
        .bind::<Array<Nullable<SqlUuid>>, _>(chunk.iter().map(|r| r.env).collect::<Vec<_>>())
        .bind::<Array<Date>, _>(chunk.iter().map(|r| r.day).collect::<Vec<_>>())
        .bind::<Array<BigInt>, _>(chunk.iter().map(|r| r.sessions).collect::<Vec<_>>())
        .execute(conn)
        .await?;
    }
    Ok(())
}

/// REPLACE recomputed session day buckets: delete every dirty (app, day),
/// then insert fresh rows. Caller supplies the full recomputed set for those
/// days, so delete-then-insert cannot lose a bucket.
pub async fn replace_session_days(
    conn: &mut AsyncPgConnection,
    dirty: &[(Uuid, NaiveDate)],
    rows: &[SessionDayRow],
) -> diesel::QueryResult<()> {
    for chunk in dirty.chunks(CHUNK * 4) {
        diesel::sql_query(
            "DELETE FROM session_stats_daily s \
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
            "INSERT INTO session_stats_daily \
                 (app_id, environment_id, day, sessions, crashed, duration_ms_sum, duration_hist, \
                  d_lt10s, d_10_60s, d_1_5m, d_5_30m, d_gte30m, updated_at) \
             SELECT app_id, env, day, sessions, crashed, dsum, dhist, b0, b1, b2, b3, b4, now() \
             FROM unnest($1::uuid[], $2::uuid[], $3::date[], $4::bigint[], $5::bigint[], $6::float8[], $7::bytea[], \
                         $8::bigint[], $9::bigint[], $10::bigint[], $11::bigint[], $12::bigint[]) \
                  AS t(app_id, env, day, sessions, crashed, dsum, dhist, b0, b1, b2, b3, b4) \
             ON CONFLICT (app_id, day, COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid)) \
             DO UPDATE SET sessions = EXCLUDED.sessions, crashed = EXCLUDED.crashed, \
                           duration_ms_sum = EXCLUDED.duration_ms_sum, duration_hist = EXCLUDED.duration_hist, \
                           d_lt10s = EXCLUDED.d_lt10s, d_10_60s = EXCLUDED.d_10_60s, d_1_5m = EXCLUDED.d_1_5m, \
                           d_5_30m = EXCLUDED.d_5_30m, d_gte30m = EXCLUDED.d_gte30m, updated_at = now()",
        )
        .bind::<Array<SqlUuid>, _>(chunk.iter().map(|r| r.app).collect::<Vec<_>>())
        .bind::<Array<Nullable<SqlUuid>>, _>(chunk.iter().map(|r| r.env).collect::<Vec<_>>())
        .bind::<Array<Date>, _>(chunk.iter().map(|r| r.day).collect::<Vec<_>>())
        .bind::<Array<BigInt>, _>(chunk.iter().map(|r| r.sessions).collect::<Vec<_>>())
        .bind::<Array<BigInt>, _>(chunk.iter().map(|r| r.crashed).collect::<Vec<_>>())
        .bind::<Array<Double>, _>(chunk.iter().map(|r| r.duration_ms_sum).collect::<Vec<_>>())
        .bind::<Array<Bytea>, _>(chunk.iter().map(|r| r.hist.to_bytes()).collect::<Vec<_>>())
        .bind::<Array<BigInt>, _>(chunk.iter().map(|r| r.fixed[0]).collect::<Vec<_>>())
        .bind::<Array<BigInt>, _>(chunk.iter().map(|r| r.fixed[1]).collect::<Vec<_>>())
        .bind::<Array<BigInt>, _>(chunk.iter().map(|r| r.fixed[2]).collect::<Vec<_>>())
        .bind::<Array<BigInt>, _>(chunk.iter().map(|r| r.fixed[3]).collect::<Vec<_>>())
        .bind::<Array<BigInt>, _>(chunk.iter().map(|r| r.fixed[4]).collect::<Vec<_>>())
        .execute(conn)
        .await?;
    }
    Ok(())
}
