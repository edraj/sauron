//! Guest → identified merge. See
//! `docs/superpowers/specs/2026-08-12-guest-identity-merge-design.md`.

mod common;

use std::time::Duration;

use common::TestDb;
use diesel::prelude::*;
use diesel::sql_types::{Text, Uuid as SqlUuid};
use diesel_async::RunQueryDsl;
use sauron_db::identity_merge::{claim_identity, Claim};
use tokio::sync::Notify;
use uuid::Uuid;

#[derive(QueryableByName)]
struct Count {
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    n: i64,
}

/// Migration 58 must add the derived pre-login marker to both event tables.
/// It is nullable with no default so the ADD COLUMN is metadata-only.
#[tokio::test]
async fn migration_058_adds_guest_alias_to_both_event_tables() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;

    let row: Count = diesel::sql_query(
        "SELECT count(*)::bigint AS n FROM information_schema.columns \
          WHERE table_name IN ('analytics_events','error_events') \
            AND column_name = 'guest_alias' AND is_nullable = 'YES'",
    )
    .get_result(&mut conn)
    .await
    .expect("column probe");

    assert_eq!(
        row.n, 2,
        "guest_alias must exist and be nullable on both event tables"
    );

    drop(conn);
    db.cleanup().await;
}

/// The queue must reject a second row for the same alias, so a redelivered
/// identify() cannot schedule the same merge twice.
#[tokio::test]
async fn identity_merges_is_unique_per_alias() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let stmt = || {
        diesel::sql_query(
            "INSERT INTO identity_merges (app_id, alias_id, distinct_id) \
             VALUES ($1, 'anon_x', 'u-42')",
        )
        .bind::<SqlUuid, _>(ids.app_id)
    };

    stmt().execute(&mut conn).await.expect("first enqueue");
    assert!(
        stmt().execute(&mut conn).await.is_err(),
        "a second queue row for the same alias must be rejected"
    );

    drop(conn);
    db.cleanup().await;
}

/// `state` is TEXT + CHECK — house rule, never a custom SQL type.
#[tokio::test]
async fn identity_merges_rejects_an_unknown_state() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let bad = diesel::sql_query(
        "INSERT INTO identity_merges (app_id, alias_id, distinct_id, state) \
         VALUES ($1, 'anon_y', 'u-42', 'sideways')",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .execute(&mut conn)
    .await;

    assert!(
        bad.is_err(),
        "the CHECK constraint must reject an unknown state"
    );

    drop(conn);
    db.cleanup().await;
}

#[tokio::test]
async fn first_claim_is_fresh_and_a_repeat_is_not() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let first = claim_identity(&mut conn, ids.app_id, "anon_x", "u-42")
        .await
        .unwrap();
    assert!(
        matches!(first, Claim::Fresh),
        "first claim must be Fresh, got {first:?}"
    );

    let again = claim_identity(&mut conn, ids.app_id, "anon_x", "u-42")
        .await
        .unwrap();
    assert!(
        matches!(again, Claim::Repeat),
        "same user re-identifying is benign, got {again:?}"
    );

    drop(conn);
    db.cleanup().await;
}

/// The burn rule: an alias is claimed once and NEVER re-pointed. A second
/// identify() from a different user must be reported as a conflict — that is
/// the only signal anyone ever gets that an app forgot reset() on logout.
#[tokio::test]
async fn a_second_user_cannot_repoint_a_burned_alias() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    claim_identity(&mut conn, ids.app_id, "anon_shared", "ahmed")
        .await
        .unwrap();
    let sara = claim_identity(&mut conn, ids.app_id, "anon_shared", "sara")
        .await
        .unwrap();

    match sara {
        Claim::Conflict { existing } => assert_eq!(existing, "ahmed"),
        other => panic!("expected Conflict{{existing: ahmed}}, got {other:?}"),
    }

    let stored: Vec<String> = diesel::sql_query(
        "SELECT distinct_id FROM identities WHERE app_id = $1 AND alias_id = 'anon_shared'",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .load::<Target>(&mut conn)
    .await
    .unwrap()
    .into_iter()
    .map(|t| t.distinct_id)
    .collect();
    assert_eq!(
        stored,
        vec!["ahmed".to_string()],
        "the alias must not be re-pointed"
    );

    drop(conn);
    db.cleanup().await;
}

/// No chains. resolve() must be single-level and idempotent — that property is
/// what makes the cold overlay correct whether a Parquet file was written
/// before or after a merge.
#[tokio::test]
async fn a_target_cannot_become_an_alias_and_vice_versa() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    claim_identity(&mut conn, ids.app_id, "anon_x", "u-42")
        .await
        .unwrap();

    // u-42 is already a target, so it may not become an alias.
    let forward = claim_identity(&mut conn, ids.app_id, "u-42", "u-99")
        .await
        .unwrap();
    assert!(
        matches!(forward, Claim::Chain),
        "u-42 → u-99 must be refused, got {forward:?}"
    );

    // anon_x is already an alias, so it may not become a target.
    let backward = claim_identity(&mut conn, ids.app_id, "anon_z", "anon_x")
        .await
        .unwrap();
    assert!(
        matches!(backward, Claim::Chain),
        "… → anon_x must be refused, got {backward:?}"
    );

    drop(conn);
    db.cleanup().await;
}

/// A self-merge (`alias_id == distinct_id`) is a degenerate one-node chain.
/// Without the guard this returns `Fresh`, a merge of `x` into itself gets
/// enqueued, and `rewrite_hot_rows` would then stamp `guest_alias = 'x'`
/// across that person's ENTIRE history — not just their pre-login rows.
#[tokio::test]
async fn claiming_an_alias_equal_to_its_own_target_is_refused() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    let claim = claim_identity(&mut conn, ids.app_id, "self-merge-x", "self-merge-x")
        .await
        .unwrap();
    assert!(
        matches!(claim, Claim::Chain),
        "alias_id == distinct_id must be refused as a degenerate chain, got {claim:?}"
    );

    let rows: Count = diesel::sql_query(
        "SELECT count(*)::bigint AS n FROM identities \
          WHERE app_id = $1 AND alias_id = 'self-merge-x'",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .get_result(&mut conn)
    .await
    .unwrap();
    assert_eq!(rows.n, 0, "no row may be written for a refused self-merge");

    drop(conn);
    db.cleanup().await;
}

#[derive(QueryableByName)]
struct Target {
    #[diesel(sql_type = Text)]
    distinct_id: String,
}

// ===========================================================================
// Concurrency: the READ COMMITTED chain race (code review finding)
// ===========================================================================

/// Key for the `BEFORE INSERT` barrier trigger below. Distinct from
/// `http_artifacts.rs`'s `BARRIER_KEY` (74001, 1) — different table, and each
/// test runs against its own ephemeral database anyway, but a shared constant
/// across crates would invite exactly the kind of accidental collision this
/// key is supposed to be immune to.
const CHAIN_RACE_BARRIER_KEY: (i32, i32) = (91101, 1);

/// How many backends are *waiting* on the race barrier's advisory lock, in
/// this database. A non-zero answer is positive proof that txn A has passed
/// its `NOT EXISTS` guards and entered the INSERT — the only statement the
/// barrier trigger fires on — while still uncommitted. Same technique
/// `bins/sauron-api/tests/http_artifacts.rs`'s insert-race tests use.
async fn chain_barrier_waiters(conn: &mut sauron_db::AsyncPgConnection) -> i64 {
    #[derive(QueryableByName)]
    struct N {
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        n: i64,
    }
    let row: N = diesel::sql_query(
        "SELECT count(*)::bigint AS n FROM pg_locks \
          WHERE locktype = 'advisory' AND classid = $1 AND objid = $2 AND NOT granted \
            AND database = (SELECT oid FROM pg_database WHERE datname = current_database())",
    )
    .bind::<diesel::sql_types::Integer, _>(CHAIN_RACE_BARRIER_KEY.0)
    .bind::<diesel::sql_types::Integer, _>(CHAIN_RACE_BARRIER_KEY.1)
    .get_result(conn)
    .await
    .expect("count advisory-lock waiters");
    row.n
}

/// Whether backend `pid` is currently parked waiting to acquire an advisory
/// lock — regardless of which one. At the point this is called below, the
/// ONLY advisory lock txn B could possibly be contending for is
/// `claim_identity`'s per-app `pg_advisory_xact_lock`: B's own row
/// (`alias_id = "u-42"`) never matches the barrier trigger's `NEW.alias_id =
/// 'anon_x'` condition, so B never touches the barrier lock itself.
async fn is_blocked_on_an_advisory_lock(conn: &mut sauron_db::AsyncPgConnection, pid: i32) -> bool {
    #[derive(QueryableByName)]
    struct W {
        #[diesel(sql_type = diesel::sql_types::Nullable<Text>)]
        wait_event_type: Option<String>,
        #[diesel(sql_type = diesel::sql_types::Nullable<Text>)]
        wait_event: Option<String>,
    }
    let row: W = diesel::sql_query(
        "SELECT wait_event_type, wait_event FROM pg_stat_activity WHERE pid = $1",
    )
    .bind::<diesel::sql_types::Integer, _>(pid)
    .get_result(conn)
    .await
    .expect("query pg_stat_activity");
    row.wait_event_type.as_deref() == Some("Lock") && row.wait_event.as_deref() == Some("advisory")
}

