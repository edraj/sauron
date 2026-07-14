# User & Session Analytics + Saved Funnel Templates — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add (A) audience & session-engagement analytics — DAU trend, all-time total users, WAU/MAU/stickiness, and session-duration avg/median/trend/distribution — to the Users and Sessions screens; and (B) saved funnel templates (a shared, app-scoped, save / reuse / clone / edit / delete library) to the Funnels screen.

**Architecture:** Two independent features in one plan. **A** is read-only: new on-read SQL aggregates in `sauron-db`, two new `GET` endpoints in `sauron-api` (`/users/summary`, `/sessions/summary`), and Svelte analytics headers reusing the existing StatTiles/DateRange/chart kit. **B** adds a `saved_funnels` table + migration, a new `funnel:write` RBAC permission, CRUD endpoints on the `funnels` collection (compute stays at singular `funnel`), and a saved-funnels panel in the builder. The two features touch mostly disjoint code and are sequenced A → B.

**Tech Stack:** Rust (axum 0.8, diesel-async/deadpool, Postgres), Svelte 5 (runes) + TypeScript + axios, Vite/vitest, Docker Compose.

## Global Constraints

- **Definitions (feature A), copied verbatim from the spec:** User = a `distinct_id` in `event_users`. Active on day D = a `distinct_id` with an analytics event **or** error on D (`COUNT(DISTINCT distinct_id)` over the `analytics_events ∪ error_events` union, `distinct_id` non-null and non-empty). DAU/WAU/MAU = distinct active users in a **rolling 1 / 7 / 30-day** window anchored at `now()`. Stickiness = `DAU / MAU`. Total users = `COUNT(*) FROM event_users` (all-time, no date filter). Session duration = `last_event_at − started_at` in **milliseconds**. Range (`since_days`) is clamped **1..365** and scopes the series, range tiles, avg/median, trend, and histogram — **not** total users / WAU / MAU.
- **No DB/handler integration-test harness exists.** The only backend tests are pure unit tests (`like_contains`, `rbac`, `jwt`, `password`, `filter`). Therefore: unit-test extracted **pure** logic (series merge, histogram fill, stickiness, funnel validation, RBAC), and verify SQL/handlers/UI **end-to-end via docker compose** (Tasks A12, B7). Do **not** invent a DB test harness.
- **Repo DTO structs** derive `#[derive(Debug, QueryableByName, serde::Serialize)]`; SQL bindings use `diesel::sql_types::{BigInt, Double, Jsonb, Nullable, Text, Timestamptz, Uuid as SqlUuid}` (already imported in `repo.rs`).
- **Compose ports:** API `10000`, ingest `10001`, dashboard `10002`.
- **Commits:** the maintainer has disabled auto-commit. Each `git commit` step below is written per TDD convention; at execution time, **stage the changes and get the maintainer's OK** before actually committing (or batch commits as they direct).
- **RBAC:** enforce every endpoint with `authorize_app(&mut conn, auth.user_id, app_id, perm::…)`. Read = `EVENT_READ`; funnel writes = the new `FUNNEL_WRITE`.

---

# PART A — User & Session Analytics

### Task A1: `user_stats` repo query

**Files:**
- Modify: `backend/crates/sauron-db/src/repo.rs` (add near `overview_totals`, ~line 1352)

**Interfaces:**
- Produces: `pub struct UserStats { total_users, active_in_range, new_in_range, dau, wau, mau: i64, avg_session_ms, median_session_ms: f64 }` and `pub async fn user_stats(conn, app_id: Uuid, since: DateTime<Utc>) -> QueryResult<UserStats>`.

- [ ] **Step 1: Add the struct + query**

```rust
#[derive(Debug, QueryableByName, serde::Serialize)]
pub struct UserStats {
    #[diesel(sql_type = BigInt)]
    pub total_users: i64,
    #[diesel(sql_type = BigInt)]
    pub active_in_range: i64,
    #[diesel(sql_type = BigInt)]
    pub new_in_range: i64,
    #[diesel(sql_type = BigInt)]
    pub dau: i64,
    #[diesel(sql_type = BigInt)]
    pub wau: i64,
    #[diesel(sql_type = BigInt)]
    pub mau: i64,
    #[diesel(sql_type = Double)]
    pub avg_session_ms: f64,
    #[diesel(sql_type = Double)]
    pub median_session_ms: f64,
}

/// Aggregate audience stats for an app. `total_users`/`wau`/`mau` ignore `since`
/// (all-time / rolling-from-now); the rest are scoped to `since`.
pub async fn user_stats(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    since: DateTime<Utc>,
) -> QueryResult<UserStats> {
    diesel::sql_query(
        "SELECT \
           (SELECT count(*) FROM event_users WHERE app_id=$1)::bigint AS total_users, \
           (SELECT count(*) FROM event_users WHERE app_id=$1 AND last_seen>=$2)::bigint AS active_in_range, \
           (SELECT count(*) FROM event_users WHERE app_id=$1 AND first_seen>=$2)::bigint AS new_in_range, \
           (SELECT count(DISTINCT distinct_id) FROM ( \
              SELECT distinct_id FROM analytics_events WHERE app_id=$1 AND occurred_at >= now() - interval '1 day' AND distinct_id IS NOT NULL AND distinct_id <> '' \
              UNION ALL \
              SELECT distinct_id FROM error_events WHERE app_id=$1 AND occurred_at >= now() - interval '1 day' AND distinct_id IS NOT NULL AND distinct_id <> '' \
            ) d1)::bigint AS dau, \
           (SELECT count(DISTINCT distinct_id) FROM ( \
              SELECT distinct_id FROM analytics_events WHERE app_id=$1 AND occurred_at >= now() - interval '7 days' AND distinct_id IS NOT NULL AND distinct_id <> '' \
              UNION ALL \
              SELECT distinct_id FROM error_events WHERE app_id=$1 AND occurred_at >= now() - interval '7 days' AND distinct_id IS NOT NULL AND distinct_id <> '' \
            ) d7)::bigint AS wau, \
           (SELECT count(DISTINCT distinct_id) FROM ( \
              SELECT distinct_id FROM analytics_events WHERE app_id=$1 AND occurred_at >= now() - interval '30 days' AND distinct_id IS NOT NULL AND distinct_id <> '' \
              UNION ALL \
              SELECT distinct_id FROM error_events WHERE app_id=$1 AND occurred_at >= now() - interval '30 days' AND distinct_id IS NOT NULL AND distinct_id <> '' \
            ) d30)::bigint AS mau, \
           COALESCE((SELECT avg(EXTRACT(EPOCH FROM (last_event_at - started_at)) * 1000) \
                     FROM sessions WHERE app_id=$1 AND last_event_at>=$2), 0)::double precision AS avg_session_ms, \
           COALESCE((SELECT percentile_cont(0.5) WITHIN GROUP (ORDER BY EXTRACT(EPOCH FROM (last_event_at - started_at)) * 1000) \
                     FROM sessions WHERE app_id=$1 AND last_event_at>=$2), 0)::double precision AS median_session_ms",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Timestamptz, _>(since)
    .get_result(conn)
    .await
}
```

- [ ] **Step 2: Compile**

Run: `cd backend && cargo build -p sauron-db`
Expected: builds clean (no errors).

- [ ] **Step 3: Commit**

```bash
git add backend/crates/sauron-db/src/repo.rs
git commit -m "feat(db): user_stats aggregate query (total/active/new/DAU/WAU/MAU/session avg+median)"
```

---

### Task A2: `active_user_series` + pure `merge_user_series`

**Files:**
- Modify: `backend/crates/sauron-db/src/repo.rs`

**Interfaces:**
- Consumes: `SeriesPoint { bucket: DateTime<Utc>, count: i64 }` (existing, ~line 872).
- Produces: `pub struct UserSeriesPoint { bucket: DateTime<Utc>, active: i64, new_users: i64 }`; `pub fn merge_user_series(active: Vec<SeriesPoint>, new: Vec<SeriesPoint>) -> Vec<UserSeriesPoint>`; `pub async fn active_user_series(conn, app_id, since) -> QueryResult<Vec<UserSeriesPoint>>`.

- [ ] **Step 1: Write the failing test for the pure merge**

Add at the bottom of `repo.rs`:

```rust
#[cfg(test)]
mod user_series_tests {
    use super::{merge_user_series, SeriesPoint, UserSeriesPoint};
    use chrono::{TimeZone, Utc};

    fn pt(day: u32, count: i64) -> SeriesPoint {
        SeriesPoint { bucket: Utc.with_ymd_and_hms(2026, 7, day, 0, 0, 0).unwrap(), count }
    }

    #[test]
    fn merges_active_and_new_by_day_zero_filling() {
        let active = vec![pt(1, 10), pt(2, 8)];
        let new = vec![pt(2, 3), pt(3, 5)]; // day 1 has no new; day 3 has no active
        let out = merge_user_series(active, new);
        let got: Vec<(u32, i64, i64)> = out
            .iter()
            .map(|p| (p.bucket.format("%d").to_string().parse().unwrap(), p.active, p.new_users))
            .collect();
        assert_eq!(got, vec![(1, 10, 0), (2, 8, 3), (3, 0, 5)]);
    }

    #[test]
    fn empty_inputs_yield_empty() {
        assert!(merge_user_series(vec![], vec![]).is_empty());
    }
}
```

