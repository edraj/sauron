-- Revert to a plain (non-partitioned) error_events with a single-column PK.
ALTER TABLE error_events RENAME TO error_events_part;

CREATE TABLE error_events (LIKE error_events_part INCLUDING DEFAULTS);
ALTER TABLE error_events ADD PRIMARY KEY (id);
ALTER TABLE error_events ADD FOREIGN KEY (app_id) REFERENCES apps(id) ON DELETE CASCADE;
ALTER TABLE error_events ADD FOREIGN KEY (environment_id) REFERENCES environments(id) ON DELETE SET NULL;
ALTER TABLE error_events ADD FOREIGN KEY (issue_id) REFERENCES issues(id) ON DELETE CASCADE;
CREATE INDEX error_events_issue_idx      ON error_events (issue_id, occurred_at DESC);
CREATE INDEX error_events_project_idx    ON error_events (app_id, occurred_at DESC);
CREATE INDEX error_events_distinct_idx   ON error_events (app_id, distinct_id, occurred_at DESC);
CREATE INDEX error_events_app_session_idx ON error_events (app_id, session_id);
CREATE INDEX error_events_app_device_idx  ON error_events (app_id, device_key);
CREATE INDEX error_events_app_screen_idx  ON error_events (app_id, screen);

INSERT INTO error_events SELECT * FROM error_events_part;
DROP TABLE error_events_part;
