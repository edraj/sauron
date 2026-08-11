//! The Wall of Shame's read path, against a real Postgres.
//!
//! The interesting behaviour here is all in SQL that the Rust compiler cannot
//! check: a `UNION ALL` across three tables, nine `($n IS NULL OR col = $n)`
//! filters, and a tuple keyset. Every one of those is the kind of thing that
//! silently returns *almost* the right rows.
//!
//! Skips (does not fail) when `TEST_DATABASE_URL` is unset — see `common`.

mod common;

use chrono::{Duration, Utc};
use common::TestDb;
use sauron_db::models::NewAuditLogEntry;
use sauron_db::repo::{self, AuditFilter};
use serde_json::json;
use uuid::Uuid;

/// Insert one entry, with everything defaulted except what a test cares about.
#[allow(clippy::too_many_arguments)]
async fn insert(
    db: &TestDb,
    org_id: Uuid,
    action: &str,
    entity_type: &str,
    actor_id: Option<Uuid>,
    actor_email: &str,
    project: Option<(Uuid, &str)>,
    app: Option<(Uuid, &str)>,
) -> Uuid {
    let mut conn = db.conn().await;
    repo::insert_audit_log(
        &mut conn,
        NewAuditLogEntry {
            org_id,
            actor_id,
            actor_email,
            action,
            entity_type,
            entity_id: None,
            entity_name: "target",
            project_id: project.map(|p| p.0),
            project_name: project.map(|p| p.1).unwrap_or(""),
            app_id: app.map(|a| a.0),
            app_name: app.map(|a| a.1).unwrap_or(""),
            environment_id: None,
            environment_name: "",
            changes: json!({}),
        },
    )
    .await
    .unwrap()
    .id
}

async fn feed(db: &TestDb, org_id: Uuid, f: &AuditFilter, limit: i64) -> Vec<repo::AuditFeedRow> {
    let mut conn = db.conn().await;
    repo::list_audit_feed(&mut conn, org_id, f, limit)
        .await
        .unwrap()
}

#[tokio::test]
async fn entries_round_trip_and_come_back_newest_first() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;

    for action in ["project.create", "app.create", "role.update"] {
        insert(
            &db,
            ids.org_id,
            action,
            "project",
            None,
            "a@example.com",
            None,
            None,
        )
        .await;
    }

    let rows = feed(&db, ids.org_id, &AuditFilter::default(), 50).await;
    assert_eq!(rows.len(), 3);
    // Newest first, and every row tagged as coming from audit_log rather than
    // one of the projected inspector tables.
    assert_eq!(rows[0].action, "role.update");
    assert_eq!(rows[2].action, "project.create");
    assert!(rows.iter().all(|r| r.source == "audit"));

    db.cleanup().await;
}

/// The property the whole feature rests on: one tenant can never see another's
/// trail, no matter what it passes.
#[tokio::test]
async fn one_orgs_trail_is_invisible_to_another() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let a = db.seed_two_envs().await;
    let other_org = {
        let mut conn = db.conn().await;
        repo::create_org(&mut conn, "Other Tenant", "other-tenant")
            .await
            .unwrap()
            .id
    };

    insert(
        &db,
        a.org_id,
        "project.create",
        "project",
        None,
        "a@example.com",
        None,
        None,
    )
    .await;
    insert(
        &db,
        other_org,
        "project.delete",
        "project",
        None,
        "b@example.com",
        None,
        None,
    )
    .await;

    let mine = feed(&db, a.org_id, &AuditFilter::default(), 50).await;
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0].action, "project.create");

    let theirs = feed(&db, other_org, &AuditFilter::default(), 50).await;
    assert_eq!(theirs.len(), 1);
    assert_eq!(theirs[0].action, "project.delete");

    db.cleanup().await;
}

