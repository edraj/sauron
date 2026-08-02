//! S3 personal notification subscriptions, against a real Postgres database.
//!
//! Skips (rather than fails) when `TEST_DATABASE_URL` is unset, matching
//! `env_scoping.rs` and `workflows.rs` — CI has no Postgres service.

mod common;

use common::TestDb;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use sauron_db::models::{NewNotificationSubscription, NotificationSubscription};
use sauron_db::schema::notification_subscriptions;
use serde_json::json;

/// `Queryable` decodes positionally, so a struct whose field order drifts from
/// the `table!` block binds `disabled_reason` into `scope_type` and compiles
/// silently. Reading a known row back through `as_select()` and asserting each
/// value is the only thing that catches it.
#[tokio::test]
async fn subscription_row_round_trips_in_declared_column_order() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let user_id = sauron_db::repo::find_user_by_email(&mut conn, &ids.owner_email)
        .await
        .expect("load harness owner")
        .expect("harness owner exists")
        .id;

    let conditions = json!({ "window_seconds": 900, "factor": 3.0, "min_count": 10 });
    let inserted: NotificationSubscription = diesel::insert_into(notification_subscriptions::table)
        .values(NewNotificationSubscription {
            user_id,
            org_id: ids.org_id,
            scope_type: "project",
            scope_id: ids.project_id,
            kind: "error_spike",
            conditions: &conditions,
            delivery: "immediate",
            throttle_seconds: 900,
            quiet_start_min: Some(1320),
            quiet_end_min: Some(360),
            quiet_tz: "Europe/Paris",
        })
        .returning(NotificationSubscription::as_returning())
        .get_result(&mut conn)
        .await
        .expect("insert subscription");

    let read: NotificationSubscription = notification_subscriptions::table
        .find(inserted.id)
        .select(NotificationSubscription::as_select())
        .first(&mut conn)
        .await
        .expect("read subscription back");

    assert_eq!(read.user_id, user_id);
    assert_eq!(read.org_id, ids.org_id);
    assert_eq!(read.scope_type, "project");
    assert_eq!(read.scope_id, ids.project_id);
    assert_eq!(read.kind, "error_spike");
    assert!(read.enabled);
    assert_eq!(read.disabled_reason, None);
    assert_eq!(read.disabled_at, None);
    assert_eq!(read.conditions, conditions);
    assert_eq!(read.delivery, "immediate");
    assert_eq!(read.throttle_seconds, 900);
    assert_eq!(read.quiet_start_min, Some(1320));
    assert_eq!(read.quiet_end_min, Some(360));
    assert_eq!(read.quiet_tz, "Europe/Paris");

    db.cleanup().await;
}

/// The live bug this slice fixes. Since migration 33, `environments` is the
/// project-level catalogue and `error_events.environment_id` holds an
/// `app_environments` ENROLLMENT id, so `alert_count_errors`'s old subquery
/// (`environment_id IN (SELECT id FROM environments WHERE name = $5)`) compared
/// two disjoint id spaces and was always false: every environment-filtered
/// alert rule in the product counted zero and had never fired.
#[tokio::test]
async fn alert_count_errors_narrows_by_enrollment_id_not_catalogue_id() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let from = chrono::Utc::now() - chrono::Duration::days(2);
    let to = chrono::Utc::now() + chrono::Duration::days(1);

    let all =
        sauron_db::repo::alert_count_errors(&mut conn, &[ids.app_id], from, to, None, None, None)
            .await
            .expect("unfiltered count");
    assert_eq!(all, 7, "seed_two_envs inserts 7 error_events");

    let enrollments =
        sauron_db::repo::enrollment_ids_for_env_name(&mut conn, &[ids.app_id], "env_a")
            .await
            .expect("resolve env_a");
    assert_eq!(
        enrollments,
        vec![ids.env_a],
        "env_a resolves to its enrollment id"
    );

    let narrowed = sauron_db::repo::alert_count_errors(
        &mut conn,
        &[ids.app_id],
        from,
        to,
        None,
        Some(&enrollments),
        None,
    )
    .await
    .expect("narrowed count");
    assert_eq!(narrowed, 4, "env_a holds 4 of the 7 error_events");

    // The old shape, spelled out, so the regression is pinned rather than
    // described: a CATALOGUE id can never equal an enrollment id.
    let catalogue: Vec<uuid::Uuid> =
        sauron_db::repo::live_enrollments_for_apps(&mut conn, &[ids.app_id])
            .await
            .expect("live enrollments")
            .into_iter()
            .filter(|(enrollment, _, _)| *enrollment == ids.env_a)
            .map(|(_, _, catalogue_env)| catalogue_env)
            .collect();
    assert_eq!(catalogue.len(), 1);
    let wrong = sauron_db::repo::alert_count_errors(
        &mut conn,
        &[ids.app_id],
        from,
        to,
        None,
        Some(&catalogue),
        None,
    )
    .await
    .expect("catalogue-id count");
    assert_eq!(wrong, 0, "catalogue ids match nothing — that WAS the bug");

    db.cleanup().await;
}

