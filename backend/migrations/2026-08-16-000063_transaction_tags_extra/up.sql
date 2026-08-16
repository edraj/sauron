-- 0063: developer-supplied metadata on transactions. `tags` (flat
-- string->string) and `extra` (freeform JSON — the request body, the response
-- body, an order id, a retry count). Mirrors 0017's shape on the event tables,
-- minus `contexts`: named context blocks are an error-debugging affordance, and
-- a span that wants structure can nest it inside `extra`.
--
-- ADD COLUMN on the partitioned parent propagates to every partition.
-- NOT NULL DEFAULT '{}' so existing rows and any client that omits the field
-- land on an empty object, never NULL.
ALTER TABLE transactions ADD COLUMN tags  JSONB NOT NULL DEFAULT '{}'::jsonb;
ALTER TABLE transactions ADD COLUMN extra JSONB NOT NULL DEFAULT '{}'::jsonb;

-- Same opclass and reasoning as 0018: jsonb_path_ops is the smaller/faster GIN
-- for the `@>` containment that backs the structured tag-`eq` filter, and the
-- index is defined on the PARENT so it propagates to every partition.
--
-- Deliberately NO index on `extra`. It is probed by containment/ILIKE over
-- freeform JSON of unbounded shape (IndexClass::Bounded in the query catalog,
-- exactly as `extra` is treated on occurrences and events), so a GIN there
-- would buy little and cost write throughput on the highest-volume table in
-- the system.
CREATE INDEX transactions_tags_gin ON transactions USING gin (tags jsonb_path_ops);
