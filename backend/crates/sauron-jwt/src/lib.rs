//! The workspace's one door to `jsonwebtoken`.
//!
//! `jsonwebtoken` 10 does no cryptography itself. It offers two backends behind
//! features, and this workspace can use neither:
//!
//! * **`rust_crypto`** — what was here before — depends on the `rsa` crate,
//!   which carries RUSTSEC-2023-0071 (the Marvin timing side channel on
//!   private-key operations). There is no fixed release to move to, and it was
//!   not an unused edge of the graph: `sauron-store` signs the Google
//!   service-account assertion with exactly that code.
//! * **`aws_lc_rs`** compiles AWS-LC, a C library. `packaging/rpm/sauron.spec`
//!   spells out that the build needs gcc and perl and nothing more, and that
//!   TLS uses ring *so that* aws-lc, cmake and clang stay out of it.
//!
//! `ring` is already in every binary — it is rustls's crypto provider — so
//! `provider` implements `jsonwebtoken`'s third option, a custom
//! `CryptoProvider`, on top of it. That adds no crate to the build and removes
//! the two dozen that `rust_crypto` brought in.
//!
//! # Use this crate, not `jsonwebtoken`
//!
//! A custom provider has to be installed before the first token is signed or
//! verified, and `jsonwebtoken` latches whatever it finds on that first call:
//! with no provider installed it latches a placeholder that panics, for the
//! life of the process. [`encode`] and [`decode`] install the provider first,
//! so going through them cannot get the order wrong. A crate that depends on
//! `jsonwebtoken` directly and calls it before anything here has run would —
//! which is why `sauron-auth` and `sauron-store` depend on this crate instead.
//!
//! Everything else is `jsonwebtoken`'s own types, re-exported unchanged.

mod provider;

use std::sync::Once;

use serde::de::DeserializeOwned;
use serde::Serialize;

pub use jsonwebtoken::errors;
pub use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, TokenData, Validation};

/// Install the ring-backed provider, once per process.
fn ensure_provider() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        if provider::RING_PROVIDER.install_default().is_err() {
            // Reachable only if something called `jsonwebtoken` directly before
            // this ran. The first provider wins and cannot be replaced, so say
            // so loudly instead of leaving a panic to be traced back to here.
            tracing::error!(
                "a jsonwebtoken crypto provider was installed before sauron-jwt's; \
                 tokens will be signed and verified by that one. Route every JWT \
                 call through sauron_jwt::{{encode, decode}}."
            );
        }
    });
}

/// Sign `claims` into a compact JWT. Same contract as `jsonwebtoken::encode`.
pub fn encode<T: Serialize>(
    header: &Header,
    claims: &T,
    key: &EncodingKey,
) -> errors::Result<String> {
    ensure_provider();
    jsonwebtoken::encode(header, claims, key)
}

/// Verify and decode a compact JWT. Same contract as `jsonwebtoken::decode`.
pub fn decode<T: DeserializeOwned>(
    token: impl AsRef<[u8]>,
    key: &DecodingKey,
    validation: &Validation,
) -> errors::Result<TokenData<T>> {
    ensure_provider();
    jsonwebtoken::decode(token, key, validation)
}

#[cfg(test)]
mod tests;
