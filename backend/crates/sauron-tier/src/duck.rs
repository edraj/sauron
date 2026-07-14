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

    /// True if at least one file matches `glob`. DuckDB's `read_parquet` errors
    /// when a glob matches zero files, so read methods guard on this first and
    /// return an empty result instead of failing. `glob()` never errors on an
    /// empty match — it just returns zero rows.
    fn any_files_match(&self, glob: &str) -> anyhow::Result<bool> {
        let mut stmt = self.conn.prepare("SELECT count(*) FROM glob(?)")?;
        let n: i64 = stmt.query_row([glob], |r| r.get(0))?;
        Ok(n > 0)
    }

    /// Total rows across the Parquet matched by `glob`. Returns 0 if no files match.
    pub fn count_parquet_rows(&self, glob: &str) -> anyhow::Result<i64> {
        if !self.any_files_match(glob)? {
            return Ok(0);
        }
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
        if !self.any_files_match(glob)? {
            return Ok(Vec::new());
        }
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
        if !self.any_files_match(glob)? {
            return Ok(0);
        }
        let sql = "SELECT count(*) FROM read_parquet(?, hive_partitioning=true, union_by_name=true) \
                   WHERE occurred_at >= ? AND occurred_at < ?";
        let mut stmt = self.conn.prepare(sql)?;
        let n: i64 = stmt.query_row(
            duckdb::params![glob, start.to_rfc3339(), end.to_rfc3339()],
            |r| r.get(0),
        )?;
        Ok(n)
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
            "COPY (SELECT app_id, occurred_at, year(occurred_at) AS year, month(occurred_at) AS month \
             FROM (VALUES \
               ('{a}'::UUID, TIMESTAMPTZ '2026-05-01 10:00:00+00'), \
               ('{a}'::UUID, TIMESTAMPTZ '2026-05-01 11:00:00+00'), \
               ('{a}'::UUID, TIMESTAMPTZ '2026-05-02 09:00:00+00') \
             ) AS v(app_id, occurred_at)) \
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

    #[test]
    fn count_parquet_rows_is_zero_when_no_files_match() {
        let eng = DuckEngine::open().unwrap();
        // Glob under a directory that does not exist → zero matches, not an error.
        let glob = crate::layout::cold_partition_glob(
            "/nonexistent-sauron-tier-cold",
            "error_events",
            Uuid::new_v4(),
        );
        assert_eq!(eng.count_parquet_rows(&glob).unwrap(), 0);
    }

    #[test]
    fn error_counts_by_day_is_empty_when_no_files_match() {
        let eng = DuckEngine::open().unwrap();
        let app = Uuid::new_v4();
        let glob = crate::layout::cold_partition_glob("/nonexistent-sauron-tier-cold", "error_events", app);
        let from = "2026-05-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let to = "2026-06-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        assert!(eng.error_counts_by_day(&glob, app, from, to).unwrap().is_empty());
    }
}
