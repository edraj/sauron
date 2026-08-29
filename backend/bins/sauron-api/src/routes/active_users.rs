//! Combined active users across the apps of one project, and the CSV export.
//!
//! A module of its own rather than more `analytics.rs`, because this is the
//! first project-scoped telemetry read in the product and its authorization
//! shape has nothing in common with `analytics.rs`'s single-app
//! `authorized_read_scope` handlers: the environment dimension is expressed
//! PER SELECTION, so a global `?environment_id=` is rejected outright.

use std::collections::{HashMap, HashSet};
use std::time::Duration as StdDuration;

use axum::extract::{Path, RawQuery, State};
use axum::Json;
use axum_extra::extract::Query;
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// `rbac::` is not optional: `sauron-auth`'s lib.rs re-exports only
// `authorize_*`, `effective_at*`, `ensure_preset_roles`, `perm` and
// `require_permission`. `grants_from_rows`, `reach_for`, `has_permission` and
// `resolve_env_filter` live behind the module path, exactly as
// `routes/projects.rs:11` and `routes/environments.rs:84` import them.
use sauron_auth::rbac::{grants_from_rows, has_permission, reach_for, resolve_env_filter};
use sauron_auth::{perm, AuthError, AuthUser};
use sauron_db::repo;
use sauron_db::repo::AppEnvScope;
use sauron_db::scope::EnvFilter;

use crate::error::ApiError;
use crate::openapi::ErrorResponse;
use crate::AppState;

/// Longest window a single request may cover.
const MAX_ACTIVE_USER_DAYS: i64 = 92;
/// Most apps one request may combine.
const MAX_SELECTED_APPS: usize = 20;
/// Cap on selections × displayed days — the thing actually being handed out.
/// 20 apps × 92 days is 1840 partition-day scans, and bounding the two
/// dimensions independently does not bound their product.
const MAX_SCAN_BUDGET: i64 = 1200;
/// How long an assembled report is served WITHOUT triggering any recompute.
///
/// Raised from the original 60 s read-through cache by request (2026-08-26):
/// the aggregate takes ~25 s on the reporting deployment, so recomputing it
/// once a minute on the request path made nearly every visit pay full price.
/// ~1 h of staleness was accepted explicitly — the overview's trade, applied
/// to this report. `computed_at` on the response is what keeps the age
/// honest.
const ACTIVE_USERS_FRESH_FOR_SECS: i64 = 3600;
/// How long an assembled report stays SERVABLE at all.
///
/// Between [`ACTIVE_USERS_FRESH_FOR_SECS`] and this, a hit is served as-is
/// and a background recompute is kicked (stale-while-revalidate) — the next
/// visitor sees current numbers. Past it, the entry is gone and the next
/// request pays the cold compute on-path. A latency optimization, NOT a DoS
/// control: the rate limiter, the scan budget and the semaphore are the
/// control.
const ACTIVE_USERS_CACHE_TTL_SECS: u64 = 3 * 3600;
/// Single-flight TTL for the background-refresh lock.
///
/// Must outlive one full compute (the request budget is 60 s) or a second
/// refresh piles onto a still-running first; must expire at all or a crashed
/// refresher wedges every future refresh of that key. When the semaphore is
/// busy the lock is released explicitly instead, so the next stale hit can
/// retry without waiting this out.
const ACTIVE_USERS_REFRESH_LOCK_SECS: u64 = 120;

/// The four values `SelectionView::resolved` can take. Named constants so the
/// handler, the tests and the dashboard cannot drift on a string literal.
const RESOLVED_ALL: &str = "all";
const RESOLVED_ONE: &str = "one";
const RESOLVED_SUBSET: &str = "subset";
const RESOLVED_UNATTRIBUTED: &str = "unattributed";

/// Deserialized with `axum_extra::extract::Query` (serde_html_form) because
/// `selection` is a repeated key. `environment_id` is deliberately NOT a field:
/// the dimension is per selection, and accepting a global one and ignoring it
/// is exactly the bug `routes::scope`'s module docs exist to prevent.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ActiveUsersQuery {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    #[serde(default)]
    pub selection: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ReportWindow {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ActiveUserPoint {
    pub day: NaiveDate,
    pub active_total: i64,
    pub active_identified: i64,
    pub active_guest: i64,
}

/// What the server actually queried for one selection.
///
/// `resolved` carries the RESOLVED filter, not the requested one, and it is a
/// tagged shape rather than `environment_id: Option<Uuid>`. That is not
/// cosmetic. `rbac.rs`'s `resolve_env_filter` turns a bare app request from a
/// partial-reach caller into `Subset(readable)`, so a member holding env grants
/// on 2 of an app's 5 environments who sends the default bare
/// `?selection=<app_uuid>` gets a number computed over 2 environments. With
/// `Option<Uuid>` that renders as `None` — indistinguishable from a true `All`
/// — under a picker that still reads "All environments". It matters more here
/// than elsewhere because `All` includes `environment_id IS NULL` rows while
/// `Subset` uses `= ANY(...)`, which never matches NULL, so two callers can
/// legitimately get different totals for what looks like the same selection.
///
/// `resolved` is a `String`, not the `&'static str` it wants to be, because
/// this report round-trips through the Redis cache and must therefore derive
/// `Deserialize`.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SelectionView {
    pub app_id: Uuid,
    pub app_name: String,
    pub resolved: String,
    /// Populated for `one` and `subset`. Empty otherwise.
    #[serde(default)]
    pub environment_ids: Vec<Uuid>,
    #[serde(default)]
    pub environment_labels: Vec<String>,
}

