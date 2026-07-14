//! `sauron-db` — the only crate that knows about diesel.
//!
//! Owns the generated [`schema`], the row/insert [`models`], the diesel-async
//! [`pool`], the [`repo`]sitory functions both binaries call, and the embedded
//! migrations run at startup.

pub mod filter;
pub mod models;
pub mod pool;
pub mod repo;
pub mod schema;

pub use pool::{build_pool, conn, PgConn, PgPool};

/// Re-exported so downstream crates can name the connection type without a
/// direct diesel-async dependency.
pub use diesel_async::AsyncPgConnection;

use diesel_async::async_connection_wrapper::AsyncConnectionWrapper;
use diesel_migrations::{embed_migrations, EmbeddedMigrations, MigrationHarness};

/// Migrations compiled into the binary. Path is relative to this crate.
pub const MIGRATIONS: EmbeddedMigrations = embed_migrations!("../../migrations");

/// Apply any pending migrations. Diesel migrations are synchronous, so we run
/// them through diesel-async's [`AsyncConnectionWrapper`] on a blocking thread —
/// this avoids linking libpq while still reusing the async Postgres transport.
pub async fn run_pending_migrations(database_url: &str) -> anyhow::Result<()> {
    let url = database_url.to_owned();
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        use diesel::Connection as _;
        let mut wrapper = AsyncConnectionWrapper::<AsyncPgConnection>::establish(&url)
            .map_err(|e| anyhow::anyhow!("connect for migrations: {e}"))?;
        wrapper
            .run_pending_migrations(MIGRATIONS)
            .map_err(|e| anyhow::anyhow!("run migrations: {e}"))?;
        Ok(())
    })
    .await??;
    Ok(())
}
