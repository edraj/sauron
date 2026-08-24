//! The restore-repair fix (Task 11, revised): `restore_to_postgres` pulls cold
//! Parquet rows back into Postgres still carrying the guest's anonymous id —
//! Parquet is immutable, so a merge's `rewrite_hot_rows` (which only ever
//! `UPDATE`s LIVE Postgres rows) could never have reached them. Left alone,
//! `count(DISTINCT distinct_id)` counts the guest and the person as two
//! people again, in EVERY reader that runs that aggregate — not just one.
//!
//! `repo::repair_restored_rows` is the fix: repair the row at the source,
//! once, right after the restore writes it. This file's headline test drives
//! `active_user_series` — deliberately NOT `active_users_by_day_hot` — because
//! that is one of the five readers a narrower, since-abandoned read-time
//! overlay could not have fixed (see `repair_restored_rows`'s own doc comment
//! for why that approach was reverted). Proving the fix through a reader the
//! overlay never touched is what proves the wider claim: the row itself is
//! correct now, so EVERY reader is correct, with no per-reader special case.
//!
//! Skips (does not fail) when `TEST_DATABASE_URL` is unset — see `common/mod.rs`.

mod common;

use chrono::{Duration, Utc};
use common::{seed_signal_event, TestDb};
use diesel::sql_types::{BigInt, Nullable, Text, Timestamptz, Uuid as SqlUuid};
use diesel_async::RunQueryDsl;
use sauron_db::repo;
use sauron_db::scope::{EnvFilter, Range, ReadScope};
use uuid::Uuid;

/// `user_stats`/`active_user_series` are both open-ended forward from `since`
/// — there is no `to` bound, unlike `active_users_by_day_hot`. `seed_two_envs`
/// anchors ITS OWN baseline fixture rows to `Utc::now()`, so any window that
/// starts at or before "now" picks up that unrelated fixture data too (caught
/// by an earlier version of this file's headline test: expected 2, got 9).
/// Anchoring 400 days into the future — safely past anything `seed_two_envs`
/// seeds — makes this test's window hermetic without needing a `to` bound
/// these readers don't have. `d`/`hour` preserve the same relative-ordering
/// role `active_users.rs`'s own `day()` helper uses.
///
/// ## `hour` is anchored to midnight, not added to the current time
///
/// The obvious spelling — `Utc::now() + days + hours` — carries the wall
/// clock's own time-of-day into every fixture timestamp, which makes `hour`
/// mean "this many hours after whenever the suite happens to run" rather than
/// "this hour of that day". `active_user_series` buckets by UTC DAY, so two
/// fixture rows at `hour = 9` and `hour = 11` land on the same day only while
/// `now_hour + 11 < 24`.
///
/// That is a real, measured failure and not a hypothetical: from 13:00 UTC,
/// `restoring_a_merged_guest_then_repairing_collapses_active_user_series_to_one`
/// put its two rows on consecutive UTC days, so the post-repair sum came out
/// `1 + 1 = 2` and the test failed — while `before == 2` still passed, for the
/// wrong reason (two people on one day, versus one person on each of two
/// days). A ~11-hour-per-day green window that silently inverts what the
/// assertion proves.
///
/// Anchoring to `date_naive().and_hms_opt(hour, 0, 0)` makes `hour` an
/// absolute time-of-day on a fixed future date, so relative ordering is
/// preserved exactly and no offset can cross midnight. Same defect class as
/// the `person_env_rollup` time-of-day flakes.
fn day(d: u32, hour: u32) -> chrono::DateTime<Utc> {
    (Utc::now() + Duration::days(400 + i64::from(d)))
        .date_naive()
        .and_hms_opt(hour, 0, 0)
        .expect("hour is always a valid time-of-day")
        .and_utc()
}

fn app_scope(app_id: Uuid) -> ReadScope {
    ReadScope {
        app_id,
        env: EnvFilter::All,
    }
}