/// Derives `Deserialize` as well as `Serialize`, and every field added after
/// v1 must carry `#[serde(default)]`: a report cached by an older build has to
/// keep deserializing rather than missing the cache for a whole TTL.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ActiveUsersReport {
    pub requested: ReportWindow,
    pub effective: ReportWindow,
    pub truncated: bool,
    /// A full human sentence naming the effective floor date — the UI renders
    /// it verbatim.
    pub truncation_reason: Option<String>,
    pub selections: Vec<SelectionView>,
    pub series: Vec<ActiveUserPoint>,
    /// When these numbers were computed — the staleness disclosure the ~1 h
    /// serve-stale window depends on (the UI stamps it, the same contract as
    /// the overview's `computed_at`). `#[serde(default)]` per this struct's
    /// own rule above; an entry cached by a build predating this field reads
    /// as `None`, which [`is_fresh`] treats as stale: served instantly,
    /// refreshed in the background.
    #[serde(default)]
    pub computed_at: Option<DateTime<Utc>>,
    pub latest: Option<ActiveUserPoint>,
}

/// Repeated `?selection=<app_uuid>[:<env_token>]`, where `<env_token>` is an
/// `app_environments.id`, the literal `all`, or the literal `none`. A bare
/// `<app_uuid>` means `all`. UUIDs contain hyphens but never colons, so `:` is
/// unambiguous and the whole thing round-trips through
/// `URLSearchParams.getAll()` with no custom codec.
///
/// Parallel `app_ids=`/`env_ids=` arrays were rejected: a length mismatch or a
/// reordering silently pairs the wrong environment with the wrong app, with no
/// error. `Subset` is never requestable — the same rule `parse_env` already
/// enforces.
fn parse_selection(raw: &[String]) -> Result<Vec<(Uuid, EnvFilter)>, ApiError> {
    if raw.is_empty() {
        return Err(ApiError::BadRequest(
            "at least one `selection` is required".into(),
        ));
    }
    if raw.len() > MAX_SELECTED_APPS {
        return Err(ApiError::BadRequest(format!(
            "at most {MAX_SELECTED_APPS} selections are allowed, got {}",
            raw.len()
        )));
    }
    let mut out: Vec<(Uuid, EnvFilter)> = Vec::with_capacity(raw.len());
    let mut seen: HashSet<Uuid> = HashSet::new();
    for token in raw {
        let (app_part, env_part) = match token.split_once(':') {
            Some((a, e)) => (a, e),
            None => (token.as_str(), RESOLVED_ALL),
        };
        let app_id = Uuid::parse_str(app_part).map_err(|_| {
            ApiError::BadRequest(format!(
                "invalid selection {token:?}: {app_part:?} is not a UUID"
            ))
        })?;
        let env = match env_part {
            "all" => EnvFilter::All,
            "none" => EnvFilter::Unattributed,
            other => EnvFilter::One(Uuid::parse_str(other).map_err(|_| {
                ApiError::BadRequest(format!(
                    "invalid selection {token:?}: {other:?} is neither \"all\", \"none\", nor a UUID"
                ))
            })?),
        };
        if !seen.insert(app_id) {
            return Err(ApiError::BadRequest(format!(
                "app {app_id} appears more than once in `selection`"
            )));
        }
        out.push((app_id, env));
    }
    Ok(out)
}

/// Truncate to 00:00 UTC.
fn floor_to_utc_day(t: DateTime<Utc>) -> DateTime<Utc> {
    Utc.from_utc_datetime(
        &t.date_naive()
            .and_hms_opt(0, 0, 0)
            .expect("midnight is a valid time on every date"),
    )
}

/// Round a hot-tier watermark UP to a whole UTC day.
///
/// A function rather than four inline lines in the handler because this is the
/// computation the design's `effective.from == series[0].day` correspondence
/// rests on, and it is the only part of the tier clamp that is testable without
/// a database. Rounding DOWN would put `effective.from` back inside the
/// tiered-out range; leaving it mid-day would render a partial first day as a
/// full day's count, since `active_users_combined` builds its grid from
/// `(effective_from AT TIME ZONE 'UTC')::date` — the same defect flooring the
/// request's `from` fixes on the way in.
fn align_clamp_up(floor: DateTime<Utc>) -> DateTime<Utc> {
    let down = floor_to_utc_day(floor);
    if down < floor {
        down + chrono::Duration::days(1)
    } else {
        down
    }
}

/// Floor both ends to UTC day boundaries, then validate.
///
/// Flooring loses nothing — the output is day-bucketed — and it fixes a real
/// correctness bug the raw contract has, where a mid-day `from` renders a
/// partial first day as a full day's count. It is also what makes the cache key
/// mean something: full-precision RFC3339 against day-granular output means
/// `from + 1µs` mints a brand-new key for a byte-identical series, i.e.
/// unlimited free cache misses.
fn validate_window(
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    selections: usize,
) -> Result<(DateTime<Utc>, DateTime<Utc>), ApiError> {
    let from = floor_to_utc_day(from);
    let to = floor_to_utc_day(to);
    if to <= from {
        return Err(ApiError::BadRequest(
            "`to` must be at least one UTC day after `from`".into(),
        ));
    }
    let days = (to - from).num_days();
    if days > MAX_ACTIVE_USER_DAYS {
        return Err(ApiError::BadRequest(format!(
            "time range must not exceed {MAX_ACTIVE_USER_DAYS} days"
        )));
    }
    let budget = selections as i64 * days;
    if budget > MAX_SCAN_BUDGET {
        return Err(ApiError::BadRequest(format!(
            "selections × days must not exceed {MAX_SCAN_BUDGET} (got {selections} × {days} = {budget})"
        )));
    }
    Ok((from, to))
}

/// The last point strictly before `today_utc`.
///
/// Today is still accumulating, and a headline tile that falls as the day
/// starts and climbs until midnight reads as a product problem. `None` — a
/// window containing only today — must render as an em-dash, never as `0`:
/// zero active users is a real and reportable answer, and rendering "we have no
/// complete day yet" as that answer is exactly the plausible-but-wrong number
/// this feature exists to stop producing.
fn latest_full_day(series: &[ActiveUserPoint], today_utc: NaiveDate) -> Option<&ActiveUserPoint> {
    series.iter().rev().find(|p| p.day < today_utc)
}

