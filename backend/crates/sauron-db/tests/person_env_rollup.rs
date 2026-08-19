//! `event_user_environments` — the per-(person, environment) rollup that lets
//! the Users Explorer page without a blocking Sort over every person in the app.
//!
//! See `docs/superpowers/specs/2026-08-12-persons-env-slowness-design.md`.

mod common;

use common::TestDb;
use diesel::prelude::*;
use diesel::sql_types::{BigInt, Text, Timestamptz, Uuid as SqlUuid};
use diesel_async::RunQueryDsl;
use sauron_db::repo::TimeWindow;
use sauron_db::scope::{EnvFilter, ReadScope};

/// `environment_id` is NULLABLE and `EnvFilter::Unattributed` is a real row, so
/// uniqueness cannot be a plain unique constraint — NULL never equals NULL, and
/// a plain `UNIQUE (app_id, distinct_id, environment_id)` would let one person
/// accumulate unlimited unattributed rows while every upsert against them
/// inserted instead of updating. The unique index is over
/// `COALESCE(environment_id, nil-uuid)`, and the upsert's `ON CONFLICT` has to
/// name that same expression or it silently degrades into an unconstrained
/// insert.
#[tokio::test]
async fn unattributed_rollup_rows_are_unique_per_person() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let stmt = || {
        diesel::sql_query(
            "INSERT INTO event_user_environments \
               (app_id, distinct_id, environment_id, first_seen, last_seen) \
             VALUES ($1, 'uniq-person', NULL, now(), now())",
        )
        .bind::<SqlUuid, _>(ids.app_id)
    };

    stmt().execute(&mut conn).await.expect("first insert");
    let second = stmt().execute(&mut conn).await;
    assert!(
        second.is_err(),
        "a second NULL-environment row for the same person must be rejected by \
         event_user_env_key_idx"
    );

    drop(conn);
    db.cleanup().await;
}

/// The marker table exists and starts empty, so `list_persons` falls back to the
/// live query for every app until a backfill says otherwise.
#[tokio::test]
async fn backfill_marker_table_starts_empty() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    #[derive(QueryableByName)]
    struct Count {
        #[diesel(sql_type = BigInt)]
        n: i64,
    }

    let c: Count =
        diesel::sql_query("SELECT count(*) AS n FROM event_user_env_backfill WHERE app_id = $1")
            .bind::<SqlUuid, _>(ids.app_id)
            .get_result(&mut conn)
            .await
            .expect("marker table must exist");

    assert_eq!(c.n, 0, "a freshly migrated app has no backfill marker");

    drop(conn);
    db.cleanup().await;
}

/// A session bumped across several batches must be reported as inserted exactly
/// once. `event_user_environments.sessions_count` is driven by this, and a naive
/// "+1 per bump" over-counts by however many batches the session spans — which
/// a single-batch test cannot see.
#[tokio::test]
async fn bump_sessions_reports_inserts_only_once() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let bump = || sauron_db::batch::SessionBump {
        app_id: ids.app_id,
        session_id: "s-repeat".to_string(),
        distinct_id: Some(ids.shared_distinct_id.clone()),
        device_key: None,
        first_at: chrono::Utc::now(),
        last_at: chrono::Utc::now(),
        context: serde_json::json!({}),
        release: None,
        environment_id: Some(ids.env_a),
        ip: None,
        events_delta: 1,
        errors_delta: 0,
        unhandled_delta: 0,
    };

    let first = sauron_db::batch::bump_sessions(&mut conn, &[bump()])
        .await
        .expect("first bump");
    assert_eq!(
        first,
        vec![(ids.app_id, "s-repeat".to_string())],
        "the first bump inserts the session and must report it"
    );

    let second = sauron_db::batch::bump_sessions(&mut conn, &[bump()])
        .await
        .expect("second bump");
    assert!(
        second.is_empty(),
        "a repeat bump UPDATES rather than inserts and must report nothing, got {second:?}"
    );

    drop(conn);
    db.cleanup().await;
}