/// Stamp `restored_pin_id` on `analytics_events` rows, standing in for what
/// `DuckEngine::restore_to_postgres` writes — the same substitution
/// `cold_restore.rs`'s `mark_as_restored` makes and documents for
/// `error_events`. A restored row is byte-for-byte the same shape whether it
/// arrived via a real Parquet round trip or was seeded directly, and
/// `sauron-db` deliberately carries no dependency on `sauron-tier`/DuckDB —
/// the real copy is `sauron-tier/tests/restore_roundtrip.rs`'s job.
async fn mark_analytics_as_restored(
    conn: &mut sauron_db::PgConn,
    pin_id: Uuid,
    from: chrono::DateTime<Utc>,
    to: chrono::DateTime<Utc>,
) -> i64 {
    #[derive(diesel::QueryableByName)]
    struct N {
        #[diesel(sql_type = BigInt)]
        n: i64,
    }
    let r: N = diesel::sql_query(
        "WITH u AS ( \
           UPDATE analytics_events SET restored_pin_id = $1 \
            WHERE occurred_at >= $2 AND occurred_at < $3 AND restored_pin_id IS NULL \
           RETURNING 1) \
         SELECT count(*)::bigint AS n FROM u",
    )
    .bind::<SqlUuid, _>(pin_id)
    .bind::<Timestamptz, _>(from)
    .bind::<Timestamptz, _>(to)
    .get_result(conn)
    .await
    .expect("mark restored");
    r.n
}

/// A minimal `identity_merges` row, seeded directly so a test can control
/// `state` independently of the real claim/enqueue/drain machinery (already
/// covered by `identity_merge.rs`).
async fn seed_merge(
    conn: &mut sauron_db::PgConn,
    app_id: Uuid,
    alias: &str,
    person: &str,
    state: &str,
) {
    diesel::sql_query(
        "INSERT INTO identity_merges (app_id, alias_id, distinct_id, state) VALUES ($1, $2, $3, $4)",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Text, _>(alias)
    .bind::<Text, _>(person)
    .bind::<Text, _>(state)
    .execute(conn)
    .await
    .expect("seed merge row");
}

async fn analytics_distinct_id(conn: &mut sauron_db::PgConn, id: Uuid) -> (String, Option<String>) {
    #[derive(diesel::QueryableByName)]
    struct Row {
        #[diesel(sql_type = Text)]
        distinct_id: String,
        #[diesel(sql_type = Nullable<Text>)]
        guest_alias: Option<String>,
    }
    let r: Row =
        diesel::sql_query("SELECT distinct_id, guest_alias FROM analytics_events WHERE id = $1")
            .bind::<SqlUuid, _>(id)
            .get_result(conn)
            .await
            .expect("read row");
    (r.distinct_id, r.guest_alias)
}

/// THE test that matters: restore a range containing a merged guest's rows,
/// run the repair, and the count collapses from 2 to 1 — proven through
/// `active_user_series`, a reader the abandoned overlay never touched.
#[tokio::test]
async fn restoring_a_merged_guest_then_repairing_collapses_active_user_series_to_one() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut c = db.conn().await;

    let (start, end) = (day(1, 0), day(20, 0));

    // The guest's row, exactly as it would look after `restore_to_postgres`
    // copied it back — still the anonymous id.
    seed_signal_event(&mut c, ids.app_id, None, "anon_x", day(10, 9)).await;
    // The identified person's own, unrelated activity, same day.
    seed_signal_event(&mut c, ids.app_id, None, "u-42", day(10, 11)).await;

    let pin = repo::create_tier_pin(
        &mut c,
        "analytics_events",
        start,
        end,
        Utc::now() + Duration::days(30),
        None,
        Some("restore"),
    )
    .await
    .unwrap();
    assert_eq!(
        mark_analytics_as_restored(&mut c, pin.id, start, end).await,
        2,
        "both rows land in the restored range"
    );
    seed_merge(&mut c, ids.app_id, "anon_x", "u-42", "done").await;

    let before = repo::active_user_series(&mut c, app_scope(ids.app_id), Range::since(day(10, 0)))
        .await
        .unwrap();
    assert_eq!(
        before.iter().map(|p| p.active).sum::<i64>(),
        2,
        "unrepaired: anon_x and u-42 still count as two different people"
    );

    let repaired = repo::repair_restored_rows(&mut c, "analytics_events", pin.id, start, end)
        .await
        .unwrap();
    assert_eq!(
        repaired, 1,
        "exactly the one guest row, not the person's own row"
    );

    let after = repo::active_user_series(&mut c, app_scope(ids.app_id), Range::since(day(10, 0)))
        .await
        .unwrap();
    assert_eq!(
        after.iter().map(|p| p.active).sum::<i64>(),
        1,
        "repaired at the source: anon_x and u-42 are now the same row/person, \
         proven through a reader the abandoned overlay never fixed"
    );

    db.cleanup().await;
}

