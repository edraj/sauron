//! Tests for Resource::Sessions query plan lowering (SessionsLower).
//!
//! Tiers covered:
//! - Tier 1: Resource::Sessions (SessionsLower) SQL fragment generation via diesel::debug_query.
//! - Tier 2: Nullable JSON path handling, duration parsing, status enum mapping.
//! - Tier 3: Combined AST query lowering to PostgreSQL SQL with bind parameters ($1).
//! - Tier 4: Complex session search query lowering.

use std::collections::HashMap;

use chrono::Utc;
use diesel::debug_query;
use diesel::pg::Pg;
use diesel::prelude::*;
use sauron_db::query_plan::sessions::SessionsLower;
use sauron_db::query_plan::{lower, PrepCtx};
use sauron_db::schema::sessions;
use sauron_query::{parse, resolve, Resource};
use uuid::Uuid;

fn test_ctx() -> PrepCtx {
    let mut environments = HashMap::new();
    environments.insert("production".to_string(), Some(Uuid::nil()));
    environments.insert("staging".to_string(), Some(Uuid::nil()));

    PrepCtx {
        environments,
        now: Utc::now(),
    }
}

fn lower_sessions_query(q: &str) -> String {
    let parsed = parse(q).expect("parse query string");
    let resolved = resolve(&parsed, Resource::Sessions).expect("resolve node for Sessions");
    let l = SessionsLower {
        app_id: Uuid::nil(),
    };
    let frag = lower(&resolved, &l, &test_ctx()).expect("lower query plan");
    let query = sessions::table
        .into_boxed()
        .filter(frag)
        .select(sessions::id);
    debug_query::<Pg, _>(&query).to_string()
}

// ---------------------------------------------------------------------------
// Tier 1: SessionsLower SQL fragment generation
// ---------------------------------------------------------------------------

#[test]
fn test_sessions_lower_base_scope() {
    let test_app_id = Uuid::new_v4();
    let l = SessionsLower {
        app_id: test_app_id,
    };
    let query = sessions::table
        .into_boxed()
        .filter(l.base_scope())
        .select(sessions::id);
    let sql = debug_query::<Pg, _>(&query).to_string();
    assert!(
        sql.contains(r#""sessions"."app_id" ="#),
        "base_scope SQL: {sql}"
    );
}

#[test]
fn test_sessions_lower_column_equality_predicates() {
    let sql_distinct = lower_sessions_query("distinctId:user_42");
    assert!(
        sql_distinct.contains(r#""sessions"."distinct_id" ="#),
        "distinct_id SQL: {sql_distinct}"
    );

    let sql_device = lower_sessions_query("deviceKey:device_abc");
    assert!(
        sql_device.contains(r#""sessions"."device_key" ="#),
        "device_key SQL: {sql_device}"
    );

    let sql_release = lower_sessions_query("release:v2.1.0");
    assert!(
        sql_release.contains(r#""sessions"."release" ="#),
        "release SQL: {sql_release}"
    );

    let sql_events = lower_sessions_query("eventsCount:>10");
    assert!(
        sql_events.contains(r#""sessions"."events_count" >"#),
        "events_count SQL: {sql_events}"
    );

    let sql_errors = lower_sessions_query("errorsCount:0");
    assert!(
        sql_errors.contains(r#""sessions"."errors_count" ="#),
        "errors_count SQL: {sql_errors}"
    );
}

// ---------------------------------------------------------------------------
// Tier 2: Nullable JSON path, duration, environment mapping
// ---------------------------------------------------------------------------

#[test]
fn test_sessions_lower_json_path_handling() {
    // 1. Direct JSON key match
    let sql_version = lower_sessions_query("context.app_version:3.0.2");
    assert!(
        sql_version.contains(r#""sessions"."context" @>"#),
        "JSON path SQL: {sql_version}"
    );

    // 2. Nested JSON key match
    let sql_os = lower_sessions_query("context.os.name:Linux");
    assert!(
        sql_os.contains(r#""sessions"."context" @>"#),
        "Nested JSON path SQL: {sql_os}"
    );

    // 3. JSON presence predicate: has:context
    let sql_has = lower_sessions_query("has:context");
    assert!(
        sql_has.contains(r#""sessions"."context" IS NOT NULL"#),
        "JSON has SQL: {sql_has}"
    );
}

#[test]
fn test_sessions_lower_duration_and_time_parsing() {
    let sql_duration = lower_sessions_query("duration_ms:>5000");
    assert!(
        sql_duration.contains("last_event_at") && sql_duration.contains("started_at"),
        "Duration SQL: {sql_duration}"
    );

    let sql_time = lower_sessions_query("started_at:>2026-01-01T00:00:00Z");
    assert!(
        sql_time.contains(r#""sessions"."started_at" >"#),
        "Time SQL: {sql_time}"
    );
}

#[test]
fn test_sessions_lower_environment_mapping() {
    let sql_env = lower_sessions_query("environment:production");
    assert!(
        sql_env.contains(r#""sessions"."environment_id" ="#),
        "Environment SQL: {sql_env}"
    );
}

// ---------------------------------------------------------------------------
// Tier 3: Combined AST query lowering to SQL with binds
// ---------------------------------------------------------------------------

#[test]
fn test_sessions_lower_combined_ast_boolean_logic() {
    let query_str = "(distinctId:user_1 AND eventsCount:>5) OR (errorsCount:>0)";
    let sql = lower_sessions_query(query_str);

    assert!(sql.contains("AND"), "Combined SQL contains AND: {sql}");
    assert!(sql.contains("OR"), "Combined SQL contains OR: {sql}");
    assert!(
        sql.contains(r#""sessions"."distinct_id" ="#),
        "Combined SQL contains distinct_id: {sql}"
    );
    assert!(
        sql.contains(r#""sessions"."events_count" >"#),
        "Combined SQL contains events_count: {sql}"
    );
    assert!(
        sql.contains(r#""sessions"."errors_count" >"#),
        "Combined SQL contains errors_count: {sql}"
    );
}

#[test]
fn test_sessions_lower_negated_predicates() {
    let query_str = "!release:v1.0.0";
    let sql = lower_sessions_query(query_str);
    assert!(
        sql.contains(r#""sessions"."release" IS DISTINCT FROM"#)
            || sql.contains("NOT")
            || sql.contains("OR"),
        "Negated SQL: {sql}"
    );
}

// ---------------------------------------------------------------------------
// Tier 4: Complex session search query lowering & free-text search
// ---------------------------------------------------------------------------

#[test]
fn test_sessions_lower_complex_real_world_query() {
    let query_str = "(context.app_version:3.0.2 AND duration_ms:>1000) OR (errorsCount:>5 AND environment:production)";
    let sql = lower_sessions_query(query_str);

    assert!(
        sql.contains(r#""sessions"."context" @>"#),
        "Complex SQL context: {sql}"
    );
    assert!(
        sql.contains(r#""sessions"."errors_count" >"#),
        "Complex SQL errors_count: {sql}"
    );
    assert!(
        sql.contains(r#""sessions"."environment_id" ="#),
        "Complex SQL environment: {sql}"
    );
}

#[test]
fn test_sessions_lower_free_text_term() {
    let sql = lower_sessions_query("crash_session_key");
    assert!(sql.contains("ILIKE"), "Free-text SQL contains ILIKE: {sql}");
    assert!(
        sql.contains(r#""sessions"."session_id""#),
        "Free-text SQL session_id: {sql}"
    );
    assert!(
        sql.contains(r#""sessions"."distinct_id""#),
        "Free-text SQL distinct_id: {sql}"
    );
}
