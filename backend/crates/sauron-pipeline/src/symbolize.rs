//! Ingest-time (hybrid write-path) JS symbolication.
//!
//! When a map is already uploaded for the event's release, pre-symbolicate the
//! frames so the stored row (and any later Parquet export) carries readable
//! frames. Strictly time-boxed and non-fatal: on timeout or miss the event is
//! stored raw with a `pending`/`no_artifacts` status and the API's on-read path
//! resolves it later.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use uuid::Uuid;

use sauron_core::envelope::Frame;
use sauron_db::PgPool;
use sauron_redis::SymbolBlobCache;
use sauron_symbols::{ArtifactRef, BlobFetch, RawFrame, Status, Symbolicator};

/// How long an app's "has symbols?" answer is trusted before re-checking. Bounds
/// the delay before ingest starts pre-symbolicating after a first upload (the
/// API's on-read path symbolicates in the meantime).
const PRESENCE_TTL: Duration = Duration::from_secs(60);

/// Shared symbolication resources threaded through the workers.
#[derive(Clone)]
pub struct SymbolizeCtx {
    pub symbolicator: Arc<Symbolicator>,
    pub cache: SymbolBlobCache,
    pub timeout_ms: u64,
    pub max_uncompressed: usize,
    /// `app_id -> (has_symbol_artifacts, checked_at)`. Skips a per-error artifact
    /// query at ingest for apps that never upload symbols.
    presence: Arc<Mutex<HashMap<Uuid, (bool, Instant)>>>,
}

