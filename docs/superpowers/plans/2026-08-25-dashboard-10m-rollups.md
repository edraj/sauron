# Dashboard 10M/day Rollups Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every dashboard aggregate reads pre-computed rollup tables (≤1-min stale, refresh-on-demand) instead of scanning raw events, per the approved spec `docs/superpowers/specs/2026-08-25-dashboard-10m-per-day-optimization-design.md`.

**Architecture:** A watermarked fold task in the ingest process incrementally aggregates newly-committed firehose rows (by `received_at`, bucketed by `occurred_at`) into 7 small rollup tables; HLL/histogram sketches carry distincts/percentiles; repo read functions branch to rollup-backed SQL behind an `is_ready` gate exactly like `device_env_backfill::is_backfilled`; a one-shot `sauron-migrate backfill-rollups` covers pre-epoch history.

**Tech Stack:** Rust (axum, diesel-async raw SQL, deadpool), Postgres 16 (stock — no extensions), Redis 7 (`sauron-redis` store), Svelte 5 + vitest.

## Global Constraints

- **NEVER `git commit`, never create branches, never `git stash`** (unconditional repo rule). All work stays uncommitted in the working tree.
- Transactions in sauron-db use `conn.batch_execute("BEGIN")`/`("COMMIT")`/`("ROLLBACK")` — **never** `conn.transaction(...)` (MSRV 1.82, batch.rs:904).
- Raw SQL binds: env fragment via `EnvFilter::sql_fragment{,_for}(idx)` consumes an index only for `One`/`Subset`; `Range::upper_sql` **binds LAST** (scope.rs discipline).
- Bulk upserts: `unnest($n::type[])` arrays, rows **sorted by conflict key** (lock ordering) and **pre-deduped** (batch.rs:368-371, :940-950).
- `distinct_id`: `analytics_events` is `NOT NULL DEFAULT ''` (empty = no identity, excluded from user counts); `error_events`/`transactions` are nullable → paired guard `IS NOT NULL AND <> ''` when unioning.
- Screen views = `analytics_events.name = '$screen'`; dwell = gap to next analytics event in session, capped 1 800 000 ms.
- Stock `postgres:16` image — sketches are Rust-side (`sha2` already a workspace dep); no `hll`/extension.
- DB tests gate on **`TEST_DATABASE_URL`** (maintenance URL) + `TEST_REDIS_URL` and SKIP silently when unset — every verify step must confirm a non-0.00 s suite time. Run with `dangerouslyDisableSandbox` and container-IP URLs (compose PG publishes no host port).
- New mutating routes must be audited or listed in `audit_coverage.rs` `EXEMPT` with a real reason.
- All new dashboard strings in **en + ar** (`catalog/*.ts`); page logic in pure `models/*.ts` modules with co-located vitest — there is no component-render harness.
- Migration dir: `backend/migrations/2026-08-25-000071_rollups/` with both `up.sql` and `down.sql`.
- Config knobs via `parse("KEY", default)` in `Config::from_env` (config.rs pattern).

## Disclosed semantic deltas (docblock each; ≈-marked in UI)

1. Distinct-user figures = HLL p=12 (±~1.6%); latency percentiles = √2 log-bucket histogram (±~3.5% value error).
2. DAU/WAU/MAU become **calendar-day** (UTC) windows, not rolling now-24h/-7d/-30d.
3. Journeys become **day-scoped** (first ≤10 steps per user per UTC day, summed over window) instead of first-N-per-user-from-window-start.
4. Rollup window matching is whole-bucket: a mid-day `from` includes its whole UTC day (hour for perf series).
5. Sessions summary windows by **started day** (was `last_event_at >= from` for stats).
6. Project-level `/v1/projects/{proj}/active-users` intentionally stays raw (mutable identified-split; own cache+gates+92d cap).

## File Structure

