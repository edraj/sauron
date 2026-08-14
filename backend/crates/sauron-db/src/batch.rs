//! Set-at-a-time versions of the ingest write path.
//!
//! The per-item functions in [`crate::repo`] each issue one statement, and
//! diesel-async runs outside an explicit transaction, so every one of them is
//! its own round trip *and* its own commit. A single error event costs seven
//! (issue upsert, event insert, session bump, device bump, `users_seen`,
//! `touch_event_user`, identification) and an analytics event costs five. The
//! measured baseline was ~10 commits per envelope — the reason the worker
//! drained 23x slower than the edge accepted.
//!
//! Everything here collapses one *batch* of stream entries into a fixed handful
//! of statements instead. Rows travel as parallel arrays `unnest`ed back into a
//! rowset — the same idiom `repo::create_member_with_grants` already uses — so
//! the bind count is constant no matter how many rows are in flight.
//!
//! ## The dedupe rule
//!
//! `ON CONFLICT DO UPDATE` refuses to touch the same row twice within one
//! statement (`ON CONFLICT DO UPDATE command cannot affect row a second time`).
//! A batch routinely contains several signals for one session, device or
//! fingerprint, so **every caller must fold duplicates in memory before calling
//! these** — summing the counters rather than dropping them. The `*Bump` structs
//! below carry the folded totals; `crate::repo`'s single-row versions take a
//! delta of 1 because they cannot see their neighbours.
//!
//! ## The ordering rule
//!
//! Every multi-row upsert here sorts its rows by conflict key before binding,
//! and that is load-bearing, not tidiness. Postgres takes row locks in the
//! order a statement happens to process rows; two concurrent batches holding
//! overlapping key sets in *different* orders deadlock, and Postgres resolves
//! that by aborting one of them. Measured on an 8-worker ingest, this was not
//! a rare race — nearly every batch died with `deadlock detected` and fell back
//! to the per-item path, which made the batched write path roughly 4x SLOWER
//! than the one it replaced while looking, from the outside, like it worked.
//! Sorting gives every transaction the same global lock order, so no cycle can
//! form. Do not remove it, and add it to any new statement here.
//!
//! Sorting is necessary but not sufficient: `INSERT … ON CONFLICT` takes locks
//! in the order rows are supplied, but `UPDATE … FROM unnest(…)` takes them in
//! whatever order the planner scans the target table, which no amount of
//! sorting the input controls. So the transaction also RETRIES on deadlock,
//! which is what Postgres intends a caller to do with SQLSTATE 40P01 — the
//! loser of a deadlock has been rolled back cleanly and can simply go again.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use diesel::dsl::sql;
use diesel::prelude::*;
use diesel::sql_types::{
    Array, BigInt, Bool, Integer, Jsonb, Nullable, Text, Timestamptz, Uuid as SqlUuid,
};
use diesel::upsert::excluded;
use diesel_async::{AsyncPgConnection, RunQueryDsl, SimpleAsyncConnection};
use serde_json::Value;
use uuid::Uuid;

use crate::models::{NewAnalyticsEvent, NewErrorEvent, NewIssue, NewTransaction};
use crate::schema::*;

/// Group an error batch into issues, one statement.
///
/// Returns `(app_id, fingerprint, issue_id)` for every input row so the caller
/// can stamp `error_events.issue_id` without a second lookup.
///
/// `times_seen` is the one place this differs from [`crate::repo::upsert_issue`]:
/// the conflict arm adds `excluded.times_seen` rather than a literal `1`. The
/// single-row path always passes `times_seen: 1`, so the two agree there; the
/// batch path passes the folded occurrence count for the fingerprint. Every
/// other column, including the `GREATEST`/`LEAST` window, is identical — the
/// two paths must be interchangeable, since `INGEST_BATCH_WRITES=0` selects
/// between them at runtime.
pub async fn upsert_issues(
    conn: &mut AsyncPgConnection,
    rows: &[NewIssue<'_>],
) -> QueryResult<Vec<(Uuid, String, Uuid)>> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    // Sorted by `(app_id, fingerprint)` for the module's ordering rule. Held as
    // a `Vec<&NewIssue>` so the caller's slice is not disturbed.
    let mut sorted: Vec<&NewIssue<'_>> = rows.iter().collect();
    sorted.sort_unstable_by(|a, b| (a.app_id, a.fingerprint).cmp(&(b.app_id, b.fingerprint)));
    // Chunked for the same bind-parameter reason as the inserts below, and safe
    // to split because the rows were deduplicated by `(app_id, fingerprint)`
    // before they got here: no two chunks can contend for the same conflict
    // key, and the sort keeps the lock order consistent across chunks.
    let mut out = Vec::with_capacity(sorted.len());
    for chunk in sorted.chunks(INSERT_CHUNK) {
        out.extend(upsert_issues_chunk(conn, chunk.to_vec()).await?);
    }
    Ok(out)
}

async fn upsert_issues_chunk(
    conn: &mut AsyncPgConnection,
    chunk: Vec<&NewIssue<'_>>,
) -> QueryResult<Vec<(Uuid, String, Uuid)>> {
    diesel::insert_into(issues::table)
        .values(chunk)
        .on_conflict((issues::app_id, issues::fingerprint))
        .do_update()
        .set((
            // GREATEST/LEAST rather than a bare `excluded.*` overwrite, so the
            // stored window does not depend on the order occurrences happen to
            // be processed in. A bare overwrite let a late-arriving OLDER
            // occurrence drag `last_seen` backwards, and it is also what made
            // this statement disagree with the batched path (which folds a
            // whole batch before writing, and therefore has no processing order
            // to inherit). Both spellings now agree and both are order-free.
            issues::last_seen.eq(sql::<Timestamptz>(
                "GREATEST(issues.last_seen, excluded.last_seen)",
            )),
            issues::first_seen.eq(sql::<Timestamptz>(
                "LEAST(issues.first_seen, excluded.first_seen)",
            )),
            issues::times_seen.eq(issues::times_seen + excluded(issues::times_seen)),
            issues::level.eq(excluded(issues::level)),
            // Sticky mask guard, verbatim from the single-row path — see its
            // comment for why a '****' title is permanent.
            issues::title.eq(sql::<Text>(
                "CASE WHEN issues.title = '****' THEN issues.title ELSE excluded.title END",
            )),
            issues::culprit.eq(sql::<Text>(
                "CASE WHEN issues.culprit = '****' THEN issues.culprit ELSE excluded.culprit END",
            )),
            issues::updated_at.eq(Utc::now()),
            issues::last_event_at.eq(Utc::now()),
        ))
        .returning((issues::app_id, issues::fingerprint, issues::id))
        .get_results(conn)
        .await
}

