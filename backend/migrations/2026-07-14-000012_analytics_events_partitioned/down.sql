ALTER TABLE analytics_events RENAME TO analytics_events_part;

CREATE TABLE analytics_events (LIKE analytics_events_part INCLUDING DEFAULTS);
ALTER TABLE analytics_events ADD PRIMARY KEY (id);
ALTER TABLE analytics_events ADD FOREIGN KEY (app_id) REFERENCES apps(id) ON DELETE CASCADE;
ALTER TABLE analytics_events ADD FOREIGN KEY (environment_id) REFERENCES environments(id) ON DELETE SET NULL;

INSERT INTO analytics_events SELECT * FROM analytics_events_part;

DROP TABLE analytics_events_part;

CREATE INDEX analytics_name_idx             ON analytics_events (app_id, name, occurred_at DESC);
CREATE INDEX analytics_distinct_idx         ON analytics_events (app_id, distinct_id, occurred_at DESC);
CREATE INDEX analytics_project_idx          ON analytics_events (app_id, occurred_at DESC);
CREATE INDEX analytics_events_app_device_idx ON analytics_events (app_id, device_key);
CREATE INDEX analytics_events_app_screen_idx ON analytics_events (app_id, screen);