fn resolved_label(env: &EnvFilter) -> &'static str {
    match env {
        EnvFilter::All => RESOLVED_ALL,
        EnvFilter::One(_) => RESOLVED_ONE,
        EnvFilter::Subset(_) => RESOLVED_SUBSET,
        EnvFilter::Unattributed => RESOLVED_UNATTRIBUTED,
    }
}

/// The canonical, injective document the cache key hashes.
#[derive(Serialize, utoipa::ToSchema)]
struct CacheFingerprint<'a> {
    project_id: Uuid,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    scopes: &'a [AppEnvScope],
}

/// Sort scopes by `app_id` and each `Subset`'s uuids, so two requests that mean
/// the same thing hash the same.
fn canonical_scopes(scopes: &[AppEnvScope]) -> Vec<AppEnvScope> {
    let mut out: Vec<AppEnvScope> = scopes
        .iter()
        .map(|s| AppEnvScope {
            app_id: s.app_id,
            env: match &s.env {
                EnvFilter::Subset(ids) => {
                    let mut ids = ids.clone();
                    ids.sort();
                    EnvFilter::Subset(ids)
                }
                other => other.clone(),
            },
        })
        .collect();
    out.sort_by_key(|s| s.app_id);
    out
}

/// The Redis key for one resolved report.
///
/// The fingerprint must be INJECTIVE BY CONSTRUCTION. `admin_storage`'s
/// `hash_token(sorted_org_uuids.join(","))` is injective only because every
/// element is a fixed-length UUID with no nesting; this one is a list of
/// `(app_id, EnvFilter)` pairs where `Subset(Vec<Uuid>)` is variable-length —
/// two levels of repetition. A naive join lets two distinct resolved selections
/// flatten to the same bytes, and the cached entry holds the whole series plus
/// every `selections[].app_name`, so a collision is a cross-tenant DATA LEAK,
/// not a staleness bug. JSON is self-delimiting, so no flattening ambiguity
/// exists.
///
/// The key uses the RESOLVED filter, never the requested token. That is what
/// keeps a caller with app-wide reach (`All`) and a caller with only env-X
/// reach (`Subset([X])`) from ever sharing an entry. Treat any deviation from
/// that in review as a Critical.
fn cache_key(
    project_id: Uuid,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    scopes: &[AppEnvScope],
) -> Result<String, ApiError> {
    let canon = canonical_scopes(scopes);
    let fingerprint = CacheFingerprint {
        project_id,
        from,
        to,
        scopes: &canon,
    };
    let json =
        serde_json::to_string(&fingerprint).map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(format!(
        "sauron:activeusers:{}",
        sauron_auth::hash_token(&json)
    ))
}

/// Requests per minute per user. This is the heaviest query in the product and
/// the lowest-privileged role (`Viewer` holds `event:read`) can run it. It is
/// also the repo's first read-route rate limit, and that is the point: it is
/// the template for the next one.
const ACTIVE_USERS_RATE_LIMIT: u32 = 30;
const ACTIVE_USERS_RATE_WINDOW_SECS: u64 = 60;

/// Budget for one Redis command.
///
/// Do NOT copy `collect_storage_cached`'s untimed `get`/`set_ex`.
/// `sauron-redis` builds its connection with `set_response_timeout(None)`, and
/// `routes/auth.rs` records the measurement: 9-19 s per command against a dead
/// Redis, "long enough that the in-flight cap fills and the whole API stalls".
/// "A Redis error is logged and the report computed" is only true for an
/// ERROR; an outage is a hang, twice per request. `admin_storage` gets away
/// without this because it is a rarely-loaded admin page; this is a nav-item
/// page with a Refresh button.
const CACHE_OP_TIMEOUT: StdDuration = StdDuration::from_millis(500);

/// `GET /v1/projects/{project_id}/active-users`
#[utoipa::path(
    get, path = "/v1/projects/{project_id}/active-users", tag = "Analytics",
    summary = "Active-users report",
    description = "\
The heaviest query in the product, and runnable by the lowest-privileged role — \
so it is admitted through a small semaphore and answers **503 rather than \
queueing** when saturated. Queueing here would surface as connection-pool \
failures on unrelated endpoints, including login and health.

