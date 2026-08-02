//! The `mail_outbox` repository surface against a real Postgres.
//!
//! Skips (does not fail) when `TEST_DATABASE_URL` is unset — see the module doc
//! on `tests/common/mod.rs`.

mod common;

use chrono::Utc;
use common::TestDb;
use diesel::prelude::*;
use diesel::sql_types::{BigInt, Text, Uuid as SqlUuid};
use diesel_async::RunQueryDsl;
use sauron_db::models::MailOutbox;
use sauron_db::schema::mail_outbox;

/// Reading a row back through `schema.rs` proves the hand-maintained column
/// order matches the migration. `Queryable` decodes positionally, so a column
/// inserted in the wrong place in `schema.rs` binds `body_html` into `status`
/// and every later assertion in this file becomes meaningless.
#[tokio::test]
async fn schema_column_order_matches_the_migration() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;

    diesel::sql_query(
        "INSERT INTO mail_outbox (kind, recipient, recipient_key, subject, body_text, body_html) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind::<Text, _>("password_reset")
    .bind::<Text, _>("Victim@Corp.Test")
    .bind::<Text, _>("victim@corp.test")
    .bind::<Text, _>("Reset your password")
    .bind::<Text, _>("plain body")
    .bind::<Text, _>("<p>html body</p>")
    .execute(&mut conn)
    .await
    .expect("insert");

    let row: MailOutbox = mail_outbox::table
        .select(MailOutbox::as_select())
        .first(&mut conn)
        .await
        .expect("select");

    assert_eq!(row.kind, "password_reset");
    assert_eq!(row.recipient, "Victim@Corp.Test");
    assert_eq!(row.recipient_key, "victim@corp.test");
    assert_eq!(row.subject, "Reset your password");
    assert_eq!(row.body_text, "plain body");
    assert_eq!(row.body_html, "<p>html body</p>");
    assert_eq!(row.status, "pending");
    assert_eq!(row.attempts, 0);
    assert_eq!(row.max_attempts, 8);
    assert!(row.last_error.is_none());
    assert!(row.user_id.is_none());
    assert!(row.sent_at.is_none());
    // The column DEFAULT is one hour. It is a backstop for a hand-written row,
    // never the policy — every enqueue passes its own.
    assert!(row.expires_at > Utc::now());

    drop(conn);
    db.cleanup().await;
}

/// A pending row's body is a live credential. One `warn!(row = ?r, ...)` in a
/// drain loop must not put a working reset URL in the journal.
#[test]
fn debug_redacts_the_body() {
    let row = MailOutbox {
        id: uuid::Uuid::nil(),
        kind: "password_reset".into(),
        recipient: "victim@corp.test".into(),
        recipient_key: "victim@corp.test".into(),
        subject: "Reset your password".into(),
        body_text: "https://sauron.test/#/reset-password?token=SECRETTOKEN".into(),
        body_html: "<a href=\"https://sauron.test/#/reset-password?token=SECRETTOKEN\">x</a>"
            .into(),
        status: "pending".into(),
        attempts: 0,
        max_attempts: 8,
        next_attempt_at: Utc::now(),
        expires_at: Utc::now(),
        last_error: None,
        user_id: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        sent_at: None,
    };
    let printed = format!("{row:?}");
    assert!(printed.contains("<redacted>"), "got: {printed}");
    assert!(!printed.contains("SECRETTOKEN"), "got: {printed}");
    // The fields an operator actually needs must still be there.
    assert!(printed.contains("password_reset"));
    assert!(printed.contains("victim@corp.test"));
}

use sauron_db::models::NewMailOutbox;
use sauron_db::repo;
use uuid::Uuid;

/// A `NewMailOutbox` with every field at a recognisable value.
fn new_row<'a>(kind: &'a str, recipient_key: &'a str) -> NewMailOutbox<'a> {
    NewMailOutbox {
        kind,
        recipient: recipient_key,
        recipient_key,
        subject: "Reset your password",
        body_text: "plain body with a token",
        body_html: "<p>html body with a token</p>",
        user_id: None,
    }
}

