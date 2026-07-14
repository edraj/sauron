//! Uptime-monitor decision logic (pure) plus probe execution (I/O).
//!
//! The pure modules (`status`, `state`, `ssrf`, `webhook`) are unit-tested
//! without a network or database; `probe` performs the actual HTTP/TCP I/O.

pub mod status;
pub mod state;
pub mod ssrf;
pub mod webhook;
pub mod probe;

pub use status::{evaluate_http, status_matches};
pub use state::{apply, status_str, MonitorState, Outcome, ProbeResult, Status, TransitionKind};
pub use webhook::WebhookPayload;
pub use probe::{probe, Kind, ProbeSpec};
