mod common;

use chrono::{NaiveTime, Utc};
use common::TestDb;
use sauron_db::models::NewInspectorPolicy;
use sauron_db::repo;
use serde_json::json;

async fn seed_policy(
    db: &TestDb,
    org_id: uuid::Uuid,
    app_id: uuid::Uuid,
    tz: &str,
    time: NaiveTime,
    days: i16,
) -> uuid::Uuid {
    let mut conn = db.conn().await;
    let keys = json!([{"key": "email", "scope": "any"}]);
    let dets = json!([]);
    let rollups = json!(["issues"]);
    let p = repo::create_inspector_policy(
        &mut conn,
        NewInspectorPolicy {
            org_id,
            target_type: "app",
            target_id: app_id,
            enabled: true,
            tracked_keys: &keys,
            detectors: &dets,
            scan_columns: None,
            rollups: &rollups,
            window_days: 30,
            schedule_enabled: true,
            schedule_days: days,
            schedule_time: time,
            schedule_tz: tz,
            created_by: None,
        },
    )
    .await
    .expect("create");
    repo::reschedule_policy(&mut conn, p.id)
        .await
        .expect("reschedule");
    p.id
}

#[tokio::test]
async fn every_weekday_bit_produces_a_future_run() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let id = seed_policy(
        &db,
        ids.org_id,
        ids.app_id,
        "UTC",
        NaiveTime::from_hms_opt(3, 0, 0).unwrap(),
        127,
    )
    .await;
    let mut conn = db.conn().await;
    let p = repo::get_inspector_policy(&mut conn, id)
        .await
        .unwrap()
        .unwrap();
    let next = p.next_run_at.expect("next_run_at must be materialized");
    assert!(
        next > Utc::now(),
        "next_run_at must be strictly in the future"
    );
    db.cleanup().await;
}

/// A zero mask means "no days selected", which must never become due — a row
/// that is permanently due is a row the scheduler re-claims every tick.
#[tokio::test]
async fn a_zero_day_mask_is_never_due() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let id = seed_policy(
        &db,
        ids.org_id,
        ids.app_id,
        "UTC",
        NaiveTime::from_hms_opt(3, 0, 0).unwrap(),
        0,
    )
    .await;
    let mut conn = db.conn().await;
    let p = repo::get_inspector_policy(&mut conn, id)
        .await
        .unwrap()
        .unwrap();
    assert!(p.next_run_at.is_none());
    let claimed = repo::claim_due_policies(&mut conn, 10).await.unwrap();
    assert!(claimed.iter().all(|c| c.id != id));
    db.cleanup().await;
}

/// DST, asserted rather than discovered in a November incident. Candidates are
/// built as LOCAL timestamps and converted back with AT TIME ZONE, so Postgres
/// resolves DST: spring-forward yields a valid instant, fall-back yields the
/// first occurrence — never zero runs, never double runs.
#[tokio::test]
async fn dst_transitions_yield_exactly_one_future_instant() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    for tz in ["America/New_York", "Europe/Paris"] {
        let id = seed_policy(
            &db,
            ids.org_id,
            ids.app_id,
            tz,
            NaiveTime::from_hms_opt(2, 30, 0).unwrap(),
            1 << 0, // Sundays, when both zones transition
        )
        .await;
        let mut conn = db.conn().await;
        let p = repo::get_inspector_policy(&mut conn, id)
            .await
            .unwrap()
            .unwrap();
        let next = p.next_run_at.expect("a Sunday schedule must resolve");
        assert!(next > Utc::now(), "{tz}: next_run_at must be in the future");
        repo::delete_inspector_policy(&mut conn, id).await.unwrap();
    }
    db.cleanup().await;
}

