//! Cohort retention, lifecycle and churn.
//!
//! Its own module rather than more `analytics.rs`, which is already 1,768
//! lines. The authorization shape is the same as its neighbours there:
//! `authorized_read_scope` on `EVENT_READ`, with `environment_id` read from
//! `RawQuery` — NOT as a `Query<T>` field, per `routes::scope`'s module docs.
//!
//! # The grid and lifecycle are served stale-while-revalidate
//!
//! Both walk every person-day in the window — measured at 0.9 s (grid) and
//! 1.8 s (lifecycle) against 51k persons — so their assembled responses are
//! cached in Redis on the `active_users.rs` pattern: under an hour old,
//! served as-is; between one and three, served as-is while ONE background
//! refresh (Redis `SET NX` single-flight) recomputes; past three, gone, and
//! the next request pays the compute on-path. `computed_at` on the response
//! discloses the age. Three deliberate differences from the template:
//!
//! * The cache key hashes the RESOLVED environment filter, never the request
//!   (same as the template — any deviation is review-Critical), and also the
//!   UTC DAY: this endpoint derives its own window from "today", so rotating
//!   the key at midnight removes the stale-window class outright.
//! * `ready:false` responses are never cached. They are two point lookups,
//!   and caching one would keep answering "not backfilled" for up to an hour
//!   after the operator's backfill finishes.
//! * No admission semaphore. The compute is ~2 s, not active-users' ~25 s;
//!   the per-user rate limit and the 30 s `TimeoutLayer` remain the bounds,
//!   and the surviving refresh lock doubles as a per-key failure cooldown.
//!
//! Churn is NOT cached: 39 ms at the same scale, and it pages by cursor.

use std::time::Duration as StdDuration;

use axum::extract::{Path, Query, RawQuery, State};
use axum::Json;
use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use sauron_auth::{hash_token, perm, AuthUser};
use sauron_db::retention::{self, ErrorSplit, Granularity};
use sauron_db::rollups::person_days;
use sauron_db::scope::{EnvFilter, ReadScope};

use super::db;
use crate::error::ApiError;
use crate::openapi::ErrorResponse;
use crate::AppState;

/// Cap on `cohorts x periods` — the thing actually being handed out.
///
/// Bounding the two dimensions independently does NOT bound their product, and
/// the product is what the query walks. `active_users.rs` records the same
/// lesson after 20 apps x 92 days turned into 1,840 partition-day scans behind
/// two individually reasonable-looking limits.
const MAX_RETENTION_CELLS: i64 = 400;

/// Per-dimension ceilings, applied before the product check so a caller asking
/// for something absurd gets a clamp rather than an overflow.
const MAX_DIM: i64 = 52;

/// How long an assembled grid or lifecycle is served without any recompute.
const RETENTION_FRESH_FOR_SECS: i64 = 3600;
/// How long an entry stays servable at all; between fresh and this, a hit is
/// served as-is and a background refresh is kicked.
const RETENTION_CACHE_TTL_SECS: u64 = 3 * 3600;
/// Single-flight TTL for the refresh lock — must outlive one compute, must
/// expire at all (a crashed refresher would otherwise wedge the key forever).
const RETENTION_REFRESH_LOCK_SECS: u64 = 120;
/// Budget for one Redis command. Do NOT drop this: `sauron-redis` sets no
/// response timeout, and `routes/auth.rs` measured 9-19 s per command against
/// a dead Redis — twice per request here would stall the API, not degrade it.
const CACHE_OP_TIMEOUT: StdDuration = StdDuration::from_millis(500);

fn parse_granularity(s: &str) -> Result<Granularity, ApiError> {
    match s {
        "day" => Ok(Granularity::Day),
        "week" => Ok(Granularity::Week),
        other => Err(ApiError::BadRequest(format!(
            "granularity must be 'day' or 'week', got '{other}'"
        ))),
    }
}

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct GridQuery {
    #[serde(default = "default_granularity")]
    pub granularity: String,
    #[serde(default = "default_dim")]
    pub cohorts: i64,
    #[serde(default = "default_dim")]
    pub periods: i64,
    #[serde(default)]
    pub split: Option<String>,
    // `environment_id` is deliberately NOT a field here — see the module docs.
}

