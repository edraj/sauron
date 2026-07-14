//! `sauron-core` — pure, I/O-free domain layer shared by every Sauron service.
//!
//! It owns the ingest wire contract (the [`envelope`] module), the error
//! [`fingerprint`]ing algorithm used to group issues, small domain [`ids`]
//! helpers, and the process [`config`] loader. Nothing here touches a database,
//! a socket, or a clock beyond `chrono::Utc::now` — which keeps it trivially
//! unit-testable.

pub mod config;
pub mod envelope;
pub mod fingerprint;
pub mod ids;

pub use config::Config;
pub use envelope::{
    AnalyticsItem, Breadcrumb, Envelope, EnvelopeContext, EnvelopeHeader, EnvelopeItem, EventUser,
    ExceptionInfo, Frame, IdentifyItem, IngestJob, Level, Mechanism, SdkInfo,
};
pub use fingerprint::fingerprint;
