//! Rejects `?release=` on every **GET** route that does not consume it, so a
//! caller never silently gets an unfiltered answer. Mirrors the
//! `environment_id` fail-loud rule (`routes/scope.rs`'s
//! `reject_environment_id`), but as one middleware instead of a call per
//! handler, because the accepting set is five routes and the rejecting set is
//! every other read.
//!
//! Writes are out of scope on purpose — `release` on a non-`GET` is the
//! written row's own attribute, not a filter. See [`method_is_guarded`].
//!
//! # No `crate::` imports, on purpose
//!
//! `tests/http_release_scoping.rs` includes this file verbatim via
//! `#[path = "../src/release_guard.rs"] mod release_guard;` — the same trick
//! `route_table.rs` already uses, and for the same reason: an integration
//! test cannot `use` a binary crate. But `crate::` inside a `#[path]`-included
//! file resolves against the INCLUDING crate root, not `sauron-api`'s — a
//! `use crate::error::ApiError` here would refer to a nonexistent item in the
//! test binary. So this file sticks to `std`, `axum`, `serde_json` and
//! `form_urlencoded`, and builds its 400 body by hand rather than returning
//! `ApiError::BadRequest` — see `bad_request_body` below, and the note on
//! `templates_match_concrete_paths_only_at_the_same_depth` for what that
//! costs this file's own test coverage.

use axum::extract::Request;
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::Response;

/// Route TEMPLATES (as registered in `main.rs`) that read `release`. The
/// dashboard's release-scope parity test (Task 11) parses this array out of
/// this file's own source text, so keep it in exactly this shape: one string
/// literal per line, no computed entries.
pub const RELEASE_ACCEPTING_PATHS: &[&str] = &[
    "/v1/apps/{app_id}/issues",
    "/v1/apps/{app_id}/issues/{issue_id}/events",
    "/v1/apps/{app_id}/issues/{issue_id}/events/stats",
    "/v1/apps/{app_id}/events/list",
    "/v1/apps/{app_id}/sessions",
    "/v1/apps/{app_id}/transactions",
];

/// Does `path` (a concrete request path) match `template` (as registered with
/// axum, e.g. `/v1/apps/{app_id}/issues`)? Segment-for-segment: same segment
/// count, and every non-`{param}` template segment matches literally.
fn matches_template(template: &str, path: &str) -> bool {
    let t: Vec<&str> = template.split('/').collect();
    let p: Vec<&str> = path.split('/').collect();
    t.len() == p.len() && t.iter().zip(&p).all(|(a, b)| a.starts_with('{') || a == b)
}

pub fn path_accepts_release(path: &str) -> bool {
    // ONE trailing slash is stripped first, because `matches_template` splits
    // on `/` and `"/v1/apps/x/issues/".split('/')` yields a trailing EMPTY
    // segment — six where the template has five — so `/v1/apps/x/issues/`
    // matched nothing and the middleware answered "release is not supported on
    // this endpoint". That message is wrong however the router then treats the
    // path: if it routes to the issues handler, the caller was refused a
    // parameter that endpoint does accept; if it 404s, the caller was told
    // about a parameter instead of about the path. Either way the guard has no
    // business being the one to answer.
    //
    // Exactly one, not `trim_end_matches`: `//` is a genuinely different path
    // (an empty segment), not a cosmetic variant, and collapsing a run of them
    // would let `/v1/apps/x/issues///` in as well.
    let path = path.strip_suffix('/').unwrap_or(path);
    RELEASE_ACCEPTING_PATHS
        .iter()
        .any(|t| matches_template(t, path))
}

/// Does this request even fall under the guard?
///
/// **Only `GET`.** `release` is a *filter* parameter, and filter semantics
/// exist only on reads. On a write, `release=` is the written row's own
/// attribute and has nothing to do with narrowing a result set — the source
/// map uploader is the live example: `POST /v1/apps/{app_id}/artifacts
/// ?kind=js_sourcemap&platform=web&release=1.4.0&name=app.js.map` puts the
/// artifact's release in the query string because the request *body* is the
/// raw file (see `routes/artifacts.rs`'s `UploadParams::release`). Guarding
/// that would 400 every source-map upload that names a release, which is
/// every real one.
///
/// So the fail-loud rule is scoped to the reads it was written for: a `GET`
/// carrying `release=` either filters on it or is told it cannot. Non-`GET`
/// routes own their query parameters and reject unknown ones themselves via
/// their `Query<T>` extractor.
fn method_is_guarded(method: &Method) -> bool {
    method == Method::GET
}

