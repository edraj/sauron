-- Devices gains a caller-chosen time-window column: `first_seen` alongside the
-- `last_seen` that `since_days` has always used. Neither table was indexed for
-- it.
--
-- `devices` carried only `devices_app_last_seen_idx (app_id, last_seen DESC)`,
-- and `device_environments` only `device_env_app_env_idx
-- (app_id, environment_id, last_seen DESC)`. `event_users` and
-- `event_user_environments` already carry BOTH columns for the Persons list --
-- which is why the Users page needs no migration and this one does.
--
-- Not DESC. The `last_seen` indexes are DESC because their dominant use is
-- `ORDER BY last_seen DESC`; `first_seen` is used as a RANGE BOUND, and a btree
-- walks either direction for that. Adding DESC would imply an ordering
-- preference this column does not have.
--
-- MEASURED, not assumed, on 50,000 `devices` rows spread over 400 days of
-- `first_seen` (Postgres 16, after ANALYZE), for the predicate the Devices list
-- actually emits — `app_id = $1 AND first_seen >= $2`:
--
--   with this index:    Bitmap Heap Scan       cost   850.70   (875 rows)
--   without it:         Seq Scan               cost  1770.00   (875 rows)
--
-- 2.1x on that fixture, which understates it: the Seq Scan's cost grows with
-- the whole device table while the index scan grows with the MATCHING rows, so
-- the gap widens with fleet size. The falsifiable signal is structural — which
-- of the two node types appears — not the ratio, and a future optimiser wanting
-- a number to beat should re-measure on their own row count rather than trust
-- 2.1x.
--
-- Both shapes are covered on purpose. `list_device_groups` reads the rollup
-- (`device_environments`) once an app is backfilled and the live `devices`
-- shape before that, so indexing only one leaves the window unindexed for every
-- app on the other side of that marker -- and under the 30s TimeoutLayer an
-- unindexed window does not read as a slow page, it reads as a broken endpoint.

CREATE INDEX devices_app_first_seen_idx
  ON devices (app_id, first_seen);

CREATE INDEX device_env_app_env_first_seen_idx
  ON device_environments (app_id, environment_id, first_seen);
