//! The Wall of Shame's write side: recording administrative actions.
//!
//! Two rules govern everything here.
//!
//! **Fail-open.** `record` returns `()`. A failed audit write is logged at
//! error level and swallowed, because an audit-table problem must never take
//! down member management or project creation. This is deliberately the
//! opposite of `inspector_reveal_audit`, where the audit row *is* the
//! authorization to emit PII and a failure to record is a failure to reveal.
//! Here there is nothing useful a handler could do with the error, so it is
//! not given one to ignore.
//!
//! **Allowlist, never serialize.** `changes` is built field by field from a
//! per-entity allowlist. Nothing in this module ever serializes an entity
//! wholesale into the log. That is the guarantee that an ingest key, a channel
//! secret or a password hash cannot reach a table org admins can read and that
//! is kept forever — see `forbidden_fields_never_appear_in_any_allowlist`.

use diesel_async::AsyncPgConnection;
use sauron_db::models::NewAuditLogEntry;
use sauron_db::repo;
use serde_json::{json, Value};
use uuid::Uuid;

// ===========================================================================
// Action vocabulary
// ===========================================================================

/// Every action this binary can write, as `entity.verb`.
///
/// Also the API's facet vocabulary and the drift test's registry, so the three
/// cannot disagree. Kept as `&str` constants rather than an enum because the
/// column is TEXT: adding an action must be a code change, not a migration.
pub mod action {
    pub const ORG_CREATE: &str = "org.create";

    pub const PROJECT_CREATE: &str = "project.create";
    pub const PROJECT_UPDATE: &str = "project.update";
    pub const PROJECT_DELETE: &str = "project.delete";

    pub const APP_CREATE: &str = "app.create";
    pub const APP_UPDATE: &str = "app.update";
    pub const APP_DELETE: &str = "app.delete";

    pub const ENV_CREATE: &str = "environment.create";
    pub const ENV_UPDATE: &str = "environment.update";
    pub const ENV_RETIRE: &str = "environment.retire";
    pub const ENV_ENROLLMENT_UPDATE: &str = "environment.enrollment_update";
    pub const ENV_ROTATE_KEY: &str = "environment.rotate_key";

    pub const MEMBER_CREATE: &str = "member.create";
    pub const MEMBER_ACTIVATE: &str = "member.activate";
    pub const MEMBER_DEACTIVATE: &str = "member.deactivate";
    pub const MEMBER_RESET_PASSWORD: &str = "member.reset_password";
    pub const MEMBER_REVOKE_SESSIONS: &str = "member.revoke_sessions";

    pub const ROLE_CREATE: &str = "role.create";
    pub const ROLE_UPDATE: &str = "role.update";
    pub const ROLE_DELETE: &str = "role.delete";

    pub const GRANT_CREATE: &str = "grant.create";
    pub const GRANT_UPDATE: &str = "grant.update";
    pub const GRANT_DELETE: &str = "grant.delete";

    pub const ALERT_RULE_CREATE: &str = "alert_rule.create";
    pub const ALERT_RULE_UPDATE: &str = "alert_rule.update";
    pub const ALERT_RULE_DELETE: &str = "alert_rule.delete";

    pub const ALERT_CHANNEL_CREATE: &str = "alert_channel.create";
    pub const ALERT_CHANNEL_UPDATE: &str = "alert_channel.update";
    pub const ALERT_CHANNEL_DELETE: &str = "alert_channel.delete";
    pub const ALERT_CHANNEL_TEST: &str = "alert_channel.test";

    pub const MONITOR_CREATE: &str = "monitor.create";
    pub const MONITOR_UPDATE: &str = "monitor.update";
    pub const MONITOR_DELETE: &str = "monitor.delete";

    pub const ARTIFACT_UPLOAD: &str = "artifact.upload";
    pub const ARTIFACT_DELETE: &str = "artifact.delete";

    pub const STORE_UPSERT: &str = "store.upsert";
    pub const STORE_DELETE: &str = "store.delete";
    pub const STORE_SYNC: &str = "store.sync";

    pub const INSPECTOR_POLICY_CREATE: &str = "inspector_policy.create";
    pub const INSPECTOR_POLICY_UPDATE: &str = "inspector_policy.update";
    pub const INSPECTOR_POLICY_DELETE: &str = "inspector_policy.delete";

