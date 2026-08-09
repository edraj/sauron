//! At-rest encryption of a notification channel's stored payload — BOTH its
//! secret bundle (SMTP passwords, bot tokens, access tokens) and its config
//! (webhook URL, arbitrary request headers, SMTP relay, Matrix homeserver).
//!
//! AES-256-GCM with a random 12-byte nonce prefixed to the ciphertext. The
//! 256-bit key is derived by SHA-256 over `NOTIFY_SECRET_KEY`, so an operator
//! never has to hand-manage raw key bytes.
//!
//! There is **no fallback key**. `NOTIFY_SECRET_KEY` is required and every
//! service that touches channels refuses to start without it
//! (`Config::require_notify_secret_key`). It used to fall back to `JWT_SECRET`
//! behind a `warn!`, which failed OPEN in the worst way: rotating the JWT
//! signing secret — a routine, well-documented operation — silently made every
//! stored channel secret undecryptable, and nothing surfaced it until an alert
//! failed to deliver. Losing `NOTIFY_SECRET_KEY` is unrecoverable by design:
//! the ciphertext is all there is, so the only remedy is to delete and
//! re-create every channel.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use chrono::{DateTime, Utc};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tracing::info;
use uuid::Uuid;

use sauron_db::{repo, PgPool};

const NONCE_LEN: usize = 12;

/// A configured AES-256-GCM cipher for channel secrets.
#[derive(Clone)]
pub struct SecretCipher {
    key: [u8; 32],
}

impl std::fmt::Debug for SecretCipher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the key material.
        f.write_str("SecretCipher(<redacted>)")
    }
}

impl SecretCipher {
    /// Derive a cipher from arbitrary key material (env secret). Any non-empty
    /// string works; entropy is the caller's responsibility.
    pub fn new(key_material: &str) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(b"sauron-notify-secret-v1");
        hasher.update(key_material.as_bytes());
        let digest = hasher.finalize();
        let mut key = [0u8; 32];
        key.copy_from_slice(&digest);
        Self { key }
    }

    fn cipher(&self) -> Aes256Gcm {
        Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&self.key))
    }

    /// Encrypt plaintext → `nonce || ciphertext+tag`.
    pub fn encrypt(&self, plaintext: &[u8]) -> anyhow::Result<Vec<u8>> {
        let mut nonce_bytes = [0u8; NONCE_LEN];
        getrandom::fill(&mut nonce_bytes).map_err(|e| anyhow::anyhow!("rng unavailable: {e}"))?;
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ct = self
            .cipher()
            .encrypt(nonce, plaintext)
            .map_err(|_| anyhow::anyhow!("secret encryption failed"))?;
        let mut out = Vec::with_capacity(NONCE_LEN + ct.len());
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&ct);
        Ok(out)
    }

    /// Encrypt a UTF-8 string secret bundle (JSON) to a blob.
    pub fn encrypt_str(&self, plaintext: &str) -> anyhow::Result<Vec<u8>> {
        self.encrypt(plaintext.as_bytes())
    }

    /// Decrypt a `nonce || ciphertext` blob back to bytes.
    pub fn decrypt(&self, blob: &[u8]) -> anyhow::Result<Vec<u8>> {
        if blob.len() < NONCE_LEN + 16 {
            anyhow::bail!("ciphertext too short");
        }
        let (nonce_bytes, ct) = blob.split_at(NONCE_LEN);
        let nonce = Nonce::from_slice(nonce_bytes);
        self.cipher()
            .decrypt(nonce, ct)
            .map_err(|_| anyhow::anyhow!("secret decryption failed (wrong key or tampered)"))
    }

    /// Decrypt to a UTF-8 string.
    pub fn decrypt_str(&self, blob: &[u8]) -> anyhow::Result<String> {
        let bytes = self.decrypt(blob)?;
        String::from_utf8(bytes).map_err(|_| anyhow::anyhow!("decrypted secret is not utf-8"))
    }

    /// Encrypt a JSON value (a channel's config or secret bundle).
    pub fn encrypt_json(&self, v: &Value) -> anyhow::Result<Vec<u8>> {
        self.encrypt_str(&serde_json::to_string(v)?)
    }

    /// Decrypt a JSON value.
    ///
    /// Unlike the historical secret-decrypt call sites, a malformed plaintext is
    /// an ERROR, not a silent `Null`: a config that decrypts to garbage means the
    /// blob is not what we think it is, and treating that as "empty config" is
    /// how an admin edit blanks a channel whose ciphertext merely could not be
    /// read.
    pub fn decrypt_json(&self, blob: &[u8]) -> anyhow::Result<Value> {
        let plain = self.decrypt_str(blob)?;
        serde_json::from_str(&plain)
            .map_err(|e| anyhow::anyhow!("decrypted payload is not valid JSON: {e}"))
    }
}

