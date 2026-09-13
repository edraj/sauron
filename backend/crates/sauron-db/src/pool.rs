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
/// [`build_pool`] with a server-side `statement_timeout` baked into every
/// connection as its SESSION DEFAULT (via the connection string's `options`),
/// so `RESET statement_timeout` lands back on the budget rather than on "off".
///
/// This is the pool for request-serving processes. A request layer that gives
/// up after N seconds only stops *waiting*; the Postgres backend keeps
/// executing the abandoned statement to completion, still holding its pool
/// slot and its share of the server. Measured at 30M rows: a device-groups
/// query answered 503 at 60 s and ran 182 s more; a handful of dashboard
/// reloads then filled every slot with orphans and cheap endpoints failed
/// too. A statement budget is what makes the timeout actually free resources.
///
/// Work that legitimately outlives a request (cache recomputes, retention
/// sweeps) must not share this pool — give it a plain [`build_pool`].
pub fn build_pool_with_statement_timeout(
    database_url: &str,
    max_size: usize,
    timeout_ms: u64,
) -> anyhow::Result<PgPool> {
    build_pool(
        &url_with_statement_timeout(database_url, timeout_ms),
        max_size,
    )
}

/// Append `options=-c statement_timeout=<ms>` to a Postgres URL, keeping any
/// query string it already carries. Percent-encoded because the value holds a
/// space and an `=`, both of which the URL query grammar would otherwise eat.
pub fn url_with_statement_timeout(database_url: &str, timeout_ms: u64) -> String {
    let sep = if database_url.contains('?') { '&' } else { '?' };
    format!("{database_url}{sep}options=-c%20statement_timeout%3D{timeout_ms}")
}

/// Whether a diesel error is Postgres cancelling a statement for exceeding
/// `statement_timeout` (SQLSTATE 57014, message "canceling statement due to
/// statement timeout"). Callers map it to a "try a narrower window" response
/// instead of a generic 500.
pub fn is_statement_timeout(err: &diesel::result::Error) -> bool {
    match err {
        diesel::result::Error::DatabaseError(_, info) => {
            info.message().contains("statement timeout")
        }
        _ => false,
    }
}

pub async fn conn(pool: &PgPool) -> anyhow::Result<PgConn> {
    pool.get()
        .await
        .map_err(|e| anyhow::anyhow!("db pool checkout failed: {e}"))
}
