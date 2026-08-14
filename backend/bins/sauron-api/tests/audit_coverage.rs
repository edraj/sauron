//! The Wall of Shame's anti-drift guard.
//!
//! The failure mode this defends against is not a bug in the audit log — it is
//! somebody adding a mutating endpoint six months from now and never wiring it
//! up. The feature silently stops being complete, every other test still
//! passes, and nobody finds out until an incident review turns up nothing.
//!
//! So: every handler mounted behind `post`/`put`/`patch`/`delete` in `main.rs`
//! must appear in exactly one of the two lists below. Adding a route without
//! classifying it fails the build.
//!
//! This is a SOURCE-level test because `axum::Router` does not expose its
//! routing table at runtime; there is nothing to enumerate at test time but the
//! text that built it.

/// The router source. Any new mutating route lands here first.
const MAIN_RS: &str = include_str!("../src/main.rs");

/// Handlers that call `audit::record` (or `record_all_orgs`) on their success
/// path. Verified against the source of the handler itself by
/// `audited_handlers_actually_call_record`.
const AUDITED: &[&str] = &[
    "routes::admin::create_restore",
    "routes::admin::extend_pin",
    "routes::admin::release_pin",
    "routes::admin::set_tier_policy",
    // All three purge transitions are audited. `preview` destroys nothing but
    // is where the scope is chosen and frozen, so it is the only record of
    // what was ASKED for as distinct from what confirm executed.
    "routes::purge::preview",
    "routes::purge::confirm",
    "routes::purge::cancel",
    "routes::auth::change_password",
    "routes::auth::login",
    "routes::auth::logout",
    "routes::apps::delete_app",
    // Both write via `record_all_orgs`: a failure group may have no org_id at
    // all (the dominant failure never decoded), and the drop is a hard DELETE,
    // so its entry is the only surviving record that the events existed.
    "routes::failures::drop_group",
    "routes::failures::retry",
    "routes::apps::update_app",
    "routes::artifacts::delete",
    "routes::artifacts::upload",
    "routes::environments::create_project_environment",
    "routes::environments::retire_project_environment",
    "routes::environments::rotate_app_environment_key",
    "routes::environments::update_app_environment",
    "routes::environments::update_project_environment",
    "routes::inspector::create_policy",
    "routes::inspector::delete_policy",
    "routes::inspector::patch_policy",
    "routes::monitors::create",
    "routes::monitors::delete",
    "routes::monitors::update",
    "routes::notifications::create_channel",
    "routes::notifications::create_rule",
    "routes::notifications::delete_channel",
    "routes::notifications::delete_rule",
    "routes::notifications::test_channel",
    "routes::notifications::update_channel",
    "routes::notifications::update_rule",
    "routes::orgs::create_grant",
    "routes::orgs::create_member",
    "routes::orgs::create_org",
    "routes::orgs::create_role",
    "routes::orgs::delete_grant",
    "routes::orgs::delete_role_handler",
    "routes::orgs::reset_member_password",
    "routes::orgs::revoke_member_sessions",
    "routes::orgs::set_member_active",
    "routes::orgs::update_grant_handler",
    "routes::orgs::update_role_handler",
    "routes::projects::create_app",
    "routes::projects::create_project",
    "routes::projects::delete_project",
    "routes::projects::update_project",
    "routes::stores::delete",
    "routes::stores::queue_sync",
    "routes::stores::upsert",
];

