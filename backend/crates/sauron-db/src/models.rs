//! Diesel row models (`Queryable`/`Selectable`) and insert structs.
//!
//! Hierarchy: `Organization → Project (grouping) → App (ingest unit) → signals`.
//! Access control: `Role` (permission bundle) + `RoleGrant` (user↔role at a
//! scope). Row structs derive `Serialize`; secrets are skipped.

use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::schema::*;

/// Serialize a client IP in coarsened form.
///
/// Raw end-user IPs are personal data, and every read endpoint that returns
/// events/sessions is reachable with the ordinary `event:read` permission the
/// preset Viewer role holds. Masking the host portion (IPv4 → /24, IPv6 → /48)
/// keeps the network/geo signal a dashboard actually uses while removing the
/// ability to harvest identifiable addresses at scale. Storage is unchanged —
/// only what leaves the API is coarsened.
pub fn serialize_masked_ip<S>(ip: &Option<String>, s: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match ip.as_deref().map(mask_ip) {
        Some(masked) => s.serialize_some(&masked),
        None => s.serialize_none(),
    }
}

/// Coarsen an IP literal; unparseable input becomes `None`-like `"invalid"`
/// rather than being echoed back verbatim.
fn mask_ip(raw: &str) -> String {
    match raw.trim().parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(v4)) => {
            let o = v4.octets();
            format!("{}.{}.{}.0/24", o[0], o[1], o[2])
        }
        Ok(std::net::IpAddr::V6(v6)) => {
            let seg = v6.segments();
            format!("{:x}:{:x}:{:x}::/48", seg[0], seg[1], seg[2])
        }
        Err(_) => "invalid".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Organizations & users
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize, utoipa::ToSchema)]
#[diesel(table_name = organizations)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Organization {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = organizations)]
pub struct NewOrganization<'a> {
    pub name: &'a str,
    pub slug: &'a str,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize, utoipa::ToSchema)]
#[diesel(table_name = users)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct User {
    pub id: Uuid,
    pub email: String,
    #[serde(skip_serializing)]
    pub password_hash: String,
    pub name: String,
    pub last_login_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub is_active: bool,
    pub must_change_password: bool,
    /// Set when an admin forced a password reset and the replacement has not
    /// been chosen yet; `login` refuses on it *after* the Argon2 verification.
    ///
    /// `#[serde(skip_serializing)]` because `User` is returned by `/v1/me` and
    /// inside `AuthResponse`, and a caller holding either has by definition just
    /// authenticated — so the field could only ever be null there. A
    /// permanently-null key in the public user object is noise someone will
    /// eventually build a client behaviour on.
    #[serde(skip_serializing)]
    pub credentials_invalidated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = users)]
pub struct NewUser<'a> {
    pub email: &'a str,
    pub password_hash: &'a str,
    pub name: &'a str,
}

// ---------------------------------------------------------------------------
// Projects (grouping) & apps (ingest unit)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize, utoipa::ToSchema)]
#[diesel(table_name = projects)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Project {
    pub id: Uuid,
    pub org_id: Uuid,
    pub name: String,
    pub slug: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = projects)]
pub struct NewProject<'a> {
    pub org_id: Uuid,
    pub name: &'a str,
    pub slug: &'a str,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize, utoipa::ToSchema)]
#[diesel(table_name = apps)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct App {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub platform: Option<String>,
    pub ingest_enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub app_type: String,
    pub project_id: Uuid,
    /// The environment whose build ships to the app stores, or `None`.
    ///
    /// An `app_environments` (enrollment) id, not a catalogue id — the same id
    /// the dashboard's environment switcher carries, so the Overview gate is a
    /// plain equality check. The stores themselves have no environment
    /// dimension; this only decides where the section is shown.
    pub store_environment_id: Option<Uuid>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = apps)]
pub struct NewApp<'a> {
    pub project_id: Uuid,
    pub name: &'a str,
    pub slug: &'a str,
    pub app_type: &'a str,
}

/// An environment as an admin defines it: a name, owned by a *project*.
///
/// This is the catalogue entry, not the thing an SDK reports to. It holds no
/// key and no ingest switch — those belong to the per-app enrollment below,
/// because a key that did not name an app could not prove which app an incoming
/// event belonged to.
#[derive(Debug, Clone, Queryable, Selectable, Serialize, utoipa::ToSchema)]
#[diesel(table_name = environments)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Environment {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub retired_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = environments)]
pub struct NewEnvironment<'a> {
    pub project_id: Uuid,
    pub name: &'a str,
}

/// One app's enrollment in one environment: the ingest credential and the
/// switches that are legitimately per-app.
///
/// `is_default` lives here rather than on [`Environment`] because "which
/// environment does this app report to by default" is a property of the app,
/// and a second `is_default` one level up would give two rows the authority to
/// answer the same question.
#[derive(Debug, Clone, Queryable, Selectable, Serialize, utoipa::ToSchema)]
#[diesel(table_name = app_environments)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct AppEnvironment {
    pub id: Uuid,
    pub app_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub public_key: String,
    pub ingest_enabled: bool,
    pub is_default: bool,
    pub retired_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
    pub environment_id: Uuid,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = app_environments)]
pub struct NewAppEnvironment<'a> {
    pub app_id: Uuid,
    pub environment_id: Uuid,
    pub public_key: &'a str,
    pub is_default: bool,
}

/// An enrollment joined to its catalogue name — what the dashboard's per-app
/// environment list and DSN table actually render. The name is not stored on
/// the enrollment (that is exactly the drift this feature removed), so any
/// caller that needs to *display* an enrollment needs this join.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct AppEnvironmentView {
    #[serde(flatten)]
    pub enrollment: AppEnvironment,
    pub name: String,
}

/// Everything the ingest edge needs after presenting a key: the environment it
/// belongs to, its ancestry, and both ingest switches. Cached in Redis as JSON.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct EnvRef {
    pub env_id: Uuid,
    pub app_id: Uuid,
    pub project_id: Uuid,
    pub org_id: Uuid,
    pub env_ingest_enabled: bool,
    pub app_ingest_enabled: bool,
}

// ---------------------------------------------------------------------------
// RBAC: roles & grants
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize, utoipa::ToSchema)]
#[diesel(table_name = roles)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Role {
    pub id: Uuid,
    pub org_id: Option<Uuid>,
    pub name: String,
    pub description: String,
    pub is_system: bool,
    pub permissions: Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = roles)]
pub struct NewRole<'a> {
    pub org_id: Option<Uuid>,
    pub name: &'a str,
    pub description: &'a str,
    pub is_system: bool,
    pub permissions: Value,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize, utoipa::ToSchema)]
#[diesel(table_name = role_grants)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct RoleGrant {
    pub id: Uuid,
    pub org_id: Uuid,
    pub user_id: Uuid,
    pub role_id: Uuid,
    pub scope_type: String,
    pub scope_id: Uuid,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = role_grants)]
pub struct NewRoleGrant {
    pub org_id: Uuid,
    pub user_id: Uuid,
    pub role_id: Uuid,
    pub scope_type: String,
    pub scope_id: Uuid,
}

// ---------------------------------------------------------------------------
// Issues & error events (keyed by app_id)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize, utoipa::ToSchema)]
#[diesel(table_name = issues)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Issue {
    pub id: Uuid,
    pub app_id: Uuid,
    pub fingerprint: String,
    #[serde(rename = "type")]
    pub type_: String,
    pub title: String,
    pub culprit: String,
    pub level: String,
    pub status: String,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub times_seen: i64,
    pub users_seen: i64,
    pub assignee_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Ingest-side clock, advanced only when a new event lands on this issue.
    /// The regression trigger keys off this rather than `last_seen` (which is
    /// client-supplied) or `updated_at` (which status changes also bump).
    ///
    /// Internal watermark — not part of the API contract, so it stays out of
    /// the serialized issue the dashboard consumes.
    #[serde(skip_serializing)]
    pub last_event_at: DateTime<Utc>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = issues)]
pub struct NewIssue<'a> {
    pub app_id: Uuid,
    pub fingerprint: &'a str,
    #[diesel(column_name = type_)]
    pub type_: &'a str,
    pub title: &'a str,
    pub culprit: &'a str,
    pub level: &'a str,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub times_seen: i64,
}

