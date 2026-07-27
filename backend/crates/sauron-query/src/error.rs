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
    /// `at` carries two different units depending on where the error originated:
    /// a byte offset into the original query string (for caret rendering) when
    /// raised from `parse`/`resolve`, or the zero-based index of the offending
    /// `filter=` parameter when raised from `from_legacy`, which has no query
    /// string to offset into. Structural limits have no single offset and
    /// report 0.
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
        assert_eq!(
            e.to_string(),
            "`enviroment` is not a valid field for this view"
        );
    }
}
