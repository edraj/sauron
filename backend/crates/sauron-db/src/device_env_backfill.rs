//! Populate `device_environments` for data that predates the rollup.
//!
//! The device twin of [`crate::person_env_backfill`], and identical in shape —
//! read that module's header for the full reasoning. The short version:
//!
//! Not part of a migration, and not part of `sauron-migrate`'s default no-arg
//! path, both on purpose: `require_current_schema` fail-closes the API on a
//! stale schema, and every RPM daemon `Requires=` the migrator unit, so anything
//! slow in either place is a boot outage proportional to retained data.
//!
//! ## Additive against a cutoff, NOT `ON CONFLICT DO NOTHING`
//!
//! The write path bumps this table from the moment migration 59 lands,
//! including for apps that are not yet backfilled, so a live bump can create a
//! row before the backfill reaches that device. `DO NOTHING` would then skip it
//! and leave that device short by its entire history — silently, and
//! permanently. Instead this aggregates only rows strictly before `cutoff` and
//! ADDS them to whatever is there; live bumps carry signals at or after
//! `cutoff`, so the two sets are disjoint and the addition is exact —
//! PROVIDED `cutoff` is at or before [`rollup_epoch`], the instant the live
//! write path actually started counting. [`backfill_app`] takes `cutoff` as a
//! bare parameter and trusts its caller for this; [`backfill_all`], the one
//! production caller, upholds it by reading [`rollup_epoch`] once rather than
//! calling `Utc::now()` — see that function's own doc comment for the bug
//! this replaced (a live-counted signal re-aggregated a second time).
//!
//! KNOWN RESIDUAL: a backdated event — an SDK offline queue replaying with an
//! old `occurred_at` — that arrives AFTER the epoch (so the live write path
//! counts it) but carries a timestamp BEFORE the epoch (so the backfill's
//! `occurred_at < cutoff` aggregate counts it too) is counted twice. This is
//! no longer "bounded by the backfill's duration" — reading the epoch instead
//! of `Utc::now()` closed that window entirely — it is bounded only by how
//! late an offline queue can replay, which is genuinely small. Counter drift
//! of this kind is already an accepted property of this table (the same
//! trade `devices` makes).
//!
//! DELIBERATE DIVERGENCE from [`crate::person_env_backfill`]: that module's
//! `backfill_all` still computes `cutoff = Utc::now()` per app and carries
//! this exact double-counting defect (a live bump landing between migration
//! 56 applying and an operator running that backfill gets counted twice —
//! once by the write path, once by the re-aggregate). Not fixed here:
//! `person_env_backfill.rs` is out of scope for this task and is deliberately
//! left untouched, so this asymmetry between the two modules is intentional,
//! not an oversight.

use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel::sql_types::{Timestamptz, Uuid as SqlUuid};
use diesel_async::{AsyncPgConnection, RunQueryDsl, SimpleAsyncConnection};
use uuid::Uuid;

use crate::PgPool;

/// Aggregate one app's pre-`cutoff` history into the rollup and mark it done.
///
/// The marker insert shares this function's transaction with the aggregate, so
/// the marker can never become visible before the data it claims. That ordering
/// is the only thing standing between this design and a silently empty Devices
/// page, so it is a transaction rather than two statements.
pub async fn backfill_app(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    cutoff: DateTime<Utc>,
) -> QueryResult<usize> {
    // Explicit BEGIN/COMMIT rather than `conn.transaction(|c| …)`: diesel-async
    // 0.9's closure signature needs async closures, which would push the
    // workspace MSRV past the 1.82 the RPM spec builds against. Same reasoning
    // as `batch::write_rows_once`.
    conn.batch_execute("BEGIN").await?;
    match backfill_app_inner(conn, app_id, cutoff).await {
        Ok(n) => {
            conn.batch_execute("COMMIT").await?;
            Ok(n)
        }
        Err(e) => {
            // Best-effort: if the ROLLBACK itself fails the connection is
            // already unusable and the pool discards it on return, which aborts
            // the transaction anyway.
            let _ = conn.batch_execute("ROLLBACK").await;
            Err(e)
        }
    }
}

