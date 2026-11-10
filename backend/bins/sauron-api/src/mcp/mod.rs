//! The read-only MCP endpoint, `/v1/mcp`.
//!
//! Developers point Claude Code (or any MCP client) at it with a personal API
//! key (`sauron_pat_…`, see `routes::account`'s `/v1/me/api-keys`). Three
//! parts:
//!
//! - [`auth`]: the `require_api_key` middleware. It is the only thing a key
//!   authenticates, and it hands the tools an [`auth::McpCaller`].
//! - [`server`]: `SauronMcp`, the rmcp `ServerHandler` that the tools register
//!   on. Every tool resolves the caller's `user_id` and goes through the same
//!   authorization the dashboard uses; nothing here grants access on its own.
//! - [`access`]: what every tool goes through to turn its arguments into an
//!   authorized read (app reference, environment, release), the tool error
//!   type, the deadline and the dashboard links. [`format`] holds the shared
//!   text helpers, and [`tools`] the tool bodies.
//! - this file: the official `rmcp` Streamable HTTP service, run **stateless
//!   with plain JSON responses**. No MCP session lives on the server, so any
//!   API replica behind a load balancer can answer any request (no sticky
//!   sessions), proxy buffering cannot break a stream that does not exist, and
//!   none of the tools needs a server-initiated message. `GET` therefore has
//!   no stream to open and answers 405, as the transport spec allows.

mod access;
mod auth;
mod format;
mod server;
mod tools;

use axum::Router;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};

use crate::AppState;

/// Largest JSON-RPC request body rmcp will read. The same ceiling as the rest
/// of the JSON API (`API_JSON_BODY_LIMIT` in `main.rs`); rmcp reads the body
/// itself, so axum's `DefaultBodyLimit` alone would not bound it.
const MCP_MAX_REQUEST_BODY_BYTES: usize = 1024 * 1024;

/// The `/v1/mcp` router, with key authentication in front of the service.
///
/// Merged into the main router ahead of its CORS, concurrency limit, timeout,
/// security headers and trace layers, so MCP requests get all of them like any
/// other API request. Not its `DefaultBodyLimit`, which only configures axum's
/// extractors: the body ceiling is [`MCP_MAX_REQUEST_BODY_BYTES`].
pub(crate) fn router(state: AppState) -> Router<AppState> {
    let config = StreamableHttpServerConfig::default()
        // Stateless: no `Mcp-Session-Id`, every request stands alone.
        .with_legacy_session_mode(false)
        // A plain `application/json` reply instead of a one-message SSE stream.
        .with_json_response(true)
        .with_sse_keep_alive(None)
        // rmcp's default accepts only loopback `Host` values. That check
        // exists to stop DNS rebinding against an UNAUTHENTICATED server on a
        // developer's machine. Neither half applies here: installs run behind
        // arbitrary hostnames (an allowlist would 403 every real deployment),
        // and every request must carry a bearer key that a rebinding page
        // cannot obtain.
        .disable_allowed_hosts()
        .with_max_request_body_bytes(MCP_MAX_REQUEST_BODY_BYTES);

    // Built once; the factory rmcp calls per request only clones it. In
    // stateless mode the session manager holds no sessions (rmcp's own
    // stateless tests pass this same default). Having no event store is also
    // what makes rmcp answer `GET` with 405 rather than open a replay stream.
    let server = server::SauronMcp::new(state.clone());
    let service = StreamableHttpService::new(
        move || Ok(server.clone()),
        LocalSessionManager::default().into(),
        config,
    );

    Router::new()
        // `nest_service`, not `.route(…)`, on purpose: `router_parity`
        // (openapi.rs) and `audit_coverage` scan `.route(` calls for the REST
        // surface. `/v1/mcp` is a JSON-RPC endpoint with no OpenAPI operation
        // and no mutating handler to audit, so it stays out of both scans.
        .nest_service("/v1/mcp", service)
        // `route_layer`, not `layer`: it wraps only the routes above (the
        // mount path and everything under it), never this router's default
        // fallback. With `layer`, the fallback would carry the key gate too,
        // and whether an unknown path anywhere in the API answered 404 or a
        // bearer-challenge 401 would depend on the order of `merge` calls in
        // `main.rs`.
        .route_layer(axum::middleware::from_fn_with_state(
            state,
            auth::require_api_key,
        ))
}
