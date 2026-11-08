//! Cross-tier read router. Splits a query window at the tier watermark, moves
//! any restored ranges back to the hot side, and runs the Postgres and
//! Parquet halves concurrently before gluing the additive per-day partials.
//!
//! The three public entry points were three near-identical 90-line copies. They
//! are one generic path now, because the restore work had to change the same
//! six things in each of them and a fourth tiered table would have meant a
//! fourth copy.

use chrono::{DateTime, Utc};
use uuid::Uuid;

use sauron_db::{conn, repo};
use sauron_tier::duck::DuckEngine;
use sauron_tier::{cold_partition_glob, merge_day_counts, plan_with_restores, DayCount, TimeRange};

use crate::AppState;

/// Which tiered table a cross-tier read is against. Carries the table name and
/// selects the hot-side query, which is the only thing that actually differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tiered {
    Errors,
    Events,
    Transactions,
}

impl Tiered {
    fn table(self) -> &'static str {
        match self {
            Tiered::Errors => "error_events",
            Tiered::Events => "analytics_events",
            Tiered::Transactions => "transactions",
        }
    }

    fn default_partition(self) -> &'static str {
        match self {
            Tiered::Errors => "error_events_default",
            Tiered::Events => "analytics_events_default",
            Tiered::Transactions => "transactions_default",
        }
    }
}

