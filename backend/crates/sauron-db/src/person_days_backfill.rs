//! Populate `person_days` for data that predates its epoch.
//!
//! Not part of a migration, and not part of `sauron-migrate`'s default no-arg
//! path, for the reason [`crate::person_env_backfill`] records:
//! `require_current_schema` fail-closes the API on a stale schema, and every
//! RPM daemon `Requires=` the migrator unit, so anything slow in either place
//! is a boot outage proportional to retained data.
//!
//! ## Additive against a cutoff, NOT `ON CONFLICT DO NOTHING`
//!
//! The live fold bumps `person_days` from the moment migration 74 lands,
//! including for apps this backfill has not reached yet, so a live bump can
//! create a row before the backfill gets to that person. `DO NOTHING` would
//! then skip it and leave that person short by their entire pre-epoch history —
//! silently, and permanently. This aggregates only rows strictly before the
//! cutoff and ADDS them; live bumps carry rows at or after the cutoff, so the
//! two sets are disjoint and the addition is exact.
//!
//! That disjointness is a property of the CUTOFF, not of this SQL. It holds
//! only when the cutoff is the instant the live path started counting, which is
//! why this reads [`crate::rollups::person_days::epoch`] and never `Utc::now()`.
//!
//! ## Known residual
//!
//! Inherited unchanged from `person_env_backfill`: a backdated event — an SDK
//! offline queue replaying with an old `occurred_at` — that arrives between the
//! cutoff and this finishing is counted twice. It is bounded by the backfill's
//! duration, and is disclosed rather than fixed, because closing it would mean
//! holding a lock across the whole backfill.

use diesel_async::RunQueryDsl;

use crate::rollups::person_days::mark_all_backfilled;

/// Aggregate every pre-cutoff signal into `person_days`, then mark every app
/// ready.
///
/// The selection is by `received_at` — the same clock the fold's watermark
/// advances on, so the two halves partition the firehose — while the BUCKET is
/// `occurred_at`, so a late-arriving event still lands in its correct
/// historical day.
pub async fn backfill_all(pool: &crate::PgPool) -> anyhow::Result<()> {
    use diesel_async::SimpleAsyncConnection;
    let mut conn = crate::conn(pool).await?;
    if !backfill_pending(&mut conn).await? {
        tracing::info!("person-days backfill: every app already marked; nothing to add");
        return Ok(());
    }
    let cutoff = crate::rollups::person_days::epoch(&mut conn).await?;
    // ONE transaction for both tables and the marker: the additive upsert
    // cannot be re-run safely, so a failure between the two INSERTs must roll
    // both back rather than leave analytics counted and errors not — which a
    // retry would then double. All-or-nothing is what makes an unattended
    // re-run (the ingest service's automatic backfill) correct.
    conn.batch_execute("BEGIN").await?;
    let out: anyhow::Result<()> = async {
        for (table, col) in [("analytics_events", "events"), ("error_events", "errors")] {
            let sql = format!(
                "INSERT INTO person_days (app_id, environment_id, distinct_id, day, {col}) \
                 SELECT app_id, environment_id, distinct_id, occurred_at::date, count(*) \
                   FROM {table} \
                  WHERE received_at < $1 AND distinct_id IS NOT NULL AND distinct_id <> '' \
                  GROUP BY app_id, environment_id, distinct_id, occurred_at::date \
                 ON CONFLICT (app_id, COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid), distinct_id, day) \
                 DO UPDATE SET {col} = person_days.{col} + EXCLUDED.{col}, updated_at = now()"
            );
            diesel::sql_query(sql)
                .bind::<diesel::sql_types::Timestamptz, _>(cutoff)
                .execute(&mut conn)
                .await?;
            tracing::info!(%table, "person-days backfill: table complete");
        }
        // The marker LAST, and only once both tables have landed: it must
        // never be visible before the rows it claims (the
        // `device_env_backfill:88` rule). Until it exists the API reports
        // `ready: false` and the dashboard says history is being built.
        mark_all_backfilled(&mut conn).await?;
        Ok(())
    }
    .await;
    match out {
        Ok(()) => {
            conn.batch_execute("COMMIT").await?;
            tracing::info!("person-days backfill complete");
            Ok(())
        }
        Err(e) => {
            let _ = conn.batch_execute("ROLLBACK").await;
            Err(e)
        }
    }
}

/// Whether any app still predates the `person_days` epoch without a marker —
/// i.e. whether [`backfill_all`] has work to do.
pub async fn backfill_pending(
    conn: &mut diesel_async::AsyncPgConnection,
) -> diesel::QueryResult<bool> {
    #[derive(diesel::QueryableByName)]
    struct Row {
        #[diesel(sql_type = diesel::sql_types::Bool)]
        present: bool,
    }
    let r: Row = diesel::sql_query(
        "SELECT EXISTS (SELECT 1 FROM apps a, person_days_epoch e \
                        WHERE a.created_at < e.started_at \
                          AND NOT EXISTS (SELECT 1 FROM person_days_backfill b WHERE b.app_id = a.id)) \
                AS present",
    )
    .get_result(conn)
    .await?;
    Ok(r.present)
}
