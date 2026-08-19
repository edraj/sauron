//! Storage & records report: per-app hot(Postgres)/cold(Parquet) record counts,
//! estimated hot bytes, and the cold Parquet file inventory. Postgres queries,
//! DuckDB per-app counts, and the /cold filesystem walk run concurrently, then
//! are assembled by app_id.
//!
//! The report is **tenant-scoped**: the caller sees only apps in orgs where they
//! hold `org:manage`, and every figure (including the database totals) is
//! computed over exactly that set. Nothing here reveals the existence or size of
//! another tenant's data.
//!
//! It is also **cached**. Assembling it costs a per-app aggregate over the three
//! largest tables plus a DuckDB pass and a filesystem walk, so an uncached
//! endpoint would be an easy way to turn one cheap request into minutes of I/O.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use sauron_db::{conn, repo};
use sauron_tier::duck::DuckEngine;
use sauron_tier::{parse_cold_path, TIERED_TABLES};

use crate::AppState;

/// How long an assembled report stays warm. Storage figures move slowly; a
/// minute of staleness is invisible to a human reading the page but collapses
/// repeated loads (and refresh-spamming) onto one computation.
const CACHE_TTL_SECS: u64 = 60;

/// Cap on the per-app cold-file inventory returned to the client. A long-lived
/// app can accumulate tens of thousands of Parquet files; the full list is
/// unbounded response size and memory for no added insight.
const MAX_COLD_FILES_PER_APP: usize = 200;

#[derive(Serialize, Deserialize)]
pub struct StorageReport {
    pub database: DatabaseInfo,
    pub apps: Vec<AppStorage>,
}

#[derive(Serialize, Deserialize)]
pub struct DatabaseInfo {
    /// Postgres bytes attributable to the caller's visible apps. For a
    /// full-scope caller this *is* `pg_database_size`; otherwise it is the
    /// physical size apportioned by row share (see [`apportion`]).
    pub total_bytes: i64,
    /// True `pg_database_size(current_database())` — indexes, TOAST, bloat and
    /// every non-tiered table included. `None` unless the caller manages every
    /// org in the deployment, because the physical size of a shared database is
    /// necessarily the sum over all tenants.
    #[serde(default)]
    pub physical_bytes: Option<i64>,
    /// Total cold/Parquet bytes across the caller's visible apps, so the page
    /// can show hot and cold side by side rather than only the Postgres half.
    #[serde(default)]
    pub cold_bytes: i64,
    /// Whether the caller's org set covers the whole deployment. Drives the
    /// page's wording: exact sizes vs. an apportioned estimate.
    #[serde(default)]
    pub full_scope: bool,
    pub tables: Vec<TableSize>,
}

#[derive(Serialize, Deserialize)]
pub struct TableSize {
    pub name: String,
    /// Physical bytes attributed to the caller's visible apps.
    pub total_bytes: i64,
    pub hot_rows: i64,
    /// True for the hot/cold tiered tables, which are the only ones carrying an
    /// `app_id` and therefore the only ones with cold counterparts. Non-tiered
    /// tables are listed only in the full-scope view.
    #[serde(default)]
    pub tiered: bool,
}

#[derive(Serialize, Deserialize)]
pub struct AppStorage {
    pub app_id: Uuid,
    pub app_name: String,
    /// The project the app hangs off. Defaulted on read so a report cached by
    /// an older build — one that had no such field — still deserializes rather
    /// than missing the cache on every request until the entry expires.
    #[serde(default)]
    pub project_name: String,
    pub org_name: String,
    pub tables: Vec<AppTableStorage>,
    pub hot_rows_total: i64,
    pub cold_rows_total: i64,
    pub cold_bytes_total: i64,
    pub estimated_hot_bytes_total: i64,
    pub cold_files: Vec<ColdFile>,
    /// Total cold files for the app; `cold_files` is truncated to
    /// [`MAX_COLD_FILES_PER_APP`] so the response stays bounded.
    pub cold_files_total: usize,
}

#[derive(Serialize, Deserialize)]
pub struct AppTableStorage {
    pub name: String,
    pub hot_rows: i64,
    pub cold_rows: i64,
    pub cold_bytes: i64,
    /// The table's physical bytes apportioned to this app by row share, so it
    /// carries this app's share of index, TOAST and page overhead rather than
    /// bare column widths.
    pub estimated_hot_bytes: i64,
}

