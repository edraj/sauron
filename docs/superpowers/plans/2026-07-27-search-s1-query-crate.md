# Search S1 — `sauron-query` crate Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `crates/sauron-query`, a pure dependency-free-of-the-database crate that turns a search string into a validated, cost-classified query AST.

**Architecture:** Two-level model. A *syntactic* level (`lex` → `parse`) produces a `Node` tree holding raw strings, knowing nothing about Sauron. A *semantic* level (`resolve`) walks that tree against a static `Dimension` catalog, producing `ResolvedNode` where every field is a `&'static Dimension` and every value is typed. `classify` then labels the resolved tree `Indexed`/`Bounded`/`Scan`. A `legacy` module parses today's `filter=`/`q=` params into the same `Node` tree, and a `render` module turns a tree back into canonical text.

**Tech Stack:** Rust 2021, edition/version from `[workspace.package]`. Dependencies limited to `serde`, `serde_json`, `chrono`, `thiserror`, `percent-encoding` — all already in `[workspace.dependencies]`. **No `diesel`, no `sauron-db`, no async, no I/O.**

## Global Constraints

- Spec: `docs/superpowers/specs/2026-07-27-pro-search-and-saved-views-design.md`. Read §5 (grammar), §6 (catalog), §7 (architecture) before starting.
- Rust edition `2021`, `rust-version = "1.82"`, license `LGPL-3.0-only` — all inherited via `version.workspace = true` / `edition.workspace = true` / `license.workspace = true`.
- Every dependency is declared `foo.workspace = true`. Never pin a version in a crate manifest.
- Tests are inline `#[cfg(test)] mod tests { use super::*; ... }` in the same file as the code. This repo has **no test DB and no integration-test directory**; that is why this crate is pure.
- Hard gates, run from `backend/`: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
- Never `cargo test --all-features` — it rebuilds DuckDB from source.
- Builds need libduckdb on the library path for *other* workspace crates even though
  this one doesn't use it. The library is **nested by version and target triple**, not
  at the top of `.cache/duckdb` — pointing at the parent directory fails to link with
  `cannot find -lduckdb`:
  ```bash
  export DUCKDB_LIB_DIR=$(ls -d "$(git rev-parse --show-toplevel)"/.cache/duckdb/*/*/ | head -1)
  export LD_LIBRARY_PATH=$DUCKDB_LIB_DIR:$LD_LIBRARY_PATH
  ```
  (fetch with `packaging/rpm/fetch-libduckdb.sh` if `.cache/duckdb` is absent).
  Scoping commands to `-p sauron-query` avoids needing this at all.
- **Never create a git branch. Never commit.** Leave all changes staged in the working tree; the user commits. Every task ends with a verification step instead of a commit step.
- This crate must not be added as a dependency of any other crate in S1. It compiles and tests standalone. Wiring happens in S2.

---

## File Structure

| File | Responsibility |
|---|---|
| `backend/crates/sauron-query/Cargo.toml` | Manifest |
| `backend/crates/sauron-query/src/lib.rs` | Module wiring + the crate's public entry points |
| `backend/crates/sauron-query/src/error.rs` | `QueryError` with byte offsets for caret-style messages |
| `backend/crates/sauron-query/src/token.rs` | `Token`, `lex()` |
| `backend/crates/sauron-query/src/ast.rs` | `Node`, `Predicate`, `MatchOp` |
| `backend/crates/sauron-query/src/parse.rs` | `parse()` — recursive descent over tokens |
| `backend/crates/sauron-query/src/catalog.rs` | `Resource`, `Store`, `ValueType`, `IndexClass`, `Dimension`, `CATALOG`, `lookup()` |
| `backend/crates/sauron-query/src/resolve.rs` | `ResolvedNode`, `ResolvedPredicate`, `TypedValue`, `resolve()` |
| `backend/crates/sauron-query/src/cost.rs` | `Cost`, `classify()` |
| `backend/crates/sauron-query/src/legacy.rs` | `from_legacy()` — `filter=`/`q=` → `Node` |
| `backend/crates/sauron-query/src/render.rs` | `render()` — `Node` → canonical string |
| `backend/Cargo.toml` | Add `sauron-query` to `[workspace.dependencies]` |

Task order matches dependency order: 1 (scaffold+error) → 2 (lex) → 3 (ast+parse) → 4 (catalog) → 5 (resolve) → 6 (cost) → 7 (legacy) → 8 (render).

---

### Task 1: Crate scaffold and error type

**Files:**
- Create: `backend/crates/sauron-query/Cargo.toml`
- Create: `backend/crates/sauron-query/src/lib.rs`
- Create: `backend/crates/sauron-query/src/error.rs`
- Modify: `backend/Cargo.toml` (add to `[workspace.dependencies]`, after the `sauron-alerts` line)

**Interfaces:**
- Consumes: nothing.
- Produces: `sauron_query::QueryError` (enum, `thiserror::Error`, `PartialEq`), with variants used by every later task. `QueryError::at()` helper returning the byte offset.

- [ ] **Step 1: Create the manifest**

`backend/crates/sauron-query/Cargo.toml`:

```toml
[package]
name = "sauron-query"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
serde.workspace = true
serde_json.workspace = true
chrono.workspace = true
thiserror.workspace = true
percent-encoding.workspace = true

[dev-dependencies]
serde_json.workspace = true
```

- [ ] **Step 2: Register in the workspace**

In `backend/Cargo.toml`, inside `[workspace.dependencies]` under `# --- internal crates ---`, add after the `sauron-alerts` line:

```toml
sauron-query = { path = "crates/sauron-query" }
```

`members = ["crates/*", "bins/*"]` already globs the new directory, so no `members` edit is needed.

- [ ] **Step 3: Write the failing test**

`backend/crates/sauron-query/src/error.rs`:

```rust
//! Errors surfaced to the user as they type. Every variant carries the byte
//! offset in the original query string so the UI can render a caret under the
//! offending token rather than a bare sentence.

use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum QueryError {
    #[error("unterminated quoted string")]
    UnterminatedQuote { at: usize },
    #[error("unmatched opening parenthesis")]
    UnmatchedOpenParen { at: usize },
    #[error("unmatched closing parenthesis")]
    UnmatchedCloseParen { at: usize },
    #[error("expected a search term after `{keyword}`")]
    DanglingKeyword { keyword: String, at: usize },
    #[error("expected a search term after `!`")]
    DanglingBang { at: usize },
    #[error("`{field}` is not a valid field for this view")]
    UnknownField { field: String, at: usize },
    #[error("operator not allowed on `{field}`")]
    BadOp { field: String, at: usize },
    #[error("`{value}` is not a valid value for `{field}`")]
    BadValue {
        field: String,
        value: String,
        at: usize,
    },
    #[error("`{value}` is not a known value for `{field}`")]
    BadEnum {
        field: String,
        value: String,
        at: usize,
    },
    #[error("`is:{value}` is not a known shorthand")]
    BadShorthand { value: String, at: usize },
    #[error("query nests deeper than {max} levels")]
    TooDeep { max: usize },
    #[error("query has more than {max} terms")]
    TooManyTerms { max: usize },
}

impl QueryError {
    /// Byte offset into the original query string, for caret rendering.
    /// Structural limits have no single offset and report 0.
    pub fn at(&self) -> usize {
        match self {
            QueryError::UnterminatedQuote { at }
            | QueryError::UnmatchedOpenParen { at }
            | QueryError::UnmatchedCloseParen { at }
            | QueryError::DanglingKeyword { at, .. }
            | QueryError::DanglingBang { at }
            | QueryError::UnknownField { at, .. }
            | QueryError::BadOp { at, .. }
            | QueryError::BadValue { at, .. }
            | QueryError::BadEnum { at, .. }
            | QueryError::BadShorthand { at, .. } => *at,
            QueryError::TooDeep { .. } | QueryError::TooManyTerms { .. } => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_offset() {
        let e = QueryError::UnknownField {
            field: "nope".into(),
            at: 12,
        };
        assert_eq!(e.at(), 12);
    }

    #[test]
    fn structural_errors_report_zero() {
        assert_eq!(QueryError::TooDeep { max: 8 }.at(), 0);
    }

    #[test]
    fn display_names_the_field() {
        let e = QueryError::UnknownField {
            field: "enviroment".into(),
            at: 0,
        };
        assert_eq!(e.to_string(), "`enviroment` is not a valid field for this view");
    }
}
```

`backend/crates/sauron-query/src/lib.rs`:

```rust
//! Sauron search query language: lex → parse → resolve → classify.
//!
//! Deliberately free of database, async, and I/O dependencies. CI runs
//! `cargo test --workspace` with no Postgres or Redis service, so anything that
//! needs a live connection cannot be tested here. Keeping this crate pure is
//! what makes the grammar testable at all.

pub mod error;

pub use error::QueryError;
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cd backend && cargo test -p sauron-query
```

Expected: `3 passed`.

- [ ] **Step 5: Verify gates**

```bash
cd backend && cargo fmt --all -- --check && cargo clippy -p sauron-query --all-targets -- -D warnings
```

Expected: no output from either. Leave changes staged; do not commit.

---

### Task 2: Lexer

**Files:**
- Create: `backend/crates/sauron-query/src/token.rs`
- Modify: `backend/crates/sauron-query/src/lib.rs`

**Interfaces:**
- Consumes: `QueryError` from Task 1.
- Produces:
  - `pub enum Token { LParen{at}, RParen{at}, Bang{at}, Or{at}, And{at}, Term{field, value, quoted, at}, Text{text, at} }`
  - `pub fn lex(input: &str) -> Result<Vec<Token>, QueryError>`

- [ ] **Step 1: Write the failing test**

Append to `backend/crates/sauron-query/src/token.rs` (write the whole file in Step 3; write only this test module first so the failure is real):

```rust
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
            vec![Token::Term {
                field: "culprit".into(),
                value: "handle(req".into(),
                quoted: false,
                at: 0,
            },
            Token::RParen { at: 18 }]
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
}
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cd backend && cargo test -p sauron-query
```

Expected: compile error — `cannot find function `lex` in this scope`.

- [ ] **Step 3: Write the implementation**

Prepend to `backend/crates/sauron-query/src/token.rs`, above the test module:

```rust
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
    LParen { at: usize },
    RParen { at: usize },
    Bang { at: usize },
    Or { at: usize },
    And { at: usize },
    Term {
        field: String,
        value: String,
        quoted: bool,
        at: usize,
    },
    Text { text: String, at: usize },
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
        // True when the opening quote is the FIRST thing in the word, meaning the
        // whole token is a quoted literal rather than a `field:"value"` pair.
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
        // literal `OR`, or for text that happens to look like `level:error` —
        // without this, rendering such a node emits it bare and the next parse
        // turns it into a keyword or a predicate.
        if quoted_whole {
            out.push(Token::Text { text: raw, at: start });
            continue;
        }

        // field:value, splitting on the FIRST colon. Reject when the field side
        // isn't an identifier — that's free text. An empty value side is allowed
        // only when it was explicitly quoted (`level:""`), so that a legacy
        // `filter=level:eq:` round-trips as a predicate rather than decaying to text.
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
```

Add to `backend/crates/sauron-query/src/lib.rs`:

```rust
pub mod token;

pub use token::{lex, Token};
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cd backend && cargo test -p sauron-query
```

Expected: all lexer tests pass.

- [ ] **Step 5: Verify gates**

```bash
cd backend && cargo fmt --all -- --check && cargo clippy -p sauron-query --all-targets -- -D warnings
```

Expected: clean. Leave staged; do not commit.

---

### Task 3: AST and parser

**Files:**
- Create: `backend/crates/sauron-query/src/ast.rs`
- Create: `backend/crates/sauron-query/src/parse.rs`
- Modify: `backend/crates/sauron-query/src/lib.rs`

**Interfaces:**
- Consumes: `Token`, `lex` (Task 2); `QueryError` (Task 1).
- Produces:
  - `pub enum Node { And(Vec<Node>), Or(Vec<Node>), Not(Box<Node>), Pred(Predicate), Text(String) }`
  - `pub struct Predicate { pub field: String, pub value: String, pub quoted: bool, pub at: usize }`
  - `pub enum MatchOp { Eq, Ne, Gt, Gte, Lt, Lte, In, Has, Like }` (declared here, populated by Task 5)
  - `pub const MAX_DEPTH: usize = 8;` `pub const MAX_TERMS: usize = 64;`
  - `pub fn parse(input: &str) -> Result<Node, QueryError>`

Grammar (recursive descent, `OR` binds looser than implicit `AND`):

```
query    := or_expr
or_expr  := and_expr ( OR and_expr )*
and_expr := unary ( AND? unary )*
unary    := '!' unary | primary
primary  := '(' or_expr ')' | Term | Text
```

- [ ] **Step 1: Write the failing test**

Create `backend/crates/sauron-query/src/parse.rs` containing only this test module:

```rust
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
        assert_eq!(strip(&parse("level:error").unwrap()), pred("level", "error"));
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
        assert!(matches!(parse("a:1 !"), Err(QueryError::DanglingBang { .. })));
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
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cd backend && cargo test -p sauron-query
```

Expected: compile error — `file not found for module `ast`` / `cannot find function `parse``.

- [ ] **Step 3: Write the AST**

`backend/crates/sauron-query/src/ast.rs`:

```rust
//! The syntactic tree. Holds raw strings only — no Sauron field knowledge and
//! no typed values. `resolve` turns this into the semantic tree.

/// Comparison chosen for a predicate. Derived from the value's leading
/// characters during resolution, not during parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchOp {
    Eq,
    Ne,
    Gt,
    Gte,
    Lt,
    Lte,
    /// `field:[a,b,c]`
    In,
    /// `has:field` — key existence, no value
    Has,
    /// Wildcard match from an unquoted value containing `*`
    Like,
    /// `field:~text` — LITERAL substring match. Distinct from `Like` because the
    /// value is not scanned for wildcards at all, so a `*` in the user's own data
    /// stays a `*`. This is what the pre-language `contains` operator meant, and
    /// the legacy bridge maps onto it to keep existing shared URLs returning the
    /// same rows.
    Contains,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Predicate {
    pub field: String,
    /// Raw value exactly as typed, minus surrounding quotes. Operator prefixes
    /// (`>`, `>=`, …) and list brackets are still present; the resolver strips them.
    pub value: String,
    /// True when the value was quoted, which makes `*` literal rather than a wildcard.
    pub quoted: bool,
    pub at: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    And(Vec<Node>),
    Or(Vec<Node>),
    Not(Box<Node>),
    Pred(Predicate),
    Text(String),
}

/// Bounds that keep a hostile query from becoming a planner problem. Chosen to
/// be far above any hand-written query and far below anything that costs real time.
pub const MAX_DEPTH: usize = 8;
pub const MAX_TERMS: usize = 64;
```

- [ ] **Step 4: Write the parser**

Prepend to `backend/crates/sauron-query/src/parse.rs`, above the test module:

```rust
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
```

Add to `backend/crates/sauron-query/src/lib.rs`:

```rust
pub mod ast;
pub mod parse;

pub use ast::{MatchOp, Node, Predicate};
pub use parse::parse;
```

- [ ] **Step 5: Run the tests to verify they pass**

```bash
cd backend && cargo test -p sauron-query
```

Expected: all parser tests pass.

- [ ] **Step 6: Verify gates**

```bash
cd backend && cargo fmt --all -- --check && cargo clippy -p sauron-query --all-targets -- -D warnings
```

Expected: clean. Leave staged; do not commit.

---

### Task 4: Dimension catalog

**Files:**
- Create: `backend/crates/sauron-query/src/catalog.rs`
- Modify: `backend/crates/sauron-query/src/lib.rs`

**Interfaces:**
- Consumes: `MatchOp` (Task 3).
- Produces:
  - `pub enum Resource { Issues, Occurrences, Events, Sessions, Devices, Persons, Transactions }`
  - `pub enum Store { Column(&'static str), JsonRoot { column, prefix }, Tag, Rollup }`
  - `pub enum ValueType { Str, Enum(&'static [&'static str]), Int, Bool, Duration, Timestamp }`
  - `pub enum IndexClass { Indexed, Bounded, Scan }`
  - `pub struct Dimension { name, aliases, ty, store, ops, resources, index }`
  - `pub const CATALOG: &[Dimension]`
  - `pub const SHORTHANDS: &[Shorthand]` with `pub struct Shorthand { keyword, field, value }`
  - `pub fn lookup(field: &str, r: Resource) -> Option<&'static Dimension>`
  - `pub fn dimensions_for(r: Resource) -> impl Iterator<Item = &'static Dimension>`

This is the artefact §6 of the spec calls the single source of truth. Later slices generate the `/search/fields` endpoint, the in-app docs table, and the `wiki/Search.md` field table from it.

- [ ] **Step 1: Write the failing test**

Create `backend/crates/sauron-query/src/catalog.rs` with only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_up_a_curated_field() {
        let d = lookup("level", Resource::Issues).unwrap();
        assert_eq!(d.name, "level");
        assert!(matches!(d.store, Store::Column("level")));
    }

    #[test]
    fn lookup_is_resource_scoped() {
        // `duration` is a transaction dimension and must not leak onto Issues.
        assert!(lookup("duration", Resource::Transactions).is_some());
        assert!(lookup("duration", Resource::Issues).is_none());
    }

    #[test]
    fn resolves_aliases() {
        // `distinctId` is the canonical spelling; the DB column name also works.
        let a = lookup("distinctId", Resource::Occurrences).unwrap();
        let b = lookup("distinct_id", Resource::Occurrences).unwrap();
        assert_eq!(a.name, b.name);
    }

    #[test]
    fn level_on_issues_is_the_column_not_the_rollup() {
        // Spec §6: exactly one meaning for `level:error` on Issues.
        let d = lookup("level", Resource::Issues).unwrap();
        assert!(matches!(d.store, Store::Column(_)));
        assert!(!matches!(d.store, Store::Rollup));
    }

    #[test]
    fn environment_on_issues_is_the_rollup() {
        let d = lookup("environment", Resource::Issues).unwrap();
        assert!(matches!(d.store, Store::Rollup));
        assert_eq!(d.index, IndexClass::Indexed);
    }

    #[test]
    fn json_roots_are_registered_for_dynamic_paths() {
        for root in ["extra", "contexts", "user", "os"] {
            assert!(
                lookup(root, Resource::Occurrences).is_some(),
                "missing JSON root `{root}`"
            );
        }
    }

    #[test]
    fn shorthands_cover_status_and_handled() {
        let names: Vec<_> = SHORTHANDS.iter().map(|s| s.keyword).collect();
        for k in ["unresolved", "resolved", "ignored", "handled", "unhandled"] {
            assert!(names.contains(&k), "missing shorthand `{k}`");
        }
    }

    #[test]
    fn handled_shorthands_target_the_handled_field() {
        let s = SHORTHANDS.iter().find(|s| s.keyword == "unhandled").unwrap();
        assert_eq!(s.field, "handled");
        assert_eq!(s.value, "false");
    }

    #[test]
    fn every_dimension_declares_at_least_one_op_and_resource() {
        for d in CATALOG {
            assert!(!d.ops.is_empty(), "`{}` declares no operators", d.name);
            assert!(!d.resources.is_empty(), "`{}` declares no resources", d.name);
        }
    }

    #[test]
    fn dimension_names_are_unique_within_a_resource() {
        for r in Resource::ALL {
            let mut seen = std::collections::HashSet::new();
            for d in dimensions_for(*r) {
                for key in std::iter::once(d.name).chain(d.aliases.iter().copied()) {
                    assert!(
                        seen.insert(key),
                        "`{key}` is declared twice for {:?}",
                        r
                    );
                }
            }
        }
    }

    #[test]
    fn enum_dimensions_list_their_options() {
        let d = lookup("is", Resource::Issues).unwrap();
        match d.ty {
            ValueType::Enum(opts) => assert!(opts.contains(&"unresolved")),
            _ => panic!("`is` should be an enum"),
        }
    }

    #[test]
    fn dimensions_for_filters_by_resource() {
        assert!(dimensions_for(Resource::Devices).any(|d| d.name == "browser"));
        assert!(!dimensions_for(Resource::Devices).any(|d| d.name == "culprit"));
    }

    #[test]
    fn dotted_names_resolve_as_whole_fields() {
        // `http.status` is a real column, matched exactly — not a JSON path.
        let d = lookup("http.status", Resource::Transactions).unwrap();
        assert!(matches!(d.store, Store::Column("http_status")));
    }

    #[test]
    fn browser_is_a_column_on_devices_and_json_on_occurrences() {
        assert!(matches!(
            lookup("browser", Resource::Devices).unwrap().store,
            Store::Column("browser")
        ));
        assert!(matches!(
            lookup("browser", Resource::Occurrences).unwrap().store,
            Store::JsonRoot { column: "context", .. }
        ));
    }

    #[test]
    fn tag_fallback_is_available_only_where_tags_exist() {
        assert!(tag_dimension(Resource::Issues).is_some());
        assert!(tag_dimension(Resource::Occurrences).is_some());
        assert!(tag_dimension(Resource::Events).is_some());
        assert!(tag_dimension(Resource::Devices).is_none());
        assert!(tag_dimension(Resource::Persons).is_none());
        assert!(tag_dimension(Resource::Transactions).is_none());
    }

    #[test]
    fn tag_dim_is_not_in_the_public_catalog() {
        // It must not show up in autocomplete or the generated docs table.
        assert!(!CATALOG.iter().any(|d| std::ptr::eq(d, &TAG_DIM)));
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cd backend && cargo test -p sauron-query
```

Expected: compile error — `cannot find type `Resource``.

- [ ] **Step 3: Write the catalog**

Prepend to `backend/crates/sauron-query/src/catalog.rs`, above the test module. Each dimension is grounded in a column that exists today unless annotated otherwise:

```rust
//! The dimension catalog: the single source of truth for what is filterable.
//!
//! Five things derive from this table rather than being maintained alongside it:
//! field resolution, the planner's SQL mapping and cost classification, the
//! `/search/fields` autocomplete endpoint, the in-app docs reference, and the
//! `wiki/Search.md` field table.
//!
//! Adding a dimension here does NOT make it queryable — the planner (S2) must
//! also learn to map its `Store` to SQL. `dimensions_for` is what the tests in
//! S2 iterate to prove nothing is declared-but-unplanned.

use crate::ast::MatchOp;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Resource {
    Issues,
    Occurrences,
    Events,
    Sessions,
    Devices,
    Persons,
    Transactions,
}

impl Resource {
    pub const ALL: &'static [Resource] = &[
        Resource::Issues,
        Resource::Occurrences,
        Resource::Events,
        Resource::Sessions,
        Resource::Devices,
        Resource::Persons,
        Resource::Transactions,
    ];
}