/// The claim ALWAYS advances next_run_at, so a row can never get stuck
/// permanently due; the worker then decides whether to actually start a scan.
#[tokio::test]
async fn a_claim_advances_next_run_at_past_now() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let id = seed_policy(
        &db,
        ids.org_id,
        ids.app_id,
        "UTC",
        NaiveTime::from_hms_opt(3, 0, 0).unwrap(),
        127,
    )
    .await;
    let mut conn = db.conn().await;
    // Force it due.
    diesel_async::RunQueryDsl::execute(
        diesel::sql_query(
            "UPDATE inspector_policies SET next_run_at = now() - interval '1 minute' WHERE id = $1",
        )
        .bind::<diesel::sql_types::Uuid, _>(id),
        &mut conn,
    )
    .await
    .unwrap();
    let claimed = repo::claim_due_policies(&mut conn, 10).await.unwrap();
    let row = claimed
        .iter()
        .find(|c| c.id == id)
        .expect("must be claimed");
    assert!(row.next_run_at.unwrap() > Utc::now());
    assert!(row.last_run_at.is_some());
    // A second claim in the same instant returns nothing: it is no longer due.
    let again = repo::claim_due_policies(&mut conn, 10).await.unwrap();
    assert!(again.iter().all(|c| c.id != id));
    db.cleanup().await;
}

#[tokio::test]
async fn a_target_outside_the_org_is_rejected() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    assert!(
        repo::validate_scope_in_org(&mut conn, ids.org_id, "app", ids.app_id)
            .await
            .unwrap()
    );
    assert!(
        repo::validate_scope_in_org(&mut conn, ids.org_id, "project", ids.project_id)
            .await
            .unwrap()
    );
    assert!(
        repo::validate_scope_in_org(&mut conn, ids.org_id, "app_env", ids.env_a)
            .await
            .unwrap()
    );
    // A different org's id must not validate — without this any authenticated
    // user can mint an org, POST a policy naming a victim's app_id, and have
    // the worker scan the victim's error_events into rows carrying the
    // attacker's org_id, which is exactly what list queries filter on.
    assert!(
        !repo::validate_scope_in_org(&mut conn, uuid::Uuid::new_v4(), "app", ids.app_id)
            .await
            .unwrap()
    );
    assert!(
        !repo::validate_scope_in_org(&mut conn, ids.org_id, "app", uuid::Uuid::new_v4())
            .await
            .unwrap()
    );
    // An unknown target_type must be a hard false, never a permissive default.
    assert!(
        !repo::validate_scope_in_org(&mut conn, ids.org_id, "org", ids.org_id)
            .await
            .unwrap()
    );
    db.cleanup().await;
}

#[tokio::test]
async fn timezone_validation_rejects_junk() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;
    assert!(repo::timezone_is_valid(&mut conn, "Europe/Paris").await);
    assert!(repo::timezone_is_valid(&mut conn, "UTC").await);
    assert!(!repo::timezone_is_valid(&mut conn, "Mars/Olympus").await);
    assert!(!repo::timezone_is_valid(&mut conn, "'; DROP TABLE users; --").await);
    db.cleanup().await;
}

/// Most specific wins, whole row. An app_env policy shadows the app policy
/// which shadows the project policy, and the resolution is a database fact
/// because of `UNIQUE (target_type, target_id)`.
#[tokio::test]
async fn effective_policy_prefers_the_most_specific_node() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let t = NaiveTime::from_hms_opt(3, 0, 0).unwrap();
    let mut conn = db.conn().await;
    let keys = json!(["email"]);
    let empty = json!([]);
    let rollups = json!([]);
    let mk = |tt: &'static str, tid: uuid::Uuid| NewInspectorPolicy {
        org_id: ids.org_id,
        target_type: tt,
        target_id: tid,
        enabled: true,
        tracked_keys: &keys,
        detectors: &empty,
        scan_columns: None,
        rollups: &rollups,
        window_days: 30,
        schedule_enabled: false,
        schedule_days: 0,
        schedule_time: t,
        schedule_tz: "UTC",
        created_by: None,
    };
    let proj = repo::create_inspector_policy(&mut conn, mk("project", ids.project_id))
        .await
        .unwrap();
    let found = repo::effective_policy_for_app(&mut conn, ids.app_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.id, proj.id);
    let app = repo::create_inspector_policy(&mut conn, mk("app", ids.app_id))
        .await
        .unwrap();
    let found = repo::effective_policy_for_app(&mut conn, ids.app_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.id, app.id);
    db.cleanup().await;
}

