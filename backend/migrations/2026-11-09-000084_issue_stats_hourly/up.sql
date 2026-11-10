-- Per-issue hourly event counts, so MCP trending can rank issues by what
-- happened IN a window instead of by lifetime totals.
-- Design: docs/superpowers/specs/2026-11-09-mcp-server-design.md
--         ("New rollup: per-issue hourly counts")
--
-- Written by its OWN watermarked fold (`rollup_watermarks.source =
-- 'issue_stats'`, rollups::fold::fold_issue_stats), REPLACED a day at a time by
-- the consistency rebuild and the operator-run backfill, both bounded by
-- `received_at <=` that watermark read inside the rollup advisory lock. The
-- base rollups' additive backfill never writes this table. That split is the
-- whole exactly-once argument; see rollups::issue_stats.
--
-- `release` uses migration 82's sentinel: '' = unknown (the telemetry
-- tables' NULL), so the unique key stays a plain column list. `environment_id`
-- keeps the house nullable shape: NULL is a real (unattributed) row, and every
-- ON CONFLICT must name the same COALESCE expression as the unique index.
CREATE TABLE issue_stats_hourly (
    app_id         uuid        NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    issue_id       uuid        NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    environment_id uuid        NULL REFERENCES app_environments(id) ON DELETE CASCADE,
    release        text        NOT NULL DEFAULT '',
    -- Hour start, UTC: date_trunc('hour', occurred_at, 'UTC'), which is what
    -- the fold's Rust `hour_of` computes too.
    hour           timestamptz NOT NULL,
    events         bigint      NOT NULL,
    updated_at     timestamptz NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX issue_stats_hourly_key ON issue_stats_hourly
    (app_id, hour, issue_id, release,
     COALESCE(environment_id, '00000000-0000-0000-0000-000000000000'::uuid));

-- Leads with issue_id so the `issue_id -> issues ON DELETE CASCADE` FK has an
-- index to probe: deleting an app cascades to its issues, and without this
-- every deleted issue seq-scans this table. Also serves one issue's window
-- (and its previous window) as a range read.
CREATE INDEX issue_stats_hourly_issue_idx ON issue_stats_hourly (issue_id, hour);

-- The consistency rebuild deletes one UTC day across EVERY app
-- (`hour >= $1 AND hour < $2`) while holding the rollup advisory lock the live
-- fold waits on; the app-leading unique key cannot serve that range.
CREATE INDEX issue_stats_hourly_hour_idx ON issue_stats_hourly (hour);

-- The instant the live fold started counting. Stamped HERE, in the migration
-- that creates the table (the migration-70 lesson): the fold owns everything
-- received after it; apps created at-or-after it are ready at once.
CREATE TABLE issue_stats_epoch (
    only_row   boolean     PRIMARY KEY DEFAULT true CHECK (only_row),
    started_at timestamptz NOT NULL DEFAULT now()
);
INSERT INTO issue_stats_epoch DEFAULT VALUES;

-- Per-app readiness marker for apps that predate the epoch, written by
-- `sauron-migrate backfill-issue-stats` only after every day it covers is
-- replaced. `covered_from` is where the hot history it could read began.
CREATE TABLE issue_stats_backfill (
    app_id       uuid        PRIMARY KEY REFERENCES apps(id) ON DELETE CASCADE,
    covered_from timestamptz NOT NULL,
    completed_at timestamptz NOT NULL DEFAULT now()
);

-- The fold's own watermark, seeded at the epoch.
INSERT INTO rollup_watermarks (source, watermark)
SELECT 'issue_stats', started_at FROM issue_stats_epoch;