/// Rows per multi-row `INSERT`.
///
/// Unlike the `unnest` statements in this module, `insert_into(…).values(&[..])`
/// binds every column of every row separately, and Postgres refuses a statement
/// with more than 65,535 bind parameters. Against the widest row here (~30
/// columns) that ceiling arrives at about 2,180 rows.
///
/// It used to be unreachable by construction: one stream entry was one item, so
/// `INGEST_BATCH_SIZE` — capped at 2000 — also capped the rows. **That coupling
/// broke when an entry became a whole envelope.** An entry may now carry up to
/// `MAX_ENVELOPE_ITEMS` (1000) items, so 200 entries is up to 200,000 items,
/// and an error-heavy batch would overflow, fail the statement, and drop the
/// whole batch onto the per-item fallback — slower, and with a memory spike, on
/// exactly the traffic that could least afford it.
///
/// Chunking here rather than capping the batch keeps the amortization the batch
/// exists for: the chunks run inside the caller's transaction, so this is still
/// one commit however many statements it takes. Batches below the chunk size —
/// every realistic one — still issue exactly one statement.
const INSERT_CHUNK: usize = 1_000;

pub async fn insert_error_events(
    conn: &mut AsyncPgConnection,
    rows: &[NewErrorEvent],
) -> QueryResult<usize> {
    let mut n = 0;
    for chunk in rows.chunks(INSERT_CHUNK) {
        n += diesel::insert_into(error_events::table)
            .values(chunk)
            .execute(conn)
            .await?;
    }
    Ok(n)
}

pub async fn insert_analytics_events(
    conn: &mut AsyncPgConnection,
    rows: &[NewAnalyticsEvent],
) -> QueryResult<usize> {
    let mut n = 0;
    for chunk in rows.chunks(INSERT_CHUNK) {
        n += diesel::insert_into(analytics_events::table)
            .values(chunk)
            .execute(conn)
            .await?;
    }
    Ok(n)
}

pub async fn insert_transactions(
    conn: &mut AsyncPgConnection,
    rows: &[NewTransaction],
) -> QueryResult<usize> {
    let mut n = 0;
    for chunk in rows.chunks(INSERT_CHUNK) {
        n += diesel::insert_into(transactions::table)
            .values(chunk)
            .execute(conn)
            .await?;
    }
    Ok(n)
}

/// One session's folded contribution from a batch. Fields mirror
/// [`crate::repo::bump_session`]'s arguments; the counters are totals.
#[derive(Debug, Clone)]
pub struct SessionBump {
    pub app_id: Uuid,
    pub session_id: String,
    pub distinct_id: Option<String>,
    pub device_key: Option<String>,
    /// Earliest and latest signal in the fold. Two fields rather than one
    /// because the conflict arm drives `started_at` through `LEAST` and
    /// `last_event_at` through `GREATEST`: collapsing a batch to a single
    /// timestamp would move `started_at` forward to the newest signal in the
    /// group, which N sequential single-row upserts would never do.
    pub first_at: DateTime<Utc>,
    pub last_at: DateTime<Utc>,
    pub context: Value,
    pub release: Option<String>,
    pub environment_id: Option<Uuid>,
    pub ip: Option<String>,
    pub events_delta: i64,
    pub errors_delta: i64,
}

/// One row of [`bump_sessions`]' `RETURNING`.
#[derive(QueryableByName)]
struct BumpedSession {
    #[diesel(sql_type = SqlUuid)]
    app_id: Uuid,
    #[diesel(sql_type = Text)]
    session_id: String,
    /// `xmax = 0` is true for exactly the rows this statement INSERTED. An
    /// upsert that took the `DO UPDATE` arm stamps the updating transaction's
    /// id into the row's `xmax`, so a non-zero value means "this already
    /// existed". It is the only way to tell the two arms apart from a single
    /// statement — `RETURNING` alone reports both identically.
    #[diesel(sql_type = Bool)]
    inserted: bool,
}

