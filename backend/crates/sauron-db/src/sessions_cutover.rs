//! The deferred half of migration 0073: copy `sessions_old_73` into the
//! partitioned `sessions`, one day per transaction, then drop it.
//!
//! Migration 0073 is schema-only because its first shape — copy everything,
//! one transaction — exhausted the Postgres lock table on the first
//! production upgrade (one daily partition is ~12 locked relations, and
//! managed Postgres may not allow raising `max_locks_per_transaction` at
//! all). This module is the other half of that trade: each day commits a
//! partition + copy + delete as one small transaction (~15 locks), so an
//! interruption at ANY point loses nothing — the next run re-lists the
//! distinct days still in the old table and continues. The old table is
//! dropped only when it is verifiably empty.
//!
//! Run it BEFORE opening traffic (`sauron-migrate finish-sessions-partitioning`,
//! see SETUP.md): until it completes, session-scoped reads see only rows
//! written after the migration, and live writes to a mid-copy day would race
//! the copy. The `ON CONFLICT DO NOTHING` below is the belt for that race —
//! the live (newer) row wins and the stale pre-cutover copy is discarded —
//! not a licence to run it under load.

use chrono::{Duration, NaiveDate};
use diesel::sql_types::{BigInt, Date};
use diesel_async::{AsyncPgConnection, RunQueryDsl, SimpleAsyncConnection};

/// Storage options for a sessions day partition. Keep in lockstep with
/// migration 0073 and `rollups::fold::ensure_session_partitions`.
const PART_OPTS: &str =
    "autovacuum_vacuum_scale_factor = 0.0, autovacuum_vacuum_threshold = 5000, \
     autovacuum_analyze_scale_factor = 0.0, autovacuum_analyze_threshold = 5000";

pub enum FinishOutcome {
    /// `sessions_old_73` no longer exists — the cutover already completed
    /// (or this deployment ran an earlier, all-in-one shape of 0073).
    AlreadyDone,
    /// Old table fully drained and dropped.
    Finished { days: usize, rows: u64 },
}

#[derive(diesel::QueryableByName)]
struct CountRow {
    #[diesel(sql_type = BigInt)]
    n: i64,
}

#[derive(diesel::QueryableByName)]
struct DayRow {
    #[diesel(sql_type = Date)]
    day: NaiveDate,
}

pub async fn finish_sessions_partitioning(
    conn: &mut AsyncPgConnection,
    mut progress: impl FnMut(NaiveDate, u64),
) -> diesel::QueryResult<FinishOutcome> {
    let present: CountRow = diesel::sql_query(
        "SELECT count(*)::bigint AS n FROM pg_class WHERE relname = 'sessions_old_73'",
    )
    .get_result(conn)
    .await?;
    if present.n == 0 {
        return Ok(FinishOutcome::AlreadyDone);
    }

    // Distinct days, oldest first. Computed once: the loop only ever REMOVES
    // days from the old table, so a stale list can only make a later run's
    // list shorter, never wrong.
    let days: Vec<DayRow> = diesel::sql_query(
        "SELECT DISTINCT (started_at AT TIME ZONE 'UTC')::date AS day \
         FROM sessions_old_73 ORDER BY 1",
    )
    .get_results(conn)
    .await?;

    let n_days = days.len();
    let mut total = 0u64;
    for d in days {
        let day = d.day;
        let lo = format!("{day} 00:00:00+00");
        let hi = format!("{} 00:00:00+00", day + Duration::days(1));
        let part = format!("sessions_{}", day.format("%Y_%m_%d"));
        // Partition + copy + delete commit TOGETHER: the day is either fully
        // moved or fully still in the old table, which is the entire
        // resumability contract. Explicit BEGIN/COMMIT via batch_execute —
        // the house pattern (`repo::bump_session`) — so the two data
        // statements can still report row counts through diesel.
        conn.batch_execute("BEGIN").await?;
        let out = async {
            // Rows for this day may already sit PARKED IN DEFAULT — a live
            // write that arrived before this day had a partition. Postgres
            // refuses to create a partition whose range is occupied by
            // default-partition rows ("would be violated by some row" — hit
            // in the smoke drive), so evict them into the old table first,
            // inside this same transaction. On (app_id, session_id) collision
            // the parked row wins: it is the LIVE, post-cutover state and the
            // old row is its stale snapshot.
            diesel::sql_query(format!(
                "WITH moved AS ( \
                     DELETE FROM sessions_default \
                     WHERE started_at >= '{lo}' AND started_at < '{hi}' \
                     RETURNING * \
                 ) \
                 INSERT INTO sessions_old_73 SELECT * FROM moved \
                 ON CONFLICT (app_id, session_id) DO UPDATE SET \
                     id = EXCLUDED.id, \
                     distinct_id = EXCLUDED.distinct_id, \
                     device_key = EXCLUDED.device_key, \
                     started_at = EXCLUDED.started_at, \
                     last_event_at = EXCLUDED.last_event_at, \
                     events_count = EXCLUDED.events_count, \
                     errors_count = EXCLUDED.errors_count, \
                     context = EXCLUDED.context, \
                     release = EXCLUDED.release, \
                     environment_id = EXCLUDED.environment_id, \
                     ip_address = EXCLUDED.ip_address, \
                     created_at = EXCLUDED.created_at, \
                     updated_at = EXCLUDED.updated_at, \
                     unhandled_errors_count = EXCLUDED.unhandled_errors_count"
            ))
            .execute(conn)
            .await?;
            diesel::sql_query(format!(
                "CREATE TABLE IF NOT EXISTS {part} PARTITION OF sessions \
                 FOR VALUES FROM ('{lo}') TO ('{hi}') WITH ({PART_OPTS})"
            ))
            .execute(conn)
            .await?;
            // The live row wins on conflict: anything already in the new
            // table for this key is newer than its pre-cutover snapshot.
            let copied = diesel::sql_query(format!(
                "INSERT INTO sessions SELECT * FROM sessions_old_73 \
                 WHERE started_at >= '{lo}' AND started_at < '{hi}' \
                 ON CONFLICT (app_id, session_id, started_at) DO NOTHING"
            ))
            .execute(conn)
            .await?;
            diesel::sql_query(format!(
                "DELETE FROM sessions_old_73 \
                 WHERE started_at >= '{lo}' AND started_at < '{hi}'"
            ))
            .execute(conn)
            .await?;
            Ok(copied as u64)
        }
        .await;
        match out {
            Ok(copied) => {
                conn.batch_execute("COMMIT").await?;
                total += copied;
                progress(day, copied);
            }
            Err(e) => {
                let _ = conn.batch_execute("ROLLBACK").await;
                return Err(e);
            }
        }
    }

    // Every distinct day was copied-and-deleted; anything left would mean new
    // writes reached the OLD table, which nothing references — a bug, not a
    // race. Refuse to drop rather than lose it.
    let left: CountRow = diesel::sql_query("SELECT count(*)::bigint AS n FROM sessions_old_73")
        .get_result(conn)
        .await?;
    if left.n != 0 {
        return Err(diesel::result::Error::QueryBuilderError(
            format!(
                "sessions_old_73 still holds {} rows after draining every \
                 distinct day; refusing to drop it",
                left.n
            )
            .into(),
        ));
    }
    conn.batch_execute("DROP TABLE sessions_old_73").await?;
    Ok(FinishOutcome::Finished {
        days: n_days,
        rows: total,
    })
}
