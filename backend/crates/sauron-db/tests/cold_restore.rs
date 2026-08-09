//! Cold-data restore: the marker column, the expiry that un-does a restore, and
//! the job row that tracks one.
//!
//! The property everything here exists to protect is narrow and easy to lose:
//!
//!   **Expiry must delete exactly the rows the restore inserted, and nothing
//!   else.**
//!
//! A cold range that has been dropped from Postgres can still accumulate rows —
//! a client sends an event with an old `occurred_at`, and because the explicit
//! partition is gone it lands in `<table>_default`. Those rows are NOT in
//! Parquet; they exist in exactly one place. A restore then adds rows to the
//! same partition that ARE in Parquet. The two are now interleaved and look
//! identical, and "clean up the restore" without a marker means guessing.
//! Guessing wrong destroys the only copy of a real event.
//!
//! `restored_pin_id` is the marker, and
//! `expiry_deletes_only_restored_rows_not_late_arrivals` is the test that says
//! so. If that one ever goes red, the feature is deleting customer data.
//!
//! Skips (does not fail) when `TEST_DATABASE_URL` is unset — see `common/mod.rs`.

mod common;

use chrono::{DateTime, Duration, Utc};
use diesel::sql_types::{Timestamptz, Uuid as SqlUuid};
use diesel_async::RunQueryDsl;
use uuid::Uuid;

use sauron_db::repo;

use common::{seed_signal_error, TestDb};

/// Stamp `restored_pin_id` on the error events in a range, standing in for what
/// `DuckEngine::restore_to_postgres` writes. Doing it as an UPDATE keeps these
/// tests free of DuckDB and Parquet: what is under test is the marker's
/// semantics, not the copy that produces it.
async fn mark_as_restored(
    conn: &mut sauron_db::PgConn,
    pin_id: Uuid,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> usize {
    diesel::sql_query(
        "UPDATE error_events SET restored_pin_id = $1 \
          WHERE occurred_at >= $2 AND occurred_at < $3 AND restored_pin_id IS NULL",
    )
    .bind::<SqlUuid, _>(pin_id)
    .bind::<Timestamptz, _>(from)
    .bind::<Timestamptz, _>(to)
    .execute(conn)
    .await
    .expect("mark restored")
}

async fn count_errors(conn: &mut sauron_db::PgConn) -> i64 {
    #[derive(diesel::QueryableByName)]
    struct N {
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        n: i64,
    }
    let r: N = diesel::sql_query("SELECT count(*)::bigint AS n FROM error_events")
        .get_result(conn)
        .await
        .expect("count");
    r.n
}

async fn count_marked(conn: &mut sauron_db::PgConn, pin_id: Uuid) -> i64 {
    #[derive(diesel::QueryableByName)]
    struct N {
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        n: i64,
    }
    let r: N = diesel::sql_query(
        "SELECT count(*)::bigint AS n FROM error_events WHERE restored_pin_id = $1",
    )
    .bind::<SqlUuid, _>(pin_id)
    .get_result(conn)
    .await
    .expect("count marked");
    r.n
}

// ===========================================================================
// The property the whole feature rests on
// ===========================================================================

#[tokio::test]
async fn expiry_deletes_only_restored_rows_not_late_arrivals() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut c = db.conn().await;

    let base = Utc::now() - Duration::days(60);
    let (start, end) = (base, base + Duration::days(1));

    // Measured, not assumed. An earlier version of this test hardcoded the
    // fixture's event count from a doc comment and failed on an off-by-one that
    // said nothing about the code under test.
    let fixture_rows = count_errors(&mut c).await;

    // Three rows that a restore brought back from Parquet...
    for i in 0..3 {
        seed_signal_error(
            &mut c,
            ids.app_id,
            None,
            ids.issue_id,
            None,
            start + Duration::hours(i),
        )
        .await;
    }
    let pin = repo::create_tier_pin(
        &mut c,
        "error_events",
        start,
        end,
        Utc::now() - Duration::minutes(1), // already lapsed
        None,
        Some("restore"),
    )
    .await
    .unwrap();
    assert_eq!(mark_as_restored(&mut c, pin.id, start, end).await, 3);

    // ...and two LATE ARRIVALS in the very same range, which exist ONLY here.
    // They must survive. Seeded after the marking so they stay unmarked.
    for i in 5..7 {
        seed_signal_error(
            &mut c,
            ids.app_id,
            None,
            ids.issue_id,
            None,
            start + Duration::hours(i),
        )
        .await;
    }

    let before = count_errors(&mut c).await;
    assert_eq!(
        before,
        fixture_rows + 5,
        "3 restored + 2 late arrivals on top of whatever the fixture seeded"
    );

    let expired = repo::expire_tier_pins(&mut c).await.unwrap();
    assert_eq!(expired.len(), 1);
    assert_eq!(
        expired[0].rows_deleted, 3,
        "exactly the restored rows, never the late arrivals"
    );
    assert_eq!(
        count_errors(&mut c).await,
        before - 3,
        "the two late arrivals are the only copy of those events and must remain"
    );
    assert_eq!(count_marked(&mut c, pin.id).await, 0);

    // The pin goes with them, in the same statement.
    assert!(repo::list_tier_pins(&mut c).await.unwrap().is_empty());

    db.cleanup().await;
}