/// Both `retired_at IS NULL` filters are load-bearing and only a DB test can
/// prove it: `(app_id, name)` is unique only among LIVE rows, so retiring
/// `staging` and creating a fresh `staging` leaves two rows with that name.
#[tokio::test]
async fn enrollment_ids_for_env_name_ignores_retired_rows() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let live = common::seed_env(
        &mut conn,
        ids.project_id,
        ids.app_id,
        "staging",
        "pk-staging-live",
        false,
    )
    .await;

    let found = sauron_db::repo::enrollment_ids_for_env_name(&mut conn, &[ids.app_id], "staging")
        .await
        .expect("resolve staging");
    assert_eq!(found, vec![live]);

    diesel::sql_query("UPDATE app_environments SET retired_at = now() WHERE id = $1")
        .bind::<diesel::sql_types::Uuid, _>(live)
        .execute(&mut conn)
        .await
        .expect("retire the enrollment");

    let found = sauron_db::repo::enrollment_ids_for_env_name(&mut conn, &[ids.app_id], "staging")
        .await
        .expect("resolve staging after retirement");
    assert!(
        found.is_empty(),
        "a retired enrollment must contribute nothing"
    );

    db.cleanup().await;
}

/// `issues` has no `environment_id`, so narrowing must go through
/// `error_events`. Bounding that EXISTS by the tick window would mix two
/// clocks — the window comes from the server-clock watermark while
/// `occurred_at` is SDK-supplied — and a backdated batch would create an issue
/// whose `created_at` is inside the window while every one of its events sits
/// outside it. So the EXISTS is bounded by the issue's OWN ingest-side
/// timestamps instead.
#[tokio::test]
async fn issue_env_narrowing_uses_the_issues_own_timestamps() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    // The harness pins every seeded `error_events.occurred_at` to today at
    // 12:00 UTC, but `issues.last_event_at` defaults to the wall clock at
    // INSERT and the harness never re-runs `upsert_issue` per event. Run the
    // suite before noon UTC and every seeded event sits AFTER the issue's own
    // ingest watermark, so the EXISTS below matches nothing and this test goes
    // red for reasons that have nothing to do with environments. Real ingest
    // cannot reach that state — `upsert_issue` advances `last_event_at` on
    // every occurrence — so replay that here rather than loosen the bound the
    // production query is being written to prove.
    diesel::sql_query(
        "UPDATE issues i SET last_event_at = GREATEST(i.last_event_at, e.newest) \
         FROM (SELECT issue_id, max(occurred_at) AS newest FROM error_events \
                WHERE app_id = $1 GROUP BY issue_id) e \
         WHERE i.id = e.issue_id",
    )
    .bind::<diesel::sql_types::Uuid, _>(ids.app_id)
    .execute(&mut conn)
    .await
    .expect("replay upsert_issue's last_event_at advance");

    let from = chrono::Utc::now() - chrono::Duration::days(2);
    let to = chrono::Utc::now() + chrono::Duration::days(1);

    let all = sauron_db::repo::alert_new_issues(&mut conn, &[ids.app_id], from, to, None, 21)
        .await
        .expect("unfiltered new issues");
    assert_eq!(
        all.len(),
        2,
        "seed_two_envs creates issue_id and issue_env_b_only"
    );

    // `issue_env_b_only`'s single error event lives in env_b alone, so an
    // env_a-narrowed probe must not see it.
    let only_a = sauron_db::repo::alert_new_issues_env(
        &mut conn,
        &[ids.app_id],
        from,
        to,
        None,
        &[ids.env_a],
        21,
    )
    .await
    .expect("env_a new issues");
    let a_ids: Vec<uuid::Uuid> = only_a.iter().map(|i| i.id).collect();
    assert!(a_ids.contains(&ids.issue_id));
    assert!(
        !a_ids.contains(&ids.issue_env_b_only),
        "an issue with no events in env_a must not appear under an env_a filter"
    );

    // The limit is the truncation sentinel: ask for 2 and get exactly 2 back.
    let capped = sauron_db::repo::alert_new_issues(&mut conn, &[ids.app_id], from, to, None, 1)
        .await
        .expect("limited new issues");
    assert_eq!(
        capped.len(),
        1,
        "LIMIT is a bound parameter, not a literal 20"
    );

    db.cleanup().await;
}

