# Design: User-count & session-duration analytics

**Date:** 2026-07-13
**Status:** Approved (brainstorming) — pending spec review
**Area:** `backend/` (sauron-db, sauron-api) + `dashboard/`

## Goal

Give the dashboard real **audience** and **session-engagement** analytics that don't exist today:

1. **Active users per day** (DAU trend) — not just one range-wide number.
2. **Total number of users** — an *all-time* count (today's "Users" tile is range-scoped).
3. Related engagement metrics: **WAU, MAU, stickiness, new-users/day**.
4. **Session-duration aggregates**: average / median session length, a per-day duration trend, and a duration distribution.

Per-session start/end/duration already exists (Sessions list "Started" + "Duration" columns, Session detail timeline) and needs no change.

## Definitions (authoritative — all numbers derive from these)

- **User** = a `distinct_id` row in `event_users`. Both analytics events and errors call `repo::touch_event_user`, so a user is any `distinct_id` that produced ≥1 event or error.
- **Active on day D** = a `distinct_id` that produced an analytics event **or** an error on D. Computed as `COUNT(DISTINCT distinct_id)` over the **union of `analytics_events` + `error_events`** bucketed by day. Rows with `NULL`/empty `distinct_id` are excluded (anonymous ≠ a user), consistent with the existing range-scoped "Users" tile.
- **DAU / WAU / MAU** = distinct active users in a **rolling 1 / 7 / 30-day** window anchored at `now()` (standard Amplitude/Mixpanel definition), independent of the selected range.
- **Stickiness** = `DAU / MAU`, rendered as a percentage.
- **Total users** = `COUNT(*) FROM event_users` for the app — **all-time**, no date filter.
- **New user on day D** = `event_users.first_seen` falls on D.
- **Session duration** = `last_event_at − started_at` (session start → last activity), in **milliseconds**. Sessions with a single event have duration 0; they are included (not filtered) so counts stay consistent with the Sessions list.
- **Selected range** (`since_days`, clamped 1..365) scopes: the DAU/new-users series, the range tiles (Active/New in range), avg/median session duration, the duration trend, and the duration histogram. It does **not** scope Total users, WAU, or MAU.

## Approach decisions (chosen)

1. **DAU computed on-read from raw tables** (not a precomputed daily-active rollup). Matches the existing `event_series`/`error_series` pattern; no migration or backfill. A per-day rollup table is the documented scale-up path if reads ever get slow — **not built now**.
2. **One combined endpoint per screen** so each screen makes a single call — mirrors how `overview` returns totals + series together.
3. **A small dedicated `UserActivityChart`** (bars = active/day, overlaid line = new/day) rather than overloading the shared `TimeSeriesChart` with multi-series. The duration **trend** does reuse `TimeSeriesChart` via a new optional `format?` prop (backward-compatible).

## Placement

- **Users screen** (`UsersExplorer.svelte`) — audience + at-a-glance engagement, one API call:
  - `UserActivityChart` (active bars + new-users line)
  - Tiles: **Total users** (all-time), Active (range), New (range), **WAU**, **MAU**, **Stickiness %**, **Avg session**, **Median session**
  - A `DateRange` control driving `sinceDays` (reusing the Overview pattern). The existing people table / search / pagination below is unchanged and range-independent.
- **Sessions screen** (`SessionsList.svelte`) — new analytics header above the existing table, one API call:
  - Tiles: Sessions (range), Crashed, **Avg session**, **Median session**
  - **Duration trend** — average session length per day (`TimeSeriesChart` with duration formatting)
  - **Duration distribution** — `DurationHistogram` over buckets `<10s`, `10–60s`, `1–5m`, `5–30m`, `30m+`
  - Its own `DateRange` control.

## Backend

### `sauron-db` (`crates/sauron-db/src/repo.rs`)

New query functions following the existing `overview_totals` / `event_series` style (`diesel::sql_query`, `#[derive(QueryableByName)]` structs, `SqlUuid`/`Timestamptz`/`BigInt`/`Double`/`Text` bindings):

- `user_stats(conn, app_id, since) -> QueryResult<UserStats>` — single row of scalar subqueries:
  - `total_users` = `count(*) FROM event_users WHERE app_id=$1`
  - `active_in_range` = `count(*) FROM event_users WHERE app_id=$1 AND last_seen>=$2`
  - `new_in_range` = `count(*) FROM event_users WHERE app_id=$1 AND first_seen>=$2`
  - `dau` / `wau` / `mau` = `count(DISTINCT distinct_id)` over the analytics+errors union with `occurred_at >= now() - interval '1|7|30 days'` and `distinct_id` not null
  - `avg_session_ms` = `avg(EXTRACT(EPOCH FROM (last_event_at - started_at)) * 1000)` over `sessions WHERE app_id=$1 AND last_event_at>=$2`
  - `median_session_ms` = `percentile_cont(0.5) WITHIN GROUP (ORDER BY EXTRACT(EPOCH FROM (last_event_at - started_at)) * 1000)` over the same set
  - (Fields are `Double`/nullable where aggregates can be null on empty sets; handler coerces null → 0.)
- `session_stats(conn, app_id, since) -> QueryResult<SessionStats>` — scalar subqueries: `sessions` = `count(*) FROM sessions WHERE app_id=$1 AND last_event_at>=$2`; `crashed` = same with `AND errors_count>0`; `avg_session_ms` / `median_session_ms` = the same avg / `percentile_cont(0.5)` over `EXTRACT(EPOCH FROM (last_event_at - started_at)) * 1000` used in `user_stats`.
- `active_user_series(conn, app_id, since) -> QueryResult<Vec<UserSeriesPoint>>` — per-day `{ bucket, active, new }`. Active from the union sub-select `GROUP BY bucket`; new from `event_users.first_seen` bucketed by day; merged in Rust by `bucket` (LEFT-join semantics, missing side → 0). Fields `bucket: Timestamptz`, `active: BigInt`, `new: BigInt` (SQL alias `new_` → Rust `new_users` to avoid the reserved word).
- `session_duration_series(conn, app_id, since) -> QueryResult<Vec<SeriesAvgPoint>>` — `{ bucket, avg_ms }` = `date_trunc('day', started_at)`, `avg(duration_ms)`, `GROUP BY bucket ORDER BY bucket`.
- `session_duration_histogram(conn, app_id, since) -> QueryResult<Vec<HistoBucket>>` — `CASE`-based bucketing on duration into the five labels, `count(*)` each. Returns rows in a fixed label order (0-fill missing buckets in the handler so the chart always shows all five).

### `sauron-api` (`bins/sauron-api/src/routes/analytics.rs` + `main.rs`)

Reuse `RangeQuery`, `AuthUser`, `authorize_app(..., perm::EVENT_READ)`, `db(&state)`, `since = now - Duration::days(clamp(1,365))` — identical guard to `overview`.

- `users_summary(auth, State, Path(app_id), Query(RangeQuery)) -> Json<UsersAnalytics>`:
  - Calls `user_stats` + `active_user_series`; computes `stickiness = if mau>0 { dau/mau } else { 0.0 }`.
  - Response `UsersAnalytics { stats: UserStats, stickiness: f64, series: Vec<UserSeriesPoint> }`.
  - Route: `.route("/v1/apps/{app_id}/users/summary", get(routes::analytics::users_summary))`.
- `sessions_summary(...) -> Json<SessionsAnalytics>`:
  - Calls a dedicated `session_stats(conn, app_id, since) -> SessionStats { sessions, crashed, avg_session_ms, median_session_ms }` + `session_duration_series` + `session_duration_histogram`.
  - Response `SessionsAnalytics { stats: SessionStats, duration_series: Vec<SeriesAvgPoint>, duration_histogram: Vec<HistoBucket> }`.
  - Route: `.route("/v1/apps/{app_id}/sessions/summary", get(routes::analytics::sessions_summary))`.

Both use the existing **flat** path convention (next to `/overview`, `/persons`, `/sessions`) — no new `/analytics/` prefix. `/users/summary` is a new namespace for aggregate audience metrics (distinct from `/persons`, which lists individuals); `/sessions/summary` sits beside the existing `/sessions` list. Existing routes are untouched.

Response DTOs derive `Serialize`; field names are snake_case to match the existing axum JSON convention. Numeric duration fields are `f64` ms.

## Frontend (`dashboard/`)

### Models (`src/lib/models/index.ts`)

```ts
export interface UserStats {
  total_users: number;
  active_in_range: number;
  new_in_range: number;
  dau: number; wau: number; mau: number;
  avg_session_ms: number; median_session_ms: number;
}
export interface UserSeriesPoint { bucket: string; active: number; new_users: number; }
export interface UsersAnalytics { stats: UserStats; stickiness: number; series: UserSeriesPoint[]; }

export interface SessionStats { sessions: number; crashed: number; avg_session_ms: number; median_session_ms: number; }
export interface SeriesAvgPoint { bucket: string; avg_ms: number; }
export interface HistoBucket { bucket: string; count: number; }
export interface SessionsAnalytics { stats: SessionStats; duration_series: SeriesAvgPoint[]; duration_histogram: HistoBucket[]; }
```

### API clients

- `src/lib/api/users.ts`: `getUserAnalytics(appId, sinceDays) => GET /v1/apps/{appId}/users/summary?since_days=`.
- `src/lib/api/sessions.ts` (extend or add): `getSessionAnalytics(appId, sinceDays) => GET /v1/apps/{appId}/sessions/summary?since_days=`.

### Components

- **`UserActivityChart.svelte`** (new) — reuses `TimeSeriesChart`'s visual language: active users as bars, new users as an overlaid line; tooltip shows both per day. Consumes `UserSeriesPoint[]`.
- **`DurationHistogram.svelte`** (new) — categorical bars for the five duration buckets, count labels; consumes `HistoBucket[]`.
- **`TimeSeriesChart.svelte`** (edit) — add optional `format?: (n: number) => string` prop (default identity/number) used in tooltips + axis so the duration trend renders `formatDuration(ms)` instead of raw numbers. No behavior change when the prop is omitted.

### Screens

- **`UsersExplorer.svelte`** — add an analytics header above the current table: `DateRange` (`sinceDays` state, default 30, `RANGES`), a `StatTiles` row (Total users / Active / New / WAU / MAU / Stickiness / Avg session / Median session — durations via `formatDuration`, stickiness via `formatPercent`), and `UserActivityChart`. Load via a `$effect` on `currentAppId` + `sinceDays`, mirroring `Overview.svelte`. Table logic untouched.
- **`SessionsList.svelte`** — add an analytics header above the table: `DateRange`, a `StatTiles` row (Sessions / Crashed / Avg session / Median session), `TimeSeriesChart` (duration trend, `format={formatDuration}`), `DurationHistogram`. Existing table untouched.

Reuse existing helpers: `formatDuration`, `durationBetween`, `compactNumber`, `formatPercent` (already imported across pages).

## Testing

- **Backend** (`cargo test`, existing integration harness): tests for `user_stats` (all-time total ≠ range-scoped active; WAU ≥ DAU; MAU ≥ WAU on seeded data), `active_user_series` (distinct-counting, new-user bucketing, day merge with 0-fill), `session_duration_series` + `session_duration_histogram` (bucket boundaries, avg/median correctness). Follow the existing repo-test seeding pattern.
- **Frontend**: `svelte-check` 0 errors + `vite build` clean. Any pure helper (e.g. duration-bucket labeling if extracted) gets a vitest.
- **End-to-end**: against the running `docker compose` stack (API :10000, dashboard :10002) with seeded signals — verify `/users` shows a non-zero DAU trend, an all-time total distinct from the range active count, and `/sessions` shows avg/median duration + a populated histogram.

## Scope guardrails (YAGNI — explicitly out)

- No cohort / retention grid.
- No precomputed daily-active or session rollup table (on-read only).
- WAU/MAU/DAU stay rolling-from-`now`, not range-scoped.
- No change to the people table's or sessions table's existing list queries.
- No new SDK or ingest changes — all data already lands in `event_users`, `analytics_events`, `error_events`, `sessions`.

## Files touched (summary)

- `backend/crates/sauron-db/src/repo.rs` — 4 new query fns + DTO structs.
- `backend/bins/sauron-api/src/routes/analytics.rs` — 2 handlers + response DTOs.
- `backend/bins/sauron-api/src/main.rs` — 2 route registrations.
- `dashboard/src/lib/models/index.ts` — new interfaces.
- `dashboard/src/lib/api/users.ts` (new), `src/lib/api/sessions.ts` (new/extend).
- `dashboard/src/lib/components/UserActivityChart.svelte` (new), `DurationHistogram.svelte` (new), `TimeSeriesChart.svelte` (edit).
- `dashboard/src/pages/UsersExplorer.svelte`, `SessionsList.svelte` (analytics headers).
- Backend + frontend tests as above.
