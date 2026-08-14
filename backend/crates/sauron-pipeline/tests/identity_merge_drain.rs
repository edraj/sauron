//! `merge::drain_once` — the actual wiring the worker runs, not just the
//! primitives it calls.
//!
//! `sauron-db/tests/identity_merge.rs` proves `claim_next`/`rewrite_hot_rows`/
//! `fold_rollups`/`complete_merge`/`fail_merge` are each individually
//! correct, but no test anywhere calls `drain_once` itself — a version that
//! called `fold_rollups` twice, skipped `complete_merge`, wrapped everything
//! in a `BEGIN` (the exact hazard Task 6 was warned about), or passed the
//! wrong `hot_days` would pass every other suite in the repo unchanged. This
//! file exists to close that gap, the same way `identity_merge_batch.rs`
//! exists to prove the batched call site (not just `process_identify`)
//! enqueues a merge.

use std::cell::Cell;

use chrono::{DateTime, Duration, Utc};
use diesel::sql_types::{Nullable, Text, Timestamptz, Uuid as SqlUuid};
use diesel_async::RunQueryDsl;
use sauron_db::models::NewAppEnvironment;
use sauron_db::repo;
use uuid::Uuid;

/// One throwaway database per test. Duplicated rather than shared with
/// `identity_merge_batch.rs`'s own private `TestDb` for the identical reason
/// its doc comment gives: each integration-test file is its own crate, so
/// nothing declared `#[cfg(test)]` — or simply private — in another file (or
/// in `sauron-db`'s own `tests/common`) is visible here. Discriminator "md"
/// (merge-drain), distinct from "ib" (identify-batch), so the two harnesses'
/// ephemeral database names cannot collide.
struct TestDb {
    pool: sauron_db::PgPool,
    admin_url: String,
    db_name: String,
    cleaned_up: Cell<bool>,
}

impl TestDb {
    async fn setup() -> Option<Self> {
        let admin_url = std::env::var("TEST_DATABASE_URL").ok()?;
        let db_name = format!(
            "sauron_test_{}_md{}",
            Utc::now().timestamp(),
            Uuid::new_v4().simple()
        );
        sauron_db::create_database(&admin_url, &db_name)
            .await
            .expect("create ephemeral test database");
        let db_url = swap_database(&admin_url, &db_name);
        sauron_db::run_pending_migrations(&db_url)
            .await
            .expect("run migrations on ephemeral test database");
        let pool = sauron_db::build_pool(&db_url, 2).expect("build test pool");
        Some(Self {
            pool,
            admin_url,
            db_name,
            cleaned_up: Cell::new(false),
        })
    }

    async fn cleanup(&self) {
        sauron_db::drop_database(&self.admin_url, &self.db_name)
            .await
            .expect("drop ephemeral test database");
        self.cleaned_up.set(true);
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        if !self.cleaned_up.get() {
            eprintln!(
                "WARNING: ephemeral test database {} may remain (TestDb::cleanup() was never \
                 reached — the test likely panicked). It is named so sauron-db's stale-db \
                 reaper will collect it after 3h, or drop it manually:\n  \
                 DROP DATABASE \"{}\" WITH (FORCE);",
                self.db_name, self.db_name
            );
        }
    }
}

fn swap_database(url: &str, new_db: &str) -> String {
    let (scheme, rest) = url
        .split_once("://")
        .expect("TEST_DATABASE_URL must be scheme://...");
    let auth_end = rest.find(['/', '?']).unwrap_or(rest.len());
    let authority = &rest[..auth_end];
    let after = &rest[auth_end..];
    let query = after.find('?').map(|i| &after[i..]).unwrap_or("");
    format!("{scheme}://{authority}/{new_db}{query}")
}

struct SeedIds {
    app_id: Uuid,
    environment_id: Uuid,
}

async fn seed_app(pool: &sauron_db::PgPool) -> SeedIds {
    let mut conn = sauron_db::conn(pool).await.expect("checkout");
    let suffix = Uuid::new_v4().simple().to_string();
    let org = repo::create_org(&mut conn, "md org", &format!("md-org-{suffix}"))
        .await
        .expect("create org");
    let project = repo::create_project(
        &mut conn,
        org.id,
        "md project",
        &format!("md-project-{suffix}"),
    )
    .await
    .expect("create project");
    let app = repo::create_app(
        &mut conn,
        project.id,
        "md app",
        &format!("md-app-{suffix}"),
        "web",
    )
    .await
    .expect("create app");
    let env = repo::create_project_environment(&mut conn, project.id, "production")
        .await
        .expect("create catalogue env");
    let environment_id = repo::create_app_environments(
        &mut conn,
        &[NewAppEnvironment {
            app_id: app.id,
            environment_id: env.id,
            public_key: &format!("pk_md_{suffix}"),
            is_default: true,
        }],
    )
    .await
    .expect("enroll app in env")
    .remove(0)
    .id;

    SeedIds {
        app_id: app.id,
        environment_id,
    }
}