- Create `backend/migrations/2026-08-25-000071_rollups/{up.sql,down.sql}` — all DDL.
- Create `backend/crates/sauron-db/src/sketch.rs` — Hll + LatencyHistogram, pure.
- Create `backend/crates/sauron-db/src/rollups/mod.rs` — watermarks, epoch, is_ready, as_of, fold orchestration, upserts.
- Create `backend/crates/sauron-db/src/rollups/fold.rs` — per-source fold (raw row pull → Rust aggregation → upsert).
- Create `backend/crates/sauron-db/src/rollups/read.rs` — rollup-backed read fns returning the existing repo output structs.
- Modify `backend/crates/sauron-db/src/lib.rs` — `pub mod sketch; pub mod rollups;`.
- Modify `backend/crates/sauron-db/src/repo.rs` — gate branches inside: `screen_list`, `count_screens`, `journey_graph`, `performance_summary`, `performance_series`, `user_stats`, `active_user_series`, `session_stats`, `session_duration_series`, `session_duration_histogram`, `top_events`, `overview_totals`, `event_series`, `error_series`.
- Modify `backend/bins/sauron-api/src/tier_read.rs` — `active_users_by_day` rollup branch.
- Modify `backend/bins/sauron-api/src/main.rs` + `routes/analytics.rs` — `POST /v1/apps/{app}/rollups/refresh`, `as_of`/`approx` fields.
- Modify `backend/bins/sauron-api/tests/audit_coverage.rs` — EXEMPT entry.
- Modify `backend/bins/sauron-ingest/src/main.rs` — spawn aggregator task.
- Create `backend/crates/sauron-pipeline/src/rollup_task.rs` (+ `pub mod` in lib.rs) — tick loop, leader lock, kick key, consistency day-check.
- Modify `backend/crates/sauron-core/src/config.rs` — `rollup_fold_secs`, `rollup_lag_secs`, `rollup_kick_lag_secs`, `rollup_name_cap`.
- Modify `backend/bins/sauron-migrate/src/main.rs` — `backfill-rollups` arg.
- Create `backend/crates/sauron-db/tests/rollup_equivalence.rs` — fold vs legacy-query equivalence + boundary-state tests.
- Modify `backend/bins/sauron-tier/src/main.rs` — export interlock on rollup watermark.
- Create `dashboard/src/lib/models/freshness.ts` + `freshness.test.ts`; modify pages `ScreensList/JourneyExplorer/Performance/UsersExplorer/SessionsList/Events/Overview.svelte`, `models/index.ts`, `api/rollups.ts` (new), `i18n/catalog/time.ts`.
- Create `docs/approximate-analytics.md`; modify `scripts/seed/10-dimensions.sql` (light payload mode) + `scripts/seed/README.md`.

---

### Task 1: Migration 71 — rollup schema

**Files:** Create `backend/migrations/2026-08-25-000071_rollups/up.sql`, `down.sql`.

**Interfaces (produced, relied on by every later task):** tables `rollup_epoch(only_row, started_at)`, `rollup_watermarks(source PK text, watermark timestamptz, updated_at)` seeded with `analytics_events|error_events|transactions|sessions` at the epoch instant; `rollup_backfill(app_id PK, completed_at)`; `screen_stats_daily`, `journey_nodes_daily`, `journey_links_daily`, `perf_agg_hourly`, `session_stats_daily`, `user_activity_daily`, `event_top_daily`; state tables `rollup_session_state`, `rollup_journey_state`; BRIN `*_received_brin` on the 3 firehose parents.

- [ ] **Step 1: write up.sql** (comment style per house migrations; storage: rollups are plain tables):

