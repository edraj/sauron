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

    // Opt-in, and deliberately NOT part of the default no-arg path.
    //
    // This binary is the `sauron-migrate.service` oneshot that every RPM daemon
    // pulls in via `Requires=`, and systemd never retries a failed start job —
    // so anything slow here delays every daemon's start, and anything that can
    // fail here takes them all down with it. The backfill aggregates all 29
    // partitions of the two largest tables, which is exactly that kind of work.
    // Operators run `sauron-migrate backfill-person-envs` by hand, after the
    // migrations, at a time of their choosing.
    //
    // Until it has run for an app, `repo::list_persons` reads that app through
    // the pre-rollup query, so skipping this is a performance decision and never
    // a correctness one.
    if std::env::args().any(|a| a == "backfill-person-envs") {
        let pool = sauron_db::build_pool(&url, 4)?;
        sauron_db::person_env_backfill::backfill_all(&pool).await?;
    }

    // Opt-in for exactly the same reason as `backfill-person-envs` above: this
    // binary is the `sauron-migrate.service` oneshot that every RPM daemon pulls
    // in via `Requires=`, systemd never retries a failed start job, and this
    // aggregates all 29 partitions of the two largest tables.
    //
    // Until it has run for an app, `repo::list_device_groups` reads that app
    // through the pre-rollup query, so skipping this is a performance decision
    // and never a correctness one.
    if std::env::args().any(|a| a == "backfill-device-envs") {
        let pool = sauron_db::build_pool(&url, 4)?;
        sauron_db::device_env_backfill::backfill_all(&pool).await?;
    }

    // Opt-in like its two siblings above, and heavier than both: this replays
    // every pre-epoch day of all three firehose tables into the dashboard
    // rollups (migration 71). Until it runs, every analytics read takes the
    // pre-rollup query — a performance decision, never a correctness one.
    // Refuses (with a warning) when marker rows already exist: the aggregates
    // are additive and a second run would double-count.
    if std::env::args().any(|a| a == "backfill-rollups") {
        let pool = sauron_db::build_pool(&url, 4)?;
        let mut conn = sauron_db::conn(&pool).await?;
        sauron_db::rollups::fold::backfill_all(&mut conn, 2000, |day| {
            tracing::info!(%day, "rollup backfill: day complete");
        })
        .await?;
        tracing::info!("rollup backfill complete");
    }
    Ok(())
}
