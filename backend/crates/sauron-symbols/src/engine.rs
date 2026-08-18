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
use crate::obfuscation::ObfuscationMap;

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
///
/// `Deserialize` as well as `Serialize` because this shape is not only sent —
/// it is STORED, in `error_events.stacktrace_symbolicated`, and read back to
/// re-derive the culprit for rows symbolicated before that derivation existed.
/// Container-level `default` so a stored row written by an older shape (or one
/// whose `skip_serializing_if` fields were simply absent) reads back rather
/// than failing the whole frame list.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
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

    // `without_context()` used to live here and has been DELETED. It had zero
    // callers, and its doc — "persisted lean; context is only carried in the API
    // response" — asserted the opposite of what the code does. Both persistence
    // paths store source context: `sauron-pipeline/src/symbolize.rs` ("Store
    // frames WITH source context") and `sauron-api/src/symbolicate.rs` ("Persist
    // WITH context"). A reader who trusted that sentence would conclude the
    // stored column holds no customer source, which is a large part of why the
    // `source:read` hole on four handlers looked unreachable.
    //
    // Stripping happens on the RESPONSE, in `symbolicate::strip_source_context`.
    // If lean persistence is ever wanted, write it deliberately with a caller.
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
///
/// `release` and `debug_id` arrive **canonical** — [`crate::normalize_release`]
/// and [`crate::normalize_debug_id`] are applied by the caller (see
/// [`Symbolicator::symbolicate_js`] / [`Symbolicator::symbolicate_dart`]), which
/// is the same normalization the upload path applies before storing the column.
/// Implementations compare them with plain equality and must not re-derive,
/// re-case or otherwise "fix" them; anything that would need fixing here is a
/// missing rule in `normalize`, where both sides read it from.
pub trait BlobFetch {
    fn js_artifacts(&self, release: &str) -> impl Future<Output = Vec<ArtifactRef>> + Send;
    fn dart_symbols(
        &self,
        debug_id: &str,
        arch: Option<&str>,
    ) -> impl Future<Output = Vec<ArtifactRef>> + Send;
    /// The `dart_obfuscation_map` for this build, if one was uploaded.
    ///
    /// Keyed on the SAME `debug_id` as [`Self::dart_symbols`] — the map is JSON
    /// with nothing identifying inside it, so the build id of the symbols it
    /// was emitted beside is the only thing that ties the two together. That is
    /// why `symbol_artifacts` is unique on (app, **kind**, debug_id) rather
    /// than (app, debug_id).
    fn dart_obfuscation_map(&self, debug_id: &str)
        -> impl Future<Output = Vec<ArtifactRef>> + Send;
    fn blob(&self, sha: &[u8]) -> impl Future<Output = Option<Vec<u8>>> + Send;
}

/// Symbolication engine holding the in-process parsed-map cache.
pub struct Symbolicator {
    cache: ByteLru<Vec<u8>, ParsedSourceMap>,
    /// Parsed Dart obfuscation maps, held separately from `cache` so a build
    /// with a large source map cannot evict the small name index that every
    /// error from that build needs. Given its own share of the budget for the
    /// same reason.
    obfuscation: ByteLru<Vec<u8>, ObfuscationMap>,
    context_radius: usize,
}

impl Symbolicator {
    pub fn new(budget_bytes: usize) -> Self {
        Symbolicator {
            cache: ByteLru::new(budget_bytes),
            // An eighth of the budget. A name index is a few hundred KB against
            // source maps measured in tens of MB, so this is generous in
            // practice while still bounded.
            obfuscation: ByteLru::new((budget_bytes / 8).max(1)),
            context_radius: 5,
        }
    }