/// Queue a merge directly, bypassing `claim_identity`/`claim_and_schedule` —
/// this file is testing the DRAIN side of the queue, not the enqueue side,
/// which `identity_merge_batch.rs` and `sauron-db/tests/identity_merge.rs`
/// already cover.
async fn enqueue(pool: &sauron_db::PgPool, app_id: Uuid, alias_id: &str, distinct_id: &str) {
    let mut conn = sauron_db::conn(pool).await.expect("checkout");
    diesel::sql_query(
        "INSERT INTO identity_merges (app_id, alias_id, distinct_id) VALUES ($1, $2, $3)",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Text, _>(alias_id)
    .bind::<Text, _>(distinct_id)
    .execute(&mut conn)
    .await
    .expect("enqueue merge");
}

/// Give an alias something for `fold_rollups`'s span capture to read, so a
/// merge is not a total no-op — proving `drain_once` actually invoked
/// `fold_rollups` (`alias_first_seen` gets set) rather than merely flipping
/// `identity_merges.state`.
async fn seed_activity(
    pool: &sauron_db::PgPool,
    ids: &SeedIds,
    distinct_id: &str,
    at: DateTime<Utc>,
) {
    let mut conn = sauron_db::conn(pool).await.expect("checkout");
    diesel::sql_query(
        "INSERT INTO event_user_environments \
           (app_id, distinct_id, environment_id, first_seen, last_seen, events_count) \
         VALUES ($1, $2, $3, $4, $4, 1)",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .bind::<Text, _>(distinct_id)
    .bind::<Nullable<SqlUuid>, _>(Some(ids.environment_id))
    .bind::<Timestamptz, _>(at)
    .execute(&mut conn)
    .await
    .expect("seed event_user_environments row");
}

#[derive(diesel::QueryableByName, Debug)]
struct MergeRow {
    #[diesel(sql_type = Text)]
    state: String,
    #[diesel(sql_type = Nullable<Text>)]
    last_error: Option<String>,
    #[diesel(sql_type = Nullable<Timestamptz>)]
    alias_first_seen: Option<DateTime<Utc>>,
    #[diesel(sql_type = diesel::sql_types::Integer)]
    attempts: i32,
}

async fn read_merge(pool: &sauron_db::PgPool, app_id: Uuid, alias_id: &str) -> MergeRow {
    let mut conn = sauron_db::conn(pool).await.expect("checkout");
    diesel::sql_query(
        "SELECT state, last_error, alias_first_seen, attempts FROM identity_merges \
          WHERE app_id = $1 AND alias_id = $2",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Text, _>(alias_id)
    .get_result(&mut conn)
    .await
    .expect("read back the queue row")
}

/// The headline case: a queued merge runs end to end through `drain_once`
/// (not a hand-rolled claim/rewrite/fold/complete sequence) and lands
/// `state = 'done'`. `alias_first_seen` being set proves `fold_rollups` ran,
/// not just that the state column was flipped.
#[tokio::test]
async fn the_drain_executes_a_queued_merge_end_to_end() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = seed_app(&db.pool).await;
    let at = Utc::now() - Duration::hours(1);
    seed_activity(&db.pool, &ids, "anon_e2e", at).await;
    enqueue(&db.pool, ids.app_id, "anon_e2e", "u-e2e").await;

    let done = sauron_pipeline::merge::drain_once(&db.pool, 30)
        .await
        .expect("drain");
    assert_eq!(done, 1, "the one queued merge must be executed");

    let row = read_merge(&db.pool, ids.app_id, "anon_e2e").await;
    assert_eq!(
        row.state, "done",
        "a successfully executed merge lands 'done'"
    );
    assert!(
        row.alias_first_seen.is_some(),
        "alias_first_seen must be set — proves fold_rollups actually ran, not just that \
         drain_once flipped the state column"
    );

    db.cleanup().await;
}

/// A merge that fails lands `'failed'` with `last_error` set, and — just as
/// importantly, since nothing here bounds the `while let Some(job) = ...`
/// loop except the queue itself — `drain_once` still returns rather than
/// spinning on the same row forever.
///
/// Forces the failure with the same BEFORE-trigger technique
/// `sauron-db/tests/identity_merge.rs`'s race tests use: the trigger only
/// fires on the specific UPDATE `fold_rollups`'s span capture issues (it
/// touches `alias_first_seen`; `claim_next`/`fail_merge` never do), so
/// `claim_next` and the eventual `fail_merge` both succeed normally and only
/// the merge's own work fails.
#[tokio::test]
async fn a_failing_merge_lands_failed_with_last_error_and_the_drain_returns() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = seed_app(&db.pool).await;
    seed_activity(
        &db.pool,
        &ids,
        "boom_alias",
        Utc::now() - Duration::hours(1),
    )
    .await;
    enqueue(&db.pool, ids.app_id, "boom_alias", "u-boom").await;

    let mut conn = sauron_db::conn(&db.pool).await.expect("checkout");
    diesel::sql_query(
        "CREATE FUNCTION identity_merge_drain_boom() RETURNS trigger LANGUAGE plpgsql AS $fn$
         BEGIN
           IF OLD.alias_id = 'boom_alias' AND NEW.alias_first_seen IS DISTINCT FROM OLD.alias_first_seen THEN
             RAISE EXCEPTION 'synthetic failure for drain test';
           END IF;
           RETURN NEW;
         END $fn$",
    )
    .execute(&mut conn)
    .await
    .expect("create the boom trigger function");
    diesel::sql_query(
        "CREATE TRIGGER identity_merge_drain_boom_trg BEFORE UPDATE ON identity_merges \
         FOR EACH ROW EXECUTE FUNCTION identity_merge_drain_boom()",
    )
    .execute(&mut conn)
    .await
    .expect("create the boom trigger");
    drop(conn);

    let done = sauron_pipeline::merge::drain_once(&db.pool, 30)
        .await
        .expect("drain must return Ok even though the one job it processed failed");
    assert_eq!(
        done, 0,
        "a failed merge must not count toward the completed total"
    );

    let row = read_merge(&db.pool, ids.app_id, "boom_alias").await;
    assert_eq!(row.state, "failed");
    assert!(
        row.last_error.as_deref().is_some_and(|e| !e.is_empty()),
        "last_error must carry the failure, got {:?}",
        row.last_error
    );

    db.cleanup().await;
}

#[derive(diesel::QueryableByName)]
struct StateOnly {
    #[diesel(sql_type = Text)]
    state: String,
}

async fn read_state(pool: &sauron_db::PgPool, app_id: Uuid, alias_id: &str) -> String {
    let mut conn = sauron_db::conn(pool).await.expect("checkout");
    let row: StateOnly =
        diesel::sql_query("SELECT state FROM identity_merges WHERE app_id = $1 AND alias_id = $2")
            .bind::<SqlUuid, _>(app_id)
            .bind::<Text, _>(alias_id)
            .get_result(&mut conn)
            .await
            .expect("read back state");
    row.state
}

/// The lease: a merge stranded in `running` with a stale `claimed_at` (the
/// shape a dead worker leaves behind — no reaper resets it, so without this
/// reclaim path it would never move again) IS picked back up and completed.
/// A `running` row with a FRESH `claimed_at` — still genuinely in flight, as
/// far as the drain can tell — is NOT touched.
#[tokio::test]
async fn a_stranded_running_merge_is_reclaimed_but_a_fresh_one_is_not() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = seed_app(&db.pool).await;
    let mut conn = sauron_db::conn(&db.pool).await.expect("checkout");

    // Older than RUNNING_LEASE_MINUTES (15) — an orphan from a worker that
    // died mid-merge.
    diesel::sql_query(
        "INSERT INTO identity_merges (app_id, alias_id, distinct_id, state, attempts, claimed_at) \
         VALUES ($1, 'stale_running', 'u-stale', 'running', 1, now() - interval '20 minutes')",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .execute(&mut conn)
    .await
    .expect("seed stranded running row");

    // Fresh — indistinguishable from a merge genuinely in progress right now.
    diesel::sql_query(
        "INSERT INTO identity_merges (app_id, alias_id, distinct_id, state, attempts, claimed_at) \
         VALUES ($1, 'fresh_running', 'u-fresh', 'running', 1, now())",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .execute(&mut conn)
    .await
    .expect("seed fresh running row");
    drop(conn);

    let done = sauron_pipeline::merge::drain_once(&db.pool, 30)
        .await
        .expect("drain");
    assert_eq!(
        done, 1,
        "exactly the stranded row must be reclaimed and completed; the fresh one must not"
    );

    assert_eq!(
        read_state(&db.pool, ids.app_id, "stale_running").await,
        "done",
        "a running merge whose lease expired must be reclaimed and completed"
    );
    assert_eq!(
        read_state(&db.pool, ids.app_id, "fresh_running").await,
        "running",
        "a running merge with a fresh claimed_at must be left alone — it may still be genuinely \
         in flight"
    );

    db.cleanup().await;
}

/// Backoff: a merge that just failed must not be immediately reclaimed by the
/// very next drain pass. Without this a merge failing against anything
/// longer-lived than an instant burns its entire retry budget back-to-back
/// inside the same contention window.
#[tokio::test]
async fn a_just_failed_merge_is_not_immediately_reclaimed() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = seed_app(&db.pool).await;
    enqueue(&db.pool, ids.app_id, "backoff_alias", "u-backoff").await;

    // Fail it once directly (not through drain_once, so the failure is
    // deterministic and does not depend on the trigger fixture above).
    let mut conn = sauron_db::conn(&db.pool).await.expect("checkout");
    let job = sauron_db::identity_merge::claim_next(&mut conn)
        .await
        .expect("claim")
        .expect("one pending merge");
    sauron_db::identity_merge::fail_merge(&mut conn, job.id, job.claimed_at, "synthetic")
        .await
        .expect("fail");
    drop(conn);

    let before = read_merge(&db.pool, ids.app_id, "backoff_alias").await;
    assert_eq!(before.state, "failed");
    assert_eq!(before.attempts, 1);

    let done = sauron_pipeline::merge::drain_once(&db.pool, 30)
        .await
        .expect("drain");
    assert_eq!(
        done, 0,
        "a just-failed row must not be reclaimed before its backoff elapses"
    );

    let after = read_merge(&db.pool, ids.app_id, "backoff_alias").await;
    assert_eq!(after.state, "failed", "must remain failed, not reclaimed");
    assert_eq!(
        after.attempts, 1,
        "attempts must not have incremented — the row was never re-claimed"
    );

    db.cleanup().await;
}

/// The case `a_stranded_running_merge_is_reclaimed_but_a_fresh_one_is_not`
/// cannot see: a worker dying on its LAST attempt. That test seeds
/// `attempts = 1`, so `claim_next`'s reclaim arm (`attempts < MAX_ATTEMPTS`)
/// happily picks it back up. A row at `attempts = MAX_ATTEMPTS` is different —
/// `claim_next` will never reclaim it (there is nothing left to retry), so
/// without `reap_exhausted` it would sit in `running` — which IS in the
/// runnable partial index — at the head of every scan forever, indistinguishable
/// from a merge genuinely in progress. This asserts `drain_once` reaps it to
/// the terminal `dead` state instead.
#[tokio::test]
async fn an_exhausted_stranded_merge_reaches_dead_not_stuck_running() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = seed_app(&db.pool).await;
    let mut conn = sauron_db::conn(&db.pool).await.expect("checkout");

    // Pinned to an exact, known value rather than `now() - interval` so the
    // "the reap did not touch claimed_at" assertion at the end of this test
    // can be an equality against a value read back before the drain ran.
    diesel::sql_query(
        "INSERT INTO identity_merges \
           (app_id, alias_id, distinct_id, state, attempts, claimed_at) \
         VALUES ($1, 'exhausted_running', 'u-exhausted', 'running', $2, \
                 now() - interval '20 minutes')",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .bind::<diesel::sql_types::Integer, _>(sauron_db::identity_merge::MAX_ATTEMPTS)
    .execute(&mut conn)
    .await
    .expect("seed exhausted running row");
    let claimed_before = read_claimed_at(&mut conn, ids.app_id, "exhausted_running").await;

    // The complement: also at attempts = MAX_ATTEMPTS (so the reap's
    // `attempts >= MAX_ATTEMPTS` gate does not itself exclude it — unlike
    // `a_stranded_running_merge_is_reclaimed_but_a_fresh_one_is_not`'s "fresh"
    // row, which sits at attempts = 1 and so tells us nothing about whether
    // reap_exhausted respects the LEASE specifically), but with a FRESH
    // claimed_at. Without the lease clause in reap_exhausted's predicate,
    // this row — genuinely still in progress on its last attempt — would be
    // reaped instantly alongside the truly-stranded one above.
    diesel::sql_query(
        "INSERT INTO identity_merges \
           (app_id, alias_id, distinct_id, state, attempts, claimed_at) \
         VALUES ($1, 'exhausted_but_fresh', 'u-exhausted-fresh', 'running', $2, now())",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .bind::<diesel::sql_types::Integer, _>(sauron_db::identity_merge::MAX_ATTEMPTS)
    .execute(&mut conn)
    .await
    .expect("seed exhausted-but-fresh running row");
    drop(conn);

    let done = sauron_pipeline::merge::drain_once(&db.pool, 30)
        .await
        .expect("drain");
    assert_eq!(
        done, 0,
        "reaping an exhausted orphan is not a completed merge"
    );

    assert_eq!(
        read_state(&db.pool, ids.app_id, "exhausted_running").await,
        "dead",
        "a running merge with no attempts left and an expired lease must reach the terminal \
         'dead' state, not stay stuck in 'running' forever"
    );
    assert_eq!(
        read_state(&db.pool, ids.app_id, "exhausted_but_fresh").await,
        "running",
        "a running merge at MAX_ATTEMPTS whose lease has NOT expired must be left alone — it may \
         still be genuinely in progress on its last try; the reap must respect the lease, not \
         just the attempts count"
    );

    // `reap_exhausted` must leave `claimed_at` EXACTLY as it found it, and
    // nothing asserted that before this line. It is the entire premise of
    // `complete_merge`'s widened `state IN ('running', 'dead')` fence: a
    // worker that is merely slow — still genuinely running past its own lease
    // on its last attempt — gets its row reaped to 'dead' underneath it, and
    // the ONLY reason it can still land a correcting `complete_merge`
    // afterwards is that the token it is holding still matches the row. A
    // future, entirely reasonable-looking edit ("record when we reaped it")
    // that stamps `claimed_at = now()` in the reap breaks that fence
    // silently: the merge really happened, the row says 'dead' forever, and
    // every existing test in this suite stays green. This equality is the
    // tripwire.
    let mut conn = sauron_db::conn(&db.pool).await.expect("checkout");
    let claimed_after = read_claimed_at(&mut conn, ids.app_id, "exhausted_running").await;
    drop(conn);
    assert_eq!(
        claimed_after, claimed_before,
        "reap_exhausted must not touch claimed_at — it is the fencing token a slow-but-live \
         worker still holds, and rewriting it makes that worker's correcting complete_merge \
         match nothing"
    );

    db.cleanup().await;
}

/// The `claimed_at` fencing token of one queue row. `expect`s rather than
/// returning an `Option`: every caller seeds the row itself, so a NULL or a
/// missing row is a broken test, not a case to handle.
async fn read_claimed_at(
    conn: &mut sauron_db::PgConn,
    app_id: uuid::Uuid,
    alias: &str,
) -> chrono::DateTime<chrono::Utc> {
    #[derive(diesel::QueryableByName)]
    struct ClaimedAt {
        #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Timestamptz>)]
        claimed_at: Option<chrono::DateTime<chrono::Utc>>,
    }
    let row: ClaimedAt = diesel::sql_query(
        "SELECT claimed_at FROM identity_merges WHERE app_id = $1 AND alias_id = $2",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<diesel::sql_types::Text, _>(alias)
    .get_result(conn)
    .await
    .expect("read claimed_at");
    row.claimed_at.expect("claimed_at must be set")
}

/// The fencing token: simulate the exact race `complete_merge`/`fail_merge`'s
/// doc comments describe. W1 claims a job; before it finishes, its lease is
/// treated as expired (forced here rather than waiting out the real 15
/// minutes) and W2 reclaims the SAME row, getting a fresh `claimed_at`. W2
/// finishes first and completes it. W1 — merely slow, not actually dead —
/// then finally finishes and tries its own terminal write with the STALE
/// `claimed_at` it originally captured. That write must find nothing to
/// update, and the row must still read exactly what W2 recorded.
#[tokio::test]
async fn a_stolen_jobs_original_worker_cannot_overwrite_the_winner() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = seed_app(&db.pool).await;
    enqueue(&db.pool, ids.app_id, "stolen_alias", "u-stolen").await;

    let mut conn = sauron_db::conn(&db.pool).await.expect("checkout");

    // W1 claims it.
    let job_w1 = sauron_db::identity_merge::claim_next(&mut conn)
        .await
        .expect("claim")
        .expect("one pending merge");

    // Force W1's lease to look expired, simulating it running long past
    // RUNNING_LEASE_MINUTES without having died — this is what a heavy
    // guest's cross-partition rewrite, or a lock wait, can do for real.
    diesel::sql_query(
        "UPDATE identity_merges SET claimed_at = now() - interval '20 minutes' WHERE id = $1",
    )
    .bind::<SqlUuid, _>(job_w1.id)
    .execute(&mut conn)
    .await
    .expect("force the lease to look expired");

    // W2 reclaims the same row and gets a NEW claimed_at — a different
    // fencing token from the one W1 is still holding.
    let job_w2 = sauron_db::identity_merge::claim_next(&mut conn)
        .await
        .expect("claim")
        .expect("the stale lease must be reclaimable");
    assert_eq!(job_w2.id, job_w1.id, "W2 must reclaim the SAME row");
    assert_ne!(
        job_w2.claimed_at, job_w1.claimed_at,
        "the reclaim must mint a new fencing token, or this test proves nothing"
    );

    // W1 — unaware it was stolen from — tries its own (contradictory)
    // terminal write FIRST, using the stale token it captured back when it
    // first claimed the row, WHILE THE ROW IS STILL 'running' under W2's
    // claim (W2 has not written its own outcome yet). This ordering is
    // deliberate and is the crux of the fence: a `state = 'running'` check
    // ALONE cannot catch this — the row genuinely IS `running` right now, just
    // under W2's claim, not W1's. Only comparing the exact `claimed_at`
    // distinguishes "my claim" from "a later claim that stole this row from
    // me". (The simpler ordering — W1 writing AFTER W2 already completed —
    // would be caught by `state = 'running'` alone, since `state` would
    // already have moved to `'done'`; it would not discriminate the
    // `claimed_at` fence from a plain `state` fence, which is why this test
    // does not use it.)
    let w1_updated = sauron_db::identity_merge::fail_merge(
        &mut conn,
        job_w1.id,
        job_w1.claimed_at,
        "W1 finished late",
    )
    .await
    .expect("W1's terminal write must not error, just find nothing");
    assert_eq!(
        w1_updated, 0,
        "W1 no longer holds the current claim; its write must match nothing, even though the \
         row is still genuinely 'running' (under W2's claim, not W1's)"
    );

    // Ground truth after W1's premature write: the row must still be
    // 'running' under W2's claim, untouched by W1 — proving the fence, not
    // just a lucky ordering, is what protected it.
    let mid_row = read_merge(&db.pool, ids.app_id, "stolen_alias").await;
    assert_eq!(
        mid_row.state, "running",
        "W1's fenced-out write must not have changed the state — W2's merge is still genuinely \
         in progress"
    );
    assert!(
        mid_row.last_error.is_none(),
        "W1's error message must never have been written"
    );

    // W2 now finishes and records success — its claim is still current.
    let w2_updated =
        sauron_db::identity_merge::complete_merge(&mut conn, job_w2.id, job_w2.claimed_at)
            .await
            .expect("W2's completion");
    assert_eq!(
        w2_updated, 1,
        "W2 holds the current claim; its write must land"
    );

    // Final ground truth: the row reads exactly what W2 recorded.
    let row = read_merge(&db.pool, ids.app_id, "stolen_alias").await;
    assert_eq!(
        row.state, "done",
        "the winner's outcome (W2's success) must survive; the stale loser (W1) must not have \
         been able to flip it to failed/dead"
    );
    assert!(
        row.last_error.is_none(),
        "W1's error message must never have been written — complete_merge already cleared \
         last_error, and W1's fenced-out fail_merge must not have reinstated one"
    );

    drop(conn);
    db.cleanup().await;
}

/// **The one-shot merge bug, reproduced end to end.**
///
/// A merge used to run exactly once, ~0–5 s after the identify commits, and
/// nothing ever ran it again: `enqueue_merge` is reachable only from
/// `claim_and_schedule` on `Claim::Fresh`, and the burn rule means a
/// `Fresh` claim happens once per alias, ever. So any row carrying the alias
/// that landed AFTER that sweep was never rewritten — and since
/// `fold_rollups` had already set `cold_stale = false` for a guest who
/// converted inside `hot_days`, the cold overlay excluded that alias too.
/// Both tiers double-counted the human permanently, with no error anywhere.
///
/// Stragglers are not exotic: eight workers drain one Redis stream with no
/// cross-consumer ordering, the retry ZSET replays failed items minutes
/// later, and a mobile offline queue flushes pre-login events long after they
/// occurred.
///
/// This drives the REAL sequence — `claim_and_schedule` for the first
/// identify, `drain_once`, then a straggler event, then a repeat
/// `identify()` (the `Claim::Repeat` every page load after login emits), then
/// `drain_once` again — and asserts the straggler is rewritten and the person
/// count returns to 1. Deliberately not a direct `rearm_merge` call: the bug
/// was that nothing CALLED it, so a test that calls it itself would pass
/// against the broken code.
#[tokio::test]
async fn a_straggler_arriving_after_the_merge_is_swept_by_a_repeat_identify() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = seed_app(&db.pool).await;
    let at = Utc::now() - Duration::hours(1);
    seed_activity(&db.pool, &ids, "anon_late", at).await;
    seed_event(&db.pool, ids.app_id, "anon_late", at).await;

    // First identify: Fresh claim + enqueue, exactly as the ingest path runs.
    {
        let mut conn = sauron_db::conn(&db.pool).await.expect("checkout");
        let claim = sauron_db::identity_merge::claim_and_schedule(
            &mut conn,
            ids.app_id,
            "anon_late",
            "u-late",
        )
        .await
        .expect("claim");
        assert_eq!(
            claim,
            sauron_db::identity_merge::Claim::Fresh,
            "precondition: the first identify must be a fresh claim"
        );
    }
    assert_eq!(
        sauron_pipeline::merge::drain_once(&db.pool, 30)
            .await
            .expect("drain"),
        1,
        "precondition: the first sweep must run"
    );
    assert_eq!(
        distinct_people(&db.pool, ids.app_id).await,
        1,
        "precondition: after the first sweep the guest and the person are one"
    );

    // The straggler: a pre-login event persisted AFTER the sweep, still
    // carrying the alias. This is the row the old code never rewrote.
    seed_event(&db.pool, ids.app_id, "anon_late", at).await;
    assert_eq!(
        distinct_people(&db.pool, ids.app_id).await,
        2,
        "precondition: the straggler must genuinely re-split the person, or this test \
         is asserting nothing"
    );

    // The repeat identify — a page load after login. This is the trigger.
    {
        let mut conn = sauron_db::conn(&db.pool).await.expect("checkout");
        let claim = sauron_db::identity_merge::claim_and_schedule(
            &mut conn,
            ids.app_id,
            "anon_late",
            "u-late",
        )
        .await
        .expect("re-claim");
        assert_eq!(
            claim,
            sauron_db::identity_merge::Claim::Repeat,
            "a second identify by the same person under the same alias is a Repeat"
        );
    }
    assert_eq!(
        read_state(&db.pool, ids.app_id, "anon_late").await,
        "pending",
        "the Repeat must have re-armed the completed merge; without this the straggler \
         is never swept and both tiers double-count this human forever"
    );

    // The re-arm sets `next_attempt_at = now() + REARM_GRACE_SECS`, which is
    // a deliberate several-minute delay — a burst of page loads must not
    // re-sweep continuously. Wind it back rather than sleeping: the grace is
    // the thing under test in `the_rearm_grace_defers_the_sweep` below, not
    // here.
    {
        let mut conn = sauron_db::conn(&db.pool).await.expect("checkout");
        diesel::sql_query(
            "UPDATE identity_merges SET next_attempt_at = now() - interval '1 second' \
              WHERE app_id = $1 AND alias_id = 'anon_late'",
        )
        .bind::<SqlUuid, _>(ids.app_id)
        .execute(&mut conn)
        .await
        .expect("wind the grace back");
    }

    assert_eq!(
        sauron_pipeline::merge::drain_once(&db.pool, 30)
            .await
            .expect("second drain"),
        1,
        "the re-armed merge must be claimed and executed"
    );
    assert_eq!(
        distinct_people(&db.pool, ids.app_id).await,
        1,
        "the straggler must have been rewritten onto the person — the headline assertion \
         of this whole feature, applied to a row that arrived after the first sweep"
    );

    let row = read_merge(&db.pool, ids.app_id, "anon_late").await;
    assert_eq!(row.state, "done", "the re-armed merge completes normally");
    assert!(
        row.alias_first_seen.is_some(),
        "the span must survive the re-sweep — fold_rollups widens it (LEAST/GREATEST), \
         it must never be blanked or replaced by the straggler's narrower own span"
    );

    db.cleanup().await;
}

/// The grace interval is load-bearing in the other direction: a burst of page
/// loads must not turn every `identify()` into an immediate re-sweep. The
/// `state = 'done'` predicate is what makes the burst re-arm exactly once,
/// and `next_attempt_at` is what keeps the one re-arm off the very next drain
/// cycle.
#[tokio::test]
async fn the_rearm_grace_defers_the_sweep_and_a_burst_rearms_only_once() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = seed_app(&db.pool).await;
    seed_activity(
        &db.pool,
        &ids,
        "anon_burst",
        Utc::now() - Duration::hours(1),
    )
    .await;

    let mut conn = sauron_db::conn(&db.pool).await.expect("checkout");
    sauron_db::identity_merge::claim_and_schedule(&mut conn, ids.app_id, "anon_burst", "u-burst")
        .await
        .expect("claim");
    drop(conn);
    sauron_pipeline::merge::drain_once(&db.pool, 30)
        .await
        .expect("drain");

    // A burst: ten page loads, each emitting an identify().
    let mut conn = sauron_db::conn(&db.pool).await.expect("checkout");
    for _ in 0..10 {
        sauron_db::identity_merge::claim_and_schedule(
            &mut conn,
            ids.app_id,
            "anon_burst",
            "u-burst",
        )
        .await
        .expect("repeat claim");
    }
    drop(conn);

    assert_eq!(
        read_state(&db.pool, ids.app_id, "anon_burst").await,
        "pending",
        "the burst re-armed the merge"
    );
    // Only the FIRST repeat can match `state = 'done'`; the other nine find a
    // 'pending' row and change nothing. If they did match, each would push
    // `next_attempt_at` forward by the grace again — under a continuous page-
    // load stream the sweep would be starved and never run at all, which is a
    // worse bug than the one the re-arm fixes.
    assert_eq!(
        sauron_pipeline::merge::drain_once(&db.pool, 30)
            .await
            .expect("drain during grace"),
        0,
        "a re-armed merge must not be claimable until its grace elapses, or a page-load \
         burst becomes a continuous re-sweep"
    );
    assert_eq!(
        read_state(&db.pool, ids.app_id, "anon_burst").await,
        "pending",
        "still waiting out the grace, not claimed"
    );

    db.cleanup().await;
}

