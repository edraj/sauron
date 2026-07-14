-- 0013: partition transactions by RANGE(occurred_at), mirroring error_events.
-- Note: occurred_at is NOT NULL with NO default (unlike the event tables), and
-- environment_id is a plain nullable UUID (no FK) — both preserved from the
-- original table. Indexes created after DROP to avoid name collisions.
ALTER TABLE transactions RENAME TO transactions_old;

CREATE TABLE transactions (
    id             UUID NOT NULL DEFAULT gen_random_uuid(),
    app_id         UUID NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    environment_id UUID,
    name           TEXT NOT NULL,
    op             TEXT NOT NULL,
    duration_ms    DOUBLE PRECISION NOT NULL,
    status         TEXT,
    http_method    TEXT,
    http_status    INTEGER,
    url            TEXT,
    distinct_id    TEXT,
    session_id     TEXT,
    device_key     TEXT,
    release        TEXT,
    ip_address     TEXT,
    occurred_at    TIMESTAMPTZ NOT NULL,
    received_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (id, occurred_at)
) PARTITION BY RANGE (occurred_at);

CREATE TABLE transactions_default PARTITION OF transactions DEFAULT;

INSERT INTO transactions SELECT * FROM transactions_old;

DROP TABLE transactions_old;

CREATE INDEX transactions_app_occurred_idx ON transactions (app_id, occurred_at DESC);
CREATE INDEX transactions_app_op_name_idx  ON transactions (app_id, op, name);
CREATE INDEX transactions_app_session_idx  ON transactions (app_id, session_id);