- [ ] **Step 2: Run it — expect failure (unresolved names)**

Run: `cd backend && cargo test -p sauron-db user_series_tests`
Expected: FAIL to compile — `merge_user_series` / `UserSeriesPoint` not found.

- [ ] **Step 3: Implement the struct, merge, and query**

```rust
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct UserSeriesPoint {
    pub bucket: DateTime<Utc>,
    pub active: i64,
    pub new_users: i64,
}

/// Merge per-day active + per-day new counts into one sorted series, 0-filling
/// days present in only one input. Pure — unit-tested.
pub fn merge_user_series(active: Vec<SeriesPoint>, new: Vec<SeriesPoint>) -> Vec<UserSeriesPoint> {
    use std::collections::BTreeMap;
    let mut map: BTreeMap<DateTime<Utc>, (i64, i64)> = BTreeMap::new();
    for p in active {
        map.entry(p.bucket).or_default().0 = p.count;
    }
    for p in new {
        map.entry(p.bucket).or_default().1 = p.count;
    }
    map.into_iter()
        .map(|(bucket, (active, new_users))| UserSeriesPoint { bucket, active, new_users })
        .collect()
}

/// Per-day distinct active users (analytics ∪ errors) and per-day new users,
/// merged. Both scoped to `since`.
pub async fn active_user_series(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    since: DateTime<Utc>,
) -> QueryResult<Vec<UserSeriesPoint>> {
    let active: Vec<SeriesPoint> = diesel::sql_query(
        "SELECT date_trunc('day', occurred_at) AS bucket, count(DISTINCT distinct_id)::bigint AS count \
         FROM ( \
            SELECT occurred_at, distinct_id FROM analytics_events \
              WHERE app_id=$1 AND occurred_at>=$2 AND distinct_id IS NOT NULL AND distinct_id <> '' \
            UNION ALL \
            SELECT occurred_at, distinct_id FROM error_events \
              WHERE app_id=$1 AND occurred_at>=$2 AND distinct_id IS NOT NULL AND distinct_id <> '' \
         ) u \
         GROUP BY bucket ORDER BY bucket",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Timestamptz, _>(since)
    .get_results(conn)
    .await?;

    let new: Vec<SeriesPoint> = diesel::sql_query(
        "SELECT date_trunc('day', first_seen) AS bucket, count(*)::bigint AS count \
         FROM event_users WHERE app_id=$1 AND first_seen>=$2 \
         GROUP BY bucket ORDER BY bucket",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Timestamptz, _>(since)
    .get_results(conn)
    .await?;

    Ok(merge_user_series(active, new))
}
```

- [ ] **Step 4: Run the test — expect pass**

Run: `cd backend && cargo test -p sauron-db user_series_tests`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add backend/crates/sauron-db/src/repo.rs
git commit -m "feat(db): active_user_series (per-day DAU + new users) with tested merge"
```

---

### Task A3: session stats, duration series, and histogram (+ pure `order_histogram`)

**Files:**
- Modify: `backend/crates/sauron-db/src/repo.rs`

**Interfaces:**
- Produces: `SessionStats { sessions, crashed: i64, avg_session_ms, median_session_ms: f64 }`, `SeriesAvgPoint { bucket: DateTime<Utc>, avg_ms: f64 }`, `HistoBucket { bucket: String, count: i64 }`; `session_stats`, `session_duration_series`, `session_duration_histogram`; pure `order_histogram(rows: Vec<HistoBucket>) -> Vec<HistoBucket>`.

- [ ] **Step 1: Write the failing test for `order_histogram`**

Append to `repo.rs`:

```rust
#[cfg(test)]
mod histogram_tests {
    use super::{order_histogram, HistoBucket, DURATION_BUCKETS};

    fn b(bucket: &str, count: i64) -> HistoBucket {
        HistoBucket { bucket: bucket.to_string(), count }
    }

    #[test]
    fn fills_missing_buckets_in_fixed_order() {
        let rows = vec![b("30m+", 2), b("<10s", 5)];
        let out = order_histogram(rows);
        let got: Vec<(&str, i64)> = out.iter().map(|h| (h.bucket.as_str(), h.count)).collect();
        assert_eq!(
            got,
            vec![("<10s", 5), ("10-60s", 0), ("1-5m", 0), ("5-30m", 0), ("30m+", 2)]
        );
        assert_eq!(out.len(), DURATION_BUCKETS.len());
    }
}
```

- [ ] **Step 2: Run it — expect failure**

Run: `cd backend && cargo test -p sauron-db histogram_tests`
Expected: FAIL — `order_histogram` / `HistoBucket` / `DURATION_BUCKETS` not found.

- [ ] **Step 3: Implement the structs, `order_histogram`, and the three queries**

```rust
#[derive(Debug, QueryableByName, serde::Serialize)]
pub struct SessionStats {
    #[diesel(sql_type = BigInt)]
    pub sessions: i64,
    #[diesel(sql_type = BigInt)]
    pub crashed: i64,
    #[diesel(sql_type = Double)]
    pub avg_session_ms: f64,
    #[diesel(sql_type = Double)]
    pub median_session_ms: f64,
}

#[derive(Debug, QueryableByName, serde::Serialize)]
pub struct SeriesAvgPoint {
    #[diesel(sql_type = Timestamptz)]
    pub bucket: DateTime<Utc>,
    #[diesel(sql_type = Double)]
    pub avg_ms: f64,
}

#[derive(Debug, QueryableByName, serde::Serialize)]
pub struct HistoBucket {
    #[diesel(sql_type = Text)]
    pub bucket: String,
    #[diesel(sql_type = BigInt)]
    pub count: i64,
}

/// Duration-histogram bucket labels, in display order.
pub const DURATION_BUCKETS: [&str; 5] = ["<10s", "10-60s", "1-5m", "5-30m", "30m+"];

/// Reorder DB histogram rows into the fixed bucket order, 0-filling gaps. Pure.
pub fn order_histogram(rows: Vec<HistoBucket>) -> Vec<HistoBucket> {
    DURATION_BUCKETS
        .iter()
        .map(|label| {
            let count = rows.iter().find(|r| r.bucket == *label).map(|r| r.count).unwrap_or(0);
            HistoBucket { bucket: (*label).to_string(), count }
        })
        .collect()
}

