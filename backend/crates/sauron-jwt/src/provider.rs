//! A `jsonwebtoken` [`CryptoProvider`] backed by `ring`.
//!
//! `jsonwebtoken` 10 keeps token handling and cryptography apart: every
//! signature goes through a process-wide provider. It ships two behind
//! features and leaves room for a third. This is the third — why is in the
//! crate docs — covering the same twelve algorithms as the two it stands in
//! for, so nothing that worked before stops working.
//!
//! Key material arrives exactly as `jsonwebtoken` parsed it, and its PEM layer
//! was written against ring's expectations in the first place (a PKCS#8 RSA key
//! is unwrapped to its PKCS#1 body before it gets here), so nothing is
//! re-encoded:
//!
//! | family | signing key                | verifying key                        |
//! |--------|----------------------------|--------------------------------------|
//! | HMAC   | the raw secret             | the raw secret                       |
//! | RSA    | PKCS#1 `RSAPrivateKey` DER | PKCS#1 `RSAPublicKey` DER, or (n, e) |
//! | EC     | PKCS#8 DER                 | SEC1 uncompressed point              |
//! | Ed     | PKCS#8 DER                 | the raw 32-byte public key           |
//!
//! # Where ring is stricter than `rust_crypto` was
//!
//! Both are rejections of keys that should not be in use anyway, but they are
//! behaviour changes, so they are written down:
//!
//! * RSA keys under 2048 bits are refused, for signing and for verifying, and a
//!   signing key also needs a public exponent of at least 65537. `rsa` signed
//!   with a 1024-bit key, and with an exponent of 3.
//! * An ECDSA signing key must carry its public key inside the PKCS#8, and the
//!   two are checked against each other. Apple's `.p8` does; a key exported by
//!   a tool that omits the optional field does not.
//!
//! RSA signing keys are capped at 4096 bits; verification takes public keys up
//! to 8192.

use jsonwebtoken::crypto::{CryptoProvider, JwkUtils, JwtSigner, JwtVerifier};
use jsonwebtoken::errors::{ErrorKind, Result};
use jsonwebtoken::jwk::{EllipticCurve, ThumbprintHash};
use jsonwebtoken::signature::{Error as SignatureError, Signer, Verifier};
use jsonwebtoken::{Algorithm, DecodingKey, DecodingKeyKind, EncodingKey};
use ring::rand::SystemRandom;
use ring::signature::{self as ring_sig, KeyPair as _};
use ring::{digest, hmac};

/// The provider `sauron_jwt::encode` / `decode` install before first use.
pub(crate) static RING_PROVIDER: CryptoProvider = CryptoProvider {
    signer_factory: new_signer,
    verifier_factory: new_verifier,
    jwk_utils: JwkUtils {
        extract_rsa_public_key_components,
        extract_ec_public_key_coordinates,
        compute_digest,
    },
};

/// A key of one family must never be used with an algorithm of another.
///
/// `jsonwebtoken` checks this itself before signing, but on the verifying side
/// the provider is where the check lives — it is what stops an RSA public key
/// (public, by definition) from being accepted as an HMAC secret. Both of the
/// stock backends refuse with `InvalidKeyFormat`, so this does too.
fn require_same_family(key_family: jsonwebtoken::AlgorithmFamily, alg: Algorithm) -> Result<()> {
    if key_family == alg.family() {
        Ok(())
    } else {
        Err(ErrorKind::InvalidKeyFormat.into())
    }
}

// --- signing ---------------------------------------------------------------

