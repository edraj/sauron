//! Dart obfuscation maps — turning `xY1` back into `CartException`.
//!
//! **This is the one thing DWARF cannot do.** The Dart resolver in [`crate::dart`]
//! maps program-counter addresses to function names, which is enough to make a
//! stack trace readable. It says nothing about a *type* name, and the Flutter
//! SDK reports an exception's class as `error.runtimeType.toString()` — which
//! under `flutter build --obfuscate` is already the obfuscated identifier by
//! the time it reaches the wire. No amount of symbol data recovers it. The only
//! artifact that can is the map the same build emits with
//! `--extra-gen-snapshot-options=--save-obfuscation-map=<path>`.
//!
//! **Wire format.** A flat JSON array of strings in `[original, obfuscated]`
//! pairs — not an object, and not `[obfuscated, original]`:
//!
//! ```json
//! ["CartException","xY1","CheckoutBloc","aB2"]
//! ```
//!
//! We index it backwards (obfuscated → original), because that is the direction
//! every lookup runs.

use std::collections::HashMap;

use crate::content::SymbolError;

/// An obfuscated-name → original-name index for one build.
#[derive(Debug, Default)]
pub struct ObfuscationMap {
    names: HashMap<String, String>,
    /// Bytes this occupies, for the byte-bounded cache that holds it.
    weight: usize,
}

impl ObfuscationMap {
    /// Parse the JSON array Dart's `--save-obfuscation-map` emits.
    ///
    /// A trailing unpaired element is dropped rather than treated as an error:
    /// the pairs before it are still usable, and refusing the whole map would
    /// turn a truncated upload into "no de-obfuscation at all" instead of
    /// "de-obfuscation for everything that arrived".
    // clippy suggests `as_chunks`, which is Rust 1.88+; workspace MSRV is 1.82.
    // The lint itself is clippy 1.98+, so older toolchains need unknown_lints.
    #[allow(unknown_lints)]
    #[allow(clippy::chunks_exact_to_as_chunks)]
    pub fn parse(bytes: &[u8]) -> Result<Self, SymbolError> {
        let raw: Vec<String> = serde_json::from_slice(bytes)
            .map_err(|_| SymbolError::Corrupt("obfuscation map".to_string()))?;
        let mut names = HashMap::with_capacity(raw.len() / 2);
        let mut weight = 0usize;
        for pair in raw.chunks_exact(2) {
            let (original, obfuscated) = (&pair[0], &pair[1]);
            // An identity pair carries no information and is common in real
            // maps (names the obfuscator left alone). Storing it would double
            // the index for nothing.
            if original == obfuscated {
                continue;
            }
            weight += original.len() + obfuscated.len() + 2 * std::mem::size_of::<String>();
            names.insert(obfuscated.clone(), original.clone());
        }
        Ok(ObfuscationMap { names, weight })
    }

    /// The original name for `obfuscated`, or `None` when the map does not
    /// cover it.
    ///
    /// `None` rather than echoing the input, so a caller can tell "this build's
    /// map does not know this name" (leave the value alone, it may not be
    /// obfuscated at all) from "the map says it is called this".
    pub fn original(&self, obfuscated: &str) -> Option<&str> {
        self.names.get(obfuscated).map(String::as_str)
    }

    /// De-obfuscate a **dotted** name segment by segment, e.g. `xY1.aB2`.
    ///
    /// Dart type names reaching us are usually a bare class, but a generic or a
    /// library-qualified name arrives dotted, and only some segments will be in
    /// the map. Returns `None` when NO segment resolved — meaning the whole
    /// string is untouched and the caller should keep what it had.
    pub fn original_path(&self, name: &str) -> Option<String> {
        let mut hit = false;
        let out: Vec<&str> = name
            .split('.')
            .map(|seg| match self.original(seg) {
                Some(orig) => {
                    hit = true;
                    orig
                }
                None => seg,
            })
            .collect();
        hit.then(|| out.join("."))
    }

    pub fn len(&self) -> usize {
        self.names.len()
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    /// Approximate heap footprint, for [`crate::ByteLru`].
    pub fn weight(&self) -> usize {
        self.weight
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[&str]) -> ObfuscationMap {
        let json = serde_json::to_vec(&pairs.to_vec()).unwrap();
        ObfuscationMap::parse(&json).unwrap()
    }

    #[test]
    fn indexes_the_pairs_backwards() {
        // The file is [original, obfuscated]; every lookup runs the other way.
        let m = map(&["CartException", "xY1", "CheckoutBloc", "aB2"]);
        assert_eq!(m.original("xY1"), Some("CartException"));
        assert_eq!(m.original("aB2"), Some("CheckoutBloc"));
        // Not the other direction — that would "resolve" a name that is already
        // readable into gibberish.
        assert_eq!(m.original("CartException"), None);
    }

    #[test]
    fn an_unknown_name_is_none_not_an_echo() {
        // The caller uses this to decide whether to REPLACE a value. Echoing
        // the input would make "not in this map" indistinguishable from "the
        // map says it is already called that".
        let m = map(&["CartException", "xY1"]);
        assert_eq!(m.original("zZ9"), None);
    }

    #[test]
    fn identity_pairs_are_not_stored() {
        let m = map(&["StateError", "StateError", "CartException", "xY1"]);
        assert_eq!(m.len(), 1);
        assert_eq!(m.original("StateError"), None);
    }

    #[test]
    fn a_trailing_unpaired_name_does_not_void_the_map() {
        // A truncated upload should still de-obfuscate everything that arrived.
        let m = map(&["CartException", "xY1", "CheckoutBloc"]);
        assert_eq!(m.original("xY1"), Some("CartException"));
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn dotted_names_resolve_segment_by_segment() {
        let m = map(&["CheckoutBloc", "aB2", "CartException", "xY1"]);
        assert_eq!(
            m.original_path("aB2.xY1").as_deref(),
            Some("CheckoutBloc.CartException")
        );
        // A partial hit still resolves what it can.
        assert_eq!(
            m.original_path("aB2.unknown").as_deref(),
            Some("CheckoutBloc.unknown")
        );
    }

    #[test]
    fn a_name_with_no_resolvable_segment_is_none() {
        // Distinct from `Some(unchanged)`: the caller must be able to leave the
        // stored value alone rather than rewriting it to itself.
        let m = map(&["CartException", "xY1"]);
        assert_eq!(m.original_path("SomeOtherThing"), None);
        assert_eq!(m.original_path("a.b.c"), None);
    }

    #[test]
    fn a_non_array_payload_is_corrupt_not_a_panic() {
        assert!(ObfuscationMap::parse(b"{\"a\":\"b\"}").is_err());
        assert!(ObfuscationMap::parse(b"not json at all").is_err());
    }
}
