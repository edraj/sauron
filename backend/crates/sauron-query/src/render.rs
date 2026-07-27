//! `Node` → canonical query text.
//!
//! Canonical means: implicit `AND` (never the keyword), parentheses only where
//! precedence requires them, and quoting only where the value would otherwise
//! re-lex differently. `render(parse(x))` must be idempotent — S4's chip bar
//! round-trips through this on every edit.

use crate::ast::{Node, Predicate};

/// True when the value would survive re-lexing unquoted. Anything containing
/// whitespace, a quote, or a parenthesis must be quoted — the lexer breaks a
/// word at a `)` wherever it appears, and treats a leading `(` as structural.
fn needs_quoting(v: &str) -> bool {
    v.is_empty()
        || v.chars()
            .any(|c| c.is_whitespace() || c == '"' || c == '(' || c == ')')
}

/// Free text has more ways to be misread than a value does: a bare `or`/`and`
/// is a boolean keyword, a leading `!` is a negation, and anything shaped like
/// `field:value` re-lexes as a predicate. Quote all of them.
fn text_needs_quoting(t: &str) -> bool {
    needs_quoting(t)
        || t.contains(':')
        || t.starts_with('!')
        || t.eq_ignore_ascii_case("or")
        || t.eq_ignore_ascii_case("and")
}

fn quote(v: &str) -> String {
    let mut out = String::with_capacity(v.len() + 2);
    out.push('"');
    for ch in v.chars() {
        if ch == '"' || ch == '\\' {
            out.push('\\');
        }
        out.push(ch);
    }
    out.push('"');
    out
}

/// True when re-lexing this value unquoted would change its meaning — a `*`
/// would become a wildcard, a leading `>` a comparison, a leading `~` a literal
/// substring match, `[…]` a list.
fn would_reinterpret(v: &str) -> bool {
    v.contains('*')
        || v.starts_with('>')
        || v.starts_with('<')
        || v.starts_with('~')
        || (v.len() >= 2 && v.starts_with('[') && v.ends_with(']'))
}

fn render_value(p: &Predicate) -> String {
    // Quote when the value cannot survive re-lexing at all, or when it was
    // quoted on the way in AND dropping the quotes would change its meaning.
    // Quoting solely because the input happened to be quoted would be wrong:
    // it makes `render` non-canonical, so `level:"error"` and `level:error`
    // would round-trip to different text for the same query.
    if needs_quoting(&p.value) || (p.quoted && would_reinterpret(&p.value)) {
        quote(&p.value)
    } else {
        p.value.clone()
    }
}

/// `parent_is_and` drives parenthesisation: an `Or` nested inside an `And`
/// needs parens, a top-level `Or` does not.
fn go(node: &Node, parent_is_and: bool) -> String {
    match node {
        Node::Pred(p) => format!("{}:{}", p.field, render_value(p)),
        Node::Text(t) => {
            if text_needs_quoting(t) {
                quote(t)
            } else {
                t.clone()
            }
        }
        Node::Not(inner) => {
            let body = match **inner {
                // A negated group keeps its parens; a negated term does not.
                Node::And(_) | Node::Or(_) => format!("({})", go(inner, false)),
                _ => go(inner, true),
            };
            format!("!{body}")
        }
        Node::And(v) => v.iter().map(|n| go(n, true)).collect::<Vec<_>>().join(" "),
        Node::Or(v) => {
            let joined = v
                .iter()
                .map(|n| go(n, false))
                .collect::<Vec<_>>()
                .join(" OR ");
            if parent_is_and {
                format!("({joined})")
            } else {
                joined
            }
        }
    }
}

