//! diesel-async connection pool (deadpool backend).

use std::time::Duration;

use deadpool::managed::Timeouts;
use deadpool::Runtime;
use diesel_async::pooled_connection::deadpool::{Object, Pool};
use diesel_async::pooled_connection::AsyncDieselConnectionManager;
use diesel_async::AsyncPgConnection;

/// The application-wide async Postgres pool. Cloneable and stored in axum state.
pub type PgPool = Pool<AsyncPgConnection>;

/// A checked-out pooled connection. Derefs to `AsyncPgConnection`, so it can be
/// passed to repository functions as `&mut conn`.
pub type PgConn = Object<AsyncPgConnection>;

/// How long a caller waits for a free connection before giving up.
///
/// deadpool's default is *no* timeout, so once every connection is checked out
/// each new request parks on the pool semaphore indefinitely — requests pile up
/// invisibly instead of failing fast, and the service stops shedding load. A
/// bounded wait turns saturation into a prompt 500 the caller can retry.
const POOL_WAIT_TIMEOUT: Duration = Duration::from_secs(5);
/// Cap on establishing a brand-new connection (unreachable/hung Postgres).
const POOL_CREATE_TIMEOUT: Duration = Duration::from_secs(10);
/// Cap on the liveness check when recycling an idle connection.
const POOL_RECYCLE_TIMEOUT: Duration = Duration::from_secs(5);

/// Build the pool from a connection URL.
pub fn build_pool(database_url: &str, max_size: usize) -> anyhow::Result<PgPool> {
    let manager = AsyncDieselConnectionManager::<AsyncPgConnection>::new(database_url);
    let pool = Pool::builder(manager)
        .max_size(max_size.max(1))
        .timeouts(Timeouts {
            wait: Some(POOL_WAIT_TIMEOUT),
            create: Some(POOL_CREATE_TIMEOUT),
            recycle: Some(POOL_RECYCLE_TIMEOUT),
        })
        // deadpool only enforces timeouts when a runtime is configured; without
        // this every checkout would fail with `NoRuntimeSpecified`.
        .runtime(Runtime::Tokio1)
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
