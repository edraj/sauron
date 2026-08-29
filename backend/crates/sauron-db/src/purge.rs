//! The admin data purge's data access.
//!
//! Every statement here is `sql_query`, and every SQL identifier that varies
//! comes from a `&'static str` obtained by matching on [`PurgeKind`] — never
//! from caller bytes. Identifiers cannot be bound, and the worker reads
//! `purge_jobs.kinds` back out of Postgres in a different process from the one
//! that validated it, so "it was validated in Rust at write time" is not a
//! control that holds here.
//!
//! ## The partition-pruning rule
//!
//! `error_events`, `analytics_events` and `transactions` are RANGE-partitioned
//! on `occurred_at`. Joining a CTE on `(id, occurred_at)` does **not** prune:
//! comparing `occurred_at` to a CTE COLUMN gives the planner no pruning key
//! and it plans one node per child partition. Every statement below therefore
//! repeats the window as BOUND SCALAR PARAMETERS in the mutating arm as well
//! as the selecting one. That duplication looks redundant and is load-bearing;
//! `repo::mask_batch_jsonb` documents the same rule for the same reason.
//!
//! ## The worker fence must gate the CTE, not the outer statement
//!
//! Every statement here ends with `UPDATE purge_jobs … WHERE id = $n AND
//! worker_id = $m`, which reads like a lease check on the whole operation. **It
//! is not.** In Postgres a data-modifying CTE executes regardless of whether
//! the outer statement's `WHERE` matches anything: the `DELETE` arm runs, the
//! final `UPDATE` matches zero rows, and the statement returns no row at all.
//!
//! The caller sees `None` — "I lost the claim" — while the rows are already
//! gone and no counter recorded them. That is strictly worse than having no
//! fence: silent data loss with a clean-looking error path.
//!
//! So each statement opens with a `fence` CTE and every mutating arm carries
//! `AND EXISTS (SELECT 1 FROM fence)`. With the lease gone `sel` is empty,
//! nothing is deleted, and the `None` the caller sees is then true. The
//! uncorrelated `EXISTS` is an InitPlan evaluated once and does not interfere
//! with the partition pruning above.
//!
//! MEASURED, not theorised: `a_stolen_lease_stops_the_delete` in
//! `tests/data_purge.rs` failed with the row deleted and `out.is_none()`
//! simultaneously true. No unit test can see this — it is a property of how
//! Postgres executes CTEs.

use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel::sql_types::{Array, BigInt, Bool, Jsonb, Nullable, Text, Timestamptz, Uuid as SqlUuid};
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use sauron_purge::PurgeKind;
use serde_json::Value;
use uuid::Uuid;

use crate::models::{NewPurgeJob, PurgeJob};
use crate::schema::purge_jobs;

/// The frozen scope of one job, unpacked from its row into bindable pieces.
///
/// Built once per batch rather than threaded through as a dozen arguments, and
/// deliberately NOT reconstructed from the request: the worker runs against
/// what preview stored, which is what makes it impossible for confirm to widen
/// what was counted.
pub struct Scope {
    pub app_id: Uuid,
    /// `None` = every environment INCLUDING unattributed rows.
    ///
    /// When `Some`, the predicate is `environment_id = ANY($n)`, and since
    /// `NULL = ANY(...)` is NULL rather than true, unattributed rows are
    /// excluded — which is correct: they are not in any of the named
    /// environments.
    pub environment_ids: Option<Vec<Uuid>>,
    /// The effective window: the requested range already intersected with the
    /// hot boundary, so nothing below has to reason about cold again.
    pub lo: DateTime<Utc>,
    pub hi: DateTime<Utc>,
}

impl Scope {
    /// Intersect the job's requested window with the cold boundary.
    ///
    /// Returns `None` when the intersection is empty — the whole requested
    /// range is already in cold Parquet, so there is nothing hot to delete and
    /// the caller must not run a batch that would silently match everything.
    pub fn from_job(job: &PurgeJob, cold_boundary: DateTime<Utc>) -> Option<Self> {
        // `all_time` is enforced by a CHECK constraint to have NULL bounds, so
        // the `unwrap_or` arms are the all-time case, not a fallback for bad
        // data. DateTime::<Utc>::MAX_UTC as the upper bound rather than `now()`:
        // an event with a future `occurred_at` is still in scope for "all
        // time", and clock skew makes those real.
        let lo = job.range_start.unwrap_or(DateTime::<Utc>::MIN_UTC);
        let hi = job.range_end.unwrap_or(DateTime::<Utc>::MAX_UTC);
        let lo = lo.max(cold_boundary);
        if lo >= hi {
            return None;
        }
        Some(Self {
            app_id: job.app_id,
            environment_ids: parse_env_ids(job.environment_ids.as_ref()),
            lo,
            hi,
        })
    }
}

/// `null` -> every environment; `[...]` -> those environments.
///
/// An explicitly empty array is refused at the API, so it should not reach
/// here; if it somehow does it stays `Some(vec![])`, which matches nothing.
/// That is the safe direction — a scope that deletes nothing rather than one
/// that deletes everything.
fn parse_env_ids(v: Option<&Value>) -> Option<Vec<Uuid>> {
    let arr = v?.as_array()?;
    Some(
        arr.iter()
            .filter_map(|x| x.as_str())
            .filter_map(|s| Uuid::parse_str(s).ok())
            .collect(),
    )
}

/// The three partitioned raw tables, and their per-table facts.
///
/// `issue_col` is `Some` only for `error_events`; nothing else carries an
/// `issue_id`, which is why analytics and transactions can never change an
/// issue's counters.
struct RawTable {
    name: &'static str,
    has_issue: bool,
}

fn raw_table(kind: PurgeKind) -> Option<RawTable> {
    match kind {
        PurgeKind::ErrorEvents => Some(RawTable {
            name: "error_events",
            has_issue: true,
        }),
        PurgeKind::AnalyticsEvents => Some(RawTable {
            name: "analytics_events",
            has_issue: false,
        }),
        PurgeKind::Transactions => Some(RawTable {
            name: "transactions",
            has_issue: false,
        }),
        _ => None,
    }
}

/// The rollup tables and the columns that define their activity span.
///
/// The span columns differ per table and are NOT interchangeable: `sessions`
/// uses `started_at`/`last_event_at`, everything else `first_seen`/`last_seen`,
/// and `workflows` uses `started_at`/`last_event_at` like sessions.
struct RollupTable {
    name: &'static str,
    span_start: &'static str,
    span_end: &'static str,
    /// The column holding the key recorded in `purge_touched_keys`.
    key_col: &'static str,
    env_scoped: bool,
}

