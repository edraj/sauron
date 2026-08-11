// @generated automatically by Diesel CLI.

diesel::table! {
    auth_sessions (id) {
        id -> Uuid,
        user_id -> Uuid,
        created_at -> Timestamptz,
        last_used_at -> Timestamptz,
        expires_at -> Timestamptz,
        user_agent -> Nullable<Text>,
        ip -> Nullable<Text>,
        revoked_at -> Nullable<Timestamptz>,
        revoked_reason -> Nullable<Text>,
        revoked_by -> Nullable<Uuid>,
    }
}

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
        tags -> Jsonb,
        contexts -> Jsonb,
        extra -> Jsonb,
        workflow_id -> Nullable<Text>,
        workflow_name -> Nullable<Text>,
        restored_pin_id -> Nullable<Uuid>,
    }
}

diesel::table! {
    apps (id) {
        id -> Uuid,
        name -> Text,
        slug -> Text,
        platform -> Nullable<Text>,
        ingest_enabled -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
        app_type -> Text,
        project_id -> Uuid,
        store_environment_id -> Nullable<Uuid>,
    }
}

diesel::table! {
    app_store_connections (id) {
        id -> Uuid,
        app_id -> Uuid,
        store -> Text,
        enabled -> Bool,
        identifiers -> Jsonb,
        secret_enc -> Nullable<Bytea>,
        sync_state -> Jsonb,
        next_sync_at -> Timestamptz,
        last_synced_at -> Nullable<Timestamptz>,
        last_error -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    store_daily_metrics (app_id, store, day) {
        app_id -> Uuid,
        store -> Text,
        day -> Date,
        installs -> BigInt,
        uninstalls -> BigInt,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    app_environments (id) {
        id -> Uuid,
        app_id -> Uuid,
        created_at -> Timestamptz,
        public_key -> Text,
        ingest_enabled -> Bool,
        is_default -> Bool,
        retired_at -> Nullable<Timestamptz>,
        updated_at -> Timestamptz,
        environment_id -> Uuid,
    }
}

diesel::table! {
    environments (id) {
        id -> Uuid,
        project_id -> Uuid,
        name -> Text,
        retired_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
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
        stacktrace_symbolicated -> Nullable<Jsonb>,
        symbolication_status -> Text,
        debug_meta -> Nullable<Jsonb>,
        contexts -> Jsonb,
        extra -> Jsonb,
        handled -> Nullable<Bool>,
        title -> Nullable<Text>,
        culprit -> Nullable<Text>,
        workflow_id -> Nullable<Text>,
        workflow_name -> Nullable<Text>,
        restored_pin_id -> Nullable<Uuid>,
    }
}

diesel::table! {
    symbol_blobs (sha256) {
        sha256 -> Bytea,
        content -> Bytea,
        uncompressed_size -> Int8,
        compressed_size -> Int8,
        refcount -> Int4,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    symbol_artifacts (id) {
        id -> Uuid,
        app_id -> Uuid,
        kind -> Text,
        platform -> Text,
        arch -> Nullable<Text>,
        release -> Nullable<Text>,
        dist -> Nullable<Text>,
        name -> Nullable<Text>,
        debug_id -> Nullable<Text>,
        blob_sha256 -> Bytea,
        prebuilt_index_sha256 -> Nullable<Bytea>,
        uploaded_by -> Nullable<Uuid>,
        created_at -> Timestamptz,
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
        // Appended, never inserted mid-list: `models::EventUser` derives
        // `Queryable`, which decodes POSITIONALLY, and `ALTER TABLE … ADD
        // COLUMN` appends physically. A field inserted in the middle here
        // would silently bind every later column to the wrong one.
        identified_at -> Nullable<Timestamptz>,
        identified_source -> Nullable<Text>,
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
        last_event_at -> Timestamptz,
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
        revoked_reason -> Nullable<Text>,
        session_id -> Nullable<Uuid>,
    }
}

diesel::table! {
    password_reset_tokens (id) {
        id -> Uuid,
        user_id -> Uuid,
        token_hash -> Text,
        password_fingerprint -> Text,
        mode -> Text,
        initiated_by -> Nullable<Uuid>,
        requested_from -> Nullable<Text>,
        expires_at -> Timestamptz,
        consumed_at -> Nullable<Timestamptz>,
        invalidated_at -> Nullable<Timestamptz>,
        invalidated_reason -> Nullable<Text>,
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
        is_active -> Bool,
        must_change_password -> Bool,
        credentials_invalidated_at -> Nullable<Timestamptz>,
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
    workflows (id) {
        id -> Uuid,
        app_id -> Uuid,
        environment_id -> Uuid,
        workflow_id -> Text,
        name -> Text,
        session_id -> Nullable<Text>,
        distinct_id -> Nullable<Text>,
        device_key -> Nullable<Text>,
        release -> Nullable<Text>,
        status -> Text,
        cancel_reason -> Nullable<Text>,
        started_at -> Timestamptz,
        ended_at -> Nullable<Timestamptz>,
        last_event_at -> Timestamptz,
        events_count -> Int4,
        errors_count -> Int4,
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
        workflow_id -> Nullable<Text>,
        workflow_name -> Nullable<Text>,
        restored_pin_id -> Nullable<Uuid>,
        finished_at -> Nullable<Timestamptz>,
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

diesel::table! {
    tiering_state (table_name) {
        table_name -> Text,
        watermark -> Timestamptz,
        dropped_thru -> Nullable<Timestamptz>,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    notification_channels (id) {
        id -> Uuid,
        org_id -> Uuid,
        name -> Text,
        kind -> Text,
        config -> Jsonb,
        config_enc -> Nullable<Bytea>,
        secret_enc -> Nullable<Bytea>,
        enabled -> Bool,
        created_by -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    alert_rules (id) {
        id -> Uuid,
        org_id -> Uuid,
        project_id -> Nullable<Uuid>,
        app_id -> Nullable<Uuid>,
        monitor_id -> Nullable<Uuid>,
        name -> Text,
        trigger_type -> Text,
        enabled -> Bool,
        conditions -> Jsonb,
        severity -> Text,
        throttle_seconds -> Int4,
        message_template -> Nullable<Text>,
        last_evaluated_at -> Nullable<Timestamptz>,
        created_by -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    alert_rule_channels (rule_id, channel_id) {
        rule_id -> Uuid,
        channel_id -> Uuid,
    }
}

diesel::table! {
    alert_events (id) {
        id -> Uuid,
        org_id -> Uuid,
        rule_id -> Nullable<Uuid>,
        channel_id -> Nullable<Uuid>,
        trigger_type -> Text,
        dedup_key -> Text,
        status -> Text,
        title -> Text,
        body -> Text,
        error -> Nullable<Text>,
        attempts -> Int4,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    mail_outbox (id) {
        id -> Uuid,
        kind -> Text,
        recipient -> Text,
        recipient_key -> Text,
        subject -> Text,
        body_text -> Text,
        body_html -> Text,
        status -> Text,
        attempts -> Int4,
        max_attempts -> Int4,
        next_attempt_at -> Timestamptz,
        expires_at -> Timestamptz,
        last_error -> Nullable<Text>,
        user_id -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
        sent_at -> Nullable<Timestamptz>,
    }
}

diesel::table! {
    notification_subscriptions (id) {
        id -> Uuid,
        user_id -> Uuid,
        org_id -> Uuid,
        scope_type -> Text,
        scope_id -> Uuid,
        kind -> Text,
        enabled -> Bool,
        disabled_reason -> Nullable<Text>,
        disabled_at -> Nullable<Timestamptz>,
        conditions -> Jsonb,
        delivery -> Text,
        throttle_seconds -> Int4,
        quiet_start_min -> Nullable<Int2>,
        quiet_end_min -> Nullable<Int2>,
        quiet_tz -> Text,
        last_evaluated_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    notification_subscription_envs (subscription_id, environment_id) {
        subscription_id -> Uuid,
        environment_id -> Uuid,
    }
}

diesel::table! {
    notification_queue (id) {
        id -> Uuid,
        subscription_id -> Uuid,
        user_id -> Uuid,
        org_id -> Uuid,
        project_id -> Uuid,
        app_id -> Nullable<Uuid>,
        includes_unattributed -> Bool,
        kind -> Text,
        dedup_key -> Text,
        severity -> Text,
        title -> Nullable<Text>,
        body -> Nullable<Text>,
        link -> Nullable<Text>,
        occurred_at -> Timestamptz,
        deliver_after -> Timestamptz,
        status -> Text,
        attempts -> Int2,
        message_id -> Nullable<Uuid>,
        claimed_at -> Nullable<Timestamptz>,
        sent_at -> Nullable<Timestamptz>,
        finished_at -> Nullable<Timestamptz>,
        error -> Nullable<Text>,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    notification_queue_envs (queue_id, environment_id) {
        queue_id -> Uuid,
        environment_id -> Uuid,
    }
}

diesel::table! {
    inspector_policies (id) {
        id -> Uuid,
        org_id -> Uuid,
        target_type -> Text,
        target_id -> Uuid,
        enabled -> Bool,
        tracked_keys -> Jsonb,
        detectors -> Jsonb,
        scan_columns -> Nullable<Jsonb>,
        rollups -> Jsonb,
        window_days -> Int4,
        schedule_enabled -> Bool,
        schedule_days -> Int2,
        schedule_time -> Time,
        schedule_tz -> Text,
        next_run_at -> Nullable<Timestamptz>,
        last_run_at -> Nullable<Timestamptz>,
        last_scan_id -> Nullable<Uuid>,
        last_skip_reason -> Text,
        created_by -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    inspector_scans (id) {
        id -> Uuid,
        policy_id -> Uuid,
        org_id -> Uuid,
        trigger_type -> Text,
        requested_by -> Nullable<Uuid>,
        status -> Text,
        coverage -> Text,
        coverage_note -> Text,
        window_from -> Timestamptz,
        window_to -> Timestamptz,
        params -> Jsonb,
        targets -> Jsonb,
        units_total -> Int4,
        units_done -> Int4,
        cursor -> Jsonb,
        rows_scanned -> Int8,
        findings_count -> Int4,
        findings_reaped_at -> Nullable<Timestamptz>,
        worker_id -> Nullable<Text>,
        heartbeat_at -> Nullable<Timestamptz>,
        attempts -> Int4,
        cancel_requested_at -> Nullable<Timestamptz>,
        error -> Text,
        started_at -> Nullable<Timestamptz>,
        finished_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    inspector_findings (id) {
        id -> Uuid,
        scan_id -> Uuid,
        org_id -> Uuid,
        app_id -> Uuid,
        environment_id -> Nullable<Uuid>,
        env_scope -> Text,
        source_table -> Text,
        source_column -> Text,
        key_path -> Text,
        matched_key -> Text,
        detector -> Text,
        value_type -> Text,
        match_count -> Int8,
        match_count_exact -> Bool,
        sample_preview -> Text,
        sample_row_id -> Nullable<Uuid>,
        sample_occurred_at -> Nullable<Timestamptz>,
        partition_kind -> Text,
        first_seen_at -> Nullable<Timestamptz>,
        last_seen_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    inspector_mask_actions (id) {
        id -> Uuid,
        org_id -> Uuid,
        app_id -> Uuid,
        kind -> Text,
        finding_id -> Nullable<Uuid>,
        scan_id -> Nullable<Uuid>,
        targets -> Jsonb,
        status -> Text,
        requested_by -> Nullable<Uuid>,
        requested_by_email -> Text,
        cancelled_by -> Nullable<Uuid>,
        cancelled_by_email -> Text,
        cancelled_at -> Nullable<Timestamptz>,
        requested_at -> Timestamptz,
        previewed_at -> Nullable<Timestamptz>,
        confirmed_at -> Nullable<Timestamptz>,
        started_at -> Nullable<Timestamptz>,
        finished_at -> Nullable<Timestamptz>,
        confirm_source -> Text,
        estimated_rows -> Int8,
        rows_scanned -> Int8,
        rows_masked -> Int8,
        cold_rows_skipped -> Int8,
        cold_boundary_at -> Nullable<Timestamptz>,
        day_cursor -> Nullable<Date>,
        cursor_occurred_at -> Nullable<Timestamptz>,
        cursor_id -> Nullable<Uuid>,
        phase -> Text,
        worker_id -> Nullable<Text>,
        claimed_at -> Nullable<Timestamptz>,
        vacuum_advised -> Bool,
        error -> Text,
    }
}

diesel::table! {
    inspector_masked_keys (id) {
        id -> Uuid,
        app_id -> Uuid,
        target_table -> Text,
        target_column -> Text,
        json_path -> Text,
        created_at -> Timestamptz,
        created_by -> Nullable<Uuid>,
        source_action_id -> Nullable<Uuid>,
    }
}

diesel::table! {
    inspector_reveal_audit (id) {
        id -> Uuid,
        app_id -> Uuid,
        org_id -> Uuid,
        finding_id -> Nullable<Uuid>,
        user_id -> Nullable<Uuid>,
        user_email -> Text,
        source_table -> Text,
        source_column -> Text,
        key_path -> Text,
        request_source -> Text,
        created_at -> Timestamptz,
    }
}

diesel::joinable!(analytics_events -> apps (app_id));
diesel::joinable!(analytics_events -> app_environments (environment_id));
diesel::joinable!(sessions -> apps (app_id));
diesel::joinable!(devices -> apps (app_id));
diesel::joinable!(transactions -> apps (app_id));
diesel::joinable!(apps -> projects (project_id));
diesel::joinable!(app_environments -> apps (app_id));
diesel::joinable!(app_store_connections -> apps (app_id));
diesel::joinable!(store_daily_metrics -> apps (app_id));
diesel::joinable!(app_environments -> environments (environment_id));
diesel::joinable!(environments -> projects (project_id));
diesel::joinable!(error_events -> apps (app_id));
diesel::joinable!(error_events -> app_environments (environment_id));
diesel::joinable!(error_events -> issues (issue_id));
diesel::joinable!(event_users -> apps (app_id));
diesel::joinable!(identities -> apps (app_id));
diesel::joinable!(issues -> apps (app_id));
diesel::joinable!(issues -> users (assignee_id));
diesel::joinable!(projects -> organizations (org_id));
// Deliberately the only association declared for this table. diesel allows one association per
// table pair, no query in this slice joins auth_sessions to refresh_tokens in the DSL (all
// multi-table work is raw CTEs), and `revoked_by` would need a second users association diesel
// cannot express -- an unused joinable is a future ambiguous-join trap.
diesel::joinable!(auth_sessions -> users (user_id));
diesel::joinable!(refresh_tokens -> users (user_id));
// Only the user_id FK. `password_reset_tokens` has two FKs to `users` and
// `joinable!` accepts one per table pair, so a future query for the initiating
// admin's email needs an explicit `.on(...)` rather than a second line here.
diesel::joinable!(password_reset_tokens -> users (user_id));
diesel::joinable!(role_grants -> organizations (org_id));
diesel::joinable!(role_grants -> roles (role_id));
diesel::joinable!(role_grants -> users (user_id));
diesel::joinable!(roles -> organizations (org_id));
diesel::joinable!(monitors -> projects (project_id));
diesel::joinable!(monitor_checks -> monitors (monitor_id));
diesel::joinable!(monitor_incidents -> monitors (monitor_id));
diesel::joinable!(symbol_artifacts -> apps (app_id));
diesel::joinable!(notification_channels -> organizations (org_id));
diesel::joinable!(alert_rules -> organizations (org_id));
diesel::joinable!(alert_rules -> monitors (monitor_id));
diesel::joinable!(alert_rule_channels -> alert_rules (rule_id));
diesel::joinable!(alert_rule_channels -> notification_channels (channel_id));
diesel::joinable!(alert_events -> organizations (org_id));
diesel::joinable!(workflows -> apps (app_id));
diesel::joinable!(workflows -> app_environments (environment_id));
diesel::joinable!(mail_outbox -> users (user_id));
diesel::joinable!(notification_subscriptions -> users (user_id));
diesel::joinable!(notification_subscriptions -> organizations (org_id));
diesel::joinable!(notification_subscription_envs -> notification_subscriptions (subscription_id));
diesel::joinable!(notification_queue -> notification_subscriptions (subscription_id));
diesel::joinable!(notification_queue_envs -> notification_queue (queue_id));
// No `joinable!` for the nullable `created_by`/`requested_by` FKs to `users`:
// that matches `alert_rules.created_by`, which has none. An unused association
// is a future ambiguous-join trap.
diesel::joinable!(inspector_policies -> organizations (org_id));
diesel::joinable!(inspector_scans -> inspector_policies (policy_id));
diesel::joinable!(inspector_scans -> organizations (org_id));
diesel::joinable!(inspector_findings -> inspector_scans (scan_id));
diesel::joinable!(inspector_findings -> apps (app_id));
diesel::joinable!(inspector_mask_actions -> organizations (org_id));
diesel::joinable!(inspector_mask_actions -> apps (app_id));
diesel::joinable!(inspector_masked_keys -> apps (app_id));
diesel::joinable!(inspector_reveal_audit -> apps (app_id));

diesel::table! {
    runtime_settings (key) {
        key -> Text,
        value -> Text,
        updated_at -> Timestamptz,
        updated_by -> Nullable<Uuid>,
    }
}

diesel::table! {
    tier_pins (id) {
        id -> Uuid,
        table_name -> Text,
        range_start -> Timestamptz,
        range_end -> Timestamptz,
        expires_at -> Timestamptz,
        created_at -> Timestamptz,
        created_by -> Nullable<Uuid>,
        reason -> Nullable<Text>,
    }
}

diesel::table! {
    restore_jobs (id) {
        id -> Uuid,
        table_name -> Text,
        app_id -> Nullable<Uuid>,
        range_start -> Timestamptz,
        range_end -> Timestamptz,
        status -> Text,
        pin_id -> Nullable<Uuid>,
        pin_expires_at -> Timestamptz,
        rows_estimated -> Int8,
        rows_restored -> Int8,
        worker_id -> Nullable<Text>,
        heartbeat_at -> Nullable<Timestamptz>,
        attempts -> Int4,
        error -> Text,
        requested_by -> Nullable<Uuid>,
        created_at -> Timestamptz,
        started_at -> Nullable<Timestamptz>,
        finished_at -> Nullable<Timestamptz>,
    }
}

diesel::table! {
    audit_log (id) {
        id -> Uuid,
        org_id -> Uuid,
        actor_id -> Nullable<Uuid>,
        actor_email -> Text,
        action -> Text,
        entity_type -> Text,
        entity_id -> Nullable<Uuid>,
        entity_name -> Text,
        project_id -> Nullable<Uuid>,
        project_name -> Text,
        app_id -> Nullable<Uuid>,
        app_name -> Text,
        environment_id -> Nullable<Uuid>,
        environment_name -> Text,
        changes -> Jsonb,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    ingest_failures (id) {
        id -> Uuid,
        fingerprint -> Text,
        error_kind -> Text,
        error_message -> Text,
        org_id -> Nullable<Uuid>,
        project_id -> Nullable<Uuid>,
        app_id -> Nullable<Uuid>,
        occurrences -> BigInt,
        status -> Text,
        first_seen_at -> Timestamptz,
        last_seen_at -> Timestamptz,
    }
}

diesel::table! {
    ingest_failure_payloads (id) {
        id -> Uuid,
        failure_id -> Uuid,
        payload -> Jsonb,
        attempts -> Integer,
        created_at -> Timestamptz,
        requeued_at -> Nullable<Timestamptz>,
    }
}

diesel::joinable!(ingest_failure_payloads -> ingest_failures (failure_id));

diesel::allow_tables_to_appear_in_same_query!(
    analytics_events,
    auth_sessions,
    app_environments,
    app_store_connections,
    apps,
    environments,
    store_daily_metrics,
    error_events,
    event_users,
    identities,
    issues,
    organizations,
    projects,
    refresh_tokens,
    password_reset_tokens,
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
    tiering_state,
    symbol_blobs,
    symbol_artifacts,
    notification_channels,
    alert_rules,
    alert_rule_channels,
    alert_events,
    workflows,
    mail_outbox,
    notification_subscriptions,
    notification_subscription_envs,
    notification_queue,
    notification_queue_envs,
    inspector_policies,
    inspector_scans,
    inspector_findings,
    inspector_mask_actions,
    inspector_masked_keys,
    inspector_reveal_audit,
    runtime_settings,
    tier_pins,
    restore_jobs,
    audit_log,
    ingest_failures,
    ingest_failure_payloads,
);