/// Read a channel's effective config, decrypting when it has been converted.
///
/// The dual read is the whole migration window in one function: `config_enc`
/// wins when present, and only a row predating migration 000046 falls back to
/// the legacy plaintext `config`. Both the API and the delivery path go through
/// here so the two can never disagree about which column is authoritative.
///
/// Fails rather than degrading. A wrong `NOTIFY_SECRET_KEY` must not resolve to
/// an empty config: on the delivery path that would try to send to a
/// half-resolved destination, and on the write path it would let an edit
/// overwrite an unreadable channel with a blank one.
pub fn open_channel_config(
    cipher: &SecretCipher,
    ch: &sauron_db::models::NotificationChannel,
) -> anyhow::Result<Value> {
    match &ch.config_enc {
        Some(blob) => cipher.decrypt_json(blob),
        None => Ok(ch.config.clone()),
    }
}

/// Convert every channel still holding a legacy plaintext `config` (migration
/// 000046). Returns how many rows were converted.
///
/// Runs at `sauron-api` boot rather than in the migration because the key
/// derivation is not expressible in SQL and this database has no pgcrypto, and
/// rather than in `sauron-migrate` because that binary has neither the cipher
/// nor the key — and RPM upgrades never re-run it, so a backfill wired there
/// would skip exactly the deployments that still have plaintext.
///
/// Idempotent by construction: it selects only `config_enc IS NULL` rows and the
/// write re-checks that predicate, so a second boot (or a second API instance
/// booting concurrently) is a no-op rather than a double-encrypt.
///
/// ## Two guards, both added after review, both protecting against destroying
/// something recoverable
///
/// **Key continuity.** The write encrypts `config` and blanks the plaintext in
/// the SAME statement. If the configured key is not the key this deployment's
/// existing `secret_enc` blobs were written under, that statement converts
/// recoverable plaintext into ciphertext nobody can open — a worse outcome than
/// the plaintext-at-rest problem it exists to fix. So before converting anything
/// we decrypt one existing `secret_enc` blob. If that fails, the key is wrong or
/// new, and we refuse to convert.
///
/// **Schema tolerance.** A missing `config_enc` column means migration 000046
/// has not been applied. RPM upgrades never re-run `sauron-migrate`, so this is
/// an ordinary state, not an exceptional one. The conversion is SKIPPED with a
/// loud error rather than taking the boot down — refusing to start over one
/// table's backfill turns a missed migration into a deployment-wide outage.
///
/// Any OTHER failure still propagates and takes the boot with it: a table half
/// converted under a cipher that does not work is worse than a service that
/// refuses to start and says why.
pub async fn seal_legacy_channel_configs(
    pool: &PgPool,
    cipher: &SecretCipher,
) -> anyhow::Result<usize> {
    /// One pass covers any realistic deployment; the loop exists so a pathological
    /// one still converges instead of silently leaving the tail in plaintext.
    const BATCH: i64 = 500;

    let mut conn = sauron_db::conn(pool).await?;

    // Schema tolerance — see the doc comment.
    if repo::probe_channel_config_enc(&mut conn).await.is_err() {
        tracing::error!(
            "notification_channels.config_enc is missing - run sauron-migrate (migration \
             2026-08-09-000046). Channel configs remain in PLAINTEXT at rest until it is \
             applied; alerting continues to work."
        );
        return Ok(0);
    }

    // Key continuity — see the doc comment. Done before the first write, and on
    // a blob we did not create, so it proves the configured key opens what this
    // deployment already has rather than merely that the cipher round-trips.
    if let Some(existing) = repo::any_channel_secret_enc(&mut conn).await? {
        if cipher.decrypt_json(&existing).is_err() {
            anyhow::bail!(
                "NOTIFY_SECRET_KEY cannot decrypt an existing notification-channel secret. \
                 Refusing to encrypt channel configs, because doing so would replace \
                 recoverable plaintext with ciphertext this key cannot open either. Either \
                 restore the original key, or clear notification_channels.secret_enc and \
                 re-enter the channel credentials."
            );
        }
    }

    let mut converted = 0usize;
    loop {
        let rows = repo::channels_with_legacy_plaintext_config(&mut conn, BATCH).await?;
        if rows.is_empty() {
            break;
        }
        let fetched = rows.len();
        for (id, plaintext) in rows {
            let blob = cipher
                .encrypt_json(&plaintext)
                .map_err(|e| anyhow::anyhow!("encrypting config of channel {id}: {e}"))?;
            converted += repo::seal_channel_config(&mut conn, id, blob).await?;
        }
        if (fetched as i64) < BATCH {
            break;
        }
    }
    if converted > 0 {
        info!(
            channels = converted,
            "encrypted legacy plaintext notification-channel configs (migration 000046)"
        );
    }
    Ok(converted)
}

