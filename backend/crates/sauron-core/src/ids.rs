//! Small id / secret helpers.

use uuid::Uuid;

/// Time-sortable v7 UUID — used as the primary key for high-volume event rows
/// so inserts stay append-friendly.
pub fn uuid_v7() -> Uuid {
    Uuid::now_v7()
}

/// `len` bytes of OS randomness, hex-encoded.
pub fn random_hex(len: usize) -> String {
    let mut buf = vec![0u8; len];
    getrandom::fill(&mut buf).expect("OS RNG must be available");
    hex::encode(buf)
}

/// A non-secret, write-only project ingest key (the DSN public key).
pub fn public_key() -> String {
    format!("pk_{}", random_hex(16))
}

/// A high-entropy opaque token (e.g. a refresh token). Only its hash is stored.
pub fn opaque_token() -> String {
    random_hex(32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_keys_are_unique_and_prefixed() {
        let a = public_key();
        let b = public_key();
        assert!(a.starts_with("pk_"));
        assert_ne!(a, b);
        assert_eq!(a.len(), 3 + 32); // "pk_" + 16 bytes hex
    }

    #[test]
    fn uuid_v7_is_monotonic_ish() {
        let a = uuid_v7();
        let b = uuid_v7();
        assert_ne!(a, b);
    }
}