/// Handlers deliberately NOT audited, each with the reason.
///
/// A reason per entry, so this stays reviewable instead of becoming the place
/// inconvenient routes go to be forgotten. If you are adding a line here, the
/// reason is the part that matters.
const EXEMPT: &[(&str, &str)] = &[
    // --- Self-service, not administration -----------------------------------
    (
        "routes::account::revoke_session",
        "the caller ending their own session; the Wall records what admins do TO others",
    ),
    (
        "routes::account::revoke_other_sessions",
        "same: self-service session hygiene, already visible on the Sessions page",
    ),
    // --- Auth events (locked decision 1) ------------------------------------
    (
        "routes::auth::register",
        "self-signup. The org it creates is recorded by org.create; the account \
      itself has no org to file under until that grant exists",
    ),
    (
        "routes::auth::refresh",
        "fires on every token rotation — minutes apart per active session. Recording \
      it would swamp even the opt-in auth stream while adding nothing login and \
      logout do not already show",
    ),
    (
        "routes::auth::forgot_password",
        "unauthenticated — there is no actor to attribute, and recording the email \
      would turn the trail into an account-enumeration oracle",
    ),
    (
        "routes::auth::reset_password",
        "unauthenticated (bearer is the reset token); the ADMIN half of a reset is \
      recorded by orgs::reset_member_password",
    ),
    // --- Product data, not configuration (locked decision 1) ----------------
    (
        "routes::issues::update",
        "issue triage: product data, high volume, decision 1",
    ),
    (
        "routes::funnels::compute",
        "read-only analysis; POST only because the query is a body",
    ),
    (
        "routes::funnels::create_saved",
        "saved analysis view: product data, decision 1",
    ),
    (
        "routes::funnels::update_saved",
        "saved analysis view: product data, decision 1",
    ),
    (
        "routes::funnels::delete_saved",
        "saved analysis view: product data, decision 1",
    ),
    // --- Personal preferences ------------------------------------------------
    (
        "routes::notification_prefs::create_subscription",
        "the caller's own notification prefs",
    ),
    (
        "routes::notification_prefs::patch_subscription",
        "the caller's own notification prefs",
    ),
    (
        "routes::notification_prefs::delete_subscription_route",
        "the caller's own notification prefs",
    ),
    (
        "routes::notification_prefs::unsubscribe",
        "unauthenticated one-click unsubscribe from a mailed token; no actor to attribute",
    ),
    // --- Already audited by their own tables (locked decision 10) -----------
    (
        "routes::inspector::reveal_finding",
        "writes inspector_reveal_audit, which the Wall unions in at read time; \
      double-writing would let the two copies drift",
    ),
    (
        "routes::inspector::mask_preview",
        "writes inspector_mask_actions; unioned in at read time",
    ),
    (
        "routes::inspector::confirm_mask",
        "writes inspector_mask_actions; unioned in at read time",
    ),
    (
        "routes::inspector::cancel_mask",
        "writes inspector_mask_actions; unioned in at read time",
    ),
    (
        "routes::inspector::start_scan",
        "scan lifecycle is recorded in inspector_scans and shown on the Privacy page",
    ),
    ("routes::inspector::cancel_scan", "scan lifecycle, as above"),
];