/// The exact JSON envelope `ApiError::BadRequest` serializes to (see
/// `error.rs`'s `body()` helper: `{"error":{"code":"bad_request","message":…}}`),
/// built by hand because this file cannot `use crate::error::ApiError` (see
/// the module doc comment). Kept as its own function so both the middleware
/// and this file's own unit test read from one literal shape rather than two.
fn bad_request_body(message: &str) -> serde_json::Value {
    serde_json::json!({ "error": { "code": "bad_request", "message": message } })
}

/// `#[allow(dead_code)]`: this file is compiled twice — once as a module of
/// the binary itself (`main.rs`, which registers this as a middleware layer)
/// and once via `#[path]` from `tests/http_release_scoping.rs` (which drives
/// it indirectly, over real HTTP, and never calls it directly — only
/// `RELEASE_ACCEPTING_PATHS` in that compilation). Each compilation sees
/// items the other does not use — the same pattern `route_table.rs` already
/// uses for the identical reason.
#[allow(dead_code)]
pub async fn reject_release_outside_allowlist(
    req: Request,
    next: Next,
) -> Result<Response, (StatusCode, axum::Json<serde_json::Value>)> {
    let has_release = req
        .uri()
        .query()
        .map(|q| form_urlencoded::parse(q.as_bytes()).any(|(k, _)| k == "release"))
        .unwrap_or(false);
    if has_release && method_is_guarded(req.method()) && !path_accepts_release(req.uri().path()) {
        return Err((
            StatusCode::BAD_REQUEST,
            axum::Json(bad_request_body(
                "release is not supported on this endpoint",
            )),
        ));
    }
    Ok(next.run(req).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_match_concrete_paths_only_at_the_same_depth() {
        assert!(path_accepts_release("/v1/apps/0b6a/issues"));
        assert!(path_accepts_release("/v1/apps/0b6a/issues/77/events"));
        assert!(!path_accepts_release("/v1/apps/0b6a/issues/77"));
        assert!(!path_accepts_release("/v1/apps/0b6a/overview"));
        assert!(!path_accepts_release("/v1/orgs/1/alert-rules"));
    }

    /// One trailing slash names the same endpoint; the guard must not be what
    /// answers a caller who typed it. See `path_accepts_release`.
    #[test]
    fn one_trailing_slash_is_the_same_path() {
        assert!(path_accepts_release("/v1/apps/0b6a/issues/"));
        assert!(path_accepts_release("/v1/apps/0b6a/issues/77/events/"));
        // Stripping must not promote a path that was never on the list, and
        // must not swallow a second empty segment.
        assert!(!path_accepts_release("/v1/apps/0b6a/overview/"));
        assert!(!path_accepts_release("/v1/apps/0b6a/issues//"));
        // `/v1/apps/0b6a/issues/77/` strips to `/v1/apps/0b6a/issues/77`,
        // which is the issue DETAIL route — still not on the allowlist.
        assert!(!path_accepts_release("/v1/apps/0b6a/issues/77/"));
    }

    /// The guard is a read-side rule: only `GET` is subject to it.
    ///
    /// The regression this pins is `POST /v1/apps/{app_id}/artifacts
    /// ?…&release=…` — the source-map uploader, whose `release` is the
    /// artifact's own attribute (`routes/artifacts.rs`'s `UploadParams`) and
    /// which the guard used to answer with a 400 before the handler ever ran.
    /// The end-to-end half of this lives in
    /// `tests/http_release_scoping.rs::source_map_upload_with_release_is_not_guarded`.
    #[test]
    fn only_get_is_guarded() {
        assert!(method_is_guarded(&Method::GET));
        for m in [
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
            Method::HEAD,
            Method::OPTIONS,
        ] {
            assert!(
                !method_is_guarded(&m),
                "{m} must not be subject to the release filter guard"
            );
        }
    }

    /// Pins the exact 400 envelope this middleware emits.
    ///
    /// **This does not assert byte-equality against
    /// `ApiError::BadRequest(..).into_response()`** — this file cannot `use
    /// crate::error::ApiError` (see the module doc comment on why), so there
    /// is nothing in scope to compare against. `error.rs`'s `body()` helper
    /// was read by hand to produce the literal below
    /// (`{"error":{"code":"bad_request","message":…}}`); a real byte-for-byte
    /// parity check lives instead in
    /// `tests/http_release_scoping.rs::empty_release_is_a_400`, which drives
    /// the real spawned binary over HTTP and can therefore see both code
    /// paths' output at once.
    #[test]
    fn bad_request_body_matches_api_errors_envelope_shape() {
        assert_eq!(
            bad_request_body("release is not supported on this endpoint"),
            serde_json::json!({
                "error": {
                    "code": "bad_request",
                    "message": "release is not supported on this endpoint"
                }
            })
        );
    }
}