/// `upsert_subscription` writes the parent and REPLACES the env child rows in a
/// single data-modifying CTE — one statement, therefore atomic, without
/// `conn.transaction` (MSRV 1.82 blocks it).
#[tokio::test]
async fn upsert_subscription_replaces_the_env_set_in_one_statement() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let user_id = sauron_db::repo::find_user_by_email(&mut conn, &ids.owner_email)
        .await
        .unwrap()
        .unwrap()
        .id;

    // The subscription stores CATALOGUE ids; `seed_two_envs` hands back
    // ENROLLMENT ids, so resolve across.
    let catalogue: Vec<uuid::Uuid> =
        sauron_db::repo::live_catalogue_envs_for_project(&mut conn, ids.project_id)
            .await
            .expect("catalogue envs");
    assert_eq!(catalogue.len(), 2);

    let conditions = serde_json::json!({ "level": "error" });
    let sub = sauron_db::repo::upsert_subscription(
        &mut conn,
        user_id,
        ids.org_id,
        "project",
        ids.project_id,
        "error_new_issue",
        &conditions,
        "immediate",
        900,
        None,
        None,
        "UTC",
        &catalogue,
    )
    .await
    .expect("first upsert");
    let created_at = sub.created_at;

    let envs = sauron_db::repo::subscription_envs_for(&mut conn, &[sub.id])
        .await
        .expect("child rows");
    assert_eq!(envs.len(), 2);

    // Narrow the set to one environment.
    let again = sauron_db::repo::upsert_subscription(
        &mut conn,
        user_id,
        ids.org_id,
        "project",
        ids.project_id,
        "error_new_issue",
        &conditions,
        "daily",
        1800,
        Some(1320),
        Some(360),
        "Europe/Paris",
        &catalogue[..1],
    )
    .await
    .expect("second upsert");

    assert_eq!(
        again.id, sub.id,
        "the unique key made this an update, not an insert"
    );
    assert_eq!(
        again.created_at, created_at,
        "created_at must survive the upsert"
    );
    assert_eq!(again.delivery, "daily");
    assert_eq!(again.throttle_seconds, 1800);
    assert_eq!(again.quiet_tz, "Europe/Paris");

    let envs = sauron_db::repo::subscription_envs_for(&mut conn, &[sub.id])
        .await
        .expect("child rows after narrowing");
    assert_eq!(envs.len(), 1, "the removed environment's child row is gone");
    assert_eq!(envs[0].1, catalogue[0]);

    // A rejected insert leaves no orphaned child rows — which is what proves
    // the CTE is really one statement.
    let bad = sauron_db::repo::upsert_subscription(
        &mut conn,
        user_id,
        ids.org_id,
        "org",
        ids.project_id,
        "error_new_issue",
        &conditions,
        "immediate",
        900,
        None,
        None,
        "UTC",
        &catalogue,
    )
    .await;
    assert!(bad.is_err(), "scope_type='org' violates the CHECK");
    let envs = sauron_db::repo::subscription_envs_for(&mut conn, &[sub.id])
        .await
        .expect("child rows after the failed insert");
    assert_eq!(envs.len(), 1, "the failed statement wrote nothing at all");

    db.cleanup().await;
}

use sauron_db::repo::QueueInsert;

async fn seed_subscription(
    conn: &mut sauron_db::PgConn,
    ids: &common::SeedIds,
    kind: &str,
    delivery: &str,
    quiet: Option<(i16, i16)>,
    tz: &str,
) -> sauron_db::models::NotificationSubscription {
    let user_id = sauron_db::repo::find_user_by_email(conn, &ids.owner_email)
        .await
        .unwrap()
        .unwrap()
        .id;
    sauron_db::repo::upsert_subscription(
        conn,
        user_id,
        ids.org_id,
        "project",
        ids.project_id,
        kind,
        &serde_json::json!({}),
        delivery,
        900,
        quiet.map(|q| q.0),
        quiet.map(|q| q.1),
        tz,
        &[],
    )
    .await
    .expect("seed subscription")
}

/// Without a unique constraint `ON CONFLICT DO NOTHING` can only ever fire on
/// the id PK — i.e. never — and the clause would read as idempotency while
/// providing none. Scoping the index to LIVE rows is what lets the next
/// legitimate notification through after the first one sends.
#[tokio::test]
async fn the_live_dedup_index_suppresses_only_live_duplicates() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let sub = seed_subscription(&mut conn, &ids, "error_spike", "immediate", None, "UTC").await;

    let dedup = format!("sub:{}:spike:{}", sub.id, ids.app_id);
    let row = QueueInsert {
        subscription_id: sub.id,
        project_id: ids.project_id,
        app_id: Some(ids.app_id),
        includes_unattributed: true,
        kind: "error_spike",
        dedup_key: &dedup,
        severity: "warning",
        title: "Error spike",
        body: "30 errors vs 10",
        link: None,
        env_enrollments: vec![ids.env_a],
    };

    // Two identical enqueues in the SAME statement produce one row.
    let n = sauron_db::repo::enqueue_notifications(&mut conn, &[row.clone(), row.clone()])
        .await
        .expect("double enqueue");
    assert_eq!(n, 1);

    // A third while the first is still pending produces nothing.
    let n = sauron_db::repo::enqueue_notifications(&mut conn, std::slice::from_ref(&row))
        .await
        .expect("third enqueue");
    assert_eq!(n, 0);

    diesel::sql_query(
        "UPDATE notification_queue SET status='sent', sent_at=now(), finished_at=now()",
    )
    .execute(&mut conn)
    .await
    .expect("mark sent");

    // Once it has sent, the next legitimate notification is allowed through.
    let n = sauron_db::repo::enqueue_notifications(&mut conn, &[row])
        .await
        .expect("enqueue after send");
    assert_eq!(n, 1);

    db.cleanup().await;
}