Results are served stale-while-revalidate: under an hour old they are returned \
as-is; between one and three hours they are returned immediately and refreshed \
in the background. `computed_at` states which. A cache hit costs no admission \
permit.",
    params(("project_id" = Uuid, Path, description = "The project."), ActiveUsersQuery), security(("bearerAuth" = [])),
    responses((status = 200, description = "The report, with `computed_at` disclosing its freshness.", body = ActiveUsersReport),
              (status = 400, description = "Malformed selection or window.", body = ErrorResponse), (status = 401, description = "Missing or invalid access token.", body = ErrorResponse), (status = 403, description = "No grant covers this scope.", body = ErrorResponse),
              (status = 503, description = "Report admission is saturated, or `event_users.identified_at` is missing because migrations have not been run. Retry, or run sauron-migrate.", body = ErrorResponse)),
)]
pub async fn active_users(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(project_id): Path<Uuid>,
    Query(q): Query<ActiveUsersQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<ActiveUsersReport>, ApiError> {
    let report = gated_report(&state, auth.user_id, project_id, &q, raw_query.as_deref()).await?;
    Ok(Json(report))
}

/// `GET /v1/projects/{project_id}/active-users.csv`
///
/// A separate route rather than `?format=csv`: with a format parameter the
/// handler's success type collapses to `Response` for both shapes and content
/// negotiation via a query param is easy to mis-validate. Both routes call one
/// `build_report`, so they can never disagree about the numbers — the only
/// thing `?format=csv` really bought.
#[utoipa::path(
    get, path = "/v1/projects/{project_id}/active-users.csv", tag = "Analytics",
    summary = "Active-users report as CSV",
    description = "Same computation and same admission gate as the JSON report, streamed as `text/csv`.",
    params(("project_id" = Uuid, Path, description = "The project."), ActiveUsersQuery), security(("bearerAuth" = [])),
    responses((status = 200, description = "CSV export.", content_type = "text/csv", body = String),
              (status = 400, description = "Malformed selection or window.", body = ErrorResponse), (status = 401, description = "Missing or invalid access token.", body = ErrorResponse), (status = 403, description = "No grant covers this scope.", body = ErrorResponse),
              (status = 503, description = "Report admission is saturated, or a required column is missing.", body = ErrorResponse)),
)]
pub async fn active_users_csv(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(project_id): Path<Uuid>,
    Query(q): Query<ActiveUsersQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<axum::response::Response, ApiError> {
    let report = gated_report(&state, auth.user_id, project_id, &q, raw_query.as_deref()).await?;

    let mut out = String::new();
    crate::csv::write_row(
        &mut out,
        &["day", "active_total", "active_identified", "active_guest"],
    );
    // Both halves ride along rather than only the total: a spreadsheet is
    // exactly where someone re-derives a figure months later with no page
    // around it to carry the cross-app-matching caveat, and a guest column
    // they can see is the only warning that survives the download. The
    // selection context deliberately stays out of the body — it is a per-file
    // constant, not a per-row value.
    for p in &report.series {
        let day = p.day.to_string();
        let total = p.active_total.to_string();
        let identified = p.active_identified.to_string();
        let guest = p.active_guest.to_string();
        crate::csv::write_row(&mut out, &[&day, &total, &identified, &guest]);
    }

    // Built from the EFFECTIVE window, so a downloaded file's name matches its
    // contents even when the tier clamp shortened it.
    let filename = format!(
        "sauron-active-users-{}-{}_{}.csv",
        project_id,
        report.effective.from.format("%Y%m%d"),
        report.effective.to.format("%Y%m%d"),
    );

    // Buffered `String` -> `Body::from`: at most 93 lines of ASCII. Streaming
    // is not an option anyway — `backend/Cargo.toml` has no `futures`, no
    // `tokio-util`, and tokio's feature list has no `fs`.
    axum::response::Response::builder()
        .header(axum::http::header::CONTENT_TYPE, "text/csv; charset=utf-8")
        .header(
            axum::http::header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{filename}\""),
        )
        .body(axum::body::Body::from(out))
        .map_err(|e| ApiError::Internal(e.to_string()))
}

/// The guard stack both routes share, in the order the failures must be
/// reported: parameter shape, schema readiness, per-user rate, then admission.
async fn gated_report(
    state: &AppState,
    user_id: Uuid,
    project_id: Uuid,
    q: &ActiveUsersQuery,
    raw_query: Option<&str>,
) -> Result<ActiveUsersReport, ApiError> {
    // The environment dimension is expressed PER SELECTION. Accepting a global
    // one and ignoring it is the bug `routes::scope` exists to prevent.
    crate::routes::scope::reject_environment_id(
        crate::routes::scope::raw_environment_id(raw_query).as_deref(),
    )?;

    if !state.event_users_identified {
        return Err(ApiError::Unavailable(
            "schema_migration_required",
            "event_users.identified_at is missing; run sauron-migrate, then restart \
             sauron-api (see packaging/rpm/SETUP.md §11)"
                .into(),
        ));
    }

    crate::routes::auth::rate_limit(
        state,
        &format!("sauron:analytics:active_users:{user_id}"),
        ACTIVE_USERS_RATE_LIMIT,
        ACTIVE_USERS_RATE_WINDOW_SECS,
    )
    .await?;

    // The semaphore is no longer taken here: a cache HIT costs no permit (and
    // can no longer be shed 503-busy while three computes run). The permit
    // moved to where the expensive query actually runs — `build_report`'s
    // cold-miss branch and the background refresh — which are the only things
    // it ever needed to bound.
    build_report(state, user_id, project_id, q).await
}

async fn cache_get(state: &AppState, key: &str) -> Option<ActiveUsersReport> {
    match tokio::time::timeout(CACHE_OP_TIMEOUT, state.redis.get(key)).await {
        Ok(Ok(Some(json))) => serde_json::from_str(&json).ok(),
        Ok(Ok(None)) => None,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "active-users cache read failed");
            None
        }
        Err(_elapsed) => {
            tracing::warn!("active-users cache read timed out");
            None
        }
    }
}

async fn cache_put(state: &AppState, key: &str, report: &ActiveUsersReport) {
    let Ok(json) = serde_json::to_string(report) else {
        return;
    };
    match tokio::time::timeout(
        CACHE_OP_TIMEOUT,
        state.redis.set_ex(key, &json, ACTIVE_USERS_CACHE_TTL_SECS),
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!(error = %e, "active-users cache write failed"),
        Err(_elapsed) => tracing::warn!("active-users cache write timed out"),
    }
}