fn default_granularity() -> String {
    "day".into()
}
fn default_dim() -> i64 {
    12
}

#[derive(Serialize, Deserialize, utoipa::ToSchema)]
pub struct Cohort {
    pub start: NaiveDate,
    pub size: i64,
    /// `None` serializes as JSON `null` and means NOT KNOWABLE YET — a
    /// different fact from zero. The type is what stops a client rendering an
    /// unelapsed period as 0% retention, which is the most common bug in this
    /// entire chart category.
    pub periods: Vec<Option<i64>>,
}

#[derive(Serialize, Deserialize, utoipa::ToSchema)]
pub struct GridOut {
    pub granularity: String,
    pub as_of: Option<DateTime<Utc>>,
    /// False when this app's pre-epoch history has not been backfilled. The
    /// dashboard renders the backfill command instead of an empty grid.
    pub ready: bool,
    pub cohorts: Vec<Cohort>,
    /// Present only when `split=errors`: the same cohorts restricted to people
    /// who saw NO error in period 0, for side-by-side comparison.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clean: Option<Vec<Cohort>>,
    /// When this response was computed — the staleness disclosure the
    /// serve-stale window depends on. `None` only on `ready:false` responses,
    /// which are never cached.
    #[serde(default)]
    pub computed_at: Option<DateTime<Utc>>,
}

/// Fold flat `(cohort, period, users)` rows into dense per-cohort vectors, then
/// blank out the cells that cannot be known yet.
///
/// A period is only knowable once it has fully ELAPSED: period `n` of a cohort
/// starting at `start` ends at `start + (n+1)*step`, so anything past `as_of`
/// becomes `None`.
fn densify(
    rows: &[retention::CohortRow],
    periods: usize,
    step: i64,
    as_of_day: NaiveDate,
    floor: Option<NaiveDate>,
) -> Vec<Cohort> {
    let mut out: Vec<Cohort> = Vec::new();
    for row in rows {
        if out.last().map(|c| c.start) != Some(row.cohort) {
            out.push(Cohort {
                start: row.cohort,
                size: row.size,
                periods: vec![Some(0); periods],
            });
        }
        if let Some(slot) = out
            .last_mut()
            .expect("pushed above")
            .periods
            .get_mut(row.period as usize)
        {
            *slot = Some(row.users);
        }
    }
    for c in &mut out {
        for (n, slot) in c.periods.iter_mut().enumerate() {
            let ends = c.start + Duration::days((n as i64 + 1) * step);
            if ends > as_of_day {
                *slot = None;
                continue;
            }
            // ...and blank the other end too. Cohorts come from
            // `event_user_environments.first_seen`, which is never pruned and
            // reaches back as far as ingest ever ran; activity comes from
            // `person_days`, which begins when the fold or the backfill began.
            // A period that ends at or before that floor has NO activity data
            // behind it, so reporting the 0 rows found as "0% returned" states
            // a fact the data cannot support — and does it exactly where the
            // grid looks worst. `None` says "not knowable", which is true.
            if floor.is_some_and(|f| ends <= f) {
                *slot = None;
            }
        }
    }
    out
}

/// The environment dimension of a cache key, canonicalised.
///
/// `One(x)` and `Subset([x])` collapse to the same form on purpose — they
/// render different SQL (`= $n` vs `= ANY($n)`) but select the same rows, so
/// splitting them would only halve the hit rate. Ids are sorted so grant-row
/// order cannot mint distinct keys for the same reach.
#[derive(Serialize)]
struct EnvCanon {
    tag: &'static str,
    ids: Vec<Uuid>,
}

fn canonical_env(env: &EnvFilter) -> EnvCanon {
    match env {
        EnvFilter::All => EnvCanon {
            tag: "all",
            ids: vec![],
        },
        EnvFilter::Unattributed => EnvCanon {
            tag: "unattributed",
            ids: vec![],
        },
        EnvFilter::One(id) => EnvCanon {
            tag: "envs",
            ids: vec![*id],
        },
        EnvFilter::Subset(ids) => {
            let mut ids = ids.clone();
            ids.sort_unstable();
            ids.dedup();
            EnvCanon { tag: "envs", ids }
        }
    }
}