    /// The original class name for an obfuscated Dart type, or `None`.
    ///
    /// `None` covers every "leave it alone" case and they must stay
    /// indistinguishable to the caller: no debug id on the event, no map
    /// uploaded for that build, a map that does not contain this name, or a
    /// name that was never obfuscated. In all of them the value already on the
    /// row is the best one available.
    ///
    /// **Presentational only.** The caller must not feed the result into a
    /// fingerprint: grouping runs on the raw wire values, so that uploading a
    /// map later cannot re-group existing issues.
    pub async fn deobfuscate_type<F: BlobFetch + Sync>(
        &self,
        fetch: &F,
        debug_id: Option<&str>,
        ty: &str,
    ) -> Option<String> {
        if ty.is_empty() {
            return None;
        }
        let debug_id = crate::normalize_debug_id(debug_id?);
        let art = fetch
            .dart_obfuscation_map(&debug_id)
            .await
            .into_iter()
            .next()?;
        let map = self.load_obfuscation(fetch, art.blob_sha256).await;
        map.original_path(ty)
    }

    async fn load_obfuscation<F: BlobFetch + Sync>(
        &self,
        fetch: &F,
        sha: Vec<u8>,
    ) -> Arc<ObfuscationMap> {
        let fetch_sha = sha.clone();
        self.obfuscation
            .get_or_insert(
                sha,
                |m| m.weight().max(1),
                || async move {
                    match fetch.blob(&fetch_sha).await {
                        Some(bytes) => ObfuscationMap::parse(&bytes).unwrap_or_else(|e| {
                            tracing::warn!(error = %e, "obfuscation map parse failed; caching empty");
                            ObfuscationMap::default()
                        }),
                        None => {
                            tracing::debug!("obfuscation map blob missing; caching empty");
                            ObfuscationMap::default()
                        }
                    }
                },
            )
            .await
    }

