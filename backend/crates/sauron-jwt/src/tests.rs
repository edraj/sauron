//! Tests for the ring-backed provider and the two functions in front of it.
//!
//! A round trip cannot tell a correct implementation from one that is wrong the
//! same way on both sides, so the anchor is the published examples: RFC 7515
//! Appendix A and RFC 8037 Appendix A each give a key, an input, and the
//! signature that has to come out. The round trips after them cover the
//! algorithms the RFCs give no example for.

mod rfc_examples;

use std::time::{SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use jsonwebtoken::crypto::{JwtSigner, JwtVerifier};
use ring::rand::SystemRandom;
use ring::signature::{self as ring_sig, KeyPair as _};
use serde::{Deserialize, Serialize};

use self::rfc_examples::{
    Example, ED25519_D, ED25519_X, EDDSA, ES256, ES256_X, ES256_Y, HS256, HS256_K, RS256, RS256_JWK,
};
use super::errors::ErrorKind;
use super::provider::RING_PROVIDER;
use super::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};

// --- helpers ---------------------------------------------------------------

fn b64u(text: &str) -> Vec<u8> {
    URL_SAFE_NO_PAD.decode(text).expect("valid base64url")
}

fn signer(algorithm: Algorithm, key: &EncodingKey) -> Box<dyn JwtSigner> {
    (RING_PROVIDER.signer_factory)(&algorithm, key).expect("a signer for this key")
}

fn verifier(algorithm: Algorithm, key: &DecodingKey) -> Box<dyn JwtVerifier> {
    (RING_PROVIDER.verifier_factory)(&algorithm, key).expect("a verifier for this key")
}

fn sign(algorithm: Algorithm, key: &EncodingKey, message: &[u8]) -> Vec<u8> {
    signer(algorithm, key).try_sign(message).expect("signing")
}

fn verifies(algorithm: Algorithm, key: &DecodingKey, message: &[u8], signature: &[u8]) -> bool {
    verifier(algorithm, key)
        .verify(message, &signature.to_vec())
        .is_ok()
}

/// One DER element: tag, length, body.
fn der(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    if body.len() < 0x80 {
        out.push(body.len() as u8);
    } else {
        let len = body.len().to_be_bytes();
        let len = &len[len.iter().take_while(|b| **b == 0).count()..];
        out.push(0x80 | len.len() as u8);
        out.extend_from_slice(len);
    }
    out.extend_from_slice(body);
    out
}

/// A non-negative DER INTEGER from big-endian magnitude bytes.
fn der_uint(magnitude: &[u8]) -> Vec<u8> {
    let zeros = magnitude.iter().take_while(|b| **b == 0).count();
    let mut body = magnitude[zeros.min(magnitude.len().saturating_sub(1))..].to_vec();
    if body[0] & 0x80 != 0 {
        body.insert(0, 0);
    }
    der(0x02, &body)
}

/// RFC 7515 A.2's key as a PKCS#1 `RSAPrivateKey` — the form `jsonwebtoken`
/// hands the provider, whatever wrapper the key arrived in.
fn rfc_rsa_pkcs1() -> Vec<u8> {
    let key = &RS256_JWK;
    let mut body = der_uint(&[0]); // version: two-prime
    for member in [key.n, key.e, key.d, key.p, key.q, key.dp, key.dq, key.qi] {
        body.extend(der_uint(&b64u(member)));
    }
    der(0x30, &body)
}

/// The matching PKCS#1 `RSAPublicKey`.
fn rfc_rsa_public_pkcs1() -> Vec<u8> {
    let mut body = der_uint(&b64u(RS256_JWK.n));
    body.extend(der_uint(&b64u(RS256_JWK.e)));
    der(0x30, &body)
}

/// RFC 8037's Ed25519 seed inside a PKCS#8 v1 wrapper, which is what
/// `openssl genpkey -algorithm ED25519` writes.
fn rfc_ed25519_pkcs8() -> Vec<u8> {
    let mut body = der_uint(&[0]);
    body.extend(der(0x30, &[0x06, 0x03, 0x2b, 0x65, 0x70])); // id-Ed25519
    body.extend(der(0x04, &der(0x04, &b64u(ED25519_D))));
    der(0x30, &body)
}

fn rfc_rsa_keys() -> (EncodingKey, DecodingKey) {
    (
        EncodingKey::from_rsa_der(&rfc_rsa_pkcs1()),
        DecodingKey::from_rsa_der(&rfc_rsa_public_pkcs1()),
    )
}

