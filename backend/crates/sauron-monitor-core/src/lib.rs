//! Uptime-monitor decision logic (pure) plus probe execution (I/O).
//!
//! Pure modules are unit-tested without a network or database. Later tasks add
//! `state`, `ssrf`, `webhook`, and `probe` modules alongside `status`.

pub mod status;

pub use status::{evaluate_http, status_matches};