/// Where the value physically lives. The planner switches on this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Store {
    /// A real column. The `&'static str` is the column name.
    Column(&'static str),
    /// A JSONB column addressed by a caller-supplied dynamic path, e.g. `extra.*`.
    ///
    /// `column` is the physical column; `prefix` is the path segment that must be
    /// prepended inside it. They differ because `enrich.rs` writes several
    /// namespaces into one `context` column — `os.name` lives at `context->os->name`
    /// (prefix `os`), whereas `extra.cartValue` lives at `extra->cartValue`
    /// (prefix empty) and `user.email` at `event_user->email` (prefix empty, since
    /// the column *is* the user object).
    JsonRoot {
        column: &'static str,
        prefix: &'static str,
    },
    /// The `tags` JSONB column, keyed by the dimension name itself.
    Tag,
    /// The `issue_dimensions` rollup table (built in S3).
    Rollup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueType {
    Str,
    Enum(&'static [&'static str]),
    Int,
    Bool,
    /// Accepts `2s` / `500ms` / bare milliseconds.
    Duration,
    /// Accepts `-7d` relative or ISO-8601 absolute.
    Timestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexClass {
    /// Backed by an index that serves this predicate directly.
    Indexed,
    /// Not indexed, but reached only after an indexed predicate has bounded the
    /// candidate set (same table, cheap to evaluate per row).
    Bounded,
    /// Requires reading the value off every candidate row.
    Scan,
}

/// `Debug`/`PartialEq` are required by `ResolvedPredicate` in Task 5, which
/// embeds a `&'static Dimension` and derives both. `Copy` is free here — every
/// field is a `&'static` reference or a plain enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dimension {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub ty: ValueType,
    pub store: Store,
    pub ops: &'static [MatchOp],
    pub resources: &'static [Resource],
    pub index: IndexClass,
}

/// `is:<keyword>` expands to a predicate on `field` with `value`.
pub struct Shorthand {
    pub keyword: &'static str,
    pub field: &'static str,
    pub value: &'static str,
}

/// Spec §5: `is:unhandled` and `is:handled` both exclude NULL — rows ingested
/// before the `handled` column existed are *unknown*, not handled.
pub const SHORTHANDS: &[Shorthand] = &[
    Shorthand { keyword: "unresolved", field: "is", value: "unresolved" },
    Shorthand { keyword: "resolved", field: "is", value: "resolved" },
    Shorthand { keyword: "ignored", field: "is", value: "ignored" },
    Shorthand { keyword: "handled", field: "handled", value: "true" },
    Shorthand { keyword: "unhandled", field: "handled", value: "false" },
];

const OPS_EQ: &[MatchOp] = &[MatchOp::Eq, MatchOp::Ne, MatchOp::In, MatchOp::Has];
const OPS_TEXT: &[MatchOp] = &[
    MatchOp::Eq,
    MatchOp::Ne,
    MatchOp::In,
    MatchOp::Has,
    MatchOp::Like,
    MatchOp::Contains,
];
const OPS_ORD: &[MatchOp] = &[
    MatchOp::Eq,
    MatchOp::Ne,
    MatchOp::Gt,
    MatchOp::Gte,
    MatchOp::Lt,
    MatchOp::Lte,
    MatchOp::Has,
];
const NO_ALIAS: &[&str] = &[];

const R_ISSUES: &[Resource] = &[Resource::Issues];
const R_OCC: &[Resource] = &[Resource::Occurrences];
const R_ISSUE_OCC: &[Resource] = &[Resource::Issues, Resource::Occurrences];
const R_EVENTS: &[Resource] = &[Resource::Events];
const R_OCC_EVENTS: &[Resource] = &[Resource::Occurrences, Resource::Events];
const R_TX: &[Resource] = &[Resource::Transactions];
const R_DEVICES: &[Resource] = &[Resource::Devices];
const R_PERSONS: &[Resource] = &[Resource::Persons];
const R_SESSIONS: &[Resource] = &[Resource::Sessions];

const LEVELS: &[&str] = &["debug", "info", "warning", "error", "fatal"];
const STATUSES: &[&str] = &["unresolved", "resolved", "ignored"];
const SYMBOLICATION: &[&str] = &[
    "pending",
    "processing",
    "symbolicated",
    "failed",
    "skipped",
    "unsupported",
];