/// A merge that is part of a CHAIN must never be executed, and must land in
/// the terminal `'dead'` state with a reason rather than being silently
/// skipped or left camping in the runnable index.
///
/// Both readers of the alias map already refuse a chained edge; the WRITER
/// did not, and a reader guard cannot undo what a writer has already done —
/// `rewrite_hot_rows` running `B → C` overwrites `guest_alias` from `'A'` to
/// `'B'`, destroying the original alias the design keeps so an unmerge stays
/// possible. Chains are seeded directly here, because
/// `claim_identity_locked`'s guards now refuse to create one.
#[tokio::test]
async fn a_chained_merge_is_refused_by_the_drain_and_parked_dead() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let ids = seed_app(&db.pool).await;
    let at = Utc::now() - Duration::hours(1);
    seed_activity(&db.pool, &ids, "chain_a", at).await;
    seed_activity(&db.pool, &ids, "chain_b", at).await;
    seed_event(&db.pool, ids.app_id, "chain_a", at).await;
    seed_event(&db.pool, ids.app_id, "chain_b", at).await;

    // A → B and B → C: a chain, in both roles.
    enqueue(&db.pool, ids.app_id, "chain_a", "chain_b").await;
    enqueue(&db.pool, ids.app_id, "chain_b", "chain_c").await;

    let done = sauron_pipeline::merge::drain_once(&db.pool, 30)
        .await
        .expect("drain");
    assert_eq!(done, 0, "neither leg of a chain may be executed");

    for alias in ["chain_a", "chain_b"] {
        let row = read_merge(&db.pool, ids.app_id, alias).await;
        assert_eq!(
            row.state, "dead",
            "{alias}: a chained merge must reach the terminal 'dead' state, not stay \
             pending (which would camp in the runnable index at the head of every scan)"
        );
        assert!(
            row.last_error
                .as_deref()
                .is_some_and(|e| e.contains("chain")),
            "{alias}: the refusal must say why — a silently skipped row is \
             indistinguishable from the drain being broken; last_error was {:?}",
            row.last_error
        );
    }

    // The data itself is untouched: this is the property the guard exists for.
    let mut conn = sauron_db::conn(&db.pool).await.expect("checkout");
    #[derive(diesel::QueryableByName)]
    struct GuestAlias {
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        n: i64,
    }
    let stamped: GuestAlias = diesel::sql_query(
        "SELECT count(*)::bigint AS n FROM analytics_events \
          WHERE app_id = $1 AND guest_alias IS NOT NULL",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .get_result(&mut conn)
    .await
    .expect("count stamped rows");
    drop(conn);
    assert_eq!(
        stamped.n, 0,
        "no chained rewrite may have touched the event rows — guest_alias is overwritten \
         irreversibly and no reader guard can restore it"
    );

    db.cleanup().await;
}