/// HMAC-SHA256 → lowercase hex. Implemented directly on the workspace's `sha2`
/// (RustCrypto's `hmac` crate targets a different `digest` major than the pinned
/// `sha2` 0.11, which would double-vendor `sha2`). Verified against RFC 4231.
pub fn hmac_sha256_hex(key: &[u8], message: &[u8]) -> String {
    const BLOCK: usize = 64;
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        let mut h = Sha256::new();
        h.update(key);
        let d = h.finalize();
        k[..d.len()].copy_from_slice(&d);
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(message);
    let inner = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner);
    hex::encode(outer.finalize())
}

/// How long an unsubscribe link stays valid.
///
/// A compile-time constant, not an env var: every send mints a fresh token so
/// links in live mail always work, and the only thing this bounds is a token
/// forwarded into an archive becoming a permanent silencer of someone else's
/// uptime alerts.
pub const UNSUB_TOKEN_TTL_DAYS: i64 = 90;

const UNSUB_KEY_DOMAIN: &[u8] = b"sauron-unsub-key-v1";
const UNSUB_MSG_PREFIX: &str = "sauron-unsub-v1";
/// Half of a SHA-256 in hex. Enough to make forgery infeasible without making
/// the URL unwieldy in a mail client that wraps long lines.
const UNSUB_SIG_HEX_LEN: usize = 32;

/// Days since the Unix epoch, the unit `issued_day` is measured in.
pub fn days_since_epoch(now: DateTime<Utc>) -> i64 {
    now.timestamp().div_euclid(86_400)
}

/// Derive the unsubscribe signing key from the notification secret.
///
/// Never sign with `notify_key` directly: it is the AES-GCM key that encrypts
/// stored channel secrets, so it cannot be rotated to invalidate outstanding
/// links without making every stored webhook URL and relay password
/// undecryptable.
pub fn derive_unsub_key(notify_key: &[u8]) -> String {
    hmac_sha256_hex(notify_key, UNSUB_KEY_DOMAIN)
}

/// `{subscription_id}.{issued_day}.{first 32 hex chars of the HMAC}`.
pub fn unsubscribe_token(key: &[u8], sub_id: Uuid, user_id: Uuid, issued_day: i64) -> String {
    let msg = format!("{UNSUB_MSG_PREFIX}:{sub_id}:{user_id}:{issued_day}");
    let sig = hmac_sha256_hex(key, msg.as_bytes());
    format!("{sub_id}.{issued_day}.{}", &sig[..UNSUB_SIG_HEX_LEN])
}

