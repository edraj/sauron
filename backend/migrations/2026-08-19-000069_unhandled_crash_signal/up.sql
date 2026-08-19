-- 0069: make "crash-free sessions" mean what it says.
--
-- THE DEFECT. `crashed_sessions` (repo.rs `overview_totals`) and `crashed`
-- (`session_stats`) both count `sessions.errors_count > 0`. `errors_count`
-- counts EVERY row in `error_events`, and `error_events.level` spans
-- debug/info/warning/error/fatal — so one handled, caught, warning-level
-- exception marks the whole session "crashed". The number is unactionable and
-- reads as alarming.
--
-- THE SIGNAL ALREADY EXISTS. Migration 0024 added `error_events.handled` and
-- the pipeline has written it ever since (`handled_of` in
-- sauron-pipeline::process, used by both the batch and single paths). Every SDK
-- stamps `mechanism.handled` automatically — `false` from the uncaught-error
-- hooks it installs itself, `true` only when the application explicitly calls
-- captureException. The developer never declares a crash. Nothing was missing
-- at the event level; only the SESSION-level rollup was, which is why the
-- metric still could not use it.
--
-- WHY A COUNTER AND NOT AN EXISTS AT READ TIME. Deriving the flag from
-- `error_events` per session was measured under Task 10 (Slice 3) and
-- declined: the correlated semi-join cost ~11x the column predicate even with
-- a purpose-built index, because neither `session_id` nor `environment_id` is
-- the partition key so pruning cannot help. The note on repo.rs::bump_session
-- names the alternative it wanted — "a per-session crash flag maintained by
-- this function itself, rather than computed at read time" — which is exactly
-- this column. It rides the same upsert arm as `errors_count`, so maintaining
-- it is free and the read stays a plain column predicate.
--
-- `NOT NULL DEFAULT 0` is right here even though `handled` is deliberately
-- NULLable: this counts KNOWN uncaught errors, and "none known" is genuinely
-- zero. What stops that zero from being read as a real crash-free rate is the
-- signal probe below, not the column type — an app whose SDK never reports a
-- mechanism must show "no data", not a confident 100%.
ALTER TABLE sessions ADD COLUMN unhandled_errors_count BIGINT NOT NULL DEFAULT 0;

-- Backfill what is already knowable. Sessions written before this migration
-- have the per-event truth in `error_events` — it just was never rolled up.
-- One pass, not a per-session subquery: this is the expensive shape the read
-- path refuses to run, which is precisely why it runs ONCE here instead.
UPDATE sessions s
   SET unhandled_errors_count = agg.n
  FROM (
        SELECT app_id, session_id, count(*) AS n
          FROM error_events
         WHERE handled IS FALSE AND session_id IS NOT NULL
      GROUP BY app_id, session_id
       ) agg
 WHERE s.app_id = agg.app_id
   AND s.session_id = agg.session_id;

-- NO INDEX for the `has_crash_signal` probe, deliberately.
--
-- A partial `(app_id, occurred_at) WHERE handled IS NOT NULL` was written here
-- first and then MEASURED on a 200k-row app; the planner chose it in neither
-- case, so it would have been pure write amplification on a hot partitioned
-- table. What actually happens:
--   * signal present — `EXISTS` short-circuits on the first matching row:
--     0.039 ms, and it does not care which access path it takes to find one.
--   * signal absent (the case that must prove a negative, i.e. an SDK that
--     never reports handledness) — Index Only Scan on the EXISTING
--     `(app_id, occurred_at)` index: 185 ms cold, 1.4-4.9 ms warm.
-- Against a totals query already costing ~77 ms, and one that `overview_cache`
-- runs off the request path, that is noise. Same discipline as the read-time
-- derivation this migration replaces: measure before adding the index, not
-- after. Re-measure before adding one if the probe ever shows up in a profile.
