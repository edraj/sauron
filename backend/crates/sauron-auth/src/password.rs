//! Argon2id password hashing.
//!
//! Argon2id is memory-hard by design (~19 MiB, tens of milliseconds per call).
//! Running that directly inside an async handler parks a Tokio worker thread for
//! its whole duration, so N concurrent logins stall every other request sharing
//! the runtime. The `_async` wrappers push the work onto the blocking pool;
//! async callers should always prefer them.

use std::sync::OnceLock;

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;

/// An Argon2id hash of a fixed, unguessable value, computed once on first use.
///
/// Verifying against this costs the same as verifying a real user's hash, which
/// is what lets the login path spend identical CPU whether or not the submitted
/// email exists. Derived at runtime rather than hardcoded so it can never be a
/// malformed PHC string that silently short-circuits the comparison.
fn dummy_phc() -> &'static str {
    static DUMMY: OnceLock<String> = OnceLock::new();
    DUMMY.get_or_init(|| {
        hash_password("sauron-dummy-verification-target")
            // A fixed, valid fallback keeps `verify_password` on the same code
            // path (parse-then-verify) if hashing is somehow unavailable.
            .unwrap_or_else(|_| "$argon2id$v=19$m=19456,t=2,p=1$AAAAAAAAAAAAAAAA$AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_string())
    })
}

/// Hash a plaintext password into a PHC string (`$argon2id$...`).
pub fn hash_password(password: &str) -> anyhow::Result<String> {
    let mut salt_bytes = [0u8; 16];
    getrandom::fill(&mut salt_bytes).map_err(|e| anyhow::anyhow!("rng unavailable: {e}"))?;
    let salt =
        SaltString::encode_b64(&salt_bytes).map_err(|e| anyhow::anyhow!("salt encode: {e}"))?;
    let hash = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("hash: {e}"))?;
    Ok(hash.to_string())
}

/// Constant-time verify a password against a stored PHC hash.
pub fn verify_password(password: &str, phc: &str) -> bool {
    match PasswordHash::new(phc) {
        Ok(parsed) => Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

/// [`hash_password`] on the blocking pool — use this from async code.
pub async fn hash_password_async(password: String) -> anyhow::Result<String> {
    tokio::task::spawn_blocking(move || hash_password(&password))
        .await
        .map_err(|e| anyhow::anyhow!("password hash task failed: {e}"))?
}

/// [`verify_password`] on the blocking pool — use this from async code.
pub async fn verify_password_async(password: String, phc: String) -> bool {
    tokio::task::spawn_blocking(move || verify_password(&password, &phc))
        .await
        .unwrap_or(false)
}

/// Spend the same Argon2 work as a real verification, then report failure.
///
/// Called when no user matched the submitted email so that both branches of a
/// login cost the same wall-clock time.
pub async fn spend_dummy_verify(password: String) -> bool {
    // `dummy_phc()` is resolved INSIDE the blocking closure on purpose. Its
    // first call runs a full ~19 MiB Argon2 hash to build the target, and
    // evaluating it here — in the async context, outside `spawn_blocking` —
    // would stall the runtime worker for the whole of that first login, which
    // is exactly what this module's docs say must never happen. Subsequent
    // calls hit the `OnceLock` and are free either way.
    tokio::task::spawn_blocking(move || verify_password(&password, dummy_phc()))
        .await
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let phc = hash_password("s3cret-pass").unwrap();
        assert!(phc.starts_with("$argon2"));
        assert!(verify_password("s3cret-pass", &phc));
        assert!(!verify_password("wrong", &phc));
    }

    #[test]
    fn dummy_phc_is_a_parseable_hash_that_never_matches() {
        let phc = dummy_phc();
        assert!(
            PasswordHash::new(phc).is_ok(),
            "dummy PHC must parse, or the timing-equalizing verify would short-circuit"
        );
        assert!(!verify_password("anything", phc));
    }

    #[tokio::test]
    async fn async_wrappers_match_sync_behaviour() {
        let phc = hash_password_async("s3cret-pass".into()).await.unwrap();
        assert!(verify_password_async("s3cret-pass".into(), phc.clone()).await);
        assert!(!verify_password_async("wrong".into(), phc).await);
        // The dummy verify always fails, but does the work.
        assert!(!spend_dummy_verify("whatever".into()).await);
    }
}