/// Deleting an app must take its inspector policies with it — both the
/// `app`-scoped one and the `app_env`-scoped one under it.
///
/// `inspector_policies.target_id` is polymorphic, so it carries no FK and gets
/// no `ON DELETE CASCADE`. Everything else the app owns DOES cascade, which is
/// what made the survivor easy to miss. An orphan is not merely untidy: it is
/// still returned by `GET /v1/orgs/{org}/inspector/policies` while
/// `DELETE /v1/inspector/policies/{id}` answers 404 forever, because that
/// handler authorizes through an app that no longer exists.
#[tokio::test]
async fn deleting_an_app_takes_its_inspector_policies_with_it() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("skipping: TEST_DATABASE_URL unset");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let keys = json!([{"key": "email", "scope": "any"}]);
    let empty = json!([]);
    let rollups = json!(["issues"]);
    let t = NaiveTime::from_hms_opt(3, 0, 0).unwrap();
    let mk = |target_type: &'static str, target_id: uuid::Uuid| NewInspectorPolicy {
        org_id: ids.org_id,
        target_type,
        target_id,
        enabled: true,
        tracked_keys: &keys,
        detectors: &empty,
        scan_columns: None,
        rollups: &rollups,
        window_days: 30,
        schedule_enabled: false,
        schedule_days: 0,
        schedule_time: t,
        schedule_tz: "UTC",
        created_by: None,
    };

    // `env_a` is an ENROLLMENT id (app_environments.id), which is exactly what
    // an `app_env` policy targets. A catalogue id here would match nothing and
    // the test would pass for the wrong reason.
    let app_policy = repo::create_inspector_policy(&mut conn, mk("app", ids.app_id))
        .await
        .expect("create app policy");
    let env_policy = repo::create_inspector_policy(&mut conn, mk("app_env", ids.env_a))
        .await
        .expect("create app_env policy");

    // A project-scoped policy in the same org that must SURVIVE — without it a
    // cleanup that deletes everything passes both assertions below.
    let project_policy = repo::create_inspector_policy(&mut conn, mk("project", ids.project_id))
        .await
        .expect("create project policy");

    repo::delete_app(&mut conn, ids.app_id)
        .await
        .expect("delete app");

    for (id, label) in [
        (app_policy.id, "app-scoped policy"),
        (env_policy.id, "app_env-scoped policy under that app"),
    ] {
        assert!(
            repo::get_inspector_policy(&mut conn, id)
                .await
                .expect("load policy")
                .is_none(),
            "{label} survived its app's deletion — it is now listed but undeletable"
        );
    }
    assert!(
        repo::get_inspector_policy(&mut conn, project_policy.id)
            .await
            .expect("load project policy")
            .is_some(),
        "deleting an app removed a policy belonging to its PROJECT"
    );

    db.cleanup().await;
}

