//! `sauron-db` — the only crate that knows about diesel.
//!
//! Owns the generated [`schema`], the row/insert [`models`], the diesel-async
//! [`pool`], the [`repo`]sitory functions both binaries call, and the embedded
//! migrations run at startup.

pub mod batch;
pub mod filter;
pub mod models;
pub mod pool;
pub mod query_plan;
pub mod repo;
pub mod schema;
pub mod scope;

pub use pool::{build_pool, conn, PgConn, PgPool};

/// Re-exported so downstream crates can name the connection type without a
/// direct diesel-async dependency.
pub use diesel_async::AsyncPgConnection;

use diesel_async::async_connection_wrapper::AsyncConnectionWrapper;
use diesel_migrations::{embed_migrations, EmbeddedMigrations, MigrationHarness};

/// Migrations compiled into the binary. Path is relative to this crate.
pub const MIGRATIONS: EmbeddedMigrations = embed_migrations!("../../migrations");

/// Apply any pending migrations. Diesel migrations are synchronous, so we run
/// them through diesel-async's [`AsyncConnectionWrapper`] on a blocking thread —
/// this avoids linking libpq while still reusing the async Postgres transport.
pub async fn run_pending_migrations(database_url: &str) -> anyhow::Result<()> {
    let url = database_url.to_owned();
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        use diesel::Connection as _;
        let mut wrapper = AsyncConnectionWrapper::<AsyncPgConnection>::establish(&url)
            .map_err(|e| anyhow::anyhow!("connect for migrations: {e}"))?;
        wrapper
            .run_pending_migrations(MIGRATIONS)
            .map_err(|e| anyhow::anyhow!("run migrations: {e}"))?;
        Ok(())
    })
    .await??;
    Ok(())
}

/// Default tolerance for a Postgres that is not accepting connections *yet*.
///
/// 120s covers a co-located Postgres still doing crash recovery and the common
/// "both units came up in the same boot" race, without turning a genuinely
/// unreachable database into a five-minute mystery.
pub const DEFAULT_MIGRATE_WAIT_SECS: u64 = 120;

