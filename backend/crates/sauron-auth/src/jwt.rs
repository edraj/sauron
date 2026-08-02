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
    /// The holder owes a password change; the extractor rejects every request
    /// except the change itself. `serde(default)` because tokens issued before
    /// this field existed must keep decoding across the deploy.
    #[serde(default)]
    pub must_change_password: bool,
    /// The `auth_sessions.id` this token was minted for.
    ///
    /// `Option` + `serde(default)` because tokens issued before this field
    /// existed must keep decoding across the deploy — the same reason
    /// `must_change_password` is defaulted, and
    /// `tokens_minted_before_the_flag_existed_still_decode` is the pin.
    ///
    /// Deliberately not `jti`: `jti` is per-token and is regenerated on every
    /// rotation, so reusing it would destroy the identity-across-rotation
    /// property this field exists to create.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sid: Option<Uuid>,
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
    pub fn issue_access(
        &self,
        user_id: Uuid,
        must_change_password: bool,
        session_id: Option<Uuid>,
    ) -> anyhow::Result<(String, i64)> {
        let now = Utc::now().timestamp();
        let exp = now + self.access_ttl_secs;
        let claims = Claims {
            sub: user_id.to_string(),
            iat: now,
            exp,
            jti: sauron_core::ids::random_hex(8),
            typ: "access".to_string(),
            must_change_password,
            sid: session_id,
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
        let (token, _exp) = keys.issue_access(uid, false, None).unwrap();
        let claims = keys.decode_access(&token).unwrap();
        assert_eq!(claims.sub, uid.to_string());
        assert_eq!(claims.typ, "access");
    }

    #[test]
    fn access_token_carries_the_password_change_flag() {
        let keys = JwtKeys::new("test-secret-please-change-0000000000", 900);
        let uid = Uuid::new_v4();
        let (token, _) = keys.issue_access(uid, true, None).unwrap();
        assert!(keys.decode_access(&token).unwrap().must_change_password);

        let (token, _) = keys.issue_access(uid, false, None).unwrap();
        assert!(!keys.decode_access(&token).unwrap().must_change_password);
    }

    #[test]
    fn session_id_round_trips_as_the_sid_claim() {
        let keys = JwtKeys::new("test-secret-please-change-0000000000", 900);
        let uid = Uuid::new_v4();
        let sid = Uuid::new_v4();
        let (token, _) = keys.issue_access(uid, false, Some(sid)).unwrap();
        assert_eq!(keys.decode_access(&token).unwrap().sid, Some(sid));

        let (token, _) = keys.issue_access(uid, false, None).unwrap();
        assert_eq!(keys.decode_access(&token).unwrap().sid, None);
    }

    /// The property this whole slice exists to create: identity that survives a
    /// rotation. Two separately-minted tokens for the same session must name the
    /// same session — which is exactly why `sid` is not `jti` (`jti` is
    /// per-token and is regenerated on every call).
    #[test]
    fn two_tokens_for_one_session_carry_the_same_sid() {
        let keys = JwtKeys::new("test-secret-please-change-0000000000", 900);
        let uid = Uuid::new_v4();
        let sid = Uuid::new_v4();
        let (a, _) = keys.issue_access(uid, false, Some(sid)).unwrap();
        let (b, _) = keys.issue_access(uid, false, Some(sid)).unwrap();
        let (ca, cb) = (
            keys.decode_access(&a).unwrap(),
            keys.decode_access(&b).unwrap(),
        );
        assert_eq!(ca.sid, cb.sid);
        assert_ne!(ca.jti, cb.jti, "jti is per-token and must not be reused");
    }

    #[test]
    fn tokens_minted_before_the_flag_existed_still_decode() {
        // Sessions live across a deploy. A token issued by the previous build
        // has no `must_change_password` field at all; without #[serde(default)]
        // every logged-in user is signed out the moment this ships.
        use jsonwebtoken::{encode, EncodingKey, Header};
        #[derive(serde::Serialize)]
        struct LegacyClaims {
            sub: String,
            iat: i64,
            exp: i64,
            jti: String,
            typ: String,
        }
        let uid = Uuid::new_v4();
        let now = Utc::now().timestamp();
        let legacy = LegacyClaims {
            sub: uid.to_string(),
            iat: now,
            exp: now + 900,
            jti: "abc123".to_string(),
            typ: "access".to_string(),
        };
        let secret = "test-secret-please-change-0000000000";
        let token = encode(
            &Header::default(),
            &legacy,
            &EncodingKey::from_secret(secret.as_bytes()),
        )
        .unwrap();

        let keys = JwtKeys::new(secret, 900);
        let claims = keys.decode_access(&token).unwrap();
        assert_eq!(claims.sub, uid.to_string());
        assert!(!claims.must_change_password);
        // Same reason, one deploy later: a token minted before `sid` existed has
        // no such field. Rejecting it would sign out every logged-in user at
        // deploy — the exact failure this test was originally written to
        // prevent. A sid-less token is accepted, shows no "This device" badge,
        // and is refused by the two self-service revoke endpoints; the condition
        // clears within JWT_ACCESS_TTL_SECS because every login and refresh
        // mints a `sid`.
        assert_eq!(claims.sid, None);
    }

    #[test]
    fn token_hash_is_stable_and_hex() {
        let h = hash_token("abc123");
        assert_eq!(h.len(), 64);
        assert_eq!(h, hash_token("abc123"));
        assert_ne!(h, hash_token("abc124"));
    }
}