/// Tables that hold no span of their own but die with the rollup row.
///
/// Each is keyed by `app_id` plus the SAME key column as its rollup, which is
/// what lets both delete paths prune them with one `IN (SELECT ...)` over the
/// keys actually removed.
///
/// This exists because the relationship is NOT expressed in the schema.
/// `event_user_environments` and `identities` reference `apps` and
/// `app_environments` — never `event_users` — so no cascade reaches them and
/// deleting the person alone silently strands both. `list_persons` reads
/// `event_user_environments` on a backfilled app, so a stranded row keeps the
/// purged person on the Users Explorer with their pre-purge counters: the
/// exact staleness the purge exists to repair.
///
/// Adding a table here is enough to have it purged by both paths. It must be a
/// table that has NO meaning without its rollup row — a work queue keyed by the
/// same id does not qualify (see `identity_merges`, which the purge
/// deliberately leaves to its own worker).
fn rollup_companions(kind: PurgeKind) -> &'static [&'static str] {
    match kind {
        // `person_days` is keyed by distinct_id, so it is personal data in its
        // own right: a surviving row still says which days an erased person was
        // active. It rides in the SAME statement as the rest for the reason the
        // call site documents — nothing here runs in an explicit transaction, so
        // a separate statement is a window in which the person is gone and their
        // daily activity is not.
        PurgeKind::Persons => &["event_user_environments", "identities", "person_days"],
        _ => &[],
    }
}

fn rollup_table(kind: PurgeKind) -> Option<RollupTable> {
    match kind {
        PurgeKind::Sessions => Some(RollupTable {
            name: "sessions",
            span_start: "started_at",
            span_end: "last_event_at",
            key_col: "session_id",
            env_scoped: true,
        }),
        PurgeKind::Devices => Some(RollupTable {
            name: "devices",
            span_start: "first_seen",
            span_end: "last_seen",
            key_col: "device_key",
            env_scoped: false,
        }),
        PurgeKind::Issues => Some(RollupTable {
            name: "issues",
            span_start: "first_seen",
            span_end: "last_seen",
            key_col: "id",
            env_scoped: false,
        }),
        PurgeKind::Persons => Some(RollupTable {
            name: "event_users",
            span_start: "first_seen",
            span_end: "last_seen",
            key_col: "distinct_id",
            env_scoped: false,
        }),
        PurgeKind::Workflows => Some(RollupTable {
            name: "workflows",
            span_start: "started_at",
            span_end: "last_event_at",
            key_col: "workflow_id",
            env_scoped: true,
        }),
        _ => None,
    }
}

// ===========================================================================
// Job lifecycle
// ===========================================================================

pub async fn insert_purge_job(
    conn: &mut AsyncPgConnection,
    new: NewPurgeJob<'_>,
) -> QueryResult<PurgeJob> {
    diesel::insert_into(purge_jobs::table)
        .values(new)
        .returning(PurgeJob::as_returning())
        .get_result(conn)
        .await
}

pub async fn get_purge_job(
    conn: &mut AsyncPgConnection,
    id: Uuid,
) -> QueryResult<Option<PurgeJob>> {
    purge_jobs::table
        .find(id)
        .select(PurgeJob::as_select())
        .first(conn)
        .await
        .optional()
}

/// Job history for the orgs the caller can see, newest first.
pub async fn list_purge_jobs(
    conn: &mut AsyncPgConnection,
    org_ids: &[Uuid],
    limit: i64,
) -> QueryResult<Vec<PurgeJob>> {
    purge_jobs::table
        .filter(purge_jobs::org_id.eq_any(org_ids.to_vec()))
        .order(purge_jobs::requested_at.desc())
        .limit(limit.clamp(1, 500))
        .select(PurgeJob::as_select())
        .load(conn)
        .await
}

/// Claim one job in `status`, oldest first.
///
/// `FOR UPDATE SKIP LOCKED` so two workers never take the same row, and the
/// `worker_id` written here is the fence every later flush checks: a worker
/// whose lease was stolen updates zero rows rather than double-counting.
pub async fn claim_purge_job(
    conn: &mut AsyncPgConnection,
    status: &str,
    next_status: &str,
    worker_id: &str,
    stale_after_secs: i64,
) -> QueryResult<Option<PurgeJob>> {
    diesel::sql_query(
        "WITH claimable AS ( \
           SELECT id FROM purge_jobs \
            WHERE status = $1 \
              AND (worker_id IS NULL OR claimed_at < now() - ($4 || ' seconds')::interval) \
            ORDER BY requested_at \
            FOR UPDATE SKIP LOCKED \
            LIMIT 1) \
         UPDATE purge_jobs j SET \
           status = $2, worker_id = $3, claimed_at = now(), \
           started_at = COALESCE(j.started_at, now()) \
         FROM claimable WHERE j.id = claimable.id \
         RETURNING j.*",
    )
    .bind::<Text, _>(status)
    .bind::<Text, _>(next_status)
    .bind::<Text, _>(worker_id)
    .bind::<Text, _>(stale_after_secs.to_string())
    .get_result(conn)
    .await
    .optional()
}

/// Advance a job to `previewed`, recording the counts and the cold report.
pub async fn finish_preview(
    conn: &mut AsyncPgConnection,
    job_id: Uuid,
    worker_id: &str,
    estimated: &Value,
    cold_rows_skipped: i64,
    cold_boundary_at: DateTime<Utc>,
) -> QueryResult<usize> {
    diesel::sql_query(
        "UPDATE purge_jobs SET \
           status = 'previewed', phase = 'idle', previewed_at = now(), \
           estimated_counts = $3, cold_rows_skipped = $4, cold_boundary_at = $5, \
           worker_id = NULL, claimed_at = NULL \
         WHERE id = $1 AND worker_id = $2",
    )
    .bind::<SqlUuid, _>(job_id)
    .bind::<Text, _>(worker_id)
    .bind::<Jsonb, _>(estimated.clone())
    .bind::<BigInt, _>(cold_rows_skipped)
    .bind::<Timestamptz, _>(cold_boundary_at)
    .execute(conn)
    .await
}

/// `previewed` -> `pending`, gated on the typed slug and the preview TTL.
///
/// The TTL is checked in SQL rather than in the handler so the window cannot
/// be widened by a slow request: `previewed_at` is compared to `now()` at the
/// instant the transition happens, not at the instant the handler decided to
/// attempt it.
pub async fn confirm_purge_job(
    conn: &mut AsyncPgConnection,
    job_id: Uuid,
    ttl_secs: i64,
    source: &str,
) -> QueryResult<usize> {
    diesel::sql_query(
        "UPDATE purge_jobs SET \
           status = 'pending', confirmed_at = now(), confirm_source = $3 \
         WHERE id = $1 AND status = 'previewed' \
           AND previewed_at IS NOT NULL \
           AND previewed_at > now() - ($2 || ' seconds')::interval",
    )
    .bind::<SqlUuid, _>(job_id)
    .bind::<Text, _>(ttl_secs.to_string())
    .bind::<Text, _>(source)
    .execute(conn)
    .await
}