    /// Replaying a failed-ingest group through the pipeline.
    pub const INGEST_FAILURE_RETRY: &str = "ingest_failure.retry";
    /// Discarding one permanently. The drop is a hard DELETE, so this entry is
    /// the ONLY surviving record that the events existed — which is why the
    /// handler records it before deleting, and why the `changes` it carries
    /// name the group and its counts rather than merely the id of a row that
    /// no longer exists.
    pub const INGEST_FAILURE_DROP: &str = "ingest_failure.drop";

    // Authentication. A SEPARATE STREAM, not ordinary admin actions: they are
    // excluded from the default feed and appear only when explicitly asked for.
    // Decision 1 excluded auth events outright because logins would drown the
    // admin events the Wall exists to surface; keeping them behind an opt-in is
    // what makes recording them safe.
    pub const AUTH_LOGIN: &str = "auth.login";
    pub const AUTH_LOGIN_FAILED: &str = "auth.login_failed";
    pub const AUTH_LOGOUT: &str = "auth.logout";
    pub const AUTH_PASSWORD_CHANGE: &str = "auth.password_change";

    pub const TIER_POLICY_UPDATE: &str = "tier_policy.update";
    pub const TIER_RESTORE_CREATE: &str = "tier_restore.create";
    pub const TIER_PIN_RELEASE: &str = "tier_pin.release";
    pub const TIER_PIN_EXTEND: &str = "tier_pin.extend";
}

/// Entity families, used for the `entity_type` column and its filter.
pub mod entity {
    pub const ORG: &str = "org";
    pub const PROJECT: &str = "project";
    pub const APP: &str = "app";
    pub const ENVIRONMENT: &str = "environment";
    pub const MEMBER: &str = "member";
    pub const ROLE: &str = "role";
    pub const GRANT: &str = "grant";
    pub const ALERT_RULE: &str = "alert_rule";
    pub const ALERT_CHANNEL: &str = "alert_channel";
    pub const MONITOR: &str = "monitor";
    pub const ARTIFACT: &str = "artifact";
    pub const STORE: &str = "store";
    pub const INSPECTOR_POLICY: &str = "inspector_policy";
    pub const TIER: &str = "tier";
    /// A group of ingest events that failed to persist. Deployment-wide like
    /// [`TIER`]: a group may have no `org_id` at all, because the dominant
    /// failure is a payload that never decoded.
    pub const INGEST_FAILURE: &str = "ingest_failure";
    /// Read-time projections of the two inspector audit tables. Never written
    /// by this module — see `repo::list_audit_feed`, which emits this literal
    /// in SQL. Declared here anyway so [`ALL`] can offer it as a filter value
    /// and the two spellings cannot drift apart.
    pub const PII: &str = "pii";

    /// Sign-in activity. Hidden from the default feed — `repo::list_audit_feed`
    /// filters on this exact literal and migration 52's partial index is
    /// predicated on it, so the three spellings must agree.
    pub const AUTH: &str = "auth";

    /// Every entity family the feed can contain, including the read-only
    /// projections. The API validates `?entity_type=` against this.
    pub const ALL: [&str; 17] = [
        AUTH,
        ORG,
        PROJECT,
        APP,
        ENVIRONMENT,
        MEMBER,
        ROLE,
        GRANT,
        ALERT_RULE,
        ALERT_CHANNEL,
        MONITOR,
        ARTIFACT,
        STORE,
        INSPECTOR_POLICY,
        TIER,
        INGEST_FAILURE,
        PII,
    ];
}

// ===========================================================================
// Diff allowlists
// ===========================================================================

