//! Guest → identified person merge: alias claiming, the work queue, the hot
//! rewrites, the rollup folds and the bounded cold-overlay map.
//!
//! Kept out of `repo.rs` for the same reason `person_env_backfill` is: this is
//! a self-contained subsystem with one entry point per phase, and `repo.rs` is
//! already past 16k lines.
//!
//! See `docs/superpowers/specs/2026-08-12-guest-identity-merge-design.md`.

use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel::sql_types::{BigInt, Bool, Double, Text, Timestamptz, Uuid as SqlUuid};
use diesel_async::{AsyncPgConnection, RunQueryDsl, SimpleAsyncConnection};
use uuid::Uuid;

/// The outcome of trying to bind an anonymous id to a named person.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Claim {
    /// First claim. The caller MUST enqueue a merge.
    Fresh,
    /// The same person re-identifying under the same alias. Benign, common
    /// (every page load after login can emit one).
    ///
    /// **Not "do nothing".** [`claim_and_schedule`] RE-ARMS a completed merge
    /// on this variant — see [`rearm_merge`] for why a one-shot merge leaves a
    /// permanent double-count, and why this specific variant is the right
    /// trigger for the repair.
    Repeat,
    /// The alias is already burned to a DIFFERENT person. Not re-pointed.
    ///
    /// This is the shared-device case, and it is the only externally visible
    /// symptom of an app that never calls `reset()` on logout. Callers log and
    /// count it; without that the safety behaviour is indistinguishable from
    /// the feature being broken.
    Conflict { existing: String },
    /// Refused because it would create a chain (`a → b → c`). Keeping the map
    /// single-level is what makes `resolve()` idempotent, which in turn is what
    /// lets the cold overlay be applied to a Parquet file without caring
    /// whether it was written before or after the merge.
    Chain,
}

#[derive(QueryableByName)]
struct ClaimRow {
    #[diesel(sql_type = Text)]
    distinct_id: String,
    #[diesel(sql_type = Bool)]
    inserted: bool,
}

/// Bind `alias_id` to `distinct_id`, once and for all.
///
/// One statement, three outcomes, no read-then-write race:
///
/// * one row with `inserted = true`  → `Fresh`
/// * one row with `inserted = false` → the alias existed; compare the target
/// * **zero rows**                   → a guard filtered the `SELECT` — either
///   a `NOT EXISTS` (the insert would have formed a chain) or `alias_id =
///   distinct_id` (a self-merge, which is a degenerate one-node chain) — i.e.
///   `Chain` either way
///
/// `RETURNING (xmax = 0) AS inserted` is the house pattern already used by
/// `repo::bump_session`. The `DO UPDATE SET distinct_id = identities.distinct_id`
/// is a deliberate no-op write: `DO NOTHING` would return zero rows on
/// conflict, collapsing the "burned" and "chain" cases into one.
///
/// All FOUR `NOT EXISTS` legs are indexed — two over `identities`, two over
/// `identity_merges` (see this function's "why each guard consults BOTH
/// tables" section). `identities_app_distinct_idx` (migration 38) and
/// `identities`' `UNIQUE (app_id, alias_id)` cover the first pair;
/// `identity_merges`' own `UNIQUE (app_id, alias_id)` covers the `alias_id`
/// leg of the second, and migration 0058's `identity_merges_app_distinct_idx`
/// covers its `distinct_id` leg.
///
/// That last index is not incidental. The `distinct_id` leg is the newest of
/// the four and, without it, the ONLY unindexed one — measured at 200k rows
/// it was a parallel sequential scan costing 7.5–11.2 ms, **inside the
/// per-app advisory lock**, on every fresh claim. That alone would have taken
/// the ~440 claims/s per-app ceiling documented below down to roughly 90–150
/// and degraded it linearly forever, since `identity_merges` has no purge
/// path — i.e. the chain-guard fix would have quietly undone most of the
/// unlocked-probe fix on the same code path. With the index the same leg is
/// an Index Only Scan at 0.010 ms.
///
/// ## Self-merge is rejected too
///
/// `alias_id == distinct_id` (`claim_and_schedule(app, "x", "x")`) is refused
/// as `Chain` via a plain `alias_id <> distinct_id` filter on the same
/// `SELECT`, same shape as the two `NOT EXISTS` guards. Without it this would
/// return `Fresh`, enqueue a merge of `x` into itself, and
/// [`rewrite_hot_rows`] would then stamp `guest_alias = 'x'` across that
/// person's ENTIRE history — not just their pre-login rows — marking
/// everything they ever did as pre-login. A self-edge is the degenerate case
/// of a chain (a cycle of length one), so reusing `Chain` rather than adding a
/// new variant keeps callers' handling uniform.
///
/// ## Why this needs a per-app advisory lock
///
/// The *burn* rule — two concurrent claims of the SAME `alias_id` — needs no
/// extra locking: Postgres's `ON CONFLICT (app_id, alias_id)` machinery makes
/// the second inserter block on the first inserter's row lock and re-check
/// after it commits, so that race is already closed by the unique index
/// itself.
///
/// The *chain* guards are a different shape. Under READ COMMITTED a `NOT
/// EXISTS` subquery only sees committed rows, so two claims that touch
/// DIFFERENT keys are not ordered by any index or row lock:
///
/// * txn A: `claim_identity(app, "anon_x", "u-42")`
/// * txn B (before A commits): `claim_identity(app, "u-42", "u-99")`
///
/// B's `NOT EXISTS (... alias_id = 'u-42')`-shaped guard cannot see A's
/// uncommitted `u-42` row, so both guards pass, both insert, both commit, and
/// `identities` ends up holding a real chain (`anon_x → u-42 → u-99`) despite
/// every individual statement being correct in isolation. That silently
/// breaks `resolve()`'s single-level/idempotent guarantee, which the cold
/// overlay depends on.
///
/// `pg_advisory_xact_lock` fixes this by serialising claims **per app**
/// (never per alias/target — a chain spans both keys, so a lock on only one
/// of them would still let the other leg race). The lock is taken before the
/// guards are evaluated and is transaction-scoped (auto-released on
/// COMMIT/ROLLBACK), so B simply waits for A's transaction to finish and then
/// re-evaluates its guards against A's now-committed row, correctly landing on
/// `Chain`.
///
/// The lock key is `hashtextextended(app_id::text, 0)`, a 64-bit hash of the
/// app id — deterministic (same `app_id` always hashes to the same key) but
/// lossy (two different apps can theoretically collide). That is acceptable
/// *only* because the lock is a pure serialisation device with no bearing on
/// correctness: a collision between app X and app Y just makes an app-Y claim
/// wait on an unrelated app-X claim for the few microseconds a claim takes.
/// It can never let a chain slip through, because the guards themselves are
/// still scoped by the real `app_id`, not by the hash.
///
/// ## …and why the lock is NOT on the hot path
///
/// Everything above is true only of a claim that would actually INSERT. A
/// claim that hits an existing `identities` row creates no new edge and
/// therefore cannot form a chain, so it needs no serialisation at all — and
/// that case is the overwhelming majority of the traffic, because `identify()`
/// fires on every page load after login, not once per signup. MEASURED: 2.267
/// ms for a full locked claim transaction (a ceiling of ~440 claims/s per app,
/// since the lock serialises them) against 0.076 ms for a read-only probe.
/// The batched ingest path holds this connection for the rest of its batch
/// too, so an app past that ceiling does not just slow its own identifies down
/// — it blocks worker slots.
///
/// [`identity_probe`] therefore runs FIRST, unlocked, and the locked path
/// below is entered only on a miss. The locked path still re-evaluates every
/// guard from scratch, so correctness is unchanged: the probe is an
/// optimisation that can only ever be wrong in the direction of taking the
/// lock unnecessarily (it raced an insert) or of returning a
/// `Repeat`/`Conflict` for a row a concurrent Persons purge deleted
/// microseconds later — in which case nothing is written, the next
/// `identify()` probes again, misses, and takes the locked path. Self-healing,
/// not stuck.
///
/// The other thing the fast path avoids is a write. `DO UPDATE SET
/// distinct_id = identities.distinct_id` is a deliberate no-op *value*, but it
/// is a real row version: one dead tuple on `identities` per page load, on a
/// table migration 000060 does not tune. The probe makes the common case a
/// pure read.
///
/// Explicit `BEGIN`/`COMMIT`/`ROLLBACK` rather than `conn.transaction(|c| …)`:
/// diesel-async 0.9's closure signature needs async closures, which would push
/// the workspace MSRV past the 1.82 the RPM spec builds against. Same pattern
/// as `person_env_backfill::backfill_app`.
pub async fn claim_identity(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    alias_id: &str,
    distinct_id: &str,
) -> QueryResult<Claim> {
    if let Some(fast) = identity_probe(conn, app_id, alias_id, distinct_id).await? {
        return Ok(fast);
    }
    conn.batch_execute("BEGIN").await?;
    match claim_identity_locked(conn, app_id, alias_id, distinct_id).await {
        Ok(claim) => {
            conn.batch_execute("COMMIT").await?;
            Ok(claim)
        }
        Err(e) => {
            // Best-effort: if the ROLLBACK itself fails the connection is
            // already unusable and the pool discards it on return, which
            // aborts the transaction anyway.
            let _ = conn.batch_execute("ROLLBACK").await;
            Err(e)
        }
    }
}

#[derive(QueryableByName)]
struct ProbeRow {
    #[diesel(sql_type = Text)]
    distinct_id: String,
}