/// Wait (bounded) for Postgres to accept connections, then migrate once.
///
/// **This function is what makes the daemons' `Requires=sauron-migrate.service`
/// survivable, and it is not optional.** With plain [`run_pending_migrations`]
/// the migrator makes exactly ONE `establish` attempt — a refused port fails in
/// about 6ms — and a failed oneshot start job fails the *dependent* daemon's
/// start job too. systemd does not retry a failed start job: the daemon lands
/// `inactive` with `NRestarts=0` and stays there even after the database comes
/// back, because `Restart=on-failure` governs a process that exited, not a job
/// that never ran. Measured in a real systemd container: still inactive 30s
/// after the cause was fixed. Before the `Requires=` existed, `sauron-api`
/// crash-looped through the same window and recovered by itself, so shipping the
/// dependency without this retry is strictly *worse* than shipping neither.
///
/// Only the CONNECT is retried. A migration that runs and fails is a real error
/// and must surface immediately — retrying it would just apply a broken change
/// repeatedly, or mask a genuine conflict as slowness.
///
/// Each probe gets its own timeout because a refused connection and a blackholed
/// route fail very differently: refused returns in milliseconds, a dropped
/// packet hangs indefinitely (measured: still hanging at 45s). Without
/// `PROBE_TIMEOUT` the `wait` budget could be consumed by a single attempt, and
/// `Type=oneshot` defaults to `TimeoutStartUSec=infinity`, so that would hang
/// `multi-user.target` at boot rather than failing.
pub async fn run_pending_migrations_waiting(
    database_url: &str,
    wait: std::time::Duration,
) -> anyhow::Result<()> {
    /// Per-attempt cap, so one unroutable address cannot eat the whole budget.
    const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
    /// Gap between attempts. Coarse on purpose: this is a boot-ordering race,
    /// not a hot path, and a tight loop against a starting Postgres just fills
    /// its log with rejected connections.
    const PROBE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

    // Zero means "do not wait", and it skips the probe ENTIRELY rather than
    // probing once with a zero budget. Somewhere something else already
    // guarantees the database is up — the Compose `migrate` service has
    // `depends_on: {postgres: {condition: service_healthy}}` — and in that world
    // a probe adds a failure mode without adding a guarantee.
    if wait.is_zero() {
        return run_pending_migrations(database_url).await;
    }

    let deadline = tokio::time::Instant::now() + wait;
    let mut attempts: u32 = 0;
    // Reassigned each attempt so the final error names the LAST reason, not the
    // first. No seed value: every read is preceded by an assignment in the loop,
    // and a placeholder would be dead code the compiler warns about.
    let mut last_err: String;

    loop {
        attempts += 1;
        // The probe uses the SAME establish path as the migration below —
        // `AsyncConnectionWrapper` on a blocking thread — and that is
        // deliberate: a readiness probe that can report not-ready for a server
        // the real connection would accept is worse than no probe at all, and
        // any probe that is not the real code path can drift from it. This one
        // cannot, by construction.
        //
        // (An earlier revision probed with `AsyncPgConnection::establish`
        // instead. It was changed for the reason above, on principle — NOT
        // because that flavour was observed to fail. It appeared to fail during
        // development, but the cause turned out to be a host networking failure
        // that broke every connection to a published container port, including
        // `psql`, so nothing was learned about the two constructors. Recorded
        // here so the next reader does not inherit a false root cause.)
        let probe_url = database_url.to_owned();
        let probe = tokio::task::spawn_blocking(move || {
            use diesel::Connection as _;
            AsyncConnectionWrapper::<AsyncPgConnection>::establish(&probe_url).map(|_| ())
        });
        match tokio::time::timeout(PROBE_TIMEOUT, probe).await {
            // Connection dropped immediately; it exists only to answer "is the
            // server accepting connections yet".
            Ok(Ok(Ok(()))) => break,
            Ok(Ok(Err(e))) => last_err = e.to_string(),
            Ok(Err(join)) => last_err = format!("probe task failed: {join}"),
            // The blocking task is NOT cancelled by this timeout and may linger
            // on a blackholed route. Tolerated: this binary is a one-shot that
            // exits, and `TimeoutStartSec` on sauron-migrate.service is the real
            // backstop. It is called out so nobody reuses this inside a daemon.
            Err(_) => last_err = format!("connect timed out after {}s", PROBE_TIMEOUT.as_secs()),
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(anyhow::anyhow!(
                "postgres did not accept connections within {}s ({} attempt(s)); last error: {}. \
                 Raise MIGRATE_WAIT_SECS if the database legitimately takes longer to start, and \
                 keep sauron-migrate.service's TimeoutStartSec above it.",
                wait.as_secs(),
                attempts,
                last_err
            ));
        }
        // Log from the second attempt on: one warning means "not up yet", which
        // is the normal boot race and should not look like an incident.
        if attempts == 2 {
            tracing::warn!(
                "postgres not accepting connections yet ({last_err}); retrying for up to {}s",
                wait.as_secs()
            );
        }
        tokio::time::sleep(PROBE_INTERVAL).await;
    }

    if attempts > 1 {
        tracing::info!("postgres reachable after {attempts} attempt(s)");
    }
    run_pending_migrations(database_url).await
}