/// Idempotence: a second run of the repair matches nothing, because no row
/// still has `distinct_id = alias` — the same property `rewrite_hot_rows`
/// documents and relies on for the identical reason.
#[tokio::test]
async fn the_repair_is_idempotent() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut c = db.conn().await;

    let (start, end) = (day(1, 0), day(20, 0));
    seed_signal_event(&mut c, ids.app_id, None, "anon_y", day(10, 9)).await;

    let pin = repo::create_tier_pin(
        &mut c,
        "analytics_events",
        start,
        end,
        Utc::now() + Duration::days(30),
        None,
        Some("restore"),
    )
    .await
    .unwrap();
    mark_analytics_as_restored(&mut c, pin.id, start, end).await;
    seed_merge(&mut c, ids.app_id, "anon_y", "u-77", "done").await;

    let first = repo::repair_restored_rows(&mut c, "analytics_events", pin.id, start, end)
        .await
        .unwrap();
    assert_eq!(first, 1);

    let second = repo::repair_restored_rows(&mut c, "analytics_events", pin.id, start, end)
        .await
        .unwrap();
    assert_eq!(
        second, 0,
        "the row now holds the PERSON id, which can never itself become an \
         alias (identity_merge.rs's a_target_cannot_become_an_alias_and_vice_versa), \
         so nothing matches on the second pass"
    );

    // A third run, for good measure — idempotence must hold forever, not just
    // "once more".
    let third = repo::repair_restored_rows(&mut c, "analytics_events", pin.id, start, end)
        .await
        .unwrap();
    assert_eq!(third, 0);

    db.cleanup().await;
}

/// No `state` filter: a merge still `pending` is resolved exactly the same as
/// a `done` one. Deliberate — `rewrite_hot_rows`'s own sweep is unbounded by
/// time and will find nothing left once this has already run, so resolving
/// eagerly here is safe regardless of where the merge is in its lifecycle.
#[tokio::test]
async fn the_repair_resolves_a_still_pending_merge() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut c = db.conn().await;

    let (start, end) = (day(1, 0), day(20, 0));
    seed_signal_event(&mut c, ids.app_id, None, "anon_z", day(10, 9)).await;

    let pin = repo::create_tier_pin(
        &mut c,
        "analytics_events",
        start,
        end,
        Utc::now() + Duration::days(30),
        None,
        Some("restore"),
    )
    .await
    .unwrap();
    mark_analytics_as_restored(&mut c, pin.id, start, end).await;
    // Deliberately 'pending', not 'done'.
    seed_merge(&mut c, ids.app_id, "anon_z", "u-88", "pending").await;

    let repaired = repo::repair_restored_rows(&mut c, "analytics_events", pin.id, start, end)
        .await
        .unwrap();
    assert_eq!(
        repaired, 1,
        "an in-flight merge must still be resolved, not skipped"
    );

    db.cleanup().await;
}

/// Column coverage: `analytics_events`/`error_events` set `guest_alias` to the
/// row's own pre-update `distinct_id` (matching `rewrite_hot_rows`'s shape);
/// `transactions` has no `guest_alias` column, so only `distinct_id` moves.
#[tokio::test]
async fn the_repair_sets_guest_alias_on_analytics_events() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut c = db.conn().await;

    let (start, end) = (day(1, 0), day(20, 0));
    let row_id = Uuid::new_v4();
    diesel::sql_query(
        "INSERT INTO analytics_events (id, app_id, name, distinct_id, occurred_at, received_at) \
         VALUES ($1, $2, 'signal', 'anon_g', $3, now())",
    )
    .bind::<SqlUuid, _>(row_id)
    .bind::<SqlUuid, _>(ids.app_id)
    .bind::<Timestamptz, _>(day(10, 9))
    .execute(&mut c)
    .await
    .expect("seed row with a known id");

    let pin = repo::create_tier_pin(
        &mut c,
        "analytics_events",
        start,
        end,
        Utc::now() + Duration::days(30),
        None,
        Some("restore"),
    )
    .await
    .unwrap();
    mark_analytics_as_restored(&mut c, pin.id, start, end).await;
    seed_merge(&mut c, ids.app_id, "anon_g", "u-guest-alias", "done").await;

    let (before_id, before_alias) = analytics_distinct_id(&mut c, row_id).await;
    assert_eq!(before_id, "anon_g");
    assert_eq!(before_alias, None);

    repo::repair_restored_rows(&mut c, "analytics_events", pin.id, start, end)
        .await
        .unwrap();

    let (after_id, after_alias) = analytics_distinct_id(&mut c, row_id).await;
    assert_eq!(after_id, "u-guest-alias", "distinct_id now the person");
    assert_eq!(
        after_alias,
        Some("anon_g".to_string()),
        "guest_alias preserves the original anonymous id, same as rewrite_hot_rows"
    );

    db.cleanup().await;
}

