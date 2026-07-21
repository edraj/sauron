//! On-read JS symbolication: resolve a stored error event's minified frames
//! against uploaded source maps, attach source context to the response, and
//! (for hot partitions) persist a lean symbolicated copy back.

use chrono::{Duration, Utc};
use serde_json::Value;
use uuid::Uuid;

use sauron_db::models::ErrorEvent;
use sauron_db::PgPool;
use sauron_redis::SymbolBlobCache;
use sauron_symbols::{ArtifactRef, BlobFetch, RawFrame, Status};

use crate::AppState;

/// A DB + isolated-Redis backed [`BlobFetch`]. Checks out a pooled connection
/// per call so it composes cleanly inside async handlers.
pub struct SqlBlobFetch {
    pool: PgPool,
    app_id: Uuid,
    cache: SymbolBlobCache,
    max_uncompressed: usize,
}

impl BlobFetch for SqlBlobFetch {
    async fn js_artifacts(&self, release: &str) -> Vec<ArtifactRef> {
        let Ok(mut conn) = sauron_db::conn(&self.pool).await else {
            return Vec::new();
        };
        let rows = sauron_db::repo::find_artifacts_for_release(&mut conn, self.app_id, release)
            .await
            .unwrap_or_default();
        rows.into_iter()
            .filter(|a| a.kind == "js_sourcemap")
            .map(|a| ArtifactRef {
                name: a.name,
                blob_sha256: a.blob_sha256,
            })
            .collect()
    }

    async fn dart_symbols(&self, debug_id: &str, _arch: Option<&str>) -> Vec<ArtifactRef> {
        let Ok(mut conn) = sauron_db::conn(&self.pool).await else {
            return Vec::new();
        };
        match sauron_db::repo::find_artifact_by_debug_id(&mut conn, self.app_id, debug_id).await {
            Ok(Some(a)) if a.kind == "dart_symbols" => vec![ArtifactRef {
                name: a.name,
                blob_sha256: a.blob_sha256,
            }],
            _ => Vec::new(),
        }
    }

    async fn blob(&self, sha: &[u8]) -> Option<Vec<u8>> {
        let hex = sauron_symbols::hex(sha);
        let compressed = match self.cache.get(&hex).await {
            Some(c) => c,
            None => {
                let mut conn = sauron_db::conn(&self.pool).await.ok()?;
                let c = sauron_db::repo::get_blob(&mut conn, sha).await.ok()??;
                self.cache.put(&hex, &c).await;
                c
            }
        };
        sauron_symbols::decompress(&compressed, self.max_uncompressed).ok()
    }
}

/// Remove de-obfuscated **source code** (the symbolication context lines) from a
/// response event, leaving symbol names / file / line intact. Applied for callers
/// lacking `source:read`. Does not touch the stored row (persist keeps context).
pub fn strip_source_context(event: &mut ErrorEvent) {
    if let Some(Value::Array(frames)) = event.stacktrace_symbolicated.as_mut() {
        for f in frames.iter_mut() {
            if let Some(obj) = f.as_object_mut() {
                obj.remove("context_line");
                obj.remove("pre_context");
                obj.remove("post_context");
                obj.remove("context_start_line");
            }
        }
    }
}

/// Symbolicate an event in place: sets `stacktrace_symbolicated` (with source
/// context) + `symbolication_status` on the response copy, and persists a copy
/// for hot partitions that hadn't been symbolicated yet.
pub async fn symbolicate_event(state: &AppState, app_id: Uuid, event: &mut ErrorEvent) {
    // Fast path: already fully symbolicated (at ingest or a prior read) and the
    // frames are stored with context — serve them as-is. This keeps issue/event
    // views cheap: no per-event artifact query, no re-parse (crucial for Dart,
    // whose DWARF context is rebuilt per call). Only pending/partial/no_artifacts
    // events do work (the backfill case).
    if event.symbolication_status == "symbolicated"
        && event
            .stacktrace_symbolicated
            .as_ref()
            .is_some_and(|v| v.as_array().is_some_and(|a| !a.is_empty()))
    {
        return;
    }

    let fetch = SqlBlobFetch {
        pool: state.pool.clone(),
        app_id,
        cache: state.symbols.clone(),
        max_uncompressed: state.cfg.symbols_max_uncompressed_mb * 1024 * 1024,
    };

    // Dart AOT trace (in debug_meta.raw_stacktrace) → ELF/DWARF path; otherwise
    // the JS source-map path over the raw frames.
    let (resolved, status) = if let Some(dm) = event.debug_meta.as_ref() {
        match dm.get("raw_stacktrace").and_then(|v| v.as_str()) {
            Some(rt) if !rt.is_empty() => {
                let build_id = dm.get("build_id").and_then(|v| v.as_str());
                let arch = dm.get("arch").and_then(|v| v.as_str());
                state
                    .symbolicator
                    .symbolicate_dart(&fetch, rt, build_id, arch)
                    .await
            }
            _ => return,
        }
    } else {
        let frames: Vec<RawFrame> = match serde_json::from_value(event.stacktrace.clone()) {
            Ok(f) => f,
            Err(_) => return,
        };
        if frames.is_empty() {
            return;
        }
        state
            .symbolicator
            .symbolicate_js(&fetch, event.release.as_deref(), &frames)
            .await
    };

    // Only override the response when we actually resolved something; otherwise
    // keep whatever was stored (e.g. an ingest-time pre-symbolication).
    if !matches!(status, Status::Symbolicated | Status::Partial) {
        return;
    }

    // Persist a lean copy for hot partitions that were previously unresolved —
    // never write into cold/exported partitions (respects the tiering guard).
    let hot = event.occurred_at > Utc::now() - Duration::days(state.cfg.tier_hot_days);
    let was_unresolved = matches!(
        event.symbolication_status.as_str(),
        "pending" | "no_artifacts"
    );
    if hot && was_unresolved {
        // Persist WITH context so later views short-circuit to the stored frames.
        if let (Ok(frames_json), Ok(mut conn)) = (
            serde_json::to_value(&resolved),
            sauron_db::conn(&state.pool).await,
        ) {
            let _ = sauron_db::repo::update_event_symbolication(
                &mut conn,
                event.id,
                event.occurred_at,
                frames_json,
                status.as_str(),
            )
            .await;
        }
    }

    event.stacktrace_symbolicated = serde_json::to_value(&resolved)
        .ok()
        .filter(|v| !v.is_null());
    if event.stacktrace_symbolicated.is_none() {
        event.stacktrace_symbolicated = Some(Value::Array(Vec::new()));
    }
    event.symbolication_status = status.as_str().to_string();
}