```sql
-- Epoch + watermarks: fold covers (epoch, ∞) by received_at; backfill covers
-- (-∞, epoch]. Stamped in the same migration that creates the tables, per the
-- migration-70 lesson: a later stamp lies about rows the live path never saw.
CREATE TABLE rollup_epoch (
    only_row   boolean     PRIMARY KEY DEFAULT true CHECK (only_row),
    started_at timestamptz NOT NULL DEFAULT now()
);
INSERT INTO rollup_epoch DEFAULT VALUES;

CREATE TABLE rollup_watermarks (
    source     text        PRIMARY KEY,
    watermark  timestamptz NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now()
);
INSERT INTO rollup_watermarks (source, watermark)
SELECT s, (SELECT started_at FROM rollup_epoch)
FROM unnest(ARRAY['analytics_events','error_events','transactions','sessions']) AS s;

CREATE TABLE rollup_backfill (
    app_id       uuid PRIMARY KEY REFERENCES apps(id) ON DELETE CASCADE,
    completed_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE screen_stats_daily (
    app_id uuid NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    environment_id uuid REFERENCES app_environments(id) ON DELETE CASCADE,
    day date NOT NULL,
    screen text NOT NULL,
    views bigint NOT NULL DEFAULT 0,
    events bigint NOT NULL DEFAULT 0,
    exceptions bigint NOT NULL DEFAULT 0,
    users_hll bytea,
    dwell_ms_sum double precision NOT NULL DEFAULT 0,
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX screen_stats_daily_key ON screen_stats_daily
    (app_id, day, screen, COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid));

CREATE TABLE journey_nodes_daily (
    app_id uuid NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    environment_id uuid REFERENCES app_environments(id) ON DELETE CASCADE,
    day date NOT NULL, step smallint NOT NULL, name text NOT NULL,
    count bigint NOT NULL DEFAULT 0
);
CREATE UNIQUE INDEX journey_nodes_daily_key ON journey_nodes_daily
    (app_id, day, step, name, COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid));

CREATE TABLE journey_links_daily (
    app_id uuid NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    environment_id uuid REFERENCES app_environments(id) ON DELETE CASCADE,
    day date NOT NULL, step smallint NOT NULL,
    from_name text NOT NULL, to_name text NOT NULL,
    count bigint NOT NULL DEFAULT 0
);
CREATE UNIQUE INDEX journey_links_daily_key ON journey_links_daily
    (app_id, day, step, from_name, to_name, COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid));

CREATE TABLE perf_agg_hourly (
    app_id uuid NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    environment_id uuid REFERENCES app_environments(id) ON DELETE CASCADE,
    hour timestamptz NOT NULL, name text NOT NULL, op text NOT NULL,
    count bigint NOT NULL DEFAULT 0,
    error_count bigint NOT NULL DEFAULT 0,
    duration_sum double precision NOT NULL DEFAULT 0,
    duration_hist bigint[] NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX perf_agg_hourly_key ON perf_agg_hourly
    (app_id, hour, name, op, COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid));

CREATE TABLE session_stats_daily (
    app_id uuid NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    environment_id uuid REFERENCES app_environments(id) ON DELETE CASCADE,
    day date NOT NULL,
    sessions bigint NOT NULL DEFAULT 0,
    crashed bigint NOT NULL DEFAULT 0,
    duration_ms_sum double precision NOT NULL DEFAULT 0,
    duration_hist bigint[] NOT NULL,
    d_lt10s bigint NOT NULL DEFAULT 0, d_10_60s bigint NOT NULL DEFAULT 0,
    d_1_5m bigint NOT NULL DEFAULT 0, d_5_30m bigint NOT NULL DEFAULT 0,
    d_gte30m bigint NOT NULL DEFAULT 0,
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX session_stats_daily_key ON session_stats_daily
    (app_id, day, COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid));

CREATE TABLE user_activity_daily (
    app_id uuid NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    environment_id uuid REFERENCES app_environments(id) ON DELETE CASCADE,
    day date NOT NULL,
    hll_all bytea, hll_analytics bytea,
    events bigint NOT NULL DEFAULT 0, errors bigint NOT NULL DEFAULT 0,
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX user_activity_daily_key ON user_activity_daily
    (app_id, day, COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid));

CREATE TABLE event_top_daily (
    app_id uuid NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    environment_id uuid REFERENCES app_environments(id) ON DELETE CASCADE,
    day date NOT NULL, name text NOT NULL,
    count bigint NOT NULL DEFAULT 0
);
CREATE UNIQUE INDEX event_top_daily_key ON event_top_daily
    (app_id, day, name, COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid));

-- Cross-fold state. session_state carries the pending screen-view awaiting its
-- dwell-terminating next event; journey_state carries per-user-per-day step
-- position. Both pruned by the daily maintenance pass (updated_at < now()-2d).
CREATE TABLE rollup_session_state (
    app_id uuid NOT NULL, session_id text NOT NULL,
    environment_id uuid,
    pending_screen text, pending_at timestamptz,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (app_id, session_id)
);
CREATE TABLE rollup_journey_state (
    app_id uuid NOT NULL, day date NOT NULL, distinct_id text NOT NULL,
    env_key uuid NOT NULL,
    steps smallint NOT NULL, last_name text NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (app_id, day, distinct_id, env_key)
);
CREATE INDEX rollup_session_state_age ON rollup_session_state (updated_at);
CREATE INDEX rollup_journey_state_age ON rollup_journey_state (updated_at);

-- The fold's range read: BRIN because received_at is insert-ordered append-only,
-- so a ~KB summary index answers "rows since the watermark" without a btree's
-- per-insert maintenance. Cascades to every partition.
CREATE INDEX analytics_events_received_brin ON analytics_events USING brin (received_at);
CREATE INDEX error_events_received_brin ON error_events USING brin (received_at);
CREATE INDEX transactions_received_brin ON transactions USING brin (received_at);
```

- [ ] **Step 2: down.sql** — `DROP TABLE`/`DROP INDEX IF EXISTS` for everything above (state tables included), reverse order.
- [ ] **Step 3: apply** — `cargo build -p sauron-migrate` then run against dev DB; verify `\d screen_stats_daily`, `SELECT * FROM rollup_watermarks` shows 4 rows at one instant.

### Task 2: `sauron-db::sketch` — HLL + latency histogram

**Files:** Create `backend/crates/sauron-db/src/sketch.rs`; modify `lib.rs` (+`pub mod sketch;`).