/// `transactions` carries `restored_pin_id` but no `guest_alias` column at
/// all — the repair must still resolve `distinct_id` there without trying to
/// touch a column that does not exist.
#[tokio::test]
async fn the_repair_covers_transactions_which_has_no_guest_alias_column() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut c = db.conn().await;

    let (start, end) = (day(1, 0), day(20, 0));
    let row_id = Uuid::new_v4();
    diesel::sql_query(
        "INSERT INTO transactions \
           (id, app_id, name, op, duration_ms, distinct_id, occurred_at, received_at) \
         VALUES ($1, $2, 'checkout', 'http.server', 12.5, 'anon_t', $3, now())",
    )
    .bind::<SqlUuid, _>(row_id)
    .bind::<SqlUuid, _>(ids.app_id)
    .bind::<Timestamptz, _>(day(10, 9))
    .execute(&mut c)
    .await
    .expect("seed transaction row");

    let pin = repo::create_tier_pin(
        &mut c,
        "transactions",
        start,
        end,
        Utc::now() + Duration::days(30),
        None,
        Some("restore"),
    )
    .await
    .unwrap();
    #[derive(diesel::QueryableByName)]
    struct N {
        #[diesel(sql_type = BigInt)]
        n: i64,
    }
    let marked: N = diesel::sql_query(
        "WITH u AS ( \
           UPDATE transactions SET restored_pin_id = $1 \
            WHERE occurred_at >= $2 AND occurred_at < $3 AND restored_pin_id IS NULL \
           RETURNING 1) \
         SELECT count(*)::bigint AS n FROM u",
    )
    .bind::<SqlUuid, _>(pin.id)
    .bind::<Timestamptz, _>(start)
    .bind::<Timestamptz, _>(end)
    .get_result(&mut c)
    .await
    .expect("mark restored");
    assert_eq!(marked.n, 1);
    seed_merge(&mut c, ids.app_id, "anon_t", "u-txn-target", "done").await;

    let repaired = repo::repair_restored_rows(&mut c, "transactions", pin.id, start, end)
        .await
        .unwrap();
    assert_eq!(repaired, 1);

    #[derive(diesel::QueryableByName)]
    struct Row {
        #[diesel(sql_type = Nullable<Text>)]
        distinct_id: Option<String>,
    }
    let r: Row = diesel::sql_query("SELECT distinct_id FROM transactions WHERE id = $1")
        .bind::<SqlUuid, _>(row_id)
        .get_result(&mut c)
        .await
        .expect("read row");
    assert_eq!(r.distinct_id, Some("u-txn-target".to_string()));

    db.cleanup().await;
}