/// Resolve, authorize, clamp, cache and query. The single source of both the
/// JSON body and the CSV body.
async fn build_report(
    state: &AppState,
    user_id: Uuid,
    project_id: Uuid,
    q: &ActiveUsersQuery,
) -> Result<ActiveUsersReport, ApiError> {
    let selections = parse_selection(&q.selection)?;
    let (from, to) = validate_window(q.from, q.to, selections.len())?;
    let requested_app_ids: Vec<Uuid> = selections.iter().map(|(a, _)| *a).collect();

    let mut conn = crate::routes::db(state).await?;

    // --- the three-step reach pattern, verbatim ---------------------------
    // `repo::orgs_with_permission` is UNUSABLE here: it hardcodes
    // `g.scope_type = 'org'` and would 403 every project-, app- and env-scoped
    // member.
    let org_id = repo::project_org(&mut conn, project_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let rows = repo::user_grants_in_org(&mut conn, user_id, org_id).await?;
    if rows.is_empty() {
        return Err(ApiError::Auth(AuthError::Forbidden));
    }
    let grants = grants_from_rows(rows);
    let reach = reach_for(&grants, perm::EVENT_READ);
    if !reach.org && reach.projects.is_empty() && reach.apps.is_empty() && reach.envs.is_empty() {
        return Err(ApiError::Auth(AuthError::Forbidden));
    }

    // --- app-in-project validation ---------------------------------------
    // The caller's app ids carry no FK to the path's project, so this is
    // checked by id rather than inferred, mirroring how `validate_scopes_in_org`
    // treats a scope id that does not belong.
    let ancestries = repo::app_ancestries(&mut conn, &requested_app_ids).await?;
    let in_project: HashSet<Uuid> = ancestries
        .iter()
        .filter(|(_, project, _)| *project == project_id)
        .map(|(app, _, _)| *app)
        .collect();
    for (app_id, _) in &selections {
        if !in_project.contains(app_id) {
            return Err(ApiError::BadRequest(format!(
                "app {app_id} is not in project {project_id}"
            )));
        }
    }

    // --- per-selection environment resolution ----------------------------
    // Folded into a per-app map, NEVER passed as a flat vector: see
    // `repo::env_ids_for_apps`'s doc comment for the exact way the union
    // breaks both of `resolve_env_filter`'s decisions towards granting.
    let env_rows = repo::env_ids_for_apps(&mut conn, &requested_app_ids).await?;
    let mut envs_by_app: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
    for (app_id, env_id) in env_rows {
        envs_by_app.entry(app_id).or_default().push(env_id);
    }

    let mut scopes: Vec<AppEnvScope> = Vec::with_capacity(selections.len());
    let mut denied: Vec<String> = Vec::new();
    for (app_id, requested) in &selections {
        let app_env_ids = envs_by_app.get(app_id).map(Vec::as_slice).unwrap_or(&[]);
        // Fast path: an app-wide holder asking for everything needs no
        // narrowing at all.
        let resolved = if matches!(requested, EnvFilter::All)
            && has_permission(
                &grants,
                perm::EVENT_READ,
                org_id,
                Some(project_id),
                Some(*app_id),
                None,
            ) {
            EnvFilter::All
        } else {
            // Reusing the shipped pure decision function rather than
            // re-deriving the cascade preserves `UnattributedNeedsAppReach`
            // (so `selection=<app>:none` still requires app-wide reach) and
            // the ordering of `EnvNotInApp` before `EnvNotGranted` (so probing
            // for env ids learns nothing).
            match resolve_env_filter(
                &grants,
                perm::EVENT_READ,
                org_id,
                project_id,
                *app_id,
                app_env_ids,
                requested.clone(),
            ) {
                Ok(f) => f,
                Err(_) => {
                    denied.push(app_id.to_string());
                    continue;
                }
            }
        };
        scopes.push(AppEnvScope {
            app_id: *app_id,
            env: resolved,
        });
    }
    // Partial reach is a 403, never partial data. There is no honest way to
    // render "combined active users across A,B,C,D,E" from A,B,C: a number
    // computed over a silent subset is a wrong number presented as a right
    // one, and the CSV carries it out of the UI where no notice travels with
    // it. The denied ids are echoed because the caller supplied them, so
    // nothing new is disclosed, and the page needs them to drop a stale
    // selection and retry.
    if !denied.is_empty() {
        return Err(ApiError::Forbidden(format!(
            "no read access to app(s): {}",
            denied.join(", ")
        )));
    }

    // --- the tier clamp ---------------------------------------------------
    // `None` for a table means nothing has ever been tiered for it, so it
    // imposes no floor; the union is only complete from the MAXIMUM of the
    // watermarks that are present. Deliberately conservative: between
    // sauron-tier's export and the DETACH+DROP `TIER_DROP_LAG_HOURS` later,
    // rows past the watermark are still physically in Postgres, so a caller
    // will sometimes see `truncated: true` for a day that would still have
    // returned rows. Reporting numbers that vanish 24 h later is worse.
    let mut floor: Option<DateTime<Utc>> = None;
    for table in ["analytics_events", "error_events"] {
        if let Ok(Some(wm)) = repo::get_watermark(&mut conn, table).await {
            floor = Some(floor.map_or(wm, |cur: DateTime<Utc>| cur.max(wm)));
        }
    }

    let mut effective_from = from;
    let mut truncated = false;
    let mut truncation_reason: Option<String> = None;
    if let Some(f) = floor {
        if from < f {
            // Round UP to a whole UTC day. The grid starts at
            // `(from AT TIME ZONE 'UTC')::date`, so a mid-day floor would
            // render a partial day as a full one — the same defect flooring
            // `from` fixes on the request side. The helper is unit-tested in
            // Task 9's `a_mid_day_clamp_rounds_up_to_the_next_whole_utc_day`.
            let aligned = align_clamp_up(f);
            effective_from = aligned;
            truncated = true;
            truncation_reason = Some(format!(
                "Data older than {} has been moved to cold storage, so this report starts \
                 there instead of at {}.",
                aligned.date_naive(),
                from.date_naive()
            ));
        }
    }

    let requested_window = ReportWindow { from, to };
    let effective_window = ReportWindow {
        from: effective_from,
        to,
    };

    // --- selection views (cosmetic; the authorization input above is what
    // must stay unfiltered) ----------------------------------------------
    let apps = repo::list_apps_for_project(&mut conn, project_id).await?;
    let app_names: HashMap<Uuid, String> = apps.into_iter().map(|a| (a.id, a.name)).collect();
    let mut selection_views: Vec<SelectionView> = Vec::with_capacity(scopes.len());
    for s in &scopes {
        let (environment_ids, environment_labels) = match &s.env {
            EnvFilter::One(id) => (vec![*id], env_labels(&mut conn, s.app_id, &[*id]).await?),
            EnvFilter::Subset(ids) => (ids.clone(), env_labels(&mut conn, s.app_id, ids).await?),
            EnvFilter::All | EnvFilter::Unattributed => (Vec::new(), Vec::new()),
        };
        selection_views.push(SelectionView {
            app_id: s.app_id,
            app_name: app_names
                .get(&s.app_id)
                .cloned()
                .unwrap_or_else(|| s.app_id.to_string()),
            resolved: resolved_label(&s.env).to_string(),
            environment_ids,
            environment_labels,
        });
    }

    // The cache key uses the RESOLVED filters and the DAY-FLOORED requested
    // window, so the JSON call and the CSV call moments later produce the same
    // key by construction.
    let key = cache_key(project_id, from, to, &scopes)?;

    // Never hold a pooled connection across network I/O — the API pool is 16
    // for the whole process and Redis is a different host.
    drop(conn);

    // Everything the expensive assembly needs, captured ONCE — the on-path
    // cold miss and the background refresh must build byte-identical reports
    // for the same key, and two argument lists would eventually disagree.
    // Reusing the request's RESOLVED scopes off-path is sound because they
    // are exactly what the key hashes: the refresh only ever overwrites an
    // entry with a recomputation of itself.
    let inputs = RefreshInputs {
        key,
        scopes,
        requested: requested_window,
        effective: effective_window,
        truncated,
        truncation_reason,
        selections: selection_views,
    };

    if let Some(hit) = cache_get(state, &inputs.key).await {
        // Serve whatever is cached, instantly — that is the whole point. A
        // stale hit (or one cached by a build predating `computed_at`)
        // additionally kicks a background recompute so the NEXT visitor sees
        // current numbers; this visitor keeps the ~1 h-old ones, which is the
        // accepted trade.
        if !is_fresh(hit.computed_at, Utc::now()) {
            spawn_refresh(state, inputs);
        }
        return Ok(hit);
    }

    // Cold miss — the one path that still computes on the request clock.
    //
    // `try_acquire`, not `acquire`: 503 ahead of the pool rather than queueing
    // behind it. The pool is 16 connections for the WHOLE process and
    // `POOL_WAIT_TIMEOUT` is 5 s, so sixteen people hitting a cold report — or
    // one person with the shareable URL open in a few tabs — would starve
    // /v1/auth/login and /health with "db pool checkout failed" 500s.
    // `ConcurrencyLimitLayer` and `TimeoutLayer` shed the HTTP request but
    // cancel neither the Postgres query nor the pool slot.
    //
    // `let _permit`, never `let _`: the latter drops the permit immediately and
    // the gate becomes a no-op that still compiles.
    let _permit = state.active_users_gate.try_acquire().map_err(|_| {
        ApiError::Unavailable(
            "busy",
            "too many active-user reports are already running; retry shortly".into(),
        )
    })?;
    assemble_report(state, &inputs).await
}

/// The resolved, authorized ingredients of one cache entry — see the comment
/// at its construction in [`build_report`].
struct RefreshInputs {
    key: String,
    scopes: Vec<AppEnvScope>,
    requested: ReportWindow,
    effective: ReportWindow,
    truncated: bool,
    truncation_reason: Option<String>,
    selections: Vec<SelectionView>,
}

/// Whether a cached report is young enough to serve without any recompute.
///
/// `None` — an entry cached before `computed_at` existed — is STALE, not
/// fresh: age unknown means "refresh it", never "trust it forever".
fn is_fresh(computed_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    computed_at
        .is_some_and(|t| now.signed_duration_since(t).num_seconds() < ACTIVE_USERS_FRESH_FOR_SECS)
}

/// Run the aggregate, assemble the report, cache it. The ONE producer of
/// cache entries — called on-path for a cold miss and off-path by
/// [`spawn_refresh`].
async fn assemble_report(
    state: &AppState,
    inputs: &RefreshInputs,
) -> Result<ActiveUsersReport, ApiError> {
    let rows = if inputs.effective.from >= inputs.effective.to {
        // The clamp swallowed the whole window. Skip the scan entirely rather
        // than paying for a query that can only return an empty grid.
        Vec::new()
    } else {
        let mut conn = crate::routes::db(state).await?;
        let rows = repo::active_users_combined(
            &mut conn,
            &inputs.scopes,
            inputs.effective.from,
            inputs.effective.to,
        )
        .await?;
        drop(conn);
        rows
    };

    let series: Vec<ActiveUserPoint> = rows
        .into_iter()
        .map(|r| ActiveUserPoint {
            day: r.day,
            active_total: r.active_total,
            active_identified: r.active_identified,
            active_guest: r.active_guest,
        })
        .collect();
    let latest = latest_full_day(&series, Utc::now().date_naive()).cloned();

    let report = ActiveUsersReport {
        requested: inputs.requested.clone(),
        effective: inputs.effective.clone(),
        truncated: inputs.truncated,
        truncation_reason: inputs.truncation_reason.clone(),
        selections: inputs.selections.clone(),
        series,
        latest,
        computed_at: Some(Utc::now()),
    };
    cache_put(state, &inputs.key, &report).await;
    Ok(report)
}

/// Recompute a stale entry off the request path, at most once at a time per
/// key across every replica.
///
/// Two gates, both mandatory: the Redis `SET NX` single-flight (a popular
/// stale report would otherwise spawn one refresh per visitor — thundering
/// herd on the heaviest query in the product), and the SAME semaphore the
/// on-path compute holds (a refresh IS that query; exempting it would let
/// background work bypass the one bound that protects the pool). When the
/// semaphore is full the lock is released so the next stale hit retries
/// promptly; when the lock survives to its TTL it doubles as a cooldown
/// against a failing refresh being re-kicked every request.
fn spawn_refresh(state: &AppState, inputs: RefreshInputs) {
    let state = state.clone();
    tokio::spawn(async move {
        let lock_key = format!("{}:refresh", inputs.key);
        match tokio::time::timeout(
            CACHE_OP_TIMEOUT,
            state
                .redis
                .set_nx_ex(&lock_key, "1", ACTIVE_USERS_REFRESH_LOCK_SECS),
        )
        .await
        {
            Ok(Ok(true)) => {}
            // Someone (possibly another replica) is already on it.
            Ok(Ok(false)) => return,
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "active-users refresh lock failed");
                return;
            }
            Err(_elapsed) => {
                tracing::warn!("active-users refresh lock timed out");
                return;
            }
        }
        let Ok(_permit) = state.active_users_gate.try_acquire() else {
            // Best-effort: an expired lock self-heals in 120 s anyway.
            let _ = tokio::time::timeout(CACHE_OP_TIMEOUT, state.redis.del(&lock_key)).await;
            return;
        };
        // `?e`, not `%e`: `ApiError` is a response type and deliberately has
        // no `Display`; its `Debug` names the variant, which is what a log
        // line needs.
        if let Err(e) = assemble_report(&state, &inputs).await {
            tracing::warn!(error = ?e, "active-users background refresh failed");
        }
    });
}