/// Fold N session bumps into `sessions`, one statement.
///
/// The conflict arm is copied from [`crate::repo::bump_session`] unchanged, so
/// `GREATEST`/`LEAST`/`COALESCE` still decide every field the same way. What
/// changes is only that the rows arrive together.
///
/// Returns the `(app_id, session_id)` of the sessions this call **inserted**,
/// which `write_rows_once` needs in order to credit
/// `event_user_environments.sessions_count`. A session is bumped again by every
/// batch that carries a signal for it, so crediting per bump would count one
/// session once per batch it spans — an over-count that grows with session
/// length and that no single-batch test can see.
pub async fn bump_sessions(
    conn: &mut AsyncPgConnection,
    rows: &[SessionBump],
) -> QueryResult<Vec<(Uuid, String)>> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    // Sorted by `(app_id, session_id)` so every concurrent batch takes these row locks in
    // the same order — see the module's ordering rule.
    let mut ix: Vec<usize> = (0..rows.len()).collect();
    ix.sort_unstable_by(|&a, &b| {
        (rows[a].app_id, &rows[a].session_id).cmp(&(rows[b].app_id, &rows[b].session_id))
    });
    diesel::sql_query(
        "INSERT INTO sessions \
           (app_id, session_id, distinct_id, device_key, started_at, last_event_at, \
            events_count, errors_count, context, release, environment_id, ip_address) \
         SELECT app_id, session_id, distinct_id, device_key, first_at, last_at, \
                events_delta, errors_delta, context, release, environment_id, ip_address \
         FROM unnest($1::uuid[], $2::text[], $3::text[], $4::text[], $5::timestamptz[], \
                     $6::timestamptz[], $7::bigint[], $8::bigint[], $9::jsonb[], $10::text[], \
                     $11::uuid[], $12::text[]) \
              AS t(app_id, session_id, distinct_id, device_key, first_at, last_at, \
                   events_delta, errors_delta, context, release, environment_id, ip_address) \
         ON CONFLICT (app_id, session_id) DO UPDATE SET \
            last_event_at = GREATEST(sessions.last_event_at, EXCLUDED.last_event_at), \
            started_at = LEAST(sessions.started_at, EXCLUDED.started_at), \
            events_count = sessions.events_count + EXCLUDED.events_count, \
            errors_count = sessions.errors_count + EXCLUDED.errors_count, \
            distinct_id = COALESCE(EXCLUDED.distinct_id, sessions.distinct_id), \
            device_key = COALESCE(EXCLUDED.device_key, sessions.device_key), \
            context = CASE WHEN EXCLUDED.context <> '{}'::jsonb THEN EXCLUDED.context ELSE sessions.context END, \
            release = COALESCE(EXCLUDED.release, sessions.release), \
            environment_id = COALESCE(EXCLUDED.environment_id, sessions.environment_id), \
            ip_address = COALESCE(EXCLUDED.ip_address, sessions.ip_address), \
            updated_at = now() \
         RETURNING app_id, session_id, (xmax = 0) AS inserted",
    )
    .bind::<Array<SqlUuid>, _>(ix.iter().map(|&i| rows[i].app_id).collect::<Vec<_>>())
    .bind::<Array<Text>, _>(ix.iter().map(|&i| rows[i].session_id.clone()).collect::<Vec<_>>())
    .bind::<Array<Nullable<Text>>, _>(ix.iter().map(|&i| rows[i].distinct_id.clone()).collect::<Vec<_>>())
    .bind::<Array<Nullable<Text>>, _>(ix.iter().map(|&i| rows[i].device_key.clone()).collect::<Vec<_>>())
    .bind::<Array<Timestamptz>, _>(ix.iter().map(|&i| rows[i].first_at).collect::<Vec<_>>())
    .bind::<Array<Timestamptz>, _>(ix.iter().map(|&i| rows[i].last_at).collect::<Vec<_>>())
    .bind::<Array<BigInt>, _>(ix.iter().map(|&i| rows[i].events_delta).collect::<Vec<_>>())
    .bind::<Array<BigInt>, _>(ix.iter().map(|&i| rows[i].errors_delta).collect::<Vec<_>>())
    .bind::<Array<Jsonb>, _>(ix.iter().map(|&i| rows[i].context.clone()).collect::<Vec<_>>())
    .bind::<Array<Nullable<Text>>, _>(ix.iter().map(|&i| rows[i].release.clone()).collect::<Vec<_>>())
    .bind::<Array<Nullable<SqlUuid>>, _>(ix.iter().map(|&i| rows[i].environment_id).collect::<Vec<_>>())
    .bind::<Array<Nullable<Text>>, _>(ix.iter().map(|&i| rows[i].ip.clone()).collect::<Vec<_>>())
    .get_results::<BumpedSession>(conn)
    .await
    .map(|rows| {
        rows.into_iter()
            .filter(|r| r.inserted)
            .map(|r| (r.app_id, r.session_id))
            .collect()
    })
}

/// One device's folded contribution from a batch.
#[derive(Debug, Clone)]
pub struct DeviceBump {
    pub app_id: Uuid,
    pub device_key: String,
    pub family: Option<String>,
    pub model: Option<String>,
    pub os_name: Option<String>,
    pub os_version: Option<String>,
    pub arch: Option<String>,
    pub browser: Option<String>,
    pub distinct_id: Option<String>,
    /// See [`SessionBump::first_at`] — `first_seen`/`last_seen` are driven by
    /// `LEAST`/`GREATEST` and need the two ends of the fold, not one point.
    pub first_at: DateTime<Utc>,
    pub last_at: DateTime<Utc>,
    pub events_delta: i64,
    pub errors_delta: i64,
}

/// Fold N device bumps into `devices`, one statement. Conflict arm copied from
/// [`crate::repo::bump_device`].
pub async fn bump_devices(conn: &mut AsyncPgConnection, rows: &[DeviceBump]) -> QueryResult<usize> {
    if rows.is_empty() {
        return Ok(0);
    }
    // Sorted by `(app_id, device_key)` so every concurrent batch takes these row locks in
    // the same order — see the module's ordering rule.
    let mut ix: Vec<usize> = (0..rows.len()).collect();
    ix.sort_unstable_by(|&a, &b| {
        (rows[a].app_id, &rows[a].device_key).cmp(&(rows[b].app_id, &rows[b].device_key))
    });
    diesel::sql_query(
        "INSERT INTO devices \
           (app_id, device_key, family, model, os_name, os_version, arch, browser, \
            last_distinct_id, first_seen, last_seen, events_count, errors_count) \
         SELECT app_id, device_key, family, model, os_name, os_version, arch, browser, \
                last_distinct_id, first_at, last_at, events_delta, errors_delta \
         FROM unnest($1::uuid[], $2::text[], $3::text[], $4::text[], $5::text[], $6::text[], \
                     $7::text[], $8::text[], $9::text[], $10::timestamptz[], $11::timestamptz[], \
                     $12::bigint[], $13::bigint[]) \
              AS t(app_id, device_key, family, model, os_name, os_version, arch, browser, \
                   last_distinct_id, first_at, last_at, events_delta, errors_delta) \
         ON CONFLICT (app_id, device_key) DO UPDATE SET \
            last_seen = GREATEST(devices.last_seen, EXCLUDED.last_seen), \
            first_seen = LEAST(devices.first_seen, EXCLUDED.first_seen), \
            events_count = devices.events_count + EXCLUDED.events_count, \
            errors_count = devices.errors_count + EXCLUDED.errors_count, \
            last_distinct_id = COALESCE(EXCLUDED.last_distinct_id, devices.last_distinct_id), \
            family = COALESCE(EXCLUDED.family, devices.family), \
            model = COALESCE(EXCLUDED.model, devices.model), \
            os_name = COALESCE(EXCLUDED.os_name, devices.os_name), \
            os_version = COALESCE(EXCLUDED.os_version, devices.os_version), \
            arch = COALESCE(EXCLUDED.arch, devices.arch), \
            browser = COALESCE(EXCLUDED.browser, devices.browser), \
            updated_at = now()",
    )
    .bind::<Array<SqlUuid>, _>(ix.iter().map(|&i| rows[i].app_id).collect::<Vec<_>>())
    .bind::<Array<Text>, _>(
        ix.iter()
            .map(|&i| rows[i].device_key.clone())
            .collect::<Vec<_>>(),
    )
    .bind::<Array<Nullable<Text>>, _>(
        ix.iter()
            .map(|&i| rows[i].family.clone())
            .collect::<Vec<_>>(),
    )
    .bind::<Array<Nullable<Text>>, _>(
        ix.iter()
            .map(|&i| rows[i].model.clone())
            .collect::<Vec<_>>(),
    )
    .bind::<Array<Nullable<Text>>, _>(
        ix.iter()
            .map(|&i| rows[i].os_name.clone())
            .collect::<Vec<_>>(),
    )
    .bind::<Array<Nullable<Text>>, _>(
        ix.iter()
            .map(|&i| rows[i].os_version.clone())
            .collect::<Vec<_>>(),
    )
    .bind::<Array<Nullable<Text>>, _>(ix.iter().map(|&i| rows[i].arch.clone()).collect::<Vec<_>>())
    .bind::<Array<Nullable<Text>>, _>(
        ix.iter()
            .map(|&i| rows[i].browser.clone())
            .collect::<Vec<_>>(),
    )
    .bind::<Array<Nullable<Text>>, _>(
        ix.iter()
            .map(|&i| rows[i].distinct_id.clone())
            .collect::<Vec<_>>(),
    )
    .bind::<Array<Timestamptz>, _>(ix.iter().map(|&i| rows[i].first_at).collect::<Vec<_>>())
    .bind::<Array<Timestamptz>, _>(ix.iter().map(|&i| rows[i].last_at).collect::<Vec<_>>())
    .bind::<Array<BigInt>, _>(ix.iter().map(|&i| rows[i].events_delta).collect::<Vec<_>>())
    .bind::<Array<BigInt>, _>(ix.iter().map(|&i| rows[i].errors_delta).collect::<Vec<_>>())
    .execute(conn)
    .await
}

