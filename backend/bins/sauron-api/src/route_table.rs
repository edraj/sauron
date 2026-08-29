//! Reads the real route table out of `main.rs`'s literal source.
//!
//! Test-only. Exists so [`crate::openapi`]'s parity test can compare the
//! OpenAPI document against the routes the binary actually serves, rather than
//! against a hand-maintained list of them — a second list is precisely the
//! thing that drifts.
//!
//! # Why parse source instead of asking the router
//!
//! `axum::Router` exposes no way to enumerate its registered routes, so the
//! literal source is the only available ground truth short of restructuring the
//! router itself. Parsing is narrow: `main.rs` writes every route as
//! `.route("<path>", method(handler)...)` on one shape, and the balanced-paren
//! scan below is exactly as much parsing as that shape needs. No `regex`
//! dependency.
//!
//! # Shared with `tests/http_env_scoping.rs`
//!
//! That file used to carry two near-identical copies of this scanner, for the
//! app-scoped and project-scoped `GET` questions. It now includes this module
//! directly:
//!
//! ```ignore
//! #[path = "../src/route_table.rs"]
//! mod route_table;
//! ```
//!
//! An integration test cannot `use` a binary crate, so `#[path]` is how the two
//! share one parser rather than drifting. `include_str!("main.rs")` below
//! resolves relative to *this* file, so it finds the router from either
//! compilation.
//!
//! The consolidation was verified by diffing both scanners' output against the
//! originals, not by observing that the tests still pass — those tests skip
//! silently without Postgres and Redis, so green alone proves nothing.

use std::collections::BTreeSet;

/// The literal source of the router. `include_str!` rather than a runtime read
/// so a moved or renamed `main.rs` is a compile error, not a test that starts
/// finding zero routes.
const MAIN_RS: &str = include_str!("main.rs");

/// Lower bound on the number of routes the scan must find.
///
/// Without this, a `main.rs` whose `.route(...)` shape changed would yield an
/// empty set, and the parity test would compare nothing against nothing and
/// pass. The floor is deliberately below the current count so ordinary route
/// additions do not trip it; it only catches the parser silently dying.
const MINIMUM_EXPECTED_ROUTES: usize = 130;

/// HTTP method constructors `main.rs` uses inside `.route(...)`.
const METHOD_FNS: &[(&str, &str)] = &[
    ("get(", "GET"),
    ("post(", "POST"),
    ("put(", "PUT"),
    ("patch(", "PATCH"),
    ("delete(", "DELETE"),
];

/// Every `(METHOD, path)` pair registered in `main.rs`.
///
/// Covers the main router chain and the separately-merged `artifact_routes`
/// block alike, because both are written in the same file with the same shape.
pub fn registered_operations() -> BTreeSet<(String, String)> {
    let ops = scan(MAIN_RS);
    assert!(
        ops.len() >= MINIMUM_EXPECTED_ROUTES,
        "route_table: found only {} routes in main.rs, expected at least {}. \
         The `.route(\"path\", method(handler))` shape this scanner depends on \
         has probably changed. Fix the scanner — an empty result would make \
         `openapi::tests::router_parity` pass while comparing nothing.",
        ops.len(),
        MINIMUM_EXPECTED_ROUTES,
    );
    ops
}

/// Every app-scoped path that registers a `GET`, deduplicated and sorted.
///
/// "App-scoped" means the bare `/v1/apps/{app_id}` route or anything beneath
/// it — the set `routes::scope`'s `environment_id` rules apply to.
///
/// `#[allow(dead_code)]`: this file is compiled twice — once as a module of the
/// binary's test build (where only `registered_operations` is used) and once
/// via `#[path]` from `tests/http_env_scoping.rs` (where these are). Each
/// compilation sees items the other does not use.
#[allow(dead_code)]
pub fn app_scoped_get_paths() -> Vec<String> {
    scoped_get_paths("/v1/apps/{app_id}")
}

/// The project-scoped twin of [`app_scoped_get_paths`].
#[allow(dead_code)]
pub fn project_scoped_get_paths() -> Vec<String> {
    scoped_get_paths("/v1/projects/{project_id}")
}

