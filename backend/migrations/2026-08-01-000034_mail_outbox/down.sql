-- Drops only what this migration's up.sql created; DROP TABLE takes the four
-- indexes with it. (Migration 20's down.sql dropped two indexes it had not
-- created, silently destroying migration 4's. Do not repeat that here.)
DROP TABLE IF EXISTS mail_outbox;
