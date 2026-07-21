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

// ===========================================================================
// Admin DDL — create/drop whole databases (used by the crebain benchmark to
// spin up and tear down an isolated ephemeral database).
// ===========================================================================

/// Create a database by `db_name` on the server addressed by `maintenance_url`.
/// `maintenance_url` must point at any *existing* database on the same server
/// other than the one being created (e.g. the app's own database).
///
/// `CREATE DATABASE` cannot run inside a transaction and cannot be parameterized,
/// so it is issued through the simple query protocol (`batch_execute`) and the
/// identifier is validated rather than bound.
pub async fn create_database(maintenance_url: &str, db_name: &str) -> anyhow::Result<()> {
    run_admin_ddl(
        maintenance_url,
        db_name,
        &format!("CREATE DATABASE \"{db_name}\""),
    )
    .await
}

/// Drop `db_name` if it exists, terminating any other sessions still connected
/// (`WITH (FORCE)`, Postgres 13+). Idempotent. `maintenance_url` must not point
/// at the database being dropped.
pub async fn drop_database(maintenance_url: &str, db_name: &str) -> anyhow::Result<()> {
    run_admin_ddl(
        maintenance_url,
        db_name,
        &format!("DROP DATABASE IF EXISTS \"{db_name}\" WITH (FORCE)"),
    )
    .await
}

/// Guard against SQL injection through an un-bindable identifier: only a plain,
/// lowercase Postgres identifier (letters/digits/underscore, not starting with a
/// digit, ≤ 63 bytes) is allowed.
fn validate_db_ident(name: &str) -> anyhow::Result<()> {
    let valid = !name.is_empty()
        && name.len() <= 63
        && name
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b == b'_')
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if valid {
        Ok(())
    } else {
        anyhow::bail!("unsafe database identifier: {name:?}")
    }
}

async fn run_admin_ddl(maintenance_url: &str, db_name: &str, sql: &str) -> anyhow::Result<()> {
    use diesel_async::{AsyncConnection, SimpleAsyncConnection};
    validate_db_ident(db_name)?;
    let mut conn = AsyncPgConnection::establish(maintenance_url)
        .await
        .map_err(|e| anyhow::anyhow!("connect maintenance db: {e}"))?;
    conn.batch_execute(sql)
        .await
        .map_err(|e| anyhow::anyhow!("admin ddl `{sql}` failed: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod admin_tests {
    use super::validate_db_ident;

    #[test]
    fn accepts_safe_bench_names() {
        assert!(validate_db_ident("crebain_bench_0123456789abcdef0123456789abcdef").is_ok());
        assert!(validate_db_ident("sauron").is_ok());
        assert!(validate_db_ident("_x").is_ok());
    }

    #[test]
    fn rejects_unsafe_names() {
        assert!(validate_db_ident("").is_err());
        assert!(validate_db_ident("has space").is_err());
        assert!(validate_db_ident("drop\";--").is_err());
        assert!(validate_db_ident("1leading_digit").is_err());
        assert!(validate_db_ident("UpperCase").is_err());
        assert!(validate_db_ident(&"x".repeat(64)).is_err());
    }
}
