# Postgres → Parquet Tiering (hot/cold) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Keep Postgres small and fast at any event volume by *moving* (not deleting) aged `error_events` rows out to compressed Parquet on local disk, while the dashboard transparently reads across both tiers.

**Architecture:** `error_events` becomes a RANGE-partitioned table on `occurred_at`. A new background binary `sauron-tier` periodically **copies** whole aged partitions to Parquet via DuckDB, **verifies** the row counts match, advances a per-table **watermark**, and only then **drops** the now-redundant Postgres partition (after a safety lag). A new `sauron-tier` crate holds the pure tiering logic (watermark split, path layout, partial-merge) plus an embedded DuckDB read engine. The dashboard API gains a **cross-tier query router** that splits a query window at the watermark, runs the hot (Postgres) and cold (Parquet) halves **concurrently** with `tokio::join!`, and glues the results. This is "approach A" (application-layer combine) with cold storage on a **local disk volume** behind a path abstraction. `error_events` is done end-to-end first; `analytics_events` and `transactions` are generalized last.

**Tech Stack:** Rust (Cargo workspace, axum 0.8, diesel-async 0.9 over `postgres_backend`, tokio), PostgreSQL 16 (declarative range partitioning), DuckDB (`duckdb` crate, bundled) reading/writing Parquet, Docker Compose.

## Execution Kickoff (start here in a fresh session)

**Status:** planned, not yet executed. No tiering commits exist. Base commit = `6dec107`.

**Already on disk (from the planning session):** Task 1's crate is scaffolded and matches this plan — `backend/crates/sauron-tier/{Cargo.toml, src/lib.rs, src/plan.rs}` (untracked). It is NOT yet tested, committed, or wired into `[workspace.dependencies]`. Task 1's implementer only needs to run `cargo test -p sauron-tier --lib plan`, then `git add` those three files and commit.

**How to run it:** in a new session, invoke the `superpowers:subagent-driven-development` skill (or `superpowers:executing-plans`) pointed at this plan. It reads the ledger at `.superpowers/sdd/progress.md` and resumes at the first unchecked task.

**Decisions carried from planning (change if you want):**
- Work directly on `main` (user chose). Do NOT create a branch unless you decide otherwise.
- These 6 pre-dirty, UNRELATED files must stay uncommitted — implementers `git add` only their own task files, never `git add -A`: `backend/bins/sauron-api/src/routes/auth.rs`, `backend/crates/sauron-auth/src/extractors.rs`, `dashboard/src/pages/MonitorDetail.svelte`, `dashboard/src/pages/Monitors.svelte`, `examples/flutter-app/test/widget_test.dart`, `examples/svelte-web/package.json`.
- e2e cadence: implementers do compile + `cargo test` only; the controller runs docker-compose e2e at checkpoints (after Tasks 6, 7, 9, and final). Docker Compose v5.2.0 is available in this environment.
- No DB test harness exists: Tasks 1–4 have real `cargo test`; Tasks 5–10 are verified via controller e2e — hand reviewers this constraint so they don't false-flag missing unit tests.

---

## Global Constraints

- **Never delete event data.** The `DROP … PARTITION` step drops the *Postgres copy only after* the Parquet copy is written and row-count-verified. Retention/deletion of cold data stays permanently OFF. At every instant every row exists in ≥1 tier.
- **Exactly-once tier boundary.** Hot filters use `occurred_at >= watermark`; cold filters use `occurred_at < watermark`. Ordering is always: write Parquet → verify → advance watermark → (after lag) drop partition. Never advance the watermark before Parquet is verified; never drop a partition that is not strictly below the watermark.
- **Cold storage = local disk** at `TIER_COLD_PATH` (default `/var/lib/sauron/cold`), behind a path helper so S3 can replace it later. Once a partition is dropped from PG, its Parquet is the sole copy — the volume must be backed up like `pgdata`.
- **Isolation of the native dep.** The `duckdb` crate (and its bundled native lib) lives ONLY in `sauron-tier` and, transitively, `sauron-api` (read path) and the `sauron-tier` bin. `sauron-ingest` MUST NOT depend on `sauron-tier` — the hot ingest path stays DuckDB-free.
- **Workspace conventions (verbatim):** edition `2021`, rust-version `1.82`, license `AGPL-3.0-only`, `version = "0.1.0"`. Enum-like columns are `TEXT` (not PG enums). Diesel uses `postgres_backend` (no libpq linked into Rust). All DB I/O goes through `&mut AsyncPgConnection`. Config is hand-rolled in `sauron-core::config` via `var()`/`parse()`.
- **Testing reality (IMPORTANT):** this repo has **no DB/handler integration-test harness** — only pure `#[cfg(test)]` unit tests. Therefore: pure logic (Tasks 1–3) and the self-contained DuckDB-over-Parquet engine (Task 4) get real `cargo test` TDD cycles; everything that touches Postgres or the running system (Tasks 5–10) is verified via a **docker-compose e2e script with expected output**, which stands in for the "test" step. Do not invent a DB harness; follow this split.
- **Migration numbering** continues the existing sequence; next free ids are `2026-07-14-000010` and `2026-07-14-000011`. Migrations are embedded via `embed_migrations!("../../migrations")` and applied by the `migrate` compose service.

---

## File Structure

**New crate `backend/crates/sauron-tier/`** (no diesel dependency; pure logic + DuckDB):
- `Cargo.toml` — deps: `chrono`, `uuid`, `anyhow`, `thiserror`, `tracing`, `duckdb` (bundled).
- `src/lib.rs` — module wiring + `TieredTable` / `TIERED_TABLES`.
- `src/plan.rs` — `TimeRange`, `TierPlan`, `plan()` (watermark split). Pure. (Task 1)
- `src/layout.rs` — `Granularity`, `bucket_bounds()`, `partition_suffix()`, `cold_partition_glob()`, `cold_copy_dir()`. Pure. (Task 2)
- `src/merge.rs` — `DayCount`, `merge_day_counts()`. Pure. (Task 3)
- `src/duck.rs` — `DuckEngine` (open, `error_counts_by_day`, `count_parquet_rows`, `count_range`, `export_from_postgres`). (Task 4, 7)

**Modified `backend/crates/sauron-db/`** (diesel side):
- `src/repo.rs` — add watermark fns + partition-maintenance fns + hot `error_counts_by_day_hot`. (Tasks 5, 6, 8)
- `Cargo.toml` — unchanged.

**Modified `backend/crates/sauron-core/src/config.rs`** — add `tier_*` fields. (Task 7)

**New `backend/bins/sauron-tier/`** — the archival worker binary. (Task 7)

**New migrations:**
- `backend/migrations/2026-07-14-000010_tiering_state/{up,down}.sql` (Task 5)
- `backend/migrations/2026-07-14-000011_error_events_partitioned/{up,down}.sql` (Task 6)

**Modified `backend/bins/sauron-api/`:**
- `src/tier_read.rs` (new) — cross-tier router. (Task 8)
- `src/routes/` + `src/main.rs` — register the timeseries endpoint. (Task 8)
- `Cargo.toml` — add `sauron-tier` dep.

**Modified `docker-compose.yml`** + `backend/Dockerfile` — `tier` service, `colddata` volume, libpq runtime for DuckDB's postgres extension. (Task 9)

**Generalization:** migrations `…000012_analytics_events_partitioned`, `…000013_transactions_partitioned`, plus `TIERED_TABLES` + cold-read additions. (Task 10)

---

## Task 1: `sauron-tier` crate + watermark split logic

**Files:**
- Create: `backend/crates/sauron-tier/Cargo.toml`
- Create: `backend/crates/sauron-tier/src/lib.rs`
- Create: `backend/crates/sauron-tier/src/plan.rs`

**Interfaces:**
- Produces: `sauron_tier::TimeRange { start: DateTime<Utc>, end: DateTime<Utc> }` (half-open `[start, end)`); `sauron_tier::TierPlan { hot: Option<TimeRange>, cold: Option<TimeRange> }`; `pub fn plan(watermark: DateTime<Utc>, from: DateTime<Utc>, to: DateTime<Utc>) -> TierPlan` where `from`/`to` are the half-open query window `[from, to)`.

- [ ] **Step 1: Create the crate manifest**

Create `backend/crates/sauron-tier/Cargo.toml`:

```toml
[package]
name = "sauron-tier"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
chrono.workspace = true
uuid.workspace = true
anyhow.workspace = true
thiserror.workspace = true
tracing.workspace = true
duckdb = { version = "1", features = ["bundled"] }
```

- [ ] **Step 2: Create `src/lib.rs` with module wiring**

```rust
//! `sauron-tier` — hot/cold tiering: pure planning/layout/merge logic plus an
//! embedded DuckDB engine for reading and writing Parquet cold storage. No
//! diesel here; the `sauron-tier` binary glues this to `sauron-db`.

pub mod duck;
pub mod layout;
pub mod merge;
pub mod plan;

pub use layout::{bucket_bounds, cold_copy_dir, cold_partition_glob, partition_suffix, Granularity};
pub use merge::{merge_day_counts, DayCount};
pub use plan::{plan, TierPlan, TimeRange};

/// A table that participates in tiering, keyed on its time column.
#[derive(Debug, Clone, Copy)]
pub struct TieredTable {
    pub name: &'static str,
    pub time_col: &'static str,
}

/// Tables tiered out to Parquet. `error_events` first; more added in Task 10.
pub const TIERED_TABLES: &[TieredTable] = &[TieredTable {
    name: "error_events",
    time_col: "occurred_at",
}];
```

