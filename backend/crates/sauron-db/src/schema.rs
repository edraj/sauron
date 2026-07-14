// @generated automatically by Diesel CLI.

diesel::table! {
    analytics_events (id) {
        id -> Uuid,
        app_id -> Uuid,
        environment_id -> Nullable<Uuid>,
        name -> Text,
        distinct_id -> Text,
        properties -> Jsonb,
        context -> Jsonb,
        session_id -> Nullable<Text>,
        release -> Nullable<Text>,
        ip_address -> Nullable<Text>,
        occurred_at -> Timestamptz,
        received_at -> Timestamptz,
        device_key -> Nullable<Text>,
        screen -> Nullable<Text>,
    }
}

diesel::table! {
    apps (id) {
        id -> Uuid,
        name -> Text,
        slug -> Text,
        platform -> Nullable<Text>,
        public_key -> Text,
        ingest_enabled -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
        app_type -> Text,
        project_id -> Uuid,
    }
}

diesel::table! {
    environments (id) {
        id -> Uuid,
        app_id -> Uuid,
        name -> Text,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    error_events (id) {
        id -> Uuid,
        app_id -> Uuid,
        environment_id -> Nullable<Uuid>,
        issue_id -> Uuid,
        fingerprint -> Text,
        level -> Text,
        message -> Text,
        exception_type -> Text,
        exception_value -> Text,
        stacktrace -> Jsonb,
        breadcrumbs -> Jsonb,
        context -> Jsonb,
        tags -> Jsonb,
        release -> Nullable<Text>,
        distinct_id -> Nullable<Text>,
        event_user -> Nullable<Jsonb>,
        sdk -> Nullable<Jsonb>,
        ip_address -> Nullable<Text>,
        occurred_at -> Timestamptz,
        received_at -> Timestamptz,
        session_id -> Nullable<Text>,
        device_key -> Nullable<Text>,
        screen -> Nullable<Text>,
    }
}

diesel::table! {
    event_users (id) {
        id -> Uuid,
        app_id -> Uuid,
        distinct_id -> Text,
        properties -> Jsonb,
        first_seen -> Timestamptz,
        last_seen -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    identities (id) {
        id -> Uuid,
        app_id -> Uuid,
        alias_id -> Text,
        distinct_id -> Text,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    issues (id) {
        id -> Uuid,
        app_id -> Uuid,
        fingerprint -> Text,
        #[sql_name = "type"]
        type_ -> Text,
        title -> Text,
        culprit -> Text,
        level -> Text,
        status -> Text,
        first_seen -> Timestamptz,
        last_seen -> Timestamptz,
        times_seen -> Int8,
        users_seen -> Int8,
        assignee_id -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    organizations (id) {
        id -> Uuid,
        name -> Text,
        slug -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    projects (id) {
        id -> Uuid,
        org_id -> Uuid,
        name -> Text,
        slug -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    saved_funnels (id) {
        id -> Uuid,
        app_id -> Uuid,
        name -> Text,
        description -> Nullable<Text>,
        steps -> Jsonb,
        created_by -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    refresh_tokens (id) {
        id -> Uuid,
        user_id -> Uuid,
        token_hash -> Text,
        expires_at -> Timestamptz,
        revoked_at -> Nullable<Timestamptz>,
        user_agent -> Nullable<Text>,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    role_grants (id) {
        id -> Uuid,
        org_id -> Uuid,
        user_id -> Uuid,
        role_id -> Uuid,
        scope_type -> Text,
        scope_id -> Uuid,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    roles (id) {
        id -> Uuid,
        org_id -> Nullable<Uuid>,
        name -> Text,
        description -> Text,
        is_system -> Bool,
        permissions -> Jsonb,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    users (id) {
        id -> Uuid,
        email -> Text,
        password_hash -> Text,
        name -> Text,
        last_login_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    sessions (id) {
        id -> Uuid,
        app_id -> Uuid,
        session_id -> Text,
        distinct_id -> Nullable<Text>,
        device_key -> Nullable<Text>,
        started_at -> Timestamptz,
        last_event_at -> Timestamptz,
        events_count -> Int8,
        errors_count -> Int8,
        context -> Jsonb,
        release -> Nullable<Text>,
        environment_id -> Nullable<Uuid>,
        ip_address -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    devices (id) {
        id -> Uuid,
        app_id -> Uuid,
        device_key -> Text,
        family -> Nullable<Text>,
        model -> Nullable<Text>,
        os_name -> Nullable<Text>,
        os_version -> Nullable<Text>,
        arch -> Nullable<Text>,
        browser -> Nullable<Text>,
        last_distinct_id -> Nullable<Text>,
        first_seen -> Timestamptz,
        last_seen -> Timestamptz,
        events_count -> Int8,
        errors_count -> Int8,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    transactions (id) {
        id -> Uuid,
        app_id -> Uuid,
        environment_id -> Nullable<Uuid>,
        name -> Text,
        op -> Text,
        duration_ms -> Float8,
        status -> Nullable<Text>,
        http_method -> Nullable<Text>,
        http_status -> Nullable<Int4>,
        url -> Nullable<Text>,
        distinct_id -> Nullable<Text>,
        session_id -> Nullable<Text>,
        device_key -> Nullable<Text>,
        release -> Nullable<Text>,
        ip_address -> Nullable<Text>,
        occurred_at -> Timestamptz,
        received_at -> Timestamptz,
    }
}

diesel::table! {
    monitors (id) {
        id -> Uuid,
        project_id -> Uuid,
        name -> Text,
        kind -> Text,
        target -> Text,
        method -> Text,
        config -> Jsonb,
        interval_seconds -> Int4,
        timeout_ms -> Int4,
        failure_threshold -> Int4,
        recovery_threshold -> Int4,
        webhook_url -> Nullable<Text>,
        enabled -> Bool,
        status -> Text,
        consecutive_failures -> Int4,
        consecutive_successes -> Int4,
        last_checked_at -> Nullable<Timestamptz>,
        next_check_at -> Timestamptz,
        last_status_changed_at -> Nullable<Timestamptz>,
        created_by -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    monitor_checks (id) {
        id -> Uuid,
        monitor_id -> Uuid,
        checked_at -> Timestamptz,
        up -> Bool,
        status_code -> Nullable<Int4>,
        response_time_ms -> Nullable<Int4>,
        error -> Nullable<Text>,
    }
}

diesel::table! {
    monitor_incidents (id) {
        id -> Uuid,
        monitor_id -> Uuid,
        started_at -> Timestamptz,
        resolved_at -> Nullable<Timestamptz>,
        cause -> Text,
        last_error -> Nullable<Text>,
    }
}

diesel::joinable!(analytics_events -> apps (app_id));
diesel::joinable!(analytics_events -> environments (environment_id));
diesel::joinable!(sessions -> apps (app_id));
diesel::joinable!(devices -> apps (app_id));
diesel::joinable!(transactions -> apps (app_id));
diesel::joinable!(apps -> projects (project_id));
diesel::joinable!(environments -> apps (app_id));
diesel::joinable!(error_events -> apps (app_id));
diesel::joinable!(error_events -> environments (environment_id));
diesel::joinable!(error_events -> issues (issue_id));
diesel::joinable!(event_users -> apps (app_id));
diesel::joinable!(identities -> apps (app_id));
diesel::joinable!(issues -> apps (app_id));
diesel::joinable!(issues -> users (assignee_id));
diesel::joinable!(projects -> organizations (org_id));
diesel::joinable!(refresh_tokens -> users (user_id));
diesel::joinable!(role_grants -> organizations (org_id));
diesel::joinable!(role_grants -> roles (role_id));
diesel::joinable!(role_grants -> users (user_id));
diesel::joinable!(roles -> organizations (org_id));
diesel::joinable!(monitors -> projects (project_id));
diesel::joinable!(monitor_checks -> monitors (monitor_id));
diesel::joinable!(monitor_incidents -> monitors (monitor_id));

diesel::allow_tables_to_appear_in_same_query!(
    analytics_events,
    apps,
    environments,
    error_events,
    event_users,
    identities,
    issues,
    organizations,
    projects,
    refresh_tokens,
    role_grants,
    roles,
    users,
    sessions,
    devices,
    transactions,
    saved_funnels,
    monitors,
    monitor_checks,
    monitor_incidents,
);