**Interfaces (produced):**
```rust
pub struct Hll { registers: Vec<u8> }           // p=12, m=4096, dense
impl Hll {
    pub fn new() -> Self;
    pub fn insert(&mut self, item: &str);        // sha256 → first 8 bytes BE
    pub fn merge(&mut self, other: &Hll);        // bytewise max
    pub fn estimate(&self) -> i64;               // bias-corrected, small-range linear counting
    pub fn to_bytes(&self) -> Vec<u8>;           // 4096 raw bytes
    pub fn from_bytes(b: &[u8]) -> Option<Hll>;  // None on wrong length
}
pub const HIST_BUCKETS: usize = 56;              // √2 ladder from 1 ms; top bucket open
pub struct LatencyHistogram { counts: [i64; HIST_BUCKETS] }
impl LatencyHistogram {
    pub fn new() -> Self;
    pub fn record(&mut self, ms: f64);
    pub fn merge_counts(&mut self, other: &[i64]);
    pub fn counts(&self) -> Vec<i64>;
    pub fn from_counts(c: &[i64]) -> Self;       // pads/truncates to 56
    pub fn total(&self) -> i64;
    pub fn percentile(&self, q: f64) -> f64;     // geometric-midpoint interpolation
}
```

- [ ] **Step 1: failing tests first** (same file `#[cfg(test)]`): `hll_estimates_50k_within_3pct` (insert `u_{0..50_000}`, assert `(est-50_000).abs() < 1_500`), `hll_merge_equals_union`, `hll_roundtrip_bytes`, `hist_percentile_within_bucket_error` (record 1..=10_000 ms uniform, assert p50 within ±5% of 5 000), `hist_merge_adds`.
- [ ] **Step 2: implement** — hash `u64::from_be_bytes(sha2::Sha256::digest(item)[..8])`; `idx = (h >> 52) as usize`; `rho = ((h << 12).leading_zeros() + 1).min(53) as u8`; estimator `0.7213/(1+1.079/m) * m² / Σ2^-reg`, linear counting below `2.5·m` when zero registers exist. Histogram index: `ms < 1.0 → 0` else `min(55, (ms.log2()*2.0).floor() as usize + 1)`.
- [ ] **Step 3:** `cargo test -p sauron-db sketch` → all pass (pure tests, no DB gate).

### Task 3: `sauron-db::rollups` core — watermarks, gate, upserts

**Files:** Create `backend/crates/sauron-db/src/rollups/mod.rs`; modify `lib.rs`.

**Interfaces (produced):**
```rust
pub const SOURCES: [&str; 4]; // "analytics_events","error_events","transactions","sessions"
pub async fn epoch(conn) -> QueryResult<DateTime<Utc>>;
pub async fn watermark(conn, source: &str) -> QueryResult<DateTime<Utc>>;
pub async fn set_watermark(conn, source: &str, wm: DateTime<Utc>) -> QueryResult<()>; // inside caller's txn
pub async fn is_ready(conn, app_id: Uuid) -> QueryResult<bool>;
// marker EXISTS OR apps.created_at >= rollup_epoch.started_at
pub async fn as_of(conn, sources: &[&str]) -> QueryResult<Option<DateTime<Utc>>>; // min watermark
pub async fn mark_backfilled(conn, app_id) -> QueryResult<()>;
```
Plus additive upsert helpers used by fold (each `unnest` arrays, sorted+deduped): `add_screen_stats(conn, rows: &[ScreenStatDelta])` (RMW: `SELECT users_hll FOR UPDATE` on touched keys, merge in Rust, upsert `count+=`, `users_hll=EXCLUDED`), `add_journey_nodes`, `add_journey_links`, `add_event_top` (pure additive `count = t.count + EXCLUDED.count`), `add_user_activity` (RMW for both hlls), `add_perf_agg` (RMW for hist), `replace_session_days(conn, app-agnostic day recompute rows)`.

- [ ] **Step 1:** define delta structs (`ScreenStatDelta { app_id, env: Option<Uuid>, day: NaiveDate, screen, views, events, exceptions, users: Hll, dwell_ms: f64 }`, analogous others).
- [ ] **Step 2:** implement `is_ready` SQL:
```sql
SELECT EXISTS (SELECT 1 FROM rollup_backfill WHERE app_id = $1)
    OR EXISTS (SELECT 1 FROM apps a, rollup_epoch e WHERE a.id = $1 AND a.created_at >= e.started_at) AS present
```
- [ ] **Step 3:** `cargo check -p sauron-db` clean; unit-test `is_ready`/watermark round-trip inside Task 8's DB suite (no standalone test here).

