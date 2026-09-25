//! End-to-end Postgres → Parquet → Postgres, against a real Postgres and a real
//! DuckDB.
//!
//! Everything else about restore is unit- or repo-tested. This covers the one
//! part that cannot be: the actual copy. Its correctness depends on details no
//! amount of mocking would exercise —
//!
//!  * the export writes with `PARTITION_BY (app_id, year, month)`, which STRIPS
//!    those columns out of the files and encodes them in the directory path, so
//!    reading back with `hive_partitioning=true` returns `app_id` as VARCHAR and
//!    invents `year`/`month` columns that do not exist in Postgres;
//!  * the live table's column list and the historical file's column list are not
//!    the same set, and must not be assumed to be;
//!  * every value has to survive a round trip through Parquet's type system and
//!    back into the Postgres types, JSONB included.
//!
//! Skips (does not fail) when `TEST_DATABASE_URL` is unset, matching the
//! convention in `sauron-db`'s harness.

use chrono::{DateTime, TimeZone, Utc};
use duckdb::Connection;
use sauron_tier::duck::{DuckEngine, Verdict};
use uuid::Uuid;

/// A maintenance URL to create/drop a throwaway database on.
fn maintenance_url() -> Option<String> {
    std::env::var("TEST_DATABASE_URL").ok()
}

fn t(d: u32, h: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 5, d, h, 0, 0).unwrap()
}

/// Ephemeral database, created and dropped through DuckDB's postgres extension
/// so this test needs no diesel and no second Postgres client.
struct Db {
    admin: Connection,
    name: String,
    url: String,
}

impl Db {
    /// `None` only when `TEST_DATABASE_URL` is unset. Every other failure —
    /// the DuckDB `postgres` extension not installing, the server refusing the
    /// connection, `CREATE DATABASE` failing — PANICS with the reason.
    ///
    /// It used to fold all of those into the same `None` as "unset", so the
    /// test printed "TEST_DATABASE_URL unset — skipping" and passed while the
    /// variable was set and nothing had run. A green line that executed
    /// nothing is worse than a red one.
    fn setup() -> Option<Db> {
        let base = maintenance_url()?;
        let name = format!("sauron_restore_{}", Uuid::new_v4().simple());
        let admin = Connection::open_in_memory().expect("open duckdb");
        admin
            .execute_batch("INSTALL postgres; LOAD postgres;")
            .expect("install/load the DuckDB postgres extension");
        admin
            .execute_batch(&format!("ATTACH '{base}' AS m (TYPE postgres);"))
            .expect("attach TEST_DATABASE_URL");
        // CREATE DATABASE cannot run inside a transaction, and `postgres_execute`
        // opens one by default (DuckDB 1.5 sends `BEGIN TRANSACTION ISOLATION
        // LEVEL REPEATABLE READ` first), so the wrapper is switched off.
        admin
            .execute_batch(&format!(
                "CALL postgres_execute('m', 'CREATE DATABASE {name}', use_transaction := false);"
            ))
            .expect("create the throwaway database");
        let url = swap_db(&base, &name);
        Some(Db { admin, name, url })
    }

    fn cleanup(self) {
        let _ = self.admin.execute_batch(&format!(
            "CALL postgres_execute('m', 'DROP DATABASE IF EXISTS {} WITH (FORCE)', use_transaction := false);",
            self.name
        ));
    }
}

/// Replace the database component of a libpq URL.
fn swap_db(url: &str, db: &str) -> String {
    match url.rfind('/') {
        Some(i) => {
            let (head, tail) = url.split_at(i + 1);
            // Preserve any ?query the original carried.
            match tail.find('?') {
                Some(q) => format!("{head}{db}{}", &tail[q..]),
                None => format!("{head}{db}"),
            }
        }
        None => url.to_string(),
    }
}