    /// Resolve every frame against the release's JS source maps.
    pub async fn symbolicate_js<F: BlobFetch + Sync>(
        &self,
        fetch: &F,
        release: Option<&str>,
        frames: &[RawFrame],
    ) -> (Vec<ResolvedFrame>, Status) {
        // Trimmed before it is matched, for the reason spelled out in
        // `crate::normalize`: the upload path trims what it stores, this is
        // compared to it with plain SQL equality, and an SDK that reads its
        // release out of a file or an env var at init carries the newline along
        // — `"1.0.0\n"` would silently match nothing at all.
        let release = release.map(crate::normalize_release);
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

        // Canonicalized before the lookup, and on BOTH candidate sources: the
        // SDK's `debug_meta.build_id` is untrusted wire input, and the trace's own
        // `build_id:` header is whatever the VM printed. The upload path stores
        // the lowercased id, so an uppercase report here matched nothing —
        // silently, as `no_artifacts`. See `crate::normalize`.
        //
        // A blank value counts as absent, exactly as it does on the write side
        // (`blank_to_none`): `debug_meta: {"build_id": " "}` now falls through to
        // the trace's own header instead of spending a query on a key that
        // cannot match a stored one (the column is NULL, never empty).
        let did = debug_id
            .map(crate::normalize_debug_id)
            .filter(|d| !d.is_empty())
            .or_else(|| trace.build_id.as_deref().map(crate::normalize_debug_id))
            .filter(|d| !d.is_empty());
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

        // One slot per frame, `None` where the frame's address cannot be
        // determined — never a stand-in address. `dart::resolve` keeps the slot
        // and returns it empty, so the positional pairing below still lines frame
        // i up with frame i's own result and the frame falls through to
        // `dart_unresolved` at its original position, still showing its raw `abs`
        // when the trace gave one.
        let addrs: Vec<Option<u64>> = trace
            .frames
            .iter()
            .map(|f| f.lookup_addr(trace.dso_base))
            .collect();
        let resolved = match crate::dart::resolve(&elf, &addrs) {
            Ok(r) => r,
            Err(_) => return (dart_passthrough(&trace), Status::NoArtifacts),
        };
        // The pairing below is by position, and `zip` would silently truncate
        // (dropping trailing frames) if the two lengths ever disagreed.
        debug_assert_eq!(
            resolved.len(),
            trace.frames.len(),
            "dart::resolve must return one slot per frame"
        );

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
        async fn dart_obfuscation_map(&self, _id: &str) -> Vec<ArtifactRef> {
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
        async fn dart_obfuscation_map(&self, _id: &str) -> Vec<ArtifactRef> {
            Vec::new()
        }
        async fn blob(&self, _sha: &[u8]) -> Option<Vec<u8>> {
            Some(self.elf.clone())
        }
    }

    /// Serves an obfuscation map — and, like [`Strict`], only for the exact
    /// stored id, because that is what the real fetchers do.
    struct MapMem {
        stored_debug_id: &'static str,
        json: Vec<u8>,
    }
    impl BlobFetch for MapMem {
        async fn js_artifacts(&self, _r: &str) -> Vec<ArtifactRef> {
            Vec::new()
        }
        async fn dart_symbols(&self, _id: &str, _arch: Option<&str>) -> Vec<ArtifactRef> {
            Vec::new()
        }
        async fn dart_obfuscation_map(&self, debug_id: &str) -> Vec<ArtifactRef> {
            if debug_id != self.stored_debug_id {
                return Vec::new();
            }
            vec![ArtifactRef {
                name: None,
                blob_sha256: content::sha256(&self.json).to_vec(),
            }]
        }
        async fn blob(&self, _sha: &[u8]) -> Option<Vec<u8>> {
            Some(self.json.clone())
        }
    }

    /// Answers **only** to the canonical key, and records what it was asked for.
    ///
    /// Standing in for the real fetchers, which hand the key to a plain SQL
    /// equality against a column the upload path stored normalized: if the
    /// engine passes a non-canonical spelling through, the row is simply not
    /// found. A permissive mock (like `DartMem`, which ignores the id entirely)
    /// cannot see that class of bug at all, which is how a write-only
    /// normalization got shipped in the first place.
    struct Strict {
        /// The stored, canonical `symbol_artifacts.debug_id`.
        stored_debug_id: &'static str,
        /// The stored, canonical `symbol_artifacts.release`.
        stored_release: &'static str,
        blob: Vec<u8>,
    }
    impl Strict {
        fn artifact(&self, name: Option<&str>) -> Vec<ArtifactRef> {
            vec![ArtifactRef {
                name: name.map(str::to_string),
                blob_sha256: content::sha256(&self.blob).to_vec(),
            }]
        }
    }
    impl BlobFetch for Strict {
        async fn js_artifacts(&self, release: &str) -> Vec<ArtifactRef> {
            if release != self.stored_release {
                return Vec::new();
            }
            self.artifact(Some("~/static/app.min.js"))
        }
        async fn dart_symbols(&self, debug_id: &str, _arch: Option<&str>) -> Vec<ArtifactRef> {
            if debug_id != self.stored_debug_id {
                return Vec::new();
            }
            self.artifact(None)
        }
        async fn dart_obfuscation_map(&self, _id: &str) -> Vec<ArtifactRef> {
            Vec::new()
        }
        async fn blob(&self, _sha: &[u8]) -> Option<Vec<u8>> {
            Some(self.blob.clone())
        }
    }

    fn strict_dart() -> Strict {
        Strict {
            // The lowercase form `build_id_hex`/the upload path produce.
            stored_debug_id: "ab36961b44baef9d7e3b9296dff3ce3e59be51a3",
            stored_release: "",
            blob: include_bytes!("../tests/fixtures/sample.elf").to_vec(),
        }
    }

    /// A trace whose one frame resolves to `compute_total` in `sample.elf`, with
    /// `build_id` interpolated so each test can vary only that.
    fn dart_trace_with(build_id: &str) -> String {
        format!(
            "*** *** ***\n\
             build_id: '{build_id}'\n\
             isolate_dso_base: 0, vm_dso_base: 0\n\
             \x20   #00 abs 0000000000400446 virt 0000000000400446 _kDartIsolateSnapshotInstructions+0x446\n"
        )
    }

    /// The read half of the debug-id normalization. `debug_meta.build_id` is
    /// untrusted wire input; a client reporting the id uppercase used to match a
    /// stored lowercase id (both sides verbatim), then stopped when the upload
    /// path alone started lowercasing. Both sides normalize now.
    #[tokio::test]
    async fn an_uppercase_debug_meta_build_id_matches_the_lowercase_stored_id() {
        let fetch = strict_dart();
        let reported = fetch.stored_debug_id.to_ascii_uppercase();
        let s = Symbolicator::new(4 << 20);
        let (out, status) = s
            // The trace header is deliberately a *different* id: this pins the
            // `debug_meta` value's own normalization, with no chance of the
            // fallback below supplying the match instead.
            .symbolicate_dart(&fetch, &dart_trace_with("unrelated"), Some(&reported), None)
            .await;
        assert_eq!(
            status,
            Status::Symbolicated,
            "an uppercase reported build_id must resolve against the lowercase stored id"
        );
        assert_eq!(out[0].function.as_deref(), Some("compute_total"));
    }

    /// Same rule on the other source of the key: the trace's own `build_id:`
    /// header, used when `debug_meta` carries none.
    #[tokio::test]
    async fn an_uppercase_build_id_in_the_trace_itself_is_normalized_too() {
        let fetch = strict_dart();
        let s = Symbolicator::new(4 << 20);
        let trace = dart_trace_with(&fetch.stored_debug_id.to_ascii_uppercase());
        let (out, status) = s.symbolicate_dart(&fetch, &trace, None, None).await;
        assert_eq!(
            status,
            Status::Symbolicated,
            "trace fallback must normalize"
        );
        assert_eq!(out[0].function.as_deref(), Some("compute_total"));
    }

    /// A blank reported build_id is absent, not a key. Mirrors `blank_to_none` on
    /// the write side; before, `" "` won over a perfectly good trace header and
    /// was looked up verbatim.
    #[tokio::test]
    async fn a_blank_reported_build_id_falls_back_to_the_trace() {
        let fetch = strict_dart();
        let s = Symbolicator::new(4 << 20);
        let trace = dart_trace_with(fetch.stored_debug_id);
        let (_out, status) = s.symbolicate_dart(&fetch, &trace, Some("  "), None).await;
        assert_eq!(status, Status::Symbolicated);
    }

    /// The release half of the same asymmetry: the upload path trims what it
    /// stores, so the lookup has to trim too. An SDK reading its release from a
    /// file or env var at init is the realistic source of the newline.
    #[tokio::test]
    async fn a_release_with_surrounding_whitespace_still_matches() {
        let raw = br#"{"version":3,"sources":["foo.ts"],"names":["greet"],"mappings":"AAAAA","sourcesContent":["export function greet(){ return 1 }"]}"#.to_vec();
        let fetch = Strict {
            stored_debug_id: "",
            stored_release: "web@1.0.0",
            blob: raw,
        };
        let s = Symbolicator::new(4 << 20);
        let frames = vec![frame("https://x.io/static/app.min.js", 1, 1)];
        let (out, status) = s
            .symbolicate_js(&fetch, Some(" web@1.0.0\n"), &frames)
            .await;
        assert_eq!(status, Status::Symbolicated, "release must be trimmed");
        assert_eq!(out[0].filename.as_deref(), Some("foo.ts"));
    }

    /// And a whitespace-only release is still "no release", not a lookup for
    /// `""` — the pre-existing `!r.is_empty()` guard has to see the trimmed form.
    #[tokio::test]
    async fn a_whitespace_only_release_is_not_applicable() {
        let fetch = Strict {
            stored_debug_id: "",
            stored_release: "web@1.0.0",
            blob: Vec::new(),
        };
        let s = Symbolicator::new(1 << 20);
        let (_out, status) = s
            .symbolicate_js(&fetch, Some("   "), &[frame("a", 1, 1)])
            .await;
        assert_eq!(status, Status::NotApplicable);
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
isolate_dso_base: 0, vm_dso_base: 0\n\
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
isolate_dso_base: 0, vm_dso_base: 0\n\
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

    /// `tests/fixtures/sample.c` linked with `.text` at 0x0 (see the build
    /// command in `dart.rs`'s tests). `compute_total` sits at 0x0, `helper_add`
    /// at 0x11, `main` at 0x30 — verified with `nm`.
    fn zero_base_elf() -> DartMem {
        DartMem {
            elf: include_bytes!("../tests/fixtures/sample_zero_base.elf").to_vec(),
        }
    }

    /// One frame whose address cannot be determined: no ` virt …` fragment, and
    /// an `abs` BELOW `isolate_dso_base`, so the `checked_sub` in
    /// `DartFrameRef::lookup_addr` returns None.
    const DART_UNDETERMINABLE: &str = "\
build_id: 'x'\n\
isolate_dso_base: 7b9c2b7000, vm_dso_base: 7b9c2b7000\n\
    #00 abs 0000000000001000 _kDartIsolateSnapshotInstructions+0x1000\n";

    /// A frame with no determinable address must stay unresolved rather than be
    /// looked up at some stand-in address.
    ///
    /// Against an ELF based at 0 this is not a cosmetic difference: with the old
    /// `lookup_addr(...).unwrap_or(0)` this exact input rendered `compute_total`
    /// at `sample.c:1` with `symbolicated: true` — and an overall status of
    /// `symbolicated`, not even `partial`, so nothing downstream flagged it. That
    /// status is also what `sauron-api`'s `symbolicate_with` fast path keys on: it
    /// returns early whenever a stored `symbolicated` status already carries
    /// frames, so a wrong frame stored once is served unexamined thereafter.
    #[tokio::test]
    async fn a_frame_with_no_determinable_address_is_not_resolved_at_zero() {
        let s = Symbolicator::new(4 << 20);
        let (out, status) = s
            .symbolicate_dart(&zero_base_elf(), DART_UNDETERMINABLE, Some("x"), None)
            .await;
        assert_eq!(out.len(), 1, "the frame must still be reported");
        assert!(
            !out[0].symbolicated,
            "an undeterminable address must not resolve, got {:?}",
            out[0]
        );
        assert_eq!(out[0].function, None);
        // The raw `abs` is still shown, so the frame stays legible.
        assert_eq!(out[0].filename.as_deref(), Some("<dart> +0x1000"));
        assert_eq!(status, Status::NoArtifacts);
    }

    /// Frame order and per-frame identity survive an undeterminable frame in the
    /// MIDDLE of the trace. `#00`/`#02` carry `virt` (0x11 → `helper_add`,
    /// 0x30 → `main`); `#01` cannot be determined. Output is stored crash-last,
    /// so the trace's `#00` lands at the END.
    ///
    /// This is what catches dropping the frame from the address list instead of
    /// keeping its slot. Measured with that variant in place (release build, so
    /// the `debug_assert` in `symbolicate_dart` is compiled out): the output came
    /// back `len=2` with `#01` wearing `main`'s symbols and `#02` gone entirely,
    /// still reporting `status=symbolicated`. `resolve`'s results are paired to
    /// frames by position, so a missing slot shifts every later frame by one and
    /// `zip` silently swallows the tail.
    #[tokio::test]
    async fn dart_frame_order_survives_an_undeterminable_middle_frame() {
        let trace = "\
build_id: 'x'\n\
isolate_dso_base: 7b9c2b7000, vm_dso_base: 7b9c2b7000\n\
    #00 abs 0000007b9c2b7011 virt 0000000000000011 sym+0x11\n\
    #01 abs 0000000000001000 sym+0x1000\n\
    #02 abs 0000007b9c2b7030 virt 0000000000000030 sym+0x30\n";
        let s = Symbolicator::new(4 << 20);
        let (out, status) = s
            .symbolicate_dart(&zero_base_elf(), trace, Some("x"), None)
            .await;
        assert_eq!(out.len(), 3, "no frame may be dropped or duplicated");

        // Stored order: #02, #01, #00.
        assert_eq!(out[0].function.as_deref(), Some("main"));
        assert_eq!(out[0].lineno, Some(7));
        assert!(out[0].symbolicated);

        assert!(!out[1].symbolicated, "#01 must not have taken a symbol");
        assert_eq!(out[1].function, None);
        assert_eq!(out[1].filename.as_deref(), Some("<dart> +0x1000"));

        assert_eq!(out[2].function.as_deref(), Some("helper_add"));
        assert_eq!(out[2].lineno, Some(4));
        assert!(out[2].symbolicated);

        assert_eq!(
            status,
            Status::Partial,
            "some frames resolved and one did not"
        );
    }

    #[tokio::test]
    async fn dart_no_symbols_is_no_artifacts() {
        let trace =
            "build_id: 'x'\nisolate_dso_base: 0, vm_dso_base: 0\n    #00 abs 100 virt 100 sym\n";
        let fetch = Mem {
            name: "n".into(),
            raw: vec![],
        };
        let s = Symbolicator::new(1 << 20);
        let (out, status) = s.symbolicate_dart(&fetch, trace, Some("x"), None).await;
        assert_eq!(status, Status::NoArtifacts);
        assert!(!out[0].symbolicated);
    }

    fn map_fetch() -> MapMem {
        MapMem {
            stored_debug_id: "ab36961b44baef9d7e3b9296dff3ce3e59be51a3",
            json: br#"["CartException","xY1","CheckoutBloc","aB2"]"#.to_vec(),
        }
    }

    #[tokio::test]
    async fn deobfuscates_a_type_against_the_uploaded_map() {
        let sym = Symbolicator::new(1 << 20);
        let got = sym
            .deobfuscate_type(
                &map_fetch(),
                Some("ab36961b44baef9d7e3b9296dff3ce3e59be51a3"),
                "xY1",
            )
            .await;
        assert_eq!(got.as_deref(), Some("CartException"));
    }

    #[tokio::test]
    async fn the_debug_id_is_normalized_before_the_lookup() {
        // `MapMem` answers only the canonical lowercase id, exactly as the SQL
        // equality in the real fetchers does. An uppercase id reaching the
        // query unchanged finds no row and de-obfuscates nothing — the same
        // write-only-normalization bug `Strict` exists to catch on the frame
        // path.
        let sym = Symbolicator::new(1 << 20);
        let got = sym
            .deobfuscate_type(
                &map_fetch(),
                Some("AB36961B44BAEF9D7E3B9296DFF3CE3E59BE51A3"),
                "xY1",
            )
            .await;
        assert_eq!(got.as_deref(), Some("CartException"));
    }

    #[tokio::test]
    async fn every_miss_is_none_so_the_caller_keeps_what_it_had() {
        let sym = Symbolicator::new(1 << 20);
        let f = map_fetch();
        // No debug id on the event.
        assert_eq!(sym.deobfuscate_type(&f, None, "xY1").await, None);
        // Empty type.
        assert_eq!(
            sym.deobfuscate_type(&f, Some("ab36961b44baef9d7e3b9296dff3ce3e59be51a3"), "")
                .await,
            None
        );
        // A build with no map uploaded.
        assert_eq!(
            sym.deobfuscate_type(&f, Some("00000000"), "xY1").await,
            None
        );
        // A name the map does not cover — including one that was never
        // obfuscated, which must be left exactly as it is.
        assert_eq!(
            sym.deobfuscate_type(
                &f,
                Some("ab36961b44baef9d7e3b9296dff3ce3e59be51a3"),
                "StateError"
            )
            .await,
            None
        );
    }
}