/// Every `routes::module::handler` mounted behind a mutating method.
fn mutating_handlers(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    for method in ["post(", "put(", "patch(", "delete("] {
        let mut rest = src;
        while let Some(pos) = rest.find(method) {
            rest = &rest[pos + method.len()..];
            // `axum::routing::delete(...)` and `delete(...)` both land here; the
            // captured text starts right after the paren either way.
            let path: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == ':' || *c == '_')
                .collect();
            if path.starts_with("routes::") && path.matches("::").count() == 2 {
                out.push(path);
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

#[test]
fn every_mutating_route_is_audited_or_explicitly_exempt() {
    let found = mutating_handlers(MAIN_RS);
    assert!(
        found.len() > 50,
        "extracted only {} mutating handlers — the parser has drifted from \
         main.rs's syntax and is no longer checking anything",
        found.len()
    );

    let exempt: Vec<&str> = EXEMPT.iter().map(|(h, _)| *h).collect();
    let unclassified: Vec<&String> = found
        .iter()
        .filter(|h| !AUDITED.contains(&h.as_str()) && !exempt.contains(&h.as_str()))
        .collect();

    assert!(
        unclassified.is_empty(),
        "these mutating routes are in neither AUDITED nor EXEMPT: {unclassified:#?}\n\n\
         Add an `audit::record` call on the handler's success path and list it in \
         AUDITED, or add it to EXEMPT with the reason it does not belong on the \
         Wall of Shame. Do not add it to EXEMPT just to make this pass."
    );
}

#[test]
fn no_handler_is_both_audited_and_exempt() {
    for (handler, _) in EXEMPT {
        assert!(
            !AUDITED.contains(handler),
            "{handler} is in both AUDITED and EXEMPT"
        );
    }
}

#[test]
fn every_listed_handler_is_actually_mounted() {
    // Catches the reverse drift: a handler renamed or a route deleted, leaving
    // a stale entry that makes coverage look broader than it is.
    let found = mutating_handlers(MAIN_RS);
    for h in AUDITED {
        assert!(
            found.contains(&h.to_string()),
            "AUDITED lists {h}, which is not mounted behind any mutating method \
             in main.rs — it was renamed or its route was removed"
        );
    }
    for (h, _) in EXEMPT {
        assert!(
            found.contains(&h.to_string()),
            "EXEMPT lists {h}, which is not mounted behind any mutating method \
             in main.rs — it was renamed or its route was removed"
        );
    }
}

#[test]
fn every_exemption_carries_a_real_reason() {
    for (handler, reason) in EXEMPT {
        assert!(
            reason.len() > 20,
            "{handler}'s exemption reason is too short to be a reason: {reason:?}"
        );
    }
}

/// Every route module that AUDITED names, paired with its source.
///
/// `include_str!` needs a literal path, so this list is hand-maintained — and
/// that is precisely why `every_audited_module_has_a_source` below exists. A
/// module added to AUDITED but forgotten here would be skipped silently by
/// `audited_handlers_actually_call_record`, which would then report green while
/// verifying nothing about it. That already happened once: `routes::failures`
/// was added to AUDITED by a later feature and went unchecked until this guard
/// was added.
const SOURCES: &[(&str, &str)] = &[
    ("routes::admin", include_str!("../src/routes/admin.rs")),
    ("routes::apps", include_str!("../src/routes/apps.rs")),
    (
        "routes::artifacts",
        include_str!("../src/routes/artifacts.rs"),
    ),
    ("routes::auth", include_str!("../src/routes/auth.rs")),
    (
        "routes::environments",
        include_str!("../src/routes/environments.rs"),
    ),
    (
        "routes::failures",
        include_str!("../src/routes/failures.rs"),
    ),
    (
        "routes::inspector",
        include_str!("../src/routes/inspector.rs"),
    ),
    (
        "routes::monitors",
        include_str!("../src/routes/monitors.rs"),
    ),
    (
        "routes::notifications",
        include_str!("../src/routes/notifications.rs"),
    ),
    ("routes::orgs", include_str!("../src/routes/orgs.rs")),
    (
        "routes::projects",
        include_str!("../src/routes/projects.rs"),
    ),
    ("routes::purge", include_str!("../src/routes/purge.rs")),
    ("routes::stores", include_str!("../src/routes/stores.rs")),
];

/// The module half of a `routes::module::handler` path.
fn module_of(handler: &str) -> String {
    handler
        .rsplit_once("::")
        .map(|(m, _)| m)
        .unwrap_or(handler)
        .to_string()
}

/// Closes the hole that lets `audited_handlers_actually_call_record` skip a
/// module entirely. Without this, forgetting a `SOURCES` line turns that
/// module's coverage claim into an unverified assertion.
#[test]
fn every_audited_module_has_a_source() {
    for handler in AUDITED {
        let module = module_of(handler);
        assert!(
            SOURCES.iter().any(|(m, _)| *m == module),
            "{handler} is listed in AUDITED but {module} has no entry in SOURCES, \
             so nothing verifies that it actually calls `audit::record`. Add \
             `({module:?}, include_str!(\"../src/{}.rs\"))` to SOURCES.",
            module.replace("routes::", "routes/")
        );
    }
}

/// Calls that count as recording.
///
/// `audit::record` and `audit::record_all_orgs` are the direct forms.
/// `record_auth(` is `routes::auth`'s own wrapper: auth events are written
/// DETACHED (see its doc comment — awaiting them would reintroduce a
/// login-timing enumeration oracle), so the handlers call the wrapper rather
/// than `audit::record` directly.
const RECORDING_CALLS: &[&str] = &["audit::record", "record_auth("];

/// The body of `pub async fn {name}`, from its signature to the next
/// top-level closing brace.
///
/// Crude, and adequate: this file is rustfmt'd, so a `}` in column zero ends an
/// item and nothing else. `every_audited_handler_records` asserts it actually
/// found each body, so a formatting change that broke this parser would fail
/// loudly rather than silently matching nothing.
fn handler_body<'a>(src: &'a str, name: &str) -> Option<&'a str> {
    let sig = format!("pub async fn {name}(");
    let start = src.find(&sig)?;
    let rest = &src[start..];
    let end = rest.find("\n}\n").map(|i| i + 3).unwrap_or(rest.len());
    Some(&rest[..end])
}

/// The list could still lie: a handler could be named in AUDITED without any
/// recording call at all. Check EACH handler's own body.
///
/// Per handler, not per module. The previous version counted `audit::record`
/// occurrences per file and asserted `count >= handlers`, which passed happily
/// when five call sites all sat inside one handler and the other four recorded
/// nothing — and broke honestly the moment a module recorded through a shared
/// wrapper (`routes::auth`, three handlers, one call site).
#[test]
fn every_audited_handler_records() {
    for handler in AUDITED {
        let module = module_of(handler);
        let name = handler.rsplit_once("::").map(|(_, n)| n).unwrap_or(handler);
        let src = SOURCES
            .iter()
            .find(|(m, _)| *m == module)
            .map(|(_, src)| *src)
            .unwrap_or_else(|| panic!("{module} missing from SOURCES"));

        let body = handler_body(src, name).unwrap_or_else(|| {
            panic!(
                "could not find `pub async fn {name}` in {module} — it was renamed, \
                 or this file's crude body parser has drifted from the source layout"
            )
        });

        assert!(
            RECORDING_CALLS.iter().any(|c| body.contains(c)),
            "{handler} is listed in AUDITED but its body contains no recording \
             call ({RECORDING_CALLS:?}). Either instrument it, or move it to \
             EXEMPT with the reason it does not belong on the Wall of Shame."
        );
    }
}