/// The newest occurrence of one issue, reduced to what a culprit repair needs.
///
/// Deliberately NOT a whole `ErrorEvent`: this is fetched on the issues-list
/// hot path, and the columns left out are the large ones (`stacktrace`,
/// `breadcrumbs`, `context`, `contexts`, `extra`). Selecting the full row would
/// pull a crash payload per listed issue to read one string out of it.
#[derive(Debug, Clone, QueryableByName)]
pub struct IssueLatestFrames {
    #[diesel(sql_type = diesel::sql_types::Uuid)]
    pub issue_id: Uuid,
    #[diesel(sql_type = diesel::sql_types::Uuid)]
    pub id: Uuid,
    #[diesel(sql_type = diesel::sql_types::Timestamptz)]
    pub occurred_at: DateTime<Utc>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Jsonb>)]
    pub stacktrace_symbolicated: Option<Value>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize, utoipa::ToSchema)]
#[diesel(table_name = error_events)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ErrorEvent {
    pub id: Uuid,
    pub app_id: Uuid,
    pub environment_id: Option<Uuid>,
    pub issue_id: Uuid,
    pub fingerprint: String,
    pub level: String,
    pub message: String,
    pub exception_type: String,
    pub exception_value: String,
    pub stacktrace: Value,
    pub breadcrumbs: Value,
    pub context: Value,
    pub tags: Value,
    pub release: Option<String>,
    pub distinct_id: Option<String>,
    pub event_user: Option<Value>,
    pub sdk: Option<Value>,
    #[serde(serialize_with = "serialize_masked_ip")]
    pub ip_address: Option<String>,
    pub occurred_at: DateTime<Utc>,
    pub received_at: DateTime<Utc>,
    pub session_id: Option<String>,
    pub device_key: Option<String>,
    pub screen: Option<String>,
    /// Server-symbolicated frames (no source-context lines); null until resolved.
    pub stacktrace_symbolicated: Option<Value>,
    /// pending | symbolicated | partial | no_artifacts | not_applicable | failed.
    pub symbolication_status: String,
    /// Dart debug header (`build_id`, `dso_base`, `arch`, `os`, `raw_stacktrace`).
    pub debug_meta: Option<Value>,
    /// Dev-supplied structured context blocks (distinct from machine `context`).
    pub contexts: Value,
    /// Dev-supplied freeform JSON.
    pub extra: Value,
    /// Whether the SDK saw this error caught (`Some(true)`) or uncaught (`Some(false)`).
    /// `None` for rows ingested before this column existed — never backfilled.
    pub handled: Option<bool>,
    /// The per-occurrence `build_title` string. `None` for rows written before
    /// migration 30 added the column.
    pub title: Option<String>,
    /// The per-occurrence culprit — `function (file)` for the frame nearest the
    /// crash, de-obfuscated when symbolication resolved one.
    ///
    /// Served so the session timeline can label an error row with the same
    /// string the Exceptions list shows, instead of each surface deriving its
    /// own from whatever frames it happens to have. `Some("")` is a real value:
    /// the occurrence had no frames (see `build_culprit`), which is distinct
    /// from `None`, a pre-migration-30 row that never had the column.
    pub culprit: Option<String>,
    /// `Some` = the trace lives in `error_stack_blobs`; `stacktrace` above is
    /// a placeholder until `stack_pool::hydrate` swaps the real value in.
    /// Skipped in serialization — the wire shape is unchanged by pooling.
    #[serde(skip)]
    pub stacktrace_sha256: Option<Vec<u8>>,
}

/// **`Insertable`-only, on purpose.** Diesel's `Insertable` maps fields to
/// columns by NAME, so this struct's field order is free to differ from
/// `schema.rs`'s column order — and it does: `workflow_id`/`workflow_name` sit
/// beside `screen` here for readability, while `schema.rs` appends them last
/// (a later migration added them). Do NOT add `Queryable` to this struct:
/// that derive decodes POSITIONALLY, so the very field order that is harmless
/// today would silently bind each field to whatever column occupies its index
/// and return garbage — compiling cleanly, with `check_for_backend` none the
/// wiser. Read rows into `ErrorEvent` instead.
#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = error_events)]
pub struct NewErrorEvent {
    pub id: Uuid,
    pub app_id: Uuid,
    pub environment_id: Option<Uuid>,
    pub issue_id: Uuid,
    pub fingerprint: String,
    pub level: String,
    pub message: String,
    pub exception_type: String,
    pub exception_value: String,
    pub stacktrace: Value,
    pub breadcrumbs: Value,
    pub context: Value,
    pub tags: Value,
    pub release: Option<String>,
    pub distinct_id: Option<String>,
    pub event_user: Option<Value>,
    pub sdk: Option<Value>,
    pub ip_address: Option<String>,
    pub occurred_at: DateTime<Utc>,
    pub session_id: Option<String>,
    pub device_key: Option<String>,
    pub screen: Option<String>,
    /// The workflow this occurrence was stamped as belonging to, if any —
    /// `None` for every app that never calls `startWorkflow` (byte-identical
    /// to the pre-workflows column). See `bump_workflow`/
    /// `apply_workflow_lifecycle` in `repo.rs` for the rollup this feeds.
    pub workflow_id: Option<String>,
    pub workflow_name: Option<String>,
    /// Ingest-time pre-symbolication (lean, no context); null when unresolved.
    pub stacktrace_symbolicated: Option<Value>,
    /// pending | symbolicated | partial | no_artifacts | not_applicable.
    pub symbolication_status: String,
    /// Dart debug header + verbatim trace (`{build_id,isolate_dso_base,arch,os,raw_stacktrace}`).
    pub debug_meta: Option<Value>,
    pub contexts: Value,
    pub extra: Value,
    /// Whether the SDK saw this error caught (`Some(true)`) or uncaught (`Some(false)`).
    pub handled: Option<bool>,
    /// The per-occurrence `build_title`/`build_culprit` strings computed at
    /// ingest, immediately before `upsert_issue` (which is where the same
    /// values currently overwrite the app-wide `issues` row regardless of
    /// environment). `None` for rows written before this column existed —
    /// never backfilled; the read path falls back to the app-wide `issues`
    /// column for those. `Some("")` for `culprit` is a real value — the
    /// occurrence had no exception, not "unknown" — see `build_culprit`.
    pub title: Option<String>,
    pub culprit: Option<String>,
    /// Set by `stack_pool::intern` (never by constructors — leave `None`):
    /// when pooling is on, the trace moves to `error_stack_blobs` and this
    /// carries its content address while `stacktrace` holds the placeholder.
    pub stacktrace_sha256: Option<Vec<u8>>,
}

// ---------------------------------------------------------------------------
// Analytics events & people (keyed by app_id)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize, utoipa::ToSchema)]
#[diesel(table_name = analytics_events)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct AnalyticsEvent {
    pub id: Uuid,
    pub app_id: Uuid,
    pub environment_id: Option<Uuid>,
    pub name: String,
    pub distinct_id: String,
    pub properties: Value,
    pub context: Value,
    pub session_id: Option<String>,
    pub release: Option<String>,
    #[serde(serialize_with = "serialize_masked_ip")]
    pub ip_address: Option<String>,
    pub occurred_at: DateTime<Utc>,
    pub received_at: DateTime<Utc>,
    pub device_key: Option<String>,
    pub screen: Option<String>,
    pub tags: Value,
    /// Dev-supplied structured context blocks (distinct from machine `context`).
    pub contexts: Value,
    pub extra: Value,
}

/// **`Insertable`-only, on purpose** — see [`NewErrorEvent`]'s doc comment for
/// why the field order here may differ from `schema.rs`'s column order and why
/// adding `Queryable` to this struct would turn that into a silent-garbage bug.
#[derive(Debug, Insertable)]
#[diesel(table_name = analytics_events)]
pub struct NewAnalyticsEvent {
    pub id: Uuid,
    pub app_id: Uuid,
    pub environment_id: Option<Uuid>,
    pub name: String,
    pub distinct_id: String,
    pub properties: Value,
    pub context: Value,
    pub session_id: Option<String>,
    pub release: Option<String>,
    pub ip_address: Option<String>,
    pub occurred_at: DateTime<Utc>,
    pub device_key: Option<String>,
    pub screen: Option<String>,
    /// See `NewErrorEvent::workflow_id`'s doc comment — same optional stamp,
    /// same guarantee.
    pub workflow_id: Option<String>,
    pub workflow_name: Option<String>,
    pub tags: Value,
    pub contexts: Value,
    pub extra: Value,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize, utoipa::ToSchema)]