// ===========================================================================
// Boot-time schema/binary drift gate
// ===========================================================================
//
// RPM upgrades replace the binaries but historically never re-ran
// `sauron-migrate` (packaging/rpm/SETUP.md §11). The symptom of that is the
// worst kind: the process boots, logs "listening on ...", and then every
// request that happens to name a column a missing migration adds returns 500,
// forever, with nothing in the boot log to connect the two. Operators read it
// as "the dashboard is broken" and go looking in the frontend.
//
// This turns that into one unmissable failure at t=0, naming the exact missing
// versions and the exact command that fixes it.
//
// ---------------------------------------------------------------------------
// DECISION: REFUSE TO BOOT (non-zero exit), with a documented escape hatch.
// ---------------------------------------------------------------------------
//
// The alternative — start degraded-but-loud — was rejected. The argument for
// it is real: refusing to boot converts a partially-working deployment into a
// total outage, and under `Restart=on-failure` a drifting service becomes a
// crash loop rather than a service. Three things decide it the other way:
//
//  1. "Partially working" is a fiction here in the only direction that
//     matters. Migrations in this repo add columns/tables that the *new*
//     binary's queries name unconditionally; the code has no feature flags
//     keyed on schema version. So the degraded state is not "most endpoints
//     work" — it is "an unpredictable, per-endpoint subset 500s", which is
//     precisely the silent storm this exists to prevent. A drifting
//     `sauron-ingest` is worse still: it answers the SDK `202` and then the
//     worker fails to write, so telemetry is destroyed while every client
//     believes it was delivered.
//  2. A crash loop is *findable*. `systemctl status` is red, the unit is
//     `failed`, the journal repeats the banner, and any liveness probe fires.
//     A single ERROR line at boot of an otherwise-healthy process scrolls out
//     of the journal in minutes and is exactly what nobody reads.
//  3. Refusing is cheap to undo and safe by construction: the fix
//     (`sauron-migrate`) is one command, and the failure cannot be triggered
//     by traffic, only by a deployment step that was already wrong.
//
// The escape hatch is `SAURON_ALLOW_SCHEMA_DRIFT=1`, for the operator who has
// looked at the banner and has decided that a partial 500 storm beats an
// outage right now (e.g. a migration that cannot be applied during business
// hours). It logs the same banner at ERROR every time and starts anyway. It is
// deliberately an env var and not a config-file key: it should be an explicit,
// visible, temporary act on a single unit, not something that gets committed.
//
// Cost: exactly one `SELECT version FROM __diesel_schema_migrations` per
// process start. Nothing is checked per request.
//
// FRESH INSTALL: an empty database (no `__diesel_schema_migrations` at all) is
// reported distinctly and also refuses. That is not a new failure — both
// binaries are already unusable against an empty database (`sauron-api` dies
// at `ensure_preset_roles`, the ingest worker cannot write a row) — it only
// replaces an opaque diesel "relation does not exist" with the remedy. In the
// shipped flow this state is unreachable anyway: every daemon unit now carries
// `Requires=sauron-migrate.service`, and compose's daemons depend on the
// `migrate` service, so the migrator has run before this code does.
//
// SCHEMA AHEAD OF BINARY (a downgrade, or a rolling deploy mid-flight) is
// reported as a WARNING and does not block: diesel never reverts, extra
// applied versions are additive, and an old binary against a new schema
// usually works. It is logged because it is the other half of the same
// question and an operator staring at odd behaviour deserves to see it.

/// What the binary's embedded migrations and the database's
/// `__diesel_schema_migrations` table say about each other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaStatus {
    /// Every migration version compiled into this binary, sorted.
    pub embedded: Vec<String>,
    /// Every version recorded as applied in the database, sorted.
    pub applied: Vec<String>,
    /// Embedded but not applied — the database is BEHIND this binary.
    pub missing: Vec<String>,
    /// Applied but not embedded — the database is AHEAD of this binary.
    pub unknown: Vec<String>,
    /// `missing`, rendered as `version (directory_name)`.
    ///
    /// Diesel stores the version with the dashes stripped
    /// (`2026-08-09-000046_channel_config_enc` is recorded as
    /// `20260809000046`), which nobody can map back to a file by eye. The
    /// banner has to name something an operator can `ls` for, so the full
    /// directory name is carried alongside. Same order as `missing`.
    pub missing_labels: Vec<String>,
    /// `false` when `__diesel_schema_migrations` does not exist at all, i.e. a
    /// brand-new database the migrator has never touched.
    pub table_present: bool,
}

