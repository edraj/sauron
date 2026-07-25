-- Alert triggers must key off an INGEST-side clock, not a client-supplied one.
--
-- `alert_new_issues` filtered on `first_seen`, which is the timestamp the SDK
-- put in the event. The alert evaluator advances its watermark on its own
-- clock, and the issue row only appears after the async pipeline has processed
-- the envelope. If a tick fell between the event's timestamp and the worker's
-- INSERT, the watermark moved past `first_seen` and that issue was never
-- alerted at all. Client clock skew and offline/backdated batches lose the same
-- way, permanently.
--
-- `issues.created_at` already carries the server clock at INSERT, so the
-- new-issue trigger just needs an index on it. The regression trigger has the
-- same flaw on `last_seen` but no usable server-clock column: `updated_at` is
-- bumped by status changes too, so resolving an issue would immediately fire a
-- spurious "regressed" alert. Hence a dedicated column touched only when a new
-- event lands on the issue.

ALTER TABLE issues ADD COLUMN last_event_at TIMESTAMPTZ;

-- Backfill from last_seen rather than letting the column default to now():
-- a DEFAULT would make every pre-existing issue look like it just regressed,
-- firing an alert storm on the first tick after deploy.
UPDATE issues SET last_event_at = GREATEST(last_seen, created_at);

ALTER TABLE issues ALTER COLUMN last_event_at SET NOT NULL;
ALTER TABLE issues ALTER COLUMN last_event_at SET DEFAULT now();

-- Backs the new-issue trigger's (app_id, created_at] range scan per tick.
CREATE INDEX IF NOT EXISTS issues_app_created_idx
    ON issues (app_id, created_at);
-- Backs the regression trigger's (app_id, last_event_at] range scan per tick.
CREATE INDEX IF NOT EXISTS issues_app_last_event_idx
    ON issues (app_id, last_event_at);