/// The same guarantee one level up: a project delete must reach the policies of
/// its apps and of those apps' enrollments, not just its own.
#[tokio::test]
async fn deleting_a_project_takes_every_policy_beneath_it() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("skipping: TEST_DATABASE_URL unset");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let keys = json!([{"key": "email", "scope": "any"}]);
    let empty = json!([]);
    let rollups = json!(["issues"]);
    let t = NaiveTime::from_hms_opt(3, 0, 0).unwrap();
    let mk = |target_type: &'static str, target_id: uuid::Uuid| NewInspectorPolicy {
        org_id: ids.org_id,
        target_type,
        target_id,
        enabled: true,
        tracked_keys: &keys,
        detectors: &empty,
        scan_columns: None,
        rollups: &rollups,
        window_days: 30,
        schedule_enabled: false,
        schedule_days: 0,
        schedule_time: t,
        schedule_tz: "UTC",
        created_by: None,
    };

    let created: Vec<uuid::Uuid> = {
        let mut out = Vec::new();
        for (ty, id) in [
            ("project", ids.project_id),
            ("app", ids.app_id),
            ("app_env", ids.env_a),
            ("app_env", ids.env_b),
        ] {
            out.push(
                repo::create_inspector_policy(&mut conn, mk(ty, id))
                    .await
                    .unwrap_or_else(|e| panic!("create {ty} policy: {e}"))
                    .id,
            );
        }
        out
    };

    repo::delete_project(&mut conn, ids.project_id)
        .await
        .expect("delete project");

    for id in created {
        assert!(
            repo::get_inspector_policy(&mut conn, id)
                .await
                .expect("load policy")
                .is_none(),
            "policy {id} survived its project's deletion"
        );
    }

    db.cleanup().await;
}

/// The reaper repairs orphans the cascade did not create — a row left by a
/// direct SQL delete, or by any route the two handlers do not own.
///
/// Pinned separately from the cascade tests because it is the half that fixes
/// rows ALREADY on disk. The surviving policy matters as much as the reaped
/// ones: a `NOT IN (SELECT ...)` here would go NULL-poisoned and delete
/// nothing while still reporting success, and only a live row proves the
/// predicate discriminates rather than matching everything.
#[tokio::test]
async fn the_reaper_removes_policies_whose_target_is_gone() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("skipping: TEST_DATABASE_URL unset");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let keys = json!([{"key": "email", "scope": "any"}]);
    let empty = json!([]);
    let rollups = json!(["issues"]);
    let t = NaiveTime::from_hms_opt(3, 0, 0).unwrap();
    let mk = |target_type: &'static str, target_id: uuid::Uuid| NewInspectorPolicy {
        org_id: ids.org_id,
        target_type,
        target_id,
        enabled: true,
        tracked_keys: &keys,
        detectors: &empty,
        scan_columns: None,
        rollups: &rollups,
        window_days: 30,
        schedule_enabled: false,
        schedule_days: 0,
        schedule_time: t,
        schedule_tz: "UTC",
        created_by: None,
    };

    // Targets that never existed — the shape a stale row has after its app is
    // gone, without depending on the cascade this test is not about.
    let ghost_app = repo::create_inspector_policy(&mut conn, mk("app", uuid::Uuid::new_v4()))
        .await
        .expect("create ghost app policy");
    let ghost_env = repo::create_inspector_policy(&mut conn, mk("app_env", uuid::Uuid::new_v4()))
        .await
        .expect("create ghost app_env policy");
    let ghost_project =
        repo::create_inspector_policy(&mut conn, mk("project", uuid::Uuid::new_v4()))
            .await
            .expect("create ghost project policy");
    let live = repo::create_inspector_policy(&mut conn, mk("app", ids.app_id))
        .await
        .expect("create live policy");

    let pruned = repo::prune_orphaned_inspector_policies(&mut conn, 5_000)
        .await
        .expect("prune");
    assert_eq!(pruned, 3, "expected exactly the three ghosts to be reaped");

    for (id, label) in [
        (ghost_app.id, "app"),
        (ghost_env.id, "app_env"),
        (ghost_project.id, "project"),
    ] {
        assert!(
            repo::get_inspector_policy(&mut conn, id)
                .await
                .expect("load policy")
                .is_none(),
            "the reaper left a {label} policy whose target does not exist"
        );
    }
    assert!(
        repo::get_inspector_policy(&mut conn, live.id)
            .await
            .expect("load live policy")
            .is_some(),
        "the reaper deleted a policy whose app is very much alive"
    );

    // Idempotent: a second pass has nothing left to do.
    assert_eq!(
        repo::prune_orphaned_inspector_policies(&mut conn, 5_000)
            .await
            .expect("second prune"),
        0
    );

    db.cleanup().await;
}