#[derive(Serialize, Deserialize)]
pub struct ColdFile {
    pub path: String,
    pub bytes: i64,
}

/// One cold file found by the /cold walk, keyed to its (table, app_id).
struct WalkedFile {
    table: String,
    app_id: Uuid,
    path: String,
    bytes: i64,
}

/// Assemble the report for the apps in `org_ids`, serving a cached copy when one
/// is warm. `cache_key` distinguishes callers with different visible scopes.
pub async fn collect_storage_cached(
    state: &AppState,
    org_ids: &[Uuid],
    cache_key: &str,
) -> anyhow::Result<StorageReport> {
    if let Ok(Some(hit)) = state.redis.get(cache_key).await {
        if let Ok(report) = serde_json::from_str::<StorageReport>(&hit) {
            return Ok(report);
        }
    }
    let report = collect_storage(state, org_ids).await?;
    if let Ok(json) = serde_json::to_string(&report) {
        let _ = state.redis.set_ex(cache_key, &json, CACHE_TTL_SECS).await;
    }
    Ok(report)
}

pub async fn collect_storage(state: &AppState, org_ids: &[Uuid]) -> anyhow::Result<StorageReport> {
    let cold_path = state.cfg.tier_cold_path.clone();

    // --- Postgres branch (async, one connection) ---
    let pool = state.pool.clone();
    let scope_orgs = org_ids.to_vec();
    let pg = async move {
        let mut c = conn(&pool).await?;
        let apps = repo::list_apps_with_org_scoped(&mut c, &scope_orgs).await?;
        let app_ids: Vec<Uuid> = apps.iter().map(|a| a.app_id).collect();

        // Does the caller administer every tenant? If so there is no other
        // tenant whose volume physical sizes could disclose, and the report can
        // show real bytes instead of an apportioned share.
        let full_scope = scope_orgs.len() as i64 >= repo::org_count(&mut c).await?;

        // Physical size per table, keyed by name. This is the number that
        // reconciles with `pg_database_size`; the old rows × pg_stats.avg_width
        // estimate omitted indexes, TOAST, page overhead and dead tuples, which
        // is why it read several times low.
        let physical = repo::all_table_sizes(&mut c).await?;
        let phys_by_name: HashMap<String, i64> =
            physical.iter().map(|r| (r.name.clone(), r.bytes)).collect();

        let mut tables = Vec::new();
        // hot_rows[table][app_id], and the per-table (physical, total_rows) pair
        // the per-app apportioning divides through.
        let mut hot: HashMap<&'static str, HashMap<Uuid, i64>> = HashMap::new();
        let mut share: HashMap<&'static str, (i64, i64)> = HashMap::new();
        let mut scoped_total = 0i64;
        for t in TIERED_TABLES {
            let table_bytes = phys_by_name.get(t.name).copied().unwrap_or(0);
            let table_rows = repo::table_row_estimate(&mut c, t.name).await?;
            // Scoped: only the caller's apps are counted, and the app_id filter
            // keeps the planner on the app-keyed index instead of a full scan.
            let rows = if app_ids.is_empty() {
                Vec::new()
            } else {
                repo::hot_rows_by_app_scoped(&mut c, t.name, &app_ids).await?
            };
            let total_hot: i64 = rows.iter().map(|r| r.n).sum();
            let attributed = if full_scope {
                table_bytes
            } else {
                apportion(table_bytes, total_hot, table_rows)
            };
            scoped_total = scoped_total.saturating_add(attributed);
            tables.push(TableSize {
                name: t.name.to_string(),
                total_bytes: attributed,
                hot_rows: total_hot,
                tiered: true,
            });
            hot.insert(t.name, rows.into_iter().map(|r| (r.app_id, r.n)).collect());
            share.insert(t.name, (table_bytes, table_rows));
        }

        // The rest of the schema (event_users, sessions, issues, …) has no
        // app_id to apportion by, so it appears only in the full-scope view —
        // where it is exactly what makes the table list add up to the database
        // total instead of falling short of it.
        let db_bytes = repo::db_total_bytes(&mut c).await?;
        let (total_bytes, physical_bytes) = if full_scope {
            let tiered: std::collections::HashSet<&str> =
                TIERED_TABLES.iter().map(|t| t.name).collect();
            for r in &physical {
                if tiered.contains(r.name.as_str()) || r.bytes == 0 {
                    continue;
                }
                tables.push(TableSize {
                    name: r.name.clone(),
                    total_bytes: r.bytes,
                    hot_rows: r.rows,
                    tiered: false,
                });
            }
            (db_bytes, Some(db_bytes))
        } else {
            (scoped_total, None)
        };
        tables.sort_by(|a, b| b.total_bytes.cmp(&a.total_bytes));
        Ok::<_, anyhow::Error>((
            total_bytes,
            physical_bytes,
            full_scope,
            tables,
            apps,
            hot,
            share,
        ))
    };

    // --- DuckDB branch (blocking): cold rows per (table, app_id) ---
    let cold_path_d = cold_path.clone();
    let cold_counts = tokio::task::spawn_blocking(
        move || -> anyhow::Result<HashMap<&'static str, HashMap<Uuid, i64>>> {
            let eng = DuckEngine::open()?;
            let mut out: HashMap<&'static str, HashMap<Uuid, i64>> = HashMap::new();
            for t in TIERED_TABLES {
                let glob = format!(
                    "{}/{}/**/*.parquet",
                    cold_path_d.trim_end_matches('/'),
                    t.name
                );
                let counts = eng.counts_by_app(&glob)?;
                out.insert(t.name, counts.into_iter().collect());
            }
            Ok(out)
        },
    );

    // --- Filesystem branch (blocking): cold files per (table, app_id) ---
    let cold_path_w = cold_path.clone();
    let walked = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<WalkedFile>> {
        walk_cold(&cold_path_w)
    });

    let (pg_res, cold_res, walk_res) = tokio::join!(pg, cold_counts, walked);
    let (total_bytes, physical_bytes, full_scope, tables, apps, hot, share) = pg_res?;
    let cold_counts = cold_res??;
    let walked = walk_res??;

    let visible_ids: std::collections::HashSet<Uuid> = apps.iter().map(|a| a.app_id).collect();

    // Group walked files by (app_id, table), discarding anything outside the
    // caller's scope — the cold directory holds every tenant's Parquet files and
    // their paths embed app ids.
    let mut files_by_app: HashMap<Uuid, Vec<ColdFile>> = HashMap::new();
    let mut cold_bytes: HashMap<(Uuid, &'static str), i64> = HashMap::new();
    for f in walked {
        if !visible_ids.contains(&f.app_id) {
            continue;
        }
        // Match the walked file's table string to a canonical TIERED_TABLES name.
        if let Some(t) = TIERED_TABLES.iter().find(|t| t.name == f.table) {
            *cold_bytes.entry((f.app_id, t.name)).or_insert(0) += f.bytes;
            files_by_app.entry(f.app_id).or_default().push(ColdFile {
                path: f.path,
                bytes: f.bytes,
            });
        }
    }

    let apps_out: Vec<AppStorage> = apps
        .into_iter()
        .map(|a| {
            let mut per_table = Vec::new();
            let (mut hr, mut cr, mut cb, mut ehb) = (0i64, 0i64, 0i64, 0i64);
            for t in TIERED_TABLES {
                let hot_rows = hot
                    .get(t.name)
                    .and_then(|m| m.get(&a.app_id))
                    .copied()
                    .unwrap_or(0);
                let cold_rows = cold_counts
                    .get(t.name)
                    .and_then(|m| m.get(&a.app_id))
                    .copied()
                    .unwrap_or(0);
                let cold_b = cold_bytes.get(&(a.app_id, t.name)).copied().unwrap_or(0);
                let (table_bytes, table_rows) = share.get(t.name).copied().unwrap_or((0, 0));
                let est = apportion(table_bytes, hot_rows, table_rows);
                hr += hot_rows;
                cr += cold_rows;
                cb += cold_b;
                ehb += est;
                per_table.push(AppTableStorage {
                    name: t.name.to_string(),
                    hot_rows,
                    cold_rows,
                    cold_bytes: cold_b,
                    estimated_hot_bytes: est,
                });
            }
            let mut files = files_by_app.remove(&a.app_id).unwrap_or_default();
            files.sort_by(|x, y| x.path.cmp(&y.path));
            let cold_files_total = files.len();
            files.truncate(MAX_COLD_FILES_PER_APP);
            AppStorage {
                app_id: a.app_id,
                app_name: a.app_name,
                project_name: a.project_name,
                org_name: a.org_name,
                tables: per_table,
                hot_rows_total: hr,
                cold_rows_total: cr,
                cold_bytes_total: cb,
                estimated_hot_bytes_total: ehb,
                cold_files: files,
                cold_files_total,
            }
        })
        .collect();

    // NOTE: an "orphaned cold storage" bucket (data whose app row is gone) used
    // to be appended here. Under tenant scoping it cannot be reported: from
    // inside one org, another org's cold data is indistinguishable from a
    // deleted app's, so surfacing it would leak exactly what the scoping is
    // there to prevent. Reclaiming orphaned Parquet belongs in an operator-side
    // task with deployment-wide access, not in a tenant-facing report.

    let cold_total: i64 = apps_out
        .iter()
        .fold(0i64, |acc, a| acc.saturating_add(a.cold_bytes_total));

    Ok(StorageReport {
        database: DatabaseInfo {
            total_bytes,
            physical_bytes,
            cold_bytes: cold_total,
            full_scope,
            tables,
        },
        apps: apps_out,
    })
}

