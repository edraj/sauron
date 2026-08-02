//! `sauron-mail` — compose and transmit one message.
//!
//! This crate knows how to compose and transmit a message. It does **not** know
//! what a user is, where a message queues, or when to retry. Those live in
//! `sauron-db` (`mail_outbox` + its repository functions) and in
//! `sauron-api`'s `mail.rs`, and that split is why this crate can stay a leaf
//! with no data-layer dependency.
//!
//! **The outbox is this codebase's async side-effect primitive, not a mail
//! detail.** `mail_outbox` plus its claim/drain/backoff/reap loop is the first
//! durable, restart-surviving, observable deferred-work mechanism here. Anything
//! that wants "do this after the response" should enqueue rather than
//! `tokio::spawn` a detached network call: a spawn dies with the process, has no
//! backoff, has no bound on concurrency under a burst, and cannot be observed by
//! an integration test.

pub mod kind;
pub mod smtp;
pub mod template;
pub mod text;

pub use kind::MailKind;
pub use smtp::{
    is_transient, normalize_recipient, send, MailBody, MailError, OutgoingMail, SmtpClient,
    SmtpParams,
};
pub use template::{render, Branding, Cta, MailContent, RenderedMail, TemplateError};
pub use text::{html_escape, substitute};

// Single home for both: `sauron-core`. This crate depends on `sauron-core`, so
// defining them here too would be a second, incompatible type — and `Config`
// cannot depend on `sauron-mail` without a cycle.
pub use sauron_core::config::{SmtpSettings, SmtpTls};