/// Request cancellation.
///
/// A `pending` job goes straight to `cancelled` (nothing has been deleted). A
/// `running` one goes to `cancelling`, which the worker observes on the write
/// it was making anyway and then finalizes — it does NOT stop mid-batch, and
/// it never restores rows already removed.
pub async fn cancel_purge_job(
    conn: &mut AsyncPgConnection,
    job_id: Uuid,
    user_id: Option<Uuid>,
    email: &str,
) -> QueryResult<usize> {
    diesel::sql_query(
        "UPDATE purge_jobs SET \
           status = CASE WHEN status = 'pending' THEN 'cancelled' ELSE 'cancelling' END, \
           cancelled_by = $2, cancelled_by_email = $3, cancelled_at = now(), \
           finished_at = CASE WHEN status = 'pending' THEN now() ELSE finished_at END \
         WHERE id = $1 AND status IN ('previewed','pending','running')",
    )
    .bind::<SqlUuid, _>(job_id)
    .bind::<Nullable<SqlUuid>, _>(user_id)
    .bind::<Text, _>(email)
    .execute(conn)
    .await
}

pub async fn set_purge_phase(
    conn: &mut AsyncPgConnection,
    job_id: Uuid,
    worker_id: &str,
    phase: &str,
    kind_cursor: Option<&str>,
) -> QueryResult<usize> {
    diesel::sql_query(
        "UPDATE purge_jobs SET \
           phase = $3, kind_cursor = $4, \
           cursor_occurred_at = NULL, cursor_id = NULL, claimed_at = now() \
         WHERE id = $1 AND worker_id = $2",
    )
    .bind::<SqlUuid, _>(job_id)
    .bind::<Text, _>(worker_id)
    .bind::<Text, _>(phase)
    .bind::<Nullable<Text>, _>(kind_cursor)
    .execute(conn)
    .await
}

/// Terminal transition. `cancelling` finalizes as `cancelled`, everything else
/// as `done`, so a cancel requested mid-run is not reported as a clean finish.
pub async fn finish_purge_job(
    conn: &mut AsyncPgConnection,
    job_id: Uuid,
    worker_id: &str,
    cold_rows_skipped: i64,
) -> QueryResult<usize> {
    diesel::sql_query(
        "UPDATE purge_jobs SET \
           status = CASE WHEN status = 'cancelling' THEN 'cancelled' ELSE 'done' END, \
           phase = 'finished', finished_at = now(), \
           cold_rows_skipped = $3, worker_id = NULL, claimed_at = NULL \
         WHERE id = $1 AND worker_id = $2",
    )
    .bind::<SqlUuid, _>(job_id)
    .bind::<Text, _>(worker_id)
    .bind::<BigInt, _>(cold_rows_skipped)
    .execute(conn)
    .await
}

pub async fn fail_purge_job(
    conn: &mut AsyncPgConnection,
    job_id: Uuid,
    worker_id: &str,
    error: &str,
) -> QueryResult<usize> {
    diesel::sql_query(
        "UPDATE purge_jobs SET \
           status = 'failed', phase = 'finished', finished_at = now(), \
           error = left($3, 2000), worker_id = NULL, claimed_at = NULL \
         WHERE id = $1 AND worker_id = $2",
    )
    .bind::<SqlUuid, _>(job_id)
    .bind::<Text, _>(worker_id)
    .bind::<Text, _>(error)
    .execute(conn)
    .await
}

// ===========================================================================
// Counting (preview)
// ===========================================================================

#[derive(QueryableByName)]
struct CountRow {
    #[diesel(sql_type = BigInt)]
    n: i64,
}

/// Count hot rows of a raw kind in scope.
///
/// Counted directly on `(app_id, environment_id, occurred_at)`. Deliberately
/// NOT routed through the analytics query builders: those introduce a
/// time-unbounded correlated EXISTS/LATERAL when an environment is supplied,
/// whose cost scales with retained data rather than the requested window —
/// the measured cause of the 30s timeouts on the analytics endpoints. A
/// preview built on that shape would time out on exactly the large,
/// badly-polluted app this feature exists for.
pub async fn count_raw_in_scope(
    conn: &mut AsyncPgConnection,
    kind: PurgeKind,
    scope: &Scope,
) -> QueryResult<i64> {
    let Some(t) = raw_table(kind) else {
        return Ok(0);
    };
    let sql = format!(
        "SELECT count(*)::bigint AS n FROM {} \
         WHERE app_id = $1 AND occurred_at >= $2 AND occurred_at < $3 \
           AND ($4::uuid[] IS NULL OR environment_id = ANY($4))",
        t.name
    );
    let row: CountRow = diesel::sql_query(sql)
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Timestamptz, _>(scope.lo)
        .bind::<Timestamptz, _>(scope.hi)
        .bind::<Nullable<Array<SqlUuid>>, _>(scope.environment_ids.clone())
        .get_result(conn)
        .await?;
    Ok(row.n)
}

/// Count rollup rows whose ENTIRE span falls inside the window.
///
/// Containment, not overlap — see `sauron_purge::Window::contains_span` for
/// why. `all_time` arrives as MIN/MAX bounds, so no special case is needed.
pub async fn count_rollup_contained(
    conn: &mut AsyncPgConnection,
    kind: PurgeKind,
    scope: &Scope,
) -> QueryResult<i64> {
    let Some(t) = rollup_table(kind) else {
        return Ok(0);
    };
    let env_pred = if t.env_scoped {
        "AND ($4::uuid[] IS NULL OR environment_id = ANY($4))"
    } else {
        // Bound but unused, so every call site can bind the same four
        // parameters. `validate_scope` already refuses an env filter for these
        // kinds, so ignoring it here cannot silently widen anything.
        "AND ($4::uuid[] IS NULL OR TRUE)"
    };
    let sql = format!(
        "SELECT count(*)::bigint AS n FROM {} \
         WHERE app_id = $1 AND {} >= $2 AND {} <= $3 {}",
        t.name, t.span_start, t.span_end, env_pred
    );
    let row: CountRow = diesel::sql_query(sql)
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Timestamptz, _>(scope.lo)
        .bind::<Timestamptz, _>(scope.hi)
        .bind::<Nullable<Array<SqlUuid>>, _>(scope.environment_ids.clone())
        .get_result(conn)
        .await?;
    Ok(row.n)
}

// ===========================================================================
// The delete phase
// ===========================================================================

#[derive(Debug, Clone)]
pub struct PurgeBatch {
    pub scanned: i64,
    pub deleted: i64,
    /// `None` when the batch came back short — this kind is finished.
    pub next_cursor: Option<(DateTime<Utc>, Uuid)>,
    /// Observed on a write the worker was making anyway, so a cancel is seen
    /// without a second round trip.
    pub status: String,
}

#[derive(QueryableByName)]
struct BatchRow {
    #[diesel(sql_type = BigInt)]
    scanned: i64,
    #[diesel(sql_type = BigInt)]
    deleted: i64,
    #[diesel(sql_type = Nullable<Timestamptz>)]
    cur_occurred_at: Option<DateTime<Utc>>,
    #[diesel(sql_type = Nullable<SqlUuid>)]
    cur_id: Option<Uuid>,
    #[diesel(sql_type = Text)]
    status: String,
}

