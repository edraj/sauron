-- 0010: tiering watermark, one row per tiered table.
--   watermark    — everything with occurred_at < watermark is durably in Parquet.
--   dropped_thru — everything with occurred_at < dropped_thru has been dropped
--                  from Postgres. Always dropped_thru <= watermark (drop lags
--                  export), so a slightly stale cached watermark never gaps.
CREATE TABLE tiering_state (
    table_name   TEXT PRIMARY KEY,
    watermark    TIMESTAMPTZ NOT NULL,
    dropped_thru TIMESTAMPTZ,
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