/// Verify a token and return the subscription it names.
///
/// `owner_of` resolves a subscription id to its owner's user id — the owner is
/// inside the signed message, so verification cannot be completed without it,
/// and passing it as a closure keeps this function free of `sauron-db`. It is
/// called at most once, after the token's shape has already been validated, so
/// a garbage token costs no database round trip.
///
/// `today` is `days_since_epoch(Utc::now())` at the call site, threaded in so
/// the expiry branch is testable without a clock.
pub fn verify_unsubscribe_token(
    key: &[u8],
    token: &str,
    today: i64,
    owner_of: impl FnOnce(Uuid) -> Option<Uuid>,
) -> Option<Uuid> {
    let mut parts = token.split('.');
    let sub_id: Uuid = parts.next()?.parse().ok()?;
    let issued_day: i64 = parts.next()?.parse().ok()?;
    let sig = parts.next()?;
    if parts.next().is_some() || sig.len() != UNSUB_SIG_HEX_LEN {
        return None;
    }
    // A future-dated token means a forged `issued_day`; an old one means a
    // link that has been sitting in an archive.
    if issued_day > today || today - issued_day > UNSUB_TOKEN_TTL_DAYS {
        return None;
    }
    let user_id = owner_of(sub_id)?;
    let msg = format!("{UNSUB_MSG_PREFIX}:{sub_id}:{user_id}:{issued_day}");
    let expected = hmac_sha256_hex(key, msg.as_bytes());
    ct_eq(sig.as_bytes(), &expected.as_bytes()[..UNSUB_SIG_HEX_LEN]).then_some(sub_id)
}