### Task 4: fold — analytics source (screens, journeys, events/top, user activity)

**Files:** Create `backend/crates/sauron-db/src/rollups/fold.rs`.

**Interfaces (produced):**
```rust
pub struct FoldOutcome { pub rows_read: usize, pub new_watermark: DateTime<Utc> }
pub async fn fold_analytics(conn, upto: DateTime<Utc>) -> QueryResult<Option<FoldOutcome>>;
```
One `BEGIN`…`COMMIT` per call: read watermark; `SELECT app_id, environment_id, occurred_at, name, screen, distinct_id, session_id FROM analytics_events WHERE received_at > $1 AND received_at <= $2 ORDER BY app_id, session_id, occurred_at, id` (BRIN-pruned); in Rust build: event_top deltas (name cap: >`rollup_name_cap` distinct names per (app,day) → fold tail into `~other`, `tracing::warn!`), user_activity (`hll_all`+`hll_analytics`+events, skip `distinct_id = ''` for hlls), screen_stats views/events (+`users` hll), dwell via `rollup_session_state` (load state rows for touched sessions `FOR UPDATE`, walk each session's new events in `occurred_at` order: pending screen-view's dwell = `min(gap, 1_800_000)` credited to the pending event's day; `$screen` rows open a new pending; every row closes any pending; upsert state), journeys via `rollup_journey_state` (per (app, day(occurred_at), distinct_id, env-key): continue `steps` up to 10, node delta per step, link delta for step≥1 from `last_name`; skip regressing-`occurred_at` boundary pairs); apply upserts; `set_watermark`; `COMMIT`.

- [ ] **Step 1:** implement with an internal pure function `fn fold_analytics_rows(rows, state, name_cap) -> Deltas` so ordering/dwell/journey logic is unit-testable without PG.
- [ ] **Step 2:** pure unit tests: dwell terminated by next event and capped at 30 min; dwell survives a fold boundary via state; journey steps continue across folds and stop at 10; `~other` engages past the cap.
- [ ] **Step 3:** `cargo test -p sauron-db rollups::fold` → pure tests pass.

### Task 5: fold — errors, transactions, sessions

**Files:** Modify `backend/crates/sauron-db/src/rollups/fold.rs`.

**Interfaces (produced):**
```rust
pub async fn fold_errors(conn, upto) -> QueryResult<Option<FoldOutcome>>;
// screen_stats.exceptions + users hll (rows with screen IS NOT NULL), user_activity errors + hll_all
pub async fn fold_transactions(conn, upto) -> QueryResult<Option<FoldOutcome>>;
// perf_agg_hourly: count, error_count (status='error' OR http_status>=500), duration_sum, duration_hist
pub async fn recompute_recent_sessions(conn) -> QueryResult<usize>;
// REPLACE session_stats_daily for every (app, day) with sessions.last_event_at >= now()-36h:
// day = started_at::date; sessions count, crashed = unhandled_errors_count>0,
// duration = EXTRACT(EPOCH FROM last_event_at-started_at)*1000 → sum + hist + 5 fixed buckets;
// then set_watermark("sessions", now()) as the freshness stamp.
pub async fn rebuild_day(conn, day: NaiveDate, upto: DateTime<Utc>) -> QueryResult<()>;
// DELETE all 7 tables' rows for `day` (+ that day's journey/session state is NOT touched),
// re-aggregate that day from raw with received_at <= upto — used by backfill and consistency repair.
pub async fn consistency_check_yesterday(conn) -> QueryResult<Vec<String>>; // drifted descriptions
```
Session recompute is SQL-side for counts/fixed buckets (`GROUP BY app_id, environment_id, started_at::date`), Rust-side only for the log-histogram (pull `(app, env, day, duration_ms)`).

- [ ] **Step 1:** implement; `rebuild_day` reuses the same pure fold fns fed by a day-bounded raw read (`occurred_at >= day AND < day+1 AND received_at <= $upto`), building journey state from scratch for that day (its state keys carry the day) and computing dwell within-day only (cross-midnight dwell loss is a documented sliver).
- [ ] **Step 2:** `consistency_check_yesterday`: compare `count(*)` per (source, yesterday) vs `sum(event_top_daily.count)` / `sum(user_activity_daily.errors)` / `sum(perf_agg_hourly.count)`; >0.5% relative drift → include in result.
- [ ] **Step 3:** `cargo check -p sauron-db` clean.

### Task 6: aggregator task + config + kick

**Files:** Create `backend/crates/sauron-pipeline/src/rollup_task.rs`; modify `sauron-pipeline/src/lib.rs`, `sauron-core/src/config.rs`, `bins/sauron-ingest/src/main.rs`.

**Interfaces:** `pub fn spawn_rollup_task(pool: PgPool, redis: RedisStore, cfg: RollupCfg) -> JoinHandle<()>`; `Config` gains `rollup_fold_secs: u64 = 60`, `rollup_lag_secs: i64 = 60`, `rollup_kick_lag_secs: i64 = 2`, `rollup_name_cap: usize = 2000`. Redis keys: kick `"sauron:rollups:kick"` (API `set_ex`, task `get`+`del`), leader `"sauron:rollups:leader"` via `set_nx_ex(…, 90)` re-asserted each tick (house single-runner pattern, alerts engine precedent).

- [ ] **Step 1:** loop (merge-worker spawn shape, main.rs:536-556 neighborhood): every 2 s — assert/steal leadership (non-leader: sleep); read kick; if kick or `fold_secs` elapsed: `upto = now() - lag` (kick: `kick_lag`); run `fold_analytics`, `fold_errors`, `fold_transactions`, `recompute_recent_sessions` each with `warn!`+continue on error; once per UTC day: `consistency_check_yesterday` → each drifted day `rebuild_day` + `warn!`; prune state tables (`updated_at < now()-2 days`).
- [ ] **Step 2:** spawn in ingest main next to `spawn_merge_worker`.
- [ ] **Step 3:** `cargo build -p sauron-ingest` clean; boot locally against dev DB, observe one fold log line and `rollup_watermarks` advancing.

### Task 7: backfill + `sauron-migrate backfill-rollups`

**Files:** Modify `backend/crates/sauron-db/src/rollups/mod.rs` (backfill fns), `bins/sauron-migrate/src/main.rs`.

**Interfaces:** `pub async fn backfill_all(pool: &PgPool) -> anyhow::Result<()>` — iterate UTC days from `min(occurred_at)` (per firehose min, one probe query) to epoch day inclusive; per day `rebuild_day(conn, day, epoch)`; then sessions full recompute for ALL days (`replace_session_days` unbounded variant with `received/updated` ≤ epoch NOT applicable — sessions are mutable; recompute all days from current sessions table, documented); finally `mark_backfilled` for every app in the same final transaction batch (marker-in-txn rule, device_env_backfill:88 precedent). Idempotent: re-running REPLACEs identical values.

- [ ] **Step 1:** implement + arg in sauron-migrate main (`args().any(|a| a == "backfill-rollups")`, after migrations, log per-day progress).
- [ ] **Step 2:** `cargo build -p sauron-migrate` clean. (Dev-DB run happens in Task 14 — it is the Phase A gate, not a unit check.)

### Task 8: equivalence + boundary DB tests

**Files:** Create `backend/crates/sauron-db/tests/rollup_equivalence.rs` (uses `tests/common/mod.rs` TestDb).

- [ ] **Step 1:** test `fold_matches_legacy_queries`: seed one app, 3 envs, ~2 000 synthetic rows across 3 days with controlled `received_at` batches; run fold twice (split point mid-session, mid-journey); assert vs legacy repo fns on the same conn: `top_events` exact equality; `screen_list` views/events/exceptions exact, users within ±5%, dwell within ±1 ms; `journey_graph`-rollup nodes/links exact vs a per-day reference computed in the test; `performance_summary` count/error_rate exact, p50/p95 within one bucket ratio (±~4%); `session_stats` exact; `user_stats` dau/wau/mau within ±5% under calendar-day semantics.
- [ ] **Step 2:** test `fold_is_idempotent_at_watermark` (second fold with no new rows changes nothing) and `rebuild_day_matches_incremental` (rebuild a day, rows identical).
- [ ] **Step 3:** run with real env: `TEST_DATABASE_URL=… TEST_REDIS_URL=… cargo test -p sauron-db --test rollup_equivalence` via `dangerouslyDisableSandbox`; **assert wall time > 0.5 s** (0.00 s = silently skipped = failure).

### Task 9: read fns + gate branches — screens, counts, journeys

**Files:** Create `backend/crates/sauron-db/src/rollups/read.rs`; modify `repo.rs` (`screen_list` repo.rs:10650, `count_screens` repo.rs:19271, `journey_graph` repo.rs:9802).

**Interfaces:** `read::screens(conn, &ReadScope, Range, q_pattern, sort, limit, offset) -> Vec<repo::ScreenRow>`; `read::count_screens(...) -> (i64, bool)`; `read::journey(conn, &ReadScope, Range, depth) -> (Vec<JourneyNode>, Vec<JourneyLink>)`. Each existing repo fn branches at top: `if crate::rollups::is_ready(conn, scope.app_id).await? { return read::…; }` (device-groups gate shape, repo.rs:8125).

- [ ] **Step 1:** screens SQL: `SELECT screen, sum(views), sum(events), sum(exceptions), sum(dwell_ms_sum), array_agg(users_hll) FROM screen_stats_daily WHERE app_id=$1 AND day >= $2::date AND day <= $3::date{env} AND ($4 = '' OR screen ILIKE $4) GROUP BY screen` → merge hlls in Rust → sort per `SortSpec` → slice offset/limit → `ScreenRow { avg_dwell_ms: dwell_sum / views.max(1) }`. Count = same minus pagination, `(n.min(cap), n > cap)`.
- [ ] **Step 2:** journeys: nodes `SELECT step, name, sum(count) FROM journey_nodes_daily WHERE … AND step < $depth GROUP BY step, name ORDER BY step, 3 DESC LIMIT 500`; links analogous; wrap in existing structs.
- [ ] **Step 3:** `cargo clippy -p sauron-db -- -D warnings` clean; live sanity on dev DB after Task 14 backfill.

### Task 10: read fns + branches — perf, users, sessions, top-events, overview, active-users

**Files:** `rollups/read.rs`; `repo.rs` fns listed in File Structure; `bins/sauron-api/src/tier_read.rs:238`.

- [ ] **Step 1:** perf summary: pull matching `perf_agg_hourly` rows (`hour >= date_trunc('hour',$from) AND hour < $to?`, optional `($3 IS NULL OR name=$3)` idiom preserved), merge per (name,op) in Rust (hist merge → p50/p75/p95/p99; avg = duration_sum/count; error_rate), sort by count desc, truncate 100. Series: group by hour, p50/p95 + throughput from merged hists.
- [ ] **Step 2:** users: `user_stats` rollup branch — dau/wau/mau = merged `hll_all` over last 1/7/30 **calendar** days; active_in_range = merged `hll_all` over window days; new_in_range + total_users stay on their existing (small-table) queries; avg/median from `session_stats_daily` sums + hist. `active_user_series` = per-day `hll_all` estimate + existing new-users query. `tier_read::active_users_by_day`: if `is_ready` → per-day `hll_analytics` estimates, `partial_days: vec![]`, skipping the hot/cold split entirely.
- [ ] **Step 3:** sessions: stats = sums over `session_stats_daily` (median from hist); series = `duration_ms_sum/sessions` per day; histogram = the 5 fixed-bucket columns mapped to `HistoBucket` labels in the existing order.
- [ ] **Step 4:** `top_events` = `SELECT name, sum(count) FROM event_top_daily … GROUP BY name ORDER BY 2 DESC LIMIT $n`. `overview_totals`/`event_series`/`error_series` rollup branches read `user_activity_daily` (events/errors sums per day; users = merged hll; sessions/crash-free from `session_stats_daily`).
- [ ] **Step 5:** `cargo clippy --workspace -- -D warnings`; `cargo test -p sauron-api` (394-test suite) green with real runtime.

### Task 11: refresh route + `as_of`/`approx` + audit exemption

**Files:** Modify `bins/sauron-api/src/main.rs` (route), `routes/analytics.rs` (handler + response fields), `routes/screens.rs`, `routes/journeys.rs`, `routes/performance.rs` (add fields), `bins/sauron-api/tests/audit_coverage.rs`.

- [ ] **Step 1:** `POST /v1/apps/{app}/rollups/refresh` (auth identical to `overview_refresh`, analytics.rs:1071): `set_ex("sauron:rollups:kick","1",60)` then poll `rollups::as_of` every 300 ms up to 5 s for `as_of >= request_time - kick_lag`; respond `200 {"as_of": …, "caught_up": bool}`. EXEMPT entry: *"recomputes derived rollups; reads user data, mutates none — same class as overview_refresh"*.
- [ ] **Step 2:** rollup-served responses gain `as_of: Option<DateTime<Utc>>` + `approx: bool` (true iff rollup path taken): thread a small `RollupMeta` alongside existing payloads (each route already owns its response struct — add the two fields, `skip_serializing_if = "Option::is_none"` for `as_of`).
- [ ] **Step 3:** `cargo test -p sauron-api audit_coverage` passes; manual curl of refresh returns `caught_up: true` with aggregator running.

### Task 12: dashboard freshness UI + i18n

**Files:** Create `dashboard/src/lib/models/freshness.ts` + `.test.ts`, `dashboard/src/lib/api/rollups.ts`; modify `models/index.ts` (add `as_of?: string; approx?: boolean` to `ScreenRow`-page payloads' containers, `UsersAnalytics`, `SessionsAnalytics`, journey/perf wrappers), pages `ScreensList/JourneyExplorer/Performance/UsersExplorer/SessionsList/Events.svelte`, `i18n/catalog/time.ts`.

- [ ] **Step 1:** `freshness.ts` (pure): `asOfLabel(iso: string | undefined, now: Date) -> string | null` (uses `formatTime`), `approxTitle() -> string` (t-backed), `freshnessState(iso, now) -> 'fresh'|'stale'|null` (>5 min = stale tone). Vitest beside it.
- [ ] **Step 2:** `api/rollups.ts`: `refreshRollups(appId): Promise<{as_of: string; caught_up: boolean}>` POST.
- [ ] **Step 3:** each page: `Badge size="sm" tone="neutral"` chip next to the existing `RefreshButton` — `{t('time.asOf', { time })}` with `title={payload.as_of}`; numbers flagged `approx` render `≈` prefix + `aria-label`/`title` from `t('time.approxNote')`; `refresh()` now `await refreshRollups(aid).catch(() => {})` **then** the existing reload (fallback: plain reload when endpoint 404s on an old API).
- [ ] **Step 4:** catalog `time.ts`: `'time.asOf': { en: 'as of {time}', ar: 'حتى {time}' }`, `'time.approxNote': { en: 'Approximate (±~2%) — computed from sketches for speed at scale. Exact where shown without ≈.', ar: 'تقريبي (±~2%) — يُحسب من ملخصات إحصائية للسرعة على نطاق واسع. القيم بدون ≈ دقيقة.' }`.
- [ ] **Step 5:** `npm test` (vitest incl. new freshness tests) and `npm run check` clean.

### Task 13: tier interlock + docs + light seed mode

**Files:** Modify `bins/sauron-tier/src/main.rs` (`tier_table`, main.rs:142); create `docs/approximate-analytics.md`; modify `scripts/seed/10-dimensions.sql`, `scripts/seed/README.md`.

- [ ] **Step 1:** in `tier_table` before export loop: `let ro_wm = min over rollups::as_of(conn, [analytics|errors|transactions])`; skip (log `info!`) any partition whose `range.end > ro_wm` — a partition is exportable only when the aggregator has folded past its end.
- [ ] **Step 2:** `docs/approximate-analytics.md`: which figures are approximate, error bounds, calendar-day + day-scoped-journey semantics, refresh semantics; link target for the UI tooltip copy.
- [ ] **Step 3:** seed light mode: `\set` guard in `10-dimensions.sql` replacing the sampled template pools with minimal literals (~0.2 KB payloads) when `:light` = 1; README documents `-v light=1` and the 14-day × 10M/day Phase B recipe.

### Task 14: Phase A validation on the 10M dev instance

- [ ] **Step 1:** build workspace release binaries; run migration 71; run `sauron-migrate backfill-rollups` (expect minutes; per-day progress logs).
- [ ] **Step 2:** restart host `sauron-api` from new build; start ingest (host process) so the fold task runs; confirm one fold cycle + `rollup_watermarks` advancing and `SELECT count(*)` per rollup table sane.
- [ ] **Step 3:** re-run `measure.py` (52 endpoints ×3 samples); acceptance: screens/journeys/users/perf/sessions summaries all **< 500 ms**; keyset lists unchanged; refresh endpoint round-trips `caught_up: true`.
- [ ] **Step 4:** equivalence spot-check vs pre-rollup numbers saved in `results-tuned.json` (counts exact, distincts within ±2%); update `docs/benchmarks/2026-08-24-dashboard-latency-baseline.html` + artifact with the third column.

## Self-review

- Spec coverage: §5.1→T1, §5.2→T1/T4-7, §5.3→T2, §5.4→T9/T10, §5.5→T11/T12, §5.6→T13, §5.7 guarantees→T1 (BRIN only) + T6 (off-txn fold), §6 Phase A→T14, Phase B prep→T13. Sessions partitioning + Phase B/C runs = follow-ups by spec.
- Placeholders: none; every SQL/type named. Types cross-checked: `ScreenRow`/`JourneyNode`/`JourneyLink`/`PerfSummaryRow`/`PerfSeriesPoint`/`UserStats`/`UserSeriesPoint`/`SessionStats`/`SeriesAvgPoint`/`HistoBucket`/`EventCount` reused from repo.rs (T9/T10) match explorer-verified definitions.
- Known deviations from spec, deliberate: 7 rollup tables not 6 (journeys split into nodes+links; perf hourly not daily — series is hourly on the wire); `event_top_daily` drops `users_hll` (endpoint returns name+count only); project active-users stays raw (mutable identified split). Spec §5.1 table shapes said "refined in plan".
