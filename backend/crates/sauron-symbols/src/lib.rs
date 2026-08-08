//! Sauron symbolication core.
//!
//! Storage-agnostic primitives shared by both symbolication pipelines (JS source
//! maps and Dart debug-info). Slice 1 ships the foundation only:
//!
//! - [`content`] — content-addressing (SHA-256), bounded zstd (de)compression.
//! - [`cache`] — a byte-bounded LRU with per-key single-flight for parsed indexes.
//!
//! The JS ([`js`]) and Dart resolvers land in slices 2 and 3.

pub mod build_id;
pub mod cache;
pub mod content;
pub mod dart;
pub mod dart_trace;
pub mod engine;
pub mod js;
pub mod matcher;
pub mod normalize;

pub use build_id::build_id_hex;
pub use cache::ByteLru;
pub use content::{compress, decompress, hex, sha256, SymbolError};
pub use engine::{ArtifactRef, BlobFetch, RawFrame, ResolvedFrame, Status, Symbolicator};
pub use js::{ParsedSourceMap, ResolvedLoc, SourceContext};
pub use normalize::{normalize_debug_id, normalize_release};
