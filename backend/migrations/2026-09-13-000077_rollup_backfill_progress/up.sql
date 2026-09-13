-- Resumable rollup backfill.
--
-- `backfill_all` folds pre-epoch history one UTC day per transaction and only
-- writes the per-app `rollup_backfill` markers at the very end. Until now
-- nothing recorded WHICH days had landed, so an interrupted run (SIGTERM,
-- lost connection, an operator's Ctrl-C) left the added days in place with
-- no marker — and a re-run re-added every one of them, double-counting. That
-- made the backfill unsafe to run unattended, which is why it stayed a manual
-- runbook step that upgraded deployments then never ran: every gated endpoint
-- served the legacy O(history) shape and timed out at 30M rows.
--
-- One row. `next_day` advances IN THE SAME TRANSACTION as each day's fold, so
-- a resume continues from exactly the first day that did not commit. The
-- bounds are fixed at the first run — history cannot grow behind the epoch,
-- and a stable `last_day` is what lets the status endpoint report progress.
CREATE TABLE rollup_backfill_progress (
    only_row   boolean     PRIMARY KEY DEFAULT true CHECK (only_row),
    first_day  date        NOT NULL,
    last_day   date        NOT NULL,
    next_day   date        NOT NULL,
    started_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);
