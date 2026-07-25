//! Uptime-monitor decision logic (pure) plus probe execution (I/O).
//!
//! The pure modules (`status`, `state`, `webhook`) and the `ssrf` address
//! classifier are unit-tested without a network or database; `probe` performs
//! the actual HTTP/TCP I/O. `ssrf` also owns the IP-pinning enforcement shared
//! with outbound alert delivery.

pub mod probe;
pub mod ssrf;
pub mod state;
pub mod status;
pub mod webhook;

pub use probe::{probe, Kind, ProbeSpec};
pub use ssrf::{guarded_client_builder, is_blocked_ip, resolve_checked, SsrfResolver};
pub use state::{apply, status_str, MonitorState, Outcome, ProbeResult, Status, TransitionKind};
pub use status::{evaluate_http, status_matches};
pub use webhook::WebhookPayload;
