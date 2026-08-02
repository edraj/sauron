//! `sauron-alerts` — the admin-customizable notification engine.
//!
//! An admin configures **channels** (where: email/Slack/Discord/Matrix/
//! Telegram/webhook) and **rules** (when: monitor transitions, new issues,
//! error/event thresholds and spikes, latency degradation — each with a
//! deeply-customizable `conditions` bag, throttle, and message template).
//!
//! Module map:
//! - [`channel`] — kinds, config/secret validation, typed destinations.
//! - [`rule`]    — trigger types + pure condition evaluation.
//! - [`subscription`] — personal subscriptions: kinds, conditions, probe
//!   coalescing, quiet hours, and the delivery-time coverage predicate.
//! - [`sweep`]   — self-disable personal subscriptions whose owner lost reach.
//! - [`render`]  — per-channel payloads + safe `{{var}}` templates.
//! - [`crypto`]  — AES-GCM at-rest secret encryption + HMAC signing.
//! - [`net`]     — SSRF-safe, IP-pinned outbound HTTP.
//! - [`deliver`] — channel transports (SMTP + HTTP).
//! - [`engine`]  — throttle → render → deliver → record.
//!
//! Event-driven triggers (monitor up/down) are dispatched inline by
//! `sauron-monitor`; metric triggers are polled by the `sauron-alerts` binary's
//! evaluator loop against indexed, window-bounded queries.

pub mod channel;
pub mod crypto;
pub mod deliver;
pub mod engine;
pub mod net;
pub mod render;
pub mod rule;
pub mod subscription;
pub mod sweep;

pub use channel::{ChannelKind, Destination};
pub use crypto::SecretCipher;
pub use deliver::DeliverOpts;
pub use engine::AlertEngine;
pub use render::{AlertContext, Severity};
pub use rule::{Conditions, TriggerType};