// ===========================================================================
// delete_restored_rows
// ===========================================================================

#[tokio::test]
async fn delete_restored_rows_is_scoped_to_one_pin() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut c = db.conn().await;

    let base = Utc::now() - Duration::days(60);
    let (start, end) = (base, base + Duration::days(1));
    for i in 0..4 {
        seed_signal_error(
            &mut c,
            ids.app_id,
            None,
            ids.issue_id,
            None,
            start + Duration::hours(i),
        )
        .await;
    }

    let mine = Uuid::new_v4();
    let theirs = Uuid::new_v4();
    // Two rows belong to another restore entirely.
    diesel::sql_query(
        "UPDATE error_events SET restored_pin_id = $1 \
          WHERE occurred_at >= $2 AND occurred_at < $3",
    )
    .bind::<SqlUuid, _>(theirs)
    .bind::<Timestamptz, _>(start)
    .bind::<Timestamptz, _>(start + Duration::hours(2))
    .execute(&mut c)
    .await
    .unwrap();
    diesel::sql_query(
        "UPDATE error_events SET restored_pin_id = $1 \
          WHERE occurred_at >= $2 AND occurred_at < $3",
    )
    .bind::<SqlUuid, _>(mine)
    .bind::<Timestamptz, _>(start + Duration::hours(2))
    .bind::<Timestamptz, _>(end)
    .execute(&mut c)
    .await
    .unwrap();

    let n = repo::delete_restored_rows(&mut c, "error_events", mine, start, end)
        .await
        .unwrap();
    assert_eq!(n, 2, "only this pin's rows");
    assert_eq!(
        count_marked(&mut c, theirs).await,
        2,
        "the other restore is untouched"
    );

    db.cleanup().await;
}

#[tokio::test]
async fn delete_restored_rows_respects_the_range() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut c = db.conn().await;

    let base = Utc::now() - Duration::days(60);
    for i in 0..4 {
        seed_signal_error(
            &mut c,
            ids.app_id,
            None,
            ids.issue_id,
            None,
            base + Duration::hours(i),
        )
        .await;
    }
    let pin = Uuid::new_v4();
    mark_as_restored(&mut c, pin, base, base + Duration::days(1)).await;

    // Half-open: a delete over [base, base+2h) takes the rows at +0h and +1h only.
    let n =
        repo::delete_restored_rows(&mut c, "error_events", pin, base, base + Duration::hours(2))
            .await
            .unwrap();
    assert_eq!(n, 2);
    assert_eq!(
        count_marked(&mut c, pin).await,
        2,
        "rows outside the range remain"
    );

    db.cleanup().await;
}

#[tokio::test]
async fn delete_restored_rows_refuses_a_table_outside_the_allowlist() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut c = db.conn().await;

    // The table name is interpolated into SQL, so the allowlist is a security
    // boundary and not a convenience.
    //
    // This asserts the SHAPE of the failure, not merely that one occurred. An
    // earlier version passed `"users; DROP TABLE organizations"` and asserted
    // `is_err()` — which stayed green with the guard deleted, because the
    // resulting SQL was a syntax error and Postgres refused it instead. The test
    // proved Postgres has a parser, not that we have a guard. A mutation run
    // caught it.
    let err = repo::delete_restored_rows(
        &mut c,
        "issues",
        Uuid::new_v4(),
        Utc::now() - Duration::days(1),
        Utc::now(),
    )
    .await
    .expect_err("a non-restorable table must be rejected");
    match err {
        diesel::result::Error::QueryBuilderError(msg) => {
            assert!(
                msg.to_string().contains("non-restorable"),
                "must be OUR refusal, not a database error: {msg}"
            );
        }
        other => panic!("expected our allowlist refusal, got a database error: {other:?}"),
    }

    assert!(repo::is_restorable_table("error_events"));
    assert!(repo::is_restorable_table("analytics_events"));
    assert!(repo::is_restorable_table("transactions"));
    assert!(!repo::is_restorable_table("issues"));
    assert!(!repo::is_restorable_table("users"));
    assert!(!repo::is_restorable_table(""));

    db.cleanup().await;
}

