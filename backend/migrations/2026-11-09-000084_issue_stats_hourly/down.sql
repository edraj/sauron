DELETE FROM rollup_watermarks WHERE source = 'issue_stats';
DROP TABLE issue_stats_backfill;
DROP TABLE issue_stats_epoch;
DROP TABLE issue_stats_hourly;