/// One (person, environment)'s folded contribution from a batch.
///
/// `event_users` carries no `environment_id`, so before this rollup existed the
/// Users Explorer derived membership, first/last-seen and all three counts from
/// three LATERALs plus a membership predicate, once per admitted person, with no
/// time bound of any kind.
#[derive(Debug, Clone)]
pub struct PersonEnvBump {
    pub app_id: Uuid,
    pub distinct_id: String,
    /// `None` is `EnvFilter::Unattributed` — a real row, not an absence. See
    /// migration `2026-08-12-000056`'s comment for why.
    pub environment_id: Option<Uuid>,
    /// See [`SessionBump::first_at`] — `first_seen`/`last_seen` are driven by
    /// `LEAST`/`GREATEST` and need the two ends of the fold, not one point.
    pub first_at: DateTime<Utc>,
    pub last_at: DateTime<Utc>,
    pub events_delta: i64,
    pub errors_delta: i64,
    /// **Insert-only, and not folded by the caller.** A session is bumped again
    /// by every batch that carries a signal for it, so `+1` per bump counts one
    /// session once per batch it spans. [`write_rows_once`] credits this from
    /// [`bump_sessions`]' inserted-key list, inside the same transaction; every
    /// other producer leaves it at `0`.
    pub sessions_delta: i64,
}

