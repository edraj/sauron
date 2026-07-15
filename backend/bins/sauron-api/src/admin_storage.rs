//! Admin storage report: total DB size + per-app hot(Postgres)/cold(Parquet)
//! record counts, estimated hot bytes, and the cold Parquet file inventory.
//! Postgres queries, DuckDB per-app counts, and the /cold filesystem walk run
//! concurrently, then are assembled by app_id.

use std::collections::HashMap;
use std::path::Path;

use serde::Serialize;
use uuid::Uuid;

use sauron_db::{conn, repo};
use sauron_tier::duck::DuckEngine;
use sauron_tier::{parse_cold_path, TIERED_TABLES};

use crate::AppState;

#[derive(Serialize)]
pub struct StorageReport {
    pub database: DatabaseInfo,
    pub apps: Vec<AppStorage>,
}

#[derive(Serialize)]
pub struct DatabaseInfo {
    pub total_bytes: i64,
    pub tables: Vec<TableSize>,
}

#[derive(Serialize)]
pub struct TableSize {
    pub name: String,
    pub total_bytes: i64,
    pub hot_rows: i64,
}

#[derive(Serialize)]
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
}

#[derive(Serialize)]
pub struct AppTableStorage {
    pub name: String,
    pub hot_rows: i64,
    pub cold_rows: i64,
    pub cold_bytes: i64,
    /// Approximate (rows × avg row width from pg_stats).
    pub estimated_hot_bytes: i64,
}

#[derive(Serialize)]
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