#[diesel(table_name = event_users)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct EventUser {
    pub id: Uuid,
    pub app_id: Uuid,
    pub distinct_id: String,
    pub properties: Value,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Non-NULL means this distinct_id names a person. Every read tests
    /// `IS NOT NULL` only — the timestamp itself is informational.
    pub identified_at: Option<DateTime<Utc>>,
    /// Which of `identify` / `context_user` / `backfill` set the flag. The
    /// only thing that makes a poisoned `context_user` cohort repairable
    /// without also clearing real identify() rows.
    pub identified_source: Option<String>,
}

// ---------------------------------------------------------------------------
// Refresh tokens
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = refresh_tokens)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct RefreshToken {
    pub id: Uuid,
    pub user_id: Uuid,
    pub token_hash: String,
    pub expires_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub user_agent: Option<String>,
    pub created_at: DateTime<Utc>,
    /// Why this token was revoked — see [`crate::repo::REVOKE_ROTATED`].
    pub revoked_reason: Option<String>,
    /// The `auth_sessions` row this token belongs to. Nullable because rows
    /// minted before migration 000035, and rows whose session the 30-day reaper
    /// has deleted, both have none — and the FK is ON DELETE SET NULL precisely
    /// so a reap cannot take the replay-detection history with it.
    pub session_id: Option<Uuid>,
}

/// A login that survives refresh-token rotation.
///
/// No `Serialize`, on purpose — the same discipline `RefreshToken` follows. The
/// API returns a hand-built `SessionView`; letting the model reach the wire is
/// how `revoked_by` (which admin ended your session) leaks to the member it was
/// used against.
#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = auth_sessions)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct AuthSession {
    pub id: Uuid,
    pub user_id: Uuid,
    pub created_at: DateTime<Utc>,
    /// Stamped on rotation, not per request — so this is accurate only to within
    /// `JWT_ACCESS_TTL_SECS`. Writing it on every request would turn a read-only
    /// auth path into a write on every API call.
    pub last_used_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub user_agent: Option<String>,
    pub ip: Option<String>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revoked_reason: Option<String>,
    pub revoked_by: Option<Uuid>,
}

/// A password-reset link.
///
/// Deliberately derives no `Serialize`, exactly like [`RefreshToken`]:
/// `token_hash` and `password_fingerprint` must never leave the process, and no
/// endpoint returns this row.
#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = password_reset_tokens)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct PasswordResetToken {
    pub id: Uuid,
    pub user_id: Uuid,
    pub token_hash: String,
    pub password_fingerprint: String,
    /// `"self"` or `"admin"` — see the CHECK in migration 000036.
    pub mode: String,
    pub initiated_by: Option<Uuid>,
    pub requested_from: Option<String>,
    pub expires_at: DateTime<Utc>,
    pub consumed_at: Option<DateTime<Utc>>,
    pub invalidated_at: Option<DateTime<Utc>>,
    pub invalidated_reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Insert-only. Must never gain `Queryable`: that derive decodes positionally,
/// so a struct whose field order differs from the `table!` block would bind
/// `mode` to `password_fingerprint` and still compile.
#[derive(Debug, Insertable)]
#[diesel(table_name = password_reset_tokens)]
pub struct NewPasswordResetToken {
    pub user_id: Uuid,
    pub token_hash: String,
    pub password_fingerprint: String,
    pub mode: String,
    pub initiated_by: Option<Uuid>,
    pub requested_from: Option<String>,
    pub expires_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Sessions & devices (roll-ups materialized by the pipeline, keyed by app_id)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize, utoipa::ToSchema)]
#[diesel(table_name = sessions)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Session {
    pub id: Uuid,
    pub app_id: Uuid,
    pub session_id: String,
    pub distinct_id: Option<String>,
    pub device_key: Option<String>,
    pub started_at: DateTime<Utc>,
    pub last_event_at: DateTime<Utc>,
    pub events_count: i64,
    pub errors_count: i64,
    /// Errors in this session the SDK reported as UNCAUGHT. `> 0` is what
    /// "crashed" means; `errors_count > 0` is what it used to mean and counted
    /// every handled warning too.
    pub unhandled_errors_count: i64,
    pub context: Value,
    pub release: Option<String>,
    pub environment_id: Option<Uuid>,
    #[serde(serialize_with = "serialize_masked_ip")]
    pub ip_address: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize, utoipa::ToSchema)]
#[diesel(table_name = devices)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Device {
    pub id: Uuid,
    pub app_id: Uuid,
    pub device_key: String,
    pub family: Option<String>,
    pub model: Option<String>,
    pub os_name: Option<String>,
    pub os_version: Option<String>,
    pub arch: Option<String>,
    pub browser: Option<String>,
    pub last_distinct_id: Option<String>,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub events_count: i64,
    pub errors_count: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Workflows (named, explicitly-bounded spans of activity within a session;
// entirely optional -- see docs/superpowers/specs/2026-07-29-workflow-
// grouping-design.md). Field order below must exactly match the column order
// declared in `schema.rs`'s `workflows` table! block: `Queryable` is
// positional, so a mismatch between two same-typed columns (e.g. two `Text`
// fields, or `started_at`/`ended_at`) would compile cleanly and silently
// return garbage.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize, utoipa::ToSchema)]
#[diesel(table_name = workflows)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Workflow {
    pub id: Uuid,
    pub app_id: Uuid,
    pub environment_id: Uuid,
    pub workflow_id: String,
    pub name: String,
    pub session_id: Option<String>,
    pub distinct_id: Option<String>,
    pub device_key: Option<String>,
    pub release: Option<String>,
    pub status: String,
    pub cancel_reason: Option<String>,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub last_event_at: DateTime<Utc>,
    pub events_count: i32,
    pub errors_count: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Transactions (performance signal, keyed by app_id)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize, utoipa::ToSchema)]
#[diesel(table_name = transactions)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Transaction {
    pub id: Uuid,
    pub app_id: Uuid,
    pub environment_id: Option<Uuid>,
    pub name: String,
    pub op: String,
    pub duration_ms: f64,
    pub status: Option<String>,
    pub http_method: Option<String>,
    pub http_status: Option<i32>,
    pub url: Option<String>,
    pub distinct_id: Option<String>,
    pub session_id: Option<String>,
    pub device_key: Option<String>,
    pub release: Option<String>,
    #[serde(serialize_with = "serialize_masked_ip")]
    pub ip_address: Option<String>,
    pub occurred_at: DateTime<Utc>,
    pub received_at: DateTime<Utc>,
    pub workflow_id: Option<String>,
    pub workflow_name: Option<String>,
    pub restored_pin_id: Option<Uuid>,
    pub finished_at: Option<DateTime<Utc>>,
    /// Dev-supplied flat string tags. Nulled by `strip_transaction_body` for a
    /// caller without `event:read` — see that function for why the free-text
    /// search reach must be narrowed in the same breath.
    pub tags: Value,
    /// Dev-supplied freeform JSON (request/response bodies and friends).
    /// Gated exactly as `tags` is.
    pub extra: Value,
}

/// **`Insertable`-only, on purpose** — see [`NewErrorEvent`]'s doc comment for
/// why the field order here may differ from `schema.rs`'s column order and why
/// adding `Queryable` to this struct would turn that into a silent-garbage bug.
#[derive(Debug, Insertable)]
#[diesel(table_name = transactions)]
pub struct NewTransaction {
    pub id: Uuid,
    pub app_id: Uuid,
    pub environment_id: Option<Uuid>,
    pub name: String,
    pub op: String,
    pub duration_ms: f64,
    pub status: Option<String>,
    pub http_method: Option<String>,
    pub http_status: Option<i32>,
    pub url: Option<String>,
    pub distinct_id: Option<String>,
    pub session_id: Option<String>,
    pub device_key: Option<String>,
    /// See `NewErrorEvent::workflow_id`'s doc comment — same optional stamp,
    /// same guarantee. Placed here (there is no `screen` field on
    /// transactions to sit next to) rather than at the end, so it stays
    /// grouped with the other identity/attribution fields.
    pub workflow_id: Option<String>,
    pub workflow_name: Option<String>,
    pub release: Option<String>,
    pub ip_address: Option<String>,
    pub occurred_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    /// Dev-supplied, per-call only — the pipeline passes the SDK's blob through
    /// verbatim and never merges anything into it.
    pub tags: Value,
    pub extra: Value,
}

// ---------------------------------------------------------------------------
// Monitors (uptime checks, keyed by project_id)
// ---------------------------------------------------------------------------