// ===========================================================================
// Expiry, release, extend, warning window
// ===========================================================================

#[tokio::test]
async fn expiry_leaves_live_pins_and_their_rows_alone() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut c = db.conn().await;

    let base = Utc::now() - Duration::days(60);
    let (start, end) = (base, base + Duration::days(1));
    for i in 0..3 {
        seed_signal_error(
            &mut c,
            ids.app_id,
            None,
            ids.issue_id,
            None,
            start + Duration::hours(i),
        )
        .await;
    }
    let live = repo::create_tier_pin(
        &mut c,
        "error_events",
        start,
        end,
        Utc::now() + Duration::days(5),
        None,
        Some("live"),
    )
    .await
    .unwrap();
    mark_as_restored(&mut c, live.id, start, end).await;

    let expired = repo::expire_tier_pins(&mut c).await.unwrap();
    assert!(expired.is_empty(), "nothing has lapsed");
    assert_eq!(count_marked(&mut c, live.id).await, 3);

    db.cleanup().await;
}

#[tokio::test]
async fn release_removes_the_rows_immediately() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut c = db.conn().await;

    let base = Utc::now() - Duration::days(60);
    let (start, end) = (base, base + Duration::days(1));
    for i in 0..2 {
        seed_signal_error(
            &mut c,
            ids.app_id,
            None,
            ids.issue_id,
            None,
            start + Duration::hours(i),
        )
        .await;
    }
    // Expiry is far away: release must not wait for it.
    let pin = repo::create_tier_pin(
        &mut c,
        "error_events",
        start,
        end,
        Utc::now() + Duration::days(300),
        None,
        None,
    )
    .await
    .unwrap();
    mark_as_restored(&mut c, pin.id, start, end).await;

    let released = repo::release_tier_pin(&mut c, pin.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(released.rows_deleted, 2);
    assert_eq!(count_marked(&mut c, pin.id).await, 0);
    assert!(repo::list_tier_pins(&mut c).await.unwrap().is_empty());

    db.cleanup().await;
}

#[tokio::test]
async fn releasing_an_unknown_pin_reports_it_rather_than_pretending() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut c = db.conn().await;
    assert!(repo::release_tier_pin(&mut c, Uuid::new_v4())
        .await
        .unwrap()
        .is_none());
    db.cleanup().await;
}

#[tokio::test]
async fn extending_measures_from_now_not_from_the_old_expiry() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut c = db.conn().await;

    let base = Utc::now() - Duration::days(60);
    let pin = repo::create_tier_pin(
        &mut c,
        "error_events",
        base,
        base + Duration::days(1),
        Utc::now() + Duration::minutes(30), // nearly lapsed
        None,
        None,
    )
    .await
    .unwrap();

    let target = Utc::now() + Duration::days(30);
    let out = repo::extend_tier_pin(&mut c, pin.id, target)
        .await
        .unwrap()
        .unwrap();
    assert!(
        (out.expires_at - target).num_seconds().abs() < 2,
        "extension lands on the requested instant, not old_expiry + days"
    );
    assert!(repo::extend_tier_pin(&mut c, Uuid::new_v4(), target)
        .await
        .unwrap()
        .is_none());

    db.cleanup().await;
}

