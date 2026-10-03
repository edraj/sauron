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

/// Environment variable naming this process's DuckDB memory ceiling, in MB.
///
/// Read from the environment rather than `Config` because `open()` is a static
/// constructor with call sites across two binaries, and threading config through
/// all of them would buy nothing: the right ceiling is a property of the PROCESS,
/// not of the call. `sauron-api` opens an engine per concurrent cold read, so it
/// wants a low ceiling to bound the worst case across many readers; `sauron-tier`
/// opens one at a time and streams a whole partition through it, so it wants a
/// high one. One variable, set per systemd unit, expresses both.
const DUCK_MEMORY_MB_ENV: &str = "DUCKDB_MEMORY_MB";

/// Ceiling when the variable is unset or unusable. Matches the value this was
/// hard-coded to before the knob existed, so an untouched deployment is
/// bit-identical to the old behaviour.
const DUCK_MEMORY_MB_DEFAULT: u32 = 512;

/// Parse a memory ceiling, falling back to [`DUCK_MEMORY_MB_DEFAULT`].
///
/// Split from the env read so it is testable without mutating process-global
/// state, which is unsound under a parallel test runner. Zero and negative
/// values fall back rather than erroring: a mis-typed ceiling must not be able
/// to stop tiering deployment-wide, and DuckDB rejects `0MB` outright.
fn parse_memory_mb(raw: Option<&str>) -> u32 {
    raw.map(str::trim)
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DUCK_MEMORY_MB_DEFAULT)
}

fn duck_memory_mb() -> u32 {
    parse_memory_mb(std::env::var(DUCK_MEMORY_MB_ENV).ok().as_deref())
}

/// Environment variable capping how much DuckDB may spill to its temp
/// directory, in MB.
///
/// DuckDB's own default is 90% of the free space on the temp directory's
/// filesystem, which on a typical install is the same disk as Postgres. An
/// export that spills that much fills the disk and takes Postgres down with
/// it. With a cap, the export fails instead, which the windowed retry in
/// [`DuckEngine::export_from_postgres`] is there to absorb.
const DUCK_MAX_TEMP_MB_ENV: &str = "DUCKDB_MAX_TEMP_MB";

/// Spill cap when the variable is unset or unusable.
const DUCK_MAX_TEMP_MB_DEFAULT: u32 = 10_240;

/// Parse a spill cap, falling back to [`DUCK_MAX_TEMP_MB_DEFAULT`]. Same rules
/// as [`parse_memory_mb`]: anything unusable falls back rather than erroring.
fn parse_max_temp_mb(raw: Option<&str>) -> u32 {
    raw.map(str::trim)
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DUCK_MAX_TEMP_MB_DEFAULT)
}

fn duck_max_temp_mb() -> u32 {
    parse_max_temp_mb(std::env::var(DUCK_MAX_TEMP_MB_ENV).ok().as_deref())
}

/// Every `*.parquet` path under `dir`, recursively. Missing directory ⇒ empty
/// set, which is the correct answer before the very first export.
pub(crate) fn parquet_files_under(
    dir: &std::path::Path,
) -> std::collections::HashSet<std::path::PathBuf> {
    let mut out = std::collections::HashSet::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in entries.flatten() {
        let path = e.path();
        if path.is_dir() {
            out.extend(parquet_files_under(&path));
        } else if path.extension().is_some_and(|x| x == "parquet") {
            out.insert(path);
        }
    }
    out
}

/// Sub-range size for an export retried after the single-pass COPY failed.
/// See [`DuckEngine::export_from_postgres`].
pub const EXPORT_FALLBACK_WINDOW: chrono::Duration = chrono::Duration::hours(1);

/// `[start, end)` cut into consecutive `step`-sized windows; the last one is
/// shorter when `step` does not divide the range. Empty when `start >= end`.
pub fn split_range(
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    step: chrono::Duration,
) -> Vec<(DateTime<Utc>, DateTime<Utc>)> {
    let mut out = Vec::new();
    if step <= chrono::Duration::zero() {
        if start < end {
            out.push((start, end));
        }
        return out;
    }
    let mut s = start;
    while s < end {
        let e = (s + step).min(end);
        out.push((s, e));
        s = e;
    }
    out
}

