//! The OpenAPI 3.1 document for `sauron-api`, and the tests that keep it honest.
//!
//! # Why the annotations live next to the handlers
//!
//! Schemas here are *derived* from the same `serde` types the handlers actually
//! return, so a response shape cannot drift from its documentation. The one
//! thing the derive cannot check is the `path`/`method` pair, which is a string
//! literal in each `#[utoipa::path]` and is not compared against `main.rs` by
//! anything in `utoipa`. [`router_parity`] closes that gap by parsing the real
//! router; see [`crate::route_table`].
//!
//! # Why the tests in this file are unit tests
//!
//! They deliberately do not live in `tests/http_*.rs`. That harness returns
//! `None` and reports a green pass in 0.00s when Postgres or Redis is
//! unreachable, which is the normal state of a sandboxed or laptop checkout. A
//! drift test that can silently skip is not a drift test — it is a green tick
//! that means nothing. Everything here runs on a bare `cargo test` with no
//! services at all.

use utoipa::openapi::security::{Http, HttpAuthScheme, SecurityScheme};
use utoipa::{Modify, OpenApi};

/// The uniform error envelope produced by every failing route.
///
/// Mirrors `crate::error::body`, which is the single place the API constructs an
/// error response. It is a documentation type rather than the wire type because
/// `ApiError` is an enum that renders through `IntoResponse` and never exists as
/// a serializable struct.
#[derive(serde::Serialize, utoipa::ToSchema)]
#[schema(as = ErrorResponse)]
pub struct ErrorResponse {
    pub error: ErrorBody,
}

#[derive(serde::Serialize, utoipa::ToSchema)]
pub struct ErrorBody {
    /// Machine-readable cause. One of `bad_request`, `forbidden`, `not_found`,
    /// `conflict`, `unprocessable`, `gone`, `rate_limited`, `internal`, or one
    /// of the specific codes carried by a 503 (for example
    /// `schema_behind_binary`), which name a fix an operator can act on.
    #[schema(example = "not_found")]
    pub code: String,
    /// Human-readable detail. Safe to show a user; never contains internals —
    /// a 500 always reports the constant string `internal error`.
    #[schema(example = "resource not found")]
    pub message: String,
}

/// The acknowledgement body shared by mutating routes that return no entity.
///
/// Sixteen handlers literally answer `{"ok": true}`. They are typed
/// `Json<serde_json::Value>` in Rust, so no schema can be derived from them;
/// this is the one place that shape is described, rather than sixteen inline
/// copies that would drift apart.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub struct OkResponse {
    #[schema(example = true)]
    pub ok: bool,
}

/// Attaches the security schemes.
///
/// Applied as a modifier rather than declared per-operation because the scheme
/// *definitions* are document-wide; which operations *require* them is stated
/// individually by each `#[utoipa::path(security(...))]`.
pub struct SecurityAddon;

impl Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi
            .components
            .get_or_insert_with(utoipa::openapi::Components::default);
        components.add_security_scheme(
            "bearerAuth",
            SecurityScheme::Http(
                Http::builder()
                    .scheme(HttpAuthScheme::Bearer)
                    .bearer_format("JWT")
                    .description(Some(
                        "Access token from `POST /v1/auth/login`, sent as \
                         `Authorization: Bearer <token>`. Access tokens are \
                         short-lived; use `POST /v1/auth/refresh` to rotate.",
                    ))
                    .build(),
            ),
        );
    }
}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Sauron API",
        description = "\
The JWT-authenticated dashboard and administration API for Sauron.

Every data route is scoped to the caller's org, project or app membership; a \
request for a resource outside the caller's grants is refused before the \
resource is looked up, so a 403 does not confirm that the id exists.