async fn status_of(conn: &mut sauron_db::AsyncPgConnection, id: Uuid) -> String {
    #[derive(diesel::QueryableByName)]
    struct S {
        #[diesel(sql_type = Text)]
        status: String,
    }
    let row: S = diesel::sql_query("SELECT status FROM mail_outbox WHERE id = $1")
        .bind::<SqlUuid, _>(id)
        .get_result(conn)
        .await
        .expect("status");
    row.status
}

async fn count_rows(conn: &mut sauron_db::AsyncPgConnection) -> i64 {
    #[derive(diesel::QueryableByName)]
    struct C {
        #[diesel(sql_type = BigInt)]
        n: i64,
    }
    let row: C = diesel::sql_query("SELECT count(*)::bigint AS n FROM mail_outbox")
        .get_result(conn)
        .await
        .expect("count");
    row.n
}

#[tokio::test]
async fn happy_path_enqueue_claim_send_scrubs_the_credential() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;

    let id = repo::enqueue_mail(
        &mut conn,
        new_row("password_reset", "victim@corp.test"),
        3600,
        0,
        true,
    )
    .await
    .expect("enqueue")
    .expect("committed");

    let row: MailOutbox = mail_outbox::table
        .select(MailOutbox::as_select())
        .first(&mut conn)
        .await
        .expect("select");
    assert_eq!(row.status, "pending");
    assert_eq!(row.attempts, 0);
    assert!(row.next_attempt_at <= Utc::now());

    let claimed = repo::claim_due_mail(&mut conn, 1).await.expect("claim");
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].id, id);
    assert_eq!(claimed[0].status, "sending");
    assert_eq!(claimed[0].attempts, 1);
    // The claim returns the body BY VALUE, which is what makes it safe for the
    // hygiene sweep to blank a row a drainer is mid-send on.
    assert_eq!(claimed[0].body_text, "plain body with a token");

    assert_eq!(
        repo::mark_mail_sent(&mut conn, id, 1, false)
            .await
            .expect("mark sent"),
        1
    );

    let row: MailOutbox = mail_outbox::table
        .select(MailOutbox::as_select())
        .first(&mut conn)
        .await
        .expect("select");
    assert_eq!(row.status, "sent");
    assert!(row.sent_at.is_some());
    // The assertion that matters: a delivered row holds no credential.
    assert_eq!(row.body_text, "");
    assert_eq!(row.body_html, "");

    drop(conn);
    db.cleanup().await;
}

#[tokio::test]
async fn a_sink_delivery_is_never_reported_as_sent() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;

    let id = repo::enqueue_mail(
        &mut conn,
        new_row("smtp_test", "op@corp.test"),
        300,
        0,
        true,
    )
    .await
    .expect("enqueue")
    .expect("committed");
    repo::claim_due_mail(&mut conn, 1).await.expect("claim");
    repo::mark_mail_sent(&mut conn, id, 1, true)
        .await
        .expect("mark sink");

    // `status='sent'` is the one observable this whole design offers. A sink row
    // reporting `sent` for mail that was never transmitted makes the single place
    // an operator would look actively lie.
    assert_eq!(status_of(&mut conn, id).await, "sink");

    drop(conn);
    db.cleanup().await;
}

#[tokio::test]
async fn dedup_suppresses_inside_the_window_and_a_failed_row_does_not_block_a_retry() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;

    let first = repo::enqueue_mail(
        &mut conn,
        new_row("password_reset", "victim@corp.test"),
        3600,
        300,
        true,
    )
    .await
    .expect("first")
    .expect("committed");

    let second = repo::enqueue_mail(
        &mut conn,
        new_row("password_reset", "victim@corp.test"),
        3600,
        300,
        true,
    )
    .await
    .expect("second");
    assert!(second.is_none(), "second enqueue was not suppressed");
    assert_eq!(count_rows(&mut conn).await, 1);

    // A different kind to the same mailbox is a different budget.
    let other = repo::enqueue_mail(
        &mut conn,
        new_row("smtp_test", "victim@corp.test"),
        300,
        300,
        true,
    )
    .await
    .expect("other kind");
    assert!(other.is_some());

    // A permanently-failed attempt must not block a genuine retry.
    diesel::sql_query("UPDATE mail_outbox SET status = 'failed' WHERE id = $1")
        .bind::<SqlUuid, _>(first)
        .execute(&mut conn)
        .await
        .expect("force failed");
    let retry = repo::enqueue_mail(
        &mut conn,
        new_row("password_reset", "victim@corp.test"),
        3600,
        300,
        true,
    )
    .await
    .expect("retry");
    assert!(
        retry.is_some(),
        "a failed row suppressed a legitimate retry"
    );

    drop(conn);
    db.cleanup().await;
}

