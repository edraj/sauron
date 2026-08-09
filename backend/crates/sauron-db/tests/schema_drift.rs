//! Boot-time schema/binary drift gate — `sauron_db::schema_status` and
//! `require_current_schema`.
//!
//! Deliberately self-contained rather than reusing `tests/common`: the whole
//! point of these cases is databases in states that harness never produces (not
//! migrated at all, migrated and then rolled BACKWARDS relative to the binary),
//! so it provisions its own ephemeral databases directly.
//!
//! Skips rather than fails when `TEST_DATABASE_URL` is unset, matching the rest
//! of this crate's integration tests.

use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};
use sauron_db::SchemaStatus;
use uuid::Uuid;

/// Maintenance URL, or `None` — callers skip.
fn admin_url() -> Option<String> {
    std::env::var("TEST_DATABASE_URL").ok()
}

fn ephemeral_db_name() -> String {
    format!("sauron_drift_{}", Uuid::new_v4().simple())
}

/// Swap the database component of a Postgres URL, preserving everything else.
fn swap_database(url: &str, db_name: &str) -> String {
    let (head, _) = url.rsplit_once('/').expect("url with a database component");
    format!("{head}/{db_name}")
}

/// An ephemeral database that drops itself.
struct Db {
    admin_url: String,
    name: String,
    url: String,
}

impl Db {
    /// `migrated = false` leaves the database completely empty — no
    /// `__diesel_schema_migrations` table at all.
    async fn create(migrated: bool) -> Option<Db> {
        let admin_url = admin_url()?;
        let name = ephemeral_db_name();
        sauron_db::create_database(&admin_url, &name)
            .await
            .expect("create ephemeral database");
        let url = swap_database(&admin_url, &name);
        if migrated {
            sauron_db::run_pending_migrations(&url)
                .await
                .expect("migrate ephemeral database");
        }
        Some(Db {
            admin_url,
            name,
            url,
        })
    }

    async fn conn(&self) -> AsyncPgConnection {
        AsyncPgConnection::establish(&self.url)
            .await
            .expect("connect to ephemeral database")
    }

    async fn drop_it(self) {
        sauron_db::drop_database(&self.admin_url, &self.name)
            .await
            .expect("drop ephemeral database");
    }
}

/// Forget the `n` newest applied migrations — exactly the shape an RPM upgrade
/// that replaced the binaries without re-running `sauron-migrate` leaves behind
/// (the binary embeds versions the table has never heard of). Rolling the DDL
/// back too would only make the failure louder; the recorded-version gap is
/// what the gate reads, and what a real drift always exhibits.
async fn forget_newest_applied(conn: &mut AsyncPgConnection, n: i64) -> Vec<String> {
    #[derive(diesel::QueryableByName)]
    struct V {
        #[diesel(sql_type = diesel::sql_types::Text)]
        version: String,
    }
    let removed: Vec<String> = diesel::sql_query(format!(
        "DELETE FROM __diesel_schema_migrations WHERE version IN \
         (SELECT version FROM __diesel_schema_migrations ORDER BY version DESC LIMIT {n}) \
         RETURNING version"
    ))
    .load::<V>(conn)
    .await
    .expect("delete newest applied migration rows")
    .into_iter()
    .map(|r| r.version)
    .collect();
    assert_eq!(removed.len(), n as usize, "expected to remove {n} rows");
    removed
}

// ---------------------------------------------------------------------------
// Case 1: database level with the binary — boots.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn schema_level_with_binary_boots() {
    let Some(db) = Db::create(true).await else {
        eprintln!("skipping: TEST_DATABASE_URL unset");
        return;
    };
    let mut conn = db.conn().await;

    let status = sauron_db::schema_status(&mut conn).await.expect("status");
    assert!(status.table_present, "migrated db has the tracking table");
    assert!(
        !status.embedded.is_empty(),
        "the binary must embed migrations at all"
    );
    assert_eq!(
        status.missing,
        Vec::<String>::new(),
        "a freshly migrated database is missing nothing"
    );
    assert_eq!(
        status.unknown,
        Vec::<String>::new(),
        "and has nothing this binary does not embed"
    );
    assert_eq!(status.applied, status.embedded);
    assert!(!status.is_behind());
    assert!(!status.is_ahead());

    // The gate lets the process boot.
    sauron_db::require_current_schema(&mut conn, "sauron-api")
        .await
        .expect("current schema must not block boot");

    drop(conn);
    db.drop_it().await;
}