/// One key per (endpoint, app, RESOLVED env reach, shape, UTC day).
///
/// The resolved filter — not the request — is what keeps an app-wide caller
/// and an env-scoped caller from ever sharing an entry; their requests are
/// byte-identical. Treat any deviation from that in review as a Critical.
/// `day` rotates the key at UTC midnight because the window itself is derived
/// from "today" at compute time.
#[allow(clippy::too_many_arguments)]
fn retention_cache_key(
    kind: &'static str,
    app_id: Uuid,
    env: &EnvFilter,
    granularity: &str,
    cohorts: i64,
    periods: i64,
    split: bool,
    day: NaiveDate,
) -> Result<String, ApiError> {
    #[derive(Serialize)]
    struct Fingerprint<'a> {
        kind: &'static str,
        app_id: Uuid,
        env: EnvCanon,
        granularity: &'a str,
        cohorts: i64,
        periods: i64,
        split: bool,
        day: NaiveDate,
    }
    let json = serde_json::to_string(&Fingerprint {
        kind,
        app_id,
        env: canonical_env(env),
        granularity,
        cohorts,
        periods,
        split,
        day,
    })
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(format!("sauron:retention:{}", hash_token(&json)))
}

async fn cache_get<T: serde::de::DeserializeOwned>(state: &AppState, key: &str) -> Option<T> {
    match tokio::time::timeout(CACHE_OP_TIMEOUT, state.redis.get(key)).await {
        // A cached body that no longer parses (a shape from an older build) is
        // a miss, not an error: recompute overwrites it.
        Ok(Ok(Some(json))) => serde_json::from_str(&json).ok(),
        Ok(Ok(None)) => None,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "retention cache get failed");
            None
        }
        Err(_elapsed) => {
            tracing::warn!("retention cache get timed out");
            None
        }
    }
}

/// Best-effort: a failed put means the next request recomputes, nothing worse.
async fn cache_put<T: Serialize>(state: &AppState, key: &str, value: &T) {
    let Ok(json) = serde_json::to_string(value) else {
        return;
    };
    match tokio::time::timeout(
        CACHE_OP_TIMEOUT,
        state.redis.set_ex(key, &json, RETENTION_CACHE_TTL_SECS),
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!(error = %e, "retention cache put failed"),
        Err(_elapsed) => tracing::warn!("retention cache put timed out"),
    }
}

/// `None` — which cannot come from this build's compute path — is STALE:
/// age unknown means "refresh it", never "trust it forever".
fn is_fresh(computed_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    computed_at.is_some_and(|t| (now - t) < Duration::seconds(RETENTION_FRESH_FOR_SECS))
}

/// Everything a grid compute needs, resolved — the handler's cold miss and the
/// background refresh call the same [`compute_grid`] on the same inputs, so
/// the two can only ever produce the same bytes for the same key.
#[derive(Clone)]
struct GridInputs {
    key: String,
    app_id: Uuid,
    env: EnvFilter,
    g: Granularity,
    granularity: String,
    cohorts: i64,
    periods: i64,
    split: bool,
}

#[derive(Clone)]
struct LifecycleInputs {
    key: String,
    app_id: Uuid,
    env: EnvFilter,
    g: Granularity,
    granularity: String,
    periods: i64,
}

async fn compute_grid(state: &AppState, inputs: &GridInputs) -> Result<GridOut, ApiError> {
    let mut conn = db(state).await?;
    let as_of = sauron_db::rollups::as_of(&mut conn, &["analytics_events", "error_events"]).await?;
    let g = inputs.g;
    let step = g.step_days();
    let today = Utc::now().date_naive();
    let to = today + Duration::days(1);
    let from = to - Duration::days(inputs.cohorts * step);
    let as_of_day = as_of.map(|t| t.date_naive()).unwrap_or(today);
    let scope = ReadScope::new(inputs.app_id, inputs.env.clone());

    let floor = person_days::coverage_floor(&mut conn, &scope).await?;

    let primary = retention::retention_grid(
        &mut conn,
        scope.clone(),
        g,
        from,
        to,
        inputs.periods as i32,
        if inputs.split {
            ErrorSplit::Exposed
        } else {
            ErrorSplit::All
        },
    )
    .await?;

    let clean = if inputs.split {
        let rows = retention::retention_grid(
            &mut conn,
            scope,
            g,
            from,
            to,
            inputs.periods as i32,
            ErrorSplit::Clean,
        )
        .await?;
        Some(densify(
            &rows,
            inputs.periods as usize,
            step,
            as_of_day,
            floor,
        ))
    } else {
        None
    };

    Ok(GridOut {
        granularity: inputs.granularity.clone(),
        as_of,
        ready: true,
        cohorts: densify(&primary, inputs.periods as usize, step, as_of_day, floor),
        clean,
        computed_at: Some(Utc::now()),
    })
}

