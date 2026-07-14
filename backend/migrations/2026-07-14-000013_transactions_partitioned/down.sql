ALTER TABLE transactions RENAME TO transactions_part;

CREATE TABLE transactions (LIKE transactions_part INCLUDING DEFAULTS);
ALTER TABLE transactions ADD PRIMARY KEY (id);
ALTER TABLE transactions ADD FOREIGN KEY (app_id) REFERENCES apps(id) ON DELETE CASCADE;

INSERT INTO transactions SELECT * FROM transactions_part;

DROP TABLE transactions_part;

CREATE INDEX transactions_app_occurred_idx ON transactions (app_id, occurred_at DESC);
CREATE INDEX transactions_app_op_name_idx  ON transactions (app_id, op, name);
CREATE INDEX transactions_app_session_idx  ON transactions (app_id, session_id);