// ---------------------------------------------------------------------------
// Case 2: database BEHIND the binary — the new behaviour.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn schema_behind_binary_refuses_to_boot_and_names_the_versions() {
    let Some(db) = Db::create(true).await else {
        eprintln!("skipping: TEST_DATABASE_URL unset");
        return;
    };
    let mut conn = db.conn().await;

    let mut forgotten = forget_newest_applied(&mut conn, 3).await;
    forgotten.sort();

    let status = sauron_db::schema_status(&mut conn).await.expect("status");
    assert!(status.table_present);
    assert!(status.is_behind(), "3 unapplied migrations is 'behind'");
    assert!(!status.is_ahead());
    assert_eq!(
        status.missing, forgotten,
        "names the exact missing versions"
    );
    assert_eq!(status.applied.len(), status.embedded.len() - 3);
    // Diesel records the version dash-stripped, which nobody can map back to a
    // file. Each missing entry must also carry the on-disk directory name.
    for (version, label) in forgotten.iter().zip(&status.missing_labels) {
        assert!(label.starts_with(version), "{label} should start {version}");
        assert!(
            label.contains('_'),
            "label must carry the directory name, not just the version: {label}"
        );
    }

    // Non-zero exit: `main` returns this `Err`.
    let err = sauron_db::require_current_schema(&mut conn, "sauron-api")
        .await
        .expect_err("a behind schema must refuse to boot");
    let msg = err.to_string();
    assert!(msg.contains("sauron-api"), "names the component: {msg}");
    assert!(
        msg.contains("behind this binary"),
        "says what is wrong: {msg}"
    );
    assert!(msg.contains("sauron-migrate"), "names the remedy: {msg}");
    assert!(
        msg.contains(&forgotten[0]),
        "names the oldest missing version: {msg}"
    );

    // The operator-facing banner carries every missing version and the exact
    // commands, so nobody has to go and find a runbook.
    let banner = status.banner("sauron-api", true);
    for v in &forgotten {
        assert!(banner.contains(v), "banner must list {v}:\n{banner}");
    }
    assert!(banner.contains("REFUSING TO START"), "{banner}");
    assert!(
        banner.contains("systemctl start sauron-migrate"),
        "{banner}"
    );
    assert!(banner.contains("SAURON_ALLOW_SCHEMA_DRIFT"), "{banner}");

    drop(conn);
    db.drop_it().await;
}

// ---------------------------------------------------------------------------
// Case 3: brand-new, completely empty database.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn brand_new_empty_database_is_reported_not_crashed() {
    let Some(db) = Db::create(false).await else {
        eprintln!("skipping: TEST_DATABASE_URL unset");
        return;
    };
    let mut conn = db.conn().await;

    // The probe must survive a missing tracking table rather than surfacing a
    // raw diesel "relation does not exist".
    let status = sauron_db::schema_status(&mut conn)
        .await
        .expect("an empty database must not error the probe");
    assert!(!status.table_present);
    assert!(status.applied.is_empty());
    assert_eq!(
        status.missing, status.embedded,
        "everything the binary embeds is missing"
    );
    assert_eq!(status.missing_labels.len(), status.embedded.len());
    assert!(!status.is_ahead());

    let banner = status.banner("sauron-api", true);
    assert!(
        banner.contains("NEVER BEEN MIGRATED"),
        "an empty database gets its own headline, not the drift one:\n{banner}"
    );
    assert!(
        banner.contains("no __diesel_schema_migrations table"),
        "{banner}"
    );

    // And the connection is still usable afterwards — the failed SELECT must
    // not have poisoned it, or the boot path would break in a second way.
    let status_again = sauron_db::schema_status(&mut conn)
        .await
        .expect("probe is repeatable on the same connection");
    assert_eq!(status, status_again);

    drop(conn);
    db.drop_it().await;
}

// ---------------------------------------------------------------------------
// Case 4: database AHEAD of the binary (downgrade) — warns, does not block.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn schema_ahead_of_binary_warns_but_boots() {
    let Some(db) = Db::create(true).await else {
        eprintln!("skipping: TEST_DATABASE_URL unset");
        return;
    };
    let mut conn = db.conn().await;

    diesel::sql_query(
        "INSERT INTO __diesel_schema_migrations (version) VALUES ('2099-01-01-000001')",
    )
    .execute(&mut conn)
    .await
    .expect("record a version this binary cannot know");

    let status = sauron_db::schema_status(&mut conn).await.expect("status");
    assert!(status.is_ahead());
    assert_eq!(status.unknown, vec!["2099-01-01-000001".to_string()]);
    assert!(!status.is_behind());

    sauron_db::require_current_schema(&mut conn, "sauron-api")
        .await
        .expect("an ahead schema must not block boot");

    drop(conn);
    db.drop_it().await;
}

// ---------------------------------------------------------------------------
// The decision half, without a database: no process-global env mutation, so
// these are safe under the default parallel test runner.
// ---------------------------------------------------------------------------

fn behind_status() -> SchemaStatus {
    SchemaStatus {
        embedded: vec!["20260101000001".into(), "20260102000002".into()],
        applied: vec!["20260101000001".into()],
        missing: vec!["20260102000002".into()],
        missing_labels: vec!["20260102000002  (2026-01-02-000002_widgets)".into()],
        unknown: vec![],
        table_present: true,
    }
}

#[test]
fn drift_is_fatal_by_default() {
    let err = sauron_db::enforce_schema_status(&behind_status(), "sauron-ingest", false)
        .expect_err("default is refuse-to-boot");
    assert!(err.to_string().contains("sauron-ingest"));
}

#[test]
fn drift_override_starts_anyway_and_still_says_so() {
    let status = behind_status();
    sauron_db::enforce_schema_status(&status, "sauron-ingest", true)
        .expect("the documented escape hatch must actually start the process");
    let banner = status.banner("sauron-ingest", false);
    assert!(banner.contains("starting anyway"), "{banner}");
    assert!(banner.contains("Expect 500s"), "{banner}");
    assert!(banner.contains("2026-01-02-000002_widgets"), "{banner}");
}