/// `deliver_after` is computed entirely in SQL, because the workspace has no
/// `chrono-tz` and nothing in Rust can produce a subscription's local
/// wall-clock time. This asserts the SQL agrees with `in_quiet_hours` over the
/// cases that matter, including a `quiet_tz` Postgres does not know.
#[tokio::test]
async fn deliver_after_defers_into_quiet_hours_and_survives_an_unknown_zone() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    // A window that covers the whole day but one minute: whatever the wall
    // clock says, `now()` is inside it, so delivery must be pushed forward.
    let quiet = seed_subscription(
        &mut conn,
        &ids,
        "error_new_issue",
        "immediate",
        Some((1, 0)),
        "Europe/Paris",
    )
    .await;
    let dedup_q = format!("sub:{}:issue:{}", quiet.id, ids.issue_id);
    sauron_db::repo::enqueue_notifications(
        &mut conn,
        &[QueueInsert {
            subscription_id: quiet.id,
            project_id: ids.project_id,
            app_id: Some(ids.app_id),
            includes_unattributed: true,
            kind: "error_new_issue",
            dedup_key: &dedup_q,
            severity: "warning",
            title: "New issue",
            body: "body",
            link: None,
            env_enrollments: vec![],
        }],
    )
    .await
    .expect("enqueue quiet");

    // An unknown zone must fall back to UTC rather than raising and killing the
    // whole batch — a zone that validated at write time can vanish with an OS
    // tzdata update.
    let bogus = seed_subscription(
        &mut conn,
        &ids,
        "error_regression",
        "daily",
        Some((1320, 360)),
        "Missing/Zone",
    )
    .await;
    let dedup_b = format!("sub:{}:issue:{}", bogus.id, ids.issue_id);
    sauron_db::repo::enqueue_notifications(
        &mut conn,
        &[QueueInsert {
            subscription_id: bogus.id,
            project_id: ids.project_id,
            app_id: Some(ids.app_id),
            includes_unattributed: true,
            kind: "error_regression",
            dedup_key: &dedup_b,
            severity: "warning",
            title: "Regressed",
            body: "body",
            link: None,
            env_enrollments: vec![],
        }],
    )
    .await
    .expect("enqueue with an unknown zone");

    #[derive(diesel::QueryableByName)]
    struct Row {
        #[diesel(sql_type = diesel::sql_types::Uuid)]
        subscription_id: uuid::Uuid,
        #[diesel(sql_type = diesel::sql_types::Bool)]
        deferred: bool,
    }
    let rows: Vec<Row> = diesel::sql_query(
        "SELECT subscription_id, (deliver_after > now() + interval '30 seconds') AS deferred \
           FROM notification_queue",
    )
    .load(&mut conn)
    .await
    .expect("read deliver_after");
    assert_eq!(rows.len(), 2, "the unknown zone did not kill the batch");
    for r in rows {
        assert!(
            r.deferred,
            "subscription {} should have been deferred past its quiet window",
            r.subscription_id
        );
    }

    // The Rust twin agrees on the same shape. The exhaustive
    // agreement check — including the DST case — is the next test.
    assert!(sauron_alerts::subscription::in_quiet_hours(720, 1, 0));

    db.cleanup().await;
}

