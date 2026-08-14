-- 0058: guest → identified person merge.
--
-- MUST RUN BEFORE RESTARTING sauron-ingest.
--
-- Correcting an earlier, wrong version of this warning, which claimed that
-- without the migration "every identify() is still recorded, but no merge is
-- ever enqueued". That is not what happens. The claim and the enqueue share
-- ONE transaction (`identity_merge::claim_and_schedule` — see its doc comment
-- for why they cannot be split), so a missing `identity_merges` table makes
-- the enqueue statement error and the WHOLE transaction roll back: the
-- `identities` row is not recorded either. The alias is therefore NOT burned,
-- which is the one saving grace here — a later identify() after the migration
-- lands still claims it Fresh and still merges that guest's history. The rest
-- of `process_identify` (the `event_users` upsert, the identified_at stamp) is
-- autocommitted on the same connection BEFORE this transaction opens, so it
-- survives; only the alias link is dropped, and the guest/identified
-- double-count this feature exists to remove stays in place for as long as
-- the mismatch lasts. The ingest worker logs a warning per identify
-- (`claiming an alias failed`) rather than failing a request, so no envelope
-- is rejected and the dashboard looks exactly as it does today — which is
-- precisely why this warning is here.
--
-- See docs/superpowers/specs/2026-08-12-guest-identity-merge-design.md

-- The derived pre-login marker. Nullable with no default, so ADD COLUMN is
-- metadata-only and every existing row reads NULL without a rewrite. An event
-- happened pre-login iff `guest_alias IS NOT NULL`; nothing is written at
-- ingest, only by the merge job.
--
-- Deliberately NOT added to sessions/transactions/workflows: those get their
-- distinct_id rewritten like everything else, but "was this pre-login" is a
-- question about events. Easy to add later, hard to remove.
ALTER TABLE analytics_events ADD COLUMN guest_alias TEXT;
ALTER TABLE error_events     ADD COLUMN guest_alias TEXT;