async fn backfill_app_inner(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    cutoff: DateTime<Utc>,
) -> QueryResult<usize> {
    // One UNION ALL over the four signal tables, grouped once. The four legs
    // mirror `repo::device_membership_sql`'s four legs exactly — any device with
    // a row in ANY of them qualifies — so the rollup admits precisely the
    // devices the live query admits. Drop a leg here and a device whose only
    // signal is a session (or an error, or a transaction) silently disappears
    // from the Devices inventory the moment its app is marked backfilled.
    //
    // NOTE the deliberate difference from `device_membership_sql`: that
    // predicate bounds its sessions leg by the page's `since`, because it is
    // deciding which devices to LIST in a window. This is deciding what the
    // rollup CONTAINS, which has no window — the `since` filter still applies
    // later, against `devices.last_seen`, exactly as it does today. The
    // transactions leg carries no time bound in either function — see
    // `device_membership_sql`'s doc comment for why.
    //
    // The transactions leg's three counters are all `0`: a transaction
    // contributes membership and timestamps, never counts — matching exactly
    // what the write path folds (`sauron-pipeline`'s `Acc::rollup`, called for
    // a transaction with `0,0` event/error deltas and no sessions delta at
    // all). Summing anything else here would silently disagree with the
    // number the live write path has been recording since migration 59.
    let n = diesel::sql_query(
        "INSERT INTO device_environments \
           (app_id, device_key, environment_id, first_seen, last_seen, \
            events_count, errors_count, sessions_count) \
         SELECT app_id, device_key, environment_id, \
                min(first_at), max(last_at), sum(ev), sum(er), sum(se) \
         FROM ( \
             SELECT app_id, device_key, environment_id, occurred_at AS first_at, \
                    occurred_at AS last_at, 1::bigint AS ev, 0::bigint AS er, \
                    0::bigint AS se \
             FROM analytics_events \
             WHERE app_id=$1 AND occurred_at < $2 \
               AND device_key IS NOT NULL AND device_key <> '' \
             UNION ALL \
             SELECT app_id, device_key, environment_id, occurred_at, occurred_at, \
                    0::bigint, 1::bigint, 0::bigint \
             FROM error_events \
             WHERE app_id=$1 AND occurred_at < $2 \
               AND device_key IS NOT NULL AND device_key <> '' \
             UNION ALL \
             SELECT app_id, device_key, environment_id, started_at, last_event_at, \
                    0::bigint, 0::bigint, 1::bigint \
             FROM sessions \
             WHERE app_id=$1 AND started_at < $2 \
               AND device_key IS NOT NULL AND device_key <> '' \
             UNION ALL \
             SELECT app_id, device_key, environment_id, occurred_at, occurred_at, \
                    0::bigint, 0::bigint, 0::bigint \
             FROM transactions \
             WHERE app_id=$1 AND occurred_at < $2 \
               AND device_key IS NOT NULL AND device_key <> '' \
         ) t \
         GROUP BY app_id, device_key, environment_id \
         ON CONFLICT (app_id, device_key, \
                      COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid)) \
         DO UPDATE SET \
            first_seen = LEAST(device_environments.first_seen, EXCLUDED.first_seen), \
            last_seen = GREATEST(device_environments.last_seen, EXCLUDED.last_seen), \
            events_count = device_environments.events_count + EXCLUDED.events_count, \
            errors_count = device_environments.errors_count + EXCLUDED.errors_count, \
            sessions_count = device_environments.sessions_count + EXCLUDED.sessions_count, \
            updated_at = now()",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Timestamptz, _>(cutoff)
    .execute(conn)
    .await?;

    diesel::sql_query(
        "INSERT INTO device_env_backfill (app_id, completed_at) VALUES ($1, now()) \
         ON CONFLICT (app_id) DO UPDATE SET completed_at = now()",
    )
    .bind::<SqlUuid, _>(app_id)
    .execute(conn)
    .await?;

    Ok(n)
}

/// Whether `repo::list_device_groups` may read the rollup for this app.
pub async fn is_backfilled(conn: &mut AsyncPgConnection, app_id: Uuid) -> QueryResult<bool> {
    #[derive(QueryableByName)]
    struct Present {
        #[diesel(sql_type = diesel::sql_types::Bool)]
        present: bool,
    }
    let r: Present = diesel::sql_query(
        "SELECT EXISTS (SELECT 1 FROM device_env_backfill WHERE app_id=$1) AS present",
    )
    .bind::<SqlUuid, _>(app_id)
    .get_result(conn)
    .await?;
    Ok(r.present)
}

/// The moment the live write path began maintaining `device_environments` —
/// i.e. when migration 59 applied. `bump_device_envs` has counted every
/// signal at or after this instant; the backfill must therefore only ever
/// aggregate signals strictly BEFORE it, or the two sets overlap and a
/// signal ingested in between gets counted twice — once by the live path,
/// once by the backfill re-deriving it from the raw signal tables.
///
/// Read from `device_env_rollup_epoch`, a one-row table migration 59 stamps
/// with `now()` at apply time. Deliberately NOT
/// `__diesel_schema_migrations.run_on` (diesel declares that column a naive
/// `Timestamp`, whose UTC meaning depends on the session `TimeZone` in effect
/// when the migration ran) and NOT `Utc::now()` called from here (which is
/// exactly the bug this function replaces — see the module header).
pub async fn rollup_epoch(conn: &mut AsyncPgConnection) -> QueryResult<DateTime<Utc>> {
    #[derive(QueryableByName)]
    struct Epoch {
        #[diesel(sql_type = Timestamptz)]
        started_at: DateTime<Utc>,
    }
    let r: Epoch = diesel::sql_query("SELECT started_at FROM device_env_rollup_epoch")
        .get_result(conn)
        .await?;
    Ok(r.started_at)
}

/// Backfill every app that has no marker yet, one app per transaction.
///
/// One app at a time rather than one statement for everything: a single
/// transaction over every app's history would hold locks for its whole duration
/// and lose all progress on any failure.
///
/// The cutoff is [`rollup_epoch`], read ONCE before the loop — not
/// `Utc::now()`, and not re-read per app. It is a property of the deployment
/// (when the live write path started counting), not of any one app, and not
/// of how long an earlier app in this loop happened to take: re-deriving it
/// per app, or from `now()` at all, is exactly the defect that let a signal
/// ingested between the migration landing and this function running be
/// counted twice — once by the live path, once by the backfill re-aggregating
/// it. Measured: a live-bumped signal 30 minutes old came back as 2, not 1,
/// under the old `Utc::now()` cutoff.
pub async fn backfill_all(pool: &PgPool) -> anyhow::Result<()> {
    let mut conn = crate::conn(pool).await?;

    let cutoff = rollup_epoch(&mut conn).await?;

    #[derive(QueryableByName)]
    struct AppId {
        #[diesel(sql_type = SqlUuid)]
        id: Uuid,
    }
    let apps: Vec<AppId> = diesel::sql_query(
        "SELECT id FROM apps WHERE id NOT IN (SELECT app_id FROM device_env_backfill) \
         ORDER BY id",
    )
    .get_results(&mut conn)
    .await?;

    tracing::info!(apps = apps.len(), "device/environment backfill starting");
    for a in apps {
        let n = backfill_app(&mut conn, a.id, cutoff).await?;
        tracing::info!(app_id = %a.id, rows = n, "device/environment backfill done");
    }
    tracing::info!("device/environment backfill complete");
    Ok(())
}
