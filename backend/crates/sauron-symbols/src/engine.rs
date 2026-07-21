//! Symbolication orchestration: walk a frame list, match each to an uploaded
//! source map, and resolve it — caching parsed maps in the byte-bounded LRU.
//!
//! Storage-agnostic: artifacts + blob bytes are fetched through the [`BlobFetch`]
//! trait, so the API and the ingest worker supply their own DB/Redis-backed
//! implementations while this crate stays pure and testable.

use std::future::Future;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::cache::ByteLru;
use crate::js::ParsedSourceMap;
use crate::matcher;

/// A raw (minified) stack frame — mirrors `sauron_core::envelope::Frame`.
/// `Deserialize` tolerates extra fields (e.g. `module`) from the stored JSON.
#[derive(Debug, Clone, Deserialize)]
pub struct RawFrame {
    pub function: Option<String>,
    pub filename: Option<String>,
    pub abs_path: Option<String>,
    pub lineno: Option<u32>,
    pub colno: Option<u32>,
    pub in_app: Option<bool>,
}

/// A frame after symbolication. Serializes into the shape the dashboard renders.
#[derive(Debug, Clone, Serialize)]
pub struct ResolvedFrame {
    pub function: Option<String>,
    pub filename: Option<String>,
    pub lineno: Option<u32>,
    pub colno: Option<u32>,
    pub in_app: Option<bool>,
    pub symbolicated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_line: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub pre_context: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub post_context: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_start_line: Option<u32>,
}

impl ResolvedFrame {
    fn passthrough(f: &RawFrame) -> ResolvedFrame {
        ResolvedFrame {
            function: f.function.clone(),
            filename: f.filename.clone(),
            lineno: f.lineno,
            colno: f.colno,
            in_app: f.in_app,
            symbolicated: false,
            context_line: None,
            pre_context: Vec::new(),
            post_context: Vec::new(),
            context_start_line: None,
        }
    }

    /// A copy with source-context stripped — persisted lean; context is only
    /// carried in the API response.
    pub fn without_context(&self) -> ResolvedFrame {
        ResolvedFrame {
            context_line: None,
            pre_context: Vec::new(),
            post_context: Vec::new(),
            context_start_line: None,
            ..self.clone()
        }
    }
}

/// An artifact candidate for matching (subset of `symbol_artifacts`).
#[derive(Debug, Clone)]
pub struct ArtifactRef {
    pub name: Option<String>,
    pub blob_sha256: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Symbolicated,
    Partial,
    NoArtifacts,
    NotApplicable,
}

impl Status {
    pub fn as_str(&self) -> &'static str {
        match self {
            Status::Symbolicated => "symbolicated",
            Status::Partial => "partial",
            Status::NoArtifacts => "no_artifacts",
            Status::NotApplicable => "not_applicable",
        }
    }
}

/// Fetches artifacts + blob bytes for symbolication. `blob` returns the
/// **decompressed** artifact bytes (source map or Dart ELF).
pub trait BlobFetch {
    fn js_artifacts(&self, release: &str) -> impl Future<Output = Vec<ArtifactRef>> + Send;
    fn dart_symbols(
        &self,
        debug_id: &str,
        arch: Option<&str>,
    ) -> impl Future<Output = Vec<ArtifactRef>> + Send;
    fn blob(&self, sha: &[u8]) -> impl Future<Output = Option<Vec<u8>>> + Send;
}

/// Symbolication engine holding the in-process parsed-map cache.
pub struct Symbolicator {
    cache: ByteLru<Vec<u8>, ParsedSourceMap>,
    context_radius: usize,
}

impl Symbolicator {
    pub fn new(budget_bytes: usize) -> Self {
        Symbolicator {
            cache: ByteLru::new(budget_bytes),
            context_radius: 5,
        }
    }