/// One keyset-paginated delete batch over a partitioned raw table, recording
/// the rollup keys it touched as it goes.
///
/// Four things happen in ONE statement, and they must:
///
/// * `sel` picks the batch by keyset, so a resumed job never rescans.
/// * `touched` records the distinct rollup keys, because after the rows are
///   gone there is no way to discover which rollups they fed.
/// * `del` removes them, repeating the window as scalar parameters so the
///   planner prunes partitions (see the module docs).
/// * the `purge_jobs` update advances the cursor and the counter.
///
/// All four commit together, so a SIGKILL loses at most one batch and can
/// never leave a rollup key unrecorded for a row that was already deleted —
/// which would strand that rollup permanently overcounting with nothing left
/// to detect it.
///
/// Unlike the mask batch, resume needs no "already done" guard: a deleted row
/// cannot be re-selected, so re-entering a partly-finished kind is naturally
/// idempotent.
#[allow(clippy::too_many_arguments)]
pub async fn delete_raw_batch(
    conn: &mut AsyncPgConnection,
    kind: PurgeKind,
    scope: &Scope,
    cursor: Option<(DateTime<Utc>, Uuid)>,
    limit: i64,
    job_id: Uuid,
    worker_id: &str,
    record_keys: bool,
) -> QueryResult<Option<PurgeBatch>> {
    let Some(t) = raw_table(kind) else {
        return Ok(None);
    };

    // The key columns to harvest. `transactions` and `analytics_events` carry
    // no issue_id; only `error_events` does.
    let issue_sel = if t.has_issue {
        ", issue_id::text AS issue_key"
    } else {
        ", NULL::text AS issue_key"
    };

    // `record_keys` is false only when nothing derives from this kind
    // (`inspector`), which never reaches here — kept as a parameter so the
    // worker can disable harvesting without a second statement.
    let touched_cte = if record_keys {
        "touched AS ( \
           INSERT INTO purge_touched_keys (job_id, kind, key) \
           SELECT $6, v.kind, v.key FROM sel \
             CROSS JOIN LATERAL (VALUES \
               ('sessions', sel.session_id), \
               ('devices', sel.device_key), \
               ('persons', sel.distinct_id), \
               ('issues', sel.issue_key)) AS v(kind, key) \
           WHERE v.key IS NOT NULL AND v.key <> '' \
           ON CONFLICT DO NOTHING), "
    } else {
        ""
    };

    let sql = format!(
        "WITH fence AS ( \
           SELECT 1 FROM purge_jobs WHERE id = $6 AND worker_id = $7), \
         sel AS ( \
           SELECT id, occurred_at, session_id, device_key, distinct_id{issue_sel} \
           FROM {table} \
           WHERE app_id = $1 AND occurred_at >= $2 AND occurred_at < $3 \
             AND ($4::uuid[] IS NULL OR environment_id = ANY($4)) \
             AND ($8::timestamptz IS NULL OR (occurred_at, id) > ($8, $9)) \
             AND EXISTS (SELECT 1 FROM fence) \
           ORDER BY occurred_at, id LIMIT $5), \
         {touched_cte}\
         del AS ( \
           DELETE FROM {table} e USING sel \
           WHERE e.id = sel.id AND e.occurred_at = sel.occurred_at \
             AND e.occurred_at >= $2 AND e.occurred_at < $3 \
           RETURNING 1 AS one) \
         UPDATE purge_jobs SET \
           cursor_occurred_at = (SELECT max(occurred_at) FROM sel), \
           cursor_id = (SELECT id FROM sel ORDER BY occurred_at DESC, id DESC LIMIT 1), \
           deleted_counts = jsonb_set( \
             deleted_counts, ARRAY[$10], \
             to_jsonb(COALESCE((deleted_counts->>$10)::bigint, 0) \
                      + (SELECT count(*) FROM del)), true), \
           claimed_at = now() \
         WHERE id = $6 AND worker_id = $7 \
         RETURNING (SELECT count(*) FROM sel)::bigint AS scanned, \
                   (SELECT count(*) FROM del)::bigint AS deleted, \
                   cursor_occurred_at AS cur_occurred_at, cursor_id AS cur_id, status",
        table = t.name,
        issue_sel = issue_sel,
        touched_cte = touched_cte,
    );

    let row: Option<BatchRow> = diesel::sql_query(sql)
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Timestamptz, _>(scope.lo)
        .bind::<Timestamptz, _>(scope.hi)
        .bind::<Nullable<Array<SqlUuid>>, _>(scope.environment_ids.clone())
        .bind::<BigInt, _>(limit)
        .bind::<SqlUuid, _>(job_id)
        .bind::<Text, _>(worker_id)
        .bind::<Nullable<Timestamptz>, _>(cursor.map(|c| c.0))
        .bind::<Nullable<SqlUuid>, _>(cursor.map(|c| c.1))
        .bind::<Text, _>(kind.slug())
        .get_result(conn)
        .await
        .optional()?;

    Ok(row.map(|r| PurgeBatch {
        scanned: r.scanned,
        deleted: r.deleted,
        next_cursor: if r.scanned >= limit {
            match (r.cur_occurred_at, r.cur_id) {
                (Some(a), Some(i)) => Some((a, i)),
                _ => None,
            }
        } else {
            None
        },
        status: r.status,
    }))
}

/// Delete the inspector artefacts for the app.
///
/// Not cursor-paginated: a scan is one row and its findings are bounded by it,
/// so the whole set is small enough for one statement. Scans and findings move
/// as a unit — deleting findings while their parent scan survives would leave
/// a scan reporting a finding count it no longer has.
pub async fn delete_inspector_in_scope(
    conn: &mut AsyncPgConnection,
    scope: &Scope,
    job_id: Uuid,
    worker_id: &str,
) -> QueryResult<i64> {
    let row: Option<BatchRow> = diesel::sql_query(
        "WITH fence AS ( \
           SELECT 1 FROM purge_jobs WHERE id = $4 AND worker_id = $5), \
         sel AS ( \
           SELECT id FROM inspector_scans \
            WHERE app_id = $1 AND started_at >= $2 AND started_at < $3 \
              AND EXISTS (SELECT 1 FROM fence)), \
         f AS (DELETE FROM inspector_findings WHERE scan_id IN (SELECT id FROM sel) RETURNING 1), \
         k AS (DELETE FROM inspector_masked_keys WHERE app_id = $1 \
                AND EXISTS (SELECT 1 FROM fence) RETURNING 1), \
         s AS (DELETE FROM inspector_scans WHERE id IN (SELECT id FROM sel) RETURNING 1) \
         UPDATE purge_jobs SET \
           deleted_counts = jsonb_set( \
             deleted_counts, ARRAY['inspector'], \
             to_jsonb(COALESCE((deleted_counts->>'inspector')::bigint, 0) \
                      + (SELECT count(*) FROM f) + (SELECT count(*) FROM k) \
                      + (SELECT count(*) FROM s)), true), \
           claimed_at = now() \
         WHERE id = $4 AND worker_id = $5 \
         RETURNING 0::bigint AS scanned, \
                   ((SELECT count(*) FROM f) + (SELECT count(*) FROM k) \
                    + (SELECT count(*) FROM s))::bigint AS deleted, \
                   NULL::timestamptz AS cur_occurred_at, NULL::uuid AS cur_id, status",
    )
    .bind::<SqlUuid, _>(scope.app_id)
    .bind::<Timestamptz, _>(scope.lo)
    .bind::<Timestamptz, _>(scope.hi)
    .bind::<SqlUuid, _>(job_id)
    .bind::<Text, _>(worker_id)
    .get_result(conn)
    .await
    .optional()?;
    Ok(row.map(|r| r.deleted).unwrap_or(0))
}

