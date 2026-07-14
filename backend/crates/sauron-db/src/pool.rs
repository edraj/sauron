//! diesel-async connection pool (deadpool backend).

use diesel_async::pooled_connection::deadpool::{Object, Pool};
use diesel_async::pooled_connection::AsyncDieselConnectionManager;
use diesel_async::AsyncPgConnection;

/// The application-wide async Postgres pool. Cloneable and stored in axum state.
pub type PgPool = Pool<AsyncPgConnection>;

/// A checked-out pooled connection. Derefs to `AsyncPgConnection`, so it can be
/// passed to repository functions as `&mut conn`.
pub type PgConn = Object<AsyncPgConnection>;

/// Build the pool from a connection URL.
pub fn build_pool(database_url: &str, max_size: usize) -> anyhow::Result<PgPool> {
    let manager = AsyncDieselConnectionManager::<AsyncPgConnection>::new(database_url);
    let pool = Pool::builder(manager)
        .max_size(max_size.max(1))
        .build()
        .map_err(|e| anyhow::anyhow!("failed to build db pool: {e}"))?;
    Ok(pool)
}

/// Check out a connection, mapping pool errors into `anyhow`.
pub async fn conn(pool: &PgPool) -> anyhow::Result<PgConn> {
    pool.get()
        .await
        .map_err(|e| anyhow::anyhow!("db pool checkout failed: {e}"))
}