enum SigningKey {
    Hmac(hmac::Key),
    Rsa(ring_sig::RsaKeyPair, &'static dyn ring_sig::RsaEncoding),
    Ecdsa(ring_sig::EcdsaKeyPair),
    Ed25519(ring_sig::Ed25519KeyPair),
}

struct RingSigner {
    algorithm: Algorithm,
    key: SigningKey,
}

fn rsa_signing_key(
    key: &EncodingKey,
    padding: &'static dyn ring_sig::RsaEncoding,
) -> Result<SigningKey> {
    let pair = ring_sig::RsaKeyPair::from_der(key.inner())
        .map_err(|e| ErrorKind::InvalidRsaKey(e.to_string()))?;
    Ok(SigningKey::Rsa(pair, padding))
}

fn ecdsa_signing_key(
    key: &EncodingKey,
    alg: &'static ring_sig::EcdsaSigningAlgorithm,
) -> Result<SigningKey> {
    let pair = ring_sig::EcdsaKeyPair::from_pkcs8(alg, key.inner(), &SystemRandom::new())
        .map_err(|_| ErrorKind::InvalidEcdsaKey)?;
    Ok(SigningKey::Ecdsa(pair))
}

fn new_signer(algorithm: &Algorithm, key: &EncodingKey) -> Result<Box<dyn JwtSigner>> {
    let algorithm = *algorithm;
    require_same_family(key.family(), algorithm)?;

    let key = match algorithm {
        Algorithm::HS256 => SigningKey::Hmac(hmac::Key::new(hmac::HMAC_SHA256, key.inner())),
        Algorithm::HS384 => SigningKey::Hmac(hmac::Key::new(hmac::HMAC_SHA384, key.inner())),
        Algorithm::HS512 => SigningKey::Hmac(hmac::Key::new(hmac::HMAC_SHA512, key.inner())),
        Algorithm::RS256 => rsa_signing_key(key, &ring_sig::RSA_PKCS1_SHA256)?,
        Algorithm::RS384 => rsa_signing_key(key, &ring_sig::RSA_PKCS1_SHA384)?,
        Algorithm::RS512 => rsa_signing_key(key, &ring_sig::RSA_PKCS1_SHA512)?,
        Algorithm::PS256 => rsa_signing_key(key, &ring_sig::RSA_PSS_SHA256)?,
        Algorithm::PS384 => rsa_signing_key(key, &ring_sig::RSA_PSS_SHA384)?,
        Algorithm::PS512 => rsa_signing_key(key, &ring_sig::RSA_PSS_SHA512)?,
        // JWS wants the fixed-width `r || s`, not the ASN.1 form TLS uses.
        Algorithm::ES256 => ecdsa_signing_key(key, &ring_sig::ECDSA_P256_SHA256_FIXED_SIGNING)?,
        Algorithm::ES384 => ecdsa_signing_key(key, &ring_sig::ECDSA_P384_SHA384_FIXED_SIGNING)?,
        // `_maybe_unchecked` because `openssl genpkey -algorithm ED25519` writes
        // PKCS#8 v1, which has no public key to check against. The strict
        // constructor would refuse every key made that way.
        Algorithm::EdDSA => SigningKey::Ed25519(
            ring_sig::Ed25519KeyPair::from_pkcs8_maybe_unchecked(key.inner())
                .map_err(|_| ErrorKind::InvalidEddsaKey)?,
        ),
    };

    Ok(Box::new(RingSigner { algorithm, key }))
}

impl Signer<Vec<u8>> for RingSigner {
    fn try_sign(&self, msg: &[u8]) -> std::result::Result<Vec<u8>, SignatureError> {
        match &self.key {
            SigningKey::Hmac(key) => Ok(hmac::sign(key, msg).as_ref().to_vec()),
            SigningKey::Rsa(pair, padding) => {
                let mut signature = vec![0; pair.public().modulus_len()];
                pair.sign(*padding, &SystemRandom::new(), msg, &mut signature)
                    .map_err(|_| SignatureError::new())?;
                Ok(signature)
            }
            SigningKey::Ecdsa(pair) => pair
                .sign(&SystemRandom::new(), msg)
                .map(|signature| signature.as_ref().to_vec())
                .map_err(|_| SignatureError::new()),
            SigningKey::Ed25519(pair) => Ok(pair.sign(msg).as_ref().to_vec()),
        }
    }
}

impl JwtSigner for RingSigner {
    fn algorithm(&self) -> Algorithm {
        self.algorithm
    }
}

// --- verifying -------------------------------------------------------------

enum VerifyingKey {
    Hmac(hmac::Key),
    Rsa(DecodingKey, &'static ring_sig::RsaParameters),
    /// ECDSA or Ed25519: a bare public key and the algorithm that reads it.
    Public(DecodingKey, &'static dyn ring_sig::VerificationAlgorithm),
}

struct RingVerifier {
    algorithm: Algorithm,
    key: VerifyingKey,
}

fn hmac_verifying_key(key: &DecodingKey, alg: hmac::Algorithm) -> Result<VerifyingKey> {
    Ok(VerifyingKey::Hmac(hmac::Key::new(
        alg,
        key.try_get_hmac_secret()?,
    )))
}

fn new_verifier(algorithm: &Algorithm, key: &DecodingKey) -> Result<Box<dyn JwtVerifier>> {
    let algorithm = *algorithm;
    require_same_family(key.family(), algorithm)?;

    let rsa = |params| VerifyingKey::Rsa(key.clone(), params);
    let public = |alg| VerifyingKey::Public(key.clone(), alg);

    let key = match algorithm {
        Algorithm::HS256 => hmac_verifying_key(key, hmac::HMAC_SHA256)?,
        Algorithm::HS384 => hmac_verifying_key(key, hmac::HMAC_SHA384)?,
        Algorithm::HS512 => hmac_verifying_key(key, hmac::HMAC_SHA512)?,
        Algorithm::RS256 => rsa(&ring_sig::RSA_PKCS1_2048_8192_SHA256),
        Algorithm::RS384 => rsa(&ring_sig::RSA_PKCS1_2048_8192_SHA384),
        Algorithm::RS512 => rsa(&ring_sig::RSA_PKCS1_2048_8192_SHA512),
        Algorithm::PS256 => rsa(&ring_sig::RSA_PSS_2048_8192_SHA256),
        Algorithm::PS384 => rsa(&ring_sig::RSA_PSS_2048_8192_SHA384),
        Algorithm::PS512 => rsa(&ring_sig::RSA_PSS_2048_8192_SHA512),
        Algorithm::ES256 => public(&ring_sig::ECDSA_P256_SHA256_FIXED),
        Algorithm::ES384 => public(&ring_sig::ECDSA_P384_SHA384_FIXED),
        Algorithm::EdDSA => public(&ring_sig::ED25519),
    };

    Ok(Box::new(RingVerifier { algorithm, key }))
}

impl Verifier<Vec<u8>> for RingVerifier {
    fn verify(&self, msg: &[u8], signature: &Vec<u8>) -> std::result::Result<(), SignatureError> {
        let outcome = match &self.key {
            // Constant-time: ring compares the recomputed tag without an early exit.
            VerifyingKey::Hmac(key) => hmac::verify(key, msg, signature),
            VerifyingKey::Rsa(key, params) => match key.kind() {
                DecodingKeyKind::SecretOrDer(der) => {
                    ring_sig::UnparsedPublicKey::new(*params, der).verify(msg, signature)
                }
                DecodingKeyKind::RsaModulusExponent { n, e } => {
                    ring_sig::RsaPublicKeyComponents { n, e }.verify(params, msg, signature)
                }
            },
            VerifyingKey::Public(key, alg) => match key.kind() {
                DecodingKeyKind::SecretOrDer(point) => {
                    ring_sig::UnparsedPublicKey::new(*alg, point).verify(msg, signature)
                }
                // Only an RSA key is ever built from components, and the family
                // check has already turned those away. Refuse rather than reach
                // for `DecodingKey::as_bytes`, which panics on this variant.
                DecodingKeyKind::RsaModulusExponent { .. } => Err(ring::error::Unspecified),
            },
        };
        outcome.map_err(|_| SignatureError::new())
    }
}

impl JwtVerifier for RingVerifier {
    fn algorithm(&self) -> Algorithm {
        self.algorithm
    }
}

// --- JWK helpers -----------------------------------------------------------
//
// Nothing in the workspace builds a JWK today. These are here so the provider
// is a complete stand-in: leaving them as `JwkUtils::new_unimplemented()` would
// turn the first `Jwk::from_encoding_key` someone writes into a panic.

fn extract_rsa_public_key_components(der: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    let pair =
        ring_sig::RsaKeyPair::from_der(der).map_err(|e| ErrorKind::InvalidRsaKey(e.to_string()))?;
    let parts = ring_sig::RsaPublicKeyComponents::<Vec<u8>>::from(pair.public());
    Ok((parts.n, parts.e))
}

fn extract_ec_public_key_coordinates(
    pkcs8: &[u8],
    algorithm: Algorithm,
) -> Result<(EllipticCurve, Vec<u8>, Vec<u8>)> {
    let (signing, curve, coordinate_len) = match algorithm {
        Algorithm::ES256 => (
            &ring_sig::ECDSA_P256_SHA256_FIXED_SIGNING,
            EllipticCurve::P256,
            32,
        ),
        Algorithm::ES384 => (
            &ring_sig::ECDSA_P384_SHA384_FIXED_SIGNING,
            EllipticCurve::P384,
            48,
        ),
        _ => return Err(ErrorKind::InvalidEcdsaKey.into()),
    };

    let pair = ring_sig::EcdsaKeyPair::from_pkcs8(signing, pkcs8, &SystemRandom::new())
        .map_err(|_| ErrorKind::InvalidEcdsaKey)?;

    // SEC1 uncompressed: 0x04 || X || Y.
    match pair.public_key().as_ref() {
        [0x04, coordinates @ ..] if coordinates.len() == 2 * coordinate_len => {
            let (x, y) = coordinates.split_at(coordinate_len);
            Ok((curve, x.to_vec(), y.to_vec()))
        }
        _ => Err(ErrorKind::InvalidEcdsaKey.into()),
    }
}

fn compute_digest(data: &[u8], hash: ThumbprintHash) -> Vec<u8> {
    let algorithm = match hash {
        ThumbprintHash::SHA256 => &digest::SHA256,
        ThumbprintHash::SHA384 => &digest::SHA384,
        ThumbprintHash::SHA512 => &digest::SHA512,
    };
    digest::digest(algorithm, data).as_ref().to_vec()
}
