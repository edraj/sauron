-- Reverting DESTROYS every channel's configuration.
--
-- Once a row has been converted, its `config` column holds '{}' and the only
-- copy of the destination (SMTP host/port/from/to, Matrix homeserver+room,
-- webhook URL and headers) is the ciphertext being dropped here. This migration
-- cannot decrypt it: the key lives in NOTIFY_SECRET_KEY, outside the database.
--
-- So the revert is deliberately lossy rather than silently half-working. After
-- running it, every notification channel must be re-created by hand. Decrypt and
-- write back `config` with a build that still has the cipher BEFORE reverting if
-- the configurations matter.
ALTER TABLE notification_channels DROP COLUMN IF EXISTS config_enc;

COMMENT ON COLUMN notification_channels.config IS
  'Non-secret, kind-specific settings (e.g. smtp host/port/from/to, matrix room, headers).';
