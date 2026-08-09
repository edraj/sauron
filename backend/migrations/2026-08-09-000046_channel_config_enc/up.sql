-- Encrypt a notification channel's `config`, not just its `secret`.
--
-- Migration 000019 drew the plaintext/ciphertext line in the wrong place. It
-- called `config` "non-secret, kind-specific settings" — but for the generic
-- webhook kind `config` holds the target `url` AND an arbitrary `headers` map,
-- so an `Authorization: Bearer …` a developer configured on a channel was
-- sitting in cleartext in Postgres, in every base backup, and in every WAL
-- archive. Slack/Discord are worse in kind: the resolver accepts a
-- `webhook_url` from `config` as a fallback, and that URL *is* the credential.
--
-- The fix is not "encrypt the sensitive leaves". `headers` is an arbitrary map,
-- so the sensitive set is not enumerable, and any per-kind allowlist drifts from
-- `sauron-alerts/src/channel.rs` — which is precisely how this bug happened.
-- The whole blob goes behind the same AES-256-GCM cipher as `secret_enc`.

-- Nonce-prefixed AES-256-GCM ciphertext of the config JSON. NULL means the row
-- has not been converted yet and the legacy plaintext in `config` still applies.
ALTER TABLE notification_channels ADD COLUMN config_enc BYTEA;

COMMENT ON COLUMN notification_channels.config_enc IS
  'AES-256-GCM ciphertext (nonce-prefixed) of the channel config JSON. Encrypted under NOTIFY_SECRET_KEY; losing that key makes the channel unrecoverable and it must be re-created.';

-- `config` stays NOT NULL so the row model keeps a plain `Value` rather than an
-- Option that every reader has to unwrap; converted rows carry '{}'. There is no
-- ambiguity, because the read rule is "config_enc when non-NULL, else config" —
-- an empty legacy config and a converted row are never confused.
COMMENT ON COLUMN notification_channels.config IS
  'DEPRECATED legacy plaintext. Read only when config_enc IS NULL; blanked to {} the first time the row is written. Superseded by config_enc.';

-- The row conversion CANNOT happen here. There is no pgcrypto extension in this
-- database, and the key derivation is SHA-256("sauron-notify-secret-v1" || key
-- material) over an env var that Postgres cannot see. It runs in Rust, at
-- `sauron-api` boot, immediately after the cipher is built — see
-- `sauron_alerts::crypto::seal_legacy_channel_configs`. That pass is idempotent
-- (it only touches rows where config_enc IS NULL) and it aborts startup rather
-- than half-converting the table.
--
-- Deliberately NOT hung off `sauron-migrate`: that binary depends only on
-- `sauron-db` and reads nothing but DATABASE_URL, so it has neither the cipher
-- nor the key — and RPM upgrades do not re-run it, so a backfill wired there
-- would silently never execute on exactly the deployments that have plaintext.
