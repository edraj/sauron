-- How each tiered table's last cycle went, so a table whose tiering has
-- stopped is visible without reading the journal.
--
-- `tiering_state` records how far tiering has GOT; nothing recorded whether it
-- was still moving. On a production host one table's export failed every
-- hour for weeks while the other tables tiered normally. The only evidence
-- was a WARN line per cycle, which was gone by the time anyone looked, and
-- the disk filled. This table keeps the evidence (`last_error` is the full
-- cause chain) and the count that says it is not a one-off.
--
-- Separate from `tiering_state` rather than extra columns on it: a
-- `tiering_state` row only exists once a table's first partition has been
-- exported (`watermark` is NOT NULL), and a table that has never managed one
-- is exactly the case that most needs a health row.
CREATE TABLE tiering_health (
    table_name            TEXT PRIMARY KEY,
    -- When a cycle last finished with this table, successfully or not.
    last_cycle_at         TIMESTAMPTZ NOT NULL,
    last_success_at       TIMESTAMPTZ,
    -- Cycles in a row that ended in an error; reset to 0 by a success.
    consecutive_failures  INTEGER NOT NULL DEFAULT 0,
    -- The most recent failure, kept after a later success so the cause of a
    -- past stall is still there to read.
    last_error            TEXT,
    last_error_at         TIMESTAMPTZ
);