-- The merge work queue.
--
-- A dedicated table rather than state columns on `identities`, because
-- `identities` is a pure map read on the hot cold-overlay path and must stay
-- narrow. UNIQUE (app_id, alias_id) makes enqueueing idempotent under
-- redelivery: the alias is claimed exactly once, so it is scheduled exactly
-- once.
--
-- alias_first_seen/alias_last_seen/cold_stale are NULL/TRUE until the
-- `event_user_environments` fold fills them, off the same `moved` CTE that
-- fold already reads in order to delete it. It has to be that table and not
-- `event_users`: `event_user_environments` timestamps are EVENT time (bound
-- to the analytics event's own `occurred_at`), while `event_users` timestamps
-- are INGEST time (stamped from `now()` at write time) — and the span is
-- compared against event time by both of its consumers. Until the fold runs,
-- the cold overlay MUST NOT prune on them — see the selection query in
-- sauron-db/src/identity_merge.rs::cold_alias_map.
--
-- `state` carries a fifth, terminal value beyond the obvious four: 'dead'.
-- A row that exhausts MAX_ATTEMPTS moves there instead of staying 'failed'
-- forever — 'failed' is in the runnable partial index below, so an
-- unkillable 'failed' row would sit, unclaimable (attempts >= MAX_ATTEMPTS),
-- at the head of every scan for good, since the scan orders by created_at
-- and this is the oldest row left. 'dead' is excluded from the index and is
-- self-documenting for an operator querying the table.
--
-- 'dead' is TERMINAL FOR THE DRAIN, not immutable: an earlier version of this
-- comment called it "the only state this row will ever occupy again", which
-- the very next paragraph already contradicted (`complete_merge` fences on
-- `state IN ('running','dead')` precisely so a genuinely-still-running worker
-- can correct a prematurely reaped row to 'done'). Nothing in `claim_next`
-- ever picks a 'dead' row back up, and nothing re-arms one — that is the
-- guarantee that actually matters and the only one made here. The two writers
-- that CAN move a row out of 'dead' are `complete_merge` (above) and, going
-- the other way, nothing at all.
--
-- `claimed_at`/`next_attempt_at` exist for the two failure modes a "claim,
-- do the work, mark done" queue has to survive on its own:
--
-- * `claimed_at` backs a LEASE, and also doubles as a FENCING TOKEN. Without
--   it a worker that dies between claim_next and complete_merge/fail_merge (a
--   deploy landing mid-drain, the process OOMing, fail_merge itself erroring
--   on the connection that just failed) would strand that row in 'running'
--   PERMANENTLY: the burn rule means the alias can never be reclaimed by a
--   fresh identify(), so that guest's history would simply never merge,
--   silently. claim_next's claim predicate accepts a 'running' row whose
--   `claimed_at` is older than the lease (and still has attempts left — see
--   reap_exhausted below for the row that does not), so an orphan is picked
--   back up on the very next drain cycle instead of waiting for a human.
--   Reclaiming a merge that is still genuinely in flight is safe by
--   construction — `rewrite_hot_rows` is idempotent and `fold_rollups`'s
--   `s.f IS NOT NULL` guard means a re-run never blanks an already-captured
--   span — so a stolen-but-live merge is at worst duplicated work, never
--   corruption to the ANALYTICS data. The QUEUE ROW is a separate concern:
--   `complete_merge`/`fail_merge` fence their writes on the exact
--   `claimed_at` they were handed (`state IN ('running','dead') AND
--   claimed_at = $n` / `state = 'running' AND claimed_at = $n`), so a job
--   whose lease was stolen — its worker merely slow, not actually dead —
--   cannot have its outcome overwritten by whichever of the two workers'
--   terminal writes happens to run last. `reap_exhausted`'s reap does not
--   touch `claimed_at`, so a worker that is genuinely still running past its
--   OWN lease can still land `complete_merge` afterward and correct a 'dead'
--   row a reap marked prematurely — `complete_merge`'s widened fence exists
--   for exactly that. See `sauron-db/src/identity_merge.rs`'s doc comments on
--   `claim_next`/`reap_exhausted`/`complete_merge`/`fail_merge` for the full
--   reasoning, including the two-workers race this closes.
--
-- * `next_attempt_at` backs BACKOFF. Without it, a merge that fails against
--   anything longer-lived than a moment (a lock timeout, a deadlock with the
--   tier or purge workers) gets re-claimed immediately by the same drain
--   loop, since the failed row is oldest by `created_at` and therefore
--   claimable next — so all MAX_ATTEMPTS attempts land back-to-back inside
--   the same contention window and the merge parks in 'dead' having never
--   truly been retried. fail_merge sets this with exponential backoff
--   (starting ~30s, doubling per attempt, capped at 5 minutes), and
--   claim_next's predicate requires it to have passed.
CREATE TABLE identity_merges (
    id                UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    app_id            UUID NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    alias_id          TEXT NOT NULL,
    distinct_id       TEXT NOT NULL,
    state             TEXT NOT NULL DEFAULT 'pending'
                      CHECK (state IN ('pending', 'running', 'done', 'failed', 'dead')),
    attempts          INT  NOT NULL DEFAULT 0,
    last_error        TEXT,
    alias_first_seen  TIMESTAMPTZ,
    alias_last_seen   TIMESTAMPTZ,
    cold_stale        BOOLEAN NOT NULL DEFAULT TRUE,
    claimed_at        TIMESTAMPTZ,
    next_attempt_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at      TIMESTAMPTZ,
    UNIQUE (app_id, alias_id)
);

-- The drain's claim query. Partial, because the queue is overwhelmingly
-- 'done'/'dead' once steady state is reached and only the runnable tail is
-- ever scanned. Covers 'running' as well as 'pending'/'failed' now, so the
-- lease-reclaim branch of claim_next's WHERE clause is indexed too; the
-- `attempts < $1` / `next_attempt_at <= now()` / lease-expiry checks stay as
-- heap-side filters, same as `attempts < $1` already was before this index
-- existed in its current form.
CREATE INDEX identity_merges_runnable_idx
    ON identity_merges (created_at)
    WHERE state IN ('pending', 'failed', 'running');

-- The `distinct_id` side of the table, which TWO hot queries read and which
-- nothing else here indexes.
--
-- `UNIQUE (app_id, alias_id)` above covers every lookup that asks "what is
-- this alias bound to". Both chain guards ask the mirror-image question —
-- "is this id already somebody's TARGET" — and no index answered it:
--
-- * `identity_merge::claim_identity_locked`'s fourth `NOT EXISTS` leg, which
--   runs INSIDE the per-app advisory lock on every fresh claim. MEASURED at
--   200k rows: parallel Seq Scan, 3,629 buffers, 7.5-11.2 ms. The locked
--   claim itself is 2.267 ms (a ~440 claims/s per-app ceiling, since the lock
--   serialises them), so this leg alone would have cut that to ~90-150 — and
--   kept degrading, because `identity_merges` gains a row per signup and has
--   no purge path.
-- * `identity_merge::chain_conflict`, once per drain job: 9.2 ms, of which
--   the `alias_id` leg is 0.014 ms and this leg is all the rest. Re-armed
--   merges recur per active alias, so this is the dominant per-job cost.
--
-- MEASURED with the index: 7.5 ms -> 0.010 ms and 9.2 ms -> 0.061 ms, both
-- Index Only Scan with `Heap Fetches: 0`. 9.7 MB at 200k rows.
--
-- NOT partial: `distinct_id` is `NOT NULL` here (unlike the nullable
-- `distinct_id` on the signal tables below), so there is nothing to exclude.
CREATE INDEX identity_merges_app_distinct_idx
    ON identity_merges (app_id, distinct_id);

-- The cold overlay's WINDOW arm (arm 4). Load-bearing — do not drop it.
--
-- Originally this index went unused, because `cold_alias_map` was one
-- predicate whose `OR` across `state <> 'done'` / `cold_stale` /
-- `alias_first_seen IS NULL` could not be proved to imply any partial index
-- (measured then: 7,438 buffers / 22.0 ms for a 30-day window, 3,636 / 12.2
-- for a one-day window — a full scan either way). Now that the query is four
-- separate arms, this index is what serves the arm that actually reads by
-- span.
--
-- MEASURED, 200k merges spread over 730 days, 30-day window:
--
--   arm 4, as shipped:                Bitmap Index Scan on THIS index
--                                       436 buffers /  2.47 ms
--   arm 4, this index dropped:        Bitmap Index Scan on cold_window_idx
--                                     3,710 buffers / 13.18 ms   (8.5x / 5.3x)
--
-- The reason is structural rather than a sampling accident, so it will not
-- drift back: `identity_merges_cold_window_idx` leads on `alias_first_seen`,
-- and arm 4's bound on that column (`alias_first_seen < $window_end`) is
-- near-unbounded for any window ending near now — it matched 65,958 of
-- 200,000 rows here. The leading column therefore has almost no selectivity,
-- and the scan pays for the difference in heap fetches. `alias_last_seen >=
-- $window_start` is the selective half, and THIS index is the one that has it.
--
-- The two indexes are not redundant with each other: this one serves arm 4,
-- `cold_window_idx` serves arm 3's `IS NULL` probe (which falls to a Parallel
-- Seq Scan without it, measured). Each arm has exactly one index and neither
-- index covers the other's arm.
CREATE INDEX identity_merges_app_span_idx
    ON identity_merges (app_id, alias_last_seen);