impl SymbolizeCtx {
    pub fn new(
        symbolicator: Arc<Symbolicator>,
        cache: SymbolBlobCache,
        timeout_ms: u64,
        max_uncompressed: usize,
    ) -> Self {
        SymbolizeCtx {
            symbolicator,
            cache,
            timeout_ms,
            max_uncompressed,
            presence: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Whether the app has any symbol artifacts, cached with a short TTL.
    /// Fails **open** (returns true) on a DB error so symbolication is never
    /// suppressed by a transient failure.
    async fn app_has_artifacts(&self, pool: &PgPool, app_id: Uuid) -> bool {
        let cached = self.presence.lock().unwrap().get(&app_id).copied();
        if let Some((has, at)) = cached {
            if at.elapsed() < PRESENCE_TTL {
                return has;
            }
        }
        let has = match sauron_db::conn(pool).await {
            Ok(mut conn) => sauron_db::repo::app_has_symbol_artifacts(&mut conn, app_id)
                .await
                .unwrap_or(true),
            Err(_) => true,
        };
        self.presence
            .lock()
            .unwrap()
            .insert(app_id, (has, Instant::now()));
        has
    }
}

struct PoolBlobFetch {
    pool: PgPool,
    app_id: Uuid,
    cache: SymbolBlobCache,
    max_uncompressed: usize,
}

impl PoolBlobFetch {
    /// One kind of Dart artifact for one build. Kind-scoped because
    /// `symbol_artifacts` is unique on (app, kind, debug_id): the ELF and the
    /// obfuscation map for a build share an id and are told apart by kind.
    async fn dart_artifact(&self, kind: &str, debug_id: &str) -> Vec<ArtifactRef> {
        let Ok(mut conn) = sauron_db::conn(&self.pool).await else {
            return Vec::new();
        };
        match sauron_db::repo::find_artifact_by_debug_id(&mut conn, self.app_id, kind, debug_id)
            .await
        {
            Ok(Some(a)) => vec![ArtifactRef {
                name: a.name,
                blob_sha256: a.blob_sha256,
            }],
            _ => Vec::new(),
        }
    }
}

impl BlobFetch for PoolBlobFetch {
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
        // debug_id is unique per arch, so it alone identifies the ELF.
        self.dart_artifact("dart_symbols", debug_id).await
    }

    async fn dart_obfuscation_map(&self, debug_id: &str) -> Vec<ArtifactRef> {
        self.dart_artifact("dart_obfuscation_map", debug_id).await
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

fn to_raw(frames: &[Frame]) -> Vec<RawFrame> {
    frames
        .iter()
        .map(|f| RawFrame {
            function: f.function.clone(),
            filename: f.filename.clone(),
            abs_path: f.abs_path.clone(),
            lineno: f.lineno,
            colno: f.colno,
            in_app: f.in_app,
        })
        .collect()
}

/// Build the `debug_meta` JSON stored on a Dart error event so the API's on-read
/// path can re-symbolicate: the SDK-reported header plus the verbatim trace.
pub fn build_debug_meta(dm: Option<&sauron_core::DebugMeta>, raw_stacktrace: &str) -> Value {
    serde_json::json!({
        "build_id": dm.and_then(|d| d.build_id.clone()),
        "isolate_dso_base": dm.and_then(|d| d.isolate_dso_base.clone()),
        "arch": dm.and_then(|d| d.arch.clone()),
        "os": dm.and_then(|d| d.os.clone()),
        "raw_stacktrace": raw_stacktrace,
    })
}

/// What one time-boxed pre-symbolication attempt produced.
///
/// `culprit` rides along with the frames rather than being re-derived by the
/// caller because this is the only place the TYPED `ResolvedFrame`s exist —
/// past here they are an opaque `serde_json::Value`, and rebuilding them from
/// it to read one frame would be a parse per error event on the hot path.
pub struct Symbolicated {
    /// Frames WITH source context (see the `Store frames WITH source context`
    /// comment at the write); `None` unless something resolved.
    pub frames: Option<Value>,
    /// pending | symbolicated | partial | no_artifacts | not_applicable | failed.
    pub status: String,
    /// The de-obfuscated culprit, or `None` when nothing resolved — in which
    /// case the caller KEEPS the raw one rather than blanking it. A miss must
    /// never cost the reader the minified culprit they had before.
    pub culprit: Option<String>,
}

impl Symbolicated {
    /// A miss: no frames, no culprit, just the status that explains why.
    fn unresolved(status: &str) -> Self {
        Self {
            frames: None,
            status: status.to_string(),
            culprit: None,
        }
    }
}

/// The original class name for an obfuscated Dart exception type, or `None`.
///
/// Separate from [`symbolicate_ingest_dart`] because it answers a different
/// question from a different artifact: that one resolves ADDRESSES against
/// DWARF, this one resolves a NAME against the map Dart emits with
/// `--save-obfuscation-map`. An app can upload either without the other, and a
/// build with symbols but no map still symbolicates its frames.
///
/// Time-boxed and non-fatal on the same terms as everything else on this path —
/// a miss returns `None` and the caller keeps the raw type, which the on-read
/// path can still repair once a map arrives.
///
/// **Presentational.** The result must reach only the DISPLAY fields
/// (`issues.type`/`title`, `error_events.title`). The fingerprint and
/// `error_events.exception_type` stay raw, so uploading a map cannot re-group
/// issues that already exist — the same rule the frame symbolication follows.
pub async fn deobfuscate_ingest_type(
    pool: &PgPool,
    sym: &SymbolizeCtx,
    app_id: Uuid,
    debug_id: Option<&str>,
    ty: &str,
) -> Option<String> {
    if ty.is_empty() || debug_id.is_none() {
        return None;
    }
    if !sym.app_has_artifacts(pool, app_id).await {
        return None;
    }
    let fetch = PoolBlobFetch {
        pool: pool.clone(),
        app_id,
        cache: sym.cache.clone(),
        max_uncompressed: sym.max_uncompressed,
    };
    let fut = sym.symbolicator.deobfuscate_type(&fetch, debug_id, ty);
    tokio::time::timeout(std::time::Duration::from_millis(sym.timeout_ms), fut)
        .await
        .ok()
        .flatten()
}

/// Time-boxed Dart pre-symbolication. See [`Symbolicated`] for the result.
pub async fn symbolicate_ingest_dart(
    pool: &PgPool,
    sym: &SymbolizeCtx,
    app_id: Uuid,
    raw_trace: &str,
    dm: Option<&sauron_core::DebugMeta>,
) -> Symbolicated {
    // Apps with no uploaded symbols: skip the artifact lookup entirely.
    if !sym.app_has_artifacts(pool, app_id).await {
        return Symbolicated::unresolved("no_artifacts");
    }
    let fetch = PoolBlobFetch {
        pool: pool.clone(),
        app_id,
        cache: sym.cache.clone(),
        max_uncompressed: sym.max_uncompressed,
    };
    let debug_id = dm.and_then(|d| d.build_id.as_deref());
    let arch = dm.and_then(|d| d.arch.as_deref());
    let fut = sym
        .symbolicator
        .symbolicate_dart(&fetch, raw_trace, debug_id, arch);
    match tokio::time::timeout(std::time::Duration::from_millis(sym.timeout_ms), fut).await {
        Ok((resolved, status)) => match status {
            Status::Symbolicated | Status::Partial => Symbolicated {
                frames: serde_json::to_value(&resolved).ok(),
                status: status.as_str().to_string(),
                culprit: sauron_symbols::culprit_of_resolved(&resolved),
            },
            other => Symbolicated::unresolved(other.as_str()),
        },
        Err(_) => Symbolicated::unresolved("pending"),
    }
}

/// Time-boxed pre-symbolication. See [`Symbolicated`] for the result. Never
/// returns an error.
pub async fn symbolicate_ingest(
    pool: &PgPool,
    sym: &SymbolizeCtx,
    app_id: Uuid,
    release: Option<&str>,
    frames: &[Frame],
) -> Symbolicated {
    if frames.is_empty() {
        return Symbolicated::unresolved("not_applicable");
    }
    // No release → nothing to match (symbolicate_js would return NotApplicable
    // without a query anyway). A release with no app symbols wastes a query, so
    // gate that on the presence cache.
    if release.is_some() && !sym.app_has_artifacts(pool, app_id).await {
        return Symbolicated::unresolved("no_artifacts");
    }
    let fetch = PoolBlobFetch {
        pool: pool.clone(),
        app_id,
        cache: sym.cache.clone(),
        max_uncompressed: sym.max_uncompressed,
    };
    let raw = to_raw(frames);
    let fut = sym.symbolicator.symbolicate_js(&fetch, release, &raw);
    match tokio::time::timeout(std::time::Duration::from_millis(sym.timeout_ms), fut).await {
        Ok((resolved, status)) => match status {
            // Store frames WITH source context so the API can serve them
            // straight from the row without re-symbolicating on every view.
            Status::Symbolicated | Status::Partial => Symbolicated {
                frames: serde_json::to_value(&resolved).ok(),
                status: status.as_str().to_string(),
                culprit: sauron_symbols::culprit_of_resolved(&resolved),
            },
            other => Symbolicated::unresolved(other.as_str()),
        },
        // Timed out — leave it pending for the on-read path.
        Err(_) => Symbolicated::unresolved("pending"),
    }
}
