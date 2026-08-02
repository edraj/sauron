//! `sauron-inspector` — every decision the PII inspector makes, as pure
//! functions over owned data.
//!
//! No diesel, no axum, no tokio. CI runs `cargo test --workspace` against a
//! machine with no Postgres and the DB harness SKIPS, so a decision that
//! lives in a repo function or a handler is a decision with no test. That is
//! why the walker, the matcher, the detectors, the redactor, the prefilter
//! builder, the path grammar, target expansion, target resolution and the
//! mask applier all live here rather than in the worker binary.

pub mod columns;
pub mod detect;
pub mod mask;
// `match` is a keyword, so the module is named `matching` while the file keeps
// the name the design's file list uses.
#[path = "match.rs"]
pub mod matching;
pub mod path;
pub mod prefilter;
pub mod redact;
pub mod targets;
pub mod units;
pub mod walk;