/// The SQL `CASE` and Rust's `in_quiet_hours` must agree on every case, and
/// only Postgres can turn `(now, tz)` into a local wall clock — `chrono-tz` is
/// not a dependency anywhere in this workspace and nothing in Rust here can do
/// it. Two implementations of one predicate drift silently, and the symptom of
/// the drift is somebody's phone at 04:00, so they are pinned to each other
/// over a shared table.
///
/// The two `Europe/Paris` rows on 2026-03-29 are the point of the test. The
/// clock jumps 02:00 -> 03:00 at 01:00 UTC that morning, so 01:30 UTC is 03:30
/// local, not 02:30. An implementation that computed the local minute by adding
/// a fixed offset — or that skipped the conversion entirely — puts it at 02:30,
/// inside the window, and holds the message for an hour it should have gone.
#[tokio::test]
async fn the_quiet_hours_sql_and_the_rust_twin_agree_over_a_shared_table() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;

    // (tz, instant in UTC, quiet_start_min, quiet_end_min, expected local
    //  minute, expected "is quiet")
    let cases: Vec<(&str, &str, i32, i32, i32, bool)> = vec![
        // Wrap-around window, UTC: inside, then outside.
        ("UTC", "2026-01-15T23:30:00Z", 1320, 360, 1410, true),
        ("UTC", "2026-01-15T07:00:00Z", 1320, 360, 420, false),
        // The start minute is inside; the end minute is outside.
        ("UTC", "2026-01-15T22:00:00Z", 1320, 360, 1320, true),
        ("UTC", "2026-01-15T06:00:00Z", 1320, 360, 360, false),
        // A zero-width window must not silence everything forever.
        ("UTC", "2026-01-15T05:00:00Z", 300, 300, 300, false),
        // Winter Paris is UTC+1: 22:30Z is 23:30 local (inside), 20:30Z is
        // 21:30 local (outside). A UTC-only implementation gets both backwards.
        (
            "Europe/Paris",
            "2026-01-15T22:30:00Z",
            1320,
            360,
            1410,
            true,
        ),
        (
            "Europe/Paris",
            "2026-01-15T20:30:00Z",
            1320,
            360,
            1290,
            false,
        ),
        // Spring-forward morning, window 01:00 -> 03:00 local.
        // 00:30Z is still CET (+1) => 01:30 local, inside.
        ("Europe/Paris", "2026-03-29T00:30:00Z", 60, 180, 90, true),
        // 01:30Z is already CEST (+2) => 03:30 local, OUTSIDE. Naive +1 would
        // say 02:30 and defer.
        ("Europe/Paris", "2026-03-29T01:30:00Z", 60, 180, 210, false),
    ];

    let idx: Vec<i32> = (0..cases.len() as i32).collect();
    let tzs: Vec<String> = cases.iter().map(|c| c.0.to_string()).collect();
    let ats: Vec<chrono::DateTime<chrono::Utc>> = cases
        .iter()
        .map(|c| {
            chrono::DateTime::parse_from_rfc3339(c.1)
                .expect("rfc3339 case instant")
                .with_timezone(&chrono::Utc)
        })
        .collect();
    let starts: Vec<i32> = cases.iter().map(|c| c.2).collect();
    let ends: Vec<i32> = cases.iter().map(|c| c.3).collect();

    #[derive(diesel::QueryableByName)]
    struct Verdict {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        idx: i32,
        #[diesel(sql_type = diesel::sql_types::Integer)]
        local_min: i32,
        #[diesel(sql_type = diesel::sql_types::Bool)]
        quiet: bool,
    }

    // Character-for-character the branch structure of the `CASE` inside
    // `enqueue_notifications`: equal bounds are never quiet, `start < end` is a
    // same-day half-open interval, otherwise it wraps midnight.
    let rows: Vec<Verdict> = diesel::sql_query(
        "SELECT s.idx, s.local_min, \
                CASE \
                  WHEN s.qs = s.qe THEN false \
                  WHEN s.qs < s.qe THEN (s.local_min >= s.qs AND s.local_min < s.qe) \
                  ELSE (s.local_min >= s.qs OR s.local_min < s.qe) \
                END AS quiet \
           FROM ( \
             SELECT c.idx, c.qs, c.qe, \
                    (EXTRACT(HOUR FROM (c.at AT TIME ZONE c.tz)) * 60 \
                     + EXTRACT(MINUTE FROM (c.at AT TIME ZONE c.tz)))::int AS local_min \
               FROM unnest($1::int[], $2::text[], $3::timestamptz[], $4::int[], $5::int[]) \
                      AS c(idx, tz, at, qs, qe) \
           ) s \
          ORDER BY s.idx",
    )
    .bind::<diesel::sql_types::Array<diesel::sql_types::Integer>, _>(idx)
    .bind::<diesel::sql_types::Array<diesel::sql_types::Text>, _>(tzs)
    .bind::<diesel::sql_types::Array<diesel::sql_types::Timestamptz>, _>(ats)
    .bind::<diesel::sql_types::Array<diesel::sql_types::Integer>, _>(starts)
    .bind::<diesel::sql_types::Array<diesel::sql_types::Integer>, _>(ends)
    .load(&mut conn)
    .await
    .expect("evaluate the quiet-hours table in SQL");

    assert_eq!(rows.len(), cases.len());
    for row in rows {
        let (tz, at, start, end, want_local, want_quiet) = cases[row.idx as usize];
        assert_eq!(
            row.local_min, want_local,
            "{tz} at {at}: Postgres computed local minute {} not {want_local}",
            row.local_min
        );
        assert_eq!(
            row.quiet, want_quiet,
            "{tz} at {at}: the SQL CASE disagrees with the expected verdict"
        );
        assert_eq!(
            sauron_alerts::subscription::in_quiet_hours(row.local_min, start, end),
            row.quiet,
            "{tz} at {at}: in_quiet_hours and the SQL CASE have drifted apart"
        );
    }

    db.cleanup().await;
}

/// The durable fallback for when Redis is unreachable — the direct analogue of
/// `alert_recently_sent`.
#[tokio::test]
async fn notification_recently_queued_is_the_durable_throttle_backstop() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let sub = seed_subscription(&mut conn, &ids, "error_spike", "immediate", None, "UTC").await;
    let dedup = format!("sub:{}:spike:{}", sub.id, ids.app_id);

    assert!(
        !sauron_db::repo::notification_recently_queued(&mut conn, sub.id, &dedup, 900)
            .await
            .unwrap()
    );
    sauron_db::repo::enqueue_notifications(
        &mut conn,
        &[QueueInsert {
            subscription_id: sub.id,
            project_id: ids.project_id,
            app_id: Some(ids.app_id),
            includes_unattributed: true,
            kind: "error_spike",
            dedup_key: &dedup,
            severity: "warning",
            title: "t",
            body: "b",
            link: None,
            env_enrollments: vec![],
        }],
    )
    .await
    .unwrap();
    assert!(
        sauron_db::repo::notification_recently_queued(&mut conn, sub.id, &dedup, 900)
            .await
            .unwrap()
    );
    assert!(
        !sauron_db::repo::notification_recently_queued(&mut conn, sub.id, &dedup, 0)
            .await
            .unwrap(),
        "a zero window never suppresses"
    );

    db.cleanup().await;
}

