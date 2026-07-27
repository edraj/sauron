//! Back-compatibility for the pre-language wire format: repeated
//! `filter=field:op:value` plus a single `q=`.
//!
//! Every shared URL and bookmark in the wild uses this, and `wiki/Search.md`
//! documents it. Rather than keeping a second execution path alive, this
//! translates it into the same `Node` tree the new grammar produces.

use crate::ast::{Node, Predicate, MAX_TERMS};
use crate::token::is_field_ident;
use crate::QueryError;

/// Wrap a legacy value so the resolver treats it literally. `contains` and the
/// ordering operators produce syntax that MUST stay interpretable, so they set
/// `quoted: false`; everything else is quoted to prevent a stray `*` or `>` in
/// user data from changing meaning on the way through.
fn pred(field: String, value: String, quoted: bool, at: usize) -> Node {
    Node::Pred(Predicate {
        field,
        value,
        quoted,
        at,
    })
}

pub fn from_legacy(filters: &[String], q: Option<&str>) -> Result<Node, QueryError> {
    // Both entry points feed the same planner, so they must share the bound that
    // `parse()` already enforces.
    if filters.len() > MAX_TERMS {
        return Err(QueryError::TooManyTerms { max: MAX_TERMS });
    }

    let mut parts: Vec<Node> = Vec::with_capacity(filters.len() + 1);

    for (i, item) in filters.iter().enumerate() {
        let mut bits = item.splitn(3, ':');
        let field = bits.next().unwrap_or("");
        let op = bits.next().ok_or(QueryError::BadValue {
            field: field.to_string(),
            value: item.clone(),
            at: i,
        })?;
        let raw = bits.next().ok_or(QueryError::BadValue {
            field: field.to_string(),
            value: item.clone(),
            at: i,
        })?;

        // The frontend applies `encodeURIComponent` before putting the value in
        // the query param, and the transport layer only reverses its own
        // encoding — so decode once more here. Pure percent-decoding, not
        // form-urlencoded, so a literal `+` survives (mirrors decodeURIComponent).
        let value = percent_encoding::percent_decode_str(raw)
            .decode_utf8_lossy()
            .into_owned();

        // `tag` carried its key inside the value as `k=v`. The new grammar keeps
        // addressing an identifier key as a dotted field (`tag.<key>`). Tag keys
        // are entirely unconstrained on the write path though, so a key that is
        // NOT an identifier goes through the `tag:<key>=<value>` escape hatch
        // instead — field stays `tag`, and `key_prefix` records `k` so it can be
        // spliced back into the final value below, AFTER the operator prefix
        // (computed once, further down) is applied to `v`. Splicing it in first
        // would turn `contains` into a key of `~k`, which is not what the
        // operator means.
        //
        // A key containing whitespace is rejected even though it is not an
        // identifier: the lexer breaks a word at whitespace, so `k=v` would need
        // quoting to render back out — but quoting also erases the
        // `Predicate.quoted = false` flag that `contains`/`gt`/`lt` rely on to
        // keep their operator prefix meaningful, corrupting the query on a
        // render round-trip. Keys with other non-identifier characters
        // (`cart@checkout`, `100%off`, …) have no such conflict: they never need
        // quoting, so they survive the round trip unchanged.
        let (field, value, key_prefix) = if field == "tag" {
            match value.split_once('=') {
                Some((k, v)) if !k.is_empty() && !v.is_empty() => {
                    if is_field_ident(k) {
                        (format!("tag.{k}"), v.to_string(), None)
                    } else if k.chars().any(|c| c.is_whitespace()) {
                        return Err(QueryError::BadValue {
                            field: "tag".into(),
                            value: value.clone(),
                            at: i,
                        });
                    } else {
                        ("tag".to_string(), v.to_string(), Some(k.to_string()))
                    }
                }
                _ => {
                    return Err(QueryError::BadValue {
                        field: "tag".into(),
                        value,
                        at: i,
                    })
                }
            }
        } else {
            (field.to_string(), value, None)
        };

        if !is_field_ident(&field) {
            return Err(QueryError::UnknownField {
                field: field.clone(),
                at: i,
            });
        }

        // `contains`/`gt`/`lt` produce syntax the resolver must still interpret,
        // so they stay unquoted; `eq`/`neq` are always literal. Computed once, on
        // the bare value, so the escape hatch's `key_prefix` (if any) can be
        // spliced in afterwards without the operator prefix landing on the key.
        let (transformed, quoted, negate) = match op {
            "eq" => (value, true, false),
            "neq" => (value, true, true),
            // `~` (literal substring), NOT `*{value}*`. The old `contains` never
            // treated `*` as a wildcard, so wrapping in stars would silently turn
            // a user's literal `*` into one and change what an existing shared
            // URL returns.
            "contains" => (format!("~{value}"), false, false),
            "gt" => (format!(">{value}"), false, false),
            "lt" => (format!("<{value}"), false, false),
            other => {
                return Err(QueryError::BadOp {
                    field: other.to_string(),
                    at: i,
                })
            }
        };

        let final_value = match key_prefix {
            Some(k) => format!("{k}={transformed}"),
            None => transformed,
        };

        let node = pred(field, final_value, quoted, i);
        parts.push(if negate {
            Node::Not(Box::new(node))
        } else {
            node
        });
    }

    if let Some(text) = q {
        let text = text.trim();
        if !text.is_empty() {
            parts.push(Node::Text(text.to_string()));
        }
    }

    Ok(if parts.len() == 1 {
        parts.remove(0)
    } else {
        Node::And(parts)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Node, Predicate};

    fn f(s: &str) -> Vec<String> {
        vec![s.to_string()]
    }

    fn only(n: Node) -> Predicate {
        match n {
            Node::Pred(p) => p,
            other => panic!("expected one predicate, got {other:?}"),
        }
    }

    #[test]
    fn maps_eq() {
        let p = only(from_legacy(&f("level:eq:error"), None).unwrap());
        assert_eq!(p.field, "level");
        assert_eq!(p.value, "error");
        assert!(p.quoted, "legacy values are literal, never re-interpreted");
    }

    #[test]
    fn maps_contains_to_a_literal_substring() {
        let p = only(from_legacy(&f("culprit:contains:handler"), None).unwrap());
        assert_eq!(p.value, "~handler");
        assert!(!p.quoted, "the operator prefix must stay interpretable");
    }

    #[test]
    fn contains_preserves_a_literal_star_in_the_value() {
        // The pre-language `contains` never treated `*` as a wildcard. Mapping to
        // `*{value}*` would have silently changed what this shared URL returns.
        let p = only(from_legacy(&f("culprit:contains:foo*bar"), None).unwrap());
        assert_eq!(p.value, "~foo*bar");
    }

    #[test]
    fn contains_preserves_a_leading_operator_character() {
        // `~` is stripped once and everything after it is literal.
        let p = only(from_legacy(&f("culprit:contains:>=v2"), None).unwrap());
        assert_eq!(p.value, "~>=v2");
    }

    #[test]
    fn maps_gt_and_lt() {
        assert_eq!(
            only(from_legacy(&f("times_seen:gt:100"), None).unwrap()).value,
            ">100"
        );
        assert_eq!(
            only(from_legacy(&f("times_seen:lt:5"), None).unwrap()).value,
            "<5"
        );
    }

    #[test]
    fn maps_neq_to_a_negation() {
        match from_legacy(&f("level:neq:error"), None).unwrap() {
            Node::Not(inner) => assert_eq!(only(*inner).value, "error"),
            other => panic!("expected Not, got {other:?}"),
        }
    }

    #[test]
    fn maps_tag_to_a_dotted_field() {
        let p = only(from_legacy(&f("tag:eq:region=eu"), None).unwrap());
        assert_eq!(p.field, "tag.region");
        assert_eq!(p.value, "eu");
    }

    #[test]
    fn tag_value_keeps_extra_equals() {
        // Only the FIRST '=' splits key from value, matching filter.rs today.
        let p = only(from_legacy(&f("tag:eq:expr=a=b"), None).unwrap());
        assert_eq!(p.field, "tag.expr");
        assert_eq!(p.value, "a=b");
    }

    #[test]
    fn tag_contains_becomes_a_literal_substring() {
        let p = only(from_legacy(&f("tag:contains:region=e"), None).unwrap());
        assert_eq!(p.field, "tag.region");
        assert_eq!(p.value, "~e");
    }

    #[test]
    fn percent_decodes_values() {
        let p = only(from_legacy(&f("distinct_id:eq:user%40example.com"), None).unwrap());
        assert_eq!(p.value, "user@example.com");
    }

    #[test]
    fn value_may_contain_colons() {
        let p = only(from_legacy(&f("culprit:contains:foo:bar"), None).unwrap());
        assert_eq!(p.value, "~foo:bar");
    }

    #[test]
    fn free_text_becomes_a_text_node() {
        match from_legacy(&[], Some("timeout")).unwrap() {
            Node::Text(t) => assert_eq!(t, "timeout"),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn combines_filters_and_free_text_with_and() {
        let got = from_legacy(&f("level:eq:error"), Some("timeout")).unwrap();
        match got {
            Node::And(parts) => {
                assert_eq!(parts.len(), 2);
                assert!(matches!(parts[1], Node::Text(_)));
            }
            other => panic!("expected And, got {other:?}"),
        }
    }

    #[test]
    fn empty_input_is_an_empty_and() {
        assert_eq!(from_legacy(&[], None).unwrap(), Node::And(vec![]));
        assert_eq!(from_legacy(&[], Some("  ")).unwrap(), Node::And(vec![]));
    }

    #[test]
    fn rejects_malformed_filter() {
        assert!(from_legacy(&f("level=error"), None).is_err());
    }

    #[test]
    fn rejects_unknown_operator() {
        assert!(from_legacy(&f("level:between:1"), None).is_err());
    }

    #[test]
    fn rejects_tag_without_a_key_or_value() {
        assert!(from_legacy(&f("tag:eq:region"), None).is_err());
        assert!(from_legacy(&f("tag:eq:=eu"), None).is_err());
        assert!(from_legacy(&f("tag:eq:region="), None).is_err());
    }

    #[test]
    fn rejects_a_field_name_that_is_not_an_identifier() {
        assert!(matches!(
            from_legacy(&f("my field:eq:x"), None),
            Err(QueryError::UnknownField { .. })
        ));
    }

    #[test]
    fn tag_key_with_whitespace_is_still_rejected() {
        // Most non-identifier keys are legal via the `tag:<key>=<value>` escape
        // hatch now (see `non_identifier_tag_key_routes_through_the_escape_hatch`
        // below), but the lexer breaks a word at whitespace. A key containing a
        // space would need quoting to render back out, and quoting erases the
        // `quoted: false` flag that `contains`/`gt`/`lt` need to keep their
        // operator prefix meaningful — so a whitespace-containing key is still
        // rejected, unlike a key with other non-identifier characters.
        assert!(matches!(
            from_legacy(&f("tag:eq:my key=val"), None),
            Err(QueryError::BadValue { .. })
        ));
    }

    #[test]
    fn non_identifier_tag_key_routes_through_the_escape_hatch() {
        let p = only(from_legacy(&f("tag:eq:cart@checkout=eu"), None).unwrap());
        assert_eq!(p.field, "tag");
        assert_eq!(p.value, "cart@checkout=eu");
    }

    #[test]
    fn non_identifier_tag_key_puts_the_operator_after_the_equals() {
        // The op prefix must attach to the VALUE, not to the whole `k=v` string —
        // otherwise the key becomes `~cart@checkout`.
        let p = only(from_legacy(&f("tag:contains:cart@checkout=eu"), None).unwrap());
        assert_eq!(p.field, "tag");
        assert_eq!(p.value, "cart@checkout=~eu");
    }

    #[test]
    fn identifier_tag_key_still_uses_the_dotted_form() {
        let p = only(from_legacy(&f("tag:eq:region=eu"), None).unwrap());
        assert_eq!(p.field, "tag.region");
        assert_eq!(p.value, "eu");
    }

    #[test]
    fn legacy_non_identifier_tag_key_resolves_end_to_end() {
        use crate::catalog::Resource;
        use crate::resolve::{resolve, ResolvedNode};
        let node = from_legacy(&f("tag:contains:cart@checkout=eu"), None).unwrap();
        match resolve(&node, Resource::Issues).unwrap() {
            ResolvedNode::Pred(p) => {
                assert_eq!(p.path.as_deref(), Some("cart@checkout"));
                assert_eq!(p.op, crate::ast::MatchOp::Contains);
            }
            other => panic!("expected a predicate, got {other:?}"),
        }
    }

    #[test]
    fn rejects_more_filters_than_the_parser_would_allow() {
        let many: Vec<String> = (0..MAX_TERMS + 1).map(|i| format!("f{i}:eq:1")).collect();
        assert!(matches!(
            from_legacy(&many, None),
            Err(QueryError::TooManyTerms { .. })
        ));
    }
}
