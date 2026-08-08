-- Dropping restore_jobs first: it references tier_pins, which 000044 owns.
DROP TABLE IF EXISTS restore_jobs;

-- Rows restored by this feature stay behind once the marker is gone — they are
-- real rows in the live table, and they are also still in Parquet. Reverting
-- this migration therefore leaves duplicates that nothing can now identify. Let
-- every pin expire (or delete the restored rows) BEFORE reverting.
ALTER TABLE error_events     DROP COLUMN IF EXISTS restored_pin_id;
ALTER TABLE analytics_events DROP COLUMN IF EXISTS restored_pin_id;
ALTER TABLE transactions     DROP COLUMN IF EXISTS restored_pin_id;