pub const CATALOG: &[Dimension] = &[
    // ---- issues (own columns) ----
    Dimension { name: "is", aliases: &["status"], ty: ValueType::Enum(STATUSES), store: Store::Column("status"), ops: OPS_EQ, resources: R_ISSUES, index: IndexClass::Indexed },
    Dimension { name: "level", aliases: NO_ALIAS, ty: ValueType::Enum(LEVELS), store: Store::Column("level"), ops: OPS_EQ, resources: R_ISSUE_OCC, index: IndexClass::Bounded },
    Dimension { name: "type", aliases: NO_ALIAS, ty: ValueType::Str, store: Store::Column("type"), ops: OPS_TEXT, resources: R_ISSUES, index: IndexClass::Bounded },
    Dimension { name: "culprit", aliases: NO_ALIAS, ty: ValueType::Str, store: Store::Column("culprit"), ops: OPS_TEXT, resources: R_ISSUES, index: IndexClass::Bounded },
    Dimension { name: "title", aliases: NO_ALIAS, ty: ValueType::Str, store: Store::Column("title"), ops: OPS_TEXT, resources: R_ISSUES, index: IndexClass::Scan },
    Dimension { name: "timesSeen", aliases: &["times_seen"], ty: ValueType::Int, store: Store::Column("times_seen"), ops: OPS_ORD, resources: R_ISSUES, index: IndexClass::Bounded },
    Dimension { name: "usersSeen", aliases: &["users_seen"], ty: ValueType::Int, store: Store::Column("users_seen"), ops: OPS_ORD, resources: R_ISSUES, index: IndexClass::Bounded },
    Dimension { name: "firstSeen", aliases: &["first_seen"], ty: ValueType::Timestamp, store: Store::Column("first_seen"), ops: OPS_ORD, resources: R_ISSUES, index: IndexClass::Indexed },
    Dimension { name: "lastSeen", aliases: &["last_seen"], ty: ValueType::Timestamp, store: Store::Column("last_seen"), ops: OPS_ORD, resources: R_ISSUES, index: IndexClass::Indexed },

    // ---- issue-level rollups (S3: issue_dimensions) ----
    Dimension { name: "environment", aliases: NO_ALIAS, ty: ValueType::Str, store: Store::Rollup, ops: OPS_EQ, resources: R_ISSUES, index: IndexClass::Indexed },
    Dimension { name: "release", aliases: NO_ALIAS, ty: ValueType::Str, store: Store::Rollup, ops: OPS_EQ, resources: R_ISSUES, index: IndexClass::Indexed },
    Dimension { name: "handled", aliases: NO_ALIAS, ty: ValueType::Bool, store: Store::Rollup, ops: OPS_EQ, resources: R_ISSUES, index: IndexClass::Indexed },

    // ---- error_events / occurrences ----
    Dimension { name: "handled", aliases: NO_ALIAS, ty: ValueType::Bool, store: Store::Column("handled"), ops: OPS_EQ, resources: R_OCC, index: IndexClass::Bounded },
    Dimension { name: "environment", aliases: NO_ALIAS, ty: ValueType::Str, store: Store::Column("environment_id"), ops: OPS_EQ, resources: R_OCC_EVENTS, index: IndexClass::Indexed },
    Dimension { name: "release", aliases: NO_ALIAS, ty: ValueType::Str, store: Store::Column("release"), ops: OPS_TEXT, resources: R_OCC_EVENTS, index: IndexClass::Bounded },
    Dimension { name: "distinctId", aliases: &["distinct_id"], ty: ValueType::Str, store: Store::Column("distinct_id"), ops: OPS_TEXT, resources: &[Resource::Occurrences, Resource::Events, Resource::Persons], index: IndexClass::Indexed },
    // OPS_TEXT, not OPS_EQ: the legacy `EVENT_FILTERS` granted `session_id` the
    // full string operator set, so narrowing it here would reject shared URLs of
    // the form `filter=session_id:contains:…` outright.
    Dimension { name: "session", aliases: &["session_id"], ty: ValueType::Str, store: Store::Column("session_id"), ops: OPS_TEXT, resources: &[Resource::Occurrences, Resource::Events, Resource::Sessions], index: IndexClass::Bounded },
    Dimension { name: "deviceKey", aliases: &["device_key"], ty: ValueType::Str, store: Store::Column("device_key"), ops: OPS_EQ, resources: &[Resource::Occurrences, Resource::Devices], index: IndexClass::Bounded },
    Dimension { name: "screen", aliases: NO_ALIAS, ty: ValueType::Str, store: Store::Column("screen"), ops: OPS_TEXT, resources: R_OCC, index: IndexClass::Bounded },
    Dimension { name: "symbolication", aliases: &["symbolication_status"], ty: ValueType::Enum(SYMBOLICATION), store: Store::Column("symbolication_status"), ops: OPS_EQ, resources: R_OCC, index: IndexClass::Bounded },
    Dimension { name: "message", aliases: NO_ALIAS, ty: ValueType::Str, store: Store::Column("message"), ops: OPS_TEXT, resources: R_OCC, index: IndexClass::Scan },

    // ---- JSON roots reachable by dynamic path ----
    Dimension { name: "user", aliases: NO_ALIAS, ty: ValueType::Str, store: Store::JsonRoot { column: "event_user", prefix: "" }, ops: OPS_TEXT, resources: R_OCC, index: IndexClass::Bounded },
    Dimension { name: "sdk", aliases: NO_ALIAS, ty: ValueType::Str, store: Store::JsonRoot { column: "sdk", prefix: "" }, ops: OPS_TEXT, resources: R_OCC, index: IndexClass::Bounded },
    Dimension { name: "os", aliases: NO_ALIAS, ty: ValueType::Str, store: Store::JsonRoot { column: "context", prefix: "os" }, ops: OPS_TEXT, resources: R_OCC, index: IndexClass::Bounded },
    Dimension { name: "browser", aliases: &["runtime"], ty: ValueType::Str, store: Store::JsonRoot { column: "context", prefix: "browser" }, ops: OPS_TEXT, resources: R_OCC, index: IndexClass::Bounded },
    Dimension { name: "device", aliases: NO_ALIAS, ty: ValueType::Str, store: Store::JsonRoot { column: "context", prefix: "device" }, ops: OPS_TEXT, resources: R_OCC, index: IndexClass::Bounded },
    Dimension { name: "app", aliases: NO_ALIAS, ty: ValueType::Str, store: Store::JsonRoot { column: "context", prefix: "app" }, ops: OPS_TEXT, resources: R_OCC, index: IndexClass::Bounded },
    Dimension { name: "contexts", aliases: NO_ALIAS, ty: ValueType::Str, store: Store::JsonRoot { column: "contexts", prefix: "" }, ops: OPS_TEXT, resources: R_OCC_EVENTS, index: IndexClass::Bounded },
    Dimension { name: "extra", aliases: NO_ALIAS, ty: ValueType::Str, store: Store::JsonRoot { column: "extra", prefix: "" }, ops: OPS_TEXT, resources: R_OCC_EVENTS, index: IndexClass::Bounded },
    Dimension { name: "properties", aliases: NO_ALIAS, ty: ValueType::Str, store: Store::JsonRoot { column: "properties", prefix: "" }, ops: OPS_TEXT, resources: R_EVENTS, index: IndexClass::Bounded },
    Dimension { name: "traits", aliases: NO_ALIAS, ty: ValueType::Str, store: Store::JsonRoot { column: "properties", prefix: "" }, ops: OPS_TEXT, resources: R_PERSONS, index: IndexClass::Scan },
    Dimension { name: "stack", aliases: NO_ALIAS, ty: ValueType::Str, store: Store::JsonRoot { column: "stacktrace", prefix: "" }, ops: OPS_TEXT, resources: R_OCC, index: IndexClass::Scan },

    // ---- analytics events ----
    Dimension { name: "name", aliases: NO_ALIAS, ty: ValueType::Str, store: Store::Column("name"), ops: OPS_TEXT, resources: &[Resource::Events, Resource::Transactions], index: IndexClass::Indexed },

    // ---- transactions ----
    Dimension { name: "op", aliases: NO_ALIAS, ty: ValueType::Str, store: Store::Column("op"), ops: OPS_TEXT, resources: R_TX, index: IndexClass::Bounded },
    Dimension { name: "duration", aliases: &["duration_ms"], ty: ValueType::Duration, store: Store::Column("duration_ms"), ops: OPS_ORD, resources: R_TX, index: IndexClass::Bounded },
    Dimension { name: "url", aliases: NO_ALIAS, ty: ValueType::Str, store: Store::Column("url"), ops: OPS_TEXT, resources: R_TX, index: IndexClass::Scan },
    // Dotted canonical names, not a JSON root: these are real columns
    // (`transactions.http_status` / `.http_method`) and `lookup` matches the
    // full dotted string exactly.
    Dimension { name: "http.status", aliases: &["http_status"], ty: ValueType::Int, store: Store::Column("http_status"), ops: OPS_ORD, resources: R_TX, index: IndexClass::Bounded },
    Dimension { name: "http.method", aliases: &["http_method"], ty: ValueType::Str, store: Store::Column("http_method"), ops: OPS_EQ, resources: R_TX, index: IndexClass::Bounded },

    // ---- devices ----
    // `browser` on Devices is a real column, unlike the `context->browser` JSON
    // root used for Occurrences above — disjoint resources keep both legal.
    Dimension { name: "browser", aliases: NO_ALIAS, ty: ValueType::Str, store: Store::Column("browser"), ops: OPS_TEXT, resources: R_DEVICES, index: IndexClass::Bounded },
    Dimension { name: "device.family", aliases: &["family"], ty: ValueType::Str, store: Store::Column("family"), ops: OPS_TEXT, resources: R_DEVICES, index: IndexClass::Bounded },
    Dimension { name: "device.model", aliases: &["model"], ty: ValueType::Str, store: Store::Column("model"), ops: OPS_TEXT, resources: R_DEVICES, index: IndexClass::Bounded },
    Dimension { name: "device.arch", aliases: &["arch"], ty: ValueType::Str, store: Store::Column("arch"), ops: OPS_TEXT, resources: R_DEVICES, index: IndexClass::Bounded },
    Dimension { name: "os.name", aliases: NO_ALIAS, ty: ValueType::Str, store: Store::Column("os_name"), ops: OPS_TEXT, resources: R_DEVICES, index: IndexClass::Bounded },
    Dimension { name: "os.version", aliases: NO_ALIAS, ty: ValueType::Str, store: Store::Column("os_version"), ops: OPS_TEXT, resources: R_DEVICES, index: IndexClass::Bounded },

    // ---- sessions ----
    Dimension { name: "startedAt", aliases: &["started_at"], ty: ValueType::Timestamp, store: Store::Column("started_at"), ops: OPS_ORD, resources: R_SESSIONS, index: IndexClass::Indexed },
];

/// Resources that carry a developer-supplied `tags` JSONB column.
const TAGGABLE: &[Resource] = &[Resource::Issues, Resource::Occurrences, Resource::Events];

/// The synthetic dimension every unrecognised field resolves to (spec §5, rule 3).
/// Deliberately NOT a member of `CATALOG` — it must never appear in autocomplete
/// or the generated docs table as a field literally named "tag".
pub const TAG_DIM: Dimension = Dimension {
    name: "tag",
    aliases: NO_ALIAS,
    ty: ValueType::Str,
    store: Store::Tag,
    ops: OPS_TEXT,
    resources: TAGGABLE,
    index: IndexClass::Indexed,
};

/// `Some` when this resource supports the unknown-field-means-tag fallback.
/// Devices, Persons, Sessions and Transactions have no `tags` column, so an
/// unrecognised field there is a genuine error rather than a tag lookup.
pub fn tag_dimension(r: Resource) -> Option<&'static Dimension> {
    if TAG_DIM.resources.contains(&r) {
        Some(&TAG_DIM)
    } else {
        None
    }
}

pub fn dimensions_for(r: Resource) -> impl Iterator<Item = &'static Dimension> {
    CATALOG.iter().filter(move |d| d.resources.contains(&r))
}

/// Resolve a field name (canonical or alias) within a resource. Returns `None`
/// for unknown names — the resolver then falls back to a tag lookup.
pub fn lookup(field: &str, r: Resource) -> Option<&'static Dimension> {
    dimensions_for(r).find(|d| d.name == field || d.aliases.contains(&field))
}
```

Note two dimensions named `handled` and two named `environment`/`release` exist with different `Store`s — they are disjoint by `resources`, which is exactly what the `dimension_names_are_unique_within_a_resource` test enforces.

Add to `backend/crates/sauron-query/src/lib.rs`:

```rust
pub mod catalog;

