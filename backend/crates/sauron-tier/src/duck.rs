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
        // Pin UTC so `CAST(occurred_at AS DATE)` day-bucketing matches the hot side's
        // `(occurred_at AT TIME ZONE 'UTC')::date`.
        conn.execute_batch("SET memory_limit='512MB'; SET threads=4; SET TimeZone='UTC';")?;
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
        let sql =
            "SELECT count(*) FROM read_parquet(?, hive_partitioning=true, union_by_name=true)";
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

    /// Per-day row counts for one app in `[from, to)` read from cold Parquet.
    /// Table-agnostic: reads `occurred_at` + `app_id` from whatever
    /// hive-partitioned Parquet dataset `glob` points at (error_events,
    /// analytics_events, transactions, ...). Callers select the table by
    /// building `glob` with the appropriate table name.
    pub fn counts_by_day(
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
        self.conn
            .execute_batch("INSTALL postgres; LOAD postgres;")?;
        // ATTACH is idempotent-ish within a connection; detach if re-run.
        let _ = self.conn.execute_batch("DETACH DATABASE IF EXISTS pg;");
        self.conn.execute_batch(&format!(
            "ATTACH '{pg_url}' AS pg (TYPE postgres, READ_ONLY);"
        ))?;
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

    /// Column name + DuckDB-mapped type for one relation, via `DESCRIBE`.
    fn describe(&self, relation_or_query: &str) -> anyhow::Result<Vec<(String, String)>> {
        let mut stmt = self
            .conn
            .prepare(&format!("DESCRIBE {relation_or_query}"))?;
        let rows = stmt.query_map([], |r| {
            let name: String = r.get(0)?;
            let ty: String = r.get(1)?;
            Ok((name, ty))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Copy `[start, end)` of cold Parquet back into the LIVE Postgres table,
    /// tagging every row with `pin_id`. Returns the number of rows inserted.
    ///
    /// Rows are inserted into the partitioned PARENT, so Postgres routes each to
    /// whichever partition covers its `occurred_at` — in practice
    /// `<table>_default`, because a cold range's explicit partition has already
    /// been dropped. Nothing here creates or attaches a partition; see the
    /// 000045 migration for why that approach was rejected.
    ///
    /// ## Why the column list is computed rather than hardcoded
    ///
    /// The Parquet was written by `export_from_postgres` with
    /// `PARTITION_BY (app_id, year, month)`, which STRIPS those three columns
    /// out of the files and encodes them in the directory path. Read back with
    /// `hive_partitioning=true` they come back as VARCHAR, so `app_id` needs an
    /// explicit cast and `year`/`month` must not be inserted at all — they are
    /// derived, and no such columns exist in Postgres.
    ///
    /// Beyond that, cold Parquet is a historical artifact: files written months
    /// ago predate every column added since. Intersecting the live Postgres
    /// column list with what the Parquet actually contains is what lets an old
    /// export restore into a newer schema, with the missing columns taking their
    /// Postgres defaults. Hardcoding the list would make the feature break
    /// silently the first time anyone added a column.
    ///
    /// Every shared column is cast to the type Postgres reports, which is what
    /// converts the hive `app_id` VARCHAR back to a UUID and keeps JSONB columns
    /// from arriving as text.
    #[allow(clippy::too_many_arguments)]
    pub fn restore_to_postgres(
        &self,
        pg_url: &str,
        table: &str,
        glob: &str,
        app_id: Option<Uuid>,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        pin_id: Uuid,
    ) -> anyhow::Result<i64> {
        if !self.any_files_match(glob)? {
            return Ok(0);
        }
        self.conn
            .execute_batch("INSTALL postgres; LOAD postgres;")?;
        let _ = self.conn.execute_batch("DETACH DATABASE IF EXISTS pg;");
        // NOT read-only, unlike the export path: this is the one place that
        // writes back into Postgres.
        self.conn
            .execute_batch(&format!("ATTACH '{pg_url}' AS pg (TYPE postgres);"))?;

        let pg_cols = self.describe(&format!("pg.{table}"))?;
        let src = format!("read_parquet('{glob}', hive_partitioning=true, union_by_name=true)");
        let parquet_cols: std::collections::HashSet<String> = self
            .describe(&format!("(SELECT * FROM {src} LIMIT 0)"))?
            .into_iter()
            .map(|(n, _)| n)
            .collect();

        let mut names: Vec<String> = Vec::new();
        let mut exprs: Vec<String> = Vec::new();
        for (name, ty) in &pg_cols {
            // `restored_pin_id` is supplied by us, never read from Parquet —
            // a re-restore of already-restored data must carry the NEW pin.
            if name == "restored_pin_id" || !parquet_cols.contains(name) {
                continue;
            }
            names.push(format!("\"{name}\""));
            exprs.push(format!("CAST(\"{name}\" AS {ty}) AS \"{name}\""));
        }
        if names.is_empty() {
            anyhow::bail!("no columns in common between {table} and its cold Parquet");
        }
        names.push("\"restored_pin_id\"".to_string());
        exprs.push(format!("CAST('{pin_id}' AS UUID) AS \"restored_pin_id\""));

        let app_filter = match app_id {
            Some(a) => format!(" AND CAST(app_id AS UUID) = CAST('{a}' AS UUID)"),
            None => String::new(),
        };
        let sql = format!(
            "INSERT INTO pg.{table} ({cols}) \
             SELECT {exprs} FROM {src} \
              WHERE occurred_at >= TIMESTAMPTZ '{start}' \
                AND occurred_at <  TIMESTAMPTZ '{end}'{app_filter}",
            table = table,
            cols = names.join(", "),
            exprs = exprs.join(", "),
            src = src,
            start = start.to_rfc3339(),
            end = end.to_rfc3339(),
            app_filter = app_filter,
        );
        let inserted = self.conn.execute(&sql, [])?;
        // DETACH so the connection does not hold a Postgres session open past
        // the restore; DuckDB engines here are short-lived but this one has a
        // WRITE session, which is worth releasing promptly.
        let _ = self.conn.execute_batch("DETACH DATABASE IF EXISTS pg;");
        Ok(inserted as i64)
    }

    /// Rows available in cold Parquet for `[start, end)`, optionally for one
    /// app — the denominator for restore progress.
    pub fn count_restorable(
        &self,
        glob: &str,
        app_id: Option<Uuid>,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> anyhow::Result<i64> {
        if !self.any_files_match(glob)? {
            return Ok(0);
        }
        let (sql, params): (String, Vec<String>) = match app_id {
            Some(a) => (
                "SELECT count(*) FROM read_parquet(?, hive_partitioning=true, union_by_name=true) \
                 WHERE occurred_at >= ? AND occurred_at < ? AND CAST(app_id AS UUID) = CAST(? AS UUID)"
                    .to_string(),
                vec![
                    glob.to_string(),
                    start.to_rfc3339(),
                    end.to_rfc3339(),
                    a.to_string(),
                ],
            ),
            None => (
                "SELECT count(*) FROM read_parquet(?, hive_partitioning=true, union_by_name=true) \
                 WHERE occurred_at >= ? AND occurred_at < ?"
                    .to_string(),
                vec![glob.to_string(), start.to_rfc3339(), end.to_rfc3339()],
            ),
        };
        let mut stmt = self.conn.prepare(&sql)?;
        let n: i64 = stmt.query_row(duckdb::params_from_iter(params.iter()), |r| r.get(0))?;
        Ok(n)
    }

    /// Distinct people per UTC day from cold Parquet.
    ///
    /// The cold half of the Active Users series. `TimeZone='UTC'` is pinned in
    /// [`DuckEngine::open`], so `CAST(occurred_at AS DATE)` here buckets on the
    /// same day boundary as the hot side's
    /// `(occurred_at AT TIME ZONE 'UTC')::date`. If those ever disagreed the
    /// series would show a seam at the watermark that looked like real data.
    ///
    /// Empty `distinct_id` excluded, matching `active_users_by_day_hot` — see its
    /// doc comment for why device_key is not a fallback.
    ///
    /// The result is per-day and therefore concatenable with the hot half, which
    /// a single total would NOT be: `count(DISTINCT …)` cannot be summed across
    /// tiers without double-counting anyone active on both sides.
    pub fn distinct_users_by_day(
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
            SELECT CAST(occurred_at AS DATE) AS day, count(DISTINCT distinct_id) AS cnt \
            FROM read_parquet(?, hive_partitioning=true, union_by_name=true) \
            WHERE app_id = ? AND occurred_at >= ? AND occurred_at < ? \
              AND distinct_id IS NOT NULL AND distinct_id <> '' \
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
        let sql =
            "SELECT count(*) FROM read_parquet(?, hive_partitioning=true, union_by_name=true) \
                   WHERE occurred_at >= ? AND occurred_at < ?";
        let mut stmt = self.conn.prepare(sql)?;
        let n: i64 = stmt.query_row(
            duckdb::params![glob, start.to_rfc3339(), end.to_rfc3339()],
            |r| r.get(0),
        )?;
        Ok(n)
    }

    /// Per-app row counts across the Parquet matched by `glob` (all apps in one
    /// query). `app_id` is read from the hive path as text, so we parse it back to
    /// Uuid. Returns empty when no files match.
    pub fn counts_by_app(&self, glob: &str) -> anyhow::Result<Vec<(Uuid, i64)>> {
        if !self.any_files_match(glob)? {
            return Ok(Vec::new());
        }
        let sql = "SELECT app_id, count(*) FROM read_parquet(?, hive_partitioning=true, union_by_name=true) GROUP BY app_id";
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map([glob], |r| {
            let app: String = r.get(0)?;
            let n: i64 = r.get(1)?;
            Ok((app, n))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (app, n) = row?;
            if let Ok(id) = Uuid::parse_str(&app) {
                out.push((id, n));
            }
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
        let series = eng.counts_by_day(&glob, app, from, to).unwrap();
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
    fn counts_by_day_is_empty_when_no_files_match() {
        let eng = DuckEngine::open().unwrap();
        let app = Uuid::new_v4();
        let glob = crate::layout::cold_partition_glob(
            "/nonexistent-sauron-tier-cold",
            "error_events",
            app,
        );
        let from = "2026-05-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let to = "2026-06-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        assert!(eng.counts_by_day(&glob, app, from, to).unwrap().is_empty());
    }

    #[test]
    fn counts_by_app_groups_two_apps() {
        let dir = std::env::temp_dir().join(format!("sauron-tier-cba-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let a1 = Uuid::new_v4();
        let a2 = Uuid::new_v4();
        let eng = DuckEngine::open().unwrap();
        let copy = format!(
            "COPY (SELECT app_id, occurred_at, year(occurred_at) AS year, month(occurred_at) AS month FROM (VALUES \
               ('{a1}'::UUID, TIMESTAMPTZ '2026-05-01 10:00:00+00'), \
               ('{a1}'::UUID, TIMESTAMPTZ '2026-05-02 10:00:00+00'), \
               ('{a2}'::UUID, TIMESTAMPTZ '2026-05-01 11:00:00+00') \
             ) AS v(app_id, occurred_at)) \
             TO '{d}/error_events' (FORMAT PARQUET, PARTITION_BY (app_id, year, month), APPEND)",
            a1 = a1, a2 = a2, d = dir.display()
        );
        eng.conn.execute_batch(&copy).unwrap();
        let glob = format!("{}/error_events/**/*.parquet", dir.display());
        let mut counts = eng.counts_by_app(&glob).unwrap();
        counts.sort_by_key(|(_, n)| *n);
        assert_eq!(counts.len(), 2);
        assert_eq!(counts.iter().map(|(_, n)| *n).sum::<i64>(), 3);
        assert!(counts.iter().any(|(id, n)| *id == a1 && *n == 2));
        assert!(counts.iter().any(|(id, n)| *id == a2 && *n == 1));
        std::fs::remove_dir_all(&dir).ok();
    }
}
