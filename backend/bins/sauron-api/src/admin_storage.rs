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
    /// Estimated bytes across the caller's visible apps (NOT the physical size
    /// of the database, which would disclose other tenants' volume).
    pub total_bytes: i64,
    pub tables: Vec<TableSize>,
}

#[derive(Serialize, Deserialize)]
pub struct TableSize {
    pub name: String,
    /// Estimated bytes for this table across the caller's visible apps.
    pub total_bytes: i64,
    pub hot_rows: i64,
}

#[derive(Serialize, Deserialize)]
pub struct AppStorage {
    pub app_id: Uuid,
    pub app_name: String,
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
    /// Approximate (rows × avg row width from pg_stats).
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
        let mut tables = Vec::new();
        // hot_rows[table][app_id] and avg_width[table]
        let mut hot: HashMap<&'static str, HashMap<Uuid, i64>> = HashMap::new();
        let mut avg_width: HashMap<&'static str, i64> = HashMap::new();
        let mut total_bytes = 0i64;
        for t in TIERED_TABLES {
            let width = repo::table_avg_row_width(&mut c, t.name).await?;
            // Scoped: only the caller's apps are counted, and the app_id filter
            // keeps the planner on the app-keyed index instead of a full scan.
            let rows = if app_ids.is_empty() {
                Vec::new()
            } else {
                repo::hot_rows_by_app_scoped(&mut c, t.name, &app_ids).await?
            };
            let total_hot: i64 = rows.iter().map(|r| r.n).sum();
            // Estimated (rows × avg width) rather than physical relation size:
            // the physical size covers every tenant in the deployment.
            let est_bytes = total_hot.saturating_mul(width);
            total_bytes = total_bytes.saturating_add(est_bytes);
            tables.push(TableSize {
                name: t.name.to_string(),
                total_bytes: est_bytes,
                hot_rows: total_hot,
            });
            hot.insert(t.name, rows.into_iter().map(|r| (r.app_id, r.n)).collect());
            avg_width.insert(t.name, width);
        }
        Ok::<_, anyhow::Error>((total_bytes, tables, apps, hot, avg_width))
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
    let (total_bytes, tables, apps, hot, avg_width) = pg_res?;
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
                let est = avg_width.get(t.name).copied().unwrap_or(0) * hot_rows;
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

    Ok(StorageReport {
        database: DatabaseInfo {
            total_bytes,
            tables,
        },
        apps: apps_out,
    })
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