/// The conflict arm must drive `first_seen` through `LEAST` and `last_seen`
/// through `GREATEST`, so a later batch carrying an OLDER signal moves
/// `first_seen` backwards rather than overwriting it forwards. Applied newest
/// first, because plain assignment would look correct in the other order.
///
/// Note what this does NOT do: pass two rows with the same conflict key in one
/// call. `ON CONFLICT DO UPDATE` refuses to touch a row twice in one statement,
/// so this module's contract (see its header) is that callers fold duplicates in
/// memory first — `Acc::person_env` is what does that, and
/// `pipeline::batch`'s tests are where that folding is asserted.
#[tokio::test]
async fn person_env_upsert_widens_the_seen_window_in_both_directions() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let old = chrono::Utc::now() - chrono::Duration::days(10);
    let new = chrono::Utc::now();

    let mk = |at: chrono::DateTime<chrono::Utc>, ev: i64| sauron_db::batch::PersonEnvBump {
        app_id: ids.app_id,
        distinct_id: "fold-person".to_string(),
        environment_id: Some(ids.env_a),
        first_at: at,
        last_at: at,
        events_delta: ev,
        errors_delta: 0,
        sessions_delta: 0,
    };

    sauron_db::batch::bump_person_envs(&mut conn, &[mk(new, 1)])
        .await
        .expect("newest first");
    sauron_db::batch::bump_person_envs(&mut conn, &[mk(old, 2)])
        .await
        .expect("older second");

    #[derive(QueryableByName)]
    struct Row {
        #[diesel(sql_type = Timestamptz)]
        first_seen: chrono::DateTime<chrono::Utc>,
        #[diesel(sql_type = Timestamptz)]
        last_seen: chrono::DateTime<chrono::Utc>,
        #[diesel(sql_type = BigInt)]
        events_count: i64,
    }

    let row: Row = diesel::sql_query(
        "SELECT first_seen, last_seen, events_count FROM event_user_environments \
         WHERE app_id=$1 AND distinct_id='fold-person' AND environment_id=$2",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .bind::<SqlUuid, _>(ids.env_a)
    .get_result(&mut conn)
    .await
    .expect("exactly one rollup row");

    assert_eq!(row.events_count, 3, "deltas add across calls");
    assert!(
        (row.first_seen - old).num_seconds().abs() < 2,
        "first_seen must move BACKWARDS to the older signal (LEAST), not be overwritten \
         by the last write"
    );
    assert!(
        (row.last_seen - new).num_seconds().abs() < 2,
        "last_seen must stay at the newer signal (GREATEST), not be dragged back by the \
         older write"
    );

    drop(conn);
    db.cleanup().await;
}

/// The unattributed row (`environment_id IS NULL`) must upsert, not accumulate
/// duplicates — the `ON CONFLICT` has to name the `COALESCE` expression index or
/// it silently becomes an unconstrained insert and every read doubles.
#[tokio::test]
async fn person_env_upsert_handles_null_environment() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let mk = || sauron_db::batch::PersonEnvBump {
        app_id: ids.app_id,
        distinct_id: "null-env-person".to_string(),
        environment_id: None,
        first_at: chrono::Utc::now(),
        last_at: chrono::Utc::now(),
        events_delta: 1,
        errors_delta: 0,
        sessions_delta: 0,
    };

    sauron_db::batch::bump_person_envs(&mut conn, &[mk()])
        .await
        .expect("first");
    sauron_db::batch::bump_person_envs(&mut conn, &[mk()])
        .await
        .expect("second");

    #[derive(QueryableByName)]
    struct Count {
        #[diesel(sql_type = BigInt)]
        n: i64,
        #[diesel(sql_type = BigInt)]
        events_count: i64,
    }

    let c: Count = diesel::sql_query(
        "SELECT count(*) AS n, COALESCE(max(events_count),0) AS events_count \
         FROM event_user_environments \
         WHERE app_id=$1 AND distinct_id='null-env-person'",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .get_result(&mut conn)
    .await
    .expect("count");

    assert_eq!(
        c.n, 1,
        "two bumps must produce ONE unattributed row, not two"
    );
    assert_eq!(
        c.events_count, 2,
        "and its counter must have accumulated both bumps"
    );

    drop(conn);
    db.cleanup().await;
}