/// Fold N person/environment bumps into `event_user_environments`, one statement.
///
/// Subject to the module's dedupe rule, and unusually easy to violate here: this
/// function's rows come from TWO producers — `Acc::person_env`'s fold and
/// `write_rows_once`' session crediting — and a person with both an event and a
/// newly-inserted session in the same batch is one conflict key reached from two
/// directions. Passing both as separate rows raises `ON CONFLICT DO UPDATE
/// command cannot affect row a second time` and fails the whole batch, so the
/// crediting step merges into the existing row by key rather than pushing.
pub async fn bump_person_envs(
    conn: &mut AsyncPgConnection,
    rows: &[PersonEnvBump],
) -> QueryResult<usize> {
    if rows.is_empty() {
        return Ok(0);
    }
    // Sorted by the conflict key so every concurrent batch takes these row locks
    // in the same order — see the module's ordering rule. This is the third
    // row-lock participant in `write_rows_once`; the ingest path has already
    // produced one deadlock (`users_seen` vs. the issue upsert) that stayed
    // invisible because the worker's stdout was being discarded.
    let nil = Uuid::nil();
    let mut ix: Vec<usize> = (0..rows.len()).collect();
    ix.sort_unstable_by(|&a, &b| {
        (
            rows[a].app_id,
            &rows[a].distinct_id,
            rows[a].environment_id.unwrap_or(nil),
        )
            .cmp(&(
                rows[b].app_id,
                &rows[b].distinct_id,
                rows[b].environment_id.unwrap_or(nil),
            ))
    });
    diesel::sql_query(
        "INSERT INTO event_user_environments \
           (app_id, distinct_id, environment_id, first_seen, last_seen, \
            events_count, errors_count, sessions_count) \
         SELECT app_id, distinct_id, environment_id, first_at, last_at, \
                events_delta, errors_delta, sessions_delta \
         FROM unnest($1::uuid[], $2::text[], $3::uuid[], $4::timestamptz[], \
                     $5::timestamptz[], $6::bigint[], $7::bigint[], $8::bigint[]) \
              AS t(app_id, distinct_id, environment_id, first_at, last_at, \
                   events_delta, errors_delta, sessions_delta) \
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
    .bind::<Array<SqlUuid>, _>(ix.iter().map(|&i| rows[i].app_id).collect::<Vec<_>>())
    .bind::<Array<Text>, _>(
        ix.iter()
            .map(|&i| rows[i].distinct_id.clone())
            .collect::<Vec<_>>(),
    )
    .bind::<Array<Nullable<SqlUuid>>, _>(
        ix.iter()
            .map(|&i| rows[i].environment_id)
            .collect::<Vec<_>>(),
    )
    .bind::<Array<Timestamptz>, _>(ix.iter().map(|&i| rows[i].first_at).collect::<Vec<_>>())
    .bind::<Array<Timestamptz>, _>(ix.iter().map(|&i| rows[i].last_at).collect::<Vec<_>>())
    .bind::<Array<BigInt>, _>(ix.iter().map(|&i| rows[i].events_delta).collect::<Vec<_>>())
    .bind::<Array<BigInt>, _>(ix.iter().map(|&i| rows[i].errors_delta).collect::<Vec<_>>())
    .bind::<Array<BigInt>, _>(
        ix.iter()
            .map(|&i| rows[i].sessions_delta)
            .collect::<Vec<_>>(),
    )
    .execute(conn)
    .await
}

/// One device/environment pair's folded contribution from a batch.
///
/// The device twin of [`PersonEnvBump`]. `sessions_delta` carries the same
/// insert-only rule: a session is bumped again by every batch that carries a
/// signal for it, so `+1` per bump would count one session once per batch it
/// spans. [`write_rows_once`] credits it from [`bump_sessions`]' inserted-key
/// list, inside the same transaction; every other producer leaves it at `0`.
#[derive(Debug, Clone)]
pub struct DeviceEnvBump {
    pub app_id: Uuid,
    pub device_key: String,
    /// `None` is `EnvFilter::Unattributed` — a real row, not an absence. See
    /// migration `2026-08-12-000059`'s comment for why.
    pub environment_id: Option<Uuid>,
    /// See [`SessionBump::first_at`] — `first_seen`/`last_seen` are driven by
    /// `LEAST`/`GREATEST` and need the two ends of the fold, not one point.
    pub first_at: DateTime<Utc>,
    pub last_at: DateTime<Utc>,
    pub events_delta: i64,
    pub errors_delta: i64,
    pub sessions_delta: i64,
}

/// Fold N device/environment bumps into `device_environments`, one statement.
///
/// Subject to the module's dedupe rule, and — exactly like [`bump_person_envs`]
/// — fed by TWO producers: `Acc::device_env`'s fold and `write_rows_once`'
/// session crediting. A device with both an event and a newly-inserted session
/// in the same batch is one conflict key reached from two directions. Passing
/// both as separate rows raises `ON CONFLICT DO UPDATE command cannot affect
/// row a second time` and fails the whole batch, so the crediting step merges
/// into the existing row by key rather than pushing.
pub async fn bump_device_envs(
    conn: &mut AsyncPgConnection,
    rows: &[DeviceEnvBump],
) -> QueryResult<usize> {
    if rows.is_empty() {
        return Ok(0);
    }
    // Sorted by the conflict key so every concurrent batch takes these row locks
    // in the same order — see the module's ordering rule. This is the fourth
    // row-lock participant in `write_rows_once`; the ingest path has already
    // produced one deadlock (`users_seen` vs. the issue upsert) that stayed
    // invisible because the worker's stdout was being discarded.
    let nil = Uuid::nil();
    let mut ix: Vec<usize> = (0..rows.len()).collect();
    ix.sort_unstable_by(|&a, &b| {
        (
            rows[a].app_id,
            &rows[a].device_key,
            rows[a].environment_id.unwrap_or(nil),
        )
            .cmp(&(
                rows[b].app_id,
                &rows[b].device_key,
                rows[b].environment_id.unwrap_or(nil),
            ))
    });
    diesel::sql_query(
        "INSERT INTO device_environments \
           (app_id, device_key, environment_id, first_seen, last_seen, \
            events_count, errors_count, sessions_count) \
         SELECT app_id, device_key, environment_id, first_at, last_at, \
                events_delta, errors_delta, sessions_delta \
         FROM unnest($1::uuid[], $2::text[], $3::uuid[], $4::timestamptz[], \
                     $5::timestamptz[], $6::bigint[], $7::bigint[], $8::bigint[]) \
              AS t(app_id, device_key, environment_id, first_at, last_at, \
                   events_delta, errors_delta, sessions_delta) \
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
    .bind::<Array<SqlUuid>, _>(ix.iter().map(|&i| rows[i].app_id).collect::<Vec<_>>())
    .bind::<Array<Text>, _>(
        ix.iter()
            .map(|&i| rows[i].device_key.clone())
            .collect::<Vec<_>>(),
    )
    .bind::<Array<Nullable<SqlUuid>>, _>(
        ix.iter()
            .map(|&i| rows[i].environment_id)
            .collect::<Vec<_>>(),
    )
    .bind::<Array<Timestamptz>, _>(ix.iter().map(|&i| rows[i].first_at).collect::<Vec<_>>())
    .bind::<Array<Timestamptz>, _>(ix.iter().map(|&i| rows[i].last_at).collect::<Vec<_>>())
    .bind::<Array<BigInt>, _>(ix.iter().map(|&i| rows[i].events_delta).collect::<Vec<_>>())
    .bind::<Array<BigInt>, _>(ix.iter().map(|&i| rows[i].errors_delta).collect::<Vec<_>>())
    .bind::<Array<BigInt>, _>(
        ix.iter()
            .map(|&i| rows[i].sessions_delta)
            .collect::<Vec<_>>(),
    )
    .execute(conn)
    .await
}

/// One workflow's folded contribution from a batch. Fields mirror
/// [`crate::repo::bump_workflow`]'s arguments; the counters are totals.
#[derive(Debug, Clone)]
pub struct WorkflowBump {
    pub app_id: Uuid,
    pub environment_id: Uuid,
    pub workflow_id: String,
    pub workflow_name: String,
    pub session_id: Option<String>,
    pub distinct_id: Option<String>,
    pub device_key: Option<String>,
    pub release: Option<String>,
    /// See [`SessionBump::first_at`] — `started_at`/`last_event_at` are driven
    /// by `LEAST`/`GREATEST`, so the fold has to carry both ends.
    pub first_at: DateTime<Utc>,
    pub last_at: DateTime<Utc>,
    pub events_delta: i32,
    pub errors_delta: i32,
}

/// Fold N workflow bumps into `workflows`, one statement. Conflict arm copied
/// from [`crate::repo::bump_workflow`].
///
/// Note the direction of the `COALESCE`s: they read
/// `COALESCE(workflows.session_id, EXCLUDED.session_id)`, i.e. the value
/// ALREADY on the row wins and an incoming one only fills a null. That is the
/// opposite of [`bump_sessions`], where `EXCLUDED` comes first. Under the
/// sequential upserts this replaces, that made the *earliest* non-null in a
/// run of signals stick, so the caller's fold must keep the first non-null and
/// not the last. Same for `name`, guarded by `NULLIF(…, '')` rather than a
/// plain null check.
///
/// `environment_id` appears in the insert list but in no `DO UPDATE SET` arm,
/// so it is only ever written when the row is created — the fold keeps the
/// first one for the same reason.
pub async fn bump_workflows(
    conn: &mut AsyncPgConnection,
    rows: &[WorkflowBump],
) -> QueryResult<usize> {
    if rows.is_empty() {
        return Ok(0);
    }
    // Sorted by `(app_id, workflow_id)` so every concurrent batch takes these row locks in
    // the same order — see the module's ordering rule.
    let mut ix: Vec<usize> = (0..rows.len()).collect();
    ix.sort_unstable_by(|&a, &b| {
        (rows[a].app_id, &rows[a].workflow_id).cmp(&(rows[b].app_id, &rows[b].workflow_id))
    });
    diesel::sql_query(
        "INSERT INTO workflows \
           (app_id, environment_id, workflow_id, name, session_id, distinct_id, \
            device_key, release, started_at, last_event_at, events_count, errors_count) \
         SELECT app_id, environment_id, workflow_id, name, session_id, distinct_id, \
                device_key, release, first_at, last_at, events_delta, errors_delta \
         FROM unnest($1::uuid[], $2::uuid[], $3::text[], $4::text[], $5::text[], $6::text[], \
                     $7::text[], $8::text[], $9::timestamptz[], $10::timestamptz[], \
                     $11::int[], $12::int[]) \
              AS t(app_id, environment_id, workflow_id, name, session_id, distinct_id, \
                   device_key, release, first_at, last_at, events_delta, errors_delta) \
         ON CONFLICT (app_id, workflow_id) DO UPDATE SET \
            last_event_at = GREATEST(workflows.last_event_at, EXCLUDED.last_event_at), \
            started_at    = LEAST(workflows.started_at, EXCLUDED.started_at), \
            events_count  = workflows.events_count + EXCLUDED.events_count, \
            errors_count  = workflows.errors_count + EXCLUDED.errors_count, \
            name          = COALESCE(NULLIF(workflows.name, ''), EXCLUDED.name), \
            session_id    = COALESCE(workflows.session_id, EXCLUDED.session_id), \
            distinct_id   = COALESCE(workflows.distinct_id, EXCLUDED.distinct_id), \
            device_key    = COALESCE(workflows.device_key, EXCLUDED.device_key), \
            release       = COALESCE(workflows.release, EXCLUDED.release), \
            updated_at    = now()",
    )
    .bind::<Array<SqlUuid>, _>(ix.iter().map(|&i| rows[i].app_id).collect::<Vec<_>>())
    .bind::<Array<SqlUuid>, _>(
        ix.iter()
            .map(|&i| rows[i].environment_id)
            .collect::<Vec<_>>(),
    )
    .bind::<Array<Text>, _>(
        ix.iter()
            .map(|&i| rows[i].workflow_id.clone())
            .collect::<Vec<_>>(),
    )
    .bind::<Array<Text>, _>(
        ix.iter()
            .map(|&i| rows[i].workflow_name.clone())
            .collect::<Vec<_>>(),
    )
    .bind::<Array<Nullable<Text>>, _>(
        ix.iter()
            .map(|&i| rows[i].session_id.clone())
            .collect::<Vec<_>>(),
    )
    .bind::<Array<Nullable<Text>>, _>(
        ix.iter()
            .map(|&i| rows[i].distinct_id.clone())
            .collect::<Vec<_>>(),
    )
    .bind::<Array<Nullable<Text>>, _>(
        ix.iter()
            .map(|&i| rows[i].device_key.clone())
            .collect::<Vec<_>>(),
    )
    .bind::<Array<Nullable<Text>>, _>(
        ix.iter()
            .map(|&i| rows[i].release.clone())
            .collect::<Vec<_>>(),
    )
    .bind::<Array<Timestamptz>, _>(ix.iter().map(|&i| rows[i].first_at).collect::<Vec<_>>())
    .bind::<Array<Timestamptz>, _>(ix.iter().map(|&i| rows[i].last_at).collect::<Vec<_>>())
    .bind::<Array<Integer>, _>(ix.iter().map(|&i| rows[i].events_delta).collect::<Vec<_>>())
    .bind::<Array<Integer>, _>(ix.iter().map(|&i| rows[i].errors_delta).collect::<Vec<_>>())
    .execute(conn)
    .await
}

/// Advance `last_seen` for N `(app_id, distinct_id)` pairs, one statement.
/// Column list matches [`crate::repo::touch_event_user`] exactly — deliberately
/// *not* widened to include the identification columns, for the
/// old-schema-after-RPM-upgrade reason documented on
/// [`crate::repo::mark_event_user_identified`].
pub async fn touch_event_users(
    conn: &mut AsyncPgConnection,
    rows: &[(Uuid, String)],
) -> QueryResult<usize> {
    if rows.is_empty() {
        return Ok(0);
    }
    // Sorted by `(app_id, distinct_id)` so every concurrent batch takes these row locks in
    // the same order — see the module's ordering rule.
    let mut ix: Vec<usize> = (0..rows.len()).collect();
    ix.sort_unstable_by(|&a, &b| (rows[a].0, &rows[a].1).cmp(&(rows[b].0, &rows[b].1)));
    diesel::sql_query(
        "INSERT INTO event_users (app_id, distinct_id) \
         SELECT app_id, distinct_id FROM unnest($1::uuid[], $2::text[]) \
              AS t(app_id, distinct_id) \
         ON CONFLICT (app_id, distinct_id) DO UPDATE SET last_seen = now(), updated_at = now()",
    )
    .bind::<Array<SqlUuid>, _>(ix.iter().map(|&i| rows[i].0).collect::<Vec<_>>())
    .bind::<Array<Text>, _>(ix.iter().map(|&i| rows[i].1.clone()).collect::<Vec<_>>())
    .execute(conn)
    .await
}

/// Flag N `(app_id, distinct_id)` pairs as identified, first-write-wins.
///
/// Separate statement from [`touch_event_users`] for the same reason the
/// single-row pair is separate: an RPM upgrade can run this binary against a
/// schema with no `identified_at`, and folding the columns together would turn
/// one optional feature's absence into a total loss of `last_seen` tracking.
pub async fn mark_event_users_identified(
    conn: &mut AsyncPgConnection,
    rows: &[(Uuid, String, &'static str)],
) -> QueryResult<usize> {
    if rows.is_empty() {
        return Ok(0);
    }
    // Sorted by `(app_id, distinct_id)` so every concurrent batch takes these row locks in
    // the same order — see the module's ordering rule.
    let mut ix: Vec<usize> = (0..rows.len()).collect();
    ix.sort_unstable_by(|&a, &b| (rows[a].0, &rows[a].1).cmp(&(rows[b].0, &rows[b].1)));
    diesel::sql_query(
        "UPDATE event_users SET identified_at = now(), identified_source = t.source \
         FROM unnest($1::uuid[], $2::text[], $3::text[]) AS t(app_id, distinct_id, source) \
         WHERE event_users.app_id = t.app_id \
           AND event_users.distinct_id = t.distinct_id \
           AND event_users.identified_at IS NULL",
    )
    .bind::<Array<SqlUuid>, _>(ix.iter().map(|&i| rows[i].0).collect::<Vec<_>>())
    .bind::<Array<Text>, _>(ix.iter().map(|&i| rows[i].1.clone()).collect::<Vec<_>>())
    .bind::<Array<Text>, _>(
        ix.iter()
            .map(|&i| rows[i].2.to_string())
            .collect::<Vec<_>>(),
    )
    .execute(conn)
    .await
}

/// Write N HyperLogLog cardinalities onto their issues, one statement.
///
/// Retries on deadlock like [`write_rows`], and needs to: this locks `issues`
/// rows by id while [`upsert_issues`] locks them by `(app_id, fingerprint)`, so
/// the two statements cannot be given a common sort order and will occasionally
/// cycle. Sorting by id at least makes concurrent copies of *this* statement
/// agree with each other.
pub async fn set_issue_users_seen(
    conn: &mut AsyncPgConnection,
    rows: &[(Uuid, i64)],
) -> QueryResult<usize> {
    if rows.is_empty() {
        return Ok(0);
    }
    let mut attempt = 0;
    loop {
        match set_issue_users_seen_once(conn, rows).await {
            Err(e) if is_deadlock(&e) && attempt < DEADLOCK_RETRIES => attempt += 1,
            r => return r,
        }
    }
}

async fn set_issue_users_seen_once(
    conn: &mut AsyncPgConnection,
    rows: &[(Uuid, i64)],
) -> QueryResult<usize> {
    // Sorted by issue id so every concurrent batch takes these row locks in
    // the same order — see the module's ordering rule.
    let mut ix: Vec<usize> = (0..rows.len()).collect();
    ix.sort_unstable_by(|&a, &b| rows[a].0.cmp(&rows[b].0));
    diesel::sql_query(
        "UPDATE issues SET users_seen = t.n \
         FROM unnest($1::uuid[], $2::bigint[]) AS t(id, n) \
         WHERE issues.id = t.id",
    )
    .bind::<Array<SqlUuid>, _>(ix.iter().map(|&i| rows[i].0).collect::<Vec<_>>())
    .bind::<Array<BigInt>, _>(ix.iter().map(|&i| rows[i].1).collect::<Vec<_>>())
    .execute(conn)
    .await
}

/// Everything one batch writes as a unit.
pub struct WriteSet<'a> {
    pub errors: &'a [NewErrorEvent],
    pub analytics: &'a [NewAnalyticsEvent],
    pub transactions: &'a [NewTransaction],
    pub sessions: &'a [SessionBump],
    pub devices: &'a [DeviceBump],
    pub touch_users: &'a [(Uuid, String)],
    /// Empty when the running schema has no `identified_at` — the caller probes
    /// once and passes nothing rather than letting the statement fail.
    pub identified: &'a [(Uuid, String, &'static str)],
    /// Per-(person, environment) rollup deltas. `sessions_delta` arrives ZERO
    /// here and is credited inside the transaction from [`bump_sessions`]'
    /// inserted-key list — the caller cannot know which sessions are new.
    pub person_envs: &'a [PersonEnvBump],
    /// Per-(device, environment) rollup deltas. `sessions_delta` arrives ZERO
    /// here and is credited inside the transaction from [`bump_sessions`]'
    /// inserted-key list, exactly as `person_envs` is — the caller cannot know
    /// which sessions are new.
    pub device_envs: &'a [DeviceEnvBump],
}

/// Write a batch's rows in one transaction.
///
/// Two reasons it is a transaction, and the second is the important one:
///
/// 1. Without it every statement is its own commit, so a batch still pays ~7
///    WAL flushes. With it, one.
/// 2. The ingest worker replays a failed batch item-by-item. If a later
///    statement could fail after the error-event insert had already committed,
///    that replay would insert every one of those events a SECOND time — each
///    carries a fresh uuid_v7, so nothing would dedupe them. Rolling back makes
///    the replay start from nothing.
///
/// Explicit `BEGIN`/`COMMIT` rather than `conn.transaction(|c| …)`:
/// diesel-async 0.9's closure signature needs async closures, which would push
/// the workspace MSRV past the 1.82 the RPM spec builds against.
///
/// The issue upsert is deliberately NOT part of this — see [`upsert_issues`].
pub async fn write_rows(conn: &mut AsyncPgConnection, set: WriteSet<'_>) -> QueryResult<()> {
    let mut attempt = 0;
    loop {
        match write_rows_once(conn, &set).await {
            Err(e) if is_deadlock(&e) && attempt < DEADLOCK_RETRIES => attempt += 1,
            r => return r,
        }
    }
}

/// How many times a deadlocked statement is retried before the caller is told.
/// Deadlock is a *transient* outcome — the loser was rolled back and its
/// competitor has now finished — so a couple of retries convert nearly all of
/// them into success. Bounded, because a genuinely pathological workload must
/// still surface rather than spin.
const DEADLOCK_RETRIES: usize = 3;

/// Whether an error is a deadlock (or a serialization failure), i.e. one that
/// re-running is expected to clear.
fn is_deadlock(e: &diesel::result::Error) -> bool {
    match e {
        diesel::result::Error::DatabaseError(kind, info) => {
            matches!(
                kind,
                diesel::result::DatabaseErrorKind::SerializationFailure
            ) || info.message().contains("deadlock")
        }
        _ => false,
    }
}

/// Add `sessions_count` credit to the batch's person rollup rows.
///
/// Separate from the caller because only [`bump_sessions`] knows which sessions
/// were newly INSERTED, and that is not knowable before the statement runs. A
/// session is bumped again by every batch that carries a signal for it, so
/// crediting per bump would count one session once per batch it spans.
///
/// Merges by conflict key rather than pushing: a person with both an event and
/// a new session in one batch is one key reached from two producers, and two
/// rows sharing a key abort the whole statement with "ON CONFLICT DO UPDATE
/// command cannot affect row a second time".
fn credit_sessions(set: &WriteSet<'_>, inserted: &HashSet<(Uuid, String)>) -> Vec<PersonEnvBump> {
    let mut rows: Vec<PersonEnvBump> = set.person_envs.to_vec();
    if inserted.is_empty() {
        return rows;
    }
    let nil = Uuid::nil();
    let mut at: HashMap<(Uuid, String, Uuid), usize> = rows
        .iter()
        .enumerate()
        .map(|(i, p)| {
            (
                (
                    p.app_id,
                    p.distinct_id.clone(),
                    p.environment_id.unwrap_or(nil),
                ),
                i,
            )
        })
        .collect();
    for s in set.sessions {
        // An empty distinct_id has no `event_users` row, so a rollup entry for
        // it could never be joined back to a person.
        let Some(did) = s.distinct_id.as_deref().filter(|d| !d.is_empty()) else {
            continue;
        };
        if !inserted.contains(&(s.app_id, s.session_id.clone())) {
            continue;
        }
        let key = (s.app_id, did.to_string(), s.environment_id.unwrap_or(nil));
        match at.get(&key) {
            Some(&i) => rows[i].sessions_delta += 1,
            None => {
                at.insert(key, rows.len());
                rows.push(PersonEnvBump {
                    app_id: s.app_id,
                    distinct_id: did.to_string(),
                    environment_id: s.environment_id,
                    first_at: s.first_at,
                    last_at: s.last_at,
                    events_delta: 0,
                    errors_delta: 0,
                    sessions_delta: 1,
                });
            }
        }
    }
    rows
}

/// Add `sessions_count` credit to the batch's DEVICE rollup rows.
///
/// The device twin of [`credit_sessions`], and separate from it because the two
/// key on different columns and neither implies the other: a session can carry
/// a `device_key` with no `distinct_id` (an anonymous device) or the reverse (a
/// server SDK with no device). Folding both into one function would drop
/// whichever key the session lacks.
///
/// Merges by conflict key rather than pushing, for the same reason
/// [`credit_sessions`] does: a device with both an event and a new session in
/// one batch is one key reached from two producers, and two rows sharing a key
/// abort the whole statement.
fn credit_device_sessions(
    set: &WriteSet<'_>,
    inserted: &HashSet<(Uuid, String)>,
) -> Vec<DeviceEnvBump> {
    let mut rows: Vec<DeviceEnvBump> = set.device_envs.to_vec();
    if inserted.is_empty() {
        return rows;
    }
    let nil = Uuid::nil();
    let mut at: HashMap<(Uuid, String, Uuid), usize> = rows
        .iter()
        .enumerate()
        .map(|(i, d)| {
            (
                (
                    d.app_id,
                    d.device_key.clone(),
                    d.environment_id.unwrap_or(nil),
                ),
                i,
            )
        })
        .collect();
    for s in set.sessions {
        // A session with no device_key has no row in `devices` and could never
        // be joined back to one.
        let Some(dk) = s.device_key.as_deref().filter(|d| !d.is_empty()) else {
            continue;
        };
        if !inserted.contains(&(s.app_id, s.session_id.clone())) {
            continue;
        }
        let key = (s.app_id, dk.to_string(), s.environment_id.unwrap_or(nil));
        match at.get(&key) {
            Some(&i) => rows[i].sessions_delta += 1,
            None => {
                at.insert(key, rows.len());
                rows.push(DeviceEnvBump {
                    app_id: s.app_id,
                    device_key: dk.to_string(),
                    environment_id: s.environment_id,
                    first_at: s.first_at,
                    last_at: s.last_at,
                    events_delta: 0,
                    errors_delta: 0,
                    sessions_delta: 1,
                });
            }
        }
    }
    rows
}

async fn write_rows_once(conn: &mut AsyncPgConnection, set: &WriteSet<'_>) -> QueryResult<()> {
    conn.batch_execute("BEGIN").await?;
    let r = async {
        insert_error_events(conn, set.errors).await?;
        insert_analytics_events(conn, set.analytics).await?;
        insert_transactions(conn, set.transactions).await?;
        // Touch must precede identification: the latter is an UPDATE and only
        // matches rows that already exist.
        touch_event_users(conn, set.touch_users).await?;
        mark_event_users_identified(conn, set.identified).await?;
        // The roll-ups go LAST, and `devices` and the two env rollups last of
        // all. A row lock is held until COMMIT, so the later a contended row is
        // taken the shorter every other worker waits for it — and `devices` is
        // the most contended row in the set, since every signal from one device
        // folds onto one row.
        let inserted = bump_sessions(conn, set.sessions).await?;
        bump_devices(conn, set.devices).await?;
        let inserted: HashSet<(Uuid, String)> = inserted.into_iter().collect();
        bump_person_envs(conn, &credit_sessions(set, &inserted)).await?;
        bump_device_envs(conn, &credit_device_sessions(set, &inserted)).await?;
        Ok(())
    }
    .await;
    match r {
        Ok(()) => conn.batch_execute("COMMIT").await,
        Err(e) => {
            // Best-effort: if the ROLLBACK itself fails the connection is
            // already unusable, and the pool discards it on return — which
            // aborts the transaction anyway.
            let _ = conn.batch_execute("ROLLBACK").await;
            Err(e)
        }
    }
}