/// Reproduces the review finding verbatim: txn A claims `anon_x → u-42` and,
/// before it commits, txn B claims `u-42 → u-99` — the exact interleaving a
/// plain `NOT EXISTS` guard cannot see under READ COMMITTED, since each guard
/// only sees *committed* rows. Without `claim_identity`'s per-app
/// `pg_advisory_xact_lock`, both guards would pass, both would commit, and
/// `identities` would hold a genuine chain (`anon_x → u-42 → u-99`).
///
/// Made deterministic with the same `BEFORE INSERT` advisory-lock barrier
/// `http_artifacts.rs`'s insert-race tests use, rather than looping and
/// hoping: a trigger parks txn A's own INSERT — after its guards have already
/// passed, before it commits — on a lock this test holds, so the window the
/// fix must close is a fact, not a hope.
///
/// `go_b` then guarantees the causal order the finding describes: txn B is
/// not even started until txn A is independently confirmed (via `pg_locks`)
/// to have reached that parked state, which can only happen after txn A has
/// already taken the per-app lock — otherwise a plain `tokio::join!` of both
/// calls could let txn B win the initial race for that lock, which would
/// still correctly refuse ONE of the two claims (no chain either way) but
/// would make it a coin flip which one, and would make the "txn A parked"
/// check below spuriously fail on the runs where txn B wins it.
///
/// Two independent checks confirm the fix's lock is what does the
/// serializing, rather than assuming it: `pg_locks` shows txn A parked on the
/// barrier, and `pg_stat_activity` then shows txn B itself blocked on an
/// advisory lock (the per-app one — the only kind it could be, per
/// `is_blocked_on_an_advisory_lock`'s doc comment). Convinced myself this is
/// not a phantom race by temporarily deleting the `pg_advisory_xact_lock`
/// call and the transaction wrapper from `claim_identity`: with that removed,
/// txn B never blocks (fails the second check) and — if that assertion is
/// also relaxed — the final chain check below finds exactly the chain this
/// test targets. Restored immediately after.
#[tokio::test]
async fn a_concurrent_claim_on_two_connections_cannot_form_a_chain() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;

    // Unpooled: the 2-slot test pool is needed for txn A's and txn B's own
    // connections below, and this one must stay open, doing its own queries,
    // for the whole test.
    let mut holder = db.extra_conn().await;
    diesel::sql_query(format!(
        "CREATE FUNCTION identity_merge_race_barrier() RETURNS trigger LANGUAGE plpgsql AS $fn$
         BEGIN
           IF NEW.alias_id = 'anon_x' THEN
             PERFORM pg_advisory_xact_lock({}, {});
           END IF;
           RETURN NEW;
         END $fn$",
        CHAIN_RACE_BARRIER_KEY.0, CHAIN_RACE_BARRIER_KEY.1
    ))
    .execute(&mut holder)
    .await
    .expect("create the barrier function");
    diesel::sql_query(
        "CREATE TRIGGER identity_merge_race_barrier_trg BEFORE INSERT ON identities \
         FOR EACH ROW EXECUTE FUNCTION identity_merge_race_barrier()",
    )
    .execute(&mut holder)
    .await
    .expect("create the barrier trigger");
    diesel::sql_query(format!(
        "SELECT pg_advisory_lock({}, {})",
        CHAIN_RACE_BARRIER_KEY.0, CHAIN_RACE_BARRIER_KEY.1
    ))
    .execute(&mut holder)
    .await
    .expect("take the barrier lock");

    let mut conn_a = db.conn().await;
    let mut conn_b = db.conn().await;

    #[derive(QueryableByName)]
    struct Pid {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        pid: i32,
    }
    let b_pid: i32 = diesel::sql_query("SELECT pg_backend_pid() AS pid")
        .get_result::<Pid>(&mut conn_b)
        .await
        .expect("txn B's backend pid")
        .pid;

    let go_b = Notify::new();

    let a_call = claim_identity(&mut conn_a, ids.app_id, "anon_x", "u-42");
    let b_call = async {
        go_b.notified().await;
        claim_identity(&mut conn_b, ids.app_id, "u-42", "u-99").await
    };
    let orchestrate = async {
        let mut parked = false;
        for _ in 0..150 {
            if chain_barrier_waiters(&mut holder).await > 0 {
                parked = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            parked,
            "txn A never reached its INSERT — the barrier trigger did not fire, so this test \
             would prove nothing about the race"
        );

        // Only now does txn B start — guaranteed, not merely likely, to be
        // after txn A already holds the per-app lock.
        go_b.notify_one();

        let mut b_blocked = false;
        for _ in 0..150 {
            if is_blocked_on_an_advisory_lock(&mut holder, b_pid).await {
                b_blocked = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            b_blocked,
            "txn B never blocked on an advisory lock — claim_identity's per-app lock is not \
             engaged, so this test would not exercise the race the fix closes"
        );

        diesel::sql_query(format!(
            "SELECT pg_advisory_unlock({}, {})",
            CHAIN_RACE_BARRIER_KEY.0, CHAIN_RACE_BARRIER_KEY.1
        ))
        .execute(&mut holder)
        .await
        .expect("release the barrier lock");
    };

    let (a_res, b_res, ()) = tokio::join!(a_call, b_call, orchestrate);
    let a_claim = a_res.expect("txn A's claim");
    let b_claim = b_res.expect("txn B's claim");

    // txn A is guaranteed (by `go_b`) to commit its `anon_x → u-42` row
    // before txn B's guards ever run, so txn B must see u-42 already taken
    // and refuse.
    assert!(
        matches!(a_claim, Claim::Fresh),
        "txn A must be Fresh, got {a_claim:?}"
    );
    assert!(
        matches!(b_claim, Claim::Chain),
        "txn B must be refused as a Chain, got {b_claim:?}"
    );

    // The ground-truth invariant, independent of either return value above:
    // no row may exist whose alias_id equals another row's distinct_id.
    #[derive(QueryableByName)]
    struct N {
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        n: i64,
    }
    let chained: N = diesel::sql_query(
        "SELECT count(*)::bigint AS n FROM identities i1 JOIN identities i2 \
           ON i1.alias_id = i2.distinct_id WHERE i1.app_id = $1 AND i2.app_id = $1",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .get_result(&mut holder)
    .await
    .expect("check for a chain");
    assert_eq!(
        chained.n, 0,
        "a chain formed between the two concurrent claims"
    );

    drop(conn_a);
    drop(conn_b);
    drop(holder);
    db.cleanup().await;
}

// ===========================================================================
// fold_rollups
// ===========================================================================

/// A counter fold is NOT idempotent, so it is written as a MOVE: the DELETE
/// consumes the source. Running it twice must not double the counters.
///
/// `errors_count`/`sessions_count` are seeded with values that DIFFER from
/// each other per row (not just from `events_count`), so a `DO UPDATE SET`
/// that swapped which column gets which formula — `errors_count` computed
/// from the two rows' `sessions_count`s and vice versa — would produce a
/// DIFFERENT, wrong total (11/6) instead of the correct one (6/11), rather
/// than accidentally landing on the right answer by symmetry.
///
/// Also asserts the `event_users` row for the alias is gone after the fold,
/// not just re-pointed: a copy-instead-of-move there would double nothing
/// (the merge is `LEAST`/`GREATEST`, already idempotent on its own), so it
/// would pass the counter assertions above while leaving a ghost guest
/// person permanently in the Users Explorer — the exact de-duplication this
/// feature exists to deliver.
#[tokio::test]
async fn folding_rollups_twice_does_not_double_count() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    for (did, events, errors, sessions) in
        [("anon_x", 7i64, 5i64, 2i64), ("u-42", 3i64, 1i64, 9i64)]
    {
        diesel::sql_query(
            "INSERT INTO event_user_environments \
               (app_id, distinct_id, environment_id, first_seen, last_seen, \
                events_count, errors_count, sessions_count) \
             VALUES ($1, $2, $3, now(), now(), $4, $5, $6)",
        )
        .bind::<SqlUuid, _>(ids.app_id)
        .bind::<Text, _>(did)
        .bind::<diesel::sql_types::Nullable<SqlUuid>, _>(Some(ids.env_a))
        .bind::<diesel::sql_types::BigInt, _>(events)
        .bind::<diesel::sql_types::BigInt, _>(errors)
        .bind::<diesel::sql_types::BigInt, _>(sessions)
        .execute(&mut conn)
        .await
        .unwrap();
    }
    diesel::sql_query(
        "INSERT INTO event_users (id, app_id, distinct_id, properties, first_seen, last_seen) \
         VALUES (gen_random_uuid(), $1, 'anon_x', '{}'::jsonb, now(), now()), \
                (gen_random_uuid(), $1, 'u-42',   '{}'::jsonb, now(), now())",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .execute(&mut conn)
    .await
    .unwrap();
    diesel::sql_query(
        "INSERT INTO identity_merges (app_id, alias_id, distinct_id) VALUES ($1, 'anon_x', 'u-42')",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .execute(&mut conn)
    .await
    .unwrap();

    for _ in 0..2 {
        sauron_db::identity_merge::fold_rollups(&mut conn, ids.app_id, "anon_x", "u-42", 7)
            .await
            .expect("fold");
    }

    #[derive(QueryableByName)]
    struct Counters {
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        events_count: i64,
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        errors_count: i64,
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        sessions_count: i64,
    }
    let total: Counters = diesel::sql_query(
        "SELECT events_count, errors_count, sessions_count FROM event_user_environments \
          WHERE app_id = $1 AND distinct_id = 'u-42'",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .get_result(&mut conn)
    .await
    .unwrap();
    assert_eq!(
        total.events_count, 10,
        "7 + 3 exactly once, no matter how many times the fold runs"
    );
    assert_eq!(
        total.errors_count, 6,
        "5 + 1 — a column swap in DO UPDATE SET would land this on 11 (the sessions total) instead"
    );
    assert_eq!(
        total.sessions_count, 11,
        "2 + 9 — a column swap in DO UPDATE SET would land this on 6 (the errors total) instead"
    );

    let rows: Count = diesel::sql_query(
        "SELECT count(*)::bigint AS n FROM event_user_environments \
          WHERE app_id = $1 AND distinct_id = 'anon_x'",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .get_result(&mut conn)
    .await
    .unwrap();
    assert_eq!(rows.n, 0, "the fold must consume the alias row");

    let ghost: Count = diesel::sql_query(
        "SELECT count(*)::bigint AS n FROM event_users \
          WHERE app_id = $1 AND distinct_id = 'anon_x'",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .get_result(&mut conn)
    .await
    .unwrap();
    assert_eq!(
        ghost.n, 0,
        "the event_users fold must also consume the alias — a copy would leave a ghost \
         guest person permanently in the Users Explorer"
    );

    drop(conn);
    db.cleanup().await;
}

/// environment_id is NULLABLE and Unattributed is a real, surfaced scope. The
/// ON CONFLICT must name the COALESCE expression from migration 0056 — naming
/// `(app_id, distinct_id, environment_id)` instead does not degrade
/// gracefully, it makes Postgres reject the statement outright with `42P10
/// there is no unique or exclusion constraint matching the ON CONFLICT
/// specification` (loud, not silent), which is still worth avoiding on its
/// own terms: it would take the whole fold down with it.
#[tokio::test]
async fn folding_an_unattributed_row_does_not_create_a_duplicate() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    for did in ["anon_x", "u-42"] {
        diesel::sql_query(
            "INSERT INTO event_user_environments \
               (app_id, distinct_id, environment_id, first_seen, last_seen, events_count) \
             VALUES ($1, $2, NULL, now(), now(), 4)",
        )
        .bind::<SqlUuid, _>(ids.app_id)
        .bind::<Text, _>(did)
        .execute(&mut conn)
        .await
        .unwrap();
    }
    diesel::sql_query(
        "INSERT INTO identity_merges (app_id, alias_id, distinct_id) VALUES ($1, 'anon_x', 'u-42')",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .execute(&mut conn)
    .await
    .unwrap();

    sauron_db::identity_merge::fold_rollups(&mut conn, ids.app_id, "anon_x", "u-42", 7)
        .await
        .expect("fold");

    let rows: Count = diesel::sql_query(
        "SELECT count(*)::bigint AS n FROM event_user_environments \
          WHERE app_id = $1 AND distinct_id = 'u-42' AND environment_id IS NULL",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .get_result(&mut conn)
    .await
    .unwrap();
    assert_eq!(rows.n, 1, "one unattributed row, not two");

    drop(conn);
    db.cleanup().await;
}

/// A guest active in several environments yields several `moved` rows. They
/// must not collide on one conflict target — Postgres rejects "ON CONFLICT DO
/// UPDATE command cannot affect row a second time" and the whole fold aborts.
#[tokio::test]
async fn folding_a_guest_active_in_several_environments_succeeds() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    for env in [Some(ids.env_a), Some(ids.env_b), None] {
        diesel::sql_query(
            "INSERT INTO event_user_environments \
               (app_id, distinct_id, environment_id, first_seen, last_seen, events_count) \
             VALUES ($1, 'anon_x', $2, now(), now(), 1)",
        )
        .bind::<SqlUuid, _>(ids.app_id)
        .bind::<diesel::sql_types::Nullable<SqlUuid>, _>(env)
        .execute(&mut conn)
        .await
        .unwrap();
    }
    diesel::sql_query(
        "INSERT INTO identity_merges (app_id, alias_id, distinct_id) VALUES ($1, 'anon_x', 'u-42')",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .execute(&mut conn)
    .await
    .unwrap();

    sauron_db::identity_merge::fold_rollups(&mut conn, ids.app_id, "anon_x", "u-42", 7)
        .await
        .expect("a multi-environment fold must not trip the ON CONFLICT row-twice rule");

    let rows: Count = diesel::sql_query(
        "SELECT count(*)::bigint AS n FROM event_user_environments \
          WHERE app_id = $1 AND distinct_id = 'u-42'",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .get_result(&mut conn)
    .await
    .unwrap();
    assert_eq!(rows.n, 3, "one row per environment, unattributed included");

    drop(conn);
    db.cleanup().await;
}

/// `properties` must merge ANON-FIRST: `EXCLUDED.properties || event_users.properties`,
/// so the target's (person's) own value wins on a conflicting key and jsonb's
/// `||` lets the right-hand side override. Every other test seeds `'{}'::jsonb`
/// on both sides, so swapping the operands — overwriting a person's real
/// `identify()` traits with stale guest properties — is otherwise
/// undetectable. Seeded with conflicting AND non-overlapping keys on both
/// sides so the assertion distinguishes "right operand order" from "any
/// merge at all".
#[tokio::test]
async fn folding_rollups_merges_properties_anon_first_but_person_wins_conflicts() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    diesel::sql_query(
        "INSERT INTO event_users (id, app_id, distinct_id, properties, first_seen, last_seen) \
         VALUES (gen_random_uuid(), $1, 'anon_props', \
                 '{\"plan\": \"free\", \"alias_only\": \"a\"}'::jsonb, now(), now()), \
                (gen_random_uuid(), $1, 'u-props', \
                 '{\"plan\": \"paid\", \"person_only\": \"p\"}'::jsonb, now(), now())",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .execute(&mut conn)
    .await
    .expect("seed event_users rows");
    diesel::sql_query(
        "INSERT INTO identity_merges (app_id, alias_id, distinct_id) \
         VALUES ($1, 'anon_props', 'u-props')",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .execute(&mut conn)
    .await
    .expect("enqueue merge");

    sauron_db::identity_merge::fold_rollups(&mut conn, ids.app_id, "anon_props", "u-props", 7)
        .await
        .expect("fold");

    #[derive(QueryableByName)]
    struct Props {
        #[diesel(sql_type = diesel::sql_types::Jsonb)]
        properties: serde_json::Value,
    }
    let row: Props = diesel::sql_query(
        "SELECT properties FROM event_users WHERE app_id = $1 AND distinct_id = 'u-props'",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .get_result(&mut conn)
    .await
    .expect("read back the merged properties");

    assert_eq!(
        row.properties["plan"], "paid",
        "on a conflicting key the PERSON's own identify() trait must win, not the guest's \
         (a swapped concat operand order would land this on \"free\" instead)"
    );
    assert_eq!(
        row.properties["alias_only"], "a",
        "a key only the alias had must survive the merge"
    );
    assert_eq!(
        row.properties["person_only"], "p",
        "a key only the person had must survive the merge"
    );

    drop(conn);
    db.cleanup().await;
}

/// Nothing else reads `first_seen`/`last_seen` back from either fold: the
/// counter test above seeds both rows with `now(), now()`, so a `LEAST`/
/// `GREATEST` swap in either the `event_user_environments` or the
/// `event_users` fold would go undetected. Seeded with two DISJOINT
/// timestamp ranges (alias strictly earlier than person) so the union is a
/// value neither row held on its own, and a swap lands on a visibly
/// different, wrong pair rather than a coincidentally-correct one.
#[tokio::test]
async fn folding_rollups_widens_span_to_the_union_not_the_intersection() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    // See the truncation note on timestamp round-trips further down this file
    // (`Utc::now()` carries nanoseconds; `timestamptz` stores microseconds).
    use chrono::{Duration, Timelike, Utc};
    let trunc = |t: chrono::DateTime<Utc>| t.with_nanosecond(0).expect("0ns is always valid");

    let alias_first = trunc(Utc::now() - Duration::hours(10));
    let alias_last = trunc(Utc::now() - Duration::hours(9));
    let person_first = trunc(Utc::now() - Duration::hours(5));
    let person_last = trunc(Utc::now() - Duration::hours(1));

    for (did, first, last) in [
        ("anon_span", alias_first, alias_last),
        ("u-span", person_first, person_last),
    ] {
        diesel::sql_query(
            "INSERT INTO event_user_environments \
               (app_id, distinct_id, environment_id, first_seen, last_seen, events_count) \
             VALUES ($1, $2, $3, $4, $5, 1)",
        )
        .bind::<SqlUuid, _>(ids.app_id)
        .bind::<Text, _>(did)
        .bind::<diesel::sql_types::Nullable<SqlUuid>, _>(Some(ids.env_a))
        .bind::<diesel::sql_types::Timestamptz, _>(first)
        .bind::<diesel::sql_types::Timestamptz, _>(last)
        .execute(&mut conn)
        .await
        .expect("seed event_user_environments row");

        diesel::sql_query(
            "INSERT INTO event_users (id, app_id, distinct_id, properties, first_seen, last_seen) \
             VALUES (gen_random_uuid(), $1, $2, '{}'::jsonb, $3, $4)",
        )
        .bind::<SqlUuid, _>(ids.app_id)
        .bind::<Text, _>(did)
        .bind::<diesel::sql_types::Timestamptz, _>(first)
        .bind::<diesel::sql_types::Timestamptz, _>(last)
        .execute(&mut conn)
        .await
        .expect("seed event_users row");
    }
    diesel::sql_query(
        "INSERT INTO identity_merges (app_id, alias_id, distinct_id) \
         VALUES ($1, 'anon_span', 'u-span')",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .execute(&mut conn)
    .await
    .expect("enqueue merge");

    sauron_db::identity_merge::fold_rollups(&mut conn, ids.app_id, "anon_span", "u-span", 7)
        .await
        .expect("fold");

    #[derive(QueryableByName)]
    struct Span {
        #[diesel(sql_type = diesel::sql_types::Timestamptz)]
        first_seen: chrono::DateTime<Utc>,
        #[diesel(sql_type = diesel::sql_types::Timestamptz)]
        last_seen: chrono::DateTime<Utc>,
    }

    let env_row: Span = diesel::sql_query(
        "SELECT first_seen, last_seen FROM event_user_environments \
          WHERE app_id = $1 AND distinct_id = 'u-span'",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .get_result(&mut conn)
    .await
    .expect("read back the merged event_user_environments row");
    assert_eq!(
        env_row.first_seen, alias_first,
        "event_user_environments.first_seen must widen to the alias's EARLIER value \
         (a LEAST/GREATEST swap would land this on the person's own, later first_seen)"
    );
    assert_eq!(
        env_row.last_seen, person_last,
        "event_user_environments.last_seen must widen to the person's LATER value \
         (a LEAST/GREATEST swap would land this on the alias's own, earlier last_seen)"
    );

    let user_row: Span = diesel::sql_query(
        "SELECT first_seen, last_seen FROM event_users \
          WHERE app_id = $1 AND distinct_id = 'u-span'",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .get_result(&mut conn)
    .await
    .expect("read back the merged event_users row");
    assert_eq!(
        user_row.first_seen, alias_first,
        "event_users.first_seen must widen to the alias's EARLIER value"
    );
    assert_eq!(
        user_row.last_seen, person_last,
        "event_users.last_seen must widen to the person's LATER value"
    );

    drop(conn);
    db.cleanup().await;
}

/// The span/`cold_stale` capture reads `alias_first_seen`/`alias_last_seen`
/// off the SAME `moved` CTE the `event_user_environments` fold already
/// deletes through — not from `analytics_events`, and (as of the fix-round
/// that corrected the initial submission) not from `event_users` either. This
/// proves that sourcing: no `analytics_events` row is seeded at all, and
/// `rewrite_hot_rows` (the only thing that would ever populate
/// `analytics_events.guest_alias`) is deliberately never called, yet the span
/// still comes out correct.
///
/// A person row is ALSO seeded, with a range that entirely WRAPS the alias's
/// own range on both ends. If a regression sourced the span from the fold's
/// post-merge, upserted row (the `ins` CTE's output — the union of alias and
/// person via `LEAST`/`GREATEST`) instead of from `moved` (the alias's own
/// pre-merge values), the captured span would silently equal the person's own
/// wrapping range instead of the alias's — a `count(DISTINCT ...)`-style test
/// that never seeds a person row cannot tell those two apart.
///
/// This case: the alias's own activity is comfortably inside the hot window,
/// so `cold_stale` must come out `false` — under-marking `cold_stale` is a
/// silently wrong number in production (a guest's cold-tier history would
/// never get an overlay row), so a dedicated assertion is worth it even
/// though it is the "boring" of the two outcomes.
#[tokio::test]
async fn folding_rollups_captures_the_alias_own_span_not_the_merged_span_hot_case() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    // `Utc::now()` carries nanoseconds; `timestamptz` stores microseconds, so a
    // raw `Utc::now()`-derived value would not necessarily survive the
    // round-trip unchanged, making an exact `==` assertion flaky rather than
    // wrong. Truncating to whole seconds keeps the values `now()`-relative
    // (so the hot/cold comparison still behaves) while making them exactly
    // representable — same fix `workflows.rs`'s `pinned_to_second` uses.
    use chrono::{Duration, Timelike, Utc};
    let trunc = |t: chrono::DateTime<Utc>| t.with_nanosecond(0).expect("0ns is always valid");

    let alias_first = trunc(Utc::now() - Duration::hours(3));
    let alias_last = trunc(Utc::now() - Duration::hours(1));
    // Wraps [alias_first, alias_last] entirely, so a union would differ from
    // the alias's own values on BOTH ends, not just one.
    let person_first = trunc(Utc::now() - Duration::hours(5));
    let person_last = trunc(Utc::now() - Duration::minutes(30));

    for (did, first, last) in [
        ("anon_hot", alias_first, alias_last),
        ("u-hot", person_first, person_last),
    ] {
        diesel::sql_query(
            "INSERT INTO event_user_environments \
               (app_id, distinct_id, environment_id, first_seen, last_seen, events_count) \
             VALUES ($1, $2, $3, $4, $5, 1)",
        )
        .bind::<SqlUuid, _>(ids.app_id)
        .bind::<Text, _>(did)
        .bind::<diesel::sql_types::Nullable<SqlUuid>, _>(Some(ids.env_a))
        .bind::<diesel::sql_types::Timestamptz, _>(first)
        .bind::<diesel::sql_types::Timestamptz, _>(last)
        .execute(&mut conn)
        .await
        .expect("seed event_user_environments row");
    }
    diesel::sql_query(
        "INSERT INTO identity_merges (app_id, alias_id, distinct_id) \
         VALUES ($1, 'anon_hot', 'u-hot')",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .execute(&mut conn)
    .await
    .expect("enqueue merge");

    // No rewrite_hot_rows call — proving span capture does not depend on it.
    sauron_db::identity_merge::fold_rollups(&mut conn, ids.app_id, "anon_hot", "u-hot", 7)
        .await
        .expect("fold");

    #[derive(QueryableByName, Debug)]
    struct Span {
        #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Timestamptz>)]
        alias_first_seen: Option<chrono::DateTime<Utc>>,
        #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Timestamptz>)]
        alias_last_seen: Option<chrono::DateTime<Utc>>,
        #[diesel(sql_type = diesel::sql_types::Bool)]
        cold_stale: bool,
    }
    let row: Span = diesel::sql_query(
        "SELECT alias_first_seen, alias_last_seen, cold_stale FROM identity_merges \
          WHERE app_id = $1 AND alias_id = 'anon_hot'",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .get_result(&mut conn)
    .await
    .expect("read back the queue row");

    assert_eq!(
        row.alias_first_seen.expect("alias_first_seen must be set"),
        alias_first,
        "alias_first_seen must match the ALIAS's own first_seen, not the person's \
         wrapping (and therefore union-dominant) first_seen"
    );
    assert_eq!(
        row.alias_last_seen.expect("alias_last_seen must be set"),
        alias_last,
        "alias_last_seen must match the ALIAS's own last_seen, not the person's \
         wrapping (and therefore union-dominant) last_seen"
    );
    assert!(
        !row.cold_stale,
        "activity entirely inside the hot window must not be marked cold_stale"
    );

    drop(conn);
    db.cleanup().await;
}

/// The other half of the case above: a guest whose activity predates the hot
/// window must come out `cold_stale = true`, or the cold overlay would never
/// learn it needs to cover this alias — a silently wrong number, not a crash.
/// Same `event_user_environments`-only sourcing, same wrapping person
/// distractor row, same absence of a `rewrite_hot_rows` call.
#[tokio::test]
async fn folding_rollups_marks_cold_stale_when_alias_activity_predates_hot_window() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    use chrono::{Duration, Timelike, Utc};
    let trunc = |t: chrono::DateTime<Utc>| t.with_nanosecond(0).expect("0ns is always valid");

    // hot_days = 7 means the hot/cold boundary sits at now() - 6 days. 30 days
    // ago is comfortably on the cold side of it.
    let alias_first = trunc(Utc::now() - Duration::days(30));
    let alias_last = trunc(Utc::now() - Duration::days(29));
    // Wraps [alias_first, alias_last] entirely on both ends.
    let person_first = trunc(Utc::now() - Duration::days(35));
    let person_last = trunc(Utc::now() - Duration::hours(1));

    for (did, first, last) in [
        ("anon_cold", alias_first, alias_last),
        ("u-cold", person_first, person_last),
    ] {
        diesel::sql_query(
            "INSERT INTO event_user_environments \
               (app_id, distinct_id, environment_id, first_seen, last_seen, events_count) \
             VALUES ($1, $2, $3, $4, $5, 1)",
        )
        .bind::<SqlUuid, _>(ids.app_id)
        .bind::<Text, _>(did)
        .bind::<diesel::sql_types::Nullable<SqlUuid>, _>(Some(ids.env_a))
        .bind::<diesel::sql_types::Timestamptz, _>(first)
        .bind::<diesel::sql_types::Timestamptz, _>(last)
        .execute(&mut conn)
        .await
        .expect("seed event_user_environments row");
    }
    diesel::sql_query(
        "INSERT INTO identity_merges (app_id, alias_id, distinct_id) \
         VALUES ($1, 'anon_cold', 'u-cold')",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .execute(&mut conn)
    .await
    .expect("enqueue merge");

    sauron_db::identity_merge::fold_rollups(&mut conn, ids.app_id, "anon_cold", "u-cold", 7)
        .await
        .expect("fold");

    #[derive(QueryableByName, Debug)]
    struct Span {
        #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Timestamptz>)]
        alias_first_seen: Option<chrono::DateTime<Utc>>,
        #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Timestamptz>)]
        alias_last_seen: Option<chrono::DateTime<Utc>>,
        #[diesel(sql_type = diesel::sql_types::Bool)]
        cold_stale: bool,
    }
    let row: Span = diesel::sql_query(
        "SELECT alias_first_seen, alias_last_seen, cold_stale FROM identity_merges \
          WHERE app_id = $1 AND alias_id = 'anon_cold'",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .get_result(&mut conn)
    .await
    .expect("read back the queue row");

    assert_eq!(
        row.alias_first_seen.expect("alias_first_seen must be set"),
        alias_first,
        "alias_first_seen must match the ALIAS's own first_seen, not the person's wrapping one"
    );
    assert_eq!(
        row.alias_last_seen.expect("alias_last_seen must be set"),
        alias_last,
        "alias_last_seen must match the ALIAS's own last_seen, not the person's wrapping one"
    );
    assert!(
        row.cold_stale,
        "activity that predates the hot window must be marked cold_stale"
    );

    drop(conn);
    db.cleanup().await;
}

/// The off-by-one this whole margin exists for. Against `hot_days = 7` the
/// shipped rule (`hot_days - 1` = 6 days) and a plain `hot_days` (7 days)
/// agree everywhere EXCEPT a 1-day-wide band, and neither of the two tests
/// above falls inside it (1–3 hours and 29–30 days both sit far outside).
/// `now() - 6.5 days` is inside that band: `6.5 days ago` is older than the
/// shipped rule's threshold (`now() - 6 days`, so `true`) but newer than the
/// unmargined threshold would be (`now() - 7 days`, so `false` without the
/// `- 1`). A regression that drops the margin passes every other test in this
/// file and only fails here.
///
/// Seeds `event_user_environments`, not `event_users`: the span is no longer
/// read from the latter at all, so seeding it here would leave `moved` empty
/// for this alias, `s.f IS NOT NULL` false, no `identity_merges` row touched,
/// and the assertion below would pass on `cold_stale`'s column DEFAULT `TRUE`
/// rather than on anything the margin arithmetic computed — discriminating
/// for nothing.
#[tokio::test]
async fn folding_rollups_cold_stale_respects_the_one_day_margin() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    use chrono::{Duration, Utc};

    let at = Utc::now() - Duration::hours((6.5 * 24.0) as i64);
    diesel::sql_query(
        "INSERT INTO event_user_environments \
           (app_id, distinct_id, environment_id, first_seen, last_seen, events_count) \
         VALUES ($1, 'anon_boundary', $2, $3, $3, 1)",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .bind::<diesel::sql_types::Nullable<SqlUuid>, _>(Some(ids.env_a))
    .bind::<diesel::sql_types::Timestamptz, _>(at)
    .execute(&mut conn)
    .await
    .expect("seed event_user_environments row");
    diesel::sql_query(
        "INSERT INTO identity_merges (app_id, alias_id, distinct_id) \
         VALUES ($1, 'anon_boundary', 'u-boundary')",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .execute(&mut conn)
    .await
    .expect("enqueue merge");

    sauron_db::identity_merge::fold_rollups(
        &mut conn,
        ids.app_id,
        "anon_boundary",
        "u-boundary",
        7,
    )
    .await
    .expect("fold");

    #[derive(QueryableByName)]
    struct ColdStale {
        #[diesel(sql_type = diesel::sql_types::Bool)]
        cold_stale: bool,
        #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Timestamptz>)]
        alias_first_seen: Option<chrono::DateTime<Utc>>,
    }
    let row: ColdStale = diesel::sql_query(
        "SELECT cold_stale, alias_first_seen FROM identity_merges \
          WHERE app_id = $1 AND alias_id = 'anon_boundary'",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .get_result(&mut conn)
    .await
    .expect("read back the queue row");

    // Asserted BEFORE `cold_stale`, because `cold_stale`'s column DEFAULT is
    // also `TRUE`: on its own, the assertion below cannot tell "the margin
    // rule computed TRUE" from "the span capture never fired at all and this
    // is the untouched default". `alias_first_seen` has no such ambiguity —
    // it is NULL until the capture runs — so it is what turns the next
    // assertion from a coincidence into evidence.
    assert!(
        row.alias_first_seen.is_some(),
        "the span capture must have run at all — without this, `cold_stale = true` \
         below is indistinguishable from the column's untouched TRUE default"
    );
    assert!(
        row.cold_stale,
        "6.5 days ago, hot_days=7: the shipped `hot_days - 1` rule (threshold = 6 days ago) \
         must call this cold_stale=true; only a rule missing the `- 1` margin \
         (threshold = 7 days ago) would call it false"
    );

    drop(conn);
    db.cleanup().await;
}

/// The bug this whole fix-round exists for, pinned directly: `event_users`
/// timestamps are INGEST time (every writer stamps them from `now()` at write
/// time), `event_user_environments` timestamps are EVENT time (bound to the
/// analytics event's own `occurred_at`). Both consumers of the captured span
/// — the cold overlay's window prune and `cold_stale` itself — compare it
/// against EVENT time, so sourcing from the wrong table shifts the span later
/// and can silently cross the hot/cold boundary.
///
/// This is the offline-flush shape a mobile SDK produces routinely: events
/// that *occurred* well outside the hot window, queued on-device, and only
/// *ingested* (hence only reflected in `event_users`) once the client comes
/// back online and calls `identify()`. Seeded so the two tables disagree
/// sharply — `event_user_environments` says 10 days ago (cold), `event_users`
/// says now (hot) — so this test fails against an `event_users`-sourced
/// implementation and passes only against the correct one.
#[tokio::test]
async fn folding_rollups_captures_event_time_not_ingest_time() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    use chrono::{Duration, Timelike, Utc};
    let trunc = |t: chrono::DateTime<Utc>| t.with_nanosecond(0).expect("0ns is always valid");

    // EVENT time: well past the hot_days=7 window (threshold = now() - 6 days).
    let event_first = trunc(Utc::now() - Duration::days(10));
    let event_last = trunc(Utc::now() - Duration::days(9));
    diesel::sql_query(
        "INSERT INTO event_user_environments \
           (app_id, distinct_id, environment_id, first_seen, last_seen, events_count) \
         VALUES ($1, 'anon_offline', $2, $3, $4, 1)",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .bind::<diesel::sql_types::Nullable<SqlUuid>, _>(Some(ids.env_a))
    .bind::<diesel::sql_types::Timestamptz, _>(event_first)
    .bind::<diesel::sql_types::Timestamptz, _>(event_last)
    .execute(&mut conn)
    .await
    .expect("seed event_user_environments row (event time, old)");

    // INGEST time: right now — the same alias's event_users row, as it would
    // be after the offline queue flushed and identify() fired today.
    diesel::sql_query(
        "INSERT INTO event_users (id, app_id, distinct_id, properties, first_seen, last_seen) \
         VALUES (gen_random_uuid(), $1, 'anon_offline', '{}'::jsonb, now(), now())",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .execute(&mut conn)
    .await
    .expect("seed event_users row (ingest time, fresh)");

    diesel::sql_query(
        "INSERT INTO identity_merges (app_id, alias_id, distinct_id) \
         VALUES ($1, 'anon_offline', 'u-offline')",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .execute(&mut conn)
    .await
    .expect("enqueue merge");

    sauron_db::identity_merge::fold_rollups(&mut conn, ids.app_id, "anon_offline", "u-offline", 7)
        .await
        .expect("fold");

    #[derive(QueryableByName, Debug)]
    struct Span {
        #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Timestamptz>)]
        alias_first_seen: Option<chrono::DateTime<Utc>>,
        #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Timestamptz>)]
        alias_last_seen: Option<chrono::DateTime<Utc>>,
        #[diesel(sql_type = diesel::sql_types::Bool)]
        cold_stale: bool,
    }
    let row: Span = diesel::sql_query(
        "SELECT alias_first_seen, alias_last_seen, cold_stale FROM identity_merges \
          WHERE app_id = $1 AND alias_id = 'anon_offline'",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .get_result(&mut conn)
    .await
    .expect("read back the queue row");

    assert_eq!(
        row.alias_first_seen.expect("alias_first_seen must be set"),
        event_first,
        "the captured span must be EVENT time (event_user_environments), not \
         INGEST time (event_users) — an ingest-time source would read `now()` here"
    );
    assert_eq!(
        row.alias_last_seen.expect("alias_last_seen must be set"),
        event_last,
        "the captured span must be EVENT time (event_user_environments), not \
         INGEST time (event_users) — an ingest-time source would read `now()` here"
    );
    assert!(
        row.cold_stale,
        "the alias's EVENT time predates the hot window, so this must be cold_stale=true \
         even though its INGEST time (event_users) is fresh — an ingest-time source would \
         compute now() < now() - 6 days = false and this alias would never be pruned \
         into the cold overlay"
    );

    drop(conn);
    db.cleanup().await;
}

// ===========================================================================
// rewrite_hot_rows
// ===========================================================================

/// The headline assertion: after a merge, a guest-then-identified timeline is
/// ONE person, not two.
#[tokio::test]
async fn rewriting_hot_rows_collapses_the_person_to_one() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    // Scoped to this function, not a module-level import: the file already
    // has `use std::time::Duration;` for the race test above, and a
    // top-level `use chrono::{Duration, Utc};` would collide with it.
    use chrono::{Duration, Utc};
    let t0 = Utc::now() - Duration::hours(2);

    // `analytics_events` has no `project_id` column — it was renamed to
    // `app_id` in migration 2026-07-12-000002 — so the seed inserts only the
    // columns that actually exist.
    for (did, at) in [
        ("anon_x", t0),
        ("anon_x", t0 + Duration::minutes(5)),
        ("u-42", Utc::now()),
    ] {
        diesel::sql_query(
            "INSERT INTO analytics_events (id, app_id, name, distinct_id, occurred_at) \
             VALUES (gen_random_uuid(), $1, 'page_view', $2, $3)",
        )
        .bind::<SqlUuid, _>(ids.app_id)
        .bind::<Text, _>(did)
        .bind::<diesel::sql_types::Timestamptz, _>(at)
        .execute(&mut conn)
        .await
        .expect("seed event");
    }

    // Scoped to the two ids under test, not the whole app: `seed_two_envs`
    // already seeds `analytics_events` with several other distinct_ids
    // (`shared_distinct_id`, `distinct_id_env_b_only`, `distinct_id_cross_env`,
    // a `none-an-0` unattributed row) for this same `app_id`, so an
    // app-wide `count(DISTINCT distinct_id)` would read 6, not 2, and the
    // "one human counted twice" precondition would never hold.
    let before: Count = diesel::sql_query(
        "SELECT count(DISTINCT distinct_id)::bigint AS n FROM analytics_events \
          WHERE app_id = $1 AND distinct_id IN ('anon_x', 'u-42')",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .get_result(&mut conn)
    .await
    .unwrap();
    assert_eq!(
        before.n, 2,
        "precondition: the bug — one human counted twice"
    );

    sauron_db::identity_merge::rewrite_hot_rows(&mut conn, ids.app_id, "anon_x", "u-42")
        .await
        .expect("rewrite");

    let after: Count = diesel::sql_query(
        "SELECT count(DISTINCT distinct_id)::bigint AS n FROM analytics_events \
          WHERE app_id = $1 AND distinct_id IN ('anon_x', 'u-42')",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .get_result(&mut conn)
    .await
    .unwrap();
    assert_eq!(
        after.n, 1,
        "after the merge the guest and the person are one"
    );

    let marked: Count = diesel::sql_query(
        "SELECT count(*)::bigint AS n FROM analytics_events \
          WHERE app_id = $1 AND guest_alias = 'anon_x'",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .get_result(&mut conn)
    .await
    .unwrap();
    assert_eq!(
        marked.n, 2,
        "exactly the pre-login events carry the guest marker"
    );

    drop(conn);
    db.cleanup().await;
}

/// Re-running a completed rewrite must be a no-op — recovery is "run the whole
/// job again", so every step before the folds has to be idempotent.
#[tokio::test]
async fn rewriting_hot_rows_twice_changes_nothing() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    // One row in EVERY table `rewrite_hot_rows` touches, not just
    // `analytics_events` — otherwise `first` only ever exercises one of the
    // six statements and a broken bind in any of the other five (e.g. a
    // swapped `$2`/`$3`) would still show `first == 1, second == 0` and look
    // idempotent while doing nothing on five-sixths of the real rewrite.
    //
    // See the column-list note in `rewriting_hot_rows_collapses_the_person_to_one`:
    // no `project_id` column on `analytics_events`.
    diesel::sql_query(
        "INSERT INTO analytics_events (id, app_id, name, distinct_id, occurred_at) \
         VALUES (gen_random_uuid(), $1, 'page_view', 'anon_x', now())",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .execute(&mut conn)
    .await
    .unwrap();

    diesel::sql_query(
        "INSERT INTO error_events (id, app_id, issue_id, fingerprint, distinct_id, occurred_at) \
         VALUES (gen_random_uuid(), $1, $2, 'harness-twice-error', 'anon_x', now())",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .bind::<SqlUuid, _>(ids.issue_id)
    .execute(&mut conn)
    .await
    .unwrap();

    diesel::sql_query(
        "INSERT INTO sessions (id, app_id, session_id, distinct_id, started_at, last_event_at) \
         VALUES (gen_random_uuid(), $1, 'harness-twice-session', 'anon_x', now(), now())",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .execute(&mut conn)
    .await
    .unwrap();

    diesel::sql_query(
        "INSERT INTO transactions (id, app_id, name, op, duration_ms, distinct_id, occurred_at) \
         VALUES (gen_random_uuid(), $1, 'checkout', 'http', 42.0, 'anon_x', now())",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .execute(&mut conn)
    .await
    .unwrap();

    diesel::sql_query(
        "INSERT INTO workflows \
           (id, app_id, environment_id, workflow_id, name, distinct_id, started_at, last_event_at) \
         VALUES (gen_random_uuid(), $1, $2, 'harness-twice-workflow', 'checkout-flow', 'anon_x', now(), now())",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .bind::<SqlUuid, _>(ids.env_a)
    .execute(&mut conn)
    .await
    .unwrap();

    diesel::sql_query(
        "INSERT INTO devices (id, app_id, device_key, last_distinct_id, first_seen, last_seen) \
         VALUES (gen_random_uuid(), $1, 'harness-twice-device', 'anon_x', now(), now())",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .execute(&mut conn)
    .await
    .unwrap();

    let first =
        sauron_db::identity_merge::rewrite_hot_rows(&mut conn, ids.app_id, "anon_x", "u-42")
            .await
            .unwrap();
    let second =
        sauron_db::identity_merge::rewrite_hot_rows(&mut conn, ids.app_id, "anon_x", "u-42")
            .await
            .unwrap();

    assert_eq!(
        first, 6,
        "the first pass rewrites the guest row in all six tables"
    );
    assert_eq!(second, 0, "the second pass must match nothing");

    drop(conn);
    db.cleanup().await;
}

/// Coverage for the five tables the two tests above never touch. Both of
/// them seed `analytics_events` only, so a statement for any of the other
/// five that is valid SQL but logically wrong — the concrete failure mode a
/// review flagged: `devices` written as `SET last_distinct_id = $2` (the
/// alias) instead of `$3` (the person), a silent no-op — would still return
/// `execute()` counts that make `first == 1` (or `== 6`, after the update
/// above) pass, because only `analytics_events` was ever asserted against
/// individually. Asserted per table below, not just against the aggregate
/// return count, for the same reason: an aggregate can be right while one
/// table underneath it is wrong.
///
/// `devices` is the highest-risk of the five: it is the one table whose
/// identity column has a different name (`last_distinct_id`, not
/// `distinct_id`), which is exactly where a copy-paste slip is most likely
/// and least visible.
#[tokio::test]
async fn rewriting_hot_rows_covers_every_non_analytics_table() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    // `error_events.issue_id` is NOT NULL with no default (migration
    // 2026-07-14-000011) — `ids.issue_id` is a real row `seed_two_envs`
    // already created, so this satisfies the FK instead of inventing one.
    diesel::sql_query(
        "INSERT INTO error_events (id, app_id, issue_id, fingerprint, distinct_id, occurred_at) \
         VALUES (gen_random_uuid(), $1, $2, 'harness-guest-merge-error', $3, now())",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .bind::<SqlUuid, _>(ids.issue_id)
    .bind::<Text, _>("anon_x")
    .execute(&mut conn)
    .await
    .expect("seed error_events row");

    diesel::sql_query(
        "INSERT INTO sessions (id, app_id, session_id, distinct_id, started_at, last_event_at) \
         VALUES (gen_random_uuid(), $1, 'harness-guest-merge-session', $2, now(), now())",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .bind::<Text, _>("anon_x")
    .execute(&mut conn)
    .await
    .expect("seed sessions row");

    // `transactions` has no natural-key text column to read the row back by
    // (unlike the others' fingerprint/session_id/workflow_id/device_key), so
    // a client-generated id stands in for one.
    let txn_id = Uuid::new_v4();
    diesel::sql_query(
        "INSERT INTO transactions (id, app_id, name, op, duration_ms, distinct_id, occurred_at) \
         VALUES ($1, $2, 'checkout', 'http', 42.0, $3, now())",
    )
    .bind::<SqlUuid, _>(txn_id)
    .bind::<SqlUuid, _>(ids.app_id)
    .bind::<Text, _>("anon_x")
    .execute(&mut conn)
    .await
    .expect("seed transactions row");

    // `workflows.environment_id` is NOT NULL and — despite what
    // migration 2026-07-29-000032's own CREATE TABLE text says
    // (`REFERENCES environments(id)`) — actually references
    // `app_environments` after migration 2026-08-12-000059 renamed the old
    // `environments` table under it and created a new catalogue table in its
    // place; a rename preserves the OID so the FK silently followed. `env_a`
    // is an `app_environments.id`, the right value here.
    diesel::sql_query(
        "INSERT INTO workflows \
           (id, app_id, environment_id, workflow_id, name, distinct_id, started_at, last_event_at) \
         VALUES (gen_random_uuid(), $1, $2, 'harness-guest-merge-workflow', 'checkout-flow', $3, now(), now())",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .bind::<SqlUuid, _>(ids.env_a)
    .bind::<Text, _>("anon_x")
    .execute(&mut conn)
    .await
    .expect("seed workflows row");

    diesel::sql_query(
        "INSERT INTO devices (id, app_id, device_key, last_distinct_id, first_seen, last_seen) \
         VALUES (gen_random_uuid(), $1, 'harness-guest-merge-device', $2, now(), now())",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .bind::<Text, _>("anon_x")
    .execute(&mut conn)
    .await
    .expect("seed devices row");

    let touched =
        sauron_db::identity_merge::rewrite_hot_rows(&mut conn, ids.app_id, "anon_x", "u-42")
            .await
            .expect("rewrite");
    assert_eq!(
        touched, 5,
        "exactly the five rows seeded above — no analytics_events row is seeded in this test"
    );

    // Every identity column read back below is `Nullable<Text>` in
    // `schema.rs` — `error_events`/`sessions`/`transactions`/`workflows`
    // `distinct_id` and `devices.last_distinct_id` all are. Declaring them as
    // plain `Text` happened to work only because the rows seeded above are
    // non-NULL by construction; a `Nullable` column deserialised into a
    // non-`Option` field panics at runtime the moment one of them is not, and
    // this file is where the next person copies a `QueryableByName` struct
    // from. The assertions below therefore compare `as_deref()` against
    // `Some(...)`, which additionally distinguishes "rewritten to u-42" from
    // "the column went NULL" — a plain `String` field could not.
    #[derive(QueryableByName)]
    struct DistinctAndGuest {
        #[diesel(sql_type = diesel::sql_types::Nullable<Text>)]
        distinct_id: Option<String>,
        #[diesel(sql_type = diesel::sql_types::Nullable<Text>)]
        guest_alias: Option<String>,
    }
    let error_row: DistinctAndGuest = diesel::sql_query(
        "SELECT distinct_id, guest_alias FROM error_events \
          WHERE app_id = $1 AND fingerprint = 'harness-guest-merge-error'",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .get_result(&mut conn)
    .await
    .expect("read back error_events row");
    assert_eq!(
        error_row.distinct_id.as_deref(),
        Some("u-42"),
        "error_events.distinct_id must be rewritten"
    );
    assert_eq!(
        error_row.guest_alias.as_deref(),
        Some("anon_x"),
        "error_events.guest_alias must carry the pre-login alias, like analytics_events"
    );

    #[derive(QueryableByName)]
    struct DistinctOnly {
        #[diesel(sql_type = diesel::sql_types::Nullable<Text>)]
        distinct_id: Option<String>,
    }
    let session_row: DistinctOnly = diesel::sql_query(
        "SELECT distinct_id FROM sessions \
          WHERE app_id = $1 AND session_id = 'harness-guest-merge-session'",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .get_result(&mut conn)
    .await
    .expect("read back sessions row");
    assert_eq!(
        session_row.distinct_id.as_deref(),
        Some("u-42"),
        "sessions.distinct_id must be rewritten"
    );

    let txn_row: DistinctOnly =
        diesel::sql_query("SELECT distinct_id FROM transactions WHERE id = $1")
            .bind::<SqlUuid, _>(txn_id)
            .get_result(&mut conn)
            .await
            .expect("read back transactions row");
    assert_eq!(
        txn_row.distinct_id.as_deref(),
        Some("u-42"),
        "transactions.distinct_id must be rewritten"
    );

    let workflow_row: DistinctOnly = diesel::sql_query(
        "SELECT distinct_id FROM workflows \
          WHERE app_id = $1 AND workflow_id = 'harness-guest-merge-workflow'",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .get_result(&mut conn)
    .await
    .expect("read back workflows row");
    assert_eq!(
        workflow_row.distinct_id.as_deref(),
        Some("u-42"),
        "workflows.distinct_id must be rewritten"
    );

    #[derive(QueryableByName)]
    struct LastDistinctOnly {
        #[diesel(sql_type = diesel::sql_types::Nullable<Text>)]
        last_distinct_id: Option<String>,
    }
    let device_row: LastDistinctOnly = diesel::sql_query(
        "SELECT last_distinct_id FROM devices \
          WHERE app_id = $1 AND device_key = 'harness-guest-merge-device'",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .get_result(&mut conn)
    .await
    .expect("read back devices row");
    assert_eq!(
        device_row.last_distinct_id.as_deref(),
        Some("u-42"),
        "devices.last_distinct_id must be rewritten — the one table whose identity \
         column is named differently, the likeliest spot for a copy-paste slip"
    );

    // `guest_alias` exists only on the two event tables (migration 058 added
    // it to just those two — see its own doc comment). Checked via
    // information_schema, not by selecting the column on these four tables,
    // so a wrong assertion here reads as a failed count rather than a
    // runtime "column does not exist" surprising whoever writes the next
    // query against one of them.
    let no_guest_alias: Count = diesel::sql_query(
        "SELECT count(*)::bigint AS n FROM information_schema.columns \
          WHERE table_name IN ('sessions', 'transactions', 'workflows', 'devices') \
            AND column_name = 'guest_alias'",
    )
    .get_result(&mut conn)
    .await
    .expect("column probe");
    assert_eq!(
        no_guest_alias.n, 0,
        "sessions/transactions/workflows/devices must have no guest_alias column"
    );

    drop(conn);
    db.cleanup().await;
}

// ===========================================================================
// The drain: claim_next / complete_merge / fail_merge
// ===========================================================================

/// End-to-end through the queue: a pending row is claimed, executed and marked
/// done, and a completed row is never claimed again.
#[tokio::test]
async fn the_drain_runs_a_pending_merge_exactly_once() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    // `analytics_events` has no `project_id` column — it was renamed to
    // `app_id` in migration 2026-07-12-000002 (see the column-list note on
    // `rewriting_hot_rows_collapses_the_person_to_one` above) — so the seed
    // inserts only the columns that actually exist.
    diesel::sql_query(
        "INSERT INTO analytics_events (id, app_id, name, distinct_id, occurred_at) \
         VALUES (gen_random_uuid(), $1, 'page_view', 'anon_x', now())",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .execute(&mut conn)
    .await
    .unwrap();
    diesel::sql_query(
        "INSERT INTO identity_merges (app_id, alias_id, distinct_id) VALUES ($1, 'anon_x', 'u-42')",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .execute(&mut conn)
    .await
    .unwrap();

    let job = sauron_db::identity_merge::claim_next(&mut conn)
        .await
        .unwrap()
        .expect("one pending merge");
    assert_eq!(job.alias_id, "anon_x");

    sauron_db::identity_merge::rewrite_hot_rows(
        &mut conn,
        job.app_id,
        &job.alias_id,
        &job.distinct_id,
    )
    .await
    .unwrap();
    sauron_db::identity_merge::fold_rollups(
        &mut conn,
        job.app_id,
        &job.alias_id,
        &job.distinct_id,
        7,
    )
    .await
    .unwrap();
    sauron_db::identity_merge::complete_merge(&mut conn, job.id, job.claimed_at)
        .await
        .unwrap();

    assert!(
        sauron_db::identity_merge::claim_next(&mut conn)
            .await
            .unwrap()
            .is_none(),
        "a completed merge must never be claimed again"
    );

    drop(conn);
    db.cleanup().await;
}

/// No infinite retry: a row that keeps failing lands in 'failed' and stops
/// being claimed, so one poisoned merge cannot spin the worker forever.
#[tokio::test]
async fn a_merge_stops_being_claimed_after_max_attempts() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    diesel::sql_query(
        "INSERT INTO identity_merges (app_id, alias_id, distinct_id) VALUES ($1, 'anon_x', 'u-42')",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .execute(&mut conn)
    .await
    .unwrap();

    let mut last_id = None;
    for _ in 0..sauron_db::identity_merge::MAX_ATTEMPTS {
        let job = sauron_db::identity_merge::claim_next(&mut conn)
            .await
            .unwrap()
            .expect("still runnable");
        sauron_db::identity_merge::fail_merge(&mut conn, job.id, job.claimed_at, "boom")
            .await
            .unwrap();
        // Bypass the backoff delay `fail_merge` just set: this test's job is
        // to exercise all MAX_ATTEMPTS claims in a tight loop, not to wait out
        // real wall-clock backoff (which would take minutes by the last
        // attempt). Backoff itself has its own dedicated test below.
        diesel::sql_query("UPDATE identity_merges SET next_attempt_at = now() WHERE id = $1")
            .bind::<SqlUuid, _>(job.id)
            .execute(&mut conn)
            .await
            .unwrap();
        last_id = Some(job.id);
    }

    assert!(
        sauron_db::identity_merge::claim_next(&mut conn)
            .await
            .unwrap()
            .is_none(),
        "after MAX_ATTEMPTS the row must be parked, not retried forever"
    );

    #[derive(QueryableByName)]
    struct State {
        #[diesel(sql_type = Text)]
        state: String,
    }
    let row: State = diesel::sql_query("SELECT state FROM identity_merges WHERE id = $1")
        .bind::<SqlUuid, _>(last_id.expect("loop ran at least once"))
        .get_result(&mut conn)
        .await
        .unwrap();
    assert_eq!(
        row.state, "dead",
        "an exhausted row must land in the terminal 'dead' state, not stay 'failed' forever \
         (a 'failed' row would remain in the runnable partial index, unclaimable but still \
         camping at the head of every scan)"
    );

    drop(conn);
    db.cleanup().await;
}

/// `FOR UPDATE SKIP LOCKED` exclusivity, on two REAL connections — both
/// existing `claim_next` tests run everything on a single connection, so a
/// naive rewrite (a plain `SELECT id ... LIMIT 1` followed by a separate
/// `UPDATE ... WHERE id = $1`, the classic double-claim bug) would pass them
/// unchanged. This is the two-connection harness
/// `a_concurrent_claim_on_two_connections_cannot_form_a_chain` above already
/// established for `claim_identity`'s advisory lock, aimed at `claim_next`
/// instead: exactly one queued row, two connections racing to claim it,
/// exactly one must win regardless of how the race resolves — `SKIP LOCKED`
/// makes the loser see zero candidate rows whether it arrives while the
/// winner's claim is still uncommitted (locked, so skipped) or after it has
/// already committed (state no longer matches the claim predicate), so this
/// assertion holds deterministically rather than only on a lucky interleaving.
#[tokio::test]
async fn concurrent_claim_next_calls_never_claim_the_same_row_twice() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    // Only 2 connections, matching the pool's 2-slot limit (see
    // `a_concurrent_claim_on_two_connections_cannot_form_a_chain` above for
    // the same constraint) — the seed insert reuses `conn_a` rather than
    // checking out a third.
    let mut conn_a = db.conn().await;
    let mut conn_b = db.conn().await;

    diesel::sql_query(
        "INSERT INTO identity_merges (app_id, alias_id, distinct_id) \
         VALUES ($1, 'anon_skiplocked', 'u-skiplocked')",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .execute(&mut conn_a)
    .await
    .unwrap();

    let (a, b) = tokio::join!(
        sauron_db::identity_merge::claim_next(&mut conn_a),
        sauron_db::identity_merge::claim_next(&mut conn_b),
    );
    let a = a.unwrap();
    let b = b.unwrap();

    let winners = [&a, &b].into_iter().filter(|r| r.is_some()).count();
    assert_eq!(
        winners, 1,
        "exactly one of two concurrent claim_next calls against a single queued row must win it, \
         got a={a:?} b={b:?}"
    );

    drop(conn_a);
    drop(conn_b);
    db.cleanup().await;
}

/// `complete_merge`'s widened fence: a worker that is genuinely still running
/// past its OWN lease (never re-claimed by anyone else) may correct a `dead`
/// row that `reap_exhausted` marked prematurely, because the reap never
/// touches `claimed_at` — the original worker's token still matches. Seeds
/// the row directly in the post-reap shape (`state = 'dead'`, `attempts =
/// MAX_ATTEMPTS`) rather than going through `reap_exhausted`, to isolate
/// exactly what `complete_merge` itself does with that shape.
#[tokio::test]
async fn complete_merge_can_correct_a_row_the_reap_marked_dead_under_the_same_claim() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    use chrono::{Duration as ChronoDuration, Timelike, Utc};
    // `timestamptz` stores microseconds; `Utc::now()` carries nanoseconds —
    // truncate so the round-trip `==` comparison below is exact, not flaky
    // (same fix used throughout this file, e.g. `folding_rollups_widens_span...`).
    let claimed_at = (Utc::now() - ChronoDuration::minutes(20))
        .with_nanosecond(0)
        .expect("0ns is always valid");

    diesel::sql_query(
        "INSERT INTO identity_merges \
           (app_id, alias_id, distinct_id, state, attempts, claimed_at) \
         VALUES ($1, 'reaped_alias', 'u-reaped', 'dead', $2, $3)",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .bind::<diesel::sql_types::Integer, _>(sauron_db::identity_merge::MAX_ATTEMPTS)
    .bind::<diesel::sql_types::Timestamptz, _>(claimed_at)
    .execute(&mut conn)
    .await
    .expect("seed a row in the post-reap shape");

    #[derive(QueryableByName)]
    struct IdOnly {
        #[diesel(sql_type = SqlUuid)]
        id: Uuid,
    }
    let row_id: Uuid = diesel::sql_query(
        "SELECT id FROM identity_merges WHERE app_id = $1 AND alias_id = 'reaped_alias'",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .get_result::<IdOnly>(&mut conn)
    .await
    .expect("read back the seeded row's id")
    .id;

    // The negative case FIRST, against the still-'dead' row: a caller with a
    // DIFFERENT claimed_at (a stale token from a claim that was itself later
    // re-claimed, not just reaped) must not be able to touch it.
    let wrong_token = claimed_at + ChronoDuration::seconds(1);
    let mismatched = sauron_db::identity_merge::complete_merge(&mut conn, row_id, wrong_token)
        .await
        .expect("complete_merge with the wrong token must not error, just match nothing");
    assert_eq!(
        mismatched, 0,
        "a DIFFERENT claimed_at against a 'dead' row must not update it — only the exact \
         original claim's token may correct a reaped row"
    );

    // Now the positive case: the ORIGINAL worker's own token, still matching
    // because the reap never touched claimed_at, must be able to correct the
    // record.
    let updated = sauron_db::identity_merge::complete_merge(&mut conn, row_id, claimed_at)
        .await
        .expect("complete_merge under the original claim");
    assert_eq!(
        updated, 1,
        "the original claim holder's own token must still be able to correct a row the reap \
         marked dead prematurely — the merge genuinely succeeded"
    );

    #[derive(QueryableByName)]
    struct StateOnly {
        #[diesel(sql_type = Text)]
        state: String,
    }
    let after: StateOnly = diesel::sql_query("SELECT state FROM identity_merges WHERE id = $1")
        .bind::<SqlUuid, _>(row_id)
        .get_result(&mut conn)
        .await
        .expect("read back the corrected row");
    assert_eq!(
        after.state, "done",
        "a genuinely successful late completion must overwrite a premature 'dead' reap, not be \
         silently discarded — the merge really happened"
    );

    drop(conn);
    db.cleanup().await;
}

/// **A Persons purge must not leave an alias resolving to the person it
/// erased.**
///
/// `purge::rollup_companions(PurgeKind::Persons)` lists `identities` and
/// deletes it keyed on the PERSON, so purging `P` removes every `identities`
/// row that burned an alias to `P`. `identity_merges` is deliberately left
/// alone by that same purge. The gap that opened between them was silent and
/// actively wrong, not conservative:
///
/// * `claim_identity` saw an empty `identities` and returned `Fresh` for
///   `A → D`;
/// * `enqueue_merge`'s `ON CONFLICT DO NOTHING` absorbed that against the
///   surviving `A → P` row, so no merge was ever scheduled for `A → D`;
/// * both consumers — `cold_alias_map` and `repo::repair_restored_rows` —
///   went on resolving `A` to the purged person `P`.
///
/// The `identities` row is deleted DIRECTLY here rather than by driving the
/// purge worker: the purge needs a job row, a worker lease and a whole
/// claim/fence cycle, none of which is what this asserts. The delete
/// reproduces the exact post-condition `purge.rs`'s companion CTE leaves
/// behind (`DELETE FROM identities WHERE app_id = $1 AND distinct_id IN
/// (...)`), which is the only part of the purge this hole depends on.
#[tokio::test]
async fn a_purged_person_leaves_no_alias_resolving_to_them() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let now = chrono::Utc::now();

    // Burn A → P and let its merge complete, exactly as the drain would.
    sauron_db::identity_merge::claim_and_schedule(&mut conn, ids.app_id, "anon_p", "u-purged")
        .await
        .expect("claim A -> P");
    diesel::sql_query(
        "UPDATE identity_merges SET state = 'done', cold_stale = true, \
             alias_first_seen = now() - interval '5 days', alias_last_seen = now() \
          WHERE app_id = $1 AND alias_id = 'anon_p'",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .execute(&mut conn)
    .await
    .expect("mark the first merge done");

    // The purge: exactly the companion delete `purge.rs` issues for Persons.
    diesel::sql_query("DELETE FROM identities WHERE app_id = $1 AND distinct_id = 'u-purged'")
        .bind::<SqlUuid, _>(ids.app_id)
        .execute(&mut conn)
        .await
        .expect("purge the person's identity rows");

    // The same device now signs up as somebody else.
    let claim =
        sauron_db::identity_merge::claim_and_schedule(&mut conn, ids.app_id, "anon_p", "u-new")
            .await
            .expect("claim A -> D");
    assert_eq!(
        claim,
        Claim::Fresh,
        "with the identities row purged, this is a genuinely fresh claim — that part was \
         always correct and is not what this test is about"
    );

    // The whole point: nothing may still resolve the alias to the PURGED
    // person. Checked through the consumer, not by reading the column, so a
    // future change that fixes the column but not the query still fails.
    let map = sauron_db::identity_merge::cold_alias_map(
        &mut conn,
        ids.app_id,
        now - chrono::Duration::days(30),
        now + chrono::Duration::days(1),
    )
    .await
    .expect("cold_alias_map");
    assert!(
        !map.iter().any(|e| e.person == "u-purged"),
        "no alias may resolve to a purged person — that is a wrong attribution, not a \
         conservative non-merge; map was {map:?}"
    );
    assert!(
        map.iter()
            .any(|e| e.alias == "anon_p" && e.person == "u-new"),
        "the alias must have been repointed to the person the surviving claim names; \
         map was {map:?}"
    );

    // And the queue row itself must be re-armed, or the hot rewrite for the
    // new person never runs either.
    #[derive(QueryableByName)]
    struct StateRow {
        #[diesel(sql_type = Text)]
        state: String,
        #[diesel(sql_type = Text)]
        distinct_id: String,
    }
    let row: StateRow = diesel::sql_query(
        "SELECT state, distinct_id FROM identity_merges WHERE app_id = $1 AND alias_id = 'anon_p'",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .get_result(&mut conn)
    .await
    .expect("read the queue row");
    assert_eq!(row.distinct_id, "u-new", "the queue row must be repointed");
    assert_eq!(
        row.state, "pending",
        "the repointed merge must be re-armed, or the new person's hot rows are never \
         rewritten either"
    );

    drop(conn);
    db.cleanup().await;
}

/// The other half of the same hole: after a purge, a claim that WOULD form a
/// chain in `identity_merges` must still be refused, even though `identities`
/// no longer has the evidence.
///
/// `A → B` is burned and merged, `B` is purged, and a later `identify(B, C)`
/// used to pass both `identities` guards cleanly — leaving `identity_merges`
/// holding both `A → B` and `B → C`. `UNIQUE (app_id, alias_id)` cannot stop
/// that: the two rows have different `alias_id`s.
#[tokio::test]
async fn a_chain_is_refused_from_identity_merges_after_the_identities_row_is_purged() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    sauron_db::identity_merge::claim_and_schedule(&mut conn, ids.app_id, "chain_a", "chain_b")
        .await
        .expect("claim A -> B");
    diesel::sql_query("DELETE FROM identities WHERE app_id = $1")
        .bind::<SqlUuid, _>(ids.app_id)
        .execute(&mut conn)
        .await
        .expect("purge every identities row");

    let claim =
        sauron_db::identity_merge::claim_and_schedule(&mut conn, ids.app_id, "chain_b", "chain_c")
            .await
            .expect("claim B -> C");
    assert_eq!(
        claim,
        Claim::Chain,
        "the guard must consult identity_merges too — that is the table both consumers \
         actually read, and it is the one the purge does not touch"
    );

    let n: Count =
        diesel::sql_query("SELECT count(*)::bigint AS n FROM identity_merges WHERE app_id = $1")
            .bind::<SqlUuid, _>(ids.app_id)
            .get_result(&mut conn)
            .await
            .expect("count merges");
    assert_eq!(n.n, 1, "no second merge row may have been created");

    drop(conn);
    db.cleanup().await;
}

/// A repeat `identify()` — every page load after login — must not write to
/// `identities` at all.
///
/// The old claim statement's `DO UPDATE SET distinct_id = identities.
/// distinct_id` is a no-op in VALUE but a real row version: one dead tuple
/// per page load on a table migration 0060 does not tune, plus a per-app
/// advisory lock (measured 2.267 ms per locked claim, a ~440 claims/s ceiling
/// per app) held on the same connection the batched ingest path uses for the
/// rest of its batch.
///
/// `xmin` is the assertion because it is exactly what an UPDATE changes and a
/// pure read does not — a row-count or a value comparison would pass either
/// way.
#[tokio::test]
async fn a_repeat_identify_does_not_rewrite_the_identities_row() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    sauron_db::identity_merge::claim_and_schedule(&mut conn, ids.app_id, "anon_fast", "u-fast")
        .await
        .expect("first claim");

    async fn read_xmin(conn: &mut sauron_db::PgConn, app_id: Uuid) -> String {
        #[derive(QueryableByName)]
        struct Xmin {
            #[diesel(sql_type = Text)]
            xmin: String,
        }
        let row: Xmin = diesel::sql_query(
            "SELECT xmin::text AS xmin FROM identities \
              WHERE app_id = $1 AND alias_id = 'anon_fast'",
        )
        .bind::<SqlUuid, _>(app_id)
        .get_result(conn)
        .await
        .expect("read xmin");
        row.xmin
    }

    let before = read_xmin(&mut conn, ids.app_id).await;
    for _ in 0..5 {
        let claim = sauron_db::identity_merge::claim_and_schedule(
            &mut conn,
            ids.app_id,
            "anon_fast",
            "u-fast",
        )
        .await
        .expect("repeat claim");
        assert_eq!(claim, Claim::Repeat);
    }
    let after = read_xmin(&mut conn, ids.app_id).await;

    assert_eq!(
        before,
        after,
        "five repeat identifies produced {} row versions on identities; the fast path must \
         not write at all",
        if before == after { 0 } else { 5 }
    );

    // …and the re-arm still happened, which is the interaction that is easy
    // to ship broken: a fast path that returns Repeat without re-arming keeps
    // every existing test green while silently restoring the one-shot-merge
    // bug.
    #[derive(QueryableByName)]
    struct StateOnly {
        #[diesel(sql_type = Text)]
        state: String,
    }
    diesel::sql_query(
        "UPDATE identity_merges SET state = 'done' WHERE app_id = $1 AND alias_id = 'anon_fast'",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .execute(&mut conn)
    .await
    .expect("mark done");
    sauron_db::identity_merge::claim_and_schedule(&mut conn, ids.app_id, "anon_fast", "u-fast")
        .await
        .expect("repeat after done");
    let row: StateOnly = diesel::sql_query(
        "SELECT state FROM identity_merges WHERE app_id = $1 AND alias_id = 'anon_fast'",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .get_result(&mut conn)
    .await
    .expect("read state");
    assert_eq!(
        row.state, "pending",
        "the unlocked fast path must still perform the Repeat re-arm — skipping it is \
         invisible to every other assertion in this suite"
    );

    drop(conn);
    db.cleanup().await;
}

/// Migration 0058's three added indexes: `rewrite_hot_rows`' six statements
/// must ALL be index-backed.
///
/// The design doc claimed they already were ("Steps 1–6 ride the existing
/// `(app_id, distinct_id, occurred_at)` indexes"); that index exists for
/// `analytics_events`, `error_events` and `sessions` only. Measured before
/// this migration: `transactions` was a sequential scan of every partition
/// (4,286 buffers), `devices` a sequential scan (1,682), `workflows` an index
/// scan on the app prefix with `distinct_id` as a heap-side filter. Once per
/// signup, scaling with total retained volume rather than with the guest's
/// own row count.
///
/// Plain `EXPLAIN`, not `EXPLAIN ANALYZE`: `ANALYZE` would execute the
/// `UPDATE`, and the second statement would then plan against rows the first
/// had already rewritten.
#[tokio::test]
async fn every_hot_rewrite_statement_is_index_backed() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    // Enough rows per table that a sequential scan is a live planner option,
    // and a realistic NULL share so the partial indexes' `IS NOT NULL`
    // predicate is doing something.
    diesel::sql_query(
        "INSERT INTO transactions (id, app_id, name, op, distinct_id, occurred_at, duration_ms) \
         SELECT gen_random_uuid(), $1, 'tx', 'http.server', \
                CASE WHEN g % 4 = 0 THEN NULL ELSE 'u-' || (g % 900) END, \
                now() - make_interval(hours => (g % 20)), 1.0 \
           FROM generate_series(1, 20000) g",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .execute(&mut conn)
    .await
    .expect("seed transactions");
    diesel::sql_query(
        "INSERT INTO workflows (app_id, environment_id, workflow_id, name, status, \
                                distinct_id, started_at, last_event_at) \
         SELECT $1, $2, 'wf-' || g, 'checkout', 'active', \
                CASE WHEN g % 4 = 0 THEN NULL ELSE 'u-' || (g % 900) END, \
                now() - make_interval(hours => (g % 20)), now() \
           FROM generate_series(1, 20000) g",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .bind::<SqlUuid, _>(ids.env_a)
    .execute(&mut conn)
    .await
    .expect("seed workflows");
    diesel::sql_query(
        "INSERT INTO devices (app_id, device_key, last_distinct_id, first_seen, last_seen) \
         SELECT $1, 'dev-' || g, \
                CASE WHEN g % 4 = 0 THEN NULL ELSE 'u-' || (g % 900) END, \
                now(), now() \
           FROM generate_series(1, 20000) g",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .execute(&mut conn)
    .await
    .expect("seed devices");
    diesel::sql_query("VACUUM ANALYZE transactions, workflows, devices")
        .execute(&mut conn)
        .await
        .expect("analyze");

    #[derive(QueryableByName)]
    struct Plan {
        #[diesel(sql_type = Text, column_name = "QUERY PLAN")]
        line: String,
    }
    for (table, col, index) in [
        (
            "transactions",
            "distinct_id",
            "transactions_app_distinct_idx",
        ),
        ("workflows", "distinct_id", "workflows_app_distinct_idx"),
        (
            "devices",
            "last_distinct_id",
            "devices_app_last_distinct_idx",
        ),
    ] {
        let plan: Vec<Plan> = diesel::sql_query(format!(
            "EXPLAIN UPDATE {table} SET {col} = 'u-merged' \
              WHERE app_id = '{app}' AND {col} = 'u-7'",
            app = ids.app_id,
        ))
        .load(&mut conn)
        .await
        .unwrap_or_else(|e| panic!("explain {table}: {e}"));
        let text = plan
            .iter()
            .map(|p| p.line.as_str())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            !text.contains("Seq Scan"),
            "{table}: rewrite_hot_rows must not sequentially scan — the cost would scale \
             with total retained volume rather than the guest's row count, once per \
             signup; plan was:\n{text}"
        );
        // The plan on a partitioned parent names the CHILD index, whose name
        // Postgres derives from its own columns, so match the column
        // signature rather than the parent index's name where they differ.
        assert!(
            text.contains(index) || text.contains(&format!("app_id_{col}")),
            "{table}: expected migration 0058's {index} (or its per-partition namesake); \
             plan was:\n{text}"
        );
    }

    drop(conn);
    db.cleanup().await;
}

/// **F1: a re-fold must not prune a NULL-span alias out of the cold overlay.**
///
/// A merge can reach `done` having moved NOTHING — an anon id older than
/// migration 0056, rollups already purged, or every pre-login rollup write
/// landing after the fold. `fold_rollups`'s `s.f IS NOT NULL` guard skips the
/// whole UPDATE in that case, so the span stays NULL and `cold_stale` stays at
/// its conservative `TRUE` default. `cold_alias_map`'s arm 3 then keeps that
/// alias in the overlay, which is correct and which the design doc marks
/// "cannot prove this is safe to drop… do not simplify".
///
/// `rearm_merge` made that row re-foldable for the first time, and a
/// discriminator of `alias_first_seen IS NOT NULL` gets it wrong: NULL span
/// reads as "never computed", so a straggler's RECENT timestamp recomputes
/// `cold_stale` to `false`, arms 3 and 4 both stop matching, and the alias
/// disappears from the overlay entirely. That is C1's own failure mode —
/// permanent, silent cold-tier double-count — reintroduced by C1's own fix.
///
/// `completed_at` is the discriminator that survives, because the question is
/// "has a fold ever completed for this alias", not "did a fold ever find
/// anything to move".
#[tokio::test]
async fn a_refold_does_not_prune_a_null_span_alias_from_the_cold_overlay() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let now = chrono::Utc::now();

    // A merge that completed having moved nothing: NULL span, cold_stale at
    // its TRUE default, completed_at set by complete_merge.
    diesel::sql_query(
        "INSERT INTO identity_merges \
           (app_id, alias_id, distinct_id, state, completed_at) \
         VALUES ($1, 'anon_nullspan', 'u-nullspan', 'done', now())",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .execute(&mut conn)
    .await
    .expect("seed a done merge with an empty fold");

    let before = sauron_db::identity_merge::cold_alias_map(
        &mut conn,
        ids.app_id,
        now - chrono::Duration::days(30),
        now + chrono::Duration::days(1),
    )
    .await
    .expect("cold_alias_map before");
    assert!(
        before.iter().any(|e| e.alias == "anon_nullspan"),
        "precondition: arm 3 must carry a done/NULL-span alias, or this test proves \
         nothing about losing it; map was {before:?}"
    );

    // The straggler: a RECENT rollup row, the shape a late pre-login event
    // produces via repo::bump_person_env. This is what the re-fold moves.
    diesel::sql_query(
        "INSERT INTO event_user_environments \
           (app_id, distinct_id, environment_id, first_seen, last_seen, events_count) \
         VALUES ($1, 'anon_nullspan', $2, now() - interval '1 hour', now(), 1)",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .bind::<diesel::sql_types::Nullable<SqlUuid>, _>(Some(ids.env_a))
    .execute(&mut conn)
    .await
    .expect("seed the straggler rollup row");

    // The re-fold, exactly as the re-armed drain runs it.
    sauron_db::identity_merge::fold_rollups(
        &mut conn,
        ids.app_id,
        "anon_nullspan",
        "u-nullspan",
        30,
    )
    .await
    .expect("re-fold");

    let after = sauron_db::identity_merge::cold_alias_map(
        &mut conn,
        ids.app_id,
        now - chrono::Duration::days(30),
        now + chrono::Duration::days(1),
    )
    .await
    .expect("cold_alias_map after");
    assert!(
        after.iter().any(|e| e.alias == "anon_nullspan"),
        "a re-fold must not drop an alias out of the cold overlay. cold_stale was TRUE \
         because nothing had ever computed it; recomputing it from a straggler's recent \
         timestamp turns it FALSE and this guest double-counts in Parquet forever, \
         silently. Map after the re-fold was {after:?}"
    );

    drop(conn);
    db.cleanup().await;
}

/// The other two cases the `completed_at` discriminator must not break: a
/// FIRST fold still computes `cold_stale` honestly in both directions.
///
/// Guards against "fix F1 by never recomputing", which would pin every alias
/// to `cold_stale = true` and silently delete the prune the design doc counts
/// on to remove the large majority of overlay rows.
#[tokio::test]
async fn a_first_fold_still_computes_cold_stale_in_both_directions() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    // `completed_at` is NULL on both: these are first folds of never-completed
    // merges, which is what makes them recomputable.
    for (alias, person, age_days) in [("anon_hotg", "u-hotg", 1i64), ("anon_coldg", "u-coldg", 20)]
    {
        diesel::sql_query(
            "INSERT INTO event_user_environments \
               (app_id, distinct_id, environment_id, first_seen, last_seen, events_count) \
             VALUES ($1, $2, $3, now() - make_interval(days => $4::int), \
                     now() - make_interval(days => $4::int), 1)",
        )
        .bind::<SqlUuid, _>(ids.app_id)
        .bind::<Text, _>(alias)
        .bind::<diesel::sql_types::Nullable<SqlUuid>, _>(Some(ids.env_a))
        .bind::<diesel::sql_types::Integer, _>(age_days as i32)
        .execute(&mut conn)
        .await
        .expect("seed rollup");
        diesel::sql_query(
            "INSERT INTO identity_merges (app_id, alias_id, distinct_id) VALUES ($1, $2, $3)",
        )
        .bind::<SqlUuid, _>(ids.app_id)
        .bind::<Text, _>(alias)
        .bind::<Text, _>(person)
        .execute(&mut conn)
        .await
        .expect("enqueue");
        sauron_db::identity_merge::fold_rollups(&mut conn, ids.app_id, alias, person, 14)
            .await
            .expect("fold");
    }

    async fn read(conn: &mut sauron_db::PgConn, alias: &str, app: Uuid) -> bool {
        #[derive(QueryableByName)]
        struct Stale {
            #[diesel(sql_type = diesel::sql_types::Bool)]
            cold_stale: bool,
        }
        let r: Stale = diesel::sql_query(
            "SELECT cold_stale FROM identity_merges WHERE app_id = $1 AND alias_id = $2",
        )
        .bind::<SqlUuid, _>(app)
        .bind::<Text, _>(alias)
        .get_result(conn)
        .await
        .expect("read cold_stale");
        r.cold_stale
    }

    assert!(
        !read(&mut conn, "anon_hotg", ids.app_id).await,
        "a guest active 1 day ago with hot_days=14 was entirely inside the hot window, so \
         the rewrite fixed its rows before export: cold_stale must be FALSE. A fix for F1 \
         that simply stops recomputing would pin this to TRUE and delete the prune."
    );
    assert!(
        read(&mut conn, "anon_coldg", ids.app_id).await,
        "a guest active 20 days ago with hot_days=14 predates the window: cold_stale must \
         be TRUE, or its Parquet rows never get a cold-overlay row"
    );

    drop(conn);
    db.cleanup().await;
}

/// **F2: both chain guards' `distinct_id` leg must be index-backed.**
///
/// `UNIQUE (app_id, alias_id)` answers "what is this alias bound to". Both
/// chain guards ask the mirror question — "is this id already somebody's
/// TARGET" — and nothing indexed it until migration 0058's
/// `identity_merges_app_distinct_idx`.
///
/// This is not a background cost. `claim_identity_locked`'s fourth
/// `NOT EXISTS` leg runs INSIDE the per-app advisory lock on every fresh
/// claim, so a sequential scan there consumes the very serialisation budget
/// the unlocked probe was added to protect: measured at 200k rows, 8.95 ms
/// unindexed against 0.034 ms indexed, which would have taken the ~440
/// claims/s per-app ceiling down to roughly 90-150 and kept degrading, since
/// this table gains a row per signup and has no purge path. `chain_conflict`
/// pays the same leg once per drain job, and re-armed merges recur per active
/// alias forever.
#[tokio::test]
async fn both_chain_guard_legs_are_index_backed() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    diesel::sql_query(
        "INSERT INTO identity_merges (app_id, alias_id, distinct_id, state) \
         SELECT $1, 'cg_' || g, 'u-cg-' || g, 'done' FROM generate_series(1, 40000) g",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .execute(&mut conn)
    .await
    .expect("bulk seed");
    diesel::sql_query("VACUUM ANALYZE identity_merges")
        .execute(&mut conn)
        .await
        .expect("analyze");

    #[derive(QueryableByName)]
    struct Plan {
        #[diesel(sql_type = Text, column_name = "QUERY PLAN")]
        line: String,
    }
    // The two legs, written as the guards write them: `alias_id` (served by
    // the unique key) and `distinct_id` (served only by migration 0058's
    // index). Asserted separately so a failure names which one regressed.
    for (col, index) in [
        ("alias_id", "identity_merges_app_id_alias_id_key"),
        ("distinct_id", "identity_merges_app_distinct_idx"),
    ] {
        let plan: Vec<Plan> = diesel::sql_query(format!(
            "EXPLAIN SELECT 1 FROM identity_merges WHERE app_id = '{app}' AND {col} = 'probe'",
            app = ids.app_id,
        ))
        .load(&mut conn)
        .await
        .unwrap_or_else(|e| panic!("explain {col}: {e}"));
        let text = plan
            .iter()
            .map(|p| p.line.as_str())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            !text.contains("Seq Scan"),
            "the {col} chain-guard leg must not sequentially scan — the claim-side copy of \
             it runs inside the per-app advisory lock on every fresh claim; plan was:\n{text}"
        );
        assert!(
            text.contains(index),
            "the {col} leg must ride {index}; plan was:\n{text}"
        );
    }

    drop(conn);
    db.cleanup().await;
}

// ===========================================================================
// The four gaps the final review parked. See
// `.superpowers/sdd/2026-08-12-guest-identity-merge/final-fixes-report.md`.
// ===========================================================================

/// **F1 through the REPOINT path**, which is the half nothing covered.
///
/// `a_refold_does_not_prune_a_null_span_alias_from_the_cold_overlay` drives F1
/// through `rearm_merge`, which never touched `completed_at` in the first
/// place. The other way a `done` row is put back on the queue is
/// `enqueue_merge`'s REPOINT arm, and an earlier version of that arm cleared
/// `completed_at` — which reopens F1 exactly, because `completed_at` is now a
/// correctness input to `fold_rollups`'s `cold_stale` discriminator, not just
/// an operator-facing timestamp.
///
/// Driven end to end rather than seeded, because the point is that the
/// production sequence reaches this state: a burned alias, a Persons purge
/// that deletes its `identities` row but deliberately leaves the queue row,
/// a claim to a DIFFERENT person, and the re-fold that claim schedules.
#[tokio::test]
async fn a_repoint_does_not_prune_a_null_span_alias_from_the_cold_overlay() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;
    let now = chrono::Utc::now();

    // 1. An ordinary first identify.
    let first =
        sauron_db::identity_merge::claim_and_schedule(&mut conn, ids.app_id, "anon_rp", "p-old")
            .await
            .expect("first claim");
    assert!(matches!(first, Claim::Fresh), "got {first:?}");

    // 2. Drain it. The alias has NO `event_user_environments` row, so the fold
    //    moves nothing: the span stays NULL and `cold_stale` stays at its
    //    conservative TRUE default, with `completed_at` the only evidence a
    //    fold ever ran. That is precisely arm 3's shape.
    let job = sauron_db::identity_merge::claim_next(&mut conn)
        .await
        .expect("claim_next")
        .expect("the merge just enqueued must be claimable");
    assert_eq!(job.alias_id, "anon_rp");
    sauron_db::identity_merge::fold_rollups(&mut conn, ids.app_id, "anon_rp", "p-old", 30)
        .await
        .expect("first fold");
    sauron_db::identity_merge::complete_merge(&mut conn, job.id, job.claimed_at)
        .await
        .expect("complete");

    #[derive(QueryableByName)]
    struct MergeRow {
        #[diesel(sql_type = Text)]
        distinct_id: String,
        #[diesel(sql_type = Text)]
        state: String,
        #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Timestamptz>)]
        completed_at: Option<chrono::DateTime<chrono::Utc>>,
        #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Timestamptz>)]
        alias_first_seen: Option<chrono::DateTime<chrono::Utc>>,
        #[diesel(sql_type = diesel::sql_types::Bool)]
        cold_stale: bool,
    }
    async fn read_merge(conn: &mut sauron_db::PgConn, app: Uuid, alias: &str) -> MergeRow {
        diesel::sql_query(
            "SELECT distinct_id, state, completed_at, alias_first_seen, cold_stale \
               FROM identity_merges WHERE app_id = $1 AND alias_id = $2",
        )
        .bind::<SqlUuid, _>(app)
        .bind::<Text, _>(alias)
        .get_result(conn)
        .await
        .expect("read identity_merges row")
    }

    let after_first = read_merge(&mut conn, ids.app_id, "anon_rp").await;
    assert_eq!(after_first.state, "done");
    assert!(
        after_first.alias_first_seen.is_none() && after_first.cold_stale,
        "precondition: the first fold must leave a NULL span at the TRUE default, or this \
         test is not exercising the shape F1 is about"
    );
    assert!(
        after_first.completed_at.is_some(),
        "precondition: complete_merge must stamp completed_at"
    );

    // 3. The Persons purge: `p-old` is erased, taking the `identities` row
    //    with it. `identity_merges` is deliberately NOT a companion of that
    //    purge, so the queue row survives naming a person who no longer
    //    exists. (`purge::rollup_companions(PurgeKind::Persons)` — spelled as
    //    the DELETE it performs, so this test does not depend on that
    //    module's API shape.)
    diesel::sql_query("DELETE FROM identities WHERE app_id = $1 AND alias_id = $2")
        .bind::<SqlUuid, _>(ids.app_id)
        .bind::<Text, _>("anon_rp")
        .execute(&mut conn)
        .await
        .expect("purge the identities row");

    // 4. The same device identifies as somebody else. The probe misses (no
    //    `identities` row any more), the locked path claims Fresh, and
    //    `enqueue_merge` takes its REPOINT arm on the surviving queue row.
    let second =
        sauron_db::identity_merge::claim_and_schedule(&mut conn, ids.app_id, "anon_rp", "p-new")
            .await
            .expect("second claim");
    assert!(
        matches!(second, Claim::Fresh),
        "the purge un-burned the alias, so this must be a fresh claim; got {second:?}"
    );

    let after_repoint = read_merge(&mut conn, ids.app_id, "anon_rp").await;
    assert_eq!(
        after_repoint.distinct_id, "p-new",
        "precondition: the repoint arm must have moved the target, or nothing below is \
         testing the repoint path"
    );
    assert_eq!(after_repoint.state, "pending");
    assert!(
        after_repoint.completed_at.is_some(),
        "THE REGRESSION: the repoint must NOT clear completed_at. It is the only surviving \
         evidence that a fold ever ran for this alias, and fold_rollups reads it to tell a \
         computed cold_stale from the untouched TRUE default. Clearing it makes the re-fold \
         below recompute FALSE off a straggler and drops this guest out of the cold overlay \
         permanently — F1, reintroduced through the one path no test covered."
    );

    // 5. A straggler: one late pre-login event lands and bumps the rollup.
    diesel::sql_query(
        "INSERT INTO event_user_environments \
           (app_id, distinct_id, environment_id, first_seen, last_seen, events_count) \
         VALUES ($1, 'anon_rp', $2, now() - interval '1 hour', now(), 1)",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .bind::<diesel::sql_types::Nullable<SqlUuid>, _>(Some(ids.env_a))
    .execute(&mut conn)
    .await
    .expect("seed the straggler rollup row");

    // 6. The re-fold the repoint scheduled, then the merge completes again.
    let job2 = sauron_db::identity_merge::claim_next(&mut conn)
        .await
        .expect("claim_next")
        .expect("the repointed merge must be claimable");
    assert_eq!(job2.distinct_id, "p-new");
    sauron_db::identity_merge::fold_rollups(&mut conn, ids.app_id, "anon_rp", "p-new", 30)
        .await
        .expect("re-fold");
    sauron_db::identity_merge::complete_merge(&mut conn, job2.id, job2.claimed_at)
        .await
        .expect("complete the repointed merge");

    let map = sauron_db::identity_merge::cold_alias_map(
        &mut conn,
        ids.app_id,
        now - chrono::Duration::days(30),
        now + chrono::Duration::days(1),
    )
    .await
    .expect("cold_alias_map");
    assert!(
        map.iter()
            .any(|e| e.alias == "anon_rp" && e.person == "p-new"),
        "a repointed alias must survive its re-fold in the cold overlay. Map was {map:?}"
    );

    drop(conn);
    db.cleanup().await;
}

/// **F7: `rearm_merge` may not repoint an alias into a chain.**
///
/// `enqueue_merge`'s repoint is chain-safe for free — it runs immediately
/// after `claim_identity_locked`'s four guards, under the per-app advisory
/// lock. `rearm_merge` has no such backing: its dominant caller is
/// `claim_and_schedule`'s FAST path, reached off an unlocked probe that
/// evaluates no guard at all.
///
/// The chained state is seeded directly rather than driven, on purpose. With
/// the claim-time guards now covering `identity_merges` too, the claim path
/// cannot produce it — which is the point: the writers that CAN are a
/// hand-written backfill, an admin `UPDATE`, or a row created before those
/// guards covered this table, and none of them are reachable through an API
/// this test could call.
///
/// Both halves matter. The repoint must still land when it is safe (or the
/// guard has silently deleted the one window `enqueue_merge` declines to
/// touch), and the re-arm itself must still happen when the repoint is
/// refused (or the guard has traded C1's straggler sweep away to protect a
/// bonus self-heal).
#[tokio::test]
async fn rearm_merge_refuses_to_repoint_an_alias_into_a_chain() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    // A second app under the same project, so the guard's `app_id`
    // correlation is exercised rather than assumed.
    let other_app = sauron_db::repo::create_app(
        &mut conn,
        ids.project_id,
        "second app",
        &format!("second-app-{}", Uuid::new_v4().simple()),
        "web",
    )
    .await
    .expect("create a second app");

    for (app, alias, person, state) in [
        // The control: nothing claims `p-ctl-new` as an alias.
        (ids.app_id, "anon_ctl", "p-ctl-old", "done"),
        // The chained case: `p-ch-new` IS somebody's alias in this app.
        (ids.app_id, "anon_ch", "p-ch-old", "done"),
        (ids.app_id, "p-ch-new", "p-ch-final", "pending"),
        // The cross-app case: `p-x-new` is an alias in the OTHER app only.
        (ids.app_id, "anon_x", "p-x-old", "done"),
        (other_app.id, "p-x-new", "p-x-final", "pending"),
    ] {
        diesel::sql_query(
            "INSERT INTO identity_merges (app_id, alias_id, distinct_id, state, completed_at) \
             VALUES ($1, $2, $3, $4, CASE WHEN $4 = 'done' THEN now() ELSE NULL END)",
        )
        .bind::<SqlUuid, _>(app)
        .bind::<Text, _>(alias)
        .bind::<Text, _>(person)
        .bind::<Text, _>(state)
        .execute(&mut conn)
        .await
        .expect("seed merge row");
    }

    #[derive(QueryableByName)]
    struct TargetAndState {
        #[diesel(sql_type = Text)]
        distinct_id: String,
        #[diesel(sql_type = Text)]
        state: String,
    }
    async fn read(conn: &mut sauron_db::PgConn, app: Uuid, alias: &str) -> TargetAndState {
        diesel::sql_query(
            "SELECT distinct_id, state FROM identity_merges WHERE app_id = $1 AND alias_id = $2",
        )
        .bind::<SqlUuid, _>(app)
        .bind::<Text, _>(alias)
        .get_result(conn)
        .await
        .expect("read identity_merges row")
    }

    for (alias, new_target) in [
        ("anon_ctl", "p-ctl-new"),
        ("anon_ch", "p-ch-new"),
        ("anon_x", "p-x-new"),
    ] {
        sauron_db::identity_merge::rearm_merge(&mut conn, ids.app_id, alias, new_target)
            .await
            .unwrap_or_else(|e| panic!("rearm {alias}: {e}"));
    }

    let ctl = read(&mut conn, ids.app_id, "anon_ctl").await;
    assert_eq!(
        ctl.distinct_id, "p-ctl-new",
        "a repoint that cannot form a chain must still land — that is the follow-up path \
         for the one window enqueue_merge's repoint declines to touch (a row that was \
         'running' during a purge). A guard that refuses unconditionally deletes it."
    );
    assert_eq!(ctl.state, "pending", "the re-arm itself must always happen");

    let chained = read(&mut conn, ids.app_id, "anon_ch").await;
    assert_eq!(
        chained.distinct_id, "p-ch-old",
        "THE REGRESSION: `p-ch-new` is already somebody's alias, so repointing `anon_ch` at \
         it writes `anon_ch -> p-ch-new` beside `p-ch-new -> p-ch-final` — the single-level \
         invariant resolve() and the whole cold overlay depend on. rearm_merge runs off an \
         UNLOCKED probe, so unlike enqueue_merge's repoint it has no claim-time guard \
         behind it and must carry its own."
    );
    assert_eq!(
        chained.state, "pending",
        "the re-arm must still happen even when the repoint is declined — refusing the \
         whole UPDATE would trade C1's straggler sweep away to protect a bonus self-heal"
    );

    let cross = read(&mut conn, ids.app_id, "anon_x").await;
    assert_eq!(
        cross.distinct_id, "p-x-new",
        "the guard is per-app: an alias row in ANOTHER app must not block this app's \
         repoint. Dropping `c.app_id = $1` makes every busy deployment's repoints stop \
         silently."
    );

    drop(conn);
    db.cleanup().await;
}

/// **T3: the enqueue is gated on the claim OUTCOME, and each outcome's effect
/// on the queue differs.**
///
/// The original gap was that `ON CONFLICT DO NOTHING` absorbed a duplicate
/// enqueue, so an implementation that enqueued on every claim outcome passed
/// a row-count assertion. C1 changed the shape — `Repeat` now RE-ARMS — so
/// this asserts the outcome per claim type instead of the row count:
///
/// | claim | required effect on `identity_merges` |
/// |---|---|
/// | `Fresh` | the row is created, `pending` |
/// | `Repeat` | a `done` row goes back to `pending`, deferred by the re-arm grace |
/// | `Conflict` | NOTHING changes — not the target, not the state |
/// | `Chain` | no row exists at all |
///
/// Two mutations this kills that the old tests did not: deleting the
/// `if fast == Claim::Repeat` gate on `claim_and_schedule`'s fast path (the
/// `Conflict` leg then repoints and re-arms), and deleting the `Claim::Fresh`
/// match arm in `claim_and_schedule_locked` (the `Chain` leg then inserts a
/// queue row for a claim that was refused).
#[tokio::test]
async fn the_enqueue_is_gated_on_the_claim_outcome() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    #[derive(QueryableByName)]
    struct QueueRow {
        #[diesel(sql_type = Text)]
        distinct_id: String,
        #[diesel(sql_type = Text)]
        state: String,
        #[diesel(sql_type = diesel::sql_types::Bool)]
        deferred: bool,
    }
    async fn read(conn: &mut sauron_db::PgConn, app: Uuid, alias: &str) -> Option<QueueRow> {
        let rows: Vec<QueueRow> = diesel::sql_query(
            "SELECT distinct_id, state, \
                    (next_attempt_at > now() + interval '4 minutes') AS deferred \
               FROM identity_merges WHERE app_id = $1 AND alias_id = $2",
        )
        .bind::<SqlUuid, _>(app)
        .bind::<Text, _>(alias)
        .load(conn)
        .await
        .expect("read identity_merges row");
        rows.into_iter().next()
    }

    // --- Fresh: the row is created ---------------------------------------
    let fresh =
        sauron_db::identity_merge::claim_and_schedule(&mut conn, ids.app_id, "anon_g", "u-one")
            .await
            .expect("fresh claim");
    assert!(matches!(fresh, Claim::Fresh), "got {fresh:?}");
    let row = read(&mut conn, ids.app_id, "anon_g")
        .await
        .expect("Fresh must enqueue a merge");
    assert_eq!(
        (row.distinct_id.as_str(), row.state.as_str()),
        ("u-one", "pending")
    );
    assert!(
        !row.deferred,
        "a first enqueue is due immediately — only the RE-ARM defers, and that difference \
         is what makes the Repeat leg below meaningful"
    );

    // Drain it so the next two legs run against a `done` row, which is the
    // only state `rearm_merge` matches and the state a stale repoint would
    // find.
    let job = sauron_db::identity_merge::claim_next(&mut conn)
        .await
        .expect("claim_next")
        .expect("claimable");
    sauron_db::identity_merge::complete_merge(&mut conn, job.id, job.claimed_at)
        .await
        .expect("complete");
    assert_eq!(
        read(&mut conn, ids.app_id, "anon_g").await.unwrap().state,
        "done"
    );

    // --- Repeat: re-arms, deferred by REARM_GRACE_SECS --------------------
    let repeat =
        sauron_db::identity_merge::claim_and_schedule(&mut conn, ids.app_id, "anon_g", "u-one")
            .await
            .expect("repeat claim");
    assert!(matches!(repeat, Claim::Repeat), "got {repeat:?}");
    let row = read(&mut conn, ids.app_id, "anon_g").await.unwrap();
    assert_eq!(
        row.state, "pending",
        "C1: a Repeat re-arms a completed merge so late-landing rows are swept. \
         'Repeat does nothing' is the bug, not the design."
    );
    assert!(
        row.deferred,
        "the re-arm defers by REARM_GRACE_SECS, which is what distinguishes it from an \
         enqueue/repoint (both of which set next_attempt_at = now())"
    );

    // Back to `done` by hand: `claim_next` will not touch the row until the
    // re-arm grace elapses, and this test is about the enqueue gate, not the
    // drain's timing.
    diesel::sql_query(
        "UPDATE identity_merges SET state = 'done', next_attempt_at = now() \
          WHERE app_id = $1 AND alias_id = 'anon_g'",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .execute(&mut conn)
    .await
    .expect("park the row back at done");

    // --- Conflict: nothing changes ---------------------------------------
    let conflict =
        sauron_db::identity_merge::claim_and_schedule(&mut conn, ids.app_id, "anon_g", "u-two")
            .await
            .expect("conflicting claim");
    match &conflict {
        Claim::Conflict { existing } => assert_eq!(existing, "u-one"),
        other => panic!("expected Conflict{{existing: u-one}}, got {other:?}"),
    }
    let row = read(&mut conn, ids.app_id, "anon_g").await.unwrap();
    assert_eq!(
        row.distinct_id, "u-one",
        "a Conflict is a REFUSED claim — the second person never got the alias, so nothing \
         may schedule a merge into them. An ungated enqueue repoints the queue row at \
         u-two and the drain then folds this guest's history into the wrong person."
    );
    assert_eq!(
        row.state, "done",
        "and an ungated RE-ARM on the fast path would put the completed merge back on the \
         queue for a claim that was refused"
    );

    // --- Chain: no row at all --------------------------------------------
    // `u-one` is already a target, so it may not become an alias. This one
    // goes through the LOCKED path (the probe finds no `identities` row keyed
    // on `u-one`), which is where `claim_and_schedule_locked`'s match lives.
    let chain =
        sauron_db::identity_merge::claim_and_schedule(&mut conn, ids.app_id, "u-one", "u-three")
            .await
            .expect("chaining claim");
    assert!(matches!(chain, Claim::Chain), "got {chain:?}");
    assert!(
        read(&mut conn, ids.app_id, "u-one").await.is_none(),
        "a refused chain must leave NO queue row. An ungated enqueue in \
         claim_and_schedule_locked writes one, and the drain then rewrites this person's \
         rows onto u-three — the exact edge the claim guard refused."
    );

    drop(conn);
    db.cleanup().await;
}

/// **T3: the claim and the enqueue are ATOMIC** — the property
/// `claim_and_schedule` exists for, and the one nothing regression-tested.
///
/// If they were two transactions, a process death between them would leave
/// the alias burned (the unique index makes a claim permanent) with no merge
/// ever queued. Because the burn rule means the alias can never be claimed
/// again, that guest's history would NEVER merge — silent, permanent loss of
/// exactly the thing this feature exists to preserve.
///
/// The injection is a `BEFORE INSERT` trigger on `identity_merges`, not a
/// dropped/renamed table: two of `claim_identity_locked`'s four guards SELECT
/// from `identity_merges`, so hiding the table would fail the CLAIM instead
/// of the enqueue and prove nothing. The trigger fires only on INSERT, i.e.
/// strictly after the claim.
///
/// And the trigger reports the `identities` row count it sees, so the
/// assertion does not merely observe "an error happened somewhere". Seeing
/// `1` proves the burn was really written inside the transaction, which makes
/// its absence afterwards a genuine ROLLBACK rather than a claim that never
/// ran.
#[tokio::test]
async fn a_failed_enqueue_rolls_the_burn_back() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut conn = db.conn().await;

    // No `$$` quoting anywhere: diesel sends this through the extended
    // protocol, where a `$n` inside a dollar-quoted body is asking for
    // trouble. Single-quoted body with doubled quotes is equivalent and
    // parameter-free.
    diesel::sql_query(
        "CREATE FUNCTION harness_block_enqueue() RETURNS trigger LANGUAGE plpgsql AS \
         'BEGIN RAISE EXCEPTION ''harness injected enqueue failure (identities rows for \
          this alias at this instant: %)'', \
          (SELECT count(*) FROM identities i \
            WHERE i.app_id = NEW.app_id AND i.alias_id = NEW.alias_id); END'",
    )
    .execute(&mut conn)
    .await
    .expect("create the injection function");
    diesel::sql_query(
        "CREATE TRIGGER harness_block_enqueue BEFORE INSERT ON identity_merges \
         FOR EACH ROW EXECUTE FUNCTION harness_block_enqueue()",
    )
    .execute(&mut conn)
    .await
    .expect("install the injection trigger");

    let err = sauron_db::identity_merge::claim_and_schedule(
        &mut conn,
        ids.app_id,
        "anon_atomic",
        "u-atomic",
    )
    .await
    .expect_err("the injected enqueue failure must surface as an error");
    let msg = err.to_string();
    assert!(
        msg.contains("harness injected enqueue failure"),
        "the failure must come from the enqueue INSERT, not from an earlier statement — \
         otherwise this test proves nothing about ordering. Error was: {msg}"
    );
    assert!(
        msg.contains("alias at this instant: 1"),
        "the trigger must observe the burn ALREADY WRITTEN when the enqueue runs. Seeing 0 \
         here would mean the claim never happened and the rollback assertion below is \
         vacuous. Error was: {msg}"
    );

    diesel::sql_query("DROP TRIGGER harness_block_enqueue ON identity_merges")
        .execute(&mut conn)
        .await
        .expect("remove the injection trigger");
    diesel::sql_query("DROP FUNCTION harness_block_enqueue()")
        .execute(&mut conn)
        .await
        .expect("remove the injection function");

    let burned: Count = diesel::sql_query(
        "SELECT count(*)::bigint AS n FROM identities WHERE app_id = $1 AND alias_id = $2",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .bind::<Text, _>("anon_atomic")
    .get_result(&mut conn)
    .await
    .expect("count identities");
    assert_eq!(
        burned.n, 0,
        "the burn must be rolled back with the failed enqueue. A surviving row here is \
         PERMANENT: the unique index means the alias can never be claimed again, so no \
         merge is ever scheduled for it and that guest's pre-login history is orphaned \
         forever, with no error anywhere."
    );

    // …and the proof that it is recoverable rather than merely absent: the
    // next identify claims it cleanly and DOES schedule a merge.
    let retry = sauron_db::identity_merge::claim_and_schedule(
        &mut conn,
        ids.app_id,
        "anon_atomic",
        "u-atomic",
    )
    .await
    .expect("retry after the injection is removed");
    assert!(matches!(retry, Claim::Fresh), "got {retry:?}");
    let queued: Count = diesel::sql_query(
        "SELECT count(*)::bigint AS n FROM identity_merges WHERE app_id = $1 AND alias_id = $2",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .bind::<Text, _>("anon_atomic")
    .get_result(&mut conn)
    .await
    .expect("count identity_merges");
    assert_eq!(
        queued.n, 1,
        "the retry must schedule the merge the failed attempt did not"
    );

    drop(conn);
    db.cleanup().await;
}
