//! Populate `event_user_environments` for data that predates the rollup.
//!
//! Not part of a migration, and not part of `sauron-migrate`'s default no-arg
//! path, both on purpose: `require_current_schema` fail-closes the API on a
//! stale schema, and every RPM daemon `Requires=` the migrator unit, so anything
//! slow in either place is a boot outage proportional to retained data.
//!
//! ## Additive against a cutoff, NOT `ON CONFLICT DO NOTHING`
//!
//! The write path bumps this table from the moment migration 56 lands,
//! including for apps that are not yet backfilled, so a live bump can create a
//! row before the backfill reaches that person. `DO NOTHING` would then skip it
//! and leave that person short by their entire history — silently, and
//! permanently. Instead this aggregates only rows strictly before `cutoff` and
//! ADDS them to whatever is there; live bumps carry signals at or after
//! `cutoff`, so the two sets are disjoint and the addition is exact.
//!
//! That disjointness is a property of the CUTOFF, not of this SQL: it holds
//! only when the cutoff is the instant the live path started counting. See
//! [`rollup_epoch`] for where that instant comes from and why it is not
//! `Utc::now()`.
//!
//! KNOWN RESIDUAL: a backdated event — an SDK offline queue replaying with an
//! old `occurred_at` — that arrives between `cutoff` and the backfill finishing
//! is counted twice. Bounded by the backfill's duration, and counter drift is
//! already an accepted property of this table (the same trade `devices` makes).

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
/// is the only thing standing between this design and a silently empty persons
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
    // One UNION ALL over the three signal tables, grouped once. The three legs
    // mirror `repo::event_user_membership_exists`' three legs exactly — anyone
    // with a row in ANY of them qualifies — so the rollup admits precisely the
    // people the live query admits. Drop a leg here and an identity whose only
    // signal is a session (or an error) silently disappears from the Users
    // Explorer the moment its app is marked backfilled.
    let n = diesel::sql_query(
        "INSERT INTO event_user_environments \
           (app_id, distinct_id, environment_id, first_seen, last_seen, \
            events_count, errors_count, sessions_count) \
         SELECT app_id, distinct_id, environment_id, \
                min(first_at), max(last_at), sum(ev), sum(er), sum(se) \
         FROM ( \
             SELECT app_id, distinct_id, environment_id, occurred_at AS first_at, \
                    occurred_at AS last_at, 1::bigint AS ev, 0::bigint AS er, \
                    0::bigint AS se \
             FROM analytics_events \
             WHERE app_id=$1 AND occurred_at < $2 AND distinct_id <> '' \
             UNION ALL \
             SELECT app_id, distinct_id, environment_id, occurred_at, occurred_at, \
                    0::bigint, 1::bigint, 0::bigint \
             FROM error_events \
             WHERE app_id=$1 AND occurred_at < $2 AND distinct_id <> '' \
             UNION ALL \
             SELECT app_id, distinct_id, environment_id, started_at, last_event_at, \
                    0::bigint, 0::bigint, 1::bigint \
             FROM sessions \
             WHERE app_id=$1 AND started_at < $2 \
               AND distinct_id IS NOT NULL AND distinct_id <> '' \
         ) t \
         GROUP BY app_id, distinct_id, environment_id \
         ON CONFLICT (app_id, distinct_id, \
                      COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid)) \
         DO UPDATE SET \
            first_seen = LEAST(event_user_environments.first_seen, EXCLUDED.first_seen), \
            last_seen = GREATEST(event_user_environments.last_seen, EXCLUDED.last_seen), \
            events_count = event_user_environments.events_count + EXCLUDED.events_count, \
            errors_count = event_user_environments.errors_count + EXCLUDED.errors_count, \
            sessions_count = event_user_environments.sessions_count + EXCLUDED.sessions_count, \
            updated_at = now()",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Timestamptz, _>(cutoff)
    .execute(conn)
    .await?;

    diesel::sql_query(
        "INSERT INTO event_user_env_backfill (app_id, completed_at) VALUES ($1, now()) \
         ON CONFLICT (app_id) DO UPDATE SET completed_at = now()",
    )
    .bind::<SqlUuid, _>(app_id)
    .execute(conn)
    .await?;

    Ok(n)
}