/// A table shaped like the real tiered ones: uuid key, app_id, a timestamptz,
/// JSONB, a nullable text, the restore marker, and the stack-pool hash.
///
/// `stacktrace_sha256` and `error_stack_blobs` are here because the export
/// joins them (migration 68's stack pool): an `error_events` export against a
/// table without them fails with "Table with name error_stack_blobs does not
/// exist". Nobody noticed, because `Db::setup` used to fold every setup failure
/// into a silent skip.
const DDL: &str = "CREATE TABLE error_events ( \
     id UUID PRIMARY KEY, \
     app_id UUID NOT NULL, \
     message TEXT NOT NULL, \
     stacktrace JSONB NOT NULL DEFAULT ''[]''::jsonb, \
     stacktrace_sha256 BYTEA, \
     release TEXT, \
     occurred_at TIMESTAMPTZ NOT NULL, \
     restored_pin_id UUID )";
const DDL_BLOBS: &str = "CREATE TABLE error_stack_blobs ( \
     sha256 BYTEA PRIMARY KEY, \
     content JSONB NOT NULL, \
     created_at TIMESTAMPTZ NOT NULL DEFAULT now() )";

fn pg_exec(conn: &Connection, sql: &str) {
    conn.execute_batch(&format!("CALL postgres_execute('pg', '{sql}');"))
        .unwrap_or_else(|e| panic!("pg exec failed: {sql}\n{e}"));
}

fn scalar_i64(conn: &Connection, sql: &str) -> i64 {
    let mut stmt = conn.prepare(sql).expect("prepare");
    stmt.query_row([], |r| r.get(0)).expect("query_row")
}

