-- 0005: transactions — the performance-monitoring signal. Each row is one timed
-- operation (page/screen load, HTTP call, resource, or custom span) emitted by an
-- SDK as a `transaction` envelope item. Aggregations (p50/p95/throughput/error
-- rate) are computed on read with percentile_cont.

CREATE TABLE transactions (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    app_id         UUID NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    environment_id UUID,
    name           TEXT NOT NULL,                       -- route/screen/operation label
    op             TEXT NOT NULL,                       -- navigation|http|resource|screen_load|custom
    duration_ms    DOUBLE PRECISION NOT NULL,
    status         TEXT,                                -- ok|error|<class>
    http_method    TEXT,
    http_status    INTEGER,
    url            TEXT,
    distinct_id    TEXT,
    session_id     TEXT,
    device_key     TEXT,
    release        TEXT,
    ip_address     TEXT,
    occurred_at    TIMESTAMPTZ NOT NULL,
    received_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX transactions_app_occurred_idx ON transactions (app_id, occurred_at DESC);
CREATE INDEX transactions_app_op_name_idx ON transactions (app_id, op, name);
CREATE INDEX transactions_app_session_idx ON transactions (app_id, session_id);