pub async fn session_stats(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    since: DateTime<Utc>,
) -> QueryResult<SessionStats> {
    diesel::sql_query(
        "SELECT \
           (SELECT count(*) FROM sessions WHERE app_id=$1 AND last_event_at>=$2)::bigint AS sessions, \
           (SELECT count(*) FROM sessions WHERE app_id=$1 AND last_event_at>=$2 AND errors_count>0)::bigint AS crashed, \
           COALESCE((SELECT avg(EXTRACT(EPOCH FROM (last_event_at - started_at)) * 1000) \
                     FROM sessions WHERE app_id=$1 AND last_event_at>=$2), 0)::double precision AS avg_session_ms, \
           COALESCE((SELECT percentile_cont(0.5) WITHIN GROUP (ORDER BY EXTRACT(EPOCH FROM (last_event_at - started_at)) * 1000) \
                     FROM sessions WHERE app_id=$1 AND last_event_at>=$2), 0)::double precision AS median_session_ms",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Timestamptz, _>(since)
    .get_result(conn)
    .await
}

pub async fn session_duration_series(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    since: DateTime<Utc>,
) -> QueryResult<Vec<SeriesAvgPoint>> {
    diesel::sql_query(
        "SELECT date_trunc('day', started_at) AS bucket, \
                COALESCE(avg(EXTRACT(EPOCH FROM (last_event_at - started_at)) * 1000), 0)::double precision AS avg_ms \
         FROM sessions WHERE app_id=$1 AND started_at>=$2 \
         GROUP BY bucket ORDER BY bucket",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Timestamptz, _>(since)
    .get_results(conn)
    .await
}

pub async fn session_duration_histogram(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    since: DateTime<Utc>,
) -> QueryResult<Vec<HistoBucket>> {
    let rows: Vec<HistoBucket> = diesel::sql_query(
        "SELECT bucket, count(*)::bigint AS count FROM ( \
           SELECT CASE \
             WHEN d < 10000  THEN '<10s' \
             WHEN d < 60000  THEN '10-60s' \
             WHEN d < 300000 THEN '1-5m' \
             WHEN d < 1800000 THEN '5-30m' \
             ELSE '30m+' END AS bucket \
           FROM (SELECT EXTRACT(EPOCH FROM (last_event_at - started_at)) * 1000 AS d \
                 FROM sessions WHERE app_id=$1 AND last_event_at>=$2) s \
         ) b GROUP BY bucket",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Timestamptz, _>(since)
    .get_results(conn)
    .await?;
    Ok(order_histogram(rows))
}
```

- [ ] **Step 4: Run the test — expect pass**

Run: `cd backend && cargo test -p sauron-db histogram_tests` → PASS. Then `cargo build -p sauron-db` → clean.

- [ ] **Step 5: Commit**

```bash
git add backend/crates/sauron-db/src/repo.rs
git commit -m "feat(db): session_stats + duration series + tested duration histogram"
```

---

### Task A4: `/users/summary` endpoint (+ pure `stickiness`)

**Files:**
- Modify: `backend/bins/sauron-api/src/routes/analytics.rs`
- Modify: `backend/bins/sauron-api/src/main.rs` (route registration, ~line 167 near `overview`)

**Interfaces:**
- Consumes: `repo::user_stats`, `repo::active_user_series`, `RangeQuery`, `authorize_app`, `perm::EVENT_READ`, `db(&state)`.
- Produces: `GET /v1/apps/{app_id}/users/summary?since_days=N` → `Json<UsersAnalytics>`.

- [ ] **Step 1: Write the failing test for `stickiness`**

At the bottom of `analytics.rs`:

```rust
#[cfg(test)]
mod stickiness_tests {
    use super::stickiness;

    #[test]
    fn ratio_of_dau_to_mau() {
        assert!((stickiness(5, 20) - 0.25).abs() < 1e-9);
    }

    #[test]
    fn zero_mau_is_zero_not_nan() {
        assert_eq!(stickiness(3, 0), 0.0);
    }
}
```

- [ ] **Step 2: Run it — expect failure**

Run: `cd backend && cargo test -p sauron-api stickiness_tests`
Expected: FAIL — `stickiness` not found.

- [ ] **Step 3: Implement helper, DTO, and handler**

Add to `analytics.rs` (reuse existing `use` items; add `use serde::Serialize;` if not present):

```rust
/// DAU / MAU, guarding division by zero. Pure.
pub fn stickiness(dau: i64, mau: i64) -> f64 {
    if mau > 0 {
        dau as f64 / mau as f64
    } else {
        0.0
    }
}

#[derive(Serialize)]
pub struct UsersAnalytics {
    pub stats: repo::UserStats,
    pub stickiness: f64,
    pub series: Vec<repo::UserSeriesPoint>,
}

pub async fn users_summary(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<RangeQuery>,
) -> Result<Json<UsersAnalytics>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::EVENT_READ).await?;
    let since = Utc::now() - Duration::days(q.since_days.clamp(1, 365));

    let stats = repo::user_stats(&mut conn, app_id, since).await?;
    let series = repo::active_user_series(&mut conn, app_id, since).await?;
    let stickiness = stickiness(stats.dau, stats.mau);

    Ok(Json(UsersAnalytics { stats, stickiness, series }))
}
```

Register the route in `main.rs` next to the `overview` route:

```rust
.route(
    "/v1/apps/{app_id}/users/summary",
    get(routes::analytics::users_summary),
)
```

- [ ] **Step 4: Run tests + build**

Run: `cd backend && cargo test -p sauron-api stickiness_tests` → PASS. Then `cargo build` → clean.

- [ ] **Step 5: Commit**

```bash
git add backend/bins/sauron-api/src/routes/analytics.rs backend/bins/sauron-api/src/main.rs
git commit -m "feat(api): GET /users/summary — audience stats + DAU series + stickiness"
```

---

### Task A5: `/sessions/summary` endpoint

**Files:**
- Modify: `backend/bins/sauron-api/src/routes/analytics.rs`
- Modify: `backend/bins/sauron-api/src/main.rs` (near the `/sessions` routes, ~line 174)

**Interfaces:**
- Consumes: `repo::session_stats`, `repo::session_duration_series`, `repo::session_duration_histogram`.
- Produces: `GET /v1/apps/{app_id}/sessions/summary?since_days=N` → `Json<SessionsAnalytics>`.

- [ ] **Step 1: Implement DTO + handler**

```rust
#[derive(Serialize)]
pub struct SessionsAnalytics {
    pub stats: repo::SessionStats,
    pub duration_series: Vec<repo::SeriesAvgPoint>,
    pub duration_histogram: Vec<repo::HistoBucket>,
}

pub async fn sessions_summary(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Query(q): Query<RangeQuery>,
) -> Result<Json<SessionsAnalytics>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::EVENT_READ).await?;
    let since = Utc::now() - Duration::days(q.since_days.clamp(1, 365));

    let stats = repo::session_stats(&mut conn, app_id, since).await?;
    let duration_series = repo::session_duration_series(&mut conn, app_id, since).await?;
    let duration_histogram = repo::session_duration_histogram(&mut conn, app_id, since).await?;

    Ok(Json(SessionsAnalytics { stats, duration_series, duration_histogram }))
}
```

Register in `main.rs`:

```rust
.route(
    "/v1/apps/{app_id}/sessions/summary",
    get(routes::analytics::sessions_summary),
)
```

- [ ] **Step 2: Build**

Run: `cd backend && cargo build`
Expected: builds clean.

- [ ] **Step 3: Commit**

```bash
git add backend/bins/sauron-api/src/routes/analytics.rs backend/bins/sauron-api/src/main.rs
git commit -m "feat(api): GET /sessions/summary — session stats + duration trend + histogram"
```

---

### Task A6: Frontend models + API clients

**Files:**
- Modify: `dashboard/src/lib/models/index.ts`
- Create: `dashboard/src/lib/api/users.ts`
- Create: `dashboard/src/lib/api/sessions.ts`

**Interfaces:**
- Produces: TS types `UserStats/UserSeriesPoint/UsersAnalytics/SessionStats/SeriesAvgPoint/HistoBucket/SessionsAnalytics`; `getUserAnalytics`, `getSessionAnalytics`.

- [ ] **Step 1: Add model types**

Append to `dashboard/src/lib/models/index.ts`:

```ts
// ---------------------------------------------------------------------------
// Audience & session analytics
// ---------------------------------------------------------------------------

export interface UserStats {
  total_users: number;
  active_in_range: number;
  new_in_range: number;
  dau: number;
  wau: number;
  mau: number;
  avg_session_ms: number;
  median_session_ms: number;
}

export interface UserSeriesPoint {
  bucket: string;
  active: number;
  new_users: number;
}

export interface UsersAnalytics {
  stats: UserStats;
  stickiness: number;
  series: UserSeriesPoint[];
}

export interface SessionStats {
  sessions: number;
  crashed: number;
  avg_session_ms: number;
  median_session_ms: number;
}

export interface SeriesAvgPoint {
  bucket: string;
  avg_ms: number;
}

export interface HistoBucket {
  bucket: string;
  count: number;
}

export interface SessionsAnalytics {
  stats: SessionStats;
  duration_series: SeriesAvgPoint[];
  duration_histogram: HistoBucket[];
}
```

- [ ] **Step 2: Create `api/users.ts`**

```ts
import { api } from './client';
import type { UsersAnalytics } from '../models';

export async function getUserAnalytics(
  appId: string,
  sinceDays = 30,
): Promise<UsersAnalytics> {
  const { data } = await api.get<UsersAnalytics>(`/v1/apps/${appId}/users/summary`, {
    params: { since_days: sinceDays },
  });
  return data;
}
```

- [ ] **Step 3: Create `api/sessions.ts`**

```ts
import { api } from './client';
import type { SessionsAnalytics } from '../models';

export async function getSessionAnalytics(
  appId: string,
  sinceDays = 30,
): Promise<SessionsAnalytics> {
  const { data } = await api.get<SessionsAnalytics>(`/v1/apps/${appId}/sessions/summary`, {
    params: { since_days: sinceDays },
  });
  return data;
}
```

- [ ] **Step 4: Typecheck**

Run: `cd dashboard && npx svelte-check --tsconfig ./tsconfig.json`
Expected: 0 errors, 0 warnings.

- [ ] **Step 5: Commit**

```bash
git add dashboard/src/lib/models/index.ts dashboard/src/lib/api/users.ts dashboard/src/lib/api/sessions.ts
git commit -m "feat(dashboard): analytics models + users/sessions summary API clients"
```

---

### Task A7: `TimeSeriesChart` — optional `format` + `showTotal` props

**Files:**
- Modify: `dashboard/src/lib/components/TimeSeriesChart.svelte`

**Interfaces:**
- Produces: `TimeSeriesChart` accepts `format?: (n: number) => string` (default `(n) => n.toLocaleString()`) and `showTotal?: boolean` (default `true`). Backward-compatible — existing usages unaffected.

- [ ] **Step 1: Extend Props + defaults**

Replace the `interface Props { … }` and the destructuring `let { … } = $props();` block with:

```svelte
  interface Props {
    data: SeriesPoint[];
    height?: number;
    color?: string;
    emptyLabel?: string;
    format?: (n: number) => string;
    showTotal?: boolean;
  }

  let {
    data,
    height = 160,
    color = 'var(--primary)',
    emptyLabel = 'No data in this range',
    format = (n: number) => n.toLocaleString(),
    showTotal = true,
  }: Props = $props();
```

- [ ] **Step 2: Use `format` in tooltip/title and gate the total**

Replace the `title={…}` on `.col`, the `.tip` content, and the `.total` span:

```svelte
        <div class="col" title={`${formatDateTime(point.bucket)} · ${format(point.count)}`}>
          <div class="bar" style="height:{barHeight(point.count)}%">
            <span class="tip">{format(point.count)} · {label(point.bucket)}</span>
          </div>
        </div>