#[tokio::test]
async fn a_discard_costs_the_same_round_trip_and_inserts_nothing() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;

    let discarded = repo::enqueue_mail(
        &mut conn,
        new_row("password_reset", "discard@invalid"),
        3600,
        0,
        false,
    )
    .await
    .expect("discard");
    assert!(discarded.is_none());
    assert_eq!(count_rows(&mut conn).await, 0);

    let committed = repo::enqueue_mail(
        &mut conn,
        new_row("password_reset", "discard@invalid"),
        3600,
        0,
        true,
    )
    .await
    .expect("commit");
    assert!(committed.is_some());
    assert_eq!(count_rows(&mut conn).await, 1);

    drop(conn);
    db.cleanup().await;
}

#[tokio::test]
async fn expiry_comes_from_the_caller_not_from_the_kind() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;

    // Same kind, two token lifetimes an order of magnitude apart — the exact case
    // a per-kind constant would get wrong, scrubbing a live 24-hour admin reset
    // link at the one-hour mark.
    repo::enqueue_mail(
        &mut conn,
        new_row("password_reset", "self@corp.test"),
        3600,
        0,
        true,
    )
    .await
    .expect("self-service")
    .expect("committed");
    repo::enqueue_mail(
        &mut conn,
        new_row("password_reset", "admin@corp.test"),
        86_400,
        0,
        true,
    )
    .await
    .expect("admin-initiated")
    .expect("committed");

    let rows: Vec<MailOutbox> = mail_outbox::table
        .select(MailOutbox::as_select())
        .order(mail_outbox::expires_at.asc())
        .load(&mut conn)
        .await
        .expect("load");
    assert_eq!(rows.len(), 2);
    let gap = (rows[1].expires_at - rows[0].expires_at).num_seconds();
    assert!(
        (82_000..=83_000).contains(&gap),
        "expected roughly 23 hours between the two, got {gap}s"
    );

    drop(conn);
    db.cleanup().await;
}

#[tokio::test]
async fn concurrent_claims_never_hand_the_same_row_to_two_drainers() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    {
        let mut conn = db.conn().await;
        for i in 0..3 {
            let key = format!("victim{i}@corp.test");
            repo::enqueue_mail(&mut conn, new_row("password_reset", &key), 3600, 0, true)
                .await
                .expect("enqueue")
                .expect("committed");
        }
    }

    // Two separate connections, claiming at the same time. The test pool has
    // exactly two slots, so both are held simultaneously by construction.
    let mut a = db.conn().await;
    let mut b = db.conn().await;
    let (ra, rb) = tokio::join!(
        repo::claim_due_mail(&mut a, 2),
        repo::claim_due_mail(&mut b, 2)
    );
    let mut ids: Vec<Uuid> = ra
        .expect("claim a")
        .into_iter()
        .chain(rb.expect("claim b"))
        .map(|r| r.id)
        .collect();
    let total = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), total, "a row was claimed twice");
    assert_eq!(total, 3, "some rows were never claimed");

    drop(a);
    drop(b);
    db.cleanup().await;
}

#[tokio::test]
async fn an_expired_row_is_never_claimed() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;

    let id = repo::enqueue_mail(
        &mut conn,
        new_row("password_reset", "victim@corp.test"),
        3600,
        0,
        true,
    )
    .await
    .expect("enqueue")
    .expect("committed");
    diesel::sql_query(
        "UPDATE mail_outbox SET expires_at = now() - interval '1 minute' WHERE id = $1",
    )
    .bind::<SqlUuid, _>(id)
    .execute(&mut conn)
    .await
    .expect("expire");

    // A body that survived its own deadline could never be delivered, only
    // stolen, so refusing it here is what makes the hygiene sweep's blanking free.
    assert!(repo::claim_due_mail(&mut conn, 10)
        .await
        .expect("claim")
        .is_empty());

    drop(conn);
    db.cleanup().await;
}