/// `FOR UPDATE SKIP LOCKED` alone only skips rows locked by an UNCOMMITTED
/// transaction; once replica A commits, replica B's next pass re-selects the
/// same rows and mails them again. A `claimed` status that leaves the partial
/// index is what makes the claim real — so the third pass, run after both
/// commit, is the assertion that matters.
#[tokio::test]
async fn claiming_is_exclusive_across_passes_not_just_across_transactions() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let sub = seed_subscription(&mut conn, &ids, "error_spike", "immediate", None, "UTC").await;

    let dedups: Vec<String> = (0..6)
        .map(|i| format!("sub:{}:spike:{i}", sub.id))
        .collect();
    let rows: Vec<QueueInsert> = dedups
        .iter()
        .map(|d| QueueInsert {
            subscription_id: sub.id,
            project_id: ids.project_id,
            app_id: Some(ids.app_id),
            includes_unattributed: true,
            kind: "error_spike",
            dedup_key: d,
            severity: "warning",
            title: "t",
            body: "b",
            link: None,
            env_enrollments: vec![],
        })
        .collect();
    assert_eq!(
        sauron_db::repo::enqueue_notifications(&mut conn, &rows)
            .await
            .unwrap(),
        6
    );

    let first = sauron_db::repo::claim_due_notifications(&mut conn, 4)
        .await
        .unwrap();
    assert_eq!(first.len(), 4);
    assert!(first
        .iter()
        .all(|r| r.status == "claimed" && r.attempts == 1));

    let second = sauron_db::repo::claim_due_notifications(&mut conn, 4)
        .await
        .unwrap();
    assert_eq!(
        second.len(),
        2,
        "the already-claimed rows are not re-selected"
    );

    let mut all: Vec<uuid::Uuid> = first.iter().chain(second.iter()).map(|r| r.id).collect();
    all.sort_unstable();
    all.dedup();
    assert_eq!(all.len(), 6, "disjoint claim sets");

    let third = sauron_db::repo::claim_due_notifications(&mut conn, 4)
        .await
        .unwrap();
    assert!(third.is_empty(), "a committed claim is not re-claimable");

    db.cleanup().await;
}

/// A crash between claim and terminal status must be recoverable, not an
/// infinite redelivery loop — `attempts` is what makes the give-up decision
/// reachable.
#[tokio::test]
async fn stuck_claims_return_to_pending_then_fail_at_the_attempt_cap() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let sub = seed_subscription(&mut conn, &ids, "error_spike", "immediate", None, "UTC").await;
    let dedup = format!("sub:{}:spike:one", sub.id);
    sauron_db::repo::enqueue_notifications(
        &mut conn,
        &[QueueInsert {
            subscription_id: sub.id,
            project_id: ids.project_id,
            app_id: Some(ids.app_id),
            includes_unattributed: true,
            kind: "error_spike",
            dedup_key: &dedup,
            severity: "warning",
            title: "t",
            body: "b",
            link: None,
            env_enrollments: vec![],
        }],
    )
    .await
    .unwrap();
    sauron_db::repo::claim_due_notifications(&mut conn, 10)
        .await
        .unwrap();

    diesel::sql_query("UPDATE notification_queue SET claimed_at = now() - interval '20 minutes'")
        .execute(&mut conn)
        .await
        .unwrap();
    let n = sauron_db::repo::requeue_stuck_notifications(&mut conn, 900, 3)
        .await
        .unwrap();
    assert_eq!(n, 1);

    let back = sauron_db::repo::claim_due_notifications(&mut conn, 10)
        .await
        .unwrap();
    assert_eq!(back.len(), 1);
    assert_eq!(back[0].attempts, 2);

    diesel::sql_query(
        "UPDATE notification_queue SET attempts = 3, claimed_at = now() - interval '20 minutes'",
    )
    .execute(&mut conn)
    .await
    .unwrap();
    sauron_db::repo::requeue_stuck_notifications(&mut conn, 900, 3)
        .await
        .unwrap();

    #[derive(diesel::QueryableByName)]
    struct S {
        #[diesel(sql_type = diesel::sql_types::Text)]
        status: String,
        #[diesel(sql_type = diesel::sql_types::Bool)]
        finished: bool,
    }
    let rows: Vec<S> = diesel::sql_query(
        "SELECT status, (finished_at IS NOT NULL) AS finished FROM notification_queue",
    )
    .load(&mut conn)
    .await
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, "failed");
    assert!(rows[0].finished);

    db.cleanup().await;
}

