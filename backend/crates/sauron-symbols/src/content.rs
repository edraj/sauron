//! Content-addressing + bounded (de)compression for symbol artifacts.
//!
//! Artifacts are stored content-addressed by their raw-bytes SHA-256 and held
//! zstd-compressed. Decompression is bomb-guarded: it streams and aborts the
//! moment the output would exceed the caller's cap, so a tiny malicious blob
//! can't expand to gigabytes and OOM a worker.

use sha2::{Digest, Sha256};

#[derive(Debug, thiserror::Error)]
pub enum SymbolError {
    #[error("artifact too large: {size} bytes exceeds cap {cap}")]
    TooLarge { size: usize, cap: usize },
    #[error("decompression failed: {0}")]
    Decompress(String),
    #[error("corrupt artifact: {0}")]
    Corrupt(String),
}

/// SHA-256 of the raw (uncompressed) artifact bytes — the content address.
pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let out = hasher.finalize();
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&out);
    arr
}

/// Lowercase hex of a byte slice (thin re-export of `hex::encode`).
pub fn hex(bytes: &[u8]) -> String {
    hex::encode(bytes)
}

/// zstd-compress at a high ratio (artifacts are written rarely, read via cache).
pub fn compress(raw: &[u8]) -> Vec<u8> {
    zstd::encode_all(raw, 19).expect("zstd encode of an in-memory buffer is infallible")
}

/// Streaming decompress that aborts once `max_uncompressed` bytes are produced.
pub fn decompress(comp: &[u8], max_uncompressed: usize) -> Result<Vec<u8>, SymbolError> {
    use std::io::Read;
    let mut dec =
        zstd::stream::read::Decoder::new(comp).map_err(|e| SymbolError::Decompress(e.to_string()))?;
    let mut out = Vec::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = dec
            .read(&mut buf)
            .map_err(|e| SymbolError::Decompress(e.to_string()))?;
        if n == 0 {
            break;
        }
        if out.len() + n > max_uncompressed {
            return Err(SymbolError::TooLarge {
                size: out.len() + n,
                cap: max_uncompressed,
            });
        }
        out.extend_from_slice(&buf[..n]);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_is_stable_and_hex() {
        let h = sha256(b"hello");
        assert_eq!(
            hex(&h),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn zstd_roundtrips_under_cap() {
        let raw = b"the quick brown fox".repeat(100);
        let comp = compress(&raw);
        assert!(comp.len() < raw.len());
        let back = decompress(&comp, 1 << 20).unwrap();
        assert_eq!(back, raw);
    }

    #[test]
    fn decompress_rejects_bomb() {
        let raw = vec![0u8; 4 << 20]; // 4 MiB of zeros -> tiny compressed
        let comp = compress(&raw);
        let err = decompress(&comp, 1 << 20).unwrap_err(); // cap 1 MiB
        assert!(matches!(err, SymbolError::TooLarge { .. }));
    }
}