/// The `config` keys a monitor may expose over the API. Everything else is
/// omitted.
///
/// This is an ALLOWLIST, and that is the whole point. It began as a denylist
/// that stripped exactly `headers` and passed the rest of the object through,
/// which makes every key added to `config` afterwards public by default — the
/// same posture that made migration 000019 declare channel `config`
/// "non-secret" and leak webhook URLs, and the reason
/// `routes::notifications::redacted_config` is allowlisted per channel kind.
/// `config` is free-form JSONB with no schema to constrain it, so "default
/// open" here means the next credential-shaped key ships as a leak.
///
/// The list is derived from the only two real consumers, not from guesswork:
///
///  * `bins/sauron-monitor`'s `spec_of` is the sole reader of `config`. It
///    takes `headers`, `body`, `expected_status`, `body_assertion` and
///    `follow_redirects`, and nothing else — any other key in a stored row is
///    already dead weight the prober ignores.
///  * the dashboard reads **no** `config` key at all: `MonitorDetail.svelte`
///    renders `probe_header_names` and the top-level columns, and there is no
///    config editor anywhere (`UpdateMonitorReq` cannot even change `config`,
///    so the field is write-once at create and has no read-back-to-edit use).
///
/// So of `spec_of`'s five, three are settings and two are request payload:
///
///  * `headers` — excluded, unchanged. `spec_of` copies every entry verbatim
///    into the outbound probe request, so an `Authorization: Bearer …` here is
///    a live credential.
///  * `body` — now also excluded, which **revises** the earlier note that
///    called it ordinary configuration. It is copied verbatim into the request
///    for exactly the same reason `headers` is, so a monitor probing an
///    authenticated endpoint carries the credential in it (`password=…`, an API
///    key in a JSON body). `monitor:read` is held by the preset Viewer
///    (`sauron-auth`'s `rbac::VIEWER`), nothing renders the value, and nothing
///    can edit it — omitting it costs a consumer nothing and keeps a plausible
///    credential server-side. Its existence signal is `has_probe_body`, derived
///    in `monitor_view` (`routes/monitors.rs`) alongside `probe_header_names`.
///
/// A non-object `config` serializes as `{}` rather than passing through: the
/// dashboard types the field as `Record<string, unknown>`, and a `null` or a
/// scalar arriving there would be a type break for no gain.
pub const PUBLIC_PROBE_CONFIG_KEYS: [&str; 3] =
    ["expected_status", "body_assertion", "follow_redirects"];

/// Serialize a monitor's `config` down to [`PUBLIC_PROBE_CONFIG_KEYS`].
pub fn serialize_public_probe_config<S>(config: &Value, s: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    // Builds a map of at most three borrowed-then-cloned values instead of
    // cloning the whole object the way the strip-one-key version had to, so
    // the allowlist is also the cheaper path for a config with many keys.
    let mut out = serde_json::Map::with_capacity(PUBLIC_PROBE_CONFIG_KEYS.len());
    if let Some(o) = config.as_object() {
        for key in PUBLIC_PROBE_CONFIG_KEYS {
            if let Some(v) = o.get(key) {
                out.insert(key.to_string(), v.clone());
            }
        }
    }
    Value::Object(out).serialize(s)
}

/// A project's uptime check.
///
/// Some of these fields are credentials rather than settings, and all of them
/// are redacted here at the serializer instead of in any one handler.
/// `webhook_url` is a bearer-equivalent capability URL — a Slack/Discord/
/// PagerDuty hook needs no other authentication, so possession is authority —
/// and `config` is projected down to an allowlist because the probe request it
/// describes carries the operator's own headers and body (see
/// [`PUBLIC_PROBE_CONFIG_KEYS`] above). The route that returns
/// this struct is gated on `monitor:read`, which the preset Viewer role holds
/// (`sauron-auth`'s `rbac::VIEWER`), so a read-only member could otherwise
/// read both off the wire. The model layer is the right cut for the same
/// reason it is for `NotificationChannel::secret_enc` below: a strip in
/// `detail` alone would leave `create`/`update` echoing the secret back and
/// would silently re-leak from any future route returning a `Monitor`.
///
/// `bins/sauron-monitor` reads the columns straight from Postgres and never
/// deserializes this struct, so redacting the API serializer costs the prober
/// nothing.
#[derive(Debug, Clone, Queryable, Selectable, QueryableByName, Serialize, utoipa::ToSchema)]
#[diesel(table_name = monitors)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Monitor {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub kind: String,
    pub target: String,
    pub method: String,
    #[serde(serialize_with = "serialize_public_probe_config")]
    pub config: serde_json::Value,
    pub interval_seconds: i32,
    pub timeout_ms: i32,
    pub failure_threshold: i32,
    pub recovery_threshold: i32,
    #[serde(skip_serializing)]
    pub webhook_url: Option<String>,
    pub enabled: bool,
    pub status: String,
    pub consecutive_failures: i32,
    pub consecutive_successes: i32,
    pub last_checked_at: Option<DateTime<Utc>>,
    pub next_check_at: DateTime<Utc>,
    pub last_status_changed_at: Option<DateTime<Utc>>,
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = monitors)]
pub struct NewMonitor<'a> {
    pub project_id: Uuid,
    pub name: &'a str,
    pub kind: &'a str,
    pub target: &'a str,
    pub method: &'a str,
    pub config: &'a serde_json::Value,
    pub interval_seconds: i32,
    pub timeout_ms: i32,
    pub failure_threshold: i32,
    pub recovery_threshold: i32,
    pub webhook_url: Option<&'a str>,
    pub created_by: Option<Uuid>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize, utoipa::ToSchema)]
#[diesel(table_name = monitor_incidents)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct MonitorIncidentRow {
    pub id: Uuid,
    pub monitor_id: Uuid,
    pub started_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub cause: String,
    pub last_error: Option<String>,
}

// ---------------------------------------------------------------------------
// Symbol artifacts (source maps / Dart debug-info), content-addressed
// ---------------------------------------------------------------------------

#[derive(Debug, Insertable)]
#[diesel(table_name = symbol_blobs)]
pub struct NewSymbolBlob<'a> {
    pub sha256: &'a [u8],
    pub content: &'a [u8],
    pub uncompressed_size: i64,
    pub compressed_size: i64,
    /// Set to 1 on first insert; `put_blob` bumps on conflict.
    pub refcount: i32,
}

#[derive(Debug, Clone, Queryable, Selectable, Identifiable)]
#[diesel(table_name = symbol_artifacts)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct SymbolArtifact {
    pub id: Uuid,
    pub app_id: Uuid,
    pub kind: String,
    pub platform: String,
    pub arch: Option<String>,
    pub release: Option<String>,
    pub dist: Option<String>,
    pub name: Option<String>,
    pub debug_id: Option<String>,
    pub blob_sha256: Vec<u8>,
    pub prebuilt_index_sha256: Option<Vec<u8>>,
    pub uploaded_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = symbol_artifacts)]
pub struct NewSymbolArtifact {
    pub app_id: Uuid,
    pub kind: String,
    pub platform: String,
    pub arch: Option<String>,
    pub release: Option<String>,
    pub dist: Option<String>,
    pub name: Option<String>,
    pub debug_id: Option<String>,
    pub blob_sha256: Vec<u8>,
    pub prebuilt_index_sha256: Option<Vec<u8>>,
    pub uploaded_by: Option<Uuid>,
}

// ---------------------------------------------------------------------------
// Alerting: notification channels, rules, deliveries
// ---------------------------------------------------------------------------

