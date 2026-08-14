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
    /// The alias map currently registered as the `alias_map` temp table, or
    /// `None` when its contents are unknown. See [`Self::register_alias_map`]
    /// for why this exists and why it is compared by value.
    ///
    /// `RefCell` because registration is a `&self` operation reached from
    /// `&self` query methods; `Connection` is itself neither `Sync` nor
    /// shareable, and one engine is opened per blocking task, so this adds no
    /// thread-safety obligation the type did not already have.
    alias_map: std::cell::RefCell<Option<Vec<(String, String)>>>,
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
        Ok(Self {
            conn,
            alias_map: std::cell::RefCell::new(None),
        })
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
    ///
    /// `aliases` is the bounded cold overlay (see
    /// `sauron_db::identity_merge::cold_alias_map`): guest ids Parquet still
    /// holds because cold is immutable and the hot rewrite could never reach
    /// them. Each row is resolved through [`Self::resolved_cold_events`] before
    /// counting, so a guest merged into a person is counted once under the
    /// person's id rather than as two distinct people.
    pub fn distinct_users_by_day(
        &self,
        glob: &str,
        app_id: Uuid,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        aliases: &[(String, String)],
    ) -> anyhow::Result<Vec<DayCount>> {
        if !self.any_files_match(glob)? {
            return Ok(Vec::new());
        }
        let sql = format!(
            "SELECT CAST(e.occurred_at AS DATE) AS day, \
                    count(DISTINCT COALESCE(m.person, e.distinct_id)) AS cnt \
               FROM {} \
              WHERE e.app_id = ? AND e.occurred_at >= ? AND e.occurred_at < ? \
                AND e.distinct_id IS NOT NULL AND e.distinct_id <> '' \
              GROUP BY 1 ORDER BY 1",
            self.resolved_cold_events(aliases)?
        );
        let mut stmt = self.conn.prepare(&sql)?;
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

    // -----------------------------------------------------------------------
    // The purge's cold half
    // -----------------------------------------------------------------------

    /// Rows still in cold Parquet that a purge asked for but cannot delete.
    ///
    /// Reported to the operator as `cold_rows_skipped` BEFORE they confirm, so
    /// what will survive the purge is visible up front rather than discovered
    /// afterwards. The window is `[from, boundary)` — the part of the request
    /// that has already rotated out of Postgres.
    ///
    /// `env_ids` empty means every environment INCLUDING unattributed, matching
    /// `purge_jobs.environment_ids IS NULL`. When non-empty the filter is an
    /// `IN` list and unattributed rows are excluded, exactly as the hot side's
    /// `environment_id = ANY(...)` excludes them.
    pub fn count_in_purge_scope(
        &self,
        glob: &str,
        app_id: Uuid,
        env_ids: &[Uuid],
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> anyhow::Result<i64> {
        if from >= to || !self.any_files_match(glob)? {
            return Ok(0);
        }
        let mut params: Vec<String> = vec![
            glob.to_string(),
            app_id.to_string(),
            from.to_rfc3339(),
            to.to_rfc3339(),
        ];
        let env_pred = if env_ids.is_empty() {
            String::new()
        } else {
            let marks = std::iter::repeat_n("?", env_ids.len())
                .collect::<Vec<_>>()
                .join(",");
            params.extend(env_ids.iter().map(|e| e.to_string()));
            format!(" AND environment_id IN ({marks})")
        };
        let sql = format!(
            "SELECT count(*) FROM read_parquet(?, hive_partitioning=true, union_by_name=true) \
             WHERE app_id = ? AND occurred_at >= ? AND occurred_at < ?{env_pred}"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let n: i64 = stmt.query_row(duckdb::params_from_iter(params.iter()), |r| r.get(0))?;
        Ok(n)
    }

    /// Publish the alias map as a temp table for the resolved scans to join.
    ///
    /// Registered per query rather than read through `postgres_scanner`: DuckDB
    /// is unbundled and vendored here, and making a correctness-critical path
    /// depend on an extension load is a bad trade.
    ///
    /// ## Why an identical re-registration is skipped
    ///
    /// [`Self::resolved_cold_events`] calls this on EVERY resolved query, by
    /// design — that coupling is what stops the join text and the table it
    /// joins against from coming apart. But one engine serves several queries:
    /// `tier_read.rs` reuses a single [`DuckEngine`] across the loop over cold
    /// sub-ranges (`plan_with_restores` splits the window once per overlapping
    /// restore), so a restore-heavy window would tear down and re-append the
    /// whole map once per sub-range, per request. The call still happens
    /// unconditionally; only the work is skipped.
    ///
    /// The memo compares by VALUE, not by a hash of the entries. A hash
    /// collision would silently leave the PREVIOUS map registered and join
    /// every cold row against it — a wrong answer of exactly the kind this
    /// overlay exists to prevent, and one that no test would see. The extra
    /// copy is bounded by the same two prunes that bound the map itself (see
    /// `sauron_db::identity_merge::cold_alias_map`).
    pub fn register_alias_map(&self, entries: &[(String, String)]) -> anyhow::Result<()> {
        let unchanged = self.alias_map.borrow().as_deref() == Some(entries);
        if unchanged {
            return Ok(());
        }
        // Invalidate FIRST. From here until the memo is re-armed at the bottom,
        // the registered table is in an unknown state — `CREATE OR REPLACE` has
        // already dropped the previous contents and the appends may fail
        // part-way — so any error must leave the next call rebuilding rather
        // than trusting a memo that describes a map which was never finished.
        *self.alias_map.borrow_mut() = None;
        self.conn.execute_batch(
            "CREATE OR REPLACE TEMP TABLE alias_map (alias VARCHAR, person VARCHAR)",
        )?;
        if !entries.is_empty() {
            let mut app = self.conn.appender("alias_map")?;
            for (alias, person) in entries {
                app.append_row(duckdb::params![alias.as_str(), person.as_str()])?;
            }
            app.flush()?;
        }
        *self.alias_map.borrow_mut() = Some(entries.to_vec());
        Ok(())
    }

    /// The FROM clause every identity-aggregating cold query must use.
    ///
    /// A second cold aggregation that joined `read_parquet` directly would
    /// silently keep double-counting: no error, no failing test. Funnelling
    /// the resolution through one helper means new queries inherit it by
    /// default instead of by remembering.
    ///
    /// Takes `aliases` and registers it as a side effect — rather than leaving
    /// the caller to remember a separate [`Self::register_alias_map`] call —
    /// so the join text and the table it joins against can never come apart.
    /// `register_alias_map` itself stays `pub`: it is a named produced
    /// interface in its own right, and calling it directly (as this method
    /// now also does internally) fails loudly rather than silently — a stale
    /// `alias_map` from a previous query would be a `CREATE OR REPLACE`, and
    /// a missing one is a DuckDB "table does not exist" error, not a quiet
    /// wrong answer — so there is no correctness reason to hide it.
    fn resolved_cold_events(&self, aliases: &[(String, String)]) -> anyhow::Result<&'static str> {
        self.register_alias_map(aliases)?;
        Ok(
            "read_parquet(?, hive_partitioning=true, union_by_name=true) e \
            LEFT JOIN alias_map m ON m.alias = e.distinct_id",
        )
    }

    /// Per-key surviving cold row counts and time span, for one raw table.
    ///
    /// This is the cold half of the purge's recompute. Reading it is NOT
    /// optional: a Postgres-only recompute silently UNDERCOUNTS every rollup by
    /// whatever `sauron-tier` already exported, which turns a purge meant to
    /// correct the numbers into a subtler corruption of them — and one that
    /// looks like success, because the counter moves the way the operator
    /// expected.
    ///
    /// Batched by key rather than one query per key, and grouped rather than
    /// materialising every key in the app: the touched-key set reaches millions
    /// on the purges this feature exists for, so both "a query per key" and "a
    /// map of every key" are unusable. The caller pages the touched keys and
    /// passes one page at a time.
    ///
    /// Keys absent from the result had no surviving cold rows; the caller must
    /// treat a missing key as zero rather than as "unknown", or a rollup whose
    /// cold rows are all gone would never be deleted.
    pub fn counts_by_key(
        &self,
        glob: &str,
        app_id: Uuid,
        key_column: &str,
        keys: &[String],
    ) -> anyhow::Result<Vec<ColdKeyCount>> {
        if keys.is_empty() || !self.any_files_match(glob)? {
            return Ok(Vec::new());
        }
        // `key_column` is a &'static str chosen by matching on PurgeKind in
        // `sauron_db::purge::rollup_key_column`, never caller bytes — SQL
        // identifiers cannot be bound. The KEYS are bound parameters.
        let marks = std::iter::repeat_n("?", keys.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT {key_column} AS k, count(*) AS n, \
                    min(occurred_at) AS lo, max(occurred_at) AS hi \
             FROM read_parquet(?, hive_partitioning=true, union_by_name=true) \
             WHERE app_id = ? AND {key_column} IN ({marks}) \
             GROUP BY 1"
        );
        let mut params: Vec<String> = vec![glob.to_string(), app_id.to_string()];
        params.extend(keys.iter().cloned());
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(duckdb::params_from_iter(params.iter()), |r| {
            Ok(ColdKeyCount {
                key: r.get::<_, String>(0)?,
                count: r.get::<_, i64>(1)?,
                first: r.get::<_, Option<DateTime<Utc>>>(2)?,
                last: r.get::<_, Option<DateTime<Utc>>>(3)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }
}

/// One rollup key's surviving cold rows in a single raw table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColdKeyCount {
    pub key: String,
    pub count: i64,
    pub first: Option<DateTime<Utc>>,
    pub last: Option<DateTime<Utc>>,
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

    /// The overlay's whole reason to exist: a guest id and the person it was
    /// merged into must count as ONE distinct person on the cold side, even
    /// though cold Parquet is immutable and still holds the guest's own rows
    /// verbatim (the hot rewrite could never reach them).
    ///
    /// Two distractor rows on the same day guard against a vacuous pass: `u-42`
    /// already has its own row, so a broken overlay that failed to resolve
    /// `anon_x` (or joined it to the wrong person) would still show up as a
    /// wrong count (2, not 1) rather than accidentally landing on the right
    /// answer through under-seeding.
    #[test]
    fn distinct_users_by_day_applies_the_alias_overlay() {
        let dir = std::env::temp_dir().join(format!("sauron-tier-alias-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let app = Uuid::new_v4();

        let eng = DuckEngine::open().unwrap();
        let copy = format!(
            "COPY (SELECT app_id, distinct_id, occurred_at, \
                    year(occurred_at) AS year, month(occurred_at) AS month \
             FROM (VALUES \
               ('{a}'::UUID, 'anon_x', TIMESTAMPTZ '2026-05-01 10:00:00+00'), \
               ('{a}'::UUID, 'u-42',   TIMESTAMPTZ '2026-05-01 11:00:00+00') \
             ) AS v(app_id, distinct_id, occurred_at)) \
             TO '{d}/analytics_events' (FORMAT PARQUET, PARTITION_BY (app_id, year, month), APPEND)",
            a = app,
            d = dir.display()
        );
        eng.conn.execute_batch(&copy).unwrap();

        let glob =
            crate::layout::cold_partition_glob(&dir.display().to_string(), "analytics_events", app);
        let from = "2026-05-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let to = "2026-05-02T00:00:00Z".parse::<DateTime<Utc>>().unwrap();

        // No overlay: two distinct raw ids, straight off Parquet.
        let unresolved = eng
            .distinct_users_by_day(&glob, app, from, to, &[])
            .unwrap();
        assert_eq!(unresolved.len(), 1);
        assert_eq!(
            unresolved[0].count, 2,
            "without the overlay both ids count separately"
        );

        // With the overlay: anon_x resolves to u-42, so the day's distinct set
        // collapses to just {u-42}.
        let aliases = vec![("anon_x".to_string(), "u-42".to_string())];
        let resolved = eng
            .distinct_users_by_day(&glob, app, from, to, &aliases)
            .unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(
            resolved[0].count, 1,
            "anon_x must resolve to u-42, collapsing the day to one distinct person"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Two SEPARATE aliases resolving to the SAME person must collapse to one
    /// distinct count, not two. A regression that only handled a single
    /// alias per person (e.g. an implementation shaped around one row rather
    /// than a genuine join) would pass the two-id test above but fail here.
    #[test]
    fn distinct_users_by_day_collapses_two_aliases_into_one_person() {
        let dir = std::env::temp_dir().join(format!("sauron-tier-alias2-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let app = Uuid::new_v4();

        let eng = DuckEngine::open().unwrap();
        let copy = format!(
            "COPY (SELECT app_id, distinct_id, occurred_at, \
                    year(occurred_at) AS year, month(occurred_at) AS month \
             FROM (VALUES \
               ('{a}'::UUID, 'anon_x', TIMESTAMPTZ '2026-05-01 10:00:00+00'), \
               ('{a}'::UUID, 'anon_y', TIMESTAMPTZ '2026-05-01 11:00:00+00'), \
               ('{a}'::UUID, 'u-42',   TIMESTAMPTZ '2026-05-01 12:00:00+00') \
             ) AS v(app_id, distinct_id, occurred_at)) \
             TO '{d}/analytics_events' (FORMAT PARQUET, PARTITION_BY (app_id, year, month), APPEND)",
            a = app,
            d = dir.display()
        );
        eng.conn.execute_batch(&copy).unwrap();

        let glob =
            crate::layout::cold_partition_glob(&dir.display().to_string(), "analytics_events", app);
        let from = "2026-05-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let to = "2026-05-02T00:00:00Z".parse::<DateTime<Utc>>().unwrap();

        let unresolved = eng
            .distinct_users_by_day(&glob, app, from, to, &[])
            .unwrap();
        assert_eq!(
            unresolved[0].count, 3,
            "three distinct raw ids without the overlay"
        );

        let aliases = vec![
            ("anon_x".to_string(), "u-42".to_string()),
            ("anon_y".to_string(), "u-42".to_string()),
        ];
        let resolved = eng
            .distinct_users_by_day(&glob, app, from, to, &aliases)
            .unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(
            resolved[0].count, 1,
            "anon_x and anon_y both resolve to u-42, so all three raw ids collapse to one person"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// One engine, two DIFFERENT non-empty alias maps in a row.
    ///
    /// `register_alias_map` skips an identical re-registration, because
    /// `resolved_cold_events` calls it on every query and `tier_read.rs`
    /// reuses one engine across the loop over cold sub-ranges. The regression
    /// that memo can introduce is a stale table: a second query silently
    /// answered against the FIRST map. The two tests above only go from an
    /// empty map to a populated one; this goes populated → different, which
    /// is the case a naive "already registered once" flag would get wrong.
    #[test]
    fn a_second_query_with_a_different_alias_map_is_not_answered_from_the_first() {
        let dir = std::env::temp_dir().join(format!("sauron-tier-alias3-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let app = Uuid::new_v4();

        let eng = DuckEngine::open().unwrap();
        let copy = format!(
            "COPY (SELECT app_id, distinct_id, occurred_at, \
                    year(occurred_at) AS year, month(occurred_at) AS month \
             FROM (VALUES \
               ('{a}'::UUID, 'anon_x', TIMESTAMPTZ '2026-05-01 10:00:00+00'), \
               ('{a}'::UUID, 'anon_y', TIMESTAMPTZ '2026-05-01 11:00:00+00'), \
               ('{a}'::UUID, 'u-42',   TIMESTAMPTZ '2026-05-01 12:00:00+00') \
             ) AS v(app_id, distinct_id, occurred_at)) \
             TO '{d}/analytics_events' (FORMAT PARQUET, PARTITION_BY (app_id, year, month), APPEND)",
            a = app,
            d = dir.display()
        );
        eng.conn.execute_batch(&copy).unwrap();

        let glob =
            crate::layout::cold_partition_glob(&dir.display().to_string(), "analytics_events", app);
        let from = "2026-05-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let to = "2026-05-02T00:00:00Z".parse::<DateTime<Utc>>().unwrap();

        // Map 1 folds only anon_x into u-42: {u-42, anon_y} = 2 people.
        let first = eng
            .distinct_users_by_day(
                &glob,
                app,
                from,
                to,
                &[("anon_x".to_string(), "u-42".to_string())],
            )
            .unwrap();
        assert_eq!(first[0].count, 2);

        // Map 2 folds BOTH into u-42: {u-42} = 1 person. Same engine, same
        // query, different map.
        let second = eng
            .distinct_users_by_day(
                &glob,
                app,
                from,
                to,
                &[
                    ("anon_x".to_string(), "u-42".to_string()),
                    ("anon_y".to_string(), "u-42".to_string()),
                ],
            )
            .unwrap();
        assert_eq!(
            second[0].count, 1,
            "the second query must be answered against the second map. A memo that treats \
             'already registered' as 'still current' leaves the first map in place and this \
             comes back 2 — a silently wrong distinct-user count on a dashboard read path, \
             with no error anywhere."
        );

        // …and back to the first map, to pin that the memo tracks the current
        // contents rather than latching after two registrations.
        let third = eng
            .distinct_users_by_day(
                &glob,
                app,
                from,
                to,
                &[("anon_x".to_string(), "u-42".to_string())],
            )
            .unwrap();
        assert_eq!(third[0].count, 2);

        std::fs::remove_dir_all(&dir).ok();
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

    // -----------------------------------------------------------------------
    // The purge's cold half
    // -----------------------------------------------------------------------

    /// A cold dataset carrying the columns the purge actually reads:
    /// `session_id` and `environment_id` alongside `occurred_at`. The real
    /// export is `COPY (SELECT *, …)` so cold Parquet has every column the hot
    /// table did — this fixture mirrors that.
    fn write_purge_fixture(dir: &std::path::Path, app: Uuid, env: Uuid) -> DuckEngine {
        std::fs::create_dir_all(dir).unwrap();
        let eng = DuckEngine::open().unwrap();
        let copy = format!(
            "COPY (SELECT app_id, environment_id, session_id, occurred_at, \
                    year(occurred_at) AS year, month(occurred_at) AS month FROM (VALUES \
               ('{a}'::UUID, '{e}'::UUID, 's1', TIMESTAMPTZ '2026-05-01 10:00:00+00'), \
               ('{a}'::UUID, '{e}'::UUID, 's1', TIMESTAMPTZ '2026-05-03 10:00:00+00'), \
               ('{a}'::UUID, '{e}'::UUID, 's2', TIMESTAMPTZ '2026-05-02 10:00:00+00'), \
               ('{a}'::UUID, NULL,        's3', TIMESTAMPTZ '2026-05-02 12:00:00+00') \
             ) AS v(app_id, environment_id, session_id, occurred_at)) \
             TO '{d}/error_events' (FORMAT PARQUET, PARTITION_BY (app_id, year, month), APPEND)",
            a = app,
            e = env,
            d = dir.display()
        );
        eng.conn.execute_batch(&copy).unwrap();
        eng
    }

    #[test]
    fn counts_by_key_groups_and_spans() {
        let dir = std::env::temp_dir().join(format!("sauron-purge-ck-{}", Uuid::new_v4()));
        let app = Uuid::new_v4();
        let eng = write_purge_fixture(&dir, app, Uuid::new_v4());
        let glob = cold_glob(&dir.display().to_string(), app);

        let keys = vec!["s1".to_string(), "s2".to_string()];
        let mut got = eng.counts_by_key(&glob, app, "session_id", &keys).unwrap();
        got.sort_by(|a, b| a.key.cmp(&b.key));

        assert_eq!(got.len(), 2);
        assert_eq!(got[0].key, "s1");
        assert_eq!(got[0].count, 2);
        assert_eq!(
            got[0].first.unwrap(),
            "2026-05-01T10:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
        assert_eq!(
            got[0].last.unwrap(),
            "2026-05-03T10:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
        assert_eq!(got[1].count, 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A key with no surviving cold rows must be ABSENT, so the caller reads it
    /// as zero. If it came back as a row with count 0 — or if absence were
    /// treated as "unknown" — a rollup whose cold rows are all gone would never
    /// be deleted.
    #[test]
    fn a_key_with_no_cold_rows_is_absent() {
        let dir = std::env::temp_dir().join(format!("sauron-purge-abs-{}", Uuid::new_v4()));
        let app = Uuid::new_v4();
        let eng = write_purge_fixture(&dir, app, Uuid::new_v4());
        let glob = cold_glob(&dir.display().to_string(), app);

        let keys = vec!["s1".to_string(), "does-not-exist".to_string()];
        let got = eng.counts_by_key(&glob, app, "session_id", &keys).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].key, "s1");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn counts_by_key_is_empty_without_keys_or_files() {
        let eng = DuckEngine::open().unwrap();
        let app = Uuid::new_v4();
        let glob = crate::layout::cold_partition_glob("/nonexistent-cold", "error_events", app);
        assert!(eng
            .counts_by_key(&glob, app, "session_id", &["s1".into()])
            .unwrap()
            .is_empty());
        // Also empty for an empty key list, without touching the filesystem.
        assert!(eng
            .counts_by_key(&glob, app, "session_id", &[])
            .unwrap()
            .is_empty());
    }

    #[test]
    fn purge_scope_count_respects_the_window() {
        let dir = std::env::temp_dir().join(format!("sauron-purge-sc-{}", Uuid::new_v4()));
        let app = Uuid::new_v4();
        let env = Uuid::new_v4();
        let eng = write_purge_fixture(&dir, app, env);
        let glob = cold_glob(&dir.display().to_string(), app);

        let all_from = "2026-05-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let all_to = "2026-06-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        assert_eq!(
            eng.count_in_purge_scope(&glob, app, &[], all_from, all_to)
                .unwrap(),
            4
        );

        // Half-open upper bound: 05-03 is excluded.
        let to = "2026-05-03T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        assert_eq!(
            eng.count_in_purge_scope(&glob, app, &[], all_from, to)
                .unwrap(),
            3
        );

        // An inverted or empty window is zero, never "everything".
        assert_eq!(
            eng.count_in_purge_scope(&glob, app, &[], all_to, all_from)
                .unwrap(),
            0
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Naming environments must exclude unattributed rows, matching the hot
    /// side's `environment_id = ANY(...)` where `NULL = ANY(...)` is not true.
    /// An empty list means every environment INCLUDING unattributed.
    #[test]
    fn env_filter_excludes_unattributed_but_no_filter_includes_it() {
        let dir = std::env::temp_dir().join(format!("sauron-purge-env-{}", Uuid::new_v4()));
        let app = Uuid::new_v4();
        let env = Uuid::new_v4();
        let eng = write_purge_fixture(&dir, app, env);
        let glob = cold_glob(&dir.display().to_string(), app);
        let from = "2026-05-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let to = "2026-06-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap();

        assert_eq!(
            eng.count_in_purge_scope(&glob, app, &[env], from, to)
                .unwrap(),
            3,
            "the unattributed row must not be counted under a named environment"
        );
        assert_eq!(
            eng.count_in_purge_scope(&glob, app, &[], from, to).unwrap(),
            4,
            "no filter must include the unattributed row"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