-- `cold_alias_map`'s NULL-SPAN escape hatch (arm 3). Also load-bearing.
--
-- The partial predicate is `state = 'done' AND cold_stale`, exactly the pair
-- of conditions arm 3 carries verbatim, so Postgres can prove the
-- implication. `alias_first_seen` leads because arm 3 discriminates on it as
-- an `IS NULL` probe, which btree serves natively. MEASURED at 200k rows: arm
-- 3 is an Index Scan here at 1.62 ms; drop this index and it becomes a
-- Parallel Seq Scan at 9.38 ms.
--
-- It is NOT the index for arm 4, despite arm 4's predicate also implying this
-- one — see `identity_merges_app_span_idx` above for the measurement and for
-- why leading on a near-unbounded range column loses to leading on the
-- selective one. Both indexes exist because each serves one arm; neither is
-- redundant.
--
-- Together they are what make the cold overlay's cost O(this app's *stale*
-- completed merges in the window) instead of O(every signup this deployment
-- has ever seen). This sits on a dashboard read path (`tier_read.rs` builds
-- the DuckDB overlay from it on every cold query), so that difference is per
-- request, not per merge.
CREATE INDEX identity_merges_cold_window_idx
    ON identity_merges (app_id, alias_first_seen)
    WHERE state = 'done' AND cold_stale;

