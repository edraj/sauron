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
    let mut conn = crate::conn(pool).await?;
    let cutoff = crate::rollups::person_days::epoch(&mut conn).await?;

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

    // The marker LAST, and only once both tables have landed: it must never be
    // visible before the rows it claims (the `device_env_backfill:88` rule).
    // Until it exists the API reports `ready: false` and the dashboard names
    // this command, which is strictly better than an empty grid that looks like
    // an answer.
    mark_all_backfilled(&mut conn).await?;
    tracing::info!("person-days backfill complete");
    Ok(())
}