/// Delete rollup rows whose entire span is inside the window.
///
/// Runs BEFORE the recompute pass for that kind: a row deleted here needs no
/// repair, and repairing it first would be wasted work on a row about to go.
pub async fn delete_contained_rollups(
    conn: &mut AsyncPgConnection,
    kind: PurgeKind,
    scope: &Scope,
    job_id: Uuid,
    worker_id: &str,
) -> QueryResult<i64> {
    let Some(t) = rollup_table(kind) else {
        return Ok(0);
    };
    let env_pred = if t.env_scoped {
        "AND ($4::uuid[] IS NULL OR environment_id = ANY($4))"
    } else {
        "AND ($4::uuid[] IS NULL OR TRUE)"
    };
    // One statement, so a companion row cannot outlive its rollup row under a
    // crash: these run in autocommit (the worker wraps no phase in an explicit
    // transaction), and a second statement is a second chance to stop between
    // them. Each companion CTE prunes by the keys `del` actually returned, so
    // the fence on `del` covers them too — a worker whose lease was stolen
    // deletes nothing anywhere, rather than losing the rollup fence and taking
    // the companions with it.
    let companions: String = rollup_companions(kind)
        .iter()
        .enumerate()
        .map(|(i, c)| {
            format!(
                ", del_c{i} AS ( \
                   DELETE FROM {c} \
                    WHERE app_id = $1 AND {kc} IN (SELECT k FROM del)) ",
                i = i,
                c = c,
                kc = t.key_col,
            )
        })
        .collect();
    let sql = format!(
        "WITH fence AS ( \
           SELECT 1 FROM purge_jobs WHERE id = $5 AND worker_id = $6), \
         del AS ( \
           DELETE FROM {table} \
            WHERE app_id = $1 AND {ss} >= $2 AND {se} <= $3 {env} \
              AND EXISTS (SELECT 1 FROM fence) \
            RETURNING {kc} AS k) \
         {companions} \
         UPDATE purge_jobs SET \
           deleted_counts = jsonb_set( \
             deleted_counts, ARRAY[$7], \
             to_jsonb(COALESCE((deleted_counts->>$7)::bigint, 0) \
                      + (SELECT count(*) FROM del)), true), \
           claimed_at = now() \
         WHERE id = $5 AND worker_id = $6 \
         RETURNING 0::bigint AS scanned, (SELECT count(*) FROM del)::bigint AS deleted, \
                   NULL::timestamptz AS cur_occurred_at, NULL::uuid AS cur_id, status",
        table = t.name,
        ss = t.span_start,
        se = t.span_end,
        kc = t.key_col,
        env = env_pred,
    );
    let row: Option<BatchRow> = diesel::sql_query(sql)
        .bind::<SqlUuid, _>(scope.app_id)
        .bind::<Timestamptz, _>(scope.lo)
        .bind::<Timestamptz, _>(scope.hi)
        .bind::<Nullable<Array<SqlUuid>>, _>(scope.environment_ids.clone())
        .bind::<SqlUuid, _>(job_id)
        .bind::<Text, _>(worker_id)
        .bind::<Text, _>(kind.slug())
        .get_result(conn)
        .await
        .optional()?;
    Ok(row.map(|r| r.deleted).unwrap_or(0))
}

// ===========================================================================
// The recompute phase
// ===========================================================================

#[derive(QueryableByName, Debug, Clone)]
pub struct TouchedKey {
    #[diesel(sql_type = Text)]
    pub key: String,
}

/// Read a page of touched keys for one rollup kind.
///
/// Keyset on `key` rather than OFFSET: the drain deletes as it goes, so an
/// OFFSET page would skip rows every time the set shrank underneath it.
pub async fn next_touched_keys(
    conn: &mut AsyncPgConnection,
    job_id: Uuid,
    kind: PurgeKind,
    after: Option<&str>,
    limit: i64,
) -> QueryResult<Vec<TouchedKey>> {
    diesel::sql_query(
        "SELECT key FROM purge_touched_keys \
          WHERE job_id = $1 AND kind = $2 AND ($3::text IS NULL OR key > $3) \
          ORDER BY key LIMIT $4",
    )
    .bind::<SqlUuid, _>(job_id)
    .bind::<Text, _>(kind.slug())
    .bind::<Nullable<Text>, _>(after)
    .bind::<BigInt, _>(limit)
    .load(conn)
    .await
}

#[derive(QueryableByName, Debug, Clone, Copy)]
pub struct HotCounts {
    #[diesel(sql_type = BigInt)]
    pub analytics: i64,
    #[diesel(sql_type = BigInt)]
    pub errors: i64,
    #[diesel(sql_type = BigInt)]
    pub transactions: i64,
    #[diesel(sql_type = Nullable<Timestamptz>)]
    pub first: Option<DateTime<Utc>>,
    #[diesel(sql_type = Nullable<Timestamptz>)]
    pub last: Option<DateTime<Utc>>,
}

/// Which raw tables actually carry a given rollup's key column.
///
/// **Not every key exists on every table**, and assuming otherwise is a runtime
/// error rather than a wrong number: `issue_id` exists ONLY on `error_events`,
/// so a recompute that probed `analytics_events` for it fails the whole job
/// with `column "issue_id" does not exist`. Everything else —
/// `session_id`, `device_key`, `distinct_id`, `workflow_id` — is on all three.
///
/// Returned as `(analytics, errors, transactions)` so the caller can build the
/// statement from exactly the tables that apply, and so a missing table
/// contributes a literal zero rather than a broken sub-select.
///
/// MEASURED: this was found by a live drive, not by the test suite, because the
/// integration test for issues called `apply_recomputed_rollup` directly and
/// never went through this function.
fn key_tables(kind: PurgeKind) -> (bool, bool, bool) {
    match kind {
        // error_events only.
        PurgeKind::Issues => (false, true, false),
        _ => (true, true, true),
    }
}

