//! One-shot migration runner. Applies pending migrations and exits — the
//! Docker Compose `migrate` service the other containers depend on, and the
//! `sauron-migrate.service` oneshot every RPM daemon now pulls in via
//! `Requires=`.
//!
//! Because the daemons declare `Requires=`, a failure here fails *their* start
//! jobs too, and systemd never retries a failed start job. So this binary
//! tolerates a Postgres that is merely not up **yet** — see
//! [`sauron_db::run_pending_migrations_waiting`] for why that retry is load
//! bearing rather than a nicety.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_target(false).init();

    let url =
        std::env::var("DATABASE_URL").map_err(|_| anyhow::anyhow!("DATABASE_URL is required"))?;

    // Parsed leniently on purpose: an unparseable or absent value falls back to
    // the default rather than refusing to run. Refusing would mean a typo in an
    // operator's env file takes down every daemon that now depends on this unit,
    // which is a worse failure than waiting the default 120s.
    //
    // `0` is honoured as "do not wait", which is what the Compose path wants —
    // there the `migrate` service has an explicit `depends_on` on a healthy
    // Postgres, so a retry here would only mask a broken dependency.
    let wait_secs = std::env::var("MIGRATE_WAIT_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(sauron_db::DEFAULT_MIGRATE_WAIT_SECS);

    tracing::info!("applying pending migrations (connect tolerance {wait_secs}s)");
    sauron_db::run_pending_migrations_waiting(&url, std::time::Duration::from_secs(wait_secs))
        .await?;
    tracing::info!("migrations up to date");
    Ok(())
}
