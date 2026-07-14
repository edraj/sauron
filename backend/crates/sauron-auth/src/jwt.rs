//! JWT access tokens + opaque refresh-token hashing.

use std::sync::Arc;

use chrono::Utc;
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Access-token claims. Deliberately carries no roles — authorization is
/// resolved fresh per request so revocation is immediate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub iat: i64,
    pub exp: i64,
    pub jti: String,
    pub typ: String,
}

/// HS256 signing/verification keys plus the access-token TTL.
#[derive(Clone)]
pub struct JwtKeys {
    enc: Arc<EncodingKey>,
    dec: Arc<DecodingKey>,
    access_ttl_secs: i64,
}

impl JwtKeys {
    pub fn new(secret: &str, access_ttl_secs: i64) -> Self {
        Self {
            enc: Arc::new(EncodingKey::from_secret(secret.as_bytes())),
            dec: Arc::new(DecodingKey::from_secret(secret.as_bytes())),
            access_ttl_secs,
        }
    }

    /// Issue a signed access token; returns `(token, expires_at_unix)`.
    pub fn issue_access(&self, user_id: Uuid) -> anyhow::Result<(String, i64)> {
        let now = Utc::now().timestamp();
        let exp = now + self.access_ttl_secs;
        let claims = Claims {
            sub: user_id.to_string(),
            iat: now,
            exp,
            jti: sauron_core::ids::random_hex(8),
            typ: "access".to_string(),
        };
        let token = encode(&Header::default(), &claims, &self.enc)
            .map_err(|e| anyhow::anyhow!("jwt encode: {e}"))?;
        Ok((token, exp))
    }

    /// Decode + validate an access token.
    pub fn decode_access(&self, token: &str) -> anyhow::Result<Claims> {
        let mut validation = Validation::new(Algorithm::HS256);
        validation.validate_exp = true;
        let data = decode::<Claims>(token, &self.dec, &validation)
            .map_err(|e| anyhow::anyhow!("jwt decode: {e}"))?;
        if data.claims.typ != "access" {
            anyhow::bail!("not an access token");
        }
        Ok(data.claims)
    }
}

/// Hash an opaque refresh token for storage (raw token is never persisted).
pub fn hash_token(raw: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(raw.as_bytes());
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_token_roundtrip() {
        let keys = JwtKeys::new("test-secret-please-change-0000000000", 900);
        let uid = Uuid::new_v4();
        let (token, _exp) = keys.issue_access(uid).unwrap();
        let claims = keys.decode_access(&token).unwrap();
        assert_eq!(claims.sub, uid.to_string());
        assert_eq!(claims.typ, "access");
    }

    #[test]
    fn token_hash_is_stable_and_hex() {
        let h = hash_token("abc123");
        assert_eq!(h.len(), 64);
        assert_eq!(h, hash_token("abc123"));
        assert_ne!(h, hash_token("abc124"));
    }
}