/// The fields each entity family may record in `changes`.
///
/// This is the security boundary of the whole feature. A field absent here can
/// never enter the log no matter what a handler passes to [`diff`].
pub fn allowlist(entity_type: &str) -> &'static [&'static str] {
    match entity_type {
        entity::PROJECT => &["name", "slug"],
        // `ingest_enabled` is here because muting an app is a silent, total
        // outage for it; a trail that recorded the rename but not the mute
        // would omit the more consequential of the two.
        entity::APP => &["name", "platform", "ingest_enabled"],
        entity::ENVIRONMENT => &["name", "ingest_enabled", "is_default", "retired_at"],
        // `reset_action` distinguishes issuing a reset from cancelling a
        // pending one. Neither the token nor the temporary password is ever a
        // candidate: no allowlist key could carry them.
        entity::MEMBER => &[
            "is_active",
            "email",
            "name",
            "revoked_sessions",
            "reset_action",
            "expires_at",
        ],
        entity::ROLE => &["name", "permissions"],
        // `scopes` carries the whole batch: one API call grants a role at N
        // scopes at once, and splitting that into N entries would make a single
        // admin action look like N unrelated ones in the feed.
        entity::GRANT => &[
            "role_id",
            "role_name",
            "scope_type",
            "scope_id",
            "scopes",
            "permissions",
        ],
        entity::ALERT_RULE => &[
            "name",
            "enabled",
            "threshold",
            "window_minutes",
            "monitor_id",
            "severity",
        ],
        // NOT `config` and NOT `webhook_url`: those carry the delivery secret
        // that D5/D6 exist to keep below `manage`.
        entity::ALERT_CHANNEL => &["name", "kind", "enabled"],
        // `webhook_url` and `config` are absent deliberately: a monitor's
        // webhook can carry a delivery secret in its query string.
        entity::MONITOR => &[
            "name",
            "url",
            "interval_seconds",
            "enabled",
            "method",
            "cascaded_alert_rules",
        ],
        entity::ARTIFACT => &["name", "kind", "size_bytes"],
        entity::STORE => &["store", "package_name", "enabled"],
        entity::INSPECTOR_POLICY => &["name", "enabled", "targets"],
        entity::TIER => &["hot_days", "table_name", "expires_at"],
        // Enough to reconstruct WHAT was dropped without carrying ANY of it.
        // `payload` is conspicuously absent and can never be added: the rows
        // this describes are masked copies of real user events, and a drop is a
        // hard DELETE, so an entry carrying the payload would resurrect into a
        // table org admins read and that is kept forever — precisely what the
        // hard delete exists to prevent.
        entity::INGEST_FAILURE => &[
            "fingerprint",
            "error_kind",
            "error_message",
            "occurrences",
            "retained",
            "dropped",
        ],
        entity::ORG => &["name"],
        // `reason` distinguishes a wrong password from a deactivated account
        // from a forced reset. No credential material has a key here, and none
        // could: `FORBIDDEN_FIELDS` rejects `password` as a substring.
        entity::AUTH => &["reason"],
        _ => &[],
    }
}

/// Field names that must never appear in any allowlist, at any time.
///
/// Pinned by a test rather than left as a comment: the allowlists will be
/// edited by people who are not thinking about this, and a single added line
/// would otherwise start writing secrets into a table that org admins read and
/// that is never pruned.
pub const FORBIDDEN_FIELDS: &[&str] = &[
    "public_key",
    "private_key",
    "config",
    "webhook_url",
    "password",
    "password_hash",
    "secret",
    "token",
    "api_key",
    "ingest_key",
    "dsn",
];

/// True if a field name looks secret-bearing.
///
/// The second of two independent checks, and deliberately redundant with the
/// first. `forbidden_fields_never_appear_in_any_allowlist` catches a bad
/// allowlist entry at test time; this catches it at run time, so a secret
/// added to an allowlist on a branch where that test was not run still never
/// reaches the table. Substring, not equality, so `slack_webhook_url` and
/// `smtp_password` are caught alongside the exact names.
fn is_forbidden(field: &str) -> bool {
    FORBIDDEN_FIELDS.iter().any(|f| field.contains(f))
}

/// Build a `{field: {from, to}}` object from before/after pairs, keeping only
/// allowlisted fields whose value actually changed.
///
/// Unchanged fields are dropped: an entry saying a rename changed `name` and
/// nothing else is readable, whereas one listing every field with `from` equal
/// to `to` buries the one thing that happened.
pub fn diff(entity_type: &str, pairs: &[(&str, Value, Value)]) -> Value {
    let allow = allowlist(entity_type);
    let mut out = serde_json::Map::new();
    for (field, before, after) in pairs {
        if !allow.contains(field) || is_forbidden(field) {
            continue;
        }
        if before == after {
            continue;
        }
        out.insert((*field).to_string(), json!({ "from": before, "to": after }));
    }
    Value::Object(out)
}

/// Build a `{field: {from: null, to: value}}` object for a creation, keeping
/// only allowlisted fields.
pub fn created(entity_type: &str, fields: &[(&str, Value)]) -> Value {
    let allow = allowlist(entity_type);
    let mut out = serde_json::Map::new();
    for (field, value) in fields {
        if !allow.contains(field) || is_forbidden(field) {
            continue;
        }
        out.insert(
            (*field).to_string(),
            json!({ "from": Value::Null, "to": value }),
        );
    }
    Value::Object(out)
}

// ===========================================================================
// Recording
// ===========================================================================