(Note: `duck`, `layout`, `merge` modules are created in later tasks. To let this task compile and test on its own, temporarily comment out the `pub mod duck; pub mod layout; pub mod merge;` lines and their `pub use` lines, then re-enable them as each module lands. Leave `pub mod plan;` and its `pub use` active now.)

- [ ] **Step 3: Write the failing test for `plan`**

Create `backend/crates/sauron-tier/src/plan.rs`:

```rust
//! Watermark split: given a query window and the tier watermark, decide which
//! sub-range is served hot (Postgres, `occurred_at >= watermark`) and which is
//! served cold (Parquet, `occurred_at < watermark`). Half-open ranges.

use chrono::{DateTime, Utc};

/// Half-open time range `[start, end)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeRange {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

/// Which tiers a query window touches, with the exact sub-range for each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TierPlan {
    pub hot: Option<TimeRange>,
    pub cold: Option<TimeRange>,
}

/// Split the half-open window `[from, to)` at `watermark`.
/// Everything `< watermark` is cold; everything `>= watermark` is hot. The two
/// sub-ranges are complementary → no overlap, no gap (exactly-once).
pub fn plan(watermark: DateTime<Utc>, from: DateTime<Utc>, to: DateTime<Utc>) -> TierPlan {
    if to <= from {
        return TierPlan { hot: None, cold: None };
    }
    let cold = if from < watermark {
        Some(TimeRange { start: from, end: to.min(watermark) })
    } else {
        None
    };
    let hot = if to > watermark {
        Some(TimeRange { start: from.max(watermark), end: to })
    } else {
        None
    };
    TierPlan { hot, cold }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t(y: i32, mo: u32, d: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, 0, 0, 0).unwrap()
    }

    #[test]
    fn fully_hot_when_window_after_watermark() {
        let p = plan(t(2026, 6, 1), t(2026, 6, 10), t(2026, 6, 20));
        assert_eq!(p.hot, Some(TimeRange { start: t(2026, 6, 10), end: t(2026, 6, 20) }));
        assert_eq!(p.cold, None);
    }

    #[test]
    fn fully_cold_when_window_before_watermark() {
        let p = plan(t(2026, 6, 1), t(2026, 5, 1), t(2026, 5, 20));
        assert_eq!(p.cold, Some(TimeRange { start: t(2026, 5, 1), end: t(2026, 5, 20) }));
        assert_eq!(p.hot, None);
    }

    #[test]
    fn straddle_splits_at_watermark_with_no_overlap() {
        let p = plan(t(2026, 6, 1), t(2026, 5, 15), t(2026, 6, 15));
        assert_eq!(p.cold, Some(TimeRange { start: t(2026, 5, 15), end: t(2026, 6, 1) }));
        assert_eq!(p.hot, Some(TimeRange { start: t(2026, 6, 1), end: t(2026, 6, 15) }));
    }

    #[test]
    fn boundary_exactly_at_watermark_is_hot_side_empty() {
        // window [from, watermark): entirely cold, hot omitted (to == watermark).
        let p = plan(t(2026, 6, 1), t(2026, 5, 1), t(2026, 6, 1));
        assert_eq!(p.cold, Some(TimeRange { start: t(2026, 5, 1), end: t(2026, 6, 1) }));
        assert_eq!(p.hot, None);
    }

    #[test]
    fn empty_or_inverted_window_yields_nothing() {
        assert_eq!(plan(t(2026, 6, 1), t(2026, 6, 5), t(2026, 6, 5)), TierPlan { hot: None, cold: None });
        assert_eq!(plan(t(2026, 6, 1), t(2026, 6, 9), t(2026, 6, 5)), TierPlan { hot: None, cold: None });
    }
}
```

- [ ] **Step 4: Register the crate is picked up (workspace already globs `crates/*`)**

Run: `cd backend && cargo test -p sauron-tier --lib`
Expected: FAIL to compile first if modules are mis-wired, then after commenting out not-yet-created modules per Step 2 note, tests run.

- [ ] **Step 5: Run the tests to green**

Run: `cd backend && cargo test -p sauron-tier --lib plan`
Expected: PASS (5 tests in `plan::tests`).

- [ ] **Step 6: Commit**

```bash
git add backend/crates/sauron-tier/Cargo.toml backend/crates/sauron-tier/src/lib.rs backend/crates/sauron-tier/src/plan.rs
git commit -m "feat(tier): sauron-tier crate + watermark split logic"
```

---

## Task 2: Partition-boundary + cold-path layout (pure)

**Files:**
- Create: `backend/crates/sauron-tier/src/layout.rs`
- Modify: `backend/crates/sauron-tier/src/lib.rs` (enable `pub mod layout;` + `pub use`)

**Interfaces:**
- Consumes: `TimeRange` from Task 1.
- Produces:
  - `enum Granularity { Day, Week, Month }`, `Granularity::from_str_or(default)`.
  - `fn bucket_bounds(ts: DateTime<Utc>, g: Granularity) -> TimeRange` — the partition `[start, end)` containing `ts`.
  - `fn partition_suffix(start: DateTime<Utc>) -> String` — e.g. `"2026_05_01"`, used to name PG child partitions.
  - `fn cold_copy_dir(base: &str, table: &str) -> String` — the directory DuckDB `COPY … PARTITION_BY` writes under.
  - `fn cold_partition_glob(base: &str, table: &str, app_id: Uuid) -> String` — the read glob for one app across all its cold Parquet.

- [ ] **Step 1: Write the failing tests + implementation**

Create `backend/crates/sauron-tier/src/layout.rs`:

```rust
//! Partition boundary math and cold-storage path layout. The cold path uses a
//! FIXED hive pruning key (`app_id`, `year`, `month`) regardless of partition
//! granularity, so changing day/week/month later never breaks read globs.

use chrono::{DateTime, Datelike, Duration, TimeZone, Utc};
use uuid::Uuid;

use crate::plan::TimeRange;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Granularity {
    Day,
    Week,
    Month,
}

impl Granularity {
    /// Parse `"day" | "week" | "month"`, falling back to `default` for anything else.
    pub fn from_str_or(s: &str, default: Granularity) -> Granularity {
        match s.to_ascii_lowercase().as_str() {
            "day" => Granularity::Day,
            "week" => Granularity::Week,
            "month" => Granularity::Month,
            _ => default,
        }
    }
}

fn start_of_day(ts: DateTime<Utc>) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(ts.year(), ts.month(), ts.day(), 0, 0, 0).unwrap()
}

/// The partition `[start, end)` that contains `ts` at the given granularity.
pub fn bucket_bounds(ts: DateTime<Utc>, g: Granularity) -> TimeRange {
    match g {
        Granularity::Day => {
            let start = start_of_day(ts);
            TimeRange { start, end: start + Duration::days(1) }
        }
        Granularity::Week => {
            // ISO week: Monday start.
            let sod = start_of_day(ts);
            let dow = sod.weekday().num_days_from_monday() as i64;
            let start = sod - Duration::days(dow);
            TimeRange { start, end: start + Duration::days(7) }
        }
        Granularity::Month => {
            let start = Utc.with_ymd_and_hms(ts.year(), ts.month(), 1, 0, 0, 0).unwrap();
            let (ny, nm) = if ts.month() == 12 { (ts.year() + 1, 1) } else { (ts.year(), ts.month() + 1) };
            let end = Utc.with_ymd_and_hms(ny, nm, 1, 0, 0, 0).unwrap();
            TimeRange { start, end }
        }
    }
}

/// PG child-partition name suffix, from the partition start. `2026-05-01` → `2026_05_01`.
pub fn partition_suffix(start: DateTime<Utc>) -> String {
    format!("{:04}_{:02}_{:02}", start.year(), start.month(), start.day())
}

/// Directory DuckDB writes the cold Parquet under (hive-partitioned inside).
pub fn cold_copy_dir(base: &str, table: &str) -> String {
    format!("{}/{}", base.trim_end_matches('/'), table)
}

/// Read glob for all of one app's cold Parquet for `table`.
pub fn cold_partition_glob(base: &str, table: &str, app_id: Uuid) -> String {
    format!(
        "{}/{}/app_id={}/year=*/month=*/*.parquet",
        base.trim_end_matches('/'),
        table,
        app_id
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(y: i32, mo: u32, d: u32, h: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, 30, 0).unwrap()
    }

    #[test]
    fn day_bounds() {
        let b = bucket_bounds(t(2026, 5, 15, 13), Granularity::Day);
        assert_eq!(b.start, Utc.with_ymd_and_hms(2026, 5, 15, 0, 0, 0).unwrap());
        assert_eq!(b.end, Utc.with_ymd_and_hms(2026, 5, 16, 0, 0, 0).unwrap());
    }

    #[test]
    fn week_starts_monday() {
        // 2026-05-15 is a Friday → week starts Mon 2026-05-11.
        let b = bucket_bounds(t(2026, 5, 15, 13), Granularity::Week);
        assert_eq!(b.start, Utc.with_ymd_and_hms(2026, 5, 11, 0, 0, 0).unwrap());
        assert_eq!(b.end, Utc.with_ymd_and_hms(2026, 5, 18, 0, 0, 0).unwrap());
    }

    #[test]
    fn month_bounds_roll_over_year() {
        let b = bucket_bounds(t(2026, 12, 20, 9), Granularity::Month);
        assert_eq!(b.start, Utc.with_ymd_and_hms(2026, 12, 1, 0, 0, 0).unwrap());
        assert_eq!(b.end, Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).unwrap());
    }

    #[test]
    fn suffix_and_paths() {
        let start = Utc.with_ymd_and_hms(2026, 5, 1, 0, 0, 0).unwrap();
        assert_eq!(partition_suffix(start), "2026_05_01");
        assert_eq!(cold_copy_dir("/cold/", "error_events"), "/cold/error_events");
        let app = Uuid::nil();
        assert_eq!(
            cold_partition_glob("/cold", "error_events", app),
            format!("/cold/error_events/app_id={}/year=*/month=*/*.parquet", app)
        );
    }

    #[test]
    fn granularity_parse() {
        assert_eq!(Granularity::from_str_or("WEEK", Granularity::Day), Granularity::Week);
        assert_eq!(Granularity::from_str_or("nonsense", Granularity::Day), Granularity::Day);
    }
}
```

- [ ] **Step 2: Enable the module**

In `backend/crates/sauron-tier/src/lib.rs`, ensure these lines are active (uncomment if you stubbed them in Task 1):

```rust
pub mod layout;
pub use layout::{bucket_bounds, cold_copy_dir, cold_partition_glob, partition_suffix, Granularity};
```

- [ ] **Step 3: Run tests to verify they pass**

Run: `cd backend && cargo test -p sauron-tier --lib layout`
Expected: PASS (5 tests in `layout::tests`).

- [ ] **Step 4: Commit**

```bash
git add backend/crates/sauron-tier/src/layout.rs backend/crates/sauron-tier/src/lib.rs
git commit -m "feat(tier): partition-boundary + cold-path layout"
```

---

## Task 3: Partial-merge glue (pure)

**Files:**
- Create: `backend/crates/sauron-tier/src/merge.rs`
- Modify: `backend/crates/sauron-tier/src/lib.rs` (enable `pub mod merge;` + `pub use`)

**Interfaces:**
- Produces: `struct DayCount { day: NaiveDate, count: i64 }`; `fn merge_day_counts(hot: Vec<DayCount>, cold: Vec<DayCount>) -> Vec<DayCount>` — sums counts by day and returns them sorted ascending. This is the additive-aggregate glue for the cross-tier router (Task 8).

- [ ] **Step 1: Write the failing tests + implementation**

Create `backend/crates/sauron-tier/src/merge.rs`:

```rust
//! Merge additive per-day partials from the hot and cold tiers into one series.
//! Days are usually disjoint across the watermark, but a watermark mid-day can
//! put the same day in both tiers, so we sum by day rather than concatenate.

use std::collections::BTreeMap;

use chrono::NaiveDate;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayCount {
    pub day: NaiveDate,
    pub count: i64,
}

/// Sum `hot` and `cold` per-day counts into one ascending-by-day series.
pub fn merge_day_counts(hot: Vec<DayCount>, cold: Vec<DayCount>) -> Vec<DayCount> {
    let mut acc: BTreeMap<NaiveDate, i64> = BTreeMap::new();
    for dc in hot.into_iter().chain(cold.into_iter()) {
        *acc.entry(dc.day).or_insert(0) += dc.count;
    }
    acc.into_iter().map(|(day, count)| DayCount { day, count }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn disjoint_days_concatenate_sorted() {
        let hot = vec![DayCount { day: d(2026, 6, 2), count: 5 }, DayCount { day: d(2026, 6, 1), count: 3 }];
        let cold = vec![DayCount { day: d(2026, 5, 30), count: 9 }];
        let out = merge_day_counts(hot, cold);
        assert_eq!(
            out,
            vec![
                DayCount { day: d(2026, 5, 30), count: 9 },
                DayCount { day: d(2026, 6, 1), count: 3 },
                DayCount { day: d(2026, 6, 2), count: 5 },
            ]
        );
    }

    #[test]
    fn same_day_in_both_tiers_is_summed() {
        let hot = vec![DayCount { day: d(2026, 6, 1), count: 4 }];
        let cold = vec![DayCount { day: d(2026, 6, 1), count: 6 }];
        assert_eq!(merge_day_counts(hot, cold), vec![DayCount { day: d(2026, 6, 1), count: 10 }]);
    }

    #[test]
    fn empty_sides() {
        assert_eq!(merge_day_counts(vec![], vec![]), vec![]);
        let only = vec![DayCount { day: d(2026, 6, 1), count: 1 }];
        assert_eq!(merge_day_counts(only.clone(), vec![]), only);
    }
}
```

- [ ] **Step 2: Enable the module in `lib.rs`**

```rust
pub mod merge;
pub use merge::{merge_day_counts, DayCount};
```

- [ ] **Step 3: Run tests**

Run: `cd backend && cargo test -p sauron-tier --lib merge`
Expected: PASS (3 tests).

- [ ] **Step 4: Commit**

```bash
git add backend/crates/sauron-tier/src/merge.rs backend/crates/sauron-tier/src/lib.rs
git commit -m "feat(tier): additive per-day partial-merge glue"
```

---

## Task 4: DuckDB read engine over Parquet

**Files:**
- Create: `backend/crates/sauron-tier/src/duck.rs`
- Modify: `backend/crates/sauron-tier/src/lib.rs` (enable `pub mod duck;`)

**Interfaces:**
- Consumes: `DayCount` (Task 3), `cold_partition_glob` (Task 2).
- Produces: `struct DuckEngine`; `DuckEngine::open() -> anyhow::Result<DuckEngine>`; `DuckEngine::count_parquet_rows(&self, glob: &str) -> anyhow::Result<i64>`; `DuckEngine::error_counts_by_day(&self, glob: &str, app_id: Uuid, from: DateTime<Utc>, to: DateTime<Utc>) -> anyhow::Result<Vec<DayCount>>`. (Write/export methods are added in Task 7.)

- [ ] **Step 1: Write a self-contained integration test (writes Parquet, reads it back)**

Create `backend/crates/sauron-tier/src/duck.rs`:

```rust
//! Embedded DuckDB engine. Read path over cold Parquet (this task); Postgres→
//! Parquet export is added in Task 7. DuckDB is synchronous — callers on an
//! async runtime must invoke these from `spawn_blocking`.

use anyhow::Context;
use chrono::{DateTime, NaiveDate, Utc};
use duckdb::Connection;
use uuid::Uuid;

use crate::merge::DayCount;

pub struct DuckEngine {
    conn: Connection,
}

impl DuckEngine {
    /// Open an in-memory DuckDB. Parquet is read directly from the filesystem;
    /// no persistent DuckDB database file is used.
    pub fn open() -> anyhow::Result<Self> {
        let conn = Connection::open_in_memory().context("open duckdb")?;
        // Bound memory so many concurrent cold reads can't OOM the process.
        conn.execute_batch("SET memory_limit='512MB'; SET threads=4;")?;
        Ok(Self { conn })
    }

    /// Total rows across the Parquet matched by `glob`. Returns 0 if no files match.
    pub fn count_parquet_rows(&self, glob: &str) -> anyhow::Result<i64> {
        // `union_by_name` + `hive_partitioning` tolerate schema evolution and
        // read the app_id/year/month partition columns from the paths.
        let sql = "SELECT count(*) FROM read_parquet(?, hive_partitioning=true, union_by_name=true)";
        let mut stmt = self.conn.prepare(sql)?;
        let n: i64 = stmt
            .query_row([glob], |r| r.get(0))
            .or_else(|e| match e {
                duckdb::Error::QueryReturnedNoRows => Ok(0),
                other => Err(other),
            })
            .context("count_parquet_rows")?;
        Ok(n)
    }

    /// Per-day error counts for one app in `[from, to)` read from cold Parquet.
    pub fn error_counts_by_day(
        &self,
        glob: &str,
        app_id: Uuid,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> anyhow::Result<Vec<DayCount>> {
        let sql = "\
            SELECT CAST(occurred_at AS DATE) AS day, count(*) AS cnt \
            FROM read_parquet(?, hive_partitioning=true, union_by_name=true) \
            WHERE app_id = ? AND occurred_at >= ? AND occurred_at < ? \
            GROUP BY 1 ORDER BY 1";
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(
            duckdb::params![glob, app_id.to_string(), from.to_rfc3339(), to.to_rfc3339()],
            |r| {
                let day: NaiveDate = r.get(0)?;
                let cnt: i64 = r.get(1)?;
                Ok(DayCount { day, count: cnt })
            },
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_then_read_counts_by_day() {
        let dir = std::env::temp_dir().join(format!("sauron-tier-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let app = Uuid::new_v4();

        // Write a small hive-partitioned Parquet dataset the same way the export
        // job will (PARTITION_BY app_id, year, month).
        let eng = DuckEngine::open().unwrap();
        let copy = format!(
            "COPY (SELECT * FROM (VALUES \
               ('{a}'::UUID, TIMESTAMPTZ '2026-05-01 10:00:00+00'), \
               ('{a}'::UUID, TIMESTAMPTZ '2026-05-01 11:00:00+00'), \
               ('{a}'::UUID, TIMESTAMPTZ '2026-05-02 09:00:00+00') \
             ) AS v(app_id, occurred_at) \
             SELECT app_id, occurred_at, year(occurred_at) AS year, month(occurred_at) AS month) \
             TO '{d}/error_events' (FORMAT PARQUET, PARTITION_BY (app_id, year, month), APPEND)",
            a = app,
            d = dir.display()
        );
        eng.conn.execute_batch(&copy).unwrap();

        let glob = cold_glob(&dir.display().to_string(), app);
        assert_eq!(eng.count_parquet_rows(&glob).unwrap(), 3);

        let from = "2026-05-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let to = "2026-06-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let series = eng.error_counts_by_day(&glob, app, from, to).unwrap();
        assert_eq!(series.len(), 2);
        assert_eq!(series[0].count, 2); // 2026-05-01
        assert_eq!(series[1].count, 1); // 2026-05-02

        std::fs::remove_dir_all(&dir).ok();
    }

    fn cold_glob(base: &str, app: Uuid) -> String {
        crate::layout::cold_partition_glob(base, "error_events", app)
    }
}
```

- [ ] **Step 2: Enable the module in `lib.rs`**

```rust
pub mod duck;
```

- [ ] **Step 3: Run the test to verify it fails, then passes**

Run: `cd backend && cargo test -p sauron-tier --lib duck`
Expected: first build compiles the bundled DuckDB (slow, minutes). Then PASS (1 test). If `APPEND` is rejected by your DuckDB version, use `OVERWRITE_OR_IGNORE` in the test COPY (and note it for Task 7); the read assertions are unchanged.

- [ ] **Step 4: Commit**

```bash
git add backend/crates/sauron-tier/src/duck.rs backend/crates/sauron-tier/src/lib.rs
git commit -m "feat(tier): embedded DuckDB read engine over cold Parquet"
```

---

## Task 5: `tiering_state` migration + watermark repo functions

**Files:**
- Create: `backend/migrations/2026-07-14-000010_tiering_state/up.sql`
- Create: `backend/migrations/2026-07-14-000010_tiering_state/down.sql`
- Modify: `backend/crates/sauron-db/src/repo.rs` (append a "Tiering" section)
- Modify: `backend/crates/sauron-db/src/schema.rs` (add `tiering_state` table — regenerate or hand-add)

**Interfaces:**
- Produces (repo, all `&mut AsyncPgConnection`):
  - `get_watermark(conn, table: &str) -> QueryResult<Option<DateTime<Utc>>>`
  - `advance_watermark(conn, table: &str, wm: DateTime<Utc>) -> QueryResult<()>` (upsert; never moves backward)
  - `get_dropped_thru(conn, table: &str) -> QueryResult<Option<DateTime<Utc>>>`
  - `set_dropped_thru(conn, table: &str, t: DateTime<Utc>) -> QueryResult<()>`

- [ ] **Step 1: Write the migration `up.sql`**

Create `backend/migrations/2026-07-14-000010_tiering_state/up.sql`:

```sql
-- 0010: tiering watermark, one row per tiered table.
--   watermark    — everything with occurred_at < watermark is durably in Parquet.
--   dropped_thru — everything with occurred_at < dropped_thru has been dropped
--                  from Postgres. Always dropped_thru <= watermark (drop lags
--                  export), so a slightly stale cached watermark never gaps.
CREATE TABLE tiering_state (
    table_name   TEXT PRIMARY KEY,
    watermark    TIMESTAMPTZ NOT NULL,
    dropped_thru TIMESTAMPTZ,
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
```

- [ ] **Step 2: Write the migration `down.sql`**

Create `backend/migrations/2026-07-14-000010_tiering_state/down.sql`:

```sql
DROP TABLE IF EXISTS tiering_state;
```

- [ ] **Step 3: Add the table to `schema.rs`**

Add this `diesel::table!` block to `backend/crates/sauron-db/src/schema.rs` (and add `tiering_state` to the `allow_tables_to_appear_in_same_query!` list):

```rust
diesel::table! {
    tiering_state (table_name) {
        table_name -> Text,
        watermark -> Timestamptz,
        dropped_thru -> Nullable<Timestamptz>,
        updated_at -> Timestamptz,
    }
}
```

- [ ] **Step 4: Add the repo functions**

Append to `backend/crates/sauron-db/src/repo.rs` (after the issues/error-events section):

```rust
// ===========================================================================
// Tiering (hot/cold watermark)
// ===========================================================================

pub async fn get_watermark(
    conn: &mut AsyncPgConnection,
    table: &str,
) -> QueryResult<Option<DateTime<Utc>>> {
    tiering_state::table
        .find(table)
        .select(tiering_state::watermark)
        .first(conn)
        .await
        .optional()
}

/// Upsert the watermark; never moves it backward.
pub async fn advance_watermark(
    conn: &mut AsyncPgConnection,
    table: &str,
    wm: DateTime<Utc>,
) -> QueryResult<()> {
    diesel::insert_into(tiering_state::table)
        .values((
            tiering_state::table_name.eq(table),
            tiering_state::watermark.eq(wm),
            tiering_state::updated_at.eq(Utc::now()),
        ))
        .on_conflict(tiering_state::table_name)
        .do_update()
        .set((
            tiering_state::watermark.eq(diesel::dsl::sql::<Timestamptz>("GREATEST(tiering_state.watermark, EXCLUDED.watermark)")),
            tiering_state::updated_at.eq(Utc::now()),
        ))
        .execute(conn)
        .await?;
    Ok(())
}

pub async fn get_dropped_thru(
    conn: &mut AsyncPgConnection,
    table: &str,
) -> QueryResult<Option<DateTime<Utc>>> {
    tiering_state::table
        .find(table)
        .select(tiering_state::dropped_thru)
        .first::<Option<DateTime<Utc>>>(conn)
        .await
        .optional()
        .map(|o| o.flatten())
}

pub async fn set_dropped_thru(
    conn: &mut AsyncPgConnection,
    table: &str,
    t: DateTime<Utc>,
) -> QueryResult<()> {
    diesel::update(tiering_state::table.find(table))
        .set((
            tiering_state::dropped_thru.eq(Some(t)),
            tiering_state::updated_at.eq(Utc::now()),
        ))
        .execute(conn)
        .await?;
    Ok(())
}
```

- [ ] **Step 5: Compile check**

Run: `cd backend && cargo build -p sauron-db`
Expected: compiles. If the `sql::<Timestamptz>` GREATEST expression trips the type checker, replace the conflict `.set` with a plain `tiering_state::watermark.eq(excluded(tiering_state::watermark))` and enforce monotonicity in the caller (the bin only ever advances forward anyway).

- [ ] **Step 6: e2e verify (stands in for a unit test — no DB harness)**

Run against a running compose DB:

```bash
docker compose up -d postgres migrate
docker compose exec -T postgres psql -U sauron -d sauron -c \
  "INSERT INTO tiering_state(table_name, watermark) VALUES ('error_events', '2026-06-01T00:00:00Z') \
   ON CONFLICT (table_name) DO UPDATE SET watermark = GREATEST(tiering_state.watermark, EXCLUDED.watermark); \
   SELECT table_name, watermark, dropped_thru FROM tiering_state;"
```

Expected: one row `error_events | 2026-06-01 00:00:00+00 | (null)`.

- [ ] **Step 7: Commit**

```bash
git add backend/migrations/2026-07-14-000010_tiering_state backend/crates/sauron-db/src/schema.rs backend/crates/sauron-db/src/repo.rs
git commit -m "feat(tier): tiering_state table + watermark repo fns"
```

---

## Task 6: Partition `error_events` + partition-maintenance repo functions

**Files:**
- Create: `backend/migrations/2026-07-14-000011_error_events_partitioned/up.sql`
- Create: `backend/migrations/2026-07-14-000011_error_events_partitioned/down.sql`
- Modify: `backend/crates/sauron-db/src/repo.rs` (partition-maintenance fns)

**Interfaces:**
- Produces (repo):
  - `create_range_partition(conn, table: &str, suffix: &str, start: DateTime<Utc>, end: DateTime<Utc>) -> QueryResult<()>` (idempotent `CREATE TABLE IF NOT EXISTS … PARTITION OF … FOR VALUES FROM (…) TO (…)`)
  - `list_child_partitions(conn, table: &str) -> QueryResult<Vec<String>>` (child relation names, excluding the `_default` partition)
  - `count_child_rows(conn, child: &str) -> QueryResult<i64>`
  - `detach_and_drop_partition(conn, table: &str, child: &str) -> QueryResult<()>`

- [ ] **Step 1: Write the partition migration `up.sql`**

Create `backend/migrations/2026-07-14-000011_error_events_partitioned/up.sql`:

```sql
-- 0011: convert error_events into a RANGE-partitioned table on occurred_at so
-- aged partitions can be exported to Parquet and dropped cheaply.
--
-- Partitioning requires the partition key in every unique/primary key, so the
-- PK becomes (id, occurred_at). We rebuild the table and copy rows through a
-- DEFAULT partition (which also guarantees inserts never fail before the tier
-- worker pre-creates explicit partitions).

ALTER TABLE error_events RENAME TO error_events_old;

CREATE TABLE error_events (
    id              UUID NOT NULL DEFAULT gen_random_uuid(),
    app_id          UUID NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    environment_id  UUID REFERENCES environments(id) ON DELETE SET NULL,
    issue_id        UUID NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    fingerprint     TEXT NOT NULL,
    level           TEXT NOT NULL DEFAULT 'error',
    message         TEXT NOT NULL DEFAULT '',
    exception_type  TEXT NOT NULL DEFAULT '',
    exception_value TEXT NOT NULL DEFAULT '',
    stacktrace      JSONB NOT NULL DEFAULT '[]'::jsonb,
    breadcrumbs     JSONB NOT NULL DEFAULT '[]'::jsonb,
    context         JSONB NOT NULL DEFAULT '{}'::jsonb,
    tags            JSONB NOT NULL DEFAULT '{}'::jsonb,
    release         TEXT,
    distinct_id     TEXT,
    event_user      JSONB,
    sdk             JSONB,
    ip_address      TEXT,
    occurred_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    received_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    session_id      TEXT,
    device_key      TEXT,
    screen          TEXT,
    PRIMARY KEY (id, occurred_at)
) PARTITION BY RANGE (occurred_at);

-- Indexes mirror the originals (defined on the parent → propagate to partitions).
CREATE INDEX error_events_issue_idx      ON error_events (issue_id, occurred_at DESC);
CREATE INDEX error_events_project_idx    ON error_events (app_id, occurred_at DESC);
CREATE INDEX error_events_distinct_idx   ON error_events (app_id, distinct_id, occurred_at DESC);
CREATE INDEX error_events_app_session_idx ON error_events (app_id, session_id);
CREATE INDEX error_events_app_device_idx  ON error_events (app_id, device_key);
CREATE INDEX error_events_app_screen_idx  ON error_events (app_id, screen);

-- Safety net: catches any row not covered by an explicit range partition.
CREATE TABLE error_events_default PARTITION OF error_events DEFAULT;

-- Move existing rows across (column order matches the old table exactly).
INSERT INTO error_events SELECT * FROM error_events_old;

DROP TABLE error_events_old;
```

- [ ] **Step 2: Write the partition migration `down.sql`**

Create `backend/migrations/2026-07-14-000011_error_events_partitioned/down.sql`:

```sql
-- Revert to a plain (non-partitioned) error_events with a single-column PK.
ALTER TABLE error_events RENAME TO error_events_part;

CREATE TABLE error_events (LIKE error_events_part INCLUDING DEFAULTS);
ALTER TABLE error_events ADD PRIMARY KEY (id);
ALTER TABLE error_events ADD FOREIGN KEY (app_id) REFERENCES apps(id) ON DELETE CASCADE;
ALTER TABLE error_events ADD FOREIGN KEY (environment_id) REFERENCES environments(id) ON DELETE SET NULL;
ALTER TABLE error_events ADD FOREIGN KEY (issue_id) REFERENCES issues(id) ON DELETE CASCADE;
CREATE INDEX error_events_issue_idx      ON error_events (issue_id, occurred_at DESC);
CREATE INDEX error_events_project_idx    ON error_events (app_id, occurred_at DESC);
CREATE INDEX error_events_distinct_idx   ON error_events (app_id, distinct_id, occurred_at DESC);
CREATE INDEX error_events_app_session_idx ON error_events (app_id, session_id);
CREATE INDEX error_events_app_device_idx  ON error_events (app_id, device_key);
CREATE INDEX error_events_app_screen_idx  ON error_events (app_id, screen);

INSERT INTO error_events SELECT * FROM error_events_part;
DROP TABLE error_events_part;
```

- [ ] **Step 3: Add partition-maintenance repo functions**

Append to `backend/crates/sauron-db/src/repo.rs`:

```rust
/// Create a range partition if it does not already exist. `table`/`suffix` are
/// internal identifiers (never user input); timestamps are formatted as ISO
/// literals because partition bounds cannot be bound parameters in DDL.
pub async fn create_range_partition(
    conn: &mut AsyncPgConnection,
    table: &str,
    suffix: &str,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> QueryResult<()> {
    let sql = format!(
        "CREATE TABLE IF NOT EXISTS {table}_{suffix} PARTITION OF {table} \
         FOR VALUES FROM ('{start}') TO ('{end}')",
        table = table,
        suffix = suffix,
        start = start.to_rfc3339(),
        end = end.to_rfc3339(),
    );
    diesel::sql_query(sql).execute(conn).await?;
    Ok(())
}

#[derive(diesel::QueryableByName)]
struct ChildName {
    #[diesel(sql_type = Text)]
    child: String,
}

/// Child partition relation names for `table`, excluding the DEFAULT partition.
pub async fn list_child_partitions(
    conn: &mut AsyncPgConnection,
    table: &str,
) -> QueryResult<Vec<String>> {
    let rows: Vec<ChildName> = diesel::sql_query(
        "SELECT c.relname AS child \
         FROM pg_inherits i \
         JOIN pg_class c ON c.oid = i.inhrelid \
         JOIN pg_class p ON p.oid = i.inhparent \
         WHERE p.relname = $1 AND c.relname <> ($1 || '_default') \
         ORDER BY c.relname",
    )
    .bind::<Text, _>(table)
    .load(conn)
    .await?;
    Ok(rows.into_iter().map(|r| r.child).collect())
}

#[derive(diesel::QueryableByName)]
struct CountRow {
    #[diesel(sql_type = BigInt)]
    n: i64,
}

pub async fn count_child_rows(conn: &mut AsyncPgConnection, child: &str) -> QueryResult<i64> {
    // `child` is an internal relation name derived from our own suffix, not user input.
    let row: CountRow = diesel::sql_query(format!("SELECT count(*)::bigint AS n FROM {child}"))
        .get_result(conn)
        .await?;
    Ok(row.n)
}

/// Detach then drop a partition in one transaction. Detach first so the parent
/// is never briefly missing the range.
pub async fn detach_and_drop_partition(
    conn: &mut AsyncPgConnection,
    table: &str,
    child: &str,
) -> QueryResult<()> {
    let sql = format!(
        "BEGIN; ALTER TABLE {table} DETACH PARTITION {child}; DROP TABLE {child}; COMMIT;"
    );
    diesel::sql_query(sql).execute(conn).await?;
    Ok(())
}
```

- [ ] **Step 4: Compile check**

Run: `cd backend && cargo build -p sauron-db`
Expected: compiles.

- [ ] **Step 5: e2e verify the migration + partition round-trip**

```bash
docker compose down -v && docker compose up -d postgres migrate
# error_events is now partitioned; confirm the parent + default child exist.
docker compose exec -T postgres psql -U sauron -d sauron -c \
  "SELECT relname, relkind FROM pg_class WHERE relname LIKE 'error_events%' ORDER BY relname;"
```

Expected: `error_events` with `relkind = p` (partitioned), and `error_events_default` with `relkind = r`.

- [ ] **Step 6: Commit**

```bash
git add backend/migrations/2026-07-14-000011_error_events_partitioned backend/crates/sauron-db/src/repo.rs
git commit -m "feat(tier): partition error_events + partition-maintenance repo fns"
```

---

## Task 7: `sauron-tier` binary — the copy→verify→advance→drop worker

**Files:**
- Modify: `backend/crates/sauron-core/src/config.rs` (add `tier_*` fields)
- Modify: `backend/crates/sauron-tier/src/duck.rs` (add `export_from_postgres` + `count_range`)
- Create: `backend/bins/sauron-tier/Cargo.toml`
- Create: `backend/bins/sauron-tier/src/main.rs`

**Interfaces:**
- Consumes: everything from Tasks 1–6.
- Produces: a runnable binary `sauron-tier` that on each tick, per tiered table: pre-creates upcoming partitions, exports eligible aged partitions to Parquet, verifies counts, advances the watermark, and drops partitions past the lag. Adds `DuckEngine::export_from_postgres(pg_url, table, start, end, cold_dir) -> anyhow::Result<()>` and `DuckEngine::count_range(glob, start, end) -> anyhow::Result<i64>`.

- [ ] **Step 1: Add config fields**

In `backend/crates/sauron-core/src/config.rs`, add to `struct Config`:

```rust
    pub tier_hot_days: i64,
    pub tier_granularity: String,
    pub tier_cold_path: String,
    pub tier_drop_lag_hours: i64,
    pub tier_tick_secs: u64,
    pub tier_partition_ahead: i64,
```

And in `from_env()`'s `Ok(Self { … })`:

```rust
            tier_hot_days: parse("TIER_HOT_DAYS", 30),
            tier_granularity: var("TIER_GRANULARITY").unwrap_or_else(|| "day".to_string()),
            tier_cold_path: var("TIER_COLD_PATH").unwrap_or_else(|| "/var/lib/sauron/cold".to_string()),
            tier_drop_lag_hours: parse("TIER_DROP_LAG_HOURS", 24),
            tier_tick_secs: parse("TIER_TICK_SECS", 3600),
            tier_partition_ahead: parse("TIER_PARTITION_AHEAD", 7),
```

- [ ] **Step 2: Add the DuckDB export + range-count methods**

Append to `impl DuckEngine` in `backend/crates/sauron-tier/src/duck.rs`:

```rust
    /// Copy `[start, end)` of a Postgres table into hive-partitioned Parquet
    /// under `cold_dir`, appending to existing month directories. Uses DuckDB's
    /// postgres extension (needs libpq available at runtime).
    pub fn export_from_postgres(
        &self,
        pg_url: &str,
        table: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        cold_dir: &str,
    ) -> anyhow::Result<()> {
        self.conn.execute_batch("INSTALL postgres; LOAD postgres;")?;
        // ATTACH is idempotent-ish within a connection; detach if re-run.
        let _ = self.conn.execute_batch("DETACH DATABASE IF EXISTS pg;");
        self.conn
            .execute_batch(&format!("ATTACH '{pg_url}' AS pg (TYPE postgres, READ_ONLY);"))?;
        let sql = format!(
            "COPY (SELECT *, year(occurred_at) AS year, month(occurred_at) AS month \
                   FROM pg.{table} \
                   WHERE occurred_at >= TIMESTAMPTZ '{start}' AND occurred_at < TIMESTAMPTZ '{end}') \
             TO '{cold_dir}' (FORMAT PARQUET, PARTITION_BY (app_id, year, month), APPEND);",
            table = table,
            start = start.to_rfc3339(),
            end = end.to_rfc3339(),
            cold_dir = cold_dir,
        );
        self.conn.execute_batch(&sql)?;
        Ok(())
    }

    /// Count cold rows in `[start, end)` across all apps (verification helper).
    pub fn count_range(
        &self,
        glob: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> anyhow::Result<i64> {
        let sql = "SELECT count(*) FROM read_parquet(?, hive_partitioning=true, union_by_name=true) \
                   WHERE occurred_at >= ? AND occurred_at < ?";
        let mut stmt = self.conn.prepare(sql)?;
        let n: i64 = stmt.query_row(
            duckdb::params![glob, start.to_rfc3339(), end.to_rfc3339()],
            |r| r.get(0),
        )?;
        Ok(n)
    }
```

Note: the export read-glob for `count_range` must match all apps, so use a base glob `"{cold_dir}/**/*.parquet"` (all apps) here rather than the per-app glob.

