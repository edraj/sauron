//! Depth-capped walk over one jsonb column's parsed value.
//!
//! Bounded on purpose: the accumulator downstream is keyed on
//! `(column, path, matched_key, detector)` and must stay small enough that a
//! worker's RSS is flat regardless of scan size. A depth cap plus array
//! collapse is what bounds path cardinality to roughly keys x columns.

use serde_json::Value;

/// Deeper than this and a payload is a data structure, not a field an admin
/// is going to reason about. Also the bound that keeps path cardinality flat.
pub const MAX_DEPTH: usize = 6;

/// One key encountered anywhere in the document.
#[derive(Debug, Clone, PartialEq)]
pub struct Leaf<'a> {
    /// Dot-joined path from the column root. Array elements collapse to a
    /// single `[]` segment appended to their parent key.
    pub path: String,
    /// The key's own name, LOWERCASED — matching is case-insensitive because
    /// SDK payloads mix `Email`, `EMAIL` and `email` freely.
    pub key: String,
    pub value: &'a Value,
}

/// Every key in `root`, at every depth up to [`MAX_DEPTH`].
///
/// A non-object root yields nothing rather than panicking: `contexts` is
/// sometimes the scalar string `"[Circular]"` in real live data.
pub fn walk(root: &Value) -> Vec<Leaf<'_>> {
    let mut out = Vec::new();
    descend(root, String::new(), 0, &mut out);
    out
}

fn descend<'a>(v: &'a Value, prefix: String, depth: usize, out: &mut Vec<Leaf<'a>>) {
    if depth >= MAX_DEPTH {
        return;
    }
    match v {
        Value::Object(map) => {
            for (k, child) in map {
                let path = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                out.push(Leaf {
                    path: path.clone(),
                    key: k.to_lowercase(),
                    value: child,
                });
                descend(child, path, depth + 1, out);
            }
        }
        Value::Array(items) => {
            // Every element shares one `[]` segment, and the element itself is
            // not a named key, so it produces no Leaf of its own — only its
            // children do.
            let path = format!("{prefix}[]");
            for child in items {
                match child {
                    Value::Object(_) | Value::Array(_) => {
                        descend(child, path.clone(), depth + 1, out)
                    }
                    // A scalar array element is still worth reporting under
                    // the collapsed path so `tags[]` full of emails is not
                    // invisible; its key is the parent's last segment.
                    _ => out.push(Leaf {
                        path: path.clone(),
                        key: last_segment(prefix.as_str()).to_lowercase(),
                        value: child,
                    }),
                }
            }
        }
        _ => {}
    }
}

fn last_segment(path: &str) -> &str {
    path.rsplit('.').next().unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Sorted DISTINCT paths. The walker deliberately emits one leaf per
    /// array ELEMENT — that is what makes `match_count` count occurrences
    /// rather than shapes — so an array of two objects yields the same path
    /// twice and only the distinct set is interesting to these assertions.
    fn paths(v: &serde_json::Value) -> Vec<String> {
        let mut p: Vec<String> = walk(v).into_iter().map(|l| l.path).collect();
        p.sort();
        p.dedup();
        p
    }

    #[test]
    fn one_level_tags() {
        assert_eq!(
            paths(&json!({"env": "prod", "email": "a@b.c"})),
            ["email", "env"]
        );
    }

    #[test]
    fn two_level_contexts() {
        // A key whose value is an OBJECT still yields a leaf: the matcher
        // matches leaf key names at any depth, and `contexts.order` is a real
        // finding if `order` is tracked.
        assert_eq!(
            paths(&json!({"order": {"id": 7, "email": "a@b.c"}})),
            ["order", "order.email", "order.id"]
        );
    }

    #[test]
    fn arbitrary_depth_extra() {
        assert_eq!(
            paths(&json!({"a": {"b": {"c": {"d": 1}}}})),
            ["a", "a.b", "a.b.c", "a.b.c.d"]
        );
    }

    /// Array elements collapse to a SINGLE `[]` segment. Per-index paths would
    /// make every row produce a different key_path, so the aggregate would be
    /// one finding per array position instead of one per shape.
    ///
    /// An OBJECT element is not itself a named key, so it yields no leaf of
    /// its own — only its children do. That is why `breadcrumbs[]` is absent
    /// here while `breadcrumbs[].data` appears once per element before dedup.
    #[test]
    fn breadcrumb_array_collapses_to_one_segment() {
        let v =
            json!({"breadcrumbs": [{"data": {"email": "a@b.c"}}, {"data": {"email": "d@e.f"}}]});
        assert_eq!(
            paths(&v),
            [
                "breadcrumbs",
                "breadcrumbs[].data",
                "breadcrumbs[].data.email"
            ]
        );
        // Two elements, two matches: the raw leaf list is NOT deduplicated,
        // because `match_count` counts occurrences.
        assert_eq!(
            walk(&v)
                .iter()
                .filter(|l| l.path == "breadcrumbs[].data.email")
                .count(),
            2
        );
    }

    #[test]
    fn depth_is_capped_at_six() {
        let v = json!({"a": {"b": {"c": {"d": {"e": {"f": {"g": 1}}}}}}});
        let deepest = paths(&v).into_iter().max_by_key(|p| p.len()).unwrap();
        assert_eq!(deepest, "a.b.c.d.e.f");
        assert_eq!(MAX_DEPTH, 6);
    }

    /// Real live data: a circular `contexts` block serializes as this scalar.
    /// A walker that assumes an object root panics or silently drops the row.
    #[test]
    fn tolerates_a_scalar_root() {
        assert!(walk(&json!("[Circular]")).is_empty());
        assert!(walk(&json!(null)).is_empty());
        assert!(walk(&json!(42)).is_empty());
        assert!(walk(&json!([1, 2, 3])).len() == 3);
    }

    #[test]
    fn empty_object_yields_nothing() {
        assert!(walk(&json!({})).is_empty());
    }

    /// Tag keys are unvalidated free-form UTF-8 by design (`tag:<key>=<value>`
    /// is the documented escape hatch), so the walker must not choke on
    /// separators it also uses in paths.
    #[test]
    fn keys_may_contain_dots_spaces_and_equals() {
        let v = json!({"a.b": {"c d": {"e=f": 1}}});
        let ls = walk(&v);
        assert!(ls.iter().any(|l| l.key == "e=f"));
        assert!(ls.iter().any(|l| l.path == "a.b.c d.e=f"));
    }

    #[test]
    fn key_is_lowercased_for_matching_but_path_is_not() {
        // `Leaf` borrows the document, so the value must outlive the leaves:
        // `walk(&json!(..))` alone drops the temporary at the end of the
        // statement and does not compile.
        let v = json!({"Email": 1});
        let ls = walk(&v);
        assert_eq!(ls[0].key, "email");
        assert_eq!(ls[0].path, "Email");
    }
}