pub use catalog::{
    dimensions_for, lookup, tag_dimension, Dimension, IndexClass, Resource, Shorthand, Store,
    ValueType, CATALOG, SHORTHANDS, TAG_DIM,
};
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cd backend && cargo test -p sauron-query
```

Expected: all catalog tests pass. If `dimension_names_are_unique_within_a_resource` fails, a dimension was declared for a resource it already covers — fix the `resources` lists, not the test.

- [ ] **Step 5: Verify gates**

```bash
cd backend && cargo fmt --all -- --check && cargo clippy -p sauron-query --all-targets -- -D warnings
```

Expected: clean. Leave staged; do not commit.

---

### Task 5: Resolver

**Files:**
- Create: `backend/crates/sauron-query/src/resolve.rs`
- Modify: `backend/crates/sauron-query/src/lib.rs`

**Interfaces:**
- Consumes: `Node`, `Predicate`, `MatchOp` (Task 3); `Resource`, `Dimension`, `Store`, `ValueType`, `lookup`, `tag_dimension`, `SHORTHANDS` (Task 4); `QueryError` (Task 1).
- Produces:
  - `pub enum TimeSpec { RelativeSeconds(i64), Absolute(chrono::DateTime<chrono::Utc>) }`
  - `pub enum TypedValue { Str(String), Pattern(String), Int(i64), Bool(bool), DurationMs(i64), Time(TimeSpec), List(Vec<TypedValue>), Absent }`
  - `pub struct ResolvedPredicate { pub dim: &'static Dimension, pub path: Option<String>, pub op: MatchOp, pub value: TypedValue, pub at: usize }`
  - `pub enum ResolvedNode { And(Vec<ResolvedNode>), Or(Vec<ResolvedNode>), Not(Box<ResolvedNode>), Pred(ResolvedPredicate), Text(String) }`
  - `pub fn resolve(node: &Node, r: Resource) -> Result<ResolvedNode, QueryError>`

- [ ] **Step 1: Write the failing test**

Create `backend/crates/sauron-query/src/resolve.rs` with only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse;

    fn one(q: &str, r: Resource) -> ResolvedPredicate {
        match resolve(&parse(q).unwrap(), r).unwrap() {
            ResolvedNode::Pred(p) => p,
            other => panic!("expected a single predicate, got {other:?}"),
        }
    }

    fn err(q: &str, r: Resource) -> QueryError {
        resolve(&parse(q).unwrap(), r).unwrap_err()
    }

    #[test]
    fn resolves_a_curated_field() {
        let p = one("level:error", Resource::Issues);
        assert_eq!(p.dim.name, "level");
        assert_eq!(p.op, MatchOp::Eq);
        assert_eq!(p.value, TypedValue::Str("error".into()));
        assert_eq!(p.path, None);
    }

    #[test]
    fn unknown_field_falls_back_to_a_tag() {
        // Spec §5 rule 3: this is the behaviour that makes dev-defined tags
        // first-class without a prefix.
        let p = one("checkout_step:payment", Resource::Issues);
        assert!(matches!(p.dim.store, Store::Tag));
        assert_eq!(p.path.as_deref(), Some("checkout_step"));
        assert_eq!(p.value, TypedValue::Str("payment".into()));
    }

    #[test]
    fn explicit_tag_prefix_disambiguates() {
        // `tag.level` must reach the TAG store, not the curated `level` column.
        let p = one("tag.level:custom", Resource::Issues);
        assert!(matches!(p.dim.store, Store::Tag));
        assert_eq!(p.path.as_deref(), Some("level"));
    }

    #[test]
    fn unknown_field_errors_where_tags_do_not_exist() {
        assert!(matches!(
            err("nonsense:1", Resource::Devices),
            QueryError::UnknownField { .. }
        ));
    }

    #[test]
    fn resolves_json_path_with_empty_prefix() {
        let p = one("extra.cartValue:100", Resource::Occurrences);
        assert!(matches!(p.dim.store, Store::JsonRoot { column: "extra", .. }));
        assert_eq!(p.path.as_deref(), Some("cartValue"));
    }

    #[test]
    fn resolves_json_path_with_a_prefix() {
        // `os.name` lives at context->os->name, so the prefix is retained.
        let p = one("os.name:Windows", Resource::Occurrences);
        assert!(matches!(
            p.dim.store,
            Store::JsonRoot { column: "context", prefix: "os" }
        ));
        assert_eq!(p.path.as_deref(), Some("os.name"));
    }

    #[test]
    fn resolves_user_email_without_duplicating_the_root() {
        // The column IS the user object, so the path is just `email`.
        let p = one("user.email:a@b.com", Resource::Occurrences);
        assert!(matches!(
            p.dim.store,
            Store::JsonRoot { column: "event_user", prefix: "" }
        ));
        assert_eq!(p.path.as_deref(), Some("email"));
    }

    #[test]
    fn deep_json_paths_are_preserved() {
        let p = one("extra.cart.items.count:3", Resource::Occurrences);
        assert_eq!(p.path.as_deref(), Some("cart.items.count"));
    }

    #[test]
    fn comparison_prefixes_become_operators() {
        assert_eq!(one("timesSeen:>100", Resource::Issues).op, MatchOp::Gt);
        assert_eq!(one("timesSeen:>=100", Resource::Issues).op, MatchOp::Gte);
        assert_eq!(one("timesSeen:<100", Resource::Issues).op, MatchOp::Lt);
        assert_eq!(one("timesSeen:<=100", Resource::Issues).op, MatchOp::Lte);
        assert_eq!(
            one("timesSeen:>100", Resource::Issues).value,
            TypedValue::Int(100)
        );
    }

    #[test]
    fn list_syntax_becomes_in() {
        let p = one("level:[error,fatal]", Resource::Issues);
        assert_eq!(p.op, MatchOp::In);
        assert_eq!(
            p.value,
            TypedValue::List(vec![
                TypedValue::Str("error".into()),
                TypedValue::Str("fatal".into())
            ])
        );
    }

    #[test]
    fn star_becomes_a_pattern_with_like_metacharacters_escaped() {
        let p = one("user.email:*@acme.com", Resource::Occurrences);
        assert_eq!(p.op, MatchOp::Like);
        assert_eq!(p.value, TypedValue::Pattern("%@acme.com".into()));

        // A literal % or _ in the value must not become a wildcard.
        let p2 = one("culprit:*100%_done", Resource::Issues);
        assert_eq!(p2.value, TypedValue::Pattern(r"%100\%\_done".into()));
    }

    #[test]
    fn tilde_is_a_literal_substring_match() {
        let p = one("culprit:~handler", Resource::Issues);
        assert_eq!(p.op, MatchOp::Contains);
        assert_eq!(p.value, TypedValue::Pattern("%handler%".into()));
    }

    #[test]
    fn literal_substring_does_not_interpret_a_star() {
        // The whole reason this operator exists: `*` stays a literal asterisk.
        let p = one("culprit:~foo*bar", Resource::Issues);
        assert_eq!(p.op, MatchOp::Contains);
        assert_eq!(p.value, TypedValue::Pattern("%foo*bar%".into()));
    }

    #[test]
    fn literal_substring_still_escapes_like_metacharacters() {
        let p = one("culprit:~100%_done", Resource::Issues);
        assert_eq!(p.value, TypedValue::Pattern(r"%100\%\_done%".into()));
    }

    #[test]
    fn literal_substring_takes_precedence_over_comparison() {
        // `~` is stripped first and everything after is literal.
        let p = one("culprit:~>=v2", Resource::Issues);
        assert_eq!(p.op, MatchOp::Contains);
        assert_eq!(p.value, TypedValue::Pattern("%>=v2%".into()));
    }

    #[test]
    fn quoting_makes_a_star_literal() {
        let p = one(r#"culprit:"a*b""#, Resource::Issues);
        assert_eq!(p.op, MatchOp::Eq);
        assert_eq!(p.value, TypedValue::Str("a*b".into()));
    }

    #[test]
    fn quoting_disables_comparison_prefixes_too() {
        let p = one(r#"culprit:">100""#, Resource::Issues);
        assert_eq!(p.op, MatchOp::Eq);
        assert_eq!(p.value, TypedValue::Str(">100".into()));
    }

    #[test]
    fn has_checks_key_existence() {
        let p = one("has:extra.cartValue", Resource::Occurrences);
        assert_eq!(p.op, MatchOp::Has);
        assert_eq!(p.value, TypedValue::Absent);
        assert_eq!(p.path.as_deref(), Some("cartValue"));
    }

    #[test]
    fn has_works_on_an_unknown_field_as_a_tag() {
        let p = one("has:checkout_step", Resource::Issues);
        assert!(matches!(p.dim.store, Store::Tag));
        assert_eq!(p.op, MatchOp::Has);
    }

    #[test]
    fn status_shorthands_expand_to_the_status_column() {
        let p = one("is:unresolved", Resource::Issues);
        assert_eq!(p.dim.name, "is");
        assert!(matches!(p.dim.store, Store::Column("status")));
        assert_eq!(p.value, TypedValue::Str("unresolved".into()));
    }

    #[test]
    fn handled_shorthands_expand_to_the_handled_field() {
        let p = one("is:unhandled", Resource::Occurrences);
        assert_eq!(p.dim.name, "handled");
        assert_eq!(p.value, TypedValue::Bool(false));
        assert_eq!(p.op, MatchOp::Eq);

        let p2 = one("is:handled", Resource::Occurrences);
        assert_eq!(p2.value, TypedValue::Bool(true));
    }

    #[test]
    fn unknown_shorthand_is_rejected_not_treated_as_a_tag() {
        assert!(matches!(
            err("is:banana", Resource::Issues),
            QueryError::BadShorthand { .. }
        ));
    }

    #[test]
    fn rejects_bad_enum_value() {
        assert!(matches!(
            err("level:banana", Resource::Issues),
            QueryError::BadEnum { .. }
        ));
    }

    #[test]
    fn rejects_non_numeric_for_int_fields() {
        assert!(matches!(
            err("timesSeen:>lots", Resource::Issues),
            QueryError::BadValue { .. }
        ));
    }

    #[test]
    fn rejects_disallowed_operator() {
        // `is` is an enum; ordering comparisons make no sense on it.
        assert!(matches!(
            err("is:>unresolved", Resource::Issues),
            QueryError::BadOp { .. }
        ));
    }

    #[test]
    fn parses_durations() {
        assert_eq!(
            one("duration:>2s", Resource::Transactions).value,
            TypedValue::DurationMs(2000)
        );
        assert_eq!(
            one("duration:>500ms", Resource::Transactions).value,
            TypedValue::DurationMs(500)
        );
        assert_eq!(
            one("duration:>1m", Resource::Transactions).value,
            TypedValue::DurationMs(60_000)
        );
        // A bare number is milliseconds.
        assert_eq!(
            one("duration:>250", Resource::Transactions).value,
            TypedValue::DurationMs(250)
        );
    }

    #[test]
    fn parses_relative_timestamps() {
        assert_eq!(
            one("firstSeen:>-7d", Resource::Issues).value,
            TypedValue::Time(TimeSpec::RelativeSeconds(7 * 86_400))
        );
        assert_eq!(
            one("firstSeen:>-24h", Resource::Issues).value,
            TypedValue::Time(TimeSpec::RelativeSeconds(24 * 3600))
        );
    }

    #[test]
    fn parses_absolute_timestamps() {
        let v = one("firstSeen:>2026-07-01T00:00:00Z", Resource::Issues).value;
        assert!(matches!(v, TypedValue::Time(TimeSpec::Absolute(_))));
    }

    #[test]
    fn rejects_unparseable_timestamp() {
        assert!(matches!(
            err("firstSeen:>yesterday", Resource::Issues),
            QueryError::BadValue { .. }
        ));
    }

    #[test]
    fn preserves_tree_structure_and_free_text() {
        let got = resolve(
            &parse("level:error (a:1 OR b:2) !is:resolved timeout").unwrap(),
            Resource::Issues,
        )
        .unwrap();
        match got {
            ResolvedNode::And(parts) => {
                assert_eq!(parts.len(), 4);
                assert!(matches!(parts[1], ResolvedNode::Or(_)));
                assert!(matches!(parts[2], ResolvedNode::Not(_)));
                assert!(matches!(parts[3], ResolvedNode::Text(_)));
            }
            other => panic!("expected And, got {other:?}"),
        }
    }

    #[test]
    fn error_offsets_point_at_the_failing_term() {
        let e = err("level:error timesSeen:>lots", Resource::Issues);
        assert_eq!(e.at(), 12);
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cd backend && cargo test -p sauron-query
```

Expected: compile error — `cannot find function `resolve``.

- [ ] **Step 3: Write the resolver**

Prepend to `backend/crates/sauron-query/src/resolve.rs`, above the test module:

```rust
//! Semantic resolution: raw `Node` → `ResolvedNode`, where every field is a
//! `&'static Dimension` from the catalog and every value is typed.
//!
//! This is the security boundary. After `resolve`, no caller-supplied bytes are
//! ever used as a SQL identifier — `dim.store` supplies every column and path
//! name from a `&'static str`, and values travel as typed binds. It mirrors the
//! guarantee `sauron_db::filter::parse_filters` already provides today.

use chrono::{DateTime, Utc};

use crate::ast::{MatchOp, Node, Predicate};
use crate::catalog::{lookup, tag_dimension, Dimension, Resource, Store, ValueType, SHORTHANDS};
use crate::QueryError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeSpec {
    /// Seconds *before now*, resolved against the clock at query time.
    RelativeSeconds(i64),
    Absolute(DateTime<Utc>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum TypedValue {
    Str(String),
    /// A `LIKE` pattern: the user's `*` has become `%`, and any literal
    /// `%`/`_`/`\` is already escaped.
    Pattern(String),
    Int(i64),
    Bool(bool),
    DurationMs(i64),
    Time(TimeSpec),
    List(Vec<TypedValue>),
    /// `has:` — the predicate is about presence, so there is no value.
    Absent,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedPredicate {
    pub dim: &'static Dimension,
    /// Path inside a JSONB column, or the tag key for `Store::Tag`. `None` for
    /// plain columns and rollups.
    pub path: Option<String>,
    pub op: MatchOp,
    pub value: TypedValue,
    pub at: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ResolvedNode {
    And(Vec<ResolvedNode>),
    Or(Vec<ResolvedNode>),
    Not(Box<ResolvedNode>),
    Pred(ResolvedPredicate),
    Text(String),
}

pub fn resolve(node: &Node, r: Resource) -> Result<ResolvedNode, QueryError> {
    Ok(match node {
        Node::And(v) => ResolvedNode::And(
            v.iter()
                .map(|n| resolve(n, r))
                .collect::<Result<Vec<_>, _>>()?,
        ),
        Node::Or(v) => ResolvedNode::Or(
            v.iter()
                .map(|n| resolve(n, r))
                .collect::<Result<Vec<_>, _>>()?,
        ),
        Node::Not(b) => ResolvedNode::Not(Box::new(resolve(b, r)?)),
        Node::Text(t) => ResolvedNode::Text(t.clone()),
        Node::Pred(p) => ResolvedNode::Pred(resolve_pred(p, r, false)?),
    })
}

fn resolve_pred(
    p: &Predicate,
    r: Resource,
    expanded: bool,
) -> Result<ResolvedPredicate, QueryError> {
    // `has:field` — the VALUE names the field, and there is no value of its own.
    if p.field == "has" && !expanded {
        // Every other route into `path` comes from `Predicate.field`, which the
        // lexer already constrained. This one comes from the value side, so it
        // must be constrained here — a JSONB path is often emitted as a
        // `#>> '{a,b}'` array literal where `,`, `}` and `"` are metacharacters.
        if !is_field_ident(&p.value) {
            return Err(QueryError::UnknownField {
                field: p.value.clone(),
                at: p.at,
            });
        }
        let (dim, path) = resolve_field(&p.value, r, p.at)?;
        if !dim.ops.contains(&MatchOp::Has) {
            return Err(QueryError::BadOp {
                field: p.value.clone(),
                at: p.at,
            });
        }
        return Ok(ResolvedPredicate {
            dim,
            path,
            op: MatchOp::Has,
            value: TypedValue::Absent,
            at: p.at,
        });
    }

    // `is:<keyword>` expands to a real predicate. `expanded` breaks the recursion
    // for status shorthands, whose target field is also called `is`.
    //
    // Only a bare equality is a shorthand: `is:>unresolved` carries a comparison
    // operator, so it falls through to the catalog `is` dimension and is rejected
    // there as a bad operator — which is the accurate complaint, rather than
    // claiming `>unresolved` is an unknown shorthand.
    if p.field == "is" && !expanded && split_op(&p.value, p.quoted).0 == MatchOp::Eq {
        let sh = SHORTHANDS
            .iter()
            .find(|s| s.keyword == p.value)
            .ok_or_else(|| QueryError::BadShorthand {
                value: p.value.clone(),
                at: p.at,
            })?;
        return resolve_pred(
            &Predicate {
                field: sh.field.to_string(),
                value: sh.value.to_string(),
                // Quoted so the expansion is never re-interpreted as a wildcard
                // or a comparison.
                quoted: true,
                at: p.at,
            },
            r,
            true,
        );
    }

    let (op, rest) = split_op(&p.value, p.quoted);
    let (dim, path) = resolve_field(&p.field, r, p.at)?;
    if !dim.ops.contains(&op) {
        return Err(QueryError::BadOp {
            field: p.field.clone(),
            at: p.at,
        });
    }
    let value = type_value(dim, op, rest, &p.field, p.at)?;
    Ok(ResolvedPredicate {
        dim,
        path,
        op,
        value,
        at: p.at,
    })
}

/// Field resolution order from spec §5: exact catalog match, then a dotted path
/// under a JSON root, then the tag fallback.
fn resolve_field(
    field: &str,
    r: Resource,
    at: usize,
) -> Result<(&'static Dimension, Option<String>), QueryError> {
    // 1. Exact match. Covers plain names (`level`) and dotted names that are
    //    real columns (`http.status`, `os.name` on Devices).
    if let Some(d) = lookup(field, r) {
        return Ok((d, None));
    }

    // 2. Explicit tag disambiguation.
    if let Some(key) = field.strip_prefix("tag.") {
        if !key.is_empty() {
            if let Some(d) = tag_dimension(r) {
                return Ok((d, Some(key.to_string())));
            }
        }
    }

    // 3. Dotted path under a JSON root.
    if let Some((root, remainder)) = field.split_once('.') {
        if let Some(d) = lookup(root, r) {
            if let Store::JsonRoot { prefix, .. } = d.store {
                let path = if prefix.is_empty() {
                    remainder.to_string()
                } else {
                    // The column holds several namespaces, so keep ours.
                    format!("{prefix}.{remainder}")
                };
                return Ok((d, Some(path)));
            }
        }
    }

    // 4. Unknown → a tag, where the resource has tags at all.
    match tag_dimension(r) {
        Some(d) => Ok((d, Some(field.to_string()))),
        None => Err(QueryError::UnknownField {
            field: field.to_string(),
            at,
        }),
    }
}

/// Peel a comparison prefix, list brackets, or a wildcard off the raw value.
/// Quoting suppresses all of it, which is the only way to search for a literal
/// `>` or `*`.
fn split_op(raw: &str, quoted: bool) -> (MatchOp, &str) {
    if quoted {
        return (MatchOp::Eq, raw);
    }
    // `~` is checked FIRST and returns immediately, so everything after it is
    // literal — including a `*`, a leading `>`, or another `~`. That is the whole
    // point of the operator: it is the one way to say "this exact text appears
    // somewhere in the field" without the value being reinterpreted.
    if let Some(r) = raw.strip_prefix('~') {
        return (MatchOp::Contains, r);
    }
    if let Some(r) = raw.strip_prefix(">=") {
        return (MatchOp::Gte, r);
    }
    if let Some(r) = raw.strip_prefix("<=") {
        return (MatchOp::Lte, r);
    }
    if let Some(r) = raw.strip_prefix('>') {
        return (MatchOp::Gt, r);
    }
    if let Some(r) = raw.strip_prefix('<') {
        return (MatchOp::Lt, r);
    }
    if raw.len() >= 2 && raw.starts_with('[') && raw.ends_with(']') {
        return (MatchOp::In, &raw[1..raw.len() - 1]);
    }
    if raw.contains('*') {
        return (MatchOp::Like, raw);
    }
    (MatchOp::Eq, raw)
}

fn type_value(
    dim: &'static Dimension,
    op: MatchOp,
    raw: &str,
    field: &str,
    at: usize,
) -> Result<TypedValue, QueryError> {
    match op {
        MatchOp::Has => Ok(TypedValue::Absent),
        MatchOp::In => {
            let items = raw
                .split(',')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(|s| type_value(dim, MatchOp::Eq, s, field, at))
                .collect::<Result<Vec<_>, _>>()?;
            if items.is_empty() {
                return Err(QueryError::BadValue {
                    field: field.to_string(),
                    value: raw.to_string(),
                    at,
                });
            }
            Ok(TypedValue::List(items))
        }
        MatchOp::Like => Ok(TypedValue::Pattern(to_like_pattern(raw))),
        // Literal substring: escape the LIKE metacharacters and wrap in `%`
        // ourselves. `*` is NOT translated, so it stays a literal asterisk.
        MatchOp::Contains => Ok(TypedValue::Pattern(format!("%{}%", escape_like(raw)))),
        _ => match dim.ty {
            ValueType::Str => Ok(TypedValue::Str(raw.to_string())),
            ValueType::Enum(opts) => {
                if opts.contains(&raw) {
                    Ok(TypedValue::Str(raw.to_string()))
                } else {
                    Err(QueryError::BadEnum {
                        field: field.to_string(),
                        value: raw.to_string(),
                        at,
                    })
                }
            }
            ValueType::Int => raw.parse::<i64>().map(TypedValue::Int).map_err(|_| {
                QueryError::BadValue {
                    field: field.to_string(),
                    value: raw.to_string(),
                    at,
                }
            }),
            ValueType::Bool => match raw {
                "true" => Ok(TypedValue::Bool(true)),
                "false" => Ok(TypedValue::Bool(false)),
                _ => Err(QueryError::BadValue {
                    field: field.to_string(),
                    value: raw.to_string(),
                    at,
                }),
            },
            ValueType::Duration => parse_duration_ms(raw)
                .map(TypedValue::DurationMs)
                .ok_or_else(|| QueryError::BadValue {
                    field: field.to_string(),
                    value: raw.to_string(),
                    at,
                }),
            ValueType::Timestamp => parse_time(raw).map(TypedValue::Time).ok_or_else(|| {
                QueryError::BadValue {
                    field: field.to_string(),
                    value: raw.to_string(),
                    at,
                }
            }),
        },
    }
}

/// Escape SQL `LIKE` metacharacters only. Used by `MatchOp::Contains`, where the
/// whole value is literal. Mirrors `sauron_db::repo::escape_like`.
fn escape_like(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 4);
    for ch in raw.chars() {
        match ch {
            '\\' => out.push_str(r"\\"),
            '%' => out.push_str(r"\%"),
            '_' => out.push_str(r"\_"),
            c => out.push(c),
        }
    }
    out
}

/// Escape SQL `LIKE` metacharacters, then promote the user's `*` to `%`.
/// Order matters: escaping first means a literal `%` in the input cannot be
/// confused with the wildcard we are about to introduce.
fn to_like_pattern(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 4);
    for ch in raw.chars() {
        match ch {
            '\\' => out.push_str(r"\\"),
            '%' => out.push_str(r"\%"),
            '_' => out.push_str(r"\_"),
            '*' => out.push('%'),
            c => out.push(c),
        }
    }
    out
}

fn parse_duration_ms(raw: &str) -> Option<i64> {
    let (num, mult) = if let Some(n) = raw.strip_suffix("ms") {
        (n, 1)
    } else if let Some(n) = raw.strip_suffix('s') {
        (n, 1_000)
    } else if let Some(n) = raw.strip_suffix('m') {
        (n, 60_000)
    } else if let Some(n) = raw.strip_suffix('h') {
        (n, 3_600_000)
    } else {
        (raw, 1)
    };
    // `checked_mul`, not `*`: `duration:>3000000000000h` parses as a valid i64 but
    // overflows when scaled. Unchecked, that panics in debug builds and silently
    // wraps to a negative duration in release — from an HTTP query parameter.
    num.trim()
        .parse::<i64>()
        .ok()
        .and_then(|v| v.checked_mul(mult))
}

fn parse_time(raw: &str) -> Option<TimeSpec> {
    // Relative: -7d / -24h / -30m / -45s
    if let Some(rest) = raw.strip_prefix('-') {
        let (num, mult) = match rest.chars().last()? {
            'd' => (&rest[..rest.len() - 1], 86_400),
            'h' => (&rest[..rest.len() - 1], 3_600),
            'm' => (&rest[..rest.len() - 1], 60),
            's' => (&rest[..rest.len() - 1], 1),
            _ => return None,
        };
        // See `parse_duration_ms` — the same overflow applies to `-<huge>d`.
        return num
            .parse::<i64>()
            .ok()
            .and_then(|v| v.checked_mul(mult))
            .map(TimeSpec::RelativeSeconds);
    }
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| TimeSpec::Absolute(dt.with_timezone(&Utc)))
}
```

Add to `backend/crates/sauron-query/src/lib.rs`:

```rust
pub mod resolve;

pub use resolve::{resolve, ResolvedNode, ResolvedPredicate, TimeSpec, TypedValue};
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cd backend && cargo test -p sauron-query
```

Expected: all resolver tests pass.

- [ ] **Step 5: Verify gates**

```bash
cd backend && cargo fmt --all -- --check && cargo clippy -p sauron-query --all-targets -- -D warnings
```

Expected: clean. Leave staged; do not commit.

---

### Task 6: Cost classifier

**Files:**
- Create: `backend/crates/sauron-query/src/cost.rs`
- Modify: `backend/crates/sauron-query/src/lib.rs`

**Interfaces:**
- Consumes: `ResolvedNode`, `ResolvedPredicate` (Task 5); `MatchOp` (Task 3); `IndexClass`, `Store` (Task 4).
- Produces:
  - `pub enum Cost { Indexed, Bounded, Scan }` with `Ord` (Indexed < Bounded < Scan)
  - `pub fn classify(node: &ResolvedNode) -> Cost`

The rules, and why each one holds:

| Node | Cost | Reason |
|---|---|---|
| `Pred` | see below | Depends on both the dimension's index class and the operator |
| `Text` | `Scan` | Free text is `::text ILIKE` over JSONB — never indexable today |
| `And` | **min** of children | One indexed child bounds the candidate set; the rest are per-row checks |
| `Or` | **max** of children | The worst branch dominates — every branch must be evaluated |
| `Not` | `max(inner, Bounded)` | A negation cannot index-seek; at best it is a cheap per-row check |
| empty `And` | `Indexed` | A query with no predicates is the plain list query |

Predicate cost: start from `dim.index`, then degrade — `Like` is never index-backed (no `pg_trgm` in this codebase, a deliberate non-goal in spec §3), and `Ne` cannot seek. `Eq`/`In`/`Has` keep the dimension's class, except `Has` on a `Store::Tag` degrades to `Bounded` because the existing `tags` index is `jsonb_path_ops`, which serves `@>` containment only.

- [ ] **Step 1: Write the failing test**

Create `backend/crates/sauron-query/src/cost.rs` with only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::Resource;
    use crate::parse::parse;
    use crate::resolve::resolve;

    fn cost(q: &str, r: Resource) -> Cost {
        classify(&resolve(&parse(q).unwrap(), r).unwrap())
    }

    #[test]
    fn cost_is_ordered() {
        assert!(Cost::Indexed < Cost::Bounded);
        assert!(Cost::Bounded < Cost::Scan);
    }

    #[test]
    fn indexed_field_with_equality_is_indexed() {
        assert_eq!(cost("is:unresolved", Resource::Issues), Cost::Indexed);
    }

    #[test]
    fn unindexed_field_is_bounded() {
        assert_eq!(cost("culprit:handler", Resource::Issues), Cost::Bounded);
    }

    #[test]
    fn wildcard_never_uses_an_index() {
        // No pg_trgm in this codebase — spec §3 non-goal.
        assert_eq!(cost("distinctId:u_*", Resource::Occurrences), Cost::Scan);
    }

    #[test]
    fn free_text_is_a_scan() {
        assert_eq!(cost("timeout", Resource::Issues), Cost::Scan);
    }

    #[test]
    fn literal_substring_is_a_scan() {
        // Same reason as a wildcard: no trigram index exists.
        assert_eq!(cost("culprit:~handler", Resource::Issues), Cost::Scan);
    }

    #[test]
    fn and_takes_the_cheapest_child() {
        // The indexed status predicate bounds the set; `title` is then per-row.
        assert_eq!(
            cost("is:unresolved title:*boom*", Resource::Issues),
            Cost::Indexed
        );
    }

    #[test]
    fn or_takes_the_most_expensive_child() {
        // This is the OR footgun the guard exists to catch.
        assert_eq!(
            cost("is:unresolved OR title:*boom*", Resource::Issues),
            Cost::Scan
        );
    }

    #[test]
    fn negation_never_beats_bounded() {
        assert_eq!(cost("!is:resolved", Resource::Issues), Cost::Bounded);
    }

    #[test]
    fn negated_scan_stays_a_scan() {
        assert_eq!(cost("!title:*boom*", Resource::Issues), Cost::Scan);
    }

    #[test]
    fn empty_query_is_indexed() {
        assert_eq!(cost("", Resource::Issues), Cost::Indexed);
    }

    #[test]
    fn tag_equality_is_indexed_but_tag_existence_is_not() {
        // `tags` carries a jsonb_path_ops GIN, which serves @> containment only.
        assert_eq!(cost("checkout_step:payment", Resource::Issues), Cost::Indexed);
        assert_eq!(cost("has:checkout_step", Resource::Issues), Cost::Bounded);
    }

    #[test]
    fn nested_groups_compose() {
        // AND of (indexed) with (OR of two scans) → the AND still bounds it.
        assert_eq!(
            cost(
                "is:unresolved (title:*a* OR culprit:*b*)",
                Resource::Issues
            ),
            Cost::Indexed
        );
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cd backend && cargo test -p sauron-query
```

Expected: compile error — `cannot find type `Cost``.

- [ ] **Step 3: Write the classifier**

Prepend to `backend/crates/sauron-query/src/cost.rs`, above the test module:

```rust
//! Cost classification. The planner uses this to decide whether a query may run
//! over a long window or must have its window clamped.
//!
//! Replaces `sauron_db::repo::MAX_PAYLOAD_SEARCH_DAYS`, a constant that is
//! currently unreachable because every route passes an explicit `since`. A guard
//! that never fires is worse than no guard, because it reads as protection.

use crate::ast::MatchOp;
use crate::catalog::{IndexClass, Store};
use crate::resolve::{ResolvedNode, ResolvedPredicate};

/// Ordered cheapest to most expensive; `Ord` is what makes the `min`/`max`
/// combination rules below read directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Cost {
    Indexed,
    Bounded,
    Scan,
}

impl From<IndexClass> for Cost {
    fn from(i: IndexClass) -> Cost {
        match i {
            IndexClass::Indexed => Cost::Indexed,
            IndexClass::Bounded => Cost::Bounded,
            IndexClass::Scan => Cost::Scan,
        }
    }
}

fn predicate_cost(p: &ResolvedPredicate) -> Cost {
    let base = Cost::from(p.dim.index);
    match p.op {
        // No trigram index exists, so any wildcard or substring scan reads every
        // candidate row.
        MatchOp::Like | MatchOp::Contains => Cost::Scan,
        // An inequality cannot seek; it can only be checked per row.
        MatchOp::Ne => base.max(Cost::Bounded),
        // The tags GIN is jsonb_path_ops, which serves @> containment only —
        // key existence is not answerable from it.
        MatchOp::Has if matches!(p.dim.store, Store::Tag) => base.max(Cost::Bounded),
        _ => base,
    }
}

pub fn classify(node: &ResolvedNode) -> Cost {
    match node {
        // An indexed child bounds the candidate set for its siblings.
        ResolvedNode::And(v) => v.iter().map(classify).min().unwrap_or(Cost::Indexed),
        // Every branch must be evaluated, so the worst one dominates.
        ResolvedNode::Or(v) => v.iter().map(classify).max().unwrap_or(Cost::Indexed),
        // Negation can never seek, but it is no worse than the inner cost.
        ResolvedNode::Not(b) => classify(b).max(Cost::Bounded),
        ResolvedNode::Pred(p) => predicate_cost(p),
        ResolvedNode::Text(_) => Cost::Scan,
    }
}
```

Add to `backend/crates/sauron-query/src/lib.rs`:

```rust
pub mod cost;

pub use cost::{classify, Cost};
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cd backend && cargo test -p sauron-query
```

Expected: all classifier tests pass.

- [ ] **Step 5: Verify gates**

```bash
cd backend && cargo fmt --all -- --check && cargo clippy -p sauron-query --all-targets -- -D warnings
```

Expected: clean. Leave staged; do not commit.

---

### Task 7: Legacy bridge

**Files:**
- Create: `backend/crates/sauron-query/src/legacy.rs`
- Modify: `backend/crates/sauron-query/src/lib.rs`

**Interfaces:**
- Consumes: `Node`, `Predicate` (Task 3); `QueryError` (Task 1).
- Produces: `pub fn from_legacy(filters: &[String], q: Option<&str>) -> Result<Node, QueryError>`

Spec §5 promises today's shared URLs keep working. The wire format is repeated `filter=field:op:percentEncoded(value)` plus a single `q=`, exactly as `sauron_db::filter::parse_filters` reads it today (`backend/crates/sauron-db/src/filter.rs:75-142`). This produces the same `Node` tree the new grammar produces, so there is one downstream path, not two.

Mapping: `eq`→`value`, `neq`→`Not(value)`, `contains`→`*value*`, `gt`→`>value`, `lt`→`<value`. A `tag` field with value `k=v` becomes a predicate on field `tag.k` with value `v`. Values are percent-decoded first, matching the existing comment at `filter.rs:85-90` about reversing the frontend's `encodeURIComponent`.

- [ ] **Step 1: Write the failing test**

Create `backend/crates/sauron-query/src/legacy.rs` with only this test module:

```rust
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
}
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cd backend && cargo test -p sauron-query
```

Expected: compile error — `cannot find function `from_legacy``.

- [ ] **Step 3: Write the bridge**

Prepend to `backend/crates/sauron-query/src/legacy.rs`, above the test module:

```rust
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
    // `parse()` already enforces — otherwise this one is an unbounded back door.
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

        // `tag` carried its key inside the value as `k=v`; the new grammar
        // addresses it as a dotted field instead.
        let (field, value) = if field == "tag" {
            match value.split_once('=') {
                Some((k, v)) if !k.is_empty() && !v.is_empty() => {
                    (format!("tag.{k}"), v.to_string())
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
            (field.to_string(), value)
        };

        // The legacy format never validated its field names, and `render` emits a
        // field verbatim — so a field containing a space would re-lex as two
        // unrelated tokens. Reject it here, where untrusted input arrives.
        if !is_field_ident(&field) {
            return Err(QueryError::UnknownField {
                field: field.clone(),
                at: i,
            });
        }

        parts.push(match op {
            "eq" => pred(field, value, true, i),
            "neq" => Node::Not(Box::new(pred(field, value, true, i))),
            // `~` (literal substring), NOT `*{value}*`. The old `contains` never
            // treated `*` as a wildcard, so wrapping in stars would silently turn
            // a user's literal `*` into one and change what an existing shared
            // URL returns.
            "contains" => pred(field, format!("~{value}"), false, i),
            "gt" => pred(field, format!(">{value}"), false, i),
            "lt" => pred(field, format!("<{value}"), false, i),
            other => {
                return Err(QueryError::BadOp {
                    field: other.to_string(),
                    at: i,
                })
            }
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
```

Add to `backend/crates/sauron-query/src/lib.rs`:

```rust
pub mod legacy;

pub use legacy::from_legacy;
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cd backend && cargo test -p sauron-query
```

Expected: all legacy tests pass.

- [ ] **Step 5: Verify gates**

```bash
cd backend && cargo fmt --all -- --check && cargo clippy -p sauron-query --all-targets -- -D warnings
```

Expected: clean. Leave staged; do not commit.

---

### Task 8: Renderer and round-trip property

**Files:**
- Create: `backend/crates/sauron-query/src/render.rs`
- Modify: `backend/crates/sauron-query/src/lib.rs`

**Interfaces:**
- Consumes: `Node`, `Predicate` (Task 3); `parse` (Task 3); `from_legacy` (Task 7).
- Produces: `pub fn render(node: &Node) -> String`

Two consumers need this. S5 normalises a saved view's query text before storing it, and S4's chip bar must be able to turn an edited chip set back into text — spec §11 requires text → chips → text to be stable. It is also how a legacy `filter=` URL gets upgraded into a `query=` one.

- [ ] **Step 1: Write the failing test**

Create `backend/crates/sauron-query/src/render.rs` with only this test module:

```rust
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
        assert_eq!(round("level:error is:unresolved"), "level:error is:unresolved");
    }

    #[test]
    fn normalises_explicit_and_away() {
        assert_eq!(round("level:error AND is:unresolved"), "level:error is:unresolved");
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
        assert_eq!(round(r#"message:"connection refused""#), r#"message:"connection refused""#);
    }

    #[test]
    fn quotes_values_containing_a_paren_or_quote() {
        assert_eq!(round(r#"culprit:"handle(req)""#), r#"culprit:"handle(req)""#);
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
}
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cd backend && cargo test -p sauron-query
```

Expected: compile error — `cannot find function `render``.

- [ ] **Step 3: Write the renderer**

Prepend to `backend/crates/sauron-query/src/render.rs`, above the test module:

```rust
//! `Node` → canonical query text.
//!
//! Canonical means: implicit `AND` (never the keyword), parentheses only where
//! precedence requires them, and quoting only where the value would otherwise
//! re-lex differently. `render(parse(x))` must be idempotent — S4's chip bar
//! round-trips through this on every edit.

use crate::ast::{Node, Predicate};

/// True when the value would survive re-lexing unquoted. Anything containing
/// whitespace, a quote, or a parenthesis must be quoted — the lexer breaks a word
/// at a `)` **wherever** it appears, and treats a leading `(` as structural, so
/// testing only the first/last character leaves `handle(req)x` unparseable on the
/// way back in.
fn needs_quoting(v: &str) -> bool {
    v.is_empty()
        || v.chars()
            .any(|c| c.is_whitespace() || c == '"' || c == '(' || c == ')')
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

/// Free text has more ways to be misread than a value does: a bare `or`/`and` is
/// a boolean keyword, a leading `!` is a negation, and anything shaped like
/// `field:value` re-lexes as a predicate. Quote all of them, or the round trip
/// silently changes the query — which corrupts a saved view stored by its text.
fn text_needs_quoting(t: &str) -> bool {
    needs_quoting(t)
        || t.contains(':')
        || t.starts_with('!')
        || t.eq_ignore_ascii_case("or")
        || t.eq_ignore_ascii_case("and")
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
        Node::And(v) => v
            .iter()
            .map(|n| go(n, true))
            .collect::<Vec<_>>()
            .join(" "),
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
```

Add to `backend/crates/sauron-query/src/lib.rs`:

```rust
pub mod render;

pub use render::render;
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cd backend && cargo test -p sauron-query
```

Expected: all renderer tests pass.

- [ ] **Step 5: Run the whole workspace and verify gates**

```bash
cd backend \
  && export DUCKDB_LIB_DIR=$(pwd)/../.cache/duckdb \
  && export LD_LIBRARY_PATH=$DUCKDB_LIB_DIR:$LD_LIBRARY_PATH \
  && cargo fmt --all -- --check \
  && cargo clippy --workspace --all-targets -- -D warnings \
  && cargo test --workspace
```

Expected: clean fmt, no clippy warnings, all tests pass including the pre-existing 25 in `sauron-db`. Leave everything staged; do not commit.

---

## Definition of done for S1

- `cargo test -p sauron-query` passes with roughly 90 tests across eight modules.
- `cargo test --workspace` is green and no existing test changed behaviour — S1 adds a crate and wires it into `[workspace.dependencies]`, but **no other crate depends on it yet**. Nothing in the running product changes.
- `cargo clippy --workspace --all-targets -- -D warnings` is clean.
- The catalog covers every dimension listed in spec §6.
- `render(parse(x))` is idempotent for the cases in Task 8.
- Nothing is committed.

## What S1 deliberately does not do

- No SQL. `Store` describes where a value lives; translating that into diesel is S2's job, guarded by a test that iterates `dimensions_for` and asserts every declared dimension has a planner arm.
- No HTTP. The `query=` parameter appears in S2.
- No `issue_dimensions` table. `Store::Rollup` names a table that does not exist until S3; the resolver returning it is correct and the planner will reject it until then.
- No `handled` column. Migration 24 in S2 adds it; the catalog entry is ahead of the schema on purpose so the grammar is complete and testable first.