/// Count the surviving HOT rows behind one rollup key, per source table.
///
/// Three separate counts rather than one total, because the counters they feed
/// are not the same: `events_count` comes from analytics alone and
/// `errors_count` from errors alone, while emptiness is judged on all three.
/// See `sauron_purge::recompute` for the delta table.
///
/// This is the hot half only. The caller MUST merge the cold half before
/// writing, or the counter is silently short by whatever `sauron-tier`
/// exported.
pub async fn hot_counts_for_key(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    kind: PurgeKind,
    key: &str,
) -> QueryResult<HotCounts> {
    let Some(kc) = rollup_key_column(kind) else {
        return Ok(HotCounts {
            analytics: 0,
            errors: 0,
            transactions: 0,
            first: None,
            last: None,
        });
    };
    // `issues.id` is a uuid and every other key column is text.
    let cast = if kind == PurgeKind::Issues {
        "::uuid"
    } else {
        ""
    };
    let (a, e, t) = key_tables(kind);

    // Build only the sub-selects whose table actually has the column. A table
    // that does not becomes a literal, so the shape of the result row is the
    // same either way and the caller needs no special case.
    let cnt = |on: bool, table: &str| {
        if on {
            format!("(SELECT count(*) FROM {table} WHERE app_id = $1 AND {kc} = $2{cast})::bigint")
        } else {
            "0::bigint".to_string()
        }
    };
    let agg = |on: bool, table: &str, f: &str| {
        if on {
            format!("(SELECT {f}(occurred_at) FROM {table} WHERE app_id = $1 AND {kc} = $2{cast})")
        } else {
            "NULL::timestamptz".to_string()
        }
    };

    let sql = format!(
        "SELECT {ca} AS analytics, {ce} AS errors, {ct} AS transactions, \
           LEAST({na}, {ne}, {nt}) AS first, \
           GREATEST({xa}, {xe}, {xt}) AS last",
        ca = cnt(a, "analytics_events"),
        ce = cnt(e, "error_events"),
        ct = cnt(t, "transactions"),
        na = agg(a, "analytics_events", "min"),
        ne = agg(e, "error_events", "min"),
        nt = agg(t, "transactions", "min"),
        xa = agg(a, "analytics_events", "max"),
        xe = agg(e, "error_events", "max"),
        xt = agg(t, "transactions", "max"),
    );
    diesel::sql_query(sql)
        .bind::<SqlUuid, _>(app_id)
        .bind::<Text, _>(key)
        .get_result(conn)
        .await
}

/// The column on the raw event tables that holds this rollup's key.
///
/// `&'static str` by construction so it can be interpolated into the
/// statements above without becoming an injection vector.
pub fn rollup_key_column(kind: PurgeKind) -> Option<&'static str> {
    match kind {
        PurgeKind::Sessions => Some("session_id"),
        PurgeKind::Devices => Some("device_key"),
        PurgeKind::Persons => Some("distinct_id"),
        PurgeKind::Issues => Some("issue_id"),
        PurgeKind::Workflows => Some("workflow_id"),
        _ => None,
    }
}

