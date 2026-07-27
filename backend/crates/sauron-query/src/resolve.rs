//! Semantic resolution: raw `Node` → `ResolvedNode`, where every field is a
//! `&'static Dimension` from the catalog and every value is typed.
//!
//! This is the security boundary. After `resolve`, no caller-supplied bytes are
//! ever used as a SQL identifier — `dim.store` supplies every column and path
//! name from a `&'static str`, and values travel as typed binds. It mirrors the
//! guarantee `sauron_db::filter::parse_filters` already provides today.

use chrono::{DateTime, Utc};

use crate::ast::{MatchOp, Node, Predicate};
use crate::catalog::{
    lookup, tag_dimension, Dimension, IndexClass, Resource, Store, ValueType, SHORTHANDS,
};
use crate::token::is_field_ident;
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
    /// Effective index class for THIS resource, which can be worse than
    /// `dim.index`. A tag filter is GIN-backed on Occurrences and Events, but on
    /// Issues it lowers to a correlated `EXISTS` into `error_events` (the issues
    /// table has no `tags` column), so it is a per-candidate-row subplan.
    pub index: IndexClass,
}

/// `dim.index` is the best case. Downgrade it where the storage for THIS
/// resource cannot deliver that.
fn effective_index(dim: &'static Dimension, r: Resource) -> IndexClass {
    match dim.store {
        // No `tags` column on `issues` — this becomes a correlated EXISTS.
        Store::Tag if r == Resource::Issues => IndexClass::Bounded,
        // The `issue_dimensions` rollup table does not exist yet.
        Store::Rollup => IndexClass::Bounded,
        _other => dim.index,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ResolvedNode {
    And(Vec<ResolvedNode>),
    Or(Vec<ResolvedNode>),
    Not(Box<ResolvedNode>),
    Pred(ResolvedPredicate),
    /// Free-text search term, **not** escaped and **not** wrapped in wildcards —
    /// unlike `Contains`/`Like`, which arrive as a ready `TypedValue::Pattern`.
    /// The planner owns escaping this, because it decides which columns the term
    /// is matched against. Forgetting to escape it makes `100%` match every row.
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
        // Every other route into `path` draws from `Predicate.field`, which the
        // lexer already constrained via `is_field_ident`. This is the one place
        // the operand comes from `Predicate.value` instead, so it must be
        // constrained here too — otherwise a JSONB path built as `#>> '{a,b}'`
        // could receive caller-controlled `,`/`}`/`"` metacharacters.
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
            index: effective_index(dim, r),
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

    // `tag:<key>=<value>` — the escape hatch for tag keys that are not legal
    // identifiers. Tag keys are entirely unconstrained on the write path (the
    // ingest edge stores whatever JSON object keys arrive), so the grammar needs
    // a way to name any of them. The key is everything before the FIRST `=`; the
    // remainder is an ordinary value, so every operator still composes:
    // `tag:cart@checkout=~eu` is a literal-substring match on that key.
    if p.field == "tag" && !expanded {
        let (key, rest) = p
            .value
            .split_once('=')
            .ok_or_else(|| QueryError::BadValue {
                field: "tag".to_string(),
                value: p.value.clone(),
                at: p.at,
            })?;
        if key.is_empty() {
            return Err(QueryError::BadValue {
                field: "tag".to_string(),
                value: p.value.clone(),
                at: p.at,
            });
        }
        let dim = tag_dimension(r).ok_or_else(|| QueryError::UnknownField {
            field: "tag".to_string(),
            at: p.at,
        })?;
        let (op, value_src) = split_op(rest, p.quoted);
        if !dim.ops.contains(&op) {
            return Err(QueryError::BadOp {
                field: "tag".to_string(),
                at: p.at,
            });
        }
        let value = type_value(dim, op, value_src, "tag", p.at)?;
        return Ok(ResolvedPredicate {
            dim,
            path: Some(key.to_string()),
            op,
            value,
            at: p.at,
            index: effective_index(dim, r),
        });
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
        index: effective_index(dim, r),
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
            ValueType::Int => {
                raw.parse::<i64>()
                    .map(TypedValue::Int)
                    .map_err(|_| QueryError::BadValue {
                        field: field.to_string(),
                        value: raw.to_string(),
                        at,
                    })
            }
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
            ValueType::Timestamp => {
                parse_time(raw)
                    .map(TypedValue::Time)
                    .ok_or_else(|| QueryError::BadValue {
                        field: field.to_string(),
                        value: raw.to_string(),
                        at,
                    })
            }
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
        assert!(matches!(
            p.dim.store,
            Store::JsonRoot {
                column: "extra",
                ..
            }
        ));
        assert_eq!(p.path.as_deref(), Some("cartValue"));
    }

    #[test]
    fn resolves_json_path_with_a_prefix() {
        // `os.name` lives at context->os->name, so the prefix is retained.
        let p = one("os.name:Windows", Resource::Occurrences);
        assert!(matches!(
            p.dim.store,
            Store::JsonRoot {
                column: "context",
                prefix: "os"
            }
        ));
        assert_eq!(p.path.as_deref(), Some("os.name"));
    }

    #[test]
    fn resolves_user_email_without_duplicating_the_root() {
        // The column IS the user object, so the path is just `email`.
        let p = one("user.email:a@b.com", Resource::Occurrences);
        assert!(matches!(
            p.dim.store,
            Store::JsonRoot {
                column: "event_user",
                prefix: ""
            }
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
    fn rejects_duration_that_overflows_instead_of_panicking() {
        // Parses as a valid i64, then overflows when scaled to milliseconds.
        assert!(matches!(
            err("duration:>3000000000000h", Resource::Transactions),
            QueryError::BadValue { .. }
        ));
    }

    #[test]
    fn rejects_relative_time_that_overflows_instead_of_panicking() {
        assert!(matches!(
            err("firstSeen:>-9000000000000000d", Resource::Issues),
            QueryError::BadValue { .. }
        ));
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

    #[test]
    fn has_rejects_an_operand_that_is_not_an_identifier() {
        // A JSONB path is often emitted as a `#>> '{a,b}'` array literal, where
        // `,` `}` and `"` are metacharacters — the planner must never see them.
        assert!(matches!(
            err(r#"has:"extra.a,b}""#, Resource::Occurrences),
            QueryError::UnknownField { .. }
        ));
        assert!(matches!(
            err(r#"has:"a'; DROP TABLE users--""#, Resource::Issues),
            QueryError::UnknownField { .. }
        ));
    }

    #[test]
    fn has_still_accepts_a_legal_dotted_path() {
        let p = one("has:extra.cartValue", Resource::Occurrences);
        assert_eq!(p.op, MatchOp::Has);
        assert_eq!(p.path.as_deref(), Some("cartValue"));
    }

    #[test]
    fn tag_escape_hatch_accepts_a_non_identifier_key() {
        // Tag keys are unconstrained on the write path, so the grammar must be
        // able to name any of them.
        let p = one("tag:cart@checkout=eu", Resource::Issues);
        assert!(matches!(p.dim.store, Store::Tag));
        assert_eq!(p.path.as_deref(), Some("cart@checkout"));
        assert_eq!(p.op, MatchOp::Eq);
        assert_eq!(p.value, TypedValue::Str("eu".into()));
    }

    #[test]
    fn tag_escape_hatch_composes_with_operators() {
        let p = one("tag:cart@checkout=~eu", Resource::Issues);
        assert_eq!(p.path.as_deref(), Some("cart@checkout"));
        assert_eq!(p.op, MatchOp::Contains);
        assert_eq!(p.value, TypedValue::Pattern("%eu%".into()));

        let w = one("tag:100%off=*sale*", Resource::Issues);
        assert_eq!(w.path.as_deref(), Some("100%off"));
        assert_eq!(w.op, MatchOp::Like);
    }

    #[test]
    fn tag_escape_hatch_keeps_extra_equals_in_the_value() {
        let p = one("tag:expr=a=b", Resource::Issues);
        assert_eq!(p.path.as_deref(), Some("expr"));
        assert_eq!(p.value, TypedValue::Str("a=b".into()));
    }

    #[test]
    fn tag_without_a_key_value_pair_is_rejected() {
        assert!(matches!(
            err("tag:justakey", Resource::Issues),
            QueryError::BadValue { .. }
        ));
        assert!(matches!(
            err("tag:=novalue", Resource::Issues),
            QueryError::BadValue { .. }
        ));
    }

    #[test]
    fn tag_escape_hatch_is_rejected_where_tags_do_not_exist() {
        assert!(matches!(
            err("tag:a@b=v", Resource::Devices),
            QueryError::UnknownField { .. }
        ));
    }

    #[test]
    fn browser_resolves_to_the_runtime_key_that_actually_exists() {
        // Enrichment writes the browser under `context->runtime`, not
        // `context->browser`. Pointing at the wrong key matches nothing forever.
        let p = one("browser.name:Chrome", Resource::Occurrences);
        assert_eq!(p.path.as_deref(), Some("runtime.name"));
        let alias = one("runtime.name:Chrome", Resource::Occurrences);
        assert_eq!(alias.path.as_deref(), Some("runtime.name"));
    }
}