pub fn render(node: &Node) -> String {
    go(node, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::legacy::from_legacy;
    use crate::parse::parse;

    fn round(q: &str) -> String {
        render(&parse(q).unwrap())
    }

    #[test]
    fn renders_a_simple_predicate() {
        assert_eq!(round("level:error"), "level:error");
    }

    #[test]
    fn renders_implicit_and_without_the_keyword() {
        assert_eq!(
            round("level:error is:unresolved"),
            "level:error is:unresolved"
        );
    }

    #[test]
    fn normalises_explicit_and_away() {
        assert_eq!(
            round("level:error AND is:unresolved"),
            "level:error is:unresolved"
        );
    }

    #[test]
    fn renders_or_with_parens_inside_an_and() {
        assert_eq!(round("a:1 (b:2 OR c:3)"), "a:1 (b:2 OR c:3)");
    }

    #[test]
    fn does_not_parenthesise_a_top_level_or() {
        assert_eq!(round("a:1 OR b:2"), "a:1 OR b:2");
    }

    #[test]
    fn renders_negation() {
        assert_eq!(round("!level:error"), "!level:error");
        assert_eq!(round("!(a:1 OR b:2)"), "!(a:1 OR b:2)");
    }

    #[test]
    fn quotes_values_that_need_it() {
        assert_eq!(
            round(r#"message:"connection refused""#),
            r#"message:"connection refused""#
        );
    }

    #[test]
    fn quotes_values_containing_a_paren_or_quote() {
        assert_eq!(
            round(r#"culprit:"handle(req)""#),
            r#"culprit:"handle(req)""#
        );
        assert_eq!(round(r#"culprit:"say \"hi\"""#), r#"culprit:"say \"hi\"""#);
    }

    #[test]
    fn does_not_quote_values_that_do_not_need_it() {
        assert_eq!(round("user.email:*@acme.com"), "user.email:*@acme.com");
        assert_eq!(round("duration:>2s"), "duration:>2s");
    }

    #[test]
    fn output_is_canonical_not_merely_faithful() {
        // Redundant quotes are dropped, so two spellings of the same query
        // render identically. Without this, saved views would store two
        // different strings for one query and compare unequal.
        assert_eq!(round(r#"level:"error""#), "level:error");
        assert_eq!(round(r#"level:"error""#), round("level:error"));
    }

    #[test]
    fn keeps_quotes_that_carry_meaning() {
        // Here the quotes are load-bearing: without them the `*` is a wildcard.
        assert_eq!(round(r#"culprit:"a*b""#), r#"culprit:"a*b""#);
        assert_eq!(round(r#"culprit:">100""#), r#"culprit:">100""#);
    }

    #[test]
    fn renders_free_text() {
        assert_eq!(round("level:error timeout"), "level:error timeout");
        assert_eq!(round(r#""connection refused""#), r#""connection refused""#);
    }

    #[test]
    fn empty_tree_renders_empty() {
        assert_eq!(round(""), "");
    }

    #[test]
    fn render_is_idempotent() {
        // The property S4's chip bar depends on: text → chips → text is stable.
        for q in [
            "level:error is:unresolved",
            "a:1 (b:2 OR c:3) !d:4",
            r#"message:"connection refused" timeout"#,
            "user.email:*@acme.com duration:>2s",
            "has:extra.cartValue",
            "culprit:~foo*bar",
        ] {
            let once = round(q);
            let twice = render(&parse(&once).unwrap());
            assert_eq!(once, twice, "not idempotent for {q}");
        }
    }

    #[test]
    fn upgrades_a_legacy_url_to_query_syntax() {
        let node = from_legacy(
            &[
                "level:eq:error".to_string(),
                "tag:contains:region=eu".to_string(),
            ],
            Some("timeout"),
        )
        .unwrap();
        assert_eq!(render(&node), "level:error tag.region:~eu timeout");
    }

    #[test]
    fn renders_the_literal_substring_operator() {
        assert_eq!(round("culprit:~foo*bar"), "culprit:~foo*bar");
    }

    #[test]
    fn quotes_a_value_that_would_be_read_as_an_operator() {
        // A value genuinely starting with `~` must survive a round trip.
        assert_eq!(round(r#"culprit:"~literal""#), r#"culprit:"~literal""#);
    }

    #[test]
    fn quotes_free_text_that_would_relex_as_a_keyword() {
        assert_eq!(round(r#""OR""#), r#""OR""#);
        assert_eq!(round(r#""and""#), r#""and""#);
    }

    #[test]
    fn quotes_free_text_shaped_like_a_predicate() {
        assert_eq!(round(r#""level:error""#), r#""level:error""#);
    }

    #[test]
    fn quotes_free_text_starting_with_a_bang() {
        assert_eq!(round(r#""!important""#), r#""!important""#);
    }

    #[test]
    fn round_trips_adversarial_free_text() {
        // Every one of these must survive TWO passes — the second pass is where
        // a lost quote turns into a parse error or a changed tree.
        for q in [
            r#""OR""#,
            r#""AND""#,
            r#""level:error""#,
            r#""!important""#,
            r#"level:"""#,
        ] {
            let once = round(q);
            let twice = render(&parse(&once).unwrap());
            assert_eq!(once, twice, "not idempotent for {q}");
            assert_eq!(once, q, "not canonical for {q}");
        }
    }

    #[test]
    fn round_trips_an_interior_paren() {
        // The lexer breaks a word at `)` wherever it appears, so quoting only a
        // TRAILING paren left this unparseable on the second pass.
        assert_eq!(
            round(r#"culprit:"handle(req)x""#),
            r#"culprit:"handle(req)x""#
        );
        assert_eq!(round(r#""a)b""#), r#""a)b""#);
    }

    /// Structural equality that ignores `Predicate.quoted`. Canonicalization is
    /// allowed to drop redundant quotes (see `output_is_canonical_not_merely_faithful`
    /// above), which flips a `Predicate`'s `quoted` flag from `true` to `false`
    /// without changing its field or value. Comparing the derived `PartialEq`
    /// directly (which includes `quoted`) would flag that harmless, intentional
    /// normalization as "tree changed" for the common case of a quoted value that
    /// never needed quoting — e.g. `culprit:"plain"` — even though nothing broke.
    fn same_shape(a: &Node, b: &Node) -> bool {
        match (a, b) {
            (Node::Pred(pa), Node::Pred(pb)) => pa.field == pb.field && pa.value == pb.value,
            (Node::Text(ta), Node::Text(tb)) => ta == tb,
            (Node::Not(a), Node::Not(b)) => same_shape(a, b),
            (Node::And(a), Node::And(b)) | (Node::Or(a), Node::Or(b)) => {
                a.len() == b.len() && a.iter().zip(b).all(|(x, y)| same_shape(x, y))
            }
            _ => false,
        }
    }

    #[test]
    fn parse_render_parse_is_stable_over_adversarial_values() {
        // The property S4's chip editor depends on. Hand-picked cases all happened
        // to sidestep the interior-paren break; this corpus does not.
        let values = [
            "plain", "a b", "a)b", "a(b", "(a)", "a\"b", "a\\b", "a*b", ">10", "~x", "[a,b]", "",
            "OR", "and", "a:b", "!x", "a%b", "a_b", "a~b",
        ];
        for v in values {
            for q in [format!("culprit:{}", quote(v)), quote(v).to_string()] {
                let once = render(&parse(&q).unwrap());
                let reparsed = parse(&once).unwrap_or_else(|e| {
                    panic!("`{q}` rendered to `{once}` which failed to parse: {e}")
                });
                let twice = render(&reparsed);
                assert_eq!(once, twice, "not idempotent for {q}");
                assert!(
                    same_shape(&parse(&q).unwrap(), &reparsed),
                    "tree changed for {q}"
                );
            }
        }
    }
}