- [ ] **Step 3: Create the bin manifest**

Create `backend/bins/sauron-tier/Cargo.toml`:

```toml
[package]
name = "sauron-tier-bin"
version.workspace = true
edition.workspace = true
license.workspace = true

[[bin]]
name = "sauron-tier"
path = "src/main.rs"

[dependencies]
sauron-core.workspace = true
sauron-db.workspace = true
sauron-tier.workspace = true
sauron-telemetry.workspace = true
tokio = { workspace = true }
chrono.workspace = true
anyhow.workspace = true
tracing.workspace = true
```

Add `sauron-tier = { path = "crates/sauron-tier" }` to `[workspace.dependencies]` in `backend/Cargo.toml` (mirror the other internal-crate lines).

- [ ] **Step 4: Write the worker loop**

Create `backend/bins/sauron-tier/src/main.rs`:

```rust
//! `sauron-tier` — moves aged partitions from Postgres to Parquet.
//!
//! Each cycle, per tiered table: pre-create upcoming partitions, export aged
//! partitions to Parquet (copy → verify counts → advance watermark), then drop
//! partitions that are below the watermark AND older than the drop lag. Nothing
//! is ever deleted: a partition is dropped only after its rows are verified in
//! Parquet, which is the permanent copy.

use std::time::Duration;

use chrono::{DateTime, Utc};
use tracing::{info, warn};

use sauron_core::Config;
use sauron_db::{conn, repo, PgPool};
use sauron_tier::{bucket_bounds, cold_copy_dir, partition_suffix, Granularity, TieredTable, TIERED_TABLES};
use sauron_tier::duck::DuckEngine;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    sauron_telemetry::init("sauron-tier");
    let cfg = Config::from_env()?;
    let pool = sauron_db::build_pool(&cfg.database_url, 4)?;
    let gran = Granularity::from_str_or(&cfg.tier_granularity, Granularity::Day);
    info!(hot_days = cfg.tier_hot_days, granularity = ?gran, "sauron-tier started");

    loop {
        if let Err(e) = cycle(&pool, &cfg, gran).await {
            warn!(error = %e, "tier cycle failed; backing off");
        }
        tokio::time::sleep(Duration::from_secs(cfg.tier_tick_secs)).await;
    }
}

async fn cycle(pool: &PgPool, cfg: &Config, gran: Granularity) -> anyhow::Result<()> {
    for t in TIERED_TABLES {
        if let Err(e) = tier_table(pool, cfg, gran, t).await {
            warn!(table = t.name, error = %e, "tiering table failed");
        }
    }
    Ok(())
}

async fn tier_table(pool: &PgPool, cfg: &Config, gran: Granularity, t: &TieredTable) -> anyhow::Result<()> {
    let now = Utc::now();
    let mut c = conn(pool).await?;

    // 1. Pre-create partitions for now .. now + partition_ahead buckets.
    let mut b = bucket_bounds(now, gran);
    for _ in 0..cfg.tier_partition_ahead {
        repo::create_range_partition(&mut c, t.name, &partition_suffix(b.start), b.start, b.end).await?;
        b = bucket_bounds(b.end, gran);
    }

    // 2. Eligibility cutoff: partitions whose END <= (now - hot_days) may tier.
    let cutoff = now - chrono::Duration::days(cfg.tier_hot_days);
    let cold_dir = cold_copy_dir(&cfg.tier_cold_path, t.name);
    let base_glob = format!("{}/**/*.parquet", cold_dir);

    // 3. Export eligible partitions oldest-first; stop on the first failure so
    //    the watermark never skips a gap.
    let children = repo::list_child_partitions(&mut c, t.name).await?;
    for child in children {
        let Some(start) = parse_suffix_start(&child, t.name) else { continue };
        let range = bucket_bounds(start, gran);
        if range.end > cutoff {
            continue; // still hot
        }
        let wm = repo::get_watermark(&mut c, t.name).await?;
        if let Some(w) = wm {
            if range.start < w {
                continue; // already exported
            }
        }
        let pg_rows = repo::count_child_rows(&mut c, &child).await?;

        let pg_url = cfg.database_url.clone();
        let table = t.name.to_string();
        let cold_dir_c = cold_dir.clone();
        let base_glob_c = base_glob.clone();
        let (rs, re) = (range.start, range.end);
        let cold_rows = tokio::task::spawn_blocking(move || -> anyhow::Result<i64> {
            let eng = DuckEngine::open()?;
            eng.export_from_postgres(&pg_url, &table, rs, re, &cold_dir_c)?;
            eng.count_range(&base_glob_c, rs, re)
        })
        .await??;

        if cold_rows != pg_rows {
            warn!(child = %child, pg_rows, cold_rows, "count mismatch; leaving partition for retry");
            break;
        }
        repo::advance_watermark(&mut c, t.name, range.end).await?;
        info!(child = %child, rows = pg_rows, "exported partition to Parquet");
    }

    // 4. Drop partitions strictly below the watermark AND past the drop lag.
    let wm = repo::get_watermark(&mut c, t.name).await?;
    if let Some(w) = wm {
        let lag = chrono::Duration::hours(cfg.tier_drop_lag_hours);
        for child in repo::list_child_partitions(&mut c, t.name).await? {
            let Some(start) = parse_suffix_start(&child, t.name) else { continue };
            let range = bucket_bounds(start, gran);
            if range.end <= w && (now - range.end) >= lag {
                repo::detach_and_drop_partition(&mut c, t.name, &child).await?;
                repo::set_dropped_thru(&mut c, t.name, range.end).await?;
                info!(child = %child, "dropped Postgres partition (now cold-only)");
            }
        }
    }
    Ok(())
}

/// `error_events_2026_05_01` → 2026-05-01T00:00:00Z.
fn parse_suffix_start(child: &str, table: &str) -> Option<DateTime<Utc>> {
    let suffix = child.strip_prefix(&format!("{table}_"))?;
    let parts: Vec<&str> = suffix.split('_').collect();
    if parts.len() != 3 {
        return None;
    }
    let (y, m, d) = (parts[0].parse().ok()?, parts[1].parse().ok()?, parts[2].parse().ok()?);
    chrono::TimeZone::with_ymd_and_hms(&Utc, y, m, d, 0, 0, 0).single()
}
```