/// A fresh, uniquely named staging directory for one export into `cold_dir`
/// (`<cold>/<table>`): `<cold>/.staging/<table>-<uuid>`.
///
/// Beside the table directories rather than inside one, so it matches no
/// reader's `<cold>/<table>/**` glob, and on the same filesystem as `cold_dir`,
/// so the commit can be a `rename`.
fn staging_dir_for(cold_dir: &std::path::Path) -> anyhow::Result<std::path::PathBuf> {
    let (Some(root), Some(table)) = (cold_dir.parent(), cold_dir.file_name()) else {
        anyhow::bail!("cold directory {} has no parent", cold_dir.display());
    };
    Ok(root.join(crate::layout::STAGING_DIR).join(format!(
        "{}-{}",
        table.to_string_lossy(),
        Uuid::new_v4()
    )))
}

/// Move every staged `*.parquet` to the same relative path under `cold_dir`.
///
/// If any move fails, the ones already made are removed again and the error is
/// returned, so a failed commit leaves `cold_dir` as it found it.
fn commit_staged(stage: &std::path::Path, cold_dir: &std::path::Path) -> anyhow::Result<()> {
    let mut staged: Vec<_> = parquet_files_under(stage).into_iter().collect();
    staged.sort();
    let mut committed: Vec<std::path::PathBuf> = Vec::with_capacity(staged.len());
    let outcome = (|| -> anyhow::Result<()> {
        for src in &staged {
            let rel = src
                .strip_prefix(stage)
                .context("staged file outside the stage")?;
            let dest = cold_dir.join(rel);
            if dest.exists() {
                anyhow::bail!(
                    "refusing to overwrite existing cold file {}",
                    dest.display()
                );
            }
            if let Some(dir) = dest.parent() {
                std::fs::create_dir_all(dir)
                    .with_context(|| format!("creating {}", dir.display()))?;
            }
            std::fs::rename(src, &dest)
                .with_context(|| format!("moving {} into cold storage", src.display()))?;
            committed.push(dest);
        }
        Ok(())
    })();
    if outcome.is_err() {
        for f in &committed {
            let _ = std::fs::remove_file(f);
        }
    }
    outcome.with_context(|| {
        format!(
            "committing the export failed; rolled back {} file(s)",
            committed.len()
        )
    })
}

/// Remove everything under `<cold_base>/.staging`. Returns how many entries
/// were removed.
///
/// Called once at startup, before any export: anything there was left by a
/// process that died before it could commit or clean up, and was never
/// visible to a reader, so deleting it loses nothing.
pub fn clear_staging(cold_base: &str) -> std::io::Result<usize> {
    let root = std::path::Path::new(cold_base).join(crate::layout::STAGING_DIR);
    let entries = match std::fs::read_dir(&root) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    let mut removed = 0;
    for entry in entries {
        let path = entry?.path();
        if path.is_dir() {
            std::fs::remove_dir_all(&path)?;
        } else {
            std::fs::remove_file(&path)?;
        }
        removed += 1;
    }
    Ok(removed)
}

/// One exported range, compared between Postgres and cold by primary key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RangeDiff {
    /// Rows in cold Parquet for the range.
    pub cold_rows: i64,
    /// Rows in Postgres for the range.
    pub pg_rows: i64,
    /// Postgres rows with no cold row of the same `(id, occurred_at)`.
    pub missing: i64,
}

/// What [`DuckEngine::reconcile_range`] should do with a [`RangeDiff`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconcilePlan {
    /// Every Postgres row is already in cold; nothing to append.
    Ready,
    /// Append the `missing` rows, then check again.
    ExportMissing,
    /// Do nothing, for the stated reason.
    Refuse(String),
}

/// Decide what to do with a range. Pure, so every branch is unit-tested.
///
/// The refusal is the whole safety argument for appending. If cold holds rows
/// for the range but NOT ONE of them matches a Postgres row by key, the likely
/// explanation is not "every row arrived late" but "the key comparison is
/// broken" — a type drift in how `id` or `occurred_at` reads back from
/// Parquet, say. Appending in that state would duplicate the entire range in
/// cold, silently, and every cold count would double. So it stops and says so.
///
/// Cold holding MORE rows than Postgres (a purge removed rows from the hot
/// side) is not a refusal: what matters for dropping is that no Postgres row
/// is missing from cold, and for appending that only missing keys are written.
pub fn plan_reconcile(d: RangeDiff) -> ReconcilePlan {
    if d.missing == 0 {
        return ReconcilePlan::Ready;
    }
    if d.cold_rows > 0 && d.missing == d.pg_rows {
        return ReconcilePlan::Refuse(format!(
            "none of the {} Postgres row(s) matched any of the {} cold row(s) by key; \
             refusing to append (it would duplicate the range)",
            d.pg_rows, d.cold_rows
        ));
    }
    ReconcilePlan::ExportMissing
}