/// The repair touches signal tables only. `event_users` AND
/// `event_user_environments` were already folded when the merge ran
/// (`fold_rollups`); re-touching either here would double-count on top of a
/// fold that already happened.
///
/// Asserts COUNTERS, not just row presence: a check that only looked at
/// `event_users.distinct_id` (an earlier version of this test) would still
/// pass an implementation that incremented `event_user_environments`
/// counters without moving any row — the exact re-fold this repair must
/// never perform. `events_count`/`errors_count`/`sessions_count` are read
/// back and compared byte-for-byte, on both tables.
#[tokio::test]
async fn the_repair_does_not_touch_rollups() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut c = db.conn().await;

    let (start, end) = (day(1, 0), day(20, 0));
    seed_signal_event(&mut c, ids.app_id, None, "anon_r", day(10, 9)).await;
    // Rollup rows for the ALIAS, as `fold_rollups` would have left behind
    // BEFORE the merge folded it (or, in a cold-stale case, one that was
    // never touched because the alias's activity was already cold at merge
    // time) — either way, the repair must not move, delete, or increment
    // them.
    repo::touch_event_user(&mut c, ids.app_id, "anon_r")
        .await
        .unwrap();
    repo::bump_person_env(&mut c, ids.app_id, "anon_r", None, day(10, 9), 5, 2, 1)
        .await
        .unwrap();

    let pin = repo::create_tier_pin(
        &mut c,
        "analytics_events",
        start,
        end,
        Utc::now() + Duration::days(30),
        None,
        Some("restore"),
    )
    .await
    .unwrap();
    mark_analytics_as_restored(&mut c, pin.id, start, end).await;
    seed_merge(&mut c, ids.app_id, "anon_r", "u-rollup-target", "done").await;

    #[derive(diesel::QueryableByName, Debug, PartialEq)]
    struct EventUserRow {
        #[diesel(sql_type = Text)]
        distinct_id: String,
    }
    #[derive(diesel::QueryableByName, Debug, PartialEq)]
    struct EnvRollupRow {
        #[diesel(sql_type = Text)]
        distinct_id: String,
        #[diesel(sql_type = BigInt)]
        events_count: i64,
        #[diesel(sql_type = BigInt)]
        errors_count: i64,
        #[diesel(sql_type = BigInt)]
        sessions_count: i64,
    }
    let users_q = "SELECT distinct_id FROM event_users WHERE app_id = $1 ORDER BY distinct_id";
    let envs_q = "SELECT distinct_id, events_count, errors_count, sessions_count \
                  FROM event_user_environments WHERE app_id = $1 ORDER BY distinct_id";

    let users_before: Vec<EventUserRow> = diesel::sql_query(users_q)
        .bind::<SqlUuid, _>(ids.app_id)
        .get_results(&mut c)
        .await
        .unwrap();
    let envs_before: Vec<EnvRollupRow> = diesel::sql_query(envs_q)
        .bind::<SqlUuid, _>(ids.app_id)
        .get_results(&mut c)
        .await
        .unwrap();

    repo::repair_restored_rows(&mut c, "analytics_events", pin.id, start, end)
        .await
        .unwrap();

    let users_after: Vec<EventUserRow> = diesel::sql_query(users_q)
        .bind::<SqlUuid, _>(ids.app_id)
        .get_results(&mut c)
        .await
        .unwrap();
    let envs_after: Vec<EnvRollupRow> = diesel::sql_query(envs_q)
        .bind::<SqlUuid, _>(ids.app_id)
        .get_results(&mut c)
        .await
        .unwrap();

    assert_eq!(
        users_before, users_after,
        "event_users must be byte-for-byte unchanged by the repair"
    );
    assert_eq!(
        envs_before, envs_after,
        "event_user_environments counters must be byte-for-byte unchanged — a \
         re-fold that only incremented counters without moving rows would \
         still fail this specific assertion even though it passes the \
         row-presence check alone"
    );
    assert!(
        users_after.iter().any(|r| r.distinct_id == "anon_r"),
        "the alias's rollup row specifically must still be there, untouched"
    );
    assert!(
        envs_after
            .iter()
            .any(|r| r.distinct_id == "anon_r" && r.events_count == 5),
        "the alias's counters specifically must still be there, unchanged"
    );

    db.cleanup().await;
}

/// Guards against a chain — rare but real (see `repair_restored_rows`'s doc
/// comment for how a Persons purge makes one representable) — being
/// half-resolved instead of skipped. Without the `NOT EXISTS` guard, a first
/// run would advance `chain_x` to `chain_y` (not yet the eventual
/// `chain_z`), and idempotence would break on the very next run. Seeded
/// directly via SQL, bypassing `claim_identity` (which refuses this shape),
/// to exercise the guard in isolation — the same technique
/// `identity_merge_cold.rs`'s `a_row_whose_person_is_itself_an_alias_is_excluded`
/// uses for `cold_alias_map`'s identical guard.
#[tokio::test]
async fn a_chained_alias_is_skipped_not_half_resolved() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut c = db.conn().await;

    let (start, end) = (day(1, 0), day(20, 0));
    seed_signal_event(&mut c, ids.app_id, None, "chain_x", day(10, 9)).await;

    let pin = repo::create_tier_pin(
        &mut c,
        "analytics_events",
        start,
        end,
        Utc::now() + Duration::days(30),
        None,
        Some("restore"),
    )
    .await
    .unwrap();
    mark_analytics_as_restored(&mut c, pin.id, start, end).await;

    // chain_x -> chain_y -> chain_z, exactly the shape a Persons purge
    // (which empties `identities` but leaves `identity_merges` alone) makes
    // representable: chain_y is claimed as BOTH an alias's target and, later,
    // another claim's alias.
    seed_merge(&mut c, ids.app_id, "chain_x", "chain_y", "pending").await;
    seed_merge(&mut c, ids.app_id, "chain_y", "chain_z", "pending").await;

    for attempt in 1..=2 {
        let repaired = repo::repair_restored_rows(&mut c, "analytics_events", pin.id, start, end)
            .await
            .unwrap();
        assert_eq!(
            repaired, 0,
            "attempt {attempt}: a chained alias must be skipped, not \
             half-resolved to the intermediate id"
        );
    }

    #[derive(diesel::QueryableByName)]
    struct Row {
        #[diesel(sql_type = Text)]
        distinct_id: String,
    }
    let row_id_query: Vec<Row> = diesel::sql_query(
        "SELECT distinct_id FROM analytics_events WHERE app_id = $1 AND restored_pin_id = $2",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .bind::<SqlUuid, _>(pin.id)
    .get_results(&mut c)
    .await
    .unwrap();
    assert_eq!(
        row_id_query.len(),
        1,
        "sanity: exactly one restored row for this pin"
    );
    assert_eq!(
        row_id_query[0].distinct_id, "chain_x",
        "still the original alias — never advanced to chain_y or chain_z"
    );

    db.cleanup().await;
}