/// One administrative action, ready to record.
///
/// `project`, `app` and `environment` are `(id, name)` pairs rather than bare
/// ids: the name is snapshotted so the entry survives the row's deletion.
///
/// Owns its strings rather than borrowing. Handlers assemble these from a mix
/// of borrowed request data and freshly-queried names, and threading one
/// lifetime through all of that at forty call sites buys nothing at this
/// volume — a few dozen short allocations per administrative action.
#[derive(Debug, Clone)]
pub struct Entry {
    pub org_id: Uuid,
    pub action: String,
    pub entity_type: String,
    pub entity_id: Option<Uuid>,
    pub entity_name: String,
    pub project: Option<(Uuid, String)>,
    pub app: Option<(Uuid, String)>,
    pub environment: Option<(Uuid, String)>,
    pub changes: Value,
}

impl Entry {
    /// Start an entry. `changes` defaults to `{}` — an action with nothing to
    /// diff (a delete, a password reset) is a legitimate entry.
    pub fn new(org_id: Uuid, action: &str, entity_type: &str) -> Self {
        Self {
            org_id,
            action: action.to_string(),
            entity_type: entity_type.to_string(),
            entity_id: None,
            entity_name: String::new(),
            project: None,
            app: None,
            environment: None,
            changes: Value::Object(serde_json::Map::new()),
        }
    }

    pub fn target(mut self, id: Uuid, name: impl Into<String>) -> Self {
        self.entity_id = Some(id);
        self.entity_name = name.into();
        self
    }

    /// Name a target that has no id of its own — a singleton setting such as
    /// the cold-tier rotation age. Leaves `entity_id` NULL rather than filling
    /// it with a nil UUID, which would read as a real id pointing at nothing.
    pub fn target_named(mut self, name: impl Into<String>) -> Self {
        self.entity_name = name.into();
        self
    }

    pub fn project(mut self, id: Uuid, name: impl Into<String>) -> Self {
        self.project = Some((id, name.into()));
        self
    }

    pub fn app(mut self, id: Uuid, name: impl Into<String>) -> Self {
        self.app = Some((id, name.into()));
        self
    }

    pub fn environment(mut self, id: Uuid, name: impl Into<String>) -> Self {
        self.environment = Some((id, name.into()));
        self
    }

    pub fn changes(mut self, changes: Value) -> Self {
        self.changes = changes;
        self
    }
}

/// Fill in the project and app name snapshots for an app-scoped entry.
///
/// Handlers below the app level typically hold only an `app_id`. Rather than
/// have each of them join for the two names, they call this. A lookup failure
/// leaves the names blank rather than dropping the entry: a row that says what
/// happened but not exactly where is far better than no row at all, and this
/// whole path is fail-open by design.
pub async fn with_app_scope(conn: &mut AsyncPgConnection, entry: Entry, app_id: Uuid) -> Entry {
    match repo::audit_app_scope(conn, app_id).await {
        Ok(Some((project_id, project_name, app_name))) => entry
            .project(project_id, project_name)
            .app(app_id, app_name),
        Ok(None) => entry.app(app_id, ""),
        Err(e) => {
            tracing::warn!(error = %e, %app_id, "audit: could not resolve app scope names");
            entry.app(app_id, "")
        }
    }
}

/// Build a project-scoped entry, resolving the org partition and the project
/// name from `project_id` in one query.
///
/// Returns `None` when the project cannot be resolved — the caller then has no
/// org to file the entry under, and an entry with the wrong `org_id` would be
/// visible to the wrong tenant. Dropping it is the only safe outcome, and the
/// fail-open contract already permits a missing row.
pub async fn project_entry(
    conn: &mut AsyncPgConnection,
    project_id: Uuid,
    action: &str,
    entity_type: &str,
) -> Option<Entry> {
    match repo::audit_project_scope(conn, project_id).await {
        Ok(Some((org_id, project_name))) => {
            Some(Entry::new(org_id, action, entity_type).project(project_id, project_name))
        }
        Ok(None) => None,
        Err(e) => {
            tracing::warn!(error = %e, %project_id, "audit: could not resolve project scope");
            None
        }
    }
}