pub async fn collect_storage(state: &AppState) -> anyhow::Result<StorageReport> {
    let cold_path = state.cfg.tier_cold_path.clone();

    // --- Postgres branch (async, one connection) ---
    let pool = state.pool.clone();
    let pg = async move {
        let mut c = conn(&pool).await?;
        let total_bytes = repo::db_total_bytes(&mut c).await?;
        let apps = repo::list_apps_with_org(&mut c).await?;
        let mut tables = Vec::new();
        // hot_rows[table][app_id] and avg_width[table]
        let mut hot: HashMap<&'static str, HashMap<Uuid, i64>> = HashMap::new();
        let mut avg_width: HashMap<&'static str, i64> = HashMap::new();
        for t in TIERED_TABLES {
            let size = repo::table_total_bytes(&mut c, t.name).await?;
            let width = repo::table_avg_row_width(&mut c, t.name).await?;
            let rows = repo::hot_rows_by_app(&mut c, t.name).await?;
            let total_hot: i64 = rows.iter().map(|r| r.n).sum();
            tables.push(TableSize { name: t.name.to_string(), total_bytes: size, hot_rows: total_hot });
            hot.insert(t.name, rows.into_iter().map(|r| (r.app_id, r.n)).collect());
            avg_width.insert(t.name, width);
        }
        Ok::<_, anyhow::Error>((total_bytes, tables, apps, hot, avg_width))
    };

    // --- DuckDB branch (blocking): cold rows per (table, app_id) ---
    let cold_path_d = cold_path.clone();
    let cold_counts = tokio::task::spawn_blocking(move || -> anyhow::Result<HashMap<&'static str, HashMap<Uuid, i64>>> {
        let eng = DuckEngine::open()?;
        let mut out: HashMap<&'static str, HashMap<Uuid, i64>> = HashMap::new();
        for t in TIERED_TABLES {
            let glob = format!("{}/{}/**/*.parquet", cold_path_d.trim_end_matches('/'), t.name);
            let counts = eng.counts_by_app(&glob)?;
            out.insert(t.name, counts.into_iter().collect());
        }
        Ok(out)
    });

    // --- Filesystem branch (blocking): cold files per (table, app_id) ---
    let cold_path_w = cold_path.clone();
    let walked = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<WalkedFile>> {
        walk_cold(&cold_path_w)
    });

    let (pg_res, cold_res, walk_res) = tokio::join!(pg, cold_counts, walked);
    let (total_bytes, tables, apps, hot, avg_width) = pg_res?;
    let cold_counts = cold_res??;
    let walked = walk_res??;

    // Group walked files by (app_id, table).
    let mut files_by_app: HashMap<Uuid, Vec<ColdFile>> = HashMap::new();
    let mut cold_bytes: HashMap<(Uuid, &'static str), i64> = HashMap::new();
    for f in walked {
        // Match the walked file's table string to a canonical TIERED_TABLES name.
        if let Some(t) = TIERED_TABLES.iter().find(|t| t.name == f.table) {
            *cold_bytes.entry((f.app_id, t.name)).or_insert(0) += f.bytes;
            files_by_app.entry(f.app_id).or_default().push(ColdFile { path: f.path, bytes: f.bytes });
        }
    }

    let existing_ids: std::collections::HashSet<Uuid> = apps.iter().map(|a| a.app_id).collect();

    let mut apps_out: Vec<AppStorage> = apps
        .into_iter()
        .map(|a| {
            let mut per_table = Vec::new();
            let (mut hr, mut cr, mut cb, mut ehb) = (0i64, 0i64, 0i64, 0i64);
            for t in TIERED_TABLES {
                let hot_rows = hot.get(t.name).and_then(|m| m.get(&a.app_id)).copied().unwrap_or(0);
                let cold_rows = cold_counts.get(t.name).and_then(|m| m.get(&a.app_id)).copied().unwrap_or(0);
                let cold_b = cold_bytes.get(&(a.app_id, t.name)).copied().unwrap_or(0);
                let est = avg_width.get(t.name).copied().unwrap_or(0) * hot_rows;
                hr += hot_rows; cr += cold_rows; cb += cold_b; ehb += est;
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
            }
        })
        .collect();

    // Orphaned cold storage: rows/bytes/files whose app_id is no longer in `apps`
    // (the app was deleted after its data tiered). Surface it so the operator sees
    // ALL cold storage rather than silently losing it.
    let mut orphan_tables = Vec::new();
    let (mut o_cold_rows, mut o_cold_bytes) = (0i64, 0i64);
    for t in TIERED_TABLES {
        let cold_rows: i64 = cold_counts
            .get(t.name)
            .map(|m| m.iter().filter(|(id, _)| !existing_ids.contains(id)).map(|(_, n)| *n).sum())
            .unwrap_or(0);
        let cold_b: i64 = cold_bytes
            .iter()
            .filter(|(k, _)| k.1 == t.name && !existing_ids.contains(&k.0))
            .map(|(_, b)| *b)
            .sum();
        o_cold_rows += cold_rows;
        o_cold_bytes += cold_b;
        orphan_tables.push(AppTableStorage {
            name: t.name.to_string(),
            hot_rows: 0,
            cold_rows,
            cold_bytes: cold_b,
            estimated_hot_bytes: 0,
        });
    }
    let mut orphan_files: Vec<ColdFile> = files_by_app.into_values().flatten().collect();
    if o_cold_rows > 0 || !orphan_files.is_empty() {
        orphan_files.sort_by(|a, b| a.path.cmp(&b.path));
        apps_out.push(AppStorage {
            app_id: Uuid::nil(),
            app_name: "(orphaned / deleted apps)".to_string(),
            org_name: "—".to_string(),
            tables: orphan_tables,
            hot_rows_total: 0,
            cold_rows_total: o_cold_rows,
            cold_bytes_total: o_cold_bytes,
            estimated_hot_bytes_total: 0,
            cold_files: orphan_files,
        });
    }

    Ok(StorageReport {
        database: DatabaseInfo { total_bytes, tables },
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
                let rel = path.strip_prefix(base).ok().and_then(|p| p.to_str()).unwrap_or("");
                if let Some(key) = parse_cold_path(rel) {
                    let bytes = entry.metadata().map(|m| m.len() as i64).unwrap_or(0);
                    out.push(WalkedFile { table: key.table, app_id: key.app_id, path: rel.to_string(), bytes });
                }
            }
        }
    }
    Ok(out)
}