/// Whether a range's Postgres partition may be dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Cold holds every Postgres row. `pg_rows` is how many there were when
    /// that was established: the drop must see the SAME count under its lock,
    /// or a row arrived after the check and the partition has to stay.
    Ready { pg_rows: i64 },
    /// Keep the partition, for the stated reason.
    Retain(String),
}

/// The outcome of [`DuckEngine::reconcile_range`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reconcile {
    pub pg_rows: i64,
    pub cold_rows: i64,
    /// Postgres rows still without a cold copy after this run.
    pub missing: i64,
    /// Rows this run appended to cold.
    pub exported: i64,
    pub verdict: Verdict,
}

impl DuckEngine {
    /// Open an in-memory DuckDB. Parquet is read directly from the filesystem;
    /// no persistent DuckDB database file is used.
    pub fn open() -> anyhow::Result<Self> {
        let conn = Connection::open_in_memory().context("open duckdb")?;
        // Bound memory so many concurrent cold reads can't OOM the process; see
        // `DUCK_MEMORY_MB_ENV` for why the ceiling is per-process.
        // Pin UTC so `CAST(occurred_at AS DATE)` day-bucketing matches the hot side's
        // `(occurred_at AT TIME ZONE 'UTC')::date`.
        //
        // `preserve_insertion_order=false` lets DuckDB stream an export instead of
        // buffering it to reproduce input order. Without it, exporting a wide table
        // (`error_events` carries stack traces and payloads) exhausts the ceiling
        // above and fails with "failed to pin block" long before the spill path can
        // help. Nothing downstream depends on row order within the Parquet: every
        // reader either counts, aggregates under an explicit `ORDER BY`, or inserts
        // into Postgres, which has no inherent order either. The setting does not
        // affect queries that name an `ORDER BY` of their own.
        conn.execute_batch(&format!(
            "SET memory_limit='{}MB'; SET threads=4; SET TimeZone='UTC'; \
             SET preserve_insertion_order=false; SET max_temp_directory_size='{}MB';",
            duck_memory_mb(),
            duck_max_temp_mb()
        ))?;
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

    /// Attach the Postgres database at `pg_url` as `pg`, read-only. Re-attaches
    /// if this connection already has one, so a method may call it
    /// unconditionally.
    fn attach_pg_readonly(&self, pg_url: &str) -> anyhow::Result<()> {
        self.conn
            .execute_batch("INSTALL postgres; LOAD postgres;")?;
        // ATTACH is idempotent-ish within a connection; detach if re-run.
        let _ = self.conn.execute_batch("DETACH DATABASE IF EXISTS pg;");
        self.conn.execute_batch(&format!(
            "ATTACH '{pg_url}' AS pg (TYPE postgres, READ_ONLY);"
        ))?;
        Ok(())
    }

    /// The rows of `table` in `[start, end)`, shaped for cold storage, with
    /// `extra` ANDed onto the range predicate. The table is aliased `e`.
    ///
    /// Tier 1 (migration 0068): a pooled `error_events` row carries the
    /// placeholder `[]` inline and its real trace in `error_stack_blobs`. A
    /// bare `SELECT *` would ship the placeholder plus a dangling hash into
    /// cold storage — unreadable through the cross-tier router once the hot
    /// pool row is swept. The export therefore MATERIALIZES the trace
    /// (COALESCE keeps pre-0068 and pooling-off rows intact) and EXCLUDES the
    /// hash column, so cold files keep the exact pre-Tier-1 schema and no cold
    /// reader needs to know pooling ever existed.
    fn cold_rows_query(
        table: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        extra: &str,
    ) -> String {
        let select = if table == "error_events" {
            "SELECT e.* EXCLUDE (stacktrace_sha256) \
                    REPLACE (COALESCE(b.content, e.stacktrace) AS stacktrace), \
                    year(e.occurred_at) AS year, month(e.occurred_at) AS month \
             FROM pg.error_events e \
             LEFT JOIN pg.error_stack_blobs b ON e.stacktrace_sha256 = b.sha256"
                .to_string()
        } else {
            format!(
                "SELECT e.*, year(e.occurred_at) AS year, month(e.occurred_at) AS month \
                 FROM pg.{table} e"
            )
        };
        format!(
            "{select} WHERE e.occurred_at >= TIMESTAMPTZ '{start}' \
                        AND e.occurred_at < TIMESTAMPTZ '{end}'{extra}",
            start = start.to_rfc3339(),
            end = end.to_rfc3339(),
        )
    }

    /// `COPY` each of `queries` into a private staging directory, then move the
    /// finished files into `cold_dir`. All-or-nothing, including when the
    /// process is killed.
    ///
    /// Writing straight into `cold_dir` was only all-or-nothing when the COPY
    /// returned an error: a process killed mid-COPY (an RPM upgrade restarting
    /// the unit, say) never reached the cleanup and left a truncated file
    /// inside every reader's glob, which stopped that table's tiering until
    /// someone noticed. Staged files are outside every `<cold>/<table>/**`
    /// glob, so a kill at any point during the COPY leaves nothing a reader
    /// can see, and [`clear_staging`] removes the leftovers at the next start.
    ///
    /// The commit is one `rename` per finished file, on the same filesystem.
    /// Each rename is atomic, so a reader sees a whole file or no file. If a
    /// rename fails partway, the files already moved are taken back out, so
    /// the range is exactly as it was; a kill partway through the renames
    /// leaves only complete files, which the export's key-matched reconcile
    /// already knows how to finish.
    fn copy_to_cold(&self, queries: &[String], cold_dir: &str) -> anyhow::Result<()> {
        let cold = std::path::Path::new(cold_dir);
        let stage = staging_dir_for(cold)?;
        std::fs::create_dir_all(&stage)
            .with_context(|| format!("creating staging directory {}", stage.display()))?;
        let result = self
            .copy_to_stage(queries, &stage)
            .and_then(|()| commit_staged(&stage, cold));
        // Whatever happened, the staging directory is spent: on success it is
        // empty, on failure it holds files that must not be committed.
        let _ = std::fs::remove_dir_all(&stage);
        result
    }

    fn copy_to_stage(&self, queries: &[String], stage: &std::path::Path) -> anyhow::Result<()> {
        // `APPEND` even though the directory starts empty: it makes DuckDB name
        // every file uniquely, which is what lets several COPYs share one stage
        // and what keeps the commit from colliding with files already in cold.
        for rows in queries {
            let sql = format!(
                "COPY ({rows}) \
                 TO '{}' (FORMAT PARQUET, PARTITION_BY (app_id, year, month), APPEND);",
                stage.display()
            );
            self.conn
                .execute_batch(&sql)
                .context("export failed; nothing was committed to cold storage")?;
        }
        Ok(())
    }

    /// Copy `[start, end)` of a Postgres table into hive-partitioned Parquet
    /// under `cold_dir`, appending to existing month directories. Uses DuckDB's
    /// postgres extension (needs libpq available at runtime).
    ///
    /// The range is first exported as one COPY. If that fails, it is exported
    /// again as [`EXPORT_FALLBACK_WINDOW`]-sized sub-ranges in one staged
    /// commit. A whole day of a wide table (`error_events` with its stack
    /// traces) can exhaust DuckDB's memory ceiling, and a COPY that keeps
    /// failing stalls the table's tiering for good; a sub-range holds a
    /// fraction of the rows, at the cost of more, smaller files.
    pub fn export_from_postgres(
        &self,
        pg_url: &str,
        table: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        cold_dir: &str,
    ) -> anyhow::Result<()> {
        self.attach_pg_readonly(pg_url)?;
        let whole = [Self::cold_rows_query(table, start, end, "")];
        let Err(whole_err) = self.copy_to_cold(&whole, cold_dir) else {
            return Ok(());
        };
        let windows = split_range(start, end, EXPORT_FALLBACK_WINDOW);
        if windows.len() < 2 {
            return Err(whole_err);
        }
        tracing::warn!(
            table,
            %start,
            error = %format_args!("{whole_err:#}"),
            windows = windows.len(),
            "exporting the range in one pass failed; retrying in smaller windows"
        );
        let queries: Vec<String> = windows
            .iter()
            .map(|(s, e)| Self::cold_rows_query(table, *s, *e, ""))
            .collect();
        self.copy_to_cold(&queries, cold_dir).with_context(|| {
            format!("windowed export also failed (the single pass failed with: {whole_err:#})")
        })
    }

    /// Bring cold up to date with a Postgres range that was exported before
    /// and has changed since, and say whether the Postgres copy may now go.
    ///
    /// # Why this exists
    ///
    /// A client-supplied `occurred_at` routes a late event (an SDK flushing an
    /// offline buffer, a retry after an ingest outage) into a partition that
    /// was already exported. The drop guard rightly refused to delete it,
    /// because that row was not in Parquet — but nothing ever put it there, so
    /// the partition was retained forever and Postgres never shrank. Measured
    /// on a production host: 44 partitions stuck this way, a few hundred late
    /// rows each, and the disk at 100%. Readers split at the watermark and read
    /// cold below it, so those rows were also invisible to every dashboard.
    ///
    /// # How
    ///
    /// Rows are matched by their primary key, `(id, occurred_at)` — the same
    /// key on every tiered table — so this is a set difference, not a count
    /// comparison. Only the Postgres rows with no cold counterpart are
    /// appended (`missing`), which makes the step idempotent: run it twice and
    /// the second run appends nothing.
    ///
    /// See [`plan_reconcile`] for the one case it refuses: a range where NOTHING
    /// in cold matches Postgres by key although cold has rows. That is what a
    /// broken key comparison looks like, and appending then would duplicate the
    /// whole range, so it stops instead.
    pub fn reconcile_range(
        &self,
        pg_url: &str,
        table: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        glob: &str,
        cold_dir: &str,
    ) -> anyhow::Result<Reconcile> {
        self.attach_pg_readonly(pg_url)?;
        let before = self.range_diff(table, start, end, glob)?;
        let mut rec = Reconcile {
            pg_rows: before.pg_rows,
            cold_rows: before.cold_rows,
            missing: before.missing,
            exported: 0,
            verdict: Verdict::Retain(String::new()),
        };
        match plan_reconcile(before) {
            ReconcilePlan::Ready => {
                rec.verdict = Verdict::Ready {
                    pg_rows: before.pg_rows,
                };
                return Ok(rec);
            }
            ReconcilePlan::Refuse(why) => {
                rec.verdict = Verdict::Retain(why);
                return Ok(rec);
            }
            ReconcilePlan::ExportMissing => {}
        }

        // `cold_keys` is the snapshot `range_diff` just took, so the anti-join
        // here appends exactly the rows that were missing then — plus any that
        // arrived since, which are missing too. It never appends a row that
        // cold already holds.
        let rows = Self::cold_rows_query(
            table,
            start,
            end,
            " AND NOT EXISTS (SELECT 1 FROM cold_keys k \
                              WHERE k.id = e.id AND k.occurred_at = e.occurred_at)",
        );
        self.copy_to_cold(&[rows], cold_dir)?;

        // Re-derive from the files, not from arithmetic: the proof that the
        // Postgres copy is redundant is that cold now holds every one of its
        // rows by key.
        let after = self.range_diff(table, start, end, glob)?;
        rec.exported = after.cold_rows - before.cold_rows;
        rec.pg_rows = after.pg_rows;
        rec.cold_rows = after.cold_rows;
        rec.missing = after.missing;
        rec.verdict = if after.missing == 0 {
            Verdict::Ready {
                pg_rows: after.pg_rows,
            }
        } else {
            // More late rows landed while the append ran. Not an error: the
            // next cycle appends those.
            Verdict::Retain(format!(
                "{} row(s) arrived during the append; retrying next cycle",
                after.missing
            ))
        };
        Ok(rec)
    }

    /// Rows in cold for `[start, end)`, rows in Postgres, and how many of the
    /// Postgres rows have no cold row with the same `(id, occurred_at)`.
    ///
    /// Leaves the cold keys in the temp table `cold_keys` for the caller's
    /// anti-join. `pg` must already be attached.
    ///
    /// `pg_rows` and `missing` come from ONE scan of Postgres, so they describe
    /// the same snapshot: a late row landing between two separate counts would
    /// otherwise make the pair inconsistent.
    fn range_diff(
        &self,
        table: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        glob: &str,
    ) -> anyhow::Result<RangeDiff> {
        let (s, e) = (start.to_rfc3339(), end.to_rfc3339());
        // Cast on the way in: the restore path reads `id` back with the same
        // `CAST(.. AS UUID)`, so this is the representation cold files are
        // already known to round-trip through.
        let keys_from = if self.any_files_match(glob)? {
            format!(
                "SELECT CAST(id AS UUID) AS id, CAST(occurred_at AS TIMESTAMPTZ) AS occurred_at \
                 FROM read_parquet('{glob}', hive_partitioning=true, union_by_name=true) \
                 WHERE occurred_at >= TIMESTAMPTZ '{s}' AND occurred_at < TIMESTAMPTZ '{e}'"
            )
        } else {
            "SELECT CAST(NULL AS UUID) AS id, CAST(NULL AS TIMESTAMPTZ) AS occurred_at \
             WHERE false"
                .to_string()
        };
        self.conn.execute_batch(&format!(
            "CREATE OR REPLACE TEMP TABLE cold_keys AS {keys_from};"
        ))?;
        let cold_rows: i64 = self
            .conn
            .query_row("SELECT count(*) FROM cold_keys", [], |r| r.get(0))?;
        // Distinct keys on the join side: a key duplicated in cold must not
        // multiply the Postgres row it matches, or `pg_rows` would overcount.
        let (pg_rows, missing): (i64, i64) = self.conn.query_row(
            &format!(
                "SELECT count(*), count(*) FILTER (WHERE k.id IS NULL) \
                 FROM pg.{table} e \
                 LEFT JOIN (SELECT DISTINCT id, occurred_at FROM cold_keys) k \
                   ON k.id = e.id AND k.occurred_at = e.occurred_at \
                 WHERE e.occurred_at >= TIMESTAMPTZ '{s}' AND e.occurred_at < TIMESTAMPTZ '{e}'"
            ),
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok(RangeDiff {
            cold_rows,
            pg_rows,
            missing,
        })
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

    fn diff(cold_rows: i64, pg_rows: i64, missing: i64) -> RangeDiff {
        RangeDiff {
            cold_rows,
            pg_rows,
            missing,
        }
    }

    #[test]
    fn a_range_with_nothing_missing_is_ready() {
        assert_eq!(plan_reconcile(diff(500, 500, 0)), ReconcilePlan::Ready);
        // An empty partition has nothing to lose either.
        assert_eq!(plan_reconcile(diff(0, 0, 0)), ReconcilePlan::Ready);
    }

    /// The production case: a few late rows on top of a complete export.
    #[test]
    fn late_rows_on_top_of_an_export_are_appended() {
        assert_eq!(
            plan_reconcile(diff(51_318, 52_314, 996)),
            ReconcilePlan::ExportMissing
        );
    }

    /// Cold lost its files for the range (or never had them): re-exporting
    /// the whole range duplicates nothing, because nothing is there.
    #[test]
    fn a_range_with_no_cold_rows_is_exported_in_full() {
        assert_eq!(
            plan_reconcile(diff(0, 700, 700)),
            ReconcilePlan::ExportMissing
        );
    }

    /// A purge removed hot rows after export: cold is a superset. Still only
    /// the late rows are missing, so appending them is safe.
    #[test]
    fn cold_holding_more_than_postgres_still_appends_only_what_is_missing() {
        assert_eq!(
            plan_reconcile(diff(900, 800, 5)),
            ReconcilePlan::ExportMissing
        );
        assert_eq!(plan_reconcile(diff(900, 800, 0)), ReconcilePlan::Ready);
    }

    /// Nothing in a non-empty cold range matches Postgres by key: the shape of
    /// a broken key comparison. Appending would duplicate the whole range.
    #[test]
    fn a_range_where_no_key_matches_is_refused() {
        let ReconcilePlan::Refuse(why) = plan_reconcile(diff(500, 500, 500)) else {
            panic!("a total key mismatch must be refused");
        };
        assert!(why.contains("duplicate"), "{why}");
    }

    /// Rows for two apps, shaped like an export: `PARTITION_BY (app_id, year,
    /// month)` needs those three columns.
    const TWO_APPS: &str = "SELECT * FROM (VALUES ('a', 2026, 8, 1), ('a', 2026, 8, 2), \
                            ('b', 2026, 8, 3)) t(app_id, year, month, n)";

    fn cold_root(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("sauron-tier-{tag}-{}", Uuid::new_v4()))
    }

    fn staged_leftovers(root: &std::path::Path) -> usize {
        parquet_files_under(&root.join(crate::layout::STAGING_DIR)).len()
    }

    #[test]
    fn a_successful_export_lands_every_file_in_cold_and_nothing_in_staging() {
        let root = cold_root("stage-ok");
        let cold = root.join("error_events");
        let eng = DuckEngine::open().unwrap();

        eng.copy_to_cold(&[TWO_APPS.to_string()], cold.to_str().unwrap())
            .unwrap();

        let glob = format!("{}/**/*.parquet", cold.display());
        assert_eq!(eng.count_parquet_rows(&glob).unwrap(), 3);
        assert!(cold.join("app_id=a/year=2026/month=8").is_dir());
        assert!(cold.join("app_id=b/year=2026/month=8").is_dir());
        assert_eq!(staged_leftovers(&root), 0);
        std::fs::remove_dir_all(&root).ok();
    }

    /// The whole point of staging: an export that fails partway, after some
    /// of its files were already finished, leaves cold exactly as it was.
    #[test]
    fn a_failed_export_commits_nothing() {
        let root = cold_root("stage-fail");
        let cold = root.join("error_events");
        let eng = DuckEngine::open().unwrap();
        eng.copy_to_cold(&[TWO_APPS.to_string()], cold.to_str().unwrap())
            .unwrap();
        let before = parquet_files_under(&cold);

        // The first COPY succeeds and writes files; the second fails.
        let failing = "SELECT app_id, year, month, \
                       CASE WHEN n = 3 THEN error('boom') ELSE n END AS n \
                       FROM (VALUES ('c', 2026, 9, 1), ('d', 2026, 9, 3)) t(app_id, year, month, n)";
        let err = eng
            .copy_to_cold(
                &[TWO_APPS.to_string(), failing.to_string()],
                cold.to_str().unwrap(),
            )
            .unwrap_err();
        assert!(
            format!("{err:#}").contains("nothing was committed"),
            "{err:#}"
        );

        assert_eq!(parquet_files_under(&cold), before, "cold untouched");
        assert_eq!(staged_leftovers(&root), 0, "stage cleaned up");
        std::fs::remove_dir_all(&root).ok();
    }

    /// A commit that cannot finish takes back the files it already moved.
    #[test]
    fn a_failed_commit_rolls_back_what_it_moved() {
        let root = cold_root("commit-fail");
        let cold = root.join("error_events");
        let stage = root.join(".staging/error_events-x");
        for (dir, name) in [
            ("app_id=a/year=2026/month=8", "1.parquet"),
            ("app_id=b/year=2026/month=8", "2.parquet"),
        ] {
            std::fs::create_dir_all(stage.join(dir)).unwrap();
            std::fs::write(stage.join(dir).join(name), b"PAR1").unwrap();
        }
        // The second file's destination is taken, so its move must refuse.
        let taken = cold.join("app_id=b/year=2026/month=8");
        std::fs::create_dir_all(&taken).unwrap();
        std::fs::write(taken.join("2.parquet"), b"existing").unwrap();

        let err = commit_staged(&stage, &cold).unwrap_err();
        assert!(format!("{err:#}").contains("rolled back 1 file"), "{err:#}");
        assert!(!cold.join("app_id=a/year=2026/month=8/1.parquet").exists());
        assert_eq!(std::fs::read(taken.join("2.parquet")).unwrap(), b"existing");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn clearing_staging_removes_leftovers_and_tolerates_absence() {
        let root = cold_root("clear");
        assert_eq!(clear_staging(root.to_str().unwrap()).unwrap(), 0);

        let dead = root.join(".staging/error_events-dead/app_id=a/year=2026/month=8");
        std::fs::create_dir_all(&dead).unwrap();
        std::fs::write(dead.join("truncated.parquet"), b"").unwrap();
        let cold_file = root.join("error_events/app_id=a/year=2026/month=8/x.parquet");
        std::fs::create_dir_all(cold_file.parent().unwrap()).unwrap();
        std::fs::write(&cold_file, b"PAR1").unwrap();

        assert_eq!(clear_staging(root.to_str().unwrap()).unwrap(), 1);
        assert_eq!(staged_leftovers(&root), 0);
        assert!(cold_file.exists(), "committed cold data is never touched");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn ranges_split_into_whole_windows_with_a_short_tail() {
        let s = chrono::TimeZone::with_ymd_and_hms(&Utc, 2026, 9, 18, 0, 0, 0).unwrap();
        let day = split_range(s, s + chrono::Duration::days(1), chrono::Duration::hours(1));
        assert_eq!(day.len(), 24);
        assert_eq!(day[0], (s, s + chrono::Duration::hours(1)));
        assert_eq!(day[23].1, s + chrono::Duration::days(1));
        assert!(day.windows(2).all(|w| w[0].1 == w[1].0), "contiguous");

        let tail = split_range(
            s,
            s + chrono::Duration::minutes(90),
            chrono::Duration::hours(1),
        );
        assert_eq!(tail.len(), 2);
        assert_eq!(tail[1].1 - tail[1].0, chrono::Duration::minutes(30));

        assert!(split_range(s, s, chrono::Duration::hours(1)).is_empty());
        assert_eq!(
            split_range(s, s + chrono::Duration::hours(2), chrono::Duration::zero()).len(),
            1
        );
    }

    #[test]
    fn spill_cap_falls_back_on_anything_unusable() {
        assert_eq!(parse_max_temp_mb(None), DUCK_MAX_TEMP_MB_DEFAULT);
        assert_eq!(parse_max_temp_mb(Some(" 4096 ")), 4096);
        assert_eq!(parse_max_temp_mb(Some("10GB")), DUCK_MAX_TEMP_MB_DEFAULT);
        assert_eq!(parse_max_temp_mb(Some("0")), DUCK_MAX_TEMP_MB_DEFAULT);
    }

    #[test]
    fn scanning_a_cold_dir_that_does_not_exist_yet_is_empty_not_an_error() {
        let missing = std::env::temp_dir().join(format!("sauron-tier-absent-{}", Uuid::new_v4()));
        assert!(parquet_files_under(&missing).is_empty());
    }

    #[test]
    fn memory_ceiling_falls_back_on_anything_unusable() {
        // Unset is the common case: an untouched deployment keeps the value this
        // was hard-coded to before the knob existed.
        assert_eq!(parse_memory_mb(None), DUCK_MEMORY_MB_DEFAULT);
        assert_eq!(parse_memory_mb(Some("2048")), 2048);
        assert_eq!(parse_memory_mb(Some("  2048  ")), 2048);
        // Garbage must not be able to stop tiering deployment-wide, and DuckDB
        // rejects a zero ceiling outright, so both fall back rather than error.
        assert_eq!(parse_memory_mb(Some("")), DUCK_MEMORY_MB_DEFAULT);
        assert_eq!(parse_memory_mb(Some("2048MB")), DUCK_MEMORY_MB_DEFAULT);
        assert_eq!(parse_memory_mb(Some("0")), DUCK_MEMORY_MB_DEFAULT);
        assert_eq!(parse_memory_mb(Some("-1")), DUCK_MEMORY_MB_DEFAULT);
    }

    /// The export path's actual fix. A wide table exhausts the memory ceiling
    /// while DuckDB buffers rows to reproduce input order; streaming instead is
    /// what lets `error_events` export at all. Asserted by reading the setting
    /// back, because a typo in the `SET` batch would otherwise fail silently --
    /// `execute_batch` succeeds and the OOM only reappears under a real export.
    #[test]
    fn open_streams_instead_of_preserving_insertion_order() {
        let eng = DuckEngine::open().unwrap();
        let mut stmt = eng
            .conn
            .prepare("SELECT current_setting('preserve_insertion_order')")
            .unwrap();
        // DuckDB hands this back as a BOOLEAN, not the string a `SET` takes.
        let v: bool = stmt.query_row([], |r| r.get(0)).unwrap();
        assert!(
            !v,
            "exports must stream, not buffer to preserve input order"
        );
    }

    #[test]
    fn open_applies_a_bounded_memory_ceiling() {
        let eng = DuckEngine::open().unwrap();
        let mut stmt = eng
            .conn
            .prepare("SELECT current_setting('memory_limit')")
            .unwrap();
        let raw: String = stmt.query_row([], |r| r.get(0)).unwrap();
        // DuckDB reports the ceiling back in MiB (it reads `MB` as 10^6), so the
        // number it echoes is smaller than the one we set but must still be a
        // real bound -- not the unset default, which is a large share of host RAM.
        let n: f64 = raw
            .split_whitespace()
            .next()
            .expect("a numeric prefix")
            .parse()
            .expect("a number");
        assert!(n > 0.0, "ceiling must be positive, got {raw}");
        assert!(
            n <= duck_memory_mb() as f64,
            "ceiling {raw} exceeds the configured {} MB",
            duck_memory_mb()
        );
    }

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
