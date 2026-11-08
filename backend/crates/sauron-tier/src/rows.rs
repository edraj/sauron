//! Reading whole archived ROWS back out of cold Parquet.
//!
//! Everything else that reads cold storage aggregates it (per-day counts,
//! distinct people per day), which is all the dashboards needed while every
//! page that lists individual rows was a short-window list. Two pages are not:
//! a person's profile and a session's timeline name ONE person or session and
//! no time window, so once the tier worker has moved a day out of Postgres they
//! rendered that day as nothing at all — measured on a deployment running
//! `tier.hot_days = 2`, where a three-day-old person read "0 events, 0 errors"
//! beside a Users row saying 21 and 2.
//!
//! The reader answers one question: the rows of one app's archived table that
//! match a key (a person's ids, or a session id), in a set of cold time ranges,
//! in an environment scope, newest- or oldest-first, up to a limit. Callers
//! merge the answer with the Postgres side themselves (see `sauron-api`'s
//! `tier_read`), because only they know the hot query.
//!
//! # Cost
//!
//! A person or session filter has no index in Parquet — `distinct_id` and
//! `session_id` are unsorted, so a file's min/max statistics prune nothing. The
//! cost lever is how many FILES a read opens, so the reader never globs an
//! app's whole archive: it lists the month directories on disk, keeps those
//! overlapping the requested ranges (which callers bound by the person's or
//! session's own activity span), and reads them one month at a time in the
//! requested order, stopping as soon as the limit is met. A profile that only
//! needs the last 100 rows of an active person reads one month, not a year.
//!
//! # Output
//!
//! One JSON object per row (`to_json`), so this crate stays free of the API's
//! row types. Every `TIMESTAMP WITH TIME ZONE` column is rendered as RFC 3339
//! (`2026-11-05T13:07:10.707194Z`) first: DuckDB's own JSON form,
//! `2026-11-05 13:07:10.707194+00`, is not something `chrono` deserializes.
//! JSON columns (Postgres `jsonb`) come back as JSON values, not strings. The
//! hive partition columns ride along — `app_id` as text, `year` and `month` as
//! integers — and readers ignore what they do not model.

use std::path::{Path, PathBuf};

use anyhow::Context;
use chrono::{DateTime, TimeZone, Utc};
use uuid::Uuid;

use crate::duck::DuckEngine;
use crate::plan::TimeRange;

/// Which rows of one app's archived table a read wants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowKey {
    /// `distinct_id IN (…)`: a person plus the guest ids merged into them,
    /// whose archived rows still carry the guest id because Parquet is
    /// immutable and the hot rewrite never reached it.
    DistinctIds(Vec<String>),
    /// `session_id = …`.
    Session(String),
}

/// An environment scope. Mirrors `sauron_db::scope::EnvFilter`, which this
/// crate cannot name (it has no diesel); `sauron-api` maps one onto the other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowEnv {
    All,
    One(Uuid),
    Subset(Vec<Uuid>),
    /// `environment_id IS NULL`.
    Unattributed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowOrder {
    NewestFirst,
    OldestFirst,
}

#[derive(Debug, Clone)]
pub struct RowQuery {
    pub key: RowKey,
    pub env: RowEnv,
    /// Half-open cold sub-ranges to read. The caller has already removed any
    /// restored range: a restore copies rows back into Postgres WITHOUT
    /// removing them from Parquet, so reading one here would double it.
    pub ranges: Vec<TimeRange>,
    pub order: RowOrder,
    pub limit: usize,
    /// The table's `jsonb` columns in Postgres. The export's Postgres scanner
    /// writes `jsonb` to Parquet as plain VARCHAR, so without this every JSON
    /// column would come back as a string holding JSON — `"{\"k\": 1}"` where
    /// the hot row has `{"k": 1}`. Listed by the caller from the live schema
    /// rather than hard-coded here, so a `jsonb` column added later is covered
    /// without anyone remembering this crate.
    pub json_columns: Vec<String>,
}

/// One `year=…/month=…` directory of an app's archived table.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Month {
    pub year: i32,
    pub month: u32,
    /// The directory exactly as it exists on disk. Reads are globbed from THIS,
    /// never rebuilt from `year`/`month`: a `month=08` written by anything other
    /// than the export would otherwise list as month 8 and then be read as
    /// `month=8/*.parquet`, which matches nothing — and DuckDB fails a
    /// `read_parquet` whose pattern matches nothing.
    pub dir: PathBuf,
}

