//! The guest → identified merge drain.
//!
//! Co-located in `sauron-ingest` rather than given its own binary: it already
//! owns identity writes and has the pool, and a new bin would mean touching
//! `packaging/rpm/binaries.txt` and the systemd units for no benefit.
//!
//! Off the per-item path on purpose — a merge rewrites every row a guest ever
//! produced, which must never be in the way of accepting an envelope.
//!
//! ## Why this does not wrap its own transaction
//!
//! Among every function in `sauron_db::identity_merge`, only `claim_identity`
//! and `claim_and_schedule` open an explicit `BEGIN`/`COMMIT` of their own —
//! neither is called from here. `claim_next`/`complete_merge`/`fail_merge`
//! are each a single autocommitted statement, and `rewrite_hot_rows`/
//! `fold_rollups` are deliberately a SEQUENCE of autocommitted statements
//! (see their doc comments: one implicit transaction per table/fold, not one
//! big one, so a heavy guest never holds a single long-lived lock across
//! every partition). Wrapping the calls below in an explicit transaction of
//! this loop's own would both defeat that design (turning several short locks
//! into one held for the whole merge) and — per `claim_and_schedule`'s doc
//! comment — risk the silent-COMMIT-of-the-outer-transaction hazard if a call
//! to `claim_identity`/`claim_and_schedule` were ever added inside it. So this
//! loop stays plain: each call below runs and commits on its own.
use std::sync::Mutex;
use std::time::Duration;

use sauron_db::identity_merge as im;
use sauron_db::{repo, PgPool};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

/// How long to wait when the queue is empty. Merges are not latency-critical —
/// a few seconds of double-counting right after signup is invisible — so this
/// favours an idle deployment doing almost nothing.
const IDLE_SLEEP: Duration = Duration::from_secs(5);

/// Last-logged `(pending, running, failed, dead)` tuple from the queue-depth
/// log line below. Exists solely to gate that line's severity — see its call
/// site. `'dead'` is terminal and never purged, and a `'failed'` row can sit
/// out a backoff for minutes, so without this, either one turns the line into
/// a permanent 5-second heartbeat: ~17,280 identical `INFO` lines/day,
/// forever, for a value that never moves. A `std::sync::Mutex` (not
/// `tokio::sync::Mutex`) is correct here — it is held only across a plain
/// compare-and-store with no `.await` inside the critical section.
static LAST_QUEUE_DEPTH: Mutex<(i64, i64, i64, i64)> = Mutex::new((0, 0, 0, 0));

/// Drain merges until the queue is empty, then sleep. Never returns.
///
/// `configured_hot_days` is the process's own `TIER_HOT_DAYS`/`cfg.tier_hot_days`
/// — a FALLBACK, not the value actually used. `drain_once` resolves the real,
/// possibly operator-overridden value itself on every pass; see its doc
/// comment for why that has to happen there and not once here at spawn time.
pub fn spawn_merge_worker(pool: PgPool, configured_hot_days: i64) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match drain_once(&pool, configured_hot_days).await {
                Ok(0) => tokio::time::sleep(IDLE_SLEEP).await,
                Ok(_) => {}
                Err(e) => {
                    warn!(error = %e, "merge drain failed; backing off");
                    tokio::time::sleep(IDLE_SLEEP).await;
                }
            }
        }
    })
}

