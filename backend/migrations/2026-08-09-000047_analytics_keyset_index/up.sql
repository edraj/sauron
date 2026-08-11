-- The keyset tiebreaker analytics_events never got.
--
-- Migration 25 gave issues (app_id, last_seen DESC, id DESC) and error_events
-- (issue_id, occurred_at DESC, id DESC). The closest analytics index is
-- analytics_project_idx (app_id, occurred_at DESC) — no id column. A keyset
-- cursor ordered by (occurred_at DESC, id DESC) can still seek with it, but
-- rows sharing an occurred_at have no index-level order, so a page boundary
-- landing inside such a group repeats or skips rows. That is the exact defect
-- this slice exists to remove, so the index has to exist before the cursor does.
--
-- Builds SYNCHRONOUSLY across every live child partition inside this
-- transaction, holding locks on the parent and each child. analytics_events is
-- a hot-write table: this needs a maintenance window. CONCURRENTLY is not an
-- option — migrations run in a transaction and this is a partitioned parent.
CREATE INDEX analytics_events_app_time_id_idx
    ON analytics_events (app_id, occurred_at DESC, id DESC);