- [ ] **Step 5: Compile check**

Run: `cd backend && cargo build -p sauron-tier-bin`
Expected: compiles (DuckDB already built in Task 4).

- [ ] **Step 6: e2e verify the full move (copy → verify → drop)**

This is the integration test for the whole vertical. Run with a tiny hot window so a fresh event is immediately eligible:

```bash
docker compose down -v && docker compose up -d --build postgres migrate redis ingest
# Seed one old error_event directly (occurred_at far in the past so it's cold-eligible).
docker compose exec -T postgres psql -U sauron -d sauron -c \
  "INSERT INTO apps(id, name, slug, public_key, project_id) \
     SELECT gen_random_uuid(),'t','t','pk_test', p.id FROM (INSERT INTO organizations(name,slug) VALUES('o','o') RETURNING id) o \
     CROSS JOIN LATERAL (INSERT INTO projects(org_id,name,slug) VALUES(o.id,'p','p') RETURNING id) p RETURNING id;" || true
# (If seeding via SQL is fiddly, instead POST an envelope through ingest, then
#  backdate its occurred_at with an UPDATE.)
docker compose exec -T postgres psql -U sauron -d sauron -c \
  "UPDATE error_events SET occurred_at = now() - interval '90 days';"
# Run the tier worker once with a 30-day hot window and no drop lag for the test.
docker compose run --rm -e TIER_HOT_DAYS=30 -e TIER_DROP_LAG_HOURS=0 -e TIER_TICK_SECS=1 \
  -e TIER_COLD_PATH=/cold -v sauron_cold:/cold tier &
sleep 8
# Verify: Parquet exists, rows gone from PG, watermark advanced.
docker compose exec -T postgres psql -U sauron -d sauron -c \
  "SELECT (SELECT count(*) FROM error_events) AS pg_rows, watermark, dropped_thru FROM tiering_state WHERE table_name='error_events';"
```

