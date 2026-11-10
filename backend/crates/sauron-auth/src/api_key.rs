//! Personal API key minting (MCP server). Only the SHA-256 of a key is stored.

use crate::hash_token;

/// Every key starts with this, so secret scanners can recognise a leaked one.
pub const API_KEY_PREFIX: &str = "sauron_pat_";

/// Characters of the token kept for display: the prefix plus 6 hex characters.
const DISPLAY_PREFIX_LEN: usize = API_KEY_PREFIX.len() + 6;

/// What `Debug` prints in place of a plaintext key.
pub const REDACTED_TOKEN: &str = "sauron_pat_…[redacted]";

/// A freshly minted key. `token` is shown to the user once and never stored.
///
/// `Debug` is hand-written so the plaintext token can never reach a log line
/// through `{:?}`; it prints [`REDACTED_TOKEN`] instead.
#[derive(Clone)]
pub struct MintedKey {
    pub token: String,
    pub prefix: String,
    pub hash: String,
}

impl std::fmt::Debug for MintedKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MintedKey")
            .field("token", &REDACTED_TOKEN)
            .field("prefix", &self.prefix)
            .field("hash", &self.hash)
            .finish()
    }
}

pub fn mint_api_key() -> MintedKey {
    let token = format!("{API_KEY_PREFIX}{}", sauron_core::ids::opaque_token());
    let prefix = token[..DISPLAY_PREFIX_LEN].to_string();
    let hash = hash_token(&token);
    MintedKey {
        token,
        prefix,
        hash,
    }
}

/// Cheap shape check used to route a bearer token to API-key auth instead of JWT.
pub fn looks_like_api_key(s: &str) -> bool {
    s.starts_with(API_KEY_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minted_key_has_prefix_and_hash_matches() {
        let k = mint_api_key();
        assert!(k.token.starts_with("sauron_pat_") && k.token.len() == 11 + 64);
        assert_eq!(k.prefix, &k.token[..17]);
        assert_eq!(k.hash, crate::hash_token(&k.token));
    }

    #[test]
    fn debug_never_prints_the_token() {
        let k = mint_api_key();
        let shown = format!("{k:?}");
        assert!(!shown.contains(&k.token), "{shown}");
        // Nor any of the secret part past the display prefix.
        assert!(!shown.contains(&k.token[k.prefix.len()..]), "{shown}");
        assert!(shown.contains(REDACTED_TOKEN), "{shown}");
        assert!(
            shown.contains(&k.prefix),
            "the prefix is not secret: {shown}"
        );
        assert!(!format!("{k:#?}").contains(&k.token));
    }

    #[test]
    fn looks_like_api_key_rejects_jwt() {
        assert!(!looks_like_api_key("eyJhbGciOiJIUzI1NiJ9.e30.x"));
        assert!(looks_like_api_key(&mint_api_key().token));
    }
}