/// The backfill runs while ingest is live, so it must ADD to whatever the write
/// path has already written rather than skip rows that already exist. This is
/// the test that catches the `ON CONFLICT DO NOTHING` mistake — which loses a
/// person's entire history, silently and permanently.
#[tokio::test]
async fn backfill_adds_to_rows_the_write_path_already_created() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    // Anchored to the FIXTURE's clock, not the wall clock. `seed_two_envs`
    // pins every seeded timestamp to today at 12:00 UTC (see its own `now`
    // local), while this cutoff bounds the backfill's aggregate with
    // `occurred_at < cutoff`. A plain `Utc::now()` therefore excludes the
    // entire fixture on any run before ~11:57 UTC — the seeded rows are in
    // the FUTURE relative to the cutoff — and this test failed its own
    // "must have pre-cutoff analytics rows" precondition every morning.
    // Reproduced deliberately (cutoff moved an hour BEFORE `pinned_now`) and
    // confirmed as the cause before this fix.
    //
    // `+ 1 hour` because the fixture's largest positive offset from its
    // anchor is +5 seconds, so an hour clears every seeded row while staying
    // inside the same UTC day — which the day-bucketing assertions elsewhere
    // in the fixture depend on.
    let cutoff = ids.pinned_now + chrono::Duration::hours(1);

    // What the seed put in analytics_events for this identity in env_a, which
    // is what the backfill's cutoff-bounded aggregate should find.
    #[derive(QueryableByName)]
    struct N {
        #[diesel(sql_type = BigInt)]
        n: i64,
    }
    let seeded: N = diesel::sql_query(
        "SELECT count(*) AS n FROM analytics_events \
         WHERE app_id=$1 AND distinct_id=$2 AND environment_id=$3 AND occurred_at < $4",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .bind::<Text, _>(ids.shared_distinct_id.clone())
    .bind::<SqlUuid, _>(ids.env_a)
    .bind::<Timestamptz, _>(cutoff)
    .get_result(&mut conn)
    .await
    .expect("seeded analytics count");
    assert!(
        seeded.n > 0,
        "fixture precondition: the identity must have pre-cutoff analytics rows"
    );

    // A live bump landing before the backfill reaches this person.
    sauron_db::batch::bump_person_envs(
        &mut conn,
        &[sauron_db::batch::PersonEnvBump {
            app_id: ids.app_id,
            distinct_id: ids.shared_distinct_id.clone(),
            environment_id: Some(ids.env_a),
            first_at: cutoff,
            last_at: cutoff,
            events_delta: 1,
            errors_delta: 0,
            sessions_delta: 0,
        }],
    )
    .await
    .expect("live bump");

    sauron_db::person_env_backfill::backfill_app(&mut conn, ids.app_id, cutoff)
        .await
        .expect("backfill");

    let row: N = diesel::sql_query(
        "SELECT events_count AS n FROM event_user_environments \
         WHERE app_id=$1 AND distinct_id=$2 AND environment_id=$3",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .bind::<Text, _>(ids.shared_distinct_id.clone())
    .bind::<SqlUuid, _>(ids.env_a)
    .get_result(&mut conn)
    .await
    .expect("rollup row");

    assert_eq!(
        row.n,
        seeded.n + 1,
        "backfill must ADD its cutoff-bounded aggregate to the row the live \
         write path already created, not skip it"
    );

    drop(conn);
    db.cleanup().await;
}

/// The marker must never be visible before the data it claims. If it is, reads
/// switch to a half-populated rollup and the persons page goes quiet-wrong
/// instead of erroring.
#[tokio::test]
async fn marker_is_absent_until_the_backfill_finishes() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    assert!(
        !sauron_db::person_env_backfill::is_backfilled(&mut conn, ids.app_id)
            .await
            .expect("marker check"),
        "an app with no backfill row must read as not backfilled"
    );

    sauron_db::person_env_backfill::backfill_app(&mut conn, ids.app_id, chrono::Utc::now())
        .await
        .expect("backfill");

    assert!(
        sauron_db::person_env_backfill::is_backfilled(&mut conn, ids.app_id)
            .await
            .expect("marker check"),
        "backfill_app must write the marker in the same transaction as its aggregate"
    );

    drop(conn);
    db.cleanup().await;
}

