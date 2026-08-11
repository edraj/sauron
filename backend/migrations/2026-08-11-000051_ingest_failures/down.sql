-- Drops the ingest failure store in full. There is no archive: reverting this
-- migration discards every retained failure payload permanently, including any
-- an operator had not yet retried. The Redis DLQ backstop is unaffected.
DROP TABLE IF EXISTS ingest_failure_payloads;
DROP TABLE IF EXISTS ingest_failures;
