//! Published example keys and signatures, copied verbatim from RFC 7515
//! Appendix A and RFC 8037 Appendix A.
//!
//! Every value in this file is printed in a public standard and protects
//! nothing. They live in a file of their own so that `.gitleaks.toml` can
//! allowlist this one path: the scanner is right that these are keys and
//! tokens, and the tests beside this file stay under its eye.
//!
//! Each was checked against an independent implementation (Python's `hmac`
//! and `cryptography`) before it was pasted here, so a failing test means the
//! provider is wrong, not the vector.

/// One worked example: the two halves of the signing input, and the signature
/// the RFC says they produce.
pub(super) struct Example {
    /// `BASE64URL(UTF8(JWS Protected Header))`
    pub header: &'static str,
    /// `BASE64URL(JWS Payload)`
    pub payload: &'static str,
    /// `BASE64URL(JWS Signature)`
    pub signature: &'static str,
}

impl Example {
    pub fn signing_input(&self) -> String {
        format!("{}.{}", self.header, self.payload)
    }

    pub fn token(&self) -> String {
        format!("{}.{}", self.signing_input(), self.signature)
    }
}

/// An RSA private key as its JWK members (RFC 7518 section 6.3).
pub(super) struct RsaJwk {
    pub n: &'static str,
    pub e: &'static str,
    pub d: &'static str,
    pub p: &'static str,
    pub q: &'static str,
    pub dp: &'static str,
    pub dq: &'static str,
    pub qi: &'static str,
}

/// The payload the three RFC 7515 examples share:
/// `{"iss":"joe",\r\n "exp":1300819380,\r\n "http://example.com/is_root":true}`.
const RFC_7515_PAYLOAD: &str = "eyJpc3MiOiJqb2UiLA0KICJleHAiOjEzMDA4MTkzODAsDQogImh0dHA6Ly9leGFt\
    cGxlLmNvbS9pc19yb290Ijp0cnVlfQ";

// --- RFC 7515 A.1: HMAC SHA-256 -------------------------------------------

/// The `k` member of the A.1 JWK: 64 octets.
pub(super) const HS256_K: &str = "AyM1SysPpbyDfgZld3umj1qzKObwVMkoqQ-\
    EstJQLr_T-1qS0gZH75aKtMN3Yj0iPS4hcgUuTwjAzZr1Z9CAow";

/// Header `{"typ":"JWT",\r\n "alg":"HS256"}`.
pub(super) const HS256: Example = Example {
    header: "eyJ0eXAiOiJKV1QiLA0KICJhbGciOiJIUzI1NiJ9",
    payload: RFC_7515_PAYLOAD,
    signature: "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
};

// --- RFC 7515 A.2: RSASSA-PKCS1-v1_5 SHA-256 ------------------------------

/// The A.2 key: 2048 bits, public exponent 65537.
pub(super) const RS256_JWK: RsaJwk = RsaJwk {
    n: "ofgWCuLjybRlzo0tZWJjNiuSfb4p4fAkd_wWJcyQoTbji9k0l8W26mPddxHmfHQp\
        -Vaw-4qPCJrcS2mJPMEzP1Pt0Bm4d4QlL-yRT-SFd2lZS-\
        pCgNMsD1W_YpRPEwOWvG6b32690r2jZ47soMZo9wGzjb_7OMg0LOL-\
        bSf63kpaSHSXndS5z5rexMdbBYUsLA9e-KXBdQOS-UTo7WTBEMa2R2CapHg665xs\
        mtdVMTBQY4uDZlxvb3qCo5ZwKh9kG4LT6_I5IhlJH7aGhyxXFvUK-\
        DWNmoudF8NAco9_h9iaGNj8q2ethFkMLs91kzk2PAcDTW9gb54h4FRWyuXpoQ",
    e: "AQAB",
    d: "Eq5xpGnNCivDflJsRQBXHx1hdR1k6Ulwe2JZD50LpXyWPEAeP88vLNO97IjlA7_G\
        Q5sLKMgvfTeXZx9SE-7YwVol2NXOoAJe46sui395IW_GO-\
        pWJ1O0BkTGoVEn2bKVRUCgu-GjBVaYLU6f3l9kJfFNS3E0QbVdxzubSu3Mkqzjkn\
        439X0M_V51gfpRLI9JYanrC4D4qAdGcopV_0ZHHzQlBjudU2QvXt4ehNYTCBr6XC\
        LQUShb1juUO1ZdiYoFaFQT5Tw8bGUl_x_jTj3ccPDVZFD9pIuhLhBOneufuBiB4c\
        S98l2SR_RQyGWSeWjnczT0QU91p1DhOVRuOopznQ",
    p: "4BzEEOtIpmVdVEZNCqS7baC4crd0pqnRH_5IB3jw3bcxGn6QLvnEtfdUdiYrqBds\
        s1l58BQ3KhooKeQTa9AB0Hw_Py5PJdTJNPY8cQn7ouZ2KKDcmnPGBY5t7yLc1QlQ\
        5xHdwW1VhvKn-nXqhJTBgIPgtldC-KDV5z-y2XDwGUc",
    q: "uQPEfgmVtjL0Uyyx88GZFF1fOunH3-\
        7cepKmtH4pxhtCoHqpWmT8YAmZxaewHgHAjLYsp1ZSe7zFYHj7C6ul7TjeLQeZD_\
        YwD66t62wDmpe_HlB-TnBA-\
        njbglfIsRLtXlnDzQkv5dTltRJ11BKBBypeeF6689rjcJIDEz9RWdc",
    dp: "BwKfV3Akq5_MFZDFZCnW-wzl-\
        CCo83WoZvnLQwCTeDv8uzluRSnm71I3QCLdhrqE2e9YkxvuxdBfpT_PI7Yz-FOKn\
        u1R6HsJeDCjn12Sk3vmAktV2zb34MCdy7cpdTh_YVr7tss2u6vneTwrA86rZtu5M\
        br1C1XsmvkxHQAdYo0",
    dq: "h_96-\
        mK1R_7glhsum81dZxjTnYynPbZpHziZjeeHcXYsXaaMwkOlODsWa7I9xXDoRwbKg\
        B719rrmI2oKr6N3Do9U0ajaHF-NKJnwgjMd2w9cjz3_-\
        kyNlxAr2v4IKhGNpmM5iIgOS1VZnOZ68m6_pbLBSp3nssTdlqvd0tIiTHU",
    qi: "IYd7DHOhrWvxkwPQsRM2tOgrjbcrfvtQJipd-DlcxyVuuM9sQLdgjVk2oy26F0Em\
        pScGLq2MowX7fhd_QJQ3ydy5cY7YIBi87w93IKLEdfnbJtoOPLUW0ITrJReOgo1c\
        q9SbsxYawBgfp_gh6A5603k2-ZQwVK0JKSHuLFkuQ3U",
};

