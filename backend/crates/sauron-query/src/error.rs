//! Errors surfaced to the user as they type. Every variant carries the byte
//! offset in the original query string so the UI can render a caret under the
//! offending token rather than a bare sentence.

use thiserror::Error;

use crate::catalog::{dimensions_for, tag_dimension, Resource};
use crate::token::is_field_ident;

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
    /// The name is in neither the catalog nor the `tag.<key>` / JSON-path
    /// spellings. Since `resolve_field` stopped reading an unrecognised name as
    /// a tag key, this is the ONLY outcome for one — so the message has to be
    /// good enough to act on, not just accurate.
    ///
    /// `resource` is `Some` for every error raised by `resolve`, which knows
    /// what is being searched and can therefore say what is available. It is
    /// `None` only for `from_legacy`, a purely syntactic bridge that takes no
    /// resource: the complaint there is that the name is not even an
    /// identifier, and a field list would not help.
    #[error("{}", unknown_field_message(field, *resource))]
    UnknownField {
        field: String,
        at: usize,
        resource: Option<Resource>,
    },
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

/// The body of the 400 a caller sees for an unrecognised field.
///
/// Three parts, and each earns its place. **Name the field**, because the
/// caller's query may carry several and a bare "unknown field" leaves them
/// guessing which. **Give the tag spelling**, because until this slice an
/// unrecognised name silently *became* a tag key — so the commonest reason to
/// hit this error is a developer-supplied tag that now needs saying out loud;
/// omitting it would read as "tags are no longer filterable". **List the
/// fields**, because the alternative is a round trip through the docs for what
/// is a fixed, per-resource, and rather short list.
///
/// Resource-aware throughout: Devices, Persons, Sessions and Transactions have
/// no `tags` column, so they are offered no tag spelling at all rather than
/// advice that cannot work.
fn unknown_field_message(field: &str, resource: Option<Resource>) -> String {
    let mut msg = format!("`{field}` is not a valid field for this view");
    let Some(r) = resource else {
        return msg;
    };

    if tag_dimension(r).is_some() {
        // `tag.<key>` only spells a key the lexer accepts as an identifier.
        // Recommending `tag.cart@checkout` would be advice that does not work,
        // so a key that is not an identifier is pointed at the escape hatch.
        if is_field_ident(field) {
            msg.push_str(&format!(
                "; to filter on a developer-supplied tag, write `tag.{field}`"
            ));
        } else {
            msg.push_str("; to filter on a developer-supplied tag, write `tag:<key>=<value>`");
        }
    }

    // Canonical names only. Aliases would double the length of the list for a
    // caller who has just been told the name they used is not one of them.
    let mut names: Vec<&str> = dimensions_for(r).map(|d| d.name).collect();
    names.sort_unstable();
    names.dedup();
    if !names.is_empty() {
        msg.push_str(&format!(". Available fields: {}", names.join(", ")));
    }
    msg
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
            resource: Some(Resource::Issues),
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
            resource: None,
        };
        assert_eq!(
            e.to_string(),
            "`enviroment` is not a valid field for this view"
        );
    }

    #[test]
    fn display_tells_a_taggable_resource_how_to_spell_a_tag() {
        let e = QueryError::UnknownField {
            field: "checkout_step".into(),
            at: 0,
            resource: Some(Resource::Issues),
        };
        let msg = e.to_string();
        assert!(
            msg.starts_with("`checkout_step` is not a valid field"),
            "{msg}"
        );
        assert!(msg.contains("`tag.checkout_step`"), "{msg}");
        assert!(msg.contains("Available fields: "), "{msg}");
    }

    #[test]
    fn display_lists_fields_in_a_stable_order() {
        // Two calls must not disagree, and the order must not depend on where
        // a dimension happens to sit in `CATALOG`.
        let e = QueryError::UnknownField {
            field: "nope".into(),
            at: 0,
            resource: Some(Resource::Devices),
        };
        assert_eq!(e.to_string(), e.to_string());
        let list = e.to_string();
        let names = list.split("Available fields: ").nth(1).unwrap();
        let mut sorted: Vec<&str> = names.split(", ").collect();
        let before = sorted.clone();
        sorted.sort_unstable();
        assert_eq!(before, sorted, "{list}");
    }
}
