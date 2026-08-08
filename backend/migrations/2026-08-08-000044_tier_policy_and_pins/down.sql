-- Dropping runtime_settings reverts every runtime-tuned value to the process's
-- env/default at once. That is a behaviour change on revert, not just a schema
-- one: if an operator had raised the rotation age, tiering resumes at the
-- configured value on the next tick and starts moving data the higher setting
-- was keeping hot.
--
-- Dropping tier_pins un-protects every restored range. Those partitions become
-- droppable again on the next cycle, which is the pre-restore state — the rows
-- remain durable in Parquet, so this loses no data, but a restore someone is
-- actively working with will disappear from Postgres.
DROP TABLE IF EXISTS tier_pins;
DROP TABLE IF EXISTS runtime_settings;