async fn hot_counts(
    kind: Tiered,
    c: &mut diesel_async::AsyncPgConnection,
    app_id: Uuid,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<Vec<repo::DayCountRow>, diesel::result::Error> {
    match kind {
        Tiered::Errors => repo::error_counts_by_day_hot(c, app_id, from, to).await,
        Tiered::Events => repo::event_counts_by_day_hot(c, app_id, from, to).await,
        Tiered::Transactions => repo::transaction_counts_by_day_hot(c, app_id, from, to).await,
    }
}

fn to_dc(rows: Vec<repo::DayCountRow>) -> Vec<DayCount> {
    rows.into_iter()
        .map(|r| DayCount {
            day: r.day,
            count: r.count,
        })
        .collect()
}

fn merge_all(parts: Vec<Vec<DayCount>>) -> Vec<DayCount> {
    parts.into_iter().fold(Vec::new(), merge_day_counts)
}

/// Where one tiered table's archive begins, and the ranges restored back out
/// of it into Postgres. `None` when the table has never been tiered.
///
/// Watermark and restored ranges are read together: they are the two halves of
/// one question ("which parts of this window live where?"), and reading them
/// in separate round trips from separate call sites is how readers drift
/// apart. Every reader in this module asks through here.
struct TierLayout {
    watermark: DateTime<Utc>,
    restored: Vec<TimeRange>,
}

async fn tier_layout(
    c: &mut diesel_async::AsyncPgConnection,
    table: &str,
) -> anyhow::Result<Option<TierLayout>> {
    let Some(watermark) = repo::get_watermark(c, table).await? else {
        return Ok(None);
    };
    let restored = repo::restored_ranges(c, table)
        .await?
        .into_iter()
        .map(|(start, end)| TimeRange { start, end })
        .collect();
    Ok(Some(TierLayout {
        watermark,
        restored,
    }))
}

/// Per-day counts for `[from, to)` across both tiers.
async fn counts_by_day(
    kind: Tiered,
    state: &AppState,
    app_id: Uuid,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> anyhow::Result<Vec<DayCount>> {
    let table = kind.table();

    let layout = {
        let mut c = conn(&state.pool).await?;
        tier_layout(&mut c, table).await?
    };

    // No watermark ⇒ nothing has ever been tiered, so everything is hot and
    // there is nothing for a restore to have moved.
    let Some(layout) = layout else {
        let mut c = conn(&state.pool).await?;
        return Ok(to_dc(hot_counts(kind, &mut c, app_id, from, to).await?));
    };
    let split = plan_with_restores(layout.watermark, from, to, &layout.restored);

    // PG branch: every hot sub-range (which now includes the restored islands)
    // plus, for each COLD sub-range, late arrivals sitting in the `_default`
    // partition — their explicit partition was tiered and dropped, so they are
    // NOT in Parquet. All on one pooled connection.
    let pool = state.pool.clone();
    let hot_ranges = split.hot.clone();
    let cold_ranges = split.cold.clone();
    let default_table = kind.default_partition();
    let pg = async move {
        let mut c = conn(&pool).await?;
        let mut hot_rows = Vec::new();
        for r in &hot_ranges {
            hot_rows.extend(hot_counts(kind, &mut c, app_id, r.start, r.end).await?);
        }
        let mut cold_default_rows = Vec::new();
        for r in &cold_ranges {
            cold_default_rows.extend(
                repo::default_partition_counts_by_day(
                    &mut c,
                    default_table,
                    app_id,
                    r.start,
                    r.end,
                )
                .await?,
            );
        }
        Ok::<_, anyhow::Error>((hot_rows, cold_default_rows))
    };

    // COLD Parquet branch: DuckDB is blocking → spawn_blocking, runs concurrently.
    let cold_path = state.cfg.tier_cold_path.clone();
    let cold_ranges2 = split.cold.clone();
    let cold_parquet = async move {
        if cold_ranges2.is_empty() {
            return Ok(Vec::new());
        }
        let glob = cold_partition_glob(&cold_path, table, app_id);
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<DayCount>> {
            let eng = DuckEngine::open()?;
            let mut out = Vec::new();
            for r in &cold_ranges2 {
                out = merge_day_counts(out, eng.counts_by_day(&glob, app_id, r.start, r.end)?);
            }
            Ok(out)
        })
        .await?
    };

    let (pg_res, parquet_res) = tokio::join!(pg, cold_parquet);
    let (hot_rows, cold_default_rows) = pg_res?;
    let parquet_rows = parquet_res?;

    // The three sets are disjoint by construction: a row is in exactly one of
    // Parquet (cold sub-ranges only), `_default` (cold sub-ranges only), or an
    // explicit/hot partition (hot sub-ranges only). Restored rows are counted
    // once, on the hot side, because `plan_with_restores` removed their range
    // from the cold side entirely.
    Ok(merge_all(vec![
        to_dc(hot_rows),
        parquet_rows,
        to_dc(cold_default_rows),
    ]))
}

/// Error counts per day for `[from, to)`, spanning hot + cold as needed.
pub async fn error_counts_by_day(
    state: &AppState,
    app_id: Uuid,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> anyhow::Result<Vec<DayCount>> {
    counts_by_day(Tiered::Errors, state, app_id, from, to).await
}

/// Analytics-event counts per day for `[from, to)`, spanning hot + cold as needed.
pub async fn event_counts_by_day(
    state: &AppState,
    app_id: Uuid,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> anyhow::Result<Vec<DayCount>> {
    counts_by_day(Tiered::Events, state, app_id, from, to).await
}

/// Transaction counts (throughput) per day for `[from, to)`, spanning hot +
/// cold as needed. ADDITIVE metric only — safe to sum across tiers. Transaction
/// PERCENTILES (p50/p95 of duration_ms) are HOLISTIC and are NOT merged across
/// tiers; those endpoints stay hot-only (Postgres).
pub async fn transaction_counts_by_day(
    state: &AppState,
    app_id: Uuid,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> anyhow::Result<Vec<DayCount>> {
    counts_by_day(Tiered::Transactions, state, app_id, from, to).await
}

// ===========================================================================
// Active Users — the first HOLISTIC cross-tier metric here
// ===========================================================================
//
// Everything above is additive: a row count from Postgres plus a row count from
// Parquet is the right answer, which is what lets `merge_day_counts` glue them.
//
// `COUNT(DISTINCT distinct_id)` is NOT. Two independent totals cannot be combined
// — a person active either side of the watermark is counted once in each, and
// nothing in the two numbers reveals the overlap. **Do not route this through
// `merge_day_counts`, and do not add a total-over-the-range variant that sums
// these per-day values.** (`transaction_counts_by_day`'s doc records the same
// distinction for percentiles, which stay hot-only for exactly this reason.)
//
// What makes a per-DAY distinct count safe is that a day belongs entirely to one
// tier — true because the watermark only advances to a partition END, and
// partitions are day-granular under the default `TIER_GRANULARITY=day`. Set it to
// `week` or `month` and the watermark stops being a day boundary, so one day at
// the seam gets rows from both tiers. That day is REPORTED rather than silently
// halved or double-counted.

/// A day whose count could not be computed exactly, and why.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct PartialDay {
    pub day: chrono::NaiveDate,
    pub reason: &'static str,
}

/// Distinct people per UTC day for `[from, to)`, spanning hot + cold.
///
/// Returns the series plus any day excluded from it. A day appearing in BOTH
/// tiers is excluded: its true distinct count is unknowable from two partial
/// counts, and a missing point an operator can see beats a wrong point they
/// cannot.
pub async fn active_users_by_day(
    state: &AppState,
    scope: sauron_db::scope::ReadScope,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> anyhow::Result<(Vec<DayCount>, Vec<PartialDay>)> {
    let app_id = scope.app_id;
    // Rollup gate: the migration-71 rollups never tier out, so for a ready
    // app one small read covers ANY window and the whole hot/cold seam
    // machinery below is unnecessary — including partial-day exclusions.
    {
        let mut c = conn(&state.pool).await?;
        if sauron_db::rollups::is_ready(&mut c, app_id).await? {
            let rows =
                sauron_db::rollups::read::active_users_by_day(&mut c, &scope, from, to).await?;
            return Ok((to_dc(rows), Vec::new()));
        }
    }
    let layout = {
        let mut c = conn(&state.pool).await?;
        tier_layout(&mut c, "analytics_events").await?
    };

    // Nothing tiered ⇒ entirely hot, and no seam is possible.
    let Some(layout) = layout else {
        let mut c = conn(&state.pool).await?;
        let rows = repo::active_users_by_day_hot(&mut c, scope, from, to).await?;
        return Ok((to_dc(rows), Vec::new()));
    };
    let split = plan_with_restores(layout.watermark, from, to, &layout.restored);

    let mut hot: Vec<DayCount> = Vec::new();
    {
        let mut c = conn(&state.pool).await?;
        for r in &split.hot {
            hot.extend(to_dc(
                repo::active_users_by_day_hot(&mut c, scope.clone(), r.start, r.end).await?,
            ));
        }
    }

    let cold_path = state.cfg.tier_cold_path.clone();
    let cold_ranges = split.cold.clone();
    let cold: Vec<DayCount> = if cold_ranges.is_empty() {
        Vec::new()
    } else {
        // The bounded cold overlay: guest ids Parquet still holds because cold
        // is immutable and the hot rewrite could never reach them. Fetched over
        // the FULL query window (a safe superset of the cold sub-ranges below),
        // not per sub-range — see `sauron_db::identity_merge::cold_alias_map`'s
        // doc comment for why the map is already bounded and an extra row here
        // just costs a slightly bigger overlay, never a wrong answer.
        let aliases: Vec<(String, String)> = {
            let mut c = conn(&state.pool).await?;
            sauron_db::identity_merge::cold_alias_map(&mut c, app_id, from, to)
                .await?
                .into_iter()
                .map(|e| (e.alias, e.person))
                .collect()
        };
        let glob = cold_partition_glob(&cold_path, "analytics_events", app_id);
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<DayCount>> {
            let eng = DuckEngine::open()?;
            let mut out = Vec::new();
            for r in &cold_ranges {
                out.extend(eng.distinct_users_by_day(&glob, app_id, r.start, r.end, &aliases)?);
            }
            Ok(out)
        })
        .await??
    };

    // Concatenate, not merge. A day present on both sides is the seam case: drop
    // it from the series and name it.
    use std::collections::{BTreeMap, BTreeSet};
    let hot_days: BTreeSet<chrono::NaiveDate> = hot.iter().map(|d| d.day).collect();
    let mut partial: Vec<PartialDay> = Vec::new();
    let mut series: BTreeMap<chrono::NaiveDate, i64> = BTreeMap::new();
    for d in hot {
        series.insert(d.day, d.count);
    }
    for d in cold {
        if hot_days.contains(&d.day) {
            // Only reachable when the watermark is not on a day boundary, i.e.
            // TIER_GRANULARITY is week/month, or a restore covered part of a day.
            series.remove(&d.day);
            partial.push(PartialDay {
                day: d.day,
                reason: "the hot/cold boundary falls inside this day, so a distinct \
                         count cannot be computed exactly",
            });
        } else {
            series.insert(d.day, d.count);
        }
    }
    partial.dedup_by_key(|p| p.day);
    Ok((
        series
            .into_iter()
            .map(|(day, count)| DayCount { day, count })
            .collect(),
        partial,
    ))
}

// ===========================================================================
// Archived ROWS — one person's or one session's history across both tiers
// ===========================================================================
//
// Everything above aggregates. A person's profile and a session's timeline
// list ROWS for one key with no time window, and both used to read Postgres
// alone — so once the tier worker moved a day out (`tier.hot_days`), that day
// vanished from both pages while every rollup-backed count still included it.
//
// The merge is by row, not by count, and it is deliberately unafraid of
// overlap. Postgres can hold rows below the watermark for three reasons: a
// late arrival in `_default` (not in Parquet), a restored range (in BOTH), and
// an exported partition still waiting out its drop grace (in BOTH). Rather
// than reproduce each rule, the archive is read for the cold sub-ranges minus
// restores, and the two sides are unioned by primary key with Postgres
// winning — a row in both is shown once, a row in either is shown.
//
// Three properties every caller relies on:
//
// * **No second pool checkout.** The Postgres lookups run on the caller's
//   connection. A handler that held one connection and waited for another
//   would, at pool-size concurrency, wait on itself.
// * **Postgres first, archive only when needed.** The cheap watermark lookup
//   decides; restores, the jsonb column list and DuckDB are touched only for a
//   table that actually has to be read.
// * **The archive never takes a page down.** These pages rendered from
//   Postgres alone before the archive existed for them. A read that fails (a
//   truncated file not yet quarantined, an unreadable directory, a row that
//   will not decode) is logged, the page is served from Postgres, and
//   `archive_incomplete` says so — instead of a 500.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use sauron_db::models::{AnalyticsEvent, ErrorEvent, Session, Transaction};
use sauron_db::scope::{EnvFilter, ReadScope};
use sauron_tier::{RowEnv, RowKey, RowOrder, RowQuery};

/// A row of a tiered table, as the merge needs to see it.
pub trait TieredRow: serde::de::DeserializeOwned + Send + 'static {
    const TABLE: &'static str;
    fn row_id(&self) -> Uuid;
    fn row_at(&self) -> DateTime<Utc>;
}

impl TieredRow for AnalyticsEvent {
    const TABLE: &'static str = "analytics_events";
    fn row_id(&self) -> Uuid {
        self.id
    }
    fn row_at(&self) -> DateTime<Utc> {
        self.occurred_at
    }
}

impl TieredRow for ErrorEvent {
    const TABLE: &'static str = "error_events";
    fn row_id(&self) -> Uuid {
        self.id
    }
    fn row_at(&self) -> DateTime<Utc> {
        self.occurred_at
    }
}

impl TieredRow for Transaction {
    const TABLE: &'static str = "transactions";
    fn row_id(&self) -> Uuid {
        self.id
    }
    fn row_at(&self) -> DateTime<Utc> {
        self.occurred_at
    }
}

/// Whether Postgres's answer already IS the answer, so the archive need not be
/// opened. Only provable newest-first: every archived row is older than the
/// watermark, so a full page whose oldest row is at or past it cannot be
/// displaced. Oldest-first, archived rows sort FIRST, so the caller decides by
/// whether its window starts before the watermark at all.
fn hot_is_enough<T: TieredRow>(
    hot: &[T],
    order: RowOrder,
    limit: usize,
    watermark: DateTime<Utc>,
) -> bool {
    match order {
        RowOrder::NewestFirst => {
            hot.len() >= limit && hot.last().is_some_and(|r| r.row_at() >= watermark)
        }
        RowOrder::OldestFirst => false,
    }
}

/// `hot` and `cold` unioned by primary key (Postgres wins a tie — see the
/// section comment), sorted in `order` with the id as tiebreak, cut to `limit`.
fn merge_rows<T: TieredRow>(hot: Vec<T>, cold: Vec<T>, order: RowOrder, limit: usize) -> Vec<T> {
    let seen: std::collections::HashSet<Uuid> = hot.iter().map(TieredRow::row_id).collect();
    let mut all = hot;
    all.extend(cold.into_iter().filter(|r| !seen.contains(&r.row_id())));
    all.sort_by(|a, b| {
        let k = (a.row_at(), a.row_id()).cmp(&(b.row_at(), b.row_id()));
        match order {
            RowOrder::OldestFirst => k,
            RowOrder::NewestFirst => k.reverse(),
        }
    });
    all.truncate(limit);
    all
}

fn row_env(env: &EnvFilter) -> RowEnv {
    match env {
        EnvFilter::All => RowEnv::All,
        EnvFilter::One(id) => RowEnv::One(*id),
        EnvFilter::Subset(ids) => RowEnv::Subset(ids.clone()),
        EnvFilter::Unattributed => RowEnv::Unattributed,
    }
}

/// `table`'s `jsonb` columns (see `RowQuery::json_columns`), looked up once per
/// process: the schema only changes with a migration, and a migration means a
/// restart. `information_schema` is a privilege-checked catalog view — not
/// something to scan on every page view.
async fn json_columns(
    c: &mut diesel_async::AsyncPgConnection,
    table: &'static str,
) -> anyhow::Result<Vec<String>> {
    static CACHE: OnceLock<Mutex<HashMap<&'static str, Vec<String>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(cols) = cache.lock().expect("json column cache").get(table) {
        return Ok(cols.clone());
    }
    let cols = repo::jsonb_columns(c, table).await?;
    cache
        .lock()
        .expect("json column cache")
        .insert(table, cols.clone());
    Ok(cols)
}

/// One table's archived read, or `None` when Postgres's page already is the
/// answer or nothing in `window` was ever archived.
#[allow(clippy::too_many_arguments)]
async fn plan_read<T: TieredRow>(
    c: &mut diesel_async::AsyncPgConnection,
    hot: &[T],
    key: &RowKey,
    env: &RowEnv,
    window: (DateTime<Utc>, DateTime<Utc>),
    order: RowOrder,
    limit: usize,
) -> anyhow::Result<Option<RowQuery>> {
    let Some(layout) = tier_layout(c, T::TABLE).await? else {
        return Ok(None);
    };
    if window.0 >= layout.watermark || hot_is_enough(hot, order, limit, layout.watermark) {
        return Ok(None);
    }
    let split = plan_with_restores(layout.watermark, window.0, window.1, &layout.restored);
    if split.cold.is_empty() {
        return Ok(None);
    }
    Ok(Some(RowQuery {
        key: key.clone(),
        env: env.clone(),
        ranges: split.cold,
        order,
        limit,
        json_columns: json_columns(c, T::TABLE).await?,
    }))
}

/// Read every planned table's archived rows through ONE DuckDB engine, in one
/// blocking task, one table after another: a page view costs one engine (and
/// one memory ceiling), not one per table. Each table's outcome is separate,
/// so one unreadable table does not discard another's rows.
async fn read_archives(
    state: &AppState,
    app_id: Uuid,
    plans: Vec<(&'static str, RowQuery)>,
) -> Vec<(&'static str, anyhow::Result<Vec<String>>)> {
    if plans.is_empty() {
        return Vec::new();
    }
    let base = state.cfg.tier_cold_path.clone();
    let tables: Vec<&'static str> = plans.iter().map(|(t, _)| *t).collect();
    let joined = tokio::task::spawn_blocking(move || {
        let eng = match DuckEngine::open() {
            Ok(eng) => eng,
            Err(e) => {
                let msg = format!("{e:#}");
                return plans
                    .into_iter()
                    .map(|(t, _)| (t, Err(anyhow::anyhow!("opening DuckDB: {msg}"))))
                    .collect();
            }
        };
        plans
            .into_iter()
            .map(|(t, q)| (t, eng.archived_rows(&base, t, app_id, &q)))
            .collect::<Vec<_>>()
    })
    .await;
    joined.unwrap_or_else(|e| {
        let msg = e.to_string();
        tables
            .into_iter()
            .map(|t| (t, Err(anyhow::anyhow!("archive read task failed: {msg}"))))
            .collect()
    })
}

/// `hot` completed with `T`'s archived rows from `reads`, if any were planned.
/// A failed read or an undecodable row is logged and leaves `hot` as the
/// answer, raising `incomplete` — see the section comment.
fn finish<T: TieredRow>(
    hot: Vec<T>,
    reads: &mut Vec<(&'static str, anyhow::Result<Vec<String>>)>,
    order: RowOrder,
    limit: usize,
    incomplete: &mut bool,
) -> Vec<T> {
    let Some(i) = reads.iter().position(|(t, _)| *t == T::TABLE) else {
        return hot;
    };
    let decoded = reads.swap_remove(i).1.and_then(|raw| {
        raw.iter()
            .map(|r| {
                serde_json::from_str::<T>(r).map_err(|e| {
                    anyhow::anyhow!("an archived {} row did not decode: {e}", T::TABLE)
                })
            })
            .collect::<anyhow::Result<Vec<T>>>()
    });
    match decoded {
        Ok(cold) => merge_rows(hot, cold, order, limit),
        Err(e) => {
            tracing::warn!(
                table = T::TABLE,
                error = %format_args!("{e:#}"),
                "archived rows unavailable; serving Postgres rows only"
            );
            *incomplete = true;
            hot
        }
    }
}

/// A day of slack either side of a span. It keeps a row at the very edge of a
/// month directory from depending on independently computed timestamps
/// agreeing to the microsecond; it can only widen which months are opened.
const SPAN_SLACK: chrono::Duration = chrono::Duration::days(1);

/// The occurred-at window that can hold any row of `ids`, from `event_users`:
/// the one person table that is maintained from the very first signal and
/// never tiered. Its first/last seen are INGEST times, and the ingest edge pins
/// a device time more than `MAX_CLOCK_LAG` in the past or `MAX_CLOCK_SKEW` in
/// the future to the arrival time, so widening by exactly those bounds every
/// `occurred_at` the person can have.
///
/// Deliberately not `event_user_environments` (event time, so tighter): that
/// rollup only knows what was still in Postgres when its backfill ran, so any
/// history tiered out before then lies outside its span and would be silently
/// skipped. `None` — no `event_users` row at all — reads the whole archive.
async fn person_window(
    c: &mut diesel_async::AsyncPgConnection,
    app_id: Uuid,
    ids: &[String],
) -> anyhow::Result<(DateTime<Utc>, DateTime<Utc>)> {
    let span = repo::person_ingest_span(c, app_id, ids).await?;
    Ok(span.map_or(
        (DateTime::<Utc>::UNIX_EPOCH, Utc::now() + SPAN_SLACK),
        |(first, last)| {
            (
                first - sauron_core::envelope::MAX_CLOCK_LAG - SPAN_SLACK,
                last + sauron_core::envelope::MAX_CLOCK_SKEW + SPAN_SLACK,
            )
        },
    ))
}

/// A person's page across both tiers.
pub struct PersonRows {
    pub events: Vec<AnalyticsEvent>,
    pub errors: Vec<ErrorEvent>,
    /// Some archived rows could not be read; the lists hold what Postgres had.
    pub archive_incomplete: bool,
}

/// A person's newest `limit` events and errors across both tiers.
///
/// `events` / `errors` are Postgres's newest `limit` each for this person in
/// `scope`, read on `c`, which this keeps using (see the section comment).
/// Archived rows are read for every id that is this person in cold storage
/// ([`sauron_db::identity_merge::archived_ids_for_person`]) and shown as the
/// person — what the merge's hot rewrite already did to their Postgres rows.
pub async fn person_rows(
    state: &AppState,
    c: &mut diesel_async::AsyncPgConnection,
    scope: &ReadScope,
    person: &str,
    events: Vec<AnalyticsEvent>,
    errors: Vec<ErrorEvent>,
    limit: usize,
) -> anyhow::Result<PersonRows> {
    let app_id = scope.app_id;
    let order = RowOrder::NewestFirst;
    // Settle "Postgres is enough" on the watermarks alone before anything
    // else: most profile views are of people whose newest page is all hot.
    let ev_need = repo::get_watermark(c, AnalyticsEvent::TABLE)
        .await?
        .is_some_and(|w| !hot_is_enough(&events, order, limit, w));
    let er_need = repo::get_watermark(c, ErrorEvent::TABLE)
        .await?
        .is_some_and(|w| !hot_is_enough(&errors, order, limit, w));
    let ids = if ev_need || er_need {
        sauron_db::identity_merge::archived_ids_for_person(c, app_id, person).await?
    } else {
        Vec::new()
    };
    if ids.is_empty() {
        return Ok(PersonRows {
            events,
            errors,
            archive_incomplete: false,
        });
    }
    let window = person_window(c, app_id, &ids).await?;
    let key = RowKey::DistinctIds(ids);
    let env = row_env(&scope.env);
    let mut plans = Vec::new();
    if ev_need {
        if let Some(q) = plan_read(c, &events, &key, &env, window, order, limit).await? {
            plans.push((AnalyticsEvent::TABLE, q));
        }
    }
    if er_need {
        if let Some(q) = plan_read(c, &errors, &key, &env, window, order, limit).await? {
            plans.push((ErrorEvent::TABLE, q));
        }
    }
    let mut reads = read_archives(state, app_id, plans).await;
    let mut incomplete = false;
    let mut events = finish(events, &mut reads, order, limit, &mut incomplete);
    let mut errors = finish(errors, &mut reads, order, limit, &mut incomplete);
    for e in &mut events {
        if e.distinct_id != person {
            e.distinct_id = person.to_string();
        }
    }
    for e in &mut errors {
        if e.distinct_id.as_deref().is_some_and(|d| d != person) {
            e.distinct_id = Some(person.to_string());
        }
    }
    Ok(PersonRows {
        events,
        errors,
        archive_incomplete: incomplete,
    })
}

/// An hour either side of a session's recorded span, for the same reason as
/// [`SPAN_SLACK`]; a session's rows all carry its id, so the slack can only
/// widen which month directories are opened, never which rows are returned.
const SESSION_SLACK: chrono::Duration = chrono::Duration::hours(1);

/// A session's page across both tiers.
pub struct SessionRows {
    pub events: Vec<AnalyticsEvent>,
    pub errors: Vec<ErrorEvent>,
    pub txns: Vec<Transaction>,
    /// Some archived rows could not be read; the lists hold what Postgres had.
    pub archive_incomplete: bool,
}

/// A session's oldest `limit` events, errors and transactions across both
/// tiers. Each list is Postgres's oldest `limit` for this session, read on `c`.
///
/// Raw sessions outlive their events by design (`sessions.retention_days`
/// against `tier.hot_days`), so without this a session a few days old opened
/// on an empty timeline. A guest id on any row whose merge has completed is
/// shown as the person, as on the person page.
#[allow(clippy::too_many_arguments)]
pub async fn session_rows(
    state: &AppState,
    c: &mut diesel_async::AsyncPgConnection,
    scope: &ReadScope,
    session: &Session,
    events: Vec<AnalyticsEvent>,
    errors: Vec<ErrorEvent>,
    txns: Vec<Transaction>,
    limit: usize,
) -> anyhow::Result<SessionRows> {
    let app_id = scope.app_id;
    let order = RowOrder::OldestFirst;
    let window = (
        session.started_at - SESSION_SLACK,
        session.last_event_at + SESSION_SLACK,
    );
    let key = RowKey::Session(session.session_id.clone());
    let env = row_env(&scope.env);
    let mut plans = Vec::new();
    if let Some(q) = plan_read(c, &events, &key, &env, window, order, limit).await? {
        plans.push((AnalyticsEvent::TABLE, q));
    }
    if let Some(q) = plan_read(c, &errors, &key, &env, window, order, limit).await? {
        plans.push((ErrorEvent::TABLE, q));
    }
    if let Some(q) = plan_read(c, &txns, &key, &env, window, order, limit).await? {
        plans.push((Transaction::TABLE, q));
    }
    if plans.is_empty() {
        return Ok(SessionRows {
            events,
            errors,
            txns,
            archive_incomplete: false,
        });
    }
    let mut reads = read_archives(state, app_id, plans).await;
    let mut incomplete = false;
    let mut events = finish(events, &mut reads, order, limit, &mut incomplete);
    let mut errors = finish(errors, &mut reads, order, limit, &mut incomplete);
    let mut txns = finish(txns, &mut reads, order, limit, &mut incomplete);

    let mut seen: Vec<String> = events.iter().map(|e| e.distinct_id.clone()).collect();
    seen.extend(errors.iter().filter_map(|e| e.distinct_id.clone()));
    seen.extend(txns.iter().filter_map(|t| t.distinct_id.clone()));
    seen.sort();
    seen.dedup();
    let owners = sauron_db::identity_merge::archived_alias_owners(c, app_id, &seen).await?;
    if !owners.is_empty() {
        let owner = |d: &str| owners.get(d).cloned();
        for e in &mut events {
            if let Some(p) = owner(&e.distinct_id) {
                e.distinct_id = p;
            }
        }
        for e in &mut errors {
            if let Some(p) = e.distinct_id.as_deref().and_then(owner) {
                e.distinct_id = Some(p);
            }
        }
        for t in &mut txns {
            if let Some(p) = t.distinct_id.as_deref().and_then(owner) {
                t.distinct_id = Some(p);
            }
        }
    }
    Ok(SessionRows {
        events,
        errors,
        txns,
        archive_incomplete: incomplete,
    })
}

#[cfg(test)]
mod archived_row_tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq, serde::Deserialize)]
    struct Row {
        id: Uuid,
        at: DateTime<Utc>,
        tag: String,
    }

    impl TieredRow for Row {
        const TABLE: &'static str = "test_rows";
        fn row_id(&self) -> Uuid {
            self.id
        }
        fn row_at(&self) -> DateTime<Utc> {
            self.at
        }
    }

    fn at(h: i64) -> DateTime<Utc> {
        DateTime::<Utc>::UNIX_EPOCH + chrono::Duration::hours(h)
    }

    fn row(h: i64, tag: &str) -> Row {
        Row {
            id: Uuid::new_v4(),
            at: at(h),
            tag: tag.to_string(),
        }
    }

    fn tags(rows: &[Row]) -> Vec<&str> {
        rows.iter().map(|r| r.tag.as_str()).collect()
    }

    #[test]
    fn a_row_in_both_tiers_is_shown_once_and_postgres_wins() {
        let shared = row(5, "hot copy");
        let mut cold_copy = shared.clone();
        cold_copy.tag = "cold copy".to_string();
        let merged = merge_rows(
            vec![row(9, "hot"), shared],
            vec![cold_copy, row(1, "cold")],
            RowOrder::NewestFirst,
            10,
        );
        assert_eq!(tags(&merged), vec!["hot", "hot copy", "cold"]);
    }

    #[test]
    fn merges_in_order_and_cuts_to_the_limit() {
        let hot = vec![row(2, "h2"), row(6, "h6")];
        let cold = vec![row(4, "c4"), row(1, "c1"), row(3, "c3")];
        assert_eq!(
            tags(&merge_rows(
                hot.clone(),
                cold.clone(),
                RowOrder::OldestFirst,
                3
            )),
            vec!["c1", "h2", "c3"]
        );
        assert_eq!(
            tags(&merge_rows(hot, cold, RowOrder::NewestFirst, 3)),
            vec!["h6", "c4", "c3"]
        );
    }

    #[test]
    fn ties_on_time_are_broken_by_id_so_paging_is_stable() {
        let (a, b) = (row(3, "a"), row(3, "b"));
        let lo = if a.id < b.id { "a" } else { "b" };
        let merged = merge_rows(vec![a], vec![b], RowOrder::OldestFirst, 10);
        assert_eq!(merged[0].tag, lo);
    }

    #[test]
    fn postgres_alone_answers_only_a_full_newest_first_page_above_the_watermark() {
        let wm = at(10);
        let full_above = vec![row(12, "a"), row(11, "b")];
        assert!(hot_is_enough(&full_above, RowOrder::NewestFirst, 2, wm));
        // One short: an archived row could be the missing one.
        assert!(!hot_is_enough(&full_above, RowOrder::NewestFirst, 3, wm));
        // Full, but its oldest row is below the watermark (a late arrival or a
        // partition in its drop grace): archived rows could sort above it.
        let full_straddling = vec![row(12, "a"), row(9, "b")];
        assert!(!hot_is_enough(
            &full_straddling,
            RowOrder::NewestFirst,
            2,
            wm
        ));
        // Oldest-first, archived rows sort FIRST: never provable from Postgres.
        assert!(!hot_is_enough(&full_above, RowOrder::OldestFirst, 2, wm));
        // Empty and a zero limit.
        assert!(!hot_is_enough::<Row>(&[], RowOrder::NewestFirst, 1, wm));
    }

    #[test]
    fn every_environment_scope_maps_across() {
        let id = Uuid::new_v4();
        assert_eq!(row_env(&EnvFilter::All), RowEnv::All);
        assert_eq!(row_env(&EnvFilter::One(id)), RowEnv::One(id));
        assert_eq!(
            row_env(&EnvFilter::Subset(vec![id])),
            RowEnv::Subset(vec![id])
        );
        assert_eq!(row_env(&EnvFilter::Unattributed), RowEnv::Unattributed);
    }
}