/// Break/restore verification for the chain guard: temporarily strip the
/// `NOT EXISTS` clause (simulated here by calling the guard's own SQL shape
/// with it removed) and confirm the chain WOULD be half-resolved without it,
/// then confirm the shipped function (with the guard) leaves it alone. This
/// is what makes `a_chained_alias_is_skipped_not_half_resolved` a real
/// regression guard rather than a test that could not fail.
#[tokio::test]
async fn break_restore_verify_the_chain_guard_is_load_bearing() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut c = db.conn().await;

    let (start, end) = (day(1, 0), day(20, 0));
    seed_signal_event(&mut c, ids.app_id, None, "chain_x2", day(10, 9)).await;

    let pin = repo::create_tier_pin(
        &mut c,
        "analytics_events",
        start,
        end,
        Utc::now() + Duration::days(30),
        None,
        Some("restore"),
    )
    .await
    .unwrap();
    mark_analytics_as_restored(&mut c, pin.id, start, end).await;
    seed_merge(&mut c, ids.app_id, "chain_x2", "chain_y2", "pending").await;
    seed_merge(&mut c, ids.app_id, "chain_y2", "chain_z2", "pending").await;

    // BREAK: the pre-guard shape, inline, over the SAME rows.
    #[derive(diesel::QueryableByName)]
    struct N {
        #[diesel(sql_type = BigInt)]
        n: i64,
    }
    let broken: N = diesel::sql_query(
        "WITH u AS ( \
           UPDATE analytics_events e SET distinct_id = m.distinct_id, guest_alias = e.distinct_id \
             FROM identity_merges m \
            WHERE e.restored_pin_id = $1 \
              AND e.occurred_at >= $2 AND e.occurred_at < $3 \
              AND m.app_id = e.app_id \
              AND m.alias_id = e.distinct_id \
           RETURNING 1) \
         SELECT count(*)::bigint AS n FROM u",
    )
    .bind::<SqlUuid, _>(pin.id)
    .bind::<Timestamptz, _>(start)
    .bind::<Timestamptz, _>(end)
    .get_result(&mut c)
    .await
    .unwrap();
    assert_eq!(
        broken.n, 1,
        "without the guard the chain IS half-resolved — confirms the guard \
         is load-bearing, not a no-op"
    );

    #[derive(diesel::QueryableByName)]
    struct Row {
        #[diesel(sql_type = Text)]
        distinct_id: String,
    }
    let after_break: Row = diesel::sql_query(
        "SELECT distinct_id FROM analytics_events WHERE app_id = $1 AND restored_pin_id = $2",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .bind::<SqlUuid, _>(pin.id)
    .get_result(&mut c)
    .await
    .unwrap();
    assert_eq!(
        after_break.distinct_id, "chain_y2",
        "the broken shape advances the row to the INTERMEDIATE id, not the \
         true target chain_z2 — this is the half-resolution the guard exists \
         to prevent"
    );

    // RESTORE: reset the row and re-run through the real, guarded function.
    diesel::sql_query(
        "UPDATE analytics_events SET distinct_id = 'chain_x2', guest_alias = NULL \
          WHERE app_id = $1 AND restored_pin_id = $2",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .bind::<SqlUuid, _>(pin.id)
    .execute(&mut c)
    .await
    .unwrap();

    let repaired = repo::repair_restored_rows(&mut c, "analytics_events", pin.id, start, end)
        .await
        .unwrap();
    assert_eq!(repaired, 0, "the real, guarded function skips the chain");

    let after_restore: Row = diesel::sql_query(
        "SELECT distinct_id FROM analytics_events WHERE app_id = $1 AND restored_pin_id = $2",
    )
    .bind::<SqlUuid, _>(ids.app_id)
    .bind::<SqlUuid, _>(pin.id)
    .get_result(&mut c)
    .await
    .unwrap();
    assert_eq!(after_restore.distinct_id, "chain_x2");

    db.cleanup().await;
}

/// Blast-radius containment — the single property whose absence killed the
/// overlay approach, and the one no earlier test in this file actually
/// proved: every seeded row up to here was inside the pin's own scope, so
/// deleting `e.restored_pin_id = $1` or the `occurred_at` bounds would have
/// changed no assertion. Seeds FOUR rows in the same table for the same
/// app, only one of which the repair may touch:
///   - in scope: this pin, in range        -> repaired
///   - a DIFFERENT pin, same range          -> untouched
///   - never restored (`restored_pin_id` NULL), same range -> untouched
///   - this pin, but occurred_at OUTSIDE [start, end)       -> untouched
#[tokio::test]
async fn the_repair_never_touches_a_row_outside_its_own_pin_or_range() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut c = db.conn().await;

    let (start, end) = (day(1, 0), day(20, 0));

    let in_scope = Uuid::new_v4();
    let other_pin_row = Uuid::new_v4();
    let never_restored = Uuid::new_v4();
    let out_of_range = Uuid::new_v4();

    async fn insert_row(
        c: &mut sauron_db::PgConn,
        id: Uuid,
        app_id: Uuid,
        distinct_id: &str,
        occurred_at: chrono::DateTime<Utc>,
    ) {
        diesel::sql_query(
            "INSERT INTO analytics_events (id, app_id, name, distinct_id, occurred_at, received_at) \
             VALUES ($1, $2, 'signal', $3, $4, now())",
        )
        .bind::<SqlUuid, _>(id)
        .bind::<SqlUuid, _>(app_id)
        .bind::<Text, _>(distinct_id)
        .bind::<Timestamptz, _>(occurred_at)
        .execute(c)
        .await
        .expect("seed row");
    }

    insert_row(&mut c, in_scope, ids.app_id, "anon_scope", day(10, 9)).await;
    insert_row(
        &mut c,
        other_pin_row,
        ids.app_id,
        "anon_other_pin",
        day(10, 9),
    )
    .await;
    insert_row(&mut c, never_restored, ids.app_id, "anon_never", day(10, 9)).await;
    // Inside the pin's TABLE but outside its RANGE — stamped with the same
    // pin id below, then left there, exercising the occurred_at predicate
    // independently of the pin-id predicate.
    insert_row(
        &mut c,
        out_of_range,
        ids.app_id,
        "anon_out_of_range",
        day(25, 9),
    )
    .await;

    let pin = repo::create_tier_pin(
        &mut c,
        "analytics_events",
        start,
        end,
        Utc::now() + Duration::days(30),
        None,
        Some("restore"),
    )
    .await
    .unwrap();
    let other_pin = repo::create_tier_pin(
        &mut c,
        "analytics_events",
        start,
        end,
        Utc::now() + Duration::days(30),
        None,
        Some("a different restore"),
    )
    .await
    .unwrap();

    // Stamp: in_scope + out_of_range get THIS pin; other_pin_row gets a
    // DIFFERENT pin; never_restored is left with restored_pin_id NULL.
    diesel::sql_query("UPDATE analytics_events SET restored_pin_id = $1 WHERE id = $2")
        .bind::<SqlUuid, _>(pin.id)
        .bind::<SqlUuid, _>(in_scope)
        .execute(&mut c)
        .await
        .unwrap();
    diesel::sql_query("UPDATE analytics_events SET restored_pin_id = $1 WHERE id = $2")
        .bind::<SqlUuid, _>(pin.id)
        .bind::<SqlUuid, _>(out_of_range)
        .execute(&mut c)
        .await
        .unwrap();
    diesel::sql_query("UPDATE analytics_events SET restored_pin_id = $1 WHERE id = $2")
        .bind::<SqlUuid, _>(other_pin.id)
        .bind::<SqlUuid, _>(other_pin_row)
        .execute(&mut c)
        .await
        .unwrap();

    // Every one of the four aliases is independently merge-eligible — proves
    // that scope containment, not the absence of a matching merge row, is
    // what protects the other three.
    seed_merge(&mut c, ids.app_id, "anon_scope", "u-target-1", "done").await;
    seed_merge(&mut c, ids.app_id, "anon_other_pin", "u-target-2", "done").await;
    seed_merge(&mut c, ids.app_id, "anon_never", "u-target-3", "done").await;
    seed_merge(
        &mut c,
        ids.app_id,
        "anon_out_of_range",
        "u-target-4",
        "done",
    )
    .await;

    repo::repair_restored_rows(&mut c, "analytics_events", pin.id, start, end)
        .await
        .unwrap();

    let (scope_id, _) = analytics_distinct_id(&mut c, in_scope).await;
    let (other_id, _) = analytics_distinct_id(&mut c, other_pin_row).await;
    let (never_id, _) = analytics_distinct_id(&mut c, never_restored).await;
    let (range_id, _) = analytics_distinct_id(&mut c, out_of_range).await;

    assert_eq!(scope_id, "u-target-1", "the in-scope row IS repaired");
    assert_eq!(
        other_id, "anon_other_pin",
        "a row restored by a DIFFERENT pin must be untouched"
    );
    assert_eq!(
        never_id, "anon_never",
        "a row that was never restored at all (restored_pin_id NULL) must be untouched"
    );
    assert_eq!(
        range_id, "anon_out_of_range",
        "a row stamped with THIS pin but outside [start, end) must be untouched"
    );

    db.cleanup().await;
}