impl SchemaStatus {
    /// The database lacks migrations this binary was built against.
    pub fn is_behind(&self) -> bool {
        !self.missing.is_empty()
    }

    /// The database has migrations this binary has never heard of.
    pub fn is_ahead(&self) -> bool {
        !self.unknown.is_empty()
    }

    /// The multi-line operator-facing banner. Written to be greppable and to
    /// contain the remedy inline — an error that makes you go and find a
    /// runbook is an error that gets ignored.
    pub fn banner(&self, component: &str, fatal: bool) -> String {
        let rule = "=".repeat(78);
        let headline = if self.table_present {
            format!("DATABASE SCHEMA IS BEHIND THIS BINARY ({component})")
        } else {
            format!("DATABASE HAS NEVER BEEN MIGRATED ({component})")
        };
        let verdict = if fatal {
            format!("{component} is REFUSING TO START.")
        } else {
            format!(
                "{component} is starting anyway because SAURON_ALLOW_SCHEMA_DRIFT is set. \
                 Expect 500s."
            )
        };
        let mut out = String::new();
        out.push_str(&format!("\n{rule}\n{headline}\n{verdict}\n\n"));
        out.push_str(&format!(
            "  binary embeds : {} migration(s){}\n",
            self.embedded.len(),
            self.embedded
                .last()
                .map(|v| format!(", newest {v}"))
                .unwrap_or_default()
        ));
        if self.table_present {
            out.push_str(&format!(
                "  database has  : {} applied{}\n",
                self.applied.len(),
                self.applied
                    .last()
                    .map(|v| format!(", newest {v}"))
                    .unwrap_or_default()
            ));
        } else {
            out.push_str("  database has  : no __diesel_schema_migrations table\n");
        }
        out.push_str(&format!("  MISSING ({})   :\n", self.missing.len()));
        for label in &self.missing_labels {
            out.push_str(&format!("      {label}\n"));
        }
        out.push('\n');
        out.push_str(
            "Every query naming a column or table those migrations add will fail with a 500.\n\
             Apply them, then start this service again:\n\n\
             \x20 RPM      : sudo systemctl start sauron-migrate\n\
             \x20 tarball  : sudo -u sauron DATABASE_URL=... /usr/bin/sauron-migrate\n\
             \x20 compose  : docker compose run --rm migrate\n\n\
             To start regardless (partial 500s, NOT a fix): SAURON_ALLOW_SCHEMA_DRIFT=1\n",
        );
        out.push_str(&rule);
        out
    }
}