/// Repair one person: the identity row's span, plus its PER-ENVIRONMENT
/// counters.
///
/// A person is not one row. `event_users` holds identity and span;
/// `event_user_environments` holds `events_count` / `errors_count` /
/// `sessions_count` per environment. Both have to be rebuilt, and the
/// environment dimension means the counters cannot be derived from the
/// already-computed app-wide `counts` — they must be re-aggregated grouped by
/// `environment_id`.
///
/// `sessions_count` is a DISTINCT count over `session_id`, and
/// `IS NOT DISTINCT FROM` is what matches the unattributed row: `environment_id`
/// is nullable there because `EnvFilter::Unattributed` is a real row, and plain
/// `=` never matches NULL, so an unattributed person's counters would silently
/// never be repaired.
async fn recompute_person(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    key: &str,
    counts: sauron_purge::recompute::Counts,
) -> QueryResult<bool> {
    // Person-days, re-derived from whatever raw rows survived.
    //
    // A blanket delete would be wrong here: this branch runs when the person
    // still HAS rows (a time-ranged purge), so their remaining days must
    // survive. Leaving the old rows alone would be wrong the other way — a
    // person-day whose raw rows were purged still claims the person was active
    // that day, the same misleading residue the environment recompute below
    // rejects, one row down.
    //
    // Two statements rather than one `DELETE … RETURNING` + `INSERT`: both
    // would target `person_days` within a single snapshot, so the insert's
    // uniqueness check would still see the rows the CTE is deleting and could
    // raise a spurious conflict. Nothing here runs in an explicit transaction
    // anyway, so the pair is no weaker than the statements around it.
    diesel::sql_query("DELETE FROM person_days WHERE app_id = $1 AND distinct_id = $2")
        .bind::<SqlUuid, _>(app_id)
        .bind::<Text, _>(key)
        .execute(conn)
        .await?;
    diesel::sql_query(
        "INSERT INTO person_days (app_id, environment_id, distinct_id, day, events, errors) \
         SELECT $1, environment_id, $2, day, sum(ev), sum(er) FROM ( \
             SELECT environment_id, occurred_at::date AS day, 1 AS ev, 0 AS er \
               FROM analytics_events WHERE app_id = $1 AND distinct_id = $2 \
             UNION ALL \
             SELECT environment_id, occurred_at::date, 0, 1 \
               FROM error_events WHERE app_id = $1 AND distinct_id = $2 \
         ) u GROUP BY environment_id, day",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Text, _>(key)
    .execute(conn)
    .await?;

    // The person's own row: span only, no counters.
    diesel::sql_query(
        "UPDATE event_users SET \
           first_seen = COALESCE($3, first_seen), \
           last_seen = COALESCE($4, last_seen), \
           updated_at = now() \
         WHERE app_id = $1 AND distinct_id = $2",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Text, _>(key)
    .bind::<Nullable<Timestamptz>, _>(counts.first)
    .bind::<Nullable<Timestamptz>, _>(counts.last)
    .execute(conn)
    .await?;

    // Per-environment counters, re-aggregated from every surviving raw row.
    diesel::sql_query(
        "WITH u AS ( \
           SELECT environment_id, occurred_at, session_id, 'a' AS src \
             FROM analytics_events WHERE app_id = $1 AND distinct_id = $2 \
           UNION ALL \
           SELECT environment_id, occurred_at, session_id, 'e' \
             FROM error_events WHERE app_id = $1 AND distinct_id = $2 \
           UNION ALL \
           SELECT environment_id, occurred_at, session_id, 't' \
             FROM transactions WHERE app_id = $1 AND distinct_id = $2), \
         agg AS ( \
           SELECT environment_id, \
                  count(*) FILTER (WHERE src = 'a')::bigint AS ev, \
                  count(*) FILTER (WHERE src = 'e')::bigint AS er, \
                  count(DISTINCT NULLIF(session_id, ''))::bigint AS se, \
                  min(occurred_at) AS lo, max(occurred_at) AS hi \
             FROM u GROUP BY environment_id) \
         UPDATE event_user_environments e SET \
           events_count = agg.ev, errors_count = agg.er, sessions_count = agg.se, \
           first_seen = COALESCE(agg.lo, e.first_seen), \
           last_seen = COALESCE(agg.hi, e.last_seen), \
           updated_at = now() \
         FROM agg \
         WHERE e.app_id = $1 AND e.distinct_id = $2 \
           AND e.environment_id IS NOT DISTINCT FROM agg.environment_id",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Text, _>(key)
    .execute(conn)
    .await?;

    // An environment the person no longer has ANY row in loses its counter row
    // entirely. Left behind it would claim activity in an environment where
    // nothing of theirs survives — the per-environment form of the same
    // misleading zero the app-wide rule rejects.
    diesel::sql_query(
        "DELETE FROM event_user_environments e \
          WHERE e.app_id = $1 AND e.distinct_id = $2 \
            AND NOT EXISTS ( \
              SELECT 1 FROM analytics_events x WHERE x.app_id = $1 \
                AND x.distinct_id = $2 AND x.environment_id IS NOT DISTINCT FROM e.environment_id \
              UNION ALL \
              SELECT 1 FROM error_events x WHERE x.app_id = $1 \
                AND x.distinct_id = $2 AND x.environment_id IS NOT DISTINCT FROM e.environment_id \
              UNION ALL \
              SELECT 1 FROM transactions x WHERE x.app_id = $1 \
                AND x.distinct_id = $2 AND x.environment_id IS NOT DISTINCT FROM e.environment_id)",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Text, _>(key)
    .execute(conn)
    .await?;

    let _ = counts;
    Ok(false)
}

/// Write recomputed counters back, or delete the row when nothing survives.
///
/// `users_seen` on `issues` is a DISTINCT count and is recomputed by the
/// statement rather than passed in — subtracting a row count from a distinct
/// count is simply wrong, which is why the whole design recomputes instead of
/// decrementing.
pub async fn apply_recomputed_rollup(
    conn: &mut AsyncPgConnection,
    kind: PurgeKind,
    app_id: Uuid,
    key: &str,
    counts: sauron_purge::recompute::Counts,
) -> QueryResult<bool> {
    let Some(t) = rollup_table(kind) else {
        return Ok(false);
    };

    if counts.is_empty() {
        // `issues.id` is a uuid and every other key column is text, so the
        // cast is per-kind. Casting in SQL rather than parsing here is
        // deliberate: an unparseable key means a corrupt touched-keys row, and
        // failing the job loudly is better than silently skipping a rollup
        // that then stays overcounting forever with nothing to detect it.
        let cast = if kind == PurgeKind::Issues {
            "::uuid"
        } else {
            ""
        };
        // Companions in the SAME statement as the rollup row, for the same
        // reason as `delete_contained_rollups`: nothing here runs inside an
        // explicit transaction, so a separate statement per table is a window
        // in which the person is gone and their per-environment counters are
        // not. `rollup_companions` is the single definition of which tables
        // that means.
        let companions = rollup_companions(kind);
        let sql = if companions.is_empty() {
            format!(
                "DELETE FROM {} WHERE app_id = $1 AND {} = $2{}",
                t.name, t.key_col, cast
            )
        } else {
            let mut ctes: Vec<String> = companions
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    format!(
                        "del_c{i} AS (DELETE FROM {c} WHERE app_id = $1 AND {kc} = $2{cast})",
                        i = i,
                        c = c,
                        kc = t.key_col,
                        cast = cast,
                    )
                })
                .collect();
            ctes.push(format!(
                "del AS (DELETE FROM {table} WHERE app_id = $1 AND {kc} = $2{cast})",
                table = t.name,
                kc = t.key_col,
                cast = cast,
            ));
            // A data-modifying CTE runs whether or not the main query
            // references it, which is what makes `SELECT 1` a sufficient body.
            format!("WITH {} SELECT 1", ctes.join(", "))
        };
        diesel::sql_query(sql)
            .bind::<SqlUuid, _>(app_id)
            .bind::<Text, _>(key)
            .execute(conn)
            .await?;
        return Ok(true);
    }

    // `event_users` carries NO counters — it holds only the person's identity
    // and span. The counters live on `event_user_environments`, one row per
    // (app, distinct_id, environment). Writing `events_count` to `event_users`
    // fails outright with `column "events_count" does not exist`, which is how
    // this was found: the delete phase had already run, so the job ended
    // `failed` with rows gone and counters stale.
    if kind == PurgeKind::Persons {
        return recompute_person(conn, app_id, key, counts).await;
    }

    let (counter_events, counter_errors) = match kind {
        PurgeKind::Issues => ("times_seen", "users_seen"),
        _ => ("events_count", "errors_count"),
    };

    if kind == PurgeKind::Issues {
        // Only `users_seen` needs the raw table. Everything else comes from
        // `counts`, which the caller already merged across BOTH tiers.
        //
        // This branch used to derive all five fields from a bare
        // `count(*) FROM error_events`, and that is a data-loss bug rather than
        // an imprecision: `sauron-tier` DETACHes and DROPs a partition once it
        // is exported (`detach_and_drop_partition`), so the hot table holds
        // only the rows inside the retention window. Recomputing from it
        // OVERWRITES `times_seen` with the hot-only count and silently discards
        // every exported occurrence. `issues.times_seen` is the only aggregate
        // for a repeated exception — `error_events` never dedups, one row per
        // occurrence — so the number the UI shows just drops, with nothing
        // anywhere reporting that it did.
        //
        // The caller does the cross-tier work correctly and fails the job
        // outright if the cold side is unreadable (see `cold_counts_for_page`);
        // the bug was purely that this branch threw the merged value away. The
        // hot-only span (`min`/`max(occurred_at)`) was wrong for the same
        // reason, so `first_seen`/`last_seen` move to `$3`/`$4` as well.
        //
        // `users_seen` is the one field that genuinely cannot be merged:
        // `Counts` carries no distinct-user figure, and distinct counts do not
        // sum across tiers anyway — a person appearing in both halves would be
        // counted twice. So it is written ONLY when the hot table is the whole
        // truth, which is exactly when `counts.errors` equals the hot row
        // count. With cold history present it keeps its existing value and
        // stays OVERCOUNTED, deliberately: overcounting is the direction this
        // subsystem already treats as the safe failure (the caller's bail
        // comment says as much), because it is recoverable and visibly
        // conservative, whereas a deflated count is indistinguishable from
        // real data loss.
        diesel::sql_query(
            "UPDATE issues i SET \
               times_seen = $3, \
               users_seen = CASE WHEN s.n = $3 THEN s.u ELSE i.users_seen END, \
               first_seen = COALESCE($4, i.first_seen), \
               last_seen = COALESCE($5, i.last_seen), \
               last_event_at = COALESCE($5, i.last_event_at), \
               updated_at = now() \
             FROM (SELECT count(*)::bigint AS n, \
                          count(DISTINCT NULLIF(distinct_id, ''))::bigint AS u \
                     FROM error_events WHERE app_id = $1 AND issue_id = $2::uuid) s \
             WHERE i.app_id = $1 AND i.id = $2::uuid",
        )
        .bind::<SqlUuid, _>(app_id)
        .bind::<Text, _>(key)
        .bind::<BigInt, _>(counts.errors)
        .bind::<Nullable<Timestamptz>, _>(counts.first)
        .bind::<Nullable<Timestamptz>, _>(counts.last)
        .execute(conn)
        .await?;
        return Ok(false);
    }

    let sql = format!(
        "UPDATE {table} SET \
           {ce} = $3, {cr} = $4, \
           {ss} = COALESCE($5, {ss}), {se} = COALESCE($6, {se}), \
           updated_at = now() \
         WHERE app_id = $1 AND {kc} = $2",
        table = t.name,
        ce = counter_events,
        cr = counter_errors,
        ss = t.span_start,
        se = t.span_end,
        kc = t.key_col,
    );
    diesel::sql_query(sql)
        .bind::<SqlUuid, _>(app_id)
        .bind::<Text, _>(key)
        .bind::<BigInt, _>(counts.events)
        .bind::<BigInt, _>(counts.errors)
        .bind::<Nullable<Timestamptz>, _>(counts.first)
        .bind::<Nullable<Timestamptz>, _>(counts.last)
        .execute(conn)
        .await?;
    Ok(false)
}

/// Bump the recompute progress counters on the job.
pub async fn record_recompute_progress(
    conn: &mut AsyncPgConnection,
    job_id: Uuid,
    worker_id: &str,
    recomputed: i64,
    deleted: i64,
) -> QueryResult<usize> {
    diesel::sql_query(
        "UPDATE purge_jobs SET \
           rollups_recomputed = rollups_recomputed + $3, \
           rollups_deleted = rollups_deleted + $4, \
           claimed_at = now() \
         WHERE id = $1 AND worker_id = $2",
    )
    .bind::<SqlUuid, _>(job_id)
    .bind::<Text, _>(worker_id)
    .bind::<BigInt, _>(recomputed)
    .bind::<BigInt, _>(deleted)
    .execute(conn)
    .await
}

/// Whether the app has received anything very recently.
///
/// Recorded at job start. Does not prevent the race — recompute against live
/// ingest drifts the moment it is written — it makes a confusing result
/// explainable afterwards instead of a mystery.
pub async fn app_ingest_active(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    within_secs: i64,
) -> QueryResult<bool> {
    #[derive(QueryableByName)]
    struct Flag {
        #[diesel(sql_type = Bool)]
        active: bool,
    }
    let row: Flag = diesel::sql_query(
        "SELECT EXISTS( \
           SELECT 1 FROM analytics_events \
            WHERE app_id = $1 AND received_at > now() - ($2 || ' seconds')::interval \
            LIMIT 1) AS active",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Text, _>(within_secs.to_string())
    .get_result(conn)
    .await?;
    Ok(row.active)
}

/// Drop a finished job's scratch rows.
///
/// The table is UNLOGGED and CASCADEs from `purge_jobs`, so this is hygiene
/// rather than correctness — but a completed job's touched set can be millions
/// of rows that nothing will ever read again.
pub async fn clear_touched_keys(conn: &mut AsyncPgConnection, job_id: Uuid) -> QueryResult<usize> {
    diesel::sql_query("DELETE FROM purge_touched_keys WHERE job_id = $1")
        .bind::<SqlUuid, _>(job_id)
        .execute(conn)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(d: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, d, 0, 0, 0).unwrap()
    }

    fn job(range: Option<(DateTime<Utc>, DateTime<Utc>)>) -> PurgeJob {
        PurgeJob {
            id: Uuid::nil(),
            org_id: Uuid::nil(),
            app_id: Uuid::nil(),
            app_slug: String::new(),
            app_name: String::new(),
            environment_ids: None,
            kinds: Value::Array(vec![]),
            range_start: range.map(|r| r.0),
            range_end: range.map(|r| r.1),
            all_time: range.is_none(),
            status: "pending".into(),
            phase: "idle".into(),
            estimated_counts: Value::Null,
            deleted_counts: Value::Null,
            rollups_recomputed: 0,
            rollups_deleted: 0,
            cold_rows_skipped: 0,
            cold_boundary_at: None,
            kind_cursor: None,
            cursor_occurred_at: None,
            cursor_id: None,
            requested_by: None,
            requested_by_email: String::new(),
            cancelled_by: None,
            cancelled_by_email: String::new(),
            cancelled_at: None,
            requested_at: at(1),
            previewed_at: None,
            confirmed_at: None,
            started_at: None,
            finished_at: None,
            confirm_source: String::new(),
            ingest_active: false,
            worker_id: None,
            claimed_at: None,
            error: String::new(),
        }
    }

    #[test]
    fn scope_clamps_the_lower_bound_to_the_cold_boundary() {
        let s = Scope::from_job(&job(Some((at(1), at(20)))), at(5)).unwrap();
        assert_eq!(s.lo, at(5), "must not try to delete rows already in cold");
        assert_eq!(s.hi, at(20));
    }

    #[test]
    fn scope_keeps_a_lower_bound_already_above_the_boundary() {
        let s = Scope::from_job(&job(Some((at(10), at(20)))), at(5)).unwrap();
        assert_eq!(s.lo, at(10));
    }

    /// A request entirely inside cold has no hot work. Returning a scope here
    /// would run a batch whose window is inverted or empty.
    #[test]
    fn a_fully_cold_range_yields_no_scope() {
        assert!(Scope::from_job(&job(Some((at(1), at(4)))), at(5)).is_none());
    }

    #[test]
    fn all_time_still_respects_the_cold_boundary() {
        let s = Scope::from_job(&job(None), at(5)).unwrap();
        assert_eq!(s.lo, at(5));
        assert_eq!(s.hi, DateTime::<Utc>::MAX_UTC);
    }

    #[test]
    fn null_environment_ids_means_every_environment() {
        assert!(parse_env_ids(None).is_none());
    }

    #[test]
    fn an_empty_array_matches_nothing_rather_than_everything() {
        let ids = parse_env_ids(Some(&Value::Array(vec![])));
        assert_eq!(ids, Some(vec![]), "must not degrade to None (= all envs)");
    }

    #[test]
    fn environment_ids_parse_from_strings() {
        let u = Uuid::from_u128(7);
        let v = Value::Array(vec![Value::String(u.to_string())]);
        assert_eq!(parse_env_ids(Some(&v)), Some(vec![u]));
    }

    /// Every rollup kind must map to a key column, and no raw kind may.
    /// A missing mapping would silently skip that rollup's repair.
    #[test]
    fn every_rollup_kind_has_a_key_column_and_a_table() {
        for k in sauron_purge::ALL {
            let is_rollup = k.class() == sauron_purge::Class::Rollup;
            assert_eq!(
                rollup_key_column(*k).is_some(),
                is_rollup,
                "key column mapping wrong for {k:?}"
            );
            assert_eq!(
                rollup_table(*k).is_some(),
                is_rollup,
                "rollup table mapping wrong for {k:?}"
            );
        }
    }

    #[test]
    fn only_error_events_carries_an_issue_id() {
        assert!(raw_table(PurgeKind::ErrorEvents).unwrap().has_issue);
        assert!(!raw_table(PurgeKind::AnalyticsEvents).unwrap().has_issue);
        assert!(!raw_table(PurgeKind::Transactions).unwrap().has_issue);
    }

    /// `env_scoped` here must agree with the pure crate's rule, or the API
    /// would accept a scope the SQL then ignores.
    #[test]
    fn rollup_env_scoping_agrees_with_the_pure_rule() {
        for k in sauron_purge::ALL {
            if let Some(t) = rollup_table(*k) {
                assert_eq!(t.env_scoped, k.env_scoped(), "disagreement on {k:?}");
            }
        }
    }
}
