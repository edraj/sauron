//! Lexer. Splits a query string into structural tokens without knowing any
//! Sauron field names — everything semantic happens in `resolve`.
//!
//! Two rules make the grammar unambiguous without needing escapes everywhere:
//!
//! 1. A `(` is structural only at the START of a token, and a `)` only at the
//!    END. `culprit:handle(req` keeps its interior paren; wrap the value in
//!    quotes when you need a trailing one.
//! 2. Quoting a value makes it literal — `*` inside quotes is a real asterisk,
//!    not a wildcard — which is the only way to search for a literal star.

use crate::QueryError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    LParen {
        at: usize,
    },
    RParen {
        at: usize,
    },
    Bang {
        at: usize,
    },
    Or {
        at: usize,
    },
    And {
        at: usize,
    },
    Term {
        field: String,
        value: String,
        quoted: bool,
        at: usize,
    },
    Text {
        text: String,
        at: usize,
    },
}

impl Token {
    pub fn at(&self) -> usize {
        match self {
            Token::LParen { at }
            | Token::RParen { at }
            | Token::Bang { at }
            | Token::Or { at }
            | Token::And { at }
            | Token::Term { at, .. }
            | Token::Text { at, .. } => *at,
        }
    }
}

/// True when `s` can be a field name: a leading letter or underscore, then
/// letters, digits, `_`, `-`, or `.` (for dotted paths like `user.email`).
pub(crate) fn is_field_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
}