#[tokio::test]
async fn the_warning_window_excludes_both_the_far_future_and_the_already_lapsed() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut c = db.conn().await;
    let base = Utc::now() - Duration::days(60);
    let end = base + Duration::days(1);

    // Lapsed pins are NOT warnings — they are expiry's job, and surfacing them
    // as "expiring soon" would tell an operator to extend something whose rows
    // are already gone. Inserted out of expiry order so the assertion below is
    // testing the ORDER BY rather than the insertion order.
    for exp in [
        Utc::now() - Duration::hours(1),
        Utc::now() + Duration::days(60),
    ] {
        repo::create_tier_pin(&mut c, "error_events", base, end, exp, None, None)
            .await
            .unwrap();
    }
    // Creation order is deliberately the REVERSE of expiry order. An earlier
    // version created the sooner pin last, so ordering by `created_at DESC`
    // produced the same sequence as ordering by `expires_at ASC` and the
    // assertion below could not tell them apart — a mutation run caught it.
    let soon_a = repo::create_tier_pin(
        &mut c,
        "error_events",
        base,
        end,
        Utc::now() + Duration::days(2),
        None,
        None,
    )
    .await
    .unwrap();
    let soon_b = repo::create_tier_pin(
        &mut c,
        "error_events",
        base,
        end,
        Utc::now() + Duration::days(5),
        None,
        None,
    )
    .await
    .unwrap();

    let window = repo::pins_expiring_before(&mut c, Utc::now() + Duration::days(7))
        .await
        .unwrap();
    let got: Vec<Uuid> = window.iter().map(|p| p.id).collect();
    assert_eq!(
        got,
        vec![soon_a.id, soon_b.id],
        "only unlapsed pins inside the window, soonest first"
    );

    db.cleanup().await;
}

/// The read path keys on the pin ROW EXISTING, not on its expiry — because a
/// lapsed-but-not-yet-swept pin still has its rows in Postgres, and serving
/// that range from Parquet as well would double every count.
#[tokio::test]
async fn restored_ranges_still_reports_a_lapsed_pin() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut c = db.conn().await;
    let base = Utc::now() - Duration::days(60);

    repo::create_tier_pin(
        &mut c,
        "error_events",
        base,
        base + Duration::days(1),
        Utc::now() - Duration::hours(1), // lapsed, not yet swept
        None,
        None,
    )
    .await
    .unwrap();
    repo::create_tier_pin(
        &mut c,
        "analytics_events",
        base,
        base + Duration::days(1),
        Utc::now() + Duration::days(1),
        None,
        None,
    )
    .await
    .unwrap();

    let ranges = repo::restored_ranges(&mut c, "error_events").await.unwrap();
    assert_eq!(
        ranges.len(),
        1,
        "expiry does not hide a range whose rows are still there"
    );
    // ...whereas the DROP decision does respect expiry, which is the opposite
    // question and must stay opposite.
    assert!(
        !repo::is_range_pinned(&mut c, "error_events", base, base + Duration::days(1))
            .await
            .unwrap(),
        "is_range_pinned is about dropping partitions and must ignore lapsed pins"
    );
    assert_eq!(
        repo::restored_ranges(&mut c, "transactions")
            .await
            .unwrap()
            .len(),
        0,
        "scoped per table"
    );

    db.cleanup().await;
}

// ===========================================================================
// Restore jobs
// ===========================================================================

#[tokio::test]
async fn overlapping_active_restores_are_detected_and_finished_ones_ignored() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut c = db.conn().await;
    let base = Utc::now() - Duration::days(60);
    let exp = Utc::now() + Duration::days(30);

    let job = repo::create_restore_job(
        &mut c,
        "error_events",
        None,
        base,
        base + Duration::days(10),
        exp,
        None,
    )
    .await
    .unwrap();

    // Overlap, not containment: a partial overlap would still double-insert.
    assert!(repo::overlapping_active_restore(
        &mut c,
        "error_events",
        base + Duration::days(5),
        base + Duration::days(15)
    )
    .await
    .unwrap()
    .is_some());
    // Abutting is not overlapping.
    assert!(repo::overlapping_active_restore(
        &mut c,
        "error_events",
        base + Duration::days(10),
        base + Duration::days(20)
    )
    .await
    .unwrap()
    .is_none());
    // Another table is another queue.
    assert!(repo::overlapping_active_restore(
        &mut c,
        "transactions",
        base,
        base + Duration::days(10)
    )
    .await
    .unwrap()
    .is_none());

    // A finished job must not block a re-restore of the same range.
    repo::claim_one_restore_job(&mut c, "w1", 300)
        .await
        .unwrap();
    repo::finish_restore_job(&mut c, job.id, "w1", "succeeded", 7, "")
        .await
        .unwrap();
    assert!(repo::overlapping_active_restore(
        &mut c,
        "error_events",
        base,
        base + Duration::days(10)
    )
    .await
    .unwrap()
    .is_none());

    db.cleanup().await;
}