async fn compute_lifecycle(
    state: &AppState,
    inputs: &LifecycleInputs,
) -> Result<LifecycleOut, ApiError> {
    let mut conn = db(state).await?;
    let as_of = sauron_db::rollups::as_of(&mut conn, &["analytics_events", "error_events"]).await?;
    let g = inputs.g;
    let today = Utc::now().date_naive();
    let to = today + Duration::days(1);
    // One extra period back: the earliest displayed period needs its
    // PREDECESSOR in the window to classify returning-versus-resurrected.
    let from = to - Duration::days((inputs.periods + 1) * g.step_days());
    let scope = ReadScope::new(inputs.app_id, inputs.env.clone());

    let points = retention::lifecycle(&mut conn, scope, g, from, to).await?;
    Ok(LifecycleOut {
        granularity: inputs.granularity.clone(),
        as_of,
        ready: true,
        points,
        computed_at: Some(Utc::now()),
    })
}

enum RefreshInputs {
    Grid(GridInputs),
    Lifecycle(LifecycleInputs),
}

impl RefreshInputs {
    fn key(&self) -> &str {
        match self {
            RefreshInputs::Grid(i) => &i.key,
            RefreshInputs::Lifecycle(i) => &i.key,
        }
    }
}

/// Kick ONE background recompute for a stale key.
///
/// Single-flighted through a Redis `SET NX` lock, so a burst of stale hits
/// (every dashboard visitor at once) spawns one refresh, not one each. On
/// compute failure the lock is left to its TTL and doubles as a cooldown —
/// a permanently failing refresh is retried every two minutes, not every
/// request. No admission semaphore, unlike `active_users.rs`: see the module
/// docs for why.
fn spawn_refresh(state: &AppState, inputs: RefreshInputs) {
    let state = state.clone();
    tokio::spawn(async move {
        let lock_key = format!("{}:refresh", inputs.key());
        match tokio::time::timeout(
            CACHE_OP_TIMEOUT,
            state
                .redis
                .set_nx_ex(&lock_key, "1", RETENTION_REFRESH_LOCK_SECS),
        )
        .await
        {
            Ok(Ok(true)) => {}
            // Someone (possibly another replica) is already on it.
            Ok(Ok(false)) => return,
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "retention refresh lock failed");
                return;
            }
            Err(_elapsed) => {
                tracing::warn!("retention refresh lock timed out");
                return;
            }
        }
        // `?e`: `ApiError` deliberately has no `Display`; `Debug` names the
        // variant, which is what a log line needs.
        let outcome = match &inputs {
            RefreshInputs::Grid(i) => match compute_grid(&state, i).await {
                Ok(out) => {
                    cache_put(&state, &i.key, &out).await;
                    Ok(())
                }
                Err(e) => Err(e),
            },
            RefreshInputs::Lifecycle(i) => match compute_lifecycle(&state, i).await {
                Ok(out) => {
                    cache_put(&state, &i.key, &out).await;
                    Ok(())
                }
                Err(e) => Err(e),
            },
        };
        if let Err(e) = outcome {
            tracing::warn!(error = ?e, "retention background refresh failed");
        } else {
            let _ = tokio::time::timeout(CACHE_OP_TIMEOUT, state.redis.del(&lock_key)).await;
        }
    });
}