pub fn lex(input: &str) -> Result<Vec<Token>, QueryError> {
    let bytes = input.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;

    while i < bytes.len() {
        // Skip whitespace.
        if bytes[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let start = i;

        // Structural characters that stand alone.
        match bytes[i] {
            b'(' => {
                out.push(Token::LParen { at: start });
                i += 1;
                continue;
            }
            b')' => {
                out.push(Token::RParen { at: start });
                i += 1;
                continue;
            }
            b'!' => {
                out.push(Token::Bang { at: start });
                i += 1;
                continue;
            }
            _ => {}
        }

        // Read one word. Quoted runs are absorbed whole (spaces included); an
        // unquoted `)` terminates the word so `(a OR b)` closes correctly.
        let mut raw = String::new();
        let mut saw_quote = false;
        let mut quoted_whole = false;
        while i < bytes.len() {
            let c = bytes[i];
            if c.is_ascii_whitespace() {
                break;
            }
            if c == b')' {
                break;
            }
            if c == b'"' {
                if raw.is_empty() && !saw_quote {
                    // The quote opens the word, so the WHOLE token is a quoted
                    // literal — not a `field:"value"` pair.
                    quoted_whole = true;
                }
                saw_quote = true;
                let quote_at = i;
                i += 1; // consume opening quote
                let mut closed = false;
                while i < bytes.len() {
                    if bytes[i] == b'\\' && i + 1 < bytes.len() {
                        raw.push(input[i + 1..].chars().next().unwrap());
                        i += 1 + input[i + 1..].chars().next().unwrap().len_utf8();
                        continue;
                    }
                    if bytes[i] == b'"' {
                        i += 1;
                        closed = true;
                        break;
                    }
                    let ch = input[i..].chars().next().unwrap();
                    raw.push(ch);
                    i += ch.len_utf8();
                }
                if !closed {
                    return Err(QueryError::UnterminatedQuote { at: quote_at });
                }
                continue;
            }
            let ch = input[i..].chars().next().unwrap();
            raw.push(ch);
            i += ch.len_utf8();
        }

        if raw.is_empty() {
            continue;
        }

        // Bare boolean keywords. Quoted text is never a keyword, which is why
        // `saw_quote` gates this.
        if !saw_quote {
            if raw.eq_ignore_ascii_case("or") {
                out.push(Token::Or { at: start });
                continue;
            }
            if raw.eq_ignore_ascii_case("and") {
                out.push(Token::And { at: start });
                continue;
            }
        }

        // A fully-quoted word is always free text. This is how you search for a
        // literal `OR`, or for text that happens to look like `level:error`.
        if quoted_whole {
            out.push(Token::Text {
                text: raw,
                at: start,
            });
            continue;
        }

        // field:value, splitting on the FIRST colon. Reject when the value side
        // is empty or the field side isn't an identifier — those are free text.
        //
        // Note the value side is taken from the ALREADY-UNQUOTED `raw`, so
        // `message:"a b"` yields field `message`, value `a b`.
        match raw.split_once(':') {
            Some((field, value)) if is_field_ident(field) && (!value.is_empty() || saw_quote) => {
                out.push(Token::Term {
                    field: field.to_string(),
                    value: value.to_string(),
                    quoted: saw_quote,
                    at: start,
                });
            }
            _ => out.push(Token::Text {
                text: raw,
                at: start,
            }),
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terms(input: &str) -> Vec<Token> {
        lex(input).unwrap()
    }

    #[test]
    fn lexes_a_simple_term() {
        assert_eq!(
            terms("level:error"),
            vec![Token::Term {
                field: "level".into(),
                value: "error".into(),
                quoted: false,
                at: 0,
            }]
        );
    }

    #[test]
    fn value_may_contain_colons() {
        // Split on the FIRST colon only, matching the legacy `filter=` behaviour.
        assert_eq!(
            terms("culprit:foo:bar"),
            vec![Token::Term {
                field: "culprit".into(),
                value: "foo:bar".into(),
                quoted: false,
                at: 0,
            }]
        );
    }

    #[test]
    fn quoted_value_keeps_spaces_and_sets_quoted_flag() {
        assert_eq!(
            terms(r#"message:"connection refused""#),
            vec![Token::Term {
                field: "message".into(),
                value: "connection refused".into(),
                quoted: true,
                at: 0,
            }]
        );
    }

    #[test]
    fn bare_word_is_free_text() {
        assert_eq!(
            terms("timeout"),
            vec![Token::Text {
                text: "timeout".into(),
                at: 0
            }]
        );
    }

    #[test]
    fn quoted_bare_string_is_free_text() {
        assert_eq!(
            terms(r#""connection refused""#),
            vec![Token::Text {
                text: "connection refused".into(),
                at: 0
            }]
        );
    }

    #[test]
    fn lexes_boolean_keywords_case_insensitively() {
        let got = terms("a OR b or c AND d");
        assert!(matches!(got[1], Token::Or { .. }));
        assert!(matches!(got[3], Token::Or { .. }));
        assert!(matches!(got[5], Token::And { .. }));
    }

    #[test]
    fn keywords_are_only_keywords_when_bare() {
        // A field named `or` and a quoted "OR" are values, not operators.
        assert!(matches!(terms(r#""OR""#)[0], Token::Text { .. }));
        assert!(matches!(terms("tag.or:x")[0], Token::Term { .. }));
    }

    #[test]
    fn lexes_parens_at_token_boundaries() {
        let got = terms("(a OR b)");
        assert!(matches!(got[0], Token::LParen { .. }));
        assert!(matches!(got[4], Token::RParen { .. }));
    }

    #[test]
    fn parens_inside_a_value_are_literal() {
        // Only a leading `(` or trailing `)` is structural; interior parens are
        // part of the value, so `fn(x)` needs no quoting mid-token.
        assert_eq!(
            terms("culprit:handle(req)"),
            vec![
                Token::Term {
                    field: "culprit".into(),
                    value: "handle(req".into(),
                    quoted: false,
                    at: 0,
                },
                Token::RParen { at: 18 }
            ]
        );
        // Quoting makes it unambiguous.
        assert_eq!(
            terms(r#"culprit:"handle(req)""#),
            vec![Token::Term {
                field: "culprit".into(),
                value: "handle(req)".into(),
                quoted: true,
                at: 0,
            }]
        );
    }

    #[test]
    fn lexes_bang() {
        let got = terms("!level:error");
        assert!(matches!(got[0], Token::Bang { at: 0 }));
        assert!(matches!(got[1], Token::Term { .. }));
    }

    #[test]
    fn rejects_unterminated_quote() {
        assert_eq!(
            lex(r#"message:"oops"#),
            Err(QueryError::UnterminatedQuote { at: 8 })
        );
    }

    #[test]
    fn empty_input_lexes_to_nothing() {
        assert_eq!(lex("   ").unwrap(), vec![]);
    }

    #[test]
    fn records_byte_offsets() {
        let got = terms("level:error !is:resolved");
        assert_eq!(got[0].at(), 0);
        assert_eq!(got[1].at(), 12);
        assert_eq!(got[2].at(), 13);
    }

    #[test]
    fn field_must_look_like_an_identifier() {
        // `10:30` is not a field:value pair — no leading letter/underscore.
        assert!(matches!(terms("10:30")[0], Token::Text { .. }));
    }

    #[test]
    fn empty_value_is_free_text_not_a_term() {
        assert!(matches!(terms("level:")[0], Token::Text { .. }));
    }

    #[test]
    fn a_fully_quoted_word_is_always_free_text() {
        assert!(matches!(terms(r#""level:error""#)[0], Token::Text { .. }));
        assert!(matches!(terms(r#""OR""#)[0], Token::Text { .. }));
    }

    #[test]
    fn a_quoted_value_side_still_forms_a_term() {
        assert_eq!(
            terms(r#"message:"connection refused""#),
            vec![Token::Term {
                field: "message".into(),
                value: "connection refused".into(),
                quoted: true,
                at: 0,
            }]
        );
    }

    #[test]
    fn an_explicitly_quoted_empty_value_forms_a_term() {
        assert_eq!(
            terms(r#"level:"""#),
            vec![Token::Term {
                field: "level".into(),
                value: String::new(),
                quoted: true,
                at: 0,
            }]
        );
    }
}