#[tokio::test]
async fn each_filter_axis_narrows_independently() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let actor = Uuid::new_v4();
    let other_project = Uuid::new_v4();

    insert(
        &db,
        ids.org_id,
        "project.create",
        "project",
        Some(actor),
        "a@x.com",
        Some((ids.project_id, "Alpha")),
        None,
    )
    .await;
    insert(
        &db,
        ids.org_id,
        "app.create",
        "app",
        None,
        "b@x.com",
        Some((other_project, "Beta")),
        Some((ids.app_id, "MyApp")),
    )
    .await;
    insert(
        &db,
        ids.org_id,
        "role.update",
        "role",
        Some(actor),
        "a@x.com",
        None,
        None,
    )
    .await;

    // Unfiltered sees everything, so each assertion below is a real narrowing
    // rather than a query that happened to return few rows.
    let all = feed(&db, ids.org_id, &AuditFilter::default(), 50).await;
    assert_eq!(all.len(), 3);

    let by_project = AuditFilter {
        project_id: Some(ids.project_id),
        ..Default::default()
    };
    assert_eq!(feed(&db, ids.org_id, &by_project, 50).await.len(), 1);

    let by_app = AuditFilter {
        app_id: Some(ids.app_id),
        ..Default::default()
    };
    assert_eq!(feed(&db, ids.org_id, &by_app, 50).await.len(), 1);

    let by_actor = AuditFilter {
        actor_id: Some(actor),
        ..Default::default()
    };
    assert_eq!(feed(&db, ids.org_id, &by_actor, 50).await.len(), 2);

    let by_action = AuditFilter {
        action: Some("role.update".into()),
        ..Default::default()
    };
    assert_eq!(feed(&db, ids.org_id, &by_action, 50).await.len(), 1);

    let by_entity = AuditFilter {
        entity_type: Some("app".into()),
        ..Default::default()
    };
    assert_eq!(feed(&db, ids.org_id, &by_entity, 50).await.len(), 1);

    // A filter naming something absent returns nothing — not everything, which
    // is how an `IS NULL OR` predicate fails when the cast is wrong.
    let by_missing = AuditFilter {
        app_id: Some(Uuid::new_v4()),
        ..Default::default()
    };
    assert_eq!(feed(&db, ids.org_id, &by_missing, 50).await.len(), 0);

    db.cleanup().await;
}

#[tokio::test]
async fn filters_combine_as_and_not_or() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let actor = Uuid::new_v4();

    insert(
        &db,
        ids.org_id,
        "project.create",
        "project",
        Some(actor),
        "a@x.com",
        Some((ids.project_id, "Alpha")),
        None,
    )
    .await;
    insert(
        &db,
        ids.org_id,
        "project.delete",
        "project",
        None,
        "b@x.com",
        Some((ids.project_id, "Alpha")),
        None,
    )
    .await;

    // Matches the project but not the actor. An OR bug returns 2 here.
    let rows = feed(
        &db,
        ids.org_id,
        &AuditFilter {
            project_id: Some(ids.project_id),
            actor_id: Some(actor),
            ..Default::default()
        },
        50,
    )
    .await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].action, "project.create");

    db.cleanup().await;
}

