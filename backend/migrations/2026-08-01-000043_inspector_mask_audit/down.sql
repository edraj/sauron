-- masked_keys and reveal_audit reference mask_actions / findings, so they go
-- first. Dropping inspector_masked_keys re-enables ingest of every masked key
-- immediately: a revert restores raw values on the write path, it does not
-- restore the ones already overwritten.
DROP TABLE IF EXISTS inspector_reveal_audit;
DROP TABLE IF EXISTS inspector_masked_keys;
DROP TABLE IF EXISTS inspector_mask_actions;