#[utoipa::path(
    get, path = "/v1/apps/{app_id}/retention", tag = "Analytics",
    summary = "Retention cohort grid",
    description = "Cohorts by first-seen period, with the share returning in each subsequent period. \
Served stale-while-revalidate: under an hour old as-is; between one and three hours as-is while one \
background refresh recomputes. `computed_at` states which.",
    params(("app_id" = Uuid, Path, description = "The app."), GridQuery), security(("bearerAuth" = [])),
    responses((status = 200, description = "The cohort grid.", body = GridOut),
              (status = 400, description = "Malformed window or period.", body = ErrorResponse), (status = 401, description = "Missing or invalid access token.", body = ErrorResponse), (status = 403, description = "No grant covers this scope.", body = ErrorResponse), (status = 503, description = "Query exceeded its time budget, or a required rollup is missing. The message names which.", body = ErrorResponse)),
)]
pub async fn grid(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<GridQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<GridOut>, ApiError> {
    let mut conn = db(&state).await?;
    let scope = super::scope::authorized_read_scope(
        &mut conn,
        auth.user_id,
        app_id,
        perm::EVENT_READ,
        raw_query.as_deref(),
    )
    .await?;

    let g = parse_granularity(&q.granularity)?;
    let cohorts = q.cohorts.clamp(1, MAX_DIM);
    let periods = q.periods.clamp(1, MAX_DIM);
    if cohorts * periods > MAX_RETENTION_CELLS {
        return Err(ApiError::BadRequest(format!(
            "cohorts x periods must not exceed {MAX_RETENTION_CELLS}; got \
             {cohorts} x {periods} = {}",
            cohorts * periods
        )));
    }
    let split = match q.split.as_deref() {
        None | Some("none") => false,
        Some("errors") => true,
        Some(other) => {
            return Err(ApiError::BadRequest(format!(
                "split must be 'none' or 'errors', got '{other}'"
            )))
        }
    };

    let ready = person_days::is_ready(&mut conn, app_id).await?;

    // A not-ready app ships NO cohort rows — and the response is NOT cached.
    // Returning whatever happens to be in the table would be a partial answer
    // indistinguishable from a complete one, and caching the refusal would
    // keep refusing for up to an hour after the operator's backfill finishes.
    if !ready {
        let as_of =
            sauron_db::rollups::as_of(&mut conn, &["analytics_events", "error_events"]).await?;
        return Ok(Json(GridOut {
            granularity: q.granularity,
            as_of,
            ready,
            cohorts: vec![],
            clean: None,
            computed_at: None,
        }));
    }
    // The compute path opens its own connection; return this one to the pool
    // rather than holding two per request.
    drop(conn);

    let inputs = GridInputs {
        key: retention_cache_key(
            "grid",
            app_id,
            &scope.env,
            &q.granularity,
            cohorts,
            periods,
            split,
            Utc::now().date_naive(),
        )?,
        app_id,
        env: scope.env,
        g,
        granularity: q.granularity,
        cohorts,
        periods,
        split,
    };

    if let Some(hit) = cache_get::<GridOut>(&state, &inputs.key).await {
        if !is_fresh(hit.computed_at, Utc::now()) {
            spawn_refresh(&state, RefreshInputs::Grid(inputs));
        }
        return Ok(Json(hit));
    }

    let out = compute_grid(&state, &inputs).await?;
    cache_put(&state, &inputs.key, &out).await;
    Ok(Json(out))
}

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct LifecycleQuery {
    #[serde(default = "default_granularity")]
    pub granularity: String,
    #[serde(default = "default_dim")]
    pub periods: i64,
}

#[derive(Serialize, Deserialize, utoipa::ToSchema)]
pub struct LifecycleOut {
    pub granularity: String,
    pub as_of: Option<DateTime<Utc>>,
    pub ready: bool,
    pub points: Vec<retention::LifecyclePoint>,
    /// Same contract as [`GridOut::computed_at`].
    #[serde(default)]
    pub computed_at: Option<DateTime<Utc>>,
}

#[utoipa::path(
    get, path = "/v1/apps/{app_id}/retention/lifecycle", tag = "Analytics",
    summary = "User lifecycle breakdown",
    description = "New, returning, resurrected and dormant users per period. Served \
stale-while-revalidate on the same contract as the grid; `computed_at` disclosures apply.",
    params(("app_id" = Uuid, Path, description = "The app."), LifecycleQuery), security(("bearerAuth" = [])),
    responses((status = 200, description = "Lifecycle series.", body = LifecycleOut),
              (status = 400, description = "Malformed window or period.", body = ErrorResponse), (status = 401, description = "Missing or invalid access token.", body = ErrorResponse), (status = 403, description = "No grant covers this scope.", body = ErrorResponse), (status = 503, description = "Query exceeded its time budget, or a required rollup is missing. The message names which.", body = ErrorResponse)),
)]
pub async fn lifecycle(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<LifecycleQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<LifecycleOut>, ApiError> {
    let mut conn = db(&state).await?;
    let scope = super::scope::authorized_read_scope(
        &mut conn,
        auth.user_id,
        app_id,
        perm::EVENT_READ,
        raw_query.as_deref(),
    )
    .await?;

    let g = parse_granularity(&q.granularity)?;
    let periods = q.periods.clamp(1, MAX_DIM);
    let ready = person_days::is_ready(&mut conn, app_id).await?;
    if !ready {
        // Uncached, same reasoning as the grid's not-ready branch.
        let as_of =
            sauron_db::rollups::as_of(&mut conn, &["analytics_events", "error_events"]).await?;
        return Ok(Json(LifecycleOut {
            granularity: q.granularity,
            as_of,
            ready,
            points: vec![],
            computed_at: None,
        }));
    }
    drop(conn);

    let inputs = LifecycleInputs {
        key: retention_cache_key(
            "lifecycle",
            app_id,
            &scope.env,
            &q.granularity,
            0,
            periods,
            false,
            Utc::now().date_naive(),
        )?,
        app_id,
        env: scope.env,
        g,
        granularity: q.granularity,
        periods,
    };

    if let Some(hit) = cache_get::<LifecycleOut>(&state, &inputs.key).await {
        if !is_fresh(hit.computed_at, Utc::now()) {
            spawn_refresh(&state, RefreshInputs::Lifecycle(inputs));
        }
        return Ok(Json(hit));
    }

    let out = compute_lifecycle(&state, &inputs).await?;
    cache_put(&state, &inputs.key, &out).await;
    Ok(Json(out))
}

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ChurnQuery {
    #[serde(default = "default_granularity")]
    pub granularity: String,
    #[serde(default = "default_silent")]
    pub silent_periods: i64,
    #[serde(default = "default_limit")]
    pub limit: i64,
    /// `column` for descending, `-column` for ascending — the bare form is
    /// DESC, matching `sortParam` in the dashboard and `parse_sort` in
    /// `routes/search.rs`. Columns: `last_seen`, `first_seen`, `events`,
    /// `errors`, `sessions`.
    pub sort: Option<String>,
    /// Opaque token from the previous page's `next_cursor`. Only valid for the
    /// same `sort` it was minted under.
    pub cursor: Option<String>,
}

fn default_silent() -> i64 {
    4
}
fn default_limit() -> i64 {
    50
}

/// `sort=` → (column, descending). Bare = descending, `-` = ascending.
fn parse_churn_sort(sort: Option<&str>) -> Result<(retention::ChurnSort, bool), ApiError> {
    let Some(raw) = sort else {
        return Ok((retention::ChurnSort::LastSeen, true));
    };
    let (name, descending) = match raw.strip_prefix('-') {
        Some(rest) => (rest, false),
        None => (raw, true),
    };
    match retention::ChurnSort::parse(name) {
        Some(col) => Ok((col, descending)),
        None => Err(ApiError::BadRequest(format!(
            "sort must be one of last_seen, first_seen, events, errors, sessions \
             (with optional '-' prefix for ascending), got '{raw}'"
        ))),
    }
}

/// Cursor wire format: `{value}|{distinct_id}`, value typed by the ACTIVE sort
/// column. The distinct_id half is taken verbatim (ids may themselves contain
/// any character except our separator's first occurrence — `splitn` keeps
/// everything after the first `|` intact).
fn parse_churn_cursor(
    cursor: &str,
    sort: retention::ChurnSort,
) -> Result<retention::ChurnCursor, ApiError> {
    let mut halves = cursor.splitn(2, '|');
    let (Some(v), Some(id)) = (halves.next(), halves.next()) else {
        return Err(ApiError::BadRequest("malformed cursor".into()));
    };
    if sort.is_time() {
        let t = DateTime::parse_from_rfc3339(v)
            .map_err(|_| ApiError::BadRequest("malformed cursor timestamp".into()))?
            .with_timezone(&Utc);
        Ok(retention::ChurnCursor::Time(t, id.to_string()))
    } else {
        let n: i64 = v
            .parse()
            .map_err(|_| ApiError::BadRequest("malformed cursor value".into()))?;
        Ok(retention::ChurnCursor::Count(n, id.to_string()))
    }
}

/// The `next_cursor` for a page ending on `row`, under `sort`.
fn churn_cursor_for(row: &retention::ChurnRow, sort: retention::ChurnSort) -> String {
    let v = match sort {
        retention::ChurnSort::LastSeen => row.last_seen.to_rfc3339(),
        retention::ChurnSort::FirstSeen => row.first_seen.to_rfc3339(),
        retention::ChurnSort::Events => row.events_count.to_string(),
        retention::ChurnSort::Errors => row.errors_count.to_string(),
        retention::ChurnSort::Sessions => row.sessions_count.to_string(),
    };
    format!("{v}|{}", row.distinct_id)
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct ChurnOut {
    pub ready: bool,
    pub silent_days: i64,
    pub people: Vec<retention::ChurnRow>,
    /// Opaque cursor for the next page, or `None` at the end. Bound to the
    /// `sort` it was minted under.
    pub next_cursor: Option<String>,
}

#[utoipa::path(
    get, path = "/v1/apps/{app_id}/retention/churn", tag = "Analytics",
    summary = "At-risk users",
    description = "Users who stopped appearing: silent for the given number of periods, with their \
lifetime aggregates. Sortable via `sort` (bare column = descending, `-column` = ascending) and \
paged by row-value keyset via `cursor`.",
    params(("app_id" = Uuid, Path, description = "The app."), ChurnQuery), security(("bearerAuth" = [])),
    responses((status = 200, description = "The at-risk page.", body = ChurnOut),
              (status = 400, description = "Malformed window, sort or cursor.", body = ErrorResponse), (status = 401, description = "Missing or invalid access token.", body = ErrorResponse), (status = 403, description = "No grant covers this scope.", body = ErrorResponse), (status = 503, description = "Query exceeded its time budget, or a required rollup is missing. The message names which.", body = ErrorResponse)),
)]
pub async fn churn(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<ChurnQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<ChurnOut>, ApiError> {
    let mut conn = db(&state).await?;
    let scope = super::scope::authorized_read_scope(
        &mut conn,
        auth.user_id,
        app_id,
        perm::EVENT_READ,
        raw_query.as_deref(),
    )
    .await?;

    let g = parse_granularity(&q.granularity)?;
    let limit = q.limit.clamp(1, 200);
    // "Churned" is expressed in the SAME unit the grid is drawn in, so the two
    // cards cannot disagree about what a period means.
    let silent_days = q.silent_periods.clamp(1, MAX_DIM) * g.step_days();
    let (sort, descending) = parse_churn_sort(q.sort.as_deref())?;
    let cursor = q
        .cursor
        .as_deref()
        .map(|c| parse_churn_cursor(c, sort))
        .transpose()?;

    // Churn reads `event_user_environments`, which is maintained by the write
    // path rather than by this feature's fold, so it does NOT need the
    // person-days gate. Reporting readiness anyway keeps the page's four cards
    // telling one story.
    let ready = person_days::is_ready(&mut conn, app_id).await?;

    // limit + 1: the probe row proves a next page exists. A bare
    // `len == limit` check advertises one exactly when the set ends on a page
    // boundary.
    let mut people = retention::churn(
        &mut conn,
        scope,
        silent_days,
        sort,
        descending,
        cursor,
        limit + 1,
    )
    .await?;
    let next_cursor = if people.len() as i64 > limit {
        people.truncate(limit as usize);
        people.last().map(|p| churn_cursor_for(p, sort))
    } else {
        None
    };

    Ok(Json(ChurnOut {
        ready,
        silent_days,
        people,
        next_cursor,
    }))
}