impl Month {
    /// The month's half-open span, or `None` for a year chrono cannot
    /// represent (a stray `year=999999` directory must be skipped, not panic).
    fn span(&self) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
        let first = |y: i32, m: u32| Utc.with_ymd_and_hms(y, m, 1, 0, 0, 0).single();
        let (ny, nm) = if self.month == 12 {
            (self.year.checked_add(1)?, 1)
        } else {
            (self.year, self.month + 1)
        };
        Some((first(self.year, self.month)?, first(ny, nm)?))
    }

    /// Whether this month's half-open span meets any of `ranges`.
    fn overlaps(&self, ranges: &[TimeRange]) -> bool {
        self.span()
            .is_some_and(|(s, e)| ranges.iter().any(|r| r.start < e && s < r.end))
    }
}

/// The month directories under `{base}/{table}/app_id={app}` that hold at least
/// one Parquet file, ascending. An app with nothing archived is an empty list,
/// not an error.
///
/// Read off the directory tree rather than globbed because DuckDB's
/// `read_parquet` FAILS on a pattern that matches no file, and because the list
/// is what lets a read open one month at a time.
pub fn archived_months(base: &str, table: &str, app_id: Uuid) -> std::io::Result<Vec<Month>> {
    let app_dir = Path::new(base.trim_end_matches('/'))
        .join(table)
        .join(format!("app_id={app_id}"));
    let years = match std::fs::read_dir(&app_dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut out = Vec::new();
    for y in years {
        let y = y?;
        let Some(year) = partition_value::<i32>(&y.file_name(), "year") else {
            continue;
        };
        for m in std::fs::read_dir(y.path())? {
            let m = m?;
            let Some(month) = partition_value::<u32>(&m.file_name(), "month") else {
                continue;
            };
            if !(1..=12).contains(&month) || !has_parquet(&m.path())? {
                continue;
            }
            let found = Month {
                year,
                month,
                dir: m.path(),
            };
            if found.span().is_some() {
                out.push(found);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// `year=2026` → `Some(2026)` for `key = "year"`; anything else → `None`.
fn partition_value<T: std::str::FromStr>(name: &std::ffi::OsStr, key: &str) -> Option<T> {
    name.to_str()?
        .strip_prefix(key)?
        .strip_prefix('=')?
        .parse()
        .ok()
}

fn has_parquet(dir: &Path) -> std::io::Result<bool> {
    for f in std::fs::read_dir(dir)? {
        let p = f?.path();
        if p.extension().is_some_and(|x| x == "parquet") && p.is_file() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// SQL string literal: the glob is inlined rather than bound, because a
/// prepared parameter is not accepted inside `DESCRIBE`. Paths are built from
/// server configuration and a UUID, but quote anyway.
fn lit(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// The `WHERE` body for `q`, and its parameters in placeholder order. Every
/// parameter is text; DuckDB casts it to the column's type explicitly.
fn predicate(q: &RowQuery) -> (String, Vec<String>) {
    let mut params = Vec::new();
    let key = match &q.key {
        RowKey::DistinctIds(ids) => {
            params.extend(ids.iter().cloned());
            format!("distinct_id IN ({})", vec!["?"; ids.len()].join(", "))
        }
        RowKey::Session(id) => {
            params.push(id.clone());
            "session_id = ?".to_string()
        }
    };
    let ranges = q
        .ranges
        .iter()
        .map(|r| {
            params.push(r.start.to_rfc3339());
            params.push(r.end.to_rfc3339());
            "(occurred_at >= CAST(? AS TIMESTAMPTZ) AND occurred_at < CAST(? AS TIMESTAMPTZ))"
        })
        .collect::<Vec<_>>()
        .join(" OR ");
    let env = match &q.env {
        RowEnv::All => String::new(),
        RowEnv::One(id) => {
            params.push(id.to_string());
            " AND environment_id = CAST(? AS UUID)".to_string()
        }
        RowEnv::Subset(ids) if ids.is_empty() => " AND FALSE".to_string(),
        RowEnv::Subset(ids) => {
            params.extend(ids.iter().map(Uuid::to_string));
            format!(
                " AND environment_id IN ({})",
                vec!["CAST(? AS UUID)"; ids.len()].join(", ")
            )
        }
        RowEnv::Unattributed => " AND environment_id IS NULL".to_string(),
    };
    (format!("{key} AND ({ranges}){env}"), params)
}

impl DuckEngine {
    /// The archived rows of `table` for `app_id` matching `q`, as one JSON
    /// object per row (see the module docs for the shape).
    ///
    /// Reads month by month in `q.order` and stops once `q.limit` rows are in
    /// hand, so the result is the true first `limit` rows in that order. The
    /// rows are NOT sorted on return — DuckDB runs with
    /// `preserve_insertion_order=false`, so even a month's own `ORDER BY` does
    /// not survive the projection around it. Callers sort, which they must do
    /// anyway to merge with Postgres.
    pub fn archived_rows(
        &self,
        base: &str,
        table: &str,
        app_id: Uuid,
        q: &RowQuery,
    ) -> anyhow::Result<Vec<String>> {
        let nothing_to_match = match &q.key {
            RowKey::DistinctIds(ids) => ids.is_empty(),
            RowKey::Session(id) => id.is_empty(),
        };
        if q.limit == 0 || q.ranges.is_empty() || nothing_to_match {
            return Ok(Vec::new());
        }
        let mut months: Vec<Month> = archived_months(base, table, app_id)
            .with_context(|| format!("listing archived months of {table}"))?
            .into_iter()
            .filter(|m| m.overlaps(&q.ranges))
            .collect();
        if q.order == RowOrder::NewestFirst {
            months.reverse();
        }
        let (where_sql, params) = predicate(q);
        let dir_sql = if q.order == RowOrder::NewestFirst {
            "DESC"
        } else {
            "ASC"
        };

        let mut out: Vec<String> = Vec::new();
        for m in months {
            let remaining = q.limit - out.len();
            let src = format!(
                "read_parquet({}, hive_partitioning=true, union_by_name=true)",
                lit(&format!("{}/*.parquet", m.dir.display()))
            );
            let rows = format!(
                "SELECT * FROM {src} WHERE {where_sql} \
                 ORDER BY occurred_at {dir_sql}, id {dir_sql} LIMIT {remaining}"
            );
            let sql = format!(
                "SELECT to_json(t)::VARCHAR FROM ({}) t",
                self.normalized(&src, &rows, &q.json_columns)?
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let found = stmt
                .query_map(duckdb::params_from_iter(params.iter()), |r| {
                    r.get::<_, String>(0)
                })?
                .collect::<Result<Vec<_>, _>>()
                .with_context(|| {
                    format!("reading archived {table} rows for {}-{}", m.year, m.month)
                })?;
            out.extend(found);
            if out.len() >= q.limit {
                break;
            }
        }
        Ok(out)
    }

    /// `rows` re-projected into the shape the API's row types decode:
    ///
    /// * every `TIMESTAMP WITH TIME ZONE` column as RFC 3339 text (DuckDB's own
    ///   JSON form, `… +00`, is not something `chrono` parses);
    /// * every VARCHAR column named in `json_columns` cast to JSON, so
    ///   `to_json` embeds it as a value rather than a string (see
    ///   [`RowQuery::json_columns`] for why the export loses the type).
    ///
    /// Columns are discovered per read rather than assumed: each table has its
    /// own set, and an older month may lack a column the current schema has —
    /// naming a missing column in `REPLACE` is an error, so only columns the
    /// month's files actually carry are touched.
    fn normalized(&self, src: &str, rows: &str, json_columns: &[String]) -> anyhow::Result<String> {
        let mut stmt = self
            .conn
            .prepare(&format!("DESCRIBE SELECT * FROM {src}"))?;
        let cols = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        let replaces: Vec<String> = cols
            .into_iter()
            .filter_map(|(name, ty)| {
                let q = format!("\"{}\"", name.replace('"', "\"\""));
                if ty.starts_with("TIMESTAMP WITH TIME ZONE") {
                    Some(format!("strftime({q}, '%Y-%m-%dT%H:%M:%S.%fZ') AS {q}"))
                } else if ty == "VARCHAR" && json_columns.contains(&name) {
                    Some(format!("CAST({q} AS JSON) AS {q}"))
                } else {
                    None
                }
            })
            .collect();
        Ok(if replaces.is_empty() {
            rows.to_string()
        } else {
            format!("SELECT * REPLACE ({}) FROM ({rows})", replaces.join(", "))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn ts(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    fn range(a: &str, b: &str) -> TimeRange {
        TimeRange {
            start: ts(a),
            end: ts(b),
        }
    }

    /// Everything up to the start of 2027 — "all of cold" for these fixtures.
    fn all_cold() -> Vec<TimeRange> {
        vec![range("2020-01-01T00:00:00Z", "2027-01-01T00:00:00Z")]
    }

    struct Fixture {
        dir: std::path::PathBuf,
        app: Uuid,
        prod: Uuid,
        stg: Uuid,
    }

    impl Fixture {
        fn base(&self) -> String {
            self.dir.display().to_string()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// An `analytics_events`-shaped archive written the way the export writes
    /// one (`PARTITION_BY (app_id, year, month)`), across three months, two
    /// environments plus unattributed, two people and a guest id, two sessions,
    /// and a second app that must never leak in.
    fn fixture() -> Fixture {
        let dir = std::env::temp_dir().join(format!("sauron-rows-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = Fixture {
            dir,
            app: Uuid::new_v4(),
            prod: Uuid::new_v4(),
            stg: Uuid::new_v4(),
        };
        let other_app = Uuid::new_v4();
        let rows = [
            // (app, env, distinct_id, session, occurred_at, name)
            (
                f.app,
                Some(f.prod),
                "p1",
                "s1",
                "2026-09-30 23:59:59.5+00",
                "sep",
            ),
            (
                f.app,
                Some(f.prod),
                "p1",
                "s1",
                "2026-10-05 10:00:00+00",
                "oct-a",
            ),
            (
                f.app,
                Some(f.stg),
                "p1",
                "s2",
                "2026-10-06 10:00:00+00",
                "oct-stg",
            ),
            (
                f.app,
                None,
                "p1",
                "s2",
                "2026-10-07 10:00:00+00",
                "oct-unattr",
            ),
            (
                f.app,
                Some(f.prod),
                "guest",
                "s1",
                "2026-10-08 10:00:00+00",
                "oct-guest",
            ),
            (
                f.app,
                Some(f.prod),
                "p2",
                "s3",
                "2026-10-09 10:00:00+00",
                "oct-p2",
            ),
            (
                f.app,
                Some(f.prod),
                "p1",
                "s1",
                "2026-11-01 08:00:00.123456+00",
                "nov-a",
            ),
            (
                f.app,
                Some(f.prod),
                "p1",
                "s1",
                "2026-11-02 08:00:00+00",
                "nov-b",
            ),
            (
                other_app,
                Some(f.prod),
                "p1",
                "s1",
                "2026-11-03 08:00:00+00",
                "other-app",
            ),
        ];
        let values = rows
            .iter()
            .map(|(app, env, did, sid, at, name)| {
                let env = env.map_or("NULL".to_string(), |e| format!("'{e}'::UUID"));
                format!(
                    "(uuid(), '{app}'::UUID, {env}, '{did}', '{sid}', TIMESTAMPTZ '{at}', \
                      '{name}', '{{\"k\":[1,2]}}'::JSON, '{{\"os\":\"{name}\"}}')"
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        let eng = DuckEngine::open().unwrap();
        eng.conn
            .execute_batch(&format!(
                "COPY (SELECT *, year(occurred_at) AS year, month(occurred_at) AS month FROM (VALUES {values}) \
                   AS v(id, app_id, environment_id, distinct_id, session_id, occurred_at, name, properties, context)) \
                 TO '{}/analytics_events' (FORMAT PARQUET, PARTITION_BY (app_id, year, month), APPEND)",
                f.dir.display()
            ))
            .unwrap();
        f
    }

    fn query(key: RowKey, env: RowEnv, order: RowOrder, limit: usize) -> RowQuery {
        RowQuery {
            key,
            env,
            ranges: all_cold(),
            order,
            limit,
            json_columns: vec!["context".into()],
        }
    }

    fn names(rows: &[String]) -> Vec<String> {
        let mut v: Vec<(String, String)> = rows
            .iter()
            .map(|r| {
                let j: Value = serde_json::from_str(r).unwrap();
                (
                    j["occurred_at"].as_str().unwrap().to_string(),
                    j["name"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        v.sort();
        v.into_iter().map(|(_, n)| n).collect()
    }

    fn ids(v: &[&str]) -> RowKey {
        RowKey::DistinctIds(v.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn lists_only_month_directories_that_hold_parquet() {
        let f = fixture();
        std::fs::create_dir_all(f.dir.join(format!(
            "analytics_events/app_id={}/year=2026/month=3",
            f.app
        )))
        .unwrap();
        let months: Vec<(i32, u32)> = archived_months(&f.base(), "analytics_events", f.app)
            .unwrap()
            .into_iter()
            .map(|m| (m.year, m.month))
            .collect();
        assert_eq!(months, vec![(2026, 9), (2026, 10), (2026, 11)]);
        assert!(
            archived_months(&f.base(), "analytics_events", Uuid::new_v4())
                .unwrap()
                .is_empty()
        );
        assert!(archived_months(&f.base(), "error_events", f.app)
            .unwrap()
            .is_empty());
    }

    /// A zero-padded month directory is read through its real name, and a
    /// year chrono cannot represent is skipped rather than panicking.
    #[test]
    fn reads_month_directories_by_their_real_name() {
        let f = fixture();
        let app_dir = f.dir.join(format!("analytics_events/app_id={}", f.app));
        // Move November's files under a zero-padded name.
        std::fs::rename(
            app_dir.join("year=2026/month=11"),
            app_dir.join("year=2026/month=011"),
        )
        .unwrap();
        let bogus = app_dir.join("year=999999/month=1");
        std::fs::create_dir_all(&bogus).unwrap();
        std::fs::copy(
            std::fs::read_dir(app_dir.join("year=2026/month=011"))
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .path(),
            bogus.join("x.parquet"),
        )
        .unwrap();
        let eng = DuckEngine::open().unwrap();
        let rows = eng
            .archived_rows(
                &f.base(),
                "analytics_events",
                f.app,
                &query(ids(&["p1"]), RowEnv::All, RowOrder::NewestFirst, 2),
            )
            .unwrap();
        assert_eq!(names(&rows), vec!["nov-a", "nov-b"]);
    }

    #[test]
    fn newest_first_returns_the_true_newest_rows_and_stops_early() {
        let f = fixture();
        let eng = DuckEngine::open().unwrap();
        let rows = eng
            .archived_rows(
                &f.base(),
                "analytics_events",
                f.app,
                &query(ids(&["p1"]), RowEnv::All, RowOrder::NewestFirst, 3),
            )
            .unwrap();
        assert_eq!(names(&rows), vec!["oct-unattr", "nov-a", "nov-b"]);
    }

    #[test]
    fn oldest_first_reads_from_the_other_end() {
        let f = fixture();
        let eng = DuckEngine::open().unwrap();
        let rows = eng
            .archived_rows(
                &f.base(),
                "analytics_events",
                f.app,
                &query(ids(&["p1"]), RowEnv::All, RowOrder::OldestFirst, 2),
            )
            .unwrap();
        assert_eq!(names(&rows), vec!["sep", "oct-a"]);
    }

    #[test]
    fn honours_every_environment_scope() {
        let f = fixture();
        let eng = DuckEngine::open().unwrap();
        let read = |env: RowEnv| {
            names(
                &eng.archived_rows(
                    &f.base(),
                    "analytics_events",
                    f.app,
                    &query(ids(&["p1"]), env, RowOrder::NewestFirst, 100),
                )
                .unwrap(),
            )
        };
        assert_eq!(
            read(RowEnv::One(f.prod)),
            vec!["sep", "oct-a", "nov-a", "nov-b"]
        );
        assert_eq!(read(RowEnv::One(f.stg)), vec!["oct-stg"]);
        assert_eq!(read(RowEnv::Unattributed), vec!["oct-unattr"]);
        assert_eq!(read(RowEnv::Subset(vec![f.stg, f.prod])).len(), 5);
        assert!(read(RowEnv::Subset(vec![])).is_empty());
        assert_eq!(read(RowEnv::All).len(), 6);
    }

    #[test]
    fn several_ids_read_as_one_person_and_other_people_stay_out() {
        let f = fixture();
        let eng = DuckEngine::open().unwrap();
        let rows = eng
            .archived_rows(
                &f.base(),
                "analytics_events",
                f.app,
                &query(
                    ids(&["p1", "guest"]),
                    RowEnv::One(f.prod),
                    RowOrder::NewestFirst,
                    100,
                ),
            )
            .unwrap();
        assert_eq!(
            names(&rows),
            vec!["sep", "oct-a", "oct-guest", "nov-a", "nov-b"]
        );
    }

    #[test]
    fn a_session_key_reads_that_session_only() {
        let f = fixture();
        let eng = DuckEngine::open().unwrap();
        let rows = eng
            .archived_rows(
                &f.base(),
                "analytics_events",
                f.app,
                &query(
                    RowKey::Session("s2".into()),
                    RowEnv::All,
                    RowOrder::OldestFirst,
                    100,
                ),
            )
            .unwrap();
        assert_eq!(names(&rows), vec!["oct-stg", "oct-unattr"]);
    }

    /// A restored island is excluded by the caller passing two ranges around
    /// it; nothing inside the gap may come back, and the half-open bounds must
    /// hold at both edges.
    #[test]
    fn reads_only_inside_the_given_ranges() {
        let f = fixture();
        let eng = DuckEngine::open().unwrap();
        let q = RowQuery {
            key: ids(&["p1"]),
            env: RowEnv::All,
            ranges: vec![
                range("2026-09-01T00:00:00Z", "2026-10-05T10:00:00Z"),
                range("2026-10-07T10:00:00Z", "2026-11-02T08:00:00Z"),
            ],
            order: RowOrder::NewestFirst,
            limit: 100,
            json_columns: vec![],
        };
        let rows = eng
            .archived_rows(&f.base(), "analytics_events", f.app, &q)
            .unwrap();
        assert_eq!(names(&rows), vec!["sep", "oct-unattr", "nov-a"]);
    }

    #[test]
    fn renders_timestamps_as_rfc3339_and_json_columns_as_json() {
        let f = fixture();
        let eng = DuckEngine::open().unwrap();
        let rows = eng
            .archived_rows(
                &f.base(),
                "analytics_events",
                f.app,
                &query(ids(&["p1"]), RowEnv::All, RowOrder::NewestFirst, 2),
            )
            .unwrap();
        let nov_a = rows
            .iter()
            .map(|r| serde_json::from_str::<Value>(r).unwrap())
            .find(|j| j["name"] == "nov-a")
            .unwrap();
        assert_eq!(nov_a["occurred_at"], "2026-11-01T08:00:00.123456Z");
        let parsed: DateTime<Utc> = nov_a["occurred_at"].as_str().unwrap().parse().unwrap();
        assert_eq!(parsed, ts("2026-11-01T08:00:00.123456Z"));
        assert_eq!(nov_a["properties"], serde_json::json!({ "k": [1, 2] }));
        // `context` is VARCHAR in the file, as a real export writes `jsonb`;
        // named in `json_columns`, it must come back as a value. `name` is
        // VARCHAR too and NOT named, so it stays text.
        assert_eq!(nov_a["context"], serde_json::json!({ "os": "nov-a" }));
        assert_eq!(nov_a["name"], "nov-a");
        assert_eq!(nov_a["app_id"], f.app.to_string());
    }

    #[test]
    fn empty_inputs_read_nothing_and_touch_no_file() {
        let eng = DuckEngine::open().unwrap();
        let app = Uuid::new_v4();
        // A base that does not exist: any file access would error.
        let base = "/nonexistent/sauron-rows";
        let mut q = query(ids(&[]), RowEnv::All, RowOrder::NewestFirst, 10);
        assert!(eng
            .archived_rows(base, "analytics_events", app, &q)
            .unwrap()
            .is_empty());
        q.key = ids(&["p1"]);
        q.limit = 0;
        assert!(eng
            .archived_rows(base, "analytics_events", app, &q)
            .unwrap()
            .is_empty());
        q.limit = 10;
        q.ranges.clear();
        assert!(eng
            .archived_rows(base, "analytics_events", app, &q)
            .unwrap()
            .is_empty());
        // And an app with no archive at all is empty, not an error.
        q.ranges = all_cold();
        assert!(eng
            .archived_rows(base, "analytics_events", app, &q)
            .unwrap()
            .is_empty());
    }
}