/// One analytics event for `distinct_id`, so a merge has something to rewrite
/// and `count(DISTINCT distinct_id)` has something to count.
async fn seed_event(pool: &sauron_db::PgPool, app_id: Uuid, distinct_id: &str, at: DateTime<Utc>) {
    let mut conn = sauron_db::conn(pool).await.expect("checkout");
    diesel::sql_query(
        "INSERT INTO analytics_events (id, app_id, name, distinct_id, occurred_at) \
         VALUES (gen_random_uuid(), $1, 'page_view', $2, $3)",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Text, _>(distinct_id)
    .bind::<Timestamptz, _>(at)
    .execute(&mut conn)
    .await
    .expect("seed analytics event");
}

/// `count(DISTINCT distinct_id)` over the app's analytics events — the
/// headline number this whole feature exists to keep at 1 for one human.
async fn distinct_people(pool: &sauron_db::PgPool, app_id: Uuid) -> i64 {
    let mut conn = sauron_db::conn(pool).await.expect("checkout");
    #[derive(diesel::QueryableByName)]
    struct N {
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        n: i64,
    }
    let row: N = diesel::sql_query(
        "SELECT count(DISTINCT distinct_id)::bigint AS n FROM analytics_events WHERE app_id = $1",
    )
    .bind::<SqlUuid, _>(app_id)
    .get_result(&mut conn)
    .await
    .expect("count distinct people");
    row.n
}
