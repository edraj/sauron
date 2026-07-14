//! One-shot migration runner. Applies pending migrations and exits — the
//! Docker Compose `migrate` service the other containers depend on.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_target(false).init();

    let url =
        std::env::var("DATABASE_URL").map_err(|_| anyhow::anyhow!("DATABASE_URL is required"))?;

    tracing::info!("applying pending migrations");
    sauron_db::run_pending_migrations(&url).await?;
    tracing::info!("migrations up to date");
    Ok(())
}
