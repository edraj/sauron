DROP INDEX IF EXISTS transactions_tags_gin;
ALTER TABLE transactions DROP COLUMN IF EXISTS extra;
ALTER TABLE transactions DROP COLUMN IF EXISTS tags;