Expected: `pg_rows` for the backdated day is 0 (dropped), `watermark`/`dropped_thru` advanced past that day, and Parquet files exist under the `sauron_cold` volume (`error_events/app_id=…/year=…/month=…/*.parquet`).

- [ ] **Step 7: Commit**

```bash
git add backend/Cargo.toml backend/crates/sauron-core/src/config.rs backend/crates/sauron-tier/src/duck.rs backend/bins/sauron-tier
git commit -m "feat(tier): sauron-tier worker (copy/verify/advance/drop)"
```

---

## Task 8: Cross-tier read router + timeseries endpoint

**Files:**
- Modify: `backend/crates/sauron-db/src/repo.rs` (hot `error_counts_by_day_hot`)
- Create: `backend/bins/sauron-api/src/tier_read.rs`
- Modify: `backend/bins/sauron-api/src/main.rs` (register `mod tier_read;` + route)
- Modify: `backend/bins/sauron-api/src/routes/` (add the handler; mirror an existing analytics handler)
- Modify: `backend/bins/sauron-api/Cargo.toml` (add `sauron-tier` dep)

**Interfaces:**
- Consumes: `sauron_tier::{plan, merge_day_counts, cold_partition_glob, DayCount}`, `DuckEngine`, `repo::get_watermark`.
- Produces: `tier_read::error_counts_by_day(state, app_id, from, to) -> anyhow::Result<Vec<DayCount>>`; `repo::error_counts_by_day_hot(conn, app_id, from, to) -> QueryResult<Vec<DayCount>>` returning `Vec<sauron_tier::DayCount>`. HTTP: `GET /v1/apps/{app_id}/errors/timeseries?from=<rfc3339>&to=<rfc3339>`.