#[tokio::test]
async fn a_lost_claim_cannot_be_completed_by_the_zombie_that_lost_it() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;

    let id = repo::enqueue_mail(
        &mut conn,
        new_row("password_reset", "victim@corp.test"),
        3600,
        0,
        true,
    )
    .await
    .expect("enqueue")
    .expect("committed");
    repo::claim_due_mail(&mut conn, 1).await.expect("claim");
    // Simulate the row being reclaimed underneath a slow sender: attempts is now 2.
    diesel::sql_query("UPDATE mail_outbox SET attempts = 2 WHERE id = $1")
        .bind::<SqlUuid, _>(id)
        .execute(&mut conn)
        .await
        .expect("bump attempts");

    // Without the `attempts = $2` fence the zombie would blank the body and mark
    // `sent` a row another drainer is mid-send on.
    assert_eq!(
        repo::mark_mail_sent(&mut conn, id, 1, false)
            .await
            .expect("mark"),
        0
    );
    assert_eq!(status_of(&mut conn, id).await, "sending");
    assert_eq!(
        repo::mark_mail_failed(&mut conn, id, 1, "boom", false)
            .await
            .expect("mark"),
        0
    );
    assert_eq!(status_of(&mut conn, id).await, "sending");

    drop(conn);
    db.cleanup().await;
}

#[tokio::test]
async fn backoff_keeps_the_body_and_giving_up_is_reachable_two_ways() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;

    // First failure: back to pending, ~30s out, body intact.
    let id = repo::enqueue_mail(
        &mut conn,
        new_row("password_reset", "a@corp.test"),
        3600,
        0,
        true,
    )
    .await
    .expect("enqueue")
    .expect("committed");
    repo::claim_due_mail(&mut conn, 1).await.expect("claim");
    assert_eq!(
        repo::mark_mail_failed(&mut conn, id, 1, "connection reset", false)
            .await
            .expect("fail"),
        1
    );
    let row: MailOutbox = mail_outbox::table
        .filter(mail_outbox::id.eq(id))
        .select(MailOutbox::as_select())
        .first(&mut conn)
        .await
        .expect("reload");
    assert_eq!(row.status, "pending");
    assert_eq!(row.last_error.as_deref(), Some("connection reset"));
    let delay = (row.next_attempt_at - Utc::now()).num_seconds();
    assert!((25..=35).contains(&delay), "expected ~30s, got {delay}s");
    // NOT blanked. Blanking on failure is what made a misclassification
    // irreversible; the expiry sweep covers the credential instead, and until
    // then an operator can requeue the row by hand.
    assert_eq!(row.body_text, "plain body with a token");

    // Exhausting max_attempts gives up.
    diesel::sql_query(
        "UPDATE mail_outbox SET status = 'sending', attempts = max_attempts WHERE id = $1",
    )
    .bind::<SqlUuid, _>(id)
    .execute(&mut conn)
    .await
    .expect("exhaust");
    assert_eq!(
        repo::mark_mail_failed(&mut conn, id, 8, "connection reset", false)
            .await
            .expect("fail"),
        1
    );
    assert_eq!(status_of(&mut conn, id).await, "failed");

    // A permanent error gives up on the first attempt without consuming the rest.
    let id2 = repo::enqueue_mail(
        &mut conn,
        new_row("password_reset", "b@corp.test"),
        3600,
        0,
        true,
    )
    .await
    .expect("enqueue")
    .expect("committed");
    repo::claim_due_mail(&mut conn, 1).await.expect("claim");
    assert_eq!(
        repo::mark_mail_failed(&mut conn, id2, 1, "550 no such user", true)
            .await
            .expect("fail"),
        1
    );
    assert_eq!(status_of(&mut conn, id2).await, "failed");

    drop(conn);
    db.cleanup().await;
}