```

```svelte
    <div class="axis">
      <span>{label(data[0].bucket)}</span>
      {#if showTotal}<span class="total">{total.toLocaleString()} total</span>{/if}
      <span>{label(data[data.length - 1].bucket)}</span>
    </div>
```

- [ ] **Step 3: Typecheck**

Run: `cd dashboard && npx svelte-check --tsconfig ./tsconfig.json`
Expected: 0 errors (existing callers still compile — new props optional).

- [ ] **Step 4: Commit**

```bash
git add dashboard/src/lib/components/TimeSeriesChart.svelte
git commit -m "feat(dashboard): TimeSeriesChart optional value formatter + showTotal"
```

---

### Task A8: `UserActivityChart` component (active bars + new-users line)

**Files:**
- Create: `dashboard/src/lib/components/UserActivityChart.svelte`

**Interfaces:**
- Consumes: `UserSeriesPoint[]`. Produces: `<UserActivityChart data={series} />`.

- [ ] **Step 1: Create the component**

```svelte
<script lang="ts">
  import type { UserSeriesPoint } from '../models';
  import { formatDateTime } from '../utils/format';

  interface Props {
    data: UserSeriesPoint[];
    height?: number;
    emptyLabel?: string;
  }

  let { data, height = 180, emptyLabel = 'No user activity in this range' }: Props = $props();

  const maxActive = $derived(data.length ? Math.max(...data.map((d) => d.active), 1) : 1);
  const maxNew = $derived(data.length ? Math.max(...data.map((d) => d.new_users), 1) : 1);

  function barHeight(v: number): number {
    if (maxActive <= 0) return 0;
    return v === 0 ? 2 : Math.max(4, (v / maxActive) * 100);
  }

  function label(bucket: string): string {
    const d = new Date(bucket);
    if (Number.isNaN(d.getTime())) return bucket;
    return d.toLocaleDateString(undefined, { month: 'short', day: 'numeric' });
  }

  // New-users overlay as a polyline in a 0..100 viewBox (y inverted).
  const linePoints = $derived(
    data
      .map((d, i) => {
        const x = data.length === 1 ? 50 : (i / (data.length - 1)) * 100;
        const y = 100 - (maxNew <= 0 ? 0 : (d.new_users / maxNew) * 100);
        return `${x.toFixed(2)},${y.toFixed(2)}`;
      })
      .join(' '),
  );
</script>

{#if data.length === 0}
  <div class="chart-empty" style="height:{height}px">{emptyLabel}</div>
{:else}
  <div class="chart">
    <div class="plot" style="height:{height}px">
      <div class="bars">
        {#each data as point (point.bucket)}
          <div
            class="col"
            title={`${formatDateTime(point.bucket)} · ${point.active} active · ${point.new_users} new`}
          >
            <div class="bar" style="height:{barHeight(point.active)}%">
              <span class="tip">{point.active} active · {point.new_users} new<br />{label(point.bucket)}</span>
            </div>
          </div>
        {/each}
      </div>
      {#if maxNew > 0}
        <svg class="overlay" viewBox="0 0 100 100" preserveAspectRatio="none" aria-hidden="true">
          <polyline points={linePoints} fill="none" stroke="var(--info)" stroke-width="1.5" vector-effect="non-scaling-stroke" />
        </svg>
      {/if}
    </div>
    <div class="axis">
      <span>{label(data[0].bucket)}</span>
      <span class="legend"><i class="k a"></i> active <i class="k n"></i> new</span>
      <span>{label(data[data.length - 1].bucket)}</span>
    </div>
  </div>
{/if}

<style>
  .chart { display: flex; flex-direction: column; gap: 8px; }
  .plot { position: relative; }
  .bars {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: flex-end;
    gap: 3px;
    padding: 4px 2px 0;
    border-bottom: 1px solid var(--border);
  }
  .col { flex: 1; min-width: 3px; height: 100%; display: flex; align-items: flex-end; justify-content: center; }
  .bar {
    position: relative;
    width: 100%;
    max-width: 42px;
    border-radius: 3px 3px 0 0;
    background: linear-gradient(to top, color-mix(in srgb, var(--primary) 55%, transparent), var(--primary));
    transition: filter 0.12s ease;
  }
  .col:hover .bar { filter: brightness(1.18); }
  .overlay { position: absolute; inset: 0; width: 100%; height: 100%; pointer-events: none; }
  .tip {
    position: absolute;
    bottom: calc(100% + 6px);
    left: 50%;
    transform: translateX(-50%);
    padding: 4px 8px;
    background: var(--surface-3);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-sm);
    font-size: 11px;
    white-space: nowrap;
    color: var(--text);
    opacity: 0;
    pointer-events: none;
    transition: opacity 0.12s ease;
    z-index: 2;
    box-shadow: var(--shadow);
  }
  .col:hover .tip { opacity: 1; }
  .axis { display: flex; justify-content: space-between; align-items: center; font-size: 11px; color: var(--text-faint); }
  .legend { display: inline-flex; align-items: center; gap: 6px; color: var(--text-muted); }
  .k { display: inline-block; width: 9px; height: 9px; border-radius: 2px; vertical-align: middle; }
  .k.a { background: var(--primary); }
  .k.n { background: var(--info); }
  .chart-empty {
    display: grid;
    place-items: center;
    color: var(--text-faint);
    font-size: 13px;
    border: 1px dashed var(--border);
    border-radius: var(--radius);
  }
</style>
```

- [ ] **Step 2: Typecheck**

Run: `cd dashboard && npx svelte-check --tsconfig ./tsconfig.json`
Expected: 0 errors.

- [ ] **Step 3: Commit**

```bash
git add dashboard/src/lib/components/UserActivityChart.svelte
git commit -m "feat(dashboard): UserActivityChart — DAU bars with new-users overlay"
```

---

### Task A9: `DurationHistogram` component

**Files:**
- Create: `dashboard/src/lib/components/DurationHistogram.svelte`

**Interfaces:**
- Consumes: `HistoBucket[]`. Produces: `<DurationHistogram data={hist} />`.

- [ ] **Step 1: Create the component**

```svelte
<script lang="ts">
  import type { HistoBucket } from '../models';

  interface Props {
    data: HistoBucket[];
    height?: number;
    emptyLabel?: string;
  }

  let { data, height = 160, emptyLabel = 'No sessions in this range' }: Props = $props();

  const max = $derived(data.length ? Math.max(...data.map((d) => d.count), 1) : 1);
  const total = $derived(data.reduce((sum, d) => sum + d.count, 0));

  function barHeight(count: number): number {
    if (max <= 0) return 0;
    return count === 0 ? 2 : Math.max(4, (count / max) * 100);
  }
</script>

{#if total === 0}
  <div class="chart-empty" style="height:{height}px">{emptyLabel}</div>
{:else}
  <div class="chart">
    <div class="plot" style="height:{height}px">
      {#each data as b (b.bucket)}
        <div class="col" title={`${b.bucket}: ${b.count.toLocaleString()} sessions`}>
          <div class="bar" style="height:{barHeight(b.count)}%">
            <span class="cnt">{b.count.toLocaleString()}</span>
          </div>
          <span class="lbl">{b.bucket}</span>
        </div>
      {/each}
    </div>
  </div>
{/if}

<style>
  .chart { display: flex; flex-direction: column; gap: 8px; }
  .plot { display: flex; align-items: flex-end; gap: 10px; padding: 16px 4px 0; }
  .col { flex: 1; height: 100%; display: flex; flex-direction: column; align-items: center; justify-content: flex-end; gap: 6px; }
  .bar {
    position: relative;
    width: 100%;
    max-width: 64px;
    border-radius: 4px 4px 0 0;
    background: linear-gradient(to top, color-mix(in srgb, var(--primary) 55%, transparent), var(--primary));
  }
  .cnt {
    position: absolute;
    bottom: calc(100% + 3px);
    left: 50%;
    transform: translateX(-50%);
    font-size: 11px;
    color: var(--text-muted);
    white-space: nowrap;
  }
  .lbl { font-size: 11.5px; color: var(--text-faint); white-space: nowrap; }
  .chart-empty {
    display: grid;
    place-items: center;
    color: var(--text-faint);
    font-size: 13px;
    border: 1px dashed var(--border);
    border-radius: var(--radius);
  }
</style>
```

- [ ] **Step 2: Typecheck + commit**

Run: `cd dashboard && npx svelte-check --tsconfig ./tsconfig.json` → 0 errors.

```bash
git add dashboard/src/lib/components/DurationHistogram.svelte
git commit -m "feat(dashboard): DurationHistogram component"
```

---

### Task A10: Users screen analytics header

**Files:**
- Modify: `dashboard/src/pages/UsersExplorer.svelte`

**Interfaces:**
- Consumes: `getUserAnalytics`, `UsersAnalytics`, `StatTiles`, `StatTile`, `DateRange`, `UserActivityChart`, `Card`, `Spinner`, `formatDuration`, `formatPercent`, `compactNumber`.

- [ ] **Step 1: Add imports**

In the `<script>` block of `UsersExplorer.svelte`, add:

```ts
  import StatTiles from '../lib/components/StatTiles.svelte';
  import StatTile from '../lib/components/StatTile.svelte';
  import DateRange from '../lib/components/DateRange.svelte';
  import UserActivityChart from '../lib/components/UserActivityChart.svelte';
  import { getUserAnalytics } from '../lib/api/users';
  import { compactNumber, formatDuration, formatPercent } from '../lib/utils/format';
  import type { UsersAnalytics } from '../lib/models';
```

(If `relativeTime, formatDateTime, initials, hueFromString` are already imported from `../lib/utils/format`, merge `compactNumber, formatDuration, formatPercent` into that existing import instead of duplicating it.)

- [ ] **Step 2: Add analytics state + loader**

After the existing `let debounce…` declaration, add:

```ts
  let sinceDays = $state(30);
  let analytics = $state<UsersAnalytics | null>(null);
  let analyticsError = $state<string | null>(null);

  async function loadAnalytics(appId: string, days: number) {
    analyticsError = null;
    try {
      analytics = await getUserAnalytics(appId, days);
    } catch (err) {
      analyticsError = errorMessage(err);
      analytics = null;
    }
  }

  $effect(() => {
    const aid = sessionStore.currentAppId;
    const days = sinceDays;
    if (aid) void loadAnalytics(aid, days);
  });
```

- [ ] **Step 3: Add the header markup above the existing people-table Card**

Immediately inside the page's top container (above the search/table Card), insert:

```svelte
  <div class="analytics-head">
    <h2 class="section-title">Audience</h2>
    <DateRange value={sinceDays} onchange={(d) => (sinceDays = d)} />
  </div>

  {#if analytics}
    <StatTiles min={150}>
      <StatTile label="Total users" value={compactNumber(analytics.stats.total_users)} tone="primary" sub="all time" />
      <StatTile label="Active" value={compactNumber(analytics.stats.active_in_range)} sub={`last ${sinceDays}d`} />
      <StatTile label="New" value={compactNumber(analytics.stats.new_in_range)} sub={`last ${sinceDays}d`} />
      <StatTile label="WAU" value={compactNumber(analytics.stats.wau)} sub="7-day" />
      <StatTile label="MAU" value={compactNumber(analytics.stats.mau)} sub="30-day" />
      <StatTile label="Stickiness" value={formatPercent(analytics.stickiness)} sub="DAU / MAU" />
      <StatTile label="Avg session" value={formatDuration(analytics.stats.avg_session_ms)} />
      <StatTile label="Median session" value={formatDuration(analytics.stats.median_session_ms)} />
    </StatTiles>

    <Card title="Active users per day">
      <UserActivityChart data={analytics.series} />
    </Card>
  {:else if analyticsError}
    <Card><p class="muted">{analyticsError}</p></Card>
  {/if}
```

> Note: `formatDuration(ms)` takes **milliseconds** (verified in `format.ts` — `const s = ms / 1000`), so pass `avg_session_ms` / `median_session_ms` directly, no conversion.

- [ ] **Step 4: Typecheck + build**

Run: `cd dashboard && npx svelte-check --tsconfig ./tsconfig.json && npx vite build`
Expected: 0 errors; build succeeds.

- [ ] **Step 5: Commit**

```bash
git add dashboard/src/pages/UsersExplorer.svelte
git commit -m "feat(dashboard): audience analytics header on Users screen"
```

---

### Task A11: Sessions screen analytics header

**Files:**
- Modify: `dashboard/src/pages/SessionsList.svelte`

**Interfaces:**
- Consumes: `getSessionAnalytics`, `SessionsAnalytics`, `StatTiles`, `StatTile`, `DateRange`, `TimeSeriesChart`, `DurationHistogram`, `Card`, `formatDuration`, `compactNumber`.

- [ ] **Step 1: Add imports**

```ts
  import StatTiles from '../lib/components/StatTiles.svelte';
  import StatTile from '../lib/components/StatTile.svelte';
  import DateRange from '../lib/components/DateRange.svelte';
  import TimeSeriesChart from '../lib/components/TimeSeriesChart.svelte';
  import DurationHistogram from '../lib/components/DurationHistogram.svelte';
  import { getSessionAnalytics } from '../lib/api/sessions';
  import { compactNumber } from '../lib/utils/format';
  import type { SessionsAnalytics, SeriesPoint } from '../lib/models';
```

(`formatDuration` is already imported in this file — do not re-import it. Merge `compactNumber` into the existing `../lib/utils/format` import.)

- [ ] **Step 2: Add state, loader, and a derived duration-series adapter**

```ts
  let sinceDays = $state(30);
  let analytics = $state<SessionsAnalytics | null>(null);
  let analyticsError = $state<string | null>(null);

  // TimeSeriesChart consumes {bucket, count}; map avg_ms → count and format as duration.
  const durationSeries = $derived<SeriesPoint[]>(
    (analytics?.duration_series ?? []).map((p) => ({ bucket: p.bucket, count: p.avg_ms })),
  );

  async function loadAnalytics(appId: string, days: number) {
    analyticsError = null;
    try {
      analytics = await getSessionAnalytics(appId, days);
    } catch (err) {
      analyticsError = errorMessage(err);
      analytics = null;
    }
  }

  $effect(() => {
    const aid = sessionStore.currentAppId;
    const days = sinceDays;
    if (aid) void loadAnalytics(aid, days);
  });
```

(If `errorMessage` isn't already imported in this file, add `import { errorMessage } from '../lib/api/client';`.)

- [ ] **Step 3: Add header markup above the sessions table**

```svelte
  <div class="analytics-head">
    <h2 class="section-title">Session engagement</h2>
    <DateRange value={sinceDays} onchange={(d) => (sinceDays = d)} />
  </div>

  {#if analytics}
    <StatTiles min={160}>
      <StatTile label="Sessions" value={compactNumber(analytics.stats.sessions)} tone="primary" sub={`last ${sinceDays}d`} />
      <StatTile label="Crashed" value={compactNumber(analytics.stats.crashed)} tone={analytics.stats.crashed > 0 ? 'warning' : 'neutral'} />
      <StatTile label="Avg session" value={formatDuration(analytics.stats.avg_session_ms)} />
      <StatTile label="Median session" value={formatDuration(analytics.stats.median_session_ms)} />
    </StatTiles>

    <div class="session-charts">
      <Card title="Average session duration per day">
        <TimeSeriesChart data={durationSeries} format={formatDuration} showTotal={false} />
      </Card>
      <Card title="Session length distribution">
        <DurationHistogram data={analytics.duration_histogram} />
      </Card>
    </div>
  {:else if analyticsError}
    <Card><p class="muted">{analyticsError}</p></Card>
  {/if}
```

Add to the `<style>` block:

```css
  .session-charts { display: grid; grid-template-columns: 1fr 1fr; gap: 18px; align-items: start; margin: 16px 0; }
  @media (max-width: 900px) { .session-charts { grid-template-columns: 1fr; } }
  .analytics-head { display: flex; align-items: center; justify-content: space-between; gap: 12px; margin: 8px 0 12px; }
```

- [ ] **Step 4: Typecheck + build**

Run: `cd dashboard && npx svelte-check --tsconfig ./tsconfig.json && npx vite build`
Expected: 0 errors; build succeeds.

- [ ] **Step 5: Commit**

```bash
git add dashboard/src/pages/SessionsList.svelte
git commit -m "feat(dashboard): session-engagement analytics header on Sessions screen"
```

---

### Task A12: End-to-end verification (Part A)

**Files:** none (verification only).

- [ ] **Step 1: Bring up the stack**

Run: `docker compose up --build -d` (from repo root). Wait for migrations to apply and seeding to complete (check `docker compose logs -f sauron-api` for readiness).

- [ ] **Step 2: Obtain a token + app id, then curl the new endpoints**

```bash
# Log in with the seeded user (see .env / seed for credentials) to get an access token:
TOKEN=$(curl -s localhost:10000/v1/auth/login -H 'content-type: application/json' \
  -d '{"email":"<seeded-email>","password":"<seeded-password>"}' | jq -r .access_token)
APP=<seeded-app-uuid>   # from GET /v1/orgs or the dashboard URL
curl -s "localhost:10000/v1/apps/$APP/users/summary?since_days=30" -H "authorization: Bearer $TOKEN" | jq
curl -s "localhost:10000/v1/apps/$APP/sessions/summary?since_days=30" -H "authorization: Bearer $TOKEN" | jq
```

Expected:
- `/users/summary`: `stats.total_users` ≥ `stats.active_in_range` (all-time ≥ range); `wau` ≥ `dau`, `mau` ≥ `wau`; `series` is a non-empty array of `{bucket, active, new_users}`; `stickiness` in `[0,1]`; `avg_session_ms`/`median_session_ms` > 0.
- `/sessions/summary`: `duration_histogram` has exactly 5 buckets in order `<10s,10-60s,1-5m,5-30m,30m+`; `duration_series` non-empty; `stats.sessions` > 0.

- [ ] **Step 3: Verify the UI with the preview tools**

Start the dashboard preview (`preview_start`), navigate to `#/users` and `#/sessions`. Confirm: Users shows the Total-users tile + DAU chart with the new-users overlay; Sessions shows avg/median tiles + duration trend + a 5-bucket histogram. Toggle the DateRange and confirm the header refreshes. Capture a screenshot for the maintainer.

- [ ] **Step 4: Record result**

Note pass/fail with the actual JSON snippets in the task log. If any assertion fails, fix the offending repo query/handler and re-run.

---

# PART B — Saved Funnel Templates

### Task B1: Migration + diesel schema

**Files:**
- Create: `backend/migrations/2026-07-13-000006_saved_funnels/up.sql`
- Create: `backend/migrations/2026-07-13-000006_saved_funnels/down.sql`
- Modify: `backend/crates/sauron-db/src/schema.rs`

- [ ] **Step 1: Write `up.sql`**

```sql
-- 0006: saved_funnels — persisted, app-scoped funnel definitions (a shared team
-- library). A definition is an ordered array of event-name strings; conversion is
-- still computed on read via POST /funnel. `created_by` is display-only.
CREATE TABLE saved_funnels (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    app_id      UUID NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    description TEXT,
    steps       JSONB NOT NULL,
    created_by  UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX saved_funnels_app_updated_idx ON saved_funnels (app_id, updated_at DESC);
```

- [ ] **Step 2: Write `down.sql`**

```sql
DROP TABLE IF EXISTS saved_funnels;
```

- [ ] **Step 3: Add the diesel table**

In `backend/crates/sauron-db/src/schema.rs`, add (keep the file's alphabetical-ish grouping — place near `projects`):

```rust
diesel::table! {
    saved_funnels (id) {
        id -> Uuid,
        app_id -> Uuid,
        name -> Text,
        description -> Nullable<Text>,
        steps -> Jsonb,
        created_by -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
```

If `schema.rs` ends with a `diesel::allow_tables_to_appear_in_same_query!( … );` block, add `saved_funnels` to it.

- [ ] **Step 4: Apply + verify the migration**

Run: `docker compose up --build -d` then `docker compose logs sauron-api | grep -i migrat` (or the project's migrate command). Confirm `2026-07-13-000006_saved_funnels` applied and `cargo build -p sauron-db` is clean.

- [ ] **Step 5: Commit**

```bash
git add backend/migrations/2026-07-13-000006_saved_funnels backend/crates/sauron-db/src/schema.rs
git commit -m "feat(db): saved_funnels table + migration + diesel schema"
```

---

### Task B2: `funnel:write` RBAC permission

**Files:**
- Modify: `backend/crates/sauron-auth/src/rbac.rs`

**Interfaces:**
- Produces: `perm::FUNNEL_WRITE = "funnel:write"`, added to `perm::ALL`, `ADMIN`, `DEVELOPER` (not `VIEWER`, not `OWNER`'s explicit list — Owner uses `ALL`).

- [ ] **Step 1: Update the existing RBAC unit tests to the new expected counts (they will fail first)**

In the `#[cfg(test)] mod tests`, change the three length assertions:
- `owner_has_every_permission`: `assert_eq!(OWNER.permissions.len(), 16);` → `17`.
- `admin_is_all_except_org_manage`: `assert_eq!(ADMIN.permissions.len(), 15);` → `16`.
- `developer_can_write_issues_not_manage_members`: `assert_eq!(DEVELOPER.permissions.len(), 9);` → `10`; and add `assert!(DEVELOPER.permissions.contains(&perm::FUNNEL_WRITE));`.

Add a new test:

```rust
    #[test]
    fn viewer_cannot_write_funnels() {
        assert!(VIEWER.permissions.contains(&perm::EVENT_READ));
        assert!(!VIEWER.permissions.contains(&perm::FUNNEL_WRITE));
    }
```

- [ ] **Step 2: Run tests — expect failure**

Run: `cd backend && cargo test -p sauron-auth`
Expected: FAIL — `perm::FUNNEL_WRITE` unresolved and length assertions off.

- [ ] **Step 3: Add the permission**

In `pub mod perm`:
- Add `pub const FUNNEL_WRITE: &str = "funnel:write";` (place after `EVENT_READ`).
- Change `pub const ALL: [&str; 16]` → `[&str; 17]` and add `FUNNEL_WRITE,` to the array (right after `EVENT_READ,`).

In `ADMIN.permissions`: add `perm::FUNNEL_WRITE,` (after `perm::EVENT_READ,`).
In `DEVELOPER.permissions`: add `perm::FUNNEL_WRITE,` (after `perm::EVENT_READ,`).
Leave `VIEWER` and `OWNER` (uses `&perm::ALL`) as-is.

- [ ] **Step 4: Run tests — expect pass**

Run: `cd backend && cargo test -p sauron-auth`
Expected: PASS (all preset tests green).

- [ ] **Step 5: Commit**

```bash
git add backend/crates/sauron-auth/src/rbac.rs
git commit -m "feat(auth): funnel:write permission (Admin/Developer, not Viewer)"
```

---

### Task B3: Saved-funnels repo CRUD

**Files:**
- Modify: `backend/crates/sauron-db/src/repo.rs`

**Interfaces:**
- Produces: `SavedFunnelRow { id, app_id, name, description: Option<String>, steps: Value, created_by_name: Option<String>, created_at, updated_at }`; `list_saved_funnels`, `create_saved_funnel`, `update_saved_funnel`, `delete_saved_funnel`.

- [ ] **Step 1: Add row struct + CRUD via `sql_query` (join users for `created_by_name`)**

```rust
#[derive(Debug, QueryableByName, serde::Serialize)]
pub struct SavedFunnelRow {
    #[diesel(sql_type = SqlUuid)]
    pub id: Uuid,
    #[diesel(sql_type = SqlUuid)]
    pub app_id: Uuid,
    #[diesel(sql_type = Text)]
    pub name: String,
    #[diesel(sql_type = Nullable<Text>)]
    pub description: Option<String>,
    #[diesel(sql_type = Jsonb)]
    pub steps: Value,
    #[diesel(sql_type = Nullable<Text>)]
    pub created_by_name: Option<String>,
    #[diesel(sql_type = Timestamptz)]
    pub created_at: DateTime<Utc>,
    #[diesel(sql_type = Timestamptz)]
    pub updated_at: DateTime<Utc>,
}

const SAVED_FUNNEL_SELECT: &str = "SELECT sf.id, sf.app_id, sf.name, sf.description, sf.steps, \
    u.name AS created_by_name, sf.created_at, sf.updated_at \
    FROM saved_funnels sf LEFT JOIN users u ON u.id = sf.created_by ";

pub async fn list_saved_funnels(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
) -> QueryResult<Vec<SavedFunnelRow>> {
    diesel::sql_query(format!(
        "{SAVED_FUNNEL_SELECT} WHERE sf.app_id=$1 ORDER BY sf.updated_at DESC"
    ))
    .bind::<SqlUuid, _>(app_id)
    .get_results(conn)
    .await
}

pub async fn create_saved_funnel(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    created_by: Uuid,
    name: &str,
    description: Option<&str>,
    steps: &Value,
) -> QueryResult<SavedFunnelRow> {
    diesel::sql_query(format!(
        "WITH ins AS ( \
           INSERT INTO saved_funnels (app_id, name, description, steps, created_by) \
           VALUES ($1, $2, $3, $4, $5) RETURNING * \
         ) {} FROM ins sf LEFT JOIN users u ON u.id = sf.created_by",
        // reuse the same projection but from the CTE
        "SELECT sf.id, sf.app_id, sf.name, sf.description, sf.steps, u.name AS created_by_name, sf.created_at, sf.updated_at"
    ))
    .bind::<SqlUuid, _>(app_id)
    .bind::<Text, _>(name)
    .bind::<Nullable<Text>, _>(description)
    .bind::<Jsonb, _>(steps)
    .bind::<SqlUuid, _>(created_by)
    .get_result(conn)
    .await
}

/// Returns number of rows updated (0 → not found / wrong app).
pub async fn update_saved_funnel(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    id: Uuid,
    name: &str,
    description: Option<&str>,
    steps: &Value,
) -> QueryResult<usize> {
    diesel::sql_query(
        "UPDATE saved_funnels SET name=$3, description=$4, steps=$5, updated_at=now() \
         WHERE app_id=$1 AND id=$2",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<SqlUuid, _>(id)
    .bind::<Text, _>(name)
    .bind::<Nullable<Text>, _>(description)
    .bind::<Jsonb, _>(steps)
    .execute(conn)
    .await
}

/// Returns number of rows deleted (0 → not found / wrong app).
pub async fn delete_saved_funnel(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    id: Uuid,
) -> QueryResult<usize> {
    diesel::sql_query("DELETE FROM saved_funnels WHERE app_id=$1 AND id=$2")
        .bind::<SqlUuid, _>(app_id)
        .bind::<SqlUuid, _>(id)
        .execute(conn)
        .await
}
```

- [ ] **Step 2: Build**

Run: `cd backend && cargo build -p sauron-db`
Expected: clean. (If the `create_saved_funnel` CTE projection is awkward, an accepted simpler alternative: `INSERT … RETURNING` the row without the join, then `list`-style re-select by id — but the CTE above returns the joined row in one round-trip.)

- [ ] **Step 3: Commit**

```bash
git add backend/crates/sauron-db/src/repo.rs
git commit -m "feat(db): saved_funnels CRUD repo functions"
```

---

### Task B4: Saved-funnels API (list/create/update/delete) + validation

**Files:**
- Modify: `backend/bins/sauron-api/src/routes/funnels.rs`
- Modify: `backend/bins/sauron-api/src/main.rs`

**Interfaces:**
- Consumes: `repo::{list_saved_funnels, create_saved_funnel, update_saved_funnel, delete_saved_funnel}`, `perm::{EVENT_READ, FUNNEL_WRITE}`.
- Produces: `GET/POST /v1/apps/{app_id}/funnels`, `PATCH/DELETE /v1/apps/{app_id}/funnels/{funnel_id}`; pure `validate_steps(&[String]) -> Result<(), String>`.

- [ ] **Step 1: Write the failing test for `validate_steps`**

At the bottom of `funnels.rs`:

```rust
#[cfg(test)]
mod validate_steps_tests {
    use super::validate_steps;

    #[test]
    fn rejects_too_few() {
        assert!(validate_steps(&["a".into()]).is_err());
    }

    #[test]
    fn rejects_too_many() {
        let steps: Vec<String> = (0..11).map(|i| i.to_string()).collect();
        assert!(validate_steps(&steps).is_err());
    }

    #[test]
    fn accepts_two_to_ten() {
        assert!(validate_steps(&["a".into(), "b".into()]).is_ok());
    }
}
```

- [ ] **Step 2: Run it — expect failure**

Run: `cd backend && cargo test -p sauron-api validate_steps_tests`
Expected: FAIL — `validate_steps` not found.

- [ ] **Step 3: Implement validation, DTOs, and the four handlers**

Add to `funnels.rs` (extend imports: `use axum::extract::Path;` already present; add `Query`? not needed. Add `use crate::error::ApiError;` already present. Import `repo` already present.):

```rust
/// Shared 2..=10 step-count validation (matches `compute`).
pub fn validate_steps(steps: &[String]) -> Result<(), String> {
    if steps.len() < 2 {
        return Err("a funnel needs at least 2 steps".into());
    }
    if steps.len() > 10 {
        return Err("at most 10 steps".into());
    }
    if steps.iter().any(|s| s.trim().is_empty()) {
        return Err("steps cannot be empty".into());
    }
    Ok(())
}

#[derive(Deserialize)]
pub struct SaveFunnelReq {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub steps: Vec<String>,
}

pub async fn list_saved(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
) -> Result<Json<Vec<repo::SavedFunnelRow>>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::EVENT_READ).await?;
    Ok(Json(repo::list_saved_funnels(&mut conn, app_id).await?))
}

pub async fn create_saved(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(app_id): Path<Uuid>,
    Json(req): Json<SaveFunnelReq>,
) -> Result<Json<repo::SavedFunnelRow>, ApiError> {
    if req.name.trim().is_empty() {
        return Err(ApiError::BadRequest("name is required".into()));
    }
    validate_steps(&req.steps).map_err(ApiError::BadRequest)?;
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::FUNNEL_WRITE).await?;
    let steps = serde_json::json!(req.steps);
    let row = repo::create_saved_funnel(
        &mut conn,
        app_id,
        auth.user_id,
        req.name.trim(),
        req.description.as_deref(),
        &steps,
    )
    .await?;
    Ok(Json(row))
}

pub async fn update_saved(
    auth: AuthUser,
    State(state): State<AppState>,
    Path((app_id, funnel_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<SaveFunnelReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if req.name.trim().is_empty() {
        return Err(ApiError::BadRequest("name is required".into()));
    }
    validate_steps(&req.steps).map_err(ApiError::BadRequest)?;
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::FUNNEL_WRITE).await?;
    let steps = serde_json::json!(req.steps);
    let n = repo::update_saved_funnel(
        &mut conn,
        app_id,
        funnel_id,
        req.name.trim(),
        req.description.as_deref(),
        &steps,
    )
    .await?;
    if n == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

pub async fn delete_saved(
    auth: AuthUser,
    State(state): State<AppState>,
    Path((app_id, funnel_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut conn = db(&state).await?;
    authorize_app(&mut conn, auth.user_id, app_id, perm::FUNNEL_WRITE).await?;
    let n = repo::delete_saved_funnel(&mut conn, app_id, funnel_id).await?;
    if n == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}
```

> `ApiError::BadRequest(String)` and `ApiError::NotFound` (a **unit** variant) both exist in `error.rs` — used as written above. Add `use serde::Deserialize;` if not already imported (`funnels.rs` already imports it for `FunnelReq`).

Register routes in `main.rs` next to the existing `POST /funnel`:

```rust
.route(
    "/v1/apps/{app_id}/funnels",
    get(routes::funnels::list_saved).post(routes::funnels::create_saved),
)
.route(
    "/v1/apps/{app_id}/funnels/{funnel_id}",
    axum::routing::patch(routes::funnels::update_saved).delete(routes::funnels::delete_saved),
)
```

(Ensure `get`, `post`, `patch`, `delete` are imported from `axum::routing`; the file already imports `get`/`post`.)

- [ ] **Step 4: Run tests + build**

Run: `cd backend && cargo test -p sauron-api validate_steps_tests` → PASS. Then `cargo build` → clean.

- [ ] **Step 5: Commit**

```bash
git add backend/bins/sauron-api/src/routes/funnels.rs backend/bins/sauron-api/src/main.rs
git commit -m "feat(api): saved-funnels CRUD endpoints (funnel:write guarded)"
```

---

### Task B5: Frontend models + Permission + API client

**Files:**
- Modify: `dashboard/src/lib/models/index.ts`
- Modify: `dashboard/src/lib/api/funnels.ts`

**Interfaces:**
- Produces: `SavedFunnel` type; `'funnel:write'` in `Permission`; `listSavedFunnels`, `saveFunnel`, `updateFunnel`, `deleteFunnel`.

- [ ] **Step 1: Add `SavedFunnel` + extend `Permission`**

In `models/index.ts`, add `| 'funnel:write'` to the `Permission` union (after `'event:read'`). Then append near the Funnels section:

```ts
export interface SavedFunnel {
  id: string;
  app_id: string;
  name: string;
  description?: string | null;
  steps: string[];
  created_by_name?: string | null;
  created_at: string;
  updated_at: string;
}
```

- [ ] **Step 2: Add API client functions**

Append to `dashboard/src/lib/api/funnels.ts`:

```ts
import type { FunnelResult, SavedFunnel } from '../models';

export async function listSavedFunnels(appId: string): Promise<SavedFunnel[]> {
  const { data } = await api.get<SavedFunnel[]>(`/v1/apps/${appId}/funnels`);
  return data;
}

export interface SaveFunnelBody {
  name: string;
  description?: string;
  steps: string[];
}

export async function saveFunnel(appId: string, body: SaveFunnelBody): Promise<SavedFunnel> {
  const { data } = await api.post<SavedFunnel>(`/v1/apps/${appId}/funnels`, body);
  return data;
}

export async function updateFunnel(appId: string, id: string, body: SaveFunnelBody): Promise<void> {
  await api.patch(`/v1/apps/${appId}/funnels/${id}`, body);
}

export async function deleteFunnel(appId: string, id: string): Promise<void> {
  await api.delete(`/v1/apps/${appId}/funnels/${id}`);
}
```

> Merge the `SavedFunnel` import into the existing `import type { FunnelResult } from '../models';` line rather than adding a duplicate import.

- [ ] **Step 3: Typecheck + commit**

Run: `cd dashboard && npx svelte-check --tsconfig ./tsconfig.json` → 0 errors.

```bash
git add dashboard/src/lib/models/index.ts dashboard/src/lib/api/funnels.ts
git commit -m "feat(dashboard): SavedFunnel model + funnel:write perm + CRUD client"
```

---

### Task B6: Saved-funnels panel in the builder (save / load / clone / update / delete)

**Files:**
- Modify: `dashboard/src/pages/FunnelBuilder.svelte`

**Interfaces:**
- Consumes: `listSavedFunnels, saveFunnel, updateFunnel, deleteFunnel, SavedFunnel`, `sessionStore.can`, `Button`, `Icon`.

- [ ] **Step 1: Imports + state**

Add to the `<script>`:

```ts
  import { listSavedFunnels, saveFunnel, updateFunnel, deleteFunnel } from '../lib/api/funnels';
  import type { SavedFunnel } from '../lib/models';

  let saved = $state<SavedFunnel[]>([]);
  let loadedId = $state<string | null>(null);
  let saveError = $state<string | null>(null);
  const canWrite = $derived(sessionStore.can('funnel:write'));

  async function loadSaved(aid: string) {
    try {
      saved = await listSavedFunnels(aid);
    } catch {
      saved = [];
    }
  }

  async function onSaveNew() {
    const aid = sessionStore.currentAppId;
    if (!aid || steps.length < 2) return;
    const name = prompt('Name this funnel template:');
    if (!name) return;
    saveError = null;
    try {
      const created = await saveFunnel(aid, { name: name.trim(), steps: [...steps] });
      loadedId = created.id;
      await loadSaved(aid);
    } catch (err) {
      saveError = errorMessage(err);
    }
  }

  async function onUpdate() {
    const aid = sessionStore.currentAppId;
    if (!aid || !loadedId || steps.length < 2) return;
    const current = saved.find((f) => f.id === loadedId);
    saveError = null;
    try {
      await updateFunnel(aid, loadedId, { name: current?.name ?? 'Funnel', steps: [...steps] });
      await loadSaved(aid);
    } catch (err) {
      saveError = errorMessage(err);
    }
  }

  function loadFunnel(f: SavedFunnel) {
    steps = [...f.steps];
    loadedId = f.id;
    const aid = sessionStore.currentAppId;
    if (aid) void compute(aid, sinceDays);
  }

  async function duplicateFunnel(f: SavedFunnel) {
    const aid = sessionStore.currentAppId;
    if (!aid) return;
    try {
      await saveFunnel(aid, { name: `Copy of ${f.name}`, description: f.description ?? undefined, steps: [...f.steps] });
      await loadSaved(aid);
    } catch (err) {
      saveError = errorMessage(err);
    }
  }

  async function removeFunnel(f: SavedFunnel) {
    const aid = sessionStore.currentAppId;
    if (!aid || !confirm(`Delete “${f.name}”?`)) return;
    try {
      await deleteFunnel(aid, f.id);
      if (loadedId === f.id) loadedId = null;
      await loadSaved(aid);
    } catch (err) {
      saveError = errorMessage(err);
    }
  }
```

Add `errorMessage` to the imports if not present: `import { errorMessage } from '../lib/api/client';` (the file currently imports it — confirm). Load saved funnels in the existing app-change `$effect` (the one calling `loadEvents`): add `void loadSaved(aid);` alongside it.

- [ ] **Step 2: Save controls in the Builder card**

In the `<Card title="Builder">`, inside `.compute-row` (after the Compute button block), add:

```svelte
          {#if canWrite}
            {#if loadedId}
              <Button variant="secondary" size="sm" onclick={onUpdate} disabled={steps.length < 2}>Update</Button>
              <Button variant="secondary" size="sm" onclick={onSaveNew} disabled={steps.length < 2}>Save as new</Button>
            {:else}
              <Button variant="secondary" size="sm" onclick={onSaveNew} disabled={steps.length < 2}>Save template</Button>
            {/if}
          {/if}
```

`{#if saveError}<span class="faint hint">{saveError}</span>{/if}` under the row.

- [ ] **Step 3: Saved-funnels list panel**

Add a third card in the `.grid` (or above it). Insert after the closing `</Card>` of the Results card, still inside `.grid` if a third column is desired, or above `.grid` as a full-width strip. Full-width strip version (place directly above `<div class="grid">`):

```svelte
    {#if saved.length > 0}
      <Card title="Saved funnels">
        <ul class="saved-list">
          {#each saved as f (f.id)}
            <li class="saved-item" class:active={f.id === loadedId}>
              <button class="load" type="button" onclick={() => loadFunnel(f)} title="Load this funnel">
                <span class="sf-name truncate">{f.name}</span>
                <span class="sf-meta">{f.steps.length} steps{#if f.created_by_name} · {f.created_by_name}{/if}</span>
              </button>
              <div class="sf-actions">
                <button type="button" title="Duplicate" onclick={() => duplicateFunnel(f)}><Icon name="copy" size={14} /></button>
                {#if canWrite}
                  <button type="button" title="Delete" onclick={() => removeFunnel(f)}><Icon name="trash-2" size={14} /></button>
                {/if}
              </div>
            </li>
          {/each}
        </ul>
      </Card>
    {/if}
```

Add styles:

```css
  .saved-list { list-style: none; margin: 0; padding: 0; display: flex; flex-wrap: wrap; gap: 8px; }
  .saved-item { display: flex; align-items: center; gap: 4px; border: 1px solid var(--border); border-radius: var(--radius-sm); background: var(--surface-2); }
  .saved-item.active { border-color: var(--primary-border); }
  .saved-item .load { display: flex; flex-direction: column; align-items: flex-start; gap: 2px; padding: 7px 10px; background: none; border: none; cursor: pointer; text-align: left; }
  .sf-name { font-size: 13px; color: var(--text); font-weight: 560; max-width: 220px; }
  .sf-meta { font-size: 11px; color: var(--text-faint); }
  .sf-actions { display: flex; gap: 2px; padding-right: 6px; }
  .sf-actions button { background: none; border: none; color: var(--text-faint); padding: 4px; border-radius: var(--radius-sm); cursor: pointer; }
  .sf-actions button:hover { color: var(--text); background: var(--surface-3); }
```

(Confirm the icon names `copy` and `trash-2` exist in the `Icon` set; if not, use ones that do — e.g. `x` for delete, matching the existing remove-step button.)

- [ ] **Step 4: Typecheck + build**

Run: `cd dashboard && npx svelte-check --tsconfig ./tsconfig.json && npx vite build`
Expected: 0 errors; build succeeds.

- [ ] **Step 5: Commit**

```bash
git add dashboard/src/pages/FunnelBuilder.svelte
git commit -m "feat(dashboard): saved-funnel templates — save/load/clone/update/delete panel"
```

---

### Task B7: End-to-end verification (Part B)

**Files:** none (verification only).

- [ ] **Step 1: Ensure the stack is up with the new migration applied** (`docker compose up --build -d`; confirm `saved_funnels` exists via `docker compose exec db psql -U <user> -d <db> -c '\d saved_funnels'`).

- [ ] **Step 2: CRUD via curl (as a write-capable user)**

```bash
# create
FID=$(curl -s -X POST "localhost:10000/v1/apps/$APP/funnels" -H "authorization: Bearer $TOKEN" \
  -H 'content-type: application/json' \
  -d '{"name":"Signup flow","steps":["page_view","signup_start","signup_complete"]}' | jq -r .id)
# list
curl -s "localhost:10000/v1/apps/$APP/funnels" -H "authorization: Bearer $TOKEN" | jq
# update
curl -s -X PATCH "localhost:10000/v1/apps/$APP/funnels/$FID" -H "authorization: Bearer $TOKEN" \
  -H 'content-type: application/json' -d '{"name":"Signup flow v2","steps":["page_view","signup_complete"]}' | jq
# delete
curl -s -X DELETE "localhost:10000/v1/apps/$APP/funnels/$FID" -H "authorization: Bearer $TOKEN" | jq
```

Expected: create returns the row with `created_by_name`; list includes it; update returns `{ok:true}`; delete returns `{ok:true}`; a second delete of the same id returns 404. A `steps` array with 1 entry returns 400.

- [ ] **Step 3: Authz check** — repeat the `POST` with a Viewer's token (a user granted only `event:read`): expect **403**; the `GET` list still returns 200.

- [ ] **Step 4: UI check** — via preview, open `#/funnels`: build a funnel, click **Save template**, confirm it appears in the Saved funnels panel; **Duplicate** it (a `Copy of …` appears); **Load** the copy, change a step, **Save as new**; **Delete** one. Confirm a Viewer login sees the panel read-only (no Save/Delete, Duplicate hidden or disabled per `canWrite`). Screenshot for the maintainer.

- [ ] **Step 5: Record result** with the actual responses; fix and re-run on any failure.

---

## Self-Review (completed by plan author)

**Spec coverage — feature A:** DAU trend (A2/A8/A10) ✓; total users all-time (A1/A10) ✓; WAU/MAU (A1/A10) ✓; stickiness (A4/A10) ✓; new-users/day (A2/A8) ✓; avg+median session (A1/A3/A10/A11) ✓; duration trend (A3/A11) ✓; duration histogram (A3/A9/A11) ✓; one call per screen (A4/A5) ✓; flat routes (A4/A5) ✓; range clamps + windows (A1/A4) ✓. **Feature B:** table+migration (B1) ✓; `funnel:write` + Viewer-safe (B2) ✓; CRUD repo (B3) ✓; endpoints + validation + authz (B4/B7) ✓; clone = create, Duplicate + Save-as-new (B6) ✓; shared library w/ `created_by_name` (B3/B6) ✓; range not persisted (B1) ✓.

**Placeholder scan:** none — every code step carries full code. Two explicit "confirm this exists" notes (Icon names, `ApiError::NotFound` variant, `formatDuration` unit) are verification instructions, not placeholders; each names a concrete fallback.

**Type consistency:** `UserSeriesPoint.new_users` (Rust `new_users` / TS `new_users`) consistent A2↔A6↔A8. `SeriesAvgPoint.avg_ms` mapped to `SeriesPoint.count` only inside A11's derived adapter. `SavedFunnelRow.created_by_name` matches TS `SavedFunnel.created_by_name` (B3↔B5↔B6). `validate_steps`/`stickiness`/`merge_user_series`/`order_histogram` names match their tests.
