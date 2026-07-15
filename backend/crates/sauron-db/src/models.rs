//! Diesel row models (`Queryable`/`Selectable`) and insert structs.
//!
//! Hierarchy: `Organization → Project (grouping) → App (ingest unit) → signals`.
//! Access control: `Role` (permission bundle) + `RoleGrant` (user↔role at a
//! scope). Row structs derive `Serialize`; secrets are skipped.

use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use crate::schema::*;

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
    pub public_key: String,
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
    pub public_key: &'a str,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = environments)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Environment {
    pub id: Uuid,
    pub app_id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = environments)]
pub struct NewEnvironment<'a> {
    pub app_id: Uuid,
    pub name: &'a str,
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
}

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
    /// Ingest-time pre-symbolication (lean, no context); null when unresolved.
    pub stacktrace_symbolicated: Option<Value>,
    /// pending | symbolicated | partial | no_artifacts | not_applicable.
    pub symbolication_status: String,
    /// Dart debug header + verbatim trace (`{build_id,isolate_dso_base,arch,os,raw_stacktrace}`).
    pub debug_meta: Option<Value>,
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
    pub ip_address: Option<String>,
    pub occurred_at: DateTime<Utc>,
    pub received_at: DateTime<Utc>,
    pub device_key: Option<String>,
    pub screen: Option<String>,
}

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
    pub ip_address: Option<String>,
    pub occurred_at: DateTime<Utc>,
    pub received_at: DateTime<Utc>,
}

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
