-- Drops the administrative audit trail in full. There is no archive: reverting
-- this migration discards every recorded action permanently.
DROP TABLE IF EXISTS audit_log;