- [ ] **Step 1: Add the hot per-day count repo fn**

Append to `backend/crates/sauron-db/src/repo.rs` (add `sauron-tier` is NOT a dep of sauron-db — return a local row type and let the API map it):

```rust
#[derive(diesel::QueryableByName)]
pub struct DayCountRow {
    #[diesel(sql_type = diesel::sql_types::Date)]
    pub day: chrono::NaiveDate,
    #[diesel(sql_type = BigInt)]
    pub count: i64,
}

/// Per-day error counts from the HOT (Postgres) tier for `[from, to)`.
pub async fn error_counts_by_day_hot(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> QueryResult<Vec<DayCountRow>> {
    diesel::sql_query(
        "SELECT (occurred_at AT TIME ZONE 'UTC')::date AS day, count(*)::bigint AS count \
         FROM error_events \
         WHERE app_id = $1 AND occurred_at >= $2 AND occurred_at < $3 \
         GROUP BY 1 ORDER BY 1",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Timestamptz, _>(from)
    .bind::<Timestamptz, _>(to)
    .load(conn)
    .await
}
```

- [ ] **Step 2: Add `sauron-tier` to the API crate**

In `backend/bins/sauron-api/Cargo.toml` `[dependencies]`, add:

```toml
sauron-tier.workspace = true
```

