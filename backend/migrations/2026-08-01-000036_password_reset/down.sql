-- The indexes and the UNIQUE constraint go with the table.
DROP TABLE IF EXISTS password_reset_tokens;
ALTER TABLE users DROP COLUMN IF EXISTS credentials_invalidated_at;
