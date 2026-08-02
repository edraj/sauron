//! Phase-1 SQL prefilter construction.
//!
//! The scan is two phases: a cheap `column::text ILIKE ANY($patterns)` over an
//! index-bounded row window, then a `serde_json` walk in Rust over only the
//! rows that survive. Measured on this codebase, `extra::text ILIKE` over
//! 210,146 rows / 678 MB is 184 ms — about 0.9 us/row — and eliminates 95-99%
//! of rows before anything is parsed.
//!
//! DETECTION IS BEST-EFFORT, NOT A COMPLIANCE GUARANTEE. This greps the JSON
//! *text* for the quoted key name, so a key serialized with a unicode escape
//! (`"email"`) evades it, as does anything inside a base64 or URL-encoded
//! blob. That is the right tool for accidental PII, which is what it is for,
//! and useless against an adversary. The Findings tab says so non-dismissibly.

use crate::detect::Detector;
use crate::matching::TrackedKey;

/// Escape Postgres LIKE/ILIKE wildcards so a key matches literally.
///
/// Re-implemented rather than imported: `repo::escape_like` is private and
/// this crate has no diesel dependency on purpose. Postgres' default
/// LIKE/ILIKE escape character is `\`, and exactly three characters need it.
pub fn escape_like(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for c in v.chars() {
        if matches!(c, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

pub fn like_contains(v: &str) -> String {
    format!("%{}%", escape_like(v))
}

/// One `%"key"%` pattern per tracked key, for a single bound `text[]`.
///
/// The quotes are load-bearing: the value is matched against the column's
/// serialized JSON, where an object key always appears as `"name":`. Dropping
/// them turns tracking `id` into a substring match against every UUID in the
/// row and the prefilter stops eliminating anything.
pub fn key_patterns(keys: &[TrackedKey]) -> Vec<String> {
    keys.iter()
        .map(|k| like_contains(&format!("\"{}\"", k.key)))
        .collect()
}

/// The same list WITHOUT the quotes, for `ColumnKind::Text` columns.
///
/// A TEXT column is not JSON: `error_events.title` is `Error: jane@acme.com`,
/// with no `"email":` anywhere in it. Applying the quoted pattern to it
/// matches nothing, so a policy tracking `email` would report zero findings
/// with `coverage='full'` for exactly the ten `default_on` TEXT columns the
/// Issues list renders. The trade is honest and stated in the UI: an unquoted
/// substring over free text is noisier than a key-name grep, which is why
/// phase 2 still has to agree before a finding is written.
pub fn text_key_patterns(keys: &[TrackedKey]) -> Vec<String> {
    keys.iter().map(|k| like_contains(&k.key)).collect()
}

/// Whether phase 1 applies an ILIKE predicate at all.
///
/// False when any detector is enabled: a detector looks at VALUES, and no key
/// name predicate can pre-select rows for it. Building a pattern list from the
/// key list alone and applying it anyway is how a detector-only policy scans
/// zero rows and reports zero findings with `coverage='full'`.
pub fn use_prefilter(keys: &[TrackedKey], detectors: &[Detector]) -> bool {
    let _ = keys;
    detectors.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::Detector;
    use crate::matching::{KeyScope, TrackedKey};

    fn k(name: &str) -> TrackedKey {
        TrackedKey {
            key: name.into(),
            scope: KeyScope::Any,
        }
    }

    #[test]
    fn escapes_the_three_like_metacharacters() {
        assert_eq!(escape_like("50%"), "50\\%");
        assert_eq!(escape_like("a_b"), "a\\_b");
        assert_eq!(escape_like("c\\d"), "c\\\\d");
    }

    /// A double quote is NOT a LIKE metacharacter and must survive verbatim —
    /// the whole pattern is `%"key"%`, so escaping it would match nothing.
    #[test]
    fn a_double_quote_is_untouched() {
        assert_eq!(escape_like("say\"hi"), "say\"hi");
    }

    #[test]
    fn like_contains_wraps_in_percent() {
        assert_eq!(like_contains("50%"), "%50\\%%");
    }

    /// The pattern greps the JSON TEXT for the QUOTED key name. Without the
    /// quotes, tracking `id` matches every row that contains the letters "id"
    /// anywhere — including inside a UUID — and the prefilter eliminates
    /// nothing, which is the entire cost model.
    #[test]
    fn patterns_quote_the_key() {
        assert_eq!(key_patterns(&[k("email")]), vec!["%\"email\"%".to_string()]);
    }

    #[test]
    fn patterns_escape_metacharacters_inside_the_key() {
        assert_eq!(
            key_patterns(&[k("a%b_c")]),
            vec!["%\"a\\%b\\_c\"%".to_string()]
        );
    }

    /// A TEXT column is not JSON, so there are no quotes to grep for. Applying
    /// the quoted pattern to `error_events.title` matches NOTHING — which is
    /// how ten `default_on` TEXT columns, the ones the Issues list renders,
    /// come to report zero findings with `coverage='full'`.
    #[test]
    fn a_text_column_pattern_is_unquoted() {
        assert_eq!(
            text_key_patterns(&[k("email")]),
            vec!["%email%".to_string()]
        );
        assert_eq!(text_key_patterns(&[k("a%b")]), vec!["%a\\%b%".to_string()]);
    }

    /// When detectors are on, the prefilter is omitted ENTIRELY and every row
    /// in the (shorter) detector window is walked. That is what makes a
    /// detector-only policy work at all — otherwise a policy with no tracked
    /// keys builds an empty pattern list, matches zero rows, and finishes
    /// `succeeded` / `coverage='full'` / zero findings. A confident false
    /// negative on a privacy scan is the worst thing this feature can emit.
    #[test]
    fn detectors_disable_the_prefilter() {
        assert!(!use_prefilter(&[], &[Detector::Email]));
        assert!(!use_prefilter(&[k("email")], &[Detector::Email]));
        assert!(use_prefilter(&[k("email")], &[]));
    }

    /// No keys and no detectors is rejected at the API with a 400; if one ever
    /// reaches the worker it must scan nothing rather than everything.
    #[test]
    fn an_empty_policy_still_uses_the_prefilter() {
        assert!(use_prefilter(&[], &[]));
        assert!(key_patterns(&[]).is_empty());
    }
}