#[tokio::test]
async fn a_heartbeat_moves_updated_at_only_while_the_row_is_sending() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;

    let id = repo::enqueue_mail(
        &mut conn,
        new_row("password_reset", "victim@corp.test"),
        3600,
        0,
        true,
    )
    .await
    .expect("enqueue")
    .expect("committed");
    // Pending: nothing to keep alive.
    assert_eq!(repo::heartbeat_mail(&mut conn, id).await.expect("hb"), 0);

    repo::claim_due_mail(&mut conn, 1).await.expect("claim");
    diesel::sql_query(
        "UPDATE mail_outbox SET updated_at = now() - interval '10 minutes' WHERE id = $1",
    )
    .bind::<SqlUuid, _>(id)
    .execute(&mut conn)
    .await
    .expect("age it");
    assert_eq!(repo::heartbeat_mail(&mut conn, id).await.expect("hb"), 1);

    let row: MailOutbox = mail_outbox::table
        .filter(mail_outbox::id.eq(id))
        .select(MailOutbox::as_select())
        .first(&mut conn)
        .await
        .expect("reload");
    assert!((Utc::now() - row.updated_at).num_seconds() < 5);

    drop(conn);
    db.cleanup().await;
}

#[tokio::test]
async fn a_row_orphaned_mid_send_is_requeued_with_backoff_and_can_still_give_up() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;

    let id = repo::enqueue_mail(
        &mut conn,
        new_row("password_reset", "victim@corp.test"),
        3600,
        0,
        true,
    )
    .await
    .expect("enqueue")
    .expect("committed");
    repo::claim_due_mail(&mut conn, 1).await.expect("claim");
    diesel::sql_query(
        "UPDATE mail_outbox SET updated_at = now() - interval '10 minutes' WHERE id = $1",
    )
    .bind::<SqlUuid, _>(id)
    .execute(&mut conn)
    .await
    .expect("age it");

    assert_eq!(
        repo::requeue_stuck_mail(&mut conn, 300)
            .await
            .expect("requeue"),
        1
    );
    let row: MailOutbox = mail_outbox::table
        .filter(mail_outbox::id.eq(id))
        .select(MailOutbox::as_select())
        .first(&mut conn)
        .await
        .expect("reload");
    assert_eq!(row.status, "pending");
    // Without resetting next_attempt_at, a requeued row is immediately eligible
    // for the very next claim, bypassing the backoff ladder entirely on exactly
    // the path that most needs it.
    assert!(
        row.next_attempt_at > Utc::now(),
        "requeued row bypassed the backoff ladder"
    );

    // A row whose send reliably kills the process must eventually be given up on.
    // The give-up decision otherwise lives only in `mark_mail_failed`, which a
    // process that crashed or was OOM-killed never reaches, so the row would be
    // claimed → orphaned → requeued → claimed, forever.
    diesel::sql_query(
        "UPDATE mail_outbox SET status = 'sending', attempts = max_attempts, \
                updated_at = now() - interval '10 minutes' WHERE id = $1",
    )
    .bind::<SqlUuid, _>(id)
    .execute(&mut conn)
    .await
    .expect("exhaust");
    assert_eq!(
        repo::requeue_stuck_mail(&mut conn, 300)
            .await
            .expect("requeue"),
        1
    );
    assert_eq!(status_of(&mut conn, id).await, "failed");

    // A row that is still being worked on is untouched.
    let fresh = repo::enqueue_mail(
        &mut conn,
        new_row("password_reset", "other@corp.test"),
        3600,
        0,
        true,
    )
    .await
    .expect("enqueue")
    .expect("committed");
    repo::claim_due_mail(&mut conn, 1).await.expect("claim");
    assert_eq!(
        repo::requeue_stuck_mail(&mut conn, 300)
            .await
            .expect("requeue"),
        0
    );
    assert_eq!(status_of(&mut conn, fresh).await, "sending");

    drop(conn);
    db.cleanup().await;
}