/// The unlocked read that keeps [`claim_identity`]/[`claim_and_schedule`] off
/// the advisory lock for the case that dominates the traffic.
///
/// `Some(claim)` means the outcome is already settled and NO write to
/// `identities` is possible, so the caller must skip the locked path entirely.
/// `None` means "take the lock and do it properly".
///
/// Three settled outcomes, in the order they are checked:
///
/// * **self-merge** (`alias_id == distinct_id`) → [`Claim::Chain`], with no
///   query at all. Checked FIRST and unconditionally, because the locked
///   statement expresses this as a `$2 <> $3` filter on its `SELECT` — a
///   filter the probe path never evaluates. Without this line,
///   `claim(app, "x", "x")` against an existing `x → y` row would come back
///   `Conflict { y }` on the fast path and `Chain` on the slow one: the same
///   input answered differently depending on cache state, and the
///   [`rewrite_hot_rows`] catastrophe that `Chain` exists to prevent
///   (`guest_alias = 'x'` stamped across that person's ENTIRE history) waved
///   through on whichever path happened to run.
/// * **row exists, same target** → [`Claim::Repeat`].
/// * **row exists, different target** → [`Claim::Conflict`].
///
/// ## Why a hit can skip the guards the locked path evaluates
///
/// The chain guards exist to stop a new EDGE from being created. On a hit,
/// `ON CONFLICT (app_id, alias_id)` guarantees the locked statement would have
/// inserted nothing — the edge already exists and the burn rule makes it
/// permanent — so there is no new edge for a guard to refuse and nothing to
/// serialise.
///
/// This does change one answer relative to the pre-probe implementation, and
/// it changes it toward the truth. Given `A → B` and (only reachable via the
/// purge hole [`enqueue_merge`] documents) `B → C`, a re-`identify(A, B)` used
/// to return `Chain`, because the locked statement's `NOT EXISTS (… alias_id =
/// $3)` guard filtered its `SELECT` before `ON CONFLICT` could ever fire. But
/// `identify(A, B)` forms nothing: `A → B` is already there. Reporting a chain
/// for a claim that creates no edge told an operator to go looking for a write
/// that never happened, and — because `Chain` is not `Repeat` — suppressed the
/// re-arm that [`rearm_merge`] performs. The pre-existing chain is still
/// caught, by the reader guards and by the merge worker's own
/// [`chain_conflict`] check, which are the places that can actually do
/// something about it.
///
/// ## Why a MISS cannot be trusted, and a hit can
///
/// A miss is racy by construction — another claim may insert between this
/// `SELECT` and the lock — which is exactly why a miss falls through to the
/// locked path rather than concluding `Fresh`. A hit races only against a
/// DELETE, and the sole deleter of `identities` is a Persons purge
/// (`purge::rollup_companions`). Losing that race means returning
/// `Repeat`/`Conflict` for a row that no longer exists: nothing is written,
/// and the next `identify()` — this table is read on every page load — probes
/// again, misses, and claims properly.
async fn identity_probe(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    alias_id: &str,
    distinct_id: &str,
) -> QueryResult<Option<Claim>> {
    if alias_id == distinct_id {
        return Ok(Some(Claim::Chain));
    }
    let rows: Vec<ProbeRow> =
        diesel::sql_query("SELECT distinct_id FROM identities WHERE app_id = $1 AND alias_id = $2")
            .bind::<SqlUuid, _>(app_id)
            .bind::<Text, _>(alias_id)
            .load(conn)
            .await?;

    Ok(rows.into_iter().next().map(|r| {
        if r.distinct_id == distinct_id {
            Claim::Repeat
        } else {
            Claim::Conflict {
                existing: r.distinct_id,
            }
        }
    }))
}