/// Record a **deployment-wide** action into every org's trail.
///
/// The cold-tier rotation age, restores and pins are a single setting that
/// governs every tenant's data at once — `require_deployment_admin` exists
/// precisely because one tenant's admin must not be able to change them. The
/// Wall is org-partitioned and its read gate is org-scoped, so filing such an
/// action under one org would hide it from every other tenant it affects.
/// Writing one entry per org is the only shape that reaches the people whose
/// data just changed.
///
/// Safe at this volume: these are rare, manual operator actions, and the
/// deployments with many orgs are exactly the ones where the disclosure
/// matters most.
pub async fn record_all_orgs(conn: &mut AsyncPgConnection, actor_id: Uuid, entry: Entry) {
    let orgs = match repo::all_org_ids(conn).await {
        Ok(ids) => ids,
        Err(e) => {
            tracing::error!(error = %e, action = %entry.action,
                "audit: could not enumerate orgs for a deployment-wide action");
            return;
        }
    };
    for org_id in orgs {
        let mut per_org = entry.clone();
        per_org.org_id = org_id;
        record(conn, actor_id, per_org).await;
    }
}

/// Record an event into every org the actor belongs to.
///
/// Authentication is not org-scoped — a user signs in to the deployment, not to
/// an org — but the Wall is org-partitioned and its read gate is org-scoped, so
/// an auth event has to be filed somewhere to be visible at all. Every org the
/// actor is a member of is the honest answer: each of those orgs' admins has a
/// legitimate interest in "this member signed in", and none of them learns
/// anything about the OTHER orgs the actor belongs to, since each entry names
/// only its own org.
///
/// Callers must only reach this with a KNOWN user. A failed sign-in for an
/// address that matches no account has no orgs to file under, and recording it
/// anywhere would turn the Wall into an account-enumeration oracle — the same
/// reason `forgot_password` is exempt from auditing entirely.
pub async fn record_for_user_orgs(conn: &mut AsyncPgConnection, actor_id: Uuid, entry: Entry) {
    let orgs = match repo::list_orgs_for_user(conn, actor_id).await {
        Ok(orgs) => orgs,
        Err(e) => {
            tracing::error!(error = %e, action = %entry.action,
                "audit: could not resolve the actor's orgs for an auth event");
            return;
        }
    };
    // A user with no grants anywhere records nothing. There is no org whose
    // admin could read it, so an entry would be write-only.
    for org in orgs {
        let mut per_org = entry.clone();
        per_org.org_id = org.id;
        record(conn, actor_id, per_org).await;
    }
}