/// Header `{"alg":"RS256"}`. PKCS#1 v1.5 is deterministic, so signing must
/// reproduce this exactly.
pub(super) const RS256: Example = Example {
    header: "eyJhbGciOiJSUzI1NiJ9",
    payload: RFC_7515_PAYLOAD,
    signature: "cC4hiUPoj9Eetdgtv3hF80EGrhuB__dzERat0XF9g2VtQgr9PJbu3XOiZj5RZmh7\
                AAuHIm4Bh-\
                0Qc_lF5YKt_O8W2Fp5jujGbds9uJdbF9CUAr7t1dnZcAcQjbKBYNX4BAynRFdiuB\
                --f_nZLgrnbyTyWzO75vRK5h6xBArLIARNPvkSjtQBMHlb1L07Qe7K0GarZRmB_e\
                SN9383LcOLn6_dO--xi12jzDwusC-eOkHWEsqtFZESc6BfI7noOPqvhJ1phCnvWh\
                6IeYI2w9QOYEUipUTI8np6LbgGY9Fs98rqVt5AXLIhWkWywlVmtVrBp0igcN_Ioy\
                pGlUPQGe77Rw",
};

// --- RFC 7515 A.3: ECDSA P-256 SHA-256 ------------------------------------

/// The public half of the A.3 key.
pub(super) const ES256_X: &str = "f83OJ3D2xF1Bg8vub9tLe1gHMzV76e8Tus9uPHvRVEU";
pub(super) const ES256_Y: &str = "x_FEzRu9m36HLN_tue659LNpXW6pCyStikYjKIWI5a0";

/// Header `{"alg":"ES256"}`. ECDSA draws a fresh nonce per signature, so this
/// one can be verified but never reproduced.
pub(super) const ES256: Example = Example {
    header: "eyJhbGciOiJFUzI1NiJ9",
    payload: RFC_7515_PAYLOAD,
    signature: "DtEhU3ljbEg8L38VWAfUAqOyKAM6-Xx-\
                F4GawxaepmXFCgfTjDxw5djxLa8ISlSApmWQxfKTUJqPP3-Kg6NU1Q",
};

// --- RFC 8037 A.4: Ed25519 ------------------------------------------------

/// The A.1 private key (`d`, the 32-octet seed) and A.2 public key (`x`).
pub(super) const ED25519_D: &str = "nWGxne_9WmC6hEr0kuwsxERJxWl7MmkZcDusAxyuf2A";
pub(super) const ED25519_X: &str = "11qYAYKxCrfVS_7TyWQHOg7hcvPapiMlrwIaaPcHURo";

/// Header `{"alg":"EdDSA"}`; the payload is the text `Example of Ed25519
/// signing`, which is not JSON. Ed25519 is deterministic.
pub(super) const EDDSA: Example = Example {
    header: "eyJhbGciOiJFZERTQSJ9",
    payload: "RXhhbXBsZSBvZiBFZDI1NTE5IHNpZ25pbmc",
    signature: "hgyY0il_MGCjP0JzlnLWG1PPOt7-\
                09PGcvMg3AIbQR6dWbhijcNR4ki4iylGjg5BhVsPt9g7sVvpAr_MuM0KAg",
};