/// Paths registering a `GET` that are `prefix` itself or sit beneath it.
#[allow(dead_code)]
fn scoped_get_paths(prefix: &str) -> Vec<String> {
    let nested = format!("{prefix}/");
    let mut out: Vec<String> = registered_operations()
        .into_iter()
        .filter(|(method, path)| method == "GET" && (path == prefix || path.starts_with(&nested)))
        .map(|(_, path)| path)
        .collect();
    out.sort();
    out.dedup();
    out
}

fn scan(src: &str) -> BTreeSet<(String, String)> {
    let bytes = src.as_bytes();
    let mut out = BTreeSet::new();
    let marker = ".route(";
    let mut from = 0usize;

    while let Some(rel) = src[from..].find(marker) {
        let open = from + rel + marker.len() - 1;
        let close = match matching_paren(bytes, open) {
            Some(c) => c,
            None => panic!("route_table: unbalanced parens in a .route(...) call at byte {open}"),
        };
        let args = &src[open + 1..close];

        if let Some(path) = first_string_literal(args) {
            for (needle, method) in METHOD_FNS {
                if contains_call(args, needle) {
                    out.insert((method.to_string(), path.clone()));
                }
            }
        }
        from = close + 1;
    }
    out
}

fn matching_paren(bytes: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0i32;
    for (i, b) in bytes.iter().enumerate().skip(open) {
        match b {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

fn first_string_literal(args: &str) -> Option<String> {
    let q1 = args.find('"')?;
    let rest = &args[q1 + 1..];
    let q2 = rest.find('"')?;
    Some(rest[..q2].to_string())
}

/// Whether `args` calls `needle` (e.g. `get(`) as a function rather than merely
/// containing those characters — `.route("/x", get(routes::widget::get_all))`
/// must not be read as registering a second `get`.
fn contains_call(args: &str, needle: &str) -> bool {
    let bytes = args.as_bytes();
    let n = needle.as_bytes();
    (0..bytes.len().saturating_sub(n.len() - 1))
        .any(|i| &bytes[i..i + n.len()] == n && (i == 0 || !is_ident_byte(bytes[i - 1])))
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_single_route() {
        let ops = scan(r#".route("/health", get(health))"#);
        assert_eq!(
            ops.iter().cloned().collect::<Vec<_>>(),
            vec![("GET".to_string(), "/health".to_string())]
        );
    }

    #[test]
    fn parses_multiple_methods_on_one_path() {
        let ops = scan(r#".route("/v1/orgs", get(list_orgs).post(create_org))"#);
        assert!(ops.contains(&("GET".into(), "/v1/orgs".into())));
        assert!(ops.contains(&("POST".into(), "/v1/orgs".into())));
        assert_eq!(ops.len(), 2);
    }

    /// A handler *named* `get_all` must not be mistaken for a `get(` registration.
    #[test]
    fn a_handler_name_containing_a_method_word_is_not_a_registration() {
        let ops = scan(r#".route("/x", post(routes::widget::get_all))"#);
        assert_eq!(
            ops.iter().cloned().collect::<Vec<_>>(),
            vec![("POST".to_string(), "/x".to_string())]
        );
    }

    #[test]
    fn handles_the_multiline_shape_main_rs_actually_uses() {
        let ops = scan(
            r#".route(
                "/v1/apps/{app_id}/issues/{issue_id}",
                get(routes::issues::detail).patch(routes::issues::update),
            )"#,
        );
        assert!(ops.contains(&("GET".into(), "/v1/apps/{app_id}/issues/{issue_id}".into())));
        assert!(ops.contains(&("PATCH".into(), "/v1/apps/{app_id}/issues/{issue_id}".into())));
    }

    /// The scan of the real file must find the whole table, not a prefix of it.
    #[test]
    fn the_real_main_rs_parses_to_a_plausible_table() {
        let ops = registered_operations();
        assert!(ops.contains(&("GET".into(), "/health".into())));
        assert!(ops.contains(&("POST".into(), "/v1/auth/login".into())));
        // From the separately-merged `artifact_routes` block, which carries its
        // own body limit and is easy to miss when scanning only the main chain.
        assert!(
            ops.contains(&("POST".into(), "/v1/apps/{app_id}/artifacts".into())),
            "the artifact_routes block was not scanned; found: {ops:#?}"
        );
    }
}
