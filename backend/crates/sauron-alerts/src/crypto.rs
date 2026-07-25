//! At-rest encryption of notification-channel secrets (SMTP passwords, bot
//! tokens, webhook URLs). AES-256-GCM with a random 12-byte nonce prefixed to
//! the ciphertext. The 256-bit key is derived by SHA-256 over the configured
//! key material (`NOTIFY_SECRET_KEY`, falling back to `JWT_SECRET`), so an
//! operator never has to hand-manage raw key bytes.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use sha2::{Digest, Sha256};

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
}
