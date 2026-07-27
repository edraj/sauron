//! Recursive descent over the token stream.
//!
//! `OR` binds looser than the implicit `AND`, so `a b OR c` is `(a AND b) OR c`
//! — the same precedence every search bar users have met before uses.

use crate::ast::{Node, Predicate, MAX_DEPTH, MAX_TERMS};
use crate::token::{lex, Token};
use crate::QueryError;

struct Parser {
    toks: Vec<Token>,
    pos: usize,
    terms: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.toks.get(self.pos)
    }

    fn bump(&mut self) -> Option<Token> {
        let t = self.toks.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    /// Position to report for an error at the end of input.
    fn end_at(&self) -> usize {
        self.toks.last().map(|t| t.at()).unwrap_or(0)
    }

    fn count_term(&mut self) -> Result<(), QueryError> {
        self.terms += 1;
        if self.terms > MAX_TERMS {
            return Err(QueryError::TooManyTerms { max: MAX_TERMS });
        }
        Ok(())
    }

    fn or_expr(&mut self, depth: usize) -> Result<Node, QueryError> {
        if depth > MAX_DEPTH {
            return Err(QueryError::TooDeep { max: MAX_DEPTH });
        }
        let mut branches = vec![self.and_expr(depth)?];
        while let Some(Token::Or { at }) = self.peek().cloned() {
            self.bump();
            if self.at_expression_end() {
                return Err(QueryError::DanglingKeyword {
                    keyword: "OR".into(),
                    at,
                });
            }
            branches.push(self.and_expr(depth)?);
        }
        Ok(flatten(Node::Or(branches)))
    }

    fn and_expr(&mut self, depth: usize) -> Result<Node, QueryError> {
        if depth > MAX_DEPTH {
            return Err(QueryError::TooDeep { max: MAX_DEPTH });
        }
        let mut parts = vec![self.unary(depth)?];
        loop {
            // An explicit AND is sugar for the implicit one.
            if let Some(Token::And { at }) = self.peek().cloned() {
                self.bump();
                if self.at_expression_end() {
                    return Err(QueryError::DanglingKeyword {
                        keyword: "AND".into(),
                        at,
                    });
                }
                parts.push(self.unary(depth)?);
                continue;
            }
            if self.at_expression_end() {
                break;
            }
            parts.push(self.unary(depth)?);
        }
        Ok(flatten(Node::And(parts)))
    }

    /// True when the next token cannot start another operand.
    fn at_expression_end(&self) -> bool {
        matches!(
            self.peek(),
            None | Some(Token::Or { .. }) | Some(Token::RParen { .. })
        )
    }

    fn unary(&mut self, depth: usize) -> Result<Node, QueryError> {
        // A run of `!` recurses without passing through or_expr/and_expr, so it
        // MUST advance depth itself — otherwise `"!".repeat(100_000) + "a:1"`
        // overflows the stack before any guard fires, straight from an HTTP
        // query parameter.
        if depth > MAX_DEPTH {
            return Err(QueryError::TooDeep { max: MAX_DEPTH });
        }
        if let Some(Token::Bang { at }) = self.peek().cloned() {
            self.bump();
            if self.at_expression_end() {
                return Err(QueryError::DanglingBang { at });
            }
            return Ok(Node::Not(Box::new(self.unary(depth + 1)?)));
        }
        self.primary(depth)
    }

    fn primary(&mut self, depth: usize) -> Result<Node, QueryError> {
        match self.bump() {
            Some(Token::LParen { at }) => {
                let inner = self.or_expr(depth + 1)?;
                match self.bump() {
                    Some(Token::RParen { .. }) => Ok(inner),
                    _ => Err(QueryError::UnmatchedOpenParen { at }),
                }
            }
            Some(Token::RParen { at }) => Err(QueryError::UnmatchedCloseParen { at }),
            Some(Token::Term {
                field,
                value,
                quoted,
                at,
            }) => {
                self.count_term()?;
                Ok(Node::Pred(Predicate {
                    field,
                    value,
                    quoted,
                    at,
                }))
            }
            Some(Token::Text { text, .. }) => {
                self.count_term()?;
                Ok(Node::Text(text))
            }
            Some(Token::Or { at }) => Err(QueryError::DanglingKeyword {
                keyword: "OR".into(),
                at,
            }),
            Some(Token::And { at }) => Err(QueryError::DanglingKeyword {
                keyword: "AND".into(),
                at,
            }),
            Some(Token::Bang { at }) => Err(QueryError::DanglingBang { at }),
            None => Err(QueryError::DanglingKeyword {
                keyword: "".into(),
                at: self.end_at(),
            }),
        }
    }
}

/// Collapse single-child And/Or so the tree stays minimal. The planner and the
/// renderer both assume no redundant wrappers.
fn flatten(n: Node) -> Node {
    match n {
        Node::And(mut v) | Node::Or(mut v) if v.len() == 1 => v.remove(0),
        other => other,
    }
}

