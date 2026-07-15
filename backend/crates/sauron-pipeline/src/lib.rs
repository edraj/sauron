//! `sauron-pipeline` — the ingest worker's brain, kept library-shaped so it can
//! be tested without a running server. Consumes [`IngestJob`]s, enriches them,
//! groups errors into issues, and writes durable rows.
//!
//! [`IngestJob`]: sauron_core::envelope::IngestJob

pub mod enrich;
pub mod process;
pub mod symbolize;
pub mod worker;

pub use process::process_job;
pub use symbolize::SymbolizeCtx;
pub use worker::spawn_workers;