/// A row that fails DETERMINISTICALLY must still stop.
///
/// `fail_notifications` returns a row to `pending`, which
/// `requeue_stuck_notifications` can never see — it matches only
/// `status = 'claimed'`. So if `fail_notifications` did not apply the attempts
/// cap itself, a body that fails to render every single pass would be claimed,
/// failed and re-queued forever with nothing in the system able to break the
/// loop. This test drives exactly that: claim, fail, claim, fail, claim, fail.
#[tokio::test]
async fn a_deterministic_failure_stops_at_the_attempt_cap() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let sub = seed_subscription(&mut conn, &ids, "error_spike", "immediate", None, "UTC").await;
    let dedup = format!("sub:{}:spike:doomed", sub.id);
    sauron_db::repo::enqueue_notifications(
        &mut conn,
        &[QueueInsert {
            subscription_id: sub.id,
            project_id: ids.project_id,
            app_id: Some(ids.app_id),
            includes_unattributed: true,
            kind: "error_spike",
            dedup_key: &dedup,
            severity: "warning",
            title: "t",
            body: "b",
            link: None,
            env_enrollments: vec![],
        }],
    )
    .await
    .unwrap();

    for expected_attempt in 1..=3i16 {
        let claimed = sauron_db::repo::claim_due_notifications(&mut conn, 10)
            .await
            .unwrap();
        assert_eq!(
            claimed.len(),
            1,
            "attempt {expected_attempt} should be claimable"
        );
        assert_eq!(claimed[0].attempts, expected_attempt);
        let queue_ids: Vec<uuid::Uuid> = claimed.iter().map(|r| r.id).collect();
        sauron_db::repo::fail_notifications(&mut conn, &queue_ids, "render exploded", 3)
            .await
            .unwrap();
    }

    let after = sauron_db::repo::claim_due_notifications(&mut conn, 10)
        .await
        .unwrap();
    assert!(
        after.is_empty(),
        "the third failure was terminal; a fourth claim means the cap is not applied"
    );

    #[derive(diesel::QueryableByName)]
    struct S {
        #[diesel(sql_type = diesel::sql_types::Text)]
        status: String,
        #[diesel(sql_type = diesel::sql_types::Bool)]
        finished: bool,
    }
    let rows: Vec<S> = diesel::sql_query(
        "SELECT status, (finished_at IS NOT NULL) AS finished FROM notification_queue",
    )
    .load(&mut conn)
    .await
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, "failed");
    assert!(
        rows[0].finished,
        "a terminal row must carry finished_at or the prune never reaps it"
    );

    db.cleanup().await;
}

/// Pruning on `created_at` with no status guard would destroy still-`pending`
/// rows — precisely the evidence of the outage that made them pile up.
#[tokio::test]
async fn the_prune_never_touches_pending_or_claimed_rows() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let sub = seed_subscription(&mut conn, &ids, "error_spike", "immediate", None, "UTC").await;
    let dedups: Vec<String> = (0..3)
        .map(|i| format!("sub:{}:spike:{i}", sub.id))
        .collect();
    let rows: Vec<QueueInsert> = dedups
        .iter()
        .map(|d| QueueInsert {
            subscription_id: sub.id,
            project_id: ids.project_id,
            app_id: Some(ids.app_id),
            includes_unattributed: true,
            kind: "error_spike",
            dedup_key: d,
            severity: "warning",
            title: "t",
            body: "b",
            link: None,
            env_enrollments: vec![],
        })
        .collect();
    sauron_db::repo::enqueue_notifications(&mut conn, &rows)
        .await
        .unwrap();

    // Age every row past any retention, then finish exactly one of them.
    diesel::sql_query("UPDATE notification_queue SET created_at = now() - interval '400 days'")
        .execute(&mut conn)
        .await
        .unwrap();
    diesel::sql_query(
        "UPDATE notification_queue SET status='sent', sent_at=now(), \
         finished_at = now() - interval '400 days' WHERE dedup_key = $1",
    )
    .bind::<diesel::sql_types::Text, _>(&dedups[0])
    .execute(&mut conn)
    .await
    .unwrap();

    let pruned = sauron_db::repo::prune_notification_queue(&mut conn, 14)
        .await
        .unwrap();
    assert_eq!(pruned, 1, "only the finished row goes");

    let left: i64 = diesel::sql_query("SELECT count(*) AS n FROM notification_queue")
        .get_result::<sauron_db::repo::AlertCountRow>(&mut conn)
        .await
        .unwrap()
        .n;
    assert_eq!(left, 2);

    db.cleanup().await;
}