/// Human names for a resolved environment id list.
///
/// Per-app rather than batched, deliberately: this is DISPLAY data, bounded by
/// `MAX_SELECTED_APPS`, and `list_app_environments` applies an ordering and a
/// cap that would be wrong to feed into an authorization decision.
/// `env_ids_for_apps` is the unlimited, unordered call that feeds that.
async fn env_labels(
    conn: &mut sauron_db::AsyncPgConnection,
    app_id: Uuid,
    ids: &[Uuid],
) -> Result<Vec<String>, ApiError> {
    let views = repo::list_app_environments(conn, app_id, true).await?;
    let by_id: HashMap<Uuid, String> = views
        .into_iter()
        .map(|v| (v.enrollment.id, v.name))
        .collect();
    Ok(ids
        .iter()
        .map(|id| by_id.get(id).cloned().unwrap_or_else(|| id.to_string()))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uuid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    #[test]
    fn a_bare_app_id_means_all_environments() {
        let a = uuid(1);
        let parsed = parse_selection(&[a.to_string()]).expect("bare uuid");
        assert_eq!(parsed, vec![(a, EnvFilter::All)]);
    }

    #[test]
    fn the_three_env_tokens_map_to_the_three_requestable_filters() {
        let a = uuid(1);
        let b = uuid(2);
        let c = uuid(3);
        let e = uuid(9);
        let parsed =
            parse_selection(&[format!("{a}:all"), format!("{b}:none"), format!("{c}:{e}")])
                .expect("three tokens");
        assert_eq!(
            parsed,
            vec![
                (a, EnvFilter::All),
                (b, EnvFilter::Unattributed),
                (c, EnvFilter::One(e)),
            ]
        );
    }

    #[test]
    fn a_malformed_app_uuid_is_a_400_naming_the_token() {
        let err = parse_selection(&["not-a-uuid:all".to_string()]).expect_err("must reject");
        assert!(format!("{err:?}").contains("not-a-uuid"), "{err:?}");
    }

    #[test]
    fn an_unknown_env_token_is_a_400_naming_the_token() {
        let a = uuid(1);
        let err = parse_selection(&[format!("{a}:production")]).expect_err("must reject");
        assert!(format!("{err:?}").contains("production"), "{err:?}");
    }

    #[test]
    fn a_duplicate_app_id_is_a_400() {
        let a = uuid(1);
        let err = parse_selection(&[a.to_string(), format!("{a}:none")]).expect_err("must reject");
        assert!(format!("{err:?}").contains("more than once"), "{err:?}");
    }

    #[test]
    fn an_empty_selection_is_a_400() {
        assert!(parse_selection(&[]).is_err());
    }

    #[test]
    fn more_than_max_selected_apps_is_a_400() {
        let raw: Vec<String> = (0..=MAX_SELECTED_APPS)
            .map(|i| uuid(i as u128 + 1).to_string())
            .collect();
        let err = parse_selection(&raw).expect_err("must reject");
        assert!(format!("{err:?}").contains("at most 20"), "{err:?}");
    }

    #[test]
    fn the_window_is_floored_to_utc_days() {
        let from = DateTime::parse_from_rfc3339("2026-05-04T13:37:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let to = DateTime::parse_from_rfc3339("2026-05-06T01:02:03Z")
            .unwrap()
            .with_timezone(&Utc);
        let (f, t) = validate_window(from, to, 1).expect("valid");
        assert_eq!(f.to_rfc3339(), "2026-05-04T00:00:00+00:00");
        assert_eq!(t.to_rfc3339(), "2026-05-06T00:00:00+00:00");
    }

    #[test]
    fn a_window_shorter_than_one_day_is_a_400() {
        let from = DateTime::parse_from_rfc3339("2026-05-04T01:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let to = DateTime::parse_from_rfc3339("2026-05-04T23:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert!(
            validate_window(from, to, 1).is_err(),
            "both floor to the same day"
        );
        assert!(validate_window(to, from, 1).is_err(), "`to` before `from`");
    }

    #[test]
    fn a_window_longer_than_the_cap_is_a_400() {
        let from = DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let to = from + chrono::Duration::days(MAX_ACTIVE_USER_DAYS + 1);
        let err = validate_window(from, to, 1).expect_err("must reject");
        assert!(format!("{err:?}").contains("92 days"), "{err:?}");
    }

    #[test]
    fn the_scan_budget_bounds_the_product_not_each_dimension() {
        let from = DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let to = from + chrono::Duration::days(92);
        // 20 apps and 92 days are each individually legal; 1840 is not.
        let err = validate_window(from, to, 20).expect_err("must reject");
        assert!(format!("{err:?}").contains("1200"), "{err:?}");
        assert!(validate_window(from, to, 13).is_ok(), "13 × 92 = 1196");
    }

    fn point(day: &str, total: i64) -> ActiveUserPoint {
        ActiveUserPoint {
            day: day.parse().expect("valid date"),
            active_total: total,
            active_identified: 0,
            active_guest: total,
        }
    }

    #[test]
    fn latest_full_day_skips_today() {
        let series = vec![point("2026-05-04", 3), point("2026-05-05", 9)];
        let today: NaiveDate = "2026-05-05".parse().unwrap();
        assert_eq!(
            latest_full_day(&series, today).map(|p| p.day.to_string()),
            Some("2026-05-04".to_string())
        );
    }

    #[test]
    fn latest_full_day_is_none_when_the_window_contains_only_today() {
        let series = vec![point("2026-05-05", 9)];
        let today: NaiveDate = "2026-05-05".parse().unwrap();
        assert!(
            latest_full_day(&series, today).is_none(),
            "the tiles must render an em-dash, never 0"
        );
    }

    /// The `effective.from == series[0].day` correspondence, pinned on the one
    /// computation that can break it. A clamp landing mid-day inside the display
    /// window must move `effective.from` UP to the next midnight, because the
    /// day grid starts at `(effective_from AT TIME ZONE 'UTC')::date` and a
    /// partial day rendered as a full one is a plausible-but-wrong number.
    #[test]
    fn a_mid_day_clamp_rounds_up_to_the_next_whole_utc_day() {
        let mid_day = DateTime::parse_from_rfc3339("2026-05-04T13:37:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let aligned = align_clamp_up(mid_day);
        assert_eq!(aligned.to_rfc3339(), "2026-05-05T00:00:00+00:00");
        assert_eq!(
            aligned.date_naive(),
            point("2026-05-05", 0).day,
            "this is the day the series grid starts on, i.e. `series[0].day`"
        );

        // A watermark already on a boundary must not cost a whole day of data.
        let midnight = DateTime::parse_from_rfc3339("2026-05-04T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            align_clamp_up(midnight).to_rfc3339(),
            "2026-05-04T00:00:00+00:00"
        );
    }

    #[test]
    fn the_cache_fingerprint_is_injective_across_subset_nesting() {
        let p = uuid(100);
        let a = uuid(1);
        let b = uuid(2);
        let x = uuid(10);
        let y = uuid(11);
        let from = DateTime::parse_from_rfc3339("2026-05-04T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let to = from + chrono::Duration::days(7);

        let one = cache_key(
            p,
            from,
            to,
            &[AppEnvScope {
                app_id: a,
                env: EnvFilter::Subset(vec![x, y]),
            }],
        )
        .unwrap();
        let two = cache_key(
            p,
            from,
            to,
            &[
                AppEnvScope {
                    app_id: a,
                    env: EnvFilter::Subset(vec![x]),
                },
                AppEnvScope {
                    app_id: b,
                    env: EnvFilter::Subset(vec![y]),
                },
            ],
        )
        .unwrap();
        assert_ne!(one, two, "a flattening join would collide these");
    }

    /// `All` includes `environment_id IS NULL` rows; a `Subset` over every one
    /// of the app's environments does not. They are different questions and
    /// must never share a cache entry.
    #[test]
    fn all_and_a_full_subset_are_distinct_cache_keys() {
        let p = uuid(100);
        let a = uuid(1);
        let x = uuid(10);
        let y = uuid(11);
        let from = DateTime::parse_from_rfc3339("2026-05-04T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let to = from + chrono::Duration::days(7);
        let all = cache_key(
            p,
            from,
            to,
            &[AppEnvScope {
                app_id: a,
                env: EnvFilter::All,
            }],
        )
        .unwrap();
        let subset = cache_key(
            p,
            from,
            to,
            &[AppEnvScope {
                app_id: a,
                env: EnvFilter::Subset(vec![x, y]),
            }],
        )
        .unwrap();
        assert_ne!(all, subset);
    }

    #[test]
    fn the_cache_key_is_order_independent() {
        let p = uuid(100);
        let a = uuid(1);
        let b = uuid(2);
        let from = DateTime::parse_from_rfc3339("2026-05-04T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let to = from + chrono::Duration::days(7);
        let ab = cache_key(
            p,
            from,
            to,
            &[
                AppEnvScope {
                    app_id: a,
                    env: EnvFilter::All,
                },
                AppEnvScope {
                    app_id: b,
                    env: EnvFilter::All,
                },
            ],
        )
        .unwrap();
        let ba = cache_key(
            p,
            from,
            to,
            &[
                AppEnvScope {
                    app_id: b,
                    env: EnvFilter::All,
                },
                AppEnvScope {
                    app_id: a,
                    env: EnvFilter::All,
                },
            ],
        )
        .unwrap();
        assert_eq!(ab, ba);
    }

    /// The serve-stale decision. The `None` arm is the one worth pinning: a
    /// report cached by a build that predates `computed_at` must read as
    /// STALE (served, but refreshed) — treating unknown age as fresh would
    /// freeze pre-upgrade numbers in place for the whole TTL.
    #[test]
    fn freshness_boundary_and_unknown_age() {
        let now = Utc::now();
        assert!(is_fresh(Some(now), now), "just computed is fresh");
        assert!(
            is_fresh(
                Some(now - chrono::Duration::seconds(ACTIVE_USERS_FRESH_FOR_SECS - 1)),
                now
            ),
            "one second inside the horizon is fresh"
        );
        assert!(
            !is_fresh(
                Some(now - chrono::Duration::seconds(ACTIVE_USERS_FRESH_FOR_SECS)),
                now
            ),
            "exactly at the horizon is stale"
        );
        assert!(!is_fresh(None, now), "unknown age is stale, never fresh");
    }
}