/// Split `table_bytes` in proportion to `part_rows / total_rows`.
///
/// This is how a partial-scope caller gets a figure that still carries index,
/// TOAST and page overhead without ever being told the absolute size of a table
/// they only partly own. Widened to `i128` because a large table times a large
/// row count overflows `i64` well before either factor is implausible.
fn apportion(table_bytes: i64, part_rows: i64, total_rows: i64) -> i64 {
    if table_bytes <= 0 || part_rows <= 0 || total_rows <= 0 {
        return 0;
    }
    // Row counts come from two different sources (an exact per-app count and a
    // planner estimate for the whole table), so the ratio can land just above 1.
    let part = part_rows.min(total_rows) as i128;
    let scaled = (table_bytes as i128) * part / (total_rows as i128);
    scaled.min(i64::MAX as i128) as i64
}

/// Recursively collect `*.parquet` files under `base`, keyed to (table, app_id)
/// via the hive path. Missing base dir ⇒ empty (nothing tiered yet).
fn walk_cold(base: &str) -> anyhow::Result<Vec<WalkedFile>> {
    let base = Path::new(base);
    let mut out = Vec::new();
    if !base.exists() {
        return Ok(out);
    }
    let mut stack = vec![base.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("parquet") {
                let rel = path
                    .strip_prefix(base)
                    .ok()
                    .and_then(|p| p.to_str())
                    .unwrap_or("");
                if let Some(key) = parse_cold_path(rel) {
                    let bytes = entry.metadata().map(|m| m.len() as i64).unwrap_or(0);
                    out.push(WalkedFile {
                        table: key.table,
                        app_id: key.app_id,
                        path: rel.to_string(),
                        bytes,
                    });
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::apportion;

    #[test]
    fn splits_by_row_share() {
        assert_eq!(apportion(1000, 25, 100), 250);
        assert_eq!(apportion(1000, 100, 100), 1000);
    }

    #[test]
    fn degenerate_inputs_are_zero_not_a_panic() {
        // A never-analyzed table reports 0 rows; dividing through it must not
        // divide by zero, and an empty table must not claim bytes.
        assert_eq!(apportion(1000, 10, 0), 0);
        assert_eq!(apportion(0, 10, 100), 0);
        assert_eq!(apportion(1000, 0, 100), 0);
        assert_eq!(apportion(-1, 10, 100), 0);
    }

    #[test]
    fn part_above_total_clamps_to_the_whole_table() {
        // `part_rows` is an exact count while `total_rows` is a planner
        // estimate, so the ratio really can exceed 1 between autovacuums. It
        // must not attribute more than the table's own size.
        assert_eq!(apportion(1000, 150, 100), 1000);
    }

    #[test]
    fn large_tables_do_not_overflow() {
        // 4 TiB across 2e9 rows: the i64 product would wrap; i128 must not.
        let bytes = 4i64 << 40;
        assert_eq!(apportion(bytes, 1_000_000_000, 2_000_000_000), bytes / 2);
    }
}