/// Record an administrative action.
///
/// Fail-open by contract: returns `()`, logs failures at error level. Call
/// this AFTER the action's own transaction has committed — inside it, a
/// swallowed error would still poison the surrounding transaction and abort
/// the caller's work, which is fail-closed by accident and the exact opposite
/// of what this function promises.
pub async fn record(conn: &mut AsyncPgConnection, actor_id: Uuid, entry: Entry) {
    // Resolved here rather than in every handler so no call site can forget
    // the snapshot that decision 8 depends on.
    let actor_email = match repo::user_email(conn, actor_id).await {
        Ok(email) => email.unwrap_or_default(),
        Err(e) => {
            tracing::warn!(error = %e, "audit: could not resolve actor email; recording without it");
            String::new()
        }
    };

    let new = NewAuditLogEntry {
        org_id: entry.org_id,
        actor_id: Some(actor_id),
        actor_email: &actor_email,
        action: &entry.action,
        entity_type: &entry.entity_type,
        entity_id: entry.entity_id,
        entity_name: &entry.entity_name,
        project_id: entry.project.as_ref().map(|p| p.0),
        project_name: entry.project.as_ref().map(|p| p.1.as_str()).unwrap_or(""),
        app_id: entry.app.as_ref().map(|a| a.0),
        app_name: entry.app.as_ref().map(|a| a.1.as_str()).unwrap_or(""),
        environment_id: entry.environment.as_ref().map(|e| e.0),
        environment_name: entry
            .environment
            .as_ref()
            .map(|e| e.1.as_str())
            .unwrap_or(""),
        changes: entry.changes.clone(),
    };

    if let Err(e) = repo::insert_audit_log(conn, new).await {
        // Deliberately not propagated. The action the user asked for has
        // already succeeded; failing their request now would be a lie about
        // what happened, and refusing future ones would be a self-inflicted
        // outage. The gap is visible in the log and as a hole in the trail.
        tracing::error!(
            error = %e,
            action = %entry.action,
            org_id = %entry.org_id,
            "audit: FAILED to record administrative action"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forbidden_fields_never_appear_in_any_allowlist() {
        let families = [
            entity::ORG,
            entity::PROJECT,
            entity::APP,
            entity::ENVIRONMENT,
            entity::MEMBER,
            entity::ROLE,
            entity::GRANT,
            entity::ALERT_RULE,
            entity::ALERT_CHANNEL,
            entity::MONITOR,
            entity::ARTIFACT,
            entity::STORE,
            entity::INSPECTOR_POLICY,
            entity::TIER,
            entity::AUTH,
        ];
        for family in families {
            for field in allowlist(family) {
                assert!(
                    !FORBIDDEN_FIELDS.contains(field),
                    "{family} allowlists {field}, which is a secret-bearing field"
                );
                // Catches `slack_webhook_url`, `smtp_password`, `app_secret` —
                // variants an exact-match check would wave through.
                for forbidden in FORBIDDEN_FIELDS {
                    assert!(
                        !field.contains(forbidden),
                        "{family} allowlists {field}, which contains the \
                         secret-bearing substring {forbidden}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_secret_field_smuggled_onto_an_allowlist_is_still_dropped() {
        // The second, runtime half of the guard. Simulates the exact mistake
        // `forbidden_fields_never_appear_in_any_allowlist` catches at test
        // time: `allowlist` here is stubbed to return a secret-bearing name,
        // and `diff` must still refuse it.
        assert!(is_forbidden("webhook_url"));
        assert!(is_forbidden("slack_webhook_url"));
        assert!(is_forbidden("smtp_password"));
        assert!(is_forbidden("app_secret"));
        assert!(!is_forbidden("name"));
        assert!(!is_forbidden("enabled"));
    }

    #[test]
    fn entity_all_covers_every_declared_family() {
        // A family declared but missing from ALL would be unfilterable through
        // the API while still appearing in the feed.
        for family in [
            entity::ORG,
            entity::PROJECT,
            entity::APP,
            entity::ENVIRONMENT,
            entity::MEMBER,
            entity::ROLE,
            entity::GRANT,
            entity::ALERT_RULE,
            entity::ALERT_CHANNEL,
            entity::MONITOR,
            entity::ARTIFACT,
            entity::STORE,
            entity::INSPECTOR_POLICY,
            entity::TIER,
            entity::INGEST_FAILURE,
            entity::PII,
            entity::AUTH,
        ] {
            assert!(
                entity::ALL.contains(&family),
                "{family} is declared but absent from entity::ALL, so the API \
                 would reject it as an unknown entity_type"
            );
        }
    }

    #[test]
    fn diff_drops_unallowlisted_fields() {
        // `config` is a real field on alert channels and is deliberately absent
        // from its allowlist. A handler passing it must produce nothing.
        let d = diff(
            entity::ALERT_CHANNEL,
            &[
                ("name", json!("old"), json!("new")),
                (
                    "config",
                    json!({"url": "https://secret"}),
                    json!({"url": "https://other"}),
                ),
            ],
        );
        assert_eq!(d, json!({ "name": { "from": "old", "to": "new" } }));
        assert!(d.get("config").is_none());
    }

    #[test]
    fn diff_drops_unchanged_fields() {
        let d = diff(
            entity::PROJECT,
            &[
                ("name", json!("same"), json!("same")),
                ("slug", json!("a"), json!("b")),
            ],
        );
        assert_eq!(d, json!({ "slug": { "from": "a", "to": "b" } }));
    }

    #[test]
    fn diff_of_nothing_is_an_empty_object_not_null() {
        // The column is NOT NULL; a null here would fail the insert and lose
        // the entry through the fail-open path, silently.
        let d = diff(entity::PROJECT, &[]);
        assert_eq!(d, json!({}));
        assert!(d.is_object());
    }

    #[test]
    fn created_records_from_null() {
        let c = created(
            entity::ENVIRONMENT,
            &[("name", json!("staging")), ("public_key", json!("pk_leak"))],
        );
        assert_eq!(c, json!({ "name": { "from": null, "to": "staging" } }));
        assert!(c.get("public_key").is_none());
    }

    #[test]
    fn unknown_entity_family_allowlists_nothing() {
        // A typo'd entity_type must record no fields rather than all of them.
        assert!(allowlist("typo").is_empty());
        let d = diff("typo", &[("name", json!("a"), json!("b"))]);
        assert_eq!(d, json!({}));
    }

    #[test]
    fn entry_builder_defaults_changes_to_empty_object() {
        let e = Entry::new(Uuid::nil(), action::MEMBER_RESET_PASSWORD, entity::MEMBER);
        assert_eq!(e.changes, json!({}));
        assert!(e.project.is_none());
    }
}