/// Constant-time byte comparison. `==` on `&[u8]` short-circuits on the first
/// differing byte, which leaks the length of a correct prefix to anyone who can
/// time the endpoint.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_rfc4231_case2() {
        // RFC 4231 test case 2: key="Jefe", data="what do ya want for nothing?"
        let mac = hmac_sha256_hex(b"Jefe", b"what do ya want for nothing?");
        assert_eq!(
            mac,
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn roundtrip() {
        let c = SecretCipher::new("some-key-material-abc");
        let blob = c.encrypt_str("{\"password\":\"hunter2\"}").unwrap();
        // Nonce is random → ciphertext differs each time.
        let blob2 = c.encrypt_str("{\"password\":\"hunter2\"}").unwrap();
        assert_ne!(blob, blob2);
        assert_eq!(c.decrypt_str(&blob).unwrap(), "{\"password\":\"hunter2\"}");
    }

    #[test]
    fn wrong_key_fails() {
        let a = SecretCipher::new("key-a");
        let b = SecretCipher::new("key-b");
        let blob = a.encrypt_str("secret").unwrap();
        assert!(b.decrypt(&blob).is_err());
    }

    #[test]
    fn tamper_detected() {
        let c = SecretCipher::new("k");
        let mut blob = c.encrypt_str("secret").unwrap();
        let last = blob.len() - 1;
        blob[last] ^= 0xff;
        assert!(c.decrypt(&blob).is_err());
    }

    #[test]
    fn short_blob_rejected() {
        let c = SecretCipher::new("k");
        assert!(c.decrypt(&[0u8; 4]).is_err());
    }

    /// Build a channel row with whatever payload columns the test needs.
    fn channel(
        config: Value,
        config_enc: Option<Vec<u8>>,
    ) -> sauron_db::models::NotificationChannel {
        sauron_db::models::NotificationChannel {
            id: Uuid::from_u128(1),
            org_id: Uuid::from_u128(2),
            name: "wh".into(),
            kind: "webhook".into(),
            config,
            config_enc,
            secret_enc: None,
            enabled: true,
            created_by: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    /// A webhook config carries the target URL and an arbitrary header map. The
    /// header map is the reason this column had to move behind the cipher, so
    /// the roundtrip is asserted with a real bearer token in it — and the
    /// ciphertext is checked for the plaintext bytes, which is the assertion
    /// that would actually catch "we stored it encoded, not encrypted".
    #[test]
    fn a_config_with_an_authorization_header_round_trips_and_leaves_no_plaintext() {
        let c = SecretCipher::new("some-key-material-abcdefghijklmnop");
        let cfg = serde_json::json!({
            "url": "https://hooks.example/x",
            "headers": { "Authorization": "Bearer sk-live-DO-NOT-LEAK" }
        });
        let blob = c.encrypt_json(&cfg).unwrap();
        assert_eq!(c.decrypt_json(&blob).unwrap(), cfg);

        let haystack = String::from_utf8_lossy(&blob).to_string();
        for needle in ["sk-live-DO-NOT-LEAK", "Authorization", "hooks.example"] {
            assert!(
                !haystack.contains(needle),
                "{needle} survived into the ciphertext"
            );
        }
    }

    /// The fail-closed half of the ruling, at the primitive level: a mismatched
    /// key must ERROR, never resolve to an empty config. A `Value::Null` here
    /// would be delivered to a half-resolved destination on the send path and
    /// would blank the row on the write path.
    #[test]
    fn decrypting_a_config_with_the_wrong_key_fails_rather_than_emptying_it() {
        let real = SecretCipher::new("the-real-notify-key-aaaaaaaaaaaaaaaa");
        let other = SecretCipher::new("a-rotated-jwt-secret-bbbbbbbbbbbbbbb");
        let blob = real
            .encrypt_json(&serde_json::json!({ "url": "https://h/x" }))
            .unwrap();
        let err = other.decrypt_json(&blob).unwrap_err().to_string();
        assert!(err.contains("decryption failed"), "got: {err}");
    }

    /// Non-JSON plaintext is an error too. It can only mean the blob is not what
    /// we think it is, and "not what we think it is" must never read as `{}`.
    #[test]
    fn a_blob_that_decrypts_to_non_json_is_an_error() {
        let c = SecretCipher::new("key-material-cccccccccccccccccccccccc");
        let blob = c.encrypt_str("not json at all").unwrap();
        assert!(c.decrypt_json(&blob).is_err());
    }

    /// The migration window in one assertion: an unconverted row reads its
    /// legacy plaintext, a converted row reads its ciphertext, and the two
    /// produce the same value — which is what makes the boot conversion a no-op
    /// from every reader's point of view.
    #[test]
    fn open_channel_config_reads_the_ciphertext_when_present_and_the_legacy_column_otherwise() {
        let c = SecretCipher::new("key-material-dddddddddddddddddddddddd");
        let cfg = serde_json::json!({ "url": "https://h/x", "headers": { "X-Api-Key": "k" } });

        let legacy = channel(cfg.clone(), None);
        assert_eq!(open_channel_config(&c, &legacy).unwrap(), cfg);

        // Converted: plaintext blanked, ciphertext authoritative.
        let converted = channel(serde_json::json!({}), Some(c.encrypt_json(&cfg).unwrap()));
        assert_eq!(open_channel_config(&c, &converted).unwrap(), cfg);
        assert_eq!(
            open_channel_config(&c, &legacy).unwrap(),
            open_channel_config(&c, &converted).unwrap(),
            "conversion must be invisible to every reader"
        );

        // And an unreadable row is an error, not an empty config.
        let wrong = SecretCipher::new("wrong-key-eeeeeeeeeeeeeeeeeeeeeeeeeee");
        assert!(open_channel_config(&wrong, &converted).is_err());
    }

    #[test]
    fn unsubscribe_token_round_trips_for_its_own_subscription() {
        let key = derive_unsub_key(b"notify-secret");
        let sub = uuid::Uuid::from_u128(1);
        let user = uuid::Uuid::from_u128(2);
        let day = 20_000;
        let token = unsubscribe_token(key.as_bytes(), sub, user, day);
        assert_eq!(
            verify_unsubscribe_token(key.as_bytes(), &token, day, |id| {
                (id == sub).then_some(user)
            }),
            Some(sub)
        );
    }

    #[test]
    fn a_token_signed_with_another_key_never_verifies() {
        let a = derive_unsub_key(b"key-a");
        let b = derive_unsub_key(b"key-b");
        let sub = uuid::Uuid::from_u128(1);
        let user = uuid::Uuid::from_u128(2);
        let token = unsubscribe_token(a.as_bytes(), sub, user, 20_000);
        assert_eq!(
            verify_unsubscribe_token(b.as_bytes(), &token, 20_000, |_| Some(user)),
            None
        );
    }

    #[test]
    fn a_token_for_one_subscription_does_not_verify_against_another() {
        let key = derive_unsub_key(b"notify-secret");
        let user = uuid::Uuid::from_u128(2);
        let token = unsubscribe_token(key.as_bytes(), uuid::Uuid::from_u128(1), user, 20_000);
        // The stored token names subscription 1, but the row it points at is
        // owned by a different user — the HMAC covers the pair, so the swap is
        // detected rather than silently accepted.
        assert_eq!(
            verify_unsubscribe_token(key.as_bytes(), &token, 20_000, |_| Some(
                uuid::Uuid::from_u128(999)
            )),
            None
        );
    }

    #[test]
    fn tokens_expire_and_are_not_accepted_from_the_future() {
        let key = derive_unsub_key(b"notify-secret");
        let sub = uuid::Uuid::from_u128(1);
        let user = uuid::Uuid::from_u128(2);
        let old = unsubscribe_token(key.as_bytes(), sub, user, 20_000);
        assert_eq!(
            verify_unsubscribe_token(key.as_bytes(), &old, 20_000 + 91, |_| Some(user)),
            None,
            "91 days old is past UNSUB_TOKEN_TTL_DAYS"
        );
        assert_eq!(
            verify_unsubscribe_token(key.as_bytes(), &old, 20_000 + 90, |_| Some(user)),
            Some(sub),
            "exactly 90 days old still works"
        );
        let future = unsubscribe_token(key.as_bytes(), sub, user, 20_050);
        assert_eq!(
            verify_unsubscribe_token(key.as_bytes(), &future, 20_000, |_| Some(user)),
            None,
            "a token dated in the future is a forged issued_day"
        );
    }

    #[test]
    fn malformed_tokens_are_rejected_without_panicking() {
        let key = derive_unsub_key(b"notify-secret");
        for bad in [
            "",
            ".",
            "a.b",
            "a.b.c",
            "....",
            "zzz.20000.deadbeef",
            &"x".repeat(4096),
        ] {
            assert_eq!(
                verify_unsubscribe_token(key.as_bytes(), bad, 20_000, |_| Some(
                    uuid::Uuid::from_u128(2)
                )),
                None
            );
        }
    }

    #[test]
    fn ct_eq_compares_the_whole_buffer_and_refuses_length_mismatches() {
        // The design requires the signature comparison to be constant-time, and
        // the only way that property can regress silently is someone replacing
        // the loop with `a == b`. These assertions pin the two observable
        // consequences: a length mismatch is refused without indexing past the
        // end, and a difference in the LAST byte is still caught (a
        // short-circuiting prefix compare would pass every earlier byte and is
        // exactly what leaks the length of a correct prefix to a timing
        // attacker).
        assert!(ct_eq(b"", b""));
        assert!(ct_eq(b"abcdef", b"abcdef"));
        assert!(!ct_eq(b"abcdef", b"abcde"), "a prefix is not a match");
        assert!(!ct_eq(b"abcde", b"abcdef"));
        assert!(!ct_eq(b"abcdef", b"abcdeg"), "the last byte still decides");
        assert!(!ct_eq(b"abcdef", b"zbcdef"), "the first byte still decides");
    }

    #[test]
    fn the_derived_key_is_domain_separated_from_the_notify_key() {
        // NOTIFY_SECRET_KEY is documented as the AES-GCM key that encrypts
        // stored channel secrets, so "rotate it to invalidate outstanding
        // links" is not available — rotating it makes every stored Slack
        // webhook URL and SMTP password undecryptable. Domain separation at
        // least keeps the two uses independent.
        let raw = b"notify-secret";
        assert_ne!(derive_unsub_key(raw), String::from_utf8_lossy(raw));
        assert_eq!(derive_unsub_key(raw).len(), 64, "hex sha256");
    }
}