/// Run one drain pass: claim and execute merges until the queue has nothing
/// left runnable, then return. `pub` (rather than private) so
/// `sauron-pipeline`'s own integration tests can exercise the whole
/// claim → rewrite → fold → complete/fail sequence directly, the way
/// `crate::batch::process_batch` already is — a hand-rolled call sequence in
/// a test proves the primitives work but not that this function actually
/// wires them the same way.
///
/// ## Why `hot_days` is resolved HERE, once per pass, not once at process boot
///
/// It is operator-tunable at runtime (`runtime_settings['tier.hot_days']`) —
/// same setting, same reasoning as `sauron-tier`'s own `cycle` function keeps
/// it out of `main`. `sauron-ingest` restarts far less often than `sauron-tier`
/// ticks, so a boot-time snapshot can drift for weeks before a value change
/// ever takes effect.
///
/// The drift matters here in the DANGEROUS direction. `hot_days` reaches
/// exactly one place: `fold_rollups`'s
/// `cold_stale = alias_first_seen < now() - make_interval(days => hot_days - 1)`.
/// An operator LOWERING the rotation age (30 → 7, the common storage-saving
/// move) while this drain still computes against a stale, higher value
/// UNDER-marks `cold_stale` — exactly the direction `fold_rollups`'s own doc
/// comment calls silently wrong (over-marking only costs a slower cold
/// query; under-marking means an alias whose data is already in Parquet never
/// gets a cold-overlay row, so its cold-tier history stays double-counted
/// forever). A boot-time read here would reintroduce that.
///
/// Resolved once per PASS, not once per JOB: `effective_tier_hot_days` is one
/// round trip, this drain can process an unbounded number of jobs per pass,
/// and every job in one pass should see the same cutoff regardless of where
/// in the batch it lands — the same reasoning `sauron-tier`'s `cycle` uses for
/// resolving it once per cycle rather than once per table.
pub async fn drain_once(pool: &PgPool, configured_hot_days: i64) -> anyhow::Result<usize> {
    let mut conn = sauron_db::conn(pool).await?;

    // Queue-depth LOG LINE (not a metric/gauge — `sauron-telemetry`'s
    // Prometheus surface at `sauron-telemetry/src/metrics.rs` is Redis/probe-
    // derived with a test pinning its exact counter/gauge count, so wiring a
    // Postgres-sourced value into it is a separate, bigger change than "log
    // it"; nothing can alert on this today). The design doc's failure-mode
    // table promises "surfaced as a pending-merge gauge" and nothing before
    // now built even this much, so a stuck `'failed'` row or a `'dead'` one
    // had no signal at all.
    //
    // Taken BEFORE this pass's own claim loop touches anything, so it reads
    // as "how much was waiting when this pass woke up", not "how much is
    // left after we just drained it" — the latter would read near-zero on
    // every healthy pass and hide exactly the backlog this exists to
    // surface. Two queries, not one — `queue_depth_by_state` (index-backed,
    // bounded by the live backlog) and `dead_merge_count` (NOT index-backed;
    // see its own doc comment for the measured cost and why it cannot be
    // folded into the first query without losing the index for BOTH) — see
    // `identity_merge::queue_depth_by_state`/`dead_merge_count`.
    let runnable = im::queue_depth_by_state(&mut conn).await?;
    let mut pending = 0i64;
    let mut running = 0i64;
    let mut failed = 0i64;
    for row in &runnable {
        match row.state.as_str() {
            "pending" => pending = row.n,
            "running" => running = row.n,
            "failed" => failed = row.n,
            other => warn!(
                state = other,
                n = row.n,
                "identity_merges: unrecognized state from queue_depth_by_state"
            ),
        }
    }
    let dead = im::dead_merge_count(&mut conn).await?;

    // Silent when there is nothing outside `'done'` to report at all — the
    // drain wakes every `IDLE_SLEEP`, and a line of zeros on every idle pass
    // of an idle deployment is pure noise. Otherwise: `INFO` only when the
    // tuple actually changed since the last pass that logged anything,
    // `DEBUG` (off by default) for an unchanged repeat — see
    // `LAST_QUEUE_DEPTH`'s doc comment for why an unconditional `INFO` here
    // becomes a permanent heartbeat. The mutex is always updated (even on
    // the fully-idle branch) so a LATER nonzero reading is compared against
    // the true previous value rather than a stale one from before an idle
    // gap.
    let current = (pending, running, failed, dead);
    let changed = {
        let mut last = LAST_QUEUE_DEPTH.lock().expect("queue depth mutex poisoned");
        let changed = *last != current;
        *last = current;
        changed
    };
    if current != (0, 0, 0, 0) {
        if changed {
            info!(
                pending,
                running, failed, dead, "identity_merges queue depth"
            );
        } else {
            debug!(
                pending,
                running, failed, dead, "identity_merges queue depth (unchanged)"
            );
        }
    }

    let hot_days = repo::effective_tier_hot_days(&mut conn, configured_hot_days).await?;
    if hot_days != configured_hot_days {
        info!(
            configured_hot_days,
            hot_days, "merge drain using an operator-overridden hot_days"
        );
    }

    // `claim_next`'s reclaim arm deliberately never touches a `running` row
    // with no attempts left (see its doc comment) — reap that case straight
    // to `dead` here instead, once per pass, rather than spending one more
    // attempt on a row already known to be doomed. Without this a worker
    // that dies on its LAST attempt would strand the row in `running`
    // forever, indistinguishable from a merge genuinely in progress.
    let reaped = im::reap_exhausted(&mut conn).await?;
    if reaped > 0 {
        warn!(
            reaped,
            "reaped exhausted, orphaned merges to 'dead' — the worker(s) holding them died \
             without reporting an outcome"
        );
    }

    let mut done = 0usize;

    while let Some(job) = im::claim_next(&mut conn).await? {
        // The chain guard on the WRITE side. Both readers of the alias map
        // (`cold_alias_map`, `repo::repair_restored_rows`) already refuse to
        // resolve a chained edge; this is the only place that stops one from
        // being written into the event rows themselves, where no reader guard
        // can undo it — see `im::chain_conflict` for the two specific,
        // permanent corruptions (`guest_alias` clobbered past recovery, and
        // hot/cold split-counting the same guest forever).
        //
        // Checked after the claim rather than folded into `claim_next`'s
        // predicate so the refusal is OBSERVABLE: a filtered-out row would be
        // invisible and would sit in the runnable index at the head of every
        // scan for good. `dead_letter_merge`, not `fail_merge`, because a
        // chain is a property of the data — retrying it five times re-decides
        // a question whose answer cannot change.
        if im::chain_conflict(&mut conn, job.app_id, &job.alias_id, &job.distinct_id).await? {
            let reason = format!(
                "refused: merging {} into {} would traverse an alias chain; one of these ids is \
                 already the other side of another merge for this app. Running it would overwrite \
                 guest_alias irreversibly and split this guest across the hot and cold tiers.",
                job.alias_id, job.distinct_id
            );
            warn!(
                app_id = %job.app_id, alias = %job.alias_id, person = %job.distinct_id,
                "refusing a chained merge; parking it in 'dead' for inspection"
            );
            im::dead_letter_merge(&mut conn, job.id, job.claimed_at, &reason).await?;
            continue;
        }

        let outcome = async {
            let rows = im::rewrite_hot_rows(&mut conn, job.app_id, &job.alias_id, &job.distinct_id)
                .await?;
            im::fold_rollups(
                &mut conn,
                job.app_id,
                &job.alias_id,
                &job.distinct_id,
                hot_days,
            )
            .await?;
            Ok::<u64, diesel::result::Error>(rows)
        }
        .await;

        match outcome {
            Ok(rows) => {
                // A 0 here means this job's lease was stolen and the thief
                // already recorded an outcome (`complete_merge`'s doc comment
                // has the full race) — the only signal that ever happens is
                // this write finding nothing, so it is logged rather than
                // silently swallowed.
                let updated = im::complete_merge(&mut conn, job.id, job.claimed_at).await?;
                if updated == 0 {
                    warn!(
                        app_id = %job.app_id, alias = %job.alias_id, person = %job.distinct_id,
                        "merge succeeded but its lease had already been reclaimed by another \
                         worker; not overwriting whatever outcome that worker recorded"
                    );
                } else {
                    info!(
                        app_id = %job.app_id, alias = %job.alias_id, person = %job.distinct_id, rows,
                        "merged a guest into an identified person"
                    );
                    done += 1;
                }
            }
            Err(e) => {
                // Every step is idempotent or consuming, so a partially applied
                // merge is safe to run again from the top.
                warn!(app_id = %job.app_id, alias = %job.alias_id, error = %e, "merge failed");
                let updated =
                    im::fail_merge(&mut conn, job.id, job.claimed_at, &e.to_string()).await?;
                if updated == 0 {
                    // A 0 here is NOT specifically a lease steal, which is
                    // what an earlier version of this line claimed. The fence
                    // is `state = 'running' AND claimed_at = $2`, and by far
                    // the commonest way to miss it is `reap_exhausted`: this
                    // worker ran past `RUNNING_LEASE_MINUTES` on its LAST
                    // attempt, the reap moved the row to 'dead' without
                    // touching `claimed_at`, and now the `state` half of the
                    // fence fails. A genuine steal (another `claim_next`
                    // minting a fresh `claimed_at`) is the rarer case. Naming
                    // only the rare one sends an operator to look for a second
                    // worker that does not exist.
                    //
                    // The error text is logged HERE because this branch is the
                    // only place it survives: `fail_merge` writes it to
                    // `last_error`, and a fence miss means that write matched
                    // nothing — so without this the actual reason the merge
                    // failed is discarded, and the row an operator eventually
                    // inspects carries either a reap's placeholder or an older
                    // attempt's message instead.
                    warn!(
                        app_id = %job.app_id, alias = %job.alias_id, error = %e,
                        "merge failed and its terminal write was fenced out — the row was reaped \
                         to 'dead', or (rarer) another worker reclaimed the lease; not \
                         overwriting whatever outcome is already recorded. This log line is the \
                         only surviving copy of the error"
                    );
                }
            }
        }
    }

    Ok(done)
}