/// The guards, evaluated under the per-app advisory lock.
///
/// ## Why each guard consults BOTH `identities` and `identity_merges`
///
/// The no-chain invariant is *asserted* here, over `identities` — but it is
/// *consumed* from `identity_merges`, by [`cold_alias_map`] and by
/// `repo::repair_restored_rows`. Only one of those two tables is purgeable,
/// and that asymmetry is a live hole rather than a theoretical one:
///
/// `purge::rollup_companions(PurgeKind::Persons)` lists `identities` and
/// deletes it KEYED ON THE PERSON, so purging person `P` removes every
/// `identities` row that burned an alias to `P`. `identity_merges` is
/// deliberately left alone by that same purge (`purge.rs` argues the case
/// explicitly: a work queue keyed by the same id is not a companion of the
/// rollup row). So a guard that reads only `identities` sees a map that a
/// purge has emptied, while both consumers still read the surviving
/// `identity_merges` rows — and waves through exactly the claim the invariant
/// exists to refuse:
///
/// * burn `A → P`, merge it, purge `P`
/// * claim `B → C` where `B` was `A`'s target: the `identities` guard sees
///   nothing, so `identity_merges` ends up holding BOTH `A → B` and `B → C`.
///   `UNIQUE (app_id, alias_id)` cannot prevent this — different `alias_id`s.
///
/// Reading both tables in each guard puts the assertion on the same rows the
/// consumers read. `identity_merges (app_id, alias_id)` is the table's unique
/// key, and the `distinct_id` leg is the same shape `cold_alias_map`'s own
/// chain guard already runs.
async fn claim_identity_locked(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    alias_id: &str,
    distinct_id: &str,
) -> QueryResult<Claim> {
    // Transaction-scoped: released automatically at COMMIT/ROLLBACK, so a
    // panicking or erroring caller can never leave the app wedged.
    diesel::sql_query("SELECT pg_advisory_xact_lock(hashtextextended($1::text, 0))")
        .bind::<SqlUuid, _>(app_id)
        .execute(conn)
        .await?;

    let rows: Vec<ClaimRow> = diesel::sql_query(
        "INSERT INTO identities (app_id, alias_id, distinct_id) \
         SELECT $1, $2, $3 \
          WHERE $2 <> $3 \
            AND NOT EXISTS (SELECT 1 FROM identities \
                             WHERE app_id = $1 AND distinct_id = $2) \
            AND NOT EXISTS (SELECT 1 FROM identities \
                             WHERE app_id = $1 AND alias_id = $3) \
            AND NOT EXISTS (SELECT 1 FROM identity_merges \
                             WHERE app_id = $1 AND distinct_id = $2) \
            AND NOT EXISTS (SELECT 1 FROM identity_merges \
                             WHERE app_id = $1 AND alias_id = $3) \
         ON CONFLICT (app_id, alias_id) \
         DO UPDATE SET distinct_id = identities.distinct_id \
         RETURNING distinct_id, (xmax = 0) AS inserted",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Text, _>(alias_id)
    .bind::<Text, _>(distinct_id)
    .load(conn)
    .await?;

    Ok(match rows.into_iter().next() {
        None => Claim::Chain,
        Some(r) if r.inserted => Claim::Fresh,
        Some(r) if r.distinct_id == distinct_id => Claim::Repeat,
        Some(r) => Claim::Conflict {
            existing: r.distinct_id,
        },
    })
}

/// Schedule the merge for a freshly claimed alias — or REPOINT a stale one.
///
/// `UNIQUE (app_id, alias_id)` makes this idempotent under stream redelivery:
/// the alias is claimed exactly once, so it is scheduled exactly once, and a
/// duplicate delivery lands on the conflict arm rather than starting a second
/// rewrite pass.
///
/// Callers on the hot ingest path should prefer [`claim_and_schedule`], which
/// runs the claim and this enqueue in the same transaction. This function is
/// still exposed standalone because [`claim_and_schedule`] calls it directly
/// (same connection, already inside its transaction) and because it is a
/// useful unit of its own for anything that needs to (re-)enqueue a merge for
/// an alias it already knows is claimed.
///
/// ## Why the conflict arm is a REPOINT and not `DO NOTHING`
///
/// `DO NOTHING` is the obvious spelling and it is silently wrong in exactly
/// one reachable case — the same purge hole `claim_identity_locked`'s guards
/// now cover from the other side. A Persons purge deletes `identities` but
/// not `identity_merges`, so:
///
/// * `A → P` is burned, merged, `done`; person `P` is then purged
/// * a later `identify(anon = A, person = D)` finds no `identities` row and
///   claims `A → D` **Fresh** — correctly, that is the whole point of the
///   purge — and calls this function
/// * with `DO NOTHING`, the surviving `A → P` row absorbs it as a no-op. No
///   merge is ever scheduled for `A → D`, and BOTH consumers of this table
///   ([`cold_alias_map`] and `repo::repair_restored_rows`) go on resolving
///   `A` to the **purged** person `P`.
///
/// That is a wrong attribution, not a conservative non-merge, and it is
/// completely silent: `enqueue_merge` returned `0`, which is also what a
/// legitimate redelivery returns.
///
/// **Repoint rather than refuse the claim**, deliberately, because refusing is
/// the *less* conservative of the two options here. Refusing leaves the stale
/// `A → P` row in place and therefore leaves both consumers resolving `A` to a
/// person the operator explicitly erased — it preserves the wrong answer
/// instead of merely declining to compute a new one. Repointing makes the
/// table agree with `identities`, which is the source of truth the claim just
/// wrote.
///
/// This is not a breach of the burn rule (D3, "an alias is burned on first
/// claim and never re-pointed"). The burn is enforced by, and lives in,
/// `identities`; reaching this arm REQUIRES that an operator already deleted
/// that row on purpose. What is repointed here is queue residue that outlived
/// the fact it described.
///
/// Two limits, both deliberate and both disclosed rather than papered over:
///
/// * **`state <> 'running'`.** A running merge's outcome is fenced on its
///   `claimed_at` (see [`complete_merge`]); repointing `distinct_id` under a
///   live worker would let it fold the alias into the OLD person and then
///   stamp `done` on a row naming the NEW one. That window is ~seconds wide
///   and self-heals: once the row reaches `done`, the next `identify()` for
///   this alias returns [`Claim::Repeat`] and [`rearm_merge`] corrects the
///   target then.
/// * **The pre-purge history is not un-merged.** Rows that already moved from
///   `A` to `P` stay on `P` — a merge is not reversible and this does not
///   pretend otherwise. What the repoint fixes is everything from here on:
///   stragglers still carrying `A` rewrite to `D`, and the cold overlay
///   resolves `A` to `D` instead of to a purged person.
///
/// The span, `cold_stale` and `completed_at` are deliberately NOT reset. The
/// first two describe the ALIAS's activity, which does not change when its
/// target does, and both are only ever widened from here ([`fold_rollups`]) —
/// resetting them would be the one direction that can silently prune a stale
/// alias out of the cold overlay.
///
/// `completed_at` is the subtle one, and clearing it here (as an earlier
/// version of this function did) reopens that exact prune through this path.
/// [`fold_rollups`] uses `alias_first_seen IS NOT NULL OR completed_at IS NOT
/// NULL` to decide whether `cold_stale` holds a computed value or its
/// meaningless `TRUE` default. A merge that completed with an empty `moved`
/// has a NULL span, so `completed_at` is its ONLY evidence that a fold ever
/// ran; clear it and the next fold treats a conservatively-`TRUE` row as a
/// first capture and computes it down to `FALSE` off a recent straggler,
/// dropping the alias from the cold overlay for good. Reading it as "when a
/// fold for this alias last completed" rather than "when THIS target's merge
/// completed" is what makes it survive both here and in `rearm_merge`, which
/// already leaves it alone for the same reason.
pub async fn enqueue_merge(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    alias_id: &str,
    distinct_id: &str,
) -> QueryResult<usize> {
    diesel::sql_query(
        "INSERT INTO identity_merges (app_id, alias_id, distinct_id) \
         VALUES ($1, $2, $3) \
         ON CONFLICT (app_id, alias_id) DO UPDATE SET \
             distinct_id     = EXCLUDED.distinct_id, \
             state           = 'pending', \
             attempts        = 0, \
             last_error      = NULL, \
             next_attempt_at = now() \
           WHERE identity_merges.distinct_id <> EXCLUDED.distinct_id \
             AND identity_merges.state <> 'running'",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Text, _>(alias_id)
    .bind::<Text, _>(distinct_id)
    .execute(conn)
    .await
}

/// How long a re-armed merge waits before the drain sweeps it.
///
/// Picked against the three mechanisms that actually land alias-carrying rows
/// after a merge has already completed (see [`rearm_merge`]): the retry ZSET
/// replays a failed item seconds-to-minutes later, eight concurrent workers
/// drain one Redis stream with no cross-consumer ordering, and a mobile
/// offline queue flushes whenever connectivity returns. Five minutes clears
/// the first two comfortably and starts converging on the third immediately,
/// while being far enough above the drain's own 5-second idle cadence that
/// re-arming never turns into a busy loop.
///
/// The grace is a floor on latency, not a throttle on frequency — that job is
/// done by [`rearm_merge`]'s `state = 'done'` predicate, which is what makes a
/// burst of page loads re-arm at most once. Deliberately equal to
/// [`BACKOFF_CAP_SECS`]: both answer "how long may a merge lag reality before
/// somebody would call it broken", and having the same answer in two places
/// with two different numbers is how one of them silently rots.
const REARM_GRACE_SECS: f64 = 300.0;

/// Re-arm a COMPLETED merge so the drain sweeps the alias again.
///
/// ## The bug this exists to fix
///
/// Without it a merge is one-shot. `enqueue_merge` is reachable only from
/// `claim_and_schedule_locked` on [`Claim::Fresh`], and an alias is burned on
/// first claim — so [`rewrite_hot_rows`] runs exactly once, roughly 0–5
/// seconds after the identify commits, and nothing ever runs it again.
///
/// Any row carrying the alias that lands AFTER that sweep is therefore never
/// rewritten. Three entirely ordinary mechanisms produce them:
///
/// * eight concurrent workers drain one Redis stream with no cross-consumer
///   ordering, so a pre-login event can be persisted after the identify that
///   followed it;
/// * the retry ZSET replays a failed item seconds-to-minutes later;
/// * a mobile SDK's offline queue flushes pre-login events long after they
///   occurred — routinely, by design.
///
/// And the hot tier is only half of it. A guest who converted inside
/// `hot_days` gets `cold_stale = false` from [`fold_rollups`], which
/// [`cold_alias_map`] treats as "Parquet is already correct" and prunes the
/// alias out of the overlay PERMANENTLY. So the straggler double-counts in
/// both tiers, forever, with no error and no signal anywhere.
///
/// ## Why [`Claim::Repeat`] is the right trigger
///
/// It is the one event that recurs for an alias after its merge is done — the
/// `Claim` enum's own doc calls it "common — every page load after login can
/// emit one" — so using it turns the highest-volume benign case into a free
/// self-healing sweep. A re-sweep with nothing to do is genuinely cheap:
/// [`rewrite_hot_rows`]' six statements all match zero rows (and, since
/// migration 0058, all six are index-backed), and [`fold_rollups`]' two
/// `DELETE … RETURNING` CTEs consume nothing.
///
/// ## The three things this must not break
///
/// * **It must not re-sweep continuously.** `state = 'done'` is what bounds
///   it: the first repeat after a completed merge flips the row to `pending`,
///   and every repeat after that matches nothing until the drain finishes it
///   again. A page-load burst re-arms once, not once per load — and, because
///   the predicate stops matching immediately, later repeats cannot keep
///   pushing `next_attempt_at` forward and starve the sweep they asked for.
/// * **It must not let a poisoned merge retry forever.** `attempts` IS reset
///   here, and that is safe precisely because the precondition is
///   `state = 'done'`: a merge that reaches `done` succeeded. A poisoned one
///   never gets here — it lands in `dead`, which this predicate excludes and
///   nothing re-arms. The retry budget is therefore `MAX_ATTEMPTS` per
///   demonstrated success, not per identify. NOT resetting would be the
///   actual hazard: a merge that spent its budget before succeeding
///   (`attempts = MAX_ATTEMPTS`, then `complete_merge`) would be re-armed to
///   `pending` and then be unclaimable — `claim_next` gates on
///   `attempts < MAX_ATTEMPTS` and [`reap_exhausted`] only reaps `running` —
///   leaving it camped in the runnable partial index at the head of every
///   scan for good.
/// * **It must not disturb an already-captured span.** [`fold_rollups`]'
///   `s.f IS NOT NULL` guard covers the empty case (a re-run with nothing left
///   to move writes nothing), but NOT the case this change creates: a
///   straggler event calls `repo::bump_person_env`, so the alias has a NEW,
///   NARROWER `event_user_environments` row, `moved` is non-empty, and a
///   plain assignment would REPLACE the original span with the straggler's
///   own. That shrinks the window the cold overlay prunes on and can drop the
///   alias out of it. `fold_rollups` therefore widens (`LEAST`/`GREATEST`)
///   rather than assigns — see its doc comment.
///
/// `distinct_id` is rewritten to `$3` as well — but only when doing so cannot
/// form a chain. On [`Claim::Repeat`] the `identities` row already says this
/// alias points at `$3`, so the assignment is a no-op in the healthy case; it
/// is load-bearing only as the follow-up path for the one window
/// [`enqueue_merge`]'s repoint declines to touch (a stale row that was
/// `running` at the time).
///
/// ## Why the assignment is guarded and the re-arm is not
///
/// [`enqueue_merge`]'s repoint gets its chain safety for free: it runs inside
/// [`claim_and_schedule_locked`], immediately after
/// [`claim_identity_locked`]'s four guards passed under the per-app advisory
/// lock, so at that instant no `identity_merges` row can have
/// `alias_id = $3`. This function has no such backing — its dominant caller is
/// [`claim_and_schedule`]'s FAST path, which reaches it off an unlocked probe
/// that evaluates no guard at all. An unconditional `distinct_id = $3` would
/// therefore write `A → D` next to an existing `D → E`, i.e. exactly the
/// single-level invariant `resolve()` and the cold overlay depend on.
///
/// The written form is `CASE WHEN NOT EXISTS (… alias_id = $3) THEN $3 ELSE
/// distinct_id END` — the same anti-join [`claim_identity_locked`]'s fourth
/// guard and [`chain_conflict`] already run — so the REPOINT is declined
/// while the re-arm itself still happens. That split is deliberate: the
/// re-arm's actual job is the straggler sweep (see the whole first half of
/// this comment), and refusing the whole UPDATE would sacrifice C1's fix to
/// protect a self-heal that is a bonus. Declining leaves the row on its stale
/// target, which the sweep then treats as it did before this function
/// existed — conservative, and no worse than the `running`-window decline it
/// is the follow-up for.
///
/// The other chain shape — an existing row with `distinct_id = $2`, i.e.
/// `X → A` beside this row's `A → …` — is NOT guarded here, because this
/// statement cannot create it: `alias_id` is a `WHERE` key, never assigned.
/// Such a pair is pre-existing, and [`chain_conflict`] refuses it at drain
/// time in both roles.
pub async fn rearm_merge(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    alias_id: &str,
    distinct_id: &str,
) -> QueryResult<usize> {
    diesel::sql_query(
        "UPDATE identity_merges SET \
             state           = 'pending', \
             distinct_id     = CASE WHEN NOT EXISTS ( \
                                        SELECT 1 FROM identity_merges c \
                                         WHERE c.app_id = $1 AND c.alias_id = $3) \
                                    THEN $3 ELSE distinct_id END, \
             attempts        = 0, \
             last_error      = NULL, \
             next_attempt_at = now() + make_interval(secs => $4) \
          WHERE app_id = $1 AND alias_id = $2 AND state = 'done'",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Text, _>(alias_id)
    .bind::<Text, _>(distinct_id)
    .bind::<Double, _>(REARM_GRACE_SECS)
    .execute(conn)
    .await
}

/// Claim an alias and, on a fresh claim, schedule its merge — atomically.
///
/// ## Why this cannot be `claim_identity` followed by a separate `enqueue_merge`
///
/// `claim_identity` commits its own transaction (see its doc comment). If a
/// caller ran that, then called `enqueue_merge` as a second, separate
/// statement, a process death between the two would leave the alias burned —
/// the unique index makes a claim permanent — with no merge ever queued for
/// it. Because the burn rule means the alias can never be claimed again, that
/// guest's history would NEVER merge: silent, permanent loss of exactly the
/// thing this feature exists to preserve.
///
/// So the claim and the (Fresh-only) enqueue share one transaction here: the
/// same `BEGIN` / `pg_advisory_xact_lock` / `COMMIT`-or-`ROLLBACK` shape as
/// `claim_identity`, with the enqueue folded in before the `COMMIT`. Either
/// both happen or neither does.
///
/// **Callers must not invoke this from inside an already-open transaction.**
/// In Postgres a `BEGIN` while a transaction is already open is a no-op (with
/// a warning), which means the `COMMIT` below would commit the OUTER
/// transaction early instead of just this claim — silently, and
/// catastrophically on a caller that is itself inside a larger batch write.
///
/// Returns the same [`Claim`] a caller would have gotten from `claim_identity`
/// alone, so logging stays identical regardless of which path produced it.
///
/// ## How the unlocked probe and the `Repeat` re-arm interact
///
/// These two changes pull in opposite directions and the interaction is the
/// whole correctness argument, so it is spelled out rather than left to be
/// re-derived:
///
/// * [`identity_probe`] exists because the advisory lock was on the hot path
///   and `Repeat` — the case that fires on every page load — did not need it.
/// * [`rearm_merge`] then gave `Repeat` real work to do, which is exactly the
///   kind of change that silently un-does an optimisation's premise.
///
/// The premise survives, because the two are about different tables. The lock
/// serialises writes to `identities` so that no CHAIN can form; the re-arm
/// writes to `identity_merges` and creates no edge at all — it only moves one
/// existing row from `done` back to `pending`. So the fast path skips the lock
/// and still performs the re-arm, as a single autocommitted statement.
///
/// It does not need `claim_and_schedule`'s atomicity argument either. That
/// argument is about losing an enqueue after a burn the burn rule makes
/// permanent; here the alias is ALREADY burned and the queue row ALREADY
/// exists, so a process death between the probe and the re-arm loses nothing
/// but one sweep — and the next page load re-arms again. The `Fresh` path,
/// where that argument does apply, is still the transactional one below.
///
/// The failure that would be easy to ship, and that the tests pin: a fast path
/// that returns `Repeat` without re-arming. Everything stays green (the claim
/// answer is right, the merge row is right) and the straggler sweep silently
/// never happens — the exact bug the re-arm was added to fix, reintroduced by
/// the optimisation meant to be orthogonal to it.
pub async fn claim_and_schedule(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    alias_id: &str,
    distinct_id: &str,
) -> QueryResult<Claim> {
    if let Some(fast) = identity_probe(conn, app_id, alias_id, distinct_id).await? {
        if fast == Claim::Repeat {
            rearm_merge(conn, app_id, alias_id, distinct_id).await?;
        }
        return Ok(fast);
    }
    conn.batch_execute("BEGIN").await?;
    match claim_and_schedule_locked(conn, app_id, alias_id, distinct_id).await {
        Ok(claim) => {
            conn.batch_execute("COMMIT").await?;
            Ok(claim)
        }
        Err(e) => {
            // Best-effort, same reasoning as `claim_identity`: if the ROLLBACK
            // itself fails the connection is already unusable and the pool
            // discards it on return, which aborts the transaction anyway.
            let _ = conn.batch_execute("ROLLBACK").await;
            Err(e)
        }
    }
}

async fn claim_and_schedule_locked(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    alias_id: &str,
    distinct_id: &str,
) -> QueryResult<Claim> {
    let claim = claim_identity_locked(conn, app_id, alias_id, distinct_id).await?;
    match claim {
        Claim::Fresh => {
            enqueue_merge(conn, app_id, alias_id, distinct_id).await?;
        }
        // Reachable when the probe MISSED (nothing in `identities`) and a
        // concurrent claim inserted the same alias before this transaction
        // took the lock. Rare, but it must re-arm for the same reason the
        // fast path does — otherwise whether a straggler is ever swept
        // depends on which of two identical calls happened to lose a race.
        Claim::Repeat => {
            rearm_merge(conn, app_id, alias_id, distinct_id).await?;
        }
        Claim::Conflict { .. } | Claim::Chain => {}
    }
    Ok(claim)
}

/// Rewrite every hot row that names `alias` so it names `person` instead.
///
/// Each statement is idempotent: after it runs, no row matches
/// `distinct_id = alias`, so a re-run touches nothing. That is what makes
/// recovery "run the whole job again" with no per-table progress tracking.
///
/// Each runs in its own implicit transaction rather than one big one, so a
/// heavy guest does not hold a single long-lived lock across every partition.
/// None of them touch `occurred_at`, so no row moves between partitions.
///
/// Returns the total number of rows touched, for the caller's log line.
pub async fn rewrite_hot_rows(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    alias: &str,
    person: &str,
) -> QueryResult<u64> {
    let mut total = 0u64;

    for sql in [
        "UPDATE analytics_events SET distinct_id = $3, guest_alias = $2 \
          WHERE app_id = $1 AND distinct_id = $2",
        "UPDATE error_events SET distinct_id = $3, guest_alias = $2 \
          WHERE app_id = $1 AND distinct_id = $2",
        "UPDATE sessions      SET distinct_id = $3 WHERE app_id = $1 AND distinct_id = $2",
        "UPDATE transactions  SET distinct_id = $3 WHERE app_id = $1 AND distinct_id = $2",
        "UPDATE workflows     SET distinct_id = $3 WHERE app_id = $1 AND distinct_id = $2",
        "UPDATE devices SET last_distinct_id = $3 WHERE app_id = $1 AND last_distinct_id = $2",
    ] {
        total += diesel::sql_query(sql)
            .bind::<SqlUuid, _>(app_id)
            .bind::<Text, _>(alias)
            .bind::<Text, _>(person)
            .execute(conn)
            .await? as u64;
    }

    Ok(total)
}

/// Fold the alias's rollup rows into the person's, and record what the cold
/// overlay needs to know about this alias.
///
/// Both folds are written as a MOVE, not a copy: the `DELETE` in the CTE
/// consumes the source, so a second run finds nothing to move and adds nothing.
/// A plain "copy and add" would double every counter on retry, and retry is the
/// documented recovery path.
///
/// The `ON CONFLICT` target names the `COALESCE(environment_id, nil-uuid)`
/// expression from migration 0056 verbatim. It has to: the unique key is an
/// expression index, and naming `(app_id, distinct_id, environment_id)` instead
/// would make Postgres reject the statement outright with `42P10 there is no
/// unique or exclusion constraint matching the ON CONFLICT specification` —
/// loud, not silent — but that is still worth avoiding on its own terms: it
/// would take the whole fold down with it rather than degrading gracefully.
///
/// No two `moved` rows can collide on one conflict target — the alias's own
/// rows are already unique per environment key — so this cannot trip
/// "ON CONFLICT DO UPDATE command cannot affect row a second time".
///
/// The `alias_first_seen`/`alias_last_seen`/`cold_stale` span capture rides in
/// the SAME statement as this fold, off the SAME `moved` CTE this statement
/// already reads in order to do its `DELETE` — see the comment on that part
/// of the statement for why `event_user_environments`, specifically, is the
/// right source. This makes span capture atomic with the move and
/// independent of whether [`rewrite_hot_rows`] has already run.
///
/// The span and `cold_stale` only ever WIDEN — `LEAST`/`GREATEST`/`OR`, never
/// assignment. That is what makes this safe to run a second time against an
/// alias that has since acquired a late, narrow straggler row, which is
/// exactly what [`rearm_merge`] arranges; see the inline comment for the
/// silent cold-overlay prune a plain assignment causes there.
pub async fn fold_rollups(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    alias: &str,
    person: &str,
    hot_days: i64,
) -> QueryResult<()> {
    const NIL: &str = "'00000000-0000-0000-0000-000000000000'::uuid";

    // The `alias_first_seen`/`alias_last_seen`/`cold_stale` span capture rides
    // in this statement, off the SAME `moved` CTE this fold already reads to
    // do its `DELETE` — not off the `event_users` fold below, and not off
    // `analytics_events`. It has to be `event_user_environments`, specifically,
    // because the span and its two consumers must share ONE clock:
    //
    // * `event_user_environments.first_seen`/`last_seen` are EVENT time —
    //   `repo::bump_person_env` binds both to the analytics event's own
    //   `occurred_at`.
    // * `event_users.first_seen`/`last_seen` are INGEST time — every writer
    //   (`upsert_event_user`, `touch_event_user`, the batched
    //   `touch_event_users`) stamps them from `now()` at write time, never
    //   from `occurred_at`.
    //
    // Both consumers of the span compare it against EVENT time: the cold
    // overlay's window prune matches an `occurred_at` range on Parquet, and
    // `cold_stale` is a proxy for "was any of this alias's data already
    // exported when the rewrite ran", where export is driven by `occurred_at`
    // versus the tier watermark. Ingest time is always >= event time, so
    // sourcing the span from `event_users` instead shifts it later — and for
    // a client that queues events offline and flushes them well after they
    // occurred (the offline-flush shape a mobile SDK produces routinely),
    // "later" can cross the hot/cold boundary entirely: `cold_stale` would
    // come out `false` for a guest whose data was already exported, and that
    // guest's cold-tier double-count would never be corrected. The
    // `hot_days - 1` day margin exists to absorb exactly this kind of race,
    // but only protects against it when both sides of the comparison are on
    // the same clock; it provides no protection once they are not.
    //
    // `moved` can hold MULTIPLE rows (a guest active in several
    // environments), so the span is an aggregate: `min(first_seen)` /
    // `max(last_seen)` across every row this fold is moving.
    //
    // `SELECT min(...), max(...)` over zero input rows returns ONE row of
    // NULLs, not zero rows — a different trap from the `IS NOT NULL` guard a
    // now-superseded version of this comment used to describe. Without the
    // guard below, a re-run against an alias with nothing left to move (its
    // `event_user_environments` rows were already consumed by a prior run)
    // would blank an already-captured span back to NULL and try to write NULL
    // into `cold_stale`, which is `NOT NULL` — the guard turns that into the
    // no-op a re-run needs instead.
    //
    // THE SPAN WIDENS, IT DOES NOT ASSIGN — and the `IS NOT NULL` guard is
    // NOT sufficient for that on its own. It covers a re-run with an EMPTY
    // `moved`; it does nothing for a re-run whose `moved` is non-empty but
    // NARROWER, which is precisely the shape `rearm_merge` now produces on
    // purpose. A straggler event that lands after the first merge calls
    // `repo::bump_person_env`, so the alias gets a FRESH
    // `event_user_environments` row spanning only that one late event. A
    // plain `alias_first_seen = s.f` would then replace the guest's real span
    // with the straggler's, shrinking the window `cold_alias_map` prunes on —
    // and a window that no longer covers the guest's actual activity prunes
    // that alias straight out of the cold overlay. `LEAST`/`GREATEST` ignore
    // NULL operands (the result is NULL only when every operand is), so the
    // same expression serves the first capture and every widening after it
    // with no special case.
    //
    // `cold_stale` widens too, but it CANNOT be a plain `OR` with the column,
    // and getting that wrong is a silent, total loss of the prune. The
    // column's default is `TRUE` — conservative, meaning "not computed yet",
    // NOT "known stale" — so `m.cold_stale OR …` is `TRUE OR …` on every
    // first capture and pins every alias that ever merges to `TRUE` forever.
    // That is not a wrong number (over-marking is the safe direction) but it
    // deletes the prune the design doc calls out as removing "the large
    // majority" of the overlay, and it does so with every test still green
    // except the one that asserts a hot-window guest comes out `false`.
    //
    // The discriminator has to answer "is `m.cold_stale` a COMPUTED value, or
    // the untouched `TRUE` default", and `alias_first_seen IS NOT NULL` alone
    // is the WRONG proxy for that — it fails on exactly the row
    // `cold_alias_map`'s arm 3 exists to protect. A merge can reach `done`
    // with an EMPTY `moved` (an anon id older than migration 0056, rollups
    // already purged, or every pre-login rollup write landing after the
    // fold): the `s.f IS NOT NULL` guard skips the whole UPDATE, so the span
    // stays NULL and `cold_stale` stays at its `TRUE` default. Arm 3 then
    // keeps that alias in the overlay, correctly, because a NULL span means
    // "unknown", not "outside the window". But a later re-fold — which only
    // `rearm_merge` made reachable, so this is a hazard THIS change
    // introduced — sees `NULL IS NOT NULL` = FALSE, treats a `done` row as a
    // first capture, and recomputes `cold_stale` from the straggler's own
    // recent timestamp. It comes out FALSE, arms 3 and 4 both stop matching,
    // and the alias vanishes from the overlay entirely: C1's own failure mode
    // (permanent, silent cold-tier double-count) reintroduced in the one case
    // the design doc marks "cannot prove this is safe to drop".
    //
    // `completed_at` is the honest marker, because the question is "has a
    // fold ever completed for this alias", not "did a fold ever find
    // anything". `complete_merge` is its only writer and only runs after
    // `fold_rollups`, so non-NULL implies a fold ran; and it survives
    // `rearm_merge` untouched. Either signal is sufficient, hence the `OR`:
    // a captured span proves a fold moved rows, a `completed_at` proves a
    // fold ran at all.
    //
    // Given that, the rule is: keep a previously computed `TRUE` (once
    // an alias is known stale in Parquet, no later re-run may argue it back
    // to clean — a straggler is recent by definition, so its own freshly
    // computed value is almost always `false` and assigning it would UNDO the
    // original merge's `true`), and otherwise recompute against the WIDENED
    // first_seen, which is `LEAST(m.alias_first_seen, s.f)` — the same value
    // the `alias_first_seen` assignment above lands on, since every SET
    // expression in an UPDATE reads the row's pre-update values. The widened
    // form can only ever move `false` toward `true` as the watermark
    // advances, which is again the safe direction.
    //
    // `cold_stale` is deliberately conservative: over-marking costs a few
    // extra overlay rows and a slower cold query, under-marking is a silently
    // wrong number. The extra day covers the tier watermark advancing between
    // enqueue and this statement.
    let env_fold = format!(
        "WITH moved AS ( \
             DELETE FROM event_user_environments \
              WHERE app_id = $1 AND distinct_id = $2 \
             RETURNING environment_id, first_seen, last_seen, \
                       events_count, errors_count, sessions_count), \
         ins AS ( \
             INSERT INTO event_user_environments \
                 (app_id, distinct_id, environment_id, first_seen, last_seen, \
                  events_count, errors_count, sessions_count) \
             SELECT $1, $3, environment_id, first_seen, last_seen, \
                    events_count, errors_count, sessions_count \
               FROM moved \
             ON CONFLICT (app_id, distinct_id, COALESCE(environment_id, {NIL})) \
             DO UPDATE SET \
                 first_seen     = LEAST(event_user_environments.first_seen, EXCLUDED.first_seen), \
                 last_seen      = GREATEST(event_user_environments.last_seen, EXCLUDED.last_seen), \
                 events_count   = event_user_environments.events_count   + EXCLUDED.events_count, \
                 errors_count   = event_user_environments.errors_count   + EXCLUDED.errors_count, \
                 sessions_count = event_user_environments.sessions_count + EXCLUDED.sessions_count, \
                 updated_at     = now()) \
         UPDATE identity_merges m SET \
             alias_first_seen = LEAST(m.alias_first_seen, s.f), \
             alias_last_seen  = GREATEST(m.alias_last_seen, s.l), \
             cold_stale       = ((m.alias_first_seen IS NOT NULL \
                                  OR m.completed_at IS NOT NULL) AND m.cold_stale) \
                                OR LEAST(m.alias_first_seen, s.f) \
                                   < now() - make_interval(days => ($4::int - 1)) \
           FROM (SELECT min(first_seen) AS f, max(last_seen) AS l FROM moved) s \
          WHERE m.app_id = $1 AND m.alias_id = $2 AND s.f IS NOT NULL"
    );

    diesel::sql_query(env_fold)
        .bind::<SqlUuid, _>(app_id)
        .bind::<Text, _>(alias)
        .bind::<Text, _>(person)
        .bind::<diesel::sql_types::Integer, _>(hot_days as i32)
        .execute(conn)
        .await?;

    // Person-days move with the identity, and they UNION rather than sum.
    //
    // Same `DELETE … RETURNING` + `INSERT … ON CONFLICT` shape as `env_fold`
    // above, and the union falls out of `person_days_key`: the alias and the
    // person having both been active on one day is a conflict, so their two
    // rows collapse into one with the counters added.
    //
    // A plain `UPDATE person_days SET distinct_id = person` would not merely
    // miscount — it would raise a unique violation on exactly that overlapping
    // day, which is the COMMON case rather than the rare one: an identify()
    // typically fires on a day the guest was already active.
    //
    // Untouched deliberately: the person's cohort. It is derived from
    // `event_user_environments.first_seen`, which `env_fold` above has just
    // widened with `LEAST`, so a guest who identifies moves to the EARLIER
    // cohort automatically. That is correct — they were always that person —
    // and it does mean historical cohorts shift under a merge, which the
    // dashboard footnotes.
    diesel::sql_query(format!(
        "WITH moved AS ( \
             DELETE FROM person_days \
              WHERE app_id = $1 AND distinct_id = $2 \
             RETURNING environment_id, day, events, errors) \
         INSERT INTO person_days (app_id, environment_id, distinct_id, day, events, errors) \
         SELECT $1, environment_id, $3, day, events, errors FROM moved \
         ON CONFLICT (app_id, COALESCE(environment_id, {NIL}), distinct_id, day) \
         DO UPDATE SET events = person_days.events + EXCLUDED.events, \
                       errors = person_days.errors + EXCLUDED.errors, \
                       updated_at = now()"
    ))
    .bind::<SqlUuid, _>(app_id)
    .bind::<Text, _>(alias)
    .bind::<Text, _>(person)
    .execute(conn)
    .await?;

    // `properties` is concatenated ANON-FIRST so the person's identify() traits
    // win: jsonb `||` lets the right-hand side override. identified_at and
    // identified_source are left untouched — the surviving row is already
    // stamped by process_identify, and the alias never was.
    //
    // No span capture here — see the comment on the fold above for why
    // `event_user_environments`, not this table, is the span's source.
    diesel::sql_query(
        "WITH moved AS ( \
             DELETE FROM event_users WHERE app_id = $1 AND distinct_id = $2 \
             RETURNING properties, first_seen, last_seen) \
         INSERT INTO event_users (id, app_id, distinct_id, properties, first_seen, last_seen) \
         SELECT gen_random_uuid(), $1, $3, properties, first_seen, last_seen FROM moved \
         ON CONFLICT (app_id, distinct_id) DO UPDATE SET \
             first_seen = LEAST(event_users.first_seen, EXCLUDED.first_seen), \
             last_seen  = GREATEST(event_users.last_seen, EXCLUDED.last_seen), \
             properties = EXCLUDED.properties || event_users.properties, \
             updated_at = now()",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Text, _>(alias)
    .bind::<Text, _>(person)
    .execute(conn)
    .await?;

    Ok(())
}

// ===========================================================================
// The drain queue: claim, complete, fail
// ===========================================================================

/// Hard retry cap. A poisoned merge parks in `dead` for inspection rather
/// than spinning the worker forever.
pub const MAX_ATTEMPTS: i32 = 5;

/// How long a merge may sit `running` before it is presumed orphaned — the
/// worker that claimed it died between `claim_next` and
/// `complete_merge`/`fail_merge` — and becomes claimable again.
///
/// Nothing else ever resets a `running` row on its own; a worker dying in
/// that window (a deploy landing mid-drain, an OOM, `fail_merge` itself
/// erroring on the very connection that just failed) would otherwise strand
/// the row in `running` forever — and because the burn rule means the alias
/// can never be claimed again, that guest's history would simply never
/// merge, silently. `claim_next`'s predicate reclaims a `running` row whose
/// `claimed_at` is older than the lease AND still has attempts left;
/// [`reap_exhausted`] covers the other half — a `running` row whose lease
/// expired with NO attempts left, which must not be reclaimed (there is
/// nothing left to retry) but still needs a terminal state, or it would sit
/// in `running` — camping at the head of every scan via the same partial
/// index — forever, indistinguishable from a merge actually in progress.
///
/// 15 minutes: long enough that a genuinely slow merge on a heavy guest is
/// never stolen mid-flight, short enough that a deploy's orphans are picked
/// up on the very next drain cycle rather than waiting for a human. Safe to
/// steal even if the original worker is not actually dead — `rewrite_hot_rows`
/// is idempotent and `fold_rollups`'s `s.f IS NOT NULL` guard means a re-run
/// never blanks an already-captured span — so a stolen-but-still-live merge is
/// at worst duplicated work, never corruption. `complete_merge`/`fail_merge`
/// additionally fence on the exact `claimed_at` they were handed, so if BOTH
/// the original worker and the one that stole the lease eventually try to
/// write a terminal state, only the current claim's write can land — the
/// stale one finds nothing to update instead of overwriting the winner.
pub const RUNNING_LEASE_MINUTES: i64 = 15;

/// Base backoff before a failed merge is retried, doubled per attempt and
/// capped at [`BACKOFF_CAP_SECS`].
///
/// Without backoff, a merge that fails against anything longer-lived than a
/// moment (a lock timeout, a deadlock with the tier or purge workers) is
/// re-claimed immediately by the same drain loop — the failed row is oldest
/// by `created_at`, so it is the very next thing `claim_next` picks up — and
/// all `MAX_ATTEMPTS` attempts land back-to-back inside the same contention
/// window. A transient fault then becomes permanent silent loss by the same
/// route `RUNNING_LEASE_MINUTES` closes for a dead worker.
const BACKOFF_BASE_SECS: f64 = 30.0;
/// Ceiling on the backoff delay.
const BACKOFF_CAP_SECS: f64 = 300.0;

/// One unit of merge work.
///
/// `claimed_at` doubles as a FENCING TOKEN: `complete_merge`/`fail_merge` both
/// require it to match the row's current `claimed_at` before writing a
/// terminal state, so a worker whose lease was stolen (see
/// [`RUNNING_LEASE_MINUTES`]) cannot overwrite the outcome the thief already
/// recorded — its terminal write simply matches nothing.
#[derive(Debug, Clone, QueryableByName)]
pub struct PendingMerge {
    #[diesel(sql_type = SqlUuid)]
    pub id: Uuid,
    #[diesel(sql_type = SqlUuid)]
    pub app_id: Uuid,
    #[diesel(sql_type = Text)]
    pub alias_id: String,
    #[diesel(sql_type = Text)]
    pub distinct_id: String,
    #[diesel(sql_type = Timestamptz)]
    pub claimed_at: DateTime<Utc>,
}

/// Take the oldest runnable merge and mark it `running`.
///
/// "Runnable" is a `pending`/`failed` row whose backoff has elapsed, OR a
/// `running` row that both still has attempts left AND whose lease has
/// expired (see [`RUNNING_LEASE_MINUTES`]) — the latter is what lets a worker
/// that died mid-merge be recovered by the next drain cycle instead of
/// staying stranded forever. `attempts < $1` gates BOTH arms, deliberately: a
/// `running` row with no attempts left is not reclaimed here at all — see
/// [`reap_exhausted`], which gives that case its own terminal transition
/// instead of spending one more (wasted, if idempotent) attempt on a row
/// already known to be doomed.
///
/// `claimed_at IS NULL` also counts as expired. `claim_next` is the only
/// writer of `state = 'running'` and always stamps `claimed_at` in the same
/// statement, so a `running` row with a NULL `claimed_at` is unreachable
/// through this code — the guard is defensive, purely against hand-written
/// SQL (an admin `UPDATE`, a future backfill) leaving one behind. Without it
/// such a row would compare NULL against the lease expression, evaluate to
/// NULL (neither true nor false), and strand permanently — the exact failure
/// mode this whole lease exists to close, just reached through a different
/// door. [`reap_exhausted`] carries the identical guard for the same reason.
///
/// `FOR UPDATE SKIP LOCKED` so several replicas can drain the same queue
/// without contending or double-running a merge. A single UPDATE ...
/// RETURNING, so this is autocommitted as one implicit transaction — it does
/// not need (and must not use) an explicit `BEGIN`/`COMMIT` of its own.
pub async fn claim_next(conn: &mut AsyncPgConnection) -> QueryResult<Option<PendingMerge>> {
    let rows: Vec<PendingMerge> = diesel::sql_query(
        "UPDATE identity_merges SET state = 'running', attempts = attempts + 1, claimed_at = now() \
          WHERE id = (SELECT id FROM identity_merges \
                       WHERE attempts < $1 \
                         AND ((state IN ('pending', 'failed') AND next_attempt_at <= now()) \
                              OR (state = 'running' \
                                  AND (claimed_at IS NULL \
                                       OR claimed_at < now() - make_interval(mins => $2)))) \
                       ORDER BY created_at FOR UPDATE SKIP LOCKED LIMIT 1) \
         RETURNING id, app_id, alias_id, distinct_id, claimed_at",
    )
    .bind::<diesel::sql_types::Integer, _>(MAX_ATTEMPTS)
    .bind::<diesel::sql_types::Integer, _>(RUNNING_LEASE_MINUTES as i32)
    .load(conn)
    .await?;
    Ok(rows.into_iter().next())
}

/// Reap a `running` row whose lease expired with NO attempts left, straight
/// to the terminal `dead` state.
///
/// `claim_next`'s reclaim arm will not touch a row like this — `attempts <
/// $1` gates it out on purpose, so a worker that dies on its LAST attempt
/// does not get handed one more (wasted) attempt just to reach `dead` via
/// `fail_merge` the normal way. Without this function that row has no path
/// to a terminal state at all: excluded from both arms of `claim_next`'s
/// predicate, it would sit in `running` — which IS in the runnable partial
/// index — at the head of every scan forever, indistinguishable from a merge
/// genuinely in progress to anyone reading the table.
///
/// `last_error` is filled only if still empty: a row that already failed at
/// least once before its final, fatal claim already carries a real message
/// from `fail_merge`; this only supplies one for the case where the very
/// first (and last) claim's worker died before ever calling `fail_merge`.
pub async fn reap_exhausted(conn: &mut AsyncPgConnection) -> QueryResult<usize> {
    diesel::sql_query(
        "UPDATE identity_merges SET \
             state = 'dead', \
             last_error = COALESCE(last_error, \
                 'reaped: the worker holding this merge never reported an outcome, and its \
                  attempts were exhausted') \
          WHERE state = 'running' AND attempts >= $1 \
            AND (claimed_at IS NULL OR claimed_at < now() - make_interval(mins => $2))",
    )
    .bind::<diesel::sql_types::Integer, _>(MAX_ATTEMPTS)
    .bind::<diesel::sql_types::Integer, _>(RUNNING_LEASE_MINUTES as i32)
    .execute(conn)
    .await
}

/// Is this merge part of a chain, in EITHER role?
///
/// ## Why the writer needs this even though both readers already have it
///
/// [`cold_alias_map`] and `repo::repair_restored_rows` both carry a chain
/// guard. The function that actually MUTATES the data did not, and a reader
/// guard cannot protect data a writer has already destroyed:
///
/// * **`guest_alias` is overwritten, irreversibly.** Given `A → B` and
///   `B → C`, running the `B → C` job makes [`rewrite_hot_rows`] match every
///   row whose `distinct_id` is `B` — which, after `A → B` ran, includes
///   `A`'s rows — and stamps `guest_alias = 'B'` over the `'A'` already
///   there. The design doc's "Out of scope" section keeps `guest_alias` at
///   its ORIGINAL value specifically so an unmerge stays possible later;
///   after this there is nothing left to unmerge back to. No reader guard can
///   restore it.
/// * **The two tiers end up disagreeing, permanently.** Backoff and
///   `ORDER BY created_at` do not guarantee the jobs run in edge order. Run
///   `B → C` first and then `A → B`, and `A`'s rows land on `B` — a person
///   who no longer exists as a distinct identity, since `B`'s own rows are
///   now `C`. The readers' guard then EXCLUDES `A → B` from the overlay
///   (`B` is somebody's alias), so hot says `C`, cold says `A`, and the guest
///   is split-counted for good. The readers behaving correctly is what makes
///   the disagreement permanent rather than self-correcting.
///
/// So this is checked in BOTH roles, which is strictly wider than the
/// readers' one-sided guard: refuse a job whose person is somebody's alias
/// (`A → B` when `B → C` exists — matching the readers, so hot and cold agree
/// on the same set), AND refuse a job whose alias is somebody's person
/// (`B → C` when `A → B` exists — the `guest_alias` clobber, which the
/// readers' shape does not cover because it is a write-only hazard).
///
/// Chains are refused at claim time ([`claim_identity_locked`], now over both
/// tables) so nothing should reach here; this is the layer that keeps a
/// bypass — a hand-written backfill, a row created before that guard covered
/// `identity_merges` — from being written into the events themselves.
pub async fn chain_conflict(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    alias_id: &str,
    distinct_id: &str,
) -> QueryResult<bool> {
    #[derive(QueryableByName)]
    struct Chained {
        #[diesel(sql_type = Bool)]
        chained: bool,
    }
    let row: Chained = diesel::sql_query(
        "SELECT (EXISTS (SELECT 1 FROM identity_merges \
                          WHERE app_id = $1 AND alias_id = $3) \
              OR EXISTS (SELECT 1 FROM identity_merges \
                          WHERE app_id = $1 AND distinct_id = $2)) AS chained",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Text, _>(alias_id)
    .bind::<Text, _>(distinct_id)
    .get_result(conn)
    .await?;
    Ok(row.chained)
}

/// Send a merge straight to the terminal `dead` state, skipping the retry
/// budget entirely.
///
/// For a refusal that is a PROPERTY OF THE DATA rather than a transient
/// fault — today, exactly one caller: the drain's [`chain_conflict`] check.
/// Routing that through [`fail_merge`] would spend `MAX_ATTEMPTS` claims and
/// four backoff windows re-deciding a question whose answer cannot change,
/// and would log a "merge failed" line five times for one permanent
/// condition.
///
/// Not left `pending` instead, which would be the other obvious choice:
/// `pending` sits in `identity_merges_runnable_idx`, so a permanently
/// unrunnable row would camp at the head of every `claim_next` scan (oldest
/// by `created_at`) forever AND hold the drain's pending gauge permanently
/// off zero, destroying the one signal that gauge exists to give. `dead`
/// leaves the runnable index, is counted by [`dead_merge_count`], and carries
/// the reason in `last_error`.
///
/// Same fence as [`complete_merge`]/[`fail_merge`], for the same reason: only
/// the caller still holding the current claim may write a terminal state.
pub async fn dead_letter_merge(
    conn: &mut AsyncPgConnection,
    id: Uuid,
    claimed_at: DateTime<Utc>,
    err: &str,
) -> QueryResult<usize> {
    diesel::sql_query(
        "UPDATE identity_merges SET state = 'dead', last_error = $3 \
          WHERE id = $1 AND state = 'running' AND claimed_at = $2",
    )
    .bind::<SqlUuid, _>(id)
    .bind::<Timestamptz, _>(claimed_at)
    .bind::<Text, _>(err)
    .execute(conn)
    .await
}

/// One state bucket's row count, for the drain's once-per-pass depth log.
#[derive(Debug, Clone, QueryableByName)]
pub struct QueueDepthRow {
    #[diesel(sql_type = Text)]
    pub state: String,
    #[diesel(sql_type = BigInt)]
    pub n: i64,
}

/// Queue depth for the three RUNNABLE states (`'pending'`, `'failed'`,
/// `'running'`), for `merge.rs::drain_once`'s once-per-pass log line — the
/// design doc's failure-mode table promises a "pending-merge gauge" and
/// nothing before this built one, so a stuck or parked merge had no signal
/// at all.
///
/// Deliberately does NOT also select `'dead'` — see [`dead_merge_count`]'s
/// doc comment for why that has to be a separate query, not an added value
/// in this one's `WHERE`. The predicate here is written to match
/// `identity_merges_runnable_idx`'s own partial-index `WHERE` clause EXACTLY
/// (`state IN ('pending', 'failed', 'running')`), which is what lets Postgres
/// use it: a partial index only serves a query whose predicate provably
/// IMPLIES the index's predicate. This query's cost is bounded by the live
/// backlog (an index scan over the partial index), not by total historical
/// merge volume — confirmed by measurement, see [`dead_merge_count`] for the
/// number that motivated splitting these apart.
pub async fn queue_depth_by_state(conn: &mut AsyncPgConnection) -> QueryResult<Vec<QueueDepthRow>> {
    diesel::sql_query(
        "SELECT state, count(*)::bigint AS n FROM identity_merges \
          WHERE state IN ('pending', 'failed', 'running') GROUP BY state",
    )
    .load(conn)
    .await
}

/// Count of terminal `'dead'` rows. A SEPARATE query from
/// [`queue_depth_by_state`] on purpose — an earlier version of this module
/// answered both with one `WHERE state <> 'done'` (or, equivalently, `state
/// IN (..., 'dead')`), and that shape defeats `identity_merges_runnable_idx`
/// ENTIRELY, not just for `'dead'`: Postgres will only use a partial index
/// when the query predicate provably implies the index's own predicate, and
/// `'dead'` escaping `state IN ('pending', 'failed', 'running')` breaks that
/// implication for the whole `OR`. Measured at 200k rows (199,999 `'done'`,
/// 1 `'dead'`):
///
/// ```text
/// WHERE state <> 'done'                          → Parallel Seq Scan, 2,894 buffers
/// WHERE state IN ('pending','failed','running')  → Index Scan,            1 buffer
/// ```
///
/// Splitting them apart is what lets the common case (the actual "pending-
/// merge gauge" signal: is there a backlog right now) stay index-backed.
///
/// This query has no such escape hatch of its own, and until migration 0058
/// grew `identity_merges_dead_idx` it had no index either: measured at 200k
/// rows it was a 3,636-buffer / 10 ms sequential scan of the WHOLE table,
/// unconditionally, run every 5 seconds by every replica. `identity_merges`
/// gains a row per signup and has no purge path
/// (`sauron_db::purge::rollup_companions`'s doc comment defers it to "its own
/// worker", which does not exist yet), so that cost only ever grew.
///
/// `identity_merges_dead_idx` is `(app_id) WHERE state = 'dead'`, and this
/// count is now an index-only scan over the dead rows alone — bounded by how
/// many merges have actually died, which in a healthy deployment is zero. The
/// indexed column is `app_id` rather than `id` because a count over a partial
/// index is index-only whichever column is stored, so `(app_id)` is free here
/// and additionally serves [`cold_alias_map`]'s per-app `'dead'` arm; `(id)`
/// would have served only this query.
///
/// This is the same trap the split above documents, applied to the other half
/// of the pair: a partial index only serves a query whose predicate provably
/// implies the index's predicate. `WHERE state = 'dead'` implies
/// `WHERE state = 'dead'` exactly, which is why one line of DDL was the whole
/// fix once the query had been written not to `OR` its way out of it.
pub async fn dead_merge_count(conn: &mut AsyncPgConnection) -> QueryResult<i64> {
    #[derive(QueryableByName)]
    struct Count {
        #[diesel(sql_type = BigInt)]
        n: i64,
    }
    let row: Count =
        diesel::sql_query("SELECT count(*)::bigint AS n FROM identity_merges WHERE state = 'dead'")
            .get_result(conn)
            .await?;
    Ok(row.n)
}

/// Mark a merge finished — but only if `claimed_at` still matches, i.e. this
/// caller still holds the current claim.
///
/// The fencing check (`claimed_at = $2`, not just `id = $1`) exists because
/// the lease makes a job reachable by two workers at once whenever the first
/// is merely slow rather than dead: a merge across every partition for a
/// heavy guest, or one waiting on a lock, can plausibly run past
/// [`RUNNING_LEASE_MINUTES`]. Without the fence, a second worker's
/// `complete_merge` and the first worker's eventual (stale) terminal write
/// would both land — whichever runs last wins, silently overwriting a
/// genuinely successful merge with a lost one or vice versa. Returns `0`
/// (not an error) when the fence does not match, so callers can log the
/// steal rather than silently losing the signal.
///
/// `state IN ('running', 'dead')`, not `state = 'running'` alone: the second
/// member exists because [`reap_exhausted`] can mark a row `dead` while its
/// ORIGINAL claim holder is still genuinely working past its lease (the same
/// merge-runs-longer-than-the-lease scenario as the fencing race above) — the
/// reap never touches `claimed_at`, so that worker's token still matches. If it then
/// finishes successfully, this must let it correct the record: the merge
/// really happened, and a `'dead'` row saying otherwise is the exact
/// silently-wrong direction (an alias whose cold-tier history was actually
/// already folded would look, to anything gating on `state = 'done'`, like it
/// never was — never corrected, because a dead row is never retried). This
/// does NOT reopen the round-2 hazard: a row that was actually *re-claimed*
/// (not just reaped) has a DIFFERENT `claimed_at` — minted fresh by
/// `claim_next` — so the original worker's stale token still fails the
/// `claimed_at = $2` half of the fence regardless of which state the row is
/// in. `fail_merge` deliberately does NOT get the same widening: a late
/// failure report on an already-`dead` row has nothing to correct — `dead` is
/// already the right terminal answer — so admitting it would only let a late
/// writer reset `last_error` for no benefit.
pub async fn complete_merge(
    conn: &mut AsyncPgConnection,
    id: Uuid,
    claimed_at: DateTime<Utc>,
) -> QueryResult<usize> {
    diesel::sql_query(
        "UPDATE identity_merges SET state = 'done', completed_at = now(), last_error = NULL \
          WHERE id = $1 AND state IN ('running', 'dead') AND claimed_at = $2",
    )
    .bind::<SqlUuid, _>(id)
    .bind::<Timestamptz, _>(claimed_at)
    .execute(conn)
    .await
}

/// Park a merge for another attempt — or permanently, once [`MAX_ATTEMPTS`]
/// is exhausted — retaining the error for inspection.
///
/// Same fencing as [`complete_merge`] and for the identical reason: only a
/// caller whose `claimed_at` still matches the row's current claim may write
/// a terminal state, so a stolen job's original (merely slow, not dead)
/// worker cannot flip an already-completed merge back to `failed`/`dead`
/// after the fact. See `complete_merge`'s doc comment for the full race.
///
/// `attempts` was already incremented by the `claim_next` that produced this
/// job, so it counts the attempt that just failed. Once it reaches
/// `MAX_ATTEMPTS` the row moves to the terminal `dead` state instead of
/// staying `failed`: `failed` sits in the runnable partial index, so an
/// unkillable `failed` row would otherwise camp at the head of every scan
/// (oldest by `created_at`) forever, past the point retrying it means
/// anything.
///
/// `next_attempt_at` is set with exponential backoff — see
/// [`BACKOFF_BASE_SECS`]/[`BACKOFF_CAP_SECS`] — so `claim_next` will not pick
/// this row back up until the delay elapses.
pub async fn fail_merge(
    conn: &mut AsyncPgConnection,
    id: Uuid,
    claimed_at: DateTime<Utc>,
    err: &str,
) -> QueryResult<usize> {
    diesel::sql_query(
        "UPDATE identity_merges SET \
             state = CASE WHEN attempts >= $4 THEN 'dead' ELSE 'failed' END, \
             last_error = $3, \
             next_attempt_at = now() + make_interval(secs => \
                 LEAST($5 * power(2.0, GREATEST(attempts - 1, 0)::double precision), $6)) \
          WHERE id = $1 AND state = 'running' AND claimed_at = $2",
    )
    .bind::<SqlUuid, _>(id)
    .bind::<Timestamptz, _>(claimed_at)
    .bind::<Text, _>(err)
    .bind::<diesel::sql_types::Integer, _>(MAX_ATTEMPTS)
    .bind::<Double, _>(BACKOFF_BASE_SECS)
    .bind::<Double, _>(BACKOFF_CAP_SECS)
    .execute(conn)
    .await
}

// ===========================================================================
// The cold overlay: the bounded alias → person map the DuckDB read path joins
// ===========================================================================

/// One alias → person edge, as the cold overlay consumes it.
#[derive(Debug, Clone, QueryableByName)]
pub struct AliasEntry {
    #[diesel(sql_type = Text)]
    pub alias: String,
    #[diesel(sql_type = Text)]
    pub person: String,
}

/// The alias edges a cold query over `[from, to)` could possibly need.
///
/// Unbounded, this map is one row per converted device per app — millions at
/// scale, shipped into DuckDB on every query. Two prunes bound it, and BOTH
/// apply only to merges that have actually completed:
///
/// * `cold_stale = false` — every row was still hot when the merge ran, so the
///   rewrite fixed them before export and Parquet is already correct.
/// * span vs. window — the alias was never active in the queried range.
///
/// `state <> 'done'` short-circuits both. Until the fold runs, the span is NULL
/// and `cold_stale` is its conservative default, so pruning on either would
/// drop an alias whose hot rewrite has ALSO not landed yet — the one window in
/// which a row is stale in both tiers simultaneously.
///
/// ## A `done` row can ALSO have a NULL span — and that must not be pruned either
///
/// `fold_rollups`'s span capture is guarded by `s.f IS NOT NULL`: an alias
/// whose `moved` CTE (over `event_user_environments`) comes back empty — its
/// activity was entirely cold already, or predates that table — leaves
/// `alias_first_seen`/`alias_last_seen` NULL even after the merge reaches
/// `done`. Without the `IS NULL` escape hatch below, `NULL < $3` and
/// `NULL >= $2` both evaluate to SQL NULL, `cold_stale AND NULL` is NULL, and
/// `FALSE OR NULL` is not TRUE — so the row is silently excluded from the map
/// on a span it does not have, and that guest double-counts in every cold
/// query forever with no error anywhere. A NULL span means "unknown", not
/// "outside the window", so it gets the same conservative "cannot prove this
/// is safe to drop" treatment `state <> 'done'` already gives the in-flight
/// case. Do not simplify this back to a plain inequality: the inequality
/// alone is correct only when both timestamps are non-NULL.
///
/// ## Defence in depth against a chain
///
/// Chains are refused at claim time (`claim_identity_locked`'s `NOT EXISTS`
/// guards under a per-app advisory lock) — that is the actual, load-bearing
/// defence, and nothing in this module deletes from `identities`, so a chain
/// should never reach this query. The `NOT EXISTS` below is insurance on top
/// of that: if the invariant were ever violated (a bug, a hand-written
/// backfill), a plain `COALESCE` here only resolves one hop, so `X → Y, Y →
/// Z` would silently return `X → Y` and `Y → Z` as if they were two
/// independent, correct edges instead of the broken chain they are. Excluding
/// any row whose `person` is itself claimed as somebody else's `alias` means
/// a broken invariant shows up as a stale (but never wrong) alias, not a
/// wrong resolution. Do not remove this thinking the claim-time guard alone
/// is enough — it is enough only as long as it is never bypassed.
///
/// ## Why this is FOUR arms `UNION ALL`'d and not one `WHERE`
///
/// Everything above describes a SELECTION, and the selection is unchanged.
/// What changed is the shape it is written in, because the natural shape was
/// an unindexable one. As a single predicate, the `OR` spanning
/// `state <> 'done'` / `cold_stale` / `alias_first_seen IS NULL` cannot be
/// proved to imply any partial index's predicate, so Postgres fell back to a
/// full scan — MEASURED at 200k rows: 7,438 buffers / 22.0 ms for a 30-day
/// window and 3,636 buffers / 12.2 ms for a ONE-day window.
/// `identity_merges_app_span_idx` was chosen in NEITHER case, and the pair of
/// numbers is the tell: a real window prune shrinks with the window, a
/// sequential scan only shrinks by how much it can discard after reading
/// everything. So the cost was O(every signup this deployment has ever seen),
/// on a DASHBOARD READ PATH — `tier_read.rs` rebuilds the DuckDB overlay from
/// this on every cold query — which makes it per request, not per merge.
///
/// This is [`dead_merge_count`]'s trap one level up: a partial index only
/// serves a query whose predicate provably implies the index's. Each arm is
/// written so that it does.
///
/// | arm | predicate | index | measured (200k merges / 730 days / 30-day window) |
/// |---|---|---|---|
/// | 1 | `state IN ('pending','failed','running')` | `identity_merges_runnable_idx` | 41 buffers / 0.13 ms |
/// | 2 | `state = 'dead'` | `identity_merges_dead_idx` | 24 buffers / 0.09 ms |
/// | 3 | `state = 'done' AND cold_stale AND alias_first_seen IS NULL` | `identity_merges_cold_window_idx` | 689 buffers / 1.62 ms |
/// | 4 | `state = 'done' AND cold_stale AND` span overlaps the window | `identity_merges_app_span_idx` | 436 buffers / 2.47 ms |
///
/// **Arm 4 rides `app_span_idx`, NOT `cold_window_idx`** — one index per arm,
/// and neither covers the other's. Arm 4's predicate implies BOTH partial
/// indexes, so it is tempting to assume the more specific one wins and that
/// `app_span_idx` is therefore redundant; measurement says the opposite, for
/// a structural reason. `cold_window_idx` leads on `alias_first_seen`, and
/// arm 4's bound on that column (`< $window_end`) is near-unbounded for any
/// window ending near now — 65,958 of 200,000 rows matched it. Forcing arm 4
/// onto `cold_window_idx` costs 3,710 buffers / 13.18 ms, 8.5x the buffers.
/// The selective half of arm 4 is `alias_last_seen >= $window_start`, which
/// only `app_span_idx` carries. Do not drop either index.
///
/// Arms 1 and 2 together are exactly the old `state <> 'done'`: the `CHECK`
/// on `state` enumerates five values and this splits them four-and-one, so
/// the union is the same set while each half now names something an index can
/// be proved against. **A sixth state added to that `CHECK` must be added to
/// arm 1 or arm 2 too**, or it silently drops out of the overlay — that is
/// the one maintenance burden this rewrite creates, and it is why the arms
/// enumerate states positively instead of writing `state <> 'done'` and
/// hoping an index turns up.
///
/// Arms 3 and 4 are disjoint by construction: `alias_first_seen < $3` is
/// false (not NULL-true) for a NULL span, so a NULL-span row can only ever
/// match arm 3. That is what keeps `UNION ALL` — no dedup, no sort — correct
/// rather than needing `UNION`. Arms 1/2 against 3/4 are disjoint by `state`.
/// A duplicate here would not be cosmetic: this map is registered as a DuckDB
/// temp table and joined, so a doubled edge doubles the rows it resolves.
///
/// The chain guard is applied ONCE, outside the union, rather than per arm.
/// It is the same anti-join over the same unique key `(app_id, alias_id)` for
/// every row regardless of which arm produced it, and four copies is four
/// chances for one to drift.
pub async fn cold_alias_map(
    conn: &mut AsyncPgConnection,
    app_id: Uuid,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> QueryResult<Vec<AliasEntry>> {
    diesel::sql_query(
        "SELECT m.alias_id AS alias, m.distinct_id AS person \
           FROM ( SELECT app_id, alias_id, distinct_id FROM identity_merges \
                   WHERE app_id = $1 AND state IN ('pending', 'failed', 'running') \
                  UNION ALL \
                  SELECT app_id, alias_id, distinct_id FROM identity_merges \
                   WHERE app_id = $1 AND state = 'dead' \
                  UNION ALL \
                  SELECT app_id, alias_id, distinct_id FROM identity_merges \
                   WHERE app_id = $1 AND state = 'done' AND cold_stale \
                     AND alias_first_seen IS NULL \
                  UNION ALL \
                  SELECT app_id, alias_id, distinct_id FROM identity_merges \
                   WHERE app_id = $1 AND state = 'done' AND cold_stale \
                     AND alias_first_seen < $3 AND alias_last_seen >= $2 ) m \
          WHERE NOT EXISTS (SELECT 1 FROM identity_merges c \
                             WHERE c.app_id = m.app_id AND c.alias_id = m.distinct_id)",
    )
    .bind::<SqlUuid, _>(app_id)
    .bind::<Timestamptz, _>(from)
    .bind::<Timestamptz, _>(to)
    .load(conn)
    .await
}
