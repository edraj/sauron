//! Uptime-monitor decision logic (pure) plus probe execution (I/O).
//!
//! The pure modules (`status`, `state`, `ssrf`, `webhook`) are unit-tested
//! without a network or database; `probe` performs the actual HTTP/TCP I/O.

pub mod probe;
pub mod ssrf;
pub mod state;
pub mod status;
pub mod webhook;

pub use probe::{probe, Kind, ProbeSpec};
pub use state::{apply, status_str, MonitorState, Outcome, ProbeResult, Status, TransitionKind};
pub use status::{evaluate_http, status_matches};
pub use webhook::WebhookPayload;
