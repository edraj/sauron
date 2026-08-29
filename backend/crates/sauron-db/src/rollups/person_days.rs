//! Per-person, per-day activity — retention's substrate.
//!
//! One row per (app, environment, person, day), exact rather than sketched.
//! `user_activity_daily` next door stores HyperLogLog, which UNIONS but does
//! not INTERSECT, and retention is precisely an intersection: who was in cohort
//! C *and* also active in period N. No accuracy setting makes HLL answer that,
//! so this table exists.
//!
//! Unlike [`super::add_user_activity`], the write here needs no
//! read-modify-write round trip: there is no sketch to merge in Rust, so it is
//! a plain additive upsert in the shape of [`super::add_event_top`]. That
//! matters — this is the one rollup whose write volume scales with users rather
//! than with buckets, so a per-row `SELECT … FOR UPDATE` would be felt.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use diesel::sql_types::{
    Array, BigInt, Date, Integer, Nullable, Text, Timestamptz, Uuid as SqlUuid,
};
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use uuid::Uuid;

use super::{DayKey, CHUNK};

/// `((app, env, day), distinct_id)`.
pub type PersonKey = (DayKey, String);

#[derive(Default, Clone)]
pub struct PersonDayDelta {
    pub events: i64,
    pub errors: i64,
}

/// Additive upsert.
///
/// The `ON CONFLICT` target must name the same `COALESCE(environment_id, nil)`
/// expression the unique index is built over; a bare column list there silently
/// degrades into an unconstrained insert and the table grows duplicate rows
/// that every `count(DISTINCT …)` downstream then has to paper over.
///
/// Collapsing same-day collisions onto one row is not just a storage nicety —
/// it is what makes an identity merge a SET UNION of days rather than a double
/// count.
pub async fn add_person_days(
    conn: &mut AsyncPgConnection,
    deltas: &BTreeMap<PersonKey, PersonDayDelta>,
) -> diesel::QueryResult<()> {
    for chunk in deltas.iter().collect::<Vec<_>>().chunks(CHUNK) {
        diesel::sql_query(
            "INSERT INTO person_days (app_id, environment_id, distinct_id, day, events, errors) \
             SELECT app_id, env, distinct_id, day, events, errors \
             FROM unnest($1::uuid[], $2::uuid[], $3::text[], $4::date[], $5::bigint[], $6::bigint[]) \
                  AS t(app_id, env, distinct_id, day, events, errors) \
             ON CONFLICT (app_id, COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid), distinct_id, day) \
             DO UPDATE SET events = person_days.events + EXCLUDED.events, \
                           errors = person_days.errors + EXCLUDED.errors, \
                           updated_at = now()",
        )
        .bind::<Array<SqlUuid>, _>(chunk.iter().map(|((k, _), _)| k.0).collect::<Vec<_>>())
        .bind::<Array<Nullable<SqlUuid>>, _>(
            chunk.iter().map(|((k, _), _)| k.1).collect::<Vec<_>>(),
        )
        .bind::<Array<Text>, _>(chunk.iter().map(|((_, d), _)| d.clone()).collect::<Vec<_>>())
        .bind::<Array<Date>, _>(chunk.iter().map(|((k, _), _)| k.2).collect::<Vec<_>>())
        .bind::<Array<BigInt>, _>(chunk.iter().map(|(_, v)| v.events).collect::<Vec<_>>())
        .bind::<Array<BigInt>, _>(chunk.iter().map(|(_, v)| v.errors).collect::<Vec<_>>())
        .execute(conn)
        .await?;
    }
    Ok(())
}

#[derive(diesel::QueryableByName)]
struct BoolRow {
    #[diesel(sql_type = diesel::sql_types::Bool)]
    present: bool,
}

#[derive(diesel::QueryableByName)]
struct TsRow {
    #[diesel(sql_type = Timestamptz)]
    t: DateTime<Utc>,
}

/// [`super::is_ready`]'s twin, over THIS table's own marker.
///
/// Deliberately not `rollups::is_ready`: the markers in `rollup_backfill` were
/// written by migration-71 backfills that never touched `person_days`, because
/// the table did not exist. Reusing them reports READY for an app whose
/// person-days are empty, and the API then answers 0% retention — confidently,
/// which is worse than an error because it looks like an answer.
///
/// An app created at-or-after this table's epoch is implicitly ready: every row
/// it will ever have is post-epoch and therefore folded live.
pub async fn is_ready(conn: &mut AsyncPgConnection, app_id: Uuid) -> diesel::QueryResult<bool> {
    let r: BoolRow = diesel::sql_query(
        "SELECT EXISTS (SELECT 1 FROM person_days_backfill WHERE app_id = $1) \
             OR EXISTS (SELECT 1 FROM apps a, person_days_epoch e \
                        WHERE a.id = $1 AND a.created_at >= e.started_at) AS present",
    )
    .bind::<SqlUuid, _>(app_id)
    .get_result(conn)
    .await?;
    Ok(r.present)
}

/// Marker write for every app that exists right now. Called by the backfill
/// inside its final transaction — the marker must never be visible before the
/// rows it claims (the `device_env_backfill:88` rule).
pub async fn mark_all_backfilled(conn: &mut AsyncPgConnection) -> diesel::QueryResult<usize> {
    diesel::sql_query(
        "INSERT INTO person_days_backfill (app_id) SELECT id FROM apps \
         ON CONFLICT (app_id) DO UPDATE SET completed_at = now()",
    )
    .execute(conn)
    .await
}

/// The instant the live fold started counting for this table.
///
/// The backfill's cutoff, and the reason it is not `Utc::now()`: the two halves
/// of history are disjoint only when the cutoff is exactly this instant.
pub async fn epoch(conn: &mut AsyncPgConnection) -> diesel::QueryResult<DateTime<Utc>> {
    let r: TsRow = diesel::sql_query("SELECT started_at AS t FROM person_days_epoch")
        .get_result(conn)
        .await?;
    Ok(r.t)
}

/// Longest horizon this table is kept for.
///
/// Matches `MAX_TIMESERIES_DAYS` — the longest window any endpoint can answer
/// over — so pruning can never drop a day a query could still ask about.
pub const MAX_KEEP_DAYS: i32 = 400;

/// Drop person-days past the horizon.
pub async fn prune(conn: &mut AsyncPgConnection, keep_days: i32) -> diesel::QueryResult<usize> {
    diesel::sql_query(
        "DELETE FROM person_days WHERE day < current_date - make_interval(days => $1::int)",
    )
    .bind::<Integer, _>(keep_days.clamp(1, MAX_KEEP_DAYS))
    .execute(conn)
    .await
}