pub fn parse(input: &str) -> Result<Node, QueryError> {
    let toks = lex(input)?;
    if toks.is_empty() {
        return Ok(Node::And(vec![]));
    }
    let mut p = Parser {
        toks,
        pos: 0,
        terms: 0,
    };
    let node = p.or_expr(0)?;
    // Anything left over is a stray closer.
    if let Some(tok) = p.peek() {
        return Err(QueryError::UnmatchedCloseParen { at: tok.at() });
    }
    Ok(node)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{MatchOp, Node, Predicate};

    fn pred(field: &str, value: &str) -> Node {
        Node::Pred(Predicate {
            field: field.into(),
            value: value.into(),
            quoted: false,
            at: 0,
        })
    }

    /// Compare structure while ignoring byte offsets, which vary by input.
    fn strip(n: &Node) -> Node {
        match n {
            Node::And(v) => Node::And(v.iter().map(strip).collect()),
            Node::Or(v) => Node::Or(v.iter().map(strip).collect()),
            Node::Not(b) => Node::Not(Box::new(strip(b))),
            Node::Pred(p) => Node::Pred(Predicate { at: 0, ..p.clone() }),
            Node::Text(t) => Node::Text(t.clone()),
        }
    }

    #[test]
    fn parses_a_single_predicate() {
        assert_eq!(
            strip(&parse("level:error").unwrap()),
            pred("level", "error")
        );
    }

    #[test]
    fn implicit_and_between_terms() {
        assert_eq!(
            strip(&parse("level:error is:unresolved").unwrap()),
            Node::And(vec![pred("level", "error"), pred("is", "unresolved")])
        );
    }

    #[test]
    fn explicit_and_is_equivalent_to_implicit() {
        assert_eq!(
            strip(&parse("a:1 AND b:2").unwrap()),
            strip(&parse("a:1 b:2").unwrap())
        );
    }

    #[test]
    fn or_binds_looser_than_and() {
        // a AND b OR c  ==  (a AND b) OR c
        assert_eq!(
            strip(&parse("a:1 b:2 OR c:3").unwrap()),
            Node::Or(vec![
                Node::And(vec![pred("a", "1"), pred("b", "2")]),
                pred("c", "3"),
            ])
        );
    }

    #[test]
    fn parens_override_precedence() {
        assert_eq!(
            strip(&parse("a:1 (b:2 OR c:3)").unwrap()),
            Node::And(vec![
                pred("a", "1"),
                Node::Or(vec![pred("b", "2"), pred("c", "3")]),
            ])
        );
    }

    #[test]
    fn bang_negates_the_following_term() {
        assert_eq!(
            strip(&parse("!level:error").unwrap()),
            Node::Not(Box::new(pred("level", "error")))
        );
    }

    #[test]
    fn bang_negates_a_parenthesised_group() {
        assert_eq!(
            strip(&parse("!(a:1 OR b:2)").unwrap()),
            Node::Not(Box::new(Node::Or(vec![pred("a", "1"), pred("b", "2")])))
        );
    }

    #[test]
    fn free_text_becomes_a_text_node() {
        assert_eq!(
            strip(&parse("level:error timeout").unwrap()),
            Node::And(vec![pred("level", "error"), Node::Text("timeout".into())])
        );
    }

    #[test]
    fn empty_query_is_an_empty_and() {
        assert_eq!(parse("").unwrap(), Node::And(vec![]));
        assert_eq!(parse("   ").unwrap(), Node::And(vec![]));
    }

    #[test]
    fn single_child_groups_are_flattened() {
        // Avoid And([And([x])]) — the planner and renderer both assume minimal trees.
        assert_eq!(strip(&parse("(a:1)").unwrap()), pred("a", "1"));
    }

    #[test]
    fn rejects_unmatched_open_paren() {
        assert!(matches!(
            parse("(a:1"),
            Err(QueryError::UnmatchedOpenParen { .. })
        ));
    }

    #[test]
    fn rejects_unmatched_close_paren() {
        assert!(matches!(
            parse("a:1)"),
            Err(QueryError::UnmatchedCloseParen { .. })
        ));
    }

    #[test]
    fn rejects_dangling_or() {
        assert!(matches!(
            parse("a:1 OR"),
            Err(QueryError::DanglingKeyword { .. })
        ));
    }

    #[test]
    fn rejects_dangling_bang() {
        assert!(matches!(
            parse("a:1 !"),
            Err(QueryError::DanglingBang { .. })
        ));
    }

    #[test]
    fn rejects_excessive_nesting() {
        let deep = format!("{}a:1{}", "(".repeat(20), ")".repeat(20));
        assert!(matches!(parse(&deep), Err(QueryError::TooDeep { .. })));
    }

    #[test]
    fn rejects_a_long_negation_chain_without_overflowing() {
        // `!` recursion bypasses or_expr/and_expr, so it needs its own depth
        // check. Without one this overflows the stack instead of erroring.
        let bangs = format!("{}a:1", "!".repeat(10_000));
        assert!(matches!(parse(&bangs), Err(QueryError::TooDeep { .. })));
    }

    #[test]
    fn allows_negation_up_to_the_depth_limit() {
        // Guard against "fix" by rejecting all negation.
        assert!(parse("!!a:1").is_ok());
    }

    #[test]
    fn rejects_too_many_terms() {
        let wide = (0..100)
            .map(|i| format!("f{i}:1"))
            .collect::<Vec<_>>()
            .join(" ");
        assert!(matches!(parse(&wide), Err(QueryError::TooManyTerms { .. })));
    }

    #[test]
    fn match_op_is_declared() {
        // Populated by the resolver in Task 5; asserted here so the enum exists.
        assert_ne!(MatchOp::Eq, MatchOp::Ne);
    }
}