/// The tiebreaker test. Entries written by one request share a `created_at` to
/// microsecond precision; a cursor on the timestamp alone silently skips or
/// repeats one of them at the page boundary.
#[tokio::test]
async fn keyset_pagination_does_not_skip_or_repeat_across_identical_timestamps() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;

    // Six rows sharing ONE created_at, written directly so the timestamps are
    // exactly equal rather than merely close.
    let stamp = Utc::now();
    {
        use diesel::prelude::*;
        use diesel_async::RunQueryDsl;
        use sauron_db::schema::audit_log;
        let mut conn = db.conn().await;
        for i in 0..6 {
            let id = repo::insert_audit_log(
                &mut conn,
                NewAuditLogEntry {
                    org_id: ids.org_id,
                    actor_id: None,
                    actor_email: "a@x.com",
                    action: "project.update",
                    entity_type: "project",
                    entity_id: None,
                    entity_name: &format!("row-{i}"),
                    project_id: None,
                    project_name: "",
                    app_id: None,
                    app_name: "",
                    environment_id: None,
                    environment_name: "",
                    changes: json!({}),
                },
            )
            .await
            .unwrap()
            .id;
            diesel::update(audit_log::table.find(id))
                .set(audit_log::created_at.eq(stamp))
                .execute(&mut conn)
                .await
                .unwrap();
        }
    }

    // Page through two at a time and collect every id seen.
    let mut seen: Vec<Uuid> = Vec::new();
    let mut cursor = None;
    for _ in 0..5 {
        let rows = feed(
            &db,
            ids.org_id,
            &AuditFilter {
                cursor,
                ..Default::default()
            },
            2,
        )
        .await;
        if rows.is_empty() {
            break;
        }
        cursor = rows.last().map(|r| (r.created_at, r.id));
        seen.extend(rows.iter().map(|r| r.id));
    }

    assert_eq!(seen.len(), 6, "paging lost or duplicated rows: {seen:?}");
    let mut unique = seen.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), 6, "a row was served on two different pages");

    db.cleanup().await;
}

#[tokio::test]
async fn time_bounds_are_inclusive_and_exclude_outside_rows() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let id = insert(
        &db,
        ids.org_id,
        "project.create",
        "project",
        None,
        "a@x.com",
        None,
        None,
    )
    .await;

    // Backdate it a week.
    {
        use diesel::prelude::*;
        use diesel_async::RunQueryDsl;
        use sauron_db::schema::audit_log;
        let mut conn = db.conn().await;
        diesel::update(audit_log::table.find(id))
            .set(audit_log::created_at.eq(Utc::now() - Duration::days(7)))
            .execute(&mut conn)
            .await
            .unwrap();
    }

    let last_day = AuditFilter {
        from: Some(Utc::now() - Duration::days(1)),
        ..Default::default()
    };
    assert_eq!(feed(&db, ids.org_id, &last_day, 50).await.len(), 0);

    let last_month = AuditFilter {
        from: Some(Utc::now() - Duration::days(30)),
        to: Some(Utc::now()),
        ..Default::default()
    };
    assert_eq!(feed(&db, ids.org_id, &last_month, 50).await.len(), 1);

    db.cleanup().await;
}

#[tokio::test]
async fn facets_are_sourced_from_the_trail_including_deleted_targets() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let actor = Uuid::new_v4();
    // A project id that does not exist in `projects` — i.e. one that has been
    // deleted. Its entries must still be selectable, which is the whole reason
    // facets come from the trail rather than from live rows.
    let ghost_project = Uuid::new_v4();

    insert(
        &db,
        ids.org_id,
        "project.delete",
        "project",
        Some(actor),
        "gone@x.com",
        Some((ghost_project, "Deleted Project")),
        None,
    )
    .await;

    let mut conn = db.conn().await;
    let actors = repo::audit_actor_facets(&mut conn, ids.org_id, false)
        .await
        .unwrap();
    assert_eq!(actors.len(), 1);
    assert_eq!(actors[0].label, "gone@x.com");

    let actions = repo::audit_action_facets(&mut conn, ids.org_id, false)
        .await
        .unwrap();
    assert!(actions.iter().any(|a| a.label == "project.delete"));

    let scopes = repo::audit_scope_facets(&mut conn, ids.org_id)
        .await
        .unwrap();
    assert_eq!(scopes.projects.len(), 1);
    assert_eq!(scopes.projects[0].label, "Deleted Project");
    assert_eq!(scopes.projects[0].id, Some(ghost_project));
    drop(conn);

    // A rename must leave the dropdown offering the CURRENT name. `MAX(name)`
    // would answer "Aardvark" here purely because it sorts first, and the
    // dropdown would disagree with every other page in the dashboard.
    insert(
        &db,
        ids.org_id,
        "project.update",
        "project",
        Some(actor),
        "gone@x.com",
        Some((ghost_project, "Aardvark Renamed Later")),
        None,
    )
    .await;
    let mut conn = db.conn().await;
    let after = repo::audit_scope_facets(&mut conn, ids.org_id)
        .await
        .unwrap();
    assert_eq!(
        after.projects.len(),
        1,
        "a rename must not create a second option"
    );
    assert_eq!(after.projects[0].label, "Aardvark Renamed Later");
    // No app entries were written, so the app dropdown must be empty rather
    // than listing the org's live apps.
    assert!(scopes.apps.is_empty());

    db.cleanup().await;
}

