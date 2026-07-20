-- 0018: GIN indexes on the dev-supplied `tags` JSONB, defined on the
-- partitioned PARENT tables so they propagate to the default partition and
-- every range partition (same rule the existing B-tree indexes follow).
-- jsonb_path_ops is the smaller/faster GIN opclass for `@>` containment, which
-- backs the structured tag-`eq` filter (`tags @> '{"key":"value"}'`).
CREATE INDEX error_events_tags_gin     ON error_events     USING gin (tags jsonb_path_ops);
CREATE INDEX analytics_events_tags_gin ON analytics_events USING gin (tags jsonb_path_ops);
