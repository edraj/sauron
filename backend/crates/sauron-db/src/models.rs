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

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
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

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
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

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
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

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
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
#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
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
#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
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
#[derive(Debug, Clone, Serialize)]
pub struct AppEnvironmentView {
    #[serde(flatten)]
    pub enrollment: AppEnvironment,
    pub name: String,
}

/// Everything the ingest edge needs after presenting a key: the environment it
/// belongs to, its ancestry, and both ingest switches. Cached in Redis as JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
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

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
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

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
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

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
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
#[derive(Debug, Insertable)]
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
}

// ---------------------------------------------------------------------------
// Analytics events & people (keyed by app_id)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
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

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
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
}

#[derive(Debug, Insertable)]
#[diesel(table_name = refresh_tokens)]
pub struct NewRefreshToken {
    pub user_id: Uuid,
    pub token_hash: String,
    pub expires_at: DateTime<Utc>,
    pub user_agent: Option<String>,
}

// ---------------------------------------------------------------------------
// Sessions & devices (roll-ups materialized by the pipeline, keyed by app_id)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
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
    pub context: Value,
    pub release: Option<String>,
    pub environment_id: Option<Uuid>,
    #[serde(serialize_with = "serialize_masked_ip")]
    pub ip_address: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
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

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
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

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
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
}

// ---------------------------------------------------------------------------
// Monitors (uptime checks, keyed by project_id)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, QueryableByName, Serialize)]
#[diesel(table_name = monitors)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Monitor {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub kind: String,
    pub target: String,
    pub method: String,
    pub config: serde_json::Value,
    pub interval_seconds: i32,
    pub timeout_ms: i32,
    pub failure_threshold: i32,
    pub recovery_threshold: i32,
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

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
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

/// A configured delivery destination. `secret_enc` holds the AES-GCM ciphertext
/// of the channel's secret bundle and is NEVER serialized to API clients.
#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = notification_channels)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct NotificationChannel {
    pub id: Uuid,
    pub org_id: Uuid,
    pub name: String,
    pub kind: String,
    pub config: Value,
    #[serde(skip_serializing)]
    pub secret_enc: Option<Vec<u8>>,
    pub enabled: bool,
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = notification_channels)]
pub struct NewNotificationChannel<'a> {
    pub org_id: Uuid,
    pub name: &'a str,
    pub kind: &'a str,
    pub config: &'a Value,
    pub secret_enc: Option<Vec<u8>>,
    pub created_by: Option<Uuid>,
}

/// An admin-defined trigger. `conditions` is a free-form bag interpreted per
/// `trigger_type` (threshold / comparator / window / filters / spike factor…).
#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = alert_rules)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct AlertRule {
    pub id: Uuid,
    pub org_id: Uuid,
    pub project_id: Option<Uuid>,
    pub app_id: Option<Uuid>,
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
    pub name: &'a str,
    pub trigger_type: &'a str,
    pub conditions: &'a Value,
    pub severity: &'a str,
    pub throttle_seconds: i32,
    pub message_template: Option<&'a str>,
    pub last_evaluated_at: Option<DateTime<Utc>>,
    pub created_by: Option<Uuid>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
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