    /// Resolve every frame against the release's JS source maps.
    pub async fn symbolicate_js<F: BlobFetch + Sync>(
        &self,
        fetch: &F,
        release: Option<&str>,
        frames: &[RawFrame],
    ) -> (Vec<ResolvedFrame>, Status) {
        let release = match release {
            Some(r) if !r.is_empty() && !frames.is_empty() => r,
            _ => {
                return (
                    frames.iter().map(ResolvedFrame::passthrough).collect(),
                    Status::NotApplicable,
                )
            }
        };

        let artifacts = fetch.js_artifacts(release).await;
        if artifacts.is_empty() {
            return (
                frames.iter().map(ResolvedFrame::passthrough).collect(),
                Status::NoArtifacts,
            );
        }

        let mut out = Vec::with_capacity(frames.len());
        let mut any_resolved = false;
        let mut any_unresolved = false;
        for frame in frames {
            match self.try_resolve(fetch, &artifacts, frame).await {
                Some(rf) => {
                    any_resolved = true;
                    out.push(rf);
                }
                None => {
                    any_unresolved = true;
                    out.push(ResolvedFrame::passthrough(frame));
                }
            }
        }

        let status = if any_resolved && !any_unresolved {
            Status::Symbolicated
        } else if any_resolved {
            Status::Partial
        } else {
            Status::NoArtifacts
        };
        (out, status)
    }

    /// Resolve a verbatim Dart (Flutter AOT) stack trace against uploaded
    /// `--split-debug-info` ELF symbols. `debug_id`/`arch` come from the SDK's
    /// `debug_meta`; `debug_id` falls back to the trace's own `build_id`.
    pub async fn symbolicate_dart<F: BlobFetch + Sync>(
        &self,
        fetch: &F,
        raw_trace: &str,
        debug_id: Option<&str>,
        arch: Option<&str>,
    ) -> (Vec<ResolvedFrame>, Status) {
        let trace = crate::dart_trace::parse(raw_trace);
        if trace.frames.is_empty() {
            return (Vec::new(), Status::NotApplicable);
        }

        let did = debug_id
            .map(str::to_string)
            .or_else(|| trace.build_id.clone());
        let Some(did) = did else {
            return (dart_passthrough(&trace), Status::NoArtifacts);
        };

        let artifacts = fetch.dart_symbols(&did, arch).await;
        let Some(art) = artifacts.into_iter().next() else {
            return (dart_passthrough(&trace), Status::NoArtifacts);
        };
        let Some(elf) = fetch.blob(&art.blob_sha256).await else {
            return (dart_passthrough(&trace), Status::NoArtifacts);
        };

        let addrs: Vec<u64> = trace
            .frames
            .iter()
            .map(|f| f.lookup_addr(trace.dso_base).unwrap_or(0))
            .collect();
        let resolved = match crate::dart::resolve(&elf, &addrs) {
            Ok(r) => r,
            Err(_) => return (dart_passthrough(&trace), Status::NoArtifacts),
        };

        let mut out = Vec::with_capacity(trace.frames.len());
        let mut any_resolved = false;
        let mut any_unresolved = false;
        for (frame, locs) in trace.frames.iter().zip(resolved.iter()) {
            if locs.is_empty() {
                any_unresolved = true;
                out.push(dart_unresolved(frame));
            } else {
                any_resolved = true;
                // Expand the inline chain (innermost first) into one logical
                // frame each, so inlined functions aren't hidden.
                for loc in locs {
                    out.push(dart_resolved(loc));
                }
            }
        }
        // Store crash-last (matches the JS wire convention; the view reverses).
        out.reverse();

        let status = if any_resolved && !any_unresolved {
            Status::Symbolicated
        } else if any_resolved {
            Status::Partial
        } else {
            Status::NoArtifacts
        };
        (out, status)
    }

    async fn try_resolve<F: BlobFetch + Sync>(
        &self,
        fetch: &F,
        artifacts: &[ArtifactRef],
        frame: &RawFrame,
    ) -> Option<ResolvedFrame> {
        let path = frame.filename.as_deref().or(frame.abs_path.as_deref())?;
        let lineno = frame.lineno?;
        let colno = frame.colno?;

        // Prefer an exact path match; fall back to a same-basename match. With
        // artifacts ordered newest-first, this makes duplicate names deterministic.
        let art = artifacts
            .iter()
            .find(|a| {
                a.name
                    .as_deref()
                    .is_some_and(|n| matcher::matches_exact(path, n))
            })
            .or_else(|| {
                artifacts
                    .iter()
                    .find(|a| a.name.as_deref().is_some_and(|n| matcher::matches(path, n)))
            })?;

        let sha = art.blob_sha256.clone();
        let map = self.load_map(fetch, sha).await;
        let loc = map.resolve(lineno, colno)?;

        let ctx = map.context(loc.source_index, loc.line, self.context_radius);
        let (context_line, pre_context, post_context, context_start_line) = match ctx {
            Some(c) => (Some(c.line), c.pre, c.post, Some(c.start_line)),
            None => (None, Vec::new(), Vec::new(), None),
        };

        Some(ResolvedFrame {
            function: loc.name.or_else(|| frame.function.clone()),
            filename: Some(loc.source),
            lineno: Some(loc.line),
            colno: Some(loc.column),
            in_app: frame.in_app,
            symbolicated: true,
            context_line,
            pre_context,
            post_context,
            context_start_line,
        })
    }