/// Compare the migrations embedded in this binary against those recorded in the
/// database. One query on the happy path.
///
/// Takes a live connection rather than a URL so callers reuse the pool
/// checkout they already have; the caller decides what an *unreachable*
/// database means for it (fatal for `sauron-api`, tolerated by `sauron-ingest`
/// — see their boot paths).
pub async fn schema_status(conn: &mut AsyncPgConnection) -> anyhow::Result<SchemaStatus> {
    use diesel_async::RunQueryDsl;

    #[derive(diesel::QueryableByName)]
    struct VersionRow {
        #[diesel(sql_type = diesel::sql_types::Text)]
        version: String,
    }
    #[derive(diesel::QueryableByName)]
    struct PresentRow {
        #[diesel(sql_type = diesel::sql_types::Bool)]
        present: bool,
    }

    // (version, directory name), sorted by version.
    let mut embedded_pairs = embedded_migrations()?;
    embedded_pairs.sort();
    let embedded: Vec<String> = embedded_pairs.iter().map(|(v, _)| v.clone()).collect();

    let (mut applied, table_present) =
        match diesel::sql_query("SELECT version FROM __diesel_schema_migrations")
            .load::<VersionRow>(&mut *conn)
            .await
        {
            Ok(rows) => (
                rows.into_iter().map(|r| r.version).collect::<Vec<_>>(),
                true,
            ),
            // The happy path is one query. Only on failure do we spend a second
            // one distinguishing "brand-new database" from a genuine problem —
            // conflating the two would either mask a broken database or fail a
            // fresh install with the wrong message.
            Err(e) => {
                let present = diesel::sql_query(
                    "SELECT to_regclass('__diesel_schema_migrations') IS NOT NULL AS present",
                )
                .load::<PresentRow>(&mut *conn)
                .await
                // `.into_iter().next()`, not `.first()`: `RunQueryDsl` is in
                // scope and its blanket-impl `first` wins method resolution
                // over the slice inherent method, which does not compile and
                // whose error names `LimitDsl`, not this line.
                .map(|r: Vec<PresentRow>| r.into_iter().next().is_some_and(|p| p.present))
                .unwrap_or(true);
                if present {
                    return Err(anyhow::anyhow!("read __diesel_schema_migrations: {e}"));
                }
                (Vec::new(), false)
            }
        };
    applied.sort();

    let missing_pairs: Vec<&(String, String)> = embedded_pairs
        .iter()
        .filter(|(v, _)| applied.binary_search(v).is_err())
        .collect();
    let missing: Vec<String> = missing_pairs.iter().map(|(v, _)| v.clone()).collect();
    let missing_labels: Vec<String> = missing_pairs
        .iter()
        .map(|(v, name)| format!("{v}  ({name})"))
        .collect();
    let unknown: Vec<String> = applied
        .iter()
        .filter(|v| embedded.binary_search(v).is_err())
        .cloned()
        .collect();

    Ok(SchemaStatus {
        embedded,
        applied,
        missing,
        missing_labels,
        unknown,
        table_present,
    })
}

/// Boot gate. Call once, early, from every long-lived binary EXCEPT
/// `sauron-migrate` — the migrator is the thing that resolves this condition,
/// so gating it on the condition would be a deadlock by construction.
///
/// Returns `Err` (so the caller's `main` exits non-zero and systemd's
/// `Restart=on-failure` / a container restart policy surfaces it) when the
/// database is behind, unless `SAURON_ALLOW_SCHEMA_DRIFT` is set to a truthy
/// value. See the decision comment above this function's module section.
pub async fn require_current_schema(
    conn: &mut AsyncPgConnection,
    component: &str,
) -> anyhow::Result<()> {
    let status = schema_status(conn).await?;
    let allow_drift = std::env::var("SAURON_ALLOW_SCHEMA_DRIFT")
        .map(|v| {
            let v = v.trim().to_ascii_lowercase();
            !(v.is_empty() || v == "0" || v == "false" || v == "no")
        })
        .unwrap_or(false);
    enforce_schema_status(&status, component, allow_drift)
}

/// The pure half of [`require_current_schema`]: log, then decide. Split out so
/// both branches are testable without mutating process-global environment.
pub fn enforce_schema_status(
    status: &SchemaStatus,
    component: &str,
    allow_drift: bool,
) -> anyhow::Result<()> {
    if status.is_ahead() {
        // Not fatal — see the module comment. Still worth a line, because it is
        // the signature of a downgrade and of a half-finished rolling deploy.
        tracing::warn!(
            unknown = %status.unknown.join(", "),
            "database has migrations this binary does not embed ({component} is older than the \
             schema); this is usually harmless but is the signature of a downgrade"
        );
    }
    if !status.is_behind() {
        tracing::info!(
            applied = status.applied.len(),
            embedded = status.embedded.len(),
            "schema is current"
        );
        return Ok(());
    }
    let banner = status.banner(component, !allow_drift);
    tracing::error!("{banner}");
    // Also to stderr, unconditionally. `tracing` output depends on RUST_LOG and
    // on a subscriber being installed; the one message that must never be lost
    // is the one explaining why the process is about to die.
    eprintln!("{banner}");
    if allow_drift {
        return Ok(());
    }
    Err(anyhow::anyhow!(
        "{component}: database schema is behind this binary; {} migration(s) not applied \
         (oldest missing: {}). Run sauron-migrate, or set SAURON_ALLOW_SCHEMA_DRIFT=1 to start \
         anyway.",
        status.missing.len(),
        status
            .missing_labels
            .first()
            .map(String::as_str)
            .unwrap_or("?")
    ))
}