/// Ids on this table are inert snapshots. If a future migration reintroduces a
/// foreign key, delete handlers can no longer record their own event and this
/// insert starts failing — which the fail-open path would swallow silently.
#[tokio::test]
async fn an_entry_can_name_rows_that_no_longer_exist() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;

    let mut conn = db.conn().await;
    let written = repo::insert_audit_log(
        &mut conn,
        NewAuditLogEntry {
            org_id: ids.org_id,
            // None of these three exist in their tables.
            actor_id: Some(Uuid::new_v4()),
            actor_email: "deleted-user@x.com",
            action: "project.delete",
            entity_type: "project",
            entity_id: Some(Uuid::new_v4()),
            entity_name: "Deleted Project",
            project_id: Some(Uuid::new_v4()),
            project_name: "Deleted Project",
            app_id: Some(Uuid::new_v4()),
            app_name: "Deleted App",
            environment_id: Some(Uuid::new_v4()),
            environment_name: "staging",
            changes: json!({"name": {"from": "Deleted Project", "to": null}}),
        },
    )
    .await;

    assert!(
        written.is_ok(),
        "audit_log rejected an entry naming deleted rows — a foreign key has \
         been reintroduced, and delete actions can no longer be recorded: {written:?}"
    );

    db.cleanup().await;
}

#[tokio::test]
async fn limit_bounds_the_page() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    for i in 0..5 {
        insert(
            &db,
            ids.org_id,
            "project.update",
            "project",
            None,
            &format!("a{i}@x.com"),
            None,
            None,
        )
        .await;
    }
    assert_eq!(
        feed(&db, ids.org_id, &AuditFilter::default(), 2)
            .await
            .len(),
        2
    );
    assert_eq!(
        feed(&db, ids.org_id, &AuditFilter::default(), 50)
            .await
            .len(),
        5
    );

    db.cleanup().await;
}

// ---------------------------------------------------------------------------
// The auth stream
// ---------------------------------------------------------------------------

/// Auth events must be invisible unless asked for. This is the whole mechanism
/// that lets them be recorded at all: decision 1 excluded them outright because
/// logins would bury the member, role and key events the feed exists to show.
#[tokio::test]
async fn auth_events_are_hidden_from_the_default_feed() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let actor = Uuid::new_v4();

    insert(
        &db,
        ids.org_id,
        "role.update",
        "role",
        Some(actor),
        "a@x.com",
        None,
        None,
    )
    .await;
    for _ in 0..5 {
        insert(
            &db,
            ids.org_id,
            "auth.login",
            "auth",
            Some(actor),
            "a@x.com",
            None,
            None,
        )
        .await;
    }

    // Default: the one admin action, and none of the five logins.
    let default = feed(&db, ids.org_id, &AuditFilter::default(), 50).await;
    assert_eq!(default.len(), 1);
    assert_eq!(default[0].action, "role.update");

    // Opted in: everything.
    let with_auth = feed(
        &db,
        ids.org_id,
        &AuditFilter {
            include_auth: true,
            ..Default::default()
        },
        50,
    )
    .await;
    assert_eq!(with_auth.len(), 6);

    // Filtering FOR auth returns only auth.
    let only_auth = feed(
        &db,
        ids.org_id,
        &AuditFilter {
            include_auth: true,
            entity_type: Some("auth".into()),
            ..Default::default()
        },
        50,
    )
    .await;
    assert_eq!(only_auth.len(), 5);
    assert!(only_auth.iter().all(|r| r.action == "auth.login"));

    db.cleanup().await;
}