/// `sessions_count` is credited inside the write transaction, from the sessions
/// that write actually INSERTED. Writing the same batch twice must leave
/// `sessions_count` at 1 while `events_count` doubles — that asymmetry is the
/// whole point, and a test that writes once cannot see it.
///
/// It also covers the dedupe trap: the person here has BOTH an event bump and a
/// newly-inserted session in the same write, so the crediting step must merge
/// into the existing row rather than push a second row with the same conflict
/// key (which would abort the batch with "cannot affect row a second time").
#[tokio::test]
async fn write_rows_credits_a_session_once_across_batches() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let did = "wr-person".to_string();

    let session = sauron_db::batch::SessionBump {
        app_id: ids.app_id,
        session_id: "wr-session".to_string(),
        distinct_id: Some(did.clone()),
        device_key: None,
        first_at: chrono::Utc::now(),
        last_at: chrono::Utc::now(),
        context: serde_json::json!({}),
        release: None,
        environment_id: Some(ids.env_a),
        ip: None,
        events_delta: 1,
        errors_delta: 0,
        unhandled_delta: 0,
    };
    let person = sauron_db::batch::PersonEnvBump {
        app_id: ids.app_id,
        distinct_id: did.clone(),
        environment_id: Some(ids.env_a),
        first_at: chrono::Utc::now(),
        last_at: chrono::Utc::now(),
        events_delta: 1,
        errors_delta: 0,
        sessions_delta: 0,
    };

    for _ in 0..2 {
        sauron_db::batch::write_rows(
            &mut conn,
            sauron_db::batch::WriteSet {
                errors: &[],
                analytics: &[],
                transactions: &[],
                sessions: std::slice::from_ref(&session),
                devices: &[],
                touch_users: &[],
                identified: &[],
                person_envs: std::slice::from_ref(&person),
                device_envs: &[],
            },
        )
        .await
        .expect("write_rows");
    }

    #[derive(QueryableByName)]
    struct Row {
        #[diesel(sql_type = BigInt)]
        events_count: i64,
        #[diesel(sql_type = BigInt)]
        sessions_count: i64,
    }

    let row: Row = diesel::sql_query(
        "SELECT events_count, sessions_count FROM event_user_environments \
         WHERE app_id=$1 AND distinct_id=$2 AND environment_id=$3",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .bind::<Text, _>(did)
    .bind::<SqlUuid, _>(ids.env_a)
    .get_result(&mut conn)
    .await
    .expect("exactly one rollup row");

    assert_eq!(row.events_count, 2, "two batches, two events");
    assert_eq!(
        row.sessions_count, 1,
        "one session across two batches is ONE session, not one per batch"
    );

    drop(conn);
    db.cleanup().await;
}

