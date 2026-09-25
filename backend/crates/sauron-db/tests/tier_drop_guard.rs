//! `repo::drop_partition_if_unchanged` — the tier worker's last line of defence
//! against deleting a row that exists nowhere else.
//!
//! The worker verifies, in DuckDB and seconds earlier, that cold Parquet holds
//! every row of a partition. A late row can still land after that check. The
//! drop therefore re-counts the partition AFTER taking the locks that stop any
//! further write, and refuses on any difference. These tests pin the three
//! outcomes on a real Postgres, against a throwaway partitioned table so they
//! need no app/org fixtures and cannot disturb the real tiered tables.

mod common;

use common::TestDb;
use diesel::sql_types::BigInt;
use diesel_async::{RunQueryDsl, SimpleAsyncConnection};
use sauron_db::repo::{self, DropOutcome};
use uuid::Uuid;

#[derive(diesel::QueryableByName)]
struct N {
    #[diesel(sql_type = BigInt)]
    n: i64,
}

async fn scalar(conn: &mut sauron_db::PgConn, sql: &str) -> i64 {
    diesel::sql_query(sql)
        .get_result::<N>(conn)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
        .n
}

/// A partitioned parent with one daily child holding `rows` rows.
async fn probe_table(conn: &mut sauron_db::PgConn, rows: i64) -> (String, String) {
    let parent = format!("tier_drop_probe_{}", Uuid::new_v4().simple());
    let child = format!("{parent}_2026_08_15");
    conn.batch_execute(&format!(
        "CREATE TABLE {parent} (id UUID NOT NULL, occurred_at TIMESTAMPTZ NOT NULL, \
                                PRIMARY KEY (id, occurred_at)) \
           PARTITION BY RANGE (occurred_at); \
         CREATE TABLE {child} PARTITION OF {parent} \
           FOR VALUES FROM ('2026-08-15') TO ('2026-08-16'); \
         INSERT INTO {parent} (id, occurred_at) \
           SELECT gen_random_uuid(), '2026-08-15 12:00+00' FROM generate_series(1, {rows});"
    ))
    .await
    .expect("create probe table");
    (parent, child)
}

async fn is_attached(conn: &mut sauron_db::PgConn, parent: &str, child: &str) -> bool {
    scalar(
        conn,
        &format!(
            "SELECT count(*)::bigint AS n FROM pg_inherits i \
               JOIN pg_class c ON c.oid = i.inhrelid \
               JOIN pg_class p ON p.oid = i.inhparent \
             WHERE p.relname = '{parent}' AND c.relname = '{child}'"
        ),
    )
    .await
        == 1
}

async fn exists(conn: &mut sauron_db::PgConn, rel: &str) -> bool {
    scalar(
        conn,
        &format!("SELECT count(*)::bigint AS n FROM pg_class WHERE relname = '{rel}'"),
    )
    .await
        == 1
}

#[tokio::test]
async fn an_unchanged_partition_is_dropped() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut conn = db.conn().await;
    let (parent, child) = probe_table(&mut conn, 25).await;

    let out = repo::drop_partition_if_unchanged(&mut conn, &parent, &child, 25)
        .await
        .expect("drop");
    assert_eq!(out, DropOutcome::Dropped);
    assert!(
        !exists(&mut conn, &child).await,
        "the child table must be gone"
    );
    assert!(exists(&mut conn, &parent).await, "the parent must survive");

    conn.batch_execute(&format!("DROP TABLE {parent}"))
        .await
        .unwrap();
    db.cleanup().await;
}

/// The race this function exists for: cold was verified at 25 rows, then a
/// late row landed. Dropping now would destroy that row's only copy.
#[tokio::test]
async fn a_partition_that_gained_a_row_after_the_check_is_kept_intact() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut conn = db.conn().await;
    let (parent, child) = probe_table(&mut conn, 25).await;
    conn.batch_execute(&format!(
        "INSERT INTO {parent} (id, occurred_at) VALUES (gen_random_uuid(), '2026-08-15 23:59+00')"
    ))
    .await
    .unwrap();

    let out = repo::drop_partition_if_unchanged(&mut conn, &parent, &child, 25)
        .await
        .expect("drop");
    assert_eq!(out, DropOutcome::Changed { rows: 26 });
    // Rolled back, not half-done: still attached, and every row still reachable
    // through the parent — a detached-but-kept child would hide them from reads.
    assert!(
        is_attached(&mut conn, &parent, &child).await,
        "must be re-attached"
    );
    assert_eq!(
        scalar(
            &mut conn,
            &format!("SELECT count(*)::bigint AS n FROM {parent}")
        )
        .await,
        26
    );

    conn.batch_execute(&format!("DROP TABLE {parent}"))
        .await
        .unwrap();
    db.cleanup().await;
}

/// A long reader on the parent must make the drop give up, not queue: while
/// `DETACH` waits for ACCESS EXCLUSIVE, every later query on the table queues
/// behind it, ingest included.
#[tokio::test]
async fn a_busy_table_is_left_alone_rather_than_waited_on() {
    let Some(db) = TestDb::setup().await else {
        return;
    };
    let mut conn = db.conn().await;
    let (parent, child) = probe_table(&mut conn, 3).await;

    // Hold ACCESS SHARE on the parent in an open transaction, as a long
    // dashboard query would.
    let mut reader = db.extra_conn().await;
    reader
        .batch_execute(&format!("BEGIN; SELECT count(*) FROM {parent};"))
        .await
        .unwrap();

    let started = std::time::Instant::now();
    let out = repo::drop_partition_if_unchanged(&mut conn, &parent, &child, 3)
        .await
        .expect("a lock timeout is an outcome, not an error");
    assert_eq!(out, DropOutcome::LockBusy);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "must give up on its own lock timeout, took {:?}",
        started.elapsed()
    );

    reader.batch_execute("ROLLBACK").await.unwrap();
    assert!(
        is_attached(&mut conn, &parent, &child).await,
        "nothing dropped"
    );

    conn.batch_execute(&format!("DROP TABLE {parent}"))
        .await
        .unwrap();
    db.cleanup().await;
}