/// A configured delivery destination.
///
/// BOTH payload columns are ciphertext and NEITHER is serialized to API
/// clients. `secret_enc` holds the credential bundle; `config_enc` holds the
/// destination — and the destination is not "non-secret settings" as migration
/// 000019 assumed: it is the webhook URL, its arbitrary header map (where an
/// `Authorization: Bearer …` lives), and for Slack/Discord a URL that is itself
/// the credential.
///
/// `config` is the DEPRECATED legacy plaintext. It is read only when
/// `config_enc` is NULL (rows predating migration 000046) and is blanked to
/// `{}` the first time the row is written. Reach the real value through
/// `sauron_alerts::crypto::open_channel_config`, never this field — reading
/// `config` directly is how a caller silently gets `{}` on a converted row.
#[derive(Debug, Clone, Queryable, Selectable, Serialize, utoipa::ToSchema)]
#[diesel(table_name = notification_channels)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct NotificationChannel {
    pub id: Uuid,
    pub org_id: Uuid,
    pub name: String,
    pub kind: String,
    #[serde(skip_serializing)]
    pub config: Value,
    #[serde(skip_serializing)]
    pub config_enc: Option<Vec<u8>>,
    #[serde(skip_serializing)]
    pub secret_enc: Option<Vec<u8>>,
    pub enabled: bool,
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Insert shape for a channel. There is no `config` field on purpose: new rows
/// only ever carry ciphertext, and the column's `DEFAULT '{}'::jsonb` supplies
/// the blank legacy value. Omitting it makes "write plaintext" unspellable
/// rather than merely discouraged.
#[derive(Debug, Insertable)]
#[diesel(table_name = notification_channels)]
pub struct NewNotificationChannel<'a> {
    pub org_id: Uuid,
    pub name: &'a str,
    pub kind: &'a str,
    pub config_enc: Option<Vec<u8>>,
    pub secret_enc: Option<Vec<u8>>,
    pub created_by: Option<Uuid>,
}

/// An admin-defined trigger. `conditions` is a free-form bag interpreted per
/// `trigger_type` (threshold / comparator / window / filters / spike factor…).
#[derive(Debug, Clone, Queryable, Selectable, Serialize, utoipa::ToSchema)]
#[diesel(table_name = alert_rules)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct AlertRule {
    pub id: Uuid,
    pub org_id: Uuid,
    pub project_id: Option<Uuid>,
    pub app_id: Option<Uuid>,
    pub monitor_id: Option<Uuid>,
    pub name: String,
    pub trigger_type: String,
    pub enabled: bool,
    pub conditions: Value,
    pub severity: String,
    pub throttle_seconds: i32,
    pub message_template: Option<String>,
    pub last_evaluated_at: Option<DateTime<Utc>>,
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = alert_rules)]
pub struct NewAlertRule<'a> {
    pub org_id: Uuid,
    pub project_id: Option<Uuid>,
    pub app_id: Option<Uuid>,
    pub monitor_id: Option<Uuid>,
    pub name: &'a str,
    pub trigger_type: &'a str,
    pub conditions: &'a Value,
    pub severity: &'a str,
    pub throttle_seconds: i32,
    pub message_template: Option<&'a str>,
    pub last_evaluated_at: Option<DateTime<Utc>>,
    pub created_by: Option<Uuid>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize, utoipa::ToSchema)]
#[diesel(table_name = alert_events)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct AlertEventRow {
    pub id: Uuid,
    pub org_id: Uuid,
    pub rule_id: Option<Uuid>,
    pub channel_id: Option<Uuid>,
    pub trigger_type: String,
    pub dedup_key: String,
    pub status: String,
    pub title: String,
    pub body: String,
    pub error: Option<String>,
    pub attempts: i32,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = alert_events)]
pub struct NewAlertEvent<'a> {
    pub org_id: Uuid,
    pub rule_id: Option<Uuid>,
    pub channel_id: Option<Uuid>,
    pub trigger_type: &'a str,
    pub dedup_key: &'a str,
    pub status: &'a str,
    pub title: &'a str,
    pub body: &'a str,
    pub error: Option<&'a str>,
    pub attempts: i32,
}

// ---------------------------------------------------------------------------
// Transactional email outbox
// ---------------------------------------------------------------------------

/// One rendered, queued message.
///
/// Derives neither `Serialize` nor `Debug`, deliberately. No `Serialize`, so a
/// pending row's body cannot reach an API view struct by someone adding
/// `#[derive(Serialize)]` upstream. `QueryableByName` because the claim is a
/// `sql_query` with `RETURNING *`.
#[derive(Clone, Queryable, Selectable, QueryableByName)]
#[diesel(table_name = mail_outbox)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct MailOutbox {
    pub id: Uuid,
    pub kind: String,
    pub recipient: String,
    pub recipient_key: String,
    pub subject: String,
    pub body_text: String,
    pub body_html: String,
    pub status: String,
    pub attempts: i32,
    pub max_attempts: i32,
    pub next_attempt_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub last_error: Option<String>,
    pub user_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub sent_at: Option<DateTime<Utc>>,
}

impl std::fmt::Debug for MailOutbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // A pending body is a live credential — a working password-reset URL.
        // One `warn!(row = ?r, ...)` in the drain loop would otherwise write it
        // to the journal, where it outlives the row and reaches a broader reader
        // set than the database does. Same precedent as `SecretCipher`.
        f.debug_struct("MailOutbox")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("recipient", &self.recipient)
            .field("status", &self.status)
            .field("attempts", &self.attempts)
            .field("body_text", &"<redacted>")
            .field("body_html", &"<redacted>")
            .finish()
    }
}

/// Insert side of [`MailOutbox`]. `Insertable` only — a `Queryable` derive here
/// would decode positionally against a seventeen-column table and silently bind
/// `subject` into `recipient_key`.
#[derive(Insertable)]
#[diesel(table_name = mail_outbox)]
pub struct NewMailOutbox<'a> {
    pub kind: &'a str,
    pub recipient: &'a str,
    pub recipient_key: &'a str,
    pub subject: &'a str,
    pub body_text: &'a str,
    pub body_html: &'a str,
    pub user_id: Option<Uuid>,
}

// ---------------------------------------------------------------------------
// Personal notification subscriptions (S3)
// ---------------------------------------------------------------------------

/// One row per `(user, scope, kind)`. `scope_id` is polymorphic with no FK, so
/// a row can outlive its target and every read path must tolerate an
/// unresolvable id.
///
/// `QueryableByName` as well as `Queryable`, for the same reason
/// [`NotificationQueueItem`] carries it: `upsert_subscription` is one
/// data-modifying CTE ending in `SELECT * FROM up`, and `diesel::sql_query`
/// decodes by column NAME, which plain `Queryable` cannot do.
#[derive(Debug, Clone, Queryable, Selectable, QueryableByName, Serialize, utoipa::ToSchema)]
#[diesel(table_name = notification_subscriptions)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct NotificationSubscription {
    pub id: Uuid,
    pub user_id: Uuid,
    pub org_id: Uuid,
    pub scope_type: String,
    pub scope_id: Uuid,
    pub kind: String,
    pub enabled: bool,
    pub disabled_reason: Option<String>,
    pub disabled_at: Option<DateTime<Utc>>,
    pub conditions: Value,
    pub delivery: String,
    pub throttle_seconds: i32,
    pub quiet_start_min: Option<i16>,
    pub quiet_end_min: Option<i16>,
    pub quiet_tz: String,
    pub last_evaluated_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Insertable only. Deliberately NOT `Queryable`: that derive decodes
/// positionally, so a field order that drifts from the `table!` block would
/// bind values to the wrong columns without a compile error.
#[derive(Debug, Insertable)]
#[diesel(table_name = notification_subscriptions)]
pub struct NewNotificationSubscription<'a> {
    pub user_id: Uuid,
    pub org_id: Uuid,
    pub scope_type: &'a str,
    pub scope_id: Uuid,
    pub kind: &'a str,
    pub conditions: &'a Value,
    pub delivery: &'a str,
    pub throttle_seconds: i32,
    pub quiet_start_min: Option<i16>,
    pub quiet_end_min: Option<i16>,
    pub quiet_tz: &'a str,
}

/// `environment_id` here is a **catalogue** `environments.id`, never an
/// `app_environments` enrollment id.
#[derive(Debug, Clone, Queryable, Selectable, Insertable, Serialize, utoipa::ToSchema)]
#[diesel(table_name = notification_subscription_envs)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct NotificationSubscriptionEnv {
    pub subscription_id: Uuid,
    pub environment_id: Uuid,
}

/// `QueryableByName` as well as `Queryable`, because the drain's claim is a
/// `sql_query ... RETURNING *`.
#[derive(Debug, Clone, Queryable, Selectable, QueryableByName, Serialize, utoipa::ToSchema)]
#[diesel(table_name = notification_queue)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct NotificationQueueItem {
    pub id: Uuid,
    pub subscription_id: Uuid,
    pub user_id: Uuid,
    pub org_id: Uuid,
    pub project_id: Uuid,
    pub app_id: Option<Uuid>,
    pub includes_unattributed: bool,
    pub kind: String,
    pub dedup_key: String,
    pub severity: String,
    pub title: Option<String>,
    pub body: Option<String>,
    pub link: Option<String>,
    pub occurred_at: DateTime<Utc>,
    pub deliver_after: DateTime<Utc>,
    pub status: String,
    pub attempts: i16,
    pub message_id: Option<Uuid>,
    pub claimed_at: Option<DateTime<Utc>>,
    pub sent_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Insertable only, for the tests that seed the queue directly. The production