/// The overwhelmingly common revocation is PARTIAL — moved off a project, an
/// env grant narrowed, a role downgraded so it no longer carries `issue:read`
/// — and in every one of those the user still holds grants in the org. So
/// "does this user still have any grants here" is the wrong question, and this
/// pins the right one: the subscription's own scope, against its own required
/// permission.
#[tokio::test]
async fn a_partial_revocation_still_leaves_grants_in_the_org() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let user_id = sauron_db::repo::find_user_by_email(&mut conn, &ids.owner_email)
        .await
        .unwrap()
        .unwrap()
        .id;

    let sub = seed_subscription(&mut conn, &ids, "error_new_issue", "immediate", None, "UTC").await;
    assert!(sub.enabled);

    sauron_db::repo::disable_subscription(&mut conn, sub.id, "access_revoked")
        .await
        .expect("disable");

    let after = sauron_db::repo::get_subscription(&mut conn, sub.id)
        .await
        .unwrap()
        .unwrap();
    assert!(!after.enabled);
    assert_eq!(after.disabled_reason.as_deref(), Some("access_revoked"));
    assert!(after.disabled_at.is_some());

    // Still visible to its owner and to the sweep's org-scoped query, so the
    // card can explain WHY it is off instead of it looking broken.
    let mine = sauron_db::repo::list_subscriptions_for_user(&mut conn, user_id)
        .await
        .unwrap();
    assert_eq!(mine.len(), 1);
    let live = sauron_db::repo::subscriptions_for_user_in_org(&mut conn, user_id, ids.org_id)
        .await
        .unwrap();
    assert!(live.is_empty(), "the sweep only re-evaluates enabled rows");

    db.cleanup().await;
}

/// The delivery-time re-check. The write-time check is a point-in-time snapshot
/// and reach can be revoked afterwards, so the drain repeats the whole
/// computation against freshly loaded grants immediately before rendering — the
/// last moment the data is still inside the trust boundary. A dropped row's
/// content is blanked in the SAME statement that marks it, because it has no
/// further purpose and must not sit at rest for the retention window outside
/// the reader's authorization.
#[tokio::test]
async fn dropping_a_row_for_lost_access_blanks_its_content() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let sub = seed_subscription(&mut conn, &ids, "error_new_issue", "immediate", None, "UTC").await;
    let dedup = format!("sub:{}:issue:{}", sub.id, ids.issue_id);
    sauron_db::repo::enqueue_notifications(
        &mut conn,
        &[QueueInsert {
            subscription_id: sub.id,
            project_id: ids.project_id,
            app_id: Some(ids.app_id),
            includes_unattributed: true,
            kind: "error_new_issue",
            dedup_key: &dedup,
            severity: "warning",
            title: "Secret issue title",
            body: "Secret body",
            link: Some("https://example.test/#/issues/1"),
            env_enrollments: vec![],
        }],
    )
    .await
    .unwrap();

    let claimed = sauron_db::repo::claim_due_notifications(&mut conn, 10)
        .await
        .unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].title.as_deref(), Some("Secret issue title"));

    let n = sauron_db::repo::drop_notifications(&mut conn, &[claimed[0].id], "dropped_no_access")
        .await
        .unwrap();
    assert_eq!(n, 1);

    let after = sauron_db::repo::notification_history_for_user(&mut conn, claimed[0].user_id, 10)
        .await
        .unwrap();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].status, "dropped_no_access");
    assert_eq!(after[0].title, None);
    assert_eq!(after[0].body, None);
    assert_eq!(after[0].link, None);
    assert!(after[0].finished_at.is_some());

    db.cleanup().await;
}

/// A project whose admin configured NO monitor_down/monitor_up alert rule is
/// exactly the deployment where a personal uptime subscription is the entire
/// point. `notify_transition` used to `return` on `rules.is_empty()`, so under
/// that early return the enqueue would never happen, forever, with no log line
/// — and that is invisible to every other test in the repository.
#[tokio::test]
async fn a_project_with_zero_alert_rules_still_has_uptime_subscribers() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let sub = seed_subscription(&mut conn, &ids, "uptime", "immediate", None, "UTC").await;

    let rules = sauron_db::repo::alert_rules_for_monitor(&mut conn, ids.project_id, "monitor_down")
        .await
        .expect("load rules");
    assert!(rules.is_empty(), "the harness configures no alert rules");

    let found = sauron_db::repo::uptime_subscriptions_for_project(&mut conn, ids.project_id)
        .await
        .expect("uptime subscriptions");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, sub.id);

    // And the enqueue path itself works with `app_id = NULL` — uptime has no app
    // dimension, because `monitors` carries only `project_id`.
    let monitor_id = uuid::Uuid::from_u128(7);
    let dedup = format!("sub:{}:monitor:{monitor_id}:monitor_down", sub.id);
    let n = sauron_db::repo::enqueue_notifications(
        &mut conn,
        &[QueueInsert {
            subscription_id: sub.id,
            project_id: ids.project_id,
            app_id: None,
            includes_unattributed: false,
            kind: "uptime",
            dedup_key: &dedup,
            severity: "critical",
            title: "Monitor down: api",
            body: "api (https://example.test) is DOWN",
            link: None,
            env_enrollments: vec![],
        }],
    )
    .await
    .expect("enqueue uptime notification");
    assert_eq!(n, 1);

    db.cleanup().await;
}