    /// Get the parsed map for a blob, building (fetch + parse) once per key.
    async fn load_map<F: BlobFetch + Sync>(&self, fetch: &F, sha: Vec<u8>) -> Arc<ParsedSourceMap> {
        let fetch_sha = sha.clone();
        self.cache
            .get_or_insert(
                sha,
                |m| m.weight().max(1),
                || async move {
                    match fetch.blob(&fetch_sha).await {
                        Some(bytes) => ParsedSourceMap::parse(&bytes).unwrap_or_else(|e| {
                            tracing::warn!(error = %e, "source map parse failed; caching empty");
                            ParsedSourceMap::empty()
                        }),
                        None => {
                            tracing::debug!("source map blob missing; caching empty");
                            ParsedSourceMap::empty()
                        }
                    }
                },
            )
            .await
    }
}

/// Dart frames when no symbols could be applied: keep the address so the trace
/// is still legible, marked unsymbolicated. Stored crash-last (view reverses).
fn dart_passthrough(trace: &crate::dart_trace::DartTrace) -> Vec<ResolvedFrame> {
    let mut out: Vec<ResolvedFrame> = trace.frames.iter().map(dart_unresolved).collect();
    out.reverse();
    out
}

/// One resolved (possibly inlined) Dart frame. Dart has no `sourcesContent`, so
/// there are no source-context lines — just function/file/line.
fn dart_resolved(loc: &crate::js::ResolvedLoc) -> ResolvedFrame {
    ResolvedFrame {
        function: loc.name.clone(),
        filename: Some(loc.source.clone()),
        lineno: (loc.line > 0).then_some(loc.line),
        colno: (loc.column > 0).then_some(loc.column),
        in_app: None,
        symbolicated: true,
        context_line: None,
        pre_context: Vec::new(),
        post_context: Vec::new(),
        context_start_line: None,
    }
}