/// The moment the live write path's counts became the only thing in
/// `event_user_environments` — i.e. when migration 70 applied.
///
/// [`backfill_app`] must only ever aggregate signals strictly BEFORE this, or
/// it re-counts what `batch::bump_person_envs` has already counted since.
///
/// Read from `event_user_env_rollup_epoch`, a one-row table migration 70
/// stamps with `now()` at apply time. Deliberately NOT
/// `__diesel_schema_migrations.run_on` (diesel declares that column a naive
/// `Timestamp`, whose UTC meaning depends on the session `TimeZone` in effect
/// when the migration ran) and NOT `Utc::now()` called from here — that is
/// precisely the defect this replaces.
///
/// Note what migration 70 is NOT: the instant the live path started writing.
/// That was migration 56, and it stamped nothing recoverable. 70 makes its own
/// stamp true instead, by deleting the rows accumulated before it for every app
/// that has no backfill marker — apps for which nothing reads this table yet.
/// Its `up.sql` carries the full argument.
pub async fn rollup_epoch(conn: &mut AsyncPgConnection) -> QueryResult<DateTime<Utc>> {
    #[derive(QueryableByName)]
    struct Epoch {
        #[diesel(sql_type = Timestamptz)]
        started_at: DateTime<Utc>,
    }
    let r: Epoch = diesel::sql_query("SELECT started_at FROM event_user_env_rollup_epoch")
        .get_result(conn)
        .await?;
    Ok(r.started_at)
}

/// Whether `repo::list_persons` may read the rollup for this app.
pub async fn is_backfilled(conn: &mut AsyncPgConnection, app_id: Uuid) -> QueryResult<bool> {
    #[derive(QueryableByName)]
    struct Present {
        #[diesel(sql_type = diesel::sql_types::Bool)]
        present: bool,
    }
    let r: Present = diesel::sql_query(
        "SELECT EXISTS (SELECT 1 FROM event_user_env_backfill WHERE app_id=$1) AS present",
    )
    .bind::<SqlUuid, _>(app_id)
    .get_result(conn)
    .await?;
    Ok(r.present)
}

/// Backfill every app that has no marker yet, one app per transaction.
///
/// One app at a time rather than one statement for everything: a single
/// transaction over every app's history would hold locks for its whole duration
/// and lose all progress on any failure.
///
/// The cutoff is [`rollup_epoch`], read ONCE before the loop — not `Utc::now()`,
/// and not re-read per app. It is a property of the DEPLOYMENT (when this
/// table's contents became live-path-only), not of any one app, and not of how
/// long an earlier app in this loop happened to take.
pub async fn backfill_all(pool: &PgPool) -> anyhow::Result<()> {
    let mut conn = crate::conn(pool).await?;
    let cutoff = rollup_epoch(&mut conn).await?;

    #[derive(QueryableByName)]
    struct AppId {
        #[diesel(sql_type = SqlUuid)]
        id: Uuid,
    }
    let apps: Vec<AppId> = diesel::sql_query(
        "SELECT id FROM apps WHERE id NOT IN (SELECT app_id FROM event_user_env_backfill) \
         ORDER BY id",
    )
    .get_results(&mut conn)
    .await?;

    tracing::info!(apps = apps.len(), "person/environment backfill starting");
    for a in apps {
        let n = backfill_app(&mut conn, a.id, cutoff).await?;
        tracing::info!(app_id = %a.id, rows = n, "person/environment backfill done");
    }
    tracing::info!("person/environment backfill complete");
    Ok(())
}