/// `error_events` is the other table that carries `guest_alias` — the
/// `set_clause` branch is `table == "analytics_events" || table ==
/// "error_events"`, and only the `analytics_events` leg was exercised
/// before this test. A typo in the `error_events` string literal would drop
/// it silently into the no-`guest_alias` branch with every other test still
/// green, on the table carrying the most `count(DISTINCT distinct_id)`
/// readers (`issue_aggregate_still_uses_an_index_only_scan` and friends in
/// `identity_merge_perf.rs`).
#[tokio::test]
async fn the_repair_sets_guest_alias_on_error_events() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let ids = db.seed_two_envs().await;
    let mut c = db.conn().await;

    let (start, end) = (day(1, 0), day(20, 0));
    let row_id = Uuid::new_v4();
    diesel::sql_query(
        "INSERT INTO error_events \
           (id, app_id, issue_id, fingerprint, distinct_id, occurred_at, received_at) \
         VALUES ($1, $2, $3, 'repair-test-fp', 'anon_err', $4, now())",
    )
    .bind::<SqlUuid, _>(row_id)
    .bind::<SqlUuid, _>(ids.app_id)
    .bind::<SqlUuid, _>(ids.issue_id)
    .bind::<Timestamptz, _>(day(10, 9))
    .execute(&mut c)
    .await
    .expect("seed error_events row");

    let pin = repo::create_tier_pin(
        &mut c,
        "error_events",
        start,
        end,
        Utc::now() + Duration::days(30),
        None,
        Some("restore"),
    )
    .await
    .unwrap();
    #[derive(diesel::QueryableByName)]
    struct N {
        #[diesel(sql_type = BigInt)]
        n: i64,
    }
    let marked: N = diesel::sql_query(
        "WITH u AS ( \
           UPDATE error_events SET restored_pin_id = $1 \
            WHERE occurred_at >= $2 AND occurred_at < $3 AND restored_pin_id IS NULL \
           RETURNING 1) \
         SELECT count(*)::bigint AS n FROM u",
    )
    .bind::<SqlUuid, _>(pin.id)
    .bind::<Timestamptz, _>(start)
    .bind::<Timestamptz, _>(end)
    .get_result(&mut c)
    .await
    .expect("mark restored");
    assert_eq!(marked.n, 1);
    seed_merge(&mut c, ids.app_id, "anon_err", "u-err-target", "done").await;

    let repaired = repo::repair_restored_rows(&mut c, "error_events", pin.id, start, end)
        .await
        .unwrap();
    assert_eq!(repaired, 1);

    #[derive(diesel::QueryableByName)]
    struct Row {
        #[diesel(sql_type = Text)]
        distinct_id: String,
        #[diesel(sql_type = Nullable<Text>)]
        guest_alias: Option<String>,
    }
    let r: Row =
        diesel::sql_query("SELECT distinct_id, guest_alias FROM error_events WHERE id = $1")
            .bind::<SqlUuid, _>(row_id)
            .get_result(&mut c)
            .await
            .expect("read row");
    assert_eq!(r.distinct_id, "u-err-target");
    assert_eq!(
        r.guest_alias,
        Some("anon_err".to_string()),
        "error_events must take the guest_alias branch, not silently fall \
         into the no-guest_alias shape transactions uses"
    );

    db.cleanup().await;
}

/// The interpolated table name is a security boundary, same as
/// `delete_restored_rows`'s identical guard.
#[tokio::test]
async fn the_repair_refuses_a_table_outside_the_allowlist() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut c = db.conn().await;

    let err = repo::repair_restored_rows(
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

    db.cleanup().await;
}
