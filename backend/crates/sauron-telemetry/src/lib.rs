//! Shared tracing setup for the Sauron services.

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

/// Initialize a `tracing` subscriber honoring `RUST_LOG` (default `info`).
/// Call once, at the top of each binary's `main`.
pub fn init(service: &str) {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,sauron=debug"));

    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_target(true))
        .init();

    tracing::info!(service, "tracing initialized");
}