The ingest gateway that SDKs post telemetry to is a **separate service** with \
its own document — select \"Sauron Ingest\" above.",
        version = env!("CARGO_PKG_VERSION"),
        license(name = "AGPL-3.0-only"),
    ),
    modifiers(&SecurityAddon),
    tags(
        (name = "Health", description = "Liveness and readiness."),
        (name = "Auth", description = "Registration, login, token rotation and password management. The only unauthenticated routes in the API."),
        (name = "Account", description = "The calling user's own sessions and profile."),
        (name = "Apps", description = "Apps: the unit telemetry is attached to. Every analytics route is scoped to one."),
        (name = "Issues", description = "Grouped errors and their events. One row per occurrence — events are not de-duplicated."),
        (name = "Sessions", description = "User sessions reconstructed from telemetry."),
        (name = "Devices", description = "Devices and device groups seen by an app."),
        (name = "Performance", description = "Transactions, spans and operation timings. All durations are milliseconds."),
        (name = "Analytics", description = "Product analytics: events, overview, funnels, journeys, retention and active users."),
        (name = "Search", description = "The query DSL and the schema it accepts."),
        (name = "Stores", description = "App Store and Play Console connections, and the install metrics synced from them."),
        (name = "Monitors", description = "Uptime monitors, their check results and downtime incidents."),
        (name = "Inspector", description = "The privacy inspector: policies, scans, findings and irreversible masking. Findings are redacted by default; reveals are audited."),
        (name = "Notifications", description = "Alert rules, delivery channels and per-user subscriptions."),
        (name = "Organizations", description = "Organizations, members, grants and roles. The RBAC surface."),
        (name = "Projects", description = "Projects group apps within an organization."),
        (name = "Environments", description = "The environment catalogue and per-app enrollments. Enrollment ids are what analytics routes accept as environment_id."),
        (name = "Artifacts", description = "Source maps and debug artifacts used to symbolicate stack traces."),
        (name = "Admin", description = "Deployment-wide operations: storage, retention and cold-storage tiering. Org-owner only, and several are destructive."),
    ),
    paths(
        crate::health,
        crate::routes::auth::register,
        crate::routes::auth::login,
        crate::routes::auth::refresh,
        crate::routes::auth::logout,
        crate::routes::auth::forgot_password,
        crate::routes::auth::reset_password,
        crate::routes::auth::change_password,
        crate::routes::auth::me,
        crate::routes::account::list_sessions,
        crate::routes::account::revoke_session,
        crate::routes::account::revoke_other_sessions,
        crate::routes::apps::get_app,
        crate::routes::apps::update_app,
        crate::routes::apps::delete_app,
        crate::routes::apps::first_event,
        crate::routes::admin::storage,
        crate::routes::admin::get_tier_policy,
        crate::routes::admin::set_tier_policy,
        crate::routes::admin::set_session_retention,
        crate::routes::admin::create_restore,
        crate::routes::admin::list_restores,
        crate::routes::admin::get_restore,
        crate::routes::admin::release_pin,
        crate::routes::admin::extend_pin,
        crate::routes::projects::list_projects,
        crate::routes::projects::create_project,
        crate::routes::projects::get_project,
        crate::routes::projects::update_project,
        crate::routes::projects::delete_project,
        crate::routes::projects::list_apps,
        crate::routes::projects::create_app,
        crate::routes::environments::list_project_environments,
        crate::routes::environments::create_project_environment,
        crate::routes::environments::update_project_environment,
        crate::routes::environments::retire_project_environment,
        crate::routes::environments::list_app_environments,
        crate::routes::environments::update_app_environment,
        crate::routes::environments::rotate_app_environment_key,
        crate::routes::failures::list,
        crate::routes::failures::payloads,
        crate::routes::failures::retry,
        crate::routes::failures::drop_group,
        crate::routes::purge::preview,
        crate::routes::purge::confirm,
        crate::routes::purge::cancel,
        crate::routes::purge::get_job,
        crate::routes::purge::list_jobs,
        crate::routes::artifacts::upload,
        crate::routes::artifacts::list,
        crate::routes::artifacts::delete,
        crate::routes::orgs::list_orgs,
        crate::routes::orgs::create_org,
        crate::routes::orgs::access,
        crate::routes::orgs::list_members,
        crate::routes::orgs::create_member,
        crate::routes::orgs::set_member_active,
        crate::routes::orgs::revoke_member_sessions,
        crate::routes::orgs::reset_member_password,
        crate::routes::orgs::create_grant,
        crate::routes::orgs::update_grant_handler,
        crate::routes::orgs::delete_grant,
        crate::routes::orgs::list_roles,
        crate::routes::orgs::create_role,
        crate::routes::orgs::update_role_handler,
        crate::routes::orgs::delete_role_handler,
        crate::routes::issues::list,
        crate::routes::issues::detail,
        crate::routes::issues::update,
        crate::routes::issues::events,
        crate::routes::issues::event_stats,
        crate::routes::issues::stats,
        crate::routes::sessions::list,
        crate::routes::sessions::detail,
        crate::routes::transactions::list,
        crate::routes::devices::list,
        crate::routes::devices::groups,
        crate::routes::devices::count,
        crate::routes::devices::detail,
        crate::routes::performance::summary,
        crate::routes::performance::series,
        crate::routes::journeys::explore,
        crate::routes::search::schema,
        crate::routes::audit::list,
        crate::routes::audit::export_csv,
        crate::routes::active_users::active_users,
        crate::routes::active_users::active_users_csv,
        crate::routes::retention::grid,
        crate::routes::retention::lifecycle,
        crate::routes::retention::churn,
        crate::routes::screens::list,
        crate::routes::screens::count,
        crate::routes::screens::detail,
        crate::routes::screens::section_events,
        crate::routes::screens::section_exceptions,
        crate::routes::screens::section_devices,
        crate::routes::screens::section_users,
        crate::routes::workflows::list,
        crate::routes::workflows::count,
        crate::routes::workflows::detail,
        crate::routes::workflows::runs,
        crate::routes::workflows::session_spans,
        crate::routes::funnels::compute,
        crate::routes::funnels::list_saved,
        crate::routes::funnels::create_saved,
        crate::routes::funnels::update_saved,
        crate::routes::funnels::delete_saved,
        crate::routes::stores::list,
        crate::routes::stores::upsert,
        crate::routes::stores::delete,
        crate::routes::stores::queue_sync,
        crate::routes::stores::metrics,
        crate::routes::monitors::list,
        crate::routes::monitors::create,
        crate::routes::monitors::detail,
        crate::routes::monitors::update,
        crate::routes::monitors::delete,
        crate::routes::monitors::checks,
        crate::routes::monitors::incidents,
        crate::routes::analytics::overview,
        crate::routes::analytics::overview_totals,
        crate::routes::analytics::overview_series,
        crate::routes::analytics::overview_top_events,
        crate::routes::analytics::overview_top_issues,
        crate::routes::analytics::overview_refresh,
        crate::routes::analytics::overview_stream,
        crate::routes::analytics::events_list,
        crate::routes::analytics::top_events,
        crate::routes::analytics::event_series,
        crate::routes::analytics::event_timeseries,
        crate::routes::analytics::error_timeseries,
        crate::routes::analytics::transaction_timeseries,
        crate::routes::analytics::active_users_series,
        crate::routes::analytics::persons_list,
        crate::routes::analytics::persons_count,
        crate::routes::analytics::person,
        crate::routes::analytics::users_summary,
        crate::routes::analytics::sessions_summary,
        crate::routes::analytics::rollups_status,
        crate::routes::analytics::rollups_refresh,
        crate::routes::inspector::list_policies,
        crate::routes::inspector::create_policy,
        crate::routes::inspector::get_policy,
        crate::routes::inspector::patch_policy,
        crate::routes::inspector::delete_policy,
        crate::routes::inspector::list_scans,
        crate::routes::inspector::start_scan,
        crate::routes::inspector::get_scan,
        crate::routes::inspector::cancel_scan,
        crate::routes::inspector::list_findings,
        crate::routes::inspector::reveal_finding,
        crate::routes::inspector::effective_policy,
        crate::routes::inspector::list_app_masked_keys,
        crate::routes::inspector::mask_preview,
        crate::routes::inspector::list_app_mask_actions,
        crate::routes::inspector::list_org_mask_actions,
        crate::routes::inspector::get_mask_action_handler,
        crate::routes::inspector::confirm_mask,
        crate::routes::inspector::cancel_mask,
        crate::routes::notifications::meta,
        crate::routes::notifications::list_channels,
        crate::routes::notifications::create_channel,
        crate::routes::notifications::get_channel,
        crate::routes::notifications::update_channel,
        crate::routes::notifications::delete_channel,
        crate::routes::notifications::test_channel,
        crate::routes::notifications::list_rules,
        crate::routes::notifications::create_rule,
        crate::routes::notifications::get_rule,
        crate::routes::notifications::update_rule,
        crate::routes::notifications::delete_rule,
        crate::routes::notifications::list_history,
        crate::routes::notification_prefs::list_notifications,
        crate::routes::notification_prefs::list_subscriptions,
        crate::routes::notification_prefs::create_subscription,
        crate::routes::notification_prefs::patch_subscription,
        crate::routes::notification_prefs::delete_subscription_route,
        crate::routes::notification_prefs::unsubscribe,
    ),
    components(schemas(ErrorResponse, ErrorBody, OkResponse)),
)]
pub struct ApiDoc;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// The tests below work against the **serialized** document rather than
    /// utoipa's typed structs.
    ///
    /// Two reasons. It is the artifact clients actually consume, so asserting on
    /// it tests the published thing rather than the builder that produced it.
    /// And `SecurityRequirement`'s contents are private, so "does this operation
    /// require auth" is not answerable through the typed API at all.
    fn document() -> serde_json::Value {
        serde_json::to_value(ApiDoc::openapi()).expect("document should serialize")
    }

    const METHODS: &[&str] = &["get", "put", "post", "delete", "options", "head", "patch"];

    /// Every `(METHOD, path)` pair the document describes.
    fn documented_operations(doc: &serde_json::Value) -> BTreeSet<(String, String)> {
        let mut out = BTreeSet::new();
        let paths = doc["paths"]
            .as_object()
            .expect("document should have paths");
        for (path, item) in paths {
            for method in METHODS {
                if item.get(*method).is_some() {
                    out.insert((method.to_uppercase(), path.clone()));
                }
            }
        }
        out
    }

    /// The document must describe exactly the routes the router serves.
    ///
    /// A `#[utoipa::path]` annotation carries its method and path as string
    /// literals that nothing compares against `main.rs`. Without this test,
    /// renaming a route leaves the document advertising the old one, and the
    /// failure is invisible until a client 404s against published documentation.
    #[test]
    fn router_parity() {
        let router = crate::route_table::registered_operations();
        let documented = documented_operations(&document());

        let undocumented: Vec<_> = router.difference(&documented).collect();
        let phantom: Vec<_> = documented.difference(&router).collect();

        assert!(
            undocumented.is_empty() && phantom.is_empty(),
            "the OpenAPI document and the router in `main.rs` disagree.\n\
             \n\
             In the router but NOT documented ({}):\n{:#?}\n\
             \n\
             Documented but NOT in the router ({}):\n{:#?}\n\
             \n\
             Add or correct the `#[utoipa::path]` annotation and its entry in \
             `ApiDoc`'s `paths(...)` list.",
            undocumented.len(),
            undocumented,
            phantom.len(),
            phantom,
        );
    }

    /// Only these operations may be served without credentials.
    ///
    /// Hardcoded rather than derived: the point is that adding an
    /// unauthenticated endpoint must be a deliberate edit to this list, and
    /// cannot happen by forgetting a `security(...)` clause.
    const PUBLIC_OPERATIONS: &[(&str, &str)] = &[
        ("GET", "/health"),
        ("POST", "/v1/auth/register"),
        ("POST", "/v1/auth/login"),
        ("POST", "/v1/auth/refresh"),
        ("POST", "/v1/auth/logout"),
        ("POST", "/v1/auth/forgot-password"),
        ("POST", "/v1/auth/reset-password"),
        ("POST", "/v1/notifications/unsubscribe"),
    ];

    /// An operation is public when it states no security requirement at all, or
    /// states an empty one. There is no document-level `security`, so an
    /// operation that simply forgot the clause is genuinely public — which is
    /// exactly the mistake this test exists to catch.
    fn requires_authentication(op: &serde_json::Value) -> bool {
        match op.get("security") {
            None => false,
            Some(serde_json::Value::Array(reqs)) => reqs.iter().any(|r| {
                r.as_object()
                    .is_some_and(|scheme_map| !scheme_map.is_empty())
            }),
            Some(_) => false,
        }
    }

    #[test]
    fn only_the_allowlisted_operations_are_public() {
        let doc = document();
        let documented = documented_operations(&doc);

        let mut actually_public = BTreeSet::new();
        let paths = doc["paths"].as_object().unwrap();
        for (path, item) in paths {
            for method in METHODS {
                if let Some(op) = item.get(*method) {
                    if !requires_authentication(op) {
                        actually_public.insert((method.to_uppercase(), path.clone()));
                    }
                }
            }
        }

        // Intersected with what the document currently contains, so the test is
        // meaningful while `paths(...)` is still being filled in rather than
        // failing over routes that are not annotated yet. Once `router_parity`
        // passes, `documented` is the whole API and this is the full check.
        let allowed: BTreeSet<(String, String)> = PUBLIC_OPERATIONS
            .iter()
            .map(|(m, p)| (m.to_string(), p.to_string()))
            .collect();
        let expected: BTreeSet<_> = allowed.intersection(&documented).cloned().collect();

        assert_eq!(
            actually_public, expected,
            "the set of operations served without authentication changed.\n\
             If you added a genuinely public endpoint, add it to \
             `PUBLIC_OPERATIONS`. If you did not, an operation is missing its \
             `security((\"bearerAuth\" = []))` clause and is published as \
             requiring no credentials."
        );
    }

    /// utoipa emits a `$ref` for any type named in a response whether or not
    /// that type was registered as a schema. A dangling reference renders in
    /// Swagger UI as an empty box, with no error anywhere.
    #[test]
    fn every_schema_reference_resolves() {
        let doc = document();

        let defined: BTreeSet<String> = doc
            .pointer("/components/schemas")
            .and_then(|s| s.as_object())
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();

        let mut referenced = BTreeSet::new();
        collect_refs(&doc, &mut referenced);

        let dangling: Vec<_> = referenced.difference(&defined).collect();
        assert!(
            dangling.is_empty(),
            "the document references schemas that are not defined in \
             `components`: {dangling:#?}\n\
             Add the type to `components(schemas(...))`, or give it \
             `#[derive(ToSchema)]` so it is collected transitively."
        );
    }

    fn collect_refs(value: &serde_json::Value, out: &mut BTreeSet<String>) {
        match value {
            serde_json::Value::Object(map) => {
                for (k, v) in map {
                    if k == "$ref" {
                        if let Some(name) = v
                            .as_str()
                            .and_then(|r| r.strip_prefix("#/components/schemas/"))
                        {
                            out.insert(name.to_string());
                        }
                    }
                    collect_refs(v, out);
                }
            }
            serde_json::Value::Array(items) => items.iter().for_each(|i| collect_refs(i, out)),
            _ => {}
        }
    }

    /// The security scheme the whole document depends on must actually exist.
    /// Every authenticated operation references `bearerAuth` by name; if the
    /// modifier stopped running, those references would silently mean nothing.
    #[test]
    fn the_bearer_scheme_is_defined() {
        let doc = document();
        let scheme = doc
            .pointer("/components/securitySchemes/bearerAuth")
            .expect("bearerAuth must be defined; SecurityAddon did not run");
        assert_eq!(scheme["type"], "http");
        assert_eq!(scheme["scheme"], "bearer");
    }
}

#[cfg(test)]
mod dump {
    /// Writes the document to `$SAURON_OPENAPI_DUMP` when that variable is set.
    /// Ignored by default; a review aid, not a gate.
    #[test]
    #[ignore]
    fn dump_document() {
        let path = std::env::var("SAURON_OPENAPI_DUMP").expect("set SAURON_OPENAPI_DUMP");
        let doc = <super::ApiDoc as utoipa::OpenApi>::openapi();
        std::fs::write(&path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
        eprintln!("wrote {path}");
    }
}