/// `(version, directory name)` for every migration compiled into this binary.
///
/// The version is what `__diesel_schema_migrations` stores and the only thing
/// comparable against it; the name is what an operator can find on disk.
fn embedded_migrations() -> anyhow::Result<Vec<(String, String)>> {
    use diesel::migration::MigrationSource;
    let migrations = MigrationSource::<diesel::pg::Pg>::migrations(&MIGRATIONS)
        .map_err(|e| anyhow::anyhow!("read embedded migrations: {e}"))?;
    Ok(migrations
        .iter()
        .map(|m| {
            let name = m.name();
            (name.version().to_string(), name.to_string())
        })
        .collect())
}

// ===========================================================================
// Admin DDL — create/drop whole databases (used by the crebain benchmark to
// spin up and tear down an isolated ephemeral database).
// ===========================================================================

/// Create a database by `db_name` on the server addressed by `maintenance_url`.
/// `maintenance_url` must point at any *existing* database on the same server
/// other than the one being created (e.g. the app's own database).
///
/// `CREATE DATABASE` cannot run inside a transaction and cannot be parameterized,
/// so it is issued through the simple query protocol (`batch_execute`) and the
/// identifier is validated rather than bound.
pub async fn create_database(maintenance_url: &str, db_name: &str) -> anyhow::Result<()> {
    run_admin_ddl(
        maintenance_url,
        db_name,
        &format!("CREATE DATABASE \"{db_name}\""),
    )
    .await
}

/// Drop `db_name` if it exists, terminating any other sessions still connected
/// (`WITH (FORCE)`, Postgres 13+). Idempotent. `maintenance_url` must not point
/// at the database being dropped.
pub async fn drop_database(maintenance_url: &str, db_name: &str) -> anyhow::Result<()> {
    run_admin_ddl(
        maintenance_url,
        db_name,
        &format!("DROP DATABASE IF EXISTS \"{db_name}\" WITH (FORCE)"),
    )
    .await
}

/// Guard against SQL injection through an un-bindable identifier: only a plain,
/// lowercase Postgres identifier (letters/digits/underscore, not starting with a
/// digit, ≤ 63 bytes) is allowed.
fn validate_db_ident(name: &str) -> anyhow::Result<()> {
    let valid = !name.is_empty()
        && name.len() <= 63
        && name
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b == b'_')
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if valid {
        Ok(())
    } else {
        anyhow::bail!("unsafe database identifier: {name:?}")
    }
}

async fn run_admin_ddl(maintenance_url: &str, db_name: &str, sql: &str) -> anyhow::Result<()> {
    use diesel_async::{AsyncConnection, SimpleAsyncConnection};
    validate_db_ident(db_name)?;
    let mut conn = AsyncPgConnection::establish(maintenance_url)
        .await
        .map_err(|e| anyhow::anyhow!("connect maintenance db: {e}"))?;
    conn.batch_execute(sql)
        .await
        .map_err(|e| anyhow::anyhow!("admin ddl `{sql}` failed: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod admin_tests {
    use super::validate_db_ident;

    #[test]
    fn accepts_safe_bench_names() {
        assert!(validate_db_ident("crebain_bench_0123456789abcdef0123456789abcdef").is_ok());
        assert!(validate_db_ident("sauron").is_ok());
        assert!(validate_db_ident("_x").is_ok());
    }

    #[test]
    fn rejects_unsafe_names() {
        assert!(validate_db_ident("").is_err());
        assert!(validate_db_ident("has space").is_err());
        assert!(validate_db_ident("drop\";--").is_err());
        assert!(validate_db_ident("1leading_digit").is_err());
        assert!(validate_db_ident("UpperCase").is_err());
        assert!(validate_db_ident(&"x".repeat(64)).is_err());
    }
}