/// A fresh ECDSA key pair, as PKCS#8 and as the public point.
fn generated_ecdsa(algorithm: &'static ring_sig::EcdsaSigningAlgorithm) -> (Vec<u8>, Vec<u8>) {
    let rng = SystemRandom::new();
    let pkcs8 = ring_sig::EcdsaKeyPair::generate_pkcs8(algorithm, &rng).expect("generate");
    let pair = ring_sig::EcdsaKeyPair::from_pkcs8(algorithm, pkcs8.as_ref(), &rng).expect("parse");
    (pkcs8.as_ref().to_vec(), pair.public_key().as_ref().to_vec())
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after 1970")
        .as_secs() as i64
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Claims {
    sub: String,
    exp: i64,
}

fn claims() -> Claims {
    Claims {
        sub: "user-1".to_string(),
        exp: now() + 600,
    }
}

/// The RFC examples expired in 2011. Everything but the signature is beside
/// the point for them.
fn signature_only(algorithm: Algorithm) -> Validation {
    let mut validation = Validation::new(algorithm);
    validation.validate_exp = false;
    validation
}

// --- known answers ---------------------------------------------------------

fn assert_reproduces(example: &Example, algorithm: Algorithm, key: &EncodingKey) {
    let signature = sign(algorithm, key, example.signing_input().as_bytes());
    assert_eq!(
        URL_SAFE_NO_PAD.encode(signature),
        example.signature,
        "{algorithm:?} did not produce the signature the RFC prints"
    );
}

#[test]
fn hs256_reproduces_the_rfc_7515_signature() {
    assert_reproduces(
        &HS256,
        Algorithm::HS256,
        &EncodingKey::from_secret(&b64u(HS256_K)),
    );
}

#[test]
fn rs256_reproduces_the_rfc_7515_signature() {
    assert_reproduces(&RS256, Algorithm::RS256, &rfc_rsa_keys().0);
}

#[test]
fn eddsa_reproduces_the_rfc_8037_signature() {
    assert_reproduces(
        &EDDSA,
        Algorithm::EdDSA,
        &EncodingKey::from_ed_der(&rfc_ed25519_pkcs8()),
    );
    assert!(verifies(
        Algorithm::EdDSA,
        &DecodingKey::from_ed_der(&b64u(ED25519_X)),
        EDDSA.signing_input().as_bytes(),
        &b64u(EDDSA.signature),
    ));
}

/// The same three examples, but as whole tokens through the public `decode` —
/// the path a real request takes.
#[test]
fn the_rfc_7515_tokens_decode() {
    let cases = [
        (
            &HS256,
            Algorithm::HS256,
            DecodingKey::from_secret(&b64u(HS256_K)),
        ),
        (&RS256, Algorithm::RS256, rfc_rsa_keys().1),
        (
            &ES256,
            Algorithm::ES256,
            DecodingKey::from_ec_components(ES256_X, ES256_Y).expect("the A.3 public key"),
        ),
    ];
    for (example, algorithm, key) in cases {
        let data = decode::<serde_json::Value>(example.token(), &key, &signature_only(algorithm))
            .unwrap_or_else(|e| panic!("{algorithm:?}: {e}"));
        assert_eq!(data.header.alg, algorithm);
        assert_eq!(data.claims["iss"], "joe");
        assert_eq!(data.claims["exp"], 1_300_819_380);
    }
}

/// An RSA public key can also arrive as its two numbers — that is how a JWKS
/// publishes one — and takes a different branch in the verifier.
#[test]
fn rs256_verifies_against_a_key_given_as_modulus_and_exponent() {
    let key = DecodingKey::from_rsa_components(RS256_JWK.n, RS256_JWK.e).expect("n and e");
    assert!(verifies(
        Algorithm::RS256,
        &key,
        RS256.signing_input().as_bytes(),
        &b64u(RS256.signature),
    ));
}

// --- rejection -------------------------------------------------------------

#[test]
fn one_changed_byte_anywhere_fails_verification() {
    let cases = [
        (
            &HS256,
            Algorithm::HS256,
            DecodingKey::from_secret(&b64u(HS256_K)),
        ),
        (&RS256, Algorithm::RS256, rfc_rsa_keys().1),
        (
            &ES256,
            Algorithm::ES256,
            DecodingKey::from_ec_components(ES256_X, ES256_Y).expect("the A.3 public key"),
        ),
        (
            &EDDSA,
            Algorithm::EdDSA,
            DecodingKey::from_ed_der(&b64u(ED25519_X)),
        ),
    ];
    for (example, algorithm, key) in cases {
        let message = example.signing_input().into_bytes();
        let signature = b64u(example.signature);
        assert!(
            verifies(algorithm, &key, &message, &signature),
            "{algorithm:?}"
        );

        let mut other_message = message.clone();
        other_message[0] ^= 1;
        assert!(
            !verifies(algorithm, &key, &other_message, &signature),
            "{algorithm:?} accepted a signature over a different message"
        );

        let mut other_signature = signature.clone();
        *other_signature.last_mut().expect("non-empty") ^= 1;
        assert!(
            !verifies(algorithm, &key, &message, &other_signature),
            "{algorithm:?} accepted a corrupted signature"
        );

        assert!(
            !verifies(algorithm, &key, &message, &signature[..signature.len() - 1]),
            "{algorithm:?} accepted a truncated signature"
        );
        assert!(
            !verifies(algorithm, &key, &message, &[]),
            "{algorithm:?} accepted an empty signature"
        );
    }
}

#[test]
fn a_token_signed_with_another_secret_is_refused() {
    let token = encode(
        &Header::default(),
        &claims(),
        &EncodingKey::from_secret(b"the-secret-this-was-signed-with-0000"),
    )
    .expect("encode");
    let err = decode::<Claims>(
        &token,
        &DecodingKey::from_secret(b"a-different-secret-of-equal-length-0"),
        &Validation::new(Algorithm::HS256),
    )
    .expect_err("a different secret must not verify");
    assert!(matches!(err.kind(), ErrorKind::InvalidSignature), "{err:?}");
}

/// The check that stops algorithm confusion lives in the provider on the
/// verifying side, so it is pinned here family by family.
#[test]
fn a_key_is_refused_for_an_algorithm_of_another_family() {
    let (rsa_private, rsa_public) = rfc_rsa_keys();
    let secret = b"test-secret-please-change-0000000000";
    let (ec_pkcs8, ec_point) = generated_ecdsa(&ring_sig::ECDSA_P256_SHA256_FIXED_SIGNING);

    let signing = [
        (Algorithm::HS256, &rsa_private),
        (Algorithm::RS256, &EncodingKey::from_secret(secret)),
        (Algorithm::ES256, &rsa_private),
        (Algorithm::EdDSA, &EncodingKey::from_ec_der(&ec_pkcs8)),
        (Algorithm::PS256, &EncodingKey::from_ec_der(&ec_pkcs8)),
    ];
    for (algorithm, key) in signing {
        let err = (RING_PROVIDER.signer_factory)(&algorithm, key)
            .err()
            .unwrap_or_else(|| panic!("{algorithm:?} signer accepted a foreign key"));
        assert!(matches!(err.kind(), ErrorKind::InvalidKeyFormat), "{err:?}");
    }

    let verifying = [
        (Algorithm::HS256, &rsa_public),
        (Algorithm::HS512, &DecodingKey::from_ec_der(&ec_point)),
        (Algorithm::RS256, &DecodingKey::from_secret(secret)),
        (Algorithm::ES256, &rsa_public),
        (Algorithm::EdDSA, &DecodingKey::from_ec_der(&ec_point)),
    ];
    for (algorithm, key) in verifying {
        let err = (RING_PROVIDER.verifier_factory)(&algorithm, key)
            .err()
            .unwrap_or_else(|| panic!("{algorithm:?} verifier accepted a foreign key"));
        assert!(matches!(err.kind(), ErrorKind::InvalidKeyFormat), "{err:?}");
    }
}

/// The classic attack: the RSA public key is public, so an attacker signs an
/// HS256 token using it as the HMAC secret and hopes the server verifies with
/// "its key". It must fail whether or not the caller pinned the algorithm.
#[test]
fn an_rsa_public_key_cannot_be_replayed_as_an_hmac_secret() {
    let public_der = rfc_rsa_public_pkcs1();
    let forged = encode(
        &Header::new(Algorithm::HS256),
        &claims(),
        &EncodingKey::from_secret(&public_der),
    )
    .expect("the attacker can always sign");
    let key = DecodingKey::from_rsa_der(&public_der);

    assert!(decode::<Claims>(&forged, &key, &Validation::new(Algorithm::RS256)).is_err());

    let mut careless = Validation::new(Algorithm::RS256);
    careless.algorithms = vec![Algorithm::RS256, Algorithm::HS256];
    assert!(decode::<Claims>(&forged, &key, &careless).is_err());
}

#[test]
fn a_malformed_private_key_is_an_error_not_a_panic() {
    let junk = [0x30, 0x03, 0x02, 0x01, 0x00];
    let cases = [
        (Algorithm::RS256, EncodingKey::from_rsa_der(&junk)),
        (Algorithm::PS512, EncodingKey::from_rsa_der(&[])),
        (Algorithm::ES256, EncodingKey::from_ec_der(&junk)),
        (Algorithm::EdDSA, EncodingKey::from_ed_der(&junk)),
    ];
    for (algorithm, key) in cases {
        assert!(
            encode(&Header::new(algorithm), &claims(), &key).is_err(),
            "{algorithm:?} signed with a key that is not a key"
        );
    }
}

/// P-256 and P-384 keys are both "EC" as far as `jsonwebtoken` is concerned,
/// so the family check cannot tell them apart. ring can.
#[test]
fn an_ecdsa_key_is_refused_for_the_other_curve() {
    let (p256, _) = generated_ecdsa(&ring_sig::ECDSA_P256_SHA256_FIXED_SIGNING);
    let err = (RING_PROVIDER.signer_factory)(&Algorithm::ES384, &EncodingKey::from_ec_der(&p256))
        .err()
        .expect("a P-256 key must not sign ES384");
    assert!(matches!(err.kind(), ErrorKind::InvalidEcdsaKey), "{err:?}");
}

// --- round trips -----------------------------------------------------------

#[test]
fn every_algorithm_signs_what_it_then_verifies() {
    let secret = b"test-secret-please-change-0000000000";
    let (rsa_private, rsa_public) = rfc_rsa_keys();
    let (p256, p256_point) = generated_ecdsa(&ring_sig::ECDSA_P256_SHA256_FIXED_SIGNING);
    let (p384, p384_point) = generated_ecdsa(&ring_sig::ECDSA_P384_SHA384_FIXED_SIGNING);
    let hmac = || {
        (
            EncodingKey::from_secret(secret),
            DecodingKey::from_secret(secret),
        )
    };
    let rsa = || (rsa_private.clone(), rsa_public.clone());

    let cases = [
        (Algorithm::HS256, hmac(), 32),
        (Algorithm::HS384, hmac(), 48),
        (Algorithm::HS512, hmac(), 64),
        (Algorithm::RS256, rsa(), 256),
        (Algorithm::RS384, rsa(), 256),
        (Algorithm::RS512, rsa(), 256),
        (Algorithm::PS256, rsa(), 256),
        (Algorithm::PS384, rsa(), 256),
        (Algorithm::PS512, rsa(), 256),
        (
            Algorithm::ES256,
            (
                EncodingKey::from_ec_der(&p256),
                DecodingKey::from_ec_der(&p256_point),
            ),
            64,
        ),
        (
            Algorithm::ES384,
            (
                EncodingKey::from_ec_der(&p384),
                DecodingKey::from_ec_der(&p384_point),
            ),
            96,
        ),
        (
            Algorithm::EdDSA,
            (
                EncodingKey::from_ed_der(&rfc_ed25519_pkcs8()),
                DecodingKey::from_ed_der(&b64u(ED25519_X)),
            ),
            64,
        ),
    ];

    // Twelve algorithms exist; a thirteenth must not slip past this test.
    assert_eq!(cases.len(), 12);

    for (algorithm, (private, public), signature_len) in cases {
        let message = b"header.payload";
        let made = signer(algorithm, &private);
        assert_eq!(made.algorithm(), algorithm);
        assert_eq!(verifier(algorithm, &public).algorithm(), algorithm);

        let signature = made.try_sign(message).expect("signing");
        assert_eq!(signature.len(), signature_len, "{algorithm:?}");
        assert!(
            verifies(algorithm, &public, message, &signature),
            "{algorithm:?}"
        );
        assert!(
            !verifies(algorithm, &public, b"header.other", &signature),
            "{algorithm:?}"
        );
    }
}

#[test]
fn the_hash_is_part_of_the_algorithm() {
    let (private, public) = rfc_rsa_keys();
    let message = b"header.payload";

    let pkcs1 = sign(Algorithm::RS256, &private, message);
    assert!(!verifies(Algorithm::RS384, &public, message, &pkcs1));
    assert!(!verifies(Algorithm::PS256, &public, message, &pkcs1));

    let pss = sign(Algorithm::PS256, &private, message);
    assert!(verifies(Algorithm::PS256, &public, message, &pss));
    assert!(!verifies(Algorithm::RS256, &public, message, &pss));
    // PSS is salted: two signatures over one message differ, and both hold.
    let again = sign(Algorithm::PS256, &private, message);
    assert_ne!(pss, again);
    assert!(verifies(Algorithm::PS256, &public, message, &again));

    let secret = b"test-secret-please-change-0000000000";
    let tag = sign(Algorithm::HS256, &EncodingKey::from_secret(secret), message);
    assert!(!verifies(
        Algorithm::HS512,
        &DecodingKey::from_secret(secret),
        message,
        &tag
    ));
}

// --- the two public functions ----------------------------------------------

#[test]
fn encode_then_decode_returns_the_claims() {
    let secret = b"test-secret-please-change-0000000000";
    let sent = claims();
    let token =
        encode(&Header::default(), &sent, &EncodingKey::from_secret(secret)).expect("encode");
    assert_eq!(token.split('.').count(), 3);

    let data = decode::<Claims>(
        &token,
        &DecodingKey::from_secret(secret),
        &Validation::new(Algorithm::HS256),
    )
    .expect("decode");
    assert_eq!(data.claims, sent);
    assert_eq!(data.header.alg, Algorithm::HS256);
}

/// Swapping the backend must not have touched claim validation, which is
/// `jsonwebtoken`'s and not the provider's.
#[test]
fn an_expired_token_is_still_refused_as_expired() {
    let secret = b"test-secret-please-change-0000000000";
    let stale = Claims {
        sub: "user-1".to_string(),
        exp: now() - 3600,
    };
    let token = encode(
        &Header::default(),
        &stale,
        &EncodingKey::from_secret(secret),
    )
    .expect("encode");
    let err = decode::<Claims>(
        &token,
        &DecodingKey::from_secret(secret),
        &Validation::new(Algorithm::HS256),
    )
    .expect_err("expired");
    assert!(matches!(err.kind(), ErrorKind::ExpiredSignature), "{err:?}");
}

/// `encode` and `decode` install the provider on first use. Nothing orders the
/// first use across threads — an API worker and a background job can both get
/// there first.
#[test]
fn first_use_from_many_threads_at_once() {
    let workers: Vec<_> = (0..16)
        .map(|n| {
            std::thread::spawn(move || {
                let secret = format!("test-secret-please-change-{n:010}");
                let sent = Claims {
                    sub: format!("user-{n}"),
                    exp: now() + 600,
                };
                let token = encode(
                    &Header::default(),
                    &sent,
                    &EncodingKey::from_secret(secret.as_bytes()),
                )
                .expect("encode");
                let data = decode::<Claims>(
                    &token,
                    &DecodingKey::from_secret(secret.as_bytes()),
                    &Validation::new(Algorithm::HS256),
                )
                .expect("decode");
                assert_eq!(data.claims, sent);
            })
        })
        .collect();
    for worker in workers {
        worker.join().expect("worker");
    }
}

// --- JWK helpers -----------------------------------------------------------

#[test]
fn the_public_half_is_recovered_from_a_private_key() {
    let (n, e) =
        (RING_PROVIDER.jwk_utils.extract_rsa_public_key_components)(&rfc_rsa_pkcs1()).expect("rsa");
    assert_eq!(n, b64u(RS256_JWK.n));
    assert_eq!(e, b64u(RS256_JWK.e));

    let (pkcs8, point) = generated_ecdsa(&ring_sig::ECDSA_P256_SHA256_FIXED_SIGNING);
    let (curve, x, y) =
        (RING_PROVIDER.jwk_utils.extract_ec_public_key_coordinates)(&pkcs8, Algorithm::ES256)
            .expect("ec");
    assert_eq!(curve, jsonwebtoken::jwk::EllipticCurve::P256);
    assert_eq!([&[0x04][..], &x[..], &y[..]].concat(), point);

    assert!(
        (RING_PROVIDER.jwk_utils.extract_ec_public_key_coordinates)(&pkcs8, Algorithm::RS256)
            .is_err()
    );
}

#[test]
fn digests_match_the_fips_180_examples() {
    use jsonwebtoken::jwk::ThumbprintHash::{SHA256, SHA384, SHA512};
    let digest = |hash| hex((RING_PROVIDER.jwk_utils.compute_digest)(b"abc", hash));
    assert_eq!(
        digest(SHA256),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        digest(SHA384),
        "cb00753f45a35e8bb5a03d699ac65007272c32ab0eded1631a8b605a43ff5bed\
         8086072ba1e7cc2358baeca134c825a7"
    );
    assert_eq!(
        digest(SHA512),
        "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a\
         2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
    );
}

fn hex(bytes: Vec<u8>) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// --- keys as PEM text ------------------------------------------------------
//
// This is how `sauron-store` receives both of its credentials, so these two
// are the closest thing here to the real calls: PEM text in, signed token out.
// The PEM is assembled at run time from DER, never written down — there is no
// private key in this file to find.

#[cfg(feature = "pem")]
mod pem {
    use base64::engine::general_purpose::STANDARD;

    use super::*;

    fn armor(label: &str, der: &[u8]) -> String {
        let body = STANDARD.encode(der);
        let lines: Vec<&str> = body
            .as_bytes()
            .chunks(64)
            .map(|line| std::str::from_utf8(line).expect("base64 is ascii"))
            .collect();
        format!(
            "-----BEGIN {label}-----\n{}\n-----END {label}-----\n",
            lines.join("\n")
        )
    }

    /// A Google service-account `private_key`: an RSA key in a PKCS#8 wrapper.
    #[test]
    fn a_pkcs8_rsa_pem_signs_rs256() {
        const RSA_ENCRYPTION: [u8; 15] = [
            0x30, 0x0d, 0x06, 0x09, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01, 0x05,
            0x00,
        ];
        let mut body = der_uint(&[0]);
        body.extend(RSA_ENCRYPTION);
        body.extend(der(0x04, &rfc_rsa_pkcs1()));
        let text = armor("PRIVATE KEY", &der(0x30, &body));

        let key = EncodingKey::from_rsa_pem(text.as_bytes()).expect("a PKCS#8 RSA key");
        // Same key, same input, deterministic scheme: the wrapper must not
        // change what comes out.
        assert_reproduces(&RS256, Algorithm::RS256, &key);

        let token = encode(&Header::new(Algorithm::RS256), &claims(), &key).expect("encode");
        decode::<Claims>(
            &token,
            &rfc_rsa_keys().1,
            &Validation::new(Algorithm::RS256),
        )
        .expect("decode");
    }

    /// The same key without the wrapper — `BEGIN RSA PRIVATE KEY`.
    #[test]
    fn a_pkcs1_rsa_pem_signs_rs256() {
        let text = armor("RSA PRIVATE KEY", &rfc_rsa_pkcs1());
        let key = EncodingKey::from_rsa_pem(text.as_bytes()).expect("a PKCS#1 RSA key");
        assert_reproduces(&RS256, Algorithm::RS256, &key);
    }

    /// An Apple `.p8`: a P-256 key in PKCS#8 with its public key included.
    #[test]
    fn a_pkcs8_ec_pem_signs_es256() {
        let (pkcs8, point) = generated_ecdsa(&ring_sig::ECDSA_P256_SHA256_FIXED_SIGNING);
        let text = armor("PRIVATE KEY", &pkcs8);
        let key = EncodingKey::from_ec_pem(text.as_bytes()).expect("a PKCS#8 EC key");

        let mut header = Header::new(Algorithm::ES256);
        header.kid = Some("ABC123DEFG".to_string());
        let token = encode(&header, &claims(), &key).expect("encode");

        let data = decode::<Claims>(
            &token,
            &DecodingKey::from_ec_der(&point),
            &Validation::new(Algorithm::ES256),
        )
        .expect("decode");
        assert_eq!(data.header.kid.as_deref(), Some("ABC123DEFG"));
    }

    #[test]
    fn a_key_of_the_wrong_kind_is_refused_at_the_pem() {
        let (pkcs8, _) = generated_ecdsa(&ring_sig::ECDSA_P256_SHA256_FIXED_SIGNING);
        let ec = armor("PRIVATE KEY", &pkcs8);
        assert!(EncodingKey::from_rsa_pem(ec.as_bytes()).is_err());

        let rsa = armor("RSA PRIVATE KEY", &rfc_rsa_pkcs1());
        assert!(EncodingKey::from_ec_pem(rsa.as_bytes()).is_err());
    }
}
