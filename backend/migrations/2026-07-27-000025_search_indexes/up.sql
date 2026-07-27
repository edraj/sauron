-- Indexes for the query planner landing in the next slice. Two groups: the
-- curated dimensions that become filterable, and the keyset-cursor support that
-- makes deep paging stable.
--
-- A third group — jsonb_ops GINs on the JSONB roots — was measured and then
-- DELIBERATELY LEFT OUT. See the note at the bottom of this file.
--
-- Every CREATE INDEX here builds SYNCHRONOUSLY across all live child partitions
-- inside this migration's transaction, holding locks on the parent and each
-- child. error_events is the hottest-write table in the schema. This needs a
-- maintenance window.
--
-- CONCURRENTLY is not an option: migrations run in a transaction and these are
-- partitioned parents. Indexes are declared on the parent only; children inherit
-- them under Postgres-generated names, so a later DROP on the parent cascades.

-- 1. Keyset support. `issues_app_last_seen_id_idx` is the index the two indexes
--    dropped below are genuine prefixes of, and it also serves the cursor's
--    ROW(last_seen, id) < ROW(?, ?) comparison as an Index Cond. Dropping the
--    duplicates WITHOUT this replacement regresses the default issues list to a
--    Sort, because issues_list_idx buries last_seen behind an equality on status.
CREATE INDEX issues_app_last_seen_id_idx ON issues (app_id, last_seen DESC, id DESC);
DROP INDEX IF EXISTS issues_app_last_seen_idx;   -- duplicate added by 0020
DROP INDEX IF EXISTS issues_last_seen_idx;       -- the original, renamed by 0002

--    Same trick for the occurrences list: the old index is a strict prefix of
--    the new one, so it is genuinely redundant once this exists.
CREATE INDEX error_events_issue_time_id_idx ON error_events (issue_id, occurred_at DESC, id DESC);
DROP INDEX IF EXISTS error_events_issue_idx;

-- 2. Curated dimensions. Three-column, time-trailing, mirroring the shape 0020
--    established: tenant key, then the filtered dimension, then the sort column.
CREATE INDEX error_events_app_env_time_idx     ON error_events     (app_id, environment_id, occurred_at DESC);
CREATE INDEX error_events_app_level_time_idx   ON error_events     (app_id, level,          occurred_at DESC);
CREATE INDEX error_events_app_release_time_idx ON error_events     (app_id, release,        occurred_at DESC);
CREATE INDEX analytics_events_app_env_time_idx ON analytics_events (app_id, environment_id, occurred_at DESC);
CREATE INDEX analytics_events_app_rel_time_idx ON analytics_events (app_id, release,        occurred_at DESC);

-- 3. JSONB roots — INTENTIONALLY OMITTED. Do not add these back without
--    re-reading this note.
--
--    The design originally called for `jsonb_ops` GINs on error_events
--    (context, contexts, extra) and analytics_events (contexts, extra,
--    properties), so that arbitrary-path equality and `has:` key existence
--    would be index-backed. They were written, applied, and then measured on a
--    realistically-seeded database (59,665 error events / 61,962 analytics
--    events, payloads of 9–13 JSONB keys with a production-like cardinality
--    mix). The numbers:
--
--      storage   583 bytes/row of index against a 2050 bytes/row heap  (+28%)
--                versus 12 bytes/row for the existing tags jsonb_path_ops GIN
--
--      writes    40k inserts, same real rows, scratch table:
--                  no GIN                 142 ms   1.0x
--                  3x jsonb_path_ops      630 ms   4.4x
--                  3x jsonb_ops          1273 ms   9.0x
--
--    9x write amplification on the two hottest-write tables in the schema is a
--    real operational cost for a self-hosted product. And nothing collects on
--    it yet: every dimension backed by these columns is declared
--    `IndexClass::Bounded` in sauron-query's catalog, not `Indexed`, so the
--    shipped cost model does not plan around them. They would buy only `has:`
--    key existence — which `jsonb_path_ops` genuinely cannot serve — for a
--    query nothing issues until the planner lands.
--
--    So they are deferred to the slice that introduces the planner, when the
--    real query mix is known. CREATE INDEX is additive and low-risk then.
--    `tags` keeps its jsonb_path_ops GIN from 0018 — not touched here.