#[tokio::test]
async fn claiming_takes_one_job_and_holds_a_lease() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut c = db.conn().await;
    let base = Utc::now() - Duration::days(60);
    let exp = Utc::now() + Duration::days(30);

    for i in 0..2 {
        repo::create_restore_job(
            &mut c,
            "error_events",
            None,
            base + Duration::days(i * 10),
            base + Duration::days(i * 10 + 5),
            exp,
            None,
        )
        .await
        .unwrap();
    }

    let first = repo::claim_one_restore_job(&mut c, "w1", 300)
        .await
        .unwrap()
        .expect("a queued job");
    assert_eq!(first.status, "running");
    assert_eq!(first.attempts, 1);
    assert!(first.started_at.is_some());

    // A different worker gets the OTHER job, not the leased one.
    let second = repo::claim_one_restore_job(&mut c, "w2", 300)
        .await
        .unwrap()
        .expect("the second queued job");
    assert_ne!(second.id, first.id);

    // Nothing left to claim for a third worker while both leases hold.
    assert!(repo::claim_one_restore_job(&mut c, "w3", 300)
        .await
        .unwrap()
        .is_none());

    // The owner CAN re-claim its own running job — that is what lets the
    // executor yield and re-enter, and it bumps attempts so a poison job
    // eventually gives up.
    let again = repo::claim_one_restore_job(&mut c, "w1", 300)
        .await
        .unwrap()
        .expect("own running job is re-claimable");
    assert_eq!(again.id, first.id);
    assert_eq!(again.attempts, 2);

    // A lapsed lease is re-claimable by anyone: that IS the crash-resume path.
    let stolen = repo::claim_one_restore_job(&mut c, "w4", 0)
        .await
        .unwrap()
        .expect("expired lease is reclaimable");
    assert_eq!(stolen.worker_id.as_deref(), Some("w4"));

    db.cleanup().await;
}

#[tokio::test]
async fn progress_and_completion_are_guarded_by_worker_id() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut c = db.conn().await;
    let base = Utc::now() - Duration::days(60);

    let job = repo::create_restore_job(
        &mut c,
        "error_events",
        None,
        base,
        base + Duration::days(1),
        Utc::now() + Duration::days(30),
        None,
    )
    .await
    .unwrap();
    repo::claim_one_restore_job(&mut c, "owner", 300)
        .await
        .unwrap();

    // A worker whose lease was stolen must not keep writing progress onto a job
    // somebody else now owns.
    assert_eq!(
        repo::beat_restore_job(&mut c, job.id, "impostor", 999)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        repo::beat_restore_job(&mut c, job.id, "owner", 42)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        repo::get_restore_job(&mut c, job.id)
            .await
            .unwrap()
            .unwrap()
            .rows_restored,
        42
    );

    assert_eq!(
        repo::finish_restore_job(&mut c, job.id, "impostor", "failed", 0, "nope")
            .await
            .unwrap(),
        0
    );
    repo::finish_restore_job(&mut c, job.id, "owner", "succeeded", 42, "")
        .await
        .unwrap();
    let done = repo::get_restore_job(&mut c, job.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(done.status, "succeeded");
    assert!(done.finished_at.is_some());
    assert_eq!(done.error, "");

    db.cleanup().await;
}

#[tokio::test]
async fn the_pin_is_recorded_on_the_job_and_survives_the_pin_being_removed() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut c = db.conn().await;
    let base = Utc::now() - Duration::days(60);
    let (start, end) = (base, base + Duration::days(1));

    let job = repo::create_restore_job(
        &mut c,
        "error_events",
        None,
        start,
        end,
        Utc::now() + Duration::days(30),
        None,
    )
    .await
    .unwrap();
    let pin = repo::create_tier_pin(
        &mut c,
        "error_events",
        start,
        end,
        Utc::now() - Duration::minutes(1),
        None,
        None,
    )
    .await
    .unwrap();
    repo::set_restore_job_pin(&mut c, job.id, pin.id)
        .await
        .unwrap();
    repo::set_restore_job_estimate(&mut c, job.id, 1234)
        .await
        .unwrap();

    repo::expire_tier_pins(&mut c).await.unwrap();

    // ON DELETE SET NULL: the history of the restore outlives the restored data,
    // and `pin_expires_at` is still there to say how long it was meant to live.
    let after = repo::get_restore_job(&mut c, job.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.pin_id, None);
    assert_eq!(after.rows_estimated, 1234);
    assert!(after.pin_expires_at > start);

    db.cleanup().await;
}
