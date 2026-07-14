//! `sauron-tier` — hot/cold tiering: pure planning/layout/merge logic plus an
//! embedded DuckDB engine for reading and writing Parquet cold storage. No
//! diesel here; the `sauron-tier` binary glues this to `sauron-db`.

// pub mod duck;
// pub mod layout;
// pub mod merge;
pub mod plan;

// pub use layout::{bucket_bounds, cold_copy_dir, cold_partition_glob, partition_suffix, Granularity};
// pub use merge::{merge_day_counts, DayCount};
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