#[tokio::test]
async fn expiry_sweep_fails_the_row_and_body_scrubbing_keys_off_the_rows_own_deadline() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;

    // One expired, one with a live 24-hour deadline.
    let expired = repo::enqueue_mail(
        &mut conn,
        new_row("password_reset", "expired@corp.test"),
        3600,
        0,
        true,
    )
    .await
    .expect("enqueue")
    .expect("committed");
    let live = repo::enqueue_mail(
        &mut conn,
        new_row("password_reset", "live@corp.test"),
        86_400,
        0,
        true,
    )
    .await
    .expect("enqueue")
    .expect("committed");
    diesel::sql_query(
        "UPDATE mail_outbox SET expires_at = now() - interval '1 minute' WHERE id = $1",
    )
    .bind::<SqlUuid, _>(expired)
    .execute(&mut conn)
    .await
    .expect("expire");

    assert_eq!(
        repo::blank_expired_mail_bodies(&mut conn)
            .await
            .expect("blank"),
        1
    );
    let rows: Vec<MailOutbox> = mail_outbox::table
        .select(MailOutbox::as_select())
        .order(mail_outbox::expires_at.asc())
        .load(&mut conn)
        .await
        .expect("load");
    assert_eq!(rows[0].id, expired);
    assert_eq!(rows[0].body_text, "");
    assert_eq!(rows[0].body_html, "");
    // Status untouched: blanking is not a state transition.
    assert_eq!(rows[0].status, "pending");
    // THE assertion that catches anyone reintroducing a flat age cutoff and
    // scrubbing a live 24-hour admin reset mail an hour after it was queued.
    assert_eq!(rows[1].id, live);
    assert_eq!(rows[1].body_text, "plain body with a token");

    assert_eq!(repo::expire_stale_mail(&mut conn).await.expect("expire"), 1);
    let row: MailOutbox = mail_outbox::table
        .filter(mail_outbox::id.eq(expired))
        .select(MailOutbox::as_select())
        .first(&mut conn)
        .await
        .expect("reload");
    assert_eq!(row.status, "failed");
    assert_eq!(row.last_error.as_deref(), Some("expired before delivery"));
    assert_eq!(status_of(&mut conn, live).await, "pending");

    drop(conn);
    db.cleanup().await;
}

#[tokio::test]
async fn retention_deletes_only_terminal_rows_and_reports_queue_depth() {
    let Some(db) = TestDb::setup().await else {
        eprintln!("TEST_DATABASE_URL unset — skipping");
        return;
    };
    let mut conn = db.conn().await;

    for (key, status) in [
        ("sent@corp.test", "sent"),
        ("failed@corp.test", "failed"),
        ("sink@corp.test", "sink"),
        ("pending@corp.test", "pending"),
        ("sending@corp.test", "sending"),
    ] {
        let id = repo::enqueue_mail(&mut conn, new_row("smtp_test", key), 3600, 0, true)
            .await
            .expect("enqueue")
            .expect("committed");
        diesel::sql_query("UPDATE mail_outbox SET status = $2 WHERE id = $1")
            .bind::<SqlUuid, _>(id)
            .bind::<Text, _>(status)
            .execute(&mut conn)
            .await
            .expect("force status");
    }

    // Age 0 days, so every terminal row is eligible regardless of clock skew.
    assert_eq!(
        repo::prune_mail_outbox(&mut conn, 0, 5000)
            .await
            .expect("prune"),
        3
    );
    assert_eq!(count_rows(&mut conn).await, 2);
    // A second pass returns 0, which is the loop's termination condition.
    assert_eq!(
        repo::prune_mail_outbox(&mut conn, 0, 5000)
            .await
            .expect("prune"),
        0
    );

    let (pending, oldest) = repo::mail_outbox_depth(&mut conn).await.expect("depth");
    assert_eq!(pending, 1);
    assert!(oldest.is_some());

    diesel::sql_query("DELETE FROM mail_outbox WHERE status = 'pending'")
        .execute(&mut conn)
        .await
        .expect("clear");
    let (pending, oldest) = repo::mail_outbox_depth(&mut conn).await.expect("depth");
    assert_eq!(pending, 0);
    assert!(oldest.is_none(), "an empty queue has no oldest row");

    drop(conn);
    db.cleanup().await;
}