/// A dropdown must only offer values that return results. With auth hidden,
/// offering "Signed in" would produce an empty table and read as a bug.
#[tokio::test]
async fn facets_hide_auth_unless_it_is_included() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let admin = Uuid::new_v4();
    let signer = Uuid::new_v4();

    insert(
        &db,
        ids.org_id,
        "role.update",
        "role",
        Some(admin),
        "admin@x.com",
        None,
        None,
    )
    .await;
    insert(
        &db,
        ids.org_id,
        "auth.login",
        "auth",
        Some(signer),
        "signer@x.com",
        None,
        None,
    )
    .await;

    let mut conn = db.conn().await;

    let actions = repo::audit_action_facets(&mut conn, ids.org_id, false)
        .await
        .unwrap();
    assert!(actions.iter().all(|a| a.label != "auth.login"));
    let actions_with = repo::audit_action_facets(&mut conn, ids.org_id, true)
        .await
        .unwrap();
    assert!(actions_with.iter().any(|a| a.label == "auth.login"));

    // An actor who ONLY ever signed in must not appear in the "Who" dropdown of
    // a feed that hides sign-ins — selecting them would return nothing.
    let actors = repo::audit_actor_facets(&mut conn, ids.org_id, false)
        .await
        .unwrap();
    assert_eq!(actors.len(), 1);
    assert_eq!(actors[0].label, "admin@x.com");
    let actors_with = repo::audit_actor_facets(&mut conn, ids.org_id, true)
        .await
        .unwrap();
    assert_eq!(actors_with.len(), 2);

    db.cleanup().await;
}

/// Migration 52's partial index must actually be used by the default feed.
/// Postgres only uses a partial index when the query predicate provably implies
/// the index predicate, so a cosmetic difference between the two spellings costs
/// a full scan silently — no error, no signal but latency.
#[tokio::test]
async fn the_default_feed_uses_the_admin_partial_index() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    for i in 0..20 {
        insert(
            &db,
            ids.org_id,
            "auth.login",
            "auth",
            None,
            &format!("u{i}@x.com"),
            None,
            None,
        )
        .await;
    }
    insert(
        &db,
        ids.org_id,
        "role.update",
        "role",
        None,
        "a@x.com",
        None,
        None,
    )
    .await;

    let mut conn = db.conn().await;
    // ANALYZE first: on a tiny table the planner will pick a seq scan whatever
    // indexes exist, so force index use for the shape assertion.
    {
        use diesel_async::SimpleAsyncConnection as _;
        conn.batch_execute("ANALYZE audit_log; SET enable_seqscan = off;")
            .await
            .unwrap();
    }

    #[derive(diesel::QueryableByName)]
    struct Plan {
        #[diesel(sql_type = diesel::sql_types::Text)]
        #[diesel(column_name = "QUERY PLAN")]
        line: String,
    }
    use diesel_async::RunQueryDsl as _;
    let plan: Vec<Plan> = diesel::sql_query(format!(
        "EXPLAIN SELECT id FROM audit_log WHERE org_id = '{}' \
         AND entity_type <> 'auth' ORDER BY created_at DESC, id DESC LIMIT 50",
        ids.org_id
    ))
    .load(&mut conn)
    .await
    .unwrap();
    let text = plan
        .iter()
        .map(|p| p.line.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        text.contains("audit_log_org_time_admin_idx"),
        "the default feed is not using migration 52's partial index — the query \
         predicate and the index predicate have drifted apart. Plan was:\n{text}"
    );

    db.cleanup().await;
}
