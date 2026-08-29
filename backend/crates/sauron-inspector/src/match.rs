//! Tracked-key matching.
//!
//! A tracked key is a literal NAME, lowercased at write, matched
//! case-insensitively and EXACTLY against a leaf's own key at any depth.
//! Admin-authored regex was rejected outright: it means accepting ReDoS
//! authored by an org admin against a shared worker, and `regex` is only a
//! transitive dependency today so declaring it is a workspace edit.
//!
//! Dotted paths are wrong as INPUT — the admin does not know the SDK nested
//! the field under `contexts.order` — and right as OUTPUT, which is what a
//! finding's `key_path` is.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::walk::Leaf;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum KeyScope {
    /// Any depth. The default: a policy whose entry omits `scope` must widen,
    /// never narrow — defaulting to `Top` would silently stop matching the
    /// nested payloads that are the whole reason this feature exists.
    #[default]
    Any,
    /// Only the top level of the column.
    Top,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct TrackedKey {
    pub key: String,
    #[serde(default)]
    pub scope: KeyScope,
}

/// Lowercase + trim. Applied at policy write AND at match time, so a row that
/// predates the normalization still matches.
pub fn normalize_key(raw: &str) -> String {
    raw.trim().to_lowercase()
}

/// True when the path names a key directly under the column root. An array
/// segment is never top level — `tags[]` is inside a container.
pub fn is_top_level(path: &str) -> bool {
    !path.contains('.') && !path.contains('[')
}

/// The tracked key this leaf satisfies, if any.
pub fn matched<'k>(keys: &'k [TrackedKey], leaf: &Leaf<'_>) -> Option<&'k TrackedKey> {
    keys.iter()
        .find(|k| k.key == leaf.key && (k.scope == KeyScope::Any || is_top_level(&leaf.path)))
}

/// Load a policy's `tracked_keys` jsonb.
///
/// Tolerant by design: a policy whose keys silently parse to an empty list
/// produces a scan that reads zero rows and finishes `succeeded`,
/// `coverage='full'`, zero findings. A confident false negative on a privacy
/// scan is the worst thing this feature can emit, so malformed ENTRIES are
/// dropped individually rather than failing the whole list.
pub fn parse_tracked_keys(v: &Value) -> Vec<TrackedKey> {
    let Some(arr) = v.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let (raw, scope) = match item {
            Value::String(s) => (s.as_str(), KeyScope::Any),
            Value::Object(o) => {
                let Some(Value::String(s)) = o.get("key") else {
                    continue;
                };
                let scope = match o.get("scope").and_then(|s| s.as_str()) {
                    Some("top") => KeyScope::Top,
                    _ => KeyScope::Any,
                };
                (s.as_str(), scope)
            }
            _ => continue,
        };
        let key = normalize_key(raw);
        if key.is_empty() {
            continue;
        }
        out.push(TrackedKey { key, scope });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::walk::walk;
    use serde_json::json;

    fn keys(spec: &[(&str, KeyScope)]) -> Vec<TrackedKey> {
        spec.iter()
            .map(|(k, s)| TrackedKey {
                key: normalize_key(k),
                scope: *s,
            })
            .collect()
    }

    #[test]
    fn matching_is_case_insensitive() {
        let ks = keys(&[("email", KeyScope::Any)]);
        for doc in [
            json!({"Email": 1}),
            json!({"EMAIL": 1}),
            json!({"email": 1}),
        ] {
            let ls = walk(&doc);
            assert!(matched(&ks, &ls[0]).is_some(), "{doc} should match");
        }
    }

    /// Exact, not substring. Substring matching over ~15 keys per row across
    /// millions of rows is a cross product that produces findings nobody
    /// asked for, and it would force a per-key OR instead of one bound text[].
    #[test]
    fn matching_is_exact_not_substring() {
        let ks = keys(&[("email", KeyScope::Any)]);
        for doc in [
            json!({"user_email": 1}),
            json!({"emails": 1}),
            json!({"e-mail": 1}),
        ] {
            let ls = walk(&doc);
            assert!(matched(&ks, &ls[0]).is_none(), "{doc} must not match");
        }
    }

    #[test]
    fn any_scope_matches_at_depth() {
        let ks = keys(&[("email", KeyScope::Any)]);
        let doc = json!({"order": {"customer": {"email": "a@b.c"}}});
        let ls = walk(&doc);
        assert!(ls.iter().any(|l| matched(&ks, l).is_some()));
    }

    #[test]
    fn top_scope_matches_only_the_first_level() {
        let ks = keys(&[("email", KeyScope::Top)]);
        // `Leaf` borrows the document, so the value must outlive the leaves:
        // `walk(&json!(..))` alone drops the temporary at the end of the
        // statement and does not compile (E0716).
        let nested_doc = json!({"order": {"email": "a@b.c"}});
        let nested = walk(&nested_doc);
        assert!(nested.iter().all(|l| matched(&ks, l).is_none()));
        let top_doc = json!({"email": "a@b.c"});
        let top = walk(&top_doc);
        assert!(matched(&ks, &top[0]).is_some());
    }

    /// An array segment is not the top level: `tags[]` is inside a container.
    #[test]
    fn an_array_segment_is_not_top_level() {
        assert!(is_top_level("email"));
        assert!(!is_top_level("order.email"));
        assert!(!is_top_level("tags[]"));
    }

    #[test]
    fn keys_containing_dots_spaces_and_equals_are_accepted() {
        let ks = keys(&[
            ("a.b", KeyScope::Any),
            ("c d", KeyScope::Any),
            ("e=f", KeyScope::Any),
        ]);
        for (doc, _) in [
            (json!({"A.B": 1}), 0),
            (json!({"C D": 1}), 0),
            (json!({"E=F": 1}), 0),
        ] {
            let ls = walk(&doc);
            assert!(matched(&ks, &ls[0]).is_some(), "{doc} should match");
        }
    }

    #[test]
    fn normalize_trims_and_lowercases() {
        assert_eq!(normalize_key("  Email \n"), "email");
    }

    #[test]
    fn parse_tolerates_a_bare_string_entry() {
        // Older policy rows and hand-written JSON use `["email"]`; the object
        // form is `[{"key":"email","scope":"top"}]`. Both must load, because a
        // policy that silently parses to zero keys scans nothing and reports
        // a confident false negative.
        let v = json!(["Email", {"key": "SSN", "scope": "top"}]);
        let ks = parse_tracked_keys(&v);
        assert_eq!(ks.len(), 2);
        assert_eq!(
            ks[0],
            TrackedKey {
                key: "email".into(),
                scope: KeyScope::Any
            }
        );
        assert_eq!(
            ks[1],
            TrackedKey {
                key: "ssn".into(),
                scope: KeyScope::Top
            }
        );
    }

    #[test]
    fn parse_drops_blank_and_non_string_entries() {
        let v = json!(["", "  ", 7, {"key": ""}, {"nope": 1}, "email"]);
        assert_eq!(
            parse_tracked_keys(&v),
            vec![TrackedKey {
                key: "email".into(),
                scope: KeyScope::Any
            }]
        );
    }
}