/// The rollup branch must return exactly what the live branch returns, for
/// every scope. Both branches are "correct" in isolation; the failure this
/// catches is the two disagreeing, which no single-branch test can see — and
/// which would surface as a page whose numbers change the day an operator runs
/// the backfill.
#[tokio::test]
async fn rollup_branch_matches_live_branch_for_every_scope() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let scopes = vec![
        ("All", EnvFilter::All),
        ("One(env_a)", EnvFilter::One(ids.env_a)),
        ("One(env_b)", EnvFilter::One(ids.env_b)),
        ("Subset(a,b)", EnvFilter::Subset(vec![ids.env_a, ids.env_b])),
        ("Unattributed", EnvFilter::Unattributed),
    ];

    // Before the backfill every call takes the live branch. Capture its answers.
    let mut live = Vec::new();
    for (name, env) in &scopes {
        let rows = sauron_db::repo::list_persons(
            &mut conn,
            ReadScope::new(ids.app_id, env.clone()),
            None,
            200,
            0,
            common::default_person_sort(),
            TimeWindow::since(
                "last_seen",
                chrono::Utc::now() - chrono::Duration::days(3650),
            ),
        )
        .await
        .unwrap_or_else(|e| panic!("live list_persons under {name}: {e}"));
        live.push(rows);
    }

    // Without this the whole comparison below could pass vacuously — two empty
    // result sets are equal. `One(env_a)` is the scope the reported 503 used.
    assert!(
        live[1].len() >= 5,
        "fixture precondition: One(env_a) must return people to compare, got {}",
        live[1].len()
    );
    assert!(
        !live[0].is_empty(),
        "fixture precondition: All must return people to compare"
    );

    // Anchored to the FIXTURE's clock — see
    // `backfill_adds_to_rows_the_write_path_already_created` for the full
    // reasoning. With `Utc::now()` here, a run before ~11:57 UTC backfilled
    // an EMPTY aggregate (every seeded row is at 12:00 UTC, i.e. after the
    // cutoff), the marker was still written, and every scope below compared a
    // populated live branch against an empty rollup branch: `left: 8, right:
    // 0`.
    sauron_db::person_env_backfill::backfill_app(
        &mut conn,
        ids.app_id,
        ids.pinned_now + chrono::Duration::hours(1),
    )
    .await
    .expect("backfill");

    // And the branch must actually have switched, or this compares live to live.
    assert!(
        sauron_db::person_env_backfill::is_backfilled(&mut conn, ids.app_id)
            .await
            .expect("marker"),
        "the marker must be set, otherwise every call below takes the live branch \
         again and the comparison proves nothing"
    );

    for (before, (name, env)) in live.iter().zip(scopes.iter()) {
        let after = sauron_db::repo::list_persons(
            &mut conn,
            ReadScope::new(ids.app_id, env.clone()),
            None,
            200,
            0,
            common::default_person_sort(),
            TimeWindow::since(
                "last_seen",
                chrono::Utc::now() - chrono::Duration::days(3650),
            ),
        )
        .await
        .unwrap_or_else(|e| panic!("rollup list_persons under {name}: {e}"));

        assert_eq!(
            before.len(),
            after.len(),
            "{name}: the rollup branch admitted a different number of people than the live \
             branch\nlive:   {:?}\nrollup: {:?}",
            before.iter().map(|r| &r.distinct_id).collect::<Vec<_>>(),
            after.iter().map(|r| &r.distinct_id).collect::<Vec<_>>(),
        );
        for (b, a) in before.iter().zip(after.iter()) {
            assert_eq!(b.distinct_id, a.distinct_id, "{name}: ordering diverged");
            let who = &b.distinct_id;
            assert_eq!(b.events_count, a.events_count, "{name}: {who} events");
            assert_eq!(b.errors_count, a.errors_count, "{name}: {who} errors");
            assert_eq!(b.sessions_count, a.sessions_count, "{name}: {who} sessions");
            assert_eq!(b.first_seen, a.first_seen, "{name}: {who} first_seen");
            assert_eq!(b.last_seen, a.last_seen, "{name}: {who} last_seen");
        }
    }

    drop(conn);
    db.cleanup().await;
}

/// Not an assertion — a printer. `cargo test -p sauron-db --test person_env_rollup
/// print_person_sql -- --nocapture --ignored` emits the exact strings
/// `list_persons` executes, so a measurement runs the real query rather than a
/// hand-transcribed lookalike (the trap the design doc records for the earlier
/// env-scoping work: EXPLAIN over a retyped query measures the wrong thing).
#[tokio::test]
#[ignore]
async fn print_person_sql() {
    let env = EnvFilter::One(uuid::Uuid::nil());
    println!("=====LIVE=====");
    println!(
        "{}",
        sauron_db::repo::list_persons_sql_for_test(env.clone())
    );
    println!("=====ROLLUP=====");
    println!("{}", sauron_db::repo::list_persons_rollup_sql_for_test(env));
}