/// enqueue path is `repo::enqueue_notifications`, one data-modifying CTE.
#[derive(Debug, Insertable)]
#[diesel(table_name = notification_queue)]
pub struct NewNotificationQueueItem<'a> {
    pub subscription_id: Uuid,
    pub user_id: Uuid,
    pub org_id: Uuid,
    pub project_id: Uuid,
    pub app_id: Option<Uuid>,
    pub includes_unattributed: bool,
    pub kind: &'a str,
    pub dedup_key: &'a str,
    pub severity: &'a str,
    pub title: Option<&'a str>,
    pub body: Option<&'a str>,
    pub link: Option<&'a str>,
    pub deliver_after: DateTime<Utc>,
}

/// `environment_id` here is an **enrollment** `app_environments.id`.
#[derive(Debug, Clone, Queryable, Selectable, Insertable, Serialize, utoipa::ToSchema)]
#[diesel(table_name = notification_queue_envs)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct NotificationQueueEnv {
    pub queue_id: Uuid,
    pub environment_id: Uuid,
}

// --- PII inspector ----------------------------------------------------------

/// `QueryableByName` as well as `Queryable`: `claim_due_policies` and
/// `effective_policy_for_app` are raw `sql_query`s (the scheduling arithmetic
/// and the precedence `ORDER BY CASE` have no diesel DSL equivalent), and
/// `sql_query` loads by column NAME, which `Queryable` cannot do.
#[derive(Debug, Clone, Queryable, Selectable, Serialize, QueryableByName, utoipa::ToSchema)]
#[diesel(table_name = inspector_policies)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct InspectorPolicy {
    pub id: Uuid,
    pub org_id: Uuid,
    pub target_type: String,
    pub target_id: Uuid,
    pub enabled: bool,
    pub tracked_keys: Value,
    pub detectors: Value,
    pub scan_columns: Option<Value>,
    pub rollups: Value,
    pub window_days: i32,
    pub schedule_enabled: bool,
    pub schedule_days: i16,
    #[serde(serialize_with = "ser_time")]
    pub schedule_time: chrono::NaiveTime,
    pub schedule_tz: String,
    pub next_run_at: Option<DateTime<Utc>>,
    pub last_run_at: Option<DateTime<Utc>>,
    pub last_scan_id: Option<Uuid>,
    pub last_skip_reason: String,
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// `chrono::NaiveTime`'s default Serialize emits `03:00:00`; the dashboard's
/// `<input type="time">` round-trips `HH:MM`. Pinning the format here rather
/// than reformatting in three call sites keeps the wire shape single-sourced.
fn ser_time<S: serde::Serializer>(t: &chrono::NaiveTime, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&t.format("%H:%M").to_string())
}

#[derive(Debug, Insertable)]
#[diesel(table_name = inspector_policies)]
pub struct NewInspectorPolicy<'a> {
    pub org_id: Uuid,
    pub target_type: &'a str,
    pub target_id: Uuid,
    pub enabled: bool,
    pub tracked_keys: &'a Value,
    pub detectors: &'a Value,
    pub scan_columns: Option<&'a Value>,
    pub rollups: &'a Value,
    pub window_days: i32,
    pub schedule_enabled: bool,
    pub schedule_days: i16,
    pub schedule_time: chrono::NaiveTime,
    pub schedule_tz: &'a str,
    pub created_by: Option<Uuid>,
}

/// PATCH body lowered to a diesel changeset. Deliberately NOT `Queryable`:
/// `Insertable`/`AsChangeset` map by name, `Queryable` decodes positionally,
/// so adding it would silently bind each field to whatever column occupies
/// its index.
#[derive(Debug, Default, AsChangeset)]
#[diesel(table_name = inspector_policies)]
pub struct InspectorPolicyPatch<'a> {
    pub enabled: Option<bool>,
    pub tracked_keys: Option<&'a Value>,
    pub detectors: Option<&'a Value>,
    pub scan_columns: Option<Option<&'a Value>>,
    pub rollups: Option<&'a Value>,
    pub window_days: Option<i32>,
    pub schedule_enabled: Option<bool>,
    pub schedule_days: Option<i16>,
    pub schedule_time: Option<chrono::NaiveTime>,
    pub schedule_tz: Option<&'a str>,
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize, QueryableByName, utoipa::ToSchema)]
#[diesel(table_name = inspector_scans)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct InspectorScan {
    pub id: Uuid,
    pub policy_id: Uuid,
    pub org_id: Uuid,
    pub trigger_type: String,
    pub requested_by: Option<Uuid>,
    pub status: String,
    pub coverage: String,
    pub coverage_note: String,
    pub window_from: DateTime<Utc>,
    pub window_to: DateTime<Utc>,
    pub params: Value,
    pub targets: Value,
    pub units_total: i32,
    pub units_done: i32,
    pub cursor: Value,
    pub rows_scanned: i64,
    pub findings_count: i32,
    pub findings_reaped_at: Option<DateTime<Utc>>,
    pub worker_id: Option<String>,
    pub heartbeat_at: Option<DateTime<Utc>>,
    pub attempts: i32,
    pub cancel_requested_at: Option<DateTime<Utc>>,
    pub error: String,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = inspector_scans)]
pub struct NewInspectorScan<'a> {
    pub policy_id: Uuid,
    pub org_id: Uuid,
    pub trigger_type: &'a str,
    pub requested_by: Option<Uuid>,
    pub window_from: DateTime<Utc>,
    pub window_to: DateTime<Utc>,
    pub params: &'a Value,
    pub targets: &'a Value,
    pub units_total: i32,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize, utoipa::ToSchema)]
#[diesel(table_name = inspector_findings)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct InspectorFinding {
    pub id: Uuid,
    pub scan_id: Uuid,
    pub org_id: Uuid,
    pub app_id: Uuid,
    pub environment_id: Option<Uuid>,
    pub env_scope: String,
    pub source_table: String,
    pub source_column: String,
    pub key_path: String,
    pub matched_key: String,
    pub detector: String,
    pub value_type: String,
    pub match_count: i64,
    pub match_count_exact: bool,
    pub sample_preview: String,
    pub sample_row_id: Option<Uuid>,
    pub sample_occurred_at: Option<DateTime<Utc>>,
    pub partition_kind: String,
    pub first_seen_at: Option<DateTime<Utc>>,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize, QueryableByName, utoipa::ToSchema)]
#[diesel(table_name = inspector_mask_actions)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct InspectorMaskAction {
    pub id: Uuid,
    pub org_id: Uuid,
    pub app_id: Uuid,
    pub kind: String,
    pub finding_id: Option<Uuid>,
    pub scan_id: Option<Uuid>,
    pub targets: Value,
    pub status: String,
    pub requested_by: Option<Uuid>,
    pub requested_by_email: String,
    pub cancelled_by: Option<Uuid>,
    pub cancelled_by_email: String,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub requested_at: DateTime<Utc>,
    pub previewed_at: Option<DateTime<Utc>>,
    pub confirmed_at: Option<DateTime<Utc>>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub confirm_source: String,
    pub estimated_rows: i64,
    pub rows_scanned: i64,
    pub rows_masked: i64,
    pub cold_rows_skipped: i64,
    pub cold_boundary_at: Option<DateTime<Utc>>,
    pub day_cursor: Option<chrono::NaiveDate>,
    pub cursor_occurred_at: Option<DateTime<Utc>>,
    pub cursor_id: Option<Uuid>,
    pub phase: String,
    pub worker_id: Option<String>,
    pub claimed_at: Option<DateTime<Utc>>,
    pub vacuum_advised: bool,
    pub error: String,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = inspector_mask_actions)]
pub struct NewInspectorMaskAction<'a> {
    pub org_id: Uuid,
    pub app_id: Uuid,
    pub kind: &'a str,
    pub finding_id: Option<Uuid>,
    pub scan_id: Option<Uuid>,
    pub targets: &'a Value,
    pub requested_by: Option<Uuid>,
    pub requested_by_email: &'a str,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize, utoipa::ToSchema)]
#[diesel(table_name = inspector_masked_keys)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct InspectorMaskedKey {
    pub id: Uuid,
    pub app_id: Uuid,
    pub target_table: String,
    pub target_column: String,
    pub json_path: String,
    pub created_at: DateTime<Utc>,
    pub created_by: Option<Uuid>,
    pub source_action_id: Option<Uuid>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = inspector_masked_keys)]