#[test]
fn cold_parquet_restores_into_postgres_with_the_marker() {
    let Some(db) = Db::setup() else {
        eprintln!("TEST_DATABASE_URL unset — skipping restore round trip");
        return;
    };
    let dir = std::env::temp_dir().join(format!("sauron-restore-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let cold_dir = format!("{}/error_events", dir.display());
    let glob = format!("{cold_dir}/**/*.parquet");
    let app = Uuid::new_v4();
    let other_app = Uuid::new_v4();

    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("INSTALL postgres; LOAD postgres; SET TimeZone='UTC';")
        .unwrap();
    conn.execute_batch(&format!("ATTACH '{}' AS pg (TYPE postgres);", db.url))
        .unwrap();
    pg_exec(&conn, DDL);
    pg_exec(&conn, DDL_BLOBS);

    // Four rows for our app across two days, plus one for a different app that
    // must never be swept up by an app-scoped restore.
    for (d, h) in [(1u32, 10u32), (1, 11), (2, 9), (3, 8)] {
        pg_exec(
            &conn,
            &format!(
                "INSERT INTO error_events (id, app_id, message, stacktrace, release, occurred_at) \
                 VALUES (''{}'', ''{}'', ''boom {d}-{h}'', ''[{{\"f\": 1}}]''::jsonb, ''v1'', ''{}'')",
                Uuid::new_v4(),
                app,
                t(d, h).to_rfc3339()
            ),
        );
    }
    pg_exec(
        &conn,
        &format!(
            "INSERT INTO error_events (id, app_id, message, occurred_at) \
             VALUES (''{}'', ''{}'', ''other app'', ''{}'')",
            Uuid::new_v4(),
            other_app,
            t(1, 12).to_rfc3339()
        ),
    );
    assert_eq!(scalar_i64(&conn, "SELECT count(*) FROM pg.error_events"), 5);

    let eng = DuckEngine::open().unwrap();
    let (from, to) = (t(1, 0), t(4, 0));

    // 1. Export, exactly as sauron-tier does.
    eng.export_from_postgres(&db.url, "error_events", from, to, &cold_dir)
        .expect("export");
    assert_eq!(
        eng.count_range(&glob, from, to).unwrap(),
        5,
        "all 5 in cold"
    );

    // 2. Drop from Postgres, as the tier worker's drop step would.
    pg_exec(&conn, "DELETE FROM error_events");
    assert_eq!(scalar_i64(&conn, "SELECT count(*) FROM pg.error_events"), 0);

    // 3. Restore only OUR app, only the first two days.
    let pin = Uuid::new_v4();
    let (rs, re) = (t(1, 0), t(3, 0));
    assert_eq!(
        eng.count_restorable(&glob, Some(app), rs, re).unwrap(),
        3,
        "2 rows on day 1 + 1 on day 2, and NOT the other app's row in the same window"
    );
    let n = eng
        .restore_to_postgres(&db.url, "error_events", &glob, Some(app), rs, re, pin)
        .expect("restore");
    assert_eq!(n, 3);

    // 4. The rows are back, scoped correctly, and every one carries the marker —
    //    which is what makes the restore exactly reversible.
    assert_eq!(scalar_i64(&conn, "SELECT count(*) FROM pg.error_events"), 3);
    assert_eq!(
        scalar_i64(
            &conn,
            &format!("SELECT count(*) FROM pg.error_events WHERE restored_pin_id = '{pin}'")
        ),
        3,
        "every restored row must be identifiable, or expiry cannot un-do this"
    );
    assert_eq!(
        scalar_i64(
            &conn,
            &format!("SELECT count(*) FROM pg.error_events WHERE app_id = '{other_app}'")
        ),
        0,
        "an app-scoped restore must not drag in another tenant's rows"
    );
    assert_eq!(
        scalar_i64(
            &conn,
            &format!(
                "SELECT count(*) FROM pg.error_events WHERE occurred_at >= '{}'",
                t(3, 0).to_rfc3339()
            )
        ),
        0,
        "half-open range: day 3 is outside [day1, day3)"
    );

    // 5. Values survived the round trip — not just the row count. A restore that
    //    returns the right number of blank rows is worse than none.
    assert_eq!(
        scalar_i64(
            &conn,
            "SELECT count(*) FROM pg.error_events WHERE release = 'v1' AND message LIKE 'boom%'"
        ),
        3,
        "scalar columns preserved"
    );
    assert_eq!(
        scalar_i64(
            &conn,
            "SELECT count(*) FROM pg.error_events WHERE stacktrace::text LIKE '%\"f\"%'",
        ),
        3,
        "JSONB survived Parquet and came back as JSON, not as a quoted string"
    );
    // The hive path columns are NOT inserted. No assertion is needed for this and
    // a `duckdb_columns()` check was removed as vacuous: that view does not list
    // an attached Postgres table's columns, so it returns 0 whether or not the
    // bug exists. The real proof is that the INSERT above succeeded at all —
    // `year`/`month` do not exist in Postgres, so writing them would have failed
    // the whole statement and `n` would never have reached 3.

    std::fs::remove_dir_all(&dir).ok();
    db.cleanup();
}

#[test]
fn restoring_a_range_with_no_cold_data_is_zero_not_an_error() {
    let eng = DuckEngine::open().unwrap();
    let glob = "/nonexistent-sauron-restore/error_events/**/*.parquet";
    // "There was nothing there" is a legitimate answer to a restore request and
    // must not surface as a failed job.
    assert_eq!(
        eng.count_restorable(glob, None, t(1, 0), t(2, 0)).unwrap(),
        0
    );
    assert_eq!(
        eng.restore_to_postgres(
            "postgres://unused",
            "error_events",
            glob,
            None,
            t(1, 0),
            t(2, 0),
            Uuid::new_v4()
        )
        .unwrap(),
        0,
        "must short-circuit before touching Postgres at all"
    );
}

// ---------------------------------------------------------------------------
// Late arrivals: `reconcile_range`
// ---------------------------------------------------------------------------
//
// A client-supplied `occurred_at` routes a late event into a partition that
// was already exported. Before `reconcile_range` nothing ever put such a row
// into Parquet, so the drop guard retained the partition forever — on a
// production host, 44 partitions and a full disk. These run the real export,
// the real append and the real key comparison against Postgres and Parquet.

/// One `error_events` row for `app` at `at`, tagged `message`.
fn insert_error(conn: &Connection, id: Uuid, app: Uuid, message: &str, at: DateTime<Utc>) {
    pg_exec(
        conn,
        &format!(
            "INSERT INTO error_events (id, app_id, message, occurred_at) \
             VALUES (''{id}'', ''{app}'', ''{message}'', ''{}'')",
            at.to_rfc3339()
        ),
    );
}

/// Rows in cold for `glob` whose message is `message`.
fn cold_with_message(conn: &Connection, glob: &str, message: &str) -> i64 {
    scalar_i64(
        conn,
        &format!(
            "SELECT count(*) FROM read_parquet('{glob}', hive_partitioning=true, union_by_name=true) \
             WHERE message = '{message}'"
        ),
    )
}

/// A fresh database with the error_events schema, a DuckDB connection attached
/// to it, and an empty cold directory.
fn late_fixture(db: &Db) -> (Connection, std::path::PathBuf, String, String) {
    let dir = std::env::temp_dir().join(format!("sauron-late-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let cold_dir = format!("{}/error_events", dir.display());
    let glob = format!("{cold_dir}/**/*.parquet");
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("INSTALL postgres; LOAD postgres; SET TimeZone='UTC';")
        .unwrap();
    conn.execute_batch(&format!("ATTACH '{}' AS pg (TYPE postgres);", db.url))
        .unwrap();
    pg_exec(&conn, DDL);
    pg_exec(&conn, DDL_BLOBS);
    (conn, dir, cold_dir, glob)
}

#[test]
fn late_rows_are_appended_once_and_the_range_becomes_droppable() {
    let Some(db) = Db::setup() else {
        eprintln!("TEST_DATABASE_URL unset — skipping late-arrival reconcile");
        return;
    };
    let (conn, dir, cold_dir, glob) = late_fixture(&db);
    let app = Uuid::new_v4();
    let (from, to) = (t(1, 0), t(2, 0));
    for h in [8u32, 9, 10, 11] {
        insert_error(&conn, Uuid::new_v4(), app, "on time", t(1, h));
    }

    let eng = DuckEngine::open().unwrap();
    eng.export_from_postgres(&db.url, "error_events", from, to, &cold_dir)
        .expect("export");

    // Straight after export: nothing missing, droppable at 4 rows.
    let rec = eng
        .reconcile_range(&db.url, "error_events", from, to, &glob, &cold_dir)
        .expect("reconcile");
    assert_eq!(rec.exported, 0);
    assert_eq!(rec.verdict, Verdict::Ready { pg_rows: 4 });

    // Two late rows land in the exported day. One carries a POOLED stack trace
    // (placeholder inline, real trace in `error_stack_blobs`), so the append is
    // held to the same shaping as the export: the trace must be materialized.
    insert_error(&conn, Uuid::new_v4(), app, "late", t(1, 20));
    pg_exec(
        &conn,
        "INSERT INTO error_stack_blobs (sha256, content) \
         VALUES (''\\x01'', ''[{\"function\": \"pooled_frame\"}]''::jsonb)",
    );
    pg_exec(
        &conn,
        &format!(
            "INSERT INTO error_events (id, app_id, message, stacktrace, stacktrace_sha256, occurred_at) \
             VALUES (''{}'', ''{app}'', ''late'', ''[]''::jsonb, ''\\x01'', ''{}'')",
            Uuid::new_v4(),
            t(1, 21).to_rfc3339()
        ),
    );
    // And one for the NEXT day, outside the range: it must not be appended.
    insert_error(&conn, Uuid::new_v4(), app, "late", t(2, 1));

    let rec = eng
        .reconcile_range(&db.url, "error_events", from, to, &glob, &cold_dir)
        .expect("reconcile");
    assert_eq!(
        rec.exported, 2,
        "exactly the two late rows in range: {rec:?}"
    );
    assert_eq!(rec.missing, 0);
    assert_eq!(rec.verdict, Verdict::Ready { pg_rows: 6 });
    assert_eq!(eng.count_range(&glob, from, to).unwrap(), 6);
    assert_eq!(cold_with_message(&conn, &glob, "late"), 2);
    assert_eq!(
        cold_with_message(&conn, &glob, "on time"),
        4,
        "no duplicates"
    );
    assert_eq!(
        scalar_i64(
            &conn,
            &format!(
                "SELECT count(*) FROM read_parquet('{glob}', hive_partitioning=true, union_by_name=true) \
                 WHERE CAST(stacktrace AS VARCHAR) LIKE '%pooled_frame%'"
            )
        ),
        1,
        "a late pooled trace is materialized in cold, like the export does"
    );

    // Idempotent: a second pass appends nothing.
    let rec = eng
        .reconcile_range(&db.url, "error_events", from, to, &glob, &cold_dir)
        .expect("reconcile");
    assert_eq!(rec.exported, 0);
    assert_eq!(rec.verdict, Verdict::Ready { pg_rows: 6 });
    assert_eq!(eng.count_range(&glob, from, to).unwrap(), 6);

    std::fs::remove_dir_all(&dir).ok();
    db.cleanup();
}

/// A range with NO cold rows (the files were lost, or never written) is
/// exported in full — appending duplicates nothing because nothing is there.
#[test]
fn a_range_with_no_cold_rows_is_exported_in_full() {
    let Some(db) = Db::setup() else {
        eprintln!("TEST_DATABASE_URL unset — skipping late-arrival reconcile");
        return;
    };
    let (conn, dir, cold_dir, glob) = late_fixture(&db);
    let app = Uuid::new_v4();
    let (from, to) = (t(1, 0), t(2, 0));
    for h in [8u32, 9, 10] {
        insert_error(&conn, Uuid::new_v4(), app, "orphaned", t(1, h));
    }

    let eng = DuckEngine::open().unwrap();
    let rec = eng
        .reconcile_range(&db.url, "error_events", from, to, &glob, &cold_dir)
        .expect("reconcile");
    assert_eq!(rec.exported, 3);
    assert_eq!(rec.verdict, Verdict::Ready { pg_rows: 3 });
    assert_eq!(eng.count_range(&glob, from, to).unwrap(), 3);

    std::fs::remove_dir_all(&dir).ok();
    db.cleanup();
}

/// Cold holds rows for the range and not one matches Postgres by key — the
/// shape a broken key comparison would take. Appending would duplicate the
/// whole range, so the range is refused and cold is left exactly as it was.
#[test]
fn a_range_whose_keys_match_nothing_is_refused_not_duplicated() {
    let Some(db) = Db::setup() else {
        eprintln!("TEST_DATABASE_URL unset — skipping late-arrival reconcile");
        return;
    };
    let (conn, dir, cold_dir, glob) = late_fixture(&db);
    let app = Uuid::new_v4();
    let (from, to) = (t(1, 0), t(2, 0));
    for h in [8u32, 9, 10] {
        insert_error(&conn, Uuid::new_v4(), app, "exported", t(1, h));
    }
    let eng = DuckEngine::open().unwrap();
    eng.export_from_postgres(&db.url, "error_events", from, to, &cold_dir)
        .expect("export");
    // Replace every hot row with a different one: same count, no key in common.
    pg_exec(&conn, "DELETE FROM error_events");
    for h in [8u32, 9, 10] {
        insert_error(&conn, Uuid::new_v4(), app, "replaced", t(1, h));
    }

    let rec = eng
        .reconcile_range(&db.url, "error_events", from, to, &glob, &cold_dir)
        .expect("reconcile");
    assert_eq!(rec.exported, 0);
    let Verdict::Retain(why) = rec.verdict else {
        panic!(
            "a total key mismatch must be retained, got {:?}",
            rec.verdict
        );
    };
    assert!(why.contains("duplicate"), "{why}");
    assert_eq!(
        eng.count_range(&glob, from, to).unwrap(),
        3,
        "cold untouched"
    );

    std::fs::remove_dir_all(&dir).ok();
    db.cleanup();
}