- [ ] **Step 3: Write the cross-tier router (concurrent hot + cold)**

Create `backend/bins/sauron-api/src/tier_read.rs`:

```rust
//! Cross-tier read router. Splits a query window at the tier watermark and runs
//! the hot (Postgres) and cold (Parquet/DuckDB) halves concurrently, then glues
//! the additive per-day partials.

use chrono::{DateTime, Utc};
use uuid::Uuid;

use sauron_db::{conn, repo, PgPool};
use sauron_tier::duck::DuckEngine;
use sauron_tier::{cold_partition_glob, merge_day_counts, plan, DayCount};

use crate::AppState;

/// Error counts per day for `[from, to)`, spanning hot + cold as needed.
pub async fn error_counts_by_day(
    state: &AppState,
    app_id: Uuid,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> anyhow::Result<Vec<DayCount>> {
    // No watermark yet ⇒ everything is hot (nothing tiered).
    let wm = {
        let mut c = conn(&state.pool).await?;
        repo::get_watermark(&mut c, "error_events").await?
    };
    let watermark = match wm {
        Some(w) => w,
        None => {
            let mut c = conn(&state.pool).await?;
            let rows = repo::error_counts_by_day_hot(&mut c, app_id, from, to).await?;
            return Ok(rows.into_iter().map(|r| DayCount { day: r.day, count: r.count }).collect());
        }
    };

    let split = plan(watermark, from, to);

    // HOT branch: Postgres via diesel (async).
    let pool = state.pool.clone();
    let hot = async move {
        if let Some(r) = split.hot {
            let mut c = conn(&pool).await?;
            let rows = repo::error_counts_by_day_hot(&mut c, app_id, r.start, r.end).await?;
            Ok::<_, anyhow::Error>(rows.into_iter().map(|r| DayCount { day: r.day, count: r.count }).collect())
        } else {
            Ok(Vec::new())
        }
    };

    // COLD branch: DuckDB is blocking → spawn_blocking, runs concurrently.
    let cold_path = state.cfg.tier_cold_path.clone();
    let cold = async move {
        if let Some(r) = split.cold {
            let glob = cold_partition_glob(&cold_path, "error_events", app_id);
            tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<DayCount>> {
                let eng = DuckEngine::open()?;
                eng.error_counts_by_day(&glob, app_id, r.start, r.end)
            })
            .await?
        } else {
            Ok(Vec::new())
        }
    };

    let (hot_rows, cold_rows) = tokio::join!(hot, cold);
    Ok(merge_day_counts(hot_rows?, cold_rows?))
}
```

- [ ] **Step 4: Register the module + route**

In `backend/bins/sauron-api/src/main.rs`, add `mod tier_read;` near `mod routes;`, and register the route in the router builder alongside the existing app-scoped analytics routes:

```rust
        .route("/v1/apps/{app_id}/errors/timeseries", get(routes::analytics::error_timeseries))
```

- [ ] **Step 5: Add the handler**

In the analytics routes module (mirror an existing app-scoped handler for auth/`authorize_app` and query parsing — e.g. the issues-stats handler), add:

```rust
#[derive(serde::Deserialize)]
pub struct TimeseriesQuery {
    pub from: chrono::DateTime<chrono::Utc>,
    pub to: chrono::DateTime<chrono::Utc>,
}

#[derive(serde::Serialize)]
pub struct DayCountOut {
    pub day: chrono::NaiveDate,
    pub count: i64,
}

pub async fn error_timeseries(
    State(state): State<AppState>,
    claims: Claims,                         // same extractor other handlers use
    Path(app_id): Path<uuid::Uuid>,
    Query(q): Query<TimeseriesQuery>,
) -> Result<Json<Vec<DayCountOut>>, ApiError> {
    authorize_app(&state, &claims, app_id, "issue:read").await?;   // reuse existing guard
    let series = crate::tier_read::error_counts_by_day(&state, app_id, q.from, q.to)
        .await
        .map_err(ApiError::internal)?;                             // map to your error type
    Ok(Json(series.into_iter().map(|d| DayCountOut { day: d.day, count: d.count }).collect()))
}
```

Match the exact imports, extractor names (`Claims`), guard (`authorize_app`), and error type (`ApiError` / `ApiError::internal`) used by neighboring handlers in that file — adapt the four `//` -commented spots to the real symbols.

- [ ] **Step 6: Compile check**

Run: `cd backend && cargo build -p sauron-api`
Expected: compiles.

- [ ] **Step 7: e2e verify a straddling query**

```bash
docker compose up -d --build postgres migrate redis ingest api
# Ensure some errors exist across a boundary: seed/backdate half of them, tier them
# (as in Task 6 Step 6), leave recent ones hot. Then set a watermark between them
# and call the endpoint with a window that spans it.
TOKEN=... # obtain via /v1/auth/login as your other e2e steps do
curl -s "http://localhost:10000/v1/apps/$APP_ID/errors/timeseries?from=2026-04-01T00:00:00Z&to=2026-08-01T00:00:00Z" \
  -H "Authorization: Bearer $TOKEN" | jq .
```

Expected: a single ascending-by-day series whose early days come from Parquet (cold) and recent days from Postgres (hot), with counts equal to a control `COUNT(*) GROUP BY day` taken before tiering.

- [ ] **Step 8: Commit**

```bash
git add backend/crates/sauron-db/src/repo.rs backend/bins/sauron-api
git commit -m "feat(tier): concurrent cross-tier read router + errors timeseries endpoint"
```

---

## Task 9: Docker Compose wiring — `tier` service + cold volume

