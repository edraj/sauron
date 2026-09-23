-- Keyset support for sorting the issues list by its two count columns.
--
-- `GET /v1/apps/{id}/issues?sort=times_seen|users_seen` pages on
-- `(count, id)`, the same `(column, id) < ROW(?, ?)` walk migration 25 gave
-- `last_seen`, and for the same reason: thousands of issues legitimately tie
-- on a count, so the column alone is not a total order and `id` has to be in
-- the tuple AND in the index for the cursor comparison to be an Index Cond
-- rather than a Sort over every matching row.
--
-- `issues` is small (one row per grouped error, not per occurrence), so these
-- build in a moment and cost nothing measurable on the ingest write path,
-- which touches one `issues` row per occurrence either way.
--
-- Only the APP-WIDE ordering uses these. Under an environment scope the
-- displayed counts are derived per environment from `error_events` and the
-- ordering is computed over those (see `repo::search_issues_by_env_count`),
-- so no index over `issues`' own stored counts could serve it.
CREATE INDEX issues_app_times_seen_id_idx ON issues (app_id, times_seen DESC, id DESC);
CREATE INDEX issues_app_users_seen_id_idx ON issues (app_id, users_seen DESC, id DESC);