pub struct NewInspectorMaskedKey<'a> {
    pub app_id: Uuid,
    pub target_table: &'a str,
    pub target_column: &'a str,
    pub json_path: &'a str,
    pub created_by: Option<Uuid>,
    pub source_action_id: Option<Uuid>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize, utoipa::ToSchema)]
#[diesel(table_name = inspector_reveal_audit)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct InspectorRevealAudit {
    pub id: Uuid,
    pub app_id: Uuid,
    pub org_id: Uuid,
    pub finding_id: Option<Uuid>,
    pub user_id: Option<Uuid>,
    pub user_email: String,
    pub source_table: String,
    pub source_column: String,
    pub key_path: String,
    pub request_source: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = inspector_reveal_audit)]
pub struct NewInspectorRevealAudit<'a> {
    pub app_id: Uuid,
    pub org_id: Uuid,
    pub finding_id: Option<Uuid>,
    pub user_id: Option<Uuid>,
    pub user_email: &'a str,
    pub source_table: &'a str,
    pub source_column: &'a str,
    pub key_path: &'a str,
    pub request_source: &'a str,
}

/// A range sauron-tier must not drop from Postgres, even though it is below the
/// export watermark and durable in Parquet. Created by a cold-data restore; see
/// the `tier_pins` migration for why a restore without one is undone on the very
/// next tier cycle.
#[derive(Debug, Clone, Queryable, Selectable, Serialize, utoipa::ToSchema)]
#[diesel(table_name = tier_pins)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct TierPin {
    pub id: Uuid,
    pub table_name: String,
    pub range_start: DateTime<Utc>,
    pub range_end: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub created_by: Option<Uuid>,
    pub reason: Option<String>,
}

/// One cold-data restore, from request to completion.
///
/// Carries the claim/heartbeat/attempts trio so the executor survives a crash:
/// a `running` row whose heartbeat has lapsed is re-claimable, and the re-claim
/// deletes the job's own partial output (by `pin_id`) before re-inserting. That
/// is what keeps a resumed restore from duplicating rows.
/// `QueryableByName` as well as `Queryable`: the claim goes through
/// `sql_query(... RETURNING *)`, which loads by column name rather than by
/// position. Same reason `InspectorScan` carries both.
#[derive(Debug, Clone, Queryable, Selectable, Serialize, QueryableByName, utoipa::ToSchema)]
#[diesel(table_name = restore_jobs)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct RestoreJob {
    pub id: Uuid,
    pub table_name: String,
    /// `None` restores every app in the range.
    pub app_id: Option<Uuid>,
    pub range_start: DateTime<Utc>,
    pub range_end: DateTime<Utc>,
    pub status: String,
    /// Nulled when the pin is purged at expiry — the job history outlives the
    /// restored data, so `pin_expires_at` is kept separately.
    pub pin_id: Option<Uuid>,
    pub pin_expires_at: DateTime<Utc>,
    pub rows_estimated: i64,
    pub rows_restored: i64,
    pub worker_id: Option<String>,
    pub heartbeat_at: Option<DateTime<Utc>>,
    pub attempts: i32,
    pub error: String,
    pub requested_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
}

/// One app's credentials and sync bookkeeping for one store.
///
/// Derives **no** `Serialize`, deliberately: `secret_enc` is the AES-GCM
/// credential, and the API's response type is a separate struct that carries
/// only `has_secret`. Returning this row from a handler is therefore a compile
/// error rather than a credential leak.
///
/// `identifiers` is public, displayable configuration whose shape depends on
/// `store`; `sync_state` is connector-private bookkeeping (for Apple, the id of
/// the ongoing `analyticsReportRequest`, created once and reused).
// `QueryableByName` is required by `claim_due_store_connections`, which is raw
// `sql_query` (FOR UPDATE SKIP LOCKED has no query-builder equivalent). Still
// no `Serialize` — that is the compile-time half of "secrets are write-only".
#[derive(Debug, Clone, Queryable, Selectable, QueryableByName)]
#[diesel(table_name = app_store_connections)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct AppStoreConnection {
    pub id: Uuid,
    pub app_id: Uuid,
    pub store: String,
    pub enabled: bool,
    pub identifiers: Value,
    pub secret_enc: Option<Vec<u8>>,
    pub sync_state: Value,
    pub next_sync_at: DateTime<Utc>,
    pub last_synced_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One store's counts for one calendar day.
///
/// No `environment_id`: the stores key their data to a package name or bundle
/// id and have no environment dimension to report. See migration 49.
#[derive(Debug, Clone, Queryable, Selectable, Serialize, utoipa::ToSchema)]
#[diesel(table_name = store_daily_metrics)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct StoreDailyMetric {
    pub app_id: Uuid,
    pub store: String,
    pub day: chrono::NaiveDate,
    pub installs: i64,
    pub uninstalls: i64,
    pub updated_at: DateTime<Utc>,
}

/// One recorded administrative action — the Wall of Shame's row type.
///
/// `actor_email`, `entity_name`, `project_name`, `app_name` and
/// `environment_name` are snapshots, not joins. Every id on this row is
/// `ON DELETE SET NULL` (or, for `environment_id`, unconstrained), so the
/// names are the only thing that keeps an entry readable once the thing it
/// describes has been deleted — which is exactly when the trail is consulted.
#[derive(Debug, Clone, Queryable, Selectable, Serialize, utoipa::ToSchema)]
#[diesel(table_name = audit_log)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct AuditLogEntry {
    pub id: Uuid,
    pub org_id: Uuid,
    pub actor_id: Option<Uuid>,
    pub actor_email: String,
    pub action: String,
    pub entity_type: String,
    pub entity_id: Option<Uuid>,
    pub entity_name: String,
    pub project_id: Option<Uuid>,
    pub project_name: String,
    pub app_id: Option<Uuid>,
    pub app_name: String,
    pub environment_id: Option<Uuid>,
    pub environment_name: String,
    /// `{field: {from, to}}`, changed fields only. Populated from a per-entity
    /// allowlist in `sauron-api::audit`; never from serializing an entity
    /// wholesale, so secrets cannot reach a table org admins can read.
    pub changes: Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = audit_log)]
pub struct NewAuditLogEntry<'a> {
    pub org_id: Uuid,
    pub actor_id: Option<Uuid>,
    pub actor_email: &'a str,
    pub action: &'a str,
    pub entity_type: &'a str,
    pub entity_id: Option<Uuid>,
    pub entity_name: &'a str,
    pub project_id: Option<Uuid>,
    pub project_name: &'a str,
    pub app_id: Option<Uuid>,
    pub app_name: &'a str,
    pub environment_id: Option<Uuid>,
    pub environment_name: &'a str,
    pub changes: Value,
}

// ===========================================================================
// Ingest failure recovery
// ===========================================================================

/// One *kind* of ingest failure, not one occurrence.
///
/// 242,700 identical malformed payloads are one row here with
/// `occurrences = 242_700`. The individual payloads live in
/// [`IngestFailurePayload`], capped per group — grouping alone would reduce
/// "retry" to replaying a single sample, which verifies a fix but recovers
/// nothing.
///
/// `org_id`, `project_id` and `app_id` carry no foreign key and are nullable,
/// for the same reasons as [`AuditLogEntry`]: the row is an inert snapshot that
/// must outlive the app it describes, and the dominant failure mode is a
/// payload that never decoded, so there is no app to point at.
#[derive(Debug, Clone, Queryable, Selectable, Serialize, utoipa::ToSchema)]
#[diesel(table_name = ingest_failures)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct IngestFailure {
    pub id: Uuid,
    pub fingerprint: String,
    /// Low-cardinality slug (`decode`, `db_deadlock`, …). Also a metrics label,
    /// which is why the raw message is a separate column.
    pub error_kind: String,
    pub error_message: String,
    pub org_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub app_id: Option<Uuid>,
    /// Everything ever seen for this fingerprint, including occurrences the
    /// payload cap refused to store.
    ///
    /// The only counter on the row. Retained and dropped counts are derived
    /// (see [`IngestFailureRow`]) rather than denormalized, because bumping
    /// them would require updating this row twice in one statement — which
    /// Postgres silently declines to do.
    pub occurrences: i64,
    /// `failed` | `requeued` | `resolved`.
    pub status: String,
    pub first_seen_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
}