-- The terminal-'dead' index, serving TWO readers with different shapes.
--
-- 1. `identity_merge::dead_merge_count` — the drain's every-5-seconds-per-
--    replica gauge. Measured at 200k rows it was a 3,636-buffer / 10 ms
--    sequential scan, unconditionally: no index in this schema carried
--    `state = 'dead'` at all, and `identity_merges` gains a row per signup
--    with no purge path (`purge::rollup_companions` deliberately leaves this
--    table to a worker that does not exist yet), so that cost only grows for
--    the life of a deployment. With this index the count is an index-only
--    scan over the dead rows alone.
-- 2. `identity_merge::cold_alias_map`'s 'dead' arm. A 'dead' merge is one
--    whose hot rewrite never landed, so it MUST stay in the cold overlay —
--    it is the case where the alias is stale in both tiers at once, forever.
--
-- Indexed on `(app_id)`, not on `(id)`: a count over a partial index is
-- index-only regardless of which column is stored, so `(app_id)` costs
-- `dead_merge_count` nothing while additionally letting `cold_alias_map`'s
-- 'dead' arm seek straight to one app instead of scanning every dead row in
-- the deployment. `(id)` would serve reader 1 only.
CREATE INDEX identity_merges_dead_idx
    ON identity_merges (app_id)
    WHERE state = 'dead';

-- The three MISSING hot-rewrite indexes.
--
-- `identity_merge::rewrite_hot_rows` issues six `UPDATE ... WHERE app_id = $1
-- AND distinct_id = $2` statements per merge. The design doc claimed all six
-- "ride the existing (app_id, distinct_id, occurred_at) indexes" — that index
-- exists for `analytics_events`, `error_events` and `sessions` ONLY. MEASURED
-- against the real migration set, before these three indexes existed:
--
--   analytics_events  Index Scan                                2 buffers   0.03 ms
--   transactions      SEQ SCAN, ALL PARTITIONS, 300k filtered   4,286       19.2 ms
--   devices           SEQ SCAN, 100k filtered                   1,682        5.1 ms
--   workflows         Index Scan on the app prefix,
--                     `Filter: distinct_id`                     O(app's workflows)
--
-- Once per signup, and scaling with TOTAL RETAINED VOLUME rather than with
-- the guest's own row count — so a merge of a 5-row guest reads the whole
-- table. At 10M transactions that is ~1.1 GB of buffer churn per merge,
-- evicting exactly the cache the read path this feature exists to protect
-- depends on.
--
-- PARTIAL on `IS NOT NULL` because all three columns are nullable and a large
-- share of rows carry NULL there (a `transactions`/`workflows` row from a
-- server SDK never has a `distinct_id` at all — the three server SDKs
-- hardcode it — and a `devices` row has `last_distinct_id` only once someone
-- identified on that device). A NULL row can never match `distinct_id = $2`,
-- so excluding it costs the rewrite nothing and keeps the index off apps that
-- never identify. Postgres proves the implication for free: `distinct_id =
-- $2` is a strict operator on that column, which implies `distinct_id IS NOT
-- NULL`, so the partial index is usable by exactly the statement it exists
-- for. (Same reasoning and same shape as migration 000032's
-- `WHERE workflow_id IS NOT NULL` workflow indexes.)
--
-- `transactions` is partitioned: `CREATE INDEX` on the parent, with no
-- CONCURRENTLY available inside a migration transaction, propagates
-- synchronously to every child — migration 000031/000032's precedent.
CREATE INDEX transactions_app_distinct_idx
    ON transactions (app_id, distinct_id)
    WHERE distinct_id IS NOT NULL;
CREATE INDEX workflows_app_distinct_idx
    ON workflows (app_id, distinct_id)
    WHERE distinct_id IS NOT NULL;
CREATE INDEX devices_app_last_distinct_idx
    ON devices (app_id, last_distinct_id)
    WHERE last_distinct_id IS NOT NULL;
