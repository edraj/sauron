//! `sauron-db` — the only crate that knows about diesel.
//!
//! Owns the generated [`schema`], the row/insert [`models`], the diesel-async
//! [`pool`], the [`repo`]sitory functions both binaries call, and the embedded
//! migrations run at startup.

pub mod batch;
pub mod device_env_backfill;
pub mod filter;
pub mod identity_merge;
pub mod models;
pub mod person_env_backfill;
pub mod pool;
pub mod purge;
pub mod query_plan;
pub mod repo;
pub mod schema;
pub mod scope;
pub mod stack_pool;

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
/// A short, stable hex digest of the migration set compiled into this binary.
///
/// Intended as a cache key for anything derived from the schema — notably the
/// test harness's template database, which must be rebuilt the moment a
/// migration is added or removed. Two properties matter and neither is
/// accidental:
///
/// * It is content-independent: only migration *identities* (version + name)
///   feed the hash, not the SQL inside them. Editing an already-applied
///   migration in place therefore does NOT change this value — which is sound
///   only because the `migrations` CI job rejects that edit outright. If that
///   guard is ever removed, this must start hashing file contents.
/// * The hash is FNV-1a written out here rather than `DefaultHasher`, whose
///   output is explicitly not stable across Rust releases. A digest that
///   silently changes on a toolchain bump would quietly invalidate every
///   cached artifact keyed on it.
pub fn migrations_fingerprint() -> anyhow::Result<String> {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |bytes: &[u8]| {
        for b in bytes {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    for (version, name) in embedded_migrations()? {
        eat(version.as_bytes());
        eat(b"\0");
        eat(name.as_bytes());
        eat(b"\n");
    }
    Ok(format!("{h:016x}"))
}

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

/// Create `db_name` as a physical copy of an existing `template` database,
/// rather than as an empty database that then has DDL replayed into it.
///
/// Postgres implements this as a file-level copy of the template's directory,
/// which is dramatically cheaper than executing the equivalent statements: for
/// this workspace's 63 migrations, measured against `postgres:16`, a
/// create + migrate + drop cycle is ~395 ms and a create-from-template + drop
/// cycle is ~66 ms.
///
/// Two constraints the caller owns, both enforced by Postgres rather than here:
/// no session may be connected to `template` while the copy runs, and the
/// template must already contain the schema the caller expects — this function
/// does not verify what it is copying.
pub async fn create_database_from_template(
    maintenance_url: &str,
    db_name: &str,
    template: &str,
) -> anyhow::Result<()> {
    // `run_admin_ddl` validates `db_name`; `template` is interpolated into the
    // same statement and needs the identical guard.
    validate_db_ident(template)?;
    run_admin_ddl(
        maintenance_url,
        db_name,
        &format!("CREATE DATABASE \"{db_name}\" TEMPLATE \"{template}\""),
    )
    .await
}

/// Rename `from` to `to`. Requires that no session is connected to `from`.
pub async fn rename_database(maintenance_url: &str, from: &str, to: &str) -> anyhow::Result<()> {
    validate_db_ident(to)?;
    run_admin_ddl(
        maintenance_url,
        from,
        &format!("ALTER DATABASE \"{from}\" RENAME TO \"{to}\""),
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

// ===========================================================================
// Test-harness support — one migrated template per server, copied per test.
//
// Lives here rather than in any single harness because there are twenty of
// them: `sauron-db`'s `tests/common`, sixteen `sauron-api` HTTP suites, and
// `sauron-pipeline`'s. Each used to open with the same create-then-migrate
// pair, so each independently replayed all migrations into every database it
// made. Centralising the fast path is what lets one change reach all of them;
// leaving the pair inlined per harness is what let the cost hide.
// ===========================================================================

/// Prefix of the shared template database. Inside the `sauron_test_%`
/// namespace so the harnesses' stale-database reapers find it with the pattern
/// they already use — and so they can recognise and skip it.
pub const TEST_TEMPLATE_PREFIX: &str = "sauron_test_tmpl_";

static TEST_TEMPLATE: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();

/// Create `db_name` already migrated, for a test harness.
///
/// Equivalent in outcome to [`create_database`] followed by
/// [`run_pending_migrations`], and that is exactly the fallback if anything
/// about the fast path is unavailable. The fast path instead copies a template
/// database that was migrated once for this server, which Postgres performs as
/// a file-level copy: measured on `postgres:16` against this workspace's 63
/// migrations, 395 ms → 66 ms per database.
///
/// The fallback is not a formality. A copy is refused while any session is
/// connected to the template, so a busy server can legitimately reject it; when
/// that happens the caller must still get a correctly migrated database, just
/// more slowly. No failure mode here may turn into a test failure that looks
/// like a schema bug.
pub async fn create_test_database(maintenance_url: &str, db_name: &str) -> anyhow::Result<()> {
    if let Some(template) = ensure_migrated_template(maintenance_url).await {
        match create_database_from_template(maintenance_url, db_name, &template).await {
            Ok(()) => return Ok(()),
            Err(e) => {
                eprintln!(
                    "create_test_database: copying {template} -> {db_name} failed, \
                     migrating from scratch instead: {e}"
                );
                // The failed CREATE may or may not have left the name taken.
                let _ = drop_database(maintenance_url, db_name).await;
            }
        }
    }
    create_database(maintenance_url, db_name).await?;
    run_pending_migrations(&swap_database_url(maintenance_url, db_name)).await
}

/// Name and advisory-lock key of the template matching the migrations compiled
/// into this binary.
///
/// The fingerprint in the name is what makes adding a migration correct by
/// construction: it produces a *different* name, so a stale template is never
/// asked for again rather than being detected and repaired.
fn test_template_identity() -> Option<(String, i64)> {
    let fingerprint = migrations_fingerprint().ok()?;
    let key = u64::from_str_radix(&fingerprint, 16).ok()? as i64;
    Some((format!("{TEST_TEMPLATE_PREFIX}{fingerprint}"), key))
}

async fn ensure_migrated_template(maintenance_url: &str) -> Option<String> {
    if let Some(cached) = TEST_TEMPLATE.get() {
        return cached.clone();
    }
    let resolved = build_migrated_template(maintenance_url).await;
    // Racing callers all compute the same name and the advisory lock means only
    // one did any work; whichever result lands first is kept.
    let _ = TEST_TEMPLATE.set(resolved.clone());
    resolved
}

async fn build_migrated_template(maintenance_url: &str) -> Option<String> {
    use diesel_async::{AsyncConnection, RunQueryDsl};
    let Some((name, lock_key)) = test_template_identity() else {
        eprintln!("test template builder: could not fingerprint the embedded migrations");
        return None;
    };
    let mut conn = match AsyncPgConnection::establish(maintenance_url).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("test template builder: connect to maintenance db failed: {e}");
            return None;
        }
    };

    // Serializes every builder pointed at this server, across threads AND
    // processes — which is what keeps this correct under a process-per-test
    // runner as well as under `cargo test`. Session-scoped, so a builder that
    // panics or is killed releases it by disconnecting instead of wedging the
    // whole suite behind a lock nobody holds any more.
    if diesel::sql_query("SELECT pg_advisory_lock($1)")
        .bind::<diesel::sql_types::BigInt, _>(lock_key)
        .execute(&mut conn)
        .await
        .is_err()
    {
        return None;
    }

    let built = build_template_locked(maintenance_url, &name, &mut conn).await;

    let _ = diesel::sql_query("SELECT pg_advisory_unlock($1)")
        .bind::<diesel::sql_types::BigInt, _>(lock_key)
        .execute(&mut conn)
        .await;
    built
}

async fn build_template_locked(
    maintenance_url: &str,
    name: &str,
    conn: &mut AsyncPgConnection,
) -> Option<String> {
    use diesel_async::RunQueryDsl;
    if database_exists(conn, name).await {
        return Some(name.to_string());
    }

    // Migrate under a throwaway name and rename only on success, rather than
    // creating `name` and filling it in place. The difference shows up on a
    // crash: a builder killed midway would otherwise leave a half-migrated
    // database under the canonical name, and every later test would copy that
    // broken schema and fail somewhere far away from the cause. Here the
    // canonical name does not exist until it is complete, and the abandoned
    // scratch database is an ordinary `sauron_test_<ts>_<uuid>` that the
    // harnesses' reapers already collect.
    //
    // `_tb` rather than a readable `_tmplbuild`: `validate_db_ident` caps
    // identifiers at Postgres's 63-byte limit, and the readable spelling put
    // this name at exactly 64. It failed the guard before reaching the server,
    // every builder gave up, and the suite fell back to migrating per test —
    // still green, still correct, and 2x slower with nothing in the output
    // saying so. Hence the length check in the debug assertion below.
    let scratch = format!(
        "sauron_test_{}_tb{}",
        chrono::Utc::now().timestamp(),
        uuid::Uuid::new_v4().simple()
    );
    debug_assert!(
        scratch.len() <= 63,
        "scratch template name is {} bytes, over Postgres's 63-byte identifier limit: {scratch}",
        scratch.len()
    );
    if let Err(e) = create_database(maintenance_url, &scratch).await {
        // Loud on purpose. Every other failure here degrades to the old
        // migrate-per-test path, which is correct but silently costs roughly
        // double; without this line the only symptom is a slow suite.
        eprintln!("test template builder: creating {scratch} failed: {e}");
        return None;
    }
    let scratch_url = swap_database_url(maintenance_url, &scratch);
    if let Err(e) = run_pending_migrations(&scratch_url).await {
        eprintln!("test template builder: migrating {scratch} failed: {e}");
        let _ = drop_database(maintenance_url, &scratch).await;
        return None;
    }

    // `ALTER DATABASE ... RENAME` refuses while any session is connected. The
    // migration runner's own connection is finished by now, but a socket
    // closing and the backend exiting are not the same instant, and the
    // resulting error would read as a schema problem rather than a timing one.
    let _ = diesel::sql_query(
        "SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
         WHERE datname = $1 AND pid <> pg_backend_pid()",
    )
    .bind::<diesel::sql_types::Text, _>(scratch.clone())
    .execute(conn)
    .await;

    match rename_database(maintenance_url, &scratch, name).await {
        Ok(()) => Some(name.to_string()),
        Err(e) => {
            let _ = drop_database(maintenance_url, &scratch).await;
            // A builder in another process outside this server's lock (a second
            // checkout, say) may have taken the name first. That is a success
            // for us: the caller needs a usable template, not authorship of it.
            if database_exists(conn, name).await {
                Some(name.to_string())
            } else {
                eprintln!("test template builder: renaming {scratch} -> {name} failed: {e}");
                None
            }
        }
    }
}

async fn database_exists(conn: &mut AsyncPgConnection, name: &str) -> bool {
    use diesel_async::RunQueryDsl;
    #[derive(diesel::QueryableByName)]
    struct Count {
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        n: i64,
    }
    diesel::sql_query("SELECT count(*) AS n FROM pg_database WHERE datname = $1")
        .bind::<diesel::sql_types::Text, _>(name.to_string())
        .get_result::<Count>(conn)
        .await
        .map(|c| c.n > 0)
        .unwrap_or(false)
}

/// `url` with its database (path) segment replaced by `new_db`, preserving
/// scheme, authority and any `?query`.
fn swap_database_url(url: &str, new_db: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let auth_end = rest.find(['/', '?']).unwrap_or(rest.len());
    let authority = &rest[..auth_end];
    let after = &rest[auth_end..];
    let query = after.find('?').map(|i| &after[i..]).unwrap_or("");
    format!("{scheme}://{authority}/{new_db}{query}")
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