/// An [`IngestFailure`] as the admin page reads it, with the two counts the
/// table deliberately does not store.
///
/// `retained` is `COUNT(children)` and `dropped` is `occurrences - retained`.
/// Deriving beats denormalizing here: the alternative is bumping counter
/// columns in the same statement as the fingerprint upsert, and Postgres will
/// not update one row twice in a single statement — the bump would silently
/// not apply and the counters would drift while every test still passed.
///
/// `dropped` is rendered wherever it is non-zero. Silent truncation that reads
/// as full coverage is the specific bug class this page exists to expose, so
/// the number is never hidden behind a tooltip or an expander.
#[derive(Debug, Clone, QueryableByName, Serialize, utoipa::ToSchema)]
pub struct IngestFailureRow {
    #[diesel(sql_type = diesel::sql_types::Uuid)]
    pub id: Uuid,
    #[diesel(sql_type = diesel::sql_types::Text)]
    pub fingerprint: String,
    #[diesel(sql_type = diesel::sql_types::Text)]
    pub error_kind: String,
    #[diesel(sql_type = diesel::sql_types::Text)]
    pub error_message: String,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Uuid>)]
    pub org_id: Option<Uuid>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Uuid>)]
    pub project_id: Option<Uuid>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Uuid>)]
    pub app_id: Option<Uuid>,
    /// Denormalized like `audit_log`'s name columns: the row must stay readable
    /// after its app is deleted, which is often when it is finally read.
    #[diesel(sql_type = diesel::sql_types::Text)]
    pub app_name: String,
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    pub occurrences: i64,
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    pub retained: i64,
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    pub dropped: i64,
    #[diesel(sql_type = diesel::sql_types::Text)]
    pub status: String,
    #[diesel(sql_type = diesel::sql_types::Timestamptz)]
    pub first_seen_at: DateTime<Utc>,
    #[diesel(sql_type = diesel::sql_types::Timestamptz)]
    pub last_seen_at: DateTime<Utc>,
}

/// Status values for [`IngestFailure::status`], as the one place they are spelt.
pub mod ingest_failure_status {
    /// Terminal until a human acts on it.
    pub const FAILED: &str = "failed";
    /// Re-injected onto the ingest stream; awaiting the worker's verdict.
    pub const REQUEUED: &str = "requeued";
    /// Every retained payload was replayed successfully.
    pub const RESOLVED: &str = "resolved";
}

#[derive(Debug, Insertable)]
#[diesel(table_name = ingest_failures)]
pub struct NewIngestFailure<'a> {
    pub fingerprint: &'a str,
    pub error_kind: &'a str,
    pub error_message: &'a str,
    pub org_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub app_id: Option<Uuid>,
}

/// One retained, **already PII-masked** payload behind an [`IngestFailure`].
///
/// `mask::apply_wire` runs in the worker before anything is persisted or
/// re-queued, so this is never the raw wire payload. It is still a copy of a
/// real user event, which is why the retention reaper exists.
#[derive(Debug, Clone, Queryable, Selectable, Serialize, utoipa::ToSchema)]
#[diesel(table_name = ingest_failure_payloads)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct IngestFailurePayload {
    pub id: Uuid,
    pub failure_id: Uuid,
    pub payload: Value,
    /// Retries burned before this landed here. Always 0 for a permanent
    /// failure, which is never retried by design.
    pub attempts: i32,
    pub created_at: DateTime<Utc>,
    /// Set when re-injected, cleared if that attempt fails. This is what closes
    /// the manual-retry loop.
    pub requeued_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = ingest_failure_payloads)]
pub struct NewIngestFailurePayload {
    pub failure_id: Uuid,
    pub payload: Value,
    pub attempts: i32,
}

/// One admin data-purge job: the queue entry, the frozen scope, the resume
/// cursor, the progress meter and the record of who did it.
///
/// Field order MUST match `schema::purge_jobs` exactly — `Queryable` decodes
/// POSITIONALLY, so a field inserted in the middle silently binds every later
/// column to the wrong one. Append only.
#[derive(Debug, Clone, Queryable, Selectable, Serialize, QueryableByName, utoipa::ToSchema)]
#[diesel(table_name = purge_jobs)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct PurgeJob {
    pub id: Uuid,
    pub org_id: Uuid,
    pub app_id: Uuid,
    pub app_slug: String,
    pub app_name: String,
    /// `None` = every environment, including unattributed. `Some([])` is a
    /// scope that matches nothing and is refused at the API.
    pub environment_ids: Option<Value>,
    pub kinds: Value,
    pub range_start: Option<DateTime<Utc>>,
    pub range_end: Option<DateTime<Utc>>,
    pub all_time: bool,
    pub status: String,
    pub phase: String,
    pub estimated_counts: Value,
    pub deleted_counts: Value,
    pub rollups_recomputed: i64,
    pub rollups_deleted: i64,
    pub cold_rows_skipped: i64,
    pub cold_boundary_at: Option<DateTime<Utc>>,
    pub kind_cursor: Option<String>,
    pub cursor_occurred_at: Option<DateTime<Utc>>,
    pub cursor_id: Option<Uuid>,
    pub requested_by: Option<Uuid>,
    pub requested_by_email: String,
    pub cancelled_by: Option<Uuid>,
    pub cancelled_by_email: String,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub requested_at: DateTime<Utc>,
    pub previewed_at: Option<DateTime<Utc>>,
    pub confirmed_at: Option<DateTime<Utc>>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub confirm_source: String,
    pub ingest_active: bool,
    pub worker_id: Option<String>,
    pub claimed_at: Option<DateTime<Utc>>,
    pub error: String,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = purge_jobs)]
pub struct NewPurgeJob<'a> {
    pub org_id: Uuid,
    pub app_id: Uuid,
    pub app_slug: &'a str,
    pub app_name: &'a str,
    pub environment_ids: Option<Value>,
    pub kinds: Value,
    pub range_start: Option<DateTime<Utc>>,
    pub range_end: Option<DateTime<Utc>>,
    pub all_time: bool,
    pub requested_by: Option<Uuid>,
    pub requested_by_email: &'a str,
}

#[cfg(test)]
mod tests {
    use super::mask_ip;

    #[test]
    fn masks_ipv4_to_slash_24() {
        assert_eq!(mask_ip("203.0.113.42"), "203.0.113.0/24");
        assert_eq!(mask_ip(" 8.8.8.8 "), "8.8.8.0/24");
    }

    #[test]
    fn masks_ipv6_to_slash_48() {
        assert_eq!(mask_ip("2606:4700:4700::1111"), "2606:4700:4700::/48");
    }

    #[test]
    fn unparseable_is_not_echoed_back() {
        // Never reflect arbitrary stored text into the response.
        assert_eq!(mask_ip("not-an-ip"), "invalid");
        assert_eq!(mask_ip("<script>"), "invalid");
    }
}

#[cfg(test)]
mod openapi_schema_tests {
    use utoipa::PartialSchema;

    /// `User::password_hash` and `User::credentials_invalidated_at` carry
    /// `#[serde(skip_serializing)]`, so they are never present in a response
    /// body. The OpenAPI schema is derived by a *different* macro than the one
    /// that honours those attributes at runtime, and a schema that advertises
    /// `password_hash` on the object returned by `/v1/me` would be a published
    /// claim that Sauron hands out password hashes.
    ///
    /// Serde-attribute handling is exactly the kind of thing a dependency bump
    /// changes quietly, so this asserts the property rather than trusting it.
    #[test]
    fn user_schema_omits_fields_that_are_never_serialized() {
        let schema =
            serde_json::to_string(&super::User::schema()).expect("User schema should serialize");

        assert!(
            !schema.contains("password_hash"),
            "the derived OpenAPI schema for `User` advertises `password_hash`, \
             which `#[serde(skip_serializing)]` guarantees is never in a \
             response. Fix the derive (e.g. `#[schema(ignore)]`) — do not \
             delete this test. Schema was: {schema}"
        );
        assert!(
            !schema.contains("credentials_invalidated_at"),
            "the derived OpenAPI schema for `User` advertises \
             `credentials_invalidated_at`, which is never serialized. \
             Schema was: {schema}"
        );

        // Guards the assertions above against passing vacuously: if the schema
        // ever stopped containing any fields at all, the two `!contains`
        // checks would still succeed while documenting nothing.
        assert!(
            schema.contains("email") && schema.contains("is_active"),
            "expected the `User` schema to describe its serialized fields; \
             got: {schema}"
        );
    }
}