**Files:**
- Modify: `docker-compose.yml`
- Modify: `backend/Dockerfile` (ensure libpq present in the `tier` runtime image for DuckDB's postgres extension)
- Modify: `.env.example` (document `TIER_*`)

**Interfaces:** none (infra).

- [ ] **Step 1: Add the `tier` service and shared cold volume**

In `docker-compose.yml`, add a service (mirror `monitor`) and mount a new `colddata` volume; also mount the SAME volume read-only into `api` so the cross-tier router can read Parquet:

```yaml
  tier:
    build:
      context: ./backend
      args:
        BIN: sauron-tier
    environment:
      DATABASE_URL: postgres://${POSTGRES_USER:-sauron}:${POSTGRES_PASSWORD:-sauron}@postgres:5432/${POSTGRES_DB:-sauron}
      TIER_HOT_DAYS: ${TIER_HOT_DAYS:-30}
      TIER_GRANULARITY: ${TIER_GRANULARITY:-day}
      TIER_COLD_PATH: /cold
      TIER_DROP_LAG_HOURS: ${TIER_DROP_LAG_HOURS:-24}
      TIER_TICK_SECS: ${TIER_TICK_SECS:-3600}
      TIER_PARTITION_AHEAD: ${TIER_PARTITION_AHEAD:-7}
      RUST_LOG: ${RUST_LOG:-info,sauron=debug}
    volumes:
      - colddata:/cold
    depends_on:
      migrate:
        condition: service_completed_successfully
      postgres:
        condition: service_healthy
```

Add `TIER_COLD_PATH: /cold` to the `api` service `environment:` and mount the volume read-only:

```yaml
    volumes:
      - colddata:/cold:ro
```

And under top-level `volumes:` add `colddata: {}`.

- [ ] **Step 2: Ensure libpq in the runtime image**

DuckDB's `postgres` extension loads libpq at runtime. In `backend/Dockerfile`'s final runtime stage, install it (Debian-based example):

```dockerfile
RUN apt-get update && apt-get install -y --no-install-recommends libpq5 ca-certificates && rm -rf /var/lib/apt/lists/*
```

(If the runtime base is distroless/alpine, adjust accordingly; this only affects the `tier` and `api` images, which now embed DuckDB.)

- [ ] **Step 3: Document env in `.env.example`**

Append:

```dotenv
# --- hot/cold tiering (sauron-tier) ---
TIER_HOT_DAYS=30
TIER_GRANULARITY=day
TIER_DROP_LAG_HOURS=24
TIER_TICK_SECS=3600
TIER_PARTITION_AHEAD=7
```

- [ ] **Step 4: e2e verify the whole stack boots and tiers**

```bash
docker compose down -v && docker compose up -d --build
sleep 20
docker compose logs tier | tail -n 20
docker compose ps
```

Expected: `tier` service healthy/running, logs show `sauron-tier started` and (once data ages) `exported partition to Parquet`; `api` still serves the timeseries endpoint reading the shared `/cold` volume.

- [ ] **Step 5: Commit**

```bash
git add docker-compose.yml backend/Dockerfile .env.example
git commit -m "feat(tier): compose tier service + shared cold volume + libpq runtime"
```

---

## Task 10: Generalize to `analytics_events` and `transactions`

**Files:**
- Create: `backend/migrations/2026-07-14-000012_analytics_events_partitioned/{up,down}.sql`
- Create: `backend/migrations/2026-07-14-000013_transactions_partitioned/{up,down}.sql`
- Modify: `backend/crates/sauron-tier/src/lib.rs` (`TIERED_TABLES`)
- Modify: `backend/crates/sauron-db/src/repo.rs` + `backend/bins/sauron-api/src/tier_read.rs` (cold-read metrics for the new tables)

**Interfaces:**
- Produces: `TIERED_TABLES` includes all three; `tier_read::event_counts_by_day(...)` for `analytics_events`.

- [ ] **Step 1: Partition `analytics_events` (migration `…000012`)**

`up.sql` follows Task 6 exactly, with `analytics_events`'s columns (from `schema.rs`): `id, app_id, environment_id, name, distinct_id, properties, context, session_id, release, ip_address, occurred_at, received_at, device_key, screen`, PK `(id, occurred_at)`, `PARTITION BY RANGE (occurred_at)`, a `analytics_events_default` DEFAULT partition, and these indexes recreated on the parent:

```sql
CREATE INDEX analytics_name_idx            ON analytics_events (app_id, name, occurred_at DESC);
CREATE INDEX analytics_distinct_idx        ON analytics_events (app_id, distinct_id, occurred_at DESC);
CREATE INDEX analytics_project_idx         ON analytics_events (app_id, occurred_at DESC);
CREATE INDEX analytics_events_app_device_idx ON analytics_events (app_id, device_key);
CREATE INDEX analytics_events_app_screen_idx ON analytics_events (app_id, screen);
```

FKs: `app_id → apps ON DELETE CASCADE`, `environment_id → environments ON DELETE SET NULL`. `down.sql` mirrors Task 6's down.

- [ ] **Step 2: Partition `transactions` (migration `…000013`)**

Same shape with `transactions`'s columns: `id, app_id, environment_id, name, op, duration_ms, status, http_method, http_status, url, distinct_id, session_id, device_key, release, ip_address, occurred_at, received_at`, PK `(id, occurred_at)`, `transactions_default` DEFAULT partition, indexes:

```sql
CREATE INDEX transactions_app_occurred_idx ON transactions (app_id, occurred_at DESC);
CREATE INDEX transactions_app_op_name_idx  ON transactions (app_id, op, name);
CREATE INDEX transactions_app_session_idx  ON transactions (app_id, session_id);
```

FKs: `app_id → apps ON DELETE CASCADE`. `down.sql` mirrors Task 6's down.

- [ ] **Step 3: Add the tables to `TIERED_TABLES`**

In `backend/crates/sauron-tier/src/lib.rs`:

```rust
pub const TIERED_TABLES: &[TieredTable] = &[
    TieredTable { name: "error_events", time_col: "occurred_at" },
    TieredTable { name: "analytics_events", time_col: "occurred_at" },
    TieredTable { name: "transactions", time_col: "occurred_at" },
];
```

The `sauron-tier` worker (Task 7) is already table-generic, so it now tiers all three with no further code. Verify: `cd backend && cargo build -p sauron-tier-bin`.

- [ ] **Step 4: Add cold-read for analytics event counts (additive → cross-tier safe)**

In `duck.rs` add `event_counts_by_day` (identical to `error_counts_by_day` but the glob points at `analytics_events`). In `repo.rs` add `event_counts_by_day_hot` (identical to `error_counts_by_day_hot` but `FROM analytics_events`). In `tier_read.rs` add `event_counts_by_day` mirroring the error version with `"analytics_events"` as the table/glob. Wire a `GET /v1/apps/{app_id}/events/timeseries` endpoint mirroring Task 8 Steps 4–5.

- [ ] **Step 5: Note the holistic-metric boundary for transactions**

`transactions` percentiles (p50/p95) are **holistic** and do NOT merge across tiers by summing. For cross-tier transaction queries, either (a) serve percentiles **hot-only** (window clamped to `>= watermark`), or (b) add mergeable sketches later (t-digest/DDSketch). For this task, only add the **additive** transaction metric (throughput/count per day) cross-tier via the same pattern; leave percentile endpoints reading Postgres (hot) as they do today. Document this in a comment on the transactions cold-read function so it isn't mistaken for percentile-capable.

- [ ] **Step 6: e2e verify all three tier**

Repeat Task 7 Step 6's backdate-and-tier check for `analytics_events` and `transactions` (backdate rows, run `tier`, confirm Parquet appears and PG partitions drop, watermarks advance for all three `tiering_state` rows).

- [ ] **Step 7: Commit**

```bash
git add backend/migrations/2026-07-14-000012_analytics_events_partitioned backend/migrations/2026-07-14-000013_transactions_partitioned backend/crates/sauron-tier/src/lib.rs backend/crates/sauron-db/src/repo.rs backend/crates/sauron-tier/src/duck.rs backend/bins/sauron-api
git commit -m "feat(tier): generalize tiering to analytics_events + transactions"
```

---

## Self-Review

**Spec coverage:**
- Move-not-delete → Global Constraints + Task 7 (copy→verify→advance→drop, deletion off). ✓
- Partition firehose tables → Tasks 6 (error_events), 10 (analytics_events, transactions). ✓
- Tiering job → Task 7. ✓
- Cross-tier router with concurrency → Task 8 (`tokio::join!`, DuckDB on `spawn_blocking`). ✓
- Local-disk cold storage behind a path helper → Task 2 (`cold_*` fns), Task 9 (`colddata` volume). ✓
- Watermark + exactly-once + drop lag → Tasks 1, 5, 7. ✓
- error_events first, then generalize → Tasks 1–9 then 10. ✓

**Placeholder scan:** No `TBD`/`TODO`/"add error handling"/"similar to Task N" — the four adaptation points in Task 8 Step 5 (`Claims`, `authorize_app`, `ApiError`) are explicit "match the neighboring handler" instructions, not vague placeholders, because those symbols are file-local and must be read from the routes module at implementation time.

**Type consistency:** `DayCount { day: NaiveDate, count: i64 }` used consistently across `merge.rs`, `duck.rs`, `tier_read.rs`. `repo::DayCountRow` (diesel `QueryableByName`) is mapped to `sauron_tier::DayCount` at the API boundary (sauron-db has no `sauron-tier` dep). `plan()`/`TimeRange`/`TierPlan` signatures match between Task 1 and Task 8. `bucket_bounds`/`partition_suffix`/`cold_partition_glob`/`cold_copy_dir` signatures match between Task 2 and Tasks 4/7/8.

## Risks & Notes for the implementer

- **DuckDB build time:** the first `cargo build` with `duckdb`/`bundled` compiles the native lib (minutes). It only affects `sauron-tier`, `sauron-tier-bin`, and `sauron-api`.
- **DuckDB COPY options vary by version:** if `APPEND` is unsupported, use `OVERWRITE_OR_IGNORE` with a unique `FILENAME_PATTERN 'part_{uuid}'`. The read side is unaffected.
- **libpq at runtime** is required for DuckDB's `postgres` extension in the `tier`/`api` images (Task 9 Step 2). This does NOT reintroduce libpq into the diesel path — diesel still uses `postgres_backend`.
- **Semgrep Guardian hook:** if the user's Semgrep plugin is active this session, Write/Edit may be blocked until logged in; per project notes, apply file changes via Bash with the user's OK, or disable the plugin and restart.
- **No DB test harness** (documented): Tasks 5–10 rely on compose e2e verification, not `cargo test`. Keep the pure logic (Tasks 1–4) fully unit-tested so regressions surface without the stack.