fn dart_unresolved(frame: &crate::dart_trace::DartFrameRef) -> ResolvedFrame {
    let addr = frame.virt.or(frame.abs);
    ResolvedFrame {
        function: None,
        filename: addr.map(|a| format!("<dart> +0x{a:x}")),
        lineno: None,
        colno: None,
        in_app: None,
        symbolicated: false,
        context_line: None,
        pre_context: Vec::new(),
        post_context: Vec::new(),
        context_start_line: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content;

    struct Mem {
        name: String,
        raw: Vec<u8>,
    }
    impl BlobFetch for Mem {
        async fn js_artifacts(&self, _r: &str) -> Vec<ArtifactRef> {
            vec![ArtifactRef {
                name: Some(self.name.clone()),
                blob_sha256: content::sha256(&self.raw).to_vec(),
            }]
        }
        async fn dart_symbols(&self, _id: &str, _arch: Option<&str>) -> Vec<ArtifactRef> {
            Vec::new()
        }
        async fn blob(&self, _sha: &[u8]) -> Option<Vec<u8>> {
            Some(self.raw.clone())
        }
    }

    // Fetch that serves an ELF as the Dart symbols artifact.
    struct DartMem {
        elf: Vec<u8>,
    }
    impl BlobFetch for DartMem {
        async fn js_artifacts(&self, _r: &str) -> Vec<ArtifactRef> {
            Vec::new()
        }
        async fn dart_symbols(&self, _id: &str, _arch: Option<&str>) -> Vec<ArtifactRef> {
            vec![ArtifactRef {
                name: None,
                blob_sha256: content::sha256(&self.elf).to_vec(),
            }]
        }
        async fn blob(&self, _sha: &[u8]) -> Option<Vec<u8>> {
            Some(self.elf.clone())
        }
    }

    fn frame(url: &str, line: u32, col: u32) -> RawFrame {
        RawFrame {
            function: None,
            filename: Some(url.to_string()),
            abs_path: Some(url.to_string()),
            lineno: Some(line),
            colno: Some(col),
            in_app: Some(true),
        }
    }

    #[tokio::test]
    async fn symbolicates_a_matching_frame() {
        let raw = br#"{"version":3,"sources":["foo.ts"],"names":["greet"],"mappings":"AAAAA","sourcesContent":["export function greet(){ return 1 }"]}"#.to_vec();
        let fetch = Mem {
            name: "~/static/app.min.js".into(),
            raw,
        };
        let s = Symbolicator::new(4 << 20);
        let frames = vec![frame("https://x.io/static/app.min.js", 1, 1)];
        let (out, status) = s.symbolicate_js(&fetch, Some("web@1"), &frames).await;
        assert_eq!(out[0].filename.as_deref(), Some("foo.ts"));
        assert_eq!(out[0].lineno, Some(1));
        assert_eq!(out[0].function.as_deref(), Some("greet"));
        assert!(out[0].symbolicated);
        assert_eq!(
            out[0].context_line.as_deref(),
            Some("export function greet(){ return 1 }")
        );
        assert_eq!(status, Status::Symbolicated);
    }

    #[tokio::test]
    async fn unmatched_frame_is_partial_or_no_artifacts() {
        let raw = br#"{"version":3,"sources":["foo.ts"],"names":[],"mappings":"AAAA","sourcesContent":["x"]}"#.to_vec();
        let fetch = Mem {
            name: "~/static/app.min.js".into(),
            raw,
        };
        let s = Symbolicator::new(1 << 20);
        // frame path doesn't match the artifact name
        let frames = vec![frame("https://x.io/other/vendor.js", 1, 1)];
        let (out, status) = s.symbolicate_js(&fetch, Some("web@1"), &frames).await;
        assert!(!out[0].symbolicated);
        assert_eq!(status, Status::NoArtifacts);
    }

    #[tokio::test]
    async fn no_release_is_not_applicable() {
        let s = Symbolicator::new(1 << 20);
        let fetch = Mem {
            name: "n".into(),
            raw: vec![],
        };
        let (_out, status) = s.symbolicate_js(&fetch, None, &[frame("a", 1, 1)]).await;
        assert_eq!(status, Status::NotApplicable);
    }

    #[tokio::test]
    async fn symbolicates_dart_against_elf() {
        // virt = compute_total's vaddr in the (-no-pie) fixture ELF.
        let trace = "\
*** *** ***\n\
build_id: 'deadbeef'\n\
isolate_dso_base: 0\n\
    #00 abs 0000000000400446 virt 0000000000400446 _kDartIsolateSnapshotInstructions+0x446\n";
        let fetch = DartMem {
            elf: include_bytes!("../tests/fixtures/sample.elf").to_vec(),
        };
        let s = Symbolicator::new(4 << 20);
        let (out, status) = s
            .symbolicate_dart(&fetch, trace, Some("deadbeef"), Some("arm64"))
            .await;
        assert_eq!(status, Status::Symbolicated);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].function.as_deref(), Some("compute_total"));
        assert!(out[0].filename.as_deref().unwrap().ends_with("sample.c"));
        assert!(out[0].symbolicated);
    }

    #[tokio::test]
    async fn dart_expands_inline_frames() {
        // virt 0x400460 is inside scale() inlined into outer() (see dart.rs).
        let trace = "\
build_id: 'inl'\n\
isolate_dso_base: 0\n\
    #00 abs 0000000000400460 virt 0000000000400460 sym+0x0\n";
        let fetch = DartMem {
            elf: include_bytes!("../tests/fixtures/sample_inline.elf").to_vec(),
        };
        let s = Symbolicator::new(4 << 20);
        let (out, status) = s.symbolicate_dart(&fetch, trace, Some("inl"), None).await;
        assert_eq!(status, Status::Symbolicated);
        assert_eq!(
            out.len(),
            2,
            "one physical frame should expand to 2 inlined"
        );
        let names: std::collections::HashSet<_> =
            out.iter().filter_map(|f| f.function.as_deref()).collect();
        assert!(names.contains("scale"));
        assert!(names.contains("outer"));
        assert!(out.iter().all(|f| f.symbolicated));
    }

    #[tokio::test]
    async fn dart_no_symbols_is_no_artifacts() {
        let trace = "build_id: 'x'\nisolate_dso_base: 0\n    #00 abs 100 virt 100 sym\n";
        let fetch = Mem {
            name: "n".into(),
            raw: vec![],
        };
        let s = Symbolicator::new(1 << 20);
        let (out, status) = s.symbolicate_dart(&fetch, trace, Some("x"), None).await;
        assert_eq!(status, Status::NoArtifacts);
        assert!(!out[0].symbolicated);
    }
}
